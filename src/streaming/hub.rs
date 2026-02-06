use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use serde_json::json;
use tokio::sync::{broadcast, RwLock};

use super::events::{audio_level_payload, StreamEvent, StreamStatusSnapshot, StreamStatusState};

pub struct StreamHub {
    next_id: AtomicU64,
    events_tx: broadcast::Sender<StreamEvent>,
    state: RwLock<StreamStatusState>,
}

impl Default for StreamHub {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamHub {
    pub fn new() -> Self {
        let (events_tx, _) = broadcast::channel(1024);
        Self {
            next_id: AtomicU64::new(1),
            events_tx,
            state: RwLock::new(StreamStatusState::default()),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StreamEvent> {
        self.events_tx.subscribe()
    }

    pub async fn snapshot(&self) -> StreamStatusSnapshot {
        let state = self.state.read().await;
        StreamStatusSnapshot::from(&*state)
    }

    pub async fn client_connected(&self) {
        let mut state = self.state.write().await;
        state.connected_clients = state.connected_clients.saturating_add(1);
    }

    pub async fn client_disconnected(&self) {
        let mut state = self.state.write().await;
        state.connected_clients = state.connected_clients.saturating_sub(1);
    }

    pub async fn set_queue_depth(&self, depth: usize) {
        let mut state = self.state.write().await;
        state.queue_depth = depth;
    }

    pub async fn increment_dropped_chunks(&self) {
        let mut state = self.state.write().await;
        state.dropped_chunks = state.dropped_chunks.saturating_add(1);
    }

    pub async fn session_started(&self, job_id: &str) -> Result<()> {
        {
            let mut state = self.state.write().await;
            state.active = true;
            state.job_id = Some(job_id.to_string());
            state.sessions_started = state.sessions_started.saturating_add(1);
            state.last_error = None;
        }

        self.emit(
            "session_started",
            Some(job_id.to_string()),
            json!({ "message": "Streaming session started" }),
        )
        .await
    }

    pub async fn session_stopped(&self, job_id: &str) -> Result<()> {
        {
            let mut state = self.state.write().await;
            state.active = false;
            state.job_id = None;
            state.queue_depth = 0;
            state.sessions_stopped = state.sessions_stopped.saturating_add(1);
        }

        self.emit(
            "session_stopped",
            Some(job_id.to_string()),
            json!({ "message": "Streaming session stopped" }),
        )
        .await
    }

    pub async fn partial_text(&self, job_id: &str, text: &str) -> Result<()> {
        {
            let mut state = self.state.write().await;
            state.partial_events = state.partial_events.saturating_add(1);
        }

        self.emit("partial", Some(job_id.to_string()), json!({ "text": text }))
            .await
    }

    pub async fn final_text(&self, job_id: &str, text: &str) -> Result<()> {
        {
            let mut state = self.state.write().await;
            state.final_events = state.final_events.saturating_add(1);
        }

        self.emit("final", Some(job_id.to_string()), json!({ "text": text }))
            .await
    }

    pub async fn audio_level(
        &self,
        job_id: Option<&str>,
        rms_dbfs: f32,
        peak_dbfs: f32,
        clipping: bool,
    ) -> Result<()> {
        self.emit(
            "audio_level",
            job_id.map(ToString::to_string),
            audio_level_payload(rms_dbfs, peak_dbfs, clipping),
        )
        .await
    }

    pub async fn error(&self, job_id: Option<&str>, message: &str) -> Result<()> {
        {
            let mut state = self.state.write().await;
            state.error_events = state.error_events.saturating_add(1);
            state.last_error = Some(message.to_string());
        }

        self.emit(
            "error",
            job_id.map(ToString::to_string),
            json!({ "message": message }),
        )
        .await
    }

    pub async fn warning(&self, job_id: Option<&str>, message: &str) -> Result<()> {
        self.emit(
            "warning",
            job_id.map(ToString::to_string),
            json!({ "message": message }),
        )
        .await
    }

    pub async fn emit(
        &self,
        event_type: &str,
        job_id: Option<String>,
        data: serde_json::Value,
    ) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let event = StreamEvent::new(id, event_type, job_id, data);
        let _ = self.events_tx.send(event);
        Ok(())
    }
}
