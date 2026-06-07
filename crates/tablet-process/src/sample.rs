//! [`ProcessedSample`] — a raw [`PenSample`] plus all values derived by the
//! active [`CalibrationProfile`].
//!
//! The `raw` field is never mutated; consumers can always fall back to the
//! ground-truth capture data while also reading the convenience derived fields.
//!
//! [`PenSample`]: tablet_core::PenSample
//! [`CalibrationProfile`]: crate::CalibrationProfile

use serde::{Deserialize, Serialize};
use tablet_core::PenSample;

/// A raw sample plus everything the active [`CalibrationProfile`] derived from
/// it.
///
/// `raw` is never mutated, so consumers can always fall back to ground truth.
///
/// [`CalibrationProfile`]: crate::CalibrationProfile
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProcessedSample {
    /// Untouched capture sample (SPEC_1.md §5.1).
    pub raw: PenSample,

    /// Position mapped into the profile's target space (see
    /// [`TargetSpace`](crate::TargetSpace)).
    pub x: f64,
    /// Position mapped into the profile's target space.
    pub y: f64,
    /// Smoothed/filtered position (None if smoothing is disabled).
    pub x_filtered: Option<f64>,
    /// Smoothed/filtered position (None if smoothing is disabled).
    pub y_filtered: Option<f64>,

    /// Pressure after the response curve and clamp, in [0.0, 1.0].
    pub pressure: f64,
    /// Whether the pen is "active" (in contact) per the activation threshold.
    pub active: bool,

    /// Tilt expressed in the profile's chosen convention / units.
    pub tilt_x: Option<f64>,
    /// Tilt expressed in the profile's chosen convention / units.
    pub tilt_y: Option<f64>,
    /// Twist / rotation in the profile's chosen units.
    pub twist: Option<f64>,

    /// `true` if any axis fell outside [`DeviceCapabilities`] range this
    /// sample.
    ///
    /// [`DeviceCapabilities`]: tablet_core::DeviceCapabilities
    pub out_of_range: bool,
}
