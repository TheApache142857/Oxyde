//! The left panel: reference planes, the feature timeline and the rollback bar.
//!
//! Features are edited the SolidWorks way: double-click (or right-click › Edit) opens the
//! feature's own window, or sketch mode for a sketch. Selecting a face or an edge in the
//! viewport highlights the feature that created it. The rollback bar is a line with a handle
//! that can be dragged between features.

use eframe::egui::{self, Color32, CursorIcon, RichText, Sense, Stroke};
use oxyde_model::{
    FeatureId, FeatureKind, FeatureParameter, FeatureStatus, StandardPlane, TimelineEntry,
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
    Edit(FeatureId),
    Rename(FeatureId),
    Suppress(FeatureId, bool),
    /// Move the rollback bar so that this many features are active.
    RollTo(usize),
    Delete(FeatureId),
}

impl OxydeApp {
    pub(crate) fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.heading(self.doc.display_name());
        ui.add_space(2.0);

        let mut action = None;
        let creator = self.selection_creator();
        let enabled = self.sketch.is_none();
        ui.add_enabled_ui(enabled, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
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
                    let mut rows = Vec::with_capacity(entries.len());
                    let mut bar = None;
                    for (index, entry) in entries.iter().enumerate() {
                        if index == active {
                            bar = Some(rollback_bar(ui));
                        }
                        let highlighted =
                            self.selected_feature == Some(entry.id) || creator == Some(entry.id);
                        rows.push(self.timeline_row(ui, entry, highlighted, &mut action));
                    }
                    if active == entries.len() {
                        bar = Some(rollback_bar(ui));
                    }

                    // Dragging the bar: show where it will land, move it on release.
                    if let Some(bar) = bar {
                        let pointer = ui.input(|i| i.pointer.interact_pos());
                        if bar.dragged() {
                            if let Some(pos) = pointer {
                                ui.painter().hline(
                                    bar.rect.x_range(),
                                    pos.y,
                                    Stroke::new(2.0, ROLLBACK.gamma_multiply(0.5)),
                                );
                            }
                        }
                        if bar.drag_stopped() {
                            if let Some(pos) = pointer {
                                let target = rows.iter().filter(|r| r.center().y < pos.y).count();
                                if target != active {
                                    action = Some(Action::RollTo(target));
                                }
                            }
                        }
                    }
                });
        });

        if let Some(action) = action {
            self.apply_action(action);
        }
    }

    /// Draws one feature row and returns its rectangle.
    fn timeline_row(
        &self,
        ui: &mut egui::Ui,
        entry: &TimelineEntry,
        highlighted: bool,
        action: &mut Option<Action>,
    ) -> egui::Rect {
        let suppressed = self
            .doc
            .part
            .feature(entry.id)
            .is_some_and(|f| f.is_suppressed());
        let label = if entry.under_defined {
            format!("(-) {}", entry.name)
        } else {
            entry.name.clone()
        };
        let mut text = match &entry.status {
            FeatureStatus::Error(_) => RichText::new(format!("⚠ {label}")).color(ERROR),
            FeatureStatus::Warning(_) => RichText::new(format!("⚠ {label}")).color(WARNING),
            FeatureStatus::Ok => RichText::new(label),
            FeatureStatus::Suppressed | FeatureStatus::RolledBack | FeatureStatus::Pending => {
                RichText::new(label).color(INACTIVE)
            }
        };
        if suppressed {
            text = text.strikethrough();
        }

        let mut response = ui.selectable_label(highlighted, text);
        response = match &entry.status {
            FeatureStatus::Error(message) | FeatureStatus::Warning(message) => {
                response.on_hover_text(message.as_str())
            }
            _ if entry.under_defined => {
                response.on_hover_text("Under defined: the sketch can still move")
            }
            _ => response.on_hover_text(entry.type_name),
        };
        if response.double_clicked() {
            *action = Some(Action::Edit(entry.id));
        } else if response.clicked() {
            *action = Some(Action::Select(entry.id));
        }
        let id = entry.id;
        let is_sketch = entry.type_name == "Sketch";
        response.context_menu(|ui| {
            let edit = if is_sketch {
                "Edit sketch"
            } else {
                "Edit feature"
            };
            if ui.button(edit).clicked() {
                *action = Some(Action::Edit(id));
                ui.close_menu();
            }
            if ui.button("Rename…").clicked() {
                *action = Some(Action::Rename(id));
                ui.close_menu();
            }
            let label = if suppressed { "Unsuppress" } else { "Suppress" };
            if ui.button(label).clicked() {
                *action = Some(Action::Suppress(id, !suppressed));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Delete").clicked() {
                *action = Some(Action::Delete(id));
                ui.close_menu();
            }
        });
        response.rect
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
                self.selected_plane = None;
                self.doc.end_edit();
            }
            Action::Edit(id) => self.edit_feature(id),
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
            Action::RollTo(active) => {
                let before = self.doc.part.features().get(active).map(|f| f.id());
                self.doc.checkpoint();
                if self.doc.part.roll_back_to(before).is_ok() {
                    self.rebuild();
                }
            }
            Action::Delete(id) => self.delete_feature(id),
        }
    }

    /// The feature that created the most recently selected face or edge. An edge appears
    /// with the later of the faces it separates.
    pub(crate) fn selection_creator(&self) -> Option<FeatureId> {
        use crate::app::Selected;
        let features = self.doc.part.features();
        let position = |id: FeatureId| features.iter().position(|f| f.id() == id);
        match self.selection.last()? {
            Selected::Face(face) => Some(face.key.feature()),
            Selected::Edge(edge) => edge
                .faces
                .iter()
                .map(|f| f.key.feature())
                .max_by_key(|id| position(*id)),
        }
    }

    /// Full name of a feature's own dimension, e.g. "D1@Boss-Extrude1".
    pub(crate) fn parameter_full_name(
        &self,
        id: FeatureId,
        parameter: FeatureParameter,
    ) -> Option<String> {
        self.doc
            .part
            .dimension(oxyde_model::DimensionRef {
                feature: id,
                key: oxyde_model::DimensionKey::Feature(parameter),
            })
            .map(|d| {
                if d.driven {
                    format!("Σ {} (driven by an equation)", d.full_name)
                } else {
                    d.full_name
                }
            })
    }

    /// Opens the right editor for a feature.
    pub(crate) fn edit_feature(&mut self, id: FeatureId) {
        let Some(kind) = self.doc.part.feature(id).map(|f| f.kind().clone()) else {
            return;
        };
        self.selected_feature = Some(id);
        match kind {
            FeatureKind::Sketch(_) => self.edit_sketch(id, false),
            FeatureKind::Extrude(e) => {
                self.dialog = Some(Dialog::Extrude(crate::dialogs::ExtrudeDialog {
                    editing: Some(id),
                    dimension: self.parameter_full_name(id, FeatureParameter::Depth),
                    sketch: e.sketch,
                    sketches: self.sketches(),
                    op: e.op,
                    depth: e.extent.depth(),
                    mid_plane: matches!(e.extent, oxyde_model::ExtrudeExtent::MidPlane { .. }),
                    reverse: e.reverse,
                }));
            }
            FeatureKind::Fillet(b) | FeatureKind::Chamfer(b) => {
                let fillet = matches!(
                    self.doc.part.feature(id).map(|f| f.kind()),
                    Some(FeatureKind::Fillet(_))
                );
                let parameter = if fillet {
                    FeatureParameter::Radius
                } else {
                    FeatureParameter::Distance
                };
                self.dialog = Some(Dialog::Blend(crate::dialogs::BlendDialog {
                    editing: Some(id),
                    dimension: self.parameter_full_name(id, parameter),
                    fillet,
                    size: b.size,
                    edges: b.edges,
                    selected_edges: self.selected_edges(),
                    use_selection: false,
                }));
            }
        }
    }
}

/// The rollback bar: a line with a round handle, dragged vertically between features.
fn rollback_bar(ui: &mut egui::Ui) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 12.0), Sense::drag());
    let y = rect.center().y;
    let painter = ui.painter();
    painter.hline(rect.x_range(), y, Stroke::new(2.0, ROLLBACK));
    painter.circle_filled(egui::pos2(rect.left() + 6.0, y), 4.5, ROLLBACK);
    let cursor = if response.dragged() {
        CursorIcon::Grabbing
    } else {
        CursorIcon::ResizeVertical
    };
    response.on_hover_cursor(cursor)
}
