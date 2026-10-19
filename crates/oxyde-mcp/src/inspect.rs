//! Reading geometry the way a designer reads a drawing: faces and edges with their
//! measures, exact sections through a plane with the shapes of their loops recognised
//! (circles, rectangles, slots, polygons) and grouped into patterns, the comparison of two
//! sets of solids by booleans, and distances.

use std::collections::BTreeMap;
use std::f64::consts::{PI, TAU};

use oxyde_geom::{Plane, Point2, Point3, Transform, Vec3};
use oxyde_kernel::{
    BooleanOp, CurveKind, FaceSource, Kernel, Mesh, ShapeHandle, SubShape, SurfaceKind, Topology,
};
use oxyde_occt::OcctKernel;
use serde_json::{Value, json};

use crate::args::Set;
use crate::args::{self, Result, round};
use crate::geometry::{SolidRange, Solids};
use crate::select::{Kind, Scope};

// ---- Faces and edges ---------------------------------------------------------------------------

pub(crate) fn surface_name(kind: SurfaceKind) -> &'static str {
    match kind {
        SurfaceKind::Plane => "plane",
        SurfaceKind::Cylinder => "cylinder",
        SurfaceKind::Cone => "cone",
        SurfaceKind::Sphere => "sphere",
        SurfaceKind::Torus => "torus",
        SurfaceKind::BSpline => "bspline",
        SurfaceKind::Other => "other",
    }
}

pub(crate) fn curve_name(kind: CurveKind) -> &'static str {
    match kind {
        CurveKind::Line => "line",
        CurveKind::Circle => "circle",
        CurveKind::Ellipse => "ellipse",
        CurveKind::BSpline => "bspline",
        CurveKind::Degenerate => "degenerate",
        CurveKind::Other => "other",
    }
}

/// One face, as `faces` lists it.
pub(crate) fn face_json(scope: &Scope<'_>, i: u32) -> Value {
    let Some(f) = scope.solids.topology.faces.get(i as usize) else {
        return Value::Null;
    };
    let mut out = json!({
        "index": i,
        "name": scope.solids.face_label(i),
        "surface": surface_name(f.surface),
        "area": round(f.area),
        "centroid": args::round3(f.centroid),
    });
    if let Some(n) = f.normal {
        out.set("normal", json!(args::round_vec(n)));
    }
    if let Some(axis) = f.axis {
        out.set(
            "axis",
            json!({ "origin": args::round3(axis.origin),
            "direction": args::round_vec(axis.direction) }),
        );
    }
    if let Some(r) = scope.radius_of(Kind::Face, i) {
        out.set("radius", json!(round(r)));
    }
    out
}

/// One edge, as `edges` lists it.
pub(crate) fn edge_json(scope: &Scope<'_>, i: u32) -> Value {
    let topology = scope.solids.topology;
    let Some(e) = topology.edges.get(i as usize) else {
        return Value::Null;
    };
    let mut out = json!({
        "index": i,
        "name": scope.solids.edge_label(i),
        "curve": curve_name(e.curve),
        "length": round(e.length),
        "midpoint": args::round3(e.midpoint),
        "faces": e.faces,
    });
    let ends: Vec<[f64; 3]> = e
        .vertices
        .iter()
        .filter_map(|&v| topology.vertices.get(v as usize))
        .map(|v| args::round3(v.point))
        .collect();
    out.set("ends", json!(ends));
    if let Some(axis) = e.axis {
        match e.curve {
            CurveKind::Line => out.set("direction", json!(args::round_vec(axis.direction))),
            CurveKind::Circle => {
                out.set("center", json!(args::round3(axis.origin)));
                out.set("normal", json!(args::round_vec(axis.direction)));
                if let Some(r) = scope.radius_of(Kind::Edge, i) {
                    out.set("radius", json!(round(r)));
                }
            }
            _ => {}
        }
    }
    out
}

// ---- Sections ----------------------------------------------------------------------------------

/// A curve of a section, in the plane's frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Seg {
    Line(Point2, Point2),
    /// From `from` to `to` counter-clockwise when `ccw`, about `center`.
    Arc {
        center: Point2,
        radius: f64,
        from: Point2,
        to: Point2,
        ccw: bool,
    },
    Circle(Point2, f64),
    /// A curve that is neither (ellipse, spline), as points.
    Points(Vec<Point2>),
}

impl Seg {
    fn ends(&self) -> Option<(Point2, Point2)> {
        match self {
            Seg::Line(a, b) => Some((*a, *b)),
            Seg::Arc { from, to, .. } => Some((*from, *to)),
            Seg::Circle(..) => None,
            Seg::Points(p) => Some((*p.first()?, *p.last()?)),
        }
    }

    /// Points along the curve, from its start to its end.
    fn polyline(&self) -> Vec<Point2> {
        match self {
            Seg::Line(a, b) => vec![*a, *b],
            Seg::Arc {
                center,
                radius,
                from,
                to,
                ccw,
            } => {
                let a0 = (from.y - center.y).atan2(from.x - center.x);
                let a1 = (to.y - center.y).atan2(to.x - center.x);
                let sweep = if *ccw {
                    (a1 - a0).rem_euclid(TAU)
                } else {
                    -(a0 - a1).rem_euclid(TAU)
                };
                let steps = ((sweep.abs() / (PI / 32.0)).ceil() as usize).max(2);
                (0..=steps)
                    .map(|k| {
                        let t = a0 + sweep * k as f64 / steps as f64;
                        Point2::new(center.x + radius * t.cos(), center.y + radius * t.sin())
                    })
                    .collect()
            }
            Seg::Circle(c, r) => (0..64)
                .map(|k| {
                    let t = TAU * k as f64 / 64.0;
                    Point2::new(c.x + r * t.cos(), c.y + r * t.sin())
                })
                .collect(),
            Seg::Points(p) => p.clone(),
        }
    }

    fn json(&self) -> Value {
        match self {
            Seg::Line(a, b) => json!({ "line": [rp(*a), rp(*b)] }),
            Seg::Arc {
                center,
                radius,
                from,
                to,
                ccw,
            } => json!({ "arc": { "center": rp(*center), "radius": round(*radius),
                "from": rp(*from), "to": rp(*to), "ccw": ccw } }),
            Seg::Circle(c, r) => {
                json!({ "circle": { "center": rp(*c), "diameter": round(2.0 * r) } })
            }
            Seg::Points(p) => json!({ "points": p.iter().map(|q| rp(*q)).collect::<Vec<_>>() }),
        }
    }
}

fn rp(p: Point2) -> [f64; 2] {
    [round(p.x), round(p.y)]
}

