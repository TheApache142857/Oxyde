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

use crate::revolution::Revolution;

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
        }
    }

    pub fn is_degenerate(&self) -> bool {
        matches!(self, Curve::Degenerate { .. })
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
