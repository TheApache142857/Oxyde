//! File › Export › STL, OBJ or 3MF: the part (or every visible part of the assembly, placed)
//! meshed for 3D printing or rendering, with a window to choose the format, how fine, which
//! unit and (for STL) which flavour, and the mesh shown in the viewport as it will be
//! written, triangles and all, before anything is saved. The window reports how many
//! triangles there are, how big an STL file will be and whether the mesh is watertight,
//! which slicers need.
//!
//! The mesh is made with [`Kernel::tessellate_for_export`]: a maximum distance from the
//! true surface and a maximum angle between neighbouring triangles, so small holes and
//! fillets keep their shape at any part size. OBJ and 3MF keep each part (and each body of
//! a part) as an object of its own, named, 3MF with its colour.

use eframe::egui;
use oxyde_format::stl::{self, MeshReport, StlQuality, StlTriangle, StlUnit};
use oxyde_format::{obj, threemf};
use oxyde_kernel::{Kernel, Mesh};
use oxyde_render::Scene;

use crate::app::OxydeApp;

/// The mesh file formats the window writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshFormat {
    /// Binary or ASCII STL: what every slicer reads, one mesh, no names or colours.
    Stl,
    /// Wavefront OBJ: one named object per part or body, with normals; Blender and renderers.
    Obj,
    /// 3MF: one named, coloured object per part or body, with the unit; modern slicers.
    ThreeMf,
}

impl MeshFormat {
    pub const ALL: [MeshFormat; 3] = [MeshFormat::Stl, MeshFormat::Obj, MeshFormat::ThreeMf];

    pub fn label(self) -> &'static str {
        match self {
            MeshFormat::Stl => "STL (3D printing, one mesh)",
            MeshFormat::Obj => "OBJ (Blender, renderers; named objects)",
            MeshFormat::ThreeMf => "3MF (slicers; named, coloured objects, with the unit)",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            MeshFormat::Stl => "STL",
            MeshFormat::Obj => "OBJ",
            MeshFormat::ThreeMf => "3MF",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            MeshFormat::Stl => "stl",
            MeshFormat::Obj => "obj",
            MeshFormat::ThreeMf => "3mf",
        }
    }
}

/// One object of the file: a part of the assembly, or a body of the part.
pub struct ExportObject {
    pub name: String,
    /// In mm, in the document's coordinates.
    pub mesh: Mesh,
    /// Its appearance colour, for formats that carry one.
    pub color: Option<[u8; 3]>,
}

pub struct MeshExportDialog {
    pub format: MeshFormat,
    pub quality: StlQuality,
    /// Maximum distance between the mesh and the true surface, mm.
    pub deflection: f64,
    /// Maximum angle between the normals across a triangle edge, degrees.
    pub angle: f64,
    pub unit: StlUnit,
    /// STL only: binary or ASCII.
    pub binary: bool,
    /// Draw the triangles' edges over the mesh in the viewport.
    pub wireframe: bool,
    /// The whole mesh as it will be written, in mm, in the document's coordinates.
    pub mesh: Option<Mesh>,
    /// The same mesh, object by object.
    pub objects: Vec<ExportObject>,
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
                    if let (Some(p), Some(q)) = (
                        face.positions.get(a as usize),
                        face.positions.get(b as usize),
                    ) {
                        out.push(vec![*p, *q]);
                    }
                }
            }
        }
    }
    out
}

/// Meshes every solid of a body, each as an object named after `name` (and its body
/// number when there are several), with the body's or part's colour.
fn body_objects<K: Kernel>(
    kernel: &K,
    part: &oxyde_model::Part,
    name: &str,
    deflection: f64,
    angle: f64,
    placement: Option<&oxyde_assembly::Placement>,
) -> Result<Vec<ExportObject>, String> {
    let Some(body) = part.body() else {
        return Ok(Vec::new());
    };
    let colors = part.step_colors(body);
    let mut out = Vec::with_capacity(body.solids.len());
    for (i, solid) in body.solids.iter().enumerate() {
        let mut mesh = kernel
            .tessellate_for_export(solid.shape, deflection, angle)
            .map_err(|e| e.to_string())?;
        if let Some(p) = placement {
            mesh = oxyde_assembly::scene::placed(&mesh, p);
        }
        let color = colors
            .get(i)
            .and_then(|c| c.solids.first().copied().flatten())
            .map(|c| c.rgb);
        let object_name = if body.solids.len() > 1 {
            format!("{name} Body {}", i + 1)
        } else {
            name.to_owned()
        };
        out.push(ExportObject {
            name: object_name,
            mesh,
            color,
        });
    }
    Ok(out)
}

