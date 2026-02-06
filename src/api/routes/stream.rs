//! Streaming event routes.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Json;
use axum::routing::get;
use axum::Router;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

use crate::streaming::events::StreamStatusSnapshot;
use crate::streaming::StreamHub;

#[derive(Clone)]
pub struct StreamState {
    pub hub: Arc<StreamHub>,
}

struct ClientDisconnectGuard {
    hub: Arc<StreamHub>,
}

impl Drop for ClientDisconnectGuard {
    fn drop(&mut self) {
        let hub = self.hub.clone();
        tokio::spawn(async move {
            hub.client_disconnected().await;
        });
    }
}

pub fn router(state: StreamState) -> Router {
    Router::new()
        .route("/events", get(stream_events))
        .route("/status", get(stream_status))
        .with_state(state)
}

async fn stream_events(
    State(state): State<StreamState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    state.hub.client_connected().await;
    let guard = Arc::new(ClientDisconnectGuard {
        hub: state.hub.clone(),
    });

    let stream = BroadcastStream::new(state.hub.subscribe()).map(move |result| {
        let _guard = &guard;
        match result {
            Ok(event) => {
                let payload = match serde_json::to_string(&event) {
                    Ok(payload) => payload,
                    Err(_) => {
                        return Ok(Event::default().event("error").data(
                            serde_json::json!({
                                "message": "failed to serialize stream event"
                            })
                            .to_string(),
                        ));
                    }
                };

                Ok(Event::default()
                    .id(event.id.to_string())
                    .event(event.event_type)
                    .data(payload))
            }
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
                Ok(Event::default().event("warning").data(
                    serde_json::json!({
                        "message": format!("SSE subscriber lagged by {skipped} events")
                    })
                    .to_string(),
                ))
            }
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

async fn stream_status(State(state): State<StreamState>) -> Json<StreamStatusSnapshot> {
    Json(state.hub.snapshot().await)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::extract::State;
    use axum::response::Json;

    use crate::streaming::StreamHub;

    use super::{stream_status, StreamState};

    #[tokio::test]
    async fn status_endpoint_returns_snapshot() {
        let hub = Arc::new(StreamHub::new());
        let state = StreamState { hub };

        let Json(snapshot) = stream_status(State(state)).await;
        assert!(!snapshot.active);
        assert_eq!(snapshot.queue_depth, 0);
    }
}
