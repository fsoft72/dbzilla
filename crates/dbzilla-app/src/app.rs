//! Top level application state and frame loop.

use crate::theme;
use eframe::egui;

/// Root application object.
pub struct App;

impl App {
    /// Creates the app and installs the Obsidian theme exactly once.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        Self
    }
}

impl eframe::App for App {
    /// Draws one frame.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("DBZilla");
            ui.label("Native database client");
        });
    }
}
