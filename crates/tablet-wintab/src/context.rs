//! LOGCONTEXT full-resolution configuration (SPEC §6.2, §6.3).
//!
//! [`build_logcontext`] constructs a Wintab `LOGCONTEXT` that requests:
//! - all supported packet fields (`FULL_PKT_DATA`),
//! - absolute mode for all axes (`lcPktMode = 0`),
//! - a 1:1 input/output extent so coordinates arrive in native digitizer units
//!   rather than being scaled to screen pixels, and
//! - `WT_PACKET` message delivery to our window without perturbing the system
//!   cursor.
//!
//! Accepting an `Option<LOGCONTEXT>` as the base (rather than calling WTInfo
//! internally) lets the override logic be tested cross-platform without a DLL.

#[cfg(windows)]
use wintab_lite::{Bitmask, CXO, LOGCONTEXT, WTPKT, XYZ};

use tablet_core::DeviceCapabilities;

// ---------------------------------------------------------------------------
// WTPKT / PK_* flag constants (mirror wintab_lite::WTPKT bit values)
// ---------------------------------------------------------------------------
//
// wintab_lite exposes these as `WTPKT` bitflags, but we also export plain u32
// constants so callers can compose/inspect the mask without importing the type.

/// Reporting context handle field.
pub const PK_CONTEXT: u32 = 0x0001;
/// Status bits (proximity, inverted, etc.).
pub const PK_STATUS: u32 = 0x0002;
/// Device timestamp (milliseconds).
pub const PK_TIME: u32 = 0x0004;
/// Changed-fields vector.
pub const PK_CHANGED: u32 = 0x0008;
/// Packet serial number (gap detection).
pub const PK_SERIAL_NUMBER: u32 = 0x0010;
/// Cursor / tool identifier.
pub const PK_CURSOR: u32 = 0x0020;
/// Button bitmask.
pub const PK_BUTTONS: u32 = 0x0040;
/// X-axis position.
pub const PK_X: u32 = 0x0080;
/// Y-axis position.
pub const PK_Y: u32 = 0x0100;
/// Z-axis position.
pub const PK_Z: u32 = 0x0200;
/// Tip / normal pressure.
pub const PK_NORMAL_PRESSURE: u32 = 0x0400;
/// Barrel / tangent pressure (airbrush wheel).
pub const PK_TANGENT_PRESSURE: u32 = 0x0800;
/// Azimuth + altitude + twist orientation.
pub const PK_ORIENTATION: u32 = 0x1000;
/// Rotation (Wintab 1.1).
pub const PK_ROTATION: u32 = 0x2000;

/// Full packet-data mask: bitwise OR of every WTPKT flag above.
///
/// Use this as `lcPktData` when opening a context to request all supported
/// fields.  Fields that the current device/driver does not support are simply
/// absent from actual packets; the driver does not fail `WTOpen` for them.
pub const FULL_PKT_DATA: u32 = PK_CONTEXT
    | PK_STATUS
    | PK_TIME
    | PK_CHANGED
    | PK_SERIAL_NUMBER
    | PK_CURSOR
    | PK_BUTTONS
    | PK_X
    | PK_Y
    | PK_Z
    | PK_NORMAL_PRESSURE
    | PK_TANGENT_PRESSURE
    | PK_ORIENTATION
    | PK_ROTATION;

// ---------------------------------------------------------------------------
// CXO_* context-option constants
// ---------------------------------------------------------------------------
//
// wintab_lite exports these as the `CXO` bitflags type.  We additionally
// provide plain-u32 views for documentation / cross-platform test assertions.

/// CXO_SYSTEM: context moves the system cursor. We **clear** this bit so
/// the app captures without perturbing the desktop pointer.
pub const CXO_SYSTEM: u32 = 0x0001;
/// CXO_MESSAGES: deliver `WT_PACKET` messages to the owning window. We
/// **set** this bit so our message loop receives packet notifications.
pub const CXO_MESSAGES: u32 = 0x0004;

// ---------------------------------------------------------------------------
// Pure override logic (cross-platform)
// ---------------------------------------------------------------------------
//
// `apply_full_resolution_overrides` contains every field override required by
// SPEC §6.3 expressed in terms of wintab_lite types.  It is called by
// `build_logcontext` on Windows (which can supply a real default context)
// and by tests on any OS (which supply a zeroed/default context).

