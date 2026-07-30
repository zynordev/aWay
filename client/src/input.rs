//! Uzaktan giriş: fare + klavye — `media` feature.
//!
//! Akış: VIEWER'daki egui olayları `InputEvent`e çevrilir, WebRTC **data channel**'ından
//! JSON olarak gider, HOST'ta `enigo` ile işletim sistemine enjekte edilir. Video track'i
//! yerine ayrı bir data channel kullanılır: giriş olayları küçük ama sıralı ve kayıpsız
//! olmalı (bir "tuş bırakıldı" kaybolursa tuş sonsuza dek basılı kalır).
//!
//! Kanalı HOST açar, çünkü offer'ı da HOST üretiyor; data channel'ın SDP'de yer alması için
//! offer'dan önce oluşturulması gerekir. Kanal çift yönlü olduğundan viewer'ın göndermesine
//! engel değil.
//!
//! Koordinatlar 0..1 aralığında normalize taşınır: izleyicinin penceresi ile host'un ekranı
//! farklı boyutta/oranda olabilir, piksele çevirmeyi host kendi ekran boyutuyla yapar.

use anyhow::Result;
use bytes::Bytes;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key as EKey, Keyboard, Mouse, Settings};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::data_channel_state::RTCDataChannelState;
use webrtc::data_channel::RTCDataChannel;

/// Giriş kanalının etiketi. Host açar, viewer `on_data_channel` ile bu etiketten tanır.
pub const CHANNEL: &str = "input";

/// Fare tuşu (izleyicinin gördüğü mantıksal tuş).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Karakter üretmeyen tuşlar. Harf/rakam/noktalama bunlara girmez; onlar
/// [`InputEvent::Text`] (düz yazı) veya [`InputEvent::Char`] (kısayol) ile taşınır.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyCode {
    Ctrl,
    Shift,
    Alt,
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    Space,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// Viewer -> host giriş olayı.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum InputEvent {
    /// İmleci taşı. `x`/`y` uzak ekrana göre 0..1.
    Move { x: f32, y: f32 },
    /// Fare tuşu. Konum da taşınır: tıklamanın son `Move`'u beklememesi için.
    Button { b: MouseButton, x: f32, y: f32, down: bool },
    /// Tekerlek. Birim: 15°'lik bir "klik". Pozitif `dy` aşağı, pozitif `dx` sağa.
    Scroll { dx: i32, dy: i32 },
    /// Özel tuş (bas/bırak ayrı gelir ki basılı tutmalar çalışsın).
    Key { k: KeyCode, down: bool },
    /// Kısayoldaki karakter tuşu (Ctrl+C gibi). Düzeni host çözer.
    Char { c: char, down: bool },
    /// Düz metin girişi. Klavye düzenini İZLEYİCİ çözdüğü için Türkçe karakterler
    /// (ğ, ı, ş…) host'un düzeninden bağımsız olarak doğru gider.
    Text { s: String },
}

// ---------------------------------------------------------------------------
// VIEWER tarafı: olayları data channel'a yaz
// ---------------------------------------------------------------------------

/// Giriş olaylarını data channel'a yazan görevi başlatır. `rx` kapanınca (oturum bitti) durur.
pub fn spawn_sender(dc: Arc<RTCDataChannel>, mut rx: UnboundedReceiver<InputEvent>) {
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            // Kanal henüz açılmadıysa olayı düşür. Giriş anlıktır; kuyruğa alıp sonra
            // uygulamak faydasız (geciken tıklama yanlış yere düşer).
            if dc.ready_state() != RTCDataChannelState::Open {
                continue;
            }
            match serde_json::to_vec(&ev) {
                Ok(bytes) => {
                    if let Err(e) = dc.send(&Bytes::from(bytes)).await {
                        tracing::warn!("giriş gönderilemedi: {e}");
                        break;
                    }
                }
                Err(e) => tracing::warn!("giriş serileştirilemedi: {e}"),
            }
        }
        tracing::info!("giriş göndericisi durdu");
    });
}

// ---------------------------------------------------------------------------
// HOST tarafı: gelen olayları işletim sistemine enjekte et
// ---------------------------------------------------------------------------

/// Gelen giriş kanalını enjektöre bağlar (host).
pub fn wire_host_channel(dc: &Arc<RTCDataChannel>) {
    let tx = spawn_injector();
    dc.on_message(Box::new(move |msg: DataChannelMessage| {
        match serde_json::from_slice::<InputEvent>(&msg.data) {
            Ok(ev) => {
                let _ = tx.send(ev);
            }
            Err(e) => tracing::debug!("bozuk giriş mesajı: {e}"),
        }
        Box::pin(async {})
    }));
}

