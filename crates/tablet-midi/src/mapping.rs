//! [`MidiMapping`] — the serializable configuration that defines how stylus
//! input becomes MPE MIDI, plus its TOML/JSON I/O and validation.
//!
//! Pure data, mirroring `tablet_process::CalibrationProfile`: the mutable
//! per-stream state lives in [`MpeEngine`](crate::MpeEngine), not here. Saved
//! as `*.midimap.toml` (default) or `.json`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::scale::ScaleKind;

/// MPE channel layout and pitch-bend range.
///
/// Channels are **0-based**. The MPE "Lower Zone" default used here is a master
/// on channel 0 (MIDI ch 1) with members on channels 1–15 (MIDI ch 2–16).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MpeConfig {
    /// Master / zone channel (0-based). Default 0.
    pub master_channel: u8,
    /// First member channel (0-based, inclusive). Default 1.
    pub member_low: u8,
    /// Last member channel (0-based, inclusive). Default 15.
    pub member_high: u8,
    /// Per-note pitch-bend range in semitones. MPE's default member range is
    /// ±48 semitones.
    pub pitch_bend_range_semitones: f64,
}

impl Default for MpeConfig {
    fn default() -> Self {
        Self {
            master_channel: 0,
            member_low: 1,
            member_high: 15,
            pitch_bend_range_semitones: 48.0,
        }
    }
}

impl MpeConfig {
    /// Number of member channels (always ≥ 1 for a valid config).
    pub fn member_count(&self) -> u8 {
        self.member_high.saturating_sub(self.member_low) + 1
    }
}

/// How note-on velocity is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum VelocitySource {
    /// Constant velocity for every note.
    Fixed(u8),
    /// Map the pen pressure at note-on, in `[0, 1]`, onto `[min, max]`.
    Pressure { min: u8, max: u8 },
}

impl Default for VelocitySource {
    fn default() -> Self {
        // A floor well above 1: at the instant a note is struck the pen
        // pressure is often near zero, and a velocity of ~1 is inaudible on
        // most synths (notably the Windows GS Wavetable). 40 keeps the attack
        // audible while still leaving room for pressure-driven dynamics.
        VelocitySource::Pressure { min: 40, max: 127 }
    }
}

/// How horizontal pen movement behaves *after* a note is struck.
///
/// Pen-down always strikes the in-scale note under the pen. What dragging the
/// pen sideways does next is the difference between an instrument that sustains
/// one note and one that fires a stream of notes:
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NoteMode {
    /// **Sustain (default).** The struck pitch is latched for as long as the pen
    /// is down; moving sideways does *not* change the note. Only Y (timbre) and
    /// pressure (aftertouch) shape the held sound — "press a note and play it."
    #[default]
    Hold,
    /// **Glide.** One note is struck, then its pitch slides continuously toward
    /// the pointed pitch via MPE pitch-bend (theremin-like). The note only
    /// retriggers if the required bend exceeds the configured bend range.
    Glide,
    /// **Keyboard.** Every in-scale note the pen crosses retriggers a new note
    /// (NoteOff the old, NoteOn the new). Good for fast runs, not for sustain.
    Discrete,
}

impl NoteMode {
    /// All variants in display order, for UI pickers.
    pub const ALL: [NoteMode; 3] = [NoteMode::Hold, NoteMode::Glide, NoteMode::Discrete];

    /// Short human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            NoteMode::Hold => "Hold (sustain one note)",
            NoteMode::Glide => "Glide (bend between notes)",
            NoteMode::Discrete => "Keyboard (retrigger per note)",
        }
    }
}

/// How horizontal pen movement behaves *after* a note is struck.
///
/// Pen-down always strikes the in-scale note under the pen. What dragging the
/// pen sideways does next is the difference between an instrument that sustains
/// one note and one that fires a stream of notes:
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NoteMode {
    /// **Sustain (default).** The struck pitch is latched for as long as the pen
    /// is down; moving sideways does *not* change the note. Only Y (timbre) and
    /// pressure (aftertouch) shape the held sound — "press a note and play it."
    #[default]
    Hold,
    /// **Glide.** One note is struck, then its pitch slides continuously toward
    /// the pointed pitch via MPE pitch-bend (theremin-like). The note only
    /// retriggers if the required bend exceeds the configured bend range.
    Glide,
    /// **Keyboard.** Every in-scale note the pen crosses retriggers a new note
    /// (NoteOff the old, NoteOn the new). Good for fast runs, not for sustain.
    Discrete,
}

