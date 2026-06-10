//! The pen-guide modal: a one-time cheat sheet shown when a pen the user has
//! never used before comes into range.
//!
//! "Which pen is this?" is answered by a **capability fingerprint**
//! ([`PenSignature`]), not a model name — the stream carries none, and on the
//! Raw Input backend `PenSample.tool_serial` is always 0. The per-sample
//! `Option` axes (tilt, twist, tangent pressure, rotation) are populated
//! strictly from the device/tool profile on both backends, so their presence
//! plus the device name and cursor id is what distinguishes one pen from
//! another. Two pen models with identical feature sets are indistinguishable —
//! and would get an identical guide anyway.
//!
//! Detection ([`PenTracker`]) requires the same signature on several
//! consecutive samples before reporting a pen change, so a single glitched
//! packet can't flash the modal. Dismissed pens are remembered in
//! [`HudPrefs::seen_pens`](crate::prefs::HudPrefs) and never re-trigger;
//! `Action::ShowPenGuide` re-opens the guide for the current pen on demand.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use serde::{Deserialize, Serialize};
use tablet_core::PenSample;
use tablet_midi::{TiltAxis, VelocitySource};

use crate::app::HudApp;
use crate::bindings::{InputChord, PenButton};
use crate::theme;

/// Consecutive samples a signature must hold before it counts as the current
/// pen (~50 ms at typical report rates — long enough to swallow a glitched
/// packet, short enough to feel instant when swapping pens).
const STABLE_SAMPLES: u8 = 8;

/// A pen's capability fingerprint — the identity the guide is keyed on.
///
/// Persisted (as TOML, inside the prefs file) once the guide is dismissed.
/// Every field defaults so files written by other versions keep loading.
///
/// `ToolKind` is deliberately **not** part of the identity: on Raw Input the
/// kind flips to `Eraser` whenever the invert switch asserts, which would
/// re-introduce the same physical pen mid-stroke. (On Wintab the eraser end is
/// a different cursor index, so it still gets its own one-time guide through
/// `tool_serial` — correct, since it can carry different axes.)
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PenSignature {
    /// Tablet name from the capabilities handshake.
    #[serde(default)]
    pub device_name: String,
    /// Wintab cursor id (`PK_CURSOR`); always 0 on the Raw Input backend.
    #[serde(default)]
    pub tool_serial: u64,
    /// The pen reports tilt (directly or derived from azimuth/altitude).
    #[serde(default)]
    pub has_tilt: bool,
    /// The pen reports twist around its own axis.
    #[serde(default)]
    pub has_twist: bool,
    /// The pen has an airbrush wheel (tangent/barrel pressure).
    #[serde(default)]
    pub has_tangent_pressure: bool,
    /// The pen reports rotation (Wintab `PK_ROTATION`).
    #[serde(default)]
    pub has_rotation: bool,
}

impl PenSignature {
    /// Fingerprint one sample against the device it came from.
    pub fn of(sample: &PenSample, device_name: &str) -> Self {
        Self {
            device_name: device_name.to_owned(),
            tool_serial: sample.tool_serial,
            has_tilt: sample.tilt_x_deg.is_some() || sample.tilt_y_deg.is_some(),
            has_twist: sample.twist_deci_deg.is_some(),
            has_tangent_pressure: sample.tangent_pressure_raw.is_some(),
            has_rotation: sample.rotation_deci_deg.is_some(),
        }
    }

    /// Short "pressure · tilt · twist" feature summary for the modal subtitle.
    pub fn features_label(&self) -> String {
        let mut parts = vec!["pressure"];
        if self.has_tilt {
            parts.push("tilt");
        }
        if self.has_twist {
            parts.push("twist");
        }
        if self.has_tangent_pressure {
            parts.push("airbrush wheel");
        }
        if self.has_rotation {
            parts.push("rotation");
        }
        parts.join(" · ")
    }
}

/// Debounced pen-change detection. Pure state machine, no I/O: feed it every
/// sample's signature; it reports a signature once it has been stable for
/// [`STABLE_SAMPLES`] consecutive observations *and* differs from the pen it
/// last reported.
#[derive(Default)]
pub struct PenTracker {
    current: Option<PenSignature>,
    candidate: Option<(PenSignature, u8)>,
}

