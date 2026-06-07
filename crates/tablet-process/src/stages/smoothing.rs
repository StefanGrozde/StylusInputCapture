//! Stage 4 — Position smoothing / filtering.
//!
//! Implements EMA (Exponential Moving Average) and One-Euro adaptive low-pass
//! filters for position, producing `x_filtered`/`y_filtered` on the
//! [`ProcessedSample`](crate::ProcessedSample).
//!
//! Filter running state lives in [`ProcessorState`](crate::ProcessorState),
//! not in the profile, so [`SmoothingConfig`](crate::SmoothingConfig) stays
//! serializable plain data.
//!
//! # Filter modes
//!
//! - [`SmoothingMode::Off`] — no filtering; outputs are `None`.
//! - [`SmoothingMode::Ema`] — simple EMA: `out = α·in + (1−α)·prev`.
//! - [`SmoothingMode::OneEuro`] — adaptive 1€ filter that reduces jitter at
//!   low speeds while remaining responsive during fast strokes.
//!
//! # Determinism
//!
//! Both filters are deterministic: given the same sequence of inputs and the
//! same starting [`ProcessorState`], they produce identical outputs on every
//! run.

use crate::profile::{OneEuroParams, SmoothingConfig, SmoothingMode};
use crate::state::ProcessorState;

// ── One-Euro filter per-axis state ───────────────────────────────────────────

/// Running state for one axis of the One-Euro filter.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OneEuroAxisState {
    /// Previous filtered value (the "signal" estimate).
    pub prev_filtered: f64,
    /// Previous derivative estimate (filtered speed).
    pub prev_dx: f64,
}

// ── EMA per-axis state ────────────────────────────────────────────────────────

// EMA state is just an `Option<f64>` (previous output); the fields
// `ProcessorState::ema_x` / `ema_y` hold that directly.

// ── SmoothingConfig::apply ───────────────────────────────────────────────────

impl SmoothingConfig {
    /// Apply the configured filter to the post-coordinate `(x, y)` position.
    ///
    /// # Parameters
    ///
    /// - `x`, `y` — the mapped position (output of the coordinate stage).
    /// - `dt_seconds` — elapsed time since the previous sample in seconds
    ///   (used by the One-Euro filter).  For EMA this value is ignored.
    ///   Pass `0.0` when not available; One-Euro will treat the sample as a
    ///   "first sample" and warm up without filtering.
    /// - `state` — mutable per-stream state that holds previous filter values.
    ///
    /// # Returns
    ///
    /// `(x_filtered, y_filtered)` — `(None, None)` when the mode is
    /// [`SmoothingMode::Off`].
    pub fn apply(
        &self,
        x: f64,
        y: f64,
        dt_seconds: f64,
        state: &mut ProcessorState,
    ) -> (Option<f64>, Option<f64>) {
        match self.mode {
            SmoothingMode::Off => (None, None),

            SmoothingMode::Ema { alpha } => {
                let alpha = alpha.clamp(f64::EPSILON, 1.0);
                let xf = ema_step(x, state.ema_x, alpha);
                let yf = ema_step(y, state.ema_y, alpha);
                state.ema_x = Some(xf);
                state.ema_y = Some(yf);
                (Some(xf), Some(yf))
            }

            SmoothingMode::OneEuro(params) => {
                let xf = one_euro_step(x, dt_seconds, &mut state.one_euro_x, &params);
                let yf = one_euro_step(y, dt_seconds, &mut state.one_euro_y, &params);
                (Some(xf), Some(yf))
            }
        }
    }
}

// ── EMA helpers ──────────────────────────────────────────────────────────────

/// Single EMA step.
///
/// On the very first sample (`prev` is `None`) the filter returns the input
/// unchanged (cold start / no prior history).
#[inline]
fn ema_step(input: f64, prev: Option<f64>, alpha: f64) -> f64 {
    match prev {
        None => input,
        Some(p) => alpha * input + (1.0 - alpha) * p,
    }
}

// ── One-Euro helpers ──────────────────────────────────────────────────────────

