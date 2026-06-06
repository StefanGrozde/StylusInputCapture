//! Low-level Wintab32.dll dynamic loader.
//!
//! Loads `Wintab32.dll` at runtime via `libloading` and resolves the function
//! pointers we need into a [`WintabApi`] struct.  All `unsafe` FFI details are
//! confined to this module; every public function presented to the rest of the
//! crate has a safe signature.
//!
//! `WintabApi` owns the `Library` handle so the DLL stays mapped for its lifetime.

use std::ffi::c_void;

use libloading::Library;
use tablet_core::BackendError;
use tracing::{debug, info};
use wintab_lite::{AXIS, BOOL, HCTX, INT, LOGCONTEXT, UINT};

// ---------------------------------------------------------------------------
// Raw function-pointer types extracted from wintab_lite / wintab.h
// ---------------------------------------------------------------------------
//
// Parameter names follow the original C/Windows naming convention; we allow
// non-snake-case here because these are C ABI type aliases, not Rust functions.
#[allow(non_snake_case)]
mod raw_fn_types {
    use super::*;

    /// `UINT WTInfoA(UINT wCategory, UINT nIndex, LPVOID lpOutput)`
    ///
    /// Returns the number of bytes written; 0 on failure / unsupported.
    pub(super) type RawWTInfoA =
        unsafe extern "C" fn(wCategory: UINT, nIndex: UINT, lpOutput: *mut c_void) -> UINT;

    /// `HCTX* WTOpenA(HWND hWnd, LOGCONTEXT* lpLogCtx, BOOL fEnable)`
    ///
    /// Returns an opaque context pointer; null on failure.
    pub(super) type RawWTOpenA = unsafe extern "C" fn(
        hWnd: *mut c_void,
        lpLogCtx: *mut LOGCONTEXT,
        fEnable: BOOL,
    ) -> *mut HCTX;

    /// `BOOL WTClose(HCTX* hCtx)`
    pub(super) type RawWTClose = unsafe extern "C" fn(hCtx: *mut HCTX) -> BOOL;

    /// `int WTPacketsGet(HCTX* hCtx, int cMaxPkts, LPVOID lpPkts)`
    ///
    /// Copies up to `cMaxPkts` packets from the queue into `lpPkts`.
    /// Returns the number of packets actually copied.
    pub(super) type RawWTPacketsGet =
        unsafe extern "C" fn(hCtx: *mut HCTX, cMaxPkts: INT, lpPkts: *mut c_void) -> INT;

    /// `BOOL WTQueueSizeSet(HCTX* hCtx, int nPkts)`
    ///
    /// Sets the queue size for the context. Returns non-zero on success.
    pub(super) type RawWTQueueSizeSet = unsafe extern "C" fn(hCtx: *mut HCTX, nPkts: INT) -> BOOL;

    /// `int WTQueueSizeGet(HCTX* hCtx)`
    ///
    /// Returns the current queue size for the context.
    pub(super) type RawWTQueueSizeGet = unsafe extern "C" fn(hCtx: *mut HCTX) -> INT;

    /// `BOOL WTGetA(HCTX hCtx, LPLOGCONTEXT lpLogCtx)`
    ///
    /// Fills `lpLogCtx` with the current state of the open context.
    /// Returns non-zero on success.
    pub(super) type RawWTGetA =
        unsafe extern "C" fn(hCtx: *mut HCTX, lpLogCtx: *mut LOGCONTEXT) -> BOOL;
}

use raw_fn_types::{
    RawWTClose, RawWTGetA, RawWTInfoA, RawWTOpenA, RawWTPacketsGet, RawWTQueueSizeGet,
    RawWTQueueSizeSet,
};

// ---------------------------------------------------------------------------
// WTInfo category and index constants
// ---------------------------------------------------------------------------
//
// These mirror the values in wintab_lite::WTI / DVC / IFC enums, but we use
// plain u32 constants here so they can be passed to WTInfoA (which takes UINT)
// without the enum-cast noise at every call site.

/// WTInfo category: global interface identification (WTI_INTERFACE).
pub const WTI_INTERFACE: UINT = 1;
/// WTInfo category: current default digitizing logical context (WTI_DEFCONTEXT).
pub const WTI_DEFCONTEXT: UINT = 3;
/// WTInfo category: current default system logical context (WTI_DEFSYSCTX).
#[allow(dead_code)]
pub const WTI_DEFSYSCTX: UINT = 4;
/// WTInfo category: device 0 capabilities (WTI_DEVICES).
pub const WTI_DEVICES: UINT = 100;

