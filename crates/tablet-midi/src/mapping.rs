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

/// The pad-grid layout of the playing surface (Push-style).
///
/// The surface is a `rows × cols` grid of square pads, each sounding one
/// in-scale note. Row 0 is the **bottom** row (low notes at the bottom, like a
/// Push), and each row up is `row_interval_degrees` scale degrees higher than
/// the pad below it — the Push default of 3 degrees is "in 4ths" for a
/// 7-note scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridConfig {
    /// Number of pad rows.
    pub rows: u8,
    /// Number of pad columns.
    pub cols: u8,
    /// Scale degrees a row is offset from the row below it.
    pub row_interval_degrees: u8,
}

impl Default for GridConfig {
    fn default() -> Self {
        Self {
            rows: 8,
            cols: 8,
            row_interval_degrees: 3,
        }
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

/// What sliding the pen onto a *different pad* does while a note is held.
///
/// Pen-down always strikes the pad under the pen, and the note sustains for as
/// long as the pen stays pressed. These modes only differ once the pen slides
/// out of the struck pad:
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NoteMode {
    /// **Latch (default).** The struck pad is held for as long as the pen is
    /// down; sliding onto other pads does *not* change the note. Dragging only
    /// modulates the held sound: up/down → pitch bend, left/right → CC74,
    /// pressure → channel pressure, tilt → CC.
    #[default]
    Hold,
    /// **Glide.** One note is struck, then its pitch bends continuously toward
    /// the pad under the pen (theremin-like). The note only retriggers if the
    /// required bend exceeds the configured bend range.
    Glide,
    /// **Pads.** Each pad is a key: sliding onto a new pad retriggers
    /// (NoteOff the old, NoteOn the new pad's note) — Push-like drum pads.
    Discrete,
}

impl NoteMode {
    /// All variants in display order, for UI pickers.
    pub const ALL: [NoteMode; 3] = [NoteMode::Hold, NoteMode::Glide, NoteMode::Discrete];

    /// Short human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            NoteMode::Hold => "Latch (drag modulates, note stays)",
            NoteMode::Glide => "Glide (bend between pads)",
            NoteMode::Discrete => "Pads (retrigger per pad)",
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
    /// MIDI note of the bottom-left pad.
    pub low_note: u8,
    /// Pad-grid layout (rows, columns, per-row interval).
    #[serde(default)]
    pub grid: GridConfig,
    /// Semitones of pitch bend for dragging the pen vertically across the full
    /// surface height, measured from where the note was struck (up = higher).
    /// 0 disables. Applies in Latch/Pads modes; Glide bends toward the pointed
    /// pad instead.
    #[serde(default = "default_y_bend_semitones")]
    pub y_bend_semitones: f64,
    /// What sliding onto a different pad does while a note is held (latch /
    /// glide / retrigger). Defaults to [`NoteMode::Hold`].
    #[serde(default)]
    pub mode: NoteMode,
    /// MPE channel layout + pitch-bend range.
    pub mpe: MpeConfig,
    /// Note-on velocity source.
    pub velocity: VelocitySource,
    /// Drive CC74 (timbre / "frequency" / brightness) from horizontal pen
    /// movement, measured from where the note was struck: the strike is the
    /// neutral center (64), dragging right brightens, left darkens.
    #[serde(default = "default_true", alias = "y_to_cc74")]
    pub x_to_cc74: bool,
    /// Map pen pressure to channel pressure (per-note aftertouch).
    pub pressure_to_channel_pressure: bool,
    /// Optional extra CC driven by pen tilt.
    pub tilt_cc: Option<TiltCc>,
}

fn default_y_bend_semitones() -> f64 {
    12.0
}

fn default_true() -> bool {
    true
}

impl Default for MidiMapping {
    fn default() -> Self {
        Self {
            name: String::from("default"),
            scale: ScaleKind::MajorPentatonic,
            key: 0,
            low_note: 48, // C3
            grid: GridConfig::default(),
            y_bend_semitones: default_y_bend_semitones(),
            mode: NoteMode::Hold,
            mpe: MpeConfig::default(),
            velocity: VelocitySource::default(),
            x_to_cc74: true,
            pressure_to_channel_pressure: true,
            // Tilt modulates out of the box: pen tilt X → mod wheel.
            tilt_cc: Some(TiltCc {
                controller: 1,
                axis: TiltAxis::X,
                range_deg: 60.0,
            }),
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
        if self.grid.rows == 0 || self.grid.cols == 0 {
            return Err(MappingError::Validation {
                field: "grid.rows / grid.cols",
                reason: format!(
                    "grid must have at least one row and column, got {}x{}",
                    self.grid.rows, self.grid.cols
                ),
            });
        }
        if self.grid.row_interval_degrees == 0 {
            return Err(MappingError::Validation {
                field: "grid.row_interval_degrees",
                reason: "row interval must be >= 1 scale degree".to_owned(),
            });
        }
        if self.low_note > 127 {
            return Err(MappingError::Validation {
                field: "low_note",
                reason: format!("low_note must be <= 127, got {}", self.low_note),
            });
        }
        if self.y_bend_semitones.is_nan() || self.y_bend_semitones < 0.0 {
            return Err(MappingError::Validation {
                field: "y_bend_semitones",
                reason: format!(
                    "drag bend must be >= 0 semitones, got {}",
                    self.y_bend_semitones
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
    fn rejects_empty_grid() {
        let mut m = MidiMapping::default();
        m.grid.rows = 0;
        assert!(m.validate().is_err());
        m.grid.rows = 8;
        m.grid.cols = 0;
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_negative_drag_bend() {
        let mut m = MidiMapping::default();
        m.y_bend_semitones = -1.0;
        assert!(m.validate().is_err());
    }

    #[test]
    fn legacy_mapping_without_grid_fields_loads_with_defaults() {
        // A pre-grid mapping file: no [grid] table, no y_bend_semitones, and
        // now-removed span_notes / y_invert keys that must be ignored. The old
        // y_to_cc74 key aliases onto x_to_cc74.
        let dir = std::env::temp_dir().join(format!("tablet-midi-map-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.midimap.toml");
        std::fs::write(
            &path,
            r#"
name = "legacy"
scale = "MajorPentatonic"
key = 0
low_note = 48
span_notes = 24
y_to_cc74 = true
y_invert = true
pressure_to_channel_pressure = true

[mpe]
master_channel = 0
member_low = 1
member_high = 15
pitch_bend_range_semitones = 48.0

[velocity.Pressure]
min = 40
max = 127
"#,
        )
        .unwrap();

        let loaded = MidiMapping::load(&path).unwrap();
        assert_eq!(loaded.grid, GridConfig::default());
        assert_eq!(loaded.y_bend_semitones, default_y_bend_semitones());
        assert_eq!(loaded.mode, NoteMode::Hold);
        assert!(loaded.x_to_cc74, "legacy y_to_cc74 key must alias");

        let _ = std::fs::remove_dir_all(&dir);
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
