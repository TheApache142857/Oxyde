//! Modal windows: new sketch, extrude, fillet/chamfer, rename, part properties, unsaved
//! changes.

use eframe::egui;
use oxyde_format::Properties;
use oxyde_geom::Point2;
use oxyde_model::{
    EdgeBlend, EdgeName, Extrude, ExtrudeExtent, ExtrudeOp, FaceName, FeatureId, FeatureKind,
    Sketch, SketchPlane, StandardPlane,
};

use crate::app::{OxydeApp, PendingAction};

#[derive(Debug, Clone, PartialEq)]
pub enum PlaneChoice {
    Standard(StandardPlane),
    Face(FaceName),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SketchShape {
    Rectangle,
    Circle,
}

pub struct SketchDialog {
    pub plane: PlaneChoice,
    /// The planar face selected when the dialog opened, offered as a sketch plane.
    pub face: Option<FaceName>,
    pub shape: SketchShape,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub radius: f64,
}

pub struct ExtrudeDialog {
    pub sketch: FeatureId,
    pub sketches: Vec<(FeatureId, String)>,
    pub op: ExtrudeOp,
    pub depth: f64,
    pub mid_plane: bool,
    pub reverse: bool,
}

pub struct BlendDialog {
    pub fillet: bool,
    pub size: f64,
    pub edges: Vec<EdgeName>,
}

pub enum Dialog {
    Sketch(SketchDialog),
    Extrude(ExtrudeDialog),
    Blend(BlendDialog),
    Rename { feature: FeatureId, name: String },
    PartProperties(Properties),
    Discard(PendingAction),
    About,
}

/// User's answer to a dialog this frame.
enum Answer {
    Open,
    Ok,
    Cancel,
}

fn window(title: &str) -> egui::Window<'static> {
    egui::Window::new(title.to_owned())
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
}

fn ok_cancel(ui: &mut egui::Ui, ok_label: &str) -> Answer {
    let mut answer = Answer::Open;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button(ok_label).clicked() {
            answer = Answer::Ok;
        }
        if ui.button("Cancel").clicked() {
            answer = Answer::Cancel;
        }
    });
    answer
}

fn length(ui: &mut egui::Ui, value: &mut f64) {
    ui.add(egui::DragValue::new(value).speed(0.1).suffix(" mm"));
}

fn positive_length(ui: &mut egui::Ui, value: &mut f64) {
    length(ui, value);
    if *value < 0.001 {
        *value = 0.001;
    }
}

pub fn plane_name(plane: StandardPlane) -> &'static str {
    match plane {
        StandardPlane::Front => "Front",
        StandardPlane::Top => "Top",
        StandardPlane::Right => "Right",
    }
}

fn plane_label(choice: &PlaneChoice) -> String {
    match choice {
        PlaneChoice::Standard(p) => format!("{} Plane", plane_name(*p)),
        PlaneChoice::Face(_) => "Selected face".to_owned(),
    }
}

