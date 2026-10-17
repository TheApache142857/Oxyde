//! The Oxyde desktop application: timeline, properties and 3D viewport around the modeling
//! engine. This is the only crate that picks a concrete kernel backend (OpenCASCADE).

// Never crash: a panic in a modelling or UI path would take the unsaved document with it.
// Return an error or fall back to a safe default instead (docs/ARCHITECTURE.md, "Never crash").
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]
#![warn(clippy::indexing_slicing)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing
    )
)]
// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod appearance_ui;
mod assembly;
mod assembly_ui;
mod command_bar;
mod configurations_ui;
mod demo;
mod dialogs;
mod display;
mod document;
mod edit_in_place;
mod equations_ui;
mod evaluate;
mod export;
mod features_ui;
mod gpu;
mod icons;
mod interference;
mod jobs;
mod lockfile;
mod materials;
mod materials_ui;
mod mesh_export;
mod model_refs;
mod prefs;
mod prefs_ui;
mod preview;
mod property_manager;
mod render_ui;
mod section_view;
mod sketch_mode;
mod splash;
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
            .with_min_inner_size([800.0, 500.0])
            .with_icon(egui::IconData {
                // 64 × 64 RGBA, drawn by packaging/make_icon.py.
                rgba: include_bytes!("../assets/icon-64.rgba").to_vec(),
                width: 64,
                height: 64,
            }),
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
    let first = std::env::args_os().nth(1);
    // `--version`: prints the versions and quits; the bundle script uses it to check that
    // every DLL is there (OpenCASCADE is loaded to ask it its version).
    if first.as_deref() == Some(std::ffi::OsStr::new("--version")) {
        println!(
            "Oxyde {} (OpenCASCADE {})",
            env!("CARGO_PKG_VERSION"),
            oxyde_occt::OcctKernel::occt_version()
        );
        return Ok(());
    }
    let file = first.map(PathBuf::from);
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
