//! Modal windows: extrude, fillet/chamfer, sketch plane, dimension value, rename, part
//! properties, unsaved changes.

use eframe::egui;
use oxyde_format::Properties;
use oxyde_model::{
    EdgeBlend, EdgeName, Extrude, ExtrudeExtent, ExtrudeOp, FeatureId, FeatureKind, SketchPlane,
    StandardPlane,
};
use oxyde_sketch::{Constraint, RelationId};

use crate::app::{OxydeApp, PendingAction};
use crate::features_ui::{
    self, AxisDialog, PatternDialog, PlaneDialog, RevolveDialog, ShellDialog,
};

pub struct ExtrudeDialog {
    /// The feature being edited, or `None` for a new one.
    pub editing: Option<FeatureId>,
    /// Full name of the depth dimension ("D1@Boss-Extrude1"), when editing.
    pub dimension: Option<String>,
    pub sketch: FeatureId,
    pub sketches: Vec<(FeatureId, String)>,
    pub op: ExtrudeOp,
    pub depth: f64,
    pub mid_plane: bool,
    pub reverse: bool,
}

pub struct BlendDialog {
    pub editing: Option<FeatureId>,
    /// Full name of the radius or distance dimension, when editing.
    pub dimension: Option<String>,
    pub fillet: bool,
    pub size: f64,
    pub edges: Vec<EdgeName>,
    /// Edges selected in the viewport when the dialog opened (editing only).
    pub selected_edges: Vec<EdgeName>,
    pub use_selection: bool,
}

/// Which dimension the dimension dialog sets.
pub enum DimensionTarget {
    /// A new dimension on the current sketch selection.
    New(Constraint),
    /// The value (and name) of an existing dimension.
    Existing(RelationId),
}

pub enum Dialog {
    Extrude(ExtrudeDialog),
    Revolve(RevolveDialog),
    Blend(BlendDialog),
    Shell(ShellDialog),
    /// Linear pattern, circular pattern or mirror.
    Pattern(PatternDialog),
    RefPlane(PlaneDialog),
    RefAxis(AxisDialog),
    /// Pick the plane of a new sketch.
    ChoosePlane,
    Dimension {
        target: DimensionTarget,
        /// Window title: the dimension's full name ("D1@Sketch2"), or what is measured.
        label: String,
        /// Editable name, for existing dimensions.
        name: Option<String>,
        /// mm, or degrees for angles.
        value: f64,
        angle: bool,
    },
    Rename {
        feature: FeatureId,
        name: String,
    },
    PartProperties(Properties),
    Discard(PendingAction),
    About,
}

/// User's answer to a dialog this frame.
pub(crate) enum Answer {
    Open,
    Ok,
    Cancel,
}

pub(crate) fn window(title: &str) -> egui::Window<'static> {
    egui::Window::new(title.to_owned())
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
}

pub(crate) fn ok_cancel(ui: &mut egui::Ui, ok_label: &str) -> Answer {
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
    // Enter confirms, Escape cancels.
    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        answer = Answer::Ok;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        answer = Answer::Cancel;
    }
    answer
}

