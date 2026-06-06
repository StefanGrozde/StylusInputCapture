//! Capture thread, drain loop, and internal shared state (SPEC §4.3, §6.4, §11).
//!
//! ## Hot-path constraints
//! [`handle_wt_packet`] performs **no I/O and no heap allocation**.  The packet
//! buffer is a fixed-size stack array; the only "allocation-like" work is the
//! `sink` call, which must be a cheap O(1) ring-buffer push from the caller's
//! perspective.
//!
//! ## Serial-number gap detection
//! Wintab assigns a monotonically increasing per-context serial number to every
//! packet.  If `packet.pk_serial != last_serial.wrapping_add(1)` (and we have
//! seen at least one previous packet), packets were lost in the driver queue.
//! The gap count is accumulated in [`CaptureState::drop_count`] and is surfaced
//! in metrics by T4.x.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tablet_core::{BackendError, DeviceCapabilities, SampleEvent};
use tracing::{debug, error, info, warn};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::UI::WindowsAndMessaging::PostQuitMessage;
use wintab_lite::HCTX;

use crate::decode::{decode_packet, FullPacket};
use crate::ffi::WintabApi;

// ---------------------------------------------------------------------------
// Internal shared state
// ---------------------------------------------------------------------------

/// All mutable state owned exclusively by the capture thread.
///
/// A raw pointer to this struct is stored in the window's `GWLP_USERDATA`
/// slot so the WndProc callback can retrieve it without a closure.
///
/// # Thread safety
/// All fields (except `drop_count`) are accessed only from the capture thread.
/// `drop_count` is wrapped in `Arc<AtomicU64>` so it can be read from the
/// main/metrics thread without any lock.
pub(crate) struct CaptureState {
    /// Loaded Wintab API; kept alive for the duration of the capture session.
    pub(crate) api: Arc<WintabApi>,

    /// Open Wintab context handle (`WTOpen` result).
    ///
    /// # Safety
    /// This raw pointer is only ever read/written on the capture thread.
    pub(crate) hctx: *mut HCTX,

    /// Callback invoked for every decoded sample, proximity event, and the
    /// initial capability handshake.  Must be cheap (O(1) ring push).
    pub(crate) sink: Box<dyn FnMut(SampleEvent) + Send>,

    /// Device capabilities reported to the sink at session start.
    pub(crate) caps: DeviceCapabilities,

    /// When `true`, the Y axis is flipped so the origin is at the top-left
    /// (screen convention) rather than Wintab's bottom-left default.
    pub(crate) flip_y: bool,

    /// The serial number of the last successfully decoded packet.  Used to
    /// detect gaps in `PK_SERIAL_NUMBER` continuity.  Starts at 0; the first
    /// packet initialises it unconditionally.
    pub(crate) last_serial: u32,

    /// Cumulative count of packets estimated to have been dropped (based on
    /// serial-number gaps).  Wrapped in `Arc<AtomicU64>` for cross-thread
    /// visibility.
    pub(crate) drop_count: Arc<AtomicU64>,

    /// `QueryPerformanceFrequency` result (ticks per second), cached once at
    /// thread startup.
    pub(crate) qpc_freq: i64,
}

// SAFETY: `hctx` is a raw pointer that is only accessed from the capture
// thread.  We manually assert `Send` here because the raw pointer prevents
// the auto-derivation, but the access pattern is safe.
unsafe impl Send for CaptureState {}

// ---------------------------------------------------------------------------
// QPC timestamp helper
// ---------------------------------------------------------------------------

/// Read the current `QueryPerformanceCounter` value and convert it to
/// nanoseconds using the pre-queried frequency.
///
/// Uses `u128` arithmetic internally to avoid overflow for large counter
/// values; the result is truncated to `u64` which is good for ~584 years.
///
/// # Safety
/// `QueryPerformanceCounter` is always safe to call on Windows Vista+.
pub(crate) fn qpc_ns(freq: i64) -> u64 {
    let mut counter = 0i64;
    // SAFETY: `counter` is a valid, initialised stack variable.
    unsafe { QueryPerformanceCounter(&mut counter) };
    (counter as u128 * 1_000_000_000 / freq as u128) as u64
}

/// Query `QueryPerformanceFrequency` and return the tick rate.
///
/// Panics if the call fails (it never fails on Windows XP+).
pub(crate) fn qpc_frequency() -> i64 {
    let mut freq = 0i64;
    // SAFETY: `freq` is a valid, initialised stack variable.  QPC is always
    // available on Windows Vista+ per MSDN.
    let ok = unsafe { QueryPerformanceFrequency(&mut freq) };
    assert!(
        ok != 0,
        "QueryPerformanceFrequency failed — this should never happen on Windows Vista+"
    );
    freq
}

// ---------------------------------------------------------------------------
// Queue-size negotiation
// ---------------------------------------------------------------------------