/// Single One-Euro filter step for one axis.
///
/// On the very first sample (`state` is `None`) the output equals the input
/// and the derivative estimate is zeroed (cold start).
fn one_euro_step(
    input: f64,
    dt: f64,
    state: &mut Option<OneEuroAxisState>,
    params: &OneEuroParams,
) -> f64 {
    match state {
        None => {
            // Cold start: seed with the first value, no smoothing applied.
            *state = Some(OneEuroAxisState {
                prev_filtered: input,
                prev_dx: 0.0,
            });
            input
        }
        Some(s) => {
            // Guard dt to avoid division by zero or negative time.
            let dt = dt.max(f64::EPSILON);

            // ── Estimate the derivative (speed) ──────────────────────────────
            let dx_raw = (input - s.prev_filtered) / dt;

            // Low-pass filter the derivative with d_cutoff.
            let alpha_d = alpha_for_cutoff(params.d_cutoff, dt);
            let dx = ema_step(dx_raw, Some(s.prev_dx), alpha_d);

            // ── Adaptive cutoff ───────────────────────────────────────────────
            let cutoff = params.min_cutoff + params.beta * dx.abs();
            let alpha_s = alpha_for_cutoff(cutoff, dt);

            // ── Filter the signal ─────────────────────────────────────────────
            let filtered = ema_step(input, Some(s.prev_filtered), alpha_s);

            // ── Update state ──────────────────────────────────────────────────
            s.prev_filtered = filtered;
            s.prev_dx = dx;

            filtered
        }
    }
}

