use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use audetic::config::{Config, OverlayConfig};
use audetic::streaming::events::StreamEvent;
use eframe::egui;

type DynError = Box<dyn std::error::Error>;

#[derive(Debug, Clone)]
struct OverlayState {
    connected: bool,
    phase: String,
    status_line: String,
    partial_text: String,
    recent_finals: VecDeque<String>,
    meter_level: f32,
    clipping: bool,
    last_error: Option<String>,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self {
            connected: false,
            phase: "idle".to_string(),
            status_line: "Waiting for stream".to_string(),
            partial_text: String::new(),
            recent_finals: VecDeque::new(),
            meter_level: 0.0,
            clipping: false,
            last_error: None,
        }
    }
}

struct OverlayApp {
    state: Arc<Mutex<OverlayState>>,
    opacity: f32,
    show_meter: bool,
}

impl OverlayApp {
    fn new(state: Arc<Mutex<OverlayState>>, opacity: f32, show_meter: bool) -> Self {
        Self {
            state,
            opacity,
            show_meter,
        }
    }
}

impl eframe::App for OverlayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(33));

        let snapshot = {
            let guard = self.state.lock().expect("overlay state lock poisoned");
            guard.clone()
        };

        let alpha = (255.0 * self.opacity.clamp(0.2, 1.0)).round() as u8;
        let bg = egui::Color32::from_rgba_unmultiplied(20, 23, 28, alpha);
        let panel_frame = egui::Frame::default()
            .fill(bg)
            .inner_margin(egui::Margin::same(12.0));

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Audetic Live");

                    let status_color = if snapshot.connected {
                        egui::Color32::from_rgb(77, 214, 114)
                    } else {
                        egui::Color32::from_rgb(232, 99, 87)
                    };
                    ui.colored_label(status_color, "●");
                    ui.label(format!("{} | {}", snapshot.phase, snapshot.status_line));
                });

                if self.show_meter {
                    let meter = egui::ProgressBar::new(snapshot.meter_level)
                        .show_percentage()
                        .text("Mic level");
                    ui.add(meter);

                    if snapshot.clipping {
                        ui.colored_label(egui::Color32::from_rgb(255, 96, 96), "Clipping detected");
                    }
                }

                ui.separator();
                ui.label("Live partial");
                if snapshot.partial_text.trim().is_empty() {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("...").italics());
                    ui.add_space(6.0);
                } else {
                    ui.label(egui::RichText::new(snapshot.partial_text).size(18.0));
                }

                ui.separator();
                ui.label("Recent final segments");

                egui::ScrollArea::vertical()
                    .max_height(110.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if snapshot.recent_finals.is_empty() {
                            ui.label(egui::RichText::new("No final transcript yet").italics());
                        } else {
                            for line in snapshot.recent_finals {
                                ui.label(line);
                            }
                        }
                    });

                if let Some(err) = snapshot.last_error {
                    ui.separator();
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 96, 96),
                        format!("Error: {}", err),
                    );
                }
            });
    }
}

#[derive(Debug, Clone)]
struct OverlayRuntimeConfig {
    url: String,
    always_on_top: bool,
    width: f32,
    height: f32,
    opacity: f32,
    show_meter: bool,
}

impl OverlayRuntimeConfig {
    fn from_overlay_config(cfg: &OverlayConfig) -> Self {
        Self {
            url: cfg.url.clone(),
            always_on_top: cfg.always_on_top,
            width: cfg.width as f32,
            height: cfg.height as f32,
            opacity: cfg.opacity,
            show_meter: cfg.show_meter,
        }
    }
}

fn load_runtime_config() -> OverlayRuntimeConfig {
    match Config::load() {
        Ok(config) => OverlayRuntimeConfig::from_overlay_config(&config.overlay),
        Err(_) => OverlayRuntimeConfig::from_overlay_config(&OverlayConfig::default()),
    }
}

fn dbfs_to_level(dbfs: f32) -> f32 {
    let floor = -60.0;
    let normalized = (dbfs - floor) / (0.0 - floor);
    normalized.clamp(0.0, 1.0)
}

fn push_final_line(state: &mut OverlayState, line: String) {
    if line.trim().is_empty() {
        return;
    }

    state.recent_finals.push_back(line);
    while state.recent_finals.len() > 6 {
        state.recent_finals.pop_front();
    }
}

fn apply_stream_event(state: &Arc<Mutex<OverlayState>>, stream_event: StreamEvent) {
    let mut guard = state.lock().expect("overlay state lock poisoned");
    guard.connected = true;
    guard.last_error = None;

    match stream_event.event_type.as_str() {
        "session_started" => {
            guard.phase = "recording".to_string();
            guard.status_line = "Listening".to_string();
            guard.partial_text.clear();
            guard.meter_level = 0.0;
            guard.clipping = false;
        }
        "session_stopped" => {
            guard.phase = "idle".to_string();
            guard.status_line = "Stopped".to_string();
            guard.partial_text.clear();
            guard.meter_level = 0.0;
            guard.clipping = false;
        }
        "partial" => {
            if let Some(text) = stream_event.data.get("text").and_then(|v| v.as_str()) {
                guard.phase = "recording".to_string();
                guard.status_line = "Transcribing".to_string();
                guard.partial_text = text.to_string();
            }
        }
        "final" => {
            if let Some(text) = stream_event.data.get("text").and_then(|v| v.as_str()) {
                guard.phase = "processing".to_string();
                guard.status_line = "Final segment".to_string();
                push_final_line(&mut guard, text.to_string());
                guard.partial_text.clear();
            }
        }
        "audio_level" => {
            let rms = stream_event
                .data
                .get("rms_dbfs")
                .and_then(|v| v.as_f64())
                .unwrap_or(-90.0) as f32;
            let clipping = stream_event
                .data
                .get("clipping")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            guard.meter_level = dbfs_to_level(rms);
            guard.clipping = clipping;
        }
        "error" => {
            guard.phase = "error".to_string();
            guard.status_line = "Stream error".to_string();
            guard.last_error = stream_event
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
        }
        "warning" => {
            if let Some(msg) = stream_event.data.get("message").and_then(|v| v.as_str()) {
                guard.status_line = msg.to_string();
            }
        }
        _ => {}
    }
}

