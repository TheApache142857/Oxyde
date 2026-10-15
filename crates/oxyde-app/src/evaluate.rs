//! The Evaluate tab's windows: mass properties of the part, and measurements of the faces
//! and edges selected in the viewport.

use eframe::egui;
use oxyde_kernel::{CurveKind, EdgeInfo, FaceInfo, SurfaceKind};

use crate::app::{OxydeApp, Selected};

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
        let Some(body) = self.doc.part.body() else {
            ui.label("There is no solid yet.");
            return;
        };
        let volume = crate::app::bodies_volume(&self.kernel, body).ok();
        let topology = &body.topology;
        let area: f64 = topology.faces.iter().map(|f| f.area).sum();
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for p in topology
            .vertices
            .iter()
            .map(|v| v.point)
            .chain(topology.faces.iter().map(|f| f.centroid))
            .chain(topology.edges.iter().map(|e| e.midpoint))
        {
            for (k, value) in [p.x, p.y, p.z].into_iter().enumerate() {
                lo[k] = lo[k].min(value);
                hi[k] = hi[k].max(value);
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
            });
        if !self.doc.properties.material.is_empty() {
            ui.label(
                egui::RichText::new(format!("Material: {}", self.doc.properties.material))
                    .small()
                    .weak(),
            );
        }
        ui.label(
            egui::RichText::new("The size is measured along the model axes, from the vertices.")
                .small()
                .weak(),
        );
    }

    fn measure_ui(&mut self, ui: &mut egui::Ui) {
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
            ui.label("Click faces or edges in the graphics area (Ctrl+click for more).");
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
                    // A full circle: its radius follows from its length.
                    if e.curve == CurveKind::Circle && e.vertices.len() < 2 {
                        text.push_str(&format!("  (Ø {:.3})", e.length / std::f64::consts::PI));
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
}
