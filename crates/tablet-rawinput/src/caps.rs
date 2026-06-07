//! HID capability model and the [`DeviceCapabilities`] builder (SPEC_BGC §6.1, §7).
//!
//! [`ProfileCaps`] is the **pure, OS-independent** description of what a device's
//! HID report descriptor exposes: which usages are present and, for value usages,
//! their logical min/max (the native digitizer range) plus bit size and signedness
//! used by the decoder. It is the single source the decoder reads for
//! normalization, and the source [`capabilities_from_profile`] maps onto the wire
//! [`DeviceCapabilities`].
//!
//! On Windows the live profile (preparsed data + handle) is built in
//! [`crate::enumerate`]; here we keep only the plain-data parts so the decoder and
//! the capability builder stay unit-testable on any OS.

use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities};

/// Range and layout metadata for one HID **value** usage.
///
/// `min`/`max` are the HID *logical* min/max — the device's native range, which
/// for X/Y is typically far finer than screen resolution. `bit_size` and the
/// implied signedness (`min < 0`) let the live Windows reader sign-extend raw
/// `HidP_GetUsageValue` results; the pure decoder uses `min`/`max` for
/// normalization only.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AxisRange {
    /// HID logical minimum.
    pub min: i32,
    /// HID logical maximum.
    pub max: i32,
    /// Native resolution (units per physical unit) when derivable from the HID
    /// physical range / unit exponent; `0.0` when unknown.
    pub resolution: f64,
    /// Field width in bits (for sign extension on the live path).
    pub bit_size: u16,
}

impl AxisRange {
    /// Construct a range with unknown resolution and a default bit size.
    pub fn new(min: i32, max: i32) -> Self {
        Self {
            min,
            max,
            resolution: 0.0,
            bit_size: 0,
        }
    }

    /// `true` when the field is signed (logical minimum is negative).
    pub fn is_signed(&self) -> bool {
        self.min < 0
    }
}

/// Pure description of a digitizer's HID capabilities.
///
/// Value usages are `Some(range)` when present in the report descriptor and
/// `None` when absent. Button usages are booleans. This struct carries no OS
/// handles, so it (and everything that consumes it) is testable without hardware.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProfileCaps {
    /// Human-readable / interface-path device name.
    pub device_name: String,
    /// Best-effort driver/HID version string.
    pub driver_version: String,

    /// X position (Generic Desktop 0x30).
    pub x: Option<AxisRange>,
    /// Y position (Generic Desktop 0x31).
    pub y: Option<AxisRange>,
    /// Tip pressure (Digitizer 0x30).
    pub pressure: Option<AxisRange>,
    /// Barrel / tangent pressure (Digitizer 0x31), airbrush only.
    pub tangent_pressure: Option<AxisRange>,
    /// X tilt (Digitizer 0x3D).
    pub x_tilt: Option<AxisRange>,
    /// Y tilt (Digitizer 0x3E).
    pub y_tilt: Option<AxisRange>,
    /// Azimuth (Digitizer 0x3F) — orientation alternative to X/Y tilt.
    pub azimuth: Option<AxisRange>,
    /// Altitude (Digitizer 0x40).
    pub altitude: Option<AxisRange>,
    /// Twist (Digitizer 0x41).
    pub twist: Option<AxisRange>,

    /// In-Range (proximity) button usage present.
    pub has_in_range: bool,
    /// Tip switch present.
    pub has_tip: bool,
    /// Barrel switch present.
    pub has_barrel: bool,
    /// Eraser switch present.
    pub has_eraser: bool,
    /// Invert usage present.
    pub has_invert: bool,
}

/// Build the supported [`AxisInfo`] for a present axis, or the documented
/// "unsupported" descriptor (`supported = false`) when the usage is absent.
fn axis_info(range: Option<AxisRange>, unit: AxisUnit) -> AxisInfo {
    match range {
        Some(r) => AxisInfo {
            min: r.min as i64,
            max: r.max as i64,
            resolution: r.resolution,
            unit,
            supported: true,
        },
        None => AxisInfo {
            min: 0,
            max: 0,
            resolution: 0.0,
            unit: AxisUnit::None,
            supported: false,
        },
    }
}