impl NoteMode {
    /// All variants in display order, for UI pickers.
    pub const ALL: [NoteMode; 3] = [NoteMode::Hold, NoteMode::Glide, NoteMode::Discrete];

    /// Short human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            NoteMode::Hold => "Hold (sustain one note)",
            NoteMode::Glide => "Glide (bend between notes)",
            NoteMode::Discrete => "Keyboard (retrigger per note)",
        }
    }
}

/// Which tilt component feeds the optional extra CC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TiltAxis {
    X,
    Y,
}

/// Optional mapping of pen tilt onto a control-change message.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TiltCc {
    /// Controller number (0–127).
    pub controller: u8,
    /// Which tilt axis drives it.
    pub axis: TiltAxis,
    /// Tilt magnitude (degrees) mapped to CC value 127. Tilt is clamped to
    /// `[-range_deg, +range_deg]` and rescaled to `[0, 127]` (center 64).
    pub range_deg: f64,
}

/// Complete stylus→MPE mapping configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MidiMapping {
    /// Human-readable name.
    pub name: String,
    /// Scale used to quantize pitch.
    pub scale: ScaleKind,
    /// Root pitch class, 0 = C .. 11 = B.
    pub key: u8,
    /// MIDI note at the left edge of the surface (`x = 0`).
    pub low_note: u8,
    /// Number of semitones spanned across the surface (`x = 0 → 1`).
    pub span_notes: u8,
    /// What dragging the pen sideways does after a note is struck (sustain /
    /// glide / retrigger). Defaults to [`NoteMode::Hold`].
    #[serde(default)]
    pub mode: NoteMode,
    /// MPE channel layout + pitch-bend range.
    pub mpe: MpeConfig,
    /// Note-on velocity source.
    pub velocity: VelocitySource,
    /// Map vertical position to CC74 (timbre).
    pub y_to_cc74: bool,
    /// Invert Y so the top of the surface is the high CC value (screen Y grows
    /// downward, so this is on by default for an intuitive "up = brighter").
    pub y_invert: bool,
    /// Map pen pressure to channel pressure (per-note aftertouch).
    pub pressure_to_channel_pressure: bool,
    /// Optional extra CC driven by pen tilt.
    pub tilt_cc: Option<TiltCc>,
}

impl Default for MidiMapping {
    fn default() -> Self {
        Self {
            name: String::from("default"),
            scale: ScaleKind::MajorPentatonic,
            key: 0,
            low_note: 48, // C3
            span_notes: 24, // two octaves
            mode: NoteMode::Hold,
            mpe: MpeConfig::default(),
            velocity: VelocitySource::default(),
            y_to_cc74: true,
            y_invert: true,
            pressure_to_channel_pressure: true,
            tilt_cc: None,
        }
    }
}

/// Errors from [`MidiMapping::load`], [`MidiMapping::save`], and
/// [`MidiMapping::validate`].
#[derive(Debug, thiserror::Error)]
pub enum MappingError {
    #[error("I/O error for '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML parse error in '{path}': {message}")]
    TomlParse { path: String, message: String },
    #[error("JSON parse error in '{path}': {message}")]
    JsonParse { path: String, message: String },
    #[error("Invalid mapping field '{field}': {reason}")]
    Validation { field: &'static str, reason: String },
}

fn is_json(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
}

impl MidiMapping {
    /// Load a mapping from `path` (`.json` → JSON, else TOML), then validate.
    pub fn load(path: &Path) -> Result<Self, MappingError> {
        let path_str = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| MappingError::Io {
            path: path_str.clone(),
            source: e,
        })?;

        let mapping: MidiMapping = if is_json(path) {
            serde_json::from_str(&text).map_err(|e| MappingError::JsonParse {
                path: path_str,
                message: e.to_string(),
            })?
        } else {
            toml::from_str(&text).map_err(|e| MappingError::TomlParse {
                path: path_str,
                message: e.to_string(),
            })?
        };