impl OxydeApp {
    /// File › Export › STL, OBJ or 3MF: opens the window and meshes the document.
    pub(crate) fn begin_mesh_export(&mut self, format: MeshFormat) {
        let empty = match &self.asm {
            Some(asm) => asm.leaves(&asm.assembly).is_empty(),
            None => self.doc.part.body().is_none(),
        };
        if empty {
            self.status = "Nothing to export: there is no solid yet".to_owned();
            return;
        }
        self.finish_sketch(true);
        if let Some(d) = self.mesh_export.as_mut() {
            d.format = format;
            return;
        }
        let diagonal = self.model_diagonal();
        let (deflection, angle) = StlQuality::Normal
            .settings(diagonal)
            .unwrap_or((0.05, 20.0));
        self.mesh_export = Some(MeshExportDialog {
            format,
            quality: StlQuality::Normal,
            deflection,
            angle,
            unit: StlUnit::Millimetre,
            binary: true,
            wireframe: true,
            mesh: None,
            objects: Vec::new(),
            report: None,
            error: None,
            revision: None,
            dirty: true,
            took: None,
            diagonal,
        });
    }

    /// The document's size, mm: the diagonal of its bounding box.
    pub(crate) fn model_diagonal(&self) -> f64 {
        let bounds = match &self.asm {
            Some(asm) => asm.bounds(&asm.assembly),
            None => {
                let mut lo = [f64::INFINITY; 3];
                let mut hi = [f64::NEG_INFINITY; 3];
                for solid in self
                    .doc
                    .part
                    .body()
                    .map(|b| b.solids.as_slice())
                    .unwrap_or(&[])
                {
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

    pub(crate) fn document_revision(&self) -> u64 {
        match &self.asm {
            Some(asm) => asm.revision,
            None => self.doc.revision,
        }
    }

    /// Meshes the document with the window's settings and shows the result.
    fn remesh_export(&mut self) {
        let Some(d) = self.mesh_export.as_ref() else {
            return;
        };
        let (deflection, angle) = (
            d.deflection.max(0.0005),
            d.angle.clamp(1.0, 60.0).to_radians(),
        );
        let start = std::time::Instant::now();
        let result: Result<Vec<ExportObject>, String> = match self.asm.as_ref() {
            Some(asm) => {
                // Every visible part, placed, one object each, named by its place in the
                // tree ("Base", "Wheel assembly / Axle").
                let mut objects = Vec::new();
                for (o, placement, part) in asm.leaves(&asm.assembly) {
                    let name = asm.occurrence_name(&o);
                    match body_objects(
                        &self.kernel,
                        &part.part,
                        &name,
                        deflection,
                        angle,
                        Some(&placement),
                    ) {
                        Ok(mut found) => {
                            // A component's own appearance paints the whole part.
                            if let Some(a) = asm.occurrence_appearance(&asm.assembly, &o) {
                                for object in &mut found {
                                    object.color = Some(a.color);
                                }
                            }
                            objects.extend(found);
                        }
                        Err(e) => {
                            self.mesh_export_failed(e, start);
                            return;
                        }
                    }
                }
                Ok(objects)
            }
            None => body_objects(
                &self.kernel,
                &self.doc.part,
                &self.doc.display_name(),
                deflection,
                angle,
                None,
            ),
        };
        match result {
            Ok(objects) => {
                let mut mesh = Mesh::default();
                for object in &objects {
                    mesh.faces.extend(object.mesh.faces.iter().cloned());
                    mesh.edges.extend(object.mesh.edges.iter().cloned());
                }
                let took = start.elapsed().as_secs_f64();
                let revision = self.document_revision();
                if let Some(d) = self.mesh_export.as_mut() {
                    d.dirty = false;
                    d.revision = Some(revision);
                    d.took = Some(took);
                    let tris = stl::triangles(&mesh, d.unit.per_mm());
                    d.report = Some(stl::report(&tris));
                    d.mesh = Some(mesh);
                    d.objects = objects;
                    d.error = None;
                }
                self.show_mesh_preview();
            }
            Err(e) => self.mesh_export_failed(e, start),
        }
    }

    fn mesh_export_failed(&mut self, error: String, start: std::time::Instant) {
        let took = start.elapsed().as_secs_f64();
        let revision = self.document_revision();
        if let Some(d) = self.mesh_export.as_mut() {
            d.dirty = false;
            d.revision = Some(revision);
            d.took = Some(took);
            d.mesh = None;
            d.objects.clear();
            d.report = None;
            d.error = Some(error);
        }
    }

    /// Shows the export mesh in the viewport instead of the display mesh.
    fn show_mesh_preview(&mut self) {
        let Some(d) = self.mesh_export.as_ref() else {
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
        // The objects' own colours, face by face, so the preview looks like the model.
        let mut materials: Vec<oxyde_render::Material> = Vec::with_capacity(shown.faces.len());
        for object in &d.objects {
            let material = oxyde_render::Material {
                color: object.color,
                ..oxyde_render::Material::default()
            };
            materials.extend(std::iter::repeat_n(material, object.mesh.faces.len()));
        }
        self.add_context(&mut shown, &mut materials);
        let mut scene = Scene::from_mesh(&shown);
        scene.set_materials(materials, Vec::new());
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
    fn close_mesh_export(&mut self) {
        self.mesh_export = None;
        self.viewport.pickable = None;
        self.scene_revision = None;
        self.asm_scene = None;
    }

    /// The document's name, for the file and the STL solid.
    fn export_name(&self) -> String {
        match &self.asm {
            Some(asm) => asm.display_name(),
            None => self.doc.display_name(),
        }
    }

    /// The triangles as STL will write them.
    fn stl_triangles(&self) -> Option<Vec<StlTriangle>> {
        let d = self.mesh_export.as_ref()?;
        let mesh = d.mesh.as_ref()?;
        Some(stl::triangles(mesh, d.unit.per_mm()))
    }

    fn export_mesh_file(&mut self) {
        let Some(d) = self.mesh_export.as_ref() else {
            return;
        };
        let format = d.format;
        let name = self.export_name();
        let extension = format.extension();
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter(format.name(), &[extension])
            .set_file_name(format!("{name}.{extension}"))
            .save_file()
        else {
            return;
        };
        if !crate::assembly_ui::has_extension(&path, extension) {
            path.set_extension(extension);
        }
        let result = match format {
            MeshFormat::Stl => {
                let Some(tris) = self.stl_triangles() else {
                    return;
                };
                stl::save(&path, &name, &tris, d.binary).map(|()| tris.len())
            }
            MeshFormat::Obj => {
                let meshes: Vec<obj::NamedMesh<'_>> = d
                    .objects
                    .iter()
                    .map(|o| obj::NamedMesh {
                        name: &o.name,
                        mesh: &o.mesh,
                    })
                    .collect();
                obj::save(&path, &meshes, d.unit.per_mm()).map(|()| d.objects.len())
            }
            MeshFormat::ThreeMf => {
                let meshes: Vec<threemf::NamedMesh<'_>> = d
                    .objects
                    .iter()
                    .map(|o| threemf::NamedMesh {
                        name: &o.name,
                        mesh: &o.mesh,
                        color: o.color,
                    })
                    .collect();
                threemf::save(&path, &meshes, d.unit).map(|()| d.objects.len())
            }
        };
        self.status = match result {
            Ok(count) => match format {
                MeshFormat::Stl => format!(
                    "Exported {} ({} triangles)",
                    path.display(),
                    thousands(count as u64)
                ),
                _ => format!("Exported {} ({count} object(s))", path.display()),
            },
            Err(e) => format!("Export failed: {e}"),
        };
    }

    /// The mesh export window, in part and assembly mode.
    pub(crate) fn mesh_export_window(&mut self, ctx: &egui::Context) {
        if self.mesh_export.is_none() {
            return;
        }
        // The model changed under the window: mesh it again.
        let revision = self.document_revision();
        if self
            .mesh_export
            .as_ref()
            .is_some_and(|d| d.revision.is_some_and(|r| r != revision))
        {
            if let Some(d) = self.mesh_export.as_mut() {
                d.dirty = true;
            }
        }
        let mut open = true;
        let mut export = false;
        let mut close = false;
        let mut preview_changed = false;
        let assembly = self.asm.is_some();
        let title = self
            .mesh_export
            .as_ref()
            .map_or("Export mesh", |d| match d.format {
                MeshFormat::Stl => "Export STL",
                MeshFormat::Obj => "Export OBJ",
                MeshFormat::ThreeMf => "Export 3MF",
            });
        egui::Window::new(title)
            .id(egui::Id::new("mesh_export"))
            .open(&mut open)
            .resizable(false)
            .default_pos(ctx.screen_rect().right_top() + egui::vec2(-360.0, 70.0))
            .show(ctx, |ui| {
                let Some(d) = self.mesh_export.as_mut() else {
                    return;
                };
                ui.set_width(320.0);
                egui::Grid::new("mesh_export_settings")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Format");
                        egui::ComboBox::from_id_salt("mesh_format")
                            .width(220.0)
                            .selected_text(d.format.label())
                            .show_ui(ui, |ui| {
                                for format in MeshFormat::ALL {
                                    ui.selectable_value(&mut d.format, format, format.label());
                                }
                            });
                        ui.end_row();

                        ui.label("Quality");
                        let before = d.quality;
                        egui::ComboBox::from_id_salt("mesh_quality")
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

                        ui.label("Units").on_hover_text(match d.format {
                            MeshFormat::ThreeMf => "Written into the file, which readers honour",
                            _ => "The format has no unit: the coordinates are scaled to it",
                        });
                        egui::ComboBox::from_id_salt("mesh_unit")
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

                        if d.format == MeshFormat::Stl {
                            ui.label("Flavour");
                            ui.horizontal(|ui| {
                                ui.radio_value(&mut d.binary, true, "Binary")
                                    .on_hover_text("Compact; every slicer reads it");
                                ui.radio_value(&mut d.binary, false, "ASCII")
                                    .on_hover_text("Text, about five times larger");
                            });
                            ui.end_row();
                        }
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
                    egui::Grid::new("mesh_report")
                        .num_columns(2)
                        .spacing([12.0, 3.0])
                        .show(ui, |ui| {
                            if d.format != MeshFormat::Stl {
                                ui.label("Objects");
                                ui.label(d.objects.len().to_string());
                                ui.end_row();
                            }
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
                            if d.format == MeshFormat::Stl {
                                ui.label("File size");
                                ui.label(if d.binary {
                                    size_text(r.binary_size)
                                } else {
                                    format!("about {}", size_text(r.ascii_size))
                                });
                                ui.end_row();
                            }
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
                    egui::RichText::new(match (assembly, d.format) {
                        (true, MeshFormat::Stl) => {
                            "Every visible part, where it sits in the assembly, as one mesh. \
                             Hidden components are left out."
                        }
                        (true, _) => {
                            "Every visible part, where it sits in the assembly, as an object \
                             of its own named after the tree. Hidden components are left out."
                        }
                        (false, MeshFormat::Stl) => {
                            "The viewport shows the mesh as it will be written."
                        }
                        (false, _) => {
                            "Each body is an object of its own, named after the part. The \
                             viewport shows the mesh as it will be written."
                        }
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
        if self.mesh_export.as_ref().is_some_and(|d| d.dirty) {
            self.remesh_export();
        } else if preview_changed {
            self.show_mesh_preview();
        }
        if export {
            self.export_mesh_file();
        }
        if close || !open {
            self.close_mesh_export();
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
