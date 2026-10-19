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
    StandardPlane, Sweep,
};
use oxyde_occt::OcctKernel;

fn assert_all_ok(part: &Part) {
    for entry in part.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{}", entry.name);
    }
}

fn volume(part: &Part, kernel: &OcctKernel) -> f64 {
    part.body()
        .expect("part has a body")
        .solids
        .iter()
        .map(|s| kernel.volume(s.shape).unwrap())
        .sum()
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
        bodies: Vec::new(),
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

/// OpenCASCADE turns a profile lying across its axis into a solid that passes through
/// itself, without an error; SolidWorks refuses the sketch. A profile touching the axis
/// is a plain solid of revolution.
#[test]
fn a_profile_across_the_revolve_axis_is_refused() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (across, axis) = revolve_profile(-5.0, 10.0, 0.0, 20.0);
    let s1 = part.add_feature(FeatureKind::Sketch(across));
    let bad = part.add_feature(revolve(
        s1,
        ExtrudeOp::Boss,
        RevolveAxis::SketchLine(axis),
        TAU,
    ));
    // A circle across the model Y axis, about which it is turned.
    let mut disc = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    disc.circle(Point2::new(3.0, 40.0), 4.0);
    let s2 = part.add_feature(FeatureKind::Sketch(disc));
    let round = part.add_feature(revolve(
        s2,
        ExtrudeOp::NewBody,
        RevolveAxis::Axis(AxisRef::Standard(StandardAxis::Y)),
        PI,
    ));
    part.rebuild(&mut kernel);
    for feature in [bad, round] {
        match part.status(feature) {
            Some(FeatureStatus::Error(message)) => {
                assert!(message.contains("crosses the axis"), "{message}")
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }
    assert!(part.body().is_none(), "nothing was built");

    // From the axis outwards: a cylinder of radius 10.
    let (touching, axis) = revolve_profile(0.0, 10.0, 0.0, 20.0);
    match part.edit(s1).unwrap() {
        FeatureKind::Sketch(s) => *s = touching,
        _ => unreachable!(),
    }
    match part.edit(bad).unwrap() {
        FeatureKind::Revolve(r) => r.axis = RevolveAxis::SketchLine(axis),
        _ => unreachable!(),
    }
    part.rebuild(&mut kernel);
    assert_eq!(part.status(bad), Some(FeatureStatus::Ok));
    assert_close(volume(&part, &kernel), PI * 100.0 * 20.0);
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

/// A Ø20 × 20 cylinder on the Front plane, and a shell of it.
fn shelled_cylinder(
    faces: impl Fn(FeatureId) -> Vec<FaceName>,
    thickness: f64,
    outward: bool,
) -> (Part, FeatureId, FeatureId) {
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.circle(Point2::ORIGIN, 10.0);
    let s1 = part.add_feature(FeatureKind::Sketch(sketch));
    let base = part.add_feature(boss(s1, 20.0));
    let shell = part.add_feature(FeatureKind::Shell(Shell {
        faces: faces(base),
        thickness,
        outward,
    }));
    (part, base, shell)
}

#[test]
fn a_shell_with_no_face_removed_is_hollow() {
    let mut kernel = OcctKernel::new();
    for outward in [false, true] {
        let (mut part, base, shell) = shelled_cylinder(|_| Vec::new(), 1.0, outward);
        part.rebuild(&mut kernel);
        assert_all_ok(&part);
        // Inward: Ø20 × 20 around a Ø18 × 18 void; outward: Ø22 × 22 around the part.
        let (outside, void) = if outward {
            (PI * 121.0 * 22.0, PI * 100.0 * 20.0)
        } else {
            (PI * 100.0 * 20.0, PI * 81.0 * 18.0)
        };
        assert_close(volume(&part, &kernel), outside - void);
        let body = part.body().unwrap();
        assert_eq!(
            kernel.topology_counts(body.solids[0].shape).unwrap().shells,
            2
        );
        // The part's faces and their offset copies, all named.
        for face in [FaceName::start_cap(base), FaceName::end_cap(base)] {
            let copy = FaceName::new(FaceKey::Offset {
                feature: shell,
                face: Box::new(face.clone()),
            });
            assert!(body.naming.find_face(&face).is_some(), "{face}");
            assert!(body.naming.find_face(&copy).is_some(), "{copy}");
        }
        part.release(&mut kernel);
    }
}

#[test]
fn a_cylinder_is_as_wide_as_its_diameter() {
    let mut kernel = OcctKernel::new();
    let (mut part, _, shell) = shelled_cylinder(|_| Vec::new(), 1.0, false);
    part.set_suppressed(shell, true).unwrap();
    part.rebuild(&mut kernel);
    let body = part.body().unwrap();
    let (lo, hi) = kernel.bounds(body.solids[0].shape).unwrap();
    for (got, want) in [
        (lo.x, -10.0),
        (lo.y, -10.0),
        (lo.z, 0.0),
        (hi.x, 10.0),
        (hi.y, 10.0),
        (hi.z, 20.0),
    ] {
        assert!((got - want).abs() < 1e-6, "{got} vs {want}");
    }
    part.release(&mut kernel);
}

#[test]
fn a_shell_thicker_than_the_part_is_an_error() {
    let mut kernel = OcctKernel::new();
    // 12 mm walls in a 10 mm radius: OpenCASCADE turns the wall inside out.
    let (mut part, _, shell) = shelled_cylinder(|base| vec![FaceName::end_cap(base)], 12.0, false);
    part.rebuild(&mut kernel);
    assert!(matches!(part.status(shell), Some(FeatureStatus::Error(_))));
    // An outward shell, open at the top, is fine.
    if let Ok(FeatureKind::Shell(s)) = part.edit(shell) {
        s.thickness = 1.0;
        s.outward = true;
    }
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(
        volume(&part, &kernel),
        PI * 121.0 * 21.0 - PI * 100.0 * 20.0,
    );
    part.release(&mut kernel);
}

#[test]
fn a_fillet_on_a_smooth_edge_says_so() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    // A slot: straight sides tangent to the round ends.
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let g = &mut sketch.geometry;
    let (a, b) = (
        g.point(Point2::new(0.0, 0.0)),
        g.point(Point2::new(10.0, 0.0)),
    );
    let (c, d) = (
        g.point(Point2::new(10.0, 10.0)),
        g.point(Point2::new(0.0, 10.0)),
    );
    let (right, left) = (
        g.point(Point2::new(10.0, 5.0)),
        g.point(Point2::new(0.0, 5.0)),
    );
    let bottom = g.line_between(a, b);
    let end = g.arc_between(right, b, c);
    g.line_between(c, d);
    g.arc_between(left, d, a);
    let s1 = part.add_feature(FeatureKind::Sketch(sketch));
    let slot = part.add_feature(boss(s1, 5.0));
    let fillet = part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: vec![EdgeName::between(
            FaceName::profile(slot, bottom.0),
            FaceName::profile(slot, end.0),
        )],
        size: 1.0,
        chamfer: Default::default(),
    }));
    part.rebuild(&mut kernel);
    match part.status(fillet) {
        Some(FeatureStatus::Error(message)) => assert!(message.contains("smooth"), "{message}"),
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
}

