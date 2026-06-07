//! `RawInputBackend` — the public Raw Input implementation of [`TabletBackend`]
//! (SPEC_BGC §3, §4, §8).
//!
//! ## Lifecycle
//! 1. `capabilities()` — enumerate HID digitizers and return the first device's
//!    descriptor (no capture started). `NoDevice` (with the Windows-Ink hint) if
//!    none are present.
//! 2. `start(sink)` — spawn the capture thread, which: creates the message-only
//!    window, enumerates digitizers, registers Raw Input (`RIDEV_INPUTSINK`),
//!    emits `Capabilities`, installs `CaptureState`, then runs the Win32 message
//!    loop draining `WM_INPUT`.
//! 3. `stop()` — post `WM_USER_STOP`, then join (the thread unregisters with
//!    `RIDEV_REMOVE` and destroys the window).
//!
//! The window `HWND` is sent to the caller over an `mpsc` channel once ready, so
//! `stop()` can post the quit message without a race.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use tablet_core::{BackendError, DeviceCapabilities, SampleEvent, TabletBackend};
use tracing::{debug, error, info, warn};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, GetWindowLongPtrW, PostMessageW, SetWindowLongPtrW,
    TranslateMessage, GWLP_USERDATA, MSG,
};

use crate::capture::{CaptureState, BATCH_REPORTS};
use crate::caps::capabilities_from_profile;
use crate::enumerate::enumerate_digitizers;
use crate::register::{register, unregister};
use crate::window::{create_message_window, destroy_message_window, WM_USER_STOP};

/// Raw Input pen-capture backend for Windows.
pub struct RawInputBackend {
    thread: Option<JoinHandle<()>>,
    hwnd: Option<isize>,
    /// Ring-overflow counter (parity with the Wintab backend; real drops are
    /// counted by the consumer's ring — SPEC_BGC §5.2).
    pub drop_count: Arc<AtomicU64>,
}

impl RawInputBackend {
    /// Create a backend with default settings.
    pub fn new() -> Self {
        Self {
            thread: None,
            hwnd: None,
            drop_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Cumulative dropped-event count for metrics.
    pub fn dropped_packets(&self) -> u64 {
        self.drop_count.load(Ordering::Relaxed)
    }
}

impl Default for RawInputBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl TabletBackend for RawInputBackend {
    fn capabilities(&self) -> Result<DeviceCapabilities, BackendError> {
        let profiles = enumerate_digitizers();
        match profiles.values().next() {
            Some(profile) => Ok(capabilities_from_profile(&profile.caps, BATCH_REPORTS)),
            None => Err(BackendError::NoDevice),
        }
    }

    fn start(&mut self, sink: Box<dyn FnMut(SampleEvent) + Send>) -> Result<(), BackendError> {
        if self.thread.is_some() {
            warn!("RawInputBackend::start() called while already running; ignoring");
            return Ok(());
        }

        // Carry a start result back so `start()` can surface NoDevice / registration
        // failures synchronously instead of via a bare 0 HWND.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<isize, BackendError>>();
        let drop_count = Arc::clone(&self.drop_count);

        let handle = thread::Builder::new()
            .name("tablet-rawinput-capture".into())
            .spawn(move || capture_thread_main(sink, drop_count, ready_tx))
            .map_err(|e| BackendError::Transport(format!("thread spawn failed: {e}")))?;

        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(hwnd)) => {
                debug!(hwnd, "capture thread ready");
                self.hwnd = Some(hwnd);
                self.thread = Some(handle);
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                error!("timed out waiting for capture thread to start");
                let _ = handle.join();
                Err(BackendError::RawInputRegistrationFailed { win32_error: 0 })
            }
        }
    }

