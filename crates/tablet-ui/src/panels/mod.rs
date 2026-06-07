pub mod calibration;
pub mod inspector;
pub mod orientation;
pub mod pressure;
pub mod telemetry;
pub mod trace;

use eframe::egui;

/// Lay two sub-panels side by side, falling back to a vertical stack when the
/// available width is too small to host two columns.
///
/// `egui::Ui::columns` divides `(available_width - item_spacing) / 2` and feeds
/// that to `Ui::set_min_width`, which `debug_assert!`s on a negative value. When
/// the host area collapses (e.g. a narrow window starving the central panel)
/// that division underflows and panics. Stacking vertically below a threshold
/// keeps the layout valid at any size.
pub fn two_columns<L, R>(ui: &mut egui::Ui, left: L, right: R)
where
    L: FnOnce(&mut egui::Ui),
    R: FnOnce(&mut egui::Ui),
{
    const MIN_COLUMN_WIDTH: f32 = 80.0;
    let spacing = ui.spacing().item_spacing.x;
    if ui.available_width() >= 2.0 * MIN_COLUMN_WIDTH + spacing {
        ui.columns(2, |cols| {
            left(&mut cols[0]);
            right(&mut cols[1]);
        });
    } else {
        ui.vertical(|ui| {
            left(ui);
            ui.separator();
            right(ui);
        });
    }
}
