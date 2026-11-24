//! Part edges and vertices seen in a sketch plane, and the copies Convert Entities makes of
//! them. An edge in the plane is taken as it is; any other edge and every vertex is
//! projected onto the plane along its normal, as SolidWorks does. Every projection is exact:
//! a circle tilted to the plane projects to an ellipse, an ellipse to an ellipse, and any
//! other curve, which the kernel hands over as a rational B-spline, to the B-spline on its
//! projected poles. A [`Projection`] links a copy to its source, so that the copy follows
//! the part on each rebuild ([`follow`]).

use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::sync::Arc;

use oxyde_geom::{Axis, Ellipse2, Nurbs2, Plane, Point2, Point3, Vec3};
use oxyde_kernel::{CurveKind, EdgeCurve, EdgeInfo, Topology};
use oxyde_sketch::{ConstrainedSketch, Constraint, EntityId, Geometry};

use crate::{Body, EdgeName, VertexName};

/// Edges further than this from the sketch plane are projected onto it, not snapped to (mm).
const ON_PLANE: f64 = 1e-4;

/// Two projected points closer than this are one (mm).
pub const SAME_POINT: f64 = 1e-9;

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

pub fn local(plane: &Plane, p: Point3) -> Point2 {
    let v = p - plane.origin;
    Point2::new(v.dot(plane.x_dir), v.dot(plane.y_dir()))
}

pub fn on_plane(plane: &Plane, p: Point3) -> bool {
    (p - plane.origin).dot(plane.normal).abs() < ON_PLANE
}

/// Whether a circle about `axis` lies in planes parallel to the sketch plane, so that it
/// projects onto it as a circle of the same radius.
pub fn flat(plane: &Plane, axis: &Axis) -> bool {
    axis.direction.cross(plane.normal).length() < 1e-6
}

