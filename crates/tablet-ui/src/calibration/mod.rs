//! Calibration workflow state — UI-side helpers that drive `tablet-process`'s
//! pure fitting routines (TC3.3).
//!
//! Kept separate from `panels::calibration` so the workflow state and its pure
//! mutating methods are independently unit-testable without an `egui::Ui`.

pub mod geometry;

pub use geometry::GeometryCalibration;
