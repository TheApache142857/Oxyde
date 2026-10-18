//! File › Export: the formats a part or an assembly goes out as.
//!
//! - **STEP** (AP214, through OpenCASCADE's XDE layer): the exact B-rep, with the colours
//!   and the material of each solid, which other CAD systems read back. An assembly goes
//!   out as its visible parts, each moved to its place, in one file (flat: the product
//!   structure is not written yet).
//! - **IGES** 5.3: the exact B-rep as solids, for the systems that still want it.
//! - **STL**, **OBJ**, **3MF**, **glTF**: meshes, through the mesh export window
//!   (`mesh_export`).
//! - **Image**: the view as a PNG, drawn by the software renderer at any size.

use std::path::{Path, PathBuf};

use eframe::egui;
use oxyde_kernel::{Kernel, ShapeHandle, StepColors, StepMaterial};
use oxyde_render::{Highlight, StandardView, render_styled};

use crate::app::OxydeApp;
use crate::mesh_export::MeshFormat;

/// File › Export › Image: the picture settings.
pub struct ImageExportDialog {
    pub width: u32,
    pub height: u32,
    /// A standard view, or `None` for the view as it is.
    pub view: Option<StandardView>,
}

/// The solids to write, with their colours and materials: the part's body, or every
/// visible part of the assembly moved to its place (copies, released after the export).
pub(crate) struct ExportShapes {
    pub shapes: Vec<ShapeHandle>,
    colors: Vec<StepColors>,
    materials: Vec<Option<StepMaterial>>,
    /// Shapes made for the export alone.
    temporary: Vec<ShapeHandle>,
}

fn step_material(part: &oxyde_model::Part) -> Option<StepMaterial> {
    part.material().map(|m| StepMaterial {
        name: m.name.clone(),
        description: m.category.clone(),
        density: m.density,
    })
}

impl OxydeApp {
    /// The Export submenu of the File menu, in part and assembly mode.
    pub(crate) fn export_menu(&mut self, ui: &mut egui::Ui) {
        let assembly = self.asm.is_some();
        ui.menu_button("Export", |ui| {
            if ui
                .button("STEP…")
                .on_hover_text(if assembly {
                    "Every visible part where it sits, with its colours and material, in \
                     one STEP file"
                } else {
                    "The exact solid, with its colours and material, for any CAD system"
                })
                .clicked()
            {
                ui.close_menu();
                self.export_step();
            }
            if ui
                .button("IGES…")
                .on_hover_text("The exact solid as IGES 5.3, for older systems")
                .clicked()
            {
                ui.close_menu();
                self.export_iges();
            }
            ui.separator();
            for (format, hint) in [
                (MeshFormat::Stl, "A mesh for 3D printing, with a preview"),
                (
                    MeshFormat::Obj,
                    "A mesh with a named object per part or body, for Blender and renderers",
                ),
                (
                    MeshFormat::ThreeMf,
                    "A mesh with named, coloured objects and the unit, for modern slicers",
                ),
                (
                    MeshFormat::Gltf,
                    "Named objects with physically based materials (the Render tab's \
                     finishes), for Blender, three.js and Windows' 3D Viewer",
                ),
            ] {
                if ui
                    .button(format!("{}…", format.name()))
                    .on_hover_text(hint)
                    .clicked()
                {
                    ui.close_menu();
                    self.begin_mesh_export(format);
                }
            }
            ui.separator();
            if ui
                .button("Drawing (PDF, SVG, DXF)…")
                .on_hover_text(
                    "The standard views on a sheet with a title block, hidden lines dashed; \
                     DXF at true size, for laser and CAM",
                )
                .clicked()
            {
                ui.close_menu();
                self.begin_drawing_export();
            }
            if ui
                .button("Image (PNG)…")
                .on_hover_text("A picture of the view at any size")
                .clicked()
            {
                ui.close_menu();
                self.begin_image_export();
            }
        });
    }