pub(crate) fn positive_length(ui: &mut egui::Ui, value: &mut f64) {
    ui.add(egui::DragValue::new(value).speed(0.1).suffix(" mm"));
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

impl OxydeApp {
    pub(crate) fn dialogs_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let answer = match &mut dialog {
            Dialog::Extrude(d) => extrude_window(ctx, d),
            Dialog::Revolve(d) => {
                let answer = features_ui::revolve_window(ctx, d);
                self.refresh_revolve_axes(d);
                answer
            }
            Dialog::Blend(d) => blend_window(ctx, d),
            Dialog::Shell(d) => features_ui::shell_window(ctx, d),
            Dialog::Pattern(d) => features_ui::pattern_window(ctx, d),
            Dialog::RefPlane(d) => features_ui::ref_plane_window(ctx, d),
            Dialog::RefAxis(d) => features_ui::ref_axis_window(ctx, d),
            Dialog::ChoosePlane => {
                let mut answer = Answer::Open;
                let mut chosen = None;
                let references: Vec<(FeatureId, String)> = self
                    .plane_choices(None)
                    .into_iter()
                    .filter_map(|(label, plane)| match plane {
                        oxyde_model::PlaneRef::Reference(id) => Some((id, label)),
                        _ => None,
                    })
                    .collect();
                window("New sketch").show(ctx, |ui| {
                    ui.label("Sketch on which plane?");
                    ui.label(
                        egui::RichText::new("Tip: select a flat face first to sketch on it.")
                            .small()
                            .weak(),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        for plane in [
                            StandardPlane::Front,
                            StandardPlane::Top,
                            StandardPlane::Right,
                        ] {
                            if ui.button(format!("{} Plane", plane_name(plane))).clicked() {
                                chosen = Some(SketchPlane::Standard(plane));
                            }
                        }
                    });
                    if !references.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            for (id, name) in &references {
                                if ui.button(name).clicked() {
                                    chosen = Some(SketchPlane::Reference(*id));
                                }
                            }
                        });
                    }
                    ui.add_space(4.0);
                    if ui.button("Cancel").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        answer = Answer::Cancel;
                    }
                });
                if let Some(plane) = chosen {
                    self.start_new_sketch(plane);
                    answer = Answer::Cancel; // already handled
                }
                answer
            }
            Dialog::Dimension {
                label,
                name,
                value,
                angle,
                ..
            } => {
                let mut answer = Answer::Open;
                window(label).show(ctx, |ui| {
                    let suffix = if *angle { "°" } else { " mm" };
                    egui::Grid::new("dimension_dialog")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label("Value");
                            let response =
                                ui.add(egui::DragValue::new(value).speed(0.1).suffix(suffix));
                            if name.is_none() {
                                response.request_focus();
                            }
                            ui.end_row();
                            if let Some(name) = name {
                                ui.label("Name");
                                ui.text_edit_singleline(name)
                                    .on_hover_text("Used in equations, e.g. Width@Sketch1");
                                ui.end_row();
                            }
                        });
                    answer = ok_cancel(ui, "OK");
                });
                answer
            }
            Dialog::Rename { name, .. } => {
                let mut answer = Answer::Open;
                window("Rename feature").show(ctx, |ui| {
                    ui.text_edit_singleline(name).request_focus();
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
            Answer::Open => {
                // A handler may have opened another dialog meanwhile; keep the newer one.
                if self.dialog.is_none() {
                    self.dialog = Some(dialog);
                }
            }
            Answer::Cancel => {}
            Answer::Ok => self.apply_dialog(dialog, ctx),
        }
    }

    fn apply_dialog(&mut self, dialog: Dialog, ctx: &egui::Context) {
        match dialog {
            Dialog::Extrude(d) => {
                let extent = if d.mid_plane {
                    ExtrudeExtent::MidPlane { depth: d.depth }
                } else {
                    ExtrudeExtent::Blind { depth: d.depth }
                };
                let kind = FeatureKind::Extrude(Extrude {
                    sketch: d.sketch,
                    op: d.op,
                    extent,
                    reverse: d.reverse && !d.mid_plane,
                });
                match d.editing {
                    Some(id) => self.replace_feature(id, kind),
                    None => {
                        self.add_feature(kind);
                    }
                }
            }
            Dialog::Blend(d) => {
                let edges = if d.use_selection && !d.selected_edges.is_empty() {
                    d.selected_edges
                } else {
                    d.edges
                };
                let blend = EdgeBlend {
                    edges,
                    size: d.size,
                };
                let kind = if d.fillet {
                    FeatureKind::Fillet(blend)
                } else {
                    FeatureKind::Chamfer(blend)
                };
                match d.editing {
                    Some(id) => self.replace_feature(id, kind),
                    None => {
                        self.add_feature(kind);
                    }
                }
            }
            Dialog::Revolve(d) => self.apply_revolve(d),
            Dialog::Shell(d) => self.apply_shell(d),
            Dialog::Pattern(d) => self.apply_pattern(d),
            Dialog::RefPlane(d) => self.apply_ref_plane(d),
            Dialog::RefAxis(d) => self.apply_ref_axis(d),
            Dialog::ChoosePlane => {}
            Dialog::Dimension {
                target,
                name,
                value,
                angle,
                ..
            } => {
                let value = if angle { value.to_radians() } else { value };
                if let Some(session) = self.sketch.as_mut() {
                    match target {
                        DimensionTarget::New(relation) => {
                            session.add_dimension(relation, value);
                        }
                        DimensionTarget::Existing(id) => {
                            if let Some(name) = name {
                                session.rename_dimension(id, name.trim());
                            }
                            session.set_dimension(id, value);
                        }
                    }
                }
            }
            Dialog::Rename { feature, name } => {
                let name = name.trim().to_owned();
                if let Err(e) = self.rename_feature(feature, name) {
                    self.status = e.to_string();
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
                ui.horizontal(|ui| {
                    positive_length(ui, &mut d.depth);
                    if let Some(name) = &d.dimension {
                        ui.label(egui::RichText::new(name).small().weak());
                    }
                });
                ui.end_row();
            });
        if !d.mid_plane {
            ui.checkbox(&mut d.reverse, "Reverse direction");
        }
        answer = ok_cancel(
            ui,
            if d.editing.is_some() {
                "Apply"
            } else {
                "Extrude"
            },
        );
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
        ui.label(format!("{} edge(s)", d.edges.len()));
        if d.editing.is_some() && !d.selected_edges.is_empty() {
            ui.checkbox(
                &mut d.use_selection,
                format!(
                    "Use the {} edge(s) selected now instead",
                    d.selected_edges.len()
                ),
            );
        }
        ui.horizontal(|ui| {
            ui.label(label);
            positive_length(ui, &mut d.size);
            if let Some(name) = &d.dimension {
                ui.label(egui::RichText::new(name).small().weak());
            }
        });
        answer = ok_cancel(ui, if d.editing.is_some() { "Apply" } else { title });
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
