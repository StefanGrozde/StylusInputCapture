//! Stage 2 — Pressure response.
//!
//! Implements [`PressureCurve::apply`], which maps a raw device pressure value
//! (`u32`) through a three-step pipeline:
//!
//! 1. **Clamp** — values below [`PressureCurve::pressure_min`] are lifted to
//!    `min`; values above [`PressureCurve::pressure_max`] are clamped to `max`.
//! 2. **Normalize** — the clamped value is mapped linearly to `[0.0, 1.0]`
//!    over the `[min, max]` window.
//! 3. **Curve** — the normalized value is shaped by the active [`CurveKind`].
//!
//! The output is always in `[0.0, 1.0]`.
//!
//! Also provides [`learn_min_max`], a pure helper that scans a slice of
//! [`PenSample`]s and returns the observed `(min, max)` raw pressure range.
//!
//! [`PenSample`]: tablet_core::PenSample

use tablet_core::PenSample;

use crate::profile::{CurveKind, PressureCurve};

// ── PressureCurve::apply ─────────────────────────────────────────────────────

impl PressureCurve {
    /// Map a raw device pressure value through this stage's pipeline.
    ///
    /// # Steps
    ///
    /// 1. Clamp `raw_pressure` to `[pressure_min, pressure_max]`.
    /// 2. Normalize the clamped value to `[0.0, 1.0]` over `[min, max]`.
    ///    If `min == max` the output is `0.0`.
    /// 3. Apply the active [`CurveKind`] to the normalized value.
    ///
    /// # Return value
    ///
    /// A value in `[0.0, 1.0]`.  The result is guaranteed to be within that
    /// range regardless of the input or curve parameters.
    ///
    /// # When to call
    ///
    /// Only call this when `self.enabled == true`.  The caller (see
    /// [`crate::CalibrationProfile::apply`]) is responsible for falling back
    /// to `PenSample::pressure_norm` when the stage is disabled.
    pub fn apply(&self, raw_pressure: u32) -> f64 {
        // ── Step 1: clamp to [min, max] ──────────────────────────────────────
        let clamped = raw_pressure.clamp(self.pressure_min, self.pressure_max);

        // ── Step 2: normalize to [0.0, 1.0] over [min, max] ─────────────────
        let t = normalize_raw(clamped, self.pressure_min, self.pressure_max);

        // ── Step 3: apply curve ──────────────────────────────────────────────
        let shaped = apply_curve(t, &self.curve);

        // Final clamp to guard against floating-point edge cases.
        shaped.clamp(0.0, 1.0)
    }
}

// ── Curve evaluation ─────────────────────────────────────────────────────────

/// Evaluate a [`CurveKind`] at normalized input `t ∈ [0.0, 1.0]`.
///
/// Returns a value nominally in `[0.0, 1.0]`; the caller applies a final
/// `clamp` for robustness.
fn apply_curve(t: f64, curve: &CurveKind) -> f64 {
    match curve {
        CurveKind::Linear => t,

        CurveKind::Gamma(g) => {
            // Guard against degenerate exponents.
            let g = if *g > 0.0 { *g } else { 1.0 };
            t.powf(g)
        }

        CurveKind::Custom(points) => {
            if points.is_empty() {
                // No control points → fall back to linear.
                return t;
            }
            piecewise_linear(t, points)
        }
    }
}

/// Normalize a raw `u32` value within `[min, max]` to `[0.0, 1.0]`.
///
/// Returns `0.0` when `min == max` (degenerate range).
fn normalize_raw(value: u32, min: u32, max: u32) -> f64 {
    if max == min {
        return 0.0;
    }
    let v = value as f64;
    let lo = min as f64;
    let hi = max as f64;
    // Result is in [0.0, 1.0] because `value` was clamped to [min, max].
    (v - lo) / (hi - lo)
}

