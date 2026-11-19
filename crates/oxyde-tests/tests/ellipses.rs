//! Ellipses and elliptic arcs in sketches, extruded, cut and revolved by OpenCASCADE.
//! Volumes are checked against π·a·b times the depth or the path of the centre, whichever
//! semi-axis is the longer and whichever way the profile walks an arc.

use std::f64::consts::{PI, TAU};

use oxyde_geom::Point2;
use oxyde_kernel::Kernel;
use oxyde_model::{
    Extrude, ExtrudeOp, FaceName, FeatureId, FeatureKind, FeatureStatus, Part, Resolution, Revolve,
    RevolveAxis, Sketch, SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;

fn assert_all_ok(part: &Part) {
    for entry in part.timeline() {
        assert_eq!(entry.status, FeatureStatus::Ok, "{}", entry.name);
    }
}

fn volume(part: &Part, kernel: &OcctKernel) -> f64 {
    part.body()
        .expect("part has a body")
        .solids
        .iter()
        .map(|s| kernel.volume(s.shape).unwrap())
        .sum()
}

fn assert_close(actual: f64, expected: f64) {
    let tolerance = 1e-6 * expected.abs().max(1.0);
    assert!(
        (actual - expected).abs() < tolerance,
        "expected {expected}, got {actual}"
    );
}

fn front() -> Sketch {
    Sketch::new(SketchPlane::Standard(StandardPlane::Front))
}

fn extrude(part: &mut Part, sketch: Sketch, op: ExtrudeOp, depth: f64) -> FeatureId {
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Extrude(Extrude::blind(s, op, depth)))
}

#[test]
fn an_extruded_ellipse_has_the_volume_of_its_area() {
    // Tilted by 30°, and once with the other semi-axis the longer one.
    let (cos, sin) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
    for (major, minor) in [(6.0, 2.0), (2.0, 6.0)] {
        let mut kernel = OcctKernel::new();
        let mut part = Part::new();
        let mut sketch = front();
        let id = sketch.geometry.ellipse(
            Point2::new(5.0, 5.0),
            Point2::new(5.0 + major * cos, 5.0 + major * sin),
            minor,
        );
        let boss = extrude(&mut part, sketch, ExtrudeOp::Boss, 10.0);
        part.rebuild(&mut kernel);
        assert_all_ok(&part);
        assert_close(volume(&part, &kernel), PI * 6.0 * 2.0 * 10.0);
        let body = part.body().unwrap();
        assert_eq!(body.topology.faces.len(), 3);
        // The wall is named by the ellipse that swept it.
        assert!(matches!(
            body.naming.resolve_face(&FaceName::profile(boss, id.0)),
            Resolution::Exact(_)
        ));
        part.release(&mut kernel);
    }
}

#[test]
fn half_an_ellipse_closed_by_its_axis_extrudes_either_way_round() {
    // The line runs one way or the other, so the profile walks the arc forwards or
    // backwards.
    for line_from_left in [true, false] {
        let mut kernel = OcctKernel::new();
        let mut part = Part::new();
        let mut sketch = front();
        let g = &mut sketch.geometry;
        let c = g.point(Point2::ORIGIN);
        let m = g.point(Point2::new(8.0, 0.0));
        let (s, e) = (
            g.point(Point2::new(8.0, 0.0)),
            g.point(Point2::new(-8.0, 0.0)),
        );
        g.elliptic_arc_between(c, m, 3.0, s, e);
        if line_from_left {
            g.line(Point2::new(-8.0, 0.0), Point2::new(8.0, 0.0));
        } else {
            g.line(Point2::new(8.0, 0.0), Point2::new(-8.0, 0.0));
        }
        extrude(&mut part, sketch, ExtrudeOp::Boss, 4.0);
        part.rebuild(&mut kernel);
        assert_all_ok(&part);
        assert_close(volume(&part, &kernel), PI * 8.0 * 3.0 / 2.0 * 4.0);
        part.release(&mut kernel);
    }
}

#[test]
fn an_elliptic_hole_cuts_its_area_out() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let mut block = front();
    block.rectangle(Point2::new(-20.0, -20.0), 40.0, 40.0);
    let boss = extrude(&mut part, block, ExtrudeOp::Boss, 5.0);
    let mut hole = Sketch::new(SketchPlane::Face(FaceName::end_cap(boss)));
    hole.geometry
        .ellipse(Point2::ORIGIN, Point2::new(0.0, 7.0), 12.0);
    extrude(&mut part, hole, ExtrudeOp::Cut, 5.0);
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(
        volume(&part, &kernel),
        40.0 * 40.0 * 5.0 - PI * 12.0 * 7.0 * 5.0,
    );
    part.release(&mut kernel);
}

#[test]
fn a_revolved_ellipse_sweeps_its_area_round_the_axis() {
    // An ellipse 3 by 2, its centre 10 from a centerline on the y axis: by Pappus, its area
    // times the path of its centre.
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let mut sketch = front();
    let axis = sketch
        .geometry
        .line(Point2::new(0.0, -5.0), Point2::new(0.0, 5.0));
    sketch.geometry.set_construction(axis, true);
    sketch
        .geometry
        .ellipse(Point2::new(10.0, 0.0), Point2::new(10.0, 3.0), 2.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Revolve(Revolve {
        sketch: s,
        op: ExtrudeOp::Boss,
        axis: RevolveAxis::SketchLine(axis.0),
        angle: TAU,
        mid_plane: false,
        reverse: false,
        bodies: Vec::new(),
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), PI * 3.0 * 2.0 * TAU * 10.0);
    part.release(&mut kernel);
}
