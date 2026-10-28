//! The native kernel (`oxyde-native-kernel`) against OpenCASCADE: the same parts and shapes
//! built by both, compared on what the model and the user see: volume, centre of mass,
//! bounds, topology counts, the persistent name of every face, edge and vertex, and each
//! named face's area and normal. OpenCASCADE is the reference; the native kernel is
//! written from the mathematics (docs/ARCHITECTURE.md §10, Native kernel).

use oxyde_geom::{Axis, Plane, Point2, Point3, Transform, Vec3};
use oxyde_kernel::{Kernel, ShapeHandle, SurfaceKind, TopologyCounts};
use oxyde_model::{
    AxisRef, ChamferShape, EdgeBlend, EdgeName, Extrude, ExtrudeExtent, ExtrudeOp, FaceKey,
    FaceName, FeatureId, FeatureKind, FeatureStatus, LinearPattern, Part, PatternDirection, Sketch,
    SketchPlane, StandardAxis, StandardPlane, VertexName,
};
use oxyde_native_kernel::NativeKernel;
use oxyde_occt::OcctKernel;

/// What a kernel made of a shape, as the comparisons read it.
#[derive(Debug, Clone, Copy)]
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

/// How close the two kernels' numbers must be: OpenCASCADE's are exact to about 1e-9 where
/// its faces are bounded by lines, circles and ellipses. Where it bounds them by B-splines
/// it fitted to where two surfaces cross, its areas and volumes are only as close as the
/// fit, a few parts in a million (a hole across another: 8e-6 off the exact area, which the
/// native kernel gets, `oxyde-native-kernel`'s tests check that against the integral).
const EXACT: f64 = 1e-6;
const FITTED: f64 = 1e-4;
const HIGH_DEGREE: f64 = 1e-3;

fn assert_close(what: &str, native: f64, occt: f64, tolerance: f64) {
    assert!(
        (native - occt).abs() <= tolerance * occt.abs().max(1.0),
        "{what}: native {native}, OpenCASCADE {occt}"
    );
}

fn assert_point_close(what: &str, native: Point3, occt: Point3, tolerance: f64) {
    assert!(
        native.distance(occt) <= tolerance * occt.to_vec().length().max(1.0),
        "{what}: native {native:?}, OpenCASCADE {occt:?}"
    );
}

fn assert_same_measures(what: &str, native: &Measures, occt: &Measures, tolerance: f64) {
    assert_close(
        &format!("{what}, volume"),
        native.volume,
        occt.volume,
        tolerance,
    );
    assert_point_close(
        &format!("{what}, centre of mass"),
        native.centre,
        occt.centre,
        tolerance,
    );
    assert_point_close(
        &format!("{what}, lower bound"),
        native.bounds.0,
        occt.bounds.0,
        tolerance,
    );
    assert_point_close(
        &format!("{what}, upper bound"),
        native.bounds.1,
        occt.bounds.1,
        tolerance,
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
    /// Whether some edge is a B-spline: a curve the kernel fitted to where two surfaces
    /// cross.
    fitted: bool,
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
    let fitted = body
        .topology
        .edges
        .iter()
        .any(|e| e.curve == oxyde_kernel::CurveKind::BSpline);
    part.release(kernel);
    Built {
        fitted,
        solids,
        faces,
        edges,
        vertices,
    }
}

/// Builds `part` with both kernels and compares everything [`Built`] holds.
fn assert_same_part(what: &str, part: &Part) {
    compare_parts(what, part, true);
}

/// As [`assert_same_part`], but for where OpenCASCADE cuts an edge at a point that is no
/// corner (only two edges meet there, between the same two faces) and the native kernel
/// leaves it whole: every face the same, the edges and vertices not counted.
fn assert_same_faces(what: &str, part: &Part) {
    compare_parts(what, part, false);
}

/// As [`assert_same_part`], the solids' volumes, centres and bounds held to [`EXACT`] even
/// where OpenCASCADE's edges or faces are B-splines: where the native kernel makes the same
/// B-splines (a loft's). A B-spline face's area and centroid are held to [`HIGH_DEGREE`]:
/// OpenCASCADE takes them by a Gauss rule of so many points, further off the higher the
/// surface's degree (a smooth loft through four sections, degree 3 across: 3e-6 of its
/// faces' areas; through seven, degree 6: 1.5e-4 of a face's centroid), where the native
/// kernel's match the integral to 1e-10 (`oxyde-native-kernel`'s loft tests check it).
fn assert_same_exactly(what: &str, part: &Part) {
    compare_parts_within(what, part, true, Some(EXACT));
}

fn compare_parts(what: &str, part: &Part, every_edge: bool) {
    compare_parts_within(what, part, every_edge, None);
}

fn compare_parts_within(what: &str, part: &Part, every_edge: bool, within: Option<f64>) {
    let occt = build(part, &mut OcctKernel::new());
    let native = build(part, &mut NativeKernel::new());
    let tolerance = within.unwrap_or(if occt.fitted { FITTED } else { EXACT });
    assert_eq!(native.solids.len(), occt.solids.len(), "{what}: bodies");
    for (i, (n, o)) in native.solids.iter().zip(&occt.solids).enumerate() {
        if every_edge {
            assert_same_measures(&format!("{what}, body {i}"), n, o, tolerance);
        } else {
            let faces_only = |m: &Measures| Measures {
                counts: TopologyCounts {
                    edges: 0,
                    vertices: 0,
                    ..m.counts
                },
                ..*m
            };
            assert_same_measures(
                &format!("{what}, body {i}"),
                &faces_only(n),
                &faces_only(o),
                tolerance,
            );
        }
    }
    let names =
        |faces: &[SeenFace]| -> Vec<FaceName> { faces.iter().map(|f| f.name.clone()).collect() };
    assert_eq!(
        names(&native.faces),
        names(&occt.faces),
        "{what}: face names"
    );
    if every_edge {
        assert_eq!(native.edges, occt.edges, "{what}: edge names");
        assert_eq!(native.vertices, occt.vertices, "{what}: vertex names");
    }
    // Where OpenCASCADE fits a B-spline surface to a blend (a bevel between a cylinder and
    // a cone on one axis), the native kernel makes the exact one, a cone: their areas only
    // as close as the fit.
    let any_bspline = occt.faces.iter().any(|f| f.surface == SurfaceKind::BSpline);
    for (n, o) in native.faces.iter().zip(&occt.faces) {
        let name = &n.name;
        let tolerance = match within {
            Some(_) if o.surface == SurfaceKind::BSpline => HIGH_DEGREE,
            Some(t) => t,
            None if any_bspline => FITTED,
            None => tolerance,
        };
        let fitted_blend = o.surface == SurfaceKind::BSpline
            && n.surface != SurfaceKind::Plane
            && matches!(
                name.key,
                FaceKey::FromEdge { .. } | FaceKey::FromVertex { .. }
            );
        if !fitted_blend {
            assert_eq!(n.surface, o.surface, "{what}, surface of {name:?}");
        }
        assert_close(
            &format!("{what}, area of {name:?}"),
            n.area,
            o.area,
            tolerance,
        );
        assert_point_close(
            &format!("{what}, centroid of {name:?}"),
            n.centroid,
            o.centroid,
            tolerance,
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
            // A round's cylinder runs along its edge, each kernel taking its own edge's way,
            // and a torus the way that cylinder does: the same line, either way along it.
            // A sphere is the same about any line through its centre.
            (Some(n), Some(o))
                if loose_axis(name, n_surface(&native, name))
                    && n_surface(&native, name) == SurfaceKind::Sphere =>
            {
                assert_point_close(
                    &format!("{what}, centre of {name:?}"),
                    n.origin,
                    o.origin,
                    EXACT,
                );
            }
            (Some(n), Some(o)) if loose_axis(name, n_surface(&native, name)) => {
                assert!(
                    n.direction.cross(o.direction).length() < 1e-9,
                    "{what}, axis of {name:?}: native {n:?}, OpenCASCADE {o:?}"
                );
                let off = n.origin - o.origin;
                let off = off - o.direction * off.dot(o.direction);
                assert!(
                    off.length() <= EXACT * o.origin.to_vec().length().max(1.0),
                    "{what}, axis of {name:?}: native {n:?}, OpenCASCADE {o:?}"
                );
            }
            (Some(n), Some(o)) => {
                assert!(
                    (n.direction - o.direction).length() < 1e-9,
                    "{what}, axis of {name:?}: native {n:?}, OpenCASCADE {o:?}"
                );
                assert_point_close(
                    &format!("{what}, axis of {name:?}"),
                    n.origin,
                    o.origin,
                    EXACT,
                );
            }
            (None, None) => {}
            // OpenCASCADE's fitted surface has no axis.
            (Some(_), None) if fitted_blend => {}
            other => panic!("{what}, axis of {name:?}: {other:?}"),
        }
    }
}

/// The kind of surface the native kernel gave the face named `name`.
fn n_surface(built: &Built, name: &FaceName) -> SurfaceKind {
    built
        .faces
        .iter()
        .find(|f| &f.name == name)
        .map_or(SurfaceKind::Other, |f| f.surface)
}

/// Whether a face's axis is compared as a line only: a blend's, whose way along its axis
/// each kernel takes from the way it runs along its edges (a round's cylinder runs along its
/// edge, a torus or a cone the way the edge's circle or the cylinder it leaves does, which
/// may be another blend's); a corner's sphere by its centre only.
fn loose_axis(name: &FaceName, surface: SurfaceKind) -> bool {
    matches!(
        name.key,
        FaceKey::FromEdge { .. } | FaceKey::FromVertex { .. }
    ) && surface != SurfaceKind::Plane
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

fn cut(part: &mut Part, sketch: Sketch, extent: ExtrudeExtent) {
    extrude(part, sketch, ExtrudeOp::Cut, extent, false);
}

#[test]
fn a_round_hole_through_all() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 6.0);
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
    assert_same_part("round hole", &part);
}

#[test]
fn a_round_boss_on_a_face() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 6.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 5.0 },
        false,
    );
    assert_same_part("round boss on a face", &part);
}

#[test]
fn a_blind_round_pocket() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 6.0);
    cut(&mut part, sketch, ExtrudeExtent::Blind { depth: 4.0 });
    assert_same_part("blind round pocket", &part);
}

#[test]
fn half_holes_at_the_edges() {
    // At +x the half kept leaves out angle zero; at x = 0 it holds it.
    for x in [40.0, 0.0] {
        let mut part = Part::new();
        let base = base_block(&mut part);
        let mut sketch = on_top(base);
        sketch.circle(Point2::new(x, 15.0), 6.0);
        cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
        assert_same_part(&format!("half hole at x = {x}"), &part);
    }
}

#[test]
fn a_hole_close_to_a_side() {
    // Half a millimetre of wall between the hole's angle zero, where its seam is, and the
    // side. (Touching the side, the hole would meet it along a line: OpenCASCADE then splits
    // the side there and gives the seam to three faces, which the native kernel does not.)
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(33.5, 15.0), 6.0);
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
    assert_same_part("hole close to a side", &part);
}

#[test]
fn a_counterbore() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 4.0);
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 7.0);
    cut(&mut part, sketch, ExtrudeExtent::Blind { depth: 3.0 });
    assert_same_part("counterbore", &part);
}

#[test]
fn a_round_boss_overhanging_an_edge() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(38.0, 15.0), 6.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 5.0 },
        false,
    );
    assert_same_part("round boss overhanging", &part);
}

#[test]
fn a_slot_cut_through() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.line(Point2::new(10.0, 10.0), Point2::new(25.0, 10.0));
    arc(&mut sketch, (25.0, 15.0), (25.0, 10.0), (25.0, 20.0));
    sketch.line(Point2::new(25.0, 20.0), Point2::new(10.0, 20.0));
    arc(&mut sketch, (10.0, 15.0), (10.0, 20.0), (10.0, 10.0));
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
    assert_same_part("slot cut through", &part);
}

#[test]
fn a_hole_across_from_the_side() {
    let mut part = Part::new();
    base_block(&mut part);
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    sketch.circle(Point2::new(-5.0, 15.0), 3.0);
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
        false,
    );
    assert_same_part("hole across from the side", &part);
}

#[test]
fn a_round_boss_cut_in_two_by_a_pocket() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 8.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 10.0 },
        false,
    );
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    sketch.rectangle(Point2::new(15.0, -40.0), 4.0, 50.0);
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
        false,
    );
    assert_same_part("round boss cut in two", &part);
}

#[test]
fn a_hole_from_the_front_plane_reversed() {
    let mut part = Part::new();
    base_block(&mut part);
    let mut sketch = front();
    sketch.circle(Point2::new(20.0, 15.0), 6.0);
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: false },
        true,
    );
    assert_same_part("hole from the front plane", &part);
}

#[test]
fn round_bosses_stacked_on_one_axis() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 8.0);
    let lower = boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 4.0 },
        false,
    );
    let mut sketch = Sketch::new(SketchPlane::Face(FaceName::end_cap(lower)));
    sketch.circle(Point2::new(20.0, 15.0), 5.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 3.0 },
        false,
    );
    assert_same_part("round bosses stacked", &part);
}

#[test]
fn a_hole_through_a_round_boss() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 8.0);
    let round = boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 6.0 },
        false,
    );
    let mut sketch = Sketch::new(SketchPlane::Face(FaceName::end_cap(round)));
    sketch.circle(Point2::new(20.0, 15.0), 3.0);
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
    assert_same_part("hole through a round boss", &part);
}

#[test]
fn an_intersection_with_a_cylinder() {
    let mut part = Part::new();
    base_block(&mut part);
    let mut sketch = front();
    sketch.circle(Point2::new(30.0, 15.0), 12.0);
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Intersect,
        ExtrudeExtent::Blind { depth: 6.0 },
        true,
    );
    assert_same_part("intersection with a cylinder", &part);
}

