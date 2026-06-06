//! Packet decode: Wintab raw packet → [`PenSample`] (SPEC §5.1, §6.2).
//!
//! [`decode_packet`] is a **pure function** with no I/O, no heap allocation,
//! and no OS calls.  It is the sole occupant of the capture hot path's decode
//! step (SPEC §4.3, §11).
//!
//! ## Cross-platform design
//! The [`FullPacket`], [`RawOrientation`], and [`RawRotation`] types are
//! defined here using only primitive Rust types.  This allows the decode logic
//! and all unit tests to compile and run on **any** OS without a Windows SDK
//! or the Wintab DLL.  On Windows, the capture thread casts the raw bytes
//! returned by `WTPacketsGet` into a `*const FullPacket` (valid because both
//! the Wintab driver and this struct use `#[repr(C)]` with matching field
//! order and the same packet-data mask `FULL_PKT_DATA`).
//!
//! ## Packet field order
//! The struct layout mirrors the WTPKT bit positions from low to high (SPEC
//! §6.2 table).  Wintab packs fields in ascending bit order, so the layout
//! must exactly follow that order when `lcPktData = FULL_PKT_DATA`.
//!
//! ## `pkXYZ` note
//! wintab_lite's `Packet` struct combines X/Y/Z into a single `XYZ<LONG>`
//! field.  `FullPacket` does the same via the `pkXYZ` field (separate `x`,
//! `y`, `z` sub-fields accessed as `packet.pkXYZ.x` etc.) so the binary
//! layout matches exactly.

use tablet_core::{normalize, tilt_from_orientation, DeviceCapabilities, PenSample, ToolKind};

// ---------------------------------------------------------------------------
// TPS status-bit constants
// ---------------------------------------------------------------------------

/// `TPS_PROXIMITY`: set when the cursor is out of the context (pen not in
/// range).  The pen is **in** proximity when this bit is **clear**.
const TPS_PROXIMITY: u32 = 0x0001;

/// `TPS_INVERT`: set when the cursor is in its inverted (eraser) state.
const TPS_INVERT: u32 = 0x0010;

// ---------------------------------------------------------------------------
// Raw orientation / rotation structs (primitive types, cross-platform)
// ---------------------------------------------------------------------------

/// Azimuth, altitude, and twist components of `PK_ORIENTATION`.
///
/// Units are 0.1° (deci-degrees) as returned by the Wintab driver.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawOrientation {
    /// Azimuth: clockwise angle in the tablet plane, 0.1° units.
    pub or_azimuth: i32,
    /// Altitude: angle from the tablet surface upward, 0.1° units.
    /// 900 = pen perpendicular to surface; 0 = pen flat.
    pub or_altitude: i32,
    /// Twist: clockwise rotation about the pen's own axis, 0.1° units.
    pub or_twist: i32,
}

/// Pitch, roll, and yaw components of `PK_ROTATION`.
///
/// Units are 0.1° (deci-degrees).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawRotation {
    pub ro_pitch: i32,
    pub ro_roll: i32,
    pub ro_yaw: i32,
}

/// XYZ position triple (mirrors `wintab_lite::XYZ<i32>`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawXYZ {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

// ---------------------------------------------------------------------------
// FullPacket — fixed-layout raw packet struct
// ---------------------------------------------------------------------------

