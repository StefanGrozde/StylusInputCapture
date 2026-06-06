// tablet-wintab: Windows Wintab backend (cfg(windows) only)
// All unsafe FFI is isolated here behind safe wrappers.
//
// On non-Windows targets this crate compiles to an empty library.

#[cfg(windows)]
pub mod caps;
#[cfg(windows)]
pub mod ffi;

// context.rs and decode.rs are compiled on all platforms so that their
// cross-platform tests run in CI.  The Windows-specific items inside those
// modules are individually gated with #[cfg(windows)].
pub mod context;
pub mod decode;

// T3.3: capture thread, message-only window, and the public WintabBackend.
// All three are Windows-only (they use Win32 APIs).
#[cfg(windows)]
pub mod backend;
#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod window;

#[cfg(windows)]
pub use caps::query_capabilities;
#[cfg(windows)]
pub use ffi::WintabApi;

// context re-exports: build_logcontext is Windows-only; constants are universal.
#[cfg(windows)]
pub use context::build_logcontext;
pub use context::{
    CXO_MESSAGES, CXO_SYSTEM, FULL_PKT_DATA, PK_BUTTONS, PK_CHANGED, PK_CONTEXT, PK_CURSOR,
    PK_NORMAL_PRESSURE, PK_ORIENTATION, PK_ROTATION, PK_SERIAL_NUMBER, PK_STATUS,
    PK_TANGENT_PRESSURE, PK_TIME, PK_X, PK_Y, PK_Z,
};

// decode re-exports: cross-platform
pub use decode::{decode_packet, FullPacket, RawOrientation, RawRotation, RawXYZ};

// T3.3: public backend type
#[cfg(windows)]
pub use backend::WintabBackend;

// Re-export the error type so callers don't need a direct tablet-core dep.
pub use tablet_core::BackendError;
