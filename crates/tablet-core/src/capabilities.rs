use serde::{Deserialize, Serialize};

/// Physical unit reported by Wintab for an axis.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AxisUnit {
    Inch,
    Centimeter,
    Degree,
    None,
}

/// Range and resolution metadata for a single tablet axis.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AxisInfo {
    /// Minimum raw value the device can report on this axis.
    pub min: i64,
    /// Maximum raw value the device can report on this axis.
    pub max: i64,
    /// Native resolution: units per inch/cm as reported by WTInfo.
    pub resolution: f64,
    /// Physical unit of measurement for this axis.
    pub unit: AxisUnit,
    /// Whether this axis is supported by the current device/driver.
    pub supported: bool,
}

/// Handshake descriptor sent once at session start (and on WT_INFOCHANGE) so
/// consumers can interpret raw axis ranges without guessing.
///
/// `DeviceCapabilities` is NOT `Copy` because it contains `String` fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    /// Human-readable device name from WTInfo.
    pub device_name: String,
    /// Driver version string from WTInfo.
    pub driver_version: String,

    pub x: AxisInfo,
    pub y: AxisInfo,
    pub z: AxisInfo,
    pub pressure: AxisInfo,
    pub tangent_pressure: AxisInfo,
    pub azimuth: AxisInfo,
    pub altitude: AxisInfo,
    pub twist: AxisInfo,
    pub rotation: AxisInfo,

    /// Actual negotiated packet rate in Hz (re-read from lcPktRate after WTOpen).
    pub max_packet_rate_hz: u32,
    /// Negotiated Wintab queue depth (after WTQueueSizeSet backoff).
    pub queue_size: u32,
}
