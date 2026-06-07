//! [`CalibrationProfile`] and its pipeline stage sub-structs.
//!
//! A `CalibrationProfile` is a serializable, ordered set of processing stages
//! that transform a raw [`PenSample`] into a [`ProcessedSample`].  Each stage
//! carries an `enabled` flag; disabled stages are pass-throughs.  The profile
//! is pure data — all mutable per-stream state (filter history, activation
//! latches) lives in [`ProcessorState`](crate::ProcessorState).
//!
//! [`PenSample`]: tablet_core::PenSample

use serde::{Deserialize, Serialize};

// ── Target coordinate space ──────────────────────────────────────────────────

/// The coordinate space into which processed `x`/`y` values are mapped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TargetSpace {
    /// Normalized [0.0, 1.0] over the device's digitizer extent (default).
    Normalized,
    /// Integer screen pixels, with a known canvas size.
    ScreenPixels { w: u32, h: u32 },
    /// Physical millimeters, derived from the axis resolution reported by
    /// [`DeviceCapabilities`](tablet_core::DeviceCapabilities).
    Millimeters,
}

// ── Stage sub-structs ────────────────────────────────────────────────────────

/// Stage 1 — Coordinate mapping.
///
/// Applies an affine (and optional projective) matrix to map raw digitizer
/// coordinates into the profile's [`TargetSpace`].  The transform parameters
/// are placeholder data fields for now; real application logic is added in
/// TC1.2.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoordinateMap {
    /// When `false` the stage is a pass-through (raw normalized position used).
    pub enabled: bool,
    /// Affine 2×3 matrix stored in row-major order `[a,b,c, d,e,f]`.
    /// Identity: `[1, 0, 0, 0, 1, 0]`.
    pub affine: [f64; 6],
    /// Optional full 3×3 projective matrix (row-major).  `None` ⇒ affine only.
    pub projective: Option<[f64; 9]>,
}

impl CoordinateMap {
    /// Identity affine matrix — coordinates pass through unchanged.
    pub fn identity() -> Self {
        Self {
            enabled: false,
            affine: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            projective: None,
        }
    }
}

impl Default for CoordinateMap {
    fn default() -> Self {
        Self::identity()
    }
}

/// The shape of the pressure response curve applied after clamping and
/// normalizing the raw pressure value.
///
/// All variants map the normalized input `t ∈ [0.0, 1.0]` to an output in
/// `[0.0, 1.0]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CurveKind {
    /// Identity mapping: output = input.  Equivalent to `Gamma(1.0)`.
    Linear,
    /// Power-law curve: output = input^g.
    ///
    /// - `g > 1.0` — concave (lighter pen strokes register less pressure).
    /// - `g = 1.0` — linear (equivalent to [`CurveKind::Linear`]).
    /// - `0.0 < g < 1.0` — convex (lighter pen strokes register more
    ///   pressure).
    ///
    /// `g` must be `> 0.0`; values ≤ 0 are treated as `1.0` at runtime.
    Gamma(f64),
    /// Piecewise-linear curve defined by sorted, monotone control points.
    ///
    /// Each element is `(t, v)` where `t` is the normalized input in
    /// `[0.0, 1.0]` and `v` is the desired output in `[0.0, 1.0]`.  Points
    /// must be **sorted by `t`** and **monotone** (non-decreasing in `v`).
    /// If the slice is empty the stage falls back to linear.
    Custom(Vec<(f64, f64)>),
}

impl Default for CurveKind {
    fn default() -> Self {
        CurveKind::Linear
    }
}

/// Stage 2 — Pressure response curve.
///
/// Clamps raw pressure (in native device units, `u32`) to the observed
/// `[pressure_min, pressure_max]` window, normalizes the clamped value to
/// `[0.0, 1.0]`, and then applies a [`CurveKind`] to shape the response.
/// The output is always in `[0.0, 1.0]`.
///
/// When `enabled = false` the raw normalized pressure (`PenSample::pressure_norm`)
/// is passed through unchanged (identity behaviour).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PressureCurve {
    /// When `false` raw normalized pressure passes through unchanged.
    pub enabled: bool,
    /// Lower clamp boundary in raw device pressure units.
    ///
    /// Raw pressure values below this level are treated as `0.0` after
    /// normalization.  Use [`learn_min_max`](crate::learn_min_max) to
    /// populate this from observed samples.
    pub pressure_min: u32,
    /// Upper clamp boundary in raw device pressure units.
    ///
    /// Raw pressure values above this level are treated as `1.0` after
    /// normalization.  Must be `>= pressure_min`; if equal the output is
    /// `0.0` for all inputs.
    pub pressure_max: u32,
    /// The curve shape applied after clamping and normalizing.
    pub curve: CurveKind,
}

