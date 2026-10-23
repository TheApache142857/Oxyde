//! An assembly's equations driving its mates and its parts, on real part files rebuilt by
//! OpenCASCADE: the values reach the parts' geometry, the parts are saved with them, and a
//! linked equations file drives the whole assembly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use oxyde_assembly::{
    Assembly, ComponentId, MateKind, MateRef, MateTarget, PartValues, Placement, TopLevelOnly,
    resolve, solve,
};
use oxyde_format::{AssemblyFile, PartFile, Properties};
use oxyde_geom::{Point2, Vec3};
use oxyde_kernel::Kernel;
use oxyde_model::{Extrude, ExtrudeOp, FeatureKind, Part, Sketch, SketchPlane, StandardPlane};
use oxyde_occt::OcctKernel;
use oxyde_sketch::{Constraint, EntityId};

/// A 40 × 20 plate, 10 thick, on the Front plane: `D1@Sketch1` = 40, `D2@Sketch1` = 20,
/// `D1@Boss-Extrude1` = 10.
fn plate() -> Part {
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
    part
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oxyde-asm-eq-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn save_plate(path: &Path) {
    let file = PartFile {
        document_id: oxyde_format::new_document_id(),
        definition: plate().definition(),
        properties: Properties::default(),
    };
    oxyde_format::save_part(path, &file).unwrap();
}

/// Two instances of the plate, held apart along Z by `Distance1` between their Front planes.
fn two_plates(dir: &Path) -> AssemblyFile {
    save_plate(&dir.join("Plate.oxyprt"));
    let mut file = AssemblyFile::new();
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
        Placement::IDENTITY.moved(
            Vec3::ZERO,
            oxyde_geom::Point3::ORIGIN,
            Vec3::new(0.0, 0.0, 30.0),
        ),
    );
    let front = |c| MateRef::new(c, MateTarget::Plane(StandardPlane::Front));
    asm.add_mate(MateKind::Distance(25.0), front(a), front(b), true);
    file
}

/// The assembly's parts, loaded from their files and rebuilt: what the application does.
struct Loaded<'k> {
    kernel: &'k mut OcctKernel,
    files: HashMap<ComponentId, PathBuf>,
    parts: HashMap<PathBuf, Part>,
    changed: Vec<PathBuf>,
}

impl<'k> Loaded<'k> {
    fn open(kernel: &'k mut OcctKernel, dir: &Path, assembly: &Assembly) -> Self {
        let mut loaded = Loaded {
            kernel,
            files: HashMap::new(),
            parts: HashMap::new(),
            changed: Vec::new(),
        };
        for c in &assembly.components {
            let path = dir.join(&c.path);
            if !loaded.parts.contains_key(&path) {
                let file = oxyde_format::load_part(&path).unwrap();
                let mut part = Part::from_definition(file.definition).unwrap();
                part.rebuild(loaded.kernel);
                loaded.parts.insert(path.clone(), part);
            }
            loaded.files.insert(c.id, path);
        }
        loaded
    }

    /// Rebuilds the parts the equations changed.
    fn rebuild(&mut self) {
        for path in std::mem::take(&mut self.changed) {
            if let Some(part) = self.parts.get_mut(&path) {
                part.rebuild(self.kernel);
            }
        }
    }

    fn volume(&mut self, path: &Path) -> f64 {
        let part = &self.parts[path];
        part.body()
            .unwrap()
            .shapes()
            .iter()
            .map(|s| self.kernel.volume(*s).unwrap())
            .sum()
    }
}

impl PartValues for Loaded<'_> {
    fn file(&self, path: &[ComponentId]) -> Option<PathBuf> {
        self.files.get(path.first()?).cloned()
    }

    fn value(&self, path: &[ComponentId], name: &str) -> Option<f64> {
        self.parts
            .get(self.files.get(path.first()?)?)?
            .value_of(name)
    }

    fn drive(&mut self, path: &[ComponentId], name: &str, value: f64) -> Result<f64, String> {
        let file = path
            .first()
            .and_then(|c| self.files.get(c))
            .ok_or("not loaded")?
            .clone();
        let part = self.parts.get_mut(&file).ok_or("not loaded")?;
        let (now, changed) = part.drive(name, value)?;
        if changed && !self.changed.contains(&file) {
            self.changed.push(file);
        }
        Ok(now)
    }
}

fn add_equation(asm: &mut Assembly, target: &str, text: &str) {
    let target = asm.find_dimension(target, &TopLevelOnly).unwrap();
    let name = asm.dimension_name(&target, &TopLevelOnly).unwrap();
    let expr = asm.parse_expression(text, &TopLevelOnly).unwrap();
    asm.equations.add_equation(target, name, expr).unwrap();
}

