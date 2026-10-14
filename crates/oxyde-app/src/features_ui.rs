//! Windows of the features added in M3–M5: revolve, shell, reference plane and axis,
//! linear and circular patterns, mirror. Each opens for a new feature (from the command
//! bar) or for an existing one (double-click in the timeline).
//!
//! Planes, axes and directions are picked from lists: the standard planes and axes, the
//! reference planes and axes before the feature, and whatever suitable faces and edges are
//! selected in the viewport.

use eframe::egui;
use oxyde_model::{
    AxisRef, CircularPattern, ExtrudeOp, FaceName, Feature, FeatureId, FeatureKind,
    FeatureParameter, LinearPattern, Mirror, PatternDirection, PlaneRef, RefAxis, RefPlane,
    Revolve, RevolveAxis, Shell, StandardAxis, StandardPlane,
};
use oxyde_sketch::Geometry;

use crate::app::{OxydeApp, Selected};
use crate::dialogs::{Answer, Dialog, ok_cancel, plane_name, positive_length, window};

/// Labelled choices for a combo box.
pub type Choices<T> = Vec<(String, T)>;

pub struct RevolveDialog {
    pub editing: Option<FeatureId>,
    /// Full name of the angle dimension, when editing.
    pub dimension: Option<String>,
    pub sketch: FeatureId,
    pub sketches: Vec<(FeatureId, String)>,
    pub op: ExtrudeOp,
    pub axes: Choices<RevolveAxis>,
    pub axis: usize,
    /// The sketch `axes` were listed for.
    pub axes_for: FeatureId,
    /// Degrees.
    pub angle: f64,
    pub mid_plane: bool,
    pub reverse: bool,
}

pub struct ShellDialog {
    pub editing: Option<FeatureId>,
    pub dimension: Option<String>,
    pub faces: Vec<FaceName>,
    /// Faces selected in the viewport when the window opened (editing only).
    pub selected_faces: Vec<FaceName>,
    pub use_selection: bool,
    pub thickness: f64,
    pub outward: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    Linear,
    Circular,
    Mirror,
}

pub struct PatternDialog {
    pub editing: Option<FeatureId>,
    pub kind: PatternKind,
    /// Full names of the feature's dimensions, when editing.
    pub dimensions: Vec<String>,
    /// Features that can be copied.
    pub candidates: Vec<(FeatureId, String)>,
    pub seeds: Vec<FeatureId>,
    pub axes: Choices<AxisRef>,
    pub axis: usize,
    pub spacing: f64,
    pub count: u32,
    pub reverse: bool,
    pub second: bool,
    pub axis2: usize,
    pub spacing2: f64,
    pub count2: u32,
    pub reverse2: bool,
    /// Degrees.
    pub angle: f64,
    pub equal_spacing: bool,
    pub planes: Choices<PlaneRef>,
    pub plane: usize,
}

pub struct PlaneDialog {
    pub editing: Option<FeatureId>,
    pub dimension: Option<String>,
    pub planes: Choices<PlaneRef>,
    pub base: usize,
    pub offset: f64,
    pub flip: bool,
}

pub struct AxisDialog {
    pub editing: Option<FeatureId>,
    pub two_planes: bool,
    pub axes: Choices<AxisRef>,
    pub axis: usize,
    pub planes: Choices<PlaneRef>,
    pub plane_a: usize,
    pub plane_b: usize,
}

fn combo<T>(ui: &mut egui::Ui, id: &str, choices: &[(String, T)], selected: &mut usize) {
    let text = choices
        .get(*selected)
        .map_or("(none available)", |(label, _)| label.as_str());
    egui::ComboBox::from_id_source(id)
        .selected_text(text)
        .width(200.0)
        .show_ui(ui, |ui| {
            for (i, (label, _)) in choices.iter().enumerate() {
                ui.selectable_value(selected, i, label);
            }
        });
}

fn degrees(ui: &mut egui::Ui, value: &mut f64) {
    ui.add(egui::DragValue::new(value).speed(1.0).suffix("°"));
    *value = value.clamp(0.01, 360.0);
}

fn count(ui: &mut egui::Ui, value: &mut u32) {
    ui.add(egui::DragValue::new(value).speed(0.1));
    *value = (*value).clamp(1, 1000);
}

