//! Stage 5 — Activation / hover threshold.
//!
//! Implements [`ActivationConfig::apply`], which derives
//! [`ProcessedSample::active`](crate::ProcessedSample::active) from:
//!
//! 1. **Pressure hysteresis** — separate on/off thresholds so the active flag
//!    does not chatter when pressure hovers in the threshold band.
//! 2. **Proximity gating** — optionally requiring `in_proximity` in addition to
//!    pressure, so the pen going out of range immediately clears `active`.
//!
//! The latch state (`active_latch: bool`) lives in
//! [`ProcessorState`](crate::ProcessorState), not in the profile.  The profile
//! itself stays plain serializable data.
//!
//! # Hysteresis rule
//!
//! ```text
//! if pressure >= on_threshold  → switch active = true
//! if pressure <= off_threshold → switch active = false
//! if off_threshold < pressure < on_threshold → hold previous state (no chatter)
//! ```
//!
//! `off_threshold` must be ≤ `on_threshold`.  If they are equal there is no
//! hysteresis band and the behavior degenerates to a single threshold.
//!
//! # When disabled
//!
//! When `enabled = false`, `active` equals `raw.in_proximity` (identity
//! behaviour matching TC1.1).

use crate::profile::ActivationConfig;
use crate::state::ProcessorState;

// ── ActivationConfig::apply ──────────────────────────────────────────────────

impl ActivationConfig {
    /// Derive the `active` signal for one sample.
    ///
    /// # Parameters
    ///
    /// - `pressure` — the **processed** pressure in `[0.0, 1.0]` (i.e. after
    ///   the pressure-response stage, not the raw value).
    /// - `in_proximity` — whether the pen is in range of the tablet.
    /// - `state` — mutable per-stream state holding the hysteresis latch.
    ///
    /// # Returns
    ///
    /// `true` when the pen is considered "active" (in contact / pressed) per the
    /// configured thresholds and proximity gate.
    ///
    /// # When to call
    ///
    /// Only call this when `self.enabled == true`.  The caller (see
    /// [`crate::CalibrationProfile::apply`]) falls back to `raw.in_proximity`
    /// when the stage is disabled.
    pub fn apply(&self, pressure: f64, in_proximity: bool, state: &mut ProcessorState) -> bool {
        // ── Pressure hysteresis ──────────────────────────────────────────────
        if pressure >= self.on_threshold {
            state.active_latch = true;
        } else if pressure <= self.off_threshold {
            state.active_latch = false;
        }
        // In the band (off_threshold < pressure < on_threshold): hold previous.

        // ── Proximity gate ───────────────────────────────────────────────────
        if self.require_proximity && !in_proximity {
            // Pen left the tablet surface; clear the latch so contact will be
            // freshly detected on the next approach.
            state.active_latch = false;
        }

        state.active_latch
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::profile::ActivationConfig;
    use crate::state::ProcessorState;

    fn cfg(on: f64, off: f64, require_prox: bool) -> ActivationConfig {
        ActivationConfig {
            enabled: true,
            on_threshold: on,
            off_threshold: off,
            require_proximity: require_prox,
        }
    }

    // ── Hysteresis: no chatter in the band ───────────────────────────────────

    /// A pressure value inside the band (off < p < on) holds the previous state.
    #[test]
    fn hysteresis_holds_previous_state_in_band() {
        let c = cfg(0.5, 0.3, false);
        let mut state = ProcessorState::new();
        // Start inactive: pressure below off_threshold.
        let a = c.apply(0.1, true, &mut state);
        assert!(!a, "should be inactive below off_threshold");

        // Rise above on_threshold → becomes active.
        let a = c.apply(0.6, true, &mut state);
        assert!(a, "should become active above on_threshold");

        // Drop into the band [0.3, 0.5] → must HOLD active (no chatter).
        let a = c.apply(0.4, true, &mut state);
        assert!(a, "should stay active inside hysteresis band (was active)");

        // Drop below off_threshold → becomes inactive.
        let a = c.apply(0.2, true, &mut state);
        assert!(!a, "should become inactive below off_threshold");

        // Rise into the band again → must HOLD inactive (no chatter).
        let a = c.apply(0.4, true, &mut state);
        assert!(
            !a,
            "should stay inactive inside hysteresis band (was inactive)"
        );

        // Rise above on_threshold again → becomes active.
        let a = c.apply(0.55, true, &mut state);
        assert!(a, "should become active again above on_threshold");
    }

    /// With equal on/off thresholds there is no hysteresis band.
    #[test]
    fn equal_thresholds_no_band() {
        let c = cfg(0.5, 0.5, false);
        let mut state = ProcessorState::new();
        // Exactly at the threshold: both conditions fire simultaneously.
        // on: pressure >= 0.5 → true; off: pressure <= 0.5 → also true.
        // off fires second (it's an `else if`) so the latch ends up false.
        // This is acceptable degenerate behavior; the key requirement is
        // no chatter for values strictly inside a non-empty band.
        let a = c.apply(0.5, true, &mut state);
        // The value exactly at the threshold triggers on, then stays on
        // (both fire? No—they're if/else if, so on fires first → true).
        assert!(a, "at threshold should be active (on fires first)");

        // Below threshold → inactive.
        let a2 = c.apply(0.4, true, &mut state);
        assert!(!a2, "below threshold should be inactive");
    }

    // ── Proximity gating ─────────────────────────────────────────────────────

    /// With require_proximity = true, going out of proximity clears active.
    #[test]
    fn proximity_gate_clears_active_on_leave() {
        let c = cfg(0.3, 0.1, true);
        let mut state = ProcessorState::new();

        // Press down: in proximity, above on_threshold.
        let a = c.apply(0.4, true, &mut state);
        assert!(a, "should be active in proximity above threshold");

        // Pen leaves the tablet surface.
        let a = c.apply(0.4, false, &mut state);
        assert!(!a, "should be inactive when out of proximity");

        // Latch was cleared: even if proximity returns, pressure must re-trigger.
        let a = c.apply(0.4, true, &mut state);
        assert!(
            a,
            "should re-activate when back in proximity and above threshold"
        );
    }

    /// With require_proximity = false, proximity does not gate activation.
    #[test]
    fn no_proximity_gate_ignores_proximity() {
        let c = cfg(0.3, 0.1, false);
        let mut state = ProcessorState::new();

        // Activate via pressure.
        c.apply(0.4, true, &mut state);

        // Go out of proximity: latch must NOT be cleared.
        let a = c.apply(0.4, false, &mut state);
        assert!(
            a,
            "should stay active when proximity gate disabled and pressure high"
        );
    }

    // ── Simple on/off ────────────────────────────────────────────────────────

    /// Pressure rising above on_threshold activates.
    #[test]
    fn activates_above_on_threshold() {
        let c = cfg(0.4, 0.2, false);
        let mut state = ProcessorState::new();
        let a = c.apply(0.5, true, &mut state);
        assert!(a);
    }

    /// Pressure below off_threshold deactivates from the start.
    #[test]
    fn inactive_below_off_threshold() {
        let c = cfg(0.4, 0.2, false);
        let mut state = ProcessorState::new();
        let a = c.apply(0.1, true, &mut state);
        assert!(!a);
    }

    /// Fresh state starts inactive.
    #[test]
    fn fresh_state_starts_inactive() {
        assert!(!ProcessorState::new().active_latch);
    }
}
