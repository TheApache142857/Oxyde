//! STEP assembly import: the product structure of a STEP file becomes part files holding
//! "Imported" features and assembly files placing them, and every part rebuilds.
//!
//! `data/cart.step` was written by OpenCASCADE's XCAF writer: a base plate, four pins
//! (one part), a "Wheel unit" sub-assembly (wheel + axle) used twice, once turned half a
//! turn, and a bracket used twice.

use std::path::Path;

use oxyde_format::step_import::{ImportedDocument, plan_step_import};
use oxyde_format::{load_assembly, load_part};
use oxyde_geom::Vec3;
use oxyde_kernel::{Kernel, StepNodeContent};
use oxyde_model::{FeatureStatus, Part};
use oxyde_occt::OcctKernel;

const CART: &[u8] = include_bytes!("data/cart.step");

fn volume_of_part(kernel: &mut OcctKernel, path: &Path) -> f64 {
    let file = load_part(path).unwrap();
    let mut part = Part::from_definition(file.definition).unwrap();
    part.rebuild(kernel);
    for entry in part.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{}", path.display());
    }
    part.body()
        .unwrap()
        .solids
        .iter()
        .map(|s| kernel.volume(s.shape).unwrap())
        .sum()
}

#[test]
fn product_structure_is_read_once_per_product() {
    let mut kernel = OcctKernel::new();
    let tree = kernel.read_step_assembly(CART).unwrap();
    let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Base plate",
            "Pin D6",
            "Wheel",
            "Axle",
            "Wheel unit",
            "Bracket",
            "Cart"
        ]
    );
    assert_eq!(tree.nodes[tree.root].name, "Cart");
    let StepNodeContent::Assembly { instances } = &tree.nodes[tree.root].content else {
        panic!("the top level is an assembly");
    };
    assert_eq!(instances.len(), 9);
    // Unnamed instances come through without OCCT's label placeholders.
    assert!(instances.iter().all(|i| !i.name.starts_with("=>")));
    // The second wheel unit is turned half a turn about Z.
    let turned = &instances[6].transform.rows;
    assert!((turned[0][0] + 1.0).abs() < 1e-9 && (turned[1][1] + 1.0).abs() < 1e-9);
    assert!((turned[0][3] - 100.0).abs() < 1e-9);
}

#[test]
fn cart_becomes_part_and_assembly_files_that_rebuild() {
    let mut kernel = OcctKernel::new();
    let tree = kernel.read_step_assembly(CART).unwrap();
    let plan = plan_step_import(&tree, "cart.step").unwrap();
    let names: Vec<&str> = plan.files.iter().map(|f| f.file_name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Base plate.oxyprt",
            "Pin D6.oxyprt",
            "Wheel.oxyprt",
            "Axle.oxyprt",
            "Wheel unit.oxyasm",
            "Bracket.oxyprt",
            "Cart.oxyasm",
        ]
    );

    let dir = std::env::temp_dir().join(format!("oxyde-cart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let top = plan.write(&dir).unwrap();

    // Each part holds just its own solid, in its own coordinates.
    let pi = std::f64::consts::PI;
    for (file, expected) in [
        ("Base plate.oxyprt", 100.0 * 60.0 * 5.0),
        ("Pin D6.oxyprt", pi * 9.0 * 20.0),
        ("Wheel.oxyprt", pi * 144.0 * 6.0),
        ("Axle.oxyprt", pi * 4.0 * 16.0),
        ("Bracket.oxyprt", 30.0 * 10.0 * 4.0 + 4.0 * 10.0 * 16.0),
    ] {
        let volume = volume_of_part(&mut kernel, &dir.join(file));
        assert!(
            (volume - expected).abs() < 1e-3 * expected,
            "{file}: {volume}"
        );
    }

    let cart = load_assembly(&top).unwrap();
    let components = &cart.assembly.components;
    let listed: Vec<(&str, &str)> = components
        .iter()
        .map(|c| (c.name.as_str(), c.path.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("Base plate<1>", "Base plate.oxyprt"),
            ("Pin D6<1>", "Pin D6.oxyprt"),
            ("Pin D6<2>", "Pin D6.oxyprt"),
            ("Pin D6<3>", "Pin D6.oxyprt"),
            ("Pin D6<4>", "Pin D6.oxyprt"),
            ("Wheel unit<1>", "Wheel unit.oxyasm"),
            ("Wheel unit<2>", "Wheel unit.oxyasm"),
            ("Bracket<1>", "Bracket.oxyprt"),
            ("Bracket<2>", "Bracket.oxyprt"),
        ]
    );
    assert_eq!(
        components[4].placement.translation,
        Vec3::new(90.0, 50.0, 5.0)
    );
    let ImportedDocument::Assembly(unit) = &plan.files[4].document else {
        panic!()
    };
    assert_eq!(components[5].document_id, unit.document_id);
    let unit = load_assembly(&dir.join("Wheel unit.oxyasm")).unwrap();
    assert_eq!(unit.assembly.components.len(), 2);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_plain_part_file_imports_as_one_part() {
    let mut kernel = OcctKernel::new();
    let shape = kernel
        .make_box(
            oxyde_geom::Point3::new(0.0, 0.0, 0.0),
            Vec3::new(10.0, 10.0, 10.0),
        )
        .unwrap();
    let path = std::env::temp_dir().join(format!("oxyde-cube-{}.step", std::process::id()));
    kernel.export_step(&[shape], &path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);

    let tree = kernel.read_step_assembly(&bytes).unwrap();
    let plan = plan_step_import(&tree, "Cube.step").unwrap();
    assert_eq!(plan.files.len(), 1);
    assert!(
        plan.top().file_name.ends_with(".oxyprt"),
        "{}",
        plan.top().file_name
    );
}
