# SPEC_BGC.md — Background Pen Capture (Raw Input rebuild)

> **Status:** design reference for the *Background Capture* sprint. This spec
> **supersedes the capture mechanism in `SPEC_1.md` §6** (Wintab). The
> platform-agnostic core (`tablet-core`), the stream/IPC layer
> (`tablet-stream`), and the calibration UI (`SPEC_CUI.md`) are **retained**;
> only the *acquisition path* is rebuilt. Where this file and `SPEC_1.md`
> disagree about how packets are acquired, **this file wins**.

---

## 1. Why this rebuild exists

The product requirement is to capture pen/stylus input **continuously while a
different application (a DAW / music app) owns the foreground window**. The
capturing process is therefore a **background app by design** — it must never
need focus to receive data.

Wacom's **Wintab** delivers `WT_PACKET` only to the **foreground
application**. This is not a configuration mistake and is not fixable by:

- merging the capture and UI processes (the merged process is still background
  while the DAW is focused);
- clearing `CXO_SYSTEM` (that controls *system-cursor movement*, not
  packet-delivery focus — `SPEC_1.md:294` is wrong on this point);
- forcing context overlap with `WTOverlap(hctx, TRUE)` on a timer (racy, fights
  the foreground app for the context, and does not reliably stick on modern
  Wacom drivers).

The fix is to acquire input through a **focus-independent Windows API**. We use
**Raw Input with `RIDEV_INPUTSINK`**, which is the sanctioned mechanism for
"deliver this device's input to my window even when I am not the foreground
window." The pen is consumed as a **HID digitizer**, and reports are parsed with
the HID parsing API (`HidP_*`) against the device's preparsed data.

### Consequence for the existing design

- `tablet-wintab` is **deprecated for capture** and is not on the default path.
  It may remain in the tree as a reference/fallback behind a non-default
  feature, but no sprint task depends on it.
- The **two-process split (`tablet-cli` capturer + `tablet-ui` consumer over
  TCP) becomes valid again.** It was never the cause of the failure — Raw Input
  lets the background capturer keep receiving data regardless of which process
  is focused. No process-architecture change is required.

---

## 2. Goals & priorities (unchanged from SPEC_1 §2, restated)

In priority order:

1. **Background capture.** Receive every pen report while another app is
   foreground. This is the defining requirement of this sprint.
2. **Maximum spatial resolution.** Report native HID **logical** X/Y units
   (digitizer counts), not screen pixels. Driven by the device's HID logical
   min/max, which are typically far finer than screen resolution.
3. **Lossless capture.** No dropped reports under normal load; detect and
   surface any loss.
4. **Full axis fidelity.** Pressure, X/Y tilt, twist, in-range, tip/barrel/
   eraser switches — whatever the device's HID report descriptor exposes.
5. **Low added latency** (< ~2 ms capture → transport), same target as SPEC_1.

Non-goals for this sprint: macOS/Linux backends, Windows Ink/RealTimeStylus,
and any UI changes beyond what is needed to consume the rebuilt stream.

---

## 3. Architecture

A Cargo workspace, same shape as SPEC_1 but with the Wintab backend replaced by
a Raw Input backend:

- **`tablet-core`** — *unchanged.* Platform-agnostic `PenSample`, `ToolKind`,
  `AxisInfo`, `AxisUnit`, `DeviceCapabilities`, `SampleEvent`, `TabletBackend`,
  `BackendError`. The Raw Input backend maps onto these existing types so the
  stream layer and UI are untouched. (Field-level adaptations in §6.)
- **`tablet-rawinput`** *(new, `cfg(windows)`, feature `backend-rawinput`)* —
  the Raw Input + HID capture backend implementing `TabletBackend`. All
  `unsafe` Win32/HID FFI is isolated here behind safe wrappers. Owns a
  message-only window, the capture thread, device enumeration, and HID report
  decode.
- **`tablet-stream`** — *unchanged.* WCAP wire format, framing, transports
  (stdout / TCP / named pipe).
