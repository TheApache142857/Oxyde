//! Sketch Fillet and Offset Entities: geometry made from existing curves, with the
//! relations that keep it following them.

use std::collections::{HashMap, HashSet};
use std::f64::consts::{PI, TAU};

use oxyde_geom::Point2;

use crate::edit::{
    Carrier, Curve, TOLERANCE, angle_of, carrier, circle_circle, line_circle, line_line, on_circle,
};
use crate::sketch::{ConstrainedSketch, Constraint, EntityId, Geometry};

fn unit(dx: f64, dy: f64) -> Option<(f64, f64)> {
    let length = dx.hypot(dy);
    (length > TOLERANCE).then(|| (dx / length, dy / length))
}

fn shifted(p: Point2, direction: (f64, f64), by: f64) -> Point2 {
    Point2::new(p.x + direction.0 * by, p.y + direction.1 * by)
}

/// Points along a counter-clockwise arc, for previews.
fn arc_polyline(center: Point2, radius: f64, start: f64, sweep: f64) -> Vec<Point2> {
    let segments = ((sweep / TAU * 72.0).ceil() as usize).clamp(8, 72);
    (0..=segments)
        .map(|k| on_circle(center, radius, start + sweep * k as f64 / segments as f64))
        .collect()
}

/// The start angle and the counter-clockwise sweep from `start` to `end` around `center`.
fn sweep_of(center: Point2, start: Point2, end: Point2) -> (f64, f64) {
    let a0 = angle_of(center, start);
    let mut sweep = (angle_of(center, end) - a0).rem_euclid(TAU);
    if sweep < 1e-12 {
        sweep = TAU;
    }
    (a0, sweep)
}

/// Where the carrier lines and circles of two curves cross, anywhere along them.
fn carrier_crossings(a: &Curve, b: &Curve) -> Vec<Point2> {
    match (carrier(a), carrier(b)) {
        (Carrier::Line(p, d), Carrier::Line(q, e)) => line_line(p, d, q, e),
        (Carrier::Line(p, d), Carrier::Circle(c, r))
        | (Carrier::Circle(c, r), Carrier::Line(p, d)) => line_circle(p, d, c, r),
        (Carrier::Circle(c1, r1), Carrier::Circle(c2, r2)) => circle_circle(c1, r1, c2, r2),
    }
}

// ---- Sketch Fillet -------------------------------------------------------------------------

/// What a fillet makes of a corner.
struct FilletPlan {
    corner: EntityId,
    /// The far ends of the two lines.
    far: (EntityId, EntityId),
    /// Where each line now ends.
    ends: (Point2, Point2),
    center: Point2,
    radius: f64,
    /// The arc runs counter-clockwise from the first line's new end to the second's.
    ccw: bool,
}

impl ConstrainedSketch {
    fn is_line(&self, id: EntityId) -> bool {
        matches!(
            self.entity(id).map(|e| e.geometry),
            Some(Geometry::Line { .. })
        )
    }

    /// The lines and arcs with an end at `point`.
    fn curves_ending_at(&self, point: EntityId) -> Vec<EntityId> {
        self.entities()
            .iter()
            .filter(|e| match e.geometry {
                Geometry::Line { start, end } | Geometry::Arc { start, end, .. } => {
                    start == point || end == point
                }
                _ => false,
            })
            .map(|e| e.id)
            .collect()
    }

    /// The two lines meeting at `point`, when nothing else ends there: a corner Sketch
    /// Fillet can round.
    pub fn corner_lines(&self, point: EntityId) -> Option<(EntityId, EntityId)> {
        match self.curves_ending_at(point)[..] {
            [a, b] if self.is_line(a) && self.is_line(b) => Some((a, b)),
            _ => None,
        }
    }

    /// The end point two lines share.
    pub fn shared_corner(&self, first: EntityId, second: EntityId) -> Option<EntityId> {
        if first == second {
            return None;
        }
        let (a, b) = self.endpoints(first)?;
        let (c, d) = self.endpoints(second)?;
        [a, b].into_iter().find(|p| *p == c || *p == d)
    }

