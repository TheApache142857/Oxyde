//! The curves and surfaces of the native kernel: lines, circles and the points that stand
//! for an edge at a cone's apex or a sphere's pole; planes and surfaces of revolution
//! (cylinders, cones, spheres and tori, [`crate::revolution`]).
//!
//! An edge on a circle runs counter-clockwise about the circle's normal, from its start
//! vertex to its end vertex; a closed circle starts and ends at one vertex. A face on a
//! surface of revolution is a region of the surface's `(θ, v)` plane, bounded (in what the
//! kernel builds so far) by lines of constant `θ` and lines of constant `v`, so that its area
//! and mass integrals come from its boundary alone.

use std::f64::consts::TAU;

use oxyde_geom::{Plane, Point3, Transform, Vec3};

use crate::revolution::{Meridian, Revolution};

/// A unit vector square to `normal`, the same for the same normal: the angle-zero direction
/// of a circle that has none of its own.
pub(crate) fn perpendicular(normal: Vec3) -> Vec3 {
    // Crossed with the model axis it is least aligned with, so the result is never short.
    let (x, y, z) = (normal.x.abs(), normal.y.abs(), normal.z.abs());
    let other = if x <= y && x <= z {
        Vec3::X
    } else if y <= z {
        Vec3::Y
    } else {
        Vec3::Z
    };
    normal.cross(other).normalized().unwrap_or(Vec3::X)
}

/// What an edge runs along.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Curve {
    /// Straight from its start vertex to its end vertex.
    Line,
    Circle(Circle),
    /// No curve at all: an edge of no length at a point where a surface of revolution meets
    /// its axis (a cone's apex, a sphere's pole), which its face's boundary runs along in the
    /// face's `(θ, v)` plane as it turns about the axis there, as OpenCASCADE's degenerate
    /// edges do. It turns `turn` radians (in `(0, 2π]`) counter-clockwise about `axis`
    /// from its start to its end, which are one vertex.
    Degenerate {
        axis: Vec3,
        turn: f64,
    },
    /// Where a cylinder or a cone meets another surface, followed about the cylinder's or
    /// the cone's axis ([`Section`]).
    Section(Section),
}

impl Curve {
    /// The curve moved by `transform`. A mirror turns the way round, so a circle's normal
    /// and a degenerate edge's axis are reversed to keep an edge running from the same start
    /// to the same end.
    pub fn transformed(&self, transform: &Transform) -> Curve {
        match self {
            Curve::Line => Curve::Line,
            Curve::Circle(circle) => Curve::Circle(circle.transformed(transform)),
            Curve::Degenerate { axis, turn } => {
                let axis = transform.apply_vector(*axis);
                Curve::Degenerate {
                    axis: if transform.is_mirror() { -axis } else { axis },
                    turn: *turn,
                }
            }
            Curve::Section(section) => Curve::Section(section.transformed(transform)),
        }
    }

    /// The same curve run the other way: a line as it is, a circle about its reversed
    /// normal, a degenerate edge about its reversed axis.
    pub fn reversed(&self) -> Curve {
        match self {
            Curve::Line => Curve::Line,
            Curve::Circle(c) => Curve::Circle(Circle {
                normal: -c.normal,
                ..*c
            }),
            Curve::Degenerate { axis, turn } => Curve::Degenerate {
                axis: -*axis,
                turn: *turn,
            },
            Curve::Section(section) => Curve::Section(Section {
                ccw: !section.ccw,
                ..*section
            }),
        }
    }

    pub fn is_degenerate(&self) -> bool {
        matches!(self, Curve::Degenerate { .. })
    }

    /// How far an edge on this curve from `start` to `end` runs: its length for a line, the
    /// angle it turns through for a circle or a section (a whole turn when `closed`), a
    /// degenerate edge's turn.
    pub fn extent(&self, start: Point3, end: Point3, closed: bool) -> f64 {
        match self {
            Curve::Line => start.distance(end),
            Curve::Circle(c) => c.sweep(start, end, closed),
            Curve::Section(s) => s.sweep(start, end, closed),
            Curve::Degenerate { turn, .. } => *turn,
        }
    }

    /// The point `s` along an edge on this curve from `start` to `end` (`s` measured as
    /// [`Curve::extent`] measures it), `None` off a section's reach.
    pub fn at(&self, start: Point3, end: Point3, s: f64) -> Option<Point3> {
        match self {
            Curve::Line => {
                let d = end - start;
                let l = d.length();
                Some(if l > 0.0 { start + d * (s / l) } else { start })
            }
            Curve::Circle(c) => Some(c.point_at(c.angle_of(start) + s)),
            Curve::Section(sec) => sec.along(start, s),
            Curve::Degenerate { .. } => Some(start),
        }
    }

