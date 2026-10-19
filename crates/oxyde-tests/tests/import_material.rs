//! A material given to an imported part that has never been rebuilt (a part file written
//! by an assembly's STEP import, given a material from the assembly) keeps its look when
//! the file is opened again: the STEP colours the import was still due to apply do not
//! come back over it. Nor does the material the STEP file names come back on such a part
//! whose material was removed from the assembly.

use std::io::Cursor;

use oxyde_format::step_import::{ImportedDocument, plan_step_import};
use oxyde_format::{PartFile, read_part, write_part};
use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{Kernel, StepColor, StepColors, StepMaterial};
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

#[test]
fn removed_material_stays_removed_from_an_imported_part() {
    let mut kernel = OcctKernel::new();

    // ---- A red steel box, written to STEP with its colour and its material.
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
    let steel = StepMaterial {
        name: "Acier".to_owned(),
        description: String::new(),
        density: 7850.0,
    };
    let path = std::env::temp_dir().join(format!(
        "oxyde-import-removed-material-{}.step",
        std::process::id()
    ));
    kernel
        .export_step(&[shape], &[colors], &[Some(steel)], &path)
        .unwrap();
    kernel.release(shape);
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);

    // ---- The part file the import of a STEP assembly writes: made of the STEP material,
    // never rebuilt, so its import still has the colour (and the material) to apply.
    let tree = kernel.read_step_assembly(&bytes).unwrap();
    let plan = plan_step_import(&tree, "Bar.step").unwrap();
    let ImportedDocument::Part(file) = &plan.top().unwrap().document else {
        panic!("a single part");
    };
    let unbuilt = file.definition.clone();
    assert_eq!(
        unbuilt.material.as_ref().map(|m| m.name.as_str()),
        Some("Acier")
    );
    assert!(unbuilt.appearances.is_empty());

    // ---- The copy an assembly loads, rebuilt; the material removed from it is what the
    // user sees: no material, the STEP colour.
    let mut loaded = Part::from_definition(unbuilt.clone()).unwrap();
    loaded.rebuild(&mut kernel);
    let built = loaded.definition();
    loaded.set_material(None);
    let seen = looks(&loaded);
    assert!(seen.iter().all(|l| *l == Some((RED, 0.0))), "{seen:?}");
    loaded.release(&mut kernel);

    // ---- Only the material taken out of the file: the import puts the STEP material back
    // on reopening, since the part has none.
    let mut kept = unbuilt.clone();
    kept.material = None;
    let mut reopened = reopen(kept, &mut kernel);
    assert_eq!(reopened.material().map(|m| m.name.as_str()), Some("Acier"));
    reopened.release(&mut kernel);

    // ---- Settled from the loaded copy first, as the assembly writes it: no material and
    // the same look, after any number of reopenings.
    let mut settled = unbuilt;
    assert!(settled.settle_imports(&built));
    settled.material = None;
    let mut reopened = reopen(settled, &mut kernel);
    for _ in 0..2 {
        assert!(reopened.material().is_none(), "{:?}", reopened.material());
        assert_eq!(looks(&reopened), seen);
        let definition = reopened.definition();
        reopened.release(&mut kernel);
        reopened = reopen(definition, &mut kernel);
    }
    reopened.release(&mut kernel);
}
