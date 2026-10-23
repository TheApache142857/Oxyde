//! External references, as SolidWorks has them: values of a part that an assembly's
//! equations drive. The assembly records them in the part when it saves it
//! ([`PartDefinition::set_driven_by`]), so that the part opened on its own shows those values
//! as driven, and refuses to change them until they are released ([`Part::release_driven`]).
//!
//! An entry names the value as the part's own equations do (`D1@Sketch1`, or a global
//! variable's name) and the assembly by its file and document id. One assembly at a time
//! drives a value: [`Part::drive`] refuses a second one.

use std::collections::BTreeSet;

use crate::dimensions::DimensionRef;
use crate::error::ModelError;
use crate::expression::GlobalId;
use crate::feature::{FeatureId, FeatureKind};
use crate::part::{Part, PartDefinition};

/// A value of the part driven by an assembly's equation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DrivenBy {
    /// A dimension's full name (`D1@Sketch1`) or a global variable's name (`Thickness`).
    pub name: String,
    /// The assembly's file, relative to the part's folder with `/`, or absolute.
    pub assembly: String,
    /// The assembly's document id, to know it again after it moved.
    pub assembly_id: String,
}

impl DrivenBy {
    /// The assembly's name, for messages: its file name without the extension.
    pub fn assembly_name(&self) -> &str {
        Self::name_of(&self.assembly)
    }

    /// The name of the assembly stored as `assembly` (a path): its file name without the
    /// extension.
    pub fn name_of(assembly: &str) -> &str {
        let file = assembly.rsplit(['/', '\\']).next().unwrap_or(assembly);
        file.rsplit_once('.').map_or(file, |(stem, _)| stem)
    }
}

/// Sorts the entries and drops repeated ones, so that one set is always written the same way.
pub(crate) fn tidy(entries: &mut Vec<DrivenBy>) {
    entries.sort();
    entries.dedup();
}

/// Replaces the entries of the assembly `assembly_id` with `names`; whether they changed.
fn replace(
    entries: &mut Vec<DrivenBy>,
    assembly_id: &str,
    assembly: &str,
    names: &BTreeSet<String>,
) -> bool {
    let mut new: Vec<DrivenBy> = entries
        .iter()
        .filter(|d| d.assembly_id != assembly_id)
        .cloned()
        .collect();
    new.extend(names.iter().map(|name| DrivenBy {
        name: name.clone(),
        assembly: assembly.to_owned(),
        assembly_id: assembly_id.to_owned(),
    }));
    tidy(&mut new);
    let changed = new != *entries;
    *entries = new;
    changed
}

impl PartDefinition {
    /// Records that the assembly `assembly_id`, whose file is `assembly` as seen from the
    /// part's folder, drives `names` in the part, and nothing else: its other entries go.
    /// Returns whether the entries changed.
    pub fn set_driven_by(
        &mut self,
        assembly_id: &str,
        assembly: &str,
        names: &BTreeSet<String>,
    ) -> bool {
        replace(&mut self.driven_by, assembly_id, assembly, names)
    }
}

impl Part {
    /// The values assemblies drive in the part.
    pub fn driven_by(&self) -> &[DrivenBy] {
        &self.driven_by
    }

    /// The assembly driving `name` (a dimension's full name or a global variable's), if one
    /// does.
    pub fn driver(&self, name: &str) -> Option<&DrivenBy> {
        let name = name.trim().trim_matches('"');
        self.driven_by.iter().find(|d| d.name == name)
    }

    /// [`PartDefinition::set_driven_by`] on the part.
    pub fn set_driven_by(
        &mut self,
        assembly_id: &str,
        assembly: &str,
        names: &BTreeSet<String>,
    ) -> bool {
        replace(&mut self.driven_by, assembly_id, assembly, names)
    }

    /// Releases `name` from the assembly driving it, as SolidWorks breaks an external
    /// reference: it is the part's own again. The assembly drives it again, and records it,
    /// when it is next opened and saved, unless its equation is deleted there.
    pub fn release_driven(&mut self, name: &str) -> Result<(), ModelError> {
        let name = name.trim().trim_matches('"');
        let before = self.driven_by.len();
        self.driven_by.retain(|d| d.name != name);
        if self.driven_by.len() == before {
            return Err(ModelError::Expression(format!(
                "\"{name}\" is not driven by an assembly"
            )));
        }
        Ok(())
    }