#[test]
fn contours_that_cross_are_not_extruded() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    // Two overlapping circles: OpenCASCADE would make two overlapping solids.
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.circle(Point2::ORIGIN, 10.0);
    sketch.circle(Point2::new(8.0, 0.0), 5.0);
    let s1 = part.add_feature(FeatureKind::Sketch(sketch));
    let bad = part.add_feature(boss(s1, 5.0));
    part.rebuild(&mut kernel);
    match part.status(bad) {
        Some(FeatureStatus::Error(message)) => assert!(message.contains("cross"), "{message}"),
        other => panic!("expected an error, got {other:?}"),
    }
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
        chamfer: Default::default(),
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
fn pattern_directions_along_edges() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let base = block(&mut part, Point2::ORIGIN, 100.0, 50.0, 10.0);
    part.rebuild(&mut kernel);
    // Two edges of the top face: one along X, one along Y.
    let (along_x, along_y) = {
        let body = part.body().expect("part has a body");
        let top = FaceName::end_cap(base);
        let edge = |direction: [f64; 3]| {
            body.topology
                .edges
                .iter()
                .enumerate()
                .filter_map(|(i, e)| {
                    let axis = e.axis.as_ref()?;
                    let d = axis.direction;
                    let parallel = (d.x.abs() - direction[0]).abs() < 1e-9
                        && (d.y.abs() - direction[1]).abs() < 1e-9
                        && (d.z.abs() - direction[2]).abs() < 1e-9;
                    let name = body.naming.edge(i as u32)?;
                    (parallel && name.faces.contains(&top)).then(|| name.clone())
                })
                .next()
                .expect("an edge of the top face")
        };
        (edge([1.0, 0.0, 0.0]), edge([0.0, 1.0, 0.0]))
    };
    let mut sketch = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    sketch.circle(Point2::new(10.0, 10.0), 3.0);
    let s2 = part.add_feature(FeatureKind::Sketch(sketch));
    let hole = part.add_feature(cut(s2, 10.0));
    let direction = |axis: EdgeName, spacing: f64, count: u32| PatternDirection {
        axis: AxisRef::Edge(axis),
        spacing,
        count,
        reverse: false,
    };
    let pattern = part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![hole],
        first: direction(along_x.clone(), 20.0, 4),
        second: Some(direction(along_y, 15.0, 3)),
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 50000.0 - 12.0 * hole_volume());

    // The same edge twice: refused rather than copies stacked on each other.
    if let Ok(FeatureKind::LinearPattern(p)) = part.edit(pattern) {
        p.second = Some(direction(along_x, 15.0, 3));
    }
    part.rebuild(&mut kernel);
    match part.status(pattern) {
        Some(FeatureStatus::Error(message)) => assert!(message.contains("parallel")),
        other => panic!("expected an error, got {other:?}"),
    }
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
        bodies: Vec::new(),
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
    let face = &body.topology.faces[index as usize];
    assert_close(face.centroid.x, -7.5);
    assert_close(face.centroid.z, 15.0);
    // Mirrored faces face outward like any other: a boss sketched on the mirrored top
    // grows upwards, out of the part.
    assert_eq!(face.normal, Some(oxyde_geom::Vec3::Z));
    let mut on_top = Sketch::new(SketchPlane::Face(mirrored_top));
    on_top.circle(Point2::new(-7.5, 7.5), 1.0);
    let s3 = part.add_feature(FeatureKind::Sketch(on_top));
    part.add_feature(boss(s3, 2.0));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 8000.0 + 2.0 * 125.0 + PI * 2.0);
    part.release(&mut kernel);
}

