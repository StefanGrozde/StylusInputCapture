//! Pressure panel — time strip, histogram, and an interactive response-curve
//! editor with a "learn min/max" workflow (TC2.4 + TC3.2).
//!
//! The TC2.4 time strip and histogram are retained; TC3.2 adds:
//!
//! - An **interactive curve editor** plotting the active
//!   [`PressureCurve`](tablet_process::PressureCurve)'s shaped response over
//!   normalized input `[0, 1]`, with controls appropriate to the active
//!   [`CurveKind`]:
//!   - `Linear` — identity line, no controls.
//!   - `Gamma(g)` — a slider that mutates `g` live (clamped to `(0, 10]`).
//!   - `Custom(points)` — draggable control-point markers; dragging mutates
//!     `Vec<(f64, f64)>` in place, keeping points sorted by `t` and monotone
//!     in `v`. Points can be added/removed.
//! - A **live input-pressure dot** overlaid on the curve at
//!   `(pressure_norm, shaped)` for the latest sample, so tuning is immediate.
//! - A **"learn min/max"** button that calls [`tablet_process::learn_min_max`]
//!   over the recent raw samples in [`TraceHistory`] and writes the observed
//!   range into `pressure_min` / `pressure_max`.
//!
//! ## Curve display mapping
//!
//! `PressureCurve::apply` clamps raw pressure to `[pressure_min, pressure_max]`
//! before normalizing and shaping — so naively feeding `(x * 65535.0) as u32`
//! across the full `u32` range would mostly hit the clamp boundaries and not
//! show the curve's *shape*. Instead, the editor samples `apply` over raw
//! values spanning the **active `[pressure_min, pressure_max]` window**
//! (`min + t * (max - min)`), so the displayed curve is exactly the shape the
//! pipeline currently produces for in-range input. The live dot uses the
//! latest sample's actual `pressure_raw → apply(...)` output as Y and its
//! `pressure_norm` (the raw normalized input, independent of the clamp window)
//! as X — i.e. precisely what the pipeline does this frame.
//!
//! Edits flow through the same live-apply path as TC3.1: mutating
//! `profile.pressure` here is suficient because `drain_source` already calls
//! `profile.apply(...)` per incoming sample.

use eframe::egui::{self, Color32, Stroke};
use egui_plot::{Bar, BarChart, Legend, Line, MarkerShape, Plot, PlotPoint, Points, PlotPoints};

use tablet_core::PenSample;
use tablet_process::{learn_min_max, CurveKind, PressureCurve};

use crate::panels::trace::TraceHistory;

const HISTOGRAM_BUCKETS: usize = 24;
const CURVE_SAMPLE_COUNT: usize = 129;

/// Smallest positive gamma the editor allows (matches `tablet-process`
/// validation, which requires strictly `> 0.0`).
const GAMMA_MIN: f64 = 0.001;
/// Largest gamma exposed by the slider — an arbitrary but generous ceiling;
/// `PressureCurve::apply` itself accepts any positive exponent.
const GAMMA_MAX: f64 = 10.0;

/// Hit-test radius (in screen pixels) for picking a control point to drag.
const DRAG_HIT_RADIUS: f32 = 12.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub min: f64,
    pub max: f64,
    pub counts: Vec<usize>,
}

pub fn pressure_histogram(values: &[f64], bucket_count: usize) -> Histogram {
    let bucket_count = bucket_count.max(1);
    let finite: Vec<_> = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect();
    if finite.is_empty() {
        return Histogram {
            min: 0.0,
            max: 1.0,
            counts: vec![0; bucket_count],
        };
    }

    let min = finite.iter().copied().fold(f64::INFINITY, f64::min);
    let max = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut counts = vec![0; bucket_count];

    if (max - min).abs() <= f64::EPSILON {
        counts[0] = finite.len();
        return Histogram { min, max, counts };
    }

    let width = (max - min) / bucket_count as f64;
    for value in finite {
        let bucket = ((value - min) / width).floor() as usize;
        let bucket = bucket.min(bucket_count - 1);
        counts[bucket] += 1;
    }

    Histogram { min, max, counts }
}

// ── Curve sampling ───────────────────────────────────────────────────────────

