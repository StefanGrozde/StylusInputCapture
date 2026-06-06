//! Capability query: build `DeviceCapabilities` from `WTInfo`.
//!
//! [`query_capabilities`] interrogates the Wintab driver via [`WintabApi`]
//! and constructs a fully-populated [`DeviceCapabilities`] handshake
//! descriptor.  Axis entries with `supported = false` indicate axes that the
//! current device/driver does not expose (WTInfo returned zero bytes for them).
//!
//! This module does **not** open a capture context; that is done in T3.3.
//! Consequently [`DeviceCapabilities::queue_size`] is set to `0` here and
//! updated after `WTOpen` + `WTQueueSizeSet`.

use tablet_core::{AxisInfo, AxisUnit, BackendError, DeviceCapabilities};
use tracing::{debug, warn};
use wintab_lite::AXIS;

use crate::ffi::{
    WintabApi, DVC_NAME, DVC_NPRESSURE, DVC_ORIENTATION, DVC_PKTRATE, DVC_ROTATION, DVC_TPRESSURE,
    DVC_X, DVC_Y, DVC_Z, IFC_WINTABID, WTI_DEVICES, WTI_INTERFACE,
};

// ---------------------------------------------------------------------------
// Unit mapping
// ---------------------------------------------------------------------------

/// Map a Wintab `TU_*` unit code to the platform-agnostic [`AxisUnit`].
///
/// Wintab axis unit codes (axUnits field):
/// - 0 = TU_NONE       → `AxisUnit::None`
/// - 1 = TU_INCHES     → `AxisUnit::Inch`
/// - 2 = TU_CENTIMETERS → `AxisUnit::Centimeter`
/// - 3 = TU_CIRCLE     → `AxisUnit::Degree`
fn map_axis_unit(tu: u32) -> AxisUnit {
    match tu {
        1 => AxisUnit::Inch,
        2 => AxisUnit::Centimeter,
        3 => AxisUnit::Degree,
        _ => AxisUnit::None,
    }
}

// ---------------------------------------------------------------------------
// AXIS → AxisInfo conversion
// ---------------------------------------------------------------------------

/// Convert a Wintab `AXIS` struct to an [`AxisInfo`], marking the axis as
/// supported.
///
/// The `axResolution` field is a FIX32 value (16.16 fixed-point); dividing by
/// 65536.0 yields a float giving units-per-inch (or per-cm, per-circle, etc.)
/// as reported by the driver.
fn axis_to_info(ax: &AXIS) -> AxisInfo {
    let resolution: f64 = ax.axResolution.into();
    let unit = map_axis_unit(ax.axUnits as u32);
    AxisInfo {
        min: ax.axMin as i64,
        max: ax.axMax as i64,
        resolution,
        unit,
        supported: true,
    }
}

/// A zeroed-out, unsupported [`AxisInfo`] used as a placeholder when the
/// driver does not expose a particular axis.
fn unsupported_axis() -> AxisInfo {
    AxisInfo {
        min: 0,
        max: 0,
        resolution: 0.0,
        unit: AxisUnit::None,
        supported: false,
    }
}

