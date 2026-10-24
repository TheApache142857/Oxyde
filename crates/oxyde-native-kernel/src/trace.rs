//! Curves traced where two surfaces cross, where no closed form and no single angle follows
//! them: a torus cut off its axis, a hole that only partly crosses another (its curve turns
//! back along either one's lines).
//!
//! A traced curve ([`Trace`]) is held by what makes it: the two surfaces, the point it
//! starts at, the way it runs from there (along the cross product of the two surfaces'
//! normals or against it), and how long it is. Its parameter is the length along the
//! polygon of points marched along it from its start, each point on both surfaces: a step
//! along the curve's tangent, then Newton's method back onto both surfaces in the plane
//! square to the tangent, the steps shortened where the curve bends. The points are kept
//! in a cache that can always be made again from the curve itself, the same every time,
//! so a curve stays a small value that copies. A point at any parameter is the cubic
//! between the two points about it, moved onto both surfaces by the same Newton's method:
//! exact to rounding, as a section's points are. OpenCASCADE fits a B-spline to such a
//! curve; it is reported as one.

// Indices into the samples of a curve, made here.
#![allow(clippy::indexing_slicing)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use oxyde_geom::{LINEAR_TOLERANCE, Point3, Transform, Vec3};

use crate::geometry::Surface;
use crate::revolution::Meridian;

/// A curve where two surfaces cross, followed from `start`: the way it runs from there is
/// along `n_a × n_b` when `forward`, against it when not, for `length` along the polygon of
/// its points; a closed one comes back to its start. An edge on it runs the way its
/// parameter grows when `up`, as an edge on a section runs as its angle grows when `ccw`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Trace {
    pub a: Surface,
    pub b: Surface,
    pub start: Point3,
    pub forward: bool,
    pub length: f64,
    pub closed: bool,
    pub up: bool,
}

/// The points marched along a curve: each one's parameter, the point, and the unit tangent
/// there, the way the parameter grows.
#[derive(Debug)]
struct Samples {
    points: Vec<(f64, Point3, Vec3)>,
}

/// A curve as the cache knows it: the bits of what makes it.
type Key = Vec<u64>;

/// The curves of one hash the cache holds, each with its points.
type Bucket = Vec<(Key, Arc<Samples>)>;

/// The points of every curve traced lately, by the curve (its `up` aside): by a hash of
/// it, the whole of it kept with them to tell curves of one hash apart.
static CACHE: LazyLock<Mutex<HashMap<u64, Bucket>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

thread_local! {
    /// The parameters of the points edges start at, looked up lately: an edge is followed
    /// from its start many times over.
    static PARAMS: RefCell<Vec<(Key, [u64; 3], f64)>> = const { RefCell::new(Vec::new()) };
}

/// How many curves the cache holds before it starts again.
const CACHE_SIZE: usize = 2048;

fn surface_bits(s: &Surface, out: &mut Vec<u64>) {
    match s {
        Surface::Plane(p) => {
            out.push(0);
            for v in [p.origin.to_vec(), p.x_dir, p.normal] {
                out.extend([v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]);
            }
        }
        Surface::Revolution(r) => {
            out.push(1);
            for v in [r.axis.origin.to_vec(), r.axis.direction, r.x_dir] {
                out.extend([v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]);
            }
            match r.meridian {
                Meridian::Line { radius, slope } => {
                    out.extend([2, radius.to_bits(), slope.to_bits()]);
                }
                Meridian::Circle { center, radius } => {
                    out.extend([3, center.to_bits(), radius.to_bits()]);
                }
            }
            out.push(u64::from(r.outward));
        }
    }
}

/// The surface's normal at a point near it (out of its solid).
fn normal(s: &Surface, p: Point3) -> Vec3 {
    match s {
        Surface::Plane(plane) => plane.normal,
        Surface::Revolution(r) => r.normal_at(p),
    }
}

