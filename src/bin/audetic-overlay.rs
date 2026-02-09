use std::collections::VecDeque;
use std::f32::consts::TAU;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use audetic::config::{Config, OverlayConfig};
use audetic::streaming::events::StreamEvent;
use cpal::traits::{DeviceTrait, HostTrait};
use eframe::egui;
use fs2::FileExt;

type DynError = Box<dyn std::error::Error>;
const WAVE_HISTORY_CAP: usize = 260;
const WAVE_BAR_COUNT: usize = 64;
const OVERLAY_HIDE_DELAY_MS: u64 = 1300;
const METER_FLOOR_DBFS: f32 = -58.0;
const METER_GATE_DBFS: f32 = -52.0;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct OutputModeState {
    copy_to_clipboard: bool,
    auto_paste: bool,
    append_newline: bool,
    send_enter: bool,
}

#[derive(Debug, Clone)]
struct OverlayState {
    connected: bool,
    phase: String,
    status_line: String,
    partial_text: String,
    recent_finals: VecDeque<String>,
    meter_level: f32,
    meter_history: VecDeque<f32>,
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
            meter_history: VecDeque::new(),
            clipping: false,
            last_error: None,
        }
    }
}

struct OverlayApp {
    state: Arc<Mutex<OverlayState>>,
    toggle_url: String,
    start_url: String,
    stop_url: String,
    engine_mode: EngineMode,
    streaming_model_label: String,
    streaming_source_label: String,
    batch_model_label: String,
    batch_source_label: String,
    mic_label: String,
    control_mode: InputControlMode,
    copy_to_clipboard: bool,
    auto_paste: bool,
    append_newline: bool,
    send_enter: bool,
    ptt_button_down: bool,
    ptt_owns_session: bool,
    ptt_start_requested: bool,
    ptt_pending_stop_after_start: bool,
    ptt_press_started_at: Option<Instant>,
    ptt_activation_delay_ms: u64,
    last_toggle_request_at: Option<Instant>,
    show_settings: bool,
    audio_ducking: bool,
    ducking_level_percent: u8,
    preserve_clipboard: bool,
    opacity: f32,
    show_meter: bool,
    settings_dirty: bool,
    last_settings_save_at: Option<Instant>,
    overlay_visible: bool,
    has_seen_active_phase: bool,
    last_active_phase_at: Option<Instant>,
}

#[derive(Debug, Clone)]
struct OverlayAppUiConfig {
    toggle_url: String,
    start_url: String,
    stop_url: String,
    engine_mode: EngineMode,
    streaming_model_label: String,
    streaming_source_label: String,
    batch_model_label: String,
    batch_source_label: String,
    mic_label: String,
    control_mode: InputControlMode,
    ptt_activation_delay_ms: u64,
    copy_to_clipboard: bool,
    auto_paste: bool,
    append_newline: bool,
    send_enter: bool,
    audio_ducking: bool,
    ducking_level_percent: u8,
    preserve_clipboard: bool,
    opacity: f32,
    show_meter: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum InputControlMode {
    Toggle,
    Hold,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    CopyAndPaste,
    CopyOnly,
    PasteAndEnter,
    NoOutput,
}

impl OutputMode {
    fn label(self) -> &'static str {
        match self {
            Self::CopyAndPaste => "Copy + paste",
            Self::CopyOnly => "Copy only",
            Self::PasteAndEnter => "Paste + Enter",
            Self::NoOutput => "No output",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::CopyAndPaste => Self::PasteAndEnter,
            Self::PasteAndEnter => Self::CopyOnly,
            Self::CopyOnly => Self::NoOutput,
            Self::NoOutput => Self::CopyAndPaste,
        }
    }

    fn flags(self) -> (bool, bool, bool, bool) {
        match self {
            Self::CopyAndPaste => (true, true, false, false),
            Self::CopyOnly => (true, false, false, false),
            Self::PasteAndEnter => (true, true, false, true),
            Self::NoOutput => (false, false, false, false),
        }
    }
}

