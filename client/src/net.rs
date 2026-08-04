//! Arka plan ağ motoru (sinyal + WebRTC) ve UI ile paylaşılan durum — `media` feature.
//!
//! eframe uygulaması ana thread'i sahiplenir; bu motor ayrı bir tokio runtime'ında çalışır.
//! İkisi arasında: UI -> motor `UiCommand` kanalı, motor -> UI `Arc<Mutex<UiState>>` + egui
//! `Context::request_repaint`. Motor, host/viewer rollerini ve SDP/ICE değiş tokuşunu tek
//! olay döngüsünde yürütür; gelen bağlantıda kullanıcıdan Kabul/Ret bekler.

use crate::frame::FrameBuffer;
use crate::input::{self, InputEvent};
use crate::video;
use crate::webrtc_conn::{new_peer_connection, to_rtc_ice_servers};
use anyhow::anyhow;
use away_shared::protocol::{ClientMessage, IceServer, ServerMessage, SignalPayload};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

use crate::signaling::Signaling;

/// Sinyal WebSocket'ini canlı tutan uygulama-seviyesi ping aralığı.
/// Boşta kalan WS'i Cloudflare ~100 sn, nginx varsayılanı 60 sn sonra kapatır;
/// bu aralık ikisinin de rahatça altında. (Ping olmadan oturum ~2 dk'da kopuyordu.)
const KEEPALIVE_SECS: u64 = 25;

/// Sinyal koparsa kullanıcıyı rahatsız etmeden kaç kez yeniden giriş denensin.
const RECONNECT_TRIES: u32 = 5;

/// UI'ın gösterdiği ekran (durum makinesi).
#[derive(Clone, Debug)]
pub enum Screen {
    /// Giriş formu (sunucu + kullanıcı + şifre).
    Login,
    /// Hesap oluşturma formu (sunucu + kullanıcı + şifre + şifre tekrar).
    Register,
    /// Sinyal bağlantısı koptu; otomatik yeniden giriş deneniyor.
    Reconnecting,
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
    /// VIEWER: fare/klavye olayını karşı tarafa ilet. Oturum yoksa yok sayılır.
    Input(InputEvent),
}

enum Role {
    Host,
    Viewer,
}

/// Giriş bilgileri — sinyal koparsa otomatik yeniden giriş için saklanır.
#[derive(Clone)]
struct Creds {
    server: String,
    user: String,
    pass: String,
}

/// P2P bağlantı durumu bildirimi (webrtc callback'i -> motor döngüsü).
/// Doğrudan UI'a yazmak yerine motora gidiyor; çünkü kopma hâlinde oturumun
/// motor tarafındaki kaydının da temizlenmesi gerekiyor.
type PcStateTx = UnboundedSender<RTCPeerConnectionState>;

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
    /// HOST: yakalama/encode thread'i bağlantı kurulunca başlatılır; kanalın gönderen
    /// ucu o ana kadar burada bekler. Erken başlatılırsa ilk IDR henüz bağlanmamış
    /// track'e yazılıp kaybolur ve izleyici uzun süre siyah ekran görür.
    capture_tx: Option<tokio::sync::mpsc::Sender<(Vec<u8>, Duration)>>,
    /// VIEWER: giriş olaylarının çıkış ucu. Oturum düşünce bu da düşer; karşı taraftaki
    /// enjektör thread'i kapanır ve basılı kalan tuşlar bırakılır.
    input_tx: Option<UnboundedSender<InputEvent>>,
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

/// Sunucuya bağlan, (istenirse) hesabı aç ve giriş yap.
async fn try_auth(c: &Creds, new_account: bool) -> anyhow::Result<Signaling> {
    let mut s = Signaling::connect(&c.server)
        .await
        .map_err(|e| anyhow!("sunucuya bağlanılamadı: {e}"))?;
    if new_account {
        s.register(&c.user, &c.pass).await?;
    }
    s.login(&c.user, &c.pass).await?;
    Ok(s)
}

