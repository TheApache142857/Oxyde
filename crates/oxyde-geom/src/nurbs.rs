//! Non-uniform rational B-splines in a sketch plane: the curve every part edge that is not
//! a line or a conic is copied into a sketch as, and what a spline edge projects to.
//!
//! A curve of degree `p` with `n` poles has `n + p + 1` knots and one weight per pole. It is
//! evaluated by de Boor's algorithm on the poles multiplied by their weights (homogeneous
//! coordinates), which gives the exact point and, from the next to last level of the same
//! computation, the exact derivative.

use crate::{LINEAR_TOLERANCE, Point2};

/// A rational B-spline curve in plane coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct Nurbs2 {
    pub degree: usize,
    pub poles: Vec<Point2>,
    pub weights: Vec<f64>,
    pub knots: Vec<f64>,
}

/// Samples per knot span when a spline is drawn, measured or scanned for crossings: fine
/// enough that two crossings within one sample would need curves closer than sketches draw.
pub(crate) const SAMPLES_PER_SPAN: usize = 32;

/// Halvings that bring a crossing or an extreme found between two samples to the precision
/// of an `f64` parameter.
const REFINE_STEPS: usize = 60;

/// A point times its weight, and the weight.
type Homogeneous = [f64; 3];

fn lerp(a: Homogeneous, b: Homogeneous, t: f64) -> Homogeneous {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

impl Nurbs2 {
    fn knot(&self, i: usize) -> f64 {
        self.knots.get(i).copied().unwrap_or(0.0)
    }

    /// Whether the numbers describe a curve: at least `degree + 1` poles, one positive weight
    /// each, `poles + degree + 1` knots in order, a domain of some length, all finite.
    pub fn is_valid(&self) -> bool {
        let (n, p) = (self.poles.len(), self.degree);
        let (lo, hi) = self.domain();
        p >= 1
            && n > p
            && self.weights.len() == n
            && self.knots.len() == n + p + 1
            && self.knots.windows(2).all(|w| w.first() <= w.get(1))
            && self.knots.iter().all(|k| k.is_finite())
            && self.weights.iter().all(|w| w.is_finite() && *w > 0.0)
            && self
                .poles
                .iter()
                .all(|q| q.x.is_finite() && q.y.is_finite())
            && hi > lo
    }

    /// The parameter range the curve covers.
    pub fn domain(&self) -> (f64, f64) {
        (self.knot(self.degree), self.knot(self.poles.len()))
    }

    /// The knot span holding `t`: `k` with `knots[k] <= t < knots[k + 1]`, the last span of
    /// some length at the end of the domain.
    fn span(&self, t: f64) -> usize {
        let (p, n) = (self.degree, self.poles.len());
        let mut k = p;
        while k + 1 < n && self.knot(k + 1) <= t {
            k += 1;
        }
        while k > p && self.knot(k + 1) <= self.knot(k) {
            k -= 1;
        }
        k
    }

    /// The point at `t` and the derivative there, in homogeneous coordinates.
    fn de_boor(&self, t: f64) -> (Homogeneous, Homogeneous) {
        let p = self.degree;
        let (lo, hi) = self.domain();
        let t = t.clamp(lo, hi);
        let k = self.span(t);
        let mut d: Vec<Homogeneous> = (0..=p)
            .map(|j| {
                let i = j + k - p;
                let q = self.poles.get(i).copied().unwrap_or_default();
                let w = self.weights.get(i).copied().unwrap_or(1.0);
                [q.x * w, q.y * w, w]
            })
            .collect();
        let mut derivative = [0.0; 3];
        for r in 1..=p {
            if r == p {
                // The last two points of the level before the last one give the derivative.
                let span = self.knot(k + 1) - self.knot(k);
                if let (Some(a), Some(b)) = (d.get(p - 1), d.get(p))
                    && span > 0.0
                {
                    let scale = p as f64 / span;
                    derivative = [
                        (b[0] - a[0]) * scale,
                        (b[1] - a[1]) * scale,
                        (b[2] - a[2]) * scale,
                    ];
                }
            }
            for j in (r..=p).rev() {
                let left = self.knot(j + k - p);
                let right = self.knot(j + 1 + k - r);
                let alpha = if right > left {
                    (t - left) / (right - left)
                } else {
                    0.0
                };
                let previous = d.get(j - 1).copied().unwrap_or_default();
                if let Some(current) = d.get_mut(j) {
                    *current = lerp(previous, *current, alpha);
                }
            }
        }
        (d.get(p).copied().unwrap_or_default(), derivative)
    }

    /// The point at parameter `t` (clamped to the domain).
    pub fn point_at(&self, t: f64) -> Point2 {
        let ([x, y, w], _) = self.de_boor(t);
        if w == 0.0 {
            return Point2::new(x, y);
        }
        Point2::new(x / w, y / w)
    }

    /// The derivative of the curve at `t`, exactly: its direction and its speed.
    pub fn derivative_at(&self, t: f64) -> (f64, f64) {
        let ([x, y, w], [dx, dy, dw]) = self.de_boor(t);
        if w == 0.0 {
            return (dx, dy);
        }
        let (px, py) = (x / w, y / w);
        ((dx - dw * px) / w, (dy - dw * py) / w)
    }

    pub fn start(&self) -> Point2 {
        self.point_at(self.domain().0)
    }

    pub fn end(&self) -> Point2 {
        self.point_at(self.domain().1)
    }

    /// Whether the curve ends where it starts.
    pub fn is_closed(&self) -> bool {
        self.start().distance(self.end()) <= LINEAR_TOLERANCE
    }

    /// The same curve run the other way.
    pub fn reversed(&self) -> Nurbs2 {
        let (lo, hi) = self.domain();
        Nurbs2 {
            degree: self.degree,
            poles: self.poles.iter().rev().copied().collect(),
            weights: self.weights.iter().rev().copied().collect(),
            knots: self.knots.iter().rev().map(|k| lo + hi - k).collect(),
        }
    }

    /// The parameters the curve is sampled at: `per_span` steps in every knot span of some
    /// length, both ends of the domain included.
    pub fn sample_parameters(&self, per_span: usize) -> Vec<f64> {
        let per_span = per_span.max(1);
        let (lo, hi) = self.domain();
        let mut out = vec![lo];
        for w in self.knots.windows(2) {
            let (&a, &b) = match w {
                [a, b] => (a, b),
                _ => continue,
            };
            if b <= a || b <= lo || a >= hi {
                continue;
            }
            let (a, b) = (a.max(lo), b.min(hi));
            for s in 1..=per_span {
                out.push(a + (b - a) * s as f64 / per_span as f64);
            }
        }
        out
    }

    /// Points along the curve, `per_span` to a knot span, from its start to its end.
    pub fn points(&self, per_span: usize) -> Vec<Point2> {
        self.sample_parameters(per_span)
            .into_iter()
            .map(|t| self.point_at(t))
            .collect()
    }

    /// The smallest and the largest value of `p.x * ux + p.y * uy` over the curve, each
    /// found between two samples and refined.
    pub fn range_along(&self, ux: f64, uy: f64) -> (f64, f64) {
        let along = |t: f64| {
            let p = self.point_at(t);
            p.x * ux + p.y * uy
        };
        let ts = self.sample_parameters(SAMPLES_PER_SPAN);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for t in &ts {
            let v = along(*t);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        for w in ts.windows(3) {
            if let [a, m, b] = *w {
                let (va, vm, vb) = (along(a), along(m), along(b));
                if vm >= va && vm >= vb {
                    hi = hi.max(along(extreme(|t| -along(t), a, b)));
                }
                if vm <= va && vm <= vb {
                    lo = lo.min(along(extreme(along, a, b)));
                }
            }
        }
        (lo, hi)
    }

    /// The parameter of the point of the curve closest to `q`.
    pub fn closest(&self, q: Point2) -> f64 {
        let distance = |t: f64| self.point_at(t).distance(q);
        let ts = self.sample_parameters(SAMPLES_PER_SPAN);
        let best = ts
            .iter()
            .enumerate()
            .min_by(|a, b| distance(*a.1).total_cmp(&distance(*b.1)))
            .map_or(0, |(i, _)| i);
        let (lo, hi) = self.domain();
        let a = best
            .checked_sub(1)
            .and_then(|i| ts.get(i))
            .copied()
            .unwrap_or(lo);
        let b = ts.get(best + 1).copied().unwrap_or(hi);
        // Where the curve's direction is square to the way to `q`: found by halving where
        // that changes sign, which reaches the precision a flat smallest distance does not.
        let square = |t: f64| {
            let p = self.point_at(t);
            let (dx, dy) = self.derivative_at(t);
            (p.x - q.x) * dx + (p.y - q.y) * dy
        };
        let (mut a, mut b) = (a, b);
        let (fa, fb) = (square(a), square(b));
        if fa.signum() == fb.signum() {
            return extreme(distance, a, b);
        }
        for _ in 0..REFINE_STEPS {
            let mid = 0.5 * (a + b);
            if square(mid).signum() == fa.signum() {
                a = mid;
            } else {
                b = mid;
            }
        }
        0.5 * (a + b)
    }

    /// The smallest box holding the poles, which holds the curve.
    pub fn pole_bounds(&self) -> (Point2, Point2) {
        let mut lo = Point2::new(f64::INFINITY, f64::INFINITY);
        let mut hi = Point2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
        for q in &self.poles {
            lo = Point2::new(lo.x.min(q.x), lo.y.min(q.y));
            hi = Point2::new(hi.x.max(q.x), hi.y.max(q.y));
        }
        (lo, hi)
    }

    /// Points of the curve where `g`, a signed measure that is about the distance to
    /// another curve near it, changes sign, or comes within `tolerance` of zero where the
    /// curves touch.
    pub(crate) fn zeros_of(&self, g: impl Fn(Point2) -> f64, tolerance: f64) -> Vec<Point2> {
        let f = |t: f64| g(self.point_at(t));
        let samples: Vec<(f64, f64)> = self
            .sample_parameters(SAMPLES_PER_SPAN)
            .into_iter()
            .map(|t| (t, f(t)))
            .collect();
        let mut out = Vec::new();
        for pair in samples.windows(2) {
            let &[(t0, v0), (t1, v1)] = pair else {
                continue;
            };
            if v0 == 0.0 {
                out.push(self.point_at(t0));
            } else if v0.signum() != v1.signum() && v1 != 0.0 {
                let (mut lo, mut hi) = (t0, t1);
                for _ in 0..REFINE_STEPS {
                    let mid = 0.5 * (lo + hi);
                    if f(mid).signum() == v0.signum() {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                out.push(self.point_at(0.5 * (lo + hi)));
            }
        }
        if let Some(&(t, v)) = samples.last()
            && v == 0.0
        {
            out.push(self.point_at(t));
        }
        for triple in samples.windows(3) {
            let &[(t0, v0), (_, v1), (t2, v2)] = triple else {
                continue;
            };
            if v1.abs() <= v0.abs() && v1.abs() <= v2.abs() {
                let t = extreme(|t| f(t).abs(), t0, t2);
                if f(t).abs() <= tolerance {
                    out.push(self.point_at(t));
                }
            }
        }
        out
    }
}

/// Where `f` is smallest on `[lo, hi]`, by golden-section search: it has one smallest value
/// there, between two samples.
fn extreme(f: impl Fn(f64) -> f64, mut lo: f64, mut hi: f64) -> f64 {
    let ratio = (5.0_f64.sqrt() - 1.0) / 2.0;
    for _ in 0..REFINE_STEPS {
        let a = hi - ratio * (hi - lo);
        let b = lo + ratio * (hi - lo);
        if f(a) <= f(b) {
            hi = b;
        } else {
            lo = a;
        }
    }
    0.5 * (lo + hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quarter circle of radius 2 about the origin, from (2, 0) to (0, 2), exactly: a
    /// quadratic with the middle weight cos 45°.
    fn quarter() -> Nurbs2 {
        Nurbs2 {
            degree: 2,
            poles: vec![
                Point2::new(2.0, 0.0),
                Point2::new(2.0, 2.0),
                Point2::new(0.0, 2.0),
            ],
            weights: vec![1.0, std::f64::consts::FRAC_1_SQRT_2, 1.0],
            knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        }
    }

    /// A cubic through two spans, not rational.
    fn wave() -> Nurbs2 {
        Nurbs2 {
            degree: 3,
            poles: vec![
                Point2::new(0.0, 0.0),
                Point2::new(1.0, 3.0),
                Point2::new(3.0, -2.0),
                Point2::new(5.0, 2.0),
                Point2::new(6.0, 0.0),
            ],
            weights: vec![1.0; 5],
            knots: vec![0.0, 0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 1.0],
        }
    }

    #[test]
    fn a_rational_quadratic_is_exactly_a_circle() {
        let q = quarter();
        assert!(q.is_valid());
        for k in 0..=20 {
            let t = k as f64 / 20.0;
            let p = q.point_at(t);
            assert!((p.distance(Point2::ORIGIN) - 2.0).abs() < 1e-12, "{p:?}");
            // The derivative is square to the radius.
            let (dx, dy) = q.derivative_at(t);
            assert!((dx * p.x + dy * p.y).abs() < 1e-9 * dx.hypot(dy), "{t}");
        }
        assert!(q.start().distance(Point2::new(2.0, 0.0)) < 1e-12);
        assert!(q.end().distance(Point2::new(0.0, 2.0)) < 1e-12);
    }

    #[test]
    fn derivatives_match_differences_and_reversal_keeps_the_curve() {
        let w = wave();
        assert!(w.is_valid());
        for k in 1..20 {
            let t = k as f64 / 20.0;
            let h = 1e-6;
            let (a, b) = (w.point_at(t - h), w.point_at(t + h));
            let (dx, dy) = w.derivative_at(t);
            assert!(((b.x - a.x) / (2.0 * h) - dx).abs() < 1e-5, "{t}");
            assert!(((b.y - a.y) / (2.0 * h) - dy).abs() < 1e-5, "{t}");
            let r = w.reversed();
            assert!(r.point_at(1.0 - t).distance(w.point_at(t)) < 1e-12);
        }
        assert!(!w.is_closed());
    }

    #[test]
    fn reach_and_closest_points_are_refined() {
        let q = quarter();
        let s = std::f64::consts::FRAC_1_SQRT_2;
        let (lo, hi) = q.range_along(s, s);
        assert!((hi - 2.0).abs() < 1e-12, "{hi}");
        assert!((lo - 2.0 * s).abs() < 1e-12, "{lo}");
        let t = q.closest(Point2::new(5.0, 5.0));
        assert!(q.point_at(t).distance(Point2::new(2.0 * s, 2.0 * s)) < 1e-9);
        // A line x = 1 crosses the quarter circle at (1, √3).
        let crossings = q.zeros_of(|p| p.x - 1.0, 1e-9);
        assert!(
            crossings
                .iter()
                .any(|p| p.distance(Point2::new(1.0, 3f64.sqrt())) < 1e-9)
        );
    }

    #[test]
    fn malformed_numbers_are_refused() {
        let mut bad = wave();
        bad.knots.pop();
        assert!(!bad.is_valid());
        let mut bad = wave();
        bad.weights[1] = 0.0;
        assert!(!bad.is_valid());
        let mut bad = wave();
        bad.knots.swap(4, 5);
        assert!(!bad.is_valid());
    }
}
