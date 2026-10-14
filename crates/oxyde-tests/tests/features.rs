//! Revolve, shell, reference geometry, patterns, mirror and equations, built by
//! OpenCASCADE. Volumes are checked against hand calculations, and persistent names of the
//! new faces (offset walls, pattern instances) are checked to resolve.

use std::f64::consts::{PI, TAU};

use oxyde_geom::Point2;
use oxyde_kernel::Kernel;
use oxyde_model::{
    AxisRef, CircularPattern, EdgeBlend, EdgeName, Extrude, ExtrudeOp, FaceKey, FaceName,
    FeatureId, FeatureKind, FeatureStatus, LinearPattern, Mirror, Part, PatternDirection, PlaneRef,
    RefAxis, RefPlane, Revolve, RevolveAxis, Shell, Sketch, SketchPlane, StandardAxis,
    StandardPlane,
};
use oxyde_occt::OcctKernel;

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

fn boss(sketch: FeatureId, depth: f64) -> FeatureKind {
    FeatureKind::Extrude(Extrude::blind(sketch, ExtrudeOp::Boss, depth))
}

fn cut(sketch: FeatureId, depth: f64) -> FeatureKind {
    FeatureKind::Extrude(Extrude::blind(sketch, ExtrudeOp::Cut, depth))
}

/// A block on the Front plane: `width` × `height` from `corner`, `depth` along +Z.
fn block(part: &mut Part, corner: Point2, width: f64, height: f64, depth: f64) -> FeatureId {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(corner, width, height);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(boss(s, depth))
}

/// A sketch with a construction centerline along the Y axis, then a rectangle `x0..x1` ×
/// `y0..y1`. Returns the sketch and the centerline id.
fn revolve_profile(x0: f64, x1: f64, y0: f64, y1: f64) -> (Sketch, u32) {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let axis = sketch
        .geometry
        .line(Point2::new(0.0, -5.0), Point2::new(0.0, 25.0));
    sketch.geometry.set_construction(axis, true);
    sketch.rectangle(Point2::new(x0, y0), x1 - x0, y1 - y0);
    (sketch, axis.0)
}

fn revolve(sketch: FeatureId, op: ExtrudeOp, axis: RevolveAxis, angle: f64) -> FeatureKind {
    FeatureKind::Revolve(Revolve {
        sketch,
        op,
        axis,
        angle,
        mid_plane: false,
        reverse: false,
    })
}

#[test]
fn revolving_a_rectangle_makes_a_tube() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (profile, axis) = revolve_profile(5.0, 10.0, 0.0, 20.0);
    let s1 = part.add_feature(FeatureKind::Sketch(profile));
    let tube = part.add_feature(revolve(
        s1,
        ExtrudeOp::Boss,
        RevolveAxis::SketchLine(axis),
        TAU,
    ));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let full = PI * (100.0 - 25.0) * 20.0;
    assert_close(volume(&part, &kernel), full);

    // Half a turn, centred on the sketch plane, about the model Y axis this time.
    match part.edit(tube).unwrap() {
        FeatureKind::Revolve(r) => {
            r.angle = PI;
            r.mid_plane = true;
            r.axis = RevolveAxis::Axis(AxisRef::Standard(StandardAxis::Y));
        }
        _ => unreachable!(),
    }
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), full / 2.0);

    // The angle is a dimension like any other.
    let angle = part.find_dimension("D1@Revolve1").unwrap();
    part.set_dimension(angle, PI / 2.0).unwrap();
    part.rebuild(&mut kernel);
    assert_close(volume(&part, &kernel), full / 4.0);
    part.release(&mut kernel);
}

#[test]
fn revolved_cut_cuts_a_groove() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    // Cylinder r = 10 from y = 0 to 20.
    let mut disc = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    disc.circle(Point2::ORIGIN, 10.0);
    let s1 = part.add_feature(FeatureKind::Sketch(disc));
    part.add_feature(boss(s1, 20.0));
    // Groove from r = 8 outwards, y = 8 to 12.
    let (profile, axis) = revolve_profile(8.0, 12.0, 8.0, 12.0);
    let s2 = part.add_feature(FeatureKind::Sketch(profile));
    part.add_feature(revolve(
        s2,
        ExtrudeOp::Cut,
        RevolveAxis::SketchLine(axis),
        TAU,
    ));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(
        volume(&part, &kernel),
        PI * 100.0 * 20.0 - PI * (100.0 - 64.0) * 4.0,
    );
    part.release(&mut kernel);
}

