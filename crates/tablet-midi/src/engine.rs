//! [`MpeEngine`] — the stateful, pure mapping from [`ProcessedSample`]s to a
//! stream of [`MidiEvent`]s under a [`MidiMapping`].
//!
//! "Pure" in the same sense as `tablet_process`: no I/O, no clock reads, no
//! randomness. Given the same engine state, mapping, and sample it always
//! emits the same events, so behaviour is fully unit-testable without MIDI
//! hardware. The HUD app owns the `midir` connection and simply forwards
//! whatever events `process` produces.
//!
//! ## Voice model (single pen, pad grid)
//! The surface is a grid of pads ([`pads`]); pressing one starts its note and
//! the note **sustains for as long as the pen stays pressed**. While held, the
//! pen's continuous inputs modulate the voice's MPE expression — all measured
//! relative to where the note was struck, so a fresh note always starts
//! neutral: dragging **up/down** → pitch bend (up = higher), dragging
//! **left/right** → CC74 (timbre / brightness, the synth's "frequency"),
//! pressure → channel pressure, tilt → an extra CC. In the default Latch mode
//! dragging across other pads never changes the note — it only modulates. One
//! pen contacts one point, so at most one note sounds at a time — but each
//! note is assigned its **own MPE member channel** (round-robin), so
//! successive notes don't share controller state. True chords would require
//! multitouch, a future extension.

use tablet_process::ProcessedSample;

use crate::event::{MidiEvent, CC_ALL_NOTES_OFF, CC_TIMBRE, PITCH_BEND_CENTER};
use crate::mapping::{MidiMapping, NoteMode, TiltAxis, VelocitySource};
use crate::pads::{self, Pad};

/// The note currently sounding, the member channel carrying it, and the pad
/// state needed for relative expression.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Voice {
    channel: u8,
    note: u8,
    /// The pad whose note is sounding (for HUD highlight + slide detection).
    pad: Pad,
    /// Surface position at the strike — drag expression (Y → bend,
    /// X → CC74) is measured from here, so a fresh note always starts at
    /// neutral pitch and timbre no matter where it was struck.
    x_origin: f64,
    y_origin: f64,
}

/// Stateful MPE mapper. Construct once per session and thread it through every
/// [`process`](MpeEngine::process) call, mirroring `tablet_process::ProcessorState`.
#[derive(Clone, Debug, Default)]
pub struct MpeEngine {
    /// Round-robin offset within the member-channel range.
    next_member_offset: u8,
    /// The active voice, if a note is currently held.
    voice: Option<Voice>,
    /// Last emitted expression values (for the active voice), so we only emit
    /// on change and don't flood the bus at the capture rate.
    last_bend: Option<u16>,
    last_cc74: Option<u8>,
    last_pressure: Option<u8>,
    last_tilt_cc: Option<u8>,
}

