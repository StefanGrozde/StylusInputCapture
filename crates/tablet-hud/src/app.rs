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

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use tablet_consumer::{
    spawn_producer, ConnectionStatus, Source, SourceHandle, SourceState, SpawnOptions,
};
use tablet_core::{DeviceCapabilities, PenSample};
use tablet_midi::{
    is_root, pad_at, pad_note, MidiEvent, MidiMapping, MpeEngine, NoteMode, Pad, ScaleKind,
    TiltAxis, TiltCc, VelocitySource, PITCH_BEND_CENTER, PITCH_CLASS_NAMES,
};
use tablet_process::{CalibrationProfile, ProcessedSample, ProcessorState};
use tablet_stream::{Format, StreamMessage};

use crate::cli::{format_label, source_label, Args};
use crate::midi_out::{mpe_init_events, MidiOut};
use crate::prefs::HudPrefs;
use crate::theme;

/// Windows default MIDI synth — present on most systems but ignores MPE.
const GS_WAVETABLE_SUBSTR: &str = "Microsoft GS Wavetable Synth";

/// Bounded ring of recent points drawn as the fading trail on the surface.
const HISTORY_CAPACITY: usize = 512;

/// Bounded ring of recent NoteOn/NoteOff entries in the Activity section.
const ACTIVITY_CAPACITY: usize = 64;

/// How long the white note-on flash takes to fade out.
const FLASH_DURATION: Duration = Duration::from_millis(150);

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
    /// Last connect/disconnect outcome; `true` marks an error (drawn red).
    midi_status: Option<(String, bool)>,
    /// Running count of MIDI events actually sent to a connected port — a live
    /// "is anything leaving the app?" indicator in the top bar.
    events_sent: u64,
    /// Events-per-second over the last rate window, shown beside the counter.
    tx_rate: f32,
    tx_at_rate_tick: u64,
    rate_tick: Instant,
    /// A manually-triggered diagnostic note awaiting its NoteOff: (channel,
    /// note, deadline). Sent on the master channel at a loud fixed velocity so
    /// it's audible on any synth regardless of the pen / velocity mapping.
    test_note: Option<(u8, u8, Instant)>,

    /// Recent NoteOn/NoteOff events for the Activity section (CC/bend/pressure
    /// are skipped — they arrive at capture rate and would flood the log).
    activity: VecDeque<(Instant, String)>,
    /// Pad that just received a NoteOn, overlaid white and fading out.
    flash: Option<(Pad, Instant)>,

    history: VecDeque<HudPoint>,

    prefs: HudPrefs,
    /// Window inner size captured each frame so `on_exit` can persist it.
    last_window_size: Option<(f32, f32)>,
}

impl HudApp {
    pub fn new(args: Args, prefs: HudPrefs) -> Self {
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
            .unwrap_or_else(|| {
                // Contact must come from pressure, not proximity: Wacom pens
                // report in_proximity while merely hovering, which would
                // consume the note-on edge before the pad is ever touched.
                let mut p = CalibrationProfile::identity();
                p.activation.enabled = true;
                p.activation.on_threshold = 0.04;
                p.activation.off_threshold = 0.02;
                p
            });

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
        // Prefill the path field from prefs when `--mapping` wasn't given —
        // select only, the user still decides whether to load it.
        let mapping_path_text = mapping_path
            .as_ref()
            .or(prefs.last_mapping_path.as_ref())
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        let midi_ports = MidiOut::list_ports();
        // Preselect the remembered port if it still exists (never auto-connect
        // unless `--midi-port` was given — pushing MPE init unprompted is surprising).
        let selected_port = prefs
            .last_midi_port
            .as_ref()
            .and_then(|last| midi_ports.iter().position(|name| name == last))
            .unwrap_or(0);

        let mut app = Self {
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
            midi_ports,
            selected_port,
            midi_status: None,
            events_sent: 0,
            tx_rate: 0.0,
            tx_at_rate_tick: 0,
            rate_tick: Instant::now(),
            test_note: None,
            activity: VecDeque::with_capacity(ACTIVITY_CAPACITY),
            flash: None,
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
            prefs,
            last_window_size: None,
        };

        if let Some(substring) = args.midi_port.as_deref() {
            app.try_connect_by_name(substring);
        }

        app
    }

