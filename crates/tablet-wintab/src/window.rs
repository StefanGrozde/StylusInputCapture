//! Message-only window creation for Wintab packet delivery (SPEC §6.5).
//!
//! A "message-only window" (`HWND_MESSAGE` as parent) is a Win32 window that
//! has no visual representation but participates in the message queue.  Wintab
//! requires a valid `HWND` to deliver `WT_PACKET` and other notifications;
//! using `HWND_MESSAGE` ensures the window is invisible and does not interfere
//! with the desktop.
//!
//! All Win32 calls in this module are wrapped with safety comments.  This file
//! must only be compiled on Windows.

use std::ffi::c_void;

use tablet_core::BackendError;
use tracing::{debug, error, warn};
use windows_sys::Win32::Foundation::{GetLastError, HWND, LRESULT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExA, DefWindowProcA, DestroyWindow, GetWindowLongPtrA, PostQuitMessage,
    RegisterClassExA, CW_USEDEFAULT, GWLP_USERDATA, HMENU, HWND_MESSAGE, WNDCLASSEXA,
    WS_OVERLAPPEDWINDOW,
};

use crate::capture::CaptureState;

// ---------------------------------------------------------------------------
// Wintab window-message constants (not provided by windows-sys / wintab_lite)
// ---------------------------------------------------------------------------

/// `WT_PACKET`: delivered when pen data is available in the Wintab queue.
pub(crate) const WT_PACKET: u32 = 0x7FF0;
/// `WT_PROXIMITY`: delivered on pen proximity change (enter/leave context).
pub(crate) const WT_PROXIMITY: u32 = 0x7FF1;
/// `WT_INFOCHANGE`: driver/device configuration changed (hot-plug, etc.).
pub(crate) const WT_INFOCHANGE: u32 = 0x7FF3;
/// `WT_CTXOPEN`: a Wintab context has been opened.
#[allow(dead_code)]
pub(crate) const WT_CTXOPEN: u32 = 0x7FF4;
/// `WT_CTXCLOSE`: a Wintab context has been closed.
#[allow(dead_code)]
pub(crate) const WT_CTXCLOSE: u32 = 0x7FF5;

/// Custom message sent to the message-only window to request graceful shutdown.
/// Uses `WM_USER + 1` in the application-specific range (0x0400..0x7FFF).
pub(crate) const WM_USER_STOP: u32 = 0x0400 + 1;

// ---------------------------------------------------------------------------
// Window procedure
// ---------------------------------------------------------------------------