    /// Where `p`, a point on an edge on this curve from `start`, is along it, as
    /// [`Curve::extent`] measures it (in `[0, 2π)` round a circle or a section).
    pub fn position(&self, start: Point3, end: Point3, p: Point3) -> f64 {
        match self {
            Curve::Line => {
                let d = end - start;
                let l = d.length();
                if l > 0.0 { (p - start).dot(d) / l } else { 0.0 }
            }
            Curve::Circle(c) => (c.angle_of(p) - c.angle_of(start)).rem_euclid(TAU),
            Curve::Section(sec) => sec.turned(start, p),
            Curve::Degenerate { .. } => 0.0,
        }
    }

    /// How far `p` lies off the curve (an edge's line taken whole, its point for a
    /// degenerate edge at `start`).
    pub fn distance(&self, start: Point3, end: Point3, p: Point3) -> f64 {
        match self {
            Curve::Line => {
                let d = end - start;
                let l2 = d.dot(d);
                if l2 == 0.0 {
                    return p.distance(start);
                }
                let t = (p - start).dot(d) / l2;
                p.distance(start + d * t)
            }
            Curve::Circle(c) => c.distance(p),
            Curve::Section(s) => s.distance(p),
            Curve::Degenerate { .. } => p.distance(start),
        }
    }

    /// Points of an edge on this curve from `start` to `end`, both ends included, no chord
    /// further than `deflection` from it and none turning more than a twelfth of a turn: a
    /// line's two ends, an arc's at equal steps, a section's halved until close enough, a
    /// degenerate edge's one point.
    pub fn polyline(
        &self,
        start: Point3,
        end: Point3,
        closed: bool,
        deflection: f64,
    ) -> Vec<Point3> {
        let extent = self.extent(start, end, closed);
        let d = deflection.max(1e-7);
        match self {
            Curve::Line => vec![start, end],
            Curve::Degenerate { .. } => vec![start],
            Curve::Circle(c) => {
                let step = if d < c.radius {
                    (2.0 * (1.0 - d / c.radius).acos()).min(TAU / 12.0)
                } else {
                    TAU / 12.0
                };
                let n = ((extent / step).ceil() as usize).max(if closed { 3 } else { 1 });
                let mut points: Vec<Point3> = (0..n)
                    .map(|i| {
                        if i == 0 {
                            start
                        } else {
                            c.point_at(c.angle_of(start) + extent * i as f64 / n as f64)
                        }
                    })
                    .collect();
                points.push(end);
                points
            }
            Curve::Section(sec) => sec.polyline(start, end, extent, d),
        }
    }
}

/// Where a cylinder or a cone (the carrier) meets another surface, as a curve about the
/// carrier's axis: at each angle `θ` about it, the point where the carrier's straight line
/// at `θ` meets the other surface, the `branch`-th of those points along the line (a plane
/// meets it once, a cylinder, a cone or a sphere up to twice). An edge on it runs as `θ`
/// grows (counter-clockwise about the carrier's axis) when `ccw`, the other way when not,
/// as an edge on a circle runs about its normal. A plane slanted to a cylinder makes an
/// ellipse; two crossing cylinders make the loops where one goes through the other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Section {
    pub carrier: Revolution,
    pub other: Surface,
    pub branch: u8,
    pub ccw: bool,
}

impl Section {
    /// The carrier's straight line at `θ`: its point at `v = 0` and its direction, `v`
    /// growing.
    fn line(&self, theta: f64) -> (Point3, Vec3) {
        let (x, y, a) = self.carrier.frame();
        let e = x * theta.cos() + y * theta.sin();
        let (radius, slope) = match self.carrier.meridian {
            Meridian::Line { radius, slope } => (radius, slope),
            Meridian::Circle { .. } => (0.0, 0.0),
        };
        (self.carrier.axis.origin + e * radius, e * slope + a)
    }

    /// Where the carrier's straight line at `θ` meets the other surface, on the carrier's
    /// own nappe, in order along it: the line's point and direction, and each `v`.
    fn crossings(&self, theta: f64) -> (Point3, Vec3, Vec<f64>) {
        let (p, d) = self.line(theta);
        let mut roots = self.other.line_hits(p, d);
        if let Meridian::Line { radius, slope } = self.carrier.meridian {
            roots.retain(|&v| radius + slope * v >= -1e-9 * (1.0 + radius.abs()));
        }
        (p, d, roots)
    }