#[test]
fn revolved_flat_faces_face_outward_and_caps_keep_their_names() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    // A washer: r = 5 to 10, y = 0 to 2, a quarter turn about the sketch's centerline.
    let (profile, axis) = revolve_profile(5.0, 10.0, 0.0, 2.0);
    let s1 = part.add_feature(FeatureKind::Sketch(profile));
    let washer = part.add_feature(revolve(
        s1,
        ExtrudeOp::Boss,
        RevolveAxis::SketchLine(axis),
        PI / 2.0,
    ));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let body = part.body().unwrap();
    let center = oxyde_geom::Point3::new(5.0, 1.0, -5.0);
    let mut caps = 0;
    for (i, face) in body.topology.faces.iter().enumerate() {
        let Some(normal) = face.normal else { continue };
        let name = body.naming.face(i as u32).unwrap();
        assert!(
            (face.centroid - center).dot(normal) > 0.0,
            "{name} faces inward"
        );
        if [FaceName::start_cap(washer), FaceName::end_cap(washer)].contains(name) {
            caps += 1;
        } else {
            // The flat ring faces come from the bottom and top lines of the rectangle.
            assert!(matches!(name.key, FaceKey::Profile { .. }), "{name}");
            assert_eq!(name.part, 0, "{name}");
        }
    }
    assert_eq!(caps, 2);
    part.release(&mut kernel);
}

