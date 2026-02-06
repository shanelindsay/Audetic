use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    pub id: u64,
    pub event_type: String,
    pub job_id: Option<String>,
    pub ts_ms: i64,
    pub data: serde_json::Value,
}

impl StreamEvent {
    pub fn new(
        id: u64,
        event_type: impl Into<String>,
        job_id: Option<String>,
        data: serde_json::Value,
    ) -> Self {
        Self {
            id,
            event_type: event_type.into(),
            job_id,
            ts_ms: Utc::now().timestamp_millis(),
            data,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamStatusSnapshot {
    pub active: bool,
    pub job_id: Option<String>,
    pub queue_depth: usize,
    pub dropped_chunks: u64,
    pub connected_clients: usize,
    pub sessions_started: u64,
    pub sessions_stopped: u64,
    pub partial_events: u64,
    pub final_events: u64,
    pub error_events: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct StreamStatusState {
    pub active: bool,
    pub job_id: Option<String>,
    pub queue_depth: usize,
    pub dropped_chunks: u64,
    pub connected_clients: usize,
    pub sessions_started: u64,
    pub sessions_stopped: u64,
    pub partial_events: u64,
    pub final_events: u64,
    pub error_events: u64,
    pub last_error: Option<String>,
}

impl From<&StreamStatusState> for StreamStatusSnapshot {
    fn from(value: &StreamStatusState) -> Self {
        Self {
            active: value.active,
            job_id: value.job_id.clone(),
            queue_depth: value.queue_depth,
            dropped_chunks: value.dropped_chunks,
            connected_clients: value.connected_clients,
            sessions_started: value.sessions_started,
            sessions_stopped: value.sessions_stopped,
            partial_events: value.partial_events,
            final_events: value.final_events,
            error_events: value.error_events,
            last_error: value.last_error.clone(),
        }
    }
}

pub fn audio_level_payload(rms_dbfs: f32, peak_dbfs: f32, clipping: bool) -> serde_json::Value {
    json!({
        "rms_dbfs": rms_dbfs,
        "peak_dbfs": peak_dbfs,
        "clipping": clipping
    })
}

#[cfg(test)]
mod tests {
    use super::StreamEvent;

    #[test]
    fn stream_event_serializes_expected_shape() {
        let event = StreamEvent::new(
            42,
            "partial",
            Some("job-1".to_string()),
            serde_json::json!({ "text": "hello" }),
        );

        let value = serde_json::to_value(event).expect("event should serialize");
        assert_eq!(value.get("id").and_then(|v| v.as_u64()), Some(42));
        assert_eq!(
            value.get("event_type").and_then(|v| v.as_str()),
            Some("partial")
        );
        assert_eq!(value.get("job_id").and_then(|v| v.as_str()), Some("job-1"));
        assert!(value.get("ts_ms").and_then(|v| v.as_i64()).is_some());
        assert_eq!(
            value
                .get("data")
                .and_then(|v| v.get("text"))
                .and_then(|v| v.as_str()),
            Some("hello")
        );
    }
}
