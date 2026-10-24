//! Chamfers and fillets: edges bevelled or rounded.
//!
//! A blend is made by booleans with tools built exactly. For each edge, the tool is the
//! region its bevel or round takes off (a convex edge, cut from the solid) or fills in (a
//! concave one, fused onto it), swept along the edge:
//!
//! - a straight edge between faces that do not change along it (planes holding its
//!   direction, cylinders whose axes are parallel to it) sweeps its cross-section square to
//!   the edge along it: a prism;
//! - an arc or a circle about an axis its faces are turned about (planes square to the
//!   axis; cylinders, cones, spheres and tori on it) turns its cross-section in the
//!   meridian half-plane about the axis.
//!
//! The cross-section is bounded by the two faces' traces, from the edge to where the blend
//! leaves each face, and the bevel's line or the round's arc, tangent to both faces. A
//! bevel is a plane or a cone; a round a cylinder or a torus.
//!
//! What OpenCASCADE builds, observed as a black box, sets the rest:
//!
//! - A blend runs on along edges that continue the selected ones smoothly (the straight
//!   sides and round ends of a slot), one face per edge.
//! - Where a blend ends at a corner with one other face, it runs into that face, which it
//!   cuts in its own cross-section. Where two blended edges meet at a corner whose third
//!   edge is not blended, each runs into the face the other blends, and the two meet in a
//!   mitre. Where three meet, chamfers leave a triangle across the corner, through the
//!   three points where their edges on each face cross, and rounds between three planes a
//!   piece of the sphere they all touch.
//! - A round's torus has its axis the way the cylinder it rolls on runs when its tube's
//!   centre is outside the cylinder, the other way inside; a bevel's cone has its axis
//!   towards its apex.
//! - A selected edge whose faces meet smoothly has no corner to blend, and the blend fails
//!   saying so; one too large for its faces fails too.

// Indices into a solid's own vertices, edges and faces, and into tools built here.
#![allow(clippy::indexing_slicing)]

use std::collections::{HashMap, HashSet};
use std::f64::consts::TAU;
use std::ops::{Add, Mul, Neg, Sub};

use oxyde_geom::{
    Axis, Curve2, LINEAR_TOLERANCE, Loop, Plane, Point2, Point3, Profile, ProfileSegment, Region,
    Vec3,
};
use oxyde_kernel::{BooleanOp, ChamferSecond, FaceSource, OpHistory};

use crate::NativeError;
use crate::boolean;
use crate::brep::{Edge, EdgeUse, Face, Shape, Solid, Vertex};
use crate::build;
use crate::curved;
use crate::geometry::{Curve, Surface, perpendicular};
use crate::revolution::{Meridian, Revolution};
use crate::revolve;

/// What a blend puts along its edges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Blend {
    /// A bevel `first` mm along each edge's reference face, the other side set by `second`.
    Chamfer { first: f64, second: ChamferSecond },
    /// A round of this radius.
    Fillet { radius: f64 },
}

impl Blend {
    fn what(&self) -> &'static str {
        match self {
            Blend::Chamfer { .. } => "chamfer",
            Blend::Fillet { .. } => "fillet",
        }
    }

    /// The error for a blend larger than its faces, in OpenCASCADE's words.
    fn too_large(&self, unequal: bool) -> NativeError {
        NativeError::Failed(
            match (self, unequal) {
                (Blend::Fillet { .. }, _) => {
                    "fillet failed (radius too large for the selected edges?)"
                }
                (Blend::Chamfer { .. }, false) => {
                    "chamfer failed (distance too large for the selected edges?)"
                }
                (Blend::Chamfer { .. }, true) => {
                    "chamfer failed (sizes too large for the selected edges?)"
                }
            }
            .to_owned(),
        )
    }

    fn smooth(&self) -> NativeError {
        NativeError::Failed(format!(
            "{} failed: the selected edges are smooth (their faces meet tangentially), there \
             is no corner to round",
            self.what()
        ))
    }
}

/// Points and directions closer than this are one.
const TOL: f64 = 10.0 * LINEAR_TOLERANCE;

/// Normals closer than this (in radians) meet smoothly.
const SMOOTH: f64 = 1e-6;

/// `shape` with `edges` (each an index into its topology, with the face, also an index
/// into its topology, an unequal chamfer's first size runs along) blended. `unequal` says
/// the sizes were given apart (for the error a blend too large gives). The history names,
/// for each face of the result, the input face it is made of
/// ([`FaceSource::InputFace`]), the edge a blend face runs along
/// ([`FaceSource::InputEdge`]) or the vertex a corner covers ([`FaceSource::InputVertex`]).
pub(crate) fn blend(
    shape: &Shape,
    edges: &[(u32, Option<u32>)],
    blend: Blend,
    unequal: bool,
) -> Result<(Shape, OpHistory), NativeError> {
    match blend {
        Blend::Fillet { radius } if radius.is_nan() || radius <= LINEAR_TOLERANCE => {
            return Err(NativeError::Failed(
                "the fillet's radius must be positive".to_owned(),
            ));
        }
        Blend::Chamfer { first, second } => {
            let positive = |d: f64| d > LINEAR_TOLERANCE;
            let second_ok = match second {
                ChamferSecond::Distance(d) => positive(d),
                ChamferSecond::Angle(a) => a > 1e-9 && a < std::f64::consts::FRAC_PI_2 - 1e-9,
            };
            if !positive(first) || !second_ok {
                return Err(NativeError::Failed(
                    "the chamfer's sizes must be positive".to_owned(),
                ));
            }
        }
        Blend::Fillet { .. } => {}
    }
    if edges.is_empty() {
        return Err(NativeError::Failed(format!("no edges to {}", blend.what())));
    }
    // Each solid with its own edges; the others as they are.
    let mut per_solid: Vec<Vec<(u32, Option<u32>)>> = vec![Vec::new(); shape.solids.len()];
    let (mut e0, mut f0) = (0u32, 0u32);
    let mut starts = Vec::with_capacity(shape.solids.len());
    for solid in &shape.solids {
        starts.push((e0, f0, solid.vertices.len() as u32));
        e0 += solid.edges.len() as u32;
        f0 += solid.faces.len() as u32;
    }
    for &(edge, face) in edges {
        let Some(s) = starts
            .iter()
            .rposition(|&(first_edge, _, _)| first_edge <= edge)
        else {
            return Err(NativeError::Failed(format!("there is no edge {edge}")));
        };
        let (first_edge, first_face, _) = starts[s];
        let local = edge - first_edge;
        if local as usize >= shape.solids[s].edges.len() {
            return Err(NativeError::Failed(format!("there is no edge {edge}")));
        }
        let face = match face {
            Some(f) => {
                let lf = f
                    .checked_sub(first_face)
                    .filter(|&lf| (lf as usize) < shape.solids[s].faces.len());
                Some(lf.ok_or_else(|| {
                    NativeError::Failed(format!("face {f} is not on the edge's solid"))
                })?)
            }
            None => None,
        };
        if !per_solid[s].iter().any(|&(e, _)| e == local) {
            per_solid[s].push((local, face));
        }
    }
    let mut out = Shape::default();
    let mut history = OpHistory::default();
    let mut v_start = 0u32;
    for (s, solid) in shape.solids.iter().enumerate() {
        let (first_edge, first_face, _) = starts[s];
        let offsets = Offsets {
            face: first_face,
            edge: first_edge,
            vertex: v_start,
        };
        v_start += solid.vertices.len() as u32;
        if per_solid[s].is_empty() {
            out.solids.push(solid.clone());
            history.faces.extend((0..solid.faces.len() as u32).map(|f| {
                vec![FaceSource::InputFace {
                    input: 0,
                    face: first_face + f,
                }]
            }));
            continue;
        }
        let (solids, sources) = blend_solid(solid, &per_solid[s], blend, unequal)?;
        out.solids.extend(solids);
        history.faces.extend(
            sources
                .into_iter()
                .map(|roles| roles.into_iter().map(|r| r.source(offsets)).collect()),
        );
    }
    Ok((out, history))
}

/// Where a solid's numbering starts in its shape's.
#[derive(Debug, Clone, Copy)]
struct Offsets {
    face: u32,
    edge: u32,
    vertex: u32,
}

/// What a face of a tool, or of the result, is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Role {
    /// A face of the solid being blended.
    Input(u32),
    /// The blend face of an edge.
    Blend(u32),
    /// The face across a corner where three blends meet.
    Corner(u32),
    /// A face of a tool that lies on the solid's faces or between tools: it never makes
    /// a face of the result on its own.
    Support,
}

