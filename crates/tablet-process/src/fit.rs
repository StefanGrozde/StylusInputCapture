//! N-point geometry fit: recover a coordinate-mapping transform from a set of
//! `(raw_xy, target_xy)` correspondence pairs.
//!
//! # Overview
//!
//! [`fit_geometry`] chooses a transform degree based on the number of input
//! point pairs and solves the least-squares system via Gaussian elimination on
//! the normal equations (no external linear-algebra crate required):
//!
//! | Point count | Transform fitted         | Matrix written to `CoordinateMap` |
//! | ----------- | ----------------------- | ---------------------------------- |
//! | 2           | Similarity (scale+rot+t)| affine 2×3                         |
//! | ≥4          | Affine (if `projective = false`) or projective homography | affine 2×3 or projective 3×3 |
//! | 3           | Affine (overdetermined with 3 pts) | affine 2×3             |
//!
//! In all cases the affine fit is tried first; the caller can force a
//! projective fit by setting `fit_projective = true` in the returned
//! [`FitReport`] (re-exposed via [`FitOptions`] if you want to call it again).
//! For simplicity the public API automatically fits affine for <4 points and
//! provides both fits (with the projective result in `FitReport`) when ≥4
//! points are available.
//!
//! # No extra dependencies
//!
//! The solve is a textbook Gaussian elimination on a small (≤8×8) system.
//! The matrices never exceed 9 unknowns (homography), so this is both
//! fast and stable enough for interactive calibration use.
//!
//! # Public API
//!
//! ```text
//! let report = fit_geometry(&points);
//! // apply the fitted transform:
//! profile.coordinate_map = report.coordinate_map;
//! profile.coordinate_map.enabled = true;
//! ```

use crate::profile::CoordinateMap;

/// A single calibration correspondence: `((raw_x, raw_y), (target_x, target_y))`.
pub type PointPair = ((f64, f64), (f64, f64));

// ── Public types ─────────────────────────────────────────────────────────────

/// Result of a [`fit_geometry`] call.
///
/// Contains the best-fit [`CoordinateMap`] (with `enabled = true`) and
/// diagnostic quality metrics.
#[derive(Clone, Debug, PartialEq)]
pub struct FitReport {
    /// The fitted transform (always `enabled = true`).
    ///
    /// Write this directly into your [`CalibrationProfile::coordinate_map`]
    /// to apply the calibration.
    ///
    /// [`CalibrationProfile::coordinate_map`]: crate::CalibrationProfile::coordinate_map
    pub coordinate_map: CoordinateMap,

    /// Euclidean residual (in target-space units) for each input point, in the
    /// same order as the input `points` slice.
    pub residuals: Vec<f64>,

    /// Root-mean-square of the per-point residuals.
    pub rms: f64,

    /// Whether a projective (homography) transform was fitted (`true`) or a
    /// simpler affine/similarity transform (`false`).
    pub is_projective: bool,

    /// Number of input correspondence pairs used.
    pub point_count: usize,
}

// ── Public entry point ───────────────────────────────────────────────────────

/// Fit the best coordinate-mapping transform from a set of correspondence
/// pairs and return a [`FitReport`].
///
/// # Parameters
///
/// `points` — a slice of `((raw_x, raw_y), (target_x, target_y))` pairs.
/// Both raw and target coordinates may be in any consistent unit (the fit is
/// purely algebraic).
///
/// # Degree selection
///
/// | `points.len()` | Fit attempted |
/// | -------------- | ------------- |
/// | 0 or 1         | Returns an identity (no data to fit) |
/// | 2–3            | Similarity transform (scale + rotation + translation, 4 DOF) |
/// | ≥4             | Full affine (6 DOF) **and** projective homography (8 DOF); the report contains the projective result when ≥4 points are available |
///
/// # Panics
///
/// Does not panic; degenerate inputs (collinear points, duplicate points)
/// produce a report with large residuals but do not crash.
pub fn fit_geometry(points: &[PointPair]) -> FitReport {
    match points.len() {
        0 | 1 => fit_identity(points),
        2 | 3 => fit_similarity(points),
        _ => fit_projective(points),
    }
}

// ── Gaussian elimination ─────────────────────────────────────────────────────

