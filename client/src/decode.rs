//! H264 decode (openh264) — `media` feature.
//!
//! Gelen Annex-B erişim birimini (access unit) çözer, RGBA kareye dönüştürür.
//!
//! NOT (sürüm hassas): `Decoder::new` / `decode` / `DecodedYUV::dimensions` /
//! `write_rgba8` openh264 0.6 hattına göre. İlk derlemede imza uyuşmazlığı olursa
//! burası düzeltilir (bkz. encode.rs notu).

use crate::frame::RgbaFrame;
use anyhow::{anyhow, Result};
use openh264::decoder::Decoder;

pub struct H264Decoder {
    inner: Decoder,
}

impl H264Decoder {
    pub fn new() -> Result<Self> {
        let inner = Decoder::new().map_err(|e| anyhow!("decoder init: {e}"))?;
        Ok(Self { inner })
    }

    /// Bir Annex-B erişim birimini çöz. Kare henüz hazır değilse `None`.
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<RgbaFrame>> {
        match self.inner.decode(au).map_err(|e| anyhow!("decode: {e}"))? {
            Some(yuv) => {
                let (width, height) = yuv.dimensions();
                let mut data = vec![0u8; width * height * 4];
                yuv.write_rgba8(&mut data);
                Ok(Some(RgbaFrame { width, height, data }))
            }
            None => Ok(None),
        }
    }
}
