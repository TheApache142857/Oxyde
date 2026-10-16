//! The Evaluate tab's windows: mass properties of the part (or of the whole assembly), and
//! measurements of the faces and edges selected in the viewport.

use eframe::egui;
use oxyde_assembly::Primitive;
use oxyde_kernel::{CurveKind, EdgeInfo, FaceInfo, Kernel, SurfaceKind};

use crate::app::{OxydeApp, Selected};
use crate::assembly::AsmSelected;

fn surface_name(kind: SurfaceKind) -> &'static str {
    match kind {
        SurfaceKind::Plane => "Flat",
        SurfaceKind::Cylinder => "Cylindrical",
        SurfaceKind::Cone => "Conical",
        SurfaceKind::Sphere => "Spherical",
        SurfaceKind::Torus => "Toroidal",
        SurfaceKind::BSpline => "Free-form",
        SurfaceKind::Other => "Curved",
    }
}

fn curve_name(kind: CurveKind) -> &'static str {
    match kind {
        CurveKind::Line => "Straight",
        CurveKind::Circle => "Circular",
        CurveKind::Ellipse => "Elliptical",
        CurveKind::BSpline => "Free-form",
        CurveKind::Degenerate => "Degenerate",
        CurveKind::Other => "Curved",
    }
}

fn row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(label);
    ui.label(egui::RichText::new(value).monospace());
    ui.end_row();
}

/// A mass in the unit that reads best.
/// What a circular edge measures: "R 5.000 (Ø 10.000)" for an arc, "Ø 10.000" for a full
/// circle. The radius is the distance of an end point from the axis; a full circle has
/// one vertex at most, and its radius also follows from its length.
fn circle_text(e: &EdgeInfo, topology: &oxyde_kernel::Topology) -> Option<String> {
    if e.curve != CurveKind::Circle {
        return None;
    }
    let from_vertex = e.axis.as_ref().and_then(|axis| {
        let v = topology.vertices.get(*e.vertices.first()? as usize)?;
        let d = v.point - axis.origin;
        let along = d.dot(axis.direction);
        Some((d - axis.direction * along).length())
    });
    let radius = from_vertex.unwrap_or(e.length / std::f64::consts::TAU);
    Some(if e.vertices.len() < 2 {
        format!("Ø {:.3}", radius * 2.0)
    } else {
        format!("R {:.3} (Ø {:.3})", radius, radius * 2.0)
    })
}

fn mass_text(kg: f64) -> String {
    if kg >= 1.0 {
        format!("{kg:.3} kg")
    } else if kg >= 0.001 {
        format!("{:.2} g", kg * 1000.0)
    } else {
        format!("{:.3} mg", kg * 1e6)
    }
}

impl OxydeApp {
    pub(crate) fn evaluate_windows(&mut self, ctx: &egui::Context) {
        if self.show_mass_properties {
            let mut open = true;
            egui::Window::new("Mass Properties")
                .open(&mut open)
                .resizable(false)
                .default_pos(ctx.screen_rect().right_top() + egui::vec2(-340.0, 220.0))
                .show(ctx, |ui| self.mass_properties_ui(ui));
            self.show_mass_properties = open;
        }
        if self.show_measure {
            let mut open = true;
            egui::Window::new("Measure")
                .open(&mut open)
                .resizable(false)
                .default_pos(ctx.screen_rect().right_top() + egui::vec2(-340.0, 480.0))
                .show(ctx, |ui| self.measure_ui(ui));
            self.show_measure = open;
        }
    }