#[test]
fn pattern_copies_that_overlap_each_other() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let base = block(&mut part, Point2::ORIGIN, 50.0, 10.0, 5.0);
    // A 2 mm wide cut across the plate, copied every 1 mm: the copies overlap, and all of
    // them go in one boolean. Together they clear x = 5 to 11.
    let mut slot = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    let [_, right, _, _] = slot.rectangle(Point2::new(5.0, -1.0), 2.0, 12.0);
    let s2 = part.add_feature(FeatureKind::Sketch(slot));
    let cut_slot = part.add_feature(cut(s2, 5.0));
    let pattern = part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![cut_slot],
        first: PatternDirection {
            axis: AxisRef::Standard(StandardAxis::X),
            spacing: 1.0,
            count: 5,
            reverse: false,
        },
        second: None,
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 2500.0 - 6.0 * 10.0 * 5.0);
    // The far wall of the last copy is named after it.
    let last_wall = FaceName::new(FaceKey::Instance {
        feature: pattern,
        instance: [4, 0],
        face: Box::new(FaceName::profile(cut_slot, right)),
    });
    let body = part.body().unwrap();
    assert!(
        body.naming.find_face(&last_wall).is_some(),
        "{:?}",
        body.naming.faces()
    );
    part.release(&mut kernel);
}

#[test]
fn a_pattern_or_mirror_of_the_first_boss_joins_its_body() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let base = block(&mut part, Point2::ORIGIN, 10.0, 10.0, 5.0);
    // Instances overlap the seed: one body of 3 overlapping blocks, 20 × 10 × 5.
    part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![base],
        first: PatternDirection {
            axis: AxisRef::Standard(StandardAxis::X),
            spacing: 5.0,
            count: 3,
            reverse: false,
        },
        second: None,
    }));
    // Mirrored through the Right plane: touching it, merged with it.
    part.add_feature(FeatureKind::Mirror(Mirror {
        seeds: vec![base],
        plane: PlaneRef::Standard(StandardPlane::Right),
        bodies: Vec::new(),
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let body = part.body().unwrap();
    assert_eq!(body.solids.len(), 1);
    assert_eq!(
        kernel.topology_counts(body.solids[0].shape).unwrap().solids,
        1
    );
    assert_close(volume(&part, &kernel), 1000.0 + 500.0);
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
        chamfer: Default::default(),
    }));
    let pattern = part.add_feature(FeatureKind::Mirror(Mirror {
        seeds: vec![fillet],
        plane: PlaneRef::Standard(StandardPlane::Right),
        bodies: Vec::new(),
    }));
    part.rebuild(&mut kernel);
    assert!(matches!(
        part.status(pattern),
        Some(FeatureStatus::Error(_))
    ));
    part.release(&mut kernel);
}

#[test]
fn patterns_and_mirrors_copy_patterns_and_mirrors() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let base = block(&mut part, Point2::ORIGIN, 100.0, 50.0, 10.0);
    let mut sketch = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    let circle = sketch.circle(Point2::new(10.0, 10.0), 3.0);
    let s2 = part.add_feature(FeatureKind::Sketch(sketch));
    let hole = part.add_feature(cut(s2, 10.0));
    let along = |axis, spacing, count| PatternDirection {
        axis: AxisRef::Standard(axis),
        spacing,
        count,
        reverse: false,
    };
    // Two holes along X, then that pair (seed and pattern) twice along Y: four holes, as
    // SolidWorks patterns a pattern.
    let row = part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![hole],
        first: along(StandardAxis::X, 20.0, 2),
        second: None,
    }));
    let rows = part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![hole, row],
        first: along(StandardAxis::Y, 15.0, 2),
        second: None,
    }));
    // Mirrored through the middle of the plate: eight holes.
    let middle = part.add_feature(FeatureKind::Plane(RefPlane {
        base: PlaneRef::Standard(StandardPlane::Right),
        offset: 50.0,
        flip: false,
    }));
    let mirror = part.add_feature(FeatureKind::Mirror(Mirror {
        seeds: vec![hole, row, rows],
        plane: PlaneRef::Reference(middle),
        bodies: Vec::new(),
    }));
    // A fillet on the rim of a copy of a copy of a copy: its name nests the instances.
    let wall = FaceName::new(FaceKey::Instance {
        feature: mirror,
        instance: [1, 0],
        face: Box::new(FaceName::new(FaceKey::Instance {
            feature: rows,
            instance: [1, 0],
            face: Box::new(FaceName::new(FaceKey::Instance {
                feature: row,
                instance: [1, 0],
                face: Box::new(FaceName::profile(hole, circle)),
            })),
        })),
    });
    part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: vec![EdgeName::between(wall, FaceName::end_cap(base))],
        size: 0.5,
        chamfer: Default::default(),
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let rim = 0.25 * (1.0 - PI / 4.0) * 2.0 * PI * 3.0;
    let actual = volume(&part, &kernel);
    let expected = 50000.0 - 8.0 * hole_volume() - rim;
    assert!(
        (actual - expected).abs() < 0.5,
        "got {actual}, expected {expected}"
    );

    // More rows: the mirror follows, the fillet stays on its hole.
    let count = part.find_dimension("D2@LPattern2").unwrap();
    part.set_dimension(count, 3.0).unwrap();
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let actual = volume(&part, &kernel);
    let expected = 50000.0 - 12.0 * hole_volume() - rim;
    assert!(
        (actual - expected).abs() < 0.5,
        "got {actual}, expected {expected}"
    );
    part.release(&mut kernel);
}

