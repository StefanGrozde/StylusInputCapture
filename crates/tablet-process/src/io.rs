//! Profile (de)serialization, validation, and I/O.
//!
//! Provides [`CalibrationProfile::load`] and [`CalibrationProfile::save`] with
//! automatic format selection based on the file extension:
//!
//! | Extension | Format |
//! | --------- | ------ |
//! | `.toml`   | TOML (default, human-readable) |
//! | `.json`   | JSON  |
//!
//! Any other extension is treated as TOML.
//!
//! # Validation
//!
//! [`CalibrationProfile::validate`] is called automatically by `load` after
//! parsing.  It rejects profiles with semantically invalid parameters (e.g.
//! `pressure_min > pressure_max`, non-positive gamma, `alpha` outside `(0, 1]`).
//! See [`ProfileError`] for the full list of typed errors.

use std::path::Path;

use crate::profile::{CalibrationProfile, CurveKind, SmoothingMode};

// ── ProfileError ─────────────────────────────────────────────────────────────

/// Errors returned by [`CalibrationProfile::load`], [`CalibrationProfile::save`],
/// and [`CalibrationProfile::validate`].
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// The profile file could not be read from or written to disk.
    ///
    /// `path` is the path that was attempted; `source` is the underlying
    /// I/O error.
    #[error("I/O error for '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// The file content could not be parsed as TOML.
    ///
    /// Includes the raw parser error message for actionable diagnosis.
    #[error("TOML parse error in '{path}': {message}")]
    TomlParse { path: String, message: String },

    /// The file content could not be parsed as JSON.
    ///
    /// Includes the raw parser error message for actionable diagnosis.
    #[error("JSON parse error in '{path}': {message}")]
    JsonParse { path: String, message: String },

    /// The profile was parsed successfully but failed semantic validation.
    ///
    /// `field` names the failing field/stage; `reason` explains what rule was
    /// violated and what values are acceptable.
    #[error("Invalid profile field '{field}': {reason}")]
    Validation { field: &'static str, reason: String },
}

// ── Format detection ──────────────────────────────────────────────────────────

/// Determine the serialization format from the file extension.
///
/// `.json` → JSON; anything else (including `.toml` or no extension) → TOML.
fn is_json(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
}

// ── CalibrationProfile I/O ────────────────────────────────────────────────────

