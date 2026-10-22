//! The native kernel (`oxyde-native-kernel`) against OpenCASCADE: the same parts and shapes
//! built by both, compared on what the model and the user see: volume, centre of mass,
//! bounds, topology counts, the persistent name of every face, edge and vertex, and each
//! named face's area and normal. OpenCASCADE is the reference; the native kernel is
//! written from the mathematics (docs/ARCHITECTURE.md §10, Native kernel).

use oxyde_geom::{Axis, Plane, Point2, Point3, Transform, Vec3};
use oxyde_kernel::{Kernel, ShapeHandle, SurfaceKind, TopologyCounts};
use oxyde_model::{
    AxisRef, EdgeName, Extrude, ExtrudeExtent, ExtrudeOp, FaceName, FeatureId, FeatureKind,
    FeatureStatus, LinearPattern, Part, PatternDirection, Sketch, SketchPlane, StandardAxis,
    StandardPlane, VertexName,
};
use oxyde_native_kernel::NativeKernel;
use oxyde_occt::OcctKernel;

/// What a kernel made of a shape, as the comparisons read it.
#[derive(Debug)]
struct Measures {
    volume: f64,
    centre: Point3,
    bounds: (Point3, Point3),
    counts: TopologyCounts,
}

fn measures<K: Kernel>(kernel: &K, shape: ShapeHandle) -> Measures {
    Measures {
        volume: kernel.volume(shape).unwrap(),
        centre: kernel.centre_of_mass(shape).unwrap(),
        bounds: kernel.bounds(shape).unwrap(),
        counts: kernel.topology_counts(shape).unwrap(),
    }
}

fn assert_close(what: &str, native: f64, occt: f64) {
    assert!(
        (native - occt).abs() <= 1e-6 * occt.abs().max(1.0),
        "{what}: native {native}, OpenCASCADE {occt}"
    );
}

fn assert_point_close(what: &str, native: Point3, occt: Point3) {
    assert!(
        native.distance(occt) <= 1e-6 * occt.to_vec().length().max(1.0),
        "{what}: native {native:?}, OpenCASCADE {occt:?}"
    );
}

fn assert_same_measures(what: &str, native: &Measures, occt: &Measures) {
    assert_close(&format!("{what}, volume"), native.volume, occt.volume);
    assert_point_close(
        &format!("{what}, centre of mass"),
        native.centre,
        occt.centre,
    );
    assert_point_close(
        &format!("{what}, lower bound"),
        native.bounds.0,
        occt.bounds.0,
    );
    assert_point_close(
        &format!("{what}, upper bound"),
        native.bounds.1,
        occt.bounds.1,
    );
    assert_eq!(native.counts, occt.counts, "{what}, topology counts");
}

/// A face as the model sees it: its name, its kind of surface, its area and centroid,
/// and a plane's normal or a curved face's axis.
#[derive(Debug)]
struct SeenFace {
    name: FaceName,
    surface: SurfaceKind,
    area: f64,
    centroid: Point3,
    normal: Option<Vec3>,
    axis: Option<Axis>,
}

/// A part's body as built by one kernel: the measures of each solid and every name, with
/// each face's surface, area, centroid and normal or axis, sorted by name.
#[derive(Debug)]
struct Built {
    solids: Vec<Measures>,
    faces: Vec<SeenFace>,
    edges: Vec<EdgeName>,
    vertices: Vec<VertexName>,
}

fn build<K: Kernel>(part: &Part, kernel: &mut K) -> Built {
    let mut part = Part::from_definition(part.definition()).unwrap();
    part.rebuild(kernel);
    for entry in part.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{}", entry.name);
    }
    let body = part.body().expect("a body");
    let solids = body
        .solids
        .iter()
        .map(|s| measures(kernel, s.shape))
        .collect();
    let mut faces: Vec<SeenFace> = body
        .naming
        .faces()
        .iter()
        .zip(&body.topology.faces)
        .map(|(name, face)| SeenFace {
            name: name.clone(),
            surface: face.surface,
            area: face.area,
            centroid: face.centroid,
            normal: face.normal,
            axis: face.axis,
        })
        .collect();
    faces.sort_by(|a, b| a.name.cmp(&b.name));
    let mut edges = body.naming.edges().to_vec();
    edges.sort();
    let mut vertices = body.naming.vertices().to_vec();
    vertices.sort();
    part.release(kernel);
    Built {
        solids,
        faces,
        edges,
        vertices,
    }
}

