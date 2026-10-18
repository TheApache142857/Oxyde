//! Parts with several solid bodies: new bodies, cuts through some or all of them,
//! Combine, and features working on one body.

use oxyde_geom::Point2;
use oxyde_kernel::Kernel;
use oxyde_model::{
    AxisRef, BodyId, Combine, CombineOp, EdgeBlend, EdgeName, Extrude, ExtrudeOp, FaceName,
    FeatureId, FeatureKind, FeatureStatus, LinearPattern, Part, PatternDirection, Sketch,
    SketchPlane, StandardAxis, StandardPlane,
};
use oxyde_occt::OcctKernel;

/// A 40 × 20 × 10 block on the Front plane from `x`, made with `op`. Returns the feature
/// and the ids of its sketch's bottom, right, top and left lines.
fn block(part: &mut Part, x: f64, op: ExtrudeOp) -> (FeatureId, [u32; 4]) {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let lines = sketch.rectangle(Point2::new(x, 0.0), 40.0, 20.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let mut extrude = Extrude::blind(s, op, 10.0);
    extrude.reverse = false;
    (part.add_feature(FeatureKind::Extrude(extrude)), lines)
}

/// A Ø6 hole through everything at (`x`, 10), cutting `bodies` (empty: all of them).
fn hole(part: &mut Part, x: f64, bodies: Vec<BodyId>) -> FeatureId {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.circle(Point2::new(x, 10.0), 3.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let mut cut = Extrude::blind(s, ExtrudeOp::Cut, 10.0);
    // Cuts go against the sketch normal; this sketch is under the blocks.
    cut.reverse = true;
    cut.bodies = bodies;
    part.add_feature(FeatureKind::Extrude(cut))
}

fn hole_volume() -> f64 {
    std::f64::consts::PI * 9.0 * 10.0
}

fn bodies(part: &Part) -> usize {
    part.body().map_or(0, |b| b.solids.len())
}

fn volume(part: &Part, kernel: &OcctKernel) -> f64 {
    part.body()
        .expect("a body")
        .solids
        .iter()
        .map(|s| kernel.volume(s.shape).unwrap())
        .sum()
}

fn assert_ok(part: &Part) {
    for entry in part.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{}", entry.name);
    }
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-3 * expected.max(1.0),
        "expected {expected}, got {actual}"
    );
}

/// Two blocks overlapping on 20 × 20 × 10, the second a new body.
fn two_bodies(part: &mut Part) -> (FeatureId, FeatureId) {
    let (a, _) = block(part, 0.0, ExtrudeOp::Boss);
    let (b, _) = block(part, 20.0, ExtrudeOp::NewBody);
    (a, b)
}

#[test]
fn a_new_body_stays_apart() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    two_bodies(&mut part);
    part.rebuild(&mut kernel);
    assert_ok(&part);
    assert_eq!(bodies(&part), 2);
    // Both whole: the overlap counts twice.
    close(volume(&part, &kernel), 16000.0);
    // Faces of both are named, and found on the right body.
    let body = part.body().unwrap();
    assert_eq!(body.topology.faces.len(), 12);
    part.release(&mut kernel);
}

#[test]
fn a_cut_goes_through_every_body_unless_told() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (_, b) = two_bodies(&mut part);
    // In the overlap: through both bodies.
    hole(&mut part, 30.0, Vec::new());
    part.rebuild(&mut kernel);
    assert_ok(&part);
    close(volume(&part, &kernel), 16000.0 - 2.0 * hole_volume());
    // Only through the second body, still where both are (clear of the first hole).
    hole(&mut part, 36.5, vec![BodyId::new(b, 0)]);
    part.rebuild(&mut kernel);
    assert_ok(&part);
    close(volume(&part, &kernel), 16000.0 - 3.0 * hole_volume());
    assert_eq!(bodies(&part), 2);
    part.release(&mut kernel);
}

#[test]
fn combine_joins_or_cuts_bodies() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (a, b) = two_bodies(&mut part);
    let combine = part.add_feature(FeatureKind::Combine(Combine {
        target: BodyId::new(a, 0),
        tools: vec![BodyId::new(b, 0)],
        op: CombineOp::Join,
        keep_tools: false,
    }));
    part.rebuild(&mut kernel);
    assert_ok(&part);
    assert_eq!(bodies(&part), 1);
    close(volume(&part, &kernel), 12000.0);

    // Cut instead, keeping the tool: 8000 − 4000 left of the first, the second whole.
    if let Ok(FeatureKind::Combine(c)) = part.edit(combine) {
        c.op = CombineOp::Cut;
        c.keep_tools = true;
    }
    part.rebuild(&mut kernel);
    assert_ok(&part);
    assert_eq!(bodies(&part), 2);
    close(volume(&part, &kernel), 4000.0 + 8000.0);
    part.release(&mut kernel);
}

#[test]
fn a_fillet_works_on_the_body_of_its_edges() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (_, _) = block(&mut part, 0.0, ExtrudeOp::Boss);
    let (b, lines) = block(&mut part, 60.0, ExtrudeOp::NewBody);
    part.rebuild(&mut kernel);
    let before = volume(&part, &kernel);
    // The top edge of the second block's end face.
    part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: vec![EdgeName::between(
            FaceName::profile(b, lines[2]),
            FaceName::end_cap(b),
        )],
        size: 2.0,
        chamfer: Default::default(),
    }));
    part.rebuild(&mut kernel);
    assert_ok(&part);
    assert_eq!(bodies(&part), 2);
    let removed = before - volume(&part, &kernel);
    // A 2 mm round along 40 mm: (4 − π) × 40.
    close(removed, (4.0 - std::f64::consts::PI) * 40.0);
    part.release(&mut kernel);
}

#[test]
fn a_pattern_of_a_new_body_makes_bodies() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (b, _) = block(&mut part, 0.0, ExtrudeOp::NewBody);
    part.add_feature(FeatureKind::LinearPattern(LinearPattern {
        seeds: vec![b],
        first: PatternDirection {
            axis: AxisRef::Standard(StandardAxis::X),
            spacing: 50.0,
            count: 3,
            reverse: false,
        },
        second: None,
    }));
    part.rebuild(&mut kernel);
    assert_ok(&part);
    assert_eq!(bodies(&part), 3);
    close(volume(&part, &kernel), 3.0 * 8000.0);
    part.release(&mut kernel);
}

#[test]
fn a_single_body_part_builds_as_before() {
    // The first boss makes the first body; a cut and a fillet change it in place.
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let (a, lines) = block(&mut part, 0.0, ExtrudeOp::Boss);
    hole(&mut part, 10.0, Vec::new());
    part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: vec![EdgeName::between(
            FaceName::profile(a, lines[0]),
            FaceName::end_cap(a),
        )],
        size: 1.0,
        chamfer: Default::default(),
    }));
    part.rebuild(&mut kernel);
    assert_ok(&part);
    assert_eq!(bodies(&part), 1);
    assert_eq!(part.body().unwrap().solids[0].id, BodyId::new(a, 0));
    part.release(&mut kernel);
}
