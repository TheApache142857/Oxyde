//! Linking a part's values to its assembly while it is edited in place, and "=" in a sketch
//! dimension's Modify box, as in SolidWorks.
//!
//! An equation is typed in the Modify box (`= "Width" - 6`) or in a field of the part's
//! Equations window, with names between double quotes, typed, picked from the list under
//! the box, or clicked in the tree's Equations folders (`equation_tree.rs`,
//! `in_place_tree.rs`).
//!
//! An equation naming only the part's own values (its global variables, `D1@Sketch1`) is
//! the part's. While the part is edited in place, one naming the assembly's variables
//! (`Width@Hackintosh case.Assembly`) or another part's (`Gap@Foot<1>.Part`) is an equation
//! of the assembly on that value, a dimension or a global variable of the part: the part's
//! own names in it are written as the assembly names them, the value is set in the open
//! part at once, and the part records that the assembly drives it (`driven_by`, SolidWorks'
//! external reference). The assembly keeps the equation and drives the part from then on.
//!
//! The linking itself works on the open part's [`Document`] and the assembly set aside
//! ([`Suspended`]), so that it can be tested without a window.

use std::collections::BTreeSet;

use oxyde_assembly::{AsmDimension, ComponentId};
use oxyde_model::{DimensionKey, DimensionRef, GlobalId, ModelError, Operand, Part};
use oxyde_sketch::RelationId;

use crate::app::OxydeApp;
use crate::dialogs::{Dialog, DimensionTarget};
use crate::document::Document;
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

/// Whether an expression names another document's values: it is then an equation of the
/// assembly the part is edited in.
pub(crate) fn reaches_outside(text: &str) -> bool {
    names_in(text).iter().any(|n| is_outside(n))
}

/// Puts a variable's quoted name in an equation's text: it replaces a lone value (a number,
/// or one name: clicking another variable points the value at it), completes a name begun
/// with a quote, or comes after what is there.
pub(crate) fn insert_name(text: &mut String, name: &str) {
    let (prefix, body) = match text.trim_start().strip_prefix('=') {
        Some(body) => ("= ", body.trim()),
        None => ("", text.trim()),
    };
    let lone_name = body.len() > 1
        && body.starts_with('"')
        && body.ends_with('"')
        && body.matches('"').count() == 2;
    if lone_name || body.parse::<f64>().is_ok() {
        *text = format!("{prefix}{name}");
        return;
    }
    if text.matches('"').count() % 2 == 1 {
        if let Some(open) = text.rfind('"') {
            text.truncate(open);
        }
    } else if !text.trim().is_empty() && !text.ends_with(' ') && !text.trim_end().ends_with('=') {
        text.push(' ');
    }
    text.push_str(name);
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

/// Changes the part document as one undo step; a change refused leaves no trace.
pub(crate) fn edit_part_doc(
    doc: &mut Document,
    change: &dyn Fn(&mut Part) -> Result<(), ModelError>,
) -> Result<(), ModelError> {
    let mut trial = Part::from_definition(doc.part.definition())?;
    change(&mut trial)?;
    doc.checkpoint();
    change(&mut doc.part)
}

/// The value an expression typed in the part edited in place gives, when it names the
/// assembly's or other parts' values: the assembly's last results, and the edited part's
/// values from the open document rather than the assembly's copy of it.
pub(crate) fn evaluate_in_place(
    part: &Part,
    parent: &Suspended,
    body: &str,
) -> Result<f64, String> {
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
            Operand::Dimension(AsmDimension::Part { path, name }) if *path == names.path => {
                part.value_of(name)
            }
            Operand::Dimension(AsmDimension::Part { path, name }) => doc.part_value(path, name),
        };
        value.ok_or_else(|| format!("\"{name}\" has no value"))
    })
}

/// The equation of the assembly the part is edited in that drives the part's value `full`
/// (a dimension's full name or a global variable's name), as the part names things.
pub(crate) fn link_text(parent: &Suspended, full: &str) -> Option<String> {
    let names = Names::of(parent);
    let target = names.target(full);
    let assembly = &parent.asm.assembly;
    let equation = assembly
        .equations
        .equations
        .iter()
        .find(|e| e.target.as_ref() == Some(&target))?;
    Some(names.to_part(&assembly.expression_text(&equation.expr, &parent.asm.tree())))
}

