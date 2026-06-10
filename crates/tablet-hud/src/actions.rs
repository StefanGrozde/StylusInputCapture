//! The HUD's bindable-action registry.
//!
//! Every discrete thing the app can do on demand is an [`Action`]. The settings
//! system binds [`InputChord`](crate::bindings::InputChord)s to actions through
//! their **stable string ids** (`Action::id`), so saved `settings.toml` files
//! survive enum reordering and future additions. Adding a feature is three
//! steps: a variant here (plus `ALL`/`id`/`label`/`category` arms) and a match
//! arm in `HudApp::execute_action`.

/// A discrete, bindable app action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    // ── Performance ──────────────────────────────────────────────────────
    /// Release everything: NoteOff + All-Notes-Off sweep.
    Panic,
    /// Send a loud middle-C on the master channel (MIDI path check).
    TestNote,
    /// Shift the pad grid up one octave.
    OctaveUp,
    /// Shift the pad grid down one octave.
    OctaveDown,

    // ── Mapping ──────────────────────────────────────────────────────────
    /// Next scale in the picker order.
    ScaleNext,
    /// Previous scale in the picker order.
    ScalePrev,
    /// Root pitch class up a semitone.
    KeyNext,
    /// Root pitch class down a semitone.
    KeyPrev,
    /// Cycle the slide behavior (Latch → Glide → Pads).
    CycleNoteMode,
    /// Toggle drag left/right → CC74.
    ToggleXToCc74,
    /// Toggle pressure → channel pressure.
    TogglePressure,
    /// Toggle tilt → CC.
    ToggleTiltCc,

    // ── MIDI / session ───────────────────────────────────────────────────
    /// Connect to the selected MIDI output port.
    MidiConnect,
    /// Disconnect (after a panic sweep).
    MidiDisconnect,
    /// Re-enumerate MIDI output ports.
    RefreshPorts,
    /// Load the mapping file from the path field.
    LoadMapping,
    /// Save the mapping to the path field.
    SaveMapping,

    // ── App ──────────────────────────────────────────────────────────────
    /// Show / hide the Settings window.
    ToggleSettings,
}

impl Action {
    /// Every action, in settings-window display order.
    pub const ALL: [Action; 18] = [
        Action::Panic,
        Action::TestNote,
        Action::OctaveUp,
        Action::OctaveDown,
        Action::ScaleNext,
        Action::ScalePrev,
        Action::KeyNext,
        Action::KeyPrev,
        Action::CycleNoteMode,
        Action::ToggleXToCc74,
        Action::TogglePressure,
        Action::ToggleTiltCc,
        Action::MidiConnect,
        Action::MidiDisconnect,
        Action::RefreshPorts,
        Action::LoadMapping,
        Action::SaveMapping,
        Action::ToggleSettings,
    ];

    /// Stable string id used in `settings.toml`. **Never change an existing
    /// id** — saved bindings reference actions by this string.
    pub fn id(self) -> &'static str {
        match self {
            Action::Panic => "panic",
            Action::TestNote => "test_note",
            Action::OctaveUp => "octave_up",
            Action::OctaveDown => "octave_down",
            Action::ScaleNext => "scale_next",
            Action::ScalePrev => "scale_prev",
            Action::KeyNext => "key_next",
            Action::KeyPrev => "key_prev",
            Action::CycleNoteMode => "cycle_note_mode",
            Action::ToggleXToCc74 => "toggle_x_to_cc74",
            Action::TogglePressure => "toggle_pressure",
            Action::ToggleTiltCc => "toggle_tilt_cc",
            Action::MidiConnect => "midi_connect",
            Action::MidiDisconnect => "midi_disconnect",
            Action::RefreshPorts => "refresh_ports",
            Action::LoadMapping => "load_mapping",
            Action::SaveMapping => "save_mapping",
            Action::ToggleSettings => "toggle_settings",
        }
    }

    /// Look an action up by its stable id (unknown ids — e.g. from a newer
    /// settings file — yield `None` and are skipped by the keymap).
    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }

    /// Human-readable label for the settings window.
    pub fn label(self) -> &'static str {
        match self {
            Action::Panic => "Panic (all notes off)",
            Action::TestNote => "Test note",
            Action::OctaveUp => "Octave up",
            Action::OctaveDown => "Octave down",
            Action::ScaleNext => "Next scale",
            Action::ScalePrev => "Previous scale",
            Action::KeyNext => "Key up (semitone)",
            Action::KeyPrev => "Key down (semitone)",
            Action::CycleNoteMode => "Cycle slide behavior",
            Action::ToggleXToCc74 => "Toggle drag → CC74",
            Action::TogglePressure => "Toggle pressure → channel pressure",
            Action::ToggleTiltCc => "Toggle tilt → CC",
            Action::MidiConnect => "Connect MIDI port",
            Action::MidiDisconnect => "Disconnect MIDI port",
            Action::RefreshPorts => "Refresh MIDI ports",
            Action::LoadMapping => "Load mapping file",
            Action::SaveMapping => "Save mapping file",
            Action::ToggleSettings => "Show/hide settings",
        }
    }

    /// Settings-window group header.
    pub fn category(self) -> &'static str {
        match self {
            Action::Panic | Action::TestNote | Action::OctaveUp | Action::OctaveDown => {
                "Performance"
            }
            Action::ScaleNext
            | Action::ScalePrev
            | Action::KeyNext
            | Action::KeyPrev
            | Action::CycleNoteMode
            | Action::ToggleXToCc74
            | Action::TogglePressure
            | Action::ToggleTiltCc => "Mapping",
            Action::MidiConnect
            | Action::MidiDisconnect
            | Action::RefreshPorts
            | Action::LoadMapping
            | Action::SaveMapping => "MIDI & files",
            Action::ToggleSettings => "App",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique_and_round_trip() {
        let mut seen = HashSet::new();
        for action in Action::ALL {
            assert!(seen.insert(action.id()), "duplicate id: {}", action.id());
            assert_eq!(Action::from_id(action.id()), Some(action));
        }
    }

    #[test]
    fn unknown_id_yields_none() {
        assert_eq!(Action::from_id("warp_drive"), None);
    }

    #[test]
    fn every_action_has_label_and_category() {
        for action in Action::ALL {
            assert!(!action.label().is_empty());
            assert!(!action.category().is_empty());
        }
    }
}
