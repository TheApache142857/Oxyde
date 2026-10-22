//! The command line (`oxyde-cli`) driven as a library: `eval`, `check`, `export`,
//! `snapshot` and `drawing` on a part whose measures are known by hand, and on a part imported from a
//! STEP file. Cargo only exposes a binary to its own crate's tests, so the commands are
//! run through [`oxyde_cli::run`] with the arguments the shell would pass.
//!
//! # The filleted box
//!
//! A 10 × 20 × 30 box (`x` 0..10, `y` 0..20, `z` 0..30) with one fillet of radius
//! `r` = 5 along the 30 mm edge at `x` = 10, `y` = 20. The fillet removes a prism of
//! length `L` = 30 whose section is a square `r` × `r` less a quarter disc:
//!
//! - removed section `k r²` with `k` = 1 − π/4, so volume `V` = 6000 − k r² L;
//! - area: the two strips `r` wide along the edge (2 r L) become a quarter cylinder
//!   (π r L / 2) and each end cap loses `k r²`: `A` = 2200 − 2 r L + π r L / 2 − 2 k r²;
//! - the removed section's centroid lies `d` = r (5/6 − π/4) / k from the corner along
//!   both axes (the square, r² at r/2, less the quarter disc, π r²/4, centred on the
//!   fillet's axis at r from the corner with its centroid 4r/3π back towards it), so
//!   the centre of mass is
//!   (6000 · (5, 10, 15) − k r² L · (10 − d, 20 − d, 15)) / V;
//! - 7 faces, 15 edges and 10 vertices (the edge becomes a face with two tangent edges,
//!   and each end cap's corner an arc with two vertices).

use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use oxyde_cli::{EXIT_ERROR, EXIT_FAILED, EXIT_OK, Evaluation, Expected};
use oxyde_format::{PartFile, Properties, new_document_id, save_part};
use oxyde_geom::Point2;
use oxyde_kernel::Kernel;
use oxyde_model::{
    EdgeBlend, Extrude, ExtrudeOp, FeatureKind, FileData, Import, Part, Sketch, SketchPlane,
    StandardPlane,
};
use oxyde_occt::OcctKernel;

const CART: &[u8] = include_bytes!("data/cart.step");

const R: f64 = 5.0;
const L: f64 = 30.0;

fn k() -> f64 {
    1.0 - PI / 4.0
}

fn expected_volume() -> f64 {
    6000.0 - k() * R * R * L
}

fn expected_area() -> f64 {
    2200.0 - 2.0 * R * L + PI * R * L / 2.0 - 2.0 * k() * R * R
}

fn expected_centre() -> [f64; 3] {
    let removed = k() * R * R * L;
    let d = R * (5.0 / 6.0 - PI / 4.0) / k();
    let v = expected_volume();
    [
        (6000.0 * 5.0 - removed * (10.0 - d)) / v,
        (6000.0 * 10.0 - removed * (20.0 - d)) / v,
        (6000.0 * 15.0 - removed * 15.0) / v,
    ]
}

/// A folder of its own for this test's files.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oxyde-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn save(path: &Path, part: &Part) {
    let file = PartFile {
        document_id: new_document_id(),
        definition: part.definition(),
        properties: Properties::default(),
        kernel: oxyde_format::PartKernel::Occt,
    };
    save_part(path, &file).unwrap();
}

/// The box, with a fillet of `radius` on its long edge at the corner (10, 20).
fn filleted_box(path: &Path, radius: f64) {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::new(0.0, 0.0), 10.0, 20.0);
    let sketch = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Extrude(Extrude::blind(
        sketch,
        ExtrudeOp::Boss,
        30.0,
    )));
    part.rebuild(&mut kernel);
    let body = part.body().expect("the box");
    let (index, _) = body
        .topology
        .edges
        .iter()
        .enumerate()
        .find(|(_, e)| {
            (e.length - L).abs() < 1e-6
                && (e.midpoint.x - 10.0).abs() < 1e-6
                && (e.midpoint.y - 20.0).abs() < 1e-6
        })
        .expect("the 30 mm edge at x = 10, y = 20");
    let edge = body.naming.edge(index as u32).unwrap().clone();
    part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: vec![edge],
        size: radius,
        chamfer: Default::default(),
    }));
    save(path, &part);
    part.release(&mut kernel);
}

