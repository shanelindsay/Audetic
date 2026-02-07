//! Recording control endpoints.
//!
//! Provides HTTP endpoints for:
//! - Toggling recording (POST /toggle)
//! - Getting recording status (GET /status)

use crate::audio::{JobOptions, RecordingPhase, RecordingStatus, RecordingStatusHandle};
use crate::config::WaybarConfig;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::{error, info};

/// Request body for the toggle recording endpoint.
/// All fields are optional - if not provided, defaults are used from config.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ToggleRequest {
    /// Whether to copy the transcription to clipboard (default: true)
    #[serde(default)]
    pub copy_to_clipboard: Option<bool>,
    /// Whether to auto-paste/inject text into the focused app (default: from config)
    #[serde(default)]
    pub auto_paste: Option<bool>,
    /// Whether to append newline (Enter) after auto-paste (default: from config)
    #[serde(default)]
    pub append_newline: Option<bool>,
    /// Whether to send a real Enter keypress after auto-paste (default: false)
    #[serde(default)]
    pub send_enter: Option<bool>,
}

#[derive(Clone)]
pub enum ApiCommand {
    /// Toggle recording with optional per-job options
    ToggleRecording(Option<JobOptions>),
    /// Start recording only when idle/error
    StartRecording(Option<JobOptions>),
    /// Stop recording only when currently recording
    StopRecording(Option<JobOptions>),
}

#[derive(Clone)]
pub struct RecordingState {
    pub tx: mpsc::Sender<ApiCommand>,
    pub status: RecordingStatusHandle,
    pub waybar_config: WaybarConfig,
}

/// Creates the recording router with all recording-related endpoints.
pub fn router(state: RecordingState) -> Router {
    Router::new()
        .route("/toggle", post(toggle_recording))
        .route("/start", post(start_recording))
        .route("/stop", post(stop_recording))
        .route("/status", get(recording_status))
        .with_state(state)
}

/// Toggles recording on or off with optional per-job options.
///
/// # Request Body
/// Optional JSON with fields:
/// - `copy_to_clipboard`: bool - Copy transcription to clipboard
/// - `auto_paste`: bool - Auto-paste/inject text into focused app
/// - `append_newline`: bool - Append newline/Enter after injected text
/// - `send_enter`: bool - Send a real Enter keypress after injected text
///
/// # Response
/// Returns JSON with recording status and current job information.
async fn toggle_recording(
    State(state): State<RecordingState>,
    body: Option<Json<ToggleRequest>>,
) -> Result<Json<Value>, StatusCode> {
    let job_options = parse_job_options(body);

    info!(
        "Toggle recording command received via API with options: {:?}",
        job_options
    );

    dispatch_recording_command(&state, ApiCommand::ToggleRecording(job_options)).await
}

async fn start_recording(
    State(state): State<RecordingState>,
    body: Option<Json<ToggleRequest>>,
) -> Result<Json<Value>, StatusCode> {
    let job_options = parse_job_options(body);
    info!(
        "Start recording command received via API with options: {:?}",
        job_options
    );
    dispatch_recording_command(&state, ApiCommand::StartRecording(job_options)).await
}

async fn stop_recording(
    State(state): State<RecordingState>,
    body: Option<Json<ToggleRequest>>,
) -> Result<Json<Value>, StatusCode> {
    let job_options = parse_job_options(body);
    info!(
        "Stop recording command received via API with options: {:?}",
        job_options
    );
    dispatch_recording_command(&state, ApiCommand::StopRecording(job_options)).await
}

fn parse_job_options(body: Option<Json<ToggleRequest>>) -> Option<JobOptions> {
    body.and_then(|Json(req)| {
        if req.copy_to_clipboard.is_some()
            || req.auto_paste.is_some()
            || req.append_newline.is_some()
            || req.send_enter.is_some()
        {
            Some(JobOptions {
                copy_to_clipboard: req.copy_to_clipboard.unwrap_or(true),
                auto_paste: req.auto_paste.unwrap_or(true),
                append_newline: req.append_newline,
                send_enter: req.send_enter,
            })
        } else {
            None
        }
    })
}

async fn dispatch_recording_command(
    state: &RecordingState,
    command: ApiCommand,
) -> Result<Json<Value>, StatusCode> {
    match state.tx.send(command).await {
        Ok(_) => {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let status = state.status.get().await;

            Ok(Json(json!({
                "success": true,
                "phase": status.phase.as_str(),
                "job_id": status.current_job_id,
                "message": format!("Recording {}", status.phase.as_str())
            })))
        }
        Err(e) => {
            error!("Failed to send recording command: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Gets the current recording status.
///
/// # Query Parameters
/// - `style=waybar` - Returns response formatted for Waybar integration
///
/// # Response
/// Returns JSON with current recording phase, job ID, and last completed job info.
/// When `style=waybar` is specified, returns Waybar-formatted response with text, class, and tooltip.
async fn recording_status(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<RecordingState>,
) -> Json<Value> {
    let status = state.status.get().await;

    // Check if waybar style is requested
    if params.get("style") == Some(&"waybar".to_string()) {
        return Json(generate_waybar_response(&status, &state.waybar_config));
    }

    // Build last_completed_job object if available
    let last_completed_job = status.last_completed_job.as_ref().map(|job| {
        json!({
            "job_id": job.job_id,
            "history_id": job.history_id,
            "text": job.text,
            "created_at": job.created_at
        })
    });

    // Default JSON response with full job context
    Json(json!({
        "recording": status.phase == RecordingPhase::Recording,
        "phase": status.phase.as_str(),
        "job_id": status.current_job_id,
        "last_completed_job": last_completed_job,
        "last_error": status.last_error,
    }))
}

/// Generates a response formatted for Waybar integration.
///
/// Maps recording phases to appropriate Waybar display elements:
/// - Idle: Shows configured idle text and tooltip
/// - Recording: Shows configured recording text and tooltip
/// - Processing: Shows processing icon
/// - Error: Shows error icon with error message
fn generate_waybar_response(status: &RecordingStatus, config: &WaybarConfig) -> Value {
    let (text, class, tooltip) = match status.phase {
        RecordingPhase::Idle => (
            config.idle_text.clone(),
            "audetic-idle".to_string(),
            config.idle_tooltip.clone(),
        ),
        RecordingPhase::Recording => (
            config.recording_text.clone(),
            "audetic-recording".to_string(),
            config.recording_tooltip.clone(),
        ),
        RecordingPhase::Processing => (
            "󰦖".to_string(),
            "audetic-processing".to_string(),
            "Processing transcription".to_string(),
        ),
        RecordingPhase::Error => (
            "".to_string(),
            "audetic-error".to_string(),
            status
                .last_error
                .clone()
                .unwrap_or_else(|| "Recording error".to_string()),
        ),
    };

    json!({
        "text": text,
        "class": class,
        "tooltip": tooltip
    })
}
