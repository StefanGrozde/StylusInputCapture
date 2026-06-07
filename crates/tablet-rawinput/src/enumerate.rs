//! Device enumeration and live `DeviceProfile` construction (SPEC_BGC §7).
//!
//! Replaces Wintab's `WTInfo` as the source of [`ProfileCaps`]: we enumerate Raw
//! Input devices, keep HID digitizers/pens (usage page `0x0D`, usage `0x01`/`0x02`),
//! and parse each one's HID **preparsed data** with `HidP_*` to learn which usages
//! exist and their logical ranges. The owning preparsed-data buffer is retained in
//! the profile so the decode hot path can read usages without re-querying.
//!
//! All `unsafe` FFI is isolated here behind safe wrappers; nothing `unsafe` leaks
//! into a public signature.

use std::collections::HashMap;

use tracing::{debug, warn};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HidP_GetButtonCaps, HidP_GetCaps, HidP_GetValueCaps, HidP_Input, HIDP_BUTTON_CAPS, HIDP_CAPS,
    HIDP_VALUE_CAPS,
};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::UI::Input::{
    GetRawInputDeviceInfoW, GetRawInputDeviceList, RAWINPUTDEVICELIST, RIDI_DEVICEINFO,
    RIDI_DEVICENAME, RIDI_PREPARSEDDATA, RID_DEVICE_INFO, RIM_TYPEHID,
};

use crate::caps::{AxisRange, ProfileCaps};
use crate::hid::*;

/// `HidP_*` success status (`HIDP_STATUS_SUCCESS`).
const HIDP_STATUS_SUCCESS: i32 = 0x0011_0000;

/// A live, decoded device: its Raw Input handle, the owned HID preparsed-data
/// buffer (kept alive for the decode hot path), and the parsed [`ProfileCaps`].
///
/// The preparsed-data pointer for `HidP_*` is [`Self::preparsed_ptr`].
pub struct DeviceProfile {
    /// Raw Input device handle (`RAWINPUTHEADER.hDevice`).
    pub handle: HANDLE,
    /// Owned HID preparsed-data bytes; `HidP_*` reads through `preparsed_ptr()`.
    preparsed: Vec<u8>,
    /// Parsed, OS-independent capability model.
    pub caps: ProfileCaps,
}

impl DeviceProfile {
    /// Pointer to the preparsed data for `HidP_*` calls.
    pub fn preparsed_ptr(&self) -> isize {
        self.preparsed.as_ptr() as isize
    }
}

/// Map keyed by Raw Input device handle (as `isize`) → its profile.
pub type ProfileMap = HashMap<isize, DeviceProfile>;

/// Enumerate connected HID digitizer/pen devices and build a profile for each.
///
/// Returns an empty map if none are present; callers translate that into
/// [`BackendError::NoDevice`] with the Windows-Ink hint (SPEC_BGC §9).
pub fn enumerate_digitizers() -> ProfileMap {
    let mut map = ProfileMap::new();
    // SAFETY: standard two-call GetRawInputDeviceList pattern; sizes are checked.
    for entry in unsafe { raw_input_device_list() } {
        if entry.dwType != RIM_TYPEHID {
            continue;
        }
        match build_profile(entry.hDevice) {
            Some(profile) => {
                debug!(name = %profile.caps.device_name, "digitizer profile built");
                map.insert(entry.hDevice, profile);
            }
            None => continue,
        }
    }
    map
}

/// Build a [`DeviceProfile`] for a single device handle, or `None` if the device
/// is not a HID digitizer/pen or its descriptor cannot be parsed.
pub fn build_profile(handle: HANDLE) -> Option<DeviceProfile> {
    // SAFETY: `handle` came from GetRawInputDeviceList / a device-change message.
    let info = unsafe { device_info(handle)? };
    // RID_DEVICE_INFO union: the HID arm is valid because dwType == RIM_TYPEHID.
    let hid = unsafe { info.Anonymous.hid };
    if hid.usUsagePage != PAGE_DIGITIZER
        || (hid.usUsage != TLC_DIGITIZER && hid.usUsage != TLC_PEN)
    {
        return None;
    }

    let name = unsafe { device_name(handle) }.unwrap_or_else(|| "HID digitizer".to_string());
    let preparsed = unsafe { preparsed_data(handle)? };

    let mut caps = parse_caps(&preparsed)?;
    caps.device_name = name;
    caps.driver_version = format!(
        "hid vid:{:04x} pid:{:04x} ver:{}",
        hid.dwVendorId, hid.dwProductId, hid.dwVersionNumber
    );

    Some(DeviceProfile {
        handle,
        preparsed,
        caps,
    })
}

// ---------------------------------------------------------------------------
// HID preparsed-data → ProfileCaps
// ---------------------------------------------------------------------------

