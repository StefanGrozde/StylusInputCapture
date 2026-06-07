//! Pipeline stage implementations.
//!
//! Each sub-module contains the application logic for one
//! [`CalibrationProfile`] stage.  The stage *parameters* (the data carried in
//! the profile and serialized to `*.cal.toml`) live in [`crate::profile`];
//! only the `apply` / computation logic lives here.
//!
//! [`CalibrationProfile`]: crate::CalibrationProfile

pub mod activation;
pub mod coordinate;
pub mod pressure;
pub mod smoothing;
pub mod tilt;
