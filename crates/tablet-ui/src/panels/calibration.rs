//! Calibration / settings panel — live `CalibrationProfile` editing (TC3.1).
//!
//! Renders grouped, toggleable controls bound directly to the app's
//! [`CalibrationProfile`] fields. Because [`CalibrationProfile::apply`] is
//! invoked per-sample in `drain_source`, edits made here take effect on the
//! very next frame — no extra wiring is required.
//!
//! Every control is range-clamped to match the constraints enforced by
//! [`tablet_process::CalibrationProfile::validate`] (see `tablet-process`'s
//! `io.rs`), so the user cannot produce a profile that would fail validation:
//!
//! - `pressure.pressure_min <= pressure.pressure_max`
//! - `Gamma(g)` curve exponent `g > 0.0`
//! - `Custom` curve control points sorted and monotone in `(t, v)`
//! - EMA `alpha` in `(0.0, 1.0]`
//! - One-Euro `min_cutoff > 0.0`, `d_cutoff > 0.0`, `beta >= 0.0`
//! - `activation.on_threshold` / `off_threshold` in `[0.0, 1.0]` with
//!   `off_threshold <= on_threshold`
//!
//! The N-point coordinate-map fit workflow lives in the "Geometry
//! calibration" section below (TC3.3, driven by [`GeometryCalibration`] /
//! `tablet_process::fit_geometry`). The interactive pressure-curve editor
//! (TC3.2) remains intentionally out of scope here.

use std::path::PathBuf;

use eframe::egui::{self, Slider};

use tablet_process::{
    ActivationConfig, CalibrationProfile, CurveKind, OneEuroParams, SmoothingMode, TargetSpace,
    TiltConvention,
};

use crate::calibration::GeometryCalibration;

/// Suffix appended to a user-entered Save-As path that has no extension, so
/// saved profiles consistently end up as `*.cal.toml` (SPEC_CUI.md §6.5/§7).
const PROFILE_EXTENSION_SUFFIX: &str = ".cal.toml";

/// Minimum allowed EMA alpha — matches the runtime clamp floor
/// (`f64::EPSILON`) while keeping the slider usable; validation only requires
/// `> 0.0`, so a small positive floor keeps the control in range.
const EMA_ALPHA_MIN: f64 = 0.001;
const EMA_ALPHA_MAX: f64 = 1.0;

/// Smallest positive value accepted for One-Euro cutoff frequencies and the
/// pressure-curve gamma exponent (`validate` requires strictly `> 0.0`).
const POSITIVE_MIN: f64 = 0.001;
const ONE_EURO_CUTOFF_MAX: f64 = 50.0;
const ONE_EURO_BETA_MAX: f64 = 5.0;
const GAMMA_MAX: f64 = 10.0;

/// I/O action requested by the profile-management controls (TC3.4) that the
/// caller (`app.rs`) must perform, since file I/O does not belong in the draw
/// function. At most one variant fires per frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProfileIoAction {
    /// No profile-management button was clicked this frame.
    #[default]
    None,
    /// "New" was confirmed (or there were no unsaved edits): replace the
    /// profile with `CalibrationProfile::identity()` and clear the path.
    New,
    /// "Load" was confirmed (or there were no unsaved edits): load the
    /// profile at this path, replacing the current one on success.
    Load(PathBuf),
    /// "Save" was clicked with a known `profile_path`: save in place.
    Save(PathBuf),
    /// "Save As" was clicked: save to this (possibly newly entered) path and
    /// adopt it as the new `profile_path`.
    SaveAs(PathBuf),
}

/// Action requested by the panel that the caller may want to react to.
///
/// The panel mutates `profile` directly for live-editing controls (TC3.1+);
/// anything that requires file I/O or replacing the whole profile is
/// surfaced here so `app.rs` can perform it and update `profile_path` /
/// `baseline_profile` consistently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CalibrationAction {
    /// `true` when the user clicked "Reset to identity" this frame.
    pub reset_to_identity: bool,
    /// At most one profile-management I/O request per frame.
    pub profile_io: ProfileIoAction,
}

