# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Status

**Pre-implementation.** The repository currently contains only `SPEC_1.md`, a
complete design reference. There is no Cargo workspace, no source code, and no
git history yet. The first implementation work is to scaffold the workspace
described in the spec. Treat `SPEC_1.md` as the source of truth for design
decisions — when implementing, follow it rather than inventing alternatives, and
update it if a decision changes.

## What This Builds

A Rust application that captures Wacom pen/stylus tablet input at the **highest
native resolution and report rate** and streams the samples over a real-time
IPC interface. Key design priorities, in order: maximum spatial resolution
(native digitizer units, not screen pixels), lossless capture (every hardware
packet, zero drops), full axis fidelity (pressure, tilt, twist, rotation,
tangent pressure), and low added latency (< ~2 ms capture→transport target).

Windows is the only implemented backend for v1, via **Wintab** (the Wacom
`Wintab32.dll` driver). Wintab is chosen over Windows Ink / RealTimeStylus
because it exposes full digitizer resolution and lets the app configure the
logical context, packet contents, report rate, and queue depth directly. The
cost is a hard runtime dependency on the installed Wacom driver, which the app
must detect and surface clearly (`BackendError::DriverMissing`) rather than
crash.

## Planned Architecture

A Cargo **workspace** with four crates, keeping the platform-agnostic core
decoupled from the Windows backend and the transport layer:

- `tablet-core` — Platform-agnostic types and the `TabletBackend` trait
  (`PenSample`, `ToolKind`, `AxisInfo`, `DeviceCapabilities`, `SampleEvent`,
  `BackendError`). **No OS-specific dependencies.** Everything else depends only
  on this crate, so consumers are unaffected by which backend is active.
- `tablet-wintab` — Windows Wintab backend (`cfg(windows)`): FFI, `LOGCONTEXT`
  configuration, capture thread, hidden message-only window, and packet decode
  to `PenSample`. All `unsafe` FFI is isolated here behind safe wrappers.
- `tablet-stream` — Wire formats (postcard binary default, JSONL debug),
  framing/handshake, and transports (stdout, TCP, named pipe).
- `tablet-cli` — The binary: `clap` argument parsing, TOML config (CLI
  overrides win), lifecycle, `tracing` logging, and metrics reporting.

### Data flow and threading (the part that needs the whole picture)

```
Wacom pen → Wintab32 driver → [Capture thread] → ring buffer → [Streaming thread] → transport → consumer
```

Three threads with a strict separation of concerns:

- **Capture thread** owns the Wintab context and message-only window, blocks in
  the Win32 message loop, and on each `WT_PACKET` drains *all* pending packets
  with `WTPacketsGet` in a loop, decodes them to `PenSample`, and pushes to the
  ring. This is the hot path: **no I/O and no heap allocation here.**
- **Streaming thread** pops from the ring, serializes, frames, and writes to the
  transport. It owns all I/O and backpressure.
- **Main thread** handles lifecycle, config, signals, and metrics.

The capture→stream handoff is a **bounded SPSC ring** (`rtrb` or `crossbeam`)
with a **drop-oldest** overflow policy and a `dropped` counter surfaced in
metrics. The capture thread must *never block* — if a consumer stalls, the ring
absorbs the burst and overflow is reported, but capture keeps running.

### Two design points that are easy to get wrong

1. **Full-resolution context mapping.** In the `LOGCONTEXT`, the input extent
   must map **1:1** to the output extent (`lcInExtX/Y == lcOutExtX/Y ==
   device.max` from `WTInfo`), so coordinates stay in native digitizer units
   instead of being scaled to the screen. Also clear `CXO_SYSTEM` (do not move
   the system cursor) and set `CXO_MESSAGES` (deliver `WT_PACKET` to our
   window). After `WTOpen`, re-read `lcPktRate` to learn the *actual* negotiated
   rate. See `SPEC_1.md` §6.3.
2. **Lossless drain.** The default Wintab queue is small — grow it with
   `WTQueueSizeSet` (try ~1024, back off until accepted), and on every
   `WT_PACKET` drain the queue fully rather than one packet at a time. Detect
   drops via `PK_SERIAL_NUMBER` continuity. See `SPEC_1.md` §6.4.

### Data model

`PenSample` carries **both raw device values and normalized convenience values**
so consumers can use either without losing fidelity. `DeviceCapabilities` is a
handshake descriptor sent once at session start (and re-sent on `WT_INFOCHANGE`)
so consumers can interpret raw axis ranges. Full struct definitions are in
`SPEC_1.md` §5.

### Wire protocol

Every stream begins `[ MAGIC "WCAP" ][ u16 version ][ u8 format ]`, then framed
messages: `[ u32 LE payload_len ][ u8 kind ][ payload ]` where kind is
Capabilities / Sample / ProximityEvent / Metrics / Heartbeat. JSONL mode skips
binary framing and emits one JSON object per line. See `SPEC_1.md` §7.3.

## Cross-Platform Path

All backends implement the single `TabletBackend` trait in `tablet-core` and map
their native fields onto the same `PenSample` / `AxisInfo`, so adding a backend
doesn't change consumers. Backends are feature-gated: `backend-wintab` (v1,
implemented), `backend-evdev` (Linux, future stub), `backend-macos` (future
stub). Keep `tablet-core` OS-agnostic — that decoupling is what makes the
portability path work.

## Build & Test (once the workspace exists)

The spec does not pin dependency versions — use `cargo add` for the latest
stable at implementation time (key crates: `wintab_lite`, `libloading`,
`windows`/`windows-sys`, `rtrb`/`crossbeam`, `serde` + `postcard`, `clap`,
`toml`, `tracing`, `thiserror`). Standard Cargo workflow once scaffolded:

```powershell
cargo build
cargo test                          # whole workspace
cargo test -p tablet-core           # one crate
cargo test -p tablet-core normalize # one test by name filter
cargo run -p tablet-cli -- --transport stdout | my-consumer
cargo run -p tablet-cli -- --transport tcp --format json
```

If `bindgen` is used for full Wintab coverage (fallback beyond `wintab_lite`),
it requires `clang` / `LIBCLANG_PATH` at build time and vendored Wacom headers.

### Testing without hardware

Most tests must run on any OS without a tablet. The strategy (`SPEC_1.md` §12):
a **mock `TabletBackend`** that synthesizes deterministic `PenSample` streams
drives end-to-end transport/framing tests; unit tests cover normalization math,
tilt derivation, and serialization round-trips; loss/gap tests inject serial
gaps and assert metrics. Real Wacom hardware on Windows is needed only for the
manual fidelity checklist.
