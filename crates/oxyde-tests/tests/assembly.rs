//! Assemblies of real parts: mates resolved on OpenCASCADE geometry and solved.

use oxyde_assembly::{
    Assembly, MateKind, MateRef, MateStatus, MateTarget, Placement, Primitive, resolve, solve,
};
use oxyde_geom::{Point2, Point3, Vec3};
use oxyde_model::{
    Extrude, ExtrudeOp, FaceName, FeatureKind, Part, Sketch, SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;

/// A 60 × 40 × 10 block on the Front plane with a Ø10 hole through it at (20, 20).
/// Returns the part and the names of the hole's wall and the block's top (+Z) face.
fn block_with_hole(kernel: &mut OcctKernel) -> (Part, FaceName, FaceName) {
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::ORIGIN, 60.0, 40.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let block = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        10.0,
    )));
    let mut hole = Sketch::new(SketchPlane::Face(FaceName::end_cap(block)));
    let circle = hole.circle(Point2::new(20.0, 20.0), 5.0);
    let s2 = part.add_feature(FeatureKind::Sketch(hole));
    let cut = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s2,
        ExtrudeOp::Cut,
        10.0,
    )));
    part.rebuild(kernel);
    (
        part,
        FaceName::profile(cut, circle),
        FaceName::end_cap(block),
    )
}

/// A Ø10 pin, 30 long along +Z from the Front plane, centred on the origin. Returns the
/// part, its round face and its bottom (start) face.
fn pin(kernel: &mut OcctKernel) -> (Part, FaceName, FaceName) {
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let circle = sketch.circle(Point2::ORIGIN, 5.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let pin = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        30.0,
    )));
    part.rebuild(kernel);
    (
        part,
        FaceName::profile(pin, circle),
        FaceName::start_cap(pin),
    )
}

#[test]
fn a_pin_goes_into_a_hole() {
    let mut kernel = OcctKernel::new();
    let (block, hole, top) = block_with_hole(&mut kernel);
    let (pin_part, round, bottom) = pin(&mut kernel);

    // The faces resolve to the geometry a mate needs.
    let wall = resolve(&MateTarget::Face(hole.clone()), block.body()).unwrap();
    match wall {
        Primitive::Line { origin, direction } => {
            assert!((origin.x - 20.0).abs() < 1e-6 && (origin.y - 20.0).abs() < 1e-6);
            assert!(direction.cross(Vec3::Z).length() < 1e-9);
        }
        other => panic!("a hole wall should give its axis, got {other:?}"),
    }
    match resolve(&MateTarget::Face(top.clone()), block.body()).unwrap() {
        Primitive::Plane { normal, .. } => assert!((normal.z - 1.0).abs() < 1e-9, "{normal:?}"),
        other => panic!("a flat face should give a plane, got {other:?}"),
    }

    let mut asm = Assembly::new();
    let a = asm.add_component(
        "Block",
        "block.oxyprt".into(),
        "a".into(),
        Placement::IDENTITY,
    );
    let b = asm.add_component(
        "Pin",
        "pin.oxyprt".into(),
        "b".into(),
        Placement::IDENTITY.moved(
            Vec3::new(0.3, 0.2, 0.0),
            Point3::ORIGIN,
            Vec3::new(80.0, 10.0, 25.0),
        ),
    );
    let side = |component, face: &FaceName| MateRef {
        component,
        target: MateTarget::Face(face.clone()),
    };
    let concentric = asm.add_mate(MateKind::Concentric, side(a, &hole), side(b, &round), true);
    // The pin's bottom flush with the block's top: the faces face each other.
    let flush = asm.add_mate(MateKind::Coincident, side(a, &top), side(b, &bottom), false);

    let body = |c| {
        if c == a {
            block.body()
        } else {
            pin_part.body()
        }
    };
    let solution = solve(&asm, |r| resolve(&r.target, body(r.component)), 100.0);
    assert_eq!(solution.status(concentric), Some(&MateStatus::Ok));
    assert_eq!(solution.status(flush), Some(&MateStatus::Ok));
    let placement = solution.placements[0].1;
    // The pin's axis is the hole's, and its bottom sits on the top face.
    let foot = placement.apply_point(Point3::ORIGIN);
    assert!(
        (foot.x - 20.0).abs() < 1e-5 && (foot.y - 20.0).abs() < 1e-5,
        "{foot:?}"
    );
    assert!((foot.z - 10.0).abs() < 1e-5, "{foot:?}");
    assert!(placement.apply_vector(Vec3::Z).z.abs() > 1.0 - 1e-9);
    // It can still turn about its axis.
    assert_eq!(solution.degrees(b), Some(1));

    let mut parts = [block, pin_part];
    for p in &mut parts {
        p.release(&mut kernel);
    }
}
