//! A planar arrangement of straight segments and circular arcs: the segments split at every
//! point where they cross, touch or overlap, the points welded within a tolerance, and the
//! regions they enclose traced out with their holes.
//!
//! The booleans cut each face along the curves it shares with the other solid in the face's
//! own coordinates (a plane's, or a cylinder's angle and height, where the curves are
//! straight) and keep or drop the regions this gives.
//!
//! Regions are traced through the half-edges round each point, ordered counter-clockwise by
//! the direction they leave in, an arc that leaves tangent to another edge after it when it
//! turns more to the left; a region lies on the left of its half-edges, so its outer loop
//! runs counter-clockwise and its holes clockwise.

// Indices into the points and edges made here.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;
use std::f64::consts::{PI, TAU};

/// A point of the plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct P {
    pub x: f64,
    pub y: f64,
}

impl P {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn sub(self, o: P) -> P {
        P::new(self.x - o.x, self.y - o.y)
    }

    fn add(self, o: P) -> P {
        P::new(self.x + o.x, self.y + o.y)
    }

    fn scale(self, k: f64) -> P {
        P::new(self.x * k, self.y * k)
    }

    fn dot(self, o: P) -> f64 {
        self.x * o.x + self.y * o.y
    }

    fn cross(self, o: P) -> f64 {
        self.x * o.y - self.y * o.x
    }

    fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    pub fn distance(self, o: P) -> f64 {
        self.sub(o).length()
    }

    fn angle(self) -> f64 {
        self.y.atan2(self.x)
    }
}

/// What a segment runs along between its two ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Geom {
    Line,
    /// The circle centred on `center` of `radius`, turning counter-clockwise from the
    /// segment's start to its end when `ccw`. From a point back to itself: a whole circle.
    Arc {
        center: P,
        radius: f64,
        ccw: bool,
    },
}

/// What a straight piece of a curve laid out as points stands for: the curve, and its
/// parameters (an angle about the curve's axis) at the piece's start and end. The booleans
/// lay out curves no chart makes straight or round this way, and put the pieces of one back
/// together into the exact curve once the arrangement is traced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Tag {
    pub curve: crate::geometry::Curve,
    pub t0: f64,
    pub t1: f64,
}

/// A segment from `a` to `b`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Segment {
    pub a: P,
    pub b: P,
    pub geom: Geom,
    /// The curve a straight piece stands for, if it does.
    pub tag: Option<Tag>,
}

impl Segment {
    pub fn line(a: P, b: P) -> Self {
        Self {
            a,
            b,
            geom: Geom::Line,
            tag: None,
        }
    }

    /// A straight piece of a curve laid out as points.
    pub fn tagged(a: P, b: P, tag: Tag) -> Self {
        Self {
            a,
            b,
            geom: Geom::Line,
            tag: Some(tag),
        }
    }

    pub fn arc(a: P, b: P, center: P, ccw: bool) -> Self {
        Self {
            a,
            b,
            geom: Geom::Arc {
                center,
                radius: a.distance(center),
                ccw,
            },
            tag: None,
        }
    }

    /// The tag of the stretch from `u0` to `u1` along it (as [`Segment::extent`] measures).
    fn tag_between(&self, u0: f64, u1: f64, tol: f64) -> Option<Tag> {
        let tag = self.tag?;
        let extent = self.extent(tol);
        if extent <= 0.0 {
            return Some(tag);
        }
        let at = |u: f64| tag.t0 + (tag.t1 - tag.t0) * (u / extent);
        Some(Tag {
            t0: at(u0),
            t1: at(u1),
            ..tag
        })
    }

    /// How far it runs: its length for a line, the angle it turns through for an arc
    /// (`(0, 2π]`, a whole turn from a point back to itself within `tol`).
    pub fn extent(&self, tol: f64) -> f64 {
        match self.geom {
            Geom::Line => self.a.distance(self.b),
            Geom::Arc { center, ccw, .. } => {
                if self.a.distance(self.b) <= tol {
                    return TAU;
                }
                let (a0, a1) = (self.a.sub(center).angle(), self.b.sub(center).angle());
                let t = if ccw { a1 - a0 } else { a0 - a1 }.rem_euclid(TAU);
                if t <= 0.0 { TAU } else { t }
            }
        }
    }

    /// The point `s` along it, `s` in `[0, extent]`.
    pub fn at(&self, s: f64, tol: f64) -> P {
        match self.geom {
            Geom::Line => {
                let length = self.extent(tol);
                if length <= 0.0 {
                    self.a
                } else {
                    self.a.add(self.b.sub(self.a).scale(s / length))
                }
            }
            Geom::Arc {
                center,
                radius,
                ccw,
            } => {
                let start = self.a.sub(center).angle();
                let t = if ccw { start + s } else { start - s };
                center.add(P::new(t.cos(), t.sin()).scale(radius))
            }
        }
    }

