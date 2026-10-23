//! Booleans of solids with planar faces and faces on surfaces of revolution, by cutting
//! faces along the curves where they meet.
//!
//! Each face is laid out flat in its own chart (a plane's coordinates, or a surface of
//! revolution's `(k θ, m v)`: its angle about the axis and its parameter along its
//! meridian, scaled to lengths, where everything the kernel meets on it so far is
//! straight), cut along the curves it shares with the other solid's faces
//! ([`crate::arrangement`]), and every piece is kept or dropped by where a point inside it
//! lies: inside the other solid, outside it, or on one of its faces facing the same way or
//! the other. The pieces kept are welded at their corners, the pieces lying on one surface
//! merged back into faces (a face that goes round its axis whole cut open along its seam at
//! its angle zero, one that does not left whole), and edges along one line or circle
//! between the same two faces joined, as OpenCASCADE's `SimplifyResult` leaves them. A
//! piece that reaches a cone's apex or a sphere's pole runs along a degenerate edge there.
//! A shell left inside a solid with its faces turned inwards is a void of that solid.
//!
//! What meets in lines and circles only: planes; cylinders square to the planes they meet
//! or along them, and parallel to the other cylinders; cones, spheres and tori sharing
//! their axis with the surfaces of revolution they meet, square to the planes they meet or
//! holding them along their axis. Anything else answers [`NativeError::Unsupported`].

// Indices into the points, edges and faces gathered here.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;
use std::f64::consts::TAU;

use oxyde_geom::{LINEAR_TOLERANCE, Point3, Vec3};
use oxyde_kernel::{BooleanOp, FaceSource, OpHistory};

use crate::NativeError;
use crate::arrangement::{self, Arrangement, Geom, P, Segment, Tag};
use crate::brep::{Edge, EdgeUse, Face, Shape, Solid, Vertex};
use crate::geometry::{Circle, Curve, Section, Surface};
use crate::revolution::{Meridian, PlaneCurve, Revolution, WalkEdge, walk};

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

/// A surface's flat coordinates: a plane's own, or a surface of revolution's
/// `x = k θ` (counter-clockwise about its axis from its angle zero) and `y = m v`, `k` a
/// radius of the surface and `m` its meridian circle's radius (1 for a straight meridian,
/// whose `v` is already a length), so that lengths in the chart are about lengths in space.
#[derive(Debug, Clone, Copy)]
enum Chart {
    Plane {
        origin: Point3,
        x: Vec3,
        y: Vec3,
        normal: Vec3,
    },
    Revolution {
        surface: Revolution,
        k: f64,
        m: f64,
    },
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
            Surface::Revolution(r) => Chart::revolution(*r),
        }
    }

    fn revolution(surface: Revolution) -> Chart {
        let (k, m) = match surface.meridian {
            Meridian::Line { radius, .. } => (radius.abs().max(1.0), 1.0),
            Meridian::Circle { center, radius } => ((center + radius).max(1e-9), radius),
        };
        Chart::Revolution { surface, k, m }
    }

    /// The length a whole turn takes along `x`: none for a plane.
    fn period(&self) -> Option<f64> {
        match self {
            Chart::Plane { .. } => None,
            Chart::Revolution { k, .. } => Some(TAU * k),
        }
    }

    /// The length a whole turn round a ring torus's tube takes along `y`.
    fn v_period(&self) -> Option<f64> {
        match self {
            Chart::Revolution { surface, m, .. } if surface.v_periodic() => Some(TAU * m),
            _ => None,
        }
    }

    fn to_world(self, p: P) -> Point3 {
        match self {
            Chart::Plane { origin, x, y, .. } => origin + x * p.x + y * p.y,
            Chart::Revolution { surface, k, m } => surface.point(p.x / k, p.y / m),
        }
    }

    /// `p` in the chart: on a surface of revolution, its angle taken in `[0, 2π)`.
    fn to_2d(self, p: Point3) -> P {
        match self {
            Chart::Plane { origin, x, y, .. } => {
                let d = p - origin;
                P::new(d.dot(x), d.dot(y))
            }
            Chart::Revolution { surface, k, m } => {
                let (theta, v) = surface.params(p);
                P::new(theta.rem_euclid(TAU) * k, v * m)
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
            (Chart::Revolution { surface, k, m }, Geom::Line) => {
                let a = surface.axis.direction;
                if (s.a.x - s.b.x).abs() <= TOL {
                    // Along the meridian at its angle.
                    let theta = s.a.x / k;
                    match surface.meridian {
                        Meridian::Line { .. } => Ok(Curve::Line),
                        Meridian::Circle { .. } => {
                            let circle = surface
                                .meridian_circle(theta)
                                .ok_or_else(|| "no meridian circle".to_owned())?;
                            Ok(Curve::Circle(if s.b.y > s.a.y {
                                circle
                            } else {
                                Circle {
                                    normal: -circle.normal,
                                    ..circle
                                }
                            }))
                        }
                    }
                } else if (s.a.y - s.b.y).abs() <= TOL {
                    // Round the axis at its height: a parallel, or a pole.
                    let v = s.a.y / m;
                    let up = s.b.x > s.a.x;
                    if surface.is_pole(v) {
                        return Ok(Curve::Degenerate {
                            axis: if up { a } else { -a },
                            turn: (s.b.x - s.a.x).abs() / k,
                        });
                    }
                    let circle = surface
                        .parallel(v)
                        .ok_or_else(|| "no parallel".to_owned())?;
                    Ok(Curve::Circle(Circle {
                        normal: if up { a } else { -a },
                        ..circle
                    }))
                } else {
                    Err(
                        "a cut across a surface of revolution that is neither along nor round it"
                            .to_owned(),
                    )
                }
            }
            (Chart::Revolution { .. }, Geom::Arc { .. }) => {
                Err("an arc in a surface of revolution's chart".to_owned())
            }
        }
    }

    /// The outward normal of the face at `p` (in space).
    fn normal_at(&self, p: Point3) -> Vec3 {
        match self {
            Chart::Plane { normal, .. } => *normal,
            Chart::Revolution { surface, .. } => surface.normal_at(p),
        }
    }
}

/// An edge in space, from `a` to `b`: a line, an arc counter-clockwise about its circle's
/// normal (a whole circle when `a` is `b`), a section, or a degenerate edge at a point.
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
            curve: self.curve.reversed(),
        }
    }

    fn closed(&self) -> bool {
        self.a.distance(self.b) <= TOL
    }

    /// The angle it turns through (a circle, a section, a degenerate edge), its length (a
    /// line).
    fn extent(&self) -> f64 {
        self.curve.extent(self.a, self.b, self.closed())
    }

    fn midpoint(&self) -> Point3 {
        self.curve
            .at(self.a, self.b, self.extent() / 2.0)
            .unwrap_or(self.a)
    }

    /// Where `p` is along it, strictly between its ends: `None` when it is off it, or at
    /// an end.
    fn inner(&self, p: Point3) -> Option<f64> {
        if p.distance(self.a) <= TOL || p.distance(self.b) <= TOL || self.curve.is_degenerate() {
            return None;
        }
        if self.curve.distance(self.a, self.b, p) > TOL {
            return None;
        }
        let s = self.curve.position(self.a, self.b, p);
        (s > 0.0 && s < self.extent()).then_some(s)
    }

    fn walk_edge(&self) -> WalkEdge {
        WalkEdge {
            curve: self.curve,
            a: self.a,
            b: self.b,
            sweep: self.extent(),
            mid: self.midpoint(),
            tolerance: TOL,
        }
    }
}

/// The arrangement's regions run counter-clockwise in a chart, which is outward for a plane
/// and for a surface of revolution whose normal is its own; one whose normal is the other
/// way (a hole's wall) runs the other way.
fn outward_loops(chart: &Chart, loops: Vec<Vec<Edge3>>) -> Vec<Vec<Edge3>> {
    match chart {
        Chart::Revolution { surface, .. } if !surface.outward => loops
            .into_iter()
            .map(|l| l.iter().rev().map(Edge3::reversed).collect())
            .collect(),
        _ => loops,
    }
}

