/// Normalize a raw axis value into the range `[0.0, 1.0]`.
///
/// Maps `raw` linearly over the closed interval `[min, max]` and clamps the
/// result so out-of-range inputs never produce values outside `[0.0, 1.0]`.
///
/// # Edge cases
/// - If `max == min` (degenerate axis), returns `0.0` rather than dividing by zero.
/// - Values below `min` clamp to `0.0`; values above `max` clamp to `1.0`.
pub fn normalize(raw: i64, min: i64, max: i64) -> f64 {
    if max == min {
        return 0.0;
    }
    let raw_f = raw as f64;
    let min_f = min as f64;
    let max_f = max as f64;
    ((raw_f - min_f) / (max_f - min_f)).clamp(0.0, 1.0)
}

/// Derive Cartesian tilt angles (in degrees) from Wintab orientation angles.
///
/// # Coordinate conventions
/// Wintab reports orientation as:
/// - `azimuth_deci_deg`: compass angle of the pen in the tablet plane, in
///   0.1° units (0 = pointing toward positive X, increasing counter-clockwise).
/// - `altitude_deci_deg`: angle from the tablet plane to the pen shaft, in 0.1°
///   units (0 = flat on the tablet, 900 = perfectly vertical / pen perpendicular
///   to the tablet surface).
///
/// # Formula
/// Inputs are converted to radians by dividing by 10 (to get degrees) then
/// multiplying by π/180:
///
/// ```text
/// az  = azimuth_deci_deg  / 10 * π/180
/// alt = altitude_deci_deg / 10 * π/180
///
/// tilt_x_deg = atan(cos(az) / tan(alt)) * 180/π
/// tilt_y_deg = atan(sin(az) / tan(alt)) * 180/π
/// ```
///
/// # Special case
/// When `altitude_deci_deg == 900` (pen is perfectly vertical / 90°), `tan(alt)`
/// diverges and the tilt is zero by definition.  Returns `(0.0, 0.0)`.
///
/// # Returns
/// `(tilt_x_deg, tilt_y_deg)` in degrees.
pub fn tilt_from_orientation(azimuth_deci_deg: i32, altitude_deci_deg: i32) -> (f64, f64) {
    // Pen is perfectly vertical (altitude == 90° == 900 deci-deg): no tilt.
    if altitude_deci_deg == 900 {
        return (0.0, 0.0);
    }

    // Units: 0.1° per integer step (deci-degrees).  Divide by 10 to get degrees,
    // then multiply by π/180 to get radians.
    // Example: altitude_deci_deg = 900 → 900/10 = 90° → π/2 rad (pen vertical).
    let az_rad = (azimuth_deci_deg as f64) / 10.0 * (std::f64::consts::PI / 180.0);
    let alt_rad = (altitude_deci_deg as f64) / 10.0 * (std::f64::consts::PI / 180.0);

    let tan_alt = alt_rad.tan();
    // If altitude is effectively zero (pen lying flat), tan_alt -> 0 and the
    // tilt magnitude approaches 90°; clamp via atan which is bounded.
    let tilt_x = (az_rad.cos() / tan_alt).atan() * (180.0 / std::f64::consts::PI);
    let tilt_y = (az_rad.sin() / tan_alt).atan() * (180.0 / std::f64::consts::PI);

    (tilt_x, tilt_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- normalize tests ---

    #[test]
    fn normalize_min_value() {
        assert_eq!(normalize(0, 0, 1000), 0.0);
    }

    #[test]
    fn normalize_max_value() {
        assert_eq!(normalize(1000, 0, 1000), 1.0);
    }

    #[test]
    fn normalize_midpoint() {
        let result = normalize(500, 0, 1000);
        assert!((result - 0.5).abs() < 1e-12, "expected ~0.5, got {result}");
    }

    #[test]
    fn normalize_clamp_below() {
        // Value below min must clamp to 0.0.
        assert_eq!(normalize(-50, 0, 1000), 0.0);
    }

    #[test]
    fn normalize_clamp_above() {
        // Value above max must clamp to 1.0.
        assert_eq!(normalize(1500, 0, 1000), 1.0);
    }

    #[test]
    fn normalize_degenerate_max_equals_min() {
        // Must not divide by zero; return 0.0 for safety.
        assert_eq!(normalize(42, 42, 42), 0.0);
        assert_eq!(normalize(0, 5, 5), 0.0);
    }

    #[test]
    fn normalize_non_zero_min() {
        // Axis that doesn't start at zero.
        let result = normalize(150, 100, 200);
        assert!((result - 0.5).abs() < 1e-12, "expected ~0.5, got {result}");
    }

    // --- tilt_from_orientation tests ---

    #[test]
    fn tilt_vertical_pen_is_zero() {
        // altitude = 900 deci-deg = 90° (900 × 0.1° = 90°): pen perpendicular to tablet.
        let (tx, ty) = tilt_from_orientation(0, 900);
        assert_eq!(tx, 0.0);
        assert_eq!(ty, 0.0);
    }

    #[test]
    fn tilt_vertical_pen_any_azimuth() {
        // For a vertical pen, azimuth is irrelevant — tilt must still be (0, 0).
        let (tx, ty) = tilt_from_orientation(3600, 900);
        assert_eq!(tx, 0.0);
        assert_eq!(ty, 0.0);
    }

    #[test]
    fn tilt_known_pair() {
        // azimuth = 0 deci-deg (0°, pointing +X), altitude = 450 deci-deg (45°).
        // Each unit is 0.1°, so 450 × 0.1° = 45°.
        // az_rad = 0, alt_rad = π/4 => tan(alt) = 1.
        // tilt_x = atan(cos(0) / 1) * 180/π = atan(1) * 180/π = 45°.
        // tilt_y = atan(sin(0) / 1) * 180/π = atan(0) * 180/π = 0°.
        let (tx, ty) = tilt_from_orientation(0, 450);
        assert!((tx - 45.0).abs() < 1e-6, "tilt_x: expected 45.0, got {tx}");
        assert!(ty.abs() < 1e-6, "tilt_y: expected 0.0, got {ty}");
    }

    #[test]
    fn tilt_azimuth_90_degrees() {
        // azimuth = 900 deci-deg (= 90°), altitude = 450 deci-deg (= 45°).
        // Each unit is 0.1°, so 900 × 0.1° = 90°, 450 × 0.1° = 45°.
        // az_rad = π/2, alt_rad = π/4 => tan(alt) = 1.
        // tilt_x = atan(cos(π/2) / 1) ≈ atan(0) = 0°.
        // tilt_y = atan(sin(π/2) / 1) = atan(1) = 45°.
        let (tx, ty) = tilt_from_orientation(900, 450);
        assert!(tx.abs() < 1e-6, "tilt_x: expected ~0.0, got {tx}");
        assert!((ty - 45.0).abs() < 1e-6, "tilt_y: expected 45.0, got {ty}");
    }

    /// Compile-time assertion that `PenSample` implements `Copy`.
    #[test]
    fn pen_sample_is_copy() {
        fn _assert_copy<T: Copy>() {}
        _assert_copy::<crate::PenSample>();
    }
}
