//! "=" in a sketch dimension's Modify box, as in SolidWorks: the value is typed as an
//! equation, `= "Width" - 6`, with names between double quotes, typed, picked from the list
//! under the box, or clicked in the tree's Equations folders (`equation_tree.rs`,
//! `in_place_tree.rs`).
//!
//! An equation naming only the part's own values (its global variables, `D1@Sketch1`) is
//! the part's. While the part is edited in place, an equation naming the assembly's
//! variables (`Width@Hackintosh case.Assembly`) or another part's (`Gap@Foot<1>.Part`) is an
//! equation of the assembly on this dimension: the part's own names in it are written as
//! the assembly names them, the value is set in the open part at once, and the part records
//! that the assembly drives it (`driven_by`), as SolidWorks makes an in-context reference.
//! The assembly keeps the equation, and drives the part from then on.

use std::collections::BTreeSet;

use oxyde_assembly::{AsmDimension, ComponentId};
use oxyde_model::{DimensionKey, DimensionRef, Operand};
use oxyde_sketch::RelationId;

use crate::app::OxydeApp;
use crate::dialogs::{Dialog, DimensionTarget};
use crate::edit_in_place::Suspended;
use crate::equation_tree::qualified;

/// The quoted names of an equation's text, in order (an unclosed last one included).
pub(crate) fn names_in(text: &str) -> Vec<&str> {
    text.split('"').skip(1).step_by(2).collect()
}

/// `text` with each quoted name replaced by `rename(name)`.
pub(crate) fn rename_names(text: &str, rename: &dyn Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, piece) in text.split('"').enumerate() {
        if i > 0 {
            out.push('"');
        }
        if i % 2 == 1 {
            out.push_str(&rename(piece));
        } else {
            out.push_str(piece);
        }
    }
    out
}

/// The expression of a value typed as an equation (`= "Width" * 2`), or `None` for a value.
pub(crate) fn equation_body(text: &str) -> Option<&str> {
    text.trim_start().strip_prefix('=').map(str::trim)
}

/// Whether a name is another document's: the assembly's (`…@Case.Assembly`) or another
/// part's (`…@Foot<1>.Part`).
fn is_outside(name: &str) -> bool {
    name.ends_with(".Assembly") || name.ends_with(".Part")
}

/// How the part being edited in place and its assembly are named in equations.
pub(crate) struct Names {
    /// `@Hackintosh case.Assembly`: after a name of the assembly's, seen from the part.
    assembly: String,
    /// `Top panel<1>.Part` (through sub-assemblies, `SSD<1>.Part@SSD holder<1>.Assembly`):
    /// after a name of the part's, seen from the assembly.
    part: String,
    /// The part's component path in the assembly.
    path: Vec<ComponentId>,
}

impl Names {
    pub(crate) fn of(parent: &Suspended) -> Self {
        let path: Vec<ComponentId> = std::iter::once(parent.component.top)
            .chain(parent.component.path.iter().copied())
            .collect();
        let probe = qualified(&parent.asm, &path, "N");
        Self {
            assembly: format!("@{}.Assembly", parent.asm.display_name()),
            part: probe
                .strip_prefix("N@")
                .map_or_else(|| probe.clone(), str::to_owned),
            path,
        }
    }

    /// An equation typed in the part, as the assembly writes it: the assembly's names lose
    /// their `@Case.Assembly`, the part's get `@Top panel<1>.Part`.
    pub(crate) fn to_assembly(&self, body: &str) -> String {
        rename_names(body, &|name| {
            if let Some(own) = name.strip_suffix(&self.assembly) {
                own.to_owned()
            } else if is_outside(name) {
                name.to_owned()
            } else {
                format!("{name}@{}", self.part)
            }
        })
    }

    /// An equation of the assembly, as the part shows it: the reverse of
    /// [`Names::to_assembly`].
    pub(crate) fn to_part(&self, text: &str) -> String {
        let own = format!("@{}", self.part);
        rename_names(text, &|name| {
            if let Some(part) = name.strip_suffix(&own) {
                part.to_owned()
            } else if is_outside(name) {
                name.to_owned()
            } else {
                format!("{name}{}", self.assembly)
            }
        })
    }

    /// The assembly's name for one of the part's values.
    fn target(&self, full_name: &str) -> AsmDimension {
        AsmDimension::Part {
            path: self.path.clone(),
            name: full_name.to_owned(),
        }
    }
}

/// Puts a variable's quoted name in an equation's text: it completes a name begun with a
/// quote, or comes after what is there.
pub(crate) fn insert_name(text: &mut String, name: &str) {
    if text.matches('"').count() % 2 == 1 {
        if let Some(open) = text.rfind('"') {
            text.truncate(open);
        }
    } else if !text.ends_with(' ') && !text.trim_end().ends_with('=') {
        text.push(' ');
    }
    text.push_str(name);
}

