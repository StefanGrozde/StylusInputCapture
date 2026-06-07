//! Stage 1 — Coordinate mapping.
//!
//! Implements [`CoordinateMap::apply`], which maps raw digitizer coordinates
//! through the stage's affine (and optional projective) matrix and then
//! converts the result into the profile's [`TargetSpace`].
//!
//! The `CoordinateMap` struct itself (fields, `Default`, `identity()`) lives in
//! [`crate::profile`] so it can be serialized and referenced from the profile
//! without an extra import.  This module adds the computational methods.
//!
//! # Transform conventions
//!
//! The affine matrix is stored as `[a, b, c, d, e, f]` representing:
//!
//! ```text
//! | a  b  c |   | x_in |   | x_out |
//! | d  e  f | × |  y_in | = | y_out |
//!             |   1   |
//! ```
//!
//! i.e. `x_out = a*x + b*y + c`,  `y_out = d*x + e*y + f`.
//!
//! The projective matrix is stored as a row-major 3×3 `[h0..h8]`:
//!
//! ```text
//! | h0 h1 h2 |   | x |       | x'w |
//! | h3 h4 h5 | × | y |  =    | y'w |
//! | h6 h7 h8 |   | 1 |       |  w  |
//! ```
//!
//! Final coordinates: `x_out = x'w / w`,  `y_out = y'w / w`.
//!
//! # Target-space units
//!
//! After the matrix transform the resulting `(x, y)` are in the same raw
//! digitizer units as the inputs (they are linear combinations of the raw
//! inputs).  The `TargetSpace` conversion then translates those values into
//! the desired output units:
//!
//! - [`TargetSpace::Normalized`] → divide by the device extent so the output
//!   lies in `[0.0, 1.0]`.
//! - [`TargetSpace::ScreenPixels`] → first normalize, then scale by `(w, h)`.
//! - [`TargetSpace::Millimeters`] → use `resolution` (units per inch or per
//!   cm, according to `unit`) to convert raw digitizer units to mm.

use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities};

use crate::profile::{CoordinateMap, TargetSpace};

// ── CoordinateMap::apply ─────────────────────────────────────────────────────

impl CoordinateMap {
    /// Map a raw digitizer position through this stage's transform and convert
    /// the result into the given `target_space`.
    ///
    /// # Parameters
    ///
    /// - `raw_x`, `raw_y` — raw digitizer values (e.g. `PenSample::x_raw`,
    ///   `y_raw` cast to `f64`).
    /// - `caps` — device capabilities carrying axis extents and resolution.
    /// - `target_space` — the coordinate space requested by the active profile.
    ///
    /// # Return value
    ///
    /// `(x, y)` expressed in `target_space` units after the matrix transform.
    /// When `!self.enabled` callers should skip this call and use the raw
    /// normalized position directly (see [`crate::CalibrationProfile::apply`]).
    ///
    /// # Projection
    ///
    /// When `self.projective` is `Some`, it is applied *after* the affine step
    /// (i.e. the projective matrix transforms the affine-mapped coordinates).
    /// In practice a caller will fill in either the affine *or* the projective
    /// matrix, not both — but chaining is well-defined and harmless.
    pub fn apply(
        &self,
        raw_x: f64,
        raw_y: f64,
        caps: &DeviceCapabilities,
        target_space: &TargetSpace,
    ) -> (f64, f64) {
        // ── Step 1: affine transform ─────────────────────────────────────────
        let [a, b, c, d, e, f] = self.affine;
        let mut tx = a * raw_x + b * raw_y + c;
        let mut ty = d * raw_x + e * raw_y + f;

        // ── Step 2: optional projective transform ────────────────────────────
        if let Some(h) = self.projective {
            let w = h[6] * tx + h[7] * ty + h[8];
            let px = h[0] * tx + h[1] * ty + h[2];
            let py = h[3] * tx + h[4] * ty + h[5];
            if w.abs() > f64::EPSILON {
                tx = px / w;
                ty = py / w;
            }
            // If w ≈ 0, leave (tx, ty) unchanged (degenerate projection).
        }

        // ── Step 3: convert to target space ─────────────────────────────────
        convert_to_target(tx, ty, caps, target_space)
    }
}

// ── Target-space conversion ──────────────────────────────────────────────────