#[test]
fn a_pattern_of_round_holes() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(6.0, 15.0), 3.0);
    let hole = extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: false },
        false,
    );
    part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![hole],
        first: PatternDirection {
            axis: AxisRef::Standard(StandardAxis::X),
            spacing: 9.0,
            count: 4,
            reverse: false,
        },
        second: None,
        skip: Vec::new(),
    }));
    assert_same_part("pattern of round holes", &part);
}

#[test]
fn a_rounded_rectangle_boss_on_a_face() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    // 20 × 12 with corners rounded to 3.
    let (x0, y0, x1, y1, r) = (10.0, 9.0, 30.0, 21.0, 3.0);
    sketch.line(Point2::new(x0 + r, y0), Point2::new(x1 - r, y0));
    arc(&mut sketch, (x1 - r, y0 + r), (x1 - r, y0), (x1, y0 + r));
    sketch.line(Point2::new(x1, y0 + r), Point2::new(x1, y1 - r));
    arc(&mut sketch, (x1 - r, y1 - r), (x1, y1 - r), (x1 - r, y1));
    sketch.line(Point2::new(x1 - r, y1), Point2::new(x0 + r, y1));
    arc(&mut sketch, (x0 + r, y1 - r), (x0 + r, y1), (x0, y1 - r));
    sketch.line(Point2::new(x0, y1 - r), Point2::new(x0, y0 + r));
    arc(&mut sketch, (x0 + r, y0 + r), (x0, y0 + r), (x0 + r, y0));
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 4.0 },
        false,
    );
    assert_same_part("rounded rectangle boss", &part);
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
    assert_same_measures("box", &measures(&native, n), &measures(&occt, o), EXACT);

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
            EXACT,
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

/// The volume a display mesh holds: each triangle's tetrahedron to the origin, signed.
fn mesh_volume(mesh: &oxyde_kernel::Mesh) -> f64 {
    let mut held = 0.0;
    for face in &mesh.faces {
        for t in &face.triangles {
            let [a, b, c] = t.map(|k| {
                let [x, y, z] = face.positions[k as usize];
                Vec3::new(f64::from(x), f64::from(y), f64::from(z))
            });
            held += a.dot(b.cross(c)) / 6.0;
        }
    }
    held
}

#[test]
fn display_meshes_of_a_plate_with_holes_at_its_corners_hold_its_volume() {
    // Holes reaching as far along a cap as each other: the bridge from one to the cap's
    // boundary is met by the next one's, which must join it on its inner side, or the cap's
    // triangles overlap (a FreeCAD part's top face, holed so, covered 1.8 times its area).
    let mut part = Part::new();
    let mut sketch = front();
    sketch.rectangle(Point2::new(0.0, 0.0), 40.0, 30.0);
    for (x, y) in [(5.0, 5.0), (35.0, 5.0), (5.0, 25.0), (35.0, 25.0)] {
        sketch.circle(Point2::new(x, y), 2.5);
    }
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 5.0 },
        false,
    );
    let mut kernel = NativeKernel::new();
    part.rebuild(&mut kernel);
    let shape = part.body().expect("a body").solids[0].shape;
    // At a thousandth of its size, as the viewport meshes it.
    let (lo, hi) = kernel.bounds(shape).unwrap();
    let held = mesh_volume(&kernel.tessellate(shape, lo.distance(hi) * 1e-3).unwrap());
    let volume = kernel.volume(shape).unwrap();
    // Chords 0.05 off a hole of radius 2.5 leave out 2.6% of it, 1.8e-3 of the volume:
    // overlapping triangles held a third more.
    assert!(
        (held - volume).abs() <= 3e-3 * volume,
        "the mesh holds {held} mm³, the solid {volume} mm³"
    );
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

/// A linear pattern of `seed` along x (`count` × `spacing`) and, given, along y.
fn pattern(part: &mut Part, seed: FeatureId, x: (u32, f64), y: Option<(u32, f64)>) {
    let along = |axis, (count, spacing): (u32, f64)| PatternDirection {
        axis: AxisRef::Standard(axis),
        spacing,
        count,
        reverse: false,
    };
    part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![seed],
        first: along(StandardAxis::X, x),
        second: y.map(|y| along(StandardAxis::Y, y)),
        skip: Vec::new(),
    }));
}

#[test]
fn staggered_patterns_of_hexagonal_holes() {
    // Two grids of hexagonal holes through the block, the second between the first's, as
    // the hackintosh case's panels are: each pattern one boolean with all its holes as tools.
    let mut part = Part::new();
    let base = base_block(&mut part);
    for (ox, oy) in [(7.0, 6.0), (11.0, 9.5)] {
        let mut sketch = on_top(base);
        let corners: Vec<(f64, f64)> = (0..6)
            .map(|k| {
                let a = std::f64::consts::FRAC_PI_6 + f64::from(k) * std::f64::consts::FRAC_PI_3;
                (ox + 2.5 * a.cos(), oy + 2.5 * a.sin())
            })
            .collect();
        polygon(&mut sketch, &corners);
        let hole = extrude(
            &mut part,
            sketch,
            ExtrudeOp::Cut,
            ExtrudeExtent::ThroughAll { both: false },
            false,
        );
        pattern(&mut part, hole, (4, 8.0), Some((3, 7.0)));
    }
    assert_same_part("staggered hexagonal holes", &part);
}

#[test]
fn a_pattern_of_holes_running_into_each_other() {
    // Holes closer than their diameter: tools that overlap are joined before the cut.
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(8.0, 15.0), 3.0);
    let hole = extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: false },
        false,
    );
    pattern(&mut part, hole, (5, 4.5), None);
    assert_same_part("pattern of overlapping holes", &part);
}

#[test]
fn a_pattern_of_round_bosses() {
    // A fuse with many tools apart from each other.
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(6.0, 6.0), 2.0);
    let peg = boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 3.0 },
        false,
    );
    pattern(&mut part, peg, (5, 7.0), Some((3, 9.0)));
    assert_same_part("pattern of round bosses", &part);
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
    let base = base_block(&mut part);
    // A hole down through the block, which the native kernel cuts.
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 4.0);
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
    // A smooth loft through ten squares above it, which it cannot make yet (OpenCASCADE
    // approximates rather than passes through so many).
    let mut profiles = Vec::new();
    for k in 0..10 {
        let mut square = section_sketch(&mut part, 12.0 + 5.0 * f64::from(k));
        let side = 12.0 + f64::from(k % 3);
        square.rectangle(
            Point2::new(20.0 - side / 2.0, 15.0 - side / 2.0),
            side,
            side,
        );
        profiles.push(part.add_feature(FeatureKind::Sketch(square)));
    }
    let loft = part.add_feature(FeatureKind::Loft(oxyde_model::Loft {
        profiles,
        op: ExtrudeOp::Boss,
        ruled: false,
        bodies: Vec::new(),
    }));

    let mut kernel = NativeKernel::new();
    part.rebuild(&mut kernel);
    let statuses: Vec<FeatureStatus> = part.timeline().into_iter().map(|e| e.status).collect();
    assert_eq!(statuses[1], FeatureStatus::Ok);
    assert_eq!(statuses[3], FeatureStatus::Ok);
    match part.status(loft) {
        Some(FeatureStatus::Error(message)) => {
            assert!(message.contains("loft"), "{message}")
        }
        other => panic!("the loft should fail, not {other:?}"),
    }
    // The block with its hole is still there, whole.
    let body = part.body().expect("a body");
    let volume: f64 = body
        .solids
        .iter()
        .map(|s| kernel.volume(s.shape).unwrap())
        .sum();
    let expected = 12_000.0 - std::f64::consts::PI * 16.0 * 10.0;
    assert!((volume - expected).abs() < 1e-6, "{volume}");
    part.release(&mut kernel);
}

// ---------------------------------------------------------------------------------------
// Revolves: planes, cylinders, cones, spheres and tori, and the booleans of them that meet
// in circles across their axis.

/// A sketch on the Front plane with a construction line along the Y axis, drawn from
/// `from` to `to` (its direction is the revolve's axis), and the line's id.
fn about_y(from: f64, to: f64) -> (Sketch, u32) {
    let mut sketch = front();
    let axis = sketch
        .geometry
        .line(Point2::new(0.0, from), Point2::new(0.0, to));
    sketch.geometry.set_construction(axis, true);
    (sketch, axis.0)
}

fn revolve(part: &mut Part, sketch: Sketch, axis: u32, op: ExtrudeOp, angle: f64) -> FeatureId {
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Revolve(oxyde_model::Revolve {
        sketch: s,
        op,
        axis: oxyde_model::RevolveAxis::SketchLine(axis),
        angle,
        mid_plane: false,
        reverse: false,
        bodies: Vec::new(),
    }))
}

/// A part of one revolve of `corners` (a polygon) about the Y axis, drawn up or down.
fn revolved_polygon(corners: &[(f64, f64)], up: bool, angle: f64) -> Part {
    let mut part = Part::new();
    let (mut sketch, axis) = if up {
        about_y(-20.0, 40.0)
    } else {
        about_y(40.0, -20.0)
    };
    polygon(&mut sketch, corners);
    revolve(&mut part, sketch, axis, ExtrudeOp::Boss, angle);
    part
}

const QUARTER: f64 = std::f64::consts::FRAC_PI_2;
const WHOLE: f64 = std::f64::consts::TAU;

#[test]
fn revolved_rings_rods_and_cones() {
    let ring = [(10.0, 0.0), (20.0, 0.0), (20.0, 5.0), (10.0, 5.0)];
    let rod = [(0.0, 0.0), (5.0, 0.0), (5.0, 10.0), (0.0, 10.0)];
    let cone = [(0.0, 0.0), (5.0, 0.0), (0.0, 10.0)];
    let apex_first = [(0.0, 0.0), (0.0, 10.0), (5.0, 0.0)];
    let frustum = [(2.0, 0.0), (6.0, 0.0), (3.0, 10.0), (2.0, 10.0)];
    let left = [(-10.0, 0.0), (-20.0, 0.0), (-20.0, 5.0), (-10.0, 5.0)];
    for (name, corners) in [
        ("ring", &ring[..]),
        ("rod", &rod[..]),
        ("cone", &cone[..]),
        ("cone drawn from its apex", &apex_first[..]),
        ("frustum off the axis", &frustum[..]),
        ("ring left of the axis", &left[..]),
    ] {
        for up in [true, false] {
            for angle in [WHOLE, QUARTER, 2.0] {
                let what = format!(
                    "{name}, axis {}, {angle} rad",
                    if up { "up" } else { "down" }
                );
                assert_same_part(&what, &revolved_polygon(corners, up, angle));
            }
        }
    }
}

/// A part of one revolve about the Y axis of a sketch `draw` makes.
fn revolved_with(draw: impl Fn(&mut Sketch), up: bool, angle: f64) -> Part {
    let mut part = Part::new();
    let (mut sketch, axis) = if up {
        about_y(-20.0, 40.0)
    } else {
        about_y(40.0, -20.0)
    };
    draw(&mut sketch);
    revolve(&mut part, sketch, axis, ExtrudeOp::Boss, angle);
    part
}

#[test]
fn revolved_balls_caps_and_tori() {
    // A half disc on the axis: a ball. Its arc counter-clockwise from the south pole.
    let ball = |s: &mut Sketch| {
        let g = &mut s.geometry;
        let (south, north, centre) = (
            g.point(Point2::new(0.0, -5.0)),
            g.point(Point2::new(0.0, 5.0)),
            g.point(Point2::new(0.0, 0.0)),
        );
        g.arc_between(centre, south, north);
        g.line_between(north, south);
    };
    // A ball cut flat at both ends: a sphere's zone and two discs.
    let cap = |s: &mut Sketch| {
        let g = &mut s.geometry;
        let (top, bottom, centre) = (
            g.point(Point2::new(3.0, 4.0)),
            g.point(Point2::new(3.0, -4.0)),
            g.point(Point2::new(0.0, 0.0)),
        );
        let (axis_bottom, axis_top) = (
            g.point(Point2::new(0.0, -4.0)),
            g.point(Point2::new(0.0, 4.0)),
        );
        g.arc_between(centre, bottom, top);
        g.line_between(top, axis_top);
        g.line_between(axis_top, axis_bottom);
        g.line_between(axis_bottom, bottom);
    };
    // A circle off the axis: a torus.
    let torus = |s: &mut Sketch| {
        s.circle(Point2::new(10.0, 2.0), 3.0);
    };
    // Half a circle off the axis and its diameter: half a torus's tube and a cylinder.
    let half_torus = |s: &mut Sketch| {
        arc(s, (10.0, 0.0), (10.0, -3.0), (10.0, 3.0));
        s.line(Point2::new(10.0, 3.0), Point2::new(10.0, -3.0));
    };
    let inner_half_torus = |s: &mut Sketch| {
        arc(s, (10.0, 0.0), (10.0, 3.0), (10.0, -3.0));
        s.line(Point2::new(10.0, -3.0), Point2::new(10.0, 3.0));
    };
    for up in [true, false] {
        for angle in [WHOLE, QUARTER] {
            let axis = if up { "up" } else { "down" };
            assert_same_part(
                &format!("ball, axis {axis}, {angle}"),
                &revolved_with(ball, up, angle),
            );
            assert_same_part(
                &format!("cap, axis {axis}, {angle}"),
                &revolved_with(cap, up, angle),
            );
            assert_same_part(
                &format!("torus, axis {axis}, {angle}"),
                &revolved_with(torus, up, angle),
            );
            assert_same_part(
                &format!("half torus, axis {axis}, {angle}"),
                &revolved_with(half_torus, up, angle),
            );
            assert_same_part(
                &format!("inner half torus, axis {axis}, {angle}"),
                &revolved_with(inner_half_torus, up, angle),
            );
        }
    }
}

