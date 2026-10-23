//! The tree while a part is edited in place, as SolidWorks' Edit Component shows it: the
//! assembly it is edited in at the root, with its Equations folder, and its components. The
//! edited part's own tree hangs under its component; the other components are greyed, and
//! open to show what they hold and their Equations folders.
//!
//! While a dimension's value is typed as an equation, the variables of every folder can be
//! clicked into it: the part's own by name, the assembly's as `Width@Case.Assembly` and
//! another part's as `Gap@Foot<1>.Part`. A link to anything outside the part becomes an
//! equation of the assembly (`dimension_equations.rs`).

use std::collections::HashSet;

use eframe::egui::{self, RichText};
use oxyde_assembly::ComponentId;

use crate::app::OxydeApp;
use crate::assembly::{Loaded, Occurrence};
use crate::edit_in_place::Suspended;
use crate::equation_tree::{
    FolderEvent, assembly_contents, equations_folder, part_contents, qualified,
};
use crate::icons::Icon;
use crate::timeline::{Row, tree_row};

/// A node of the tree that can be opened: a component (by its path from the assembly) or,
/// with `folder`, its Equations folder (the assembly's own for an empty path).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Node {
    pub path: Vec<ComponentId>,
    pub folder: bool,
}

/// What a click in the assembly's part of the tree asks for.
pub(crate) enum PlaceAction {
    Toggle(Node),
    /// Edit another part in place: the current one is finished first.
    EditPart(Occurrence),
    /// A variable's quoted name, for the equation being typed.
    Insert(String),
}

/// The tree around the edited part, drawn for one frame.
pub(crate) struct InPlaceTree<'a> {
    pub parent: &'a Suspended,
    pub expanded: &'a HashSet<Node>,
    pub action: &'a mut Option<PlaceAction>,
    /// Not in sketch mode: components can be opened and edited.
    pub enabled: bool,
    /// An equation is being typed: variables can be clicked into it.
    pub inserting: bool,
}

fn dim() -> egui::Color32 {
    crate::theme::tokens().dim
}