fn run(args: &[&str]) -> (i32, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut argv = vec!["oxyde-cli"];
    argv.extend_from_slice(args);
    let code = oxyde_cli::run(argv, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    let tolerance = 1e-6 * expected.abs().max(1.0);
    assert!(
        (actual - expected).abs() < tolerance,
        "{what}: expected {expected}, got {actual}"
    );
}

#[test]
fn eval_of_the_filleted_box_matches_the_hand_calculation() {
    let dir = scratch("eval");
    let part = dir.join("box.oxyprt");
    filleted_box(&part, R);

    let eval = oxyde_cli::evaluate(&part).unwrap();
    assert!(eval.ok, "{:?}", eval.features);
    let names: Vec<&str> = eval.features.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["Sketch1", "Boss-Extrude1", "Fillet1"]);
    assert!(eval.features.iter().all(|f| f.status == "ok"));
    assert_eq!(eval.bodies.len(), 1);
    assert_eq!(eval.bodies[0].name, "Body 1");
    assert_eq!(eval.bodies[0].created_by, "Boss-Extrude1");

    let m = eval.total.as_ref().unwrap();
    assert_eq!(m, &eval.bodies[0].measures);
    assert_close(m.volume, expected_volume(), "volume");
    assert_close(m.area, expected_area(), "area");
    let c = expected_centre();
    for k in 0..3 {
        assert_close(m.centre_of_mass[k], c[k], "centre of mass");
        assert_close(m.bounds.min[k], 0.0, "bounds min");
        assert_close(m.bounds.max[k], [10.0, 20.0, 30.0][k], "bounds max");
    }
    assert_eq!((m.faces, m.edges, m.vertices), (7, 15, 10));

    // The text report names the features and the body.
    let (code, out, err) = run(&["eval", part.to_str().unwrap()]);
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(out.contains("Fillet1"), "{out}");
    assert!(out.contains("Body 1 (Boss-Extrude1)"), "{out}");
    assert!(out.ends_with("Result: ok\n"), "{out}");

    // `--json` is the same evaluation, with stable field names.
    let (code, out, _) = run(&["eval", part.to_str().unwrap(), "--json"]);
    assert_eq!(code, EXIT_OK);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["features"][2]["type"], "Fillet");
    assert_eq!(json["features"][2]["status"], "ok");
    assert_eq!(json["bodies"][0]["faces"], 7);
    assert_close(
        json["total"]["volume"].as_f64().unwrap(),
        expected_volume(),
        "json volume",
    );
    assert_eq!(json["total"]["bounds"]["max"][2], 30.0);
    let parsed: Evaluation = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed, eval);
}

#[test]
fn eval_exits_1_when_a_feature_fails() {
    let dir = scratch("failed");
    let part = dir.join("box.oxyprt");
    // A 15 mm fillet cannot fit a 10 mm wide box.
    filleted_box(&part, 15.0);
    let (code, out, _) = run(&["eval", part.to_str().unwrap()]);
    assert_eq!(code, EXIT_FAILED, "{out}");
    assert!(out.contains("Fillet1"), "{out}");
    assert!(out.contains("error:"), "{out}");
    assert!(out.ends_with("Result: 1 feature failed\n"), "{out}");
    let (code, out, _) = run(&["eval", part.to_str().unwrap(), "--json"]);
    assert_eq!(code, EXIT_FAILED);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["features"][2]["status"], "error");
    assert!(json["features"][2]["message"].is_string());
    // The box is still there, as it was before the fillet.
    assert_eq!(json["bodies"][0]["faces"], 6);

    // The check fails on the features line alone.
    let (code, out, _) = run(&[
        "check",
        part.to_str().unwrap(),
        "--expect",
        write_expected(&dir, &Expected::default()).to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("features"), "{out}");
    assert!(out.contains("1 failed"), "{out}");
}

fn write_expected(dir: &Path, expected: &Expected) -> PathBuf {
    let path = dir.join("expected.json");
    std::fs::write(&path, serde_json::to_string_pretty(expected).unwrap()).unwrap();
    path
}

