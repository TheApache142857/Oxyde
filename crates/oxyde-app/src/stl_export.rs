//! File › Export STL: the part (or every visible part of the assembly, placed) meshed for
//! 3D printing, with a window to choose how fine, which unit and which flavour of STL,
//! and the mesh shown in the viewport as it will be written, triangles and all, before
//! anything is saved. The window reports how many triangles there are, how big the file
//! will be and whether the mesh is watertight, which slicers need.
//!
//! The mesh is made with [`Kernel::tessellate_for_export`]: a maximum distance from the
//! true surface and a maximum angle between neighbouring triangles, so small holes and
//! fillets keep their shape at any part size.

use eframe::egui;
use oxyde_format::stl::{self, MeshReport, StlQuality, StlTriangle, StlUnit};
use oxyde_kernel::{Kernel, Mesh};
use oxyde_render::Scene;

use crate::app::OxydeApp;

pub struct StlDialog {
    pub quality: StlQuality,
    /// Maximum distance between the mesh and the true surface, mm.
    pub deflection: f64,
    /// Maximum angle between the normals across a triangle edge, degrees.
    pub angle: f64,
    pub unit: StlUnit,
    pub binary: bool,
    /// Draw the triangles' edges over the mesh in the viewport.
    pub wireframe: bool,
    /// The mesh as it will be written, in mm, in the document's coordinates.
    pub mesh: Option<Mesh>,
    pub report: Option<MeshReport>,
    pub error: Option<String>,
    /// Document revision the mesh was made for.
    pub revision: Option<u64>,
    /// Settings changed: mesh again.
    pub dirty: bool,
    /// Seconds the last meshing took.
    pub took: Option<f64>,
    /// Model size, to scale the presets.
    pub diagonal: f64,
}

/// A number with thousands separated: "12 345".
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

fn size_text(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// The edges of every triangle, as two-point polylines, each once per face.
fn triangle_edges(mesh: &Mesh) -> Vec<Vec<[f32; 3]>> {
    let mut out = Vec::new();
    for face in &mesh.faces {
        let mut seen = std::collections::HashSet::new();
        for t in &face.triangles {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                let key = (a.min(b), a.max(b));
                if a != b && seen.insert(key) {
                    if let (Some(p), Some(q)) =
                        (face.positions.get(a as usize), face.positions.get(b as usize))
                    {
                        out.push(vec![*p, *q]);
                    }
                }
            }
        }
    }
    out
}

impl OxydeApp {
    /// File › Export STL: opens the window and meshes the document.
    pub(crate) fn begin_stl_export(&mut self) {
        let empty = match &self.asm {
            Some(asm) => asm.leaves(&asm.assembly).is_empty(),
            None => self.doc.part.body().is_none(),
        };
        if empty {
            self.status = "Nothing to export: there is no solid yet".to_owned();
            return;
        }
        self.finish_sketch(true);
        let diagonal = self.model_diagonal();
        let (deflection, angle) = StlQuality::Normal.settings(diagonal).unwrap_or((0.05, 20.0));
        self.stl = Some(StlDialog {
            quality: StlQuality::Normal,
            deflection,
            angle,
            unit: StlUnit::Millimetre,
            binary: true,
            wireframe: true,
            mesh: None,
            report: None,
            error: None,
            revision: None,
            dirty: true,
            took: None,
            diagonal,
        });
    }

    /// The document's size, mm: the diagonal of its bounding box.
    fn model_diagonal(&self) -> f64 {
        let bounds = match &self.asm {
            Some(asm) => asm.bounds(&asm.assembly),
            None => {
                let mut lo = [f64::INFINITY; 3];
                let mut hi = [f64::NEG_INFINITY; 3];
                for solid in self.doc.part.body().map(|b| b.solids.as_slice()).unwrap_or(&[]) {
                    if let Ok((a, b)) = self.kernel.bounds(solid.shape) {
                        for (k, (a, b)) in
                            [(a.x, b.x), (a.y, b.y), (a.z, b.z)].into_iter().enumerate()
                        {
                            lo[k] = lo[k].min(a);
                            hi[k] = hi[k].max(b);
                        }
                    }
                }
                lo[0].is_finite().then(|| {
                    (
                        oxyde_geom::Point3::new(lo[0], lo[1], lo[2]),
                        oxyde_geom::Point3::new(hi[0], hi[1], hi[2]),
                    )
                })
            }
        };
        bounds
            .map(|(lo, hi)| (hi - lo).length())
            .filter(|d| d.is_finite() && *d > 0.0)
            .unwrap_or(100.0)
    }