    /// Refuses a change to `name` when an assembly drives it.
    pub(crate) fn check_not_driven_by_assembly(&self, name: &str) -> Result<(), ModelError> {
        match self.driver(name) {
            Some(d) => Err(ModelError::Expression(format!(
                "\"{}\" is driven by {} (an assembly equation): release it first",
                d.name,
                d.assembly_name()
            ))),
            None => Ok(()),
        }
    }

    /// Refuses a change to a global variable an assembly drives.
    pub(crate) fn check_global_not_driven(&self, id: GlobalId) -> Result<(), ModelError> {
        match self.equations.globals.iter().find(|g| g.id == id) {
            Some(g) => self.check_not_driven_by_assembly(&g.name),
            None => Ok(()),
        }
    }

    /// The dimensions of feature `id` that an assembly drives, with their values, to check
    /// that an edit of the feature leaves them alone.
    pub(crate) fn assembly_driven_values(&self, id: FeatureId) -> Vec<(DimensionRef, String, f64)> {
        if self.driven_by.is_empty() {
            return Vec::new();
        }
        self.dimensions()
            .into_iter()
            .filter(|d| d.reference.feature == id && d.driven_by.is_some())
            .map(|d| (d.reference, d.full_name, d.value))
            .collect()
    }

    /// Puts `old` back as feature `id`'s definition, and refuses the edit, when the edit just
    /// made changed a dimension an assembly drives (`held`, from
    /// [`Part::assembly_driven_values`]).
    pub(crate) fn keep_assembly_driven_values(
        &mut self,
        id: FeatureId,
        old: FeatureKind,
        held: &[(DimensionRef, String, f64)],
    ) -> Result<(), ModelError> {
        for (reference, name, value) in held {
            let now = self.dimension(*reference).map(|d| d.value);
            if now.is_none_or(|v| (v - value).abs() > 1e-12 * value.abs().max(1.0)) {
                let refused = self.check_not_driven_by_assembly(name);
                if let Some(feature) = self.feature_mut_quietly(id) {
                    feature.kind = old;
                    feature.tidy_parameter_names();
                }
                return refused;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use oxyde_geom::Point2;
    use oxyde_sketch::{Constraint, EntityId};

    use super::*;
    use crate::expression::Expr;
    use crate::feature::{Extrude, ExtrudeExtent, ExtrudeOp, Sketch, SketchPlane, StandardPlane};

    /// Sketch1 (40 × 20 with both sides dimensioned), Boss-Extrude1 10 deep, and a global
    /// variable "Thickness".
    fn plate() -> (Part, FeatureId) {
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
        let boss = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            10.0,
        )));
        part.add_global("Thickness", Expr::Number(3.0)).unwrap();
        (part, boss)
    }

    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn values_an_assembly_drives_are_marked_and_refuse_edits() {
        let (mut part, boss) = plate();
        assert!(part.set_driven_by(
            "asm-a",
            "../Case.oxyasm",
            &names(&["D1@Boss-Extrude1", "Thickness"])
        ));
        // The same record again changes nothing.
        assert!(!part.set_driven_by(
            "asm-a",
            "../Case.oxyasm",
            &names(&["Thickness", "D1@Boss-Extrude1"])
        ));
        let depth = part.find_dimension("D1@Boss-Extrude1").unwrap();
        let d = part.dimension(depth).unwrap();
        assert!(d.driven && part.is_driven(depth));
        assert_eq!(d.driven_by.as_deref(), Some("../Case.oxyasm"));
        assert!(part.driver("D1@Sketch1").is_none());

        // The part refuses to change them.
        let thickness = part.equations().globals[0].id;
        let refused = part
            .set_global_expression(thickness, Expr::Number(4.0))
            .unwrap_err();
        assert!(refused.to_string().contains("Case"), "{refused}");
        assert!(part.rename_global(thickness, "T").is_err());
        assert!(part.remove_global(thickness).is_err());
        assert!(part.add_equation(depth, Expr::Number(5.0)).is_err());
        let mut deeper = part.feature(boss).unwrap().kind().clone();
        if let FeatureKind::Extrude(e) = &mut deeper {
            e.extent = ExtrudeExtent::Blind { depth: 25.0 };
        }
        assert!(part.replace_feature(boss, deeper).is_err());
        assert_eq!(part.dimension(depth).unwrap().value, 10.0);

        // The assembly that drives them may; another one may not.
        assert_eq!(
            part.drive("D1@Boss-Extrude1", 12.0, "asm-a"),
            Ok((12.0, true))
        );
        // Over the part's own equation, the assembly's value wins: the equation goes.
        let width = part.find_dimension("D1@Sketch1").unwrap();
        part.equations
            .add_equation(width, "D1@Sketch1".to_owned(), Expr::Number(40.0))
            .unwrap();
        assert_eq!(part.drive("D1@Sketch1", 40.0, "asm-a"), Ok((40.0, true)));
        assert!(part.equations().equations.is_empty());
        assert_eq!(part.drive("Thickness", 4.0, "asm-a"), Ok((4.0, true)));
        let refused = part.drive("D1@Boss-Extrude1", 15.0, "asm-b").unwrap_err();
        assert!(
            refused.contains("Case") && refused.contains("release"),
            "{refused}"
        );
        assert_eq!(part.drive("D1@Sketch1", 50.0, "asm-b"), Ok((50.0, true)));

        // Released, the value is the part's own again.
        part.release_driven("D1@Boss-Extrude1").unwrap();
        assert!(!part.is_driven(depth));
        assert!(part.release_driven("D1@Boss-Extrude1").is_err());
        assert_eq!(part.driven_by().len(), 1);
        // The assembly no longer driving anything leaves no entry.
        assert!(part.set_driven_by("asm-a", "../Case.oxyasm", &BTreeSet::new()));
        assert!(part.driven_by().is_empty());
    }

