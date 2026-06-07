//! Capture-thread state, the lossless `WM_INPUT` drain, and the live HID reader
//! (SPEC_BGC §3.2, §5, §6, §8).
//!
//! On each `WM_INPUT` the queue is drained in full with `GetRawInputBuffer` into a
//! **pre-sized buffer** (no per-report allocation, no I/O), iterating packed
//! `RAWINPUT` blocks and the `dwCount × dwSizeHid` HID reports inside each. Every
//! report is decoded by the pure [`crate::decode::decode_report`] through a
//! [`HidReader`] backed by `HidP_*`, stamped with one `QueryPerformanceCounter`
//! reading per drain burst and a synthesized monotonic serial, and pushed to the
//! sink. Proximity transitions and device hot-plug are handled here too (§8).

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tablet_core::{SampleEvent, ToolKind};
use tracing::{debug, warn};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HidP_GetUsageValue, HidP_GetUsages, HidP_Input,
};
use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
use windows_sys::Win32::System::Performance::{
    QueryPerformanceCounter, QueryPerformanceFrequency,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputBuffer, GetRawInputData, RAWINPUT, RAWINPUTHEADER, RID_INPUT, RIM_TYPEHID,
};

use crate::caps::{capabilities_from_profile, AxisRange, ProfileCaps};
use crate::decode::{decode_report, UsageReader};
use crate::enumerate::{build_profile, ProfileMap};
use crate::hid::*;

/// `HidP_*` success status.
const HIDP_STATUS_SUCCESS: i32 = 0x0011_0000;

/// Drain buffer size in bytes for `GetRawInputBuffer`. Pre-sized once; the drain
/// loop re-calls until the queue is empty, so this only bounds per-syscall batch.
const DRAIN_BUF_BYTES: usize = 64 * 1024;

/// Nominal reports-per-drain capacity reported as `DeviceCapabilities.queue_size`
/// (SPEC_BGC §5.2 — Wintab's queue depth has no Raw Input analogue).
pub const BATCH_REPORTS: u32 = (DRAIN_BUF_BYTES / 64) as u32;

/// `WM_INPUT_DEVICE_CHANGE` wParam values.
pub const GIDC_ARRIVAL: usize = 1;
pub const GIDC_REMOVAL: usize = 2;

/// All state owned by the capture thread and reached from the window procedure
/// via `GWLP_USERDATA`. Single-threaded: only the capture thread touches it.
pub struct CaptureState {
    /// Per-device HID profiles (preparsed data + caps), keyed by device handle.
    pub profiles: ProfileMap,
    /// Event sink (ring push). Must be cheap — no blocking I/O.
    pub sink: Box<dyn FnMut(SampleEvent) + Send>,
    /// Synthesized monotonic serial (SPEC_BGC §5.2).
    pub serial: u32,
    /// `QueryPerformanceCounter` ticks per second.
    pub qpc_freq: i64,
    /// Ring-overflow counter (kept for API parity; real drops are counted by the
    /// consumer's ring — SPEC_BGC §5.2).
    pub drop_count: Arc<AtomicU64>,
    /// Last observed in-range state per device, for proximity transitions.
    pub prox: HashMap<isize, bool>,
    /// Pre-sized drain buffer (no allocation on the hot path).
    pub buf: Vec<u8>,
    /// QPC ns of the first decoded report (rate measurement, §7/B6).
    pub first_ns: u64,
    /// Reports counted since `first_ns`.
    pub count_since_first: u64,
    /// Whether the measured-rate `Capabilities` re-emit has happened.
    pub rate_emitted: bool,
    /// Handle of the device that last produced a report (for removal proximity).
    pub active_handle: isize,
    /// Diagnostic counters (temporary bring-up instrumentation; one-shot logs).
    pub dbg_wm_input: u64,
    pub dbg_blocks: u64,
    pub dbg_unknown: u64,
    pub dbg_samples: u64,
}