/// Makes the assembly the part is edited in (`parent`) drive the part's value `full` by
/// `body`, written as the part names things. `prepare` changes the part first: its own
/// equation on a dimension goes, a variable takes the value. The part records the assembly
/// (one undo step in the part) and the assembly keeps the equation (one undo step there,
/// left unsaved).
pub(crate) fn link_in_place(
    doc: &mut Document,
    parent: &mut Suspended,
    full: &str,
    body: &str,
    prepare: &dyn Fn(&mut Part) -> Result<(), ModelError>,
) -> Result<(), String> {
    let assembly_path = parent
        .asm
        .path
        .clone()
        .ok_or_else(|| "Save the assembly first: the part records it by its file".to_owned())?;
    let assembly_id = parent.asm.document_id.clone();
    if let Some(other) = doc
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

    // The part: changed as `prepare` says, and recording the assembly.
    let stored = oxyde_assembly::paths::stored(
        &assembly_path,
        doc.path.as_deref().and_then(std::path::Path::parent),
    );
    let mut driven: BTreeSet<String> = doc
        .part
        .driven_by()
        .iter()
        .filter(|d| d.assembly_id == assembly_id)
        .map(|d| d.name.clone())
        .collect();
    driven.insert(full.to_owned());
    edit_part_doc(doc, &|p| {
        prepare(p)?;
        p.set_driven_by(&assembly_id, &stored, &driven);
        Ok(())
    })
    .map_err(|e| e.to_string())?;

    // The assembly keeps the equation.
    let asm = &mut parent.asm;
    asm.checkpoint();
    let equations = &mut asm.assembly.equations;
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
    asm.modified = true;
    Ok(())
}

/// The assembly the part is edited in drives one of its dimensions by `body`; the part's own
/// equation on it gives way.
pub(crate) fn link_dimension_in_place(
    doc: &mut Document,
    parent: &mut Suspended,
    reference: DimensionRef,
    body: &str,
) -> Result<(), String> {
    let full = doc
        .part
        .dimension(reference)
        .map(|d| d.full_name)
        .ok_or_else(|| "the dimension is not in the part".to_owned())?;
    let own = doc
        .part
        .equations()
        .equations
        .iter()
        .position(|e| e.target == Some(reference));
    link_in_place(doc, parent, &full, body, &|p| match own {
        Some(i) => p.remove_equation(i),
        None => Ok(()),
    })
}

/// The assembly the part is edited in drives one of its global variables by `body`. A
/// variable of the part's linked equations file is taken over, no longer read from the
/// file.
pub(crate) fn link_global_in_place(
    doc: &mut Document,
    parent: &mut Suspended,
    name: &str,
    body: &str,
) -> Result<(), String> {
    let value = evaluate_in_place(&doc.part, parent, body)?;
    let assembly_id = parent.asm.document_id.clone();
    link_in_place(doc, parent, name, body, &|p| {
        p.drive(name, value, &assembly_id)
            .map(|_| ())
            .map_err(ModelError::Expression)
    })
}

/// The assembly no longer drives the part's value `full`: its equation goes (when the part
/// is edited in place from that assembly), and the part's record of it.
pub(crate) fn unlink_in_place(
    doc: &mut Document,
    parent: Option<&mut Suspended>,
    full: &str,
) -> Result<(), String> {
    let Some(driver) = doc.part.driver(full).cloned() else {
        return Ok(());
    };
    let parent = parent
        .filter(|p| p.asm.document_id == driver.assembly_id)
        .ok_or_else(|| {
            format!(
                "{full} is driven by {}: release it in Tools › Equations first",
                driver.assembly_name()
            )
        })?;
    let target = Names::of(parent).target(full);
    let asm = &mut parent.asm;
    if let Some(i) = asm
        .assembly
        .equations
        .equations
        .iter()
        .position(|e| e.target.as_ref() == Some(&target))
    {
        asm.checkpoint();
        asm.assembly
            .equations
            .remove_equation(i)
            .map_err(|e| e.to_string())?;
        asm.modified = true;
    }
    edit_part_doc(doc, &|p| p.release_driven(full)).map_err(|e| e.to_string())
}