#[test]
fn a_revolved_groove_and_a_revolved_cut_through_a_block() {
    // A rod with a groove turned into it.
    let mut part = revolved_polygon(
        &[(0.0, 0.0), (20.0, 0.0), (20.0, 30.0), (0.0, 30.0)],
        true,
        WHOLE,
    );
    let (mut groove, axis) = about_y(-20.0, 40.0);
    polygon(
        &mut groove,
        &[(17.0, 12.0), (25.0, 12.0), (25.0, 18.0), (17.0, 18.0)],
    );
    revolve(&mut part, groove, axis, ExtrudeOp::Cut, WHOLE);
    assert_same_part("groove", &part);
    // A ring turned about the block's middle line, cutting a round channel through it.
    let mut part = Part::new();
    let mut base = front();
    base.rectangle(Point2::new(-20.0, -15.0), 40.0, 30.0);
    boss(&mut part, base, ExtrudeExtent::Blind { depth: 10.0 }, false);
    let (mut ring, axis) = about_y(-20.0, 40.0);
    polygon(
        &mut ring,
        &[(8.0, 2.0), (12.0, 2.0), (12.0, 6.0), (8.0, 6.0)],
    );
    revolve(&mut part, ring, axis, ExtrudeOp::Cut, WHOLE);
    assert_same_part("channel", &part);
}

/// A 40 × 20 × 10 block with a sketch on its top holding two points, and a hole of `kind`
/// at each.
fn holes(kind: oxyde_model::HoleKind, depth: Option<f64>) -> Part {
    let mut part = Part::new();
    let mut sketch = front();
    sketch.rectangle(Point2::new(0.0, 0.0), 40.0, 20.0);
    let base = boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 10.0 },
        false,
    );
    let mut spots = on_top(base);
    spots.geometry.point(Point2::new(10.0, 10.0));
    spots.geometry.point(Point2::new(30.0, 10.0));
    let spots = part.add_feature(FeatureKind::Sketch(spots));
    part.add_feature(FeatureKind::Hole(oxyde_model::Hole {
        sketch: spots,
        kind,
        diameter: 6.0,
        depth,
        reverse: false,
        bodies: Vec::new(),
    }));
    part
}

#[test]
fn holes_of_every_kind() {
    use oxyde_model::HoleKind;
    assert_same_part("simple holes", &holes(HoleKind::Simple, None));
    assert_same_part("blind holes", &holes(HoleKind::Simple, Some(6.0)));
    let counterbore = HoleKind::Counterbore {
        diameter: 10.0,
        depth: 3.0,
    };
    assert_same_part("counterbored holes", &holes(counterbore, None));
    assert_same_part("blind counterbored holes", &holes(counterbore, Some(7.0)));
    let countersink = HoleKind::Countersink {
        diameter: 10.0,
        angle: QUARTER,
    };
    assert_same_part("countersunk holes", &holes(countersink, None));
    assert_same_part("blind countersunk holes", &holes(countersink, Some(7.0)));
}

#[test]
fn the_demo_flange() {
    let mut part = Part::new();
    let (mut section, axis) = about_y(-5.0, 30.0);
    let corners = [
        (10.0, 0.0),
        (40.0, 0.0),
        (40.0, 6.0),
        (16.0, 6.0),
        (16.0, 25.0),
        (10.0, 25.0),
    ]
    .map(|(x, y)| section.geometry.point(Point2::new(x, y)));
    let mut lines = Vec::new();
    for i in 0..corners.len() {
        lines.push(
            section
                .geometry
                .line_between(corners[i], corners[(i + 1) % corners.len()]),
        );
    }
    let body = revolve(&mut part, section, axis, ExtrudeOp::Boss, WHOLE);
    let mut hole = Sketch::new(SketchPlane::Face(FaceName::profile(body, lines[2].0)));
    hole.circle(Point2::new(28.0, 0.0), 3.0);
    let s = part.add_feature(FeatureKind::Sketch(hole));
    let hole = part.add_feature(FeatureKind::Extrude(Extrude::blind(s, ExtrudeOp::Cut, 6.0)));
    part.add_feature(FeatureKind::CircularPattern(oxyde_model::CircularPattern {
        seeds: vec![hole],
        axis: AxisRef::Standard(StandardAxis::Y),
        angle: WHOLE,
        count: 6,
        equal_spacing: true,
        reverse: false,
        skip: Vec::new(),
    }));
    assert_same_part("flange", &part);
}

#[test]
fn a_ball_on_a_block_and_a_ball_cut_from_it() {
    // A ball whose centre is on the block's top: half of it stands out, half is in.
    for op in [ExtrudeOp::Boss, ExtrudeOp::Cut] {
        let mut part = Part::new();
        let mut base = front();
        base.rectangle(Point2::new(-20.0, -15.0), 40.0, 30.0);
        boss(&mut part, base, ExtrudeExtent::Blind { depth: 10.0 }, false);
        // About the Z axis this time, on the Right plane: the ball's centre at z = 10.
        let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
        // The Right plane's x runs along -Z and its y along Y.
        let axis = sketch
            .geometry
            .line(Point2::new(20.0, 0.0), Point2::new(-30.0, 0.0));
        sketch.geometry.set_construction(axis, true);
        let g = &mut sketch.geometry;
        let (a, b, centre) = (
            g.point(Point2::new(-4.0, 0.0)),
            g.point(Point2::new(-16.0, 0.0)),
            g.point(Point2::new(-10.0, 0.0)),
        );
        g.arc_between(centre, a, b);
        g.line_between(b, a);
        revolve(&mut part, sketch, axis.0, op, WHOLE);
        assert_same_part(&format!("ball {op:?}"), &part);
    }
}

#[test]
fn a_pocket_inside_the_block_is_a_void() {
    // A round pocket cut blind from a plane in the middle of the block, inside it: a void.
    let mut part = Part::new();
    base_block(&mut part);
    let mid = part.add_feature(FeatureKind::Plane(oxyde_model::RefPlane::Offset {
        base: oxyde_model::PlaneRef::Standard(StandardPlane::Front),
        offset: 3.0,
        flip: false,
    }));
    let mut sketch = Sketch::new(SketchPlane::Reference(mid));
    sketch.circle(Point2::new(20.0, 15.0), 5.0);
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::Blind { depth: 4.0 },
        true,
    );
    assert_same_part("void", &part);
}

/// A revolve about the Y axis of the half disc of radius `r` centred at height `y`.
fn ball(part: &mut Part, y: f64, r: f64, op: ExtrudeOp) {
    let (mut sketch, axis) = about_y(-40.0, 60.0);
    let g = &mut sketch.geometry;
    let (south, north, centre) = (
        g.point(Point2::new(0.0, y - r)),
        g.point(Point2::new(0.0, y + r)),
        g.point(Point2::new(0.0, y)),
    );
    g.arc_between(centre, south, north);
    g.line_between(north, south);
    revolve(part, sketch, axis, op, WHOLE);
}

#[test]
fn revolved_solids_cut_in_half_along_their_axis() {
    // The boxes cover x > 0 (and more), from z = -20 to 20: their face x = 0 holds the axis.
    let half_box = |part: &mut Part| {
        let mut sketch = front();
        sketch.rectangle(Point2::new(0.0, -30.0), 30.0, 60.0);
        extrude(
            part,
            sketch,
            ExtrudeOp::Cut,
            ExtrudeExtent::MidPlane { depth: 40.0 },
            false,
        );
    };
    // A ball: a great circle through its poles.
    let mut part = Part::new();
    ball(&mut part, 0.0, 5.0, ExtrudeOp::Boss);
    half_box(&mut part);
    assert_same_part("half ball", &part);
    // A cone: two straight lines from its apex.
    let mut part = revolved_polygon(&[(0.0, 0.0), (5.0, 0.0), (0.0, 10.0)], true, WHOLE);
    half_box(&mut part);
    assert_same_part("half cone", &part);
    // A torus: two of its meridian circles.
    let mut part = Part::new();
    let (mut sketch, axis) = about_y(-20.0, 40.0);
    sketch.circle(Point2::new(10.0, 2.0), 3.0);
    revolve(&mut part, sketch, axis, ExtrudeOp::Boss, WHOLE);
    half_box(&mut part);
    assert_same_part("half torus", &part);
}

#[test]
fn balls_tori_and_rods_on_one_axis() {
    // A rod with a ball at its end, the ball's centre on the rod's end: the cylinder meets
    // the sphere in a circle, and the ball's lower half is inside the rod's end.
    let mut part = revolved_polygon(
        &[(0.0, 0.0), (3.0, 0.0), (3.0, 20.0), (0.0, 20.0)],
        true,
        WHOLE,
    );
    ball(&mut part, 20.0, 5.0, ExtrudeOp::Boss);
    assert_same_part("rod and ball", &part);
    // A thinner ball, the rod's end standing out of it.
    let mut part = revolved_polygon(
        &[(0.0, 0.0), (3.0, 0.0), (3.0, 20.0), (0.0, 20.0)],
        true,
        WHOLE,
    );
    ball(&mut part, 18.0, 4.0, ExtrudeOp::Cut);
    assert_same_part("rod with a ball cut from its end", &part);
    // A torus half sunk into a block, its axis square to the block's top.
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    // The Top plane's sketch y runs along -Z.
    sketch.rectangle(Point2::new(-20.0, -20.0), 40.0, 40.0);
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Boss,
        ExtrudeExtent::Blind { depth: 10.0 },
        true,
    );
    let (mut ring, axis) = about_y(-20.0, 40.0);
    ring.circle(Point2::new(10.0, 0.0), 3.0);
    revolve(&mut part, ring, axis, ExtrudeOp::Boss, WHOLE);
    assert_same_part("torus on a block", &part);
    // An O-ring's groove: a circle turned about a rod's axis, half into it.
    let mut part = revolved_polygon(
        &[(0.0, 0.0), (10.0, 0.0), (10.0, 30.0), (0.0, 30.0)],
        true,
        WHOLE,
    );
    let (mut ring, axis) = about_y(-20.0, 40.0);
    ring.circle(Point2::new(10.0, 15.0), 2.0);
    revolve(&mut part, ring, axis, ExtrudeOp::Cut, WHOLE);
    assert_same_part("o-ring groove", &part);
}

// ---------------------------------------------------------------------------------------
// Curves no chart makes straight or round: sections of cylinders and cones, and circles on
// spheres that are neither parallels nor meridians.

#[test]
fn holes_crossing_each_other() {
    // A hole down through the block, then one across it, through its axis and beside it;
    // one as wide through its axis meets it in two ellipses that cross each other, which
    // OpenCASCADE also cuts at the ends of their long axes, where the native kernel has
    // a corner only where a seam ends.
    // One beside it, only partly through it, meets it in a loop that turns back along
    // either one's lines: a traced curve.
    for (y, r) in [
        (15.0, 3.0),
        (15.5, 3.0),
        (15.0, 2.0),
        (15.0, 4.0),
        (20.0, 4.0),
    ] {
        let mut part = Part::new();
        let base = base_block(&mut part);
        let mut sketch = on_top(base);
        sketch.circle(Point2::new(20.0, 15.0), 4.0);
        cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
        let mut side = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
        side.circle(Point2::new(-5.0, y), r);
        extrude(
            &mut part,
            side,
            ExtrudeOp::Cut,
            ExtrudeExtent::ThroughAll { both: true },
            false,
        );
        let what = format!("holes crossing at y = {y}, radius {r}");
        if r == 4.0 {
            assert_same_faces(&what, &part);
        } else {
            assert_same_part(&what, &part);
        }
    }
}

#[test]
fn a_hole_through_a_round_bar() {
    let mut part = Part::new();
    let mut bar = front();
    bar.circle(Point2::new(0.0, 0.0), 10.0);
    boss(&mut part, bar, ExtrudeExtent::Blind { depth: 30.0 }, false);
    let mut hole = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    hole.circle(Point2::new(-15.0, 0.0), 3.0);
    extrude(
        &mut part,
        hole,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
        false,
    );
    assert_same_part("radial hole", &part);
}

/// A wedge cut through everything from the Right plane (its x along -Z, its y along Y),
/// above the line from (z0, y0) to (z1, y1).
fn slant_cut(part: &mut Part, (z0, y0): (f64, f64), (z1, y1): (f64, f64)) {
    let mut wedge = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    polygon(&mut wedge, &[(-z0, y0), (-z1, y1), (-z1 - 20.0, y1 + 40.0)]);
    extrude(
        part,
        wedge,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
        false,
    );
}

#[test]
fn round_bars_and_cones_cut_at_a_slant() {
    // A round bar along Z: an ellipse.
    let mut part = Part::new();
    let mut bar = front();
    bar.circle(Point2::new(0.0, 0.0), 5.0);
    boss(&mut part, bar, ExtrudeExtent::Blind { depth: 30.0 }, false);
    slant_cut(&mut part, (30.0, -6.0), (20.0, 6.0));
    assert_same_part("bar cut at a slant", &part);
    // A round boss on a block, cut at a slant: the ellipse ends on the block's top.
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 5.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 15.0 },
        false,
    );
    slant_cut(&mut part, (25.0, 0.0), (15.0, 30.0));
    assert_same_part("boss cut at a slant", &part);
    // A cone about Y, cut at a slant across its axis: an ellipse on the cone.
    let mut part = revolved_polygon(&[(0.0, 0.0), (8.0, 0.0), (0.0, 16.0)], true, WHOLE);
    let mut cutter = front();
    polygon(
        &mut cutter,
        &[(-20.0, 6.0), (20.0, 10.0), (20.0, 30.0), (-20.0, 30.0)],
    );
    extrude(
        &mut part,
        cutter,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
        false,
    );
    assert_same_part("cone cut at a slant", &part);
}