fn dimension_note(ui: &mut egui::Ui, name: Option<&String>) {
    if let Some(name) = name {
        ui.label(egui::RichText::new(name).small().weak());
    }
}

/// Position of `value` in `choices`, or 0.
fn position<T: PartialEq>(choices: &[(String, T)], value: Option<&T>) -> usize {
    value
        .and_then(|v| choices.iter().position(|(_, c)| c == v))
        .unwrap_or(0)
}

/// Puts the current value first in the list if it is not there yet (a face or an edge
/// picked earlier and not selected now).
fn with_current<T: PartialEq + Clone>(mut choices: Choices<T>, current: Option<&T>) -> Choices<T> {
    if let Some(current) = current {
        if !choices.iter().any(|(_, c)| c == current) {
            choices.insert(0, ("Current reference".to_owned(), current.clone()));
        }
    }
    choices
}

impl OxydeApp {
    // ---- Choices -----------------------------------------------------------------------------

    /// Features a new or edited feature may refer to: those before it.
    fn earlier(&self, editing: Option<FeatureId>) -> &[Feature] {
        let features = self.doc.part.features();
        let end = match editing.and_then(|id| self.doc.part.index_of(id)) {
            Some(index) => index,
            None => self.doc.part.rollback_position().unwrap_or(features.len()),
        };
        &features[..end.min(features.len())]
    }

    pub(crate) fn plane_choices(&self, editing: Option<FeatureId>) -> Choices<PlaneRef> {
        let mut out: Choices<PlaneRef> = [
            StandardPlane::Front,
            StandardPlane::Top,
            StandardPlane::Right,
        ]
        .into_iter()
        .map(|p| (format!("{} Plane", plane_name(p)), PlaneRef::Standard(p)))
        .collect();
        for feature in self.earlier(editing) {
            if matches!(feature.kind(), FeatureKind::Plane(_)) {
                out.push((feature.name().to_owned(), PlaneRef::Reference(feature.id())));
            }
        }
        if let Some(body) = self.doc.part.body() {
            let mut n = 0;
            for selected in &self.selection {
                if let Selected::Face(name) = selected {
                    let planar = body
                        .naming
                        .find_face(name)
                        .and_then(|i| body.topology.faces.get(i as usize))
                        .is_some_and(|f| f.normal.is_some());
                    if planar {
                        n += 1;
                        out.push((format!("Selected face {n}"), PlaneRef::Face(name.clone())));
                    }
                }
            }
        }
        out
    }

    pub(crate) fn axis_choices(&self, editing: Option<FeatureId>) -> Choices<AxisRef> {
        let mut out: Choices<AxisRef> = [
            ("X axis", StandardAxis::X),
            ("Y axis", StandardAxis::Y),
            ("Z axis", StandardAxis::Z),
        ]
        .into_iter()
        .map(|(label, axis)| (label.to_owned(), AxisRef::Standard(axis)))
        .collect();
        for feature in self.earlier(editing) {
            if matches!(feature.kind(), FeatureKind::Axis(_)) {
                out.push((feature.name().to_owned(), AxisRef::Reference(feature.id())));
            }
        }
        if let Some(body) = self.doc.part.body() {
            let (mut edges, mut faces) = (0, 0);
            for selected in &self.selection {
                match selected {
                    Selected::Edge(name) => {
                        let usable = body
                            .naming
                            .find_edge(name)
                            .and_then(|i| body.topology.edges.get(i as usize))
                            .is_some_and(|e| e.axis.is_some());
                        if usable {
                            edges += 1;
                            out.push((
                                format!("Selected edge {edges}"),
                                AxisRef::Edge(name.clone()),
                            ));
                        }
                    }
                    Selected::Face(name) => {
                        let usable = body
                            .naming
                            .find_face(name)
                            .and_then(|i| body.topology.faces.get(i as usize))
                            .is_some_and(|f| f.axis.is_some() || f.normal.is_some());
                        if usable {
                            faces += 1;
                            out.push((
                                format!("Selected face {faces}"),
                                AxisRef::Face(name.clone()),
                            ));
                        }
                    }
                }
            }
        }
        out
    }