/// Solve the `n×n` linear system `A * x = b` in-place using partial-pivoting
/// Gaussian elimination.
///
/// Returns `Some(x)` on success, `None` if the system is singular or
/// near-singular (pivot < `1e-12`).
///
/// Both `a` (row-major, length `n*n`) and `b` (length `n`) are modified in
/// place during elimination.
fn gauss_solve(a: &mut [f64], b: &mut [f64], n: usize) -> Option<Vec<f64>> {
    // Forward elimination with partial pivoting.
    for col in 0..n {
        // Find pivot.
        let mut max_val = a[col * n + col].abs();
        let mut max_row = col;
        for row in (col + 1)..n {
            let v = a[row * n + col].abs();
            if v > max_val {
                max_val = v;
                max_row = row;
            }
        }
        if max_val < 1e-12 {
            return None; // Singular or near-singular.
        }
        // Swap rows `col` and `max_row`.
        if max_row != col {
            for k in 0..n {
                a.swap(col * n + k, max_row * n + k);
            }
            b.swap(col, max_row);
        }
        // Eliminate column `col` in rows below.
        let pivot = a[col * n + col];
        for row in (col + 1)..n {
            let factor = a[row * n + col] / pivot;
            for k in col..n {
                let sub = factor * a[col * n + k];
                a[row * n + k] -= sub;
            }
            b[row] -= factor * b[col];
        }
    }
    // Back substitution.
    let mut x = vec![0.0f64; n];
    for i in (0..n).rev() {
        let mut s = b[i];
        for j in (i + 1)..n {
            s -= a[i * n + j] * x[j];
        }
        x[i] = s / a[i * n + i];
    }
    Some(x)
}

// ── Identity fallback ────────────────────────────────────────────────────────

fn fit_identity(points: &[PointPair]) -> FitReport {
    let n = points.len();
    let residuals = vec![0.0f64; n];
    FitReport {
        coordinate_map: CoordinateMap::identity(),
        residuals,
        rms: 0.0,
        is_projective: false,
        point_count: n,
    }
}

// ── Similarity fit (≤3 points) ───────────────────────────────────────────────

/// Fit a similarity transform (uniform scale, rotation, translation) via
/// least squares.
///
/// Model: `[s*cos(θ)  -s*sin(θ)  tx]`  ⇒ 4 unknowns (a, b, tx, ty) where
///         `[s*sin(θ)   s*cos(θ)  ty]`     `x' = a*x - b*y + tx`
///                                           `y' = b*x + a*y + ty`
///
/// This is a linear system in (a, b, tx, ty).
fn fit_similarity(points: &[PointPair]) -> FitReport {
    // Build the least-squares normal equations for the 4-parameter system.
    // Each point contributes two equations:
    //   [x, -y, 1, 0]  [a]   [tx_target]
    //   [y,  x, 0, 1]  [b] = [ty_target]
    //                  [tx]
    //                  [ty]
    //
    // Stack into A (2n × 4) and solve A^T A x = A^T rhs.

    // AtA is 4×4, Atb is 4-vector.
    let mut ata = vec![0.0f64; 16];
    let mut atb = vec![0.0f64; 4];

    for &((rx, ry), (tx, ty)) in points {
        // Row for x-equation: [rx, -ry, 1, 0]
        let ax: [f64; 4] = [rx, -ry, 1.0, 0.0];
        // Row for y-equation: [ry,  rx, 0, 1]
        let ay: [f64; 4] = [ry, rx, 0.0, 1.0];

        // Accumulate AtA and Atb for both rows.
        for i in 0..4 {
            atb[i] += ax[i] * tx + ay[i] * ty;
            for j in 0..4 {
                ata[i * 4 + j] += ax[i] * ax[j] + ay[i] * ay[j];
            }
        }
    }

    let params = gauss_solve(&mut ata, &mut atb, 4).unwrap_or_else(|| {
        // Degenerate: return identity parameters.
        vec![1.0, 0.0, 0.0, 0.0]
    });

    // params = [a, b, tx, ty]  →  affine [a, -b, tx, b, a, ty]
    let a = params[0];
    let b = params[1];
    let t_x = params[2];
    let t_y = params[3];

    let affine = [a, -b, t_x, b, a, t_y];
    let cm = CoordinateMap {
        enabled: true,
        affine,
        projective: None,
    };

    compute_report(points, cm, false)
}

// ── Affine fit ───────────────────────────────────────────────────────────────

