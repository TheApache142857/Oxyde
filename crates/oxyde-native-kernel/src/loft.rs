//! Lofts through closed contours, as OpenCASCADE's `BRepOffsetAPI_ThruSections` builds them
//! (observed as a black box; docs/NATIVE_KERNEL.md §4):
//!
//! - **Sections matched.** Sections with as many edges each are matched corner to corner,
//!   each one's start and way round chosen to keep its corners nearest the previous one's
//!   (the least sum of the distances). Sections with different numbers of edges are cut at
//!   the polar angles, about each one's centroid (along its perimeter), of every section's
//!   corners, so that all have one corner at each angle; the first section's first corner
//!   starts them all.
//! - **Edges as polynomials.** A line is of degree one; an arc takes the polynomial form
//!   [`crate::nurbs::polynomial_arc`] gives it, in proportion to its angle.
//! - **Ruled** (and any loft of two sections): a face for each pair of edges from one
//!   section to the next, straight between them: a cone or a cylinder between circles on one
//!   axis, else a B-spline surface of degree one across. The sections' edges are their exact
//!   lines and circles; the edges between faces are lines, as B-splines.
//! - **Smooth** (three sections to nine): one surface for each edge through every section,
//!   the polynomial of degree one less than the number of sections across them, each
//!   section at its share of the length of the polygon of the sections' poles (the root
//!   mean square of the distances between corresponding poles, the sections' edges raised
//!   to one degree and their knots to one set). The end sections' edges are the surfaces'
//!   B-spline curves there, the edges between faces their B-spline curves across.
//! - Faces in OpenCASCADE's order: the side faces span after span (smooth: one span), edge
//!   after edge from the first corner; then the first section's cap and the last's. A side
//!   face holding the middle of one of the first section's edges is named after it, as the
//!   OpenCASCADE kernel names it.

// Indices into sections, edges and poles gathered here.
#![allow(clippy::indexing_slicing)]

use std::f64::consts::TAU;

use oxyde_geom::{Axis, Curve2, Loop, Plane, Point2, Point3, Vec3};
use oxyde_kernel::{FaceSource, OpHistory};

use crate::NativeError;
use crate::brep::{Edge, EdgeUse, Face, Shape, Solid, Vertex};
use crate::build::{circumcentre, turn};
use crate::geometry::{Circle, Curve, SplineCurve, SplineSurface, Surface};
use crate::nurbs::{BSplineCurve, BSplineSurface, bernstein, elevated, polynomial_arc, solve};
use crate::revolution::{Meridian, Revolution};

/// The most sections a smooth loft goes through: OpenCASCADE approximates rather than
/// interpolates through more (observed), which is not written yet.
const MAX_SMOOTH_SECTIONS: usize = 9;

/// One edge of a section, in space.
#[derive(Debug, Clone, Copy)]
enum SectionEdge {
    Line {
        a: Point3,
        b: Point3,
    },
    /// An arc about `center`, counter-clockwise about `normal`, from `a` turning `sweep` (a
    /// whole turn for a circle).
    Arc {
        center: Point3,
        normal: Vec3,
        radius: f64,
        a: Point3,
        sweep: f64,
    },
}

impl SectionEdge {
    /// The arc's angle-zero and quarter-turn directions: towards its start, and on.
    fn frame(center: Point3, normal: Vec3, radius: f64, a: Point3) -> (Vec3, Vec3) {
        let x = (a - center) * (1.0 / radius);
        (x, normal.cross(x))
    }

    fn start(&self) -> Point3 {
        match *self {
            SectionEdge::Line { a, .. } | SectionEdge::Arc { a, .. } => a,
        }
    }

    /// Its point at fraction `f` of the way along (of its length, or its angle).
    fn at(&self, f: f64) -> Point3 {
        match *self {
            SectionEdge::Line { a, b } => a + (b - a) * f,
            SectionEdge::Arc {
                center,
                normal,
                radius,
                a,
                sweep,
            } => {
                let (x, y) = SectionEdge::frame(center, normal, radius, a);
                let t = sweep * f;
                center + (x * t.cos() + y * t.sin()) * radius
            }
        }
    }

    fn end(&self) -> Point3 {
        match *self {
            SectionEdge::Line { b, .. } => b,
            SectionEdge::Arc { sweep, a, .. } if sweep >= TAU - 1e-12 => a,
            _ => self.at(1.0),
        }
    }

    fn reversed(&self) -> SectionEdge {
        match *self {
            SectionEdge::Line { a, b } => SectionEdge::Line { a: b, b: a },
            SectionEdge::Arc {
                center,
                normal,
                radius,
                sweep,
                ..
            } => SectionEdge::Arc {
                center,
                normal: -normal,
                radius,
                a: self.end(),
                sweep,
            },
        }
    }

    /// The piece of it between fractions `f0` and `f1`.
    fn piece(&self, f0: f64, f1: f64) -> SectionEdge {
        match *self {
            SectionEdge::Line { .. } => SectionEdge::Line {
                a: self.at(f0),
                b: self.at(f1),
            },
            SectionEdge::Arc {
                center,
                normal,
                radius,
                sweep,
                ..
            } => SectionEdge::Arc {
                center,
                normal,
                radius,
                a: self.at(f0),
                sweep: sweep * (f1 - f0),
            },
        }
    }

    fn length(&self) -> f64 {
        match *self {
            SectionEdge::Line { a, b } => a.distance(b),
            SectionEdge::Arc { radius, sweep, .. } => radius * sweep,
        }
    }

