//! Commands of the features added in M3–M5: revolve, shell, reference plane and axis,
//! linear and circular patterns, mirror. Each opens in the PropertyManager for a new feature
//! (from the command bar) or for an existing one (double-click in the timeline).
//!
//! Planes, axes and directions start from what is selected and are listed with the
//! standard planes and axes and the reference geometry before the feature; clicking in the
//! viewport or the timeline while the command is open adds to the lists.

use oxyde_model::{
    AxisRef, BodyId, CircularPattern, Combine, CombineOp, ExtrudeOp, FaceName, Feature, FeatureId,
    FeatureKind, FeatureParameter, LinearPattern, Mirror, PatternDirection, PlaneRef, RefAxis,
    RefPlane, Revolve, RevolveAxis, Shell, StandardAxis, StandardPlane,
};
use oxyde_sketch::Geometry;

use crate::app::{OxydeApp, Selected};
use crate::dialogs::{Dialog, plane_name};

/// Labelled choices for a combo box.
pub type Choices<T> = Vec<(String, T)>;

pub struct RevolveDialog {
    pub editing: Option<FeatureId>,
    /// Full name of the angle dimension, when editing.
    pub dimension: Option<String>,
    /// The sketch to revolve; picked in the timeline.
    pub sketch: Option<FeatureId>,
    pub op: ExtrudeOp,
    pub axes: Choices<RevolveAxis>,
    pub axis: usize,
    /// The sketch `axes` were listed for.
    pub axes_for: Option<FeatureId>,
    /// Degrees.
    pub angle: f64,
    pub mid_plane: bool,
    pub reverse: bool,
    /// Bodies it joins, cuts or intersects; empty = automatic.
    pub bodies: Vec<BodyId>,
}

/// Combine: a target body and the tool bodies joined to, cut from or intersected with it.
pub struct CombineDialog {
    pub editing: Option<FeatureId>,
    pub target: Option<BodyId>,
    pub tools: Vec<BodyId>,
    pub op: CombineOp,
    pub keep_tools: bool,
    /// Clicks fill the tools box rather than the target.
    pub tools_next: bool,
}

pub struct ShellDialog {
    pub editing: Option<FeatureId>,
    pub dimension: Option<String>,
    /// Faces to remove, picked in the viewport.
    pub faces: Vec<FaceName>,
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
    /// None until a second direction is picked.
    pub axis2: Option<usize>,
    pub spacing2: f64,
    pub count2: u32,
    pub reverse2: bool,
    /// Whether clicks set direction 2 rather than direction 1: the active selection box.
    pub second_next: bool,
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
    /// The next clicked plane sets the second plane.
    pub next_b: bool,
}

// What each command makes, as it stands: previewed while it is open, added on OK. An
// error says what the command still needs.

impl RevolveDialog {
    pub fn feature(&self) -> Result<FeatureKind, &'static str> {
        let sketch = self.sketch.ok_or("Choose the sketch to revolve")?;
        let (_, axis) = self
            .axes
            .get(self.axis)
            .ok_or("Choose an axis to revolve about")?;
        Ok(FeatureKind::Revolve(Revolve {
            sketch,
            op: self.op,
            axis: axis.clone(),
            angle: self.angle.to_radians(),
            mid_plane: self.mid_plane,
            reverse: self.reverse,
            bodies: self.bodies.clone(),
        }))
    }
}

impl ShellDialog {
    pub fn feature(&self) -> FeatureKind {
        FeatureKind::Shell(Shell {
            faces: self.faces.clone(),
            thickness: self.thickness,
            outward: self.outward,
        })
    }
}

