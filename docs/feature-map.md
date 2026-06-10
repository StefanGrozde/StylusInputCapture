# Feature Map — MIDI Controls, Actions & Keybindings

The complete inventory of what the MPE MIDI HUD (`tablet-hud`) can do, where
each control lives in the code, and how the settings/keybinding system binds
inputs to actions. This is the reference to update when adding a feature.

---

## 1. Continuous controls (pen → MIDI)

These run on every pen sample through `MpeEngine::process`
(`crates/tablet-midi/src/engine.rs`), configured by the fields of
`MidiMapping` (`crates/tablet-midi/src/mapping.rs`). They are *mappings*, not
discrete actions — they have sliders/checkboxes in the sidebar, and their
on/off switches are also bindable actions (§2).

| Pen input | MIDI output | Configured by (`MidiMapping`) | Notes |
| --- | --- | --- | --- |
| Pad position at pen-down | `NoteOn` (member channel, round-robin) | `scale`, `key`, `low_note`, `grid` (rows/cols/row interval) | Push-style pad grid; pads beyond MIDI range are dead |
| Pen lift | `NoteOff` | — | Note sustains while pressed |
| Slide onto another pad | latch / glide-bend / retrigger | `mode` (`NoteMode::Hold` / `Glide` / `Discrete`) | |
| Pressure at pen-down | `NoteOn` velocity | `velocity` (`Fixed` or `Pressure { min, max }`) | Velocity floor keeps attacks audible |
| Drag up/down from strike | `PitchBend` (14-bit) | `y_bend_semitones`, `mpe.pitch_bend_range_semitones` | Glide mode bends toward the pointed pad instead |
| Drag left/right from strike | `CC74` (timbre/brightness) | `x_to_cc74` | Strike point is the neutral center (64) |
| Pressure while held | `ChannelPressure` | `pressure_to_channel_pressure` | The MPE "press" dimension |
| Tilt (X or Y) | configurable `CC` | `tilt_cc` (`controller`, `axis`, `range_deg`) | Default: tilt X → CC1 (mod wheel) |

MPE session setup (zone MCM + per-member bend range RPN) is pushed by
`mpe_init_events` (`crates/tablet-hud/src/midi_out.rs`) on connect and on
live MPE edits.

## 2. Discrete actions (the bindable registry)

Defined in `crates/tablet-hud/src/actions.rs`; executed by
`HudApp::execute_action` (`crates/tablet-hud/src/app.rs`). UI buttons dispatch
through the same function as keybindings, so the two can't drift. **Ids are
stable** — they are what `settings.toml` stores; never rename one.

| Id | Label | Category | Default binding |
| --- | --- | --- | --- |
| `panic` | Panic (all notes off) | Performance | `Escape` |
| `test_note` | Test note | Performance | `T` |
| `octave_up` | Octave up | Performance | `PageUp` |
| `octave_down` | Octave down | Performance | `PageDown` |
| `scale_next` | Next scale | Mapping | — |
| `scale_prev` | Previous scale | Mapping | — |
| `key_next` | Key up (semitone) | Mapping | — |
| `key_prev` | Key down (semitone) | Mapping | — |
| `cycle_note_mode` | Cycle slide behavior | Mapping | — |
| `toggle_x_to_cc74` | Toggle drag → CC74 | Mapping | — |
| `toggle_pressure` | Toggle pressure → channel pressure | Mapping | — |
| `toggle_tilt_cc` | Toggle tilt → CC | Mapping | — |
| `midi_connect` | Connect MIDI port | MIDI & files | — |
| `midi_disconnect` | Disconnect MIDI port | MIDI & files | — |
| `refresh_ports` | Refresh MIDI ports | MIDI & files | — |
| `load_mapping` | Load mapping file | MIDI & files | — |
| `save_mapping` | Save mapping file | MIDI & files | — |
| `toggle_settings` | Show/hide settings | App | — |

