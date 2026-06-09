//! The MPE HUD application: reads the pen stream, maps it to MPE MIDI, and
//! draws a playing surface.
//!
//! Threading mirrors `tablet-ui`: a `tablet-consumer` reader thread fills a
//! display buffer; this (UI) thread drains it each frame, runs each sample
//! through the calibration profile and the [`MpeEngine`], forwards the
//! resulting [`MidiEvent`]s to [`MidiOut`], and repaints.
//!
//! Latency note: MIDI is emitted during the per-frame drain (~60 fps), the
//! same threading the calibration UI uses. The engine is pure, so a future
//! revision can move emission onto a dedicated thread fed by the ring for
//! lower jitter without touching the mapping logic.

use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use tablet_consumer::{
    spawn_producer, ConnectionStatus, Source, SourceHandle, SourceState, SpawnOptions,
};
use tablet_core::{DeviceCapabilities, PenSample};
use tablet_midi::{
    MidiEvent, MidiMapping, MpeEngine, NoteMode, ScaleKind, VelocitySource, PITCH_CLASS_NAMES,
};
use tablet_process::{CalibrationProfile, ProcessedSample, ProcessorState};
use tablet_stream::{Format, StreamMessage};

use crate::cli::{format_label, source_label, Args};
use crate::midi_out::{mpe_init_events, MidiOut};

/// Bounded ring of recent points drawn as the fading trail on the surface.
const HISTORY_CAPACITY: usize = 512;

#[derive(Clone, Copy)]
struct HudPoint {
    x: f64,
    y: f64,
    pressure: f64,
    active: bool,
}

pub struct HudApp {
    source: Source,
    format: Format,
    spawn_requested: bool,
    source_handle: SourceHandle,
    source_state: SourceState,
    latest_capabilities: Option<DeviceCapabilities>,
    latest_sample: Option<PenSample>,
    latest_processed: Option<ProcessedSample>,

    profile: CalibrationProfile,
    processor_state: ProcessorState,

    mapping: MidiMapping,
    mapping_path: Option<PathBuf>,
    mapping_path_text: String,
    mapping_status: Option<String>,

    engine: MpeEngine,
    events_scratch: Vec<MidiEvent>,

    midi: MidiOut,
    midi_ports: Vec<String>,
    selected_port: usize,
    midi_status: Option<String>,
    /// Running count of MIDI events actually sent to a connected port — a live
    /// "is anything leaving the app?" indicator in the top bar.
    events_sent: u64,
    /// A manually-triggered diagnostic note awaiting its NoteOff: (channel,
    /// note, deadline). Sent on the master channel at a loud fixed velocity so
    /// it's audible on any synth regardless of the pen / velocity mapping.
    test_note: Option<(u8, u8, Instant)>,

    history: VecDeque<HudPoint>,
}

impl HudApp {
    pub fn new(args: Args) -> Self {
        let source_handle = if args.spawn {
            match spawn_producer(&SpawnOptions::default())
                .and_then(SourceHandle::spawn_child_producer)
            {
                Ok(handle) => handle,
                Err(error) => {
                    eprintln!("failed to spawn tablet-cli (--spawn): {error}");
                    SourceHandle::spawn(args.source.clone(), args.format)
                }
            }
        } else {
            SourceHandle::spawn(args.source.clone(), args.format)
        };

        let profile = args
            .profile
            .as_deref()
            .and_then(|p| match CalibrationProfile::load(p) {
                Ok(profile) => Some(profile),
                Err(error) => {
                    eprintln!("failed to load profile '{}': {error}", p.display());
                    None
                }
            })
            .unwrap_or_else(CalibrationProfile::identity);

        let (mapping, mapping_path, mapping_status) = match args.mapping {
            Some(path) => match MidiMapping::load(&path) {
                Ok(mapping) => (mapping, Some(path), None),
                Err(error) => (
                    MidiMapping::default(),
                    Some(path),
                    Some(format!("load failed: {error}")),
                ),
            },
            None => (MidiMapping::default(), None, None),
        };
        let mapping_path_text = mapping_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        Self {
            source_state: source_handle.state(),
            source: args.source,
            format: args.format,
            spawn_requested: args.spawn,
            source_handle,
            latest_capabilities: None,
            latest_sample: None,
            latest_processed: None,
            profile,
            processor_state: ProcessorState::new(),
            mapping,
            mapping_path,
            mapping_path_text,
            mapping_status,
            engine: MpeEngine::new(),
            events_scratch: Vec::new(),
            midi: MidiOut::new(),
            midi_ports: MidiOut::list_ports(),
            selected_port: 0,
            midi_status: None,
            events_sent: 0,
            test_note: None,
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
        }
    }