/// A face's boundary in its chart, walked as its loops run: on a surface of revolution,
/// the angle unwrapped as it goes round, so that a face crossing angle zero stays in one
/// piece and a face going round whole runs a whole period along `x`. An edge the chart makes
/// neither straight nor round is laid out as points ([`lay`]).
fn boundary_2d(chart: &Chart, loops: &[Vec<Edge3>]) -> Result<Vec<Segment>, String> {
    let mut out: Vec<Segment> = Vec::new();
    // The outer loop's middle: each later loop (a hole) is moved by whole turns to lie
    // about it.
    let mut middle: Option<P> = None;
    for edges in loops {
        let first = out.len();
        match chart {
            Chart::Plane { normal, .. } => {
                for e in edges {
                    let (a, b) = (chart.to_2d(e.a), chart.to_2d(e.b));
                    match e.curve {
                        Curve::Line => out.push(Segment::line(a, b)),
                        Curve::Circle(c) => out.push(Segment::arc(
                            a,
                            b,
                            chart.to_2d(c.center),
                            c.normal.dot(*normal) > 0.0,
                        )),
                        Curve::Section(_) => out.extend(lay(chart, e, a)),
                        Curve::Degenerate { .. } => {}
                    }
                }
            }
            Chart::Revolution { surface, k, m } => {
                let walked = walk(
                    surface,
                    &edges.iter().map(Edge3::walk_edge).collect::<Vec<_>>(),
                    TOL,
                )?;
                for (e, w) in edges.iter().zip(walked) {
                    let from = P::new(w.from.0 * k, w.from.1 * m);
                    if w.straight {
                        out.push(Segment::line(from, P::new(w.to.0 * k, w.to.1 * m)));
                    } else {
                        out.extend(lay(chart, e, from));
                    }
                }
            }
        }
        let (lo, hi) = box_2d(&out[first..]);
        let mid = P::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
        match middle {
            None => middle = Some(mid),
            Some(outer) => {
                let turn = |period: Option<f64>, from: f64, to: f64| {
                    period.map_or(0.0, |p| p * ((to - from) / p).round())
                };
                let (dx, dy) = (
                    turn(chart.period(), mid.x, outer.x),
                    turn(chart.v_period(), mid.y, outer.y),
                );
                if dx != 0.0 || dy != 0.0 {
                    for s in &mut out[first..] {
                        *s = shifted(s, dx, dy);
                    }
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Curves no chart makes straight or round: laid out as points, put back together exactly.

/// The step between the points a curve is laid out at, in its own angle: the same for every
/// copy of a curve, so that two layouts of one curve share their points. The layout only
/// says which piece is where; where pieces meet is found again on the exact curves
/// ([`meeting_point`]) and their integrals follow the exact curves, so a few hundred points a
/// turn are enough.
const LAY_STEP: f64 = TAU / 256.0;

/// How far a meeting point found on the exact curves may be from where the layouts met: a
/// chord a 256th of a turn round a circle of radius 100 is 0.008 mm off it.
const REFINE_REACH: f64 = 0.05;

/// A circle or a section in the one form a tag holds whichever way an edge runs on it: a
/// section followed as its angle grows, a circle about the one of its two normals whose
/// first component away from zero is positive.
fn canonical(curve: Curve) -> Curve {
    match curve {
        Curve::Section(s) => Curve::Section(Section { ccw: true, ..s }),
        Curve::Circle(c) => {
            let n = c.normal;
            let first = if n.x.abs() > 1e-12 {
                n.x
            } else if n.y.abs() > 1e-12 {
                n.y
            } else {
                n.z
            };
            Curve::Circle(Circle {
                normal: if first < 0.0 { -n } else { n },
                ..c
            })
        }
        other => other,
    }
}

/// The angle of `p` on a canonical curve.
fn param_of(canon: &Curve, p: Point3) -> f64 {
    match canon {
        Curve::Section(s) => s.angle_of(p),
        Curve::Circle(c) => c.angle_of(p),
        _ => 0.0,
    }
}

/// The point at angle `t` on a canonical curve.
fn point_of(canon: &Curve, t: f64) -> Option<Point3> {
    match canon {
        Curve::Section(s) => s.point_at(t),
        Curve::Circle(c) => Some(c.point_at(t)),
        _ => None,
    }
}

/// Whether an edge on `curve` runs the way its canonical form's angle grows.
fn runs_up(curve: &Curve) -> bool {
    match curve {
        Curve::Section(s) => s.ccw,
        Curve::Circle(c) => match canonical(*curve) {
            Curve::Circle(k) => k.normal.dot(c.normal) > 0.0,
            _ => true,
        },
        _ => true,
    }
}

/// `p` in `chart`, moved by whole periods to be nearest `near`.
fn chart_near(chart: &Chart, p: Point3, near: P) -> P {
    let mut q = chart.to_2d(p);
    if let Some(period) = chart.period() {
        q.x += period * ((near.x - q.x) / period).round();
    }
    if let Some(period) = chart.v_period() {
        q.y += period * ((near.y - q.y) / period).round();
    }
    q
}

/// An edge on a circle or a section laid in `chart` as straight pieces from `from` (where
/// its start lies there), each tagged with the curve and its angles at its ends: at every
/// multiple of [`LAY_STEP`] of the curve's angle between its ends.
fn lay(chart: &Chart, e: &Edge3, from: P) -> Vec<Segment> {
    let canon = canonical(e.curve);
    let up = runs_up(&e.curve);
    let ta = param_of(&canon, e.a);
    let extent = e.extent();
    let tb = if up { ta + extent } else { ta - extent };
    let (lo, hi) = (ta.min(tb), ta.max(tb));
    let mut ts: Vec<f64> = ((lo / LAY_STEP).floor() as i64 + 1..=(hi / LAY_STEP).ceil() as i64)
        .map(|k| k as f64 * LAY_STEP)
        .filter(|&t| t > lo + 1e-9 && t < hi - 1e-9)
        .collect();
    if !up {
        ts.reverse();
    }
    ts.insert(0, ta);
    ts.push(tb);
    let mut points: Vec<(f64, P)> = Vec::with_capacity(ts.len());
    let mut near = from;
    for (i, &t) in ts.iter().enumerate() {
        let p = if i == 0 {
            e.a
        } else if i + 1 == ts.len() {
            e.b
        } else {
            match point_of(&canon, t) {
                Some(p) => p,
                None => continue,
            }
        };
        let q = if i == 0 {
            from
        } else {
            chart_near(chart, p, near)
        };
        near = q;
        points.push((t, q));
    }
    points
        .windows(2)
        .map(|w| {
            Segment::tagged(
                w[0].1,
                w[1].1,
                Tag {
                    curve: canon,
                    t0: w[0].0,
                    t1: w[1].0,
                },
            )
        })
        .collect()
}

/// A whole loop on a circle or a section, from `start` round and back, laid in `chart` from
/// where `start` lies there.
fn lay_loop(chart: &Chart, curve: Curve, start: Point3) -> Vec<Segment> {
    let e = Edge3 {
        a: start,
        b: start,
        curve,
    };
    lay(chart, &e, chart.to_2d(start))
}

/// The edges of a traced cycle of the arrangement, its segments in order: an untagged
/// segment is the curve the chart makes of it; a run of segments tagged with one curve,
/// their angles following on, is one edge on that curve, its ends where the tags say,
/// moved onto both curves where it meets another edge.
fn cycle_edges(chart: &Chart, segments: &[Segment]) -> Result<Vec<Edge3>, String> {
    let n = segments.len();
    // Whether segment `k` carries on the tagged run of the one before it.
    let carries_on = |k: usize| -> bool {
        let (Some(prev), Some(this)) = (segments[(k + n - 1) % n].tag, segments[k].tag) else {
            return false;
        };
        let gap = (this.t0 - prev.t1).rem_euclid(TAU);
        prev.curve == this.curve && gap.min(TAU - gap) <= 1e-9
    };
    // Start at a run's first segment; round a loop that is one run, where its angles go
    // back a whole turn: where it was laid from (a section's from its carrier's angle zero),
    // so that every face sharing the loop starts it at the same vertex.
    let start = (0..n)
        .find(|&k| !carries_on(k))
        .or_else(|| {
            (0..n).find(|&k| {
                let (Some(prev), Some(this)) = (segments[(k + n - 1) % n].tag, segments[k].tag)
                else {
                    return false;
                };
                (this.t0 - prev.t1).abs() > 1e-6
            })
        })
        .unwrap_or(0);
    let mut edges: Vec<(Edge3, bool)> = Vec::new();
    let mut k = 0;
    while k < n {
        let s = segments[(start + k) % n];
        match s.tag {
            None => {
                edges.push((
                    Edge3 {
                        a: chart.to_world(s.a),
                        b: chart.to_world(s.b),
                        curve: chart.curve(&s)?,
                    },
                    false,
                ));
                k += 1;
            }
            Some(tag) => {
                // The whole run, its angles followed on.
                let t_start = tag.t0;
                let mut t_end = tag.t1;
                k += 1;
                while k < n && carries_on((start + k) % n) {
                    let next = segments[(start + k) % n].tag.ok_or("a run lost its tag")?;
                    let shift = TAU * ((t_end - next.t0) / TAU).round();
                    t_end = next.t1 + shift;
                    k += 1;
                }
                let up = t_end > t_start;
                let whole = (t_end - t_start).abs() > TAU - 1e-6;
                let a = point_of(&tag.curve, t_start).ok_or("a run's start is off its curve")?;
                let b = if whole {
                    a
                } else {
                    point_of(&tag.curve, t_end).ok_or("a run's end is off its curve")?
                };
                edges.push((
                    Edge3 {
                        a,
                        b,
                        curve: if up { tag.curve } else { tag.curve.reversed() },
                    },
                    true,
                ));
            }
        }
    }
    // Where a run meets another edge, the meeting point on both, exactly.
    let m = edges.len();
    if m > 1 {
        for i in 0..m {
            let j = (i + 1) % m;
            let (e, laid) = edges[i];
            let (f, next_laid) = edges[j];
            if !(laid || next_laid) || e.curve.is_degenerate() || f.curve.is_degenerate() {
                continue;
            }
            let q = meeting_point(&e, &f, e.b);
            edges[i].0.b = q;
            edges[j].0.a = q;
        }
    }
    Ok(edges.into_iter().map(|(e, _)| e).collect())
}

/// An edge's curve as a function of one parameter, and that parameter at `p`.
fn parametric(e: &Edge3, p: Point3) -> (f64, impl Fn(f64) -> Option<Point3> + '_) {
    let d = e.b - e.a;
    let t = match e.curve {
        Curve::Line => {
            let l2 = d.dot(d).max(f64::MIN_POSITIVE);
            (p - e.a).dot(d) / l2
        }
        Curve::Circle(c) => c.angle_of(p),
        Curve::Section(s) => s.angle_of(p),
        Curve::Degenerate { .. } => 0.0,
    };
    let f = move |t: f64| match e.curve {
        Curve::Line => Some(e.a + d * t),
        Curve::Circle(c) => Some(c.point_at(t)),
        Curve::Section(s) => s.point_at(t),
        Curve::Degenerate { .. } => Some(e.a),
    };
    (t, f)
}

/// The point where the curves of `e` and `f` meet near `p`, by Gauss–Newton on their two
/// parameters; `p` itself when they do not meet clearly there.
fn meeting_point(e: &Edge3, f: &Edge3, p: Point3) -> Point3 {
    let (mut t, c) = parametric(e, p);
    let (mut u, d) = parametric(f, p);
    let h = 1e-7;
    for _ in 0..40 {
        let (Some(pc), Some(pd)) = (c(t), d(u)) else {
            return p;
        };
        let r = pc - pd;
        if r.length() < 1e-13 {
            break;
        }
        let (Some(c1), Some(c0), Some(d1), Some(d0)) = (c(t + h), c(t - h), d(u + h), d(u - h))
        else {
            return p;
        };
        let jc = (c1 - c0) * (1.0 / (2.0 * h));
        let jd = (d1 - d0) * (-1.0 / (2.0 * h));
        // (JᵀJ) Δ = −Jᵀ r for J = [jc jd].
        let (a11, a12, a22) = (jc.dot(jc), jc.dot(jd), jd.dot(jd));
        let (b1, b2) = (-jc.dot(r), -jd.dot(r));
        let det = a11 * a22 - a12 * a12;
        if det.abs() <= 1e-12 * (a11 * a22).max(f64::MIN_POSITIVE) {
            return p;
        }
        let dt = (b1 * a22 - b2 * a12) / det;
        let du = (a11 * b2 - a12 * b1) / det;
        t += dt;
        u += du;
        if dt.abs() + du.abs() < 1e-15 {
            break;
        }
    }
    match (c(t), d(u)) {
        (Some(a), Some(b)) if a.distance(b) <= TOL && a.distance(p) <= REFINE_REACH => {
            a + (b - a) * 0.5
        }
        _ => p,
    }
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
    let mut out = Vec::new();
    let mut index = 0;
    for solid in &tagged.shape.solids {
        for face in &solid.faces {
            out.push(prepare_face(
                solid,
                face,
                tagged.sources.get(index).cloned().unwrap_or_default(),
            )?);
            index += 1;
        }
    }
    Ok(out)
}

fn prepare_face(solid: &Solid, face: &Face, sources: Vec<Source>) -> Result<Prepared, NativeError> {
    let loops: Vec<Vec<Edge3>> = face
        .loops
        .iter()
        .map(|uses| uses.iter().map(|&u| edge3(solid, u)).collect())
        .collect();
    let chart = Chart::of(&face.surface);
    let boundary = boundary_2d(&chart, &loops).map_err(NativeError::Internal)?;
    let (lo, hi) = box_2d(&boundary);
    let (min, max) = face_box(solid, face, &loops);
    Ok(Prepared {
        surface: face.surface,
        chart,
        loops,
        boundary,
        lo,
        hi,
        min,
        max,
        sources,
    })
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

fn box_of(points: impl IntoIterator<Item = Point3>) -> (Point3, Point3) {
    let mut min = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut max = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in points {
        min = Point3::new(min.x.min(p.x), min.y.min(p.y), min.z.min(p.z));
        max = Point3::new(max.x.max(p.x), max.y.max(p.y), max.z.max(p.z));
    }
    (min, max)
}

fn edges_box(loops: &[Vec<Edge3>]) -> (Point3, Point3) {
    let mut points = Vec::new();
    for e in loops.iter().flatten() {
        points.push(e.a);
        if let Curve::Circle(c) = e.curve {
            points.extend(c.extremes(c.angle_of(e.a), e.extent()));
        }
    }
    box_of(points)
}

/// A face's box: its edges', and where a sphere's or a torus's face bulges out between
/// them.
fn face_box(solid: &Solid, face: &Face, loops: &[Vec<Edge3>]) -> (Point3, Point3) {
    let (min, max) = edges_box(loops);
    box_of([min, max].into_iter().chain(solid.bulges(face)))
}

fn boxes_meet(a: &Prepared, b: &Prepared) -> bool {
    a.min.x <= b.max.x + TOL
        && b.min.x <= a.max.x + TOL
        && a.min.y <= b.max.y + TOL
        && b.min.y <= a.max.y + TOL
        && a.min.z <= b.max.z + TOL
        && b.min.z <= a.max.z + TOL
}

/// Whether a face's box lies wholly on one side of a plane, so that the face cannot meet
/// it.
fn beside(plane: &oxyde_geom::Plane, min: Point3, max: Point3) -> bool {
    let corners = (0..8).map(|i| {
        Point3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        )
    });
    let (mut below, mut above) = (false, false);
    for c in corners {
        let d = (c - plane.origin).dot(plane.normal);
        below |= d < TOL;
        above |= d > -TOL;
    }
    !(below && above)
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
    /// A whole circle, starting at `start` (on a surface of revolution's parallel, the point
    /// at its angle zero, where its seam will meet it).
    Circle(Circle, Point3),
    /// A whole loop of a section, starting at `start` (at the carrier's angle zero).
    Section(Section, Point3),
    /// The two faces lie on one surface: each is cut along the other's boundary.
    Same,
}

/// The curves where faces `a` and `b` meet, within their boxes.
fn meet(a: &Prepared, b: &Prepared) -> Result<Vec<Cut>, NativeError> {
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
        (Surface::Plane(_), Surface::Revolution(_)) => plane_revolution(a, b),
        (Surface::Revolution(_), Surface::Plane(_)) => plane_revolution(b, a),
        (Surface::Revolution(r1), Surface::Revolution(r2)) => {
            if let Some(m) = r1.coaxial_meridian(r2, TOL) {
                return coaxial(a, b, r1, &m);
            }
            match (r1.cylinder_radius(), r2.cylinder_radius()) {
                (Some(_), Some(_))
                    if r1.axis.direction.cross(r2.axis.direction).length() <= 1e-9 =>
                {
                    parallel_cylinders(a, b, r1, r2)
                }
                _ => crossing(a, b, r1, r2),
            }
        }
    }
}

/// Two cylinders whose axes are parallel but apart: the lines along both where their
/// circles across the axes cross.
fn parallel_cylinders(
    a: &Prepared,
    b: &Prepared,
    c1: &Revolution,
    c2: &Revolution,
) -> Result<Vec<Cut>, NativeError> {
    let (Some(r1), Some(r2)) = (c1.cylinder_radius(), c2.cylinder_radius()) else {
        return Ok(Vec::new());
    };
    let a1 = c1.axis.direction;
    let d = c2.axis.origin - c1.axis.origin;
    let offset = d - a1 * d.dot(a1);
    let dist = offset.length();
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

/// Two surfaces of revolution on one axis: they meet in circles across it, one where their
/// meridians cross.
fn coaxial(
    a: &Prepared,
    b: &Prepared,
    r1: &Revolution,
    other: &PlaneCurve,
) -> Result<Vec<Cut>, NativeError> {
    let Some(crossings) = r1.meridian_crossings(other, TOL) else {
        return Ok(vec![Cut::Same]);
    };
    let ax = r1.axis.direction;
    let mut cuts = Vec::new();
    for (rho, z) in crossings {
        let circle = Circle {
            center: r1.axis.origin + ax * z,
            normal: ax,
            radius: rho,
        };
        if circle_meets(&circle, a) && circle_meets(&circle, b) {
            let v = r1.v_of(rho, z);
            cuts.push(Cut::Circle(circle, r1.point(0.0, v)));
        }
    }
    Ok(cuts)
}

/// Whether a whole circle's box meets a face's box.
fn circle_meets(circle: &Circle, face: &Prepared) -> bool {
    let start = circle.point_at(0.0);
    let (cmin, cmax) = edges_box(&[vec![Edge3 {
        a: start,
        b: start,
        curve: Curve::Circle(*circle),
    }]]);
    cmin.x <= face.max.x + TOL
        && face.min.x <= cmax.x + TOL
        && cmin.y <= face.max.y + TOL
        && face.min.y <= cmax.y + TOL
        && cmin.z <= face.max.z + TOL
        && face.min.z <= cmax.z + TOL
}

/// A plane and a surface of revolution: circles across the axis where the plane is square
/// to it, the meridians where it holds the axis, a cylinder's lines where it runs along the
/// axis.
fn plane_revolution(
    plane_face: &Prepared,
    revolution_face: &Prepared,
) -> Result<Vec<Cut>, NativeError> {
    let (Surface::Plane(p), Surface::Revolution(r)) =
        (&plane_face.surface, &revolution_face.surface)
    else {
        return Ok(Vec::new());
    };
    if beside(p, revolution_face.min, revolution_face.max) {
        return Ok(Vec::new());
    }
    let a = r.axis.direction;
    let along = p.normal.dot(a);
    if along.abs() > 1.0 - 1e-9 {
        // Square to the axis: circles where the plane's height crosses the meridian.
        let t = (p.origin - r.axis.origin).dot(p.normal) / along;
        let mut cuts = Vec::new();
        for (rho, z) in plane_height_crossings(r, t) {
            let circle = Circle {
                center: r.axis.origin + a * z,
                normal: a,
                radius: rho,
            };
            if circle_meets(&circle, plane_face) && circle_meets(&circle, revolution_face) {
                let v = r.v_of(rho, z);
                cuts.push(Cut::Circle(circle, r.point(0.0, v)));
            }
        }
        return Ok(cuts);
    }
    if along.abs() > 1e-9 {
        return plane_section(plane_face, revolution_face, p, r);
    }
    // Along the axis, `d` from it.
    let d = (r.axis.origin - p.origin).dot(p.normal);
    let (x, y, _) = r.frame();
    if d.abs() <= TOL {
        // The plane holds the axis: it meets the surface along its meridians at the two
        // angles square to the plane's normal.
        let w = p.normal.cross(a);
        let theta = w.dot(y).atan2(w.dot(x));
        let mut cuts = Vec::new();
        match r.meridian {
            Meridian::Line { radius, slope } => {
                for t in [theta, theta + std::f64::consts::PI] {
                    let e = x * t.cos() + y * t.sin();
                    // The straight line ρ = radius + slope v, along e at height v.
                    let base = r.axis.origin + e * radius;
                    let dir = (e * slope + a).normalized().unwrap_or(a);
                    let scale = (1.0 + slope * slope).sqrt();
                    // Within the face's own stretch of v, and the boxes.
                    let range = (revolution_face.lo.y * scale, revolution_face.hi.y * scale);
                    let range = clip_line(base, dir, plane_face.min, plane_face.max, range);
                    let (t0, t1) =
                        clip_line(base, dir, revolution_face.min, revolution_face.max, range);
                    if t1 - t0 > TOL {
                        cuts.push(Cut::Line(base + dir * t0, base + dir * t1));
                    }
                }
            }
            Meridian::Circle {
                center: 0.0,
                radius,
            } => {
                // A sphere: a great circle through both poles, starting at one.
                let circle = Circle {
                    center: r.axis.origin,
                    normal: p.normal,
                    radius,
                };
                if circle_meets(&circle, plane_face) {
                    cuts.push(Cut::Circle(circle, r.axis.origin + a * radius));
                }
            }
            Meridian::Circle { .. } => {
                for t in [theta, theta + std::f64::consts::PI] {
                    if let Some(circle) = r.meridian_circle(t)
                        && circle_meets(&circle, plane_face)
                    {
                        cuts.push(Cut::Circle(circle, r.point(t, 0.0)));
                    }
                }
            }
        }
        return Ok(cuts);
    }
    match r.cylinder_radius() {
        Some(radius) => {
            // Lines where the plane crosses the cylinder.
            if d.abs() > radius + TOL {
                return Ok(Vec::new());
            }
            let phi = p.normal.dot(y).atan2(p.normal.dot(x));
            let delta = (-d / radius).clamp(-1.0, 1.0).acos();
            let angles = if delta <= 1e-9 || (std::f64::consts::PI - delta) <= 1e-9 {
                vec![phi + delta]
            } else {
                vec![phi + delta, phi - delta]
            };
            let mut cuts = Vec::new();
            for angle in angles {
                let base = r.point(angle, 0.0);
                let range = (revolution_face.lo.y, revolution_face.hi.y);
                let (t0, t1) = clip_line(base, a, plane_face.min, plane_face.max, range);
                if t1 - t0 > TOL {
                    cuts.push(Cut::Line(base + a * t0, base + a * t1));
                }
            }
            Ok(cuts)
        }
        None => plane_section(plane_face, revolution_face, p, r),
    }
}

/// Where a plane meets a surface of revolution in a curve no chart makes straight or round:
/// a sphere's circle, or a section of a cylinder or a cone that goes all the way round it.
fn plane_section(
    plane_face: &Prepared,
    revolution_face: &Prepared,
    p: &oxyde_geom::Plane,
    r: &Revolution,
) -> Result<Vec<Cut>, NativeError> {
    match r.meridian {
        Meridian::Circle {
            center: 0.0,
            radius,
        } => {
            // A sphere: the circle where the plane cuts it, if it does.
            let d = (r.axis.origin - p.origin).dot(p.normal);
            if d.abs() >= radius - TOL {
                return Ok(Vec::new());
            }
            let circle = Circle {
                center: r.axis.origin - p.normal * d,
                normal: p.normal,
                radius: (radius * radius - d * d).sqrt(),
            };
            Ok(
                if circle_meets(&circle, plane_face) && circle_meets(&circle, revolution_face) {
                    vec![Cut::Circle(circle, seam_start(r, &circle))]
                } else {
                    Vec::new()
                },
            )
        }
        Meridian::Line { .. } => sections(r, &Surface::Plane(*p), plane_face, revolution_face)
            .ok_or(NativeError::Unsupported(
                "intersect a cone with a plane it meets in a curve that does not go round its axis",
            )),
        Meridian::Circle { .. } => Err(NativeError::Unsupported(
            "intersect a torus with a plane that neither holds its axis nor is square to it",
        )),
    }
}

/// Where a whole circle on a surface of revolution should start: where it crosses the
/// surface's angle zero (its seam's half-plane), if it does, else its own angle zero.
fn seam_start(r: &Revolution, circle: &Circle) -> Point3 {
    let (x, y, _) = r.frame();
    let (u, w) = circle.frame();
    // The circle's point at angle t is in the seam's plane (normal y) where
    // A cos t + B sin t = -C.
    let (a, b) = (circle.radius * u.dot(y), circle.radius * w.dot(y));
    let c = (circle.center - r.axis.origin).dot(y);
    let reach = (a * a + b * b).sqrt();
    if reach <= f64::EPSILON || c.abs() > reach {
        return circle.point_at(0.0);
    }
    let (phase, spread) = (b.atan2(a), (-c / reach).acos());
    [phase + spread, phase - spread]
        .into_iter()
        .map(|t| circle.point_at(t))
        .find(|p| (*p - r.axis.origin).dot(x) >= 0.0)
        .unwrap_or_else(|| circle.point_at(0.0))
}

/// The loops where the carrier's straight lines meet `other` all the way round its axis,
/// each a section, those that come near both faces; `None` when one of them does not go all
/// the way round (the curve turns back, or touches itself).
fn sections(carrier: &Revolution, other: &Surface, a: &Prepared, b: &Prepared) -> Option<Vec<Cut>> {
    let mut cuts = Vec::new();
    for branch in 0..2u8 {
        let s = Section {
            carrier: *carrier,
            other: *other,
            branch,
            ccw: true,
        };
        let reaches = (0..360)
            .filter(|i| s.point_at(TAU * f64::from(*i) / 360.0).is_some())
            .count();
        if reaches == 0 {
            break;
        }
        if reaches < 360 || !s.goes_round() {
            return None;
        }
        let start = s.point_at(0.0)?;
        if section_meets(&s, a) && section_meets(&s, b) {
            cuts.push(Cut::Section(s, start));
        }
    }
    Some(cuts)
}

/// Whether a whole loop of a section comes into a face's box.
fn section_meets(s: &Section, face: &Prepared) -> bool {
    let points: Vec<Point3> = (0..128)
        .filter_map(|i| s.point_at(TAU * f64::from(i) / 128.0))
        .collect();
    let (lo, hi) = box_of(points.iter().copied());
    // Samples a 128th of a turn apart fall short of the loop's extremes by a hair.
    let pad = 0.01 * (hi - lo).length() + TOL;
    lo.x <= face.max.x + pad
        && face.min.x <= hi.x + pad
        && lo.y <= face.max.y + pad
        && face.min.y <= hi.y + pad
        && lo.z <= face.max.z + pad
        && face.min.z <= hi.z + pad
}

/// Two surfaces of revolution on different axes, not two parallel cylinders: two spheres
/// meet in a circle; otherwise the loops where the straight lines of a cylinder or a cone
/// (the narrower first) meet the other, when they go all the way round.
fn crossing(
    a: &Prepared,
    b: &Prepared,
    r1: &Revolution,
    r2: &Revolution,
) -> Result<Vec<Cut>, NativeError> {
    if let (
        Meridian::Circle {
            center: 0.0,
            radius: ra,
        },
        Meridian::Circle {
            center: 0.0,
            radius: rb,
        },
    ) = (r1.meridian, r2.meridian)
    {
        let d = r2.axis.origin - r1.axis.origin;
        let dist = d.length();
        if dist <= TOL || dist >= ra + rb - TOL || dist <= (ra - rb).abs() + TOL {
            return Ok(Vec::new());
        }
        let n = d * (1.0 / dist);
        let x = (dist * dist + ra * ra - rb * rb) / (2.0 * dist);
        let circle = Circle {
            center: r1.axis.origin + n * x,
            normal: n,
            radius: (ra * ra - x * x).max(0.0).sqrt(),
        };
        return Ok(if circle_meets(&circle, a) && circle_meets(&circle, b) {
            vec![Cut::Circle(circle, seam_start(r1, &circle))]
        } else {
            Vec::new()
        });
    }
    let torus =
        |r: &Revolution| matches!(r.meridian, Meridian::Circle { center, .. } if center > 0.0);
    if torus(r1) || torus(r2) {
        return Err(NativeError::Unsupported(
            "intersect a torus with a surface of revolution on another axis",
        ));
    }
    let mut carriers: Vec<(&Revolution, &Revolution)> = [(r1, r2), (r2, r1)]
        .into_iter()
        .filter(|(c, _)| matches!(c.meridian, Meridian::Line { .. }))
        .collect();
    carriers.sort_by(|p, q| p.0.size().total_cmp(&q.0.size()));
    for (carrier, other) in carriers {
        if let Some(cuts) = sections(carrier, &Surface::Revolution(*other), a, b) {
            return Ok(cuts);
        }
    }
    Err(NativeError::Unsupported(
        "intersect surfaces of revolution that meet in a curve turning back on itself",
    ))
}

/// Where the meridian of `r` crosses the height `t` along its axis, as `(ρ, z)` with `ρ > 0`.
fn plane_height_crossings(r: &Revolution, t: f64) -> Vec<(f64, f64)> {
    match r.meridian {
        Meridian::Line { radius, slope } => {
            let rho = radius + slope * t;
            if rho > TOL {
                vec![(rho, t)]
            } else {
                Vec::new()
            }
        }
        Meridian::Circle { center, radius } => {
            if t.abs() > radius + TOL {
                return Vec::new();
            }
            let half = (radius * radius - t * t).max(0.0).sqrt();
            let mut out = vec![(center + half, t)];
            if half > TOL {
                out.push((center - half, t));
            }
            out.retain(|&(rho, _)| rho > TOL);
            out
        }
    }
}

/// A cut as segments of `face`'s chart (several for a face that runs round more than once
/// along its unwrapped `x`).
fn cut_into(face: &Prepared, cut: &Cut, other: &Prepared) -> Result<Vec<Segment>, NativeError> {
    let chart = &face.chart;
    let internal = NativeError::Internal;
    Ok(match (cut, chart) {
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
        (Cut::Line(p, q), Chart::Revolution { surface, k, .. }) => {
            // Along a straight line of the surface, at the angle of its middle (an end may
            // be the apex, where the angle is none).
            let mid = *p + (*q - *p) * 0.5;
            let x = surface.params(mid).0.rem_euclid(TAU) * k;
            let (ya, yb) = (chart.to_2d(*p).y, chart.to_2d(*q).y);
            copies(face, x)
                .into_iter()
                .map(|x| Segment::line(P::new(x, ya), P::new(x, yb)))
                .collect()
        }
        (Cut::Circle(circle, _), Chart::Revolution { surface, k, m }) => {
            if let Some(v) = surface.parallel_v(circle, TOL) {
                let y = v * m;
                v_copies(face, y)
                    .into_iter()
                    .map(|y| Segment::line(P::new(face.lo.x, y), P::new(face.hi.x, y)))
                    .collect()
            } else if let Some(theta) = surface.meridian_theta(circle, TOL) {
                let Meridian::Circle { center, .. } = surface.meridian else {
                    return Err(internal(
                        "a meridian circle on a straight meridian".to_owned(),
                    ));
                };
                // A torus's meridian circle goes round its tube whole; a sphere's great circle
                // is two half meridians from pole to pole, half a turn apart.
                let thetas = if center > 0.0 {
                    vec![theta]
                } else {
                    vec![theta, theta + std::f64::consts::PI]
                };
                let (ylo, yhi) = if center > 0.0 {
                    (face.lo.y, face.hi.y)
                } else {
                    (
                        -std::f64::consts::FRAC_PI_2 * m,
                        std::f64::consts::FRAC_PI_2 * m,
                    )
                };
                let mut out = Vec::new();
                for t in thetas {
                    for x in copies(face, t.rem_euclid(TAU) * k) {
                        out.push(Segment::line(P::new(x, ylo), P::new(x, yhi)));
                    }
                }
                out
            } else {
                laid_copies(
                    face,
                    lay_loop(chart, Curve::Circle(*circle), seam_start(surface, circle)),
                )
            }
        }
        (Cut::Section(section, start), _) => {
            laid_copies(face, lay_loop(chart, Curve::Section(*section), *start))
        }
        (Cut::Same, _) => {
            // The other face's boundary, laid out in this face's chart, at each of its
            // places along this face's unwrapped `x`.
            let segments = boundary_2d(chart, &other.loops).map_err(internal)?;
            match chart.period() {
                None => segments,
                Some(_) => {
                    let (lo, _) = box_2d(&segments);
                    let mut out = Vec::new();
                    for x in copies(face, lo.x) {
                        out.extend(segments.iter().map(|s| shifted(s, x - lo.x, 0.0)));
                    }
                    out
                }
            }
        }
    })
}

/// A curve laid out in a face's chart, at each of its places along the face's unwrapped
/// range (whole turns along `x`, and round a ring torus's tube along `y`).
fn laid_copies(face: &Prepared, laid: Vec<Segment>) -> Vec<Segment> {
    let (lo, hi) = box_2d(&laid);
    let shifts = |period: Option<f64>, lo: f64, hi: f64, face_lo: f64, face_hi: f64| -> Vec<f64> {
        match period {
            None => vec![0.0],
            Some(p) => (-2..=2)
                .map(|k| f64::from(k) * p)
                .filter(|&d| lo + d < face_hi - TOL && hi + d > face_lo + TOL)
                .collect(),
        }
    };
    let mut out = Vec::new();
    for dx in shifts(face.chart.period(), lo.x, hi.x, face.lo.x, face.hi.x) {
        for dy in shifts(face.chart.v_period(), lo.y, hi.y, face.lo.y, face.hi.y) {
            out.extend(laid.iter().map(|s| shifted(s, dx, dy)));
        }
    }
    out
}

fn shifted(s: &Segment, dx: f64, dy: f64) -> Segment {
    let mut s = *s;
    s.a.x += dx;
    s.a.y += dy;
    s.b.x += dx;
    s.b.y += dy;
    if let Geom::Arc {
        center,
        radius,
        ccw,
    } = s.geom
    {
        s.geom = Geom::Arc {
            center: P::new(center.x + dx, center.y + dy),
            radius,
            ccw,
        };
    }
    s
}

/// The places `x` (taken within one period) falls along a face's unwrapped range.
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

/// The places `y` falls along a ring torus's face's unwrapped range round its tube.
fn v_copies(face: &Prepared, y: f64) -> Vec<f64> {
    let Some(period) = face.chart.v_period() else {
        return vec![y];
    };
    let mut out = Vec::new();
    let k0 = ((face.lo.y - y) / period).floor() as i64 - 1;
    for k in k0..k0 + 4 {
        let v = y + k as f64 * period;
        if v >= face.lo.y - TOL && v <= face.hi.y + TOL {
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
            surface: self.surface.reversed(),
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
            for c in meet(face, other)? {
                segments.extend(cut_into(face, &c, other)?);
            }
        }
        // A face of revolution is cut at its angle zero too, every period, and a ring
        // torus's round its tube at its v zero.
        if let Some(period) = face.chart.period() {
            let k0 = (face.lo.x / period).floor() as i64;
            for k in k0..=k0 + 2 {
                let x = k as f64 * period;
                if x > face.lo.x - TOL && x < face.hi.x + TOL {
                    segments.push(Segment::line(P::new(x, face.lo.y), P::new(x, face.hi.y)));
                }
            }
        }
        if let Some(period) = face.chart.v_period() {
            let k0 = (face.lo.y / period).floor() as i64;
            for k in k0..=k0 + 2 {
                let y = k as f64 * period;
                if y > face.lo.y - TOL && y < face.hi.y + TOL {
                    segments.push(Segment::line(P::new(face.lo.x, y), P::new(face.hi.x, y)));
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
            // A piece is taken within one period from angle zero (and v zero round a ring
            // torus's tube).
            let shift = match face.chart.period() {
                Some(period) => -(inner.x / period).floor() * period,
                None => 0.0,
            };
            let v_shift = match face.chart.v_period() {
                Some(period) => -(inner.y / period).floor() * period,
                None => 0.0,
            };
            let mut loops = Vec::new();
            for cycle in std::iter::once(&region.outer).chain(&region.holes) {
                let segments: Vec<Segment> = cycle
                    .iter()
                    .map(|&half| shifted(&arrangement.segment(half), shift, v_shift))
                    .collect();
                loops.push(cycle_edges(&face.chart, &segments).map_err(NativeError::Internal)?);
            }
            let point = face
                .chart
                .to_world(P::new(inner.x + shift, inner.y + v_shift));
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

/// `p`, a point of the face's surface, in its chart, its `x` (and a ring torus's `y`) taken
/// within the face's unwrapped range when it can be; `None` when it falls outside that range
/// altogether.
fn on_face(face: &Prepared, p: Point3) -> Option<P> {
    let q = face.chart.to_2d(p);
    let x = *copies(face, q.x).first()?;
    let y = *v_copies(face, q.y).first()?;
    Some(P::new(x, y))
}

/// Where the ray `point + t dir` meets a surface: each `Some(t)`, or `None` when it grazes
/// a plane (runs along it).
fn ray_hits(surface: &Surface, point: Point3, dir: Vec3) -> Vec<Option<f64>> {
    match surface {
        Surface::Plane(p) => {
            let denom = dir.dot(p.normal);
            if denom.abs() < 1e-12 {
                return Vec::new();
            }
            vec![Some((p.origin - point).dot(p.normal) / denom)]
        }
        Surface::Revolution(r) => r.ray_hits(point, dir).into_iter().map(Some).collect(),
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
        (Surface::Revolution(c), Surface::Revolution(d)) => c.same_as(d, TOL),
        _ => false,
    }
}

/// Edges between welded points, told apart by their middles: the same edge, whichever
/// way it is run, gets the same number. A degenerate edge is never shared: each face
/// reaching a pole has its own there, as OpenCASCADE's faces do.
#[derive(Debug, Default)]
struct Edges {
    by_ends: HashMap<(u32, u32), Vec<(Point3, u32)>>,
    count: u32,
}

impl Edges {
    /// The edge's number, and whether it is new.
    fn index(&mut self, points: &mut Points, e: &Edge3) -> (u32, bool) {
        if e.curve.is_degenerate() {
            self.count += 1;
            return (self.count - 1, true);
        }
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
/// face, the edges between them gone, in a chart where they lie in one piece (on a surface
/// of revolution, cut open at angle zero when they go round whole, elsewhere when they do
/// not).
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
            if e.curve.is_degenerate() {
                continue;
            }
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
        if group.len() == 1 && !matches!(first.surface, Surface::Revolution(_)) {
            out.push(Piece {
                sources,
                ..first.clone()
            });
            continue;
        }
        let chart = group_chart(&first.surface, group.iter().map(|&i| &pieces[i]));
        // Every piece's boundary in the group's chart, each walked from a start taken in
        // the chart's period, then moved by whole periods to lie within it (round a ring
        // torus's tube, within a period that starts in a gap between the pieces if they
        // leave one, so that pieces either side of its v zero come together).
        let mut walked: Vec<Vec<Segment>> = Vec::new();
        for &i in &group {
            let mut s = boundary_2d(&chart, &pieces[i].loops).map_err(internal)?;
            if let Some(period) = chart.period() {
                let (lo, _) = box_2d(&s);
                let shift = -((lo.x + TOL) / period).floor() * period;
                s = s.iter().map(|seg| shifted(seg, shift, 0.0)).collect();
            }
            walked.push(s);
        }
        if let Some(period) = chart.v_period() {
            let start = window_start(walked.iter().map(|s| box_2d(s)), period);
            for s in &mut walked {
                let (lo, _) = box_2d(s);
                let shift = -((lo.y + TOL - start) / period).floor() * period;
                *s = s.iter().map(|seg| shifted(seg, 0.0, shift)).collect();
            }
        }
        let mut segments: Vec<Segment> = Vec::new();
        let mut outlines: Vec<Vec<Segment>> = Vec::new();
        for s in walked {
            segments.extend(s.iter().copied());
            outlines.push(s);
        }
        // Edges two pieces share, run once each way, are inside the merged face: for each,
        // the first later one starting near where it ends that runs back along it.
        let cell = |p: P| ((p.x / TOL).floor() as i64, (p.y / TOL).floor() as i64);
        let mut by_start: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, seg) in segments.iter().enumerate() {
            by_start.entry(cell(seg.a)).or_default().push(i);
        }
        let mut gone = vec![false; segments.len()];
        for i in 0..segments.len() {
            if gone[i] {
                continue;
            }
            let (cx, cy) = cell(segments[i].b);
            let mut candidates: Vec<usize> = Vec::new();
            for dx in -1..=1 {
                for dy in -1..=1 {
                    candidates.extend(by_start.get(&(cx + dx, cy + dy)).into_iter().flatten());
                }
            }
            candidates.sort_unstable();
            if let Some(&j) = candidates
                .iter()
                .find(|&&j| j > i && !gone[j] && opposite(&segments[i], &segments[j]))
            {
                gone[i] = true;
                gone[j] = true;
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
            // The merged faces only: not what their holes enclose.
            let covered = arrangement.inside_point(&region).is_some_and(|(p, _)| {
                outlines
                    .iter()
                    .any(|outline| arrangement::inside(outline, p, TOL))
            });
            if !covered {
                continue;
            }
            let mut loops = Vec::new();
            for cycle in std::iter::once(&region.outer).chain(&region.holes) {
                let segments: Vec<Segment> = cycle
                    .iter()
                    .map(|&half| arrangement.segment(half))
                    .collect();
                loops.push(cycle_edges(&chart, &segments).map_err(internal)?);
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

/// Where a window of one `period` along `y` should start so that none of the stretches
/// `boxes` cover (each taken within one period) is split by its ends: in the middle of the
/// widest gap between them, or at 0 when they leave none.
fn window_start(boxes: impl Iterator<Item = (P, P)>, period: f64) -> f64 {
    let mut spans: Vec<(f64, f64)> = boxes
        .map(|(lo, hi)| {
            let shift = -((lo.y + TOL) / period).floor() * period;
            (lo.y + shift, hi.y + shift)
        })
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
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
    let mut best: Option<(f64, f64)> = None;
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
    best.map_or(0.0, |(from, to)| (from + to) / 2.0)
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
/// surface of revolution turned so that its angle zero falls in a gap between the pieces
/// when they leave one (so that a face crossing angle zero is not cut there), at the
/// surface's own angle zero when they go round whole (where its seam goes).
fn group_chart<'a>(surface: &Surface, group: impl Iterator<Item = &'a Piece>) -> Chart {
    let Surface::Revolution(r) = surface else {
        return Chart::of(surface);
    };
    let chart = Chart::revolution(*r);
    let (Some(period), Chart::Revolution { k, .. }) = (chart.period(), chart) else {
        return chart;
    };
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
            let turn = ((from + to) / 2.0) / k;
            let (x, y, _) = r.frame();
            Chart::revolution(Revolution {
                x_dir: x * turn.cos() + y * turn.sin(),
                ..*r
            })
        }
        None => chart,
    }
}

/// The faces welded into solids: corners and edges shared, edges along one line or circle
/// between the same two faces joined, each connected set of faces a shell, and each shell
/// facing inwards a void of the solid it lies in.
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
                    if solid.edges.len() != edge as usize {
                        return Err(internal("the edges were numbered out of order"));
                    }
                    solid.edges.push(Edge::new(i, j, e.curve));
                }
                let stored = solid.edges[edge as usize];
                // Run its own way when it starts where this use starts, and for a closed
                // circle or section when it turns the same way.
                let reversed = if stored.start == stored.end {
                    !same_way(&stored.curve, &e.curve)
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
    solid.vertices = points.points.into_iter().map(Vertex::new).collect();
    join_edges(&mut solid);
    let (shells, shell_sources) = split_solids(solid, sources).map_err(|e| internal(&e))?;
    let (solids, solid_sources) = place_voids(shells, shell_sources)?;
    let mut shape = Shape { solids };
    for s in &mut shape.solids {
        s.settle_tolerances().map_err(NativeError::Internal)?;
    }
    Ok(Tagged {
        shape,
        sources: solid_sources,
    })
}

/// The shells of a result made solids: each shell facing outwards a solid, each facing
/// inwards (a negative volume) a void of the smallest solid holding it. The faces' sources
/// follow their faces.
fn place_voids(
    shells: Vec<Solid>,
    sources: Vec<Vec<Source>>,
) -> Result<(Vec<Solid>, Vec<Vec<Source>>), NativeError> {
    let internal = NativeError::Internal;
    let mut offset = 0;
    let mut parts: Vec<(Solid, Vec<Vec<Source>>, f64)> = Vec::new();
    for shell in shells {
        let n = shell.faces.len();
        let volume = Shape {
            solids: vec![shell.clone()],
        }
        .volume()
        .map_err(internal)?;
        parts.push((shell, sources[offset..offset + n].to_vec(), volume));
        offset += n;
    }
    let (solids, voids): (Vec<_>, Vec<_>) = parts.into_iter().partition(|(_, _, v)| *v >= 0.0);
    let mut solids: Vec<(Solid, Vec<Vec<Source>>, f64)> = solids;
    for (void, void_sources, _) in voids {
        let probe = void
            .vertices
            .first()
            .map(|v| v.point)
            .ok_or_else(|| internal("a void with no vertex".to_owned()))?;
        // The smallest solid the void's corner is inside.
        let mut holder: Option<usize> = None;
        for (i, (solid, _, volume)) in solids.iter().enumerate() {
            let faces = prepare(&Tagged {
                shape: Shape {
                    solids: vec![solid.clone()],
                },
                sources: Vec::new(),
            })?;
            if classify(probe, Vec3::Z, &faces)? == Where::Inside
                && holder.is_none_or(|h| *volume < solids[h].2)
            {
                holder = Some(i);
            }
        }
        let Some(h) = holder else {
            return Err(internal(
                "a shell facing inwards lies in no solid".to_owned(),
            ));
        };
        let (solid, solid_sources, _) = &mut solids[h];
        add_shell(solid, &void);
        solid_sources.extend(void_sources);
    }
    let mut out = Vec::new();
    let mut out_sources = Vec::new();
    for (solid, s, _) in solids {
        out.push(solid);
        out_sources.extend(s);
    }
    Ok((out, out_sources))
}

/// Adds `void`'s faces, edges and vertices to `solid` as a shell of its own.
fn add_shell(solid: &mut Solid, void: &Solid) {
    let (v0, e0, f0) = (
        solid.vertices.len() as u32,
        solid.edges.len() as u32,
        solid.faces.len() as u32,
    );
    if solid.shells.is_empty() {
        solid.shells.push((0..f0).collect());
    }
    solid.vertices.extend(void.vertices.iter().copied());
    solid.edges.extend(void.edges.iter().map(|e| Edge {
        start: e.start + v0,
        end: e.end + v0,
        ..*e
    }));
    solid.faces.extend(void.faces.iter().map(|f| {
        Face {
            surface: f.surface,
            loops: f
                .loops
                .iter()
                .map(|l| {
                    l.iter()
                        .map(|u| EdgeUse {
                            edge: u.edge + e0,
                            reversed: u.reversed,
                        })
                        .collect()
                })
                .collect(),
        }
    }));
    solid
        .shells
        .push((f0..f0 + void.faces.len() as u32).collect());
}

/// Whether two curves of one closed edge (a circle, a section, a degenerate edge) turn the
/// same way round.
fn same_way(a: &Curve, b: &Curve) -> bool {
    match (a, b) {
        (Curve::Circle(s), Curve::Circle(t)) => s.normal.dot(t.normal) > 0.0,
        (Curve::Section(s), Curve::Section(t)) => s.ccw == t.ccw,
        (Curve::Degenerate { axis: s, .. }, Curve::Degenerate { axis: t, .. }) => s.dot(*t) > 0.0,
        _ => true,
    }
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
        (Curve::Section(c), Curve::Section(d)) => Section { ccw: d.ccw, ..c } == d,
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
                    let curve = solid.edges[u.edge as usize].curve;
                    let curve = if u.reversed { curve.reversed() } else { curve };
                    new_edge = Some(Edge::new(start, end, curve));
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
                        // A closed circle or section: the way it turns.
                        let d = snapshot.edges[u.edge as usize].curve;
                        let d = if u.reversed { d.reversed() } else { d };
                        !same_way(&new_edge.curve, &d)
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

/// Each set of faces joined by edges, a shell of its own, renumbered; the faces' sources
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
        let mut part = Solid::closed(
            solid.vertices.clone(),
            solid.edges.clone(),
            members[&r]
                .iter()
                .map(|&f| solid.faces[f].clone())
                .collect(),
        );
        compact(&mut part);
        out_sources.extend(members[&r].iter().map(|&f| sources[f].clone()));
        solids.push(part);
    }
    Ok((solids, out_sources))
}

#[cfg(test)]
mod tests {
    use oxyde_geom::{Axis, Curve2, Plane, Point2, Profile, ProfileSegment, Vec3};

    use super::*;

    fn polygon(corners: &[(f64, f64)]) -> Profile {
        let n = corners.len();
        let segments: Vec<ProfileSegment> = (0..n)
            .map(|i| ProfileSegment {
                id: i as u32 + 1,
                curve: Curve2::Line {
                    start: Point2::new(corners[i].0, corners[i].1),
                    end: Point2::new(corners[(i + 1) % n].0, corners[(i + 1) % n].1),
                },
            })
            .collect();
        Profile::from_curves(&segments).unwrap()
    }

    #[test]
    fn a_ring_turned_through_a_block_cuts_a_channel_out_of_it() {
        // A ring about the Y axis, which lies on the block's bottom, its seams there too,
        // and wide enough to come out of the block's top.
        let block = crate::build::extrude(
            &Plane::FRONT,
            &polygon(&[(-20.0, -15.0), (20.0, -15.0), (20.0, 15.0), (-20.0, 15.0)]),
            Vec3::new(0.0, 0.0, 10.0),
        )
        .unwrap()
        .0;
        let ring = crate::revolve::revolve(
            &Plane::FRONT,
            &polygon(&[(8.0, 2.0), (12.0, 2.0), (12.0, 6.0), (8.0, 6.0)]),
            &Axis::new(Point3::ORIGIN, Vec3::Y).unwrap(),
            TAU,
        )
        .unwrap()
        .0;
        let (cut, _) = boolean(BooleanOp::Cut, &block, &[&ring]).unwrap();
        cut.check().unwrap();
        let (common, _) = boolean(BooleanOp::Common, &block, &[&ring]).unwrap();
        common.check().unwrap();
        // What the cut takes away is what the two share.
        let total = cut.volume().unwrap() + common.volume().unwrap();
        assert!((total - 12_000.0).abs() < 1e-9, "{total}");
        // Half the ring is in the block, but for the caps its top cuts off.
        let half_ring = std::f64::consts::PI * (144.0 - 64.0) * 4.0 / 2.0;
        assert!(common.volume().unwrap() < half_ring);
    }

    fn circle_profile(cx: f64, cy: f64, r: f64) -> Profile {
        Profile::from_curves(&[ProfileSegment {
            id: 1,
            curve: Curve2::Circle {
                center: Point2::new(cx, cy),
                radius: r,
            },
        }])
        .unwrap()
    }

    /// `∫ f` from `a` to `b` by the midpoint rule on `n` pieces.
    fn midpoint_rule(a: f64, b: f64, n: usize, f: impl Fn(f64) -> f64) -> f64 {
        let h = (b - a) / n as f64;
        (0..n).map(|i| f(a + h * (i as f64 + 0.5))).sum::<f64>() * h
    }

    #[test]
    fn holes_across_each_other_and_through_a_bar_are_exact() {
        // A 40 × 30 × 10 block with a hole of radius 4 down through it at (20, 15), and one of
        // radius 3 across it along X through the first's axis, at z = 5.
        let block = crate::build::extrude(
            &Plane::FRONT,
            &polygon(&[(0.0, 0.0), (40.0, 0.0), (40.0, 30.0), (0.0, 30.0)]),
            Vec3::new(0.0, 0.0, 10.0),
        )
        .unwrap()
        .0;
        let top = Plane {
            origin: Point3::new(0.0, 0.0, 10.0),
            ..Plane::FRONT
        };
        let down = crate::build::extrude(
            &top,
            &circle_profile(20.0, 15.0, 4.0),
            Vec3::new(0.0, 0.0, -12.0),
        )
        .unwrap()
        .0;
        let (drilled, _) = boolean(BooleanOp::Cut, &block, &[&down]).unwrap();
        let side = Plane {
            origin: Point3::new(-1.0, 0.0, 0.0),
            x_dir: Vec3::new(0.0, 0.0, -1.0),
            normal: Vec3::X,
        };
        let across = crate::build::extrude(
            &side,
            &circle_profile(-5.0, 15.0, 3.0),
            Vec3::new(42.0, 0.0, 0.0),
        )
        .unwrap()
        .0;
        let (both, _) = boolean(BooleanOp::Cut, &drilled, &[&across]).unwrap();
        both.check().unwrap();
        let counts = both.counts();
        assert_eq!((counts.faces, counts.edges), (9, 23), "{counts:?}");
        // The first hole's wall: its whole less the two ovals the second cuts out of it,
        // each (with t its angle) 2 sqrt(9 − 16 sin² t) high, 4 dt wide.
        let oval = midpoint_rule(
            -std::f64::consts::FRAC_PI_2,
            std::f64::consts::FRAC_PI_2,
            20_000,
            |u| {
                // t = asin(3/4 sin u) takes the root's end away.
                let t = (0.75 * u.sin()).asin();
                let dt = 0.75 * u.cos() / (1.0 - (0.75 * u.sin()).powi(2)).sqrt();
                8.0 * (9.0 - 16.0 * t.sin().powi(2)).max(0.0).sqrt() * dt
            },
        );
        let wall = std::f64::consts::TAU * 4.0 * 10.0 - 2.0 * oval;
        let topology = both.topology().unwrap();
        let area = topology
            .faces
            .iter()
            .find(|f| {
                f.axis
                    .is_some_and(|a| a.direction.cross(Vec3::Z).length() < 1e-9)
            })
            .unwrap()
            .area;
        assert!((area - wall).abs() < 1e-7 * wall, "{area} {wall}");

        // A round bar of radius 10 and length 30 along Z, a hole of radius 3 through it along
        // X at half its length: what goes is ∫ 2 √(100 − y²) · 2 √(9 − y²) dy.
        let bar = crate::build::extrude(
            &Plane::FRONT,
            &circle_profile(0.0, 0.0, 10.0),
            Vec3::new(0.0, 0.0, 30.0),
        )
        .unwrap()
        .0;
        let side = Plane {
            origin: Point3::new(-15.0, 0.0, 0.0),
            x_dir: Vec3::new(0.0, 0.0, -1.0),
            normal: Vec3::X,
        };
        let hole = crate::build::extrude(
            &side,
            &circle_profile(-15.0, 0.0, 3.0),
            Vec3::new(30.0, 0.0, 0.0),
        )
        .unwrap()
        .0;
        let (drilled, _) = boolean(BooleanOp::Cut, &bar, &[&hole]).unwrap();
        drilled.check().unwrap();
        let gone = midpoint_rule(
            -std::f64::consts::FRAC_PI_2,
            std::f64::consts::FRAC_PI_2,
            20_000,
            |u| {
                let y = 3.0 * u.sin();
                4.0 * (100.0 - y * y).sqrt() * 9.0 * u.cos() * u.cos()
            },
        );
        let expected = std::f64::consts::PI * 100.0 * 30.0 - gone;
        let volume = drilled.volume().unwrap();
        assert!(
            (volume - expected).abs() < 1e-9 * expected,
            "{volume} {expected}"
        );
    }

    #[test]
    #[ignore]
    fn time_a_radial_hole() {
        let t = std::time::Instant::now();
        let bar = crate::build::extrude(
            &Plane::FRONT,
            &circle_profile(0.0, 0.0, 10.0),
            Vec3::new(0.0, 0.0, 30.0),
        )
        .unwrap()
        .0;
        let side = Plane {
            origin: Point3::new(-15.0, 0.0, 0.0),
            x_dir: Vec3::new(0.0, 0.0, -1.0),
            normal: Vec3::X,
        };
        let hole = crate::build::extrude(
            &side,
            &circle_profile(-15.0, 0.0, 3.0),
            Vec3::new(30.0, 0.0, 0.0),
        )
        .unwrap()
        .0;
        println!("build {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let (drilled, _) = boolean(BooleanOp::Cut, &bar, &[&hole]).unwrap();
        println!("boolean {:?}", t.elapsed());
        let t = std::time::Instant::now();
        drilled.check().unwrap();
        println!("check {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let _ = drilled.topology().unwrap();
        println!("topology {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let _ = drilled.volume().unwrap();
        println!("volume {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let _ = drilled.bounds().unwrap();
        println!("bounds {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let _ = crate::tessellate::mesh(&drilled, 0.03).unwrap();
        println!("mesh {:?}", t.elapsed());
    }
}