/// The Win32 window procedure for the Wintab message-only window.
///
/// # Safety
/// This function is called by the OS as part of the Win32 message dispatch.
/// The `GWLP_USERDATA` slot must contain a valid `*mut CaptureState` before
/// any Wintab message arrives; this is established in `start()` before the
/// message loop begins.
pub(crate) unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> LRESULT {
    // Retrieve the CaptureState pointer stored in the window's user-data slot.
    let state_ptr = GetWindowLongPtrA(hwnd, GWLP_USERDATA) as *mut CaptureState;
    if state_ptr.is_null() {
        // State not yet installed (early messages during window creation) —
        // fall through to default processing.
        return DefWindowProcA(hwnd, msg, wparam, lparam);
    }

    // SAFETY: pointer is non-null, was allocated via Box::into_raw in
    // backend::start(), and is only accessed from this single capture thread.
    let state = &mut *state_ptr;

    match msg {
        WT_PACKET => {
            // WPARAM carries the context handle; the drain loop re-uses
            // state.hctx, so we ignore it here.
            crate::capture::handle_wt_packet(state);
            0
        }
        WT_PROXIMITY => {
            // WPARAM: LOWORD = context handle (unused here).
            // LPARAM: LOWORD = near/far flag, HIWORD = tool serial.
            crate::capture::handle_wt_proximity(state, wparam as u32, lparam as u32);
            0
        }
        WT_INFOCHANGE => {
            // Re-query capabilities, re-emit the Capabilities handshake, and
            // (if extents changed) reopen the Wintab context.  SPEC §6.6.
            // SAFETY: `state` is exclusively owned by this thread; `hwnd` is
            // the valid message-only window handle that owns this message loop.
            crate::capture::handle_wt_infochange(state, hwnd);
            0
        }
        WM_USER_STOP => {
            // Graceful shutdown: exit the message loop.
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcA(hwnd, msg, wparam, lparam),
    }
}

// ---------------------------------------------------------------------------
// Window class registration
// ---------------------------------------------------------------------------

/// Register the window class for our message-only Wintab window.
///
/// `class_name` must be a NUL-terminated byte slice.  If the class is already
/// registered (e.g. the process was restarted without unloading the DLL) the
/// function returns `Ok(())` — `ERROR_CLASS_ALREADY_EXISTS` is acceptable.
///
/// # Safety
/// `class_name` must remain valid for the lifetime of the call.
unsafe fn register_class(class_name: &[u8]) -> Result<(), BackendError> {
    let wc = WNDCLASSEXA {
        cbSize: std::mem::size_of::<WNDCLASSEXA>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: 0isize, // current module; 0 is fine for msg-only window
        lpszClassName: class_name.as_ptr(),
        // All other fields: zero / null (no icon, no cursor, no background).
        style: 0,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hIcon: 0isize,
        hCursor: 0isize,
        hbrBackground: 0isize,
        lpszMenuName: std::ptr::null(),
        hIconSm: 0isize,
    };

    let atom = RegisterClassExA(&wc);
    if atom == 0 {
        let err = GetLastError();
        // ERROR_CLASS_ALREADY_EXISTS = 0x582 (1410).  Re-use the existing
        // class if it's already there (same process, e.g. repeated start/stop).
        if err == 1410 {
            debug!("Window class already registered; reusing existing class");
            return Ok(());
        }
        error!(win32_err = err, "RegisterClassExA failed");
        return Err(BackendError::ContextOpenFailed);
    }

    debug!("Window class registered (atom={atom})");
    Ok(())
}

// ---------------------------------------------------------------------------
// Public window-creation entry point
// ---------------------------------------------------------------------------

/// Create a message-only window that will receive Wintab `WT_PACKET` messages.
///
/// The returned `HWND` is owned by the calling thread; the caller is
/// responsible for calling [`destroy_message_window`] to release it.
///
/// # Safety
/// Must be called from the thread that will run the message loop.  The
/// returned `HWND` is only safe to use from that thread.
pub(crate) unsafe fn create_message_window(class_name: &[u8]) -> Result<HWND, BackendError> {
    // 1. Register the window class (idempotent).
    register_class(class_name)?;

    // 2. Create the window with HWND_MESSAGE as parent → invisible / message-only.
    //    `HWND_MESSAGE` is defined as -3 cast to HWND.
    let hwnd = CreateWindowExA(
        0,                          // dwExStyle
        class_name.as_ptr(),        // lpClassName
        std::ptr::null(),           // lpWindowName (none needed)
        WS_OVERLAPPEDWINDOW,        // dwStyle (ignored for HWND_MESSAGE)
        CW_USEDEFAULT,              // X
        CW_USEDEFAULT,              // Y
        CW_USEDEFAULT,              // nWidth
        CW_USEDEFAULT,              // nHeight
        HWND_MESSAGE,               // hWndParent — message-only
        0isize as HMENU,            // hMenu
        0isize,                     // hInstance
        std::ptr::null::<c_void>(), // lpParam (state attached later via GWLP_USERDATA)
    );

    if hwnd == 0 {
        let err = GetLastError();
        error!(win32_err = err, "CreateWindowExA failed");
        return Err(BackendError::ContextOpenFailed);
    }

    debug!(?hwnd, "Message-only window created");
    Ok(hwnd)
}

/// Destroy a message-only window previously created with [`create_message_window`].
///
/// # Safety
/// `hwnd` must be a valid window handle created on the calling thread.
pub(crate) unsafe fn destroy_message_window(hwnd: HWND) {
    if DestroyWindow(hwnd) == 0 {
        let err = unsafe { GetLastError() };
        warn!(
            win32_err = err,
            "DestroyWindow failed (window may already be gone)"
        );
    } else {
        debug!("Message-only window destroyed");
    }
}
