//! The menus, the tabbed command bar (Features, Sketch, Evaluate — SolidWorks'
//! CommandManager) and the heads-up view toolbar over the viewport.

use eframe::egui;
use oxyde_render::StandardView;

use crate::app::{Command, CommandTab, OxydeApp, PendingAction};
use crate::dialogs::Dialog;
use crate::equations_ui::EquationsTab;
use crate::features_ui::PatternKind;
use crate::icons::{self, Icon, command_button};
use crate::sketch_mode::Tool;

const VIEWS: [(Icon, &str, StandardView); 4] = [
    (Icon::ViewFront, "Front", StandardView::Front),
    (Icon::ViewTop, "Top", StandardView::Top),
    (Icon::ViewRight, "Right", StandardView::Right),
    (Icon::ViewIso, "Isometric", StandardView::Isometric),
];

/// A thin vertical separator between groups of command buttons.
fn group_separator(ui: &mut egui::Ui) {
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(2.0);
}

impl OxydeApp {
    /// What is there to build on, for enabling commands.
    fn has_sketch(&self) -> bool {
        !self.sketches().is_empty()
    }

    fn has_body(&self) -> bool {
        self.doc.part.body().is_some()
    }

    fn has_patternable(&self) -> bool {
        self.doc
            .part
            .features()
            .iter()
            .any(|f| f.kind().is_patternable())
    }

