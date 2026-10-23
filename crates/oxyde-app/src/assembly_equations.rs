//! The open assembly's equations, as SolidWorks has them: its global variables, and
//! equations driving its mate values and the dimensions and global variables of its parts.
//!
//! Equations are evaluated before every solve. A part an equation drives is changed in the
//! loaded copy at once, rebuilt and meshed again ([`AssemblyDoc::settle`]), and saved with
//! the assembly ([`AssemblyDoc::save_changed_parts`]). Every instance of a part shares its
//! file, so they all change together.
//!
//! An assembly linked to an equations file shares its variables with the parts linked to the
//! same file: changing one in the assembly writes the file, then refreshes those parts.
//!
//! Each part records what the assembly drives in it (`driven_by`, SolidWorks' external
//! references), written when the assembly saves it: opened on its own, the part shows those
//! values as driven. A value another assembly drives is refused.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use oxyde_assembly::{AsmDimension, ComponentId, ComponentTree, PartValues};
use oxyde_kernel::Kernel;
use oxyde_model::{DimensionUnit, LinkedValue, ModelError};

use crate::assembly::{AssemblyDoc, Loaded};

type Docs = HashMap<PathBuf, Result<Loaded, String>>;

/// The file `path` leads to: a top-level component, then down through sub-assemblies.
fn file_at(
    docs: &Docs,
    sources: &HashMap<ComponentId, PathBuf>,
    path: &[ComponentId],
) -> Option<PathBuf> {
    let (first, rest) = path.split_first()?;
    let mut file = sources.get(first)?;
    for id in rest {
        let Some(Ok(Loaded::Assembly(sub))) = docs.get(file) else {
            return None;
        };
        file = sub.sources.get(id)?;
    }
    Some(file.clone())
}

/// The loaded sub-assemblies, as the assembly's equations name what is inside them.
pub(crate) struct LoadedTree<'a> {
    docs: &'a Docs,
    sources: &'a HashMap<ComponentId, PathBuf>,
}

impl ComponentTree for LoadedTree<'_> {
    fn components(&self, path: &[ComponentId]) -> Option<Vec<(ComponentId, String)>> {
        let Some(Ok(Loaded::Assembly(sub))) =
            self.docs.get(&file_at(self.docs, self.sources, path)?)
        else {
            return None;
        };
        Some(
            sub.assembly
                .components
                .iter()
                .map(|c| (c.id, c.name.clone()))
                .collect(),
        )
    }
}

/// The loaded parts, as the assembly's equations read and drive them. Instances of a part
/// share one loaded file.
struct LoadedParts<'a> {
    docs: &'a mut Docs,
    sources: &'a HashMap<ComponentId, PathBuf>,
    /// The assembly's document id: the parts know it as the assembly driving them.
    assembly_id: &'a str,
    /// Files changed by this evaluation, to rebuild.
    changed: Vec<PathBuf>,
}

impl PartValues for LoadedParts<'_> {
    fn file(&self, path: &[ComponentId]) -> Option<PathBuf> {
        file_at(self.docs, self.sources, path)
    }

    fn value(&self, path: &[ComponentId], name: &str) -> Option<f64> {
        match self.docs.get(&file_at(self.docs, self.sources, path)?)? {
            Ok(Loaded::Part(p)) => p.part.value_of(name),
            _ => None,
        }
    }

    fn drive(&mut self, path: &[ComponentId], name: &str, value: f64) -> Result<f64, String> {
        let file = file_at(self.docs, self.sources, path)
            .ok_or_else(|| "the component's file is not loaded".to_owned())?;
        let Some(Ok(Loaded::Part(p))) = self.docs.get_mut(&file) else {
            return Err("the component is not a part".to_owned());
        };
        let (now, changed) = p.part.drive(name, value, self.assembly_id)?;
        if changed && !self.changed.contains(&file) {
            self.changed.push(file);
        }
        Ok(now)
    }
}

/// Rounds of rebuilding driven parts and evaluating again before giving up: an equation
/// may read a part value that the part's own equations derive from one the assembly drives.
const MAX_ROUNDS: usize = 3;