/// A Wintab packet with all fields enabled (`lcPktData = FULL_PKT_DATA`).
///
/// The binary layout is `#[repr(C)]` and matches the field order of the
/// `WTPKT` bitmask (ascending bit position), which is also the order Wintab
/// uses when packing packets into memory.  The capture thread casts raw bytes
/// from `WTPacketsGet` directly to this type on Windows.
///
/// Every field is a primitive or a `repr(C)` struct of primitives so there
/// are no alignment surprises.  The layout is validated against
/// `wintab_lite::Packet` in the Windows integration tests.
///
/// # Field correspondence
/// | Field            | WTPKT bit | SPEC §6.2 name        |
/// |------------------|-----------|-----------------------|
/// | `pk_context`     | 0x0001    | PK_CONTEXT            |
/// | `pk_status`      | 0x0002    | PK_STATUS             |
/// | `pk_time`        | 0x0004    | PK_TIME               |
/// | `pk_changed`     | 0x0008    | PK_CHANGED            |
/// | `pk_serial`      | 0x0010    | PK_SERIAL_NUMBER      |
/// | `pk_cursor`      | 0x0020    | PK_CURSOR             |
/// | `pk_buttons`     | 0x0040    | PK_BUTTONS            |
/// | `pk_xyz`         | 0x0380    | PK_X | PK_Y | PK_Z   |
/// | `pk_normal_pres` | 0x0400    | PK_NORMAL_PRESSURE    |
/// | `pk_tangent_pres`| 0x0800    | PK_TANGENT_PRESSURE   |
/// | `pk_orientation` | 0x1000    | PK_ORIENTATION        |
/// | `pk_rotation`    | 0x2000    | PK_ROTATION           |
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FullPacket {
    /// Handle of the reporting context (`PK_CONTEXT`).
    /// Stored as `usize` to be pointer-width on both 32- and 64-bit Windows.
    pub pk_context: usize,

    /// Status bits (`PK_STATUS`): proximity, queue overflow, margin, invert.
    pub pk_status: u32,

    /// Device timestamp in milliseconds (`PK_TIME`).
    pub pk_time: u32,

    /// Changed-fields bitmask (`PK_CHANGED`).
    pub pk_changed: u32,

    /// Per-context serial number for gap detection (`PK_SERIAL_NUMBER`).
    pub pk_serial: u32,

    /// Cursor/tool identifier (`PK_CURSOR`).
    pub pk_cursor: u32,

    /// Button state bitmask (`PK_BUTTONS`).
    pub pk_buttons: u32,

    /// X/Y/Z position in native digitizer units (`PK_X | PK_Y | PK_Z`).
    pub pk_xyz: RawXYZ,

    /// Tip / normal pressure (`PK_NORMAL_PRESSURE`).
    pub pk_normal_pressure: u32,

    /// Barrel / tangent pressure (`PK_TANGENT_PRESSURE`), signed.
    pub pk_tangent_pressure: i32,

    /// Orientation: azimuth, altitude, twist (`PK_ORIENTATION`).
    pub pk_orientation: RawOrientation,

    /// Rotation: pitch, roll, yaw (`PK_ROTATION`).
    pub pk_rotation: RawRotation,
}

// ---------------------------------------------------------------------------
// Cursor → ToolKind mapping
// ---------------------------------------------------------------------------

/// Map a Wintab cursor ID and status flags to a [`ToolKind`].
///
/// Wintab cursor numbering is device-specific, but the convention is:
/// - Cursor IDs that are multiples of 3:       Pen tip
/// - Cursor IDs that are multiples of 3 + 1:   Eraser / inverted tip
/// - Cursor IDs that are multiples of 3 + 2:   Lens cursor / puck
///
/// The `TPS_INVERT` bit in `status` takes precedence: if the cursor is
/// currently in its inverted state, report `ToolKind::Eraser` regardless of
/// the cursor ID.
fn cursor_to_tool_kind(cursor_id: u32, status: u32) -> ToolKind {
    // TPS_INVERT: the physical tool is in eraser orientation.
    if status & TPS_INVERT != 0 {
        return ToolKind::Eraser;
    }

    // Wintab cursor IDs follow a 3-based pattern per tool type.
    match cursor_id % 3 {
        0 => ToolKind::Pen,
        1 => ToolKind::Eraser,
        2 => ToolKind::Cursor,
        _ => ToolKind::Unknown, // unreachable for % 3, but keeps the match exhaustive
    }
}

// ---------------------------------------------------------------------------
// decode_packet — the hot-path decode function
// ---------------------------------------------------------------------------