/// A closed loop of a section.
#[derive(Debug, Clone)]
pub(crate) struct SectionLoop {
    pub segments: Vec<Seg>,
    pub polygon: Vec<Point2>,
    /// Unsigned.
    pub area: f64,
    /// 0 for an outer boundary, 1 for a hole in it, 2 for an island in the hole…
    pub depth: usize,
}

/// A section of solids by a plane: its loops.
pub(crate) struct Section {
    pub loops: Vec<SectionLoop>,
}

/// Cuts `shapes` by `plane` (keeping the side behind its normal) and returns the curves
/// the cut leaves on the plane, chained into loops, in the plane's frame.
pub(crate) fn section(
    kernel: &mut OcctKernel,
    shapes: &[ShapeHandle],
    plane: &Plane,
) -> Result<Section> {
    let (lo, hi) = crate::geometry::bounds(kernel, shapes).ok_or("nothing to cut")?;
    let size = (hi - lo).length().max(1.0) * 4.0;
    // The half-space in front of the plane, as a big box: its back face on the plane,
    // centred on where the solids are.
    let middle = Point3::new(
        (lo.x + hi.x) / 2.0,
        (lo.y + hi.y) / 2.0,
        (lo.z + hi.z) / 2.0,
    );
    let along = (middle - plane.origin).dot(plane.normal);
    let centre = middle - plane.normal * along;
    let frame = Plane {
        origin: centre,
        ..*plane
    };
    let local = kernel
        .make_box(
            Point3::new(-size / 2.0, -size / 2.0, 0.0),
            Vec3::new(size, size, size),
        )
        .map_err(|e| e.to_string())?;
    let placed = kernel.transform(local, &Transform::from_plane(&frame));
    kernel.release(local);
    let half_space = placed.map_err(|e| e.to_string())?.shape;
    let mut chains: Vec<Vec<Seg>> = Vec::new();
    let mut failure = None;
    for shape in shapes {
        match kernel.boolean(BooleanOp::Cut, *shape, half_space) {
            Ok(result) => {
                let cut = cut_segments(kernel, result.shape, &result.history.faces, plane);
                kernel.release(result.shape);
                match cut {
                    // Each cut face's curves are chained on their own, so loops of different
                    // faces meeting at a point are not run together.
                    Ok(faces) => faces.into_iter().for_each(|f| chains.extend(chain(f))),
                    Err(e) => failure = Some(e),
                }
            }
            Err(e) => failure = Some(e.to_string()),
        }
    }
    kernel.release(half_space);
    if let (Some(e), true) = (failure, chains.is_empty()) {
        return Err(e);
    }
    Ok(Section {
        loops: nest(chains),
    })
}

/// The curves bounding each face the cut made (those coming from the tool, input 1), face
/// by face.
fn cut_segments(
    kernel: &OcctKernel,
    shape: ShapeHandle,
    history: &[Vec<FaceSource>],
    plane: &Plane,
) -> Result<Vec<Vec<Seg>>> {
    let topology = kernel.topology(shape).map_err(|e| e.to_string())?;
    let mesh = kernel.tessellate(shape, 0.01).map_err(|e| e.to_string())?;
    let mut faces: Vec<Vec<u32>> = Vec::new();
    for (index, sources) in history.iter().enumerate() {
        let from_tool = sources
            .iter()
            .any(|s| matches!(s, FaceSource::InputFace { input: 1, .. }));
        if !from_tool {
            continue;
        }
        if let Some(face) = topology.faces.get(index) {
            let mut edges = face.edges.clone();
            edges.sort_unstable();
            edges.dedup();
            faces.push(edges);
        }
    }
    let to_plane = |p: Point3| {
        let v = p - plane.origin;
        Point2::new(v.dot(plane.x_dir), v.dot(plane.y_dir()))
    };
    let mut groups = Vec::new();
    for edges in faces {
        let mut out = Vec::new();
        for e in edges {
            let Some(info) = topology.edges.get(e as usize) else {
                continue;
            };
            let ends: Vec<Point2> = info
                .vertices
                .iter()
                .filter_map(|&v| topology.vertices.get(v as usize))
                .map(|v| to_plane(v.point))
                .collect();
            let seg = match (info.curve, info.axis, ends.as_slice()) {
                (CurveKind::Line, _, [a, b]) => Seg::Line(*a, *b),
                (CurveKind::Circle, Some(axis), ends) => {
                    let center = to_plane(axis.origin);
                    let radius = (info.midpoint - axis.origin).length();
                    match ends {
                        [a, b] if a.distance(*b) > 1e-9 => {
                            let mid = to_plane(info.midpoint);
                            let angle = |p: Point2| (p.y - center.y).atan2(p.x - center.x);
                            let (ta, tb, tm) = (angle(*a), angle(*b), angle(mid));
                            let ccw = (tm - ta).rem_euclid(TAU) < (tb - ta).rem_euclid(TAU);
                            Seg::Arc {
                                center,
                                radius,
                                from: *a,
                                to: *b,
                                ccw,
                            }
                        }
                        _ => Seg::Circle(center, radius),
                    }
                }
                _ => {
                    let points: Vec<Point2> = mesh
                        .edges
                        .get(e as usize)
                        .map(|line| {
                            line.iter()
                                .map(|p| to_plane(Point3::ORIGIN + crate::geometry::v3(*p)))
                                .collect()
                        })
                        .unwrap_or_default();
                    if points.len() < 2 {
                        continue;
                    }
                    Seg::Points(points)
                }
            };
            out.push(seg);
        }
        groups.push(out);
    }
    Ok(groups)
}

/// Chains curves end to end into closed loops and works out which loop lies in which.
#[cfg(test)]
fn chain_loops(segments: Vec<Seg>) -> Vec<SectionLoop> {
    nest(chain(segments))
}

/// Chains curves end to end into closed loops.
fn chain(segments: Vec<Seg>) -> Vec<Vec<Seg>> {
    const JOIN: f64 = 1e-5;
    let mut loops: Vec<Vec<Seg>> = Vec::new();
    let mut open: Vec<Seg> = Vec::new();
    for seg in segments {
        match seg {
            Seg::Circle(..) => loops.push(vec![seg]),
            Seg::Points(ref p)
                if p.len() > 2
                    && p.first()
                        .zip(p.last())
                        .is_some_and(|(a, b)| a.distance(*b) < JOIN) =>
            {
                loops.push(vec![seg]);
            }
            other => open.push(other),
        }
    }
    while let Some(first) = open.pop() {
        let Some((start, mut end)) = first.ends() else {
            continue;
        };
        let mut chain = vec![first];
        let mut guard = 0;
        while end.distance(start) > JOIN && guard < 100_000 {
            guard += 1;
            let next = open.iter().position(|s| {
                s.ends()
                    .is_some_and(|(a, b)| a.distance(end) < JOIN || b.distance(end) < JOIN)
            });
            let Some(k) = next else {
                break;
            };
            let seg = open.remove(k);
            let Some((a, b)) = seg.ends() else {
                break;
            };
            if a.distance(end) < JOIN {
                end = b;
                chain.push(seg);
            } else {
                end = a;
                chain.push(reverse(seg));
            }
        }
        loops.push(chain);
    }
    loops
}

