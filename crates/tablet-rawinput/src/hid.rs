//! HID usage-page / usage constants and the Raw Input message constants used by
//! the backend (SPEC_BGC §4, §6.1).
//!
//! These are plain integer constants with no OS dependency, so they compile and
//! are usable from the pure decode path on any platform.

// ---------------------------------------------------------------------------
// HID usage pages
// ---------------------------------------------------------------------------

/// Generic Desktop usage page (carries X / Y for the digitizer).
pub const PAGE_GENERIC_DESKTOP: u16 = 0x01;
/// Digitizer usage page (pressure, tilt, twist, switches, in-range, …).
pub const PAGE_DIGITIZER: u16 = 0x0D;

// ---------------------------------------------------------------------------
// Generic Desktop usages (page 0x01)
// ---------------------------------------------------------------------------

/// X position (Generic Desktop).
pub const USAGE_X: u16 = 0x30;
/// Y position (Generic Desktop).
pub const USAGE_Y: u16 = 0x31;

// ---------------------------------------------------------------------------
// Digitizer top-level collection usages (registered with Raw Input, §4)
// ---------------------------------------------------------------------------

/// Top-level Digitizer collection usage.
pub const TLC_DIGITIZER: u16 = 0x01;
/// Top-level Pen collection usage.
pub const TLC_PEN: u16 = 0x02;

// ---------------------------------------------------------------------------
// Digitizer usages (page 0x0D)
// ---------------------------------------------------------------------------

/// Tip pressure.
pub const USAGE_TIP_PRESSURE: u16 = 0x30;
/// Barrel (secondary) pressure — airbrush wheel / tangent pressure.
pub const USAGE_BARREL_PRESSURE: u16 = 0x31;
/// In Range (proximity) — a button-type usage.
pub const USAGE_IN_RANGE: u16 = 0x32;
/// Invert (eraser end is toward the surface).
pub const USAGE_INVERT: u16 = 0x3C;
/// X Tilt (signed degrees).
pub const USAGE_X_TILT: u16 = 0x3D;
/// Y Tilt (signed degrees).
pub const USAGE_Y_TILT: u16 = 0x3E;
/// Azimuth (in-plane compass angle) — alternative to X/Y tilt on some devices.
pub const USAGE_AZIMUTH: u16 = 0x3F;
/// Altitude (angle from the tablet plane) — paired with azimuth.
pub const USAGE_ALTITUDE: u16 = 0x40;
/// Twist (barrel rotation about the pen axis).
pub const USAGE_TWIST: u16 = 0x41;
/// Tip switch (pen tip pressed).
pub const USAGE_TIP_SWITCH: u16 = 0x42;
/// Barrel switch (side button).
pub const USAGE_BARREL_SWITCH: u16 = 0x44;
/// Eraser switch (eraser end pressed).
pub const USAGE_ERASER: u16 = 0x45;

// ---------------------------------------------------------------------------
// `PenSample.buttons` bit assignments (SPEC_BGC §6.1)
// ---------------------------------------------------------------------------

/// Bit 0: tip switch.
pub const BUTTON_BIT_TIP: u32 = 0;
/// Bit 1: barrel switch.
pub const BUTTON_BIT_BARREL: u32 = 1;
/// Bit 2: eraser switch.
pub const BUTTON_BIT_ERASER: u32 = 2;