impl OxydeApp {
    pub(crate) fn dialogs_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let answer = match &mut dialog {
            Dialog::Sketch(d) => sketch_window(ctx, d),
            Dialog::Extrude(d) => extrude_window(ctx, d),
            Dialog::Blend(d) => blend_window(ctx, d),
            Dialog::Rename { name, .. } => {
                let mut answer = Answer::Open;
                window("Rename feature").show(ctx, |ui| {
                    ui.text_edit_singleline(name);
                    answer = ok_cancel(ui, "Rename");
                });
                answer
            }
            Dialog::PartProperties(p) => properties_window(ctx, p),
            Dialog::Discard(_) => {
                let mut answer = Answer::Open;
                let mut save = false;
                window("Unsaved changes").show(ctx, |ui| {
                    ui.label(format!(
                        "Save the changes to {} first?",
                        self.doc.display_name()
                    ));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            save = true;
                            answer = Answer::Ok;
                        }
                        if ui.button("Don't save").clicked() {
                            answer = Answer::Ok;
                        }
                        if ui.button("Cancel").clicked() {
                            answer = Answer::Cancel;
                        }
                    });
                });
                if save && !self.save() {
                    // Saving failed or was cancelled: stay where we are.
                    answer = Answer::Cancel;
                }
                answer
            }
            Dialog::About => {
                let mut answer = Answer::Open;
                window("About Oxyde").show(ctx, |ui| {
                    ui.heading("Oxyde");
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.label("Parametric CAD in Rust, on the OpenCASCADE kernel.");
                    ui.label(format!(
                        "OpenCASCADE {}",
                        oxyde_occt::OcctKernel::occt_version()
                    ));
                    if ui.button("Close").clicked() {
                        answer = Answer::Cancel;
                    }
                });
                answer
            }
        };

        match answer {
            Answer::Open => self.dialog = Some(dialog),
            Answer::Cancel => {}
            Answer::Ok => self.apply_dialog(dialog, ctx),
        }
    }

    fn apply_dialog(&mut self, dialog: Dialog, ctx: &egui::Context) {
        match dialog {
            Dialog::Sketch(d) => {
                let plane = match d.plane {
                    PlaneChoice::Standard(p) => SketchPlane::Standard(p),
                    PlaneChoice::Face(face) => SketchPlane::Face(face),
                };
                let mut sketch = Sketch::new(plane);
                match d.shape {
                    SketchShape::Rectangle => {
                        sketch.rectangle(Point2::new(d.x, d.y), d.width, d.height);
                    }
                    SketchShape::Circle => {
                        sketch.circle(Point2::new(d.x, d.y), d.radius);
                    }
                }
                let id = self.add_feature(FeatureKind::Sketch(sketch));
                // Straight on to the extrusion, as most sketches are extruded.
                self.begin_extrude_of(id, ExtrudeOp::Boss);
            }
            Dialog::Extrude(d) => {
                let extent = if d.mid_plane {
                    ExtrudeExtent::MidPlane { depth: d.depth }
                } else {
                    ExtrudeExtent::Blind { depth: d.depth }
                };
                self.add_feature(FeatureKind::Extrude(Extrude {
                    sketch: d.sketch,
                    op: d.op,
                    extent,
                    reverse: d.reverse && !d.mid_plane,
                }));
            }
            Dialog::Blend(d) => {
                let blend = EdgeBlend {
                    edges: d.edges,
                    size: d.size,
                };
                self.add_feature(if d.fillet {
                    FeatureKind::Fillet(blend)
                } else {
                    FeatureKind::Chamfer(blend)
                });
            }
            Dialog::Rename { feature, name } => {
                let name = name.trim().to_owned();
                if !name.is_empty() {
                    self.doc.checkpoint();
                    let _ = self.doc.part.rename(feature, name);
                }
            }
            Dialog::PartProperties(properties) => {
                if properties != self.doc.properties {
                    self.doc.properties = properties;
                    self.doc.modified = true;
                }
            }
            Dialog::Discard(action) => self.perform(action, ctx),
            Dialog::About => {}
        }
    }
}

