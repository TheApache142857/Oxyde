//! The curves and surfaces of the native kernel: lines and circles, planes and cylinders.
//!
//! An edge on a circle runs counter-clockwise about the circle's normal, from its start
//! vertex to its end vertex; a closed circle starts and ends at one vertex. A cylinder has
//! an angle-zero direction across its axis, so that a point of it has an angle about the
//! axis and a height along it: its faces are regions of that `(angle, height)` plane,
//! bounded (in what the kernel builds so far) by lines along the axis and circles across
//! it, so that their area, centroid and mass integrals have closed forms.

use std::f64::consts::TAU;

use oxyde_geom::{Axis, LINEAR_TOLERANCE, Plane, Point3, Transform, Vec3};

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

fn component(v: Vec3, k: usize) -> f64 {
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
    Cylinder(Cylinder),
}

impl Surface {
    pub fn plane(&self) -> Option<&Plane> {
        match self {
            Surface::Plane(plane) => Some(plane),
            Surface::Cylinder(_) => None,
        }
    }

    pub fn transformed(&self, transform: &Transform) -> Surface {
        match self {
            Surface::Plane(plane) => Surface::Plane(Plane {
                origin: transform.apply_point(plane.origin),
                x_dir: transform.apply_vector(plane.x_dir),
                normal: transform.apply_vector(plane.normal),
            }),
            Surface::Cylinder(cylinder) => Surface::Cylinder(cylinder.transformed(transform)),
        }
    }

    /// How far `p` lies off the surface.
    pub fn distance(&self, p: Point3) -> f64 {
        match self {
            Surface::Plane(plane) => (p - plane.origin).dot(plane.normal).abs(),
            Surface::Cylinder(cylinder) => (cylinder.radial(p).length() - cylinder.radius).abs(),
        }
    }
}

/// A circular cylinder. `axis.direction` is unit; `x_dir`, square to it, is angle zero, and
/// angles turn counter-clockwise about the axis. `outward`: the face's normal points away
/// from the axis (a boss) rather than towards it (a hole).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Cylinder {
    pub axis: Axis,
    pub x_dir: Vec3,
    pub radius: f64,
    pub outward: bool,
}

impl Cylinder {
    /// The frame `(x, y, axis)`, right-handed.
    pub fn frame(&self) -> (Vec3, Vec3, Vec3) {
        let a = self.axis.direction;
        (self.x_dir, a.cross(self.x_dir), a)
    }

    /// `p`'s offset from the axis, square to it.
    pub fn radial(&self, p: Point3) -> Vec3 {
        let d = p - self.axis.origin;
        d - self.axis.direction * d.dot(self.axis.direction)
    }

    /// `p`'s angle about the axis, in `(-π, π]`, and its height along it.
    pub fn coordinates(&self, p: Point3) -> (f64, f64) {
        let (x, y, a) = self.frame();
        let d = p - self.axis.origin;
        (d.dot(y).atan2(d.dot(x)), d.dot(a))
    }

    pub fn point(&self, angle: f64, height: f64) -> Point3 {
        let (x, y, a) = self.frame();
        self.axis.origin + a * height + (x * angle.cos() + y * angle.sin()) * self.radius
    }

    pub fn transformed(&self, transform: &Transform) -> Cylinder {
        Cylinder {
            axis: Axis {
                origin: transform.apply_point(self.axis.origin),
                direction: transform.apply_vector(self.axis.direction),
            },
            x_dir: transform.apply_vector(self.x_dir),
            radius: self.radius,
            outward: self.outward,
        }
    }

    /// Whether `circle` lies across this cylinder: centred on its axis, square to it and of
    /// its radius, so an edge on it is a line of constant height on the cylinder.
    pub fn holds_circle(&self, circle: &Circle) -> bool {
        let tolerance = 10.0 * LINEAR_TOLERANCE;
        self.radial(circle.center).length() <= tolerance
            && circle.normal.cross(self.axis.direction).length() <= 1e-9
            && (circle.radius - self.radius).abs() <= tolerance
    }
}

/// Integrals over a region of a cylinder's `(angle, height)` plane bounded by lines of
/// constant angle and constant height, from its boundary alone (Green's theorem: the
/// integral of `f(θ) g(h)` over the region is `∮ F(θ) g(h) dh`, `F' = f`, which only the
/// lines of constant angle contribute to). Each sum is signed by the way the boundary
/// turns: positive counter-clockwise in the plane.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AngleHeightSums {
    /// `∫∫ f(θ) g(h)` for f in 1, cos, sin, cos², sin², cos·sin (rows) and g in 1, h
    /// (columns).
    pub sums: [[f64; 2]; 6],
}

impl AngleHeightSums {
    /// Adds the boundary line at angle `angle` from height `from` to height `to`.
    pub fn add_line(&mut self, angle: f64, from: f64, to: f64) {
        let (s, c) = angle.sin_cos();
        let antiderivatives = [
            angle,
            s,
            -c,
            angle / 2.0 + (2.0 * angle).sin() / 4.0,
            angle / 2.0 - (2.0 * angle).sin() / 4.0,
            s * s / 2.0,
        ];
        let g = [to - from, (to * to - from * from) / 2.0];
        for (row, f) in self.sums.iter_mut().zip(antiderivatives) {
            row[0] += f * g[0];
            row[1] += f * g[1];
        }
    }

