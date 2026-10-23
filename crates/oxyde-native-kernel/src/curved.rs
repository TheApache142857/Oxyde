//! Booleans of solids with planar and cylindrical faces, by cutting faces along the curves
//! where they meet.
//!
//! Each face is laid out flat in its own chart (a plane's coordinates, or a cylinder's
//! `(radius × angle, height)`, where everything the kernel meets is straight), cut along the
//! curves it shares with the other solid's faces ([`crate::arrangement`]), and every piece
//! is kept or dropped by where a point inside it lies: inside the other solid, outside it,
//! or on one of its faces facing the same way or the other. The pieces kept are welded at
//! their corners, the pieces lying on one surface merged back into faces (a cylinder's face
//! that goes round whole cut open along its seam at its angle zero, one that does not left
//! whole), and edges along one line or circle between the same two faces joined, as
//! OpenCASCADE's `SimplifyResult` leaves them.
//!
//! What it meets in lines and circles only: planes, and cylinders whose axes are square to
//! the planes they meet or along them, and parallel to the other cylinders. Anything else
//! answers [`NativeError::Unsupported`].

// Indices into the points, edges and faces gathered here.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;
use std::f64::consts::TAU;

use oxyde_geom::{LINEAR_TOLERANCE, Point3, Vec3};
use oxyde_kernel::{BooleanOp, FaceSource, OpHistory};

use crate::NativeError;
use crate::arrangement::{self, Arrangement, Geom, P, Segment};
use crate::brep::{Edge, EdgeUse, Face, Shape, Solid};
use crate::geometry::{Circle, Curve, Cylinder, Surface};

/// An input face a face of the result is made of: the input's position in the boolean's
/// arguments and the face's index in it.
type Source = (u32, u32);

/// Points closer than this are one.
const TOL: f64 = 10.0 * LINEAR_TOLERANCE;

/// A shape with, for each of its faces (solid after solid), the input faces it is made of.
#[derive(Debug, Clone)]
struct Tagged {
    shape: Shape,
    sources: Vec<Vec<Source>>,
}

/// `target` (input 0) fused with, cut by or in common with `tools` (inputs 1, 2…), as
/// [`crate::boolean::boolean`] does for straight-sided solids.
pub(crate) fn boolean(
    op: BooleanOp,
    target: &Shape,
    tools: &[&Shape],
) -> Result<(Shape, OpHistory), NativeError> {
    let tag = |shape: &Shape, input: u32| Tagged {
        shape: shape.clone(),
        sources: (0..shape.face_count() as u32)
            .map(|f| vec![(input, f)])
            .collect(),
    };
    let target = tag(target, 0);
    let tools: Vec<Tagged> = tools
        .iter()
        .enumerate()
        .map(|(i, t)| tag(t, i as u32 + 1))
        .collect();
    let result = match op {
        BooleanOp::Fuse | BooleanOp::Cut => {
            let mut acc = target;
            for tool in &tools {
                acc = pairwise(op, &acc, tool)?;
            }
            acc
        }
        BooleanOp::Common => {
            let Some((first, rest)) = tools.split_first() else {
                return Err(NativeError::Failed("a boolean needs a tool".to_owned()));
            };
            let mut union = first.clone();
            for tool in rest {
                union = pairwise(BooleanOp::Fuse, &union, tool)?;
            }
            pairwise(BooleanOp::Common, &target, &union)?
        }
    };
    let history = OpHistory {
        faces: result
            .sources
            .iter()
            .map(|sources| {
                sources
                    .iter()
                    .map(|&(input, face)| FaceSource::InputFace { input, face })
                    .collect()
            })
            .collect(),
    };
    Ok((result.shape, history))
}

// ---------------------------------------------------------------------------------------
// Charts: a face laid out flat.

/// A surface's flat coordinates: a plane's own, or a cylinder's `x = radius × angle`
/// (counter-clockwise about its axis from its angle zero) and `y = height` along its axis.
#[derive(Debug, Clone, Copy)]
enum Chart {
    Plane {
        origin: Point3,
        x: Vec3,
        y: Vec3,
        normal: Vec3,
    },
    Cylinder(Cylinder),
}

impl Chart {
    fn of(surface: &Surface) -> Chart {
        match surface {
            Surface::Plane(plane) => Chart::Plane {
                origin: plane.origin,
                x: plane.x_dir,
                y: plane.normal.cross(plane.x_dir),
                normal: plane.normal,
            },
            Surface::Cylinder(cylinder) => Chart::Cylinder(*cylinder),
        }
    }

    /// The length a whole turn takes along `x`: none for a plane.
    fn period(&self) -> Option<f64> {
        match self {
            Chart::Plane { .. } => None,
            Chart::Cylinder(c) => Some(TAU * c.radius),
        }
    }

    fn to_world(self, p: P) -> Point3 {
        match self {
            Chart::Plane { origin, x, y, .. } => origin + x * p.x + y * p.y,
            Chart::Cylinder(c) => c.point(p.x / c.radius, p.y),
        }
    }

    /// `p` in the chart: for a cylinder, its angle taken in `[0, 2π)`.
    fn to_2d(self, p: Point3) -> P {
        match self {
            Chart::Plane { origin, x, y, .. } => {
                let d = p - origin;
                P::new(d.dot(x), d.dot(y))
            }
            Chart::Cylinder(c) => {
                let (angle, height) = c.coordinates(p);
                P::new(angle.rem_euclid(TAU) * c.radius, height)
            }
        }
    }

    /// The curve a segment of the chart runs along in space, from its start to its end.
    fn curve(&self, s: &Segment) -> Result<Curve, String> {
        match (self, s.geom) {
            (Chart::Plane { .. }, Geom::Line) => Ok(Curve::Line),
            (
                Chart::Plane { normal, .. },
                Geom::Arc {
                    center,
                    radius,
                    ccw,
                },
            ) => Ok(Curve::Circle(Circle {
                center: self.to_world(center),
                normal: if ccw { *normal } else { -*normal },
                radius,
            })),
            (Chart::Cylinder(c), Geom::Line) => {
                if (s.a.x - s.b.x).abs() <= TOL {
                    Ok(Curve::Line)
                } else if (s.a.y - s.b.y).abs() <= TOL {
                    let a = c.axis.direction;
                    Ok(Curve::Circle(Circle {
                        center: c.axis.origin + a * s.a.y,
                        normal: if s.b.x > s.a.x { a } else { -a },
                        radius: c.radius,
                    }))
                } else {
                    Err("a cut across a cylinder that is neither along nor round it".to_owned())
                }
            }
            (Chart::Cylinder(_), Geom::Arc { .. }) => {
                Err("an arc in a cylinder's chart".to_owned())
            }
        }
    }

