use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

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
    toggle_url: String,
    engine_mode: EngineMode,
    hotkey_hint: String,
    control_mode: InputControlMode,
    copy_to_clipboard: bool,
    auto_paste: bool,
    ptt_button_down: bool,
    ptt_owns_session: bool,
    ptt_start_requested: bool,
    ptt_pending_stop_after_start: bool,
    ptt_press_started_at: Option<Instant>,
    ptt_activation_delay_ms: u64,
    last_toggle_request_at: Option<Instant>,
    opacity: f32,
    show_meter: bool,
}

#[derive(Debug, Clone)]
struct OverlayAppUiConfig {
    toggle_url: String,
    engine_mode: EngineMode,
    hotkey_hint: String,
    control_mode: InputControlMode,
    copy_to_clipboard: bool,
    auto_paste: bool,
    opacity: f32,
    show_meter: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputControlMode {
    Toggle,
    Hold,
}

impl InputControlMode {
    fn label(self) -> &'static str {
        match self {
            Self::Toggle => "Tap",
            Self::Hold => "Hold",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineMode {
    Streaming,
    Batch,
}

impl EngineMode {
    fn label(self) -> &'static str {
        match self {
            Self::Streaming => "Streaming",
            Self::Batch => "Batch",
        }
    }

    fn toggled(self) -> Self {
        match self {
            Self::Streaming => Self::Batch,
            Self::Batch => Self::Streaming,
        }
    }

    fn is_streaming(self) -> bool {
        matches!(self, Self::Streaming)
    }
}

impl OverlayApp {
    fn new(state: Arc<Mutex<OverlayState>>, ui_cfg: OverlayAppUiConfig) -> Self {
        Self {
            state,
            toggle_url: ui_cfg.toggle_url,
            engine_mode: ui_cfg.engine_mode,
            hotkey_hint: ui_cfg.hotkey_hint,
            control_mode: ui_cfg.control_mode,
            copy_to_clipboard: ui_cfg.copy_to_clipboard,
            auto_paste: ui_cfg.auto_paste,
            ptt_button_down: false,
            ptt_owns_session: false,
            ptt_start_requested: false,
            ptt_pending_stop_after_start: false,
            ptt_press_started_at: None,
            ptt_activation_delay_ms: 260,
            last_toggle_request_at: None,
            opacity: ui_cfg.opacity,
            show_meter: ui_cfg.show_meter,
        }
    }

    fn reset_hold_state(&mut self) {
        self.ptt_button_down = false;
        self.ptt_owns_session = false;
        self.ptt_start_requested = false;
        self.ptt_pending_stop_after_start = false;
        self.ptt_press_started_at = None;
    }

    fn set_control_mode(&mut self, mode: InputControlMode) {
        if self.control_mode != mode {
            self.control_mode = mode;
            self.reset_hold_state();
        }
    }

    fn request_toggle_action(&mut self) {
        let now = Instant::now();
        if self
            .last_toggle_request_at
            .map(|last| now.duration_since(last) < Duration::from_millis(150))
            .unwrap_or(false)
        {
            return;
        }

        self.last_toggle_request_at = Some(now);
        {
            let mut guard = self.state.lock().expect("overlay state lock poisoned");
            guard.status_line = "Toggling...".to_string();
        }
        let body = ToggleBody {
            copy_to_clipboard: Some(self.copy_to_clipboard),
            auto_paste: Some(self.auto_paste),
        };
        request_toggle(self.toggle_url.clone(), self.state.clone(), Some(body));
    }

    fn cycle_final_action_mode(&mut self) {
        match (self.copy_to_clipboard, self.auto_paste) {
            (true, true) => {
                self.copy_to_clipboard = true;
                self.auto_paste = false;
            }
            (true, false) => {
                self.copy_to_clipboard = false;
                self.auto_paste = true;
            }
            (false, true) => {
                self.copy_to_clipboard = false;
                self.auto_paste = false;
            }
            (false, false) => {
                self.copy_to_clipboard = true;
                self.auto_paste = true;
            }
        }

        let mut guard = self.state.lock().expect("overlay state lock poisoned");
        guard.status_line = format!(
            "Final action set: {}",
            format_run_output_mode(self.copy_to_clipboard, self.auto_paste)
        );
    }

