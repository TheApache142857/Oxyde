//! Holes (Hole Wizard) at the points of a sketch on a face: simple, blind with a drill
//! point, counterbored and countersunk, through a block whose volumes are worked out by
//! hand; their faces' names, and a pattern of them.

use std::f64::consts::{FRAC_PI_2, PI};

use oxyde_geom::Point2;
use oxyde_kernel::Kernel;
use oxyde_model::{
    AxisRef, Extrude, ExtrudeOp, FaceKey, FaceName, FeatureId, FeatureKind, FeatureStatus, Hole,
    HoleKind, LinearPattern, Part, PatternDirection, Resolution, Sketch, SketchPlane, StandardAxis,
    StandardPlane,
};
use oxyde_occt::OcctKernel;

const BLOCK: f64 = 40.0 * 20.0 * 10.0;

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

/// A 40 × 20 × 10 block on the Front plane, and a sketch on its top face (z = 10, its
/// x and y those of the model) with points at (10, 10) and (30, 10). Returns the sketch
/// and the two points' ids.
fn drilled_block(part: &mut Part) -> (FeatureId, [u32; 2]) {
    let mut base = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    base.rectangle(Point2::ORIGIN, 40.0, 20.0);
    let s = part.add_feature(FeatureKind::Sketch(base));
    let block = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        10.0,
    )));
    let mut spots = Sketch::new(SketchPlane::Face(FaceName::end_cap(block)));
    let a = spots.geometry.point(Point2::new(10.0, 10.0));
    let b = spots.geometry.point(Point2::new(30.0, 10.0));
    // A line and its ends are no hole positions.
    spots
        .geometry
        .line(Point2::new(2.0, 2.0), Point2::new(8.0, 2.0));
    let sketch = part.add_feature(FeatureKind::Sketch(spots));
    (sketch, [a.0, b.0])
}

fn hole(sketch: FeatureId, kind: HoleKind, diameter: f64, depth: Option<f64>) -> FeatureKind {
    FeatureKind::Hole(Hole {
        sketch,
        kind,
        diameter,
        depth,
        reverse: false,
        bodies: Vec::new(),
    })
}

fn set(part: &mut Part, feature: FeatureId, kind: HoleKind, diameter: f64, depth: Option<f64>) {
    match part.edit(feature).unwrap() {
        FeatureKind::Hole(h) => {
            h.kind = kind;
            h.diameter = diameter;
            h.depth = depth;
        }
        _ => unreachable!(),
    }
}

fn ok(part: &Part, feature: FeatureId) {
    assert_eq!(part.status(feature), Some(FeatureStatus::Ok));
}

#[test]
fn holes_of_every_kind_take_what_they_should() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (sketch, [a, _]) = drilled_block(&mut part);
    let holes = part.add_feature(hole(sketch, HoleKind::Simple, 6.0, None));
    part.rebuild(&mut kernel);
    ok(&part, holes);
    // Two Ø6 holes through the 10 mm block.
    close(volume(&part, &kernel), BLOCK - 2.0 * PI * 9.0 * 10.0);
    // The hole's wall is named after its point.
    let wall = FaceName::new(FaceKey::Instance {
        feature: holes,
        instance: [a, 0],
        face: Box::new(FaceName::profile(holes, 4)),
    });
    let found = part.body().unwrap().naming.resolve_face(&wall);
    assert!(matches!(found, Resolution::Exact(_)), "{wall}: {found:?}");
    // The hole's diameter is its first dimension.
    assert!(part.find_dimension("D1@Hole1").is_some());

    // Blind, 6 deep, with a 118° drill point under it.
    set(&mut part, holes, HoleKind::Simple, 6.0, Some(6.0));
    part.rebuild(&mut kernel);
    ok(&part, holes);
    let tip = 3.0 / (59f64.to_radians()).tan();
    let blind = PI * 9.0 * 6.0 + PI * 9.0 * tip / 3.0;
    close(volume(&part, &kernel), BLOCK - 2.0 * blind);

    // Counterbored Ø11 × 4 over a Ø6.6 through hole.
    set(
        &mut part,
        holes,
        HoleKind::Counterbore {
            diameter: 11.0,
            depth: 4.0,
        },
        6.6,
        None,
    );
    part.rebuild(&mut kernel);
    ok(&part, holes);
    let counterbored = PI * 5.5 * 5.5 * 4.0 + PI * 3.3 * 3.3 * 6.0;
    close(volume(&part, &kernel), BLOCK - 2.0 * counterbored);

    // Countersunk Ø12 at 90° over a Ø6 through hole: a cone 3 deep, then the hole.
    set(
        &mut part,
        holes,
        HoleKind::Countersink {
            diameter: 12.0,
            angle: FRAC_PI_2,
        },
        6.0,
        None,
    );
    part.rebuild(&mut kernel);
    ok(&part, holes);
    let countersunk = PI * 3.0 / 3.0 * (36.0 + 18.0 + 9.0) + PI * 9.0 * 7.0;
    close(volume(&part, &kernel), BLOCK - 2.0 * countersunk);

    // A counterbore narrower than its hole is refused.
    set(
        &mut part,
        holes,
        HoleKind::Counterbore {
            diameter: 5.0,
            depth: 4.0,
        },
        6.0,
        None,
    );
    part.rebuild(&mut kernel);
    match part.status(holes) {
        Some(FeatureStatus::Error(message)) => assert!(message.contains("wider"), "{message}"),
        other => panic!("expected an error, got {other:?}"),
    }
    part.release(&mut kernel);
}

#[test]
fn a_hole_needs_points_and_can_be_patterned() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (sketch, _) = drilled_block(&mut part);
    // A sketch with a circle and no lone point: nothing to drill.
    let mut round = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    round.circle(Point2::new(5.0, 5.0), 1.0);
    let empty = part.add_feature(FeatureKind::Sketch(round));
    let nothing = part.add_feature(hole(empty, HoleKind::Simple, 3.0, None));
    let holes = part.add_feature(hole(sketch, HoleKind::Simple, 4.0, None));
    let row = part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![holes],
        first: PatternDirection {
            axis: AxisRef::Standard(StandardAxis::Y),
            spacing: 6.0,
            count: 2,
            reverse: false,
        },
        second: None,
    }));
    part.rebuild(&mut kernel);
    match part.status(nothing) {
        Some(FeatureStatus::Error(message)) => assert!(message.contains("no lone points")),
        other => panic!("expected an error, got {other:?}"),
    }
    ok(&part, holes);
    ok(&part, row);
    // Four Ø4 holes through: two, and their copies 6 mm along Y.
    close(volume(&part, &kernel), BLOCK - 4.0 * PI * 4.0 * 10.0);
    part.release(&mut kernel);
}
