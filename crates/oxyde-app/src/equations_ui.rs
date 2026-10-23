//! Tools › Equations: global variables, equations and every dimension of the part, as in
//! the SolidWorks "Equations, Global Variables, and Dimensions" window.
//!
//! - **Equations** tab: global variables (`"Width" = 50`) and equations driving dimensions
//!   (`"D1@Sketch1" = "Width" * 2`), with the value each evaluates to, or its error.
//! - **Dimensions** tab: every dimension under its full name, with its value. Names and
//!   values can be edited; dimensions driven by an equation are shown with Σ.
//!
//! Every change is one undo step and triggers a rebuild.
//!
//! A part can be linked to an equations file shared with other parts (SolidWorks' "Link to
//! external file"): its variables show with 🔗, and changing one writes it into the file,
//! which every part linked to it reads when it is opened.
//!
//! In an assembly the same window shows the assembly's own equations
//! (`assembly_equations_ui.rs`). A part whose values an assembly drives lists them under
//! *Driven by assemblies*, read-only until released, as SolidWorks' external references.

use std::collections::HashMap;

use eframe::egui::{self, Color32, RichText};
use oxyde_model::{Dimension, DimensionRef, DimensionUnit, GlobalId, ModelError, Part};

use crate::app::OxydeApp;

pub(crate) fn error() -> Color32 {
    crate::theme::tokens().err
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquationsTab {
    Equations,
    Dimensions,
}

/// A text field of the window, while it is being typed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Field {
    DimensionName(DimensionRef),
    GlobalName(GlobalId),
    GlobalExpression(GlobalId),
    EquationExpression(usize),
}

pub struct EquationsWindow {
    pub open: bool,
    pub tab: EquationsTab,
    /// Text of the field being typed in; every other field shows the model.
    pub(crate) editing: HashMap<Field, String>,
    pub(crate) new_global_name: String,
    pub(crate) new_global_expression: String,
    new_equation_target: Option<DimensionRef>,
    pub(crate) new_equation_expression: String,
    /// In an assembly: what the new equation drives, and the text narrowing the list.
    pub(crate) new_assembly_target: Option<oxyde_assembly::AsmDimension>,
    pub(crate) target_filter: String,
    /// In an assembly: the mate whose value is being dragged, one undo step for the drag.
    pub(crate) editing_mate: Option<oxyde_assembly::MateId>,
    /// The field typed in during the last frame, and the one this frame: a click in the
    /// tree takes the focus away, and its variable goes into that field.
    pub(crate) focused: Option<Field>,
    focused_now: Option<Field>,
    /// A variable's quoted name clicked in the tree, for the field that had the focus.
    pub(crate) pending_insert: Option<String>,
}

impl EquationsWindow {
    pub fn new() -> Self {
        Self {
            open: false,
            tab: EquationsTab::Equations,
            editing: HashMap::new(),
            new_global_name: String::new(),
            new_global_expression: String::new(),
            new_equation_target: None,
            new_equation_expression: String::new(),
            new_assembly_target: None,
            target_filter: String::new(),
            editing_mate: None,
            focused: None,
            focused_now: None,
            pending_insert: None,
        }
    }

    /// Before the window is drawn: no field has the focus yet this frame.
    pub(crate) fn begin_frame(&mut self) {
        self.focused_now = None;
    }

    /// After it is drawn: the field with the focus, for the tree's clicks in the next frame.
    pub(crate) fn end_frame(&mut self) {
        self.focused = if self.open { self.focused_now } else { None };
        self.pending_insert = None;
    }

    pub fn show(&mut self, tab: EquationsTab) {
        self.open = true;
        self.tab = tab;
    }

