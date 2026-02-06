use crate::global;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub whisper: WhisperConfig,
    pub ui: UiConfig,
    pub wayland: WaylandConfig,
    pub behavior: BehaviorConfig,
    pub streaming: StreamingConfig,
    pub overlay: OverlayConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WhisperConfig {
    pub model: Option<String>,
    pub language: Option<String>,
    pub command_path: Option<String>,
    pub model_path: Option<String>,
    pub api_endpoint: Option<String>,
    pub provider: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub notification_color: String,
    pub waybar: WaybarConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WaybarConfig {
    pub idle_text: String,
    pub recording_text: String,
    pub idle_tooltip: String,
    pub recording_tooltip: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WaylandConfig {
    pub input_method: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BehaviorConfig {
    pub auto_paste: bool,
    pub preserve_clipboard: bool,
    pub delete_audio_files: bool,
    #[serde(default = "default_audio_feedback")]
    pub audio_feedback: bool,
    #[serde(default)]
    pub append_newline: bool,
    #[serde(default)]
    pub audio_ducking: bool,
    #[serde(default = "default_ducking_level_percent")]
    pub ducking_level_percent: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamingConfig {
    pub enabled: bool,
    pub provider: String,
    pub api_key: Option<String>,
    pub model: String,
    pub api_base_url: String,
    pub sample_rate_hz: u32,
    pub chunk_ms: u32,
    pub silence_timeout_ms: u64,
    pub commit_target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub url: String,
    pub always_on_top: bool,
    pub width: u32,
    pub height: u32,
    pub opacity: f32,
    pub show_meter: bool,
}

fn default_audio_feedback() -> bool {
    true
}

fn default_ducking_level_percent() -> u8 {
    35
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            model: Some("base".to_string()),
            language: Some("en".to_string()),
            command_path: None,
            model_path: None,
            api_endpoint: None,
            provider: Some("audetic-api".to_string()),
            api_key: None,
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            notification_color: "rgb(ff1744)".to_string(),
            waybar: WaybarConfig::default(),
        }
    }
}

impl Default for WaybarConfig {
    fn default() -> Self {
        Self {
            idle_text: "󰑊".to_string(),      // Nerd Font circle with dot (idle)
            recording_text: "󰻃".to_string(), // Nerd Font record button (recording)
            idle_tooltip: "Press Super+R to record".to_string(),
            recording_tooltip: "Recording... Press Super+R to stop".to_string(),
        }
    }
}

impl Default for WaylandConfig {
    fn default() -> Self {
        Self {
            input_method: "wtype".to_string(),
        }
    }
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            auto_paste: true,
            preserve_clipboard: false,
            delete_audio_files: true,
            audio_feedback: true,
            append_newline: false,
            audio_ducking: false,
            ducking_level_percent: default_ducking_level_percent(),
        }
    }
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "mistral_realtime".to_string(),
            api_key: None,
            model: "voxtral-mini-transcribe-realtime-2602".to_string(),
            api_base_url: "https://api.mistral.ai".to_string(),
            sample_rate_hz: 16_000,
            chunk_ms: 20,
            silence_timeout_ms: 700,
            commit_target: "clipboard".to_string(),
        }
    }
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            url: "http://127.0.0.1:3737/stream/events".to_string(),
            always_on_top: true,
            width: 560,
            height: 220,
            opacity: 0.94,
            show_meter: true,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;
        if !config_path.exists() {
            info!(
                "Config file not found, creating default at {:?}",
                config_path
            );
            let config = Self::default();
            config.save()?;
            return Ok(config);
        }

        let content =
            std::fs::read_to_string(&config_path).context("Failed to read config file")?;

        let config: Self = toml::from_str(&content).context("Failed to parse config file")?;

        info!("Loaded config from {:?}", config_path);
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create config directory")?;
        }

        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;

        std::fs::write(&config_path, content).context("Failed to write config file")?;

        Ok(())
    }

    fn config_path() -> Result<PathBuf> {
        global::config_file()
    }
}