    /// Re-enumerate MIDI ports and keep [`selected_port`] aligned with the
    /// previously selected port name when it still exists.
    fn refresh_midi_ports(&mut self) {
        let prev_name = self.midi_ports.get(self.selected_port).cloned();
        self.midi_ports = MidiOut::list_ports();
        if let Some(name) = prev_name {
            if let Some(index) = self.midi_ports.iter().position(|p| p == &name) {
                self.selected_port = index;
            } else if self.selected_port >= self.midi_ports.len() {
                self.selected_port = 0;
            }
        } else if self.selected_port >= self.midi_ports.len() {
            self.selected_port = 0;
        }
    }

    /// Connect to the first port whose name contains `substring`.
    fn try_connect_by_name(&mut self, substring: &str) {
        let Some(index) = MidiOut::find_port_index_by_name(substring) else {
            self.midi_status = Some((format!("no MIDI port matching '{substring}'"), true));
            return;
        };
        self.selected_port = index;
        match self.midi.connect_index(index) {
            Ok(()) => self.after_connect(),
            Err(error) => self.midi_status = Some((format!("connect failed: {error}"), true)),
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
        // Take the scratch buffer so note events can be logged (`push_activity`
        // needs `&mut self`) while iterating; it is put back below, keeping the
        // no-allocation steady state.
        let mut events = std::mem::take(&mut self.events_scratch);
        self.engine.process(&processed, &self.mapping, &mut events);
        let mut note_on = false;
        for event in &events {
            self.midi.send(event);
            match event {
                MidiEvent::NoteOn {
                    channel,
                    note,
                    velocity,
                } => {
                    self.push_activity(format!(
                        "▶ {} ch{channel} v{velocity}",
                        note_label(*note)
                    ));
                    note_on = true;
                }
                MidiEvent::NoteOff { channel, note, .. } => {
                    self.push_activity(format!("■ {} ch{channel}", note_label(*note)));
                }
                _ => {}
            }
        }
        if self.midi.is_connected() {
            self.events_sent += events.len() as u64;
        }
        self.events_scratch = events;
        if note_on {
            // The voice the engine just started is the pad to flash.
            if let Some(pad) = self.engine.active_pad() {
                self.flash = Some((pad, Instant::now()));
            }
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

    fn push_activity(&mut self, entry: String) {
        if self.activity.len() == ACTIVITY_CAPACITY {
            self.activity.pop_front();
        }
        self.activity.push_back((Instant::now(), entry));
    }

    /// Dot color + label for the stream-status pill.
    fn status_visual(status: ConnectionStatus) -> (Color32, &'static str) {
        match status {
            ConnectionStatus::Connecting => (theme::STATUS_WARN, "connecting"),
            ConnectionStatus::Connected => (theme::STATUS_OK, "connected"),
            ConnectionStatus::Disconnected => (theme::STATUS_ERR, "disconnected"),
        }
    }

    /// Shift the whole grid by `delta` octaves, clamped to the slider range.
    fn octave_shift(&mut self, delta: i32) {
        let shifted = i32::from(self.mapping.low_note) + delta * 12;
        self.mapping.low_note = shifted.clamp(0, 96) as u8;
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
            .map(|l| (format!("connected: {l}"), false));
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
        // Refresh the events-per-second readout once a second.
        let window = self.rate_tick.elapsed();
        if window >= Duration::from_secs(1) {
            let delta = self.events_sent - self.tx_at_rate_tick;
            self.tx_rate = delta as f32 / window.as_secs_f32();
            self.tx_at_rate_tick = self.events_sent;
            self.rate_tick = Instant::now();
        }

        ui.horizontal_wrapped(|ui| {
            // Stream pill: ● + source/format, colored by connection status.
            let source = if self.spawn_requested {
                "spawned tablet-cli".to_owned()
            } else {
                source_label(&self.source)
            };
            let (color, _) = Self::status_visual(self.source_state.status);
            theme::status_dot(
                ui,
                color,
                &format!("{source} [{}]", format_label(self.format)),
            );
            ui.separator();

            // MIDI pill + port selection + connection controls.
            if let Some(label) = self.midi.port_label() {
                let label = label.to_owned();
                theme::status_dot(ui, theme::STATUS_OK, &label);
            } else {
                theme::status_dot(ui, theme::STATUS_IDLE, "no MIDI");
            }
            egui::ComboBox::from_id_salt("midi_port")
                .selected_text(
                    self.midi_ports
                        .get(self.selected_port)
                        .cloned()
                        .unwrap_or_else(|| "(no ports)".to_owned()),
                )
                .show_ui(ui, |ui| {
                    // Popup is open — pick up ports created since last frame.
                    self.refresh_midi_ports();
                    for (index, name) in self.midi_ports.iter().enumerate() {
                        ui.selectable_value(&mut self.selected_port, index, name);
                    }
                });
            if ui.button("⟳").on_hover_text("Refresh the port list").clicked() {
                self.refresh_midi_ports();
            }
            if ui.button("Connect").clicked() {
                match self.midi.connect_index(self.selected_port) {
                    Ok(()) => self.after_connect(),
                    Err(error) => {
                        self.midi_status = Some((format!("connect failed: {error}"), true));
                    }
                }
            }
            if MidiOut::virtual_supported() && ui.button("Virtual port").clicked() {
                match self.midi.connect_virtual() {
                    Ok(()) => self.after_connect(),
                    Err(error) => {
                        self.midi_status = Some((format!("virtual failed: {error}"), true));
                    }
                }
            }
            if self.midi.is_connected() && ui.button("Disconnect").clicked() {
                self.panic_all_notes_off();
                self.midi.disconnect();
                self.midi_status = Some(("disconnected".to_owned(), false));
            }
            ui.separator();
            if ui
                .button("Panic")
                .on_hover_text("Release everything: NoteOff + All-Notes-Off sweep (Esc)")
                .clicked()
            {
                self.panic_all_notes_off();
            }
            if ui
                .add_enabled(self.midi.is_connected(), egui::Button::new("Test note"))
                .on_hover_text(
                    "Send a loud middle-C on the master channel to verify audio output (T)",
                )
                .clicked()
            {
                self.send_test_note();
            }

            // Right-aligned tx counter + rate.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("tx: {} ({:.0}/s)", self.events_sent, self.tx_rate))
                        .color(if self.midi.is_connected() {
                            theme::ACCENT_DIM
                        } else {
                            theme::STATUS_IDLE
                        }),
                );
            });
        });

        if let Some((status, is_error)) = &self.midi_status {
            if *is_error {
                ui.colored_label(theme::STATUS_ERR, status);
            } else {
                ui.weak(status);
            }
        }
        if self.midi_ports.len() == 1
            && self
                .midi_ports
                .first()
                .is_some_and(|name| name.contains(GS_WAVETABLE_SUBSTR))
        {
            ui.weak(
                "Microsoft GS Wavetable Synth does not support MPE — install loopMIDI \
                 and route to Surge XT (or another MPE-capable synth).",
            );
        }
    }

