//! Rebuilds are deterministic (ARCHITECTURE.md): the same timeline gives the same
//! topology in the same order, the same persistent names and the same operation histories
//! on every rebuild. The booleans in the shim run on every core (`SetRunParallel` in
//! `shim.cpp`), so the parts here are built from several booleans and rebuilt ten times,
//! each in a fresh kernel, and every run is compared with the first, item by item.
//!
//! To compare two builds of the kernel (a flag changed in the shim, a new OpenCASCADE),
//! set `OXYDE_DETERMINISM_DUMP` to a directory: the first run of each test is written
//! there as text, and the files of the two builds can be diffed.

use std::ops::Range;

use oxyde_geom::{Plane, Point2, Point3, Transform, Vec3};
use oxyde_kernel::{BooleanOp, Kernel, OpHistory, Topology};
use oxyde_model::{
    AxisRef, EdgeBlend, EdgeName, Extrude, ExtrudeOp, FaceName, FeatureId, FeatureKind,
    FeatureStatus, FileData, Import, LinearPattern, Part, PartDefinition, PatternDirection, Sketch,
    SketchPlane, StandardAxis, StandardPlane, VertexName,
};
use oxyde_occt::OcctKernel;

const CART: &[u8] = include_bytes!("data/cart.step");

/// Rebuilds per part. Parallel work that reorders anything shows within a few runs.
const RUNS: usize = 10;

/// Everything a rebuild produces that must not change from one rebuild to the next: the
/// body's topology in its order, the persistent names of its faces, edges and vertices,
/// and which solid each belongs to.
#[derive(Debug, Clone, PartialEq)]
struct BodySnapshot {
    topology: Topology,
    faces: Vec<FaceName>,
    edges: Vec<EdgeName>,
    vertices: Vec<VertexName>,
    solids: Vec<[Range<u32>; 3]>,
}

fn snapshot(part: &Part) -> BodySnapshot {
    let body = part.body().expect("the part has a body");
    BodySnapshot {
        topology: body.topology.clone(),
        faces: body.naming.faces().to_vec(),
        edges: body.naming.edges().to_vec(),
        vertices: body.naming.vertices().to_vec(),
        solids: body
            .solids
            .iter()
            .map(|s| [s.faces.clone(), s.edges.clone(), s.vertices.clone()])
            .collect(),
    }
}

/// Writes `value` as text to `OXYDE_DETERMINISM_DUMP/<name>.txt` when that variable is
/// set, for comparing two builds of the kernel.
fn dump(name: &str, value: &impl std::fmt::Debug) {
    if let Some(dir) = std::env::var_os("OXYDE_DETERMINISM_DUMP") {
        let path = std::path::Path::new(&dir).join(format!("{name}.txt"));
        std::fs::write(&path, format!("{value:#?}")).expect("the dump is written");
    }
}

fn assert_all_ok(part: &Part) {
    for entry in part.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{}", entry.name);
    }
}

/// Compares two lists element by element, so a failure names the first item that differs
/// rather than printing two whole topologies.
fn assert_same<T: PartialEq + std::fmt::Debug>(what: &str, run: usize, first: &[T], again: &[T]) {
    assert_eq!(
        first.len(),
        again.len(),
        "{what}: {} on the first run, {} on run {run}",
        first.len(),
        again.len()
    );
    for (i, (a, b)) in first.iter().zip(again).enumerate() {
        assert_eq!(
            a, b,
            "{what} {i} differs between the first run and run {run}"
        );
    }
}

fn assert_same_body(run: usize, first: &BodySnapshot, again: &BodySnapshot) {
    assert_same("solid", run, &first.solids, &again.solids);
    assert_same("face", run, &first.topology.faces, &again.topology.faces);
    assert_same("edge", run, &first.topology.edges, &again.topology.edges);
    assert_same(
        "vertex",
        run,
        &first.topology.vertices,
        &again.topology.vertices,
    );
    assert_same("face name", run, &first.faces, &again.faces);
    assert_same("edge name", run, &first.edges, &again.edges);
    assert_same("vertex name", run, &first.vertices, &again.vertices);
}