impl Role {
    fn source(self, at: Offsets) -> FaceSource {
        match self {
            Role::Input(face) => FaceSource::InputFace {
                input: 0,
                face: at.face + face,
            },
            Role::Blend(edge) => FaceSource::InputEdge {
                input: 0,
                edge: at.edge + edge,
            },
            Role::Corner(vertex) => FaceSource::InputVertex {
                input: 0,
                vertex: at.vertex + vertex,
            },
            // Filtered out before: a support never names a face.
            Role::Support => FaceSource::InputFace {
                input: 0,
                face: at.face,
            },
        }
    }
}

// ---------------------------------------------------------------------------------------
// Two dimensions: the cross-section.

#[derive(Debug, Clone, Copy, PartialEq)]
struct V2 {
    x: f64,
    y: f64,
}

impl V2 {
    const fn new(x: f64, y: f64) -> V2 {
        V2 { x, y }
    }

    fn dot(self, o: V2) -> f64 {
        self.x * o.x + self.y * o.y
    }

    fn cross(self, o: V2) -> f64 {
        self.x * o.y - self.y * o.x
    }

    fn length(self) -> f64 {
        self.x.hypot(self.y)
    }

    fn unit(self) -> Option<V2> {
        let l = self.length();
        (l > 1e-15).then(|| V2::new(self.x / l, self.y / l))
    }

    /// Turned a quarter turn counter-clockwise.
    fn perp(self) -> V2 {
        V2::new(-self.y, self.x)
    }

    fn rotated(self, angle: f64) -> V2 {
        let (s, c) = angle.sin_cos();
        V2::new(c * self.x - s * self.y, s * self.x + c * self.y)
    }

    fn angle(self) -> f64 {
        self.y.atan2(self.x)
    }

    fn point(self) -> Point2 {
        Point2::new(self.x, self.y)
    }
}