/// Compute the EMA alpha for a given low-pass cutoff frequency (Hz) and sample
/// period (seconds).
///
/// The standard RC low-pass ↔ EMA correspondence:
/// `τ = 1 / (2π·fc)`,  `alpha = 1 − e^(−dt/τ)` ≈ `dt / (τ + dt)`.
///
/// We use the exact exponential form for correctness at non-uniform sample rates.
#[inline]
fn alpha_for_cutoff(cutoff_hz: f64, dt: f64) -> f64 {
    // τ = 1 / (2π·fc)
    let tau = 1.0 / (2.0 * std::f64::consts::PI * cutoff_hz.max(f64::EPSILON));
    // alpha = 1 − e^(−dt/τ)   [exact EMA–RC equivalence]
    (1.0 - (-dt / tau).exp()).clamp(f64::EPSILON, 1.0)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::profile::{OneEuroParams, SmoothingConfig, SmoothingMode};
    use crate::state::ProcessorState;

    // ── Off mode ─────────────────────────────────────────────────────────────

    /// Off mode always returns (None, None).
    #[test]
    fn off_mode_returns_none() {
        let cfg = SmoothingConfig {
            mode: SmoothingMode::Off,
        };
        let mut state = ProcessorState::new();
        let (xf, yf) = cfg.apply(0.5, 0.7, 0.016, &mut state);
        assert_eq!(xf, None);
        assert_eq!(yf, None);
        // State should be unchanged.
        assert_eq!(state.ema_x, None);
        assert_eq!(state.ema_y, None);
    }

    // ── EMA determinism ──────────────────────────────────────────────────────

    /// EMA sequence is deterministic: given the same inputs and fresh state,
    /// the outputs are identical on every run.
    #[test]
    fn ema_deterministic() {
        let cfg = SmoothingConfig {
            mode: SmoothingMode::Ema { alpha: 0.5 },
        };
        let inputs = [(0.0f64, 0.0f64), (1.0, 0.0), (1.0, 1.0), (0.5, 0.5)];

        let run = |inputs: &[(f64, f64)]| -> Vec<(Option<f64>, Option<f64>)> {
            let mut state = ProcessorState::new();
            inputs
                .iter()
                .map(|&(x, y)| cfg.apply(x, y, 0.016, &mut state))
                .collect()
        };

        assert_eq!(run(&inputs), run(&inputs), "EMA is not deterministic");
    }

    /// EMA matches a hand-computed sequence with alpha = 0.5.
    ///
    /// Initial state: ema_x = None, ema_y = None.
    ///
    /// Sample 0: x=0.0  → cold start → ema=0.0
    /// Sample 1: x=1.0  → 0.5*1 + 0.5*0 = 0.5
    /// Sample 2: x=1.0  → 0.5*1 + 0.5*0.5 = 0.75
    /// Sample 3: x=0.0  → 0.5*0 + 0.5*0.75 = 0.375
    #[test]
    fn ema_hand_computed_sequence() {
        let cfg = SmoothingConfig {
            mode: SmoothingMode::Ema { alpha: 0.5 },
        };
        let mut state = ProcessorState::new();
        let dt = 0.016; // arbitrary; EMA ignores dt

        let (x0, _) = cfg.apply(0.0, 0.0, dt, &mut state);
        assert_eq!(x0, Some(0.0), "sample 0 (cold start)");

        let (x1, _) = cfg.apply(1.0, 0.0, dt, &mut state);
        assert!((x1.unwrap() - 0.5).abs() < 1e-12, "sample 1: {x1:?}");

        let (x2, _) = cfg.apply(1.0, 0.0, dt, &mut state);
        assert!((x2.unwrap() - 0.75).abs() < 1e-12, "sample 2: {x2:?}");

        let (x3, _) = cfg.apply(0.0, 0.0, dt, &mut state);
        assert!((x3.unwrap() - 0.375).abs() < 1e-12, "sample 3: {x3:?}");
    }

    /// EMA with alpha = 1.0 is a pass-through (no smoothing).
    #[test]
    fn ema_alpha_one_passthrough() {
        let cfg = SmoothingConfig {
            mode: SmoothingMode::Ema { alpha: 1.0 },
        };
        let mut state = ProcessorState::new();
        // After cold start the output equals the input every time.
        cfg.apply(0.2, 0.8, 0.016, &mut state);
        let (xf, yf) = cfg.apply(0.6, 0.3, 0.016, &mut state);
        assert!((xf.unwrap() - 0.6).abs() < 1e-12);
        assert!((yf.unwrap() - 0.3).abs() < 1e-12);
    }

    /// EMA cold start: first sample is echoed unchanged.
    #[test]
    fn ema_cold_start_echoes_input() {
        let cfg = SmoothingConfig {
            mode: SmoothingMode::Ema { alpha: 0.3 },
        };
        let mut state = ProcessorState::new();
        let (xf, yf) = cfg.apply(0.7, 0.2, 0.016, &mut state);
        assert_eq!(xf, Some(0.7), "cold start x");
        assert_eq!(yf, Some(0.2), "cold start y");
    }

    // ── One-Euro determinism ─────────────────────────────────────────────────

    /// One-Euro filter is deterministic for a given input sequence and seed state.
    #[test]
    fn one_euro_deterministic() {
        let params = OneEuroParams {
            min_cutoff: 1.0,
            beta: 0.007,
            d_cutoff: 1.0,
        };
        let cfg = SmoothingConfig {
            mode: SmoothingMode::OneEuro(params),
        };
        // Use uniform dt = 0.008 s (125 Hz) for determinism.
        let dt = 0.008_f64;
        let inputs: Vec<(f64, f64)> = (0..20)
            .map(|i| ((i as f64) * 0.05, (i as f64) * 0.03))
            .collect();

        let run = |inputs: &[(f64, f64)]| -> Vec<(Option<f64>, Option<f64>)> {
            let mut state = ProcessorState::new();
            inputs
                .iter()
                .map(|&(x, y)| cfg.apply(x, y, dt, &mut state))
                .collect()
        };

        assert_eq!(run(&inputs), run(&inputs), "One-Euro is not deterministic");
    }

    /// One-Euro cold start: first sample echoes the input.
    #[test]
    fn one_euro_cold_start_echoes_input() {
        let cfg = SmoothingConfig {
            mode: SmoothingMode::OneEuro(OneEuroParams::default()),
        };
        let mut state = ProcessorState::new();
        let (xf, yf) = cfg.apply(0.4, 0.9, 0.01, &mut state);
        assert_eq!(xf, Some(0.4), "cold start x");
        assert_eq!(yf, Some(0.9), "cold start y");
    }

    /// One-Euro with a constant input converges toward that constant.
    ///
    /// After many samples with the same input the filtered output should be
    /// very close to the input.
    #[test]
    fn one_euro_constant_input_converges() {
        let cfg = SmoothingConfig {
            mode: SmoothingMode::OneEuro(OneEuroParams {
                min_cutoff: 2.0,
                beta: 0.0,
                d_cutoff: 1.0,
            }),
        };
        let mut state = ProcessorState::new();
        let target = 0.7_f64;
        let dt = 0.016_f64;

        // Warm up with many identical samples.
        let mut last_x = None;
        for _ in 0..500 {
            let (xf, _) = cfg.apply(target, 0.0, dt, &mut state);
            last_x = xf;
        }

        let out = last_x.unwrap();
        assert!(
            (out - target).abs() < 1e-3,
            "One-Euro did not converge: {out} (target {target})"
        );
    }
}