    fn fillet_plan(&self, first: EntityId, second: EntityId, radius: f64) -> Option<FilletPlan> {
        if !radius.is_finite() || radius <= TOLERANCE {
            return None;
        }
        let corner = self.shared_corner(first, second)?;
        let far = |line: EntityId| {
            let (s, e) = self.endpoints(line)?;
            Some(if s == corner { e } else { s })
        };
        let far = (far(first)?, far(second)?);
        let p = self.position(corner)?;
        let (a, b) = (self.position(far.0)?, self.position(far.1)?);
        let u = unit(a.x - p.x, a.y - p.y)?;
        let v = unit(b.x - p.x, b.y - p.y)?;
        let theta = (u.0 * v.0 + u.1 * v.1).clamp(-1.0, 1.0).acos();
        if theta < 1e-6 || theta > PI - 1e-6 {
            return None; // in line: no corner to round
        }
        let half = theta / 2.0;
        // How far back from the corner the arc touches each line.
        let back = radius / half.tan();
        if back > p.distance(a) - TOLERANCE || back > p.distance(b) - TOLERANCE {
            return None; // no room
        }
        let bisector = unit(u.0 + v.0, u.1 + v.1)?;
        let center = shifted(p, bisector, radius / half.sin());
        let ends = (shifted(p, u, back), shifted(p, v, back));
        let (r1, r2) = (
            (ends.0.x - center.x, ends.0.y - center.y),
            (ends.1.x - center.x, ends.1.y - center.y),
        );
        let ccw = r1.0 * r2.1 - r1.1 * r2.0 > 0.0;
        Some(FilletPlan {
            corner,
            far,
            ends,
            center,
            radius,
            ccw,
        })
    }

    /// The arc Sketch Fillet would put between two lines (see [`fillet`](Self::fillet)),
    /// for a preview.
    pub fn fillet_preview(
        &self,
        first: EntityId,
        second: EntityId,
        radius: f64,
    ) -> Option<Vec<Point2>> {
        let plan = self.fillet_plan(first, second, radius)?;
        let (s, e) = if plan.ccw {
            (plan.ends.0, plan.ends.1)
        } else {
            (plan.ends.1, plan.ends.0)
        };
        let (a0, sweep) = sweep_of(plan.center, s, e);
        Some(arc_polyline(plan.center, plan.radius, a0, sweep))
    }

    /// Sketch Fillet: rounds the corner where two lines meet with an arc of `radius`
    /// tangent to both, cutting the lines back to it. Returns the arc; `None` when the
    /// lines share no end point, are in line, or are too short for the radius.
    ///
    /// The corner point is kept as a construction point on both lines (a virtual sharp, as
    /// SolidWorks keeps) when relations refer to it; a length dimension on a cut line then
    /// goes on measuring to it.
    pub fn fillet(&mut self, first: EntityId, second: EntityId, radius: f64) -> Option<EntityId> {
        let plan = self.fillet_plan(first, second, radius)?;
        let corner = plan.corner;
        let q1 = self.point(plan.ends.0);
        let q2 = self.point(plan.ends.1);
        let center = self.point(plan.center);
        self.replace_point(first, corner, q1);
        self.replace_point(second, corner, q2);
        let arc = if plan.ccw {
            self.arc_between(center, q1, q2)
        } else {
            self.arc_between(center, q2, q1)
        };
        let construction = [first, second]
            .iter()
            .all(|l| self.entity(*l).is_some_and(|e| e.construction));
        self.set_construction(arc, construction);
        self.constrain(Constraint::Tangent(first, arc));
        self.constrain(Constraint::Tangent(second, arc));

        // Length dimensions on the cut lines go on measuring to the corner; midpoint
        // relations would pull the lines back, and go.
        let mut virtual_sharp = Some(corner) == self.origin();
        for r in &mut self.relations {
            if let Constraint::Length(l, value) = r.constraint
                && (l == first || l == second)
            {
                let far = if l == first { plan.far.0 } else { plan.far.1 };
                r.constraint = Constraint::Distance(far, corner, value);
                virtual_sharp = true;
            }
        }
        self.relations.retain(
            |r| !matches!(r.constraint, Constraint::Midpoint(_, l) if l == first || l == second),
        );
        if self
            .relations
            .iter()
            .any(|r| r.constraint.entities().contains(&corner))
        {
            virtual_sharp = true;
        }
        if virtual_sharp {
            self.set_construction(corner, true);
            self.constrain(Constraint::PointOnLine(corner, first));
            self.constrain(Constraint::PointOnLine(corner, second));
        } else {
            self.remove(&[corner]);
        }
        Some(arc)
    }

