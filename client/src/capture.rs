//! Ekran yakalama (scrap: Windows DXGI / Linux X11) — `media` feature.
//!
//! Yakalama + dönüşüm + encode CPU-yoğun ve bloklayıcı olduğundan ayrı bir std
//! thread'de çalışır; kodlanmış H264 erişim birimleri tokio kanalıyla async yazıcıya
//! (`video::spawn_sample_writer`) aktarılır.

use crate::convert::{self, I420};
use crate::encode::H264Encoder;
use anyhow::{anyhow, Result};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Görüntü akışı ayarları (komut satırından gelir, oturum boyunca sabittir).
#[derive(Clone, Copy)]
pub struct VideoOpts {
    pub fps: u32,
    /// Ölçek böleni; `None` ise çözünürlüğe göre otomatik (bkz. `convert::auto_scale`).
    pub scale: Option<u32>,
    /// Hedef bit hızı; `None` ise çözünürlük+fps'ten hesaplanır.
    pub bitrate_kbps: Option<u32>,
}

/// Ayrı thread'de yakalama+encode döngüsü başlatır. Alıcı (`tx`) kapanınca durur.
pub fn spawn_capture_encoder(tx: mpsc::Sender<(Vec<u8>, Duration)>, opts: VideoOpts) {
    std::thread::spawn(move || {
        if let Err(e) = capture_encode_loop(&tx, opts) {
            tracing::error!("yakalama/encode durdu: {e}");
        }
    });
}

/// Anahtar kare (IDR) aralıkları.
///
/// openh264 varsayılanında PERİYODİK IDR YOKTUR — yalnızca ilk kare ve sahne değişimleri.
/// İzleyicinin decoder'ı ilk IDR'ı kaçırırsa görüntü, ekranda büyük bir değişiklik olana
/// kadar HİÇ açılmaz (ilk bağlantıdaki ~20 sn'lik bekleyişin sebebi buydu).
///
/// Ama IDR pahalıdır: tam ekranın komple yeniden kodlanması, hem CPU hem de tek seferde
/// giden büyük bir veri patlaması. Sabit 2 sn'de bir zorlamak gecikmeyi hissedilir şekilde
/// bozuyordu. Bunun yerine: oturumun ilk saniyelerinde sık (hızlı açılsın), sonra seyrek
/// (yalnızca paket kaybından kurtulma sigortası).
const KEYFRAME_WARMUP: Duration = Duration::from_secs(3);
const KEYFRAME_WARMUP_INTERVAL: Duration = Duration::from_secs(1);
const KEYFRAME_INTERVAL: Duration = Duration::from_secs(10);

/// Ölçüm raporu aralığı. Gecikme/CPU şikâyetlerini tahminle değil bu satırla kovalıyoruz;
/// host'un konsolunda görünür.
const STATS_INTERVAL: Duration = Duration::from_secs(5);

