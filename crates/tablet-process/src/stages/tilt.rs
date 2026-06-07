//! Stage 3 — Tilt / orientation derivation.
//!
//! Implements [`TiltConfig::apply`], which selects the tilt convention to expose
//! and optionally derives Cartesian tilt from azimuth/altitude orientation.
//!
//! Two conventions are supported:
//!
//! - **`TiltXY` (default):** copy `tilt_x_deg` / `tilt_y_deg` directly from the
//!   [`PenSample`].  These are already derived by the capture layer using
//!   [`tilt_from_orientation`].
//! - **`AzimuthAltitude`:** expose azimuth and altitude (from
//!   `azimuth_deci_deg` / `altitude_deci_deg`) converted to degrees.  The
//!   azimuth is stored in `tilt_x`, the altitude in `tilt_y`.
//!
//! Both paths also copy `twist_deci_deg` → degrees into the output `twist`
//! field.  When the raw sample does not have tilt / orientation data, all
//! outputs are `None`.
//!
//! When the stage is disabled (`enabled = false`), the pass-through behaviour
//! matches TC1.1: copy `tilt_x_deg` / `tilt_y_deg` from the raw sample and
//! convert `twist_deci_deg` to degrees.
//!
//! [`PenSample`]: tablet_core::PenSample
//! [`tilt_from_orientation`]: tablet_core::tilt_from_orientation

use tablet_core::{tilt_from_orientation, PenSample};

use crate::profile::{TiltConfig, TiltConvention};

// ── TiltConfig::apply ────────────────────────────────────────────────────────