    /// Makes a line or arc stand on `to` where it stood on `from`.
    fn replace_point(&mut self, curve: EntityId, from: EntityId, to: EntityId) {
        if let Some(e) = self.entities.iter_mut().find(|e| e.id == curve)
            && let Geometry::Line { start, end } | Geometry::Arc { start, end, .. } =
                &mut e.geometry
        {
            if *start == from {
                *start = to;
            }
            if *end == from {
                *end = to;
            }
        }
    }
}

// ---- Offset Entities -----------------------------------------------------------------------

/// A curve in a chain, and whether the chain walks it from its start to its end.
#[derive(Debug, Clone, Copy)]
struct Link {
    id: EntityId,
    forward: bool,
}

/// The end points of a line or an arc.
fn ends(geometry: &Geometry) -> Option<(EntityId, EntityId)> {
    match *geometry {
        Geometry::Line { start, end } | Geometry::Arc { start, end, .. } => Some((start, end)),
        _ => None,
    }
}

/// Where one chain's offset goes.
struct OffsetPlan {
    links: Vec<Link>,
    closed: bool,
    /// The new position of every end point.
    points: HashMap<EntityId, Point2>,
    /// The new radius of every circle and arc.
    radii: HashMap<EntityId, f64>,
}

impl ConstrainedSketch {
    /// The curves joined end to end with `curve`, as Offset Entities' chain selection:
    /// through shared end points, stopping where fewer or more than two curves meet,
    /// leaving construction geometry out. A circle is a chain of its own; a point is none.
    pub fn chain_of(&self, curve: EntityId) -> Vec<EntityId> {
        let Some(e) = self.entity(curve) else {
            return Vec::new();
        };
        let Some((s, f)) = ends(&e.geometry) else {
            return match e.geometry {
                Geometry::Circle { .. } => vec![curve],
                _ => Vec::new(),
            };
        };
        let around = |point: EntityId| -> Vec<EntityId> {
            self.entities()
                .iter()
                .filter(|e| {
                    !e.construction
                        && ends(&e.geometry).is_some_and(|(a, b)| a == point || b == point)
                })
                .map(|e| e.id)
                .collect()
        };
        let mut chain = vec![curve];
        for mut point in [s, f] {
            let mut here = curve;
            loop {
                let next = match around(point)[..] {
                    [a, b] if a == here => b,
                    [a, b] if b == here => a,
                    _ => break,
                };
                if chain.contains(&next) {
                    break;
                }
                chain.push(next);
                let Some((a, b)) = self.entity(next).and_then(|e| ends(&e.geometry)) else {
                    break;
                };
                point = if a == point { b } else { a };
                here = next;
            }
        }
        chain
    }