impl Add for V2 {
    type Output = V2;
    fn add(self, o: V2) -> V2 {
        V2::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for V2 {
    type Output = V2;
    fn sub(self, o: V2) -> V2 {
        V2::new(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f64> for V2 {
    type Output = V2;
    fn mul(self, k: f64) -> V2 {
        V2::new(self.x * k, self.y * k)
    }
}

impl Neg for V2 {
    type Output = V2;
    fn neg(self) -> V2 {
        V2::new(-self.x, -self.y)
    }
}

/// A face's trace in the cross-section, run from the edge's point `E` into the face.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Track {
    /// A straight line from `E` along `dir`.
    Line { dir: V2 },
    /// A circle about `center` through `E`, run counter-clockwise from it when `ccw`.
    Circle { center: V2, radius: f64, ccw: bool },
}

impl Track {
    fn sense(ccw: bool) -> f64 {
        if ccw { 1.0 } else { -1.0 }
    }

    /// The point `s` along the track from `e`.
    fn at(&self, e: V2, s: f64) -> V2 {
        match *self {
            Track::Line { dir } => e + dir * s,
            Track::Circle {
                center,
                radius,
                ccw,
            } => center + (e - center).rotated(Track::sense(ccw) * s / radius),
        }
    }

    /// The unit tangent at `p`, running on away from `e`.
    fn tangent(&self, p: V2) -> V2 {
        match *self {
            Track::Line { dir } => dir,
            Track::Circle { center, ccw, .. } => {
                (p - center).unit().map_or(V2::new(1.0, 0.0), V2::perp) * Track::sense(ccw)
            }
        }
    }

    /// How far along the track from `e` `p` (a point of it) is: in `[0, 2π r)` round a
    /// circle.
    fn length_to(&self, e: V2, p: V2) -> f64 {
        match *self {
            Track::Line { dir } => (p - e).dot(dir),
            Track::Circle {
                center,
                radius,
                ccw,
            } => {
                let turn = ((p - center).angle() - (e - center).angle()) * Track::sense(ccw);
                turn.rem_euclid(TAU) * radius
            }
        }
    }

    /// The point of the track nearest `x`.
    fn foot(&self, e: V2, x: V2) -> V2 {
        match *self {
            Track::Line { dir } => e + dir * (x - e).dot(dir),
            Track::Circle { center, radius, .. } => {
                center + (x - center).unit().unwrap_or(e - center) * radius
            }
        }
    }

    /// The track moved `r` towards `side` (a unit normal at `e`): the curve a round's
    /// centre runs along. `None` for a circle no smaller than `r` on its inside.
    fn offset(&self, e: V2, side: V2, r: f64) -> Option<Offset> {
        match *self {
            Track::Line { dir } => Some(Offset::Line {
                point: e + side * r,
                dir,
            }),
            Track::Circle { center, radius, .. } => {
                let inside = (center - e).dot(side) > 0.0;
                let radius = if inside { radius - r } else { radius + r };
                (radius > TOL).then_some(Offset::Circle { center, radius })
            }
        }
    }

    /// Where the ray `from + t dir` (`t > 0`) first meets the track, ahead of `e`.
    fn ray(&self, e: V2, from: V2, dir: V2) -> Option<V2> {
        let hits: Vec<V2> = match *self {
            Track::Line { dir: d } => {
                let den = dir.cross(d);
                if den.abs() < 1e-14 {
                    return None;
                }
                let t = (e - from).cross(d) / den;
                (t > TOL).then(|| from + dir * t).into_iter().collect()
            }
            Track::Circle { center, radius, .. } => {
                let w = from - center;
                let (a, b, c) = (dir.dot(dir), 2.0 * w.dot(dir), w.dot(w) - radius * radius);
                crate::revolution::quadratic_roots(a, b, c)
                    .into_iter()
                    .filter(|&t| t > TOL)
                    .map(|t| from + dir * t)
                    .collect()
            }
        };
        hits.into_iter()
            .filter(|&p| self.length_to(e, p) > TOL)
            .min_by(|p, q| p.sub(from).length().total_cmp(&q.sub(from).length()))
    }

    /// The segment of a profile from `a` to `b` along the track.
    fn segment(&self, id: u32, e: V2, a: V2, b: V2) -> ProfileSegment {
        let curve = match *self {
            Track::Line { .. } => Curve2::Line {
                start: a.point(),
                end: b.point(),
            },
            Track::Circle { .. } => {
                let (sa, sb) = (self.length_to(e, a), self.length_to(e, b));
                Curve2::Arc {
                    start: a.point(),
                    mid: self.at(e, (sa + sb) / 2.0).point(),
                    end: b.point(),
                }
            }
        };
        ProfileSegment { id, curve }
    }
}

/// A curve a round's centre runs along: a face's trace moved by the radius.
#[derive(Debug, Clone, Copy)]
enum Offset {
    Line { point: V2, dir: V2 },
    Circle { center: V2, radius: f64 },
}

/// Where two curves cross.
fn crossings(a: Offset, b: Offset) -> Vec<V2> {
    match (a, b) {
        (Offset::Line { point: p, dir: d }, Offset::Line { point: q, dir: e }) => {
            let den = d.cross(e);
            if den.abs() < 1e-14 {
                return Vec::new();
            }
            vec![p + d * ((q - p).cross(e) / den)]
        }
        (Offset::Line { point, dir }, Offset::Circle { center, radius })
        | (Offset::Circle { center, radius }, Offset::Line { point, dir }) => {
            let w = point - center;
            crate::revolution::quadratic_roots(
                dir.dot(dir),
                2.0 * w.dot(dir),
                w.dot(w) - radius * radius,
            )
            .into_iter()
            .map(|t| point + dir * t)
            .collect()
        }
        (
            Offset::Circle {
                center: c1,
                radius: r1,
            },
            Offset::Circle {
                center: c2,
                radius: r2,
            },
        ) => {
            let d = (c2 - c1).length();
            if d < 1e-14 || d > r1 + r2 + TOL || d < (r1 - r2).abs() - TOL {
                return Vec::new();
            }
            let u = (c2 - c1) * (1.0 / d);
            let along = (r1 * r1 - r2 * r2 + d * d) / (2.0 * d);
            let half = (r1 * r1 - along * along).max(0.0).sqrt();
            let base = c1 + u * along;
            vec![base + u.perp() * half, base - u.perp() * half]
        }
    }
}

/// The edge's cross-section: its point, and the two faces' traces from it.
#[derive(Debug, Clone, Copy)]
struct Section2 {
    e: V2,
    tracks: [Track; 2],
}

impl Section2 {
    /// Into each face from the edge.
    fn into(&self, i: usize) -> V2 {
        self.tracks[i].tangent(self.e)
    }

    /// The unit normal of face `i`'s trace at the edge on the side the other face is.
    fn side(&self, i: usize) -> V2 {
        let n = self.into(i).perp();
        if n.dot(self.into(1 - i)) >= 0.0 {
            n
        } else {
            -n
        }
    }
}

/// A blend's cross-section: where it leaves each face, and a round's centre.
#[derive(Debug, Clone, Copy)]
struct Cut2 {
    touch: [V2; 2],
    center: Option<V2>,
}

impl Cut2 {
    /// The cross-section's outline: along face 0's trace to where the blend leaves it, the
    /// blend's line or arc, and back along face 1's trace: segments 0, 1 and 2.
    fn profile(&self, section: &Section2) -> Profile {
        let e = section.e;
        let [t0, t1] = self.touch;
        let middle = match self.center {
            None => Curve2::Line {
                start: t0.point(),
                end: t1.point(),
            },
            Some(c) => {
                // The arc's middle faces the edge.
                let r = (t0 - c).length();
                let toward = (e - c).unit().unwrap_or((t0 + t1) * 0.5 - c);
                Curve2::Arc {
                    start: t0.point(),
                    mid: (c + toward.unit().unwrap_or(toward) * r).point(),
                    end: t1.point(),
                }
            }
        };
        Profile {
            regions: vec![Region {
                outer: Loop {
                    segments: vec![
                        section.tracks[0].segment(0, e, e, t0),
                        ProfileSegment {
                            id: 1,
                            curve: middle,
                        },
                        section.tracks[1].segment(2, e, t1, e),
                    ],
                },
                holes: Vec::new(),
            }],
        }
    }
}

/// A bevel `first` along face `side`'s trace, the other side `second`.
fn bevel(section: &Section2, side: usize, first: f64, second: ChamferSecond) -> Option<Cut2> {
    let (a, b) = (side, 1 - side);
    let e = section.e;
    let ta = section.tracks[a].at(e, first);
    let tb = match second {
        ChamferSecond::Distance(d) => section.tracks[b].at(e, d),
        ChamferSecond::Angle(angle) => {
            // From `ta`, back towards the edge at `angle` to face a, on the other face's side.
            let tangent = section.tracks[a].tangent(ta);
            // The other face's side, which keeps its place against the trace's direction.
            let sign = if section.into(a).perp().dot(section.side(a)) >= 0.0 {
                1.0
            } else {
                -1.0
            };
            let normal = tangent.perp() * sign;
            let dir = tangent * (-angle.cos()) + normal * angle.sin();
            section.tracks[b].ray(e, ta, dir)?
        }
    };
    let mut touch = [ta, tb];
    if side == 1 {
        touch.swap(0, 1);
    }
    Some(Cut2 {
        touch,
        center: None,
    })
}

/// A round of radius `r` tangent to both faces' traces, between them.
fn round(section: &Section2, r: f64) -> Option<Cut2> {
    let e = section.e;
    let o0 = section.tracks[0].offset(e, section.side(0), r)?;
    let o1 = section.tracks[1].offset(e, section.side(1), r)?;
    let mut best: Option<(f64, Cut2)> = None;
    for c in crossings(o0, o1) {
        let t0 = section.tracks[0].foot(e, c);
        let t1 = section.tracks[1].foot(e, c);
        let (s0, s1) = (
            section.tracks[0].length_to(e, t0),
            section.tracks[1].length_to(e, t1),
        );
        // Ahead of the edge on both faces, not round the far side of a circle.
        let ahead = |s: f64, track: &Track| {
            s > TOL
                && match track {
                    Track::Line { .. } => true,
                    Track::Circle { radius, .. } => s < std::f64::consts::PI * radius,
                }
        };
        if !ahead(s0, &section.tracks[0]) || !ahead(s1, &section.tracks[1]) {
            continue;
        }
        let reach = s0.max(s1);
        if best.as_ref().is_none_or(|(b, _)| reach < *b) {
            best = Some((
                reach,
                Cut2 {
                    touch: [t0, t1],
                    center: Some(c),
                },
            ));
        }
    }
    best.map(|(_, cut)| cut)
}

// ---------------------------------------------------------------------------------------
// The edges of a solid.

/// How an edge's cross-section lies in space.
#[derive(Debug, Clone, Copy)]
enum Along {
    /// A straight edge from `origin`, `length` long along `dir`: the section square to it,
    /// `x` and `y` across it (`x × y = dir`).
    Straight {
        origin: Point3,
        dir: Vec3,
        x: Vec3,
        y: Vec3,
        length: f64,
    },
    /// An arc or a circle about the axis through `center` along `normal`, from its start
    /// (at angle zero, along `x`) `sweep` counter-clockwise: the section is the meridian
    /// half-plane, `ρ` along `x` and `z` along `normal`.
    Round {
        center: Point3,
        normal: Vec3,
        x: Vec3,
        sweep: f64,
        closed: bool,
    },
}

impl Along {
    /// A point of the cross-section, the section `s` along the edge (a length, or an angle
    /// from its start), in space.
    fn world(&self, p: V2, s: f64) -> Point3 {
        match *self {
            Along::Straight {
                origin, dir, x, y, ..
            } => origin + x * p.x + y * p.y + dir * s,
            Along::Round {
                center, normal, x, ..
            } => {
                let e = x * s.cos() + normal.cross(x) * s.sin();
                center + e * p.x + normal * p.y
            }
        }
    }

    /// How far the edge runs: its length, or its sweep.
    fn extent(&self) -> f64 {
        match *self {
            Along::Straight { length, .. } => length,
            Along::Round { sweep, .. } => sweep,
        }
    }
}

/// A blended edge: its faces (the one running along it its own way, then the other), how
/// its cross-section lies, the cross-section and the blend's.
#[derive(Debug, Clone, Copy)]
struct Run {
    edge: u32,
    faces: [u32; 2],
    along: Along,
    convex: bool,
    section: Section2,
    cut: Cut2,
}

/// The faces using each edge: the face and whether it runs along it the other way.
fn edge_uses(solid: &Solid) -> Vec<Vec<(u32, bool)>> {
    let mut uses = vec![Vec::new(); solid.edges.len()];
    for (f, face) in solid.faces.iter().enumerate() {
        for u in face.loops.iter().flatten() {
            uses[u.edge as usize].push((f as u32, u.reversed));
        }
    }
    uses
}

fn normal_at(surface: &Surface, p: Point3) -> Vec3 {
    match surface {
        Surface::Plane(plane) => plane.normal,
        Surface::Revolution(r) => r.normal_at(p),
        Surface::Spline(s) => s.normal_at(p),
    }
}

/// The unit tangent of an edge at a point of it, the way the edge runs.
fn tangent_at(solid: &Solid, edge: &Edge, p: Point3) -> Option<Vec3> {
    match edge.curve {
        Curve::Line => (solid.point(edge.end) - solid.point(edge.start)).normalized(),
        Curve::Circle(c) => c.normal.cross(p - c.center).normalized(),
        Curve::Section(_) | Curve::Traced(_) | Curve::Spline(_) => {
            let (a, b) = (solid.point(edge.start), solid.point(edge.end));
            let s = edge.curve.position(a, b, p);
            let h = 1e-6;
            let p1 = edge.curve.at(a, b, s + h)?;
            let p0 = edge.curve.at(a, b, s - h)?;
            (p1 - p0).normalized()
        }
        Curve::Degenerate { .. } => None,
    }
}

/// Into face `face` from edge `edge` at `p`, square to the edge, along the face.
fn into_face(solid: &Solid, face: u32, reversed: bool, edge: &Edge, p: Point3) -> Option<Vec3> {
    let t = tangent_at(solid, edge, p)?;
    let t = if reversed { -t } else { t };
    normal_at(&solid.faces[face as usize].surface, p)
        .cross(t)
        .normalized()
}

/// How the faces of `edge` meet: `Some(true)` at a ridge, `Some(false)` in a valley, `None`
/// smoothly.
fn convexity(solid: &Solid, uses: &[(u32, bool)], edge: &Edge, p: Point3) -> Option<bool> {
    let [(f0, r0), (f1, r1)] = [uses[0], uses[1]];
    let n0 = normal_at(&solid.faces[f0 as usize].surface, p);
    let n1 = normal_at(&solid.faces[f1 as usize].surface, p);
    if (n0 - n1).length() < SMOOTH {
        return None;
    }
    let w1 = into_face(solid, f1, r1, edge, p)?;
    let _ = r0;
    Some(w1.dot(n0) < 0.0)
}

/// Whether the two directions are one, within `SMOOTH`.
fn same_dir(a: Vec3, b: Vec3) -> bool {
    (a - b).length() < SMOOTH * 10.0
}

/// The direction an edge leaves vertex `v` in.
fn leaving(solid: &Solid, e: u32, v: u32) -> Option<Vec3> {
    let edge = &solid.edges[e as usize];
    let p = solid.point(v);
    let t = tangent_at(solid, edge, p)?;
    Some(if edge.start == v { t } else { -t })
}

/// The edges meeting at each vertex.
fn vertex_edges(solid: &Solid) -> Vec<Vec<u32>> {
    let mut out = vec![Vec::new(); solid.vertices.len()];
    for (e, edge) in solid.edges.iter().enumerate() {
        out[edge.start as usize].push(e as u32);
        if edge.end != edge.start {
            out[edge.end as usize].push(e as u32);
        }
    }
    out
}

/// The faces meeting at each vertex.
fn vertex_faces(solid: &Solid, uses: &[Vec<(u32, bool)>]) -> Vec<Vec<u32>> {
    let mut out: Vec<Vec<u32>> = vec![Vec::new(); solid.vertices.len()];
    for (e, edge) in solid.edges.iter().enumerate() {
        for v in [edge.start, edge.end] {
            for &(f, _) in &uses[e] {
                if !out[v as usize].contains(&f) {
                    out[v as usize].push(f);
                }
            }
        }
    }
    out
}

/// The edge that carries `e` on smoothly past vertex `v`: the other way along one line or
/// circle, its faces meeting `e`'s faces' surfaces smoothly there.
fn continuation(
    solid: &Solid,
    uses: &[Vec<(u32, bool)>],
    at_vertex: &[Vec<u32>],
    e: u32,
    v: u32,
) -> Option<u32> {
    let p = solid.point(v);
    let out = leaving(solid, e, v)?;
    let normals = |edge: u32| -> Option<Vec<Vec3>> {
        let list = &uses[edge as usize];
        if list.len() != 2 {
            return None;
        }
        Some(
            list.iter()
                .map(|&(f, _)| normal_at(&solid.faces[f as usize].surface, p))
                .collect(),
        )
    };
    let mine = normals(e)?;
    at_vertex[v as usize].iter().copied().find(|&other| {
        if other == e || solid.edges[other as usize].curve.is_degenerate() {
            return false;
        }
        let Some(dir) = leaving(solid, other, v) else {
            return false;
        };
        if !same_dir(dir, -out) {
            return false;
        }
        let Some(theirs) = normals(other) else {
            return false;
        };
        let matched = |a: &Vec3| theirs.iter().any(|b| same_dir(*a, *b));
        mine.iter().all(matched)
    })
}

/// How a blended edge's tool ends at one of its vertices.
#[derive(Debug, Clone, Copy, PartialEq)]
enum End {
    /// The blend runs on along the next edge: the tool ends square to the edge.
    Joint,
    /// The blend runs into this face, which cuts it.
    Into(u32),
    /// Three blends meet at the vertex, whose third face (the one the edge is not on) is
    /// `face`.
    Corner { vertex: u32, face: u32 },
}

// ---------------------------------------------------------------------------------------
// Blending one solid.

fn blend_solid(
    solid: &Solid,
    selected: &[(u32, Option<u32>)],
    blend: Blend,
    unequal: bool,
) -> Result<(Vec<Solid>, Vec<Vec<Role>>), NativeError> {
    let uses = edge_uses(solid);
    let at_vertex = vertex_edges(solid);
    let faces_at = vertex_faces(solid, &uses);

    // The selected edges and the ones that carry them on smoothly, each with the side its
    // first size runs along.
    let mut side_of: HashMap<u32, usize> = HashMap::new();
    let mut queue: Vec<u32> = Vec::new();
    for &(e, face) in selected {
        let list = &uses[e as usize];
        let edge = &solid.edges[e as usize];
        if edge.curve.is_degenerate() || list.len() != 2 || list[0].0 == list[1].0 {
            return Err(NativeError::Unsupported(
                "blend a seam or an edge of no length",
            ));
        }
        // The forward user first.
        let side = match face {
            Some(f) => list.iter().position(|&(g, _)| g == f).ok_or_else(|| {
                NativeError::Failed("the face is not one of the edge's".to_owned())
            })?,
            None => 0,
        };
        let side = if list[0].1 { 1 - side } else { side };
        if side_of.insert(e, side).is_none() {
            queue.push(e);
        }
    }
    while let Some(e) = queue.pop() {
        let edge = solid.edges[e as usize];
        for v in [edge.start, edge.end] {
            let Some(next) = continuation(solid, &uses, &at_vertex, e, v) else {
                continue;
            };
            if side_of.contains_key(&next) {
                continue;
            }
            // The same face's side: the face whose normal there is the one of `e`'s side.
            let p = solid.point(v);
            let mine = ordered(&uses[e as usize]);
            let theirs = ordered(&uses[next as usize]);
            let n_side = normal_at(&solid.faces[mine[side_of[&e]] as usize].surface, p);
            let side = (0..2)
                .find(|&k| {
                    same_dir(
                        normal_at(&solid.faces[theirs[k] as usize].surface, p),
                        n_side,
                    )
                })
                .unwrap_or(0);
            side_of.insert(next, side);
            queue.push(next);
        }
    }
    let mut blended: Vec<u32> = side_of.keys().copied().collect();
    blended.sort_unstable();
    let blended_set: HashSet<u32> = blended.iter().copied().collect();

    // Each edge's cross-section and blend.
    let mut runs: Vec<Run> = Vec::new();
    for &e in &blended {
        runs.push(run_of(solid, &uses, e, side_of[&e], blend, unequal)?);
    }
    let run_of_edge: HashMap<u32, usize> =
        runs.iter().enumerate().map(|(i, r)| (r.edge, i)).collect();

    // How each run ends at its vertices.
    let mut corners: Vec<u32> = Vec::new();
    let mut ends: Vec<[End; 2]> = Vec::new();
    for run in &runs {
        let edge = solid.edges[run.edge as usize];
        let mut pair = [End::Joint, End::Joint];
        if !edge.is_closed() {
            for (k, v) in [edge.start, edge.end].into_iter().enumerate() {
                if continuation(solid, &uses, &at_vertex, run.edge, v)
                    .is_some_and(|n| blended_set.contains(&n))
                {
                    continue;
                }
                let others: Vec<u32> = faces_at[v as usize]
                    .iter()
                    .copied()
                    .filter(|f| !run.faces.contains(f))
                    .collect();
                let [third] = others[..] else {
                    return Err(NativeError::Unsupported(
                        "blend an edge that ends where more than three faces meet",
                    ));
                };
                let meeting: Vec<u32> = at_vertex[v as usize]
                    .iter()
                    .copied()
                    .filter(|e| blended_set.contains(e))
                    .collect();
                let convexities: HashSet<bool> = meeting
                    .iter()
                    .map(|e| runs[run_of_edge[e]].convex)
                    .collect();
                if convexities.len() > 1 {
                    return Err(NativeError::Unsupported(
                        "blend a ridge and a valley that meet at a corner",
                    ));
                }
                pair[k] = match meeting.len() {
                    1 | 2 => End::Into(third),
                    3 if at_vertex[v as usize].len() == 3 => {
                        if !corners.contains(&v) {
                            corners.push(v);
                        }
                        End::Corner {
                            vertex: v,
                            face: third,
                        }
                    }
                    _ => {
                        return Err(NativeError::Unsupported(
                            "blend edges that meet where more than three edges do",
                        ));
                    }
                };
            }
        }
        ends.push(pair);
    }

    // Where each tool stops, and the blend must lie on its faces up to there.
    let mut spans = Vec::with_capacity(runs.len());
    for (run, pair) in runs.iter().zip(&ends) {
        let span = span_of(solid, run, pair, blend, &runs, &run_of_edge, &at_vertex)?;
        check_fits(solid, run, &span, blend, unequal)?;
        spans.push(span);
    }

    // The tools: one per edge, one per corner.
    let mut convex_tools: Vec<Tool> = Vec::new();
    let mut concave_tools: Vec<Tool> = Vec::new();
    for (run, span) in runs.iter().zip(&spans) {
        let tool = edge_tool(solid, run, span)?;
        if run.convex {
            convex_tools.push(tool);
        } else {
            concave_tools.push(tool);
        }
    }
    for &v in &corners {
        let meeting: Vec<&Run> = at_vertex[v as usize]
            .iter()
            .map(|e| &runs[run_of_edge[e]])
            .collect();
        let convex = meeting[0].convex;
        let tool = corner_tool(solid, v, &meeting, blend, &faces_at[v as usize])?;
        if convex {
            convex_tools.push(tool);
        } else {
            concave_tools.push(tool);
        }
    }

    // Cut what the ridges take off, fuse what the valleys fill in.
    let mut current = Shape {
        solids: vec![solid.clone()],
    };
    let mut roles: Vec<Vec<Role>> = (0..solid.faces.len() as u32)
        .map(|f| vec![Role::Input(f)])
        .collect();
    for (op, tools) in [
        (BooleanOp::Cut, &convex_tools),
        (BooleanOp::Fuse, &concave_tools),
    ] {
        if tools.is_empty() {
            continue;
        }
        let shapes: Vec<&Shape> = tools.iter().map(|t| &t.shape).collect();
        let (shape, history) = boolean::boolean(op, &current, &shapes)?;
        roles = history
            .faces
            .iter()
            .map(|sources| {
                let mut out: Vec<Role> = Vec::new();
                for source in sources {
                    let FaceSource::InputFace { input, face } = *source else {
                        continue;
                    };
                    let from: Vec<Role> = if input == 0 {
                        roles.get(face as usize).cloned().unwrap_or_default()
                    } else {
                        tools
                            .get(input as usize - 1)
                            .and_then(|t| t.roles.get(face as usize))
                            .map(|&r| vec![r])
                            .unwrap_or_default()
                    };
                    out.extend(from);
                }
                out
            })
            .collect();
        current = shape;
    }
    // A face made of the solid's faces and supports is the solid's; supports name nothing.
    let roles: Vec<Vec<Role>> = roles
        .into_iter()
        .map(|mut list| {
            list.retain(|r| *r != Role::Support);
            list.sort();
            list.dedup();
            list
        })
        .collect();
    // Every blend made its face.
    for run in &runs {
        if !roles.iter().flatten().any(|r| *r == Role::Blend(run.edge)) {
            return Err(blend.too_large(unequal));
        }
    }
    Ok((current.solids, roles))
}

/// An edge's two faces, the forward user first.
fn ordered(list: &[(u32, bool)]) -> [u32; 2] {
    if list[0].1 {
        [list[1].0, list[0].0]
    } else {
        [list[0].0, list[1].0]
    }
}

/// An edge's cross-section and blend.
fn run_of(
    solid: &Solid,
    uses: &[Vec<(u32, bool)>],
    e: u32,
    side: usize,
    blend: Blend,
    unequal: bool,
) -> Result<Run, NativeError> {
    let edge = solid.edges[e as usize];
    let faces = ordered(&uses[e as usize]);
    let surfaces = faces.map(|f| solid.faces[f as usize].surface);
    let start = solid.point(edge.start);
    let probe = solid.midpoint(&edge);
    let convex = convexity(solid, &uses[e as usize], &edge, probe).ok_or_else(|| blend.smooth())?;
    let into = |k: usize, p: Point3| -> Result<Vec3, NativeError> {
        into_face(solid, faces[k], k == 1, &edge, p)
            .ok_or_else(|| NativeError::Internal("an edge with no direction".to_owned()))
    };
    let unsupported = NativeError::Unsupported("blend an edge between these faces");
    let (along, section) = match edge.curve {
        Curve::Line => {
            let end = solid.point(edge.end);
            let length = start.distance(end);
            let dir = (end - start)
                .normalized()
                .ok_or_else(|| NativeError::Internal("an edge of no length".to_owned()))?;
            let x = into(0, start)?;
            let y = dir.cross(x);
            let mut tracks = [Track::Line {
                dir: V2::new(1.0, 0.0),
            }; 2];
            for k in 0..2 {
                let w = into(k, start)?;
                let w2 = V2::new(w.dot(x), w.dot(y));
                tracks[k] = match surfaces[k] {
                    Surface::Plane(p) if p.normal.dot(dir).abs() < 1e-9 => Track::Line { dir: w2 },
                    Surface::Revolution(r)
                        if r.cylinder_radius().is_some()
                            && r.axis.direction.cross(dir).length() < 1e-9 =>
                    {
                        let o = r.axis.origin - start;
                        let center = V2::new(o.dot(x), o.dot(y));
                        let radius = r.cylinder_radius().unwrap_or(0.0);
                        Track::Circle {
                            center,
                            radius,
                            ccw: center.neg().cross(w2) > 0.0,
                        }
                    }
                    _ => return Err(unsupported),
                };
            }
            (
                Along::Straight {
                    origin: start,
                    dir,
                    x,
                    y,
                    length,
                },
                Section2 {
                    e: V2::new(0.0, 0.0),
                    tracks,
                },
            )
        }
        Curve::Circle(circle) => {
            let (c, n, radius) = (circle.center, circle.normal, circle.radius);
            let x = (start - c)
                .normalized()
                .ok_or_else(|| NativeError::Internal("a circle of no size".to_owned()))?;
            let e = V2::new(radius, 0.0);
            let mut tracks = [Track::Line {
                dir: V2::new(1.0, 0.0),
            }; 2];
            for k in 0..2 {
                let w = into(k, start)?;
                let w2 = V2::new(w.dot(x), w.dot(n));
                tracks[k] = match surfaces[k] {
                    Surface::Plane(p) if p.normal.cross(n).length() < 1e-9 => {
                        Track::Line { dir: w2 }
                    }
                    Surface::Revolution(r) => {
                        let a = r.axis.direction;
                        let off = c - r.axis.origin;
                        if a.cross(n).length() > 1e-9 || (off - a * off.dot(a)).length() > TOL {
                            return Err(unsupported);
                        }
                        match r.meridian {
                            Meridian::Line { .. } => Track::Line { dir: w2 },
                            Meridian::Circle { center, radius: rm } => {
                                let mc = V2::new(center, (r.axis.origin - c).dot(n));
                                Track::Circle {
                                    center: mc,
                                    radius: rm,
                                    ccw: (e - mc).cross(w2) > 0.0,
                                }
                            }
                        }
                    }
                    _ => return Err(unsupported),
                };
            }
            (
                Along::Round {
                    center: c,
                    normal: n,
                    x,
                    sweep: solid.sweep(&edge),
                    closed: edge.is_closed(),
                },
                Section2 { e, tracks },
            )
        }
        _ => return Err(unsupported),
    };
    let cut = match blend {
        Blend::Chamfer { first, second } => bevel(&section, side, first, second),
        Blend::Fillet { radius } => round(&section, radius),
    }
    .ok_or_else(|| blend.too_large(unequal))?;
    Ok(Run {
        edge: e,
        faces,
        along,
        convex,
        section,
        cut,
    })
}

/// Where an edge's tool starts and stops along it (a length, or an angle from its start),
/// and the planes of the faces that cut it there: the solid's side of each is kept.
#[derive(Debug, Clone)]
struct Span {
    stops: [f64; 2],
    clips: Vec<Plane>,
}

impl Span {
    /// Whether `p` is on the solid's side of every face that cuts the tool.
    fn keeps(&self, p: Point3) -> bool {
        self.clips
            .iter()
            .all(|plane| (p - plane.origin).dot(plane.normal) < -TOL)
    }
}

/// Where a run's tool stops at each end: square to the edge at a joint, a face it runs
/// into square to it or holding a round edge's axis; past a slanted face, which then cuts
/// it; at a round's corner, square to the edge through the corner's sphere's centre.
fn span_of(
    solid: &Solid,
    run: &Run,
    ends: &[End; 2],
    blend: Blend,
    runs: &[Run],
    run_of_edge: &HashMap<u32, usize>,
    at_vertex: &[Vec<u32>],
) -> Result<Span, NativeError> {
    // How far the cross-section reaches from the edge.
    let reach = run
        .cut
        .touch
        .iter()
        .map(|t| (*t - run.section.e).length())
        .fold(0.0, f64::max)
        + run.cut.center.map_or(0.0, |c| (c - run.section.e).length());
    let mut span = Span {
        stops: [0.0, run.along.extent()],
        clips: Vec::new(),
    };
    for (k, end) in ends.iter().enumerate() {
        let outward = if k == 0 { -1.0 } else { 1.0 };
        let face = match *end {
            End::Joint => continue,
            End::Corner { vertex, .. } if matches!(blend, Blend::Fillet { .. }) => {
                let meeting: Vec<&Run> = at_vertex[vertex as usize]
                    .iter()
                    .map(|e| &runs[run_of_edge[e]])
                    .collect();
                let center = corner_center(solid, vertex, &meeting, blend)?;
                let Along::Straight { origin, dir, .. } = run.along else {
                    return Err(NativeError::Unsupported(
                        "round a corner where curved edges meet",
                    ));
                };
                span.stops[k] = (center - origin).dot(dir);
                continue;
            }
            End::Into(face) | End::Corner { face, .. } => face,
        };
        let Surface::Plane(plane) = solid.faces[face as usize].surface else {
            return Err(NativeError::Unsupported("end a blend on a curved face"));
        };
        match run.along {
            Along::Straight { dir, .. } => {
                let along = plane.normal.dot(dir);
                if along.abs() > 1.0 - 1e-9 {
                    continue;
                }
                if along.abs() < 0.05 {
                    return Err(NativeError::Unsupported(
                        "end a blend on a face that runs nearly along its edge",
                    ));
                }
                span.stops[k] += outward * (2.0 * reach / along.abs() + 1.0);
            }
            Along::Round { center, normal, .. } => {
                let holds_axis = plane.normal.dot(normal).abs() < 1e-9
                    && (center - plane.origin).dot(plane.normal).abs() < TOL;
                if holds_axis {
                    continue;
                }
                span.stops[k] += outward * 0.5;
            }
        }
        span.clips.push(plane);
    }
    Ok(span)
}

/// Fails when the blend runs off the faces it should lie on: across where it leaves each
/// face, along the edge as far as its tool goes.
fn check_fits(
    solid: &Solid,
    run: &Run,
    span: &Span,
    blend: Blend,
    unequal: bool,
) -> Result<(), NativeError> {
    let extent = run.along.extent();
    // A little in from the ends, which may lie on the faces' edges.
    let (lo, hi) = (span.stops[0].max(0.0), span.stops[1].min(extent));
    let (lo, hi) = (lo + 0.01 * (hi - lo), hi - 0.01 * (hi - lo));
    let n = 8;
    for k in 0..2 {
        let face = &solid.faces[run.faces[k] as usize];
        let test = curved::FaceTest::new(solid, face)?;
        for i in 0..=n {
            let s = lo + (hi - lo) * f64::from(i) / f64::from(n);
            for w in [0.5, 1.0] {
                let p2 = run.section.e + (run.cut.touch[k] - run.section.e) * w;
                let p2 = run.section.tracks[k].foot(run.section.e, p2);
                let p = run.along.world(p2, s);
                if !span.keeps(p) {
                    continue;
                }
                if !test.contains(p) {
                    return Err(blend.too_large(unequal));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Tools.

/// A solid a blend cuts away or adds, and what each of its faces is.
#[derive(Debug, Clone)]
struct Tool {
    shape: Shape,
    roles: Vec<Role>,
}

impl Tool {
    /// The tool cut by `plane`, keeping the side its normal points away from.
    fn clip(self, plane: &Plane) -> Result<Tool, NativeError> {
        let (lo, hi) = self
            .shape
            .bounds()
            .ok_or_else(|| NativeError::Internal("an empty tool".to_owned()))?;
        let size = 4.0 * lo.distance(hi) + 10.0;
        let middle = Point3::new(
            (lo.x + hi.x) / 2.0,
            (lo.y + hi.y) / 2.0,
            (lo.z + hi.z) / 2.0,
        );
        let n = plane.normal;
        let origin = middle - n * (middle - plane.origin).dot(n);
        let base = Plane {
            origin,
            x_dir: perpendicular(n),
            normal: n,
        };
        let square = square_profile(size);
        let (half, _) = build::extrude(&base, &square, n * size)?;
        let (shape, history) = boolean::boolean(BooleanOp::Cut, &self.shape, &[&half])?;
        let roles = history
            .faces
            .iter()
            .map(|sources| {
                sources
                    .iter()
                    .find_map(|s| match *s {
                        FaceSource::InputFace { input: 0, face } => {
                            self.roles.get(face as usize).copied()
                        }
                        _ => None,
                    })
                    .unwrap_or(Role::Support)
            })
            .collect();
        Ok(Tool { shape, roles })
    }
}

/// A square of side `2 size` about the origin.
fn square_profile(size: f64) -> Profile {
    let corners = [(-size, -size), (size, -size), (size, size), (-size, size)];
    let segments = (0..4)
        .map(|i| {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            ProfileSegment {
                id: i as u32,
                curve: Curve2::Line {
                    start: Point2::new(a.0, a.1),
                    end: Point2::new(b.0, b.1),
                },
            }
        })
        .collect();
    Profile {
        regions: vec![Region {
            outer: Loop { segments },
            holes: Vec::new(),
        }],
    }
}

/// The roles of a swept tool's faces: segment 1 of its cross-section is the blend.
fn swept_roles(history: &OpHistory, edge: u32) -> Vec<Role> {
    history
        .faces
        .iter()
        .map(|sources| {
            if sources.contains(&FaceSource::ProfileSegment(1)) {
                Role::Blend(edge)
            } else {
                Role::Support
            }
        })
        .collect()
}

/// The tool of one edge: its cross-section swept along it over its span, cut by the faces
/// it runs into.
fn edge_tool(solid: &Solid, run: &Run, span: &Span) -> Result<Tool, NativeError> {
    let profile = run.cut.profile(&run.section);
    let [s0, s1] = span.stops;
    let (shape, history) = match run.along {
        Along::Straight { origin, dir, x, .. } => {
            let plane = Plane {
                origin: origin + dir * s0,
                x_dir: x,
                normal: dir,
            };
            build::extrude(&plane, &profile, dir * (s1 - s0))?
        }
        Along::Round {
            center,
            normal,
            x,
            closed,
            ..
        } => {
            let turn = if closed { TAU } else { s1 - s0 };
            if turn > TAU - 1e-6 && !closed {
                return Err(NativeError::Unsupported(
                    "blend an arc that goes nearly all the way round",
                ));
            }
            let e = x * s0.cos() + normal.cross(x) * s0.sin();
            let plane = Plane {
                origin: center,
                x_dir: e,
                normal: e.cross(normal),
            };
            revolve::revolve(
                &plane,
                &profile,
                &Axis {
                    origin: center,
                    direction: normal,
                },
                turn,
            )?
        }
    };
    let roles = swept_roles(&history, run.edge);
    let mut tool = Tool { shape, roles };
    // The blend face's surface in OpenCASCADE's frame.
    if let Some(surface) = blend_surface(solid, run) {
        for solid in &mut tool.shape.solids {
            for (face, role) in solid.faces.iter_mut().zip(&tool.roles) {
                if *role == Role::Blend(run.edge)
                    && let Surface::Revolution(r) = &mut face.surface
                {
                    *r = r.reframed(&surface);
                }
            }
        }
    }
    for plane in &span.clips {
        tool = tool.clip(plane)?;
    }
    Ok(tool)
}

/// The surface a blend face should have, in OpenCASCADE's frame: a round's cylinder along
/// its edge from its start, a torus with its axis towards the plane it rolls on, a bevel's
/// cone with its axis against the cylinder it leaves, angle zero at the edge's start.
fn blend_surface(solid: &Solid, run: &Run) -> Option<Revolution> {
    let surfaces = run.faces.map(|f| solid.faces[f as usize].surface);
    match (run.along, run.cut.center) {
        (
            Along::Straight {
                origin, dir, x, y, ..
            },
            Some(c),
        ) => {
            let center = origin + x * c.x + y * c.y;
            let r = (run.cut.touch[0] - c).length();
            // Angle zero away from the round, so that its face never crosses it.
            let middle = (run.section.e - c).unit()?;
            let away = -(x * middle.x + y * middle.y);
            Some(Revolution::cylinder(
                Axis {
                    origin: center,
                    direction: dir,
                },
                away,
                r,
                false,
            ))
        }
        (
            Along::Round {
                center, normal, x, ..
            },
            Some(c),
        ) => {
            let origin = center + normal * c.y;
            // The torus's axis the way the cylinder's it rolls on runs when its tube's centre
            // is outside the cylinder, the other way inside; else towards the plane.
            let cylinder = surfaces.iter().find_map(|s| match s {
                Surface::Revolution(r) => {
                    r.cylinder_radius().map(|radius| (r.axis.direction, radius))
                }
                _ => None,
            });
            let direction = match cylinder {
                Some((d, radius)) => {
                    let d = if d.dot(normal) >= 0.0 {
                        normal
                    } else {
                        -normal
                    };
                    if c.x > radius { d } else { -d }
                }
                None => {
                    let toward = surfaces.iter().find_map(|s| match s {
                        Surface::Plane(p) => Some((p.origin - origin).dot(normal)),
                        _ => None,
                    });
                    if toward.unwrap_or(1.0) < 0.0 {
                        -normal
                    } else {
                        normal
                    }
                }
            };
            Some(Revolution {
                axis: Axis { origin, direction },
                x_dir: x,
                meridian: Meridian::Circle {
                    center: c.x,
                    radius: (run.cut.touch[0] - c).length(),
                },
                outward: false,
            })
        }
        (
            Along::Round {
                center, normal, x, ..
            },
            None,
        ) => {
            // The cone's axis from where it leaves a cylinder (else its first face), towards
            // its apex.
            let k = (0..2)
                .find(|&k| {
                    matches!(surfaces[k], Surface::Revolution(r) if r.cylinder_radius().is_some())
                })
                .unwrap_or(0);
            let t = run.cut.touch[k];
            let d = run.cut.touch[1 - k] - t;
            let direction = if d.x * d.y < 0.0 { normal } else { -normal };
            Some(Revolution {
                axis: Axis {
                    origin: center + normal * t.y,
                    direction,
                },
                x_dir: x,
                meridian: Meridian::Line {
                    radius: t.x,
                    slope: 0.0,
                },
                outward: false,
            })
        }
        (Along::Straight { .. }, None) => None,
    }
}

// ---------------------------------------------------------------------------------------
// Corners where three blends meet.

/// The centre of the sphere three rounds at vertex `v` all touch: as far from each of the
/// three planes there as the radius, on the side the rounds are.
fn corner_center(
    solid: &Solid,
    v: u32,
    meeting: &[&Run],
    blend: Blend,
) -> Result<Point3, NativeError> {
    let Blend::Fillet { radius } = blend else {
        return Err(NativeError::Internal(
            "a chamfer's corner has no centre".to_owned(),
        ));
    };
    let planes = corner_planes(solid, meeting)?;
    let convex = meeting[0].convex;
    let p = solid.point(v);
    // n_i · (x − p) = ∓r for each plane: the solid's side for ridges, the other for valleys.
    let depth = if convex { -radius } else { radius };
    let rows: Vec<(Vec3, f64)> = planes.iter().map(|pl| (pl.normal, depth)).collect();
    solve3(&rows).map(|d| p + d).ok_or(NativeError::Unsupported(
        "round a corner whose faces do not meet in a point",
    ))
}

/// The three planes at a corner where three blended edges meet.
fn corner_planes(solid: &Solid, meeting: &[&Run]) -> Result<Vec<Plane>, NativeError> {
    let mut faces: Vec<u32> = meeting.iter().flat_map(|r| r.faces).collect();
    faces.sort_unstable();
    faces.dedup();
    if faces.len() != 3 {
        return Err(NativeError::Unsupported(
            "blend a corner that is not where three faces meet",
        ));
    }
    faces
        .iter()
        .map(|&f| match solid.faces[f as usize].surface {
            Surface::Plane(p) => Ok(p),
            Surface::Revolution(_) | Surface::Spline(_) => Err(NativeError::Unsupported(
                "blend a corner where curved faces meet",
            )),
        })
        .collect()
}

/// `x` with `n_i · x = d_i` for three rows, by Cramer's rule.
fn solve3(rows: &[(Vec3, f64)]) -> Option<Vec3> {
    let [(a, da), (b, db), (c, dc)] = rows[..] else {
        return None;
    };
    let det = a.dot(b.cross(c));
    if det.abs() < 1e-9 {
        return None;
    }
    Some((b.cross(c) * da + c.cross(a) * db + a.cross(b) * dc) * (1.0 / det))
}

/// The tool of a corner where three blends meet: for chamfers the tetrahedron between the
/// three bevels and the triangle through the points where their edges on each face cross;
/// for rounds between three planes, the box between the planes and the ends of the three
/// rounds' tools, less the sphere they all touch.
fn corner_tool(
    solid: &Solid,
    v: u32,
    meeting: &[&Run],
    blend: Blend,
    faces_at: &[u32],
) -> Result<Tool, NativeError> {
    let planes = corner_planes(solid, meeting)?;
    let faces: Vec<u32> = {
        let mut f: Vec<u32> = meeting.iter().flat_map(|r| r.faces).collect();
        f.sort_unstable();
        f.dedup();
        f
    };
    let _ = faces_at;
    let p = solid.point(v);
    match blend {
        Blend::Chamfer { .. } => {
            if !meeting[0].convex {
                return Err(NativeError::Unsupported(
                    "chamfer a corner where three valleys meet",
                ));
            }
            // Each bevel's plane, through where it leaves both its faces.
            let bevels: Vec<(Vec3, f64)> = meeting
                .iter()
                .map(|run| {
                    let a = run.along.world(run.cut.touch[0], 0.0);
                    let b = run.along.world(run.cut.touch[1], 0.0);
                    let Along::Straight { dir, .. } = run.along else {
                        return Err(NativeError::Unsupported(
                            "chamfer a corner where curved edges meet",
                        ));
                    };
                    let n = (b - a)
                        .cross(dir)
                        .normalized()
                        .ok_or_else(|| NativeError::Internal("a bevel of no width".to_owned()))?;
                    Ok((n, n.dot(a.to_vec())))
                })
                .collect::<Result<_, _>>()?;
            // On each face, where the lines its two bevels leave it along cross.
            let mut points = Vec::new();
            for (i, &f) in faces.iter().enumerate() {
                let on: Vec<&Run> = meeting
                    .iter()
                    .filter(|r| r.faces.contains(&f))
                    .copied()
                    .collect();
                let [r1, r2] = on[..] else {
                    return Err(NativeError::Internal(
                        "a corner face with one bevel".to_owned(),
                    ));
                };
                let line = |run: &Run| {
                    let k = usize::from(run.faces[1] == f);
                    let Along::Straight { dir, .. } = run.along else {
                        return (Point3::ORIGIN, Vec3::X);
                    };
                    (run.along.world(run.cut.touch[k], 0.0), dir)
                };
                let (a, da) = line(r1);
                let (b, db) = line(r2);
                let n = planes[i].normal;
                // a + s da = b + t db, in the face's plane.
                let rows = [
                    (da.cross(n), da.cross(n).dot(a.to_vec())),
                    (db.cross(n), db.cross(n).dot(b.to_vec())),
                    (n, n.dot(a.to_vec())),
                ];
                let x = solve3(&rows).ok_or(NativeError::Unsupported(
                    "chamfer a corner whose bevels run side by side",
                ))?;
                points.push(Point3::new(x.x, x.y, x.z));
            }
            let apex = solve3(&bevels).ok_or(NativeError::Unsupported(
                "chamfer a corner whose bevels do not meet in a point",
            ))?;
            let apex = Point3::new(apex.x, apex.y, apex.z);
            let tetra = polyhedron(
                &[points[0], points[1], points[2], apex],
                &[vec![0, 1, 2], vec![0, 1, 3], vec![1, 2, 3], vec![2, 0, 3]],
            )?;
            // The face across the three points is the corner.
            let corner_normal = (points[1] - points[0]).cross(points[2] - points[0]);
            let roles = tetra
                .faces
                .iter()
                .map(|face| match face.surface {
                    Surface::Plane(pl)
                        if pl.normal.cross(corner_normal).length()
                            < 1e-9 * corner_normal.length().max(1.0)
                            && (pl.origin - points[0]).dot(pl.normal).abs() < TOL =>
                    {
                        Role::Corner(v)
                    }
                    _ => Role::Support,
                })
                .collect();
            let _ = p;
            Ok(Tool {
                shape: Shape {
                    solids: vec![tetra],
                },
                roles,
            })
        }
        Blend::Fillet { radius } => {
            let center = corner_center(solid, v, meeting, blend)?;
            // Where the sphere touches each plane, and where it is square to each edge.
            let touch: Vec<Point3> = planes
                .iter()
                .map(|pl| center - pl.normal * (center - pl.origin).dot(pl.normal))
                .collect();
            let mut feet = Vec::new();
            for run in meeting {
                let Along::Straight { origin, dir, .. } = run.along else {
                    return Err(NativeError::Unsupported(
                        "round a corner where curved edges meet",
                    ));
                };
                feet.push((run.faces, origin + dir * (center - origin).dot(dir)));
            }
            // Vertices: the corner, the centre, the three touches, the three feet.
            let mut points = vec![p, center];
            points.extend(touch.iter().copied());
            let foot_of = |a: usize, b: usize| -> Option<usize> {
                feet.iter()
                    .position(|(fs, _)| fs.contains(&faces[a]) && fs.contains(&faces[b]))
            };
            for (_, f) in &feet {
                points.push(*f);
            }
            let foot = |a: usize, b: usize| foot_of(a, b).map(|i| 5 + i);
            let mut polys: Vec<Vec<usize>> = Vec::new();
            for i in 0..3 {
                let (j, k) = ((i + 1) % 3, (i + 2) % 3);
                let (Some(fij), Some(fik), Some(fjk)) = (foot(i, j), foot(i, k), foot(j, k)) else {
                    return Err(NativeError::Internal("a corner edge is missing".to_owned()));
                };
                // On plane i: corner, foot on edge ij, touch i, foot on edge ik.
                polys.push(vec![0, fij, 2 + i, fik]);
                // Square to edge jk through the centre: centre, touch j, foot jk, touch k.
                polys.push(vec![1, 2 + j, fjk, 2 + k]);
            }
            let block = polyhedron(&points, &polys)?;
            // The sphere, its pole where it touches the face the lowest edge does not.
            let lowest = meeting
                .iter()
                .min_by_key(|r| r.edge)
                .map(|r| r.faces)
                .unwrap_or_default();
            let pole_face = (0..3).find(|&i| !lowest.contains(&faces[i])).unwrap_or(0);
            let axis_dir = (center - touch[pole_face])
                .normalized()
                .ok_or_else(|| NativeError::Internal("a sphere of no size".to_owned()))?;
            let ball = ball(center, axis_dir, radius)?;
            let (shape, history) = boolean::boolean(
                BooleanOp::Cut,
                &Shape {
                    solids: vec![block],
                },
                &[&ball],
            )?;
            let roles = history
                .faces
                .iter()
                .map(|sources| {
                    if sources
                        .iter()
                        .any(|s| matches!(s, FaceSource::InputFace { input: 1, .. }))
                    {
                        Role::Corner(v)
                    } else {
                        Role::Support
                    }
                })
                .collect();
            Ok(Tool { shape, roles })
        }
    }
}

/// A ball about `center`, its axis along `axis` (its south pole at `center − radius axis`).
fn ball(center: Point3, axis: Vec3, radius: f64) -> Result<Shape, NativeError> {
    let x = perpendicular(axis);
    let plane = Plane {
        origin: center,
        x_dir: x,
        normal: x.cross(axis),
    };
    let profile = Profile {
        regions: vec![Region {
            outer: Loop {
                segments: vec![
                    ProfileSegment {
                        id: 0,
                        curve: Curve2::Line {
                            start: Point2::new(0.0, -radius),
                            end: Point2::new(0.0, radius),
                        },
                    },
                    ProfileSegment {
                        id: 1,
                        curve: Curve2::Arc {
                            start: Point2::new(0.0, radius),
                            mid: Point2::new(radius, 0.0),
                            end: Point2::new(0.0, -radius),
                        },
                    },
                ],
            },
            holes: Vec::new(),
        }],
    };
    let (mut shape, _) = revolve::revolve(
        &plane,
        &profile,
        &Axis {
            origin: center,
            direction: axis,
        },
        TAU,
    )?;
    let wanted = Revolution {
        axis: Axis {
            origin: center,
            direction: axis,
        },
        x_dir: x,
        meridian: Meridian::Circle {
            center: 0.0,
            radius,
        },
        outward: true,
    };
    for solid in &mut shape.solids {
        for face in &mut solid.faces {
            if let Surface::Revolution(r) = &mut face.surface {
                *r = r.reframed(&wanted);
            }
        }
    }
    Ok(shape)
}

/// A solid bounded by planar polygons (indices into `points`), turned to face outwards: a
/// convex polyhedron.
fn polyhedron(points: &[Point3], polys: &[Vec<usize>]) -> Result<Solid, NativeError> {
    let n = points.len() as f64;
    let centroid = points.iter().fold(Vec3::ZERO, |acc, p| acc + p.to_vec()) * (1.0 / n);
    let mut solid = Solid {
        vertices: points.iter().map(|&p| Vertex::new(p)).collect(),
        ..Solid::default()
    };
    let mut edges: HashMap<(usize, usize), u32> = HashMap::new();
    for poly in polys {
        // Newell's normal, turned away from the middle.
        let mut normal = Vec3::ZERO;
        for (i, &a) in poly.iter().enumerate() {
            let (p, q) = (points[a], points[poly[(i + 1) % poly.len()]]);
            normal = normal
                + Vec3::new(
                    (p.y - q.y) * (p.z + q.z),
                    (p.z - q.z) * (p.x + q.x),
                    (p.x - q.x) * (p.y + q.y),
                );
        }
        let mut poly = poly.clone();
        let middle = poly
            .iter()
            .fold(Vec3::ZERO, |acc, &i| acc + points[i].to_vec())
            * (1.0 / poly.len() as f64);
        if normal.dot(middle - centroid) < 0.0 {
            poly.reverse();
            normal = -normal;
        }
        let normal = normal
            .normalized()
            .ok_or_else(|| NativeError::Internal("a flat face of no area".to_owned()))?;
        let mut uses = Vec::new();
        for (i, &a) in poly.iter().enumerate() {
            let b = poly[(i + 1) % poly.len()];
            let key = (a.min(b), a.max(b));
            let e = *edges.entry(key).or_insert_with(|| {
                solid.edges.push(Edge::line(a as u32, b as u32));
                solid.edges.len() as u32 - 1
            });
            let reversed = solid.edges[e as usize].start != a as u32;
            uses.push(EdgeUse { edge: e, reversed });
        }
        let origin = points[poly[0]];
        let x_dir = (points[poly[1]] - origin)
            .normalized()
            .unwrap_or_else(|| perpendicular(normal));
        solid.faces.push(Face {
            surface: Surface::Plane(Plane {
                origin,
                x_dir,
                normal,
            }),
            loops: vec![uses],
        });
    }
    Ok(solid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 40 × 30 × 10 box and the index of its edge along x at y = 0, z = 10.
    fn block() -> (Shape, u32) {
        let shape = build::make_box(Point3::ORIGIN, Vec3::new(40.0, 30.0, 10.0)).unwrap();
        let solid = &shape.solids[0];
        let edge = solid
            .edges
            .iter()
            .position(|e| {
                let (a, b) = (solid.point(e.start), solid.point(e.end));
                (a.y.abs() < 1e-9 && b.y.abs() < 1e-9)
                    && (a.z - 10.0).abs() < 1e-9
                    && (b.z - 10.0).abs() < 1e-9
            })
            .unwrap() as u32;
        (shape, edge)
    }

    #[test]
    fn a_chamfer_of_a_box_edge_takes_off_a_prism() {
        let (shape, edge) = block();
        let chamfer = Blend::Chamfer {
            first: 2.0,
            second: ChamferSecond::Distance(2.0),
        };
        let (out, history) = blend(&shape, &[(edge, None)], chamfer, false).unwrap();
        let volume = out.volume().unwrap();
        assert!((volume - (12000.0 - 80.0)).abs() < 1e-6, "{volume}");
        assert_eq!(out.solids[0].faces.len(), 7);
        assert!(
            history
                .faces
                .iter()
                .flatten()
                .any(|s| matches!(s, FaceSource::InputEdge { .. }))
        );
    }

    #[test]
    fn rounds_in_a_pocket_s_corners_fill_them_in() {
        let block = build::make_box(Point3::ORIGIN, Vec3::new(60.0, 40.0, 10.0)).unwrap();
        let hole =
            build::make_box(Point3::new(15.0, 10.0, -1.0), Vec3::new(30.0, 20.0, 12.0)).unwrap();
        let (pocket, _) = boolean::boolean(BooleanOp::Cut, &block, &[&hole]).unwrap();
        let solid = &pocket.solids[0];
        let corners: Vec<(u32, Option<u32>)> = solid
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let (a, b) = (solid.point(e.start), solid.point(e.end));
                (a.x - b.x).abs() < 1e-9
                    && (a.y - b.y).abs() < 1e-9
                    && (a.x == 15.0 || a.x == 45.0)
                    && (a.y == 10.0 || a.y == 30.0)
            })
            .map(|(i, _)| (i as u32, None))
            .collect();
        assert_eq!(corners.len(), 4);
        let (out, _) = blend(&pocket, &corners, Blend::Fillet { radius: 3.0 }, false).unwrap();
        let volume = out.volume().unwrap();
        let added = 4.0 * (9.0 - std::f64::consts::PI * 9.0 / 4.0) * 10.0;
        assert!(
            (volume - (24000.0 - 6000.0 + added)).abs() < 1e-6,
            "{volume}"
        );
    }

    #[test]
    fn a_fillet_of_a_box_edge_takes_off_a_quarter_round() {
        let (shape, edge) = block();
        let (out, _) = blend(
            &shape,
            &[(edge, None)],
            Blend::Fillet { radius: 2.0 },
            false,
        )
        .unwrap();
        let volume = out.volume().unwrap();
        let removed = (4.0 - std::f64::consts::PI) * 40.0;
        assert!((volume - (12000.0 - removed)).abs() < 1e-6, "{volume}");
    }
}
