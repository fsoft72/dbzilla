//! DBZilla entry point: native database client.

// The widget kit and glyph set are complete before every screen uses them; trimmed in the polish task.
#![allow(dead_code)]

mod app;
mod autocomplete;
mod cli;
mod fuzzy;
mod glyphs;
mod keymap;
mod mcpbackend;
mod sqlhl;
#[cfg(test)]
mod testsupport;
mod sqltext;
mod theme;
mod ui;

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
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = cli::parse_args(&args);
    if cli.help {
        println!("{}", cli::HELP);
        return Ok(());
    }
    let viewport = egui::ViewportBuilder::default().with_app_id(APP_ID).with_title(APP_TITLE).with_inner_size(INITIAL_SIZE).with_min_inner_size(MIN_SIZE);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(APP_TITLE, options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, cli)))))
}