    /// What to write: the part's solids, or the assembly's visible parts placed.
    pub(crate) fn export_shapes(&mut self) -> Result<ExportShapes, String> {
        match self.asm.as_ref() {
            None => {
                let body = self
                    .doc
                    .part
                    .body()
                    .ok_or_else(|| "Nothing to export: there is no solid yet".to_owned())?;
                let shapes = body.shapes();
                let colors = self.doc.part.step_colors(body);
                let materials = vec![step_material(&self.doc.part); shapes.len()];
                Ok(ExportShapes {
                    shapes,
                    colors,
                    materials,
                    temporary: Vec::new(),
                })
            }
            Some(asm) => {
                let mut out = ExportShapes {
                    shapes: Vec::new(),
                    colors: Vec::new(),
                    materials: Vec::new(),
                    temporary: Vec::new(),
                };
                for (_, placement, part) in asm.leaves(&asm.assembly) {
                    let Some(body) = part.part.body() else {
                        continue;
                    };
                    let colors = part.part.step_colors(body);
                    let material = step_material(&part.part);
                    let transform = placement.transform();
                    for (solid, color) in body.solids.iter().zip(colors) {
                        let moved = self
                            .kernel
                            .transform(solid.shape, &transform)
                            .map_err(|e| e.to_string())?;
                        out.temporary.push(moved.shape);
                        out.shapes.push(moved.shape);
                        out.colors.push(color);
                        out.materials.push(material.clone());
                    }
                }
                if out.shapes.is_empty() {
                    return Err("Nothing to export: no visible part has a solid".to_owned());
                }
                Ok(out)
            }
        }
    }

    pub(crate) fn release_temporary(&mut self, shapes: &ExportShapes) {
        for shape in &shapes.temporary {
            self.kernel.release(*shape);
        }
    }

    /// Asks where to save, with the document's name and `extension`.
    pub(crate) fn ask_export_path(&self, filter: &str, extensions: &[&str]) -> Option<PathBuf> {
        let name = match &self.asm {
            Some(asm) => asm.display_name(),
            None => self.doc.display_name(),
        };
        let first = extensions.first().copied().unwrap_or("txt");
        let mut path = rfd::FileDialog::new()
            .add_filter(filter, extensions)
            .set_file_name(format!("{name}.{first}"))
            .save_file()?;
        if !extensions
            .iter()
            .any(|e| crate::assembly_ui::has_extension(&path, e))
        {
            path.set_extension(first);
        }
        Some(path)
    }

