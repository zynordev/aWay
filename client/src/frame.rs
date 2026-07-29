//! Ham (sıkıştırılmamış) kare tipleri ve UI'a köprü tampon — `media` feature.
//!
//! `BgraFrame`: ekran yakalamanın çıktısı (BGRA8888, satır adımı = width*4, dolgusuz).
//! `RgbaFrame`: decoder çıktısı / izleyici penceresine giren (RGBA8888).
//! `FrameBuffer`: decode görevinden UI'a en güncel kareyi taşıyan paylaşılan tampon.

use std::sync::{Arc, Mutex};

/// Yakalanan ham ekran karesi (BGRA, sıkı paketli).
pub struct BgraFrame {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// Çözülmüş, çizime hazır kare (RGBA, sıkı paketli).
pub struct RgbaFrame {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// En güncel çözülmüş kareyi tutan paylaşılan tampon (yalnızca son kare saklanır).
/// Decode görevi `set` eder, UI her yeniden çizimde `take` eder.
#[derive(Clone, Default)]
pub struct FrameBuffer(Arc<Mutex<Option<RgbaFrame>>>);

impl FrameBuffer {
    pub fn set(&self, frame: RgbaFrame) {
        *self.0.lock().unwrap() = Some(frame);
    }
    pub fn take(&self) -> Option<RgbaFrame> {
        self.0.lock().unwrap().take()
    }
}
