# TASKS_CUI.md — Calibration & Visualization UI

Implementation task breakdown derived from `SPEC_CUI.md` (the source of truth for
this feature). Each task is dispatched to an agent with **only `SPEC_CUI.md` and
the task text**. The agent may read whatever files already exist in the repo
(the existing capture workspace plus the output of its completed dependency
tasks), but receives no other task descriptions.

Section references like "§4.2" point into `SPEC_CUI.md`. References to
`SPEC_1.md` point into the existing capture spec.

**Hard boundary (SPEC_CUI.md §1.2 / §12):** these tasks add two **new** crates
(`tablet-process`, `tablet-ui`) and must **not** modify `tablet-core`,
`tablet-wintab`, `tablet-stream`, or `tablet-cli`. They depend only on those
crates' existing public APIs. Raw `PenSample` data is never mutated — processing
only *derives* new fields.

Build order (note: the processing library is built **first** as the foundation,
which differs from the visualize-first labelling in `SPEC_CUI.md` §11):
**Sprint C1** (`tablet-process`) → **Sprint C2** (`tablet-ui` scaffold,
ingestion, visualization) → **Sprint C3** (calibration workflows, profiles,
polish). Within C2/C3, tasks depend on C1 being `DONE`.

Dependency rule: a task is only eligible to run once every task in its
**Depends on** list is `DONE`. Update statuses as work completes.

Useful existing APIs the agents will reuse (do not re-implement):
- `tablet_core`: `PenSample`, `DeviceCapabilities`, `AxisInfo`, `AxisUnit`,
  `ToolKind`, `normalize`, `tilt_from_orientation`.
- `tablet_stream`: `FrameReader` (`new_from_header`, `new_json`, `read_message`
  returning `Result<StreamMessage, StreamError>`; EOF surfaces as
  `StreamError::TruncatedFrame`), `Format`, `StreamMessage`, `Metrics`. See the
  reference consumer at `crates/tablet-stream/examples/consumer.rs` for the read
  pattern.

---

# Sprint C1 — Processing & Calibration Library (`tablet-process`)
Branch: `cui-1-process`
Status: DONE

A pure, OS-agnostic library (`SPEC_CUI.md` §3.2, §4). No UI, no I/O on the hot
path, fully unit-testable without hardware or a display. This is the foundation
the UI and the user's future features both consume.

## Tasks

### TC1.1 — Scaffold `tablet-process` crate + core data model
Status: DONE
Depends on: none

**Context:**
Create the new library crate per §3.2. It depends only on `tablet-core` and
`serde` (plus `thiserror`); **no OS, UI, or stream deps** (§1.4, §2.2). Define
the data model from §4.1 and §4.4 as plain serializable values: `TargetSpace`
(`Normalized` | `ScreenPixels { w, h }` | `Millimeters`), `ProcessedSample`
(§4.1, holds the untouched `raw: PenSample` plus derived fields),
`CalibrationProfile` (§4.4) and its stage sub-structs (`CoordinateMap`,
`PressureCurve`, `TiltConfig`, `SmoothingConfig`, `ActivationConfig`), and a
`ProcessorState` that will carry per-stream filter/hysteresis state (§4.5). This
task establishes the types and an **identity** profile whose `apply` is a faithful
pass-through (mapped position = raw normalized, pressure = raw normalized, raw
retained); later tasks fill in each stage.

**Files:**
- MODIFY: root `Cargo.toml` (add `crates/tablet-process` to `members`)
- CREATE: `crates/tablet-process/Cargo.toml` (deps: `tablet-core` path,
  `serde` with `derive`, `thiserror`)
- CREATE: `crates/tablet-process/src/profile.rs` (`CalibrationProfile`, stage
  sub-structs, `TargetSpace`, `identity()`)
- CREATE: `crates/tablet-process/src/sample.rs` (`ProcessedSample`)
- CREATE: `crates/tablet-process/src/state.rs` (`ProcessorState`)
- CREATE: `crates/tablet-process/src/lib.rs` (declare + re-export; `apply` skeleton)