/// Enjeksiyon thread'ini başlatır.
///
/// `Enigo` platforma özgü bir bağlantı tutar (X11'de kendi soketi) ve thread'ler arasında
/// dolaştırılmamalı; bu yüzden tek bir std thread'e hapsedilir ve olaylar kanalla gelir.
/// Gönderen uç düşünce (oturum kapandı) döngü biter, `Enigo` drop olur ve basılı kalan
/// tuşlar serbest bırakılır — `Settings::release_keys_when_dropped` varsayılanı bunu yapar.
/// Bu önemli: kullanıcı Ctrl basılıyken bağlantıyı keserse karşı makinede Ctrl asılı kalmaz.
fn spawn_injector() -> UnboundedSender<InputEvent> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<InputEvent>();
    std::thread::spawn(move || {
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("giriş enjektörü açılamadı: {e}");
                return;
            }
        };
        // Normalize koordinatları piksele çevirmek için hedef ekran boyutu.
        let (w, h) = match enigo.main_display() {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("ekran boyutu alınamadı: {e}");
                return;
            }
        };
        tracing::info!("giriş enjektörü hazır ({w}x{h})");

        while let Some(ev) = rx.blocking_recv() {
            if let Err(e) = apply(&mut enigo, ev, w, h) {
                // Tek bir olayın başarısız olması oturumu bitirmemeli.
                tracing::debug!("giriş enjeksiyonu: {e}");
            }
        }
        tracing::info!("giriş enjektörü durdu");
    });
    tx
}

fn apply(enigo: &mut Enigo, ev: InputEvent, w: i32, h: i32) -> Result<()> {
    match ev {
        InputEvent::Move { x, y } => {
            let (px, py) = to_pixels(x, y, w, h);
            enigo.move_mouse(px, py, Coordinate::Abs)?;
        }
        InputEvent::Button { b, x, y, down } => {
            // Tıklamadan önce konumu tazele: aradaki `Move` düşmüş olabilir.
            let (px, py) = to_pixels(x, y, w, h);
            enigo.move_mouse(px, py, Coordinate::Abs)?;
            enigo.button(to_button(b), direction(down))?;
        }
        InputEvent::Scroll { dx, dy } => {
            if dy != 0 {
                enigo.scroll(dy, Axis::Vertical)?;
            }
            if dx != 0 {
                enigo.scroll(dx, Axis::Horizontal)?;
            }
        }
        InputEvent::Key { k, down } => enigo.key(to_key(k), direction(down))?,
        InputEvent::Char { c, down } => enigo.key(EKey::Unicode(c), direction(down))?,
        InputEvent::Text { s } => enigo.text(&s)?,
    }
    Ok(())
}

/// 0..1 -> piksel. Aralık dışı değerler ekrana kırpılır (kenardaki bir tıklama
/// ekranın dışına düşmemeli).
fn to_pixels(x: f32, y: f32, w: i32, h: i32) -> (i32, i32) {
    let px = (x.clamp(0.0, 1.0) * (w - 1).max(0) as f32).round() as i32;
    let py = (y.clamp(0.0, 1.0) * (h - 1).max(0) as f32).round() as i32;
    (px, py)
}

fn direction(down: bool) -> Direction {
    if down {
        Direction::Press
    } else {
        Direction::Release
    }
}

fn to_button(b: MouseButton) -> Button {
    match b {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
        MouseButton::Back => Button::Back,
        MouseButton::Forward => Button::Forward,
    }
}

fn to_key(k: KeyCode) -> EKey {
    use EKey as E;
    match k {
        KeyCode::Ctrl => E::Control,
        KeyCode::Shift => E::Shift,
        KeyCode::Alt => E::Alt,
        KeyCode::Enter => E::Return,
        KeyCode::Tab => E::Tab,
        KeyCode::Backspace => E::Backspace,
        KeyCode::Delete => E::Delete,
        KeyCode::Escape => E::Escape,
        KeyCode::Space => E::Space,
        KeyCode::Insert => E::Insert,
        KeyCode::Up => E::UpArrow,
        KeyCode::Down => E::DownArrow,
        KeyCode::Left => E::LeftArrow,
        KeyCode::Right => E::RightArrow,
        KeyCode::Home => E::Home,
        KeyCode::End => E::End,
        KeyCode::PageUp => E::PageUp,
        KeyCode::PageDown => E::PageDown,
        KeyCode::F1 => E::F1,
        KeyCode::F2 => E::F2,
        KeyCode::F3 => E::F3,
        KeyCode::F4 => E::F4,
        KeyCode::F5 => E::F5,
        KeyCode::F6 => E::F6,
        KeyCode::F7 => E::F7,
        KeyCode::F8 => E::F8,
        KeyCode::F9 => E::F9,
        KeyCode::F10 => E::F10,
        KeyCode::F11 => E::F11,
        KeyCode::F12 => E::F12,
    }
}
