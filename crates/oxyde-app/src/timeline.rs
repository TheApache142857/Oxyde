//! The left panel: reference planes, the feature timeline and the rollback bar.
//!
//! Features are edited the SolidWorks way: double-click (or right-click › Edit) opens the
//! feature in the PropertyManager, or sketch mode for a sketch. Clicking a feature selects
//! it for the next command; while a command is open, clicking hands the feature (a sketch
//! to extrude, a feature to pattern, a plane…) to that command. Selecting a face or an edge
//! in the viewport highlights the feature that created it. The rollback bar is a line with
//! a handle that can be dragged between features.
//!
//! Above the features, the Material row says what the part is made of; a click on it
//! lists the library's materials (SolidWorks' `Material <not specified>` row).

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

/// How a click changes the feature selection: plain, Ctrl (one more or one less) or
/// Shift (everything between the last selected feature and this one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectMode {
    Replace,
    Toggle,
    Range,
}

/// Timeline interactions, applied after drawing.
enum Action {
    SelectPlane(StandardPlane),
    Select(FeatureId, SelectMode),
    Edit(FeatureId),
    Rename(FeatureId),
    Suppress(Vec<FeatureId>, bool),
    /// Show or hide several features at once.
    Show(Vec<FeatureId>, bool),
    /// Move the rollback bar so that this many features are active.
    RollTo(usize),
    Delete(Vec<FeatureId>),
    /// Show or hide the sketch a feature absorbs.
    ToggleExpand(FeatureId),
    /// Show or hide an item in the viewport.
    ToggleItem(Item),
    ToggleBodies,
    SelectBody(oxyde_model::BodyId),
    BodyAppearance(oxyde_model::BodyId),
    /// Give the part this library material, or none.
    Material(Option<String>),
    /// The Material page of the Appearance panel.
    MaterialPage,
    /// The part's material in the Materials window.
    EditMaterial,
}