    /// Where `p`, a point on it or near it, is along it: `None` when off it by more than
    /// `tol` or beyond its ends.
    fn locate(&self, p: P, tol: f64) -> Option<f64> {
        let extent = self.extent(tol);
        let s = match self.geom {
            Geom::Line => {
                let d = self.b.sub(self.a);
                let length = d.length();
                if length <= 0.0 {
                    return None;
                }
                let s = p.sub(self.a).dot(d) / length;
                let off = p.sub(self.a).cross(d).abs() / length;
                if off > tol {
                    return None;
                }
                s
            }
            Geom::Arc {
                center,
                radius,
                ccw,
            } => {
                let r = p.distance(center);
                if (r - radius).abs() > tol {
                    return None;
                }
                let (a0, t) = (self.a.sub(center).angle(), p.sub(center).angle());
                let mut s = if ccw { t - a0 } else { a0 - t }.rem_euclid(TAU);
                // Just before the start, an arc's angle wraps round: take it as the start.
                if s * radius > extent * radius + tol && (TAU - s) * radius <= tol {
                    s -= TAU;
                }
                s * radius
            }
        };
        // `s` is a length for a line and an arc length for an arc, to compare with `tol`.
        let full = match self.geom {
            Geom::Line => extent,
            Geom::Arc { radius, .. } => extent * radius,
        };
        if s < -tol || s > full + tol {
            return None;
        }
        Some(match self.geom {
            Geom::Line => s.clamp(0.0, extent),
            Geom::Arc { radius, .. } => (s / radius).clamp(0.0, extent),
        })
    }

    /// The direction it leaves `a` in (unit), and how much it turns left there (its signed
    /// curvature).
    fn leaving(&self) -> (P, f64) {
        match self.geom {
            Geom::Line => {
                let d = self.b.sub(self.a);
                let l = d.length().max(f64::MIN_POSITIVE);
                (d.scale(1.0 / l), 0.0)
            }
            Geom::Arc {
                center,
                radius,
                ccw,
            } => {
                let r = self
                    .a
                    .sub(center)
                    .scale(1.0 / radius.max(f64::MIN_POSITIVE));
                if ccw {
                    (P::new(-r.y, r.x), 1.0 / radius)
                } else {
                    (P::new(r.y, -r.x), -1.0 / radius)
                }
            }
        }
    }

    /// A box round it, `pad` wider all round: a line's ends, an arc's whole circle.
    fn bounds(&self, pad: f64) -> (P, P) {
        let (lo, hi) = match self.geom {
            Geom::Line => (
                P::new(self.a.x.min(self.b.x), self.a.y.min(self.b.y)),
                P::new(self.a.x.max(self.b.x), self.a.y.max(self.b.y)),
            ),
            Geom::Arc { center, radius, .. } => (
                P::new(center.x - radius, center.y - radius),
                P::new(center.x + radius, center.y + radius),
            ),
        };
        (
            P::new(lo.x - pad, lo.y - pad),
            P::new(hi.x + pad, hi.y + pad),
        )
    }

    /// The same segment run the other way.
    pub fn reversed(&self) -> Segment {
        Segment {
            tag: self.tag.map(|t| Tag {
                t0: t.t1,
                t1: t.t0,
                ..t
            }),
            a: self.b,
            b: self.a,
            geom: match self.geom {
                Geom::Line => Geom::Line,
                Geom::Arc {
                    center,
                    radius,
                    ccw,
                } => Geom::Arc {
                    center,
                    radius,
                    ccw: !ccw,
                },
            },
        }
    }

    /// Twice the signed area it sweeps about the origin (`∮ x dy − y dx`).
    pub fn twice_area(&self, tol: f64) -> f64 {
        match self.geom {
            Geom::Line => self.a.cross(self.b),
            Geom::Arc {
                center,
                radius,
                ccw,
            } => {
                let t0 = self.a.sub(center).angle();
                let sweep = self.extent(tol);
                let t1 = if ccw { t0 + sweep } else { t0 - sweep };
                radius * center.x * (t1.sin() - t0.sin())
                    - radius * center.y * (t1.cos() - t0.cos())
                    + radius * radius * (t1 - t0)
            }
        }
    }

    /// Points along it, its start included and its end left out: chords no longer than a
    /// 64th of a turn for an arc.
    fn polyline(&self, tol: f64) -> Vec<P> {
        match self.geom {
            Geom::Line => vec![self.a],
            Geom::Arc { .. } => {
                let extent = self.extent(tol);
                let n = ((extent / (TAU / 64.0)).ceil() as usize).max(2);
                (0..n)
                    .map(|i| self.at(extent * i as f64 / n as f64, tol))
                    .collect()
            }
        }
    }