/// Decode a single raw Wintab packet to a [`PenSample`].
///
/// # Parameters
/// - `packet`        — raw packet as received from `WTPacketsGet`.
/// - `caps`          — [`DeviceCapabilities`] describing axis ranges and
///                     which axes are supported.
/// - `t_capture_ns`  — monotonic capture timestamp from `QueryPerformanceCounter`,
///                     in nanoseconds.  Captured once per drain burst and shared
///                     across all packets in that burst.
/// - `flip_y`        — when `true`, flip the Y coordinate so the origin is at
///                     the top-left (screen convention) rather than the
///                     bottom-left (Wintab convention).
///
/// # Hot-path constraints
/// This function performs **no I/O and no heap allocation**.  It uses only
/// arithmetic on stack values and calls into `tablet_core`'s pure math
/// functions.
#[inline]
pub fn decode_packet(
    packet: &FullPacket,
    caps: &DeviceCapabilities,
    t_capture_ns: u64,
    flip_y: bool,
) -> PenSample {
    // -- Position ----------------------------------------------------------

    let x_raw = packet.pk_xyz.x;

    // Y-flip: Wintab's origin is bottom-left; flip to top-left if requested.
    let y_raw = if flip_y {
        caps.y.max as i32 - packet.pk_xyz.y
    } else {
        packet.pk_xyz.y
    };

    let z_raw = packet.pk_xyz.z;

    let x_norm = normalize(x_raw as i64, caps.x.min, caps.x.max);
    let y_norm = normalize(y_raw as i64, caps.y.min, caps.y.max);

    // -- Pressure ----------------------------------------------------------

    let pressure_raw = packet.pk_normal_pressure;
    let pressure_norm = normalize(
        packet.pk_normal_pressure as i64,
        caps.pressure.min,
        caps.pressure.max,
    );

    let tangent_pressure_raw = if caps.tangent_pressure.supported {
        Some(packet.pk_tangent_pressure)
    } else {
        None
    };

    // -- Orientation (azimuth / altitude / twist / derived tilt) -----------

    let (azimuth_deci_deg, altitude_deci_deg, twist_deci_deg, tilt_x_deg, tilt_y_deg) =
        if caps.azimuth.supported && caps.altitude.supported {
            let az = packet.pk_orientation.or_azimuth;
            let alt = packet.pk_orientation.or_altitude;
            let twist = if caps.twist.supported {
                Some(packet.pk_orientation.or_twist)
            } else {
                None
            };
            let (tx, ty) = tilt_from_orientation(az, alt);
            (Some(az), Some(alt), twist, Some(tx), Some(ty))
        } else {
            (None, None, None, None, None)
        };

    // -- Rotation ----------------------------------------------------------

    let rotation_deci_deg = if caps.rotation.supported {
        Some(packet.pk_rotation.ro_yaw)
    } else {
        None
    };

    // -- Tool identity -----------------------------------------------------

    let tool = cursor_to_tool_kind(packet.pk_cursor, packet.pk_status);
    // A more precise serial requires cursor-tracking across packets (T3.3).
    // For now use the cursor ID as a stable stand-in.
    let tool_serial = packet.pk_cursor as u64;

    // -- Proximity ---------------------------------------------------------

    // TPS_PROXIMITY is set when the pen is OUT of range (confusingly named).
    let in_proximity = (packet.pk_status & TPS_PROXIMITY) == 0;

    // -- Assemble PenSample -----------------------------------------------

    PenSample {
        t_capture_ns,
        t_device_ms: packet.pk_time,
        serial: packet.pk_serial,

        x_raw,
        y_raw,
        z_raw,
        x_norm,
        y_norm,

        pressure_raw,
        pressure_norm,
        tangent_pressure_raw,

        azimuth_deci_deg,
        altitude_deci_deg,
        twist_deci_deg,
        tilt_x_deg,
        tilt_y_deg,
        rotation_deci_deg,

        buttons: packet.pk_buttons,

        tool,
        tool_serial,

        in_proximity,
        status: packet.pk_status,
    }
}

