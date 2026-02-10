use std::collections::BTreeSet;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};

struct DeviceCandidate {
    device: cpal::Device,
    name: String,
    is_monitor: bool,
    is_routing: bool,
    is_virtual: bool,
}

fn normalize_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn is_monitor_input_name(name: &str) -> bool {
    normalize_name(name).contains("monitor")
}

fn is_routing_input_name(name: &str) -> bool {
    matches!(
        normalize_name(name).as_str(),
        "default" | "default input" | "pipewire" | "pulse"
    )
}

fn is_virtual_input_name(name: &str) -> bool {
    let normalized = normalize_name(name);
    normalized.is_empty() || is_routing_input_name(name) || normalized.contains("monitor")
}

fn host_priority(host_id: cpal::HostId) -> usize {
    let name = format!("{host_id:?}").to_ascii_lowercase();
    if name.contains("pipewire") {
        0
    } else if name.contains("pulse") {
        1
    } else if name.contains("alsa") {
        3
    } else {
        2
    }
}

fn available_hosts_in_priority_order() -> Vec<cpal::Host> {
    let mut host_ids = cpal::available_hosts();
    host_ids.sort_by_key(|host_id| host_priority(*host_id));
    host_ids
        .into_iter()
        .filter_map(|host_id| cpal::host_from_id(host_id).ok())
        .collect()
}

fn enumerate_host_input_candidates(host: &cpal::Host) -> Result<Vec<DeviceCandidate>> {
    let devices = host
        .input_devices()
        .context("Failed to enumerate input devices")?;
    let mut candidates = Vec::new();

    for device in devices {
        if device.default_input_config().is_err() {
            continue;
        }

        let name = match device.name() {
            Ok(name) => {
                let trimmed = name.trim();
                if trimmed.is_empty() {
                    continue;
                }
                trimmed.to_string()
            }
            Err(_) => continue,
        };

        candidates.push(DeviceCandidate {
            device,
            is_monitor: is_monitor_input_name(&name),
            is_routing: is_routing_input_name(&name),
            is_virtual: is_virtual_input_name(&name),
            name,
        });
    }

    Ok(candidates)
}

fn preferred_match_index(candidates: &[DeviceCandidate], preferred_name: &str) -> Option<usize> {
    let preferred = normalize_name(preferred_name);
    if preferred.is_empty() {
        return None;
    }

    candidates
        .iter()
        .position(|candidate| normalize_name(&candidate.name) == preferred)
        .or_else(|| {
            candidates.iter().position(|candidate| {
                let candidate_name = normalize_name(&candidate.name);
                candidate_name.contains(&preferred) || preferred.contains(&candidate_name)
            })
        })
}

fn pick_candidate_index(candidates: &[DeviceCandidate], default_name: Option<&str>) -> Option<usize> {
    if let Some(default_name) = default_name.and_then(|name| {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }) {
        if let Some(idx) = preferred_match_index(candidates, default_name) {
            let candidate = &candidates[idx];
            if candidate.is_routing || !candidate.is_virtual {
                return Some(idx);
            }
        }
    }

    candidates
        .iter()
        .position(|candidate| candidate.is_routing && !candidate.is_monitor)
        .or_else(|| {
            candidates
                .iter()
                .position(|candidate| !candidate.is_virtual && !candidate.is_monitor)
        })
        .or_else(|| {
            candidates
                .iter()
                .position(|candidate| !candidate.is_monitor && !candidate.name.trim().is_empty())
        })
        .or_else(|| candidates.first().map(|_| 0))
}

fn select_input_device_on_host(
    host: &cpal::Host,
    preferred_name: Option<&str>,
) -> Result<cpal::Device> {
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok())
        .map(|name| name.trim().to_string());

    let mut candidates = enumerate_host_input_candidates(host)?;
    if let Some(preferred) = preferred_name.and_then(|name| {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }) {
        let idx = preferred_match_index(&candidates, preferred).ok_or_else(|| {
            anyhow!("Preferred input device '{preferred}' not found on this host")
        })?;
        return Ok(candidates.swap_remove(idx).device);
    }

    let idx = pick_candidate_index(&candidates, default_name.as_deref())
        .ok_or_else(|| anyhow!("No input device available"))?;

    Ok(candidates.swap_remove(idx).device)
}

pub fn select_input_device(host: &cpal::Host) -> Result<cpal::Device> {
    select_input_device_on_host(host, None)
}

pub fn select_input_device_any_host() -> Result<cpal::Device> {
    select_input_device_any_host_with_preference(None)
}

pub fn select_input_device_any_host_with_preference(
    preferred_name: Option<&str>,
) -> Result<cpal::Device> {
    let mut fallback: Option<cpal::Device> = None;
    let preferred_name = preferred_name.and_then(|name| {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });

    if let Some(preferred) = preferred_name {
        for host in available_hosts_in_priority_order() {
            if let Ok(device) = select_input_device_on_host(&host, Some(preferred)) {
                return Ok(device);
            }
        }
    }

    for host in available_hosts_in_priority_order() {
        if let Ok(device) = select_input_device_on_host(&host, None) {
            return Ok(device);
        }

        if fallback.is_none() {
            fallback = host
                .default_input_device()
                .filter(|device| device.default_input_config().is_ok());
        }
    }

    fallback.ok_or_else(|| anyhow!("No input device available on any host"))
}

pub fn selected_input_device_name(preferred_name: Option<&str>) -> Option<String> {
    let device = select_input_device_any_host_with_preference(preferred_name).ok()?;
    let name = device.name().ok()?;
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn preferred_input_device_name() -> Option<String> {
    selected_input_device_name(None)
}

pub fn best_available_input_device_name() -> Option<String> {
    selected_input_device_name(None)
}

pub fn best_available_input_device_name_with_preference(
    preferred_name: Option<&str>,
) -> Option<String> {
    selected_input_device_name(preferred_name)
}

fn arecord_input_device_names() -> Vec<String> {
    let output = match Command::new("arecord").arg("-L").output() {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };

    let Ok(text) = String::from_utf8(output.stdout) else {
        return Vec::new();
    };

    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with('#'))
        .filter(|line| !line.starts_with("null"))
        .filter(|line| !line.starts_with("default"))
        .filter(|line| !line.starts_with("pulse"))
        .filter(|line| !line.starts_with("pipewire"))
        .filter(|line| {
            line.starts_with("sysdefault:")
                || line.starts_with("front:")
                || line.starts_with("hw:")
                || line.starts_with("plughw:")
        })
        .map(ToString::to_string)
        .collect()
}

pub fn available_input_device_names() -> Vec<String> {
    let mut names = BTreeSet::new();

    for host in available_hosts_in_priority_order() {
        let Ok(candidates) = enumerate_host_input_candidates(&host) else {
            continue;
        };
        for candidate in candidates {
            if candidate.is_monitor {
                continue;
            }
            names.insert(candidate.name);
        }
    }

    for alsa_name in arecord_input_device_names() {
        names.insert(alsa_name);
    }

    names.into_iter().collect()
}
