//! The modeling engine driving OpenCASCADE: timelines are rebuilt after upstream edits and
//! every feature must stay attached to the same geometry.

use std::f64::consts::PI;

use oxyde_geom::Point2;
use oxyde_kernel::Kernel;
use oxyde_model::{
    EdgeBlend, EdgeName, Extrude, ExtrudeExtent, ExtrudeOp, FaceName, FeatureId, FeatureKind,
    FeatureStatus, Part, Sketch, SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;

// Rectangle sketches drawn with `Sketch::rectangle` have these entity ids.
const BOTTOM: u32 = 1;
const RIGHT: u32 = 2;
const LEFT: u32 = 4;

fn rectangle_on_front(width: f64, height: f64) -> FeatureKind {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::ORIGIN, width, height);
    FeatureKind::Sketch(sketch)
}

fn boss(sketch: FeatureId, depth: f64) -> FeatureKind {
    FeatureKind::Extrude(Extrude::blind(sketch, ExtrudeOp::Boss, depth))
}

fn cut(sketch: FeatureId, depth: f64) -> FeatureKind {
    FeatureKind::Extrude(Extrude::blind(sketch, ExtrudeOp::Cut, depth))
}

fn fillet(edges: Vec<EdgeName>, radius: f64) -> FeatureKind {
    FeatureKind::Fillet(EdgeBlend {
        edges,
        size: radius,
    })
}

fn set_depth(part: &mut Part, extrude: FeatureId, depth: f64) {
    match part.edit(extrude).unwrap() {
        FeatureKind::Extrude(e) => e.extent = ExtrudeExtent::Blind { depth },
        other => panic!("not an extrusion: {other:?}"),
    }
}

fn assert_all_ok(part: &Part) {
    for entry in part.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{}", entry.name);
    }
}

fn volume(part: &Part, kernel: &OcctKernel) -> f64 {
    kernel
        .volume(part.body().expect("part has a body").shape)
        .unwrap()
}

fn assert_close(actual: f64, expected: f64) {
    let tolerance = 1e-4 * expected.abs().max(1.0);
    assert!(
        (actual - expected).abs() < tolerance,
        "expected {expected}, got {actual}"
    );
}

/// Material removed by a fillet of radius `r` along a straight 90° edge of length `length`.
fn fillet_loss(r: f64, length: f64) -> f64 {
    r * r * (1.0 - PI / 4.0) * length
}

#[test]
fn fillet_stays_on_its_edge_through_upstream_edits() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();

    let sketch = part.add_feature(rectangle_on_front(40.0, 20.0));
    let base = part.add_feature(boss(sketch, 10.0));
    let edge = EdgeName::between(FaceName::profile(base, RIGHT), FaceName::end_cap(base));
    let round = part.add_feature(fillet(vec![edge.clone()], 2.0));

    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(
        volume(&part, &kernel),
        40.0 * 20.0 * 10.0 - fillet_loss(2.0, 20.0),
    );
    let blend_face = FaceName::from_edge(round, edge.clone());
    assert!(part.body().unwrap().naming.find_face(&blend_face).is_some());

    // Deeper extrusion: different topology numbering, same edge.
    set_depth(&mut part, base, 25.0);
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(
        volume(&part, &kernel),
        40.0 * 20.0 * 25.0 - fillet_loss(2.0, 20.0),
    );

    // Wider sketch: the right side moves, the fillet follows it.
    *part.edit(sketch).unwrap() = rectangle_on_front(60.0, 20.0);
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(
        volume(&part, &kernel),
        60.0 * 20.0 * 25.0 - fillet_loss(2.0, 20.0),
    );
    assert!(part.body().unwrap().naming.find_face(&blend_face).is_some());

    // The result exports cleanly.
    let path = std::env::temp_dir().join(format!("oxyde-fillet-{}.step", std::process::id()));
    kernel
        .export_step(&[part.body().unwrap().shape], &path)
        .unwrap();
    let _ = std::fs::remove_file(&path);

    part.release(&mut kernel);
}

#[test]
fn sketch_on_a_face_follows_that_face() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();

    let s1 = part.add_feature(rectangle_on_front(40.0, 20.0));
    let base = part.add_feature(boss(s1, 10.0));

    let mut on_top = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    let circle = on_top.circle(Point2::new(20.0, 10.0), 3.0);
    let s2 = part.add_feature(FeatureKind::Sketch(on_top));
    let hole = part.add_feature(cut(s2, 5.0));

    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let hole_volume = PI * 3.0 * 3.0 * 5.0;
    assert_close(volume(&part, &kernel), 40.0 * 20.0 * 10.0 - hole_volume);

    // Fillet the rim of the hole: the edge between the hole wall and the top face.
    let rim = EdgeName::between(FaceName::profile(hole, circle), FaceName::end_cap(base));
    part.add_feature(fillet(vec![rim], 1.0));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let with_rim_fillet = volume(&part, &kernel);
    assert!(with_rim_fillet < 40.0 * 20.0 * 10.0 - hole_volume);

    // Thicker base: the top face moves up, the sketch, the hole and its fillet follow.
    set_depth(&mut part, base, 25.0);
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), with_rim_fillet + 40.0 * 20.0 * 15.0);

    part.release(&mut kernel);
}

