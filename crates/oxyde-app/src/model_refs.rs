//! The part's edges as seen from a sketch, in sketch coordinates. Sketch mode snaps to the end
//! points, middles and centres of the straight, circular and arc edges lying in the sketch
//! plane. Convert Entities copies those edges into the sketch, and also projects the part's
//! other edges and its vertices onto the plane along its normal, as SolidWorks does, so that
//! new geometry can be placed exactly on existing (or imported) geometry. Every projection
//! is exact: a circle tilted to the plane projects to an ellipse, an ellipse to an ellipse,
//! and any other curve, which the kernel hands over as a rational B-spline, to the B-spline
//! on its projected poles.

use std::f64::consts::{PI, TAU};
use std::sync::Arc;

use oxyde_geom::{Axis, Ellipse2, Nurbs2, Plane, Point2, Point3, Vec3};
use oxyde_kernel::{CurveKind, EdgeCurve, EdgeInfo, Topology};

/// Edges further than this from the sketch plane are projected onto it, not snapped to (mm).
const ON_PLANE: f64 = 1e-4;

/// Two projected points closer than this are one (mm).
const SAME_POINT: f64 = 1e-9;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModelPointKind {
    /// End of an edge.
    Vertex,
    /// Middle of a straight edge.
    Middle,
    /// Centre of a circular edge.
    Center,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelCurve {
    Line(Point2, Point2),
    Circle(Point2, f64),
    /// Counter-clockwise from `start` to `end`.
    Arc {
        center: Point2,
        start: Point2,
        end: Point2,
    },
    Ellipse(Ellipse2),
    /// Counter-clockwise from `start` to `end` on the ellipse.
    EllipticArc {
        ellipse: Ellipse2,
        start: Point2,
        end: Point2,
    },
    Spline(Arc<Nurbs2>),
}

/// The part geometry a sketch can refer to.
#[derive(Debug, Clone, Default)]
pub struct ModelGeometry {
    /// Points to snap to: those of the edges lying in the sketch plane.
    pub points: Vec<(Point2, ModelPointKind)>,
    /// Curves Convert Entities can copy: the edges lying in the sketch plane first, then the
    /// other edges projected onto it. A projection that repeats a curve already listed is
    /// left out, so the top and bottom edges of a box seen from above give one curve per
    /// side.
    pub curves: Vec<ModelCurve>,
    /// Every vertex of the part projected onto the sketch plane, once each, for Convert
    /// Entities to copy as a fixed point.
    pub vertices: Vec<Point2>,
}

fn local(plane: &Plane, p: Point3) -> Point2 {
    let v = p - plane.origin;
    Point2::new(v.dot(plane.x_dir), v.dot(plane.y_dir()))
}

fn on_plane(plane: &Plane, p: Point3) -> bool {
    (p - plane.origin).dot(plane.normal).abs() < ON_PLANE
}

/// Whether a circle about `axis` lies in planes parallel to the sketch plane, so that it
/// projects onto it as a circle of the same radius.
fn flat(plane: &Plane, axis: &Axis) -> bool {
    axis.direction.cross(plane.normal).length() < 1e-6
}

/// Whether `p` lies on the counter-clockwise arc from `start` to `end` around `center`.
fn on_ccw_arc(center: Point2, start: Point2, end: Point2, p: Point2) -> bool {
    let angle = |q: Point2| (q.y - center.y).atan2(q.x - center.x);
    let tau = std::f64::consts::TAU;
    let sweep = (angle(end) - angle(start)).rem_euclid(tau);
    let at = (angle(p) - angle(start)).rem_euclid(tau);
    at <= sweep
}

/// A circular edge about a flat axis, seen in the sketch plane: an arc between its two ends,
/// counter-clockwise through its middle, or a whole circle.
fn round_curve(plane: &Plane, axis: &Axis, edge: &EdgeInfo, ends: &[Point3]) -> ModelCurve {
    let center = local(plane, axis.origin);
    let middle = local(plane, edge.midpoint);
    match ends {
        [a, b] if a.distance(*b) > 1e-9 => {
            let (mut start, mut end) = (local(plane, *a), local(plane, *b));
            if !on_ccw_arc(center, start, end, middle) {
                std::mem::swap(&mut start, &mut end);
            }
            ModelCurve::Arc { center, start, end }
        }
        _ => ModelCurve::Circle(center, center.distance(middle)),
    }
}

/// A circular edge about an axis tilted to the sketch plane, seen along the plane's normal:
/// an ellipse, or an arc of it counter-clockwise through the projected middle. The ellipse's
/// long semi-axis is the radius, square to both the axis and the normal; the short one is the
/// radius times the cosine of the tilt. A circle seen edge-on projects to the stretch of the
/// line it covers.
fn tilted_curve(
    plane: &Plane,
    axis: &Axis,
    edge: &EdgeInfo,
    ends: &[Point3],
) -> Option<ModelCurve> {
    let n = axis.direction.normalized()?;
    let radius = edge.midpoint.distance(axis.origin);
    let major_3d = n.cross(plane.normal).normalized()?;
    let major = Point2::new(major_3d.dot(plane.x_dir), major_3d.dot(plane.y_dir()));
    let center = local(plane, axis.origin);
    let ellipse = Ellipse2 {
        center,
        major: Point2::new(center.x + radius * major.x, center.y + radius * major.y),
        minor: radius * n.dot(plane.normal).abs(),
    };
    let arc = match ends {
        [a, b] if a.distance(*b) > 1e-9 => Some((*a, *b)),
        _ => None,
    };
    if !ellipse.is_valid() {
        // Edge-on: where the circle reaches along the major direction, from its angle
        // about the axis measured from that direction.
        let across = n.cross(major_3d);
        let angle = |p: Point3| {
            let d = p - axis.origin;
            d.dot(across).atan2(d.dot(major_3d))
        };
        let (lo, hi) = match arc {
            None => (-radius, radius),
            Some((a, b)) => {
                let (a0, am, a1) = (angle(a), angle(edge.midpoint), angle(b));
                let ccw = (am - a0).rem_euclid(TAU) < (a1 - a0).rem_euclid(TAU);
                let (from, sweep) = if ccw {
                    (a0, (a1 - a0).rem_euclid(TAU))
                } else {
                    (a1, (a0 - a1).rem_euclid(TAU))
                };
                let (p, q) = (radius * a0.cos(), radius * a1.cos());
                let (mut lo, mut hi) = (p.min(q), p.max(q));
                for (extreme, at) in [(0.0, radius), (PI, -radius)] {
                    if (extreme - from).rem_euclid(TAU) <= sweep {
                        lo = lo.min(at);
                        hi = hi.max(at);
                    }
                }
                (lo, hi)
            }
        };
        let at = |t: f64| Point2::new(center.x + t * major.x, center.y + t * major.y);
        return (hi - lo >= SAME_POINT).then(|| ModelCurve::Line(at(lo), at(hi)));
    }
    Some(match arc {
        Some((a, b)) => {
            let (mut start, mut end) = (local(plane, a), local(plane, b));
            let (a0, sweep) = ellipse.sweep(start, end);
            if (ellipse.angle_of(local(plane, edge.midpoint)) - a0).rem_euclid(TAU) > sweep {
                std::mem::swap(&mut start, &mut end);
            }
            ModelCurve::EllipticArc {
                ellipse,
                start,
                end,
            }
        }
        None => ModelCurve::Ellipse(ellipse),
    })
}

/// A conic edge seen along the sketch plane's normal. Its point at angle `t` lands at
/// `c + u·cos t + v·sin t`, `u` and `v` the projected semi-axes: an ellipse whose own axes lie
/// where that point is furthest from and nearest to `c`. Seen edge-on, the stretch of line it
/// covers.
fn projected_conic(plane: &Plane, curve: &EdgeCurve) -> Option<ModelCurve> {
    let EdgeCurve::Conic {
        center,
        x_dir,
        y_dir,
        major,
        minor,
        start,
        end,
    } = *curve
    else {
        return None;
    };
    let c = local(plane, center);
    let flat = |d: Vec3, r: f64| (r * d.dot(plane.x_dir), r * d.dot(plane.y_dir()));
    let (u, v) = (flat(x_dir, major), flat(y_dir, minor));
    let at = |t: f64| {
        Point2::new(
            c.x + u.0 * t.cos() + v.0 * t.sin(),
            c.y + u.1 * t.cos() + v.1 * t.sin(),
        )
    };
    let whole = end - start >= TAU - 1e-9;
    let dot = u.0 * v.0 + u.1 * v.1;
    let turn = 0.5 * (2.0 * dot).atan2(u.0 * u.0 + u.1 * u.1 - v.0 * v.0 - v.1 * v.1);
    let axis_end = at(turn);
    let across = at(turn + PI / 2.0);
    let ellipse = Ellipse2 {
        center: c,
        major: axis_end,
        minor: across.distance(c),
    };
    if !ellipse.is_valid() {
        // Edge-on: as far as the curve reaches either way along its line.
        let length = axis_end.distance(c);
        if length < SAME_POINT {
            return None;
        }
        let d = ((axis_end.x - c.x) / length, (axis_end.y - c.y) / length);
        let reach = |t: f64| {
            let p = at(t);
            (p.x - c.x) * d.0 + (p.y - c.y) * d.1
        };
        let far = (v.0 * d.0 + v.1 * d.1).atan2(u.0 * d.0 + u.1 * d.1);
        let (mut lo, mut hi) = (reach(start).min(reach(end)), reach(start).max(reach(end)));
        for t in [far, far + PI] {
            if whole || (t - start).rem_euclid(TAU) <= end - start {
                lo = lo.min(reach(t));
                hi = hi.max(reach(t));
            }
        }
        let point = |s: f64| Point2::new(c.x + s * d.0, c.y + s * d.1);
        return (hi - lo >= SAME_POINT).then(|| ModelCurve::Line(point(lo), point(hi)));
    }
    if whole {
        return Some(ModelCurve::Ellipse(ellipse));
    }
    let (mut first, mut last) = (at(start), at(end));
    let (a0, sweep) = ellipse.sweep(first, last);
    if (ellipse.angle_of(at(0.5 * (start + end))) - a0).rem_euclid(TAU) > sweep {
        std::mem::swap(&mut first, &mut last);
    }
    Some(ModelCurve::EllipticArc {
        ellipse,
        start: first,
        end: last,
    })
}

/// A B-spline edge seen along the sketch plane's normal: the same knots and weights on the
/// projected poles, which is exactly the projected curve.
fn projected_nurbs(plane: &Plane, curve: &EdgeCurve) -> Option<ModelCurve> {
    let EdgeCurve::Nurbs { curve, .. } = curve else {
        return None;
    };
    let poles: Vec<Point2> = curve.poles.iter().map(|p| local(plane, *p)).collect();
    let first = *poles.first()?;
    if poles.iter().all(|p| p.distance(first) < SAME_POINT) {
        return None;
    }
    let spline = Nurbs2 {
        degree: curve.degree as usize,
        poles,
        weights: curve.weights.clone(),
        knots: curve.knots.clone(),
    };
    spline
        .is_valid()
        .then(|| ModelCurve::Spline(Arc::new(spline)))
}

/// An edge's exact curve from the kernel, seen in the sketch plane.
fn projected(plane: &Plane, curve: &EdgeCurve) -> Option<ModelCurve> {
    match curve {
        EdgeCurve::Line { start, end } => {
            let (a, b) = (local(plane, *start), local(plane, *end));
            (a.distance(b) >= SAME_POINT).then_some(ModelCurve::Line(a, b))
        }
        EdgeCurve::Conic { .. } => projected_conic(plane, curve),
        EdgeCurve::Nurbs { .. } => projected_nurbs(plane, curve),
    }
}

impl ModelCurve {
    /// Its two ends, or none for a full circle or ellipse and a closed spline.
    fn ends(&self) -> Option<[Point2; 2]> {
        match self {
            ModelCurve::Line(a, b) => Some([*a, *b]),
            ModelCurve::Arc { start, end, .. } | ModelCurve::EllipticArc { start, end, .. } => {
                Some([*start, *end])
            }
            ModelCurve::Spline(spline) => {
                (!spline.is_closed()).then(|| [spline.start(), spline.end()])
            }
            ModelCurve::Circle(..) | ModelCurve::Ellipse(_) => None,
        }
    }

    /// Whether `other` is the same curve: a line either way round, a circle, or an arc (both
    /// counter-clockwise, so their ends compare in order).
    fn same_as(&self, other: &ModelCurve) -> bool {
        let near = |a: Point2, b: Point2| a.distance(b) < SAME_POINT;
        if let (ModelCurve::Spline(a), ModelCurve::Spline(b)) = (self, other) {
            let same = |a: &Nurbs2, b: &Nurbs2| {
                a.degree == b.degree
                    && a.weights == b.weights
                    && a.knots == b.knots
                    && a.poles.len() == b.poles.len()
                    && a.poles.iter().zip(&b.poles).all(|(p, q)| near(*p, *q))
            };
            return same(a, b) || same(a, &b.reversed());
        }
        match (self.clone(), other.clone()) {
            (ModelCurve::Line(a, b), ModelCurve::Line(c, d)) => {
                (near(a, c) && near(b, d)) || (near(a, d) && near(b, c))
            }
            (ModelCurve::Circle(a, r), ModelCurve::Circle(b, s)) => {
                near(a, b) && (r - s).abs() < SAME_POINT
            }
            (
                ModelCurve::Arc { center, start, end },
                ModelCurve::Arc {
                    center: c,
                    start: s,
                    end: e,
                },
            ) => near(center, c) && near(start, s) && near(end, e),
            (ModelCurve::Ellipse(a), ModelCurve::Ellipse(b)) => a.same_as(&b, SAME_POINT),
            (
                ModelCurve::EllipticArc {
                    ellipse,
                    start,
                    end,
                },
                ModelCurve::EllipticArc {
                    ellipse: other,
                    start: s,
                    end: e,
                },
            ) => ellipse.same_as(&other, SAME_POINT) && near(start, s) && near(end, e),
            _ => false,
        }
    }
}

impl ModelGeometry {
    /// The curves joined end to end with curve `index`, itself included, in no particular
    /// order: a face's outline in the sketch plane, or its projection, for Convert Entities
    /// to copy at once.
    pub fn chain_of(&self, index: usize) -> Vec<usize> {
        let near = |a: Point2, b: Point2| a.distance(b) < 1e-6;
        let mut chain = vec![index];
        let mut k = 0;
        while let Some(&current) = chain.get(k) {
            k += 1;
            let Some(ends) = self.curves.get(current).and_then(ModelCurve::ends) else {
                continue;
            };
            for (i, curve) in self.curves.iter().enumerate() {
                if chain.contains(&i) {
                    continue;
                }
                let joined = curve
                    .ends()
                    .is_some_and(|other| ends.iter().any(|a| other.iter().any(|b| near(*a, *b))));
                if joined {
                    chain.push(i);
                }
            }
        }
        chain
    }

    /// `curve_of` gives the kernel's exact curve of an edge (by index in `topology`), asked
    /// for the edges that are neither lines nor circles.
    pub fn from_body(
        topology: &Topology,
        plane: &Plane,
        curve_of: &dyn Fn(usize) -> Option<EdgeCurve>,
    ) -> Self {
        let mut out = Self::default();
        let vertex = |i: u32| topology.vertices.get(i as usize).map(|v| v.point);
        // The edges in the plane first, so that their curves keep the first places.
        let mut away = Vec::new();
        for (index, edge) in topology.edges.iter().enumerate() {
            let ends: Vec<Point3> = edge.vertices.iter().filter_map(|i| vertex(*i)).collect();
            if !ends.iter().all(|p| on_plane(plane, *p)) || !on_plane(plane, edge.midpoint) {
                away.push((index, edge, ends));
                continue;
            }
            match (edge.curve, ends.as_slice()) {
                (CurveKind::Line, [p, q]) => {
                    let (a, b) = (local(plane, *p), local(plane, *q));
                    out.curves.push(ModelCurve::Line(a, b));
                    out.add(a, ModelPointKind::Vertex);
                    out.add(b, ModelPointKind::Vertex);
                    out.add(local(plane, edge.midpoint), ModelPointKind::Middle);
                }
                (CurveKind::Circle, _) => {
                    let Some(axis) = edge.axis else { continue };
                    // Its axis along the normal: a circle standing across the plane has its
                    // vertex and its middle on it too (two ends of a diameter).
                    if !flat(plane, &axis) || !on_plane(plane, axis.origin) {
                        continue;
                    }
                    out.add(local(plane, axis.origin), ModelPointKind::Center);
                    let curve = round_curve(plane, &axis, edge, &ends);
                    if let ModelCurve::Arc { start, end, .. } = curve {
                        out.add(start, ModelPointKind::Vertex);
                        out.add(end, ModelPointKind::Vertex);
                    }
                    out.curves.push(curve);
                }
                (CurveKind::Degenerate, _) => {}
                _ => {
                    for p in &ends {
                        out.add(local(plane, *p), ModelPointKind::Vertex);
                    }
                    if let Some(curve) = curve_of(index).and_then(|c| projected(plane, &c)) {
                        out.curves.push(curve);
                    }
                }
            }
        }
        // The other edges projected along the normal. A line square to the plane projects to
        // its vertex alone, a circle tilted to the plane to an ellipse, any other curve to what
        // its exact curve from the kernel projects to.
        for (index, edge, ends) in away {
            let curve = match (edge.curve, ends.as_slice()) {
                (CurveKind::Line, [p, q]) => {
                    let (a, b) = (local(plane, *p), local(plane, *q));
                    (a.distance(b) >= SAME_POINT).then_some(ModelCurve::Line(a, b))
                }
                (CurveKind::Circle, _) => edge.axis.and_then(|axis| {
                    if flat(plane, &axis) {
                        Some(round_curve(plane, &axis, edge, &ends))
                    } else {
                        tilted_curve(plane, &axis, edge, &ends)
                    }
                }),
                (CurveKind::Line | CurveKind::Degenerate, _) => None,
                _ => curve_of(index).and_then(|c| projected(plane, &c)),
            };
            if let Some(curve) = curve
                && !out.curves.iter().any(|c| c.same_as(&curve))
            {
                out.curves.push(curve);
            }
        }
        for v in &topology.vertices {
            let p = local(plane, v.point);
            if !out.vertices.iter().any(|q| q.distance(p) < SAME_POINT) {
                out.vertices.push(p);
            }
        }
        out
    }

    fn add(&mut self, p: Point2, kind: ModelPointKind) {
        if !self.points.iter().any(|(q, _)| q.distance(p) < 1e-9) {
            self.points.push((p, kind));
        }
    }
}

#[cfg(test)]
mod tests {
    use oxyde_geom::{Axis, Ellipse2, Nurbs2, Plane, Point2, Point3, Vec3};
    use oxyde_kernel::{CurveKind, EdgeCurve, EdgeInfo, Nurbs3, Topology, VertexInfo};

    use super::{ModelCurve, ModelGeometry, ModelPointKind, local};

    fn edge(
        curve: CurveKind,
        vertices: Vec<u32>,
        midpoint: Point3,
        axis: Option<Axis>,
    ) -> EdgeInfo {
        EdgeInfo {
            curve,
            length: 0.0,
            midpoint,
            axis,
            vertices,
            faces: Vec::new(),
        }
    }

    #[test]
    fn edges_in_the_sketch_plane_become_references() {
        // On the Top plane (XZ): a line and a hole in it, and an edge above it (left out).
        let topology = Topology {
            faces: Vec::new(),
            vertices: [
                (0.0, 0.0, 0.0),
                (10.0, 0.0, 0.0),
                (0.0, 5.0, 0.0),
                (3.0, 0.0, 4.0),
            ]
            .map(|(x, y, z)| VertexInfo {
                point: Point3::new(x, y, z),
            })
            .to_vec(),
            edges: vec![
                edge(
                    CurveKind::Line,
                    vec![0, 1],
                    Point3::new(5.0, 0.0, 0.0),
                    None,
                ),
                edge(
                    CurveKind::Line,
                    vec![0, 2],
                    Point3::new(0.0, 2.5, 0.0),
                    None,
                ),
                edge(
                    CurveKind::Circle,
                    vec![3],
                    Point3::new(3.0, 0.0, 5.0),
                    Axis::new(Point3::new(3.0, 0.0, 6.0), Vec3::Y),
                ),
                // A circle standing across the plane: its vertex and middle are on it.
                edge(
                    CurveKind::Circle,
                    vec![1],
                    Point3::new(20.0, 0.0, 0.0),
                    Axis::new(Point3::new(15.0, 0.0, 0.0), Vec3::Z),
                ),
            ],
        };
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| None);
        assert_eq!(geometry.curves.len(), 2);
        assert!(matches!(geometry.curves[1], ModelCurve::Circle(_, r) if (r - 1.0).abs() < 1e-9));
        let centers: Vec<Point2> = geometry
            .points
            .iter()
            .filter(|(_, k)| *k == ModelPointKind::Center)
            .map(|(p, _)| *p)
            .collect();
        assert_eq!(centers.len(), 1);
        assert_eq!(
            geometry
                .points
                .iter()
                .filter(|(_, k)| *k == ModelPointKind::Middle)
                .count(),
            1
        );
    }

    #[test]
    fn edges_and_vertices_off_the_plane_are_projected_onto_it() {
        // On the Top plane a model point (x, y, z) projects to sketch (x, -z): the plane's x
        // direction is +X and its y direction is normal × x = -Z.
        let topology = Topology {
            faces: Vec::new(),
            vertices: [
                (0.0, 0.0, 0.0),
                (10.0, 0.0, 0.0),
                // The edge 0-1 again, 5 mm above the plane.
                (0.0, 5.0, 0.0),
                (10.0, 5.0, 0.0),
                // A slanted edge above the plane.
                (2.0, 7.0, 3.0),
                (8.0, 9.0, 3.0),
                // Ends of a half circle of radius 2 about a vertical axis, 9 mm up.
                (7.0, 9.0, 5.0),
                (3.0, 9.0, 5.0),
                // The vertex of a circle seen edge-on.
                (20.0, 5.0, 0.0),
            ]
            .map(|(x, y, z)| VertexInfo {
                point: Point3::new(x, y, z),
            })
            .to_vec(),
            edges: vec![
                edge(
                    CurveKind::Line,
                    vec![0, 1],
                    Point3::new(5.0, 0.0, 0.0),
                    None,
                ),
                edge(
                    CurveKind::Line,
                    vec![2, 3],
                    Point3::new(5.0, 5.0, 0.0),
                    None,
                ),
                // Square to the plane: its vertex only.
                edge(
                    CurveKind::Line,
                    vec![0, 2],
                    Point3::new(0.0, 2.5, 0.0),
                    None,
                ),
                edge(
                    CurveKind::Line,
                    vec![4, 5],
                    Point3::new(5.0, 8.0, 3.0),
                    None,
                ),
                edge(
                    CurveKind::Circle,
                    vec![6, 7],
                    Point3::new(5.0, 9.0, 7.0),
                    Axis::new(Point3::new(5.0, 9.0, 5.0), Vec3::Y),
                ),
                // Its axis in the plane: seen edge-on, across its diameter.
                edge(
                    CurveKind::Circle,
                    vec![8],
                    Point3::new(20.0, 1.0, 0.0),
                    Axis::new(Point3::new(20.0, 3.0, 0.0), Vec3::X),
                ),
                edge(
                    CurveKind::BSpline,
                    vec![4, 6],
                    Point3::new(5.0, 8.0, 4.0),
                    None,
                ),
            ],
        };
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| None);
        let near = |a: Point2, b: Point2| a.distance(b) < 1e-9;
        // The edge in the plane, the slanted edge, the half circle and the circle seen
        // edge-on: the copy of edge 0-1 above the plane is the same curve and is listed once.
        assert_eq!(geometry.curves.len(), 4, "{:?}", geometry.curves);
        assert!(matches!(
            geometry.curves[0],
            ModelCurve::Line(a, b) if near(a, Point2::new(0.0, 0.0)) && near(b, Point2::new(10.0, 0.0))
        ));
        assert!(matches!(
            geometry.curves[1],
            ModelCurve::Line(a, b) if near(a, Point2::new(2.0, -3.0)) && near(b, Point2::new(8.0, -3.0))
        ));
        let ModelCurve::Arc { center, start, end } = geometry.curves[2] else {
            panic!(
                "the half circle projects to an arc: {:?}",
                geometry.curves[2]
            );
        };
        assert!(near(center, Point2::new(5.0, -5.0)));
        assert!((start.distance(center) - 2.0).abs() < 1e-9);
        assert!((end.distance(center) - 2.0).abs() < 1e-9);
        // Counter-clockwise through the projected middle (5, -7).
        assert!(super::on_ccw_arc(
            center,
            start,
            end,
            Point2::new(5.0, -7.0)
        ));
        // The circle about X seen from above: the line x = 20 from z = -2 to 2.
        assert!(matches!(
            geometry.curves[3],
            ModelCurve::Line(a, b) if (near(a, Point2::new(20.0, -2.0)) && near(b, Point2::new(20.0, 2.0)))
                || (near(a, Point2::new(20.0, 2.0)) && near(b, Point2::new(20.0, -2.0)))
        ));
        // Snapping stays on the plane: the ends and middle of edge 0-1, nothing projected.
        assert_eq!(geometry.points.len(), 3);
        assert!(
            geometry
                .points
                .iter()
                .all(|(_, k)| *k != ModelPointKind::Center)
        );
        // Every vertex once: 2 and 3 project onto 0 and 1.
        let expected = [
            (0.0, 0.0),
            (10.0, 0.0),
            (2.0, -3.0),
            (8.0, -3.0),
            (7.0, -5.0),
            (3.0, -5.0),
            (20.0, 0.0),
        ];
        assert_eq!(geometry.vertices.len(), expected.len());
        for (x, y) in expected {
            assert!(
                geometry
                    .vertices
                    .iter()
                    .any(|v| near(*v, Point2::new(x, y))),
                "({x}, {y}) in {:?}",
                geometry.vertices
            );
        }
    }

    /// A circle of radius 2 about an axis through (30, 4, 0) tilted 45° from the Top plane's
    /// normal towards +Z, as a whole circle from its vertex at (32, 4, 0), or as the half from
    /// there through its highest point.
    fn tilted_circle(half: bool) -> Topology {
        let s = std::f64::consts::FRAC_1_SQRT_2;
        let direction = Vec3::new(0.0, s, s);
        let (ends, midpoint) = if half {
            // Counter-clockwise about the axis from (32, 4, 0): through the point along
            // axis × (1, 0, 0) = (0, s, −s), to (28, 4, 0).
            (
                vec![(32.0, 4.0, 0.0), (28.0, 4.0, 0.0)],
                Point3::new(30.0, 4.0 + 2.0 * s, -2.0 * s),
            )
        } else {
            (vec![(32.0, 4.0, 0.0)], Point3::new(28.0, 4.0, 0.0))
        };
        let n = ends.len() as u32;
        Topology {
            faces: Vec::new(),
            vertices: ends
                .into_iter()
                .map(|(x, y, z)| VertexInfo {
                    point: Point3::new(x, y, z),
                })
                .collect(),
            edges: vec![edge(
                CurveKind::Circle,
                (0..n).collect(),
                midpoint,
                Axis::new(Point3::new(30.0, 4.0, 0.0), direction),
            )],
        }
    }

    #[test]
    fn a_tilted_circle_projects_to_an_ellipse() {
        let geometry = ModelGeometry::from_body(&tilted_circle(false), &Plane::TOP, &|_| None);
        let [ModelCurve::Ellipse(ellipse)] = geometry.curves[..] else {
            panic!("{:?}", geometry.curves);
        };
        // The radius across the tilt, along x; the radius times cos 45° along the tilt.
        let want = Ellipse2 {
            center: Point2::new(30.0, 0.0),
            major: Point2::new(32.0, 0.0),
            minor: 2.0 * std::f64::consts::FRAC_1_SQRT_2,
        };
        assert!(ellipse.same_as(&want, 1e-12), "{ellipse:?}");
        // Every point of the circle lands on it: the projection is exact.
        let s = std::f64::consts::FRAC_1_SQRT_2;
        for k in 0..12 {
            let t = std::f64::consts::TAU * k as f64 / 12.0;
            let p = Point3::new(
                30.0 + 2.0 * t.cos(),
                4.0 + 2.0 * s * t.sin(),
                -2.0 * s * t.sin(),
            );
            let q = local(&Plane::TOP, p);
            let (x, y) = ((q.x - 30.0) / 2.0, q.y / (2.0 * s));
            assert!((x * x + y * y - 1.0).abs() < 1e-12, "{q:?}");
        }
    }

    #[test]
    fn a_tilted_half_circle_projects_to_half_an_ellipse_through_its_middle() {
        let geometry = ModelGeometry::from_body(&tilted_circle(true), &Plane::TOP, &|_| None);
        let [
            ModelCurve::EllipticArc {
                ellipse,
                start,
                end,
            },
        ] = geometry.curves[..]
        else {
            panic!("{:?}", geometry.curves);
        };
        // The middle (30, 4 + √2, −√2) lands at (30, √2): the sketch's y is −z.
        let middle = Point2::new(30.0, std::f64::consts::SQRT_2);
        let (a0, sweep) = ellipse.sweep(start, end);
        let at = (ellipse.angle_of(middle) - a0).rem_euclid(std::f64::consts::TAU);
        assert!(at < sweep, "{start:?} {end:?}");
        assert!((sweep - std::f64::consts::PI).abs() < 1e-12);
        let ends = [start, end];
        assert!(
            ends.iter()
                .any(|p| p.distance(Point2::new(32.0, 0.0)) < 1e-12)
        );
        assert!(
            ends.iter()
                .any(|p| p.distance(Point2::new(28.0, 0.0)) < 1e-12)
        );
    }

    /// One edge of the given kind above the Top plane, its curve handed over by `curve`.
    fn one_edge(kind: CurveKind, ends: &[(f64, f64, f64)], midpoint: Point3) -> Topology {
        Topology {
            faces: Vec::new(),
            vertices: ends
                .iter()
                .map(|&(x, y, z)| VertexInfo {
                    point: Point3::new(x, y, z),
                })
                .collect(),
            edges: vec![edge(kind, (0..ends.len() as u32).collect(), midpoint, None)],
        }
    }

    #[test]
    fn a_tilted_ellipse_edge_projects_to_the_exact_ellipse() {
        // An ellipse 6 by 2 about (1, 5, 2), its long axis along (1, 1, 0)/√2 and its short
        // one along Z, so tilted to the Top plane (normal Y).
        let s = std::f64::consts::FRAC_1_SQRT_2;
        let curve = EdgeCurve::Conic {
            center: Point3::new(1.0, 5.0, 2.0),
            x_dir: Vec3::new(s, s, 0.0),
            y_dir: Vec3::new(0.0, 0.0, 1.0),
            major: 6.0,
            minor: 2.0,
            start: 0.0,
            end: std::f64::consts::TAU,
        };
        let topology = one_edge(
            CurveKind::Ellipse,
            &[(1.0 + 6.0 * s, 5.0 + 6.0 * s, 2.0)],
            Point3::new(1.0 - 6.0 * s, 5.0 - 6.0 * s, 2.0),
        );
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| Some(curve.clone()));
        let [ModelCurve::Ellipse(ellipse)] = geometry.curves[..] else {
            panic!("{:?}", geometry.curves);
        };
        // Every point of the edge lands on it.
        for k in 0..24 {
            let t = std::f64::consts::TAU * k as f64 / 24.0;
            let p = Point3::new(
                1.0 + 6.0 * s * t.cos(),
                5.0 + 6.0 * s * t.cos(),
                2.0 + 2.0 * t.sin(),
            );
            let q = super::local(&Plane::TOP, p);
            assert!(ellipse.at(ellipse.angle_of(q)).distance(q) < 1e-9, "{q:?}");
        }
        // Semi-axes 6/√2 along x and 2 along z.
        assert!((ellipse.largest_radius() - 6.0 * s).abs() < 1e-9);
        assert!((ellipse.smallest_radius() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_conic_seen_edge_on_projects_to_the_line_it_covers() {
        // An ellipse in the plane x = 3, seen from above: the stretch z = −4..4 of x = 3.
        let curve = EdgeCurve::Conic {
            center: Point3::new(3.0, 5.0, 0.0),
            x_dir: Vec3::new(0.0, 0.0, 1.0),
            y_dir: Vec3::new(0.0, 1.0, 0.0),
            major: 4.0,
            minor: 2.0,
            start: 0.0,
            end: std::f64::consts::TAU,
        };
        let topology = one_edge(
            CurveKind::Ellipse,
            &[(3.0, 5.0, 4.0)],
            Point3::new(3.0, 5.0, -4.0),
        );
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| Some(curve.clone()));
        let [ModelCurve::Line(a, b)] = geometry.curves[..] else {
            panic!("{:?}", geometry.curves);
        };
        let ends = [a, b];
        assert!(
            ends.iter()
                .any(|p| p.distance(Point2::new(3.0, -4.0)) < 1e-9)
        );
        assert!(
            ends.iter()
                .any(|p| p.distance(Point2::new(3.0, 4.0)) < 1e-9)
        );
    }

    #[test]
    fn a_spline_edge_projects_to_the_spline_on_its_projected_poles() {
        // A rational cubic wandering in space above the Top plane.
        let poles3 = [
            (0.0, 4.0, 0.0),
            (2.0, 9.0, -3.0),
            (6.0, 2.0, 1.0),
            (9.0, 7.0, -2.0),
        ];
        let curve = Nurbs3 {
            degree: 3,
            poles: poles3
                .iter()
                .map(|&(x, y, z)| Point3::new(x, y, z))
                .collect(),
            weights: vec![1.0, 0.7, 1.6, 1.0],
            knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
        };
        let topology = one_edge(
            CurveKind::BSpline,
            &[(0.0, 4.0, 0.0), (9.0, 7.0, -2.0)],
            Point3::new(4.0, 6.0, -1.0),
        );
        let edge_curve = EdgeCurve::Nurbs {
            curve: curve.clone(),
            exact: true,
        };
        let geometry =
            ModelGeometry::from_body(&topology, &Plane::TOP, &|_| Some(edge_curve.clone()));
        let [ModelCurve::Spline(ref spline)] = geometry.curves[..] else {
            panic!("{:?}", geometry.curves);
        };
        // The space curve evaluated by components: (x, y) and (z, 0) with the same weights
        // and knots.
        let component = |f: fn(&Point3) -> (f64, f64)| Nurbs2 {
            degree: 3,
            poles: curve
                .poles
                .iter()
                .map(|p| {
                    let (a, b) = f(p);
                    Point2::new(a, b)
                })
                .collect(),
            weights: curve.weights.clone(),
            knots: curve.knots.clone(),
        };
        let (xy, z) = (component(|p| (p.x, p.y)), component(|p| (p.z, 0.0)));
        for k in 0..=20 {
            let t = k as f64 / 20.0;
            let (a, b) = (xy.point_at(t), z.point_at(t));
            let q = super::local(&Plane::TOP, Point3::new(a.x, a.y, b.x));
            assert!(spline.point_at(t).distance(q) < 1e-12, "{t}");
        }
        // Without the kernel's curve, the edge gives its vertices only.
        let without = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| None);
        assert!(without.curves.is_empty());
        assert_eq!(without.vertices.len(), 2);
    }

    #[test]
    fn a_face_above_the_plane_projects_to_an_outline_converted_at_once() {
        // The top face of a 10 x 4 box standing 6 mm tall on the Top plane, alone.
        let corners = [
            (0.0, 6.0, 0.0),
            (10.0, 6.0, 0.0),
            (10.0, 6.0, 4.0),
            (0.0, 6.0, 4.0),
        ];
        let topology = Topology {
            faces: Vec::new(),
            vertices: corners
                .map(|(x, y, z)| VertexInfo {
                    point: Point3::new(x, y, z),
                })
                .to_vec(),
            edges: (0..4u32)
                .map(|i| {
                    let (a, b) = (corners[i as usize], corners[(i as usize + 1) % 4]);
                    edge(
                        CurveKind::Line,
                        vec![i, (i + 1) % 4],
                        Point3::new((a.0 + b.0) / 2.0, 6.0, (a.2 + b.2) / 2.0),
                        None,
                    )
                })
                .collect(),
        };
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| None);
        assert_eq!(geometry.curves.len(), 4);
        let mut chain = geometry.chain_of(2);
        chain.sort_unstable();
        assert_eq!(chain, vec![0, 1, 2, 3]);
    }
}