        mapping.validate()?;
        Ok(mapping)
    }

    /// Save this mapping to `path` (`.json` → JSON pretty, else TOML).
    pub fn save(&self, path: &Path) -> Result<(), MappingError> {
        let path_str = path.display().to_string();
        let text = if is_json(path) {
            serde_json::to_string_pretty(self).map_err(|e| MappingError::Io {
                path: path_str.clone(),
                source: std::io::Error::other(e.to_string()),
            })?
        } else {
            toml::to_string_pretty(self).map_err(|e| MappingError::Io {
                path: path_str.clone(),
                source: std::io::Error::other(e.to_string()),
            })?
        };
        std::fs::write(path, text).map_err(|e| MappingError::Io {
            path: path_str,
            source: e,
        })
    }

    /// Validate channel layout, note range, and value ranges.
    pub fn validate(&self) -> Result<(), MappingError> {
        if self.key > 11 {
            return Err(MappingError::Validation {
                field: "key",
                reason: format!("key (pitch class) must be 0..=11, got {}", self.key),
            });
        }
        if self.span_notes == 0 {
            return Err(MappingError::Validation {
                field: "span_notes",
                reason: "span_notes must be >= 1".to_owned(),
            });
        }
        if i32::from(self.low_note) + i32::from(self.span_notes) > 127 {
            return Err(MappingError::Validation {
                field: "low_note / span_notes",
                reason: format!(
                    "low_note ({}) + span_notes ({}) must be <= 127",
                    self.low_note, self.span_notes
                ),
            });
        }
        let m = &self.mpe;
        if m.master_channel > 15 || m.member_low > 15 || m.member_high > 15 {
            return Err(MappingError::Validation {
                field: "mpe channels",
                reason: "all channels must be 0..=15".to_owned(),
            });
        }
        if m.member_low > m.member_high {
            return Err(MappingError::Validation {
                field: "mpe.member_low / member_high",
                reason: format!(
                    "member_low ({}) must be <= member_high ({})",
                    m.member_low, m.member_high
                ),
            });
        }
        if m.pitch_bend_range_semitones <= 0.0 {
            return Err(MappingError::Validation {
                field: "mpe.pitch_bend_range_semitones",
                reason: format!(
                    "pitch-bend range must be > 0, got {}",
                    m.pitch_bend_range_semitones
                ),
            });
        }
        if let VelocitySource::Pressure { min, max } = self.velocity {
            if min > max {
                return Err(MappingError::Validation {
                    field: "velocity (Pressure)",
                    reason: format!("velocity min ({min}) must be <= max ({max})"),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mapping_validates() {
        MidiMapping::default().validate().unwrap();
    }

    #[test]
    fn rejects_bad_channel_range() {
        let mut m = MidiMapping::default();
        m.mpe.member_low = 10;
        m.mpe.member_high = 3;
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_note_range_overflow() {
        let mut m = MidiMapping::default();
        m.low_note = 120;
        m.span_notes = 24;
        assert!(m.validate().is_err());
    }

    #[test]
    fn member_count_is_inclusive() {
        let m = MpeConfig::default();
        assert_eq!(m.member_count(), 15); // channels 1..=15
    }

    #[test]
    fn toml_round_trips() {
        let dir = std::env::temp_dir().join(format!("tablet-midi-map-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.midimap.toml");

        let mut m = MidiMapping::default();
        m.name = "round-trip".to_owned();
        m.scale = ScaleKind::Dorian;
        m.mode = NoteMode::Glide;
        m.tilt_cc = Some(TiltCc {
            controller: 1,
            axis: TiltAxis::X,
            range_deg: 60.0,
        });

        m.save(&path).unwrap();
        let loaded = MidiMapping::load(&path).unwrap();
        assert_eq!(loaded, m);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_round_trips() {
        let dir = std::env::temp_dir().join(format!("tablet-midi-map-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.midimap.json");

        let m = MidiMapping::default();
        m.save(&path).unwrap();
        let loaded = MidiMapping::load(&path).unwrap();
        assert_eq!(loaded, m);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