    fn document_revision(&self) -> u64 {
        match &self.asm {
            Some(asm) => asm.revision,
            None => self.doc.revision,
        }
    }

    /// Meshes the document with the window's settings and shows the result.
    fn remesh_stl(&mut self) {
        let Some(d) = self.stl.as_ref() else {
            return;
        };
        let (deflection, angle) = (d.deflection.max(0.0005), d.angle.clamp(1.0, 60.0).to_radians());
        let start = std::time::Instant::now();
        let result = match self.asm.as_mut() {
            Some(asm) => {
                // Every visible part, placed; the ranges let the materials follow.
                let leaves = asm.leaves(&asm.assembly);
                let mut meshes = Vec::new();
                let mut error = None;
                for (o, placement, part) in leaves {
                    let Some(body) = part.part.body() else {
                        continue;
                    };
                    let mut mesh = Mesh::default();
                    for solid in &body.solids {
                        match self.kernel.tessellate_for_export(solid.shape, deflection, angle) {
                            Ok(m) => {
                                mesh.faces.extend(m.faces);
                                mesh.edges.extend(m.edges);
                            }
                            Err(e) => error = Some(e.to_string()),
                        }
                    }
                    meshes.push((o.clone(), placement, mesh));
                }
                match error {
                    Some(e) => Err(e),
                    None => {
                        let (mesh, ranges) = oxyde_assembly::scene::combine(
                            meshes.iter().map(|(o, p, m)| (o.clone(), p, m)),
                        );
                        asm.ranges = ranges;
                        Ok(mesh)
                    }
                }
            }
            None => match self.doc.part.body() {
                Some(body) => {
                    let mut mesh = Mesh::default();
                    let mut error = None;
                    for solid in &body.solids {
                        match self.kernel.tessellate_for_export(solid.shape, deflection, angle) {
                            Ok(m) => {
                                mesh.faces.extend(m.faces);
                                mesh.edges.extend(m.edges);
                            }
                            Err(e) => error = Some(e.to_string()),
                        }
                    }
                    error.map_or(Ok(mesh), Err)
                }
                None => Err("there is no solid".to_owned()),
            },
        };
        let took = start.elapsed().as_secs_f64();
        let revision = self.document_revision();
        let Some(d) = self.stl.as_mut() else {
            return;
        };
        d.dirty = false;
        d.revision = Some(revision);
        d.took = Some(took);
        match result {
            Ok(mesh) => {
                let tris = stl::triangles(&mesh, d.unit.per_mm());
                d.report = Some(stl::report(&tris));
                d.mesh = Some(mesh);
                d.error = None;
            }
            Err(e) => {
                d.mesh = None;
                d.report = None;
                d.error = Some(e);
            }
        }
        self.show_stl_preview();
    }

    /// Shows the export mesh in the viewport instead of the display mesh.
    fn show_stl_preview(&mut self) {
        let Some(d) = self.stl.as_ref() else {
            return;
        };
        let Some(mesh) = d.mesh.as_ref() else {
            return;
        };
        let mut shown = Mesh {
            faces: mesh.faces.clone(),
            edges: if d.wireframe {
                triangle_edges(mesh)
            } else {
                mesh.edges.clone()
            },
        };
        let (mut materials, textures) = match self.asm.as_ref().map(|a| a.assembly.clone()) {
            Some(assembly) => self.assembly_materials(&assembly),
            None => self.part_materials(),
        };
        self.add_context(&mut shown, &mut materials);
        let mut scene = Scene::from_mesh(&shown);
        scene.set_materials(materials, textures);
        // Looked at, not picked: faces and edges are the mesh's, not the model's.
        self.viewport.pickable = Some((0, 0));
        self.viewport.set_scene(scene, false);
        // The normal scene is not rebuilt over the preview until the window closes.
        let revision = self.document_revision();
        if self.asm.is_some() {
            self.asm_scene = Some(revision);
        } else {
            self.scene_revision = Some(revision);
        }
    }

    /// Closes the window and shows the model again.
    fn close_stl_export(&mut self) {
        self.stl = None;
        self.viewport.pickable = None;
        self.scene_revision = None;
        self.asm_scene = None;
    }