    /// The points where it meets the horizontal line at height `y`, crossing it (a point it
    /// only grazes counts too: the callers keep away from those).
    pub fn crossings(&self, y: f64, tol: f64, out: &mut Vec<f64>) {
        match self.geom {
            Geom::Line => {
                let (a, b) = (self.a, self.b);
                // Half-open, so that a point shared by two edges counts once.
                if (a.y <= y && y < b.y) || (b.y <= y && y < a.y) {
                    out.push(a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x));
                }
            }
            Geom::Arc { center, radius, .. } => {
                let dy = y - center.y;
                if dy.abs() >= radius {
                    return;
                }
                let dx = (radius * radius - dy * dy).sqrt();
                for x in [center.x - dx, center.x + dx] {
                    let p = P::new(x, y);
                    if self.locate(p, tol).is_none() {
                        continue;
                    }
                    // An end on the line counts as a line's lower end would (half-open): when
                    // the arc leaves it upwards. A whole circle has no end: it crosses there.
                    let closed = self.a.distance(self.b) <= tol;
                    let leaving_up = if closed {
                        None
                    } else if p.distance(self.a) <= tol {
                        Some(self.leaving().0.y > 0.0)
                    } else if p.distance(self.b) <= tol {
                        Some(self.reversed().leaving().0.y > 0.0)
                    } else {
                        None
                    };
                    if leaving_up.unwrap_or(true) {
                        out.push(x);
                    }
                }
            }
        }
    }
}

impl Segment {
    /// How far `p` is from it.
    pub fn distance(&self, p: P, tol: f64) -> f64 {
        match self.geom {
            Geom::Line => {
                let d = self.b.sub(self.a);
                let l2 = d.dot(d);
                if l2 <= 0.0 {
                    return p.distance(self.a);
                }
                let t = (p.sub(self.a).dot(d) / l2).clamp(0.0, 1.0);
                p.distance(self.a.add(d.scale(t)))
            }
            Geom::Arc { center, radius, .. } => {
                let on = center.add(
                    p.sub(center)
                        .scale(radius / p.distance(center).max(f64::MIN_POSITIVE)),
                );
                if self.locate(on, tol).is_some() {
                    p.distance(on)
                } else {
                    p.distance(self.a).min(p.distance(self.b))
                }
            }
        }
    }
}

/// Whether `p` is inside the region the closed loops of `segments` bound (an odd number of
/// them crossed going right from it).
pub(crate) fn inside(segments: &[Segment], p: P, tol: f64) -> bool {
    let mut xs = Vec::new();
    for s in segments {
        s.crossings(p.y, tol, &mut xs);
    }
    !xs.iter().filter(|&&x| x > p.x).count().is_multiple_of(2)
}

/// How far `p` is from the nearest of `segments`.
pub(crate) fn distance_to(segments: &[Segment], p: P, tol: f64) -> f64 {
    segments
        .iter()
        .map(|s| s.distance(p, tol))
        .fold(f64::INFINITY, f64::min)
}

/// How a point where two segments meet was found: where they cross (worked out from both),
/// or as an end of the first or of the second lying on the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Met {
    Crossing,
    EndOfFirst,
    EndOfSecond,
}

/// The points where two segments meet (crossing, touching, or the ends of an overlap).
fn meet(s: &Segment, t: &Segment, tol: f64) -> Vec<(P, Met)> {
    let mut out: Vec<(P, Met)> = Vec::new();
    match (s.geom, t.geom) {
        (Geom::Line, Geom::Line) => {
            let (d1, d2) = (s.b.sub(s.a), t.b.sub(t.a));
            let denom = d1.cross(d2);
            let (l1, l2) = (d1.length(), d2.length());
            // Parallel: where they overlap, the ends of each on the other (below).
            if denom.abs() > 1e-12 * l1 * l2 {
                let u = t.a.sub(s.a).cross(d2) / denom;
                out.push((s.a.add(d1.scale(u)), Met::Crossing));
            }
        }
        (Geom::Line, Geom::Arc { center, radius, .. })
        | (Geom::Arc { center, radius, .. }, Geom::Line) => {
            let line = if s.geom == Geom::Line { s } else { t };
            let d = line.b.sub(line.a);
            let l = d.length();
            if l > 0.0 {
                let u = d.scale(1.0 / l);
                let along = center.sub(line.a).dot(u);
                let foot = line.a.add(u.scale(along));
                let off = center.sub(foot).length();
                if off <= radius + tol {
                    let half = (radius * radius - off * off).max(0.0).sqrt();
                    out.push((foot.add(u.scale(half)), Met::Crossing));
                    out.push((foot.sub(u.scale(half)), Met::Crossing));
                }
            }
        }
        (
            Geom::Arc {
                center: c1,
                radius: r1,
                ..
            },
            Geom::Arc {
                center: c2,
                radius: r2,
                ..
            },
        ) => {
            let d = c2.sub(c1);
            let dist = d.length();
            if dist <= tol && (r1 - r2).abs() <= tol {
                // The same circle: where they overlap, the ends of each on the other.
            } else if dist > tol && dist <= r1 + r2 + tol && dist >= (r1 - r2).abs() - tol {
                let a = (r1 * r1 - r2 * r2 + dist * dist) / (2.0 * dist);
                let h = (r1 * r1 - a * a).max(0.0).sqrt();
                let u = d.scale(1.0 / dist);
                let mid = c1.add(u.scale(a));
                let n = P::new(-u.y, u.x);
                out.push((mid.add(n.scale(h)), Met::Crossing));
                out.push((mid.sub(n.scale(h)), Met::Crossing));
            }
        }
    }
    // The ends of each that lie on the other: touching, and overlaps.
    for p in [s.a, s.b] {
        if t.locate(p, tol).is_some() {
            out.push((p, Met::EndOfFirst));
        }
    }
    for p in [t.a, t.b] {
        if s.locate(p, tol).is_some() {
            out.push((p, Met::EndOfSecond));
        }
    }
    out.retain(|&(p, _)| s.locate(p, tol).is_some() && t.locate(p, tol).is_some());
    out
}