impl CaptureState {
    /// Build capture state around an enumerated profile map and sink.
    pub fn new(
        profiles: ProfileMap,
        sink: Box<dyn FnMut(SampleEvent) + Send>,
        drop_count: Arc<AtomicU64>,
    ) -> Self {
        Self {
            profiles,
            sink,
            serial: 0,
            qpc_freq: qpc_frequency(),
            drop_count,
            prox: HashMap::new(),
            buf: vec![0u8; DRAIN_BUF_BYTES],
            first_ns: 0,
            count_since_first: 0,
            rate_emitted: false,
            active_handle: 0,
            dbg_wm_input: 0,
            dbg_blocks: 0,
            dbg_unknown: 0,
            dbg_samples: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// QPC helpers
// ---------------------------------------------------------------------------

/// Read the QPC frequency (ticks/sec).
pub fn qpc_frequency() -> i64 {
    let mut f: i64 = 0;
    // SAFETY: writes a single i64; always succeeds on supported platforms.
    unsafe { QueryPerformanceFrequency(&mut f) };
    if f == 0 {
        1
    } else {
        f
    }
}

/// Current monotonic time in nanoseconds from QPC.
pub fn qpc_ns(freq: i64) -> u64 {
    let mut c: i64 = 0;
    // SAFETY: writes a single i64.
    unsafe { QueryPerformanceCounter(&mut c) };
    ((c as i128 * 1_000_000_000) / freq as i128) as u64
}

// ---------------------------------------------------------------------------
// WM_INPUT drain (hot path)
// ---------------------------------------------------------------------------

/// Drain the entire Raw Input queue and decode every pending report.
///
/// `hrawinput` is the `WM_INPUT` `lParam` (an `HRAWINPUT`), used for the
/// single-read fallback if the batched `GetRawInputBuffer` path yields nothing.
///
/// One QPC timestamp is taken for the whole burst (SPEC_BGC §5.1). No allocation
/// or I/O occurs here.
pub fn handle_wm_input(state: &mut CaptureState, hrawinput: isize) {
    let now_ns = qpc_ns(state.qpc_freq);
    let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;

    state.dbg_wm_input += 1;
    if state.dbg_wm_input == 1 {
        debug!("diag: first WM_INPUT received (registration is delivering)");
    }

    let mut total_blocks: u64 = 0;
    loop {
        let mut size = state.buf.len() as u32;
        // SAFETY: buffer is valid for `size` bytes; header_size is correct.
        let count = unsafe {
            GetRawInputBuffer(
                state.buf.as_mut_ptr() as *mut RAWINPUT,
                &mut size,
                header_size,
            )
        };
        if count == 0 {
            break; // buffered queue empty
        }
        if count == u32::MAX {
            let err = unsafe { GetLastError() };
            warn!(win32_error = err, "GetRawInputBuffer returned error; aborting this drain");
            break;
        }
        total_blocks += count as u64;
        if state.dbg_wm_input <= 3 {
            debug!(blocks = count, "diag: GetRawInputBuffer drained blocks");
        }

        // Walk `count` packed RAWINPUT blocks.
        let mut ptr = state.buf.as_ptr() as *const RAWINPUT;
        for _ in 0..count {
            // SAFETY: `ptr` addresses a valid RAWINPUT within the filled buffer.
            unsafe {
                process_raw_block(state, ptr, now_ns);
                ptr = next_block(ptr);
            }
        }
    }

    // Fallback: if the buffered read yielded nothing for this WM_INPUT, read the
    // single report directly. Some drivers / message-only window setups don't
    // populate the buffered queue, so GetRawInputBuffer returns 0 even though a
    // report is available via GetRawInputData(lParam).
    if total_blocks == 0 && hrawinput != 0 {
        if state.dbg_wm_input <= 3 {
            debug!("diag: buffered drain empty; using GetRawInputData single-read fallback");
        }
        handle_wm_input_single(state, hrawinput, now_ns);
    }
}

/// Fallback single-report read via `GetRawInputData` for the `WM_INPUT` handle,
/// used only if batched draining is unavailable. `now_ns` is the burst timestamp.
pub fn handle_wm_input_single(state: &mut CaptureState, hrawinput: isize, now_ns: u64) {
    let mut size = state.buf.len() as u32;
    let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    // SAFETY: buffer valid for `size` bytes; RID_INPUT reads one record.
    let written = unsafe {
        GetRawInputData(
            hrawinput as _,
            RID_INPUT,
            state.buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut size,
            header_size,
        )
    };
    if written == u32::MAX {
        let err = unsafe { GetLastError() };
        warn!(win32_error = err, "GetRawInputData single-read failed");
        return;
    }
    if written == 0 {
        return;
    }
    // SAFETY: buffer now holds one valid RAWINPUT.
    unsafe { process_raw_block(state, state.buf.as_ptr() as *const RAWINPUT, now_ns) };
}

/// Advance to the next packed `RAWINPUT` block (`NEXTRAWINPUTBLOCK`: round the
/// end of this block up to an 8-byte boundary).
///
/// # Safety
/// `ptr` must point at a valid `RAWINPUT` whose `header.dwSize` is correct.
unsafe fn next_block(ptr: *const RAWINPUT) -> *const RAWINPUT {
    let size = (*ptr).header.dwSize as usize;
    let raw = ptr as usize + size;
    let aligned = (raw + 7) & !7usize;
    aligned as *const RAWINPUT
}

/// Decode every HID report inside one `RAWINPUT` block and push samples.
///
/// # Safety
/// `raw` must point at a valid `RAWINPUT`.
unsafe fn process_raw_block(state: &mut CaptureState, raw: *const RAWINPUT, now_ns: u64) {
    state.dbg_blocks += 1;
    let dwtype = (*raw).header.dwType;
    let key = (*raw).header.hDevice;
    if state.dbg_blocks <= 3 {
        debug!(dwtype, key, "diag: raw block header");
    }
    if dwtype != RIM_TYPEHID {
        return;
    }

    // If we have no profile for this handle, the device delivering reports wasn't
    // in the enumerated set (e.g. it appeared without a device-change message, or
    // its top-level usage differed at enumeration). Try to build one lazily before
    // giving up, so we don't silently drop a live pen.
    if !state.profiles.contains_key(&key) {
        match build_profile(key) {
            Some(profile) => {
                debug!(key, name = %profile.caps.device_name, "diag: lazily built profile for reporting device");
                state.profiles.insert(key, profile);
            }
            None => {
                state.dbg_unknown += 1;
                if state.dbg_unknown <= 3 {
                    warn!(key, "diag: WM_INPUT for handle with no digitizer profile; dropping report");
                }
                return;
            }
        }
    }

    // Borrow the profile only long enough to capture Copy/raw views, so the sink
    // (which mutably borrows `state`) doesn't conflict. The profile map is not
    // mutated during a drain, so the raw caps pointer stays valid.
    let (preparsed, caps_ptr): (isize, *const ProfileCaps) = match state.profiles.get(&key) {
        Some(p) => (p.preparsed_ptr(), &p.caps as *const _),
        None => return, // unreachable: just inserted above
    };
    let caps: &ProfileCaps = &*caps_ptr;

    let hid = &(*raw).data.hid;
    let size_hid = hid.dwSizeHid as usize;
    let count = hid.dwCount as usize;
    if state.dbg_blocks <= 3 {
        debug!(size_hid, count, "diag: hid report dimensions");
    }
    if size_hid == 0 || count == 0 {
        return;
    }
    let base = std::ptr::addr_of!((*raw).data.hid.bRawData) as *const u8;

    for i in 0..count {
        let report = base.add(i * size_hid);
        let reader = HidReader {
            preparsed,
            report,
            report_len: size_hid as u32,
            caps,
        };
        state.serial = state.serial.wrapping_add(1);
        let sample = decode_report(&reader, caps, now_ns, state.serial);

        state.dbg_samples += 1;
        if state.dbg_samples <= 5 {
            debug!(
                x = sample.x_raw,
                y = sample.y_raw,
                pressure = sample.pressure_raw,
                in_proximity = sample.in_proximity,
                "diag: decoded sample"
            );
        }

        emit_proximity_transition(state, key, sample.in_proximity, sample.tool_serial);
        state.active_handle = key;
        (state.sink)(SampleEvent::Sample(sample));

        measure_rate(state, key, now_ns);
    }
}

/// Emit a `Proximity` event when a device's in-range state changes (SPEC_BGC §8).
fn emit_proximity_transition(
    state: &mut CaptureState,
    key: isize,
    in_range: bool,
    tool_serial: u64,
) {
    let prev = state.prox.insert(key, in_range);
    if prev != Some(in_range) {
        (state.sink)(SampleEvent::Proximity {
            in_range,
            tool_serial,
        });
    }
}

/// Measure the report rate over the first ~1 s and re-emit `Capabilities` once
/// with the observed `max_packet_rate_hz` (SPEC_BGC §7/B6).
fn measure_rate(state: &mut CaptureState, key: isize, now_ns: u64) {
    if state.rate_emitted {
        return;
    }
    if state.first_ns == 0 {
        state.first_ns = now_ns;
    }
    state.count_since_first += 1;
    let elapsed = now_ns.saturating_sub(state.first_ns);
    if elapsed >= 1_000_000_000 {
        let hz = (state.count_since_first as u128 * 1_000_000_000 / elapsed as u128) as u32;
        if let Some(profile) = state.profiles.get(&key) {
            let mut caps = capabilities_from_profile(&profile.caps, BATCH_REPORTS);
            caps.max_packet_rate_hz = hz;
            debug!(rate_hz = hz, "measured packet rate; re-emitting capabilities");
            (state.sink)(SampleEvent::Capabilities(caps));
        }
        state.rate_emitted = true;
    }
}

// ---------------------------------------------------------------------------
// Hot-plug (WM_INPUT_DEVICE_CHANGE)
// ---------------------------------------------------------------------------

/// Handle device arrival/removal (SPEC_BGC §8): refresh the profile cache and
/// re-emit `Capabilities` on arrival; drop the cache entry (and emit a leaving
/// proximity event if it was active) on removal.
pub fn handle_device_change(state: &mut CaptureState, gidc: usize, hdevice: HANDLE) {
    let key = hdevice;
    match gidc {
        GIDC_ARRIVAL => {
            if let Some(profile) = build_profile(hdevice) {
                let caps = capabilities_from_profile(&profile.caps, BATCH_REPORTS);
                state.profiles.insert(key, profile);
                debug!("device arrival: re-emitting capabilities");
                (state.sink)(SampleEvent::Capabilities(caps));
            }
        }
        GIDC_REMOVAL => {
            if state.prox.get(&key).copied() == Some(true) {
                (state.sink)(SampleEvent::Proximity {
                    in_range: false,
                    tool_serial: 0,
                });
            }
            state.profiles.remove(&key);
            state.prox.remove(&key);
            debug!("device removal: cache entry dropped");
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Live HID reader (HidP-backed UsageReader)
// ---------------------------------------------------------------------------

/// A [`UsageReader`] backed by `HidP_GetUsageValue` / `HidP_GetUsages` against a
/// device's preparsed data. Robust across descriptor variation — Windows resolves
/// each usage's exact bit position; we only sign-extend signed value usages using
/// the cached caps.
struct HidReader<'a> {
    preparsed: isize,
    report: *const u8,
    report_len: u32,
    caps: &'a ProfileCaps,
}

impl HidReader<'_> {
    /// The cached range for a value usage, used to know signedness/bit size.
    fn range_for(&self, page: u16, usage: u16) -> Option<AxisRange> {
        match (page, usage) {
            (PAGE_GENERIC_DESKTOP, USAGE_X) => self.caps.x,
            (PAGE_GENERIC_DESKTOP, USAGE_Y) => self.caps.y,
            (PAGE_DIGITIZER, USAGE_TIP_PRESSURE) => self.caps.pressure,
            (PAGE_DIGITIZER, USAGE_BARREL_PRESSURE) => self.caps.tangent_pressure,
            (PAGE_DIGITIZER, USAGE_X_TILT) => self.caps.x_tilt,
            (PAGE_DIGITIZER, USAGE_Y_TILT) => self.caps.y_tilt,
            (PAGE_DIGITIZER, USAGE_AZIMUTH) => self.caps.azimuth,
            (PAGE_DIGITIZER, USAGE_ALTITUDE) => self.caps.altitude,
            (PAGE_DIGITIZER, USAGE_TWIST) => self.caps.twist,
            _ => None,
        }
    }
}

impl UsageReader for HidReader<'_> {
    fn value(&self, usage_page: u16, usage: u16) -> Option<i32> {
        let mut raw: u32 = 0;
        // SAFETY: preparsed/report are valid for this report; HidP does not write
        // through `report` (it is logically const).
        let status = unsafe {
            HidP_GetUsageValue(
                HidP_Input,
                usage_page,
                0,
                usage,
                &mut raw,
                self.preparsed,
                self.report as *mut u8,
                self.report_len,
            )
        };
        if status != HIDP_STATUS_SUCCESS {
            return None;
        }
        match self.range_for(usage_page, usage) {
            Some(r) if r.is_signed() => Some(sign_extend(raw, r.bit_size)),
            _ => Some(raw as i32),
        }
    }

    fn button(&self, usage_page: u16, usage: u16) -> bool {
        let mut list = [0u16; 64];
        let mut len: u32 = list.len() as u32;
        // SAFETY: `list`/`len` describe a valid in/out buffer; report is valid.
        let status = unsafe {
            HidP_GetUsages(
                HidP_Input,
                usage_page,
                0,
                list.as_mut_ptr(),
                &mut len,
                self.preparsed,
                self.report as *mut u8,
                self.report_len,
            )
        };
        if status != HIDP_STATUS_SUCCESS {
            return false;
        }
        list[..len as usize].contains(&usage)
    }
}

/// Two's-complement sign-extend a `bit_size`-bit value to `i32`.
fn sign_extend(val: u32, bit_size: u16) -> i32 {
    if bit_size == 0 || bit_size >= 32 {
        return val as i32;
    }
    let shift = 32 - bit_size as u32;
    ((val << shift) as i32) >> shift
}

// Silence "unused" warnings for items reserved for diagnostics/parity.
#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Arc<AtomicU64>>();
    let _ = ToolKind::Pen;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_extend_handles_negative_8bit() {
        assert_eq!(sign_extend(0xE2, 8), -30);
        assert_eq!(sign_extend(0x2D, 8), 45);
        assert_eq!(sign_extend(0xFFFF, 16), -1);
        assert_eq!(sign_extend(0x7FFF, 16), 32767);
    }

    #[test]
    fn sign_extend_passes_through_full_width() {
        assert_eq!(sign_extend(12345, 32), 12345);
        assert_eq!(sign_extend(0, 0), 0);
    }

    /// Pure proximity-transition state machine (no hardware): the sink should see
    /// exactly one `Proximity` per change of in-range state.
    #[test]
    fn proximity_emits_only_on_transition() {
        use std::sync::{Arc, Mutex};
        let events: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let mut state = CaptureState::new(
            ProfileMap::new(),
            Box::new(move |e| {
                if let SampleEvent::Proximity { in_range, .. } = e {
                    sink_events.lock().unwrap().push(in_range);
                }
            }),
            Arc::new(AtomicU64::new(0)),
        );

        let key = 1isize;
        // enter, stay, stay, leave, leave, enter
        for r in [true, true, true, false, false, true] {
            emit_proximity_transition(&mut state, key, r, 0);
        }
        assert_eq!(*events.lock().unwrap(), vec![true, false, true]);
    }

    #[test]
    fn removal_emits_leaving_proximity_when_active() {
        use std::sync::{Arc, Mutex};
        let events: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let mut state = CaptureState::new(
            ProfileMap::new(),
            Box::new(move |e| {
                if let SampleEvent::Proximity { in_range, .. } = e {
                    sink_events.lock().unwrap().push(in_range);
                }
            }),
            Arc::new(AtomicU64::new(0)),
        );
        let key = 5isize;
        state.prox.insert(key, true); // device currently in range
        handle_device_change(&mut state, GIDC_REMOVAL, key as HANDLE);
        assert_eq!(*events.lock().unwrap(), vec![false]);
        assert!(!state.prox.contains_key(&key));
    }
}