/// Apply SPEC §6.3 overrides to a mutable `LOGCONTEXT`.
///
/// The caller provides the starting context (either from `WTI_DEFCONTEXT` or
/// a default-initialized struct); this function mutates it in place and
/// returns a reference for convenience.
///
/// # Overrides applied
/// - `lcOptions`: clear `CXO_SYSTEM`, set `CXO_MESSAGES`.
/// - `lcPktData`: full superset mask (`FULL_PKT_DATA`).
/// - `lcPktMode`: `0` — absolute mode for all axes.
/// - `lcMoveMask`: same as `lcPktData`.
/// - `lcBtnDnMask` / `lcBtnUpMask`: `0xFFFF`.
/// - `lcInOrgXYZ` / `lcOutOrgXYZ`: origin at (0, 0, 0).
/// - `lcInExtXYZ` / `lcOutExtXYZ`: extent set to `(caps.x.max, caps.y.max, 0)`,
///   producing a **1:1 mapping** so coordinates stay in native digitizer units.
/// - `lcPktRate`: request `caps.max_packet_rate_hz`.
#[cfg(windows)]
pub fn apply_overrides(ctx: &mut LOGCONTEXT, caps: &DeviceCapabilities) {
    // Context options: do NOT move the system cursor; DO deliver WT_PACKET.
    ctx.lcOptions.remove(CXO::SYSTEM);
    ctx.lcOptions.insert(CXO::MESSAGES);

    // Request the full packet field superset.
    ctx.lcPktData = WTPKT::all();

    // All axes in absolute mode (lcPktMode bits clear = absolute for that axis).
    ctx.lcPktMode = WTPKT::empty();

    // Generate move events for every packet field.
    ctx.lcMoveMask = WTPKT::all();

    // Process all button press and release events.
    ctx.lcBtnDnMask = Bitmask(0xFFFF);
    ctx.lcBtnUpMask = Bitmask(0xFFFF);

    // 1:1 mapping: input and output extents are identical so no scaling occurs.
    // Origin at (0, 0, 0); extents cover the full device range.
    let x_max = caps.x.max as i32;
    let y_max = caps.y.max as i32;

    ctx.lcInOrgXYZ = XYZ { x: 0, y: 0, z: 0 };
    ctx.lcInExtXYZ = XYZ {
        x: x_max,
        y: y_max,
        z: 0,
    };
    ctx.lcOutOrgXYZ = XYZ { x: 0, y: 0, z: 0 };
    ctx.lcOutExtXYZ = XYZ {
        x: x_max,
        y: y_max,
        z: 0,
    };

    // Request the maximum hardware packet rate.
    ctx.lcPktRate = caps.max_packet_rate_hz as u32;
}

// ---------------------------------------------------------------------------
// Public API (Windows only — requires WTInfo for default context)
// ---------------------------------------------------------------------------