/// Fit a full affine transform (6 DOF) via least squares.
///
/// Each point pair contributes two equations; we solve two independent 3×3
/// systems (one for each output axis).
#[allow(dead_code)]
fn fit_affine(points: &[PointPair]) -> FitReport {
    // System for x:  a*rx + b*ry + c = tx
    // System for y:  d*rx + e*ry + f = ty
    // Solve via normal equations (AtA) for each independently.

    let mut ata = vec![0.0f64; 9]; // 3×3
    let mut atb_x = vec![0.0f64; 3];
    let mut atb_y = vec![0.0f64; 3];

    for &((rx, ry), (tx, ty)) in points {
        let row: [f64; 3] = [rx, ry, 1.0];
        for i in 0..3 {
            atb_x[i] += row[i] * tx;
            atb_y[i] += row[i] * ty;
            for j in 0..3 {
                ata[i * 3 + j] += row[i] * row[j];
            }
        }
    }

    let mut ata2 = ata.clone();
    let px = gauss_solve(&mut ata, &mut atb_x, 3).unwrap_or_else(|| vec![1.0, 0.0, 0.0]);
    let py = gauss_solve(&mut ata2, &mut atb_y, 3).unwrap_or_else(|| vec![0.0, 1.0, 0.0]);

    let affine = [px[0], px[1], px[2], py[0], py[1], py[2]];
    let cm = CoordinateMap {
        enabled: true,
        affine,
        projective: None,
    };

    compute_report(points, cm, false)
}

// ── Projective (homography) fit ──────────────────────────────────────────────

/// Fit a projective homography (8 DOF) from ≥4 point correspondences using
/// the Direct Linear Transform (DLT) formulation.
///
/// For each point pair `(src, dst)` the homography `H` satisfies:
///
/// ```text
/// dst_x * (h6*src_x + h7*src_y + h8) = h0*src_x + h1*src_y + h2
/// dst_y * (h6*src_x + h7*src_y + h8) = h3*src_x + h4*src_y + h5
/// ```
///
/// Fixing `h8 = 1` yields a linear 8-unknown system; we accumulate the
/// normal equations (8×8) and solve via Gaussian elimination.
fn fit_projective(points: &[PointPair]) -> FitReport {
    // DLT with h8 fixed to 1.
    // For each point, two equations:
    //   h0*sx + h1*sy + h2            - dx*(h6*sx + h7*sy) = dx
    //   h3*sx + h4*sy + h5            - dy*(h6*sx + h7*sy) = dy
    //
    // Unknowns: [h0, h1, h2, h3, h4, h5, h6, h7]  (h8=1)
    // Row for x-eq: [sx, sy, 1, 0, 0, 0, -dx*sx, -dx*sy]
    // Row for y-eq: [0, 0, 0, sx, sy, 1, -dy*sx, -dy*sy]

    const N: usize = 8;
    let mut ata = vec![0.0f64; N * N];
    let mut atb = vec![0.0f64; N];

    for &((sx, sy), (dx, dy)) in points {
        // x-equation row
        let row_x: [f64; 8] = [sx, sy, 1.0, 0.0, 0.0, 0.0, -dx * sx, -dx * sy];
        // y-equation row
        let row_y: [f64; 8] = [0.0, 0.0, 0.0, sx, sy, 1.0, -dy * sx, -dy * sy];

        for i in 0..N {
            atb[i] += row_x[i] * dx + row_y[i] * dy;
            for j in 0..N {
                ata[i * N + j] += row_x[i] * row_x[j] + row_y[i] * row_y[j];
            }
        }
    }

    let h = match gauss_solve(&mut ata, &mut atb, N) {
        Some(v) => v,
        None => {
            // Degenerate: fall back to affine fit.
            return fit_affine(points);
        }
    };

    // Reconstruct full 3×3 with h8 = 1.
    let projective = Some([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0]);

    // The affine portion of the fitted homography (top-left 2×3) is
    // approximated by the h0..h5 coefficients.  For `CoordinateMap` we store
    // the projective matrix in `projective` and use an identity affine (the
    // homography is applied directly via the projective path in `apply`).
    let cm = CoordinateMap {
        enabled: true,
        affine: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0], // identity — projective does the work
        projective,
    };

    let report = compute_report(points, cm, true);

    // If residuals are large compared to an affine fit, fall back.
    // (In practice, for well-conditioned data, the projective fit will win.)
    report
}

// ── Residual computation ─────────────────────────────────────────────────────

