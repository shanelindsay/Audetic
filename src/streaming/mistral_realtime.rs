use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{AUTHORIZATION, USER_AGENT};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use super::hub::StreamHub;

#[derive(Debug, Clone)]
pub struct MistralRealtimeClient {
    pub api_key: String,
    pub model: String,
    pub api_base_url: String,
    pub sample_rate_hz: u32,
}

#[derive(Debug, Clone)]
pub struct RealtimeRunResult {
    pub final_text: String,
}

impl MistralRealtimeClient {
    pub fn websocket_url(&self) -> Result<String> {
        let mut url = reqwest::Url::parse(self.api_base_url.trim_end_matches('/'))
            .context("Invalid Mistral API base URL")?;

        let scheme = match url.scheme() {
            "https" => "wss",
            "http" => "ws",
            other => {
                return Err(anyhow::anyhow!(
                    "Unsupported Mistral API URL scheme: {}",
                    other
                ))
            }
        };

        url.set_scheme(scheme)
            .map_err(|_| anyhow::anyhow!("Failed to set websocket scheme"))?;
        url.set_path("/v1/audio/transcriptions/realtime");

        {
            let mut qp = url.query_pairs_mut();
            qp.clear();
            qp.append_pair("model", &self.model);
        }

        Ok(url.to_string())
    }

    pub async fn run(
        &self,
        job_id: &str,
        mut chunk_rx: mpsc::Receiver<Vec<i16>>,
        hub: std::sync::Arc<StreamHub>,
    ) -> Result<RealtimeRunResult> {
        let ws_url = self.websocket_url()?;
        let mut request = ws_url
            .as_str()
            .into_client_request()
            .context("Failed to build websocket request")?;

        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.api_key))
                .context("Invalid authorization header")?,
        );

        request.headers_mut().insert(
            USER_AGENT,
            HeaderValue::from_str(&format!("audetic/{}", env!("CARGO_PKG_VERSION")))
                .context("Invalid user-agent header")?,
        );

        info!("Connecting to Mistral realtime websocket");
        let (mut ws, _) = connect_async(request)
            .await
            .context("Failed to connect to Mistral realtime websocket")?;

        // Keep audio format explicit to avoid server defaults mismatch.
        let session_update = json!({
            "type": "session.update",
            "session": {
                "audio_format": {
                    "encoding": "pcm_s16le",
                    "sample_rate": self.sample_rate_hz
                }
            }
        });
        ws.send(Message::Text(session_update.to_string()))
            .await
            .context("Failed to send session.update")?;

        let mut audio_ended = false;
        let mut end_sent_at: Option<Instant> = None;

        let mut partial_accumulator = String::new();
        let mut final_text: Option<String> = None;

        loop {
            if audio_ended {
                let deadline = end_sent_at
                    .map(|start| start + Duration::from_secs(12))
                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(12));

                if Instant::now() >= deadline {
                    warn!("Realtime stream timed out waiting for transcription.done");
                    break;
                }
            }

            tokio::select! {
                maybe_chunk = chunk_rx.recv(), if !audio_ended => {
                    match maybe_chunk {
                        Some(chunk) => {
                            if chunk.is_empty() {
                                continue;
                            }

                            let mut bytes = Vec::with_capacity(chunk.len() * 2);
                            for sample in chunk {
                                bytes.extend_from_slice(&sample.to_le_bytes());
                            }

                            let payload = json!({
                                "type": "input_audio.append",
                                "audio": BASE64.encode(bytes)
                            });

                            ws.send(Message::Text(payload.to_string()))
                                .await
                                .context("Failed to send input_audio.append")?;
                        }
                        None => {
                            ws.send(Message::Text(json!({"type":"input_audio.end"}).to_string()))
                                .await
                                .context("Failed to send input_audio.end")?;
                            audio_ended = true;
                            end_sent_at = Some(Instant::now());
                        }
                    }
                }

                maybe_msg = ws.next() => {
                    let Some(msg) = maybe_msg else {
                        break;
                    };

                    let msg = msg.context("Realtime websocket read error")?;

                    let payload_text = match msg {
                        Message::Text(text) => text,
                        Message::Binary(bin) => String::from_utf8(bin).unwrap_or_default(),
                        Message::Ping(_) | Message::Pong(_) => continue,
                        Message::Close(_) => break,
                        _ => continue,
                    };

                    if payload_text.trim().is_empty() {
                        continue;
                    }

                    let payload: serde_json::Value = match serde_json::from_str(&payload_text) {
                        Ok(value) => value,
                        Err(_) => {
                            debug!("Ignoring non-JSON realtime payload");
                            continue;
                        }
                    };

                    let msg_type = payload
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    match msg_type {
                        "transcription.text.delta" => {
                            let text = payload
                                .get("text")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if !text.is_empty() {
                                partial_accumulator.push_str(text);
                                let _ = hub.partial_text(job_id, &partial_accumulator).await;
                            }
                        }
                        "transcription.segment" => {
                            if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    partial_accumulator.push_str(text);
                                    let _ = hub.partial_text(job_id, &partial_accumulator).await;
                                }
                            }
                        }
                        "transcription.done" => {
                            let text = payload
                                .get("text")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .trim()
                                .to_string();

                            if !text.is_empty() {
                                final_text = Some(text.clone());
                                let _ = hub.final_text(job_id, &text).await;
                            }
                            break;
                        }
                        "error" => {
                            let message = payload
                                .get("error")
                                .and_then(|v| v.get("message"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("Unknown realtime transcription error");

                            let _ = hub.error(Some(job_id), message).await;
                            return Err(anyhow::anyhow!("Mistral realtime error: {}", message));
                        }
                        "session.created" | "session.updated" | "transcription.language" => {
                            // Expected control messages.
                        }
                        other => {
                            if !other.is_empty() {
                                let _ = hub
                                    .warning(
                                        Some(job_id),
                                        &format!("Ignored realtime event type: {}", other),
                                    )
                                    .await;
                            }
                        }
                    }
                }

                _ = tokio::time::sleep(Duration::from_millis(50)), if audio_ended => {}
            }
        }

        let final_text = final_text
            .or_else(|| {
                let trimmed = partial_accumulator.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            })
            .unwrap_or_default();

        Ok(RealtimeRunResult { final_text })
    }
}

#[cfg(test)]
mod tests {
    use super::MistralRealtimeClient;

    #[test]
    fn websocket_url_uses_wss_and_model_query() {
        let client = MistralRealtimeClient {
            api_key: "test".to_string(),
            model: "voxtral-mini".to_string(),
            api_base_url: "https://api.mistral.ai".to_string(),
            sample_rate_hz: 16_000,
        };

        let url = client.websocket_url().expect("url should build");
        assert!(url.starts_with("wss://api.mistral.ai/v1/audio/transcriptions/realtime?"));
        assert!(url.contains("model=voxtral-mini"));
    }

    #[test]
    fn websocket_url_rejects_invalid_scheme() {
        let client = MistralRealtimeClient {
            api_key: "test".to_string(),
            model: "voxtral-mini".to_string(),
            api_base_url: "ftp://example.com".to_string(),
            sample_rate_hz: 16_000,
        };

        assert!(client.websocket_url().is_err());
    }
}
