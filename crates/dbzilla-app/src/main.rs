//! DBZilla entry point: native database client.

mod app;
mod theme;

use eframe::egui;

/// Application id, must match the desktop file on Wayland.
const APP_ID: &str = "dbzilla";
/// Window title.
const APP_TITLE: &str = "DBZilla";
/// Initial window size.
const INITIAL_SIZE: [f32; 2] = [1280.0, 800.0];
/// Minimum window size.
const MIN_SIZE: [f32; 2] = [900.0, 560.0];

/// Starts the eframe event loop.
fn main() -> eframe::Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_app_id(APP_ID)
        .with_title(APP_TITLE)
        .with_inner_size(INITIAL_SIZE)
        .with_min_inner_size(MIN_SIZE);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(APP_TITLE, options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
