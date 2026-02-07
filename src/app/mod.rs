#![allow(clippy::arc_with_non_send_sync)]

use crate::api::{ApiCommand, ApiServer};
use crate::audio::{
    AudioDuckingController, AudioStreamManager, BehaviorOptions, RecordingMachine, RecordingPhase,
    RecordingStatusHandle, ToggleResult,
};
use crate::config::Config;
use crate::streaming::{StreamHub, StreamingMachine};
use crate::text_io::TextIoService;
use crate::transcription::{ProviderConfig, Transcriber, TranscriptionService};
use crate::ui::Indicator;
use crate::update::{UpdateConfig, UpdateEngine};
use anyhow::{anyhow, Result};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

pub async fn run_service() -> Result<()> {
    info!("Starting Audetic service");

    let config = Config::load()?;

    let (tx, mut rx) = mpsc::channel::<ApiCommand>(10);
    let audio_recorder = Arc::new(Mutex::new(AudioStreamManager::new()?));
    let stream_hub = Arc::new(StreamHub::new());

    let text_io = TextIoService::new(
        Some(&config.wayland.input_method),
        config.behavior.preserve_clipboard,
    )?;
    let indicator =
        Indicator::from_config(&config.ui).with_audio_feedback(config.behavior.audio_feedback);
    let behavior = BehaviorOptions {
        auto_paste: config.behavior.auto_paste,
        delete_audio_files: config.behavior.delete_audio_files,
        append_newline: config.behavior.append_newline,
    };
    let ducking = AudioDuckingController::new(
        config.behavior.audio_ducking,
        config.behavior.ducking_level_percent,
    );

    let status_handle = RecordingStatusHandle::default();
    let recording_machine = if config.streaming.enabled {
        None
    } else {
        let whisper = build_transcriber(&config)?;
        let transcription_service = Arc::new(TranscriptionService::new(whisper)?);
        Some(RecordingMachine::new(
            audio_recorder.clone(),
            transcription_service,
            indicator.clone(),
            text_io.clone(),
            behavior,
            status_handle.clone(),
        ))
    };

    let streaming_machine = if config.streaming.enabled {
        info!(
            "Streaming mode enabled with provider={} model={}",
            config.streaming.provider, config.streaming.model
        );

        Some(StreamingMachine::new(
            audio_recorder.clone(),
            indicator.clone(),
            text_io.clone(),
            behavior,
            status_handle.clone(),
            stream_hub.clone(),
            config.streaming.clone(),
        ))
    } else {
        None
    };

    let api_server = ApiServer::new(tx, status_handle.clone(), &config, Some(stream_hub.clone()));
    tokio::spawn(async move {
        if let Err(e) = api_server.start().await {
            error!("API server failed: {}", e);
        }
    });

    spawn_update_manager();

    info!("Audetic is ready!");
    info!("Add this to your Hyprland config:");
    info!("bindd = SUPER, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/toggle");
    info!("Or test manually: curl -X POST http://127.0.0.1:3737/toggle");

    while let Some(command) = rx.recv().await {
        match command {
            ApiCommand::ToggleRecording(job_options) => {
                handle_toggle_result(
                    "toggle",
                    if let Some(machine) = streaming_machine.as_ref() {
                        machine.toggle(job_options).await
                    } else {
                        recording_machine
                            .as_ref()
                            .expect("recording machine should exist when streaming is disabled")
                            .toggle(job_options)
                            .await
                    },
                    &ducking,
                )
                .await;
            }
            ApiCommand::StartRecording(job_options) => {
                handle_toggle_result(
                    "start",
                    if let Some(machine) = streaming_machine.as_ref() {
                        machine.start(job_options).await
                    } else {
                        recording_machine
                            .as_ref()
                            .expect("recording machine should exist when streaming is disabled")
                            .start(job_options)
                            .await
                    },
                    &ducking,
                )
                .await;
            }
            ApiCommand::StopRecording(job_options) => {
                handle_toggle_result(
                    "stop",
                    if let Some(machine) = streaming_machine.as_ref() {
                        machine.stop(job_options).await
                    } else {
                        recording_machine
                            .as_ref()
                            .expect("recording machine should exist when streaming is disabled")
                            .stop(job_options)
                            .await
                    },
                    &ducking,
                )
                .await;
            }
        }
    }

    ducking.restore().await;

    Ok(())
}

fn build_transcriber(config: &Config) -> Result<Transcriber> {
    let provider = config
        .whisper
        .provider
        .as_deref()
        .ok_or_else(|| anyhow!("No transcription provider configured. Set [whisper].provider in ~/.config/audetic/config.toml"))?;

    let provider_config = ProviderConfig {
        model: config.whisper.model.clone(),
        model_path: config.whisper.model_path.clone(),
        language: config.whisper.language.clone(),
        command_path: config.whisper.command_path.clone(),
        api_endpoint: config.whisper.api_endpoint.clone(),
        api_key: config.whisper.api_key.clone(),
    };

    Transcriber::with_provider(provider, provider_config)
}

fn spawn_update_manager() {
    match UpdateConfig::detect(None)
        .and_then(UpdateEngine::new)
        .map(|engine| engine.spawn_background(None))
    {
        Ok(Some(_handle)) => info!("Auto-update manager running in background"),
        Ok(None) => info!("Auto-update manager not started (disabled or unsupported)"),
        Err(err) => warn!("Failed to initialize auto-update manager: {err:?}"),
    }
}

async fn handle_toggle_result(
    action: &str,
    toggle_result: Result<ToggleResult>,
    ducking: &AudioDuckingController,
) {
    match toggle_result {
        Ok(ToggleResult {
            phase: RecordingPhase::Recording,
            job_id,
        }) => {
            ducking.activate().await;
            info!("Recording started via {} with job_id={:?}", action, job_id);
        }
        Ok(ToggleResult {
            phase: RecordingPhase::Processing,
            job_id,
        }) => {
            ducking.restore().await;
            info!(
                "Recording moved to processing via {} for job_id={:?}",
                action, job_id
            );
        }
        Ok(ToggleResult { phase, job_id }) => {
            if phase != RecordingPhase::Recording {
                ducking.restore().await;
            }
            info!(
                "RecordingMachine action={} phase={:?} job_id={:?}",
                action, phase, job_id
            );
        }
        Err(e) => {
            ducking.restore().await;
            error!("Failed recording action {}: {}", action, e);
        }
    }
}