#[test]
fn balls_cut_off_their_axis() {
    // A ball about Y cut by a box whose face, x = 2, runs along the axis beside it: a circle
    // on the sphere that is neither a parallel nor a meridian.
    let mut part = Part::new();
    ball(&mut part, 0.0, 5.0, ExtrudeOp::Boss);
    let mut sketch = front();
    sketch.rectangle(Point2::new(2.0, -20.0), 20.0, 40.0);
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::MidPlane { depth: 40.0 },
        false,
    );
    assert_same_part("ball cut beside its axis", &part);
    // A hole through a ball, beside its centre: a cylinder's sections through a sphere.
    let mut part = Part::new();
    ball(&mut part, 0.0, 5.0, ExtrudeOp::Boss);
    let mut hole = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    hole.circle(Point2::new(-1.0, 1.0), 2.0);
    extrude(
        &mut part,
        hole,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
        false,
    );
    assert_same_part("hole through a ball", &part);
}

// ---------------------------------------------------------------------------------------
// Chamfers and fillets.

/// A fillet (or a chamfer) of `size` on `edges`.
fn blended_by(fillet: bool, edges: Vec<EdgeName>, size: f64) -> FeatureKind {
    let blend = EdgeBlend {
        edges,
        size,
        chamfer: ChamferShape::Equal,
    };
    if fillet {
        FeatureKind::Fillet(blend)
    } else {
        FeatureKind::Chamfer(blend)
    }
}

/// The 40 × 30 × 10 block of `base_block`, with its profile's four lines: along y = 0,
/// x = 40, y = 30 and x = 0.
fn lined_block(part: &mut Part) -> (FeatureId, [u32; 4]) {
    let mut sketch = front();
    let lines = sketch.rectangle(Point2::new(0.0, 0.0), 40.0, 30.0);
    let block = boss(part, sketch, ExtrudeExtent::Blind { depth: 10.0 }, false);
    (block, lines)
}

/// Which edges of `lined_block` (its feature and its profile's lines) to blend.
type EdgesOf = Box<dyn Fn(FeatureId, [u32; 4]) -> Vec<EdgeName>>;

#[test]
fn blends_of_a_block_s_edges_and_corners() {
    for fillet in [false, true] {
        let kind = if fillet { "fillet" } else { "chamfer" };
        let cases: Vec<(&str, EdgesOf)> = vec![
            (
                "one top edge",
                Box::new(|b, l| {
                    vec![EdgeName::between(
                        FaceName::profile(b, l[0]),
                        FaceName::end_cap(b),
                    )]
                }),
            ),
            (
                "two top edges meeting at a corner",
                Box::new(|b, l| {
                    vec![
                        EdgeName::between(FaceName::profile(b, l[0]), FaceName::end_cap(b)),
                        EdgeName::between(FaceName::profile(b, l[1]), FaceName::end_cap(b)),
                    ]
                }),
            ),
            (
                "a top edge and the upright edge at its end",
                Box::new(|b, l| {
                    vec![
                        EdgeName::between(FaceName::profile(b, l[0]), FaceName::end_cap(b)),
                        EdgeName::between(FaceName::profile(b, l[0]), FaceName::profile(b, l[1])),
                    ]
                }),
            ),
            (
                "the three edges of a corner",
                Box::new(|b, l| {
                    vec![
                        EdgeName::between(FaceName::profile(b, l[0]), FaceName::end_cap(b)),
                        EdgeName::between(FaceName::profile(b, l[1]), FaceName::end_cap(b)),
                        EdgeName::between(FaceName::profile(b, l[0]), FaceName::profile(b, l[1])),
                    ]
                }),
            ),
        ];
        for (what, edges) in &cases {
            let mut part = Part::new();
            let (b, l) = lined_block(&mut part);
            part.add_feature(blended_by(fillet, edges(b, l), 2.0));
            assert_same_part(&format!("{kind}: {what}"), &part);
        }
    }
}

#[test]
fn blends_of_round_edges() {
    for fillet in [false, true] {
        let kind = if fillet { "fillet" } else { "chamfer" };
        // A hole's rim.
        let mut part = Part::new();
        let base = base_block(&mut part);
        let mut sketch = on_top(base);
        let circle = sketch.circle(Point2::new(20.0, 15.0), 4.0);
        let hole = extrude(
            &mut part,
            sketch,
            ExtrudeOp::Cut,
            ExtrudeExtent::ThroughAll { both: false },
            false,
        );
        part.add_feature(blended_by(
            fillet,
            vec![EdgeName::between(
                FaceName::profile(hole, circle),
                FaceName::end_cap(base),
            )],
            1.0,
        ));
        assert_same_part(&format!("{kind}: a hole's rim"), &part);
        // A round boss's top and foot.
        let mut part = Part::new();
        let base = base_block(&mut part);
        let mut sketch = on_top(base);
        let circle = sketch.circle(Point2::new(20.0, 15.0), 5.0);
        let post = boss(
            &mut part,
            sketch,
            ExtrudeExtent::Blind { depth: 6.0 },
            false,
        );
        part.add_feature(blended_by(
            fillet,
            vec![
                EdgeName::between(FaceName::profile(post, circle), FaceName::end_cap(post)),
                EdgeName::between(FaceName::profile(post, circle), FaceName::end_cap(base)),
            ],
            1.0,
        ));
        assert_same_part(&format!("{kind}: a round boss's top and foot"), &part);
    }
}

#[test]
fn blends_along_a_slot_and_in_an_inner_corner() {
    for fillet in [false, true] {
        let kind = if fillet { "fillet" } else { "chamfer" };
        // One straight top edge of a slot: the blend runs on round its ends.
        let mut part = Part::new();
        let mut sketch = front();
        let bottom = sketch.line(Point2::new(0.0, 0.0), Point2::new(20.0, 0.0));
        arc(&mut sketch, (20.0, 5.0), (20.0, 0.0), (20.0, 10.0));
        sketch.line(Point2::new(20.0, 10.0), Point2::new(0.0, 10.0));
        arc(&mut sketch, (0.0, 5.0), (0.0, 10.0), (0.0, 0.0));
        let slot = boss(
            &mut part,
            sketch,
            ExtrudeExtent::Blind { depth: 4.0 },
            false,
        );
        part.add_feature(blended_by(
            fillet,
            vec![EdgeName::between(
                FaceName::profile(slot, bottom),
                FaceName::end_cap(slot),
            )],
            1.0,
        ));
        assert_same_part(&format!("{kind}: a slot's top edge"), &part);
        // The inner edge of an L: a valley, filled in.
        let mut part = Part::new();
        let mut sketch = front();
        sketch.rectangle(Point2::new(-40.0, -25.0), 80.0, 50.0);
        let base = boss(
            &mut part,
            sketch,
            ExtrudeExtent::Blind { depth: 10.0 },
            false,
        );
        let mut sketch = on_top(base);
        let upright = sketch.rectangle(Point2::new(-40.0, -25.0), 15.0, 50.0);
        let wall = boss(
            &mut part,
            sketch,
            ExtrudeExtent::Blind { depth: 30.0 },
            false,
        );
        part.add_feature(blended_by(
            fillet,
            vec![EdgeName::between(
                FaceName::profile(wall, upright[1]),
                FaceName::end_cap(base),
            )],
            4.0,
        ));
        assert_same_part(&format!("{kind}: the inner edge of an L"), &part);
    }
}

#[test]
fn unequal_chamfers() {
    for shape in [
        ChamferShape::TwoDistances {
            distance: 4.0,
            flip: false,
        },
        ChamferShape::TwoDistances {
            distance: 4.0,
            flip: true,
        },
        ChamferShape::AngleDistance {
            angle: 30f64.to_radians(),
            flip: false,
        },
    ] {
        let mut part = Part::new();
        let (b, l) = lined_block(&mut part);
        part.add_feature(FeatureKind::Chamfer(EdgeBlend {
            edges: vec![EdgeName::between(
                FaceName::profile(b, l[2]),
                FaceName::end_cap(b),
            )],
            size: 2.0,
            chamfer: shape,
        }));
        assert_same_part(&format!("unequal chamfer {shape:?}"), &part);
    }
}

#[test]
fn blends_that_cannot_be_made_say_why() {
    // A fillet larger than the block is high.
    let mut part = Part::new();
    let (b, l) = lined_block(&mut part);
    let fillet = part.add_feature(blended_by(
        true,
        vec![EdgeName::between(
            FaceName::profile(b, l[0]),
            FaceName::end_cap(b),
        )],
        15.0,
    ));
    let mut kernel = NativeKernel::new();
    part.rebuild(&mut kernel);
    match part.status(fillet) {
        Some(FeatureStatus::Error(message)) => {
            assert!(message.contains("too large"), "{message}")
        }
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
    // A slot's straight side meets its round end smoothly.
    let mut part = Part::new();
    let mut sketch = front();
    let bottom = sketch.line(Point2::new(0.0, 0.0), Point2::new(20.0, 0.0));
    let end = sketch.geometry.arc(
        Point2::new(20.0, 5.0),
        Point2::new(20.0, 0.0),
        Point2::new(20.0, 10.0),
    );
    sketch.line(Point2::new(20.0, 10.0), Point2::new(0.0, 10.0));
    arc(&mut sketch, (0.0, 5.0), (0.0, 10.0), (0.0, 0.0));
    let slot = boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 5.0 },
        false,
    );
    let fillet = part.add_feature(blended_by(
        true,
        vec![EdgeName::between(
            FaceName::profile(slot, bottom),
            FaceName::profile(slot, end.0),
        )],
        1.0,
    ));
    let mut kernel = NativeKernel::new();
    part.rebuild(&mut kernel);
    match part.status(fillet) {
        Some(FeatureStatus::Error(message)) => assert!(message.contains("smooth"), "{message}"),
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
}

#[test]
fn rounds_in_a_pocket_s_corners_then_round_its_rim() {
    // A pocket through a block, its four inner corners rounded (valleys, filled in), then
    // its rim round the top rounded along lines and arcs that run on smoothly.
    let mut part = Part::new();
    let mut sketch = front();
    sketch.rectangle(Point2::new(0.0, 0.0), 60.0, 40.0);
    let base = boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 10.0 },
        false,
    );
    let mut pocket = on_top(base);
    let walls = pocket.rectangle(Point2::new(15.0, 10.0), 30.0, 20.0);
    let hole = extrude(
        &mut part,
        pocket,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: false },
        false,
    );
    let corners: Vec<EdgeName> = (0..4)
        .map(|i| {
            EdgeName::between(
                FaceName::profile(hole, walls[i]),
                FaceName::profile(hole, walls[(i + 1) % 4]),
            )
        })
        .collect();
    part.add_feature(blended_by(true, corners, 3.0));
    assert_same_part("a pocket's corners rounded", &part);
    // The rim: every edge of the top that a later feature made.
    let mut kernel = OcctKernel::new();
    let mut built = Part::from_definition(part.definition()).unwrap();
    built.rebuild(&mut kernel);
    let top = FaceName::end_cap(base);
    let rim: Vec<EdgeName> = built
        .body()
        .unwrap()
        .naming
        .edges()
        .iter()
        .filter(|e| e.faces.contains(&top) && e.faces.iter().any(|f| f.key.feature() != base))
        .cloned()
        .collect();
    built.release(&mut kernel);
    assert_eq!(rim.len(), 8);
    for fillet in [true, false] {
        let mut part = Part::from_definition(part.definition()).unwrap();
        part.add_feature(blended_by(fillet, rim.clone(), 1.0));
        assert_same_part(&format!("a pocket's rim, fillet {fillet}"), &part);
    }
}

#[test]
fn every_edge_of_a_block_blended() {
    for fillet in [false, true] {
        let mut part = Part::new();
        let (b, l) = lined_block(&mut part);
        let mut edges = Vec::new();
        for i in 0..4 {
            let side = FaceName::profile(b, l[i]);
            edges.push(EdgeName::between(side.clone(), FaceName::end_cap(b)));
            edges.push(EdgeName::between(side.clone(), FaceName::start_cap(b)));
            edges.push(EdgeName::between(
                side,
                FaceName::profile(b, l[(i + 1) % 4]),
            ));
        }
        part.add_feature(blended_by(fillet, edges, 2.0));
        assert_same_part(&format!("every edge of a block, fillet {fillet}"), &part);
    }
}

#[test]
fn blends_of_a_boss_s_edges_down_to_the_plate_and_a_hole_s_far_rim() {
    for fillet in [false, true] {
        // The upright edges of a square boss: they end on the plate, in a valley.
        let mut part = Part::new();
        let base = base_block(&mut part);
        let mut sketch = on_top(base);
        let sides = sketch.rectangle(Point2::new(10.0, 10.0), 12.0, 8.0);
        let post = boss(
            &mut part,
            sketch,
            ExtrudeExtent::Blind { depth: 6.0 },
            false,
        );
        let uprights: Vec<EdgeName> = (0..4)
            .map(|i| {
                EdgeName::between(
                    FaceName::profile(post, sides[i]),
                    FaceName::profile(post, sides[(i + 1) % 4]),
                )
            })
            .collect();
        part.add_feature(blended_by(fillet, uprights, 2.0));
        assert_same_part(&format!("a boss's upright edges, fillet {fillet}"), &part);
        // A hole's rim where it comes out underneath.
        let mut part = Part::new();
        let base = base_block(&mut part);
        let mut sketch = on_top(base);
        let circle = sketch.circle(Point2::new(20.0, 15.0), 4.0);
        let hole = extrude(
            &mut part,
            sketch,
            ExtrudeOp::Cut,
            ExtrudeExtent::ThroughAll { both: false },
            false,
        );
        part.add_feature(blended_by(
            fillet,
            vec![EdgeName::between(
                FaceName::profile(hole, circle),
                FaceName::start_cap(base),
            )],
            1.5,
        ));
        assert_same_part(&format!("a hole's far rim, fillet {fillet}"), &part);
    }
}