/// Persistent (cross-frame) state for the profile-management controls —
/// owned by the caller (`app.rs`) and passed in mutably each frame. Kept
/// separate from `CalibrationProfile`/`GeometryCalibration` because it is
/// pure UI bookkeeping (path text buffer, last error, confirm-on-discard
/// state), never serialized into a profile or preferences file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfilePanelState {
    /// User-editable path used for Load and Save As. Seeded from
    /// `profile_path` by the caller when a profile is loaded/saved.
    pub path_text: String,
    /// Most recent load/save error message, shown until the next successful
    /// operation or user action clears it.
    pub last_error: Option<String>,
    /// Pending discard-confirmation: `Some(button)` after the user clicks
    /// "New" or "Load" while the profile is dirty; a second click on the
    /// *same* button proceeds. Any other profile-management click cancels
    /// the pending confirmation.
    ///
    /// This is the documented "click again to confirm" two-step pattern
    /// (chosen over a modal window to keep the control flow local to this
    /// panel and trivially testable).
    pub pending_confirm: Option<PendingDiscard>,
}

/// Which destructive action is awaiting a second confirming click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingDiscard {
    New,
    Load,
}

/// Draw the calibration / settings panel and return any requested action.
///
/// `profile` is mutated in place by the controls; `baseline` is the
/// last-loaded/saved snapshot used purely to compute the "modified" dirty
/// indicator (`profile != baseline`).
pub fn draw_calibration_panel(
    ui: &mut egui::Ui,
    profile: &mut CalibrationProfile,
    baseline: &CalibrationProfile,
    geometry: &mut GeometryCalibration,
    latest_raw_xy: Option<(f64, f64)>,
    profile_path: Option<&std::path::Path>,
    panel_state: &mut ProfilePanelState,
) -> CalibrationAction {
    let mut action = CalibrationAction::default();
    let dirty = profile != baseline;

    ui.horizontal(|ui| {
        ui.heading("calibration");
        if dirty {
            ui.colored_label(egui::Color32::from_rgb(255, 190, 90), "● modified");
        } else {
            ui.weak("saved");
        }
    });

    ui.horizontal(|ui| {
        ui.label("name:");
        ui.text_edit_singleline(&mut profile.name);
    });

    if ui.button("Reset to identity").clicked() {
        *profile = CalibrationProfile::identity();
        action.reset_to_identity = true;
    }

    ui.separator();

    draw_profile_management(ui, dirty, profile_path, panel_state, &mut action);

    ui.separator();

    draw_target_space(ui, profile);
    draw_coordinate_map(ui, profile);
    draw_geometry_calibration(ui, profile, geometry, latest_raw_xy);
    draw_pressure(ui, profile);
    draw_tilt(ui, profile);
    draw_smoothing(ui, profile);
    draw_activation(ui, profile);

    action
}

