//! `WintabBackend` — the public Wintab implementation of [`TabletBackend`].
//!
//! ## Lifecycle
//! 1. `capabilities()` — loads the DLL, verifies the interface, queries axes.
//!    May be called at any time without side effects.
//! 2. `start(sink)` — spawns the capture thread, which:
//!    a. loads the Wintab API,
//!    b. queries capabilities and emits `SampleEvent::Capabilities` via `sink`,
//!    c. builds a `LOGCONTEXT` and calls `WTOpen`,
//!    d. negotiates the queue size via `WTQueueSizeSet`,
//!    e. re-reads the actual `lcPktRate` via `WTGetA`,
//!    f. enters the Win32 message loop until `WM_USER_STOP` is received.
//! 3. `stop()` — posts `WM_USER_STOP` to the window and joins the thread.
//!
//! Thread-safety note: the `HWND` of the capture window is sent back to the
//! main thread over a `std::sync::mpsc` channel immediately after the window
//! is created, so `stop()` can post the quit message without races.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use tablet_core::{BackendError, DeviceCapabilities, SampleEvent, TabletBackend};
use tracing::{debug, error, info, warn};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageA, GetMessageA, GetWindowLongPtrA, PostMessageA, SetWindowLongPtrA,
    TranslateMessage, GWLP_USERDATA, MSG,
};

use crate::caps::query_capabilities;
use crate::capture::{
    context_open_failed, negotiate_queue_size, qpc_frequency, qpc_ns, refresh_pkt_rate,
    CaptureState,
};
use crate::context::build_logcontext;
use crate::ffi::{WintabApi, WTI_DEFCONTEXT};
use crate::window::{create_message_window, destroy_message_window, WM_USER_STOP};

// ---------------------------------------------------------------------------
// Class name for the message-only window (NUL-terminated)
// ---------------------------------------------------------------------------

/// Fixed class name.  Must be unique enough not to collide with other
/// applications' window classes within the same process.
const WINDOW_CLASS: &[u8] = b"WintabMsgWnd\0";

/// Desired Wintab queue depth (packets).  The driver will negotiate down if
/// needed; [`negotiate_queue_size`] handles the backoff.
const DESIRED_QUEUE_SIZE: i32 = 1024;

// ---------------------------------------------------------------------------
// Public backend struct
// ---------------------------------------------------------------------------

/// Wintab-based tablet backend for Windows.
///
/// Implements [`TabletBackend`] using the Wacom `Wintab32.dll` driver.
/// See module-level docs for the lifecycle sequence.
pub struct WintabBackend {
    /// When `true`, Y coordinates are flipped so the origin is top-left
    /// (screen convention) rather than Wintab's bottom-left.
    flip_y: bool,

    /// Desired Wintab queue size (packets).  The driver may negotiate lower.
    queue_size: i32,

    /// Handle to the capture thread, present while capture is active.
    thread: Option<JoinHandle<()>>,

    /// `HWND` of the message-only window, stored as `isize` (the natural ABI
    /// width).  Set after `start()` returns; used by `stop()` to post
    /// `WM_USER_STOP`.
    hwnd: Option<isize>,

    /// Shared drop counter; written by the capture thread, read by metrics.
    pub drop_count: Arc<AtomicU64>,
}