/// Evaluate a piecewise-linear curve at `t`.
///
/// `points` must be **sorted by the first element** (the input axis) and
/// should be monotone (non-decreasing in the second element).  Extrapolation
/// beyond the first / last control point uses the boundary value (clamp).
fn piecewise_linear(t: f64, points: &[(f64, f64)]) -> f64 {
    debug_assert!(
        !points.is_empty(),
        "piecewise_linear called with empty points"
    );

    // Clamp to boundary values if t is outside the defined range.
    if t <= points[0].0 {
        return points[0].1;
    }
    let last = points[points.len() - 1];
    if t >= last.0 {
        return last.1;
    }

    // Find the segment containing `t` via linear search.
    // For the small number of control points expected in practice (< 32), a
    // linear scan is faster than a binary search due to cache locality.
    for window in points.windows(2) {
        let (t0, v0) = window[0];
        let (t1, v1) = window[1];
        if t >= t0 && t <= t1 {
            if (t1 - t0).abs() < f64::EPSILON {
                // Coincident x-coordinates: return the right-side value.
                return v1;
            }
            // Linear interpolation between the two control points.
            let alpha = (t - t0) / (t1 - t0);
            return v0 + alpha * (v1 - v0);
        }
    }

    // Unreachable in valid input (t is between first and last points and a
    // matching window was not found — only possible with unsorted points).
    // Fall back to the last value.
    last.1
}

// ── learn_min_max ─────────────────────────────────────────────────────────────

