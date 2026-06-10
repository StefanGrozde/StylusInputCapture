# TASKS_BGC.md — Background Pen Capture (Raw Input rebuild)

Implementation task breakdown derived from `SPEC_BGC.md` (the source of truth for
this sprint). Each task is dispatched to an agent with **only `SPEC_BGC.md` and
the task text**. The agent may read whatever files already exist in the repo
(i.e. the output of its completed dependency tasks), but receives no other task
descriptions.

Section references like "§6.1" point into `SPEC_BGC.md`. Where `SPEC_BGC.md` and
`SPEC_1.md` disagree about how packets are acquired, **`SPEC_BGC.md` wins**.

This sprint **replaces the Wintab capture path** with a focus-independent Raw
Input + HID backend so capture continues while another app (a DAW) is the
foreground window. It keeps `tablet-core`, `tablet-stream`, and the UI crates
untouched; it adds one crate (`tablet-rawinput`) and re-points `tablet-cli` to
it.

Build order: **B1** (scaffold) → **B2** (enumeration/caps) and **B4** (decode)
in parallel → **B3** (registration/loop, needs B1+B2) → **B5** (lifecycle, needs
B3+B4) → **B6** (CLI wiring + diagnostics, needs B3+B4) → **B7** (manual
hardware validation, needs B6).

Dependency rule: a task is only eligible to run once every task in its
**Depends on** list is `DONE`. Update statuses as work completes.

> **Relationship to existing sprints:** `tablet-wintab` (Sprint 3 of `TASKS.md`)
> is demoted to a non-default `backend-wintab` feature and kept for reference; no
> task here depends on it. `tablet-stream` (Sprint 2) and the UI crates
> (`TASKS_CUI.md`) are reused verbatim.

---

# Sprint B — Background Capture (`tablet-rawinput`)
Branch: `sprint-bgc-rawinput`
Status: B1–B6 DONE (code complete, builds + tests pass on Windows); B7 manual hardware run pending

## Tasks

### B1 — Scaffold `tablet-rawinput` crate & message-only window
Status: DONE
Depends on: none

**Context:**
New capture crate (§3). Create `tablet-rawinput` as a `cfg(windows)` workspace
member behind feature `backend-rawinput`, depending only on `tablet-core` (plus
`windows`/`windows-sys`, `tracing`, `thiserror`). Provide a **message-only
window** (`HWND_MESSAGE` parent) — Raw Input with `RIDEV_INPUTSINK` requires a
non-null `hwndTarget` (§4). If the Wintab message-only-window helper is suitable,
extract it into a small shared util both crates can use; otherwise create a fresh
one here. Provide a `RawInputBackend` skeleton implementing `TabletBackend` whose
methods return `BackendError::NotImplemented { backend: "rawinput" }` for now.

**Files:**
- CREATE: `crates/tablet-rawinput/Cargo.toml`
- CREATE: `crates/tablet-rawinput/src/lib.rs`
- CREATE: `crates/tablet-rawinput/src/window.rs` (message-only window create/destroy + WndProc shell)
- CREATE: `crates/tablet-rawinput/src/backend.rs` (`RawInputBackend: TabletBackend`, stubbed)
- MODIFY: root `Cargo.toml` (add member + `backend-rawinput` feature wiring)

**Steps:**
1. Add the crate to the workspace and define the `backend-rawinput` feature
   (default on Windows in `tablet-cli`, B6).
2. Implement message-only window creation (`CreateWindowExA` with `HWND_MESSAGE`
   parent) and a `wnd_proc` that currently only handles `WM_DESTROY`/a stop
   message; reserve `WM_INPUT` and `WM_INPUT_DEVICE_CHANGE` arms for later tasks.
3. Define `RawInputBackend` with `capabilities()`/`start()`/`stop()` returning
   `NotImplemented` so the crate compiles and links against `tablet-core`.
4. Confirm the crate builds under `cargo build -p tablet-rawinput` on Windows and
   is excluded cleanly on non-Windows.

**Acceptance Criteria:**
- [ ] `tablet-rawinput` is a workspace member gated to Windows + `backend-rawinput`.
- [ ] A message-only window can be created and destroyed without leaking the class.
- [ ] `RawInputBackend` implements `TabletBackend` (stubbed) and the crate compiles.
- [ ] No `unsafe` appears in the crate's public function signatures.

---

### B2 — Device enumeration & `DeviceCapabilities` from HID
Status: DONE
Depends on: B1

