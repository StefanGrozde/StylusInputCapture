use std::{collections::VecDeque, path::PathBuf, time::Instant};

use eframe::egui;
use tablet_core::{DeviceCapabilities, PenSample};
use tablet_process::{CalibrationProfile, ProcessedSample, ProcessorState};
use tablet_stream::{Format, Metrics, StreamMessage};

use crate::calibration::GeometryCalibration;
use crate::cli::{format_label, Args, Source};
use crate::panels::calibration::{
    draw_calibration_panel, CalibrationAction, ProfileIoAction, ProfilePanelState,
};
use crate::panels::inspector::draw_inspector_panel;
use crate::panels::orientation::draw_orientation_panel;
use crate::panels::pressure::draw_pressure_panel;
use crate::panels::telemetry::{
    detect_tool_change, draw_telemetry_panel, proximity_event, EventLogEntry,
};
use crate::panels::trace::{draw_trace_panel, raw_mapped_point, TraceHistory, TracePoint};
use crate::prefs::UiPrefs;
use crate::source::{ConnectionStatus, SourceHandle, SourceState};
use crate::spawn::{spawn_producer, SpawnOptions};

const EVENT_LOG_CAPACITY: usize = 256;

pub struct TabletUiApp {
    source: Source,
    format: Format,
    profile_path: Option<PathBuf>,
    spawn_requested: bool,
    profile: CalibrationProfile,
    /// Snapshot of the profile as last loaded/saved, used to compute the
    /// "modified" dirty indicator in the calibration panel (`profile !=
    /// baseline_profile`), and cleared on every successful load/save (TC3.4).
    baseline_profile: CalibrationProfile,
    /// N-point geometry calibration workflow state (TC3.3): collected
    /// `(raw, target)` correspondence points and the most recent fit report.
    geometry: GeometryCalibration,
    /// Cross-frame UI bookkeeping for the profile-management controls (path
    /// text buffer, last error, confirm-on-discard) — TC3.4. Deliberately not
    /// part of `CalibrationProfile`/`UiPrefs`.
    profile_panel_state: ProfilePanelState,
    /// UI preferences (`tablet-ui.toml`), loaded at startup and persisted on
    /// exit (TC3.4, SPEC_CUI.md §7). Kept strictly separate from
    /// `CalibrationProfile`.
    prefs: UiPrefs,
    /// Most recently observed window inner size, captured each frame so
    /// `on_exit` can persist it even though `eframe` does not hand the size
    /// to the exit hook directly.
    last_window_size: Option<(f32, f32)>,
    processor_state: ProcessorState,
    source_handle: SourceHandle,
    source_state: SourceState,
    latest_capabilities: Option<DeviceCapabilities>,
    latest_metrics: Option<Metrics>,
    latest_sample: Option<PenSample>,
    latest_processed: Option<ProcessedSample>,
    trace_history: TraceHistory,
    event_log: VecDeque<EventLogEntry>,
    latest_sample_received_at: Option<Instant>,
    show_raw_overlay: bool,
}

impl TabletUiApp {
    pub fn new(args: Args, prefs: UiPrefs) -> Self {
        // `--spawn` (SPEC_CUI.md §5.2, TC3.5): launch `tablet-cli` as a child
        // producer and read its stdout as a postcard stream — orchestration
        // only, the capture pipeline is untouched. Falls back to the
        // configured `source`/`format` if spawning fails (e.g. binary not
        // found), so the UI still opens and surfaces a clear "disconnected"
        // status rather than refusing to start.
        let source_handle = if args.spawn {
            match spawn_producer(&SpawnOptions::default()) {
                Ok(producer) => match SourceHandle::spawn_child_producer(producer) {
                    Ok(handle) => handle,
                    Err(error) => {
                        eprintln!("failed to read spawned tablet-cli stdout: {error}");
                        SourceHandle::spawn(args.source.clone(), args.format)
                    }
                },
                Err(error) => {
                    eprintln!("failed to spawn tablet-cli (--spawn): {error}");
                    SourceHandle::spawn(args.source.clone(), args.format)
                }
            }
        } else {
            SourceHandle::spawn(args.source.clone(), args.format)
        };

        // `--profile` wins; fall back to the last profile path remembered in
        // preferences (SPEC_CUI.md §7 / TC3.4 acceptance criteria).
        let requested_path = args.profile.clone().or_else(|| prefs.last_profile_path.clone());

        let mut profile = CalibrationProfile::identity();
        let mut profile_path = None;
        let mut path_text = String::new();
        let mut last_error = None;

        if let Some(path) = requested_path {
            match CalibrationProfile::load(&path) {
                Ok(loaded) => {
                    path_text = path.display().to_string();
                    profile = loaded;
                    profile_path = Some(path);
                }
                Err(error) => {
                    eprintln!("failed to load startup profile '{}': {error}", path.display());
                    last_error = Some(format!("failed to load '{}': {error}", path.display()));
                    path_text = path.display().to_string();
                }
            }
        }

        let baseline_profile = profile.clone();

        Self {
            source: args.source,
            format: args.format,
            profile_path,
            spawn_requested: args.spawn,
            profile,
            baseline_profile,
            geometry: GeometryCalibration::new(),
            profile_panel_state: ProfilePanelState {
                path_text,
                last_error,
                pending_confirm: None,
            },
            prefs,
            last_window_size: None,
            processor_state: ProcessorState::new(),
            source_state: source_handle.state(),
            source_handle,
            latest_capabilities: None,
            latest_metrics: None,
            latest_sample: None,
            latest_processed: None,
            trace_history: TraceHistory::default(),
            event_log: VecDeque::with_capacity(EVENT_LOG_CAPACITY),
            latest_sample_received_at: None,
            show_raw_overlay: true,
        }
    }

