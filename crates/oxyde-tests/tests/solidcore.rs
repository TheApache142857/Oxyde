//! The solidcore spike against OpenCASCADE: the same parts built with both kernels give the
//! same volumes, areas, centres, bounds, topology and persistent face names, and solidcore
//! gives the same names every rebuild. The tolerances are the comparison's, chosen before
//! running it: measures within 1e-6 relative, positions within 1e-6 of the part's size.

use oxyde_geom::{Point2, Point3, Transform, Vec3};
use oxyde_kernel::{FaceSource, Kernel};
use oxyde_model::{
    Extrude, ExtrudeOp, FeatureKind, FeatureStatus, Part, Sketch, SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;
use oxyde_solidcore::SolidcoreKernel;

/// Measures relative, positions relative to the part's size.
const RELATIVE: f64 = 1e-6;

/// What the comparison looks at in a built part.
#[derive(Debug)]
struct Measured {
    volume: f64,
    area: f64,
    centre: [f64; 3],
    lo: [f64; 3],
    hi: [f64; 3],
    faces: usize,
    edges: usize,
    vertices: usize,
    names: Vec<String>,
    /// The surface kind of each face, sorted.
    surfaces: Vec<String>,
}

fn arr(p: Point3) -> [f64; 3] {
    [p.x, p.y, p.z]
}

fn measure<K: Kernel>(part: &Part, kernel: &K) -> Measured {
    for entry in part.timeline() {
        assert_eq!(
            entry.status,
            FeatureStatus::Ok,
            "{}: {:?}",
            entry.name,
            entry.status
        );
    }
    let body = part.body().expect("a body");
    let shapes = body.shapes();
    assert_eq!(shapes.len(), 1, "one solid");
    let shape = shapes[0];
    let (lo, hi) = kernel.bounds(shape).unwrap();
    let mut names: Vec<String> = body
        .naming
        .faces()
        .iter()
        .map(|n| format!("{n:?}"))
        .collect();
    names.sort();
    let mut surfaces: Vec<String> = body
        .topology
        .faces
        .iter()
        .map(|f| format!("{:?}", f.surface))
        .collect();
    surfaces.sort();
    Measured {
        volume: kernel.volume(shape).unwrap(),
        area: body.topology.faces.iter().map(|f| f.area).sum(),
        centre: arr(kernel.centre_of_mass(shape).unwrap()),
        lo: arr(lo),
        hi: arr(hi),
        faces: body.topology.faces.len(),
        edges: body.topology.edges.len(),
        vertices: body.topology.vertices.len(),
        names,
        surfaces,
    }
}

/// Where the measures (volume, area, centre, bounds) disagree, none when they agree.
fn measures_differ(occt: &Measured, solidcore: &Measured) -> Vec<String> {
    let size = (0..3)
        .map(|i| occt.hi[i] - occt.lo[i])
        .fold(0.0f64, f64::max)
        .max(1.0);
    let rel = |a: f64, b: f64| (a - b).abs() <= RELATIVE * a.abs().max(b.abs()).max(1.0);
    let at = |a: [f64; 3], b: [f64; 3]| (0..3).all(|i| (a[i] - b[i]).abs() <= RELATIVE * size);
    let mut problems = Vec::new();
    if !rel(occt.volume, solidcore.volume) {
        problems.push(format!(
            "volume {} against {}",
            solidcore.volume, occt.volume
        ));
    }
    if !rel(occt.area, solidcore.area) {
        problems.push(format!("area {} against {}", solidcore.area, occt.area));
    }
    if !at(occt.centre, solidcore.centre) {
        problems.push(format!(
            "centre {:?} against {:?}",
            solidcore.centre, occt.centre
        ));
    }
    if !at(occt.lo, solidcore.lo) || !at(occt.hi, solidcore.hi) {
        problems.push(format!(
            "bounds {:?}..{:?} against {:?}..{:?}",
            solidcore.lo, solidcore.hi, occt.lo, occt.hi
        ));
    }
    problems
}

fn same(name: &str, occt: &Measured, solidcore: &Measured) {
    let mut problems = measures_differ(occt, solidcore);
    if (occt.faces, occt.edges, occt.vertices)
        != (solidcore.faces, solidcore.edges, solidcore.vertices)
    {
        problems.push(format!(
            "faces, edges, vertices {:?} against {:?}",
            (solidcore.faces, solidcore.edges, solidcore.vertices),
            (occt.faces, occt.edges, occt.vertices)
        ));
    }
    if occt.names != solidcore.names {
        problems.push(format!(
            "face names\n  solidcore {:?}\n  occt      {:?}",
            solidcore.names, occt.names
        ));
    }
    assert!(problems.is_empty(), "{name}:\n{}", problems.join("\n"));
}

/// `make`'s part built with OpenCASCADE and with solidcore.
fn both(make: fn() -> Part) -> (Measured, Measured) {
    let mut occt_kernel = OcctKernel::new();
    let mut occt = make();
    occt.rebuild(&mut occt_kernel);
    let mut solid_kernel = SolidcoreKernel::new();
    let mut solid = make();
    solid.rebuild(&mut solid_kernel);
    (measure(&occt, &occt_kernel), measure(&solid, &solid_kernel))
}

/// Builds `make`'s part with both kernels and compares them.
fn compare(name: &str, make: fn() -> Part) {
    let (occt, solid) = both(make);
    same(name, &occt, &solid);
}

/// A known difference, pinned as it is today: solidcore makes a circle's extruded wall as two
/// rational-spline faces, OpenCASCADE as one cylinder. The measures still agree; the part has
/// one face, three edges and two vertices more; and the wall named `wall` (part 0) with
/// OpenCASCADE is parts 1 and 2 with solidcore. This fails the day solidcore changes it.
fn known_round_wall_difference(name: &str, make: fn() -> Part, wall: &str) {
    let (occt, solid) = both(make);
    let problems = measures_differ(&occt, &solid);
    assert!(
        problems.is_empty(),
        "{name}:
{}",
        problems.join(
            "
"
        )
    );
    assert_eq!(
        (occt.faces, occt.edges, occt.vertices),
        (7, 15, 10),
        "{name}: OpenCASCADE"
    );
    assert_eq!(
        (solid.faces, solid.edges, solid.vertices),
        (8, 18, 12),
        "{name}: solidcore"
    );
    let count = |m: &Measured, kind: &str| m.surfaces.iter().filter(|s| *s == kind).count();
    assert_eq!(
        (count(&occt, "Cylinder"), count(&occt, "BSpline")),
        (1, 0),
        "{name}"
    );
    assert_eq!(
        (count(&solid, "Cylinder"), count(&solid, "BSpline")),
        (0, 2),
        "{name}"
    );
    assert!(
        occt.names.iter().any(|n| n == wall),
        "{name}: {wall} in {:?}",
        occt.names
    );
    let mut expected: Vec<String> = occt
        .names
        .iter()
        .flat_map(|n| {
            if n == wall {
                vec![
                    n.replace("part: 0", "part: 1"),
                    n.replace("part: 0", "part: 2"),
                ]
            } else {
                vec![n.clone()]
            }
        })
        .collect();
    expected.sort();
    assert_eq!(solid.names, expected, "{name}");
}

fn sketch_on_front() -> Sketch {
    Sketch::new(SketchPlane::Standard(StandardPlane::Front))
}

/// A 40 × 20 × 10 block on the Front plane, from `x`.
fn add_block(part: &mut Part, x: f64, op: ExtrudeOp) {
    let mut sketch = sketch_on_front();
    sketch.rectangle(Point2::new(x, 0.0), 40.0, 20.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let mut extrude = Extrude::blind(s, op, 10.0);
    extrude.reverse = false;
    part.add_feature(FeatureKind::Extrude(extrude));
}

fn block() -> Part {
    let mut part = Part::new();
    add_block(&mut part, 0.0, ExtrudeOp::Boss);
    part
}

fn block_with_a_hole_in_its_sketch() -> Part {
    let mut part = Part::new();
    let mut sketch = sketch_on_front();
    sketch.rectangle(Point2::new(0.0, 0.0), 40.0, 20.0);
    sketch.circle(Point2::new(20.0, 10.0), 4.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        10.0,
    )));
    part
}

