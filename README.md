# StylusInputCapture

Capture Wacom pen/stylus input at the **highest native resolution and report
rate** and stream the samples over a real-time IPC interface — **while another
application (e.g. a DAW) owns the foreground window**.

Background capture is the defining requirement, so the default Windows backend is
**Raw Input + HID** (`tablet-rawinput`), which receives a copy of the pen's HID
reports regardless of focus. See `SPEC_BGC.md` for the design.

## ⚠️ Windows requirement: enable "Use Windows Ink"

For the pen to present as a **standard HID digitizer** — which is what Raw Input
parses — **"Use Windows Ink" must be ON** in *Wacom Tablet Properties → Mapping*
(or the pen settings, depending on driver version). This is the **opposite** of
typical Wintab guidance.

With Windows Ink **off**, the Wacom driver biases toward the Wintab path and the
standard digitizer collection can go silent. If no HID digitizer is found at
startup, the CLI fails with a clear, non-panicking error
(`BackendError::NoDevice`) pointing you at this toggle — it does not stall
silently.

## Architecture (workspace crates)

| Crate             | Role |
|-------------------|------|
| `tablet-core`     | Platform-agnostic types + the `TabletBackend` trait. No OS deps. |
| `tablet-rawinput` | **Default Windows backend.** Raw Input (`RIDEV_INPUTSINK`) + HID (`HidP_*`) capture. |
| `tablet-wintab`   | Legacy Wintab backend — *deprecated for capture* (foreground-only); opt-in `backend-wintab`. |
| `tablet-stream`   | WCAP wire format, framing, transports (stdout / TCP / named pipe). |
| `tablet-cli`      | The capture binary (clap args, TOML config, lifecycle, tracing, metrics). |
| `tablet-consumer` | Shared stream-ingestion (reader thread, reconnect, `--spawn`) used by every consumer app. |
| `tablet-process` / `tablet-ui` | Calibration & visualization consumer (`SPEC_CUI.md`). |
| `tablet-midi`     | Pure MPE mapping library: pen → MIDI Polyphonic Expression events. No I/O. |
| `tablet-hud`      | MPE MIDI-controller HUD (`midir` output) — play the pen as an instrument. |

Data flow:

```
Wacom pen → HID digitizer → Raw Input (RIDEV_INPUTSINK)
          → [capture thread: WM_INPUT / GetRawInputBuffer → HidP_* decode → PenSample]
          → SPSC ring (drop-oldest) → [streaming thread: serialize + frame] → transport → consumer
```

## Build & run

```powershell
cargo build
cargo test                              # whole workspace

# Capture (Raw Input by default on Windows) and stream to stdout:
cargo run -p tablet-cli -- --transport stdout | my-consumer

# Stream over TCP as JSONL for debugging:
cargo run -p tablet-cli -- --transport tcp --format json
```

### Backend selection (Windows)

- **Default:** `backend-rawinput` (focus-independent; works while a DAW is
  foreground).
- **Opt-in fallback:** `backend-wintab` — only captures while the capturer is the
  foreground app. Build with
  `cargo run -p tablet-cli --no-default-features --features backend-wintab`.

On non-Windows targets the CLI falls back to the mock backend (no hardware).

## MPE MIDI HUD (`tablet-hud`)

Turn the pen into an expressive [MPE](https://www.midi.org/midi-articles/midi-polyphonic-expression-mpe)
instrument: horizontal position → **pitch** (snapped to a selectable scale/key),
vertical position → **CC74** (timbre), pen pressure → **channel pressure**, each
note on its own MPE member channel. The mapping logic lives in the pure
`tablet-midi` crate; `tablet-hud` is the egui app (top bar / mapping sidebar /
playing surface) that reads the stream and drives MIDI via
[`midir`](https://crates.io/crates/midir).

```powershell
# Spawn the capturer and play, picking a MIDI port (or "Virtual port") in the top bar:
cargo run -p tablet-hud -- --spawn

# Or connect to a capturer streaming over TCP:
cargo run -p tablet-cli -- --transport tcp        # producer
cargo run -p tablet-hud -- --tcp 127.0.0.1:9123   # HUD
```

- **Virtual ports** are created on macOS/Linux; on **Windows** install a
  loopback driver (e.g. [loopMIDI](https://www.tobias-erichsen.de/software/loopmidi.html))
  and connect to its port instead.
- On Linux the `midir` backend needs ALSA dev headers (`libasound2-dev`). Build
  without MIDI output (`--no-default-features`) to compile the HUD on systems
  that lack them.

## Fidelity notes (Raw Input backend)

Raw Input exposes no per-packet device serial or device timestamp, so under this
backend `PenSample.serial` is **host-synthesized** (monotonic, gap-only on ring
overflow), `t_device_ms` is **host-derived** (`0`), and `tool_serial` is `0`
unless a vendor usage provides one. Real loss is surfaced only via the ring
`dropped` metric. The reported packet rate is **measured** over the first second
of capture. See `SPEC_BGC.md` §5, §11.