pub fn feature_icon(kind: &FeatureKind) -> Icon {
    match kind {
        FeatureKind::Sketch(_) => Icon::Sketch,
        FeatureKind::Extrude(_) => Icon::Extrude,
        FeatureKind::Revolve(_) => Icon::Revolve,
        FeatureKind::Sweep(_) => Icon::Sweep,
        FeatureKind::Hole(_) => Icon::Hole,
        FeatureKind::Loft(_) => Icon::Loft,
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
    /// A cut extrusion, revolve or sweep: red badge on the icon.
    pub(crate) cut: bool,
    pub(crate) text: RichText,
    pub(crate) highlighted: bool,
    /// Drawn as if the pointer were over it: what is under the pointer in the viewport.
    pub(crate) hovered: bool,
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
        } else if response.hovered() || row.hovered {
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
        self.edit_in_place_banner(ui);

        // The multi-selection lives as long as its anchor stays selected: commands and the
        // viewport set `selected_feature` directly, which ends it.
        if self
            .selected_feature
            .is_none_or(|id| !self.selected_features.contains(&id))
        {
            self.selected_features.clear();
        }
        let mut action = None;
        let mut hover_feature = None;
        let mut hover_body = None;
        // Features in display order, for Shift+click ranges.
        let mut order: Vec<FeatureId> = Vec::new();
        let creator = self.selection_creator();
        // The face or edge under the pointer in the viewport lights up its feature here.
        let under_pointer = self.viewport.hovered().and_then(|p| self.pick_creator(p));
        let referenced = self.panel_features();
        let material_names: Vec<String> = self
            .materials
            .materials()
            .iter()
            .map(|m| m.name.clone())
            .collect();
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
                                hovered: false,
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
                            hovered: false,
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
                                hovered: false,
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
                                        hovered: false,
                                        badge: None,
                                        indent: 16.0,
                                        expander: None,
                                        eye: None,
                                    },
                                );
                                if row.response.hovered() {
                                    hover_body = Some(id);
                                }
                                let response =
                                    row.response.on_hover_text(format!("Made by {maker}"));
                                if response.clicked() {
                                    action = Some(Action::SelectBody(id));
                                }
                                response.context_menu(|ui| {
                                    if ui.button("Appearance…").clicked() {
                                        action = Some(Action::BodyAppearance(id));
                                        ui.close_menu();
                                    }
                                });
                            }
                        }
                    }
                    // What the part is made of, a click away from changing.
                    let material = self.doc.part.material();
                    let row = crate::assembly_ui::material_row(ui, material, 0.0);
                    if let Some(choice) =
                        crate::assembly_ui::material_popup(ui, &row.response, &material_names)
                    {
                        action = Some(Action::Material(choice));
                    }
                    if row.response.double_clicked() {
                        action = Some(Action::MaterialPage);
                    }
                    row.response.context_menu(|ui| {
                        ui.menu_button("Change material", |ui| {
                            if let Some(choice) =
                                crate::assembly_ui::material_menu(ui, &material_names)
                            {
                                action = Some(Action::Material(choice));
                            }
                        });
                        if ui
                            .button("Material…")
                            .on_hover_text("The Material page: the part's look and its density")
                            .clicked()
                        {
                            action = Some(Action::MaterialPage);
                            ui.close_menu();
                        }
                        if material.is_some()
                            && ui.button("Edit in the Materials window…").clicked()
                        {
                            action = Some(Action::EditMaterial);
                            ui.close_menu();
                        }
                    });
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
                            || self.selected_features.contains(&id)
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
                        // The sketches the feature absorbs (a sweep has two: its
                        // profile and its path), in timeline order.
                        let mut children: Vec<usize> = absorbed
                            .iter()
                            .filter(|(_, parent)| **parent == entry.id)
                            .filter_map(|(sketch, _)| index_of(*sketch))
                            .collect();
                        children.sort_unstable();
                        let expanded = self.expanded.contains(&entry.id);
                        let rect = self.timeline_row(
                            ui,
                            entry,
                            highlighted(entry.id),
                            under_pointer == Some(entry.id),
                            0.0,
                            (!children.is_empty()).then_some(expanded),
                            &mut action,
                        );
                        rows.push((rect, index));
                        if expanded {
                            for child_index in children {
                                let Some(child) = entries.get(child_index) else {
                                    continue;
                                };
                                let rect = self.timeline_row(
                                    ui,
                                    child,
                                    highlighted(child.id),
                                    under_pointer == Some(child.id),
                                    16.0,
                                    None,
                                    &mut action,
                                );
                                rows.push((rect, child_index));
                            }
                        }
                    }
                    order = rows.iter().map(|(_, i)| entries[*i].id).collect();
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
                                    Stroke::new(2.0_f32, rollback().gamma_multiply(0.5)),
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
        // The row was scrolled into view while drawing; once is enough.
        self.scroll_to_feature = None;
        if let Some(action) = action {
            self.apply_action(action, &order);
        }
    }

    /// The features a command from the timeline applies to: the multi-selection when `id`
    /// is part of it, else `id` alone.
    fn targets_of(&self, id: FeatureId) -> Vec<FeatureId> {
        if self.selected_features.len() > 1 && self.selected_features.contains(&id) {
            self.selected_features.clone()
        } else {
            vec![id]
        }
    }

    /// Every selected feature: the multi-selection, or the one selected feature.
    pub(crate) fn selected_feature_set(&self) -> Vec<FeatureId> {
        if self.selected_features.len() > 1 {
            self.selected_features.clone()
        } else {
            self.selected_feature.into_iter().collect()
        }
    }

    /// Changes the feature selection by a click in the timeline.
    fn select_feature(&mut self, id: FeatureId, mode: SelectMode, order: &[FeatureId]) {
        match mode {
            SelectMode::Replace => {
                self.selected_feature = Some(id);
                self.selected_features = vec![id];
            }
            SelectMode::Toggle => {
                if self.selected_features.is_empty() {
                    self.selected_features.extend(self.selected_feature);
                }
                match self.selected_features.iter().position(|f| *f == id) {
                    Some(i) => {
                        self.selected_features.remove(i);
                        self.selected_feature = self.selected_features.last().copied();
                    }
                    None => {
                        self.selected_features.push(id);
                        self.selected_feature = Some(id);
                    }
                }
            }
            SelectMode::Range => {
                let anchor = self
                    .selected_feature
                    .and_then(|a| order.iter().position(|f| *f == a));
                let end = order.iter().position(|f| *f == id);
                match (anchor, end) {
                    (Some(a), Some(b)) => {
                        let range = &order[a.min(b)..=a.max(b)];
                        for f in range {
                            if !self.selected_features.contains(f) {
                                self.selected_features.push(*f);
                            }
                        }
                        // Clicked above the anchor: the range reads down to it.
                        if !self.selected_features.contains(&order[a]) {
                            self.selected_features.push(order[a]);
                        }
                    }
                    _ => {
                        self.selected_feature = Some(id);
                        self.selected_features = vec![id];
                    }
                }
            }
        }
        if self.selected_features.len() > 1 {
            self.status = format!("{} features selected", self.selected_features.len());
        }
    }

    /// The feature that created a face or edge of the viewport (for an edge, the later of
    /// the two features that made its faces).
    pub(crate) fn pick_creator(&self, pick: oxyde_render::Pick) -> Option<FeatureId> {
        let body = self.shown_body()?;
        let features = self.doc.part.features();
        let position = |id: FeatureId| features.iter().position(|f| f.id() == id);
        match pick {
            oxyde_render::Pick::Face(i) => body.naming.face(i).map(|n| n.key.feature()),
            oxyde_render::Pick::Edge(i) => body
                .naming
                .edge(i)?
                .faces
                .iter()
                .map(|f| f.key.feature())
                .max_by_key(|id| position(*id)),
        }
    }

    /// The faces of one body of the part, for highlighting.
    pub(crate) fn body_picks(&self, id: oxyde_model::BodyId) -> Vec<oxyde_render::Pick> {
        self.shown_body()
            .and_then(|b| b.solid(id))
            .map(|s| s.faces.clone().map(oxyde_render::Pick::Face).collect())
            .unwrap_or_default()
    }

    /// The faces of the body a feature made or changed, for highlighting.
    fn feature_picks(&self, id: FeatureId) -> Vec<oxyde_render::Pick> {
        let Some(body) = self.shown_body() else {
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
        hovered: bool,
        indent: f32,
        expander: Option<bool>,
        action: &mut Option<Action>,
    ) -> egui::Rect {
        let feature = self.doc.part.feature(entry.id);
        let suppressed = feature.is_some_and(|f| f.is_suppressed());
        let icon = feature.map_or(Icon::Part, |f| feature_icon(f.kind()));
        let cut = feature.is_some_and(|f| match f.kind() {
            FeatureKind::Combine(c) => c.op == oxyde_model::CombineOp::Cut,
            kind => kind.sweep_op() == Some(ExtrudeOp::Cut),
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
                hovered,
                badge,
                indent,
                expander,
                eye,
            },
        );
        let mut response = row.response;
        if self.scroll_to_feature == Some(entry.id) {
            response.scroll_to_me(Some(egui::Align::Center));
        }
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
            let (ctrl, shift) =
                ui.input(|i| (i.modifiers.ctrl || i.modifiers.command, i.modifiers.shift));
            let mode = if shift {
                SelectMode::Range
            } else if ctrl {
                SelectMode::Toggle
            } else {
                SelectMode::Replace
            };
            *action = Some(Action::Select(entry.id, mode));
        }
        let id = entry.id;
        let is_sketch = entry.type_name == "Sketch";
        // The sketches it sweeps, to edit them without unfolding it.
        let sketches: Vec<(FeatureId, String)> = feature
            .map(|f| f.kind().swept_sketches())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| Some((s, self.doc.part.feature(s)?.name().to_owned())))
            .collect();
        let index = self.doc.part.index_of(id);
        let rolled_back = self.doc.part.rollback_position().is_some();
        // Suppress, Hide and Delete act on the whole multi-selection when the row is in it.
        let targets = self.targets_of(id);
        let many = targets.len() > 1;
        let count = |label: &str| {
            if many {
                format!("{label} ({} features)", targets.len())
            } else {
                label.to_owned()
            }
        };
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
            for (sketch, name) in &sketches {
                if ui.button(format!("Edit {name}")).clicked() {
                    *action = Some(Action::Edit(*sketch));
                    ui.close_menu();
                }
            }
            if ui.button("Rename…").clicked() {
                *action = Some(Action::Rename(id));
                ui.close_menu();
            }
            if let Some(shown) = eye {
                if many {
                    let visible: Vec<FeatureId> = targets
                        .iter()
                        .copied()
                        .filter(|f| self.has_visibility(*f))
                        .collect();
                    let label = if shown { "Hide" } else { "Show" };
                    if ui
                        .button(format!("{label} ({} features)", visible.len()))
                        .clicked()
                    {
                        *action = Some(Action::Show(visible, !shown));
                        ui.close_menu();
                    }
                } else if ui.button(if shown { "Hide" } else { "Show" }).clicked() {
                    *action = Some(Action::ToggleItem(item));
                    ui.close_menu();
                }
            }
            let label = if suppressed { "Unsuppress" } else { "Suppress" };
            if ui.button(count(label)).clicked() {
                *action = Some(Action::Suppress(targets.clone(), !suppressed));
                ui.close_menu();
            }
            // SolidWorks' Rollback: the part as it was before this feature.
            if let Some(index) = index
                && ui
                    .button("Roll back")
                    .on_hover_text("Move the rollback bar above this feature")
                    .clicked()
            {
                *action = Some(Action::RollTo(index));
                ui.close_menu();
            }
            if rolled_back && ui.button("Roll to end").clicked() {
                *action = Some(Action::RollTo(usize::MAX));
                ui.close_menu();
            }
            ui.separator();
            if ui.button(count("Delete")).clicked() {
                *action = Some(Action::Delete(targets.clone()));
                ui.close_menu();
            }
        });
        response.rect
    }

    fn apply_action(&mut self, action: Action, order: &[FeatureId]) {
        match action {
            Action::SelectPlane(plane) => {
                if self.offer_to_panel(PanelPick::Plane(plane)) {
                    return;
                }
                self.selected_plane = Some(plane);
                self.selected_feature = None;
                self.selected_features.clear();
                self.selection.clear();
                self.doc.end_edit();
            }
            Action::Select(id, mode) => {
                if mode != SelectMode::Range && self.offer_to_panel(PanelPick::Feature(id)) {
                    return;
                }
                self.select_feature(id, mode, order);
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
            Action::BodyAppearance(id) => self.begin_body_appearance(id),
            Action::Material(Some(name)) => self.apply_material_to_part(&name),
            Action::Material(None) => {
                if self.doc.part.material().is_some() {
                    self.set_part_material(None, false);
                }
            }
            Action::MaterialPage => self.begin_material(),
            Action::EditMaterial => match self.doc.part.material().cloned() {
                Some(m) => self.edit_material(m),
                None => self.open_materials_window(),
            },
            Action::Edit(id) => self.edit_feature(id),
            Action::Rename(feature) => {
                let name = self
                    .doc
                    .part
                    .feature(feature)
                    .map(|f| f.name().to_owned())
                    .unwrap_or_default();
                self.open_dialog(Dialog::Rename { feature, name });
            }
            Action::Suppress(ids, suppressed) => {
                self.doc.checkpoint();
                let changed = ids
                    .iter()
                    .filter(|id| self.doc.part.set_suppressed(**id, suppressed).is_ok())
                    .count();
                if changed > 0 {
                    self.rebuild();
                }
                if ids.len() > 1 {
                    self.status = format!(
                        "{changed} features {}",
                        if suppressed {
                            "suppressed"
                        } else {
                            "unsuppressed"
                        }
                    );
                }
            }
            Action::Show(ids, shown) => {
                for id in ids {
                    self.visibility.set_item_shown(Item::Feature(id), shown);
                }
            }
            Action::RollTo(active) => {
                // Past the last feature (`usize::MAX`): to the end.
                let count = self.doc.part.features().len();
                let current = self.doc.part.rollback_position().unwrap_or(count);
                if active.min(count) == current.min(count) {
                    return;
                }
                let before = self.doc.part.features().get(active).map(|f| f.id());
                self.doc.checkpoint();
                if self.doc.part.roll_back_to(before).is_ok() {
                    self.rebuild();
                }
            }
            Action::Delete(ids) => self.delete_features(&ids),
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
            FeatureKind::Extrude(e) => self.open_extrude(Some(id), Some(e.sketch), Some(&e)),
            FeatureKind::Fillet(b) | FeatureKind::Chamfer(b) => {
                let fillet = matches!(kind, FeatureKind::Fillet(_));
                let parameter = if fillet {
                    FeatureParameter::Radius
                } else {
                    FeatureParameter::Distance
                };
                let mut dialog = crate::dialogs::BlendDialog::new(fillet, b.edges);
                dialog.editing = Some(id);
                dialog.dimension = self.parameter_full_name(id, parameter);
                dialog.size = b.size;
                match b.chamfer {
                    oxyde_model::ChamferShape::Equal => {}
                    oxyde_model::ChamferShape::TwoDistances { distance, flip } => {
                        dialog.chamfer_type = crate::dialogs::ChamferType::TwoDistances;
                        dialog.second = distance;
                        dialog.flip = flip;
                    }
                    oxyde_model::ChamferShape::AngleDistance { angle, flip } => {
                        dialog.chamfer_type = crate::dialogs::ChamferType::AngleDistance;
                        dialog.angle = angle.to_degrees();
                        dialog.flip = flip;
                    }
                }
                self.open_dialog(Dialog::Blend(dialog));
            }
            FeatureKind::Revolve(r) => self.open_revolve(Some(id), Some(r.sketch), r.op, Some(&r)),
            FeatureKind::Sweep(s) => self.open_sweep(Some(id), Some(&s)),
            FeatureKind::Shell(shell) => self.open_shell(Some(id), Some(&shell)),
            FeatureKind::Hole(hole) => self.open_hole(id, &hole),
            FeatureKind::Loft(l) => self.open_loft(Some(id), Some(&l)),
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
    painter.hline(rect.x_range(), y, Stroke::new(2.0_f32, rollback()));
    painter.circle_filled(egui::pos2(rect.left() + 6.0, y), 4.5, rollback());
    let cursor = if response.dragged() {
        CursorIcon::Grabbing
    } else {
        CursorIcon::ResizeVertical
    };
    response.on_hover_cursor(cursor)
}