    /// Apply a [`CalibrationAction`] returned by the calibration panel:
    /// reset-to-identity is already mutated in place by the panel; profile
    /// I/O (New/Load/Save/Save As) is performed here, in app.rs, so the draw
    /// function stays free of file access (TC3.4).
    fn apply_calibration_action(&mut self, action: CalibrationAction) {
        match action.profile_io {
            ProfileIoAction::None => {}
            ProfileIoAction::New => {
                self.profile = CalibrationProfile::identity();
                self.baseline_profile = self.profile.clone();
                self.profile_path = None;
                self.profile_panel_state.path_text.clear();
                self.profile_panel_state.last_error = None;
            }
            ProfileIoAction::Load(path) => match CalibrationProfile::load(&path) {
                Ok(loaded) => {
                    self.profile = loaded;
                    self.baseline_profile = self.profile.clone();
                    self.profile_panel_state.path_text = path.display().to_string();
                    self.profile_path = Some(path);
                    self.profile_panel_state.last_error = None;
                }
                Err(error) => {
                    self.profile_panel_state.last_error =
                        Some(format!("load failed: {error}"));
                }
            },
            ProfileIoAction::Save(path) => self.save_profile_to(path),
            ProfileIoAction::SaveAs(path) => self.save_profile_to(path),
        }
    }

    /// Save `self.profile` to `path`; on success clear the dirty indicator
    /// (`baseline_profile = profile.clone()`) and adopt `path` as the
    /// resolved `profile_path`. Shared by `Save` (existing path) and
    /// `Save As` (possibly new path) — both end in the same state.
    fn save_profile_to(&mut self, path: PathBuf) {
        let outcome = save_and_rebaseline(&self.profile, &path, &self.baseline_profile);
        match outcome {
            Ok(new_baseline) => {
                self.baseline_profile = new_baseline;
                self.profile_panel_state.path_text = path.display().to_string();
                self.profile_path = Some(path);
                self.profile_panel_state.last_error = None;
            }
            Err(error) => {
                self.profile_panel_state.last_error = Some(format!("save failed: {error}"));
            }
        }
    }

    fn source_label(&self) -> String {
        match &self.source {
            Source::Stdin => "stdin".to_owned(),
            Source::Tcp(addr) => format!("tcp: {addr}"),
            Source::Pipe(name) => format!("pipe: {name}"),
        }
    }

    fn drain_source(&mut self) {
        self.source_state = self.source_handle.state();
        if self.latest_capabilities.is_none() {
            self.latest_capabilities = self.source_state.latest_capabilities.clone();
        }
        if self.latest_metrics.is_none() {
            self.latest_metrics = self.source_state.latest_metrics.clone();
        }

        for message in self.source_handle.drain_messages() {
            match message {
                StreamMessage::Capabilities(caps) => self.latest_capabilities = Some(caps),
                StreamMessage::Metrics(metrics) => self.latest_metrics = Some(metrics),
                StreamMessage::Sample(sample) => {
                    if let Some(event) = detect_tool_change(self.latest_sample.as_ref(), &sample) {
                        self.push_event(event);
                    }
                    self.latest_sample_received_at = Some(Instant::now());
                    self.latest_sample = Some(sample);
                    if let Some(caps) = &self.latest_capabilities {
                        let processed =
                            self.profile.apply(&sample, caps, &mut self.processor_state);
                        let raw_mapped =
                            raw_mapped_point(&sample, caps, &self.profile.target_space);
                        self.trace_history.append(TracePoint {
                            processed: processed.clone(),
                            raw_mapped,
                        });
                        self.latest_processed = Some(processed);
                    }
                }
                StreamMessage::Proximity {
                    in_range,
                    tool_serial,
                } => self.push_event(proximity_event(in_range, tool_serial)),
                StreamMessage::Heartbeat => {}
            }
        }

        self.source_state = self.source_handle.state();
    }