impl OverlayApp {
    fn new(state: Arc<Mutex<OverlayState>>, ui_cfg: OverlayAppUiConfig) -> Self {
        Self {
            state,
            toggle_url: ui_cfg.toggle_url,
            start_url: ui_cfg.start_url,
            stop_url: ui_cfg.stop_url,
            engine_mode: ui_cfg.engine_mode,
            streaming_model_label: ui_cfg.streaming_model_label,
            streaming_source_label: ui_cfg.streaming_source_label,
            batch_model_label: ui_cfg.batch_model_label,
            batch_source_label: ui_cfg.batch_source_label,
            mic_label: ui_cfg.mic_label,
            control_mode: ui_cfg.control_mode,
            copy_to_clipboard: ui_cfg.copy_to_clipboard,
            auto_paste: ui_cfg.auto_paste,
            append_newline: ui_cfg.append_newline,
            send_enter: ui_cfg.send_enter,
            ptt_button_down: false,
            ptt_owns_session: false,
            ptt_start_requested: false,
            ptt_pending_stop_after_start: false,
            ptt_press_started_at: None,
            ptt_activation_delay_ms: ui_cfg.ptt_activation_delay_ms,
            last_toggle_request_at: None,
            show_settings: false,
            audio_ducking: ui_cfg.audio_ducking,
            ducking_level_percent: ui_cfg.ducking_level_percent,
            preserve_clipboard: ui_cfg.preserve_clipboard,
            opacity: ui_cfg.opacity,
            show_meter: ui_cfg.show_meter,
            settings_dirty: false,
            last_settings_save_at: None,
            overlay_visible: true,
            has_seen_active_phase: false,
            last_active_phase_at: None,
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
        self.request_control_action(self.toggle_url.clone());
    }

    fn request_start_action(&mut self) {
        self.request_control_action(self.start_url.clone());
    }

    fn request_stop_action(&mut self) {
        self.request_control_action(self.stop_url.clone());
    }

    fn request_control_action(&mut self, url: String) {
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
            append_newline: Some(self.append_newline),
            send_enter: Some(self.send_enter),
        };
        request_control(url, self.state.clone(), Some(body));
    }

    fn cycle_final_action_mode(&mut self) {
        let next_mode = output_mode_from_flags(
            self.copy_to_clipboard,
            self.auto_paste,
            self.append_newline,
            self.send_enter,
        )
        .next();
        let (copy_to_clipboard, auto_paste, append_newline, send_enter) = next_mode.flags();
        self.copy_to_clipboard = copy_to_clipboard;
        self.auto_paste = auto_paste;
        self.append_newline = append_newline;
        self.send_enter = send_enter;

        let mut guard = self.state.lock().expect("overlay state lock poisoned");
        let persist_result = persist_output_mode_state(OutputModeState {
            copy_to_clipboard: self.copy_to_clipboard,
            auto_paste: self.auto_paste,
            append_newline: self.append_newline,
            send_enter: self.send_enter,
        });
        match persist_result {
            Ok(()) => {
                guard.status_line = format!("Final action set: {}", next_mode.label());
                guard.last_error = None;
            }
            Err(err) => {
                guard.status_line =
                    format!("Final action set: {} (save failed)", next_mode.label());
                guard.last_error = Some(err);
            }
        }
    }

    fn cycle_engine_mode(&mut self) {
        self.engine_mode = self.engine_mode.toggled();
        persist_engine_mode(self.engine_mode, self.state.clone());
    }

    fn active_model_label(&self) -> &str {
        match self.engine_mode {
            EngineMode::Streaming => &self.streaming_model_label,
            EngineMode::Batch => &self.batch_model_label,
        }
    }

    fn active_source_label(&self) -> &str {
        match self.engine_mode {
            EngineMode::Streaming => &self.streaming_source_label,
            EngineMode::Batch => &self.batch_source_label,
        }
    }

    fn save_settings(&mut self) {
        let update = OverlaySettingsUpdate {
            control_mode: self.control_mode,
            ptt_activation_delay_ms: self.ptt_activation_delay_ms,
            opacity: self.opacity,
            show_meter: self.show_meter,
            audio_ducking: self.audio_ducking,
            ducking_level_percent: self.ducking_level_percent,
            preserve_clipboard: self.preserve_clipboard,
        };
        persist_overlay_settings(update, self.state.clone());
    }

    fn mark_settings_dirty(&mut self) {
        self.settings_dirty = true;
    }

    fn maybe_autosave_settings(&mut self, force: bool) {
        if !self.settings_dirty {
            return;
        }

        let now = Instant::now();
        let should_save = force
            || self
                .last_settings_save_at
                .map(|last| now.duration_since(last) >= Duration::from_millis(320))
                .unwrap_or(true);
        if !should_save {
            return;
        }

        self.save_settings();
        self.last_settings_save_at = Some(now);
        self.settings_dirty = false;
    }

    fn sync_overlay_visibility(&mut self, ctx: &egui::Context, phase: &str) {
        let now = Instant::now();
        let active = matches!(phase, "recording" | "processing");
        if active {
            self.has_seen_active_phase = true;
            self.last_active_phase_at = Some(now);
        }

        let should_be_visible = if self.show_settings || active || !self.has_seen_active_phase {
            true
        } else {
            self.last_active_phase_at
                .map(|last| now.duration_since(last) < Duration::from_millis(OVERLAY_HIDE_DELAY_MS))
                .unwrap_or(false)
        };

        if should_be_visible != self.overlay_visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(should_be_visible));
            self.overlay_visible = should_be_visible;
        }
    }
}

