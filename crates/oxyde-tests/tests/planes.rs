//! Reference planes beyond the offset plane, built by OpenCASCADE: at an angle about a
//! line, through a line parallel or perpendicular to a plane, through three points, and
//! normal to a curve at a point. Positions, volumes and bounds are checked by hand.

use oxyde_geom::{Plane, Point2, Point3, Vec3};
use oxyde_kernel::Kernel;
use oxyde_model::{
    AxisRef, CurveRef, EdgeName, Extrude, ExtrudeExtent, ExtrudeOp, FaceName, FeatureId,
    FeatureKind, FeatureStatus, Part, PlaneRef, PointRef, RefPlane, ReferenceGeometry, Sketch,
    SketchPlane, StandardAxis, StandardPlane, VertexName,
};
use oxyde_occt::OcctKernel;

fn assert_all_ok(part: &Part) {
    for entry in part.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{}", entry.name);
    }
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-6 * expected.abs().max(1.0),
        "expected {expected}, got {actual}"
    );
}

fn assert_point(actual: Point3, expected: Point3) {
    assert!(
        actual.distance(expected) < 1e-6,
        "expected {expected:?}, got {actual:?}"
    );
}

fn assert_direction(actual: Vec3, expected: Vec3) {
    assert!(
        (actual - expected).length() < 1e-9,
        "expected {expected:?}, got {actual:?}"
    );
}