/// Works out which loop lies in which: the depth of each, outer boundaries first.
fn nest(loops: Vec<Vec<Seg>>) -> Vec<SectionLoop> {
    let mut out: Vec<SectionLoop> = loops
        .into_iter()
        .map(|segments| {
            let mut polygon: Vec<Point2> = Vec::new();
            for s in &segments {
                let pts = s.polyline();
                let skip = usize::from(!polygon.is_empty());
                polygon.extend(pts.into_iter().skip(skip));
            }
            let area = signed_area(&polygon).abs();
            SectionLoop {
                segments,
                polygon,
                area,
                depth: 0,
            }
        })
        .collect();
    // Depth: how many other loops hold a point of this one.
    let depths: Vec<usize> = out
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let probe = l.polygon.first().copied().unwrap_or(Point2::ORIGIN);
            out.iter()
                .enumerate()
                .filter(|(j, other)| {
                    *j != i && other.area > l.area && contains(&other.polygon, probe)
                })
                .count()
        })
        .collect();
    for (l, d) in out.iter_mut().zip(depths) {
        l.depth = d;
    }
    out.sort_by(|a, b| a.depth.cmp(&b.depth).then(b.area.total_cmp(&a.area)));
    out
}

fn reverse(seg: Seg) -> Seg {
    match seg {
        Seg::Line(a, b) => Seg::Line(b, a),
        Seg::Arc {
            center,
            radius,
            from,
            to,
            ccw,
        } => Seg::Arc {
            center,
            radius,
            from: to,
            to: from,
            ccw: !ccw,
        },
        Seg::Points(mut p) => {
            p.reverse();
            Seg::Points(p)
        }
        circle => circle,
    }
}

fn signed_area(polygon: &[Point2]) -> f64 {
    let n = polygon.len();
    (0..n)
        .filter_map(|i| {
            let a = polygon.get(i)?;
            let b = polygon.get((i + 1) % n)?;
            Some(a.x * b.y - b.x * a.y)
        })
        .sum::<f64>()
        / 2.0
}

fn contains(polygon: &[Point2], p: Point2) -> bool {
    let n = polygon.len();
    let mut inside = false;
    for i in 0..n {
        let (Some(a), Some(b)) = (polygon.get(i), polygon.get((i + n - 1) % n)) else {
            continue;
        };
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}

/// What a loop is, as the sketch tools would draw it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Shape {
    Circle {
        center: Point2,
        diameter: f64,
    },
    Rect {
        center: Point2,
        w: f64,
        h: f64,
        radius: f64,
    },
    Slot {
        start: Point2,
        end: Point2,
        width: f64,
    },
    Polygon {
        center: Point2,
        sides: usize,
        radius: f64,
        angle: f64,
    },
    Outline,
}

impl Shape {
    fn center(&self) -> Option<Point2> {
        match self {
            Shape::Circle { center, .. }
            | Shape::Rect { center, .. }
            | Shape::Polygon { center, .. } => Some(*center),
            Shape::Slot { start, end, .. } => Some(Point2::new(
                (start.x + end.x) / 2.0,
                (start.y + end.y) / 2.0,
            )),
            Shape::Outline => None,
        }
    }

    /// What tells shapes of the same size apart, wherever they are.
    fn signature(&self) -> Option<String> {
        let r = |v: f64| round(v * 1000.0) / 1000.0;
        match self {
            Shape::Circle { diameter, .. } => Some(format!("circle diameter {}", r(*diameter))),
            Shape::Rect { w, h, radius, .. } if *radius > 0.0 => {
                Some(format!("rect {} x {} radius {}", r(*w), r(*h), r(*radius)))
            }
            Shape::Rect { w, h, .. } => Some(format!("rect {} x {}", r(*w), r(*h))),
            Shape::Slot { start, end, width } => Some(format!(
                "slot dx {} dy {} width {}",
                r((end.x - start.x).abs()),
                r((end.y - start.y).abs()),
                r(*width)
            )),
            Shape::Polygon {
                sides,
                radius,
                angle,
                ..
            } => Some(format!(
                "polygon {sides} sides radius {} angle {}",
                r(*radius),
                r(angle.to_degrees())
            )),
            Shape::Outline => None,
        }
    }

    fn json(&self) -> Value {
        match self {
            Shape::Circle { center, diameter } => {
                json!({ "type": "circle", "center": rp(*center), "diameter": round(*diameter) })
            }
            Shape::Rect {
                center,
                w,
                h,
                radius,
            } => {
                let mut v = json!({ "type": "rect", "center": rp(*center), "size": [round(*w), round(*h)] });
                if *radius > 0.0 {
                    v.set("radius", json!(round(*radius)));
                }
                v
            }
            Shape::Slot { start, end, width } => {
                json!({ "type": "slot", "start": rp(*start), "end": rp(*end), "width": round(*width) })
            }
            Shape::Polygon {
                center,
                sides,
                radius,
                angle,
            } => json!({ "type": "polygon",
                "center": rp(*center), "sides": sides, "radius": round(*radius),
                "angle": round(angle.to_degrees()) }),
            Shape::Outline => json!({ "type": "outline" }),
        }
    }
}

const SAME: f64 = 1e-4;