fn two_blocks_joined() -> Part {
    let mut part = Part::new();
    add_block(&mut part, 0.0, ExtrudeOp::Boss);
    add_block(&mut part, 20.0, ExtrudeOp::Boss);
    part
}

fn a_hole_cut_through_a_block() -> Part {
    let mut part = block();
    let mut sketch = sketch_on_front();
    sketch.circle(Point2::new(20.0, 10.0), 3.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let mut cut = Extrude::blind(s, ExtrudeOp::Cut, 10.0);
    cut.reverse = true;
    part.add_feature(FeatureKind::Extrude(cut));
    part
}

#[test]
fn an_extruded_block_matches_opencascade() {
    compare("block", block);
}

#[test]
fn a_block_with_a_hole_in_its_sketch_differs_only_by_its_round_wall() {
    known_round_wall_difference(
        "block with a hole",
        block_with_a_hole_in_its_sketch,
        "FaceName { key: Profile { feature: FeatureId(2), segment: 10 }, part: 0 }",
    );
}

#[test]
fn two_blocks_joined_match_opencascade() {
    compare("two blocks joined", two_blocks_joined);
}

#[test]
fn a_hole_cut_through_a_block_differs_only_by_its_round_wall() {
    known_round_wall_difference(
        "hole cut through a block",
        a_hole_cut_through_a_block,
        "FaceName { key: Profile { feature: FeatureId(4), segment: 2 }, part: 0 }",
    );
}

/// A notch cut down into the block's top: the top side face is split in two by the cut, so
/// its two pieces are named through the attribute's split rule, not by geometry.
fn a_notch_splitting_a_side_face() -> Part {
    let mut part = block();
    let mut sketch = sketch_on_front();
    sketch.rectangle(Point2::new(18.0, 12.0), 4.0, 12.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let mut cut = Extrude::blind(s, ExtrudeOp::Cut, 10.0);
    cut.reverse = true;
    part.add_feature(FeatureKind::Extrude(cut));
    part
}

#[test]
fn a_notch_splitting_a_side_face_matches_opencascade() {
    compare("notch splitting a side face", a_notch_splitting_a_side_face);
}

/// A box and a moved copy of it, straight through the kernels.
#[test]
fn a_box_and_its_moved_copy_match_opencascade() {
    let mut occt = OcctKernel::new();
    let mut solid = SolidcoreKernel::new();
    let corner = Point3::new(1.0, 2.0, 3.0);
    let size = Vec3::new(30.0, 20.0, 10.0);
    let a = occt.make_box(corner, size).unwrap();
    let b = solid.make_box(corner, size).unwrap();
    assert!((occt.volume(a).unwrap() - solid.volume(b).unwrap()).abs() < RELATIVE * 6000.0);
    assert_eq!(
        occt.topology_counts(a).unwrap(),
        solid.topology_counts(b).unwrap()
    );
    let mut moved = Transform::IDENTITY;
    moved.rows[0][3] = 10.0;
    let ma = occt.transform(a, &moved).unwrap();
    let mb = solid.transform(b, &moved).unwrap();
    let (lo, hi) = solid.bounds(mb.shape).unwrap();
    let (olo, ohi) = occt.bounds(ma.shape).unwrap();
    for (s, o) in [(lo, olo), (hi, ohi)] {
        assert!(
            (s.x - o.x).abs() < 1e-6 && (s.y - o.y).abs() < 1e-6 && (s.z - o.z).abs() < 1e-6,
            "{s:?} against {o:?}"
        );
    }
    // Every face of the copy comes from one face of the box, as with OpenCASCADE.
    let from = |h: &oxyde_kernel::OpHistory| {
        let mut v: Vec<Vec<FaceSource>> = h.faces.clone();
        v.sort_by_key(|s| format!("{s:?}"));
        v
    };
    assert_eq!(from(&mb.history), from(&ma.history));
    assert!(mb.history.faces.iter().all(|s| s.len() == 1));
}

/// The same part rebuilt ten times gives the same topology and the same names.
#[test]
fn solidcore_rebuilds_give_the_same_names() {
    let first = {
        let mut kernel = SolidcoreKernel::new();
        let mut part = two_blocks_joined();
        part.rebuild(&mut kernel);
        measure(&part, &kernel)
    };
    for _ in 0..9 {
        let mut kernel = SolidcoreKernel::new();
        let mut part = two_blocks_joined();
        part.rebuild(&mut kernel);
        let again = measure(&part, &kernel);
        assert_eq!(again.names, first.names);
        assert_eq!(
            (again.faces, again.edges, again.vertices),
            (first.faces, first.edges, first.vertices)
        );
    }
}

/// A known difference: solidcore builds an ellipse's wall from its three exact conic
/// pieces, OpenCASCADE as one face. The measures agree, and the wall named `wall` (part 0)
/// with OpenCASCADE is parts 1, 2 and 3 with solidcore.
///
/// With `exact_area`, solidcore's area is held to it instead of to OpenCASCADE's: the face
/// areas OpenCASCADE's topology reports for a long, thin ellipse's wall are off by about
/// 2e-4 of the whole, more than the comparison allows.
fn known_ellipse_wall_difference(
    name: &str,
    make: fn() -> Part,
    wall: &str,
    exact_area: Option<f64>,
) {
    let (occt, solid) = both(make);
    let mut problems = measures_differ(&occt, &solid);
    if let Some(exact) = exact_area {
        problems.retain(|p| !p.starts_with("area"));
        assert!(
            (solid.area - exact).abs() <= RELATIVE * exact,
            "{name}: area {} against exactly {exact}",
            solid.area
        );
    }
    assert!(problems.is_empty(), "{name}: {}", problems.join("; "));
    assert_eq!(solid.faces, occt.faces + 2, "{name}");
    assert!(
        occt.names.iter().any(|n| n == wall),
        "{name}: {wall} in {:?}",
        occt.names
    );
    let mut expected: Vec<String> = occt
        .names
        .iter()
        .flat_map(|n| {
            if n == wall {
                (1..=3)
                    .map(|k| n.replace("part: 0", &format!("part: {k}")))
                    .collect()
            } else {
                vec![n.clone()]
            }
        })
        .collect();
    expected.sort();
    assert_eq!(solid.names, expected, "{name}");
}

fn an_extruded_ellipse() -> Part {
    let mut part = Part::new();
    let mut sketch = sketch_on_front();
    sketch
        .geometry
        .ellipse(Point2::new(20.0, 10.0), Point2::new(28.0, 13.0), 3.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        10.0,
    )));
    part
}