/// Parse a preparsed-data buffer into [`ProfileCaps`] via `HidP_*`.
fn parse_caps(preparsed: &[u8]) -> Option<ProfileCaps> {
    let pp = preparsed.as_ptr() as isize;

    // HidP_GetCaps → counts of input value/button caps.
    let mut hidp_caps: HIDP_CAPS = unsafe { std::mem::zeroed() };
    // SAFETY: `pp` points at a valid preparsed-data buffer we own.
    if unsafe { HidP_GetCaps(pp, &mut hidp_caps) } != HIDP_STATUS_SUCCESS {
        warn!("HidP_GetCaps failed");
        return None;
    }

    let mut caps = ProfileCaps::default();

    // --- value caps ---
    let n_values = hidp_caps.NumberInputValueCaps as usize;
    if n_values > 0 {
        let mut value_caps: Vec<HIDP_VALUE_CAPS> = vec![unsafe { std::mem::zeroed() }; n_values];
        let mut len = n_values as u16;
        // SAFETY: buffer holds `n_values` entries; `len` is in/out.
        let status = unsafe {
            HidP_GetValueCaps(HidP_Input, value_caps.as_mut_ptr(), &mut len, pp)
        };
        if status == HIDP_STATUS_SUCCESS {
            for vc in value_caps.iter().take(len as usize) {
                apply_value_cap(&mut caps, vc);
            }
        } else {
            warn!("HidP_GetValueCaps failed");
        }
    }

    // --- button caps ---
    let n_buttons = hidp_caps.NumberInputButtonCaps as usize;
    if n_buttons > 0 {
        let mut button_caps: Vec<HIDP_BUTTON_CAPS> = vec![unsafe { std::mem::zeroed() }; n_buttons];
        let mut len = n_buttons as u16;
        // SAFETY: buffer holds `n_buttons` entries; `len` is in/out.
        let status = unsafe {
            HidP_GetButtonCaps(HidP_Input, button_caps.as_mut_ptr(), &mut len, pp)
        };
        if status == HIDP_STATUS_SUCCESS {
            for bc in button_caps.iter().take(len as usize) {
                apply_button_cap(&mut caps, bc);
            }
        } else {
            warn!("HidP_GetButtonCaps failed");
        }
    }

    Some(caps)
}

/// Fold one value cap into [`ProfileCaps`].
fn apply_value_cap(caps: &mut ProfileCaps, vc: &HIDP_VALUE_CAPS) {
    let page = vc.UsagePage;
    // We only consume single (non-range) value usages from the maps below.
    let usage = if vc.IsRange != 0 {
        unsafe { vc.Anonymous.Range.UsageMin }
    } else {
        unsafe { vc.Anonymous.NotRange.Usage }
    };

    let range = AxisRange {
        min: vc.LogicalMin,
        max: vc.LogicalMax,
        resolution: resolution_from_cap(vc),
        bit_size: vc.BitSize,
    };

    match (page, usage) {
        (PAGE_GENERIC_DESKTOP, USAGE_X) => caps.x = Some(range),
        (PAGE_GENERIC_DESKTOP, USAGE_Y) => caps.y = Some(range),
        (PAGE_DIGITIZER, USAGE_TIP_PRESSURE) => caps.pressure = Some(range),
        (PAGE_DIGITIZER, USAGE_BARREL_PRESSURE) => caps.tangent_pressure = Some(range),
        (PAGE_DIGITIZER, USAGE_X_TILT) => caps.x_tilt = Some(range),
        (PAGE_DIGITIZER, USAGE_Y_TILT) => caps.y_tilt = Some(range),
        (PAGE_DIGITIZER, USAGE_AZIMUTH) => caps.azimuth = Some(range),
        (PAGE_DIGITIZER, USAGE_ALTITUDE) => caps.altitude = Some(range),
        (PAGE_DIGITIZER, USAGE_TWIST) => caps.twist = Some(range),
        // In-Range sometimes appears as a value rather than a button.
        (PAGE_DIGITIZER, USAGE_IN_RANGE) => caps.has_in_range = true,
        _ => {}
    }
}

/// Fold one button cap into [`ProfileCaps`], honoring usage ranges.
fn apply_button_cap(caps: &mut ProfileCaps, bc: &HIDP_BUTTON_CAPS) {
    if bc.UsagePage != PAGE_DIGITIZER {
        return;
    }
    let (lo, hi) = if bc.IsRange != 0 {
        unsafe { (bc.Anonymous.Range.UsageMin, bc.Anonymous.Range.UsageMax) }
    } else {
        let u = unsafe { bc.Anonymous.NotRange.Usage };
        (u, u)
    };
    let covers = |u: u16| u >= lo && u <= hi;

    if covers(USAGE_IN_RANGE) {
        caps.has_in_range = true;
    }
    if covers(USAGE_TIP_SWITCH) {
        caps.has_tip = true;
    }
    if covers(USAGE_BARREL_SWITCH) {
        caps.has_barrel = true;
    }
    if covers(USAGE_ERASER) {
        caps.has_eraser = true;
    }
    if covers(USAGE_INVERT) {
        caps.has_invert = true;
    }
}

