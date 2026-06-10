# Settings & Keybinding System — Specification (SPEC_SETTINGS_SYSTEM.md)

> Reference for the HUD's settings system: the bindable-action registry, the
> input-chord/keymap model, persistence, and — the main purpose of this
> document — the **recipes for extending it** when future features are added.
>
> Status: implemented. Companion inventory: `docs/feature-map.md` (the live
> table of every action, control, and binding source). Update both when the
> system grows.

---

## 1. Overview

### 1.1 Purpose
Every discrete thing the HUD can do on demand is an **action** with a stable
identity. Users bind actions to **input chords** — keyboard keys, stylus
buttons, or physical tablet ExpressKeys — through a settings window, and the
bindings persist in `settings.toml`. The design goal is that adding a new
feature to this system is a small, local, mechanical change that can never
corrupt existing user configuration.

### 1.2 Design principles

1. **Stable ids are the contract.** Bindings are persisted against an action's
   string id (`"panic"`, `"octave_up"`), never its enum discriminant. Ids are
   append-only: new ones may be added, existing ones must never be renamed or
   reused.
2. **One dispatch path.** UI buttons and keybindings both go through
   `HudApp::execute_action`. A feature triggered two ways cannot drift apart.
3. **Loading never fails.** A missing, unreadable, or invalid settings file
   yields defaults; an unknown action id (from a newer version's file) is kept
   on disk but simply never resolves. Settings from any version load in any
   other version.
4. **Settings ≠ prefs ≠ presets.** `settings.toml` is deliberate user
   configuration (keybinds, future options). `tablet-hud.toml` is incidental
   state the app remembers (window size, last port). `*.midimap.toml` files
   are portable instrument presets. New persistent state must be placed in the
   right one of these three.

## 2. Architecture

```
 keyboard (egui, HUD focused)   stylus buttons             tablet ExpressKeys
        │                       (PenSample.buttons,        (StreamMessage::
        │                        edge-detected; global)     TabletButton; global)
        │                              │                          │
        └──────────────┬───────────────┴──────────────────────────┘
                       ▼
              InputChord  (bindings.rs)
                       │
        listening? ────┤  HudApp::on_chord (app.rs)
        yes: capture   │  no: Keymap::resolve → Action
        as binding     ▼
              HudApp::execute_action  ◄─── UI buttons call this too
                       │
              match arm per Action variant
```

| Piece | File | Role |
| --- | --- | --- |
| `Action` | `crates/tablet-hud/src/actions.rs` | Registry: variant + `ALL` + `id`/`label`/`category`/`from_id` |
| `InputChord`, `PenButton`, `Keymap` | `crates/tablet-hud/src/bindings.rs` | Chord model, string (de)serialization, chord→action map |
| `HudSettings` | `crates/tablet-hud/src/settings.rs` | Persistence (`settings.toml` in the OS config dir) |
| Settings window | `crates/tablet-hud/src/settings_ui.rs` | Per-action Bind/Clear editor, listen mode, reset |
| Dispatch & input collection | `crates/tablet-hud/src/app.rs` | `on_chord`, `execute_action`, keyboard/pen/tablet event collection |

**Listen mode:** `HudApp::listening: Option<Action>` — set by the settings
window's **Bind…** button. While set, the next chord from *any* source is
captured as that action's binding instead of dispatched; a bare `Escape`
cancels. Closing the settings window clears listen mode.

**Keymap invariants** (enforced by `Keymap::bind`): at most one binding per
chord and one binding per action; rebinding displaces both sides and returns
what was displaced so the UI can report it.

## 3. Extension recipes

### 3.1 Add a new bindable action (the common case)

Three local edits, all in `tablet-hud`:

1. **Register it** — `actions.rs`: add the variant, append it to `Action::ALL`
   (display order = settings-window order), and add arms to `id()`, `label()`,
   and `category()`. Pick the id once and keep it forever; pick an existing
   category or introduce a new string (the window groups by it automatically).
2. **Implement it** — `app.rs`: add the match arm in
   `HudApp::execute_action`. Keep the body a call to a named method if it's
   more than a couple of lines.
3. **Wire any button** — if the feature also has a UI button, the button's
   click handler must call `self.execute_action(Action::YourAction)` (and use
   `self.action_hover(...)` for its tooltip so the hover text shows the live
   binding).

That's all. The settings window, persistence, conflict handling, and every
input source pick the action up automatically. Do **not** add a default
binding unless the action is as universal as panic — unbound is the right
default for almost everything (surprising the user with a pre-bound pen
button is worse than one trip to the settings window).

Then update the action table in `docs/feature-map.md`, and extend the
`actions::tests` expectations only if you changed structural behavior — the
existing `ids_are_unique_and_round_trip` test covers the new variant by
iterating `ALL`.

**What does *not* belong in the registry:** continuous controls (sliders,
combos — they aren't chord-shaped), platform-conditional one-offs (the
Virtual-port button), and the pen tip (reserved for playing notes).

### 3.2 Add a new app-level setting (not a keybind)

1. Add a field to `HudSettings` (`settings.rs`) with `#[serde(default)]` —
   this is mandatory, it is what keeps old files loading.
2. If the default is not `Default::default()`, give the field a
   `#[serde(default = "fn_name")]` provider (see `MidiMapping` in
   `tablet-midi` for the pattern).
3. Surface it in the settings window (`settings_ui.rs`) and call
   `self.save_settings()` when it changes (settings save eagerly; `on_exit`
   is only a backstop).
4. Decide deliberately that it belongs here and not in prefs or the mapping
   preset (§1.2.4).

### 3.3 Add a new input source (new chord kind)

Example: a MIDI-input pedal, a touch-ring gesture, multi-pen distinction.

1. **Model** — `bindings.rs`: add an `InputChord` variant, extend `label()`
   (choose a readable, prefix-style string form like `"pedal:1"`) and
   `parse()` symmetrically. Serde comes for free (it round-trips through
   those two functions). Extend `chord_labels_round_trip_through_parse` and
   `invalid_chords_fail_to_parse`.
2. **Produce chords** — find where the source's events surface in
   `HudApp` and call `self.on_chord(chord)` on each *press edge* (releases
   are not dispatched; see `handle_tablet_button` and the pen-button edge
   detection in `handle_sample` for the two existing patterns).
3. Nothing else changes: listen-mode capture, resolution, persistence, and
   the settings window's chord chips all operate on the chord's string form.

Note the reach distinction documented in `docs/feature-map.md` §3: chords
produced from the capture stream work in the background; chords produced from
egui input require HUD focus. State which one your source is.

### 3.4 Add a new capture-pipeline event (new wire kind)

Follow the `TabletButton` (kind `0x06`) trail bottom-up; each step has an
exemplar to copy:

1. `tablet-core/src/backend.rs` — new `SampleEvent` variant.
2. `tablet-stream/src/message.rs` — new `StreamMessage` variant + the next
   free `KIND_*` byte (never reuse a retired one).
3. `tablet-stream/src/codec.rs` — encode/decode arms for Postcard **and**
   JSON (mirror `ProximityPayload`), plus round-trip tests for both formats.
4. `tablet-stream/src/framing.rs` — `kind_str` + `decode_jsonl` entries for
   the JSONL `"kind"` field.
5. `tablet-stream/src/mock.rs` — a `MockConfig` knob to synthesize the event,
   so the end-to-end path is testable without hardware (see
   `tablet_button_every`).
6. `tablet-cli/src/runtime.rs` — `stream_message_from_event` arm.
7. `tablet-consumer/src/source.rs` — `handle_message` arm (pass-through or
   state update).
8. Consumers (`tablet-hud`, `tablet-ui` `drain_source` matches) — handle or
   explicitly ignore. The matches are deliberately exhaustive (no `_`) so the
   compiler walks you to every consumer.
9. Document the kind in `docs/feature-map.md` §5. Compatibility rule: readers
   built before the kind existed fail with `StreamError::UnknownKind`;
   producer and consumers ship together in this repo, so rebuild both sides.

## 4. Persistence contract

- Path: `ProjectDirs::from("", "", "tablet-hud")` config dir,
  `settings.toml` (same directory as `tablet-hud.toml`).
- `HudSettings::load()` is infallible-by-contract: any failure ⇒ defaults.
- `HudSettings::save()` is best-effort (`io::Result` returned for logging);
  called on every settings mutation and again in `on_exit`.
- The file is hand-editable; chord strings are the same ones shown in the UI.
  Example:

```toml
[[keymap.bindings]]
input = "Escape"
action = "panic"

[[keymap.bindings]]
input = "pen:barrel"
action = "cycle_note_mode"

[[keymap.bindings]]
input = "tablet:0"
action = "octave_up"
```

## 5. Testing requirements

Every extension keeps these suites green and extends the relevant one:

- `actions::tests` — id uniqueness/round-trip iterates `Action::ALL`, so new
  variants are covered automatically.
- `bindings::tests` — chord string round-trips, invalid-chord rejection,
  keymap displace/clear semantics, unknown-id tolerance.
- `settings::tests` — TOML round-trip via temp dir, invalid-file fallback.
- `tablet-stream` codec/framing round-trips + `test_d2_*` end-to-end mock
  tests for wire-level additions.
- All of the above run on any OS without hardware (`cargo test`). Behavior
  that genuinely needs a device goes into `docs/bgc-hardware-checklist.md`
  as a manual checklist item instead.