/// One side of an edge: `edge` run its own way, or `reversed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Half {
    pub edge: usize,
    pub reversed: bool,
}

/// An edge of the arrangement, between two of its points.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ArrEdge {
    pub from: usize,
    pub to: usize,
    pub segment: Segment,
}

/// A region the edges enclose: its outer loop, counter-clockwise, and its holes, clockwise.
#[derive(Debug, Clone)]
pub(crate) struct Region {
    pub outer: Vec<Half>,
    pub holes: Vec<Vec<Half>>,
}

/// The segments split at every point they meet, their points welded.
#[derive(Debug, Clone, Default)]
pub(crate) struct Arrangement {
    pub points: Vec<P>,
    pub edges: Vec<ArrEdge>,
    tol: f64,
    /// The points by the square of side `tol` they lie in, to weld a new one quickly.
    grid: HashMap<(i64, i64), Vec<usize>>,
    /// The edges by their two ends, the smaller first, to find one already there quickly.
    by_ends: HashMap<(usize, usize), Vec<usize>>,
}

impl Arrangement {
    /// The arrangement of `segments`, points closer than `tol` taken as one.
    pub fn new(segments: &[Segment], tol: f64) -> Arrangement {
        let mut arrangement = Arrangement {
            points: Vec::new(),
            edges: Vec::new(),
            tol,
            grid: HashMap::new(),
            by_ends: HashMap::new(),
        };
        // Every segment's split points, each with how sure it is: its own ends (0), another
        // segment's end lying on it (1), and where it crosses another, worked out from both (2).
        let mut cuts: Vec<Vec<(f64, u8)>> = segments
            .iter()
            .map(|s| vec![(0.0, 0), (s.extent(tol), 0)])
            .collect();
        // Only segments whose boxes overlap can meet: swept along x, in the order of their
        // left ends.
        let boxes: Vec<(P, P)> = segments.iter().map(|s| s.bounds(2.0 * tol)).collect();
        let mut order: Vec<usize> = (0..segments.len()).collect();
        order.sort_by(|&a, &b| boxes[a].0.x.total_cmp(&boxes[b].0.x).then(a.cmp(&b)));
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        for (k, &i) in order.iter().enumerate() {
            for &j in &order[k + 1..] {
                if boxes[j].0.x > boxes[i].1.x {
                    break;
                }
                if boxes[j].0.y <= boxes[i].1.y && boxes[i].0.y <= boxes[j].1.y {
                    pairs.push((i.min(j), i.max(j)));
                }
            }
        }
        pairs.sort_unstable();
        for (i, j) in pairs {
            {
                for (p, met) in meet(&segments[i], &segments[j], tol) {
                    let (on_i, on_j) = match met {
                        Met::Crossing => (2, 2),
                        Met::EndOfFirst => (0, 1),
                        Met::EndOfSecond => (1, 0),
                    };
                    if let Some(u) = segments[i].locate(p, tol) {
                        cuts[i].push((u, on_i));
                    }
                    if let Some(u) = segments[j].locate(p, tol) {
                        cuts[j].push((u, on_j));
                    }
                }
            }
        }
        for (segment, mut at) in segments.iter().zip(cuts) {
            at.sort_by(|a, b| a.0.total_cmp(&b.0));
            let extent = segment.extent(tol);
            let closed =
                matches!(segment.geom, Geom::Arc { .. }) && segment.a.distance(segment.b) <= tol;
            // Split points within the tolerance of each other are one, standing at the surest
            // of them: a segment running a hair past where it meets another ends at the
            // crossing, and the stub beyond is dropped; an end of another segment a hair past a
            // crossing does not move it.
            let pick = |cluster: &[(f64, u8)]| -> f64 {
                let best = cluster.iter().map(|c| c.1).max().unwrap_or(0);
                cluster.iter().find(|c| c.1 == best).map_or(0.0, |c| c.0)
            };
            let mut stops: Vec<f64> = Vec::new();
            let mut cluster: Vec<(f64, u8)> = vec![at[0]];
            for &c in &at[1..] {
                let last = cluster[cluster.len() - 1].0;
                let near = segment.at(last, tol).distance(segment.at(c.0, tol)) <= 2.0 * tol
                    && !(closed && c.0 - cluster[0].0 > PI);
                if near {
                    cluster.push(c);
                } else {
                    stops.push(pick(&cluster));
                    cluster = vec![c];
                }
            }
            stops.push(pick(&cluster));
            let pieces: Vec<(f64, f64)> = stops.windows(2).map(|w| (w[0], w[1])).collect();
            for (u0, u1) in pieces {
                let a = if u0 <= 0.0 {
                    segment.a
                } else {
                    segment.at(u0, tol)
                };
                let b = if u1 >= extent {
                    segment.b
                } else {
                    segment.at(u1, tol)
                };
                let from = arrangement.point(a);
                let to = arrangement.point(b);
                let piece = Segment {
                    a: arrangement.points[from],
                    b: arrangement.points[to],
                    geom: segment.geom,
                    tag: segment.tag_between(u0.max(0.0), u1.min(extent), tol),
                };
                if from == to && !closed {
                    continue;
                }
                arrangement.add_edge(from, to, piece);
            }
        }
        arrangement.prune();
        // Pruning renumbered the edges; the map of them is not needed any more.
        arrangement.by_ends.clear();
        arrangement
    }