fn output_mode_from_flags(
    copy_to_clipboard: bool,
    auto_paste: bool,
    append_newline: bool,
    send_enter: bool,
) -> OutputMode {
    match (copy_to_clipboard, auto_paste, append_newline, send_enter) {
        (true, true, _, true) => OutputMode::PasteAndEnter,
        (true, true, _, false) => OutputMode::CopyAndPaste,
        (true, false, _, _) => OutputMode::CopyOnly,
        _ => OutputMode::NoOutput,
    }
}

fn output_mode_state_path() -> Option<PathBuf> {
    audetic::global::data_dir()
        .ok()
        .map(|dir| dir.join("output_mode.json"))
}

fn load_output_mode_state() -> Option<OutputModeState> {
    let path = output_mode_state_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<OutputModeState>(&content).ok()
}

fn persist_output_mode_state(state: OutputModeState) -> Result<(), String> {
    let path = output_mode_state_path().ok_or("Could not resolve data directory".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create output-mode directory: {err}"))?;
    }
    let content = serde_json::to_string_pretty(&state)
        .map_err(|err| format!("Failed to serialize output mode state: {err}"))?;
    std::fs::write(path, content).map_err(|err| format!("Failed to write output mode state: {err}"))
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct OverlayUiState {
    control_mode: InputControlMode,
    ptt_activation_delay_ms: u64,
    opacity: f32,
    show_meter: bool,
}

#[derive(Debug, Clone, Copy)]
struct OverlaySettingsUpdate {
    control_mode: InputControlMode,
    ptt_activation_delay_ms: u64,
    opacity: f32,
    show_meter: bool,
    audio_ducking: bool,
    ducking_level_percent: u8,
    preserve_clipboard: bool,
}

fn overlay_ui_state_path() -> Option<PathBuf> {
    audetic::global::data_dir()
        .ok()
        .map(|dir| dir.join("overlay_ui_state.json"))
}

fn load_overlay_ui_state() -> Option<OverlayUiState> {
    let path = overlay_ui_state_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<OverlayUiState>(&content).ok()
}

fn persist_overlay_ui_state(state: OverlayUiState) -> Result<(), String> {
    let path = overlay_ui_state_path().ok_or("Could not resolve data directory".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create overlay state directory: {err}"))?;
    }
    let content = serde_json::to_string_pretty(&state)
        .map_err(|err| format!("Failed to serialize overlay state: {err}"))?;
    std::fs::write(path, content).map_err(|err| format!("Failed to write overlay state: {err}"))
}

#[derive(Debug, Clone, serde::Serialize)]
struct ToggleBody {
    copy_to_clipboard: Option<bool>,
    auto_paste: Option<bool>,
    append_newline: Option<bool>,
    send_enter: Option<bool>,
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

        self.sync_overlay_visibility(ctx, snapshot.phase.as_str());

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Options ⚙")
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(52, 73, 94))
                            .stroke(egui::Stroke::new(1.0, egui::Color32::from_black_alpha(120))),
                        )
                        .clicked()
                    {
                        if self.show_settings {
                            self.maybe_autosave_settings(true);
                            self.show_settings = false;
                        } else {
                            self.show_settings = true;
                            if !self.overlay_visible {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                                self.overlay_visible = true;
                            }
                        }
                    }
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
                        self.active_model_label(),
                        egui::Color32::from_rgb(40, 53, 147),
                        egui::Color32::WHITE,
                    );
                    draw_badge(
                        ui,
                        self.active_source_label(),
                        egui::Color32::from_rgb(2, 119, 189),
                        egui::Color32::WHITE,
                    );
                });

                ui.horizontal_wrapped(|ui| {
                    if draw_clickable_badge(
                        ui,
                        &format!(
                            "Final action: {}",
                            output_mode_from_flags(
                                self.copy_to_clipboard,
                                self.auto_paste,
                                self.append_newline,
                                self.send_enter,
                            )
                            .label()
                        ),
                        egui::Color32::from_rgb(56, 142, 60),
                        egui::Color32::BLACK,
                    ) {
                        self.cycle_final_action_mode();
                    }
                    draw_badge(
                        ui,
                        &format!("Mic: {}", self.mic_label),
                        egui::Color32::from_rgb(69, 90, 100),
                        egui::Color32::WHITE,
                    );
                });

                if self.show_meter {
                    ui.add_space(6.0);
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgba_unmultiplied(18, 22, 28, 210))
                        .rounding(egui::Rounding::same(8.0))
                        .inner_margin(egui::Margin::same(8.0))
                        .show(ui, |ui| {
                            draw_audio_waveform(
                                ui,
                                &snapshot.meter_history,
                                snapshot.meter_level,
                                snapshot.phase.as_str(),
                                snapshot.clipping,
                            );
                            ui.horizontal(|ui| {
                                let pct = (snapshot.meter_level * 100.0).round() as i32;
                                let colour = if snapshot.phase == "recording" {
                                    egui::Color32::from_rgb(255, 214, 102)
                                } else {
                                    egui::Color32::from_rgb(165, 188, 215)
                                };
                                ui.label(
                                    egui::RichText::new(format!("Level: {pct}%")).color(colour),
                                );
                                if snapshot.clipping {
                                    ui.colored_label(egui::Color32::from_rgb(255, 96, 96), "CLIP");
                                }
                            });
                        });
                }

                if self.show_settings {
                    let mut options_changed = false;
                    let mut close_clicked = false;

                    ui.add_space(6.0);
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(31, 35, 41))
                        .rounding(egui::Rounding::same(8.0))
                        .inner_margin(egui::Margin::same(10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Options").strong());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("Close").clicked() {
                                            close_clicked = true;
                                        }
                                    },
                                );
                            });

                            ui.add_space(4.0);
                            let settings_max_height = (ui.available_height() - 8.0).max(140.0);
                            egui::ScrollArea::vertical()
                                .max_height(settings_max_height)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label("Control mode:");
                                        if ui
                                            .selectable_label(
                                                self.control_mode == InputControlMode::Toggle,
                                                "Tap",
                                            )
                                            .clicked()
                                        {
                                            self.set_control_mode(InputControlMode::Toggle);
                                            options_changed = true;
                                        }
                                        if ui
                                            .selectable_label(
                                                self.control_mode == InputControlMode::Hold,
                                                "Hold",
                                            )
                                            .clicked()
                                        {
                                            self.set_control_mode(InputControlMode::Hold);
                                            options_changed = true;
                                        }
                                    });

                                    ui.horizontal(|ui| {
                                        ui.label("Hold delay:");
                                        let hold_delay = ui.add(
                                            egui::Slider::new(
                                                &mut self.ptt_activation_delay_ms,
                                                120..=800,
                                            )
                                            .suffix(" ms")
                                            .clamping(egui::SliderClamping::Always),
                                        );
                                        options_changed |= hold_delay.changed();
                                    });

                                    ui.horizontal(|ui| {
                                        let meter_toggle =
                                            ui.checkbox(&mut self.show_meter, "Show mic meter");
                                        ui.label("Overlay opacity:");
                                        let opacity_slider = ui
                                            .add(egui::Slider::new(&mut self.opacity, 0.45..=1.0));
                                        options_changed |=
                                            meter_toggle.changed() || opacity_slider.changed();
                                    });

                                    ui.horizontal(|ui| {
                                        let ducking_toggle =
                                            ui.checkbox(&mut self.audio_ducking, "Audio ducking");
                                        let ducking_slider = ui.add_enabled(
                                            self.audio_ducking,
                                            egui::Slider::new(
                                                &mut self.ducking_level_percent,
                                                5_u8..=95_u8,
                                            )
                                            .suffix(" %"),
                                        );
                                        options_changed |=
                                            ducking_toggle.changed() || ducking_slider.changed();
                                    });

                                    ui.horizontal(|ui| {
                                        let preserve_clipboard = ui.checkbox(
                                            &mut self.preserve_clipboard,
                                            "Restore clipboard after paste",
                                        );
                                        options_changed |= preserve_clipboard.changed();
                                    });

                                    if ui.button("Save settings").clicked() {
                                        self.save_settings();
                                        self.last_settings_save_at = Some(Instant::now());
                                        self.settings_dirty = false;
                                    }
                                });
                        });

                    if options_changed {
                        self.mark_settings_dirty();
                    }

                    if close_clicked {
                        self.show_settings = false;
                        self.maybe_autosave_settings(true);
                    } else {
                        self.maybe_autosave_settings(false);
                    }
                }

                if !self.show_settings && self.control_mode == InputControlMode::Hold {
                    let is_recording = snapshot.phase == "recording";
                    if self.ptt_start_requested && is_recording {
                        self.ptt_start_requested = false;
                        self.ptt_owns_session = true;
                    }
                    if self.ptt_pending_stop_after_start && is_recording {
                        self.request_stop_action();
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
                                self.request_start_action();
                                self.ptt_start_requested = true;
                            }
                        }

                        if !is_down && self.ptt_button_down {
                            if self.ptt_owns_session && is_recording {
                                self.request_stop_action();
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
                } else if !self.show_settings {
                    self.reset_hold_state();
                }

                if !self.show_settings && !snapshot.partial_text.trim().is_empty() {
                    ui.separator();
                    ui.label(
                        egui::RichText::new(snapshot.partial_text)
                            .size(24.0)
                            .color(egui::Color32::from_rgb(255, 224, 130)),
                    );
                }

                if !self.show_settings {
                    egui::ScrollArea::vertical()
                        .max_height(110.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for line in snapshot.recent_finals.into_iter().rev() {
                                ui.label(line);
                            }
                        });
                }

                if !self.show_settings {
                    if let Some(err) = snapshot.last_error {
                        ui.separator();
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 96, 96),
                            format!("Error: {}", err),
                        );
                    }
                }
            });
    }
}

