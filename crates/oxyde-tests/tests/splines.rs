//! Splines in sketches built by OpenCASCADE, and the kernel's exact curve of an edge
//! (`Kernel::edge_curve`), which Convert Entities projects into sketches.

use oxyde_geom::Point2;
use oxyde_kernel::{CurveKind, EdgeCurve, Kernel};
use oxyde_model::{
    Extrude, ExtrudeOp, FeatureId, FeatureKind, FeatureStatus, Part, Revolve, RevolveAxis, Sketch,
    SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;
use oxyde_sketch::SplineDef;

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

/// The cubic from (0, 0) through poles (2, 6) and (8, 6) to (10, 0), closed by its chord,
/// on the Front plane: ∫ y dx = 162/5 under it.
fn bump(weights: [f64; 4]) -> Sketch {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let g = &mut sketch.geometry;
    let poles: Vec<_> = [(0.0, 0.0), (2.0, 6.0), (8.0, 6.0), (10.0, 0.0)]
        .iter()
        .map(|(x, y)| g.point(Point2::new(*x, *y)))
        .collect();
    let (first, last) = (poles[0], poles[3]);
    g.spline_on(SplineDef {
        poles,
        weights: weights.to_vec(),
        knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
        degree: 3,
    });
    g.line_between(last, first);
    sketch
}

fn extrude(part: &mut Part, sketch: Sketch, depth: f64) -> FeatureId {
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        depth,
    )))
}

#[test]
fn an_extruded_spline_profile_has_the_volume_of_its_area() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    extrude(&mut part, bump([1.0; 4]), 5.0);
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    assert_close(volume(&part, &kernel), 162.0 / 5.0 * 5.0);
    part.release(&mut kernel);
}

#[test]
fn a_revolved_spline_profile_sweeps_its_area_round_the_axis() {
    // About a centerline 2 below the chord: by Pappus, the area times the path of its
    // centroid, 13/7 above the chord.
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let mut sketch = bump([1.0; 4]);
    let axis = sketch
        .geometry
        .line(Point2::new(0.0, -2.0), Point2::new(10.0, -2.0));
    sketch.geometry.set_construction(axis, true);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Revolve(Revolve {
        sketch: s,
        op: ExtrudeOp::Boss,
        axis: RevolveAxis::SketchLine(axis.0),
        angle: std::f64::consts::TAU,
        mid_plane: false,
        reverse: false,
        bodies: Vec::new(),
    }));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let path = std::f64::consts::TAU * (13.0 / 7.0 + 2.0);
    assert_close(volume(&part, &kernel), 162.0 / 5.0 * path);
    part.release(&mut kernel);
}

#[test]
fn edge_curves_come_back_exactly() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    // A rational spline profile, extruded; then an ellipse and a circle on its end cap.
    let boss = extrude(&mut part, bump([1.0, 0.5, 2.0, 1.0]), 5.0);
    let mut cap = Sketch::new(SketchPlane::Face(oxyde_model::FaceName::end_cap(boss)));
    cap.geometry
        .ellipse(Point2::new(5.0, 1.5), Point2::new(7.0, 1.5), 0.5);
    let s = part.add_feature(FeatureKind::Sketch(cap));
    part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        2.0,
    )));
    part.rebuild(&mut kernel);
    assert_all_ok(&part);
    let body = part.body().unwrap();
    let solid = &body.solids[0];
    let (mut lines, mut conics, mut splines) = (0, 0, 0);
    for (index, edge) in body.topology.edges.iter().enumerate() {
        let local = solid.local_edge(index as u32).unwrap();
        let curve = kernel.edge_curve(solid.shape, local).unwrap();
        match (edge.curve, curve) {
            (CurveKind::Line, Some(EdgeCurve::Line { start, end })) => {
                lines += 1;
                assert!((start.distance(end) - edge.length).abs() < 1e-9);
            }
            (
                CurveKind::Ellipse,
                Some(EdgeCurve::Conic {
                    major,
                    minor,
                    start,
                    end,
                    ..
                }),
            ) => {
                conics += 1;
                assert!((major - 2.0).abs() < 1e-9 && (minor - 0.5).abs() < 1e-9);
                assert!((end - start - std::f64::consts::TAU).abs() < 1e-9);
            }
            (CurveKind::BSpline, Some(EdgeCurve::Nurbs { curve, exact })) => {
                splines += 1;
                assert!(exact);
                assert_eq!(curve.degree, 3);
                // The weights come back as given, up to OpenCASCADE's scaling of all of
                // them together.
                let scale = curve.weights[0];
                let given = [1.0, 0.5, 2.0, 1.0];
                for (w, g) in curve.weights.iter().zip(given) {
                    assert!((w / scale - g).abs() < 1e-12, "{:?}", curve.weights);
                }
                // The poles lie in z = 0 or z = 5 (the start and end caps), at the sketch's
                // poles.
                let z = curve.poles[0].z;
                assert!(z.abs() < 1e-9 || (z - 5.0).abs() < 1e-9);
                for (p, (x, y)) in
                    curve
                        .poles
                        .iter()
                        .zip([(0.0, 0.0), (2.0, 6.0), (8.0, 6.0), (10.0, 0.0)])
                {
                    assert!((p.x - x).abs() < 1e-9 && (p.y - y).abs() < 1e-9, "{p:?}");
                }
            }
            (kind, curve) => panic!("edge {index}: {kind:?} came back as {curve:?}"),
        }
    }
    // The chord at either end, the two side edges where the chord meets the spline, and the
    // seam of the ellipse's wall.
    assert_eq!(lines, 5);
    assert_eq!(splines, 2);
    assert_eq!(conics, 2);
    part.release(&mut kernel);
}
