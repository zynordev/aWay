//! Ekran yakalama (scrap: Windows DXGI / Linux X11) — `media` feature.
//!
//! Yakalama + encode CPU-yoğun ve bloklayıcı olduğundan ayrı bir std thread'de çalışır;
//! kodlanmış H264 erişim birimleri tokio kanalıyla async yazıcıya (video::spawn_sample_writer)
//! aktarılır.

use crate::encode::H264Encoder;
use crate::frame::BgraFrame;
use anyhow::{anyhow, Result};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Ayrı thread'de yakalama+encode döngüsü başlatır. Alıcı (`tx`) kapanınca durur.
pub fn spawn_capture_encoder(tx: mpsc::Sender<Vec<u8>>, fps: u32) {
    std::thread::spawn(move || {
        if let Err(e) = capture_encode_loop(&tx, fps) {
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

fn capture_encode_loop(tx: &mpsc::Sender<Vec<u8>>, fps: u32) -> Result<()> {
    use scrap::{Capturer, Display};

    let display = Display::primary().map_err(|e| anyhow!("birincil ekran: {e}"))?;
    let mut capturer = Capturer::new(display).map_err(|e| anyhow!("yakalayıcı: {e}"))?;
    let (w, h) = (capturer.width(), capturer.height());
    tracing::info!("ekran yakalanıyor: {w}x{h} @ {fps}fps");

    let mut encoder = H264Encoder::new(fps)?;
    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let started = Instant::now();
    // İlk kare zaten IDR olarak üretilir; sayaç oradan işlemeye başlar.
    let mut last_keyframe = Instant::now();
    // Kare tamponu yeniden kullanılır: 1080p'de her tur 8 MB'lık ayırma demekti.
    let mut bgra = BgraFrame { width: w, height: h, data: Vec::with_capacity(w * h * 4) };

    loop {
        let t0 = Instant::now();
        match capturer.frame() {
            Ok(buf) => {
                // scrap karesi BGRA; satır adımı dolgulu olabilir (stride >= w*4).
                let stride = buf.len() / h;
                let row = w * 4;
                bgra.data.clear();
                for y in 0..h {
                    let s = y * stride;
                    bgra.data.extend_from_slice(&buf[s..s + row]);
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Bu tur yeni kare yok; kısa bekle.
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(e) => return Err(anyhow!("kare alınamadı: {e}")),
        }

        let keyframe_every = if started.elapsed() < KEYFRAME_WARMUP {
            KEYFRAME_WARMUP_INTERVAL
        } else {
            KEYFRAME_INTERVAL
        };
        if last_keyframe.elapsed() >= keyframe_every {
            encoder.force_keyframe();
            last_keyframe = Instant::now();
        }

        let encoded = encoder.encode(&bgra)?;
        if !encoded.is_empty() && tx.blocking_send(encoded).is_err() {
            break; // async taraf kapandı → oturum bitti
        }

        if let Some(rem) = interval.checked_sub(t0.elapsed()) {
            std::thread::sleep(rem);
        }
    }
    Ok(())
}