    fn mass_properties_ui(&mut self, ui: &mut egui::Ui) {
        if self.asm.is_some() {
            self.asm_mass_properties_ui(ui);
            return;
        }
        let Some(body) = self.doc.part.body() else {
            ui.label("There is no solid yet.");
            return;
        };
        let volume = crate::app::bodies_volume(&self.kernel, body).ok();
        let topology = &body.topology;
        let area: f64 = topology.faces.iter().map(|f| f.area).sum();
        // The kernel's exact box: vertices, centroids and edge middles all lie on one
        // diameter of a cylinder, which then measured 0 across.
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for solid in &body.solids {
            if let Ok((a, b)) = self.kernel.bounds(solid.shape) {
                for (k, (a, b)) in [(a.x, b.x), (a.y, b.y), (a.z, b.z)].into_iter().enumerate() {
                    lo[k] = lo[k].min(a);
                    hi[k] = hi[k].max(b);
                }
            }
        }
        egui::Grid::new("mass_properties")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                row(
                    ui,
                    "Volume",
                    volume.map_or_else(|| "?".to_owned(), |v| format!("{v:.2} mm³")),
                );
                row(ui, "Surface area", format!("{area:.2} mm²"));
                if lo[0].is_finite() {
                    row(
                        ui,
                        "Size (X × Y × Z)",
                        format!(
                            "{:.2} × {:.2} × {:.2} mm",
                            hi[0] - lo[0],
                            hi[1] - lo[1],
                            hi[2] - lo[2]
                        ),
                    );
                }
                row(ui, "Faces", topology.faces.len().to_string());
                row(ui, "Edges", topology.edges.len().to_string());
                row(ui, "Vertices", topology.vertices.len().to_string());
                if let Some(material) = self.doc.part.material() {
                    row(ui, "Material", material.name.clone());
                    row(ui, "Density", format!("{:.0} kg/m³", material.density));
                    row(
                        ui,
                        "Mass",
                        volume.map_or_else(|| "?".to_owned(), |v| mass_text(material.mass_kg(v))),
                    );
                }
            });
        if self.doc.part.material().is_none() {
            let note = if self.doc.properties.material.is_empty() {
                "No material: give the part one (right-click › Material…) to get its mass."
                    .to_owned()
            } else {
                format!(
                    "Material property \"{}\" has no density; pick a library material for the mass.",
                    self.doc.properties.material
                )
            };
            ui.label(egui::RichText::new(note).small().weak());
        }
        ui.label(
            egui::RichText::new("The size is the bounding box along the model axes.")
                .small()
                .weak(),
        );
    }

    /// Volume, size and counts of the whole assembly.
    fn asm_mass_properties_ui(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        if doc.assembly.components.is_empty() {
            ui.label("There are no components yet.");
            return;
        }
        // Each part file's volume, computed once per load.
        let missing: Vec<std::path::PathBuf> = doc
            .docs
            .keys()
            .filter(|p| !doc.volumes.contains_key(*p))
            .cloned()
            .collect();
        for path in missing {
            let volume = match doc.docs.get(&path) {
                Some(Ok(crate::assembly::Loaded::Part(p))) => p
                    .part
                    .body()
                    .and_then(|b| crate::app::bodies_volume(&self.kernel, b).ok()),
                _ => None,
            };
            doc.volumes.insert(path, volume);
        }
        let leaves = doc.leaves(&doc.assembly);
        let parts = leaves.len();
        let mut volume = Some(0.0);
        // Mass of the parts that have a material; the others are counted.
        let mut mass = 0.0;
        let mut without_material = 0;
        for (o, _, part) in &leaves {
            let v = doc
                .path_at(o)
                .and_then(|p| doc.volumes.get(&p).copied().flatten());
            volume = match (volume, v) {
                (Some(total), Some(v)) => Some(total + v),
                _ => None,
            };
            match (part.part.material(), v) {
                (Some(m), Some(v)) => mass += m.mass_kg(v),
                _ => without_material += 1,
            }
        }
        let bounds = doc.bounds(&doc.assembly);
        let hidden = doc.assembly.components.iter().filter(|c| c.hidden).count();
        egui::Grid::new("asm_mass_properties")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                row(
                    ui,
                    "Volume",
                    volume.map_or_else(|| "?".to_owned(), |v| format!("{v:.2} mm³")),
                );
                if let Some((lo, hi)) = bounds {
                    row(
                        ui,
                        "Size (X × Y × Z)",
                        format!(
                            "{:.2} × {:.2} × {:.2} mm",
                            hi.x - lo.x,
                            hi.y - lo.y,
                            hi.z - lo.z
                        ),
                    );
                }
                if parts > without_material {
                    row(ui, "Mass", mass_text(mass));
                }
                row(ui, "Components", doc.assembly.components.len().to_string());
                row(ui, "Parts (every level)", parts.to_string());
                row(ui, "Mates", doc.assembly.mates.len().to_string());
            });
        let mut notes =
            vec!["Volumes add up every part shown, through every sub-assembly.".to_owned()];
        if hidden > 0 {
            notes.push("Hidden components are left out.".to_owned());
        }
        if without_material > 0 && parts > without_material {
            notes.push(format!(
                "{without_material} part(s) without a material are left out of the mass."
            ));
        } else if parts > 0 && without_material == parts {
            notes.push(
                "Give the parts materials (right-click › Material) to get the mass.".to_owned(),
            );
        }
        ui.label(egui::RichText::new(notes.join(" ")).small().weak());
    }

    fn measure_ui(&mut self, ui: &mut egui::Ui) {
        if self.asm.is_some() {
            self.asm_measure_ui(ui);
            return;
        }
        let Some(body) = self.doc.part.body() else {
            ui.label("There is no solid yet.");
            return;
        };
        let mut faces: Vec<&FaceInfo> = Vec::new();
        let mut edges: Vec<&EdgeInfo> = Vec::new();
        for selected in &self.selection {
            match selected {
                Selected::Face(name) => {
                    if let Some(f) = body
                        .naming
                        .find_face(name)
                        .and_then(|i| body.topology.faces.get(i as usize))
                    {
                        faces.push(f);
                    }
                }
                Selected::Edge(name) => {
                    if let Some(e) = body
                        .naming
                        .find_edge(name)
                        .and_then(|i| body.topology.edges.get(i as usize))
                    {
                        edges.push(e);
                    }
                }
            }
        }
        if faces.is_empty() && edges.is_empty() {
            ui.label(
                "Click faces or edges in the graphics area (Ctrl+click or Shift+click for more).",
            );
            return;
        }
        egui::Grid::new("measure")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for (i, f) in faces.iter().enumerate() {
                    row(
                        ui,
                        &format!("Face {} ({})", i + 1, surface_name(f.surface)),
                        format!("{:.3} mm²", f.area),
                    );
                }
                for (i, e) in edges.iter().enumerate() {
                    let mut text = format!("{:.3} mm", e.length);
                    if let Some(circle) = circle_text(e, &body.topology) {
                        text.push_str(&format!("  {circle}"));
                    }
                    row(
                        ui,
                        &format!("Edge {} ({})", i + 1, curve_name(e.curve)),
                        text,
                    );
                }
                if faces.len() > 1 {
                    let total: f64 = faces.iter().map(|f| f.area).sum();
                    row(ui, "Total area", format!("{total:.3} mm²"));
                }
                if edges.len() > 1 {
                    let total: f64 = edges.iter().map(|e| e.length).sum();
                    row(ui, "Total length", format!("{total:.3} mm"));
                }
                // Two flat faces: the angle between them, and their distance if parallel.
                if let [a, b] = faces.as_slice() {
                    if let (Some(na), Some(nb)) = (a.normal, b.normal) {
                        let cos = na.dot(nb).clamp(-1.0, 1.0);
                        let angle = cos.acos().to_degrees();
                        let folded = angle.min(180.0 - angle);
                        row(ui, "Angle", format!("{folded:.3}°"));
                        if folded < 1e-3 {
                            let distance = (b.centroid - a.centroid).dot(na).abs();
                            row(ui, "Distance", format!("{distance:.3} mm"));
                        }
                    }
                }
                // Two straight edges: the angle between them.
                if let [a, b] = edges.as_slice() {
                    if let (Some(aa), Some(ab)) = (&a.axis, &b.axis) {
                        if a.curve == CurveKind::Line && b.curve == CurveKind::Line {
                            let cos = aa.direction.dot(ab.direction).clamp(-1.0, 1.0);
                            let angle = cos.acos().to_degrees();
                            row(ui, "Angle", format!("{:.3}°", angle.min(180.0 - angle)));
                        }
                    }
                }
            });
    }

    /// Areas, lengths, and the angle and distance between two faces or edges selected in
    /// the assembly, where they sit.
    fn asm_measure_ui(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        // Each selected face or edge: its label, its size, and its geometry in the
        // assembly (a plane, a line, a circle…).
        let mut faces: Vec<(String, f64, Option<Primitive>)> = Vec::new();
        let mut edges: Vec<(String, String, Option<Primitive>)> = Vec::new();
        for item in &doc.selection {
            let Some(part) = (match item {
                AsmSelected::Face(o, _) | AsmSelected::Edge(o, _) => doc.part_at(o),
                _ => None,
            }) else {
                continue;
            };
            let Some(body) = part.part.body() else {
                continue;
            };
            let placed = item.mate_ref().and_then(|r| doc.placed(&doc.assembly, &r));
            match item {
                AsmSelected::Face(_, name) => {
                    if let Some(f) = body
                        .naming
                        .find_face(name)
                        .and_then(|i| body.topology.faces.get(i as usize))
                    {
                        faces.push((surface_name(f.surface).to_owned(), f.area, placed));
                    }
                }
                AsmSelected::Edge(_, name) => {
                    if let Some(e) = body
                        .naming
                        .find_edge(name)
                        .and_then(|i| body.topology.edges.get(i as usize))
                    {
                        let mut text = format!("{:.3} mm", e.length);
                        if let Some(circle) = circle_text(e, &body.topology) {
                            text.push_str(&format!("  {circle}"));
                        }
                        edges.push((curve_name(e.curve).to_owned(), text, placed));
                    }
                }
                _ => {}
            }
        }
        if faces.is_empty() && edges.is_empty() {
            ui.label(
                "Click faces or edges in the graphics area (Ctrl+click or Shift+click for more).",
            );
            return;
        }
        egui::Grid::new("asm_measure")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for (i, (kind, area, _)) in faces.iter().enumerate() {
                    row(
                        ui,
                        &format!("Face {} ({kind})", i + 1),
                        format!("{area:.3} mm²"),
                    );
                }
                for (i, (kind, text, _)) in edges.iter().enumerate() {
                    row(ui, &format!("Edge {} ({kind})", i + 1), text.clone());
                }
                if faces.len() > 1 {
                    let total: f64 = faces.iter().map(|(_, a, _)| a).sum();
                    row(ui, "Total area", format!("{total:.3} mm²"));
                }
                // Two things with a direction: the angle between them, and the distance
                // between two parallel planes or a plane and a point.
                let directed: Vec<&Primitive> = faces
                    .iter()
                    .map(|(_, _, p)| p)
                    .chain(edges.iter().map(|(_, _, p)| p))
                    .flatten()
                    .collect();
                if let [a, b] = directed.as_slice() {
                    let direction = |p: &Primitive| match *p {
                        Primitive::Plane { normal, .. } | Primitive::Circle { normal, .. } => {
                            Some(normal)
                        }
                        Primitive::Line { direction, .. } => Some(direction),
                        Primitive::Point(_) => None,
                    };
                    if let (Some(u), Some(v)) = (direction(a), direction(b)) {
                        let cos = u.dot(v).clamp(-1.0, 1.0);
                        let angle = cos.acos().to_degrees();
                        let folded = angle.min(180.0 - angle);
                        row(ui, "Angle", format!("{folded:.3}°"));
                        if let (Primitive::Plane { origin, normal }, other) = (a, b) {
                            let anchor = match **other {
                                Primitive::Plane { origin, .. }
                                | Primitive::Line { origin, .. } => origin,
                                Primitive::Circle { center, .. } => center,
                                Primitive::Point(p) => p,
                            };
                            if folded < 1e-3 {
                                let distance = (anchor - *origin).dot(*normal).abs();
                                row(ui, "Distance", format!("{distance:.3} mm"));
                            }
                        }
                    }
                }
            });
    }
}