/// Recognises a loop as one of the sketch tools' shapes.
pub(crate) fn recognise(segments: &[Seg]) -> Shape {
    let lines: Vec<(Point2, Point2)> = segments
        .iter()
        .filter_map(|s| match s {
            Seg::Line(a, b) => Some((*a, *b)),
            _ => None,
        })
        .collect();
    let arcs: Vec<(Point2, f64, Point2, Point2)> = segments
        .iter()
        .filter_map(|s| match s {
            Seg::Arc {
                center,
                radius,
                from,
                to,
                ..
            } => Some((*center, *radius, *from, *to)),
            _ => None,
        })
        .collect();
    let axis_aligned =
        |(a, b): &(Point2, Point2)| (a.x - b.x).abs() < SAME || (a.y - b.y).abs() < SAME;
    let bbox = |points: &mut dyn Iterator<Item = Point2>| {
        let mut lo = Point2::new(f64::INFINITY, f64::INFINITY);
        let mut hi = Point2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
        for p in points {
            lo = Point2::new(lo.x.min(p.x), lo.y.min(p.y));
            hi = Point2::new(hi.x.max(p.x), hi.y.max(p.y));
        }
        (lo, hi)
    };
    match (segments, lines.len(), arcs.len()) {
        ([Seg::Circle(c, r)], ..) => Shape::Circle {
            center: *c,
            diameter: 2.0 * r,
        },
        (_, 4, 0) if lines.iter().all(axis_aligned) => {
            let (lo, hi) = bbox(&mut lines.iter().flat_map(|(a, b)| [*a, *b]));
            Shape::Rect {
                center: Point2::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0),
                w: hi.x - lo.x,
                h: hi.y - lo.y,
                radius: 0.0,
            }
        }
        (_, 4, 4)
            if lines.iter().all(axis_aligned)
                && arcs
                    .iter()
                    .all(|a| (a.1 - arcs.first().map_or(0.0, |f| f.1)).abs() < SAME) =>
        {
            let r = arcs.first().map_or(0.0, |a| a.1);
            let (lo, hi) = bbox(&mut lines.iter().flat_map(|(a, b)| [*a, *b]));
            Shape::Rect {
                center: Point2::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0),
                w: hi.x - lo.x,
                h: hi.y - lo.y,
                radius: r,
            }
        }
        (_, 2, 2) => {
            let (Some(a), Some(b)) = (arcs.first(), arcs.get(1)) else {
                return Shape::Outline;
            };
            let half = |arc: &(Point2, f64, Point2, Point2)| {
                (arc.2.distance(arc.3) - 2.0 * arc.1).abs() < 1e-3
            };
            if (a.1 - b.1).abs() < SAME && half(a) && half(b) {
                Shape::Slot {
                    start: a.0,
                    end: b.0,
                    width: 2.0 * a.1,
                }
            } else {
                Shape::Outline
            }
        }
        (_, n, 0) if n >= 3 => {
            let corners: Vec<Point2> = lines.iter().map(|(a, _)| *a).collect();
            let count = corners.len() as f64;
            let center = Point2::new(
                corners.iter().map(|p| p.x).sum::<f64>() / count,
                corners.iter().map(|p| p.y).sum::<f64>() / count,
            );
            let radius = corners.first().map_or(0.0, |p| p.distance(center));
            let length = lines.first().map_or(0.0, |(a, b)| a.distance(*b));
            let regular = corners
                .iter()
                .all(|p| (p.distance(center) - radius).abs() < 1e-3)
                && lines
                    .iter()
                    .all(|(a, b)| (a.distance(*b) - length).abs() < 1e-3);
            if regular {
                let step = TAU / count;
                let angle = corners
                    .iter()
                    .map(|p| (p.y - center.y).atan2(p.x - center.x).rem_euclid(step))
                    .fold(f64::INFINITY, f64::min);
                let angle = if (step - angle).abs() < 1e-6 {
                    0.0
                } else {
                    angle
                };
                Shape::Polygon {
                    center,
                    sides: n,
                    radius,
                    angle,
                }
            } else {
                Shape::Outline
            }
        }
        _ => Shape::Outline,
    }
}

/// A section as the `section` tool reports it: the outer boundaries with their curves,
/// and the other loops recognised and grouped by shape, with where the copies sit.
pub(crate) fn section_json(section: &Section, detail: bool) -> Value {
    let mut outlines = Vec::new();
    let mut groups: BTreeMap<(usize, String), Vec<Point2>> = BTreeMap::new();
    let mut others = Vec::new();
    let mut material = 0.0;
    let mut lo = Point2::new(f64::INFINITY, f64::INFINITY);
    let mut hi = Point2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    for l in &section.loops {
        material += if l.depth % 2 == 0 { l.area } else { -l.area };
        for p in &l.polygon {
            lo = Point2::new(lo.x.min(p.x), lo.y.min(p.y));
            hi = Point2::new(hi.x.max(p.x), hi.y.max(p.y));
        }
        let shape = recognise(&l.segments);
        match (shape.signature(), l.depth) {
            (Some(sig), d) if d > 0 || !detail => {
                groups
                    .entry((d, sig))
                    .or_default()
                    .push(shape.center().unwrap_or(Point2::ORIGIN));
            }
            _ => {
                let mut v =
                    json!({ "depth": l.depth, "area": round(l.area), "shape": shape.json() });
                if shape == Shape::Outline || detail {
                    v.set(
                        "segments",
                        Value::Array(l.segments.iter().map(Seg::json).collect()),
                    );
                }
                if l.depth == 0 {
                    outlines.push(v);
                } else {
                    others.push(v);
                }
            }
        }
    }
    let groups: Vec<Value> = groups
        .into_iter()
        .map(|((depth, signature), mut centers)| {
            centers.sort_by(|a, b| round(a.y).total_cmp(&round(b.y)).then(a.x.total_cmp(&b.x)));
            let mut v = json!({
                "depth": depth,
                "role": if depth % 2 == 1 { "hole" } else { "material" },
                "shape": signature,
                "count": centers.len(),
                "centers": centers.iter().map(|c| rp(*c)).collect::<Vec<_>>(),
            });
            if centers.len() > 1 {
                v.set("grid", grid(&centers));
            }
            v
        })
        .collect();
    json!({
        "area": round(material),
        "bounds": { "min": rp(lo), "max": rp(hi), "size": [round(hi.x - lo.x), round(hi.y - lo.y)] },
        "loops": section.loops.len(),
        "outlines": outlines,
        "shapes": groups,
        "other_loops": others,
    })
}

/// The distinct x and y of a set of centres, and their spacing when it is even.
fn grid(centers: &[Point2]) -> Value {
    let distinct = |values: Vec<f64>| {
        let mut v: Vec<f64> = values
            .into_iter()
            .map(|x| (x * 1e3).round() / 1e3)
            .collect();
        v.sort_by(f64::total_cmp);
        v.dedup_by(|a, b| (*a - *b).abs() < 2e-3);
        v
    };
    let xs = distinct(centers.iter().map(|c| c.x).collect());
    let ys = distinct(centers.iter().map(|c| c.y).collect());
    let spacing = |v: &[f64]| {
        let steps: Vec<f64> = v
            .windows(2)
            .filter_map(|w| Some(w.get(1)? - w.first()?))
            .collect();
        let first = steps.first().copied()?;
        steps
            .iter()
            .all(|s| (s - first).abs() < 2e-3)
            .then_some(round(first))
    };
    json!({
        "x": xs,
        "y": ys,
        "x_spacing": spacing(&xs),
        "y_spacing": spacing(&ys),
        "full_grid": xs.len() * ys.len() == centers.len(),
    })
}

// ---- Comparison --------------------------------------------------------------------------------

