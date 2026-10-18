//! The ConfigurationManager: the left panel's second tab, as in SolidWorks, where a part's
//! configurations are listed and edited as a design table. Rows are configurations (the
//! active one marked, click a name to activate it), columns the configured dimensions and
//! features; the active row's cells edit the part itself, the others only the values the
//! configuration will apply when activated. Every change is one undo step.

use eframe::egui::{self, RichText};
use oxyde_model::{DimensionRef, DimensionUnit, FeatureId, ModelError, Part};

use crate::app::OxydeApp;
use crate::icons::{self, Icon};

/// Which of the left panel's tabs is shown under the PropertyManager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftTab {
    /// The FeatureManager: the timeline.
    Features,
    /// The ConfigurationManager.
    Configurations,
}

/// What the table asks for; applied after drawing, as one undo step each.
enum Change {
    Activate(usize),
    Add(String),
    Rename(usize, String),
    Remove(usize),
    ConfigureDimension(DimensionRef),
    UnconfigureDimension(DimensionRef),
    ConfigureFeature(FeatureId),
    UnconfigureFeature(FeatureId),
    /// A cell's value, in internal units.
    SetValue(usize, DimensionRef, f64),
    /// The value's drag or typing ended.
    EndEdit,
    SetSuppressed(usize, FeatureId, bool),
}

#[derive(Default)]
pub struct ConfigurationsPanel {
    /// The name being typed for a new configuration.
    new_name: String,
    /// A row's name being typed, with its index.
    renaming: Option<(usize, String)>,
}

impl OxydeApp {
    /// The tab strip at the top of the left panel: FeatureManager, ConfigurationManager.
    pub(crate) fn left_panel_tabs(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let count = self.doc.part.configurations().list.len();
            for (tab, icon, label, hint) in [
                (
                    LeftTab::Features,
                    Icon::Part,
                    "Features".to_owned(),
                    "The feature tree (FeatureManager)",
                ),
                (
                    LeftTab::Configurations,
                    Icon::Configurations,
                    if count > 0 {
                        format!("Configurations ({count})")
                    } else {
                        "Configurations".to_owned()
                    },
                    "Named variants of the part: dimension values and suppressed features \
                     (ConfigurationManager)",
                ),
            ] {
                let on = self.left_tab == tab;
                let response = ui
                    .scope(|ui| {
                        let fill = if on {
                            crate::theme::tokens().accent_bg
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        egui::Frame::none()
                            .fill(fill)
                            .rounding(4.0)
                            .inner_margin(egui::Margin::symmetric(6.0, 2.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    icons::small(ui, icon, 14.0);
                                    ui.selectable_label(on, RichText::new(label).small())
                                })
                                .inner
                            })
                            .inner
                    })
                    .inner;
                if response.on_hover_text(hint).clicked() {
                    self.left_tab = tab;
                }
            }
        });
        ui.add_space(2.0);
    }

    /// The ConfigurationManager.
    pub(crate) fn configurations_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            icons::small(ui, Icon::Configurations, 22.0);
            ui.heading(self.doc.display_name());
        });
        ui.add_space(2.0);
        self.edit_in_place_banner(ui);
        let sketching = self.sketch.is_some();
        let mut changes = Vec::new();
        ui.add_enabled_ui(!sketching, |ui| {
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    configurations_table(
                        ui,
                        &self.doc.part,
                        &mut self.configurations_panel,
                        &mut changes,
                    );
                });
        });
        if sketching && !changes.is_empty() {
            changes.clear();
        }
        for change in changes {
            self.apply_configuration_change(change);
        }
    }

    fn apply_configuration_change(&mut self, change: Change) {
        let outcome: Result<Option<&'static str>, ModelError> = match change {
            Change::Activate(index) => {
                let name = self
                    .doc
                    .part
                    .configurations()
                    .list
                    .get(index)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                let result = self.edit_part(move |p| p.activate_configuration(index));
                if result.is_ok() {
                    self.rebuild();
                    self.status = format!("Configuration {name} is active");
                    self.viewport.request_fit();
                }
                return self.report(result.map(|()| None));
            }
            Change::Add(name) => {
                let added = self.edit_part(move |p| p.add_configuration(&name).map(|_| ()));
                added.map(|()| Some("Configuration added: click its name to activate it"))
            }
            Change::Rename(index, name) => self
                .edit_part(move |p| p.rename_configuration(index, &name))
                .map(|()| None),
            Change::Remove(index) => self
                .edit_part(move |p| p.remove_configuration(index))
                .map(|()| None),
            Change::ConfigureDimension(r) => self
                .edit_part(move |p| p.configure_dimension(r))
                .map(|()| Some("Dimension configured: each configuration has its own value")),
            Change::UnconfigureDimension(r) => self
                .edit_part(move |p| {
                    p.unconfigure_dimension(r);
                    Ok(())
                })
                .map(|()| None),
            Change::ConfigureFeature(id) => self
                .edit_part(move |p| p.configure_feature(id))
                .map(|()| Some("Feature configured: each configuration suppresses it or not")),
            Change::UnconfigureFeature(id) => self
                .edit_part(move |p| {
                    p.unconfigure_feature(id);
                    Ok(())
                })
                .map(|()| None),
            Change::SetValue(index, r, value) => {
                // One drag or one typed value is one undo step, like the Dimensions tab.
                self.doc.checkpoint_edit(r.feature);
                let active = self.doc.part.configurations().active == index;
                let result = self.doc.part.set_configuration_value(index, r, value);
                if result.is_ok() && active {
                    self.rebuild();
                }
                return self.report(result.map(|()| None));
            }
            Change::EndEdit => {
                self.doc.end_edit();
                return;
            }
            Change::SetSuppressed(index, id, suppressed) => {
                let active = self.doc.part.configurations().active == index;
                let result =
                    self.edit_part(move |p| p.set_configuration_suppressed(index, id, suppressed));
                if result.is_ok() && active {
                    self.rebuild();
                }
                return self.report(result.map(|()| None));
            }
        };
        self.report(outcome);
    }

    fn report(&mut self, outcome: Result<Option<&'static str>, ModelError>) {
        match outcome {
            Ok(Some(message)) => self.status = message.to_owned(),
            Ok(None) => {}
            Err(e) => self.status = e.to_string(),
        }
    }
}

