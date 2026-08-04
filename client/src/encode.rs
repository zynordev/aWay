//! H264 encode (openh264 0.6) — `media` feature.
//!
//! Girdi doğrudan I420'dir (bkz. `convert.rs`); openh264'ün kendi RGB->YUV
//! dönüştürücüsü kullanılmaz, çünkü piksel başına kayan noktalı çalışıyor ve
//! encode'un kendisiyle yarışacak kadar pahalı.
//!
//! Çıktı Annex-B baytlarıdır; webrtc-rs `TrackLocalStaticSample` bunları RTP'ye paketler.

use crate::convert::I420;
use anyhow::{anyhow, bail, Result};
use openh264::encoder::{Encoder, EncoderConfig, RateControlMode, UsageType};
use openh264::OpenH264API;
use openh264_sys2::{ENCODER_OPTION_SVC_ENCODE_PARAM_EXT, SEncParamExt, SM_FIXEDSLCNUM_SLICE};
use std::ptr::addr_of_mut;

/// Hedef bit hızı: çözünürlük ve kare hızıyla ölçeklenir.
///
/// Sabit bir tavan iki yönden de yanlıştı. Düşük tutunca büyük ekranda görüntü
/// bulanıklaşıyor; yüksek tutunca anahtar kareler devasa patlamalar hâlinde gidiyor
/// ve yükleme hattı dolduğu için doğrudan gecikmeye dönüşüyor. ~0,15 bit/piksel/kare
/// ekran içeriği (çoğu bölgesi sabit) için tatlı nokta.
///
/// Tavan 5 Mbit/s: tam çözünürlük 1080p@15 bunun hemen altına düşer. Ev bağlantısının
/// yükleme hızı bunu kaldırmıyorsa `--bitrate` ile düşürülür — hattı aşan bit hızı
/// kaliteyi artırmaz, yalnızca kuyruk oluşturup gecikmeye dönüşür.
fn target_bitrate(w: usize, h: usize, fps: u32) -> u32 {
    let bps = (w * h) as f64 * f64::from(fps) * 0.15;
    (bps as u32).clamp(1_000_000, 5_000_000)
}

pub struct H264Encoder {
    inner: Encoder,
    /// Çok çekirdek denemesi yalnızca bir kez yapılır; encoder ilk kareyle kurulduğu
    /// için ayar ancak ilk encode'dan SONRA yazılabiliyor.
    multicore_tried: bool,
}

impl H264Encoder {
    /// `bitrate_kbps` verilirse otomatik hesap yerine o kullanılır.
    pub fn new(width: usize, height: usize, fps: u32, bitrate_kbps: Option<u32>) -> Result<Self> {
        let bitrate = bitrate_kbps.map_or_else(
            || target_bitrate(width, height, fps),
            |k| k.saturating_mul(1000).max(200_000),
        );
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
        Ok(Self { inner, multicore_tried: false })
    }

    /// Sonraki kareyi IDR (anahtar kare) olarak ürettir.
    pub fn force_keyframe(&mut self) {
        self.inner.force_intra_frame();
    }

    /// Bir I420 kareyi H264 Annex-B'ye kodlar. Encoder kareyi atlarsa boş dönebilir.
    pub fn encode(&mut self, frame: &I420) -> Result<Vec<u8>> {
        let out = {
            let bitstream = self.inner.encode(frame).map_err(|e| anyhow!("encode: {e}"))?;
            bitstream.to_vec()
        };

        // İlk kare encoder'ı kurar; çok çekirdek ayarı ancak bundan sonra yazılabilir.
        if !self.multicore_tried {
            self.multicore_tried = true;
            match enable_multicore(&mut self.inner) {
                Ok(threads) if threads > 1 => {
                    tracing::info!("encode {threads} çekirdekte (çok dilimli)");
                    // Yeniden kurulan encoder'ın ilk karesi zaten IDR olur; yine de
                    // garantiye alıyoruz, aksi hâlde izleyici bir sonraki IDR'a kadar
                    // çözemeyeceği kareler alır.
                    self.inner.force_intra_frame();
                }
                Ok(_) => tracing::info!("encode tek çekirdekte (çok dilim uygulanamadı)"),
                Err(e) => tracing::warn!("çok çekirdekli encode açılamadı, tek çekirdek: {e}"),
            }
        }

        Ok(out)
    }
}

/// openh264'ü çok çekirdekli çalışmaya ikna et.
///
/// Varsayılan yapılandırma kareyi TEK DİLİM (`SM_SINGLE_SLICE`) olarak kodlar;
/// `InitSliceSettings` bu durumda `iMultipleThreadIdc = min(çekirdek, dilim)` = 1 yapar,
/// yani 8 çekirdekli makinede bile encode tek core'da koşar — ekran paylaşımında en
/// büyük tek darboğaz buydu. Rust katmanı dilim modunu dışarı açmıyor, ama encoder'ın
/// kendi parametrelerini `raw_api` ile okuyup geri yazabiliyoruz.
///
/// Bu bir kaçamak değil, kütüphanenin desteklediği yol: `WelsEncoderParamAdjust` dilim
/// modu değiştiğinde encoder'ı temiz biçimde uninit + init ediyor (aynı yolu openh264
/// crate'i de çözünürlük değişiminde kullanıyor). `uiSliceNum = 0` "dilim sayısını
/// çekirdek sayısından türet" demek; tek çekirdekli makinede ya da çok küçük görüntüde
/// openh264 kendiliğinden tek dilime geri dönüyor.
///
/// Dönen değer encoder'ın gerçekten kullandığı thread sayısıdır. Hata ölümcül değildir:
/// çağıran uyarı basıp tek çekirdekle devam eder.
fn enable_multicore(enc: &mut Encoder) -> Result<u16> {
    let mut params = SEncParamExt::default();

    // SAFETY: encoder ilk encode ile kurulmuş durumda. get_option, encoder'ın kendi
    // parametre yapısını `SEncParamExt` boyutunda kopyalıyor; set_option da aynı yapıyı
    // geri okuyor. Yani yazdığımız her alan encoder'ın kendi verdiği değerden türetiliyor,
    // sıfırdan uydurulmuş bir yapılandırma göndermiyoruz.
    unsafe {
        let raw = enc.raw_api();
        if raw.get_option(ENCODER_OPTION_SVC_ENCODE_PARAM_EXT, addr_of_mut!(params).cast()) != 0 {
            bail!("mevcut encoder parametreleri okunamadı");
        }

        let layers = (params.iSpatialLayerNum.max(0) as usize).min(params.sSpatialLayers.len());
        for layer in &mut params.sSpatialLayers[..layers] {
            layer.sSliceArgument.uiSliceMode = SM_FIXEDSLCNUM_SLICE;
            layer.sSliceArgument.uiSliceNum = 0; // 0 = çekirdek sayısına göre otomatik
        }
        params.iMultipleThreadIdc = 0; // 0 = otomatik

        if raw.set_option(ENCODER_OPTION_SVC_ENCODE_PARAM_EXT, addr_of_mut!(params).cast()) != 0 {
            bail!("çok dilimli yapılandırma reddedildi");
        }
        // Ne uyguladığını tahmin etmiyoruz, geri okuyoruz: openh264 isteği sessizce
        // tek dilime düşürebilir (tek çekirdek, çok küçük görüntü).
        if raw.get_option(ENCODER_OPTION_SVC_ENCODE_PARAM_EXT, addr_of_mut!(params).cast()) != 0 {
            bail!("yeni parametreler doğrulanamadı");
        }
    }

    Ok(params.iMultipleThreadIdc)
}