/// The curve's direction at `p`, the way `forward` says: `None` where the surfaces touch,
/// or nearly (their normals within a thousandth of a radian of each other).
fn direction(a: &Surface, b: &Surface, forward: bool, p: Point3) -> Option<Vec3> {
    let t = normal(a, p).cross(normal(b, p));
    if t.length() < 1e-3 {
        return None;
    }
    let t = t.normalized()?;
    Some(if forward { t } else { -t })
}

/// `x` with `rows · x = b`, by Cramer's rule.
fn solve(rows: [Vec3; 3], b: [f64; 3]) -> Option<Vec3> {
    let [r0, r1, r2] = rows;
    let det = r0.dot(r1.cross(r2));
    if det.abs() < 1e-14 {
        return None;
    }
    Some((r1.cross(r2) * b[0] + r2.cross(r0) * b[1] + r0.cross(r1) * b[2]) * (1.0 / det))
}

/// `guess` moved onto both surfaces, keeping its place along `t` (in the plane through it
/// square to `t`).
pub(crate) fn onto_both(a: &Surface, b: &Surface, guess: Point3, t: Vec3) -> Option<Point3> {
    let mut q = guess;
    for _ in 0..30 {
        let r = [
            -a.signed_distance(q),
            -b.signed_distance(q),
            -(q - guess).dot(t),
        ];
        let step = solve([normal(a, q), normal(b, q), t], r)?;
        q = q + step;
        if step.length() < 1e-14 {
            break;
        }
    }
    (a.signed_distance(q).abs() < 1e-9 && b.signed_distance(q).abs() < 1e-9).then_some(q)
}

/// A length the two surfaces are about the size of: what steps are scaled by.
fn scale(a: &Surface, b: &Surface) -> f64 {
    let size = |s: &Surface| match s {
        Surface::Plane(_) => f64::INFINITY,
        Surface::Revolution(r) => match r.meridian {
            Meridian::Line { radius, .. } => radius.abs().max(1.0),
            Meridian::Circle { radius, .. } => radius,
        },
    };
    let s = size(a).min(size(b));
    if s.is_finite() { s } else { 10.0 }
}

/// Where marching stops.
#[derive(Debug, Clone, Copy)]
enum Stop {
    /// After this length along it (a curve already traced, marched again).
    Length(f64),
    /// Where it leaves this box, or comes back to where it started.
    Region(Point3, Point3),
}

/// Why marching stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// It came back to where it started.
    Closed,
    /// It ran its length, or out of the box.
    Out,
    /// The surfaces touch there: the curve cannot be followed on.
    Touched,
}

/// The points along the curve from `start`, the way `forward` says, until `stop`, and why
/// it stopped.
fn march(
    a: &Surface,
    b: &Surface,
    start: Point3,
    forward: bool,
    stop: Stop,
) -> (Vec<(f64, Point3, Vec3)>, Ended) {
    let size = scale(a, b);
    let (h_min, h_max) = (1e-5 * size, 0.05 * size);
    let mut h = 0.02 * size;
    let Some(t0) = direction(a, b, forward, start) else {
        return (vec![(0.0, start, Vec3::ZERO)], Ended::Touched);
    };
    let mut points = vec![(0.0, start, t0)];
    let (mut p, mut t, mut s) = (start, t0, 0.0);
    let limit = match stop {
        Stop::Length(l) => l,
        Stop::Region(..) => f64::INFINITY,
    };
    for _ in 0..50_000 {
        if s >= limit - 1e-12 {
            break;
        }
        // At the end of a curve already traced, the last step lands on its end.
        let step = h.min(limit - s);
        let Some(q) = onto_both(a, b, p + t * step, t) else {
            if h > h_min {
                h /= 2.0;
                continue;
            }
            return (points, Ended::Touched);
        };
        let Some(tq) = direction(a, b, forward, q) else {
            return (points, Ended::Touched);
        };
        let turn = t.dot(tq).clamp(-1.0, 1.0).acos();
        if turn > 3f64.to_radians() && h > h_min {
            h /= 2.0;
            continue;
        }
        let ds = q.distance(p);
        // Back where it started: closed.
        if let Stop::Region(..) = stop
            && s > 4.0 * h
            && q.distance(start) <= 1.5 * h.max(ds)
            && (start - p).dot(t) > 0.0
        {
            let last = p.distance(start);
            points.push((s + last, start, t0));
            return (points, Ended::Closed);
        }
        s += ds;
        points.push((s, q, tq));
        (p, t) = (q, tq);
        if let Stop::Region(lo, hi) = stop {
            let pad = 1e-9;
            if q.x < lo.x - pad
                || q.y < lo.y - pad
                || q.z < lo.z - pad
                || q.x > hi.x + pad
                || q.y > hi.y + pad
                || q.z > hi.z + pad
            {
                break;
            }
        }
        if turn < 1f64.to_radians() {
            h = (h * 1.5).min(h_max);
        }
    }
    (points, Ended::Out)
}