impl OxydeApp {
    /// Whether a dimension's value is being typed as an equation, so that the tree's
    /// variables can be clicked into it.
    pub(crate) fn equation_field_open(&self) -> bool {
        matches!(&self.dialog, Some(Dialog::Dimension { text, .. }) if equation_body(text).is_some())
    }

    /// Puts a variable's quoted name in the equation being typed: it completes a name begun
    /// with a quote, or comes after what is there.
    pub(crate) fn insert_into_equation_field(&mut self, name: &str) {
        if let Some(Dialog::Dimension { text, .. }) = &mut self.dialog
            && equation_body(text).is_some()
        {
            insert_name(text, name);
        }
    }

    /// The variables an equation typed here can name, quoted: the part's, and the
    /// assembly's while the part is edited in place.
    pub(crate) fn equation_variables(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .doc
            .part
            .equations()
            .globals
            .iter()
            .map(|g| format!("\"{}\"", g.name))
            .collect();
        if let Some(parent) = self.edit_stack.last() {
            let names = Names::of(parent);
            out.extend(
                parent
                    .asm
                    .assembly
                    .equations
                    .globals
                    .iter()
                    .map(|g| format!("\"{}{}\"", g.name, names.assembly)),
            );
        }
        out
    }

    /// The value an equation typed in the Modify box gives, in display units (mm or
    /// degrees), or why it has none.
    pub(crate) fn evaluate_dimension_equation(&self, text: &str) -> Result<f64, String> {
        let body = equation_body(text).ok_or_else(|| "not an equation".to_owned())?;
        if body.is_empty() {
            return Err("Type an expression: \"Width\" / 2, or click a variable".to_owned());
        }
        if !names_in(body).iter().any(|n| is_outside(n)) {
            let part = &self.doc.part;
            let expr = part.parse_expression(body).map_err(|e| e.to_string())?;
            let results = part.equation_results();
            return expr.evaluate(&mut |operand, name| match operand {
                Operand::Global(id) => part
                    .equations()
                    .globals
                    .iter()
                    .position(|g| g.id == *id)
                    .and_then(|i| results.globals.get(i))
                    .and_then(|r| r.as_ref().ok().copied())
                    .ok_or_else(|| format!("\"{name}\" has no value")),
                Operand::Dimension(r) => part
                    .dimension(*r)
                    .map(|d| d.unit.to_display(d.value))
                    .ok_or_else(|| format!("\"{name}\" does not exist")),
            });
        }
        let parent = self.edit_stack.last().ok_or_else(|| {
            "Names with @…Assembly or @…Part need the part edited in place in its assembly"
                .to_owned()
        })?;
        let names = Names::of(parent);
        let doc = &parent.asm;
        let expr = doc
            .assembly
            .parse_expression(&names.to_assembly(body), &doc.tree())
            .map_err(|e| e.to_string())?;
        expr.evaluate(&mut |operand, name| {
            let value = match operand {
                Operand::Global(id) => doc
                    .assembly
                    .equations
                    .globals
                    .iter()
                    .position(|g| g.id == *id)
                    .and_then(|i| doc.equation_results.globals.get(i))
                    .and_then(|r| r.as_ref().ok().copied()),
                Operand::Dimension(AsmDimension::Mate(id)) => doc
                    .assembly
                    .mate_dimensions()
                    .into_iter()
                    .find(|d| d.mate == *id)
                    .map(|d| d.value),
                // The part being edited is the open document, not the assembly's copy.
                Operand::Dimension(AsmDimension::Part { path, name }) if *path == names.path => {
                    self.doc.part.value_of(name)
                }
                Operand::Dimension(AsmDimension::Part { path, name }) => doc.part_value(path, name),
            };
            value.ok_or_else(|| format!("\"{name}\" has no value"))
        })
    }

    /// The equation driving a sketch dimension, as its Modify box shows it (`= "Width"`), or
    /// why it cannot be changed here: an assembly drives it that the part is not edited in.
    pub(crate) fn dimension_equation_text(
        &self,
        reference: DimensionRef,
    ) -> Result<String, String> {
        let part = &self.doc.part;
        let dimension = part
            .dimension(reference)
            .ok_or_else(|| "the dimension does not exist".to_owned())?;
        if let Some(e) = part
            .equations()
            .equations
            .iter()
            .find(|e| e.target == Some(reference))
        {
            return Ok(format!("= {}", part.expression_text(&e.expr)));
        }
        let Some(driver) = part.driver(&dimension.full_name) else {
            return Err(format!("{} is not driven", dimension.full_name));
        };
        let refused = || {
            format!(
                "{} is driven by {} (an assembly equation): edit it in place from there, or \
                 release it in Tools › Equations",
                dimension.full_name,
                driver.assembly_name()
            )
        };
        let parent = self
            .edit_stack
            .last()
            .filter(|p| p.asm.document_id == driver.assembly_id)
            .ok_or_else(refused)?;
        let names = Names::of(parent);
        let target = names.target(&dimension.full_name);
        let assembly = &parent.asm.assembly;
        let equation = assembly
            .equations
            .equations
            .iter()
            .find(|e| e.target.as_ref() == Some(&target))
            .ok_or_else(refused)?;
        let text = assembly.expression_text(&equation.expr, &parent.asm.tree());
        Ok(format!("= {}", names.to_part(&text)))
    }

