//! Ellipses in a sketch plane: among others, what a circle tilted to the plane projects to.
//!
//! A point of an ellipse is named by its eccentric angle `t`: the point
//! `center + a·cos t·u + b·sin t·v`, where `u` points from the centre to `major`, `v` is `u`
//! turned a quarter turn counter-clockwise, and `a`, `b` are the two semi-axes. The angle
//! grows counter-clockwise, as a circle's does.

use std::f64::consts::TAU;

use crate::{LINEAR_TOLERANCE, Point2};

/// An ellipse in plane coordinates. One semi-axis runs from `center` to `major`; the other,
/// square to it, is `minor` long. Either may be the longer one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ellipse2 {
    pub center: Point2,
    pub major: Point2,
    pub minor: f64,
}

/// Steps of the scan that looks for where an ellipse meets another round curve: fine enough
/// that two crossings of one step would need curves closer than sketches draw them.
const SCAN_STEPS: usize = 720;

/// Halvings that bring a crossing found by the scan to the precision of an `f64` angle.
const REFINE_STEPS: usize = 60;

impl Ellipse2 {
    /// Unit direction towards `major`, and the lengths of the semi-axes along it and across.
    fn frame(&self) -> ((f64, f64), f64, f64) {
        let (dx, dy) = (self.major.x - self.center.x, self.major.y - self.center.y);
        let a = dx.hypot(dy);
        let u = if a > 0.0 {
            (dx / a, dy / a)
        } else {
            (1.0, 0.0)
        };
        (u, a, self.minor.abs())
    }

    /// Length of the semi-axis towards `major`.
    pub fn major_radius(&self) -> f64 {
        self.center.distance(self.major)
    }

    /// Whether both semi-axes have a length.
    pub fn is_valid(&self) -> bool {
        let (_, a, b) = self.frame();
        a > LINEAR_TOLERANCE && b > LINEAR_TOLERANCE && a.is_finite() && b.is_finite()
    }

    /// The longer semi-axis.
    pub fn largest_radius(&self) -> f64 {
        let (_, a, b) = self.frame();
        a.max(b)
    }

    /// The shorter semi-axis.
    pub fn smallest_radius(&self) -> f64 {
        let (_, a, b) = self.frame();
        a.min(b)
    }

    /// The point at eccentric angle `t`.
    pub fn at(&self, t: f64) -> Point2 {
        self.from_unit(t.cos(), t.sin())
    }

    /// The eccentric angle of `p`, a point of the ellipse; for any other point, the angle of
    /// the point of the ellipse on the line from the centre through it, as seen with both
    /// semi-axes scaled to one.
    pub fn angle_of(&self, p: Point2) -> f64 {
        let (x, y) = self.to_unit(p);
        y.atan2(x)
    }

    /// `p` in the ellipse's own coordinates, in units of its semi-axes: the ellipse is the
    /// unit circle there.
    pub(crate) fn to_unit(&self, p: Point2) -> (f64, f64) {
        let ((ux, uy), a, b) = self.frame();
        let (dx, dy) = (p.x - self.center.x, p.y - self.center.y);
        ((dx * ux + dy * uy) / a, (dy * ux - dx * uy) / b)
    }

    /// The point at `(x, y)` in the ellipse's own coordinates (see [`Self::to_unit`]).
    pub(crate) fn from_unit(&self, x: f64, y: f64) -> Point2 {
        let ((ux, uy), a, b) = self.frame();
        let (along, across) = (a * x, b * y);
        Point2::new(
            self.center.x + along * ux - across * uy,
            self.center.y + along * uy + across * ux,
        )
    }

    /// How far `p` lies outside the ellipse (negative inside). Near the curve it is about
    /// the distance to it, and for a circle exactly half of `(d² − r²) / r`.
    pub(crate) fn offset(&self, p: Point2) -> f64 {
        let (_, a, b) = self.frame();
        let (x, y) = self.to_unit(p);
        (x * x + y * y - 1.0) * a * b / (a + b)
    }