    /// The centroid of the edge as a curve.
    fn centroid(&self) -> Point3 {
        match *self {
            SectionEdge::Line { a, b } => a + (b - a) * 0.5,
            SectionEdge::Arc {
                center,
                radius,
                sweep,
                ..
            } => {
                if sweep >= TAU - 1e-12 {
                    return center;
                }
                // On the arc's bisector, r sin(θ/2) / (θ/2) from its centre.
                let _ = radius;
                let half = sweep / 2.0;
                center + (self.at(0.5) - center) * (half.sin() / half)
            }
        }
    }

    /// Its polynomial form: Bézier spans, as OpenCASCADE's loft makes it.
    fn spans(&self) -> Vec<Vec<Point3>> {
        match *self {
            SectionEdge::Line { a, b } => vec![vec![a, b]],
            SectionEdge::Arc {
                center,
                normal,
                radius,
                a,
                sweep,
            } => {
                let (x, y) = SectionEdge::frame(center, normal, radius, a);
                polynomial_arc(center, x, y, radius, 0.0, sweep)
            }
        }
    }

    /// The exact curve of an edge on it, and whether it closes on itself.
    fn curve(&self) -> (Curve, bool) {
        match *self {
            SectionEdge::Line { .. } => (Curve::Line, false),
            SectionEdge::Arc {
                center,
                normal,
                radius,
                sweep,
                ..
            } => (
                Curve::Circle(Circle {
                    center,
                    normal,
                    radius,
                }),
                sweep >= TAU - 1e-12,
            ),
        }
    }
}

/// A section's frame: two directions in its plane and its normal, along the loft.
#[derive(Debug, Clone, Copy)]
struct Frame {
    x: Vec3,
    y: Vec3,
    n: Vec3,
}

impl Frame {
    /// The frame turned by the least rotation that takes its normal to `n`.
    fn carried_to(&self, n: Vec3) -> Frame {
        let axis = self.n.cross(n);
        let s = axis.length();
        let c = self.n.dot(n);
        let Some(k) = axis.normalized() else {
            return Frame { n, ..*self };
        };
        let angle = s.atan2(c);
        let rotate = |v: Vec3| {
            let (sn, cs) = angle.sin_cos();
            v * cs + k.cross(v) * sn + k * (k.dot(v) * (1.0 - cs))
        };
        Frame {
            x: rotate(self.x),
            y: rotate(self.y),
            n,
        }
    }
}

/// A section's edges in space, as OpenCASCADE's kernel makes them from the sketch: lines,
/// arcs from their start through their middle to their end, circles counter-clockwise
/// about the plane's normal from its angle zero.
fn section_edges(plane: &Plane, contour: &Loop) -> Result<Vec<(u32, SectionEdge)>, NativeError> {
    contour
        .segments
        .iter()
        .map(|segment| {
            let edge = match segment.curve {
                Curve2::Line { start, end } => SectionEdge::Line {
                    a: plane.to_world(start),
                    b: plane.to_world(end),
                },
                Curve2::Arc { start, mid, end } => {
                    let c = circumcentre(start, mid, end).ok_or_else(|| {
                        NativeError::Failed(
                            "an arc of a loft section is a straight line".to_owned(),
                        )
                    })?;
                    let ccw = (mid.x - start.x) * (end.y - mid.y)
                        - (mid.y - start.y) * (end.x - mid.x)
                        > 0.0;
                    SectionEdge::Arc {
                        center: plane.to_world(c),
                        normal: if ccw { plane.normal } else { -plane.normal },
                        radius: start.distance(c),
                        a: plane.to_world(start),
                        sweep: turn(c, start, end, ccw),
                    }
                }
                Curve2::Circle { center, radius } => SectionEdge::Arc {
                    center: plane.to_world(center),
                    normal: plane.normal,
                    radius,
                    a: plane.to_world(Point2::new(center.x + radius, center.y)),
                    sweep: TAU,
                },
            };
            Ok((segment.id, edge))
        })
        .collect()
}

/// The same section run the other way from the same first corner.
fn reversed_section(edges: &[SectionEdge]) -> Vec<SectionEdge> {
    edges.iter().rev().map(SectionEdge::reversed).collect()
}

/// The section's centroid along its perimeter.
fn perimeter_centroid(edges: &[SectionEdge]) -> Point3 {
    let total: f64 = edges.iter().map(SectionEdge::length).sum();
    let sum = edges.iter().fold(Vec3::ZERO, |acc, e| {
        acc + e.centroid().to_vec() * e.length()
    });
    let c = sum * (1.0 / total.max(1e-300));
    Point3::new(c.x, c.y, c.z)
}

/// The angle of `p` about `c` in `frame`, in `[0, 2π)`.
fn polar(frame: &Frame, c: Point3, p: Point3) -> f64 {
    let d = p - c;
    d.dot(frame.y).atan2(d.dot(frame.x)).rem_euclid(TAU)
}

/// Twice the signed area the section's points span in `frame`, positive counter-clockwise.
fn winding(frame: &Frame, edges: &[SectionEdge]) -> f64 {
    let points: Vec<(f64, f64)> = edges
        .iter()
        .flat_map(|e| {
            let n = match e {
                SectionEdge::Line { .. } => 1,
                SectionEdge::Arc { .. } => 32,
            };
            (0..n).map(move |i| e.at(f64::from(i) / f64::from(n)))
        })
        .map(|p| (p.to_vec().dot(frame.x), p.to_vec().dot(frame.y)))
        .collect();
    let n = points.len();
    (0..n)
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum()
}

