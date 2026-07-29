//! aWay sinyal sunucusu — kütüphane arayüzü.
//!
//! `main.rs` bunun ince bir sarmalayıcısıdır; entegrasyon testleri de sunucuyu
//! süreç-içi ayağa kaldırmak için buradaki `build_app`'i kullanır.

pub mod config;
pub mod state;
pub mod turn;
pub mod users;
pub mod ws;

use axum::routing::get;
use axum::Router;
use state::AppState;
use std::sync::Arc;
use tower_http::trace::TraceLayer;

/// Router'ı verilen paylaşılan durumla kur.
pub fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws::ws_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