/// Return the observed raw pressure range `(min, max)` from a slice of
/// [`PenSample`]s.
///
/// # Edge case — empty slice
///
/// Returns `(0, 0)` for an empty slice.  Callers should treat this as "no
/// data observed" and avoid using the result as a clamp range (a min==max
/// clamp would produce all-zero pressure output).
///
/// # Usage
///
/// This helper is typically called by the UI's "learn min/max" button with
/// a recent history of samples to set [`PressureCurve::pressure_min`] and
/// [`PressureCurve::pressure_max`].
///
/// ```rust
/// use tablet_process::learn_min_max;
/// // let (min, max) = learn_min_max(&recent_samples);
/// // profile.pressure.pressure_min = min;
/// // profile.pressure.pressure_max = max;
/// ```
pub fn learn_min_max(samples: &[PenSample]) -> (u32, u32) {
    if samples.is_empty() {
        return (0, 0);
    }
    let min = samples.iter().map(|s| s.pressure_raw).min().unwrap_or(0);
    let max = samples.iter().map(|s| s.pressure_raw).max().unwrap_or(0);
    (min, max)
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_core::ToolKind;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_curve(min: u32, max: u32, curve: CurveKind) -> PressureCurve {
        PressureCurve {
            enabled: true,
            pressure_min: min,
            pressure_max: max,
            curve,
        }
    }

    fn make_sample(pressure_raw: u32) -> PenSample {
        PenSample {
            t_capture_ns: 0,
            t_device_ms: 0,
            serial: 0,
            x_raw: 0,
            y_raw: 0,
            z_raw: 0,
            x_norm: 0.0,
            y_norm: 0.0,
            pressure_raw,
            pressure_norm: 0.0,
            tangent_pressure_raw: None,
            azimuth_deci_deg: None,
            altitude_deci_deg: None,
            twist_deci_deg: None,
            tilt_x_deg: None,
            tilt_y_deg: None,
            rotation_deci_deg: None,
            buttons: 0,
            tool: ToolKind::Pen,
            tool_serial: 0,
            in_proximity: true,
            status: 0,
        }
    }

    // ── clamp edges ──────────────────────────────────────────────────────────

    /// Raw pressure at min → output 0.0.
    #[test]
    fn clamp_at_min_gives_zero() {
        let curve = make_curve(100, 900, CurveKind::Linear);
        assert_eq!(curve.apply(100), 0.0);
    }

    /// Raw pressure at max → output 1.0.
    #[test]
    fn clamp_at_max_gives_one() {
        let curve = make_curve(100, 900, CurveKind::Linear);
        assert_eq!(curve.apply(900), 1.0);
    }

    /// Raw pressure below min → clamped to min → output 0.0.
    #[test]
    fn clamp_below_min() {
        let curve = make_curve(100, 900, CurveKind::Linear);
        assert_eq!(curve.apply(0), 0.0);
    }

    /// Raw pressure above max → clamped to max → output 1.0.
    #[test]
    fn clamp_above_max() {
        let curve = make_curve(100, 900, CurveKind::Linear);
        assert_eq!(curve.apply(u32::MAX), 1.0);
    }

    /// Degenerate range (min == max) → output is always 0.0.
    #[test]
    fn degenerate_range_min_equals_max() {
        let curve = make_curve(500, 500, CurveKind::Linear);
        assert_eq!(curve.apply(500), 0.0);
        assert_eq!(curve.apply(0), 0.0);
        assert_eq!(curve.apply(1000), 0.0);
    }

    // ── Linear ───────────────────────────────────────────────────────────────

    /// Linear curve at midpoint → 0.5.
    #[test]
    fn linear_midpoint() {
        let curve = make_curve(0, 1000, CurveKind::Linear);
        assert!((curve.apply(500) - 0.5).abs() < 1e-12);
    }

    /// Linear curve is identity: apply(x) == (x - min) / (max - min).
    #[test]
    fn linear_quarter_point() {
        let curve = make_curve(0, 1000, CurveKind::Linear);
        assert!((curve.apply(250) - 0.25).abs() < 1e-12);
    }

    // ── Gamma ─────────────────────────────────────────────────────────────────

    /// Gamma curve: endpoints 0 → 0.0 and max → 1.0.
    #[test]
    fn gamma_endpoints() {
        let curve = make_curve(0, 1000, CurveKind::Gamma(2.0));
        assert_eq!(curve.apply(0), 0.0, "gamma at min");
        assert!((curve.apply(1000) - 1.0).abs() < 1e-12, "gamma at max");
    }

    /// Gamma(2.0) at midpoint → 0.25 (= 0.5^2).
    #[test]
    fn gamma_2_midpoint() {
        let curve = make_curve(0, 1000, CurveKind::Gamma(2.0));
        let result = curve.apply(500);
        assert!((result - 0.25).abs() < 1e-12, "gamma(2) midpoint: {result}");
    }

    /// Gamma curve is monotonically non-decreasing.
    #[test]
    fn gamma_monotonic() {
        let curve = make_curve(0, 1000, CurveKind::Gamma(3.0));
        let mut prev = curve.apply(0);
        for raw in (0..=1000u32).step_by(50) {
            let v = curve.apply(raw);
            assert!(
                v >= prev - 1e-15,
                "gamma not monotonic at raw={raw}: v={v}, prev={prev}"
            );
            prev = v;
        }
    }

    /// Gamma(0.5) at midpoint → ~0.707 (= 0.5^0.5).
    #[test]
    fn gamma_half_midpoint() {
        let curve = make_curve(0, 1000, CurveKind::Gamma(0.5));
        let result = curve.apply(500);
        let expected = 0.5_f64.sqrt();
        assert!(
            (result - expected).abs() < 1e-12,
            "gamma(0.5) midpoint: {result}"
        );
    }

    /// Degenerate Gamma(0.0) falls back to linear (g treated as 1.0).
    #[test]
    fn gamma_zero_fallback_to_linear() {
        let curve = make_curve(0, 1000, CurveKind::Gamma(0.0));
        let result = curve.apply(500);
        // Gamma(0.0) is treated as 1.0 → linear → 0.5.
        assert!((result - 0.5).abs() < 1e-12, "gamma(0) fallback: {result}");
    }

    // ── Custom (piecewise-linear) ────────────────────────────────────────────

    /// Custom curve hits its own control points exactly.
    #[test]
    fn custom_hits_control_points() {
        let points = vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)];
        let curve = make_curve(0, 1000, CurveKind::Custom(points));
        // At t=0.0 (raw=0): output = 0.0
        assert!((curve.apply(0) - 0.0).abs() < 1e-12, "at t=0");
        // At t=0.5 (raw=500): output = 0.3
        assert!((curve.apply(500) - 0.3).abs() < 1e-12, "at t=0.5");
        // At t=1.0 (raw=1000): output = 1.0
        assert!((curve.apply(1000) - 1.0).abs() < 1e-12, "at t=1.0");
    }

    /// Custom curve interpolates correctly between control points.
    #[test]
    fn custom_interpolates_between_points() {
        // Between (0.0, 0.0) and (0.5, 0.3): at t=0.25 → 0.15.
        let points = vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)];
        let curve = make_curve(0, 1000, CurveKind::Custom(points));
        let result = curve.apply(250); // t = 0.25
        let expected = 0.15; // linear interp: 0.0 + (0.25/0.5) * 0.3
        assert!(
            (result - expected).abs() < 1e-12,
            "custom interp: {result}, expected {expected}"
        );
    }

    /// Custom with no control points falls back to linear.
    #[test]
    fn custom_empty_falls_back_to_linear() {
        let curve = make_curve(0, 1000, CurveKind::Custom(vec![]));
        assert!((curve.apply(500) - 0.5).abs() < 1e-12);
    }

    /// Custom curve extrapolates by clamping at the boundary values.
    #[test]
    fn custom_extrapolation_clamped() {
        // Points only defined in [0.2, 0.8].
        let points = vec![(0.2, 0.1), (0.8, 0.9)];
        let curve = make_curve(0, 1000, CurveKind::Custom(points));
        // t = 0.0 < 0.2 → output = 0.1 (first control-point value).
        let below = curve.apply(0);
        assert!((below - 0.1).abs() < 1e-12, "extrapolate below: {below}");
        // t = 1.0 > 0.8 → output = 0.9 (last control-point value).
        let above = curve.apply(1000);
        assert!((above - 0.9).abs() < 1e-12, "extrapolate above: {above}");
    }

    // ── Output always in [0, 1] ───────────────────────────────────────────────

    /// All curve kinds produce output in [0.0, 1.0] for all raw values.
    #[test]
    fn output_always_in_range() {
        let curves = vec![
            make_curve(100, 900, CurveKind::Linear),
            make_curve(100, 900, CurveKind::Gamma(0.5)),
            make_curve(100, 900, CurveKind::Gamma(2.0)),
            make_curve(
                100,
                900,
                CurveKind::Custom(vec![(0.0, 0.0), (0.25, 0.6), (0.75, 0.8), (1.0, 1.0)]),
            ),
        ];
        // Test at boundaries and a sweep of raw values.
        for curve in &curves {
            for raw in [0u32, 100, 200, 500, 700, 900, 1000, u32::MAX] {
                let v = curve.apply(raw);
                assert!(
                    (0.0..=1.0).contains(&v),
                    "output out of range: {v} (raw={raw})"
                );
            }
        }
    }

    // ── learn_min_max ─────────────────────────────────────────────────────────

    /// `learn_min_max` returns `(0, 0)` for an empty slice.
    #[test]
    fn learn_min_max_empty() {
        assert_eq!(learn_min_max(&[]), (0, 0));
    }

    /// `learn_min_max` returns the true min and max from a synthetic set.
    #[test]
    fn learn_min_max_synthetic() {
        let pressures = [0u32, 150, 700, 1023, 512, 42, 999];
        let samples: Vec<PenSample> = pressures.iter().map(|&p| make_sample(p)).collect();
        let (min, max) = learn_min_max(&samples);
        assert_eq!(min, 0, "min");
        assert_eq!(max, 1023, "max");
    }

    /// `learn_min_max` returns the single value for a one-element slice.
    #[test]
    fn learn_min_max_single() {
        let samples = [make_sample(500)];
        assert_eq!(learn_min_max(&samples), (500, 500));
    }

    /// `learn_min_max` returns the same value for identical pressures.
    #[test]
    fn learn_min_max_uniform() {
        let samples: Vec<PenSample> = (0..5).map(|_| make_sample(300)).collect();
        assert_eq!(learn_min_max(&samples), (300, 300));
    }
}