/// Where along the section the ray from `c` at angle `angle` (in `frame`) crosses it: the
/// edge and the fraction along it.
fn ray_crossing(
    frame: &Frame,
    c: Point3,
    edges: &[SectionEdge],
    angle: f64,
) -> Option<(usize, f64)> {
    let local = |p: Point3| {
        let d = p - c;
        (d.dot(frame.x), d.dot(frame.y))
    };
    let dir = (angle.cos(), angle.sin());
    let mut best: Option<(f64, usize, f64)> = None;
    let mut consider = |t: f64, i: usize, f: f64| {
        if t > 0.0 && best.is_none_or(|b| t > b.0) {
            best = Some((t, i, f));
        }
    };
    for (i, e) in edges.iter().enumerate() {
        match *e {
            SectionEdge::Line { a, b } => {
                let (pa, pb) = (local(a), local(b));
                let d = (pb.0 - pa.0, pb.1 - pa.1);
                let det = d.0 * (-dir.1) - d.1 * (-dir.0);
                if det.abs() < 1e-300 {
                    continue;
                }
                // pa + f d = t dir.
                let f = ((-pa.0) * (-dir.1) - (-pa.1) * (-dir.0)) / det;
                let t = (d.0 * (-pa.1) - d.1 * (-pa.0)) / det;
                if (-1e-12..=1.0 + 1e-12).contains(&f) {
                    consider(t, i, f.clamp(0.0, 1.0));
                }
            }
            SectionEdge::Arc { .. } => {
                // The polar angle along the arc, in samples, then bisection.
                let n = 256;
                let angle_at = |f: f64| polar(frame, c, e.at(f));
                let diff = |f: f64| {
                    let d = angle_at(f) - angle;
                    (d + std::f64::consts::PI).rem_euclid(TAU) - std::f64::consts::PI
                };
                for k in 0..n {
                    let (f0, f1) = (f64::from(k) / f64::from(n), f64::from(k + 1) / f64::from(n));
                    let (d0, d1) = (diff(f0), diff(f1));
                    if d0 == 0.0 {
                        let p = local(e.at(f0));
                        consider((p.0 * p.0 + p.1 * p.1).sqrt(), i, f0);
                        continue;
                    }
                    if (d0 < 0.0) == (d1 < 0.0) || (d1 - d0).abs() > 1.0 {
                        continue;
                    }
                    let (mut lo, mut hi) = (f0, f1);
                    for _ in 0..80 {
                        let mid = 0.5 * (lo + hi);
                        if (diff(mid) < 0.0) == (d0 < 0.0) {
                            lo = mid;
                        } else {
                            hi = mid;
                        }
                    }
                    let f = 0.5 * (lo + hi);
                    let p = local(e.at(f));
                    consider((p.0 * p.0 + p.1 * p.1).sqrt(), i, f);
                }
            }
        }
    }
    best.map(|b| (b.1, b.2))
}

/// The sections cut at every section's corners' polar angles, about each one's perimeter
/// centroid in its frame, each run counter-clockwise from the first section's first
/// corner's angle.
fn polar_match(sections: &mut [Vec<SectionEdge>], frames: &[Frame]) -> Result<(), NativeError> {
    let fail = || {
        NativeError::Failed("the loft's sections cannot be matched round their centres".to_owned())
    };
    for (k, edges) in sections.iter_mut().enumerate() {
        if winding(&frames[k], edges) < 0.0 {
            *edges = reversed_section(edges);
        }
    }
    let centres: Vec<Point3> = sections.iter().map(|s| perimeter_centroid(s)).collect();
    let mut angles: Vec<f64> = Vec::new();
    for (k, edges) in sections.iter().enumerate() {
        for e in edges {
            angles.push(polar(&frames[k], centres[k], e.start()));
        }
    }
    let start = polar(&frames[0], centres[0], sections[0][0].start());
    let from = |a: f64| (a - start).rem_euclid(TAU);
    angles.sort_by(|a, b| from(*a).total_cmp(&from(*b)));
    angles.dedup_by(|a, b| {
        let d = (*a - *b).rem_euclid(TAU);
        d.min(TAU - d) <= 1e-9
    });
    for (k, edges) in sections.iter_mut().enumerate() {
        let hits: Vec<(usize, f64)> = angles
            .iter()
            .map(|&a| {
                // A corner of this section at that angle is that corner exactly.
                edges
                    .iter()
                    .position(|e| {
                        let d = (polar(&frames[k], centres[k], e.start()) - a).rem_euclid(TAU);
                        d.min(TAU - d) <= 1e-9
                    })
                    .map(|i| (i, 0.0))
                    .or_else(|| ray_crossing(&frames[k], centres[k], edges, a))
                    .ok_or_else(fail)
            })
            .collect::<Result<_, _>>()?;
        let n = hits.len();
        let mut out = Vec::with_capacity(n);
        for j in 0..n {
            let (i0, f0) = hits[j];
            let (i1, f1) = hits[(j + 1) % n];
            let piece = if i0 == i1 && f1 > f0 {
                edges[i0].piece(f0, f1)
            } else if f1 == 0.0 && (i0 + 1) % edges.len() == i1 {
                edges[i0].piece(f0, 1.0)
            } else if edges.len() == 1 && f1 <= f0 {
                // Round a one-edge section past its corner.
                edges[i0].piece(f0, 1.0 + f1)
            } else {
                return Err(fail());
            };
            out.push(piece);
        }
        *edges = out;
    }
    Ok(())
}