fn apply_fallback_event(state: &Arc<Mutex<OverlayState>>, event_name: &str, payload: &str) {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) {
        if let Some(message) = json.get("message").and_then(|v| v.as_str()) {
            let mut guard = state.lock().expect("overlay state lock poisoned");
            guard.connected = true;
            guard.status_line = message.to_string();
            if event_name == "error" {
                guard.phase = "error".to_string();
                guard.last_error = Some(message.to_string());
            }
        }
    }
}

fn handle_dispatch(state: &Arc<Mutex<OverlayState>>, event_name: &str, payload: &str) {
    if payload.trim().is_empty() {
        return;
    }

    match serde_json::from_str::<StreamEvent>(payload) {
        Ok(event) => apply_stream_event(state, event),
        Err(_) => apply_fallback_event(state, event_name, payload),
    }
}

fn run_sse_loop(url: String, state: Arc<Mutex<OverlayState>>) {
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(45))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            let mut guard = state.lock().expect("overlay state lock poisoned");
            guard.phase = "error".to_string();
            guard.last_error = Some(format!("Failed to create HTTP client: {}", err));
            return;
        }
    };

    loop {
        {
            let mut guard = state.lock().expect("overlay state lock poisoned");
            guard.connected = false;
            guard.status_line = "Connecting...".to_string();
        }

        let response = client
            .get(&url)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send();

        let response = match response {
            Ok(response) => response,
            Err(err) => {
                let mut guard = state.lock().expect("overlay state lock poisoned");
                guard.connected = false;
                guard.status_line = "Retrying stream".to_string();
                guard.last_error = Some(err.to_string());
                drop(guard);
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };

        if !response.status().is_success() {
            let mut guard = state.lock().expect("overlay state lock poisoned");
            guard.connected = false;
            guard.status_line = "Retrying stream".to_string();
            guard.last_error = Some(format!("HTTP {} from stream endpoint", response.status()));
            drop(guard);
            thread::sleep(Duration::from_secs(2));
            continue;
        }

        {
            let mut guard = state.lock().expect("overlay state lock poisoned");
            guard.connected = true;
            guard.last_error = None;
            guard.status_line = "Connected".to_string();
        }

        let mut current_event = String::new();
        let mut current_data = String::new();
        let reader = BufReader::new(response);

        for line_result in reader.lines() {
            let line = match line_result {
                Ok(line) => line,
                Err(err) => {
                    let mut guard = state.lock().expect("overlay state lock poisoned");
                    guard.connected = false;
                    guard.status_line = "Stream disconnected".to_string();
                    guard.last_error = Some(err.to_string());
                    break;
                }
            };

            if line.is_empty() {
                if !current_data.is_empty() {
                    handle_dispatch(&state, &current_event, &current_data);
                }
                current_event.clear();
                current_data.clear();
                continue;
            }

            if let Some(value) = line.strip_prefix("event:") {
                current_event = value.trim().to_string();
                continue;
            }

            if let Some(value) = line.strip_prefix("data:") {
                if !current_data.is_empty() {
                    current_data.push('\n');
                }
                current_data.push_str(value.trim_start());
            }
        }

        {
            let mut guard = state.lock().expect("overlay state lock poisoned");
            guard.connected = false;
            if guard.phase != "error" {
                guard.status_line = "Reconnecting".to_string();
            }
        }

        thread::sleep(Duration::from_millis(900));
    }
}

fn run() -> Result<(), DynError> {
    let runtime_cfg = load_runtime_config();
    let state = Arc::new(Mutex::new(OverlayState::default()));

    {
        let stream_state = state.clone();
        let stream_url = runtime_cfg.url.clone();
        thread::Builder::new()
            .name("audetic-overlay-sse".to_string())
            .spawn(move || run_sse_loop(stream_url, stream_state))
            .map_err(|err| -> DynError { Box::new(err) })?;
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Audetic Overlay")
        .with_app_id("audetic")
        .with_inner_size(egui::vec2(runtime_cfg.width, runtime_cfg.height))
        .with_min_inner_size(egui::vec2(380.0, 150.0));

    if runtime_cfg.always_on_top {
        viewport = viewport.with_always_on_top();
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let opacity = runtime_cfg.opacity;
    let show_meter = runtime_cfg.show_meter;
    eframe::run_native(
        "Audetic Overlay",
        native_options,
        Box::new(move |_cc| {
            Ok(Box::new(OverlayApp::new(
                state.clone(),
                opacity,
                show_meter,
            )))
        }),
    )
    .map_err(|err| -> DynError { Box::new(err) })?;

    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("audetic-overlay: {}", err);
        std::process::exit(1);
    }
}
