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

fn capture_encode_loop(tx: &mpsc::Sender<Vec<u8>>, fps: u32) -> Result<()> {
    use scrap::{Capturer, Display};

    let display = Display::primary().map_err(|e| anyhow!("birincil ekran: {e}"))?;
    let mut capturer = Capturer::new(display).map_err(|e| anyhow!("yakalayıcı: {e}"))?;
    let (w, h) = (capturer.width(), capturer.height());
    tracing::info!("ekran yakalanıyor: {w}x{h} @ {fps}fps");

    let mut encoder = H264Encoder::new()?;
    let interval = Duration::from_secs_f64(1.0 / fps as f64);

    loop {
        let t0 = Instant::now();
        let bgra = match capturer.frame() {
            Ok(buf) => {
                // scrap karesi BGRA; satır adımı dolgulu olabilir (stride >= w*4).
                let stride = buf.len() / h;
                let row = w * 4;
                let mut data = Vec::with_capacity(w * h * 4);
                for y in 0..h {
                    let s = y * stride;
                    data.extend_from_slice(&buf[s..s + row]);
                }
                BgraFrame { width: w, height: h, data }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Bu tur yeni kare yok; kısa bekle.
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(e) => return Err(anyhow!("kare alınamadı: {e}")),
        };

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