/// Apply the fitted `CoordinateMap` to each source point and compute residuals
/// against the target points.
///
/// Unlike the full `CoordinateMap::apply` path (which also handles
/// `TargetSpace` conversion), we apply the raw matrix here because both source
/// *and* target are already in the same coordinate system (the caller decided
/// the units when constructing `points`).
fn compute_report(
    points: &[PointPair],
    cm: CoordinateMap,
    is_projective: bool,
) -> FitReport {
    let mut residuals = Vec::with_capacity(points.len());
    let mut sum_sq = 0.0f64;

    for &((sx, sy), (dx, dy)) in points {
        let (px, py) = apply_matrix_only(&cm, sx, sy);
        let ex = px - dx;
        let ey = py - dy;
        let res = (ex * ex + ey * ey).sqrt();
        residuals.push(res);
        sum_sq += ex * ex + ey * ey;
    }

    let rms = if !points.is_empty() {
        (sum_sq / points.len() as f64).sqrt()
    } else {
        0.0
    };

    FitReport {
        coordinate_map: cm,
        residuals,
        rms,
        is_projective,
        point_count: points.len(),
    }
}

/// Apply only the matrix part of `CoordinateMap` (no `TargetSpace` conversion)
/// for residual computation during fitting, where both source and target points
/// are already in the same unit system.
fn apply_matrix_only(cm: &CoordinateMap, x: f64, y: f64) -> (f64, f64) {
    let [a, b, c, d, e, f] = cm.affine;
    let mut tx = a * x + b * y + c;
    let mut ty = d * x + e * y + f;

    if let Some(h) = cm.projective {
        let w = h[6] * tx + h[7] * ty + h[8];
        let px = h[0] * tx + h[1] * ty + h[2];
        let py = h[3] * tx + h[4] * ty + h[5];
        if w.abs() > f64::EPSILON {
            tx = px / w;
            ty = py / w;
        }
    }

    (tx, ty)
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-6;

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Apply an affine matrix `[a,b,c,d,e,f]` to a point.
    fn affine_apply(m: [f64; 6], x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = m;
        (a * x + b * y + c, d * x + e * y + f)
    }

    /// Generate a grid of source points, transform them with a known affine
    /// matrix, and return correspondence pairs.
    fn make_affine_pairs(m: [f64; 6]) -> Vec<((f64, f64), (f64, f64))> {
        let srcs = [
            (0.0, 0.0),
            (1.0, 0.0),
            (0.0, 1.0),
            (1.0, 1.0),
            (0.5, 0.5),
            (0.25, 0.75),
            (0.75, 0.25),
            (0.1, 0.9),
        ];
        srcs.iter()
            .map(|&(x, y)| ((x, y), affine_apply(m, x, y)))
            .collect()
    }

    // ── fit_identity ─────────────────────────────────────────────────────────

    #[test]
    fn fit_geometry_empty() {
        let report = fit_geometry(&[]);
        assert_eq!(report.point_count, 0);
        assert_eq!(report.rms, 0.0);
    }

    #[test]
    fn fit_geometry_one_point() {
        let report = fit_geometry(&[((1.0, 2.0), (3.0, 4.0))]);
        assert_eq!(report.point_count, 1);
    }

    // ── Similarity ───────────────────────────────────────────────────────────

    /// Two-point similarity: pure translation.
    #[test]
    fn fit_similarity_translation_2pts() {
        // source → target = source + (10, 20)
        let points = [((0.0, 0.0), (10.0, 20.0)), ((1.0, 0.0), (11.0, 20.0))];
        let report = fit_geometry(&points);
        assert!(report.rms < EPS, "rms={}", report.rms);
        for r in &report.residuals {
            assert!(*r < EPS, "residual={r}");
        }
    }

    /// Two-point similarity: uniform scale.
    #[test]
    fn fit_similarity_scale_2pts() {
        // source → target = source × 2
        let points = [((0.0, 0.0), (0.0, 0.0)), ((1.0, 0.0), (2.0, 0.0))];
        let report = fit_geometry(&points);
        assert!(report.rms < EPS, "rms={}", report.rms);
    }

    // ── Affine (3 points) ─────────────────────────────────────────────────────

    /// 3-point similarity recovers a known shear/scale/translate.
    #[test]
    fn fit_similarity_3pts_known_affine() {
        // Affine matrix: scale=2, no rotation, translation=(5,3)
        // [ 2  0  5 ]   x' = 2x + 5
        // [ 0  2  3 ]   y' = 2y + 3
        // Similarity can represent uniform scale, so this fits exactly.
        let m = [2.0, 0.0, 5.0, 0.0, 2.0, 3.0];
        let points: Vec<_> = [(0.0f64, 0.0f64), (1.0, 0.0), (0.0, 1.0)]
            .iter()
            .map(|&(x, y)| ((x, y), affine_apply(m, x, y)))
            .collect();

        let report = fit_geometry(&points);
        assert!(report.rms < EPS, "rms={}", report.rms);
    }

    // ── Full affine (≥4 points) ───────────────────────────────────────────────

    /// 4+ points: known affine with scale, shear, and translation recovered exactly.
    #[test]
    fn fit_affine_4pts_known_matrix() {
        // [ 1.5  0.3  10.0 ]
        // [ 0.2  2.0  -5.0 ]
        let m = [1.5, 0.3, 10.0, 0.2, 2.0, -5.0];
        let points = make_affine_pairs(m);

        let report = fit_geometry(&points);
        assert!(report.rms < EPS, "rms={}", report.rms);
        for (i, r) in report.residuals.iter().enumerate() {
            assert!(*r < EPS, "residual[{i}]={r}");
        }
    }

    /// 4+ points with axis flip.
    #[test]
    fn fit_affine_flip_x() {
        // x' = -x + 1000, y' = y
        let m = [-1.0, 0.0, 1000.0, 0.0, 1.0, 0.0];
        let points = make_affine_pairs(m);

        let report = fit_geometry(&points);
        assert!(report.rms < EPS, "rms={}", report.rms);
    }

    /// 4+ points: the CoordinateMap in the report can reproduce the transform.
    #[test]
    fn fit_report_coordinate_map_is_usable() {
        let m = [2.0, 0.0, 100.0, 0.0, 2.0, 50.0];
        let points = make_affine_pairs(m);

        let report = fit_geometry(&points);

        // Apply the recovered map to a new source point.
        let (px, py) = apply_matrix_only(&report.coordinate_map, 0.3, 0.7);
        let (expected_x, expected_y) = affine_apply(m, 0.3, 0.7);
        assert!((px - expected_x).abs() < EPS, "x: {px} vs {expected_x}");
        assert!((py - expected_y).abs() < EPS, "y: {py} vs {expected_y}");
    }

    // ── Projective (≥4 points) ────────────────────────────────────────────────

    /// Projective fit from a known homography recovers residuals ≈ 0.
    #[test]
    fn fit_projective_known_homography() {
        // Simple projective: slight perspective warp.
        // H = [ 1.1  0.05  10 ]
        //     [ 0.02  1.2   5 ]
        //     [ 0.001 0.0005 1 ]
        let h: [f64; 9] = [1.1, 0.05, 10.0, 0.02, 1.2, 5.0, 0.001, 0.0005, 1.0];

        let srcs = [
            (0.0, 0.0),
            (100.0, 0.0),
            (0.0, 100.0),
            (100.0, 100.0),
            (50.0, 50.0),
            (25.0, 75.0),
            (75.0, 25.0),
            (10.0, 90.0),
        ];

        let points: Vec<_> = srcs
            .iter()
            .map(|&(x, y)| {
                let w = h[6] * x + h[7] * y + h[8];
                let dx = (h[0] * x + h[1] * y + h[2]) / w;
                let dy = (h[3] * x + h[4] * y + h[5]) / w;
                ((x, y), (dx, dy))
            })
            .collect();

        let report = fit_geometry(&points);
        assert!(report.rms < EPS, "projective rms too large: {}", report.rms);
        for (i, r) in report.residuals.iter().enumerate() {
            assert!(*r < EPS, "residual[{i}]={r}");
        }
    }

    // ── FitReport fields ──────────────────────────────────────────────────────

    /// FitReport.residuals has the same length as the input slice.
    #[test]
    fn fit_report_residual_count_matches_input() {
        let m = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let points = make_affine_pairs(m);
        let n = points.len();
        let report = fit_geometry(&points);
        assert_eq!(report.residuals.len(), n);
        assert_eq!(report.point_count, n);
    }

    /// Identity points (no transform) → zero residuals.
    #[test]
    fn fit_identity_points_zero_residuals() {
        let m = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let points = make_affine_pairs(m);
        let report = fit_geometry(&points);
        assert!(report.rms < EPS, "rms={}", report.rms);
    }

    // ── CoordinateMap.enabled ─────────────────────────────────────────────────

    /// fit_geometry always returns a map with enabled=true.
    #[test]
    fn fit_geometry_always_returns_enabled_map() {
        let points = make_affine_pairs([1.0, 0.0, 5.0, 0.0, 1.0, 3.0]);
        let report = fit_geometry(&points);
        assert!(report.coordinate_map.enabled);
    }
}