/// Convert a position that is already in raw digitizer units (after any matrix
/// transform) into the requested [`TargetSpace`].
///
/// This is a free function so that the identity path in `lib.rs` can also call
/// it when the coordinate stage is disabled but a non-`Normalized` target space
/// is requested (future use, `TC1.5`).  For now the identity path uses raw
/// `x_norm`/`y_norm` which are already normalized.
pub(crate) fn convert_to_target(
    x_raw: f64,
    y_raw: f64,
    caps: &DeviceCapabilities,
    target_space: &TargetSpace,
) -> (f64, f64) {
    match target_space {
        TargetSpace::Normalized => normalize_xy(x_raw, y_raw, &caps.x, &caps.y),

        TargetSpace::ScreenPixels { w, h } => {
            let (nx, ny) = normalize_xy(x_raw, y_raw, &caps.x, &caps.y);
            (nx * (*w as f64), ny * (*h as f64))
        }

        TargetSpace::Millimeters => (axis_to_mm(x_raw, &caps.x), axis_to_mm(y_raw, &caps.y)),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Normalize raw digitizer values to `[0.0, 1.0]` using the device axis
/// extents.  Mirrors the `tablet_core::normalize` function but operates on
/// `f64` values that may have been transformed by a matrix (so they can lie
/// slightly outside the raw integer range).
fn normalize_xy(x: f64, y: f64, ax: &AxisInfo, ay: &AxisInfo) -> (f64, f64) {
    (normalize_axis(x, ax), normalize_axis(y, ay))
}

fn normalize_axis(value: f64, axis: &AxisInfo) -> f64 {
    let min = axis.min as f64;
    let max = axis.max as f64;
    if (max - min).abs() < f64::EPSILON {
        return 0.0;
    }
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

/// Convert a raw digitizer value to millimeters using axis resolution and unit.
///
/// `AxisInfo::resolution` is "units per inch" or "units per centimeter"
/// according to `AxisInfo::unit` (per SPEC_1.md §5.2 / Wintab spec).
///
/// - `Inch`:       `mm = raw / resolution * 25.4`
/// - `Centimeter`: `mm = raw / resolution * 10.0`
/// - `Degree`/`None`: no meaningful physical conversion; return raw value
///   unchanged (caller should disable this path for non-spatial axes).
fn axis_to_mm(raw: f64, axis: &AxisInfo) -> f64 {
    if axis.resolution <= 0.0 {
        return raw;
    }
    match axis.unit {
        AxisUnit::Inch => raw / axis.resolution * 25.4,
        AxisUnit::Centimeter => raw / axis.resolution * 10.0,
        // Non-spatial axis: return raw value unchanged.
        AxisUnit::Degree | AxisUnit::None => raw,
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities};

    fn make_caps(x_max: i64, y_max: i64) -> DeviceCapabilities {
        let axis = |max: i64| AxisInfo {
            min: 0,
            max,
            resolution: 1000.0, // 1000 units/inch
            unit: AxisUnit::Inch,
            supported: true,
        };
        DeviceCapabilities {
            device_name: String::from("Test"),
            driver_version: String::from("1.0"),
            x: axis(x_max),
            y: axis(y_max),
            z: AxisInfo {
                min: 0,
                max: 0,
                resolution: 0.0,
                unit: AxisUnit::None,
                supported: false,
            },
            pressure: AxisInfo {
                min: 0,
                max: 1023,
                resolution: 0.0,
                unit: AxisUnit::None,
                supported: true,
            },
            tangent_pressure: AxisInfo {
                min: 0,
                max: 0,
                resolution: 0.0,
                unit: AxisUnit::None,
                supported: false,
            },
            azimuth: AxisInfo {
                min: 0,
                max: 3600,
                resolution: 0.0,
                unit: AxisUnit::Degree,
                supported: true,
            },
            altitude: AxisInfo {
                min: 0,
                max: 900,
                resolution: 0.0,
                unit: AxisUnit::Degree,
                supported: true,
            },
            twist: AxisInfo {
                min: 0,
                max: 3600,
                resolution: 0.0,
                unit: AxisUnit::Degree,
                supported: false,
            },
            rotation: AxisInfo {
                min: 0,
                max: 3600,
                resolution: 0.0,
                unit: AxisUnit::Degree,
                supported: false,
            },
            max_packet_rate_hz: 200,
            queue_size: 128,
        }
    }

    // ── identity matrix ──────────────────────────────────────────────────────

    /// An identity affine matrix must leave coordinates unchanged.
    #[test]
    fn identity_affine_passthrough() {
        let cm = CoordinateMap::identity();
        let caps = make_caps(10_000, 8_000);
        let (x, y) = cm.apply(5_000.0, 4_000.0, &caps, &TargetSpace::Normalized);
        // After identity transform: still 5000 / 10000 = 0.5 and 4000 / 8000 = 0.5.
        assert!((x - 0.5).abs() < 1e-12, "x: {x}");
        assert!((y - 0.5).abs() < 1e-12, "y: {y}");
    }

    /// Identity at origin → normalized (0, 0).
    #[test]
    fn identity_at_origin() {
        let cm = CoordinateMap::identity();
        let caps = make_caps(10_000, 8_000);
        let (x, y) = cm.apply(0.0, 0.0, &caps, &TargetSpace::Normalized);
        assert_eq!(x, 0.0);
        assert_eq!(y, 0.0);
    }

    /// Identity at max → normalized (1, 1).
    #[test]
    fn identity_at_max() {
        let cm = CoordinateMap::identity();
        let caps = make_caps(10_000, 8_000);
        let (x, y) = cm.apply(10_000.0, 8_000.0, &caps, &TargetSpace::Normalized);
        assert!((x - 1.0).abs() < 1e-12, "x: {x}");
        assert!((y - 1.0).abs() < 1e-12, "y: {y}");
    }

    // ── TargetSpace::ScreenPixels ────────────────────────────────────────────

    /// Normalized centre → screen centre.
    #[test]
    fn screen_pixels_centre() {
        let cm = CoordinateMap::identity();
        let caps = make_caps(10_000, 8_000);
        let (x, y) = cm.apply(
            5_000.0,
            4_000.0,
            &caps,
            &TargetSpace::ScreenPixels { w: 1920, h: 1080 },
        );
        assert!((x - 960.0).abs() < 1e-9, "x: {x}");
        assert!((y - 540.0).abs() < 1e-9, "y: {y}");
    }

    /// Max digitizer → (w, h).
    #[test]
    fn screen_pixels_max() {
        let cm = CoordinateMap::identity();
        let caps = make_caps(10_000, 8_000);
        let (x, y) = cm.apply(
            10_000.0,
            8_000.0,
            &caps,
            &TargetSpace::ScreenPixels { w: 1920, h: 1080 },
        );
        assert!((x - 1920.0).abs() < 1e-9, "x: {x}");
        assert!((y - 1080.0).abs() < 1e-9, "y: {y}");
    }

    // ── TargetSpace::Millimeters ─────────────────────────────────────────────

    /// 1000 units/inch: 1000 raw units → 25.4 mm.
    #[test]
    fn millimeters_conversion_inch() {
        let cm = CoordinateMap::identity();
        let caps = make_caps(10_000, 8_000);
        // caps.x.resolution = 1000 units/inch → 1000 raw = 1 inch = 25.4 mm.
        let (x, _y) = cm.apply(1_000.0, 0.0, &caps, &TargetSpace::Millimeters);
        assert!((x - 25.4).abs() < 1e-9, "x mm: {x}");
    }

    /// Centimeter resolution: 100 units/cm, 100 raw → 10 mm.
    #[test]
    fn millimeters_conversion_cm() {
        let mut caps = make_caps(1_000, 1_000);
        caps.x.resolution = 100.0;
        caps.x.unit = AxisUnit::Centimeter;
        let cm = CoordinateMap::identity();
        let (x, _) = cm.apply(100.0, 0.0, &caps, &TargetSpace::Millimeters);
        // 100 raw / 100 units-per-cm × 10 mm/cm = 10 mm
        assert!((x - 10.0).abs() < 1e-9, "x mm: {x}");
    }

    // ── Non-identity affine ──────────────────────────────────────────────────

    /// Scale × 2 in x, uniform y.
    #[test]
    fn affine_scale_x2() {
        let cm = CoordinateMap {
            enabled: true,
            // [ 2  0  0 ]  (scale x by 2, y unchanged)
            // [ 0  1  0 ]
            affine: [2.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            projective: None,
        };
        let caps = make_caps(10_000, 8_000);
        // raw: (1000, 1000) → affine → (2000, 1000).
        // Normalized: x = 2000/10000 = 0.2, y = 1000/8000 = 0.125.
        let (x, y) = cm.apply(1_000.0, 1_000.0, &caps, &TargetSpace::Normalized);
        assert!((x - 0.2).abs() < 1e-12, "x: {x}");
        assert!((y - 0.125).abs() < 1e-12, "y: {y}");
    }

    /// Affine translation.
    #[test]
    fn affine_translation() {
        let cm = CoordinateMap {
            enabled: true,
            // [ 1  0  500 ]  (shift x by +500)
            // [ 0  1  0   ]
            affine: [1.0, 0.0, 500.0, 0.0, 1.0, 0.0],
            projective: None,
        };
        let caps = make_caps(10_000, 8_000);
        let (x, _) = cm.apply(0.0, 0.0, &caps, &TargetSpace::Normalized);
        // 500 / 10000 = 0.05
        assert!((x - 0.05).abs() < 1e-12, "x: {x}");
    }

    // ── Projective transform ─────────────────────────────────────────────────

    /// Identity projective matrix must leave coordinates unchanged.
    #[test]
    fn projective_identity() {
        let cm = CoordinateMap {
            enabled: true,
            affine: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            // 3×3 identity
            projective: Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
        };
        let caps = make_caps(10_000, 8_000);
        let (x, y) = cm.apply(5_000.0, 4_000.0, &caps, &TargetSpace::Normalized);
        assert!((x - 0.5).abs() < 1e-12, "x: {x}");
        assert!((y - 0.5).abs() < 1e-12, "y: {y}");
    }
}
