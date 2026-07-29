//! Kısa ömürlü TURN kimlik bilgisi üretimi (coturn `use-auth-secret` / REST API).
//!
//! coturn `static-auth-secret` ile paylaşılan gizli anahtar kullanılarak:
//!   username   = "<son_geçerlilik_unix>:<kullanıcı>"
//!   credential = base64( HMAC-SHA1( secret, username ) )
//! Böylece sunucu, veritabanına TURN kullanıcısı yazmadan geçici kimlik dağıtır.

use away_shared::protocol::IceServer;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;

type HmacSha1 = Hmac<Sha1>;

/// Verilen kullanıcı için (STUN + varsa TURN) ICE sunucu listesini üretir.
pub fn ice_servers_for(cfg: &Config, user: &str) -> Vec<IceServer> {
    let mut servers = Vec::new();

    if !cfg.stun_urls.is_empty() {
        servers.push(IceServer {
            urls: cfg.stun_urls.clone(),
            username: None,
            credential: None,
        });
    }

    if let (Some(secret), false) = (&cfg.turn_secret, cfg.turn_urls.is_empty()) {
        let expiry = now_unix() + cfg.turn_ttl;
        let username = format!("{expiry}:{user}");
        let credential = sign(secret, &username);
        servers.push(IceServer {
            urls: cfg.turn_urls.clone(),
            username: Some(username),
            credential: Some(credential),
        });
    }

    servers
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sign(secret: &str, message: &str) -> String {
    let mut mac = HmacSha1::new_from_slice(secret.as_bytes())
        .expect("HMAC herhangi bir anahtar boyutunu kabul eder");
    mac.update(message.as_bytes());
    let result = mac.finalize().into_bytes();
    base64::engine::general_purpose::STANDARD.encode(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stun_only_when_no_turn_secret() {
        let cfg = Config {
            bind: "127.0.0.1:0".into(),
            accounts_path: "accounts.json".into(),
            turn_secret: None,
            turn_urls: vec!["turn:example:3478".into()],
            stun_urls: vec!["stun:example:3478".into()],
            turn_ttl: 600,
            allow_register: false,
        };
        let s = ice_servers_for(&cfg, "murat");
        assert_eq!(s.len(), 1);
        assert!(s[0].username.is_none());
    }

    #[test]
    fn turn_credentials_are_generated() {
        let cfg = Config {
            bind: "127.0.0.1:0".into(),
            accounts_path: "accounts.json".into(),
            turn_secret: Some("topsecret".into()),
            turn_urls: vec!["turn:example:3478".into()],
            stun_urls: vec![],
            turn_ttl: 600,
            allow_register: false,
        };
        let s = ice_servers_for(&cfg, "murat");
        assert_eq!(s.len(), 1);
        let turn = &s[0];
        assert!(turn.username.as_ref().unwrap().ends_with(":murat"));
        assert!(turn.credential.is_some());
    }
}
