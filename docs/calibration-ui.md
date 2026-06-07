# Calibration & Visualization UI (`tablet-ui`)

`tablet-ui` is a native (egui/eframe) desktop tool that **visualizes** live
Wacom pen input and lets you **tune how that input is processed** —
coordinate mapping, pressure response, tilt, smoothing, activation — without
touching the capture pipeline (`SPEC_1.md`). It is a **read-only consumer** of
the existing `WCAP` stream: it connects to a running `tablet-cli` producer
exactly the way the reference consumer does (`docs/consumer.md`), decodes with
`tablet_stream::FrameReader`, and never reconfigures the live Wintab context.

See `SPEC_CUI.md` for the full design reference. This document covers running
the UI against each transport, the `*.cal.toml` profile format, how to apply a
profile from your own code via `tablet-process`, and the manual hardware
checklist.

---

## 1. Running against each source

`tablet-ui` supports the same three transports as `tablet-cli` plus an
optional one-process `--spawn` launcher. Pick whichever fits your workflow —
none of them touch capture.

### 1.1 Stdin pipe (simplest)

Pipe a producer directly into the UI (PowerShell):

```powershell
cargo run -p tablet-cli -- --transport stdout | cargo run -p tablet-ui
```

This is the default source (`Source::Stdin`, `--format postcard`). EOF on
stdin is **terminal** — see §3.

### 1.2 TCP

Start the producer in one terminal:

```powershell
cargo run -p tablet-cli -- --transport tcp --format json
```

Connect the UI from another terminal:

```powershell
cargo run -p tablet-ui -- --tcp 127.0.0.1:9123 --format json
```

TCP sources **reconnect automatically** with backoff if the producer restarts
or the connection drops — see §3.

### 1.3 Named pipe (Windows only)

Start the producer:

```powershell
cargo run -p tablet-cli -- --transport pipe --pipe-name wacom-capture
```

Connect the UI:

```powershell
cargo run -p tablet-ui -- --pipe wacom-capture
```

Like TCP, named-pipe sources **reconnect automatically** with backoff.

### 1.4 `--spawn` (one-process launch)

As a convenience, the UI can launch `tablet-cli` itself as a **child process**
and read its stdout — pure process orchestration; the capture code is
untouched (`SPEC_CUI.md` §5.2):

```powershell
cargo run -p tablet-ui -- --spawn
```

This resolves the `tablet-cli` binary next to the running `tablet-ui`
executable (the layout `cargo build`/`cargo run` produce for a workspace —
both binaries land in the same `target/<profile>/` directory), falling back to
a bare `tablet-cli`/`tablet-cli.exe` resolved via `PATH`. The child is started
with `--transport stdout --format postcard` (the UI always treats a spawned
producer as postcard, regardless of any `--format` you pass). The status bar
shows `source: spawned tablet-cli (child process, postcard via stdout)` while
this mode is active.

The child is **killed and reaped on UI exit** — no orphan `tablet-cli`
processes are left behind. EOF on the spawned child's stdout is terminal, just
like plain `Source::Stdin` (see §3).

### 1.5 CLI flags reference

```text
--tcp <addr>              Connect to a TCP stream (e.g. 127.0.0.1:9123)
--pipe <name>             Connect to a Windows named pipe (\\.\pipe\<name>)
--format <postcard|json>  Decode framed postcard binary or JSONL (default: postcard)
--profile <path>          Load a *.cal.toml / *.cal.json calibration profile at startup
--spawn                   Launch tablet-cli as a child process and read its stdout
```

`--tcp` and `--pipe` are mutually exclusive; the default source is stdin.

---

## 2. The `*.cal.toml` calibration profile format

A **calibration profile** (`CalibrationProfile`, `tablet-process`) is the
portable, serializable artifact this UI produces and downstream features
consume. It is an ordered, non-destructive processing pipeline: each stage
reads the raw `PenSample` (and any running per-stream state) and *derives*
fields — the raw sample is never mutated (`SPEC_CUI.md` §1.2, §4).

A profile contains:

| Field | Purpose |
| --- | --- |
| `name` | Human-readable label shown in the UI status bar. |
| `target_space` | Where raw digitizer coordinates are mapped: `Normalized` ([0,1]), `ScreenPixels { w, h }`, or `Millimeters` (uses axis `resolution` from `DeviceCapabilities`). |
| `coordinate_map` | Affine (and optional projective) transform + `enabled` flag; can be hand-set or fitted from an N-point calibration (§6.5/§4.3 in `SPEC_CUI.md`, the geometry workflow in the calibration panel). |
| `pressure` | Clamp `[min, max]` + curve kind (`Linear` / `Gamma(g)` / `Custom` monotone control points), producing pressure normalized to `[0, 1]`. |
| `tilt` | Exposed convention (`AzimuthAltitude` vs `TiltXY` degrees), units, and optional smoothing. |
| `smoothing` | Position-only filter: `Off` / `Ema { alpha }` / `OneEuro { .. }`; running state lives in `ProcessorState`, not the profile. |
| `activation` | On/off pressure thresholds with hysteresis (and optional proximity gating) deriving the `active` ("in contact") signal. |

It is serialized via `serde`:

- **`*.cal.toml`** — the default, human-readable format.
- **`*.cal.json`** — optional JSON, selected automatically by file extension.

Any other extension is treated as TOML. `CalibrationProfile::load` validates
the profile on read (e.g. `pressure_min <= pressure_max`, `gamma > 0`, `alpha`
in `(0, 1]`, monotone custom curve points) and rejects invalid files with a
typed `ProfileError`.

Manage profiles from the UI's **Calibration** panel (left side): New / Load /
Save / Save As, a dirty indicator, and the resolved path. `--profile <path>`
loads a profile at startup (falling back to the last-used path remembered in
`tablet-ui.toml` UI preferences — which are *never* mixed into a `.cal.toml`).

---

## 3. Connection status & reconnection

The top status bar always shows the active source, format (or "spawned
tablet-cli" when `--spawn` is active), and connection status
(`connecting` / `connected` / `disconnected`):

- **TCP and named-pipe sources reconnect automatically.** On EOF or any I/O
  error the status drops to `disconnected`, then the reader retries with
  exponential backoff (starting at 250 ms, doubling, capped at ~3 s) until it
  reconnects or the UI exits. A connection that is actually established resets
  the backoff, so a brief blip doesn't compound into a long wait. Status
  transitions (`connecting` → `connected` → `disconnected` → `connecting` …)
  are visible live in the status bar as the loop runs.
- **Stdin and `--spawn` are terminal.** EOF on stdin (the pipe closed, the
  producer exited) or on a spawned child's stdout means there is nothing left
  to read — the status moves to `disconnected` and **stays there**; the status
  bar shows a note that no automatic reconnect will happen for this source. To
  resume, restart the UI (or the piped producer) from the shell.

On exit, the UI signals its reader thread to stop (it checks a shared flag
between retries and respects the backoff sleep — it never spins) and, if a
producer was spawned via `--spawn`, kills and waits on the child process so no
orphan `tablet-cli` is left running.

---

## 4. Downstream usage: applying a profile in your own code

`tablet-process` is a pure, OS-agnostic library — the same crate the UI uses
to preview processing live. Load a profile the UI produced and apply it to
samples from your own `tablet-stream`/`tablet-core` consumer to get **exactly**
the behavior you tuned in the UI:

```rust
use std::path::Path;

use tablet_process::{CalibrationProfile, ProcessorState};

// Load once at startup (mirrors the UI's `--profile <path>`).
let profile = CalibrationProfile::load(Path::new("my-rig.cal.toml"))?;

// Per-stream mutable state for stateful stages (smoothing filters,
// activation hysteresis latch). Create one per stream; thread it through
// every `apply` call for that stream.
let mut state = ProcessorState::new();

// `caps` is the `DeviceCapabilities` from the stream's `Capabilities` frame
// (handshake); `pen_sample` is a raw `PenSample` decoded from the stream.
let processed = profile.apply(&pen_sample, &caps, &mut state);

// `processed.raw` is the untouched input; `processed.x`/`y`/`pressure`/
// `tilt_x`/`tilt_y`/`twist`/`active`/`out_of_range` are the derived values —
// identical to what the calibration UI showed you while tuning.
# Ok::<(), tablet_process::ProfileError>(())
```

`apply` is pure and allocation-light (no I/O, no clock reads), so it is safe
to call on a hot path. `CalibrationProfile::identity()` gives you a
pass-through profile (`processed == raw` for the mapped fields) if you want a
neutral starting point instead of loading a file.

---

## 5. Manual hardware checklist (`SPEC_CUI.md` §9)

Run the UI against a live producer with real Wacom hardware connected
(e.g. `cargo run -p tablet-cli -- --transport stdout | cargo run -p tablet-ui`,
or `cargo run -p tablet-ui -- --spawn`) and confirm:

- [ ] **Trace tracks the pen.** Moving the stylus over the tablet draws a path
      in the XY trace canvas that follows the physical motion (position,
      direction, and speed all look right; newest segment brightest, fading
      with age).
- [ ] **Pressure width responds.** Pressing harder visibly thickens the
      stroke (processed pressure drives stroke width); lifting off (hover)
      switches to the thin/dashed "not active" rendering.
- [ ] **Tilt vector points correctly.** Tilting the pen changes the short
      vector drawn from the cursor: its direction matches the azimuth you're
      tilting toward, and its length grows with altitude (more upright = a
      certain length convention — confirm it matches the orientation panel's
      numeric azimuth/altitude readout).
- [ ] **Eraser/airbrush detected.** Switching tools (flipping the pen to the
      eraser end, or using an airbrush stylus if available) is reflected in
      the tool/proximity state and logged as a tool-change event in the
      telemetry event log.
- [ ] **Proximity fires.** Bringing the pen into and out of range of the
      tablet produces `proximity in` / `proximity out` events in the
      telemetry event log promptly.
- [ ] **Fitted geometry calibration lands a test grid within tolerance.**
      Using the calibration panel's N-point geometry workflow, collect ≥4
      correspondence points across a test grid, run "Fit", confirm the
      reported per-point residuals/RMS are small, and verify the live
      processed trace now aligns raw input to the intended target-space
      positions (e.g. tracing the physical grid produces a visually aligned
      grid in the target space).