#[test]
fn blends_that_end_on_slanted_faces() {
    for fillet in [false, true] {
        // A block whose ends lean: the top edge's blend runs into them at a slant.
        let mut part = Part::new();
        let mut sketch = front();
        let bottom = sketch.line(Point2::new(0.0, 0.0), Point2::new(40.0, 0.0));
        sketch.line(Point2::new(40.0, 0.0), Point2::new(30.0, 20.0));
        sketch.line(Point2::new(30.0, 20.0), Point2::new(8.0, 20.0));
        sketch.line(Point2::new(8.0, 20.0), Point2::new(0.0, 0.0));
        let block = boss(
            &mut part,
            sketch,
            ExtrudeExtent::Blind { depth: 10.0 },
            false,
        );
        part.add_feature(blended_by(
            fillet,
            vec![EdgeName::between(
                FaceName::profile(block, bottom),
                FaceName::end_cap(block),
            )],
            2.0,
        ));
        assert_same_part(
            &format!("a blend into slanted ends, fillet {fillet}"),
            &part,
        );
    }
}

#[test]
fn blends_of_a_turned_shaft_and_the_demo_flange() {
    for fillet in [false, true] {
        // A shaft turned from a bar: a collar, a cone down to a neck, the neck's end. Its
        // round edges are between planes, cylinders and the cone, all on one axis.
        let mut part = Part::new();
        let (mut section, axis) = about_y(-5.0, 45.0);
        let corners = [
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 20.0),
            (6.0, 24.0),
            (6.0, 40.0),
            (0.0, 40.0),
        ]
        .map(|(x, y)| section.geometry.point(Point2::new(x, y)));
        let mut lines = Vec::new();
        for i in 0..corners.len() {
            lines.push(
                section
                    .geometry
                    .line_between(corners[i], corners[(i + 1) % corners.len()]),
            );
        }
        let shaft = revolve(&mut part, section, axis, ExtrudeOp::Boss, WHOLE);
        let side = |k: usize| FaceName::profile(shaft, lines[k].0);
        part.add_feature(blended_by(
            fillet,
            vec![
                // The collar's foot, round its bottom.
                EdgeName::between(side(0), side(1)),
                // Where the collar meets the cone: a ridge.
                EdgeName::between(side(1), side(2)),
                // Where the cone meets the neck: a valley.
                EdgeName::between(side(2), side(3)),
                // The neck's end.
                EdgeName::between(side(3), side(4)),
            ],
            1.0,
        ));
        assert_same_part(&format!("a turned shaft's edges, fillet {fillet}"), &part);
        // The demo flange's rim and the foot of its hub.
        let mut part = Part::new();
        let (mut section, axis) = about_y(-5.0, 30.0);
        let corners = [
            (10.0, 0.0),
            (40.0, 0.0),
            (40.0, 6.0),
            (16.0, 6.0),
            (16.0, 25.0),
            (10.0, 25.0),
        ]
        .map(|(x, y)| section.geometry.point(Point2::new(x, y)));
        let mut lines = Vec::new();
        for i in 0..corners.len() {
            lines.push(
                section
                    .geometry
                    .line_between(corners[i], corners[(i + 1) % corners.len()]),
            );
        }
        let body = revolve(&mut part, section, axis, ExtrudeOp::Boss, WHOLE);
        let side = |k: usize| FaceName::profile(body, lines[k].0);
        part.add_feature(blended_by(
            fillet,
            vec![
                EdgeName::between(side(1), side(2)),
                EdgeName::between(side(2), side(3)),
            ],
            2.0,
        ));
        assert_same_part(&format!("the flange's rim and hub, fillet {fillet}"), &part);
    }
}

// ---------------------------------------------------------------------------------------
// Shells.

fn shelled(faces: Vec<FaceName>, thickness: f64, outward: bool) -> FeatureKind {
    FeatureKind::Shell(oxyde_model::Shell {
        faces,
        thickness,
        outward,
    })
}

#[test]
fn shells_of_a_block_and_a_round_bar() {
    for outward in [false, true] {
        // A block opened at the top, and one hollowed whole.
        for open in [true, false] {
            let mut part = Part::new();
            let base = base_block(&mut part);
            let faces = if open {
                vec![FaceName::end_cap(base)]
            } else {
                Vec::new()
            };
            part.add_feature(shelled(faces, 2.0, outward));
            assert_same_part(
                &format!("a block's shell, open {open}, outward {outward}"),
                &part,
            );
        }
        // A round bar, opened at its end and at both ends.
        for ends in [1, 2] {
            let mut part = Part::new();
            let mut sketch = front();
            sketch.circle(Point2::new(0.0, 0.0), 10.0);
            let bar = boss(
                &mut part,
                sketch,
                ExtrudeExtent::Blind { depth: 20.0 },
                false,
            );
            let mut faces = vec![FaceName::end_cap(bar)];
            if ends == 2 {
                faces.push(FaceName::start_cap(bar));
            }
            part.add_feature(shelled(faces, 1.0, outward));
            assert_same_part(
                &format!("a round bar's shell, {ends} ends open, outward {outward}"),
                &part,
            );
        }
    }
}

#[test]
fn shells_of_holed_rounded_and_turned_parts() {
    for outward in [false, true] {
        // A block with a hole down through it, opened at the top: the hole's wall grows.
        // Off the block's middle, so that its rim's centroid is not the outer rim's, which
        // tells the two apart.
        let mut part = Part::new();
        let base = base_block(&mut part);
        let mut sketch = on_top(base);
        sketch.circle(Point2::new(12.0, 15.0), 4.0);
        cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
        part.add_feature(shelled(vec![FaceName::end_cap(base)], 1.5, outward));
        assert_same_part(&format!("a holed block's shell, outward {outward}"), &part);
        // A block with its upright edges rounded, opened at the top.
        let mut part = Part::new();
        let (b, l) = lined_block(&mut part);
        let edges = (0..4)
            .map(|i| {
                EdgeName::between(
                    FaceName::profile(b, l[i]),
                    FaceName::profile(b, l[(i + 1) % 4]),
                )
            })
            .collect();
        part.add_feature(blended_by(true, edges, 4.0));
        part.add_feature(shelled(vec![FaceName::end_cap(b)], 1.0, outward));
        assert_same_part(
            &format!("a rounded block's shell, outward {outward}"),
            &part,
        );
        // A cup turned from a profile with a cone, opened at its rim.
        let mut part = Part::new();
        let (mut section, axis) = about_y(-5.0, 45.0);
        let corners = [
            (0.0, 0.0),
            (12.0, 0.0),
            (12.0, 10.0),
            (16.0, 25.0),
            (0.0, 25.0),
        ]
        .map(|(x, y)| section.geometry.point(Point2::new(x, y)));
        let mut lines = Vec::new();
        for i in 0..corners.len() {
            lines.push(
                section
                    .geometry
                    .line_between(corners[i], corners[(i + 1) % corners.len()]),
            );
        }
        let cup = revolve(&mut part, section, axis, ExtrudeOp::Boss, WHOLE);
        part.add_feature(shelled(
            vec![FaceName::profile(cup, lines[3].0)],
            1.0,
            outward,
        ));
        assert_same_part(&format!("a turned cup's shell, outward {outward}"), &part);
    }
}

#[test]
fn a_shell_thicker_than_the_part_fails() {
    // 12 mm walls in a bar of 10 mm radius.
    let mut part = Part::new();
    let mut sketch = front();
    sketch.circle(Point2::new(0.0, 0.0), 10.0);
    let bar = boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 20.0 },
        false,
    );
    let shell = part.add_feature(shelled(vec![FaceName::end_cap(bar)], 12.0, false));
    let mut kernel = NativeKernel::new();
    part.rebuild(&mut kernel);
    match part.status(shell) {
        Some(FeatureStatus::Error(message)) => {
            assert!(message.contains("too thick"), "{message}")
        }
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
}

// ---------------------------------------------------------------------------------------
// Sweeps.

fn top() -> Sketch {
    Sketch::new(SketchPlane::Standard(StandardPlane::Top))
}

fn swept(part: &mut Part, profile: Sketch, path: Sketch, op: ExtrudeOp) -> FeatureId {
    let profile = part.add_feature(FeatureKind::Sketch(profile));
    let path = part.add_feature(FeatureKind::Sketch(path));
    part.add_feature(FeatureKind::Sweep(oxyde_model::Sweep {
        profile,
        path,
        op,
        bodies: Vec::new(),
    }))
}

#[test]
fn sweeps_along_lines_arcs_and_circles() {
    // The Top plane's sketch y runs along -Z.
    let circle_at = |x: f64, r: f64| {
        let mut s = front();
        s.circle(Point2::new(x, 0.0), r);
        s
    };
    // Along a line.
    let mut part = Part::new();
    let mut path = top();
    path.geometry
        .line(Point2::new(0.0, 0.0), Point2::new(0.0, -30.0));
    swept(&mut part, circle_at(0.0, 5.0), path, ExtrudeOp::Boss);
    assert_same_part("a circle swept along a line", &part);
    // Along a quarter turn.
    let mut part = Part::new();
    let mut path = top();
    path.geometry.arc(
        Point2::new(20.0, 0.0),
        Point2::new(0.0, 0.0),
        Point2::new(20.0, -20.0),
    );
    swept(&mut part, circle_at(0.0, 5.0), path, ExtrudeOp::Boss);
    assert_same_part("a circle swept along an arc", &part);
    // Round a whole circle: a ring.
    let mut part = Part::new();
    let mut path = top();
    path.circle(Point2::ORIGIN, 20.0);
    swept(&mut part, circle_at(20.0, 5.0), path, ExtrudeOp::Boss);
    assert_same_part("a circle swept round a circle", &part);
    // Along a line, then a quarter turn that carries it on.
    let mut part = Part::new();
    let mut path = top();
    let a = path.geometry.point(Point2::new(0.0, 0.0));
    let b = path.geometry.point(Point2::new(0.0, -20.0));
    path.geometry.line_between(a, b);
    let c = path.geometry.point(Point2::new(10.0, -20.0));
    let d = path.geometry.point(Point2::new(10.0, -30.0));
    path.geometry.arc_between(c, b, d);
    swept(&mut part, circle_at(0.0, 3.0), path, ExtrudeOp::Boss);
    assert_same_part("a circle swept along a line and an arc", &part);
    // A square along an arc.
    let mut part = Part::new();
    let mut square = front();
    square.rectangle(Point2::new(-2.0, -2.0), 4.0, 4.0);
    let mut path = top();
    path.geometry.arc(
        Point2::new(20.0, 0.0),
        Point2::new(0.0, 0.0),
        Point2::new(20.0, -20.0),
    );
    swept(&mut part, square, path, ExtrudeOp::Boss);
    assert_same_part("a square swept along an arc", &part);
    // A slot along an arc and on along a line: its round ends sweep tori, then
    // cylinders.
    let mut part = Part::new();
    let mut slot = front();
    slot.line(Point2::new(-2.0, -1.0), Point2::new(2.0, -1.0));
    arc(&mut slot, (2.0, 0.0), (2.0, -1.0), (2.0, 1.0));
    slot.line(Point2::new(2.0, 1.0), Point2::new(-2.0, 1.0));
    arc(&mut slot, (-2.0, 0.0), (-2.0, 1.0), (-2.0, -1.0));
    let mut path = top();
    let a = path.geometry.point(Point2::new(0.0, 0.0));
    let b = path.geometry.point(Point2::new(20.0, -20.0));
    let c = path.geometry.point(Point2::new(20.0, 0.0));
    path.geometry.arc_between(c, a, b);
    path.geometry
        .line(Point2::new(20.0, -20.0), Point2::new(40.0, -20.0));
    swept(&mut part, slot, path, ExtrudeOp::Boss);
    assert_same_part("a slot swept along an arc and a line", &part);
}

#[test]
fn a_cut_sweep_and_a_sweep_round_a_corner() {
    // A groove swept down into a block.
    let mut part = Part::new();
    let mut sketch = front();
    sketch.rectangle(Point2::new(0.0, 0.0), 40.0, 40.0);
    boss(
        &mut part,
        sketch,
        ExtrudeExtent::Blind { depth: 10.0 },
        false,
    );
    let mut profile = front();
    profile.circle(Point2::new(10.0, 20.0), 2.0);
    let mut path = top();
    path.geometry
        .line(Point2::new(10.0, 0.0), Point2::new(10.0, -10.0));
    swept(&mut part, profile, path, ExtrudeOp::Cut);
    assert_same_part("a cut swept into a block", &part);
    // Two lines meeting at a corner: refused, saying why.
    let mut part = Part::new();
    let mut profile = front();
    profile.circle(Point2::new(0.0, 0.0), 3.0);
    let mut path = top();
    let a = path.geometry.point(Point2::new(0.0, 0.0));
    let b = path.geometry.point(Point2::new(0.0, -20.0));
    let c = path.geometry.point(Point2::new(15.0, -20.0));
    path.geometry.line_between(a, b);
    path.geometry.line_between(b, c);
    let sweep = swept(&mut part, profile, path, ExtrudeOp::Boss);
    let mut kernel = NativeKernel::new();
    part.rebuild(&mut kernel);
    match part.status(sweep) {
        Some(FeatureStatus::Error(message)) => {
            assert!(message.contains("turns sharply"), "{message}")
        }
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
}

#[test]
fn tori_cut_beside_their_axis() {
    // A ring turned about Y, its tube 4 round a circle of radius 12.
    let ring = || {
        let mut part = Part::new();
        let (mut section, axis) = about_y(-20.0, 20.0);
        section.circle(Point2::new(12.0, 0.0), 4.0);
        let ring = revolve(&mut part, section, axis, ExtrudeOp::Boss, WHOLE);
        (part, ring)
    };
    // Cut by a block whose face runs along the axis, through the tube.
    let (mut part, _) = ring();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    sketch.rectangle(Point2::new(-30.0, -30.0), 60.0, 60.0);
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::Blind { depth: 10.0 },
        false,
    );
    assert_same_part("a ring cut along its axis beside it", &part);
    // A hole down through the tube.
    let (mut part, _) = ring();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    sketch.circle(Point2::new(12.0, 0.0), 2.0);
    extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
        false,
    );
    assert_same_part("a hole down through a ring's tube", &part);
}