/// Each section after the first started and run round to keep its corners nearest the
/// previous section's: of every start and both ways, the least sum of distances between
/// corresponding corners (the first found of equal ones).
fn align(sections: &mut [Vec<SectionEdge>]) {
    for k in 1..sections.len() {
        let previous: Vec<Point3> = sections[k - 1].iter().map(SectionEdge::start).collect();
        let n = previous.len();
        let candidates = [sections[k].clone(), reversed_section(&sections[k])];
        let mut best: Option<(f64, Vec<SectionEdge>)> = None;
        for edges in &candidates {
            for shift in 0..n {
                let turned: Vec<SectionEdge> = (0..n).map(|i| edges[(i + shift) % n]).collect();
                let cost: f64 = turned
                    .iter()
                    .zip(&previous)
                    .map(|(e, &p)| e.start().distance(p))
                    .sum();
                if best
                    .as_ref()
                    .is_none_or(|b| cost < b.0 - 1e-9 * (1.0 + b.0))
                {
                    best = Some((cost, turned));
                }
            }
        }
        if let Some((_, edges)) = best {
            sections[k] = edges;
        }
    }
}

/// A section's edges as polynomial pieces, all to degree `degree`, each edge's pieces split
/// where `splits` says (fractions of the edge), so that every section's edge `i` has the same
/// pieces.
fn pieces_of(spans: &[Vec<Point3>], splits: &[f64], degree: usize) -> Vec<Vec<Point3>> {
    // The edge's spans in its own fractions.
    let n = spans.len();
    let mut out = Vec::new();
    for (k, span) in spans.iter().enumerate() {
        let (a, b) = (k as f64 / n as f64, (k + 1) as f64 / n as f64);
        let mut cuts: Vec<f64> = splits
            .iter()
            .copied()
            .filter(|&s| s > a + 1e-12 && s < b - 1e-12)
            .collect();
        cuts.sort_by(f64::total_cmp);
        let mut rest = span.clone();
        let mut at = a;
        for c in cuts {
            let t = (c - at) / (b - at);
            let (left, right) = split_bezier(&rest, t);
            out.push(left);
            rest = right;
            at = c;
        }
        out.push(rest);
    }
    out.into_iter()
        .map(|mut p| {
            while p.len() < degree + 1 {
                p = elevated(&p);
            }
            p
        })
        .collect()
}

/// A Bézier curve cut in two at `t`, by de Casteljau's construction.
fn split_bezier(poles: &[Point3], t: f64) -> (Vec<Point3>, Vec<Point3>) {
    let n = poles.len();
    let mut work = poles.to_vec();
    let mut left = Vec::with_capacity(n);
    let mut right = vec![Point3::ORIGIN; n];
    for r in 0..n {
        left.push(work[0]);
        right[n - 1 - r] = work[n - 1 - r];
        for k in 0..(n - 1 - r) {
            work[k] = work[k] + (work[k + 1] - work[k]) * t;
        }
    }
    (left, right)
}

/// Where an edge's spans start and end, as fractions of it.
fn span_fractions(spans: usize) -> Vec<f64> {
    (1..spans).map(|k| k as f64 / spans as f64).collect()
}

/// The B-spline curve of polynomial pieces of one degree joined end to end over `[0, 1]`,
/// each joint at its fraction a knot of full multiplicity.
fn joined(pieces: &[Vec<Point3>], fractions: &[f64]) -> BSplineCurve {
    let d = pieces[0].len() - 1;
    let mut knots = vec![0.0; d + 1];
    let mut poles = pieces[0].clone();
    for (k, piece) in pieces.iter().enumerate().skip(1) {
        knots.extend(std::iter::repeat_n(fractions[k - 1], d));
        poles.extend_from_slice(&piece[1..]);
    }
    knots.extend(std::iter::repeat_n(1.0, d + 1));
    BSplineCurve {
        degree: d,
        knots,
        poles,
        weights: None,
    }
}

/// The poles OpenCASCADE's loft spaces the sections by: the edge's pieces joined with only
/// the continuity their joints have (a piece's own halves meet with their tangents: the
/// pole between dropped), each edge after the first without its first pole (the corner it
/// shares).
fn spacing_poles(pieces: &[Vec<Vec<Point3>>], smooth_joints: &[Vec<bool>]) -> Vec<Point3> {
    let mut out: Vec<Point3> = Vec::new();
    for (i, edge) in pieces.iter().enumerate() {
        let mut poles: Vec<Point3> = edge[0].clone();
        for (k, piece) in edge.iter().enumerate().skip(1) {
            if smooth_joints[i][k - 1] {
                // C1: the joint pole is the middle of its neighbours.
                poles.pop();
            }
            poles.extend_from_slice(&piece[1..]);
        }
        if i == 0 {
            out.extend(poles);
        } else {
            out.extend_from_slice(&poles[1..]);
        }
    }
    out
}

/// What a face of the loft is made of before the solid is put together.
struct SideFace {
    surface: Surface,
    /// Its edges at the lower and upper section, its edges up the loft at its first and last
    /// corner, as indices into the loft's edges.
    lower: usize,
    upper: usize,
    first: usize,
    last: usize,
}

