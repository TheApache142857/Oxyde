//! The PropertyManager: a feature's options, shown at the top of the left panel while the
//! feature is created or edited, as in SolidWorks.
//!
//! The viewport and the timeline stay live: clicking a sketch, a face, an edge, a plane or
//! a feature fills the matching selection box of the open command (the sketch to extrude,
//! the edges to fillet, the features to pattern…) instead of selecting it. Commands opened
//! with something already selected start with it.

use eframe::egui::{self, Color32, RichText, Stroke};
use oxyde_model::{
    AxisRef, CombineOp, EdgeName, ExtrudeOp, FaceKey, FaceName, FeatureId, FeatureKind, PlaneRef,
    RevolveAxis, SketchPlane, StandardPlane,
};
use oxyde_render::Pick;

use crate::app::OxydeApp;
use crate::dialogs::{
    Answer, BlendDialog, ChamferType, Dialog, EndCondition, ExtrudeDialog, plane_name,
    positive_length,
};
use crate::features_ui::{
    AxisDialog, Choices, CombineDialog, PatternDialog, PatternKind, RevolveDialog, ShellDialog,
    SweepDialog,
};
use crate::icons::{self, Icon};

/// Something clicked while a command is open.
#[derive(Debug, Clone)]
pub enum PanelPick {
    Feature(FeatureId),
    /// A body, from the Solid Bodies folder.
    Body(oxyde_model::BodyId),
    Plane(StandardPlane),
    Face(FaceName),
    Edge(EdgeName),
    /// A corner of the body (the Plane command draws the one under the pointer).
    Vertex(oxyde_model::VertexName),
    /// A point of a sketch, by entity id.
    SketchPoint(FeatureId, u32),
    /// A line, an arc or a circle of a sketch, by entity id.
    SketchCurve(FeatureId, u32),
}

/// Fill and outline of the active selection box.
pub(crate) fn box_fill() -> Color32 {
    crate::theme::tokens().accent_bg
}

pub(crate) fn box_active() -> Color32 {
    crate::theme::tokens().accent
}

/// A selection box: what the command acts on, filled by clicking in the viewport or the
/// timeline. Items are (label, hover text). Returns the index of an item the user removed.
pub(crate) fn selection_box(
    ui: &mut egui::Ui,
    items: &[(String, String)],
    hint: &str,
) -> Option<usize> {
    let mut removed = None;
    egui::Frame::none()
        .fill(box_fill())
        .stroke(Stroke::new(1.0_f32, box_active()))
        .rounding(3.0)
        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if items.is_empty() {
                ui.label(RichText::new(hint).italics().weak());
            }
            egui::ScrollArea::vertical()
                .max_height(110.0)
                .id_salt(hint)
                .show(ui, |ui| {
                    for (i, (label, hover)) in items.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if icons::icon_button(ui, Icon::Cancel, 16.0, true)
                                .on_hover_text("Remove from the selection")
                                .clicked()
                            {
                                removed = Some(i);
                            }
                            let response = ui.label(label);
                            if !hover.is_empty() {
                                response.on_hover_text(hover.as_str());
                            }
                        });
                    }
                });
        });
    removed
}

/// A single-reference selection box (a direction, an axis, a plane), as in SolidWorks:
/// the active box, outlined, takes the next click in the viewport or the timeline;
/// clicking a box makes it the active one. The arrow lists the other choices, the model
/// axes and planes among them. Returns true when the user clicked the box or chose from
/// the list.
fn reference_box<T>(
    ui: &mut egui::Ui,
    id: &str,
    choices: &[(String, T)],
    selected: &mut Option<usize>,
    active: bool,
    hint: &str,
) -> bool {
    let current = selected
        .and_then(|i| choices.get(i))
        .map(|(label, _)| label);
    let stroke = if active {
        Stroke::new(2.0_f32, box_active())
    } else {
        Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color)
    };
    let (mut clicked, mut chosen) = (false, false);
    ui.push_id(id, |ui| {
        egui::Frame::none()
            .fill(if active {
                box_fill()
            } else {
                Color32::TRANSPARENT
            })
            .stroke(stroke)
            .rounding(3.0)
            .inner_margin(egui::Margin::symmetric(6.0, 4.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                // `horizontal` first: a bare right-to-left layout takes all the height left
                // in the panel and centres the label in it, stretching the box to the bottom.
                let row = egui::Layout::right_to_left(egui::Align::Center);
                ui.horizontal(|ui| {
                    ui.with_layout(row, |ui| {
                        // ⏷, not ▾: the interface fonts have no ▾, which showed as a box.
                        ui.menu_button("⏷", |ui| {
                            for (i, (label, _)) in choices.iter().enumerate() {
                                if ui.selectable_label(*selected == Some(i), label).clicked() {
                                    *selected = Some(i);
                                    chosen = true;
                                    ui.close_menu();
                                }
                            }
                        })
                        .response
                        .on_hover_text("Choose from the list");
                        // The rest of the box, left of the arrow, activates it.
                        let layout = egui::Layout::left_to_right(egui::Align::Center);
                        ui.with_layout(layout, |ui| {
                            let text = match current {
                                Some(label) => RichText::new(label),
                                None => RichText::new(hint).italics().weak(),
                            };
                            let label = ui.add(
                                egui::Label::new(text)
                                    .truncate()
                                    .sense(egui::Sense::click()),
                            );
                            let rest = ui.allocate_response(
                                egui::vec2(ui.available_width(), label.rect.height()),
                                egui::Sense::click(),
                            );
                            let hover = if active {
                                "Click in the graphics area or the tree to fill this in"
                            } else {
                                "Click here, then in the graphics area or the tree"
                            };
                            clicked = label.on_hover_text(hover).clicked()
                                || rest.on_hover_text(hover).clicked();
                        });
                    })
                });
            });
    });
    clicked || chosen
}

/// `reference_box` for a field that always has a value.
fn reference_box_set<T>(
    ui: &mut egui::Ui,
    id: &str,
    choices: &[(String, T)],
    selected: &mut usize,
    active: bool,
    hint: &str,
) -> bool {
    let mut value = Some(*selected);
    let clicked = reference_box(ui, id, choices, &mut value, active, hint);
    if let Some(value) = value {
        *selected = value;
    }
    clicked
}

/// A section title, as KiCad Flux's panels: small, upper case, dimmed.
pub(crate) fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .size(11.0)
            .extra_letter_spacing(0.6)
            .color(crate::theme::tokens().dim),
    );
}

fn degrees(ui: &mut egui::Ui, value: &mut f64) {
    ui.add(
        egui::DragValue::new(value)
            .speed(1.0)
            .range(0.01..=360.0)
            .suffix("°")
            .custom_parser(crate::dialogs::angle_parser),
    );
}

fn count(ui: &mut egui::Ui, value: &mut u32) {
    ui.add(egui::DragValue::new(value).speed(0.1).range(1..=1000));
}

fn dimension_note(ui: &mut egui::Ui, name: Option<&String>) {
    if let Some(name) = name {
        ui.label(RichText::new(name).small().weak());
    }
}