fn capture_encode_loop(tx: &mpsc::Sender<(Vec<u8>, Duration)>, opts: VideoOpts) -> Result<()> {
    use scrap::{Capturer, Display};

    let VideoOpts { fps, scale, bitrate_kbps } = opts;
    let display = Display::primary().map_err(|e| anyhow!("birincil ekran: {e}"))?;
    let mut capturer = Capturer::new(display).map_err(|e| anyhow!("yakalayıcı: {e}"))?;
    let (sw, sh) = (capturer.width(), capturer.height());

    let n = scale.map_or_else(|| convert::auto_scale(sw), |s| (s as usize).max(1));
    let (ow, oh) = ((sw / n) & !1, (sh / n) & !1);
    if ow == 0 || oh == 0 {
        return Err(anyhow!("ölçek çok büyük: {sw}x{sh} / {n}"));
    }
    tracing::info!("ekran {sw}x{sh} → {ow}x{oh} (ölçek 1/{n}) @ {fps}fps");

    let mut encoder = H264Encoder::new(ow, oh, fps, bitrate_kbps)?;
    let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
    let started = Instant::now();
    // İlk kare zaten IDR olarak üretilir; sayaç oradan işlemeye başlar.
    let mut last_keyframe = Instant::now();
    // Örnek süresi gerçek geçen zamandan hesaplanır: kare atlandığında sabit 1/fps
    // yazmak alıcının zaman damgalarını kaydırırdı.
    let mut last_sent = Instant::now();

    // Çift tampon: `cur` yeni kare, `prev` en son GÖNDERİLEN kare. İkisi aynıysa
    // encode hiç çalışmaz — sabit bir masaüstünde (okuma, düşünme, yazı yazma
    // aralarında) CPU pratikte sıfıra iner. En pahalı işi tamamen atlamanın tek yolu.
    let mut cur = I420::new();
    let mut prev = I420::new();
    let mut have_prev = false;

    let mut stats = Stats::new();

    loop {
        let t0 = Instant::now();
        match capturer.frame() {
            Ok(buf) => {
                // scrap karesi BGRA; satır adımı dolgulu olabilir (stride >= sw*4).
                // Dönüşüm doğrudan bu tampondan okur — araya sıkı bir kopya girmez.
                let stride = buf.len() / sh;
                convert::bgra_to_i420(&buf, stride, sw, sh, n, &mut cur);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Bu tur yeni kare yok; kısa bekle.
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(e) => return Err(anyhow!("kare alınamadı: {e}")),
        }
        let t_conv = t0.elapsed();

        let keyframe_every = if started.elapsed() < KEYFRAME_WARMUP {
            KEYFRAME_WARMUP_INTERVAL
        } else {
            KEYFRAME_INTERVAL
        };
        // Anahtar kare zamanı geldiyse ekran sabit olsa da kodlanır: paket kaybından
        // sonra bozuk kalan görüntünün tek kurtuluşu bu.
        let key_due = last_keyframe.elapsed() >= keyframe_every;

        if !key_due && have_prev && cur.same_as(&prev) {
            stats.skipped += 1;
            stats.convert += t_conv;
            sleep_rest(interval, t0);
            stats.maybe_log(ow, oh, n);
            continue;
        }

        if key_due {
            encoder.force_keyframe();
            last_keyframe = Instant::now();
        }

        let t1 = Instant::now();
        let encoded = encoder.encode(&cur)?;
        stats.encode += t1.elapsed();
        stats.convert += t_conv;
        stats.frames += 1;

        if !encoded.is_empty() {
            stats.bytes += encoded.len() as u64;
            let dur = last_sent.elapsed();
            last_sent = Instant::now();
            if tx.blocking_send((encoded, dur)).is_err() {
                break; // async taraf kapandı → oturum bitti
            }
        }

        std::mem::swap(&mut cur, &mut prev);
        have_prev = true;

        sleep_rest(interval, t0);
        stats.maybe_log(ow, oh, n);
    }
    Ok(())
}

fn sleep_rest(interval: Duration, t0: Instant) {
    if let Some(rem) = interval.checked_sub(t0.elapsed()) {
        std::thread::sleep(rem);
    }
}

/// Periyodik ölçüm. "Gecikme fazla / CPU fazla" şikâyetlerinde tahmin yürütmemek için.
struct Stats {
    since: Instant,
    frames: u64,
    skipped: u64,
    bytes: u64,
    convert: Duration,
    encode: Duration,
}

impl Stats {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            frames: 0,
            skipped: 0,
            bytes: 0,
            convert: Duration::ZERO,
            encode: Duration::ZERO,
        }
    }

    fn maybe_log(&mut self, w: usize, h: usize, n: usize) {
        let el = self.since.elapsed();
        if el < STATS_INTERVAL {
            return;
        }
        let secs = el.as_secs_f64();
        let looked = (self.frames + self.skipped).max(1);
        tracing::info!(
            "ekran {w}x{h} (1/{n}) | gönderilen {:.1} fps | atlanan {} | \
             dönüşüm {:.1} ms | encode {:.1} ms | {:.0} kbps",
            self.frames as f64 / secs,
            self.skipped,
            self.convert.as_secs_f64() * 1000.0 / looked as f64,
            self.encode.as_secs_f64() * 1000.0 / self.frames.max(1) as f64,
            self.bytes as f64 * 8.0 / secs / 1000.0,
        );
        *self = Self::new();
    }
}