#[test]
fn a_hole_across_a_rounded_rim() {
    // A hole's rim rounded, then a hole across from the side through the round.
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    let circle = sketch.circle(Point2::new(20.0, 15.0), 6.0);
    let hole = extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: false },
        false,
    );
    part.add_feature(blended_by(
        true,
        vec![EdgeName::between(
            FaceName::profile(hole, circle),
            FaceName::end_cap(base),
        )],
        2.0,
    ));
    for (z, r, what) in [
        // Through the round only.
        (9.0, 0.8, "a hole across a rounded rim"),
        (
            8.0,
            1.4,
            "a hole across a rounded rim and the wall below it, thinner",
        ),
        // Through the round and the hole's wall below it, across where they meet.
        (
            8.0,
            1.5,
            "a hole across a rounded rim and the wall below it",
        ),
    ] {
        let mut part = Part::from_definition(part.definition()).unwrap();
        let mut side = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
        side.circle(Point2::new(-z, if r == 1.4 { 15.3 } else { 15.0 }), r);
        extrude(
            &mut part,
            side,
            ExtrudeOp::Cut,
            ExtrudeExtent::ThroughAll { both: true },
            false,
        );
        assert_same_part(what, &part);
    }
}

#[test]
fn a_hole_touching_a_round_where_it_crosses_it_fails_saying_so() {
    // A hole's rim rounded, then a hole from the side whose top touches the round's torus
    // where the torus touches the block's top: the curve they cross in runs through a
    // point where they touch, which the native kernel does not follow yet.
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    let circle = sketch.circle(Point2::new(20.0, 15.0), 6.0);
    let hole = extrude(
        &mut part,
        sketch,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: false },
        false,
    );
    part.add_feature(blended_by(
        true,
        vec![EdgeName::between(
            FaceName::profile(hole, circle),
            FaceName::end_cap(base),
        )],
        2.0,
    ));
    let mut side = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    side.circle(Point2::new(-8.5, 15.0), 1.5);
    let across = extrude(
        &mut part,
        side,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
        false,
    );
    let mut kernel = NativeKernel::new();
    part.rebuild(&mut kernel);
    match part.status(across) {
        Some(FeatureStatus::Error(message)) => {
            assert!(message.contains("touch"), "{message}")
        }
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
}

// ---------------------------------------------------------------------------------------
// Lofts: B-spline surfaces through the sections, as OpenCASCADE makes them.

/// A sketch on the Front plane moved `z` along its normal (the Front plane itself at 0).
fn section_sketch(part: &mut Part, z: f64) -> Sketch {
    if z == 0.0 {
        return front();
    }
    let plane = part.add_feature(FeatureKind::Plane(oxyde_model::RefPlane::Offset {
        base: oxyde_model::PlaneRef::Standard(StandardPlane::Front),
        offset: z,
        flip: false,
    }));
    Sketch::new(SketchPlane::Reference(plane))
}

/// What a loft section is: drawn on its sketch.
type Draw = Box<dyn Fn(&mut Sketch)>;

fn square_of(side: f64) -> Draw {
    Box::new(move |s: &mut Sketch| {
        s.rectangle(Point2::new(-side / 2.0, -side / 2.0), side, side);
    })
}

fn rectangle_of(w: f64, h: f64) -> Draw {
    Box::new(move |s: &mut Sketch| {
        s.rectangle(Point2::new(-w / 2.0, -h / 2.0), w, h);
    })
}

fn circle_of(r: f64) -> Draw {
    Box::new(move |s: &mut Sketch| {
        s.circle(Point2::ORIGIN, r);
    })
}

fn polygon_of(corners: Vec<(f64, f64)>) -> Draw {
    Box::new(move |s: &mut Sketch| polygon(s, &corners))
}

/// A slot `w` between its arcs' centres, `r` round them, on the sketch's x axis.
fn slot_of(w: f64, r: f64) -> Draw {
    Box::new(move |s: &mut Sketch| {
        let h = w / 2.0;
        s.line(Point2::new(-h, -r), Point2::new(h, -r));
        arc(s, (h, 0.0), (h, -r), (h, r));
        s.line(Point2::new(h, r), Point2::new(-h, r));
        arc(s, (-h, 0.0), (-h, r), (-h, -r));
    })
}

/// A part of one loft through `sections`, each drawn at its height.
fn loft_of(sections: Vec<(f64, Draw)>, ruled: bool) -> Part {
    let mut part = Part::new();
    let mut profiles = Vec::new();
    for (z, draw) in sections {
        let mut sketch = section_sketch(&mut part, z);
        draw(&mut sketch);
        profiles.push(part.add_feature(FeatureKind::Sketch(sketch)));
    }
    part.add_feature(FeatureKind::Loft(oxyde_model::Loft {
        profiles,
        op: ExtrudeOp::NewBody,
        ruled,
        bodies: Vec::new(),
    }));
    part
}

#[test]
fn lofts_between_two_sections() {
    for ruled in [false, true] {
        let way = if ruled { "ruled" } else { "smooth" };
        let cases: Vec<(&str, Vec<(f64, Draw)>)> = vec![
            (
                "squares",
                vec![(0.0, square_of(20.0)), (15.0, square_of(10.0))],
            ),
            (
                "narrowing circles",
                vec![(0.0, circle_of(10.0)), (15.0, circle_of(5.0))],
            ),
            (
                "widening circles",
                vec![(0.0, circle_of(5.0)), (15.0, circle_of(10.0))],
            ),
            (
                "equal circles",
                vec![(0.0, circle_of(10.0)), (15.0, circle_of(10.0))],
            ),
            (
                "a circle to a square",
                vec![(0.0, circle_of(8.0)), (15.0, square_of(12.0))],
            ),
            (
                "a rectangle to a circle",
                vec![(0.0, rectangle_of(16.0, 8.0)), (15.0, circle_of(8.0))],
            ),
            (
                "a triangle to a square",
                vec![
                    (
                        0.0,
                        polygon_of(vec![
                            (10.0, 0.0),
                            (-5.0, 8.660254037844386),
                            (-5.0, -8.660254037844386),
                        ]),
                    ),
                    (15.0, square_of(12.0)),
                ],
            ),
            (
                "slots",
                vec![(0.0, slot_of(20.0, 5.0)), (15.0, slot_of(10.0, 3.0))],
            ),
            (
                "a rectangle to one across it",
                vec![
                    (0.0, rectangle_of(16.0, 8.0)),
                    (15.0, rectangle_of(8.0, 16.0)),
                ],
            ),
        ];
        for (what, sections) in cases {
            assert_same_exactly(&format!("{way} loft, {what}"), &loft_of(sections, ruled));
        }
    }
}

#[test]
fn lofts_through_three_sections_and_more() {
    let squares = |secs: &[(f64, f64)]| -> Vec<(f64, Draw)> {
        secs.iter().map(|&(z, s)| (z, square_of(s))).collect()
    };
    let circles = |secs: &[(f64, f64)]| -> Vec<(f64, Draw)> {
        secs.iter().map(|&(z, s)| (z, circle_of(s / 2.0))).collect()
    };
    let spacings: [&[(f64, f64)]; 4] = [
        &[(0.0, 20.0), (5.0, 10.0), (20.0, 20.0)],
        &[(0.0, 20.0), (4.0, 10.0), (20.0, 15.0), (24.0, 20.0)],
        &[
            (0.0, 20.0),
            (3.0, 10.0),
            (11.0, 15.0),
            (30.0, 20.0),
            (32.0, 8.0),
        ],
        &[
            (0.0, 20.0),
            (10.0, 10.0),
            (20.0, 15.0),
            (30.0, 20.0),
            (40.0, 8.0),
            (50.0, 12.0),
            (60.0, 14.0),
        ],
    ];
    for secs in spacings {
        for ruled in [false, true] {
            let way = if ruled { "ruled" } else { "smooth" };
            assert_same_exactly(
                &format!("{way} squares {secs:?}"),
                &loft_of(squares(secs), ruled),
            );
            assert_same_exactly(
                &format!("{way} circles {secs:?}"),
                &loft_of(circles(secs), ruled),
            );
        }
    }
    let mixed: Vec<(f64, Draw)> = vec![
        (0.0, rectangle_of(16.0, 8.0)),
        (4.0, circle_of(8.0)),
        (20.0, square_of(10.0)),
    ];
    assert_same_exactly("smooth rectangle, circle, square", &loft_of(mixed, false));
    let slots: Vec<(f64, Draw)> = vec![
        (0.0, slot_of(20.0, 5.0)),
        (10.0, slot_of(10.0, 3.0)),
        (25.0, slot_of(16.0, 4.0)),
    ];
    assert_same_exactly("smooth slots", &loft_of(slots, false));
}

/// A loft of `op` through `sections` drawn on sketches the caller made.
fn loft_feature(
    part: &mut Part,
    profiles: Vec<FeatureId>,
    op: ExtrudeOp,
    ruled: bool,
) -> FeatureId {
    part.add_feature(FeatureKind::Loft(oxyde_model::Loft {
        profiles,
        op,
        ruled,
        bodies: Vec::new(),
    }))
}

#[test]
fn lofts_joined_to_cut_from_and_cut_by_other_bodies() {
    // A loft from a circle on a block's top up to a square, joined to the block.
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut round = on_top(base);
    round.circle(Point2::new(20.0, 15.0), 8.0);
    let round = part.add_feature(FeatureKind::Sketch(round));
    let mut square = section_sketch(&mut part, 25.0);
    square.rectangle(Point2::new(14.0, 9.0), 12.0, 12.0);
    let square = part.add_feature(FeatureKind::Sketch(square));
    loft_feature(&mut part, vec![round, square], ExtrudeOp::Boss, false);
    assert_same_exactly("a loft on a block", &part);

    // A smooth loft of three circles cut out of a block, through it.
    let mut part = Part::new();
    base_block(&mut part);
    let mut profiles = Vec::new();
    for (z, r) in [(-5.0, 6.0), (5.0, 9.0), (15.0, 5.0)] {
        let mut s = section_sketch(&mut part, z);
        s.circle(Point2::new(20.0, 15.0), r);
        profiles.push(part.add_feature(FeatureKind::Sketch(s)));
    }
    loft_feature(&mut part, profiles, ExtrudeOp::Cut, false);
    assert_same_part("a smooth loft cut through a block", &part);

    // A hole across a smooth loft of squares.
    let mut part = Part::new();
    let mut profiles = Vec::new();
    for (z, side) in [(0.0, 20.0), (10.0, 14.0), (25.0, 18.0)] {
        let mut s = section_sketch(&mut part, z);
        s.rectangle(Point2::new(-side / 2.0, -side / 2.0), side, side);
        profiles.push(part.add_feature(FeatureKind::Sketch(s)));
    }
    loft_feature(&mut part, profiles, ExtrudeOp::NewBody, false);
    let solid_loft = Part::from_definition(part.definition()).unwrap();
    // On the Right plane, the sketch's x runs down the model's z.
    let mut hole = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    hole.circle(Point2::new(-12.0, 0.0), 3.0);
    cut(&mut part, hole, ExtrudeExtent::ThroughAll { both: true });
    assert_same_part("a hole across a smooth loft", &part);
    // And one across its bottom corner, partly in it.
    let mut part = solid_loft;
    let mut hole = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    hole.circle(Point2::new(0.0, 12.0), 3.0);
    cut(&mut part, hole, ExtrudeExtent::ThroughAll { both: true });
    assert_same_part("a hole across a smooth loft's corner", &part);

    // A smooth loft of circles cut by a slab beside its axis.
    let mut part = Part::new();
    let mut profiles = Vec::new();
    for (z, r) in [(0.0, 10.0), (10.0, 6.0), (20.0, 9.0)] {
        let mut s = section_sketch(&mut part, z);
        s.circle(Point2::ORIGIN, r);
        profiles.push(part.add_feature(FeatureKind::Sketch(s)));
    }
    loft_feature(&mut part, profiles, ExtrudeOp::NewBody, false);
    let mut slab = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    slab.rectangle(Point2::new(-24.0, -5.0), 20.0, 30.0);
    cut(&mut part, slab, ExtrudeExtent::ThroughAll { both: true });
    assert_same_part("a smooth loft of circles cut beside its axis", &part);
}

