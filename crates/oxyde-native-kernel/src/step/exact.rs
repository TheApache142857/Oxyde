//! B-spline curves and surfaces that are exactly lines, circles, planes, cylinders, cones,
//! spheres or tori (as rational B-splines of degree two carry circles exactly), told as
//! what they are: within a billionth of their size at every point looked at, which an
//! approximation (a polynomial following a circle to 1e-7) is not.

use oxyde_geom::{Axis, Plane, Point3, Vec3};

use crate::geometry::{Circle, Surface, perpendicular};
use crate::nurbs::{BSplineCurve, BSplineSurface};
use crate::revolution::{Meridian, Revolution};

/// How far from what it is taken for a curve or a surface may be, for its size.
fn tolerance(size: f64) -> f64 {
    1e-9 * size.max(1.0)
}

/// Parameters across a domain: its ends, its knots, and points between each.
fn params(knots: &[f64], (a, b): (f64, f64), per: usize) -> Vec<f64> {
    let mut breaks: Vec<f64> = vec![a];
    for &k in knots {
        if k > a && k < b && breaks.last().is_none_or(|&l| k > l) {
            breaks.push(k);
        }
    }
    breaks.push(b);
    let mut out = Vec::new();
    for w in breaks.windows(2) {
        for i in 0..per {
            out.push(w[0] + (w[1] - w[0]) * i as f64 / per as f64);
        }
    }
    out.push(b);
    out
}

fn size_of(points: &[Point3]) -> f64 {
    points
        .iter()
        .map(|p| p.x.abs().max(p.y.abs()).max(p.z.abs()))
        .fold(0.0, f64::max)
}

/// The circle through three points, if they are not in a line: its centre, its normal
/// (`(b − a) × (c − a)`) and its radius.
fn circumcircle(a: Point3, b: Point3, c: Point3) -> Option<(Point3, Vec3, f64)> {
    let (u, v) = (b - a, c - a);
    let n = u.cross(v);
    let nn = n.dot(n);
    if nn <= 1e-24 * (u.dot(u) * v.dot(v)).max(1e-300) {
        return None;
    }
    // a + (|u|² (v × n) + |v|² (n × u)) / (2 |n|²)
    let w = (v.cross(n) * u.dot(u) + n.cross(u) * v.dot(v)) * (1.0 / (2.0 * nn));
    let centre = a + w;
    Some((centre, n.normalized()?, w.length()))
}

/// What a B-spline curve is exactly, when it is a line or a circle.
pub(crate) enum ExactCurve {
    Line,
    /// The circle, its normal the way the curve's parameter runs round it.
    Circle(Circle),
}

pub(crate) fn curve(c: &BSplineCurve) -> Option<ExactCurve> {
    let size = size_of(&c.poles);
    let tol = tolerance(size);
    let (a, b) = c.domain();
    let ts = params(&c.knots, (a, b), 8);
    let points: Vec<Point3> = ts.iter().map(|&t| c.point(t)).collect();
    let first = c.poles[0];
    let last = c.poles[c.poles.len() - 1];
    // A line: every pole on the line through the first and the last, in order along it.
    if let Some(d) = (last - first).normalized() {
        let off = |p: &Point3| {
            let q = *p - first;
            (q - d * q.dot(d)).length()
        };
        let along: Vec<f64> = c.poles.iter().map(|p| (*p - first).dot(d)).collect();
        if c.poles.iter().all(|p| off(p) <= tol) && along.windows(2).all(|w| w[1] >= w[0] - tol) {
            return Some(ExactCurve::Line);
        }
    }
    // A circle: through three of its points, every other on it, in its plane.
    let n = points.len();
    let (centre, normal, radius) = circumcircle(points[0], points[n / 3], points[2 * n / 3])?;
    if radius > 1e6 * size.max(1.0) {
        return None;
    }
    let on = |p: &Point3| {
        let q = *p - centre;
        let h = q.dot(normal);
        ((q - normal * h).length() - radius).abs().max(h.abs())
    };
    if !points.iter().all(|p| on(p) <= tol) {
        return None;
    }
    // Its normal the way the parameter runs: counter-clockwise about it.
    let d = c.derivatives(a, 1)[1];
    let normal = if normal.cross(points[0] - centre).dot(d) >= 0.0 {
        normal
    } else {
        -normal
    };
    Some(ExactCurve::Circle(Circle {
        center: centre,
        normal,
        radius,
    }))
}

