//! The open command or window (`Dialog`), applying it, and the small windows: dimension
//! value, rename, part properties, unsaved changes, about. Feature commands are drawn in the
//! PropertyManager (`property_manager`).

use eframe::egui;
use oxyde_format::Properties;
use oxyde_model::{
    EdgeBlend, EdgeName, Extrude, ExtrudeExtent, ExtrudeOp, FeatureId, FeatureKind, PlaneRef,
    StandardPlane,
};
use oxyde_sketch::{Constraint, RelationId};

use crate::app::{OxydeApp, PendingAction};
use crate::features_ui::{AxisDialog, PatternDialog, RevolveDialog, ShellDialog, SweepDialog};
use crate::sketch_mode;

/// Where an extrusion ends, as SolidWorks lists the end conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndCondition {
    Blind,
    ThroughAll,
    ThroughAllBoth,
    UpToSurface,
    MidPlane,
}

impl EndCondition {
    /// Every end condition, with its label and what it does.
    pub const ALL: [(EndCondition, &'static str, &'static str); 5] = [
        (
            EndCondition::Blind,
            "Blind",
            "A depth from the sketch plane",
        ),
        (
            EndCondition::ThroughAll,
            "Through All",
            "Through everything the part holds on that side of the sketch",
        ),
        (
            EndCondition::ThroughAllBoth,
            "Through All - Both",
            "Through everything the part holds, on both sides of the sketch",
        ),
        (
            EndCondition::UpToSurface,
            "Up To Surface",
            "Up to a flat face or a plane (extended past its edges), whichever side it is on",
        ),
        (
            EndCondition::MidPlane,
            "Mid Plane",
            "A depth in all, half on each side of the sketch plane",
        ),
    ];

    pub fn label(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(e, ..)| *e == self)
            .map_or("", |(_, label, _)| label)
    }

    /// The end condition an extrusion has.
    pub fn of(extent: &ExtrudeExtent) -> Self {
        match extent {
            ExtrudeExtent::Blind { .. } => EndCondition::Blind,
            ExtrudeExtent::MidPlane { .. } => EndCondition::MidPlane,
            ExtrudeExtent::ThroughAll { both: false } => EndCondition::ThroughAll,
            ExtrudeExtent::ThroughAll { both: true } => EndCondition::ThroughAllBoth,
            ExtrudeExtent::UpToSurface { .. } => EndCondition::UpToSurface,
        }
    }

    /// Whether it ends at a depth typed in.
    pub fn has_depth(self) -> bool {
        matches!(self, EndCondition::Blind | EndCondition::MidPlane)
    }

    /// Whether it goes one way, which can be reversed (Up To Surface goes towards its
    /// surface by itself).
    pub fn reversible(self) -> bool {
        matches!(self, EndCondition::Blind | EndCondition::ThroughAll)
    }
}

pub struct ExtrudeDialog {
    /// The feature being edited, or `None` for a new one.
    pub editing: Option<FeatureId>,
    /// Full name of the depth dimension ("D1@Boss-Extrude1"), when editing.
    pub dimension: Option<String>,
    /// The sketch to extrude; picked in the timeline.
    pub sketch: Option<FeatureId>,
    pub op: ExtrudeOp,
    pub end: EndCondition,
    /// For the end conditions with a depth.
    pub depth: f64,
    pub reverse: bool,
    /// What Up To Surface can go up to: the standard planes, the reference planes before
    /// the feature, the selected flat faces and the faces picked since.
    pub planes: crate::features_ui::Choices<PlaneRef>,
    /// The plane Up To Surface goes up to, in `planes`.
    pub up_to: Option<usize>,
    /// The Up To Surface box takes the next flat face or plane clicked.
    pub up_to_next: bool,
    /// Bodies it joins, cuts or intersects; empty = automatic.
    pub bodies: Vec<oxyde_model::BodyId>,
}

impl ExtrudeDialog {
    /// The extrusion as it stands, once a sketch (and for Up To Surface, a surface) is
    /// chosen.
    pub fn feature(&self) -> Option<FeatureKind> {
        let extent = match self.end {
            EndCondition::Blind => ExtrudeExtent::Blind { depth: self.depth },
            EndCondition::MidPlane => ExtrudeExtent::MidPlane { depth: self.depth },
            EndCondition::ThroughAll => ExtrudeExtent::ThroughAll { both: false },
            EndCondition::ThroughAllBoth => ExtrudeExtent::ThroughAll { both: true },
            EndCondition::UpToSurface => ExtrudeExtent::UpToSurface {
                plane: self.planes.get(self.up_to?)?.1.clone(),
            },
        };
        Some(FeatureKind::Extrude(Extrude {
            sketch: self.sketch?,
            op: self.op,
            extent,
            reverse: self.reverse && self.end.reversible(),
            bodies: self.bodies.clone(),
        }))
    }
}