- **`tablet-cli`** — selects `tablet-rawinput` as the default backend on
  Windows; otherwise unchanged (clap args, TOML config, lifecycle, tracing,
  metrics).
- **`tablet-process` / `tablet-ui`** — *unchanged* (`SPEC_CUI.md`); they consume
  the same stream and never touch capture.

### 3.1 Data flow

```
Wacom pen → HID digitizer → Raw Input (RIDEV_INPUTSINK)
          → [Capture thread: WM_INPUT / GetRawInputBuffer → HidP_* decode → PenSample]
          → SPSC ring (drop-oldest) → [Streaming thread: serialize + frame] → transport → consumer
```

### 3.2 Threading (same separation of concerns as SPEC_1 §4.3)

- **Capture thread** owns the message-only window and the Raw Input
  registration, runs the Win32 message loop, and on each `WM_INPUT` (or via
  batched `GetRawInputBuffer`) decodes HID reports to `PenSample` and pushes to
  the ring. **Hot path: no I/O, no heap allocation.** Preparsed data and value
  caps are looked up once per device and cached.
- **Streaming thread** pops from the ring, serializes, frames, writes to the
  transport. Owns all I/O and backpressure.
- **Main thread** handles lifecycle, config, signals, metrics.

The capture→stream handoff is a **bounded SPSC ring** with **drop-oldest**
overflow policy and a `dropped` counter in metrics. The capture thread must
never block.

---

## 4. Raw Input registration

Register for the HID **Digitizer** usage page (`0x0D`), both relevant top-level
usages, with the input-sink flag so the window receives data while in the
background. The `hwndTarget` is our **message-only window** (`HWND_MESSAGE`
parent) — `RIDEV_INPUTSINK` *requires* a non-null `hwndTarget`.

```c
RAWINPUTDEVICE rid[2];

// Digitizer (top-level collection that wraps pen/stylus on most Wacom devices)
rid[0].usUsagePage = 0x0D;   // HID_USAGE_PAGE_DIGITIZER
rid[0].usUsage     = 0x01;   // Digitizer
rid[0].dwFlags     = RIDEV_INPUTSINK;
rid[0].hwndTarget  = hMsgWnd;

// Pen
rid[1].usUsagePage = 0x0D;
rid[1].usUsage     = 0x02;   // Pen
rid[1].dwFlags     = RIDEV_INPUTSINK;
rid[1].hwndTarget  = hMsgWnd;

RegisterRawInputDevices(rid, 2, sizeof(RAWINPUTDEVICE));
```

Notes:
- Registering both `0x01` (Digitizer) and `0x02` (Pen) is belt-and-suspenders:
  different Wacom models/firmware present the pen under one or the other
  top-level collection.
- **Do not** set `RIDEV_NOLEGACY` (we are not trying to suppress the system's
  own pointer handling; we only want a copy of the reports).
- `RIDEV_EXINPUTSINK` is a fallback to consider if a foreground app that also
  uses input-sink starves us; default to `RIDEV_INPUTSINK` and revisit only if
  testing shows starvation.
- Re-registration is needed after certain session changes; handle
  `WM_INPUT_DEVICE_CHANGE` (see §8).

---

## 5. Reading reports

Two read paths; implement the batched one for the hot path:

- **`WM_INPUT` (correctness baseline):** on each message, call
  `GetRawInputData(hRawInput, RID_INPUT, …)` into a fixed stack buffer, confirm
  `raw->header.dwType == RIM_TYPEHID`, and decode (§6). One report per message.
- **`GetRawInputBuffer` (preferred hot path):** drain *all* queued raw inputs in
  one syscall into a fixed buffer and iterate, mirroring the Wintab "drain the
  whole queue on every notification" rule (SPEC_1 §6.4). This bounds per-report
  overhead under bursts and is the lossless-drain analogue for Raw Input. Walk
  packed `RAWINPUT` records with the `NEXTRAWINPUTBLOCK` alignment macro.

A single HID report may contain **multiple reports concatenated**
(`raw->data.hid.dwCount` records of `dwSizeHid` bytes each); iterate all of
them.

### 5.1 Timestamps