    fn stop(&mut self) -> Result<(), BackendError> {
        if let Some(hwnd) = self.hwnd.take() {
            // SAFETY: `hwnd` is valid until the capture thread (joined next) exits.
            let posted = unsafe { PostMessageW(hwnd as HWND, WM_USER_STOP, 0, 0) };
            if posted == 0 {
                warn!("PostMessageW(WM_USER_STOP) failed; window may have already exited");
            }
        }
        if let Some(handle) = self.thread.take() {
            match handle.join() {
                Ok(()) => debug!("capture thread joined cleanly"),
                Err(e) => error!("capture thread panicked: {e:?}"),
            }
        }
        Ok(())
    }
}

/// Capture-thread entry point: build window → enumerate → register → emit caps →
/// message loop, then unregister/destroy on exit.
fn capture_thread_main(
    sink: Box<dyn FnMut(SampleEvent) + Send>,
    drop_count: Arc<AtomicU64>,
    ready_tx: std::sync::mpsc::Sender<Result<isize, BackendError>>,
) {
    // 1. Message-only window (Raw Input target).
    // SAFETY: created on this thread, which owns the message loop.
    let hwnd = match unsafe { create_message_window() } {
        Ok(h) => h,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    // 2. Enumerate HID digitizers.
    let profiles = enumerate_digitizers();
    if profiles.is_empty() {
        warn!("no HID digitizer found (is 'Use Windows Ink' enabled?)");
        // SAFETY: hwnd just created on this thread.
        unsafe { destroy_message_window(hwnd) };
        let _ = ready_tx.send(Err(BackendError::NoDevice));
        return;
    }

    // 3. Register Raw Input (INPUTSINK | DEVNOTIFY).
    if let Err(e) = register(hwnd) {
        // SAFETY: hwnd valid on this thread.
        unsafe { destroy_message_window(hwnd) };
        let _ = ready_tx.send(Err(e));
        return;
    }

    // 4. Build capture state and emit the initial Capabilities handshake.
    let mut state = Box::new(CaptureState::new(profiles, sink, drop_count));
    if let Some(profile) = state.profiles.values().next() {
        let caps = capabilities_from_profile(&profile.caps, BATCH_REPORTS);
        (state.sink)(SampleEvent::Capabilities(caps));
    }

    // 5. Install state pointer in the window, then signal readiness.
    let state_ptr = Box::into_raw(state);
    // SAFETY: hwnd valid; state_ptr lives until reclaimed at teardown.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize) };

    if ready_tx.send(Ok(hwnd)).is_err() {
        warn!("main thread dropped receiver before window was ready");
        teardown(hwnd, state_ptr);
        return;
    }

    // 6. Message loop until WM_QUIT.
    info!("raw-input capture thread entering message loop");
    // SAFETY: standard Win32 message loop; messages dispatch to `wnd_proc`.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let ret = GetMessageW(&mut msg, hwnd, 0, 0);
            if ret == 0 {
                debug!("WM_QUIT received");
                break;
            }
            if ret == -1 {
                error!("GetMessageW error");
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // 7. Teardown.
    teardown(hwnd, state_ptr);
    info!("raw-input capture thread exiting");
}

/// Unregister Raw Input, reclaim and drop `CaptureState`, destroy the window.
fn teardown(hwnd: HWND, state_ptr: *mut CaptureState) {
    unregister();
    // SAFETY: state_ptr was installed via Box::into_raw and not freed elsewhere;
    // clear the slot first so any late WndProc call sees null and no-ops.
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CaptureState;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        if !ptr.is_null() {
            drop(Box::from_raw(ptr));
        } else if !state_ptr.is_null() {
            drop(Box::from_raw(state_ptr));
        }
        destroy_message_window(hwnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_construction() {
        let b = RawInputBackend::new();
        assert_eq!(b.dropped_packets(), 0);
        assert!(b.thread.is_none());
        assert!(b.hwnd.is_none());
    }

    /// `capabilities()` must return `NoDevice` (not panic) when no digitizer is
    /// present. On a machine with a tablet it may return Ok instead.
    #[test]
    fn capabilities_without_device_is_no_device_or_ok() {
        match RawInputBackend::new().capabilities() {
            Ok(_) | Err(BackendError::NoDevice) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