impl MpeEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// True while a note is sounding (useful for HUD highlighting).
    pub fn active_note(&self) -> Option<u8> {
        self.voice.map(|v| v.note)
    }

    /// The pad whose note is currently sounding (for HUD highlighting).
    pub fn active_pad(&self) -> Option<Pad> {
        self.voice.map(|v| v.pad)
    }

    /// The MPE member channel carrying the active voice (for HUD meters).
    pub fn active_channel(&self) -> Option<u8> {
        self.voice.map(|v| v.channel)
    }

    /// Last emitted pitch-bend value (14-bit, 8192 = center), if any.
    pub fn last_bend(&self) -> Option<u16> {
        self.last_bend
    }

    /// Last emitted CC74 (timbre) value, if any.
    pub fn last_cc74(&self) -> Option<u8> {
        self.last_cc74
    }

    /// Last emitted channel-pressure value, if any.
    pub fn last_pressure(&self) -> Option<u8> {
        self.last_pressure
    }

    /// Last emitted tilt-CC value, if any.
    pub fn last_tilt_cc(&self) -> Option<u8> {
        self.last_tilt_cc
    }

    /// Process one sample, pushing any resulting events onto `out`.
    pub fn process(&mut self, s: &ProcessedSample, m: &MidiMapping, out: &mut Vec<MidiEvent>) {
        if !s.active {
            self.release(out);
            return;
        }

        let pad = pads::pad_at(s.x, s.y, &m.grid);

        match self.voice {
            None => self.start_note(s, m, pad, out),
            Some(voice) => self.continue_note(s, m, pad, voice, out),
        }

        self.emit_expression(s, m, out);
    }

    /// Release the active note (pen lifted) and reset expression dedup state.
    fn release(&mut self, out: &mut Vec<MidiEvent>) {
        if let Some(voice) = self.voice.take() {
            out.push(MidiEvent::NoteOff {
                channel: voice.channel,
                note: voice.note,
                velocity: 0,
            });
        }
        self.reset_expression();
    }

    /// Pen-down rising edge: allocate a channel and start the pressed pad's
    /// note. Pads beyond the MIDI range stay silent.
    fn start_note(
        &mut self,
        s: &ProcessedSample,
        m: &MidiMapping,
        pad: Pad,
        out: &mut Vec<MidiEvent>,
    ) {
        let Some(note) = pads::pad_note(pad, m) else {
            return;
        };
        let channel = self.alloc_channel(m);
        let velocity = velocity_for(m.velocity, s.pressure);
        out.push(MidiEvent::NoteOn {
            channel,
            note,
            velocity,
        });
        self.voice = Some(Voice {
            channel,
            note,
            pad,
            x_origin: s.x,
            y_origin: s.y,
        });
        self.reset_expression();
    }

    /// Held note: what sliding onto a *different* pad does depends on
    /// [`NoteMode`] — keep the struck pad latched, glide via pitch-bend, or
    /// retrigger the new pad (Push-like, the default).
    fn continue_note(
        &mut self,
        s: &ProcessedSample,
        m: &MidiMapping,
        pad: Pad,
        voice: Voice,
        out: &mut Vec<MidiEvent>,
    ) {
        if pad == voice.pad {
            return; // same pad: the note sustains, only expression moves
        }
        match m.mode {
            // Latch: the struck pad is held; sliding around never changes the
            // note (only expression, in `emit_expression`).
            NoteMode::Hold => {}
            NoteMode::Glide => {
                // Keep one note and bend toward the pad under the pen. If the
                // required bend exceeds the configured range, retrigger on the
                // pointed pad to re-center.
                if let Some(target) = pads::pad_note(pad, m) {
                    let bend = f64::from(target) - f64::from(voice.note);
                    if bend.abs() > m.mpe.pitch_bend_range_semitones {
                        self.retrigger(s, m, pad, target, out);
                    }
                }
            }
            NoteMode::Discrete => {
                // Pad keyboard: entering a new pad retriggers its note.
                if let Some(note) = pads::pad_note(pad, m) {
                    self.retrigger(s, m, pad, note, out);
                }
            }
        }
    }

    /// NoteOff the current voice and NoteOn `note` on a fresh member channel.
    fn retrigger(
        &mut self,
        s: &ProcessedSample,
        m: &MidiMapping,
        pad: Pad,
        note: u8,
        out: &mut Vec<MidiEvent>,
    ) {
        if let Some(old) = self.voice.take() {
            out.push(MidiEvent::NoteOff {
                channel: old.channel,
                note: old.note,
                velocity: 0,
            });
        }
        let channel = self.alloc_channel(m);
        let velocity = velocity_for(m.velocity, s.pressure);
        out.push(MidiEvent::NoteOn {
            channel,
            note,
            velocity,
        });
        self.voice = Some(Voice {
            channel,
            note,
            pad,
            x_origin: s.x,
            y_origin: s.y,
        });
        self.reset_expression();
    }

    /// Emit per-note expression (pitch-bend, CC74, channel pressure, tilt CC)
    /// for the active voice, only when a value changed since last emission.
    fn emit_expression(&mut self, s: &ProcessedSample, m: &MidiMapping, out: &mut Vec<MidiEvent>) {
        let Some(voice) = self.voice else {
            return;
        };

        // Pitch bend. In Latch/Pads modes dragging vertically from the strike
        // point bends the held note (up = higher; stream Y grows downward, so
        // the sign flips); a full surface height of travel = `y_bend_semitones`.
        // In Glide the bend tracks the pad under the pen instead, so the pitch
        // slides continuously across the surface.
        let bend_semitones = match m.mode {
            NoteMode::Hold | NoteMode::Discrete => {
                (voice.y_origin - s.y) * m.y_bend_semitones
            }
            NoteMode::Glide => {
                let pad = pads::pad_at(s.x, s.y, &m.grid);
                let target = pads::pad_note(pad, m).unwrap_or(voice.note);
                f64::from(target) - f64::from(voice.note)
            }
        };
        let bend = bend_value(bend_semitones, m.mpe.pitch_bend_range_semitones);
        if self.last_bend != Some(bend) {
            out.push(MidiEvent::PitchBend {
                channel: voice.channel,
                value: bend,
            });
            self.last_bend = Some(bend);
        }

        if m.x_to_cc74 {
            // Horizontal drag from the strike point → CC74 (timbre /
            // brightness). The strike is the neutral center (64); a half
            // surface width of travel reaches the rails.
            let cc = to_7bit(0.5 + (s.x - voice.x_origin));
            if self.last_cc74 != Some(cc) {
                out.push(MidiEvent::ControlChange {
                    channel: voice.channel,
                    controller: CC_TIMBRE,
                    value: cc,
                });
                self.last_cc74 = Some(cc);
            }
        }

        if m.pressure_to_channel_pressure {
            let p = to_7bit(s.pressure);
            if self.last_pressure != Some(p) {
                out.push(MidiEvent::ChannelPressure {
                    channel: voice.channel,
                    value: p,
                });
                self.last_pressure = Some(p);
            }
        }

        if let Some(tilt) = m.tilt_cc {
            let raw = match tilt.axis {
                TiltAxis::X => s.tilt_x,
                TiltAxis::Y => s.tilt_y,
            };
            if let Some(deg) = raw {
                let value = tilt_to_7bit(deg, tilt.range_deg);
                if self.last_tilt_cc != Some(value) {
                    out.push(MidiEvent::ControlChange {
                        channel: voice.channel,
                        controller: tilt.controller,
                        value,
                    });
                    self.last_tilt_cc = Some(value);
                }
            }
        }
    }

    /// Panic: release any active note and send All-Notes-Off on every member
    /// channel so nothing is left hanging in the receiver.
    pub fn all_notes_off(&mut self, m: &MidiMapping, out: &mut Vec<MidiEvent>) {
        self.release(out);
        for channel in m.mpe.member_low..=m.mpe.member_high {
            out.push(MidiEvent::ControlChange {
                channel,
                controller: CC_ALL_NOTES_OFF,
                value: 0,
            });
        }
        self.next_member_offset = 0;
    }

    /// Next member channel, cycling through the configured range.
    fn alloc_channel(&mut self, m: &MidiMapping) -> u8 {
        let count = m.mpe.member_count().max(1);
        let channel = m.mpe.member_low + (self.next_member_offset % count);
        self.next_member_offset = (self.next_member_offset + 1) % count;
        channel
    }

    fn reset_expression(&mut self) {
        self.last_bend = None;
        self.last_cc74 = None;
        self.last_pressure = None;
        self.last_tilt_cc = None;
    }
}