/// A fresh kernel and the part rebuilt in it from `definition`, every feature ok.
fn rebuild(definition: &PartDefinition) -> (OcctKernel, Part) {
    let mut kernel = OcctKernel::new();
    let mut part = Part::from_definition(definition.clone()).expect("a valid definition");
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    (kernel, part)
}

/// Rebuilds `definition` [`RUNS`] times, each in a new kernel, and checks every rebuild
/// against the first.
fn assert_rebuilds_identical(name: &str, definition: &PartDefinition) {
    let (mut kernel, mut part) = rebuild(definition);
    let first = snapshot(&part);
    part.release(&mut kernel);
    dump(name, &first);
    assert!(!first.faces.is_empty(), "the part has faces");
    for run in 1..RUNS {
        let (mut kernel, mut part) = rebuild(definition);
        let again = snapshot(&part);
        part.release(&mut kernel);
        assert_same_body(run, &first, &again);
    }
}

/// A `width` × `height` × 10 block on the Front plane from the origin, along +Z.
fn block(part: &mut Part, width: f64, height: f64) -> FeatureId {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::ORIGIN, width, height);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        10.0,
    )))
}

/// A through cut of `sketch`, drawn on the top face of a block, down through it.
fn cut(sketch: FeatureId) -> FeatureKind {
    FeatureKind::Extrude(Extrude::blind(sketch, ExtrudeOp::Cut, 10.0))
}

/// A 100 × 60 plate with a row of five holes cut in one operation, then one more hole
/// copied by a 4 × 3 pattern: a boolean with many tools, and the pattern's.
fn plate_with_holes() -> PartDefinition {
    let mut part = Part::new();
    let base = block(&mut part, 100.0, 60.0);
    let mut row = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    for i in 0..5 {
        row.circle(Point2::new(15.0 + 16.0 * f64::from(i), 50.0), 2.5);
    }
    let s = part.add_feature(FeatureKind::Sketch(row));
    part.add_feature(cut(s));
    let mut one = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    one.circle(Point2::new(10.0, 10.0), 3.0);
    let s = part.add_feature(FeatureKind::Sketch(one));
    let hole = part.add_feature(cut(s));
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
            spacing: 12.0,
            count: 3,
            reverse: false,
        }),
    }));
    part.definition()
}

/// A block with a rectangular pocket cut through it, its four corners filleted, and the
/// rim of the pocket filleted after that: fillets on edges a boolean made.
fn pocket_with_fillets() -> PartDefinition {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let base = block(&mut part, 60.0, 40.0);
    let mut pocket = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    let walls = pocket.rectangle(Point2::new(15.0, 10.0), 30.0, 20.0);
    let s = part.add_feature(FeatureKind::Sketch(pocket));
    let hole = part.add_feature(cut(s));
    // The pocket's corners: the edges between two consecutive walls.
    let corners = (0..4)
        .map(|i| {
            EdgeName::between(
                FaceName::profile(hole, walls[i]),
                FaceName::profile(hole, walls[(i + 1) % 4]),
            )
        })
        .collect();
    part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: corners,
        size: 3.0,
        chamfer: Default::default(),
    }));
    // The rim of the pocket on the top face: picked by name after a rebuild, since the
    // corner fillets changed the edges there.
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let top = FaceName::end_cap(base);
    let rim: Vec<EdgeName> = part
        .body()
        .expect("a body")
        .naming
        .edges()
        .iter()
        .filter(|e| e.faces.contains(&top) && e.faces.iter().any(|f| f.key.feature() != base))
        .cloned()
        .collect();
    assert_eq!(
        rim.len(),
        8,
        "four walls and four corner fillets meet the top"
    );
    part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: rim,
        size: 1.0,
        chamfer: Default::default(),
    }));
    part.release(&mut kernel);
    part.definition()
}

#[test]
fn a_plate_with_a_pattern_of_holes_rebuilds_the_same_every_time() {
    assert_rebuilds_identical("plate_with_holes", &plate_with_holes());
}

