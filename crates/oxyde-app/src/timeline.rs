//! The left panel: the feature timeline (with reference planes and the rollback bar) and,
//! below it, the properties of the selected feature.

use eframe::egui::{self, Color32, RichText};
use oxyde_geom::{Curve2, Point2};
use oxyde_model::{
    EdgeBlend, EdgeName, Extrude, ExtrudeExtent, ExtrudeOp, FeatureId, FeatureKind, FeatureStatus,
    Sketch, SketchPlane, StandardPlane, TimelineEntry,
};

use crate::app::OxydeApp;
use crate::dialogs::{Dialog, plane_name};

const ERROR: Color32 = Color32::from_rgb(200, 40, 40);
const WARNING: Color32 = Color32::from_rgb(210, 120, 0);
const INACTIVE: Color32 = Color32::from_gray(140);
const ROLLBACK: Color32 = Color32::from_rgb(0, 110, 255);

/// Timeline interactions, applied after drawing.
enum Action {
    SelectPlane(StandardPlane),
    Select(FeatureId),
    Rename(FeatureId),
    Suppress(FeatureId, bool),
    RollBackTo(FeatureId),
    RollForward,
    RollToEnd,
    Delete(FeatureId),
}

impl OxydeApp {
    pub(crate) fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.heading(self.doc.display_name());
        ui.add_space(2.0);