/// How two sets of solids differ.
pub(crate) struct Comparison {
    pub volume_a: f64,
    pub volume_b: f64,
    pub common: f64,
    /// Material in `a` that `b` does not have, and the other way round, as solids the
    /// caller releases (for pictures).
    pub extra: Vec<ShapeHandle>,
    pub missing: Vec<ShapeHandle>,
    /// How far the reference was moved to get a common volume at all, when OpenCASCADE
    /// gave none for solids whose faces coincide exactly.
    pub nudged: Option<f64>,
    /// How the common volume was found.
    pub method: &'static str,
    /// The volumes of `a`, of `b` and of what they share as the meshes were sampled, when
    /// the comparison was made by sampling only: shared space counts once there (solids
    /// touching or overlapping each other), so the match and the shares of extra and missing
    /// material come from these, and the exact volumes only scale them.
    pub sampled: Option<(f64, f64, f64)>,
}

impl Comparison {
    pub fn json(&self) -> Value {
        let (extra, missing, ratio) = match self.sampled {
            Some((sa, sb, sc)) if sa > 0.0 && sb > 0.0 => (
                self.volume_a * (1.0 - sc / sa),
                self.volume_b * (1.0 - sc / sb),
                sc / (sa + sb - sc),
            ),
            _ => {
                let union = self.volume_a + self.volume_b - self.common;
                (
                    self.volume_a - self.common,
                    self.volume_b - self.common,
                    if union > 0.0 {
                        self.common / union
                    } else {
                        0.0
                    },
                )
            }
        };
        let mut out = json!({
            "volume_model": round(self.volume_a),
            "volume_reference": round(self.volume_b),
            "volume_common": round(self.common),
            "volume_extra": round(extra),
            "volume_missing": round(missing),
            "match": round(ratio),
            "nudged_by_mm": self.nudged,
            "method": self.method,
        });
        if let Some((sa, sb, sc)) = self.sampled {
            out.set(
                "sampled_volumes",
                json!({ "model": round(sa), "reference": round(sb), "common": round(sc) }),
            );
        }
        out
    }

    pub fn release(self, kernel: &mut OcctKernel) {
        for s in self.extra.into_iter().chain(self.missing) {
            kernel.release(s);
        }
    }
}

/// The common volume, the volumes left of `a` and of `b`, and those leftovers as solids.
type BooleanVolumes = (f64, f64, f64, Vec<ShapeHandle>, Vec<ShapeHandle>);

/// The volume `a` shares with `b` (a common), and what is left of each after taking the
/// other away (two cuts), with those leftovers kept as solids when `keep`.
fn boolean_volumes(
    kernel: &mut OcctKernel,
    a: &[ShapeHandle],
    b: &[ShapeHandle],
    keep: bool,
) -> Result<BooleanVolumes> {
    let mut common = 0.0;
    for shape in a {
        let r = kernel
            .boolean_many(BooleanOp::Common, *shape, b)
            .map_err(|e| e.to_string())?;
        common += kernel.volume(r.shape).unwrap_or(0.0);
        kernel.release(r.shape);
    }
    let left = |kernel: &mut OcctKernel, from: &[ShapeHandle], take: &[ShapeHandle]| {
        let mut volume = 0.0;
        let mut kept = Vec::new();
        for shape in from {
            match kernel.boolean_many(BooleanOp::Cut, *shape, take) {
                Ok(r) => {
                    volume += kernel.volume(r.shape).unwrap_or(0.0);
                    if keep {
                        kept.push(r.shape);
                    } else {
                        kernel.release(r.shape);
                    }
                }
                // A failed cut leaves the whole solid: what makes the check below fail.
                Err(_) => volume += kernel.volume(*shape).unwrap_or(0.0),
            }
        }
        (volume, kept)
    };
    let (extra, extra_shapes) = left(kernel, a, b);
    let (missing, missing_shapes) = left(kernel, b, a);
    Ok((common, extra, missing, extra_shapes, missing_shapes))
}

/// Compares `a` (the model) with `b` (the reference); `keep` keeps the difference solids.
///
/// OpenCASCADE's booleans can fail without saying so on solids whose faces coincide, which
/// a remodelled part's do with the original's by design: a common that comes back empty,
/// or short of a region. So the common and the two cuts are checked to add up; when they
/// do not, the meshes are sampled ([`sampled_volumes`]) and the boolean figure nearest the
/// sampled one is kept if it agrees within 1%, else the sampled one.
pub(crate) fn compare(
    kernel: &mut OcctKernel,
    a: &[ShapeHandle],
    b: &[ShapeHandle],
    keep: bool,
) -> Result<Comparison> {
    let volume = |kernel: &OcctKernel, shapes: &[ShapeHandle]| -> f64 {
        shapes.iter().filter_map(|s| kernel.volume(*s).ok()).sum()
    };
    let volume_a = volume(kernel, a);
    let volume_b = volume(kernel, b);
    let smaller = volume_a.min(volume_b).max(1e-9);
    let tolerance = 1e-4 * smaller + 1e-3;
    let overlap = match (
        crate::geometry::bounds(kernel, a),
        crate::geometry::bounds(kernel, b),
    ) {
        (Some((a0, a1)), Some((b0, b1))) => {
            a0.x < b1.x && b0.x < a1.x && a0.y < b1.y && b0.y < a1.y && a0.z < b1.z && b0.z < a1.z
        }
        _ => false,
    };
    let (common, extra_v, missing_v, extra, missing) = boolean_volumes(kernel, a, b, keep)?;
    let candidates = [common, volume_a - extra_v, volume_b - missing_v];
    let agree = candidates.iter().all(|c| (c - common).abs() <= tolerance);
    let empty = common <= 1e-6 * smaller && overlap;
    if agree && !empty {
        return Ok(Comparison {
            volume_a,
            volume_b,
            common,
            extra,
            missing,
            nudged: None,
            method: "booleans",
            sampled: None,
        });
    }
    // The booleans disagree with each other: sampling decides.
    let mut candidates: Vec<(f64, Option<f64>)> = candidates.iter().map(|c| (*c, None)).collect();
    if empty {
        // Solids whose faces coincide exactly can come back from OpenCASCADE's common
        // with nothing at all; moved a few microns, they compare as they should, and what
        // the nudge itself takes off is taken back (measured at twice the shift too, a
        // straight line back to no shift).
        let way = Vec3::new(1.0, 0.7, 0.4) * (1.0 / Vec3::new(1.0, 0.7, 0.4).length());
        let moved_common = |kernel: &mut OcctKernel, shift: f64| -> Result<f64> {
            let moved: Vec<ShapeHandle> = b
                .iter()
                .filter_map(|s| {
                    kernel
                        .transform(*s, &Transform::translation(way * shift))
                        .ok()
                        .map(|r| r.shape)
                })
                .collect();
            let mut c = 0.0;
            for shape in a {
                if let Ok(r) = kernel.boolean_many(BooleanOp::Common, *shape, &moved) {
                    c += kernel.volume(r.shape).unwrap_or(0.0);
                    kernel.release(r.shape);
                }
            }
            for m in moved {
                kernel.release(m);
            }
            Ok(c)
        };
        for shift in [0.002, 0.01] {
            let c1 = moved_common(kernel, shift)?;
            if c1 > 1e-6 * smaller {
                let c2 = moved_common(kernel, 2.0 * shift)?;
                let c = if c2 > 1e-6 * smaller {
                    (2.0 * c1 - c2).clamp(c1, smaller)
                } else {
                    c1
                };
                candidates.push((c, Some(shift)));
                break;
            }
        }
    }
    let sampled = sampled_common(kernel, a, b, volume_a, volume_b).unwrap_or(common);
    let nearest = candidates
        .iter()
        .min_by(|x, y| (x.0 - sampled).abs().total_cmp(&(y.0 - sampled).abs()))
        .copied();
    let (common, nudged, method) = match nearest {
        Some((c, nudged)) if (c - sampled).abs() <= 0.01 * smaller => {
            (c, nudged, "booleans, checked by sampling the meshes")
        }
        _ => (sampled, None, "sampled meshes (the booleans failed)"),
    };
    Ok(Comparison {
        volume_a,
        volume_b,
        common,
        extra,
        missing,
        nudged,
        method,
        sampled: None,
    })
}