impl OxydeApp {
    /// Whether a dimension's value is being typed as an equation in a Modify box.
    pub(crate) fn equation_field_open(&self) -> bool {
        matches!(&self.dialog, Some(Dialog::Dimension { text, .. }) if equation_body(text).is_some())
    }

    /// Whether a click on a variable in the tree goes somewhere: an equation typed in a
    /// Modify box, or a field of the Equations window being typed in.
    pub(crate) fn variables_clickable(&self) -> bool {
        self.equation_field_open() || (self.equations.open && self.equations.focused.is_some())
    }

    /// Puts a variable's quoted name where an equation is being typed: the Modify box, else
    /// the Equations window's field that had the focus (it takes it in its next drawing).
    pub(crate) fn insert_into_equation_field(&mut self, name: &str) {
        if let Some(Dialog::Dimension { text, .. }) = &mut self.dialog
            && equation_body(text).is_some()
        {
            insert_name(text, name);
            return;
        }
        if self.equations.open && self.equations.focused.is_some() {
            self.equations.pending_insert = Some(name.to_owned());
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
        if reaches_outside(body) {
            let parent = self.edit_stack.last().ok_or_else(|| {
                "Names with @…Assembly or @…Part need the part edited in place in its \
                 assembly"
                    .to_owned()
            })?;
            return evaluate_in_place(&self.doc.part, parent, body);
        }
        let part = &self.doc.part;
        let expr = part.parse_expression(body).map_err(|e| e.to_string())?;
        let results = part.equation_results();
        expr.evaluate(&mut |operand, name| match operand {
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
        })
    }

    /// The equation of the assembly the part is edited in that drives the part's value
    /// `full`, as the part names things; `None` when that assembly does not drive it.
    pub(crate) fn assembly_link_text(&self, full: &str) -> Option<String> {
        let parent = self.edit_stack.last()?;
        let driver = self.doc.part.driver(full)?;
        if driver.assembly_id != parent.asm.document_id {
            return None;
        }
        link_text(parent, full)
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
        self.assembly_link_text(&dimension.full_name)
            .map(|text| format!("= {text}"))
            .ok_or_else(|| {
                format!(
                    "{} is driven by {} (an assembly equation): edit it in place from there, \
                     or release it in Tools › Equations",
                    dimension.full_name,
                    driver.assembly_name()
                )
            })
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
        if reaches_outside(&body) {
            self.link_dimension_to_assembly(reference, &body)?;
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
        let full = self
            .doc
            .part
            .dimension(reference)
            .map_or_else(String::new, |d| d.full_name);
        Ok(format!("\"{full}\" = {body}"))
    }

    /// The assembly the part is edited in drives one of its dimensions by `body`; the
    /// part's own equation on it gives way.
    pub(crate) fn link_dimension_to_assembly(
        &mut self,
        reference: DimensionRef,
        body: &str,
    ) -> Result<(), String> {
        let parent = self.edit_stack.last_mut().ok_or_else(not_in_place)?;
        link_dimension_in_place(&mut self.doc, parent, reference, body)
    }

    /// The Equations window's equation `index`, given names of the assembly's: the
    /// assembly drives that dimension instead.
    pub(crate) fn link_equation_to_assembly(
        &mut self,
        index: usize,
        body: &str,
    ) -> Result<(), String> {
        let reference = self
            .doc
            .part
            .equations()
            .equations
            .get(index)
            .and_then(|e| e.target)
            .ok_or_else(|| "that equation's dimension no longer exists".to_owned())?;
        self.link_dimension_to_assembly(reference, body)
    }

    /// A global variable given names of the assembly's: the assembly drives it. A variable
    /// of the part's linked equations file is taken over, no longer read from the file.
    pub(crate) fn link_global_to_assembly(
        &mut self,
        id: GlobalId,
        body: &str,
    ) -> Result<(), String> {
        let name = self
            .doc
            .part
            .equations()
            .globals
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.name.clone())
            .ok_or_else(|| "no such variable".to_owned())?;
        let parent = self.edit_stack.last_mut().ok_or_else(not_in_place)?;
        link_global_in_place(&mut self.doc, parent, &name, body)
    }

    /// Whether the assembly the part is edited in drives this global variable.
    pub(crate) fn global_linked_to_assembly(&self, id: GlobalId) -> bool {
        self.doc
            .part
            .equations()
            .globals
            .iter()
            .find(|g| g.id == id)
            .is_some_and(|g| self.assembly_link_text(&g.name).is_some())
    }

    /// A value typed over a global variable the assembly drives: the link goes, and the
    /// variable is the part's own, with that value.
    pub(crate) fn unlink_global_from_assembly(
        &mut self,
        id: GlobalId,
        text: &str,
    ) -> Result<(), String> {
        let name = self
            .doc
            .part
            .equations()
            .globals
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.name.clone())
            .ok_or_else(|| "no such variable".to_owned())?;
        let expr = self
            .doc
            .part
            .parse_expression(text)
            .map_err(|e| e.to_string())?;
        unlink_in_place(&mut self.doc, self.edit_stack.last_mut(), &name)?;
        self.edit_part(|p| p.set_global_expression(id, expr.clone()))
            .map_err(|e| e.to_string())
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
        unlink_in_place(&mut self.doc, self.edit_stack.last_mut(), &full)?;
        if let Some(session) = self.sketch.as_mut() {
            session.driven.remove(&relation);
        }
        Ok(())
    }
}

