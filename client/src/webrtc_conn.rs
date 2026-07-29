//! WebRTC peer connection kurulumu ve sinyal kablolaması.
//!
//! Sunucudan gelen ICE sunucularıyla bir `RTCPeerConnection` oluşturur, yerel ICE
//! adaylarını (trickle) sunucu üzerinden karşıya iletir. Data channel handler'ları
//! caller/callee tarafında ayrı kurulur (bkz. main.rs).

use anyhow::Result;
use away_shared::protocol::{ClientMessage, IceServer, SignalPayload};
use std::sync::Arc;
use tokio::sync::mpsc;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::RTCPeerConnection;

/// Ortak `IceServer` -> webrtc `RTCIceServer` dönüşümü.
pub fn to_rtc_ice_servers(list: &[IceServer]) -> Vec<RTCIceServer> {
    list.iter()
        .map(|s| RTCIceServer {
            urls: s.urls.clone(),
            username: s.username.clone().unwrap_or_default(),
            credential: s.credential.clone().unwrap_or_default(),
            ..Default::default()
        })
        .collect()
}

/// Yeni peer connection oluştur; yerel ICE adaylarını sunucuya ilet.
pub async fn new_peer_connection(
    ice_servers: Vec<RTCIceServer>,
    session: String,
    peer: String,
    to_server: mpsc::UnboundedSender<ClientMessage>,
) -> Result<Arc<RTCPeerConnection>> {
    // Medya motoru: data channel için gerekmez ama video track (M3) müzakeresi için
    // codec'lerin (H264 dâhil) kayıtlı olması şart. Interceptor'lar NACK/RTCP/TWCC
    // gibi standart RTP mekanizmalarını ekler.
    let mut media = MediaEngine::default();
    media.register_default_codecs()?;
    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut media)?;
    let api = APIBuilder::new()
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .build();
    let config = RTCConfiguration { ice_servers, ..Default::default() };
    let pc = Arc::new(api.new_peer_connection(config).await?);

    // Trickle ICE: yerel aday üretildikçe sunucu üzerinden karşıya gönder.
    pc.on_ice_candidate(Box::new(move |cand: Option<RTCIceCandidate>| {
        let to_server = to_server.clone();
        let session = session.clone();
        let peer = peer.clone();
        Box::pin(async move {
            if let Some(c) = cand {
                match c.to_json() {
                    Ok(init) => {
                        let _ = to_server.send(ClientMessage::Signal {
                            session,
                            to: peer,
                            payload: SignalPayload::IceCandidate {
                                candidate: init.candidate,
                                sdp_mid: init.sdp_mid,
                                sdp_mline_index: init.sdp_mline_index,
                            },
                        });
                    }
                    Err(e) => tracing::warn!("ice candidate json: {e}"),
                }
            }
        })
    }));

    pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
        tracing::info!("bağlantı durumu: {s}");
        Box::pin(async {})
    }));

    Ok(pc)
}

/// Bir data channel'a açılış + mesaj handler'ları bağla.
/// Açıldığında `hello` metnini gönderir; gelen mesajı loglar (test doğrulaması için
/// belirgin `RECV_OK:` öneki ile). Yalnızca çekirdek (media kapalı) M2 testinde kullanılır.
#[allow(dead_code)]
pub fn wire_data_channel(dc: Arc<RTCDataChannel>, hello: String) {
    let dc_open = dc.clone();
    dc.on_open(Box::new(move || {
        let dc = dc_open.clone();
        let hello = hello.clone();
        Box::pin(async move {
            tracing::info!("veri kanalı açıldı → gönderiliyor: {hello}");
            if let Err(e) = dc.send_text(hello).await {
                tracing::warn!("send_text: {e}");
            }
        })
    }));

    dc.on_message(Box::new(move |msg: DataChannelMessage| {
        let txt = String::from_utf8_lossy(&msg.data).to_string();
        tracing::info!("RECV_OK: {txt}");
        Box::pin(async {})
    }));
}