/// A chamfer's type, as SolidWorks lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChamferType {
    /// One distance on both faces.
    Distance,
    /// A distance on each face.
    TwoDistances,
    /// A distance on one face and an angle from it.
    AngleDistance,
}

pub struct BlendDialog {
    pub editing: Option<FeatureId>,
    /// Full name of the radius or distance dimension, when editing.
    pub dimension: Option<String>,
    pub fillet: bool,
    pub size: f64,
    /// Picked in the viewport while the command is open.
    pub edges: Vec<EdgeName>,
    /// A chamfer's type, its other distance and its angle (degrees), and whether its
    /// sides swap.
    pub chamfer_type: ChamferType,
    pub second: f64,
    pub angle: f64,
    pub flip: bool,
}

impl BlendDialog {
    /// A new fillet (`fillet`) or chamfer of `edges`.
    pub fn new(fillet: bool, edges: Vec<EdgeName>) -> Self {
        Self {
            editing: None,
            dimension: None,
            fillet,
            size: if fillet { 2.0 } else { 1.0 },
            edges,
            chamfer_type: ChamferType::Distance,
            second: 2.0,
            angle: 45.0,
            flip: false,
        }
    }

    /// The chamfer shape the dialog sets.
    fn chamfer(&self) -> oxyde_model::ChamferShape {
        use oxyde_model::ChamferShape;
        match self.chamfer_type {
            ChamferType::Distance => ChamferShape::Equal,
            ChamferType::TwoDistances => ChamferShape::TwoDistances {
                distance: self.second,
                flip: self.flip,
            },
            ChamferType::AngleDistance => ChamferShape::AngleDistance {
                angle: self.angle.to_radians(),
                flip: self.flip,
            },
        }
    }

    /// The fillet or chamfer as it stands, once it has edges.
    pub fn feature(&self) -> Option<FeatureKind> {
        if self.edges.is_empty() {
            return None;
        }
        let blend = EdgeBlend {
            edges: self.edges.clone(),
            size: self.size,
            chamfer: if self.fillet {
                Default::default()
            } else {
                self.chamfer()
            },
        };
        Some(if self.fillet {
            FeatureKind::Fillet(blend)
        } else {
            FeatureKind::Chamfer(blend)
        })
    }
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
    Sweep(SweepDialog),
    Loft(crate::features_ui::LoftDialog),
    Blend(BlendDialog),
    Shell(ShellDialog),
    /// Hole Wizard.
    Hole(crate::hole_wizard::HoleDialog),
    /// Linear pattern, circular pattern or mirror.
    Pattern(PatternDialog),
    Combine(crate::features_ui::CombineDialog),
    RefPlane(crate::plane_ui::PlaneDialog),
    RefAxis(AxisDialog),
    /// Colour, transparency and texture of faces, a body, the part or a component.
    Appearance(crate::appearance_ui::AppearanceDialog),
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
    /// A file another window is editing: open read-only, take over, or not at all.
    Locked {
        path: std::path::PathBuf,
        holder: crate::lockfile::LockInfo,
        request: crate::edit_in_place::OpenRequest,
    },
    About,
}

impl Dialog {
    /// Shown in the PropertyManager rather than as a window.
    pub fn is_panel(&self) -> bool {
        matches!(
            self,
            Dialog::Extrude(_)
                | Dialog::Revolve(_)
                | Dialog::Sweep(_)
                | Dialog::Loft(_)
                | Dialog::Blend(_)
                | Dialog::Shell(_)
                | Dialog::Hole(_)
                | Dialog::Pattern(_)
                | Dialog::Combine(_)
                | Dialog::RefPlane(_)
                | Dialog::RefAxis(_)
                | Dialog::Appearance(_)
                | Dialog::ChoosePlane
        )
    }

    /// A command that adds or edits a feature of the part (and is previewed).
    pub fn is_feature(&self) -> bool {
        matches!(
            self,
            Dialog::Extrude(_)
                | Dialog::Revolve(_)
                | Dialog::Sweep(_)
                | Dialog::Loft(_)
                | Dialog::Blend(_)
                | Dialog::Shell(_)
                | Dialog::Hole(_)
                | Dialog::Pattern(_)
                | Dialog::Combine(_)
                | Dialog::RefPlane(_)
                | Dialog::RefAxis(_)
        )
    }