/// Builds `part` with both kernels and compares everything [`Built`] holds.
fn assert_same_part(what: &str, part: &Part) {
    let occt = build(part, &mut OcctKernel::new());
    let native = build(part, &mut NativeKernel::new());
    assert_eq!(native.solids.len(), occt.solids.len(), "{what}: bodies");
    for (i, (n, o)) in native.solids.iter().zip(&occt.solids).enumerate() {
        assert_same_measures(&format!("{what}, body {i}"), n, o);
    }
    let names =
        |faces: &[SeenFace]| -> Vec<FaceName> { faces.iter().map(|f| f.name.clone()).collect() };
    assert_eq!(
        names(&native.faces),
        names(&occt.faces),
        "{what}: face names"
    );
    assert_eq!(native.edges, occt.edges, "{what}: edge names");
    assert_eq!(native.vertices, occt.vertices, "{what}: vertex names");
    for (n, o) in native.faces.iter().zip(&occt.faces) {
        let name = &n.name;
        assert_eq!(n.surface, o.surface, "{what}, surface of {name:?}");
        assert_close(&format!("{what}, area of {name:?}"), n.area, o.area);
        assert_point_close(
            &format!("{what}, centroid of {name:?}"),
            n.centroid,
            o.centroid,
        );
        match (n.normal, o.normal) {
            (Some(n), Some(o)) => assert!(
                (n - o).length() < 1e-9,
                "{what}, normal of {name:?}: native {n:?}, OpenCASCADE {o:?}"
            ),
            (None, None) => {}
            other => panic!("{what}, normal of {name:?}: {other:?}"),
        }
        match (n.axis, o.axis) {
            (Some(n), Some(o)) => {
                assert!(
                    (n.direction - o.direction).length() < 1e-9,
                    "{what}, axis of {name:?}: native {n:?}, OpenCASCADE {o:?}"
                );
                assert_point_close(&format!("{what}, axis of {name:?}"), n.origin, o.origin);
            }
            (None, None) => {}
            other => panic!("{what}, axis of {name:?}: {other:?}"),
        }
    }
}

/// A closed polygon of lines through `corners`, in the sketch's coordinates.
fn polygon(sketch: &mut Sketch, corners: &[(f64, f64)]) {
    for (i, &(x, y)) in corners.iter().enumerate() {
        let (nx, ny) = corners[(i + 1) % corners.len()];
        sketch.line(Point2::new(x, y), Point2::new(nx, ny));
    }
}

fn extrude(
    part: &mut Part,
    sketch: Sketch,
    op: ExtrudeOp,
    extent: ExtrudeExtent,
    reverse: bool,
) -> FeatureId {
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let mut extrude = Extrude::blind(s, op, 1.0);
    extrude.extent = extent;
    extrude.reverse = reverse;
    part.add_feature(FeatureKind::Extrude(extrude))
}

fn boss(part: &mut Part, sketch: Sketch, extent: ExtrudeExtent, reverse: bool) -> FeatureId {
    extrude(part, sketch, ExtrudeOp::Boss, extent, reverse)
}

fn front() -> Sketch {
    Sketch::new(SketchPlane::Standard(StandardPlane::Front))
}

/// A 40 × 30 × 10 block on the Front plane: its top, at z = 10, is the extrusion's end cap.
fn base_block(part: &mut Part) -> FeatureId {
    let mut sketch = front();
    sketch.rectangle(Point2::new(0.0, 0.0), 40.0, 30.0);
    boss(part, sketch, ExtrudeExtent::Blind { depth: 10.0 }, false)
}

/// A sketch on the top of `base_block`, whose coordinates are the model's x and y.
fn on_top(base: FeatureId) -> Sketch {
    Sketch::new(SketchPlane::Face(FaceName::end_cap(base)))
}

/// A counter-clockwise arc about `center` from `start` to `end`, in the sketch's coordinates.
fn arc(sketch: &mut Sketch, center: (f64, f64), start: (f64, f64), end: (f64, f64)) {
    sketch.geometry.arc(
        Point2::new(center.0, center.1),
        Point2::new(start.0, start.1),
        Point2::new(end.0, end.1),
    );
}

#[test]
fn a_round_boss_has_a_seam() {
    let mut part = Part::new();
    let mut sketch = front();
    sketch.circle(Point2::new(10.0, 10.0), 5.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 8.0 },
        false,
    );
    assert_same_part("round boss", &part);
}