**Context:**
Build the capability handshake from HID, replacing `WTInfo` (§7). Enumerate with
`GetRawInputDeviceList`, keep entries where `dwType == RIM_TYPEHID` and the
`RID_DEVICE_INFO_HID` usage page is `0x0D` with usage `0x01` (Digitizer) or
`0x02` (Pen). For each, fetch the device name (`RIDI_DEVICENAME`) and the
preparsed data (`RIDI_PREPARSEDDATA` → `PHIDP_PREPARSED_DATA`), walk
`HidP_GetValueCaps`/`HidP_GetButtonCaps` to learn which usages exist and their
**logical min/max**, and assemble a `tablet_core::DeviceCapabilities`. Cache the
preparsed data + value-cap table per device handle for the decode hot path (B4).
Set `AxisInfo.supported` from descriptor presence; `unit = Degree` for tilt/twist
else `None`; `resolution` from HID physical units/unit-exponent when present else
`0.0`. `max_packet_rate_hz = 0` here (measured later, B6); `queue_size = 0` or the
batch capacity (§5.2). If no digitizer HID device is found, return `NoDevice` with
text pointing at the Windows-Ink requirement (§9).

**Files:**
- CREATE: `crates/tablet-rawinput/src/enumerate.rs` (device list filter, name lookup)
- CREATE: `crates/tablet-rawinput/src/caps.rs` (preparsed-data → `DeviceCapabilities`, value-cap cache type)
- MODIFY: `crates/tablet-rawinput/src/lib.rs`

**Steps:**
1. Implement `enumerate_digitizers()` → list of `(HANDLE, RID_DEVICE_INFO_HID,
   device_name)` filtered to usage page `0x0D`, usages `0x01`/`0x02`.
2. Implement a `DeviceProfile` cache holding `PHIDP_PREPARSED_DATA` + parsed
   value/button caps (logical min/max keyed by `(usage_page, usage)`).
3. Implement `capabilities_from_profile(&DeviceProfile) -> DeviceCapabilities`
   mapping each axis per §6.1/§7; mark unsupported axes correctly.
4. Wire `RawInputBackend::capabilities()` to enumerate, build the first device's
   caps, and return `NoDevice` (Windows-Ink-aware message) when none match.
5. Add unit tests that build a synthetic value-cap table (no live handles) and
   assert the resulting `DeviceCapabilities` (min/max, `supported`, `unit`).

**Acceptance Criteria:**
- [ ] Enumeration returns only HID digitizer/pen devices (usage page `0x0D`).
- [ ] `DeviceCapabilities` is populated from HID logical min/max with `supported`
      set per descriptor presence.
- [ ] Absent digitizer ⇒ `BackendError::NoDevice` with a Windows-Ink hint.
- [ ] Capability assembly is unit-tested with synthetic caps (no hardware).

---

### B4 — HID report decode → `PenSample` (pure, unit-tested)
Status: DONE
Depends on: B2

**Context:**
The core new test surface (§6, §12). Implement a **pure** decode function that
takes a single HID report byte slice **plus the device's value-cap table** (from
B2) and produces a `tablet_core::PenSample` — no live OS handles, so it is fully
unit-testable on any OS. Parse with `HidP_GetUsageValue`/`HidP_GetUsages` per the
§6.1 usage map: X/Y on Generic Desktop `0x01` usages `0x30`/`0x31`; pressure
(`0x0D`/`0x30`), X/Y tilt (`0x3D`/`0x3E`), twist (`0x41`), in-range (`0x32`), tip
(`0x42`), barrel (`0x44`), eraser (`0x45`), invert (`0x3C`) on the Digitizer page.
Compute `x_norm`/`y_norm`/`pressure_norm` via `tablet_core::normalize` against the
logical min/max. Derive `tilt_*` from azimuth/altitude via
`tablet_core::tilt_from_orientation` when the device reports orientation instead
of tilt. Select `ToolKind` per §6.3 (eraser/invert ⇒ `Eraser`; secondary
wheel ⇒ `Airbrush`; else `Pen`). Set `serial`, `t_capture_ns`, `t_device_ms`
from inputs passed by the caller (synthesized/host-derived per §5). `z_raw=0`,
orientation/rotation `Option`s `None` when unsupported; `tool_serial=0` unless a
vendor usage supplies one.

**Files:**
- CREATE: `crates/tablet-rawinput/src/decode.rs` (`decode_report(report, &DeviceProfile, t_capture_ns, serial) -> PenSample`)
- MODIFY: `crates/tablet-rawinput/src/lib.rs`

**Steps:**
1. Implement `decode_report` taking `(&[u8] report, &DeviceProfile, t_capture_ns,
   serial)` and returning `PenSample`, reading each usage via `HidP_*` against the
   profile's preparsed data.
2. Apply normalization and tilt derivation from `tablet-core`.
3. Implement `ToolKind` selection and the buttons bitmask assembly.
4. Add unit tests driven by **captured/synthetic HID report byte buffers** +
   synthetic caps, asserting X/Y, pressure, tilt, switches, `in_proximity`, and
   `ToolKind`. Include at least one eraser/invert case and one pen case.

