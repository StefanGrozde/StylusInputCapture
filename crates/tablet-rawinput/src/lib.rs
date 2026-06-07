//! `tablet-rawinput` — focus-independent Windows pen capture via **Raw Input +
//! HID** (SPEC_BGC).
//!
//! Unlike Wintab, Raw Input (`RIDEV_INPUTSINK`) delivers a copy of the pen's HID
//! reports to our message-only window **even while another application owns the
//! foreground** — which is the defining requirement of this project (capture
//! while a DAW is focused).
//!
//! # Layout
//! - [`hid`] — HID usage-page / usage constants (pure, cross-platform).
//! - [`caps`] — HID capability model (`ProfileCaps`) and the
//!   [`DeviceCapabilities`](tablet_core::DeviceCapabilities) builder. The pure
//!   model compiles everywhere; live preparsed-data parsing is Windows-only.
//! - [`decode`] — the **pure** HID-report → [`PenSample`](tablet_core::PenSample)
//!   decoder. Takes a [`decode::UsageReader`] + caps, no OS handles, so it is
//!   fully unit-testable on any OS.
//! - `enumerate` / `register` / `capture` / `window` / `backend` — Windows-only
//!   live capture machinery (all `unsafe` Win32/HID FFI is isolated here behind
//!   safe wrappers).
//!
//! All `unsafe` FFI stays out of public function signatures.

pub mod caps;
pub mod decode;
pub mod hid;

#[cfg(windows)]
pub mod backend;
#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod enumerate;
#[cfg(windows)]
pub mod register;
#[cfg(windows)]
pub mod window;

pub use caps::{capabilities_from_profile, AxisRange, ProfileCaps};
pub use decode::{decode_report, UsageReader};

#[cfg(windows)]
pub use backend::RawInputBackend;

// Re-export the error type so callers don't need a direct tablet-core dep.
pub use tablet_core::BackendError;