    /// How far the ellipse reaches from its centre along `(wx, wy)`, scaled by that
    /// direction's length.
    pub(crate) fn reach(&self, wx: f64, wy: f64) -> f64 {
        let ((ux, uy), a, b) = self.frame();
        (a * (ux * wx + uy * wy)).hypot(b * (ux * wy - uy * wx))
    }

    /// The eccentric angle where the ellipse reaches furthest along `(wx, wy)`; the opposite
    /// angle reaches furthest the other way.
    pub(crate) fn furthest_along(&self, wx: f64, wy: f64) -> f64 {
        let ((ux, uy), a, b) = self.frame();
        (b * (ux * wy - uy * wx)).atan2(a * (ux * wx + uy * wy))
    }

    /// Whether `other` is the same curve, within `tolerance`: the same centre, and each
    /// ellipse through the other's axis ends and a point between them, which two different
    /// concentric ellipses never share all of.
    pub(crate) fn same_as(&self, other: &Ellipse2, tolerance: f64) -> bool {
        let on = |e: &Ellipse2, p: Point2| e.offset(p).abs() <= tolerance;
        let eighth = TAU / 8.0;
        self.center.distance(other.center) <= tolerance
            && [0.0, eighth, 2.0 * eighth]
                .into_iter()
                .all(|t| on(self, other.at(t)) && on(other, self.at(t)))
    }

    /// Points of the ellipse where `g`, a signed measure that is about the distance to
    /// another curve near it, changes sign, or comes within `tolerance` of zero where the
    /// curves touch.
    pub(crate) fn zeros_of(&self, g: impl Fn(Point2) -> f64, tolerance: f64) -> Vec<Point2> {
        let f = |t: f64| g(self.at(t));
        let step = TAU / SCAN_STEPS as f64;
        let samples: Vec<(f64, f64)> = (0..=SCAN_STEPS)
            .map(|k| {
                let t = step * k as f64;
                (t, f(t))
            })
            .collect();
        let mut out = Vec::new();
        for pair in samples.windows(2) {
            let &[(t0, v0), (t1, v1)] = pair else {
                continue;
            };
            if v0 == 0.0 {
                out.push(self.at(t0));
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
                out.push(self.at(0.5 * (lo + hi)));
            }
        }
        // Touches: a smallest |g| between its neighbours, brought down by golden section.
        for triple in samples.windows(3) {
            let &[(t0, v0), (_, v1), (t2, v2)] = triple else {
                continue;
            };
            if v1.abs() <= v0.abs() && v1.abs() <= v2.abs() {
                let t = smallest(|t| f(t).abs(), t0, t2);
                if f(t).abs() <= tolerance {
                    out.push(self.at(t));
                }
            }
        }
        // The scan's seam at angle 0 is a neighbour pair too.
        if let (Some(&(_, v1)), Some(&(_, v0)), Some(&(_, v2))) =
            (samples.first(), samples.get(SCAN_STEPS - 1), samples.get(1))
            && v1.abs() <= v0.abs()
            && v1.abs() <= v2.abs()
        {
            let t = smallest(|t| f(t).abs(), -step, step);
            if f(t).abs() <= tolerance {
                out.push(self.at(t));
            }
        }
        out
    }
}