impl TiltConfig {
    /// Derive tilt values from a raw [`PenSample`] according to this stage's
    /// convention.
    ///
    /// # Returns
    ///
    /// `(tilt_x, tilt_y, twist)` — all in degrees.  Each is `None` when the
    /// underlying raw fields are absent from the sample.
    ///
    /// # Convention details
    ///
    /// See [`TiltConvention`] for a full description of the two modes.
    ///
    /// # When to call
    ///
    /// Only call this when `self.enabled == true`.  The caller in
    /// [`crate::CalibrationProfile::apply`] handles the disabled fall-through.
    pub fn apply(&self, raw: &PenSample) -> (Option<f64>, Option<f64>, Option<f64>) {
        let twist = raw.twist_deci_deg.map(|d| d as f64 / 10.0);

        match self.convention {
            TiltConvention::TiltXY => {
                // Copy the pre-computed Cartesian tilt directly from the sample.
                (raw.tilt_x_deg, raw.tilt_y_deg, twist)
            }
            TiltConvention::AzimuthAltitude => {
                // Derive from azimuth/altitude via tilt_from_orientation, then
                // expose as azimuth → tilt_x, altitude → tilt_y (in degrees).
                match (raw.azimuth_deci_deg, raw.altitude_deci_deg) {
                    (Some(az), Some(alt)) => {
                        let az_deg = az as f64 / 10.0;
                        let alt_deg = alt as f64 / 10.0;
                        (Some(az_deg), Some(alt_deg), twist)
                    }
                    // Partial data: fall back to None for both.
                    _ => (None, None, twist),
                }
            }
            TiltConvention::DerivedFromOrientation => {
                // Drive tilt_x/tilt_y from azimuth+altitude via the core
                // conversion formula, ignoring the sample's pre-computed fields.
                match (raw.azimuth_deci_deg, raw.altitude_deci_deg) {
                    (Some(az), Some(alt)) => {
                        let (tx, ty) = tilt_from_orientation(az, alt);
                        (Some(tx), Some(ty), twist)
                    }
                    // Partial data: fall back to None for both.
                    _ => (None, None, twist),
                }
            }
        }
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::TiltConvention;
    use tablet_core::ToolKind;

    fn make_sample_full(
        azimuth: Option<i32>,
        altitude: Option<i32>,
        twist: Option<i32>,
        tilt_x: Option<f64>,
        tilt_y: Option<f64>,
    ) -> PenSample {
        PenSample {
            t_capture_ns: 0,
            t_device_ms: 0,
            serial: 0,
            x_raw: 0,
            y_raw: 0,
            z_raw: 0,
            x_norm: 0.0,
            y_norm: 0.0,
            pressure_raw: 0,
            pressure_norm: 0.0,
            tangent_pressure_raw: None,
            azimuth_deci_deg: azimuth,
            altitude_deci_deg: altitude,
            twist_deci_deg: twist,
            tilt_x_deg: tilt_x,
            tilt_y_deg: tilt_y,
            rotation_deci_deg: None,
            buttons: 0,
            tool: ToolKind::Pen,
            tool_serial: 0,
            in_proximity: true,
            status: 0,
        }
    }

    // ── TiltXY convention ────────────────────────────────────────────────────

    /// TiltXY copies `tilt_x_deg`/`tilt_y_deg` directly from the raw sample.
    #[test]
    fn tilt_xy_copies_raw_fields() {
        let raw = make_sample_full(None, None, None, Some(15.0), Some(-10.0));
        let cfg = TiltConfig {
            enabled: true,
            convention: TiltConvention::TiltXY,
        };
        let (tx, ty, twist) = cfg.apply(&raw);
        assert_eq!(tx, Some(15.0));
        assert_eq!(ty, Some(-10.0));
        assert_eq!(twist, None);
    }

    /// TiltXY returns None for both when the raw sample has no tilt data.
    #[test]
    fn tilt_xy_none_when_absent() {
        let raw = make_sample_full(None, None, None, None, None);
        let cfg = TiltConfig {
            enabled: true,
            convention: TiltConvention::TiltXY,
        };
        let (tx, ty, twist) = cfg.apply(&raw);
        assert_eq!(tx, None);
        assert_eq!(ty, None);
        assert_eq!(twist, None);
    }

    // ── AzimuthAltitude convention ───────────────────────────────────────────

    /// AzimuthAltitude exposes az → tilt_x, alt → tilt_y in degrees.
    #[test]
    fn azimuth_altitude_convention_degrees() {
        // azimuth = 1800 deci-deg = 180°; altitude = 450 deci-deg = 45°.
        let raw = make_sample_full(Some(1800), Some(450), Some(900), None, None);
        let cfg = TiltConfig {
            enabled: true,
            convention: TiltConvention::AzimuthAltitude,
        };
        let (tx, ty, twist) = cfg.apply(&raw);
        assert_eq!(tx, Some(180.0), "azimuth as tilt_x");
        assert_eq!(ty, Some(45.0), "altitude as tilt_y");
        assert_eq!(twist, Some(90.0), "twist 900 deci-deg → 90°");
    }

    /// AzimuthAltitude returns None for both when azimuth/altitude absent.
    #[test]
    fn azimuth_altitude_none_when_absent() {
        let raw = make_sample_full(None, None, None, Some(5.0), Some(3.0));
        let cfg = TiltConfig {
            enabled: true,
            convention: TiltConvention::AzimuthAltitude,
        };
        let (tx, ty, _) = cfg.apply(&raw);
        assert_eq!(tx, None);
        assert_eq!(ty, None);
    }

    // ── DerivedFromOrientation convention ────────────────────────────────────

    /// DerivedFromOrientation uses tilt_from_orientation to compute tilt_x/y.
    ///
    /// Known values from tablet_core tests:
    /// azimuth = 0 deci-deg (0°), altitude = 450 deci-deg (45°)
    ///   → tilt_x = 45.0°, tilt_y = 0.0°.
    #[test]
    fn derived_from_orientation_known_values() {
        let raw = make_sample_full(Some(0), Some(450), None, None, None);
        let cfg = TiltConfig {
            enabled: true,
            convention: TiltConvention::DerivedFromOrientation,
        };
        let (tx, ty, _) = cfg.apply(&raw);
        assert!((tx.unwrap() - 45.0).abs() < 1e-6, "tilt_x: {:?}", tx);
        assert!(ty.unwrap().abs() < 1e-6, "tilt_y: {:?}", ty);
    }

    /// DerivedFromOrientation: vertical pen (altitude = 900) yields (0, 0).
    #[test]
    fn derived_from_orientation_vertical_pen() {
        let raw = make_sample_full(Some(0), Some(900), None, None, None);
        let cfg = TiltConfig {
            enabled: true,
            convention: TiltConvention::DerivedFromOrientation,
        };
        let (tx, ty, _) = cfg.apply(&raw);
        assert_eq!(tx, Some(0.0));
        assert_eq!(ty, Some(0.0));
    }

    // ── Twist conversion ─────────────────────────────────────────────────────

    /// Twist is converted from deci-degrees to degrees for all conventions.
    #[test]
    fn twist_deci_deg_to_degrees() {
        // 450 deci-deg = 45.0°.
        let raw = make_sample_full(None, None, Some(450), Some(0.0), Some(0.0));
        for convention in [
            TiltConvention::TiltXY,
            TiltConvention::AzimuthAltitude,
            TiltConvention::DerivedFromOrientation,
        ] {
            let cfg = TiltConfig {
                enabled: true,
                convention,
            };
            let (_, _, twist) = cfg.apply(&raw);
            assert_eq!(
                twist,
                Some(45.0),
                "twist conversion failed for {convention:?}"
            );
        }
    }
}
