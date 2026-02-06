use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

fn service_is_running() -> bool {
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_millis(400))
        .timeout(Duration::from_millis(700))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };

    client
        .get("http://127.0.0.1:3737/version")
        .send()
        .map(|response| response.status().is_success())
        .unwrap_or(false)
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

fn audetic_service_command() -> PathBuf {
    resolve_sibling_binary("audetic").unwrap_or_else(|| PathBuf::from("audetic"))
}

fn audetic_overlay_command() -> PathBuf {
    resolve_sibling_binary("audetic-overlay").unwrap_or_else(|| PathBuf::from("audetic-overlay"))
}

fn audetic_tray_command() -> PathBuf {
    resolve_sibling_binary("audetic-tray").unwrap_or_else(|| PathBuf::from("audetic-tray"))
}

fn start_service_detached() -> Result<()> {
    Command::new(audetic_service_command())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to spawn audetic service")?;

    Ok(())
}

fn start_tray_detached() {
    let _ = Command::new(audetic_tray_command())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn main() -> Result<()> {
    if !service_is_running() {
        start_service_detached()?;
        thread::sleep(Duration::from_millis(600));
    }

    start_tray_detached();

    let status = Command::new(audetic_overlay_command())
        .status()
        .context("Failed to launch audetic-overlay")?;

    if !status.success() {
        anyhow::bail!("audetic-overlay exited with status {}", status);
    }

    Ok(())
}
