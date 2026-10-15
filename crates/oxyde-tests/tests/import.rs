//! STEP import: geometry read from a STEP file becomes an "Imported" feature that later
//! features build on, and survives a save and a reload with the file stored in the part.

use std::io::Cursor;

use oxyde_geom::{Point2, Point3, Vec3};
use oxyde_kernel::Kernel;
use oxyde_model::{
    EdgeBlend, EdgeName, Extrude, ExtrudeOp, FaceKey, FeatureKind, FeatureStatus, FileData, Import,
    Part, Sketch, SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;

/// A 40 × 20 × 10 box at the origin, written to STEP and read back as bytes.
fn box_step(kernel: &mut OcctKernel) -> Vec<u8> {
    let shape = kernel
        .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(40.0, 20.0, 10.0))
        .unwrap();
    let path = std::env::temp_dir().join(format!("oxyde-import-{}.step", std::process::id()));
    kernel.export_step(&[shape], &path).unwrap();
    kernel.release(shape);
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    bytes
}

fn import(bytes: Vec<u8>) -> FeatureKind {
    FeatureKind::Import(Import {
        file_name: "box.step".to_owned(),
        data: FileData::new(bytes),
    })
}

fn assert_all_ok(part: &Part) {
    for entry in part.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{}", entry.name);
    }
}

#[test]
fn imported_box_is_named_and_can_be_built_on() {
    let mut kernel = OcctKernel::new();
    let bytes = box_step(&mut kernel);
    let mut part = Part::new();
    let imported = part.add_feature(import(bytes));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_eq!(part.feature(imported).unwrap().name(), "Imported1");
    let volume = kernel.volume(part.body().unwrap().solids[0].shape).unwrap();
    assert!((volume - 8000.0).abs() < 1e-3, "{volume}");

    // Every face is named after the import; round one of its edges.
    let body = part.body().unwrap();
    assert_eq!(body.topology.faces.len(), 6);
    for i in 0..6 {
        let name = body.naming.face(i).unwrap();
        assert!(matches!(name.key, FaceKey::Imported { feature, .. } if feature == imported));
    }
    let edge: EdgeName = body.naming.edge(0).unwrap().clone();
    part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: vec![edge],
        size: 2.0,
    }));

    // And a boss sketched on the Front plane, fused with the imported body.
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::new(50.0, 0.0), 10.0, 10.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        5.0,
    )));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let volume = kernel.volume(part.body().unwrap().solids[0].shape).unwrap();
    // The fillet removes (1 - π/4)·2²·length, with length 40, 20 or 10 depending on the edge.
    let boss = 500.0;
    let removed = |length: f64| (1.0 - std::f64::consts::FRAC_PI_4) * 4.0 * length;
    assert!(
        [40.0, 20.0, 10.0]
            .iter()
            .any(|l| (volume - (8000.0 + boss - removed(*l))).abs() < 1e-2),
        "{volume}"
    );

    // The part keeps the file: save, reload, rebuild to the same volume.
    let file = oxyde_format::PartFile {
        definition: part.definition(),
        ..oxyde_format::PartFile::new()
    };
    let bytes = oxyde_format::write_part(Cursor::new(Vec::new()), &file)
        .unwrap()
        .into_inner();
    let loaded = oxyde_format::read_part(Cursor::new(bytes)).unwrap();
    let mut reloaded = Part::from_definition(loaded.definition).unwrap();
    reloaded.rebuild(&mut kernel);
    assert_all_ok(&reloaded);
    let again = kernel
        .volume(reloaded.body().unwrap().solids[0].shape)
        .unwrap();
    assert!((again - volume).abs() < 1e-6);
}

#[test]
fn a_file_that_is_not_step_is_reported() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let id = part.add_feature(import(b"this is not a STEP file".to_vec()));
    part.rebuild(&mut kernel);
    match part.status(id) {
        Some(FeatureStatus::Error(message)) => assert!(message.contains("box.step"), "{message}"),
        other => panic!("expected an error, got {other:?}"),
    }
}
