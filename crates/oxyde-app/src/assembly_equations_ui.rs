//! Tools › Equations in an assembly, as in SolidWorks: the assembly's global variables, and
//! equations driving its mate values (`"D1@Distance1" = "Gap" * 2`) and the dimensions and
//! variables of its parts (`"D1@Sketch1@Top panel<1>.Part" = "Width" - 6`).
//!
//! - **Equations** tab: global variables and equations, each with what it evaluates to or
//!   its error. The list of what an equation can drive narrows as a name is typed.
//! - **Dimensions** tab: the value of every Distance and Angle mate, editable unless an
//!   equation drives it (Σ).
//!
//! Every change is one undo step. A part an equation drives changes for every instance of
//! it, and is saved with the assembly. Linked to an equations file, the assembly shares its
//! variables with the parts linked to the same file: changing one here writes the file and
//! updates those parts.

use eframe::egui::{self, RichText};
use oxyde_assembly::{AsmDimension, Assembly, ComponentTree, MateId, MateKind};
use oxyde_model::{DimensionUnit, GlobalId, LinkedValue, ModelError};

use crate::app::OxydeApp;
use crate::assembly::AssemblyDoc;
use crate::equations_ui::{
    EquationsTab, EquationsWindow, Field, error, format_value, result_label,
};

/// Entries of the list of what a new equation drives, at most, before the filter narrows it.
const MAX_TARGETS: usize = 300;

/// A change requested in the window, applied once it is drawn.
enum Change {
    AddGlobal(String, String),
    RenameGlobal(GlobalId, String),
    SetGlobal(GlobalId, String),
    RemoveGlobal(GlobalId),
    AddEquation(AsmDimension, String),
    SetEquation(usize, String),
    RemoveEquation(usize),
    /// A mate's value, in mm or degrees.
    SetMate(MateId, f64),
    EndMateEdit,
    /// Link the assembly to the equations file the user picks.
    LinkFile,
    Unlink,
}

fn invalid(message: &str) -> ModelError {
    ModelError::Expression(message.to_owned())
}

