//! The Settings window: a per-action keybinding editor.
//!
//! Actions are listed grouped by category. **Bind…** puts the app into listen
//! mode — the next keyboard press, stylus button, or tablet ExpressKey becomes
//! the binding (Escape cancels). Chord capture itself happens in
//! `HudApp::on_chord` / the keyboard dispatch in `ui()`; this module only
//! draws the editor and mutates the keymap for clear/reset.

use eframe::egui::{self, RichText};

use crate::actions::Action;
use crate::app::HudApp;
use crate::bindings::Keymap;
use crate::theme;

impl HudApp {
    /// Draw the Settings window when open.
    pub(crate) fn draw_settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            // Closing the window also leaves listen mode — a hidden listener
            // silently eating the next keypress would be confusing.
            self.listening = None;
            return;
        }
        let mut open = self.settings_open;
        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(440.0)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("settings_scroll")
                    .show(ui, |ui| self.draw_keybindings(ui));
            });
        self.settings_open = open;
    }

    fn draw_keybindings(&mut self, ui: &mut egui::Ui) {
        ui.label(
            "Click Bind, then press a keyboard key, a stylus button, or a tablet \
             ExpressKey. Esc cancels. Stylus and tablet buttons work even while \
             another app is focused; keyboard binds need the HUD focused.",
        );
        ui.add_space(6.0);

        let mut category = "";
        for action in Action::ALL {
            if action.category() != category {
                category = action.category();
                ui.add_space(8.0);
                ui.label(RichText::new(category).strong().color(theme::ACCENT_DIM));
                ui.separator();
            }
            self.draw_binding_row(ui, action);
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui
                .button("Reset to defaults")
                .on_hover_text("Restore the built-in shortcuts (Esc, T, PageUp/PageDown)")
                .clicked()
            {
                self.settings.keymap = Keymap::default();
                self.listening = None;
                self.settings_status = Some("bindings reset to defaults".to_owned());
                self.save_settings();
            }
            if ui
                .button("Pen guide")
                .on_hover_text(
                    "Re-open the pen guide for the current pen (it appears \
                     automatically the first time a pen is used)",
                )
                .clicked()
            {
                self.execute_action(Action::ShowPenGuide);
            }
        });
        if let Some(status) = &self.settings_status {
            ui.add_space(4.0);
            ui.weak(status);
        }
    }

    fn draw_binding_row(&mut self, ui: &mut egui::Ui, action: Action) {
        ui.horizontal(|ui| {
            // Fixed-width label column keeps the chips roughly aligned without
            // a Grid (which would fight the per-category headers).
            ui.add_sized(
                [230.0, 18.0],
                egui::Label::new(action.label()).halign(egui::Align::LEFT),
            );

            let binding = self.settings.keymap.binding_for(action).copied();
            let chip = match &binding {
                Some(chord) => RichText::new(chord.label()).color(theme::ACCENT_DIM),
                None => RichText::new("unbound").color(theme::STATUS_IDLE),
            };
            ui.add_sized([110.0, 18.0], egui::Label::new(chip));

            if self.listening == Some(action) {
                if ui
                    .button(RichText::new("press input…").color(theme::ACCENT))
                    .on_hover_text("Press a key / stylus button / tablet key — Esc cancels")
                    .clicked()
                {
                    self.listening = None;
                }
            } else if ui.button("Bind…").clicked() {
                self.listening = Some(action);
                self.settings_status = None;
            }

            if binding.is_some() && ui.button("✕").on_hover_text("Remove binding").clicked() {
                self.settings.keymap.clear(action);
                self.settings_status = Some(format!("{} unbound", action.label()));
                self.save_settings();
            }
        });
    }
}