    fn cycle_engine_mode(&mut self) {
        self.engine_mode = self.engine_mode.toggled();
        persist_engine_mode(self.engine_mode, self.state.clone());
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct ToggleBody {
    copy_to_clipboard: Option<bool>,
    auto_paste: Option<bool>,
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

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let is_recording = snapshot.phase == "recording";
                        let button_label = if is_recording { "Stop" } else { "Start" };
                        let button_fill = if is_recording {
                            egui::Color32::from_rgb(210, 70, 70)
                        } else {
                            egui::Color32::from_rgb(70, 180, 95)
                        };
                        let button_text = if is_recording {
                            egui::RichText::new(button_label)
                                .strong()
                                .color(egui::Color32::WHITE)
                        } else {
                            egui::RichText::new(button_label)
                                .strong()
                                .color(egui::Color32::BLACK)
                        };
                        let clicked = ui
                            .add(
                                egui::Button::new(button_text)
                                    .fill(button_fill)
                                    .stroke(egui::Stroke::new(1.0, egui::Color32::BLACK)),
                            )
                            .clicked();
                        if clicked {
                            self.request_toggle_action();
                        }
                    });
                });

                ui.horizontal_wrapped(|ui| {
                    if draw_clickable_badge(
                        ui,
                        &format!("Engine: {}", self.engine_mode.label()),
                        egui::Color32::from_rgb(63, 81, 181),
                        egui::Color32::WHITE,
                    ) {
                        self.cycle_engine_mode();
                    }
                    draw_badge(
                        ui,
                        &format!("State: {}", snapshot.phase),
                        egui::Color32::from_rgb(84, 110, 122),
                        egui::Color32::WHITE,
                    );
                    if draw_clickable_badge(
                        ui,
                        &format!(
                            "Final action: {}",
                            format_run_output_mode(self.copy_to_clipboard, self.auto_paste)
                        ),
                        egui::Color32::from_rgb(56, 142, 60),
                        egui::Color32::BLACK,
                    ) {
                        self.cycle_final_action_mode();
                    }
                    draw_badge(
                        ui,
                        &format!("Input: {}", self.control_mode.label()),
                        egui::Color32::from_rgb(121, 85, 72),
                        egui::Color32::WHITE,
                    );
                    draw_badge(
                        ui,
                        &format!("Hotkey: {}", self.hotkey_hint),
                        egui::Color32::from_rgb(96, 125, 139),
                        egui::Color32::WHITE,
                    );
                });
                ui.small("Click Engine to switch (service restart required).");
                ui.small("Click Final action to cycle output mode for next run.");

                ui.horizontal(|ui| {
                    ui.label("Control mode:");
                    if ui
                        .selectable_label(self.control_mode == InputControlMode::Toggle, "Tap")
                        .clicked()
                    {
                        self.set_control_mode(InputControlMode::Toggle);
                    }
                    if ui
                        .selectable_label(self.control_mode == InputControlMode::Hold, "Hold")
                        .clicked()
                    {
                        self.set_control_mode(InputControlMode::Hold);
                    }
                });

                if self.control_mode == InputControlMode::Hold {
                    let is_recording = snapshot.phase == "recording";
                    if self.ptt_start_requested && is_recording {
                        self.ptt_start_requested = false;
                        self.ptt_owns_session = true;
                    }
                    if self.ptt_pending_stop_after_start && is_recording {
                        self.request_toggle_action();
                        self.ptt_pending_stop_after_start = false;
                        self.ptt_owns_session = false;
                    }

                    ui.horizontal(|ui| {
                        let hold_button = ui.add(
                            egui::Button::new(
                                egui::RichText::new("Hold To Talk")
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(121, 92, 44))
                            .stroke(egui::Stroke::new(1.0, egui::Color32::BLACK)),
                        );

                        let is_down = hold_button.is_pointer_button_down_on();
                        let now = Instant::now();

                        if is_down && !self.ptt_button_down {
                            self.ptt_press_started_at = Some(now);
                            self.ptt_pending_stop_after_start = false;
                        }

                        if is_down && !self.ptt_owns_session && !self.ptt_start_requested {
                            let elapsed = self
                                .ptt_press_started_at
                                .map(|started| now.duration_since(started))
                                .unwrap_or_default();
                            let ready =
                                elapsed >= Duration::from_millis(self.ptt_activation_delay_ms);
                            if ready && !is_recording && snapshot.phase != "processing" {
                                self.request_toggle_action();
                                self.ptt_start_requested = true;
                            }
                        }

                        if !is_down && self.ptt_button_down {
                            if self.ptt_owns_session && is_recording {
                                self.request_toggle_action();
                            } else if self.ptt_start_requested {
                                self.ptt_pending_stop_after_start = true;
                            }

                            self.ptt_owns_session = false;
                            self.ptt_start_requested = false;
                            self.ptt_press_started_at = None;
                        }

                        self.ptt_button_down = is_down;
                        ui.label("Hold to talk (quick taps are ignored).");
                    });
                } else {
                    self.reset_hold_state();
                }

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
    toggle_url: String,
    engine_mode: EngineMode,
    hotkey_hint: String,
    control_mode: InputControlMode,
    copy_to_clipboard: bool,
    auto_paste: bool,
    always_on_top: bool,
    width: f32,
    height: f32,
    opacity: f32,
    show_meter: bool,
}