/// The common volume of `a` and `b` from sampling their meshes ([`sampled`]), scaled by how
/// far the sampled volumes are from the exact ones. `None` when there is nothing to sample.
pub(crate) fn sampled_common(
    kernel: &OcctKernel,
    a: &[ShapeHandle],
    b: &[ShapeHandle],
    volume_a: f64,
    volume_b: f64,
) -> Option<f64> {
    let (sa, sb, sc) = sampled(kernel, a, b, volume_a, volume_b)?;
    Some((sc * (volume_a / sa + volume_b / sb) / 2.0).min(volume_a.min(volume_b)))
}

/// The sampled volumes of `a`, of `b` and of what they share ([`sampled_volumes`]), along
/// the axis whose sampled volumes are nearest the exact ones. `None` when there is nothing
/// to sample.
fn sampled(
    kernel: &OcctKernel,
    a: &[ShapeHandle],
    b: &[ShapeHandle],
    volume_a: f64,
    volume_b: f64,
) -> Option<(f64, f64, f64)> {
    let mesh_of = |shapes: &[ShapeHandle]| -> Mesh {
        let size =
            crate::geometry::bounds(kernel, shapes).map_or(100.0, |(lo, hi)| (hi - lo).length());
        let mut mesh = Mesh::default();
        for s in shapes {
            if let Ok(m) = kernel.tessellate(*s, (size * 2e-4).max(1e-3)) {
                mesh.faces.extend(m.faces);
                mesh.edges.extend(m.edges);
            }
        }
        mesh
    };
    let error = |(sa, sb, _): &(f64, f64, f64)| {
        (sa - volume_a).abs() / volume_a.max(1e-9) + (sb - volume_b).abs() / volume_b.max(1e-9)
    };
    let estimates = sampled_volumes(&mesh_of(a), &mesh_of(b), 240);
    let (sa, sb, sc) = estimates
        .iter()
        .min_by(|x, y| error(x).total_cmp(&error(y)))
        .copied()?;
    (sa > 0.0 && sb > 0.0).then_some((sa, sb, sc))
}

/// [`compare`] without booleans: the meshes sampled only. For shapes OpenCASCADE's booleans
/// cannot handle in reasonable time (lettering surfaces against simplified ones).
pub(crate) fn compare_sampled(
    kernel: &OcctKernel,
    a: &[ShapeHandle],
    b: &[ShapeHandle],
) -> Comparison {
    let volume = |shapes: &[ShapeHandle]| -> f64 {
        shapes.iter().filter_map(|s| kernel.volume(*s).ok()).sum()
    };
    let (volume_a, volume_b) = (volume(a), volume(b));
    let sampled = sampled(kernel, a, b, volume_a, volume_b);
    // The common volume as a share of each side, averaged.
    let common = sampled.map_or(0.0, |(sa, sb, sc)| {
        (volume_a * sc / sa + volume_b * sc / sb) / 2.0
    });
    Comparison {
        volume_a,
        volume_b,
        common,
        extra: Vec::new(),
        missing: Vec::new(),
        nudged: None,
        method: "sampled meshes",
        sampled,
    }
}

// ---- Comparison by sampling ------------------------------------------------------------------

/// Where the line `(pu, pv)` along `axis` (the other two coordinates being `u` and `v`)
/// crosses triangle `a b c`, and whether it enters the solid there (+1, the triangle facing
/// against the axis; triangles are counter-clockwise seen from outside) or leaves it (-1).
#[allow(clippy::too_many_arguments)]
fn crossing(
    a: [f64; 3],
    b: [f64; 3],
    c: [f64; 3],
    axis: usize,
    u: usize,
    v: usize,
    pu: f64,
    pv: f64,
) -> Option<(f64, i32)> {
    let at = |p: [f64; 3], k: usize| p.get(k).copied().unwrap_or(0.0);
    let d = (at(b, u) - at(a, u)) * (at(c, v) - at(a, v))
        - (at(c, u) - at(a, u)) * (at(b, v) - at(a, v));
    if d.abs() < 1e-18 {
        return None;
    }
    let l1 =
        ((pu - at(a, u)) * (at(c, v) - at(a, v)) - (at(c, u) - at(a, u)) * (pv - at(a, v))) / d;
    let l2 =
        ((at(b, u) - at(a, u)) * (pv - at(a, v)) - (pu - at(a, u)) * (at(b, v) - at(a, v))) / d;
    if l1 < 0.0 || l2 < 0.0 || l1 + l2 > 1.0 {
        return None;
    }
    let t = at(a, axis) + l1 * (at(b, axis) - at(a, axis)) + l2 * (at(c, axis) - at(a, axis));
    // The sign of the triangle's normal along the axis is the sign of `d` (u, v, axis
    // being a right-handed cycle): facing against the axis, the line enters.
    Some((t, if d < 0.0 { 1 } else { -1 }))
}

/// The stretches where the winding number of a line's crossings is positive: inside the
/// solid. Unlike pairing crossings two by two, this stays right where solids touch, where a
/// solid has faces inside it, and where the line meets an edge shared by two triangles.
fn material(mut hits: Vec<(f64, i32)>) -> Vec<(f64, f64)> {
    hits.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut out = Vec::new();
    let mut winding = 0;
    let mut start = 0.0;
    let mut k = 0;
    while k < hits.len() {
        // Crossings at the same place count together.
        let t = hits.get(k).map_or(0.0, |h| h.0);
        let mut change = 0;
        while let Some(h) = hits.get(k) {
            if (h.0 - t).abs() > 1e-9 {
                break;
            }
            change += h.1;
            k += 1;
        }
        let before = winding;
        winding += change;
        if before <= 0 && winding > 0 {
            start = t;
        } else if before > 0 && winding <= 0 && t > start {
            out.push((start, t));
        }
    }
    out
}

