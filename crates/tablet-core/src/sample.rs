use serde::{Deserialize, Serialize};

/// The kind of tool currently in use at the tablet.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToolKind {
    Pen,
    Eraser,
    Airbrush,
    Cursor,
    Unknown,
}

/// A single pen data packet, normalized into a platform-agnostic form.
///
/// Carries both **raw** device values (lossless) and **normalized** convenience
/// values so consumers can use either without losing fidelity.
///
/// `PenSample` is `Copy` so it can be carried by value in a ring buffer with no
/// heap allocation on the capture hot path (see §11 of SPEC_1.md).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PenSample {
    /// Monotonic capture timestamp (QueryPerformanceCounter), nanoseconds.
    pub t_capture_ns: u64,
    /// Device-provided timestamp (PK_TIME), milliseconds; may wrap.
    pub t_device_ms: u32,
    /// Per-context packet serial number (PK_SERIAL_NUMBER) for gap detection.
    pub serial: u32,

    /// Raw position in native digitizer units (full resolution).
    pub x_raw: i32,
    pub y_raw: i32,
    pub z_raw: i32,
    /// Position normalized to [0.0, 1.0] over the device's input extent.
    pub x_norm: f64,
    pub y_norm: f64,

    /// Raw tip/normal pressure.
    pub pressure_raw: u32,
    /// Normalized [0.0, 1.0] tip/normal pressure.
    pub pressure_norm: f64,
    /// Raw tangent/barrel pressure (airbrush wheel), if supported.
    pub tangent_pressure_raw: Option<i32>,

    /// Orientation (PK_ORIENTATION): azimuth + altitude + twist, in 0.1° units.
    pub azimuth_deci_deg: Option<i32>,
    pub altitude_deci_deg: Option<i32>,
    pub twist_deci_deg: Option<i32>,
    /// Convenience tilt in degrees derived from azimuth/altitude.
    pub tilt_x_deg: Option<f64>,
    pub tilt_y_deg: Option<f64>,
    /// Rotation (PK_ROTATION) where supported, in 0.1° units.
    pub rotation_deci_deg: Option<i32>,

    /// Button bitmask (PK_BUTTONS).
    pub buttons: u32,

    /// Tool identity.
    pub tool: ToolKind,
    /// Physical pen serial number (PK_CURSOR derived).
    pub tool_serial: u64,

    /// Proximity / status flags: whether the tool is in range.
    pub in_proximity: bool,
    /// Raw status word (PK_STATUS).
    pub status: u32,
}