    /// The existing feature this command edits, if any.
    pub fn editing(&self) -> Option<FeatureId> {
        match self {
            Dialog::Extrude(d) => d.editing,
            Dialog::Revolve(d) => d.editing,
            Dialog::Sweep(d) => d.editing,
            Dialog::Loft(d) => d.editing,
            Dialog::Blend(d) => d.editing,
            Dialog::Shell(d) => d.editing,
            Dialog::Hole(d) => d.editing,
            Dialog::Pattern(d) => d.editing,
            Dialog::Combine(d) => d.editing,
            Dialog::RefPlane(d) => d.editing,
            Dialog::RefAxis(d) => d.editing,
            _ => None,
        }
    }
}

/// A bug caught (a panic in a rebuild or in a frame), shown in a window until dismissed.
pub(crate) struct InternalErrorReport {
    /// The panic's message.
    pub message: String,
    /// The change that led to it was taken back.
    pub undone: bool,
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

/// A value typed by the user: a number or arithmetic, with units (see `quantity`):
/// degrees for an angle, else millimetres.
pub(crate) fn parse_value(text: &str, angle: bool) -> Option<f64> {
    use crate::quantity::{Quantity, parse};
    parse(
        text,
        if angle {
            Quantity::Angle
        } else {
            Quantity::Length
        },
    )
}

/// The largest length a value field takes, in mm (a kilometre). Every value field has a
/// finite range: egui reads typed text with Rust's number parser, which accepts "inf" and
/// "NaN", and only a finite range brings those back to numbers (a file holding them would
/// not open again).
pub(crate) const MAX_LENGTH: f64 = 1.0e6;

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
    ui.add(
        egui::DragValue::new(value)
            .speed(0.1)
            .range(0.001..=MAX_LENGTH)
            .suffix(" mm")
            .custom_parser(length_parser),
    )
    .on_hover_text("Drag, or click and type a length: 12.5, 80/2, 1in, 3/8 in");
}

/// What a length field reads typed text with: arithmetic and units (`quantity`).
pub(crate) fn length_parser(text: &str) -> Option<f64> {
    crate::quantity::parse(text, crate::quantity::Quantity::Length)
}

/// What an angle field reads typed text with: arithmetic and units, in degrees.
pub(crate) fn angle_parser(text: &str) -> Option<f64> {
    crate::quantity::parse(text, crate::quantity::Quantity::Angle)
}

pub fn plane_name(plane: StandardPlane) -> &'static str {
    match plane {
        StandardPlane::Front => "Front",
        StandardPlane::Top => "Top",
        StandardPlane::Right => "Right",
    }
}

impl OxydeApp {
    /// Opens a command or a window in place of the one open. An Appearance panel is
    /// cancelled first: its preview is on the part, and would otherwise stay there,
    /// neither recorded for undo nor marked as a change.
    pub(crate) fn open_dialog(&mut self, dialog: Dialog) {
        self.close_dialog();
        self.dialog = Some(dialog);
    }

    /// Closes the open command or window, cancelling an Appearance panel's preview and
    /// dropping a feature command's.
    pub(crate) fn close_dialog(&mut self) {
        if let Some(Dialog::Appearance(d)) = self.dialog.take() {
            self.cancel_appearance(d);
        }
        self.dialog = None;
        self.clear_preview();
    }

    /// Tells the user about a caught bug: in the status bar, and in a window over
    /// everything (`internal_error_window`) until dismissed.
    pub(crate) fn report_internal_error(&mut self, error: crate::document::InternalError) {
        self.status = if error.undone {
            "Internal error: your last change was undone".to_owned()
        } else {
            "Internal error".to_owned()
        };
        self.internal_error = Some(InternalErrorReport {
            message: error.message,
            undone: error.undone,
        });
    }

