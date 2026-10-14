//! Editing helpers on solved geometry: where curves cross, the closest point on a curve, and
//! trimming a curve back to its nearest crossings (SolidWorks' "Trim Entities", power trim).

use std::collections::HashMap;
use std::f64::consts::TAU;

use oxyde_geom::Point2;

use crate::sketch::{ConstrainedSketch, Constraint, Entity, EntityId, Geometry, curve_points};

/// Two points closer than this are the same point.
const TOLERANCE: f64 = 1e-6;

/// A curve as plain geometry, for intersection and projection.
#[derive(Debug, Clone, Copy)]
enum Curve {
    Segment(Point2, Point2),
    Circle(Point2, f64),
    /// Centre, radius, start angle and counter-clockwise sweep.
    Arc(Point2, f64, f64, f64),
}

fn angle_of(center: Point2, p: Point2) -> f64 {
    (p.y - center.y).atan2(p.x - center.x)
}

fn on_circle(center: Point2, radius: f64, angle: f64) -> Point2 {
    Point2::new(
        center.x + radius * angle.cos(),
        center.y + radius * angle.sin(),
    )
}

impl Curve {
    /// Where `p` is along the curve: 0 to 1 along a segment, the angle (0 to 2π) around a
    /// circle, the counter-clockwise angle from the start of an arc.
    fn parameter(&self, p: Point2) -> f64 {
        match *self {
            Curve::Segment(a, b) => {
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let length2 = dx * dx + dy * dy;
                if length2 < 1e-24 {
                    0.0
                } else {
                    ((p.x - a.x) * dx + (p.y - a.y) * dy) / length2
                }
            }
            Curve::Circle(c, _) => angle_of(c, p).rem_euclid(TAU),
            Curve::Arc(c, _, start, sweep) => {
                let offset = (angle_of(c, p) - start).rem_euclid(TAU);
                // Just before the start counts as the start, not as the far end of the turn.
                if offset > sweep && offset > TAU - 1e-9 {
                    0.0
                } else {
                    offset
                }
            }
        }
    }

    fn at(&self, t: f64) -> Point2 {
        match *self {
            Curve::Segment(a, b) => Point2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t),
            Curve::Circle(c, r) => on_circle(c, r, t),
            Curve::Arc(c, r, start, _) => on_circle(c, r, start + t),
        }
    }

    /// Range of `parameter` covered by the curve.
    fn range(&self) -> (f64, f64) {
        match *self {
            Curve::Segment(..) => (0.0, 1.0),
            Curve::Circle(..) => (0.0, TAU),
            Curve::Arc(_, _, _, sweep) => (0.0, sweep),
        }
    }

    /// The curve's parameter tolerance matching `TOLERANCE` in length.
    fn slack(&self) -> f64 {
        match *self {
            Curve::Segment(a, b) => TOLERANCE / a.distance(b).max(1e-12),
            Curve::Circle(_, r) | Curve::Arc(_, r, _, _) => TOLERANCE / r.max(1e-12),
        }
    }

    fn closest(&self, p: Point2) -> Point2 {
        let (lo, hi) = self.range();
        match *self {
            Curve::Circle(c, r) => {
                if c.distance(p) < 1e-12 {
                    on_circle(c, r, 0.0)
                } else {
                    on_circle(c, r, angle_of(c, p))
                }
            }
            Curve::Segment(..) => self.at(self.parameter(p).clamp(lo, hi)),
            Curve::Arc(_, _, _, sweep) => {
                let t = self.parameter(p);
                if t <= sweep {
                    self.at(t)
                } else {
                    // Past the end: whichever end is nearer.
                    let (s, e) = (self.at(0.0), self.at(sweep));
                    if s.distance(p) <= e.distance(p) { s } else { e }
                }
            }
        }
    }

    fn contains(&self, p: Point2) -> bool {
        if self.closest(p).distance(p) > TOLERANCE * 10.0 {
            return false;
        }
        let (lo, hi) = self.range();
        let t = self.parameter(p);
        let slack = self.slack() * 10.0;
        t >= lo - slack && t <= hi + slack
    }

    /// Points where the carrier lines and circles of two curves cross, on both curves.
    fn intersect(&self, other: &Curve) -> Vec<Point2> {
        let candidates = match (carrier(self), carrier(other)) {
            (Carrier::Line(p, d), Carrier::Line(q, e)) => line_line(p, d, q, e),
            (Carrier::Line(p, d), Carrier::Circle(c, r))
            | (Carrier::Circle(c, r), Carrier::Line(p, d)) => line_circle(p, d, c, r),
            (Carrier::Circle(c1, r1), Carrier::Circle(c2, r2)) => circle_circle(c1, r1, c2, r2),
        };
        candidates
            .into_iter()
            .filter(|p| self.contains(*p) && other.contains(*p))
            .collect()
    }
}

