//! What the native kernel builds so far: a corpus of small parts, every feature type among
//! them, rebuilt by OpenCASCADE and by the native kernel, feature by feature. The report
//! says which features the native kernel builds, which it cannot yet and why, whether what
//! it builds is what OpenCASCADE builds, and how long each kernel takes to rebuild and mesh
//! each part (docs/ARCHITECTURE.md §10, Native kernel).
//!
//! The report is printed (`cargo test -p oxyde-tests --test kernel_coverage -- --nocapture`)
//! and written to `kernel-coverage.md` in Cargo's directory for test files. Set
//! `OXYDE_CORPUS` to a folder to add every `.oxyprt` file in it to the corpus.
//!
//! The test fails when OpenCASCADE cannot build a part of the built-in corpus (the corpus
//! is wrong), when a part the native kernel builds whole is not what OpenCASCADE builds,
//! or when a part of [`NATIVE_BUILDS`] no longer builds with the native kernel. A part
//! that newly builds is reported, to be added to the list.

use std::f64::consts::{PI, TAU};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use oxyde_geom::{Point2, Point3};
use oxyde_kernel::{Kernel, TopologyCounts};
use oxyde_model::{
    AxisRef, BodyId, CircularPattern, Combine, CombineOp, EdgeBlend, EdgeName, Extrude,
    ExtrudeExtent, ExtrudeOp, FaceName, FeatureId, FeatureKind, FeatureStatus, FileData, Hole,
    HoleKind, Import, LinearPattern, Loft, Mirror, Part, PartDefinition, PatternDirection,
    PlaneRef, RefPlane, Revolve, RevolveAxis, Shell, Sketch, SketchPlane, StandardAxis,
    StandardPlane, Sweep,
};
use oxyde_native_kernel::NativeKernel;
use oxyde_occt::OcctKernel;

/// Parts of the built-in corpus the native kernel builds whole, as OpenCASCADE does.
const NATIVE_BUILDS: &[&str] = &[
    "block",
    "pocket",
    "slot cut through",
    "round boss on a face",
    "plate with round holes",
    "pattern of holes",
    "mirrored boss",
    "two bodies combined",
    "flange (demo)",
    "revolved ring",
    "revolved quarter ring",
    "revolved rod",
    "revolved cone",
    "revolved ball",
    "revolved torus",
    "revolved groove",
    "simple holes",
    "blind holes",
    "counterbored holes",
    "countersunk holes",
    "crossing holes",
    "radial hole",
    "bar cut at a slant",
    "hole through a ball",
    "bracket (demo)",
    "fillet",
    "chamfer",
    "shell",
    "sweep along an arc",
    "loft",
    "STEP import",
];

// ---------------------------------------------------------------------------------------
// The corpus.

/// Id of the right line of the first rectangle of a sketch (corners 1–4, then the bottom,
/// right, top and left lines 5–8).
const RIGHT: u32 = 6;

