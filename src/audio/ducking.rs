use std::process::Command;

use anyhow::{anyhow, Context, Result};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy)]
struct DuckState {
    active: bool,
    previous_volume: f32,
    previous_muted: bool,
}

#[derive(Debug)]
pub struct AudioDuckingController {
    enabled: bool,
    duck_volume: f32,
    state: Mutex<Option<DuckState>>,
}

impl AudioDuckingController {
    pub fn new(enabled: bool, duck_level_percent: u8) -> Self {
        let clamped_percent = duck_level_percent.clamp(5, 95);
        let duck_volume = (clamped_percent as f32) / 100.0;
        Self {
            enabled,
            duck_volume,
            state: Mutex::new(None),
        }
    }

    pub async fn activate(&self) {
        if !self.enabled {
            return;
        }

        {
            let guard = self.state.lock().await;
            if guard.as_ref().is_some_and(|state| state.active) {
                return;
            }
        }

        match tokio::task::spawn_blocking({
            let target = self.duck_volume;
            move || duck_default_sink(target)
        })
        .await
        {
            Ok(Ok(state)) => {
                let mut guard = self.state.lock().await;
                *guard = Some(state);
                info!(
                    "Audio ducking enabled: {:.0}% -> {:.0}%",
                    state.previous_volume * 100.0,
                    self.duck_volume * 100.0
                );
            }
            Ok(Err(err)) => {
                warn!("Failed to enable audio ducking: {err}");
            }
            Err(err) => {
                warn!("Audio ducking worker failed: {err}");
            }
        }
    }

    pub async fn restore(&self) {
        if !self.enabled {
            return;
        }

        let previous = {
            let mut guard = self.state.lock().await;
            guard.take()
        };

        let Some(previous) = previous else {
            return;
        };

        match tokio::task::spawn_blocking(move || restore_default_sink(previous)).await {
            Ok(Ok(())) => {
                debug!("Audio ducking restored");
            }
            Ok(Err(err)) => {
                warn!("Failed to restore audio ducking: {err}");
            }
            Err(err) => {
                warn!("Audio ducking restore worker failed: {err}");
            }
        }
    }
}

fn duck_default_sink(target_volume: f32) -> Result<DuckState> {
    let (previous_volume, previous_muted) = get_default_sink_state()?;
    if (previous_volume - target_volume).abs() > 0.005 {
        set_default_sink_volume(target_volume)?;
    }

    Ok(DuckState {
        active: true,
        previous_volume,
        previous_muted,
    })
}

fn restore_default_sink(state: DuckState) -> Result<()> {
    let (current_volume, current_muted) = get_default_sink_state()?;

    if (current_volume - state.previous_volume).abs() > 0.005 {
        set_default_sink_volume(state.previous_volume)?;
    }

    if current_muted != state.previous_muted {
        if state.previous_muted {
            run_wpctl(["set-mute", "@DEFAULT_AUDIO_SINK@", "1"])?;
        } else {
            run_wpctl(["set-mute", "@DEFAULT_AUDIO_SINK@", "0"])?;
        }
    }

    Ok(())
}

fn get_default_sink_state() -> Result<(f32, bool)> {
    let output = run_wpctl(["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
    parse_wpctl_volume(&output).ok_or_else(|| anyhow!("Unexpected wpctl output: {output}"))
}

fn set_default_sink_volume(volume: f32) -> Result<()> {
    let clamped = volume.clamp(0.0, 1.5);
    run_wpctl([
        "set-volume",
        "@DEFAULT_AUDIO_SINK@",
        &format!("{clamped:.4}"),
    ])?;
    Ok(())
}

fn run_wpctl<const N: usize>(args: [&str; N]) -> Result<String> {
    let output = Command::new("wpctl")
        .args(args)
        .output()
        .context("Failed to execute wpctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("wpctl command failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_wpctl_volume(output: &str) -> Option<(f32, bool)> {
    let muted = output.to_ascii_lowercase().contains("[muted]");
    for token in output.split_whitespace() {
        let cleaned = token.trim_matches(|c: char| !(c.is_ascii_digit() || c == '.'));
        if cleaned.is_empty() {
            continue;
        }
        if let Ok(value) = cleaned.parse::<f32>() {
            return Some((value, muted));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_wpctl_volume;

    #[test]
    fn parse_wpctl_volume_unmuted() {
        let parsed = parse_wpctl_volume("Volume: 0.81");
        assert_eq!(parsed, Some((0.81, false)));
    }

    #[test]
    fn parse_wpctl_volume_muted() {
        let parsed = parse_wpctl_volume("Volume: 0.33 [MUTED]");
        assert_eq!(parsed, Some((0.33, true)));
    }

    #[test]
    fn parse_wpctl_volume_invalid() {
        let parsed = parse_wpctl_volume("no-number");
        assert!(parsed.is_none());
    }
}