/// Sample a [`PressureCurve`]'s shaped response across normalized input
/// `t ∈ [0, 1]`, returning `count` evenly spaced `(t, shaped)` points.
///
/// Each sample's raw input is `pressure_min + t * (pressure_max - pressure_min)`
/// (rounded to the nearest `u32`), so the curve drawn is exactly the shape
/// `apply` produces for in-range raw input — see the module-level doc comment
/// for why this is the correct mapping (as opposed to sampling the full `u32`
/// range, which would mostly show clamp plateaus).
pub fn sample_curve(curve: &PressureCurve, count: usize) -> Vec<[f64; 2]> {
    let count = count.max(2);
    let lo = curve.pressure_min as f64;
    let hi = curve.pressure_max as f64;
    let span = hi - lo;
    (0..count)
        .map(|i| {
            let t = i as f64 / (count - 1) as f64;
            let raw = if span.abs() <= f64::EPSILON {
                curve.pressure_min
            } else {
                (lo + t * span).round().clamp(0.0, u32::MAX as f64) as u32
            };
            [t, curve.apply(raw)]
        })
        .collect()
}

// ── Custom control-point editing helpers (pure, tested) ──────────────────────

/// Clamp a dragged control point at `idx` so the list stays sorted by `t` and
/// monotone (non-decreasing) in `v`, with both axes in `[0, 1]`.
///
/// `proposed` is the point's desired new `(t, v)`. The returned point has:
/// - `t` clamped to `[0, 1]` and to `(prev.t, next.t)` (points cannot cross
///   their neighbours; the first/last point's `t` is also pinned to `0`/`1`
///   respectively to keep the curve fully defined over `[0, 1]`),
/// - `v` clamped to `[0, 1]` and to `[prev.v, next.v]` (monotone).
///
/// This is the pure core of the dragging interaction so it can be unit tested
/// without a display.
pub fn clamp_dragged_point(points: &[(f64, f64)], idx: usize, proposed: (f64, f64)) -> (f64, f64) {
    let len = points.len();
    debug_assert!(idx < len, "clamp_dragged_point: idx out of range");

    let (mut t, mut v) = proposed;
    t = t.clamp(0.0, 1.0);
    v = v.clamp(0.0, 1.0);

    // Endpoints are pinned on the t-axis so the curve stays defined over the
    // full [0, 1] input range.
    if idx == 0 {
        t = 0.0;
    } else if idx == len - 1 {
        t = 1.0;
    } else {
        let (prev_t, _) = points[idx - 1];
        let (next_t, _) = points[idx + 1];
        t = t.clamp(prev_t, next_t);
    }

    let prev_v = if idx > 0 { points[idx - 1].1 } else { 0.0 };
    let next_v = if idx + 1 < len { points[idx + 1].1 } else { 1.0 };
    v = v.clamp(prev_v.min(next_v), prev_v.max(next_v));

    (t, v)
}

/// Insert a new control point at the curve's midpoint, evaluated against the
/// current curve so the inserted point lies *on* the curve (keeping it
/// visually unchanged immediately after insertion). Returns the insertion
/// index.
fn insert_midpoint(points: &mut Vec<(f64, f64)>) -> usize {
    if points.len() < 2 {
        points.push((1.0, 1.0));
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        return points.len() - 1;
    }
    // Find the widest gap on the t-axis and split it.
    let mut widest = 0;
    let mut widest_span = -1.0;
    for i in 0..points.len() - 1 {
        let span = points[i + 1].0 - points[i].0;
        if span > widest_span {
            widest_span = span;
            widest = i;
        }
    }
    let (t0, v0) = points[widest];
    let (t1, v1) = points[widest + 1];
    let t = (t0 + t1) * 0.5;
    let v = (v0 + v1) * 0.5;
    points.insert(widest + 1, (t, v));
    widest + 1
}

// ── Drag state (kept in egui memory, scoped to this panel's plot id) ─────────

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct DragState {
    dragged_idx: Option<usize>,
}

// ── Panel ────────────────────────────────────────────────────────────────────

/// Draw the pressure panel.
///
/// `pressure` is the active profile's pressure stage (mutated live by the
/// curve editor and the "learn min/max" button — both flow through the same
/// per-sample `profile.apply(...)` path the app already runs, see TC3.1).
/// `history` supplies both the time-strip/histogram data and the raw-sample
/// source for "learn min/max" (`point.processed.raw` for each point — no new
/// ingestion path is added, per the task boundary).
pub fn draw_pressure_panel(ui: &mut egui::Ui, pressure: &mut PressureCurve, history: &TraceHistory) {
    ui.vertical(|ui| {
        ui.heading("pressure");

        if history.is_empty() {
            ui.label("waiting for samples");
            return;
        }

        draw_time_strip(ui, history);
        draw_histogram(ui, history);
        draw_curve_editor(ui, pressure, history);
        draw_learn_min_max(ui, pressure, history);
    });
}

