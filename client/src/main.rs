//! aWay native istemci.
//!
//! `media` feature (Windows/Linux geliştirme makinesi, `--features media`): AnyDesk tarzı
//! **tek pencereli GUI**. Açılınca giriş → ana ekran (kendi kullanıcı adın + bağlan kutusu,
//! gelen bağlantıları dinler) → gelen için Kabul/Ret → uzak ekran / ekran paylaşımı.
//! Bağlantıyı VIEWER başlatır, WebRTC offer'ını ekrana sahip HOST üretir.
//!   away-client --features media --user murat --pass p1
//!   (opsiyonel otomatik bağlan: --connect ahmet)
//!
//! Çekirdek (media KAPALI): sinyal + WebRTC data channel taşıma testi (M2), GUI'siz:
//!   away-client --user murat --pass p1 --connect ahmet   # arayan
//!   away-client --user ahmet --pass p2                    # bekleyen

mod signaling;
mod webrtc_conn;

#[cfg(feature = "media")]
mod app;
#[cfg(feature = "media")]
mod capture;
#[cfg(feature = "media")]
mod convert;
#[cfg(feature = "media")]
mod decode;
#[cfg(feature = "media")]
mod encode;
#[cfg(feature = "media")]
mod frame;
#[cfg(feature = "media")]
mod input;
#[cfg(feature = "media")]
mod net;
#[cfg(feature = "media")]
mod video;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Clone)]
#[command(name = "away-client", about = "aWay istemci")]
struct Args {
    /// Sinyal sunucusu. Varsayılan: canlı VDS köprüsü. Yerel test için:
    /// --server ws://127.0.0.1:9000/ws
    #[arg(long, env = "AWAY_SERVER", default_value = "wss://away.bilgicoderteam.tr/ws")]
    server: String,
    #[arg(long, default_value = "")]
    user: String,
    #[arg(long, default_value = "")]
    pass: String,
    /// Bağlanılacak kullanıcı adı. (media) verilirse giriş sonrası otomatik bağlanır;
    /// (çekirdek) verilirse arayan, verilmezse bekleyen olur.
    #[arg(long)]
    connect: Option<String>,
    /// (media) Paylaşım kare hızı. Yakalama+encode tamamen yazılımsal olduğundan CPU
    /// maliyeti doğrudan bununla orantılı; masaüstü için 15 akıcı ve belirgin şekilde
    /// ucuz. Güçlü makinede `--fps 30` denenebilir.
    #[arg(long, default_value_t = 15)]
    fps: u32,
    /// (media) Görüntüyü kaç kat küçülterek gönder (1 = tam çözünürlük, 2 = yarı…).
    /// Verilmezse otomatik: 2560 pikselden geniş ekranlar (4K) yarıya iner, 1080p ve
    /// 1440p tam çözünürlük gider. CPU ve gecikme doğrudan piksel sayısıyla orantılıdır —
    /// makine yetişemiyorsa `--scale 2` belirgin rahatlama sağlar.
    #[arg(long)]
    scale: Option<u32>,
    /// (media) Hedef bit hızı (kbps). Verilmezse çözünürlük ve fps'ten hesaplanır
    /// (~0,15 bit/piksel/kare, en çok 5000). Görüntü bulanık/bloklu geliyorsa artır;
    /// internet yüklemesi yetişmiyorsa (kareler geç geliyor) düşür — hattı aşan bit
    /// hızı kaliteyi artırmaz, sadece kuyruk oluşturup gecikmeye dönüşür.
    #[arg(long)]
    bitrate: Option<u32>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,away_client=debug".into()),
        )
        .init();

    let args = Args::parse();

    #[cfg(feature = "media")]
    {
        run_gui(args)
    }
    #[cfg(not(feature = "media"))]
    {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        rt.block_on(core_main(args))
    }
}

// ---------------------------------------------------------------------------
// media: tek pencereli GUI başlatıcı
// ---------------------------------------------------------------------------