    /// OK in the Modify box with an equation: the value it gives is set as a typed value
    /// would be, then the equation is recorded, the part's or the assembly's.
    pub(crate) fn apply_dimension_equation(
        &mut self,
        target: DimensionTarget,
        name: Option<String>,
        text: &str,
    ) {
        match self.dimension_equation(target, name, text) {
            Ok(message) => self.status = message,
            Err(e) => self.status = e,
        }
    }

    fn dimension_equation(
        &mut self,
        target: DimensionTarget,
        name: Option<String>,
        text: &str,
    ) -> Result<String, String> {
        let body = equation_body(text)
            .ok_or_else(|| "not an equation".to_owned())?
            .to_owned();
        let value = self.evaluate_dimension_equation(text)?;
        let session = self
            .sketch
            .as_mut()
            .ok_or_else(|| "no sketch is open".to_owned())?;
        let relation = match target {
            DimensionTarget::New(relation) => session
                .add_dimension_id(relation, crate::sketch_mode::typed_value(&relation, value))
                .ok_or_else(|| "That dimension conflicts with the existing relations".to_owned())?,
            DimensionTarget::Existing(id) => {
                if let Some(name) = name {
                    session.rename_dimension(id, name.trim());
                }
                let constraint = session
                    .sketch
                    .relation(id)
                    .map(|r| r.constraint)
                    .ok_or_else(|| "the dimension is gone".to_owned())?;
                session.set_dimension(id, crate::sketch_mode::typed_value(&constraint, value));
                id
            }
        };
        let feature = session.feature;
        // The sketch as it is now goes into the part, where the equation can name it.
        self.commit_sketch();
        let reference = DimensionRef {
            feature,
            key: DimensionKey::Sketch(relation),
        };
        let full = self
            .doc
            .part
            .dimension(reference)
            .map(|d| d.full_name)
            .ok_or_else(|| "the dimension is not in the part".to_owned())?;
        if names_in(&body).iter().any(|n| is_outside(n)) {
            self.link_to_assembly(reference, &full, &body)?;
        } else {
            let expr = self
                .doc
                .part
                .parse_expression(&body)
                .map_err(|e| e.to_string())?;
            let existing = self
                .doc
                .part
                .equations()
                .equations
                .iter()
                .position(|e| e.target == Some(reference));
            self.edit_part(|p| match existing {
                Some(i) => p.set_equation_expression(i, expr.clone()),
                None => p.add_equation(reference, expr.clone()).map(|_| ()),
            })
            .map_err(|e| e.to_string())?;
        }
        if let Some(session) = self.sketch.as_mut() {
            session.driven.insert(relation);
        }
        self.rebuild();
        Ok(format!("\"{full}\" = {body}"))
    }

    /// Makes the assembly the part is edited in drive one of its dimensions by `body`
    /// (written as the part names things): the equation goes in the assembly, as one undo
    /// step there; the part's own equation on the dimension gives way, and the part records
    /// that the assembly drives it.
    fn link_to_assembly(
        &mut self,
        reference: DimensionRef,
        full: &str,
        body: &str,
    ) -> Result<(), String> {
        let parent = self.edit_stack.last().ok_or_else(|| {
            "Names with @…Assembly or @…Part need the part edited in place in its assembly"
                .to_owned()
        })?;
        let assembly_path =
            parent.asm.path.clone().ok_or_else(|| {
                "Save the assembly first: the part records it by its file".to_owned()
            })?;
        let assembly_id = parent.asm.document_id.clone();
        if let Some(other) = self
            .doc
            .part
            .driver(full)
            .filter(|d| d.assembly_id != assembly_id)
        {
            return Err(format!(
                "\"{full}\" is driven by {}: release it first (Tools › Equations)",
                other.assembly_name()
            ));
        }
        let names = Names::of(parent);
        let target = names.target(full);
        let (expr, target_name) = {
            let tree = parent.asm.tree();
            let expr = parent
                .asm
                .assembly
                .parse_expression(&names.to_assembly(body), &tree)
                .map_err(|e| e.to_string())?;
            let name = parent
                .asm
                .assembly
                .dimension_name(&target, &tree)
                .unwrap_or_else(|| full.to_owned());
            (expr, name)
        };

        // The part: its own equation on the dimension goes, and it records the assembly.
        let stored = oxyde_assembly::paths::stored(
            &assembly_path,
            self.doc.path.as_deref().and_then(std::path::Path::parent),
        );
        let mut driven: BTreeSet<String> = self
            .doc
            .part
            .driven_by()
            .iter()
            .filter(|d| d.assembly_id == assembly_id)
            .map(|d| d.name.clone())
            .collect();
        driven.insert(full.to_owned());
        let own = self
            .doc
            .part
            .equations()
            .equations
            .iter()
            .position(|e| e.target == Some(reference));
        self.edit_part(|p| {
            if let Some(i) = own {
                p.remove_equation(i)?;
            }
            p.set_driven_by(&assembly_id, &stored, &driven);
            Ok(())
        })
        .map_err(|e| e.to_string())?;

        // The assembly keeps the equation.
        let Some(parent) = self.edit_stack.last_mut() else {
            return Err("the assembly is gone".to_owned());
        };
        let doc = &mut parent.asm;
        doc.checkpoint();
        let equations = &mut doc.assembly.equations;
        let existing = equations
            .equations
            .iter()
            .position(|e| e.target.as_ref() == Some(&target));
        match existing {
            Some(i) => equations.set_equation_expression(i, expr),
            None => equations
                .add_equation(target, target_name, expr)
                .map(|_| ()),
        }
        .map_err(|e| e.to_string())?;
        doc.modified = true;
        Ok(())
    }