#[derive(Debug, Clone)]
struct OverlayRuntimeConfig {
    url: String,
    toggle_url: String,
    start_url: String,
    stop_url: String,
    engine_mode: EngineMode,
    streaming_model_label: String,
    streaming_source_label: String,
    batch_model_label: String,
    batch_source_label: String,
    mic_label: String,
    control_mode: InputControlMode,
    ptt_activation_delay_ms: u64,
    copy_to_clipboard: bool,
    auto_paste: bool,
    append_newline: bool,
    send_enter: bool,
    audio_ducking: bool,
    ducking_level_percent: u8,
    preserve_clipboard: bool,
    always_on_top: bool,
    width: f32,
    height: f32,
    opacity: f32,
    show_meter: bool,
}

impl OverlayRuntimeConfig {
    fn from_config(config: &Config) -> Self {
        let overlay_cfg: &OverlayConfig = &config.overlay;
        let toggle_url = derive_control_url(&overlay_cfg.url, "/toggle");
        let start_url = derive_control_url(&overlay_cfg.url, "/start");
        let stop_url = derive_control_url(&overlay_cfg.url, "/stop");
        let engine_mode = if config.streaming.enabled {
            EngineMode::Streaming
        } else {
            EngineMode::Batch
        };
        let streaming_model_label = config.streaming.model.clone();
        let streaming_source_label = provider_source_label(&config.streaming.provider).to_string();
        let batch_provider = config
            .whisper
            .provider
            .as_deref()
            .unwrap_or("audetic-api")
            .to_string();
        let batch_model_label = config
            .whisper
            .model
            .clone()
            .unwrap_or_else(|| "<default>".to_string());
        let batch_source_label = provider_source_label(&batch_provider).to_string();
        let mic_label = detect_active_mic_name()
            .map(|name| short_mic_label(&name, 12))
            .unwrap_or_else(|| "Unknown".to_string());

        let mut runtime = Self {
            url: overlay_cfg.url.clone(),
            toggle_url,
            start_url,
            stop_url,
            engine_mode,
            streaming_model_label,
            streaming_source_label,
            batch_model_label,
            batch_source_label,
            mic_label,
            control_mode: InputControlMode::Toggle,
            ptt_activation_delay_ms: 260,
            copy_to_clipboard: true,
            auto_paste: config.behavior.auto_paste,
            append_newline: config.behavior.append_newline,
            send_enter: false,
            audio_ducking: config.behavior.audio_ducking,
            ducking_level_percent: config.behavior.ducking_level_percent.clamp(5, 95),
            preserve_clipboard: config.behavior.preserve_clipboard,
            always_on_top: overlay_cfg.always_on_top,
            width: overlay_cfg.width as f32,
            height: overlay_cfg.height as f32,
            opacity: overlay_cfg.opacity,
            show_meter: overlay_cfg.show_meter,
        };

        if let Some(saved) = load_output_mode_state() {
            runtime.copy_to_clipboard = saved.copy_to_clipboard;
            runtime.auto_paste = saved.auto_paste;
            runtime.append_newline = saved.append_newline;
            runtime.send_enter = saved.send_enter;
        }

        if let Some(saved) = load_overlay_ui_state() {
            runtime.control_mode = saved.control_mode;
            runtime.ptt_activation_delay_ms = saved.ptt_activation_delay_ms.clamp(120, 800);
            runtime.opacity = saved.opacity.clamp(0.45, 1.0);
            runtime.show_meter = saved.show_meter;
        }

        runtime
    }