/// Where a line through `at` along model axis `axis` (0 x, 1 y, 2 z) is inside a closed
/// mesh, in order along the axis.
pub(crate) fn probe(mesh: &Mesh, at: Point3, axis: usize) -> Vec<(f64, f64)> {
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let p = [at.x, at.y, at.z];
    let (pu, pv) = (
        p.get(u).copied().unwrap_or(0.0),
        p.get(v).copied().unwrap_or(0.0),
    );
    let mut hits = Vec::new();
    for face in &mesh.faces {
        for t in &face.triangles {
            let corner = |i: u32| {
                face.positions
                    .get(i as usize)
                    .map(|q| [f64::from(q[0]), f64::from(q[1]), f64::from(q[2])])
            };
            let (Some(a), Some(b), Some(c)) = (corner(t[0]), corner(t[1]), corner(t[2])) else {
                continue;
            };
            hits.extend(crossing(a, b, c, axis, u, v, pu, pv));
        }
    }
    material(hits)
}

/// Volumes of two closed meshes and of what they share, measured without booleans: rays
/// cast along a model axis through a `cells` x `cells` grid over both, the stretches of
/// each ray inside each mesh (between successive crossings) summed, and their overlaps;
/// one estimate per axis (X, Y, Z), the best being along the parts' thinnest direction.
/// Exact along the rays; across them, within a cell of the outline. For when
/// OpenCASCADE's booleans fail on the pair (faces that coincide exactly, round ones
/// especially).
pub(crate) fn sampled_volumes(a: &Mesh, b: &Mesh, cells: usize) -> [(f64, f64, f64); 3] {
    let triangles = |mesh: &Mesh| -> Vec<[[f64; 3]; 3]> {
        mesh.faces
            .iter()
            .flat_map(|f| {
                f.triangles.iter().filter_map(|t| {
                    let p = |i: u32| {
                        f.positions
                            .get(i as usize)
                            .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
                    };
                    Some([p(t[0])?, p(t[1])?, p(t[2])?])
                })
            })
            .collect()
    };
    let (ta, tb) = (triangles(a), triangles(b));
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in ta.iter().chain(&tb).flatten() {
        for ((l, h), x) in lo.iter_mut().zip(hi.iter_mut()).zip(p) {
            *l = l.min(*x);
            *h = h.max(*x);
        }
    }
    let mut out = [(0.0, 0.0, 0.0); 3];
    if !(lo.iter().all(|x| x.is_finite()) && hi.iter().all(|x| x.is_finite())) {
        return out;
    }
    let at = |p: &[f64; 3], k: usize| p.get(k).copied().unwrap_or(0.0);
    for (axis, estimate) in out.iter_mut().enumerate() {
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        let (lu, lv) = (at(&lo, u), at(&lo, v));
        let (su, sv) = ((at(&hi, u) - lu).max(1e-9), (at(&hi, v) - lv).max(1e-9));
        let (du, dv) = (su / cells as f64, sv / cells as f64);
        // Each triangle listed in the cells its shadow covers.
        let bin = |tris: &[[[f64; 3]; 3]]| -> Vec<Vec<usize>> {
            let mut grid = vec![Vec::new(); cells * cells];
            for (index, t) in tris.iter().enumerate() {
                let (mut u0, mut u1, mut v0, mut v1) = (
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                );
                for p in t {
                    u0 = u0.min(at(p, u));
                    u1 = u1.max(at(p, u));
                    v0 = v0.min(at(p, v));
                    v1 = v1.max(at(p, v));
                }
                let cell = |x: f64, origin: f64, step: f64| -> usize {
                    (((x - origin) / step).floor().max(0.0) as usize).min(cells - 1)
                };
                for i in cell(u0, lu, du)..=cell(u1, lu, du) {
                    for j in cell(v0, lv, dv)..=cell(v1, lv, dv) {
                        if let Some(list) = grid.get_mut(i * cells + j) {
                            list.push(index);
                        }
                    }
                }
            }
            grid
        };
        let (ga, gb) = (bin(&ta), bin(&tb));
        // Where a ray at (pu, pv) crosses the triangles of one cell, sorted.
        let inside = |tris: &[[[f64; 3]; 3]], list: &[usize], pu: f64, pv: f64| {
            material(
                list.iter()
                    .filter_map(|&index| {
                        let t = tris.get(index)?;
                        crossing(t[0], t[1], t[2], axis, u, v, pu, pv)
                    })
                    .collect(),
            )
        };
        let (mut va, mut vb, mut vc) = (0.0, 0.0, 0.0);
        for i in 0..cells {
            for j in 0..cells {
                // Off the cell's centre by an odd fraction, so rays miss mesh vertices.
                let pu = lu + (i as f64 + 0.5 + 0.0137) * du;
                let pv = lv + (j as f64 + 0.5 - 0.0071) * dv;
                let empty = Vec::new();
                let la = ga.get(i * cells + j).unwrap_or(&empty);
                let lb = gb.get(i * cells + j).unwrap_or(&empty);
                let ia = inside(&ta, la, pu, pv);
                let ib = inside(&tb, lb, pu, pv);
                va += ia.iter().map(|(s, e)| e - s).sum::<f64>();
                vb += ib.iter().map(|(s, e)| e - s).sum::<f64>();
                for (s0, e0) in &ia {
                    for (s1, e1) in &ib {
                        vc += (e0.min(*e1) - s0.max(*s1)).max(0.0);
                    }
                }
            }
        }
        let area = du * dv;
        *estimate = (va * area, vb * area, vc * area);
    }
    out
}

// ---- Measure -----------------------------------------------------------------------------------

/// The shortest distance between two picked faces or edges of the same solids.
pub(crate) fn distance(
    kernel: &OcctKernel,
    solids: &Solids<'_>,
    a: (Kind, u32),
    b: (Kind, u32),
) -> Result<Value> {
    let part = |(kind, i): (Kind, u32)| -> Result<(ShapeHandle, SubShape)> {
        let range: &SolidRange = match kind {
            Kind::Face => solids.solid_of_face(i),
            Kind::Edge => solids.solid_of_edge(i),
        }
        .ok_or("nothing there")?;
        Ok(match kind {
            Kind::Face => (range.shape, SubShape::Face(i - range.faces.start)),
            Kind::Edge => (range.shape, SubShape::Edge(i - range.edges.start)),
        })
    };
    let (sa, pa) = part(a)?;
    let (sb, pb) = part(b)?;
    let d = kernel
        .distance(sa, pa, sb, pb)
        .map_err(|e| e.to_string())?
        .ok_or("the kernel cannot measure distances")?;
    let delta = d.on_b - d.on_a;
    Ok(json!({
        "distance": round(d.distance),
        "on_a": args::round3(d.on_a),
        "on_b": args::round3(d.on_b),
        "delta": args::round_vec(delta),
    }))
}