/// How far apart the two plates' origins are along Z, once the mates are solved.
fn gap(asm: &mut Assembly) -> f64 {
    let solution = solve(asm, |r| resolve(&r.target, None), 100.0);
    for (id, p) in &solution.placements {
        asm.component_mut(*id).unwrap().placement = *p;
    }
    let z = |i: usize| asm.components[i].placement.translation.z;
    (z(1) - z(0)).abs()
}

#[test]
fn assembly_equations_drive_mates_and_parts_which_are_saved() {
    let dir = folder("drive");
    let mut file = two_plates(&dir);
    let asm = &mut file.assembly;
    let gap_value = asm.parse_expression("4", &TopLevelOnly).unwrap();
    asm.equations.add_global("Gap", gap_value).unwrap();
    add_equation(asm, "D1@Distance1", "\"Gap\" * 3");
    add_equation(asm, "D1@Boss-Extrude1@Plate<1>.Part", "\"Gap\" + 1");
    // The two instances share one file: a second value for it is refused.
    add_equation(asm, "D1@Boss-Extrude1@Plate<2>.Part", "\"Gap\" + 2");
    // An equation may read a part's value.
    add_equation(
        asm,
        "D1@Sketch1@Plate<2>.Part",
        "\"D2@Sketch1@Plate<1>.Part\" * 3",
    );
    let path = dir.join("Plates.oxyasm");
    oxyde_format::save_assembly(&path, &file).unwrap();

    // Opened again, as the application opens it.
    let mut file = oxyde_format::load_assembly(&path).unwrap();
    let mut kernel = OcctKernel::new();
    let plate = dir.join("Plate.oxyprt");
    let mut loaded = Loaded::open(&mut kernel, &dir, &file.assembly);
    assert!((loaded.volume(&plate) - 40.0 * 20.0 * 10.0).abs() < 1e-6);
    let results = file.assembly.evaluate_equations(&mut loaded);
    assert_eq!(results.equations[0], Ok(12.0));
    assert_eq!(results.equations[1], Ok(5.0));
    let refused = results.equations[2].as_ref().unwrap_err();
    assert!(refused.contains("instance"), "{refused}");
    assert_eq!(results.equations[3], Ok(60.0));
    loaded.rebuild();
    assert!((loaded.volume(&plate) - 60.0 * 20.0 * 5.0).abs() < 1e-6);
    assert!((gap(&mut file.assembly) - 12.0).abs() < 1e-6);

    // Saved with the assembly, the part keeps the values the assembly gave it.
    let definition = loaded.parts[&plate].definition();
    let mut part_file = oxyde_format::load_part(&plate).unwrap();
    part_file.definition = definition;
    oxyde_format::save_part(&plate, &part_file).unwrap();
    let reopened =
        Part::from_definition(oxyde_format::load_part(&plate).unwrap().definition).unwrap();
    let depth = reopened.find_dimension("D1@Boss-Extrude1").unwrap();
    assert_eq!(reopened.dimension(depth).unwrap().value, 5.0);

    // Evaluated again, nothing changes: no part to rebuild.
    file.assembly.evaluate_equations(&mut loaded);
    assert!(loaded.changed.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_linked_equations_file_drives_the_assembly_and_its_parts() {
    let dir = folder("linked");
    std::fs::write(dir.join("Case.txt"), "' The case\n\"Gap\" = 4\n").unwrap();
    let mut file = two_plates(&dir);
    let values = oxyde_format::equations_file::read_equations_file(&dir.join("Case.txt")).unwrap();
    let asm = &mut file.assembly;
    asm.link_equations(Some("Case.txt".to_owned()), &values, &TopLevelOnly)
        .unwrap();
    add_equation(asm, "D1@Distance1", "\"Gap\" * 2");
    add_equation(asm, "D1@Boss-Extrude1@Plate<1>.Part", "\"Gap\"");
    let path = dir.join("Plates.oxyasm");
    oxyde_format::save_assembly(&path, &file).unwrap();

    // Another document changed the file: opening the assembly takes the new value, and
    // its mates and parts follow.
    oxyde_format::equations_file::update_equations_file(
        &dir.join("Case.txt"),
        &[oxyde_model::LinkedValue {
            name: "Gap".to_owned(),
            expression: "7".to_owned(),
        }],
    )
    .unwrap();
    let mut file = oxyde_format::load_assembly(&path).unwrap();
    let mut kernel = OcctKernel::new();
    let plate = dir.join("Plate.oxyprt");
    let mut loaded = Loaded::open(&mut kernel, &dir, &file.assembly);
    let results = file.assembly.evaluate_equations(&mut loaded);
    assert_eq!(results.globals, vec![Ok(7.0)]);
    assert_eq!(results.equations, vec![Ok(14.0), Ok(7.0)]);
    loaded.rebuild();
    assert!((loaded.volume(&plate) - 40.0 * 20.0 * 7.0).abs() < 1e-6);
    assert!((gap(&mut file.assembly) - 14.0).abs() < 1e-6);
    std::fs::remove_dir_all(&dir).unwrap();
}