    /// The triangles as they will be written.
    fn stl_triangles(&self) -> Option<(Vec<StlTriangle>, String)> {
        let d = self.stl.as_ref()?;
        let mesh = d.mesh.as_ref()?;
        let name = match &self.asm {
            Some(asm) => asm.display_name(),
            None => self.doc.display_name(),
        };
        Some((stl::triangles(mesh, d.unit.per_mm()), name))
    }

    fn export_stl_file(&mut self) {
        let Some((tris, name)) = self.stl_triangles() else {
            return;
        };
        let binary = self.stl.as_ref().is_some_and(|d| d.binary);
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("STL", &["stl"])
            .set_file_name(format!("{name}.stl"))
            .save_file()
        else {
            return;
        };
        if !crate::assembly_ui::has_extension(&path, "stl") {
            path.set_extension("stl");
        }
        self.status = match stl::save(&path, &name, &tris, binary) {
            Ok(()) => format!(
                "Exported {} ({} triangles)",
                path.display(),
                thousands(tris.len() as u64)
            ),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    /// The Export STL window, in part and assembly mode.
    pub(crate) fn stl_window(&mut self, ctx: &egui::Context) {
        if self.stl.is_none() {
            return;
        }
        // The model changed under the window: mesh it again.
        let revision = self.document_revision();
        if self
            .stl
            .as_ref()
            .is_some_and(|d| d.revision.is_some_and(|r| r != revision))
        {
            if let Some(d) = self.stl.as_mut() {
                d.dirty = true;
            }
        }
        let mut open = true;
        let mut export = false;
        let mut close = false;
        let mut preview_changed = false;
        let assembly = self.asm.is_some();
        egui::Window::new("Export STL")
            .open(&mut open)
            .resizable(false)
            .default_pos(ctx.screen_rect().right_top() + egui::vec2(-360.0, 70.0))
            .show(ctx, |ui| {
                let Some(d) = self.stl.as_mut() else {
                    return;
                };
                ui.set_width(320.0);
                egui::Grid::new("stl_settings")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Quality");
                        let before = d.quality;
                        egui::ComboBox::from_id_salt("stl_quality")
                            .width(220.0)
                            .selected_text(
                                StlQuality::ALL
                                    .iter()
                                    .find(|(q, _)| *q == d.quality)
                                    .map_or("", |(_, l)| *l),
                            )
                            .show_ui(ui, |ui| {
                                for (quality, label) in StlQuality::ALL {
                                    ui.selectable_value(&mut d.quality, quality, label);
                                }
                            });
                        if d.quality != before {
                            if let Some((deflection, angle)) = d.quality.settings(d.diagonal) {
                                d.deflection = deflection;
                                d.angle = angle;
                            }
                            d.dirty = true;
                        }
                        ui.end_row();

                        let custom = d.quality == StlQuality::Custom;
                        ui.label("Maximum deviation")
                            .on_hover_text("How far the mesh may lie from the true surface");
                        ui.add_enabled_ui(custom, |ui| {
                            if ui
                                .add(
                                    egui::DragValue::new(&mut d.deflection)
                                        .speed(0.005)
                                        .range(0.001..=5.0)
                                        .suffix(" mm")
                                        .min_decimals(3),
                                )
                                .changed()
                            {
                                d.dirty = true;
                            }
                        });
                        ui.end_row();
                        ui.label("Maximum angle")
                            .on_hover_text("Between neighbouring triangles on a curved face");
                        ui.add_enabled_ui(custom, |ui| {
                            if ui
                                .add(
                                    egui::DragValue::new(&mut d.angle)
                                        .speed(0.5)
                                        .range(1.0..=60.0)
                                        .suffix("°"),
                                )
                                .changed()
                            {
                                d.dirty = true;
                            }
                        });
                        ui.end_row();

                        ui.label("Units");
                        egui::ComboBox::from_id_salt("stl_unit")
                            .width(220.0)
                            .selected_text(d.unit.label())
                            .show_ui(ui, |ui| {
                                for unit in StlUnit::ALL {
                                    if ui
                                        .selectable_value(&mut d.unit, unit, unit.label())
                                        .changed()
                                    {
                                        d.dirty = true;
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("Format");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut d.binary, true, "Binary")
                                .on_hover_text("Compact; every slicer reads it");
                            ui.radio_value(&mut d.binary, false, "ASCII")
                                .on_hover_text("Text, about five times larger");
                        });
                        ui.end_row();
                    });
                if ui
                    .checkbox(&mut d.wireframe, "Show the triangles in the viewport")
                    .changed()
                {
                    preview_changed = true;
                }
                ui.add_space(6.0);
                crate::property_manager::section(ui, "Mesh");
                if let Some(e) = &d.error {
                    ui.colored_label(crate::theme::tokens().err, format!("Meshing failed: {e}"));
                }
                if let Some(r) = &d.report {
                    let unit = d.unit.symbol();
                    egui::Grid::new("stl_report")
                        .num_columns(2)
                        .spacing([12.0, 3.0])
                        .show(ui, |ui| {
                            ui.label("Triangles");
                            ui.label(thousands(r.triangles as u64));
                            ui.end_row();
                            ui.label("Vertices");
                            ui.label(thousands(r.vertices as u64));
                            ui.end_row();
                            if let Some((lo, hi)) = r.bounds {
                                ui.label("Size");
                                ui.label(format!(
                                    "{:.2} × {:.2} × {:.2} {unit}",
                                    hi[0] - lo[0],
                                    hi[1] - lo[1],
                                    hi[2] - lo[2]
                                ));
                                ui.end_row();
                            }
                            ui.label("File size");
                            ui.label(if d.binary {
                                size_text(r.binary_size)
                            } else {
                                format!("about {}", size_text(r.ascii_size))
                            });
                            ui.end_row();
                            ui.label("Watertight");
                            if r.watertight() {
                                ui.label("Yes: a closed surface, ready to slice");
                            } else {
                                let mut problems = Vec::new();
                                if r.open_edges > 0 {
                                    problems.push(format!("{} open edges", r.open_edges));
                                }
                                if r.crowded_edges > 0 {
                                    problems.push(format!(
                                        "{} edges shared by more than two triangles",
                                        r.crowded_edges
                                    ));
                                }
                                ui.colored_label(
                                    crate::theme::tokens().warn,
                                    format!("No: {}", problems.join(", ")),
                                )
                                .on_hover_text(
                                    "Slicers may refuse the file or fill it wrongly. Open \
                                     edges come from faces meshed apart along a shared edge; \
                                     a finer or coarser setting often closes them.",
                                );
                            }
                            ui.end_row();
                            if let Some(took) = d.took {
                                ui.label("Meshed in");
                                ui.label(format!("{took:.2} s"));
                                ui.end_row();
                            }
                        });
                }
                ui.label(
                    egui::RichText::new(if assembly {
                        "Every visible part, where it sits in the assembly, as one mesh. \
                         Hidden components are left out."
                    } else {
                        "The viewport shows the mesh as it will be written."
                    })
                    .small()
                    .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(d.mesh.is_some() && !d.dirty, egui::Button::new("Export…"))
                        .clicked()
                    {
                        export = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        if self.stl.as_ref().is_some_and(|d| d.dirty) {
            self.remesh_stl();
        } else if preview_changed {
            self.show_stl_preview();
        }
        if export {
            self.export_stl_file();
        }
        if close || !open {
            self.close_stl_export();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_scale_with_the_model_and_numbers_read_well() {
        let (small, _) = StlQuality::Normal.settings(10.0).unwrap();
        let (large, _) = StlQuality::Normal.settings(1000.0).unwrap();
        assert!(small < large);
        assert_eq!(StlQuality::Custom.settings(10.0), None);
        let (coarse, a) = StlQuality::Coarse.settings(100.0).unwrap();
        let (fine, b) = StlQuality::VeryFine.settings(100.0).unwrap();
        assert!(coarse > fine && a > b);
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(12_345_678), "12\u{202f}345\u{202f}678");
        assert_eq!(size_text(500), "500 bytes");
        assert_eq!(size_text(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn triangle_edges_are_listed_once_per_face() {
        let mesh = Mesh {
            faces: vec![oxyde_kernel::FaceMesh {
                positions: vec![[0.0; 3], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]],
                triangles: vec![[0, 1, 2], [0, 2, 3]],
            }],
            edges: Vec::new(),
        };
        // Two triangles share the diagonal: five edges, not six.
        assert_eq!(triangle_edges(&mesh).len(), 5);
    }
}