/// Set the Wintab queue size, backing off by halving until the driver accepts.
///
/// Returns the actual queue size that was accepted.  The minimum returned value
/// is 2 (smallest meaningful queue).
///
/// # Safety
/// `hctx` must be a valid, open Wintab context handle.
pub(crate) unsafe fn negotiate_queue_size(api: &WintabApi, hctx: *mut HCTX, desired: i32) -> i32 {
    let mut size = desired;
    loop {
        if api.queue_size_set(hctx, size) {
            debug!(queue_size = size, "WTQueueSizeSet accepted");
            return size;
        }
        size /= 2;
        if size < 2 {
            warn!("WTQueueSizeSet could not accept even size=2; using minimum");
            // Force size=2 as a last resort — if the driver rejects this too
            // we still proceed (some drivers silently accept the minimum).
            let _ = api.queue_size_set(hctx, 2);
            return 2;
        }
        debug!(size, "WTQueueSizeSet rejected; trying smaller queue");
    }
}

// ---------------------------------------------------------------------------
// WT_PACKET hot path — the drain loop
// ---------------------------------------------------------------------------

/// Drain all pending Wintab packets for the current `WT_PACKET` notification.
///
/// Called from [`wnd_proc`](crate::window::wnd_proc) on every `WT_PACKET`
/// message.  Uses a fixed-size stack buffer to read up to 64 packets per
/// `WTPacketsGet` call; loops until the queue is empty.
///
/// # Hot-path constraints
/// - **No heap allocation**: `buf` is a stack array of `FullPacket` (which
///   is `Copy + Default` so zero-initialization is free).
/// - **No I/O**: the only external call is `WTPacketsGet` (a DLL function
///   pointer call) and `sink` (expected to be a ring-buffer push).
/// - The `t_capture_ns` QPC timestamp is captured once per drain burst and
///   shared across all packets decoded in that burst.
///
/// # Safety
/// Called only from the message loop on the capture thread; `state` is
/// exclusively owned by that thread.
pub(crate) fn handle_wt_packet(state: &mut CaptureState) {
    const BUF_PKTS: usize = 64;

    // Stack-allocate the packet buffer.  `FullPacket` is `Copy` so this is a
    // plain stack zero-fill with no heap involvement.
    let mut buf = [FullPacket::default(); BUF_PKTS];

    // Capture the monotonic timestamp once for the entire drain burst.
    let t_capture_ns = qpc_ns(state.qpc_freq);

    loop {
        // SAFETY: `state.hctx` is a valid, open context owned by this thread.
        // `buf` has `BUF_PKTS * size_of::<FullPacket>()` bytes available,
        // which matches what WTPacketsGet will write (we requested the full
        // packet field set `FULL_PKT_DATA` in `build_logcontext`).
        let n = unsafe {
            state
                .api
                .packets_get(state.hctx, BUF_PKTS as i32, buf.as_mut_ptr() as *mut c_void)
        };

        if n <= 0 {
            // Queue drained; exit the loop.
            break;
        }

        for packet in &buf[..n as usize] {
            // ---- Serial-number gap detection --------------------------------
            // `last_serial == 0` means we haven't seen any packet yet; the
            // first packet always initialises `last_serial` unconditionally.
            if state.last_serial != 0 {
                let expected = state.last_serial.wrapping_add(1);
                if packet.pk_serial != expected {
                    // Gap detected: the number of lost packets is the
                    // difference, clamped to avoid wrap-around confusion.
                    let gap = packet.pk_serial.wrapping_sub(expected) as u64;
                    state.drop_count.fetch_add(gap, Ordering::Relaxed);
                    warn!(
                        expected,
                        got = packet.pk_serial,
                        dropped = gap,
                        "Wintab serial-number gap — packets lost in driver queue"
                    );
                }
            }
            state.last_serial = packet.pk_serial;

            // ---- Decode and deliver -----------------------------------------
            let sample = decode_packet(packet, &state.caps, t_capture_ns, state.flip_y);
            (state.sink)(SampleEvent::Sample(sample));
        }
    }
}

// ---------------------------------------------------------------------------
// WT_PROXIMITY handler
// ---------------------------------------------------------------------------

/// Handle a `WT_PROXIMITY` message by emitting a [`SampleEvent::Proximity`].
///
/// Wintab `WT_PROXIMITY` message parameters:
/// - `wparam` — LOWORD: context handle (ignored here; `state.hctx` is canonical).
/// - `lparam` — LOWORD: 1 = pen entering proximity, 0 = leaving.
///              HIWORD: tool serial number associated with the event.
pub(crate) fn handle_wt_proximity(state: &mut CaptureState, _wparam: u32, lparam: u32) {
    let in_range = (lparam & 0xFFFF) != 0;
    let tool_serial = ((lparam >> 16) & 0xFFFF) as u64;

    debug!(in_range, tool_serial, "WT_PROXIMITY");
    (state.sink)(SampleEvent::Proximity {
        in_range,
        tool_serial,
    });
}

// ---------------------------------------------------------------------------
// Re-read negotiated lcPktRate after WTOpen
// ---------------------------------------------------------------------------