    /// The point at angle `θ` about the carrier's axis, if the line there reaches the
    /// other surface often enough.
    pub fn point_at(&self, theta: f64) -> Option<Point3> {
        let (p, d, roots) = self.crossings(theta);
        roots.get(usize::from(self.branch)).map(|&v| p + d * v)
    }

    /// `p`'s angle about the carrier's axis.
    pub fn angle_of(&self, p: Point3) -> f64 {
        self.carrier.params(p).0
    }

    /// The angle an edge from `start` turns through to `p`, its way round, in `[0, 2π)`.
    pub fn turned(&self, start: Point3, p: Point3) -> f64 {
        let t = self.angle_of(p) - self.angle_of(start);
        if self.ccw { t } else { -t }.rem_euclid(TAU)
    }

    /// The angle an edge from `start` to `end` turns through, in `(0, 2π]`: a whole turn for
    /// a closed one.
    pub fn sweep(&self, start: Point3, end: Point3, closed: bool) -> f64 {
        if closed {
            return TAU;
        }
        let t = self.turned(start, end);
        if t <= f64::EPSILON { TAU } else { t }
    }

    /// The point `s` radians along from `start`, its way round.
    pub fn along(&self, start: Point3, s: f64) -> Option<Point3> {
        let t = self.angle_of(start);
        self.point_at(if self.ccw { t + s } else { t - s })
    }

    /// How far `p` lies off the curve, measured to its point at `p`'s angle.
    pub fn distance(&self, p: Point3) -> f64 {
        self.point_at(self.angle_of(p))
            .map_or(f64::INFINITY, |q| q.distance(p))
    }

    /// The section moved by `transform`. A mirror turns angles the other way round, so the
    /// way an edge on it runs is turned too, to keep it running from the same start to the
    /// same end.
    pub fn transformed(&self, transform: &Transform) -> Section {
        Section {
            carrier: self.carrier.transformed(transform),
            other: self.other.transformed(transform),
            branch: self.branch,
            ccw: self.ccw != transform.is_mirror(),
        }
    }

    /// Whether it is an ellipse: a plane's section of a cylinder.
    pub fn is_ellipse(&self) -> bool {
        matches!(self.other, Surface::Plane(_)) && self.carrier.cylinder_radius().is_some()
    }

    /// Whether every one of the carrier's straight lines reaches the other surface often
    /// enough, and clearly (no two of its crossings nearly one): a whole loop round the axis
    /// that this section can follow everywhere.
    pub fn goes_round(&self) -> bool {
        let n = 720;
        (0..n).all(|i| {
            let theta = TAU * f64::from(i) / f64::from(n);
            let (_, d, roots) = self.crossings(theta);
            let k = usize::from(self.branch);
            let Some(&v) = roots.get(k) else {
                return false;
            };
            !roots
                .iter()
                .enumerate()
                .any(|(j, &w)| j != k && (w - v).abs() * d.length() < 1e-4)
        })
    }