    /// Axes a revolve of `sketch` can turn about: the sketch's lines (centerlines first),
    /// then the other axes.
    fn revolve_axis_choices(
        &self,
        sketch: FeatureId,
        editing: Option<FeatureId>,
    ) -> Choices<RevolveAxis> {
        let mut out = Vec::new();
        if let Some(FeatureKind::Sketch(s)) = self.doc.part.feature(sketch).map(|f| f.kind()) {
            let mut lines: Vec<(bool, u32)> = s
                .geometry
                .entities()
                .iter()
                .filter(|e| matches!(e.geometry, Geometry::Line { .. }))
                .map(|e| (!e.construction, e.id.0))
                .collect();
            lines.sort();
            for (solid, id) in lines {
                let label = if solid {
                    format!("Line {id}")
                } else {
                    format!("Centerline {id}")
                };
                out.push((label, RevolveAxis::SketchLine(id)));
            }
        }
        out.extend(
            self.axis_choices(editing)
                .into_iter()
                .map(|(label, axis)| (label, RevolveAxis::Axis(axis))),
        );
        out
    }

    fn seed_choices(&self, editing: Option<FeatureId>) -> Vec<(FeatureId, String)> {
        self.earlier(editing)
            .iter()
            .filter(|f| f.kind().is_patternable())
            .map(|f| (f.id(), f.name().to_owned()))
            .collect()
    }

    fn selected_faces(&self) -> Vec<FaceName> {
        self.selection
            .iter()
            .filter_map(|s| match s {
                Selected::Face(f) => Some(f.clone()),
                Selected::Edge(_) => None,
            })
            .collect()
    }

    fn dimension_names(&self, id: FeatureId) -> Vec<String> {
        let Some(feature) = self.doc.part.feature(id) else {
            return Vec::new();
        };
        feature
            .kind()
            .parameters()
            .into_iter()
            .filter_map(|(p, _)| self.parameter_full_name(id, p))
            .collect()
    }

    // ---- Opening the windows -----------------------------------------------------------------

    pub(crate) fn open_revolve(
        &mut self,
        editing: Option<FeatureId>,
        sketch: FeatureId,
        op: ExtrudeOp,
        existing: Option<&Revolve>,
    ) {
        let axes = self.revolve_axis_choices(sketch, editing);
        let axes = with_current(axes, existing.map(|r| &r.axis));
        let axis = position(&axes, existing.map(|r| &r.axis));
        self.dialog = Some(Dialog::Revolve(RevolveDialog {
            editing,
            dimension: editing.and_then(|id| self.parameter_full_name(id, FeatureParameter::Angle)),
            sketch,
            sketches: self.sketches(),
            op,
            axes,
            axis,
            axes_for: sketch,
            angle: existing.map_or(360.0, |r| r.angle.to_degrees()),
            mid_plane: existing.is_some_and(|r| r.mid_plane),
            reverse: existing.is_some_and(|r| r.reverse),
        }));
    }

    pub(crate) fn begin_revolve(&mut self, op: ExtrudeOp) {
        let selected = self.selected_feature.filter(|id| {
            self.doc
                .part
                .feature(*id)
                .is_some_and(|f| matches!(f.kind(), FeatureKind::Sketch(_)))
        });
        match selected.or_else(|| self.sketches().last().map(|(id, _)| *id)) {
            Some(sketch) => self.open_revolve(None, sketch, op, None),
            None => self.status = "Create a sketch first".to_owned(),
        }
    }

    pub(crate) fn open_shell(&mut self, editing: Option<FeatureId>, existing: Option<&Shell>) {
        let selected = self.selected_faces();
        let (faces, selected_faces) = match existing {
            Some(shell) => (shell.faces.clone(), selected),
            None => (selected, Vec::new()),
        };
        self.dialog = Some(Dialog::Shell(ShellDialog {
            editing,
            dimension: editing
                .and_then(|id| self.parameter_full_name(id, FeatureParameter::Thickness)),
            faces,
            selected_faces,
            use_selection: false,
            thickness: existing.map_or(2.0, |s| s.thickness),
            outward: existing.is_some_and(|s| s.outward),
        }));
    }

    pub(crate) fn begin_shell(&mut self) {
        if self.doc.part.body().is_none() {
            self.status = "There is nothing to shell yet".to_owned();
            return;
        }
        self.open_shell(None, None);
    }