#[test]
fn a_revolve_axis_off_the_sketch_plane_is_an_error() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (profile, _) = revolve_profile(5.0, 10.0, 0.0, 20.0);
    let s1 = part.add_feature(FeatureKind::Sketch(profile));
    let bad = part.add_feature(revolve(
        s1,
        ExtrudeOp::Boss,
        RevolveAxis::Axis(AxisRef::Standard(StandardAxis::Z)),
        TAU,
    ));
    part.rebuild(&mut kernel);
    match part.status(bad) {
        Some(FeatureStatus::Error(message)) => assert!(message.contains("sketch plane")),
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
}

#[test]
fn shell_hollows_the_part_and_names_its_inner_walls() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let base = block(&mut part, Point2::ORIGIN, 40.0, 20.0, 10.0);
    let shell = part.add_feature(FeatureKind::Shell(Shell {
        faces: vec![FaceName::end_cap(base)],
        thickness: 2.0,
        outward: false,
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 8000.0 - 36.0 * 16.0 * 8.0);

    let body = part.body().unwrap();
    // The outside keeps its names; the inside is named after the faces it is offset from.
    assert!(body.naming.find_face(&FaceName::start_cap(base)).is_some());
    let inner_bottom = FaceName::new(FaceKey::Offset {
        feature: shell,
        face: Box::new(FaceName::start_cap(base)),
    });
    assert!(
        body.naming.find_face(&inner_bottom).is_some(),
        "faces: {:?}",
        body.naming.faces()
    );

    // A thicker wall, through its dimension.
    let thickness = part.find_dimension("D1@Shell1").unwrap();
    part.set_dimension(thickness, 1.0).unwrap();
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 8000.0 - 38.0 * 18.0 * 9.0);
    part.release(&mut kernel);
}

#[test]
fn sketch_on_a_reference_plane() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let plane = part.add_feature(FeatureKind::Plane(RefPlane {
        base: PlaneRef::Standard(StandardPlane::Top),
        offset: 30.0,
        flip: false,
    }));
    let mut sketch = Sketch::new(SketchPlane::Reference(plane));
    sketch.circle(Point2::ORIGIN, 5.0);
    let s1 = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(boss(s1, 10.0));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), PI * 25.0 * 10.0);
    // The cylinder starts on the plane, 30 mm up.
    let body = part.body().unwrap();
    let lowest = body
        .topology
        .vertices
        .iter()
        .map(|v| v.point.y)
        .fold(f64::INFINITY, f64::min);
    assert_close(lowest, 30.0);
    part.release(&mut kernel);
}

/// A 100 × 20 × 10 plate with one Ø6 hole at (10, 10). Returns the part, the plate, the
/// hole feature and the name of the hole's wall.
fn plate_with_hole() -> (Part, FeatureId, FeatureId, FaceName) {
    let mut part = Part::new();
    let base = block(&mut part, Point2::ORIGIN, 100.0, 20.0, 10.0);
    let mut sketch = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    let circle = sketch.circle(Point2::new(10.0, 10.0), 3.0);
    let s2 = part.add_feature(FeatureKind::Sketch(sketch));
    let hole = part.add_feature(cut(s2, 10.0));
    (part, base, hole, FaceName::profile(hole, circle))
}

fn hole_volume() -> f64 {
    PI * 9.0 * 10.0
}

#[test]
fn linear_pattern_copies_a_hole_and_its_instances_can_be_referenced() {
    let mut kernel = OcctKernel::new();
    let (mut part, base, hole, wall) = plate_with_hole();
    let pattern = part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![hole],
        first: PatternDirection {
            axis: AxisRef::Standard(StandardAxis::X),
            spacing: 20.0,
            count: 4,
            reverse: false,
        },
        second: None,
    }));
    // A fillet on the rim of the second hole: a reference to a pattern instance.
    let instance_wall = FaceName::new(FaceKey::Instance {
        feature: pattern,
        instance: [1, 0],
        face: Box::new(wall),
    });
    part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: vec![EdgeName::between(
            instance_wall.clone(),
            FaceName::end_cap(base),
        )],
        size: 0.5,
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let plate = 100.0 * 20.0 * 10.0;
    let rim = 0.25 * (1.0 - PI / 4.0) * 2.0 * PI * 3.0;
    let with_rim = |holes: f64| plate - holes * hole_volume() - rim;
    // The fillet removes a little material around one rim (approximately: the rim is round).
    let actual = volume(&part, &kernel);
    assert!((actual - with_rim(4.0)).abs() < 0.5, "got {actual}");

    // Wider spacing: the fillet follows its hole.
    let spacing = part.find_dimension("D1@LPattern1").unwrap();
    part.set_dimension(spacing, 25.0).unwrap();
    part.rebuild(&mut kernel);
    assert_all_ok(&part);

    // The count from a global variable, through an equation.
    let n = part.parse_expression("3").unwrap();
    part.add_global("Holes", n).unwrap();
    let count = part.find_dimension("D2@LPattern1").unwrap();
    let expr = part.parse_expression("\"Holes\"").unwrap();
    part.add_equation(count, expr).unwrap();
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let actual = volume(&part, &kernel);
    assert!((actual - with_rim(3.0)).abs() < 0.5, "got {actual}");
    part.release(&mut kernel);
}