fn not_in_place() -> String {
    "Names with @…Assembly or @…Part need the part edited in place in its assembly".to_owned()
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
        assert!(reaches_outside(typed) && !reaches_outside("\"Width\" * 2"));
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
        // An emptied field takes the name alone; a lone value or name is replaced.
        let mut text = String::new();
        insert_name(&mut text, "\"Gap\"");
        assert_eq!(text, "\"Gap\"");
        let mut text = "300".to_owned();
        insert_name(&mut text, "\"Gap@Case.Assembly\"");
        assert_eq!(text, "\"Gap@Case.Assembly\"");
        let mut text = "= \"Depth\"".to_owned();
        insert_name(&mut text, "\"Gap@Case.Assembly\"");
        assert_eq!(text, "= \"Gap@Case.Assembly\"");
    }
}

/// The whole cycle of linking a part to its assembly while editing it in place, as the
/// owner does it on the Hackintosh case: the part's variable comes from its linked
/// equations file, the assembly has a variable of its own, and the link must hold through
/// saving, finishing the edit, changing the assembly's variable and opening everything
/// again.
#[cfg(test)]
mod cycle {
    use std::path::{Path, PathBuf};

    use oxyde_assembly::Placement;
    use oxyde_geom::Point2;
    use oxyde_kernel::Mesh;
    use oxyde_model::{
        Expr, Extrude, ExtrudeOp, FeatureKind, LinkedValue, Sketch, SketchPlane, StandardPlane,
    };
    use oxyde_occt::OcctKernel;
    use oxyde_sketch::{Constraint, EntityId};

    use super::*;
    use crate::assembly::{AssemblyDoc, Loaded, Occurrence};

