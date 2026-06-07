//! Raw Input registration/unregistration (SPEC_BGC §4, §8).
//!
//! Registers the HID **Digitizer** (`0x0D`/`0x01`) and **Pen** (`0x0D`/`0x02`)
//! top-level collections with `RIDEV_INPUTSINK` so our message-only window keeps
//! receiving reports while another application owns the foreground — the defining
//! requirement of this backend. `RIDEV_DEVNOTIFY` is set so the same window also
//! gets `WM_INPUT_DEVICE_CHANGE` for hot-plug (SPEC_BGC §8). `RIDEV_INPUTSINK`
//! requires a non-null `hwndTarget`.

use tablet_core::BackendError;
use tracing::{debug, error};
use windows_sys::Win32::Foundation::{GetLastError, HWND};
use windows_sys::Win32::UI::Input::{
    RegisterRawInputDevices, RAWINPUTDEVICE, RIDEV_DEVNOTIFY, RIDEV_INPUTSINK, RIDEV_REMOVE,
};

use crate::hid::{PAGE_DIGITIZER, TLC_DIGITIZER, TLC_PEN};

/// Build the two `RAWINPUTDEVICE` entries (Digitizer + Pen) for a target window.
fn devices(hwnd: HWND, flags: u32) -> [RAWINPUTDEVICE; 2] {
    [
        RAWINPUTDEVICE {
            usUsagePage: PAGE_DIGITIZER,
            usUsage: TLC_DIGITIZER,
            dwFlags: flags,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: PAGE_DIGITIZER,
            usUsage: TLC_PEN,
            dwFlags: flags,
            hwndTarget: hwnd,
        },
    ]
}

/// Register for background pen reports on `hwnd` (`RIDEV_INPUTSINK | RIDEV_DEVNOTIFY`).
///
/// `hwnd` must be a valid (message-only) window owned by the calling thread.
pub fn register(hwnd: HWND) -> Result<(), BackendError> {
    let rid = devices(hwnd, RIDEV_INPUTSINK | RIDEV_DEVNOTIFY);
    // SAFETY: `rid` is a valid 2-element array; `hwnd` is a live window handle.
    let ok = unsafe {
        RegisterRawInputDevices(
            rid.as_ptr(),
            rid.len() as u32,
            std::mem::size_of::<RAWINPUTDEVICE>() as u32,
        )
    };
    if ok == 0 {
        let win32_error = unsafe { GetLastError() };
        error!(win32_error, "RegisterRawInputDevices failed");
        return Err(BackendError::RawInputRegistrationFailed { win32_error });
    }
    debug!("Raw Input registered (INPUTSINK | DEVNOTIFY) for digitizer + pen");
    Ok(())
}

/// Unregister both collections (`RIDEV_REMOVE`). `hwndTarget` must be null for
/// removal, per the Win32 contract. Best-effort: failures are logged, not fatal.
pub fn unregister() {
    let rid = devices(0 as HWND, RIDEV_REMOVE);
    // SAFETY: valid array; RIDEV_REMOVE requires a null hwndTarget, which we pass.
    let ok = unsafe {
        RegisterRawInputDevices(
            rid.as_ptr(),
            rid.len() as u32,
            std::mem::size_of::<RAWINPUTDEVICE>() as u32,
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        debug!(win32_error = err, "RegisterRawInputDevices(RIDEV_REMOVE) failed");
    } else {
        debug!("Raw Input unregistered");
    }
}