impl WintabBackend {
    /// Create a new `WintabBackend` with default settings.
    pub fn new() -> Self {
        Self {
            flip_y: false,
            queue_size: DESIRED_QUEUE_SIZE,
            thread: None,
            hwnd: None,
            drop_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Set Y-axis flip behaviour.
    pub fn with_flip_y(mut self, flip_y: bool) -> Self {
        self.flip_y = flip_y;
        self
    }

    /// Override the desired Wintab queue depth (packets).
    pub fn with_queue_size(mut self, size: i32) -> Self {
        self.queue_size = size;
        self
    }

    /// Read the cumulative drop count (serial-number gaps) from the last or
    /// current capture session.
    pub fn dropped_packets(&self) -> u64 {
        self.drop_count.load(Ordering::Relaxed)
    }
}

impl Default for WintabBackend {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// TabletBackend implementation
// ---------------------------------------------------------------------------

impl TabletBackend for WintabBackend {
    /// Query device capabilities without opening a capture context.
    ///
    /// Loads `Wintab32.dll`, verifies the interface is live, and queries all
    /// axis metadata.  Does not start capture.
    fn capabilities(&self) -> Result<DeviceCapabilities, BackendError> {
        let api = WintabApi::load()?;
        if !api.is_interface_live() {
            return Err(BackendError::DriverMissing);
        }
        query_capabilities(&api)
    }

    /// Spawn the capture thread and begin delivering events via `sink`.
    ///
    /// Returns once the capture window has been created and the message loop
    /// has started (signalled via an `mpsc` channel).  `sink` will receive:
    /// - `SampleEvent::Capabilities` — once at startup (before any samples).
    /// - `SampleEvent::Sample(PenSample)` — for every decoded pen packet.
    /// - `SampleEvent::Proximity` — on proximity change events.
    fn start(&mut self, sink: Box<dyn FnMut(SampleEvent) + Send>) -> Result<(), BackendError> {
        if self.thread.is_some() {
            warn!("WintabBackend::start() called while already running; ignoring");
            return Ok(());
        }

        // Channel: capture thread → main thread, carries the HWND.
        let (hwnd_tx, hwnd_rx) = std::sync::mpsc::channel::<isize>();

        let flip_y = self.flip_y;
        let queue_size = self.queue_size;
        let drop_count = Arc::clone(&self.drop_count);

        let handle = thread::Builder::new()
            .name("tablet-wintab-capture".into())
            .spawn(move || {
                capture_thread_main(sink, flip_y, queue_size, drop_count, hwnd_tx);
            })
            .map_err(|e| {
                error!("Failed to spawn capture thread: {e}");
                BackendError::Transport(format!("thread spawn failed: {e}"))
            })?;

        // Wait for the capture thread to create the window.  Use a generous
        // timeout — on slow systems window creation can take a few hundred ms.
        match hwnd_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(hwnd) if hwnd != 0 => {
                debug!(hwnd, "Capture thread window ready");
                self.hwnd = Some(hwnd);
                self.thread = Some(handle);
                Ok(())
            }
            Ok(_) => {
                // Thread sent 0 — window creation failed; join the thread.
                let _ = handle.join();
                Err(BackendError::ContextOpenFailed)
            }
            Err(_) => {
                error!("Timed out waiting for capture thread to create window");
                let _ = handle.join();
                Err(BackendError::ContextOpenFailed)
            }
        }
    }

    /// Stop the capture thread and release all OS resources.
    ///
    /// Posts `WM_USER_STOP` to the message-only window, which causes the
    /// message loop to exit.  The thread then performs cleanup (WTClose,
    /// DestroyWindow) before exiting.
    fn stop(&mut self) -> Result<(), BackendError> {
        if let Some(hwnd) = self.hwnd.take() {
            // SAFETY: `hwnd` was created by the capture thread and is valid
            // until the thread exits (which we join next).
            let posted = unsafe { PostMessageA(hwnd as HWND, WM_USER_STOP, 0, 0) };
            if posted == 0 {
                warn!("PostMessageA(WM_USER_STOP) failed; the window may have already exited");
            }
        }

        if let Some(handle) = self.thread.take() {
            match handle.join() {
                Ok(()) => debug!("Capture thread joined cleanly"),
                Err(e) => error!("Capture thread panicked: {e:?}"),
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Capture thread entry point
// ---------------------------------------------------------------------------

/// Main function of the capture thread.
///
/// Performs the full initialization sequence (API load → capabilities →
/// window → WTOpen → queue size → message loop) and the teardown sequence
/// on exit (WTClose → DestroyWindow).
///
/// On any fatal error before the message loop, sends `0` as the HWND so
/// `start()` can detect the failure.
fn capture_thread_main(
    mut sink: Box<dyn FnMut(SampleEvent) + Send>,
    flip_y: bool,
    queue_size: i32,
    drop_count: Arc<AtomicU64>,
    hwnd_tx: std::sync::mpsc::Sender<isize>,
) {
    // -------------------------------------------------------------------------
    // 1. Load Wintab API
    // -------------------------------------------------------------------------
    let api = match WintabApi::load() {
        Ok(a) => Arc::new(a),
        Err(e) => {
            error!("Capture thread: {e}");
            let _ = hwnd_tx.send(0);
            return;
        }
    };

    if !api.is_interface_live() {
        error!("Capture thread: Wintab interface not live (no tablet?)");
        let _ = hwnd_tx.send(0);
        return;
    }

    // -------------------------------------------------------------------------
    // 2. Query capabilities and emit the handshake event
    // -------------------------------------------------------------------------
    let mut caps = match query_capabilities(&api) {
        Ok(c) => c,
        Err(e) => {
            error!("Capture thread: {e}");
            let _ = hwnd_tx.send(0);
            return;
        }
    };
    // Will emit SampleEvent::Capabilities after WTOpen so queue_size is set.

    // -------------------------------------------------------------------------
    // 3. Build the LOGCONTEXT
    // -------------------------------------------------------------------------
    // Start from the system default context if available.
    let base_ctx = api.query::<wintab_lite::LOGCONTEXT>(WTI_DEFCONTEXT, 0);
    let mut ctx = build_logcontext(base_ctx, &caps);

    // -------------------------------------------------------------------------
    // 4. Create the message-only window
    // -------------------------------------------------------------------------
    // SAFETY: called from the capture thread, which will own the message loop.
    let hwnd = unsafe {
        match create_message_window(WINDOW_CLASS) {
            Ok(h) => h,
            Err(e) => {
                error!("Capture thread: {e}");
                let _ = hwnd_tx.send(0);
                return;
            }
        }
    };

    // -------------------------------------------------------------------------
    // 5. Open the Wintab context
    // -------------------------------------------------------------------------
    // SAFETY: `hwnd` is valid; `ctx` is a properly-configured LOGCONTEXT.
    let hctx = unsafe { api.open_context(hwnd as *mut std::ffi::c_void, &mut ctx, true) };
    if hctx.is_null() {
        let err = context_open_failed();
        error!("Capture thread: {err}");
        // SAFETY: hwnd was just successfully created above.
        unsafe { destroy_message_window(hwnd) };
        let _ = hwnd_tx.send(0);
        return;
    }
    info!("Wintab context opened");

    // -------------------------------------------------------------------------
    // 6. Negotiate queue size
    // -------------------------------------------------------------------------
    // SAFETY: hctx is a valid, open context.
    let actual_queue = unsafe { negotiate_queue_size(&api, hctx, queue_size) };
    caps.queue_size = actual_queue as u32;
    debug!(actual_queue, "Wintab queue negotiated");

    // -------------------------------------------------------------------------
    // 7. Re-read actual lcPktRate
    // -------------------------------------------------------------------------
    // SAFETY: hctx is a valid, open context.
    unsafe { refresh_pkt_rate(&api, hctx, &mut caps) };

    // Now that caps reflects actual queue size and rate, emit the handshake.
    sink(SampleEvent::Capabilities(caps.clone()));

    // -------------------------------------------------------------------------
    // 8. Cache QPC frequency
    // -------------------------------------------------------------------------
    let qpc_freq = qpc_frequency();

    // -------------------------------------------------------------------------
    // 9. Build CaptureState and attach it to the window via GWLP_USERDATA
    // -------------------------------------------------------------------------
    let state = Box::new(CaptureState {
        api: Arc::clone(&api),
        hctx,
        sink,
        caps,
        flip_y,
        last_serial: 0,
        drop_count,
        qpc_freq,
    });
    let state_ptr = Box::into_raw(state);

    // SAFETY: `hwnd` is valid; `state_ptr` is a heap-allocated, non-null
    // pointer that lives until we Box::from_raw it at the end.
    unsafe {
        SetWindowLongPtrA(hwnd, GWLP_USERDATA, state_ptr as isize);
    }

    // -------------------------------------------------------------------------
    // 10. Notify main thread that the window is ready
    // -------------------------------------------------------------------------
    if hwnd_tx.send(hwnd as isize).is_err() {
        // Main thread already gone; clean up and exit.
        warn!("Capture thread: main thread dropped receiver before window was ready");
        // Reclaim state before destroying.
        let state = unsafe { Box::from_raw(state_ptr) };
        unsafe { api.close_context(state.hctx) };
        drop(state);
        unsafe { destroy_message_window(hwnd) };
        return;
    }

    // -------------------------------------------------------------------------
    // 11. Message loop
    // -------------------------------------------------------------------------
    info!("Capture thread entering message loop");

    // Capture the timestamp at which the loop starts (informational).
    let _loop_start_ns = qpc_ns(qpc_freq);

    // SAFETY: standard Win32 message loop; all messages are dispatched to
    // `wnd_proc` which handles them safely.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        // GetMessageA returns 0 on WM_QUIT, -1 on error.
        loop {
            let ret = GetMessageA(&mut msg, hwnd, 0, 0);
            if ret == 0 {
                // WM_QUIT received — exit the loop.
                debug!("Capture thread: WM_QUIT received");
                break;
            }
            if ret == -1 {
                // Error: log and exit.
                error!("Capture thread: GetMessageA error");
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
    }

    // -------------------------------------------------------------------------
    // 12. Teardown
    // -------------------------------------------------------------------------
    info!("Capture thread tearing down");

    // Reclaim the CaptureState from the window's user-data slot.
    // SAFETY: We stored `state_ptr` there above and nothing else touches it.
    let state = unsafe {
        let ptr = GetWindowLongPtrA(hwnd, GWLP_USERDATA) as *mut CaptureState;
        // Clear the slot before deallocating, so any stray WndProc calls
        // after this point fall through to DefWindowProcA.
        SetWindowLongPtrA(hwnd, GWLP_USERDATA, 0);
        Box::from_raw(ptr)
    };

    // Close the Wintab context.
    // SAFETY: `state.hctx` is a valid, open context.
    let closed = unsafe { api.close_context(state.hctx) };
    if !closed {
        warn!("WTClose returned false; context may have already been invalidated");
    } else {
        debug!("Wintab context closed");
    }

    // Drop state (frees sink closure and caps).
    drop(state);

    // Destroy the message-only window.
    // SAFETY: `hwnd` was created on this thread and is still valid.
    unsafe { destroy_message_window(hwnd) };

    info!("Capture thread exiting");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Default construction must not panic and must report no dropped packets.
    #[test]
    fn default_construction() {
        let backend = WintabBackend::new();
        assert_eq!(backend.dropped_packets(), 0);
        assert!(backend.thread.is_none());
        assert!(backend.hwnd.is_none());
    }

    /// Builder methods must set the correct fields.
    #[test]
    fn builder_setters() {
        let backend = WintabBackend::new().with_flip_y(true).with_queue_size(512);
        assert!(backend.flip_y);
        assert_eq!(backend.queue_size, 512);
    }

    /// `capabilities()` on a system without Wintab32.dll must return
    /// `BackendError::DriverMissing` rather than panic.
    ///
    /// On a system with the Wintab driver this test may succeed instead.
    #[test]
    fn capabilities_without_driver_returns_driver_missing_or_ok() {
        let backend = WintabBackend::new();
        match backend.capabilities() {
            Ok(_caps) => {
                // Wintab driver is present and a tablet is connected.
                // This is fine; the test just verifies no panic.
            }
            Err(BackendError::DriverMissing) | Err(BackendError::NoDevice) => {
                // Expected on a machine without the Wacom driver or tablet.
            }
            Err(e) => {
                panic!("Unexpected error from capabilities(): {e}");
            }
        }
    }
}
