//! Arka plan ağ motoru (sinyal + WebRTC) ve UI ile paylaşılan durum — `media` feature.
//!
//! eframe uygulaması ana thread'i sahiplenir; bu motor ayrı bir tokio runtime'ında çalışır.
//! İkisi arasında: UI -> motor `UiCommand` kanalı, motor -> UI `Arc<Mutex<UiState>>` + egui
//! `Context::request_repaint`. Motor, host/viewer rollerini ve SDP/ICE değiş tokuşunu tek
//! olay döngüsünde yürütür; gelen bağlantıda kullanıcıdan Kabul/Ret bekler.

use crate::frame::FrameBuffer;
use crate::video;
use crate::webrtc_conn::{new_peer_connection, to_rtc_ice_servers};
use away_shared::protocol::{ClientMessage, IceServer, ServerMessage, SignalPayload};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

use crate::signaling::Signaling;

/// UI'ın gösterdiği ekran (durum makinesi).
#[derive(Clone, Debug)]
pub enum Screen {
    /// Giriş formu (sunucu + kullanıcı + şifre).
    Login,
    /// Hesap oluşturma formu (sunucu + kullanıcı + şifre + şifre tekrar).
    Register,
    /// Ana ekran: kendi kullanıcı adın + bağlan kutusu; gelen bağlantıyı dinler.
    Home,
    /// Giden bağlantı kuruluyor.
    Connecting { peer: String },
    /// Gelen bağlantı isteği — Kabul/Ret bekleniyor.
    Incoming { from: String },
    /// Uzak ekran izleniyor (viewer).
    RemoteScreen { peer: String },
    /// Kendi ekranın paylaşılıyor (host).
    Sharing { peer: String },
    /// Bilgilendirme/hata ekranı (Tamam -> Home).
    Message { text: String, error: bool },
}

/// UI ile paylaşılan durum. Motor yazar, UI her çizimde okur.
#[derive(Clone)]
pub struct UiState {
    pub screen: Screen,
    pub my_username: Option<String>,
    pub status: String,
}

impl Default for UiState {
    fn default() -> Self {
        Self { screen: Screen::Login, my_username: None, status: String::new() }
    }
}

pub type Shared = Arc<Mutex<UiState>>;

/// UI -> motor komutları.
pub enum UiCommand {
    Login { server: String, user: String, pass: String },
    /// Hesap oluştur, ardından aynı bilgilerle otomatik giriş yap.
    Register { server: String, user: String, pass: String },
    Connect { to: String },
    Accept,
    Hangup,
    Reject,
}

enum Role {
    Host,
    Viewer,
}

/// Aktif oturum durumu (tek seferde bir oturum).
struct Session {
    id: String,
    peer: String,
    /// İleride renegotiation/rol-bazlı davranış için tutuluyor.
    #[allow(dead_code)]
    role: Role,
    pc: Arc<RTCPeerConnection>,
    remote_set: bool,
    pending: Vec<RTCIceCandidateInit>,
}

/// Kabul bekleyen gelen istek.
struct Incoming {
    session: String,
    from: String,
    ice: Vec<IceServer>,
}

/// Kısa durum güncellemesi + yeniden çizim.
fn set_screen(shared: &Shared, ctx: &egui::Context, screen: Screen, status: impl Into<String>) {
    {
        let mut s = shared.lock().unwrap();
        s.screen = screen;
        s.status = status.into();
    }
    ctx.request_repaint();
}

fn set_status(shared: &Shared, ctx: &egui::Context, status: impl Into<String>) {
    shared.lock().unwrap().status = status.into();
    ctx.request_repaint();
}