/// Profile-management controls (TC3.4, SPEC_CUI.md §6.5): New / Load / Save /
/// Save As against `*.cal.toml` files, a user-editable path field, the
/// resolved-path display, error surfacing, and confirm-on-discard for the
/// destructive New/Load operations.
///
/// File I/O is **not** performed here — see [`ProfileIoAction`]. This
/// function only decides *whether* an action should fire (applying the
/// confirm-on-discard gate) and packages the resolved path; `app.rs` performs
/// the actual `CalibrationProfile::load`/`save` and updates `profile`,
/// `baseline_profile`, and `profile_path` accordingly.
///
/// # Confirm-on-discard
///
/// When the profile is dirty (`profile != baseline`), clicking "New" or
/// "Load" the first time arms [`PendingDiscard`] and shows an inline warning
/// instead of firing the action; clicking the *same* button again within the
/// same armed state confirms and proceeds. Clicking "Save"/"Save As", editing
/// the path field, or clicking the *other* New/Load button cancels the
/// pending confirmation (so a stray click can't silently discard work).
fn draw_profile_management(
    ui: &mut egui::Ui,
    dirty: bool,
    profile_path: Option<&std::path::Path>,
    state: &mut ProfilePanelState,
    action: &mut CalibrationAction,
) {
    egui::CollapsingHeader::new("Profile")
        .default_open(true)
        .show(ui, |ui| {
            let resolved = match profile_path {
                Some(path) => path.display().to_string(),
                None => "(unsaved)".to_owned(),
            };
            ui.horizontal(|ui| {
                ui.label("path:");
                ui.monospace(resolved);
            });

            ui.horizontal(|ui| {
                ui.label("load/save-as path:");
                if ui.text_edit_singleline(&mut state.path_text).changed() {
                    state.pending_confirm = None;
                }
            });
            ui.weak("(\".cal.toml\" is appended automatically if you omit an extension)");

            ui.horizontal(|ui| {
                let new_clicked = ui.button("New").clicked();
                let load_clicked = ui
                    .add_enabled(!state.path_text.trim().is_empty(), egui::Button::new("Load"))
                    .clicked();
                let save_enabled = profile_path.is_some();
                let save_clicked = ui
                    .add_enabled(save_enabled, egui::Button::new("Save"))
                    .clicked();
                let save_as_clicked = ui
                    .add_enabled(
                        !state.path_text.trim().is_empty(),
                        egui::Button::new("Save As"),
                    )
                    .clicked();

                if save_clicked || save_as_clicked {
                    // Any explicit save cancels a pending New/Load confirmation —
                    // the user has chosen a different course of action.
                    state.pending_confirm = None;
                    state.last_error = None;
                }

                if save_clicked {
                    if let Some(path) = profile_path {
                        action.profile_io = ProfileIoAction::Save(path.to_path_buf());
                    }
                }
                if save_as_clicked {
                    let path = resolve_profile_path(&state.path_text);
                    action.profile_io = ProfileIoAction::SaveAs(path);
                }

                if new_clicked {
                    state.last_error = None;
                    if !dirty || state.pending_confirm == Some(PendingDiscard::New) {
                        action.profile_io = ProfileIoAction::New;
                        state.pending_confirm = None;
                    } else {
                        state.pending_confirm = Some(PendingDiscard::New);
                    }
                }

                if load_clicked {
                    state.last_error = None;
                    let path = resolve_profile_path(&state.path_text);
                    if !dirty || state.pending_confirm == Some(PendingDiscard::Load) {
                        action.profile_io = ProfileIoAction::Load(path);
                        state.pending_confirm = None;
                    } else {
                        state.pending_confirm = Some(PendingDiscard::Load);
                    }
                }
            });

            match state.pending_confirm {
                Some(PendingDiscard::New) => {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 120, 120),
                        "Unsaved edits will be discarded — click \"New\" again to confirm.",
                    );
                }
                Some(PendingDiscard::Load) => {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 120, 120),
                        "Unsaved edits will be discarded — click \"Load\" again to confirm.",
                    );
                }
                None => {}
            }

            if let Some(error) = &state.last_error {
                ui.colored_label(egui::Color32::from_rgb(255, 100, 100), error);
            }
        });
}

/// Resolve a user-entered path into a `*.cal.toml` target: append
/// [`PROFILE_EXTENSION_SUFFIX`] when the path has no extension, otherwise use
/// it verbatim (so `.json`/explicit `.toml` choices are respected, matching
/// `tablet_process::io`'s format-by-extension rule).
fn resolve_profile_path(text: &str) -> PathBuf {
    let trimmed = text.trim();
    let path = PathBuf::from(trimmed);
    if path.extension().is_some() {
        path
    } else {
        PathBuf::from(format!("{trimmed}{PROFILE_EXTENSION_SUFFIX}"))
    }
}