    /// The outward normal of the face at `p` (in space).
    fn normal_at(&self, p: Point3) -> Vec3 {
        match self {
            Chart::Plane { normal, .. } => *normal,
            Chart::Cylinder(c) => {
                let r = c.radial(p).normalized().unwrap_or(c.x_dir);
                if c.outward { r } else { -r }
            }
        }
    }
}

/// An edge in space, from `a` to `b`: a line, or an arc counter-clockwise about its
/// circle's normal (a whole circle when `a` is `b`).
#[derive(Debug, Clone, Copy)]
struct Edge3 {
    a: Point3,
    b: Point3,
    curve: Curve,
}

impl Edge3 {
    fn reversed(&self) -> Edge3 {
        Edge3 {
            a: self.b,
            b: self.a,
            curve: match self.curve {
                Curve::Line => Curve::Line,
                Curve::Circle(c) => Curve::Circle(Circle {
                    normal: -c.normal,
                    ..c
                }),
            },
        }
    }

    fn closed(&self) -> bool {
        self.a.distance(self.b) <= TOL
    }

    /// The angle it turns through (a circle), its length (a line).
    fn extent(&self) -> f64 {
        match self.curve {
            Curve::Line => self.a.distance(self.b),
            Curve::Circle(c) => c.sweep(self.a, self.b, self.closed()),
        }
    }

    fn midpoint(&self) -> Point3 {
        match self.curve {
            Curve::Line => self.a + (self.b - self.a) * 0.5,
            Curve::Circle(c) => c.point_at(c.angle_of(self.a) + self.extent() / 2.0),
        }
    }

    /// Where `p` is along it, strictly between its ends: `None` when it is off it, or at
    /// an end.
    fn inner(&self, p: Point3) -> Option<f64> {
        if p.distance(self.a) <= TOL || p.distance(self.b) <= TOL {
            return None;
        }
        match self.curve {
            Curve::Line => {
                let d = self.b - self.a;
                let l = d.length();
                let t = (p - self.a).dot(d) / (l * l);
                let off = (p - (self.a + d * t)).length();
                (off <= TOL && t > 0.0 && t < 1.0).then_some(t * l)
            }
            Curve::Circle(c) => {
                let d = p - c.center;
                let along = d.dot(c.normal);
                let r = (d - c.normal * along).length();
                if along.abs() > TOL || (r - c.radius).abs() > TOL {
                    return None;
                }
                let s = (c.angle_of(p) - c.angle_of(self.a)).rem_euclid(TAU);
                (s > 0.0 && s < self.extent()).then_some(s)
            }
        }
    }
}

/// The arrangement's regions run counter-clockwise in a chart, which is outward for a plane
/// and for a cylinder whose normal points away from its axis; a cylinder whose normal points
/// towards it (a hole's wall) runs the other way.
fn outward_loops(chart: &Chart, loops: Vec<Vec<Edge3>>) -> Vec<Vec<Edge3>> {
    match chart {
        Chart::Cylinder(c) if !c.outward => loops
            .into_iter()
            .map(|l| l.iter().rev().map(Edge3::reversed).collect())
            .collect(),
        _ => loops,
    }
}

