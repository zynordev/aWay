//! H264 encode (openh264 0.6) — `media` feature.
//!
//! Girdi doğrudan I420'dir (bkz. `convert.rs`); openh264'ün kendi RGB->YUV
//! dönüştürücüsü kullanılmaz, çünkü piksel başına kayan noktalı çalışıyor ve
//! encode'un kendisiyle yarışacak kadar pahalı.
//!
//! Çıktı Annex-B baytlarıdır; webrtc-rs `TrackLocalStaticSample` bunları RTP'ye paketler.

use crate::convert::I420;
use anyhow::{anyhow, Result};
use openh264::encoder::{Encoder, EncoderConfig, RateControlMode, UsageType};
use openh264::OpenH264API;

/// Hedef bit hızı: çözünürlük ve kare hızıyla ölçeklenir.
///
/// Sabit bir tavan iki yönden de yanlıştı. Düşük tutunca büyük ekranda görüntü
/// bulanıklaşıyor; yüksek tutunca anahtar kareler devasa patlamalar hâlinde gidiyor
/// ve yükleme hattı dolduğu için doğrudan gecikmeye dönüşüyor. ~0,15 bit/piksel/kare
/// ekran içeriği (çoğu bölgesi sabit) için tatlı nokta.
fn target_bitrate(w: usize, h: usize, fps: u32) -> u32 {
    let bps = (w * h) as f64 * f64::from(fps) * 0.15;
    (bps as u32).clamp(800_000, 3_000_000)
}

pub struct H264Encoder {
    inner: Encoder,
}

impl H264Encoder {
    pub fn new(width: usize, height: usize, fps: u32) -> Result<Self> {
        let bitrate = target_bitrate(width, height, fps);
        tracing::info!("encoder: {width}x{height} @ {fps}fps, hedef {} kbps", bitrate / 1000);
        // Varsayılan config kamera içeriğine göre ayarlıdır; ekran paylaşımında
        // metin/kenar ağırlıklı içerik için ScreenContentRealTime belirgin şekilde daha iyi.
        // Bitrate modu şart: varsayılan Quality modunda hedef bit hızı yalnızca bir temenni,
        // encoder istediği kadar taşabiliyor ve ani patlamalar gecikmeye dönüşüyor.
        let config = EncoderConfig::new()
            .set_bitrate_bps(bitrate)
            .max_frame_rate(fps as f32)
            .rate_control_mode(RateControlMode::Bitrate)
            .usage_type(UsageType::ScreenContentRealTime);
        let inner = Encoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(|e| anyhow!("encoder init: {e}"))?;
        Ok(Self { inner })
    }

    /// Sonraki kareyi IDR (anahtar kare) olarak ürettir.
    pub fn force_keyframe(&mut self) {
        self.inner.force_intra_frame();
    }

    /// Bir I420 kareyi H264 Annex-B'ye kodlar. Encoder kareyi atlarsa boş dönebilir.
    pub fn encode(&mut self, frame: &I420) -> Result<Vec<u8>> {
        let bitstream = self.inner.encode(frame).map_err(|e| anyhow!("encode: {e}"))?;
        Ok(bitstream.to_vec())
    }
}
