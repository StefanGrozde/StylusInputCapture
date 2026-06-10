mod actions;
mod app;
mod bindings;
mod cli;
mod midi_out;
mod prefs;
mod settings;
mod settings_ui;
mod theme;

use eframe::egui;

use app::HudApp;
use cli::Args;
use prefs::HudPrefs;
use settings::HudSettings;

fn main() -> eframe::Result<()> {
    let args = match Args::parse_env() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("argument error: {error}");
            std::process::exit(2);
        }
    };

    let prefs = HudPrefs::load();
    let settings = HudSettings::load();
    let (width, height) = prefs.window_size.unwrap_or((1280.0, 800.0));

    let viewport = egui::ViewportBuilder::default()
        .with_title("Tablet MPE HUD")
        .with_min_inner_size(egui::vec2(960.0, 640.0))
        .with_inner_size(egui::vec2(width, height));

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Tablet MPE HUD",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(HudApp::new(args, prefs, settings)))
        }),
    )
}
