//! WebSocket handler — her istemci bağlantısının yaşam döngüsü ve mesaj işleme.
//!
//! Akış: bağlan → (Register|Login) → Connect/IncomingConnect → ConnectResponse
//! → ConnectAccepted → Signal (SDP/ICE karşılıklı) → Hangup.

use away_shared::protocol::{ClientMessage, ErrorCode, ServerMessage};
use away_shared::PROTOCOL_VERSION;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::state::{AppState, Session, Tx};
use crate::turn::ice_servers_for;
use crate::users::UserStore;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    // Yazıcı görevi: kanala düşen ServerMessage'ları JSON olarak WS'e yaz.
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match serde_json::to_string(&msg) {
                Ok(json) => {
                    if sink.send(Message::Text(json)).await.is_err() {
                        break;
                    }
                }
                Err(e) => tracing::error!("serialize hatası: {e}"),
            }
        }
    });

    // Okuyucu döngüsü: gelen mesajları işle.
    let mut authed: Option<String> = None;
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text) {
                Ok(cm) => handle_client_msg(&state, &tx, &mut authed, cm).await,
                Err(e) => {
                    let _ = tx.send(err(ErrorCode::BadRequest, format!("bozuk mesaj: {e}")));
                }
            },
            Message::Close(_) => break,
            _ => {} // Ping/Pong/Binary — yok say (axum ping'e otomatik yanıt verir)
        }
    }

    // Temizlik: kullanıcıyı offline yap ve aktif oturumların karşı taraflarına Hangup gönder.
    if let Some(user) = authed {
        state.set_offline_if(&user, &tx);
        for (session, peer) in state.drop_sessions_of(&user) {
            state.send_to(&peer, ServerMessage::Hangup { session });
            state.send_to(&peer, ServerMessage::Presence { user: user.clone(), online: false });
        }
    }
    drop(tx);
    let _ = writer.await;
}

