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
use openh264::encoder::{Encoder, EncoderConfig, RateControlMode, UsageType};
use openh264::formats::{BgraSliceU8, YUVBuffer};
use openh264::OpenH264API;

/// Hedef bit hızı.
///
/// openh264'ün varsayılanı 120 kbps'tir — tam ekran bir masaüstü için çok düşük. Ama fazlası
/// da zarar: yüksek tavan, anahtar karelerin devasa patlamalar hâlinde gitmesine yol açıyor,
/// bu da tam olarak gecikme demek. 2,5 Mbit/s ekran içeriği için tatlı nokta.
const TARGET_BITRATE_BPS: u32 = 2_500_000;

pub struct H264Encoder {
    inner: Encoder,
    /// Kare başına yeniden ayırmamak için saklanan YUV tamponu. 1080p'de her kare
    /// ~3 MB'lık bir ayırma demekti; 15-30 fps'te bu tek başına ciddi CPU yükü.
    yuv: Option<YUVBuffer>,
    dims: Option<(usize, usize)>,
}

impl H264Encoder {
    pub fn new(fps: u32) -> Result<Self> {
        // Varsayılan config kamera içeriğine göre ayarlıdır; ekran paylaşımında
        // metin/kenar ağırlıklı içerik için ScreenContentRealTime belirgin şekilde daha iyi.
        // Bitrate modu şart: varsayılan Quality modunda hedef bit hızı yalnızca bir temenni,
        // encoder istediği kadar taşabiliyor ve ani patlamalar gecikmeye dönüşüyor.
        let config = EncoderConfig::new()
            .set_bitrate_bps(TARGET_BITRATE_BPS)
            .max_frame_rate(fps as f32)
            .rate_control_mode(RateControlMode::Bitrate)
            .usage_type(UsageType::ScreenContentRealTime);
        let inner = Encoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(|e| anyhow!("encoder init: {e}"))?;
        Ok(Self { inner, yuv: None, dims: None })
    }

    /// Sonraki kareyi IDR (anahtar kare) olarak ürettir.
    pub fn force_keyframe(&mut self) {
        self.inner.force_intra_frame();
    }

    /// Bir BGRA kareyi H264 Annex-B'ye kodlar. Encoder kareyi atlarsa boş dönebilir.
    pub fn encode(&mut self, frame: &BgraFrame) -> Result<Vec<u8>> {
        let dims = (frame.width, frame.height);
        if self.dims != Some(dims) {
            self.yuv = Some(YUVBuffer::new(frame.width, frame.height));
            self.dims = Some(dims);
        }
        let yuv = self.yuv.as_mut().expect("yuv tamponu yukarıda kuruldu");
        yuv.read_rgb(BgraSliceU8::new(&frame.data, dims));

        let bitstream = self.inner.encode(&*yuv).map_err(|e| anyhow!("encode: {e}"))?;
        Ok(bitstream.to_vec())
    }
}