**Acceptance Criteria:**
- [ ] `decode_report` is pure (bytes + caps in, `PenSample` out) — no OS handles.
- [ ] All §6.1 usages are mapped; unsupported axes take their documented defaults.
- [ ] Normalization and tilt reuse `tablet-core::normalize`/`tilt_from_orientation`.
- [ ] Decode is unit-tested with synthetic report bytes covering pen + eraser.

---

### B3 — Raw Input registration, capture thread & lossless drain
Status: DONE
Depends on: B1, B2

**Context:**
The hot path (§3.2, §4, §5). On `start(sink)`, spawn the capture thread which:
creates the message-only window (B1), registers the digitizer/pen usages with
`RegisterRawInputDevices` using `RIDEV_INPUTSINK` and `hwndTarget = hMsgWnd`
(also set `RIDEV_DEVNOTIFY` so B5 gets device-change messages), emits the initial
`SampleEvent::Capabilities` (B2), then runs the Win32 message loop. On each
`WM_INPUT`, **drain all queued reports** — prefer batched `GetRawInputBuffer`
into a fixed pre-sized buffer, iterating the packed `RAWINPUT` blocks with the
`NEXTRAWINPUTBLOCK` alignment macro, and within each block iterating
`dwCount × dwSizeHid` concatenated HID reports (§5). For each report: capture a
`QueryPerformanceCounter` ns timestamp **once per drain burst**, increment the
synthesized monotonic `serial`, call `decode_report` (B4), and push
`SampleEvent::Sample` via `sink`. **No I/O, no heap allocation on this path** —
preparsed data and value caps are looked up once per device (B2) and cached.
`stop()` posts a stop message, unregisters with `RIDEV_REMOVE`, destroys the
window, joins the thread.

**Files:**
- CREATE: `crates/tablet-rawinput/src/register.rs` (`RegisterRawInputDevices`/`RIDEV_REMOVE` wrappers, flag constants)
- CREATE: `crates/tablet-rawinput/src/capture.rs` (capture thread + drain loop + QPC helper)
- MODIFY: `crates/tablet-rawinput/src/window.rs` (handle `WM_INPUT`)
- MODIFY: `crates/tablet-rawinput/src/backend.rs` (`start`/`stop` real impl)

**Steps:**
1. Implement registration with `RIDEV_INPUTSINK | RIDEV_DEVNOTIFY` for usage page
   `0x0D` usages `0x01` and `0x02`, targeting the message-only window.
2. Implement the capture thread: build window → register → emit Capabilities →
   message loop; `stop()` tears down in reverse with `RIDEV_REMOVE`.
3. Implement the `WM_INPUT` drain: `GetRawInputBuffer` into a fixed buffer, walk
   blocks/records, decode and `sink`-push each report; fall back to a single
   `GetRawInputData(RID_INPUT)` read if `GetRawInputBuffer` is unavailable.
4. Add a QPC helper (`QueryPerformanceCounter`/`QueryPerformanceFrequency` →
   monotonic ns) captured once per burst.
5. Maintain a `dropped` counter (`Arc<AtomicU64>`) for ring overflow visibility
   (the only real loss signal under Raw Input, §5.2).
6. Ensure the drain loop performs no allocation (pre-sized buffer) and no I/O.

**Acceptance Criteria:**
- [ ] `RegisterRawInputDevices` uses `RIDEV_INPUTSINK` with a non-null `hwndTarget`.
- [ ] `WM_INPUT` handling drains all queued reports per notification (batched).
- [ ] One QPC ns timestamp per drain burst; `serial` increments monotonically.
- [ ] `stop()` unregisters (`RIDEV_REMOVE`), destroys the window, and joins cleanly.
- [ ] No heap allocation or I/O inside the drain loop (reviewable).

---

### B5 — Lifecycle: hot-plug, proximity & clean shutdown
Status: DONE
Depends on: B3, B4

**Context:**
Device arrival/removal and proximity (§8). Handle `WM_INPUT_DEVICE_CHANGE`
(`GIDC_ARRIVAL`/`GIDC_REMOVAL`): on arrival, enumerate the new device, build/
refresh its preparsed-data + caps cache (B2), and re-emit
`SampleEvent::Capabilities`; on removal, drop its cache entry and, if it was the
active tool, emit an out-of-range `SampleEvent::Proximity`. Track the In-Range
usage (`0x32`) across reports and emit `SampleEvent::Proximity { in_range,
tool_serial }` on transitions, matching the existing event contract.

**Files:**
- MODIFY: `crates/tablet-rawinput/src/window.rs` (handle `WM_INPUT_DEVICE_CHANGE`)
- MODIFY: `crates/tablet-rawinput/src/capture.rs` (proximity transition tracking, cache refresh)
- MODIFY: `crates/tablet-rawinput/src/caps.rs` (incremental add/remove of a device profile)