fn an_elliptic_hole_cut_through_a_block() -> Part {
    let mut part = block();
    let mut sketch = sketch_on_front();
    sketch
        .geometry
        .ellipse(Point2::new(20.0, 10.0), Point2::new(26.0, 10.0), 4.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let mut cut = Extrude::blind(s, ExtrudeOp::Cut, 10.0);
    cut.reverse = true;
    part.add_feature(FeatureKind::Extrude(cut));
    part
}

#[test]
fn an_extruded_ellipse_matches_opencascade_but_for_its_wall_in_three_pieces() {
    // Two caps of π·a·b and a wall of the perimeter (a periodic integral, which the
    // midpoint rule gives to rounding) times the depth.
    let (a, b) = (73.0_f64.sqrt(), 3.0);
    let steps = 4096;
    let perimeter: f64 = (0..steps)
        .map(|k| {
            let t = std::f64::consts::TAU * (k as f64 + 0.5) / steps as f64;
            (a * t.sin()).hypot(b * t.cos())
        })
        .sum::<f64>()
        * std::f64::consts::TAU
        / steps as f64;
    let area = 2.0 * std::f64::consts::PI * a * b + perimeter * 10.0;
    known_ellipse_wall_difference(
        "extruded ellipse",
        an_extruded_ellipse,
        "FaceName { key: Profile { feature: FeatureId(2), segment: 3 }, part: 0 }",
        Some(area),
    );
}

#[test]
fn an_elliptic_hole_matches_opencascade_but_for_its_wall_in_three_pieces() {
    known_ellipse_wall_difference(
        "elliptic hole",
        an_elliptic_hole_cut_through_a_block,
        "FaceName { key: Profile { feature: FeatureId(4), segment: 3 }, part: 0 }",
        None,
    );
}

/// How far a point of an edge solidcore builds from an ellipse may lie from the exact
/// ellipse: rounding only, since its conic pieces are the ellipse itself.
const ON_ELLIPSE: f64 = 1e-9;

#[test]
fn the_edges_solidcore_builds_from_an_ellipse_lie_on_it() {
    let ellipse = oxyde_geom::Ellipse2 {
        center: Point2::new(20.0, 10.0),
        major: Point2::new(28.0, 13.0),
        minor: 3.0,
    };
    let mut kernel = SolidcoreKernel::new();
    let mut part = an_extruded_ellipse();
    part.rebuild(&mut kernel);
    let body = part.body().expect("a body");
    let topology = &body.topology;
    let mut checked = 0;
    for edge in &topology.edges {
        let ends: Vec<Point3> = edge
            .vertices
            .iter()
            .filter_map(|v| topology.vertices.get(*v as usize).map(|v| v.point))
            .collect();
        // The cap edges, at either end of the extrusion along Z.
        let z = edge.midpoint.z;
        if !(z.abs() < ON_ELLIPSE || (z - 10.0).abs() < ON_ELLIPSE) {
            continue;
        }
        for p in ends.iter().chain([&edge.midpoint]) {
            let q = Point2::new(p.x, p.y);
            let off = ellipse.at(ellipse.angle_of(q)).distance(q);
            assert!(off < ON_ELLIPSE, "{p:?} is {off} off the ellipse");
        }
        checked += 1;
    }
    // Three pieces at each end.
    assert_eq!(checked, 6);
}
