//! The open command or window (`Dialog`), applying it, and the small windows: dimension
//! value, rename, part properties, unsaved changes, about. Feature commands are drawn in the
//! PropertyManager (`property_manager`).

use eframe::egui;
use oxyde_format::Properties;
use oxyde_model::{
    EdgeBlend, EdgeName, Extrude, ExtrudeExtent, ExtrudeOp, FeatureId, FeatureKind, StandardPlane,
};
use oxyde_sketch::{Constraint, RelationId};

use crate::app::{OxydeApp, PendingAction};
use crate::features_ui::{AxisDialog, PatternDialog, PlaneDialog, RevolveDialog, ShellDialog};

pub struct ExtrudeDialog {
    /// The feature being edited, or `None` for a new one.
    pub editing: Option<FeatureId>,
    /// Full name of the depth dimension ("D1@Boss-Extrude1"), when editing.
    pub dimension: Option<String>,
    /// The sketch to extrude; picked in the timeline.
    pub sketch: Option<FeatureId>,
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
    /// Picked in the viewport while the command is open.
    pub edges: Vec<EdgeName>,
}

/// Which dimension the dimension dialog sets.
pub enum DimensionTarget {
    /// A new dimension on the current sketch selection.
    New(Constraint),
    /// The value (and name) of an existing dimension.
    Existing(RelationId),
}

/// An open command or window. Feature commands (and the sketch plane prompt) show in the
/// PropertyManager (see `property_manager`); the others are small windows.
pub enum Dialog {
    Extrude(ExtrudeDialog),
    Revolve(RevolveDialog),
    Blend(BlendDialog),
    Shell(ShellDialog),
    /// Linear pattern, circular pattern or mirror.
    Pattern(PatternDialog),
    RefPlane(PlaneDialog),
    RefAxis(AxisDialog),
    /// Pick the plane of a new sketch, in the timeline or the viewport.
    ChoosePlane,
    Dimension {
        target: DimensionTarget,
        /// Window title: the dimension's full name ("D1@Sketch2"), or what is measured.
        label: String,
        /// Editable name, for existing dimensions.
        name: Option<String>,
        /// The value as typed: mm, or degrees for angles.
        text: String,
        angle: bool,
        /// First frame: focus the value with its text selected, ready to type over.
        fresh: bool,
    },
    Rename {
        feature: FeatureId,
        name: String,
    },
    PartProperties(Properties),
    Discard(PendingAction),
    About,
}

impl Dialog {
    /// Shown in the PropertyManager rather than as a window.
    pub fn is_panel(&self) -> bool {
        matches!(
            self,
            Dialog::Extrude(_)
                | Dialog::Revolve(_)
                | Dialog::Blend(_)
                | Dialog::Shell(_)
                | Dialog::Pattern(_)
                | Dialog::RefPlane(_)
                | Dialog::RefAxis(_)
                | Dialog::ChoosePlane
        )
    }
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

/// A number typed by the user; a decimal comma is accepted.
pub(crate) fn parse_value(text: &str) -> Option<f64> {
    text.trim()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

/// A value shown for editing: up to four decimals, without trailing zeros.
pub(crate) fn format_value(value: f64) -> String {
    let text = format!("{value:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// The text field of a value. On its first frame it takes the focus with its text
/// selected, so typing replaces it.
fn value_field(ui: &mut egui::Ui, text: &mut String, fresh: &mut bool) {
    let id = ui.make_persistent_id("dimension_value");
    let output = egui::TextEdit::singleline(text)
        .id(id)
        .desired_width(110.0)
        .show(ui);
    if *fresh {
        output.response.request_focus();
        let mut state = output.state;
        let end = egui::text::CCursor::new(text.chars().count());
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                end,
            )));
        state.store(ui.ctx(), id);
        *fresh = false;
    }
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
        if dialog.is_panel() {
            self.dialog = Some(dialog);
            return;
        }
        let answer = match &mut dialog {
            Dialog::Extrude(_)
            | Dialog::Revolve(_)
            | Dialog::Blend(_)
            | Dialog::Shell(_)
            | Dialog::Pattern(_)
            | Dialog::RefPlane(_)
            | Dialog::RefAxis(_) => Answer::Open,
            Dialog::ChoosePlane => Answer::Open,
            Dialog::Dimension {
                label,
                name,
                text,
                angle,
                fresh,
                ..
            } => {
                let mut answer = Answer::Open;
                window(label).show(ctx, |ui| {
                    let suffix = if *angle { "°" } else { "mm" };
                    egui::Grid::new("dimension_dialog")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label("Value");
                            ui.horizontal(|ui| {
                                value_field(ui, text, fresh);
                                ui.label(suffix);
                            });
                            ui.end_row();
                            if let Some(name) = name {
                                ui.label("Name");
                                ui.text_edit_singleline(name)
                                    .on_hover_text("Used in equations, e.g. Width@Sketch1");
                                ui.end_row();
                            }
                        });
                    if parse_value(text).is_none() {
                        ui.label(
                            egui::RichText::new("Type a number")
                                .small()
                                .color(crate::theme::tokens().err),
                        );
                    }
                    answer = ok_cancel(ui, "OK");
                    if matches!(answer, Answer::Ok) && parse_value(text).is_none() {
                        answer = Answer::Open;
                    }
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
                let name = match &self.asm {
                    Some(asm) => asm.display_name(),
                    None => self.doc.display_name(),
                };
                window("Unsaved changes").show(ctx, |ui| {
                    ui.label(format!("Save the changes to {name} first?"));
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
                let saved = if self.asm.is_some() {
                    save && self.save_assembly(false)
                } else {
                    save && self.save()
                };
                if save && !saved {
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
                    ui.label(format!("Viewport: {}", self.viewport.renderer_name()));
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

    pub(crate) fn apply_dialog(&mut self, dialog: Dialog, ctx: &egui::Context) {
        match dialog {
            Dialog::Extrude(d) => {
                let Some(sketch) = d.sketch else {
                    return;
                };
                let extent = if d.mid_plane {
                    ExtrudeExtent::MidPlane { depth: d.depth }
                } else {
                    ExtrudeExtent::Blind { depth: d.depth }
                };
                let kind = FeatureKind::Extrude(Extrude {
                    sketch,
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
                let blend = EdgeBlend {
                    edges: d.edges,
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
                text,
                angle,
                ..
            } => {
                let Some(value) = parse_value(&text) else {
                    return;
                };
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
