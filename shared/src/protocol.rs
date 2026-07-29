//! aWay signaling protokolü (JSON over WebSocket).
//!
//! AnyDesk mantığı: iki taraf da aynı uygulamayı çalıştırır. Herkes sunucuya
//! **kullanıcı adı + şifre** ile giriş yapar (ID YOK). Biri ötekine bağlanmak
//! isteyince sunucu isteği hedefe iletir; hedef makinede kabul/ret sorulur.
//! Kabul edilirse taraflar WebRTC için SDP/ICE bilgilerini sunucu üzerinden
//! değiş tokuş eder ve doğrudan (P2P) bağlanır; olmazsa VDS'teki TURN relay'e düşer.
//!
//! Sunucu, `Signal.payload` içeriğini OPAK kabul eder — yani WebRTC ayrıntılarını
//! yorumlamadan sadece hedef kullanıcıya iletir. Böylece istemci/APK protokolü
//! kırmadan evrilebilir.

use serde::{Deserialize, Serialize};

/// Kullanıcı adı — sistemdeki kimlik. (AnyDesk'teki sayısal ID'nin yerine.)
pub type Username = String;

/// Bir bağlantı oturumunu tanımlayan kimlik. Sunucu, `Connect` isteği kabul
/// edilince üretir ve iki tarafa da bildirir; sonraki tüm `Signal` mesajları bu
/// oturuma etiketlenir (aynı anda birden çok oturumu ayırt etmek için).
pub type SessionId = String;

// ─────────────────────────────────────────────────────────────────────────────
// İstemci → Sunucu
// ─────────────────────────────────────────────────────────────────────────────

/// İstemcinin sunucuya gönderdiği mesajlar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Yeni hesap oluştur. (v1: sadece 2 hesap açılacak; sonra kapatılabilir.)
    Register {
        protocol_version: u32,
        username: Username,
        password: String,
    },
    /// Var olan hesapla giriş yap. Başarılıysa sunucu `LoggedIn` döner ve bu
    /// bağlantı artık `username` için "online" sayılır.
    Login {
        protocol_version: u32,
        username: Username,
        password: String,
    },
    /// Bir kullanıcıya bağlanma isteği başlat (viewer/controller tarafı).
    Connect { to: Username },
    /// Gelen bir bağlantı isteğine yanıt (host tarafı: kabul/ret penceresi sonucu).
    ConnectResponse {
        /// Hangi oturuma yanıt (sunucunun `IncomingConnect` ile verdiği kimlik).
        session: SessionId,
        /// İsteği başlatan taraf.
        to: Username,
        accept: bool,
        /// Reddedilirse opsiyonel neden ("busy", "denied", ...).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// WebRTC SDP/ICE bilgisini karşı tarafa ilet (sunucu içeriği yorumlamaz).
    Signal {
        session: SessionId,
        to: Username,
        payload: SignalPayload,
    },
    /// Bir oturumu sonlandır (kullanıcı bağlantıyı kapattı).
    Hangup { session: SessionId },
    /// Bağlantıyı canlı tutmak için (WS ping'e ek uygulama seviyesi keepalive).
    Ping,
}

// ─────────────────────────────────────────────────────────────────────────────
// Sunucu → İstemci
// ─────────────────────────────────────────────────────────────────────────────

/// Sunucunun istemciye gönderdiği mesajlar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Kayıt başarılı.
    Registered { username: Username },
    /// Giriş başarılı. `session_token` sonraki oturum için saklanabilir.
    LoggedIn {
        username: Username,
        session_token: String,
    },
    /// Sana bağlanmak isteyen biri var → kabul/ret penceresini göster.
    /// Kabul edilirse WebRTC için bu `ice_servers` kullanılacak.
    IncomingConnect {
        session: SessionId,
        from: Username,
        ice_servers: Vec<IceServer>,
    },
    /// Başlattığın bağlantı isteği kabul edildi; artık SDP/ICE değiş tokuşu başlar.
    /// Konvansiyon: isteği BAŞLATAN taraf WebRTC "offer"ı üretir (impolite peer).
    ConnectAccepted {
        session: SessionId,
        peer: Username,
        ice_servers: Vec<IceServer>,
    },
    /// Başlattığın bağlantı isteği reddedildi ya da hedefe ulaşılamadı.
    ConnectRejected {
        peer: Username,
        reason: String,
    },
    /// Karşı taraftan iletilen WebRTC SDP/ICE bilgisi.
    Signal {
        session: SessionId,
        from: Username,
        payload: SignalPayload,
    },
    /// Karşı taraf oturumu kapattı.
    Hangup { session: SessionId },
    /// Bir kullanıcının online/offline durumu değişti (opsiyonel presence).
    Presence { user: Username, online: bool },
    /// Hata bildirimi.
    Error { code: ErrorCode, message: String },
    /// Ping yanıtı.
    Pong,
}

/// WebRTC bağlantısını kurmak için taşınan alt-yük. Sunucu için opaktır; sadece
/// istemciler yorumlar. TURN kimlik bilgileri sunucu tarafından `ConnectAccepted`
/// sonrası ayrı bir kanalla değil, burada `IceServers` ile de dağıtılabilir.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalPayload {
    /// WebRTC oturum tanımı (offer).
    Offer { sdp: String },
    /// WebRTC oturum tanımı (answer).
    Answer { sdp: String },
    /// Tekil ICE adayı (trickle ICE).
    IceCandidate {
        candidate: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sdp_mid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sdp_mline_index: Option<u16>,
    },
}

/// STUN/TURN sunucu bilgisi — istemci `RTCConfiguration.iceServers`'a koyar.
/// TURN kimlik bilgileri kısa ömürlüdür (coturn `use-auth-secret` / REST API):
/// `username = "<expiry_unix>:<user>"`, `credential = base64(hmac_sha1(secret, username))`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// Hata kodları — istemci UI'ında kullanıcıya anlamlı mesaj göstermek için.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Protokol sürümü uyumsuz.
    ProtocolMismatch,
    /// Kullanıcı adı zaten var (kayıt).
    UsernameTaken,
    /// Kullanıcı adı/şifre hatalı (giriş).
    InvalidCredentials,
    /// Giriş yapılmadan işlem denendi.
    NotAuthenticated,
    /// Hedef kullanıcı bulunamadı / online değil.
    PeerNotFound,
    /// Hedef kullanıcı meşgul (başka oturumda) — v1'de opsiyonel.
    PeerBusy,
    /// Bilinmeyen/oturumsuz `session` referansı.
    UnknownSession,
    /// Geçersiz istek (bozuk mesaj vb.).
    BadRequest,
    /// Sunucu içi hata.
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_roundtrips_as_tagged_json() {
        let msg = ClientMessage::Login {
            protocol_version: 1,
            username: "murat".into(),
            password: "secret".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"login\""));
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        matches!(back, ClientMessage::Login { .. });
    }

    #[test]
    fn signal_payload_is_kind_tagged() {
        let p = SignalPayload::IceCandidate {
            candidate: "candidate:...".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"kind\":\"ice_candidate\""));
    }
}