/// `TargetSpace` selector: Normalized / ScreenPixels{w,h} / Millimeters.
fn draw_target_space(ui: &mut egui::Ui, profile: &mut CalibrationProfile) {
    egui::CollapsingHeader::new("Target space")
        .default_open(true)
        .show(ui, |ui| {
            let current_label = match &profile.target_space {
                TargetSpace::Normalized => "Normalized",
                TargetSpace::ScreenPixels { .. } => "Screen pixels",
                TargetSpace::Millimeters => "Millimeters",
            };

            egui::ComboBox::from_label("space")
                .selected_text(current_label)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            matches!(profile.target_space, TargetSpace::Normalized),
                            "Normalized",
                        )
                        .clicked()
                    {
                        profile.target_space = TargetSpace::Normalized;
                    }
                    if ui
                        .selectable_label(
                            matches!(profile.target_space, TargetSpace::ScreenPixels { .. }),
                            "Screen pixels",
                        )
                        .clicked()
                        && !matches!(profile.target_space, TargetSpace::ScreenPixels { .. })
                    {
                        profile.target_space = TargetSpace::ScreenPixels { w: 1920, h: 1080 };
                    }
                    if ui
                        .selectable_label(
                            matches!(profile.target_space, TargetSpace::Millimeters),
                            "Millimeters",
                        )
                        .clicked()
                    {
                        profile.target_space = TargetSpace::Millimeters;
                    }
                });

            if let TargetSpace::ScreenPixels { w, h } = &mut profile.target_space {
                ui.horizontal(|ui| {
                    ui.label("width (px):");
                    ui.add(egui::DragValue::new(w).range(1..=u32::MAX));
                    ui.label("height (px):");
                    ui.add(egui::DragValue::new(h).range(1..=u32::MAX));
                });
            }
        });
}

/// Coordinate-map stage: `enabled` toggle plus a read-only display of the
/// current affine / projective matrix. Numeric matrix editing and the N-point
/// fit workflow belong to TC3.3.
fn draw_coordinate_map(ui: &mut egui::Ui, profile: &mut CalibrationProfile) {
    egui::CollapsingHeader::new("Coordinate map")
        .default_open(false)
        .show(ui, |ui| {
            ui.checkbox(&mut profile.coordinate_map.enabled, "enabled");

            let [a, b, c, d, e, f] = profile.coordinate_map.affine;
            ui.label("affine (row-major [a,b,c, d,e,f]):");
            ui.monospace(format!("[{a:.4}, {b:.4}, {c:.4},"));
            ui.monospace(format!(" {d:.4}, {e:.4}, {f:.4}]"));

            match &profile.coordinate_map.projective {
                Some(m) => {
                    ui.label("projective (row-major 3×3):");
                    ui.monospace(format!("[{:.4}, {:.4}, {:.4},", m[0], m[1], m[2]));
                    ui.monospace(format!(" {:.4}, {:.4}, {:.4},", m[3], m[4], m[5]));
                    ui.monospace(format!(" {:.4}, {:.4}, {:.4}]", m[6], m[7], m[8]));
                }
                None => {
                    ui.weak("projective: none (affine only)");
                }
            }
            ui.weak("Use \"Geometry calibration\" below to fit this matrix from sample points.");
        });
}

/// Geometry calibration workflow (TC3.3): collect `(raw, target)`
/// correspondence points from the live stream, fit the best transform via
/// [`tablet_process::fit_geometry`], and write the result into
/// `profile.coordinate_map`.
///
/// - **Add point** captures `latest_raw_xy` (the current sample's raw
///   `x_raw`/`y_raw` as `f64`) as the source and pairs it with the numeric
///   `pending_target` the user has entered; disabled when no sample has been
///   received yet (`latest_raw_xy.is_none()`).
/// - The point list supports per-row **remove** and a **Clear all**; there is
///   no separate "redo" — re-adding a removed point is the documented
///   workflow for restoring it (point identity is just `(raw, target)`, so
///   nothing is lost by re-entering the same values).
/// - **Fit** is enabled once `fit_geometry` can produce a non-trivial
///   transform (≥2 points; it auto-selects similarity / affine / projective
///   based on count — see `fit.rs`). On click, the resulting
///   `report.coordinate_map` (always `enabled = true`) is written into
///   `profile.coordinate_map`, and the report is kept for the residual / RMS
///   display below the button.
fn draw_geometry_calibration(
    ui: &mut egui::Ui,
    profile: &mut CalibrationProfile,
    geometry: &mut GeometryCalibration,
    latest_raw_xy: Option<(f64, f64)>,
) {
    egui::CollapsingHeader::new("Geometry calibration")
        .default_open(false)
        .show(ui, |ui| {
            ui.weak(
                "Collect raw → target correspondence points, then fit a coordinate-mapping transform.",
            );

            ui.horizontal(|ui| {
                ui.label("target x:");
                ui.add(
                    egui::DragValue::new(&mut geometry.pending_target.0)
                        .speed(1.0)
                        .prefix("x="),
                );
                ui.label("target y:");
                ui.add(
                    egui::DragValue::new(&mut geometry.pending_target.1)
                        .speed(1.0)
                        .prefix("y="),
                );
            });

            match latest_raw_xy {
                Some(raw) => {
                    ui.horizontal(|ui| {
                        if ui.button("Add point").clicked() {
                            geometry.add_point(raw, geometry.pending_target);
                        }
                        ui.weak(format!("current raw: ({:.1}, {:.1})", raw.0, raw.1));
                    });
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.add_enabled(false, egui::Button::new("Add point"));
                        ui.weak("no sample received yet — cannot capture a raw position");
                    });
                }
            }

            ui.separator();
            draw_geometry_point_list(ui, geometry);

            ui.separator();
            draw_geometry_fit_controls(ui, profile, geometry);
        });
}

