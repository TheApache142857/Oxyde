//! The Oxyde desktop application: timeline, properties and 3D viewport around the modeling
//! engine. This is the only crate that picks a concrete kernel backend (OpenCASCADE).

// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod assembly;
mod assembly_ui;
mod command_bar;
mod demo;
mod dialogs;
mod display;
mod document;
mod equations_ui;
mod evaluate;
mod features_ui;
mod gpu;
mod icons;
mod model_refs;
mod prefs;
mod prefs_ui;
mod property_manager;
mod sketch_mode;
mod theme;
mod timeline;
mod viewport;

use std::path::PathBuf;

use eframe::egui;

fn options(renderer: eframe::Renderer) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Oxyde")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 500.0]),
        renderer,
        ..Default::default()
    }
}

fn run(renderer: eframe::Renderer, file: Option<PathBuf>) -> eframe::Result<()> {
    eframe::run_native(
        "Oxyde",
        options(renderer),
        Box::new(move |cc| Ok(Box::new(app::OxydeApp::new(cc, file)))),
    )
}

/// The viewport is drawn by the GPU (wgpu: Direct3D 12, Vulkan or Metal). If that cannot
/// start, or with `OXYDE_RENDERER=cpu`, the window falls back to OpenGL and the viewport to
/// the software renderer.
fn main() -> eframe::Result<()> {
    let file = std::env::args_os().nth(1).map(PathBuf::from);
    let software = std::env::var("OXYDE_RENDERER").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"))
        || prefs::Preferences::load().graphics.renderer == prefs::Renderer::Software;
    if software {
        return run(eframe::Renderer::Glow, file);
    }
    match run(eframe::Renderer::Wgpu, file.clone()) {
        Err(e) => {
            eprintln!("GPU rendering unavailable ({e}); using the software renderer");
            run(eframe::Renderer::Glow, file)
        }
        ok => ok,
    }
}
