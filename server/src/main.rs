//! aWay sinyal sunucusu — VDS'te çalışır, nginx arkasında TLS ile sunulur.
//!
//! Çalıştırma:
//!   away-server                       # sunucuyu başlat (AWAY_* env değişkenleriyle)
//!   away-server adduser <ad> <şifre>  # hesap oluştur (açık kayıt kapalı olduğu için)
//!
//! Sunucu WebRTC medyasını GÖRMEZ; yalnızca kullanıcı adı doğrulaması + SDP/ICE
//! buluşturması yapar. Medya doğrudan (P2P) ya da coturn relay üzerinden akar.

use anyhow::Result;
use away_server::build_app;
use away_server::config::Config;
use away_server::state::AppState;
use away_server::users::{FileUserStore, UserStore};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,away_server=debug".into()),
        )
        .init();

    let cfg = Config::from_env();

    // ── CLI alt komutu: adduser ──────────────────────────────────────────────
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("adduser") {
        return add_user_cli(&cfg, &args);
    }

    // ── Sunucu ───────────────────────────────────────────────────────────────
    let users = FileUserStore::load(&cfg.accounts_path)?;
    let bind = cfg.bind.clone();
    let state = AppState::new(cfg, users);
    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("aWay sinyal sunucusu dinliyor: {bind}  (WS: /ws)");
    axum::serve(listener, app).await?;
    Ok(())
}

fn add_user_cli(cfg: &Config, args: &[String]) -> Result<()> {
    let (Some(username), Some(password)) = (args.get(2), args.get(3)) else {
        eprintln!("kullanım: away-server adduser <kullanıcı_adı> <şifre>");
        std::process::exit(2);
    };
    let store = FileUserStore::load(&cfg.accounts_path)?;
    match store.register(username, password) {
        Ok(()) => {
            println!("hesap oluşturuldu: {username}  ({})", cfg.accounts_path.display());
            Ok(())
        }
        Err(e) => {
            eprintln!("hata: {e}");
            std::process::exit(1);
        }
    }
}