enum Carrier {
    /// Point and direction.
    Line(Point2, (f64, f64)),
    Circle(Point2, f64),
}

fn carrier(curve: &Curve) -> Carrier {
    match *curve {
        Curve::Segment(a, b) => Carrier::Line(a, (b.x - a.x, b.y - a.y)),
        Curve::Circle(c, r) | Curve::Arc(c, r, _, _) => Carrier::Circle(c, r),
    }
}

fn line_line(p: Point2, d: (f64, f64), q: Point2, e: (f64, f64)) -> Vec<Point2> {
    let cross = d.0 * e.1 - d.1 * e.0;
    let scale = (d.0.hypot(d.1) * e.0.hypot(e.1)).max(1e-24);
    if cross.abs() < 1e-12 * scale {
        return Vec::new(); // parallel or overlapping: no single crossing
    }
    let t = ((q.x - p.x) * e.1 - (q.y - p.y) * e.0) / cross;
    vec![Point2::new(p.x + d.0 * t, p.y + d.1 * t)]
}

fn line_circle(p: Point2, d: (f64, f64), c: Point2, r: f64) -> Vec<Point2> {
    let length = d.0.hypot(d.1);
    if length < 1e-12 {
        return Vec::new();
    }
    let u = (d.0 / length, d.1 / length);
    // Foot of the perpendicular from the centre.
    let along = (c.x - p.x) * u.0 + (c.y - p.y) * u.1;
    let foot = Point2::new(p.x + u.0 * along, p.y + u.1 * along);
    let h = c.distance(foot);
    if h > r + TOLERANCE {
        Vec::new()
    } else if h > r - TOLERANCE {
        vec![foot] // tangent
    } else {
        let half = (r * r - h * h).sqrt();
        vec![
            Point2::new(foot.x - u.0 * half, foot.y - u.1 * half),
            Point2::new(foot.x + u.0 * half, foot.y + u.1 * half),
        ]
    }
}

fn circle_circle(c1: Point2, r1: f64, c2: Point2, r2: f64) -> Vec<Point2> {
    let d = c1.distance(c2);
    if d < 1e-12 || d > r1 + r2 + TOLERANCE || d < (r1 - r2).abs() - TOLERANCE {
        return Vec::new();
    }
    let a = (r1 * r1 - r2 * r2 + d * d) / (2.0 * d);
    let h = (r1 * r1 - a * a).max(0.0).sqrt();
    let u = ((c2.x - c1.x) / d, (c2.y - c1.y) / d);
    let m = Point2::new(c1.x + u.0 * a, c1.y + u.1 * a);
    if h < TOLERANCE {
        vec![m]
    } else {
        vec![
            Point2::new(m.x - u.1 * h, m.y + u.0 * h),
            Point2::new(m.x + u.1 * h, m.y - u.0 * h),
        ]
    }
}

/// Where a curve is cut by another one.
#[derive(Debug, Clone, Copy)]
struct Cut {
    param: f64,
    point: Point2,
    by: EntityId,
}

/// What a trim removes from a curve.
#[derive(Debug, Clone, Copy)]
enum Piece {
    /// Nothing crosses the curve (or, for a circle, fewer than two crossings): delete it.
    Whole,
    /// From the start to a crossing.
    Start(Cut),
    /// From a crossing to the end.
    End(Cut),
    /// Between two crossings. For a circle, counter-clockwise from the first to the second.
    Between(Cut, Cut),
}

#[derive(Debug, Clone, Copy)]
struct TrimPlan {
    shape: Curve,
    piece: Piece,
}