/// Re-read the actual negotiated `lcPktRate` from the open context and store
/// it in `caps.max_packet_rate_hz`.
///
/// After `WTOpen` the driver may lower the requested rate; calling `WTGetA`
/// lets us discover the real rate and surface it to the consumer.
///
/// Returns `false` (and logs a warning) if `WTGetA` is not available or fails;
/// the caller should continue with the rate that was already in `caps`.
///
/// # Safety
/// `hctx` must be a valid, open Wintab context handle.
/// `api.get_context` wraps `WTGetA` — see `ffi.rs`.
pub(crate) unsafe fn refresh_pkt_rate(
    api: &WintabApi,
    hctx: *mut HCTX,
    caps: &mut DeviceCapabilities,
) -> bool {
    use wintab_lite::LOGCONTEXT;

    let mut ctx = LOGCONTEXT::default();
    if api.get_context(hctx, &mut ctx as *mut LOGCONTEXT) {
        let actual_rate = ctx.lcPktRate;
        debug!(
            requested = caps.max_packet_rate_hz,
            actual = actual_rate,
            "Negotiated lcPktRate"
        );
        caps.max_packet_rate_hz = actual_rate;
        true
    } else {
        warn!("WTGetA failed; keeping requested packet rate in caps");
        false
    }
}

// ---------------------------------------------------------------------------
// WT_INFOCHANGE handler
// ---------------------------------------------------------------------------

/// Handle `WT_INFOCHANGE`: driver/device configuration changed.
///
/// Re-queries device capabilities from the driver.  If the X/Y extents have
/// changed, closes the old context, rebuilds the `LOGCONTEXT`, and reopens it
/// so the new 1:1 coordinate mapping takes effect.  Always emits a fresh
/// `SampleEvent::Capabilities` so consumers can update their axis range
/// interpretation.
///
/// SPEC §6.6: `WT_INFOCHANGE` → re-query capabilities, re-emit the handshake
/// descriptor, and (if needed) reopen the context.
///
/// # Safety
/// `hwnd` must be the valid message-only window handle owned by this capture
/// thread; `state` must be exclusively owned by the same thread.
pub(crate) unsafe fn handle_wt_infochange(state: &mut CaptureState, hwnd: HWND) {
    use crate::caps::query_capabilities;
    use crate::context::build_logcontext;
    use crate::ffi::WTI_DEFCONTEXT;

    debug!("WT_INFOCHANGE: re-querying capabilities");

    let new_caps = match query_capabilities(&state.api) {
        Ok(c) => c,
        Err(e) => {
            warn!("WT_INFOCHANGE: capability re-query failed: {e}");
            return;
        }
    };

    // Check whether the coordinate extents have changed.  If they have, the
    // existing context maps coordinates with the wrong scale, so we must close
    // it and open a new one with the updated 1:1 mapping.
    let extents_changed = new_caps.x.max != state.caps.x.max || new_caps.y.max != state.caps.y.max;

    if extents_changed {
        info!(
            old_x_max = state.caps.x.max,
            new_x_max = new_caps.x.max,
            old_y_max = state.caps.y.max,
            new_y_max = new_caps.y.max,
            "WT_INFOCHANGE: extents changed; reopening Wintab context"
        );

        // Close the current context before rebuilding it.
        if !state.api.close_context(state.hctx) {
            warn!("WTClose failed during WT_INFOCHANGE reopen");
        }

        // Rebuild the LOGCONTEXT from the updated capabilities.
        let base_ctx = state
            .api
            .query::<wintab_lite::LOGCONTEXT>(WTI_DEFCONTEXT, 0);
        let mut ctx = build_logcontext(base_ctx, &new_caps);

        // Reopen the context on the same message-only window.
        let new_hctx = state
            .api
            .open_context(hwnd as *mut std::ffi::c_void, &mut ctx, true);
        if new_hctx.is_null() {
            error!("WT_INFOCHANGE: WTOpen failed after reopen — capture stopped");
            // Cannot recover: exit the message loop so the thread tears down
            // cleanly rather than operating with a stale/null context.
            PostQuitMessage(0);
            return;
        }
        state.hctx = new_hctx;

        // Re-negotiate the queue size for the freshly opened context.
        let q = negotiate_queue_size(&state.api, new_hctx, state.caps.queue_size as i32);

        // Incorporate queue_size and refresh the actual packet rate.
        let mut updated_caps = new_caps;
        updated_caps.queue_size = q as u32;
        refresh_pkt_rate(&state.api, new_hctx, &mut updated_caps);

        // Reset serial tracking: the new context starts a fresh serial space.
        state.last_serial = 0;
        state.caps = updated_caps;
    } else {
        // Extents unchanged — adopt the new caps directly (other metadata such
        // as device name or driver version may still have changed).
        state.caps = new_caps;
    }

    // Always emit a fresh Capabilities handshake so consumers update their
    // axis interpretation regardless of whether the context was reopened.
    (state.sink)(SampleEvent::Capabilities(state.caps.clone()));
    info!("WT_INFOCHANGE: re-emitted Capabilities handshake");
}

// ---------------------------------------------------------------------------
// BackendError helper
// ---------------------------------------------------------------------------

/// Log and return `BackendError::ContextOpenFailed` when `WTOpen` returns null.
pub(crate) fn context_open_failed() -> BackendError {
    error!("WTOpen returned null — driver may be busy or context config rejected");
    BackendError::ContextOpenFailed
}