    /// A text field that shows `current` unless it is being typed in. Returns the new text
    /// when the user is done (focus lost or Enter) and it differs.
    pub(crate) fn field(
        &mut self,
        ui: &mut egui::Ui,
        field: Field,
        current: &str,
        width: f32,
    ) -> Option<String> {
        let mut text = self
            .editing
            .get(&field)
            .cloned()
            .unwrap_or_else(|| current.to_owned());
        let response = ui.add(egui::TextEdit::singleline(&mut text).desired_width(width));
        if response.lost_focus() {
            // A variable clicked in the tree took the focus: it goes into the field, which
            // stays open, focused again with the cursor at the end.
            if self.focused == Some(field)
                && let Some(name) = self.pending_insert.take()
            {
                crate::dimension_equations::insert_name(&mut text, &name);
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), response.id) {
                    let end = egui::text::CCursor::new(text.chars().count());
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::one(end)));
                    state.store(ui.ctx(), response.id);
                }
                response.request_focus();
                self.editing.insert(field, text);
                self.focused_now = Some(field);
                return None;
            }
            self.editing.remove(&field);
            (text.trim() != current).then(|| text.trim().to_owned())
        } else {
            if response.has_focus() {
                self.editing.insert(field, text);
                self.focused_now = Some(field);
            }
            None
        }
    }
}

/// A change requested in the window, applied once it is drawn.
enum Change {
    RenameDimension(DimensionRef, String),
    SetDimension(DimensionRef, f64),
    EndEdit,
    AddGlobal(String, String),
    RenameGlobal(GlobalId, String),
    SetGlobal(GlobalId, String),
    RemoveGlobal(GlobalId),
    AddEquation(DimensionRef, String),
    SetEquation(usize, String),
    RemoveEquation(usize),
    /// Link the part to the equations file the user picks.
    LinkFile,
    Unlink,
    /// Make a value an assembly drives the part's own again.
    Release(String),
}

/// The value an expression evaluates to (with the unit of its dimension, if any), or its
/// error.
pub(crate) fn result_label(
    ui: &mut egui::Ui,
    result: Option<&Result<f64, String>>,
    unit: Option<DimensionUnit>,
) {
    match result {
        Some(Ok(value)) => {
            let unit_ = unit.unwrap_or(DimensionUnit::Length);
            let suffix = unit.map_or("", DimensionUnit::suffix);
            ui.label(format!("{}{suffix}", format_value(*value, unit_)));
        }
        Some(Err(message)) => {
            ui.label(RichText::new("⚠ error").color(error()))
                .on_hover_text(message.as_str());
        }
        None => {
            ui.label(RichText::new("…").weak());
        }
    }
}

pub(crate) fn format_value(value: f64, unit: DimensionUnit) -> String {
    match unit {
        DimensionUnit::Count => format!("{value:.0}"),
        _ => {
            let text = format!("{value:.4}");
            let text = text.trim_end_matches('0').trim_end_matches('.');
            if text == "-0" {
                "0".to_owned()
            } else {
                text.to_owned()
            }
        }
    }
}

fn unit_of(dimensions: &[Dimension], reference: Option<DimensionRef>) -> DimensionUnit {
    reference
        .and_then(|r| dimensions.iter().find(|d| d.reference == r))
        .map_or(DimensionUnit::Length, |d| d.unit)
}