impl AssemblyDoc {
    /// What the equations' names see inside sub-assemblies.
    pub(crate) fn tree(&self) -> LoadedTree<'_> {
        LoadedTree {
            docs: &self.docs,
            sources: &self.sources,
        }
    }

    /// Evaluates the assembly's equations: driven mates take their values, and driven parts
    /// change in their loaded copy, waiting for [`AssemblyDoc::settle`] to rebuild them.
    pub fn evaluate_equations(&mut self) {
        let AssemblyDoc {
            assembly,
            docs,
            sources,
            document_id,
            ..
        } = self;
        if assembly.equations.is_empty() {
            self.equation_results = Default::default();
            self.record_driven();
            return;
        }
        assembly.resolve_equation_names(&LoadedTree { docs, sources });
        let mut parts = LoadedParts {
            docs,
            sources,
            assembly_id: document_id,
            changed: Vec::new(),
        };
        let results = assembly.evaluate_equations(&mut parts);
        let changed = parts.changed;
        self.equation_results = results;
        for file in changed {
            self.pending.insert(file.clone());
            self.changed_parts.insert(file);
            self.modified = true;
        }
        self.record_driven();
    }

    /// What the assembly's equations drive in each loaded part file, by name, leaving out
    /// what another assembly drives (refused).
    fn driven_names(&self) -> BTreeMap<PathBuf, BTreeSet<String>> {
        let mut out: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
        for equation in &self.assembly.equations.equations {
            let Some(AsmDimension::Part { path, name }) = &equation.target else {
                continue;
            };
            let Some(file) = file_at(&self.docs, &self.sources, path) else {
                continue;
            };
            let Some(Ok(Loaded::Part(p))) = self.docs.get(&file) else {
                continue;
            };
            if p.part
                .driver(name)
                .is_some_and(|d| d.assembly_id != self.document_id)
            {
                continue;
            }
            out.entry(file).or_default().insert(name.clone());
        }
        out
    }

    /// Records in every loaded part what the assembly drives in it, as SolidWorks records
    /// external references; a part whose record changes is saved with the assembly. An
    /// assembly not saved yet records nothing: it has no file for the parts to name.
    pub fn record_driven(&mut self) {
        let Some(assembly) = self.path.clone() else {
            return;
        };
        let names = self.driven_names();
        let none = BTreeSet::new();
        for (file, loaded) in &mut self.docs {
            let Ok(Loaded::Part(p)) = loaded else {
                continue;
            };
            let stored = oxyde_assembly::paths::stored(&assembly, file.parent());
            let these = names.get(file).unwrap_or(&none);
            if p.part.set_driven_by(&self.document_id, &stored, these) {
                self.changed_parts.insert(file.clone());
                self.modified = true;
            }
        }
    }

    /// After Save as, the assembly is a new document: what the old one drove in the parts is
    /// now driven by this one, as SolidWorks moves the external references to the new file.
    pub fn take_over_driven(&mut self, old_id: &str) {
        let none = BTreeSet::new();
        for (file, loaded) in &mut self.docs {
            let Ok(Loaded::Part(p)) = loaded else {
                continue;
            };
            if p.part.driven_by().iter().any(|d| d.assembly_id == old_id) {
                p.part.set_driven_by(old_id, "", &none);
                self.changed_parts.insert(file.clone());
            }
        }
        self.record_driven();
    }

    /// The value of `name` (a dimension `D1@Sketch1`, or a global variable) in the loaded
    /// part `path` leads to, in display units.
    pub(crate) fn part_value(&self, path: &[ComponentId], name: &str) -> Option<f64> {
        match self.docs.get(&file_at(&self.docs, &self.sources, path)?)? {
            Ok(Loaded::Part(p)) => p.part.value_of(name),
            _ => None,
        }
    }

    /// Whether parts changed by the equations wait to be rebuilt.
    pub fn has_pending_parts(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Rebuilds and meshes again the parts the equations changed, evaluates the equations
    /// again for what read them, and solves the mates.
    pub fn settle<K: Kernel>(&mut self, kernel: &mut K) {
        let mut rounds = 0;
        while !self.pending.is_empty() {
            for path in std::mem::take(&mut self.pending) {
                if let Some(Ok(Loaded::Part(p))) = self.docs.get_mut(&path) {
                    p.part.rebuild(kernel);
                    (p.mesh, p.bounds) =
                        crate::assembly::mesh_part(&p.part, kernel, self.mesh_fraction);
                }
                self.volumes.remove(&path);
            }
            rounds += 1;
            if rounds == MAX_ROUNDS {
                break;
            }
            self.evaluate_equations();
        }
        self.pending.clear();
        self.solve_mates();
    }

    /// Evaluates the equations, rebuilds the parts they drive and solves the mates: after
    /// the parts were loaded, or the equations changed.
    pub fn apply_equations<K: Kernel>(&mut self, kernel: &mut K) {
        self.evaluate_equations();
        self.settle(kernel);
    }

    /// Reads the assembly's linked equations file again, as opening the assembly does: a
    /// part edited meanwhile may have changed it. A file that cannot be read leaves the
    /// assembly's values as they are.
    pub fn refresh_linked_equations(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let mut assembly = self.assembly.clone();
        if oxyde_format::equations_file::refresh_assembly_linked_equations(&path, &mut assembly)
            .is_ok()
        {
            self.assembly = assembly;
        }
    }

    /// Takes the parts whose linked equations file changed them when they were read: their
    /// files are saved with the assembly.
    pub fn note_stale_parts(&mut self) {
        for (path, loaded) in &self.docs {
            if matches!(loaded, Ok(Loaded::Part(p)) if p.stale) {
                self.changed_parts.insert(path.clone());
            }
        }
    }

    /// Saves the part files the assembly changed, as SolidWorks saves the modified
    /// components with their assembly. A part being edited in another window, or changed
    /// on disk since it was read, is left alone and named in the messages returned, with
    /// the number of parts saved.
    pub fn save_changed_parts(&mut self) -> (usize, Vec<String>) {
        let mut saved = 0;
        let mut notes = Vec::new();
        for path in std::mem::take(&mut self.changed_parts) {
            match self.save_part_file(&path) {
                Ok(true) => saved += 1,
                Ok(false) => {}
                Err(note) => {
                    notes.push(note);
                    self.changed_parts.insert(path);
                }
            }
        }
        (saved, notes)
    }

    /// Saves one part file the assembly changed, if it did: before the part is edited in
    /// place, so that it opens as the assembly shows it.
    pub fn save_changed_part(&mut self, path: &Path) -> Result<(), String> {
        if self.changed_parts.contains(path) {
            self.save_part_file(path)?;
            self.changed_parts.remove(path);
        }
        Ok(())
    }

    /// Writes a loaded part to its file. `Ok(false)` when it is not loaded; an error says
    /// why it was left alone.
    fn save_part_file(&mut self, path: &Path) -> Result<bool, String> {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let Some(Ok(Loaded::Part(p))) = self.docs.get(path) else {
            return Ok(false);
        };
        if let Some(holder) = crate::lockfile::held_elsewhere(path) {
            return Err(format!("{name} is being edited by {}", holder.describe()));
        }
        if self.stamps.get(path).copied().flatten() != crate::assembly::modified(path) {
            return Err(format!("{name} changed on disk: reload the parts (Ctrl+B)"));
        }
        let file = oxyde_format::PartFile {
            document_id: p.document_id.clone(),
            definition: p.part.definition(),
            properties: p.properties.clone(),
        };
        oxyde_format::save_part(path, &file)
            .map_err(|e| format!("{name} could not be saved: {e}"))?;
        self.stamps
            .insert(path.to_path_buf(), crate::assembly::modified(path));
        if let Some(Ok(Loaded::Part(p))) = self.docs.get_mut(path) {
            p.stale = false;
        }
        Ok(true)
    }

    /// Everything an equation of the assembly can drive, by full name: the mate values,
    /// then the dimensions and global variables of every part, through sub-assemblies.
    /// Dimensions the parts' own equations drive and variables of their linked files are
    /// left out: the part's own value would win.
    pub fn equation_targets(&self) -> Vec<(AsmDimension, String)> {
        let mut out: Vec<(AsmDimension, String)> = self
            .assembly
            .mate_dimensions()
            .into_iter()
            .filter(|d| !d.driven)
            .map(|d| (AsmDimension::Mate(d.mate), d.full_name))
            .collect();
        let tree = self.tree();
        let mut paths: Vec<Vec<ComponentId>> = self
            .assembly
            .components
            .iter()
            .map(|c| vec![c.id])
            .collect();
        while let Some(path) = paths.pop() {
            let Some(file) = file_at(&self.docs, &self.sources, &path) else {
                continue;
            };
            match self.docs.get(&file) {
                Some(Ok(Loaded::Part(p))) => {
                    let equations = p.part.equations();
                    let names = p
                        .part
                        .dimensions()
                        .into_iter()
                        .filter(|d| !d.driven)
                        .map(|d| d.full_name)
                        .chain(
                            equations
                                .globals
                                .iter()
                                .filter(|g| !(g.linked && equations.linked_file.is_some()))
                                .map(|g| g.name.clone()),
                        );
                    for name in names {
                        // Driven by another assembly: refused until released in the part.
                        if p.part
                            .driver(&name)
                            .is_some_and(|d| d.assembly_id != self.document_id)
                        {
                            continue;
                        }
                        let target = AsmDimension::Part {
                            path: path.clone(),
                            name,
                        };
                        if self.assembly.equations.is_driven(&target) {
                            continue;
                        }
                        if let Some(full) = self.assembly.dimension_name(&target, &tree) {
                            out.push((target, full));
                        }
                    }
                }
                Some(Ok(Loaded::Assembly(sub))) if path.len() < 16 => {
                    for c in sub.assembly.components.iter().rev() {
                        let mut inner = path.clone();
                        inner.push(c.id);
                        paths.push(inner);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The unit of what an equation drives or reads: mm or degrees for a mate, the part's
    /// dimension's unit, or none for a global variable.
    pub fn target_unit(&self, target: &AsmDimension) -> Option<DimensionUnit> {
        match target {
            AsmDimension::Mate(id) => self.assembly.mate(*id).map(|m| match m.kind {
                oxyde_assembly::MateKind::Angle(_) => DimensionUnit::Angle,
                _ => DimensionUnit::Length,
            }),
            AsmDimension::Part { path, name } => {
                let file = file_at(&self.docs, &self.sources, path)?;
                let Some(Ok(Loaded::Part(p))) = self.docs.get(&file) else {
                    return None;
                };
                let reference = p.part.find_dimension(name)?;
                p.part.dimension(reference).map(|d| d.unit)
            }
        }
    }

    /// Where the assembly's linked equations file is on disk.
    pub fn linked_file_path(&self) -> Result<PathBuf, ModelError> {
        let file = self
            .assembly
            .equations
            .linked_file
            .clone()
            .ok_or_else(|| ModelError::Expression("the assembly is not linked".to_owned()))?;
        if Path::new(&file).is_absolute() {
            return Ok(file.into());
        }
        let assembly = self.path.as_deref().ok_or_else(|| {
            ModelError::Expression(
                "save the assembly first: its equations file is next to it".into(),
            )
        })?;
        Ok(oxyde_format::equations_file::linked_file_path(
            assembly, &file,
        ))
    }

    /// Gives the parts linked to the equations file `file` its new `values`, at every depth;
    /// the parts that change are rebuilt by [`AssemblyDoc::settle`] and saved with the
    /// assembly.
    pub fn refresh_linked_parts(&mut self, file: &Path, values: &[LinkedValue]) {
        let same = |a: &Path, b: &Path| {
            let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            canonical(a) == canonical(b)
        };
        let mut changed = Vec::new();
        for (path, loaded) in &mut self.docs {
            let Ok(Loaded::Part(p)) = loaded else {
                continue;
            };
            let Some(linked) = p.part.equations().linked_file.clone() else {
                continue;
            };
            if !same(
                &oxyde_format::equations_file::linked_file_path(path, &linked),
                file,
            ) {
                continue;
            }
            let before = p.part.definition();
            if p.part.apply_linked_values(values).is_ok() && p.part.definition() != before {
                changed.push(path.clone());
            }
        }
        for path in changed {
            self.pending.insert(path.clone());
            self.changed_parts.insert(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use oxyde_assembly::{MateKind, MateRef, MateTarget, Placement, TopLevelOnly};
    use oxyde_geom::{Point2, Point3, Vec3};
    use oxyde_model::{Extrude, ExtrudeOp, FeatureKind, Part, Sketch, SketchPlane, StandardPlane};
    use oxyde_occt::OcctKernel;
    use oxyde_sketch::{Constraint, EntityId};

    use super::*;

    /// A 40 × 20 plate, 10 thick, on the Front plane, saved at `path`.
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
        let file = oxyde_format::PartFile {
            document_id: oxyde_format::new_document_id(),
            definition: part.definition(),
            properties: Default::default(),
        };
        oxyde_format::save_part(path, &file).unwrap();
    }

    fn depth(part: &Part) -> f64 {
        let depth = part.find_dimension("D1@Boss-Extrude1").unwrap();
        part.dimension(depth).unwrap().value
    }

    #[test]
    fn driven_parts_are_rebuilt_and_saved_with_the_assembly() {
        let dir = std::env::temp_dir().join(format!("oxyde-app-asm-eq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plate = dir.join("Plate.oxyprt");
        save_plate(&plate);
        let mut file = oxyde_format::AssemblyFile::new();
        let asm = &mut file.assembly;
        let a = asm.add_component(
            "Plate",
            "Plate.oxyprt".into(),
            String::new(),
            Placement::IDENTITY,
        );
        let b = asm.add_component(
            "Plate",
            "Plate.oxyprt".into(),
            String::new(),
            Placement::IDENTITY.moved(Vec3::ZERO, Point3::ORIGIN, Vec3::new(0.0, 0.0, 30.0)),
        );
        let front = |c| MateRef::new(c, MateTarget::Plane(StandardPlane::Front));
        let distance = asm.add_mate(MateKind::Distance(25.0), front(a), front(b), true);
        let gap = asm.parse_expression("4", &TopLevelOnly).unwrap();
        asm.equations.add_global("Gap", gap).unwrap();
        for (target, text) in [
            ("D1@Distance1", "\"Gap\" * 3"),
            ("D1@Boss-Extrude1@Plate<2>.Part", "\"Gap\" + 1"),
        ] {
            let target = asm.find_dimension(target, &TopLevelOnly).unwrap();
            let name = asm.dimension_name(&target, &TopLevelOnly).unwrap();
            let expr = asm.parse_expression(text, &TopLevelOnly).unwrap();
            asm.equations.add_equation(target, name, expr).unwrap();
        }
        let path = dir.join("Plates.oxyasm");
        oxyde_format::save_assembly(&path, &file).unwrap();

        let file = oxyde_format::load_assembly(&path).unwrap();
        let mut doc = AssemblyDoc::from_file(file, Some(path));
        let mut kernel = OcctKernel::new();
        assert!(doc.load_parts(&mut kernel).is_empty());
        doc.apply_equations(&mut kernel);
        assert_eq!(doc.equation_results.equations, vec![Ok(12.0), Ok(5.0)]);
        assert!(doc.assembly.is_mate_driven(distance));
        let Some(Ok(Loaded::Part(p))) = doc.docs.get(&plate) else {
            panic!("the plate is not loaded");
        };
        assert_eq!(depth(&p.part), 5.0);
        // Rebuilt and meshed again: 5 thick.
        let (lo, hi) = p.bounds.unwrap();
        assert!((hi.z - lo.z - 5.0).abs() < 1e-3, "{lo:?} {hi:?}");
        assert!(doc.changed_parts.contains(&plate) && doc.modified);
        let z = |doc: &AssemblyDoc, i: usize| doc.assembly.components[i].placement.translation.z;
        assert!(((z(&doc, 1) - z(&doc, 0)).abs() - 12.0).abs() < 1e-6);
        // The targets offered for a new equation leave out the driven ones.
        let targets: Vec<String> = doc.equation_targets().into_iter().map(|(_, n)| n).collect();
        assert!(
            targets.contains(&"D1@Sketch1@Plate<1>.Part".to_owned()),
            "{targets:?}"
        );
        assert!(!targets.contains(&"D1@Distance1".to_owned()), "{targets:?}");
        assert!(!targets.contains(&"D1@Boss-Extrude1@Plate<2>.Part".to_owned()));

        let (saved, notes) = doc.save_changed_parts();
        assert_eq!((saved, notes), (1, Vec::<String>::new()));
        assert!(doc.changed_parts.is_empty());
        let reopened =
            Part::from_definition(oxyde_format::load_part(&plate).unwrap().definition).unwrap();
        assert_eq!(depth(&reopened), 5.0);
        // The part records that the assembly drives it, as SolidWorks' external references.
        let driver = reopened.driver("D1@Boss-Extrude1").unwrap();
        assert_eq!(driver.assembly, "Plates.oxyasm");
        assert_eq!(driver.assembly_id, doc.document_id);

        // Another assembly driving the same dimension is refused.
        let mut other = AssemblyDoc::from_file(
            oxyde_format::AssemblyFile::new(),
            Some(dir.join("Other.oxyasm")),
        );
        other.assembly = doc.assembly.clone();
        other.load_parts(&mut kernel);
        other.apply_equations(&mut kernel);
        let refused = other.equation_results.equations[1].as_ref().unwrap_err();
        assert!(refused.contains("Plates"), "{refused}");
        other.release(&mut kernel);

        // Evaluated again, nothing is left to rebuild or save.
        doc.solve();
        assert!(!doc.has_pending_parts() && doc.changed_parts.is_empty());
        doc.release(&mut kernel);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