    fn push_event(&mut self, event: EventLogEntry) {
        if self.event_log.len() == EVENT_LOG_CAPACITY {
            self.event_log.pop_front();
        }
        self.event_log.push_back(event);
    }

    fn status_label(status: ConnectionStatus) -> &'static str {
        match status {
            ConnectionStatus::Connecting => "connecting",
            ConnectionStatus::Connected => "connected",
            ConnectionStatus::Disconnected => "disconnected",
        }
    }

    /// Whether the active source treats EOF as terminal (no reconnect):
    /// `Stdin` and the `--spawn` child both behave this way (SPEC_CUI.md
    /// §5.3) — only `Tcp`/`Pipe` retry with backoff.
    fn is_terminal_source(spawn_requested: bool, source: &Source) -> bool {
        spawn_requested || matches!(source, Source::Stdin)
    }
}

impl eframe::App for TabletUiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The stream is live: request a repaint every frame so the panels keep
        // draining the source and animating between input events (egui is
        // otherwise event-driven and would freeze once input settles).
        ui.ctx().request_repaint();
        self.drain_source();

        // Track the current window inner size so it can be persisted on exit
        // (TC3.4 / SPEC_CUI.md §7). `eframe` doesn't hand the size to
        // `on_exit` directly, so we capture it opportunistically each frame.
        if let Some(rect) = ui.ctx().input(|i| i.viewport().inner_rect) {
            let size = rect.size();
            if size.x > 0.0 && size.y > 0.0 {
                self.last_window_size = Some((size.x, size.y));
            }
        }

        egui::Panel::top("status").show_inside(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if self.spawn_requested {
                    ui.label("source: spawned tablet-cli (child process, postcard via stdout)");
                } else {
                    ui.label(format!("source: {}", self.source_label()));
                    ui.separator();
                    ui.label(format!("format: {}", format_label(self.format)));
                }
                ui.separator();
                ui.label(format!(
                    "status: {}",
                    Self::status_label(self.source_state.status)
                ));
                if Self::is_terminal_source(self.spawn_requested, &self.source)
                    && self.source_state.status == ConnectionStatus::Disconnected
                {
                    ui.separator();
                    ui.label("(EOF is terminal for this source — no automatic reconnect)");
                }
                ui.separator();
                ui.label(format!("profile: {}", self.profile.name));
                if let Some(path) = &self.profile_path {
                    ui.separator();
                    ui.label(path.display().to_string());
                }
            });
        });

        egui::Panel::left("calibration")
            .resizable(true)
            .default_size(260.0)
            .show_inside(ui, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_min_height(120.0);
                    egui::ScrollArea::vertical()
                        .id_salt("calibration_scroll")
                        .show(ui, |ui| {
                            let latest_raw_xy = self
                                .latest_sample
                                .as_ref()
                                .map(|s| (s.x_raw as f64, s.y_raw as f64));
                            let action = draw_calibration_panel(
                                ui,
                                &mut self.profile,
                                &self.baseline_profile,
                                &mut self.geometry,
                                latest_raw_xy,
                                self.profile_path.as_deref(),
                                &mut self.profile_panel_state,
                            );
                            self.apply_calibration_action(action);
                        });
                });
            });

        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(380.0)
            .show_inside(ui, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_min_height(120.0);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        draw_inspector_panel(
                            ui,
                            self.latest_sample.as_ref(),
                            self.latest_processed.as_ref(),
                        );
                    });
                });
            });

        egui::Panel::bottom("telemetry")
            .resizable(true)
            .default_size(240.0)
            .show_inside(ui, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_min_height(120.0);
                    draw_telemetry_panel(
                        ui,
                        self.latest_metrics.as_ref(),
                        &self.source_state,
                        &self.trace_history,
                        &self.event_log,
                        self.latest_sample_received_at,
                    );
                });
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            // Pressure + orientation share a resizable strip beneath the trace,
            // which keeps the trace canvas as the primary visualization.
            let secondary_height = (ui.available_height() * 0.4).clamp(180.0, 340.0);
            egui::Panel::bottom("viz_secondary")
                .resizable(true)
                .default_size(secondary_height)
                .show_inside(ui, |ui| {
                    let trace_history = &self.trace_history;
                    let pressure = &mut self.profile.pressure;
                    let capabilities = self.latest_capabilities.as_ref();
                    crate::panels::two_columns(
                        ui,
                        |ui| draw_pressure_panel(ui, pressure, trace_history),
                        |ui| draw_orientation_panel(ui, trace_history, capabilities),
                    );
                });

            egui::CentralPanel::default().show_inside(ui, |ui| {
                draw_trace_panel(
                    ui,
                    &self.trace_history,
                    &self.profile.target_space,
                    self.latest_capabilities.as_ref(),
                    &mut self.show_raw_overlay,
                );
            });
        });
    }

    /// Persist UI preferences (`tablet-ui.toml`) once on shutdown
    /// (TC3.4 / SPEC_CUI.md §7). The `glow` feature is enabled (see
    /// `Cargo.toml`), so `on_exit` receives an optional GL context — unused
    /// here, since saving prefs needs neither rendering state nor egui.
    ///
    /// Preferences are strictly UI-level (window size, last source/format,
    /// last profile path) and are never written into a `.cal.toml`.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Signal the reader thread to stop and (if `--spawn` launched a
        // producer) kill + reap the child so no orphan process remains
        // (SPEC_CUI.md §5.2/§5.3, TC3.5 acceptance criteria). `SourceHandle`
        // also does this on `Drop`, but doing it explicitly here means
        // shutdown happens promptly, before prefs are written, rather than
        // whenever the app struct happens to be dropped.
        self.source_handle.stop();

        self.prefs.window_size = self.last_window_size.or(self.prefs.window_size);
        self.prefs.last_source = Some(self.source_label());
        self.prefs.last_format = Some(format_label(self.format).to_owned());
        self.prefs.last_profile_path = self.profile_path.clone().or_else(|| {
            self.prefs.last_profile_path.clone()
        });

        if let Err(error) = self.prefs.save() {
            eprintln!("failed to save UI preferences: {error}");
        }
    }
}