impl InPlaceTree<'_> {
    /// Draws the tree, with `body` (the edited part's own tree) under its component.
    pub fn show(&mut self, ui: &mut egui::Ui, body: &mut dyn FnMut(&mut egui::Ui)) {
        // Borrowed apart from `self`, whose action is set while drawing.
        let parent = self.parent;
        let doc = &parent.asm;
        let root = tree_row(
            ui,
            Row {
                icon: Icon::Assembly,
                cut: false,
                text: RichText::new(doc.display_name()).strong(),
                highlighted: false,
                hovered: false,
                badge: None,
                indent: 0.0,
                expander: None,
                eye: None,
            },
        );
        root.response.on_hover_text(format!(
            "{} is edited in this assembly: File › Finish editing goes back to it",
            parent.component_name
        ));
        let edited: Vec<ComponentId> = std::iter::once(parent.component.top)
            .chain(parent.component.path.iter().copied())
            .collect();
        ui.indent("in_place_assembly", |ui| {
            let contents = assembly_contents(doc, true);
            if !contents.is_empty() {
                let node = Node {
                    path: Vec::new(),
                    folder: true,
                };
                let open = self.expanded.contains(&node) || self.inserting;
                let event = ui
                    .add_enabled_ui(self.enabled || self.inserting, |ui| {
                        equations_folder(ui, &contents, 0.0, open, self.inserting, false)
                    })
                    .inner;
                self.folder_event(event, node);
            }
            for c in &doc.assembly.components {
                let loaded = doc.loaded(c.id);
                self.component(ui, vec![c.id], &c.name, loaded, &edited, body);
            }
        });
    }

    fn folder_event(&mut self, event: Option<FolderEvent>, node: Node) {
        match event {
            Some(FolderEvent::Toggle) => *self.action = Some(PlaceAction::Toggle(node)),
            Some(FolderEvent::Insert(name)) => *self.action = Some(PlaceAction::Insert(name)),
            Some(FolderEvent::Manage) | None => {}
        }
    }

    fn component(
        &mut self,
        ui: &mut egui::Ui,
        path: Vec<ComponentId>,
        name: &str,
        loaded: Option<&Loaded>,
        edited: &[ComponentId],
        body: &mut dyn FnMut(&mut egui::Ui),
    ) {
        let parent = self.parent;
        let doc = &parent.asm;
        let on_path = edited.starts_with(&path);
        let is_edited = edited == path.as_slice();
        let node = Node {
            path: path.clone(),
            folder: false,
        };
        let expanded = on_path || self.expanded.contains(&node);
        let sub = matches!(loaded, Some(Loaded::Assembly(_)));
        // "->" for a part this assembly drives, as SolidWorks marks external references.
        let external = match loaded {
            Some(Loaded::Part(p)) if !p.part.driven_by().is_empty() => {
                if p.part
                    .driven_by()
                    .iter()
                    .any(|d| d.assembly_id == doc.document_id)
                {
                    " ->"
                } else {
                    " ->?"
                }
            }
            _ => "",
        };
        let label = format!("{name}{external}");
        let text = if on_path {
            RichText::new(label)
        } else {
            RichText::new(label).color(dim())
        };
        let row = ui
            .add_enabled_ui(self.enabled || on_path, |ui| {
                tree_row(
                    ui,
                    Row {
                        icon: if sub { Icon::Assembly } else { Icon::Part },
                        cut: false,
                        text,
                        highlighted: is_edited,
                        hovered: false,
                        badge: None,
                        indent: 0.0,
                        expander: (!is_edited).then_some(expanded),
                        eye: None,
                    },
                )
            })
            .inner;
        if is_edited {
            row.response
                .on_hover_text("Being edited: its features are below");
            ui.indent(("in_place_edited", &path), |ui| body(ui));
            return;
        }
        if row.response.clicked() && !on_path {
            *self.action = Some(PlaceAction::Toggle(node));
        }
        if let (Some((top, inner)), true) = (path.split_first(), loaded.is_some() && !on_path) {
            let occurrence = Occurrence {
                top: *top,
                path: inner.to_vec(),
            };
            row.response.context_menu(|ui| {
                let label = if sub {
                    "Edit sub-assembly in place"
                } else {
                    "Edit part in place"
                };
                if ui
                    .button(label)
                    .on_hover_text("Finishes editing this part, then opens that one")
                    .clicked()
                {
                    *self.action = Some(PlaceAction::EditPart(occurrence.clone()));
                    ui.close_menu();
                }
            });
        }
        if !expanded {
            return;
        }
        ui.indent(("in_place_component", &path), |ui| match loaded {
            Some(Loaded::Part(p)) => {
                let contents = part_contents(&p.part, &|name| qualified(doc, &path, name));
                if contents.is_empty() {
                    ui.label(RichText::new("No global variables").small().weak());
                    return;
                }
                let node = Node {
                    path: path.clone(),
                    folder: true,
                };
                let open = self.expanded.contains(&node) || self.inserting;
                let event = ui
                    .add_enabled_ui(self.enabled || self.inserting, |ui| {
                        equations_folder(ui, &contents, 0.0, open, self.inserting, false)
                    })
                    .inner;
                self.folder_event(event, node);
            }
            Some(Loaded::Assembly(sub)) => {
                for c in &sub.assembly.components {
                    let inner = sub
                        .sources
                        .get(&c.id)
                        .and_then(|file| doc.docs.get(file))
                        .and_then(|l| l.as_ref().ok());
                    let mut child = path.clone();
                    child.push(c.id);
                    self.component(ui, child, &c.name, inner, edited, body);
                }
            }
            None => {
                ui.label(RichText::new("Not loaded").small().weak());
            }
        });
    }
}

impl OxydeApp {
    /// Applies a click in the assembly's part of the tree while editing in place.
    pub(crate) fn apply_place_action(&mut self, action: PlaceAction, ctx: &egui::Context) {
        match action {
            PlaceAction::Toggle(node) => {
                if !self.place_expanded.remove(&node) {
                    self.place_expanded.insert(node);
                }
            }
            PlaceAction::EditPart(o) => {
                // Finished first, asking about unsaved changes; the other part follows once
                // the assembly is back (`assembly_back`).
                self.after_unwind = Some(crate::app::PendingAction::OpenComponent(o));
                self.guard(crate::app::PendingAction::FinishEdit, ctx);
            }
            PlaceAction::Insert(name) => self.insert_into_equation_field(&name),
        }
    }
}