pub(crate) fn loft(
    sections: &[(Plane, Loop)],
    ruled: bool,
) -> Result<(Shape, OpHistory), NativeError> {
    if sections.len() < 2 {
        return Err(NativeError::Failed(
            "a loft goes through two sections at least".to_owned(),
        ));
    }
    if !ruled && sections.len() > MAX_SMOOTH_SECTIONS {
        return Err(NativeError::Unsupported(
            "loft smoothly through more than nine sections",
        ));
    }
    let mut first_marks: Vec<(u32, Point3)> = Vec::new();
    let mut edges_of: Vec<Vec<SectionEdge>> = Vec::with_capacity(sections.len());
    for (k, (plane, contour)) in sections.iter().enumerate() {
        let edges = section_edges(plane, contour)?;
        if edges.is_empty() {
            return Err(NativeError::Failed(
                "a loft section without a contour".to_owned(),
            ));
        }
        if k == 0 {
            first_marks = edges.iter().map(|(id, e)| (*id, e.at(0.5))).collect();
        }
        edges_of.push(edges.into_iter().map(|(_, e)| e).collect());
    }
    // Frames along the loft: the first plane's, carried from section to section.
    let along =
        perimeter_centroid(&edges_of[edges_of.len() - 1]) - perimeter_centroid(&edges_of[0]);
    let mut frames: Vec<Frame> = Vec::with_capacity(sections.len());
    for (k, (plane, _)) in sections.iter().enumerate() {
        let frame = if k == 0 {
            let f = Frame {
                x: plane.x_dir,
                y: plane.y_dir(),
                n: plane.normal,
            };
            if f.n.dot(along) < 0.0 {
                Frame {
                    x: f.x,
                    y: -f.y,
                    n: -f.n,
                }
            } else {
                f
            }
        } else {
            let prev = frames[k - 1];
            let n = if plane.normal.dot(prev.n) < 0.0 {
                -plane.normal
            } else {
                plane.normal
            };
            prev.carried_to(n)
        };
        frames.push(frame);
    }
    let counts: Vec<usize> = edges_of.iter().map(Vec::len).collect();
    let same_count = counts.iter().all(|&c| c == counts[0]);
    let starts_agree = || {
        let centres: Vec<Point3> = edges_of.iter().map(|s| perimeter_centroid(s)).collect();
        let a0 = polar(&frames[0], centres[0], edges_of[0][0].start());
        edges_of.iter().enumerate().all(|(k, s)| {
            let d = (polar(&frames[k], centres[k], s[0].start()) - a0).rem_euclid(TAU);
            d.min(TAU - d) <= 1e-9
        })
    };
    if same_count && (counts[0] > 1 || starts_agree()) {
        if counts[0] == 1 {
            // Circles: each the way round the first is.
            for k in 1..edges_of.len() {
                if winding(&frames[k], &edges_of[k]) * winding(&frames[0], &edges_of[0]) < 0.0 {
                    edges_of[k] = reversed_section(&edges_of[k]);
                }
            }
        } else {
            align(&mut edges_of);
        }
    } else {
        polar_match(&mut edges_of, &frames)?;
    }
    let m = edges_of[0].len();
    let closed_single = m == 1;
    let spans: Vec<Vec<Vec<Vec<Point3>>>> = edges_of
        .iter()
        .map(|s| s.iter().map(SectionEdge::spans).collect())
        .collect();

    let mut solid = Solid::default();
    let vertex_index = |solid: &mut Solid, p: Point3| -> u32 {
        solid.vertices.push(Vertex::new(p));
        solid.vertices.len() as u32 - 1
    };
    let n = edges_of.len();
    let smooth = !ruled && n > 2;
    // Corners: every section's for a ruled loft, the end sections' for a smooth one.
    let corner_sections: Vec<usize> = if smooth {
        vec![0, n - 1]
    } else {
        (0..n).collect()
    };
    let mut corners: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &k in &corner_sections {
        corners[k] = edges_of[k]
            .iter()
            .map(|e| vertex_index(&mut solid, e.start()))
            .collect();
    }
    let mut faces: Vec<SideFace> = Vec::new();
    // The edge of each section's edge `i` (where there is one).
    let mut section_edge: Vec<Vec<usize>> = vec![Vec::new(); n];

    if smooth {
        // Every edge to one degree, its pieces to one set.
        let degree = spans
            .iter()
            .flatten()
            .flatten()
            .map(|p| p.len() - 1)
            .max()
            .unwrap_or(1);
        let mut splits: Vec<Vec<f64>> = vec![Vec::new(); m];
        for s in &spans {
            for (i, e) in s.iter().enumerate() {
                for f in span_fractions(e.len()) {
                    if !splits[i].iter().any(|&g| (g - f).abs() <= 1e-12) {
                        splits[i].push(f);
                    }
                }
            }
        }
        for s in &mut splits {
            s.sort_by(f64::total_cmp);
        }
        let pieces: Vec<Vec<Vec<Vec<Point3>>>> = spans
            .iter()
            .map(|s| {
                s.iter()
                    .enumerate()
                    .map(|(i, e)| pieces_of(e, &splits[i], degree))
                    .collect()
            })
            .collect();
        // A joint inside an edge is smooth (the knot of multiplicity degree − 1).
        let smooth_joints: Vec<Vec<bool>> = splits.iter().map(|s| vec![true; s.len()]).collect();
        let spacing: Vec<Vec<Point3>> = pieces
            .iter()
            .map(|p| spacing_poles(p, &smooth_joints))
            .collect();
        let mut t = vec![0.0; n];
        for k in 1..n {
            let (a, b) = (&spacing[k - 1], &spacing[k]);
            let mean = a
                .iter()
                .zip(b)
                .map(|(p, q)| (*q - *p).dot(*q - *p))
                .sum::<f64>()
                / a.len().max(1) as f64;
            t[k] = t[k - 1] + mean.sqrt();
        }
        let total = t[n - 1];
        if total.is_nan() || total <= 0.0 {
            return Err(NativeError::Failed(
                "the loft's sections are all in one place".to_owned(),
            ));
        }
        for x in &mut t {
            *x /= total;
        }
        // The Bernstein system through the sections at their parameters.
        let p = n - 1;
        let matrix: Vec<Vec<f64>> = t
            .iter()
            .map(|&tk| (0..=p).map(|j| bernstein(p, j, tk)).collect())
            .collect();
        let across = |rows: &[Point3]| -> Result<Vec<Point3>, NativeError> {
            let b: Vec<Vec<f64>> = rows.iter().map(|q| vec![q.x, q.y, q.z]).collect();
            let x = solve(matrix.clone(), b).ok_or_else(|| {
                NativeError::Failed("the loft's sections cannot be passed through".to_owned())
            })?;
            Ok(x.iter().map(|r| Point3::new(r[0], r[1], r[2])).collect())
        };
        let v_knots: Vec<f64> = std::iter::repeat_n(0.0, n)
            .chain(std::iter::repeat_n(1.0, n))
            .collect();
        // Each edge's surface; its end sections' curves; the curve up at each corner.
        let mut ups: Vec<usize> = Vec::with_capacity(m);
        for i in 0..m {
            let fractions = &splits[i];
            let curves: Vec<BSplineCurve> =
                (0..n).map(|k| joined(&pieces[k][i], fractions)).collect();
            let count = curves[0].poles.len();
            let mut poles = vec![Point3::ORIGIN; count * n];
            for r in 0..count {
                let column: Vec<Point3> = curves.iter().map(|c| c.poles[r]).collect();
                for (j, q) in across(&column)?.into_iter().enumerate() {
                    poles[r * n + j] = q;
                }
            }
            let surface = BSplineSurface::new(
                [degree, p],
                [curves[0].knots.clone(), v_knots.clone()],
                [count, n],
                poles,
                None,
            )
            .map_err(NativeError::Internal)?;
            let surface = SplineSurface::new(surface, true);
            for (k, c) in [(0, &curves[0]), (n - 1, &curves[n - 1])] {
                let start = corners[k][i];
                let end = corners[k][(i + 1) % m];
                solid.edges.push(Edge::new(
                    start,
                    end,
                    Curve::Spline(SplineCurve::new(c.clone())),
                ));
                section_edge[k].push(solid.edges.len() - 1);
            }
            let up = surface.iso(1, 0.0);
            solid.edges.push(Edge::new(
                corners[0][i],
                corners[n - 1][i],
                Curve::Spline(up),
            ));
            ups.push(solid.edges.len() - 1);
            faces.push(SideFace {
                surface: Surface::Spline(surface),
                lower: section_edge[0][i],
                upper: section_edge[n - 1][i],
                first: 0,
                last: 0,
            });
        }
        for (i, face) in faces.iter_mut().enumerate() {
            face.first = ups[i];
            face.last = ups[(i + 1) % m];
        }
    } else {
        // The sections' edges, exact.
        for k in 0..n {
            for i in 0..m {
                let (curve, _) = edges_of[k][i].curve();
                solid
                    .edges
                    .push(Edge::new(corners[k][i], corners[k][(i + 1) % m], curve));
                section_edge[k].push(solid.edges.len() - 1);
            }
        }
        for k in 0..n - 1 {
            // The lines up from each corner, as B-splines of degree one.
            let ups: Vec<usize> = (0..m)
                .map(|i| {
                    let (a, b) = (edges_of[k][i].start(), edges_of[k + 1][i].start());
                    let line = BSplineCurve::bezier(vec![a, b]).map_err(NativeError::Internal)?;
                    solid.edges.push(Edge::new(
                        corners[k][i],
                        corners[k + 1][i],
                        Curve::Spline(SplineCurve::new(line)),
                    ));
                    Ok(solid.edges.len() - 1)
                })
                .collect::<Result<_, NativeError>>()?;
            for i in 0..m {
                let surface = ruled_surface(
                    &edges_of[k][i],
                    &edges_of[k + 1][i],
                    &spans[k][i],
                    &spans[k + 1][i],
                )?;
                faces.push(SideFace {
                    surface,
                    lower: section_edge[k][i],
                    upper: section_edge[k + 1][i],
                    first: ups[i],
                    last: ups[(i + 1) % m],
                });
            }
        }
    }

    // The faces: sides, then the caps.
    let mut history: Vec<Vec<FaceSource>> = Vec::new();
    for f in &faces {
        let lower = solid.edges[f.lower];
        let mut sources = Vec::new();
        if solid.faces.len() < m {
            // The first span's faces, named after a first-section edge whose middle they hold.
            let (a, b) = (solid.point(lower.start), solid.point(lower.end));
            for &(id, mid) in &first_marks {
                let off = lower.curve.distance(a, b, mid);
                let s = lower.curve.position(a, b, mid);
                let extent = lower.curve.extent(a, b, lower.is_closed());
                if off <= 1e-6 && s >= -1e-9 && s <= extent + 1e-9 {
                    sources.push(FaceSource::ProfileSegment(id));
                    break;
                }
            }
        }
        let loop_uses = if closed_single {
            vec![
                EdgeUse::forward(f.lower as u32),
                EdgeUse::forward(f.first as u32),
                EdgeUse::backward(f.upper as u32),
                EdgeUse::backward(f.first as u32),
            ]
        } else {
            vec![
                EdgeUse::forward(f.lower as u32),
                EdgeUse::forward(f.last as u32),
                EdgeUse::backward(f.upper as u32),
                EdgeUse::backward(f.first as u32),
            ]
        };
        solid.faces.push(Face {
            surface: f.surface,
            loops: vec![loop_uses],
        });
        history.push(sources);
    }
    let cap = |k: usize, outward: Vec3| -> Surface {
        let plane = &sections[k].0;
        Surface::Plane(Plane {
            origin: plane.origin,
            x_dir: plane.x_dir,
            normal: if plane.normal.dot(outward) >= 0.0 {
                plane.normal
            } else {
                -plane.normal
            },
        })
    };
    solid.faces.push(Face {
        surface: cap(0, -frames[0].n),
        loops: vec![
            section_edge[0]
                .iter()
                .rev()
                .map(|&e| EdgeUse::backward(e as u32))
                .collect(),
        ],
    });
    history.push(vec![FaceSource::StartCap]);
    solid.faces.push(Face {
        surface: cap(n - 1, frames[n - 1].n),
        loops: vec![
            section_edge[n - 1]
                .iter()
                .map(|&e| EdgeUse::forward(e as u32))
                .collect(),
        ],
    });
    history.push(vec![FaceSource::EndCap]);

    // Sections run clockwise along the loft make the solid inside out: every face turned.
    let mut shape = Shape {
        solids: vec![solid],
    };
    if winding(&frames[0], &edges_of[0]) < 0.0 {
        for face in &mut shape.solids[0].faces {
            face.surface = face.surface.reversed();
            for l in &mut face.loops {
                *l = l.iter().rev().map(|u| u.flipped()).collect();
            }
        }
    }
    for s in &mut shape.solids {
        s.settle_tolerances().map_err(NativeError::Internal)?;
    }
    Ok((shape, OpHistory { faces: history }))
}

