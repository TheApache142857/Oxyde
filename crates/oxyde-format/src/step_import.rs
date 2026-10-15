//! Turning a STEP assembly into Oxyde documents: one part file per distinct part, holding
//! an "Imported" feature with that part's solids, and one assembly file per assembly,
//! placing them where the STEP file had them.
//!
//! The geometry is read by the kernel ([`oxyde_kernel::Kernel::read_step_assembly`]); this
//! module only decides the files: their names, their contents and the order to write them.
//! All files go into one folder, so components refer to their files by bare name.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use oxyde_assembly::Placement;
use oxyde_kernel::{StepAssembly, StepNodeContent};
use oxyde_model::{FeatureKind, FileData, Import, Part};

use crate::{
    ASSEMBLY_EXTENSION, AssemblyFile, FormatError, PART_EXTENSION, PartFile, save_assembly,
    save_part,
};

/// A document to create.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportedDocument {
    Part(PartFile),
    Assembly(AssemblyFile),
}

/// A file of the import: its name in the target folder and its content.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedFile {
    /// File name with extension, e.g. `Housing.oxyprt`.
    pub file_name: String,
    /// Product name in the STEP file (may be empty).
    pub product: String,
    pub document: ImportedDocument,
}

/// The files of an import, every file after the ones it refers to: the last one is the
/// top-level document (an assembly, or a part when the STEP file holds a single part).
#[derive(Debug, Clone, PartialEq)]
pub struct ImportPlan {
    pub files: Vec<ImportedFile>,
}

impl ImportPlan {
    pub fn top(&self) -> &ImportedFile {
        self.files.last().expect("a plan has at least one file")
    }

    pub fn part_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| matches!(f.document, ImportedDocument::Part(_)))
            .count()
    }

    pub fn assembly_count(&self) -> usize {
        self.files.len() - self.part_count()
    }

    /// The files of the plan that already exist in `dir`.
    pub fn existing_files(&self, dir: &Path) -> Vec<PathBuf> {
        self.files
            .iter()
            .map(|f| dir.join(&f.file_name))
            .filter(|p| p.exists())
            .collect()
    }

    /// Writes every file into `dir` (created if needed), overwriting files of the same
    /// name. Returns the path of the top-level document.
    pub fn write(&self, dir: &Path) -> Result<PathBuf, FormatError> {
        std::fs::create_dir_all(dir)?;
        for file in &self.files {
            let path = dir.join(&file.file_name);
            match &file.document {
                ImportedDocument::Part(part) => save_part(&path, part)?,
                ImportedDocument::Assembly(assembly) => save_assembly(&path, assembly)?,
            }
        }
        Ok(dir.join(&self.top().file_name))
    }
}

