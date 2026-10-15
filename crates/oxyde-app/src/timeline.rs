//! The left panel: reference planes, the feature timeline and the rollback bar.
//!
//! Features are edited the SolidWorks way: double-click (or right-click › Edit) opens the
//! feature in the PropertyManager, or sketch mode for a sketch. Clicking a feature selects
//! it for the next command; while a command is open, clicking hands the feature (a sketch
//! to extrude, a feature to pattern, a plane…) to that command. Selecting a face or an edge
//! in the viewport highlights the feature that created it. The rollback bar is a line with
//! a handle that can be dragged between features.

use eframe::egui::{self, Color32, CursorIcon, RichText, Sense, Stroke};
use oxyde_model::{
    ExtrudeOp, FeatureId, FeatureKind, FeatureParameter, FeatureStatus, StandardPlane,
    TimelineEntry,
};

use crate::app::OxydeApp;
use crate::dialogs::{Dialog, plane_name};
use crate::display::Item;
use crate::features_ui::PatternKind;
use crate::icons::{self, Icon};
use crate::property_manager::PanelPick;

fn error() -> Color32 {
    crate::theme::tokens().err
}
fn warning() -> Color32 {
    crate::theme::tokens().warn
}
fn inactive() -> Color32 {
    crate::theme::tokens().dim
}
fn rollback() -> Color32 {
    crate::theme::tokens().accent
}

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
    /// Show or hide the sketch a feature absorbs.
    ToggleExpand(FeatureId),
    /// Show or hide an item in the viewport.
    ToggleItem(Item),
    ToggleBodies,
    SelectBody(oxyde_model::BodyId),
}

pub fn feature_icon(kind: &FeatureKind) -> Icon {
    match kind {
        FeatureKind::Sketch(_) => Icon::Sketch,
        FeatureKind::Extrude(_) => Icon::Extrude,
        FeatureKind::Revolve(_) => Icon::Revolve,
        FeatureKind::Fillet(_) => Icon::Fillet,
        FeatureKind::Chamfer(_) => Icon::Chamfer,
        FeatureKind::Shell(_) => Icon::Shell,
        FeatureKind::LinearPattern(_) => Icon::LinearPattern,
        FeatureKind::CircularPattern(_) => Icon::CircularPattern,
        FeatureKind::Mirror(_) => Icon::Mirror,
        FeatureKind::Plane(_) => Icon::RefPlane,
        FeatureKind::Axis(_) => Icon::RefAxis,
        FeatureKind::Import(_) => Icon::Imported,
        FeatureKind::Combine(_) => Icon::Combine,
    }
}

/// One row of the tree.
pub(crate) struct Row {
    pub(crate) icon: Icon,
    /// A cut extrusion or revolve: red badge on the icon.
    pub(crate) cut: bool,
    pub(crate) text: RichText,
    pub(crate) highlighted: bool,
    /// Error or warning, on the right.
    pub(crate) badge: Option<Icon>,
    /// Children are indented.
    pub(crate) indent: f32,
    /// `Some(expanded)` for a row that absorbs a sketch.
    pub(crate) expander: Option<bool>,
    /// `Some(shown)` for an item that can be hidden.
    pub(crate) eye: Option<bool>,
}

pub(crate) struct RowResponse {
    pub(crate) response: egui::Response,
    pub(crate) expand_clicked: bool,
    pub(crate) eye_clicked: bool,
}

