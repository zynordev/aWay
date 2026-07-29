//! WebRTC video track kablolaması (H264) — `media` feature.
//!
//! Host tarafı: peer connection'a bir H264 `TrackLocalStaticSample` ekler ve kodlanmış
//! kareleri `write_sample` ile yazar. Viewer tarafı: gelen `TrackRemote`'tan RTP okur,
//! H264 depacketize + decode eder, RGBA kareleri kanala iletir.

use crate::decode::H264Decoder;
use crate::frame::FrameBuffer;
use anyhow::Result;
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::api::media_engine::MIME_TYPE_H264;
use webrtc::media::Sample;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp::codecs::h264::H264Packet;
use webrtc::rtp::packetizer::Depacketizer;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;
use webrtc::track::track_remote::TrackRemote;

/// Host: peer connection'a H264 ekran track'i ekler ve gönderenin RTCP'sini drenaj eder
/// (NACK/RTX gibi interceptor mekanizmalarının çalışması için gerekli).
pub async fn add_screen_track(pc: &Arc<RTCPeerConnection>) -> Result<Arc<TrackLocalStaticSample>> {
    let track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability { mime_type: MIME_TYPE_H264.to_owned(), ..Default::default() },
        "video".to_owned(),
        "away-screen".to_owned(),
    ));

    let sender = pc
        .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
        .await?;

    tokio::spawn(async move {
        let mut buf = vec![0u8; 1500];
        while sender.read(&mut buf).await.is_ok() {}
    });

    Ok(track)
}

/// Kodlanmış H264 karelerini track'e yazan async görev.
pub fn spawn_sample_writer(track: Arc<TrackLocalStaticSample>, mut rx: mpsc::Receiver<Vec<u8>>, fps: u32) {
    let dur = Duration::from_secs_f64(1.0 / fps as f64);
    tokio::spawn(async move {
        while let Some(data) = rx.recv().await {
            let sample = Sample { data: Bytes::from(data), duration: dur, ..Default::default() };
            if let Err(e) = track.write_sample(&sample).await {
                tracing::warn!("write_sample: {e}");
                break;
            }
        }
        tracing::info!("örnek yazıcı durdu");
    });
}

/// Viewer: gelen video track'ini oku, depacketize + decode et, çözülen kareyi UI tamponuna
/// koy ve pencereyi yeniden çizdir. `ctx` egui bağlamıdır (thread-güvenli; kopyalanabilir).
pub fn on_video_track(track: Arc<TrackRemote>, frames: FrameBuffer, ctx: egui::Context) {
    tokio::spawn(async move {
        let mut decoder = match H264Decoder::new() {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("decoder açılamadı: {e}");
                return;
            }
        };
        let mut depack = H264Packet::default();
        let mut au: Vec<u8> = Vec::new();

        loop {
            match track.read_rtp().await {
                Ok((pkt, _)) => {
                    match depack.depacketize(&pkt.payload) {
                        Ok(part) => au.extend_from_slice(&part),
                        Err(e) => tracing::trace!("depacketize: {e}"),
                    }
                    // RTP marker biti erişim biriminin sonunu işaretler.
                    if pkt.header.marker && !au.is_empty() {
                        match decoder.decode(&au) {
                            Ok(Some(frame)) => {
                                frames.set(frame);
                                ctx.request_repaint(); // yeni kare → pencereyi yenile
                            }
                            Ok(None) => {}
                            Err(e) => tracing::trace!("decode: {e}"),
                        }
                        au.clear();
                    }
                }
                Err(e) => {
                    tracing::info!("video track kapandı: {e}");
                    break;
                }
            }
        }
    });
}