impl PressureCurve {
    /// Identity — raw normalized pressure is passed through unchanged.
    ///
    /// `enabled = false`, so the stage is a no-op in the pipeline.
    /// `pressure_min = 0`, `pressure_max = u32::MAX` (full raw range),
    /// `curve = Linear`.
    pub fn identity() -> Self {
        Self {
            enabled: false,
            pressure_min: 0,
            pressure_max: u32::MAX,
            curve: CurveKind::Linear,
        }
    }
}

impl Default for PressureCurve {
    fn default() -> Self {
        Self::identity()
    }
}

/// Which tilt representation to expose in a [`ProcessedSample`].
///
/// All three conventions write their output to
/// `ProcessedSample::tilt_x`/`tilt_y` (in degrees) so downstream consumers
/// only need to know which convention the profile was configured with.
///
/// [`ProcessedSample`]: crate::ProcessedSample
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum TiltConvention {
    /// Copy `tilt_x_deg` / `tilt_y_deg` straight from the [`PenSample`].
    ///
    /// These are the Cartesian tilt angles pre-computed by the capture layer
    /// using [`tilt_from_orientation`].  This is the default convention and
    /// requires no additional computation.
    ///
    /// [`PenSample`]: tablet_core::PenSample
    /// [`tilt_from_orientation`]: tablet_core::tilt_from_orientation
    TiltXY,

    /// Expose azimuth and altitude as-captured (converted to degrees).
    ///
    /// `tilt_x` ← azimuth in degrees (compass direction of the pen in the
    /// tablet plane).  `tilt_y` ← altitude in degrees (angle from the tablet
    /// surface to the pen shaft; 0 = flat, 90 = vertical).
    ///
    /// Useful when downstream features need the raw orientation angles rather
    /// than the Cartesian decomposition.
    AzimuthAltitude,

    /// Re-derive Cartesian tilt from `azimuth_deci_deg` + `altitude_deci_deg`
    /// via [`tilt_from_orientation`], ignoring the pre-computed
    /// `tilt_x_deg`/`tilt_y_deg` on the sample.
    ///
    /// Use this if the capture layer's pre-computation is known to differ from
    /// the formula in [`tilt_from_orientation`], or for explicit reproducibility.
    ///
    /// [`tilt_from_orientation`]: tablet_core::tilt_from_orientation
    DerivedFromOrientation,
}

/// Stage 3 — Tilt / orientation derivation.
///
/// Selects which tilt convention is exposed in [`ProcessedSample`] and derives
/// the appropriate values.  When the stage is disabled, `tilt_x`/`tilt_y` are
/// copied directly from the raw sample's `tilt_x_deg`/`tilt_y_deg` fields and
/// `twist` is converted from `twist_deci_deg` to degrees (identity behaviour).
///
/// [`ProcessedSample`]: crate::ProcessedSample
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TiltConfig {
    /// When `false` tilt values are copied directly from the raw sample
    /// (`tilt_x_deg` / `tilt_y_deg`), matching the TC1.1 pass-through.
    pub enabled: bool,
    /// Which tilt representation to expose (see [`TiltConvention`]).
    pub convention: TiltConvention,
}

impl TiltConfig {
    /// Identity — tilt values passed through from the raw sample unchanged.
    ///
    /// `enabled = false`; convention defaults to [`TiltConvention::TiltXY`]
    /// (the same data already present on the raw sample).
    pub fn identity() -> Self {
        Self {
            enabled: false,
            convention: TiltConvention::TiltXY,
        }
    }
}

impl Default for TiltConfig {
    fn default() -> Self {
        Self::identity()
    }
}

/// Parameters for the One-Euro position filter.
///
/// The One-Euro filter is an adaptive low-pass filter that reduces jitter at
/// low speeds while preserving responsiveness at high speeds.
///
/// # References
/// Casiez, G., Roussel, N., Vogel, D. (2012). "1€ Filter: A Simple Speed-based
/// Low-pass Filter for Noisy Input in Interactive Systems." CHI 2012.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OneEuroParams {
    /// Minimum cutoff frequency in Hz (> 0).  Controls smoothing at rest.
    /// Smaller values → more smoothing at low speeds, but higher lag.
    /// Typical range: 0.5 – 5.0 Hz.
    pub min_cutoff: f64,
    /// Speed coefficient β (≥ 0).  Controls how fast the cutoff increases with
    /// speed.  Larger β → the filter adapts faster to rapid movement (less lag
    /// during fast strokes) but reduces smoothing.
    /// Typical range: 0.0 – 1.0.
    pub beta: f64,
    /// Cutoff frequency for the derivative (speed) estimate, Hz (> 0).
    /// Controls how smoothly the filter reacts to speed changes.
    /// Typical range: 0.5 – 5.0 Hz.
    pub d_cutoff: f64,
}

impl Default for OneEuroParams {
    fn default() -> Self {
        Self {
            min_cutoff: 1.0,
            beta: 0.007,
            d_cutoff: 1.0,
        }
    }
}