/// What an extrusion, a revolve or a sweep does: join, cut, intersect or a new body (Fusion's
/// operations), with icons.
fn operation(ui: &mut egui::Ui, op: &mut ExtrudeOp, has_body: bool) {
    let options = [
        (ExtrudeOp::Boss, Icon::Add, "Join"),
        (ExtrudeOp::Cut, Icon::Remove, "Cut"),
        (ExtrudeOp::Intersect, Icon::Intersect, "Intersect"),
        (ExtrudeOp::NewBody, Icon::NewBody, "New body"),
    ];
    egui::Grid::new(ui.next_auto_id())
        .num_columns(2)
        .spacing([4.0, 2.0])
        .show(ui, |ui| {
            for (i, (value, icon, label)) in options.into_iter().enumerate() {
                let needs_body = matches!(value, ExtrudeOp::Cut | ExtrudeOp::Intersect);
                let enabled = !needs_body || has_body || *op == value;
                let selected = *op == value;
                let response = ui
                    .add_enabled_ui(enabled, |ui| {
                        egui::Frame::none()
                            .fill(if selected {
                                ui.visuals().selection.bg_fill
                            } else {
                                Color32::TRANSPARENT
                            })
                            .rounding(4.0)
                            .inner_margin(egui::Margin::symmetric(6.0, 3.0))
                            .show(ui, |ui| {
                                ui.set_min_width(96.0);
                                ui.horizontal(|ui| {
                                    icons::small(ui, icon, 16.0);
                                    ui.label(label);
                                });
                            })
                            .response
                            .interact(egui::Sense::click())
                    })
                    .inner;
                let hint = match value {
                    ExtrudeOp::Boss => "Add material to a body",
                    ExtrudeOp::Cut => "Remove material from bodies",
                    ExtrudeOp::Intersect => "Keep only what it shares with a body",
                    ExtrudeOp::NewBody => "A separate body, joined to nothing",
                };
                let response = if enabled {
                    response.on_hover_text(hint)
                } else {
                    response.on_hover_text("There is no body yet")
                };
                if enabled && response.clicked() {
                    *op = value;
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
}

/// Adds `value` to the list, or takes it out.
fn toggle<T: PartialEq>(list: &mut Vec<T>, value: T) {
    match list.iter().position(|v| *v == value) {
        Some(i) => {
            list.remove(i);
        }
        None => list.push(value),
    }
}

/// Puts `value` in `choices` (labelled `label` if new) and returns its index.
/// Picked faces and edges are numbered ("Picked edge 2").
fn choose<T: PartialEq + Clone>(choices: &mut Choices<T>, value: T, label: String) -> usize {
    match choices.iter().position(|(_, c)| *c == value) {
        Some(i) => i,
        None => {
            let label = if label.starts_with("Picked") {
                let n = choices
                    .iter()
                    .filter(|(l, _)| l.starts_with(&label))
                    .count();
                format!("{label} {}", n + 1)
            } else {
                label
            };
            choices.push((label, value));
            choices.len() - 1
        }
    }
}

impl OxydeApp {
    fn feature_name(&self, id: FeatureId) -> String {
        self.doc
            .part
            .feature(id)
            .map(|f| f.name().to_owned())
            .unwrap_or_else(|| "(missing)".to_owned())
    }

    fn is_sketch(&self, id: FeatureId) -> bool {
        matches!(
            self.doc.part.feature(id).map(|f| f.kind()),
            Some(FeatureKind::Sketch(_))
        )
    }

    /// Whether `id` comes before the feature being edited (or the rollback bar).
    pub(crate) fn usable(&self, id: FeatureId, editing: Option<FeatureId>) -> bool {
        let Some(index) = self.doc.part.index_of(id) else {
            return false;
        };
        let end = match editing.and_then(|e| self.doc.part.index_of(e)) {
            Some(e) => e,
            None => self
                .doc
                .part
                .rollback_position()
                .unwrap_or(self.doc.part.features().len()),
        };
        index < end
    }

    fn face_is_planar(&self, face: &FaceName) -> bool {
        self.doc.part.body().is_some_and(|body| {
            body.naming
                .find_face(face)
                .and_then(|i| body.topology.faces.get(i as usize))
                .is_some_and(|f| f.normal.is_some())
        })
    }

    fn face_has_axis(&self, face: &FaceName) -> bool {
        self.doc.part.body().is_some_and(|body| {
            body.naming
                .find_face(face)
                .and_then(|i| body.topology.faces.get(i as usize))
                .is_some_and(|f| f.axis.is_some() || f.normal.is_some())
        })
    }

    fn edge_has_axis(&self, edge: &EdgeName) -> bool {
        self.doc.part.body().is_some_and(|body| {
            body.naming
                .find_edge(edge)
                .and_then(|i| body.topology.edges.get(i as usize))
                .is_some_and(|e| e.axis.is_some())
        })
    }

    /// The edges around a face, by name.
    fn face_edges(&self, face: &FaceName) -> Vec<EdgeName> {
        let Some(body) = self.doc.part.body() else {
            return Vec::new();
        };
        body.naming
            .find_face(face)
            .and_then(|i| body.topology.faces.get(i as usize))
            .map(|f| {
                f.edges
                    .iter()
                    .filter_map(|e| body.naming.edge(*e).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn axis_of_pick(
        &self,
        pick: &PanelPick,
        editing: Option<FeatureId>,
    ) -> Option<(AxisRef, String)> {
        match pick {
            PanelPick::Edge(e) if self.edge_has_axis(e) => {
                Some((AxisRef::Edge(e.clone()), "Picked edge".to_owned()))
            }
            PanelPick::Face(f) if self.face_has_axis(f) => {
                Some((AxisRef::Face(f.clone()), "Picked face".to_owned()))
            }
            PanelPick::Feature(id)
                if self.usable(*id, editing)
                    && matches!(
                        self.doc.part.feature(*id).map(|f| f.kind()),
                        Some(FeatureKind::Axis(_))
                    ) =>
            {
                Some((AxisRef::Reference(*id), self.feature_name(*id)))
            }
            _ => None,
        }
    }

    /// The body a click stands for: a body in the tree, or the body of a face.
    fn body_of_pick(&self, pick: &PanelPick) -> Option<oxyde_model::BodyId> {
        match pick {
            PanelPick::Body(id) => Some(*id),
            PanelPick::Face(f) => self.body_of_face(f),
            _ => None,
        }
    }

    fn plane_of_pick(
        &self,
        pick: &PanelPick,
        editing: Option<FeatureId>,
    ) -> Option<(PlaneRef, String)> {
        match pick {
            PanelPick::Plane(p) => {
                Some((PlaneRef::Standard(*p), format!("{} Plane", plane_name(*p))))
            }
            PanelPick::Face(f) if self.face_is_planar(f) => {
                Some((PlaneRef::Face(f.clone()), "Picked face".to_owned()))
            }
            PanelPick::Feature(id)
                if self.usable(*id, editing)
                    && matches!(
                        self.doc.part.feature(*id).map(|f| f.kind()),
                        Some(FeatureKind::Plane(_))
                    ) =>
            {
                Some((PlaneRef::Reference(*id), self.feature_name(*id)))
            }
            _ => None,
        }
    }

    /// Whether a face, an edge or a body clicked in the viewport exists just before the
    /// feature being edited (or the one being made, at the rollback bar): the only
    /// geometry that feature can use.
    fn exists_before(&self, pick: &PanelPick, editing: Option<FeatureId>) -> bool {
        let body = match editing {
            Some(id) => self.doc.part.body_before(id),
            None => self.doc.part.body(),
        };
        match pick {
            PanelPick::Face(f) => body.is_some_and(|b| b.naming.find_face(f).is_some()),
            PanelPick::Edge(e) => body.is_some_and(|b| b.naming.find_edge(e).is_some()),
            PanelPick::Body(id) => body.is_some_and(|b| b.solid(*id).is_some()),
            PanelPick::Vertex(v) => body.is_some_and(|b| b.naming.find_vertex(v).is_some()),
            // Sketches are checked by the command, which knows what comes before it.
            PanelPick::Feature(_)
            | PanelPick::Plane(_)
            | PanelPick::SketchPoint(..)
            | PanelPick::SketchCurve(..) => true,
        }
    }

    /// The instance of the open linear or circular pattern that a face or an edge clicked
    /// in the viewport belongs to, if it is one of the copies the pattern makes: in its
    /// preview, or in the part when the pattern is edited without one.
    fn copy_of_pick(&self, pick: &PanelPick) -> Option<[u32; 2]> {
        let Some(Dialog::Pattern(d)) = &self.dialog else {
            return None;
        };
        if d.kind == PatternKind::Mirror {
            return None;
        }
        let made_by = self.preview_feature();
        let instance = |face: &FaceName| match &face.key {
            FaceKey::Instance {
                feature, instance, ..
            } if Some(*feature) == made_by || Some(*feature) == d.editing => Some(*instance),
            _ => None,
        };
        match pick {
            PanelPick::Face(face) => instance(face),
            PanelPick::Edge(edge) => edge.faces.iter().find_map(instance),
            _ => None,
        }
    }

    /// Offers a click to the open command. Returns true when the command took it.
    pub(crate) fn offer_to_panel(&mut self, pick: PanelPick) -> bool {
        let pick = self.unpreview_pick(pick);
        // A copy the open pattern makes is left out while its Instances to Skip take
        // clicks, as SolidWorks does; it is never a reference for the pattern itself.
        if let Some(instance) = self.copy_of_pick(&pick) {
            match &mut self.dialog {
                Some(Dialog::Pattern(d)) if d.skip_next => d.toggle_skip(instance),
                _ => {
                    self.status = "That is one of the pattern's copies: turn on Pick instances, \
                                   under Instances to Skip, to leave it out"
                        .to_owned();
                }
            }
            return true;
        }
        // A feature can only use what exists before it: not what it makes itself (shown
        // by its preview), nor, when it is edited in the finished part, what later
        // features made or changed (which would stop the file from opening).
        if let Some(dialog) = self.dialog.as_ref().filter(|d| d.is_feature()) {
            let editing = dialog.editing();
            if !self.exists_before(&pick, editing) {
                self.status = match (self.preview_refusal(&pick), editing) {
                    (Some(why), _) => why,
                    (None, Some(id)) => format!(
                        "That is made or changed by a feature after {}: pick geometry that exists before it",
                        self.feature_name(id)
                    ),
                    (None, None) => "That is no longer in the part: pick it again".to_owned(),
                };
                return true;
            }
        }
        let Some(mut dialog) = self.dialog.take() else {
            return false;
        };
        let mut keep = true;
        let taken = match &mut dialog {
            Dialog::ChoosePlane => {
                let plane = match &pick {
                    PanelPick::Plane(p) => Some(SketchPlane::Standard(*p)),
                    PanelPick::Face(f) if self.face_is_planar(f) => {
                        Some(SketchPlane::Face(f.clone()))
                    }
                    // Only a plane before the rollback bar: the sketch goes there, and a
                    // feature may only refer to what comes before it.
                    PanelPick::Feature(id)
                        if self.usable(*id, None)
                            && matches!(
                                self.doc.part.feature(*id).map(|f| f.kind()),
                                Some(FeatureKind::Plane(_))
                            ) =>
                    {
                        Some(SketchPlane::Reference(*id))
                    }
                    _ => None,
                };
                match plane {
                    Some(plane) => {
                        keep = false;
                        self.start_new_sketch(plane);
                        true
                    }
                    None => false,
                }
            }
            Dialog::Extrude(d) => match pick {
                PanelPick::Feature(id) if self.is_sketch(id) && self.usable(id, d.editing) => {
                    d.sketch = Some(id);
                    true
                }
                // The surface Up To Surface goes to, while its box takes clicks.
                other
                    if d.end == EndCondition::UpToSurface
                        && (d.up_to_next || d.up_to.is_none())
                        && self.plane_of_pick(&other, d.editing).is_some() =>
                {
                    if let Some((plane, label)) = self.plane_of_pick(&other, d.editing) {
                        d.up_to = Some(choose(&mut d.planes, plane, label));
                        d.up_to_next = false;
                    }
                    true
                }
                // A body, or a face of one: in or out of the feature's scope.
                other => match self.body_of_pick(&other) {
                    Some(body) if d.op != ExtrudeOp::NewBody => {
                        toggle(&mut d.bodies, body);
                        true
                    }
                    _ => false,
                },
            },
            Dialog::Hole(d) => match pick {
                PanelPick::Feature(id) if self.is_sketch(id) && self.usable(id, d.editing) => {
                    d.sketch = Some(id);
                    true
                }
                other => match self.body_of_pick(&other) {
                    Some(body) => {
                        toggle(&mut d.bodies, body);
                        true
                    }
                    None => false,
                },
            },
            Dialog::Loft(d) => match &pick {
                // Sketches go into the list in the order clicked; a second click takes
                // one out.
                PanelPick::Feature(id) if self.is_sketch(*id) && self.usable(*id, d.editing) => {
                    toggle(&mut d.profiles, *id);
                    true
                }
                other => match self.body_of_pick(other) {
                    Some(body) if d.op != ExtrudeOp::NewBody => {
                        toggle(&mut d.bodies, body);
                        true
                    }
                    _ => false,
                },
            },
            Dialog::Combine(d) => match self.body_of_pick(&pick) {
                Some(body) => {
                    if d.tools_next {
                        if Some(body) != d.target {
                            toggle(&mut d.tools, body);
                        }
                    } else {
                        d.tools.retain(|t| *t != body);
                        d.target = Some(body);
                        d.tools_next = true;
                    }
                    true
                }
                None => false,
            },
            Dialog::Revolve(d) => match &pick {
                PanelPick::Feature(id) if self.is_sketch(*id) && self.usable(*id, d.editing) => {
                    d.sketch = Some(*id);
                    true
                }
                PanelPick::Face(_) | PanelPick::Body(_)
                    if d.op != ExtrudeOp::NewBody
                        && self.body_of_pick(&pick).is_some()
                        && self.axis_of_pick(&pick, d.editing).is_none() =>
                {
                    if let Some(body) = self.body_of_pick(&pick) {
                        toggle(&mut d.bodies, body);
                    }
                    true
                }
                _ => match self.axis_of_pick(&pick, d.editing) {
                    Some((axis, label)) => {
                        d.axis = choose(&mut d.axes, RevolveAxis::Axis(axis), label);
                        true
                    }
                    None => false,
                },
            },
            Dialog::Sweep(d) => match &pick {
                PanelPick::Feature(id) if self.is_sketch(*id) && self.usable(*id, d.editing) => {
                    // A sketch goes into the empty box, else into the active one; the
                    // first pick is the profile, the next the path, as in SolidWorks.
                    if d.profile.is_none() && !d.path_next {
                        d.profile = Some(*id);
                        d.path_next = true;
                    } else if d.path.is_none() || d.path_next {
                        d.path = Some(*id);
                        d.path_next = false;
                    } else {
                        d.profile = Some(*id);
                        d.path_next = true;
                    }
                    true
                }
                other => match self.body_of_pick(other) {
                    Some(body) if d.op != ExtrudeOp::NewBody => {
                        toggle(&mut d.bodies, body);
                        true
                    }
                    _ => false,
                },
            },
            Dialog::Blend(d) => match pick {
                PanelPick::Edge(e) => {
                    match d.edges.iter().position(|x| *x == e) {
                        Some(i) => {
                            d.edges.remove(i);
                        }
                        None => d.edges.push(e),
                    }
                    true
                }
                PanelPick::Face(f) => {
                    // All the edges around the face, or none of them if they are all in.
                    let around = self.face_edges(&f);
                    if around.iter().all(|e| d.edges.contains(e)) {
                        d.edges.retain(|e| !around.contains(e));
                    } else {
                        for e in around {
                            if !d.edges.contains(&e) {
                                d.edges.push(e);
                            }
                        }
                    }
                    true
                }
                _ => false,
            },
            Dialog::Shell(d) => match pick {
                PanelPick::Face(f) => {
                    match d.faces.iter().position(|x| *x == f) {
                        Some(i) => {
                            d.faces.remove(i);
                        }
                        None => d.faces.push(f),
                    }
                    true
                }
                _ => false,
            },
            Dialog::Pattern(d) => {
                let seed = match &pick {
                    PanelPick::Feature(id) => {
                        d.candidates.iter().any(|(c, _)| c == id).then_some(*id)
                    }
                    _ => None,
                };
                let body = (d.kind == PatternKind::Mirror)
                    .then(|| match &pick {
                        PanelPick::Body(id) => Some(*id),
                        PanelPick::Face(_) if d.bodies_next => self.body_of_pick(&pick),
                        _ => None,
                    })
                    .flatten();
                if let Some(id) = seed {
                    match d.seeds.iter().position(|s| *s == id) {
                        Some(i) => {
                            d.seeds.remove(i);
                        }
                        None => d.seeds.push(id),
                    }
                    true
                } else if let Some(body) = body {
                    toggle(&mut d.bodies, body);
                    true
                } else if d.skip_next && matches!(pick, PanelPick::Face(_) | PanelPick::Edge(_)) {
                    self.status = "Click a copy of the pattern to leave it out, or a direction's \
                                   box first to pick a direction"
                        .to_owned();
                    true
                } else if d.kind == PatternKind::Mirror {
                    match self.plane_of_pick(&pick, d.editing) {
                        Some((plane, label)) => {
                            d.plane = choose(&mut d.planes, plane, label);
                            true
                        }
                        None => false,
                    }
                } else {
                    match self.axis_of_pick(&pick, d.editing) {
                        Some((axis, label)) => {
                            let i = choose(&mut d.axes, axis, label);
                            if d.kind == PatternKind::Linear && d.second && d.second_next {
                                d.axis2 = Some(i);
                            } else {
                                d.axis = i;
                                // On to the second direction while it is still empty.
                                if d.kind == PatternKind::Linear && d.second && d.axis2.is_none() {
                                    d.second_next = true;
                                }
                            }
                            true
                        }
                        None => false,
                    }
                }
            }
            Dialog::RefPlane(d) => self.plane_pick(d, &pick),
            Dialog::Appearance(d) => self.appearance_pick(d, pick),
            Dialog::RefAxis(d) => {
                if d.two_planes {
                    match self.plane_of_pick(&pick, d.editing) {
                        Some((plane, label)) => {
                            let i = choose(&mut d.planes, plane, label);
                            if d.next_b {
                                d.plane_b = i;
                            } else {
                                d.plane_a = i;
                            }
                            d.next_b = !d.next_b;
                            true
                        }
                        None => false,
                    }
                } else {
                    match self.axis_of_pick(&pick, d.editing) {
                        Some((axis, label)) => {
                            d.axis = choose(&mut d.axes, axis, label);
                            true
                        }
                        None => false,
                    }
                }
            }
            _ => false,
        };
        if keep && self.dialog.is_none() {
            self.dialog = Some(dialog);
        }
        taken
    }

    /// Faces and edges the open command refers to, highlighted in the viewport.
    pub(crate) fn panel_picks(&self) -> Option<Vec<Pick>> {
        let dialog = self.dialog.as_ref().filter(|d| d.is_panel())?;
        let body = self.shown_body();
        let face = |f: &FaceName| body.and_then(|b| b.naming.find_face(f)).map(Pick::Face);
        let edge = |e: &EdgeName| body.and_then(|b| b.naming.find_edge(e)).map(Pick::Edge);
        let axis = |a: Option<&AxisRef>| match a {
            Some(AxisRef::Edge(e)) => edge(e),
            Some(AxisRef::Face(f)) => face(f),
            _ => None,
        };
        let plane = |p: Option<&PlaneRef>| match p {
            Some(PlaneRef::Face(f)) => face(f),
            _ => None,
        };
        let picks = match dialog {
            Dialog::Appearance(d) => match d.scope {
                crate::appearance_ui::Scope::Faces => d.faces.iter().filter_map(face).collect(),
                crate::appearance_ui::Scope::Body(id) => body
                    .and_then(|b| b.solid(id))
                    .map(|s| s.faces.clone().map(Pick::Face).collect())
                    .unwrap_or_default(),
                _ => Vec::new(),
            },
            Dialog::Blend(d) => d.edges.iter().filter_map(edge).collect(),
            Dialog::Extrude(d) if d.end == EndCondition::UpToSurface => {
                plane(d.up_to.and_then(|i| d.planes.get(i)).map(|(_, p)| p))
                    .into_iter()
                    .collect()
            }
            Dialog::Shell(d) => d.faces.iter().filter_map(face).collect(),
            Dialog::Revolve(d) => match d.axes.get(d.axis).map(|(_, a)| a) {
                Some(RevolveAxis::Axis(a)) => axis(Some(a)).into_iter().collect(),
                _ => Vec::new(),
            },
            Dialog::Pattern(d) => match d.kind {
                PatternKind::Mirror => plane(d.planes.get(d.plane).map(|(_, p)| p))
                    .into_iter()
                    .collect(),
                _ => {
                    let mut v: Vec<Pick> = axis(d.axes.get(d.axis).map(|(_, a)| a))
                        .into_iter()
                        .collect();
                    if d.kind == PatternKind::Linear && d.second {
                        v.extend(axis(d.axis2.and_then(|i| d.axes.get(i)).map(|(_, a)| a)));
                    }
                    v
                }
            },
            Dialog::RefPlane(d) => self.plane_dialog_picks(d),
            Dialog::RefAxis(d) if !d.two_planes => axis(d.axes.get(d.axis).map(|(_, a)| a))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };
        Some(picks)
    }

    /// Features the open command refers to, highlighted in the timeline.
    pub(crate) fn panel_features(&self) -> Vec<FeatureId> {
        match &self.dialog {
            Some(Dialog::Extrude(d)) => d.sketch.into_iter().collect(),
            Some(Dialog::Revolve(d)) => d.sketch.into_iter().collect(),
            Some(Dialog::Sweep(d)) => d.profile.into_iter().chain(d.path).collect(),
            Some(Dialog::Hole(d)) => d.sketch.into_iter().collect(),
            Some(Dialog::Loft(d)) => d.profiles.clone(),
            Some(Dialog::Pattern(d)) => d.seeds.clone(),
            Some(Dialog::RefPlane(d)) => d.features(),
            _ => Vec::new(),
        }
    }

    /// The open command's options, at the top of the left panel.
    pub(crate) fn property_manager(&mut self, ui: &mut egui::Ui) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        if !dialog.is_panel() {
            self.dialog = Some(dialog);
            return;
        }
        let (icon, title, ready) = self.panel_header(&dialog);
        let mut answer = Answer::Open;
        // The command finished by itself (a sketch plane was picked).
        let mut finished = false;

        egui::Frame::none()
            .fill(ui.visuals().faint_bg_color)
            .stroke(Stroke::new(
                1.0_f32,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
            .rounding(4.0)
            .inner_margin(egui::Margin::same(8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    icons::small(ui, icon, 22.0);
                    ui.label(RichText::new(title).strong().size(15.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::icon_button(ui, Icon::Cancel, 24.0, true)
                            .on_hover_text("Cancel (Esc)")
                            .clicked()
                        {
                            answer = Answer::Cancel;
                        }
                        let ok = icons::icon_button(ui, Icon::Ok, 24.0, ready);
                        let ok = if ready {
                            ok.on_hover_text("OK (Enter)")
                        } else {
                            ok.on_hover_text("Select what the command needs first")
                        };
                        if ok.clicked() {
                            answer = Answer::Ok;
                        }
                    });
                });
                ui.separator();
                if self.panel_body(ui, &mut dialog) {
                    finished = true;
                }
                if dialog.is_feature() {
                    self.preview_footer(ui);
                }
            });
        ui.add_space(6.0);

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            answer = Answer::Cancel;
        }
        if ready && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            answer = Answer::Ok;
        }
        if let Dialog::Revolve(d) = &mut dialog {
            self.refresh_revolve_axes(d);
        }
        let ctx = ui.ctx().clone();
        if finished {
            return;
        }
        match answer {
            Answer::Open => {
                if self.dialog.is_none() {
                    self.dialog = Some(dialog);
                }
            }
            Answer::Cancel => {
                if let Dialog::Appearance(d) = dialog {
                    self.cancel_appearance(d);
                }
            }
            Answer::Ok => self.apply_dialog(dialog, &ctx),
        }
    }

    /// Icon, title, and whether the command has what it needs to run.
    fn panel_header(&self, dialog: &Dialog) -> (Icon, &'static str, bool) {
        let editing = |e: Option<FeatureId>, new: &'static str, edit: &'static str| {
            if e.is_some() { edit } else { new }
        };
        match dialog {
            Dialog::ChoosePlane => (Icon::Sketch, "Sketch", false),
            Dialog::Extrude(d) => (
                Icon::Extrude,
                editing(d.editing, "Extrude", "Edit Extrude"),
                d.feature().is_some(),
            ),
            Dialog::Revolve(d) => (
                Icon::Revolve,
                editing(d.editing, "Revolve", "Edit Revolve"),
                d.sketch.is_some() && !d.axes.is_empty(),
            ),
            Dialog::Sweep(d) => (
                Icon::Sweep,
                editing(d.editing, "Sweep", "Edit Sweep"),
                d.feature().is_ok(),
            ),
            Dialog::Loft(d) => (
                Icon::Loft,
                editing(d.editing, "Loft", "Edit Loft"),
                d.feature().is_ok(),
            ),
            Dialog::Blend(d) if d.fillet => (
                Icon::Fillet,
                editing(d.editing, "Fillet", "Edit Fillet"),
                !d.edges.is_empty(),
            ),
            Dialog::Blend(d) => (
                Icon::Chamfer,
                editing(d.editing, "Chamfer", "Edit Chamfer"),
                !d.edges.is_empty(),
            ),
            Dialog::Shell(d) => (Icon::Shell, editing(d.editing, "Shell", "Edit Shell"), true),
            Dialog::Hole(d) => (
                Icon::Hole,
                editing(d.editing, "Hole Wizard", "Edit Hole"),
                d.feature().is_some(),
            ),
            Dialog::Pattern(d) => {
                let (icon, title) = match d.kind {
                    PatternKind::Linear => (Icon::LinearPattern, "Linear Pattern"),
                    PatternKind::Circular => (Icon::CircularPattern, "Circular Pattern"),
                    PatternKind::Mirror => (Icon::Mirror, "Mirror"),
                };
                let directions = d.kind != PatternKind::Linear || !d.second || d.axis2.is_some();
                let copies =
                    !d.seeds.is_empty() || (d.kind == PatternKind::Mirror && !d.bodies.is_empty());
                (icon, title, copies && directions)
            }
            Dialog::Combine(d) => (
                Icon::Combine,
                editing(d.editing, "Combine", "Edit Combine"),
                d.target.is_some() && !d.tools.is_empty(),
            ),
            Dialog::RefPlane(d) => (Icon::RefPlane, "Plane", d.feature().is_ok()),
            Dialog::RefAxis(_) => (Icon::RefAxis, "Axis", true),
            Dialog::Appearance(d) => {
                let (title, ready) = self.appearance_header(d);
                (Icon::Appearance, title, ready)
            }
            _ => (Icon::Part, "", false),
        }
    }

    /// Returns true when the command finished by itself.
    fn panel_body(&mut self, ui: &mut egui::Ui, dialog: &mut Dialog) -> bool {
        match dialog {
            Dialog::ChoosePlane => return self.choose_plane_body(ui),
            Dialog::Extrude(d) => self.extrude_body(ui, d),
            Dialog::Revolve(d) => self.revolve_body(ui, d),
            Dialog::Sweep(d) => self.sweep_body(ui, d),
            Dialog::Loft(d) => self.loft_body(ui, d),
            Dialog::Blend(d) => self.blend_body(ui, d),
            Dialog::Shell(d) => self.shell_body(ui, d),
            Dialog::Hole(d) => {
                self.hole_body(ui, d);
                self.bodies_box(ui, ExtrudeOp::Cut, &mut d.bodies);
            }
            Dialog::Pattern(d) => self.pattern_body(ui, d),
            Dialog::Combine(d) => self.combine_body(ui, d),
            Dialog::RefPlane(d) => crate::plane_ui::ref_plane_body(ui, d),
            Dialog::RefAxis(d) => ref_axis_body(ui, d),
            Dialog::Appearance(d) => return self.appearance_body(ui, d),
            _ => {}
        }
        false
    }

    fn choose_plane_body(&mut self, ui: &mut egui::Ui) -> bool {
        ui.label("Select a plane in the tree or the graphics area, or a flat face of the part.");
        ui.add_space(4.0);
        let mut chosen = None;
        for plane in [
            StandardPlane::Front,
            StandardPlane::Top,
            StandardPlane::Right,
        ] {
            ui.horizontal(|ui| {
                icons::small(ui, Icon::Plane, 16.0);
                if ui.link(format!("{} Plane", plane_name(plane))).clicked() {
                    chosen = Some(SketchPlane::Standard(plane));
                }
            });
        }
        for (label, plane) in self.plane_choices(None) {
            if let PlaneRef::Reference(id) = plane {
                ui.horizontal(|ui| {
                    icons::small(ui, Icon::RefPlane, 16.0);
                    if ui.link(label).clicked() {
                        chosen = Some(SketchPlane::Reference(id));
                    }
                });
            }
        }
        match chosen {
            Some(plane) => {
                self.start_new_sketch(plane);
                true
            }
            None => false,
        }
    }

    fn extrude_body(&self, ui: &mut egui::Ui, d: &mut ExtrudeDialog) {
        section(ui, "Sketch");
        let items: Vec<(String, String)> = d
            .sketch
            .map(|id| (self.feature_name(id), String::new()))
            .into_iter()
            .collect();
        if selection_box(ui, &items, "Select a sketch in the tree").is_some() {
            d.sketch = None;
        }
        let has_body = self.doc.part.body().is_some();
        section(ui, "Operation");
        operation(ui, &mut d.op, has_body);
        self.bodies_box(ui, d.op, &mut d.bodies);
        section(ui, "Direction");
        egui::Grid::new("extrude_panel")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("End condition");
                egui::ComboBox::from_id_salt("extrude_end")
                    .selected_text(d.end.label())
                    .show_ui(ui, |ui| {
                        for (end, label, hint) in EndCondition::ALL {
                            // Through All goes through bodies: there must be one.
                            let through = matches!(
                                end,
                                EndCondition::ThroughAll | EndCondition::ThroughAllBoth
                            );
                            let enabled = !through || has_body || d.end == end;
                            let response = ui.add_enabled(
                                enabled,
                                egui::SelectableLabel::new(d.end == end, label),
                            );
                            let response = if enabled {
                                response.on_hover_text(hint)
                            } else {
                                response.on_disabled_hover_text("There is no body yet")
                            };
                            if response.clicked() {
                                d.end = end;
                                // Up To Surface waits for its surface.
                                d.up_to_next =
                                    end == EndCondition::UpToSurface && d.up_to.is_none();
                            }
                        }
                    });
                ui.end_row();
                if d.end.has_depth() {
                    ui.label("Depth");
                    ui.horizontal(|ui| {
                        positive_length(ui, &mut d.depth);
                        dimension_note(ui, d.dimension.as_ref());
                    });
                    ui.end_row();
                }
            });
        if d.end == EndCondition::UpToSurface {
            if reference_box(
                ui,
                "extrude_up_to",
                &d.planes,
                &mut d.up_to,
                d.up_to_next,
                "Select a flat face or a plane",
            ) {
                d.up_to_next = true;
            }
            ui.label(
                RichText::new(
                    "It goes towards the surface, on whichever side of the sketch it is.",
                )
                .small()
                .weak(),
            );
        }
        if d.end.reversible() {
            ui.checkbox(&mut d.reverse, "Reverse direction");
        }
    }

    fn revolve_body(&self, ui: &mut egui::Ui, d: &mut RevolveDialog) {
        section(ui, "Sketch");
        let items: Vec<(String, String)> = d
            .sketch
            .map(|id| (self.feature_name(id), String::new()))
            .into_iter()
            .collect();
        if selection_box(ui, &items, "Select a sketch in the tree").is_some() {
            d.sketch = None;
        }
        section(ui, "Operation");
        operation(ui, &mut d.op, self.doc.part.body().is_some());
        self.bodies_box(ui, d.op, &mut d.bodies);
        section(ui, "Axis");
        reference_box_set(
            ui,
            "revolve_axis",
            &d.axes,
            &mut d.axis,
            true,
            "Select an edge, a round face or an axis",
        );
        ui.label(
            RichText::new("The arrow lists the sketch's centerlines too.")
                .small()
                .weak(),
        );
        section(ui, "Angle");
        ui.horizontal(|ui| {
            degrees(ui, &mut d.angle);
            dimension_note(ui, d.dimension.as_ref());
        });
        ui.checkbox(&mut d.mid_plane, "Mid-plane (half on each side)");
        if !d.mid_plane {
            ui.checkbox(&mut d.reverse, "Reverse direction");
        }
    }

    fn sweep_body(&self, ui: &mut egui::Ui, d: &mut SweepDialog) {
        let active = |ui: &mut egui::Ui, on: bool, items: &[(String, String)], hint: &str| {
            let frame = egui::Frame::none()
                .stroke(Stroke::new(
                    if on { 2.0_f32 } else { 0.0_f32 },
                    box_active(),
                ))
                .rounding(4.0);
            frame.show(ui, |ui| selection_box(ui, items, hint)).inner
        };
        section(ui, "Profile");
        let profile: Vec<(String, String)> = d
            .profile
            .map(|id| (self.feature_name(id), String::new()))
            .into_iter()
            .collect();
        if active(
            ui,
            !d.path_next,
            &profile,
            "Select the sketch to sweep in the tree",
        )
        .is_some()
        {
            d.profile = None;
            d.path_next = false;
        }
        section(ui, "Path");
        let path: Vec<(String, String)> = d
            .path
            .map(|id| (self.feature_name(id), String::new()))
            .into_iter()
            .collect();
        if active(
            ui,
            d.path_next,
            &path,
            "Select the sketch to sweep along in the tree",
        )
        .is_some()
        {
            d.path = None;
            d.path_next = true;
        }
        ui.horizontal(|ui| {
            if ui
                .small_button("Pick the profile")
                .on_hover_text("The next sketch clicked is the profile")
                .clicked()
            {
                d.path_next = false;
            }
            if ui
                .small_button("Pick the path")
                .on_hover_text("The next sketch clicked is the path")
                .clicked()
            {
                d.path_next = true;
            }
        });
        ui.label(
            RichText::new(
                "The profile's closed contours follow the path's curves, kept at the same \
                 angle to it. The path starts on the profile's plane, as a rule.",
            )
            .small()
            .weak(),
        );
        section(ui, "Operation");
        operation(ui, &mut d.op, self.doc.part.body().is_some());
        self.bodies_box(ui, d.op, &mut d.bodies);
    }

    fn loft_body(&self, ui: &mut egui::Ui, d: &mut crate::features_ui::LoftDialog) {
        section(ui, "Profiles, in order");
        let items: Vec<(String, String)> = d
            .profiles
            .iter()
            .enumerate()
            .map(|(i, id)| {
                (
                    format!("{}. {}", i + 1, self.feature_name(*id)),
                    String::new(),
                )
            })
            .collect();
        if let Some(i) = selection_box(ui, &items, "Click the sketches in the tree, in order") {
            d.profiles.remove(i);
        }
        ui.horizontal(|ui| {
            if ui
                .small_button("Reverse order")
                .on_hover_text("The last profile first")
                .clicked()
            {
                d.profiles.reverse();
            }
        });
        ui.label(
            RichText::new(
                "Each sketch is one closed contour; the solid goes through them in this order.",
            )
            .small()
            .weak(),
        );
        ui.checkbox(&mut d.ruled, "Straight between profiles")
            .on_hover_text("Ruled: flat or straight sides from one profile to the next");
        section(ui, "Operation");
        operation(ui, &mut d.op, self.doc.part.body().is_some());
        self.bodies_box(ui, d.op, &mut d.bodies);
    }

    /// The bodies a join, cut or intersection works on, when the part has several.
    fn bodies_box(&self, ui: &mut egui::Ui, op: ExtrudeOp, bodies: &mut Vec<oxyde_model::BodyId>) {
        if op == ExtrudeOp::NewBody || self.body_count() < 2 {
            return;
        }
        section(ui, "Bodies");
        let items: Vec<(String, String)> = bodies
            .iter()
            .map(|b| (self.body_label(*b), String::new()))
            .collect();
        let hint = match op {
            ExtrudeOp::Cut => "Automatic: every body. Click faces to choose",
            _ => "Automatic: the body under the sketch. Click faces to choose",
        };
        if let Some(i) = selection_box(ui, &items, hint) {
            bodies.remove(i);
        }
    }

    fn combine_body(&self, ui: &mut egui::Ui, d: &mut CombineDialog) {
        section(ui, "Target body");
        let target: Vec<(String, String)> = d
            .target
            .map(|b| (self.body_label(b), String::new()))
            .into_iter()
            .collect();
        let active = |ui: &mut egui::Ui, on: bool, items: &[(String, String)], hint: &str| {
            let frame = egui::Frame::none()
                .stroke(Stroke::new(
                    if on { 2.0_f32 } else { 0.0_f32 },
                    box_active(),
                ))
                .rounding(4.0);
            frame.show(ui, |ui| selection_box(ui, items, hint)).inner
        };
        if active(
            ui,
            !d.tools_next,
            &target,
            "Click a face of the body to change",
        )
        .is_some()
        {
            d.target = None;
            d.tools_next = false;
        }
        if ui
            .small_button("Pick the target")
            .on_hover_text("The next click sets the target body")
            .clicked()
        {
            d.tools_next = false;
        }
        section(ui, "Tool bodies");
        let tools: Vec<(String, String)> = d
            .tools
            .iter()
            .map(|b| (self.body_label(*b), String::new()))
            .collect();
        if let Some(i) = active(ui, d.tools_next, &tools, "Click faces of the other bodies") {
            d.tools.remove(i);
        }
        section(ui, "Operation");
        ui.horizontal(|ui| {
            for (op, icon, label) in [
                (CombineOp::Join, Icon::Add, "Join"),
                (CombineOp::Cut, Icon::Remove, "Cut"),
                (CombineOp::Intersect, Icon::Intersect, "Intersect"),
            ] {
                icons::small(ui, icon, 16.0);
                ui.radio_value(&mut d.op, op, label);
            }
        });
        ui.checkbox(&mut d.keep_tools, "Keep the tool bodies");
    }

    fn blend_body(&self, ui: &mut egui::Ui, d: &mut BlendDialog) {
        section(
            ui,
            if d.fillet {
                "Edges to fillet"
            } else {
                "Edges to chamfer"
            },
        );
        let items: Vec<(String, String)> = d
            .edges
            .iter()
            .enumerate()
            .map(|(i, e)| (format!("Edge {}", i + 1), e.to_string()))
            .collect();
        if let Some(i) = selection_box(ui, &items, "Click edges (or a face) in the graphics area") {
            d.edges.remove(i);
        }
        if !d.fillet {
            section(ui, "Chamfer type");
            for (t, label, hint) in [
                (
                    ChamferType::Distance,
                    "Distance",
                    "The same distance on both faces (45°)",
                ),
                (
                    ChamferType::TwoDistances,
                    "Distance - Distance",
                    "A distance on each face",
                ),
                (
                    ChamferType::AngleDistance,
                    "Angle - Distance",
                    "A distance on one face, at an angle from it",
                ),
            ] {
                ui.radio_value(&mut d.chamfer_type, t, label)
                    .on_hover_text(hint);
            }
        }
        section(ui, if d.fillet { "Radius" } else { "Distance" });
        ui.horizontal(|ui| {
            positive_length(ui, &mut d.size);
            dimension_note(ui, d.dimension.as_ref());
        });
        if !d.fillet && d.chamfer_type != ChamferType::Distance {
            match d.chamfer_type {
                ChamferType::TwoDistances => {
                    section(ui, "Second distance");
                    positive_length(ui, &mut d.second);
                }
                _ => {
                    section(ui, "Angle");
                    ui.add(
                        egui::DragValue::new(&mut d.angle)
                            .speed(0.5)
                            .range(0.1..=89.9)
                            .suffix("°")
                            .custom_parser(crate::dialogs::angle_parser),
                    );
                }
            }
            ui.checkbox(&mut d.flip, "Flip direction")
                .on_hover_text("The first distance on the edge's other face");
        }
    }

    fn shell_body(&self, ui: &mut egui::Ui, d: &mut ShellDialog) {
        section(ui, "Faces to remove");
        let items: Vec<(String, String)> = d
            .faces
            .iter()
            .enumerate()
            .map(|(i, f)| (format!("Face {}", i + 1), f.to_string()))
            .collect();
        if let Some(i) = selection_box(
            ui,
            &items,
            "Click faces in the graphics area (none: a closed hollow part)",
        ) {
            d.faces.remove(i);
        }
        section(ui, "Thickness");
        ui.horizontal(|ui| {
            positive_length(ui, &mut d.thickness);
            dimension_note(ui, d.dimension.as_ref());
        });
        ui.checkbox(&mut d.outward, "Shell outward");
    }

    fn pattern_body(&self, ui: &mut egui::Ui, d: &mut PatternDialog) {
        section(ui, "Features to copy");
        let items: Vec<(String, String)> = d
            .seeds
            .iter()
            .map(|id| (self.feature_name(*id), String::new()))
            .collect();
        if let Some(i) = selection_box(ui, &items, "Click extrusions or revolves in the tree") {
            d.seeds.remove(i);
        }
        match d.kind {
            PatternKind::Linear => {
                section(ui, "Direction 1");
                let first_active = !(d.second && d.second_next) && !d.skip_next;
                if reference_box_set(
                    ui,
                    "pattern_axis",
                    &d.axes,
                    &mut d.axis,
                    first_active,
                    "Select an edge, a face or an axis",
                ) {
                    d.second_next = false;
                    d.skip_next = false;
                }
                egui::Grid::new("pattern_d1").num_columns(2).show(ui, |ui| {
                    ui.label("Spacing");
                    positive_length(ui, &mut d.spacing);
                    ui.end_row();
                    ui.label("Instances");
                    count(ui, &mut d.count);
                    ui.end_row();
                });
                ui.checkbox(&mut d.reverse, "Reverse direction");
                ui.add_space(4.0);
                if ui.checkbox(&mut d.second, "Direction 2").changed() {
                    // Turned on: the next click sets it.
                    d.second_next = d.second;
                    d.skip_next &= !d.second;
                }
                if d.second {
                    if reference_box(
                        ui,
                        "pattern_axis2",
                        &d.axes,
                        &mut d.axis2,
                        d.second_next && !d.skip_next,
                        "Select an edge, a face or an axis",
                    ) {
                        d.second_next = true;
                        d.skip_next = false;
                    }
                    egui::Grid::new("pattern_d2").num_columns(2).show(ui, |ui| {
                        ui.label("Spacing");
                        positive_length(ui, &mut d.spacing2);
                        ui.end_row();
                        ui.label("Instances");
                        count(ui, &mut d.count2);
                        ui.end_row();
                    });
                    ui.checkbox(&mut d.reverse2, "Reverse direction 2");
                }
                instances_to_skip(ui, d);
            }
            PatternKind::Circular => {
                section(ui, "Axis");
                if reference_box_set(
                    ui,
                    "pattern_axis",
                    &d.axes,
                    &mut d.axis,
                    !d.skip_next,
                    "Select a round edge or face, or an axis",
                ) {
                    d.skip_next = false;
                }
                egui::Grid::new("pattern_circ")
                    .num_columns(2)
                    .show(ui, |ui| {
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
                    });
                ui.checkbox(&mut d.equal_spacing, "Equal spacing");
                ui.checkbox(&mut d.reverse, "Reverse direction");
                instances_to_skip(ui, d);
            }
            PatternKind::Mirror => {
                section(ui, "Mirror plane");
                if reference_box_set(
                    ui,
                    "mirror_plane",
                    &d.planes,
                    &mut d.plane,
                    !d.bodies_next,
                    "Select a plane or a flat face",
                ) {
                    d.bodies_next = false;
                }
                section(ui, "Bodies to mirror");
                let items: Vec<(String, String)> = d
                    .bodies
                    .iter()
                    .map(|b| (self.body_label(*b), String::new()))
                    .collect();
                if let Some(i) =
                    selection_box(ui, &items, "None: the features above are copied instead")
                {
                    d.bodies.remove(i);
                }
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(d.bodies_next, "Pick bodies")
                        .on_hover_text(
                            "Clicks on faces pick the bodies to mirror, until the plane's box is \
                             clicked; the Solid Bodies folder picks them too",
                        )
                        .clicked()
                    {
                        d.bodies_next = !d.bodies_next;
                    }
                });
                ui.label(
                    RichText::new(
                        "Each body is joined to its mirror image: half a symmetric part made whole.",
                    )
                    .small()
                    .weak(),
                );
            }
        }
        if !d.dimensions.is_empty() {
            ui.label(RichText::new(d.dimensions.join(", ")).small().weak());
        }
    }
}

/// SolidWorks' Instances to Skip: the copies a linear or circular pattern leaves out. They
/// are clicked in the graphics area while *Pick instances* is on, or ticked in the list of
/// every copy by number; a copy left out is no longer there to click, so it is put back
/// from the box or the list.
fn instances_to_skip(ui: &mut egui::Ui, d: &mut PatternDialog) {
    section(ui, "Instances to Skip");
    let skipped = d.skipped();
    let items: Vec<(String, String)> = skipped
        .iter()
        .map(|instance| (d.instance_label(*instance), String::new()))
        .collect();
    let removed = selection_box(ui, &items, "None: every instance is made");
    if let Some(instance) = removed.and_then(|i| skipped.get(i)) {
        d.toggle_skip(*instance);
    }
    ui.horizontal(|ui| {
        if ui
            .selectable_label(d.skip_next, "Pick instances")
            .on_hover_text(
                "Clicks on the copies in the graphics area leave them out, until a direction's \
                 box is clicked",
            )
            .clicked()
        {
            d.skip_next = !d.skip_next;
        }
        if ui
            .selectable_label(d.skip_numbers, "List by number")
            .on_hover_text("Every copy, numbered from the seed, to tick the ones to leave out")
            .clicked()
        {
            d.skip_numbers = !d.skip_numbers;
        }
    });
    if d.skip_numbers {
        let row = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical()
            .id_salt("pattern_instance_list")
            .max_height(110.0)
            .auto_shrink([false, true])
            .show_rows(ui, row, d.copies(), |ui, rows| {
                for index in rows {
                    let instance = d.copy(index);
                    let mut skip = d.is_skipped(instance);
                    let label = format!("Skip {}", d.instance_label(instance));
                    if ui.checkbox(&mut skip, label).changed() {
                        d.toggle_skip(instance);
                    }
                }
            });
    }
}

fn ref_axis_body(ui: &mut egui::Ui, d: &mut AxisDialog) {
    ui.radio_value(&mut d.two_planes, false, "Along an edge, face or axis");
    ui.radio_value(&mut d.two_planes, true, "Where two planes meet");
    if d.two_planes {
        section(ui, "First plane");
        let hint = "Select a plane or a flat face";
        if reference_box_set(
            ui,
            "axis_plane_a",
            &d.planes,
            &mut d.plane_a,
            !d.next_b,
            hint,
        ) {
            d.next_b = false;
        }
        section(ui, "Second plane");
        if reference_box_set(
            ui,
            "axis_plane_b",
            &d.planes,
            &mut d.plane_b,
            d.next_b,
            hint,
        ) {
            d.next_b = true;
        }
    } else {
        section(ui, "Along");
        reference_box_set(
            ui,
            "axis_along",
            &d.axes,
            &mut d.axis,
            true,
            "Select an edge, a face or an axis",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The height each box takes in a tall panel, as the left panel is.
    fn heights(add: impl Fn(&mut egui::Ui)) -> f32 {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(300.0, 900.0),
            )),
            ..Default::default()
        };
        let mut height = 0.0;
        let _ = ctx.run(input, |ctx| {
            egui::SidePanel::left("panel").show(ctx, |ui| {
                let top = ui.cursor().top();
                add(ui);
                height = ui.cursor().top() - top;
            });
        });
        height
    }

    /// The boxes take a line of text, not the rest of the panel: a right-to-left layout
    /// straight in the box once stretched the Direction 1 box of Linear Pattern to the
    /// bottom of the window, hiding everything under it.
    #[test]
    fn boxes_are_one_line_high() {
        let axes = vec![("X axis".to_owned(), ()), ("Y axis".to_owned(), ())];
        for active in [true, false] {
            let filled = heights(|ui| {
                reference_box_set(ui, "axis", &axes, &mut 0, active, "Select an axis");
            });
            let empty = heights(|ui| {
                reference_box(ui, "axis", &axes, &mut None, active, "Select an axis");
            });
            assert!(filled < 40.0, "a filled reference box is {filled} high");
            assert!(empty < 40.0, "an empty reference box is {empty} high");
        }
        let items = vec![("Cut-Extrude1".to_owned(), String::new())];
        let selection = heights(|ui| {
            selection_box(ui, &items, "Click features");
        });
        assert!(selection < 60.0, "a selection box is {selection} high");
    }

    /// The window the panels are drawn in: as tall as a laptop's, wide enough for the left
    /// panel to take its default width.
    const SCREEN: egui::Vec2 = egui::vec2(1200.0, 900.0);

    /// The tallest a row or a box of a panel may be. A selection box's list is the tallest
    /// thing in a panel, and it scrolls past 110 points.
    const ROW_LIMIT: f32 = 130.0;

    /// A PropertyManager as the left panel draws it.
    struct Drawn {
        /// The rectangle of every widget and layout egui laid out, as far as it shows: a
        /// scrolling list is as high as its viewport, not as all its rows.
        widgets: Vec<egui::Rect>,
        /// Where the PropertyManager starts and ends in the panel.
        top: f32,
        bottom: f32,
    }

    /// Draws the open command's PropertyManager in the left panel, set up as the app sets
    /// it up. Twice: grids and scroll areas size themselves on the first frame.
    fn draw_panel(ctx: &egui::Context, app: &mut OxydeApp) -> Drawn {
        let (mut top, mut bottom) = (0.0, 0.0);
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::SidePanel::left("feature_manager")
                    .resizable(true)
                    .default_width(290.0)
                    .min_width(220.0)
                    .show(ctx, |ui| {
                        top = ui.cursor().top();
                        app.property_manager(ui);
                        bottom = ui.cursor().top();
                    });
            });
        }
        let widgets = ctx.viewport(|v| {
            v.prev_pass
                .widgets
                .layers()
                .flat_map(|(_, widgets)| widgets.iter().map(|w| w.interact_rect))
                .filter(|r| r.is_finite())
                .collect()
        });
        Drawn {
            widgets,
            top,
            bottom,
        }
    }

    /// Opens a command's panel.
    type OpenPanel = fn(&mut OxydeApp);

    /// Every PropertyManager panel, opened on an empty part as its command opens it, with
    /// the options that show the most of it.
    fn panels() -> Vec<(&'static str, OpenPanel)> {
        vec![
            ("Sketch", |app| app.open_dialog(Dialog::ChoosePlane)),
            ("Extrude", |app| app.open_extrude(None, None, None)),
            ("Extrude up to a surface", |app| {
                app.open_extrude(None, None, None);
                if let Some(Dialog::Extrude(d)) = &mut app.dialog {
                    d.end = EndCondition::UpToSurface;
                }
            }),
            ("Revolve", |app| app.begin_revolve()),
            ("Sweep", |app| app.begin_sweep()),
            ("Loft", |app| app.open_loft(None, None)),
            ("Fillet", |app| {
                app.open_dialog(Dialog::Blend(BlendDialog::new(true, Vec::new())));
            }),
            ("Chamfer", |app| {
                let mut d = BlendDialog::new(false, Vec::new());
                d.chamfer_type = ChamferType::AngleDistance;
                app.open_dialog(Dialog::Blend(d));
            }),
            ("Shell", |app| app.open_shell(None, None)),
            ("Hole Wizard", |app| app.begin_hole()),
            ("Linear Pattern", |app| {
                app.open_pattern(None, PatternKind::Linear, None);
                if let Some(Dialog::Pattern(d)) = &mut app.dialog {
                    d.second = true;
                    d.second_next = true;
                }
            }),
            ("Linear Pattern skipping instances", |app| {
                app.open_pattern(None, PatternKind::Linear, None);
                if let Some(Dialog::Pattern(d)) = &mut app.dialog {
                    d.second = true;
                    d.count = 10;
                    d.count2 = 10;
                    d.skip = vec![[0, 1], [1, 0], [1, 1], [2, 2], [3, 3], [4, 4], [5, 5]];
                    d.skip_numbers = true;
                    d.skip_next = true;
                }
            }),
            ("Circular Pattern", |app| {
                app.open_pattern(None, PatternKind::Circular, None);
            }),
            ("Mirror", |app| {
                app.open_pattern(None, PatternKind::Mirror, None)
            }),
            ("Combine", |app| app.open_combine(None, None)),
            ("Plane", |app| app.open_ref_plane(None, None)),
            ("Axis", |app| app.open_ref_axis(None, None)),
            ("Axis where two planes meet", |app| {
                app.open_ref_axis(None, None);
                if let Some(Dialog::RefAxis(d)) = &mut app.dialog {
                    d.two_planes = true;
                }
            }),
        ]
    }

    /// No row or box of any panel grows past a few lines, and every panel fits in the
    /// window with room to spare: a right-to-left layout straight in a reference box once
    /// stretched Linear Pattern's Direction 1 box to the bottom of the window, hiding
    /// everything under it.
    #[test]
    fn panels_fit_in_the_window() {
        let ctx = egui::Context::default();
        let mut app = OxydeApp::blank(crate::prefs::Preferences::default(), None);
        app.apply_preferences(&ctx);
        // Every panel is checked before failing, to name all the panels a change broke.
        let mut problems = Vec::new();
        for (name, open) in panels() {
            open(&mut app);
            assert!(
                app.dialog.as_ref().is_some_and(Dialog::is_panel),
                "{name} opened no panel"
            );
            let drawn = draw_panel(&ctx, &mut app);
            assert!(drawn.widgets.len() > 10, "{name} drew next to nothing");
            let height = drawn.bottom - drawn.top;
            if drawn.bottom > SCREEN.y - 100.0 {
                problems.push(format!(
                    "the {name} panel is {height} high and leaves no room for the tree"
                ));
            }
            // The panel, its frame and the resize handle hold the whole PropertyManager, or
            // all of it the window shows.
            let bottom = drawn.bottom.min(SCREEN.y);
            let tallest = drawn
                .widgets
                .iter()
                .filter(|r| !(r.top() <= drawn.top + 10.0 && r.bottom() >= bottom - 30.0))
                .max_by(|a, b| a.height().total_cmp(&b.height()));
            if let Some(rect) = tallest.filter(|r| r.height() > ROW_LIMIT) {
                problems.push(format!(
                    "a row of the {name} panel is {} high, at {rect:?}",
                    rect.height()
                ));
            }
            app.close_dialog();
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// Instances to Skip: a click on a copy in the preview is refused until *Pick instances*
    /// is on, then leaves the copy out of the preview. The part is not touched before OK,
    /// which records one undo step; Cancel leaves no trace.
    #[test]
    fn copies_clicked_while_picking_instances_are_left_out() {
        use oxyde_geom::Point2;
        use oxyde_model::{Extrude, LinearPattern, PatternDirection, Sketch, StandardAxis};

        let ctx = egui::Context::default();
        let mut app = OxydeApp::blank(crate::prefs::Preferences::default(), None);
        // A plate with a hole, patterned three times along X.
        let part = &mut app.doc.part;
        let mut plate = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
        plate.rectangle(Point2::ORIGIN, 100.0, 20.0);
        let s1 = part.add_feature(FeatureKind::Sketch(plate));
        let base = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            10.0,
        )));
        let mut drawing = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
        let circle = drawing.circle(Point2::new(10.0, 10.0), 3.0);
        let s2 = part.add_feature(FeatureKind::Sketch(drawing));
        let hole = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s2,
            ExtrudeOp::Cut,
            10.0,
        )));
        let pattern = part.add_feature(FeatureKind::LinearPattern(LinearPattern {
            seeds: vec![hole],
            first: PatternDirection {
                axis: AxisRef::Standard(StandardAxis::X),
                spacing: 20.0,
                count: 3,
                reverse: false,
            },
            second: None,
            skip: Vec::new(),
        }));
        app.rebuild();
        let saved = (app.doc.undo_depth(), app.doc.modified);
        let copy = |i: u32| {
            FaceName::new(FaceKey::Instance {
                feature: pattern,
                instance: [i, 0],
                face: Box::new(FaceName::profile(hole, circle)),
            })
        };
        let in_part = |app: &OxydeApp, i| {
            let body = app.doc.part.body().unwrap();
            body.naming.find_face(&copy(i)).is_some()
        };
        let shown = |app: &OxydeApp, i| {
            let body = app.shown_body().unwrap();
            body.naming.find_face(&copy(i)).is_some()
        };
        let skip = |app: &OxydeApp| match &app.dialog {
            Some(Dialog::Pattern(d)) => d.skip.clone(),
            _ => panic!("no pattern panel"),
        };
        let edit = |app: &mut OxydeApp| {
            let kind = app.doc.part.feature(pattern).unwrap().kind().clone();
            app.open_pattern(Some(pattern), PatternKind::Linear, Some(&kind));
            app.sync_preview(&ctx);
        };

        edit(&mut app);
        assert!(shown(&app, 1));
        assert!(app.offer_to_panel(PanelPick::Face(copy(1))));
        assert!(app.status.contains("Pick instances"), "{}", app.status);
        assert!(skip(&app).is_empty());
        if let Some(Dialog::Pattern(d)) = &mut app.dialog {
            d.skip_next = true;
        }
        assert!(app.offer_to_panel(PanelPick::Face(copy(1))));
        assert_eq!(skip(&app), [[1, 0]]);
        app.sync_preview(&ctx);
        assert!(!shown(&app, 1));
        assert!(shown(&app, 2));
        assert!(in_part(&app, 1));
        assert_eq!((app.doc.undo_depth(), app.doc.modified), saved);

        // Cancel: the part as it was, nothing to undo or save.
        app.close_dialog();
        assert!(in_part(&app, 1));
        assert_eq!((app.doc.undo_depth(), app.doc.modified), saved);

        // OK, the copy ticked in the list: one undo step, and the part leaves it out.
        edit(&mut app);
        let Some(Dialog::Pattern(mut d)) = app.dialog.take() else {
            panic!("no pattern panel");
        };
        d.toggle_skip(d.copy(0));
        app.clear_preview();
        app.apply_dialog(Dialog::Pattern(d), &ctx);
        assert_eq!(app.doc.undo_depth(), saved.0 + 1);
        assert!(!in_part(&app, 1));
        assert!(in_part(&app, 2));
        app.doc.release(&mut app.kernel);
    }
}