    pub fn get(&self, f: Trig, h: bool) -> f64 {
        self.sums
            .get(f as usize)
            .and_then(|row| row.get(usize::from(h)))
            .copied()
            .unwrap_or(0.0)
    }
}

/// The functions of the angle [`AngleHeightSums`] integrates.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Trig {
    One = 0,
    Cos = 1,
    Sin = 2,
    Cos2 = 3,
    Sin2 = 4,
    CosSin = 5,
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

    /// Of a cylinder's face whose boundary, walked as the face's loops run, gave `sums`
    /// (counter-clockwise from outside is counter-clockwise in the `(angle, height)` plane
    /// for an outward face, clockwise for an inward one), about `reference`.
    pub fn of_cylinder(cylinder: &Cylinder, sums: &AngleHeightSums, reference: Point3) -> Self {
        let (x, y, a) = cylinder.frame();
        let r = cylinder.radius;
        // The region's own integrals are the boundary's for an outward face and their
        // opposite for an inward one; the normal (±radial) brings the same sign back, so
        // the flux integrals take the boundary's sums as they are.
        let sign = if cylinder.outward { 1.0 } else { -1.0 };
        let s = |f: Trig, h: bool| sums.get(f, h);
        let area = sign * r * s(Trig::One, false);
        let first = (cylinder.axis.origin.to_vec() * s(Trig::One, false)
            + a * s(Trig::One, true)
            + (x * s(Trig::Cos, false) + y * s(Trig::Sin, false)) * r)
            * (sign * r);
        let w = cylinder.axis.origin - reference;
        let (wc, ws) = (w.dot(x), w.dot(y));
        let flux =
            r * (wc * s(Trig::Cos, false) + ws * s(Trig::Sin, false) + r * s(Trig::One, false));
        let flux_moment = (w
            * (wc * s(Trig::Cos, false) + ws * s(Trig::Sin, false) + r * s(Trig::One, false))
            + a * (wc * s(Trig::Cos, true) + ws * s(Trig::Sin, true) + r * s(Trig::One, true))
            + x * (r
                * (wc * s(Trig::Cos2, false)
                    + ws * s(Trig::CosSin, false)
                    + r * s(Trig::Cos, false)))
            + y * (r
                * (wc * s(Trig::CosSin, false)
                    + ws * s(Trig::Sin2, false)
                    + r * s(Trig::Sin, false))))
            * r;
        FaceIntegrals {
            area,
            first,
            flux,
            flux_moment,
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
    fn a_whole_cylinder_has_its_area_volume_and_centroid() {
        let cylinder = Cylinder {
            axis: Axis {
                origin: Point3::new(1.0, 2.0, 3.0),
                direction: Vec3::Z,
            },
            x_dir: Vec3::X,
            radius: 2.0,
            outward: true,
        };
        // Its region, counter-clockwise: along the bottom (angle up), up the seam at 2π,
        // back along the top, down the seam at 0. Only the seam lines count.
        let mut sums = AngleHeightSums::default();
        sums.add_line(TAU, 0.0, 5.0);
        sums.add_line(0.0, 5.0, 0.0);
        let reference = Point3::new(-4.0, 1.0, 0.5);
        let side = FaceIntegrals::of_cylinder(&cylinder, &sums, reference);
        assert!((side.area - 2.0 * PI * 2.0 * 5.0).abs() < 1e-9);
        let c = side.centroid(Point3::ORIGIN);
        assert!(c.distance(Point3::new(1.0, 2.0, 5.5)) < 1e-9);
        // The two discs closing it, and the volume by the divergence theorem.
        let bottom = Plane {
            origin: Point3::new(1.0, 2.0, 3.0),
            x_dir: Vec3::X,
            normal: -Vec3::Z,
        };
        let top = Plane {
            origin: Point3::new(1.0, 2.0, 8.0),
            x_dir: Vec3::X,
            normal: Vec3::Z,
        };
        let disc = 4.0 * PI;
        let caps = [
            FaceIntegrals::of_plane(&bottom, disc, bottom.origin.to_vec() * disc, reference),
            FaceIntegrals::of_plane(&top, disc, top.origin.to_vec() * disc, reference),
        ];
        let volume = (side.flux + caps.iter().map(|c| c.flux).sum::<f64>()) / 3.0;
        assert!((volume - 4.0 * PI * 5.0).abs() < 1e-9, "{volume}");
        let moment = (caps.iter().fold(side.flux_moment, |m, c| m + c.flux_moment)) * 0.25;
        let centre = reference + moment * (1.0 / volume);
        assert!(
            centre.distance(Point3::new(1.0, 2.0, 5.5)) < 1e-9,
            "{centre:?}"
        );
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