/// Oturum açık hâle gelene kadar bekle. Elde kimlik varsa (kopma sonrası) önce
/// sessizce yeniden dener; olmazsa giriş formuna düşer ve kullanıcıyı bekler.
/// `None` yalnızca UI kapandığında döner.
async fn authenticate(
    shared: &Shared,
    ctx: &egui::Context,
    cmd_rx: &mut UnboundedReceiver<UiCommand>,
    creds: &mut Option<Creds>,
) -> Option<Signaling> {
    if let Some(c) = creds.clone() {
        for attempt in 1..=RECONNECT_TRIES {
            set_screen(
                shared,
                ctx,
                Screen::Reconnecting,
                format!("bağlantı koptu — yeniden bağlanılıyor ({attempt}/{RECONNECT_TRIES})"),
            );
            match try_auth(&c, false).await {
                Ok(sig) => {
                    {
                        let mut st = shared.lock().unwrap();
                        st.my_username = Some(c.user.clone());
                        st.screen = Screen::Home;
                        st.status = "yeniden bağlandı".into();
                    }
                    ctx.request_repaint();
                    return Some(sig);
                }
                Err(e) => tracing::warn!("yeniden bağlanma denemesi {attempt}: {e}"),
            }
            tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
        }
        shared.lock().unwrap().my_username = None;
        set_screen(shared, ctx, Screen::Login, "yeniden bağlanılamadı — tekrar giriş yap");
    }

    loop {
        // Giriş öncesi yalnızca Login/Register anlamlı; ikisi de aynı akışı izler
        // (kayıt varsa önce hesabı aç), sonuçta oturum açılırsa fonksiyon döner.
        let (c, new_account) = match cmd_rx.recv().await {
            Some(UiCommand::Login { server, user, pass }) => (Creds { server, user, pass }, false),
            Some(UiCommand::Register { server, user, pass }) => (Creds { server, user, pass }, true),
            Some(_) => continue, // giriş öncesi diğer komutlar yok sayılır
            None => return None,
        };
        // Hata durumunda kullanıcıyı geldiği forma geri gönder.
        let form = if new_account { Screen::Register } else { Screen::Login };
        set_status(
            shared,
            ctx,
            if new_account { "hesap oluşturuluyor…" } else { "sunucuya bağlanılıyor…" },
        );

        match try_auth(&c, new_account).await {
            Ok(sig) => {
                {
                    let mut st = shared.lock().unwrap();
                    st.my_username = Some(c.user.clone());
                    st.screen = Screen::Home;
                    st.status = "hazır".into();
                }
                ctx.request_repaint();
                *creds = Some(c);
                return Some(sig);
            }
            Err(e) => set_screen(shared, ctx, form, e.to_string()),
        }
    }
}