#[test]
fn lofts_to_sections_on_planes_at_an_angle() {
    // The Front plane turned about the X axis, then moved along its normal: a sketch on it
    // keeps the Front plane's frame, turned with it.
    let tilted = |part: &mut Part, angle: f64, offset: f64| -> Sketch {
        let turned = part.add_feature(FeatureKind::Plane(oxyde_model::RefPlane::Angle {
            base: oxyde_model::PlaneRef::Standard(StandardPlane::Front),
            line: AxisRef::Standard(StandardAxis::X),
            angle,
            flip: false,
        }));
        let up = part.add_feature(FeatureKind::Plane(oxyde_model::RefPlane::Offset {
            base: oxyde_model::PlaneRef::Reference(turned),
            offset,
            flip: false,
        }));
        Sketch::new(SketchPlane::Reference(up))
    };
    let cases: Vec<(&str, Draw, Draw)> = vec![
        (
            "a square to a smaller one",
            square_of(20.0),
            square_of(10.0),
        ),
        (
            "a square to a triangle",
            square_of(20.0),
            polygon_of(vec![
                (8.0, 0.0),
                (-4.0, 6.928203230275509),
                (-4.0, -6.928203230275509),
            ]),
        ),
        ("a circle to a square", circle_of(8.0), square_of(12.0)),
        (
            "a rectangle to a circle",
            rectangle_of(16.0, 8.0),
            circle_of(6.0),
        ),
    ];
    for (what, first, last) in cases {
        for ruled in [false, true] {
            let mut part = Part::new();
            let mut a = front();
            first(&mut a);
            let a = part.add_feature(FeatureKind::Sketch(a));
            let mut b = tilted(&mut part, 30f64.to_radians(), 15.0);
            last(&mut b);
            let b = part.add_feature(FeatureKind::Sketch(b));
            loft_feature(&mut part, vec![a, b], ExtrudeOp::NewBody, ruled);
            assert_same_exactly(&format!("{what} at an angle, ruled {ruled}"), &part);
        }
    }
}

// ---------------------------------------------------------------------------------------
// STEP files.

/// The volume, centre of mass, bounds and number of faces of a body's shapes together.
fn totals<K: Kernel>(kernel: &K, shapes: &[ShapeHandle]) -> (f64, Point3, (Point3, Point3), usize) {
    let mut volume = 0.0;
    let mut moment = Vec3::ZERO;
    let mut lo = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut hi = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut faces = 0;
    for &s in shapes {
        let m = measures(kernel, s);
        volume += m.volume;
        moment = moment + m.centre.to_vec() * m.volume;
        lo = Point3::new(
            lo.x.min(m.bounds.0.x),
            lo.y.min(m.bounds.0.y),
            lo.z.min(m.bounds.0.z),
        );
        hi = Point3::new(
            hi.x.max(m.bounds.1.x),
            hi.y.max(m.bounds.1.y),
            hi.z.max(m.bounds.1.z),
        );
        faces += m.counts.faces;
    }
    let c = moment * (1.0 / volume);
    (volume, Point3::new(c.x, c.y, c.z), (lo, hi), faces)
}

/// What the native kernel builds of `part`, written to STEP by it and read back by
/// OpenCASCADE: the solids OpenCASCADE builds of it, face for face.
fn assert_written_step_reads(what: &str, part: &Part) {
    let mut native = NativeKernel::new();
    let mut built = Part::from_definition(part.definition()).unwrap();
    built.rebuild(&mut native);
    for entry in built.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{what}: {}", entry.name);
    }
    let shapes: Vec<ShapeHandle> = built
        .body()
        .unwrap()
        .solids
        .iter()
        .map(|s| s.shape)
        .collect();
    let path = std::env::temp_dir().join(format!("oxyde-native-{}.step", what.replace(' ', "-")));
    native.export_step(&shapes, &[], &[], &path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut occt = OcctKernel::new();
    let read = occt.import_step(&bytes).unwrap();
    let (volume, centre, bounds, faces) = totals(&occt, &[read.shape]);
    let mut reference = Part::from_definition(part.definition()).unwrap();
    reference.rebuild(&mut occt);
    let want: Vec<ShapeHandle> = reference
        .body()
        .unwrap()
        .solids
        .iter()
        .map(|s| s.shape)
        .collect();
    let (v, c, b, f) = totals(&occt, &want);
    assert_close(&format!("{what}, volume"), volume, v, FITTED);
    assert_point_close(&format!("{what}, centre"), centre, c, FITTED);
    assert_point_close(&format!("{what}, lower bound"), bounds.0, b.0, FITTED);
    assert_point_close(&format!("{what}, upper bound"), bounds.1, b.1, FITTED);
    assert_eq!(faces, f, "{what}: faces");
    built.release(&mut native);
    reference.release(&mut occt);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn step_files_the_native_kernel_writes_read_in_opencascade() {
    let mut part = Part::new();
    base_block(&mut part);
    assert_written_step_reads("block", &part);
    // Holes, a slot, arcs.
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(10.0, 15.0), 4.0);
    sketch.circle(Point2::new(30.0, 15.0), 3.0);
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
    assert_written_step_reads("block with holes", &part);
    // Spheres, cones, tori: their poles and apexes have no edges in the file.
    let ball = |s: &mut Sketch| {
        let g = &mut s.geometry;
        let (south, north, centre) = (
            g.point(Point2::new(0.0, -5.0)),
            g.point(Point2::new(0.0, 5.0)),
            g.point(Point2::new(0.0, 0.0)),
        );
        g.arc_between(centre, south, north);
        g.line_between(north, south);
    };
    assert_written_step_reads("ball", &revolved_with(ball, true, WHOLE));
    assert_written_step_reads(
        "half ball",
        &revolved_with(ball, true, std::f64::consts::PI),
    );
    let cone = |s: &mut Sketch| polygon(s, &[(0.0, 0.0), (5.0, 0.0), (0.0, 10.0)]);
    assert_written_step_reads("cone", &revolved_with(cone, true, WHOLE));
    let frustum = |s: &mut Sketch| polygon(s, &[(2.0, 0.0), (6.0, 0.0), (3.0, 10.0), (2.0, 10.0)]);
    assert_written_step_reads("frustum", &revolved_with(frustum, false, WHOLE));
    let torus = |s: &mut Sketch| {
        s.circle(Point2::new(10.0, 0.0), 3.0);
    };
    assert_written_step_reads("torus", &revolved_with(torus, true, WHOLE));
    // Two holes across each other (B-spline edges, fitted) and a bar cut at a slant (an
    // ellipse).
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 4.0);
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    sketch.circle(Point2::new(-5.0, 15.0), 3.0);
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: true });
    assert_written_step_reads("crossing holes", &part);
    // Lofts: B-spline surfaces.
    assert_written_step_reads(
        "a loft of circles",
        &loft_of(
            vec![
                (0.0, circle_of(10.0)),
                (10.0, circle_of(5.0)),
                (20.0, circle_of(10.0)),
            ],
            false,
        ),
    );
    assert_written_step_reads(
        "a loft from a circle to a square",
        &loft_of(vec![(0.0, circle_of(8.0)), (15.0, square_of(12.0))], false),
    );
}

/// A STEP file read by both kernels: the same volume, centre of mass, bounds, faces,
/// colours and material.
fn assert_reads_as_opencascade_does(what: &str, bytes: &[u8], tolerance: f64) {
    let (native, occt) = assert_reads_solids_as_opencascade_does(what, bytes, tolerance);
    assert_eq!(native, occt, "{what}: material");
}

/// [`assert_reads_as_opencascade_does`] but for the material, which each kernel read.
fn assert_reads_solids_as_opencascade_does(
    what: &str,
    bytes: &[u8],
    tolerance: f64,
) -> (
    Option<oxyde_kernel::StepMaterial>,
    Option<oxyde_kernel::StepMaterial>,
) {
    let mut native = NativeKernel::new();
    let read = native
        .import_step(bytes)
        .unwrap_or_else(|e| panic!("{what}: {e}"));
    let mut occt = OcctKernel::new();
    let reference = occt.import_step(bytes).unwrap();
    let (volume, centre, bounds, faces) = totals(&native, &[read.shape]);
    let (v, c, b, f) = totals(&occt, &[reference.shape]);
    assert_close(&format!("{what}, volume"), volume, v, tolerance);
    assert_point_close(&format!("{what}, centre"), centre, c, tolerance);
    assert_point_close(&format!("{what}, lower bound"), bounds.0, b.0, tolerance);
    assert_point_close(&format!("{what}, upper bound"), bounds.1, b.1, tolerance);
    assert_eq!(faces, f, "{what}: faces");
    assert_eq!(read.colors, reference.colors, "{what}: colours");
    native.release(read.shape);
    occt.release(reference.shape);
    (read.material, reference.material)
}

/// What OpenCASCADE builds of `part`, written to STEP by it and read by the native kernel:
/// what OpenCASCADE reads of the same file, face for face.
fn assert_opencascade_step_reads(what: &str, part: &Part) {
    let mut occt = OcctKernel::new();
    let mut built = Part::from_definition(part.definition()).unwrap();
    built.rebuild(&mut occt);
    for entry in built.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{what}: {}", entry.name);
    }
    let shapes: Vec<ShapeHandle> = built
        .body()
        .unwrap()
        .solids
        .iter()
        .map(|s| s.shape)
        .collect();
    let path = std::env::temp_dir().join(format!("oxyde-occt-{}.step", what.replace(' ', "-")));
    occt.export_step(&shapes, &[], &[], &path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_reads_as_opencascade_does(what, &bytes, FITTED);
    built.release(&mut occt);
}

#[test]
fn step_files_opencascade_writes_read_in_the_native_kernel() {
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(10.0, 15.0), 4.0);
    sketch.circle(Point2::new(30.0, 15.0), 3.0);
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
    assert_opencascade_step_reads("block with holes", &part);
    // Spheres, cones, tori: OpenCASCADE leaves their poles' and apexes' edges out.
    let ball = |s: &mut Sketch| {
        let g = &mut s.geometry;
        let (south, north, centre) = (
            g.point(Point2::new(0.0, -5.0)),
            g.point(Point2::new(0.0, 5.0)),
            g.point(Point2::new(0.0, 0.0)),
        );
        g.arc_between(centre, south, north);
        g.line_between(north, south);
    };
    assert_opencascade_step_reads("ball", &revolved_with(ball, true, WHOLE));
    assert_opencascade_step_reads(
        "half ball",
        &revolved_with(ball, true, std::f64::consts::PI),
    );
    let cone = |s: &mut Sketch| polygon(s, &[(0.0, 0.0), (5.0, 0.0), (0.0, 10.0)]);
    assert_opencascade_step_reads("cone", &revolved_with(cone, true, WHOLE));
    assert_opencascade_step_reads(
        "a third of a cone",
        &revolved_with(cone, false, WHOLE / 3.0),
    );
    let frustum = |s: &mut Sketch| polygon(s, &[(2.0, 0.0), (6.0, 0.0), (3.0, 10.0), (2.0, 10.0)]);
    assert_opencascade_step_reads("frustum", &revolved_with(frustum, false, WHOLE));
    let torus = |s: &mut Sketch| {
        s.circle(Point2::new(10.0, 0.0), 3.0);
    };
    assert_opencascade_step_reads("torus", &revolved_with(torus, true, WHOLE));
    // Fillets: cylinders, tori and the spheres at a corner of three.
    let mut part = Part::new();
    let (b, l) = lined_block(&mut part);
    part.add_feature(blended_by(
        true,
        vec![
            EdgeName::between(FaceName::profile(b, l[0]), FaceName::end_cap(b)),
            EdgeName::between(FaceName::profile(b, l[1]), FaceName::end_cap(b)),
            EdgeName::between(FaceName::profile(b, l[0]), FaceName::profile(b, l[1])),
        ],
        2.0,
    ));
    assert_opencascade_step_reads("a corner of three fillets", &part);
    let mut part = Part::new();
    let (b, l) = lined_block(&mut part);
    part.add_feature(blended_by(
        false,
        vec![EdgeName::between(
            FaceName::profile(b, l[0]),
            FaceName::end_cap(b),
        )],
        2.0,
    ));
    assert_opencascade_step_reads("a chamfer", &part);
    // B-spline edges and surfaces.
    let mut part = Part::new();
    let base = base_block(&mut part);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(20.0, 15.0), 4.0);
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: false });
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Right));
    sketch.circle(Point2::new(-5.0, 15.0), 3.0);
    cut(&mut part, sketch, ExtrudeExtent::ThroughAll { both: true });
    assert_opencascade_step_reads("crossing holes", &part);
    assert_opencascade_step_reads(
        "a loft of circles",
        &loft_of(
            vec![
                (0.0, circle_of(10.0)),
                (10.0, circle_of(5.0)),
                (20.0, circle_of(10.0)),
            ],
            false,
        ),
    );
    assert_opencascade_step_reads(
        "a loft from a circle to a square",
        &loft_of(vec![(0.0, circle_of(8.0)), (15.0, square_of(12.0))], false),
    );
}

fn step_data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The surfaces the native kernel reads a file's faces on, in order.
fn surfaces_read(bytes: &[u8]) -> Vec<SurfaceKind> {
    let mut native = NativeKernel::new();
    let read = native.import_step(bytes).unwrap();
    let kinds = native
        .topology(read.shape)
        .unwrap()
        .faces
        .iter()
        .map(|f| f.surface)
        .collect();
    native.release(read.shape);
    kinds
}