fn configurations_table(
    ui: &mut egui::Ui,
    part: &Part,
    state: &mut ConfigurationsPanel,
    changes: &mut Vec<Change>,
) {
    let configurations = part.configurations();
    if configurations.is_empty() {
        ui.label(
            "The part has no configurations yet: it is one design. Add a configuration to \
             keep variants of it in this file, each with its own values of chosen \
             dimensions and its own suppressed features, as SolidWorks does.",
        );
        ui.add_space(6.0);
    }
    // Adding one.
    ui.horizontal(|ui| {
        let hint = if configurations.is_empty() {
            "Name of the new configuration (the part as it is becomes \"Default\")"
        } else {
            "Name of the new configuration (a copy of the active one)"
        };
        let response = ui.add(
            egui::TextEdit::singleline(&mut state.new_name)
                .hint_text("New configuration")
                .desired_width(150.0),
        );
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let name = state.new_name.trim().to_owned();
        if (ui.button("Add").on_hover_text(hint).clicked() || enter) && !name.is_empty() {
            changes.push(Change::Add(name));
            state.new_name.clear();
        }
    });
    if configurations.is_empty() {
        return;
    }

    let dimensions = configurations.configured_dimensions();
    let features = configurations.configured_features();
    let all_dimensions = part.dimensions();
    // Columns to add: dimensions not driven and not configured, features not configured.
    ui.horizontal(|ui| {
        ui.menu_button("Add dimension…", |ui| {
            let mut any = false;
            for d in all_dimensions.iter().filter(|d| !d.driven) {
                if dimensions.contains(&d.reference) {
                    continue;
                }
                any = true;
                if ui.button(&d.full_name).clicked() {
                    changes.push(Change::ConfigureDimension(d.reference));
                    ui.close_menu();
                }
            }
            if !any {
                ui.label(RichText::new("Every dimension is configured or driven").weak());
            }
        })
        .response
        .on_hover_text("A dimension each configuration gives its own value");
        ui.menu_button("Add feature…", |ui| {
            let mut any = false;
            for f in part.features() {
                if features.contains(&f.id()) {
                    continue;
                }
                any = true;
                if ui.button(f.name()).clicked() {
                    changes.push(Change::ConfigureFeature(f.id()));
                    ui.close_menu();
                }
            }
            if !any {
                ui.label(RichText::new("Every feature is configured").weak());
            }
        })
        .response
        .on_hover_text("A feature each configuration suppresses or not");
    });
    ui.add_space(4.0);

    let active = configurations.active;
    let heading = |ui: &mut egui::Ui, text: String| {
        ui.label(RichText::new(text).small().weak());
    };
    egui::Grid::new("configurations_table")
        .striped(true)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            // Headings: the configuration, then a column per dimension and feature, each
            // with a menu to take it out.
            heading(ui, "Configuration".to_owned());
            for r in &dimensions {
                let name = part
                    .configured_dimension_name(*r)
                    .unwrap_or_else(|| "(gone)".to_owned());
                let response = ui.label(RichText::new(&name).small().weak());
                response.context_menu(|ui| {
                    if ui.button("Stop configuring this dimension").clicked() {
                        changes.push(Change::UnconfigureDimension(*r));
                        ui.close_menu();
                    }
                });
            }
            for id in &features {
                let name = part
                    .feature(*id)
                    .map_or_else(|| "(gone)".to_owned(), |f| f.name().to_owned());
                let response = ui
                    .label(RichText::new(format!("{name} suppressed")).small().weak())
                    .on_hover_text("Ticked: the configuration suppresses the feature");
                response.context_menu(|ui| {
                    if ui.button("Stop configuring this feature").clicked() {
                        changes.push(Change::UnconfigureFeature(*id));
                        ui.close_menu();
                    }
                });
            }
            ui.end_row();

            for (index, c) in configurations.list.iter().enumerate() {
                let is_active = index == active;
                // The name: a radio-like mark for the active one; click to activate,
                // double-click to rename.
                ui.horizontal(|ui| match &mut state.renaming {
                    Some((i, text)) if *i == index => {
                        let response =
                            ui.add(egui::TextEdit::singleline(text).desired_width(110.0));
                        response.request_focus();
                        let done = response.lost_focus();
                        let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if done || cancel {
                            let typed = text.trim().to_owned();
                            if !cancel && !typed.is_empty() && typed != c.name {
                                changes.push(Change::Rename(index, typed));
                            }
                            state.renaming = None;
                        }
                    }
                    _ => {
                        let text = if is_active {
                            RichText::new(format!("● {}", c.name)).strong()
                        } else {
                            RichText::new(format!("○ {}", c.name))
                        };
                        let response =
                            ui.selectable_label(is_active, text)
                                .on_hover_text(if is_active {
                                    "The active configuration: the part as shown"
                                } else {
                                    "Click to activate; double-click to rename"
                                });
                        if response.double_clicked() {
                            state.renaming = Some((index, c.name.clone()));
                        } else if response.clicked() && !is_active {
                            changes.push(Change::Activate(index));
                        }
                        response.context_menu(|ui| {
                            if ui.button("Rename").clicked() {
                                state.renaming = Some((index, c.name.clone()));
                                ui.close_menu();
                            }
                            let removable = !is_active || configurations.list.len() == 1;
                            if ui
                                .add_enabled(removable, egui::Button::new("Remove"))
                                .on_hover_text(if removable {
                                    "Take this configuration out of the part"
                                } else {
                                    "Activate another configuration first"
                                })
                                .clicked()
                            {
                                changes.push(Change::Remove(index));
                                ui.close_menu();
                            }
                        });
                    }
                });
                // The dimension cells: the active row reads the part itself.
                for r in &dimensions {
                    let current = all_dimensions.iter().find(|d| d.reference == *r);
                    let unit = current.map_or(DimensionUnit::Length, |d| d.unit);
                    let stored = c.dimensions.get(r).copied();
                    let value = if is_active {
                        current.map(|d| d.value).or(stored)
                    } else {
                        stored
                    };
                    match value {
                        None => {
                            ui.label(RichText::new("—").weak());
                        }
                        Some(value) => {
                            let angle = unit == DimensionUnit::Angle;
                            let shown = unit.to_display(value);
                            let shown = if angle { shown.abs() } else { shown };
                            let mut edited = shown;
                            let max = crate::dialogs::MAX_LENGTH;
                            let drag = match unit {
                                DimensionUnit::Count => egui::DragValue::new(&mut edited)
                                    .speed(0.1)
                                    .max_decimals(0)
                                    .range(1.0..=1000.0),
                                _ => egui::DragValue::new(&mut edited)
                                    .speed(0.1)
                                    .range(-max..=max),
                            }
                            .suffix(unit.suffix())
                            .custom_parser(move |text| {
                                crate::quantity::parse(text, crate::quantity::Quantity::of(unit))
                            });
                            let response = ui.add(drag);
                            if response.changed()
                                && let Ok(new) = unit.from_display(edited)
                            {
                                let new = if angle && value < 0.0 { -new } else { new };
                                changes.push(Change::SetValue(index, *r, new));
                            }
                            if response.drag_stopped() || response.lost_focus() {
                                changes.push(Change::EndEdit);
                            }
                        }
                    }
                }
                for id in &features {
                    let mut suppressed = if is_active {
                        part.feature(*id).is_some_and(|f| f.is_suppressed())
                    } else {
                        c.suppressed.get(id).copied().unwrap_or(false)
                    };
                    if ui.checkbox(&mut suppressed, "").changed() {
                        changes.push(Change::SetSuppressed(index, *id, suppressed));
                    }
                }
                ui.end_row();
            }
        });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "The active row shows the part as it is; what is changed while a configuration \
             is active stays with it. Dimensions driven by equations cannot be configured.",
        )
        .small()
        .weak(),
    );
}