/// Motorun giriş noktası. Giriş -> ana döngü; sinyal koparsa yeniden giriş denenir.
/// Bu fonksiyon YALNIZCA pencere kapandığında döner — aksi hâlde motor ölür ve
/// kullanıcı bir daha hiçbir şeye bağlanamaz.
pub async fn run_engine(
    shared: Shared,
    mut cmd_rx: UnboundedReceiver<UiCommand>,
    frames: FrameBuffer,
    ctx: egui::Context,
    fps: u32,
    scale: Option<u32>,
) {
    let mut creds: Option<Creds> = None;
    // P2P durum bildirimleri; motorun ömrü boyunca açık kalır.
    let (pc_tx, mut pc_rx) = tokio::sync::mpsc::unbounded_channel::<RTCPeerConnectionState>();

    loop {
        let Some(mut sig) = authenticate(&shared, &ctx, &mut cmd_rx, &mut creds).await else {
            return; // UI kapandı
        };

        let mut session: Option<Session> = None;
        let mut incoming: Option<Incoming> = None;
        let out = sig.outbound.clone();
        let mut keepalive = tokio::time::interval(Duration::from_secs(KEEPALIVE_SECS));
        keepalive.tick().await; // ilk tick anında döner; onu harca

        // `true` -> pencere kapandı, `false` -> sinyal koptu (yeniden bağlan).
        let ui_closed = loop {
            tokio::select! {
                _ = keepalive.tick() => {
                    // Boşta kalan WS'i araya giren proxy'ler kapatmasın.
                    let _ = out.send(ClientMessage::Ping);
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        None => break true,
                        Some(c) => {
                            handle_command(c, &shared, &ctx, &frames, &out, &pc_tx, &mut session, &mut incoming).await;
                        }
                    }
                }
                msg = sig.inbound.recv() => {
                    match msg {
                        None => break false,
                        Some(m) => {
                            handle_server(m, &shared, &ctx, &frames, &out, &pc_tx, &mut session, &mut incoming).await;
                        }
                    }
                }
                st = pc_rx.recv() => {
                    match st {
                        // P2P hattı hazır. HOST yakalamayı ANCAK ŞİMDİ başlatır: ilk kare
                        // her zaman IDR'dır ve artık canlı track'e düşer → görüntü hemen açılır.
                        Some(RTCPeerConnectionState::Connected) => {
                            if let Some(s) = session.as_mut() {
                                if let Some(tx) = s.capture_tx.take() {
                                    crate::capture::spawn_capture_encoder(tx, fps, scale);
                                }
                            }
                            set_status(&shared, &ctx, "eş bağlantısı kuruldu");
                        }
                        // Sinyal ayakta olsa bile P2P yolu ölebilir (NAT/ağ değişimi, TURN yok).
                        Some(RTCPeerConnectionState::Failed) => {
                            if let Some(s) = session.take() {
                                let _ = out.send(ClientMessage::Hangup { session: s.id.clone() });
                                let _ = s.pc.close().await;
                            }
                            frames.take();
                            set_screen(&shared, &ctx, Screen::Message {
                                text: "eş bağlantısı koptu (doğrudan P2P yolu kurulamadı)".into(),
                                error: true,
                            }, "koptu");
                        }
                        _ => {}
                    }
                }
            }
        };

        // Kopma/çıkış temizliği: sinyalsiz oturum sürdürülemez.
        if let Some(s) = session.take() {
            let _ = s.pc.close().await;
        }
        frames.take();
        if ui_closed {
            return;
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
    pc_tx: &PcStateTx,
    session: &mut Option<Session>,
    incoming: &mut Option<Incoming>,
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
            match start_host(shared, ctx, out, pc_tx, inc).await {
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
        UiCommand::Input(ev) => {
            if let Some(tx) = session.as_ref().and_then(|s| s.input_tx.as_ref()) {
                let _ = tx.send(ev);
            }
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
    pc_tx: &PcStateTx,
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
            match start_viewer(shared, ctx, frames, out, pc_tx, sid, peer.clone(), ice_servers).await {
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

/// P2P durum değişimlerini motora ilet. `new_peer_connection` içindeki handler'ın
/// (yalnızca log basar) üzerine yazar — webrtc-rs'te bu handler tek yuvalıdır.
fn watch_pc_state(pc: &Arc<RTCPeerConnection>, pc_tx: PcStateTx) {
    pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
        tracing::info!("bağlantı durumu: {s}");
        let _ = pc_tx.send(s);
        Box::pin(async {})
    }));
}

/// HOST rolünü başlat: pc + ekran track'i + yakalama, kabul yanıtı, offer üretimi.
async fn start_host(
    shared: &Shared,
    ctx: &egui::Context,
    out: &UnboundedSender<ClientMessage>,
    pc_tx: &PcStateTx,
    inc: Incoming,
) -> anyhow::Result<Session> {
    let pc = new_peer_connection(
        to_rtc_ice_servers(&inc.ice),
        inc.session.clone(),
        inc.from.clone(),
        out.clone(),
    )
    .await?;
    watch_pc_state(&pc, pc_tx.clone());

    // Ekran track'i offer'dan ÖNCE eklenmeli ki SDP'de video yer alsın.
    let track = video::add_screen_track(&pc).await?;
    // Kapasite 1: kodlanmış kareler burada BİRİKMEMELİ. Yazıcı geride kalırsa
    // kuyruk doğrudan gecikmeye dönüşür (8'lik kuyruk 15 fps'te yarım saniye
    // demekti). Tek yuvayla yakalama döngüsü kendiliğinden yavaşlar, gecikme büyümez.
    let (enc_tx, enc_rx) = tokio::sync::mpsc::channel::<(Vec<u8>, Duration)>(1);
    video::spawn_sample_writer(track, enc_rx);
    // Yakalama BAŞLATILMAZ — bağlantı `Connected` olunca run_engine başlatır.

    // Giriş kanalı da offer'dan ÖNCE açılmalı (SDP'ye application m-line'ı girsin).
    // Kanalı host açar ama akış ters yönde: viewer yazar, host okur ve enjekte eder.
    let input_dc = pc.create_data_channel(input::CHANNEL, None).await?;
    input::wire_host_channel(&input_dc);

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
        capture_tx: Some(enc_tx),
        input_tx: None, // host giriş göndermez, alır
    })
}

/// VIEWER rolünü başlat: pc + gelen video track handler'ı. Offer HOST'tan gelecek (apply_signal).
#[allow(clippy::too_many_arguments)]
async fn start_viewer(
    shared: &Shared,
    ctx: &egui::Context,
    frames: &FrameBuffer,
    out: &UnboundedSender<ClientMessage>,
    pc_tx: &PcStateTx,
    sid: String,
    peer: String,
    ice: Vec<IceServer>,
) -> anyhow::Result<Session> {
    let pc = new_peer_connection(to_rtc_ice_servers(&ice), sid.clone(), peer.clone(), out.clone()).await?;
    watch_pc_state(&pc, pc_tx.clone());

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

    // Giriş kanalını host açar; biz yalnızca karşılayıp gönderici görevine bağlarız.
    // Kanal gelmeden UI'dan olay akmaya başlayabileceği için alıcı uç şimdiden kurulur
    // ve kanal gelene kadar olaylar `spawn_sender` içinde düşürülür.
    let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<InputEvent>();
    let in_rx = Arc::new(Mutex::new(Some(in_rx)));
    pc.on_data_channel(Box::new(move |dc| {
        let slot = in_rx.clone();
        Box::pin(async move {
            if dc.label() != input::CHANNEL {
                return;
            }
            // Alıcı uç tektir: kanal bir kez bağlanır, tekrar gelirse yok sayılır.
            let rx = slot.lock().unwrap().take();
            if let Some(rx) = rx {
                input::spawn_sender(dc, rx);
            }
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
        capture_tx: None, // viewer ekran yakalamaz
        input_tx: Some(in_tx),
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
