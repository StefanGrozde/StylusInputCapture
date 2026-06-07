//! N-point geometry calibration workflow (TC3.3, SPEC_CUI.md §4.3, §6.5).
//!
//! Holds the list of `(raw, target)` correspondence points the user collects
//! interactively, plus the most recent [`FitReport`]. The actual least-squares
//! solve is delegated entirely to [`tablet_process::fit_geometry`] — this
//! module only manages the point list and bridges it to that pure API.
//!
//! Kept free of `egui` so the mutating methods (`add_point`, `remove`,
//! `clear`, `fit`) are unit-testable in isolation; `panels::calibration` owns
//! all rendering.

use tablet_process::fit::PointPair;
use tablet_process::{fit_geometry, FitReport};

/// A single calibration correspondence: a raw digitizer position paired with
/// the target-space coordinate the user intends it to map to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalibrationPoint {
    /// Raw digitizer position `(x_raw, y_raw)`, captured from the live sample
    /// at the moment "Add point" was clicked.
    pub raw: (f64, f64),
    /// Intended target-space coordinate for this point, entered numerically
    /// by the user (in the profile's `target_space` units).
    pub target: (f64, f64),
}

/// Workflow state for the N-point geometry calibration UI.
///
/// The panel mutates this directly (`add_point` / `remove` / `clear`) and
/// calls [`GeometryCalibration::fit`] to (re)compute the best-fit
/// [`CoordinateMap`](tablet_process::CoordinateMap); the caller is then
/// responsible for writing `report.coordinate_map` into the active
/// `CalibrationProfile`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GeometryCalibration {
    /// Collected correspondence points, in collection order.
    pub points: Vec<CalibrationPoint>,
    /// The most recent fit result, retained so the panel can display
    /// per-point residuals and the RMS error after the user clicks "Fit".
    /// Cleared implicitly only by replacing it on the next successful fit —
    /// it is *not* invalidated by point edits, so stale reports remain
    /// visible (labelled by point count) until the user re-fits.
    pub last_report: Option<FitReport>,
    /// Numeric target-coordinate entry buffer, bound to the "Add point"
    /// target fields (`DragValue`s) between additions.
    pub pending_target: (f64, f64),
}

/// Minimum number of correspondence points [`fit_geometry`] needs to produce
/// a non-trivial transform (a 2-point similarity fit). Below this, `fit`
/// returns `None` rather than handing `fit_geometry` a degenerate input.
pub const MIN_FIT_POINTS: usize = 2;

impl GeometryCalibration {
    /// Create an empty workflow state with `pending_target = (0.0, 0.0)`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a new correspondence point.
    pub fn add_point(&mut self, raw: (f64, f64), target: (f64, f64)) {
        self.points.push(CalibrationPoint { raw, target });
    }

    /// Remove the point at `idx`, if present. Out-of-range indices are
    /// ignored (no panic) so the panel can call this directly from a button
    /// handler without bounds-checking first.
    pub fn remove(&mut self, idx: usize) {
        if idx < self.points.len() {
            self.points.remove(idx);
        }
    }

    /// Remove all collected points. Does *not* clear `last_report` — the
    /// previous fit result remains visible (with its own `point_count`) until
    /// the user fits again, which avoids the panel flickering to "no report"
    /// the instant "Clear all" is pressed.
    pub fn clear(&mut self) {
        self.points.clear();
    }

    /// Convert the collected points into the `&[PointPair]` shape
    /// `fit_geometry` expects, preserving collection order.
    pub fn as_point_pairs(&self) -> Vec<PointPair> {
        self.points.iter().map(|p| (p.raw, p.target)).collect()
    }