    /// Drain the stream, process each sample to MPE MIDI, and emit it.
    fn drain_source(&mut self) {
        self.source_state = self.source_handle.state();
        if self.latest_capabilities.is_none() {
            self.latest_capabilities = self.source_state.latest_capabilities.clone();
        }

        for message in self.source_handle.drain_messages() {
            match message {
                StreamMessage::Capabilities(caps) => self.latest_capabilities = Some(caps),
                StreamMessage::Sample(sample) => self.handle_sample(sample),
                StreamMessage::Proximity { .. }
                | StreamMessage::Metrics(_)
                | StreamMessage::Heartbeat => {}
            }
        }

        self.source_state = self.source_handle.state();
    }

    fn handle_sample(&mut self, sample: PenSample) {
        self.latest_sample = Some(sample);
        let Some(caps) = self.latest_capabilities.as_ref() else {
            return; // can't process position without axis ranges yet
        };

        let processed = self
            .profile
            .apply(&sample, caps, &mut self.processor_state);

        self.events_scratch.clear();
        self.engine
            .process(&processed, &self.mapping, &mut self.events_scratch);
        for event in &self.events_scratch {
            self.midi.send(event);
        }
        if self.midi.is_connected() {
            self.events_sent += self.events_scratch.len() as u64;
        }

        if self.history.len() == HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(HudPoint {
            x: processed.x,
            y: processed.y,
            pressure: processed.pressure,
            active: processed.active,
        });
        self.latest_processed = Some(processed);
    }