/// The smoothing algorithm used by [`SmoothingConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SmoothingMode {
    /// No smoothing — `x_filtered`/`y_filtered` are `None`.
    Off,
    /// Exponential Moving Average.
    ///
    /// Each output is: `output = α × input + (1 − α) × prev_output`.
    ///
    /// - `alpha = 1.0` — no smoothing (output == input).
    /// - `alpha → 0.0` — maximum smoothing (very slow response).
    ///
    /// `alpha` must be in `(0.0, 1.0]`; values ≤ 0 are clamped to `f64::EPSILON`
    /// at runtime.
    Ema {
        /// Smoothing factor α ∈ (0.0, 1.0].
        alpha: f64,
    },
    /// One-Euro adaptive low-pass filter (position only).
    ///
    /// Reduces jitter at low speeds while preserving responsiveness at high
    /// speeds.  The filter requires the time delta between samples (in seconds)
    /// and is therefore not quite "pure" — callers must supply
    /// `dt_seconds` via [`ProcessorState`](crate::ProcessorState) when the
    /// filter is active.
    OneEuro(OneEuroParams),
}

/// Stage 4 — Position smoothing / filtering.
///
/// Optional EMA or One-Euro filter for position that produces
/// `x_filtered`/`y_filtered`.  Filter state lives in
/// [`ProcessorState`](crate::ProcessorState) so the profile stays plain data.
///
/// When `mode` is [`SmoothingMode::Off`], `x_filtered`/`y_filtered` are
/// `None`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SmoothingConfig {
    /// Smoothing algorithm and parameters.  [`SmoothingMode::Off`] disables
    /// the stage and produces no filtered outputs.
    pub mode: SmoothingMode,
}

impl SmoothingConfig {
    /// Identity — smoothing disabled; `x_filtered`/`y_filtered` remain `None`.
    pub fn identity() -> Self {
        Self {
            mode: SmoothingMode::Off,
        }
    }
}

impl Default for SmoothingConfig {
    fn default() -> Self {
        Self::identity()
    }
}

/// Stage 5 — Activation / hover threshold.
///
/// Derives [`ProcessedSample::active`](crate::ProcessedSample::active) from
/// pressure thresholds with hysteresis and/or proximity gating.
/// Latch state lives in [`ProcessorState`](crate::ProcessorState).
/// Full hysteresis logic is added in TC1.4.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActivationConfig {
    /// When `false` `active` is derived solely from `in_proximity`.
    pub enabled: bool,
    /// Normalized pressure level at which `active` switches on (rising edge).
    pub on_threshold: f64,
    /// Normalized pressure level at which `active` switches off (falling edge,
    /// must be ≤ `on_threshold` for hysteresis).
    pub off_threshold: f64,
    /// If `true`, `active` requires `in_proximity` in addition to pressure.
    pub require_proximity: bool,
}

impl ActivationConfig {
    /// Identity — activation pass-through; `active = in_proximity`.
    pub fn identity() -> Self {
        Self {
            enabled: false,
            on_threshold: 0.0,
            off_threshold: 0.0,
            require_proximity: true,
        }
    }
}

impl Default for ActivationConfig {
    fn default() -> Self {
        Self::identity()
    }
}

// ── CalibrationProfile ───────────────────────────────────────────────────────

/// A complete, serializable processing pipeline for pen input.
///
/// Each field is a stage sub-struct that carries an `enabled` flag and its
/// parameters.  Disabled stages are pass-throughs.  The profile is plain data;
/// per-stream filter state lives in [`ProcessorState`](crate::ProcessorState).
///
/// Serialize to `*.cal.toml` (default) or JSON via `serde`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationProfile {
    /// Human-readable profile name.
    pub name: String,
    /// The coordinate space [`apply`](crate::CalibrationProfile::apply) maps
    /// position values into.
    pub target_space: TargetSpace,
    /// Stage 1 — coordinate mapping (affine / projective transform).
    pub coordinate_map: CoordinateMap,
    /// Stage 2 — pressure response curve.
    pub pressure: PressureCurve,
    /// Stage 3 — tilt / orientation derivation.
    pub tilt: TiltConfig,
    /// Stage 4 — position smoothing.
    pub smoothing: SmoothingConfig,
    /// Stage 5 — activation / hover threshold.
    pub activation: ActivationConfig,
}

impl CalibrationProfile {
    /// Construct an identity (pass-through) profile.
    ///
    /// All stages are disabled; `apply` returns a [`ProcessedSample`] whose
    /// mapped values equal the raw normalized values of the input
    /// [`PenSample`].
    ///
    /// [`ProcessedSample`]: crate::ProcessedSample
    /// [`PenSample`]: tablet_core::PenSample
    pub fn identity() -> Self {
        Self {
            name: String::from("identity"),
            target_space: TargetSpace::Normalized,
            coordinate_map: CoordinateMap::identity(),
            pressure: PressureCurve::identity(),
            tilt: TiltConfig::identity(),
            smoothing: SmoothingConfig::identity(),
            activation: ActivationConfig::identity(),
        }
    }
}

impl Default for CalibrationProfile {
    fn default() -> Self {
        Self::identity()
    }
}
