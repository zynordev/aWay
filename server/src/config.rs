//! Sunucu yapılandırması — ortam değişkenlerinden okunur.
//!
//! Örnek `.env` / systemd ortamı:
//!   AWAY_BIND=127.0.0.1:9000            # nginx bunun önünde TLS sonlandırır
//!   AWAY_ACCOUNTS=/var/lib/away/accounts.json
//!   AWAY_TURN_SECRET=<coturn static-auth-secret ile aynı>
//!   AWAY_TURN_URLS=turn:relay.senindomainin:3478?transport=udp,turns:relay.senindomainin:5349
//!   AWAY_STUN_URLS=stun:relay.senindomainin:3478
//!   AWAY_TURN_TTL=600                    # üretilen TURN kimlik bilgisi ömrü (sn)

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// Dinlenecek adres (nginx arkasında localhost).
    pub bind: String,
    /// Hesap deposu dosyası (argon2 hash'li).
    pub accounts_path: PathBuf,
    /// coturn ile paylaşılan gizli anahtar (yoksa TURN kapalı, sadece STUN).
    pub turn_secret: Option<String>,
    /// TURN URL'leri.
    pub turn_urls: Vec<String>,
    /// STUN URL'leri.
    pub stun_urls: Vec<String>,
    /// Üretilen kısa ömürlü TURN kimlik bilgisi süresi (saniye).
    pub turn_ttl: u64,
    /// WS üzerinden açık kayda izin ver. Varsayılan KAPALI (hesaplar CLI ile açılır).
    pub allow_register: bool,
}

fn env_list(key: &str) -> Vec<String> {
    std::env::var(key)
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

impl Config {
    pub fn from_env() -> Self {
        let bind = std::env::var("AWAY_BIND").unwrap_or_else(|_| "127.0.0.1:9000".to_string());
        let accounts_path = std::env::var("AWAY_ACCOUNTS")
            .unwrap_or_else(|_| "accounts.json".to_string())
            .into();
        let turn_secret = std::env::var("AWAY_TURN_SECRET").ok().filter(|s| !s.is_empty());
        let mut stun_urls = env_list("AWAY_STUN_URLS");
        if stun_urls.is_empty() {
            // Makul bir varsayılan: herkese açık Google STUN (yalnızca NAT keşfi için).
            stun_urls.push("stun:stun.l.google.com:19302".to_string());
        }
        let turn_ttl = std::env::var("AWAY_TURN_TTL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(600);
        Config {
            bind,
            accounts_path,
            turn_secret,
            turn_urls: env_list("AWAY_TURN_URLS"),
            stun_urls,
            turn_ttl,
            allow_register: matches!(
                std::env::var("AWAY_ALLOW_REGISTER").as_deref(),
                Ok("1") | Ok("true") | Ok("yes")
            ),
        }
    }
}