/// The face between an edge of one section and the matching edge of the next: a cone or a
/// cylinder between arcs of circles on one axis, the same angles round it; else straight
/// across between their polynomial forms, a B-spline surface.
fn ruled_surface(
    lower: &SectionEdge,
    upper: &SectionEdge,
    lower_spans: &[Vec<Point3>],
    upper_spans: &[Vec<Point3>],
) -> Result<Surface, NativeError> {
    if let (
        SectionEdge::Arc {
            center: c1,
            normal: n1,
            radius: r1,
            a: a1,
            sweep: s1,
        },
        SectionEdge::Arc {
            center: c2,
            normal: n2,
            radius: r2,
            a: a2,
            sweep: s2,
        },
    ) = (*lower, *upper)
    {
        let up = c2 - c1;
        let h = up.length();
        let same_turn = n1.dot(n2) > 1.0 - 1e-12 && (s1 - s2).abs() <= 1e-9;
        let on_axis = h > 1e-9 && up.cross(n1).length() <= 1e-9 * h;
        let x1 = (a1 - c1) * (1.0 / r1);
        let x2 = (a2 - c2) * (1.0 / r2);
        if same_turn && on_axis && (x1 - x2).length() <= 1e-9 {
            let direction = up * (1.0 / h);
            let axis = Axis {
                origin: c1,
                direction,
            };
            return Ok(Surface::Revolution(Revolution {
                axis,
                x_dir: x1,
                meridian: Meridian::Line {
                    radius: r1,
                    slope: (r2 - r1) / h,
                },
                outward: true,
            }));
        }
    }
    let splits: Vec<f64> = {
        let mut s = span_fractions(lower_spans.len());
        for f in span_fractions(upper_spans.len()) {
            if !s.iter().any(|&g| (g - f).abs() <= 1e-12) {
                s.push(f);
            }
        }
        s.sort_by(f64::total_cmp);
        s
    };
    let degree = lower_spans
        .iter()
        .chain(upper_spans)
        .map(|p| p.len() - 1)
        .max()
        .unwrap_or(1);
    let a = joined(&pieces_of(lower_spans, &splits, degree), &splits);
    let b = joined(&pieces_of(upper_spans, &splits, degree), &splits);
    let count = a.poles.len();
    let mut poles = Vec::with_capacity(count * 2);
    for i in 0..count {
        poles.push(a.poles[i]);
        poles.push(b.poles[i]);
    }
    let surface = BSplineSurface::new(
        [degree, 1],
        [a.knots.clone(), vec![0.0, 0.0, 1.0, 1.0]],
        [count, 2],
        poles,
        None,
    )
    .map_err(NativeError::Internal)?;
    Ok(Surface::Spline(SplineSurface::new(surface, true)))
}