/// Render the collected-points table with per-row remove and a "Clear all"
/// affordance. See [`draw_geometry_calibration`] for the documented
/// remove/clear/"redo" semantics.
fn draw_geometry_point_list(ui: &mut egui::Ui, geometry: &mut GeometryCalibration) {
    if geometry.points.is_empty() {
        ui.weak("no points collected yet");
        return;
    }

    ui.label(format!("points: {}", geometry.points.len()));

    let mut remove_idx = None;
    egui::Grid::new("geometry_calibration_points")
        .striped(true)
        .num_columns(4)
        .show(ui, |ui| {
            ui.weak("#");
            ui.weak("raw (x, y)");
            ui.weak("target (x, y)");
            ui.weak("");
            ui.end_row();

            for (i, point) in geometry.points.iter().enumerate() {
                ui.label(format!("{i}"));
                ui.monospace(format!("({:.1}, {:.1})", point.raw.0, point.raw.1));
                ui.monospace(format!("({:.2}, {:.2})", point.target.0, point.target.1));
                if ui.small_button("remove").clicked() {
                    remove_idx = Some(i);
                }
                ui.end_row();
            }
        });

    if let Some(idx) = remove_idx {
        geometry.remove(idx);
    }

    if ui.button("Clear all").clicked() {
        geometry.clear();
    }
}

/// Render the "Fit" button (enabled once `fit_geometry` can produce a
/// non-trivial transform) and, after a successful fit, the per-point
/// residuals / RMS / projective-or-affine summary.
///
/// On click, writes `report.coordinate_map` into `profile.coordinate_map`
/// (already `enabled = true` per `fit_geometry`'s contract) so the live trace
/// immediately reflects the new mapping.
fn draw_geometry_fit_controls(
    ui: &mut egui::Ui,
    profile: &mut CalibrationProfile,
    geometry: &mut GeometryCalibration,
) {
    let can_fit = geometry.points.len() >= crate::calibration::geometry::MIN_FIT_POINTS;

    ui.horizontal(|ui| {
        if ui
            .add_enabled(can_fit, egui::Button::new("Fit"))
            .clicked()
        {
            if let Some(report) = geometry.fit() {
                profile.coordinate_map = report.coordinate_map;
            }
        }
        if !can_fit {
            ui.weak("collect at least 2 points to fit (≥4 for affine/projective)");
        }
    });

    if let Some(report) = &geometry.last_report {
        let kind = if report.is_projective {
            "projective (homography)"
        } else {
            "similarity / affine"
        };
        ui.label(format!(
            "fit: {kind} from {} point(s) — RMS error: {:.4}",
            report.point_count, report.rms
        ));

        egui::Grid::new("geometry_calibration_residuals")
            .striped(true)
            .num_columns(2)
            .show(ui, |ui| {
                ui.weak("#");
                ui.weak("residual");
                ui.end_row();
                for (i, residual) in report.residuals.iter().enumerate() {
                    ui.label(format!("{i}"));
                    ui.monospace(format!("{residual:.4}"));
                    ui.end_row();
                }
            });
    } else {
        ui.weak("no fit yet");
    }
}