impl OxydeApp {
    pub(crate) fn equations_window(&mut self, ctx: &egui::Context) {
        self.equations.begin_frame();
        if !self.equations.open {
            self.equations.end_frame();
            return;
        }
        let mut open = true;
        let mut changes = Vec::new();
        let sketching = self.sketch.is_some();
        // Variables the assembly the part is edited in drives: their equations, as the
        // part names things, to change here.
        let links: HashMap<String, String> = self
            .doc
            .part
            .equations()
            .globals
            .iter()
            .filter_map(|g| Some((g.name.clone(), self.assembly_link_text(&g.name)?)))
            .collect();
        let part = &self.doc.part;
        let folder = self.doc.path.as_deref().and_then(std::path::Path::parent);
        let state = &mut self.equations;
        egui::Window::new("Equations, Global Variables and Dimensions")
            .open(&mut open)
            .resizable(true)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut state.tab, EquationsTab::Equations, "Equations");
                    ui.selectable_value(&mut state.tab, EquationsTab::Dimensions, "Dimensions");
                });
                ui.separator();
                if sketching {
                    ui.label("Exit the sketch to edit equations and dimensions here.");
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| match state.tab {
                    EquationsTab::Equations => {
                        equations_tab(ui, state, part, folder, &links, &mut changes)
                    }
                    EquationsTab::Dimensions => dimensions_tab(ui, state, part, &mut changes),
                });
            });
        if !open {
            self.equations.open = false;
            self.equations.editing.clear();
        }
        self.equations.end_frame();
        for change in changes {
            self.apply_equation_change(change);
        }
    }

    fn apply_equation_change(&mut self, change: Change) {
        let part = &self.doc.part;
        let parse = |text: &str| part.parse_expression(text);
        let outside = |text: &str| crate::dimension_equations::reaches_outside(text);
        let outcome: Result<(), ModelError> = match change {
            Change::SetGlobal(id, text) if outside(&text) => self
                .link_global_to_assembly(id, &text)
                .map_err(ModelError::Expression),
            Change::SetGlobal(id, text) if self.global_linked_to_assembly(id) => self
                .unlink_global_from_assembly(id, &text)
                .map_err(ModelError::Expression),
            Change::SetEquation(index, text) if outside(&text) => self
                .link_equation_to_assembly(index, &text)
                .map_err(ModelError::Expression),
            Change::AddEquation(target, text) if outside(&text) => self
                .link_dimension_to_assembly(target, &text)
                .map_err(ModelError::Expression),
            Change::SetDimension(reference, value) => {
                self.doc.checkpoint_edit(reference.feature);
                if self.doc.part.set_dimension(reference, value).is_ok() {
                    self.rebuild();
                }
                return;
            }
            Change::EndEdit => {
                self.doc.end_edit();
                return;
            }
            Change::RenameDimension(reference, name) => {
                self.edit_part(move |p| p.rename_dimension(reference, &name))
            }
            Change::AddGlobal(name, text) => match parse(&text) {
                Ok(expr) => self.edit_part(move |p| p.add_global(&name, expr.clone()).map(|_| ())),
                Err(e) => Err(e),
            },
            Change::RenameGlobal(id, name) => self.edit_part(move |p| p.rename_global(id, &name)),
            Change::SetGlobal(id, text) => match parse(&text) {
                Ok(_) if self.is_linked(id) => self.set_linked_global(id, &text),
                Ok(expr) => self.edit_part(move |p| p.set_global_expression(id, expr.clone())),
                Err(e) => Err(e),
            },
            Change::LinkFile => match self.pick_equations_file() {
                Some(result) => result,
                None => return,
            },
            Change::Unlink => self.edit_part(|p| p.link_equations(None, &[])),
            Change::Release(name) => self.edit_part(move |p| p.release_driven(&name)),
            Change::RemoveGlobal(id) => self.edit_part(move |p| p.remove_global(id)),
            Change::AddEquation(target, text) => match parse(&text) {
                Ok(expr) => {
                    self.edit_part(move |p| p.add_equation(target, expr.clone()).map(|_| ()))
                }
                Err(e) => Err(e),
            },
            Change::SetEquation(index, text) => match parse(&text) {
                Ok(expr) => self.edit_part(move |p| p.set_equation_expression(index, expr.clone())),
                Err(e) => Err(e),
            },
            Change::RemoveEquation(index) => self.edit_part(move |p| p.remove_equation(index)),
        };
        match outcome {
            Ok(()) => {
                self.rebuild();
                self.status = "Equations updated".to_owned();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    fn is_linked(&self, id: GlobalId) -> bool {
        let equations = self.doc.part.equations();
        equations.linked_file.is_some() && equations.globals.iter().any(|g| g.id == id && g.linked)
    }

    /// The equations file linked to the part, where it is on disk.
    fn linked_file_path(&self) -> Result<std::path::PathBuf, ModelError> {
        let file = self
            .doc
            .part
            .equations()
            .linked_file
            .clone()
            .ok_or_else(|| ModelError::Expression("the part is not linked".to_owned()))?;
        if std::path::Path::new(&file).is_absolute() {
            return Ok(file.into());
        }
        let part = self.doc.path.as_deref().ok_or_else(|| {
            ModelError::Expression("save the part first: its equations file is next to it".into())
        })?;
        Ok(oxyde_format::equations_file::linked_file_path(part, &file))
    }

    /// Writes a linked variable into the equations file, then reads the whole file back.
    fn set_linked_global(&mut self, id: GlobalId, text: &str) -> Result<(), ModelError> {
        let name = self
            .doc
            .part
            .equations()
            .globals
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.name.clone())
            .ok_or_else(|| ModelError::Expression("no such variable".to_owned()))?;
        let path = self.linked_file_path()?;
        let io = |e: oxyde_format::FormatError| {
            ModelError::Expression(format!("{}: {e}", path.display()))
        };
        let value = oxyde_model::LinkedValue {
            name,
            expression: text.to_owned(),
        };
        oxyde_format::equations_file::update_equations_file(&path, &[value]).map_err(io)?;
        let values = oxyde_format::equations_file::read_equations_file(&path).map_err(io)?;
        self.edit_part(move |p| p.apply_linked_values(&values))
    }

    /// Asks for an equations file (a new one is created with the part's variables) and links
    /// the part to it. `None` when the user cancels.
    fn pick_equations_file(&mut self) -> Option<Result<(), ModelError>> {
        let Some(part_path) = self.doc.path.clone() else {
            return Some(Err(ModelError::Expression(
                "Save the part first: its equations file is found next to it".to_owned(),
            )));
        };
        let folder = part_path.parent().map(|p| p.to_path_buf());
        let mut dialog = rfd::FileDialog::new()
            .set_title("Link to an equations file")
            .add_filter("Equations file", &["txt"])
            .set_file_name("Equations.txt");
        if let Some(folder) = &folder {
            dialog = dialog.set_directory(folder);
        }
        let path = dialog.save_file()?;
        let result = (|| {
            let io = |e: oxyde_format::FormatError| {
                ModelError::Expression(format!("{}: {e}", path.display()))
            };
            if !path.exists() {
                // A new file starts with the part's own variables.
                let values: Vec<_> = self
                    .doc
                    .part
                    .definition()
                    .equations_text()
                    .globals
                    .into_iter()
                    .map(|g| oxyde_model::LinkedValue {
                        name: g.name,
                        expression: g.expression,
                    })
                    .collect();
                oxyde_format::equations_file::update_equations_file(&path, &values).map_err(io)?;
            }
            let values = oxyde_format::equations_file::read_equations_file(&path).map_err(io)?;
            // Stored relative to the part's folder when it is in it or below, so the part and
            // its file can move together.
            let stored = folder
                .as_deref()
                .and_then(|f| path.strip_prefix(f).ok())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            self.edit_part(move |p| p.link_equations(Some(stored.clone()), &values))
        })();
        Some(result)
    }

    /// Applies a change to the part as one undo step. The change is tried on a copy first,
    /// so a refused change leaves no trace.
    pub(crate) fn edit_part(
        &mut self,
        change: impl Fn(&mut Part) -> Result<(), ModelError>,
    ) -> Result<(), ModelError> {
        let mut trial = Part::from_definition(self.doc.part.definition())?;
        change(&mut trial)?;
        self.doc.checkpoint();
        change(&mut self.doc.part)
    }
}

fn equations_tab(
    ui: &mut egui::Ui,
    state: &mut EquationsWindow,
    part: &Part,
    folder: Option<&std::path::Path>,
    links: &HashMap<String, String>,
    changes: &mut Vec<Change>,
) {
    let equations = part.equations();
    let results = part.equation_results();
    let dimensions = part.dimensions();

    ui.horizontal(|ui| {
        match &equations.linked_file {
            Some(file) => {
                ui.label(format!("🔗 Linked to {file}"))
                    .on_hover_text("Variables marked 🔗 come from this file and are shared with every part linked to it. Changing one changes the file.");
                if ui.small_button("Unlink").clicked() {
                    changes.push(Change::Unlink);
                }
            }
            None => {
                if ui
                    .small_button("🔗 Link to file…")
                    .on_hover_text("Share global variables with other parts through a text file")
                    .clicked()
                {
                    changes.push(Change::LinkFile);
                }
            }
        }
    });
    ui.add_space(4.0);
    ui.strong("Global variables");
    egui::Grid::new("globals")
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
                let driver = part.driver(&global.name);
                if let Some(d) = driver {
                    // "->", as SolidWorks marks external references.
                    ui.label(format!("{} ->", global.name))
                        .on_hover_text(format!(
                            "Driven by {} (an assembly equation). Edited in place from it, its \
                         equation is here to change; else release it below",
                            d.assembly_name()
                        ));
                } else if linked {
                    // The name is the file's: renaming it here would break the link.
                    ui.label(format!("🔗 {}", global.name))
                        .on_hover_text("From the linked equations file");
                } else if let Some(name) =
                    state.field(ui, Field::GlobalName(global.id), &global.name, 110.0)
                {
                    changes.push(Change::RenameGlobal(global.id, name));
                }
                // Driven by the assembly the part is edited in: its equation there, to change
                // (a value typed instead takes the link away). By another: read only.
                let link = links.get(&global.name);
                let text = link
                    .cloned()
                    .unwrap_or_else(|| part.expression_text(&global.expr));
                if driver.is_some() && link.is_none() {
                    ui.label(&text);
                } else if let Some(text) =
                    state.field(ui, Field::GlobalExpression(global.id), &text, 220.0)
                {
                    changes.push(Change::SetGlobal(global.id, text));
                }
                result_label(ui, results.globals.get(i), None);
                if linked || driver.is_some() {
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
                    .desired_width(220.0),
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
    egui::Grid::new("equations")
        .num_columns(4)
        .striped(true)
        .show(ui, |ui| {
            ui.label(RichText::new("Dimension").weak());
            ui.label(RichText::new("= Equation").weak());
            ui.label(RichText::new("Evaluates to").weak());
            ui.label("");
            ui.end_row();
            for (i, equation) in equations.equations.iter().enumerate() {
                let target = part.equation_target_name(equation);
                let label = if equation.target.is_some_and(|t| part.dimension(t).is_some()) {
                    RichText::new(format!("\"{target}\""))
                } else {
                    RichText::new(format!("\"{target}\"")).color(error())
                };
                ui.label(label);
                let text = part.expression_text(&equation.expr);
                if let Some(text) = state.field(ui, Field::EquationExpression(i), &text, 220.0) {
                    changes.push(Change::SetEquation(i, text));
                }
                result_label(
                    ui,
                    results.equations.get(i),
                    Some(unit_of(&dimensions, equation.target)),
                );
                if ui.small_button("🗑").on_hover_text("Delete").clicked() {
                    changes.push(Change::RemoveEquation(i));
                }
                ui.end_row();
            }

            let free: Vec<&Dimension> = dimensions.iter().filter(|d| !d.driven).collect();
            if state
                .new_equation_target
                .is_some_and(|t| !free.iter().any(|d| d.reference == t))
            {
                state.new_equation_target = None;
            }
            let selected = state
                .new_equation_target
                .and_then(|t| free.iter().find(|d| d.reference == t))
                .map_or_else(|| "Dimension…".to_owned(), |d| d.full_name.clone());
            egui::ComboBox::from_id_salt("new_equation_target")
                .selected_text(selected)
                .width(150.0)
                .show_ui(ui, |ui| {
                    for d in &free {
                        ui.selectable_value(
                            &mut state.new_equation_target,
                            Some(d.reference),
                            &d.full_name,
                        );
                    }
                });
            let expression = ui.add(
                egui::TextEdit::singleline(&mut state.new_equation_expression)
                    .hint_text("e.g. \"Width\" * 2")
                    .desired_width(220.0),
            );
            let entered = expression.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let ready = state.new_equation_target.is_some()
                && !state.new_equation_expression.trim().is_empty();
            if ui.add_enabled(ready, egui::Button::new("Add")).clicked() || (entered && ready) {
                if let Some(target) = state.new_equation_target.take() {
                    changes.push(Change::AddEquation(
                        target,
                        state.new_equation_expression.trim().to_owned(),
                    ));
                    state.new_equation_expression.clear();
                }
            }
            ui.end_row();
        });

    if !part.driven_by().is_empty() {
        driven_by_list(ui, part, &dimensions, folder, changes);
    }

    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Click a field, then a variable in the tree's Equations folders to put it in. \
             Edited in place, the assembly's variables (\"Width@Case.Assembly\") and other \
             parts' (\"Gap@Foot<1>.Part\") make it an equation of the assembly.",
        )
        .small()
        .weak(),
    );
    ui.label(
        RichText::new(
            "Names go between double quotes: \"Width\", \"D1@Sketch1\". Operators + - * / ^ \
             and functions sin cos tan asin acos atan (degrees), sqrt abs exp log int round \
             sgn min max, and pi. Lengths in mm, angles in degrees.",
        )
        .small()
        .weak(),
    );
}

/// The values assemblies drive in the part, as SolidWorks lists external references: each
/// with its assembly, and Release to make it the part's own again.
fn driven_by_list(
    ui: &mut egui::Ui,
    part: &Part,
    dimensions: &[Dimension],
    folder: Option<&std::path::Path>,
    changes: &mut Vec<Change>,
) {
    ui.add_space(8.0);
    ui.strong("Driven by assemblies");
    egui::Grid::new("driven_by")
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            for d in part.driven_by() {
                let known = dimensions.iter().any(|x| x.full_name == d.name)
                    || part.equations().globals.iter().any(|g| g.name == d.name);
                let name = RichText::new(format!("\"{}\"", d.name));
                if known {
                    ui.label(name);
                } else {
                    ui.label(name.color(error()))
                        .on_hover_text("No longer in the part: renamed or deleted");
                }
                // A file that is not where the part says (moved, or deleted): SolidWorks' ->?.
                let missing = folder.is_some_and(|f| {
                    !oxyde_assembly::paths::absolute(&d.assembly, Some(f)).exists()
                });
                let text = if missing {
                    format!("{} (not found)", d.assembly_name())
                } else {
                    d.assembly_name().to_owned()
                };
                ui.label(text).on_hover_text(&d.assembly);
                if ui
                    .small_button("Release")
                    .on_hover_text(
                        "Make it the part's own again, as SolidWorks breaks an external \
                         reference. The assembly drives it again when it is next opened and \
                         saved, unless its equation is deleted there.",
                    )
                    .clicked()
                {
                    changes.push(Change::Release(d.name.clone()));
                }
                ui.end_row();
            }
        });
}