async fn handle_client_msg(
    state: &Arc<AppState>,
    tx: &Tx,
    authed: &mut Option<String>,
    msg: ClientMessage,
) {
    match msg {
        ClientMessage::Register { protocol_version, username, password } => {
            if protocol_version != PROTOCOL_VERSION {
                let _ = tx.send(protocol_mismatch());
                return;
            }
            if !state.cfg.allow_register {
                let _ = tx.send(err(
                    ErrorCode::BadRequest,
                    "açık kayıt kapalı — hesap yöneticiye açtırılır".into(),
                ));
                return;
            }
            let st = state.clone();
            let u = username.clone();
            let res = tokio::task::spawn_blocking(move || st.users.register(&u, &password)).await;
            match res {
                Ok(Ok(())) => {
                    let _ = tx.send(ServerMessage::Registered { username });
                }
                Ok(Err(e)) => {
                    let _ = tx.send(err(ErrorCode::UsernameTaken, e.to_string()));
                }
                Err(_) => {
                    let _ = tx.send(err(ErrorCode::Internal, "kayıt işlemi başarısız".into()));
                }
            }
        }

        ClientMessage::Login { protocol_version, username, password } => {
            if protocol_version != PROTOCOL_VERSION {
                let _ = tx.send(protocol_mismatch());
                return;
            }
            let st = state.clone();
            let u = username.clone();
            let res = tokio::task::spawn_blocking(move || st.users.verify(&u, &password)).await;
            match res {
                Ok(Ok(true)) => {
                    // Aynı kullanıcı başka yerde bağlıysa eski bağlantıyı düşür.
                    if let Some(old) = state.set_online(&username, tx.clone()) {
                        let _ = old.send(err(
                            ErrorCode::Internal,
                            "başka bir yerden giriş yapıldı".into(),
                        ));
                    }
                    *authed = Some(username.clone());
                    let token = AppState::new_session_id();
                    let _ = tx.send(ServerMessage::LoggedIn { username, session_token: token });
                }
                Ok(Ok(false)) => {
                    let _ = tx.send(err(ErrorCode::InvalidCredentials, "kullanıcı adı ya da şifre hatalı".into()));
                }
                _ => {
                    let _ = tx.send(err(ErrorCode::Internal, "giriş işlemi başarısız".into()));
                }
            }
        }

        ClientMessage::Connect { to } => {
            let Some(me) = authed.clone() else {
                let _ = tx.send(not_authed());
                return;
            };
            if to == me {
                let _ = tx.send(err(ErrorCode::BadRequest, "kendine bağlanamazsın".into()));
                return;
            }
            if !state.is_online(&to) {
                let _ = tx.send(err(ErrorCode::PeerNotFound, format!("{to} çevrimiçi değil")));
                return;
            }
            let session = AppState::new_session_id();
            state.insert_session(
                session.clone(),
                Session { initiator: me.clone(), target: to.clone(), accepted: false },
            );
            let ice = ice_servers_for(&state.cfg, &to);
            let delivered = state.send_to(
                &to,
                ServerMessage::IncomingConnect { session: session.clone(), from: me, ice_servers: ice },
            );
            if !delivered {
                state.remove_session(&session);
                let _ = tx.send(err(ErrorCode::PeerNotFound, format!("{to} ulaşılamadı")));
            }
        }

        ClientMessage::ConnectResponse { session, to, accept, reason } => {
            let Some(me) = authed.clone() else {
                let _ = tx.send(not_authed());
                return;
            };
            let Some(s) = state.get_session(&session) else {
                let _ = tx.send(err(ErrorCode::UnknownSession, "oturum bulunamadı".into()));
                return;
            };
            // Yalnızca oturumun HEDEFİ (host) yanıt verebilir; `to` başlatan olmalı.
            if s.target != me || s.initiator != to {
                let _ = tx.send(err(ErrorCode::BadRequest, "bu oturuma yanıt veremezsin".into()));
                return;
            }
            if accept {
                state.accept_session(&session);
                let ice = ice_servers_for(&state.cfg, &to);
                state.send_to(&to, ServerMessage::ConnectAccepted { session, peer: me, ice_servers: ice });
            } else {
                state.remove_session(&session);
                let reason = reason.unwrap_or_else(|| "reddedildi".into());
                state.send_to(&to, ServerMessage::ConnectRejected { peer: me, reason });
            }
        }

        ClientMessage::Signal { session, to, payload } => {
            let Some(me) = authed.clone() else {
                let _ = tx.send(not_authed());
                return;
            };
            let Some(s) = state.get_session(&session) else {
                let _ = tx.send(err(ErrorCode::UnknownSession, "oturum bulunamadı".into()));
                return;
            };
            if s.peer_of(&me) != Some(to.as_str()) {
                let _ = tx.send(err(ErrorCode::BadRequest, "bu oturumun tarafı değilsin".into()));
                return;
            }
            state.send_to(&to, ServerMessage::Signal { session, from: me, payload });
        }

        ClientMessage::Hangup { session } => {
            let Some(me) = authed.clone() else { return };
            if let Some(s) = state.remove_session(&session) {
                if let Some(peer) = s.peer_of(&me) {
                    state.send_to(peer, ServerMessage::Hangup { session });
                }
            }
        }

        ClientMessage::Ping => {
            let _ = tx.send(ServerMessage::Pong);
        }
    }
}

// ── küçük yardımcılar ────────────────────────────────────────────────────────

fn err(code: ErrorCode, message: String) -> ServerMessage {
    ServerMessage::Error { code, message }
}

fn not_authed() -> ServerMessage {
    err(ErrorCode::NotAuthenticated, "önce giriş yapmalısın".into())
}

fn protocol_mismatch() -> ServerMessage {
    err(ErrorCode::ProtocolMismatch, format!("sunucu protokol sürümü {PROTOCOL_VERSION}"))
}