impl CalibrationProfile {
    /// Load a [`CalibrationProfile`] from `path`.
    ///
    /// The serialization format is chosen by the file extension:
    /// - `.json` → JSON (via `serde_json`).
    /// - All other extensions (including `.toml`) → TOML (via `toml`).
    ///
    /// After parsing, [`CalibrationProfile::validate`] is called.  A parse
    /// success that fails validation is returned as
    /// [`ProfileError::Validation`].
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::Io`] on file-read failures,
    /// [`ProfileError::TomlParse`] / [`ProfileError::JsonParse`] on parse
    /// failures, and [`ProfileError::Validation`] on semantic errors.
    pub fn load(path: &Path) -> Result<Self, ProfileError> {
        let path_str = path.display().to_string();

        let text = std::fs::read_to_string(path).map_err(|e| ProfileError::Io {
            path: path_str.clone(),
            source: e,
        })?;

        let profile: CalibrationProfile = if is_json(path) {
            serde_json::from_str(&text).map_err(|e| ProfileError::JsonParse {
                path: path_str,
                message: e.to_string(),
            })?
        } else {
            toml::from_str(&text).map_err(|e| ProfileError::TomlParse {
                path: path_str,
                message: e.to_string(),
            })?
        };

        profile.validate()?;
        Ok(profile)
    }

    /// Save this [`CalibrationProfile`] to `path`.
    ///
    /// The serialization format is chosen by the file extension:
    /// - `.json` → JSON (via `serde_json`, pretty-printed).
    /// - All other extensions (including `.toml`) → TOML (via `toml`).
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::Io`] if the file cannot be written.
    /// Serialization of a well-formed profile should never fail; if it does,
    /// the error is wrapped in [`ProfileError::Io`] (the I/O write error path
    /// is re-used for the serialization failure converted to an `io::Error`).
    pub fn save(&self, path: &Path) -> Result<(), ProfileError> {
        let path_str = path.display().to_string();

        let text = if is_json(path) {
            serde_json::to_string_pretty(self).map_err(|e| ProfileError::Io {
                path: path_str.clone(),
                source: std::io::Error::other(e.to_string()),
            })?
        } else {
            toml::to_string_pretty(self).map_err(|e| ProfileError::Io {
                path: path_str.clone(),
                source: std::io::Error::other(e.to_string()),
            })?
        };

        std::fs::write(path, text).map_err(|e| ProfileError::Io {
            path: path_str,
            source: e,
        })?;

        Ok(())
    }

    /// Validate the profile's parameter ranges and semantic invariants.
    ///
    /// Called automatically by [`CalibrationProfile::load`]; callers that
    /// construct a profile programmatically may also call this before saving
    /// or applying.
    ///
    /// # Validated rules
    ///
    /// **Pressure stage:**
    /// - `pressure_min <= pressure_max`.
    /// - `Gamma(g)`: `g > 0.0`.
    /// - `Custom(points)`: control-point `t` values must be non-decreasing
    ///   (sorted) **and** `v` values must be non-decreasing (monotone).
    ///
    /// **Smoothing stage:**
    /// - `Ema { alpha }`: `alpha` must be in `(0.0, 1.0]`.
    /// - `OneEuro`: `min_cutoff > 0.0`, `d_cutoff > 0.0`, `beta >= 0.0`.
    ///
    /// **Activation stage:**
    /// - `off_threshold <= on_threshold`.
    /// - Both thresholds must be in `[0.0, 1.0]`.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::Validation`] for the first rule that is
    /// violated, with `field` naming the offending struct field and `reason`
    /// giving an actionable description.
    pub fn validate(&self) -> Result<(), ProfileError> {
        // ── Pressure stage ────────────────────────────────────────────────────
        let p = &self.pressure;

        if p.pressure_min > p.pressure_max {
            return Err(ProfileError::Validation {
                field: "pressure.pressure_min / pressure_max",
                reason: format!(
                    "pressure_min ({}) must be <= pressure_max ({})",
                    p.pressure_min, p.pressure_max
                ),
            });
        }

        match &p.curve {
            CurveKind::Gamma(g) => {
                if *g <= 0.0 {
                    return Err(ProfileError::Validation {
                        field: "pressure.curve (Gamma)",
                        reason: format!("gamma exponent must be > 0.0, got {g}"),
                    });
                }
            }
            CurveKind::Custom(points) => {
                validate_custom_points(points)?;
            }
            CurveKind::Linear => {}
        }

        // ── Smoothing stage ───────────────────────────────────────────────────
        match &self.smoothing.mode {
            SmoothingMode::Ema { alpha } => {
                if *alpha <= 0.0 || *alpha > 1.0 {
                    return Err(ProfileError::Validation {
                        field: "smoothing.mode (Ema.alpha)",
                        reason: format!("EMA alpha must be in (0.0, 1.0], got {alpha}"),
                    });
                }
            }
            SmoothingMode::OneEuro(params) => {
                if params.min_cutoff <= 0.0 {
                    return Err(ProfileError::Validation {
                        field: "smoothing.mode (OneEuro.min_cutoff)",
                        reason: format!("min_cutoff must be > 0.0, got {}", params.min_cutoff),
                    });
                }
                if params.d_cutoff <= 0.0 {
                    return Err(ProfileError::Validation {
                        field: "smoothing.mode (OneEuro.d_cutoff)",
                        reason: format!("d_cutoff must be > 0.0, got {}", params.d_cutoff),
                    });
                }
                if params.beta < 0.0 {
                    return Err(ProfileError::Validation {
                        field: "smoothing.mode (OneEuro.beta)",
                        reason: format!("beta must be >= 0.0, got {}", params.beta),
                    });
                }
            }
            SmoothingMode::Off => {}
        }

        // ── Activation stage ──────────────────────────────────────────────────
        let a = &self.activation;

        if !(0.0..=1.0).contains(&a.on_threshold) {
            return Err(ProfileError::Validation {
                field: "activation.on_threshold",
                reason: format!("on_threshold must be in [0.0, 1.0], got {}", a.on_threshold),
            });
        }
        if !(0.0..=1.0).contains(&a.off_threshold) {
            return Err(ProfileError::Validation {
                field: "activation.off_threshold",
                reason: format!(
                    "off_threshold must be in [0.0, 1.0], got {}",
                    a.off_threshold
                ),
            });
        }
        if a.off_threshold > a.on_threshold {
            return Err(ProfileError::Validation {
                field: "activation.off_threshold / on_threshold",
                reason: format!(
                    "off_threshold ({}) must be <= on_threshold ({})",
                    a.off_threshold, a.on_threshold
                ),
            });
        }

        Ok(())
    }
}