    /// A 40 × 20 plate whose depth is its linked variable `"Depth"` / 10, saved at `path`
    /// next to `Case.txt`.
    fn save_plate(path: &Path) {
        let mut part = Part::new();
        let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
        let lines = sketch.rectangle(Point2::ORIGIN, 40.0, 20.0);
        sketch
            .geometry
            .constrain(Constraint::Length(EntityId(lines[0]), 40.0));
        sketch
            .geometry
            .constrain(Constraint::Length(EntityId(lines[1]), 20.0));
        let s1 = part.add_feature(FeatureKind::Sketch(sketch));
        part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            10.0,
        )));
        let depth = LinkedValue {
            name: "Depth".to_owned(),
            expression: "300".to_owned(),
        };
        part.link_equations(Some("Case.txt".to_owned()), &[depth])
            .unwrap();
        let target = part.find_dimension("D1@Boss-Extrude1").unwrap();
        let expr = part.parse_expression("\"Depth\" / 10").unwrap();
        part.add_equation(target, expr).unwrap();
        let file = oxyde_format::PartFile {
            document_id: oxyde_format::new_document_id(),
            definition: part.definition(),
            properties: Default::default(),
        };
        oxyde_format::save_part(path, &file).unwrap();
    }

    fn open_assembly(path: &Path, kernel: &mut OcctKernel) -> AssemblyDoc {
        let file = oxyde_format::load_assembly(path).unwrap();
        let mut doc = AssemblyDoc::from_file(file, Some(path.to_path_buf()));
        assert!(doc.load_parts(kernel).is_empty());
        doc.note_stale_parts();
        doc.apply_equations(kernel);
        doc
    }

    fn loaded_part<'a>(doc: &'a AssemblyDoc, path: &Path) -> &'a Part {
        match doc.docs.get(path) {
            Some(Ok(Loaded::Part(p))) => &p.part,
            _ => panic!("{} is not loaded", path.display()),
        }
    }

    fn value(part: &Part, name: &str) -> f64 {
        part.value_of(name).unwrap_or(f64::NAN)
    }

    #[test]
    fn a_link_made_in_place_holds_through_saving_finishing_and_reopening() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("oxyde-in-place-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Case.txt"), "\"Depth\" = 300\n").unwrap();
        let plate = dir.join("Plate.oxyprt");
        save_plate(&plate);
        let asm_path = dir.join("Plates.oxyasm");
        let mut file = oxyde_format::AssemblyFile::new();
        let id = file.assembly.add_component(
            "Plate",
            "Plate.oxyprt".into(),
            String::new(),
            Placement::IDENTITY,
        );
        let width = file
            .assembly
            .parse_expression("300", &oxyde_assembly::TopLevelOnly)
            .unwrap();
        file.assembly
            .equations
            .add_global("linked width", width)
            .unwrap();
        oxyde_format::save_assembly(&asm_path, &file).unwrap();

        let mut kernel = OcctKernel::new();
        // Edit the plate in place: the assembly set aside, the part opened from its file.
        let mut parent = Suspended {
            asm: open_assembly(&asm_path, &mut kernel),
            component: Occurrence::top(id),
            component_name: "Plate<1>".to_owned(),
            context: Mesh::default(),
        };
        let mut doc = Document::from_file(
            oxyde_format::load_part(&plate).unwrap(),
            Some(plate.clone()),
        )
        .unwrap();
        doc.rebuild(&mut kernel).unwrap();
        assert_eq!(value(&doc.part, "D1@Boss-Extrude1"), 30.0);

        // Its Depth, from the file, linked to the assembly's variable; its height too.
        link_global_in_place(
            &mut doc,
            &mut parent,
            "Depth",
            "\"linked width@Plates.Assembly\" + 20",
        )
        .unwrap();
        let height = doc.part.find_dimension("D2@Sketch1").unwrap();
        link_dimension_in_place(
            &mut doc,
            &mut parent,
            height,
            "\"linked width@Plates.Assembly\" / 10",
        )
        .unwrap();
        doc.rebuild(&mut kernel).unwrap();
        assert_eq!(value(&doc.part, "Depth"), 320.0);
        assert_eq!(value(&doc.part, "D1@Boss-Extrude1"), 32.0);
        assert_eq!(
            link_text(&parent, "Depth").as_deref(),
            Some("\"linked width@Plates.Assembly\" + 20")
        );
        assert_eq!(parent.asm.assembly.equations.equations.len(), 2);
        assert!(parent.asm.modified);

        // Finish editing: the part saved, the assembly reading it again.
        oxyde_format::save_part(&plate, &doc.to_file()).unwrap();
        doc.release(&mut kernel);
        let mut asm = parent.asm;
        asm.reload_changed_with(&mut kernel, None);
        asm.refresh_linked_equations();
        asm.note_stale_parts();
        asm.apply_equations(&mut kernel);
        let results = &asm.equation_results.equations;
        assert!(results.iter().all(Result::is_ok), "{results:?}");
        let part = loaded_part(&asm, &plate);
        assert_eq!(value(part, "Depth"), 320.0);
        assert_eq!(value(part, "D2@Sketch1"), 30.0);
        assert!(part.driver("Depth").is_some() && part.driver("D2@Sketch1").is_some());

        // The assembly's variable changes: the part follows.
        let global = asm.assembly.equations.globals[0].id;
        let expr = asm
            .assembly
            .parse_expression("350", &oxyde_assembly::TopLevelOnly)
            .unwrap();
        asm.assembly
            .equations
            .set_global_expression(global, expr)
            .unwrap();
        asm.apply_equations(&mut kernel);
        let part = loaded_part(&asm, &plate);
        assert_eq!(value(part, "Depth"), 370.0);
        assert_eq!(value(part, "D1@Boss-Extrude1"), 37.0);
        assert_eq!(value(part, "D2@Sketch1"), 35.0);

        // Saved and opened again, everything is still linked.
        oxyde_format::save_assembly(&asm_path, &asm.to_file()).unwrap();
        let (saved, notes) = asm.save_changed_parts();
        assert_eq!((saved, notes), (1, Vec::<String>::new()));
        asm.release(&mut kernel);
        let asm = open_assembly(&asm_path, &mut kernel);
        assert_eq!(asm.assembly.equations.equations.len(), 2);
        let part = loaded_part(&asm, &plate);
        assert_eq!(value(part, "Depth"), 370.0);
        assert!(!asm.has_pending_parts());
        // The part alone keeps the assembly's value, not its file's 300.
        let alone =
            Part::from_definition(oxyde_format::load_part(&plate).unwrap().definition).unwrap();
        let depth = alone.equations().globals.iter().find(|g| g.name == "Depth");
        assert!(depth.is_some_and(|g| !g.linked && g.expr == Expr::Number(370.0)));
        assert!(alone.driver("Depth").is_some());
        let mut asm = asm;
        asm.release(&mut kernel);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Linked in place, then the edit finished without saving the part: the part read again
    /// still has its own equation on the dimension, and the assembly's link must win over
    /// it rather than be refused, which would look as if the link were forgotten.
    #[test]
    fn a_link_holds_when_the_part_is_not_saved() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("oxyde-in-place-unsaved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Case.txt"),
            "\"Depth\" = 300