        let mut action = None;
        let max_height = ui.available_height() * 0.55;
        ui.push_id("timeline", |ui| {
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for plane in [
                        StandardPlane::Front,
                        StandardPlane::Top,
                        StandardPlane::Right,
                    ] {
                        let selected = self.selected_plane == Some(plane);
                        let label = format!("{} Plane", plane_name(plane));
                        if ui.selectable_label(selected, label).clicked() {
                            action = Some(Action::SelectPlane(plane));
                        }
                    }
                    ui.label(RichText::new("Origin").color(INACTIVE));
                    ui.separator();

                    let entries = self.doc.part.timeline();
                    let active = self
                        .doc
                        .part
                        .rollback_position()
                        .unwrap_or(entries.len())
                        .min(entries.len());
                    for (index, entry) in entries.iter().enumerate() {
                        if index == active {
                            rollback_bar(ui, true, &mut action);
                        }
                        self.timeline_row(ui, entry, &mut action);
                    }
                    if active == entries.len() {
                        rollback_bar(ui, false, &mut action);
                    }
                });
        });

        if let Some(action) = action {
            self.apply_action(action);
        }
    }

    fn timeline_row(&self, ui: &mut egui::Ui, entry: &TimelineEntry, action: &mut Option<Action>) {
        let suppressed = self
            .doc
            .part
            .feature(entry.id)
            .is_some_and(|f| f.is_suppressed());
        let mut text = match &entry.status {
            FeatureStatus::Error(_) => RichText::new(format!("⚠ {}", entry.name)).color(ERROR),
            FeatureStatus::Warning(_) => RichText::new(format!("⚠ {}", entry.name)).color(WARNING),
            FeatureStatus::Ok => RichText::new(&entry.name),
            FeatureStatus::Suppressed | FeatureStatus::RolledBack | FeatureStatus::Pending => {
                RichText::new(&entry.name).color(INACTIVE)
            }
        };
        if suppressed {
            text = text.strikethrough();
        }

        let selected = self.selected_feature == Some(entry.id);
        let mut response = ui.selectable_label(selected, text);
        if let FeatureStatus::Error(message) | FeatureStatus::Warning(message) = &entry.status {
            response = response.on_hover_text(message.as_str());
        } else {
            response = response.on_hover_text(entry.type_name);
        }
        if response.clicked() {
            *action = Some(Action::Select(entry.id));
        }
        let id = entry.id;
        response.context_menu(|ui| {
            if ui.button("Rename…").clicked() {
                *action = Some(Action::Rename(id));
                ui.close_menu();
            }
            let label = if suppressed { "Unsuppress" } else { "Suppress" };
            if ui.button(label).clicked() {
                *action = Some(Action::Suppress(id, !suppressed));
                ui.close_menu();
            }
            if ui.button("Roll back above this").clicked() {
                *action = Some(Action::RollBackTo(id));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Delete").clicked() {
                *action = Some(Action::Delete(id));
                ui.close_menu();
            }
        });
    }

    fn apply_action(&mut self, action: Action) {
        match action {
            Action::SelectPlane(plane) => {
                self.selected_plane = Some(plane);
                self.selected_feature = None;
                self.doc.end_edit();
            }
            Action::Select(id) => {
                self.selected_feature = Some(id);
                self.doc.end_edit();
            }
            Action::Rename(feature) => {
                let name = self
                    .doc
                    .part
                    .feature(feature)
                    .map(|f| f.name().to_owned())
                    .unwrap_or_default();
                self.dialog = Some(Dialog::Rename { feature, name });
            }
            Action::Suppress(id, suppressed) => {
                self.doc.checkpoint();
                if self.doc.part.set_suppressed(id, suppressed).is_ok() {
                    self.rebuild();
                }
            }
            Action::RollBackTo(id) => {
                self.doc.checkpoint();
                if self.doc.part.roll_back_to(Some(id)).is_ok() {
                    self.rebuild();
                }
            }
            Action::RollForward => {
                let features = self.doc.part.features();
                let next = self
                    .doc
                    .part
                    .rollback_position()
                    .and_then(|active| features.get(active + 1))
                    .map(|f| f.id());
                self.doc.checkpoint();
                let _ = self.doc.part.roll_back_to(next);
                self.rebuild();
            }
            Action::RollToEnd => {
                self.doc.checkpoint();
                let _ = self.doc.part.roll_back_to(None);
                self.rebuild();
            }
            Action::Delete(id) => self.delete_feature(id),
        }
    }

    pub(crate) fn properties_ui(&mut self, ui: &mut egui::Ui) {
        ui.strong("Properties");
        let Some(id) = self.selected_feature else {
            ui.label(RichText::new("Select a feature in the timeline to edit it.").color(INACTIVE));
            return;
        };
        let Some(feature) = self.doc.part.feature(id) else {
            self.selected_feature = None;
            return;
        };
        let name = feature.name().to_owned();
        let original = feature.kind().clone();
        let mut kind = original.clone();

        ui.label(RichText::new(name).strong());
        if let Some(FeatureStatus::Error(message) | FeatureStatus::Warning(message)) =
            self.doc.part.status(id)
        {
            ui.colored_label(ERROR, message);
        }

        let selected_edges = self.selected_edges();
        ui.push_id("properties", |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match &mut kind {
                    FeatureKind::Sketch(sketch) => sketch_editor(ui, sketch),
                    FeatureKind::Extrude(extrude) => extrude_editor(ui, extrude),
                    FeatureKind::Fillet(blend) => {
                        blend_editor(ui, blend, "Radius", &selected_edges)
                    }
                    FeatureKind::Chamfer(blend) => {
                        blend_editor(ui, blend, "Distance", &selected_edges)
                    }
                });
        });

        if kind != original {
            self.doc.checkpoint_edit(id);
            if let Ok(definition) = self.doc.part.edit(id) {
                *definition = kind;
            }
            self.rebuild();
        }
    }
}

fn rollback_bar(ui: &mut egui::Ui, rolled_back: bool, action: &mut Option<Action>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("— Rollback —").color(ROLLBACK).strong());
        if rolled_back {
            if ui.small_button("Forward").clicked() {
                *action = Some(Action::RollForward);
            }
            if ui.small_button("To end").clicked() {
                *action = Some(Action::RollToEnd);
            }
        }
    });
}

fn drag(ui: &mut egui::Ui, value: &mut f64, prefix: &str) {
    ui.add(egui::DragValue::new(value).speed(0.1).prefix(prefix));
}

fn point(ui: &mut egui::Ui, p: &mut Point2) {
    drag(ui, &mut p.x, "x ");
    drag(ui, &mut p.y, "y ");
}