/// A face's boundary in its chart, walked as its loops run: for a cylinder, the angle
/// unwrapped as it goes round, so that a face crossing angle zero stays in one piece and a
/// whole cylinder's face runs a whole period along `x`.
fn boundary_2d(chart: &Chart, loops: &[Vec<Edge3>]) -> Result<Vec<Segment>, String> {
    let mut out = Vec::new();
    for edges in loops {
        match chart {
            Chart::Plane { normal, .. } => {
                for e in edges {
                    let (a, b) = (chart.to_2d(e.a), chart.to_2d(e.b));
                    out.push(match e.curve {
                        Curve::Line => Segment::line(a, b),
                        Curve::Circle(c) => {
                            Segment::arc(a, b, chart.to_2d(c.center), c.normal.dot(*normal) > 0.0)
                        }
                    });
                }
            }
            Chart::Cylinder(c) => {
                let Some(first) = edges.first() else {
                    continue;
                };
                let mut x = chart.to_2d(first.a).x;
                for e in edges {
                    let ya = (e.a - c.axis.origin).dot(c.axis.direction);
                    let yb = (e.b - c.axis.origin).dot(c.axis.direction);
                    match e.curve {
                        Curve::Line => out.push(Segment::line(P::new(x, ya), P::new(x, yb))),
                        Curve::Circle(circle) => {
                            let mut turn = e.extent();
                            if circle.normal.dot(c.axis.direction) < 0.0 {
                                turn = -turn;
                            }
                            let to = x + turn * c.radius;
                            out.push(Segment::line(P::new(x, ya), P::new(to, ya)));
                            x = to;
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// The faces of a solid, prepared for cutting and for telling where points are.

/// A face as the boolean works on it.
#[derive(Debug, Clone)]
struct Prepared {
    surface: Surface,
    chart: Chart,
    loops: Vec<Vec<Edge3>>,
    /// Its boundary in its chart.
    boundary: Vec<Segment>,
    /// The chart's box round the boundary.
    lo: P,
    hi: P,
    /// Its box in space.
    min: Point3,
    max: Point3,
    sources: Vec<Source>,
}

fn prepare(tagged: &Tagged) -> Result<Vec<Prepared>, NativeError> {
    let internal = NativeError::Internal;
    let mut out = Vec::new();
    let mut index = 0;
    for solid in &tagged.shape.solids {
        for face in &solid.faces {
            let loops: Vec<Vec<Edge3>> = face
                .loops
                .iter()
                .map(|uses| uses.iter().map(|&u| edge3(solid, u)).collect())
                .collect();
            let chart = Chart::of(&face.surface);
            let boundary = boundary_2d(&chart, &loops).map_err(internal)?;
            let (lo, hi) = box_2d(&boundary);
            let (min, max) = box_3d(&loops);
            out.push(Prepared {
                surface: face.surface,
                chart,
                loops,
                boundary,
                lo,
                hi,
                min,
                max,
                sources: tagged.sources.get(index).cloned().unwrap_or_default(),
            });
            index += 1;
        }
    }
    Ok(out)
}

fn edge3(solid: &Solid, u: EdgeUse) -> Edge3 {
    let edge = solid.edges[u.edge as usize];
    let e = Edge3 {
        a: solid.point(edge.start),
        b: solid.point(edge.end),
        curve: edge.curve,
    };
    if u.reversed { e.reversed() } else { e }
}

fn box_2d(segments: &[Segment]) -> (P, P) {
    let mut lo = P::new(f64::INFINITY, f64::INFINITY);
    let mut hi = P::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    for s in segments {
        let n = match s.geom {
            Geom::Line => 1,
            Geom::Arc { .. } => 32,
        };
        let extent = s.extent(TOL);
        for i in 0..=n {
            let p = s.at(extent * i as f64 / n as f64, TOL);
            lo = P::new(lo.x.min(p.x), lo.y.min(p.y));
            hi = P::new(hi.x.max(p.x), hi.y.max(p.y));
        }
    }
    (lo, hi)
}

fn box_3d(loops: &[Vec<Edge3>]) -> (Point3, Point3) {
    let mut points = Vec::new();
    for e in loops.iter().flatten() {
        points.push(e.a);
        if let Curve::Circle(c) = e.curve {
            points.extend(c.extremes(c.angle_of(e.a), e.extent()));
        }
    }
    let mut min = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut max = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in points {
        min = Point3::new(min.x.min(p.x), min.y.min(p.y), min.z.min(p.z));
        max = Point3::new(max.x.max(p.x), max.y.max(p.y), max.z.max(p.z));
    }
    (min, max)
}

fn boxes_meet(a: &Prepared, b: &Prepared) -> bool {
    a.min.x <= b.max.x + TOL
        && b.min.x <= a.max.x + TOL
        && a.min.y <= b.max.y + TOL
        && b.min.y <= a.max.y + TOL
        && a.min.z <= b.max.z + TOL
        && b.min.z <= a.max.z + TOL
}

/// The parameter range of the line `p + t d` (`d` unit) inside a box: exactly, so that a
/// cut does not run past the faces it lies on (a hair past, it would meet the curves they
/// end on there as a sliver).
fn clip_line(p: Point3, d: Vec3, min: Point3, max: Point3, range: (f64, f64)) -> (f64, f64) {
    let (mut t0, mut t1) = range;
    for (o, v, lo, hi) in [
        (p.x, d.x, min.x, max.x),
        (p.y, d.y, min.y, max.y),
        (p.z, d.z, min.z, max.z),
    ] {
        if v.abs() < 1e-12 {
            if o < lo - TOL || o > hi + TOL {
                return (1.0, 0.0);
            }
        } else {
            let (a, b) = ((lo - o) / v, (hi - o) / v);
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
        }
    }
    (t0, t1)
}

// ---------------------------------------------------------------------------------------
// Where two faces meet.

/// A curve two faces share, to cut both along.
#[derive(Debug, Clone)]
enum Cut {
    /// A segment of a line.
    Line(Point3, Point3),
    /// A whole circle, starting at `start` (the point at the cylinder's angle zero, where
    /// its seam will meet it).
    Circle(Circle, Point3),
    /// The two faces lie on one surface: each is cut along the other's boundary.
    Same,
}

/// The curves where faces `a` and `b` meet, within their boxes.
fn meet(a: &Prepared, b: &Prepared) -> Result<Vec<Cut>, NativeError> {
    let slanted = |what: &'static str| Err(NativeError::Unsupported(what));
    match (&a.surface, &b.surface) {
        (Surface::Plane(p), Surface::Plane(q)) => {
            let u = p.normal.cross(q.normal);
            if u.length() < 1e-9 {
                let gap = (q.origin - p.origin).dot(p.normal).abs();
                return Ok(if gap <= TOL {
                    vec![Cut::Same]
                } else {
                    Vec::new()
                });
            }
            let (d1, d2) = (
                p.normal.dot(p.origin.to_vec()),
                q.normal.dot(q.origin.to_vec()),
            );
            let point = (q.normal.cross(u) * d1 + u.cross(p.normal) * d2) * (1.0 / u.dot(u));
            let origin = Point3::new(point.x, point.y, point.z);
            let dir = u.normalized().unwrap_or(u);
            let range = clip_line(
                origin,
                dir,
                a.min,
                a.max,
                (f64::NEG_INFINITY, f64::INFINITY),
            );
            let (t0, t1) = clip_line(origin, dir, b.min, b.max, range);
            Ok(if t1 - t0 > TOL {
                vec![Cut::Line(origin + dir * t0, origin + dir * t1)]
            } else {
                Vec::new()
            })
        }
        (Surface::Plane(_), Surface::Cylinder(_)) => plane_cylinder(a, b),
        (Surface::Cylinder(_), Surface::Plane(_)) => plane_cylinder(b, a),
        (Surface::Cylinder(c1), Surface::Cylinder(c2)) => {
            let (a1, a2) = (c1.axis.direction, c2.axis.direction);
            if a1.cross(a2).length() > 1e-9 {
                return slanted("intersect cylinders whose axes are not parallel");
            }
            let offset = c1.radial(c2.axis.origin);
            let dist = offset.length();
            if dist <= TOL {
                return Ok(if (c1.radius - c2.radius).abs() <= TOL {
                    vec![Cut::Same]
                } else {
                    Vec::new()
                });
            }
            let (r1, r2) = (c1.radius, c2.radius);
            if dist > r1 + r2 + TOL || dist < (r1 - r2).abs() - TOL {
                return Ok(Vec::new());
            }
            // The circles across the axes, in the plane square to them through c1's origin.
            let along = (r1 * r1 - r2 * r2 + dist * dist) / (2.0 * dist);
            let half = (r1 * r1 - along * along).max(0.0).sqrt();
            let u = offset * (1.0 / dist);
            let n = a1.cross(u);
            let mut cuts = Vec::new();
            for side in if half <= TOL {
                vec![0.0]
            } else {
                vec![half, -half]
            } {
                let base = c1.axis.origin + u * along + n * side;
                let range = clip_line(base, a1, a.min, a.max, (f64::NEG_INFINITY, f64::INFINITY));
                let (t0, t1) = clip_line(base, a1, b.min, b.max, range);
                if t1 - t0 > TOL {
                    cuts.push(Cut::Line(base + a1 * t0, base + a1 * t1));
                }
            }
            Ok(cuts)
        }
    }
}

fn plane_cylinder(
    plane_face: &Prepared,
    cylinder_face: &Prepared,
) -> Result<Vec<Cut>, NativeError> {
    let (Surface::Plane(p), Surface::Cylinder(c)) = (&plane_face.surface, &cylinder_face.surface)
    else {
        return Ok(Vec::new());
    };
    let a = c.axis.direction;
    let along = p.normal.dot(a);
    if along.abs() > 1.0 - 1e-9 {
        // Square to the axis: a circle.
        let t = (p.origin - c.axis.origin).dot(p.normal) / along;
        let (lo, hi) = (cylinder_face.lo.y, cylinder_face.hi.y);
        if t < lo - TOL || t > hi + TOL {
            return Ok(Vec::new());
        }
        let circle = Circle {
            center: c.axis.origin + a * t,
            normal: a,
            radius: c.radius,
        };
        let (cmin, cmax) = box_3d(&[vec![Edge3 {
            a: c.point(0.0, t),
            b: c.point(0.0, t),
            curve: Curve::Circle(circle),
        }]]);
        let touches = cmin.x <= plane_face.max.x + TOL
            && plane_face.min.x <= cmax.x + TOL
            && cmin.y <= plane_face.max.y + TOL
            && plane_face.min.y <= cmax.y + TOL
            && cmin.z <= plane_face.max.z + TOL
            && plane_face.min.z <= cmax.z + TOL;
        return Ok(if touches {
            vec![Cut::Circle(circle, c.point(0.0, t))]
        } else {
            Vec::new()
        });
    }
    if along.abs() > 1e-9 {
        return Err(NativeError::Unsupported(
            "intersect a cylinder with a plane slanted to its axis",
        ));
    }
    // Along the axis: lines where the plane crosses the cylinder.
    let d = (c.axis.origin - p.origin).dot(p.normal);
    if d.abs() > c.radius + TOL {
        return Ok(Vec::new());
    }
    let (x, y, _) = c.frame();
    let phi = p.normal.dot(y).atan2(p.normal.dot(x));
    let delta = (-d / c.radius).clamp(-1.0, 1.0).acos();
    let angles = if delta <= 1e-9 || (std::f64::consts::PI - delta) <= 1e-9 {
        vec![phi + delta]
    } else {
        vec![phi + delta, phi - delta]
    };
    let mut cuts = Vec::new();
    for angle in angles {
        let base = c.point(angle, 0.0);
        let range = (cylinder_face.lo.y, cylinder_face.hi.y);
        let (t0, t1) = clip_line(base, a, plane_face.min, plane_face.max, range);
        if t1 - t0 > TOL {
            cuts.push(Cut::Line(base + a * t0, base + a * t1));
        }
    }
    Ok(cuts)
}

/// A cut as segments of `face`'s chart (several for a cylinder's face that runs round
/// more than once along its unwrapped `x`).
fn cut_into(face: &Prepared, cut: &Cut, other: &Prepared) -> Vec<Segment> {
    let chart = &face.chart;
    match (cut, chart) {
        (Cut::Line(p, q), Chart::Plane { .. }) => {
            vec![Segment::line(chart.to_2d(*p), chart.to_2d(*q))]
        }
        (Cut::Circle(circle, start), Chart::Plane { normal, .. }) => {
            let a = chart.to_2d(*start);
            vec![Segment::arc(
                a,
                a,
                chart.to_2d(circle.center),
                circle.normal.dot(*normal) > 0.0,
            )]
        }
        (Cut::Line(p, q), Chart::Cylinder(_)) => {
            let (a, b) = (chart.to_2d(*p), chart.to_2d(*q));
            copies(face, a.x)
                .into_iter()
                .map(|x| Segment::line(P::new(x, a.y), P::new(x, b.y)))
                .collect()
        }
        (Cut::Circle(_, start), Chart::Cylinder(_)) => {
            let y = chart.to_2d(*start).y;
            vec![Segment::line(P::new(face.lo.x, y), P::new(face.hi.x, y))]
        }
        (Cut::Same, _) => {
            // The other face's boundary, laid out in this face's chart.
            match chart {
                Chart::Plane { .. } => boundary_2d(chart, &other.loops).unwrap_or_default(),
                Chart::Cylinder(c) => {
                    let mut out = Vec::new();
                    for e in other.loops.iter().flatten() {
                        let (a, b) = (chart.to_2d(e.a), chart.to_2d(e.b));
                        match e.curve {
                            Curve::Line => {
                                for x in copies(face, a.x) {
                                    out.push(Segment::line(P::new(x, a.y), P::new(x, b.y)));
                                }
                            }
                            Curve::Circle(circle) => {
                                let mut turn = e.extent() * c.radius;
                                if circle.normal.dot(c.axis.direction) < 0.0 {
                                    turn = -turn;
                                }
                                for x in copies(face, a.x) {
                                    out.push(Segment::line(P::new(x, a.y), P::new(x + turn, a.y)));
                                }
                            }
                        }
                    }
                    out
                }
            }
        }
    }
}

/// The places `x` (taken within one period) falls along a cylinder face's unwrapped range.
fn copies(face: &Prepared, x: f64) -> Vec<f64> {
    let Some(period) = face.chart.period() else {
        return vec![x];
    };
    let mut out = Vec::new();
    let k0 = ((face.lo.x - x) / period).floor() as i64 - 1;
    for k in k0..k0 + 4 {
        let v = x + k as f64 * period;
        if v >= face.lo.x - TOL && v <= face.hi.x + TOL {
            out.push(v);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Pieces, and where they lie.

/// Where a piece of one solid's face lies against the other solid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Where {
    Inside,
    Outside,
    /// On a face of the other solid that faces the same way.
    OnSame,
    /// On a face of the other solid that faces the other way.
    OnOpposite,
}

/// A piece of a face, its loops in space.
#[derive(Debug, Clone)]
struct Piece {
    surface: Surface,
    loops: Vec<Vec<Edge3>>,
    sources: Vec<Source>,
}

impl Piece {
    fn reversed(&self) -> Piece {
        Piece {
            surface: match self.surface {
                Surface::Plane(p) => Surface::Plane(oxyde_geom::Plane {
                    normal: -p.normal,
                    ..p
                }),
                Surface::Cylinder(c) => Surface::Cylinder(Cylinder {
                    outward: !c.outward,
                    ..c
                }),
            },
            loops: self
                .loops
                .iter()
                .map(|l| l.iter().rev().map(Edge3::reversed).collect())
                .collect(),
            sources: self.sources.clone(),
        }
    }
}

/// Every face of `faces` cut along where it meets the faces of `others`, with where each
/// piece lies against the solid `others` bound.
fn pieces(faces: &[Prepared], others: &[Prepared]) -> Result<Vec<(Piece, Where)>, NativeError> {
    let mut out = Vec::new();
    for face in faces {
        let mut segments = face.boundary.clone();
        for other in others {
            if !boxes_meet(face, other) {
                continue;
            }
            for cut in meet(face, other)? {
                segments.extend(cut_into(face, &cut, other));
            }
        }
        // A cylinder's face is cut at its angle zero too, every period.
        if let Some(period) = face.chart.period() {
            let k0 = (face.lo.x / period).floor() as i64;
            for k in k0..=k0 + 2 {
                let x = k as f64 * period;
                if x > face.lo.x - TOL && x < face.hi.x + TOL {
                    segments.push(Segment::line(P::new(x, face.lo.y), P::new(x, face.hi.y)));
                }
            }
        }
        let arrangement = Arrangement::new(&segments, TOL);
        for region in arrangement.regions() {
            let Some((inner, _)) = arrangement.inside_point(&region) else {
                continue;
            };
            if !arrangement::inside(&face.boundary, inner, TOL) {
                continue;
            }
            // A cylinder's piece is taken within one period from angle zero.
            let shift = match face.chart.period() {
                Some(period) => -(inner.x / period).floor() * period,
                None => 0.0,
            };
            let mut loops = Vec::new();
            for cycle in std::iter::once(&region.outer).chain(&region.holes) {
                let mut edges = Vec::new();
                for &half in cycle {
                    let mut s = arrangement.segment(half);
                    s.a.x += shift;
                    s.b.x += shift;
                    if let Geom::Arc {
                        center,
                        radius,
                        ccw,
                    } = s.geom
                    {
                        s.geom = Geom::Arc {
                            center: P::new(center.x + shift, center.y),
                            radius,
                            ccw,
                        };
                    }
                    let curve = face.chart.curve(&s).map_err(NativeError::Internal)?;
                    edges.push(Edge3 {
                        a: face.chart.to_world(s.a),
                        b: face.chart.to_world(s.b),
                        curve,
                    });
                }
                loops.push(edges);
            }
            let point = face.chart.to_world(P::new(inner.x + shift, inner.y));
            let normal = face.chart.normal_at(point);
            let place = classify(point, normal, others)?;
            out.push((
                Piece {
                    surface: face.surface,
                    loops: outward_loops(&face.chart, loops),
                    sources: face.sources.clone(),
                },
                place,
            ));
        }
    }
    Ok(out)
}

/// The directions rays are cast in to tell inside from outside: odd, so that they seldom
/// run along an edge, and tried in turn when one grazes something.
const RAYS: [(f64, f64, f64); 6] = [
    (0.583_215_664, 0.271_828_182, 0.770_163_247),
    (-0.314_159_265, 0.881_373_587, 0.353_553_391),
    (0.703_125_917, -0.412_310_562, 0.574_456_264),
    (0.192_847_365, 0.364_271_829, -0.911_154_727),
    (-0.666_666_667, -0.577_350_269, 0.471_404_520),
    (0.845_154_255, 0.169_030_851, -0.507_092_552),
];

/// Where `point`, inside a piece whose outward normal there is `normal`, lies against the
/// solid bounded by `faces`.
fn classify(point: Point3, normal: Vec3, faces: &[Prepared]) -> Result<Where, NativeError> {
    // On one of its faces?
    for face in faces {
        if face.surface.distance(point) > TOL {
            continue;
        }
        if let Some(p) = on_face(face, point)
            && arrangement::inside(&face.boundary, p, TOL)
        {
            return Ok(if face.chart.normal_at(point).dot(normal) > 0.0 {
                Where::OnSame
            } else {
                Where::OnOpposite
            });
        }
    }
    'rays: for (x, y, z) in RAYS {
        let Some(dir) = Vec3::new(x, y, z).normalized() else {
            continue;
        };
        let mut count = 0usize;
        for face in faces {
            for t in ray_hits(&face.surface, point, dir) {
                let Some(t) = t else {
                    continue 'rays;
                };
                if t <= TOL {
                    continue;
                }
                let hit = point + dir * t;
                let Some(p) = on_face(face, hit) else {
                    continue;
                };
                if arrangement::distance_to(&face.boundary, p, TOL) <= 100.0 * TOL {
                    continue 'rays;
                }
                if arrangement::inside(&face.boundary, p, TOL) {
                    count += 1;
                }
            }
        }
        return Ok(if !count.is_multiple_of(2) {
            Where::Inside
        } else {
            Where::Outside
        });
    }
    Err(NativeError::Internal(
        "every ray cast to tell inside from outside grazed an edge".to_owned(),
    ))
}

/// `p`, a point of the face's surface, in its chart, its `x` taken within the face's
/// unwrapped range when it can be; `None` when it falls outside that range altogether.
fn on_face(face: &Prepared, p: Point3) -> Option<P> {
    let q = face.chart.to_2d(p);
    match face.chart.period() {
        None => Some(q),
        Some(_) => copies(face, q.x).first().map(|&x| P::new(x, q.y)),
    }
}

/// Where the ray `point + t dir` meets a surface: each `Some(t)`, or `None` when it grazes
/// it (runs along a plane in it, touches a cylinder).
fn ray_hits(surface: &Surface, point: Point3, dir: Vec3) -> Vec<Option<f64>> {
    match surface {
        Surface::Plane(p) => {
            let denom = dir.dot(p.normal);
            if denom.abs() < 1e-12 {
                return Vec::new();
            }
            vec![Some((p.origin - point).dot(p.normal) / denom)]
        }
        Surface::Cylinder(c) => {
            let a = c.axis.direction;
            let o = point - c.axis.origin;
            let (oo, dd) = (o - a * o.dot(a), dir - a * dir.dot(a));
            let qa = dd.dot(dd);
            if qa < 1e-12 {
                return Vec::new();
            }
            let qb = 2.0 * oo.dot(dd);
            let qc = oo.dot(oo) - c.radius * c.radius;
            let disc = qb * qb - 4.0 * qa * qc;
            if disc < 0.0 {
                return Vec::new();
            }
            let root = disc.sqrt();
            if root < 1e-9 * (qb.abs() + 1.0) {
                return vec![None];
            }
            vec![
                Some((-qb - root) / (2.0 * qa)),
                Some((-qb + root) / (2.0 * qa)),
            ]
        }
    }
}

// ---------------------------------------------------------------------------------------
// One boolean of two solids.

fn pairwise(op: BooleanOp, a: &Tagged, b: &Tagged) -> Result<Tagged, NativeError> {
    let (fa, fb) = (prepare(a)?, prepare(b)?);
    let mut kept: Vec<Piece> = Vec::new();
    for (piece, place) in pieces(&fa, &fb)? {
        let keep = match op {
            BooleanOp::Fuse => matches!(place, Where::Outside | Where::OnSame),
            BooleanOp::Cut => matches!(place, Where::Outside | Where::OnOpposite),
            BooleanOp::Common => matches!(place, Where::Inside | Where::OnSame),
        };
        if keep {
            kept.push(piece);
        }
    }
    for (piece, place) in pieces(&fb, &fa)? {
        match (op, place) {
            (BooleanOp::Fuse, Where::Outside) | (BooleanOp::Common, Where::Inside) => {
                kept.push(piece);
            }
            (BooleanOp::Cut, Where::Inside) => kept.push(piece.reversed()),
            _ => {}
        }
    }
    let faces = merge_faces(kept)?;
    assemble(faces)
}

// ---------------------------------------------------------------------------------------
// Welding pieces into faces and solids.

/// Points welded within the tolerance.
#[derive(Debug, Default)]
struct Points {
    points: Vec<Point3>,
}

impl Points {
    fn index(&mut self, p: Point3) -> u32 {
        if let Some(i) = self.points.iter().position(|q| q.distance(p) <= TOL) {
            return i as u32;
        }
        self.points.push(p);
        self.points.len() as u32 - 1
    }
}

/// Every edge of `loops` split at the points of `points` that lie along it.
fn split_at(loops: &[Vec<Edge3>], points: &[Point3]) -> Vec<Vec<Edge3>> {
    loops
        .iter()
        .map(|edges| {
            let mut out = Vec::new();
            for e in edges {
                let mut at: Vec<(f64, Point3)> = points
                    .iter()
                    .filter_map(|&p| e.inner(p).map(|s| (s, p)))
                    .collect();
                at.sort_by(|x, y| x.0.total_cmp(&y.0));
                let mut start = e.a;
                for (_, p) in at {
                    out.push(Edge3 {
                        a: start,
                        b: p,
                        curve: e.curve,
                    });
                    start = p;
                }
                out.push(Edge3 {
                    a: start,
                    b: e.b,
                    curve: e.curve,
                });
            }
            out
        })
        .collect()
}

/// Whether two faces lie on one surface, facing the same way.
fn same_surface(a: &Surface, b: &Surface) -> bool {
    match (a, b) {
        (Surface::Plane(p), Surface::Plane(q)) => {
            p.normal.dot(q.normal) > 1.0 - 1e-9 && (q.origin - p.origin).dot(p.normal).abs() <= TOL
        }
        (Surface::Cylinder(c), Surface::Cylinder(d)) => {
            c.outward == d.outward
                && c.axis.direction.cross(d.axis.direction).length() <= 1e-9
                && c.radial(d.axis.origin).length() <= TOL
                && (c.radius - d.radius).abs() <= TOL
        }
        _ => false,
    }
}

/// Edges between welded points, told apart by their middles: the same edge, whichever
/// way it is run, gets the same number.
#[derive(Debug, Default)]
struct Edges {
    by_ends: HashMap<(u32, u32), Vec<(Point3, u32)>>,
    count: u32,
}

impl Edges {
    /// The edge's number, and whether it is new.
    fn index(&mut self, points: &mut Points, e: &Edge3) -> (u32, bool) {
        let (i, j) = (points.index(e.a), points.index(e.b));
        let mid = e.midpoint();
        let list = self.by_ends.entry((i.min(j), i.max(j))).or_default();
        if let Some(&(_, k)) = list.iter().find(|(m, _)| m.distance(mid) <= 100.0 * TOL) {
            return (k, false);
        }
        list.push((mid, self.count));
        self.count += 1;
        (self.count - 1, true)
    }
}

/// The kept pieces merged into faces: pieces on one surface that share an edge become one
/// face, the edges between them gone, in a chart where they lie in one piece (for a
/// cylinder, cut open at angle zero when they go round whole, elsewhere when they do not).
fn merge_faces(pieces: Vec<Piece>) -> Result<Vec<Piece>, NativeError> {
    let internal = NativeError::Internal;
    // Weld the corners and split edges at corners other pieces have along them.
    let mut points = Points::default();
    for e in pieces.iter().flat_map(|p| p.loops.iter().flatten()) {
        points.index(e.a);
        points.index(e.b);
    }
    let all = points.points.clone();
    let pieces: Vec<Piece> = pieces
        .into_iter()
        .map(|p| Piece {
            loops: split_at(&p.loops, &all),
            ..p
        })
        .collect();
    // Pieces on one surface sharing an edge, grouped.
    let mut parent: Vec<usize> = (0..pieces.len()).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut table = Edges::default();
    let mut owners: Vec<Vec<usize>> = Vec::new();
    for (i, piece) in pieces.iter().enumerate() {
        for e in piece.loops.iter().flatten() {
            let (k, _) = table.index(&mut points, e);
            if owners.len() <= k as usize {
                owners.resize(k as usize + 1, Vec::new());
            }
            owners[k as usize].push(i);
        }
    }
    for list in &owners {
        for (k, &i) in list.iter().enumerate() {
            for &j in &list[k + 1..] {
                if i != j && same_surface(&pieces[i].surface, &pieces[j].surface) {
                    let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                    if ri != rj {
                        parent[ri.max(rj)] = ri.min(rj);
                    }
                }
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: HashMap<usize, usize> = HashMap::new();
    for i in 0..pieces.len() {
        let root = find(&mut parent, i);
        let g = *group_of.entry(root).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(i);
    }

    let mut out = Vec::new();
    for group in groups {
        let first = &pieces[group[0]];
        let mut sources: Vec<Source> = group
            .iter()
            .flat_map(|&i| pieces[i].sources.iter().copied())
            .collect();
        sources.sort();
        sources.dedup();
        if group.len() == 1 && !matches!(first.surface, Surface::Cylinder(_)) {
            out.push(Piece {
                sources,
                ..first.clone()
            });
            continue;
        }
        let chart = group_chart(&first.surface, group.iter().map(|&i| &pieces[i]));
        // Every piece's boundary in the group's chart, each walked from a start taken in
        // the chart's period, then moved by whole periods to lie within it.
        let mut segments: Vec<Segment> = Vec::new();
        for &i in &group {
            let mut s = boundary_2d(&chart, &pieces[i].loops).map_err(internal)?;
            if let Some(period) = chart.period() {
                let (lo, _) = box_2d(&s);
                let shift = -((lo.x + TOL) / period).floor() * period;
                for seg in &mut s {
                    seg.a.x += shift;
                    seg.b.x += shift;
                }
            }
            segments.extend(s);
        }
        // Edges two pieces share, run once each way, are inside the merged face.
        let mut gone = vec![false; segments.len()];
        for i in 0..segments.len() {
            if gone[i] {
                continue;
            }
            for j in i + 1..segments.len() {
                if !gone[j] && opposite(&segments[i], &segments[j]) {
                    gone[i] = true;
                    gone[j] = true;
                    break;
                }
            }
        }
        let kept: Vec<Segment> = segments
            .into_iter()
            .zip(gone)
            .filter(|(_, g)| !g)
            .map(|(s, _)| s)
            .collect();
        let arrangement = Arrangement::new(&kept, TOL);
        for region in arrangement.regions() {
            let mut loops = Vec::new();
            for cycle in std::iter::once(&region.outer).chain(&region.holes) {
                let mut edges = Vec::new();
                for &half in cycle {
                    let s = arrangement.segment(half);
                    edges.push(Edge3 {
                        a: chart.to_world(s.a),
                        b: chart.to_world(s.b),
                        curve: chart.curve(&s).map_err(internal)?,
                    });
                }
                loops.push(edges);
            }
            out.push(Piece {
                surface: first.surface,
                loops: outward_loops(&chart, loops),
                sources: sources.clone(),
            });
        }
    }
    Ok(out)
}

/// Whether two segments are the same, run opposite ways.
fn opposite(s: &Segment, t: &Segment) -> bool {
    if s.a.distance(t.b) > TOL || s.b.distance(t.a) > TOL {
        return false;
    }
    let (ms, mt) = (
        s.at(s.extent(TOL) / 2.0, TOL),
        t.at(t.extent(TOL) / 2.0, TOL),
    );
    ms.distance(mt) <= TOL
}

/// The chart a group of pieces on one surface is merged in: the surface's own, and for a
/// cylinder turned so that its angle zero falls in a gap between the pieces when they leave
/// one (so that a face crossing angle zero is not cut there), at the cylinder's own angle
/// zero when they go round whole (where its seam goes).
fn group_chart<'a>(surface: &Surface, group: impl Iterator<Item = &'a Piece>) -> Chart {
    let Surface::Cylinder(c) = surface else {
        return Chart::of(surface);
    };
    let chart = Chart::Cylinder(*c);
    let period = TAU * c.radius;
    // The stretches of angle the pieces cover, each within [0, period).
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for piece in group {
        let Ok(s) = boundary_2d(&chart, &piece.loops) else {
            continue;
        };
        let (lo, hi) = box_2d(&s);
        let shift = -((lo.x + TOL) / period).floor() * period;
        spans.push((lo.x + shift, hi.x + shift));
    }
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    // The widest gap between them, going round.
    let mut best: Option<(f64, f64)> = None;
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (lo, hi) in spans {
        if let Some(last) = merged.last_mut()
            && lo <= last.1 + TOL
        {
            last.1 = last.1.max(hi);
            continue;
        }
        merged.push((lo, hi));
    }
    for (i, &(lo, _)) in merged.iter().enumerate() {
        let prev_end = if i == 0 {
            merged.last().map_or(lo, |l| l.1 - period)
        } else {
            merged[i - 1].1
        };
        let gap = lo - prev_end;
        if gap > TOL && best.is_none_or(|b| gap > b.1 - b.0) {
            best = Some((prev_end, lo));
        }
    }
    match best {
        // Turned so the middle of the gap is the chart's angle zero.
        Some((from, to)) => {
            let turn = ((from + to) / 2.0) / c.radius;
            let (x, y, _) = c.frame();
            Chart::Cylinder(Cylinder {
                x_dir: x * turn.cos() + y * turn.sin(),
                ..*c
            })
        }
        None => chart,
    }
}

/// The faces welded into solids: corners and edges shared, edges along one line or circle
/// between the same two faces joined, and each connected set of faces a solid.
fn assemble(faces: Vec<Piece>) -> Result<Tagged, NativeError> {
    let internal = |m: &str| NativeError::Internal(m.to_owned());
    let mut points = Points::default();
    for e in faces.iter().flat_map(|p| p.loops.iter().flatten()) {
        points.index(e.a);
        points.index(e.b);
    }
    let all = points.points.clone();
    let faces: Vec<Piece> = faces
        .into_iter()
        .map(|p| Piece {
            loops: split_at(&p.loops, &all),
            ..p
        })
        .collect();

    let mut solid = Solid::default();
    let mut table = Edges::default();
    let mut sources: Vec<Vec<Source>> = Vec::new();
    for face in &faces {
        let mut loops = Vec::new();
        for edges in &face.loops {
            let mut uses = Vec::new();
            for e in edges {
                let (i, j) = (points.index(e.a), points.index(e.b));
                let (edge, new) = table.index(&mut points, e);
                if new {
                    solid.edges.push(Edge {
                        start: i,
                        end: j,
                        curve: e.curve,
                    });
                }
                let stored = solid.edges[edge as usize];
                // Run its own way when it starts where this use starts, and for a closed
                // circle when it turns the same way.
                let reversed = if stored.start == stored.end {
                    match (stored.curve, e.curve) {
                        (Curve::Circle(s), Curve::Circle(t)) => s.normal.dot(t.normal) < 0.0,
                        _ => false,
                    }
                } else {
                    stored.start != i
                };
                uses.push(EdgeUse { edge, reversed });
            }
            loops.push(uses);
        }
        solid.faces.push(Face {
            surface: face.surface,
            loops,
        });
        sources.push(face.sources.clone());
    }
    solid.vertices = points.points;
    join_edges(&mut solid);
    let (solids, solid_sources) = split_solids(solid, sources).map_err(|e| internal(&e))?;
    let shape = Shape { solids };
    // A shell facing inwards bounds a void inside another solid.
    for s in &shape.solids {
        let piece = Shape {
            solids: vec![s.clone()],
        };
        if piece.volume().map_err(NativeError::Internal)? < 0.0 {
            return Err(NativeError::Unsupported("leave a void inside a solid"));
        }
    }
    Ok(Tagged {
        shape,
        sources: solid_sources,
    })
}

/// Joins edges that meet at a vertex no other edge reaches, run along one line or circle,
/// between the same two faces: what is left of an edge cut by a curve that was merged away.
fn join_edges(solid: &mut Solid) {
    loop {
        let mut incident: Vec<Vec<u32>> = vec![Vec::new(); solid.vertices.len()];
        let mut faces_of: Vec<Vec<usize>> = vec![Vec::new(); solid.edges.len()];
        for (f, face) in solid.faces.iter().enumerate() {
            for u in face.loops.iter().flatten() {
                if !faces_of[u.edge as usize].contains(&f) {
                    faces_of[u.edge as usize].push(f);
                }
            }
        }
        for (e, edge) in solid.edges.iter().enumerate() {
            if faces_of[e].is_empty() {
                continue;
            }
            for v in [edge.start, edge.end] {
                if !incident[v as usize].contains(&(e as u32)) {
                    incident[v as usize].push(e as u32);
                }
            }
        }
        let mut joined = false;
        for (v, edges) in incident.iter().enumerate() {
            let [e1, e2] = edges.as_slice() else {
                continue;
            };
            let (a, b) = (solid.edges[*e1 as usize], solid.edges[*e2 as usize]);
            if a.start == a.end || b.start == b.end {
                continue;
            }
            let mut fa = faces_of[*e1 as usize].clone();
            let mut fb = faces_of[*e2 as usize].clone();
            fa.sort_unstable();
            fb.sort_unstable();
            if fa != fb || !same_curve(solid, &a, &b) {
                continue;
            }
            if join(solid, v as u32, *e1, *e2) {
                joined = true;
                break;
            }
        }
        if !joined {
            break;
        }
    }
    compact(solid);
}

fn same_curve(solid: &Solid, a: &Edge, b: &Edge) -> bool {
    match (a.curve, b.curve) {
        (Curve::Line, Curve::Line) => {
            let da = solid.point(a.end) - solid.point(a.start);
            let db = solid.point(b.end) - solid.point(b.start);
            da.cross(db).length() <= 1e-9 * da.length() * db.length()
        }
        (Curve::Circle(c), Curve::Circle(d)) => {
            c.center.distance(d.center) <= TOL
                && (c.radius - d.radius).abs() <= TOL
                && c.normal.cross(d.normal).length() <= 1e-9
        }
        _ => false,
    }
}

/// Replaces `e1` and `e2`, meeting at `v`, by one edge in every loop that runs along them.
fn join(solid: &mut Solid, v: u32, e1: u32, e2: u32) -> bool {
    // The joined edge, run as the first loop that uses both runs them.
    let mut new_edge: Option<Edge> = None;
    for face in &solid.faces {
        for uses in &face.loops {
            let n = uses.len();
            for k in 0..n {
                let (u, w) = (uses[k], uses[(k + 1) % n]);
                let pair = (u.edge == e1 && w.edge == e2) || (u.edge == e2 && w.edge == e1);
                if pair && solid.end_of(u) == v {
                    let start = solid.start_of(u);
                    let end = solid.end_of(w);
                    let curve = match solid.edges[u.edge as usize].curve {
                        Curve::Line => Curve::Line,
                        Curve::Circle(c) => Curve::Circle(if u.reversed {
                            Circle {
                                normal: -c.normal,
                                ..c
                            }
                        } else {
                            c
                        }),
                    };
                    new_edge = Some(Edge { start, end, curve });
                    break;
                }
            }
            if new_edge.is_some() {
                break;
            }
        }
        if new_edge.is_some() {
            break;
        }
    }
    let Some(new_edge) = new_edge else {
        return false;
    };
    solid.edges.push(new_edge);
    let ne = solid.edges.len() as u32 - 1;
    let snapshot = solid.clone();
    // A seam runs along its face's loop twice, once each way: every run is replaced.
    for face in &mut solid.faces {
        for uses in &mut face.loops {
            loop {
                let n = uses.len();
                let Some(k) = (0..n).find(|&k| {
                    let (u, w) = (uses[k], uses[(k + 1) % n]);
                    ((u.edge == e1 && w.edge == e2) || (u.edge == e2 && w.edge == e1))
                        && snapshot.end_of(u) == v
                }) else {
                    break;
                };
                let u = uses[k];
                let reversed = snapshot.start_of(u) != new_edge.start
                    || (new_edge.start == new_edge.end && {
                        // A closed circle: the way it turns.
                        match (new_edge.curve, snapshot.edges[u.edge as usize].curve) {
                            (Curve::Circle(c), Curve::Circle(d)) => {
                                let d = if u.reversed { -d.normal } else { d.normal };
                                c.normal.dot(d) < 0.0
                            }
                            _ => false,
                        }
                    });
                let replacement = EdgeUse { edge: ne, reversed };
                if k + 1 < n {
                    uses.splice(k..k + 2, [replacement]);
                } else {
                    uses.remove(k);
                    uses[0] = replacement;
                }
            }
        }
    }
    true
}

/// Drops edges and vertices nothing uses any more, renumbering the rest in order.
fn compact(solid: &mut Solid) {
    let mut used_edges = vec![false; solid.edges.len()];
    for u in solid.faces.iter().flat_map(|f| f.loops.iter().flatten()) {
        used_edges[u.edge as usize] = true;
    }
    let mut edge_map = vec![u32::MAX; solid.edges.len()];
    let mut edges = Vec::new();
    for (e, edge) in solid.edges.iter().enumerate() {
        if used_edges[e] {
            edge_map[e] = edges.len() as u32;
            edges.push(*edge);
        }
    }
    let mut used_points = vec![false; solid.vertices.len()];
    for e in &edges {
        used_points[e.start as usize] = true;
        used_points[e.end as usize] = true;
    }
    let mut point_map = vec![u32::MAX; solid.vertices.len()];
    let mut vertices = Vec::new();
    for (v, p) in solid.vertices.iter().enumerate() {
        if used_points[v] {
            point_map[v] = vertices.len() as u32;
            vertices.push(*p);
        }
    }
    for e in &mut edges {
        e.start = point_map[e.start as usize];
        e.end = point_map[e.end as usize];
    }
    for face in &mut solid.faces {
        for u in face.loops.iter_mut().flatten() {
            u.edge = edge_map[u.edge as usize];
        }
    }
    solid.edges = edges;
    solid.vertices = vertices;
}

/// Each set of faces joined by edges, a solid of its own, renumbered; the faces' sources
/// in the same order.
fn split_solids(
    solid: Solid,
    sources: Vec<Vec<Source>>,
) -> Result<(Vec<Solid>, Vec<Vec<Source>>), String> {
    let n = solid.faces.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut owner: HashMap<u32, usize> = HashMap::new();
    for (f, face) in solid.faces.iter().enumerate() {
        for u in face.loops.iter().flatten() {
            if let Some(&g) = owner.get(&u.edge) {
                let (a, b) = (find(&mut parent, f), find(&mut parent, g));
                if a != b {
                    parent[a.max(b)] = a.min(b);
                }
            } else {
                owner.insert(u.edge, f);
            }
        }
    }
    let mut order: Vec<usize> = Vec::new();
    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for f in 0..n {
        let r = find(&mut parent, f);
        if !members.contains_key(&r) {
            order.push(r);
        }
        members.entry(r).or_default().push(f);
    }
    let mut solids = Vec::new();
    let mut out_sources = Vec::new();
    for r in order {
        let mut part = Solid {
            vertices: solid.vertices.clone(),
            edges: solid.edges.clone(),
            faces: members[&r]
                .iter()
                .map(|&f| solid.faces[f].clone())
                .collect(),
        };
        compact(&mut part);
        out_sources.extend(members[&r].iter().map(|&f| sources[f].clone()));
        solids.push(part);
    }
    Ok((solids, out_sources))
}