#[test]
fn boss_on_top_merges_into_one_body() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();

    let s1 = part.add_feature(rectangle_on_front(40.0, 20.0));
    let base = part.add_feature(boss(s1, 10.0));
    let mut on_top = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    on_top.rectangle(Point2::new(10.0, 5.0), 10.0, 10.0);
    let s2 = part.add_feature(FeatureKind::Sketch(on_top));
    part.add_feature(boss(s2, 5.0));

    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 8000.0 + 500.0);
    let counts = kernel.topology_counts(part.body().unwrap().shape).unwrap();
    assert_eq!(counts.solids, 1);

    part.release(&mut kernel);
}

#[test]
fn suppression_and_rollback() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();

    let s1 = part.add_feature(rectangle_on_front(40.0, 20.0));
    let base = part.add_feature(boss(s1, 10.0));
    let mut on_top = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    on_top.rectangle(Point2::new(10.0, 5.0), 10.0, 10.0);
    let s2 = part.add_feature(FeatureKind::Sketch(on_top));
    let top = part.add_feature(boss(s2, 5.0));

    part.rebuild(&mut kernel);
    assert_close(volume(&part, &kernel), 8500.0);

    part.set_suppressed(top, true).unwrap();
    part.rebuild(&mut kernel);
    assert_eq!(part.status(top), Some(FeatureStatus::Suppressed));
    assert_close(volume(&part, &kernel), 8000.0);

    part.set_suppressed(top, false).unwrap();
    part.roll_back_to(Some(s2)).unwrap();
    part.rebuild(&mut kernel);
    assert_eq!(part.status(s2), Some(FeatureStatus::RolledBack));
    assert_eq!(part.status(top), Some(FeatureStatus::RolledBack));
    assert_close(volume(&part, &kernel), 8000.0);

    part.roll_back_to(None).unwrap();
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 8500.0);

    part.release(&mut kernel);
}

#[test]
fn lost_reference_is_reported_and_the_rest_still_builds() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();

    let s1 = part.add_feature(rectangle_on_front(40.0, 20.0));
    let base = part.add_feature(boss(s1, 10.0));
    // The left and right sides never touch: no such edge.
    let bogus = EdgeName::between(
        FaceName::profile(base, LEFT),
        FaceName::profile(base, RIGHT),
    );
    let broken = part.add_feature(fillet(vec![bogus], 1.0));
    let good = EdgeName::between(FaceName::profile(base, BOTTOM), FaceName::end_cap(base));
    let bevel = part.add_feature(FeatureKind::Chamfer(EdgeBlend {
        edges: vec![good],
        size: 2.0,
    }));

    part.rebuild(&mut kernel);
    assert!(matches!(part.status(broken), Some(FeatureStatus::Error(_))));
    assert_eq!(part.status(bevel), Some(FeatureStatus::Ok));
    // Chamfer of distance d on a 90° edge of length 40 removes d²/2 × 40.
    assert_close(volume(&part, &kernel), 8000.0 - 2.0 * 2.0 / 2.0 * 40.0);

    part.release(&mut kernel);
}

#[test]
fn profile_with_a_hole_and_separate_regions() {
    let mut kernel = OcctKernel::new();

    let mut part = Part::new();
    let mut plate = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    plate.rectangle(Point2::ORIGIN, 40.0, 20.0);
    plate.circle(Point2::new(20.0, 10.0), 3.0);
    let s1 = part.add_feature(FeatureKind::Sketch(plate));
    part.add_feature(boss(s1, 10.0));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 8000.0 - PI * 9.0 * 10.0);
    part.release(&mut kernel);

    let mut part = Part::new();
    let mut blocks = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    blocks.rectangle(Point2::ORIGIN, 10.0, 10.0);
    blocks.rectangle(Point2::new(20.0, 0.0), 10.0, 10.0);
    let s1 = part.add_feature(FeatureKind::Sketch(blocks));
    part.add_feature(boss(s1, 5.0));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 2.0 * 500.0);
    let counts = kernel.topology_counts(part.body().unwrap().shape).unwrap();
    assert_eq!(counts.solids, 2);
    part.release(&mut kernel);
}

#[test]
fn reversed_and_midplane_extrusions_have_positive_volume() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let s1 = part.add_feature(rectangle_on_front(10.0, 10.0));
    let e1 = part.add_feature(boss(s1, 4.0));
    if let FeatureKind::Extrude(e) = part.edit(e1).unwrap() {
        e.reverse = true;
    }
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 400.0);

    if let FeatureKind::Extrude(e) = part.edit(e1).unwrap() {
        e.reverse = false;
        e.extent = ExtrudeExtent::MidPlane { depth: 6.0 };
    }
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 600.0);
    part.release(&mut kernel);
}
