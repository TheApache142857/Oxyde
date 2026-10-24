//! The Equations folder of the trees, as SolidWorks shows one in the FeatureManager: a
//! document's global variables and equations, with their values. While a dimension's value
//! is typed as an equation ("= …"), clicking a variable puts its name in it; a double-click
//! opens the document's Equations window.

use eframe::egui::{self, RichText};
use oxyde_assembly::{AsmDimension, ComponentId};
use oxyde_model::{DimensionUnit, Part};

use crate::app::OxydeApp;
use crate::assembly::AssemblyDoc;
use crate::equations_ui::format_value;
use crate::icons::Icon;
use crate::timeline::{Row, tree_row};

/// A global variable as the folder lists it.
pub(crate) struct VariableRow {
    /// `Width = 233`, with 🔗 or Σ in front for a linked or a driven one.
    pub text: String,
    /// Its name in an equation, quoted: `"Width"`, `"Width@Hackintosh case.Assembly"`.
    pub insert: String,
    pub hover: String,
}

/// What the folder's contents are: its variables, and its equations as text.
#[derive(Default)]
pub(crate) struct FolderContents {
    pub variables: Vec<VariableRow>,
    pub equations: Vec<String>,
}

impl FolderContents {
    pub fn is_empty(&self) -> bool {
        self.variables.is_empty() && self.equations.is_empty()
    }
}

/// What a click in the folder asks for.
pub(crate) enum FolderEvent {
    /// The folder was opened or closed.
    Toggle,
    /// A variable was clicked while an equation is typed: its quoted name goes in it.
    Insert(String),
    /// The document's Equations window.
    Manage,
}

fn value_text(result: Option<&Result<f64, String>>) -> String {
    match result {
        Some(Ok(v)) => format_value(*v, DimensionUnit::Length),
        Some(Err(_)) => "⚠".to_owned(),
        None => "…".to_owned(),
    }
}

/// A part's variables and equations. `qualify` gives the name an equation of the document
/// being edited uses for a variable of this part: the name itself for the part's own, or
/// `Width@Foot<1>.Part` seen from its assembly.
pub(crate) fn part_contents(part: &Part, qualify: &dyn Fn(&str) -> String) -> FolderContents {
    let equations = part.equations();
    let results = part.equation_results();
    let linked_file = equations.linked_file.is_some();
    let variables = equations
        .globals
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let driver = part.driver(&g.name);
            // "->" for a variable an assembly drives, as SolidWorks marks external
            // references.
            let mark = if g.linked && linked_file { "🔗 " } else { "" };
            let external = if driver.is_some() { " ->" } else { "" };
            let expression = part.expression_text(&g.expr);
            let mut hover = format!("\"{}\" = {expression}", g.name);
            if let Some(d) = driver {
                hover.push_str(&format!("\nDriven by {}", d.assembly_name()));
            } else if g.linked && linked_file {
                hover.push_str("\nFrom the linked equations file");
            }
            VariableRow {
                text: format!(
                    "{mark}{} = {}{external}",
                    g.name,
                    value_text(results.globals.get(i))
                ),
                insert: format!("\"{}\"", qualify(&g.name)),
                hover,
            }
        })
        .collect();
    let mut lines: Vec<String> = equations
        .equations
        .iter()
        .map(|e| {
            format!(
                "\"{}\" = {}",
                part.equation_target_name(e),
                part.expression_text(&e.expr)
            )
        })
        .collect();
    lines.extend(
        part.driven_by()
            .iter()
            .map(|d| format!("\"{}\" -> {}", d.name, d.assembly_name())),
    );
    FolderContents {
        variables,
        equations: lines,
    }
}

/// An assembly's variables and equations. Its variables are named as an equation of one
/// of its parts edited in place names them, `Width@Hackintosh case.Assembly`, when
/// `from_part` is set, else by their own names.
pub(crate) fn assembly_contents(doc: &AssemblyDoc, from_part: bool) -> FolderContents {
    let assembly = &doc.assembly;
    let tree = doc.tree();
    let name = doc.display_name();
    let variables = assembly
        .equations
        .globals
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let linked = g.linked && assembly.equations.linked_file.is_some();
            let insert = if from_part {
                format!("\"{}@{name}.Assembly\"", g.name)
            } else {
                format!("\"{}\"", g.name)
            };
            VariableRow {
                text: format!(
                    "{}{} = {}",
                    if linked { "🔗 " } else { "" },
                    g.name,
                    value_text(doc.equation_results.globals.get(i))
                ),
                insert,
                hover: format!(
                    "\"{}\" = {}{}",
                    g.name,
                    assembly.expression_text(&g.expr, &tree),
                    if linked {
                        "\nFrom the linked equations file"
                    } else {
                        ""
                    }
                ),
            }
        })
        .collect();
    let equations = assembly
        .equations
        .equations
        .iter()
        .map(|e| {
            let target = e
                .target
                .as_ref()
                .and_then(|t| assembly.dimension_name(t, &tree))
                .unwrap_or_else(|| e.target_name.clone());
            format!(
                "\"{target}\" = {}",
                assembly.expression_text(&e.expr, &tree)
            )
        })
        .collect();
    FolderContents {
        variables,
        equations,
    }
}