Not in the registry (deliberately): the **Virtual port** button (platform
dependent), sliders/combos (continuous, not chord-shaped), and the pen tip
(reserved for playing).

### Adding a new bindable feature

1. Add a variant to `Action` and arms to `ALL`, `id`, `label`, `category`
   (`actions.rs`). Pick an id that will never change.
2. Implement it in `HudApp::execute_action` (`app.rs`).
3. If it has a UI button, make the button call `execute_action` too.

Old settings files keep working: unknown ids are kept on disk but never
resolve, and missing fields default.

## 3. Binding sources (input chords)

`InputChord` (`crates/tablet-hud/src/bindings.rs`), serialized as readable
strings:

| Source | String form | Reach | Path |
| --- | --- | --- | --- |
| Keyboard key (+ Ctrl/Shift/Alt) | `"Ctrl+Shift+T"`, `"PageUp"` | HUD focused only | egui key events in `HudApp::ui` |
| Stylus barrel button | `"pen:barrel"` | **Global** (background capture) | `PenSample.buttons` bit 1, edge-detected in `handle_sample` |
| Stylus eraser switch | `"pen:eraser"` | **Global** | `PenSample.buttons` bit 2 |
| Tablet ExpressKey | `"tablet:<index>"` | **Global** | `StreamMessage::TabletButton` (§5) |

Capture/binding flow: the Settings window's **Bind…** enters listen mode; the
next chord from any source is captured instead of dispatched (Escape cancels).
One chord maps to one action and one action has one chord; rebinding displaces
both sides and reports what moved.

While a text field has focus, only the bare `Escape` chord stays live (the
always-available panic), matching the pre-settings behavior.

### ExpressKeys caveat (Wacom driver)

With the full Wacom driver installed, the driver may consume pad reports
before Raw Input sees them; ExpressKeys then arrive as whatever keystrokes are
configured in Wacom Tablet Properties — which the **keyboard** binding path
catches while the HUD is focused. Native `tablet:<n>` capture works when raw
pad reports reach Raw Input (driverless setups, or driver pass-through). See
`docs/bgc-hardware-checklist.md` §6.

## 4. Persistence map

| File | Location | Owns | Code |
| --- | --- | --- | --- |
| `settings.toml` | OS config dir (`%APPDATA%\tablet-hud\config\` on Windows) | Keybindings + future app settings | `crates/tablet-hud/src/settings.rs` |
| `tablet-hud.toml` | same dir | Incidental prefs: window size, last port, last mapping path | `crates/tablet-hud/src/prefs.rs` |
| `*.midimap.toml` / `.json` | user-chosen path | Portable instrument preset (`MidiMapping`) | `crates/tablet-midi/src/mapping.rs` |
| Calibration profile | user-chosen path (`--profile`) | Signal processing (`CalibrationProfile`) | `crates/tablet-process` |

Settings load never fails (missing/invalid → defaults) and save on change plus
on exit.

## 5. Wire-protocol addendum: `TabletButton` (kind `0x06`)

Physical tablet buttons travel the capture pipeline like any other event:

```
pad HID report → tablet-rawinput (edge diff) → SampleEvent::TabletButton
  → tablet-cli → StreamMessage::TabletButton (kind 0x06 / "tablet_button")
  → tablet-consumer → tablet-hud keymap
```

- Payload: `index: u8` (stable per-device ordinal = position in the device's
  sorted button-usage list), `pressed: bool`.
- Capture: `tablet-rawinput` registers Consumer Control (`0x0C/0x01`) and the
  vendor-defined pages (`0xFF00`, `0xFF0D`) alongside digitizer/pen, and only
  builds pad profiles for devices sharing a USB VID with an enumerated
  digitizer — unrelated consumer devices (volume knobs etc.) are never
  decoded.
- Compatibility: consumers built before this kind existed fail a stream
  containing it with `StreamError::UnknownKind(0x06)`. Producer and consumers
  ship together in this repo; rebuild both sides.