**Steps:**
1. Add `WM_INPUT_DEVICE_CHANGE` handling that refreshes the per-device cache and
   re-emits `Capabilities` on arrival; removes the cache entry on removal.
2. Track previous In-Range state per device and emit `Proximity` on enter/leave.
3. On removal of the active device, emit a leaving-proximity event.
4. Add a unit test for proximity-transition logic (pure state machine over a
   sequence of decoded in-range flags — no hardware).

**Acceptance Criteria:**
- [ ] Device arrival re-emits `SampleEvent::Capabilities`; removal drops the cache.
- [ ] In-range transitions emit `SampleEvent::Proximity` with correct `in_range`.
- [ ] Proximity transition logic is unit-tested without hardware.

---

### B6 — Wire into `tablet-cli`, diagnostics & metrics
Status: DONE
Depends on: B3, B4

**Context:**
Make Raw Input the default capture path (§13) and surface the Windows-Ink
requirement (§9). Re-point `tablet-cli::build_backend` to construct
`RawInputBackend` on Windows under `backend-rawinput`; keep the mock fallback for
non-Windows/tests. Demote `tablet-wintab` to a non-default `backend-wintab`
feature. Measure the packet rate over the first ~1 s of capture and store it in
`DeviceCapabilities.max_packet_rate_hz` (replacing the absent negotiated rate).
When enumeration finds no digitizer HID device, fail with an actionable
diagnostic telling the user to connect the tablet and **enable "Use Windows
Ink"**. Surface `dropped` (ring overflow) and the measured rate through the
existing metrics path.

**Files:**
- MODIFY: `crates/tablet-cli/src/runtime.rs` (`build_backend` selects `RawInputBackend`)
- MODIFY: `crates/tablet-cli/Cargo.toml` (depend on `tablet-rawinput`; feature plumbing)
- MODIFY: root `Cargo.toml` / feature defaults (`backend-rawinput` default on Windows, `backend-wintab` opt-in)
- MODIFY: `README` / docs (Windows Ink requirement, new default backend)
- MODIFY: `SPEC_1.md` §6 (pointer noting capture mechanism moved to `SPEC_BGC.md`)

**Steps:**
1. Add `tablet-rawinput` as a `cfg(windows)` dependency of `tablet-cli` behind
   `backend-rawinput`; make it the default Windows feature, `backend-wintab`
   opt-in.
2. Update `build_backend` to construct `RawInputBackend` on Windows; preserve the
   mock path otherwise.
3. Implement first-second rate measurement feeding `max_packet_rate_hz`.
4. Add the no-device / Windows-Ink-off startup diagnostic (clear, non-panicking).
5. Update README + add the `SPEC_1.md` §6 pointer to `SPEC_BGC.md`.

**Acceptance Criteria:**
- [ ] On Windows, `tablet-cli` captures via Raw Input by default (no Wintab).
- [ ] Missing digitizer ⇒ actionable error mentioning the Windows Ink toggle.
- [ ] Measured packet rate appears in `DeviceCapabilities`/metrics.
- [ ] `cargo build`/`cargo test` pass for the workspace; non-Windows uses the mock.

---

### B7 — Manual hardware validation (Windows + Wacom)
Status: CHECKLIST READY (`docs/bgc-hardware-checklist.md`); manual hardware run pending
Depends on: B6

**Context:**
The defining acceptance test for this sprint (§2, §12). Validate on real hardware
with "Use Windows Ink" ON. This task produces a checklist run + notes, not code.

**Files:**
- CREATE: `docs/bgc-hardware-checklist.md` (results log)

**Steps:**
1. **Background capture (defining test):** focus a DAW (or any non-capture app),
   draw with the pen, confirm `tablet-ui` (over TCP) shows live samples while the
   DAW stays foreground.
2. **Axis fidelity:** verify X/Y at full logical resolution, pressure across its
   full range, X/Y tilt, twist (if supported), tip/barrel/eraser switches, and
   eraser→`ToolKind::Eraser`.
3. **Proximity/hot-plug:** lift/approach the pen (proximity events); unplug/replug
   the tablet (capabilities re-emit).
4. **Windows Ink OFF regression:** toggle Windows Ink off, confirm the startup
   diagnostic fires (no silent failure, no panic).
5. **Latency/loss sanity:** sustained drawing shows no `dropped` growth under
   normal load; spot-check responsiveness.

**Acceptance Criteria:**
- [ ] Live samples are received while a different app is the foreground window.
- [ ] All supported axes and switches read correctly; eraser maps to `Eraser`.
- [ ] Proximity and hot-plug events fire; capabilities re-emit on arrival.
- [ ] Windows-Ink-off produces the diagnostic, not a silent stall or panic.
