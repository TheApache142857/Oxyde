//! The Oxyde desktop application: timeline, properties and 3D viewport around the modeling
//! engine. This is the only crate that picks a concrete kernel backend (OpenCASCADE).

// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod command_bar;
mod demo;
mod dialogs;
mod display;
mod document;
mod equations_ui;
mod evaluate;
mod features_ui;
mod icons;
mod model_refs;
mod property_manager;
mod sketch_mode;
mod timeline;
mod viewport;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let file = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Oxyde")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Oxyde",
        options,
        Box::new(move |cc| Ok(Box::new(app::OxydeApp::new(cc, file)))),
    )
}