    pub(crate) fn open_pattern(
        &mut self,
        editing: Option<FeatureId>,
        kind: PatternKind,
        existing: Option<&FeatureKind>,
    ) {
        let candidates = self.seed_choices(editing);
        let axes = self.axis_choices(editing);
        let planes = self.plane_choices(editing);
        let mut d = PatternDialog {
            editing,
            kind,
            dimensions: editing
                .map(|id| self.dimension_names(id))
                .unwrap_or_default(),
            // A new pattern copies the selected feature if it can, else the last one.
            seeds: self
                .selected_feature
                .filter(|id| candidates.iter().any(|(c, _)| c == id))
                .or_else(|| candidates.last().map(|(id, _)| *id))
                .into_iter()
                .collect(),
            candidates,
            axis: 0,
            spacing: 20.0,
            count: 3,
            reverse: false,
            second: false,
            axis2: 1,
            spacing2: 20.0,
            count2: 2,
            reverse2: false,
            angle: 360.0,
            equal_spacing: true,
            plane: 0,
            axes,
            planes,
        };
        // A selected edge or face is the most likely direction.
        if editing.is_none() {
            if let Some(i) = d
                .axes
                .iter()
                .position(|(_, a)| matches!(a, AxisRef::Edge(_) | AxisRef::Face(_)))
            {
                d.axis = i;
            }
            if let Some(i) = d
                .planes
                .iter()
                .position(|(_, p)| matches!(p, PlaneRef::Face(_)))
            {
                d.plane = i;
            }
        }
        match existing {
            Some(FeatureKind::LinearPattern(p)) => {
                d.seeds = p.seeds.clone();
                d.axes = with_current(d.axes, Some(&p.first.axis));
                if let Some(second) = &p.second {
                    d.axes = with_current(d.axes, Some(&second.axis));
                }
                d.axis = position(&d.axes, Some(&p.first.axis));
                d.spacing = p.first.spacing;
                d.count = p.first.count;
                d.reverse = p.first.reverse;
                if let Some(second) = &p.second {
                    d.second = true;
                    d.axis2 = position(&d.axes, Some(&second.axis));
                    d.spacing2 = second.spacing;
                    d.count2 = second.count;
                    d.reverse2 = second.reverse;
                }
            }
            Some(FeatureKind::CircularPattern(p)) => {
                d.seeds = p.seeds.clone();
                d.axes = with_current(d.axes, Some(&p.axis));
                d.axis = position(&d.axes, Some(&p.axis));
                d.angle = p.angle.to_degrees();
                d.count = p.count;
                d.equal_spacing = p.equal_spacing;
                d.reverse = p.reverse;
            }
            Some(FeatureKind::Mirror(m)) => {
                d.seeds = m.seeds.clone();
                d.planes = with_current(d.planes, Some(&m.plane));
                d.plane = position(&d.planes, Some(&m.plane));
            }
            _ => {}
        }
        if kind == PatternKind::Circular && editing.is_none() {
            d.count = 4;
            // Around a selected round edge or face, or the selected axis.
            if let Some(i) = d.axes.iter().position(|(_, a)| match a {
                AxisRef::Reference(id) => self.selected_feature == Some(*id),
                _ => false,
            }) {
                d.axis = i;
            }
        }
        self.dialog = Some(Dialog::Pattern(d));
    }

    pub(crate) fn begin_pattern(&mut self, kind: PatternKind) {
        if self.seed_choices(None).is_empty() {
            self.status =
                "Patterns and mirrors copy extrusions and revolves: add one first".to_owned();
            return;
        }
        self.open_pattern(None, kind, None);
    }

    pub(crate) fn open_ref_plane(
        &mut self,
        editing: Option<FeatureId>,
        existing: Option<&RefPlane>,
    ) {
        let planes = with_current(self.plane_choices(editing), existing.map(|p| &p.base));
        let mut base = position(&planes, existing.map(|p| &p.base));
        if existing.is_none() {
            // Offset from the selected face, or the selected plane.
            if let Some(i) = planes.iter().position(|(_, p)| match p {
                PlaneRef::Face(_) => true,
                PlaneRef::Standard(s) => self.selected_plane == Some(*s),
                PlaneRef::Reference(id) => self.selected_feature == Some(*id),
            }) {
                base = i;
            }
        }
        self.dialog = Some(Dialog::RefPlane(PlaneDialog {
            editing,
            dimension: editing
                .and_then(|id| self.parameter_full_name(id, FeatureParameter::Offset)),
            planes,
            base,
            offset: existing.map_or(10.0, |p| p.offset),
            flip: existing.is_some_and(|p| p.flip),
        }));
    }