/// How an equation of `doc` names a value of the part at `path`: `Gap@Foot<1>.Part`.
pub(crate) fn qualified(doc: &AssemblyDoc, path: &[ComponentId], name: &str) -> String {
    let target = AsmDimension::Part {
        path: path.to_vec(),
        name: name.to_owned(),
    };
    doc.assembly
        .dimension_name(&target, &doc.tree())
        .unwrap_or_else(|| name.to_owned())
}

/// Draws an Equations folder at `indent`, with its rows when `expanded`. Variables are
/// clickable into an equation while `inserting`; `manage` offers the Equations window.
pub(crate) fn equations_folder(
    ui: &mut egui::Ui,
    contents: &FolderContents,
    indent: f32,
    expanded: bool,
    inserting: bool,
    manage: bool,
) -> Option<FolderEvent> {
    let mut event = None;
    let folder = tree_row(
        ui,
        Row {
            icon: Icon::Equations,
            cut: false,
            text: RichText::new("Equations"),
            highlighted: false,
            hovered: false,
            badge: None,
            indent,
            expander: Some(expanded),
            eye: None,
        },
    );
    if folder.response.double_clicked() && manage {
        event = Some(FolderEvent::Manage);
    } else if folder.response.clicked() {
        event = Some(FolderEvent::Toggle);
    }
    if manage {
        folder.response.context_menu(|ui| {
            if ui.button("Manage Equations…").clicked() {
                event = Some(FolderEvent::Manage);
                ui.close_menu();
            }
        });
    }
    if !expanded {
        return event;
    }
    for v in &contents.variables {
        let row = tree_row(
            ui,
            Row {
                icon: Icon::Equations,
                cut: false,
                text: RichText::new(&v.text),
                highlighted: false,
                hovered: false,
                badge: None,
                indent: indent + 16.0,
                expander: None,
                eye: None,
            },
        );
        let hint = if inserting {
            format!("{}\nClick to put {} in the equation", v.hover, v.insert)
        } else {
            v.hover.clone()
        };
        // Taken on the press: a field of the Equations window loses the focus to the click,
        // and only knows to take the name in the frame it loses it. Not by `hovered`: when
        // the tree scrolls, its scroll area takes the press (to drag it) and is the only
        // thing hovered in that frame, but the row still holds the pointer.
        let pressed = row.response.enabled()
            && row.response.contains_pointer()
            && ui.input(|i| i.pointer.primary_pressed());
        let response = row.response.on_hover_text(hint);
        if pressed && inserting {
            event = Some(FolderEvent::Insert(v.insert.clone()));
        } else if response.double_clicked() && manage {
            event = Some(FolderEvent::Manage);
        }
    }
    for line in &contents.equations {
        tree_row(
            ui,
            Row {
                icon: Icon::Equations,
                cut: false,
                text: RichText::new(line).weak(),
                highlighted: false,
                hovered: false,
                badge: None,
                indent: indent + 16.0,
                expander: None,
                eye: None,
            },
        );
    }
    event
}

impl OxydeApp {
    /// The part's Equations folder in its tree; open while an equation is typed, so that
    /// its variables are a click away.
    pub(crate) fn part_equations_folder(
        &mut self,
        ui: &mut egui::Ui,
        inserting: bool,
    ) -> Option<FolderEvent> {
        let contents = part_contents(&self.doc.part, &|name| name.to_owned());
        if contents.is_empty() {
            return None;
        }
        let expanded = self.part_equations_expanded || inserting;
        equations_folder(ui, &contents, 0.0, expanded, inserting, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxyde_model::{Expr, LinkedValue};

    #[test]
    fn a_part_folder_marks_linked_and_driven_variables() {
        let mut part = Part::new();
        part.add_global("Gap", Expr::Number(2.0)).unwrap();
        part.add_global("Thickness", Expr::Number(3.0)).unwrap();
        let width = LinkedValue {
            name: "Width".to_owned(),
            expression: "233".to_owned(),
        };
        part.link_equations(Some("Case.txt".to_owned()), &[width])
            .unwrap();
        let names = ["Thickness".to_owned()].into_iter().collect();
        part.set_driven_by("id", "../Case.oxyasm", &names);
        let contents = part_contents(&part, &|n| format!("{n}@Plate<1>.Part"));
        let texts: Vec<&str> = contents.variables.iter().map(|v| v.text.as_str()).collect();
        assert_eq!(texts, ["Gap = …", "Thickness = … ->", "🔗 Width = …"]);
        assert_eq!(contents.variables[0].insert, "\"Gap@Plate<1>.Part\"");
        assert!(contents.variables[1].hover.contains("Driven by Case"));
        assert_eq!(contents.equations, ["\"Thickness\" -> Case"]);
    }
}