    /// `entities` ordered into chains, each walked end to end. `None` when three curves of
    /// the selection meet at a point.
    fn chains(&self, entities: &[EntityId]) -> Option<Vec<Vec<Link>>> {
        let mut set: Vec<EntityId> = Vec::new();
        for id in entities {
            let curve = self
                .entity(*id)
                .is_some_and(|e| !matches!(e.geometry, Geometry::Point(_)));
            if curve && !set.contains(id) {
                set.push(*id);
            }
        }
        let ends_of = |id: EntityId| self.entity(id).and_then(|e| ends(&e.geometry));
        let mut at: HashMap<EntityId, Vec<EntityId>> = HashMap::new();
        for id in &set {
            if let Some((s, e)) = ends_of(*id) {
                at.entry(s).or_default().push(*id);
                at.entry(e).or_default().push(*id);
            }
        }
        if at.values().any(|v| v.len() > 2) {
            return None;
        }
        let degree = |p: EntityId| at.get(&p).map_or(0, Vec::len);
        // Open chains are walked from an end, so they come out in order.
        let open =
            |id: &&EntityId| ends_of(**id).is_some_and(|(s, e)| degree(s) == 1 || degree(e) == 1);
        let starts: Vec<EntityId> = set
            .iter()
            .filter(open)
            .chain(set.iter().filter(|id| !open(id)))
            .copied()
            .collect();
        let mut seen = HashSet::new();
        let mut chains = Vec::new();
        for start in starts {
            if seen.contains(&start) {
                continue;
            }
            let Some((s, e)) = ends_of(start) else {
                seen.insert(start);
                chains.push(vec![Link {
                    id: start,
                    forward: true,
                }]);
                continue;
            };
            let mut here = start;
            let mut forward = degree(s) == 1 || degree(e) != 1;
            let mut chain = Vec::new();
            loop {
                seen.insert(here);
                chain.push(Link { id: here, forward });
                let Some((s, e)) = ends_of(here) else {
                    break;
                };
                let leave = if forward { e } else { s };
                let next = at
                    .get(&leave)
                    .and_then(|v| v.iter().find(|n| **n != here))
                    .copied();
                match next {
                    Some(n) if !seen.contains(&n) => {
                        forward = ends_of(n).is_some_and(|(ns, _)| ns == leave);
                        here = n;
                    }
                    _ => break,
                }
            }
            chains.push(chain);
        }
        Some(chains)
    }

    /// Unit normal to the left of a line walked in the link's direction.
    fn left_normal(&self, link: Link) -> Option<(f64, f64)> {
        let (s, e) = self.endpoints(link.id)?;
        let (s, e) = (self.position(s)?, self.position(e)?);
        let d = unit(e.x - s.x, e.y - s.y)?;
        let n = (-d.1, d.0);
        Some(if link.forward { n } else { (-n.0, -n.1) })
    }

    /// The offset of one curve: its shifted carrier and, for circles and arcs, its new
    /// radius. `None` when the curve would vanish.
    fn offset_link(&self, link: Link, by: f64) -> Option<(Curve, Option<f64>)> {
        match self.entity(link.id)?.geometry {
            Geometry::Line { start, end } => {
                let (s, e) = (self.position(start)?, self.position(end)?);
                let n = self.left_normal(link)?;
                Some((Curve::Segment(shifted(s, n, by), shifted(e, n, by)), None))
            }
            Geometry::Arc { center, start, end } => {
                let (c, s, e) = (
                    self.position(center)?,
                    self.position(start)?,
                    self.position(end)?,
                );
                let r = c.distance(s);
                // Walking counter-clockwise, the left is towards the centre.
                let radius = if link.forward { r - by } else { r + by };
                if radius <= TOLERANCE {
                    return None;
                }
                let (a0, sweep) = sweep_of(c, s, e);
                Some((Curve::Arc(c, radius, a0, sweep), Some(radius)))
            }
            Geometry::Circle { center, radius } => {
                let r = radius - by;
                if r <= TOLERANCE {
                    return None;
                }
                Some((Curve::Circle(self.position(center)?, r), Some(r)))
            }
            Geometry::Point(_) => None,
        }
    }

    /// Where an end point of a link lands when the link alone is offset by `by`.
    fn offset_end(&self, link: Link, point: EntityId, by: f64) -> Option<Point2> {
        let p = self.position(point)?;
        match self.entity(link.id)?.geometry {
            Geometry::Line { .. } => Some(shifted(p, self.left_normal(link)?, by)),
            Geometry::Arc { center, .. } => {
                let c = self.position(center)?;
                let r = c.distance(p);
                let radius = if link.forward { r - by } else { r + by };
                Some(shifted(c, unit(p.x - c.x, p.y - c.y)?, radius))
            }
            _ => None,
        }
    }