#[test]
fn a_pattern_of_a_boss_and_a_cut_cannot_be_patterned_again() {
    let mut kernel = OcctKernel::new();
    let (mut part, _, hole, _) = plate_with_hole();
    let peg = block(&mut part, Point2::new(40.0, 5.0), 5.0, 5.0, 15.0);
    let mixed = part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![hole, peg],
        first: PatternDirection {
            axis: AxisRef::Standard(StandardAxis::X),
            spacing: 20.0,
            count: 2,
            reverse: false,
        },
        second: None,
    }));
    let again = part.add_feature(FeatureKind::Mirror(Mirror {
        seeds: vec![mixed],
        plane: PlaneRef::Standard(StandardPlane::Right),
        bodies: Vec::new(),
    }));
    part.rebuild(&mut kernel);
    assert_eq!(part.status(mixed), Some(FeatureStatus::Ok));
    match part.status(again) {
        Some(FeatureStatus::Error(message)) => {
            assert!(message.contains("do not all add"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    part.release(&mut kernel);
}

// ---- Sweep -----------------------------------------------------------------------------------

/// A round profile of radius `r` at (`x`, 0) on the Front plane.
fn round_profile(x: f64, r: f64) -> (Sketch, u32) {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let circle = sketch.circle(Point2::new(x, 0.0), r);
    (sketch, circle)
}

fn sweep(profile: FeatureId, path: FeatureId, op: ExtrudeOp) -> FeatureKind {
    FeatureKind::Sweep(Sweep {
        profile,
        path,
        op,
        bodies: Vec::new(),
    })
}

#[test]
fn a_sweep_along_a_straight_path_is_an_extrusion() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (profile, circle) = round_profile(0.0, 5.0);
    let s1 = part.add_feature(FeatureKind::Sketch(profile));
    // The Top plane's sketch Y points towards -Z: this line runs 30 mm along +Z.
    let mut path = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    path.geometry
        .line(Point2::new(0.0, 0.0), Point2::new(0.0, -30.0));
    let s2 = part.add_feature(FeatureKind::Sketch(path));
    let pipe = part.add_feature(sweep(s1, s2, ExtrudeOp::Boss));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), PI * 25.0 * 30.0);
    // Its faces are named as an extrusion's: the side after the circle, two caps.
    let body = part.body().unwrap();
    assert!(
        body.naming
            .find_face(&FaceName::profile(pipe, circle))
            .is_some()
    );
    assert!(body.naming.find_face(&FaceName::start_cap(pipe)).is_some());
    assert!(body.naming.find_face(&FaceName::end_cap(pipe)).is_some());
    assert_eq!(part.feature(pipe).unwrap().name(), "Sweep1");
    part.release(&mut kernel);
}

