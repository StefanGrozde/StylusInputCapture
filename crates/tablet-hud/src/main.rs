mod app;
mod cli;
mod midi_out;

use eframe::egui;

use app::HudApp;
use cli::Args;

fn main() -> eframe::Result<()> {
    let args = match Args::parse_env() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("argument error: {error}");
            std::process::exit(2);
        }
    };

    let viewport = egui::ViewportBuilder::default()
        .with_title("Tablet MPE HUD")
        .with_min_inner_size(egui::vec2(960.0, 640.0))
        .with_inner_size(egui::vec2(1280.0, 800.0));

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Tablet MPE HUD",
        options,
        Box::new(|_cc| Ok(Box::new(HudApp::new(args)))),
    )
}