#[test]
fn two_direction_pattern() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let base = block(&mut part, Point2::ORIGIN, 100.0, 50.0, 10.0);
    let mut sketch = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    sketch.circle(Point2::new(10.0, 10.0), 3.0);
    let s2 = part.add_feature(FeatureKind::Sketch(sketch));
    let hole = part.add_feature(cut(s2, 10.0));
    part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![hole],
        first: PatternDirection {
            axis: AxisRef::Standard(StandardAxis::X),
            spacing: 20.0,
            count: 4,
            reverse: false,
        },
        second: Some(PatternDirection {
            axis: AxisRef::Standard(StandardAxis::Y),
            spacing: 15.0,
            count: 3,
            reverse: false,
        }),
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 50000.0 - 12.0 * hole_volume());
    part.release(&mut kernel);
}

#[test]
fn circular_pattern_about_the_axis_of_a_cylindrical_face() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let mut disc = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let rim = disc.circle(Point2::ORIGIN, 30.0);
    let s1 = part.add_feature(FeatureKind::Sketch(disc));
    let plate = part.add_feature(boss(s1, 5.0));
    let mut sketch = Sketch::new(SketchPlane::Face(FaceName::end_cap(plate)));
    sketch.circle(Point2::new(20.0, 0.0), 3.0);
    let s2 = part.add_feature(FeatureKind::Sketch(sketch));
    let hole = part.add_feature(cut(s2, 5.0));
    // A reference axis along the disc's cylindrical face.
    let axis = part.add_feature(FeatureKind::Axis(RefAxis::Along(AxisRef::Face(
        FaceName::profile(plate, rim),
    ))));
    part.add_feature(FeatureKind::CircularPattern(CircularPattern {
        seeds: vec![hole],
        axis: AxisRef::Reference(axis),
        angle: TAU,
        count: 6,
        equal_spacing: true,
        reverse: false,
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(
        volume(&part, &kernel),
        PI * 900.0 * 5.0 - 6.0 * PI * 9.0 * 5.0,
    );
    part.release(&mut kernel);
}

#[test]
fn mirror_copies_a_boss_through_a_plane() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let base = block(&mut part, Point2::new(-20.0, 0.0), 40.0, 20.0, 10.0);
    let mut sketch = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    sketch.rectangle(Point2::new(5.0, 5.0), 5.0, 5.0);
    let s2 = part.add_feature(FeatureKind::Sketch(sketch));
    let lug = part.add_feature(boss(s2, 5.0));
    let mirror = part.add_feature(FeatureKind::Mirror(Mirror {
        seeds: vec![lug],
        plane: PlaneRef::Standard(StandardPlane::Right),
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 8000.0 + 2.0 * 125.0);
    let mirrored_top = FaceName::new(FaceKey::Instance {
        feature: mirror,
        instance: [1, 0],
        face: Box::new(FaceName::end_cap(lug)),
    });
    let body = part.body().unwrap();
    let index = body
        .naming
        .find_face(&mirrored_top)
        .expect("mirrored top face");
    let centroid = body.topology.faces[index as usize].centroid;
    assert_close(centroid.x, -7.5);
    assert_close(centroid.z, 15.0);
    part.release(&mut kernel);
}

#[test]
fn patterns_only_copy_extrusions_and_revolves() {
    let mut kernel = OcctKernel::new();
    let (mut part, base, _, _) = plate_with_hole();
    let fillet = part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: vec![EdgeName::between(
            FaceName::start_cap(base),
            FaceName::profile(base, 5),
        )],
        size: 1.0,
    }));
    let pattern = part.add_feature(FeatureKind::Mirror(Mirror {
        seeds: vec![fillet],
        plane: PlaneRef::Standard(StandardPlane::Right),
    }));
    part.rebuild(&mut kernel);
    assert!(matches!(
        part.status(pattern),
        Some(FeatureStatus::Error(_))
    ));
    part.release(&mut kernel);
}