/// Query a single axis from the driver, returning `unsupported_axis()` on failure.
fn query_single(api: &WintabApi, category: u32, index: u32, name: &str) -> AxisInfo {
    match api.query_axis(category, index) {
        Some(ax) => {
            debug!(
                axis = name,
                min = ax.axMin,
                max = ax.axMax,
                unit = ax.axUnits as u32,
                "axis queried"
            );
            axis_to_info(&ax)
        }
        None => {
            debug!(axis = name, "axis not supported by driver");
            unsupported_axis()
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Query the Wintab driver for device capabilities and return a fully
/// populated [`DeviceCapabilities`] descriptor.
///
/// Steps performed:
/// 1. Query device name (`DVC_NAME`).
/// 2. Query driver/hardware ID string (`IFC_WINTABID`).
/// 3. Query each axis structure (`DVC_X/Y/Z/NPRESSURE/TPRESSURE/ORIENTATION/ROTATION`).
/// 4. Query the maximum packet rate (`DVC_PKTRATE`).
///
/// `queue_size` is left as `0`; the caller updates it after `WTQueueSizeSet`.
///
/// Returns `BackendError::NoDevice` if the interface reports no active tablet
/// (WTInfo returns 0 for the device name query when no device is attached).
pub fn query_capabilities(api: &WintabApi) -> Result<DeviceCapabilities, BackendError> {
    // ------------------------------------------------------------------
    // 1. Device name
    // ------------------------------------------------------------------
    let device_name = api.query_string(WTI_DEVICES, DVC_NAME);
    if device_name.is_empty() {
        warn!("WTInfo returned empty device name — no tablet attached?");
        return Err(BackendError::NoDevice);
    }
    debug!(device_name = %device_name, "device name queried");

    // ------------------------------------------------------------------
    // 2. Driver / hardware identification string
    // ------------------------------------------------------------------
    let driver_version = api.query_string(WTI_INTERFACE, IFC_WINTABID);
    debug!(driver_version = %driver_version, "driver version queried");

    // ------------------------------------------------------------------
    // 3. Individual axes
    // ------------------------------------------------------------------

    // Position axes
    let x = query_single(api, WTI_DEVICES, DVC_X, "x");
    let y = query_single(api, WTI_DEVICES, DVC_Y, "y");
    let z = query_single(api, WTI_DEVICES, DVC_Z, "z");

    // Pressure axes
    let pressure = query_single(api, WTI_DEVICES, DVC_NPRESSURE, "npressure");
    let tangent_pressure = query_single(api, WTI_DEVICES, DVC_TPRESSURE, "tpressure");

    // Orientation: WTInfo returns 3 AXIS structs [azimuth, altitude, twist].
    let (azimuth, altitude, twist) = match api.query_axis3(WTI_DEVICES, DVC_ORIENTATION) {
        Some([az, alt, tw]) => {
            debug!(
                az_min = az.axMin,
                az_max = az.axMax,
                alt_min = alt.axMin,
                alt_max = alt.axMax,
                tw_min = tw.axMin,
                tw_max = tw.axMax,
                "orientation axes queried"
            );
            (axis_to_info(&az), axis_to_info(&alt), axis_to_info(&tw))
        }
        None => {
            debug!("orientation not supported by driver");
            (unsupported_axis(), unsupported_axis(), unsupported_axis())
        }
    };

    // Rotation: WTInfo returns 3 AXIS structs [pitch, roll, yaw].
    // We expose only the first (yaw/rotation) in DeviceCapabilities.rotation.
    let rotation = match api.query_axis3(WTI_DEVICES, DVC_ROTATION) {
        Some([rot0, _, _]) => {
            debug!(
                rot_min = rot0.axMin,
                rot_max = rot0.axMax,
                "rotation axis queried"
            );
            axis_to_info(&rot0)
        }
        None => {
            debug!("rotation not supported by driver");
            unsupported_axis()
        }
    };

    // ------------------------------------------------------------------
    // 4. Maximum packet rate (UINT)
    // ------------------------------------------------------------------
    let max_packet_rate_hz: u32 = api
        .query::<u32>(WTI_DEVICES, DVC_PKTRATE)
        .unwrap_or_else(|| {
            warn!("DVC_PKTRATE query failed; defaulting to 0");
            0
        });
    debug!(max_packet_rate_hz, "max packet rate queried");

    Ok(DeviceCapabilities {
        device_name,
        driver_version,
        x,
        y,
        z,
        pressure,
        tangent_pressure,
        azimuth,
        altitude,
        twist,
        rotation,
        max_packet_rate_hz,
        queue_size: 0, // updated after WTQueueSizeSet in T3.3
    })
}