#[test]
fn a_sweep_along_an_arc_has_the_volume_pappus_gives() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (profile, _) = round_profile(0.0, 5.0);
    let s1 = part.add_feature(FeatureKind::Sketch(profile));
    // A quarter turn of radius 20 from the origin, starting along +Z (sketch -Y on Top).
    let mut path = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    path.geometry.arc(
        Point2::new(20.0, 0.0),
        Point2::new(0.0, 0.0),
        Point2::new(20.0, -20.0),
    );
    let s2 = part.add_feature(FeatureKind::Sketch(path));
    part.add_feature(sweep(s1, s2, ExtrudeOp::Boss));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    // Pappus: the profile's area times the distance its centre travels.
    assert_close(volume(&part, &kernel), PI * 25.0 * (20.0 * PI / 2.0));
    part.release(&mut kernel);
}

#[test]
fn a_sweep_along_a_closed_path_is_a_ring() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (profile, _) = round_profile(20.0, 5.0);
    let s1 = part.add_feature(FeatureKind::Sketch(profile));
    let mut path = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    path.circle(Point2::ORIGIN, 20.0);
    let s2 = part.add_feature(FeatureKind::Sketch(path));
    part.add_feature(sweep(s1, s2, ExtrudeOp::Boss));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    // A torus: 2 π² R r².
    assert_close(volume(&part, &kernel), 2.0 * PI * PI * 20.0 * 25.0);
    part.release(&mut kernel);
}

#[test]
fn a_cut_sweep_removes_material_and_a_sweep_can_be_patterned() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let base = block(&mut part, Point2::ORIGIN, 40.0, 40.0, 10.0);
    let mut profile = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    profile.circle(Point2::new(10.0, 20.0), 2.0);
    let s1 = part.add_feature(FeatureKind::Sketch(profile));
    let mut path = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    path.geometry
        .line(Point2::new(10.0, 0.0), Point2::new(10.0, -10.0));
    let s2 = part.add_feature(FeatureKind::Sketch(path));
    let groove = part.add_feature(sweep(s1, s2, ExtrudeOp::Cut));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let hole = PI * 4.0 * 10.0;
    assert_close(volume(&part, &kernel), 16000.0 - hole);
    assert_eq!(part.feature(groove).unwrap().name(), "Cut-Sweep1");

    // Two more holes along X: the sweep's tool body is copied like an extrusion's.
    part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![groove],
        first: PatternDirection {
            axis: AxisRef::Standard(StandardAxis::X),
            spacing: 10.0,
            count: 3,
            reverse: false,
        },
        second: None,
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 16000.0 - 3.0 * hole);
    let _ = base;
    part.release(&mut kernel);
}

#[test]
fn a_sweep_refuses_a_path_in_pieces_or_the_profile_as_path() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (profile, _) = round_profile(0.0, 5.0);
    let s1 = part.add_feature(FeatureKind::Sketch(profile));
    let mut path = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    path.geometry
        .line(Point2::new(0.0, 0.0), Point2::new(0.0, -30.0));
    path.geometry
        .line(Point2::new(5.0, 0.0), Point2::new(5.0, -30.0));
    let s2 = part.add_feature(FeatureKind::Sketch(path));
    let broken = part.add_feature(sweep(s1, s2, ExtrudeOp::Boss));
    let same = part.add_feature(sweep(s1, s1, ExtrudeOp::Boss));
    part.rebuild(&mut kernel);
    match part.status(broken) {
        Some(FeatureStatus::Error(message)) => {
            assert!(message.contains("one chain"), "{message}");
        }
        other => panic!("a path in two pieces should fail, got {other:?}"),
    }
    assert!(matches!(part.status(same), Some(FeatureStatus::Error(_))));
    part.release(&mut kernel);
}