    pub(crate) fn internal_error_window(&mut self, ctx: &egui::Context) {
        let Some(report) = &self.internal_error else {
            return;
        };
        let mut close = false;
        window("Internal error").show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.label(if report.undone {
                "Oxyde hit a bug and undid your last change: the document is as it was \
                 before it. Save it if you have not."
            } else {
                "Oxyde hit a bug. The document is still open: save it under a new name to \
                 be safe, then try again."
            });
            ui.add_space(6.0);
            ui.label(egui::RichText::new(&report.message).small().monospace());
            ui.add_space(6.0);
            ui.label("Please report it, with what you were doing.");
            ui.add_space(6.0);
            if ui.button("OK").clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape))
            {
                close = true;
            }
        });
        if close {
            self.internal_error = None;
        }
    }

    pub(crate) fn dialogs_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        if dialog.is_panel() {
            self.dialog = Some(dialog);
            return;
        }
        let mut locked_mode = None;
        let answer = match &mut dialog {
            Dialog::Extrude(_)
            | Dialog::Revolve(_)
            | Dialog::Sweep(_)
            | Dialog::Loft(_)
            | Dialog::Blend(_)
            | Dialog::Shell(_)
            | Dialog::Hole(_)
            | Dialog::Pattern(_)
            | Dialog::Combine(_)
            | Dialog::RefPlane(_)
            | Dialog::RefAxis(_)
            | Dialog::Appearance(_) => Answer::Open,
            Dialog::ChoosePlane => Answer::Open,
            Dialog::Dimension {
                target,
                label,
                name,
                text,
                angle,
                fresh,
            } => {
                let mut answer = Answer::Open;
                let sketch = &mut self.sketch;
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
                    // The sketch as the value would make it, drawn dashed over it.
                    let met = match (parse_value(text, *angle), sketch.as_mut()) {
                        (Some(typed), Some(session)) => {
                            let before = session.value_preview().to_vec();
                            let met = match target {
                                DimensionTarget::New(relation) => session.preview_new_dimension(
                                    *relation,
                                    sketch_mode::typed_value(relation, typed),
                                ),
                                DimensionTarget::Existing(id) => {
                                    match session.sketch.relation(*id).map(|r| r.constraint) {
                                        Some(c) => session.preview_set_dimension(
                                            *id,
                                            sketch_mode::typed_value(&c, typed),
                                        ),
                                        None => true,
                                    }
                                }
                            };
                            if session.value_preview() != before.as_slice() {
                                ui.ctx().request_repaint();
                            }
                            met
                        }
                        (_, session) => {
                            if let Some(session) = session {
                                session.clear_value_preview();
                            }
                            true
                        }
                    };
                    if parse_value(text, *angle).is_none() {
                        ui.label(
                            egui::RichText::new(if *angle {
                                "Type an angle: 30, 90/4, 1.2rad"
                            } else {
                                "Type a length: 12.5, 80/2, 1in, 3/8 in"
                            })
                            .small()
                            .color(crate::theme::tokens().err),
                        );
                    } else if !met {
                        ui.label(
                            egui::RichText::new("The other relations cannot meet this value")
                                .small()
                                .color(crate::theme::tokens().err),
                        );
                    }
                    answer = ok_cancel(ui, "OK");
                    if matches!(answer, Answer::Ok) && parse_value(text, *angle).is_none() {
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
            Dialog::Locked { path, holder, .. } => match Self::locked_window(ctx, path, holder) {
                None => Answer::Open,
                Some(None) => Answer::Cancel,
                Some(Some(mode)) => {
                    locked_mode = Some(mode);
                    Answer::Ok
                }
            },
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
            Answer::Ok => match (dialog, locked_mode) {
                (Dialog::Locked { request, .. }, Some(mode)) => self.open_locked(request, mode),
                (dialog, _) => self.apply_dialog(dialog, ctx),
            },
        }
    }

    pub(crate) fn apply_dialog(&mut self, dialog: Dialog, ctx: &egui::Context) {
        match dialog {
            Dialog::Extrude(d) => {
                if let Some(kind) = d.feature() {
                    self.add_or_replace(d.editing, kind);
                }
            }
            Dialog::Blend(d) => {
                if let Some(kind) = d.feature() {
                    self.add_or_replace(d.editing, kind);
                }
            }
            Dialog::Revolve(d) => self.apply_revolve(d),
            Dialog::Sweep(d) => self.apply_sweep(d),
            Dialog::Loft(d) => {
                if let Ok(kind) = d.feature() {
                    self.add_or_replace(d.editing, kind);
                }
            }
            Dialog::Shell(d) => self.apply_shell(d),
            Dialog::Hole(d) => {
                if let Some(kind) = d.feature() {
                    self.add_or_replace(d.editing, kind);
                }
            }
            Dialog::Pattern(d) => self.apply_pattern(d),
            Dialog::Combine(d) => self.apply_combine(d),
            Dialog::RefPlane(d) => self.apply_ref_plane(d),
            Dialog::RefAxis(d) => self.apply_ref_axis(d),
            Dialog::Appearance(d) => self.apply_appearance(d),
            Dialog::ChoosePlane => {}
            Dialog::Dimension {
                target,
                name,
                text,
                angle,
                ..
            } => {
                let Some(typed) = parse_value(&text, angle) else {
                    return;
                };
                if let Some(session) = self.sketch.as_mut() {
                    match target {
                        DimensionTarget::New(relation) => {
                            let value = sketch_mode::typed_value(&relation, typed);
                            session.add_dimension(relation, value);
                        }
                        DimensionTarget::Existing(id) => {
                            if let Some(name) = name {
                                session.rename_dimension(id, name.trim());
                            }
                            let Some(relation) = session.sketch.relation(id) else {
                                return;
                            };
                            let value = sketch_mode::typed_value(&relation.constraint, typed);
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
            Dialog::Locked { .. } | Dialog::About => {}
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