Raw Input carries no high-resolution device timestamp. Set `t_capture_ns` from
`QueryPerformanceCounter` taken **once per drain burst** (same discipline as
SPEC_1). `t_device_ms` has no HID source on most pens — set it to `0` (or a
millisecond QPC derivative) and document that it is host-derived, not device.

### 5.2 Loss detection

HID reports have **no per-packet serial number** (unlike Wintab's
`PK_SERIAL_NUMBER`). We therefore **synthesize a monotonic `serial`** counter
incremented per accepted report; it preserves `PenSample.serial`'s gap-detection
contract for downstream code, but a gap can only ever be produced by *our own*
ring overflow, not by a driver queue (Raw Input has no app-visible queue depth).
Real loss is surfaced exclusively via the ring `dropped` metric. `queue_size`
in `DeviceCapabilities` loses its Wintab meaning — repurpose it as the
`GetRawInputBuffer` batch capacity (reports per drain) or set it to `0`.

---

## 6. HID decode → `PenSample`

Decode each report against the owning device's cached `PHIDP_PREPARSED_DATA`
using `HidP_GetUsageValue` / `HidP_GetUsages`. Per-device, cache the preparsed
data and the value-cap list (logical min/max per usage) so the hot path does no
re-query.

### 6.1 Usage map

| `PenSample` field        | HID usage page / usage          | Notes |
|--------------------------|----------------------------------|-------|
| `x_raw`                  | Generic Desktop `0x01` / `0x30` (X) | logical units = native resolution |
| `y_raw`                  | Generic Desktop `0x01` / `0x31` (Y) | |
| `pressure_raw`           | Digitizer `0x0D` / `0x30` (Tip Pressure) | |
| `tilt_x_deg`             | Digitizer `0x0D` / `0x3D` (X Tilt) | usually signed degrees |
| `tilt_y_deg`             | Digitizer `0x0D` / `0x3E` (Y Tilt) | |
| `twist_deci_deg`         | Digitizer `0x0D` / `0x41` (Twist) | scale to 0.1° units |
| `in_proximity`           | Digitizer `0x0D` / `0x32` (In Range) | button usage |
| tip switch → `buttons`b0 | Digitizer `0x0D` / `0x42` (Tip Switch) | |
| barrel switch → `buttons`b1 | Digitizer `0x0D` / `0x44` (Barrel Switch) | |
| eraser/invert → tool     | Digitizer `0x0D` / `0x45` (Eraser), `0x3C` (Invert) | selects `ToolKind::Eraser` |
| `tangent_pressure_raw`   | Digitizer secondary barrel/wheel, if present | airbrush only; `None` otherwise |

Unmapped axes (`z_raw`, `azimuth/altitude`, `rotation`) are set to their
"unsupported" values: `z_raw = 0`, the `Option<…>` orientation fields to `None`.
Where the device reports azimuth/altitude instead of X/Y tilt, derive `tilt_*`
via `tilt_from_orientation` (reuse `tablet-core::normalize`).

### 6.2 Normalization

`x_norm`, `y_norm`, `pressure_norm` are computed with the existing
`tablet-core::normalize` against the **HID logical min/max** captured at
enumeration. No screen scaling — logical range is the native device range.

### 6.3 `ToolKind`

- Eraser usage (`0x45`) active **or** Invert (`0x3C`) set → `ToolKind::Eraser`.
- Presence of a secondary pressure/wheel collection → `ToolKind::Airbrush`.
- Otherwise `ToolKind::Pen`. `ToolKind::Cursor`/`Unknown` as fallbacks.

`tool_serial`: most consumer Wacom pens do **not** expose a per-pen serial over
standard HID; set `tool_serial = 0` unless a vendor-defined usage provides one.

---

## 7. `DeviceCapabilities` from HID

Built **once at enumeration** (and rebuilt on device arrival, §8), replacing the
`WTInfo` source:

- Enumerate with `GetRawInputDeviceList`; keep entries where `dwType ==
  RIM_TYPEHID` and `RID_DEVICE_INFO_HID.usUsagePage == 0x0D` with usage `0x01`
  or `0x02`.
