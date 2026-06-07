# TASKS.md — Wacom Pen Capture

Implementation task breakdown derived from `SPEC_1.md` (the source of truth).
Each task is dispatched to an agent with **only `SPEC_1.md` and the task text**.
The agent may read whatever files already exist in the repo (i.e. the output of
its completed dependency tasks), but receives no other task descriptions.

Section references like "§6.3" point into `SPEC_1.md`.

Build order: **Sprint 1** (workspace + `tablet-core`) → **Sprint 2**
(`tablet-stream` + mock backend) and **Sprint 3** (`tablet-wintab`) in parallel
→ **Sprint 4** (`tablet-cli` + integration + hardening + stubs).

Dependency rule: a task is only eligible to run once every task in its
**Depends on** list is `DONE`. Update statuses as work completes.

> **Calibration & Visualization UI feature** (separate, builds on top of this
> capture pipeline): see `SPEC_CUI.md` for the design and `TASKS_CUI.md` for its
> task breakdown (Sprints C1–C3: `tablet-process` + `tablet-ui`). It adds two new
> crates and does not modify the capture crates below.

---

# Sprint 1 — Workspace & Core Types
Branch: `sprint-1-workspace-core`
Status: DONE

## Tasks

### T1.1 — Scaffold Cargo workspace
Status: DONE
Depends on: none

**Context:**
Pre-implementation repo: only `SPEC_1.md` and `CLAUDE.md` exist, no Cargo files,
no `crates/` dir. Per §4.1 the project is a Cargo workspace with four member
crates. The workspace root is the repo root
(`C:\Users\stefa\Documents\Claude_Code\StylusInputCapture`). Target platform is
Windows first (§1), with `tablet-wintab` compiled only under `cfg(windows)`.
Feature flags (§8.2): `backend-wintab` (default on Windows), `backend-evdev`,
`backend-macos`. Do not pin dependency versions in this task — later tasks add
deps via `cargo add` (per CLAUDE.md / §13).

**Files:**
- CREATE: `Cargo.toml` (workspace root)
- CREATE: `crates/tablet-core/Cargo.toml`
- CREATE: `crates/tablet-core/src/lib.rs` (empty module stub)
- CREATE: `crates/tablet-wintab/Cargo.toml`
- CREATE: `crates/tablet-wintab/src/lib.rs` (empty module stub)
- CREATE: `crates/tablet-stream/Cargo.toml`
- CREATE: `crates/tablet-stream/src/lib.rs` (empty module stub)
- CREATE: `crates/tablet-cli/Cargo.toml`
- CREATE: `crates/tablet-cli/src/main.rs` (`fn main() {}` stub)
- CREATE: `.gitignore` (`/target`, `Cargo.lock` kept for the binary crate)

**Steps:**
1. Create root `Cargo.toml` with `[workspace]`, `resolver = "2"`, and `members`
   listing all four crates under `crates/`.
2. Create each crate's `Cargo.toml` with name (`tablet-core`, `tablet-wintab`,
   `tablet-stream`, `tablet-cli`), edition 2021, version `0.1.0`.
3. Make `tablet-cli` a binary crate (`src/main.rs`) and the other three library
   crates (`src/lib.rs`). Leave inter-crate path dependencies commented or
   absent for now (added by later tasks).
4. In `tablet-wintab/Cargo.toml` add a `[target.'cfg(windows)'.dependencies]`
   section placeholder (empty) so the Windows-only intent is documented.
5. Add `.gitignore` for `/target`.
6. Verify the workspace builds: `cargo build` succeeds with empty stubs.

**Acceptance Criteria:**
- [ ] `cargo build` succeeds from the repo root with no errors.
- [ ] `cargo metadata` lists exactly four workspace members.
- [ ] `tablet-cli` produces a binary; the other three are libraries.
- [ ] No dependency versions are hardcoded beyond what `cargo add` would set.

---

### T1.2 — Core data model types
Status: DONE
Depends on: T1.1