// ---------------------------------------------------------------------------
// Unit tests — cross-platform (no cfg(windows) guard)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities};

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn axis(min: i64, max: i64) -> AxisInfo {
        AxisInfo {
            min,
            max,
            resolution: 1.0,
            unit: AxisUnit::None,
            supported: true,
        }
    }

    fn unsupported() -> AxisInfo {
        AxisInfo {
            min: 0,
            max: 0,
            resolution: 0.0,
            unit: AxisUnit::None,
            supported: false,
        }
    }

    /// Build a `DeviceCapabilities` with fully-supported axes using the given
    /// coordinate and pressure ranges.
    fn full_caps(
        x_max: i64,
        y_max: i64,
        pressure_max: i64,
        tangent_max: i64,
    ) -> DeviceCapabilities {
        DeviceCapabilities {
            device_name: "Test".into(),
            driver_version: "0.0".into(),
            x: axis(0, x_max),
            y: axis(0, y_max),
            z: axis(0, 1000),
            pressure: axis(0, pressure_max),
            tangent_pressure: AxisInfo {
                min: -(tangent_max as i64),
                max: tangent_max,
                resolution: 1.0,
                unit: AxisUnit::None,
                supported: true,
            },
            azimuth: axis(0, 3600),
            altitude: axis(0, 900),
            twist: axis(0, 3600),
            rotation: axis(-1800, 1800),
            max_packet_rate_hz: 200,
            queue_size: 1024,
        }
    }

    /// Build a `DeviceCapabilities` where only X/Y/pressure are supported.
    fn minimal_caps(x_max: i64, y_max: i64, pressure_max: i64) -> DeviceCapabilities {
        DeviceCapabilities {
            device_name: "Test".into(),
            driver_version: "0.0".into(),
            x: axis(0, x_max),
            y: axis(0, y_max),
            z: unsupported(),
            pressure: axis(0, pressure_max),
            tangent_pressure: unsupported(),
            azimuth: unsupported(),
            altitude: unsupported(),
            twist: unsupported(),
            rotation: unsupported(),
            max_packet_rate_hz: 100,
            queue_size: 512,
        }
    }

    /// Build a default `FullPacket` with sensible non-zero values.
    fn make_packet(
        x: i32,
        y: i32,
        z: i32,
        pressure: u32,
        tangent: i32,
        cursor: u32,
        status: u32,
        serial: u32,
        time: u32,
        buttons: u32,
        az: i32,
        alt: i32,
        twist: i32,
        ro_yaw: i32,
    ) -> FullPacket {
        FullPacket {
            pk_context: 0,
            pk_status: status,
            pk_time: time,
            pk_changed: 0,
            pk_serial: serial,
            pk_cursor: cursor,
            pk_buttons: buttons,
            pk_xyz: RawXYZ { x, y, z },
            pk_normal_pressure: pressure,
            pk_tangent_pressure: tangent,
            pk_orientation: RawOrientation {
                or_azimuth: az,
                or_altitude: alt,
                or_twist: twist,
            },
            pk_rotation: RawRotation {
                ro_pitch: 0,
                ro_roll: 0,
                ro_yaw,
            },
        }
    }

    // -----------------------------------------------------------------------
    // Raw field passthrough
    // -----------------------------------------------------------------------

    #[test]
    fn raw_fields_pass_through() {
        let caps = full_caps(65536, 49152, 1023, 127);
        let pkt = make_packet(
            1234, 5678, 42, 512, -10, 0, 0, 7, 1000, 0b0011, 0, 900, 0, 0,
        );
        let s = decode_packet(&pkt, &caps, 9_999_999, false);

        assert_eq!(s.x_raw, 1234);
        assert_eq!(s.y_raw, 5678); // no flip
        assert_eq!(s.z_raw, 42);
        assert_eq!(s.pressure_raw, 512);
        assert_eq!(s.t_device_ms, 1000);
        assert_eq!(s.serial, 7);
        assert_eq!(s.buttons, 0b0011);
        assert_eq!(s.t_capture_ns, 9_999_999);
    }

    // -----------------------------------------------------------------------
    // Normalization endpoints
    // -----------------------------------------------------------------------

    #[test]
    fn x_at_min_normalizes_to_zero() {
        let caps = full_caps(10000, 8000, 1023, 0);
        let pkt = make_packet(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.x_norm, 0.0);
    }

    #[test]
    fn x_at_max_normalizes_to_one() {
        let caps = full_caps(10000, 8000, 1023, 0);
        let pkt = make_packet(10000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.x_norm, 1.0);
    }

    #[test]
    fn pressure_at_max_normalizes_to_one() {
        let caps = full_caps(1000, 1000, 1023, 0);
        let pkt = make_packet(0, 0, 0, 1023, 0, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.pressure_norm, 1.0);
    }

    #[test]
    fn pressure_at_midpoint_normalizes_to_half() {
        let caps = full_caps(1000, 1000, 1000, 0);
        let pkt = make_packet(0, 0, 0, 500, 0, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        let diff = (s.pressure_norm - 0.5).abs();
        assert!(diff < 1e-10, "expected ~0.5, got {}", s.pressure_norm);
    }

    // -----------------------------------------------------------------------
    // Y-flip
    // -----------------------------------------------------------------------

    #[test]
    fn y_flip_transforms_correctly() {
        let caps = full_caps(1000, 1000, 1023, 0);
        // Packet y = 200, y_max = 1000 → flipped y = 1000 - 200 = 800.
        let pkt = make_packet(0, 200, 0, 0, 0, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, true);
        assert_eq!(s.y_raw, 800);
    }

    #[test]
    fn y_no_flip_passes_through() {
        let caps = full_caps(1000, 1000, 1023, 0);
        let pkt = make_packet(0, 200, 0, 0, 0, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.y_raw, 200);
    }

    #[test]
    fn y_flip_at_zero_gives_max() {
        let caps = full_caps(1000, 1000, 1023, 0);
        let pkt = make_packet(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, true);
        assert_eq!(s.y_raw, 1000); // y_max - 0 = 1000
    }

    #[test]
    fn y_flip_at_max_gives_zero() {
        let caps = full_caps(1000, 1000, 1023, 0);
        let pkt = make_packet(0, 1000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, true);
        assert_eq!(s.y_raw, 0);
    }

    // -----------------------------------------------------------------------
    // Tilt derivation
    // -----------------------------------------------------------------------

    #[test]
    fn altitude_900_deci_deg_gives_zero_tilt() {
        // altitude = 900 deci-deg = 90° → pen vertical → tilt = (0, 0).
        let caps = full_caps(1000, 1000, 1023, 0);
        let pkt = make_packet(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.tilt_x_deg, Some(0.0));
        assert_eq!(s.tilt_y_deg, Some(0.0));
    }

    #[test]
    fn tilt_known_pair_azimuth0_altitude450() {
        // azimuth = 0, altitude = 450 deci-deg (45°).
        // tilt_x = atan(cos(0) / tan(45°)) * 180/π = atan(1) = 45°.
        // tilt_y = atan(sin(0) / tan(45°)) * 180/π = atan(0) = 0°.
        let caps = full_caps(1000, 1000, 1023, 0);
        let pkt = make_packet(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 450, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);

        let tx = s.tilt_x_deg.expect("tilt_x_deg should be Some");
        let ty = s.tilt_y_deg.expect("tilt_y_deg should be Some");

        assert!((tx - 45.0).abs() < 1e-6, "tilt_x: expected 45.0, got {tx}");
        assert!(ty.abs() < 1e-6, "tilt_y: expected 0.0, got {ty}");
    }

    // -----------------------------------------------------------------------
    // Unsupported axes → None
    // -----------------------------------------------------------------------

    #[test]
    fn unsupported_axes_decode_to_none() {
        let caps = minimal_caps(1000, 1000, 1023);
        let pkt = make_packet(500, 500, 0, 512, 10, 0, 0, 1, 0, 0, 0, 0, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);

        assert_eq!(s.tangent_pressure_raw, None, "tangent should be None");
        assert_eq!(s.azimuth_deci_deg, None, "azimuth should be None");
        assert_eq!(s.altitude_deci_deg, None, "altitude should be None");
        assert_eq!(s.twist_deci_deg, None, "twist should be None");
        assert_eq!(s.tilt_x_deg, None, "tilt_x should be None");
        assert_eq!(s.tilt_y_deg, None, "tilt_y should be None");
        assert_eq!(s.rotation_deci_deg, None, "rotation should be None");
    }

    #[test]
    fn supported_tangent_pressure_is_some() {
        let caps = full_caps(1000, 1000, 1023, 127);
        let pkt = make_packet(0, 0, 0, 0, -42, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.tangent_pressure_raw, Some(-42));
    }

    #[test]
    fn supported_rotation_is_some() {
        let caps = full_caps(1000, 1000, 1023, 127);
        let pkt = make_packet(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 900, 0, 333);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.rotation_deci_deg, Some(333));
    }

    // -----------------------------------------------------------------------
    // Proximity decoding
    // -----------------------------------------------------------------------

    #[test]
    fn pen_in_range_when_tps_proximity_clear() {
        let caps = minimal_caps(1000, 1000, 1023);
        // status = 0 → TPS_PROXIMITY bit clear → in_proximity = true.
        let pkt = make_packet(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert!(
            s.in_proximity,
            "should be in proximity when TPS_PROXIMITY clear"
        );
    }

    #[test]
    fn pen_out_of_range_when_tps_proximity_set() {
        let caps = minimal_caps(1000, 1000, 1023);
        // TPS_PROXIMITY = 0x0001 → pen out of context.
        let pkt = make_packet(0, 0, 0, 0, 0, 0, 0x0001, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert!(
            !s.in_proximity,
            "should not be in proximity when TPS_PROXIMITY set"
        );
    }

    // -----------------------------------------------------------------------
    // Tool-kind mapping
    // -----------------------------------------------------------------------

    #[test]
    fn cursor_id_mod3_zero_is_pen() {
        let caps = minimal_caps(1000, 1000, 1023);
        // cursor = 0 (0 % 3 == 0), no TPS_INVERT → Pen.
        let pkt = make_packet(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.tool, ToolKind::Pen);
    }

    #[test]
    fn cursor_id_mod3_one_is_eraser() {
        let caps = minimal_caps(1000, 1000, 1023);
        // cursor = 1 (1 % 3 == 1), no TPS_INVERT → Eraser.
        let pkt = make_packet(0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.tool, ToolKind::Eraser);
    }

    #[test]
    fn cursor_id_mod3_two_is_cursor() {
        let caps = minimal_caps(1000, 1000, 1023);
        // cursor = 2 (2 % 3 == 2), no TPS_INVERT → Cursor.
        let pkt = make_packet(0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.tool, ToolKind::Cursor);
    }

    #[test]
    fn tps_invert_forces_eraser_regardless_of_cursor_id() {
        let caps = minimal_caps(1000, 1000, 1023);
        // cursor = 0 (would normally be Pen), but TPS_INVERT (0x0010) set.
        let pkt = make_packet(0, 0, 0, 0, 0, 0, TPS_INVERT, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.tool, ToolKind::Eraser);
    }

    // -----------------------------------------------------------------------
    // tool_serial passthrough
    // -----------------------------------------------------------------------

    #[test]
    fn tool_serial_matches_cursor_id() {
        let caps = minimal_caps(1000, 1000, 1023);
        let pkt = make_packet(0, 0, 0, 0, 0, 42, 0, 0, 0, 0, 0, 900, 0, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.tool_serial, 42);
    }

    // -----------------------------------------------------------------------
    // Orientation fields when supported
    // -----------------------------------------------------------------------

    #[test]
    fn orientation_fields_populated_when_supported() {
        let caps = full_caps(1000, 1000, 1023, 127);
        let pkt = make_packet(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 450, 450, 100, 0);
        let s = decode_packet(&pkt, &caps, 0, false);
        assert_eq!(s.azimuth_deci_deg, Some(450));
        assert_eq!(s.altitude_deci_deg, Some(450));
        assert_eq!(s.twist_deci_deg, Some(100));
    }
}
