//! REST API server for Audetic.
//!
//! Provides HTTP endpoints for:
//! - Recording control (toggle, status)
//! - Transcription history
//! - Keybinding management
//! - Provider configuration
//! - Update management
//! - Application logs

pub mod error;
pub mod routes;

use crate::config::Config;
use crate::streaming::StreamHub;
use anyhow::Result;
use axum::{response::Json, routing::get, Router};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceBuilder;
use tracing::info;

pub use routes::recording::{ApiCommand, RecordingState, ToggleRequest};

pub struct ApiServer {
    port: u16,
    recording_state: RecordingState,
    stream_hub: Option<Arc<StreamHub>>,
}

impl ApiServer {
    pub fn new(
        tx: tokio::sync::mpsc::Sender<ApiCommand>,
        status: crate::audio::RecordingStatusHandle,
        config: &Config,
        stream_hub: Option<Arc<StreamHub>>,
    ) -> Self {
        Self {
            port: 3737, // WHSP in numbers
            recording_state: RecordingState {
                tx,
                status,
                waybar_config: config.ui.waybar.clone(),
            },
            stream_hub,
        }
    }

    pub async fn start(self) -> Result<()> {
        let mut app = Router::new()
            // Root and version endpoints
            .route("/", get(status))
            .route("/version", get(version))
            // Recording control endpoints
            .nest("", routes::recording::router(self.recording_state))
            // Other API routes
            .nest("/history", routes::history::router())
            .nest("/keybind", routes::keybind::router())
            .nest("/logs", routes::logs::router())
            .nest("/provider", routes::provider::router())
            .nest("/update", routes::update::router())
            .layer(ServiceBuilder::new());

        if let Some(hub) = self.stream_hub {
            app = app.nest(
                "/stream",
                routes::stream::router(routes::stream::StreamState { hub }),
            );
        }

        let listener = tokio::net::TcpListener::bind(&format!("127.0.0.1:{}", self.port)).await?;

        info!("API server listening on http://127.0.0.1:{}", self.port);
        info!("Endpoints:");
        info!("  GET  /              - Service info");
        info!("  POST /toggle        - Toggle recording");
        info!("  POST /start         - Start recording (no toggle)");
        info!("  POST /stop          - Stop recording (no toggle)");
        info!("  GET  /status        - Get recording status");
        info!("  GET  /version       - Get version info");
        info!("  GET  /history       - List transcription history");
        info!("  GET  /history/:id   - Get single transcription");
        info!("  GET  /keybind/status - Get keybinding status");
        info!("  POST /keybind/install - Install keybinding");
        info!("  DELETE /keybind     - Uninstall keybinding");
        info!("  GET  /logs          - Get application logs");
        info!("  GET  /provider      - Get provider config");
        info!("  GET  /provider/status - Get provider status");
        info!("  GET  /update/check  - Check for updates");
        info!("  POST /update/install - Install update");
        info!("  PUT  /update/auto   - Toggle auto-update");
        info!("  GET  /stream/events - Live SSE stream events");
        info!("  GET  /stream/status - Streaming status snapshot");

        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn status() -> Json<Value> {
    Json(json!({
        "service": "audetic",
        "version": env!("CARGO_PKG_VERSION"),
        "status": "running"
    }))
}

async fn version() -> Json<Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "name": "audetic"
    }))
}