    pub(crate) fn open_ref_axis(&mut self, editing: Option<FeatureId>, existing: Option<&RefAxis>) {
        let mut axes = self.axis_choices(editing);
        let mut planes = self.plane_choices(editing);
        let (mut two_planes, mut axis, mut plane_a, mut plane_b) = (false, 0, 0, 1);
        match existing {
            Some(RefAxis::Along(a)) => {
                axes = with_current(axes, Some(a));
                axis = position(&axes, Some(a));
            }
            Some(RefAxis::TwoPlanes(a, b)) => {
                two_planes = true;
                planes = with_current(planes, Some(a));
                planes = with_current(planes, Some(b));
                plane_a = position(&planes, Some(a));
                plane_b = position(&planes, Some(b));
            }
            None => {
                if let Some(i) = axes
                    .iter()
                    .position(|(_, a)| matches!(a, AxisRef::Edge(_) | AxisRef::Face(_)))
                {
                    axis = i;
                }
            }
        }
        self.dialog = Some(Dialog::RefAxis(AxisDialog {
            editing,
            two_planes,
            axes,
            axis,
            planes,
            plane_a,
            plane_b,
        }));
    }

    // ---- Applying ----------------------------------------------------------------------------

    fn add_or_replace(&mut self, editing: Option<FeatureId>, kind: FeatureKind) {
        match editing {
            Some(id) => self.replace_feature(id, kind),
            None => {
                self.add_feature(kind);
            }
        }
    }

    pub(crate) fn apply_revolve(&mut self, d: RevolveDialog) {
        let Some((_, axis)) = d.axes.get(d.axis) else {
            self.status = "Choose an axis to revolve about".to_owned();
            return;
        };
        let kind = FeatureKind::Revolve(Revolve {
            sketch: d.sketch,
            op: d.op,
            axis: axis.clone(),
            angle: d.angle.to_radians(),
            mid_plane: d.mid_plane,
            reverse: d.reverse,
        });
        self.add_or_replace(d.editing, kind);
    }

    pub(crate) fn apply_shell(&mut self, d: ShellDialog) {
        let faces = if d.use_selection && !d.selected_faces.is_empty() {
            d.selected_faces
        } else {
            d.faces
        };
        let kind = FeatureKind::Shell(Shell {
            faces,
            thickness: d.thickness,
            outward: d.outward,
        });
        self.add_or_replace(d.editing, kind);
    }

    pub(crate) fn apply_pattern(&mut self, d: PatternDialog) {
        if d.seeds.is_empty() {
            self.status = "Choose at least one feature to copy".to_owned();
            return;
        }
        // Seeds in timeline order.
        let mut seeds = d.seeds.clone();
        seeds.sort_by_key(|id| self.doc.part.index_of(*id));
        let axis = |i: usize| d.axes.get(i).map(|(_, a)| a.clone());
        let kind = match d.kind {
            PatternKind::Linear => {
                let Some(first) = axis(d.axis) else {
                    self.status = "Choose a direction".to_owned();
                    return;
                };
                let second = if d.second {
                    let Some(axis2) = axis(d.axis2) else {
                        self.status = "Choose the second direction".to_owned();
                        return;
                    };
                    Some(PatternDirection {
                        axis: axis2,
                        spacing: d.spacing2,
                        count: d.count2,
                        reverse: d.reverse2,
                    })
                } else {
                    None
                };
                FeatureKind::LinearPattern(LinearPattern {
                    seeds,
                    first: PatternDirection {
                        axis: first,
                        spacing: d.spacing,
                        count: d.count,
                        reverse: d.reverse,
                    },
                    second,
                })
            }
            PatternKind::Circular => {
                let Some(axis) = axis(d.axis) else {
                    self.status = "Choose an axis".to_owned();
                    return;
                };
                FeatureKind::CircularPattern(CircularPattern {
                    seeds,
                    axis,
                    angle: d.angle.to_radians(),
                    count: d.count,
                    equal_spacing: d.equal_spacing,
                    reverse: d.reverse,
                })
            }
            PatternKind::Mirror => {
                let Some((_, plane)) = d.planes.get(d.plane) else {
                    self.status = "Choose a mirror plane".to_owned();
                    return;
                };
                FeatureKind::Mirror(Mirror {
                    seeds,
                    plane: plane.clone(),
                })
            }
        };
        self.add_or_replace(d.editing, kind);
    }

