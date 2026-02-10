use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use tokio::sync::{mpsc, watch, Mutex};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

const CLIP_THRESHOLD: f32 = 0.995;
const CLIP_RATIO_THRESHOLD: f32 = 0.005;

use crate::audio::{
    AudioStreamManager, BehaviorOptions, CompletedJob, JobOptions, RecordingPhase,
    RecordingStatusHandle, ToggleResult,
};
use crate::config::StreamingConfig;
use crate::db::{self, VoiceToTextData, Workflow, WorkflowData, WorkflowType};
use crate::text_io::TextIoService;
use crate::ui::Indicator;

use super::hub::StreamHub;
use super::mistral_realtime::MistralRealtimeClient;

struct ActiveStreaming {
    job_id: String,
    stop_tx: watch::Sender<bool>,
}

pub struct StreamingMachine {
    audio: Arc<Mutex<AudioStreamManager>>,
    indicator: Indicator,
    text_io: TextIoService,
    behavior: BehaviorOptions,
    status: RecordingStatusHandle,
    hub: Arc<StreamHub>,
    config: StreamingConfig,
    active: Arc<Mutex<Option<ActiveStreaming>>>,
}

impl StreamingMachine {
    pub fn new(
        audio: Arc<Mutex<AudioStreamManager>>,
        indicator: Indicator,
        text_io: TextIoService,
        behavior: BehaviorOptions,
        status: RecordingStatusHandle,
        hub: Arc<StreamHub>,
        config: StreamingConfig,
    ) -> Self {
        Self {
            audio,
            indicator,
            text_io,
            behavior,
            status,
            hub,
            config,
            active: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn toggle(&self, options: Option<JobOptions>) -> Result<ToggleResult> {
        let current = self.status.get().await;

        match current.phase {
            RecordingPhase::Idle | RecordingPhase::Error => {
                let job_id = Uuid::new_v4().to_string();
                let job_options = options.unwrap_or(JobOptions {
                    copy_to_clipboard: true,
                    auto_paste: self.behavior.auto_paste,
                    append_newline: Some(self.behavior.append_newline),
                    send_enter: Some(false),
                });

                self.start_stream_session(&job_id, job_options).await?;
                Ok(ToggleResult {
                    phase: RecordingPhase::Recording,
                    job_id: Some(job_id),
                })
            }
            RecordingPhase::Recording => {
                let mut active = self.active.lock().await;
                if let Some(session) = active.take() {
                    if let Some(requested) = options {
                        let mut merged = current.current_job_options.unwrap_or(JobOptions {
                            copy_to_clipboard: true,
                            auto_paste: self.behavior.auto_paste,
                            append_newline: Some(self.behavior.append_newline),
                            send_enter: Some(false),
                        });

                        merged.copy_to_clipboard = requested.copy_to_clipboard;
                        merged.auto_paste = requested.auto_paste;
                        if requested.append_newline.is_some() {
                            merged.append_newline = requested.append_newline;
                        }
                        if requested.send_enter.is_some() {
                            merged.send_enter = requested.send_enter;
                        }

                        self.status.set_current_job_options(merged).await;
                    }

                    if let Err(e) = session.stop_tx.send(true) {
                        warn!("Failed to send stop signal for streaming session: {}", e);
                    }

                    self.status.set_processing().await;
                    if let Err(e) = self.indicator.show_processing().await {
                        warn!("Failed to show processing indicator: {}", e);
                    }

                    return Ok(ToggleResult {
                        phase: RecordingPhase::Processing,
                        job_id: Some(session.job_id),
                    });
                }

                warn!("Streaming toggle requested while status=recording but no active session");
                self.status.set_processing().await;
                Ok(ToggleResult {
                    phase: RecordingPhase::Processing,
                    job_id: current.current_job_id,
                })
            }
            RecordingPhase::Processing => Ok(ToggleResult {
                phase: RecordingPhase::Processing,
                job_id: current.current_job_id,
            }),
        }
    }

    /// Start recording if currently idle/error; otherwise returns current phase without side effects.
    pub async fn start(&self, options: Option<JobOptions>) -> Result<ToggleResult> {
        let current = self.status.get().await;
        match current.phase {
            RecordingPhase::Idle | RecordingPhase::Error => self.toggle(options).await,
            _ => Ok(ToggleResult {
                phase: current.phase,
                job_id: current.current_job_id,
            }),
        }
    }

    /// Stop recording if currently recording; otherwise returns current phase without side effects.
    pub async fn stop(&self, options: Option<JobOptions>) -> Result<ToggleResult> {
        let current = self.status.get().await;
        match current.phase {
            RecordingPhase::Recording => self.toggle(options).await,
            _ => Ok(ToggleResult {
                phase: current.phase,
                job_id: current.current_job_id,
            }),
        }
    }

    async fn start_stream_session(&self, job_id: &str, job_options: JobOptions) -> Result<()> {
        info!(
            "Starting streaming session job_id={} options={:?}",
            job_id, job_options
        );

        if let Err(e) = self.indicator.show_recording().await {
            warn!("Failed to show recording indicator: {}", e);
        }

        {
            let recorder = self.audio.lock().await;
            recorder.start_recording().await?;
        }

        let (stop_tx, stop_rx) = watch::channel(false);
        {
            let mut active = self.active.lock().await;
            *active = Some(ActiveStreaming {
                job_id: job_id.to_string(),
                stop_tx,
            });
        }

        self.status.start_job(job_id.to_string(), job_options).await;
        let _ = self.hub.session_started(job_id).await;

        let ctx = StreamTaskContext {
            job_id: job_id.to_string(),
            job_options,
            audio: self.audio.clone(),
            indicator: self.indicator.clone(),
            text_io: self.text_io.clone(),
            behavior: self.behavior,
            status: self.status.clone(),
            hub: self.hub.clone(),
            config: self.config.clone(),
            active: self.active.clone(),
        };

        tokio::task::spawn_local(async move {
            if let Err(e) = run_stream_task(ctx, stop_rx).await {
                error!("Streaming task failed: {}", e);
            }
        });

        Ok(())
    }
}

#[derive(Clone)]
struct StreamTaskContext {
    job_id: String,
    job_options: JobOptions,
    audio: Arc<Mutex<AudioStreamManager>>,
    indicator: Indicator,
    text_io: TextIoService,
    behavior: BehaviorOptions,
    status: RecordingStatusHandle,
    hub: Arc<StreamHub>,
    config: StreamingConfig,
    active: Arc<Mutex<Option<ActiveStreaming>>>,
}

#[derive(Debug, Clone, Copy)]
struct AudioPumpConfig {
    sample_rate_hz: u32,
    chunk_ms: u32,
    max_chunks: usize,
    silence_timeout_ms: u64,
}

async fn run_stream_task(ctx: StreamTaskContext, stop_rx: watch::Receiver<bool>) -> Result<()> {
    let pipeline_result = run_stream_pipeline(&ctx, stop_rx).await;

    {
        let recorder = ctx.audio.lock().await;
        if let Err(e) = recorder.stop_recording_discard().await {
            warn!("Failed to stop/discard streaming recorder cleanly: {}", e);
        }
    }

    let final_outcome = match pipeline_result {
        Ok(text) => {
            let text = text.trim().to_string();
            if text.is_empty() {
                ctx.status.set_phase(RecordingPhase::Idle, None).await;
                let _ = ctx.indicator.show_error("No speech detected").await;
                None
            } else {
                let effective_options = ctx
                    .status
                    .get_current_job_options()
                    .await
                    .unwrap_or(ctx.job_options);
                info!(
                    "Applying streaming commit job_id={} options={:?}",
                    ctx.job_id, effective_options
                );

                if let Err(e) = apply_commit(&ctx, &text, &effective_options).await {
                    warn!("Failed to apply commit target: {}", e);
                }

                if let Err(e) = ctx.indicator.show_complete(&text).await {
                    warn!("Failed to show completion indicator: {}", e);
                }

                let db_text = text.clone();
                let db_job_id = ctx.job_id.clone();
                let db_id = tokio::task::spawn_blocking(move || {
                    save_stream_session_to_database(&db_text, &db_job_id)
                })
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(0);

                Some(CompletedJob {
                    job_id: ctx.job_id.clone(),
                    history_id: db_id,
                    text,
                    created_at: chrono::Utc::now().to_rfc3339(),
                })
            }
        }
        Err(err) => {
            let message = err.to_string();
            let _ = ctx.hub.error(Some(&ctx.job_id), &message).await;
            ctx.status.fail_job(message.clone()).await;
            let _ = ctx
                .indicator
                .show_error(&format!("Streaming transcription failed: {}", message))
                .await;
            None
        }
    };

    if let Some(completed) = final_outcome {
        ctx.status.complete_job(completed).await;
    }

    let _ = ctx.hub.session_stopped(&ctx.job_id).await;
    clear_active_slot(&ctx.active, &ctx.job_id).await;

    Ok(())
}

async fn run_stream_pipeline(
    ctx: &StreamTaskContext,
    stop_rx: watch::Receiver<bool>,
) -> Result<String> {
    if ctx.config.provider.trim() != "mistral_realtime" {
        return Err(anyhow!(
            "Unsupported streaming provider '{}'. Use 'mistral_realtime'",
            ctx.config.provider
        ));
    }

    let api_key = resolve_api_key(&ctx.config)?;

    let max_chunks = 50usize;
    let (chunk_tx, chunk_rx) = mpsc::channel::<Vec<i16>>(max_chunks);
    let pump_config = AudioPumpConfig {
        sample_rate_hz: ctx.config.sample_rate_hz,
        chunk_ms: ctx.config.chunk_ms,
        max_chunks,
        silence_timeout_ms: ctx.config.silence_timeout_ms,
    };

    let pump_audio = tokio::task::spawn_local(audio_pump_loop(
        ctx.audio.clone(),
        chunk_tx,
        ctx.hub.clone(),
        ctx.job_id.clone(),
        stop_rx,
        pump_config,
    ));

    let client = MistralRealtimeClient {
        api_key,
        model: ctx.config.model.clone(),
        api_base_url: ctx.config.api_base_url.clone(),
        sample_rate_hz: ctx.config.sample_rate_hz,
    };

    let transcription_result = client.run(&ctx.job_id, chunk_rx, ctx.hub.clone()).await;

    if let Err(e) = pump_audio.await {
        warn!("Audio pump task join error: {}", e);
    }

    let result = transcription_result?;
    Ok(result.final_text)
}

async fn clear_active_slot(active: &Arc<Mutex<Option<ActiveStreaming>>>, job_id: &str) {
    let mut guard = active.lock().await;
    if guard
        .as_ref()
        .map(|session| session.job_id.as_str())
        .is_some_and(|id| id == job_id)
    {
        *guard = None;
    }
}

async fn audio_pump_loop(
    audio: Arc<Mutex<AudioStreamManager>>,
    chunk_tx: mpsc::Sender<Vec<i16>>,
    hub: Arc<StreamHub>,
    job_id: String,
    mut stop_rx: watch::Receiver<bool>,
    config: AudioPumpConfig,
) {
    let sample_rate_hz = config.sample_rate_hz;
    let chunk_ms = config.chunk_ms;
    let max_chunks = config.max_chunks;
    let chunk_size = (((sample_rate_hz as u64) * (chunk_ms as u64)) / 1000) as usize;
    let mut pending: Vec<i16> = Vec::with_capacity(chunk_size.saturating_mul(2));
    let mut last_level_emit = Instant::now();
    let mut idle_after_stop_ticks = 0usize;
    let drain_grace_ticks = (config.silence_timeout_ms / 20).max(5) as usize;

    loop {
        let should_stop = *stop_rx.borrow();

        let samples = {
            let recorder = audio.lock().await;
            recorder.drain_samples()
        };

        if samples.is_empty() {
            if should_stop {
                idle_after_stop_ticks = idle_after_stop_ticks.saturating_add(1);
                if idle_after_stop_ticks > drain_grace_ticks {
                    break;
                }
            }
        } else {
            idle_after_stop_ticks = 0;

            if last_level_emit.elapsed() >= Duration::from_millis(33) {
                let (rms_dbfs, peak_dbfs, clipping) = measure_level(&samples);
                let _ = hub
                    .audio_level(Some(&job_id), rms_dbfs, peak_dbfs, clipping)
                    .await;
                last_level_emit = Instant::now();
            }

            for sample in samples {
                let clamped = sample.clamp(-1.0, 1.0);
                pending.push((clamped * i16::MAX as f32) as i16);
            }

            while pending.len() >= chunk_size && chunk_size > 0 {
                let chunk: Vec<i16> = pending.drain(..chunk_size).collect();
                match chunk_tx.try_send(chunk) {
                    Ok(()) => {
                        let depth = max_chunks.saturating_sub(chunk_tx.capacity());
                        hub.set_queue_depth(depth).await;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_dropped)) => {
                        hub.increment_dropped_chunks().await;
                        let _ = hub
                            .warning(Some(&job_id), "Audio chunk dropped due to full queue")
                            .await;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
                }
            }
        }

        if should_stop && pending.is_empty() {
            break;
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            changed = stop_rx.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }

    if !pending.is_empty() {
        let _ = chunk_tx.send(pending).await;
    }
}

fn measure_level(samples: &[f32]) -> (f32, f32, bool) {
    if samples.is_empty() {
        return (-90.0, -90.0, false);
    }

    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f32;
    let mut clip_count = 0usize;

    for &sample in samples {
        let abs = sample.abs();
        if abs > peak {
            peak = abs;
        }
        if abs >= CLIP_THRESHOLD {
            clip_count += 1;
        }
        sum_sq += (sample as f64) * (sample as f64);
    }

    let rms = (sum_sq / samples.len() as f64).sqrt() as f32;
    let rms_dbfs = 20.0 * rms.max(1e-9).log10();
    let peak_dbfs = 20.0 * peak.max(1e-9).log10();
    let clip_ratio = clip_count as f32 / samples.len() as f32;
    let clipping = peak >= CLIP_THRESHOLD && clip_ratio >= CLIP_RATIO_THRESHOLD;

    (rms_dbfs, peak_dbfs, clipping)
}

fn resolve_api_key(config: &StreamingConfig) -> Result<String> {
    if let Some(key) = config.api_key.as_ref() {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    if let Ok(key) = std::env::var("MISTRAL_API_KEY") {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    Err(anyhow!(
        "MISTRAL_API_KEY missing. Set [streaming].api_key or environment variable"
    ))
}

async fn apply_commit(ctx: &StreamTaskContext, text: &str, job_options: &JobOptions) -> Result<()> {
    let commit_target = ctx.config.commit_target.as_str();

    if commit_target == "none" {
        return Ok(());
    }

    if job_options.copy_to_clipboard {
        ctx.text_io.copy_to_clipboard(text).await?;
    }

    if commit_target == "text_io" && job_options.auto_paste {
        let append_newline = job_options
            .append_newline
            .unwrap_or(ctx.behavior.append_newline);
        let inject_text = if append_newline {
            format!("{}\n", text)
        } else {
            text.to_string()
        };

        if let Err(err) = ctx.text_io.inject_text(&inject_text).await {
            warn!("Text injection failed: {}", err);
            if job_options.copy_to_clipboard {
                let _ = ctx.text_io.paste_from_clipboard().await;
            }
        }

        if job_options.send_enter.unwrap_or(false) {
            tokio::time::sleep(Duration::from_millis(180)).await;
            if let Err(err) = ctx.text_io.send_enter_key().await {
                warn!("Enter key send failed: {}", err);
            }
        }
    }

    Ok(())
}

fn save_stream_session_to_database(text: &str, job_id: &str) -> Result<i64> {
    let conn = db::init_db()?;

    let workflow_data = WorkflowData::VoiceToText(VoiceToTextData {
        text: text.to_string(),
        audio_path: format!("stream://{}", job_id),
    });

    let workflow = Workflow::new(WorkflowType::VoiceToText, workflow_data);
    let id = db::insert_workflow(&conn, &workflow)?;

    let pruned = db::prune_old_workflows(&conn, 10_000)?;
    if pruned > 0 {
        debug!("Pruned {} old transcriptions from database", pruned);
    }

    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_measure_level() {
        let (rms_db, peak_db, clipping) = measure_level(&[0.0, 0.5, -0.5, 1.0]);
        assert!(rms_db > -20.0);
        assert!(peak_db > -0.1);
        assert!(clipping);
    }

    #[test]
    fn test_measure_level_empty() {
        let (rms_db, peak_db, clipping) = measure_level(&[]);
        assert_eq!(rms_db, -90.0);
        assert_eq!(peak_db, -90.0);
        assert!(!clipping);
    }

    #[test]
    fn test_save_stream_session_path_format() {
        let path = format!("stream://{}", "abc-123");
        assert_eq!(path, "stream://abc-123");
    }

    #[test]
    fn test_resolve_api_key_prefers_config() {
        let cfg = StreamingConfig {
            api_key: Some("abc".to_string()),
            ..StreamingConfig::default()
        };
        let key = resolve_api_key(&cfg).expect("config key should resolve");
        assert_eq!(key, "abc");
    }
}