impl OverlayRuntimeConfig {
    fn from_config(config: &Config) -> Self {
        let overlay_cfg: &OverlayConfig = &config.overlay;
        let toggle_url = derive_toggle_url(&overlay_cfg.url);
        let engine_mode = if config.streaming.enabled {
            EngineMode::Streaming
        } else {
            EngineMode::Batch
        };

        Self {
            url: overlay_cfg.url.clone(),
            toggle_url,
            engine_mode,
            hotkey_hint: "Super+R".to_string(),
            control_mode: InputControlMode::Toggle,
            copy_to_clipboard: true,
            auto_paste: config.behavior.auto_paste,
            always_on_top: overlay_cfg.always_on_top,
            width: overlay_cfg.width as f32,
            height: overlay_cfg.height as f32,
            opacity: overlay_cfg.opacity,
            show_meter: overlay_cfg.show_meter,
        }
    }

    fn fallback_default() -> Self {
        let cfg = OverlayConfig::default();
        let toggle_url = derive_toggle_url(&cfg.url);
        Self {
            url: cfg.url,
            toggle_url,
            engine_mode: EngineMode::Streaming,
            hotkey_hint: "Super+R".to_string(),
            control_mode: InputControlMode::Toggle,
            copy_to_clipboard: true,
            auto_paste: true,
            always_on_top: cfg.always_on_top,
            width: cfg.width as f32,
            height: cfg.height as f32,
            opacity: cfg.opacity,
            show_meter: cfg.show_meter,
        }
    }
}

fn format_run_output_mode(copy_to_clipboard: bool, auto_paste: bool) -> &'static str {
    match (copy_to_clipboard, auto_paste) {
        (true, true) => "Copy + paste",
        (true, false) => "Copy only",
        (false, true) => "Paste only",
        (false, false) => "No output",
    }
}

fn load_runtime_config() -> OverlayRuntimeConfig {
    match Config::load() {
        Ok(config) => OverlayRuntimeConfig::from_config(&config),
        Err(_) => OverlayRuntimeConfig::fallback_default(),
    }
}

fn persist_engine_mode(mode: EngineMode, state: Arc<Mutex<OverlayState>>) {
    thread::spawn(move || {
        let save_result = (|| -> Result<(), String> {
            let mut config =
                Config::load().map_err(|err| format!("Failed to load config: {err}"))?;
            config.streaming.enabled = mode.is_streaming();
            config
                .save()
                .map_err(|err| format!("Failed to save config: {err}"))?;
            Ok(())
        })();

        let mut guard = state.lock().expect("overlay state lock poisoned");
        match save_result {
            Ok(()) => {
                guard.last_error = None;
                guard.status_line = format!(
                    "Engine set to {}. Restart Audetic service to apply.",
                    mode.label()
                );
            }
            Err(err) => {
                guard.phase = "error".to_string();
                guard.last_error = Some(err.clone());
                guard.status_line = err;
            }
        }
    });
}

fn draw_badge(ui: &mut egui::Ui, text: &str, fill: egui::Color32, text_color: egui::Color32) {
    egui::Frame::none()
        .fill(fill)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(text_color).strong());
        });
}