pub(crate) fn tree_row(ui: &mut egui::Ui, row: Row) -> RowResponse {
    let height = 22.0;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::click());
    let left = rect.left() + row.indent;
    let expander_rect =
        egui::Rect::from_min_size(egui::pos2(left, rect.top()), egui::vec2(12.0, height));
    let badge_width = if row.badge.is_some() { 18.0 } else { 0.0 };
    let eye_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 11.0 - badge_width, rect.center().y),
        egui::Vec2::splat(16.0),
    );
    let click = response
        .clicked()
        .then(|| response.interact_pointer_pos())
        .flatten();
    let expand_clicked = row.expander.is_some() && click.is_some_and(|p| expander_rect.contains(p));
    let eye_clicked = row.eye.is_some() && click.is_some_and(|p| eye_rect.contains(p));

    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let painter = ui.painter();
        if row.highlighted {
            painter.rect_filled(rect, 3.0, visuals.selection.bg_fill.gamma_multiply(0.45));
        } else if response.hovered() {
            painter.rect_filled(rect, 3.0, visuals.widgets.hovered.weak_bg_fill);
        }
        if let Some(expanded) = row.expander {
            let c = expander_rect.center();
            let color = visuals.text_color();
            let points = if expanded {
                vec![
                    c + egui::vec2(-4.0, -2.0),
                    c + egui::vec2(4.0, -2.0),
                    c + egui::vec2(0.0, 3.0),
                ]
            } else {
                vec![
                    c + egui::vec2(-2.0, -4.0),
                    c + egui::vec2(3.0, 0.0),
                    c + egui::vec2(-2.0, 4.0),
                ]
            };
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(left + 13.0, rect.top() + 2.0),
            egui::Vec2::splat(18.0),
        );
        icons::paint(painter, icon_rect, row.icon, true);
        if row.cut {
            icons::cut_badge(painter, icon_rect);
        }
        let galley = egui::WidgetText::from(row.text).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            (eye_rect.left() - icon_rect.right() - 10.0).max(20.0),
            egui::TextStyle::Body,
        );
        let text_pos = egui::pos2(
            icon_rect.right() + 6.0,
            rect.center().y - galley.size().y / 2.0,
        );
        painter.galley(text_pos, galley, visuals.text_color());
        if let Some(shown) = row.eye {
            // A closed eye stays visible; the open one appears on hover.
            if !shown {
                icons::paint(painter, eye_rect, Icon::EyeClosed, true);
            } else if response.hovered() {
                icons::paint(painter, eye_rect, Icon::Eye, true);
            }
        }
        if let Some(badge) = row.badge {
            let badge_rect = egui::Rect::from_center_size(
                egui::pos2(rect.right() - 11.0, rect.center().y),
                egui::Vec2::splat(15.0),
            );
            icons::paint(painter, badge_rect, badge, true);
        }
    }
    RowResponse {
        response,
        expand_clicked,
        eye_clicked,
    }
}