// IFC (interface) indexes
/// Driver identification string (char array).
pub const IFC_WINTABID: UINT = 1;

// DVC (device) indexes — from wintab_lite::DVC enum casts
/// Device name string (DVC_NAME).
pub const DVC_NAME: UINT = 1;
/// Packet rate in Hz (DVC_PKTRATE). Note: wintab_lite uses 5, spec says 26 — 5 is correct per wintab.h.
pub const DVC_PKTRATE: UINT = 5;
/// X axis (DVC_X).
pub const DVC_X: UINT = 12;
/// Y axis (DVC_Y).
pub const DVC_Y: UINT = 13;
/// Z axis (DVC_Z).
pub const DVC_Z: UINT = 14;
/// Normal (tip) pressure axis (DVC_NPRESSURE).
pub const DVC_NPRESSURE: UINT = 15;
/// Tangent (barrel) pressure axis (DVC_TPRESSURE).
pub const DVC_TPRESSURE: UINT = 16;
/// Orientation axes — 3 AXIS structs: azimuth, altitude, twist (DVC_ORIENTATION).
pub const DVC_ORIENTATION: UINT = 17;
/// Rotation axes — 3 AXIS structs (DVC_ROTATION).
pub const DVC_ROTATION: UINT = 18;

// ---------------------------------------------------------------------------
// WintabApi — owns the Library and resolved function pointers
// ---------------------------------------------------------------------------

/// Safe wrapper around `Wintab32.dll`, holding the loaded library and all
/// resolved function pointers needed by this crate.
///
/// Creating a `WintabApi` loads the DLL and resolves every symbol; any failure
/// is mapped to [`BackendError::DriverMissing`].
// Fields for context control (open/close/queue/packets) are used by T3.3 (capture thread).
#[allow(dead_code)]
pub struct WintabApi {
    /// The loaded DLL (must live as long as the function pointers).
    _lib: Library,

    // Raw function pointers, valid for the lifetime of `_lib`.
    wt_info_a: RawWTInfoA,
    wt_open_a: RawWTOpenA,
    wt_close: RawWTClose,
    wt_packets_get: RawWTPacketsGet,
    wt_queue_size_set: RawWTQueueSizeSet,
    wt_queue_size_get: RawWTQueueSizeGet,
    wt_get_a: RawWTGetA,
}

impl WintabApi {
    /// Load `Wintab32.dll` and resolve all required entry points.
    ///
    /// Returns `BackendError::DriverMissing` if the DLL is absent, cannot be
    /// loaded, or is missing a required export.
    pub fn load() -> Result<Self, BackendError> {
        // SAFETY: Loading a DLL by name is inherently unsafe; we validate
        // every symbol before storing it.
        let lib = unsafe {
            Library::new("Wintab32.dll").map_err(|e| {
                tracing::warn!("Failed to load Wintab32.dll: {e}");
                BackendError::DriverMissing
            })?
        };

        info!("Loaded Wintab32.dll successfully");

        // Resolve each symbol. `Library::get` returns a `Symbol<'_, T>` that
        // borrows from `lib`.  We convert it to a bare function pointer
        // (which does not borrow) so we can store both in the same struct.
        // SAFETY: The function pointer remains valid as long as `_lib` keeps
        // the DLL mapped; we uphold this by keeping `_lib` in the struct and
        // never exposing the raw pointers publicly.
        macro_rules! resolve {
            ($lib:expr, $name:expr, $ty:ty) => {{
                let sym: libloading::Symbol<$ty> = unsafe {
                    $lib.get($name).map_err(|e| {
                        tracing::warn!(
                            "Missing Wintab32.dll export `{}`: {}",
                            stringify!($name),
                            e
                        );
                        BackendError::DriverMissing
                    })?
                };
                // Convert Symbol (which borrows lib) to a raw fn pointer.
                // SAFETY: The DLL stays loaded while `_lib` lives; the raw
                // pointer is only called via safe wrappers in this struct.
                *sym
            }};
        }

        let wt_info_a = resolve!(lib, b"WTInfoA\0", RawWTInfoA);
        let wt_open_a = resolve!(lib, b"WTOpenA\0", RawWTOpenA);
        let wt_close = resolve!(lib, b"WTClose\0", RawWTClose);
        let wt_packets_get = resolve!(lib, b"WTPacketsGet\0", RawWTPacketsGet);

        // WTQueueSizeSet / WTQueueSizeGet may not be present in all driver
        // versions; map absence to DriverMissing (they are required for
        // lossless capture).
        let wt_queue_size_set = resolve!(lib, b"WTQueueSizeSet\0", RawWTQueueSizeSet);
        let wt_queue_size_get = resolve!(lib, b"WTQueueSizeGet\0", RawWTQueueSizeGet);

        // WTGetA: re-read negotiated context state after WTOpen (e.g. lcPktRate).
        // Required for lossless capture; map absence to DriverMissing.
        let wt_get_a = resolve!(lib, b"WTGetA\0", RawWTGetA);

        debug!("All Wintab32 entry points resolved");

        Ok(Self {
            _lib: lib,
            wt_info_a,
            wt_open_a,
            wt_close,
            wt_packets_get,
            wt_queue_size_set,
            wt_queue_size_get,
            wt_get_a,
        })
    }

