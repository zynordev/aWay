//! Sinyal sunucusuna WebSocket istemcisi. Giden `ClientMessage`'ları yazar,
//! gelen `ServerMessage`'ları bir kanala aktarır.

use anyhow::{anyhow, Result};
use away_shared::protocol::{ClientMessage, ServerMessage};
use away_shared::PROTOCOL_VERSION;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

pub struct Signaling {
    /// Sunucuya mesaj göndermek için (webrtc callback'lerine klonlanabilir).
    pub outbound: mpsc::UnboundedSender<ClientMessage>,
    /// Sunucudan gelen mesajlar.
    pub inbound: mpsc::UnboundedReceiver<ServerMessage>,
}

impl Signaling {
    pub async fn connect(url: &str) -> Result<Self> {
        let (ws, _) = connect_async(url).await?;
        let (mut sink, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<ServerMessage>();

        // Yazıcı: giden ClientMessage -> WS text
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                match serde_json::to_string(&msg) {
                    Ok(json) => {
                        if sink.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => tracing::error!("serialize: {e}"),
                }
            }
        });

        // Okuyucu: WS text -> gelen ServerMessage
        tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                match msg {
                    Message::Text(t) => match serde_json::from_str::<ServerMessage>(t.as_str()) {
                        Ok(sm) => {
                            if in_tx.send(sm).is_err() {
                                break;
                            }
                        }
                        Err(e) => tracing::warn!("çözümlenemeyen mesaj: {e}"),
                    },
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        });

        Ok(Self { outbound: out_tx, inbound: in_rx })
    }

    pub fn send(&self, msg: ClientMessage) -> Result<()> {
        self.outbound.send(msg).map_err(|_| anyhow!("sinyal bağlantısı kapandı"))
    }

    /// Giriş yap; başarılıysa session_token döner.
    pub async fn login(&mut self, username: &str, password: &str) -> Result<String> {
        self.send(ClientMessage::Login {
            protocol_version: PROTOCOL_VERSION,
            username: username.to_string(),
            password: password.to_string(),
        })?;
        loop {
            match self.inbound.recv().await {
                Some(ServerMessage::LoggedIn { session_token, .. }) => return Ok(session_token),
                Some(ServerMessage::Error { code, message }) => {
                    return Err(anyhow!("giriş hatası {code:?}: {message}"))
                }
                Some(_) => continue,
                None => return Err(anyhow!("bağlantı kapandı")),
            }
        }
    }
}
