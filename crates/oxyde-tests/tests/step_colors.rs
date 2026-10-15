//! STEP colours: a coloured part written to STEP comes back with the same colours, as
//! appearances of the import feature, and goes out again unchanged.
//!
//! One test, because OpenCASCADE's STEP translators do not like being driven from several
//! threads at once (see the notes in `import.rs`).

use std::io::Cursor;

use oxyde_format::{PartFile, read_part, write_part};
use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{Kernel, StepColor, StepColors};
use oxyde_model::{
    Appearance, BodyId, FaceKey, FaceName, FeatureKind, FeatureStatus, FileData, Import, Material,
    Part,
};
use oxyde_occt::OcctKernel;

const RED: StepColor = StepColor {
    rgb: [200, 40, 40],
    transparency: 0.0,
};
const BLUE_GLASS: StepColor = StepColor {
    rgb: [50, 100, 210],
    transparency: 0.5,
};

fn close(a: Option<StepColor>, b: Option<StepColor>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.rgb == b.rgb && (a.transparency - b.transparency).abs() < 0.02,
        _ => false,
    }
}

fn temp_step(stem: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("oxyde-{stem}-{}.step", std::process::id()))
}

#[test]
fn colours_round_trip_through_step() {
    let mut kernel = OcctKernel::new();

    // ---- A red box with one blue, half transparent face, written with colours.
    let shape = kernel
        .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(40.0, 20.0, 10.0))
        .unwrap();
    let faces = kernel.topology(shape).unwrap().faces.len();
    assert_eq!(faces, 6);
    let mut colors = StepColors {
        solids: vec![Some(RED)],
        faces: vec![None; faces],
    };
    colors.faces[2] = Some(BLUE_GLASS);
    let path = temp_step("colours");
    kernel.export_step(&[shape], &[colors], &path).unwrap();
    kernel.release(shape);
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);

    // STEP stores sRGB values in 0..1: 200/255 = 0.784.
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("COLOUR_RGB"), "no colours written");
    assert!(
        text.contains("0.7843") && text.contains("0.1568"),
        "red was not written as sRGB: {}",
        text.lines().filter(|l| l.contains("COLOUR_RGB")).collect::<Vec<_>>().join("\n")
    );

    // ---- Read back by the kernel: the solid is red, face 2 blue, the others take red.
    let imported = kernel.import_step(&bytes).unwrap();
    assert_eq!(imported.colors.solids.len(), 1);
    assert!(close(imported.colors.solids[0], Some(RED)), "{:?}", imported.colors.solids);
    assert_eq!(imported.colors.faces.len(), 6);
    let blue: Vec<usize> = (0..6)
        .filter(|&i| close(imported.colors.faces[i], Some(BLUE_GLASS)))
        .collect();
    assert_eq!(blue.len(), 1, "{:?}", imported.colors.faces);
    for (i, face) in imported.colors.faces.iter().enumerate() {
        if i != blue[0] {
            assert!(close(*face, Some(RED)), "face {i}: {face:?}");
        }
    }
    kernel.release(imported.shape);

    // ---- As an Imported feature: the colours become appearances, once.
    let mut part = Part::new();
    let feature = part.add_feature(FeatureKind::Import(Import::new(
        "box.step",
        FileData::new(bytes.clone()),
    )));
    part.rebuild(&mut kernel);
    for entry in part.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{}", entry.name);
    }
    let body_id = BodyId::new(feature, 0);
    let blue_face: FaceName = {
        let appearances = part.appearances();
        let body_appearance = appearances
            .body(body_id)
            .expect("the solid's colour is on the body");
        assert_eq!(body_appearance.color, RED.rgb);
        assert_eq!(appearances.faces.len(), 1, "{:?}", appearances.faces);
        let (blue_face, blue_appearance) = &appearances.faces[0];
        assert_eq!(blue_appearance.color, BLUE_GLASS.rgb);
        assert!((blue_appearance.transparency - 0.5).abs() < 0.02);
        assert!(
            matches!(blue_face.key, FaceKey::Imported { feature: f, .. } if f == feature),
            "{blue_face}"
        );
        blue_face.clone()
    };
    match part.feature(feature).unwrap().kind() {
        FeatureKind::Import(import) => assert!(!import.apply_colors, "applied only once"),
        other => panic!("{other:?}"),
    }
    // Removing the body's appearance and rebuilding does not bring it back.
    part.appearances_mut().set_body(body_id, None);
    part.release(&mut kernel);
    part.rebuild(&mut kernel);
    assert!(part.appearances().body(body_id).is_none());
    part.appearances_mut()
        .set_body(body_id, Some(Appearance::colored(RED.rgb)));

    // ---- Saved and reopened: the appearances and the done flag survive.
    let mut file = PartFile::new();
    file.definition = part.definition();
    let archive = write_part(Cursor::new(Vec::new()), &file).unwrap().into_inner();
    let back = read_part(Cursor::new(&archive)).unwrap();
    assert_eq!(back.definition.appearances, part.appearances().clone());
    assert!(back.definition.appearances.face(&blue_face).is_some());
    match back.definition.features[0].kind() {
        FeatureKind::Import(import) => assert!(!import.apply_colors),
        other => panic!("{other:?}"),
    }

    // ---- Out again: a material gives the body its colour, the overrides go with it.
    let green = Material::new("Green plastic", "Plastic", Appearance::colored([60, 160, 70]), 1040.0);
    part.set_material(Some(green));
    part.appearances_mut().set_body(body_id, None);
    let body = part.body().unwrap();
    let colors = part.step_colors(body);
    assert_eq!(colors.len(), 1);
    assert!(close(colors[0].solids[0], Some(StepColor { rgb: [60, 160, 70], transparency: 0.0 })));
    assert_eq!(colors[0].faces.iter().filter(|c| c.is_some()).count(), 1);
    let path = temp_step("colours-out");
    kernel.export_step(&body.shapes(), &colors, &path).unwrap();
    let again = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let reread = kernel.import_step(&again).unwrap();
    assert!(close(
        reread.colors.solids[0],
        Some(StepColor { rgb: [60, 160, 70], transparency: 0.0 })
    ));
    assert_eq!(
        reread.colors.faces.iter().filter(|c| close(**c, Some(BLUE_GLASS))).count(),
        1,
        "{:?}",
        reread.colors.faces
    );
    kernel.release(reread.shape);
    part.release(&mut kernel);
}