/// What a B-spline surface is exactly, when it is a plane, a cylinder, a cone, a sphere or
/// a torus, its normal along `S_u × S_v`.
pub(crate) fn surface(s: &BSplineSurface) -> Option<Surface> {
    let size = s.size();
    let tol = tolerance(size);
    let us = params(&s.knots[0], s.domain(0), 4);
    let vs = params(&s.knots[1], s.domain(1), 4);
    let mut grid = Vec::with_capacity(us.len() * vs.len());
    for &u in &us {
        for &v in &vs {
            grid.push(s.point(u, v));
        }
    }
    // The normal S_u × S_v at the middle of the domain, the way the surface faces.
    let (um, vm) = (mid(s.domain(0)), mid(s.domain(1)));
    let d = s.derivatives(um, vm, 1);
    let facing = d[1][0].cross(d[0][1]).normalized()?;
    let at = s.point(um, vm);
    // A plane: every point off it by no more than the tolerance.
    if grid.iter().all(|p| (*p - at).dot(facing).abs() <= tol) {
        return Some(Surface::Plane(Plane {
            origin: at,
            x_dir: perpendicular(facing),
            normal: facing,
        }));
    }
    for dir in 0..2 {
        if let Some(rev) = revolution(s, dir, tol) {
            let ok = grid.iter().all(|p| rev.distance(*p) <= tol);
            if ok {
                let outward = rev.normal_at(at).dot(facing) > 0.0;
                return Some(Surface::Revolution(Revolution { outward, ..rev }));
            }
        }
    }
    None
}

fn mid((a, b): (f64, f64)) -> f64 {
    0.5 * (a + b)
}

/// The surface of revolution a B-spline surface is, when its curves along `dir` are
/// circles on one axis: that axis, and its meridian from the curve across them.
fn revolution(s: &BSplineSurface, dir: usize, tol: f64) -> Option<Revolution> {
    let (lo, hi) = s.domain(1 - dir);
    // Circles across the middle of the domain, clear of its ends (where a pole is).
    let circles: Vec<Circle> = [0.3, 0.5, 0.7]
        .iter()
        .map(|f| match curve(&s.iso(dir, lo + (hi - lo) * f))? {
            ExactCurve::Circle(c) => Some(c),
            ExactCurve::Line => None,
        })
        .collect::<Option<_>>()?;
    let a = circles[0].normal;
    let o = circles[0].center;
    for c in &circles[1..] {
        if c.normal.cross(a).length() > 1e-9 || (c.center - o).cross(a).length() > tol {
            return None;
        }
    }
    // The meridian: the curve across the circles, as (ρ, z) about the axis.
    let (clo, chi) = s.domain(dir);
    let across = s.iso(1 - dir, mid((clo, chi)));
    let (alo, ahi) = across.domain();
    let ts = params(&across.knots, (alo, ahi), 8);
    let rz: Vec<(f64, f64)> = ts
        .iter()
        .map(|&t| {
            let q = across.point(t) - o;
            let z = q.dot(a);
            ((q - a * z).length(), z)
        })
        .collect();
    let n = rz.len();
    let x_dir = perpendicular(a);
    let axis = |z0: f64| Axis {
        origin: o + a * z0,
        direction: a,
    };
    // A line: ρ = r + k z, from its two ends.
    let (p, q) = (rz[0], rz[n - 1]);
    if (q.1 - p.1).abs() > tol {
        let k = (q.0 - p.0) / (q.1 - p.1);
        if rz
            .iter()
            .all(|&(r, z)| (r - (p.0 + k * (z - p.1))).abs() <= tol)
        {
            // Its origin where the line's radius is the face's (not across the apex).
            return Some(Revolution {
                axis: axis(p.1),
                x_dir,
                meridian: Meridian::Line {
                    radius: p.0,
                    slope: if k.abs() <= 1e-12 { 0.0 } else { k },
                },
                outward: true,
            });
        }
    }
    // A circle in the half-plane: through three of its points.
    let to3 = |(r, z): (f64, f64)| Point3::new(r, z, 0.0);
    let (c, _, radius) = circumcircle(to3(rz[0]), to3(rz[n / 2]), to3(rz[n - 1]))?;
    if !rz
        .iter()
        .all(|&(r, z)| ((r - c.x).hypot(z - c.y) - radius).abs() <= tol)
    {
        return None;
    }
    let center = if c.x.abs() <= tol { 0.0 } else { c.x };
    Some(Revolution {
        axis: axis(c.y),
        x_dir,
        meridian: Meridian::Circle { center, radius },
        outward: true,
    })
}