impl Trace {
    fn key(&self) -> Vec<u64> {
        let mut out = Vec::with_capacity(40);
        surface_bits(&self.a, &mut out);
        surface_bits(&self.b, &mut out);
        let s = self.start.to_vec();
        out.extend([s.x.to_bits(), s.y.to_bits(), s.z.to_bits()]);
        out.extend([
            u64::from(self.forward),
            self.length.to_bits(),
            u64::from(self.closed),
        ]);
        out
    }

    fn samples(&self) -> Arc<Samples> {
        let key = self.key();
        let hash = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut h);
            h.finish()
        };
        if let Some(found) = CACHE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&hash)
            .and_then(|list| list.iter().find(|(k, _)| *k == key))
        {
            return Arc::clone(&found.1);
        }
        let (mut points, _) = march(
            &self.a,
            &self.b,
            self.start,
            self.forward,
            Stop::Length(self.length),
        );
        if self.closed
            && let Some(last) = points.last_mut()
        {
            // A closed curve ends where it started.
            last.1 = self.start;
        }
        let samples = Arc::new(Samples { points });
        let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.len() >= CACHE_SIZE {
            cache.clear();
        }
        cache
            .entry(hash)
            .or_default()
            .push((key, Arc::clone(&samples)));
        samples
    }

    /// The point at parameter `s` (taken round a closed curve), `None` off an open one's
    /// ends.
    pub fn point_at(&self, s: f64) -> Option<Point3> {
        let samples = self.samples();
        let points = &samples.points;
        let n = points.len();
        if n < 2 {
            return points.first().map(|p| p.1);
        }
        let end = points[n - 1].0;
        let s = if self.closed {
            s.rem_euclid(end.max(f64::MIN_POSITIVE))
        } else {
            let slack = 1e-9 * end.max(1.0);
            if s < -slack || s > end + slack {
                return None;
            }
            s.clamp(0.0, end)
        };
        // The stretch between two samples it falls in.
        let k = match points.binary_search_by(|p| p.0.total_cmp(&s)) {
            Ok(k) => return Some(points[k].1),
            Err(k) => k.clamp(1, n - 1),
        };
        let (s0, p0, t0) = points[k - 1];
        let (s1, p1, t1) = points[k];
        let h = s1 - s0;
        if h <= 0.0 {
            return Some(p0);
        }
        let u = (s - s0) / h;
        // Cubic Hermite between them, then onto both surfaces.
        let (h00, h10, h01, h11) = (
            2.0 * u * u * u - 3.0 * u * u + 1.0,
            u * u * u - 2.0 * u * u + u,
            -2.0 * u * u * u + 3.0 * u * u,
            u * u * u - u * u,
        );
        let guess = Point3::new(
            p0.x * h00 + t0.x * h * h10 + p1.x * h01 + t1.x * h * h11,
            p0.y * h00 + t0.y * h * h10 + p1.y * h01 + t1.y * h * h11,
            p0.z * h00 + t0.z * h * h10 + p1.z * h01 + t1.z * h * h11,
        );
        let tangent = (t0 * (1.0 - u) + t1 * u).normalized().unwrap_or(t0);
        Some(onto_both(&self.a, &self.b, guess, tangent).unwrap_or(guess))
    }

    /// How far the curve runs from its start: its whole length round a closed one.
    pub fn span(&self) -> f64 {
        self.samples().points.last().map_or(0.0, |p| p.0)
    }

    /// The parameter of `p`, a point of the curve (in `[0, span)` round a closed one).
    pub fn param_of(&self, p: Point3) -> f64 {
        let samples = self.samples();
        let points = &samples.points;
        let mut best = (f64::INFINITY, 0.0);
        for w in points.windows(2) {
            let (s0, a, _) = w[0];
            let (s1, b, _) = w[1];
            let d = b - a;
            let l2 = d.dot(d);
            let u = if l2 > 0.0 {
                ((p - a).dot(d) / l2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let q = a + d * u;
            let dist = q.distance(p);
            if dist < best.0 {
                best = (dist, s0 + (s1 - s0) * u);
            }
        }
        // Refined: where the curve's tangent is square to the way to `p`.
        let mut s = best.1;
        let h = 1e-6 * self.span().max(1.0);
        for _ in 0..8 {
            let (Some(q), Some(q1), Some(q0)) =
                (self.point_at(s), self.point_at(s + h), self.point_at(s - h))
            else {
                break;
            };
            let d = (q1 - q0) * (1.0 / (2.0 * h));
            let dd = d.dot(d);
            if dd <= 0.0 {
                break;
            }
            let step = (p - q).dot(d) / dd;
            s += step;
            if step.abs() < 1e-13 {
                break;
            }
        }
        if self.closed {
            s.rem_euclid(self.span().max(f64::MIN_POSITIVE))
        } else {
            s
        }
    }

    /// The parameter of `p`, remembered for the points looked up lately.
    fn param_of_remembered(&self, p: Point3) -> f64 {
        let key = self.key();
        let at = [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
        if let Some(found) = PARAMS.with(|m| {
            m.borrow()
                .iter()
                .find(|(k, q, _)| *q == at && *k == key)
                .map(|e| e.2)
        }) {
            return found;
        }
        let s = self.param_of(p);
        PARAMS.with(|m| {
            let mut m = m.borrow_mut();
            if m.len() >= 32 {
                m.remove(0);
            }
            m.push((key, at, s));
        });
        s
    }

    /// How far along an edge from `start` turns to `p`, its way round, in `[0, span)` round
    /// a closed curve.
    pub fn turned(&self, start: Point3, p: Point3) -> f64 {
        let d = self.param_of(p) - self.param_of_remembered(start);
        let d = if self.up { d } else { -d };
        if self.closed {
            d.rem_euclid(self.span())
        } else {
            d
        }
    }

    /// How far an edge from `start` to `end` runs: the whole curve for a closed edge.
    pub fn sweep(&self, start: Point3, end: Point3, closed: bool) -> f64 {
        if closed {
            return self.span();
        }
        let t = self.turned(start, end);
        if t <= 1e-12 && self.closed {
            self.span()
        } else {
            t
        }
    }

    /// The point `s` along from `start`, its way.
    pub fn along(&self, start: Point3, s: f64) -> Option<Point3> {
        let t = self.param_of_remembered(start);
        self.point_at(if self.up { t + s } else { t - s })
    }

    /// How far `p` lies off the curve.
    pub fn distance(&self, p: Point3) -> f64 {
        self.point_at(self.param_of(p))
            .map_or(f64::INFINITY, |q| q.distance(p))
    }

    /// The curve moved by `transform`: a mirror turns the cross product of the normals
    /// round, so the curve keeps its way by running the other way along it.
    pub fn transformed(&self, transform: &Transform) -> Trace {
        Trace {
            a: self.a.transformed(transform),
            b: self.b.transformed(transform),
            start: transform.apply_point(self.start),
            forward: self.forward != transform.is_mirror(),
            ..*self
        }
    }

    /// Points from `start` along `extent` to `end`, both ends included, each chord within
    /// `deflection` of the curve.
    pub fn polyline(
        &self,
        start: Point3,
        end: Point3,
        extent: f64,
        deflection: f64,
    ) -> Vec<Point3> {
        let mut out = vec![start];
        let n = ((extent / (0.02 * scale(&self.a, &self.b))).ceil() as usize).max(2);
        let mut stack: Vec<(f64, f64)> = (0..n)
            .rev()
            .map(|i| {
                (
                    extent * i as f64 / n as f64,
                    extent * (i + 1) as f64 / n as f64,
                )
            })
            .collect();
        while let Some((s0, s1)) = stack.pop() {
            let (Some(p0), Some(p1), Some(pm)) = (
                self.along(start, s0),
                self.along(start, s1),
                self.along(start, (s0 + s1) / 2.0),
            ) else {
                continue;
            };
            let chord = p0 + (p1 - p0) * 0.5;
            if pm.distance(chord) > deflection && s1 - s0 > 1e-9 {
                stack.push(((s0 + s1) / 2.0, s1));
                stack.push((s0, (s0 + s1) / 2.0));
            } else if s1 < extent {
                out.push(p1);
            }
        }
        out.push(end);
        out
    }

    /// Its length from `start` along `extent`, by quadrature of its speed.
    pub fn length_from(&self, start: Point3, extent: f64) -> f64 {
        let h = 1e-6 * extent.max(1.0);
        let speed = |s: f64| match (self.along(start, s + h), self.along(start, s - h)) {
            (Some(a), Some(b)) => a.distance(b) / (2.0 * h),
            _ => 1.0,
        };
        crate::revolution::quadrature(0.0, extent, extent / 32.0, &speed)
    }

    /// The extremes an edge from `start` running `extent` reaches along each model axis,
    /// between its ends.
    pub fn extremes(&self, start: Point3, extent: f64) -> Vec<Point3> {
        let n = 256;
        let samples: Vec<(f64, Point3)> = (0..=n)
            .filter_map(|i| {
                let s = extent * f64::from(i) / f64::from(n);
                self.along(start, s).map(|p| (s, p))
            })
            .collect();
        let h = extent / f64::from(n);
        let mut out = Vec::new();
        for k in 0..3 {
            for side in [1.0, -1.0] {
                let value = |p: Point3| side * crate::geometry::component(p.to_vec(), k);
                for w in samples.windows(3) {
                    let &[(_, a), (sm, m), (_, b)] = w else {
                        continue;
                    };
                    if value(m) <= value(a) || value(m) < value(b) {
                        continue;
                    }
                    let (mut lo, mut hi) = (sm - h, sm + h);
                    let g = 0.618_033_988_749_895;
                    for _ in 0..60 {
                        let (x1, x2) = (hi - g * (hi - lo), lo + g * (hi - lo));
                        let f1 = self.along(start, x1).map_or(f64::NEG_INFINITY, value);
                        let f2 = self.along(start, x2).map_or(f64::NEG_INFINITY, value);
                        if f1 < f2 {
                            lo = x1;
                        } else {
                            hi = x2;
                        }
                    }
                    if let Some(p) = self.along(start, (lo + hi) / 2.0) {
                        out.push(p);
                    }
                }
            }
        }
        out
    }
}

/// The curves where surfaces `a` and `b` cross near `seeds` (points on both, or near both),
/// within the box `lo`–`hi`: each traced both ways from the first seed not already on one,
/// a closed one from that seed, an open one from where it leaves the box, along `n_a × n_b`.
pub(crate) fn traces(
    a: &Surface,
    b: &Surface,
    seeds: &[Point3],
    lo: Point3,
    hi: Point3,
) -> Option<Vec<Trace>> {
    let mut out: Vec<(Trace, Vec<Point3>)> = Vec::new();
    let near = 0.05 * scale(a, b);
    for &seed in seeds {
        let Some(t) = direction(a, b, true, seed) else {
            continue;
        };
        let Some(seed) = onto_both(a, b, seed, t) else {
            continue;
        };
        if out
            .iter()
            .any(|(_, pts)| pts.iter().any(|p| p.distance(seed) <= near))
        {
            continue;
        }
        let (forward, ended) = march(a, b, seed, true, Stop::Region(lo, hi));
        let (start, length, points, closed) = match ended {
            Ended::Closed => {
                let length = forward.last().map_or(0.0, |p| p.0);
                (seed, length, forward, true)
            }
            // Where the surfaces touch the curve is not followed on.
            Ended::Touched => return None,
            Ended::Out => {
                // Back to where it leaves the box the other way, then along it from there.
                let (back, ended) = march(a, b, seed, false, Stop::Region(lo, hi));
                if ended == Ended::Touched {
                    return None;
                }
                let Some(&(_, start, _)) = back.last() else {
                    continue;
                };
                let (along, ended) = march(a, b, start, true, Stop::Region(lo, hi));
                if ended == Ended::Touched {
                    return None;
                }
                let length = along.last().map_or(0.0, |p| p.0);
                (start, length, along, false)
            }
        };
        if length <= 10.0 * LINEAR_TOLERANCE {
            continue;
        }
        let trace = Trace {
            a: *a,
            b: *b,
            start,
            forward: true,
            length,
            closed,
            up: true,
        };
        out.push((trace, points.iter().map(|p| p.1).collect()));
    }
    Some(out.into_iter().map(|(t, _)| t).collect())
}

#[cfg(test)]
mod tests {
    use oxyde_geom::{Axis, Plane};

    use super::*;
    use crate::revolution::Revolution;

    /// A torus about Z through the origin, its tube 4 round a circle of radius 10.
    fn torus() -> Surface {
        Surface::Revolution(Revolution {
            axis: Axis {
                origin: Point3::ORIGIN,
                direction: Vec3::Z,
            },
            x_dir: Vec3::X,
            meridian: Meridian::Circle {
                center: 10.0,
                radius: 4.0,
            },
            outward: true,
        })
    }

    #[test]
    fn a_plane_along_a_torus_s_axis_cuts_it_in_ovals_traced_exactly() {
        // The plane x = 8 (along the axis, through the tube): two ovals, one each side of
        // the axis's plane y = 0... here one loop through the tube on each side of y.
        let plane = Surface::Plane(Plane {
            origin: Point3::new(8.0, 0.0, 0.0),
            x_dir: Vec3::Y,
            normal: Vec3::X,
        });
        let seeds = [Point3::new(8.0, 9.0, 0.0), Point3::new(8.0, -9.0, 0.0)];
        let lo = Point3::new(-20.0, -20.0, -20.0);
        let hi = Point3::new(20.0, 20.0, 20.0);
        let found = traces(&plane, &torus(), &seeds, lo, hi).unwrap();
        assert!(!found.is_empty());
        for trace in &found {
            assert!(trace.closed, "{trace:?}");
            for i in 0..100 {
                let s = trace.span() * f64::from(i) / 100.0;
                let p = trace.point_at(s).unwrap();
                assert!(plane.signed_distance(p).abs() < 1e-9);
                assert!(torus().signed_distance(p).abs() < 1e-9);
                // The parameter comes back.
                let back = trace.param_of(p);
                assert!((back - s).abs() < 1e-6 || (back - s).abs() > trace.span() - 1e-6);
            }
        }
    }
}