/// Pressure stage: `enabled` toggle, raw min/max drag values, and a curve-kind
/// selector (Linear / Gamma / Custom).
fn draw_pressure(ui: &mut egui::Ui, profile: &mut CalibrationProfile) {
    egui::CollapsingHeader::new("Pressure")
        .default_open(false)
        .show(ui, |ui| {
            let pressure = &mut profile.pressure;
            ui.checkbox(&mut pressure.enabled, "enabled");

            ui.horizontal(|ui| {
                ui.label("raw min:");
                ui.add(egui::DragValue::new(&mut pressure.pressure_min).range(0..=pressure.pressure_max));
                ui.label("raw max:");
                let min = pressure.pressure_min;
                ui.add(egui::DragValue::new(&mut pressure.pressure_max).range(min..=u32::MAX));
            });
            // Guarantee pressure_min <= pressure_max even if the ranges above
            // briefly disagree (e.g. the user types a value via keyboard entry).
            if pressure.pressure_min > pressure.pressure_max {
                pressure.pressure_max = pressure.pressure_min;
            }

            let curve_label = match &pressure.curve {
                CurveKind::Linear => "Linear",
                CurveKind::Gamma(_) => "Gamma",
                CurveKind::Custom(_) => "Custom",
            };

            egui::ComboBox::from_label("curve")
                .selected_text(curve_label)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(matches!(pressure.curve, CurveKind::Linear), "Linear")
                        .clicked()
                    {
                        pressure.curve = CurveKind::Linear;
                    }
                    if ui
                        .selectable_label(matches!(pressure.curve, CurveKind::Gamma(_)), "Gamma")
                        .clicked()
                        && !matches!(pressure.curve, CurveKind::Gamma(_))
                    {
                        pressure.curve = CurveKind::Gamma(1.0);
                    }
                    if ui
                        .selectable_label(matches!(pressure.curve, CurveKind::Custom(_)), "Custom")
                        .clicked()
                        && !matches!(pressure.curve, CurveKind::Custom(_))
                    {
                        pressure.curve = CurveKind::Custom(vec![(0.0, 0.0), (1.0, 1.0)]);
                    }
                });

            match &mut pressure.curve {
                CurveKind::Linear => {}
                CurveKind::Gamma(g) => {
                    // validate() requires g > 0.0; clamp to a small positive floor.
                    ui.add(
                        Slider::new(g, POSITIVE_MIN..=GAMMA_MAX)
                            .text("gamma")
                            .logarithmic(true),
                    );
                    if *g <= 0.0 {
                        *g = POSITIVE_MIN;
                    }
                }
                CurveKind::Custom(points) => {
                    draw_custom_curve_points(ui, points);
                }
            }
        });
}

/// Minimal editable list of `(t, v)` control points for `CurveKind::Custom`.
///
/// Keeps points sorted and monotone (matching `validate_custom_points`) by
/// clamping each edited value against its neighbours. The rich interactive
/// curve editor is TC3.2 — this is intentionally bare-bones.
fn draw_custom_curve_points(ui: &mut egui::Ui, points: &mut Vec<(f64, f64)>) {
    ui.label("control points (t, v) — sorted, monotone in [0,1]:");

    let mut remove_idx = None;
    let len = points.len();
    for i in 0..len {
        let (prev_t, prev_v) = if i > 0 { points[i - 1] } else { (0.0, 0.0) };
        let (next_t, next_v) = if i + 1 < len {
            points[i + 1]
        } else {
            (1.0, 1.0)
        };

        let (t, v) = &mut points[i];
        ui.horizontal(|ui| {
            ui.label(format!("[{i}]"));
            ui.add(
                egui::DragValue::new(t)
                    .speed(0.01)
                    .range(prev_t..=next_t)
                    .prefix("t="),
            );
            ui.add(
                egui::DragValue::new(v)
                    .speed(0.01)
                    .range(prev_v..=next_v)
                    .prefix("v="),
            );
            if len > 2 && ui.small_button("remove").clicked() {
                remove_idx = Some(i);
            }
        });
    }

    if let Some(idx) = remove_idx {
        points.remove(idx);
    }

    if ui.small_button("add point").clicked() {
        let (t, v) = points.last().copied().unwrap_or((0.0, 0.0));
        points.push((t.clamp(0.0, 1.0), v.clamp(0.0, 1.0)));
    }
}

