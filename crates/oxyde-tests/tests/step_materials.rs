//! STEP materials: a part's material (its name and density) written to STEP comes back on
//! import, as the material of the imported part, and the parts of a STEP assembly carry
//! theirs into the import plan. Fusion 360 and SolidWorks write materials this way (AP242
//! `material name` and `density measure` properties).
//!
//! One round trip per test file, because OpenCASCADE's STEP translators do not like being
//! driven from several threads at once (see the notes in `import.rs`).

use oxyde_format::step_import::{ImportedDocument, plan_step_import};
use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{Kernel, StepMaterial, StepNodeContent};
use oxyde_model::{FeatureStatus, Part};
use oxyde_occt::OcctKernel;

fn temp_step(stem: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("oxyde-{stem}-{}.step", std::process::id()))
}

fn write_box(kernel: &mut OcctKernel, material: Option<StepMaterial>, stem: &str) -> Vec<u8> {
    let shape = kernel
        .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(40.0, 20.0, 10.0))
        .unwrap();
    let path = temp_step(stem);
    kernel
        .export_step(&[shape], &[], &[material], &path)
        .unwrap();
    kernel.release(shape);
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    bytes
}

#[test]
fn materials_round_trip_through_step() {
    let mut kernel = OcctKernel::new();
    let steel = StepMaterial {
        name: "Acier".to_owned(),
        description: "Steel".to_owned(),
        density: 7850.0,
    };
    let bytes = write_box(&mut kernel, Some(steel.clone()), "material");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("'Acier'"), "the material name is in the file");
    assert!(text.contains("density"), "the density is in the file");

    // ---- Back as one body: the material comes with it, in kg/m³.
    let imported = kernel.import_step(&bytes).unwrap();
    let back = imported.material.clone().expect("the material is read back");
    assert_eq!(back.name, "Acier");
    assert!(
        (back.density - 7850.0).abs() < 1.0,
        "density {} kg/m³, expected 7850",
        back.density
    );
    kernel.release(imported.shape);

    // ---- Through the product structure: the part node and the planned part file.
    let tree = kernel.read_step_assembly(&bytes).unwrap();
    let node_material = tree
        .nodes
        .iter()
        .find_map(|n| match &n.content {
            StepNodeContent::Part { material, .. } => material.clone(),
            StepNodeContent::Assembly { .. } => None,
        })
        .expect("the part node names its material");
    assert_eq!(node_material.name, "Acier");
    let plan = plan_step_import(&tree, "Bar.step").unwrap();
    let ImportedDocument::Part(file) = &plan.top().document else {
        panic!("a single part");
    };
    let material = file.definition.material.as_ref().expect("the part is made of it");
    assert_eq!(material.name, "Acier");
    assert!(material.is_from_step(), "told apart from the library's");
    assert!((material.density - 7850.0).abs() < 1.0);
    assert_eq!(file.properties.material, "Acier", "the bill of materials sees it");

    // ---- A part rebuilt from the file takes the material too (once, like the colours).
    let mut part = Part::from_definition(file.definition.clone()).unwrap();
    part.rebuild(&mut kernel);
    for entry in part.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok);
    }
    let on_part = part.material().expect("the rebuilt part is made of it");
    assert_eq!(on_part.name, "Acier");
    // The volume of the box is 8000 mm³: 62.8 g of steel.
    assert!((on_part.mass_kg(8000.0) - 0.0628).abs() < 1e-4);
    part.release(&mut kernel);

    // ---- A part without a material writes none and reads none.
    let plain = write_box(&mut kernel, None, "no-material");
    let imported = kernel.import_step(&plain).unwrap();
    assert!(imported.material.is_none());
    kernel.release(imported.shape);
    let tree = kernel.read_step_assembly(&plain).unwrap();
    let plan = plan_step_import(&tree, "Plain.step").unwrap();
    let ImportedDocument::Part(file) = &plan.top().document else {
        panic!("a single part");
    };
    assert!(file.definition.material.is_none());
}

/// Lists the materials a real STEP file names (a Fusion 360 or SolidWorks export), for
/// checking by hand:
///
/// ```text
/// OXYDE_STEP_FILE="C:\...\Assembly.step" cargo test -p oxyde-tests --test step_materials -- --ignored --nocapture
/// ```
#[test]
#[ignore]
fn materials_of_a_real_file() {
    let Ok(path) = std::env::var("OXYDE_STEP_FILE") else {
        eprintln!("set OXYDE_STEP_FILE to a STEP file");
        return;
    };
    let bytes = std::fs::read(&path).unwrap();
    let mut kernel = OcctKernel::new();
    let tree = kernel.read_step_assembly(&bytes).unwrap();
    let mut seen: Vec<(String, f64, usize)> = Vec::new();
    let (mut parts, mut with) = (0, 0);
    for n in &tree.nodes {
        if let StepNodeContent::Part { material, .. } = &n.content {
            parts += 1;
            if let Some(m) = material {
                with += 1;
                match seen.iter_mut().find(|(name, _, _)| *name == m.name) {
                    Some(entry) => entry.2 += 1,
                    None => seen.push((m.name.clone(), m.density, 1)),
                }
            }
        }
    }
    println!("{parts} part(s), {with} with a material:");
    for (name, density, count) in &seen {
        println!("  {name}: {density} kg/m³ ({count} part(s))");
    }
}
