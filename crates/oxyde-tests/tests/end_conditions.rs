//! Extrusion end conditions besides a blind depth: Through All (one side or both) and Up
//! To Surface (a plane on either side, parallel or slanted), built by OpenCASCADE and
//! checked against volumes worked out by hand; and the kernel's shortest distance, which
//! Measure reports, on parts like them.

use std::f64::consts::PI;

use oxyde_geom::Point2;
use oxyde_kernel::Kernel;
use oxyde_model::{
    BodyId, Extrude, ExtrudeExtent, ExtrudeOp, FaceName, FeatureId, FeatureKind, FeatureStatus,
    Part, PlaneRef, RefPlane, Resolution, Sketch, SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;

fn volume(part: &Part, kernel: &OcctKernel) -> f64 {
    part.body()
        .expect("a body")
        .solids
        .iter()
        .map(|s| kernel.volume(s.shape).unwrap())
        .sum()
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-4 * expected.abs().max(1.0),
        "expected {expected}, got {actual}"
    );
}

fn error(part: &Part, feature: FeatureId) -> String {
    match part.status(feature) {
        Some(FeatureStatus::Error(message)) => message,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn ok(part: &Part, feature: FeatureId) {
    assert_eq!(part.status(feature), Some(FeatureStatus::Ok));
}

/// A 40 × 20 block on the Front plane, 10 deep along +Z.
fn block(part: &mut Part) -> FeatureId {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::ORIGIN, 40.0, 20.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        10.0,
    )))
}

/// A plane parallel to Front, at `z`.
fn front_at(part: &mut Part, z: f64) -> FeatureId {
    part.add_feature(FeatureKind::Plane(RefPlane::Offset {
        base: PlaneRef::Standard(StandardPlane::Front),
        offset: z,
        flip: false,
    }))
}

/// A circle of radius `r` at (`x`, `y`) on `plane`.
fn circle(part: &mut Part, plane: SketchPlane, x: f64, y: f64, r: f64) -> FeatureId {
    let mut sketch = Sketch::new(plane);
    sketch.circle(Point2::new(x, y), r);
    part.add_feature(FeatureKind::Sketch(sketch))
}

fn extrude(sketch: FeatureId, op: ExtrudeOp, extent: ExtrudeExtent) -> FeatureKind {
    FeatureKind::Extrude(Extrude {
        sketch,
        op,
        extent,
        reverse: false,
        bodies: Vec::new(),
    })
}

fn set_extent(part: &mut Part, feature: FeatureId, extent: ExtrudeExtent, reverse: bool) {
    match part.edit(feature).unwrap() {
        FeatureKind::Extrude(e) => {
            e.extent = extent;
            e.reverse = reverse;
        }
        _ => unreachable!(),
    }
}

#[test]
fn a_cut_through_all_goes_through_one_side_or_both() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    block(&mut part);
    // Sketched halfway through the block.
    let middle = front_at(&mut part, 5.0);
    let s = circle(&mut part, SketchPlane::Reference(middle), 10.0, 10.0, 3.0);
    let cut = part.add_feature(extrude(
        s,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: false },
    ));
    part.rebuild(&mut kernel);
    ok(&part, cut);
    // A cut goes against the sketch normal: down to z = 0, half the block.
    let hole = PI * 9.0 * 10.0;
    close(volume(&part, &kernel), 8000.0 - hole / 2.0);
    // Through All has no depth to put in an equation.
    assert!(part.find_dimension("D1@Cut-Extrude1").is_none());

    set_extent(
        &mut part,
        cut,
        ExtrudeExtent::ThroughAll { both: true },
        false,
    );
    part.rebuild(&mut kernel);
    ok(&part, cut);
    close(volume(&part, &kernel), 8000.0 - hole);

    set_extent(
        &mut part,
        cut,
        ExtrudeExtent::ThroughAll { both: false },
        true,
    );
    part.rebuild(&mut kernel);
    ok(&part, cut);
    close(volume(&part, &kernel), 8000.0 - hole / 2.0);
    part.release(&mut kernel);
}