/// Map a normalized value in `[0, 1]` to a 7-bit MIDI value `[0, 127]`.
fn to_7bit(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 127.0).round() as u8
}

/// Map a signed tilt in degrees, clamped to `±range_deg`, onto `[0, 127]`
/// (center 64).
fn tilt_to_7bit(deg: f64, range_deg: f64) -> u8 {
    if range_deg <= 0.0 {
        return 64;
    }
    let norm = (deg / range_deg).clamp(-1.0, 1.0); // [-1, 1]
    to_7bit((norm + 1.0) / 2.0)
}

/// Resolve note-on velocity for the configured source (always ≥ 1, so a
/// note-on is never silently turned into a note-off).
fn velocity_for(source: VelocitySource, pressure: f64) -> u8 {
    let v = match source {
        VelocitySource::Fixed(v) => v,
        VelocitySource::Pressure { min, max } => {
            let p = pressure.clamp(0.0, 1.0);
            let lo = f64::from(min);
            let hi = f64::from(max);
            (lo + p * (hi - lo)).round() as u8
        }
    };
    v.clamp(1, 127)
}

/// Convert a signed bend in semitones to a 14-bit pitch-bend value.
fn bend_value(bend_semitones: f64, range_semitones: f64) -> u16 {
    if range_semitones <= 0.0 {
        return PITCH_BEND_CENTER;
    }
    let norm = (bend_semitones / range_semitones).clamp(-1.0, 1.0);
    let value = f64::from(PITCH_BEND_CENTER) + norm * 8191.0;
    value.round().clamp(0.0, 16383.0) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scale::ScaleKind;
    use tablet_core::{PenSample, ToolKind};

    fn sample(x: f64, y: f64, pressure: f64, active: bool) -> ProcessedSample {
        ProcessedSample {
            raw: PenSample {
                t_capture_ns: 0,
                t_device_ms: 0,
                serial: 0,
                x_raw: 0,
                y_raw: 0,
                z_raw: 0,
                x_norm: x,
                y_norm: y,
                pressure_raw: 0,
                pressure_norm: pressure,
                tangent_pressure_raw: None,
                azimuth_deci_deg: None,
                altitude_deci_deg: None,
                twist_deci_deg: None,
                tilt_x_deg: None,
                tilt_y_deg: None,
                rotation_deci_deg: None,
                buttons: 0,
                tool: ToolKind::Pen,
                tool_serial: 0,
                in_proximity: active,
                status: 0,
            },
            x,
            y,
            x_filtered: None,
            y_filtered: None,
            pressure,
            active,
            tilt_x: None,
            tilt_y: None,
            twist: None,
            out_of_range: false,
        }
    }

    /// Chromatic 8x8 grid from C3 (48), rows in octaves so pad pitches are
    /// easy to predict: note = 48 + row*12 + col.
    fn mapping() -> MidiMapping {
        let mut m = MidiMapping::default();
        m.scale = ScaleKind::Chromatic;
        m.key = 0;
        m.low_note = 48;
        m.grid.rows = 8;
        m.grid.cols = 8;
        m.grid.row_interval_degrees = 12;
        m.mode = NoteMode::Discrete;
        m
    }

    /// Center of pad (row, col) on the default 8x8 grid, in stream coords
    /// (Y grows downward; row 0 is the bottom row).
    fn pad_center(row: u8, col: u8) -> (f64, f64) {
        let x = (f64::from(col) + 0.5) / 8.0;
        let y = 1.0 - (f64::from(row) + 0.5) / 8.0;
        (x, y)
    }

    #[test]
    fn pad_press_emits_note_on_then_pen_up_note_off() {
        let mut e = MpeEngine::new();
        let m = mapping();
        let mut out = Vec::new();

        let (x, y) = pad_center(0, 0);
        e.process(&sample(x, y, 0.8, true), &m, &mut out);
        assert!(
            matches!(out.first(), Some(MidiEvent::NoteOn { note: 48, .. })),
            "expected NoteOn 48, got {out:?}"
        );
        let on_channel = out[0].channel();
        assert!((m.mpe.member_low..=m.mpe.member_high).contains(&on_channel));
        assert_eq!(e.active_note(), Some(48));
        assert_eq!(e.active_pad(), Some(Pad { row: 0, col: 0 }));

        out.clear();
        e.process(&sample(x, y, 0.0, false), &m, &mut out);
        assert_eq!(
            out,
            vec![MidiEvent::NoteOff {
                channel: on_channel,
                note: 48,
                velocity: 0
            }]
        );
        assert_eq!(e.active_note(), None);
    }

    #[test]
    fn holding_within_a_pad_sustains_without_retrigger() {
        let mut e = MpeEngine::new();
        let m = mapping();
        let mut out = Vec::new();

        let (x, y) = pad_center(3, 4);
        e.process(&sample(x, y, 0.6, true), &m, &mut out);
        let note = e.active_note().unwrap();
        out.clear();

        // Many held samples wiggling inside the same pad: the note must keep
        // sounding — no NoteOn/NoteOff — while expression keeps flowing.
        for i in 0..50 {
            let dx = 0.04 * f64::from(i % 5) / 5.0 / 8.0;
            e.process(&sample(x + dx, y, 0.5 + 0.005 * f64::from(i), true), &m, &mut out);
        }
        assert_eq!(e.active_note(), Some(note), "note must stay held");
        assert!(
            !out.iter()
                .any(|ev| matches!(ev, MidiEvent::NoteOn { .. } | MidiEvent::NoteOff { .. })),
            "holding inside one pad must not retrigger, got {out:?}"
        );
        assert!(
            out.iter()
                .any(|ev| matches!(ev, MidiEvent::ChannelPressure { .. })),
            "pressure changes while held must emit channel pressure"
        );
    }

    #[test]
    fn successive_notes_use_different_member_channels() {
        let mut e = MpeEngine::new();
        let m = mapping();
        let mut channels = Vec::new();

        // Three separate taps on different pads.
        for col in [0u8, 2, 4] {
            let (x, y) = pad_center(0, col);
            let mut out = Vec::new();
            e.process(&sample(x, y, 0.8, true), &m, &mut out);
            let on = out
                .iter()
                .find_map(|ev| match ev {
                    MidiEvent::NoteOn { channel, .. } => Some(*channel),
                    _ => None,
                })
                .expect("note on");
            channels.push(on);
            let mut up = Vec::new();
            e.process(&sample(x, y, 0.0, false), &m, &mut up);
        }

        assert_eq!(channels[0] + 1, channels[1]);
        assert_eq!(channels[1] + 1, channels[2]);
    }

    #[test]
    fn discrete_mode_retriggers_when_sliding_to_a_new_pad() {
        let mut e = MpeEngine::new();
        let m = mapping();
        let mut out = Vec::new();

        let (x0, y0) = pad_center(0, 0); // note 48
        e.process(&sample(x0, y0, 0.8, true), &m, &mut out);
        out.clear();

        let (x1, y1) = pad_center(0, 2); // note 50
        e.process(&sample(x1, y1, 0.8, true), &m, &mut out);

        let has_off_48 = out
            .iter()
            .any(|e| matches!(e, MidiEvent::NoteOff { note: 48, .. }));
        let has_on_50 = out
            .iter()
            .any(|e| matches!(e, MidiEvent::NoteOn { note: 50, .. }));
        assert!(has_off_48 && has_on_50, "expected retrigger, got {out:?}");
        assert_eq!(e.active_note(), Some(50));
        assert_eq!(e.active_pad(), Some(Pad { row: 0, col: 2 }));
    }

    #[test]
    fn hold_mode_latches_the_struck_pad_while_sliding() {
        let mut e = MpeEngine::new();
        let mut m = mapping();
        m.mode = NoteMode::Hold;
        let mut out = Vec::new();

        let (x0, y0) = pad_center(0, 0); // strike note 48
        e.process(&sample(x0, y0, 0.8, true), &m, &mut out);
        let on_channel = e.voice.unwrap().channel;
        out.clear();

        // Sweep across the whole bottom row: the note must not change and no
        // NoteOff/NoteOn may fire — only expression tracks the movement.
        for col in 1..8u8 {
            let (x, y) = pad_center(0, col);
            e.process(&sample(x, y, 0.8, true), &m, &mut out);
        }
        assert_eq!(e.active_note(), Some(48), "pitch must stay latched");
        assert!(
            !out.iter().any(|ev| matches!(
                ev,
                MidiEvent::NoteOn { .. } | MidiEvent::NoteOff { .. }
            )),
            "hold mode must not retrigger while moving, got {out:?}"
        );

        // Pen up releases the single sustained note.
        out.clear();
        e.process(&sample(1.0, y0, 0.0, false), &m, &mut out);
        assert_eq!(
            out,
            vec![MidiEvent::NoteOff {
                channel: on_channel,
                note: 48,
                velocity: 0
            }]
        );
    }

    #[test]
    fn dragging_up_bends_pitch_up_from_a_neutral_strike() {
        let mut e = MpeEngine::new();
        let mut m = mapping();
        m.mode = NoteMode::Hold; // the default latch behaviour
        let mut out = Vec::new();

        // Wherever the pen lands, the first bend must be center (in tune).
        let (x, y) = pad_center(2, 3);
        e.process(&sample(x, y, 0.8, true), &m, &mut out);
        let first_bend = out.iter().find_map(|ev| match ev {
            MidiEvent::PitchBend { value, .. } => Some(*value),
            _ => None,
        });
        assert_eq!(first_bend, Some(PITCH_BEND_CENTER));
        let note = e.active_note().unwrap();

        // Dragging up (smaller stream Y) bends the pitch up, note unchanged.
        out.clear();
        e.process(&sample(x, y - 0.25, 0.8, true), &m, &mut out);
        let bend_up = out
            .iter()
            .find_map(|ev| match ev {
                MidiEvent::PitchBend { value, .. } => Some(*value),
                _ => None,
            })
            .expect("bend after dragging up");
        assert!(bend_up > PITCH_BEND_CENTER, "up must bend up, got {bend_up}");
        assert_eq!(e.active_note(), Some(note));

        // Dragging below the strike bends down.
        out.clear();
        e.process(&sample(x, y + 0.2, 0.8, true), &m, &mut out);
        let bend_down = out
            .iter()
            .find_map(|ev| match ev {
                MidiEvent::PitchBend { value, .. } => Some(*value),
                _ => None,
            })
            .expect("bend after dragging down");
        assert!(bend_down < PITCH_BEND_CENTER);
    }

    #[test]
    fn dragging_right_drives_cc74_and_pressure_drives_channel_pressure_on_change_only() {
        let mut e = MpeEngine::new();
        let mut m = mapping();
        m.mode = NoteMode::Hold;

        // Strike: CC74 starts at the neutral center 64. Pressure 0.5 → 64.
        let (x, y) = pad_center(0, 2);
        let mut out = Vec::new();
        e.process(&sample(x, y, 0.5, true), &m, &mut out);
        let cc74 = out
            .iter()
            .find_map(|ev| match ev {
                MidiEvent::ControlChange {
                    controller: CC_TIMBRE,
                    value,
                    ..
                } => Some(*value),
                _ => None,
            })
            .expect("CC74 on press");
        assert_eq!(cc74, 64, "strike must be the neutral timbre center");
        assert!(out.iter().any(|e| matches!(
            e,
            MidiEvent::ChannelPressure { value: 64, .. }
        )));

        // Same position and pressure again → no new CC74/pressure events.
        out.clear();
        e.process(&sample(x, y, 0.5, true), &m, &mut out);
        assert!(!out
            .iter()
            .any(|e| matches!(e, MidiEvent::ControlChange { controller: 74, .. })));
        assert!(!out
            .iter()
            .any(|e| matches!(e, MidiEvent::ChannelPressure { .. })));

        // Dragging right brightens, dragging left of the strike darkens.
        out.clear();
        e.process(&sample(x + 0.3, y, 0.5, true), &m, &mut out);
        let cc74_right = out
            .iter()
            .find_map(|ev| match ev {
                MidiEvent::ControlChange {
                    controller: CC_TIMBRE,
                    value,
                    ..
                } => Some(*value),
                _ => None,
            })
            .expect("CC74 after dragging right");
        assert!(cc74_right > 64, "right must brighten, got {cc74_right}");

        out.clear();
        e.process(&sample(x - 0.3, y, 0.5, true), &m, &mut out);
        let cc74_left = out
            .iter()
            .find_map(|ev| match ev {
                MidiEvent::ControlChange {
                    controller: CC_TIMBRE,
                    value,
                    ..
                } => Some(*value),
                _ => None,
            })
            .expect("CC74 after dragging left");
        assert!(cc74_left < 64, "left must darken, got {cc74_left}");
    }

    #[test]
    fn glide_mode_bends_toward_the_pointed_pad() {
        let mut e = MpeEngine::new();
        let mut m = mapping();
        m.mode = NoteMode::Glide;
        let mut out = Vec::new();

        let (x0, y0) = pad_center(0, 0); // note 48
        e.process(&sample(x0, y0, 0.8, true), &m, &mut out);
        out.clear();

        let (x1, y1) = pad_center(0, 4); // note 52, within the ±48 st range
        e.process(&sample(x1, y1, 0.8, true), &m, &mut out);
        assert_eq!(e.active_note(), Some(48), "glide keeps the struck note");
        let bend = out
            .iter()
            .find_map(|ev| match ev {
                MidiEvent::PitchBend { value, .. } => Some(*value),
                _ => None,
            })
            .expect("glide must emit a bend");
        assert!(bend > PITCH_BEND_CENTER, "bend must go up toward pad 4");
        assert!(
            !out.iter()
                .any(|ev| matches!(ev, MidiEvent::NoteOn { .. } | MidiEvent::NoteOff { .. })),
            "in-range glide must not retrigger, got {out:?}"
        );
    }

    #[test]
    fn silent_pads_off_the_top_of_midi_range_do_not_note_on() {
        let mut e = MpeEngine::new();
        let mut m = mapping();
        m.low_note = 120; // row 1+ is beyond 127 (rows are octaves here)
        let mut out = Vec::new();

        let (x, y) = pad_center(1, 0);
        e.process(&sample(x, y, 0.8, true), &m, &mut out);
        assert!(out.is_empty(), "out-of-range pad must stay silent, got {out:?}");
        assert_eq!(e.active_note(), None);
    }

    #[test]
    fn all_notes_off_clears_active_and_sweeps_member_channels() {
        let mut e = MpeEngine::new();
        let m = mapping();
        let mut out = Vec::new();
        let (x, y) = pad_center(0, 0);
        e.process(&sample(x, y, 0.8, true), &m, &mut out);

        out.clear();
        e.all_notes_off(&m, &mut out);
        assert_eq!(e.active_note(), None);
        assert!(out
            .iter()
            .any(|e| matches!(e, MidiEvent::NoteOff { note: 48, .. })));
        let off_sweep = out
            .iter()
            .filter(|e| matches!(e, MidiEvent::ControlChange { controller: 123, .. }))
            .count();
        assert_eq!(off_sweep as u8, m.mpe.member_count());
    }

    #[test]
    fn expression_getters_mirror_emitted_values_and_reset_on_release() {
        let mut e = MpeEngine::new();
        let mut m = mapping();
        m.mode = NoteMode::Hold;
        let mut out = Vec::new();

        assert_eq!(e.active_channel(), None);
        assert_eq!(e.last_bend(), None);

        // Strike at pressure 0.5: bend starts centered, CC74 at the neutral
        // 64, channel pressure at 64. The getters must mirror exactly what
        // `process` emitted.
        let (x, y) = pad_center(0, 2);
        e.process(&sample(x, y, 0.5, true), &m, &mut out);
        assert_eq!(e.active_channel(), Some(e.voice.unwrap().channel));
        assert_eq!(e.last_bend(), Some(PITCH_BEND_CENTER));
        assert_eq!(e.last_cc74(), Some(64));
        assert_eq!(e.last_pressure(), Some(64));
        assert_eq!(e.last_tilt_cc(), None, "tilt CC disabled in this mapping");

        // Dragging up emits a new bend; the getter follows it.
        out.clear();
        e.process(&sample(x, y - 0.25, 0.5, true), &m, &mut out);
        let bend = e.last_bend().unwrap();
        assert!(bend > PITCH_BEND_CENTER);

        // Release: voice gone and all expression state reset.
        out.clear();
        e.process(&sample(x, y, 0.0, false), &m, &mut out);
        assert_eq!(e.active_channel(), None);
        assert_eq!(e.last_bend(), None);
        assert_eq!(e.last_cc74(), None);
        assert_eq!(e.last_pressure(), None);
        assert_eq!(e.last_tilt_cc(), None);
    }

    #[test]
    fn velocity_tracks_pressure_and_is_never_zero() {
        assert_eq!(
            velocity_for(VelocitySource::Pressure { min: 1, max: 127 }, 0.0),
            1
        );
        assert_eq!(
            velocity_for(VelocitySource::Pressure { min: 1, max: 127 }, 1.0),
            127
        );
        assert_eq!(velocity_for(VelocitySource::Fixed(0), 0.5), 1);
    }
}