#[test]
fn circles_on_every_standard_plane_either_way() {
    for plane in [
        StandardPlane::Front,
        StandardPlane::Top,
        StandardPlane::Right,
    ] {
        for (extent, reverse) in [
            (ExtrudeExtent::Blind { depth: 6.0 }, false),
            (ExtrudeExtent::Blind { depth: 6.0 }, true),
            (ExtrudeExtent::MidPlane { depth: 6.0 }, false),
        ] {
            let mut part = Part::new();
            let mut sketch = Sketch::new(SketchPlane::Standard(plane));
            sketch.circle(Point2::new(-3.0, 7.0), 4.5);
            boss(&mut part, sketch, extent.clone(), reverse);
            assert_same_part(
                &format!("circle on {plane:?}, {extent:?}, {reverse}"),
                &part,
            );
        }
    }
}

#[test]
fn a_slot_of_lines_and_half_circles() {
    let mut part = Part::new();
    let mut sketch = front();
    sketch.line(Point2::new(0.0, 0.0), Point2::new(20.0, 0.0));
    arc(&mut sketch, (20.0, 5.0), (20.0, 0.0), (20.0, 10.0));
    sketch.line(Point2::new(20.0, 10.0), Point2::new(0.0, 10.0));
    arc(&mut sketch, (0.0, 5.0), (0.0, 10.0), (0.0, 0.0));
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 4.0 },
        false,
    );
    assert_same_part("slot", &part);
}

#[test]
fn a_rounded_corner_and_a_notch() {
    // A quarter round on one corner, bulging out, and a half-round notch cut into the
    // opposite side, bulging in.
    let mut part = Part::new();
    let mut sketch = front();
    sketch.line(Point2::new(0.0, 0.0), Point2::new(30.0, 0.0));
    sketch.line(Point2::new(30.0, 0.0), Point2::new(30.0, 15.0));
    arc(&mut sketch, (25.0, 15.0), (30.0, 15.0), (25.0, 20.0));
    sketch.line(Point2::new(25.0, 20.0), Point2::new(14.0, 20.0));
    arc(&mut sketch, (10.0, 20.0), (14.0, 20.0), (6.0, 20.0));
    sketch.line(Point2::new(6.0, 20.0), Point2::new(0.0, 20.0));
    sketch.line(Point2::new(0.0, 20.0), Point2::new(0.0, 0.0));
    boss(&mut part, sketch, ExtrudeExtent::Blind { depth: 5.0 }, true);
    assert_same_part("rounded corner and notch", &part);
}

#[test]
fn a_plate_with_round_holes_in_its_profile() {
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    sketch.rectangle(Point2::new(0.0, 0.0), 40.0, 30.0);
    sketch.circle(Point2::new(10.0, 15.0), 6.0);
    sketch.circle(Point2::new(28.0, 12.0), 4.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 10.0 },
        false,
    );
    assert_same_part("plate with round holes", &part);
}

#[test]
fn display_meshes_of_curved_faces_follow_them() {
    let mut part = Part::new();
    let mut sketch = front();
    sketch.rectangle(Point2::new(0.0, 0.0), 40.0, 30.0);
    sketch.circle(Point2::new(20.0, 15.0), 6.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 10.0 },
        false,
    );
    let mut kernel = NativeKernel::new();
    part.rebuild(&mut kernel);
    let body = part.body().expect("a body");
    let shape = body.solids[0].shape;
    let topology = kernel.topology(shape).unwrap();
    let deflection = 0.01;
    let mesh = kernel.tessellate(shape, deflection).unwrap();
    assert_eq!(mesh.faces.len(), topology.faces.len());
    assert_eq!(mesh.edges.len(), topology.edges.len());
    for (i, (face, info)) in mesh.faces.iter().zip(&topology.faces).enumerate() {
        let mut area = 0.0;
        for t in &face.triangles {
            let p = |k: u32| {
                let [x, y, z] = face.positions[k as usize];
                Point3::new(f64::from(x), f64::from(y), f64::from(z))
            };
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let twice = (b - a).cross(c - a);
            // Outward: along a plane's normal; a hole's wall faces its axis.
            let outward = match (info.normal, info.axis) {
                (Some(normal), _) => normal,
                (None, Some(axis)) => {
                    let centre = Point3::new(
                        (a.x + b.x + c.x) / 3.0,
                        (a.y + b.y + c.y) / 3.0,
                        (a.z + b.z + c.z) / 3.0,
                    );
                    let d = centre - axis.origin;
                    -(d - axis.direction * d.dot(axis.direction))
                }
                (None, None) => panic!("face {i}: neither a normal nor an axis"),
            };
            assert!(
                twice.dot(outward) > 0.0,
                "face {i}: a triangle faces inwards"
            );
            area += twice.length() / 2.0;
        }
        // Chords a deflection off a circle of radius 6 lose a share of the area of about
        // two thirds of deflection / radius.
        assert!(
            (area - info.area).abs() <= 2e-3 * info.area,
            "face {i}: the mesh covers {area} mm², the face {}",
            info.area
        );
    }
}