fn sketch_on(plane: StandardPlane) -> Sketch {
    Sketch::new(SketchPlane::Standard(plane))
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

fn blind(sketch: FeatureId, op: ExtrudeOp, depth: f64) -> FeatureKind {
    FeatureKind::Extrude(Extrude::blind(sketch, op, depth))
}

/// A 40 × 20 × 10 block on the Front plane, from the origin.
fn block(part: &mut Part) -> FeatureId {
    let mut s = sketch_on(StandardPlane::Front);
    s.rectangle(Point2::ORIGIN, 40.0, 20.0);
    let s = part.add_feature(FeatureKind::Sketch(s));
    part.add_feature(blind(s, ExtrudeOp::Boss, 10.0))
}

/// A sketch on the block's top face (z = 10, x and y those of the model).
fn on_top(base: FeatureId) -> Sketch {
    Sketch::new(SketchPlane::Face(FaceName::end_cap(base)))
}

fn revolve(sketch: FeatureId, op: ExtrudeOp, axis: u32, angle: f64) -> FeatureKind {
    FeatureKind::Revolve(Revolve {
        sketch,
        op,
        axis: RevolveAxis::SketchLine(axis),
        angle,
        mid_plane: false,
        reverse: false,
        bodies: Vec::new(),
    })
}

/// A sketch on the Front plane with a construction line along the Y axis, and the line's
/// id: what the revolves turn about.
fn about_y() -> (Sketch, u32) {
    let mut s = sketch_on(StandardPlane::Front);
    let axis = s
        .geometry
        .line(Point2::new(0.0, -20.0), Point2::new(0.0, 40.0));
    s.geometry.set_construction(axis, true);
    (s, axis.0)
}

/// A closed polygon of lines through `corners`.
fn polygon(sketch: &mut Sketch, corners: &[(f64, f64)]) {
    let points: Vec<_> = corners
        .iter()
        .map(|&(x, y)| sketch.geometry.point(Point2::new(x, y)))
        .collect();
    for k in 0..points.len() {
        sketch
            .geometry
            .line_between(points[k], points[(k + 1) % points.len()]);
    }
}

fn revolved(corners: &[(f64, f64)], angle: f64) -> Part {
    let mut part = Part::new();
    let (mut s, axis) = about_y();
    polygon(&mut s, corners);
    let s = part.add_feature(FeatureKind::Sketch(s));
    part.add_feature(revolve(s, ExtrudeOp::Boss, axis, angle));
    part
}

fn holed_block(kind: HoleKind, depth: Option<f64>) -> Part {
    let mut part = Part::new();
    let base = block(&mut part);
    let mut spots = on_top(base);
    spots.geometry.point(Point2::new(10.0, 10.0));
    spots.geometry.point(Point2::new(30.0, 10.0));
    let spots = part.add_feature(FeatureKind::Sketch(spots));
    part.add_feature(FeatureKind::Hole(Hole {
        sketch: spots,
        kind,
        diameter: 6.0,
        depth,
        reverse: false,
        bodies: Vec::new(),
    }));
    part
}

fn blended(fillet: bool) -> Part {
    let mut part = Part::new();
    let base = block(&mut part);
    let blend = EdgeBlend {
        edges: vec![EdgeName::between(
            FaceName::profile(base, RIGHT),
            FaceName::end_cap(base),
        )],
        size: 3.0,
        chamfer: Default::default(),
    };
    part.add_feature(if fillet {
        FeatureKind::Fillet(blend)
    } else {
        FeatureKind::Chamfer(blend)
    });
    part
}

/// The demo bracket of `oxyde-app` (`demo.rs`): extrusions, holes, a fillet, a chamfer.
fn bracket() -> Part {
    let mut part = Part::new();
    let mut plate = sketch_on(StandardPlane::Front);
    plate.rectangle(Point2::new(-40.0, -25.0), 80.0, 50.0);
    let plate = part.add_feature(FeatureKind::Sketch(plate));
    let base = part.add_feature(blind(plate, ExtrudeOp::Boss, 10.0));
    let mut upright = on_top(base);
    upright.rectangle(Point2::new(-40.0, -25.0), 15.0, 50.0);
    let upright = part.add_feature(FeatureKind::Sketch(upright));
    let wall = part.add_feature(blind(upright, ExtrudeOp::Boss, 30.0));
    let mut holes = on_top(base);
    holes.circle(Point2::new(15.0, 12.0), 4.0);
    holes.circle(Point2::new(15.0, -12.0), 4.0);
    let holes = part.add_feature(FeatureKind::Sketch(holes));
    part.add_feature(blind(holes, ExtrudeOp::Cut, 12.0));
    part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: vec![EdgeName::between(
            FaceName::profile(wall, RIGHT),
            FaceName::end_cap(base),
        )],
        size: 4.0,
        chamfer: Default::default(),
    }));
    part.add_feature(FeatureKind::Chamfer(EdgeBlend {
        edges: vec![EdgeName::between(
            FaceName::profile(wall, RIGHT),
            FaceName::end_cap(wall),
        )],
        size: 2.0,
        chamfer: Default::default(),
    }));
    part
}

/// The demo flange of `oxyde-app` (`demo.rs`): a revolve and a circular pattern of holes.
fn flange() -> Part {
    let mut part = Part::new();
    let (mut section, axis) = about_y();
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
    let section = part.add_feature(FeatureKind::Sketch(section));
    let body = part.add_feature(revolve(section, ExtrudeOp::Boss, axis, TAU));
    let mut hole = Sketch::new(SketchPlane::Face(FaceName::profile(body, lines[2].0)));
    hole.circle(Point2::new(28.0, 0.0), 3.0);
    let hole = part.add_feature(FeatureKind::Sketch(hole));
    let hole = part.add_feature(blind(hole, ExtrudeOp::Cut, 6.0));
    part.add_feature(FeatureKind::CircularPattern(CircularPattern {
        seeds: vec![hole],
        axis: AxisRef::Standard(StandardAxis::Y),
        angle: TAU,
        count: 6,
        equal_spacing: true,
        reverse: false,
        skip: Vec::new(),
    }));
    part
}

