//! The PropertyManager: a feature's options, shown at the top of the left panel while the
//! feature is created or edited, as in SolidWorks.
//!
//! The viewport and the timeline stay live: clicking a sketch, a face, an edge, a plane or
//! a feature fills the matching selection box of the open command (the sketch to extrude,
//! the edges to fillet, the features to pattern…) instead of selecting it. Commands opened
//! with something already selected start with it.

use eframe::egui::{self, Color32, RichText, Stroke};
use oxyde_model::{
    AxisRef, CombineOp, EdgeName, ExtrudeOp, FaceName, FeatureId, FeatureKind, PlaneRef,
    RevolveAxis, SketchPlane, StandardPlane,
};
use oxyde_render::Pick;

use crate::app::OxydeApp;
use crate::dialogs::{Answer, BlendDialog, Dialog, ExtrudeDialog, plane_name, positive_length};
use crate::features_ui::{
    AxisDialog, Choices, CombineDialog, PatternDialog, PatternKind, PlaneDialog, RevolveDialog,
    ShellDialog,
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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("▾", |ui| {
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
            .suffix("°"),
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

/// What an extrusion or a revolve does: join, cut, intersect or a new body (Fusion's
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
    fn usable(&self, id: FeatureId, editing: Option<FeatureId>) -> bool {
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

    /// Whether a face, an edge or a body clicked in the finished part exists just before the
    /// feature being edited, the only geometry that feature can use.
    fn exists_before(&self, pick: &PanelPick, editing: FeatureId) -> bool {
        let body = self.doc.part.body_before(editing);
        match pick {
            PanelPick::Face(f) => body.is_some_and(|b| b.naming.find_face(f).is_some()),
            PanelPick::Edge(e) => body.is_some_and(|b| b.naming.find_edge(e).is_some()),
            PanelPick::Body(id) => body.is_some_and(|b| b.solid(*id).is_some()),
            PanelPick::Feature(_) | PanelPick::Plane(_) => true,
        }
    }

    /// Offers a click to the open command. Returns true when the command took it.
    pub(crate) fn offer_to_panel(&mut self, pick: PanelPick) -> bool {
        // The viewport shows the finished part: geometry made or changed by later features
        // cannot be used by the feature being edited (and would stop the file from opening).
        if let Some(editing) = self.dialog.as_ref().and_then(Dialog::editing) {
            if self.dialog.as_ref().is_some_and(Dialog::is_panel)
                && !self.exists_before(&pick, editing)
            {
                self.status = format!(
                    "That is made or changed by a feature after {}: pick geometry that exists before it",
                    self.feature_name(editing)
                );
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
                    PanelPick::Feature(id)
                        if matches!(
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
                // A body, or a face of one: in or out of the feature's scope.
                other => match self.body_of_pick(&other) {
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
                    let body = self.body_of_pick(&pick).expect("checked");
                    toggle(&mut d.bodies, body);
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
                if let Some(id) = seed {
                    match d.seeds.iter().position(|s| *s == id) {
                        Some(i) => {
                            d.seeds.remove(i);
                        }
                        None => d.seeds.push(id),
                    }
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
            Dialog::RefPlane(d) => match self.plane_of_pick(&pick, d.editing) {
                Some((plane, label)) => {
                    d.base = choose(&mut d.planes, plane, label);
                    true
                }
                None => false,
            },
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
        let body = self.doc.part.body();
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
            Dialog::RefPlane(d) => plane(d.planes.get(d.base).map(|(_, p)| p))
                .into_iter()
                .collect(),
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
            Some(Dialog::Pattern(d)) => d.seeds.clone(),
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
                d.sketch.is_some(),
            ),
            Dialog::Revolve(d) => (
                Icon::Revolve,
                editing(d.editing, "Revolve", "Edit Revolve"),
                d.sketch.is_some() && !d.axes.is_empty(),
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
            Dialog::Pattern(d) => {
                let (icon, title) = match d.kind {
                    PatternKind::Linear => (Icon::LinearPattern, "Linear Pattern"),
                    PatternKind::Circular => (Icon::CircularPattern, "Circular Pattern"),
                    PatternKind::Mirror => (Icon::Mirror, "Mirror"),
                };
                let directions = d.kind != PatternKind::Linear || !d.second || d.axis2.is_some();
                (icon, title, !d.seeds.is_empty() && directions)
            }
            Dialog::Combine(d) => (
                Icon::Combine,
                editing(d.editing, "Combine", "Edit Combine"),
                d.target.is_some() && !d.tools.is_empty(),
            ),
            Dialog::RefPlane(_) => (Icon::RefPlane, "Plane", true),
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
            Dialog::Blend(d) => self.blend_body(ui, d),
            Dialog::Shell(d) => self.shell_body(ui, d),
            Dialog::Pattern(d) => self.pattern_body(ui, d),
            Dialog::Combine(d) => self.combine_body(ui, d),
            Dialog::RefPlane(d) => ref_plane_body(ui, d),
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
        section(ui, "Operation");
        operation(ui, &mut d.op, self.doc.part.body().is_some());
        self.bodies_box(ui, d.op, &mut d.bodies);
        section(ui, "Direction");
        egui::Grid::new("extrude_panel")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("End condition");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut d.mid_plane, false, "Blind");
                    ui.radio_value(&mut d.mid_plane, true, "Mid-plane");
                });
                ui.end_row();
                ui.label("Depth");
                ui.horizontal(|ui| {
                    positive_length(ui, &mut d.depth);
                    dimension_note(ui, d.dimension.as_ref());
                });
                ui.end_row();
            });
        if !d.mid_plane {
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
                .stroke(Stroke::new(if on { 2.0_f32 } else { 0.0_f32 }, box_active()))
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
        section(ui, if d.fillet { "Radius" } else { "Distance" });
        ui.horizontal(|ui| {
            positive_length(ui, &mut d.size);
            dimension_note(ui, d.dimension.as_ref());
        });
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
                let first_active = !(d.second && d.second_next);
                if reference_box_set(
                    ui,
                    "pattern_axis",
                    &d.axes,
                    &mut d.axis,
                    first_active,
                    "Select an edge, a face or an axis",
                ) {
                    d.second_next = false;
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
                }
                if d.second {
                    if reference_box(
                        ui,
                        "pattern_axis2",
                        &d.axes,
                        &mut d.axis2,
                        d.second_next,
                        "Select an edge, a face or an axis",
                    ) {
                        d.second_next = true;
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
            }
            PatternKind::Circular => {
                section(ui, "Axis");
                reference_box_set(
                    ui,
                    "pattern_axis",
                    &d.axes,
                    &mut d.axis,
                    true,
                    "Select a round edge or face, or an axis",
                );
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
            }
            PatternKind::Mirror => {
                section(ui, "Mirror plane");
                reference_box_set(
                    ui,
                    "mirror_plane",
                    &d.planes,
                    &mut d.plane,
                    true,
                    "Select a plane or a flat face",
                );
            }
        }
        if !d.dimensions.is_empty() {
            ui.label(RichText::new(d.dimensions.join(", ")).small().weak());
        }
    }
}

fn ref_plane_body(ui: &mut egui::Ui, d: &mut PlaneDialog) {
    section(ui, "Offset from");
    reference_box_set(
        ui,
        "plane_base",
        &d.planes,
        &mut d.base,
        true,
        "Select a plane or a flat face",
    );
    section(ui, "Distance");
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut d.offset)
                .speed(0.1)
                .range(-crate::dialogs::MAX_LENGTH..=crate::dialogs::MAX_LENGTH)
                .suffix(" mm"),
        );
        dimension_note(ui, d.dimension.as_ref());
    });
    ui.checkbox(&mut d.flip, "Flip offset");
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
