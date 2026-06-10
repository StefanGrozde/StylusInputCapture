//! Input chords and the [`Keymap`] binding them to actions.
//!
//! An [`InputChord`] is anything the user can press to trigger an
//! [`Action`](crate::actions::Action): a keyboard key (with modifiers, while
//! the HUD is focused), a stylus button (barrel / eraser — these arrive with
//! the pen stream, so they work even while another app is focused), or a
//! physical tablet button (ExpressKey, via the native pad capture).
//!
//! Chords serialize as **human-readable strings** so `settings.toml` stays
//! hand-editable: `"Ctrl+Shift+T"`, `"PageUp"`, `"pen:barrel"`, `"tablet:3"`.

use egui::Key;
use serde::{Deserialize, Serialize};

use crate::actions::Action;

/// `PenSample.buttons` bit for the barrel (side) switch — SPEC_BGC §6.1.
/// (Defined in `tablet-rawinput`, which is not a dependency of the HUD; the
/// bit assignments are part of the wire contract.)
pub const PEN_BUTTON_BIT_BARREL: u32 = 1;
/// `PenSample.buttons` bit for the eraser switch — SPEC_BGC §6.1.
pub const PEN_BUTTON_BIT_ERASER: u32 = 2;

/// A bindable stylus button (the tip is reserved for playing notes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenButton {
    /// The barrel (side) switch.
    Barrel,
    /// The eraser switch.
    Eraser,
}

impl PenButton {
    /// The `PenSample.buttons` bit carrying this button.
    pub fn bit(self) -> u32 {
        match self {
            PenButton::Barrel => PEN_BUTTON_BIT_BARREL,
            PenButton::Eraser => PEN_BUTTON_BIT_ERASER,
        }
    }
}

/// A single input gesture a binding can attach to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputChord {
    /// Keyboard key press while the HUD window is focused.
    Key {
        key: Key,
        ctrl: bool,
        shift: bool,
        alt: bool,
    },
    /// Stylus button press (works in the background).
    Pen(PenButton),
    /// Physical tablet button press, by stable per-device index.
    Tablet { index: u8 },
}

impl InputChord {
    /// Plain key chord with no modifiers.
    pub fn key(key: Key) -> Self {
        InputChord::Key {
            key,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    /// The serialized form (also shown in the settings window).
    pub fn label(&self) -> String {
        match self {
            InputChord::Key {
                key,
                ctrl,
                shift,
                alt,
            } => {
                let mut s = String::new();
                if *ctrl {
                    s.push_str("Ctrl+");
                }
                if *shift {
                    s.push_str("Shift+");
                }
                if *alt {
                    s.push_str("Alt+");
                }
                s.push_str(key.name());
                s
            }
            InputChord::Pen(PenButton::Barrel) => "pen:barrel".to_owned(),
            InputChord::Pen(PenButton::Eraser) => "pen:eraser".to_owned(),
            InputChord::Tablet { index } => format!("tablet:{index}"),
        }
    }

    /// Parse the serialized form back. Modifier prefixes are matched from the
    /// front so key names containing `+` (egui's "Plus" is named `+`) still
    /// parse.
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(rest) = text.strip_prefix("pen:") {
            return match rest {
                "barrel" => Some(InputChord::Pen(PenButton::Barrel)),
                "eraser" => Some(InputChord::Pen(PenButton::Eraser)),
                _ => None,
            };
        }
        if let Some(rest) = text.strip_prefix("tablet:") {
            return rest.parse().ok().map(|index| InputChord::Tablet { index });
        }

        let mut rest = text;
        let (mut ctrl, mut shift, mut alt) = (false, false, false);
        loop {
            if let Some(r) = rest.strip_prefix("Ctrl+") {
                ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("Shift+") {
                shift = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("Alt+") {
                alt = true;
                rest = r;
            } else {
                break;
            }
        }
        Key::from_name(rest).map(|key| InputChord::Key {
            key,
            ctrl,
            shift,
            alt,
        })
    }
}

impl Serialize for InputChord {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.label())
    }
}

impl<'de> Deserialize<'de> for InputChord {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        InputChord::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown input chord '{text}'")))
    }
}

/// One persisted binding: an input chord and the **id** of the action it
/// triggers. The id (not the enum) is stored so settings files survive
/// additions; unknown ids are kept on disk but never resolve.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub input: InputChord,
    pub action: String,
}

/// The full chord → action map. Invariants kept by [`Keymap::bind`]: at most
/// one binding per chord and at most one binding per action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keymap {
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

impl Default for Keymap {
    /// Today's hardcoded shortcuts, preserved as the out-of-the-box keymap.
    /// Pen / tablet buttons start unbound — firing MIDI actions from a pen
    /// button the user never configured would be surprising.
    fn default() -> Self {
        Self {
            bindings: vec![
                Binding {
                    input: InputChord::key(Key::Escape),
                    action: Action::Panic.id().to_owned(),
                },
                Binding {
                    input: InputChord::key(Key::T),
                    action: Action::TestNote.id().to_owned(),
                },
                Binding {
                    input: InputChord::key(Key::PageUp),
                    action: Action::OctaveUp.id().to_owned(),
                },
                Binding {
                    input: InputChord::key(Key::PageDown),
                    action: Action::OctaveDown.id().to_owned(),
                },
            ],
        }
    }
}