    pub(crate) fn apply_ref_plane(&mut self, d: PlaneDialog) {
        let Some((_, base)) = d.planes.get(d.base) else {
            return;
        };
        let kind = FeatureKind::Plane(RefPlane {
            base: base.clone(),
            offset: d.offset,
            flip: d.flip,
        });
        self.add_or_replace(d.editing, kind);
    }

    pub(crate) fn apply_ref_axis(&mut self, d: AxisDialog) {
        let axis = if d.two_planes {
            match (d.planes.get(d.plane_a), d.planes.get(d.plane_b)) {
                (Some((_, a)), Some((_, b))) => RefAxis::TwoPlanes(a.clone(), b.clone()),
                _ => return,
            }
        } else {
            match d.axes.get(d.axis) {
                Some((_, a)) => RefAxis::Along(a.clone()),
                None => return,
            }
        };
        self.add_or_replace(d.editing, FeatureKind::Axis(axis));
    }

    /// Keeps the revolve axis list in step with the chosen sketch.
    pub(crate) fn refresh_revolve_axes(&self, d: &mut RevolveDialog) {
        if d.axes_for != d.sketch {
            d.axes = self.revolve_axis_choices(d.sketch, d.editing);
            d.axis = 0;
            d.axes_for = d.sketch;
        }
    }
}

// ---- Windows ---------------------------------------------------------------------------------

fn apply_label(editing: Option<FeatureId>, new: &'static str) -> &'static str {
    if editing.is_some() { "Apply" } else { new }
}

pub(crate) fn revolve_window(ctx: &egui::Context, d: &mut RevolveDialog) -> Answer {
    let mut answer = Answer::Open;
    let title = match d.op {
        ExtrudeOp::Boss => "Revolved Boss",
        ExtrudeOp::Cut => "Revolved Cut",
    };
    window(title).show(ctx, |ui| {
        egui::Grid::new("revolve_dialog")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Sketch");
                let current = d
                    .sketches
                    .iter()
                    .find(|(id, _)| *id == d.sketch)
                    .map_or_else(String::new, |(_, name)| name.clone());
                egui::ComboBox::from_id_source("revolve_sketch")
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

                ui.label("Axis");
                combo(ui, "revolve_axis", &d.axes, &mut d.axis);
                ui.end_row();

                ui.label("Angle");
                ui.horizontal(|ui| {
                    degrees(ui, &mut d.angle);
                    dimension_note(ui, d.dimension.as_ref());
                });
                ui.end_row();
            });
        ui.checkbox(&mut d.mid_plane, "Mid-plane (half on each side)");
        if !d.mid_plane {
            ui.checkbox(&mut d.reverse, "Reverse direction");
        }
        ui.label(
            egui::RichText::new("Tip: draw a construction centerline in the sketch as the axis.")
                .small()
                .weak(),
        );
        answer = ok_cancel(ui, apply_label(d.editing, "Revolve"));
    });
    answer
}

pub(crate) fn shell_window(ctx: &egui::Context, d: &mut ShellDialog) -> Answer {
    let mut answer = Answer::Open;
    window("Shell").show(ctx, |ui| {
        if d.faces.is_empty() {
            ui.label("No face removed: the part becomes a closed hollow body.");
        } else {
            ui.label(format!("{} face(s) removed", d.faces.len()));
        }
        if d.editing.is_some() && !d.selected_faces.is_empty() {
            ui.checkbox(
                &mut d.use_selection,
                format!(
                    "Remove the {} face(s) selected now instead",
                    d.selected_faces.len()
                ),
            );
        }
        ui.horizontal(|ui| {
            ui.label("Thickness");
            positive_length(ui, &mut d.thickness);
            dimension_note(ui, d.dimension.as_ref());
        });
        ui.checkbox(&mut d.outward, "Shell outward");
        answer = ok_cancel(ui, apply_label(d.editing, "Shell"));
    });
    answer
}