    /// File › Export › STEP: the solids with their colours (appearances and material) as
    /// STEP colours, which other CAD systems read back; an assembly's visible parts
    /// placed, in one file.
    pub(crate) fn export_step(&mut self) {
        let shapes = match self.export_shapes() {
            Ok(s) => s,
            Err(message) => {
                self.status = message;
                return;
            }
        };
        let Some(path) = self.ask_export_path("STEP", &["step", "stp"]) else {
            self.release_temporary(&shapes);
            return;
        };
        let result =
            self.kernel
                .export_step(&shapes.shapes, &shapes.colors, &shapes.materials, &path);
        self.release_temporary(&shapes);
        self.status = match result {
            Ok(()) => format!("Exported {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    /// File › Export › IGES: the solids as IGES 5.3 B-rep.
    pub(crate) fn export_iges(&mut self) {
        let shapes = match self.export_shapes() {
            Ok(s) => s,
            Err(message) => {
                self.status = message;
                return;
            }
        };
        let Some(path) = self.ask_export_path("IGES", &["igs", "iges"]) else {
            self.release_temporary(&shapes);
            return;
        };
        let result = self.kernel.export_iges(&shapes.shapes, &path);
        self.release_temporary(&shapes);
        self.status = match result {
            Ok(()) => format!("Exported {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    // ---- Image -------------------------------------------------------------------------------

    /// File › Export › Image: opens the picture settings.
    pub(crate) fn begin_image_export(&mut self) {
        if self.viewport.scene().bounds().is_none() {
            self.status = "Nothing to picture: there is no solid yet".to_owned();
            return;
        }
        let size = self.viewport_rect.size();
        self.image_export = Some(ImageExportDialog {
            width: (size.x.max(1.0) as u32 * 2).clamp(64, 8192),
            height: (size.y.max(1.0) as u32 * 2).clamp(64, 8192),
            view: None,
        });
    }

    /// Draws the scene as the window asks and writes it as a PNG.
    fn write_image(&self, d: &ImageExportDialog, path: &Path) -> Result<(), String> {
        let scene = self.viewport.scene();
        let (width, height) = (d.width.max(1) as usize, d.height.max(1) as usize);
        let mut camera = self.viewport.camera();
        if let Some(view) = d.view {
            camera.set_view(view);
        }
        // Fitted to the picture: its shape is rarely the viewport's.
        if let Some((lo, hi)) = scene.bounds() {
            camera.fit(lo, hi, width as f64, height as f64);
        }
        let style = self.prefs.graphics.style();
        let frame = render_styled(scene, &camera, width, height, &Highlight::default(), &style);
        let image = image::RgbaImage::from_raw(width as u32, height as u32, frame.rgba)
            .ok_or_else(|| "the picture could not be assembled".to_owned())?;
        image.save(path).map_err(|e| e.to_string())
    }

    /// The Export Image window.
    pub(crate) fn image_export_window(&mut self, ctx: &egui::Context) {
        if self.image_export.is_none() {
            return;
        }
        let mut open = true;
        let mut export = false;
        let mut close = false;
        egui::Window::new("Export image")
            .open(&mut open)
            .resizable(false)
            .default_pos(ctx.screen_rect().right_top() + egui::vec2(-360.0, 70.0))
            .show(ctx, |ui| {
                let Some(d) = self.image_export.as_mut() else {
                    return;
                };
                ui.set_width(300.0);
                egui::Grid::new("image_export")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Size");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut d.width)
                                    .range(64..=8192)
                                    .suffix(" px"),
                            );
                            ui.label("×");
                            ui.add(
                                egui::DragValue::new(&mut d.height)
                                    .range(64..=8192)
                                    .suffix(" px"),
                            );
                        });
                        ui.end_row();
                        ui.label("View");
                        let views: [(Option<StandardView>, &str); 8] = [
                            (None, "As on screen"),
                            (Some(StandardView::Isometric), "Isometric"),
                            (Some(StandardView::Front), "Front"),
                            (Some(StandardView::Back), "Back"),
                            (Some(StandardView::Top), "Top"),
                            (Some(StandardView::Bottom), "Bottom"),
                            (Some(StandardView::Right), "Right"),
                            (Some(StandardView::Left), "Left"),
                        ];
                        let current = views
                            .iter()
                            .find(|(v, _)| *v == d.view)
                            .map_or("", |(_, l)| *l);
                        egui::ComboBox::from_id_salt("image_view")
                            .width(180.0)
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for (view, label) in views {
                                    ui.selectable_value(&mut d.view, view, label);
                                }
                            });
                        ui.end_row();
                    });
                ui.label(
                    egui::RichText::new(
                        "Drawn by the software renderer with the viewport's colours, the \
                         model fitted to the picture. Selection and sketches are left out.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Export…").clicked() {
                        export = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        if export && let Some(path) = self.ask_export_path("PNG image", &["png"]) {
            let result = match self.image_export.as_ref() {
                Some(d) => self.write_image(d, &path),
                None => Ok(()),
            };
            self.status = match result {
                Ok(()) => format!("Exported {}", path.display()),
                Err(e) => format!("Export failed: {e}"),
            };
            close = true;
        }
        if close || !open {
            self.image_export = None;
        }
    }
}