/// Why a STEP assembly cannot be imported.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanError {
    /// No part with a solid anywhere in the file.
    NoSolids,
    /// The structure refers to a node that does not exist, or contains itself.
    InvalidStructure(String),
    /// An instance is mirrored or scaled (the kernel should have made it a part).
    NotRigid { instance: String },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::NoSolids => write!(f, "the STEP file has no solid body"),
            PlanError::InvalidStructure(why) => write!(f, "invalid assembly structure: {why}"),
            PlanError::NotRigid { instance } => {
                write!(f, "instance {instance:?} is mirrored or scaled")
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// Decides the files for `tree`, read from the STEP file named `source` (e.g.
/// `Gearbox.step`). Unnamed products are named after the source file (the top level) or
/// `Part1`, `Assembly1`…; names are made valid and unique file names (case-insensitively,
/// for Windows): `Bolt`, `Bolt (2)`. Assemblies that hold no solid are left out.
pub fn plan_step_import(tree: &StepAssembly, source: &str) -> Result<ImportPlan, PlanError> {
    let count = tree.nodes.len();
    if tree.root >= count {
        return Err(PlanError::InvalidStructure(format!(
            "root {} of {count} nodes",
            tree.root
        )));
    }
    // Children before parents, from the root; nodes without solids are dropped.
    let mut order = Vec::new();
    let mut state = vec![Visit::New; count];
    if !visit(tree, tree.root, &mut state, &mut order)? {
        return Err(PlanError::NoSolids);
    }

    let source_stem = Path::new(source)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut taken = HashSet::new();
    let mut stems = vec![String::new(); count];
    let (mut parts, mut assemblies) = (0, 0);
    // The top level first, so it gets its own name rather than `Name (2)`; then the rest
    // in file order.
    let root_first =
        std::iter::once(tree.root).chain(order.iter().copied().filter(|&n| n != tree.root));
    for node in root_first {
        let n = &tree.nodes[node];
        let mut base = sanitize(&n.name);
        if base.is_empty() && node == tree.root {
            base = sanitize(&source_stem);
        }
        if base.is_empty() {
            base = if matches!(n.content, StepNodeContent::Part { .. }) {
                parts += 1;
                format!("Part{parts}")
            } else {
                assemblies += 1;
                format!("Assembly{assemblies}")
            };
        }
        stems[node] = unique(&base, &mut taken);
    }

    let mut files: Vec<ImportedFile> = Vec::with_capacity(order.len());
    let mut file_of = vec![usize::MAX; count];
    for &node in &order {
        let n = &tree.nodes[node];
        let stem = &stems[node];
        let (file_name, document) = match &n.content {
            StepNodeContent::Part { step } => {
                let mut part = Part::new();
                part.add_feature(FeatureKind::Import(Import {
                    file_name: source.to_owned(),
                    data: FileData::new(step.clone()),
                }));
                let mut file = PartFile::new();
                file.definition = part.definition();
                file.properties.part_number = n.name.trim().to_owned();
                (
                    format!("{stem}.{PART_EXTENSION}"),
                    ImportedDocument::Part(file),
                )
            }
            StepNodeContent::Assembly { instances } => {
                let mut file = AssemblyFile::new();
                file.properties.part_number = n.name.trim().to_owned();
                for instance in instances {
                    let child = file_of[instance.node];
                    if child == usize::MAX {
                        continue; // no solid in it
                    }
                    let placement =
                        Placement::from_transform(&instance.transform).ok_or_else(|| {
                            PlanError::NotRigid {
                                instance: instance.name.clone(),
                            }
                        })?;
                    let target = &files[child];
                    file.assembly.add_component(
                        &stems[instance.node],
                        target.file_name.clone(),
                        document_id(&target.document).to_owned(),
                        placement,
                    );
                }
                (
                    format!("{stem}.{ASSEMBLY_EXTENSION}"),
                    ImportedDocument::Assembly(file),
                )
            }
        };
        file_of[node] = files.len();
        files.push(ImportedFile {
            file_name,
            product: n.name.clone(),
            document,
        });
    }
    Ok(ImportPlan { files })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Visit {
    New,
    InProgress,
    /// Visited; whether it holds a solid.
    Done(bool),
}

/// Depth-first, children first. Returns whether `node` holds a solid.
fn visit(
    tree: &StepAssembly,
    node: usize,
    state: &mut [Visit],
    order: &mut Vec<usize>,
) -> Result<bool, PlanError> {
    match state[node] {
        Visit::Done(solid) => return Ok(solid),
        Visit::InProgress => {
            return Err(PlanError::InvalidStructure(format!(
                "{:?} contains itself",
                tree.nodes[node].name
            )));
        }
        Visit::New => {}
    }
    state[node] = Visit::InProgress;
    let solid = match &tree.nodes[node].content {
        StepNodeContent::Part { step } => !step.is_empty(),
        StepNodeContent::Assembly { instances } => {
            let mut any = false;
            for instance in instances {
                if instance.node >= tree.nodes.len() {
                    return Err(PlanError::InvalidStructure(format!(
                        "instance {:?} refers to node {}",
                        instance.name, instance.node
                    )));
                }
                any |= visit(tree, instance.node, state, order)?;
            }
            any
        }
    };
    state[node] = Visit::Done(solid);
    if solid {
        order.push(node);
    }
    Ok(solid)
}

fn document_id(document: &ImportedDocument) -> &str {
    match document {
        ImportedDocument::Part(p) => &p.document_id,
        ImportedDocument::Assembly(a) => &a.document_id,
    }
}

/// A name usable as a file name on Windows and elsewhere: no reserved characters or
/// names, no trailing dots or spaces, at most 100 characters.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(100)
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']).trim_start().to_owned();
    let device = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((device.starts_with("COM") || device.starts_with("LPT"))
            && device.len() == 4
            && device.as_bytes()[3].is_ascii_digit());
    if reserved {
        format!("{cleaned}_")
    } else {
        cleaned
    }
}

/// `base`, or `base (2)`, `base (3)`… whichever is not taken yet (ignoring case).
fn unique(base: &str, taken: &mut HashSet<String>) -> String {
    let name = (1..)
        .map(|n| {
            if n == 1 {
                base.to_owned()
            } else {
                format!("{base} ({n})")
            }
        })
        .find(|name| !taken.contains(&name.to_lowercase()))
        .expect("some suffix is free");
    taken.insert(name.to_lowercase());
    name
}

#[cfg(test)]
mod tests {
    use oxyde_geom::{Transform, Vec3};
    use oxyde_kernel::{StepInstance, StepNode};

    use super::*;
    use crate::{load_assembly, load_part};

    fn part(name: &str) -> StepNode {
        StepNode {
            name: name.to_owned(),
            content: StepNodeContent::Part {
                step: format!("ISO-10303-21; {name}").into_bytes(),
            },
        }
    }

    fn assembly(name: &str, instances: &[(usize, Vec3)]) -> StepNode {
        StepNode {
            name: name.to_owned(),
            content: StepNodeContent::Assembly {
                instances: instances
                    .iter()
                    .map(|&(node, at)| StepInstance {
                        node,
                        name: String::new(),
                        transform: Transform::translation(at),
                    })
                    .collect(),
            },
        }
    }

    /// Gearbox: Housing, two screws, and a sub-assembly holding a shaft and a gear.
    fn gearbox() -> StepAssembly {
        StepAssembly {
            nodes: vec![
                assembly(
                    "Gearbox",
                    &[
                        (1, Vec3::ZERO),
                        (2, Vec3::new(10.0, 0.0, 0.0)),
                        (2, Vec3::new(-10.0, 0.0, 0.0)),
                        (3, Vec3::new(0.0, 0.0, 5.0)),
                    ],
                ),
                part("Housing"),
                part("M6x20 Screw"),
                assembly("Input stage", &[(4, Vec3::ZERO), (5, Vec3::ZERO)]),
                part("Shaft"),
                part("Gear Z20"),
            ],
            root: 0,
        }
    }

    fn names(plan: &ImportPlan) -> Vec<&str> {
        plan.files.iter().map(|f| f.file_name.as_str()).collect()
    }

    fn as_assembly(file: &ImportedFile) -> &AssemblyFile {
        match &file.document {
            ImportedDocument::Assembly(a) => a,
            ImportedDocument::Part(_) => panic!("{} is a part", file.file_name),
        }
    }

    #[test]
    fn one_file_per_distinct_part_and_assembly_children_first() {
        let plan = plan_step_import(&gearbox(), "gearbox.step").unwrap();
        assert_eq!(
            names(&plan),
            [
                "Housing.oxyprt",
                "M6x20 Screw.oxyprt",
                "Shaft.oxyprt",
                "Gear Z20.oxyprt",
                "Input stage.oxyasm",
                "Gearbox.oxyasm",
            ]
        );
        assert_eq!((plan.part_count(), plan.assembly_count()), (4, 2));

        let top = as_assembly(plan.top());
        let components = &top.assembly.components;
        assert_eq!(components.len(), 4);
        assert_eq!(components[1].name, "M6x20 Screw<1>");
        assert_eq!(components[2].name, "M6x20 Screw<2>");
        assert_eq!(components[1].path, "M6x20 Screw.oxyprt");
        assert_eq!(components[1].document_id, components[2].document_id);
        assert_eq!(
            components[2].placement.translation,
            Vec3::new(-10.0, 0.0, 0.0)
        );
        assert_eq!(components[3].path, "Input stage.oxyasm");
        assert_eq!(
            components[3].document_id,
            as_assembly(&plan.files[4]).document_id
        );

        // Each part holds one Imported feature with its own solids.
        let ImportedDocument::Part(screw) = &plan.files[1].document else {
            panic!()
        };
        let part = Part::from_definition(screw.definition.clone()).unwrap();
        let features = part.features();
        assert_eq!(features.len(), 1);
        let FeatureKind::Import(import) = features[0].kind() else {
            panic!()
        };
        assert_eq!(import.data.bytes(), b"ISO-10303-21; M6x20 Screw");
        assert_eq!(import.file_name, "gearbox.step");
        assert_eq!(screw.properties.part_number, "M6x20 Screw");
    }

    #[test]
    fn names_are_valid_unique_and_fall_back() {
        let tree = StepAssembly {
            nodes: vec![
                assembly(
                    "",
                    &[
                        (1, Vec3::ZERO),
                        (2, Vec3::ZERO),
                        (3, Vec3::ZERO),
                        (4, Vec3::ZERO),
                        (5, Vec3::ZERO),
                    ],
                ),
                part("bolt"),
                part("Bolt"),
                part(""),
                part("a/b: c?. "),
                part("con"),
            ],
            root: 0,
        };
        let plan = plan_step_import(&tree, "C:/x/Pump.STEP").unwrap();
        assert_eq!(
            names(&plan),
            [
                "bolt.oxyprt",
                "Bolt (2).oxyprt",
                "Part1.oxyprt",
                "a_b_ c_.oxyprt",
                "con_.oxyprt",
                "Pump.oxyasm",
            ]
        );
    }

    #[test]
    fn empty_products_are_dropped_and_a_lone_part_stays_a_part() {
        let mut tree = gearbox();
        // The sub-assembly's parts have no solid: it disappears.
        for node in [4, 5] {
            tree.nodes[node].content = StepNodeContent::Part { step: Vec::new() };
        }
        let plan = plan_step_import(&tree, "g.step").unwrap();
        assert_eq!(plan.files.len(), 3);
        assert_eq!(as_assembly(plan.top()).assembly.components.len(), 3);

        let single = StepAssembly {
            nodes: vec![part("")],
            root: 0,
        };
        let plan = plan_step_import(&single, "Bracket.stp").unwrap();
        assert_eq!(names(&plan), ["Bracket.oxyprt"]);

        let empty = StepAssembly {
            nodes: vec![assembly("A", &[])],
            root: 0,
        };
        assert_eq!(plan_step_import(&empty, "a.step"), Err(PlanError::NoSolids));
    }

    #[test]
    fn bad_structures_are_refused() {
        let cyclic = StepAssembly {
            nodes: vec![
                assembly("A", &[(1, Vec3::ZERO)]),
                assembly("B", &[(0, Vec3::ZERO)]),
            ],
            root: 0,
        };
        assert!(matches!(
            plan_step_import(&cyclic, "a.step"),
            Err(PlanError::InvalidStructure(_))
        ));

        let mut mirrored = gearbox();
        if let StepNodeContent::Assembly { instances } = &mut mirrored.nodes[0].content {
            instances[0].transform.rows[0][0] = -1.0;
        }
        assert!(matches!(
            plan_step_import(&mirrored, "a.step"),
            Err(PlanError::NotRigid { .. })
        ));
    }

    #[test]
    fn written_files_load_back() {
        let dir = std::env::temp_dir().join(format!("oxyde-step-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plan = plan_step_import(&gearbox(), "gearbox.step").unwrap();
        assert!(plan.existing_files(&dir).is_empty());
        let top = plan.write(&dir).unwrap();
        assert_eq!(top, dir.join("Gearbox.oxyasm"));
        assert_eq!(plan.existing_files(&dir).len(), 6);

        let gearbox = load_assembly(&top).unwrap();
        assert_eq!(gearbox.assembly.components.len(), 4);
        for component in &gearbox.assembly.components {
            let path = dir.join(&component.path);
            let id = if component.path.ends_with(".oxyasm") {
                load_assembly(&path).unwrap().document_id
            } else {
                load_part(&path).unwrap().document_id
            };
            assert_eq!(id, component.document_id);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
