mod app;
mod calibration;
mod cli;
mod panels;
mod prefs;

use eframe::egui;

use app::TabletUiApp;
use cli::Args;
use prefs::UiPrefs;

fn main() -> eframe::Result<()> {
    let args = match Args::parse_env() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("argument error: {error}");
            std::process::exit(2);
        }
    };

    // Preferences are loaded *before* `run_native` so the persisted window
    // size can seed `NativeOptions` (SPEC_CUI.md §7) — and so `TabletUiApp`
    // can fall back to `last_profile_path` when `--profile` was not given.
    let prefs = UiPrefs::load();

    // The layout stacks a left, right, and bottom panel around a central area
    // that itself splits into two columns. If the window is too narrow the
    // central area collapses to zero width and egui's `columns` underflows
    // (`debug_assert` "Negative width"). A minimum inner size keeps the window
    // above that degenerate point; a comfortable default seeds first launch.
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Tablet Calibration UI")
        .with_min_inner_size(egui::vec2(900.0, 600.0))
        .with_inner_size(egui::vec2(1280.0, 820.0));
    if let Some((width, height)) = prefs.window_size {
        if width > 0.0 && height > 0.0 {
            viewport = viewport.with_inner_size(egui::vec2(width, height));
        }
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Tablet Calibration UI",
        options,
        Box::new(|_cc| Ok(Box::new(TabletUiApp::new(args, prefs)))),
    )
}