**Steps:**
1. Add the crate to the workspace and add deps via `cargo add` (latest stable).
2. Define `TargetSpace`, `ProcessedSample` (exactly the fields in §4.1), and the
   `CalibrationProfile` + stage sub-structs (§4.4). Derive `Clone, Debug,
   PartialEq, Serialize, Deserialize`; each stage has an `enabled: bool`.
3. Implement `CalibrationProfile::identity()` (and `Default`) producing a
   pass-through profile (`TargetSpace::Normalized`, all stages disabled or
   identity-valued).
4. Define `ProcessorState` (empty for now; smoothing/activation state added in
   TC1.4) with `ProcessorState::new()`.
5. Implement a minimal `apply(&self, raw, caps, state) -> ProcessedSample`
   skeleton: for the identity profile, set `x/y` from raw normalized position,
   `pressure` from raw normalized pressure, copy tilt/twist through, `active =
   in_proximity`, `out_of_range = false`, and `raw` = the input.

**Acceptance Criteria:**
- [ ] `cargo build -p tablet-process` succeeds with no OS-specific deps.
- [ ] `CalibrationProfile::identity().apply(...)` returns a `ProcessedSample`
      whose `raw` equals the input and whose mapped `x/y/pressure` equal the raw
      normalized values.
- [ ] All public types derive `Serialize`/`Deserialize`.
- [ ] The crate is added to the workspace `members` and builds from the root.

---

### TC1.2 — Coordinate mapping stage + N-point geometry fit
Status: DONE
Depends on: TC1.1