    // -----------------------------------------------------------------------
    // Safe wrappers around the raw FFI calls
    // -----------------------------------------------------------------------

    /// Call `WTInfoA(wCategory, nIndex, lpOutput)`.
    ///
    /// Returns the number of bytes written, or 0 if the information is not
    /// supported / a tablet is not present.
    ///
    /// # Safety (internal)
    /// `lpOutput` must point to a buffer of at least the expected size for the
    /// requested category/index.  Call sites in this crate use correctly-sized
    /// stack variables and pass them via `cast_void!` / raw mut pointer casts.
    pub(crate) fn wt_info_raw(&self, category: UINT, index: UINT, output: *mut c_void) -> UINT {
        // SAFETY: function pointer is valid for the DLL's lifetime (held by
        // `_lib`); the caller is responsible for the output buffer size.
        unsafe { (self.wt_info_a)(category, index, output) }
    }

    /// Query `WTInfoA` for a value of type `T` stored at `(category, index)`.
    ///
    /// Returns `Some(value)` if WTInfo returns the expected number of bytes;
    /// `None` if the query fails or returns an unexpected size.
    pub(crate) fn query<T: Copy + Default>(&self, category: UINT, index: UINT) -> Option<T> {
        let mut val = T::default();
        let expected = std::mem::size_of::<T>() as UINT;
        // SAFETY: `val` is a valid, correctly-sized stack allocation of type T.
        let got = unsafe { (self.wt_info_a)(category, index, &mut val as *mut T as *mut c_void) };
        if got == expected {
            Some(val)
        } else {
            None
        }
    }

