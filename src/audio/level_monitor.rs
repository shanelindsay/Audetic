use std::sync::Arc;
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::audio::{RecordingPhase, RecordingStatusHandle};
use crate::streaming::StreamHub;

const METER_TICK_MS: u64 = 33;

pub fn spawn_idle_level_monitor(status: RecordingStatusHandle, hub: Arc<StreamHub>) {
    let (tx, mut rx) = mpsc::channel::<(f32, f32, bool)>(32);

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(METER_TICK_MS));
        let mut latest_level: Option<(f32, f32, bool)> = None;

        loop {
            tokio::select! {
                maybe = rx.recv() => {
                    match maybe {
                        Some(level) => latest_level = Some(level),
                        None => break,
                    }
                }
                _ = ticker.tick() => {
                    while let Ok(level) = rx.try_recv() {
                        latest_level = Some(level);
                    }

                    let Some((rms_dbfs, peak_dbfs, clipping)) = latest_level else {
                        continue;
                    };

                    let phase = status.get().await.phase;
                    if matches!(phase, RecordingPhase::Recording | RecordingPhase::Processing) {
                        continue;
                    }

                    let _ = hub.audio_level(None, rms_dbfs, peak_dbfs, clipping).await;
                }
            }
        }
    });

    thread::spawn(move || {
        if let Err(err) = run_input_level_stream(tx) {
            warn!("Idle mic level monitor failed: {err}");
        }
    });
}

fn run_input_level_stream(level_tx: mpsc::Sender<(f32, f32, bool)>) -> anyhow::Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("No default input device available"))?;
    let config = device.default_input_config()?;
    let stream_config = config.config();

    info!(
        "Idle mic level monitor started on device: {}",
        device.name().unwrap_or_else(|_| "<unknown>".to_string())
    );

    let err_fn = |err| warn!("Idle mic level monitor stream error: {err}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            {
                let level_tx = level_tx.clone();
                move |data: &[f32], _| {
                    let level = measure_level(data);
                    let _ = level_tx.try_send(level);
                }
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            {
                let level_tx = level_tx.clone();
                move |data: &[i16], _| {
                    let level = measure_level_i16(data);
                    let _ = level_tx.try_send(level);
                }
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            {
                let level_tx = level_tx.clone();
                move |data: &[u16], _| {
                    let level = measure_level_u16(data);
                    let _ = level_tx.try_send(level);
                }
            },
            err_fn,
            None,
        )?,
        sample_format => {
            return Err(anyhow::anyhow!(
                "Unsupported sample format for idle monitor: {sample_format:?}"
            ));
        }
    };

    stream.play()?;

    // Keep the stream alive for process lifetime.
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

fn measure_level(samples: &[f32]) -> (f32, f32, bool) {
    if samples.is_empty() {
        return (-90.0, -90.0, false);
    }

    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f32;
    for &sample in samples {
        let abs = sample.abs();
        if abs > peak {
            peak = abs;
        }
        sum_sq += (sample as f64) * (sample as f64);
    }

    let rms = (sum_sq / samples.len() as f64).sqrt() as f32;
    let rms_dbfs = 20.0 * rms.max(1e-9).log10();
    let peak_dbfs = 20.0 * peak.max(1e-9).log10();
    let clipping = peak >= 0.99;

    (rms_dbfs, peak_dbfs, clipping)
}

fn measure_level_i16(samples: &[i16]) -> (f32, f32, bool) {
    if samples.is_empty() {
        return (-90.0, -90.0, false);
    }

    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f32;
    for &sample in samples {
        let value = sample as f32 / i16::MAX as f32;
        let abs = value.abs();
        if abs > peak {
            peak = abs;
        }
        sum_sq += (value as f64) * (value as f64);
    }

    let rms = (sum_sq / samples.len() as f64).sqrt() as f32;
    let rms_dbfs = 20.0 * rms.max(1e-9).log10();
    let peak_dbfs = 20.0 * peak.max(1e-9).log10();
    let clipping = peak >= 0.99;

    (rms_dbfs, peak_dbfs, clipping)
}

fn measure_level_u16(samples: &[u16]) -> (f32, f32, bool) {
    if samples.is_empty() {
        return (-90.0, -90.0, false);
    }

    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f32;
    for &sample in samples {
        let value = (sample as f32 / u16::MAX as f32) * 2.0 - 1.0;
        let abs = value.abs();
        if abs > peak {
            peak = abs;
        }
        sum_sq += (value as f64) * (value as f64);
    }

    let rms = (sum_sq / samples.len() as f64).sqrt() as f32;
    let rms_dbfs = 20.0 * rms.max(1e-9).log10();
    let peak_dbfs = 20.0 * peak.max(1e-9).log10();
    let clipping = peak >= 0.99;

    (rms_dbfs, peak_dbfs, clipping)
}