/// The topology counts of a merged topology, for summaries.
pub(crate) fn counts(topology: &Topology) -> Value {
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &topology.faces {
        *kinds.entry(surface_name(f.surface)).or_default() += 1;
    }
    json!({ "faces": topology.faces.len(), "edges": topology.edges.len(),
        "vertices": topology.vertices.len(), "face_kinds": kinds })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Seg> {
        let p = |x, y| Point2::new(x, y);
        vec![
            Seg::Line(p(x0, y0), p(x1, y0)),
            Seg::Line(p(x1, y0), p(x1, y1)),
            Seg::Line(p(x1, y1), p(x0, y1)),
            Seg::Line(p(x0, y1), p(x0, y0)),
        ]
    }

    #[test]
    fn loops_are_chained_nested_and_recognised() {
        let mut segments = rect(0.0, 0.0, 100.0, 50.0);
        // Shuffled and one reversed: chaining must not care.
        segments.swap(0, 2);
        if let Some(Seg::Line(a, b)) = segments.get(1).cloned() {
            segments[1] = Seg::Line(b, a);
        }
        segments.push(Seg::Circle(Point2::new(20.0, 25.0), 2.0));
        segments.push(Seg::Circle(Point2::new(40.0, 25.0), 2.0));
        segments.extend(rect(60.0, 10.0, 80.0, 20.0));
        let loops = chain_loops(segments);
        assert_eq!(loops.len(), 4);
        assert_eq!(loops[0].depth, 0);
        assert!(loops[1..].iter().all(|l| l.depth == 1));
        assert_eq!(
            recognise(&loops[0].segments),
            Shape::Rect {
                center: Point2::new(50.0, 25.0),
                w: 100.0,
                h: 50.0,
                radius: 0.0
            }
        );
        let report = section_json(&Section { loops }, false);
        let shapes = report["shapes"].as_array().unwrap();
        let circles = shapes
            .iter()
            .find(|s| s["shape"] == "circle diameter 4")
            .unwrap();
        assert_eq!(circles["count"], 2);
        assert_eq!(circles["grid"]["x_spacing"], 20.0);
        assert!((report["area"].as_f64().unwrap() - (5000.0 - 200.0 - 8.0 * PI)).abs() < 0.2);
    }

    /// A box as a closed mesh, one face per side.
    fn box_mesh(lo: [f32; 3], hi: [f32; 3]) -> Mesh {
        let corner = |k: usize| {
            [
                if k & 1 == 0 { lo[0] } else { hi[0] },
                if k & 2 == 0 { lo[1] } else { hi[1] },
                if k & 4 == 0 { lo[2] } else { hi[2] },
            ]
        };
        let quads = [
            [0, 2, 3, 1],
            [4, 5, 7, 6],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 4, 6, 2],
            [1, 3, 7, 5],
        ];
        Mesh {
            faces: quads
                .iter()
                .map(|q| oxyde_kernel::FaceMesh {
                    positions: q.iter().map(|&k| corner(k)).collect(),
                    triangles: vec![[0, 1, 2], [0, 2, 3]],
                })
                .collect(),
            edges: Vec::new(),
        }
    }

    #[test]
    fn material_follows_the_winding_number() {
        // Two solids touching at 1: one stretch, not two.
        assert_eq!(
            material(vec![(0.0, 1), (1.0, -1), (1.0, 1), (2.0, -1)]),
            [(0.0, 2.0)]
        );
        // A line through an edge shared by two triangles meets both.
        assert_eq!(
            material(vec![(0.0, 1), (0.0, 1), (3.0, -1), (3.0, -1)]),
            [(0.0, 3.0)]
        );
        // Overlapping solids count once; a hole between two parts stays a hole.
        assert_eq!(
            material(vec![(0.0, 1), (2.0, -1), (1.0, 1), (3.0, -1)]),
            [(0.0, 3.0)]
        );
        assert_eq!(
            material(vec![(0.0, 1), (1.0, -1), (2.0, 1), (3.0, -1)]),
            [(0.0, 1.0), (2.0, 3.0)]
        );
    }

    #[test]
    fn sampling_measures_volumes_and_overlap() {
        let a = box_mesh([0.0, 0.0, 0.0], [10.0, 20.0, 30.0]);
        let b = box_mesh([5.0, 0.0, 0.0], [15.0, 20.0, 30.0]);
        // Along X both boxes fill the grid's cross-section: exact.
        let (va, vb, common) = sampled_volumes(&a, &b, 64)[0];
        assert!((va - 6000.0).abs() < 1e-6, "{va}");
        assert!((vb - 6000.0).abs() < 1e-6, "{vb}");
        assert!((common - 3000.0).abs() < 1e-6, "{common}");
        // Along Z the outline of each box falls inside cells: within a cell's width.
        let (va, _, common) = sampled_volumes(&a, &b, 64)[2];
        let cell = 15.0 / 64.0 * 20.0 * 30.0;
        assert!((va - 6000.0).abs() < cell, "{va}");
        assert!((common - 3000.0).abs() < cell, "{common}");
    }

    #[test]
    fn slots_and_polygons_are_recognised() {
        let p = |x, y| Point2::new(x, y);
        let slot = vec![
            Seg::Line(p(0.0, -3.0), p(40.0, -3.0)),
            Seg::Arc {
                center: p(40.0, 0.0),
                radius: 3.0,
                from: p(40.0, -3.0),
                to: p(40.0, 3.0),
                ccw: true,
            },
            Seg::Line(p(40.0, 3.0), p(0.0, 3.0)),
            Seg::Arc {
                center: p(0.0, 0.0),
                radius: 3.0,
                from: p(0.0, 3.0),
                to: p(0.0, -3.0),
                ccw: true,
            },
        ];
        assert_eq!(
            recognise(&slot),
            Shape::Slot {
                start: p(40.0, 0.0),
                end: p(0.0, 0.0),
                width: 6.0
            }
        );
        let hex: Vec<Seg> = (0..6)
            .map(|k| {
                let t = |k: i32| PI / 6.0 + TAU * f64::from(k) / 6.0;
                let q = |k| p(10.0 * t(k).cos(), 10.0 * t(k).sin());
                Seg::Line(q(k), q(k + 1))
            })
            .collect();
        match recognise(&hex) {
            Shape::Polygon {
                sides,
                radius,
                angle,
                ..
            } => {
                assert_eq!(sides, 6);
                assert!((radius - 10.0).abs() < 1e-9);
                assert!((angle - PI / 6.0).abs() < 1e-9);
            }
            other => panic!("{other:?}"),
        }
    }
}