impl PenTracker {
    /// The pen currently in use (the last signature [`observe`](Self::observe)
    /// reported).
    pub fn current(&self) -> Option<&PenSignature> {
        self.current.as_ref()
    }

    /// Feed one sample's signature; returns the newly stable signature when
    /// the pen changed.
    pub fn observe(&mut self, sig: PenSignature) -> Option<PenSignature> {
        if self.current.as_ref() == Some(&sig) {
            // Same pen as before: a brief blip toward another signature is
            // forgotten as soon as the known pen reports again.
            self.candidate = None;
            return None;
        }
        match &mut self.candidate {
            Some((candidate, count)) if *candidate == sig => {
                *count += 1;
                if *count >= STABLE_SAMPLES {
                    self.candidate = None;
                    self.current = Some(sig.clone());
                    return Some(sig);
                }
            }
            _ => self.candidate = Some((sig, 1)),
        }
        None
    }
}

impl HudApp {
    /// Draw the pen-guide modal while [`HudApp::pen_guide`] holds a pen.
    ///
    /// **Got it** records the pen in `prefs.seen_pens` (persisted immediately)
    /// so it never re-triggers; closing any other way (click outside, Esc)
    /// dismisses for this run only.
    pub(crate) fn draw_pen_guide(&mut self, ctx: &egui::Context) {
        let Some(sig) = self.pen_guide.clone() else {
            return;
        };

        // Everything the closure needs, resolved up front so it borrows none
        // of `self` (the bullets must reflect the *live* mapping/keymap).
        let barrel = self
            .settings
            .keymap
            .resolve(&InputChord::Pen(PenButton::Barrel));
        let eraser = self
            .settings
            .keymap
            .resolve(&InputChord::Pen(PenButton::Eraser));
        let bullets = self.pen_guide_bullets(&sig);

        let mut got_it = false;
        let modal = egui::Modal::new(egui::Id::new("pen_guide")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading(format!("New pen detected — {}", sig.device_name));
            ui.weak(format!(
                "cursor {} · reports: {}",
                sig.tool_serial,
                sig.features_label()
            ));
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(170.0, 250.0), Sense::hover());
                draw_stylus(
                    ui.painter(),
                    rect,
                    &sig,
                    barrel.is_some(),
                    eraser.is_some(),
                );
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    for (text, weak) in &bullets {
                        if *weak {
                            ui.weak(format!("•  {text}"));
                        } else {
                            ui.label(format!("•  {text}"));
                        }
                        ui.add_space(2.0);
                    }
                });
            });

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Got it").strong()).clicked() {
                    got_it = true;
                }
                ui.weak("Shown once per pen — reopen any time via Settings → Pen guide.");
            });
        });

        if got_it {
            self.prefs.seen_pens.push(sig);
            if let Err(error) = self.prefs.save() {
                eprintln!("failed to save HUD prefs: {error}");
            }
            self.pen_guide = None;
        } else if modal.should_close() {
            // Dismissed without acknowledging: don't persist, so the pen is
            // introduced again next run.
            self.pen_guide = None;
        }
    }

    /// The guide's bullet lines for `sig` under the current mapping and
    /// keymap. `(text, weak)` — weak lines are hints, not active mappings.
    fn pen_guide_bullets(&self, sig: &PenSignature) -> Vec<(String, bool)> {
        let mut lines = Vec::new();

        let velocity = match self.mapping.velocity {
            VelocitySource::Fixed(v) => format!("velocity fixed at {v}"),
            VelocitySource::Pressure { min, max } => {
                format!("velocity from strike pressure ({min}–{max})")
            }
        };
        lines.push((format!("Tip — strike a pad to play; {velocity}"), false));

        if self.mapping.pressure_to_channel_pressure {
            lines.push(("Pressure while held → channel pressure".to_owned(), false));
        }
        if self.mapping.y_bend_semitones > 0.0 {
            lines.push((
                format!(
                    "Drag up/down → pitch bend (±{:.0} st)",
                    self.mapping.y_bend_semitones
                ),
                false,
            ));
        }
        if self.mapping.x_to_cc74 {
            lines.push(("Drag left/right → CC74 (timbre)".to_owned(), false));
        }

        match (&self.mapping.tilt_cc, sig.has_tilt) {
            (Some(tilt), true) => {
                let axis = match tilt.axis {
                    TiltAxis::X => "X",
                    TiltAxis::Y => "Y",
                };
                lines.push((format!("Tilt → CC{} (tilt {axis})", tilt.controller), false));
            }
            (None, true) => lines.push((
                "Tilt available — enable \"tilt → CC\" in the Expression panel".to_owned(),
                true,
            )),
            (_, false) => lines.push((
                "This pen does not report tilt — tilt mappings have no effect".to_owned(),
                true,
            )),
        }

        if sig.has_tangent_pressure {
            lines.push((
                "Airbrush wheel detected (tangent pressure) — captured, not yet mapped".to_owned(),
                true,
            ));
        }

        let binding_line = |name: &str, button: PenButton| {
            match self.settings.keymap.resolve(&InputChord::Pen(button)) {
                Some(action) => (format!("{name} → {}", action.label()), false),
                None => (format!("{name} → unbound — bind in Settings"), true),
            }
        };
        lines.push(binding_line("Barrel button", PenButton::Barrel));
        lines.push(binding_line("Eraser", PenButton::Eraser));

        lines
    }
}