#[test]
fn a_block_on_the_front_plane() {
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::new(5.0, -3.0), 40.0, 20.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 10.0 },
        false,
    );
    assert_same_part("block", &part);
}

#[test]
fn an_l_reversed_on_the_top_plane() {
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    polygon(
        &mut sketch,
        &[
            (0.0, 0.0),
            (30.0, 0.0),
            (30.0, 8.0),
            (8.0, 8.0),
            (8.0, 25.0),
            (0.0, 25.0),
        ],
    );
    boss(&mut part, sketch, ExtrudeExtent::Blind { depth: 6.5 }, true);
    assert_same_part("L", &part);
}

#[test]
fn a_frame_mid_plane_on_the_right_plane() {
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    sketch.rectangle(Point2::new(-20.0, -15.0), 40.0, 30.0);
    // A hole that is not a rectangle, off centre: the cap is not a simple ring.
    polygon(
        &mut sketch,
        &[(-12.0, -8.0), (6.0, -10.0), (10.0, 9.0), (-4.0, 4.0)],
    );
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::MidPlane { depth: 8.0 },
        false,
    );
    assert_same_part("frame", &part);
}

#[test]
fn two_regions_of_one_sketch() {
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::new(0.0, 0.0), 10.0, 10.0);
    polygon(&mut sketch, &[(20.0, 0.0), (32.0, 0.0), (26.0, 9.0)]);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 4.0 },
        false,
    );
    assert_same_part("two regions", &part);
}

#[test]
fn boxes_moved_and_mirrored() {
    let mut occt = OcctKernel::new();
    let mut native = NativeKernel::new();
    let (corner, size) = (Point3::new(-3.0, 2.0, 7.0), Vec3::new(12.0, 5.0, 3.5));
    let o = occt.make_box(corner, size).unwrap();
    let n = native.make_box(corner, size).unwrap();
    assert_same_measures("box", &measures(&native, n), &measures(&occt, o));

    let axis = Axis::new(Point3::new(1.0, -2.0, 0.5), Vec3::new(1.0, 2.0, 3.0)).unwrap();
    let motions = [
        (
            "rotated and moved",
            Transform::rotation(&axis, 0.7)
                .then(&Transform::translation(Vec3::new(4.0, 0.0, -9.0))),
        ),
        ("mirrored", Transform::mirror(&Plane::TOP.offset(3.0))),
    ];
    for (what, motion) in motions {
        let moved_o = occt.transform(o, &motion).unwrap();
        let moved_n = native.transform(n, &motion).unwrap();
        assert_same_measures(
            what,
            &measures(&native, moved_n.shape),
            &measures(&occt, moved_o.shape),
        );
        assert_eq!(
            moved_n.history.faces.len(),
            native.topology(moved_n.shape).unwrap().faces.len(),
            "{what}: one history entry per face"
        );
        occt.release(moved_o.shape);
        native.release(moved_n.shape);
    }
    occt.release(o);
    native.release(n);
}

#[test]
fn display_meshes_cover_each_face_once_and_face_outwards() {
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::new(0.0, 0.0), 40.0, 30.0);
    polygon(
        &mut sketch,
        &[(5.0, 5.0), (15.0, 5.0), (15.0, 12.0), (5.0, 12.0)],
    );
    polygon(&mut sketch, &[(22.0, 8.0), (34.0, 6.0), (30.0, 24.0)]);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 5.0 },
        false,
    );
    let mut kernel = NativeKernel::new();
    part.rebuild(&mut kernel);
    let body = part.body().expect("a body");
    let solid = &body.solids[0];
    let topology = kernel.topology(solid.shape).unwrap();
    let mesh = kernel.tessellate(solid.shape, 0.1).unwrap();
    assert_eq!(mesh.faces.len(), topology.faces.len());
    assert_eq!(mesh.edges.len(), topology.edges.len());
    for (i, (face, info)) in mesh.faces.iter().zip(&topology.faces).enumerate() {
        let normal = info.normal.expect("a planar face");
        let mut area = 0.0;
        for t in &face.triangles {
            let p = |k: u32| {
                let [x, y, z] = face.positions[k as usize];
                Point3::new(f64::from(x), f64::from(y), f64::from(z))
            };
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let twice = (b - a).cross(c - a);
            assert!(
                twice.dot(normal) > 0.0,
                "face {i}: a triangle faces inwards"
            );
            area += twice.length() / 2.0;
        }
        // Positions are single precision.
        assert!(
            (area - info.area).abs() <= 1e-4 * info.area,
            "face {i}: the mesh covers {area} mm², the face {}",
            info.area
        );
    }
    part.release(&mut kernel);
}