- `device_name` ← `GetRawInputDeviceInfo(…, RIDI_DEVICENAME, …)` (the device
  interface path; optionally prettified via SetupAPI). `driver_version` ←
  best-effort from the HID `RID_DEVICE_INFO_HID` VID/PID/version, or `"hid"`.
- Per axis `AxisInfo { min, max, resolution, unit, supported }` from the
  preparsed value caps: `min/max` ← HID logical min/max; `resolution` ← HID
  physical units / unit-exponent when present, else `0.0`; `unit` ← `Degree`
  for tilt/twist, `None` otherwise; `supported` ← whether that usage exists in
  the report descriptor.
- `max_packet_rate_hz`: HID has no negotiated rate. Either leave `0` (unknown)
  or **measure** it (count reports over the first ~1 s and report the observed
  rate). Measuring is preferred so the UI shows a real number.
- `queue_size`: see §5.2 — repurposed or `0`.

`SampleEvent::Capabilities` is emitted once at session start and re-emitted on
device arrival/removal, exactly as today.

---

## 8. Lifecycle & hot-plug

- Register for device-change notifications (`RIDEV_DEVNOTIFY` on the
  registration, or `RegisterDeviceNotification`) and handle
  `WM_INPUT_DEVICE_CHANGE` (`GIDC_ARRIVAL` / `GIDC_REMOVAL`):
  - **Arrival:** enumerate the new device, build/refresh preparsed-data cache and
    `DeviceCapabilities`, re-emit `SampleEvent::Capabilities`.
  - **Removal:** drop its cache entry; emit an out-of-range proximity event if it
    was the active tool.
- Proximity: emit `SampleEvent::Proximity { in_range, tool_serial }` on
  transitions of the In-Range usage (`0x32`), same contract as today.
- Shutdown: post a stop message to the window, unregister the raw input devices
  (`RIDEV_REMOVE`), destroy the window, join the capture thread.

---

## 9. Wacom configuration requirement

For the pen to present as a **standard HID digitizer** (which is what Raw Input
parses), **"Use Windows Ink" must be ON** in Wacom Tablet Properties. With it
OFF, the driver biases toward the Wintab path and the standard digitizer
collection can behave differently or go silent. This is the **opposite** of
typical Wintab guidance and must be documented in the README and surfaced in a
startup diagnostic when no digitizer collection is found.

If enumeration finds no Digitizer-usage HID device, fail with a clear,
actionable error (new `BackendError` variant, §11) telling the user to connect
the tablet and enable Windows Ink — never panic.

---

## 10. Stream / IPC layer (unchanged)

`tablet-stream` is reused verbatim: WCAP magic + version + format header, framed
`Capabilities / Sample / Proximity / Metrics / Heartbeat` messages, postcard
(default) and JSONL (debug) formats, transports stdout / TCP / named pipe. The
`SampleEvent → StreamMessage` mapping is identical to the current
`tablet-cli` runtime. No wire-format change is required by this sprint.

---

## 11. `tablet-core` changes (minimal)

The core types are kept. The only additive change is error vocabulary so the
Raw Input backend can report precisely:

- Add `BackendError` variants (or repurpose existing ones with Raw Input
  wording): e.g. `RawInputRegistrationFailed`, and reuse `NoDevice` for "no HID
  digitizer found (is Windows Ink enabled?)". `DriverMissing`'s Wintab-specific
  text should be generalized or left only on the deprecated Wintab path.
- Document on `PenSample` that, under the Raw Input backend, `serial` is
  host-synthesized, `t_device_ms` is host-derived, and `tool_serial` may be `0`.

No change to `PenSample`/`DeviceCapabilities` field layout — preserving wire
compatibility with existing consumers.

---

## 12. Testing without hardware

Same philosophy as SPEC_1 §12 — most tests run on any OS with no tablet:

- **Decode unit tests:** feed **captured/synthetic HID report byte buffers** plus
  a synthetic value-cap table into the decode function and assert the resulting
  `PenSample` (X/Y, pressure, tilt, switches, tool kind). This is the core new
  test surface and is fully host-independent (the decode logic takes bytes +
  caps, not live OS handles).
