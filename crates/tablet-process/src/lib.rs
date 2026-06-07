//! `tablet-process` — pure, OS-agnostic pen-input processing library.
//!
//! Provides a [`CalibrationProfile`] that transforms a raw [`PenSample`] into
//! a [`ProcessedSample`] through an ordered, per-stage pipeline.  The library
//! has **no OS, UI, or stream dependencies** and is designed to be embedded in
//! both the calibration UI (`tablet-ui`) and downstream consumer features that
//! want to apply the same tuned profile.
//!
//! # Quick start
//!
//! ```rust
//! use tablet_process::{CalibrationProfile, ProcessorState};
//! use tablet_core::DeviceCapabilities;
//!
//! // `caps` would come from the DeviceCapabilities handshake frame.
//! // `raw` would come from the stream reader.
//! // let processed = CalibrationProfile::identity()
//! //     .apply(&raw, &caps, &mut ProcessorState::new());
//! ```
//!
//! [`PenSample`]: tablet_core::PenSample

pub mod fit;
pub mod io;
pub mod profile;
pub mod sample;
pub mod stages;
pub mod state;

pub use fit::{fit_geometry, FitReport};
pub use io::ProfileError;
pub use profile::{
    ActivationConfig, CalibrationProfile, CoordinateMap, CurveKind, OneEuroParams, PressureCurve,
    SmoothingConfig, SmoothingMode, TargetSpace, TiltConfig, TiltConvention,
};
pub use sample::ProcessedSample;
pub use stages::pressure::learn_min_max;
pub use state::ProcessorState;

use tablet_core::{DeviceCapabilities, PenSample};

impl CalibrationProfile {
    /// Apply this profile to a raw [`PenSample`], producing a
    /// [`ProcessedSample`].
    ///
    /// # Identity behaviour
    ///
    /// When all stages are disabled (as in [`CalibrationProfile::identity()`]):
    /// - `x` and `y` equal `raw.x_norm` and `raw.y_norm`.
    /// - `pressure` equals `raw.pressure_norm`.
    /// - `x_filtered` / `y_filtered` are `None`.
    /// - `tilt_x` / `tilt_y` are copied from `raw.tilt_x_deg` /
    ///   `raw.tilt_y_deg`.
    /// - `twist` is `raw.twist_deci_deg` converted from deci-degrees to degrees.
    /// - `active` equals `raw.in_proximity`.
    /// - `out_of_range` is `false`.
    /// - `raw` is the unmodified input sample.
    ///
    /// # Parameters
    ///
    /// - `raw`   — the unmodified capture sample.
    /// - `caps`  — device axis ranges; used by the coordinate-mapping stage.
    /// - `state` — per-stream mutable state (smoothing filter history,
    ///             activation latch); must be threaded through all calls.
    pub fn apply(
        &self,
        raw: &PenSample,
        caps: &DeviceCapabilities,
        state: &mut ProcessorState,
    ) -> ProcessedSample {
        // Stage 1 — Coordinate mapping (TC1.2).
        // When enabled, map raw digitizer units → target space via the
        // affine / projective transform stored in `coordinate_map`.
        // When disabled, fall back to raw normalized position (identity
        // behaviour from TC1.1).
        let (x, y) = if self.coordinate_map.enabled {
            self.coordinate_map
                .apply(raw.x_raw as f64, raw.y_raw as f64, caps, &self.target_space)
        } else {
            (raw.x_norm, raw.y_norm)
        };

        // Stage 2 — Pressure response (TC1.3).
        // When enabled, clamp and remap raw device pressure through the
        // configured curve; otherwise use raw normalized pressure as-is.
        let pressure = if self.pressure.enabled {
            self.pressure.apply(raw.pressure_raw)
        } else {
            raw.pressure_norm
        };

        // Stage 3 — Tilt / orientation derivation (TC1.4).
        // When enabled, apply the configured tilt convention / derivation.
        // When disabled, fall back to TC1.1 pass-through: copy tilt_x_deg /
        // tilt_y_deg from the raw sample and convert twist_deci_deg to degrees.
        let (tilt_x, tilt_y, twist) = if self.tilt.enabled {
            self.tilt.apply(raw)
        } else {
            // Identity: copy raw Cartesian tilt and convert twist units.
            let twist = raw.twist_deci_deg.map(|d| d as f64 / 10.0);
            (raw.tilt_x_deg, raw.tilt_y_deg, twist)
        };

        // Stage 4 — Position smoothing (TC1.4).
        // When enabled, feed the mapped (x, y) through the configured filter;
        // the filter state is held in `state`.  The time delta is derived from
        // the sample timestamps (nanoseconds → seconds).  When disabled,
        // x_filtered / y_filtered are None.
        //
        // Note: the first sample always uses dt = 0.0 (no previous timestamp
        // available), which causes the filter to echo the input unchanged.
        let dt_seconds = if let Some(prev_ns) = state.last_t_capture_ns {
            let delta_ns = raw.t_capture_ns.saturating_sub(prev_ns);
            (delta_ns as f64) * 1e-9
        } else {
            0.0
        };
        state.last_t_capture_ns = Some(raw.t_capture_ns);

        let (x_filtered, y_filtered) = self.smoothing.apply(x, y, dt_seconds, state);

        // Stage 5 — Activation threshold (TC1.4).
        // When enabled, derive `active` from pressure hysteresis; the latch
        // is held in `state`.  When disabled, fall back to TC1.1 pass-through:
        // active ⟺ pen is in proximity.
        let active = if self.activation.enabled {
            self.activation.apply(pressure, raw.in_proximity, state)
        } else {
            raw.in_proximity
        };

        // Out-of-range detection (TC1.5).
        //
        // A sample is flagged out-of-range when any axis value falls outside
        // the range advertised by `DeviceCapabilities`:
        //
        // - X position: `raw.x_raw` outside `caps.x.min..=caps.x.max`.
        // - Y position: `raw.y_raw` outside `caps.y.min..=caps.y.max`.
        // - Pressure:   `raw.pressure_raw` outside `caps.pressure.min..=caps.pressure.max`
        //               (using `i64` cast to handle `AxisInfo.min/max` which are `i64`).
        //
        // Only axes reported as supported by the device are checked.
        let out_of_range = {
            let xv = i64::from(raw.x_raw);
            let yv = i64::from(raw.y_raw);
            let pv = i64::from(raw.pressure_raw);
            let x_oor = caps.x.supported && (xv < caps.x.min || xv > caps.x.max);
            let y_oor = caps.y.supported && (yv < caps.y.min || yv > caps.y.max);
            let p_oor =
                caps.pressure.supported && (pv < caps.pressure.min || pv > caps.pressure.max);
            x_oor || y_oor || p_oor
        };

        ProcessedSample {
            raw: *raw,
            x,
            y,
            x_filtered,
            y_filtered,
            pressure,
            active,
            tilt_x,
            tilt_y,
            twist,
            out_of_range,
        }
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities, ToolKind};

