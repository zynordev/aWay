//! H264 encode (openh264 0.6) — `media` feature.
//!
//! BGRA ekran karesi -> I420 -> H264 (Annex-B). webrtc-rs `TrackLocalStaticSample` bu
//! Annex-B baytlarını RTP'ye paketler. Genişlik/yükseklik encode anında YUV kaynağından
//! alınır (openh264 `Encoder::new` sabit boyut istemez, çözünürlük değişimini destekler).
//!
//! `BgraSliceU8` sayesinde BGRA doğrudan RGBSource olarak verilir; elle renk çevirmeye
//! gerek yok. `from_rgb_source` boyutların 2'nin katı olmasını bekler (ekran çözünürlükleri
//! genelde çift).

use crate::frame::BgraFrame;
use anyhow::{anyhow, Result};
use openh264::encoder::Encoder;
use openh264::formats::{BgraSliceU8, YUVBuffer};

pub struct H264Encoder {
    inner: Encoder,
}

impl H264Encoder {
    pub fn new() -> Result<Self> {
        let inner = Encoder::new().map_err(|e| anyhow!("encoder init: {e}"))?;
        Ok(Self { inner })
    }

    /// Bir BGRA kareyi H264 Annex-B'ye kodlar. Encoder kareyi atlarsa boş dönebilir.
    pub fn encode(&mut self, frame: &BgraFrame) -> Result<Vec<u8>> {
        let src = BgraSliceU8::new(&frame.data, (frame.width, frame.height));
        let yuv = YUVBuffer::from_rgb_source(src);
        let bitstream = self.inner.encode(&yuv).map_err(|e| anyhow!("encode: {e}"))?;
        Ok(bitstream.to_vec())
    }
}