    pub(crate) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::New, &ctx);
                }
                if ui.button("Open…").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::Open, &ctx);
                }
                if ui.button("New part from STEP…").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::OpenStep, &ctx);
                }
                if ui.button("Open demo bracket").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::Demo, &ctx);
                }
                if ui.button("Open demo flange").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::DemoFlange, &ctx);
                }
                ui.separator();
                if ui.button("Save").clicked() {
                    ui.close_menu();
                    self.save();
                }
                if ui.button("Save as…").clicked() {
                    ui.close_menu();
                    self.save_as();
                }
                if ui.button("Import STEP…").clicked() {
                    ui.close_menu();
                    self.import_step();
                }
                if ui.button("Export STEP…").clicked() {
                    ui.close_menu();
                    self.export_step();
                }
                ui.separator();
                if ui.button("Part properties…").clicked() {
                    ui.close_menu();
                    self.dialog = Some(Dialog::PartProperties(self.doc.properties.clone()));
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::Exit, &ctx);
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(self.doc.can_undo(), egui::Button::new("Undo"))
                    .clicked()
                {
                    ui.close_menu();
                    self.undo();
                }
                if ui
                    .add_enabled(self.doc.can_redo(), egui::Button::new("Redo"))
                    .clicked()
                {
                    ui.close_menu();
                    self.redo();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        self.selected_feature.is_some(),
                        egui::Button::new("Delete feature"),
                    )
                    .clicked()
                {
                    ui.close_menu();
                    if let Some(id) = self.selected_feature {
                        self.delete_feature(id);
                    }
                }
                if ui.button("Rebuild all (Ctrl+B)").clicked() {
                    ui.close_menu();
                    self.doc.release(&mut self.kernel);
                    self.rebuild();
                }
            });
            ui.menu_button("Insert", |ui| {
                let (sketch, body, seeds) =
                    (self.has_sketch(), self.has_body(), self.has_patternable());
                let commands: [(Icon, &str, bool, Command); 11] = [
                    (Icon::Sketch, "Sketch", true, Command::Sketch),
                    (Icon::Extrude, "Extrude", sketch, Command::Extrude),
                    (Icon::Revolve, "Revolve", sketch, Command::Revolve),
                    (Icon::Fillet, "Fillet", body, Command::Blend(true)),
                    (Icon::Chamfer, "Chamfer", body, Command::Blend(false)),
                    (Icon::Shell, "Shell", body, Command::Shell),
                    (
                        Icon::LinearPattern,
                        "Linear Pattern",
                        seeds,
                        Command::Pattern(PatternKind::Linear),
                    ),
                    (
                        Icon::CircularPattern,
                        "Circular Pattern",
                        seeds,
                        Command::Pattern(PatternKind::Circular),
                    ),
                    (
                        Icon::Mirror,
                        "Mirror",
                        seeds,
                        Command::Pattern(PatternKind::Mirror),
                    ),
                    (Icon::RefPlane, "Reference Plane", true, Command::RefPlane),
                    (Icon::RefAxis, "Reference Axis", true, Command::RefAxis),
                ];
                for (i, (icon, label, enabled, command)) in commands.into_iter().enumerate() {
                    if i == 1 || i == 3 || i == 6 || i == 9 {
                        ui.separator();
                    }
                    let clicked = ui
                        .add_enabled_ui(enabled, |ui| {
                            ui.horizontal(|ui| {
                                icons::small(ui, icon, 16.0);
                                ui.button(label).clicked()
                            })
                            .inner
                        })
                        .inner;
                    if clicked {
                        ui.close_menu();
                        self.run(command);
                    }
                }
            });
            ui.menu_button("View", |ui| {
                for (icon, label, view) in VIEWS {
                    let clicked = ui
                        .horizontal(|ui| {
                            icons::small(ui, icon, 16.0);
                            ui.button(label).clicked()
                        })
                        .inner;
                    if clicked {
                        ui.close_menu();
                        self.viewport.set_view(view);
                    }
                }
                ui.separator();
                if ui.button("Zoom to fit (F)").clicked() {
                    ui.close_menu();
                    self.viewport.request_fit();
                }
                ui.separator();
                ui.menu_button("Hide/Show", |ui| self.visibility_menu(ui));
            });
            ui.menu_button("Tools", |ui| {
                if ui.button("Equations…").clicked() {
                    ui.close_menu();
                    self.equations.show(EquationsTab::Equations);
                }
                if ui.button("Dimensions…").clicked() {
                    ui.close_menu();
                    self.equations.show(EquationsTab::Dimensions);
                }
                ui.separator();
                if ui.button("Mass properties…").clicked() {
                    ui.close_menu();
                    self.show_mass_properties = true;
                }
                if ui.button("Measure…").clicked() {
                    ui.close_menu();
                    self.show_measure = true;
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("About Oxyde").clicked() {
                    ui.close_menu();
                    self.dialog = Some(Dialog::About);
                }
            });
        });
    }

    pub(crate) fn command_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            for (tab, label) in [
                (CommandTab::Features, "Features"),
                (CommandTab::Sketch, "Sketch"),
                (CommandTab::Evaluate, "Evaluate"),
            ] {
                let text = egui::RichText::new(label).size(13.0);
                let text = if self.tab == tab { text.strong() } else { text };
                if ui.selectable_label(self.tab == tab, text).clicked() {
                    self.tab = tab;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let sketching = self.sketch.is_some();
                let can_redo = !sketching && self.doc.can_redo();
                if icons::icon_button(ui, Icon::Redo, 22.0, can_redo)
                    .on_hover_text("Redo (Ctrl+Y)")
                    .clicked()
                {
                    self.redo();
                }
                let can_undo = match &self.sketch {
                    Some(_) => true,
                    None => self.doc.can_undo(),
                };
                if icons::icon_button(ui, Icon::Undo, 22.0, can_undo)
                    .on_hover_text("Undo (Ctrl+Z)")
                    .clicked()
                {
                    match self.sketch.as_mut() {
                        Some(session) => {
                            session.undo();
                        }
                        None => self.undo(),
                    }
                }
            });
        });
        ui.add_space(2.0);
        egui::ScrollArea::horizontal()
            .id_salt("command_bar")
            .show(ui, |ui| {
                ui.horizontal(|ui| match self.tab {
                    CommandTab::Features => self.features_tab(ui),
                    CommandTab::Sketch => self.sketch_tab(ui),
                    CommandTab::Evaluate => self.evaluate_tab(ui),
                });
            });
    }

    fn command(
        &mut self,
        ui: &mut egui::Ui,
        icon: Icon,
        label: &str,
        enabled: bool,
        hint: &str,
        command: Command,
    ) {
        let active = self.command_is_open(command);
        let response = command_button(ui, icon, label, enabled, active);
        let response = if enabled {
            response.on_hover_text(hint)
        } else {
            response.on_hover_text(format!("{hint}\n(not available yet)"))
        };
        if response.clicked() {
            self.run(command);
        }
    }

    /// Whether `command` is the one open in the PropertyManager.
    fn command_is_open(&self, command: Command) -> bool {
        match (&self.dialog, command) {
            (Some(Dialog::Extrude(d)), Command::Extrude) => d.editing.is_none(),
            (Some(Dialog::Revolve(d)), Command::Revolve) => d.editing.is_none(),
            (Some(Dialog::Blend(d)), Command::Blend(fillet)) => {
                d.editing.is_none() && d.fillet == fillet
            }
            (Some(Dialog::Shell(d)), Command::Shell) => d.editing.is_none(),
            (Some(Dialog::Pattern(d)), Command::Pattern(kind)) => {
                d.editing.is_none() && d.kind == kind
            }
            (Some(Dialog::ChoosePlane), Command::Sketch) => true,
            _ => false,
        }
    }

    fn features_tab(&mut self, ui: &mut egui::Ui) {
        let (sketch, body, seeds) = (self.has_sketch(), self.has_body(), self.has_patternable());
        self.command(
            ui,
            Icon::Extrude,
            "Extrude",
            sketch,
            "Extrude a sketch, adding or removing material.\nSelect the sketch first, or pick it in the tree after.",
            Command::Extrude,
        );
        self.command(
            ui,
            Icon::Revolve,
            "Revolve",
            sketch,
            "Revolve a sketch about an axis, adding or removing material.",
            Command::Revolve,
        );
        group_separator(ui);
        self.command(
            ui,
            Icon::Fillet,
            "Fillet",
            body,
            "Round edges. Select edges (or faces) first, or click them after.",
            Command::Blend(true),
        );
        self.command(
            ui,
            Icon::Chamfer,
            "Chamfer",
            body,
            "Bevel edges. Select edges (or faces) first, or click them after.",
            Command::Blend(false),
        );
        self.command(
            ui,
            Icon::Shell,
            "Shell",
            body,
            "Hollow the part, removing the selected faces.",
            Command::Shell,
        );
        group_separator(ui);
        self.command(
            ui,
            Icon::LinearPattern,
            "Linear Pattern",
            seeds,
            "Copy features along one or two directions.",
            Command::Pattern(PatternKind::Linear),
        );
        self.command(
            ui,
            Icon::CircularPattern,
            "Circular Pattern",
            seeds,
            "Copy features around an axis.",
            Command::Pattern(PatternKind::Circular),
        );
        self.command(
            ui,
            Icon::Mirror,
            "Mirror",
            seeds,
            "Mirror features about a plane or a flat face.",
            Command::Pattern(PatternKind::Mirror),
        );
        group_separator(ui);
        let response = command_button(
            ui,
            Icon::ReferenceGeometry,
            "Reference Geometry",
            true,
            false,
        )
        .on_hover_text("Planes and axes to build on");
        icons::dropdown_marker(ui, &response);
        let popup = ui.make_persistent_id("reference_geometry_menu");
        if response.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup));
        }
        let mut chosen = None;
        egui::popup::popup_below_widget(
            ui,
            popup,
            &response,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(140.0);
                for (icon, label, command) in [
                    (Icon::RefPlane, "Plane", Command::RefPlane),
                    (Icon::RefAxis, "Axis", Command::RefAxis),
                ] {
                    let clicked = ui
                        .horizontal(|ui| {
                            icons::small(ui, icon, 18.0);
                            ui.button(label).clicked()
                        })
                        .inner;
                    if clicked {
                        chosen = Some(command);
                    }
                }
            },
        );
        if let Some(command) = chosen {
            self.run(command);
        }
        group_separator(ui);
        if command_button(ui, Icon::Imported, "Import STEP", true, false)
            .on_hover_text("Add the solids of a STEP file to the part, as an Imported feature")
            .clicked()
        {
            self.import_step();
        }
    }

    fn sketch_tab(&mut self, ui: &mut egui::Ui) {
        let mut finish = None;
        let mut tool_clicked = None;
        let mut construction = false;
        match self.sketch.as_ref() {
            None => {
                let open = self.command_is_open(Command::Sketch);
                if command_button(ui, Icon::Sketch, "Sketch", true, open)
                    .on_hover_text("New sketch on the selected plane or flat face")
                    .clicked()
                {
                    self.run(Command::Sketch);
                }
            }
            Some(_) => {
                if command_button(ui, Icon::ExitSketch, "Exit Sketch", true, false)
                    .on_hover_text("Keep the changes (Esc)")
                    .clicked()
                {
                    finish = Some(true);
                }
                if command_button(ui, Icon::Discard, "Discard", true, false)
                    .on_hover_text("Leave the sketch without keeping the changes")
                    .clicked()
                {
                    finish = Some(false);
                }
            }
        }
        group_separator(ui);
        let sketching = self.sketch.is_some();
        let current = self.sketch.as_ref().map(|s| s.tool);
        for (tool, icon, label) in Tool::ALL {
            // Outside a sketch, drawing tools start one; the others need geometry.
            let enabled = sketching || tool.draws();
            if !sketching && tool == Tool::Select {
                continue;
            }
            if tool == Tool::Trim {
                group_separator(ui);
            }
            let response = command_button(ui, icon, label, enabled, current == Some(tool));
            let response = if sketching || tool.draws() {
                response
            } else {
                response.on_disabled_hover_text("Open a sketch first")
            };
            if response.clicked() {
                tool_clicked = Some(tool);
            }
        }
        if sketching {
            let has_curves = self.sketch.as_ref().is_some_and(|s| s.has_curve_selected());
            if command_button(ui, Icon::Construction, "Construction", has_curves, false)
                .on_hover_text("Turn the selected curves into construction geometry, or back")
                .on_disabled_hover_text("Select lines, circles or arcs first")
                .clicked()
            {
                construction = true;
            }
        }
        group_separator(ui);
        let can_normal = self.normal_to_target().is_some();
        if command_button(ui, Icon::NormalTo, "Normal To", can_normal, false)
            .on_hover_text("Look straight at the sketch plane")
            .clicked()
        {
            if let Some(plane) = self.normal_to_target() {
                self.viewport.look_at_plane(&plane);
            }
        }

        if let Some(keep) = finish {
            self.finish_sketch(keep);
        }
        if construction {
            if let Some(session) = self.sketch.as_mut() {
                session.toggle_construction();
            }
        }
        if let Some(tool) = tool_clicked {
            match self.sketch.as_mut() {
                Some(session) => session.set_tool(tool),
                None => self.begin_sketch(Some(tool)),
            }
        }
    }

    fn evaluate_tab(&mut self, ui: &mut egui::Ui) {
        let body = self.has_body();
        if command_button(
            ui,
            Icon::MassProperties,
            "Mass Properties",
            body,
            self.show_mass_properties,
        )
        .on_hover_text("Volume, surface area and size of the part")
        .clicked()
        {
            self.show_mass_properties = !self.show_mass_properties;
        }
        if command_button(ui, Icon::Measure, "Measure", body, self.show_measure)
            .on_hover_text("Lengths, areas, distances and angles of the selected faces and edges")
            .clicked()
        {
            self.show_measure = !self.show_measure;
        }
        group_separator(ui);
        if command_button(ui, Icon::Equations, "Equations", true, self.equations.open)
            .on_hover_text("Equations, global variables and dimensions")
            .clicked()
        {
            self.equations.show(EquationsTab::Equations);
        }
        if command_button(ui, Icon::Rebuild, "Rebuild", true, false)
            .on_hover_text("Rebuild every feature (Ctrl+B)")
            .clicked()
        {
            self.doc.release(&mut self.kernel);
            self.rebuild();
            self.status = "Rebuilt".to_owned();
        }
    }

    /// View › Hide/Show: whole kinds of items.
    fn visibility_menu(&mut self, ui: &mut egui::Ui) {
        let v = &mut self.visibility;
        ui.checkbox(&mut v.planes, "Planes");
        ui.checkbox(&mut v.axes, "Axes");
        ui.checkbox(&mut v.sketches, "Sketches");
        ui.checkbox(&mut v.origins, "Origins");
        ui.label(
            egui::RichText::new("Single items: the eye in the tree")
                .small()
                .weak(),
        );
    }

    /// Standard views, zoom to fit and Normal To, floating at the top of the viewport.
    pub(crate) fn heads_up_toolbar(&mut self, ctx: &egui::Context, viewport: egui::Rect) {
        egui::Area::new(egui::Id::new("heads_up_toolbar"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_TOP)
            .fixed_pos(viewport.center_top() + egui::vec2(0.0, 6.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::symmetric(4.0, 2.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            for (icon, label, view) in VIEWS {
                                if icons::icon_button(ui, icon, 26.0, true)
                                    .on_hover_text(label)
                                    .clicked()
                                {
                                    self.viewport.set_view(view);
                                }
                            }
                            ui.separator();
                            if icons::icon_button(ui, Icon::Fit, 26.0, true)
                                .on_hover_text("Zoom to fit (F)")
                                .clicked()
                            {
                                self.viewport.request_fit();
                            }
                            ui.separator();
                            let eye = icons::icon_button(ui, Icon::Eye, 26.0, true)
                                .on_hover_text("Hide/Show planes, axes, sketches, origins");
                            let popup = ui.make_persistent_id("hide_show_menu");
                            if eye.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(popup));
                            }
                            egui::popup::popup_below_widget(
                                ui,
                                popup,
                                &eye,
                                egui::PopupCloseBehavior::CloseOnClickOutside,
                                |ui| {
                                    ui.set_min_width(150.0);
                                    self.visibility_menu(ui);
                                },
                            );
                            let target = self.normal_to_target();
                            if icons::icon_button(ui, Icon::NormalTo, 26.0, target.is_some())
                                .on_hover_text(
                                    "Normal To: look straight at the sketch, plane or flat face",
                                )
                                .clicked()
                            {
                                if let Some(plane) = target {
                                    self.viewport.look_at_plane(&plane);
                                }
                            }
                        });
                    });
            });
    }
}