    fn draw_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.heading("Mapping");
        ui.add_space(4.0);
        ui.text_edit_singleline(&mut self.mapping.name);
        ui.add_space(4.0);

        egui::CollapsingHeader::new("Pitch & scale")
            .default_open(true)
            .show(ui, |ui| {
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
                ui.add(
                    egui::Slider::new(&mut self.mapping.low_note, 0..=96)
                        .text("low note (bottom-left pad)"),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button("− oct")
                        .on_hover_text("Shift the grid down an octave (PageDown)")
                        .clicked()
                    {
                        self.octave_shift(-1);
                    }
                    if ui
                        .button("+ oct")
                        .on_hover_text("Shift the grid up an octave (PageUp)")
                        .clicked()
                    {
                        self.octave_shift(1);
                    }
                    ui.colored_label(theme::ACCENT_DIM, note_label(self.mapping.low_note));
                });
            });

        egui::CollapsingHeader::new("Pad grid")
            .default_open(true)
            .show(ui, |ui| {
                ui.add(egui::Slider::new(&mut self.mapping.grid.rows, 1..=12).text("rows"));
                ui.add(egui::Slider::new(&mut self.mapping.grid.cols, 1..=12).text("columns"));
                ui.add(
                    egui::Slider::new(&mut self.mapping.grid.row_interval_degrees, 1..=7)
                        .text("row interval (scale degrees, 3 = in 4ths)"),
                );
                ui.label("Slide behavior (what sliding onto another pad does)");
                egui::ComboBox::from_id_salt("note_mode")
                    .selected_text(self.mapping.mode.label())
                    .show_ui(ui, |ui| {
                        for mode in NoteMode::ALL {
                            ui.selectable_value(&mut self.mapping.mode, mode, mode.label());
                        }
                    });
            });

        egui::CollapsingHeader::new("MPE").default_open(false).show(ui, |ui| {
            ui.add(
                egui::Slider::new(&mut self.mapping.mpe.pitch_bend_range_semitones, 1.0..=48.0)
                    .text("bend range (st)"),
            );
            ui.add(egui::Slider::new(&mut self.mapping.mpe.member_low, 1..=15).text("member low"));
            ui.add(
                egui::Slider::new(&mut self.mapping.mpe.member_high, 1..=15).text("member high"),
            );
            if self.mapping.mpe.member_low > self.mapping.mpe.member_high {
                self.mapping.mpe.member_high = self.mapping.mpe.member_low;
            }
        });

        egui::CollapsingHeader::new("Expression")
            .default_open(true)
            .show(ui, |ui| {
                ui.add(
                    egui::Slider::new(&mut self.mapping.y_bend_semitones, 0.0..=48.0)
                        .text("drag up/down → bend (st per surface height)"),
                );
                ui.checkbox(
                    &mut self.mapping.x_to_cc74,
                    "drag left/right → CC74 (timbre/frequency)",
                );
                ui.checkbox(
                    &mut self.mapping.pressure_to_channel_pressure,
                    "pressure → channel pressure",
                );
                let mut tilt_enabled = self.mapping.tilt_cc.is_some();
                if ui
                    .checkbox(&mut tilt_enabled, "tilt → CC (mod wheel by default)")
                    .changed()
                {
                    self.mapping.tilt_cc = tilt_enabled.then_some(TiltCc {
                        controller: 1,
                        axis: TiltAxis::X,
                        range_deg: 60.0,
                    });
                }
                if let Some(tilt) = &mut self.mapping.tilt_cc {
                    ui.add(egui::Slider::new(&mut tilt.controller, 0..=127).text("controller"));
                    ui.horizontal(|ui| {
                        ui.label("axis");
                        ui.selectable_value(&mut tilt.axis, TiltAxis::X, "tilt X");
                        ui.selectable_value(&mut tilt.axis, TiltAxis::Y, "tilt Y");
                    });
                    ui.add(
                        egui::Slider::new(&mut tilt.range_deg, 10.0..=90.0)
                            .text("full-scale tilt (°)"),
                    );
                }
            });

        egui::CollapsingHeader::new("Velocity")
            .default_open(false)
            .show(ui, |ui| {
                let mut from_pressure =
                    matches!(self.mapping.velocity, VelocitySource::Pressure { .. });
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
            });

        // Activation reuses the calibration profile's stage.
        egui::CollapsingHeader::new("Activation")
            .default_open(false)
            .show(ui, |ui| {
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
                    if self.profile.activation.off_threshold > self.profile.activation.on_threshold
                    {
                        self.profile.activation.off_threshold =
                            self.profile.activation.on_threshold;
                    }
                }
            });

        egui::CollapsingHeader::new("Mapping file")
            .default_open(false)
            .show(ui, |ui| {
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
                    let color = if status.contains("failed") {
                        theme::STATUS_ERR
                    } else if status == "loaded" || status == "saved" {
                        theme::STATUS_OK
                    } else {
                        theme::STATUS_WARN
                    };
                    ui.colored_label(color, status);
                }
            });

        egui::CollapsingHeader::new("Activity")
            .default_open(false)
            .show(ui, |ui| {
                if self.activity.is_empty() {
                    ui.weak("no note events yet");
                }
                // Newest first.
                for (when, entry) in self.activity.iter().rev() {
                    ui.horizontal(|ui| {
                        ui.label(entry);
                        ui.weak(format!("{:.0}s", when.elapsed().as_secs_f32()));
                    });
                }
            });
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

        self.draw_pad_grid(&painter, rect, ui);
        self.draw_trail(&painter, rect);
        self.draw_cursor(&painter, rect);
    }

    /// The Push-style pad grid: one rounded square per note, root pads in the
    /// accent color, and the sounding pad lit with a brightness that follows
    /// the current pen pressure.
    ///
    /// The grid covers the whole surface rect with the same normalized→pixel
    /// mapping as [`surface_pos`], so the pads drawn here are exactly the pads
    /// the engine hit-tests.
    fn draw_pad_grid(&self, painter: &egui::Painter, rect: Rect, ui: &egui::Ui) {
        let rows = self.mapping.grid.rows.max(1);
        let cols = self.mapping.grid.cols.max(1);
        let cell_w = rect.width() / f32::from(cols);
        let cell_h = rect.height() / f32::from(rows);
        let gap = (cell_w.min(cell_h) * 0.06).clamp(1.0, 4.0);
        let rounding = (cell_w.min(cell_h) * 0.12).clamp(2.0, 8.0);
        let active_pad = self.engine.active_pad();
        let pressure = self
            .latest_processed
            .as_ref()
            .map(|p| p.pressure as f32)
            .unwrap_or(0.0);
        let show_labels = cell_w >= 34.0 && cell_h >= 22.0;

        // Pad under a hovering (not pressing) pen: outlined so you can aim
        // before striking.
        let hover_pad = self
            .latest_processed
            .as_ref()
            .filter(|p| !p.active)
            .map(|p| pad_at(p.x, p.y, &self.mapping.grid));

        // Note-on flash, fading out over FLASH_DURATION (strength 1 → 0).
        let flash = self.flash.and_then(|(pad, started)| {
            let t = started.elapsed().as_secs_f32() / FLASH_DURATION.as_secs_f32();
            (t < 1.0).then_some((pad, 1.0 - t))
        });

        for row in 0..rows {
            for col in 0..cols {
                let pad = Pad { row, col };
                // Row 0 is the bottom row; screen Y grows downward.
                let x0 = rect.left() + f32::from(col) * cell_w;
                let y0 = rect.top() + f32::from(rows - 1 - row) * cell_h;
                let cell = Rect::from_min_size(Pos2::new(x0, y0), Vec2::new(cell_w, cell_h))
                    .shrink(gap * 0.5);

                let note = pad_note(pad, &self.mapping);
                let is_active = active_pad == Some(pad);
                let root = is_root(pad, &self.mapping);

                let (fill, stroke) = if note.is_none() {
                    // Off the top of the MIDI range: dead pad.
                    (theme::PAD_DEAD, Stroke::new(1.0, theme::PAD_DEAD_STROKE))
                } else if is_active {
                    // Lit pad: brightness follows pressure.
                    (
                        theme::pad_active(pressure),
                        Stroke::new(2.0, theme::PAD_ACTIVE_STROKE),
                    )
                } else if root {
                    (theme::PAD_ROOT, Stroke::new(1.0, theme::PAD_ROOT_STROKE))
                } else {
                    (theme::PAD, Stroke::new(1.0, theme::PAD_STROKE))
                };

                painter.rect_filled(cell, rounding, fill);
                painter.rect_stroke(cell, rounding, stroke, egui::StrokeKind::Inside);

                if is_active {
                    // Glow around the lit pad, intensity following pressure.
                    let glow = 70.0 + 140.0 * pressure.clamp(0.0, 1.0);
                    for (expand, scale) in [(2.5_f32, 1.0_f32), (5.5, 0.4)] {
                        painter.rect_stroke(
                            cell.expand(expand),
                            rounding + expand,
                            Stroke::new(
                                2.5,
                                Color32::from_rgba_unmultiplied(
                                    theme::ACCENT.r(),
                                    theme::ACCENT.g(),
                                    theme::ACCENT.b(),
                                    (glow * scale) as u8,
                                ),
                            ),
                            egui::StrokeKind::Outside,
                        );
                    }
                } else if hover_pad == Some(pad) && note.is_some() {
                    painter.rect_stroke(
                        cell,
                        rounding,
                        Stroke::new(1.5, theme::ACCENT.linear_multiply(0.45)),
                        egui::StrokeKind::Inside,
                    );
                }

                if let Some((flash_pad, strength)) = flash {
                    if flash_pad == pad {
                        painter.rect_filled(
                            cell,
                            rounding,
                            Color32::from_rgba_unmultiplied(
                                255,
                                255,
                                255,
                                (110.0 * strength) as u8,
                            ),
                        );
                    }
                }

                if show_labels {
                    if let Some(note) = note {
                        let text_color = if is_active {
                            theme::ACCENT_TEXT
                        } else if root {
                            theme::ACCENT_DIM
                        } else {
                            ui.visuals().weak_text_color()
                        };
                        painter.text(
                            Pos2::new(cell.left() + 5.0, cell.bottom() - 3.0),
                            Align2::LEFT_BOTTOM,
                            note_label(note),
                            FontId::proportional(11.0),
                            text_color,
                        );
                    }
                }
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
                Color32::from_rgba_unmultiplied(
                    theme::TRAIL.r(),
                    theme::TRAIL.g(),
                    theme::TRAIL.b(),
                    alpha,
                ),
            );
        }
    }

    fn draw_cursor(&self, painter: &egui::Painter, rect: Rect) {
        let Some(processed) = &self.latest_processed else {
            return;
        };
        let pos = surface_pos(processed.x, processed.y, rect);
        let color = if processed.active {
            theme::CURSOR_ACTIVE
        } else {
            theme::CURSOR_HOVER
        };
        let radius = 4.0 + 8.0 * processed.pressure as f32;
        painter.circle_stroke(pos, radius, Stroke::new(2.0, color));
        painter.circle_filled(pos, 2.0, color);
    }

    /// The live-expression strip beneath the surface: active-note badge plus
    /// painter-drawn bars for bend, pressure, CC74, and tilt.
    fn draw_meters(&self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            // Note badge: name + member channel, dimmed when silent.
            let (text, color) = match (self.engine.active_note(), self.engine.active_channel()) {
                (Some(note), Some(channel)) => {
                    (format!("{} ch{channel}", note_label(note)), theme::ACCENT)
                }
                _ => ("—".to_owned(), theme::STATUS_IDLE),
            };
            ui.allocate_ui(Vec2::new(110.0, 40.0), |ui| {
                ui.label(RichText::new(text).color(color).size(20.0).strong());
            });
            ui.separator();

            let active = self.engine.active_note().is_some();

            // Bipolar bend bar centered at PITCH_BEND_CENTER.
            let bend = self
                .engine
                .last_bend()
                .map(|b| (f32::from(b) - f32::from(PITCH_BEND_CENTER)) / 8191.0)
                .unwrap_or(0.0);
            meter_widget(ui, "bend", bend, active, true);

            if self.mapping.pressure_to_channel_pressure {
                let p = self.engine.last_pressure().map(seven_bit_frac).unwrap_or(0.0);
                meter_widget(ui, "pressure", p, active, false);
            }
            if self.mapping.x_to_cc74 {
                let cc = self.engine.last_cc74().map(seven_bit_frac).unwrap_or(0.0);
                meter_widget(ui, "CC74", cc, active, false);
            }
            if let Some(tilt) = &self.mapping.tilt_cc {
                let v = self.engine.last_tilt_cc().map(seven_bit_frac).unwrap_or(0.0);
                meter_widget(ui, &format!("tilt CC{}", tilt.controller), v, active, false);
            }
        });
    }
}