/// Every part of the built-in corpus, by name.
fn corpus() -> Vec<(String, Part)> {
    let mut parts: Vec<(&str, Part)> = Vec::new();

    let mut part = Part::new();
    block(&mut part);
    parts.push(("block", part));

    let mut part = Part::new();
    let base = block(&mut part);
    let mut s = on_top(base);
    s.rectangle(Point2::new(5.0, 5.0), 10.0, 6.0);
    let s = part.add_feature(FeatureKind::Sketch(s));
    part.add_feature(blind(s, ExtrudeOp::Cut, 4.0));
    parts.push(("pocket", part));

    let mut part = Part::new();
    let base = block(&mut part);
    let mut s = on_top(base);
    let g = &mut s.geometry;
    let (a, b) = (
        g.point(Point2::new(12.0, 7.0)),
        g.point(Point2::new(28.0, 7.0)),
    );
    let (c, d) = (
        g.point(Point2::new(28.0, 13.0)),
        g.point(Point2::new(12.0, 13.0)),
    );
    let (right, left) = (
        g.point(Point2::new(28.0, 10.0)),
        g.point(Point2::new(12.0, 10.0)),
    );
    g.line_between(a, b);
    g.arc_between(right, b, c);
    g.line_between(c, d);
    g.arc_between(left, d, a);
    let s = part.add_feature(FeatureKind::Sketch(s));
    part.add_feature(extrude(
        s,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: false },
    ));
    parts.push(("slot cut through", part));

    let mut part = Part::new();
    let base = block(&mut part);
    let mut s = on_top(base);
    s.circle(Point2::new(20.0, 10.0), 6.0);
    let s = part.add_feature(FeatureKind::Sketch(s));
    part.add_feature(blind(s, ExtrudeOp::Boss, 8.0));
    parts.push(("round boss on a face", part));

    let mut part = Part::new();
    let mut s = sketch_on(StandardPlane::Front);
    s.rectangle(Point2::ORIGIN, 60.0, 30.0);
    s.circle(Point2::new(15.0, 15.0), 5.0);
    s.circle(Point2::new(45.0, 15.0), 5.0);
    let s = part.add_feature(FeatureKind::Sketch(s));
    part.add_feature(blind(s, ExtrudeOp::Boss, 4.0));
    parts.push(("plate with round holes", part));

    let mut part = Part::new();
    let base = block(&mut part);
    let mut s = on_top(base);
    s.circle(Point2::new(6.0, 10.0), 2.5);
    let s = part.add_feature(FeatureKind::Sketch(s));
    let hole = part.add_feature(extrude(
        s,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: false },
    ));
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
    parts.push(("pattern of holes", part));

    let mut part = Part::new();
    let base = block(&mut part);
    let mut s = on_top(base);
    s.rectangle(Point2::new(28.0, 4.0), 8.0, 12.0);
    let s = part.add_feature(FeatureKind::Sketch(s));
    let boss = part.add_feature(blind(s, ExtrudeOp::Boss, 5.0));
    let mid = part.add_feature(FeatureKind::Plane(RefPlane::Offset {
        base: PlaneRef::Standard(StandardPlane::Right),
        offset: 20.0,
        flip: false,
    }));
    part.add_feature(FeatureKind::Mirror(Mirror {
        seeds: vec![boss],
        plane: PlaneRef::Reference(mid),
        bodies: Vec::new(),
    }));
    parts.push(("mirrored boss", part));

    let mut part = Part::new();
    let first = block(&mut part);
    let mut s = sketch_on(StandardPlane::Front);
    s.rectangle(Point2::new(30.0, 5.0), 20.0, 10.0);
    let s = part.add_feature(FeatureKind::Sketch(s));
    let second = part.add_feature(blind(s, ExtrudeOp::NewBody, 6.0));
    part.add_feature(FeatureKind::Combine(Combine {
        target: BodyId::new(first, 0),
        tools: vec![BodyId::new(second, 0)],
        op: CombineOp::Join,
        keep_tools: false,
    }));
    parts.push(("two bodies combined", part));

    let mut part = Part::new();
    let base = block(&mut part);
    let mut s = on_top(base);
    s.circle(Point2::new(10.0, 10.0), 3.0);
    let s = part.add_feature(FeatureKind::Sketch(s));
    part.add_feature(extrude(
        s,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: false },
    ));
    let mut side = sketch_on(StandardPlane::Right);
    // The Right plane's x runs along −Z: the hole crosses the first one at mid-height.
    side.circle(Point2::new(-5.0, 10.0), 2.0);
    let side = part.add_feature(FeatureKind::Sketch(side));
    part.add_feature(extrude(
        side,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
    ));
    parts.push(("crossing holes", part));

    let mut part = Part::new();
    let mut bar = sketch_on(StandardPlane::Front);
    bar.circle(Point2::ORIGIN, 10.0);
    let bar = part.add_feature(FeatureKind::Sketch(bar));
    part.add_feature(blind(bar, ExtrudeOp::Boss, 30.0));
    let mut hole = sketch_on(StandardPlane::Right);
    hole.circle(Point2::new(-15.0, 0.0), 3.0);
    let hole = part.add_feature(FeatureKind::Sketch(hole));
    part.add_feature(extrude(
        hole,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
    ));
    parts.push(("radial hole", part));

    let mut part = Part::new();
    let mut bar = sketch_on(StandardPlane::Front);
    bar.circle(Point2::ORIGIN, 5.0);
    let bar = part.add_feature(FeatureKind::Sketch(bar));
    part.add_feature(blind(bar, ExtrudeOp::Boss, 30.0));
    let mut wedge = sketch_on(StandardPlane::Right);
    // The Right plane's x runs along -Z: a wedge above the line from z = 30 to z = 20.
    polygon(&mut wedge, &[(-30.0, -6.0), (-20.0, 6.0), (-40.0, 46.0)]);
    let wedge = part.add_feature(FeatureKind::Sketch(wedge));
    part.add_feature(extrude(
        wedge,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
    ));
    parts.push(("bar cut at a slant", part));

    parts.push(("bracket (demo)", bracket()));
    parts.push(("flange (demo)", flange()));
    parts.push((
        "revolved ring",
        revolved(&[(10.0, 0.0), (20.0, 0.0), (20.0, 5.0), (10.0, 5.0)], TAU),
    ));
    parts.push((
        "revolved quarter ring",
        revolved(
            &[(10.0, 0.0), (20.0, 0.0), (20.0, 5.0), (10.0, 5.0)],
            TAU / 4.0,
        ),
    ));
    parts.push((
        "revolved rod",
        revolved(&[(0.0, 0.0), (5.0, 0.0), (5.0, 10.0), (0.0, 10.0)], TAU),
    ));
    parts.push((
        "revolved cone",
        revolved(&[(0.0, 0.0), (5.0, 0.0), (0.0, 10.0)], TAU),
    ));

    let mut part = Part::new();
    let (mut s, axis) = about_y();
    let g = &mut s.geometry;
    let (south, north, centre) = (
        g.point(Point2::new(0.0, -5.0)),
        g.point(Point2::new(0.0, 5.0)),
        g.point(Point2::ORIGIN),
    );
    g.arc_between(centre, south, north);
    g.line_between(north, south);
    let s = part.add_feature(FeatureKind::Sketch(s));
    part.add_feature(revolve(s, ExtrudeOp::Boss, axis, TAU));
    parts.push((
        "revolved ball",
        Part::from_definition(part.definition()).expect("a valid part"),
    ));

    let mut hole = sketch_on(StandardPlane::Right);
    hole.circle(Point2::new(-1.0, 1.0), 2.0);
    let hole = part.add_feature(FeatureKind::Sketch(hole));
    part.add_feature(extrude(
        hole,
        ExtrudeOp::Cut,
        ExtrudeExtent::ThroughAll { both: true },
    ));
    parts.push(("hole through a ball", part));

    let mut part = Part::new();
    let (mut s, axis) = about_y();
    s.circle(Point2::new(10.0, 0.0), 3.0);
    let s = part.add_feature(FeatureKind::Sketch(s));
    part.add_feature(revolve(s, ExtrudeOp::Boss, axis, TAU));
    parts.push(("revolved torus", part));

    let mut part = Part::new();
    let (mut s, axis) = about_y();
    polygon(
        &mut s,
        &[(0.0, 0.0), (20.0, 0.0), (20.0, 30.0), (0.0, 30.0)],
    );
    let s = part.add_feature(FeatureKind::Sketch(s));
    part.add_feature(revolve(s, ExtrudeOp::Boss, axis, TAU));
    let (mut groove, axis) = about_y();
    polygon(
        &mut groove,
        &[(17.0, 12.0), (25.0, 12.0), (25.0, 18.0), (17.0, 18.0)],
    );
    let groove = part.add_feature(FeatureKind::Sketch(groove));
    part.add_feature(revolve(groove, ExtrudeOp::Cut, axis, TAU));
    parts.push(("revolved groove", part));

    parts.push(("simple holes", holed_block(HoleKind::Simple, None)));
    parts.push(("blind holes", holed_block(HoleKind::Simple, Some(6.0))));
    parts.push((
        "counterbored holes",
        holed_block(
            HoleKind::Counterbore {
                diameter: 10.0,
                depth: 3.0,
            },
            None,
        ),
    ));
    parts.push((
        "countersunk holes",
        holed_block(
            HoleKind::Countersink {
                diameter: 10.0,
                angle: PI / 2.0,
            },
            None,
        ),
    ));
    parts.push(("fillet", blended(true)));
    parts.push(("chamfer", blended(false)));

    let mut part = Part::new();
    let base = block(&mut part);
    part.add_feature(FeatureKind::Shell(Shell {
        faces: vec![FaceName::end_cap(base)],
        thickness: 2.0,
        outward: false,
    }));
    parts.push(("shell", part));

    let mut part = Part::new();
    let mut profile = sketch_on(StandardPlane::Front);
    profile.circle(Point2::new(20.0, 0.0), 3.0);
    let profile = part.add_feature(FeatureKind::Sketch(profile));
    let mut path = sketch_on(StandardPlane::Top);
    path.geometry.arc(
        Point2::new(0.0, 0.0),
        Point2::new(20.0, 0.0),
        Point2::new(0.0, 20.0),
    );
    let path = part.add_feature(FeatureKind::Sketch(path));
    part.add_feature(FeatureKind::Sweep(Sweep {
        profile,
        path,
        op: ExtrudeOp::Boss,
        bodies: Vec::new(),
    }));
    parts.push(("sweep along an arc", part));

    let mut part = Part::new();
    let mut round = sketch_on(StandardPlane::Front);
    round.circle(Point2::ORIGIN, 8.0);
    let round = part.add_feature(FeatureKind::Sketch(round));
    let up = part.add_feature(FeatureKind::Plane(RefPlane::Offset {
        base: PlaneRef::Standard(StandardPlane::Front),
        offset: 15.0,
        flip: false,
    }));
    let mut square = Sketch::new(SketchPlane::Reference(up));
    square.rectangle(Point2::new(-6.0, -6.0), 12.0, 12.0);
    let square = part.add_feature(FeatureKind::Sketch(square));
    part.add_feature(FeatureKind::Loft(Loft {
        profiles: vec![round, square],
        op: ExtrudeOp::NewBody,
        ruled: false,
        bodies: Vec::new(),
    }));
    parts.push(("loft", part));

    let mut part = Part::new();
    let step = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/cart.step"))
        .expect("the cart's STEP file");
    part.add_feature(FeatureKind::Import(Import::new(
        "cart.step",
        FileData::new(step),
    )));
    parts.push(("STEP import", part));

    parts
        .into_iter()
        .map(|(name, part)| (name.to_owned(), part))
        .collect()
}

