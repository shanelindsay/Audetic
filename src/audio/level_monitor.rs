use std::sync::Arc;
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, StreamTrait};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::audio::{
    select_input_device_any_host_with_preference, RecordingPhase, RecordingStatusHandle,
};
use crate::streaming::StreamHub;

const METER_TICK_MS: u64 = 33;
const CLIP_THRESHOLD: f32 = 0.995;
const CLIP_RATIO_THRESHOLD: f32 = 0.005;

pub fn spawn_idle_level_monitor(
    status: RecordingStatusHandle,
    hub: Arc<StreamHub>,
    preferred_input_device: Option<String>,
    input_gain_percent: u16,
) {
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
        if let Err(err) =
            run_input_level_stream(tx, preferred_input_device.as_deref(), input_gain_percent)
        {
            warn!("Idle mic level monitor failed: {err}");
        }
    });
}

fn run_input_level_stream(
    level_tx: mpsc::Sender<(f32, f32, bool)>,
    preferred_input_device: Option<&str>,
    input_gain_percent: u16,
) -> anyhow::Result<()> {
    let device = select_input_device_any_host_with_preference(preferred_input_device)?;
    let config = device.default_input_config()?;
    let stream_config = config.config();
    let input_gain = (input_gain_percent as f32 / 100.0).clamp(0.25, 3.0);

    info!(
        "Idle mic level monitor started on device: {} (input_gain={}%)",
        device.name().unwrap_or_else(|_| "<unknown>".to_string()),
        (input_gain * 100.0).round() as u16
    );

    let err_fn = |err| warn!("Idle mic level monitor stream error: {err}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            {
                let level_tx = level_tx.clone();
                let input_gain = input_gain;
                move |data: &[f32], _| {
                    let level = measure_level(data, input_gain);
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
                let input_gain = input_gain;
                move |data: &[i16], _| {
                    let level = measure_level_i16(data, input_gain);
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
                let input_gain = input_gain;
                move |data: &[u16], _| {
                    let level = measure_level_u16(data, input_gain);
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

fn measure_level(samples: &[f32], input_gain: f32) -> (f32, f32, bool) {
    if samples.is_empty() {
        return (-90.0, -90.0, false);
    }

    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f32;
    let mut clip_count = 0usize;
    for &sample in samples {
        let adjusted = (sample * input_gain).clamp(-1.0, 1.0);
        let abs = adjusted.abs();
        if abs > peak {
            peak = abs;
        }
        if abs >= CLIP_THRESHOLD {
            clip_count += 1;
        }
        sum_sq += (adjusted as f64) * (adjusted as f64);
    }

    let rms = (sum_sq / samples.len() as f64).sqrt() as f32;
    let rms_dbfs = 20.0 * rms.max(1e-9).log10();
    let peak_dbfs = 20.0 * peak.max(1e-9).log10();
    let clip_ratio = clip_count as f32 / samples.len() as f32;
    let clipping = peak >= CLIP_THRESHOLD && clip_ratio >= CLIP_RATIO_THRESHOLD;

    (rms_dbfs, peak_dbfs, clipping)
}

fn measure_level_i16(samples: &[i16], input_gain: f32) -> (f32, f32, bool) {
    if samples.is_empty() {
        return (-90.0, -90.0, false);
    }

    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f32;
    let mut clip_count = 0usize;
    for &sample in samples {
        let value = (sample as f32 / i16::MAX as f32 * input_gain).clamp(-1.0, 1.0);
        let abs = value.abs();
        if abs > peak {
            peak = abs;
        }
        if abs >= CLIP_THRESHOLD {
            clip_count += 1;
        }
        sum_sq += (value as f64) * (value as f64);
    }

    let rms = (sum_sq / samples.len() as f64).sqrt() as f32;
    let rms_dbfs = 20.0 * rms.max(1e-9).log10();
    let peak_dbfs = 20.0 * peak.max(1e-9).log10();
    let clip_ratio = clip_count as f32 / samples.len() as f32;
    let clipping = peak >= CLIP_THRESHOLD && clip_ratio >= CLIP_RATIO_THRESHOLD;

    (rms_dbfs, peak_dbfs, clipping)
}

fn measure_level_u16(samples: &[u16], input_gain: f32) -> (f32, f32, bool) {
    if samples.is_empty() {
        return (-90.0, -90.0, false);
    }

    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f32;
    let mut clip_count = 0usize;
    for &sample in samples {
        let value = (((sample as f32 / u16::MAX as f32) * 2.0 - 1.0) * input_gain).clamp(-1.0, 1.0);
        let abs = value.abs();
        if abs > peak {
            peak = abs;
        }
        if abs >= CLIP_THRESHOLD {
            clip_count += 1;
        }
        sum_sq += (value as f64) * (value as f64);
    }

    let rms = (sum_sq / samples.len() as f64).sqrt() as f32;
    let rms_dbfs = 20.0 * rms.max(1e-9).log10();
    let peak_dbfs = 20.0 * peak.max(1e-9).log10();
    let clip_ratio = clip_count as f32 / samples.len() as f32;
    let clipping = peak >= CLIP_THRESHOLD && clip_ratio >= CLIP_RATIO_THRESHOLD;

    (rms_dbfs, peak_dbfs, clipping)
}
