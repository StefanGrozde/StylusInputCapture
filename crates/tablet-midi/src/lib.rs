//! `tablet-midi` — pure, OS-agnostic mapping from processed pen input to MPE
//! (MIDI Polyphonic Expression) events.
//!
//! Mirrors `tablet-process`: it has **no I/O, UI, or MIDI-driver dependency**.
//! A [`MidiMapping`] (serializable config) plus an [`MpeEngine`] (per-stream
//! state) turn each [`ProcessedSample`](tablet_process::ProcessedSample) into a
//! list of [`MidiEvent`]s. The HUD app owns the actual `midir` connection and
//! forwards `event.to_bytes()`.
//!
//! Mapping (per the design decisions): the surface is a Push-style **pad
//! grid** ([`pads`]) — each pad sounds one in-scale note that sustains while
//! the pen is pressed, with within-pad Y → **pitch bend**, within-pad X →
//! **CC74** (timbre/slide), pen pressure → **channel pressure**, and tilt →
//! an extra CC.

pub mod engine;
pub mod event;
pub mod expression;
pub mod mapping;
pub mod pads;
pub mod scale;
pub mod vibrato;

pub use engine::MpeEngine;
pub use event::{MidiBytes, MidiEvent, CC_ALL_NOTES_OFF, CC_TIMBRE, PITCH_BEND_CENTER};
pub use expression::{ExpressionSnapshot, Rgb, TrailColorScheme};
pub use mapping::{
    GridConfig, MappingError, MidiMapping, MpeConfig, NoteMode, TiltAxis, TiltCc, VelocitySource,
};
pub use vibrato::{VibratoConfig, VibratoDisplay, VibratoState};
pub use pads::{is_root, pad_at, pad_fraction, pad_note, Pad};
pub use scale::{quantize, QuantizedPitch, ScaleKind, PITCH_CLASS_NAMES};