    #[test]
    fn a_linked_variable_an_assembly_drives_is_no_longer_read_from_the_file() {
        let (mut part, _) = plate();
        let linked = |value: &str| {
            [crate::equations::LinkedValue {
                name: "Depth".to_owned(),
                expression: value.to_owned(),
            }]
        };
        part.link_equations(Some("Case.txt".to_owned()), &linked("300"))
            .unwrap();
        assert_eq!(part.drive("Depth", 320.0, "asm"), Ok((320.0, true)));
        part.set_driven_by("asm", "Case.oxyasm", &names(&["Depth"]));
        // The file changes: the variable keeps the assembly's value, not linked any more.
        part.apply_linked_values(&linked("280")).unwrap();
        let depth = part.equations().globals.iter().find(|g| g.name == "Depth");
        assert!(depth.is_some_and(|g| !g.linked && g.expr == Expr::Number(320.0)));
        // Released, the file gives it again.
        part.release_driven("Depth").unwrap();
        part.apply_linked_values(&linked("280")).unwrap();
        let depth = part.equations().globals.iter().find(|g| g.name == "Depth");
        assert!(depth.is_some_and(|g| g.linked && g.expr == Expr::Number(280.0)));
    }

    #[test]
    fn records_survive_the_definition_sorted() {
        let (mut part, _) = plate();
        part.set_driven_by("b", "B.oxyasm", &names(&["Thickness"]));
        part.set_driven_by("a", "A.oxyasm", &names(&["D1@Sketch1"]));
        let mut definition = part.definition();
        definition.driven_by.reverse();
        definition.driven_by.push(definition.driven_by[0].clone());
        let part = Part::from_definition(definition).unwrap();
        let listed: Vec<_> = part.driven_by().iter().map(|d| d.name.as_str()).collect();
        assert_eq!(listed, ["D1@Sketch1", "Thickness"]);
        assert_eq!(
            DrivenBy::name_of("../cases/Hackintosh case.oxyasm"),
            "Hackintosh case"
        );
    }
}