#[test]
fn step_files_other_systems_write_read_in_the_native_kernel() {
    // tests/data/step/make.py writes these, as other systems write such solids.
    assert_reads_as_opencascade_does("cart", &step_data("cart.step"), EXACT);
    let cylinder = step_data("step/seamless_cylinder.step");
    assert_reads_as_opencascade_does("a cylinder without a seam", &cylinder, EXACT);
    let pin = step_data("step/pointed_pin.step");
    assert_reads_as_opencascade_does("a pin in inches with a point", &pin, EXACT);
    let hemisphere = step_data("step/hemisphere_across.step");
    assert_reads_as_opencascade_does("a hemisphere across its sphere's axis", &hemisphere, EXACT);
    // OpenCASCADE takes a rational B-spline face's area by a rule of too few points: its
    // volume of this dome is 4.6e-3 too large. The native kernel reads its surface as the
    // sphere it is.
    let dome = step_data("step/spline_dome.step");
    // The exact volumes.
    let mut native = NativeKernel::new();
    for (what, bytes, volume) in [
        ("cylinder", &cylinder, std::f64::consts::PI * 250.0),
        (
            "pin",
            &pin,
            std::f64::consts::PI * 0.0625 * (1.0 + 0.25 / 3.0) * 25.4f64.powi(3),
        ),
        (
            "hemisphere",
            &hemisphere,
            std::f64::consts::TAU / 3.0 * 1000.0,
        ),
        ("dome", &dome, std::f64::consts::TAU / 3.0 * 1000.0),
    ] {
        let read = native.import_step(bytes).unwrap();
        assert_close(what, native.volume(read.shape).unwrap(), volume, 1e-12);
        native.release(read.shape);
    }
    // A rational B-spline that is a sphere is read as one, its edge a circle.
    assert_eq!(
        surfaces_read(&dome),
        vec![SurfaceKind::Sphere, SurfaceKind::Plane]
    );
    assert_eq!(
        surfaces_read(&pin),
        vec![SurfaceKind::Cylinder, SurfaceKind::Cone, SurfaceKind::Plane]
    );
}

#[test]
fn step_edges_written_against_their_curves_read_the_way_their_vertices_say() {
    // tests/data/step/make.py: a prism on a straight line and a cubic arch over it, its two
    // arch edges written as cadmpeg 0.6.7 writes most of its B-spline edges, the sense .T.
    // with the start vertex at the curve's last pole and the end at its first. Followed
    // forwards from its start, such an edge ran on past the end of its curve and the file
    // was refused (an edge 60 mm off its faces); it is the curve between its vertices, run
    // backwards, as OpenCASCADE reads it.
    let bytes = step_data("step/backwards_spline_edges.step");
    assert_reads_as_opencascade_does("a prism with backwards arch edges", &bytes, EXACT);
    let mut native = NativeKernel::new();
    let read = native.import_step(&bytes).unwrap();
    // The arch's area, 1440 ∫ t²(1 − t)² dt = 48 mm², times its height.
    assert_close(
        "the prism's volume",
        native.volume(read.shape).unwrap(),
        240.0,
        1e-12,
    );
    native.release(read.shape);
}

#[test]
fn a_sphere_s_seam_read_from_a_file_keeps_clear_of_a_hole_joined_to_its_rim() {
    // tests/data/step/make.py: a hemisphere of radius 10 with a flat facet 0.3 rad across,
    // its sphere's axis through the facet and its seam from the facet down to the rim, one
    // loop round both, as Fusion 360 writes such a face. The reader turns the sphere's axis
    // to z, where both of the loop's vertices are at the facet's angle: the seam put up to
    // the pole from there crossed the facet's circle, and the face's mesh covered part of
    // the facet and left as much uncovered beside it.
    let bytes = step_data("step/facetted_dome.step");
    let (r, a) = (10.0f64, 0.3f64);
    let h = r * (1.0 - a.cos());
    let pi = std::f64::consts::PI;
    let exact = 2.0 / 3.0 * pi * r.powi(3) - pi * h * h * (3.0 * r - h) / 3.0;
    let mut native = NativeKernel::new();
    let read = native
        .import_step(&bytes)
        .unwrap_or_else(|e| panic!("the facetted dome: {e}"));
    let volume = native.volume(read.shape).unwrap();
    assert_close("the facetted dome's volume", volume, exact, EXACT);
    // Finer than the viewport's 0.03, whose chords leave out 7e-3 of a ball this size.
    let mesh = native.tessellate(read.shape, 0.002).unwrap();
    let held = mesh_volume(&mesh);
    assert!(
        (held - volume).abs() <= 1e-3 * volume,
        "the mesh holds {held} mm³, the solid {volume} mm³"
    );
    let n = Vec3::new(3f64.sqrt() / 2.0, 0.0, 0.5);
    let topology = native.topology(read.shape).unwrap();
    for (face, info) in mesh.faces.iter().zip(&topology.faces) {
        if info.surface != SurfaceKind::Sphere {
            continue;
        }
        for t in &face.triangles {
            let c = t.iter().fold(Vec3::ZERO, |s, &k| {
                let [x, y, z] = face.positions[k as usize];
                s + Vec3::new(f64::from(x), f64::from(y), f64::from(z))
            });
            let off = c.normalized().unwrap().dot(n).acos();
            assert!(
                off > a - 1e-2,
                "a triangle of the dome lies {off} rad from the facet's middle"
            );
        }
    }
    native.release(read.shape);
}

#[test]
fn step_files_written_loosely_read_in_the_native_kernel() {
    use oxyde_kernel::StepNodeContent;
    // tests/data/step/make.py: a half cylinder written as loosely as its stated accuracy of
    // 0.01 mm allows, as Fusion 360 writes some faces: an arc about a centre off the axis,
    // 5e-3 off the cylinder, and a corner 2e-5 above its arc's other end.
    let bytes = step_data("step/loose_half_cylinder.step");
    let half = std::f64::consts::PI * 25.0 * 10.0 / 2.0;
    let mut native = NativeKernel::new();
    let read = native
        .import_step(&bytes)
        .unwrap_or_else(|e| panic!("the half cylinder: {e}"));
    let volume = native.volume(read.shape).unwrap();
    // As close as its loose top allows: 5e-3 mm off a radius of 5.
    assert_close("the half cylinder's volume", volume, half, 1e-3);
    native.release(read.shape);
    // OpenCASCADE integrates along the file's curves in the surface's parameters, the native
    // kernel along the curves themselves (§8 of docs/NATIVE_KERNEL.md): 1.7e-4 apart here.
    let mut occt = OcctKernel::new();
    let reference = occt.import_step(&bytes).unwrap();
    assert_close(
        "the half cylinder's volume against OpenCASCADE's",
        volume,
        occt.volume(reference.shape).unwrap(),
        1e-3,
    );
    occt.release(reference.shape);
    // Opening it reads its part from a file of its own the native kernel writes: stating
    // the accuracy the part's edges need, it reads back.
    let tree = native.read_step_assembly(&bytes).unwrap();
    let [node] = tree.nodes.as_slice() else {
        panic!("one part, not {} nodes", tree.nodes.len());
    };
    let StepNodeContent::Part { step, .. } = &node.content else {
        panic!("a part");
    };
    let part = native
        .import_step(step)
        .unwrap_or_else(|e| panic!("the part's own file: {e}"));
    assert_close(
        "the part's volume",
        native.volume(part.shape).unwrap(),
        volume,
        1e-9,
    );
    native.release(part.shape);
}

#[test]
fn step_files_the_native_kernel_writes_read_back_in_it() {
    use oxyde_kernel::{StepColor, StepColors, StepMaterial};
    let ball = |s: &mut Sketch| {
        let g = &mut s.geometry;
        let (south, north, centre) = (
            g.point(Point2::new(0.0, -5.0)),
            g.point(Point2::new(0.0, 5.0)),
            g.point(Point2::new(0.0, 0.0)),
        );
        g.arc_between(centre, south, north);
        g.line_between(north, south);
    };
    let mut holes = Part::new();
    let base = base_block(&mut holes);
    let mut sketch = on_top(base);
    sketch.circle(Point2::new(10.0, 15.0), 4.0);
    cut(
        &mut holes,
        sketch,
        ExtrudeExtent::ThroughAll { both: false },
    );
    let torus = |s: &mut Sketch| {
        s.circle(Point2::new(10.0, 0.0), 3.0);
    };
    let parts = [
        ("block with a hole", holes),
        ("ball", revolved_with(ball, true, WHOLE)),
        ("torus", revolved_with(torus, true, WHOLE)),
        (
            "loft",
            loft_of(vec![(0.0, circle_of(8.0)), (15.0, square_of(12.0))], false),
        ),
    ];
    for (what, part) in parts {
        let mut native = NativeKernel::new();
        let mut built = Part::from_definition(part.definition()).unwrap();
        built.rebuild(&mut native);
        let shapes: Vec<ShapeHandle> = built
            .body()
            .unwrap()
            .solids
            .iter()
            .map(|s| s.shape)
            .collect();
        let faces = native.topology(shapes[0]).unwrap().faces.len();
        let red = StepColor {
            rgb: [200, 30, 30],
            transparency: 0.0,
        };
        let blue = StepColor {
            rgb: [20, 40, 220],
            transparency: 0.25,
        };
        let mut face_colors = vec![Some(red); faces];
        face_colors[0] = Some(blue);
        let colors = StepColors {
            solids: vec![Some(red)],
            faces: face_colors,
        };
        let material = StepMaterial {
            name: "Aluminium".to_owned(),
            description: "6061-T6".to_owned(),
            density: 2700.0,
        };
        let path =
            std::env::temp_dir().join(format!("oxyde-native-back-{}.step", what.replace(' ', "-")));
        native
            .export_step(
                &shapes,
                std::slice::from_ref(&colors),
                &[Some(material.clone())],
                &path,
            )
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let read = native.import_step(&bytes).unwrap();
        let (volume, centre, bounds, n) = totals(&native, &[read.shape]);
        let (v, c, b, f) = totals(&native, &shapes);
        assert_close(&format!("{what}, volume"), volume, v, 1e-12);
        assert_point_close(&format!("{what}, centre"), centre, c, 1e-12);
        assert_point_close(&format!("{what}, lower bound"), bounds.0, b.0, 1e-9);
        assert_point_close(&format!("{what}, upper bound"), bounds.1, b.1, 1e-9);
        assert_eq!(n, f, "{what}: faces");
        assert_eq!(
            read.colors.solids, colors.solids,
            "{what}: the solid's colour"
        );
        let blues = read
            .colors
            .faces
            .iter()
            .filter(|c| c.is_some_and(|c| c.rgb == blue.rgb))
            .count();
        assert_eq!(blues, 1, "{what}: one face blue");
        assert_eq!(read.material, Some(material), "{what}: material");
        // OpenCASCADE reads what the native kernel read.
        assert_reads_as_opencascade_does(what, &bytes, FITTED);
        native.release(read.shape);
        built.release(&mut native);
    }
}

#[test]
fn step_assemblies_read_in_the_native_kernel() {
    use oxyde_kernel::StepNodeContent;
    let bytes = step_data("step/assembly.step");
    // The material the file names for its part: OpenCASCADE gives none for a file whose
    // top product is an assembly (docs/NATIVE_KERNEL.md §8).
    let (material, _) =
        assert_reads_solids_as_opencascade_does("an assembly of two blocks", &bytes, EXACT);
    assert_eq!(material.map(|m| m.name), Some("Steel".to_owned()));
    let mut native = NativeKernel::new();
    let mut occt = OcctKernel::new();
    let tree = native.read_step_assembly(&bytes).unwrap();
    let reference = occt.read_step_assembly(&bytes).unwrap();
    assert_eq!(tree.nodes.len(), reference.nodes.len(), "nodes");
    let instances = |t: &oxyde_kernel::StepAssembly| match &t.nodes[t.root].content {
        StepNodeContent::Assembly { instances } => instances.clone(),
        StepNodeContent::Part { .. } => panic!("the root is a part"),
    };
    let (ours, theirs) = (instances(&tree), instances(&reference));
    assert_eq!(ours.len(), 2);
    assert_eq!(ours.len(), theirs.len());
    assert_eq!(
        tree.nodes[tree.root].name,
        reference.nodes[reference.root].name
    );
    for (a, b) in ours.iter().zip(&theirs) {
        assert_eq!(a.name, b.name, "instance names");
        for (ra, rb) in a.transform.rows.iter().zip(&b.transform.rows) {
            for (x, y) in ra.iter().zip(rb) {
                assert!(
                    (x - y).abs() <= 1e-12,
                    "{}: {:?} against {:?}",
                    a.name,
                    a.transform,
                    b.transform
                );
            }
        }
        let (StepNodeContent::Part { step, material }, StepNodeContent::Part { material: m, .. }) = (
            &tree.nodes[a.node].content,
            &reference.nodes[b.node].content,
        ) else {
            panic!("instances of parts");
        };
        // OpenCASCADE's density comes back through its own unit, a rounding off.
        let (material, m) = (material.as_ref().unwrap(), m.as_ref().unwrap());
        assert_eq!(
            (&material.name, &material.description),
            (&m.name, &m.description)
        );
        assert_close("the part's density", material.density, m.density, 1e-12);
        assert_eq!(
            tree.nodes[a.node].name, reference.nodes[b.node].name,
            "the part's name"
        );
        // The part's own file reads as the block, in its own place.
        let part = native.import_step(step).unwrap();
        assert_close(
            "the part's volume",
            native.volume(part.shape).unwrap(),
            6000.0,
            1e-12,
        );
        let (lo, hi) = native.bounds(part.shape).unwrap();
        assert_point_close("the part's corner", lo, Point3::new(0.0, 0.0, 0.0), 1e-12);
        assert_point_close(
            "the part's far corner",
            hi,
            Point3::new(10.0, 20.0, 30.0),
            1e-12,
        );
        assert_eq!(
            part.colors.solids,
            vec![Some(oxyde_kernel::StepColor {
                rgb: [255, 0, 0],
                transparency: 0.0
            })]
        );
        native.release(part.shape);
    }
    // One part, used twice.
    assert_eq!(ours[0].node, ours[1].node);
}