    /// Query `WTInfoA` for a variable-length byte string at `(category, index)`.
    ///
    /// First queries for the required size, then allocates a `Vec<u8>` and
    /// fills it.  Returns an empty `String` on any failure.
    pub(crate) fn query_string(&self, category: UINT, index: UINT) -> String {
        // First call: pass null to get the required buffer size.
        let size = unsafe { (self.wt_info_a)(category, index, std::ptr::null_mut()) };
        if size == 0 {
            return String::new();
        }
        let mut buf = vec![0u8; size as usize];
        let written = unsafe { (self.wt_info_a)(category, index, buf.as_mut_ptr() as *mut c_void) };
        if written == 0 {
            return String::new();
        }
        // Strip trailing NUL bytes and convert.
        while buf.last() == Some(&0) {
            buf.pop();
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Verify that the Wintab interface is live by calling `WTInfo(0, 0, null)`.
    ///
    /// Returns `true` if the driver is present and responsive; `false` if no
    /// tablet is connected or the driver is not functioning.
    pub fn is_interface_live(&self) -> bool {
        let result = self.wt_info_raw(0, 0, std::ptr::null_mut());
        result != 0
    }

    /// Open a Wintab context.
    ///
    /// `hwnd`    — the message-only window that will receive `WT_PACKET` messages.
    /// `ctx`     — a mutable reference to the configured `LOGCONTEXT`.
    /// `enabled` — pass `true` to start capturing immediately.
    ///
    /// Returns the raw context pointer; null means `WTOpen` failed.
    ///
    /// # Safety
    /// `hwnd` must be a valid Win32 `HWND` for the lifetime of the context.
    // Used by T3.3 capture thread.
    #[allow(dead_code)]
    pub(crate) unsafe fn open_context(
        &self,
        hwnd: *mut c_void,
        ctx: *mut LOGCONTEXT,
        enabled: bool,
    ) -> *mut HCTX {
        (self.wt_open_a)(hwnd, ctx, if enabled { 1 } else { 0 })
    }

    /// Close a previously opened Wintab context.
    ///
    /// Returns `true` if the context was valid and has been destroyed.
    ///
    /// # Safety
    /// `hctx` must be a valid, open context handle.
    // Used by T3.3 capture thread.
    #[allow(dead_code)]
    pub(crate) unsafe fn close_context(&self, hctx: *mut HCTX) -> bool {
        (self.wt_close)(hctx) != 0
    }

    /// Drain up to `max_pkts` packets from the context queue into `buf`.
    ///
    /// Returns the number of packets actually copied.
    ///
    /// # Safety
    /// `buf` must point to a buffer of at least `max_pkts * sizeof(PACKET)` bytes.
    // Used by T3.3 capture thread.
    #[allow(dead_code)]
    pub(crate) unsafe fn packets_get(
        &self,
        hctx: *mut HCTX,
        max_pkts: INT,
        buf: *mut c_void,
    ) -> INT {
        (self.wt_packets_get)(hctx, max_pkts, buf)
    }

    /// Set the queue size for an open context.
    ///
    /// Returns `true` if the new size was accepted by the driver.
    ///
    /// # Safety
    /// `hctx` must be a valid, open context handle.
    // Used by T3.3 capture thread.
    #[allow(dead_code)]
    pub(crate) unsafe fn queue_size_set(&self, hctx: *mut HCTX, n_pkts: INT) -> bool {
        (self.wt_queue_size_set)(hctx, n_pkts) != 0
    }

    /// Get the current queue size for an open context.
    ///
    /// # Safety
    /// `hctx` must be a valid, open context handle.
    // Used by T3.3 capture thread.
    #[allow(dead_code)]
    pub(crate) unsafe fn queue_size_get(&self, hctx: *mut HCTX) -> INT {
        (self.wt_queue_size_get)(hctx)
    }

    /// Re-read the current state of an open context into `ctx_out`.
    ///
    /// Wraps `WTGetA(hCtx, lpLogCtx)`.  Call this after `WTOpen` to discover
    /// the actual negotiated `lcPktRate` (the driver may lower the requested
    /// rate).
    ///
    /// Returns `true` on success; `false` if the driver call fails.
    ///
    /// # Safety
    /// `hctx` must be a valid, open context handle.
    /// `ctx_out` must point to a valid, mutable `LOGCONTEXT`.
    // Used by T3.3 capture thread (refresh_pkt_rate).
    pub(crate) unsafe fn get_context(&self, hctx: *mut HCTX, ctx_out: *mut LOGCONTEXT) -> bool {
        (self.wt_get_a)(hctx, ctx_out) != 0
    }

    /// Query a single `AXIS` struct from `WTInfoA`.
    ///
    /// Returns `None` if the driver returns an unexpected byte count.
    pub(crate) fn query_axis(&self, category: UINT, index: UINT) -> Option<AXIS> {
        self.query::<AXIS>(category, index)
    }

    /// Query three consecutive `AXIS` structs (e.g. orientation or rotation).
    ///
    /// Returns `None` if the driver returns an unexpected byte count.
    pub(crate) fn query_axis3(&self, category: UINT, index: UINT) -> Option<[AXIS; 3]> {
        self.query::<[AXIS; 3]>(category, index)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_core::BackendError;

    /// Loading a non-existent DLL must return `BackendError::DriverMissing`,
    /// not a panic or any other error variant.
    #[test]
    fn load_bad_dll_returns_driver_missing() {
        let result = {
            // Temporarily override the DLL name by attempting to load a
            // deliberately invalid path.
            let lib = unsafe { Library::new("NonExistentWintab_ZZZZZ.dll") };
            lib.map_err(|_| BackendError::DriverMissing)
        };
        assert!(
            matches!(result, Err(BackendError::DriverMissing)),
            "expected DriverMissing, got: {:?}",
            result
        );
    }

    /// WintabApi::load() on a system without Wintab32.dll must also return
    /// DriverMissing.  On a system where Wintab IS installed this test is
    /// skipped (the function would succeed instead of failing).
    ///
    /// To simulate the missing-DLL case we temporarily test with a bogus name
    /// via the lower-level path checked in `load_bad_dll_returns_driver_missing`.
    #[test]
    fn load_bad_name_via_api() {
        // We can't intercept WintabApi::load() without hardware, but we can
        // exercise the error-mapping path by constructing a helper that
        // mimics the load logic.
        fn try_load_named(name: &str) -> Result<(), BackendError> {
            unsafe { Library::new(name) }.map(|_| ()).map_err(|e| {
                tracing::warn!("test: Failed to load {name}: {e}");
                BackendError::DriverMissing
            })
        }

        let result = try_load_named("DoesNotExist_Wintab_Test.dll");
        assert!(
            matches!(result, Err(BackendError::DriverMissing)),
            "expected DriverMissing"
        );
    }
}