- **Normalization/tilt tests:** reuse existing `tablet-core::normalize` tests.
- **End-to-end transport/framing:** reuse the existing `MockBackend` to drive the
  stream layer — unchanged.
- **Loss/metrics:** inject ring overflow and assert the `dropped` metric (serial
  gaps now originate only from our ring, §5.2).
- **Manual hardware checklist (Windows + Wacom):** confirm reports arrive while a
  DAW is foreground (the defining acceptance test); verify full axis fidelity and
  the "Use Windows Ink ON" requirement.

Isolate live Win32/HID handle calls behind thin wrappers so the decode core
stays pure and unit-testable.

---

## 13. Migration / what changes in the tree

- **Add** crate `tablet-rawinput` with feature `backend-rawinput` (default on
  Windows).
- **Re-point** `tablet-cli::build_backend` to construct `tablet-rawinput` on
  Windows; keep the mock fallback for non-Windows/tests.
- **Demote** `tablet-wintab` to a non-default `backend-wintab` feature (kept for
  reference; no task depends on it). Optionally extract the message-only-window
  helper into a shared util both backends can use.
- **Update** `SPEC_1.md` §6.3/§6 with a pointer to this file noting the capture
  mechanism changed; **update** the README with the Windows Ink requirement.

---

## 14. Open questions / risks

1. **Report-descriptor variation across Wacom models.** Tilt may appear as X/Y
   tilt *or* azimuth/altitude; twist and tangent pressure are model-dependent.
   The decode path must be driven entirely by the device's value caps, not
   hard-coded field offsets. Mitigation: capture real report descriptors from
   target hardware early and add them as decode test fixtures.
2. **Latency of the Win32 message queue.** `WM_INPUT` is delivered via the thread
   message queue; confirm the < ~2 ms target holds under load, and prefer
   `GetRawInputBuffer` batching. Measure on hardware.
3. **`RIDEV_INPUTSINK` vs `RIDEV_EXINPUTSINK` under a competing foreground app.**
   Validate that a focused DAW that itself consumes pen input does not starve us;
   escalate to `EXINPUTSINK` if it does.
4. **Windows Ink dependency.** Capture silently breaks if the user toggles
   Windows Ink off. Surface this as an explicit, detectable startup diagnostic.
5. **No device serial / device timestamp.** Accept host-synthesized `serial` and
   host-derived timing; document the fidelity boundary for consumers.
```

---

## 15. Sprint task outline (to expand in TASKS_BGC.md)

- **B1 — `tablet-rawinput` scaffold:** crate, feature `backend-rawinput`,
  message-only window (reuse/extract), empty `TabletBackend` impl returning
  `NotImplemented`.
- **B2 — Enumeration + capabilities:** `GetRawInputDeviceList`, filter digitizer
  HID, preparsed-data + value-cap cache, build `DeviceCapabilities`. Unit-tested
  with synthetic caps.
- **B3 — Registration + message loop:** `RegisterRawInputDevices`
  (`RIDEV_INPUTSINK`), capture thread, `WM_INPUT` + `GetRawInputBuffer` drain,
  QPC timestamping, synthesized serial.
- **B4 — HID decode → `PenSample`:** `HidP_*` parsing per §6, normalization,
  `ToolKind`, proximity. **Pure decode function unit-tested with captured report
  bytes** (the key host-independent test surface).
- **B5 — Lifecycle/hot-plug:** `WM_INPUT_DEVICE_CHANGE`, re-emit capabilities,
  proximity transitions, clean shutdown (`RIDEV_REMOVE`).
- **B6 — Wire into `tablet-cli` + diagnostics:** default to `backend-rawinput`
  on Windows, Windows-Ink-off detection + actionable error, README/SPEC_1
  pointer, metrics (`dropped`, measured rate).
- **B7 — Manual hardware validation:** background-capture acceptance (reports
  arrive while a DAW is focused) + axis-fidelity checklist.