#[test]
fn a_boss_through_all_ends_level_with_the_furthest_geometry() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let first = circle(
        &mut part,
        SketchPlane::Standard(StandardPlane::Front),
        0.0,
        0.0,
        1.0,
    );
    // Nothing to go through yet.
    let lonely = part.add_feature(extrude(
        first,
        ExtrudeOp::Boss,
        ExtrudeExtent::ThroughAll { both: false },
    ));
    part.rebuild(&mut kernel);
    assert!(error(&part, lonely).contains("needs a body"));
    part.set_suppressed(lonely, true).unwrap();

    let base = block(&mut part);
    let below = front_at(&mut part, -20.0);
    let s = circle(&mut part, SketchPlane::Reference(below), 10.0, 10.0, 3.0);
    let pin = part.add_feature(extrude(
        s,
        ExtrudeOp::Boss,
        ExtrudeExtent::ThroughAll { both: false },
    ));
    part.rebuild(&mut kernel);
    ok(&part, base);
    ok(&part, pin);
    // From z = -20 to the block's far face at z = 10; inside the block it adds nothing.
    close(volume(&part, &kernel), 8000.0 + PI * 9.0 * 20.0);
    let body = part.body().unwrap();
    let (_, hi) = kernel.bounds(body.solids[0].shape).unwrap();
    assert!((hi.z - 10.0).abs() < 1e-6, "ends at z = {}", hi.z);

    // Away from the block there is nothing to go through.
    set_extent(
        &mut part,
        pin,
        ExtrudeExtent::ThroughAll { both: false },
        true,
    );
    part.rebuild(&mut kernel);
    assert!(error(&part, pin).contains("nothing to go through"));
    part.release(&mut kernel);
}

#[test]
fn up_to_surface_goes_to_a_plane_on_either_side() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let ahead = front_at(&mut part, 25.0);
    let behind = front_at(&mut part, -15.0);
    let s = circle(
        &mut part,
        SketchPlane::Standard(StandardPlane::Front),
        0.0,
        0.0,
        2.0,
    );
    let rod = part.add_feature(extrude(
        s,
        ExtrudeOp::Boss,
        ExtrudeExtent::UpToSurface {
            plane: PlaneRef::Reference(ahead),
        },
    ));
    part.rebuild(&mut kernel);
    ok(&part, rod);
    close(volume(&part, &kernel), PI * 4.0 * 25.0);
    let end = part
        .body()
        .unwrap()
        .naming
        .resolve_face(&FaceName::end_cap(rod));
    assert!(matches!(end, Resolution::Exact(_)), "{end:?}");
    // The plane depends on the feature, which must come after it.
    assert!(part.dependents(ahead).contains(&rod));

    // Behind the sketch: the extrusion goes that way by itself.
    set_extent(
        &mut part,
        rod,
        ExtrudeExtent::UpToSurface {
            plane: PlaneRef::Reference(behind),
        },
        false,
    );
    part.rebuild(&mut kernel);
    ok(&part, rod);
    close(volume(&part, &kernel), PI * 4.0 * 15.0);

    for (plane, why) in [
        (
            PlaneRef::Standard(StandardPlane::Front),
            "lies on that surface",
        ),
        (PlaneRef::Standard(StandardPlane::Top), "parallel"),
    ] {
        set_extent(&mut part, rod, ExtrudeExtent::UpToSurface { plane }, false);
        part.rebuild(&mut kernel);
        assert!(error(&part, rod).contains(why), "{}", error(&part, rod));
    }
    part.release(&mut kernel);
}

/// A wedge on the Front plane, 10 deep: its top runs from (0, 10) up to (40, 20), the
/// plane y = 10 + x/4. Returns the feature and the id of the top line.
fn wedge(part: &mut Part) -> (FeatureId, u32) {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let g = &mut sketch.geometry;
    let p: Vec<_> = [(0.0, 0.0), (40.0, 0.0), (40.0, 20.0), (0.0, 10.0)]
        .into_iter()
        .map(|(x, y)| g.point(Point2::new(x, y)))
        .collect();
    g.line_between(p[0], p[1]);
    g.line_between(p[1], p[2]);
    let top = g.line_between(p[2], p[3]);
    g.line_between(p[3], p[0]);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let w = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        10.0,
    )));
    (w, top.0)
}

