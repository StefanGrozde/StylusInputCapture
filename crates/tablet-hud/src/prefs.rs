//! HUD preferences (`tablet-hud.toml`) — small app-level conveniences,
//! persisted separately from `MidiMapping` files (which are the portable,
//! shareable artifacts).
//!
//! Mirrors `tablet-ui`'s `UiPrefs`: the file lives in the OS config directory
//! resolved via the `directories` crate (`ProjectDirs::from("", "",
//! "tablet-hud")`):
//!
//! | OS | Typical path |
//! | --- | --- |
//! | Windows | `%APPDATA%\tablet-hud\config\tablet-hud.toml` |
//! | Linux | `~/.config/tablet-hud/tablet-hud.toml` |
//! | macOS | `~/Library/Application Support/tablet-hud/tablet-hud.toml` |
//!
//! Loading is infallible from the caller's perspective: a missing or
//! unparsable file simply yields [`HudPrefs::default`] (never crashes the UI).

use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::pen_guide::PenSignature;

/// File name for the HUD-preferences file, written inside the resolved config
/// directory.
const PREFS_FILE_NAME: &str = "tablet-hud.toml";

/// Window geometry, the last MIDI port, and the last mapping path — restored
/// on the next launch so the app comes back the way it was left.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HudPrefs {
    /// Last known main-window inner size, in points (`(width, height)`).
    /// Applied to `NativeOptions` viewport on the next launch.
    pub window_size: Option<(f32, f32)>,

    /// Name of the last-used MIDI output port. On startup, if it matches a
    /// name in `MidiOut::list_ports()`, it is preselected in the combo —
    /// **select only, never auto-connect** (sending MPE init messages to a
    /// port unprompted would be surprising).
    pub last_midi_port: Option<String>,

    /// Path to the last mapping that was loaded or saved. Prefills the
    /// mapping-path field when `--mapping` is not given at startup.
    pub last_mapping_path: Option<PathBuf>,

    /// Pens already introduced by the pen-guide modal; matching pens never
    /// re-trigger it. Cleared by deleting this file. `#[serde(default)]` so
    /// prefs files written before this field existed keep loading whole.
    #[serde(default)]
    pub seen_pens: Vec<PenSignature>,
}

impl HudPrefs {
    /// Resolve the config-directory path for `tablet-hud.toml`.
    ///
    /// Returns `None` if the OS config directory cannot be determined (e.g.
    /// no valid `$HOME`); callers treat that as "prefs unavailable" and fall
    /// back to defaults without erroring.
    fn file_path() -> Option<PathBuf> {
        let dirs = ProjectDirs::from("", "", "tablet-hud")?;
        Some(dirs.config_dir().join(PREFS_FILE_NAME))
    }

    /// Load preferences from the OS config directory.
    ///
    /// Never fails: a missing file, an unreadable file, or invalid TOML all
    /// yield [`HudPrefs::default`].
    pub fn load() -> Self {
        let Some(path) = Self::file_path() else {
            return Self::default();
        };

        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };

        toml::from_str(&text).unwrap_or_default()
    }

    /// Save preferences to the OS config directory, creating it if missing.
    ///
    /// Best-effort: I/O or serialization failures are swallowed (preferences
    /// are a convenience, not load-bearing for correctness) but the error is
    /// returned so callers may log it.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::file_path() else {
            return Err(std::io::Error::other(
                "could not resolve OS config directory for tablet-hud prefs",
            ));
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invalid TOML must fall back to defaults rather than erroring —
    /// exercised through the same `unwrap_or_default` path `load` uses.
    #[test]
    fn invalid_toml_yields_defaults() {
        let parsed: HudPrefs = toml::from_str("not valid = [ toml").unwrap_or_default();
        assert_eq!(parsed, HudPrefs::default());
    }

    /// Round-trip: serialize → deserialize must produce an equal value.
    /// Writes to `std::env::temp_dir()` directly (not the OS config dir) to
    /// keep the test hermetic and parallel-safe.
    #[test]
    fn round_trip_through_toml_file() {
        let prefs = HudPrefs {
            window_size: Some((1280.0, 800.0)),
            last_midi_port: Some("Synth MIDI In".to_owned()),
            last_mapping_path: Some(PathBuf::from("C:/mappings/lead.midimap.toml")),
            seen_pens: vec![PenSignature {
                device_name: "Wacom Intuos Pro".to_owned(),
                tool_serial: 1,
                has_tilt: true,
                ..PenSignature::default()
            }],
        };

        let dir = std::env::temp_dir().join(format!(
            "tablet-hud-prefs-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(PREFS_FILE_NAME);

        let text = toml::to_string_pretty(&prefs).unwrap();
        std::fs::write(&path, &text).unwrap();

        let text_back = std::fs::read_to_string(&path).unwrap();
        let prefs_back: HudPrefs = toml::from_str(&text_back).unwrap();

        assert_eq!(prefs, prefs_back);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A prefs file written before `seen_pens` existed must load its other
    /// fields intact (not collapse to whole-struct defaults).
    #[test]
    fn file_without_seen_pens_keeps_other_fields() {
        let parsed: HudPrefs =
            toml::from_str("window_size = [1024.0, 768.0]\nlast_midi_port = \"loopMIDI\"")
                .unwrap();
        assert_eq!(parsed.window_size, Some((1024.0, 768.0)));
        assert_eq!(parsed.last_midi_port.as_deref(), Some("loopMIDI"));
        assert!(parsed.seen_pens.is_empty());
    }

    /// `Default` round-trips through TOML too (an empty/near-empty prefs file
    /// is valid).
    #[test]
    fn default_round_trips_through_toml() {
        let prefs = HudPrefs::default();
        let text = toml::to_string_pretty(&prefs).unwrap();
        let back: HudPrefs = toml::from_str(&text).unwrap();
        assert_eq!(prefs, back);
    }
}
