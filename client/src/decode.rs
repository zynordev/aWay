//! H264 decode (openh264) — `media` feature.
//!
//! Gelen Annex-B erişim birimini (access unit) çözer ve doğrudan çizime hazır
//! `ScreenFrame` üretir. YUV->RGB dönüşümü `convert` içindeki tamsayı sürümle yapılır;
//! openh264'ün `write_rgba8`'i piksel başına kayan noktalı çalıştığı için izleyicideki
//! CPU yükünün büyük kısmını tek başına o yiyordu.

use crate::convert;
use crate::frame::ScreenFrame;
use anyhow::{anyhow, Result};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

pub struct H264Decoder {
    inner: Decoder,
}

impl H264Decoder {
    pub fn new() -> Result<Self> {
        let inner = Decoder::new().map_err(|e| anyhow!("decoder init: {e}"))?;
        Ok(Self { inner })
    }

    /// Bir Annex-B erişim birimini çöz. Kare henüz hazır değilse `None`.
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<ScreenFrame>> {
        match self.inner.decode(au).map_err(|e| anyhow!("decode: {e}"))? {
            Some(yuv) => {
                let (w, h) = yuv.dimensions();
                // 4:2:0 zaten çift boyut üretir; yine de tek gelirse son satır/sütunu
                // düşürmek, dönüşümün kroma eşlemesini bozmaktan iyidir.
                let (w, h) = (w & !1, h & !1);
                if w == 0 || h == 0 {
                    return Ok(None);
                }
                let mut pixels = Vec::new();
                convert::i420_to_pixels(
                    yuv.y(),
                    yuv.u(),
                    yuv.v(),
                    yuv.strides(),
                    w,
                    h,
                    &mut pixels,
                );
                Ok(Some(ScreenFrame { width: w, height: h, pixels }))
            }
            None => Ok(None),
        }
    }
}
