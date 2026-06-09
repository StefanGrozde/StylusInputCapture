//! `tablet-midi` — pure, OS-agnostic mapping from processed pen input to MPE
//! (MIDI Polyphonic Expression) events.
//!
//! Mirrors `tablet-process`: it has **no I/O, UI, or MIDI-driver dependency**.
//! A [`MidiMapping`] (serializable config) plus an [`MpeEngine`] (per-stream
//! state) turn each [`ProcessedSample`](tablet_process::ProcessedSample) into a
//! list of [`MidiEvent`]s. The HUD app owns the actual `midir` connection and
//! forwards `event.to_bytes()`.
//!
//! Mapping (per the design decisions): horizontal position → **pitch** (snapped
//! to a selectable scale/key, with MPE pitch-bend for micro-expression),
//! vertical position → **CC74** (timbre), pen pressure → **channel pressure**.

pub mod engine;
pub mod event;
pub mod mapping;
pub mod scale;

pub use engine::MpeEngine;
pub use event::{MidiBytes, MidiEvent, CC_ALL_NOTES_OFF, CC_TIMBRE, PITCH_BEND_CENTER};
pub use mapping::{
    MappingError, MidiMapping, MpeConfig, NoteMode, TiltAxis, TiltCc, VelocitySource,
};
pub use scale::{quantize, QuantizedPitch, ScaleKind, PITCH_CLASS_NAMES};