impl OxydeApp {
    pub(crate) fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            icons::small(ui, Icon::Part, 22.0);
            ui.heading(self.doc.display_name());
        });
        ui.add_space(2.0);

        let mut action = None;
        let mut hover_feature = None;
        let mut hover_body = None;
        let creator = self.selection_creator();
        let referenced = self.panel_features();
        let enabled = self.sketch.is_none();
        ui.add_enabled_ui(enabled, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for plane in [
                        StandardPlane::Front,
                        StandardPlane::Top,
                        StandardPlane::Right,
                    ] {
                        let item = Item::Plane(plane);
                        let row = tree_row(
                            ui,
                            Row {
                                icon: Icon::Plane,
                                cut: false,
                                text: RichText::new(format!("{} Plane", plane_name(plane))),
                                highlighted: self.selected_plane == Some(plane),
                                badge: None,
                                indent: 0.0,
                                expander: None,
                                eye: Some(self.item_shown(item)),
                            },
                        );
                        if row.eye_clicked {
                            action = Some(Action::ToggleItem(item));
                        } else if row.response.clicked() {
                            action = Some(Action::SelectPlane(plane));
                        }
                    }
                    let origin = tree_row(
                        ui,
                        Row {
                            icon: Icon::Origin,
                            cut: false,
                            text: RichText::new("Origin"),
                            highlighted: false,
                            badge: None,
                            indent: 0.0,
                            expander: None,
                            eye: Some(self.item_shown(Item::Origin)),
                        },
                    );
                    if origin.eye_clicked {
                        action = Some(Action::ToggleItem(Item::Origin));
                    }
                    // Solid Bodies, when there is more than the one.
                    if let Some(body) = self.doc.part.body().filter(|b| b.solids.len() > 1) {
                        let folder = tree_row(
                            ui,
                            Row {
                                icon: Icon::Bodies,
                                cut: false,
                                text: RichText::new(format!(
                                    "Solid Bodies ({})",
                                    body.solids.len()
                                )),
                                highlighted: false,
                                badge: None,
                                indent: 0.0,
                                expander: Some(self.bodies_expanded),
                                eye: None,
                            },
                        );
                        if folder.response.clicked() {
                            action = Some(Action::ToggleBodies);
                        }
                        if self.bodies_expanded {
                            let mut ids: Vec<oxyde_model::BodyId> =
                                body.solids.iter().map(|s| s.id).collect();
                            ids.sort_by_key(|b| (self.doc.part.index_of(b.feature), b.index));
                            for id in ids {
                                let maker = self
                                    .doc
                                    .part
                                    .feature(id.feature)
                                    .map_or_else(String::new, |f| f.name().to_owned());
                                let row = tree_row(
                                    ui,
                                    Row {
                                        icon: Icon::Part,
                                        cut: false,
                                        text: RichText::new(self.body_label(id)),
                                        highlighted: self.selected_body == Some(id),
                                        badge: None,
                                        indent: 16.0,
                                        expander: None,
                                        eye: None,
                                    },
                                );
                                if row.response.hovered() {
                                    hover_body = Some(id);
                                }
                                if row
                                    .response
                                    .on_hover_text(format!("Made by {maker}"))
                                    .clicked()
                                {
                                    action = Some(Action::SelectBody(id));
                                }
                            }
                        }
                    }
                    ui.add_space(3.0);
                    ui.separator();

                    let entries = self.doc.part.timeline();
                    let active = self
                        .doc
                        .part
                        .rollback_position()
                        .unwrap_or(entries.len())
                        .min(entries.len());
                    let absorbed = self.absorbed_sketches();
                    let index_of = |id: FeatureId| entries.iter().position(|e| e.id == id);
                    let highlighted = |id: FeatureId| {
                        self.selected_feature == Some(id)
                            || creator == Some(id)
                            || referenced.contains(&id)
                    };
                    // Rows in display order, with the timeline index of their feature.
                    let mut rows: Vec<(egui::Rect, usize)> = Vec::with_capacity(entries.len());
                    let mut bar = None;
                    for (index, entry) in entries.iter().enumerate() {
                        if absorbed.contains_key(&entry.id) {
                            continue; // shown under the feature that uses it
                        }
                        if bar.is_none() && index >= active {
                            bar = Some(rollback_bar(ui));
                        }
                        let child = absorbed
                            .iter()
                            .find(|(_, parent)| **parent == entry.id)
                            .map(|(sketch, _)| *sketch);
                        let expanded = self.expanded.contains(&entry.id);
                        let rect = self.timeline_row(
                            ui,
                            entry,
                            highlighted(entry.id),
                            0.0,
                            child.map(|_| expanded),
                            &mut action,
                        );
                        rows.push((rect, index));
                        if let (Some(sketch), true) = (child, expanded) {
                            if let Some(child_index) = index_of(sketch) {
                                let rect = self.timeline_row(
                                    ui,
                                    &entries[child_index],
                                    highlighted(sketch),
                                    16.0,
                                    None,
                                    &mut action,
                                );
                                rows.push((rect, child_index));
                            }
                        }
                    }
                    // The feature under the pointer lights up what it made in the viewport.
                    hover_feature = ui
                        .ctx()
                        .pointer_hover_pos()
                        .and_then(|p| rows.iter().find(|(r, _)| r.contains(p)))
                        .map(|(_, i)| entries[*i].id);
                    if bar.is_none() {
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
                                    Stroke::new(2.0, rollback().gamma_multiply(0.5)),
                                );
                            }
                        }
                        if bar.drag_stopped() {
                            if let Some(pos) = pointer {
                                let target = rows
                                    .iter()
                                    .find(|(r, _)| r.center().y > pos.y)
                                    .map_or(entries.len(), |(_, index)| *index);
                                if target != active {
                                    action = Some(Action::RollTo(target));
                                }
                            }
                        }
                    }
                });
        });

        if let Some(id) = hover_feature {
            self.viewport.preselect = self.feature_picks(id);
        }
        if let Some(id) = hover_body {
            self.viewport.preselect = self.body_picks(id);
        }
        if let Some(action) = action {
            self.apply_action(action);
        }
    }

    /// The faces of one body of the part, for highlighting.
    pub(crate) fn body_picks(&self, id: oxyde_model::BodyId) -> Vec<oxyde_render::Pick> {
        self.doc
            .part
            .body()
            .and_then(|b| b.solid(id))
            .map(|s| s.faces.clone().map(oxyde_render::Pick::Face).collect())
            .unwrap_or_default()
    }

    /// The faces of the body a feature made or changed, for highlighting.
    fn feature_picks(&self, id: FeatureId) -> Vec<oxyde_render::Pick> {
        let Some(body) = self.doc.part.body() else {
            return Vec::new();
        };
        (0..body.topology.faces.len() as u32)
            .filter(|&i| body.naming.face(i).is_some_and(|n| n.key.feature() == id))
            .map(oxyde_render::Pick::Face)
            .collect()
    }

    /// Draws one feature row and returns its rectangle.
    fn timeline_row(
        &self,
        ui: &mut egui::Ui,
        entry: &TimelineEntry,
        highlighted: bool,
        indent: f32,
        expander: Option<bool>,
        action: &mut Option<Action>,
    ) -> egui::Rect {
        let feature = self.doc.part.feature(entry.id);
        let suppressed = feature.is_some_and(|f| f.is_suppressed());
        let icon = feature.map_or(Icon::Part, |f| feature_icon(f.kind()));
        let cut = feature.is_some_and(|f| match f.kind() {
            FeatureKind::Extrude(e) => e.op == ExtrudeOp::Cut,
            FeatureKind::Revolve(r) => r.op == ExtrudeOp::Cut,
            FeatureKind::Combine(c) => c.op == oxyde_model::CombineOp::Cut,
            _ => false,
        });
        let label = if entry.under_defined {
            format!("(-) {}", entry.name)
        } else {
            entry.name.clone()
        };
        let (mut text, badge) = match &entry.status {
            FeatureStatus::Error(_) => (RichText::new(label).color(error()), Some(Icon::Error)),
            FeatureStatus::Warning(_) => {
                (RichText::new(label).color(warning()), Some(Icon::Warning))
            }
            FeatureStatus::Ok => (RichText::new(label), None),
            FeatureStatus::Suppressed | FeatureStatus::RolledBack | FeatureStatus::Pending => {
                (RichText::new(label).color(inactive()), None)
            }
        };
        if suppressed {
            text = text.strikethrough();
        }
        let item = Item::Feature(entry.id);
        let eye = self.has_visibility(entry.id).then(|| self.item_shown(item));

        let row = tree_row(
            ui,
            Row {
                icon,
                cut,
                text,
                highlighted,
                badge,
                indent,
                expander,
                eye,
            },
        );
        let mut response = row.response;
        response = match &entry.status {
            FeatureStatus::Error(message) | FeatureStatus::Warning(message) => {
                response.on_hover_text(message.as_str())
            }
            _ if entry.under_defined => {
                response.on_hover_text("Under defined: the sketch can still move")
            }
            _ => match feature.map(|f| f.kind()) {
                Some(FeatureKind::Import(import)) => {
                    response.on_hover_text(format!("Imported from {}", import.file_name))
                }
                _ => response.on_hover_text(entry.type_name),
            },
        };
        if row.expand_clicked {
            *action = Some(Action::ToggleExpand(entry.id));
        } else if row.eye_clicked {
            *action = Some(Action::ToggleItem(item));
        } else if response.double_clicked() {
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
            if let Some(shown) = eye {
                if ui.button(if shown { "Hide" } else { "Show" }).clicked() {
                    *action = Some(Action::ToggleItem(item));
                    ui.close_menu();
                }
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
                if self.offer_to_panel(PanelPick::Plane(plane)) {
                    return;
                }
                self.selected_plane = Some(plane);
                self.selected_feature = None;
                self.selection.clear();
                self.doc.end_edit();
            }
            Action::Select(id) => {
                if self.offer_to_panel(PanelPick::Feature(id)) {
                    return;
                }
                self.selected_feature = Some(id);
                self.selected_plane = None;
                self.doc.end_edit();
            }
            Action::ToggleBodies => self.bodies_expanded = !self.bodies_expanded,
            Action::SelectBody(id) => {
                if self.offer_to_panel(PanelPick::Body(id)) {
                    return;
                }
                self.selected_body = (self.selected_body != Some(id)).then_some(id);
                self.selected_feature = None;
                self.selected_plane = None;
                self.selection.clear();
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
            Action::ToggleExpand(id) => {
                if !self.expanded.remove(&id) {
                    self.expanded.insert(id);
                }
            }
            Action::ToggleItem(item) => self.toggle_item(item),
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
        if self.sketch.is_some() {
            self.finish_sketch(true);
        }
        self.selected_feature = Some(id);
        match kind.clone() {
            FeatureKind::Sketch(_) => self.edit_sketch(id, false),
            FeatureKind::Extrude(e) => {
                self.dialog = Some(Dialog::Extrude(crate::dialogs::ExtrudeDialog {
                    editing: Some(id),
                    dimension: self.parameter_full_name(id, FeatureParameter::Depth),
                    sketch: Some(e.sketch),
                    op: e.op,
                    depth: e.extent.depth(),
                    mid_plane: matches!(e.extent, oxyde_model::ExtrudeExtent::MidPlane { .. }),
                    reverse: e.reverse,
                    bodies: e.bodies.clone(),
                }));
            }
            FeatureKind::Fillet(b) | FeatureKind::Chamfer(b) => {
                let fillet = matches!(kind, FeatureKind::Fillet(_));
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
                }));
            }
            FeatureKind::Revolve(r) => self.open_revolve(Some(id), Some(r.sketch), r.op, Some(&r)),
            FeatureKind::Shell(shell) => self.open_shell(Some(id), Some(&shell)),
            FeatureKind::Combine(c) => self.open_combine(Some(id), Some(&c)),
            FeatureKind::Plane(plane) => self.open_ref_plane(Some(id), Some(&plane)),
            FeatureKind::Axis(axis) => self.open_ref_axis(Some(id), Some(&axis)),
            FeatureKind::LinearPattern(_) => {
                self.open_pattern(Some(id), PatternKind::Linear, Some(&kind))
            }
            FeatureKind::CircularPattern(_) => {
                self.open_pattern(Some(id), PatternKind::Circular, Some(&kind))
            }
            FeatureKind::Mirror(_) => self.open_pattern(Some(id), PatternKind::Mirror, Some(&kind)),
            FeatureKind::Import(import) => {
                self.status = format!(
                    "{} holds the geometry of {}; it has nothing to edit",
                    self.doc
                        .part
                        .feature(id)
                        .map(|f| f.name().to_owned())
                        .unwrap_or_default(),
                    import.file_name
                );
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
    painter.hline(rect.x_range(), y, Stroke::new(2.0, rollback()));
    painter.circle_filled(egui::pos2(rect.left() + 6.0, y), 4.5, rollback());
    let cursor = if response.dragged() {
        CursorIcon::Grabbing
    } else {
        CursorIcon::ResizeVertical
    };
    response.on_hover_cursor(cursor)
}
