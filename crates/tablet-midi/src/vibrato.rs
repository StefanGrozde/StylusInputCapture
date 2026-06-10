//! Per-voice vibrato: gesture-driven depth from Y oscillations, clean sine LFO
//! output layered on manual pitch bend. Pure functions and state — no I/O.

use serde::{Deserialize, Serialize};

/// Serializable vibrato tuning (lives on [`MidiMapping`](crate::MidiMapping)).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VibratoConfig {
    /// When false the vibrato path is a no-op and manual bend is unchanged.
    pub enabled_by_default: bool,
    /// Target depth (semitones) at full gesture engagement.
    pub default_depth_semitones: f64,
    /// LFO rate in Hz (stable; gesture controls depth, not rate).
    pub default_rate_hz: f64,
    /// Time to ramp fade-in multiplier from 0 → 1 after gesture engages (ms).
    pub fade_in_ms: f64,
    /// Depth slew time constant (ms).
    pub depth_smoothing_ms: f64,
    /// Rate slew time constant (ms).
    pub rate_smoothing_ms: f64,
    /// How much pen pressure scales vibrato depth (0 = ignore, 1 = full).
    pub pressure_to_amount: f64,
    /// Sensitivity of Y-oscillation envelope → depth.
    pub y_axis_to_amount: f64,
    /// Oscillation envelope below this (normalized Y units) is treated as jitter.
    pub gesture_deadzone: f64,
    /// Hard cap on vibrato depth in semitones.
    pub max_depth_semitones: f64,
    /// EMA time constant separating slow manual bend from oscillation (ms).
    pub manual_bend_smoothing_ms: f64,
}

impl Default for VibratoConfig {
    fn default() -> Self {
        Self {
            enabled_by_default: true,
            default_depth_semitones: 0.35,
            default_rate_hz: 6.0,
            fade_in_ms: 200.0,
            depth_smoothing_ms: 50.0,
            rate_smoothing_ms: 80.0,
            pressure_to_amount: 0.5,
            y_axis_to_amount: 1.0,
            gesture_deadzone: 0.002,
            max_depth_semitones: 1.5,
            manual_bend_smoothing_ms: 8.0,
        }
    }
}

impl VibratoConfig {
    /// Clamp fields to sane ranges (called from [`MidiMapping::validate`]).
    pub fn clamp(&mut self) {
        self.default_depth_semitones = self.default_depth_semitones.clamp(0.0, 12.0);
        self.default_rate_hz = self.default_rate_hz.clamp(0.5, 20.0);
        self.fade_in_ms = self.fade_in_ms.clamp(0.0, 2000.0);
        self.depth_smoothing_ms = self.depth_smoothing_ms.clamp(0.0, 2000.0);
        self.rate_smoothing_ms = self.rate_smoothing_ms.clamp(0.0, 2000.0);
        self.pressure_to_amount = self.pressure_to_amount.clamp(0.0, 1.0);
        self.y_axis_to_amount = self.y_axis_to_amount.clamp(0.0, 4.0);
        self.gesture_deadzone = self.gesture_deadzone.clamp(0.0, 0.05);
        self.max_depth_semitones = self.max_depth_semitones.clamp(0.0, 12.0);
        self.manual_bend_smoothing_ms = self.manual_bend_smoothing_ms.clamp(0.0, 500.0);
        if self.default_depth_semitones > self.max_depth_semitones {
            self.default_depth_semitones = self.max_depth_semitones;
        }
    }
}

/// Per-voice vibrato runtime state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VibratoState {
    pub phase: f64,
    pub depth_current: f64,
    pub depth_target: f64,
    pub rate_current: f64,
    pub slow_y: f64,
    pub slow_y_initialized: bool,
    pub osc_env: f64,
    /// Fade-in progress 0..1 after gesture engages.
    pub fade: f64,
}

/// HUD-facing vibrato visualization (depth + LFO phase).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VibratoDisplay {
    /// Current depth / `max_depth_semitones`, in `[0, 1]`.
    pub depth_norm: f64,
    pub phase: f64,
}

/// Advance LFO phase by `rate_hz * dt` (wraps at τ).
#[must_use]
pub fn advance_phase(phase: f64, dt: f64, rate_hz: f64) -> f64 {
    if dt <= 0.0 || rate_hz <= 0.0 {
        return phase;
    }
    let next = phase + std::f64::consts::TAU * rate_hz * dt;
    next.rem_euclid(std::f64::consts::TAU)
}

