use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use tablet_core::{AxisInfo, DeviceCapabilities, PenSample};

use crate::panels::trace::TraceHistory;

pub fn draw_orientation_panel(
    ui: &mut egui::Ui,
    history: &TraceHistory,
    caps: Option<&DeviceCapabilities>,
) {
    ui.vertical(|ui| {
        ui.heading("orientation");

        let Some(latest) = history.latest() else {
            ui.label("waiting for samples");
            return;
        };

        let processed = &latest.processed;
        let raw = &processed.raw;
        let out_of_range = processed.out_of_range || raw_orientation_out_of_range(raw, caps);
        if out_of_range {
            ui.colored_label(Color32::from_rgb(255, 120, 95), "out of range");
        }

        ui.horizontal(|ui| {
            draw_compass(ui, raw.azimuth_deci_deg.map(deci_to_deg));
            draw_altitude_gauge(ui, raw.altitude_deci_deg.map(deci_to_deg));
            draw_dial(
                ui,
                "twist",
                processed
                    .twist
                    .or_else(|| raw.twist_deci_deg.map(deci_to_deg)),
            );
            draw_dial(ui, "rotation", raw.rotation_deci_deg.map(deci_to_deg));
        });

        ui.separator();
        egui::Grid::new("orientation_numbers")
            .num_columns(3)
            .spacing([12.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                ui.strong("axis");
                ui.strong("raw deci-deg");
                ui.strong("processed deg");
                ui.end_row();

                number_row(
                    ui,
                    "azimuth / tilt x",
                    raw.azimuth_deci_deg,
                    processed.tilt_x,
                );
                number_row(
                    ui,
                    "altitude / tilt y",
                    raw.altitude_deci_deg,
                    processed.tilt_y,
                );
                number_row(ui, "twist", raw.twist_deci_deg, processed.twist);
                number_row(ui, "rotation", raw.rotation_deci_deg, None);
            });

        ui.separator();
        if let Some(caps) = caps {
            range_bar(
                ui,
                "azimuth",
                raw.azimuth_deci_deg.map(i64::from),
                &caps.azimuth,
            );
            range_bar(
                ui,
                "altitude",
                raw.altitude_deci_deg.map(i64::from),
                &caps.altitude,
            );
            range_bar(ui, "twist", raw.twist_deci_deg.map(i64::from), &caps.twist);
            range_bar(
                ui,
                "rotation",
                raw.rotation_deci_deg.map(i64::from),
                &caps.rotation,
            );
        } else {
            ui.label("waiting for capabilities");
        }
    });
}

fn number_row(ui: &mut egui::Ui, label: &str, raw_deci: Option<i32>, processed: Option<f64>) {
    ui.label(label);
    ui.label(format_optional_i32(raw_deci));
    ui.label(format_optional_f64(processed));
    ui.end_row();
}

fn draw_compass(ui: &mut egui::Ui, degrees: Option<f64>) {
    draw_circular_indicator(ui, "azimuth", degrees, 64.0, true);
}

fn draw_dial(ui: &mut egui::Ui, label: &str, degrees: Option<f64>) {
    draw_circular_indicator(ui, label, degrees, 54.0, true);
}

fn draw_circular_indicator(
    ui: &mut egui::Ui,
    label: &str,
    degrees: Option<f64>,
    size: f32,
    wrap: bool,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let painter = ui.painter_at(rect);
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.34;
    painter.circle_stroke(
        center,
        radius,
        Stroke::new(1.0, ui.visuals().weak_text_color()),
    );

    if let Some(degrees) = degrees {
        let degrees = if wrap {
            degrees.rem_euclid(360.0)
        } else {
            degrees
        };
        let hand = Vec2::angled((degrees as f32).to_radians()) * radius;
        painter.line_segment(
            [center, center + hand],
            Stroke::new(2.0, Color32::from_rgb(255, 205, 95)),
        );
        painter.circle_filled(center + hand, 2.5, Color32::from_rgb(255, 205, 95));
    }

    painter.text(
        Pos2::new(center.x, rect.bottom() - 2.0),
        Align2::CENTER_BOTTOM,
        label,
        FontId::proportional(11.0),
        ui.visuals().text_color(),
    );
}