#[test]
fn up_to_a_slanted_face_ends_along_its_plane() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (w, top) = wedge(&mut part);
    // Under the wedge, centred on x = 20 (z = 5): Top's sketch y points to −Z.
    let s = circle(
        &mut part,
        SketchPlane::Standard(StandardPlane::Top),
        20.0,
        -5.0,
        2.0,
    );
    let column = part.add_feature(extrude(
        s,
        ExtrudeOp::NewBody,
        ExtrudeExtent::UpToSurface {
            plane: PlaneRef::Face(FaceName::profile(w, top)),
        },
    ));
    part.rebuild(&mut kernel);
    ok(&part, w);
    ok(&part, column);
    let body = part.body().unwrap();
    assert_eq!(body.solids.len(), 2);
    let made = body.solid(BodyId::new(column, 0)).expect("the new body");
    // Its height under the plane averages 10 + 20/4 = 15 over the disc.
    close(kernel.volume(made.shape).unwrap(), PI * 4.0 * 15.0);
    close(
        volume(&part, &kernel),
        (10.0 + 20.0) / 2.0 * 40.0 * 10.0 + PI * 4.0 * 15.0,
    );
    // The slanted end is the extrusion's end cap; its side keeps its name.
    for name in [FaceName::end_cap(column), FaceName::start_cap(column)] {
        let found = body.naming.resolve_face(&name);
        assert!(matches!(found, Resolution::Exact(_)), "{name}: {found:?}");
    }

    // From the Right plane the face's plane runs through the circle (y 8 to 16 meets it
    // between x = -8 and x = 24): some of it would go backwards.
    let across = circle(
        &mut part,
        SketchPlane::Standard(StandardPlane::Right),
        -5.0,
        12.0,
        4.0,
    );
    let bad = part.add_feature(extrude(
        across,
        ExtrudeOp::NewBody,
        ExtrudeExtent::UpToSurface {
            plane: PlaneRef::Face(FaceName::profile(w, top)),
        },
    ));
    part.rebuild(&mut kernel);
    assert!(error(&part, bad).contains("passes through the sketch"));
    part.release(&mut kernel);
}

/// Measure's distance: between the walls of two holes, and the two caps of a block.
#[test]
fn the_kernel_measures_the_shortest_distance_between_faces() {
    use oxyde_kernel::SubShape;
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let base = block(&mut part);
    let mut holes = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let left = holes.circle(Point2::new(10.0, 10.0), 3.0);
    let right = holes.circle(Point2::new(30.0, 10.0), 3.0);
    let s = part.add_feature(FeatureKind::Sketch(holes));
    let cut = part.add_feature(extrude(
        s,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
    ));
    part.rebuild(&mut kernel);
    ok(&part, cut);
    let body = part.body().unwrap();
    let solid = &body.solids[0];
    let face = |name: FaceName| {
        let i = body.naming.find_face(&name).unwrap();
        SubShape::Face(solid.local_face(i).unwrap())
    };
    let between = |a: FaceName, b: FaceName| {
        kernel
            .distance(solid.shape, face(a), solid.shape, face(b))
            .unwrap()
            .expect("OpenCASCADE measures distances")
    };
    // Hole centres 20 apart, radius 3: 14 between their walls, along X.
    let walls = between(FaceName::profile(cut, left), FaceName::profile(cut, right));
    close(walls.distance, 14.0);
    close((walls.on_b - walls.on_a).x.abs(), 14.0);
    let caps = between(FaceName::start_cap(base), FaceName::end_cap(base));
    close(caps.distance, 10.0);
    // The whole solid to one of its faces: they touch.
    let touching = kernel
        .distance(
            solid.shape,
            SubShape::Whole,
            solid.shape,
            face(FaceName::end_cap(base)),
        )
        .unwrap()
        .unwrap();
    assert!(touching.distance.abs() < 1e-9);
    part.release(&mut kernel);
}