impl ConstrainedSketch {
    fn curve(&self, id: EntityId) -> Option<Curve> {
        let at = |p| self.position(p);
        match self.entity(id)?.geometry {
            Geometry::Point(_) => None,
            Geometry::Line { start, end } => Some(Curve::Segment(at(start)?, at(end)?)),
            Geometry::Circle { center, radius } => Some(Curve::Circle(at(center)?, radius)),
            Geometry::Arc { center, start, end } => {
                let (c, s, e) = (at(center)?, at(start)?, at(end)?);
                let a0 = angle_of(c, s);
                let mut sweep = (angle_of(c, e) - a0).rem_euclid(TAU);
                if sweep < 1e-12 {
                    sweep = TAU;
                }
                Some(Curve::Arc(c, c.distance(s), a0, sweep))
            }
        }
    }

    fn curves(&self) -> Vec<(EntityId, Curve)> {
        self.entities
            .iter()
            .filter_map(|e| Some((e.id, self.curve(e.id)?)))
            .collect()
    }

    /// The point of a line, circle or arc closest to `p`.
    pub fn closest_point(&self, curve: EntityId, p: Point2) -> Option<Point2> {
        Some(self.curve(curve)?.closest(p))
    }

    /// The middle of a line.
    pub fn midpoint_of(&self, line: EntityId) -> Option<Point2> {
        let (a, b) = self.endpoints(line)?;
        let (a, b) = (self.position(a)?, self.position(b)?);
        Some(Point2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0))
    }

    /// Every point where two curves cross, with the two curves. Points shared by connected
    /// curves (a common end point) are left out.
    pub fn intersections(&self) -> Vec<(EntityId, EntityId, Point2)> {
        let curves = self.curves();
        let mut out = Vec::new();
        for (i, (a, ca)) in curves.iter().enumerate() {
            for (b, cb) in &curves[i + 1..] {
                for p in ca.intersect(cb) {
                    if !self.is_shared_point(*a, *b, p) {
                        out.push((*a, *b, p));
                    }
                }
            }
        }
        out
    }

    /// Whether `p` is a point entity both curves stand on.
    fn is_shared_point(&self, a: EntityId, b: EntityId, p: Point2) -> bool {
        let points = |id| {
            self.entity(id)
                .map(|e| curve_points(&e.geometry))
                .unwrap_or_default()
        };
        let pb = points(b);
        points(a).iter().any(|q| {
            pb.contains(q)
                && self
                    .position(*q)
                    .is_some_and(|at| at.distance(p) < TOLERANCE * 10.0)
        })
    }

    fn trim_plan(&self, curve: EntityId, at: Point2) -> Option<TrimPlan> {
        let shape = self.curve(curve)?;
        let slack = shape.slack() * 10.0;
        let (lo, hi) = shape.range();
        let closed = matches!(shape, Curve::Circle(..));
        let mut cuts: Vec<Cut> = Vec::new();
        for (other, other_shape) in self.curves() {
            if other == curve {
                continue;
            }
            for point in shape.intersect(&other_shape) {
                let param = shape.parameter(point);
                let interior = closed || (param > lo + slack && param < hi - slack);
                let duplicate = cuts.iter().any(|c| (c.param - param).abs() < slack);
                if interior && !duplicate {
                    cuts.push(Cut {
                        param,
                        point,
                        by: other,
                    });
                }
            }
        }
        cuts.sort_by(|a, b| a.param.total_cmp(&b.param));
        let clicked = shape.parameter(shape.closest(at));

        let piece = if closed {
            if cuts.len() < 2 {
                Piece::Whole
            } else {
                // The gap around the click, wrapping past 2π.
                let after = cuts.iter().position(|c| c.param > clicked);
                let (from, to) = match after {
                    Some(0) | None => (cuts[cuts.len() - 1], cuts[0]),
                    Some(i) => (cuts[i - 1], cuts[i]),
                };
                Piece::Between(from, to)
            }
        } else {
            let before = cuts.iter().rev().find(|c| c.param < clicked).copied();
            let after = cuts.iter().find(|c| c.param >= clicked).copied();
            match (before, after) {
                (None, None) => Piece::Whole,
                (None, Some(to)) => Piece::Start(to),
                (Some(from), None) => Piece::End(from),
                (Some(from), Some(to)) => Piece::Between(from, to),
            }
        };
        Some(TrimPlan { shape, piece })
    }

    /// The part of `curve` that trimming at `at` would remove, as a polyline for preview.
    pub fn trim_preview(&self, curve: EntityId, at: Point2) -> Option<Vec<Point2>> {
        let plan = self.trim_plan(curve, at)?;
        let (lo, hi) = plan.shape.range();
        let (from, to) = match plan.piece {
            Piece::Whole => (lo, hi),
            Piece::Start(cut) => (lo, cut.param),
            Piece::End(cut) => (cut.param, hi),
            Piece::Between(a, b) => {
                let to = if b.param <= a.param {
                    b.param + TAU
                } else {
                    b.param
                };
                (a.param, to)
            }
        };
        let steps = match plan.shape {
            Curve::Segment(..) => 1,
            _ => 48,
        };
        Some(
            (0..=steps)
                .map(|k| plan.shape.at(from + (to - from) * k as f64 / steps as f64))
                .collect(),
        )
    }

    /// Trims `curve` back to the crossings nearest to `at`, removing the piece `at` is on: a
    /// curve nothing crosses is deleted, an end piece is cut back, a middle piece splits the
    /// curve in two, and a circle becomes an arc. New end points stay on the curve that cut
    /// them (point-on-line or point-on-circle relations). Returns false if `curve` is not a
    /// line, circle or arc.
    pub fn trim(&mut self, curve: EntityId, at: Point2) -> bool {
        let Some(plan) = self.trim_plan(curve, at) else {
            return false;
        };
        let Some(entity) = self.entity(curve).copied() else {
            return false;
        };
        match plan.piece {
            Piece::Whole => self.remove(&[curve]),
            Piece::Start(cut) => {
                let q = self.cut_point(cut);
                self.reattach(curve, |g| set_start(g, q));
                self.after_shortening(curve, &entity);
            }
            Piece::End(cut) => {
                let q = self.cut_point(cut);
                self.reattach(curve, |g| set_end(g, q));
                self.after_shortening(curve, &entity);
            }
            Piece::Between(from, to) => match entity.geometry {
                Geometry::Circle { center, .. } => {
                    // Keep the rest of the circle: counter-clockwise from `to` round to `from`.
                    let (start, end) = (self.cut_point(to), self.cut_point(from));
                    self.reattach(curve, |g| *g = Geometry::Arc { center, start, end });
                }
                Geometry::Line { end, .. } | Geometry::Arc { end, .. } => {
                    let (q1, q2) = (self.cut_point(from), self.cut_point(to));
                    self.reattach(curve, |g| set_end(g, q1));
                    let second = match entity.geometry {
                        Geometry::Arc { center, .. } => {
                            let id = self.arc_between(center, q2, end);
                            self.constrain(Constraint::EqualRadius(curve, id));
                            id
                        }
                        _ => {
                            let id = self.line_between(q2, end);
                            self.copy_line_relations(curve, id);
                            id
                        }
                    };
                    self.set_construction(second, entity.construction);
                    self.after_shortening(curve, &entity);
                }
                Geometry::Point(_) => {}
            },
        }
        true
    }

    /// A new point at a crossing, kept on the curve that made it.
    fn cut_point(&mut self, cut: Cut) -> EntityId {
        let q = self.point(cut.point);
        let on = match self.entity(cut.by).map(|e| e.geometry) {
            Some(Geometry::Line { .. }) => Constraint::PointOnLine(q, cut.by),
            _ => Constraint::PointOnCircle(q, cut.by),
        };
        self.constrain(on);
        q
    }

    fn reattach(&mut self, curve: EntityId, change: impl FnOnce(&mut Geometry)) {
        if let Some(e) = self.entities.iter_mut().find(|e| e.id == curve) {
            change(&mut e.geometry);
        }
    }

    /// After a curve got shorter: drop its length dimension and midpoint relations (they
    /// would pull the geometry back), and end points nothing stands on any more.
    fn after_shortening(&mut self, curve: EntityId, before: &Entity) {
        self.relations.retain(|r| {
            !matches!(r.constraint,
                Constraint::Length(l, _) | Constraint::Midpoint(_, l) if l == curve)
        });
        let in_use: Vec<EntityId> = self
            .entities
            .iter()
            .flat_map(|e| curve_points(&e.geometry))
            .collect();
        let origin = self.origin();
        let orphans: Vec<EntityId> = curve_points(&before.geometry)
            .into_iter()
            .filter(|p| !in_use.contains(p) && Some(*p) != origin)
            .collect();
        if !orphans.is_empty() {
            self.entities.retain(|e| !orphans.contains(&e.id));
            self.relations.retain(|r| {
                r.constraint
                    .entities()
                    .iter()
                    .all(|id| !orphans.contains(id))
            });
        }
    }

    /// The second piece of a split line keeps the first one's direction relations and stays
    /// in line with it.
    fn copy_line_relations(&mut self, from: EntityId, to: EntityId) {
        let copies: Vec<Constraint> = self
            .relations
            .iter()
            .filter_map(|r| match r.constraint {
                Constraint::Horizontal(l) if l == from => Some(Constraint::Horizontal(to)),
                Constraint::Vertical(l) if l == from => Some(Constraint::Vertical(to)),
                _ => None,
            })
            .collect();
        if copies.is_empty() {
            self.constrain(Constraint::Parallel(from, to));
        }
        for c in copies {
            self.constrain(c);
        }
        // With the start of the second piece on the first one, the two stay in line.
        if let Some((start, _)) = self.endpoints(to) {
            self.constrain(Constraint::PointOnLine(start, from));
        }
    }
}

