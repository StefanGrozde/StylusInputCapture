//! [`MpeEngine`] — the stateful, pure mapping from [`ProcessedSample`]s to a
//! stream of [`MidiEvent`]s under a [`MidiMapping`].
//!
//! "Pure" in the same sense as `tablet_process`: no I/O, no clock reads, no
//! randomness. Given the same engine state, mapping, and sample it always
//! emits the same events, so behaviour is fully unit-testable without MIDI
//! hardware. The HUD app owns the `midir` connection and simply forwards
//! whatever events `process` produces.
//!
//! ## Voice model (single pen)
//! One pen contacts one point, so at most one note sounds at a time — but each
//! note is assigned its **own MPE member channel** (round-robin), so its
//! pitch-bend, timbre (CC74) and pressure are independent and successive notes
//! don't share controller state. That per-note independence is the point of
//! MPE; true chords would require multitouch, a future extension.

use tablet_process::ProcessedSample;

use crate::event::{MidiEvent, CC_ALL_NOTES_OFF, CC_TIMBRE, PITCH_BEND_CENTER};
use crate::mapping::{MidiMapping, NoteMode, TiltAxis, VelocitySource};
use crate::scale::{self, QuantizedPitch};

/// The note currently sounding and the member channel carrying it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Voice {
    channel: u8,
    note: u8,
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

    /// Process one sample, pushing any resulting events onto `out`.
    pub fn process(&mut self, s: &ProcessedSample, m: &MidiMapping, out: &mut Vec<MidiEvent>) {
        if !s.active {
            self.release(out);
            return;
        }

        let q = scale::quantize(s.x, m.low_note, m.span_notes, m.scale, m.key);

        match self.voice {
            None => self.start_note(s, m, q, out),
            Some(voice) => self.continue_note(s, m, q, voice, out),
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

    /// Pen-down rising edge: allocate a channel and start a note at the snapped
    /// pitch.
    fn start_note(
        &mut self,
        s: &ProcessedSample,
        m: &MidiMapping,
        q: QuantizedPitch,
        out: &mut Vec<MidiEvent>,
    ) {
        let channel = self.alloc_channel(m);
        let velocity = velocity_for(m.velocity, s.pressure);
        out.push(MidiEvent::NoteOn {
            channel,
            note: q.note,
            velocity,
        });
        self.voice = Some(Voice {
            channel,
            note: q.note,
        });
        self.reset_expression();
    }

    /// Held note: behaviour depends on [`NoteMode`] — sustain the struck pitch,
    /// glide via pitch-bend, or retrigger on each new scale note.
    fn continue_note(
        &mut self,
        s: &ProcessedSample,
        m: &MidiMapping,
        q: QuantizedPitch,
        voice: Voice,
        out: &mut Vec<MidiEvent>,
    ) {
        match m.mode {
            // Sustain: the struck pitch is latched; sideways movement never
            // changes the note (only expression, in `emit_expression`).
            NoteMode::Hold => {}
            NoteMode::Glide => {
                // Keep one note and bend toward the exact pointed pitch. If the
                // required bend exceeds the configured range, retrigger on the
                // snapped note to re-center.
                let bend = q.continuous - f64::from(voice.note);
                if bend.abs() > m.mpe.pitch_bend_range_semitones {
                    self.retrigger(s, m, q.note, out);
                }
            }
            NoteMode::Discrete => {
                if q.note != voice.note {
                    // Discrete keyboard: each new scale note retriggers.
                    self.retrigger(s, m, q.note, out);
                }
            }
        }
    }

    /// NoteOff the current voice and NoteOn `note` on a fresh member channel.
    fn retrigger(
        &mut self,
        s: &ProcessedSample,
        m: &MidiMapping,
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
        self.voice = Some(Voice { channel, note });
        self.reset_expression();
    }

    /// Emit per-note expression (pitch-bend, CC74, channel pressure, tilt CC)
    /// for the active voice, only when a value changed since last emission.
    fn emit_expression(&mut self, s: &ProcessedSample, m: &MidiMapping, out: &mut Vec<MidiEvent>) {
        let Some(voice) = self.voice else {
            return;
        };

        // Pitch bend: residual to the exact pointed pitch (relative to the
        // held note), clamped to the configured range.
        let q = scale::quantize(s.x, m.low_note, m.span_notes, m.scale, m.key);
        let bend_semitones = match m.mode {
            // Glide bends toward the exact pointed pitch.
            NoteMode::Glide => q.continuous - f64::from(voice.note),
            // Hold and Discrete keep the struck pitch (no expressive bend).
            NoteMode::Hold | NoteMode::Discrete => 0.0,
        };
        let bend = bend_value(bend_semitones, m.mpe.pitch_bend_range_semitones);
        if self.last_bend != Some(bend) {
            out.push(MidiEvent::PitchBend {
                channel: voice.channel,
                value: bend,
            });
            self.last_bend = Some(bend);
        }

        if m.y_to_cc74 {
            let y = if m.y_invert { 1.0 - s.y } else { s.y };
            let cc = to_7bit(y);
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

    fn mapping() -> MidiMapping {
        // Chromatic, key C, 48..72, keyboard mode for predictable per-note
        // retrigger behaviour in the tests.
        let mut m = MidiMapping::default();
        m.scale = ScaleKind::Chromatic;
        m.span_notes = 24;
        m.low_note = 48;
        m.mode = NoteMode::Discrete;
        m
    }

    #[test]
    fn pen_down_emits_note_on_then_pen_up_note_off() {
        let mut e = MpeEngine::new();
        let m = mapping();
        let mut out = Vec::new();

        e.process(&sample(0.0, 0.5, 0.8, true), &m, &mut out);
        assert!(
            matches!(out.first(), Some(MidiEvent::NoteOn { note: 48, .. })),
            "expected NoteOn 48, got {out:?}"
        );
        let on_channel = out[0].channel();
        assert!((m.mpe.member_low..=m.mpe.member_high).contains(&on_channel));
        assert_eq!(e.active_note(), Some(48));

        out.clear();
        e.process(&sample(0.0, 0.5, 0.0, false), &m, &mut out);
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
    fn successive_notes_use_different_member_channels() {
        let mut e = MpeEngine::new();
        let m = mapping();
        let mut channels = Vec::new();

        // Three separate taps at different positions.
        for x in [0.0, 0.5, 1.0] {
            let mut out = Vec::new();
            e.process(&sample(x, 0.5, 0.8, true), &m, &mut out);
            let on = out
                .iter()
                .find_map(|ev| match ev {
                    MidiEvent::NoteOn { channel, .. } => Some(*channel),
                    _ => None,
                })
                .expect("note on");
            channels.push(on);
            let mut up = Vec::new();
            e.process(&sample(x, 0.5, 0.0, false), &m, &mut up);
        }

        assert_eq!(channels[0] + 1, channels[1]);
        assert_eq!(channels[1] + 1, channels[2]);
    }

    #[test]
    fn discrete_mode_retriggers_on_new_scale_note() {
        let mut e = MpeEngine::new();
        let m = mapping();
        let mut out = Vec::new();

        e.process(&sample(0.0, 0.5, 0.8, true), &m, &mut out); // note 48
        out.clear();
        // Move far enough to cross into note 60 (x=0.5 → 48 + 12).
        e.process(&sample(0.5, 0.5, 0.8, true), &m, &mut out);

        let has_off_48 = out
            .iter()
            .any(|e| matches!(e, MidiEvent::NoteOff { note: 48, .. }));
        let has_on_60 = out
            .iter()
            .any(|e| matches!(e, MidiEvent::NoteOn { note: 60, .. }));
        assert!(has_off_48 && has_on_60, "expected retrigger, got {out:?}");
        assert_eq!(e.active_note(), Some(60));
    }

    #[test]
    fn hold_mode_sustains_one_note_while_moving() {
        let mut e = MpeEngine::new();
        let mut m = mapping();
        m.mode = NoteMode::Hold;
        let mut out = Vec::new();

        e.process(&sample(0.0, 0.5, 0.8, true), &m, &mut out); // strike note 48
        let on_channel = e.voice.unwrap().channel;
        out.clear();

        // Sweep all the way across the surface: the note must not change and no
        // NoteOff/NoteOn may fire — only expression (CC74) tracks the movement.
        for x in [0.25, 0.5, 0.75, 1.0] {
            e.process(&sample(x, 0.5, 0.8, true), &m, &mut out);
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
        e.process(&sample(1.0, 0.5, 0.0, false), &m, &mut out);
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
    fn y_drives_cc74_and_pressure_drives_channel_pressure_on_change_only() {
        let mut e = MpeEngine::new();
        let m = mapping();

        let mut out = Vec::new();
        e.process(&sample(0.0, 0.0, 0.5, true), &m, &mut out);
        // y=0, inverted → 1.0 → CC74 127. pressure 0.5 → 64.
        assert!(out.contains(&MidiEvent::ControlChange {
            channel: e.voice.unwrap().channel,
            controller: CC_TIMBRE,
            value: 127
        }));
        assert!(out.iter().any(|e| matches!(
            e,
            MidiEvent::ChannelPressure { value: 64, .. }
        )));

        // Same Y and pressure again → no new CC74/pressure events.
        out.clear();
        e.process(&sample(0.0, 0.0, 0.5, true), &m, &mut out);
        assert!(!out
            .iter()
            .any(|e| matches!(e, MidiEvent::ControlChange { controller: 74, .. })));
        assert!(!out
            .iter()
            .any(|e| matches!(e, MidiEvent::ChannelPressure { .. })));
    }

    #[test]
    fn all_notes_off_clears_active_and_sweeps_member_channels() {
        let mut e = MpeEngine::new();
        let m = mapping();
        let mut out = Vec::new();
        e.process(&sample(0.0, 0.5, 0.8, true), &m, &mut out);

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
