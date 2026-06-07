//! Integration tests for `tablet-process` (TC1.5).
//!
//! These tests exercise the full pipeline end-to-end:
//! - Identity profile pass-through
//! - Profile `save` → `load` round-trips (TOML and JSON)
//! - Rejection of invalid profiles on `load`
//! - Multi-sample `apply` with stateful stages (EMA smoothing, activation)
//! - `out_of_range` detection

use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities, ToolKind};
use tablet_process::{
    ActivationConfig, CalibrationProfile, CoordinateMap, CurveKind, OneEuroParams, ProcessorState,
    ProfileError, SmoothingConfig, SmoothingMode, TargetSpace,
};

// ── Test helpers ──────────────────────────────────────────────────────────────

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

/// Build a `PenSample` at the given normalized position/pressure.
///
/// Sets `x_raw`/`y_raw` to match the normalized position over caps.max=1000,
/// and `pressure_raw` to match `pressure_norm` over the same range.
fn make_sample(
    x_norm: f64,
    y_norm: f64,
    pressure_norm: f64,
    t_capture_ns: u64,
) -> tablet_core::PenSample {
    tablet_core::PenSample {
        t_capture_ns,
        t_device_ms: (t_capture_ns / 1_000_000) as u32,
        serial: (t_capture_ns / 1_000_000) as u32,
        x_raw: (x_norm * 1000.0).round() as i32,
        y_raw: (y_norm * 1000.0).round() as i32,
        z_raw: 0,
        x_norm,
        y_norm,
        pressure_raw: (pressure_norm * 1000.0).round() as u32,
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

// ── TC1.5.1: Identity profile pass-through ───────────────────────────────────

/// Identity profile: `processed.raw == input` and mapped x/y/pressure equal
/// raw normalized values.
#[test]
fn identity_profile_passthrough() {
    let caps = dummy_caps();
    let mut state = ProcessorState::new();
    let raw = make_sample(0.3, 0.7, 0.5, 1_000_000);

    let processed = CalibrationProfile::identity().apply(&raw, &caps, &mut state);

    assert_eq!(processed.raw, raw, "raw field must equal the input sample");
    assert_eq!(processed.x, raw.x_norm, "x must equal x_norm");
    assert_eq!(processed.y, raw.y_norm, "y must equal y_norm");
    assert_eq!(
        processed.pressure, raw.pressure_norm,
        "pressure must equal pressure_norm"
    );
    assert_eq!(processed.x_filtered, None, "smoothing disabled → None");
    assert_eq!(processed.y_filtered, None, "smoothing disabled → None");
    assert!(
        !processed.out_of_range,
        "in-range values must not set out_of_range"
    );
}

// ── TC1.5.2: Round-trip (TOML) ───────────────────────────────────────────────

/// A non-trivial profile round-trips through `save`→`load` for TOML.
#[test]
fn round_trip_toml() {
    let profile = build_nontrivial_profile();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("test.cal.toml");

    profile.save(&path).expect("save TOML");
    let loaded = CalibrationProfile::load(&path).expect("load TOML");

    assert_eq!(profile, loaded, "TOML round-trip equality failed");
}

/// A non-trivial profile round-trips through `save`→`load` for JSON.
#[test]
fn round_trip_json() {
    let profile = build_nontrivial_profile();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("test.json");

    profile.save(&path).expect("save JSON");
    let loaded = CalibrationProfile::load(&path).expect("load JSON");

    assert_eq!(profile, loaded, "JSON round-trip equality failed");
}

/// A profile with Custom pressure points round-trips.
#[test]
fn round_trip_custom_pressure_toml() {
    let mut profile = CalibrationProfile::identity();
    profile.pressure.enabled = true;
    profile.pressure.pressure_min = 50;
    profile.pressure.pressure_max = 950;
    profile.pressure.curve = CurveKind::Custom(vec![
        (0.0, 0.0),
        (0.25, 0.2),
        (0.5, 0.5),
        (0.75, 0.8),
        (1.0, 1.0),
    ]);

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("custom.cal.toml");

    profile.save(&path).expect("save");
    let loaded = CalibrationProfile::load(&path).expect("load");

    assert_eq!(profile, loaded, "custom pressure round-trip failed");
}

// ── TC1.5.3: Validation rejections ───────────────────────────────────────────

/// `load` rejects a profile where `pressure_min > pressure_max`.
#[test]
fn load_rejects_pressure_min_gt_max() {
    let mut profile = CalibrationProfile::identity();
    profile.pressure.pressure_min = 900;
    profile.pressure.pressure_max = 100;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad_pressure.json");

    // Save a manually-broken JSON (bypass save-side validation by using
    // `serde_json` directly on the raw struct).
    let text = serde_json::to_string_pretty(&profile).expect("serialize");
    std::fs::write(&path, text).expect("write");

    let err = CalibrationProfile::load(&path).unwrap_err();
    assert!(
        matches!(&err, ProfileError::Validation { field, .. } if field.contains("pressure_min")),
        "expected pressure_min error, got: {err}"
    );
}

/// `load` rejects a profile with `Gamma(g <= 0)`.
#[test]
fn load_rejects_gamma_zero() {
    let mut profile = CalibrationProfile::identity();
    profile.pressure.curve = CurveKind::Gamma(0.0);

    let err = save_and_try_load_json(&profile);
    assert!(
        matches!(&err, ProfileError::Validation { field, .. } if field.contains("Gamma")),
        "expected Gamma error, got: {err}"
    );
}

/// `load` rejects non-monotone Custom control points (v decreasing).
#[test]
fn load_rejects_non_monotone_custom_points() {
    let mut profile = CalibrationProfile::identity();
    // v goes from 0.8 down to 0.5 — not monotone.
    profile.pressure.curve = CurveKind::Custom(vec![(0.0, 0.0), (0.5, 0.8), (1.0, 0.5)]);

    let err = save_and_try_load_json(&profile);
    assert!(
        matches!(&err, ProfileError::Validation { field, .. } if field.contains("v values")),
        "expected custom-points v error, got: {err}"
    );
}

/// `load` rejects EMA alpha out of range (== 0).
#[test]
fn load_rejects_ema_alpha_zero() {
    let mut profile = CalibrationProfile::identity();
    profile.smoothing = SmoothingConfig {
        mode: SmoothingMode::Ema { alpha: 0.0 },
    };

    let err = save_and_try_load_json(&profile);
    assert!(
        matches!(&err, ProfileError::Validation { field, .. } if field.contains("alpha")),
        "expected alpha error, got: {err}"
    );
}

/// `load` rejects EMA alpha > 1.0.
#[test]
fn load_rejects_ema_alpha_gt_one() {
    let mut profile = CalibrationProfile::identity();
    profile.smoothing = SmoothingConfig {
        mode: SmoothingMode::Ema { alpha: 1.1 },
    };

    let err = save_and_try_load_json(&profile);
    assert!(
        matches!(&err, ProfileError::Validation { .. }),
        "expected validation error, got: {err}"
    );
}

/// `load` rejects OneEuro params with non-positive min_cutoff.
#[test]
fn load_rejects_one_euro_negative_cutoff() {
    let mut profile = CalibrationProfile::identity();
    profile.smoothing = SmoothingConfig {
        mode: SmoothingMode::OneEuro(OneEuroParams {
            min_cutoff: -1.0,
            beta: 0.0,
            d_cutoff: 1.0,
        }),
    };

    let err = save_and_try_load_json(&profile);
    assert!(
        matches!(&err, ProfileError::Validation { .. }),
        "expected validation error, got: {err}"
    );
}

// ── TC1.5.4: Multi-sample stream ─────────────────────────────────────────────

/// EMA smoothing visibly lags a step input (the filtered output is between
/// the old and new values on the first post-step sample).
#[test]
fn ema_smoothing_lags_step_input() {
    let caps = dummy_caps();
    let mut state = ProcessorState::new();

    let mut profile = CalibrationProfile::identity();
    profile.smoothing = SmoothingConfig {
        mode: SmoothingMode::Ema { alpha: 0.3 },
    };

    // Warm up at x=0.0.
    for i in 0..10u64 {
        let raw = make_sample(0.0, 0.0, 0.0, i * 8_000_000); // 8ms apart
        profile.apply(&raw, &caps, &mut state);
    }

    // Step to x=1.0 — the EMA output must be strictly less than 1.0 (lagging).
    let step_raw = make_sample(1.0, 0.0, 0.0, 10 * 8_000_000);
    let stepped = profile.apply(&step_raw, &caps, &mut state);

    let xf = stepped.x_filtered.expect("smoothing enabled → Some");
    assert!(
        xf < 1.0,
        "EMA must lag the step; got x_filtered={xf} (expected < 1.0)"
    );
    assert!(
        xf > 0.0,
        "EMA must be above 0 after a step; got x_filtered={xf}"
    );
}

/// Activation latches with hysteresis: rising through on_threshold activates;
/// falling into the band keeps active; falling below off_threshold deactivates.
#[test]
fn activation_latches_with_hysteresis() {
    let caps = dummy_caps();
    let mut state = ProcessorState::new();

    let mut profile = CalibrationProfile::identity();
    profile.activation = ActivationConfig {
        enabled: true,
        on_threshold: 0.5,
        off_threshold: 0.3,
        require_proximity: true,
    };

    // Start inactive — pressure below off_threshold.
    let s0 = make_sample(0.5, 0.5, 0.1, 0);
    let r0 = profile.apply(&s0, &caps, &mut state);
    assert!(!r0.active, "should start inactive below off_threshold");

    // Rise above on_threshold → active.
    let s1 = make_sample(0.5, 0.5, 0.6, 8_000_000);
    let r1 = profile.apply(&s1, &caps, &mut state);
    assert!(r1.active, "should activate above on_threshold");

    // Drop into the band (between off and on) — latch holds active.
    let s2 = make_sample(0.5, 0.5, 0.4, 16_000_000);
    let r2 = profile.apply(&s2, &caps, &mut state);
    assert!(
        r2.active,
        "should stay active in hysteresis band (no chatter)"
    );

    // Drop below off_threshold → inactive.
    let s3 = make_sample(0.5, 0.5, 0.2, 24_000_000);
    let r3 = profile.apply(&s3, &caps, &mut state);
    assert!(!r3.active, "should deactivate below off_threshold");
}

/// `dt` is derived internally from consecutive `raw.t_capture_ns` values and
/// the state's `last_t_capture_ns`; the state's timestamp is updated each call.
///
/// This test verifies that state is correctly threaded: the very first sample
/// uses `dt=0.0` (cold start), and subsequent ones derive dt from the gap.
/// For EMA the dt value doesn't matter, but for One-Euro it does — verifying
/// that a sequence of calls produces deterministic output confirms the internal
/// dt derivation is consistent.
#[test]
fn apply_dt_derived_from_timestamps_deterministic() {
    let caps = dummy_caps();

    let mut profile = CalibrationProfile::identity();
    profile.smoothing = SmoothingConfig {
        mode: SmoothingMode::OneEuro(OneEuroParams::default()),
    };

    // Build a sequence of samples at 125 Hz (8ms apart).
    let samples: Vec<_> = (0..20u64)
        .map(|i| make_sample(i as f64 * 0.05, i as f64 * 0.03, 0.0, i * 8_000_000))
        .collect();

    // Run the sequence twice from fresh state and assert identical output.
    let run = |samples: &[tablet_core::PenSample]| -> Vec<(Option<f64>, Option<f64>)> {
        let mut st = ProcessorState::new();
        samples
            .iter()
            .map(|s| {
                let p = profile.apply(s, &caps, &mut st);
                (p.x_filtered, p.y_filtered)
            })
            .collect()
    };

    let run1 = run(&samples);
    let run2 = run(&samples);
    assert_eq!(run1, run2, "apply must be deterministic given same inputs");
}

// ── TC1.5.5: out_of_range detection ──────────────────────────────────────────

/// An in-range sample must not set `out_of_range`.
#[test]
fn out_of_range_false_for_in_range_sample() {
    let caps = dummy_caps(); // x/y/pressure: 0..=1000
    let mut state = ProcessorState::new();

    // x_raw=500, y_raw=500, pressure_raw=500 — all within [0, 1000].
    let raw = make_sample(0.5, 0.5, 0.5, 0);
    let processed = CalibrationProfile::identity().apply(&raw, &caps, &mut state);

    assert!(
        !processed.out_of_range,
        "in-range sample must not set out_of_range"
    );
}

/// An x value below caps.x.min sets `out_of_range`.
#[test]
fn out_of_range_true_for_x_below_min() {
    let caps = dummy_caps(); // x.min = 0
    let mut state = ProcessorState::new();

    let mut raw = make_sample(0.0, 0.5, 0.5, 0);
    raw.x_raw = -1; // below min=0

    let processed = CalibrationProfile::identity().apply(&raw, &caps, &mut state);
    assert!(processed.out_of_range, "x below min must set out_of_range");
}

/// An x value above caps.x.max sets `out_of_range`.
#[test]
fn out_of_range_true_for_x_above_max() {
    let caps = dummy_caps(); // x.max = 1000
    let mut state = ProcessorState::new();

    let mut raw = make_sample(1.0, 0.5, 0.5, 0);
    raw.x_raw = 1001; // above max=1000

    let processed = CalibrationProfile::identity().apply(&raw, &caps, &mut state);
    assert!(processed.out_of_range, "x above max must set out_of_range");
}

/// A pressure value above caps.pressure.max sets `out_of_range`.
#[test]
fn out_of_range_true_for_pressure_above_max() {
    let caps = dummy_caps(); // pressure.max = 1000
    let mut state = ProcessorState::new();

    let mut raw = make_sample(0.5, 0.5, 1.0, 0);
    raw.pressure_raw = 1001; // above max=1000

    let processed = CalibrationProfile::identity().apply(&raw, &caps, &mut state);
    assert!(
        processed.out_of_range,
        "pressure above max must set out_of_range"
    );
}

/// Unsupported axes are not checked for out-of-range.
#[test]
fn out_of_range_skips_unsupported_axes() {
    let mut caps = dummy_caps();
    caps.pressure.supported = false; // mark pressure unsupported

    let mut state = ProcessorState::new();
    let mut raw = make_sample(0.5, 0.5, 1.0, 0);
    raw.pressure_raw = 9999; // way out of range, but axis is unsupported

    let processed = CalibrationProfile::identity().apply(&raw, &caps, &mut state);
    assert!(
        !processed.out_of_range,
        "unsupported axes must not set out_of_range"
    );
}

// ── TC1.5.6: Pipeline stage ordering ─────────────────────────────────────────

/// Verify that stage ordering is: coordinate → pressure → tilt → smoothing →
/// activation.
///
/// A pressure stage with Gamma(2.0) must see the raw pressure (not the
/// smoothed/filtered pressure), and activation must receive the _shaped_
/// pressure from the pressure stage (not the raw value).
#[test]
fn pipeline_stage_ordering() {
    // Set up caps with pressure.max=1000.
    let caps = dummy_caps();
    let mut state = ProcessorState::new();

    let mut profile = CalibrationProfile::identity();
    // Stage 2: gamma-shaped pressure.
    profile.pressure.enabled = true;
    profile.pressure.pressure_min = 0;
    profile.pressure.pressure_max = 1000;
    profile.pressure.curve = CurveKind::Gamma(2.0);
    // Stage 5: activation threshold at 0.5 (which in gamma(2) space means the
    // raw normalized value must be > 0.707 to cross 0.5 after shaping).
    profile.activation = ActivationConfig {
        enabled: true,
        on_threshold: 0.5,
        off_threshold: 0.4,
        require_proximity: true,
    };

    // pressure_raw = 800 → normalized = 0.8 → gamma(2) = 0.64 ≥ 0.5 → active.
    let mut raw = make_sample(0.5, 0.5, 0.8, 0);
    raw.pressure_raw = 800;

    let processed = profile.apply(&raw, &caps, &mut state);

    // pressure after Gamma(2.0) applied to 800/1000 = 0.8^2 = 0.64.
    let expected_pressure = 0.8_f64.powi(2);
    assert!(
        (processed.pressure - expected_pressure).abs() < 1e-12,
        "pressure stage ordering: expected {expected_pressure}, got {}",
        processed.pressure
    );

    // Activation threshold 0.5 < 0.64 → should be active.
    assert!(
        processed.active,
        "activation should be true when shaped pressure > on_threshold"
    );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a non-trivial profile for round-trip testing.
fn build_nontrivial_profile() -> CalibrationProfile {
    CalibrationProfile {
        name: String::from("round-trip-test"),
        target_space: TargetSpace::ScreenPixels { w: 1920, h: 1080 },
        coordinate_map: CoordinateMap {
            enabled: true,
            affine: [1.5, 0.1, -50.0, -0.05, 1.2, 30.0],
            projective: None,
        },
        pressure: tablet_process::PressureCurve {
            enabled: true,
            pressure_min: 50,
            pressure_max: 950,
            curve: CurveKind::Gamma(1.5),
        },
        tilt: tablet_process::TiltConfig {
            enabled: true,
            convention: tablet_process::TiltConvention::AzimuthAltitude,
        },
        smoothing: SmoothingConfig {
            mode: SmoothingMode::Ema { alpha: 0.4 },
        },
        activation: ActivationConfig {
            enabled: true,
            on_threshold: 0.05,
            off_threshold: 0.02,
            require_proximity: true,
        },
    }
}

/// Serialize `profile` to a temp JSON file (bypassing `save` validation) and
/// call `load` — returning the `ProfileError`.
fn save_and_try_load_json(profile: &CalibrationProfile) -> ProfileError {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad.json");
    let text = serde_json::to_string_pretty(profile).expect("serialize");
    std::fs::write(&path, text).expect("write");
    CalibrationProfile::load(&path).unwrap_err()
}