    /// The index of the point at `p`, welding it to one within the tolerance.
    fn point(&mut self, p: P) -> usize {
        let cell = |p: P| {
            (
                (p.x / self.tol).floor() as i64,
                (p.y / self.tol).floor() as i64,
            )
        };
        let (cx, cy) = cell(p);
        // The first point made within the tolerance, as a search in order would find.
        let mut found: Option<usize> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &i in self.grid.get(&(cx + dx, cy + dy)).into_iter().flatten() {
                    if self.points[i].distance(p) <= self.tol && found.is_none_or(|f| i < f) {
                        found = Some(i);
                    }
                }
            }
        }
        if let Some(i) = found {
            return i;
        }
        self.points.push(p);
        let index = self.points.len() - 1;
        self.grid.entry((cx, cy)).or_default().push(index);
        index
    }

    /// Adds an edge unless the same one, either way round, is there already.
    fn add_edge(&mut self, from: usize, to: usize, segment: Segment) {
        let mid = segment.at(segment.extent(self.tol) / 2.0, self.tol);
        let key = (from.min(to), from.max(to));
        let same = self.by_ends.get(&key).into_iter().flatten().any(|&e| {
            let e = &self.edges[e];
            let m = e.segment.at(e.segment.extent(self.tol) / 2.0, self.tol);
            m.distance(mid) <= self.tol
        });
        if !same {
            self.edges.push(ArrEdge { from, to, segment });
            self.by_ends
                .entry(key)
                .or_default()
                .push(self.edges.len() - 1);
        }
    }

    /// Edges with an end no other edge reaches: what is left of a curve that only touched
    /// the face, or ran out short of its boundary by rounding. They enclose nothing. Peeled
    /// from their loose ends inwards, each point's count of edges kept up to date.
    fn prune(&mut self) {
        let mut degree = vec![0usize; self.points.len()];
        let mut at: Vec<Vec<usize>> = vec![Vec::new(); self.points.len()];
        for (i, e) in self.edges.iter().enumerate() {
            degree[e.from] += 1;
            degree[e.to] += 1;
            if e.from != e.to {
                at[e.from].push(i);
                at[e.to].push(i);
            }
        }
        let mut removed = vec![false; self.edges.len()];
        let mut loose: Vec<usize> = (0..self.points.len()).filter(|&p| degree[p] == 1).collect();
        while let Some(p) = loose.pop() {
            for &i in &at[p] {
                if removed[i] {
                    continue;
                }
                removed[i] = true;
                let e = &self.edges[i];
                for q in [e.from, e.to] {
                    degree[q] -= 1;
                    if degree[q] == 1 {
                        loose.push(q);
                    }
                }
            }
        }
        let mut k = 0;
        self.edges.retain(|_| {
            k += 1;
            !removed[k - 1]
        });
    }

    /// The segment a half-edge runs along, its way.
    pub fn segment(&self, half: Half) -> Segment {
        let s = self.edges[half.edge].segment;
        if half.reversed { s.reversed() } else { s }
    }

    fn head(&self, half: Half) -> usize {
        let e = &self.edges[half.edge];
        if half.reversed { e.from } else { e.to }
    }

    /// The regions the edges enclose, each with its holes.
    pub fn regions(&self) -> Vec<Region> {
        let tol = self.tol;
        // Half-edges leaving each point, counter-clockwise by direction, then curvature.
        let mut around: Vec<Vec<(Half, f64, f64)>> = vec![Vec::new(); self.points.len()];
        for (e, edge) in self.edges.iter().enumerate() {
            for reversed in [false, true] {
                let half = Half { edge: e, reversed };
                let segment = self.segment(half);
                let (d, k) = segment.leaving();
                let tail = if reversed { edge.to } else { edge.from };
                // Within a hair of a whole turn is no turn: a tangent rounded just below the
                // x axis leaves the same way as one along it.
                let mut angle = d.angle().rem_euclid(TAU);
                if angle > TAU - 1e-9 {
                    angle -= TAU;
                }
                around[tail].push((half, angle, k));
            }
        }
        for list in &mut around {
            list.sort_by(|a, b| {
                if (a.1 - b.1).abs() > 1e-9 {
                    a.1.total_cmp(&b.1)
                } else {
                    a.2.total_cmp(&b.2)
                }
            });
        }
        // The next half-edge round a region: at the head, the one just clockwise of the way
        // back.
        let next = |half: Half| -> Option<Half> {
            let head = self.head(half);
            let twin = Half {
                edge: half.edge,
                reversed: !half.reversed,
            };
            let list = &around[head];
            let k = list.iter().position(|h| h.0 == twin)?;
            Some(list[(k + list.len() - 1) % list.len()].0)
        };
        let mut seen = vec![[false; 2]; self.edges.len()];
        let mut cycles: Vec<(Vec<Half>, f64)> = Vec::new();
        for e in 0..self.edges.len() {
            for reversed in [false, true] {
                if seen[e][usize::from(reversed)] {
                    continue;
                }
                let start = Half { edge: e, reversed };
                let mut cycle = Vec::new();
                let mut h = start;
                loop {
                    if seen[h.edge][usize::from(h.reversed)] {
                        break;
                    }
                    seen[h.edge][usize::from(h.reversed)] = true;
                    cycle.push(h);
                    match next(h) {
                        Some(n) => h = n,
                        None => break,
                    }
                    if h == start {
                        break;
                    }
                }
                let area: f64 = cycle
                    .iter()
                    .map(|&h| self.segment(h).twice_area(tol))
                    .sum::<f64>()
                    / 2.0;
                cycles.push((cycle, area));
            }
        }
        // Positive cycles bound regions; a negative one is a hole of the smallest region
        // holding it, or the outside of everything.
        let mut regions: Vec<(Region, f64, Vec<P>)> = cycles
            .iter()
            .filter(|c| c.1 > tol * tol)
            .map(|(cycle, area)| {
                (
                    Region {
                        outer: cycle.clone(),
                        holes: Vec::new(),
                    },
                    *area,
                    self.polygon(cycle),
                )
            })
            .collect();
        let edge_sets: Vec<std::collections::HashSet<usize>> = regions
            .iter()
            .map(|(region, _, _)| region.outer.iter().map(|h| h.edge).collect())
            .collect();
        for (cycle, area) in &cycles {
            if *area > tol * tol {
                continue;
            }
            let Some(&first) = cycle.first() else {
                continue;
            };
            let probe = self.points[self.head(first)];
            let mut best: Option<usize> = None;
            for (r, (_, region_area, polygon)) in regions.iter().enumerate() {
                // Not the region on the other side of the same edges.
                if cycle.iter().any(|c| edge_sets[r].contains(&c.edge)) {
                    continue;
                }
                if winding(polygon, probe) != 0 && best.is_none_or(|b| *region_area < regions[b].1)
                {
                    best = Some(r);
                }
            }
            if let Some(b) = best {
                regions[b].0.holes.push(cycle.clone());
            }
        }
        regions.into_iter().map(|(r, _, _)| r).collect()
    }

    /// A cycle as a polygon, its arcs cut into short chords.
    fn polygon(&self, cycle: &[Half]) -> Vec<P> {
        cycle
            .iter()
            .flat_map(|&h| self.segment(h).polyline(self.tol))
            .collect()
    }

    /// Whether `p` lies inside `region` (on neither its outer loop's outside nor in a hole).
    #[cfg(test)]
    pub fn contains(&self, region: &Region, p: P) -> bool {
        winding(&self.polygon(&region.outer), p) != 0
            && region
                .holes
                .iter()
                .all(|hole| winding(&self.polygon(hole), p) == 0)
    }

    /// A point well inside `region`: the middle of the widest stretch inside it along one of
    /// a few horizontal lines across it, and how wide that stretch is.
    pub fn inside_point(&self, region: &Region) -> Option<(P, f64)> {
        let tol = self.tol;
        let halves: Vec<Half> = region
            .outer
            .iter()
            .chain(region.holes.iter().flatten())
            .copied()
            .collect();
        let polygon: Vec<P> = halves
            .iter()
            .flat_map(|&h| self.segment(h).polyline(tol))
            .collect();
        let (lo, hi) = polygon
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                (lo.min(p.y), hi.max(p.y))
            });
        if hi - lo <= tol || hi.is_nan() || lo.is_nan() {
            return None;
        }
        let mut best: Option<(P, f64)> = None;
        // Not half way first: a circle's vertex, at its angle zero, is at its centre's height.
        for f in [
            0.381_966, 0.618_034, 0.472_136, 0.236_068, 0.763_932, 0.145_898, 0.854_102,
        ] {
            let y = lo + (hi - lo) * f;
            let mut xs = Vec::new();
            for &h in &halves {
                self.segment(h).crossings(y, tol, &mut xs);
            }
            xs.sort_by(f64::total_cmp);
            for pair in xs.chunks(2) {
                if let [a, b] = pair {
                    let width = b - a;
                    if best.is_none_or(|(_, w)| width > w) {
                        best = Some((P::new((a + b) / 2.0, y), width));
                    }
                }
            }
        }
        best.filter(|(_, w)| *w > tol)
    }
}