fn draw_time_strip(ui: &mut egui::Ui, history: &TraceHistory) {
    let raw_points: Vec<[f64; 2]> = history
        .iter()
        .enumerate()
        .map(|(idx, point)| [idx as f64, point.processed.raw.pressure_norm])
        .collect();
    let shaped_points: Vec<[f64; 2]> = history
        .iter()
        .enumerate()
        .map(|(idx, point)| [idx as f64, point.processed.pressure])
        .collect();

    Plot::new("pressure_time_strip")
        .height(120.0)
        .include_y(0.0)
        .include_y(1.0)
        .legend(Legend::default())
        .show(ui, |plot_ui| {
            plot_ui.line(
                Line::new("raw", PlotPoints::from(raw_points))
                    .color(Color32::from_rgb(130, 180, 220))
                    .stroke(Stroke::new(1.5, Color32::from_rgb(130, 180, 220))),
            );
            plot_ui.line(
                Line::new("shaped", PlotPoints::from(shaped_points))
                    .color(Color32::from_rgb(255, 205, 95))
                    .stroke(Stroke::new(2.0, Color32::from_rgb(255, 205, 95))),
            );
        });
}

fn draw_histogram(ui: &mut egui::Ui, history: &TraceHistory) {
    let recent: Vec<_> = history
        .iter()
        .map(|point| point.processed.pressure)
        .collect();
    let histogram = pressure_histogram(&recent, HISTOGRAM_BUCKETS);
    let span = (histogram.max - histogram.min).abs();
    let bucket_width = if span <= f64::EPSILON {
        1.0
    } else {
        span / histogram.counts.len() as f64
    };
    let bars: Vec<_> = histogram
        .counts
        .iter()
        .enumerate()
        .map(|(idx, count)| {
            let center = if span <= f64::EPSILON {
                histogram.min
            } else {
                histogram.min + (idx as f64 + 0.5) * bucket_width
            };
            Bar::new(center, *count as f64)
                .width(bucket_width * 0.9)
                .name(format!("{center:.3}"))
        })
        .collect();

    ui.horizontal(|ui| {
        ui.label(format!("observed min: {:.3}", histogram.min));
        ui.separator();
        ui.label(format!("observed max: {:.3}", histogram.max));
    });

    Plot::new("pressure_histogram")
        .height(120.0)
        .include_x(0.0)
        .include_x(1.0)
        .include_y(0.0)
        .show(ui, |plot_ui| {
            plot_ui.bar_chart(
                BarChart::new("recent pressure", bars)
                    .width(bucket_width * 0.9)
                    .color(Color32::from_rgb(120, 210, 170)),
            );
        });
}