/// Best-effort native resolution (counts per physical unit) from a value cap.
///
/// HID encodes physical scaling via PhysicalMin/Max and a unit exponent; when a
/// physical range is present we report logical-per-physical scaled by the
/// exponent, else `0.0` (unknown) per SPEC_BGC §7.
fn resolution_from_cap(vc: &HIDP_VALUE_CAPS) -> f64 {
    if vc.PhysicalMax == vc.PhysicalMin {
        return 0.0;
    }
    let logical_span = (vc.LogicalMax - vc.LogicalMin) as f64;
    let physical_span = (vc.PhysicalMax - vc.PhysicalMin) as f64;
    let exp = unit_exponent(vc.UnitsExp);
    let denom = physical_span * 10f64.powi(exp);
    if denom == 0.0 {
        0.0
    } else {
        (logical_span / denom).abs()
    }
}

/// Decode a HID 4-bit unit exponent nibble (`0x0..0xF`) into a signed power of ten.
fn unit_exponent(raw: u32) -> i32 {
    let nibble = (raw & 0xF) as i32;
    if nibble > 7 {
        nibble - 16
    } else {
        nibble
    }
}

// ---------------------------------------------------------------------------
// Thin GetRawInputDeviceInfo / GetRawInputDeviceList wrappers
// ---------------------------------------------------------------------------

/// Two-call enumeration into an owned `Vec<RAWINPUTDEVICELIST>`.
///
/// # Safety
/// Calls the Win32 enumeration API; the returned vec owns its contents.
unsafe fn raw_input_device_list() -> Vec<RAWINPUTDEVICELIST> {
    let cb = std::mem::size_of::<RAWINPUTDEVICELIST>() as u32;
    let mut count: u32 = 0;
    // First call: learn the device count.
    let ret = GetRawInputDeviceList(std::ptr::null_mut(), &mut count, cb);
    if ret == u32::MAX || count == 0 {
        return Vec::new();
    }
    let mut list: Vec<RAWINPUTDEVICELIST> = vec![std::mem::zeroed(); count as usize];
    let written = GetRawInputDeviceList(list.as_mut_ptr(), &mut count, cb);
    if written == u32::MAX {
        return Vec::new();
    }
    list.truncate(written as usize);
    list
}

/// Fetch `RID_DEVICE_INFO` for a device handle.
///
/// # Safety
/// `handle` must be a valid Raw Input device handle.
unsafe fn device_info(handle: HANDLE) -> Option<RID_DEVICE_INFO> {
    let mut info: RID_DEVICE_INFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<RID_DEVICE_INFO>() as u32;
    let mut size = info.cbSize;
    let ret = GetRawInputDeviceInfoW(
        handle,
        RIDI_DEVICEINFO,
        &mut info as *mut _ as *mut core::ffi::c_void,
        &mut size,
    );
    if ret == u32::MAX || ret == 0 {
        return None;
    }
    Some(info)
}

/// Fetch the device interface path (`RIDI_DEVICENAME`) as a UTF-8 string.
///
/// # Safety
/// `handle` must be a valid Raw Input device handle.
unsafe fn device_name(handle: HANDLE) -> Option<String> {
    let mut chars: u32 = 0;
    // Size call (count of UTF-16 chars including NUL).
    GetRawInputDeviceInfoW(handle, RIDI_DEVICENAME, std::ptr::null_mut(), &mut chars);
    if chars == 0 || chars == u32::MAX {
        return None;
    }
    let mut buf: Vec<u16> = vec![0u16; chars as usize];
    let ret = GetRawInputDeviceInfoW(
        handle,
        RIDI_DEVICENAME,
        buf.as_mut_ptr() as *mut core::ffi::c_void,
        &mut chars,
    );
    if ret == u32::MAX {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// Fetch the owned HID preparsed-data buffer (`RIDI_PREPARSEDDATA`).
///
/// # Safety
/// `handle` must be a valid Raw Input device handle.
unsafe fn preparsed_data(handle: HANDLE) -> Option<Vec<u8>> {
    let mut bytes: u32 = 0;
    GetRawInputDeviceInfoW(handle, RIDI_PREPARSEDDATA, std::ptr::null_mut(), &mut bytes);
    if bytes == 0 || bytes == u32::MAX {
        return None;
    }
    let mut buf: Vec<u8> = vec![0u8; bytes as usize];
    let ret = GetRawInputDeviceInfoW(
        handle,
        RIDI_PREPARSEDDATA,
        buf.as_mut_ptr() as *mut core::ffi::c_void,
        &mut bytes,
    );
    if ret == u32::MAX {
        return None;
    }
    Some(buf)
}