/// One captioned meter bar in the meters strip.
fn meter_widget(ui: &mut egui::Ui, label: &str, frac: f32, active: bool, bipolar: bool) {
    ui.vertical(|ui| {
        ui.label(
            RichText::new(label)
                .size(10.0)
                .color(if active { theme::ACCENT_DIM } else { theme::STATUS_IDLE }),
        );
        let (rect, _) = ui.allocate_exact_size(Vec2::new(150.0, 16.0), Sense::hover());
        if bipolar {
            theme::bipolar_bar(ui.painter(), rect, frac, theme::ACCENT);
        } else {
            theme::meter_bar(ui.painter(), rect, frac, theme::ACCENT);
        }
    });
}

/// Map a 7-bit MIDI value onto `[0, 1]` for the meter bars.
fn seven_bit_frac(v: u8) -> f32 {
    f32::from(v) / 127.0
}

/// "C#3"-style label for a MIDI note number (C4 = 60).
fn note_label(note: u8) -> String {
    let name = PITCH_CLASS_NAMES[usize::from(note % 12)];
    let octave = i32::from(note) / 12 - 1;
    format!("{name}{octave}")
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

        // Drop the note-on flash once it has fully faded.
        if self
            .flash
            .is_some_and(|(_, started)| started.elapsed() >= FLASH_DURATION)
        {
            self.flash = None;
        }

        // Track the window inner size so `on_exit` can persist it (eframe
        // doesn't hand the size to `on_exit` directly).
        if let Some(rect) = ui.ctx().input(|i| i.viewport().inner_rect) {
            let size = rect.size();
            if size.x > 0.0 && size.y > 0.0 {
                self.last_window_size = Some((size.x, size.y));
            }
        }

        // Keyboard shortcuts (skipped while a text field has focus).
        let typing = ui.ctx().egui_wants_keyboard_input();
        let (esc, test, page_up, page_down) = ui.ctx().input(|i| {
            (
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::T),
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::PageDown),
            )
        });
        if esc {
            self.panic_all_notes_off();
        }
        if !typing {
            if test && self.midi.is_connected() {
                self.send_test_note();
            }
            if page_up {
                self.octave_shift(1);
            }
            if page_down {
                self.octave_shift(-1);
            }
        }

        egui::Panel::top("hud_top").show_inside(ui, |ui| self.draw_top_bar(ui));

        egui::Panel::left("hud_sidebar")
            .resizable(true)
            .default_size(280.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("sidebar_scroll")
                    .show(ui, |ui| self.draw_sidebar(ui));
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::Panel::bottom("hud_meters")
                .resizable(false)
                .default_size(84.0)
                .show_inside(ui, |ui| self.draw_meters(ui));
            egui::CentralPanel::default().show_inside(ui, |ui| self.draw_surface(ui));
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Persist prefs first (best-effort).
        if let Some(size) = self.last_window_size {
            self.prefs.window_size = Some(size);
        }
        self.prefs.last_midi_port = self
            .midi
            .port_label()
            .map(str::to_owned)
            .or_else(|| self.midi_ports.get(self.selected_port).cloned());
        self.prefs.last_mapping_path = self.mapping_path.clone();
        if let Err(error) = self.prefs.save() {
            eprintln!("failed to save HUD prefs: {error}");
        }

        // Silence anything still sounding, then stop the reader thread (and
        // reap a `--spawn`ed producer).
        self.panic_all_notes_off();
        self.midi.disconnect();
        self.source_handle.stop();
    }
}