#[test]
fn fillets_after_a_cut_rebuild_the_same_every_time() {
    assert_rebuilds_identical("pocket_with_fillets", &pocket_with_fillets());
}

/// The result of a boolean asked of the kernel directly: its topology and its history.
#[derive(Debug, Clone, PartialEq)]
struct CutSnapshot {
    topology: Topology,
    history: OpHistory,
}

/// As `plane_transform` in the app's section view: the plane's frame to the world.
fn plane_transform(p: &Plane) -> Transform {
    let y = p.y_dir();
    Transform {
        rows: [
            [p.x_dir.x, y.x, p.normal.x, p.origin.x],
            [p.x_dir.y, y.y, p.normal.y, p.origin.y],
            [p.x_dir.z, y.z, p.normal.z, p.origin.z],
        ],
    }
}

/// Cuts every solid of the imported cart by a half-space through the middle of the model
/// on each standard plane, as Evaluate › Section View does, and returns the topology and
/// history of every cut, in order.
fn section_cuts() -> Vec<CutSnapshot> {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    part.add_feature(FeatureKind::Import(Import::new(
        "cart.step",
        FileData::new(CART.to_vec()),
    )));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let body = part.body().expect("the cart has a body").clone();
    assert!(!body.solids.is_empty(), "the cart has a solid");

    let mut lo = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut hi = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for s in &body.solids {
        let (a, b) = kernel.bounds(s.shape).expect("bounds");
        lo = Point3::new(lo.x.min(a.x), lo.y.min(a.y), lo.z.min(a.z));
        hi = Point3::new(hi.x.max(b.x), hi.y.max(b.y), hi.z.max(b.z));
    }
    let centre = Point3::new(
        (lo.x + hi.x) / 2.0,
        (lo.y + hi.y) / 2.0,
        (lo.z + hi.z) / 2.0,
    );

    let mut out = Vec::new();
    for standard in [
        StandardPlane::Front,
        StandardPlane::Top,
        StandardPlane::Right,
    ] {
        let base = standard.plane();
        let plane = base.offset((centre - base.origin).dot(base.normal));
        let reach = (hi - lo).length() + 1.0;
        let raw = kernel
            .make_box(
                Point3::new(-reach, -reach, 0.0),
                Vec3::new(2.0 * reach, 2.0 * reach, reach),
            )
            .expect("the half-space box");
        let tool = kernel
            .transform(raw, &plane_transform(&plane))
            .expect("the half-space placed")
            .shape;
        kernel.release(raw);
        for solid in &body.solids {
            let result = kernel
                .boolean(BooleanOp::Cut, solid.shape, tool)
                .expect("the section cut");
            let topology = kernel.topology(result.shape).expect("topology");
            kernel.release(result.shape);
            out.push(CutSnapshot {
                topology,
                history: result.history,
            });
        }
        kernel.release(tool);
    }
    part.release(&mut kernel);
    out
}

#[test]
fn section_cuts_of_the_step_cart_are_the_same_every_time() {
    let first = section_cuts();
    dump("section_cuts", &first);
    assert!(
        first.iter().any(|c| !c.topology.faces.is_empty()),
        "some cut keeps something"
    );
    for run in 1..RUNS {
        let again = section_cuts();
        assert_eq!(first.len(), again.len(), "number of cuts on run {run}");
        for (i, (a, b)) in first.iter().zip(&again).enumerate() {
            let what = format!("cut {i}");
            assert_same(
                &format!("{what} face"),
                run,
                &a.topology.faces,
                &b.topology.faces,
            );
            assert_same(
                &format!("{what} edge"),
                run,
                &a.topology.edges,
                &b.topology.edges,
            );
            assert_same(
                &format!("{what} vertex"),
                run,
                &a.topology.vertices,
                &b.topology.vertices,
            );
            assert_same(
                &format!("{what} history of face"),
                run,
                &a.history.faces,
                &b.history.faces,
            );
        }
    }
}