impl PatternDialog {
    /// `part` puts the seeds in timeline order.
    pub fn feature(&self, part: &oxyde_model::Part) -> Result<FeatureKind, &'static str> {
        if self.seeds.is_empty() {
            return Err("Choose at least one feature to copy");
        }
        let mut seeds = self.seeds.clone();
        seeds.sort_by_key(|id| part.index_of(*id));
        let axis = |i: usize| self.axes.get(i).map(|(_, a)| a.clone());
        Ok(match self.kind {
            PatternKind::Linear => {
                let first = axis(self.axis).ok_or("Choose a direction")?;
                let second = if self.second {
                    let axis2 = self
                        .axis2
                        .and_then(axis)
                        .ok_or("Select the second direction: an edge, a face or an axis")?;
                    Some(PatternDirection {
                        axis: axis2,
                        spacing: self.spacing2,
                        count: self.count2,
                        reverse: self.reverse2,
                    })
                } else {
                    None
                };
                FeatureKind::LinearPattern(LinearPattern {
                    seeds,
                    first: PatternDirection {
                        axis: first,
                        spacing: self.spacing,
                        count: self.count,
                        reverse: self.reverse,
                    },
                    second,
                })
            }
            PatternKind::Circular => FeatureKind::CircularPattern(CircularPattern {
                seeds,
                axis: axis(self.axis).ok_or("Choose an axis")?,
                angle: self.angle.to_radians(),
                count: self.count,
                equal_spacing: self.equal_spacing,
                reverse: self.reverse,
            }),
            PatternKind::Mirror => {
                let (_, plane) = self.planes.get(self.plane).ok_or("Choose a mirror plane")?;
                FeatureKind::Mirror(Mirror {
                    seeds,
                    plane: plane.clone(),
                })
            }
        })
    }
}

impl CombineDialog {
    pub fn feature(&self) -> Result<FeatureKind, &'static str> {
        let target = self.target.ok_or("Choose the target body")?;
        if self.tools.is_empty() {
            return Err("Choose the tool bodies");
        }
        Ok(FeatureKind::Combine(Combine {
            target,
            tools: self.tools.clone(),
            op: self.op,
            keep_tools: self.keep_tools,
        }))
    }
}

impl PlaneDialog {
    pub fn feature(&self) -> Option<FeatureKind> {
        let (_, base) = self.planes.get(self.base)?;
        Some(FeatureKind::Plane(RefPlane {
            base: base.clone(),
            offset: self.offset,
            flip: self.flip,
        }))
    }
}

