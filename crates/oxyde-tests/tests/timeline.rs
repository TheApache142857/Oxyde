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
use oxyde_sketch::RelationId;

// In a new sketch, `Sketch::rectangle` creates its four corner points first (ids 1–4),
// then its bottom, right, top and left lines (ids 5–8).
const BOTTOM: u32 = 5;
const RIGHT: u32 = 6;
const LEFT: u32 = 8;

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
        .volume(part.body().expect("part has a body").solids[0].shape)
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
        .export_step(&[part.body().unwrap().solids[0].shape], &[], &[], &path)
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
    let counts = kernel
        .topology_counts(part.body().unwrap().solids[0].shape)
        .unwrap();
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
    let counts = kernel
        .topology_counts(part.body().unwrap().solids[0].shape)
        .unwrap();
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

/// A 40 × 20 rectangle fully defined by a fixed corner and two dimensions. Returns the sketch,
/// the line ids (bottom, right, top, left) and the ids of the width and height dimensions
/// (named D1 and D2).
fn dimensioned_rectangle() -> (Sketch, [u32; 4], RelationId, RelationId) {
    use oxyde_sketch::{Constraint, EntityId};
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let lines = sketch.rectangle(Point2::new(0.5, 0.5), 37.0, 18.0);
    let corner = sketch.geometry.endpoints(EntityId(lines[0])).unwrap().0;
    sketch.geometry.move_point(corner, Point2::ORIGIN);
    sketch.geometry.constrain(Constraint::Fixed(corner));
    let width = sketch
        .geometry
        .constrain(Constraint::Length(EntityId(lines[0]), 40.0));
    let height = sketch
        .geometry
        .constrain(Constraint::Length(EntityId(lines[1]), 20.0));
    (sketch, lines, width, height)
}

#[test]
fn sketch_dimensions_drive_the_part() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();

    let (sketch, lines, width, _) = dimensioned_rectangle();
    let s1 = part.add_feature(FeatureKind::Sketch(sketch));
    let base = part.add_feature(boss(s1, 10.0));
    let edge = EdgeName::between(FaceName::profile(base, lines[1]), FaceName::end_cap(base));
    part.add_feature(fillet(vec![edge], 2.0));

    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    // Fully defined: no "(-)" in the timeline.
    assert!(!part.timeline()[0].under_defined);
    assert_close(volume(&part, &kernel), 8000.0 - fillet_loss(2.0, 20.0));

    // Change the width dimension: the solver moves the right side, the fillet follows.
    if let FeatureKind::Sketch(s) = part.edit(s1).unwrap() {
        assert!(s.geometry.set_dimension(width, 60.0));
    }
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 12000.0 - fillet_loss(2.0, 20.0));

    // The same through SolidWorks-style names, as equations will: D1@Sketch1 is the width,
    // D1@Boss-Extrude1 the depth.
    let width = part.find_dimension("D1@Sketch1").unwrap();
    let depth = part.find_dimension("D1@Boss-Extrude1").unwrap();
    part.set_dimension(width, 50.0).unwrap();
    part.set_dimension(depth, 5.0).unwrap();
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 5000.0 - fillet_loss(2.0, 20.0));
    part.release(&mut kernel);
}

#[test]
fn a_cut_that_misses_the_body_is_reported() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let s1 = part.add_feature(rectangle_on_front(40.0, 20.0));
    let base = part.add_feature(boss(s1, 10.0));
    // A cut sketched on the Front plane goes against its normal, away from the boss:
    // it removes nothing, which the feature says rather than keeping quiet.
    let mut hole = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    hole.circle(Point2::new(20.0, 10.0), 3.0);
    let s2 = part.add_feature(FeatureKind::Sketch(hole));
    let miss = part.add_feature(cut(s2, 5.0));
    part.rebuild(&mut kernel);
    match part.status(miss) {
        Some(FeatureStatus::Warning(message)) => assert!(message.contains("removed nothing")),
        other => panic!("expected a warning, got {other:?}"),
    }
    assert_close(volume(&part, &kernel), 40.0 * 20.0 * 10.0);
    // Reversed, it cuts the hole and the warning goes.
    match part.edit(miss).unwrap() {
        FeatureKind::Extrude(e) => e.reverse = true,
        other => panic!("not an extrusion: {other:?}"),
    }
    part.rebuild(&mut kernel);
    assert_eq!(part.status(miss), Some(FeatureStatus::Ok));
    assert_close(volume(&part, &kernel), 40.0 * 20.0 * 10.0 - PI * 9.0 * 5.0);
    // An intersection with nothing in common would make the body vanish: refused, the
    // body kept.
    let mut away = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    away.rectangle(Point2::new(100.0, 100.0), 10.0, 10.0);
    let s3 = part.add_feature(FeatureKind::Sketch(away));
    let empty = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s3,
        ExtrudeOp::Intersect,
        5.0,
    )));
    part.rebuild(&mut kernel);
    match part.status(empty) {
        Some(FeatureStatus::Error(message)) => assert!(message.contains("nothing in common")),
        other => panic!("expected an error, got {other:?}"),
    }
    assert_eq!(part.status(base), Some(FeatureStatus::Ok));
    assert_close(volume(&part, &kernel), 40.0 * 20.0 * 10.0 - PI * 9.0 * 5.0);
    part.release(&mut kernel);
}

#[test]
fn under_and_over_defined_sketches() {
    use oxyde_sketch::{Constraint, EntityId};
    let mut kernel = OcctKernel::new();

    // Without dimensions the rectangle can still move and stretch.
    let mut part = Part::new();
    let s1 = part.add_feature(rectangle_on_front(40.0, 20.0));
    part.add_feature(boss(s1, 10.0));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert!(part.timeline()[0].under_defined);
    part.release(&mut kernel);

    // Two contradictory widths: the sketch fails, and so does the extrusion that uses it.
    let mut part = Part::new();
    let (mut sketch, lines, _, _) = dimensioned_rectangle();
    sketch
        .geometry
        .constrain(Constraint::Length(EntityId(lines[2]), 55.0));
    let s1 = part.add_feature(FeatureKind::Sketch(sketch));
    let base = part.add_feature(boss(s1, 10.0));
    part.rebuild(&mut kernel);
    assert!(matches!(part.status(s1), Some(FeatureStatus::Error(_))));
    assert!(matches!(part.status(base), Some(FeatureStatus::Error(_))));
    assert!(part.body().is_none());
    // The sketch is still there to be looked at and repaired: its plane and its
    // geometry as stored, which is what the editor opens.
    let kept = part
        .sketch_result(s1)
        .expect("an over-defined sketch keeps its result");
    assert_eq!(kept.plane, StandardPlane::Front.plane());
    assert_eq!(kept.solved.entities().len(), 8);
    part.release(&mut kernel);
}