/// The winding number of a closed polygon about `p`.
fn winding(polygon: &[P], p: P) -> i32 {
    let mut w = 0;
    for (i, a) in polygon.iter().enumerate() {
        let b = polygon[(i + 1) % polygon.len()];
        if a.y <= p.y {
            if b.y > p.y && b.sub(*a).cross(p.sub(*a)) > 0.0 {
                w += 1;
            }
        } else if b.y <= p.y && b.sub(*a).cross(p.sub(*a)) < 0.0 {
            w -= 1;
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOL: f64 = 1e-9;

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Segment> {
        let (a, b, c, d) = (
            P::new(x0, y0),
            P::new(x1, y0),
            P::new(x1, y1),
            P::new(x0, y1),
        );
        vec![
            Segment::line(a, b),
            Segment::line(b, c),
            Segment::line(c, d),
            Segment::line(d, a),
        ]
    }

    fn area(arrangement: &Arrangement, region: &Region) -> f64 {
        let cycle = |c: &[Half]| -> f64 {
            c.iter()
                .map(|&h| arrangement.segment(h).twice_area(TOL))
                .sum::<f64>()
                / 2.0
        };
        cycle(&region.outer) + region.holes.iter().map(|h| cycle(h)).sum::<f64>()
    }

    #[test]
    fn a_square_cut_by_a_line_makes_two_regions() {
        let mut segments = square(0.0, 0.0, 4.0, 2.0);
        segments.push(Segment::line(P::new(1.0, 0.0), P::new(3.0, 2.0)));
        let arrangement = Arrangement::new(&segments, TOL);
        let regions = arrangement.regions();
        assert_eq!(regions.len(), 2);
        let mut areas: Vec<f64> = regions.iter().map(|r| area(&arrangement, r)).collect();
        areas.sort_by(f64::total_cmp);
        assert!(
            (areas[0] - 4.0).abs() < 1e-12 && (areas[1] - 4.0).abs() < 1e-12,
            "{areas:?}"
        );
    }

    #[test]
    fn a_circle_inside_a_square_is_a_disc_and_a_hole() {
        let mut segments = square(0.0, 0.0, 10.0, 8.0);
        let start = P::new(7.0, 4.0);
        segments.push(Segment::arc(start, start, P::new(5.0, 4.0), true));
        let arrangement = Arrangement::new(&segments, TOL);
        let regions = arrangement.regions();
        assert_eq!(regions.len(), 2);
        let disc = 4.0 * PI;
        let holed = regions.iter().find(|r| r.holes.len() == 1).unwrap();
        assert!((area(&arrangement, holed) - (80.0 - disc)).abs() < 1e-9);
        let inner = regions.iter().find(|r| r.holes.is_empty()).unwrap();
        assert!((area(&arrangement, inner) - disc).abs() < 1e-9);
        let (p, _) = arrangement.inside_point(holed).unwrap();
        assert!(p.distance(P::new(5.0, 4.0)) > 2.0);
        assert!(arrangement.contains(holed, p) && !arrangement.contains(inner, p));
    }

    #[test]
    fn a_circle_across_an_edge_splits_both() {
        // A disc centred on the square's right side: half in, half out.
        let mut segments = square(0.0, 0.0, 10.0, 8.0);
        let start = P::new(12.0, 4.0);
        segments.push(Segment::arc(start, start, P::new(10.0, 4.0), true));
        let arrangement = Arrangement::new(&segments, TOL);
        let regions = arrangement.regions();
        assert_eq!(regions.len(), 3);
        let mut areas: Vec<f64> = regions.iter().map(|r| area(&arrangement, r)).collect();
        areas.sort_by(f64::total_cmp);
        let half = 2.0 * PI;
        assert!((areas[0] - half).abs() < 1e-9 && (areas[1] - half).abs() < 1e-9);
        assert!((areas[2] - (80.0 - half)).abs() < 1e-9, "{areas:?}");
    }

    #[test]
    fn a_slot_with_tangent_arcs_is_one_region() {
        let (a, b, c, d) = (
            P::new(0.0, 0.0),
            P::new(4.0, 0.0),
            P::new(4.0, 2.0),
            P::new(0.0, 2.0),
        );
        let segments = vec![
            Segment::line(a, b),
            Segment::arc(b, c, P::new(4.0, 1.0), true),
            Segment::line(c, d),
            Segment::arc(d, a, P::new(0.0, 1.0), true),
            // A chord across, tangent to nothing: two regions.
            Segment::line(P::new(2.0, 0.0), P::new(2.0, 2.0)),
        ];
        let arrangement = Arrangement::new(&segments, TOL);
        let regions = arrangement.regions();
        assert_eq!(regions.len(), 2);
        let total: f64 = regions.iter().map(|r| area(&arrangement, r)).sum();
        assert!((total - (8.0 + PI)).abs() < 1e-9);
    }

    #[test]
    fn concentric_circles_make_a_ring_and_a_disc() {
        let (c, r2) = (P::new(1.0, 1.0), 3.0);
        let segments = vec![
            Segment::arc(P::new(3.0, 1.0), P::new(3.0, 1.0), c, true),
            Segment::arc(P::new(4.0, 1.0), P::new(4.0, 1.0), c, false),
        ];
        let arrangement = Arrangement::new(&segments, TOL);
        let regions = arrangement.regions();
        assert_eq!(regions.len(), 2);
        let ring = regions.iter().find(|r| r.holes.len() == 1).unwrap();
        assert!((area(&arrangement, ring) - PI * (r2 * r2 - 4.0)).abs() < 1e-9);
    }

    #[test]
    fn a_line_along_a_side_adds_no_region() {
        let mut segments = square(0.0, 0.0, 4.0, 2.0);
        segments.push(Segment::line(P::new(1.0, 0.0), P::new(3.0, 0.0)));
        let arrangement = Arrangement::new(&segments, TOL);
        let regions = arrangement.regions();
        assert_eq!(regions.len(), 1);
        assert!((area(&arrangement, &regions[0]) - 8.0).abs() < 1e-12);
        // The side is split where the line's ends are.
        assert_eq!(arrangement.edges.len(), 6);
    }

    #[test]
    fn a_slot_cut_by_its_own_circles_keeps_one_hole() {
        // A slot's outline, the whole circles its ends lie on, and the lines along its sides
        // running a hair past their ends, as a face meets them when the slot cuts it.
        let mut segments = square(0.0, 0.0, 40.0, 30.0);
        let (a, b, c, d) = (
            P::new(10.0, 10.0),
            P::new(25.0, 10.0),
            P::new(25.0, 20.0),
            P::new(10.0, 20.0),
        );
        segments.extend([
            Segment::line(a, b),
            Segment::arc(b, c, P::new(25.0, 15.0), true),
            Segment::line(c, d),
            Segment::arc(d, a, P::new(10.0, 15.0), true),
        ]);
        for cx in [10.0, 25.0] {
            let s = P::new(cx + 5.0, 15.0);
            segments.push(Segment::arc(s, s, P::new(cx, 15.0), true));
        }
        segments.push(Segment::line(
            P::new(25.000_001, 20.0),
            P::new(9.999_999, 20.0),
        ));
        segments.push(Segment::line(
            P::new(9.999_999, 10.0),
            P::new(25.000_001, 10.0),
        ));
        let arrangement = Arrangement::new(&segments, 1e-5);
        let regions = arrangement.regions();
        assert_eq!(regions.len(), 4);
        let disc = 25.0 * PI;
        let outer = regions.iter().find(|r| !r.holes.is_empty()).unwrap();
        assert_eq!(outer.holes.len(), 1);
        assert!((area(&arrangement, outer) - (1200.0 - 150.0 - disc)).abs() < 1e-6);
        // The hole runs round the slot's outline: two lines, the half circle at one end, and
        // at the other the half circle cut where its whole circle starts.
        assert_eq!(outer.holes[0].len(), 5);
    }
}