    /// Build a minimal `DeviceCapabilities` for testing (all axes use dummy
    /// ranges; only the struct shape matters for the identity skeleton).
    fn dummy_caps() -> DeviceCapabilities {
        let axis = AxisInfo {
            min: 0,
            max: 1000,
            resolution: 100.0,
            unit: AxisUnit::None,
            supported: true,
        };
        DeviceCapabilities {
            device_name: String::from("Test Device"),
            driver_version: String::from("1.0"),
            x: axis,
            y: axis,
            z: axis,
            pressure: axis,
            tangent_pressure: axis,
            azimuth: axis,
            altitude: axis,
            twist: axis,
            rotation: axis,
            max_packet_rate_hz: 200,
            queue_size: 128,
        }
    }

    /// Build a `PenSample` with known normalized values for assertions.
    fn make_sample(x_norm: f64, y_norm: f64, pressure_norm: f64) -> PenSample {
        PenSample {
            t_capture_ns: 1_000_000,
            t_device_ms: 100,
            serial: 1,
            x_raw: 500,
            y_raw: 500,
            z_raw: 0,
            x_norm,
            y_norm,
            pressure_raw: 512,
            pressure_norm,
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

    /// Identity profile: `apply` must be a faithful pass-through.
    ///
    /// - `processed.raw` must equal the input sample.
    /// - `x`/`y`/`pressure` must equal the raw normalized values.
    #[test]
    fn identity_apply_passthrough() {
        let raw = make_sample(0.25, 0.75, 0.5);
        let caps = dummy_caps();
        let mut state = ProcessorState::new();

        let processed = CalibrationProfile::identity().apply(&raw, &caps, &mut state);

        assert_eq!(processed.raw, raw, "raw field must equal the input sample");
        assert_eq!(processed.x, raw.x_norm, "x must equal x_norm");
        assert_eq!(processed.y, raw.y_norm, "y must equal y_norm");
        assert_eq!(
            processed.pressure, raw.pressure_norm,
            "pressure must equal pressure_norm"
        );
        assert_eq!(processed.x_filtered, None, "x_filtered must be None");
        assert_eq!(processed.y_filtered, None, "y_filtered must be None");
        assert_eq!(processed.tilt_x, raw.tilt_x_deg);
        assert_eq!(processed.tilt_y, raw.tilt_y_deg);
        assert_eq!(processed.active, raw.in_proximity);
        assert!(!processed.out_of_range);
    }

    /// Twist conversion: `twist_deci_deg` (0.1°) must be converted to degrees.
    #[test]
    fn twist_deci_deg_conversion() {
        let mut raw = make_sample(0.5, 0.5, 0.5);
        raw.twist_deci_deg = Some(450); // 45.0°

        let caps = dummy_caps();
        let mut state = ProcessorState::new();

        let processed = CalibrationProfile::identity().apply(&raw, &caps, &mut state);

        assert_eq!(processed.twist, Some(45.0));
    }

    /// `ProcessorState::new()` and `Default` must produce equal values.
    #[test]
    fn processor_state_new_equals_default() {
        assert_eq!(ProcessorState::new(), ProcessorState::default());
    }

    /// `CalibrationProfile::identity()` and `Default` must produce equal values.
    #[test]
    fn profile_identity_equals_default() {
        assert_eq!(
            CalibrationProfile::identity(),
            CalibrationProfile::default()
        );
    }

    // ── TC1.2: coordinate-mapping stage wiring ───────────────────────────────

    /// When `coordinate_map.enabled = false`, `apply` uses raw normalized
    /// position regardless of `target_space`.
    #[test]
    fn disabled_coordinate_stage_uses_raw_normalized() {
        let raw = make_sample(0.3, 0.7, 0.5);
        let caps = dummy_caps();
        let mut state = ProcessorState::new();
        let mut profile = CalibrationProfile::identity();
        profile.coordinate_map.enabled = false;

        let processed = profile.apply(&raw, &caps, &mut state);

        assert_eq!(processed.x, raw.x_norm);
        assert_eq!(processed.y, raw.y_norm);
    }

    /// When `coordinate_map.enabled = true` with the identity affine,
    /// `apply` should produce the same normalized position as the raw
    /// `x_norm`/`y_norm` (because raw_x/max == x_norm when the axis spans
    /// [0, max] and x_raw is at x_raw=500, max=1000 → 0.5).
    #[test]
    fn enabled_identity_coordinate_stage_normalizes_raw_xy() {
        // caps.x.max = 1000, caps.y.max = 1000.
        // make_sample sets x_raw=500, y_raw=500 → normalized = 0.5, 0.5.
        let raw = make_sample(0.5, 0.5, 0.5);
        let caps = dummy_caps(); // max=1000
        let mut state = ProcessorState::new();
        let mut profile = CalibrationProfile::identity();
        profile.coordinate_map.enabled = true;
        // identity affine [1,0,0, 0,1,0]: (500, 500) → (500, 500) → /1000 = 0.5
        profile.coordinate_map.affine = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

        let processed = profile.apply(&raw, &caps, &mut state);

        assert!((processed.x - 0.5).abs() < 1e-12, "x: {}", processed.x);
        assert!((processed.y - 0.5).abs() < 1e-12, "y: {}", processed.y);
    }

    /// Enabled coordinate stage with a scale-2 affine doubles the raw digitizer
    /// values before normalization.
    #[test]
    fn enabled_coordinate_stage_scale_x2() {
        // caps: max=1000 for both axes.
        // raw: x_raw=250, y_raw=250 → affine scale×2 → (500, 500) → /1000 = 0.5
        let mut raw = make_sample(0.25, 0.25, 0.5);
        raw.x_raw = 250;
        raw.y_raw = 250;
        let caps = dummy_caps();
        let mut state = ProcessorState::new();
        let mut profile = CalibrationProfile::identity();
        profile.coordinate_map.enabled = true;
        profile.coordinate_map.affine = [2.0, 0.0, 0.0, 0.0, 2.0, 0.0];

        let processed = profile.apply(&raw, &caps, &mut state);

        assert!((processed.x - 0.5).abs() < 1e-12, "x: {}", processed.x);
        assert!((processed.y - 0.5).abs() < 1e-12, "y: {}", processed.y);
    }

    /// TargetSpace::ScreenPixels: a normalized (0.5, 0.5) with a 1920×1080
    /// canvas should produce (960.0, 540.0).
    #[test]
    fn coordinate_stage_screen_pixels_target_space() {
        // caps: max=1000 for both axes.
        // raw: x_raw=500, y_raw=500 → identity affine → (500, 500) → /1000 = 0.5
        // ScreenPixels{1920, 1080}: 0.5×1920=960, 0.5×1080=540
        let raw = make_sample(0.5, 0.5, 0.5);
        let caps = dummy_caps();
        let mut state = ProcessorState::new();
        let mut profile = CalibrationProfile::identity();
        profile.target_space = TargetSpace::ScreenPixels { w: 1920, h: 1080 };
        profile.coordinate_map.enabled = true;
        profile.coordinate_map.affine = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

        let processed = profile.apply(&raw, &caps, &mut state);

        assert!(
            (processed.x - 960.0).abs() < 1e-9,
            "x pixels: {}",
            processed.x
        );
        assert!(
            (processed.y - 540.0).abs() < 1e-9,
            "y pixels: {}",
            processed.y
        );
    }

    /// TargetSpace::Millimeters with caps.x.unit=Inch, resolution=100 units/inch:
    /// 100 raw → 1 inch → 25.4 mm.
    #[test]
    fn coordinate_stage_millimeters_target_space() {
        let mut caps = dummy_caps(); // min=0, max=1000
        caps.x.resolution = 100.0;
        caps.x.unit = AxisUnit::Inch;
        caps.y.resolution = 100.0;
        caps.y.unit = AxisUnit::Inch;

        let mut raw = make_sample(0.1, 0.0, 0.0);
        raw.x_raw = 100;
        raw.y_raw = 0;

        let mut state = ProcessorState::new();
        let mut profile = CalibrationProfile::identity();
        profile.target_space = TargetSpace::Millimeters;
        profile.coordinate_map.enabled = true;
        profile.coordinate_map.affine = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

        let processed = profile.apply(&raw, &caps, &mut state);

        // 100 raw / 100 units-per-inch × 25.4 mm/inch = 25.4 mm
        assert!((processed.x - 25.4).abs() < 1e-9, "x mm: {}", processed.x);
    }

    // ── TC1.3: pressure-stage wiring ─────────────────────────────────────────

    /// Disabled pressure stage uses raw normalized pressure (identity).
    #[test]
    fn disabled_pressure_stage_uses_raw_norm() {
        let raw = make_sample(0.5, 0.5, 0.7);
        let caps = dummy_caps();
        let mut state = ProcessorState::new();
        let mut profile = CalibrationProfile::identity();
        profile.pressure.enabled = false;

        let processed = profile.apply(&raw, &caps, &mut state);

        assert_eq!(
            processed.pressure, raw.pressure_norm,
            "disabled pressure stage must pass raw.pressure_norm through"
        );
    }

    /// Enabled pressure stage applies the curve to the raw pressure value.
    ///
    /// `make_sample` sets `pressure_raw = 512`.  With min=0, max=1000,
    /// Linear curve: output = 512/1000 = 0.512.
    #[test]
    fn enabled_pressure_stage_applies_curve() {
        // make_sample sets pressure_raw = 512 and pressure_norm = 0.5.
        let raw = make_sample(0.5, 0.5, 0.5);
        // Confirm the helper matches expectations.
        assert_eq!(raw.pressure_raw, 512);

        let caps = dummy_caps();
        let mut state = ProcessorState::new();
        let mut profile = CalibrationProfile::identity();
        profile.pressure.enabled = true;
        profile.pressure.pressure_min = 0;
        profile.pressure.pressure_max = 1000;
        profile.pressure.curve = CurveKind::Linear;

        let processed = profile.apply(&raw, &caps, &mut state);

        let expected = 512.0 / 1000.0; // = 0.512
        assert!(
            (processed.pressure - expected).abs() < 1e-12,
            "pressure: {}",
            processed.pressure
        );
    }

    /// Enabled pressure stage with Gamma(2.0): output = (512/1000)^2 = 0.262144.
    #[test]
    fn enabled_pressure_stage_gamma() {
        let raw = make_sample(0.5, 0.5, 0.5);
        let caps = dummy_caps();
        let mut state = ProcessorState::new();
        let mut profile = CalibrationProfile::identity();
        profile.pressure.enabled = true;
        profile.pressure.pressure_min = 0;
        profile.pressure.pressure_max = 1000;
        profile.pressure.curve = CurveKind::Gamma(2.0);

        let processed = profile.apply(&raw, &caps, &mut state);

        let t = 512.0_f64 / 1000.0;
        let expected = t.powf(2.0);
        assert!(
            (processed.pressure - expected).abs() < 1e-12,
            "gamma pressure: {}",
            processed.pressure
        );
    }

    /// Enabled pressure stage output is always in [0.0, 1.0].
    #[test]
    fn pressure_stage_output_in_range() {
        let caps = dummy_caps();
        let mut state = ProcessorState::new();
        let mut profile = CalibrationProfile::identity();
        profile.pressure.enabled = true;
        profile.pressure.pressure_min = 100;
        profile.pressure.pressure_max = 900;
        profile.pressure.curve = CurveKind::Gamma(2.0);

        for raw_p in [0u32, 50, 100, 500, 900, 1000] {
            let mut raw = make_sample(0.5, 0.5, 0.5);
            raw.pressure_raw = raw_p;
            let processed = profile.apply(&raw, &caps, &mut state);
            assert!(
                (0.0..=1.0).contains(&processed.pressure),
                "pressure out of range: {} (raw={raw_p})",
                processed.pressure
            );
        }
    }
}