fn draw_clickable_badge(
    ui: &mut egui::Ui,
    text: &str,
    fill: egui::Color32,
    text_color: egui::Color32,
) -> bool {
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(text_color).strong())
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_black_alpha(90)))
            .rounding(egui::Rounding::same(8.0))
            .min_size(egui::vec2(0.0, 22.0)),
    )
    .clicked()
}

fn dbfs_to_level(dbfs: f32) -> f32 {
    let floor = -72.0;
    let normalized = (dbfs - floor) / (0.0 - floor);
    normalized.clamp(0.0, 1.0).powf(0.55)
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
            let peak = stream_event
                .data
                .get("peak_dbfs")
                .and_then(|v| v.as_f64())
                .unwrap_or(rms as f64) as f32;
            let clipping = stream_event
                .data
                .get("clipping")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            // Blend RMS and peak for a more responsive but stable UI meter.
            let target = dbfs_to_level(rms).max(dbfs_to_level(peak) * 0.8);
            if target >= guard.meter_level {
                guard.meter_level = guard.meter_level * 0.3 + target * 0.7;
            } else {
                guard.meter_level = guard.meter_level * 0.82 + target * 0.18;
            }
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

fn derive_toggle_url(stream_events_url: &str) -> String {
    match reqwest::Url::parse(stream_events_url) {
        Ok(mut url) => {
            url.set_path("/toggle");
            url.set_query(None);
            url.to_string()
        }
        Err(_) => "http://127.0.0.1:3737/toggle".to_string(),
    }
}

fn request_toggle(toggle_url: String, state: Arc<Mutex<OverlayState>>, body: Option<ToggleBody>) {
    thread::spawn(move || {
        let client = match reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(8))
            .build()
        {
            Ok(client) => client,
            Err(err) => {
                let mut guard = state.lock().expect("overlay state lock poisoned");
                guard.phase = "error".to_string();
                guard.last_error = Some(format!("HTTP client error: {}", err));
                guard.status_line = "Toggle failed".to_string();
                return;
            }
        };

        let request = client.post(&toggle_url);
        let response = if let Some(payload) = body {
            request.json(&payload).send()
        } else {
            request.send()
        };
        let response = match response {
            Ok(resp) => resp,
            Err(err) => {
                let mut guard = state.lock().expect("overlay state lock poisoned");
                guard.phase = "error".to_string();
                guard.last_error = Some(format!("Toggle request failed: {}", err));
                guard.status_line = "Toggle failed".to_string();
                return;
            }
        };

        let status_code = response.status();
        let body = response.text().unwrap_or_default();
        let parsed = serde_json::from_str::<serde_json::Value>(&body).ok();

        let mut guard = state.lock().expect("overlay state lock poisoned");
        if status_code.is_success() {
            if let Some(json) = parsed {
                if let Some(phase) = json.get("phase").and_then(|v| v.as_str()) {
                    guard.phase = phase.to_string();
                }
                if let Some(message) = json.get("message").and_then(|v| v.as_str()) {
                    guard.status_line = message.to_string();
                } else {
                    guard.status_line = "Toggled".to_string();
                }
            } else {
                guard.status_line = "Toggled".to_string();
            }
            guard.last_error = None;
        } else {
            guard.phase = "error".to_string();
            guard.status_line = "Toggle failed".to_string();
            guard.last_error = Some(format!("HTTP {} {}", status_code, body));
        }
    });
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
    let toggle_url = runtime_cfg.toggle_url.clone();
    let engine_mode = runtime_cfg.engine_mode;
    let hotkey_hint = runtime_cfg.hotkey_hint.clone();
    let control_mode = runtime_cfg.control_mode;
    let copy_to_clipboard = runtime_cfg.copy_to_clipboard;
    let auto_paste = runtime_cfg.auto_paste;
    eframe::run_native(
        "Audetic Overlay",
        native_options,
        Box::new(move |_cc| {
            let ui_cfg = OverlayAppUiConfig {
                toggle_url: toggle_url.clone(),
                engine_mode,
                hotkey_hint: hotkey_hint.clone(),
                control_mode,
                copy_to_clipboard,
                auto_paste,
                opacity,
                show_meter,
            };
            Ok(Box::new(OverlayApp::new(state.clone(), ui_cfg)))
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