/// Map a [`ProfileCaps`] onto the wire [`DeviceCapabilities`] (SPEC_BGC §7).
///
/// `max_packet_rate_hz` is left `0` here (HID has no negotiated rate; it is
/// measured later — SPEC_BGC §7/B6). `queue_size` carries the `GetRawInputBuffer`
/// batch capacity in reports (SPEC_BGC §5.2); `0` means "use the default".
///
/// Note: `DeviceCapabilities` has no dedicated X/Y-tilt axis (its orientation
/// axes are azimuth/altitude/twist/rotation). Devices that report X/Y tilt
/// directly still deliver it per-sample via `PenSample.tilt_*`; their tilt
/// presence is therefore not reflected in this descriptor. `z`/`rotation` are
/// always unsupported under this backend.
pub fn capabilities_from_profile(caps: &ProfileCaps, queue_size: u32) -> DeviceCapabilities {
    DeviceCapabilities {
        device_name: caps.device_name.clone(),
        driver_version: caps.driver_version.clone(),
        x: axis_info(caps.x, AxisUnit::None),
        y: axis_info(caps.y, AxisUnit::None),
        z: axis_info(None, AxisUnit::None),
        pressure: axis_info(caps.pressure, AxisUnit::None),
        tangent_pressure: axis_info(caps.tangent_pressure, AxisUnit::None),
        azimuth: axis_info(caps.azimuth, AxisUnit::Degree),
        altitude: axis_info(caps.altitude, AxisUnit::Degree),
        twist: axis_info(caps.twist, AxisUnit::Degree),
        rotation: axis_info(None, AxisUnit::None),
        max_packet_rate_hz: 0,
        queue_size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_profile() -> ProfileCaps {
        ProfileCaps {
            device_name: "Test Digitizer".into(),
            driver_version: "hid".into(),
            x: Some(AxisRange {
                min: 0,
                max: 30000,
                resolution: 100.0,
                bit_size: 16,
            }),
            y: Some(AxisRange::new(0, 20000)),
            pressure: Some(AxisRange::new(0, 8191)),
            twist: Some(AxisRange::new(0, 3600)),
            ..Default::default()
        }
    }

    #[test]
    fn present_axes_are_supported_with_logical_range() {
        let caps = capabilities_from_profile(&sample_profile(), 0);
        assert!(caps.x.supported);
        assert_eq!(caps.x.min, 0);
        assert_eq!(caps.x.max, 30000);
        assert_eq!(caps.x.resolution, 100.0);
        assert!(caps.pressure.supported);
        assert_eq!(caps.pressure.max, 8191);
    }

    #[test]
    fn absent_axes_are_unsupported() {
        let caps = capabilities_from_profile(&sample_profile(), 0);
        // No tilt/azimuth/altitude/tangent in the sample profile.
        assert!(!caps.azimuth.supported);
        assert!(!caps.altitude.supported);
        assert!(!caps.tangent_pressure.supported);
        // z and rotation are never supported under this backend.
        assert!(!caps.z.supported);
        assert!(!caps.rotation.supported);
    }

    #[test]
    fn twist_axis_uses_degree_unit() {
        let caps = capabilities_from_profile(&sample_profile(), 0);
        assert!(caps.twist.supported);
        assert_eq!(caps.twist.unit, AxisUnit::Degree);
    }

    #[test]
    fn queue_size_is_passed_through() {
        let caps = capabilities_from_profile(&sample_profile(), 512);
        assert_eq!(caps.queue_size, 512);
        assert_eq!(caps.max_packet_rate_hz, 0);
    }

    #[test]
    fn signedness_follows_logical_min() {
        assert!(AxisRange::new(-90, 90).is_signed());
        assert!(!AxisRange::new(0, 8191).is_signed());
    }
}