    fn fallback_default() -> Self {
        let cfg = OverlayConfig::default();
        let toggle_url = derive_control_url(&cfg.url, "/toggle");
        let start_url = derive_control_url(&cfg.url, "/start");
        let stop_url = derive_control_url(&cfg.url, "/stop");
        let mut runtime = Self {
            url: cfg.url,
            toggle_url,
            start_url,
            stop_url,
            engine_mode: EngineMode::Streaming,
            streaming_model_label: "voxtral-mini-transcribe-realtime-2602".to_string(),
            streaming_source_label: "API".to_string(),
            batch_model_label: "base".to_string(),
            batch_source_label: "Local".to_string(),
            mic_label: "Unknown".to_string(),
            control_mode: InputControlMode::Toggle,
            ptt_activation_delay_ms: 260,
            copy_to_clipboard: true,
            auto_paste: true,
            append_newline: false,
            send_enter: false,
            audio_ducking: false,
            ducking_level_percent: 35,
            preserve_clipboard: false,
            always_on_top: cfg.always_on_top,
            width: cfg.width as f32,
            height: cfg.height as f32,
            opacity: cfg.opacity,
            show_meter: cfg.show_meter,
        };

        if let Some(saved) = load_output_mode_state() {
            runtime.copy_to_clipboard = saved.copy_to_clipboard;
            runtime.auto_paste = saved.auto_paste;
            runtime.append_newline = saved.append_newline;
            runtime.send_enter = saved.send_enter;
        }

        if let Some(saved) = load_overlay_ui_state() {
            runtime.control_mode = saved.control_mode;
            runtime.ptt_activation_delay_ms = saved.ptt_activation_delay_ms.clamp(120, 800);
            runtime.opacity = saved.opacity.clamp(0.45, 1.0);
            runtime.show_meter = saved.show_meter;
        }

        runtime
    }
}