    /// Fit the best coordinate-mapping transform from the collected points.
    ///
    /// Returns `None` (and leaves `last_report` untouched) when fewer than
    /// [`MIN_FIT_POINTS`] points have been collected — `fit_geometry` would
    /// otherwise hand back a degenerate identity report that isn't useful to
    /// apply. On success, stores and returns a clone of the new
    /// [`FitReport`]; the caller writes `report.coordinate_map` into the
    /// active profile.
    pub fn fit(&mut self) -> Option<FitReport> {
        if self.points.len() < MIN_FIT_POINTS {
            return None;
        }
        let pairs = self.as_point_pairs();
        let report = fit_geometry(&pairs);
        self.last_report = Some(report.clone());
        Some(report)
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-6;

    /// Apply an affine matrix `[a,b,c,d,e,f]` to a point — mirrors
    /// `tablet_process::fit::tests::affine_apply` for generating known-good
    /// correspondence pairs.
    fn affine_apply(m: [f64; 6], x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = m;
        (a * x + b * y + c, d * x + e * y + f)
    }

    #[test]
    fn add_point_appends_in_order() {
        let mut cal = GeometryCalibration::new();
        cal.add_point((1.0, 2.0), (10.0, 20.0));
        cal.add_point((3.0, 4.0), (30.0, 40.0));

        assert_eq!(cal.points.len(), 2);
        assert_eq!(cal.points[0].raw, (1.0, 2.0));
        assert_eq!(cal.points[0].target, (10.0, 20.0));
        assert_eq!(cal.points[1].raw, (3.0, 4.0));
        assert_eq!(cal.points[1].target, (30.0, 40.0));
    }

    #[test]
    fn remove_deletes_the_point_at_index() {
        let mut cal = GeometryCalibration::new();
        cal.add_point((0.0, 0.0), (0.0, 0.0));
        cal.add_point((1.0, 1.0), (1.0, 1.0));
        cal.add_point((2.0, 2.0), (2.0, 2.0));

        cal.remove(1);

        assert_eq!(cal.points.len(), 2);
        assert_eq!(cal.points[0].raw, (0.0, 0.0));
        assert_eq!(cal.points[1].raw, (2.0, 2.0));
    }

    #[test]
    fn remove_out_of_range_is_a_no_op() {
        let mut cal = GeometryCalibration::new();
        cal.add_point((0.0, 0.0), (0.0, 0.0));

        cal.remove(5); // out of range — must not panic
        cal.remove(1); // also out of range (len == 1)

        assert_eq!(cal.points.len(), 1);
    }

    #[test]
    fn clear_removes_all_points() {
        let mut cal = GeometryCalibration::new();
        cal.add_point((0.0, 0.0), (0.0, 0.0));
        cal.add_point((1.0, 1.0), (1.0, 1.0));

        cal.clear();

        assert!(cal.points.is_empty());
    }

    #[test]
    fn as_point_pairs_round_trips_raw_and_target() {
        let mut cal = GeometryCalibration::new();
        cal.add_point((1.5, 2.5), (100.0, 200.0));
        cal.add_point((3.5, 4.5), (300.0, 400.0));

        let pairs = cal.as_point_pairs();

        assert_eq!(pairs, vec![
            ((1.5, 2.5), (100.0, 200.0)),
            ((3.5, 4.5), (300.0, 400.0)),
        ]);
    }

    #[test]
    fn fit_returns_none_below_min_points() {
        let mut cal = GeometryCalibration::new();
        assert_eq!(cal.fit(), None);
        assert!(cal.last_report.is_none());

        cal.add_point((0.0, 0.0), (0.0, 0.0));
        assert_eq!(cal.fit(), None, "a single point must not produce a fit");
        assert!(cal.last_report.is_none());
    }

    #[test]
    fn fit_with_known_affine_yields_near_zero_residuals_and_enabled_map() {
        // A simple known affine: scale ×2, translate by (10, 5).
        let m = [2.0, 0.0, 10.0, 0.0, 2.0, 5.0];
        let srcs = [
            (0.0, 0.0),
            (1.0, 0.0),
            (0.0, 1.0),
            (1.0, 1.0),
            (0.5, 0.5),
        ];

        let mut cal = GeometryCalibration::new();
        for &(x, y) in &srcs {
            cal.add_point((x, y), affine_apply(m, x, y));
        }

        let report = cal.fit().expect("≥4 points must produce a fit");

        assert!(report.rms < EPS, "rms={}", report.rms);
        for (i, r) in report.residuals.iter().enumerate() {
            assert!(*r < EPS, "residual[{i}]={r}");
        }
        assert!(report.coordinate_map.enabled, "fitted map must be enabled");

        // The stored report matches the returned one.
        assert_eq!(cal.last_report, Some(report));
    }

    #[test]
    fn fit_with_two_points_uses_similarity_and_stores_report() {
        let mut cal = GeometryCalibration::new();
        cal.add_point((0.0, 0.0), (10.0, 20.0));
        cal.add_point((1.0, 0.0), (11.0, 20.0));

        let report = cal.fit().expect("2 points must produce a similarity fit");

        assert_eq!(report.point_count, 2);
        assert!(!report.is_projective);
        assert!(report.coordinate_map.enabled);
        assert_eq!(cal.last_report, Some(report));
    }

    #[test]
    fn refitting_after_edits_replaces_the_stored_report() {
        let mut cal = GeometryCalibration::new();
        cal.add_point((0.0, 0.0), (0.0, 0.0));
        cal.add_point((1.0, 0.0), (2.0, 0.0));
        let first = cal.fit().unwrap();
        assert_eq!(first.point_count, 2);

        cal.add_point((0.0, 1.0), (0.0, 2.0));
        let second = cal.fit().unwrap();
        assert_eq!(second.point_count, 3);
        assert_eq!(cal.last_report, Some(second));
    }
}