    /// Points from `start` along `extent` radians to `end`, both ends included, each chord
    /// within `deflection` of the curve: steps of at most a 48th of a turn, each halved until
    /// its middle is close enough to its chord.
    fn polyline(&self, start: Point3, end: Point3, extent: f64, deflection: f64) -> Vec<Point3> {
        let t0 = self.angle_of(start);
        let sign = if self.ccw { 1.0 } else { -1.0 };
        let at = |s: f64| self.point_at(t0 + sign * s);
        let mut out = vec![start];
        let n = ((extent / (TAU / 48.0)).ceil() as usize).max(2);
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
            let (Some(p0), Some(p1), Some(pm)) = (at(s0), at(s1), at((s0 + s1) / 2.0)) else {
                continue;
            };
            let chord = p0 + (p1 - p0) * 0.5;
            if pm.distance(chord) > deflection && s1 - s0 > 1e-6 {
                stack.push(((s0 + s1) / 2.0, s1));
                stack.push((s0, (s0 + s1) / 2.0));
            } else if s1 < extent {
                out.push(p1);
            }
        }
        out.push(end);
        out
    }

    /// Its length from `start` along `extent` radians, by Gauss–Legendre quadrature of its
    /// speed.
    pub fn length(&self, start: Point3, extent: f64) -> f64 {
        let t0 = self.angle_of(start);
        let sign = if self.ccw { 1.0 } else { -1.0 };
        let speed = |s: f64| {
            let h = 1e-6;
            match (
                self.point_at(t0 + sign * (s + h)),
                self.point_at(t0 + sign * (s - h)),
            ) {
                (Some(a), Some(b)) => a.distance(b) / (2.0 * h),
                _ => 0.0,
            }
        };
        crate::revolution::quadrature(0.0, extent, TAU / 32.0, &speed)
    }

    /// Along each model axis, the extremes an edge from `start` turning `extent` reaches
    /// between its ends: found among 256 samples and narrowed by golden sections.
    pub fn extremes(&self, start: Point3, extent: f64) -> Vec<Point3> {
        let t0 = self.angle_of(start);
        let sign = if self.ccw { 1.0 } else { -1.0 };
        let at = |s: f64| self.point_at(t0 + sign * s);
        let n = 256;
        let samples: Vec<(f64, Point3)> = (0..=n)
            .filter_map(|i| {
                let s = extent * f64::from(i) / f64::from(n);
                at(s).map(|p| (s, p))
            })
            .collect();
        let h = extent / f64::from(n);
        let mut out = Vec::new();
        for k in 0..3 {
            for side in [1.0, -1.0] {
                let value = |p: Point3| side * component(p.to_vec(), k);
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
                        let f1 = at(x1).map_or(f64::NEG_INFINITY, value);
                        let f2 = at(x2).map_or(f64::NEG_INFINITY, value);
                        if f1 < f2 {
                            lo = x1;
                        } else {
                            hi = x2;
                        }
                    }
                    if let Some(p) = at((lo + hi) / 2.0) {
                        out.push(p);
                    }
                }
            }
        }
        out
    }
}

/// A circle in space: its centre, its unit normal (an edge on it runs counter-clockwise
/// about the normal) and its radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Circle {
    pub center: Point3,
    pub normal: Vec3,
    pub radius: f64,
}

impl Circle {
    /// The circle's frame: an angle-zero direction and the one a quarter turn on, so that
    /// a point at angle `t` is `center + radius (cos t x + sin t y)`.
    pub fn frame(&self) -> (Vec3, Vec3) {
        let x = perpendicular(self.normal);
        (x, self.normal.cross(x))
    }

    pub fn angle_of(&self, p: Point3) -> f64 {
        let (x, y) = self.frame();
        let d = p - self.center;
        d.dot(y).atan2(d.dot(x))
    }

    pub fn point_at(&self, angle: f64) -> Point3 {
        let (x, y) = self.frame();
        self.center + (x * angle.cos() + y * angle.sin()) * self.radius
    }

    /// The angle an edge from `start` to `end` turns through, in `(0, 2π]`: a whole turn
    /// for a closed circle (`closed`).
    pub fn sweep(&self, start: Point3, end: Point3, closed: bool) -> f64 {
        if closed {
            return TAU;
        }
        let turn = (self.angle_of(end) - self.angle_of(start)).rem_euclid(TAU);
        if turn <= f64::EPSILON { TAU } else { turn }
    }

    /// The circle moved by `transform`. A mirror turns the way round, so the normal is
    /// reversed to keep an edge running from the same start to the same end.
    pub fn transformed(&self, transform: &Transform) -> Circle {
        let normal = transform.apply_vector(self.normal);
        Circle {
            center: transform.apply_point(self.center),
            normal: if transform.is_mirror() {
                -normal
            } else {
                normal
            },
            radius: self.radius,
        }
    }

    /// How far `p` lies off the circle.
    pub fn distance(&self, p: Point3) -> f64 {
        let d = p - self.center;
        let along = d.dot(self.normal);
        let radial = (d - self.normal * along).length();
        (along * along + (radial - self.radius).powi(2)).sqrt()
    }

    /// Along each model axis, the extremes the arc from angle `start` turning `sweep`
    /// reaches between its ends (its ends excepted: they are vertices).
    pub fn extremes(&self, start: f64, sweep: f64) -> Vec<Point3> {
        let (x, y) = self.frame();
        let mut out = Vec::new();
        for k in 0..3 {
            let (a, b) = (component(x, k), component(y, k));
            if a.abs() + b.abs() <= f64::EPSILON {
                continue;
            }
            // `a cos t + b sin t` is greatest at atan2(b, a) and least half a turn on.
            let peak = b.atan2(a);
            for t in [peak, peak + std::f64::consts::PI] {
                let along = (t - start).rem_euclid(TAU);
                if along > 0.0 && along < sweep {
                    out.push(self.point_at(t));
                }
            }
        }
        out
    }
}

