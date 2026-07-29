//! away-shared — sunucu ve istemcinin ortak konuştuğu tipler.
//!
//! Buradaki `protocol` modülü, WebSocket üzerinden JSON olarak taşınan signaling
//! mesajlarını tanımlar. Android APK (Kotlin) ileride aynı JSON şeklini yeniden
//! üretecek; bu yüzden alan adları ve `type` etiketleri STABİL tutulmalıdır.

pub mod protocol;

/// Protokolün sürümü. İstemci giriş yaparken gönderir; sunucu uyumsuzlukta uyarır.
pub const PROTOCOL_VERSION: u32 = 1;