/// Save `profile` to `path` and compute the new dirty-baseline on success.
///
/// Factored out of [`TabletUiApp::save_profile_to`] so the
/// save-then-clear-dirty contract (TC3.4 acceptance: "after save the
/// baseline equals the profile") is unit-testable without constructing a
/// full `TabletUiApp` (which spawns a reader thread). On failure the
/// existing baseline is returned unchanged, mirroring
/// `apply_calibration_action`'s error handling (dirty state is preserved).
fn save_and_rebaseline(
    profile: &CalibrationProfile,
    path: &std::path::Path,
    current_baseline: &CalibrationProfile,
) -> Result<CalibrationProfile, tablet_process::ProfileError> {
    profile.save(path)?;
    let _ = current_baseline; // baseline is replaced wholesale on success
    Ok(profile.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile round-trips through `save` then `load`: values are
    /// preserved, and (per TC3.4 acceptance) saving clears the dirty
    /// indicator by making the baseline equal to the saved profile.
    #[test]
    fn save_then_load_round_trips_and_clears_dirty() {
        let dir = std::env::temp_dir().join(format!(
            "tablet-ui-profile-roundtrip-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("custom.cal.toml");

        // A non-identity profile: distinguishable from `identity()` so the
        // round-trip is meaningful.
        let mut profile = CalibrationProfile::identity();
        profile.name = "bench-rig".to_owned();
        profile.pressure.enabled = true;
        profile.pressure.pressure_min = 50;
        profile.pressure.pressure_max = 900;
        profile.activation.enabled = true;
        profile.activation.on_threshold = 0.6;
        profile.activation.off_threshold = 0.4;
        assert_ne!(profile, CalibrationProfile::identity());

        // Baseline starts out different (e.g. identity, freshly loaded
        // elsewhere) — the profile is "dirty" before saving.
        let stale_baseline = CalibrationProfile::identity();
        assert_ne!(&profile, &stale_baseline);

        // Save: mirrors `TabletUiApp::save_profile_to`'s success path.
        let new_baseline = save_and_rebaseline(&profile, &path, &stale_baseline).unwrap();
        assert_eq!(new_baseline, profile, "baseline must equal profile after save (dirty cleared)");

        // Load back from disk and confirm the values survive the round trip.
        let loaded = CalibrationProfile::load(&path).unwrap();
        assert_eq!(loaded, profile, "loaded profile must equal the saved profile");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// JSON extension round-trips too (format-by-extension, per
    /// `tablet_process::io`).
    #[test]
    fn save_then_load_round_trips_through_json() {
        let dir = std::env::temp_dir().join(format!(
            "tablet-ui-profile-roundtrip-json-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("custom.cal.json");

        let mut profile = CalibrationProfile::identity();
        profile.name = "json-rig".to_owned();
        profile.smoothing.mode = tablet_process::SmoothingMode::Ema { alpha: 0.3 };

        profile.save(&path).unwrap();
        let loaded = CalibrationProfile::load(&path).unwrap();
        assert_eq!(loaded, profile);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