fn positive(ui: &mut egui::Ui, value: &mut f64) {
    ui.add(egui::DragValue::new(value).speed(0.05).suffix(" mm"));
    if *value < 0.001 {
        *value = 0.001;
    }
}

fn sketch_editor(ui: &mut egui::Ui, sketch: &mut Sketch) {
    let plane = match &sketch.plane {
        SketchPlane::Standard(p) => format!("{} Plane", plane_name(*p)),
        SketchPlane::Face(face) => format!("face {face}"),
    };
    ui.label(RichText::new(format!("On {plane}")).small().color(INACTIVE));

    let mut remove = None;
    egui::Grid::new("sketch_entities")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for (index, entity) in sketch.entities.iter_mut().enumerate() {
                let kind = match entity.curve {
                    Curve2::Line { .. } => "Line",
                    Curve2::Arc { .. } => "Arc",
                    Curve2::Circle { .. } => "Circle",
                };
                ui.label(format!("{kind} {}", entity.id));
                ui.vertical(|ui| {
                    match &mut entity.curve {
                        Curve2::Line { start, end } => {
                            ui.horizontal(|ui| point(ui, start));
                            ui.horizontal(|ui| point(ui, end));
                        }
                        Curve2::Arc { start, mid, end } => {
                            ui.horizontal(|ui| point(ui, start));
                            ui.horizontal(|ui| point(ui, mid));
                            ui.horizontal(|ui| point(ui, end));
                        }
                        Curve2::Circle { center, radius } => {
                            ui.horizontal(|ui| point(ui, center));
                            ui.horizontal(|ui| {
                                ui.label("r");
                                positive(ui, radius);
                            });
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut entity.construction, "Construction");
                        if ui.small_button("Remove").clicked() {
                            remove = Some(index);
                        }
                    });
                });
                ui.end_row();
            }
        });
    if let Some(index) = remove {
        sketch.entities.remove(index);
    }
    ui.horizontal(|ui| {
        if ui.button("Add rectangle").clicked() {
            sketch.rectangle(Point2::ORIGIN, 10.0, 10.0);
        }
        if ui.button("Add circle").clicked() {
            sketch.circle(Point2::ORIGIN, 5.0);
        }
    });
}

fn extrude_editor(ui: &mut egui::Ui, extrude: &mut Extrude) {
    ui.horizontal(|ui| {
        ui.radio_value(&mut extrude.op, ExtrudeOp::Boss, "Boss");
        ui.radio_value(&mut extrude.op, ExtrudeOp::Cut, "Cut");
    });
    let mut mid_plane = matches!(extrude.extent, ExtrudeExtent::MidPlane { .. });
    let mut depth = extrude.extent.depth();
    ui.horizontal(|ui| {
        ui.radio_value(&mut mid_plane, false, "Blind");
        ui.radio_value(&mut mid_plane, true, "Mid-plane");
    });
    ui.horizontal(|ui| {
        ui.label("Depth");
        positive(ui, &mut depth);
    });
    extrude.extent = if mid_plane {
        ExtrudeExtent::MidPlane { depth }
    } else {
        ExtrudeExtent::Blind { depth }
    };
    if !mid_plane {
        ui.checkbox(&mut extrude.reverse, "Reverse direction");
    }
}

fn blend_editor(ui: &mut egui::Ui, blend: &mut EdgeBlend, label: &str, selected: &[EdgeName]) {
    ui.horizontal(|ui| {
        ui.label(label);
        positive(ui, &mut blend.size);
    });
    ui.label(format!("{} edge(s):", blend.edges.len()));
    for edge in &blend.edges {
        ui.label(RichText::new(edge.to_string()).small().color(INACTIVE));
    }
    let button = egui::Button::new(format!("Use the {} selected edge(s)", selected.len()));
    if ui
        .add_enabled(!selected.is_empty(), button)
        .on_hover_text("Edges selected in the viewport replace this feature's edges")
        .clicked()
    {
        blend.edges = selected.to_vec();
    }
}