    fn offset_plan(&self, links: Vec<Link>, by: f64) -> Option<OffsetPlan> {
        let ends_of = |link: &Link| self.entity(link.id).and_then(|e| ends(&e.geometry));
        let enter = |link: &Link| ends_of(link).map(|(s, e)| if link.forward { s } else { e });
        let leave = |link: &Link| ends_of(link).map(|(s, e)| if link.forward { e } else { s });
        let mut curves = Vec::new();
        let mut radii = HashMap::new();
        for link in &links {
            let (curve, radius) = self.offset_link(*link, by)?;
            curves.push(curve);
            if let Some(r) = radius {
                radii.insert(link.id, r);
            }
        }
        let n = links.len();
        let closed =
            n >= 2 && enter(&links[0]).is_some() && leave(&links[n - 1]) == enter(&links[0]);
        let mut points = HashMap::new();
        // Where two neighbours meet: the shared point moved with both when they agree
        // (tangent there), else the nearest crossing of their offset carriers.
        let pairs = (0..n)
            .filter(|i| closed || *i + 1 < n)
            .map(|i| (i, (i + 1) % n));
        for (i, j) in pairs {
            let shared = leave(&links[i])?;
            if enter(&links[j]) != Some(shared) {
                return None;
            }
            let qi = self.offset_end(links[i], shared, by)?;
            let qj = self.offset_end(links[j], shared, by)?;
            let q = if qi.distance(qj) < 1e-7 {
                qi
            } else {
                let mid = Point2::new((qi.x + qj.x) / 2.0, (qi.y + qj.y) / 2.0);
                carrier_crossings(&curves[i], &curves[j])
                    .into_iter()
                    .min_by(|a, b| a.distance(mid).total_cmp(&b.distance(mid)))?
            };
            points.insert(shared, q);
        }
        if !closed && let (Some(first), Some(last)) = (links.first(), links.last()) {
            if let Some(p) = enter(first) {
                points.insert(p, self.offset_end(*first, p, by)?);
            }
            if let Some(p) = leave(last) {
                points.insert(p, self.offset_end(*last, p, by)?);
            }
        }
        Some(OffsetPlan {
            links,
            closed,
            points,
            radii,
        })
    }

    fn offset_plans(&self, entities: &[EntityId], by: f64) -> Option<Vec<OffsetPlan>> {
        let chains = self.chains(entities)?;
        if chains.is_empty() {
            return None;
        }
        chains
            .into_iter()
            .map(|links| self.offset_plan(links, by))
            .collect()
    }

    /// The curves Offset Entities would make (see [`offset`](Self::offset)), as polylines
    /// for a preview.
    pub fn offset_preview(&self, entities: &[EntityId], by: f64) -> Option<Vec<Vec<Point2>>> {
        let plans = self.offset_plans(entities, by)?;
        let mut out = Vec::new();
        for plan in &plans {
            for link in &plan.links {
                let polyline = match self.entity(link.id)?.geometry {
                    Geometry::Line { start, end } => {
                        vec![*plan.points.get(&start)?, *plan.points.get(&end)?]
                    }
                    Geometry::Arc { center, start, end } => {
                        let c = self.position(center)?;
                        let (a0, sweep) =
                            sweep_of(c, *plan.points.get(&start)?, *plan.points.get(&end)?);
                        arc_polyline(c, *plan.radii.get(&link.id)?, a0, sweep)
                    }
                    Geometry::Circle { center, .. } => {
                        arc_polyline(self.position(center)?, *plan.radii.get(&link.id)?, 0.0, TAU)
                    }
                    Geometry::Point(_) => continue,
                };
                out.push(polyline);
            }
        }
        Some(out)
    }

    /// Whether a line and an arc sharing an end point are tangent there.
    fn tangent_at_shared_point(&self, line: EntityId, arc: EntityId) -> bool {
        let Some((ls, le)) = self.endpoints(line) else {
            return false;
        };
        let Some(Geometry::Arc { center, start, end }) = self.entity(arc).map(|e| e.geometry)
        else {
            return false;
        };
        let Some(shared) = [ls, le].into_iter().find(|p| *p == start || *p == end) else {
            return false;
        };
        let positions = (
            self.position(ls),
            self.position(le),
            self.position(center),
            self.position(shared),
        );
        let (Some(a), Some(b), Some(c), Some(p)) = positions else {
            return false;
        };
        let (Some(d), Some(r)) = (unit(b.x - a.x, b.y - a.y), unit(p.x - c.x, p.y - c.y)) else {
            return false;
        };
        (d.0 * r.0 + d.1 * r.1).abs() < 1e-6
    }