/// The `.oxyprt` files of the folder `OXYDE_CORPUS` names, if any.
fn corpus_from_folder() -> Vec<(String, Part)> {
    let Ok(folder) = std::env::var("OXYDE_CORPUS") else {
        return Vec::new();
    };
    let mut paths: Vec<_> = std::fs::read_dir(&folder)
        .unwrap_or_else(|e| panic!("OXYDE_CORPUS {folder}: {e}"))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|x| x == "oxyprt"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            match oxyde_format::load_part(&path)
                .map_err(|e| e.to_string())
                .and_then(|file| Part::from_definition(file.definition).map_err(|e| e.to_string()))
            {
                Ok(part) => Some((name, part)),
                Err(e) => {
                    println!("{name}: not read ({e})");
                    None
                }
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Running a part through a kernel.

/// What a kernel made of one solid of a part's body.
#[derive(Debug, Clone, Copy)]
struct Measures {
    volume: f64,
    centre: Point3,
    bounds: (Point3, Point3),
    counts: TopologyCounts,
}

/// A part rebuilt by one kernel.
struct Run {
    /// Each feature's name, type and status, in timeline order.
    features: Vec<(String, &'static str, FeatureStatus)>,
    /// The fastest of a few rebuilds from scratch.
    rebuild: Duration,
    /// Meshing every solid of the body for display.
    mesh: Duration,
    triangles: usize,
    solids: Vec<Measures>,
    /// Whether some edge is a B-spline: a curve the kernel fitted to where two surfaces
    /// cross, which its areas and volumes are only as close to as the fit.
    fitted: bool,
}

impl Run {
    fn built_whole(&self) -> bool {
        self.features
            .iter()
            .all(|(_, _, status)| !matches!(status, FeatureStatus::Error(_)))
    }
}

const REBUILDS: usize = 3;

fn run<K: Kernel>(definition: &PartDefinition, kernel: &mut K) -> Run {
    let mut rebuild = Duration::MAX;
    let mut last = None;
    for _ in 0..REBUILDS {
        let mut part = Part::from_definition(definition.clone()).expect("a valid part");
        let start = Instant::now();
        part.rebuild(kernel);
        rebuild = rebuild.min(start.elapsed());
        if let Some(mut previous) = last.replace(part) {
            previous.release(kernel);
        }
    }
    let mut part = last.expect("rebuilt at least once");
    let features = part
        .timeline()
        .iter()
        .map(|entry| (entry.name.clone(), entry.type_name, entry.status.clone()))
        .collect();
    let mut solids = Vec::new();
    let mut mesh = Duration::ZERO;
    let mut triangles = 0;
    let fitted = part.body().is_some_and(|body| {
        body.topology
            .edges
            .iter()
            .any(|e| e.curve == oxyde_kernel::CurveKind::BSpline)
    });
    if let Some(body) = part.body() {
        for solid in &body.solids {
            let shape = solid.shape;
            let measures = (|| {
                Some(Measures {
                    volume: kernel.volume(shape).ok()?,
                    centre: kernel.centre_of_mass(shape).ok()?,
                    bounds: kernel.bounds(shape).ok()?,
                    counts: kernel.topology_counts(shape).ok()?,
                })
            })();
            let Some(measures) = measures else {
                continue;
            };
            // A display mesh about as fine as the application asks for a part this size.
            let (lo, hi) = measures.bounds;
            let deflection = (hi - lo).length() * 1e-3;
            let start = Instant::now();
            if let Ok(m) = kernel.tessellate(shape, deflection) {
                mesh += start.elapsed();
                triangles += m.faces.iter().map(|f| f.triangles.len()).sum::<usize>();
            }
            solids.push(measures);
        }
    }
    part.release(kernel);
    Run {
        features,
        rebuild,
        mesh,
        triangles,
        solids,
        fitted,
    }
}

/// How the native kernel's body differs from OpenCASCADE's, if it does: to a millionth,
/// or a ten-thousandth where OpenCASCADE fitted B-splines to where surfaces cross (the
/// native kernel follows those curves exactly; OpenCASCADE's fit is a few millionths off).
fn difference(native: &[Measures], occt: &[Measures], fitted: bool) -> Option<String> {
    if native.len() != occt.len() {
        return Some(format!("{} solids, not {}", native.len(), occt.len()));
    }
    let tolerance = if fitted { 1e-4 } else { 1e-6 };
    let close = |a: f64, b: f64| (a - b).abs() <= tolerance * b.abs().max(1.0);
    let near = |a: Point3, b: Point3| a.distance(b) <= tolerance * b.to_vec().length().max(1.0);
    for (i, (n, o)) in native.iter().zip(occt).enumerate() {
        if !close(n.volume, o.volume) {
            return Some(format!("solid {i}: volume {} not {}", n.volume, o.volume));
        }
        if !near(n.centre, o.centre) {
            return Some(format!("solid {i}: centre of mass {:?}", n.centre));
        }
        if !near(n.bounds.0, o.bounds.0) || !near(n.bounds.1, o.bounds.1) {
            return Some(format!("solid {i}: bounds {:?}", n.bounds));
        }
        if n.counts != o.counts {
            return Some(format!(
                "solid {i}: {} faces, {} edges, {} vertices, not {}, {}, {}",
                n.counts.faces,
                n.counts.edges,
                n.counts.vertices,
                o.counts.faces,
                o.counts.edges,
                o.counts.vertices
            ));
        }
    }
    None
}

fn status_text(status: &FeatureStatus) -> String {
    match status {
        FeatureStatus::Ok => "ok".to_owned(),
        FeatureStatus::Warning(m) => format!("warning: {m}"),
        FeatureStatus::Error(m) => format!("**fails**: {m}"),
        other => format!("{other:?}"),
    }
}

fn ms(d: Duration) -> String {
    format!("{:.2}", d.as_secs_f64() * 1e3)
}

#[test]
fn what_the_native_kernel_builds_so_far() {
    let builtin = corpus();
    let extra = corpus_from_folder();
    let mut summary = String::new();
    let mut details = String::new();
    let mut problems = Vec::new();
    let mut newly = Vec::new();
    let _ = writeln!(
        summary,
        "| Part | Native | OCCT rebuild (ms) | Native rebuild (ms) | OCCT mesh (ms) | Native mesh (ms) | Triangles OCCT / native |"
    );
    let _ = writeln!(summary, "|---|---|---|---|---|---|---|");
    let (mut parts_whole, mut features_ok, mut features_all) = (0, 0, 0);
    let count = builtin.len() + extra.len();
    for (index, (name, part)) in builtin.iter().chain(&extra).enumerate() {
        let from_folder = index >= builtin.len();
        let definition = part.definition();
        let occt = run(&definition, &mut OcctKernel::new());
        let native = run(&definition, &mut NativeKernel::new());
        if !occt.built_whole() && !from_folder {
            problems.push(format!("{name}: OpenCASCADE does not build it"));
        }
        let mut verdict = if native.built_whole() {
            "builds".to_owned()
        } else {
            let failed = native
                .features
                .iter()
                .filter(|(_, _, s)| matches!(s, FeatureStatus::Error(_)))
                .count();
            format!("{failed} of {} fail", native.features.len())
        };
        if native.built_whole() && occt.built_whole() {
            match difference(&native.solids, &occt.solids, occt.fitted) {
                Some(why) => {
                    verdict = format!("**differs**: {why}");
                    problems.push(format!("{name}: {why}"));
                }
                None => parts_whole += 1,
            }
        }
        let listed = NATIVE_BUILDS.contains(&name.as_str());
        let whole = native.built_whole() && !verdict.starts_with("**differs");
        if listed && !whole {
            problems.push(format!(
                "{name}: listed in NATIVE_BUILDS, but the native kernel {verdict}"
            ));
        }
        if !listed && whole && !from_folder {
            newly.push(name.clone());
        }
        let _ = writeln!(
            summary,
            "| {name} | {verdict} | {} | {} | {} | {} | {} / {} |",
            ms(occt.rebuild),
            ms(native.rebuild),
            ms(occt.mesh),
            ms(native.mesh),
            occt.triangles,
            native.triangles
        );
        let _ = writeln!(details, "\n### {name}\n");
        let _ = writeln!(details, "| Feature | Type | OpenCASCADE | Native |");
        let _ = writeln!(details, "|---|---|---|---|");
        for ((feature, kind, o), (_, _, n)) in occt.features.iter().zip(&native.features) {
            if *kind == "Sketch" {
                continue;
            }
            features_all += 1;
            if !matches!(n, FeatureStatus::Error(_)) {
                features_ok += 1;
            }
            let _ = writeln!(
                details,
                "| {feature} | {kind} | {} | {} |",
                status_text(o),
                status_text(n)
            );
        }
    }
    let report = format!(
        "# What the native kernel builds\n\n{parts_whole} of {count} parts build whole as \
         OpenCASCADE builds them; {features_ok} of {features_all} features (sketches \
         aside) build.\n\n{summary}\n## Feature by feature\n{details}"
    );
    println!("{report}");
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("kernel-coverage.md");
    if std::fs::write(&path, &report).is_ok() {
        println!("Written to {}", path.display());
    }
    if !newly.is_empty() {
        println!(
            "Now built whole by the native kernel, to add to NATIVE_BUILDS: {}",
            newly.join(", ")
        );
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