fn plane(part: &Part, id: FeatureId) -> Plane {
    match part.reference(id) {
        Some(ReferenceGeometry::Plane(plane)) => plane,
        other => panic!("not a plane: {other:?} ({:?})", part.status(id)),
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

/// Exact bounds of every body of the part.
fn bounds(part: &Part, kernel: &OcctKernel) -> (Point3, Point3) {
    let body = part.body().expect("part has a body");
    let mut lo = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut hi = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for solid in &body.solids {
        let (a, b) = kernel.bounds(solid.shape).unwrap();
        lo = Point3::new(lo.x.min(a.x), lo.y.min(a.y), lo.z.min(a.z));
        hi = Point3::new(hi.x.max(b.x), hi.y.max(b.y), hi.z.max(b.z));
    }
    (lo, hi)
}

fn boss(sketch: FeatureId, depth: f64) -> FeatureKind {
    FeatureKind::Extrude(Extrude::blind(sketch, ExtrudeOp::Boss, depth))
}

/// A 40 × 20 box on the Front plane, `depth` along +Z, from the origin. Returns the boss
/// and its bottom, right, top and left side faces (y = 0, x = 40, y = 20, x = 0).
fn block(part: &mut Part, depth: f64) -> (FeatureId, [FaceName; 4]) {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let sides = sketch.rectangle(Point2::ORIGIN, 40.0, 20.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let boss = part.add_feature(boss(s, depth));
    (boss, sides.map(|line| FaceName::profile(boss, line)))
}

/// The corner of a body where three faces meet.
fn corner(faces: [&FaceName; 3]) -> PointRef {
    let mut faces: Vec<FaceName> = faces.into_iter().cloned().collect();
    faces.sort();
    PointRef::Vertex(VertexName { faces, part: 0 })
}

/// A 20 × 10 rectangle on Plane1 at `degrees` to the Top plane about the X axis, extruded
/// 5 mm. Returns the part, the plane and its angle dimension.
fn block_on_angled_plane(degrees: f64) -> (Part, FeatureId) {
    let mut part = Part::new();
    let plane = part.add_feature(FeatureKind::Plane(RefPlane::Angle {
        base: PlaneRef::Standard(StandardPlane::Top),
        line: AxisRef::Standard(StandardAxis::X),
        angle: degrees.to_radians(),
        flip: false,
    }));
    let mut sketch = Sketch::new(SketchPlane::Reference(plane));
    sketch.rectangle(Point2::ORIGIN, 20.0, 10.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(boss(s, 5.0));
    (part, plane)
}

/// The bounds `block_on_angled_plane` gives, worked out by hand: the Top plane turned by
/// `a` about X has its normal at (0, cos a, sin a) and its sketch y axis at
/// (0, sin a, -cos a); the rectangle runs 0..20 along X and 0..10 along y, and the block
/// 0..5 along the normal.
fn angled_bounds(degrees: f64) -> (Point3, Point3) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (
        Point3::new(0.0, 0.0, -10.0 * cos),
        Point3::new(20.0, 10.0 * sin + 5.0 * cos, 5.0 * sin),
    )
}

#[test]
fn a_sketch_on_a_plane_at_30_degrees_is_extruded_along_it() {
    let mut kernel = OcctKernel::new();
    let (mut part, plane_id) = block_on_angled_plane(30.0);
    part.rebuild(&mut kernel);
    assert_all_ok(&part);

    let plane = plane(&part, plane_id);
    let (sin, cos) = 30f64.to_radians().sin_cos();
    assert_point(plane.origin, Point3::ORIGIN);
    assert_direction(plane.x_dir, Vec3::X);
    assert_direction(plane.normal, Vec3::new(0.0, cos, sin));
    assert_close(volume(&part, &kernel), 20.0 * 10.0 * 5.0);
    let (lo, hi) = bounds(&part, &kernel);
    let (want_lo, want_hi) = angled_bounds(30.0);
    assert_point(lo, want_lo);
    assert_point(hi, want_hi);

    // The angle is a dimension, D1@Plane1, in degrees for people.
    let dimension = part.find_dimension("D1@Plane1").expect("D1@Plane1");
    let shown = part.dimension(dimension).unwrap();
    assert_close(shown.value, 30f64.to_radians());
    assert_close(shown.unit.to_display(shown.value), 30.0);
    part.release(&mut kernel);
}

#[test]
fn the_plane_follows_its_angle_dimension_and_equations() {
    let mut kernel = OcctKernel::new();
    let (mut part, plane_id) = block_on_angled_plane(30.0);
    part.rebuild(&mut kernel);

    let dimension = part.find_dimension("D1@Plane1").unwrap();
    part.set_dimension(dimension, 45f64.to_radians()).unwrap();
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let (lo, hi) = bounds(&part, &kernel);
    let (want_lo, want_hi) = angled_bounds(45.0);
    assert_point(lo, want_lo);
    assert_point(hi, want_hi);
    assert_close(volume(&part, &kernel), 1000.0);

    // Driven by an equation, in degrees as the Equations window writes them.
    let expr = part.parse_expression("60").unwrap();
    part.add_equation(dimension, expr).unwrap();
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let (sin, cos) = 60f64.to_radians().sin_cos();
    assert_direction(plane(&part, plane_id).normal, Vec3::new(0.0, cos, sin));
    let (lo, hi) = bounds(&part, &kernel);
    let (want_lo, want_hi) = angled_bounds(60.0);
    assert_point(lo, want_lo);
    assert_point(hi, want_hi);

    // Flipped, it turns the other way.
    let mut flipped = match part.feature(plane_id).unwrap().kind() {
        FeatureKind::Plane(p) => p.clone(),
        _ => unreachable!(),
    };
    if let RefPlane::Angle { flip, .. } = &mut flipped {
        *flip = true;
    }
    part.replace_feature(plane_id, FeatureKind::Plane(flipped))
        .unwrap();
    part.rebuild(&mut kernel);
    assert_direction(plane(&part, plane_id).normal, Vec3::new(0.0, cos, -sin));
    part.release(&mut kernel);
}

#[test]
fn a_plane_at_an_angle_to_a_face_follows_the_face_and_its_edge() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (boss_id, [_, _, top, _]) = block(&mut part, 10.0);
    // About the top back edge of the front face (z = 10), tilted 90°: the face's plane
    // turned square to it, standing on the edge.
    let edge = EdgeName::between(top.clone(), FaceName::end_cap(boss_id));
    let plane_id = part.add_feature(FeatureKind::Plane(RefPlane::Angle {
        base: PlaneRef::Face(FaceName::end_cap(boss_id)),
        line: AxisRef::Edge(edge),
        angle: 90f64.to_radians(),
        flip: false,
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let first = plane(&part, plane_id);
    assert!(first.normal.dot(Vec3::Y).abs() > 1.0 - 1e-9, "{first:?}");
    assert_close(
        (first.origin - Point3::new(0.0, 20.0, 10.0)).dot(Vec3::Y),
        0.0,
    );

    // A deeper block: the face, its edge and the plane move up with it.
    let depth = part.find_dimension("D1@Boss-Extrude1").unwrap();
    part.set_dimension(depth, 15.0).unwrap();
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let moved = plane(&part, plane_id);
    assert_direction(moved.normal, first.normal);
    assert_close(moved.origin.z - first.origin.z, 5.0);
    part.release(&mut kernel);
}

#[test]
fn a_line_must_lie_in_the_plane_it_turns_about() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (boss_id, [_, _, top, _]) = block(&mut part, 10.0);
    // The edge is 10 mm in front of the Front plane.
    let edge = EdgeName::between(top, FaceName::end_cap(boss_id));
    let plane_id = part.add_feature(FeatureKind::Plane(RefPlane::Angle {
        base: PlaneRef::Standard(StandardPlane::Front),
        line: AxisRef::Edge(edge),
        angle: 0.5,
        flip: false,
    }));
    part.rebuild(&mut kernel);
    match part.status(plane_id) {
        Some(FeatureStatus::Error(message)) => {
            assert_eq!(message, "the line does not lie in the plane");
        }
        other => panic!("expected an error, got {other:?}"),
    }
    // The block is still there.
    assert_close(volume(&part, &kernel), 40.0 * 20.0 * 10.0);
    part.release(&mut kernel);
}

#[test]
fn planes_through_an_edge_parallel_or_perpendicular_to_a_plane() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (boss_id, [_, _, top, _]) = block(&mut part, 10.0);
    // The edge along X at y = 20, z = 10.
    let edge = EdgeName::between(top, FaceName::end_cap(boss_id));
    let through = |plane, perpendicular| {
        FeatureKind::Plane(RefPlane::ThroughLine {
            line: AxisRef::Edge(edge.clone()),
            plane: PlaneRef::Standard(plane),
            perpendicular,
            flip: false,
        })
    };
    let parallel_top = part.add_feature(through(StandardPlane::Top, false));
    let square_to_front = part.add_feature(through(StandardPlane::Front, true));
    let square_to_top = part.add_feature(through(StandardPlane::Top, true));
    let across = part.add_feature(through(StandardPlane::Right, false));
    part.rebuild(&mut kernel);

    // Parallel to Top through the edge: y = 20, facing as Top does.
    let p = plane(&part, parallel_top);
    assert_direction(p.normal, Vec3::Y);
    assert_close(p.origin.y, 20.0);
    // Square to Front through the edge: the plane y = 20 again.
    let p = plane(&part, square_to_front);
    assert!(p.normal.dot(Vec3::Y).abs() > 1.0 - 1e-9, "{p:?}");
    assert_close(p.origin.y, 20.0);
    // Square to Top through the edge: z = 10.
    let p = plane(&part, square_to_top);
    assert!(p.normal.dot(Vec3::Z).abs() > 1.0 - 1e-9, "{p:?}");
    assert_close(p.origin.z, 10.0);
    // The edge crosses the Right plane: no plane through it is parallel to it.
    match part.status(across) {
        Some(FeatureStatus::Error(message)) => {
            assert_eq!(message, "the line is not parallel to the plane");
        }
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
}

#[test]
fn a_plane_through_three_corners_of_a_box_cuts_it_in_half() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (boss_id, [bottom, right, top, left]) = block(&mut part, 10.0);
    let (front, back) = (FaceName::end_cap(boss_id), FaceName::start_cap(boss_id));
    // A = (0, 0, 10), B = (40, 0, 10), C = (0, 20, 0): the plane y + 2z = 20.
    let plane_id = part.add_feature(FeatureKind::Plane(RefPlane::ThreePoints {
        points: [
            corner([&left, &bottom, &front]),
            corner([&right, &bottom, &front]),
            corner([&left, &top, &back]),
        ],
        flip: false,
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let p = plane(&part, plane_id);
    assert_point(p.origin, Point3::new(0.0, 0.0, 10.0));
    assert_direction(p.x_dir, Vec3::X);
    let n = 5f64.sqrt();
    assert_direction(p.normal, Vec3::new(0.0, 1.0 / n, 2.0 / n));

    // A rectangle on it over the whole box, cut through everything on the side the
    // normal points to: the wedge y + 2z <= 20 is left, half the box.
    let mut sketch = Sketch::new(SketchPlane::Reference(plane_id));
    sketch.rectangle(Point2::new(-5.0, -5.0), 50.0, 35.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Extrude(Extrude {
        sketch: s,
        op: ExtrudeOp::Cut,
        extent: ExtrudeExtent::ThroughAll { both: false },
        reverse: true,
        bodies: Vec::new(),
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 40.0 * (20.0 * 10.0 / 2.0));
    part.release(&mut kernel);
}

#[test]
fn three_points_in_line_make_no_plane() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let points = [0.0, 1.0, 2.0].map(|x| sketch.geometry.point(Point2::new(x, x)).0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let plane_id = part.add_feature(FeatureKind::Plane(RefPlane::ThreePoints {
        points: points.map(|entity| PointRef::SketchPoint { sketch: s, entity }),
        flip: false,
    }));
    part.rebuild(&mut kernel);
    match part.status(plane_id) {
        Some(FeatureStatus::Error(message)) => assert_eq!(message, "the three points are in line"),
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
}

#[test]
fn planes_normal_to_curves_at_points() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (boss_id, [bottom, right, _, _]) = block(&mut part, 10.0);
    let front = FaceName::end_cap(boss_id);

    // Normal to the bottom front edge (along X) at its right end: the plane x = 40.
    let at_end = part.add_feature(FeatureKind::Plane(RefPlane::NormalToCurve {
        curve: CurveRef::Edge(EdgeName::between(bottom.clone(), front.clone())),
        point: corner([&bottom, &right, &front]),
        flip: false,
    }));

    // Normal to a circular edge at its seam vertex: a cylinder of radius 5 at (60, 0).
    let mut round = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let circle = round.circle(Point2::new(60.0, 0.0), 5.0);
    let s2 = part.add_feature(FeatureKind::Sketch(round));
    let cylinder = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s2,
        ExtrudeOp::NewBody,
        8.0,
    )));
    let side = FaceName::profile(cylinder, circle);
    let rim = EdgeName::between(side.clone(), FaceName::end_cap(cylinder));
    let seam = PointRef::Vertex(VertexName {
        faces: {
            let mut faces = vec![side.clone(), FaceName::end_cap(cylinder)];
            faces.sort();
            faces
        },
        part: 0,
    });
    let at_rim = part.add_feature(FeatureKind::Plane(RefPlane::NormalToCurve {
        curve: CurveRef::Edge(rim),
        point: seam.clone(),
        flip: false,
    }));

    // Normal to a sketch arc at its end, as for the profile of a sweep: a quarter circle
    // on Top from (100, 0, 0), centre (110, 0, 0), counter-clockwise to (110, 0, -10).
    let mut path = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    let arc = path.geometry.arc(
        Point2::new(110.0, 0.0),
        Point2::new(100.0, 0.0),
        Point2::new(110.0, -10.0),
    );
    let start = match path.geometry.entity(arc).map(|e| e.geometry) {
        Some(oxyde_sketch::Geometry::Arc { start, .. }) => start,
        _ => unreachable!(),
    };
    let s3 = part.add_feature(FeatureKind::Sketch(path));
    let on_path = part.add_feature(FeatureKind::Plane(RefPlane::NormalToCurve {
        curve: CurveRef::SketchCurve {
            sketch: s3,
            entity: arc.0,
        },
        point: PointRef::SketchPoint {
            sketch: s3,
            entity: start.0,
        },
        flip: false,
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);

    let p = plane(&part, at_end);
    assert_point(p.origin, Point3::new(40.0, 0.0, 10.0));
    assert!(p.normal.dot(Vec3::X).abs() > 1.0 - 1e-9, "{p:?}");

    let p = plane(&part, at_rim);
    let centre = Point3::new(60.0, 0.0, 8.0);
    assert_close(p.origin.z, 8.0);
    assert_close(p.origin.distance(centre), 5.0);
    // Along the circle there: square to its radius and to its axis.
    assert_close(p.normal.dot(Vec3::Z), 0.0);
    assert_close(p.normal.dot(p.origin - centre), 0.0);

    // On Top, sketch x is model X and sketch y is -Z: the arc starts at (100, 0, 0)
    // heading along sketch -y, that is +Z.
    let p = plane(&part, on_path);
    assert_point(p.origin, Point3::new(100.0, 0.0, 0.0));
    assert_direction(p.normal, Vec3::Z);
    part.release(&mut kernel);
}

#[test]
fn the_kernel_finds_where_an_edge_runs_near_a_point() {
    let mut kernel = OcctKernel::new();
    let shape = kernel
        .make_box(Point3::ORIGIN, Vec3::new(40.0, 20.0, 10.0))
        .unwrap();
    let topology = kernel.topology(shape).unwrap();
    let (index, edge) = topology
        .edges
        .iter()
        .enumerate()
        .find(|(_, e)| {
            let m = e.midpoint;
            (m.y - 0.0).abs() < 1e-9 && (m.z - 0.0).abs() < 1e-9
        })
        .expect("the edge along X through the origin");
    assert_close(edge.length, 40.0);
    // Off the edge: its nearest point, and the edge's direction, along X either way.
    let near = kernel
        .edge_tangent(shape, index as u32, Point3::new(12.0, 3.0, -4.0))
        .unwrap()
        .expect("the OpenCASCADE kernel answers");
    assert_point(near.point, Point3::new(12.0, 0.0, 0.0));
    assert_close(near.tangent.dot(Vec3::X).abs(), 1.0);
    // Past its end: the end.
    let past = kernel
        .edge_tangent(shape, index as u32, Point3::new(55.0, 0.0, 0.0))
        .unwrap()
        .unwrap();
    assert_point(past.point, Point3::new(40.0, 0.0, 0.0));
    kernel.release(shape);
}

#[test]
fn a_sketch_line_is_a_hinge_and_a_deleted_one_is_reported() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    // A centerline on Front along Y at x = 10.
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let line = sketch
        .geometry
        .line(Point2::new(10.0, 0.0), Point2::new(10.0, 5.0));
    sketch.geometry.set_construction(line, true);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let plane_id = part.add_feature(FeatureKind::Plane(RefPlane::Angle {
        base: PlaneRef::Standard(StandardPlane::Front),
        line: AxisRef::SketchLine {
            sketch: s,
            entity: line.0,
        },
        angle: 90f64.to_radians(),
        flip: false,
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    // Front turned a quarter turn about +Y: its normal +Z goes to +X, through x = 10.
    let p = plane(&part, plane_id);
    assert_direction(p.normal, Vec3::X);
    assert_close(p.origin.x, 10.0);

    // Without the line, the plane says what it lost.
    let emptied = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    part.replace_feature(s, FeatureKind::Sketch(emptied))
        .unwrap();
    part.rebuild(&mut kernel);
    match part.status(plane_id) {
        Some(FeatureStatus::Error(message)) => {
            assert!(message.contains("no longer exists"), "{message}");
        }
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
}