#[test]
fn a_boss_on_a_face() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.rectangle(Point2::new(10.0, 8.0), 15.0, 12.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 6.0 },
        false,
    );
    assert_same_part("boss on a face", &part);
}

#[test]
fn a_pocket_cut_from_a_face() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    polygon(
        &mut sketch,
        &[
            (5.0, 5.0),
            (25.0, 4.0),
            (30.0, 20.0),
            (12.0, 24.0),
            (8.0, 14.0),
        ],
    );
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::Blind { depth: 4.0 },
        false,
    );
    assert_same_part("pocket", &part);
}

#[test]
fn a_hole_through_all() {
    let mut part = Part::new();
    base_block(&mut part);
    let mut sketch = front();
    polygon(&mut sketch, &[(10.0, 10.0), (22.0, 9.0), (16.0, 21.0)]);
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
        false,
    );
    assert_same_part("hole through all", &part);
}

#[test]
fn blocks_flush_with_each_other_make_one_box() {
    let mut part = Part::new();
    base_block(&mut part);
    let mut sketch = front();
    sketch.rectangle(Point2::new(20.0, 0.0), 40.0, 30.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 10.0 },
        false,
    );
    assert_same_part("flush blocks", &part);
}

#[test]
fn a_slot_through_the_middle_leaves_two_solids() {
    let mut part = Part::new();
    base_block(&mut part);
    let mut sketch = front();
    sketch.rectangle(Point2::new(15.0, -5.0), 10.0, 40.0);
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
        false,
    );
    assert_same_part("slot", &part);
}

#[test]
fn a_pattern_of_pockets() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.rectangle(Point2::new(4.0, 10.0), 5.0, 8.0);
    let pocket = extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::Blind { depth: 3.0 },
        false,
    );
    part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![pocket],
        first: PatternDirection {
            axis: AxisRef::Standard(StandardAxis::X),
            spacing: 11.0,
            count: 3,
            reverse: false,
        },
        second: None,
        skip: Vec::new(),
    }));
    assert_same_part("pattern of pockets", &part);
}

#[test]
fn an_intersection_keeps_what_both_share() {
    let mut part = Part::new();
    base_block(&mut part);
    let mut sketch = front();
    polygon(
        &mut sketch,
        &[(-5.0, -5.0), (25.0, -5.0), (25.0, 18.0), (-5.0, 12.0)],
    );
    // On the Front plane, under the block: an intersection, like a cut, goes against the
    // sketch's normal unless reversed (into the material from a face it is sketched on), so
    // it is reversed to reach up into the block.
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Intersect,
        ExtrudeExtent::Blind { depth: 6.0 },
        true,
    );
    assert_same_part("intersection", &part);
}

#[test]
fn what_the_native_kernel_cannot_do_yet_fails_the_feature_and_keeps_the_body() {
    let mut part = Part::new();
    let mut sketch = front();
    sketch.rectangle(Point2::new(0.0, 0.0), 20.0, 20.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 5.0 },
        false,
    );
    // A second boss joins the first.
    let mut sketch = front();
    sketch.rectangle(Point2::new(10.0, 10.0), 20.0, 20.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 5.0 },
        false,
    );
    // A cylinder joined to them: the native kernel extrudes the circle but cannot join
    // curved faces yet.
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    sketch.circle(Point2::new(50.0, 0.0), 4.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 5.0 },
        false,
    );

    let mut kernel = NativeKernel::new();
    part.rebuild(&mut kernel);
    let statuses: Vec<FeatureStatus> = part.timeline().into_iter().map(|e| e.status).collect();
    assert_eq!(statuses[1], FeatureStatus::Ok);
    assert_eq!(statuses[3], FeatureStatus::Ok);
    match &statuses[5] {
        FeatureStatus::Error(message) => {
            assert!(
                message.contains("join, cut or intersect curved faces"),
                "{message}"
            )
        }
        other => panic!("the cylinder should fail, not {other:?}"),
    }
    // The two blocks joined are still there, whole.
    let body = part.body().expect("a body");
    let volume: f64 = body
        .solids
        .iter()
        .map(|s| kernel.volume(s.shape).unwrap())
        .sum();
    assert!((volume - 3500.0).abs() < 1e-9, "{volume}");
    part.release(&mut kernel);
}