pub(crate) fn component(v: Vec3, k: usize) -> f64 {
    match k {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

/// What a face lies on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Surface {
    /// Its normal points out of the solid.
    Plane(Plane),
    /// A cylinder, a cone, a sphere or a torus, saying which way its normal points.
    Revolution(Revolution),
}

impl Surface {
    pub fn plane(&self) -> Option<&Plane> {
        match self {
            Surface::Plane(plane) => Some(plane),
            Surface::Revolution(_) => None,
        }
    }

    pub fn transformed(&self, transform: &Transform) -> Surface {
        match self {
            Surface::Plane(plane) => Surface::Plane(Plane {
                origin: transform.apply_point(plane.origin),
                x_dir: transform.apply_vector(plane.x_dir),
                normal: transform.apply_vector(plane.normal),
            }),
            Surface::Revolution(r) => Surface::Revolution(r.transformed(transform)),
        }
    }

    /// The same surface with its normal the other way.
    pub fn reversed(&self) -> Surface {
        match self {
            Surface::Plane(p) => Surface::Plane(Plane {
                normal: -p.normal,
                ..*p
            }),
            Surface::Revolution(r) => Surface::Revolution(Revolution {
                outward: !r.outward,
                ..*r
            }),
        }
    }

    /// How far `p` lies off the surface.
    pub fn distance(&self, p: Point3) -> f64 {
        match self {
            Surface::Plane(plane) => (p - plane.origin).dot(plane.normal).abs(),
            Surface::Revolution(r) => r.distance(p),
        }
    }

    /// Where the line `point + t dir` meets the surface: each `t`, in increasing order.
    pub fn line_hits(&self, point: Point3, dir: Vec3) -> Vec<f64> {
        match self {
            Surface::Plane(p) => {
                let denom = dir.dot(p.normal);
                if denom.abs() < 1e-12 * dir.length() {
                    return Vec::new();
                }
                vec![(p.origin - point).dot(p.normal) / denom]
            }
            Surface::Revolution(r) => r.ray_hits(point, dir),
        }
    }
}

/// The area and mass integrals of one face: its area, `∫ x dA`, and, about a reference
/// point `r`, `∫ (x − r)·n dA` and `∫ (x − r) ((x − r)·n) dA`, from which the divergence
/// theorem gives a solid's volume (a third of the sum of the second) and first moment (a
/// quarter of the sum of the third).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FaceIntegrals {
    pub area: f64,
    pub first: Vec3,
    pub flux: f64,
    pub flux_moment: Vec3,
}

impl FaceIntegrals {
    /// The centroid, or `fallback` for a face of no area.
    pub fn centroid(&self, fallback: Point3) -> Point3 {
        if self.area.abs() > 0.0 {
            let c = self.first * (1.0 / self.area);
            Point3::new(c.x, c.y, c.z)
        } else {
            fallback
        }
    }

    /// Of a planar face of area `area` and first moment `first` on `plane`, about
    /// `reference`: `(x − r)·n` is the same all over a plane.
    pub fn of_plane(plane: &Plane, area: f64, first: Vec3, reference: Point3) -> Self {
        let d = (plane.origin - reference).dot(plane.normal);
        FaceIntegrals {
            area,
            first,
            flux: d * area,
            flux_moment: (first - reference.to_vec() * area) * d,
        }
    }
}

/// Green's theorem in a plane's own coordinates: twice the signed area, and `∮ x² dy` and
/// `∮ y² dx` (so that `∫∫ x = ∮ x² dy / 2` and `∫∫ y = -∮ y² dx / 2`), added up edge by
/// edge as a loop runs.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PlaneSums {
    pub twice_area: f64,
    pub x2_dy: f64,
    pub y2_dx: f64,
}

impl PlaneSums {
    /// A straight edge from `(px, py)` to `(qx, qy)`.
    pub fn add_line(&mut self, (px, py): (f64, f64), (qx, qy): (f64, f64)) {
        self.twice_area += px * qy - qx * py;
        self.x2_dy += (qy - py) * (px * px + px * qx + qx * qx) / 3.0;
        self.y2_dx += (qx - px) * (py * py + py * qy + qy * qy) / 3.0;
    }

