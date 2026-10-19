//! Previews: a feature built on the part's last rebuild without being added to it, as the
//! viewport shows it while the feature's command is open. The part must come out exactly
//! as it went in.

use oxyde_geom::Point2;
use oxyde_kernel::Kernel;
use oxyde_model::{
    Body, EdgeBlend, EdgeName, Extrude, ExtrudeOp, FaceName, FeatureId, FeatureKind, FeatureStatus,
    Part, PlaneRef, RefPlane, ReferenceGeometry, Sketch, SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;

// In a new sketch, `Sketch::rectangle` creates its four corner points first (ids 1–4),
// then its bottom, right, top and left lines (ids 5–8).
const RIGHT: u32 = 6;

fn rectangle_on_front(corner: Point2, width: f64, height: f64) -> FeatureKind {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(corner, width, height);
    FeatureKind::Sketch(sketch)
}

fn extrude(sketch: FeatureId, op: ExtrudeOp, depth: f64) -> FeatureKind {
    FeatureKind::Extrude(Extrude::blind(sketch, op, depth))
}

fn body_volume(body: &Body, kernel: &OcctKernel) -> f64 {
    body.solids
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

/// A 40 × 20 × 10 block, and a second sketch (10 × 10 at the corner) nothing uses yet.
fn block_and_sketch() -> (Part, FeatureId, FeatureId) {
    let mut part = Part::new();
    let s1 = part.add_feature(rectangle_on_front(Point2::ORIGIN, 40.0, 20.0));
    let base = part.add_feature(extrude(s1, ExtrudeOp::Boss, 10.0));
    let s2 = part.add_feature(rectangle_on_front(Point2::ORIGIN, 10.0, 10.0));
    (part, base, s2)
}

#[test]
fn a_new_feature_is_previewed_without_changing_the_part() {
    let mut kernel = OcctKernel::new();
    let (mut part, _, s2) = block_and_sketch();
    part.rebuild(&mut kernel);
    let definition = part.definition();
    let timeline = part.timeline();
    let before = body_volume(part.body().unwrap(), &kernel);

    // A boss 5 mm out of the back of the block: the sketch is on the face the block
    // started from, and along the normal the boss would go into the block.
    let mut boss = Extrude::blind(s2, ExtrudeOp::Boss, 5.0);
    boss.reverse = true;
    let preview = part
        .preview(FeatureKind::Extrude(boss), None, &mut kernel)
        .expect("the part is rebuilt");
    assert_eq!(preview.status(), &FeatureStatus::Ok);
    assert_eq!(preview.name(), "Boss-Extrude2");
    let body = preview.body().expect("a boss changes the body");
    assert_close(body_volume(body, &kernel), before + 10.0 * 10.0 * 5.0);
    // Its faces are named after the feature it would be.
    let made = body
        .naming
        .faces()
        .iter()
        .filter(|n| n.key.feature() == preview.feature())
        .count();
    assert!(made >= 3, "faces made by the preview: {made}");
    assert!(part.feature(preview.feature()).is_none());

    // A cut through the corner (cuts go against the sketch normal, so reversed).
    preview.release(&mut kernel);
    let mut cut = Extrude::blind(s2, ExtrudeOp::Cut, 10.0);
    cut.reverse = true;
    let preview = part
        .preview(FeatureKind::Extrude(cut), None, &mut kernel)
        .unwrap();
    assert_close(
        body_volume(preview.body().unwrap(), &kernel),
        before - 10.0 * 10.0 * 10.0,
    );
    preview.release(&mut kernel);

    // The part is as it was, and its geometry still usable.
    assert_eq!(part.definition(), definition);
    assert_eq!(part.timeline(), timeline);
    assert_close(body_volume(part.body().unwrap(), &kernel), before);
    part.release(&mut kernel);
}

#[test]
fn an_edited_feature_is_previewed_where_it_stands() {
    let mut kernel = OcctKernel::new();
    let (mut part, base, _) = block_and_sketch();
    let edge = EdgeName::between(FaceName::profile(base, RIGHT), FaceName::end_cap(base));
    let round = part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: vec![edge.clone()],
        size: 2.0,
        chamfer: Default::default(),
    }));
    part.rebuild(&mut kernel);
    let finished = body_volume(part.body().unwrap(), &kernel);

    // The block 25 mm deep instead of 10: only the block is built, not the fillet after it.
    let s1 = part.features()[0].id();
    let deeper = extrude(s1, ExtrudeOp::Boss, 25.0);
    let preview = part.preview(deeper, Some(base), &mut kernel).unwrap();
    assert_eq!(preview.feature(), base);
    assert_eq!(preview.name(), "Boss-Extrude1");
    assert_close(
        body_volume(preview.body().unwrap(), &kernel),
        40.0 * 20.0 * 25.0,
    );
    let blend_face = FaceName::from_edge(round, edge.clone());
    assert!(
        preview
            .body()
            .unwrap()
            .naming
            .find_face(&blend_face)
            .is_none()
    );
    preview.release(&mut kernel);

    // The fillet edited to a radius too large for the block: it says why, and makes nothing.
    let huge = FeatureKind::Fillet(EdgeBlend {
        edges: vec![edge],
        size: 50.0,
        chamfer: Default::default(),
    });
    let preview = part.preview(huge, Some(round), &mut kernel).unwrap();
    assert!(
        matches!(preview.status(), FeatureStatus::Error(_)),
        "{:?}",
        preview.status()
    );
    assert!(preview.body().is_none());
    preview.release(&mut kernel);

    assert_close(body_volume(part.body().unwrap(), &kernel), finished);
    assert_eq!(part.status(round), Some(FeatureStatus::Ok));
    part.release(&mut kernel);
}

#[test]
fn reference_planes_are_previewed_too() {
    let mut kernel = OcctKernel::new();
    let (mut part, _, _) = block_and_sketch();
    part.rebuild(&mut kernel);
    let plane = FeatureKind::Plane(RefPlane::Offset {
        base: PlaneRef::Standard(StandardPlane::Top),
        offset: 30.0,
        flip: false,
    });
    let preview = part.preview(plane, None, &mut kernel).unwrap();
    assert!(preview.body().is_none());
    match preview.reference() {
        Some(ReferenceGeometry::Plane(p)) => assert_close(p.origin.y, 30.0),
        other => panic!("expected a plane, got {other:?}"),
    }
    preview.release(&mut kernel);
    part.release(&mut kernel);
}

#[test]
fn nothing_is_previewed_on_a_part_not_rebuilt() {
    let mut kernel = OcctKernel::new();
    let (part, base, s2) = block_and_sketch();
    assert!(
        part.preview(extrude(s2, ExtrudeOp::Boss, 5.0), None, &mut kernel)
            .is_none()
    );
    assert!(
        part.preview(extrude(s2, ExtrudeOp::Boss, 5.0), Some(base), &mut kernel)
            .is_none()
    );
}