fn dimensions_tab(
    ui: &mut egui::Ui,
    state: &mut EquationsWindow,
    part: &Part,
    changes: &mut Vec<Change>,
) {
    let dimensions = part.dimensions();
    if dimensions.is_empty() {
        ui.label("No dimensions yet. Dimension a sketch, or add a feature.");
        return;
    }
    ui.label(
        RichText::new("Names are used in equations, e.g. D1@Sketch1. Σ: driven by an equation.")
            .small()
            .weak(),
    );
    egui::Grid::new("dimensions")
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            ui.label(RichText::new("Name").weak());
            ui.label(RichText::new("Feature").weak());
            ui.label(RichText::new("Value").weak());
            ui.end_row();
            for d in &dimensions {
                if let Some(name) =
                    state.field(ui, Field::DimensionName(d.reference), &d.name, 90.0)
                {
                    changes.push(Change::RenameDimension(d.reference, name));
                }
                let feature = d.full_name.split_once('@').map_or("", |(_, f)| f);
                ui.label(format!("@{feature}"));
                // Angles read unsigned, as on the sketch; which way round they go (the
                // sign the model keeps) stays as it is when a value is typed.
                let angle = d.unit == DimensionUnit::Angle;
                let shown = d.unit.to_display(d.value);
                let shown = if angle { shown.abs() } else { shown };
                if d.driven {
                    ui.label(format!(
                        "Σ {}{}",
                        format_value(shown, d.unit),
                        d.unit.suffix()
                    ))
                    .on_hover_text(match &d.driven_by {
                        Some(assembly) => format!(
                            "Driven by an equation of {} (release it in the Equations tab)",
                            oxyde_model::DrivenBy::name_of(assembly)
                        ),
                        None => "Driven by an equation (Equations tab)".to_owned(),
                    });
                } else {
                    let mut edited = shown;
                    let max = crate::dialogs::MAX_LENGTH;
                    let drag = match d.unit {
                        DimensionUnit::Count => egui::DragValue::new(&mut edited)
                            .speed(0.1)
                            .max_decimals(0)
                            .range(1.0..=1000.0),
                        _ => egui::DragValue::new(&mut edited)
                            .speed(0.1)
                            .range(-max..=max),
                    }
                    .suffix(d.unit.suffix())
                    .custom_parser(move |text| {
                        crate::quantity::parse(text, crate::quantity::Quantity::of(d.unit))
                    });
                    let response = ui.add(drag);
                    if response.changed() {
                        if let Ok(value) = d.unit.from_display(edited) {
                            let value = if angle && d.value < 0.0 {
                                -value
                            } else {
                                value
                            };
                            changes.push(Change::SetDimension(d.reference, value));
                        }
                    }
                    // One drag or one typed value is one undo step.
                    if response.drag_stopped() || response.lost_focus() {
                        changes.push(Change::EndEdit);
                    }
                }
                ui.end_row();
            }
        });
}