/// Sine LFO sample in `[-1, 1]`.
#[must_use]
pub fn lfo_sine(phase: f64) -> f64 {
    phase.sin()
}

/// Exponential slew toward `target` with time constant `time_const_ms`.
#[must_use]
pub fn slew(current: f64, target: f64, dt: f64, time_const_ms: f64) -> f64 {
    if time_const_ms <= 0.0 || dt <= 0.0 {
        return target;
    }
    let tau = time_const_ms / 1000.0;
    let alpha = 1.0 - (-dt / tau).exp();
    current + (target - current) * alpha
}

/// Update slow-Y EMA used to split manual bend from oscillation.
#[must_use]
pub fn update_slow_y(
    slow_y: f64,
    y: f64,
    dt: f64,
    smoothing_ms: f64,
    initialized: bool,
) -> (f64, bool) {
    if !initialized {
        return (y, true);
    }
    (slew(slow_y, y, dt, smoothing_ms), true)
}

/// Envelope follower on |deviation|: fast attack, slower release.
#[must_use]
pub fn update_envelope(env: f64, deviation: f64, dt: f64) -> f64 {
    const ATTACK_MS: f64 = 30.0;
    const RELEASE_MS: f64 = 120.0;
    let abs_dev = deviation.abs();
    if abs_dev > env {
        slew(env, abs_dev, dt, ATTACK_MS)
    } else {
        slew(env, abs_dev, dt, RELEASE_MS)
    }
}

/// Map oscillation envelope + pressure to a target vibrato depth in semitones.
#[must_use]
pub fn target_depth_from(env: f64, pressure: f64, cfg: &VibratoConfig) -> f64 {
    if env <= cfg.gesture_deadzone || cfg.max_depth_semitones <= 0.0 {
        return 0.0;
    }
    let usable = (cfg.max_depth_semitones - cfg.gesture_deadzone).max(f64::EPSILON);
    let norm = ((env - cfg.gesture_deadzone) / usable).clamp(0.0, 1.0);
    let amount = (norm * cfg.y_axis_to_amount).clamp(0.0, 1.0);
    let depth = cfg.default_depth_semitones
        + amount * (cfg.max_depth_semitones - cfg.default_depth_semitones);
    let pressure_factor =
        1.0 - cfg.pressure_to_amount + cfg.pressure_to_amount * pressure.clamp(0.0, 1.0);
    (depth * pressure_factor).clamp(0.0, cfg.max_depth_semitones)
}

/// Current vibrato pitch offset in semitones (`sin(phase) * depth`).
#[must_use]
pub fn vibrato_bend_semitones(state: &VibratoState) -> f64 {
    lfo_sine(state.phase) * state.depth_current
}

