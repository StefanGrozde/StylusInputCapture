//! App settings (`settings.toml`) — the keybinding map and future app-level
//! options, persisted in the OS config directory next to the prefs file.
//!
//! Split from [`HudPrefs`](crate::prefs::HudPrefs) on purpose: prefs are
//! incidental state the app remembers (window size, last port), settings are
//! **deliberate user configuration** the user edits. Mapping files
//! (`*.midimap.toml`) stay portable instrument presets; bindings are personal
//! and live here.
//!
//! Same persistence contract as prefs: loading never fails (missing or
//! invalid files yield defaults) and saving is best-effort.

use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::bindings::Keymap;

/// File name for the settings file, inside the resolved config directory.
const SETTINGS_FILE_NAME: &str = "settings.toml";

/// All persisted app settings. Every field carries `#[serde(default)]` so
/// files written by older versions keep loading as new fields are added.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HudSettings {
    /// Input chord → action bindings.
    #[serde(default)]
    pub keymap: Keymap,
}

impl HudSettings {
    /// Resolve the config-directory path for `settings.toml` (same directory
    /// as the prefs file).
    fn file_path() -> Option<PathBuf> {
        let dirs = ProjectDirs::from("", "", "tablet-hud")?;
        Some(dirs.config_dir().join(SETTINGS_FILE_NAME))
    }

    /// Load settings from the OS config directory. Never fails: a missing,
    /// unreadable, or invalid file yields [`HudSettings::default`].
    pub fn load() -> Self {
        let Some(path) = Self::file_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_default()
    }

    /// Save settings to the OS config directory, creating it if missing.
    /// Best-effort; the error is returned so callers may log it.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::file_path() else {
            return Err(std::io::Error::other(
                "could not resolve OS config directory for tablet-hud settings",
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
    use crate::actions::Action;
    use crate::bindings::{InputChord, PenButton};

    #[test]
    fn invalid_toml_yields_defaults() {
        let parsed: HudSettings = toml::from_str("not valid = [ toml").unwrap_or_default();
        assert_eq!(parsed, HudSettings::default());
    }

    #[test]
    fn empty_file_yields_defaults() {
        let parsed: HudSettings = toml::from_str("").unwrap_or_default();
        assert_eq!(parsed, HudSettings::default());
        // The default keymap is the legacy shortcut set, not empty.
        assert!(!parsed.keymap.bindings.is_empty());
    }

    /// Round-trip through an actual file in a temp dir (not the OS config
    /// dir), keeping the test hermetic and parallel-safe.
    #[test]
    fn round_trip_through_toml_file() {
        let mut settings = HudSettings::default();
        settings
            .keymap
            .bind(InputChord::Pen(PenButton::Barrel), Action::Panic);
        settings
            .keymap
            .bind(InputChord::Tablet { index: 1 }, Action::OctaveUp);

        let dir = std::env::temp_dir().join(format!(
            "tablet-hud-settings-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SETTINGS_FILE_NAME);

        let text = toml::to_string_pretty(&settings).unwrap();
        std::fs::write(&path, &text).unwrap();
        let back: HudSettings = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings, back);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
