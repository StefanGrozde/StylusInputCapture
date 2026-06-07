//! [`ProcessorState`] — per-stream mutable state threaded through
//! [`CalibrationProfile::apply`].
//!
//! Keeping state here (rather than inside [`CalibrationProfile`]) ensures the
//! profile remains plain, serializable data while the caller owns the
//! stateful resources needed for filters and hysteresis latches.
//!
//! [`CalibrationProfile::apply`]: crate::CalibrationProfile::apply
//! [`CalibrationProfile`]: crate::CalibrationProfile

use crate::stages::smoothing::OneEuroAxisState;

/// Mutable per-stream processing state threaded through each call to
/// [`CalibrationProfile::apply`].
///
/// Holds all stateful resources that must persist across samples:
/// - EMA filter history for X and Y position.
/// - One-Euro filter history for X and Y position.
/// - Activation hysteresis latch.
///
/// All fields are `pub(crate)` — only the stage implementations (in
/// `crate::stages`) read and write them; the public API is the `new()` /
/// `Default` constructor.
///
/// [`CalibrationProfile::apply`]: crate::CalibrationProfile::apply
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessorState {
    // ── EMA filter state (Stage 4 — Smoothing) ───────────────────────────────
    /// Previous EMA output for the X position axis.  `None` on cold start.
    pub(crate) ema_x: Option<f64>,
    /// Previous EMA output for the Y position axis.  `None` on cold start.
    pub(crate) ema_y: Option<f64>,

    // ── One-Euro filter state (Stage 4 — Smoothing) ──────────────────────────
    /// One-Euro running state for the X position axis.  `None` on cold start.
    pub(crate) one_euro_x: Option<OneEuroAxisState>,
    /// One-Euro running state for the Y position axis.  `None` on cold start.
    pub(crate) one_euro_y: Option<OneEuroAxisState>,

    // ── Activation hysteresis latch (Stage 5 — Activation) ───────────────────
    /// Current activation state.  Starts `false` (inactive).
    ///
    /// The latch is held in `ProcessorState` so the profile can be switched
    /// without resetting the latch, and so the profile stays plain data.
    pub(crate) active_latch: bool,

    // ── Timestamp tracking (Stage 4 — used to compute dt for One-Euro) ───────
    /// Capture timestamp of the previous sample in nanoseconds.
    /// `None` before the first sample.
    pub(crate) last_t_capture_ns: Option<u64>,
}

impl ProcessorState {
    /// Create a fresh, zeroed-out state (equivalent to [`Default`]).
    ///
    /// EMA history is `None` (cold start), One-Euro history is `None`, and the
    /// activation latch starts as `false` (inactive).
    pub fn new() -> Self {
        Self::default()
    }
}