/// Motorun giriş noktası. Login komutu gelene kadar bekler; giriş başarılıysa ana döngüye geçer.
pub async fn run_engine(
    shared: Shared,
    mut cmd_rx: UnboundedReceiver<UiCommand>,
    frames: FrameBuffer,
    ctx: egui::Context,
    fps: u32,
) {
    let mut sig = loop {
        // Giriş öncesi yalnızca Login/Register anlamlı; ikisi de aynı akışı izler
        // (kayıt varsa önce hesabı aç), sonuçta oturum açılırsa döngüden çıkılır.
        let (server, user, pass, new_account) = match cmd_rx.recv().await {
            Some(UiCommand::Login { server, user, pass }) => (server, user, pass, false),
            Some(UiCommand::Register { server, user, pass }) => (server, user, pass, true),
            Some(_) => continue, // giriş öncesi diğer komutlar yok sayılır
            None => return,
        };
        // Hata durumunda kullanıcıyı geldiği forma geri gönder.
        let form = if new_account { Screen::Register } else { Screen::Login };

        set_status(&shared, &ctx, "sunucuya bağlanılıyor…");
        let mut s = match Signaling::connect(&server).await {
            Ok(s) => s,
            Err(e) => {
                set_screen(&shared, &ctx, form.clone(), format!("bağlanılamadı: {e}"));
                continue;
            }
        };

        if new_account {
            set_status(&shared, &ctx, "hesap oluşturuluyor…");
            if let Err(e) = s.register(&user, &pass).await {
                set_screen(&shared, &ctx, form.clone(), format!("hesap oluşturulamadı: {e}"));
                continue;
            }
            set_status(&shared, &ctx, "hesap açıldı, giriş yapılıyor…");
        }

        match s.login(&user, &pass).await {
            Ok(_) => {
                {
                    let mut st = shared.lock().unwrap();
                    st.my_username = Some(user.clone());
                    st.screen = Screen::Home;
                    st.status = "hazır".into();
                }
                ctx.request_repaint();
                break s;
            }
            Err(e) => {
                set_screen(&shared, &ctx, form.clone(), format!("giriş başarısız: {e}"));
                continue;
            }
        }
    };

    let mut session: Option<Session> = None;
    let mut incoming: Option<Incoming> = None;
    let out = sig.outbound.clone();

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    None => break,
                    Some(c) => {
                        handle_command(c, &shared, &ctx, &frames, &out, &mut session, &mut incoming, fps).await;
                    }
                }
            }
            msg = sig.inbound.recv() => {
                match msg {
                    None => {
                        set_screen(&shared, &ctx, Screen::Message {
                            text: "sunucu bağlantısı kapandı".into(), error: true,
                        }, "kopuk");
                        break;
                    }
                    Some(m) => {
                        handle_server(m, &shared, &ctx, &frames, &out, &mut session, &mut incoming).await;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_command(
    cmd: UiCommand,
    shared: &Shared,
    ctx: &egui::Context,
    frames: &FrameBuffer,
    out: &UnboundedSender<ClientMessage>,
    session: &mut Option<Session>,
    incoming: &mut Option<Incoming>,
    fps: u32,
) {
    match cmd {
        UiCommand::Login { .. } | UiCommand::Register { .. } => {} // zaten giriş yapıldı
        UiCommand::Connect { to } => {
            if session.is_some() {
                set_status(shared, ctx, "zaten bir oturumdasın");
                return;
            }
            if to.trim().is_empty() {
                set_status(shared, ctx, "kullanıcı adı boş");
                return;
            }
            let _ = out.send(ClientMessage::Connect { to: to.clone() });
            set_screen(shared, ctx, Screen::Connecting { peer: to }, "bağlantı isteği gönderildi…");
        }
        UiCommand::Accept => {
            let Some(inc) = incoming.take() else { return };
            match start_host(shared, ctx, out, inc, fps).await {
                Ok(s) => *session = Some(s),
                Err(e) => set_screen(shared, ctx, Screen::Message {
                    text: format!("paylaşım başlatılamadı: {e}"), error: true,
                }, "hata"),
            }
        }
        UiCommand::Reject => {
            if let Some(inc) = incoming.take() {
                let _ = out.send(ClientMessage::ConnectResponse {
                    session: inc.session,
                    to: inc.from,
                    accept: false,
                    reason: Some("reddedildi".into()),
                });
            }
            set_screen(shared, ctx, Screen::Home, "hazır");
        }
        UiCommand::Hangup => {
            if let Some(s) = session.take() {
                let _ = out.send(ClientMessage::Hangup { session: s.id.clone() });
                let _ = s.pc.close().await;
            }
            frames.take(); // son kareyi temizle
            set_screen(shared, ctx, Screen::Home, "bağlantı kapatıldı");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_server(
    msg: ServerMessage,
    shared: &Shared,
    ctx: &egui::Context,
    frames: &FrameBuffer,
    out: &UnboundedSender<ClientMessage>,
    session: &mut Option<Session>,
    incoming: &mut Option<Incoming>,
) {
    match msg {
        ServerMessage::IncomingConnect { session: sid, from, ice_servers } => {
            if session.is_some() || incoming.is_some() {
                // meşgul → otomatik ret
                let _ = out.send(ClientMessage::ConnectResponse {
                    session: sid,
                    to: from,
                    accept: false,
                    reason: Some("meşgul".into()),
                });
                return;
            }
            *incoming = Some(Incoming { session: sid, from: from.clone(), ice: ice_servers });
            set_screen(shared, ctx, Screen::Incoming { from: from.clone() },
                format!("{from} bağlanmak istiyor"));
        }
        ServerMessage::ConnectAccepted { session: sid, peer, ice_servers } => {
            // Biz viewer'ız: pc kur, video track'i bekle. Offer'ı HOST üretecek.
            match start_viewer(shared, ctx, frames, out, sid, peer.clone(), ice_servers).await {
                Ok(s) => *session = Some(s),
                Err(e) => set_screen(shared, ctx, Screen::Message {
                    text: format!("bağlantı kurulamadı: {e}"), error: true,
                }, "hata"),
            }
        }
        ServerMessage::ConnectRejected { reason, .. } => {
            set_screen(shared, ctx, Screen::Message {
                text: format!("bağlantı reddedildi: {reason}"), error: true,
            }, "reddedildi");
        }
        ServerMessage::Signal { payload, .. } => {
            if let Some(s) = session.as_mut() {
                apply_signal(s, payload, out).await;
            }
        }
        ServerMessage::Hangup { .. } => {
            if let Some(s) = session.take() {
                let _ = s.pc.close().await;
            }
            frames.take();
            set_screen(shared, ctx, Screen::Message {
                text: "karşı taraf bağlantıyı kapattı".into(), error: false,
            }, "kapandı");
        }
        ServerMessage::Error { message, .. } => {
            set_status(shared, ctx, format!("sunucu: {message}"));
        }
        _ => {} // Presence, Pong, Registered, LoggedIn: yok say
    }
}

/// HOST rolünü başlat: pc + ekran track'i + yakalama, kabul yanıtı, offer üretimi.
async fn start_host(
    shared: &Shared,
    ctx: &egui::Context,
    out: &UnboundedSender<ClientMessage>,
    inc: Incoming,
    fps: u32,
) -> anyhow::Result<Session> {
    let pc = new_peer_connection(
        to_rtc_ice_servers(&inc.ice),
        inc.session.clone(),
        inc.from.clone(),
        out.clone(),
    )
    .await?;

    // Ekran track'i offer'dan ÖNCE eklenmeli ki SDP'de video yer alsın.
    let track = video::add_screen_track(&pc).await?;
    let (enc_tx, enc_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
    video::spawn_sample_writer(track, enc_rx, fps);
    crate::capture::spawn_capture_encoder(enc_tx, fps);

    out.send(ClientMessage::ConnectResponse {
        session: inc.session.clone(),
        to: inc.from.clone(),
        accept: true,
        reason: None,
    })?;

    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer.clone()).await?;
    out.send(ClientMessage::Signal {
        session: inc.session.clone(),
        to: inc.from.clone(),
        payload: SignalPayload::Offer { sdp: offer.sdp },
    })?;

    set_screen(shared, ctx, Screen::Sharing { peer: inc.from.clone() },
        format!("{} ekranını izliyor", inc.from));

    Ok(Session {
        id: inc.session,
        peer: inc.from,
        role: Role::Host,
        pc,
        remote_set: false,
        pending: Vec::new(),
    })
}

/// VIEWER rolünü başlat: pc + gelen video track handler'ı. Offer HOST'tan gelecek (apply_signal).
async fn start_viewer(
    shared: &Shared,
    ctx: &egui::Context,
    frames: &FrameBuffer,
    out: &UnboundedSender<ClientMessage>,
    sid: String,
    peer: String,
    ice: Vec<IceServer>,
) -> anyhow::Result<Session> {
    let pc = new_peer_connection(to_rtc_ice_servers(&ice), sid.clone(), peer.clone(), out.clone()).await?;

    let frames_cb = frames.clone();
    let ctx_cb = ctx.clone();
    let shared_cb = shared.clone();
    let peer_cb = peer.clone();
    pc.on_track(Box::new(move |track, _receiver, _transceiver| {
        let frames = frames_cb.clone();
        let ctx = ctx_cb.clone();
        let shared = shared_cb.clone();
        let peer = peer_cb.clone();
        Box::pin(async move {
            set_screen(&shared, &ctx, Screen::RemoteScreen { peer: peer.clone() },
                format!("{peer} ekranı"));
            video::on_video_track(track, frames, ctx);
        })
    }));

    set_screen(shared, ctx, Screen::Connecting { peer: peer.clone() }, "kabul edildi, video bekleniyor…");

    Ok(Session {
        id: sid,
        peer,
        role: Role::Viewer,
        pc,
        remote_set: false,
        pending: Vec::new(),
    })
}

/// Bir SDP/ICE sinyalini aktif oturuma uygula (offer→answer, answer→setRemote, ice→buffer/add).
async fn apply_signal(s: &mut Session, payload: SignalPayload, out: &UnboundedSender<ClientMessage>) {
    match payload {
        SignalPayload::Offer { sdp } => {
            // Yalnızca viewer offer bekler; yine de savunmacı davran.
            if let Ok(desc) = RTCSessionDescription::offer(sdp) {
                if let Err(e) = s.pc.set_remote_description(desc).await {
                    tracing::warn!("set_remote(offer): {e}");
                    return;
                }
                match s.pc.create_answer(None).await {
                    Ok(answer) => {
                        if s.pc.set_local_description(answer.clone()).await.is_ok() {
                            let _ = out.send(ClientMessage::Signal {
                                session: s.id.clone(),
                                to: s.peer.clone(),
                                payload: SignalPayload::Answer { sdp: answer.sdp },
                            });
                        }
                    }
                    Err(e) => tracing::warn!("create_answer: {e}"),
                }
                s.remote_set = true;
                flush_pending(s).await;
            }
        }
        SignalPayload::Answer { sdp } => {
            if let Ok(desc) = RTCSessionDescription::answer(sdp) {
                if let Err(e) = s.pc.set_remote_description(desc).await {
                    tracing::warn!("set_remote(answer): {e}");
                    return;
                }
                s.remote_set = true;
                flush_pending(s).await;
            }
        }
        SignalPayload::IceCandidate { candidate, sdp_mid, sdp_mline_index } => {
            let init = RTCIceCandidateInit {
                candidate,
                sdp_mid,
                sdp_mline_index,
                username_fragment: None,
            };
            if s.remote_set {
                if let Err(e) = s.pc.add_ice_candidate(init).await {
                    tracing::warn!("add_ice_candidate: {e}");
                }
            } else {
                s.pending.push(init);
            }
        }
    }
}

async fn flush_pending(s: &mut Session) {
    for init in s.pending.drain(..) {
        if let Err(e) = s.pc.add_ice_candidate(init).await {
            tracing::warn!("tamponlanmış ice: {e}");
        }
    }
}
