//! Message-only window for Raw Input delivery (SPEC_BGC §4).
//!
//! `RIDEV_INPUTSINK` requires a valid, non-null `hwndTarget`. A *message-only*
//! window (`HWND_MESSAGE` parent) has no visual presence and never takes focus,
//! yet still receives `WM_INPUT` / `WM_INPUT_DEVICE_CHANGE` — exactly what a
//! background capturer needs. The window procedure dispatches those messages into
//! [`crate::capture`] via the [`CaptureState`] pointer stashed in `GWLP_USERDATA`.
//!
//! All `unsafe` Win32 calls are wrapped here; nothing `unsafe` leaks into a public
//! signature.

use core::ffi::c_void;

use tablet_core::BackendError;
use tracing::{debug, error, warn};
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, PostQuitMessage,
    RegisterClassExW, GWLP_USERDATA, HWND_MESSAGE, WM_DESTROY, WM_INPUT, WM_INPUT_DEVICE_CHANGE,
    WNDCLASSEXW,
};

use crate::capture::{handle_device_change, handle_wm_input, CaptureState};

/// Custom shutdown message (application range `WM_USER + 1`).
pub const WM_USER_STOP: u32 = 0x0400 + 1;

/// Window class name (NUL-terminated UTF-16).
const CLASS_NAME: &[u16] = &[
    b'R' as u16, b'a' as u16, b'w' as u16, b'I' as u16, b'n' as u16, b'p' as u16, b'u' as u16,
    b't' as u16, b'M' as u16, b's' as u16, b'g' as u16, b'W' as u16, b'n' as u16, b'd' as u16, 0,
];

/// Window procedure: dispatch Raw Input messages to the capture handlers.
///
/// # Safety
/// Invoked by the OS during message dispatch. `GWLP_USERDATA` must hold a valid
/// `*mut CaptureState` (installed by the capture thread before any input message);
/// until then messages fall through to the default handler.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CaptureState;

    match msg {
        WM_INPUT => {
            if !state_ptr.is_null() {
                // SAFETY: pointer installed by the capture thread; only this
                // (capture) thread runs this window's message loop.
                handle_wm_input(&mut *state_ptr);
            }
            // INPUTSINK requires DefWindowProc to perform input cleanup.
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_INPUT_DEVICE_CHANGE => {
            if !state_ptr.is_null() {
                // wParam: GIDC_ARRIVAL/REMOVAL; lParam: device handle.
                handle_device_change(&mut *state_ptr, wparam, lparam as _);
            }
            0
        }
        WM_USER_STOP => {
            PostQuitMessage(0);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Register the window class (idempotent — `ERROR_CLASS_ALREADY_EXISTS` is fine).
///
/// # Safety
/// `CLASS_NAME` outlives the call (it is `'static`).
unsafe fn register_class() -> Result<(), BackendError> {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: 0,
        hIcon: 0,
        hCursor: 0,
        hbrBackground: 0,
        lpszMenuName: std::ptr::null(),
        lpszClassName: CLASS_NAME.as_ptr(),
        hIconSm: 0,
    };
    let atom = RegisterClassExW(&wc);
    if atom == 0 {
        let err = GetLastError();
        const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
        if err == ERROR_CLASS_ALREADY_EXISTS {
            debug!("window class already registered; reusing");
            return Ok(());
        }
        error!(win32_err = err, "RegisterClassExW failed");
        return Err(BackendError::RawInputRegistrationFailed { win32_error: err });
    }
    Ok(())
}

/// Create the message-only window. The caller (capture thread) owns it and must
/// call [`destroy_message_window`] on teardown.
///
/// # Safety
/// Must be called from the thread that will run the message loop; the returned
/// `HWND` is only valid on that thread.
pub unsafe fn create_message_window() -> Result<HWND, BackendError> {
    register_class()?;
    let hwnd = CreateWindowExW(
        0,
        CLASS_NAME.as_ptr(),
        std::ptr::null(),
        0,
        0,
        0,
        0,
        0,
        HWND_MESSAGE,
        0,
        0,
        std::ptr::null::<c_void>(),
    );
    if hwnd == 0 {
        let err = GetLastError();
        error!(win32_err = err, "CreateWindowExW failed");
        return Err(BackendError::RawInputRegistrationFailed { win32_error: err });
    }
    debug!("message-only window created");
    Ok(hwnd)
}

/// Destroy a message-only window created by [`create_message_window`].
///
/// # Safety
/// `hwnd` must be a valid window created on the calling thread.
pub unsafe fn destroy_message_window(hwnd: HWND) {
    if DestroyWindow(hwnd) == 0 {
        warn!(win32_err = GetLastError(), "DestroyWindow failed");
    } else {
        debug!("message-only window destroyed");
    }
}