    /// The new point for an old end point of the chain, made once.
    fn offset_point(
        &mut self,
        made: &mut HashMap<EntityId, EntityId>,
        plan: &OffsetPlan,
        old: EntityId,
    ) -> Option<EntityId> {
        if let Some(id) = made.get(&old) {
            return Some(*id);
        }
        let id = self.point(*plan.points.get(&old)?);
        made.insert(old, id);
        Some(id)
    }

    /// Offset Entities: copies of `entities` (lines, arcs and circles joined end to end;
    /// several chains at once) at distance `by`: to the left of each chain's first curve
    /// walked from its start, or to the right when `by` is negative.
    ///
    /// Each line stays parallel to its original and each arc and circle concentric (they
    /// share the centre point); a line tangent to a neighbouring arc stays so. The first
    /// line of each chain carries the distance as a dimension and the other lines keep the
    /// same distance (an equal-distance relation), so one value moves the whole offset; a
    /// chain without lines gets its radius as the dimension. The ends of an open chain are
    /// free. Returns the new curves; `None` when a curve would vanish (an arc or circle
    /// smaller than the offset), where three curves meet, or when two neighbours cannot be
    /// joined.
    pub fn offset(&mut self, entities: &[EntityId], by: f64) -> Option<Vec<EntityId>> {
        let plans = self.offset_plans(entities, by)?;
        let mut created = Vec::new();
        for plan in plans {
            let mut new_points: HashMap<EntityId, EntityId> = HashMap::new();
            let mut copies: HashMap<EntityId, EntityId> = HashMap::new();
            let has_line = plan.links.iter().any(|l| self.is_line(l.id));
            // The line (new, original) carrying the distance dimension.
            let mut base: Option<(EntityId, EntityId)> = None;
            let mut dimensioned = false;
            for link in &plan.links {
                let entity = *self.entity(link.id)?;
                let new = match entity.geometry {
                    Geometry::Line { start, end } => {
                        let s = self.offset_point(&mut new_points, &plan, start)?;
                        let e = self.offset_point(&mut new_points, &plan, end)?;
                        let id = self.line_between(s, e);
                        self.constrain(Constraint::Parallel(entity.id, id));
                        match base {
                            None => {
                                self.constrain(Constraint::LineDistance(id, entity.id, by.abs()));
                                base = Some((id, entity.id));
                            }
                            Some((b, ob)) => {
                                self.constrain(Constraint::EqualDistance(id, entity.id, b, ob));
                            }
                        }
                        id
                    }
                    Geometry::Arc { center, start, end } => {
                        let s = self.offset_point(&mut new_points, &plan, start)?;
                        let e = self.offset_point(&mut new_points, &plan, end)?;
                        let id = self.arc_between(center, s, e);
                        if !has_line && !dimensioned {
                            self.constrain(Constraint::Radius(id, *plan.radii.get(&entity.id)?));
                            dimensioned = true;
                        }
                        id
                    }
                    Geometry::Circle { center, .. } => {
                        let radius = *plan.radii.get(&entity.id)?;
                        let id = self.circle_around(center, radius);
                        self.constrain(Constraint::Radius(id, radius));
                        id
                    }
                    Geometry::Point(_) => continue,
                };
                self.set_construction(new, entity.construction);
                copies.insert(entity.id, new);
                created.push(new);
            }
            // Tangent joints stay tangent.
            let n = plan.links.len();
            let mut tied: HashSet<(EntityId, EntityId)> = HashSet::new();
            for i in 0..n {
                let j = (i + 1) % n;
                if j == i || (j == 0 && !plan.closed) {
                    continue;
                }
                let (a, b) = (plan.links[i].id, plan.links[j].id);
                let (line, arc) = match (self.is_line(a), self.is_line(b)) {
                    (true, false) => (a, b),
                    (false, true) => (b, a),
                    _ => continue,
                };
                if !self.tangent_at_shared_point(line, arc) || !tied.insert((line, arc)) {
                    continue;
                }
                if let (Some(l), Some(r)) = (copies.get(&line), copies.get(&arc)) {
                    self.constrain(Constraint::Tangent(*l, *r));
                }
            }
        }
        Some(created)
    }
}