/// Measure in an assembly measures moved copies of solids, finding their faces and
/// edges by index: a moved copy keeps them in order.
#[test]
fn a_moved_copy_keeps_its_faces_and_edges_in_order() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    block(&mut part);
    let mut holes = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    holes.circle(Point2::new(10.0, 10.0), 3.0);
    let s = part.add_feature(FeatureKind::Sketch(holes));
    part.add_feature(extrude(
        s,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
    ));
    part.rebuild(&mut kernel);
    let solid = &part.body().unwrap().solids[0];
    let shift = oxyde_geom::Vec3::new(100.0, -20.0, 7.0);
    let turn = oxyde_geom::Transform::rotation(&oxyde_geom::Axis::Z, 0.3);
    let moved = turn.then(&oxyde_geom::Transform::translation(shift));
    let copy = kernel.transform(solid.shape, &moved).unwrap();
    let (before, after) = (
        kernel.topology(solid.shape).unwrap(),
        kernel.topology(copy.shape).unwrap(),
    );
    assert_eq!(before.faces.len(), after.faces.len());
    assert_eq!(before.edges.len(), after.edges.len());
    for (a, b) in before.faces.iter().zip(&after.faces) {
        assert!(moved.apply_point(a.centroid).distance(b.centroid) < 1e-6);
    }
    for (a, b) in before.edges.iter().zip(&after.edges) {
        assert!(moved.apply_point(a.midpoint).distance(b.midpoint) < 1e-6);
    }
    kernel.release(copy.shape);
    part.release(&mut kernel);
}

/// A drawing's view: the block with a hole through it, seen from the front, has its
/// outline and the hole's circle visible; from the top, the hole's sides are hidden.
#[test]
fn hidden_lines_are_told_from_visible_ones() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    block(&mut part);
    let mut holes = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    holes.circle(Point2::new(10.0, 10.0), 3.0);
    let s = part.add_feature(FeatureKind::Sketch(holes));
    part.add_feature(extrude(
        s,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
    ));
    part.rebuild(&mut kernel);
    let shapes = part.body().unwrap().shapes();
    let extent = |lines: &[Vec<Point2>]| {
        lines.iter().flatten().fold(
            (
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ),
            |(x0, x1, y0, y1), p| (x0.min(p.x), x1.max(p.x), y0.min(p.y), y1.max(p.y)),
        )
    };
    // Front: looking along -Z, X to the right.
    let front = kernel
        .project_edges(&shapes, &oxyde_geom::Plane::FRONT, 0.01)
        .unwrap()
        .expect("OpenCASCADE removes hidden lines");
    let (x0, x1, y0, y1) = extent(&front.visible);
    assert!(x0.abs() < 1e-6 && (x1 - 40.0).abs() < 1e-6, "{x0}..{x1}");
    assert!(y0.abs() < 1e-6 && (y1 - 20.0).abs() < 1e-6, "{y0}..{y1}");
    // The hole is seen end on: a circle of radius 3 about (10, 10), visible.
    let on_circle = front
        .visible
        .iter()
        .flatten()
        .any(|p| (p.distance(Point2::new(10.0, 10.0)) - 3.0).abs() < 1e-3);
    assert!(on_circle);
    // From the top (looking down -Y, X to the right), the hole runs through the block
    // under the top face: its two sides are hidden lines at x = 7 and x = 13.
    let top = oxyde_geom::Plane::TOP;
    let above = kernel.project_edges(&shapes, &top, 0.01).unwrap().unwrap();
    for x in [7.0, 13.0] {
        assert!(
            above
                .hidden
                .iter()
                .flatten()
                .any(|p| (p.x - x).abs() < 1e-3),
            "a hidden line at x = {x}"
        );
    }
    part.release(&mut kernel);
}