**Context:**
Implement the coordinate-mapping stage (§4.2 #1) and the N-point fit helper
(§4.3). `CoordinateMap` carries an affine (2×3) and optional projective (3×3)
transform plus `enabled`. When enabled, map raw digitizer units → the profile's
`TargetSpace`: `Normalized` uses the device extent from `DeviceCapabilities`;
`Millimeters` uses axis `resolution`/`unit` (§5.2 of SPEC_1.md); `ScreenPixels`
scales to `{w,h}`. `fit_geometry` computes the transform from collected
`(raw_xy, target_xy)` pairs via least squares: similarity (≥2 points), affine
(≥4), or projective homography (≥4), and returns a `FitReport` with per-point
residuals and RMS error. This is pure math, unit-testable with synthetic points.

**Files:**
- CREATE: `crates/tablet-process/src/stages/coordinate.rs` (`CoordinateMap`
  apply + `TargetSpace` conversion)
- CREATE: `crates/tablet-process/src/fit.rs` (`fit_geometry`, `FitReport`)
- MODIFY: `crates/tablet-process/src/lib.rs` (wire stage into `apply`; re-export)

**Steps:**
1. Implement `CoordinateMap::apply(raw_x, raw_y, caps, target_space) -> (f64,
   f64)` applying the transform then converting into the target space.
2. Implement `fit_geometry(points) -> FitReport` (degree chosen by point count);
   write the resulting matrix back into a `CoordinateMap`. Return residuals +
   RMS.
3. Wire the stage into `CalibrationProfile::apply` (skip when `!enabled`).
4. Add `#[cfg(test)]` tests: identity transform is a no-op; a known affine
   recovered from ≥4 synthetic points yields residuals ≈ 0 (within `1e-6`);
   target-space conversions (normalized/mm/pixels) hit expected endpoints.

**Acceptance Criteria:**
- [ ] Disabled stage passes raw normalized position through unchanged.
- [ ] `fit_geometry` recovers a known affine/projective transform (residuals ≈ 0).
- [ ] `FitReport` reports per-point residuals and an RMS value.
- [ ] Target-space conversion is correct for Normalized, Millimeters, ScreenPixels.

---

### TC1.3 — Pressure response stage
Status: DONE
Depends on: TC1.1

**Context:**
Implement the pressure-response stage (§4.2 #2). `PressureCurve` clamps raw
pressure to an observed `[min, max]`, then remaps through a curve: `Linear`,
`Gamma(g)`, or `Custom(points)` (monotone control points), producing a
normalized `[0.0, 1.0]` output written to `ProcessedSample.pressure`. Also
provide a helper to **learn min/max** from a slice of recent samples (used by the
UI's "learn min/max" button, §6.2). Pure and unit-testable.

**Files:**
- CREATE: `crates/tablet-process/src/stages/pressure.rs` (`PressureCurve` apply,
  curve kinds, `learn_min_max`)
- MODIFY: `crates/tablet-process/src/lib.rs` (wire stage into `apply`; re-export)

**Steps:**
1. Implement `PressureCurve::apply(raw_pressure) -> f64` (clamp → normalize →
   curve). Implement `Linear`, `Gamma`, and `Custom` (piecewise-linear over
   sorted control points) evaluation.
2. Implement `learn_min_max(samples: &[PenSample]) -> (u32, u32)` returning the
   observed raw pressure range.
3. Wire into `CalibrationProfile::apply` (disabled → use raw normalized pressure).
4. Add `#[cfg(test)]` tests: clamp at min/max edges; `Gamma` monotonic and maps
   0→0, max→1; `Custom` interpolation hits control points; output always in
   `[0,1]`.

**Acceptance Criteria:**
- [ ] Output pressure is always within `[0.0, 1.0]`.
- [ ] Clamp edges and all three curve kinds behave per tests.
- [ ] `learn_min_max` returns the true observed range for a synthetic set.
- [ ] Disabled stage yields raw normalized pressure.

---

### TC1.4 — Tilt, smoothing & activation stages + `ProcessorState`
Status: DONE
Depends on: TC1.1

**Context:**
Implement the three stateful/derivation stages (§4.2 #3–#5). `TiltConfig` chooses
the exposed convention (`AzimuthAltitude` vs `TiltXY` degrees — both already
available on `PenSample`), units, and optional low-pass smoothing.
`SmoothingConfig` (`Off` | `Ema { alpha }` | `OneEuro { ... }`) filters
**position only**, producing `x_filtered`/`y_filtered`; its running state lives in
`ProcessorState` (§4.5), so `CalibrationProfile` stays plain data.
`ActivationConfig` derives `active` from pressure on/off thresholds with
hysteresis and/or proximity gating; its latch state also lives in
`ProcessorState`. Filters are deterministic given the same inputs.

**Files:**
- CREATE: `crates/tablet-process/src/stages/tilt.rs` (`TiltConfig` apply)
- CREATE: `crates/tablet-process/src/stages/smoothing.rs` (`SmoothingConfig` +
  EMA/OneEuro filter, state in `ProcessorState`)
- CREATE: `crates/tablet-process/src/stages/activation.rs` (`ActivationConfig` +
  hysteresis, state in `ProcessorState`)
- MODIFY: `crates/tablet-process/src/state.rs` (add filter + activation state)
- MODIFY: `crates/tablet-process/src/lib.rs` (wire stages into `apply`)

**Steps:**
1. Implement `TiltConfig::apply` selecting/deriving `tilt_x/tilt_y/twist` in the
   chosen convention/units (reuse `tablet_core::tilt_from_orientation` where
   relevant).
2. Implement EMA and One-Euro position filters holding previous values in
   `ProcessorState`; output `x_filtered`/`y_filtered` (None when `Off`).
3. Implement activation: on/off pressure thresholds with hysteresis latch (state
   in `ProcessorState`), optionally requiring `in_proximity`.
4. Wire all three into `CalibrationProfile::apply` (in pipeline order: tilt,
   smoothing, activation).
5. Add `#[cfg(test)]` tests: EMA matches a hand-computed sequence (determinism);
   hysteresis does not chatter across the threshold band; activation respects
   proximity gating.

**Acceptance Criteria:**
- [ ] EMA/OneEuro are deterministic for a given input sequence and seed state.
- [ ] Activation uses hysteresis (separate on/off thresholds), no chatter.
- [ ] Tilt convention/units selection produces the expected derived values.
- [ ] Smoothing/activation state is held in `ProcessorState`, not the profile.

---

### TC1.5 — Profile (de)serialization, validation & full `apply` pipeline
Status: DONE
Depends on: TC1.2, TC1.3, TC1.4

**Context:**
Finalize the public API (§4.4, §4.5). Implement `CalibrationProfile::load`/`save`
to **`*.cal.toml`** (default) and JSON, with a `ProfileError` (`thiserror`) and
**range validation on load** (§8). Ensure `apply` runs all stages in the
documented order (§4.2: coordinate → pressure → tilt → smoothing → activation)
and that `out_of_range` is set when any axis falls outside its
`DeviceCapabilities` range. Add end-to-end tests across the whole pipeline.

**Files:**
- CREATE: `crates/tablet-process/src/io.rs` (`load`/`save`, `ProfileError`,
  validation)
- CREATE: `crates/tablet-process/tests/pipeline.rs` (integration tests)
- MODIFY: `crates/tablet-process/Cargo.toml` (add `toml`, `serde_json`)
- MODIFY: `crates/tablet-process/src/lib.rs` (final `apply` ordering; re-export)

**Steps:**
1. Implement `save(path)` (format chosen by extension or an arg) and
   `load(path)` with `ProfileError` for read/parse/validation failures.
2. Validate on load: ranges (e.g. pressure min ≤ max, gamma > 0, alpha in
   `(0,1]`, monotone custom points); reject with actionable errors.
3. Confirm `apply` executes stages in §4.2 order and sets `out_of_range`.
4. Integration tests: identity profile ⇒ `processed.raw == input` and mapped
   values equal raw normalized; a non-trivial profile round-trips through
   `save`→`load` (TOML and JSON) preserving equality; a synthetic sample stream
   produces stable, expected processed output.

**Acceptance Criteria:**
- [ ] `save`→`load` round-trips a profile for both TOML and JSON (equality holds).
- [ ] Invalid profiles are rejected on load with typed `ProfileError`s.
- [ ] `apply` runs stages in the §4.2 order and sets `out_of_range` correctly.
- [ ] `cargo test -p tablet-process` passes on any OS with no hardware.

---

# Sprint C2 — UI Scaffold, Ingestion & Visualization (`tablet-ui`)
Branch: `cui-2-ui`
Status: DONE

A native **egui/eframe** application (§3.3) that consumes the existing stream
read-only (§5) and visualizes raw + processed data. GUI tasks build everywhere;
where a task's behavior is visual, factor non-visual logic into testable helpers.

## Tasks

### TC2.1 — Scaffold `tablet-ui` eframe binary + CLI + app skeleton
Status: DONE
Depends on: TC1.1

**Context:**
Create the new binary crate per §3.2/§3.3 using `eframe`/`egui` (+ `egui_plot`),
depending on `tablet-core`, `tablet-stream`, and `tablet-process`. Define the CLI
(§7): default reads stdin; `--tcp <addr>` and `--pipe <name>` select a transport;
`--format <postcard|json>`; `--profile <path>`; `--spawn` (parsed here, behavior
in TC3.5). Build the `eframe::App` skeleton holding app state (active
`CalibrationProfile` = identity, `ProcessorState`, placeholder panels) and a
window that runs. No stream ingestion yet (TC2.2).

**Files:**
- MODIFY: root `Cargo.toml` (add `crates/tablet-ui` to `members`)
- CREATE: `crates/tablet-ui/Cargo.toml` (deps: `eframe`, `egui`, `egui_plot`,
  `tablet-core`, `tablet-stream`, `tablet-process`)
- CREATE: `crates/tablet-ui/src/cli.rs` (arg parsing; reuse `clap` or hand-rolled
  like the reference consumer)
- CREATE: `crates/tablet-ui/src/app.rs` (`eframe::App` state + empty panels)
- CREATE: `crates/tablet-ui/src/main.rs` (parse args → launch eframe)

**Steps:**
1. Add the crate to the workspace and add deps via `cargo add`.
2. Implement CLI parsing for the flags above (default source = stdin, default
   format = postcard).
3. Define the app state struct (selected source/format, active profile =
   `CalibrationProfile::identity()`, `ProcessorState`, panel placeholders).
4. Implement `eframe::App::update` drawing labelled empty panels (trace,
   pressure, orientation, telemetry, calibration, inspector) so the layout is
   visible.
5. `main` launches the native window via `eframe::run_native`.

**Acceptance Criteria:**
- [ ] `cargo build -p tablet-ui` succeeds; the crate is in workspace `members`.
- [ ] Running it opens a window with the six labelled placeholder panels (on a
      machine with a display).
- [ ] CLI flags parse, including `--tcp`/`--pipe`/`--format`/`--profile`/`--spawn`.

---

### TC2.2 — Stream ingestion: reader thread + shared display buffer
Status: DONE
Depends on: TC2.1

**Context:**
Implement the connected-consumer ingestion (§3.4, §5). A **reader thread** opens
the selected transport — stdin (default), `TcpStream::connect(addr)` for `--tcp`,
or a Windows named-pipe client (`\\.\pipe\<name>`) for `--pipe` — builds a
`FrameReader` (`new_from_header` for postcard, `new_json` for JSON), and loops
`read_message()`, pushing decoded `StreamMessage`s into a **bounded** shared
buffer (`Mutex<VecDeque>` or `rtrb`) with a cap; overflow **drops oldest** display
samples and increments `display_dropped` (§2.2, §3.4). The thread tracks
connection state (`Connecting`/`Connected`/`Disconnected`), the latest
`DeviceCapabilities`, the latest `Metrics`, and a UI-side serial-gap counter
(mirror the producer's gap logic). The UI thread drains the buffer each frame.
Never block the producer. This task is logic, not visuals — keep it testable.

**Files:**
- CREATE: `crates/tablet-ui/src/source.rs` (reader thread, transport open,
  shared buffer, connection state, counters)
- MODIFY: `crates/tablet-ui/src/app.rs` (own the source; drain per frame; store
  caps/metrics/state)
- CREATE: `crates/tablet-ui/tests/ingest.rs` (headless ingestion tests)

**Steps:**
1. Define a bounded shared buffer + `SourceState` (connection status, latest
   caps, latest metrics, `display_dropped`, `serial_gaps`).
2. Spawn the reader thread: open the transport for the configured source, build
   the `FrameReader` by format, loop `read_message()`. On `TruncatedFrame`/error,
   set `Disconnected` and exit (reconnect added in TC3.5).
3. On each message: update caps/metrics; for `Sample`, detect serial gaps and
   push to the buffer (drop-oldest on overflow, bump `display_dropped`).
4. In `app.update`, drain everything queued each frame into the app (history
   handled by later panels).
5. Tests (headless): feed a known framed byte stream (produced via
   `tablet_stream` writer or by piping `MockBackend`) into the reader path and
   assert capabilities captured, sample count, gap counting, and overflow
   accounting.

**Acceptance Criteria:**
- [ ] Reader decodes postcard and JSON streams via `FrameReader` and never blocks
      the producer.
- [ ] Overflow drops oldest display samples and increments `display_dropped`.
- [ ] Connection state transitions to `Disconnected` on EOF/error.
- [ ] Headless ingestion tests pass (caps, counts, gaps, overflow).

---

### TC2.3 — XY trace canvas panel
Status: DONE
Depends on: TC2.2

**Context:**
Implement the primary visualization (§6.1). Maintain a **fixed-length** history
ring of recent `ProcessedSample`s (apply the app's active `CalibrationProfile`
via `tablet-process` to each drained sample). Draw the pen path with an
`egui::Painter` in the profile's target space (fit to the panel): newest segment
brightest with fade, **stroke width ∝ processed pressure**, hover (not `active`)
drawn thin/dashed, a short tilt vector (azimuth direction, length ∝ altitude),
and a small twist/rotation dial. A **raw-vs-processed overlay** toggle shows the
raw mapped path under the processed path. Show a crosshair readout of the latest
position in raw units and target units.

**Files:**
- CREATE: `crates/tablet-ui/src/panels/trace.rs` (history ring + painter)
- MODIFY: `crates/tablet-ui/src/app.rs` (apply profile per sample; hold history;
  mount panel)

**Steps:**
1. Add a fixed-capacity history ring; on each drained `Sample`, compute a
   `ProcessedSample` (and keep the raw-mapped point for the overlay) and append,
   evicting oldest.
2. Draw the path with per-segment alpha fade and width from processed pressure;
   render hover segments thin/dashed using `active`.
3. Draw the tilt vector and twist dial at the latest point; draw the crosshair +
   numeric readout (raw + target units).
4. Add the raw-vs-processed overlay toggle.
5. Factor history/decimation/scaling into pure helpers and unit-test them
   (ring eviction, world→panel scaling) without a display.

**Acceptance Criteria:**
- [ ] The trace renders and tracks incoming samples; width responds to pressure.
- [ ] Hover vs contact segments are visually distinct (uses `active`).
- [ ] The raw-vs-processed overlay toggles on/off.
- [ ] History is bounded (fixed memory); ring/scaling helpers have unit tests.

---

### TC2.4 — Pressure & orientation panels
Status: DONE
Depends on: TC2.2

**Context:**
Implement the pressure and orientation visualizations (§6.2, §6.3) using
`egui_plot`. Pressure panel: a time strip overlaying raw and shaped pressure, and
a histogram of recent pressure revealing observed min/max and dead zones.
Orientation panel: azimuth compass + altitude gauge, twist and rotation dials,
numeric raw (deci-degrees) vs processed (chosen units) side by side, and range
bars from `DeviceCapabilities` with out-of-range flagged. (The interactive curve
*editor* and "learn min/max" are TC3.2; this task renders the curves/values.)

**Files:**
- CREATE: `crates/tablet-ui/src/panels/pressure.rs` (time strip + histogram)
- CREATE: `crates/tablet-ui/src/panels/orientation.rs` (compass/gauges/dials)
- MODIFY: `crates/tablet-ui/src/app.rs` (mount panels; feed history)

**Steps:**
1. Pressure time strip: plot raw vs shaped pressure over the history window with
   `egui_plot`.
2. Pressure histogram: bin recent pressure into a fixed number of buckets and
   render; factor the binning into a pure, tested helper.
3. Orientation: draw azimuth compass + altitude gauge + twist/rotation dials;
   show raw vs processed numbers and range bars from caps; flag out-of-range.
4. Use the device axis ranges from the latest `Capabilities` for scaling/labels.

**Acceptance Criteria:**
- [ ] Pressure strip shows raw and shaped curves; histogram reflects recent data.
- [ ] Orientation panel shows azimuth/altitude/twist/rotation with raw vs
      processed values and range bars.
- [ ] Out-of-range axis values are flagged.
- [ ] The histogram binning helper has unit tests.

---

### TC2.5 — Telemetry/troubleshooting panel + sample inspector
Status: DONE
Depends on: TC2.2

**Context:**
Implement the diagnostics surface (§6.4, §6.6). Telemetry panel shows, from
`Metrics` frames: packets/s, `dropped`, `queue_depth`, `actual_rate_hz` vs
`requested_rate_hz`, connected clients. Local diagnostics: UI-side serial-gap
count and `display_dropped` (from TC2.2), an inter-sample interval histogram
(jitter) computed from `t_capture_ns`, and a coarse latency proxy. A scrolling
event log records proximity in/out and tool changes (Pen/Eraser/Airbrush). The
sample inspector shows the latest raw `PenSample` fields next to the derived
`ProcessedSample` fields for exact verification.

**Files:**
- CREATE: `crates/tablet-ui/src/panels/telemetry.rs` (metrics + diagnostics +
  event log)
- CREATE: `crates/tablet-ui/src/panels/inspector.rs` (raw vs processed fields)
- MODIFY: `crates/tablet-ui/src/app.rs` (track events; mount panels)

**Steps:**
1. Render the latest `Metrics` values and the local counters from `SourceState`.
2. Compute and draw an inter-sample interval (jitter) histogram from consecutive
   `t_capture_ns` deltas; factor the delta/binning math into tested helpers.
3. Maintain a bounded event log; append on `Proximity` messages and on tool
   changes detected between consecutive samples.
4. Inspector: display every `PenSample` field (§5.1 of SPEC_1.md) beside the
   matching `ProcessedSample` field for the latest sample.

**Acceptance Criteria:**
- [ ] Telemetry shows packets/s, dropped, queue depth, actual vs requested rate,
      connected clients from `Metrics` frames.
- [ ] Serial-gap count, `display_dropped`, and a jitter histogram are shown.
- [ ] The event log records proximity and tool-change events.
- [ ] The inspector shows latest raw vs processed fields; jitter helpers tested.

---

# Sprint C3 — Calibration Workflows, Profiles & Polish
Branch: `cui-3-calibration`
Status: DONE

Make the processing tunable from the UI, add the calibration workflows, profile
persistence, and connection polish (§4, §5, §6.5, §7).

## Tasks

### TC3.1 — Calibration/settings panel: live profile editing
Status: DONE
Depends on: TC1.5, TC2.3, TC2.4

**Context:**
Implement the settings panel (§6.5) that edits every `CalibrationProfile` stage
live. Provide controls for: `TargetSpace`; coordinate-map enable; pressure
min/max + curve kind + gamma; tilt convention/units + smoothing toggle;
`SmoothingConfig` mode + params; `ActivationConfig` thresholds + hysteresis. Edits
mutate the app's active profile in place; because the render loop already applies
the profile per sample (TC2.3/2.4), changes take effect on the next frame. Add a
**reset-to-identity** button and a **dirty** indicator (profile differs from the
last loaded/saved state).

**Files:**
- CREATE: `crates/tablet-ui/src/panels/calibration.rs` (stage controls)
- MODIFY: `crates/tablet-ui/src/app.rs` (own profile + dirty flag; mount panel)

**Steps:**
1. Build grouped, toggleable controls for each stage's parameters (§4.2).
2. Bind each control to the corresponding `CalibrationProfile` field so edits
   apply immediately.
3. Add reset-to-identity and a dirty indicator (compare against the
   last-saved/loaded snapshot).
4. Guard parameter ranges in the UI to match `tablet-process` validation (TC1.5).

**Acceptance Criteria:**
- [ ] Changing any control updates the processed view (trace/pressure/orientation)
      on the next frame.
- [ ] Reset-to-identity restores a pass-through profile.
- [ ] The dirty indicator reflects unsaved edits.

---

### TC3.2 — Pressure response curve editor + learn min/max
Status: DONE
Depends on: TC3.1

**Context:**
Upgrade the pressure panel (§6.2) with an **interactive** response-curve editor:
a gamma slider and draggable `Custom` control points, with the live input
pressure shown as a moving dot on the curve so tuning is immediate. Add a
"**learn min/max from recent samples**" button that calls
`tablet_process`'s `learn_min_max` helper (TC1.3) and writes the result into the
profile's pressure clamp.

**Files:**
- MODIFY: `crates/tablet-ui/src/panels/pressure.rs` (interactive editor +
  learn button)
- MODIFY: `crates/tablet-ui/src/app.rs` (pass recent-sample buffer to the helper)

**Steps:**
1. Add a gamma slider and draggable control points for `Custom`; keep points
   sorted/monotone so the curve stays valid.
2. Overlay the live input-pressure dot on the curve each frame.
3. Wire the "learn min/max" button to `learn_min_max` over the recent-sample
   history and update the clamp.
4. Ensure edits flow through the same live-apply path as TC3.1.

**Acceptance Criteria:**
- [ ] Dragging control points / moving the gamma slider changes the shaped output
      live.
- [ ] The live input-pressure dot tracks the current sample on the curve.
- [ ] "Learn min/max" sets the clamp from the observed range.

---

### TC3.3 — N-point geometry calibration workflow
Status: DONE
Depends on: TC3.1

**Context:**
Implement the geometry-calibration workflow (§4.3, §6.5). The user collects
calibration points: "**Add point**" captures the current raw pen position and
lets the user set the intended target coordinate (click a target location in the
canvas and/or enter it numerically). Collected points are listed and
removable/redoable. "**Fit**" calls `CalibrationProfile::fit_geometry` (TC1.2),
writes the resulting matrix into the coordinate-map stage, and displays per-point
residuals and RMS error so the user can judge quality and re-collect bad points.

**Files:**
- CREATE: `crates/tablet-ui/src/calibration/geometry.rs` (point collection + fit
  workflow state)
- MODIFY: `crates/tablet-ui/src/panels/calibration.rs` (workflow UI)
- MODIFY: `crates/tablet-ui/src/app.rs` (hold collected points)

**Steps:**
1. Implement point collection: capture current raw `(x,y)` on "Add point"; let
   the user place/enter the matching target coordinate.
2. Render the collected-point list with remove/redo controls.
3. On "Fit", call `fit_geometry`, apply the matrix to the profile's coordinate
   map, and show residuals + RMS from the `FitReport`.
4. After fitting, the live trace should align raw input to the target space
   (visual confirmation).

**Acceptance Criteria:**
- [ ] Collecting ≥4 points and fitting updates the coordinate-map transform.
- [ ] Per-point residuals and RMS are displayed.
- [ ] Points can be removed/redone and the fit recomputed.
- [ ] After a good fit, the processed trace aligns to the target space.

---

### TC3.4 — Profile management & UI preferences
Status: DONE
Depends on: TC1.5, TC3.1

**Context:**
Implement profile persistence and app preferences (§6.5, §7). Profile management:
New / Load / Save / Save As for **`*.cal.toml`** via `tablet_process` `load`/
`save`, with a dirty indicator, confirm-on-discard, and a visible resolved path;
`--profile <path>` loads at startup. Separately, persist **UI preferences**
(window size, last transport, last profile path, panel layout) to a small
`tablet-ui.toml` in the OS config dir — **never** mixed into a calibration profile
(§7).

**Files:**
- CREATE: `crates/tablet-ui/src/prefs.rs` (`tablet-ui.toml` load/save in config dir)
- MODIFY: `crates/tablet-ui/src/panels/calibration.rs` (New/Load/Save/Save As UI)
- MODIFY: `crates/tablet-ui/src/app.rs` (load `--profile` at startup; apply prefs)
- MODIFY: `crates/tablet-ui/Cargo.toml` (add `directories`/`dirs` + `toml` as needed)

**Steps:**
1. Wire New/Load/Save/Save As to `tablet_process::CalibrationProfile` I/O; track
   the loaded path and update the dirty snapshot on save/load.
2. Add confirm-on-discard when loading/new with unsaved edits.
3. Load `--profile` at startup if provided; otherwise start from identity.
4. Implement `prefs.rs`: load on start, save on exit; apply window size/last
   source/last profile/layout. Keep prefs strictly separate from profiles.

**Acceptance Criteria:**
- [ ] A profile round-trips through Save then Load (values preserved).
- [ ] `--profile` loads a profile at startup.
- [ ] Unsaved edits prompt confirm-on-discard.
- [ ] UI preferences persist across runs and never appear in a `.cal.toml`.

---

### TC3.5 — Producer spawn, reconnection & docs
Status: DONE
Depends on: TC2.2, TC3.4

**Context:**
Final connection polish and documentation (§5.2, §5.3, §9). Implement the optional
`--spawn` convenience: launch `tablet-cli` as a **child process** with passthrough
capture flags and read its stdout (process orchestration only — capture code is
untouched, §5.2). Implement reconnection (§5.3): for `--tcp`/`--pipe`, retry with
backoff on EOF/error and resume; stdin/`--spawn` EOF moves to `Disconnected`.
Write `docs/calibration-ui.md` covering how to run against each source, the
profile file format, and how downstream features consume `tablet-process`; include
the manual hardware checklist (§9).

**Files:**
- CREATE: `crates/tablet-ui/src/spawn.rs` (child `tablet-cli` process + stdout pipe)
- MODIFY: `crates/tablet-ui/src/source.rs` (reconnect/backoff for tcp/pipe)
- MODIFY: `crates/tablet-ui/src/app.rs` (connection status UI; spawn wiring)
- CREATE: `docs/calibration-ui.md` (usage, profile format, downstream use, checklist)

**Steps:**
1. Implement `--spawn`: start `tablet-cli` with the requested capture flags,
   capture its stdout, and feed it to the reader as the stdin/postcard path.
2. Add reconnect-with-backoff for TCP/pipe sources; surface connection status in
   the UI; clean up on exit (terminate a spawned child).
3. Write `docs/calibration-ui.md`: runnable commands for stdin pipe, TCP, named
   pipe, and `--spawn`; the `*.cal.toml` format; a short downstream-usage snippet
   (`CalibrationProfile::load(...).apply(...)`); and the manual hardware
   checklist (trace tracks pen, pressure width responds, tilt vector correct,
   eraser/airbrush detected, proximity fires, fitted geometry lands a test grid).

**Acceptance Criteria:**
- [ ] `--spawn` brings up the producer and UI together and streams live data.
- [ ] TCP/pipe sources reconnect after the producer restarts; status is shown.
- [ ] A spawned child process is cleaned up on UI exit.
- [ ] `docs/calibration-ui.md` has runnable commands, the profile format, a
      downstream-usage snippet, and the manual checklist.
```