    fn status_label(status: ConnectionStatus) -> &'static str {
        match status {
            ConnectionStatus::Connecting => "connecting",
            ConnectionStatus::Connected => "connected",
            ConnectionStatus::Disconnected => "disconnected",
        }
    }

    /// Connect MIDI and push the MPE setup messages so the receiver enters MPE
    /// mode immediately. Shared by the port and virtual-port buttons.
    fn after_connect(&mut self) {
        self.engine = MpeEngine::new();
        for event in mpe_init_events(&self.mapping) {
            self.midi.send(&event);
        }
        self.midi_status = self
            .midi
            .port_label()
            .map(|l| format!("connected: {l}"));
    }

    fn panic_all_notes_off(&mut self) {
        self.end_test_note();
        self.events_scratch.clear();
        self.engine
            .all_notes_off(&self.mapping, &mut self.events_scratch);
        for event in &self.events_scratch {
            self.midi.send(event);
        }
    }

    /// Fire a fixed, loud middle-C on the master channel to prove the MIDI path
    /// end to end, independent of the pen and the velocity mapping. Its NoteOff
    /// is sent shortly after by [`service_test_note`].
    fn send_test_note(&mut self) {
        // Release any previous, still-pending test note first.
        self.end_test_note();
        let channel = self.mapping.mpe.master_channel;
        let note = 60; // middle C
        self.midi.send(&MidiEvent::NoteOn {
            channel,
            note,
            velocity: 100,
        });
        self.events_sent += 1;
        self.test_note = Some((channel, note, Instant::now() + Duration::from_millis(500)));
    }

    /// Send the pending test note's NoteOff once its deadline passes.
    fn service_test_note(&mut self) {
        if let Some((channel, note, deadline)) = self.test_note {
            if Instant::now() >= deadline {
                self.midi.send(&MidiEvent::NoteOff {
                    channel,
                    note,
                    velocity: 0,
                });
                self.events_sent += 1;
                self.test_note = None;
            }
        }
    }

    fn end_test_note(&mut self) {
        if let Some((channel, note, _)) = self.test_note.take() {
            self.midi.send(&MidiEvent::NoteOff {
                channel,
                note,
                velocity: 0,
            });
        }
    }

    // ── Panels ────────────────────────────────────────────────────────────

    fn draw_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let source = if self.spawn_requested {
                "spawned tablet-cli".to_owned()
            } else {
                source_label(&self.source)
            };
            ui.label(format!("stream: {source}"));
            ui.separator();
            ui.label(format!("[{}]", format_label(self.format)));
            ui.separator();
            ui.label(format!(
                "status: {}",
                Self::status_label(self.source_state.status)
            ));
            ui.separator();

            // MIDI port selection + connection controls.
            egui::ComboBox::from_id_salt("midi_port")
                .selected_text(
                    self.midi_ports
                        .get(self.selected_port)
                        .cloned()
                        .unwrap_or_else(|| "(no ports)".to_owned()),
                )
                .show_ui(ui, |ui| {
                    for (index, name) in self.midi_ports.iter().enumerate() {
                        ui.selectable_value(&mut self.selected_port, index, name);
                    }
                });
            if ui.button("Refresh").clicked() {
                self.midi_ports = MidiOut::list_ports();
            }
            if ui.button("Connect").clicked() {
                match self.midi.connect_index(self.selected_port) {
                    Ok(()) => self.after_connect(),
                    Err(error) => self.midi_status = Some(format!("connect failed: {error}")),
                }
            }
            if MidiOut::virtual_supported() && ui.button("Virtual port").clicked() {
                match self.midi.connect_virtual() {
                    Ok(()) => self.after_connect(),
                    Err(error) => self.midi_status = Some(format!("virtual failed: {error}")),
                }
            }
            if self.midi.is_connected() && ui.button("Disconnect").clicked() {
                self.panic_all_notes_off();
                self.midi.disconnect();
                self.midi_status = Some("disconnected".to_owned());
            }
            ui.separator();
            if ui.button("Panic").clicked() {
                self.panic_all_notes_off();
            }
            if ui
                .add_enabled(self.midi.is_connected(), egui::Button::new("Test note"))
                .on_hover_text("Send a loud middle-C on the master channel to verify audio output")
                .clicked()
            {
                self.send_test_note();
            }
            ui.separator();
            ui.label(format!("tx: {}", self.events_sent));
        });

        if let Some(status) = &self.midi_status {
            ui.weak(status);
        }
    }

    fn draw_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.heading("Mapping");
        ui.add_space(4.0);
        ui.text_edit_singleline(&mut self.mapping.name);
        ui.separator();

        // ── Pitch ──
        ui.label("Scale & key");
        egui::ComboBox::from_id_salt("scale")
            .selected_text(self.mapping.scale.label())
            .show_ui(ui, |ui| {
                for kind in ScaleKind::ALL {
                    ui.selectable_value(&mut self.mapping.scale, kind, kind.label());
                }
            });
        egui::ComboBox::from_id_salt("key")
            .selected_text(PITCH_CLASS_NAMES[(self.mapping.key % 12) as usize])
            .show_ui(ui, |ui| {
                for (pc, name) in PITCH_CLASS_NAMES.iter().enumerate() {
                    ui.selectable_value(&mut self.mapping.key, pc as u8, *name);
                }
            });
        ui.add(egui::Slider::new(&mut self.mapping.low_note, 0..=96).text("low note"));
        ui.add(egui::Slider::new(&mut self.mapping.span_notes, 1..=48).text("span (semitones)"));
        ui.label("Note motion (what dragging sideways does)");
        egui::ComboBox::from_id_salt("note_mode")
            .selected_text(self.mapping.mode.label())
            .show_ui(ui, |ui| {
                for mode in NoteMode::ALL {
                    ui.selectable_value(&mut self.mapping.mode, mode, mode.label());
                }
            });
        ui.separator();

        // ── MPE ──
        ui.label("MPE");
        ui.add(
            egui::Slider::new(&mut self.mapping.mpe.pitch_bend_range_semitones, 1.0..=48.0)
                .text("bend range (st)"),
        );
        ui.add(egui::Slider::new(&mut self.mapping.mpe.member_low, 1..=15).text("member low"));
        ui.add(egui::Slider::new(&mut self.mapping.mpe.member_high, 1..=15).text("member high"));
        if self.mapping.mpe.member_low > self.mapping.mpe.member_high {
            self.mapping.mpe.member_high = self.mapping.mpe.member_low;
        }
        ui.separator();

        // ── Expression axes ──
        ui.label("Expression");
        ui.checkbox(&mut self.mapping.y_to_cc74, "Y → CC74 (timbre)");
        ui.add_enabled(
            self.mapping.y_to_cc74,
            egui::Checkbox::new(&mut self.mapping.y_invert, "invert Y (up = brighter)"),
        );
        ui.checkbox(
            &mut self.mapping.pressure_to_channel_pressure,
            "pressure → channel pressure",
        );
        ui.separator();

        // ── Velocity ──
        ui.label("Velocity");
        let mut from_pressure = matches!(self.mapping.velocity, VelocitySource::Pressure { .. });
        if ui
            .radio(from_pressure, "from pressure at note-on")
            .clicked()
        {
            from_pressure = true;
        }
        if ui.radio(!from_pressure, "fixed").clicked() {
            from_pressure = false;
        }
        match (&mut self.mapping.velocity, from_pressure) {
            (VelocitySource::Pressure { .. }, false) => {
                self.mapping.velocity = VelocitySource::Fixed(100);
            }
            (VelocitySource::Fixed(_), true) => {
                self.mapping.velocity = VelocitySource::Pressure { min: 1, max: 127 };
            }
            _ => {}
        }
        match &mut self.mapping.velocity {
            VelocitySource::Fixed(v) => {
                ui.add(egui::Slider::new(v, 1..=127).text("velocity"));
            }
            VelocitySource::Pressure { min, max } => {
                ui.add(egui::Slider::new(min, 1..=127).text("min"));
                ui.add(egui::Slider::new(max, 1..=127).text("max"));
                if *min > *max {
                    *max = *min;
                }
            }
        }
        ui.separator();

        // ── Activation (reuses the calibration profile's stage) ──
        ui.label("Activation");
        ui.checkbox(
            &mut self.profile.activation.enabled,
            "derive contact from pressure",
        );
        if self.profile.activation.enabled {
            ui.add(
                egui::Slider::new(&mut self.profile.activation.on_threshold, 0.0..=1.0)
                    .text("on threshold"),
            );
            ui.add(
                egui::Slider::new(&mut self.profile.activation.off_threshold, 0.0..=1.0)
                    .text("off threshold"),
            );
            if self.profile.activation.off_threshold > self.profile.activation.on_threshold {
                self.profile.activation.off_threshold = self.profile.activation.on_threshold;
            }
        }
        ui.separator();

        // ── Profile / mapping I/O ──
        ui.label("Mapping file");
        ui.text_edit_singleline(&mut self.mapping_path_text);
        ui.horizontal(|ui| {
            if ui.button("Load").clicked() {
                self.load_mapping();
            }
            if ui.button("Save").clicked() {
                self.save_mapping();
            }
        });
        if let Some(status) = &self.mapping_status {
            ui.weak(status);
        }
    }

    fn load_mapping(&mut self) {
        let path = PathBuf::from(self.mapping_path_text.trim());
        if path.as_os_str().is_empty() {
            self.mapping_status = Some("enter a path first".to_owned());
            return;
        }
        match MidiMapping::load(&path) {
            Ok(mapping) => {
                self.mapping = mapping;
                self.mapping_path = Some(path);
                self.mapping_status = Some("loaded".to_owned());
            }
            Err(error) => self.mapping_status = Some(format!("load failed: {error}")),
        }
    }

    fn save_mapping(&mut self) {
        let path = PathBuf::from(self.mapping_path_text.trim());
        if path.as_os_str().is_empty() {
            self.mapping_status = Some("enter a path first".to_owned());
            return;
        }
        match self.mapping.validate().and_then(|()| self.mapping.save(&path)) {
            Ok(()) => {
                self.mapping_path = Some(path);
                self.mapping_status = Some("saved".to_owned());
            }
            Err(error) => self.mapping_status = Some(format!("save failed: {error}")),
        }
    }

    fn draw_surface(&self, ui: &mut egui::Ui) {
        let available = ui.available_size_before_wrap();
        let desired = Vec2::new(available.x.max(240.0), available.y.max(240.0));
        let (rect, _response) = ui.allocate_exact_size(desired, Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            egui::StrokeKind::Inside,
        );

        if self.latest_capabilities.is_none() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "waiting for capture stream…",
                FontId::proportional(15.0),
                ui.visuals().weak_text_color(),
            );
            return;
        }

        self.draw_note_lanes(&painter, rect, ui);
        self.draw_trail(&painter, rect);
        self.draw_cursor(&painter, rect);
    }

    /// Vertical lanes for each in-scale note across X, with the sounding note
    /// highlighted.
    fn draw_note_lanes(&self, painter: &egui::Painter, rect: Rect, ui: &egui::Ui) {
        let low = self.mapping.low_note;
        let span = self.mapping.span_notes.max(1);
        let intervals = self.mapping.scale.intervals();
        let key = self.mapping.key % 12;
        let active = self.engine.active_note();
        let line_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
        let faint = Color32::from_rgba_unmultiplied(line_color.r(), line_color.g(), line_color.b(), 60);

        for offset in 0..=span {
            let note = low as i32 + offset as i32;
            if note > 127 {
                break;
            }
            let pc = ((note - i32::from(key)).rem_euclid(12)) as u8;
            if !intervals.contains(&pc) {
                continue;
            }
            let t = f32::from(offset) / f32::from(span);
            let x = rect.left() + t * rect.width();
            let is_active = active == Some(note as u8);
            let stroke_color = if is_active {
                Color32::from_rgb(90, 200, 255)
            } else {
                faint
            };
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(if is_active { 2.5 } else { 1.0 }, stroke_color),
            );
            // Label root notes (C, or scale root) with their name + octave.
            if pc == 0 || note as u8 == active.unwrap_or(255) {
                let name = PITCH_CLASS_NAMES[(note.rem_euclid(12)) as usize];
                let octave = note / 12 - 1; // MIDI: C4 = 60
                painter.text(
                    Pos2::new(x + 3.0, rect.bottom() - 4.0),
                    Align2::LEFT_BOTTOM,
                    format!("{name}{octave}"),
                    FontId::proportional(11.0),
                    if is_active {
                        Color32::from_rgb(140, 220, 255)
                    } else {
                        ui.visuals().weak_text_color()
                    },
                );
            }
        }
    }

    fn draw_trail(&self, painter: &egui::Painter, rect: Rect) {
        let count = self.history.len().max(1);
        for (idx, point) in self.history.iter().enumerate() {
            if !point.active {
                continue;
            }
            let age = (idx + 1) as f32 / count as f32;
            let alpha = (20.0 + 150.0 * age).round() as u8;
            let pos = surface_pos(point.x, point.y, rect);
            let radius = 1.5 + 4.0 * point.pressure as f32;
            painter.circle_filled(
                pos,
                radius,
                Color32::from_rgba_unmultiplied(80, 190, 255, alpha),
            );
        }
    }

    fn draw_cursor(&self, painter: &egui::Painter, rect: Rect) {
        let Some(processed) = &self.latest_processed else {
            return;
        };
        let pos = surface_pos(processed.x, processed.y, rect);
        let color = if processed.active {
            Color32::from_rgb(255, 220, 90)
        } else {
            Color32::from_rgb(150, 150, 160)
        };
        let radius = 4.0 + 8.0 * processed.pressure as f32;
        painter.circle_stroke(pos, radius, Stroke::new(2.0, color));
        painter.circle_filled(pos, 2.0, color);

        // Horizontal CC74 (timbre) reference line at the cursor's Y.
        if self.mapping.y_to_cc74 {
            let faint = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 60);
            painter.line_segment(
                [
                    Pos2::new(rect.left(), pos.y),
                    Pos2::new(rect.right(), pos.y),
                ],
                Stroke::new(1.0, faint),
            );
        }
    }
}

/// Map a normalized `(x, y)` in `[0, 1]` (target space) to a pixel position in
/// `rect`. Y grows downward, matching screen and digitizer-Y convention.
fn surface_pos(x: f64, y: f64, rect: Rect) -> Pos2 {
    Pos2::new(
        rect.left() + (x.clamp(0.0, 1.0) as f32) * rect.width(),
        rect.top() + (y.clamp(0.0, 1.0) as f32) * rect.height(),
    )
}

impl eframe::App for HudApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Live stream: repaint every frame so we keep draining and animating.
        ui.ctx().request_repaint();
        self.drain_source();
        self.service_test_note();

        egui::Panel::top("hud_top").show_inside(ui, |ui| self.draw_top_bar(ui));

        egui::Panel::left("hud_sidebar")
            .resizable(true)
            .default_size(280.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("sidebar_scroll")
                    .show(ui, |ui| self.draw_sidebar(ui));
            });

        egui::CentralPanel::default().show_inside(ui, |ui| self.draw_surface(ui));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Silence anything still sounding, then stop the reader thread (and
        // reap a `--spawn`ed producer).
        self.panic_all_notes_off();
        self.midi.disconnect();
        self.source_handle.stop();
    }
}