fn draw_altitude_gauge(ui: &mut egui::Ui, degrees: Option<f64>) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(58.0, 64.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let gauge = Rect::from_min_max(
        Pos2::new(rect.center().x - 5.0, rect.top() + 8.0),
        Pos2::new(rect.center().x + 5.0, rect.bottom() - 18.0),
    );
    painter.rect_stroke(
        gauge,
        2.0,
        Stroke::new(1.0, ui.visuals().weak_text_color()),
        StrokeKind::Inside,
    );

    if let Some(degrees) = degrees {
        let t = (degrees / 90.0).clamp(0.0, 1.0) as f32;
        let y = gauge.bottom() - gauge.height() * t;
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(gauge.left(), y), gauge.right_bottom()),
            2.0,
            Color32::from_rgb(120, 210, 170),
        );
    }

    painter.text(
        Pos2::new(rect.center().x, rect.bottom() - 2.0),
        Align2::CENTER_BOTTOM,
        "altitude",
        FontId::proportional(11.0),
        ui.visuals().text_color(),
    );
}

fn range_bar(ui: &mut egui::Ui, label: &str, value: Option<i64>, axis: &AxisInfo) {
    ui.horizontal(|ui| {
        ui.set_min_height(22.0);
        ui.label(label);

        let desired = Vec2::new(ui.available_width().max(80.0), 16.0);
        let (rect, _) = ui.allocate_exact_size(desired, Sense::hover());
        let painter = ui.painter_at(rect);
        let supported_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
        painter.rect_stroke(
            rect,
            2.0,
            Stroke::new(1.0, supported_color),
            StrokeKind::Inside,
        );

        if !axis.supported {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "unsupported",
                FontId::proportional(11.0),
                ui.visuals().weak_text_color(),
            );
            return;
        }

        if let Some(value) = value {
            let span = (axis.max - axis.min).max(1) as f32;
            let t = ((value - axis.min) as f32 / span).clamp(0.0, 1.0);
            let x = rect.left() + rect.width() * t;
            let in_range = value >= axis.min && value <= axis.max;
            let color = if in_range {
                Color32::from_rgb(120, 210, 170)
            } else {
                Color32::from_rgb(255, 120, 95)
            };
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(2.0, color),
            );
            painter.text(
                rect.left_center() + Vec2::new(4.0, 0.0),
                Align2::LEFT_CENTER,
                format!("{}..{}  value {}", axis.min, axis.max, value),
                FontId::monospace(10.0),
                ui.visuals().text_color(),
            );
        } else {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "none",
                FontId::proportional(11.0),
                ui.visuals().weak_text_color(),
            );
        }
    });
}

fn raw_orientation_out_of_range(raw: &PenSample, caps: Option<&DeviceCapabilities>) -> bool {
    let Some(caps) = caps else {
        return false;
    };

    axis_out(raw.azimuth_deci_deg.map(i64::from), &caps.azimuth)
        || axis_out(raw.altitude_deci_deg.map(i64::from), &caps.altitude)
        || axis_out(raw.twist_deci_deg.map(i64::from), &caps.twist)
        || axis_out(raw.rotation_deci_deg.map(i64::from), &caps.rotation)
}

fn axis_out(value: Option<i64>, axis: &AxisInfo) -> bool {
    axis.supported
        && value
            .map(|value| value < axis.min || value > axis.max)
            .unwrap_or(false)
}

fn deci_to_deg(value: i32) -> f64 {
    value as f64 / 10.0
}

fn format_optional_i32(value: Option<i32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_owned())
}

fn format_optional_f64(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "-".to_owned())
}