impl Keymap {
    /// The action bound to `chord`, if any (unknown action ids resolve to
    /// `None`, so a settings file from a newer version degrades gracefully).
    pub fn resolve(&self, chord: &InputChord) -> Option<Action> {
        self.bindings
            .iter()
            .find(|b| &b.input == chord)
            .and_then(|b| Action::from_id(&b.action))
    }

    /// The chord bound to `action`, if any.
    pub fn binding_for(&self, action: Action) -> Option<&InputChord> {
        self.bindings
            .iter()
            .find(|b| b.action == action.id())
            .map(|b| &b.input)
    }

    /// Bind `chord` to `action`, replacing any previous binding of either the
    /// chord or the action. Returns the action the chord was moved away from,
    /// if it displaced one (for the settings window's status line).
    pub fn bind(&mut self, chord: InputChord, action: Action) -> Option<Action> {
        let displaced = self
            .bindings
            .iter()
            .find(|b| b.input == chord && b.action != action.id())
            .and_then(|b| Action::from_id(&b.action));
        self.bindings
            .retain(|b| b.input != chord && b.action != action.id());
        self.bindings.push(Binding {
            input: chord,
            action: action.id().to_owned(),
        });
        displaced
    }

    /// Remove the binding for `action`, if any.
    pub fn clear(&mut self, action: Action) {
        self.bindings.retain(|b| b.action != action.id());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_labels_round_trip_through_parse() {
        let chords = [
            InputChord::key(Key::Escape),
            InputChord::Key {
                key: Key::T,
                ctrl: true,
                shift: true,
                alt: false,
            },
            InputChord::Key {
                key: Key::Plus, // name is "+": exercises the modifier parser
                ctrl: true,
                shift: false,
                alt: true,
            },
            InputChord::Pen(PenButton::Barrel),
            InputChord::Pen(PenButton::Eraser),
            InputChord::Tablet { index: 7 },
        ];
        for chord in chords {
            assert_eq!(InputChord::parse(&chord.label()), Some(chord));
        }
    }

    #[test]
    fn invalid_chords_fail_to_parse() {
        for text in ["", "pen:tip", "tablet:not-a-number", "Ctrl+", "NoSuchKey"] {
            assert_eq!(InputChord::parse(text), None, "should reject '{text}'");
        }
    }

    #[test]
    fn keymap_round_trips_through_toml() {
        let mut keymap = Keymap::default();
        keymap.bind(InputChord::Pen(PenButton::Barrel), Action::Panic);
        keymap.bind(InputChord::Tablet { index: 2 }, Action::ScaleNext);

        let text = toml::to_string_pretty(&keymap).unwrap();
        let back: Keymap = toml::from_str(&text).unwrap();
        assert_eq!(keymap, back);
    }

    #[test]
    fn defaults_match_legacy_shortcuts() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.resolve(&InputChord::key(Key::Escape)),
            Some(Action::Panic)
        );
        assert_eq!(
            keymap.resolve(&InputChord::key(Key::T)),
            Some(Action::TestNote)
        );
        assert_eq!(
            keymap.resolve(&InputChord::key(Key::PageUp)),
            Some(Action::OctaveUp)
        );
        assert_eq!(
            keymap.resolve(&InputChord::key(Key::PageDown)),
            Some(Action::OctaveDown)
        );
        // Modifiers distinguish chords: Ctrl+T is not the T binding.
        assert_eq!(
            keymap.resolve(&InputChord::Key {
                key: Key::T,
                ctrl: true,
                shift: false,
                alt: false,
            }),
            None
        );
    }

    #[test]
    fn bind_replaces_both_chord_and_action_bindings() {
        let mut keymap = Keymap::default();

        // Rebinding T (currently TestNote) to Panic displaces TestNote and
        // removes Panic's old Escape binding.
        let displaced = keymap.bind(InputChord::key(Key::T), Action::Panic);
        assert_eq!(displaced, Some(Action::TestNote));
        assert_eq!(keymap.resolve(&InputChord::key(Key::T)), Some(Action::Panic));
        assert_eq!(keymap.resolve(&InputChord::key(Key::Escape)), None);
        assert_eq!(keymap.binding_for(Action::TestNote), None);

        // Re-binding the same chord to the same action displaces nothing.
        assert_eq!(keymap.bind(InputChord::key(Key::T), Action::Panic), None);
    }

    #[test]
    fn clear_removes_only_that_action() {
        let mut keymap = Keymap::default();
        keymap.clear(Action::TestNote);
        assert_eq!(keymap.binding_for(Action::TestNote), None);
        assert!(keymap.binding_for(Action::Panic).is_some());
    }

    #[test]
    fn unknown_action_ids_are_kept_but_never_resolve() {
        let text = r#"
            [[bindings]]
            input = "F9"
            action = "future_feature"

            [[bindings]]
            input = "Escape"
            action = "panic"
        "#;
        let keymap: Keymap = toml::from_str(text).unwrap();
        assert_eq!(keymap.bindings.len(), 2);
        assert_eq!(
            keymap.resolve(&InputChord::parse("F9").unwrap()),
            None,
            "unknown action id must not resolve"
        );
        assert_eq!(
            keymap.resolve(&InputChord::key(Key::Escape)),
            Some(Action::Panic)
        );
    }
}