    /// An arc of the circle centred on `(cx, cy)` of radius `r`, from angle `t0` to `t1`
    /// (`t1 < t0` turning clockwise).
    pub fn add_arc(&mut self, (cx, cy): (f64, f64), r: f64, t0: f64, t1: f64) {
        let (s0, c0) = t0.sin_cos();
        let (s1, c1) = t1.sin_cos();
        self.twice_area += r * cx * (s1 - s0) - r * cy * (c1 - c0) + r * r * (t1 - t0);
        // x = cx + r cos t, dy = r cos t dt.
        let cos2 = |t: f64| t / 2.0 + (2.0 * t).sin() / 4.0;
        let cos3 = |s: f64| s - s * s * s / 3.0;
        self.x2_dy += cx * cx * r * (s1 - s0)
            + 2.0 * cx * r * r * (cos2(t1) - cos2(t0))
            + r * r * r * (cos3(s1) - cos3(s0));
        // y = cy + r sin t, dx = -r sin t dt.
        let sin2 = |t: f64| t / 2.0 - (2.0 * t).sin() / 4.0;
        let sin3 = |c: f64| -c + c * c * c / 3.0;
        self.y2_dx -= cy * cy * r * (-c1 + c0)
            + 2.0 * cy * r * r * (sin2(t1) - sin2(t0))
            + r * r * r * (sin3(c1) - sin3(c0));
    }

    /// A curved edge in the plane, `point(s)` its point `s` along it in the plane's
    /// coordinates for `s` from 0 to `extent`: the same sums by Gauss–Legendre quadrature,
    /// the derivatives from points a hair either side.
    pub fn add_path(&mut self, extent: f64, point: &impl Fn(f64) -> (f64, f64)) {
        let h = 1e-6 * extent.max(1e-9);
        let d = |s: f64| {
            let (a, b) = (point(s + h), point(s - h));
            ((a.0 - b.0) / (2.0 * h), (a.1 - b.1) / (2.0 * h))
        };
        let piece = extent / 32.0;
        self.twice_area += crate::revolution::quadrature(0.0, extent, piece, &|s| {
            let ((x, y), (dx, dy)) = (point(s), d(s));
            x * dy - y * dx
        });
        self.x2_dy += crate::revolution::quadrature(0.0, extent, piece, &|s| {
            let (x, _) = point(s);
            x * x * d(s).1
        });
        self.y2_dx += crate::revolution::quadrature(0.0, extent, piece, &|s| {
            let (_, y) = point(s);
            y * y * d(s).0
        });
    }

    /// The area and `∫ (x, y) dA` in the plane's coordinates.
    pub fn area_and_moment(&self) -> (f64, f64, f64) {
        (self.twice_area / 2.0, self.x2_dy / 2.0, -self.y2_dx / 2.0)
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    #[test]
    fn a_disc_and_a_half_disc_have_their_areas_and_centroids() {
        let mut disc = PlaneSums::default();
        disc.add_arc((1.0, 2.0), 3.0, 0.0, TAU);
        let (area, mx, my) = disc.area_and_moment();
        assert!((area - 9.0 * PI).abs() < 1e-12);
        assert!((mx / area - 1.0).abs() < 1e-12 && (my / area - 2.0).abs() < 1e-12);
        // The upper half of the unit disc: its centroid is 4 / 3π above the centre.
        let mut half = PlaneSums::default();
        half.add_arc((0.0, 0.0), 1.0, 0.0, PI);
        half.add_line((-1.0, 0.0), (1.0, 0.0));
        let (area, mx, my) = half.area_and_moment();
        assert!((area - PI / 2.0).abs() < 1e-12);
        assert!(mx.abs() < 1e-12);
        assert!((my / area - 4.0 / (3.0 * PI)).abs() < 1e-12);
        // Run clockwise, the same region is negative.
        let mut back = PlaneSums::default();
        back.add_line((1.0, 0.0), (-1.0, 0.0));
        back.add_arc((0.0, 0.0), 1.0, PI, 0.0);
        assert!((back.area_and_moment().0 + PI / 2.0).abs() < 1e-12);
    }

    #[test]
    fn an_arc_reaches_its_extremes_only_between_its_ends() {
        let circle = Circle {
            center: Point3::ORIGIN,
            normal: Vec3::Z,
            radius: 1.0,
        };
        let start = circle.angle_of(Point3::new(1.0, 0.0, 0.0));
        // A quarter turn from +x to +y reaches no extreme between its ends.
        assert!(circle.extremes(start, PI / 2.0).is_empty());
        // Three quarters, from +x to -y, pass +y and -x.
        let points = circle.extremes(start, 1.5 * PI);
        assert_eq!(points.len(), 2);
    }
}