**Context:**
`tablet-core` is platform-agnostic with **no OS-specific dependencies** (§4.1,
§14). Implement the canonical data model exactly as defined in `SPEC_1.md` §5.1
(`PenSample`), §5.2 (`AxisInfo`, `DeviceCapabilities`), §5.1/§8.1 (`ToolKind`),
and an `AxisUnit` enum (`Inch | Centimeter | Degree | None`) referenced by
`AxisInfo.unit`. `PenSample` must be `Copy` (POD-like) per §11 ("ring carries
PenSample by value"). All types must derive `serde::Serialize` +
`serde::Deserialize` (needed by `tablet-stream` in Sprint 2) and `Debug`,
`Clone`. `ToolKind` variants: `Pen | Eraser | Airbrush | Cursor | Unknown`.

**Files:**
- MODIFY: `crates/tablet-core/Cargo.toml` (add `serde` with `derive` feature)
- CREATE: `crates/tablet-core/src/sample.rs` (`PenSample`, `ToolKind`)
- CREATE: `crates/tablet-core/src/capabilities.rs` (`AxisInfo`, `AxisUnit`, `DeviceCapabilities`)
- MODIFY: `crates/tablet-core/src/lib.rs` (declare + re-export modules)

**Steps:**
1. `cargo add serde --features derive -p tablet-core`.
2. Implement `ToolKind` enum and `PenSample` struct field-for-field from §5.1
   (keep `Option<...>` fields, `_raw`/`_norm` pairs, `t_capture_ns`,
   `t_device_ms`, `serial`, `buttons`, `tool_serial`, `in_proximity`, `status`).
3. Derive `Copy, Clone, Debug, PartialEq, Serialize, Deserialize` on `PenSample`
   and `ToolKind`. Ensure every field type is itself `Copy`.
4. Implement `AxisUnit`, `AxisInfo`, and `DeviceCapabilities` from §5.2; derive
   `Clone, Debug, PartialEq, Serialize, Deserialize` (these need not be `Copy` —
   `DeviceCapabilities` contains `String`s).
5. Re-export all public types from `lib.rs`.

**Acceptance Criteria:**
- [ ] `cargo build -p tablet-core` succeeds.
- [ ] `PenSample` implements `Copy` (add a compile-time assertion test:
      `fn _assert_copy<T: Copy>() {} _assert_copy::<PenSample>();`).
- [ ] All field names/types match §5.1–§5.2 exactly.
- [ ] `tablet-core` has no OS-specific dependencies in `Cargo.toml`.

---

### T1.3 — Backend trait, events, and error taxonomy
Status: DONE
Depends on: T1.2

**Context:**
Define the single backend abstraction every backend implements (§8.1) plus the
error taxonomy (§10.1). `TabletBackend` trait, `SampleEvent` enum, and
`BackendError` enum. The `start` sink is invoked on the capture thread and must
be cheap (§8.1) — signature uses `Box<dyn FnMut(SampleEvent) + Send>`. Use
`thiserror` for `BackendError`. `SampleEvent` and `BackendError` need
`serde` derives only where they cross the wire; `BackendError` does not need
serde, `SampleEvent` does NOT need to be serialized (the stream layer serializes
the inner `PenSample`/`DeviceCapabilities`/proximity separately), but derive
`Debug, Clone` on `SampleEvent`.

`SampleEvent` variants (§8.1):
`Capabilities(DeviceCapabilities)`, `Sample(PenSample)`,
`Proximity { in_range: bool, tool_serial: u64 }`.

`BackendError` variants (§10.1): `DriverMissing`, `NoDevice`,
`ContextOpenFailed`, `UnsupportedField`, `Transport`. Give each an actionable
`#[error("...")]` message; `DriverMissing` must mention installing the Wacom
driver, `Transport` should carry a source/string.

**Files:**
- MODIFY: `crates/tablet-core/Cargo.toml` (add `thiserror`)
- CREATE: `crates/tablet-core/src/backend.rs` (`TabletBackend`, `SampleEvent`)
- CREATE: `crates/tablet-core/src/error.rs` (`BackendError`)
- MODIFY: `crates/tablet-core/src/lib.rs` (declare + re-export)

**Steps:**
1. `cargo add thiserror -p tablet-core`.
2. Implement `BackendError` with `thiserror::Error` derive and the five variants;
   write actionable messages per §10.1.
3. Implement `SampleEvent` enum (derive `Debug, Clone`).
4. Implement `TabletBackend` trait with `capabilities`, `start`, `stop` exactly
   as in §8.1 (trait is `Send`). Methods return `Result<_, BackendError>`.
5. Re-export from `lib.rs`.

**Acceptance Criteria:**
- [ ] `cargo build -p tablet-core` succeeds.
- [ ] `TabletBackend`, `SampleEvent`, `BackendError` are public and match §8.1/§10.1.
- [ ] `BackendError` implements `std::error::Error` (via `thiserror`) and `Display`.
- [ ] A throwaway struct can `impl TabletBackend` and compile.

---

### T1.4 — Normalization & tilt-derivation math + unit tests
Status: DONE
Depends on: T1.2

**Context:**
Per §12, `tablet-core` owns the pure math: raw→normalized position/pressure and
tilt (`tilt_x_deg`/`tilt_y_deg`) derived from `azimuth`/`altitude`
(`PK_ORIENTATION`, units are 0.1°, see §5.1 / §6.2). This logic must be testable
on any OS with no hardware. Normalization maps a raw value over an axis extent to
`[0.0, 1.0]` using `AxisInfo`/extent min..max.

Tilt derivation (standard Wintab orientation → tilt): altitude is the angle from
the tablet plane, azimuth is the compass angle. With `alt` and `az` in radians:
`tilt_x = atan(cos(az) / tan(alt))` and `tilt_y = atan(sin(az) / tan(alt))`,
converted to degrees. Treat `altitude == 90°` (pen vertical) as `tilt_x = tilt_y
= 0`. Inputs arrive in deci-degrees (0.1°), so divide by 10 before converting to
radians. Document the formula in a doc comment.

**Files:**
- CREATE: `crates/tablet-core/src/normalize.rs` (functions + `#[cfg(test)]` tests)
- MODIFY: `crates/tablet-core/src/lib.rs` (declare + re-export)

**Steps:**
1. Implement `pub fn normalize(raw: i64, min: i64, max: i64) -> f64` returning a
   clamped `[0.0, 1.0]` value; handle `max == min` (return 0.0) safely.
2. Implement `pub fn tilt_from_orientation(azimuth_deci_deg: i32,
   altitude_deci_deg: i32) -> (f64, f64)` returning `(tilt_x_deg, tilt_y_deg)`
   using the formula above.
3. Add `#[cfg(test)]` unit tests: normalize endpoints (min→0.0, max→1.0,
   midpoint→0.5, out-of-range clamps); tilt with altitude=900 (90.0°) → (0,0);
   tilt with a known azimuth/altitude pair → expected degrees within `1e-6`.
4. Re-export the functions from `lib.rs`.

**Acceptance Criteria:**
- [ ] `cargo test -p tablet-core normalize` passes.
- [ ] `normalize` never returns values outside `[0.0, 1.0]` and never panics on
      `max == min`.
- [ ] Vertical-pen case yields zero tilt on both axes.

---

# Sprint 2 — Streaming / IPC Layer & Mock Backend
Branch: `sprint-2-stream-mock`
Status: DONE

## Tasks

### T2.1 — Wire message types & serialization (postcard + JSON)
Status: DONE
Depends on: T1.2, T1.3

**Context:**
`tablet-stream` (§7) defines wire formats. Two formats (§7.2): `postcard` binary
(default, compact, allocation-light) and JSONL (debug, one JSON object per line).
Define a `WireMessage`/`Frame` model covering the five frame kinds (§7.3):
`Capabilities` (0x01, `DeviceCapabilities`), `Sample` (0x02, `PenSample`),
`ProximityEvent` (0x03, `{ in_range: bool, tool_serial: u64 }`), `Metrics`
(0x04, periodic — define a `Metrics` struct: `packets_per_sec: f64`,
`dropped: u64`, `queue_depth: u32`, `actual_rate_hz: u32`,
`requested_rate_hz: u32`, `connected_clients: u32`), `Heartbeat` (0x05, empty).
Reuse `tablet_core::{PenSample, DeviceCapabilities}`. Add a `Format` enum
(`Postcard`, `Json`) and a `WireFormat` value `0=postcard`, `1=json` (§7.3 header
byte).

**Files:**
- MODIFY: `crates/tablet-stream/Cargo.toml` (path dep `tablet-core`; add `serde`,
  `postcard` (with `alloc`/`use-std` as needed), `serde_json`, `thiserror`)
- CREATE: `crates/tablet-stream/src/message.rs` (`StreamMessage` enum, `Metrics`,
  `Format`, frame-kind constants)
- CREATE: `crates/tablet-stream/src/codec.rs` (encode/decode payloads in each format)
- MODIFY: `crates/tablet-stream/src/lib.rs` (declare + re-export)

**Steps:**
1. Add deps: `cargo add tablet-core --path ../tablet-core -p tablet-stream`,
   then `serde --features derive`, `postcard`, `serde_json`, `thiserror`.
2. Define frame-kind `u8` constants (0x01..0x05) and a `StreamMessage` enum with
   one variant per kind, plus the `Metrics` struct and `Format` enum.
3. In `codec.rs`, implement `encode_payload(msg, format) -> Vec<u8>` and
   `decode_payload(kind, bytes, format) -> Result<StreamMessage, StreamError>`
   using postcard or serde_json based on `Format`.
4. Add round-trip `#[cfg(test)]` tests for `PenSample`, `DeviceCapabilities`, and
   `Metrics` in both formats.

**Acceptance Criteria:**
- [ ] `cargo test -p tablet-stream` passes serialization round-trips for both formats.
- [ ] Frame-kind constants match §7.3 (0x01–0x05).
- [ ] `Metrics` carries packets/s, dropped, queue depth, actual+requested rate,
      connected clients (§10.2).

---

### T2.2 — Framing & versioned handshake
Status: DONE
Depends on: T2.1

**Context:**
Implement the on-wire framing/handshake from §7.3. Stream header:
`[ MAGIC "WCAP" (4 bytes) ][ u16 protocol_version (LE) ][ u8 format: 0=postcard
1=json ]`. Then a sequence of frames: `[ u32 LE payload_len ][ u8 kind ][
payload bytes ]`. Define `PROTOCOL_VERSION: u16 = 1`. JSONL mode (§7.3) omits
binary framing entirely and writes one JSON object per line (newline-delimited),
including a `"kind"` discriminator string (e.g. `{"kind":"sample", ...}`). Build
on `codec.rs` payload encoding from T2.1.

**Files:**
- CREATE: `crates/tablet-stream/src/framing.rs` (header + frame writer/reader,
  JSONL writer)
- CREATE: `crates/tablet-stream/src/error.rs` (`StreamError`: bad magic, version
  mismatch, truncated frame, decode error, io error — derive via `thiserror`)
- MODIFY: `crates/tablet-stream/src/lib.rs` (declare + re-export)

**Steps:**
1. Define `MAGIC: [u8;4] = *b"WCAP"` and `PROTOCOL_VERSION`.
2. Implement a `FrameWriter<W: Write>` that writes the header once, then
   `write_message(&StreamMessage)`: postcard mode emits `len|kind|payload`; JSON
   mode emits a single JSON line with a `kind` tag and newline.
3. Implement a `FrameReader<R: Read>` that validates magic+version, reads the
   format byte, then yields decoded `StreamMessage`s (binary mode). (JSONL read
   path may be a simple line-decoder used by tests/consumer.)
4. Implement `StreamError` and use it throughout.
5. Add `#[cfg(test)]` tests: write header+several frames to a `Vec<u8>`, read
   them back, assert equality and ordering; assert bad magic / version mismatch
   produce typed errors.

**Acceptance Criteria:**
- [ ] Header bytes exactly match §7.3 layout (`WCAP`, u16 LE version, u8 format).
- [ ] Frame layout is `u32 LE len | u8 kind | payload`; round-trips in tests.
- [ ] JSONL mode produces one newline-terminated JSON object per message with a
      `kind` field.
- [ ] Corrupt magic and wrong version surface typed `StreamError`s, not panics.

---

### T2.3 — Transports: stdout, TCP, named pipe
Status: DONE
Depends on: T2.2

**Context:**
Implement the three selectable transports (§7.1). Each must hand the streaming
layer a `Write` sink to which the header+frames are written. (1) **stdout** —
length-prefixed binary frames to `io::stdout` (default). (2) **TCP** — bind
`127.0.0.1:<port>` (default `9123`, §9), accept clients; **each connected client
gets the handshake then the live stream** (§7.1). (3) **Windows named pipe** —
`\\.\pipe\wacom-capture` (default name `wacom-capture`, §7.1/§9), local IPC,
`cfg(windows)` only. Network binds **localhost only** by default (§14). Define a
`Transport` trait/enum exposing a way to obtain writer(s) and a non-blocking
accept loop for multi-client transports. Backpressure (§7.4): transport writes
happen only on the streaming thread; this task provides the sinks, not the ring.

**Files:**
- MODIFY: `crates/tablet-stream/Cargo.toml` (add `windows`/`windows-sys` under
  `[target.'cfg(windows)'.dependencies]` for named pipe; std suffices for TCP/stdout)
- CREATE: `crates/tablet-stream/src/transport/mod.rs` (`Transport` abstraction)
- CREATE: `crates/tablet-stream/src/transport/stdout.rs`
- CREATE: `crates/tablet-stream/src/transport/tcp.rs`
- CREATE: `crates/tablet-stream/src/transport/pipe.rs` (`cfg(windows)`)
- MODIFY: `crates/tablet-stream/src/lib.rs`

**Steps:**
1. Define a transport abstraction returning client writer(s); TCP/pipe support
   multiple clients (accept loop), stdout is single-sink.
2. Implement stdout transport over a locked `io::Stdout`.
3. Implement TCP transport: `TcpListener::bind("127.0.0.1:<port>")`, accept in a
   loop, track connected clients; each new client receives header first.
4. Implement named-pipe transport (`cfg(windows)`) using `windows`/`windows-sys`
   `CreateNamedPipe`/`ConnectNamedPipe` on `\\.\pipe\<name>`; expose the pipe
   handle as a `Write`.
5. Provide a connected-client count accessor (for `Metrics.connected_clients`).
6. Add a test: TCP transport on an ephemeral port, connect a `TcpStream`, write
   header+one frame, read it back with `FrameReader` and assert integrity.

**Acceptance Criteria:**
- [ ] TCP binds to `127.0.0.1` only by default; a test client receives a valid
      handshake + frame.
- [ ] stdout transport writes valid framed bytes consumable by `FrameReader`.
- [ ] Named-pipe module compiles under `cfg(windows)` and is excluded elsewhere.
- [ ] Connected-client count is queryable.

---

### T2.4 — Mock `TabletBackend`
Status: DONE
Depends on: T1.3

**Context:**
Per §12, a mock backend synthesizes deterministic `PenSample` streams so
end-to-end transport/framing/loss tests run on any OS without hardware. It
implements `tablet_core::TabletBackend` (§8.1): `capabilities()` returns a fixed
`DeviceCapabilities`; `start(sink)` spawns a thread that emits a
`SampleEvent::Capabilities` first, then a deterministic sequence of
`SampleEvent::Sample` with monotonically increasing `serial` and `t_capture_ns`
(e.g. a parametric stroke); `stop()` joins the thread. Must support a
configurable count/rate and an **optional serial-gap injection** (skip serials)
so loss/gap tests (T4.4) can assert drop detection.

**Files:**
- MODIFY: `crates/tablet-stream/Cargo.toml` (ensure `tablet-core` path dep present)
- CREATE: `crates/tablet-stream/src/mock.rs` (`MockBackend`, builder/config)
- MODIFY: `crates/tablet-stream/src/lib.rs` (re-export behind a `mock` feature or always)

**Steps:**
1. Define `MockBackend` with config: sample count (or run-until-stop), nominal
   rate, optional `gap_every: Option<u32>` to skip serials.
2. Implement `capabilities()` returning a plausible fixed descriptor (sane axis
   ranges, `max_packet_rate_hz`, `queue_size`).
3. Implement `start(sink)`: emit `Capabilities` then `Sample`s on a spawned
   thread; advance `serial` (skip when gap injection fires) and `t_capture_ns`.
4. Implement `stop()` to signal and join the thread.
5. Add a `#[cfg(test)]` test that collects emitted events into a `Vec` via the
   sink and asserts ordering (Capabilities first) and serial progression.

**Acceptance Criteria:**
- [ ] `MockBackend` implements `tablet_core::TabletBackend` and compiles on any OS.
- [ ] First event is `Capabilities`; subsequent events are `Sample`s with
      increasing `serial`/`t_capture_ns`.
- [ ] Gap injection produces detectable serial discontinuities.
- [ ] `stop()` cleanly joins the producer thread.

---

### T2.5 — End-to-end transport/framing integration tests
Status: DONE
Depends on: T2.3, T2.4

**Context:**
Per §12, drive the mock backend through the real serializer/framing/transport
and assert handshake + frame integrity + ordering. This validates the full
streaming path without the ring/CLI (those arrive in Sprint 4). Use
`MockBackend` (T2.4) → encode via `codec`/`framing` (T2.1/T2.2) → write through a
transport (T2.3) → read back with `FrameReader`.

**Files:**
- CREATE: `crates/tablet-stream/tests/integration.rs`

**Steps:**
1. Test A (stdout-style sink): write `MockBackend` output through a
   `FrameWriter` into an in-memory `Vec<u8>`; read back with `FrameReader`;
   assert first message is `Capabilities`, samples are in serial order, count
   matches.
2. Test B (TCP): start the TCP transport on an ephemeral port, connect a client,
   stream a bounded mock run; on the client decode header + frames and assert
   integrity/ordering.
3. Test C (both formats): repeat Test A in JSONL mode, parse lines as JSON,
   assert `kind` discriminators and field presence.

**Acceptance Criteria:**
- [ ] `cargo test -p tablet-stream --test integration` passes on a non-Windows CI
      host (TCP + in-memory paths; pipe test may be `cfg(windows)`).
- [ ] Handshake header is validated by the reader in every test.
- [ ] Sample ordering and count are asserted in both postcard and JSON modes.

---

# Sprint 3 — Wintab Backend (Windows)
Branch: `sprint-3-wintab-backend`
Status: DONE

## Tasks

### T3.1 — Wintab DLL loading & capability query (`WTInfo`)
Status: DONE
Depends on: T1.2, T1.3

**Context:**
`tablet-wintab` is `cfg(windows)` only (§4.1) and isolates all `unsafe` FFI
behind safe wrappers (§14). Initialization sequence §6.1: (1) load
`Wintab32.dll` via `libloading` — if absent, return
`BackendError::DriverMissing` (§10.1) with guidance to install the Wacom driver;
(2) verify the interface is live: `WTInfo(0,0,NULL)` returns non-zero; (3) query
devices + axis ranges via `WTInfo(WTI_DEVICES, ...)` and
`WTInfo(WTI_DEFCONTEXT/WTI_DEFSYSCTX, ...)` to build
`tablet_core::DeviceCapabilities` (§5.2). Use the `wintab_lite` crate for Wintab
types (§3.1/§13); fall back to local FFI declarations only for symbols it
lacks. Axis `min/max/resolution/unit` come from `WTInfo` axis structures; map
Wintab units to `tablet_core::AxisUnit`. This task does NOT open a context or
start capture (that's T3.3).

**Files:**
- MODIFY: `crates/tablet-wintab/Cargo.toml` (add `tablet-core` path dep,
  `wintab_lite`, `libloading`, `windows`/`windows-sys`, `thiserror`, `tracing`)
- CREATE: `crates/tablet-wintab/src/ffi.rs` (loaded-DLL wrapper: function pointers
  for `WTInfoA/W`, `WTOpenA/W`, `WTClose`, `WTPacketsGet`, `WTQueueSizeSet`, etc.)
- CREATE: `crates/tablet-wintab/src/caps.rs` (build `DeviceCapabilities` from `WTInfo`)
- MODIFY: `crates/tablet-wintab/src/lib.rs`

**Steps:**
1. Add deps via `cargo add` (latest stable). Gate the crate body with
   `#![cfg(windows)]` or per-item `cfg`.
2. In `ffi.rs`, load `Wintab32.dll` with `libloading::Library::new`; resolve the
   needed entry points into a `WintabApi` struct of function pointers. Map a
   load failure to `BackendError::DriverMissing`.
3. Implement `is_interface_live()` calling `WTInfo(0,0,null)` and checking
   non-zero.
4. In `caps.rs`, query device name, driver version, each axis (X/Y/Z, normal
   pressure, tangent pressure, azimuth/altitude/twist, rotation), and packet rate
   via `WTInfo`; populate `DeviceCapabilities` with `supported` flags for absent
   axes. Map units to `AxisUnit`.
5. Wrap all `unsafe` in documented safe functions; no `unsafe` leaks into the
   public API.

**Acceptance Criteria:**
- [ ] `cargo build -p tablet-wintab` succeeds on Windows; crate is empty/no-op on
      non-Windows.
- [ ] Missing `Wintab32.dll` yields `BackendError::DriverMissing` with an
      install-the-driver message (testable by pointing the loader at a bad name).
- [ ] `capabilities()` populates every `AxisInfo` with `supported` correctly set.
- [ ] No `unsafe` appears in the crate's public function signatures.

---

### T3.2 — `LOGCONTEXT` full-resolution configuration
Status: DONE
Depends on: T3.1

**Context:**
The easy-to-get-wrong step (§6.3, and CLAUDE.md "Two design points"). Build a
`LOGCONTEXT` from the default context (`WTI_DEFCONTEXT`) and override:
- `lcPktData` = full `WTPKT` superset (§6.2 table: `PK_CONTEXT|PK_STATUS|PK_TIME|
  PK_CHANGED|PK_SERIAL_NUMBER|PK_CURSOR|PK_BUTTONS|PK_X|PK_Y|PK_Z|
  PK_NORMAL_PRESSURE|PK_TANGENT_PRESSURE|PK_ORIENTATION|PK_ROTATION`).
- `lcPktMode = 0` (all axes absolute). `lcMoveMask = lcPktData`.
  `lcBtnDnMask = 0xFFFF`, `lcBtnUpMask = 0xFFFF`.
- `lcOptions &= ~CXO_SYSTEM` (do NOT move system cursor); `lcOptions |=
  CXO_MESSAGES` (deliver `WT_PACKET` to our window).
- **1:1 mapping**: `lcInOrgX/Y = 0`; `lcInExtX = device.x.max`,
  `lcInExtY = device.y.max`; `lcOutOrgX/Y = 0`; `lcOutExtX/Y = device.x.max /
  device.y.max` (input extent == output extent, no screen scaling).
- `lcPktRate = device.max_packet_rate_hz` (request maximum).
This task produces the configured `LOGCONTEXT` struct (and the WTPKT mask
constant); `WTOpen` and re-reading negotiated `lcPktRate` happen in T3.3.

**Files:**
- CREATE: `crates/tablet-wintab/src/context.rs` (`build_logcontext(caps) ->
  LOGCONTEXT`, WTPKT mask const, packet-field flag constants)
- MODIFY: `crates/tablet-wintab/src/lib.rs`

**Steps:**
1. Define the `WTPKT` flag constants from §6.2 and a `FULL_PKT_DATA` mask = OR of
   all of them.
2. Define `CXO_SYSTEM`, `CXO_MESSAGES` constants (from `wintab_lite` if present).
3. Implement `build_logcontext(&DeviceCapabilities) -> LOGCONTEXT`: start from the
   default context, then apply every override in the Context block above.
4. Add a `#[cfg(all(test, windows))]` test asserting the configured struct fields:
   `lcInExtX == lcOutExtX`, `lcInExtY == lcOutExtY`, `CXO_SYSTEM` cleared,
   `CXO_MESSAGES` set, `lcPktData == FULL_PKT_DATA`, `lcPktMode == 0`.

**Acceptance Criteria:**
- [ ] Input extent equals output extent on both axes (1:1, no scaling).
- [ ] `CXO_SYSTEM` is cleared and `CXO_MESSAGES` is set in `lcOptions`.
- [ ] `lcPktData` equals the full superset mask from §6.2.
- [ ] Test asserts these invariants without needing hardware.

---

### T3.3 — Capture thread, message-only window & lossless drain
Status: DONE
Depends on: T3.2

**Context:**
The hot path (§4.3, §6.4, §6.5, §11). Implement the capture thread that owns the
Wintab context and a **message-only window** (`HWND_MESSAGE`). Sequence: create
the hidden window → `WTOpen(hWnd, &ctx, TRUE)` (null ⇒
`BackendError::ContextOpenFailed`) → grow the queue with `WTQueueSizeSet` (try
1024, back off until accepted; §6.4) → re-read `lcPktRate` to learn the **actual
negotiated rate** and record it in caps (§6.3) → run the Win32 message loop. On
each `WT_PACKET`, **drain ALL pending packets** with `WTPacketsGet` into a
fixed pre-sized buffer in a loop until empty (§6.4, §11) — **no I/O, no heap
allocation on this path**. Capture a `QueryPerformanceCounter` nanosecond
timestamp at drain time (§11) for `t_capture_ns`. Decode (T3.4) each raw packet
to `PenSample` and invoke the `sink` (a cheap ring push). Handle `WT_PROXIMITY`
→ emit `SampleEvent::Proximity`. Detect drops via `PK_SERIAL_NUMBER` continuity
and count them (surface in metrics later). Fallback polling mode (§6.5) is
optional/secondary.

**Files:**
- MODIFY: `crates/tablet-wintab/Cargo.toml` (ensure `windows`/`windows-sys` has
  features for window/message/QPC)
- CREATE: `crates/tablet-wintab/src/window.rs` (create/destroy message-only window)
- CREATE: `crates/tablet-wintab/src/capture.rs` (capture thread + drain loop)
- CREATE: `crates/tablet-wintab/src/backend.rs` (`WintabBackend: TabletBackend`)
- MODIFY: `crates/tablet-wintab/src/lib.rs`

**Steps:**
1. Implement message-only window creation (`HWND_MESSAGE` parent) and a WndProc
   that handles `WT_PACKET`, `WT_PROXIMITY`, `WT_INFOCHANGE`, `WT_CTXOPEN/CLOSE`.
2. Implement `WintabBackend` implementing `TabletBackend`: `capabilities()`
   delegates to T3.1; `start(sink)` spawns the capture thread; `stop()` posts a
   quit message, `WTClose`es, destroys the window, and joins.
3. In the capture thread: open context, `WTQueueSizeSet` with backoff, re-read
   negotiated `lcPktRate`, then pump messages.
4. On `WT_PACKET`: loop `WTPacketsGet(hCtx, BUF_N, &mut buf)` into a fixed-size
   `[PACKET; N]` until it returns 0; for each, capture QPC ns, call decode
   (T3.4), push via `sink`. Track last serial; on a gap, increment a drop
   counter.
5. Add a QPC helper (`QueryPerformanceCounter`/`QueryPerformanceFrequency` →
   monotonic ns).
6. Ensure the drain loop performs no allocation (pre-sized buffer) and no I/O.

**Acceptance Criteria:**
- [ ] `WTOpen` returning null maps to `BackendError::ContextOpenFailed`.
- [ ] Queue size set via `WTQueueSizeSet` with backoff; the negotiated value and
      actual `lcPktRate` are stored in `DeviceCapabilities`.
- [ ] `WT_PACKET` handler drains the queue fully in a loop into a fixed buffer.
- [ ] Serial-number gaps increment a drop counter.
- [ ] No heap allocation or I/O occurs inside the drain loop (reviewable).

---

### T3.4 — Packet decode to `PenSample`
Status: DONE
Depends on: T3.1

**Context:**
Pure decode from a Wintab `PACKET` to `tablet_core::PenSample` (§5.1), driven by
which fields are present (`lcPktData`/caps). Fields map as: `PK_X/Y/Z`→
`x_raw/y_raw/z_raw`; normalized `x_norm/y_norm` via
`tablet_core::normalize(raw, axis.min, axis.max)`; `PK_NORMAL_PRESSURE`→
`pressure_raw`(+`pressure_norm`); `PK_TANGENT_PRESSURE`→
`tangent_pressure_raw`; `PK_ORIENTATION`→ azimuth/altitude/twist (deci-deg) plus
derived `tilt_x_deg`/`tilt_y_deg` via `tablet_core::tilt_from_orientation`;
`PK_ROTATION`→ `rotation_deci_deg`; `PK_BUTTONS`→ `buttons`; `PK_STATUS`→
`status` + `in_proximity`; `PK_TIME`→ `t_device_ms`; `PK_SERIAL_NUMBER`→
`serial`; `PK_CURSOR`→ `tool`/`tool_serial` (map cursor to
`ToolKind::{Pen,Eraser,Airbrush,Cursor,Unknown}`). `t_capture_ns` is passed in
from the capture thread (T3.3). Optional Y-flip (bottom-left→top-left, §6.3)
controlled by a bool parameter. Unsupported fields → `None`/defaults. This is
pure and unit-testable from synthetic `PACKET` values.

**Files:**
- CREATE: `crates/tablet-wintab/src/decode.rs` (`decode_packet(packet, &caps,
  t_capture_ns, flip_y) -> PenSample`, cursor→ToolKind mapping)
- MODIFY: `crates/tablet-wintab/src/lib.rs`

**Steps:**
1. Implement `decode_packet(...)` mapping every field per the table above, using
   `tablet_core::normalize` and `tilt_from_orientation`.
2. Implement cursor→`ToolKind` mapping (inverted/eraser via `PK_STATUS` bits and
   cursor type).
3. Apply optional Y-flip: `y_raw = device.y.max - y_raw` when `flip_y`.
4. Set `Option` fields to `None` when the corresponding axis is unsupported.
5. Add `#[cfg(test)]` tests with synthetic packet values asserting raw passthrough,
   normalization endpoints, tilt derivation, and Y-flip behavior (these run on any
   OS since decode is pure — keep the `PACKET` shape locally defined or feature it
   so tests aren't `cfg(windows)`-gated if practical).

**Acceptance Criteria:**
- [ ] Every `PenSample` field is populated from the corresponding packet field.
- [ ] Normalized values use `tablet-core` math; tilt uses `tilt_from_orientation`.
- [ ] Y-flip toggles correctly and unsupported axes decode to `None`.
- [ ] Decode unit tests pass.

---

### T3.5 — Lifecycle: hot-plug & proximity events
Status: DONE
Depends on: T3.3

**Context:**
Lifecycle/hot-plug handling (§6.6). `WT_INFOCHANGE` → re-query capabilities
(T3.1), re-emit the handshake (`SampleEvent::Capabilities`), and reopen the
context if needed. `WT_PROXIMITY` → emit `SampleEvent::Proximity { in_range,
tool_serial }`. On shutdown: `WTClose(hCtx)`, destroy the window, join threads
(coordinate with T3.3's `stop()`). This task extends the WndProc/capture thread
from T3.3 rather than rebuilding it.

**Files:**
- MODIFY: `crates/tablet-wintab/src/capture.rs` (handle `WT_INFOCHANGE`,
  `WT_PROXIMITY`; clean shutdown)
- MODIFY: `crates/tablet-wintab/src/backend.rs` (re-emit capabilities path)

**Steps:**
1. On `WT_INFOCHANGE`: rebuild `DeviceCapabilities`, emit
   `SampleEvent::Capabilities` via the sink, and reopen the context if extents
   changed.
2. On `WT_PROXIMITY`: decode in/out range + tool serial and emit
   `SampleEvent::Proximity`.
3. Ensure `stop()` performs `WTClose` → destroy window → join with no leaks or
   hangs.

**Acceptance Criteria:**
- [ ] `WT_INFOCHANGE` re-emits a fresh `Capabilities` handshake.
- [ ] `WT_PROXIMITY` emits `SampleEvent::Proximity` with correct `in_range`/serial.
- [ ] Shutdown closes the context, destroys the window, and joins the thread cleanly.

---

# Sprint 4 — CLI, Integration, Hardening & Stubs
Branch: `sprint-4-cli-integration`
Status: DONE

## Tasks

### T4.1 — CLI args & TOML config
Status: DONE
Depends on: T2.1

**Context:**
`tablet-cli` config (§9): TOML file with CLI overrides (**CLI wins**), parsed
with `clap`. Config schema (§9):
```toml
[capture]   requested_rate_hz=200, queue_size=1024, flip_y=true,
            fields=["x","y","pressure","tilt","rotation","tangent","buttons"]
[output]    transport="stdout"|"tcp"|"pipe", format="postcard"|"json",
            tcp_addr="127.0.0.1:9123", pipe_name="wacom-capture"
[telemetry] metrics_interval_ms=1000, log_level="info"
```
CLI examples (§9): `--transport tcp --format json`, `--transport stdout`.
Invalid config must be **rejected at startup** with actionable errors (§10.1).
Reuse `tablet_stream::Format`. Provide a merged, validated `Config` struct.

**Files:**
- MODIFY: `crates/tablet-cli/Cargo.toml` (add `clap` (derive), `serde`, `toml`,
  `tablet-core` + `tablet-stream` path deps, `thiserror`)
- CREATE: `crates/tablet-cli/src/config.rs` (`Config` + sub-structs, defaults,
  TOML load, CLI-override merge, validation)
- CREATE: `crates/tablet-cli/src/cli.rs` (`clap` `Args`)
- MODIFY: `crates/tablet-cli/src/main.rs` (parse args → load+merge config → print)

**Steps:**
1. Add deps via `cargo add`.
2. Define `Config` with `[capture]/[output]/[telemetry]` sub-structs, all with
   `#[serde(default)]` and sensible defaults matching §9.
3. Define `clap` args mirroring the override-able fields (`--transport`,
   `--format`, `--tcp-addr`, `--pipe-name`, `--config <path>`, `--queue-size`,
   `--requested-rate-hz`, `--flip-y`, `--log-level`).
4. Implement load: read TOML if `--config` (or a default path) exists, then apply
   CLI overrides (CLI wins). Validate enums (transport/format/log level) and
   ranges; reject invalid config with a typed error and non-zero exit.
5. Have `main` print the resolved config for now (wired to runtime in T4.2).

**Acceptance Criteria:**
- [ ] TOML loads and CLI flags override file values (CLI wins) — covered by a test.
- [ ] Invalid transport/format/log-level is rejected at startup with a clear message.
- [ ] Defaults match §9 when no file/flags are given.

---

### T4.2 — Runtime wiring: backend → ring → streaming thread
Status: DONE
Depends on: T2.3, T2.4, T4.1

**Context:**
The integration spine (§4.2, §4.3, §7.4). Wire a `TabletBackend` (mock by
default; `WintabBackend` under `cfg(windows)` + `backend-wintab`) into the
streaming pipeline using the three-thread model: **capture thread** (owned by the
backend) pushes `SampleEvent`s; **streaming thread** pops, serializes, frames,
writes to the transport; **main thread** handles lifecycle/signals. The
capture→stream handoff is a **bounded SPSC ring** (`rtrb` or `crossbeam`) with
**drop-oldest** overflow and a `dropped` counter (§4.3, §11). The backend's
`sink` closure pushes into the ring (cheap, non-blocking — never block the
capture thread, §4.3/§7.4). The streaming thread sends the `Capabilities`
handshake first (per client for multi-client transports), then samples. Selection
of backend/transport/format comes from `Config` (T4.1).

**Files:**
- MODIFY: `crates/tablet-cli/Cargo.toml` (add `rtrb` or `crossbeam`; `tablet-wintab`
  path dep under `[target.'cfg(windows)'.dependencies]`; `tracing`)
- CREATE: `crates/tablet-cli/src/runtime.rs` (build ring, spawn streaming thread,
  start backend, handle shutdown/signals)
- MODIFY: `crates/tablet-cli/src/main.rs` (call into runtime after config resolve)

**Steps:**
1. Add deps; choose `rtrb` (SPSC) for the ring.
2. Build the bounded ring sized from `config.capture.queue_size`. The producer
   handle is moved into the backend `sink`; on full ring, drop the oldest and
   increment a shared `dropped` counter (never block).
3. Spawn the streaming thread: open the selected transport (T2.3), write the
   header, send `Capabilities` then drain the ring → encode (T2.1) → frame
   (T2.2) → write. Honor `format`.
4. Select the backend: `MockBackend` by default; `WintabBackend` when on Windows
   with `backend-wintab` enabled. Call `backend.start(sink)`.
5. Handle Ctrl-C / shutdown: stop the backend, flush, join the streaming thread.

**Acceptance Criteria:**
- [ ] `cargo run -p tablet-cli -- --transport stdout` (mock backend) emits a valid
      framed stream (header + Capabilities + Samples) consumable by `FrameReader`.
- [ ] Ring overflow drops oldest and increments `dropped`; capture never blocks.
- [ ] Backend/transport/format are chosen from config.
- [ ] Ctrl-C shuts down cleanly (backend stopped, threads joined).

---

### T4.3 — Metrics & structured logging
Status: DONE
Depends on: T4.2

**Context:**
Observability (§10.2, §2.2). Use `tracing` + `tracing-subscriber` for structured,
leveled logs (level from `config.telemetry.log_level`). Emit a periodic `Metrics`
frame (kind 0x04, the struct from T2.1) every `metrics_interval_ms` (§9) on the
streaming thread AND a matching log line: **packets/s, dropped count, queue
depth, actual vs requested rate, connected clients** (§10.2). Serial-number gap
detection (from T3.3 drop counter, or mock gap injection) logs hardware/queue
loss. Counters (`dropped`, packets processed, queue depth) are shared atomics
updated by the capture/stream threads and sampled by the metrics timer.

**Files:**
- MODIFY: `crates/tablet-cli/Cargo.toml` (add `tracing`, `tracing-subscriber`)
- CREATE: `crates/tablet-cli/src/metrics.rs` (shared atomic counters + periodic
  emitter)
- MODIFY: `crates/tablet-cli/src/runtime.rs` (wire counters; spawn metrics timer)
- MODIFY: `crates/tablet-cli/src/main.rs` (init `tracing-subscriber` from log_level)

**Steps:**
1. Init `tracing-subscriber` honoring `config.telemetry.log_level`.
2. Define shared atomic counters (packets processed, `dropped`, current queue
   depth) updated by capture (`dropped`) and streaming (processed) paths.
3. Spawn a periodic task/thread on `metrics_interval_ms` that computes packets/s,
   reads queue depth, actual+requested rate (from caps/config) and connected
   clients (from transport), then writes a `Metrics` frame and a `tracing` line.
4. Log a warning on any detected serial-number gap (drop) with the gap size.

**Acceptance Criteria:**
- [ ] A `Metrics` frame (kind 0x04) is emitted every `metrics_interval_ms`.
- [ ] Metrics include packets/s, dropped, queue depth, actual+requested rate,
      connected clients (§10.2).
- [ ] Log level is driven by config; serial gaps produce warning logs.

---

### T4.4 — Loss/gap tests & reference consumer
Status: DONE
Depends on: T4.2, T4.3, T2.4

**Context:**
Per §12: loss/gap tests inject serial gaps (via `MockBackend` gap injection,
T2.4) and assert metrics reporting; provide a small **reference consumer** that
decodes the stream and prints/plots samples for verification (§12). The consumer
can be a Rust example binary using `tablet_stream::FrameReader`, or a documented
Python script — prefer a Rust `examples/` reader for reuse of the codec.

**Files:**
- CREATE: `crates/tablet-cli/tests/loss_gap.rs` (drive mock w/ gaps; assert
  `dropped`/metrics)
- CREATE: `crates/tablet-stream/examples/consumer.rs` (connect/read stdin or TCP,
  decode header + frames, print samples; flag for plotting/CSV optional)
- CREATE: `docs/consumer.md` (how to run the consumer against each transport)

**Steps:**
1. Write a test that runs the pipeline with `MockBackend` gap injection and
   asserts the `dropped`/gap metric reflects the injected gaps.
2. Implement `examples/consumer.rs`: read a framed stream (stdin for stdout
   transport, or TCP connect), validate the handshake, decode and print
   `Capabilities`/`Sample`/`Metrics` (CSV or pretty).
3. Document usage in `docs/consumer.md`:
   `cargo run -p tablet-cli -- --transport stdout | cargo run -p tablet-stream --example consumer`
   and the TCP variant.

**Acceptance Criteria:**
- [ ] Injected serial gaps are reflected in reported drop metrics (test passes).
- [ ] The consumer decodes a live stream and prints samples for both transports.
- [ ] `docs/consumer.md` shows runnable commands for stdout and TCP.

---

### T4.5 — Future backend stubs (evdev, macOS)
Status: DONE
Depends on: T1.3

**Context:**
Portability path (§8.2, §15 M3). Add feature-gated stub backends that implement
`tablet_core::TabletBackend` but return `BackendError` (e.g. a "not implemented"
variant or `NoDevice`) so the workspace compiles with these features and the
trait shape is validated for future work. Features: `backend-evdev` (Linux),
`backend-macos` (macOS). These are stubs only — no real device access.

**Files:**
- CREATE: `crates/tablet-evdev/` (Cargo.toml + `src/lib.rs`, `cfg(target_os="linux")`)
- CREATE: `crates/tablet-macos/` (Cargo.toml + `src/lib.rs`, `cfg(target_os="macos")`)
- MODIFY: root `Cargo.toml` (add members)
- MODIFY: `crates/tablet-cli/Cargo.toml` (wire `backend-evdev`/`backend-macos`
  features to the respective path deps; keep them off by default)

**Steps:**
1. Create `tablet-evdev` and `tablet-macos` crates with a single struct
   implementing `TabletBackend` whose methods return a typed "not implemented"
   error (reuse/extend `BackendError`).
2. Gate each crate's body to its target OS and to its feature flag.
3. Add both as optional workspace members / path deps behind the matching CLI
   features.
4. Confirm `cargo build` (default features) is unaffected and
   `cargo build --features backend-evdev` compiles on Linux.

**Acceptance Criteria:**
- [ ] Both stub crates implement `TabletBackend` and compile under their target OS.
- [ ] Stub methods return a typed error (no panics, no real device access).
- [ ] Default `cargo build` is unchanged; the feature flags exist and are off by default.
```