pub(crate) fn pattern_window(ctx: &egui::Context, d: &mut PatternDialog) -> Answer {
    let mut answer = Answer::Open;
    let title = match d.kind {
        PatternKind::Linear => "Linear Pattern",
        PatternKind::Circular => "Circular Pattern",
        PatternKind::Mirror => "Mirror",
    };
    window(title).show(ctx, |ui| {
        ui.label("Features to copy");
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .show(ui, |ui| {
                for (id, name) in &d.candidates {
                    let mut checked = d.seeds.contains(id);
                    if ui.checkbox(&mut checked, name).changed() {
                        if checked {
                            d.seeds.push(*id);
                        } else {
                            d.seeds.retain(|s| s != id);
                        }
                    }
                }
            });
        ui.separator();
        egui::Grid::new("pattern_dialog")
            .num_columns(2)
            .show(ui, |ui| match d.kind {
                PatternKind::Linear => {
                    ui.label("Direction");
                    combo(ui, "pattern_axis", &d.axes, &mut d.axis);
                    ui.end_row();
                    ui.label("Spacing");
                    positive_length(ui, &mut d.spacing);
                    ui.end_row();
                    ui.label("Instances");
                    count(ui, &mut d.count);
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut d.reverse, "Reverse direction");
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut d.second, "Direction 2");
                    ui.end_row();
                    if d.second {
                        ui.label("Direction 2");
                        combo(ui, "pattern_axis2", &d.axes, &mut d.axis2);
                        ui.end_row();
                        ui.label("Spacing");
                        positive_length(ui, &mut d.spacing2);
                        ui.end_row();
                        ui.label("Instances");
                        count(ui, &mut d.count2);
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut d.reverse2, "Reverse direction 2");
                        ui.end_row();
                    }
                }
                PatternKind::Circular => {
                    ui.label("Axis");
                    combo(ui, "pattern_axis", &d.axes, &mut d.axis);
                    ui.end_row();
                    ui.label(if d.equal_spacing {
                        "Total angle"
                    } else {
                        "Angle between"
                    });
                    degrees(ui, &mut d.angle);
                    ui.end_row();
                    ui.label("Instances");
                    count(ui, &mut d.count);
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut d.equal_spacing, "Equal spacing");
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut d.reverse, "Reverse direction");
                    ui.end_row();
                }
                PatternKind::Mirror => {
                    ui.label("Mirror plane");
                    combo(ui, "mirror_plane", &d.planes, &mut d.plane);
                    ui.end_row();
                }
            });
        if !d.dimensions.is_empty() {
            ui.label(egui::RichText::new(d.dimensions.join(", ")).small().weak());
        }
        ui.label(
            egui::RichText::new(
                "Tip: select an edge, a face or an axis before opening this window.",
            )
            .small()
            .weak(),
        );
        answer = ok_cancel(ui, apply_label(d.editing, "OK"));
    });
    answer
}

pub(crate) fn ref_plane_window(ctx: &egui::Context, d: &mut PlaneDialog) -> Answer {
    let mut answer = Answer::Open;
    window("Reference Plane").show(ctx, |ui| {
        egui::Grid::new("plane_dialog")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Offset from");
                combo(ui, "plane_base", &d.planes, &mut d.base);
                ui.end_row();
                ui.label("Distance");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut d.offset).speed(0.1).suffix(" mm"));
                    dimension_note(ui, d.dimension.as_ref());
                });
                ui.end_row();
            });
        ui.checkbox(&mut d.flip, "Flip offset");
        answer = ok_cancel(ui, apply_label(d.editing, "OK"));
    });
    answer
}

pub(crate) fn ref_axis_window(ctx: &egui::Context, d: &mut AxisDialog) -> Answer {
    let mut answer = Answer::Open;
    window("Reference Axis").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.radio_value(&mut d.two_planes, false, "Along an edge, face or axis");
            ui.radio_value(&mut d.two_planes, true, "Two planes");
        });
        egui::Grid::new("axis_dialog")
            .num_columns(2)
            .show(ui, |ui| {
                if d.two_planes {
                    ui.label("First plane");
                    combo(ui, "axis_plane_a", &d.planes, &mut d.plane_a);
                    ui.end_row();
                    ui.label("Second plane");
                    combo(ui, "axis_plane_b", &d.planes, &mut d.plane_b);
                    ui.end_row();
                } else {
                    ui.label("Along");
                    combo(ui, "axis_along", &d.axes, &mut d.axis);
                    ui.end_row();
                }
            });
        answer = ok_cancel(ui, apply_label(d.editing, "OK"));
    });
    answer
}
