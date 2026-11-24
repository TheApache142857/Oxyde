//! Copies Convert Entities makes of part edges and vertices follow them through a rebuild
//! (`oxyde_model::projection`): a block's edge and corner copied into a sketch on the Top
//! plane move with the block's depth, stay with a warning while the block is suppressed,
//! and follow again once it is back.

use oxyde_geom::{Point2, Point3};
use oxyde_model::projection::{Projection, ProjectionSource, local};
use oxyde_model::{
    Extrude, ExtrudeExtent, ExtrudeOp, FeatureId, FeatureKind, FeatureStatus, Part, Sketch,
    SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;
use oxyde_sketch::{Constraint, EntityId};

/// The two ends of a copied edge and a copied corner must land within this of where their
/// sources project, mm.
const LANDS: f64 = 1e-6;

fn fixed(sketch: &mut Sketch, at: Point2) -> EntityId {
    let id = sketch.geometry.point(at);
    sketch.geometry.constrain(Constraint::Fixed(id));
    id
}

fn top(p: Point3) -> Point2 {
    local(&StandardPlane::Top.plane(), p)
}

fn near(a: Option<Point2>, b: Point2) -> bool {
    a.is_some_and(|a| a.distance(b) < LANDS)
}

struct Block {
    part: Part,
    extrude: FeatureId,
    copy: FeatureId,
    ends: (EntityId, EntityId),
    corner: EntityId,
}

/// A 40 × 20 block on the Front plane, 10 deep along +Z, and a sketch on the Top plane
/// holding a copy of its edge along Z at the origin and of its corner (40, 0, 10).
fn block(kernel: &mut OcctKernel) -> Block {
    let mut part = Part::new();
    let mut base = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    base.rectangle(Point2::ORIGIN, 40.0, 20.0);
    let s = part.add_feature(FeatureKind::Sketch(base));
    let extrude = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        10.0,
    )));
    part.rebuild(kernel);
    let body = part.body().expect("the block");
    let edge = body
        .topology
        .edges
        .iter()
        .position(|e| e.midpoint.distance(Point3::new(0.0, 0.0, 5.0)) < 1e-9)
        .expect("the edge along Z at the origin");
    let vertex = body
        .topology
        .vertices
        .iter()
        .position(|v| v.point.distance(Point3::new(40.0, 0.0, 10.0)) < 1e-9)
        .expect("the corner");
    let edge_name = body.naming.edge(edge as u32).cloned().expect("named");
    let vertex_name = body.naming.vertex(vertex as u32).cloned().expect("named");

    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
    let a = fixed(&mut sketch, top(Point3::new(0.0, 0.0, 0.0)));
    let b = fixed(&mut sketch, top(Point3::new(0.0, 0.0, 10.0)));
    let line = sketch.geometry.line_between(a, b);
    let corner = fixed(&mut sketch, top(Point3::new(40.0, 0.0, 10.0)));
    sketch.projections = vec![
        Projection {
            entity: line.0,
            source: ProjectionSource::Edge(edge_name),
        },
        Projection {
            entity: corner.0,
            source: ProjectionSource::Vertex(vertex_name),
        },
    ];
    let copy = part.add_feature(FeatureKind::Sketch(sketch));
    part.rebuild(kernel);
    Block {
        part,
        extrude,
        copy,
        ends: (a, b),
        corner,
    }
}

fn set_depth(block: &mut Block, depth: f64, kernel: &mut OcctKernel) {
    if let Ok(FeatureKind::Extrude(e)) = block.part.edit(block.extrude) {
        e.extent = ExtrudeExtent::Blind { depth };
    }
    block.part.rebuild(kernel);
}

/// Where the copies are after the last rebuild.
fn copies(block: &Block) -> (Option<Point2>, Option<Point2>, Option<Point2>) {
    let solved = &block.part.sketch_result(block.copy).expect("built").solved;
    (
        solved.position(block.ends.0),
        solved.position(block.ends.1),
        solved.position(block.corner),
    )
}

#[test]
fn converted_copies_follow_the_part() {
    let mut kernel = OcctKernel::new();
    let mut block = block(&mut kernel);
    assert_eq!(block.part.status(block.copy), Some(FeatureStatus::Ok));

    set_depth(&mut block, 15.0, &mut kernel);
    assert_eq!(block.part.status(block.copy), Some(FeatureStatus::Ok));
    let (a, b, corner) = copies(&block);
    assert!(near(a, top(Point3::new(0.0, 0.0, 0.0))), "{a:?}");
    assert!(near(b, top(Point3::new(0.0, 0.0, 15.0))), "{b:?}");
    assert!(
        near(corner, top(Point3::new(40.0, 0.0, 15.0))),
        "{corner:?}"
    );
}

#[test]
fn copies_of_a_suppressed_part_stay_and_follow_again() {
    let mut kernel = OcctKernel::new();
    let mut block = block(&mut kernel);
    block.part.set_suppressed(block.extrude, true).unwrap();
    block.part.rebuild(&mut kernel);
    let Some(FeatureStatus::Warning(why)) = block.part.status(block.copy) else {
        panic!("a warning: {:?}", block.part.status(block.copy));
    };
    assert!(why.contains("no longer in the part"), "{why}");
    let (_, b, corner) = copies(&block);
    assert!(near(b, top(Point3::new(0.0, 0.0, 10.0))), "{b:?}");
    assert!(
        near(corner, top(Point3::new(40.0, 0.0, 10.0))),
        "{corner:?}"
    );

    block.part.set_suppressed(block.extrude, false).unwrap();
    set_depth(&mut block, 12.0, &mut kernel);
    assert_eq!(block.part.status(block.copy), Some(FeatureStatus::Ok));
    let (_, b, corner) = copies(&block);
    assert!(near(b, top(Point3::new(0.0, 0.0, 12.0))), "{b:?}");
    assert!(
        near(corner, top(Point3::new(40.0, 0.0, 12.0))),
        "{corner:?}"
    );
}