",
        )
        .unwrap();
        let plate = dir.join("Plate.oxyprt");
        save_plate(&plate);
        let asm_path = dir.join("Plates.oxyasm");
        let mut file = oxyde_format::AssemblyFile::new();
        let id = file.assembly.add_component(
            "Plate",
            "Plate.oxyprt".into(),
            String::new(),
            Placement::IDENTITY,
        );
        let gap = file
            .assembly
            .parse_expression("12", &oxyde_assembly::TopLevelOnly)
            .unwrap();
        file.assembly.equations.add_global("Gap", gap).unwrap();
        oxyde_format::save_assembly(&asm_path, &file).unwrap();

        let mut kernel = OcctKernel::new();
        let mut parent = Suspended {
            asm: open_assembly(&asm_path, &mut kernel),
            component: Occurrence::top(id),
            component_name: "Plate<1>".to_owned(),
            context: Mesh::default(),
        };
        let mut doc = Document::from_file(
            oxyde_format::load_part(&plate).unwrap(),
            Some(plate.clone()),
        )
        .unwrap();
        doc.rebuild(&mut kernel).unwrap();
        // The depth, driven by the part's own equation, linked to the assembly's Gap.
        let depth = doc.part.find_dimension("D1@Boss-Extrude1").unwrap();
        link_dimension_in_place(&mut doc, &mut parent, depth, "\"Gap@Plates.Assembly\"").unwrap();
        doc.release(&mut kernel);

        // Finished without saving the part.
        let mut asm = parent.asm;
        asm.reload_changed_with(&mut kernel, None);
        asm.apply_equations(&mut kernel);
        let results = &asm.equation_results.equations;
        assert!(results.iter().all(Result::is_ok), "{results:?}");
        let part = loaded_part(&asm, &plate);
        assert_eq!(value(part, "D1@Boss-Extrude1"), 12.0);
        assert!(part.equations().equations.is_empty());
        assert!(asm.changed_parts.contains(&plate));
        oxyde_format::save_assembly(&asm_path, &asm.to_file()).unwrap();
        assert_eq!(asm.save_changed_parts().0, 1);
        asm.release(&mut kernel);
        let asm = open_assembly(&asm_path, &mut kernel);
        assert_eq!(value(loaded_part(&asm, &plate), "D1@Boss-Extrude1"), 12.0);
        let mut asm = asm;
        asm.release(&mut kernel);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