/// Draw the interactive response-curve editor: the curve itself, the live
/// input dot, and (for `Custom`) draggable control points; for `Gamma`, a
/// slider beneath the plot.
fn draw_curve_editor(ui: &mut egui::Ui, pressure: &mut PressureCurve, history: &TraceHistory) {
    ui.separator();
    ui.horizontal(|ui| {
        ui.label("response curve");
        let kind_label = match &pressure.curve {
            CurveKind::Linear => "Linear",
            CurveKind::Gamma(_) => "Gamma",
            CurveKind::Custom(_) => "Custom",
        };
        ui.weak(kind_label);
    });

    let curve_points = sample_curve(pressure, CURVE_SAMPLE_COUNT);

    // Live input dot: (pressure_norm, shaped) for the latest sample — exactly
    // what the pipeline produces this frame (see module doc).
    let live_dot = history.latest().map(|point| {
        let raw = &point.processed.raw;
        [raw.pressure_norm, pressure.apply(raw.pressure_raw)]
    });

    let plot_id = ui.id().with("pressure_curve_editor_plot");
    let drag_id = ui.id().with("pressure_curve_editor_drag");
    let mut drag_state = ui
        .ctx()
        .data(|data| data.get_temp::<DragState>(drag_id))
        .unwrap_or_default();

    let is_custom = matches!(pressure.curve, CurveKind::Custom(_));
    let mut new_points: Option<Vec<(f64, f64)>> = None;

    let plot = Plot::new(plot_id)
        .height(220.0)
        .view_aspect(1.4)
        .include_x(0.0)
        .include_x(1.0)
        .include_y(0.0)
        .include_y(1.0)
        .allow_drag(!is_custom)
        .allow_zoom(false)
        .allow_scroll(false)
        .legend(Legend::default());

    plot.show(ui, |plot_ui| {
        plot_ui.line(
            Line::new("identity", PlotPoints::from(vec![[0.0, 0.0], [1.0, 1.0]]))
                .color(Color32::from_rgba_unmultiplied(180, 180, 180, 70))
                .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(180, 180, 180, 70))),
        );
        plot_ui.line(
            Line::new("curve", PlotPoints::from(curve_points.clone()))
                .color(Color32::from_rgb(255, 205, 95))
                .stroke(Stroke::new(2.5, Color32::from_rgb(255, 205, 95))),
        );

        if let CurveKind::Custom(points) = &pressure.curve {
            let marker_points: Vec<[f64; 2]> = points.iter().map(|&(t, v)| [t, v]).collect();
            plot_ui.points(
                Points::new("control points", PlotPoints::from(marker_points))
                    .shape(MarkerShape::Circle)
                    .radius(5.0)
                    .filled(true)
                    .color(Color32::from_rgb(255, 130, 110))
                    .allow_hover(false),
            );

            new_points = handle_custom_drag(plot_ui, points, &mut drag_state, drag_id);
        }

        if let Some([x, y]) = live_dot {
            plot_ui.points(
                Points::new("input", PlotPoints::from(vec![[x, y]]))
                    .shape(MarkerShape::Diamond)
                    .radius(6.0)
                    .filled(true)
                    .color(Color32::from_rgb(120, 220, 255))
                    .allow_hover(false),
            );
        }
    });

    if let Some(points) = new_points {
        pressure.curve = CurveKind::Custom(points);
    }

    ui.ctx()
        .data_mut(|data| data.insert_temp(drag_id, drag_state));

    match &mut pressure.curve {
        CurveKind::Linear => {
            ui.weak("identity mapping — switch to Gamma or Custom in the calibration panel to edit");
        }
        CurveKind::Gamma(g) => {
            ui.horizontal(|ui| {
                ui.label("gamma");
                ui.push_id("pressure_panel_gamma_slider", |ui| {
                    ui.add(egui::Slider::new(g, GAMMA_MIN..=GAMMA_MAX).logarithmic(true));
                });
            });
            if *g <= 0.0 {
                *g = GAMMA_MIN;
            }
        }
        CurveKind::Custom(points) => {
            ui.horizontal(|ui| {
                ui.weak("drag points on the curve to edit; sorted & monotone is enforced");
                if ui
                    .small_button("add point")
                    .on_hover_text("insert a point at the curve's widest gap")
                    .clicked()
                {
                    insert_midpoint(points);
                }
                if points.len() > 2 && ui.small_button("remove last").clicked() {
                    // Never remove the pinned endpoints.
                    let removable = points.len() - 1;
                    points.remove(removable.saturating_sub(1).max(1).min(points.len() - 2));
                }
            });
        }
    }
}

/// Hit-test and drag custom control points directly in the plot.
///
/// Returns `Some(points)` with the updated, clamped point list when a drag
/// changed something this frame; `None` otherwise. Drag state (which point
/// index is currently grabbed) is tracked in `drag_state` across frames so a
/// drag remains stable even if the pointer momentarily leaves a point's hit
/// radius.
fn handle_custom_drag(
    plot_ui: &mut egui_plot::PlotUi,
    points: &[(f64, f64)],
    drag_state: &mut DragState,
    _drag_id: egui::Id,
) -> Option<Vec<(f64, f64)>> {
    let response = plot_ui.response();
    let pointer_plot = plot_ui.pointer_coordinate();

    if response.drag_started() {
        if let Some(PlotPoint { x, y }) = pointer_plot {
            drag_state.dragged_idx = nearest_point_within(plot_ui, points, x, y, DRAG_HIT_RADIUS);
        }
    }
    if response.drag_stopped() {
        drag_state.dragged_idx = None;
    }

    if !response.dragged() {
        return None;
    }
    let idx = drag_state.dragged_idx?;
    let PlotPoint { x, y } = pointer_plot?;
    let clamped = clamp_dragged_point(points, idx, (x, y));
    if clamped == points[idx] {
        return None;
    }
    let mut updated = points.to_vec();
    updated[idx] = clamped;
    Some(updated)
}