// ── Custom-point validation ───────────────────────────────────────────────────

/// Validate that a slice of `Custom` pressure-curve control points is sorted
/// (non-decreasing `t`) and monotone (non-decreasing `v`).
fn validate_custom_points(points: &[(f64, f64)]) -> Result<(), ProfileError> {
    for (i, window) in points.windows(2).enumerate() {
        let (t0, v0) = window[0];
        let (t1, v1) = window[1];

        if t1 < t0 {
            return Err(ProfileError::Validation {
                field: "pressure.curve (Custom points — t values)",
                reason: format!(
                    "control-point t values must be non-decreasing; \
                     point[{i}].t={t0} > point[{}].t={t1}",
                    i + 1
                ),
            });
        }
        if v1 < v0 {
            return Err(ProfileError::Validation {
                field: "pressure.curve (Custom points — v values)",
                reason: format!(
                    "control-point v values must be non-decreasing (monotone); \
                     point[{i}].v={v0} > point[{}].v={v1}",
                    i + 1
                ),
            });
        }
    }
    Ok(())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{
        ActivationConfig, CurveKind, OneEuroParams, PressureCurve, SmoothingConfig, SmoothingMode,
    };

    // ── validate: pressure ────────────────────────────────────────────────────

    /// Identity profile validates without error.
    #[test]
    fn identity_validates_ok() {
        CalibrationProfile::identity().validate().unwrap();
    }

    /// pressure_min > pressure_max is rejected.
    #[test]
    fn validate_pressure_min_gt_max() {
        let mut p = CalibrationProfile::identity();
        p.pressure = PressureCurve {
            enabled: true,
            pressure_min: 900,
            pressure_max: 100,
            curve: CurveKind::Linear,
        };
        let err = p.validate().unwrap_err();
        assert!(
            matches!(err, ProfileError::Validation { field, .. } if field.contains("pressure_min")),
            "unexpected error: {err}"
        );
    }

    /// Gamma(0.0) is rejected.
    #[test]
    fn validate_gamma_zero_rejected() {
        let mut p = CalibrationProfile::identity();
        p.pressure = PressureCurve {
            enabled: true,
            pressure_min: 0,
            pressure_max: 1000,
            curve: CurveKind::Gamma(0.0),
        };
        let err = p.validate().unwrap_err();
        assert!(
            matches!(err, ProfileError::Validation { field, .. } if field.contains("Gamma")),
            "unexpected error: {err}"
        );
    }

    /// Gamma(-1.0) is rejected.
    #[test]
    fn validate_gamma_negative_rejected() {
        let mut p = CalibrationProfile::identity();
        p.pressure.curve = CurveKind::Gamma(-1.0);
        assert!(p.validate().is_err());
    }

    /// Non-monotone custom control points (v decreasing) are rejected.
    #[test]
    fn validate_custom_non_monotone_v() {
        let mut p = CalibrationProfile::identity();
        p.pressure.curve = CurveKind::Custom(vec![(0.0, 0.0), (0.5, 0.8), (1.0, 0.5)]);
        let err = p.validate().unwrap_err();
        assert!(
            matches!(err, ProfileError::Validation { field, .. } if field.contains("v values")),
            "unexpected error: {err}"
        );
    }

    /// Unsorted custom control points (t decreasing) are rejected.
    #[test]
    fn validate_custom_unsorted_t() {
        let mut p = CalibrationProfile::identity();
        p.pressure.curve = CurveKind::Custom(vec![(0.0, 0.0), (0.8, 0.5), (0.3, 0.8), (1.0, 1.0)]);
        let err = p.validate().unwrap_err();
        assert!(
            matches!(err, ProfileError::Validation { field, .. } if field.contains("t values")),
            "unexpected error: {err}"
        );
    }

    // ── validate: smoothing ───────────────────────────────────────────────────

    /// EMA alpha = 0.0 is rejected.
    #[test]
    fn validate_ema_alpha_zero_rejected() {
        let mut p = CalibrationProfile::identity();
        p.smoothing = SmoothingConfig {
            mode: SmoothingMode::Ema { alpha: 0.0 },
        };
        let err = p.validate().unwrap_err();
        assert!(
            matches!(err, ProfileError::Validation { field, .. } if field.contains("alpha")),
            "unexpected error: {err}"
        );
    }

    /// EMA alpha > 1.0 is rejected.
    #[test]
    fn validate_ema_alpha_gt_one_rejected() {
        let mut p = CalibrationProfile::identity();
        p.smoothing = SmoothingConfig {
            mode: SmoothingMode::Ema { alpha: 1.5 },
        };
        assert!(p.validate().is_err());
    }

    /// EMA alpha = 1.0 is accepted.
    #[test]
    fn validate_ema_alpha_one_accepted() {
        let mut p = CalibrationProfile::identity();
        p.smoothing = SmoothingConfig {
            mode: SmoothingMode::Ema { alpha: 1.0 },
        };
        p.validate().unwrap();
    }

    /// OneEuro min_cutoff = 0.0 is rejected.
    #[test]
    fn validate_one_euro_min_cutoff_zero_rejected() {
        let mut p = CalibrationProfile::identity();
        p.smoothing = SmoothingConfig {
            mode: SmoothingMode::OneEuro(OneEuroParams {
                min_cutoff: 0.0,
                beta: 0.1,
                d_cutoff: 1.0,
            }),
        };
        let err = p.validate().unwrap_err();
        assert!(
            matches!(err, ProfileError::Validation { field, .. } if field.contains("min_cutoff")),
            "unexpected error: {err}"
        );
    }

    /// OneEuro d_cutoff = 0.0 is rejected.
    #[test]
    fn validate_one_euro_d_cutoff_zero_rejected() {
        let mut p = CalibrationProfile::identity();
        p.smoothing = SmoothingConfig {
            mode: SmoothingMode::OneEuro(OneEuroParams {
                min_cutoff: 1.0,
                beta: 0.0,
                d_cutoff: 0.0,
            }),
        };
        assert!(p.validate().is_err());
    }

    /// OneEuro negative beta is rejected.
    #[test]
    fn validate_one_euro_negative_beta_rejected() {
        let mut p = CalibrationProfile::identity();
        p.smoothing = SmoothingConfig {
            mode: SmoothingMode::OneEuro(OneEuroParams {
                min_cutoff: 1.0,
                beta: -0.1,
                d_cutoff: 1.0,
            }),
        };
        assert!(p.validate().is_err());
    }

    // ── validate: activation ──────────────────────────────────────────────────

    /// off_threshold > on_threshold is rejected.
    #[test]
    fn validate_activation_off_gt_on() {
        let mut p = CalibrationProfile::identity();
        p.activation = ActivationConfig {
            enabled: true,
            on_threshold: 0.3,
            off_threshold: 0.5,
            require_proximity: true,
        };
        let err = p.validate().unwrap_err();
        assert!(
            matches!(err, ProfileError::Validation { field, .. } if field.contains("off_threshold")),
            "unexpected error: {err}"
        );
    }

    /// on_threshold > 1.0 is rejected.
    #[test]
    fn validate_activation_on_threshold_out_of_range() {
        let mut p = CalibrationProfile::identity();
        p.activation = ActivationConfig {
            enabled: true,
            on_threshold: 1.5,
            off_threshold: 0.0,
            require_proximity: false,
        };
        assert!(p.validate().is_err());
    }
}
