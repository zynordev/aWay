//! Uçtan uca sinyal akışı testi: sunucuyu süreç-içi ayağa kaldırır, iki WS istemcisi
//! bağlar ve giriş → bağlantı isteği → kabul → SDP değiş tokuşu yolunu doğrular.

use away_server::config::Config;
use away_server::state::AppState;
use away_server::users::{FileUserStore, UserStore};
use away_shared::protocol::{ClientMessage, ServerMessage, SignalPayload};
use away_shared::PROTOCOL_VERSION;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn send(ws: &mut Ws, msg: &ClientMessage) {
    let json = serde_json::to_string(msg).unwrap();
    ws.send(Message::Text(json.into())).await.unwrap();
}

/// Sonraki ServerMessage'ı (zaman aşımlı) al.
async fn recv(ws: &mut Ws) -> ServerMessage {
    let fut = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    return serde_json::from_str::<ServerMessage>(t.as_ref()).unwrap()
                }
                Some(Ok(_)) => continue, // ping/pong vb.
                other => panic!("beklenmeyen ws olayı: {other:?}"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .expect("mesaj zaman aşımına uğradı")
}

#[tokio::test]
async fn full_signaling_flow() {
    // Geçici hesap dosyası
    let accounts = std::env::temp_dir().join(format!("away-test-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&accounts);

    let cfg = Config {
        bind: "127.0.0.1:0".into(),
        accounts_path: accounts.clone(),
        turn_secret: None,
        turn_urls: vec![],
        stun_urls: vec!["stun:stun.l.google.com:19302".into()],
        turn_ttl: 600,
        allow_register: false,
    };

    let users = FileUserStore::load(&cfg.accounts_path).unwrap();
    users.register("murat", "sifre1").unwrap();
    users.register("ahmet", "sifre2").unwrap();

    let state = AppState::new(cfg, users);
    let app = away_server::build_app(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let url = format!("ws://{addr}/ws");
    let (mut a, _) = connect_async(&url).await.unwrap();
    let (mut b, _) = connect_async(&url).await.unwrap();

    // ── Giriş ────────────────────────────────────────────────────────────────
    send(&mut a, &ClientMessage::Login {
        protocol_version: PROTOCOL_VERSION,
        username: "murat".into(),
        password: "sifre1".into(),
    }).await;
    assert!(matches!(recv(&mut a).await, ServerMessage::LoggedIn { .. }));

    send(&mut b, &ClientMessage::Login {
        protocol_version: PROTOCOL_VERSION,
        username: "ahmet".into(),
        password: "sifre2".into(),
    }).await;
    assert!(matches!(recv(&mut b).await, ServerMessage::LoggedIn { .. }));

    // ── A → ahmet bağlantı isteği ─────────────────────────────────────────────
    send(&mut a, &ClientMessage::Connect { to: "ahmet".into() }).await;

    // B gelen isteği alır
    let session = match recv(&mut b).await {
        ServerMessage::IncomingConnect { session, from, .. } => {
            assert_eq!(from, "murat");
            session
        }
        other => panic!("IncomingConnect beklendi, geldi: {other:?}"),
    };

    // B kabul eder
    send(&mut b, &ClientMessage::ConnectResponse {
        session: session.clone(),
        to: "murat".into(),
        accept: true,
        reason: None,
    }).await;

    // A kabul bildirimini alır (aynı oturum kimliği)
    match recv(&mut a).await {
        ServerMessage::ConnectAccepted { session: s, peer, .. } => {
            assert_eq!(s, session);
            assert_eq!(peer, "ahmet");
        }
        other => panic!("ConnectAccepted beklendi, geldi: {other:?}"),
    }

    // ── SDP değiş tokuşu ──────────────────────────────────────────────────────
    send(&mut a, &ClientMessage::Signal {
        session: session.clone(),
        to: "ahmet".into(),
        payload: SignalPayload::Offer { sdp: "v=0...".into() },
    }).await;

    match recv(&mut b).await {
        ServerMessage::Signal { from, payload: SignalPayload::Offer { sdp }, .. } => {
            assert_eq!(from, "murat");
            assert_eq!(sdp, "v=0...");
        }
        other => panic!("Signal(Offer) beklendi, geldi: {other:?}"),
    }

    // B answer'ı geri gönderir
    send(&mut b, &ClientMessage::Signal {
        session: session.clone(),
        to: "murat".into(),
        payload: SignalPayload::Answer { sdp: "v=0-answer".into() },
    }).await;

    match recv(&mut a).await {
        ServerMessage::Signal { from, payload: SignalPayload::Answer { sdp }, .. } => {
            assert_eq!(from, "ahmet");
            assert_eq!(sdp, "v=0-answer");
        }
        other => panic!("Signal(Answer) beklendi, geldi: {other:?}"),
    }

    let _ = std::fs::remove_file(&accounts);
}

#[tokio::test]
async fn connect_to_offline_user_errors() {
    let accounts = std::env::temp_dir().join(format!("away-test-off-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&accounts);

    let cfg = Config {
        bind: "127.0.0.1:0".into(),
        accounts_path: accounts.clone(),
        turn_secret: None,
        turn_urls: vec![],
        stun_urls: vec![],
        turn_ttl: 600,
        allow_register: false,
    };
    let users = FileUserStore::load(&cfg.accounts_path).unwrap();
    users.register("murat", "sifre1").unwrap();
    let state = AppState::new(cfg, users);
    let app = away_server::build_app(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

    let (mut a, _) = connect_async(&format!("ws://{addr}/ws")).await.unwrap();
    send(&mut a, &ClientMessage::Login {
        protocol_version: PROTOCOL_VERSION,
        username: "murat".into(),
        password: "sifre1".into(),
    }).await;
    assert!(matches!(recv(&mut a).await, ServerMessage::LoggedIn { .. }));

    send(&mut a, &ClientMessage::Connect { to: "yok".into() }).await;
    assert!(matches!(recv(&mut a).await, ServerMessage::Error { .. }));

    let _ = std::fs::remove_file(&accounts);
}
