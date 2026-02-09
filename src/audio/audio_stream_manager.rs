#![allow(clippy::arc_with_non_send_sync)]

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use hound::{WavSpec, WavWriter};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info};

use crate::audio::select_input_device_any_host;

/// State of the audio recording session
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordingState {
    Idle,
    Recording,
    Stopping,
}

/// Manages the lifecycle of audio streams and recordings
pub struct AudioStreamManager {
    device: cpal::Device,
    config: cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    sample_rate_hz: u32,
    channels: usize,
    samples: Arc<Mutex<Vec<f32>>>,
    active_stream: Arc<Mutex<Option<cpal::Stream>>>,
    state: Arc<Mutex<RecordingState>>,
}

impl AudioStreamManager {
    /// Create a new audio stream manager
    pub fn new() -> Result<Self> {
        let device = select_input_device_any_host().context("No input device available")?;

        info!("Using audio device: {}", device.name()?);

        let input_config = device.default_input_config()?;
        let config = input_config.config();
        let sample_rate_hz = config.sample_rate.0;
        let channels = config.channels as usize;

        Ok(Self {
            device,
            config,
            sample_format: input_config.sample_format(),
            sample_rate_hz,
            channels,
            samples: Arc::new(Mutex::new(Vec::new())),
            active_stream: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(RecordingState::Idle)),
        })
    }

    /// Start recording audio, properly managing stream lifecycle
    pub async fn start_recording(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();

        match *state {
            RecordingState::Recording => {
                return Err(anyhow::anyhow!("Recording already in progress"));
            }
            RecordingState::Stopping => {
                return Err(anyhow::anyhow!("Previous recording still stopping"));
            }
            RecordingState::Idle => {}
        }

        // Stop any existing stream before starting new one
        self.cleanup_stream();

        // Clear samples buffer for new recording
        {
            let mut samples = self.samples.lock().unwrap();
            samples.clear();
            samples.shrink_to_fit(); // Free memory from previous recordings
        }

        debug!("Creating new audio stream");
        debug!(
            "Recording stream config: format={:?} rate={}Hz channels={}",
            self.sample_format, self.sample_rate_hz, self.channels
        );

        let err_fn = |err| error!("Audio stream error: {}", err);

        let stream = match self.sample_format {
            cpal::SampleFormat::F32 => {
                let samples_clone = self.samples.clone();
                let channels = self.channels;
                self.device.build_input_stream(
                    &self.config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        if let Ok(mut samples) = samples_clone.lock() {
                            extend_mono_from_f32(data, channels, &mut samples);
                        }
                    },
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::I16 => {
                let samples_clone = self.samples.clone();
                let channels = self.channels;
                self.device.build_input_stream(
                    &self.config,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        if let Ok(mut samples) = samples_clone.lock() {
                            extend_mono_from_i16(data, channels, &mut samples);
                        }
                    },
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::U16 => {
                let samples_clone = self.samples.clone();
                let channels = self.channels;
                self.device.build_input_stream(
                    &self.config,
                    move |data: &[u16], _: &cpal::InputCallbackInfo| {
                        if let Ok(mut samples) = samples_clone.lock() {
                            extend_mono_from_u16(data, channels, &mut samples);
                        }
                    },
                    err_fn,
                    None,
                )?
            }
            sample_format => {
                return Err(anyhow::anyhow!(
                    "Unsupported sample format for recording: {sample_format:?}"
                ));
            }
        };

        stream.play()?;
        info!("Started audio recording");

        // Store stream for proper cleanup
        *self.active_stream.lock().unwrap() = Some(stream);
        *state = RecordingState::Recording;

        Ok(())
    }

    /// Stop recording and save audio to file
    pub async fn stop_recording(&self, output_path: PathBuf) -> Result<PathBuf> {
        let mut state = self.state.lock().unwrap();

        match *state {
            RecordingState::Idle => {
                return Err(anyhow::anyhow!("No recording in progress"));
            }
            RecordingState::Stopping => {
                return Err(anyhow::anyhow!("Recording already stopping"));
            }
            RecordingState::Recording => {}
        }

        *state = RecordingState::Stopping;
        drop(state); // Release lock before cleanup

        // Stop and cleanup stream
        self.cleanup_stream();

        // Extract samples
        let samples = {
            let samples_guard = self.samples.lock().unwrap();
            samples_guard.clone()
        };

        if samples.is_empty() {
            *self.state.lock().unwrap() = RecordingState::Idle;
            return Err(anyhow::anyhow!("No audio samples recorded"));
        }

        info!("Stopping recording, {} samples captured", samples.len());

        // Write WAV file
        let spec = WavSpec {
            channels: 1,
            sample_rate: self.sample_rate_hz,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };

        let mut writer = WavWriter::create(&output_path, spec)?;
        for sample in samples {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;

        // Clear samples and reset state
        {
            let mut samples = self.samples.lock().unwrap();
            samples.clear();
            samples.shrink_to_fit();
        }

        *self.state.lock().unwrap() = RecordingState::Idle;

        info!("Audio saved to: {:?}", output_path);
        Ok(output_path)
    }

    /// Drain currently buffered samples without stopping the active stream.
    pub fn drain_samples(&self) -> Vec<f32> {
        let mut samples = self.samples.lock().unwrap();
        std::mem::take(&mut *samples)
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// Stop recording and discard captured audio (used for streaming mode).
    pub async fn stop_recording_discard(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();

        match *state {
            RecordingState::Idle => {
                return Ok(());
            }
            RecordingState::Stopping => {
                return Ok(());
            }
            RecordingState::Recording => {}
        }

        *state = RecordingState::Stopping;
        drop(state);

        self.cleanup_stream();

        {
            let mut samples = self.samples.lock().unwrap();
            samples.clear();
            samples.shrink_to_fit();
        }

        *self.state.lock().unwrap() = RecordingState::Idle;
        Ok(())
    }

    /// Cleanup any active stream
    fn cleanup_stream(&self) {
        let mut active_stream = self.active_stream.lock().unwrap();
        if let Some(stream) = active_stream.take() {
            debug!("Cleaning up audio stream");
            // Stream is automatically stopped when dropped
            drop(stream);
        }
    }
}

impl Drop for AudioStreamManager {
    fn drop(&mut self) {
        debug!("Dropping AudioStreamManager, cleaning up resources");
        self.cleanup_stream();
    }
}

fn extend_mono_from_f32(input: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(input);
        return;
    }

    for frame in input.chunks(channels) {
        let sum: f32 = frame.iter().copied().sum();
        out.push(sum / frame.len() as f32);
    }
}

fn extend_mono_from_i16(input: &[i16], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend(input.iter().map(|sample| *sample as f32 / i16::MAX as f32));
        return;
    }

    for frame in input.chunks(channels) {
        let sum: f32 = frame
            .iter()
            .map(|sample| *sample as f32 / i16::MAX as f32)
            .sum();
        out.push(sum / frame.len() as f32);
    }
}

fn extend_mono_from_u16(input: &[u16], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend(
            input
                .iter()
                .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0),
        );
        return;
    }

    for frame in input.chunks(channels) {
        let sum: f32 = frame
            .iter()
            .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0)
            .sum();
        out.push(sum / frame.len() as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_ci() -> bool {
        std::env::var("CI").is_ok()
            || std::env::var("GITHUB_ACTIONS").is_ok()
            || std::env::var("GITLAB_CI").is_ok()
            || std::env::var("TRAVIS").is_ok()
    }

    #[tokio::test]
    async fn test_audio_stream_manager_creation() {
        if is_ci() {
            // Skip audio tests in CI - no audio devices available
            return;
        }

        // This test may fail in CI without audio devices
        let _manager = AudioStreamManager::new();
    }
}
