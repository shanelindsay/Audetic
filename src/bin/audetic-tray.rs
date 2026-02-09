use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use audetic::global;
use fs2::FileExt;
use ksni::blocking::TrayMethods;
use serde::Deserialize;

const STATUS_URL: &str = "http://127.0.0.1:3737/status";
const TOGGLE_URL: &str = "http://127.0.0.1:3737/toggle";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrayPhase {
    Idle,
    Recording,
    Processing,
    Error,
    Unknown,
}

impl TrayPhase {
    fn from_api(value: &str) -> Self {
        match value {
            "idle" => Self::Idle,
            "recording" => Self::Recording,
            "processing" => Self::Processing,
            "error" => Self::Error,
            _ => Self::Unknown,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Recording => "Recording",
            Self::Processing => "Processing",
            Self::Error => "Error",
            Self::Unknown => "Disconnected",
        }
    }

    fn icon_color(self) -> (u8, u8, u8) {
        match self {
            Self::Idle => (46, 204, 113),
            Self::Recording => (231, 76, 60),
            Self::Processing => (243, 156, 18),
            Self::Error => (192, 57, 43),
            Self::Unknown => (127, 140, 141),
        }
    }
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
    phase: String,
    last_error: Option<String>,
}

struct AudeticTray {
    phase: TrayPhase,
    last_error: Option<String>,
    audetic_command: PathBuf,
    overlay_command: PathBuf,
}

impl AudeticTray {
    fn set_error(&mut self, message: impl Into<String>) {
        self.last_error = Some(message.into());
    }

    fn clear_error(&mut self) {
        self.last_error = None;
    }

    fn start_service(&mut self) -> Result<()> {
        Command::new(&self.audetic_command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to spawn audetic service")?;
        Ok(())
    }

    fn service_reachable(&self) -> bool {
        fetch_status().is_some()
    }

    fn ensure_service_running(&mut self) -> bool {
        if self.service_reachable() {
            return true;
        }

        if let Err(err) = self.start_service() {
            self.set_error(err.to_string());
            return false;
        }

        for _ in 0..15 {
            thread::sleep(Duration::from_millis(120));
            if self.service_reachable() {
                self.clear_error();
                return true;
            }
        }

        self.set_error("Audetic service did not start");
        false
    }

    fn is_overlay_running(&self) -> bool {
        overlay_lock_file()
            .ok()
            .map(|file| match file.try_lock_exclusive() {
                Ok(()) => false,
                Err(err) if err.kind() == ErrorKind::WouldBlock => true,
                Err(_) => false,
            })
            .unwrap_or(false)
    }

    fn open_overlay(&self) {
        if self.is_overlay_running() {
            return;
        }
        let _ = Command::new(&self.overlay_command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }

    fn toggle_recording(&mut self) {
        let _ = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_millis(350))
            .timeout(Duration::from_secs(3))
            .build()
            .and_then(|client| client.post(TOGGLE_URL).send())
            .map(|_| self.clear_error())
            .map_err(|err| self.set_error(format!("Toggle failed: {err}")));
    }

    fn activate_primary_action(&mut self) {
        if !self.ensure_service_running() {
            return;
        }

        if !self.is_overlay_running() {
            self.open_overlay();
            return;
        }

        self.toggle_recording();
    }
}

impl ksni::Tray for AudeticTray {
    fn id(&self) -> String {
        "audetic-tray".to_string()
    }

    fn title(&self) -> String {
        format!("Audetic ({})", self.phase.label())
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![build_circle_icon(self.phase.icon_color())]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let mut description = format!("Status: {}", self.phase.label());
        if let Some(last_error) = &self.last_error {
            description.push('\n');
            description.push_str(last_error);
        }

        ksni::ToolTip {
            title: "Audetic".to_string(),
            description,
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.activate_primary_action();
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};

        vec![
            StandardItem {
                label: "Open Overlay".into(),
                activate: Box::new(|tray: &mut Self| tray.open_overlay()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Toggle Recording".into(),
                activate: Box::new(|tray: &mut Self| tray.toggle_recording()),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit Tray".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn build_circle_icon((r, g, b): (u8, u8, u8)) -> ksni::Icon {
    let size: usize = 32;
    let mut data = Vec::with_capacity(size * size * 4);
    let center = (size as f32 - 1.0) / 2.0;
    let radius = size as f32 * 0.38;
    let radius_sq = radius * radius;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let inside = dx * dx + dy * dy <= radius_sq;
            if inside {
                data.extend_from_slice(&[255, r, g, b]);
            } else {
                data.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }

    ksni::Icon {
        width: size as i32,
        height: size as i32,
        data,
    }
}

fn resolve_sibling_binary(name: &str) -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let sibling = current.with_file_name(name);
    if sibling.exists() {
        Some(sibling)
    } else {
        None
    }
}

fn overlay_command() -> PathBuf {
    resolve_sibling_binary("audetic-overlay").unwrap_or_else(|| PathBuf::from("audetic-overlay"))
}

fn audetic_command() -> PathBuf {
    resolve_sibling_binary("audetic").unwrap_or_else(|| PathBuf::from("audetic"))
}

fn overlay_lock_file() -> Result<File> {
    let data_dir = global::data_dir()?;
    fs::create_dir_all(&data_dir).context("Failed to create Audetic data directory")?;
    let lock_path = data_dir.join("overlay.lock");
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .context("Failed to open overlay lock file")
}

fn fetch_status() -> Option<StatusResponse> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_millis(350))
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let response = client.get(STATUS_URL).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<StatusResponse>().ok()
}

fn fetch_phase() -> (TrayPhase, Option<String>) {
    fetch_status()
        .map(|payload| {
            (
                TrayPhase::from_api(payload.phase.as_str()),
                payload.last_error,
            )
        })
        .unwrap_or((TrayPhase::Unknown, None))
}

fn try_acquire_instance_lock() -> Result<Option<File>> {
    let data_dir = global::data_dir()?;
    fs::create_dir_all(&data_dir).context("Failed to create Audetic data directory")?;
    let lock_path = data_dir.join("tray.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .context("Failed to open tray lock file")?;

    match lock_file.try_lock_exclusive() {
        Ok(()) => Ok(Some(lock_file)),
        Err(err) if err.kind() == ErrorKind::WouldBlock => Ok(None),
        Err(err) => Err(err).context("Failed to lock tray instance file"),
    }
}

fn run() -> Result<()> {
    let _instance_lock = match try_acquire_instance_lock()? {
        Some(lock) => lock,
        None => return Ok(()),
    };

    let tray = AudeticTray {
        phase: TrayPhase::Unknown,
        last_error: None,
        audetic_command: audetic_command(),
        overlay_command: overlay_command(),
    };

    let handle = tray.spawn().context("Failed to start tray icon")?;

    thread::spawn(move || loop {
        let (phase, last_error) = fetch_phase();
        handle.update(|tray: &mut AudeticTray| {
            tray.phase = phase;
            tray.last_error = last_error.clone();
        });
        thread::sleep(Duration::from_millis(420));
    });

    loop {
        thread::park();
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("audetic-tray: {err}");
        std::process::exit(1);
    }
}