impl OxydeApp {
    pub(crate) fn assembly_equations_window(&mut self, ctx: &egui::Context) {
        self.equations.begin_frame();
        if !self.equations.open {
            self.equations.end_frame();
            return;
        }
        let busy = self.busy();
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let mut open = true;
        let mut changes = Vec::new();
        let state = &mut self.equations;
        egui::Window::new("Equations, Global Variables and Dimensions")
            .open(&mut open)
            .resizable(true)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut state.tab, EquationsTab::Equations, "Equations");
                    ui.selectable_value(&mut state.tab, EquationsTab::Dimensions, "Dimensions");
                });
                ui.separator();
                if busy {
                    ui.label("Wait until the parts are read.");
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| match state.tab {
                    EquationsTab::Equations => equations_tab(ui, state, doc, &mut changes),
                    EquationsTab::Dimensions => dimensions_tab(ui, doc, &mut changes),
                });
            });
        if !open {
            self.equations.open = false;
            self.equations.editing.clear();
        }
        self.equations.end_frame();
        for change in changes {
            self.apply_assembly_equation_change(change);
        }
    }

    fn apply_assembly_equation_change(&mut self, change: Change) {
        let outcome: Result<(), ModelError> = match change {
            Change::SetMate(id, value) => {
                self.set_mate_value(id, value);
                return;
            }
            Change::EndMateEdit => {
                self.equations.editing_mate = None;
                return;
            }
            Change::AddGlobal(name, text) => self.edit_assembly(|a, tree| {
                let expr = a.parse_expression(&text, tree)?;
                a.equations.add_global(&name, expr).map(|_| ())
            }),
            Change::RenameGlobal(id, name) => self.edit_assembly(|a, tree| {
                a.equations.rename_global(id, &name)?;
                a.resolve_equation_names(tree);
                Ok(())
            }),
            Change::SetGlobal(id, text) if self.is_assembly_linked(id) => {
                self.set_assembly_linked_global(id, &text)
            }
            Change::SetGlobal(id, text) => self.edit_assembly(|a, tree| {
                let expr = a.parse_expression(&text, tree)?;
                a.equations.set_global_expression(id, expr)
            }),
            Change::RemoveGlobal(id) => self.edit_assembly(|a, _| a.equations.remove_global(id)),
            Change::AddEquation(target, text) => self.edit_assembly(|a, tree| {
                let name = a
                    .dimension_name(&target, tree)
                    .ok_or_else(|| invalid("that dimension no longer exists"))?;
                let expr = a.parse_expression(&text, tree)?;
                a.equations.add_equation(target, name, expr).map(|_| ())
            }),
            Change::SetEquation(index, text) => self.edit_assembly(|a, tree| {
                let expr = a.parse_expression(&text, tree)?;
                a.equations.set_equation_expression(index, expr)
            }),
            Change::RemoveEquation(index) => {
                self.edit_assembly(|a, _| a.equations.remove_equation(index))
            }
            Change::LinkFile => match self.pick_assembly_equations_file() {
                Some(result) => result,
                None => return,
            },
            Change::Unlink => self.edit_assembly(|a, tree| a.link_equations(None, &[], tree)),
        };
        match outcome {
            Ok(()) => {
                if let Some(doc) = self.asm.as_mut() {
                    doc.apply_equations(&mut self.kernel);
                }
                self.status = "Equations updated".to_owned();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    /// Applies a change to the assembly as one undo step. The change is made on a copy,
    /// so a refused change leaves no trace.
    fn edit_assembly(
        &mut self,
        change: impl FnOnce(&mut Assembly, &dyn ComponentTree) -> Result<(), ModelError>,
    ) -> Result<(), ModelError> {
        let doc = self
            .asm
            .as_mut()
            .ok_or_else(|| invalid("no assembly is open"))?;
        let mut trial = doc.assembly.clone();
        change(&mut trial, &doc.tree())?;
        doc.checkpoint();
        doc.assembly = trial;
        Ok(())
    }

    /// Sets a mate's value from the Dimensions tab; a drag is one undo step.
    fn set_mate_value(&mut self, id: MateId, value: f64) {
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        if self.equations.editing_mate != Some(id) {
            doc.checkpoint();
            self.equations.editing_mate = Some(id);
        }
        if let Some(mate) = doc.assembly.mate_mut(id) {
            match &mut mate.kind {
                MateKind::Distance(d) => *d = value.max(0.0),
                MateKind::Angle(a) => *a = value.clamp(0.0, 180.0).to_radians(),
                _ => {}
            }
        }
        doc.modified = true;
        doc.apply_equations(&mut self.kernel);
    }

    fn is_assembly_linked(&self, id: GlobalId) -> bool {
        self.asm.as_ref().is_some_and(|d| {
            let equations = &d.assembly.equations;
            equations.linked_file.is_some()
                && equations.globals.iter().any(|g| g.id == id && g.linked)
        })
    }

    /// Writes a linked variable into the equations file and reads all of it back, into the
    /// assembly and into every part linked to the same file.
    fn set_assembly_linked_global(&mut self, id: GlobalId, text: &str) -> Result<(), ModelError> {
        let doc = self
            .asm
            .as_ref()
            .ok_or_else(|| invalid("no assembly is open"))?;
        let name = doc
            .assembly
            .equations
            .globals
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.name.clone())
            .ok_or_else(|| invalid("no such variable"))?;
        doc.assembly.parse_expression(text, &doc.tree())?;
        let path = doc.linked_file_path()?;
        let io = |e: oxyde_format::FormatError| invalid(&format!("{}: {e}", path.display()));
        let value = LinkedValue {
            name,
            expression: text.to_owned(),
        };
        oxyde_format::equations_file::update_equations_file(&path, &[value]).map_err(io)?;
        let values = oxyde_format::equations_file::read_equations_file(&path).map_err(io)?;
        self.edit_assembly(|a, tree| a.apply_linked_values(&values, tree))?;
        if let Some(doc) = self.asm.as_mut() {
            doc.refresh_linked_parts(&path, &values);
        }
        Ok(())
    }

    /// Asks for an equations file (a new one is created with the assembly's variables) and
    /// links the assembly to it. `None` when the user cancels.
    fn pick_assembly_equations_file(&mut self) -> Option<Result<(), ModelError>> {
        let doc = self.asm.as_ref()?;
        let Some(assembly_path) = doc.path.clone() else {
            return Some(Err(invalid(
                "Save the assembly first: its equations file is found next to it",
            )));
        };
        // A new file starts with the assembly's own variables.
        let own: Vec<LinkedValue> = doc
            .assembly
            .equations_text(&doc.tree())
            .globals
            .into_iter()
            .map(|g| LinkedValue {
                name: g.name,
                expression: g.expression,
            })
            .collect();
        let folder = assembly_path.parent().map(|p| p.to_path_buf());
        let mut dialog = rfd::FileDialog::new()
            .set_title("Link to an equations file")
            .add_filter("Equations file", &["txt"])
            .set_file_name("Equations.txt");
        if let Some(folder) = &folder {
            dialog = dialog.set_directory(folder);
        }
        let path = dialog.save_file()?;
        let result = (|| {
            let io = |e: oxyde_format::FormatError| invalid(&format!("{}: {e}", path.display()));
            if !path.exists() {
                oxyde_format::equations_file::update_equations_file(&path, &own).map_err(io)?;
            }
            let values = oxyde_format::equations_file::read_equations_file(&path).map_err(io)?;
            // Stored relative to the assembly's folder when it is in it or below, so the
            // assembly and its file can move together.
            let stored = folder
                .as_deref()
                .and_then(|f| path.strip_prefix(f).ok())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            self.edit_assembly(|a, tree| a.link_equations(Some(stored), &values, tree))
        })();
        Some(result)
    }
}

fn equations_tab(
    ui: &mut egui::Ui,
    state: &mut EquationsWindow,
    doc: &AssemblyDoc,
    changes: &mut Vec<Change>,
) {
    let assembly = &doc.assembly;
    let equations = &assembly.equations;
    let results = &doc.equation_results;
    let tree = doc.tree();

    ui.horizontal(|ui| match &equations.linked_file {
        Some(file) => {
            ui.label(format!("🔗 Linked to {file}")).on_hover_text(
                "Variables marked 🔗 come from this file and are shared with every part and \
                 assembly linked to it. Changing one changes the file, and the parts here \
                 that are linked to it.",
            );
            if ui.small_button("Unlink").clicked() {
                changes.push(Change::Unlink);
            }
        }
        None => {
            if ui
                .small_button("🔗 Link to file…")
                .on_hover_text("Share global variables with parts through a text file")
                .clicked()
            {
                changes.push(Change::LinkFile);
            }
        }
    });
    ui.add_space(4.0);
    ui.strong("Global variables");
    egui::Grid::new("assembly_globals")
        .num_columns(4)
        .striped(true)
        .show(ui, |ui| {
            ui.label(RichText::new("Name").weak());
            ui.label(RichText::new("= Value / Equation").weak());
            ui.label(RichText::new("Evaluates to").weak());
            ui.label("");
            ui.end_row();
            for (i, global) in equations.globals.iter().enumerate() {
                let linked = global.linked && equations.linked_file.is_some();
                if linked {
                    // The name is the file's: renaming it here would break the link.
                    ui.label(format!("🔗 {}", global.name))
                        .on_hover_text("From the linked equations file");
                } else if let Some(name) =
                    state.field(ui, Field::GlobalName(global.id), &global.name, 110.0)
                {
                    changes.push(Change::RenameGlobal(global.id, name));
                }
                let text = assembly.expression_text(&global.expr, &tree);
                if let Some(text) =
                    state.field(ui, Field::GlobalExpression(global.id), &text, 240.0)
                {
                    changes.push(Change::SetGlobal(global.id, text));
                }
                result_label(ui, results.globals.get(i), None);
                if linked {
                    ui.label("");
                } else if ui.small_button("🗑").on_hover_text("Delete").clicked() {
                    changes.push(Change::RemoveGlobal(global.id));
                }
                ui.end_row();
            }
            ui.add(
                egui::TextEdit::singleline(&mut state.new_global_name)
                    .hint_text("New name")
                    .desired_width(110.0),
            );
            let expression = ui.add(
                egui::TextEdit::singleline(&mut state.new_global_expression)
                    .hint_text("Value or expression")
                    .desired_width(240.0),
            );
            let entered = expression.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let ready = !state.new_global_name.trim().is_empty()
                && !state.new_global_expression.trim().is_empty();
            if ui.add_enabled(ready, egui::Button::new("Add")).clicked() || (entered && ready) {
                changes.push(Change::AddGlobal(
                    state.new_global_name.trim().to_owned(),
                    state.new_global_expression.trim().to_owned(),
                ));
                state.new_global_name.clear();
                state.new_global_expression.clear();
            }
            ui.end_row();
        });

    ui.add_space(8.0);
    ui.strong("Equations");
    egui::Grid::new("assembly_equations")
        .num_columns(4)
        .striped(true)
        .show(ui, |ui| {
            ui.label(RichText::new("Dimension").weak());
            ui.label(RichText::new("= Equation").weak());
            ui.label(RichText::new("Evaluates to").weak());
            ui.label("");
            ui.end_row();
            for (i, equation) in equations.equations.iter().enumerate() {
                let current = equation
                    .target
                    .as_ref()
                    .and_then(|t| assembly.dimension_name(t, &tree));
                let target = current
                    .clone()
                    .unwrap_or_else(|| equation.target_name.clone());
                let label = RichText::new(format!("\"{target}\""));
                ui.label(if current.is_some() {
                    label
                } else {
                    label.color(error())
                });
                let text = assembly.expression_text(&equation.expr, &tree);
                if let Some(text) = state.field(ui, Field::EquationExpression(i), &text, 240.0) {
                    changes.push(Change::SetEquation(i, text));
                }
                let unit = equation.target.as_ref().and_then(|t| doc.target_unit(t));
                result_label(ui, results.equations.get(i), unit);
                if ui.small_button("🗑").on_hover_text("Delete").clicked() {
                    changes.push(Change::RemoveEquation(i));
                }
                ui.end_row();
            }

            let targets = doc.equation_targets();
            if state
                .new_assembly_target
                .as_ref()
                .is_some_and(|t| !targets.iter().any(|(d, _)| d == t))
            {
                state.new_assembly_target = None;
            }
            let selected = state
                .new_assembly_target
                .as_ref()
                .and_then(|t| targets.iter().find(|(d, _)| d == t))
                .map_or_else(|| "Dimension…".to_owned(), |(_, name)| name.clone());
            // A list with a filter at its top, which a combo box would close when clicked.
            let button =
                ui.add(egui::Button::new(format!("{selected} ⏷")).min_size(egui::vec2(220.0, 0.0)));
            let popup = button.id.with("targets");
            if button.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup));
            }
            egui::popup::popup_below_widget(
                ui,
                popup,
                &button,
                egui::PopupCloseBehavior::CloseOnClickOutside,
                |ui| {
                    ui.set_min_width(280.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut state.target_filter)
                            .hint_text("Type to narrow: Distance, Top panel…")
                            .desired_width(270.0),
                    );
                    let filter = state.target_filter.to_lowercase();
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            let mut shown = 0;
                            for (target, name) in &targets {
                                if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                                    continue;
                                }
                                if shown == MAX_TARGETS {
                                    ui.label(RichText::new("… type to narrow the list").weak());
                                    break;
                                }
                                shown += 1;
                                let chosen = state.new_assembly_target.as_ref() == Some(target);
                                if ui.selectable_label(chosen, name).clicked() {
                                    state.new_assembly_target = Some(target.clone());
                                    ui.memory_mut(|m| m.close_popup());
                                }
                            }
                            if shown == 0 {
                                ui.label(RichText::new("Nothing matches").weak());
                            }
                        });
                },
            );
            let expression = ui.add(
                egui::TextEdit::singleline(&mut state.new_equation_expression)
                    .hint_text("e.g. \"Width\" - 6")
                    .desired_width(240.0),
            );
            let entered = expression.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let ready = state.new_assembly_target.is_some()
                && !state.new_equation_expression.trim().is_empty();
            let add =
                ui.add_enabled(ready, egui::Button::new("Add")).clicked() || (entered && ready);
            if let Some(target) = state.new_assembly_target.take_if(|_| add) {
                changes.push(Change::AddEquation(
                    target,
                    state.new_equation_expression.trim().to_owned(),
                ));
                state.new_equation_expression.clear();
            }
            ui.end_row();
        });

    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Names go between double quotes: \"Width\", a mate's \"D1@Distance1\", a part's \
             \"D1@Sketch1@Top panel<1>.Part\" or \"Thickness@Top panel<1>.Part\". A part \
             driven here changes for every instance of it, and is saved with the assembly. \
             Operators + - * / ^ and functions sin cos tan asin acos atan (degrees), sqrt abs \
             exp log int round sgn min max, and pi. Lengths in mm, angles in degrees.",
        )
        .small()
        .weak(),
    );
}