/// Build a fully-configured `LOGCONTEXT` ready to pass to `WTOpen`.
///
/// Starts from the provided `base` context (typically obtained from
/// `WTI_DEFCONTEXT` via [`WintabApi::query`]; pass `None` to use a
/// zeroed default) and applies every SPEC §6.3 override via
/// [`apply_overrides`].
///
/// After `WTOpen` succeeds, re-read `lcPktRate` from the returned context
/// to discover the actual negotiated rate (the driver may lower it).
///
/// [`WintabApi::query`]: crate::ffi::WintabApi::query
#[cfg(windows)]
pub fn build_logcontext(base: Option<LOGCONTEXT>, caps: &DeviceCapabilities) -> LOGCONTEXT {
    let mut ctx = base.unwrap_or_default();
    apply_overrides(&mut ctx, caps);
    ctx
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities};

    /// Build a minimal `DeviceCapabilities` with the given x/y extents for use
    /// in tests.  All other axes are zeroed/unsupported, which is fine since
    /// the override logic only reads `x.max`, `y.max`, and
    /// `max_packet_rate_hz`.
    fn make_caps(x_max: i64, y_max: i64, rate_hz: u32) -> DeviceCapabilities {
        let axis = |max: i64| AxisInfo {
            min: 0,
            max,
            resolution: 1.0,
            unit: AxisUnit::None,
            supported: true,
        };
        let unsupported = AxisInfo {
            min: 0,
            max: 0,
            resolution: 0.0,
            unit: AxisUnit::None,
            supported: false,
        };
        DeviceCapabilities {
            device_name: "Test".into(),
            driver_version: "0.0".into(),
            x: axis(x_max),
            y: axis(y_max),
            z: unsupported,
            pressure: unsupported,
            tangent_pressure: unsupported,
            azimuth: unsupported,
            altitude: unsupported,
            twist: unsupported,
            rotation: unsupported,
            max_packet_rate_hz: rate_hz,
            queue_size: 0,
        }
    }

    // -----------------------------------------------------------------
    // Cross-platform: assert the constant values are correct.
    // -----------------------------------------------------------------

    #[test]
    fn full_pkt_data_value() {
        // The FULL_PKT_DATA constant must equal the bitwise OR of all 14 flags.
        let expected: u32 = PK_CONTEXT
            | PK_STATUS
            | PK_TIME
            | PK_CHANGED
            | PK_SERIAL_NUMBER
            | PK_CURSOR
            | PK_BUTTONS
            | PK_X
            | PK_Y
            | PK_Z
            | PK_NORMAL_PRESSURE
            | PK_TANGENT_PRESSURE
            | PK_ORIENTATION
            | PK_ROTATION;
        assert_eq!(FULL_PKT_DATA, expected, "FULL_PKT_DATA mismatch");
    }

    #[test]
    fn pk_flags_are_distinct_powers_of_two() {
        let flags = [
            PK_CONTEXT,
            PK_STATUS,
            PK_TIME,
            PK_CHANGED,
            PK_SERIAL_NUMBER,
            PK_CURSOR,
            PK_BUTTONS,
            PK_X,
            PK_Y,
            PK_Z,
            PK_NORMAL_PRESSURE,
            PK_TANGENT_PRESSURE,
            PK_ORIENTATION,
            PK_ROTATION,
        ];
        for &f in &flags {
            assert!(f.is_power_of_two(), "flag {:#06x} is not a power of two", f);
        }
        // All flags must be distinct.
        let mut seen = 0u32;
        for &f in &flags {
            assert_eq!(seen & f, 0, "flag {:#06x} appears more than once", f);
            seen |= f;
        }
    }

    #[test]
    fn cxo_constants_are_correct() {
        assert_eq!(CXO_SYSTEM, 0x0001);
        assert_eq!(CXO_MESSAGES, 0x0004);
    }

    // -----------------------------------------------------------------
    // Windows-only: assert the LOGCONTEXT override results.
    // -----------------------------------------------------------------

    /// The override test validates every field without touching real hardware.
    /// It supplies a zeroed `LOGCONTEXT` as the base (equivalent to calling
    /// `build_logcontext(None, &caps)`) so no WTInfo call is needed.
    #[cfg(windows)]
    #[test]
    fn logcontext_overrides_are_correct() {
        let caps = make_caps(65536, 49152, 200);
        let ctx = build_logcontext(None, &caps);

        // 1:1 mapping: input extent must equal output extent.
        assert_eq!(ctx.lcInExtXYZ.x, ctx.lcOutExtXYZ.x, "X extents differ");
        assert_eq!(ctx.lcInExtXYZ.y, ctx.lcOutExtXYZ.y, "Y extents differ");

        // Input extents must match the device max values.
        assert_eq!(ctx.lcInExtXYZ.x, 65536_i32, "lcInExtXYZ.x wrong");
        assert_eq!(ctx.lcInExtXYZ.y, 49152_i32, "lcInExtXYZ.y wrong");

        // Output extents must match as well (1:1).
        assert_eq!(ctx.lcOutExtXYZ.x, 65536_i32, "lcOutExtXYZ.x wrong");
        assert_eq!(ctx.lcOutExtXYZ.y, 49152_i32, "lcOutExtXYZ.y wrong");

        // Origins must be zero.
        assert_eq!(ctx.lcInOrgXYZ.x, 0);
        assert_eq!(ctx.lcInOrgXYZ.y, 0);
        assert_eq!(ctx.lcOutOrgXYZ.x, 0);
        assert_eq!(ctx.lcOutOrgXYZ.y, 0);

        // CXO_SYSTEM must be cleared; CXO_MESSAGES must be set.
        assert!(
            !ctx.lcOptions.contains(CXO::SYSTEM),
            "CXO_SYSTEM should be cleared"
        );
        assert!(
            ctx.lcOptions.contains(CXO::MESSAGES),
            "CXO_MESSAGES should be set"
        );

        // lcPktData must be the full superset.
        assert_eq!(
            ctx.lcPktData,
            WTPKT::all(),
            "lcPktData should be WTPKT::all()"
        );

        // lcPktMode must be empty (all axes absolute).
        assert_eq!(
            ctx.lcPktMode,
            WTPKT::empty(),
            "lcPktMode should be WTPKT::empty()"
        );

        // lcMoveMask must match lcPktData.
        assert_eq!(
            ctx.lcMoveMask,
            WTPKT::all(),
            "lcMoveMask should match lcPktData"
        );

        // Button masks must cover all buttons.
        assert_eq!(*ctx.lcBtnDnMask, 0xFFFF, "lcBtnDnMask wrong");
        assert_eq!(*ctx.lcBtnUpMask, 0xFFFF, "lcBtnUpMask wrong");

        // Packet rate must match the caps value.
        assert_eq!(ctx.lcPktRate, 200_u32, "lcPktRate wrong");
    }

    /// Test with a different set of extents to verify parameterisation.
    #[cfg(windows)]
    #[test]
    fn logcontext_extents_are_parameterised() {
        let caps = make_caps(100_000, 75_000, 133);
        let ctx = build_logcontext(None, &caps);
        assert_eq!(ctx.lcInExtXYZ.x, 100_000_i32);
        assert_eq!(ctx.lcInExtXYZ.y, 75_000_i32);
        assert_eq!(ctx.lcOutExtXYZ.x, 100_000_i32);
        assert_eq!(ctx.lcOutExtXYZ.y, 75_000_i32);
        assert_eq!(ctx.lcPktRate, 133_u32);
    }
}