fn provider_source_label(provider: &str) -> &'static str {
    match provider {
        "mistral_realtime" | "openai-api" | "assembly-ai" | "audetic-api" => "API",
        "openai-cli" | "whisper-cpp" => "Local",
        _ => "Unknown",
    }
}

fn is_placeholder_device_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "" | "default" | "default input" | "pipewire"
    )
}

fn parse_wpctl_field(output: &str, key: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let trimmed = line.trim();
        if !trimmed.starts_with(key) {
            return None;
        }
        let value = trimmed.split_once('=')?.1.trim().trim_matches('"').trim();
        if value.is_empty() || is_placeholder_device_name(value) {
            None
        } else {
            Some(value.to_string())
        }
    })
}

fn default_source_name_from_wpctl() -> Option<String> {
    let output = Command::new("wpctl")
        .args(["inspect", "@DEFAULT_AUDIO_SOURCE@"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    parse_wpctl_field(&text, "node.description")
        .or_else(|| parse_wpctl_field(&text, "node.nick"))
        .or_else(|| parse_wpctl_field(&text, "device.description"))
}

fn short_mic_label(name: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    name.chars().take(max_chars).collect()
}

fn detect_active_mic_name() -> Option<String> {
    if let Some(name) = default_source_name_from_wpctl() {
        return Some(name);
    }

    let host = cpal::default_host();
    if let Some(device) = host.default_input_device() {
        if let Ok(name) = device.name() {
            let trimmed = name.trim();
            if !trimmed.is_empty() && !is_placeholder_device_name(trimmed) {
                return Some(trimmed.to_string());
            }
        }
    }

    if let Ok(mut devices) = host.input_devices() {
        for device in devices.by_ref() {
            if let Ok(name) = device.name() {
                let trimmed = name.trim();
                if !trimmed.is_empty() && !is_placeholder_device_name(trimmed) {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    None
}

fn load_runtime_config() -> OverlayRuntimeConfig {
    match Config::load() {
        Ok(config) => OverlayRuntimeConfig::from_config(&config),
        Err(_) => OverlayRuntimeConfig::fallback_default(),
    }
}

fn persist_engine_mode(mode: EngineMode, state: Arc<Mutex<OverlayState>>) {
    let save_result = (|| -> Result<(), String> {
        let mut config = Config::load().map_err(|err| format!("Failed to load config: {err}"))?;
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
}

fn persist_overlay_settings(update: OverlaySettingsUpdate, state: Arc<Mutex<OverlayState>>) {
    let save_result = (|| -> Result<(), String> {
        persist_overlay_ui_state(OverlayUiState {
            control_mode: update.control_mode,
            ptt_activation_delay_ms: update.ptt_activation_delay_ms.clamp(120, 800),
            opacity: update.opacity.clamp(0.45, 1.0),
            show_meter: update.show_meter,
        })?;

        let mut config = Config::load().map_err(|err| format!("Failed to load config: {err}"))?;
        config.overlay.opacity = update.opacity.clamp(0.45, 1.0);
        config.overlay.show_meter = update.show_meter;
        config.behavior.audio_ducking = update.audio_ducking;
        config.behavior.ducking_level_percent = update.ducking_level_percent.clamp(5, 95);
        config.behavior.preserve_clipboard = update.preserve_clipboard;
        config
            .save()
            .map_err(|err| format!("Failed to save config: {err}"))?;
        Ok(())
    })();

    let mut guard = state.lock().expect("overlay state lock poisoned");
    match save_result {
        Ok(()) => {
            guard.last_error = None;
            guard.status_line =
                "Settings saved (restart service to apply behaviour changes).".to_string();
        }
        Err(err) => {
            guard.phase = "error".to_string();
            guard.last_error = Some(err.clone());
            guard.status_line = err;
        }
    }
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
    if dbfs <= METER_GATE_DBFS {
        return 0.0;
    }
    let normalized = (dbfs - METER_FLOOR_DBFS) / (0.0 - METER_FLOOR_DBFS);
    normalized.clamp(0.0, 1.0).powf(1.25)
}

fn sample_meter_history(history: &VecDeque<f32>, current_level: f32, samples: usize) -> Vec<f32> {
    if samples == 0 {
        return Vec::new();
    }
    if history.is_empty() {
        return vec![current_level.clamp(0.0, 1.0); samples];
    }
    if history.len() <= samples {
        let mut padded = vec![current_level.clamp(0.0, 1.0) * 0.55; samples - history.len()];
        padded.extend(history.iter().copied());
        return padded;
    }

    let len = history.len();
    let step = (len.saturating_sub(1)) as f32 / (samples.saturating_sub(1).max(1)) as f32;
    (0..samples)
        .map(|idx| {
            let sampled_idx = (idx as f32 * step).round() as usize;
            history
                .get(sampled_idx.min(len.saturating_sub(1)))
                .copied()
                .unwrap_or(0.0)
        })
        .collect()
}

fn draw_audio_waveform(
    ui: &mut egui::Ui,
    history: &VecDeque<f32>,
    current_level: f32,
    phase: &str,
    clipping: bool,
) {
    let desired_size = egui::vec2(ui.available_width(), 78.0);
    let (rect, _) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let active = phase == "recording";

    let background = if active {
        egui::Color32::from_rgba_premultiplied(18, 28, 24, 225)
    } else {
        egui::Color32::from_rgba_premultiplied(22, 24, 30, 215)
    };
    painter.rect_filled(rect, egui::Rounding::same(7.0), background);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(7.0),
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_premultiplied(180, 205, 235, 120),
        ),
    );

    let center_y = rect.center().y;
    painter.line_segment(
        [
            egui::pos2(rect.left() + 5.0, center_y),
            egui::pos2(rect.right() - 5.0, center_y),
        ],
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_premultiplied(120, 132, 150, 105),
        ),
    );

    let bars = sample_meter_history(history, current_level, WAVE_BAR_COUNT);
    let spacing = 1.5;
    let inner_width = (rect.width() - 10.0).max(10.0);
    let total_spacing = spacing * (WAVE_BAR_COUNT.saturating_sub(1) as f32);
    let bar_width = ((inner_width - total_spacing) / WAVE_BAR_COUNT as f32).max(1.2);
    let max_height = (rect.height() - 8.0).max(4.0);

    for (idx, sample) in bars.iter().enumerate() {
        let baseline = if active {
            current_level.clamp(0.0, 1.0) * 0.08
        } else {
            0.0
        };
        let amplitude = sample.max(baseline).clamp(0.0, 1.0).powf(0.52);
        if amplitude <= 0.01 {
            continue;
        }

        let x = rect.left() + 5.0 + idx as f32 * (bar_width + spacing);
        let height = (max_height * amplitude).max(3.0);
        let bar_rect = egui::Rect::from_center_size(
            egui::pos2(x + bar_width * 0.5, center_y),
            egui::vec2(bar_width, height),
        );

        let color = if clipping {
            egui::Color32::from_rgb(255, 88, 88)
        } else if active {
            let pulse = idx as f32 / (WAVE_BAR_COUNT.saturating_sub(1).max(1)) as f32;
            let red = (90.0 + pulse * 40.0).round() as u8;
            let green = (220.0 + pulse * 28.0).round() as u8;
            let blue = (245.0 - pulse * 110.0).round() as u8;
            egui::Color32::from_rgb(red, green, blue)
        } else {
            egui::Color32::from_rgb(145, 160, 185)
        };

        painter.rect_filled(bar_rect, bar_width * 0.5, color);
    }

    let time_s = ui.ctx().input(|i| i.time) as f32;
    let trace_amp = if active {
        (0.16 + current_level.clamp(0.0, 1.0) * 0.72).clamp(0.18, 0.9)
    } else {
        0.12
    };
    let mut points = Vec::with_capacity(96);
    for idx in 0..96 {
        let t = idx as f32 / 95.0;
        let x = egui::lerp((rect.left() + 5.0)..=(rect.right() - 5.0), t);
        let wave = (t * TAU * 4.0 + time_s * 6.2).sin();
        let y = center_y - wave * (max_height * 0.42 * trace_amp);
        points.push(egui::pos2(x, y));
    }
    let trace_colour = if clipping {
        egui::Color32::from_rgb(255, 112, 112)
    } else if active {
        egui::Color32::from_rgb(255, 236, 120)
    } else {
        egui::Color32::from_rgb(172, 188, 209)
    };
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(2.2, trace_colour),
    ));
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
            guard.meter_history.clear();
            guard.clipping = false;
        }
        "session_stopped" => {
            guard.phase = "idle".to_string();
            guard.status_line = "Stopped".to_string();
            guard.partial_text.clear();
            guard.meter_level = 0.0;
            guard.meter_history.clear();
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

            // Use mostly RMS with some peak contribution so pauses visibly drop.
            let rms_level = dbfs_to_level(rms);
            let peak_level = dbfs_to_level(peak);
            let target = (rms_level * 0.78 + peak_level * 0.22).clamp(0.0, 1.0);
            if target >= guard.meter_level {
                guard.meter_level = guard.meter_level * 0.2 + target * 0.8;
            } else {
                guard.meter_level = guard.meter_level * 0.58 + target * 0.42;
            }
            let level = guard.meter_level.clamp(0.0, 1.0);
            guard.meter_history.push_back(level);
            while guard.meter_history.len() > WAVE_HISTORY_CAP {
                guard.meter_history.pop_front();
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

fn derive_control_url(stream_events_url: &str, path: &str) -> String {
    match reqwest::Url::parse(stream_events_url) {
        Ok(mut url) => {
            url.set_path(path);
            url.set_query(None);
            url.to_string()
        }
        Err(_) => format!("http://127.0.0.1:3737{path}"),
    }
}

fn request_control(control_url: String, state: Arc<Mutex<OverlayState>>, body: Option<ToggleBody>) {
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
                guard.status_line = "Control request failed".to_string();
                return;
            }
        };

        let request = client.post(&control_url);
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
                guard.last_error = Some(format!("Control request failed: {}", err));
                guard.status_line = "Control request failed".to_string();
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
                    guard.status_line = "Updated".to_string();
                }
            } else {
                guard.status_line = "Updated".to_string();
            }
            guard.last_error = None;
        } else {
            guard.phase = "error".to_string();
            guard.status_line = "Control request failed".to_string();
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
    let _instance_lock = match try_acquire_overlay_lock() {
        Ok(Some(lock)) => lock,
        Ok(None) => return Ok(()),
        Err(err) => return Err(err),
    };

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

    let window_width = runtime_cfg.width.max(460.0);
    let window_height = runtime_cfg.height.max(300.0);

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Audetic Overlay")
        .with_app_id("audetic")
        .with_inner_size(egui::vec2(window_width, window_height))
        .with_min_inner_size(egui::vec2(420.0, 260.0))
        .with_visible(false)
        .with_active(false);

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
    let start_url = runtime_cfg.start_url.clone();
    let stop_url = runtime_cfg.stop_url.clone();
    let engine_mode = runtime_cfg.engine_mode;
    let streaming_model_label = runtime_cfg.streaming_model_label.clone();
    let streaming_source_label = runtime_cfg.streaming_source_label.clone();
    let batch_model_label = runtime_cfg.batch_model_label.clone();
    let batch_source_label = runtime_cfg.batch_source_label.clone();
    let mic_label = runtime_cfg.mic_label.clone();
    let control_mode = runtime_cfg.control_mode;
    let ptt_activation_delay_ms = runtime_cfg.ptt_activation_delay_ms;
    let copy_to_clipboard = runtime_cfg.copy_to_clipboard;
    let auto_paste = runtime_cfg.auto_paste;
    let append_newline = runtime_cfg.append_newline;
    let send_enter = runtime_cfg.send_enter;
    let audio_ducking = runtime_cfg.audio_ducking;
    let ducking_level_percent = runtime_cfg.ducking_level_percent;
    let preserve_clipboard = runtime_cfg.preserve_clipboard;
    eframe::run_native(
        "Audetic Overlay",
        native_options,
        Box::new(move |_cc| {
            let ui_cfg = OverlayAppUiConfig {
                toggle_url: toggle_url.clone(),
                start_url: start_url.clone(),
                stop_url: stop_url.clone(),
                engine_mode,
                streaming_model_label: streaming_model_label.clone(),
                streaming_source_label: streaming_source_label.clone(),
                batch_model_label: batch_model_label.clone(),
                batch_source_label: batch_source_label.clone(),
                mic_label: mic_label.clone(),
                control_mode,
                ptt_activation_delay_ms,
                copy_to_clipboard,
                auto_paste,
                append_newline,
                send_enter,
                audio_ducking,
                ducking_level_percent,
                preserve_clipboard,
                opacity,
                show_meter,
            };
            Ok(Box::new(OverlayApp::new(state.clone(), ui_cfg)))
        }),
    )
    .map_err(|err| -> DynError { Box::new(err) })?;

    Ok(())
}

fn try_acquire_overlay_lock() -> Result<Option<File>, DynError> {
    let data_dir =
        audetic::global::data_dir().map_err(|err| std::io::Error::other(err.to_string()))?;
    fs::create_dir_all(&data_dir)?;
    let lock_path = data_dir.join("overlay.lock");

    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)?;

    match lock_file.try_lock_exclusive() {
        Ok(()) => Ok(Some(lock_file)),
        Err(err) if err.kind() == ErrorKind::WouldBlock => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("audetic-overlay: {}", err);
        std::process::exit(1);
    }
}