#[cfg(feature = "media")]
fn run_gui(args: Args) -> Result<()> {
    use anyhow::anyhow;
    use frame::FrameBuffer;
    use net::{Shared, UiCommand, UiState};
    use std::sync::{Arc, Mutex};

    let shared: Shared = Arc::new(Mutex::new(UiState::default()));
    let frames = FrameBuffer::default();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<UiCommand>();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("aWay")
            .with_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    let shared_app = shared.clone();
    let frames_app = frames.clone();
    let cmd_tx_app = cmd_tx.clone();
    let server = args.server.clone();
    let user = args.user.clone();
    let pass = args.pass.clone();
    let auto_peer = args.connect.clone();
    let video = capture::VideoOpts {
        fps: args.fps,
        scale: args.scale,
        bitrate_kbps: args.bitrate,
    };

    eframe::run_native(
        "aWay",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            // Arka plan ağ motoru (kendi tokio runtime'ında).
            let shared_e = shared.clone();
            let frames_e = frames.clone();
            std::thread::spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("tokio runtime: {e}");
                        return;
                    }
                };
                rt.block_on(net::run_engine(shared_e, cmd_rx, frames_e, ctx, video));
            });

            // Kimlik bilgileri argümanla verildiyse otomatik giriş.
            if !user.is_empty() && !pass.is_empty() {
                let _ = cmd_tx_app.send(UiCommand::Login {
                    server: server.clone(),
                    user: user.clone(),
                    pass: pass.clone(),
                });
            }

            Ok(Box::new(app::AwayApp::new(
                shared_app.clone(),
                cmd_tx_app.clone(),
                frames_app.clone(),
                server.clone(),
                user.clone(),
                pass.clone(),
                auto_peer.clone(),
            )))
        }),
    )
    .map_err(|e| anyhow!("eframe: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// çekirdek (media kapalı): M2 data channel taşıma testi
// ---------------------------------------------------------------------------

#[cfg(not(feature = "media"))]
mod core_client {
    use super::Args;
    use anyhow::{anyhow, Result};
    use away_shared::protocol::{ClientMessage, ServerMessage, SignalPayload};
    use std::sync::Arc;
    use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
    use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
    use webrtc::peer_connection::RTCPeerConnection;

    use crate::signaling::Signaling;
    use crate::webrtc_conn::{new_peer_connection, to_rtc_ice_servers, wire_data_channel};

    pub async fn core_main(args: Args) -> Result<()> {
        let mut sig = Signaling::connect(&args.server).await?;
        sig.login(&args.user, &args.pass).await?;
        tracing::info!("giriş başarılı: {}", args.user);
        match args.connect.clone() {
            Some(peer) => run_caller(sig, peer).await,
            None => run_callee(sig).await,
        }
    }

    /// Arayan taraf: bağlantı isteği başlatır, kabul gelince offer + data channel üretir.
    async fn run_caller(mut sig: Signaling, peer: String) -> Result<()> {
        sig.send(ClientMessage::Connect { to: peer.clone() })?;

        let (session, ice) = loop {
            match sig.inbound.recv().await {
                Some(ServerMessage::ConnectAccepted { session, ice_servers, .. }) => {
                    break (session, ice_servers)
                }
                Some(ServerMessage::ConnectRejected { reason, .. }) => {
                    return Err(anyhow!("bağlantı reddedildi: {reason}"))
                }
                Some(ServerMessage::Error { message, .. }) => return Err(anyhow!("hata: {message}")),
                Some(_) => continue,
                None => return Err(anyhow!("bağlantı kapandı")),
            }
        };
        tracing::info!("kabul edildi (oturum {session}) → offer üretiliyor");

        let pc = new_peer_connection(
            to_rtc_ice_servers(&ice),
            session.clone(),
            peer.clone(),
            sig.outbound.clone(),
        )
        .await?;

        let dc = pc.create_data_channel("control", None).await?;
        wire_data_channel(dc, format!("merhaba {peer}, ben arayan"));

        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer.clone()).await?;
        sig.send(ClientMessage::Signal {
            session: session.clone(),
            to: peer.clone(),
            payload: SignalPayload::Offer { sdp: offer.sdp },
        })?;

        signal_loop(&mut sig, &pc, &session, &peer).await
    }

    /// Bekleyen taraf: gelen isteği otomatik kabul eder, offer gelince answer üretir.
    async fn run_callee(mut sig: Signaling) -> Result<()> {
        let (session, from, ice) = loop {
            match sig.inbound.recv().await {
                Some(ServerMessage::IncomingConnect { session, from, ice_servers }) => {
                    break (session, from, ice_servers)
                }
                Some(_) => continue,
                None => return Err(anyhow!("bağlantı kapandı")),
            }
        };
        tracing::info!("gelen bağlantı: {from} — otomatik kabul (test)");

        let pc = new_peer_connection(
            to_rtc_ice_servers(&ice),
            session.clone(),
            from.clone(),
            sig.outbound.clone(),
        )
        .await?;

        pc.on_data_channel(Box::new(move |dc| {
            wire_data_channel(dc, "merhaba arayan, ben bekleyen".to_string());
            Box::pin(async {})
        }));

        sig.send(ClientMessage::ConnectResponse {
            session: session.clone(),
            to: from.clone(),
            accept: true,
            reason: None,
        })?;

        signal_loop(&mut sig, &pc, &session, &from).await
    }

    /// SDP/ICE değiş tokuşunu işleyen ortak döngü (candidate tamponlama dâhil).
    async fn signal_loop(
        sig: &mut Signaling,
        pc: &Arc<RTCPeerConnection>,
        session: &str,
        peer: &str,
    ) -> Result<()> {
        let mut remote_set = false;
        let mut pending: Vec<RTCIceCandidateInit> = Vec::new();

        while let Some(msg) = sig.inbound.recv().await {
            match msg {
                ServerMessage::Signal { payload, .. } => match payload {
                    SignalPayload::Offer { sdp } => {
                        pc.set_remote_description(RTCSessionDescription::offer(sdp)?).await?;
                        let answer = pc.create_answer(None).await?;
                        pc.set_local_description(answer.clone()).await?;
                        sig.send(ClientMessage::Signal {
                            session: session.to_string(),
                            to: peer.to_string(),
                            payload: SignalPayload::Answer { sdp: answer.sdp },
                        })?;
                        remote_set = true;
                        flush_pending(pc, &mut pending).await;
                    }
                    SignalPayload::Answer { sdp } => {
                        pc.set_remote_description(RTCSessionDescription::answer(sdp)?).await?;
                        remote_set = true;
                        flush_pending(pc, &mut pending).await;
                    }
                    SignalPayload::IceCandidate { candidate, sdp_mid, sdp_mline_index } => {
                        let init = RTCIceCandidateInit {
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                            username_fragment: None,
                        };
                        if remote_set {
                            if let Err(e) = pc.add_ice_candidate(init).await {
                                tracing::warn!("add_ice_candidate: {e}");
                            }
                        } else {
                            pending.push(init);
                        }
                    }
                },
                ServerMessage::Hangup { .. } => {
                    tracing::info!("karşı taraf oturumu kapattı");
                    break;
                }
                ServerMessage::Error { message, .. } => tracing::warn!("sunucu hatası: {message}"),
                _ => {}
            }
        }
        Ok(())
    }

    async fn flush_pending(pc: &Arc<RTCPeerConnection>, pending: &mut Vec<RTCIceCandidateInit>) {
        for init in pending.drain(..) {
            if let Err(e) = pc.add_ice_candidate(init).await {
                tracing::warn!("tamponlanmış ice: {e}");
            }
        }
    }
}

#[cfg(not(feature = "media"))]
use core_client::core_main;
