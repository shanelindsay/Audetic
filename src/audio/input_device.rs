use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};

fn is_virtual_input_name(name: &str) -> bool {
    let normalized = name.trim().to_ascii_lowercase();
    normalized.is_empty()
        || normalized == "default"
        || normalized == "pulse"
        || normalized == "pipewire"
        || normalized.contains("monitor")
}

pub fn select_input_device(host: &cpal::Host) -> Result<cpal::Device> {
    select_input_device_on_host(host, true)
}

pub fn select_input_device_any_host() -> Result<cpal::Device> {
    let default_host = cpal::default_host();
    if let Ok(device) = select_input_device_on_host(&default_host, true) {
        return Ok(device);
    }

    let mut virtual_fallback: Option<cpal::Device> =
        select_input_device_on_host(&default_host, false).ok();

    for host_id in cpal::available_hosts() {
        let host = match cpal::host_from_id(host_id) {
            Ok(host) => host,
            Err(_) => continue,
        };

        if let Ok(device) = select_input_device_on_host(&host, true) {
            return Ok(device);
        }

        if virtual_fallback.is_none() {
            virtual_fallback = select_input_device_on_host(&host, false).ok();
        }
    }

    virtual_fallback.ok_or_else(|| anyhow!("No input device available on any host"))
}

fn select_input_device_on_host(
    host: &cpal::Host,
    require_real_device_name: bool,
) -> Result<cpal::Device> {
    let mut fallback: Option<cpal::Device> = None;

    if let Some(default_device) = host.default_input_device() {
        if default_device.default_input_config().is_ok() {
            let preferred = default_device
                .name()
                .map(|name| !is_virtual_input_name(&name))
                .unwrap_or(true);
            if preferred {
                return Ok(default_device);
            }
            if !require_real_device_name {
                fallback = Some(default_device);
            }
        }
    }

    let devices = host
        .input_devices()
        .context("Failed to enumerate input devices")?;
    for device in devices {
        if device.default_input_config().is_err() {
            continue;
        }
        let preferred = device
            .name()
            .map(|name| !is_virtual_input_name(&name))
            .unwrap_or(true);
        if preferred {
            return Ok(device);
        }
        if !require_real_device_name && fallback.is_none() {
            fallback = Some(device);
        }
    }

    fallback.ok_or_else(|| anyhow!("No input device available"))
}

pub fn preferred_input_device_name() -> Option<String> {
    let device = select_input_device_any_host().ok()?;
    let name = device.name().ok()?;
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn best_available_input_device_name() -> Option<String> {
    if let Some(name) = preferred_input_device_name() {
        if !is_virtual_input_name(&name) {
            return Some(name);
        }
    }

    let mut fallback: Option<String> = None;

    for host_id in cpal::available_hosts() {
        let host = match cpal::host_from_id(host_id) {
            Ok(host) => host,
            Err(_) => continue,
        };
        let devices = match host.input_devices() {
            Ok(devices) => devices,
            Err(_) => continue,
        };
        for device in devices {
            if device.default_input_config().is_err() {
                continue;
            }
            let name = match device.name() {
                Ok(name) => name,
                Err(_) => continue,
            };
            let trimmed = name.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !is_virtual_input_name(trimmed) {
                return Some(trimmed.to_string());
            }
            if fallback.is_none() {
                fallback = Some(trimmed.to_string());
            }
        }
    }

    fallback
}