/// Half a symmetric part, mirrored whole about the Right plane: one body, twice the
/// volume, the copy's faces named after the half's and referred to by later features.
#[test]
fn a_body_mirrored_whole_makes_the_part_symmetric() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    // From x = 0 to 20, against the Right plane.
    let half = block(&mut part, Point2::new(0.0, 0.0), 20.0, 10.0, 5.0);
    let mirror = part.add_feature(FeatureKind::Mirror(Mirror {
        seeds: Vec::new(),
        plane: PlaneRef::Standard(StandardPlane::Right),
        bodies: vec![oxyde_model::BodyId::new(half, 0)],
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let body = part.body().unwrap();
    assert_eq!(body.solids.len(), 1, "joined to its image");
    assert_close(volume(&part, &kernel), 2.0 * 20.0 * 10.0 * 5.0);
    let (lo, hi) = kernel.bounds(body.solids[0].shape).unwrap();
    assert!((lo.x + 20.0).abs() < 1e-6 && (hi.x - 20.0).abs() < 1e-6);
    // The far end of the copy is the mirror's image of the half's right side (the
    // rectangle's second line: entity 6, after its four corners).
    let far = FaceName::new(FaceKey::Instance {
        feature: mirror,
        instance: [1, 0],
        face: Box::new(FaceName::profile(half, 6)),
    });
    let found = body.naming.resolve_face(&far);
    assert!(
        matches!(found, oxyde_model::Resolution::Exact(_)),
        "{far}: {found:?}"
    );
    // Features and bodies together are refused.
    match part.edit(mirror).unwrap() {
        FeatureKind::Mirror(m) => m.seeds = vec![half],
        _ => unreachable!(),
    }
    part.rebuild(&mut kernel);
    assert!(matches!(part.status(mirror), Some(FeatureStatus::Error(_))));
    part.release(&mut kernel);
}

/// Chamfers with two distances, or a distance and an angle, on the top front edge of a
/// 40 × 20 × 10 block (between its end cap at z = 10 and its side at y = 20).
#[test]
fn unequal_chamfers_take_two_distances_or_an_angle() {
    use oxyde_model::ChamferShape;
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let boss = block(&mut part, Point2::new(0.0, 0.0), 40.0, 20.0, 10.0);
    // The rectangle's third line, from (40, 20) to (0, 20): entity 7.
    let edge = EdgeName::between(FaceName::profile(boss, 7), FaceName::end_cap(boss));
    let bevel = part.add_feature(FeatureKind::Chamfer(EdgeBlend {
        edges: vec![edge],
        size: 2.0,
        chamfer: ChamferShape::TwoDistances {
            distance: 4.0,
            flip: false,
        },
    }));
    // How far along the top (z = 10) the chamfer starts from the side at y = 20.
    let top_reach = |part: &Part| {
        let body = part.body().unwrap();
        body.topology
            .vertices
            .iter()
            .filter(|v| (v.point.z - 10.0).abs() < 1e-6)
            .map(|v| 20.0 - v.point.y)
            .fold(f64::INFINITY, f64::min)
    };
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    // A right triangle 2 by 4, 40 long.
    assert_close(volume(&part, &kernel), 8000.0 - 0.5 * 2.0 * 4.0 * 40.0);
    let unflipped = top_reach(&part);
    // Flipped, the distances swap sides; the volume stays.
    match part.edit(bevel).unwrap() {
        FeatureKind::Chamfer(b) => {
            b.chamfer = ChamferShape::TwoDistances {
                distance: 4.0,
                flip: true,
            }
        }
        _ => unreachable!(),
    }
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 8000.0 - 0.5 * 2.0 * 4.0 * 40.0);
    let flipped = top_reach(&part);
    let mut reaches = [unflipped, flipped];
    reaches.sort_by(f64::total_cmp);
    assert!(
        (reaches[0] - 2.0).abs() < 1e-6 && (reaches[1] - 4.0).abs() < 1e-6,
        "{reaches:?}"
    );
    // 3 mm and 30°: the other leg is 3 tan 30°. The angle is a dimension.
    match part.edit(bevel).unwrap() {
        FeatureKind::Chamfer(b) => {
            b.size = 3.0;
            b.chamfer = ChamferShape::AngleDistance {
                angle: 30f64.to_radians(),
                flip: false,
            }
        }
        _ => unreachable!(),
    }
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let leg = 3.0 * 30f64.to_radians().tan();
    assert_close(volume(&part, &kernel), 8000.0 - 0.5 * 3.0 * leg * 40.0);
    assert!(part.find_dimension("D2@Chamfer1").is_some());
    part.release(&mut kernel);
}

/// A plane parallel to Front at `z`.
fn front_plane_at(part: &mut Part, z: f64) -> FeatureId {
    part.add_feature(FeatureKind::Plane(RefPlane {
        base: PlaneRef::Standard(StandardPlane::Front),
        offset: z,
        flip: false,
    }))
}

fn loft(profiles: Vec<FeatureId>, ruled: bool) -> FeatureKind {
    FeatureKind::Loft(oxyde_model::Loft {
        profiles,
        op: ExtrudeOp::NewBody,
        ruled,
        bodies: Vec::new(),
    })
}

#[test]
fn lofts_go_through_their_sections() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    // Circles of radius 10 at z = 0 and 5 at z = 20: a cone's frustum.
    let mut big = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let big_circle = big.circle(Point2::ORIGIN, 10.0);
    let s1 = part.add_feature(FeatureKind::Sketch(big));
    let up = front_plane_at(&mut part, 20.0);
    let mut small = Sketch::new(SketchPlane::Reference(up));
    small.circle(Point2::ORIGIN, 5.0);
    let s2 = part.add_feature(FeatureKind::Sketch(small));
    let cone = part.add_feature(loft(vec![s1, s2], false));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(
        volume(&part, &kernel),
        PI * 20.0 / 3.0 * (100.0 + 50.0 + 25.0),
    );
    // The side is named after the first section's circle; the ends are the caps.
    let body = part.body().unwrap();
    for name in [
        FaceName::profile(cone, big_circle),
        FaceName::start_cap(cone),
        FaceName::end_cap(cone),
    ] {
        let found = body.naming.resolve_face(&name);
        assert!(
            matches!(found, oxyde_model::Resolution::Exact(_)),
            "{name}: {found:?}"
        );
    }

    // Squares of 20 and 10, ruled: a pyramid's frustum, whichever way each square was
    // drawn.
    let mut part = Part::new();
    let mut base = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    base.rectangle(Point2::new(-10.0, -10.0), 20.0, 20.0);
    let s1 = part.add_feature(FeatureKind::Sketch(base));
    let up = front_plane_at(&mut part, 10.0);
    let mut top = Sketch::new(SketchPlane::Reference(up));
    let g = &mut top.geometry;
    let corners: Vec<_> = [(-5.0, -5.0), (-5.0, 5.0), (5.0, 5.0), (5.0, -5.0)]
        .into_iter()
        .map(|(x, y)| g.point(Point2::new(x, y)))
        .collect();
    for k in 0..4 {
        g.line_between(corners[k], corners[(k + 1) % 4]);
    }
    let s2 = part.add_feature(FeatureKind::Sketch(top));
    part.add_feature(loft(vec![s1, s2], true));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 10.0 / 3.0 * (400.0 + 100.0 + 200.0));
    part.release(&mut kernel);

    // A circle into a square: the sections need not match.
    let mut part = Part::new();
    let mut round = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    round.circle(Point2::ORIGIN, 8.0);
    let s1 = part.add_feature(FeatureKind::Sketch(round));
    let up = front_plane_at(&mut part, 15.0);
    let mut square = Sketch::new(SketchPlane::Reference(up));
    square.rectangle(Point2::new(-6.0, -6.0), 12.0, 12.0);
    let s2 = part.add_feature(FeatureKind::Sketch(square));
    // And a sketch of two contours, which a loft refuses.
    let mut two = Sketch::new(SketchPlane::Reference(up));
    two.circle(Point2::new(30.0, 0.0), 1.0);
    two.circle(Point2::new(40.0, 0.0), 1.0);
    let s3 = part.add_feature(FeatureKind::Sketch(two));
    let transition = part.add_feature(loft(vec![s1, s2], false));
    let refused = part.add_feature(loft(vec![s1, s3], false));
    part.rebuild(&mut kernel);
    assert_eq!(part.status(transition), Some(FeatureStatus::Ok));
    let v = volume(&part, &kernel);
    assert!(v > 15.0 * 144.0 * 0.7 && v < 15.0 * PI * 64.0, "{v}");
    match part.status(refused) {
        Some(FeatureStatus::Error(message)) => assert!(message.contains("one closed contour")),
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
}