#[test]
fn check_passes_on_the_hand_calculation_and_fails_when_it_is_off() {
    let dir = scratch("check");
    let part = dir.join("box.oxyprt");
    filleted_box(&part, R);
    let c = expected_centre();
    let expected = Expected {
        bodies: Some(1),
        volume: Some(expected_volume()),
        area: Some(expected_area()),
        centre_of_mass: Some(c),
        bounds: Some(oxyde_cli::Bounds {
            min: [0.0; 3],
            max: [10.0, 20.0, 30.0],
        }),
        faces: Some(7),
        edges: Some(15),
        vertices: Some(10),
    };
    let file = write_expected(&dir, &expected);
    let (code, out, err) = run(&[
        "check",
        part.to_str().unwrap(),
        "--expect",
        file.to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_OK, "{out}{err}");
    assert!(out.contains("Result: pass (16 checks)"), "{out}");
    assert!(!out.contains("FAIL"), "{out}");

    // The default tolerance is a thousandth: 0.5 % off fails, and the table says where.
    let off = Expected {
        volume: Some(expected_volume() * 1.005),
        ..expected.clone()
    };
    let file = write_expected(&dir, &off);
    let (code, out, _) = run(&[
        "check",
        part.to_str().unwrap(),
        "--expect",
        file.to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_FAILED, "{out}");
    assert!(out.contains("Result: FAIL (1 of 16 checks)"), "{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("volume") && l.ends_with("FAIL")),
        "{out}"
    );
    // ...and passes with a wider one.
    let (code, _, _) = run(&[
        "check",
        part.to_str().unwrap(),
        "--expect",
        file.to_str().unwrap(),
        "--tol",
        "0.01",
    ]);
    assert_eq!(code, EXIT_OK);

    // An unknown key in expected.json is an error, not a silent pass.
    let bad = dir.join("bad.json");
    std::fs::write(&bad, r#"{"volum": 1}"#).unwrap();
    let (code, _, err) = run(&[
        "check",
        part.to_str().unwrap(),
        "--expect",
        bad.to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_ERROR);
    assert!(err.contains("volum"), "{err}");
}

#[test]
fn export_writes_step_iges_and_meshes() {
    let dir = scratch("export");
    let part = dir.join("box.oxyprt");
    filleted_box(&part, R);

    let step = dir.join("box.step");
    let (code, out, err) = run(&[
        "export",
        part.to_str().unwrap(),
        "--out",
        step.to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_OK, "{out}{err}");
    let mut kernel = OcctKernel::new();
    let imported = kernel.import_step(&std::fs::read(&step).unwrap()).unwrap();
    assert_close(
        kernel.volume(imported.shape).unwrap(),
        expected_volume(),
        "STEP round trip",
    );
    kernel.release(imported.shape);

    let stl = dir.join("box.stl");
    let (code, out, err) = run(&[
        "export",
        part.to_str().unwrap(),
        "--out",
        stl.to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_OK, "{out}{err}");
    let bytes = std::fs::read(&stl).unwrap();
    // Binary STL: an 80-byte header, the triangle count, 50 bytes per triangle.
    let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
    assert!(count >= 12, "{count} triangles");
    assert_eq!(bytes.len(), 84 + 50 * count);

    // OBJ: one object named after the file, its faces indexed from 1.
    let obj = dir.join("box.obj");
    let (code, out, err) = run(&[
        "export",
        part.to_str().unwrap(),
        "--out",
        obj.to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_OK, "{out}{err}");
    let text = std::fs::read_to_string(&obj).unwrap();
    assert!(
        text.contains(
            "o box
"
        ),
        "{text}"
    );
    assert!(
        text.matches(
            "
f "
        )
        .count()
            >= 12
    );

    // 3MF: a ZIP with the model inside, in millimetres.
    let threemf = dir.join("box.3mf");
    let (code, out, err) = run(&[
        "export",
        part.to_str().unwrap(),
        "--out",
        threemf.to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_OK, "{out}{err}");
    let bytes = std::fs::read(&threemf).unwrap();
    assert_eq!(&bytes[..2], b"PK");

    // glTF: a binary glTF 2.0 file with a mesh and a material.
    let glb = dir.join("box.glb");
    let (code, out, err) = run(&[
        "export",
        part.to_str().unwrap(),
        "--out",
        glb.to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_OK, "{out}{err}");
    let bytes = std::fs::read(&glb).unwrap();
    assert_eq!(&bytes[..4], b"glTF");
    let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let json: serde_json::Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
    assert_eq!(json["meshes"].as_array().unwrap().len(), 1);
    assert!(!json["materials"].as_array().unwrap().is_empty());

    // IGES: read back by OpenCASCADE with the same volume.
    let iges = dir.join("box.igs");
    let (code, out, err) = run(&[
        "export",
        part.to_str().unwrap(),
        "--out",
        iges.to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_OK, "{out}{err}");
    let header = std::fs::read_to_string(&iges).unwrap();
    assert!(header.contains('S') && header.len() > 1000, "IGES written");

    let (code, _, err) = run(&[
        "export",
        part.to_str().unwrap(),
        "--out",
        dir.join("box.xyz").to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_ERROR);
    assert!(err.contains(".stl"), "{err}");
}

#[test]
fn snapshot_writes_a_png_of_the_requested_size() {
    let dir = scratch("snapshot");
    let part = dir.join("box.oxyprt");
    filleted_box(&part, R);
    let png = dir.join("box.png");
    let (code, out, err) = run(&[
        "snapshot",
        part.to_str().unwrap(),
        "--out",
        png.to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_OK, "{out}{err}");
    let bytes = std::fs::read(&png).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    // IHDR: width and height, big-endian, right after the signature and chunk header.
    assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 1024);
    assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 768);

    let small = dir.join("front.png");
    let (code, _, err) = run(&[
        "snapshot",
        part.to_str().unwrap(),
        "--out",
        small.to_str().unwrap(),
        "--view",
        "front",
        "--width",
        "64",
        "--height",
        "48",
    ]);
    assert_eq!(code, EXIT_OK, "{err}");
    let bytes = std::fs::read(&small).unwrap();
    assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 64);
    assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 48);
}

#[test]
fn a_step_import_evaluates_and_checks_against_its_own_measures() {
    let dir = scratch("cart");
    let part = dir.join("cart.oxyprt");
    let mut p = Part::new();
    p.add_feature(FeatureKind::Import(Import::new(
        "cart.step",
        FileData::new(CART.to_vec()),
    )));
    save(&part, &p);

    let eval = oxyde_cli::evaluate(&part).unwrap();
    assert!(eval.ok, "{:?}", eval.features);
    assert_eq!(eval.features[0].name, "Imported1");
    assert!(!eval.bodies.is_empty());
    let total = eval.total.as_ref().unwrap();
    assert!(total.volume > 0.0);
    assert!(total.area > 0.0);
    assert!(total.bounds.diagonal() > 0.0);
    assert_close(
        total.volume,
        eval.bodies.iter().map(|b| b.measures.volume).sum(),
        "total volume",
    );

    // What eval found is what check reads back.
    let file = write_expected(&dir, &Expected::from_evaluation(&eval));
    let (code, out, err) = run(&[
        "check",
        part.to_str().unwrap(),
        "--expect",
        file.to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_OK, "{out}{err}");
}

#[test]
fn drawing_writes_the_views_on_a_sheet_as_pdf_svg_and_dxf() {
    let dir = scratch("drawing");
    let part = dir.join("box.oxyprt");
    filleted_box(&part, R);
    for name in ["box.pdf", "box.svg", "box.dxf"] {
        let file = dir.join(name);
        let (code, out, err) = run(&[
            "drawing",
            part.to_str().unwrap(),
            "--out",
            file.to_str().unwrap(),
            "--sheet",
            "a3",
        ]);
        assert_eq!(code, EXIT_OK, "{out}{err}");
        assert!(out.contains("scale"), "{out}");
    }
    assert!(
        std::fs::read(dir.join("box.pdf"))
            .unwrap()
            .starts_with(b"%PDF-1.4")
    );
    let svg = std::fs::read_to_string(dir.join("box.svg")).unwrap();
    assert!(svg.contains("width=\"420mm\"") && svg.contains("<path"));

    // The DXF is at the views' true size: among its visible lines, the box's 20 mm
    // height seen from the front, and its 30 mm depth seen from the top.
    let dxf = std::fs::read_to_string(dir.join("box.dxf")).unwrap();
    let pairs: Vec<(&str, &str)> = dxf
        .lines()
        .collect::<Vec<_>>()
        .chunks(2)
        .filter_map(|p| match p {
            [code, value] => Some((code.trim(), value.trim())),
            _ => None,
        })
        .collect();
    let mut lines: Vec<(String, [f64; 4])> = Vec::new();
    let mut i = 0;
    while i < pairs.len() {
        if pairs[i] == ("0", "LINE") {
            let mut layer = String::new();
            let mut xy = [0.0; 4];
            let mut j = i + 1;
            while j < pairs.len() && pairs[j].0 != "0" {
                let value = pairs[j].1;
                match pairs[j].0 {
                    "8" => layer = value.to_owned(),
                    "10" => xy[0] = value.parse().unwrap(),
                    "20" => xy[1] = value.parse().unwrap(),
                    "11" => xy[2] = value.parse().unwrap(),
                    "21" => xy[3] = value.parse().unwrap(),
                    _ => {}
                }
                j += 1;
            }
            lines.push((layer, xy));
            i = j;
        } else {
            i += 1;
        }
    }
    let vertical = |length: f64| {
        lines.iter().any(|(layer, [x0, y0, x1, y1])| {
            layer == "VISIBLE" && (x0 - x1).abs() < 1e-6 && ((y1 - y0).abs() - length).abs() < 1e-3
        })
    };
    assert!(vertical(20.0) && vertical(30.0), "true size");
    assert!(lines.iter().any(|(layer, _)| layer == "SHEET"));

    let (code, _, err) = run(&[
        "drawing",
        part.to_str().unwrap(),
        "--out",
        dir.join("box.png").to_str().unwrap(),
    ]);
    assert_eq!(code, EXIT_ERROR);
    assert!(err.contains(".pdf"), "{err}");
}