#[cfg(test)]
mod tests {
    use oxyde_geom::{Curve2, Point2, ProfileSegment};

    use super::*;

    fn square(side: f64) -> Loop {
        let h = side / 2.0;
        let c = [(-h, -h), (h, -h), (h, h), (-h, h)];
        Loop {
            segments: (0..4)
                .map(|i| ProfileSegment {
                    id: i as u32 + 5,
                    curve: Curve2::Line {
                        start: Point2::new(c[i].0, c[i].1),
                        end: Point2::new(c[(i + 1) % 4].0, c[(i + 1) % 4].1),
                    },
                })
                .collect(),
        }
    }

    fn circle(r: f64) -> Loop {
        Loop {
            segments: vec![ProfileSegment {
                id: 2,
                curve: Curve2::Circle {
                    center: Point2::ORIGIN,
                    radius: r,
                },
            }],
        }
    }

    fn at(z: f64) -> Plane {
        Plane {
            origin: Point3::new(0.0, 0.0, z),
            x_dir: Vec3::X,
            normal: Vec3::Z,
        }
    }

    /// `∬ f(u, v)` over the unit square, by Gauss–Legendre in 16 × 16 pieces.
    fn brute(f: &impl Fn(f64, f64) -> [f64; 4]) -> [f64; 4] {
        let rule = crate::revolution::gauss_legendre(20);
        let n = 16;
        let mut out = [0.0; 4];
        for i in 0..n {
            for j in 0..n {
                for &(x, wx) in &rule {
                    for &(y, wy) in &rule {
                        let u = (f64::from(i) + (x + 1.0) / 2.0) / f64::from(n);
                        let v = (f64::from(j) + (y + 1.0) / 2.0) / f64::from(n);
                        let w = wx * wy / (4.0 * f64::from(n * n));
                        for (o, value) in out.iter_mut().zip(f(u, v)) {
                            *o += w * value;
                        }
                    }
                }
            }
        }
        out
    }