fn set_start(geometry: &mut Geometry, q: EntityId) {
    match geometry {
        Geometry::Line { start, .. } | Geometry::Arc { start, .. } => *start = q,
        _ => {}
    }
}

fn set_end(geometry: &mut Geometry, q: EntityId) {
    match geometry {
        Geometry::Line { end, .. } | Geometry::Arc { end, .. } => *end = q,
        _ => {}
    }
}

impl ConstrainedSketch {
    /// Mirrors `entities` (curves and points) about `line`, as SolidWorks' Mirror Entities:
    /// each copied point is tied to its original by a symmetric relation, so the copy
    /// follows later edits. Points on the mirror line are shared. Returns the new curves and
    /// points, or `None` if `line` is not a line.
    pub fn mirror(&mut self, entities: &[EntityId], line: EntityId) -> Option<Vec<EntityId>> {
        let (a, b) = self.endpoints(line)?;
        let (a, b) = (self.position(a)?, self.position(b)?);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let length2 = dx * dx + dy * dy;
        if length2 < 1e-24 {
            return None;
        }
        let reflect = |p: Point2| {
            let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / length2;
            let foot = Point2::new(a.x + dx * t, a.y + dy * t);
            Point2::new(2.0 * foot.x - p.x, 2.0 * foot.y - p.y)
        };
        let on_line = |p: Point2| reflect(p).distance(p) < 1e-9;

        let mut copies: HashMap<EntityId, EntityId> = HashMap::new();
        let mut created = Vec::new();
        let mut copy_point = |sketch: &mut Self, p: EntityId, created: &mut Vec<EntityId>| {
            if let Some(q) = copies.get(&p) {
                return *q;
            }
            let at = sketch.position(p).unwrap_or_default();
            let q = if on_line(at) {
                p
            } else {
                let q = sketch.point(reflect(at));
                sketch.constrain(Constraint::Symmetric(p, q, line));
                created.push(q);
                q
            };
            copies.insert(p, q);
            q
        };

        let originals: Vec<Entity> = entities
            .iter()
            .filter(|id| **id != line)
            .filter_map(|id| self.entity(*id).copied())
            .collect();
        for e in originals {
            let new = match e.geometry {
                Geometry::Point(_) => {
                    copy_point(self, e.id, &mut created);
                    continue;
                }
                Geometry::Line { start, end } => {
                    let (s, f) = (
                        copy_point(self, start, &mut created),
                        copy_point(self, end, &mut created),
                    );
                    self.line_between(s, f)
                }
                Geometry::Circle { center, radius } => {
                    let c = copy_point(self, center, &mut created);
                    let id = self.circle_around(c, radius);
                    self.constrain(Constraint::EqualRadius(e.id, id));
                    id
                }
                Geometry::Arc { center, start, end } => {
                    // A mirror turns counter-clockwise into clockwise: swap the ends.
                    let c = copy_point(self, center, &mut created);
                    let s = copy_point(self, start, &mut created);
                    let f = copy_point(self, end, &mut created);
                    self.arc_between(c, f, s)
                }
            };
            self.set_construction(new, e.construction);
            created.push(new);
        }
        Some(created)
    }
}