/// Where `f` is smallest on `[lo, hi]`, by golden-section search: it has one smallest value
/// there, between two scan steps.
fn smallest(f: impl Fn(f64) -> f64, mut lo: f64, mut hi: f64) -> f64 {
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

/// Whether angle `t` lies on the arc from `start` turning by `sweep` (positive =
/// counter-clockwise).
pub(crate) fn within(t: f64, start: f64, sweep: f64) -> bool {
    let turned = if sweep >= 0.0 {
        (t - start).rem_euclid(TAU)
    } else {
        (start - t).rem_euclid(TAU)
    };
    turned <= sweep.abs()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    fn tilted() -> Ellipse2 {
        // Semi-axes 5 along (3, 4)/5 and 2 across, around (1, -2).
        Ellipse2 {
            center: Point2::new(1.0, -2.0),
            major: Point2::new(4.0, 2.0),
            minor: 2.0,
        }
    }

    #[test]
    fn angles_and_points_round_trip() {
        let e = tilted();
        assert!(e.at(0.0).distance(e.major) < 1e-12);
        // A quarter turn counter-clockwise from the major direction (3, 4)/5 is (-4, 3)/5.
        assert!(e.at(PI / 2.0).distance(Point2::new(1.0 - 1.6, -2.0 + 1.2)) < 1e-12);
        for k in 0..12 {
            let t = -PI + 0.5 * k as f64 + 0.1;
            assert!((e.angle_of(e.at(t)) - t).abs() < 1e-12);
            assert!(e.offset(e.at(t)).abs() < 1e-12);
        }
        assert!(e.offset(e.center) < 0.0);
        assert!(e.offset(Point2::new(20.0, 0.0)) > 0.0);
    }

    #[test]
    fn reach_is_exact() {
        let e = tilted();
        for (wx, wy) in [(1.0, 0.0), (0.0, 1.0), (0.6, -0.8), (2.0, 1.0)] {
            let t = e.furthest_along(wx, wy);
            let p = e.at(t);
            let along = (p.x - e.center.x) * wx + (p.y - e.center.y) * wy;
            assert!((along - e.reach(wx, wy)).abs() < 1e-12);
            // Nothing sampled reaches further.
            for k in 0..360 {
                let q = e.at(TAU * k as f64 / 360.0);
                assert!((q.x - e.center.x) * wx + (q.y - e.center.y) * wy <= along + 1e-12);
            }
        }
    }

    #[test]
    fn a_circle_crossing_an_ellipse_is_found_where_it_crosses() {
        // Unit-axis ellipse 4 by 2 and a circle of radius 3 around the same centre: they
        // cross where x²/16 + y²/4 = 1 and x² + y² = 9, so x² = 20/3.
        let e = Ellipse2 {
            center: Point2::ORIGIN,
            major: Point2::new(4.0, 0.0),
            minor: 2.0,
        };
        let points = e.zeros_of(|p| p.distance(Point2::ORIGIN) - 3.0, 1e-3);
        let x = (20.0_f64 / 3.0).sqrt();
        for (sx, sy) in [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)] {
            let want = Point2::new(sx * x, sy * (9.0 - x * x).sqrt());
            assert!(points.iter().any(|p| p.distance(want) < 1e-9), "{want:?}");
        }
        // A circle of radius 2 touches it at the ends of the minor axis, at angle π/2 and
        // −π/2, and crosses it nowhere else.
        let touching = e.zeros_of(|p| p.distance(Point2::ORIGIN) - 2.0, 1e-6);
        assert!(
            touching
                .iter()
                .any(|p| p.distance(Point2::new(0.0, 2.0)) < 1e-6)
        );
        assert!(
            touching
                .iter()
                .any(|p| p.distance(Point2::new(0.0, -2.0)) < 1e-6)
        );
        assert!(touching.iter().all(|p| p.x.abs() < 1e-3));
        // A circle of radius 1 lies inside it.
        assert!(
            e.zeros_of(|p| p.distance(Point2::ORIGIN) - 1.0, 1e-3)
                .is_empty()
        );
    }

    #[test]
    fn the_same_ellipse_is_recognised_whichever_axis_it_is_named_by() {
        let e = tilted();
        let across = Ellipse2 {
            center: e.center,
            major: e.at(PI / 2.0),
            minor: 5.0,
        };
        assert!(e.same_as(&across, 1e-9));
        assert!(e.same_as(&e, 1e-9));
        let turned = Ellipse2 {
            major: e.at(0.3),
            ..e
        };
        assert!(!e.same_as(&turned, 1e-9));
        assert!(within(0.5, 0.0, 1.0));
        assert!(!within(1.5, 0.0, 1.0));
        assert!(within(-0.5, 0.0, -1.0));
        assert!(within(PI, 3.0, 0.5));
    }
}
