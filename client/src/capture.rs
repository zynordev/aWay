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
    /// Ölçek böleni. Verilirse çözünürlük SABİTLENİR (otomatik uyarlama kapanır);
    /// verilmezse makinenin yetiştiği en büyük boyut ölçülerek seçilir.
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

    let mut ladder = match scale {
        // Elle sabitlenmişse uyarlama yok: kullanıcı ne dediyse o.
        Some(s) => {
            let n = (s as usize).max(1);
            Ladder::pinned(((sw / n) & !1, (sh / n) & !1))
        }
        None => Ladder::new(sw, sh, convert::auto_size(sw, sh)),
    };
    let (mut ow, mut oh) = ladder.current();
    if ow == 0 || oh == 0 {
        return Err(anyhow!("geçersiz çıktı boyutu: {sw}x{sh} → {ow}x{oh}"));
    }
    tracing::info!(
        "ekran {sw}x{sh} → {ow}x{oh} @ {fps}fps ({})",
        if ladder.adaptive() { "otomatik çözünürlük" } else { "sabit" }
    );

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
                convert::bgra_to_i420(&buf, stride, sw, sh, (ow, oh), &mut cur);
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
            stats.maybe_log(ow, oh);
            continue;
        }

        if key_due {
            encoder.force_keyframe();
            last_keyframe = Instant::now();
        }

        let t1 = Instant::now();
        let encoded = encoder.encode(&cur)?;
        let t_enc = t1.elapsed();
        stats.encode += t_enc;
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

        // Makine yetişiyor mu? Ölçüyü kare BAŞINA gerçek maliyetten alıyoruz; uykuyu
        // saymıyoruz, yoksa hedef fps'e uyan her şey "tam kapasite" görünürdü.
        if let Some((nw, nh)) = ladder.observe(t_conv + t_enc, interval) {
            tracing::info!("çözünürlük {ow}x{oh} → {nw}x{nh}");
            (ow, oh) = (nw, nh);
            encoder = H264Encoder::new(ow, oh, fps, bitrate_kbps)?;
            // Boyut değişti: eski kare artık karşılaştırılamaz.
            have_prev = false;
            last_keyframe = Instant::now();
        }

        sleep_rest(interval, t0);
        stats.maybe_log(ow, oh);
    }
    Ok(())
}

/// Çözünürlük basamakları ve "makine yetişiyor mu" denetleyicisi.
///
/// Tam kalite ile düşük gecikme yazılımsal encode'da doğrudan çelişir: 1080p bir karenin
/// kodlanması zayıf bir CPU'da 100 ms'yi aşabiliyor, bu da 7-8 fps ve her karede 100 ms+
/// gecikme demek. Sabit bir çözünürlük seçmek yerine makinenin GERÇEKTEN yetiştiği en
/// büyük boyutu ölçerek buluyoruz: hızlıysa tam çözünürlükte kalır, yavaşsa iner.
///
/// Basamaklar genişliği ~1,25 kat azaltır (piksel sayısında ~1,55 kat), yani her adım
/// hissedilir ama uçurum değil.
struct Ladder {
    sizes: Vec<(usize, usize)>,
    idx: usize,
    /// Kare maliyetinin üstel hareketli ortalaması (ms). Tek bir yavaş kare (anahtar kare,
    /// başka bir programın anlık yükü) çözünürlük düşürmemeli.
    avg_ms: f64,
    /// Üst üste kaç kare bütçeyi aştı.
    over: u32,
    /// Ne zamandır rahatça bütçenin altındayız (yukarı çıkmak için).
    comfortable_since: Option<Instant>,
    /// Son değişiklikten sonra ölçümün oturması için bekleme.
    changed_at: Instant,
}

impl Ladder {
    /// Bütçenin bu kadarını aşarsak küçül. Tam bütçeyi beklemek geç kalmak olur.
    const OVER: f64 = 0.85;
    /// Bunun altında kalırsak büyümeyi düşün. Bir üst basamak ~1,55 kat pahalı olduğu
    /// için 0,5 eşiği büyüdükten sonra ~0,78'e denk gelir; yani salınmayız.
    const UNDER: f64 = 0.5;
    const OVER_STREAK: u32 = 4;
    const COMFORT_HOLD: Duration = Duration::from_secs(10);
    const COOLDOWN: Duration = Duration::from_secs(3);

    fn pinned(size: (usize, usize)) -> Self {
        Self::from_sizes(vec![size])
    }

    fn new(src_w: usize, src_h: usize, base: (usize, usize)) -> Self {
        const MIN_WIDTH: usize = 640;
        let mut sizes = Vec::new();
        let mut width = base.0 & !1;
        loop {
            let h = convert::fit_height(src_w, src_h, width);
            if width < 2 || h < 2 {
                break;
            }
            sizes.push((width, h));
            if width <= MIN_WIDTH {
                break;
            }
            let next = (width * 4 / 5).max(MIN_WIDTH) & !1;
            if next >= width {
                break;
            }
            width = next;
        }
        if sizes.is_empty() {
            sizes.push(base);
        }
        Self::from_sizes(sizes)
    }

    fn from_sizes(sizes: Vec<(usize, usize)>) -> Self {
        Self {
            sizes,
            idx: 0,
            avg_ms: 0.0,
            over: 0,
            comfortable_since: None,
            changed_at: Instant::now(),
        }
    }

    fn adaptive(&self) -> bool {
        self.sizes.len() > 1
    }

    fn current(&self) -> (usize, usize) {
        self.sizes[self.idx]
    }

    /// Bir karenin maliyetini bildir; basamak değiştiyse yeni boyutu döndürür.
    fn observe(&mut self, cost: Duration, budget: Duration) -> Option<(usize, usize)> {
        if !self.adaptive() {
            return None;
        }
        let ms = cost.as_secs_f64() * 1000.0;
        // İlk ölçümde ortalamayı doğrudan oraya oturt, yoksa sıfırdan tırmanırken
        // gerçekten yavaş bir makinede birkaç saniye boşa gider.
        self.avg_ms = if self.avg_ms == 0.0 { ms } else { self.avg_ms * 0.8 + ms * 0.2 };
        let budget_ms = budget.as_secs_f64() * 1000.0;

        if self.changed_at.elapsed() < Self::COOLDOWN {
            return None;
        }

        if self.avg_ms > budget_ms * Self::OVER {
            self.over += 1;
            self.comfortable_since = None;
            if self.over >= Self::OVER_STREAK && self.idx + 1 < self.sizes.len() {
                self.idx += 1;
                return Some(self.step_taken());
            }
            return None;
        }

        self.over = 0;
        if self.avg_ms < budget_ms * Self::UNDER && self.idx > 0 {
            let since = *self.comfortable_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= Self::COMFORT_HOLD {
                self.idx -= 1;
                return Some(self.step_taken());
            }
        } else {
            self.comfortable_since = None;
        }
        None
    }

    fn step_taken(&mut self) -> (usize, usize) {
        self.over = 0;
        self.comfortable_since = None;
        self.avg_ms = 0.0;
        self.changed_at = Instant::now();
        self.current()
    }
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

    fn maybe_log(&mut self, w: usize, h: usize) {
        let el = self.since.elapsed();
        if el < STATS_INTERVAL {
            return;
        }
        let secs = el.as_secs_f64();
        let looked = (self.frames + self.skipped).max(1);
        tracing::info!(
            "ekran {w}x{h} | gönderilen {:.1} fps | atlanan {} | \
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
