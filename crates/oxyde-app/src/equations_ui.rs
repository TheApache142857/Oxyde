//! Tools › Equations: global variables, equations and every dimension of the part, as in
//! the SolidWorks "Equations, Global Variables, and Dimensions" window.
//!
//! - **Equations** tab: global variables (`"Width" = 50`) and equations driving dimensions
//!   (`"D1@Sketch1" = "Width" * 2`), with the value each evaluates to, or its error.
//! - **Dimensions** tab: every dimension under its full name, with its value. Names and
//!   values can be edited; dimensions driven by an equation are shown with Σ.
//!
//! Every change is one undo step and triggers a rebuild.

use std::collections::HashMap;

use eframe::egui::{self, Color32, RichText};
use oxyde_model::{Dimension, DimensionRef, DimensionUnit, GlobalId, ModelError, Part};

use crate::app::OxydeApp;

fn error() -> Color32 {
    crate::theme::tokens().err
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquationsTab {
    Equations,
    Dimensions,
}

/// A text field of the window, while it is being typed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Field {
    DimensionName(DimensionRef),
    GlobalName(GlobalId),
    GlobalExpression(GlobalId),
    EquationExpression(usize),
}

pub struct EquationsWindow {
    pub open: bool,
    pub tab: EquationsTab,
    /// Text of the field being typed in; every other field shows the model.
    editing: HashMap<Field, String>,
    new_global_name: String,
    new_global_expression: String,
    new_equation_target: Option<DimensionRef>,
    new_equation_expression: String,
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
        }
    }

    pub fn show(&mut self, tab: EquationsTab) {
        self.open = true;
        self.tab = tab;
    }

    /// A text field that shows `current` unless it is being typed in. Returns the new text
    /// when the user is done (focus lost or Enter) and it differs.
    fn field(
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
            self.editing.remove(&field);
            (text.trim() != current).then(|| text.trim().to_owned())
        } else {
            if response.has_focus() {
                self.editing.insert(field, text);
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
}

/// The value an expression evaluates to (with the unit of its dimension, if any), or its
/// error.
fn result_label(
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

fn format_value(value: f64, unit: DimensionUnit) -> String {
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
        if !self.equations.open {
            return;
        }
        let mut open = true;
        let mut changes = Vec::new();
        let sketching = self.sketch.is_some();
        let part = &self.doc.part;
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
                    EquationsTab::Equations => equations_tab(ui, state, part, &mut changes),
                    EquationsTab::Dimensions => dimensions_tab(ui, state, part, &mut changes),
                });
            });
        if !open {
            self.equations.open = false;
            self.equations.editing.clear();
        }
        for change in changes {
            self.apply_equation_change(change);
        }
    }

    fn apply_equation_change(&mut self, change: Change) {
        let part = &self.doc.part;
        let parse = |text: &str| part.parse_expression(text);
        let outcome: Result<(), ModelError> = match change {
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
                Ok(expr) => self.edit_part(move |p| p.set_global_expression(id, expr.clone())),
                Err(e) => Err(e),
            },
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
    changes: &mut Vec<Change>,
) {
    let equations = part.equations();
    let results = part.equation_results();
    let dimensions = part.dimensions();

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
                if let Some(name) =
                    state.field(ui, Field::GlobalName(global.id), &global.name, 110.0)
                {
                    changes.push(Change::RenameGlobal(global.id, name));
                }
                let text = part.expression_text(&global.expr);
                if let Some(text) =
                    state.field(ui, Field::GlobalExpression(global.id), &text, 220.0)
                {
                    changes.push(Change::SetGlobal(global.id, text));
                }
                result_label(ui, results.globals.get(i), None);
                if ui.small_button("🗑").on_hover_text("Delete").clicked() {
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

    ui.add_space(6.0);
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
                let shown = d.unit.to_display(d.value);
                if d.driven {
                    ui.label(format!(
                        "Σ {}{}",
                        format_value(shown, d.unit),
                        d.unit.suffix()
                    ))
                    .on_hover_text("Driven by an equation (Equations tab)");
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
                    .suffix(d.unit.suffix());
                    let response = ui.add(drag);
                    if response.changed() {
                        if let Ok(value) = d.unit.from_display(edited) {
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
