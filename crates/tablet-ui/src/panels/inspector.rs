use eframe::egui;
use tablet_core::PenSample;
use tablet_process::ProcessedSample;

pub fn draw_inspector_panel(
    ui: &mut egui::Ui,
    latest_sample: Option<&PenSample>,
    latest_processed: Option<&ProcessedSample>,
) {
    ui.vertical(|ui| {
        ui.heading("inspector");

        let Some(raw) = latest_sample else {
            ui.label("waiting for samples");
            return;
        };

        crate::panels::two_columns(
            ui,
            |ui| {
            ui.strong("raw PenSample");
            egui::Grid::new("raw_sample_fields")
                .num_columns(2)
                .spacing([10.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    raw_row(ui, "t_capture_ns", raw.t_capture_ns);
                    raw_row(ui, "t_device_ms", raw.t_device_ms);
                    raw_row(ui, "serial", raw.serial);
                    raw_row(ui, "x_raw", raw.x_raw);
                    raw_row(ui, "y_raw", raw.y_raw);
                    raw_row(ui, "z_raw", raw.z_raw);
                    raw_row(ui, "x_norm", format!("{:.6}", raw.x_norm));
                    raw_row(ui, "y_norm", format!("{:.6}", raw.y_norm));
                    raw_row(ui, "pressure_raw", raw.pressure_raw);
                    raw_row(ui, "pressure_norm", format!("{:.6}", raw.pressure_norm));
                    raw_row(
                        ui,
                        "tangent_pressure_raw",
                        format_option(raw.tangent_pressure_raw),
                    );
                    raw_row(ui, "azimuth_deci_deg", format_option(raw.azimuth_deci_deg));
                    raw_row(
                        ui,
                        "altitude_deci_deg",
                        format_option(raw.altitude_deci_deg),
                    );
                    raw_row(ui, "twist_deci_deg", format_option(raw.twist_deci_deg));
                    raw_row(ui, "tilt_x_deg", format_option_f64(raw.tilt_x_deg));
                    raw_row(ui, "tilt_y_deg", format_option_f64(raw.tilt_y_deg));
                    raw_row(
                        ui,
                        "rotation_deci_deg",
                        format_option(raw.rotation_deci_deg),
                    );
                    raw_row(ui, "buttons", format!("0x{:08x}", raw.buttons));
                    raw_row(ui, "tool", format!("{:?}", raw.tool));
                    raw_row(ui, "tool_serial", raw.tool_serial);
                    raw_row(ui, "in_proximity", raw.in_proximity);
                    raw_row(ui, "status", format!("0x{:08x}", raw.status));
                });
            },
            |ui| {
            ui.strong("derived ProcessedSample");
            if let Some(processed) = latest_processed {
                egui::Grid::new("processed_sample_fields")
                    .num_columns(2)
                    .spacing([10.0, 4.0])
                    .striped(true)
                    .show(ui, |ui| {
                        raw_row(ui, "raw retained", "yes");
                        raw_row(ui, "raw serial", processed.raw.serial);
                        raw_row(ui, "x", format!("{:.6}", processed.x));
                        raw_row(ui, "y", format!("{:.6}", processed.y));
                        raw_row(ui, "x_filtered", format_option_f64(processed.x_filtered));
                        raw_row(ui, "y_filtered", format_option_f64(processed.y_filtered));
                        raw_row(ui, "pressure", format!("{:.6}", processed.pressure));
                        raw_row(ui, "active", processed.active);
                        raw_row(ui, "tilt_x", format_option_f64(processed.tilt_x));
                        raw_row(ui, "tilt_y", format_option_f64(processed.tilt_y));
                        raw_row(ui, "twist", format_option_f64(processed.twist));
                        raw_row(ui, "out_of_range", processed.out_of_range);
                    });
            } else {
                ui.label("waiting for capabilities");
            }
            },
        );
    });
}

fn raw_row(ui: &mut egui::Ui, label: &str, value: impl ToString) {
    ui.label(label);
    ui.monospace(value.to_string());
    ui.end_row();
}

fn format_option(value: Option<i32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_owned())
}

fn format_option_f64(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.6}"))
        .unwrap_or_else(|| "-".to_owned())
}
