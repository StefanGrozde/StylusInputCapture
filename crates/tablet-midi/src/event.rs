//! Wire-agnostic MIDI events and their raw byte encoding.
//!
//! [`MidiEvent`] is the pure output of the mapping engine; it knows nothing
//! about `midir` or any OS MIDI API. The HUD app converts each event to bytes
//! via [`MidiEvent::to_bytes`] and hands them to its output connection — the
//! only I/O boundary.
//!
//! All `channel` fields are **0-based** (0–15), matching the MIDI wire format
//! (status byte low nibble). MPE configuration is expressed in the same 0-based
//! space; UI surfaces add 1 for display.

use std::ops::Deref;

/// Pitch-bend value placing a member channel at its unbent center.
pub const PITCH_BEND_CENTER: u16 = 8192;

/// CC number 74 — "Sound Controller 5 / Brightness", the MPE convention for the
/// per-note Y/timbre ("slide") dimension.
pub const CC_TIMBRE: u8 = 74;

/// CC number 123 — "All Notes Off", emitted by the panic control.
pub const CC_ALL_NOTES_OFF: u8 = 123;

/// A single MIDI channel-voice message, independent of any transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiEvent {
    /// Note on. `velocity` must be 1–127 (0 is treated as note-off by spec).
    NoteOn { channel: u8, note: u8, velocity: u8 },
    /// Note off.
    NoteOff { channel: u8, note: u8, velocity: u8 },
    /// 14-bit pitch bend; [`PITCH_BEND_CENTER`] (8192) is unbent.
    PitchBend { channel: u8, value: u16 },
    /// Channel pressure (monophonic aftertouch) — the MPE per-note "press" axis.
    ChannelPressure { channel: u8, value: u8 },
    /// Control change.
    ControlChange { channel: u8, controller: u8, value: u8 },
}

/// Up to three encoded MIDI bytes, stack-allocated (no heap, no extra deps).
///
/// Derefs to `&[u8]`, so it can be passed straight to `midir`'s
/// `MidiOutputConnection::send`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiBytes {
    buf: [u8; 3],
    len: usize,
}

impl MidiBytes {
    fn two(a: u8, b: u8) -> Self {
        Self {
            buf: [a, b, 0],
            len: 2,
        }
    }

    fn three(a: u8, b: u8, c: u8) -> Self {
        Self {
            buf: [a, b, c],
            len: 3,
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Deref for MidiBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl MidiEvent {
    /// Channel of this event (0-based).
    pub fn channel(&self) -> u8 {
        match self {
            MidiEvent::NoteOn { channel, .. }
            | MidiEvent::NoteOff { channel, .. }
            | MidiEvent::PitchBend { channel, .. }
            | MidiEvent::ChannelPressure { channel, .. }
            | MidiEvent::ControlChange { channel, .. } => *channel,
        }
    }

    /// Encode to raw MIDI bytes. Data bytes are masked to 7 bits and the
    /// channel to 4 bits, so a malformed value can never produce an invalid
    /// status/data byte.
    pub fn to_bytes(&self) -> MidiBytes {
        let ch = self.channel() & 0x0F;
        match *self {
            MidiEvent::NoteOff { note, velocity, .. } => {
                MidiBytes::three(0x80 | ch, note & 0x7F, velocity & 0x7F)
            }
            MidiEvent::NoteOn { note, velocity, .. } => {
                MidiBytes::three(0x90 | ch, note & 0x7F, velocity & 0x7F)
            }
            MidiEvent::ControlChange {
                controller, value, ..
            } => MidiBytes::three(0xB0 | ch, controller & 0x7F, value & 0x7F),
            MidiEvent::ChannelPressure { value, .. } => MidiBytes::two(0xD0 | ch, value & 0x7F),
            MidiEvent::PitchBend { value, .. } => {
                let v = value.min(0x3FFF);
                MidiBytes::three(0xE0 | ch, (v & 0x7F) as u8, ((v >> 7) & 0x7F) as u8)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_on_off_encode() {
        assert_eq!(
            MidiEvent::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100
            }
            .to_bytes()
            .as_slice(),
            &[0x91, 60, 100]
        );
        assert_eq!(
            MidiEvent::NoteOff {
                channel: 0,
                note: 64,
                velocity: 0
            }
            .to_bytes()
            .as_slice(),
            &[0x80, 64, 0]
        );
    }

    #[test]
    fn control_change_and_channel_pressure_encode() {
        assert_eq!(
            MidiEvent::ControlChange {
                channel: 2,
                controller: CC_TIMBRE,
                value: 64
            }
            .to_bytes()
            .as_slice(),
            &[0xB2, 74, 64]
        );
        assert_eq!(
            MidiEvent::ChannelPressure {
                channel: 3,
                value: 120
            }
            .to_bytes()
            .as_slice(),
            &[0xD3, 120]
        );
    }

    #[test]
    fn pitch_bend_splits_into_lsb_msb() {
        // Center 8192 = 0x2000 → lsb 0x00, msb 0x40.
        assert_eq!(
            MidiEvent::PitchBend {
                channel: 0,
                value: PITCH_BEND_CENTER
            }
            .to_bytes()
            .as_slice(),
            &[0xE0, 0x00, 0x40]
        );
        // Max 16383 = 0x3FFF → lsb 0x7F, msb 0x7F.
        assert_eq!(
            MidiEvent::PitchBend {
                channel: 0,
                value: 16383
            }
            .to_bytes()
            .as_slice(),
            &[0xE0, 0x7F, 0x7F]
        );
        // Zero → both data bytes zero.
        assert_eq!(
            MidiEvent::PitchBend {
                channel: 0,
                value: 0
            }
            .to_bytes()
            .as_slice(),
            &[0xE0, 0x00, 0x00]
        );
    }

    #[test]
    fn channel_is_masked_to_low_nibble() {
        // A channel >15 must not corrupt the status byte's high nibble.
        let bytes = MidiEvent::NoteOn {
            channel: 0x1F,
            note: 60,
            velocity: 1,
        }
        .to_bytes();
        assert_eq!(bytes.as_slice(), &[0x9F, 60, 1]);
    }
}
