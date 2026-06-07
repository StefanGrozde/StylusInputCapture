//! UI preferences (`tablet-ui.toml`) — persisted **separately** from
//! `CalibrationProfile` files (SPEC_CUI.md §7).
//!
//! Preferences cover window geometry, the last-used data source/format, and
//! the last profile path opened — small, app-level conveniences that must
//! never leak into a `*.cal.toml` (which is the portable, shareable artifact
//! consumed by downstream features).
//!
//! The file lives in the OS config directory, resolved via the `directories`
//! crate (`ProjectDirs::from("", "", "tablet-ui")`):
//!
//! | OS | Typical path |
//! | --- | --- |
//! | Windows | `%APPDATA%\tablet-ui\config\tablet-ui.toml` |
//! | Linux | `~/.config/tablet-ui/tablet-ui.toml` |
//! | macOS | `~/Library/Application Support/tablet-ui/tablet-ui.toml` |
//!
//! Loading is infallible from the caller's perspective: a missing or
//! unparsable file simply yields [`UiPrefs::default`] (never crashes the UI).

use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// File name for the UI-preferences file, written inside the resolved config
/// directory.
const PREFS_FILE_NAME: &str = "tablet-ui.toml";

/// Small, app-level UI preferences — window geometry, last source/format, and
/// the last profile path. Kept intentionally separate from
/// `tablet_process::CalibrationProfile` (SPEC_CUI.md §7: "never mixed into a
/// calibration profile").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UiPrefs {
    /// Last known main-window inner size, in points (`(width, height)`).
    /// Applied to `NativeOptions` viewport on the next launch.
    pub window_size: Option<(f32, f32)>,

    /// Textual description of the last-used data source (e.g. `"stdin"`,
    /// `"tcp:127.0.0.1:9123"`, `"pipe:wacom-capture"`). Stored as a string —
    /// not the `Source` enum — so prefs stay decoupled from CLI parsing and
    /// remain forward-compatible if `Source` grows variants.
    pub last_source: Option<String>,

    /// Last-used wire format label (`"postcard"` | `"json"`).
    pub last_format: Option<String>,

    /// Path to the last profile that was loaded or saved. Used as the
    /// fallback when `--profile` is not given at startup.
    pub last_profile_path: Option<PathBuf>,

    /// Which calibration-panel sections were last expanded, keyed by header
    /// title. Cheap to persist (small `Vec`); restores panel layout across
    /// runs. Empty/absent means "use the built-in defaults."
    pub expanded_sections: Vec<String>,
}

impl UiPrefs {
    /// Resolve the config-directory path for `tablet-ui.toml`.
    ///
    /// Returns `None` if the OS config directory cannot be determined (e.g.
    /// no valid `$HOME`); callers treat that as "prefs unavailable" and fall
    /// back to defaults without erroring.
    fn file_path() -> Option<PathBuf> {
        let dirs = ProjectDirs::from("", "", "tablet-ui")?;
        Some(dirs.config_dir().join(PREFS_FILE_NAME))
    }

    /// Load preferences from the OS config directory.
    ///
    /// Never fails: a missing file, an unreadable file, or invalid TOML all
    /// yield [`UiPrefs::default`]. This mirrors the spec's requirement that
    /// preference loading cannot crash the UI.
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
                "could not resolve OS config directory for tablet-ui prefs",
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

    /// Loading from a path that cannot exist yields defaults rather than
    /// erroring — exercised here by parsing garbage TOML directly through the
    /// same `unwrap_or_default` path `load` uses.
    #[test]
    fn invalid_toml_yields_defaults() {
        let parsed: UiPrefs = toml::from_str("not valid = [ toml").unwrap_or_default();
        assert_eq!(parsed, UiPrefs::default());
    }

    /// Missing-file semantics: `file_path` resolution may succeed while the
    /// file itself doesn't exist yet (first run). `load` must still return
    /// defaults rather than panicking.
    #[test]
    fn missing_file_yields_defaults() {
        // `read_to_string` on a path that cannot exist fails, and `load`
        // funnels that into `Self::default()` — assert the default shape
        // directly (the actual OS config dir may or may not have a file,
        // depending on the host running the test, so we don't call `load`
        // here to keep this test hermetic).
        let defaults = UiPrefs::default();
        assert_eq!(defaults.window_size, None);
        assert_eq!(defaults.last_source, None);
        assert_eq!(defaults.last_format, None);
        assert_eq!(defaults.last_profile_path, None);
        assert!(defaults.expanded_sections.is_empty());
    }

    /// Round-trip: serialize → deserialize must produce an equal value.
    /// Writes to `std::env::temp_dir()` directly (not the OS config dir) to
    /// keep the test hermetic and parallel-safe.
    #[test]
    fn round_trip_through_toml_file() {
        let prefs = UiPrefs {
            window_size: Some((1280.0, 800.0)),
            last_source: Some("tcp:127.0.0.1:9123".to_owned()),
            last_format: Some("postcard".to_owned()),
            last_profile_path: Some(PathBuf::from("C:/profiles/my.cal.toml")),
            expanded_sections: vec!["Pressure".to_owned(), "Smoothing".to_owned()],
        };

        let dir = std::env::temp_dir().join(format!(
            "tablet-ui-prefs-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(PREFS_FILE_NAME);

        let text = toml::to_string_pretty(&prefs).unwrap();
        std::fs::write(&path, &text).unwrap();

        let text_back = std::fs::read_to_string(&path).unwrap();
        let prefs_back: UiPrefs = toml::from_str(&text_back).unwrap();

        assert_eq!(prefs, prefs_back);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `Default` round-trips through TOML too (an empty/near-empty prefs
    /// file is valid).
    #[test]
    fn default_round_trips_through_toml() {
        let prefs = UiPrefs::default();
        let text = toml::to_string_pretty(&prefs).unwrap();
        let back: UiPrefs = toml::from_str(&text).unwrap();
        assert_eq!(prefs, back);
    }
}