/// Find the index of the control point nearest to plot-space `(x, y)`, within
/// `radius_px` screen pixels (using the plot's current screen transform).
/// Returns `None` if no point is within range.
fn nearest_point_within(
    plot_ui: &egui_plot::PlotUi,
    points: &[(f64, f64)],
    x: f64,
    y: f64,
    radius_px: f32,
) -> Option<usize> {
    let target = plot_ui.screen_from_plot(PlotPoint::new(x, y));
    points
        .iter()
        .enumerate()
        .map(|(idx, &(pt, pv))| {
            let screen = plot_ui.screen_from_plot(PlotPoint::new(pt, pv));
            (idx, (screen - target).length())
        })
        .filter(|&(_, dist)| dist <= radius_px)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(idx, _)| idx)
}

/// "Learn min/max" button: calls [`learn_min_max`] over the raw samples held
/// in `history` and writes the observed range into `pressure.pressure_min` /
/// `pressure_max`.
///
/// **Empty-history behavior:** the button is disabled (a visible no-op) when
/// `history` is empty — there is nothing to learn from, and writing `(0, 0)`
/// would create a degenerate clamp window that zeroes all pressure output.
/// `draw_pressure_panel` already early-returns on an empty history, so in
/// practice this guard only matters if that changes; it is kept here so the
/// button is correct in isolation too.
fn draw_learn_min_max(ui: &mut egui::Ui, pressure: &mut PressureCurve, history: &TraceHistory) {
    ui.separator();
    ui.horizontal(|ui| {
        let enabled = !history.is_empty();
        let button = ui.add_enabled(enabled, egui::Button::new("learn min/max from recent samples"));
        if button.clicked() {
            let samples: Vec<PenSample> = history.iter().map(|point| point.processed.raw).collect();
            let (min, max) = learn_min_max(&samples);
            pressure.pressure_min = min;
            pressure.pressure_max = max.max(min);
        }
        ui.weak(format!(
            "clamp: [{}, {}]",
            pressure.pressure_min, pressure.pressure_max
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(min: u32, max: u32, kind: CurveKind) -> PressureCurve {
        PressureCurve {
            enabled: true,
            pressure_min: min,
            pressure_max: max,
            curve: kind,
        }
    }

    // ── histogram (TC2.4, retained) ──────────────────────────────────────────

    #[test]
    fn histogram_ignores_non_finite_values() {
        let histogram = pressure_histogram(&[0.0, f64::NAN, 1.0, f64::INFINITY], 2);

        assert_eq!(histogram.min, 0.0);
        assert_eq!(histogram.max, 1.0);
        assert_eq!(histogram.counts, vec![1, 1]);
    }

    #[test]
    fn histogram_places_upper_edge_in_last_bucket() {
        let histogram = pressure_histogram(&[0.0, 0.25, 0.5, 0.75, 1.0], 4);

        assert_eq!(histogram.counts, vec![1, 1, 1, 2]);
    }

    #[test]
    fn histogram_constant_values_use_first_bucket() {
        let histogram = pressure_histogram(&[0.4, 0.4, 0.4], 3);

        assert_eq!(histogram.min, 0.4);
        assert_eq!(histogram.max, 0.4);
        assert_eq!(histogram.counts, vec![3, 0, 0]);
    }

    // ── sample_curve ─────────────────────────────────────────────────────────

    #[test]
    fn sample_curve_spans_zero_to_one_on_x() {
        let c = curve(0, 1000, CurveKind::Linear);
        let points = sample_curve(&c, 9);
        assert_eq!(points.first().unwrap()[0], 0.0);
        assert_eq!(points.last().unwrap()[0], 1.0);
        assert_eq!(points.len(), 9);
    }

    #[test]
    fn sample_curve_linear_is_identity() {
        let c = curve(0, 1000, CurveKind::Linear);
        for p in sample_curve(&c, 11) {
            assert!((p[0] - p[1]).abs() < 1e-9, "{p:?}");
        }
    }

    #[test]
    fn sample_curve_gamma_is_monotonic() {
        let c = curve(0, 1000, CurveKind::Gamma(2.5));
        let points = sample_curve(&c, 64);
        let mut prev = points[0][1];
        for p in &points[1..] {
            assert!(p[1] >= prev - 1e-12, "not monotonic: prev={prev}, cur={p:?}");
            prev = p[1];
        }
    }

    #[test]
    fn sample_curve_gamma_endpoints_are_zero_and_one() {
        let c = curve(0, 1000, CurveKind::Gamma(3.0));
        let points = sample_curve(&c, 17);
        assert!((points.first().unwrap()[1] - 0.0).abs() < 1e-9);
        assert!((points.last().unwrap()[1] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn sample_curve_handles_degenerate_clamp_window() {
        let c = curve(500, 500, CurveKind::Linear);
        let points = sample_curve(&c, 5);
        assert_eq!(points.len(), 5);
        for p in &points {
            assert!(p[1].is_finite());
        }
    }

    // ── clamp_dragged_point ──────────────────────────────────────────────────

    #[test]
    fn clamp_dragged_point_keeps_t_order() {
        let points = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        // Try to drag the middle point past its right neighbour.
        let (t, v) = clamp_dragged_point(&points, 1, (1.5, 0.5));
        assert!(t <= points[2].0, "t={t} must not exceed next.t");
        assert!(t >= points[0].0, "t={t} must not precede prev.t");
        assert!((0.0..=1.0).contains(&v));
    }

    #[test]
    fn clamp_dragged_point_keeps_t_order_lower_bound() {
        let points = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        let (t, _v) = clamp_dragged_point(&points, 1, (-1.0, 0.5));
        assert!(t >= points[0].0);
        assert!(t <= points[2].0);
    }

    #[test]
    fn clamp_dragged_point_keeps_v_monotone() {
        let points = vec![(0.0, 0.2), (0.5, 0.5), (1.0, 0.8)];
        // Try to drag the middle point's v above the next point's v.
        let (_t, v) = clamp_dragged_point(&points, 1, (0.5, 5.0));
        assert!(v <= points[2].1.max(points[0].1));
        assert!((0.0..=1.0).contains(&v));
    }

    #[test]
    fn clamp_dragged_point_clamps_to_unit_square() {
        let points = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        let (t, v) = clamp_dragged_point(&points, 1, (-5.0, -5.0));
        assert!((0.0..=1.0).contains(&t));
        assert!((0.0..=1.0).contains(&v));
        let (t, v) = clamp_dragged_point(&points, 1, (5.0, 5.0));
        assert!((0.0..=1.0).contains(&t));
        assert!((0.0..=1.0).contains(&v));
    }

    #[test]
    fn clamp_dragged_point_pins_endpoints_on_t_axis() {
        let points = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        let (t_first, _) = clamp_dragged_point(&points, 0, (0.3, 0.1));
        assert_eq!(t_first, 0.0, "first point's t stays pinned at 0");
        let (t_last, _) = clamp_dragged_point(&points, 2, (0.7, 0.9));
        assert_eq!(t_last, 1.0, "last point's t stays pinned at 1");
    }

    #[test]
    fn clamp_dragged_point_output_always_in_unit_square() {
        let points = vec![(0.0, 0.0), (0.25, 0.4), (0.6, 0.6), (1.0, 1.0)];
        for idx in 0..points.len() {
            for &(dt, dv) in &[
                (-2.0, -2.0),
                (-2.0, 2.0),
                (2.0, -2.0),
                (2.0, 2.0),
                (0.0, 0.0),
            ] {
                let (t, v) = clamp_dragged_point(&points, idx, (points[idx].0 + dt, points[idx].1 + dv));
                assert!((0.0..=1.0).contains(&t), "t out of range at idx={idx}: {t}");
                assert!((0.0..=1.0).contains(&v), "v out of range at idx={idx}: {v}");
            }
        }
    }

    // ── insert_midpoint ──────────────────────────────────────────────────────

    #[test]
    fn insert_midpoint_keeps_points_sorted_and_in_range() {
        let mut points = vec![(0.0, 0.0), (1.0, 1.0)];
        insert_midpoint(&mut points);
        assert_eq!(points.len(), 3);
        for w in points.windows(2) {
            assert!(w[0].0 <= w[1].0, "{points:?}");
        }
        for &(t, v) in &points {
            assert!((0.0..=1.0).contains(&t));
            assert!((0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn insert_midpoint_splits_widest_gap() {
        let mut points = vec![(0.0, 0.0), (0.1, 0.1), (1.0, 1.0)];
        let idx = insert_midpoint(&mut points);
        // The widest gap is between (0.1, 0.1) and (1.0, 1.0) → midpoint t = 0.55.
        assert!((points[idx].0 - 0.55).abs() < 1e-9, "{points:?}");
    }
}