    /// A value typed over a driven sketch dimension: the equation driving it goes, the
    /// part's or (edited in place) the assembly's, as SolidWorks deletes it, and the value
    /// is the part's own again.
    pub(crate) fn unlink_dimension(&mut self, relation: RelationId) -> Result<(), String> {
        let Some(feature) = self.sketch.as_ref().map(|s| s.feature) else {
            return Ok(());
        };
        let reference = DimensionRef {
            feature,
            key: DimensionKey::Sketch(relation),
        };
        let Some(full) = self.doc.part.dimension(reference).map(|d| d.full_name) else {
            return Ok(());
        };
        if let Some(index) = self
            .doc
            .part
            .equations()
            .equations
            .iter()
            .position(|e| e.target == Some(reference))
        {
            self.edit_part(|p| p.remove_equation(index))
                .map_err(|e| e.to_string())?;
        }
        if let Some(driver) = self.doc.part.driver(&full).cloned() {
            let parent = self
                .edit_stack
                .last_mut()
                .filter(|p| p.asm.document_id == driver.assembly_id)
                .ok_or_else(|| {
                    format!(
                        "{full} is driven by {}: release it in Tools › Equations first",
                        driver.assembly_name()
                    )
                })?;
            let names = Names::of(parent);
            let target = names.target(&full);
            let doc = &mut parent.asm;
            if let Some(i) = doc
                .assembly
                .equations
                .equations
                .iter()
                .position(|e| e.target.as_ref() == Some(&target))
            {
                doc.checkpoint();
                doc.assembly
                    .equations
                    .remove_equation(i)
                    .map_err(|e| e.to_string())?;
                doc.modified = true;
            }
            self.edit_part(|p| p.release_driven(&full))
                .map_err(|e| e.to_string())?;
        }
        if let Some(session) = self.sketch.as_mut() {
            session.driven.remove(&relation);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        Names {
            assembly: "@Hackintosh case.Assembly".to_owned(),
            part: "Top panel<1>.Part".to_owned(),
            path: vec![ComponentId(8)],
        }
    }

    #[test]
    fn names_are_rewritten_between_the_part_and_its_assembly() {
        let n = names();
        let typed = "\"Width@Hackintosh case.Assembly\" - 2 * \"Thickness\" + \"Gap@Foot<1>.Part\"";
        let assembly = n.to_assembly(typed);
        assert_eq!(
            assembly,
            "\"Width\" - 2 * \"Thickness@Top panel<1>.Part\" + \"Gap@Foot<1>.Part\""
        );
        assert_eq!(n.to_part(&assembly), typed);
        assert_eq!(names_in(typed).len(), 3);
        assert_eq!(equation_body("  = \"Width\" "), Some("\"Width\""));
        assert_eq!(equation_body("12.5"), None);
    }

    #[test]
    fn clicked_names_complete_or_follow_what_is_typed() {
        let mut text = "=".to_owned();
        insert_name(&mut text, "\"Width\"");
        assert_eq!(text, "=\"Width\"");
        text.push_str(" - 2 * \"Thi");
        insert_name(&mut text, "\"Thickness\"");
        assert_eq!(text, "=\"Width\" - 2 * \"Thickness\"");
        insert_name(&mut text, "\"Gap\"");
        assert_eq!(text, "=\"Width\" - 2 * \"Thickness\" \"Gap\"");
    }
}