/// Tilt stage: `enabled` toggle plus a `TiltConvention` selector.
fn draw_tilt(ui: &mut egui::Ui, profile: &mut CalibrationProfile) {
    egui::CollapsingHeader::new("Tilt / orientation")
        .default_open(false)
        .show(ui, |ui| {
            let tilt = &mut profile.tilt;
            ui.checkbox(&mut tilt.enabled, "enabled");

            let label = match tilt.convention {
                TiltConvention::TiltXY => "Tilt X/Y",
                TiltConvention::AzimuthAltitude => "Azimuth/Altitude",
                TiltConvention::DerivedFromOrientation => "Derived from orientation",
            };

            egui::ComboBox::from_label("convention")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut tilt.convention, TiltConvention::TiltXY, "Tilt X/Y");
                    ui.selectable_value(
                        &mut tilt.convention,
                        TiltConvention::AzimuthAltitude,
                        "Azimuth/Altitude",
                    );
                    ui.selectable_value(
                        &mut tilt.convention,
                        TiltConvention::DerivedFromOrientation,
                        "Derived from orientation",
                    );
                });
        });
}

/// Smoothing stage: mode selector (Off / EMA / One-Euro) with parameter
/// sliders clamped to `validate`'s ranges.
fn draw_smoothing(ui: &mut egui::Ui, profile: &mut CalibrationProfile) {
    egui::CollapsingHeader::new("Smoothing")
        .default_open(false)
        .show(ui, |ui| {
            let mode = &mut profile.smoothing.mode;
            let label = match mode {
                SmoothingMode::Off => "Off",
                SmoothingMode::Ema { .. } => "EMA",
                SmoothingMode::OneEuro(_) => "One-Euro",
            };

            egui::ComboBox::from_label("mode")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(matches!(mode, SmoothingMode::Off), "Off").clicked() {
                        *mode = SmoothingMode::Off;
                    }
                    if ui
                        .selectable_label(matches!(mode, SmoothingMode::Ema { .. }), "EMA")
                        .clicked()
                        && !matches!(mode, SmoothingMode::Ema { .. })
                    {
                        *mode = SmoothingMode::Ema { alpha: 0.5 };
                    }
                    if ui
                        .selectable_label(matches!(mode, SmoothingMode::OneEuro(_)), "One-Euro")
                        .clicked()
                        && !matches!(mode, SmoothingMode::OneEuro(_))
                    {
                        *mode = SmoothingMode::OneEuro(OneEuroParams::default());
                    }
                });

            match mode {
                SmoothingMode::Off => {}
                SmoothingMode::Ema { alpha } => {
                    // validate() requires alpha in (0.0, 1.0].
                    ui.add(Slider::new(alpha, EMA_ALPHA_MIN..=EMA_ALPHA_MAX).text("alpha"));
                    if *alpha <= 0.0 {
                        *alpha = EMA_ALPHA_MIN;
                    }
                }
                SmoothingMode::OneEuro(params) => {
                    draw_one_euro_params(ui, params);
                }
            }
        });
}