/// Painter-drawn side view of a stylus with callouts for the tip, the barrel
/// button, and the eraser (no asset pipeline exists; all HUD graphics are
/// vector, like the pad grid). Unbound button callouts are dimmed; a tilt arc
/// over the body appears only when the pen reports tilt.
fn draw_stylus(
    painter: &egui::Painter,
    rect: Rect,
    sig: &PenSignature,
    barrel_bound: bool,
    eraser_bound: bool,
) {
    let cx = rect.left() + 52.0;
    let top = rect.top() + 14.0;
    let tip_y = rect.bottom() - 16.0;
    let label_x = cx + 36.0;
    let font = FontId::proportional(11.0);
    let dim = theme::STATUS_IDLE;

    let callout = |part: Pos2, text: &str, color: Color32| {
        painter.line_segment(
            [part, Pos2::new(label_x - 4.0, part.y)],
            Stroke::new(1.0, color.linear_multiply(0.6)),
        );
        painter.text(
            Pos2::new(label_x, part.y),
            Align2::LEFT_CENTER,
            text,
            font.clone(),
            color,
        );
    };

    // Eraser cap.
    let eraser_color = if eraser_bound { theme::ACCENT_DIM } else { dim };
    let cap = Rect::from_min_max(Pos2::new(cx - 9.0, top), Pos2::new(cx + 9.0, top + 24.0));
    painter.rect_filled(cap, 5.0, theme::PAD_ROOT);
    painter.rect_stroke(
        cap,
        5.0,
        Stroke::new(1.0, eraser_color),
        egui::StrokeKind::Inside,
    );
    callout(Pos2::new(cx + 9.0, cap.center().y), "eraser", eraser_color);

    // Body.
    let body = Rect::from_min_max(
        Pos2::new(cx - 11.0, cap.bottom()),
        Pos2::new(cx + 11.0, tip_y - 44.0),
    );
    painter.rect_filled(body, 4.0, theme::PAD);
    painter.rect_stroke(
        body,
        4.0,
        Stroke::new(1.0, theme::PAD_STROKE),
        egui::StrokeKind::Inside,
    );

    // Barrel (side) button on the body.
    let barrel_color = if barrel_bound { theme::ACCENT_DIM } else { dim };
    let button = Rect::from_min_max(
        Pos2::new(cx + 7.0, body.center().y - 16.0),
        Pos2::new(cx + 13.0, body.center().y + 16.0),
    );
    painter.rect_filled(button, 3.0, theme::BG_RAISED);
    painter.rect_stroke(
        button,
        3.0,
        Stroke::new(1.5, barrel_color),
        egui::StrokeKind::Inside,
    );
    callout(Pos2::new(cx + 13.0, button.center().y), "barrel", barrel_color);

    // Taper down to the tip, with an accent nib.
    let taper_top = body.bottom();
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(cx - 11.0, taper_top),
            Pos2::new(cx + 11.0, taper_top),
            Pos2::new(cx, tip_y),
        ],
        theme::PAD,
        Stroke::new(1.0, theme::PAD_STROKE),
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(cx - 3.5, tip_y - 12.0),
            Pos2::new(cx + 3.5, tip_y - 12.0),
            Pos2::new(cx, tip_y),
        ],
        theme::ACCENT,
        Stroke::NONE,
    ));
    callout(Pos2::new(cx + 4.0, tip_y - 6.0), "tip", theme::ACCENT_DIM);

    // Tilt arc pivoting at the tip, only when the pen reports tilt.
    if sig.has_tilt {
        let pivot = Pos2::new(cx, tip_y);
        let radius = tip_y - top + 6.0;
        let points: Vec<Pos2> = (-4..=4)
            .map(|i| {
                let angle = i as f32 * 0.055; // ±0.22 rad ≈ ±13°
                Pos2::new(
                    pivot.x + radius * angle.sin(),
                    pivot.y - radius * angle.cos(),
                )
            })
            .collect();
        painter.add(egui::Shape::line(
            points,
            Stroke::new(1.5, theme::ACCENT_DIM.linear_multiply(0.8)),
        ));
        painter.text(
            Pos2::new(cx, top - 12.0),
            Align2::CENTER_CENTER,
            "⟷ tilt",
            font,
            theme::ACCENT_DIM,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_core::ToolKind;

    /// A bare sample with every optional axis absent.
    fn sample() -> PenSample {
        PenSample {
            t_capture_ns: 0,
            t_device_ms: 0,
            serial: 0,
            x_raw: 0,
            y_raw: 0,
            z_raw: 0,
            x_norm: 0.0,
            y_norm: 0.0,
            pressure_raw: 0,
            pressure_norm: 0.0,
            tangent_pressure_raw: None,
            azimuth_deci_deg: None,
            altitude_deci_deg: None,
            twist_deci_deg: None,
            tilt_x_deg: None,
            tilt_y_deg: None,
            rotation_deci_deg: None,
            buttons: 0,
            tool: ToolKind::Pen,
            tool_serial: 0,
            in_proximity: true,
            status: 0,
        }
    }

    #[test]
    fn signature_maps_optional_axes_to_flags() {
        let bare = PenSignature::of(&sample(), "Tablet");
        assert_eq!(
            bare,
            PenSignature {
                device_name: "Tablet".to_owned(),
                ..PenSignature::default()
            }
        );

        let mut s = sample();
        s.tilt_x_deg = Some(12.0); // one tilt axis is enough
        s.tool_serial = 3;
        let tilt = PenSignature::of(&s, "Tablet");
        assert!(tilt.has_tilt);
        assert_eq!(tilt.tool_serial, 3);
        assert!(!tilt.has_tangent_pressure);

        let mut s = sample();
        s.tangent_pressure_raw = Some(512);
        s.twist_deci_deg = Some(0);
        s.rotation_deci_deg = Some(0);
        let airbrush = PenSignature::of(&s, "Tablet");
        assert!(airbrush.has_tangent_pressure);
        assert!(airbrush.has_twist);
        assert!(airbrush.has_rotation);
        assert!(!airbrush.has_tilt);
    }

    #[test]
    fn tool_kind_does_not_change_identity() {
        let mut pen = sample();
        let mut eraser = sample();
        eraser.tool = ToolKind::Eraser;
        pen.tool = ToolKind::Pen;
        assert_eq!(
            PenSignature::of(&pen, "Tablet"),
            PenSignature::of(&eraser, "Tablet")
        );
    }

    fn sig(name: &str, has_tilt: bool) -> PenSignature {
        PenSignature {
            device_name: name.to_owned(),
            has_tilt,
            ..PenSignature::default()
        }
    }

    #[test]
    fn tracker_emits_after_stable_run_only() {
        let mut tracker = PenTracker::default();
        for i in 1..STABLE_SAMPLES {
            assert_eq!(tracker.observe(sig("A", false)), None, "sample {i}");
            assert_eq!(tracker.current(), None);
        }
        assert_eq!(tracker.observe(sig("A", false)), Some(sig("A", false)));
        assert_eq!(tracker.current(), Some(&sig("A", false)));
    }

    #[test]
    fn tracker_never_re_emits_current_pen() {
        let mut tracker = PenTracker::default();
        for _ in 0..STABLE_SAMPLES {
            tracker.observe(sig("A", false));
        }
        for _ in 0..(STABLE_SAMPLES * 3) {
            assert_eq!(tracker.observe(sig("A", false)), None);
        }
    }

    #[test]
    fn signature_change_mid_debounce_restarts_count() {
        let mut tracker = PenTracker::default();
        for _ in 0..(STABLE_SAMPLES - 1) {
            tracker.observe(sig("A", false));
        }
        // Interrupt the run right before it would emit.
        assert_eq!(tracker.observe(sig("B", false)), None);
        for i in 1..STABLE_SAMPLES - 1 {
            assert_eq!(tracker.observe(sig("B", false)), None, "sample {i}");
        }
        assert_eq!(tracker.observe(sig("B", false)), Some(sig("B", false)));
    }

    #[test]
    fn glitch_toward_other_pen_is_forgotten_on_return() {
        let mut tracker = PenTracker::default();
        for _ in 0..STABLE_SAMPLES {
            tracker.observe(sig("A", false));
        }
        // A few glitched samples, then the known pen reports again: the
        // candidate is dropped, so a later run of B must start from scratch.
        for _ in 0..(STABLE_SAMPLES - 1) {
            tracker.observe(sig("B", false));
        }
        assert_eq!(tracker.observe(sig("A", false)), None);
        for i in 1..STABLE_SAMPLES {
            assert_eq!(tracker.observe(sig("B", false)), None, "sample {i}");
        }
        assert_eq!(tracker.observe(sig("B", false)), Some(sig("B", false)));
    }

    #[test]
    fn switching_back_and_forth_emits_each_change() {
        let mut tracker = PenTracker::default();
        let mut run = |s: PenSignature| {
            let mut emitted = None;
            for _ in 0..STABLE_SAMPLES {
                if let Some(out) = tracker.observe(s.clone()) {
                    emitted = Some(out);
                }
            }
            emitted
        };
        assert_eq!(run(sig("A", true)), Some(sig("A", true)));
        assert_eq!(run(sig("B", false)), Some(sig("B", false)));
        // A again: session-level it *is* a pen change; suppressing the modal
        // for already-introduced pens is the caller's seen-pens check.
        assert_eq!(run(sig("A", true)), Some(sig("A", true)));
    }

    #[test]
    fn seen_pens_check_suppresses_reintroduction() {
        // The caller-side dedup the modal relies on.
        let seen = vec![sig("A", true)];
        assert!(seen.contains(&sig("A", true)));
        assert!(!seen.contains(&sig("A", false)));
        assert!(!seen.contains(&sig("B", true)));
    }

    #[test]
    fn features_label_lists_present_axes() {
        assert_eq!(sig("A", false).features_label(), "pressure");
        let full = PenSignature {
            has_tilt: true,
            has_twist: true,
            has_tangent_pressure: true,
            has_rotation: true,
            ..PenSignature::default()
        };
        assert_eq!(
            full.features_label(),
            "pressure · tilt · twist · airbrush wheel · rotation"
        );
    }

    #[test]
    fn signature_round_trips_through_toml_and_tolerates_missing_fields() {
        let full = PenSignature {
            device_name: "Wacom Intuos Pro".to_owned(),
            tool_serial: 2,
            has_tilt: true,
            has_twist: false,
            has_tangent_pressure: true,
            has_rotation: false,
        };
        let text = toml::to_string_pretty(&full).unwrap();
        let back: PenSignature = toml::from_str(&text).unwrap();
        assert_eq!(full, back);

        // A signature written by a version with fewer fields still parses.
        let sparse: PenSignature = toml::from_str("device_name = \"Old\"").unwrap();
        assert_eq!(sparse.device_name, "Old");
        assert!(!sparse.has_tilt);
    }
}
