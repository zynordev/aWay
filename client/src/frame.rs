//! Çözülmüş kare tipi ve UI'a köprü tampon — `media` feature.
//!
//! `ScreenFrame`: decoder çıktısı, doğrudan çizime hazır. Piksel tipi bilerek
//! `egui::Color32`: `ColorImage` zaten bunu istiyor, dolayısıyla decoder'ın ürettiği
//! tampon UI'a ek bir dönüşüm/kopya olmadan taşınabiliyor.
//!
//! `FrameBuffer`: decode görevinden UI'a EN GÜNCEL kareyi taşıyan paylaşılan tampon.
//! Yalnızca son kare saklanır — UI geride kalırsa eski kareler birikip gecikmeye
//! dönüşmez, sessizce düşer.

use egui::Color32;
use std::sync::{Arc, Mutex};

/// Çözülmüş, çizime hazır kare.
pub struct ScreenFrame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<Color32>,
}

/// En güncel çözülmüş kareyi tutan paylaşılan tampon.
/// Decode görevi `set` eder, UI her yeniden çizimde `take` eder.
#[derive(Clone, Default)]
pub struct FrameBuffer(Arc<Mutex<Option<ScreenFrame>>>);

impl FrameBuffer {
    pub fn set(&self, frame: ScreenFrame) {
        *self.0.lock().unwrap() = Some(frame);
    }
    pub fn take(&self) -> Option<ScreenFrame> {
        self.0.lock().unwrap().take()
    }
}