fn sketch_window(ctx: &egui::Context, d: &mut SketchDialog) -> Answer {
    let mut answer = Answer::Open;
    window("New sketch").show(ctx, |ui| {
        egui::Grid::new("sketch_dialog")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Plane");
                egui::ComboBox::from_id_source("sketch_plane")
                    .selected_text(plane_label(&d.plane))
                    .show_ui(ui, |ui| {
                        for p in [
                            StandardPlane::Front,
                            StandardPlane::Top,
                            StandardPlane::Right,
                        ] {
                            ui.selectable_value(
                                &mut d.plane,
                                PlaneChoice::Standard(p),
                                format!("{} Plane", plane_name(p)),
                            );
                        }
                        if let Some(face) = &d.face {
                            ui.selectable_value(
                                &mut d.plane,
                                PlaneChoice::Face(face.clone()),
                                "Selected face",
                            );
                        }
                    });
                ui.end_row();

                ui.label("Shape");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut d.shape, SketchShape::Rectangle, "Rectangle");
                    ui.radio_value(&mut d.shape, SketchShape::Circle, "Circle");
                });
                ui.end_row();

                match d.shape {
                    SketchShape::Rectangle => {
                        ui.label("Corner");
                        ui.horizontal(|ui| {
                            length(ui, &mut d.x);
                            length(ui, &mut d.y);
                        });
                        ui.end_row();
                        ui.label("Size");
                        ui.horizontal(|ui| {
                            positive_length(ui, &mut d.width);
                            positive_length(ui, &mut d.height);
                        });
                        ui.end_row();
                    }
                    SketchShape::Circle => {
                        ui.label("Centre");
                        ui.horizontal(|ui| {
                            length(ui, &mut d.x);
                            length(ui, &mut d.y);
                        });
                        ui.end_row();
                        ui.label("Radius");
                        positive_length(ui, &mut d.radius);
                        ui.end_row();
                    }
                }
            });
        if matches!(d.plane, PlaneChoice::Face(_)) {
            ui.label(
                egui::RichText::new(
                    "On a face, coordinates are measured from the model origin projected onto it.",
                )
                .small()
                .weak(),
            );
        }
        answer = ok_cancel(ui, "Create");
    });
    answer
}

fn extrude_window(ctx: &egui::Context, d: &mut ExtrudeDialog) -> Answer {
    let mut answer = Answer::Open;
    let title = match d.op {
        ExtrudeOp::Boss => "Extruded Boss",
        ExtrudeOp::Cut => "Extruded Cut",
    };
    window(title).show(ctx, |ui| {
        egui::Grid::new("extrude_dialog")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Sketch");
                let current = d
                    .sketches
                    .iter()
                    .find(|(id, _)| *id == d.sketch)
                    .map_or_else(String::new, |(_, name)| name.clone());
                egui::ComboBox::from_id_source("extrude_sketch")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (id, name) in &d.sketches {
                            ui.selectable_value(&mut d.sketch, *id, name);
                        }
                    });
                ui.end_row();

                ui.label("Operation");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut d.op, ExtrudeOp::Boss, "Boss");
                    ui.radio_value(&mut d.op, ExtrudeOp::Cut, "Cut");
                });
                ui.end_row();

                ui.label("End condition");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut d.mid_plane, false, "Blind");
                    ui.radio_value(&mut d.mid_plane, true, "Mid-plane");
                });
                ui.end_row();

                ui.label("Depth");
                positive_length(ui, &mut d.depth);
                ui.end_row();
            });
        if !d.mid_plane {
            ui.checkbox(&mut d.reverse, "Reverse direction");
        }
        answer = ok_cancel(ui, "Extrude");
    });
    answer
}

fn blend_window(ctx: &egui::Context, d: &mut BlendDialog) -> Answer {
    let mut answer = Answer::Open;
    let (title, label) = if d.fillet {
        ("Fillet", "Radius")
    } else {
        ("Chamfer", "Distance")
    };
    window(title).show(ctx, |ui| {
        ui.label(format!("{} edge(s) selected", d.edges.len()));
        ui.horizontal(|ui| {
            ui.label(label);
            positive_length(ui, &mut d.size);
        });
        answer = ok_cancel(ui, title);
    });
    answer
}

fn properties_window(ctx: &egui::Context, p: &mut Properties) -> Answer {
    let mut answer = Answer::Open;
    window("Part properties").show(ctx, |ui| {
        egui::Grid::new("part_properties")
            .num_columns(2)
            .show(ui, |ui| {
                for (label, value) in [
                    ("Part number", &mut p.part_number),
                    ("Description", &mut p.description),
                    ("Revision", &mut p.revision),
                    ("Material", &mut p.material),
                ] {
                    ui.label(label);
                    ui.text_edit_singleline(value);
                    ui.end_row();
                }
            });
        if let Some(volume) = p.volume_mm3 {
            ui.label(
                egui::RichText::new(format!("Volume at last save: {volume:.1} mm³"))
                    .small()
                    .weak(),
            );
        }
        answer = ok_cancel(ui, "OK");
    });
    answer
}