/// Whether `p` lies on the counter-clockwise arc from `start` to `end` around `center`.
pub fn on_ccw_arc(center: Point2, start: Point2, end: Point2, p: Point2) -> bool {
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
    pub fn ends(&self) -> Option<[Point2; 2]> {
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
    pub fn same_as(&self, other: &ModelCurve) -> bool {
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

/// The curve edge `index` of `topology` projects to in the sketch plane, or none: an edge
/// in the plane as it is (a circle standing across the plane gives none), any other along
/// the plane's normal. A line square to the plane projects to its vertex alone, a circle
/// tilted to the plane to an ellipse, any other curve to what its exact curve from the
/// kernel (`curve_of`, asked for the edges that are neither lines nor circles) projects to.
pub fn project_edge(
    plane: &Plane,
    topology: &Topology,
    index: usize,
    curve_of: &dyn Fn(usize) -> Option<EdgeCurve>,
) -> Option<ModelCurve> {
    let edge = topology.edges.get(index)?;
    let ends: Vec<Point3> = edge
        .vertices
        .iter()
        .filter_map(|i| topology.vertices.get(*i as usize).map(|v| v.point))
        .collect();
    let in_plane = ends.iter().all(|p| on_plane(plane, *p)) && on_plane(plane, edge.midpoint);
    match (edge.curve, ends.as_slice()) {
        (CurveKind::Line, [p, q]) => {
            let (a, b) = (local(plane, *p), local(plane, *q));
            (in_plane || a.distance(b) >= SAME_POINT).then_some(ModelCurve::Line(a, b))
        }
        (CurveKind::Circle, _) => {
            let axis = edge.axis?;
            if in_plane && !(flat(plane, &axis) && on_plane(plane, axis.origin)) {
                return None;
            }
            if flat(plane, &axis) {
                Some(round_curve(plane, &axis, edge, &ends))
            } else {
                tilted_curve(plane, &axis, edge, &ends)
            }
        }
        (CurveKind::Degenerate, _) => None,
        (CurveKind::Line, _) if !in_plane => None,
        _ => curve_of(index).and_then(|c| projected(plane, &c)),
    }
}

/// A sketch entity copied from the part by Convert Entities, and the edge or vertex it was
/// copied from: on each rebuild the copy follows its source.
#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    /// The copy: a curve for an edge, a point for a vertex.
    pub entity: u32,
    pub source: ProjectionSource,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectionSource {
    Edge(EdgeName),
    Vertex(VertexName),
}

/// Moves the copies `projections` link into `sketch` to where their sources now project
/// onto `plane`, in `body` (the part before the sketch): the fixed points that hold each
/// copy, by their role (a line's ends; a circle's centre and rim; an arc's centre and ends;
/// an ellipse's centre, major point and rim; a spline's poles), and a circle's radius or an
/// ellipse's minor semi-axis. Solve afterwards. A link whose copy is gone is skipped. A
/// source that is no longer in the part, or that now projects to another kind of curve,
/// leaves its copy where it was, with a warning; it follows again once the source is back.
pub fn follow(
    sketch: &mut ConstrainedSketch,
    plane: &Plane,
    projections: &[Projection],
    body: Option<&Body>,
    curve_of: &dyn Fn(usize) -> Option<EdgeCurve>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut lost = 0usize;
    let mut changed = 0usize;
    for projection in projections {
        let entity = EntityId(projection.entity);
        let Some(geometry) = sketch.entity(entity).map(|e| e.geometry.clone()) else {
            continue;
        };
        match &projection.source {
            ProjectionSource::Vertex(name) => {
                let at = body
                    .and_then(|b| {
                        let i = b.naming.find_vertex(name)?;
                        b.topology.vertices.get(i as usize)
                    })
                    .map(|v| local(plane, v.point));
                match at {
                    Some(at) if matches!(geometry, Geometry::Point(_)) => {
                        sketch.move_point(entity, at);
                    }
                    Some(_) => changed += 1,
                    None => lost += 1,
                }
            }
            ProjectionSource::Edge(name) => {
                let Some(curve) = body.and_then(|b| {
                    let i = b.naming.find_edge(name)?;
                    project_edge(plane, &b.topology, i as usize, curve_of)
                }) else {
                    lost += 1;
                    continue;
                };
                if !move_copy(sketch, entity, &geometry, &curve) {
                    changed += 1;
                }
            }
        }
    }
    if lost > 0 {
        warnings.push(format!(
            "{lost} converted edge(s) or vertex(es) no longer in the part: their copies stay \
             where they were"
        ));
    }
    if changed > 0 {
        warnings.push(format!(
            "{changed} converted edge(s) now project to another kind of curve: their copies \
             stay where they were"
        ));
    }
    warnings
}

/// Moves the copy `entity` (whose geometry is `geometry`) onto `curve`. False when the copy
/// is another kind of curve, or a spline of other poles, knots or weights.
fn move_copy(
    sketch: &mut ConstrainedSketch,
    entity: EntityId,
    geometry: &Geometry,
    curve: &ModelCurve,
) -> bool {
    let at = |sketch: &ConstrainedSketch, id: EntityId| sketch.position(id).unwrap_or_default();
    match (geometry, curve) {
        (Geometry::Line { start, end }, ModelCurve::Line(a, b)) => {
            let (s, e) = (at(sketch, *start), at(sketch, *end));
            let (a, b) = if s.distance(*a) + e.distance(*b) <= s.distance(*b) + e.distance(*a) {
                (*a, *b)
            } else {
                (*b, *a)
            };
            sketch.move_point(*start, a);
            sketch.move_point(*end, b);
            true
        }
        (Geometry::Circle { center, .. }, ModelCurve::Circle(c, r)) => {
            let old = at(sketch, *center);
            sketch.move_point(*center, *c);
            sketch.set_circle_radius(entity, *r);
            for rim in rims(sketch, entity) {
                let p = at(sketch, rim);
                let (dx, dy) = (p.x - old.x, p.y - old.y);
                let length = dx.hypot(dy);
                let to = if length > SAME_POINT {
                    Point2::new(c.x + r * dx / length, c.y + r * dy / length)
                } else {
                    Point2::new(c.x + r, c.y)
                };
                sketch.move_point(rim, to);
            }
            true
        }
        (
            Geometry::Arc { center, start, end },
            ModelCurve::Arc {
                center: c,
                start: s,
                end: e,
            },
        ) => {
            sketch.move_point(*center, *c);
            sketch.move_point(*start, *s);
            sketch.move_point(*end, *e);
            true
        }
        (Geometry::Ellipse { center, major, .. }, ModelCurve::Ellipse(ellipse)) => {
            sketch.move_point(*center, ellipse.center);
            sketch.move_point(*major, ellipse.major);
            sketch.set_minor(entity, ellipse.minor);
            for rim in rims(sketch, entity) {
                sketch.move_point(rim, ellipse.at(FRAC_PI_2));
            }
            true
        }
        (
            Geometry::EllipticArc {
                center,
                major,
                start,
                end,
                ..
            },
            ModelCurve::EllipticArc {
                ellipse,
                start: s,
                end: e,
            },
        ) => {
            sketch.move_point(*center, ellipse.center);
            sketch.move_point(*major, ellipse.major);
            sketch.set_minor(entity, ellipse.minor);
            sketch.move_point(*start, *s);
            sketch.move_point(*end, *e);
            for rim in rims(sketch, entity) {
                sketch.move_point(rim, ellipse.at(FRAC_PI_2));
            }
            true
        }
        (Geometry::Spline(def), ModelCurve::Spline(spline)) => {
            if def.poles.len() != spline.poles.len()
                || def.weights != spline.weights
                || def.knots != spline.knots
            {
                return false;
            }
            for (pole, to) in def.poles.iter().zip(&spline.poles) {
                sketch.move_point(*pole, *to);
            }
            true
        }
        _ => false,
    }
}

/// The fixed points held on the circle or ellipse `round`: what holds a converted circle's
/// radius or ellipse's minor semi-axis.
fn rims(sketch: &ConstrainedSketch, round: EntityId) -> Vec<EntityId> {
    sketch
        .relations()
        .iter()
        .filter_map(|r| match r.constraint {
            Constraint::PointOnCircle(p, c) | Constraint::PointOnEllipse(p, c) if c == round => {
                Some(p)
            }
            _ => None,
        })
        .filter(|p| sketch.is_fixed(*p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(sketch: &mut ConstrainedSketch, at: Point2) -> EntityId {
        let id = sketch.point(at);
        sketch.constrain(Constraint::Fixed(id));
        id
    }

    fn near(a: Point2, b: Point2) -> bool {
        a.distance(b) < 1e-9
    }

    #[test]
    fn a_line_copy_follows_its_edge_the_nearer_way_round() {
        let mut sketch = ConstrainedSketch::new();
        let a = fixed(&mut sketch, Point2::new(0.0, 0.0));
        let b = fixed(&mut sketch, Point2::new(10.0, 0.0));
        let line = sketch.line_between(a, b);
        let geometry = sketch.entity(line).unwrap().geometry.clone();
        // The edge, now 12 long, comes from the kernel the other way round.
        let curve = ModelCurve::Line(Point2::new(12.0, 0.0), Point2::new(0.0, 0.0));
        assert!(move_copy(&mut sketch, line, &geometry, &curve));
        assert!(near(sketch.position(a).unwrap(), Point2::new(0.0, 0.0)));
        assert!(near(sketch.position(b).unwrap(), Point2::new(12.0, 0.0)));
    }

    #[test]
    fn a_circle_copy_follows_with_its_rim() {
        let mut sketch = ConstrainedSketch::new();
        let c = fixed(&mut sketch, Point2::new(0.0, 0.0));
        let circle = sketch.circle_around(c, 4.0);
        let rim = fixed(&mut sketch, Point2::new(4.0, 0.0));
        sketch.constrain(Constraint::PointOnCircle(rim, circle));
        let geometry = sketch.entity(circle).unwrap().geometry.clone();
        let curve = ModelCurve::Circle(Point2::new(5.0, 1.0), 6.0);
        assert!(move_copy(&mut sketch, circle, &geometry, &curve));
        assert!(near(sketch.position(c).unwrap(), Point2::new(5.0, 1.0)));
        assert!(near(sketch.position(rim).unwrap(), Point2::new(11.0, 1.0)));
        assert_eq!(sketch.radius_of(circle), Some(6.0));
        assert!(sketch.solve().unwrap().converged);
    }

    #[test]
    fn an_ellipse_copy_follows_with_its_minor_semi_axis() {
        let mut sketch = ConstrainedSketch::new();
        let c = fixed(&mut sketch, Point2::new(0.0, 0.0));
        let m = fixed(&mut sketch, Point2::new(5.0, 0.0));
        let ellipse = sketch.ellipse_around(c, m, 2.0);
        let rim = fixed(&mut sketch, Point2::new(0.0, 2.0));
        sketch.constrain(Constraint::PointOnEllipse(rim, ellipse));
        let geometry = sketch.entity(ellipse).unwrap().geometry.clone();
        let target = Ellipse2 {
            center: Point2::new(1.0, 1.0),
            major: Point2::new(1.0, 7.0),
            minor: 3.0,
        };
        assert!(move_copy(
            &mut sketch,
            ellipse,
            &geometry,
            &ModelCurve::Ellipse(target)
        ));
        let now = sketch.ellipse_of(ellipse).unwrap();
        assert!(now.same_as(&target, 1e-9));
        assert!(near(sketch.position(rim).unwrap(), target.at(FRAC_PI_2)));
        assert!(sketch.solve().unwrap().converged);
    }

    #[test]
    fn a_copy_of_another_kind_of_curve_stays() {
        let mut sketch = ConstrainedSketch::new();
        let a = fixed(&mut sketch, Point2::new(0.0, 0.0));
        let b = fixed(&mut sketch, Point2::new(10.0, 0.0));
        let line = sketch.line_between(a, b);
        let geometry = sketch.entity(line).unwrap().geometry.clone();
        let curve = ModelCurve::Circle(Point2::new(5.0, 5.0), 2.0);
        assert!(!move_copy(&mut sketch, line, &geometry, &curve));
        assert!(near(sketch.position(b).unwrap(), Point2::new(10.0, 0.0)));
    }

    #[test]
    fn copies_whose_sources_are_gone_stay_with_a_warning() {
        let mut sketch = ConstrainedSketch::new();
        let a = fixed(&mut sketch, Point2::new(0.0, 0.0));
        let b = fixed(&mut sketch, Point2::new(10.0, 0.0));
        let line = sketch.line_between(a, b);
        let edge = EdgeName {
            faces: Vec::new(),
            part: 0,
        };
        let projections = vec![
            Projection {
                entity: line.0,
                source: ProjectionSource::Edge(edge.clone()),
            },
            // A link whose copy was deleted is skipped.
            Projection {
                entity: 999,
                source: ProjectionSource::Edge(edge),
            },
        ];
        let before = sketch.clone();
        let warnings = follow(&mut sketch, &Plane::FRONT, &projections, None, &|_| None);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("1 converted"), "{warnings:?}");
        assert_eq!(sketch, before);
    }
}