impl VibratoState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Sample-driven gesture update: slow-Y split + oscillation envelope.
    pub fn update_gesture(&mut self, y: f64, pressure: f64, dt: f64, cfg: &VibratoConfig) {
        let (slow_y, init) =
            update_slow_y(self.slow_y, y, dt, cfg.manual_bend_smoothing_ms, self.slow_y_initialized);
        self.slow_y = slow_y;
        self.slow_y_initialized = init;
        let deviation = y - slow_y;
        self.osc_env = update_envelope(self.osc_env, deviation, dt);
        self.depth_target = target_depth_from(self.osc_env, pressure, cfg);
    }

    /// Frame-driven LFO tick: fade, depth/rate slew, phase advance.
    pub fn tick(&mut self, dt: f64, pressure: f64, cfg: &VibratoConfig) {
        if dt <= 0.0 {
            return;
        }
        // Decay envelope when no fresh samples (pen held still).
        self.osc_env = slew(self.osc_env, 0.0, dt, 120.0);
        self.depth_target = target_depth_from(self.osc_env, pressure, cfg);

        if self.depth_target > 0.0 {
            let ramp = dt * 1000.0 / cfg.fade_in_ms.max(1.0);
            self.fade = (self.fade + ramp).min(1.0);
        } else {
            self.fade = slew(self.fade, 0.0, dt, cfg.fade_in_ms.max(1.0));
        }

        let effective_target = self.depth_target * self.fade;
        self.depth_current = slew(
            self.depth_current,
            effective_target,
            dt,
            cfg.depth_smoothing_ms,
        );
        self.rate_current = slew(
            self.rate_current,
            cfg.default_rate_hz,
            dt,
            cfg.rate_smoothing_ms,
        );
        self.phase = advance_phase(self.phase, dt, self.rate_current);
    }

    /// Manual bend from slow-Y (or raw Y before EMA initializes).
    #[must_use]
    pub fn manual_bend_semitones(&self, y_origin: f64, y_bend_semitones: f64) -> f64 {
        let y = if self.slow_y_initialized {
            self.slow_y
        } else {
            return 0.0;
        };
        (y_origin - y) * y_bend_semitones
    }

    #[must_use]
    pub fn display(&self, cfg: &VibratoConfig) -> VibratoDisplay {
        let depth_norm = if cfg.max_depth_semitones > 0.0 {
            (self.depth_current / cfg.max_depth_semitones).clamp(0.0, 1.0)
        } else {
            0.0
        };
        VibratoDisplay {
            depth_norm,
            phase: self.phase,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> VibratoConfig {
        VibratoConfig::default()
    }

    #[test]
    fn lfo_sine_is_bipolar() {
        assert!((lfo_sine(0.0) - 0.0).abs() < 1e-10);
        assert!((lfo_sine(std::f64::consts::FRAC_PI_2) - 1.0).abs() < 1e-10);
        assert!((lfo_sine(std::f64::consts::PI) - 0.0).abs() < 1e-10);
        assert!((lfo_sine(3.0 * std::f64::consts::FRAC_PI_2) + 1.0).abs() < 1e-10);
    }

    #[test]
    fn phase_advance_is_frame_rate_independent() {
        let rate = 6.0;
        let mut phase_a = 0.0;
        for _ in 0..120 {
            phase_a = advance_phase(phase_a, 1.0 / 120.0, rate);
        }
        let mut phase_b = 0.0;
        phase_b = advance_phase(phase_b, 1.0, rate);
        assert!(
            (phase_a - phase_b).abs() < 0.01,
            "120×(1/120)s should ≈ 1s: {phase_a} vs {phase_b}"
        );
    }

    #[test]
    fn deadzone_rejects_small_envelope() {
        let c = cfg();
        assert_eq!(target_depth_from(0.001, 1.0, &c), 0.0);
        assert!(target_depth_from(0.01, 1.0, &c) > 0.0);
    }

    #[test]
    fn fade_in_ramps_depth() {
        let mut state = VibratoState::default();
        state.osc_env = 0.01;
        let c = cfg();
        for _ in 0..50 {
            state.tick(0.004, 1.0, &c);
        }
        assert!(
            state.depth_current > 0.0 && state.depth_current < c.max_depth_semitones,
            "fade should ramp in gradually, got {}",
            state.depth_current
        );
    }

    #[test]
    fn depth_zero_produces_no_vibrato_bend() {
        let state = VibratoState::default();
        assert_eq!(vibrato_bend_semitones(&state), 0.0);
    }

    #[test]
    fn reset_clears_state() {
        let mut state = VibratoState {
            phase: 1.5,
            depth_current: 0.4,
            depth_target: 0.4,
            rate_current: 6.0,
            slow_y: 0.3,
            slow_y_initialized: true,
            osc_env: 0.01,
            fade: 1.0,
        };
        state.reset();
        assert_eq!(state, VibratoState::default());
    }

    #[test]
    fn high_rate_phase_stays_bounded() {
        let mut phase = 0.0;
        for _ in 0..10_000 {
            phase = advance_phase(phase, 1.0 / 240.0, 20.0);
        }
        assert!(phase >= 0.0 && phase < std::f64::consts::TAU);
    }

    #[test]
    fn manual_plus_vibrato_combine_in_semitones() {
        let state = VibratoState {
            depth_current: 0.3,
            phase: std::f64::consts::FRAC_PI_2,
            ..Default::default()
        };
        let manual = 2.0;
        let combined = manual + vibrato_bend_semitones(&state);
        assert!((combined - 2.3).abs() < 1e-10);
    }

    #[test]
    fn config_clamp_keeps_values_in_range() {
        let mut c = VibratoConfig {
            default_rate_hz: 100.0,
            max_depth_semitones: 50.0,
            gesture_deadzone: 1.0,
            ..Default::default()
        };
        c.clamp();
        assert!(c.default_rate_hz <= 20.0);
        assert!(c.max_depth_semitones <= 12.0);
        assert!(c.gesture_deadzone <= 0.05);
    }
}