fn dimensions_tab(ui: &mut egui::Ui, doc: &AssemblyDoc, changes: &mut Vec<Change>) {
    let dimensions = doc.assembly.mate_dimensions();
    if dimensions.is_empty() {
        ui.label("No dimensions yet: Distance and Angle mates have one each.");
        return;
    }
    ui.label(
        RichText::new(
            "Names are used in equations, e.g. D1@Distance1. Σ: driven by an equation. The \
             dimensions of the parts are in each part's own Equations window.",
        )
        .small()
        .weak(),
    );
    egui::Grid::new("assembly_dimensions")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.label(RichText::new("Name").weak());
            ui.label(RichText::new("Value").weak());
            ui.end_row();
            for d in &dimensions {
                ui.label(&d.full_name);
                let unit = if d.angle {
                    DimensionUnit::Angle
                } else {
                    DimensionUnit::Length
                };
                if d.driven {
                    ui.label(format!(
                        "Σ {}{}",
                        format_value(d.value, unit),
                        unit.suffix()
                    ))
                    .on_hover_text("Driven by an equation (Equations tab)");
                } else {
                    let mut edited = d.value;
                    let drag = if d.angle {
                        egui::DragValue::new(&mut edited)
                            .speed(0.5)
                            .range(0.0..=180.0)
                            .suffix("°")
                            .custom_parser(crate::dialogs::angle_parser)
                    } else {
                        egui::DragValue::new(&mut edited)
                            .speed(0.1)
                            .range(0.0..=crate::dialogs::MAX_LENGTH)
                            .suffix(" mm")
                            .custom_parser(crate::dialogs::length_parser)
                    };
                    let response = ui.add(drag);
                    if response.changed() {
                        changes.push(Change::SetMate(d.mate, edited));
                    }
                    // One drag or one typed value is one undo step.
                    if response.drag_stopped() || response.lost_focus() {
                        changes.push(Change::EndMateEdit);
                    }
                }
                ui.end_row();
            }
        });
}