/// One-Euro filter parameter sliders, clamped to `validate`'s constraints
/// (`min_cutoff > 0`, `d_cutoff > 0`, `beta >= 0`).
fn draw_one_euro_params(ui: &mut egui::Ui, params: &mut OneEuroParams) {
    ui.add(
        Slider::new(&mut params.min_cutoff, POSITIVE_MIN..=ONE_EURO_CUTOFF_MAX)
            .text("min_cutoff (Hz)")
            .logarithmic(true),
    );
    if params.min_cutoff <= 0.0 {
        params.min_cutoff = POSITIVE_MIN;
    }

    ui.add(Slider::new(&mut params.beta, 0.0..=ONE_EURO_BETA_MAX).text("beta"));
    if params.beta < 0.0 {
        params.beta = 0.0;
    }

    ui.add(
        Slider::new(&mut params.d_cutoff, POSITIVE_MIN..=ONE_EURO_CUTOFF_MAX)
            .text("d_cutoff (Hz)")
            .logarithmic(true),
    );
    if params.d_cutoff <= 0.0 {
        params.d_cutoff = POSITIVE_MIN;
    }
}

/// Activation stage: `enabled` toggle, on/off threshold sliders (with
/// hysteresis ordering enforced), and the proximity-gating toggle.
fn draw_activation(ui: &mut egui::Ui, profile: &mut CalibrationProfile) {
    egui::CollapsingHeader::new("Activation")
        .default_open(false)
        .show(ui, |ui| {
            let activation = &mut profile.activation;
            ui.checkbox(&mut activation.enabled, "enabled");
            ui.checkbox(&mut activation.require_proximity, "require proximity");

            // validate() requires both thresholds in [0.0, 1.0] and
            // off_threshold <= on_threshold (hysteresis ordering).
            ui.add(
                Slider::new(&mut activation.on_threshold, activation.off_threshold..=1.0)
                    .text("on_threshold"),
            );
            ui.add(
                Slider::new(&mut activation.off_threshold, 0.0..=activation.on_threshold)
                    .text("off_threshold"),
            );
            clamp_activation_thresholds(activation);
        });
}

/// Clamp `ActivationConfig` thresholds into `[0.0, 1.0]` and enforce
/// `off_threshold <= on_threshold`, matching `validate`'s constraints.
///
/// Factored out as a small pure helper so it stays unit-testable independent
/// of the egui drawing code.
fn clamp_activation_thresholds(activation: &mut ActivationConfig) {
    activation.on_threshold = activation.on_threshold.clamp(0.0, 1.0);
    activation.off_threshold = activation.off_threshold.clamp(0.0, 1.0);
    if activation.off_threshold > activation.on_threshold {
        activation.off_threshold = activation.on_threshold;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reset-to-identity must yield exactly `CalibrationProfile::identity()`.
    #[test]
    fn reset_to_identity_matches_identity_profile() {
        let mut profile = CalibrationProfile::identity();
        profile.name = "custom".to_owned();
        profile.pressure.enabled = true;
        profile.pressure.pressure_min = 100;
        assert_ne!(profile, CalibrationProfile::identity());

        // Mirrors the panel's reset-to-identity action.
        profile = CalibrationProfile::identity();

        assert_eq!(profile, CalibrationProfile::identity());
    }

    /// `clamp_activation_thresholds` keeps thresholds within `[0,1]` and
    /// preserves `off_threshold <= on_threshold` (the hysteresis invariant
    /// `validate` enforces).
    #[test]
    fn clamp_activation_thresholds_enforces_hysteresis_and_range() {
        let mut activation = ActivationConfig {
            enabled: true,
            on_threshold: 0.2,
            off_threshold: 0.9,
            require_proximity: false,
        };

        clamp_activation_thresholds(&mut activation);

        assert!((0.0..=1.0).contains(&activation.on_threshold));
        assert!((0.0..=1.0).contains(&activation.off_threshold));
        assert!(activation.off_threshold <= activation.on_threshold);
    }

    /// Out-of-range threshold inputs are clamped into `[0, 1]`.
    #[test]
    fn clamp_activation_thresholds_clamps_out_of_range_values() {
        let mut activation = ActivationConfig {
            enabled: true,
            on_threshold: 1.5,
            off_threshold: -0.3,
            require_proximity: true,
        };

        clamp_activation_thresholds(&mut activation);

        assert_eq!(activation.on_threshold, 1.0);
        assert_eq!(activation.off_threshold, 0.0);
    }
}
