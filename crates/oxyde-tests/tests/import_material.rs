//! A material given to an imported part that has never been rebuilt (a part file written
//! by an assembly's STEP import, given a material from the assembly) keeps its look when
//! the file is opened again: the STEP colours the import was still due to apply do not
//! come back over it.
//!
//! One test, as the other STEP tests: OpenCASCADE's STEP translators run behind a lock.

use std::io::Cursor;

use oxyde_format::{PartFile, read_part, write_part};
use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{Kernel, StepColor, StepColors};
use oxyde_model::{Appearance, FeatureKind, FileData, Import, Material, Part, PartDefinition};
use oxyde_occt::OcctKernel;

const RED: [u8; 3] = [200, 40, 40];

/// Writes `definition` to a part file and opens it again, rebuilt.
fn reopen(definition: PartDefinition, kernel: &mut OcctKernel) -> Part {
    let mut file = PartFile::new();
    file.definition = definition;
    let archive = write_part(Cursor::new(Vec::new()), &file)
        .unwrap()
        .into_inner();
    let back = read_part(Cursor::new(&archive)).unwrap();
    let mut part = Part::from_definition(back.definition).unwrap();
    part.rebuild(kernel);
    part
}

/// The colour and transparency every face of `part` is drawn with.
fn looks(part: &Part) -> Vec<Option<([u8; 3], f64)>> {
    let body = part.body().expect("the import builds a body");
    (0..body.topology.faces.len() as u32)
        .map(|i| {
            part.face_appearance(body, i)
                .map(|a| (a.color, a.transparency))
        })
        .collect()
}

#[test]
fn material_survives_reopening_an_imported_part() {
    let mut kernel = OcctKernel::new();

    // ---- A red box, written to STEP with its colour.
    let shape = kernel
        .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(40.0, 20.0, 10.0))
        .unwrap();
    let colors = StepColors {
        solids: vec![Some(StepColor {
            rgb: RED,
            transparency: 0.0,
        })],
        faces: vec![None; 6],
    };
    let path =
        std::env::temp_dir().join(format!("oxyde-import-material-{}.step", std::process::id()));
    kernel.export_step(&[shape], &[colors], &[], &path).unwrap();
    kernel.release(shape);
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);

    // ---- The part file as an assembly import writes it: never rebuilt, so the import
    // still has its colours to apply and the part no appearance yet.
    let mut part = Part::new();
    part.add_feature(FeatureKind::Import(Import::new(
        "panel.step",
        FileData::new(bytes),
    )));
    let unbuilt = part.definition();
    assert!(unbuilt.appearances.is_empty());
    assert!(matches!(
        unbuilt.features[0].kind(),
        FeatureKind::Import(import) if import.apply_colors
    ));
    let mut glass = Appearance::colored([205, 225, 235]);
    glass.transparency = 0.95;
    let polycarbonate = Material::new("Polycarbonate", "Plastic", glass.clone(), 1200.0);

    // ---- Only the material written: the STEP colour comes back over it on reopening, as
    // the body's appearance (an appearance wins over the material, as in SolidWorks).
    let mut kept = unbuilt.clone();
    kept.material = Some(polycarbonate.clone());
    let mut reopened = reopen(kept, &mut kernel);
    assert!(
        looks(&reopened).iter().all(|l| *l == Some((RED, 0.0))),
        "{:?}",
        looks(&reopened)
    );
    reopened.release(&mut kernel);

    // ---- The material written with the appearances removed, as Apply and remove
    // appearances does: every face has the material's look, after any number of reopenings.
    let mut cleared = unbuilt;
    cleared.material = Some(polycarbonate);
    cleared.clear_appearances();
    let mut reopened = reopen(cleared, &mut kernel);
    for _ in 0..2 {
        assert!(
            reopened.appearances().is_empty(),
            "{:?}",
            reopened.appearances()
        );
        let faces = looks(&reopened);
        assert_eq!(faces.len(), 6);
        for look in &faces {
            let (color, transparency) = look.expect("the material's look");
            assert_eq!(color, glass.color);
            assert!(
                (transparency - glass.transparency).abs() < 1e-9,
                "{transparency}"
            );
        }
        let definition = reopened.definition();
        reopened.release(&mut kernel);
        reopened = reopen(definition, &mut kernel);
    }
    reopened.release(&mut kernel);
    part.release(&mut kernel);
}