    #[test]
    fn a_smooth_loft_s_faces_have_the_integrals_of_their_surfaces() {
        let sections: Vec<(Plane, Loop)> = [
            (0.0, 20.0),
            (10.0, 10.0),
            (20.0, 15.0),
            (30.0, 20.0),
            (40.0, 8.0),
            (50.0, 12.0),
            (60.0, 14.0),
        ]
        .iter()
        .map(|&(z, s)| (at(z), square(s)))
        .collect();
        let (shape, history) = loft(&sections, false).unwrap();
        let solid = &shape.solids[0];
        assert_eq!(solid.faces.len(), 6);
        assert_eq!(history.faces[0], vec![FaceSource::ProfileSegment(5)]);
        // Its volume as OpenCASCADE's and the interpolation's (probed, and the integral of
        // the square's area along the loft) have it.
        let volume = shape.volume().unwrap();
        assert!((volume - 13588.367344446).abs() < 1e-8 * volume, "{volume}");
        for face in &solid.faces {
            let Surface::Spline(s) = face.surface else {
                continue;
            };
            let integrals = solid.face_integrals(face, Point3::ORIGIN).unwrap();
            let sums = brute(&|u, v| {
                let d = s.derivatives(u, v, 1);
                let a = d[1][0].cross(d[0][1]).length();
                [a, d[0][0].x * a, d[0][0].y * a, d[0][0].z * a]
            });
            assert!(
                (integrals.area - sums[0]).abs() < 1e-10 * sums[0],
                "{} {}",
                integrals.area,
                sums[0]
            );
            let c = integrals.centroid(Point3::ORIGIN);
            let want = Point3::new(sums[1] / sums[0], sums[2] / sums[0], sums[3] / sums[0]);
            assert!(c.distance(want) < 1e-9, "{c:?} {want:?}");
        }
    }

    #[test]
    fn a_smooth_loft_of_circles_turns_its_radius_as_a_polynomial() {
        // Radii 10, 5, 10 at heights 0, 10, 20: the radius 10 − z + z²/20 along the loft (the
        // spacing even), so the volume is π ∫ r² dz = 2800π / 3.
        let sections: Vec<(Plane, Loop)> = [(0.0, 10.0), (10.0, 5.0), (20.0, 10.0)]
            .iter()
            .map(|&(z, r)| (at(z), circle(r)))
            .collect();
        let (shape, _) = loft(&sections, false).unwrap();
        let volume = shape.volume().unwrap();
        let want = 2800.0 * std::f64::consts::PI / 3.0;
        assert!((volume - want).abs() < 1e-8 * want, "{volume} {want}");
        let centre = shape.centre_of_mass().unwrap().unwrap();
        assert!(
            centre.distance(Point3::new(0.0, 0.0, 10.0)) < 1e-7,
            "{centre:?}"
        );
        // One face round, its seam up the loft.
        let solid = &shape.solids[0];
        assert_eq!(
            (solid.faces.len(), solid.edges.len(), solid.vertices.len()),
            (3, 3, 2)
        );
        // Its mesh lies on it within the deflection asked for.
        let mesh = crate::tessellate::mesh(&shape, 0.01).unwrap();
        let surface = solid.faces[0].surface;
        let face = &mesh.faces[0];
        assert!(!face.triangles.is_empty());
        for t in &face.triangles {
            let p = |i: u32| {
                let q = face.positions[i as usize];
                Point3::new(f64::from(q[0]), f64::from(q[1]), f64::from(q[2]))
            };
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let mid = Point3::new(
                (a.x + b.x + c.x) / 3.0,
                (a.y + b.y + c.y) / 3.0,
                (a.z + b.z + c.z) / 3.0,
            );
            assert!(surface.distance(mid) < 0.02, "{}", surface.distance(mid));
        }
    }
}