impl AxisDialog {
    pub fn feature(&self) -> Option<FeatureKind> {
        let axis = if self.two_planes {
            let (_, a) = self.planes.get(self.plane_a)?;
            let (_, b) = self.planes.get(self.plane_b)?;
            RefAxis::TwoPlanes(a.clone(), b.clone())
        } else {
            let (_, a) = self.axes.get(self.axis)?;
            RefAxis::Along(a.clone())
        };
        Some(FeatureKind::Axis(axis))
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
        sketch: Option<FeatureId>,
        editing: Option<FeatureId>,
    ) -> Choices<RevolveAxis> {
        let mut out = Vec::new();
        let kind = sketch
            .and_then(|id| self.doc.part.feature(id))
            .map(|f| f.kind());
        if let Some(FeatureKind::Sketch(s)) = kind {
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
        sketch: Option<FeatureId>,
        op: ExtrudeOp,
        existing: Option<&Revolve>,
    ) {
        let axes = self.revolve_axis_choices(sketch, editing);
        let axes = with_current(axes, existing.map(|r| &r.axis));
        let axis = position(&axes, existing.map(|r| &r.axis));
        self.open_dialog(Dialog::Revolve(RevolveDialog {
            editing,
            dimension: editing.and_then(|id| self.parameter_full_name(id, FeatureParameter::Angle)),
            sketch,
            op,
            axes,
            axis,
            axes_for: sketch,
            angle: existing.map_or(360.0, |r| r.angle.to_degrees()),
            mid_plane: existing.is_some_and(|r| r.mid_plane),
            reverse: existing.is_some_and(|r| r.reverse),
            bodies: existing.map(|r| r.bodies.clone()).unwrap_or_default(),
        }));
    }

    pub(crate) fn begin_revolve(&mut self) {
        let sketch = self.sketch_to_use();
        self.open_revolve(None, sketch, ExtrudeOp::Boss, None);
    }

    pub(crate) fn open_shell(&mut self, editing: Option<FeatureId>, existing: Option<&Shell>) {
        let faces = match existing {
            Some(shell) => shell.faces.clone(),
            None => self.selected_faces(),
        };
        self.open_dialog(Dialog::Shell(ShellDialog {
            editing,
            dimension: editing
                .and_then(|id| self.parameter_full_name(id, FeatureParameter::Thickness)),
            faces,
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
            axis2: None,
            spacing2: 20.0,
            count2: 2,
            reverse2: false,
            second_next: false,
            angle: 360.0,
            equal_spacing: true,
            plane: 0,
            axes,
            planes,
        };
        // A selected edge or face is the most likely direction; a second one, the second
        // direction.
        if editing.is_none() {
            let mut picked = d
                .axes
                .iter()
                .enumerate()
                .filter(|(_, (_, a))| matches!(a, AxisRef::Edge(_) | AxisRef::Face(_)))
                .map(|(i, _)| i);
            if let Some(i) = picked.next() {
                d.axis = i;
            }
            if let (PatternKind::Linear, Some(i)) = (kind, picked.next()) {
                d.second = true;
                d.axis2 = Some(i);
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
                    d.axis2 = Some(position(&d.axes, Some(&second.axis)));
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
        self.open_dialog(Dialog::Pattern(d));
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
        self.open_dialog(Dialog::RefPlane(PlaneDialog {
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
        self.open_dialog(Dialog::RefAxis(AxisDialog {
            editing,
            two_planes,
            axes,
            axis,
            planes,
            plane_a,
            plane_b,
            next_b: false,
        }));
    }

    // ---- Applying ----------------------------------------------------------------------------

    pub(crate) fn add_or_replace(&mut self, editing: Option<FeatureId>, kind: FeatureKind) {
        match editing {
            Some(id) => self.replace_feature(id, kind),
            None => {
                self.add_feature(kind);
            }
        }
    }

    /// Adds or changes the feature a command describes, or says what it still needs.
    fn apply_feature(&mut self, editing: Option<FeatureId>, kind: Result<FeatureKind, &str>) {
        match kind {
            Ok(kind) => self.add_or_replace(editing, kind),
            Err(missing) => self.status = missing.to_owned(),
        }
    }

    pub(crate) fn apply_revolve(&mut self, d: RevolveDialog) {
        if d.sketch.is_some() {
            self.apply_feature(d.editing, d.feature());
        }
    }

    pub(crate) fn apply_shell(&mut self, d: ShellDialog) {
        self.apply_feature(d.editing, Ok(d.feature()));
    }

    pub(crate) fn apply_pattern(&mut self, d: PatternDialog) {
        let kind = d.feature(&self.doc.part);
        self.apply_feature(d.editing, kind);
    }

    pub(crate) fn open_combine(&mut self, editing: Option<FeatureId>, existing: Option<&Combine>) {
        // A new Combine starts from the body selected in the tree, if any.
        let target = existing.map(|c| c.target).or(self.selected_body);
        self.open_dialog(Dialog::Combine(CombineDialog {
            editing,
            tools_next: target.is_some(),
            target,
            tools: existing.map(|c| c.tools.clone()).unwrap_or_default(),
            op: existing.map_or(CombineOp::Join, |c| c.op),
            keep_tools: existing.is_some_and(|c| c.keep_tools),
        }));
        if self.body_count() < 2 && existing.is_none() {
            self.status = "Combine needs two bodies: make one with Extrude › New body".to_owned();
        }
    }

    pub(crate) fn apply_combine(&mut self, d: CombineDialog) {
        self.apply_feature(d.editing, d.feature());
    }

    /// How many bodies the part has now.
    pub(crate) fn body_count(&self) -> usize {
        self.doc.part.body().map_or(0, |b| b.solids.len())
    }

    /// "Body 2": bodies are numbered in the order their features come in the timeline.
    pub(crate) fn body_label(&self, id: BodyId) -> String {
        let mut ids: Vec<BodyId> = self
            .doc
            .part
            .body()
            .map(|b| b.solids.iter().map(|s| s.id).collect())
            .unwrap_or_default();
        ids.sort_by_key(|b| (self.doc.part.index_of(b.feature), b.index));
        match ids.iter().position(|b| *b == id) {
            Some(i) => format!("Body {}", i + 1),
            None => format!("Body {id} (gone)"),
        }
    }

    /// The body a face of the part is on.
    pub(crate) fn body_of_face(&self, face: &FaceName) -> Option<BodyId> {
        let body = self.doc.part.body()?;
        let index = body.naming.find_face(face)?;
        body.solid_of_face(index).map(|s| s.id)
    }

    pub(crate) fn apply_ref_plane(&mut self, d: PlaneDialog) {
        if let Some(kind) = d.feature() {
            self.add_or_replace(d.editing, kind);
        }
    }

    pub(crate) fn apply_ref_axis(&mut self, d: AxisDialog) {
        if let Some(kind) = d.feature() {
            self.add_or_replace(d.editing, kind);
        }
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
