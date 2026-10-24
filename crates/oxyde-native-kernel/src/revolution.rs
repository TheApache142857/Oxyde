//! Surfaces of revolution: cylinders, cones, spheres and tori, each a meridian (a line or a
//! circle in the half-plane of distance from the axis and height along it) turned about an
//! axis.
//!
//! A point of one has an angle `θ` about the axis (counter-clockwise about its direction,
//! from its angle-zero direction `x`) and a parameter `v` along the meridian: the height
//! along the axis for a line, the angle round the circle's centre for a circle. With
//! `e(θ) = cos θ x + sin θ y`, the point is `origin + ρ(v) e(θ) + z(v) a`. Its faces are
//! regions of the `(θ, v)` plane, and the curves where two such surfaces on one axis meet,
//! or a plane square to the axis or through it meets one, are lines of constant `θ` or
//! constant `v` there: what the kernel's booleans cut faces along so far.
//!
//! The area and mass integrals of a face come from Green's theorem in `(θ, v)`: the
//! integral of `f(θ) g(v)` over the region is `∮ F(θ) g(v) dv` with `F' = f`, to which only
//! the boundary's lines of constant `θ` contribute; the `v` integrals are taken by
//! Gauss–Legendre quadrature, exact for a line's polynomials and to rounding for a circle's
//! sines and cosines.

// Indices into the quadrature rule, the integrals' rows and a loop's edges, all made here.
#![allow(clippy::indexing_slicing)]

use std::f64::consts::{PI, TAU};
use std::sync::LazyLock;

use oxyde_geom::{Axis, Point3, Transform, Vec3};
use oxyde_kernel::SurfaceKind;

use crate::geometry::{Circle, Curve, perpendicular};

/// The curve a surface of revolution turns about its axis, in the half-plane of distance
/// `ρ` from the axis and height `z` along it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Meridian {
    /// `ρ = radius + slope v`, `z = v`: a cylinder (no slope) or a cone.
    Line { radius: f64, slope: f64 },
    /// `ρ = center + radius cos v`, `z = radius sin v`: a sphere (centred on the axis) or a
    /// torus.
    Circle { center: f64, radius: f64 },
}

/// A meridian laid in the half-plane of another surface of revolution on the same axis:
/// `ρ = radius + slope z`, or the circle of `radius` centred `center` from the axis at
/// `height` along it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PlaneCurve {
    Line {
        radius: f64,
        slope: f64,
    },
    Circle {
        center: f64,
        height: f64,
        radius: f64,
    },
}

impl PlaneCurve {
    /// Whether the two are one curve.
    fn same_as(&self, other: &PlaneCurve, tolerance: f64) -> bool {
        match (*self, *other) {
            (
                PlaneCurve::Line { radius, slope },
                PlaneCurve::Line {
                    radius: r2,
                    slope: s2,
                },
            ) => (radius - r2).abs() <= tolerance && (slope - s2).abs() <= 1e-9,
            (
                PlaneCurve::Circle {
                    center,
                    height,
                    radius,
                },
                PlaneCurve::Circle {
                    center: c2,
                    height: h2,
                    radius: r2,
                },
            ) => {
                (center - c2).abs() <= tolerance
                    && (height - h2).abs() <= tolerance
                    && (radius - r2).abs() <= tolerance
            }
            _ => false,
        }
    }
}

/// A surface of revolution. `axis.direction` is unit and `x_dir`, square to it, is angle
/// zero. `outward`: the face's normal is `(z' e − ρ' a) / |…|` (away from the axis for a
/// cylinder or a cone, away from the meridian circle's centre for a sphere or a torus)
/// rather than the opposite.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Revolution {
    pub axis: Axis,
    pub x_dir: Vec3,
    pub meridian: Meridian,
    pub outward: bool,
}

/// Points on the axis closer than this to it are on it: a cone's apex, a sphere's poles.
const ON_AXIS: f64 = 1e-9;

impl Revolution {
    pub fn cylinder(axis: Axis, x_dir: Vec3, radius: f64, outward: bool) -> Self {
        Self {
            axis,
            x_dir,
            meridian: Meridian::Line { radius, slope: 0.0 },
            outward,
        }
    }

    pub fn kind(&self) -> SurfaceKind {
        match self.meridian {
            Meridian::Line { slope: 0.0, .. } => SurfaceKind::Cylinder,
            Meridian::Line { .. } => SurfaceKind::Cone,
            Meridian::Circle { center: 0.0, .. } => SurfaceKind::Sphere,
            Meridian::Circle { .. } => SurfaceKind::Torus,
        }
    }

    /// The cylinder's radius, if it is one.
    pub fn cylinder_radius(&self) -> Option<f64> {
        match self.meridian {
            Meridian::Line { radius, slope: 0.0 } => Some(radius),
            _ => None,
        }
    }

    /// The frame `(x, y, axis)`, right-handed.
    pub fn frame(&self) -> (Vec3, Vec3, Vec3) {
        let a = self.axis.direction;
        (self.x_dir, a.cross(self.x_dir), a)
    }

    /// `(ρ, z)` at `v`.
    pub fn meridian_at(&self, v: f64) -> (f64, f64) {
        match self.meridian {
            Meridian::Line { radius, slope } => (radius + slope * v, v),
            Meridian::Circle { center, radius } => (center + radius * v.cos(), radius * v.sin()),
        }
    }

    /// `(ρ', z')` at `v`.
    pub fn meridian_tangent(&self, v: f64) -> (f64, f64) {
        match self.meridian {
            Meridian::Line { slope, .. } => (slope, 1.0),
            Meridian::Circle { radius, .. } => (-radius * v.sin(), radius * v.cos()),
        }
    }

    pub fn point(&self, theta: f64, v: f64) -> Point3 {
        let (x, y, a) = self.frame();
        let (rho, z) = self.meridian_at(v);
        self.axis.origin + (x * theta.cos() + y * theta.sin()) * rho + a * z
    }

    /// `p`'s distance from the axis and height along it.
    pub fn rho_z(&self, p: Point3) -> (f64, f64) {
        let d = p - self.axis.origin;
        let z = d.dot(self.axis.direction);
        ((d - self.axis.direction * z).length(), z)
    }

    /// `p`'s angle about the axis in `(−π, π]` (0 on the axis) and its `v`: its height for
    /// a line, its angle round the meridian circle's centre in `(−π, π]` for a circle.
    pub fn params(&self, p: Point3) -> (f64, f64) {
        let (x, y, a) = self.frame();
        let d = p - self.axis.origin;
        let theta = d.dot(y).atan2(d.dot(x));
        let z = d.dot(a);
        let v = match self.meridian {
            Meridian::Line { .. } => z,
            Meridian::Circle { center, .. } => {
                let rho = (d - a * z).length();
                z.atan2(rho - center)
            }
        };
        (theta, v)
    }

    /// The normal of the surface's face at `(θ, v)`, pointing out of the solid.
    pub fn normal(&self, theta: f64, v: f64) -> Vec3 {
        let (x, y, a) = self.frame();
        let e = x * theta.cos() + y * theta.sin();
        let (dr, dz) = self.meridian_tangent(v);
        let n = (e * dz - a * dr).normalized().unwrap_or(e);
        if self.outward { n } else { -n }
    }

    /// The normal at a point of the surface (off its axis).
    pub fn normal_at(&self, p: Point3) -> Vec3 {
        let (theta, v) = self.params(p);
        self.normal(theta, v)
    }

    /// How far `p` lies off the surface.
    pub fn distance(&self, p: Point3) -> f64 {
        let (rho, z) = self.rho_z(p);
        match self.meridian {
            Meridian::Line { radius, slope } => {
                (rho - radius - slope * z).abs() / (1.0 + slope * slope).sqrt()
            }
            Meridian::Circle { center, radius } => {
                (((rho - center).powi(2) + z * z).sqrt() - radius).abs()
            }
        }
    }

    /// Whether the meridian is a whole circle that never meets the axis (a ring torus), so
    /// that `v` goes round with a period of `2π`.
    pub fn v_periodic(&self) -> bool {
        matches!(self.meridian, Meridian::Circle { center, radius } if center > radius)
    }

    /// Whether `v` is where the surface meets its axis.
    pub fn is_pole(&self, v: f64) -> bool {
        self.meridian_at(v).0.abs() <= ON_AXIS * self.size().max(1.0)
    }

    /// A length the surface is about the size of: what tolerances on it are scaled by.
    pub fn size(&self) -> f64 {
        match self.meridian {
            Meridian::Line { radius, .. } => radius.abs(),
            Meridian::Circle { center, radius } => center + radius,
        }
    }

    /// The parallel at `v`: the circle of constant `v`, counter-clockwise about the axis.
    /// `None` at a pole.
    pub fn parallel(&self, v: f64) -> Option<Circle> {
        let (rho, z) = self.meridian_at(v);
        (rho > ON_AXIS * self.size().max(1.0)).then(|| Circle {
            center: self.axis.origin + self.axis.direction * z,
            normal: self.axis.direction,
            radius: rho,
        })
    }

    /// The meridian circle at angle `θ` of a sphere or a torus, run the way `v` grows:
    /// counter-clockwise about `e(θ) × a`.
    pub fn meridian_circle(&self, theta: f64) -> Option<Circle> {
        let Meridian::Circle { center, radius } = self.meridian else {
            return None;
        };
        let (x, y, a) = self.frame();
        let e = x * theta.cos() + y * theta.sin();
        Some(Circle {
            center: self.axis.origin + e * center,
            normal: e.cross(a),
            radius,
        })
    }

    /// Whether `circle` is a parallel of this surface (centred on the axis, square to it,
    /// on the surface), and its `v` then.
    pub fn parallel_v(&self, circle: &Circle, tolerance: f64) -> Option<f64> {
        let a = self.axis.direction;
        if circle.normal.cross(a).length() > 1e-9 {
            return None;
        }
        let d = circle.center - self.axis.origin;
        if (d - a * d.dot(a)).length() > tolerance {
            return None;
        }
        let p = circle.center + perpendicular(circle.normal) * circle.radius;
        if self.distance(p) > tolerance {
            return None;
        }
        Some(self.params(p).1)
    }

    /// Whether `circle` is a meridian circle of this sphere or torus (in a plane through the
    /// axis, on the surface), and its angle `θ` then, the one whose half-plane holds the
    /// circle's centre (for a sphere, either of the two the circle runs through).
    pub fn meridian_theta(&self, circle: &Circle, tolerance: f64) -> Option<f64> {
        let Meridian::Circle { center, radius } = self.meridian else {
            return None;
        };
        let a = self.axis.direction;
        // The circle's plane holds the axis: its normal is square to the axis and the
        // axis's origin is in it.
        if circle.normal.dot(a).abs() > 1e-9
            || (self.axis.origin - circle.center).dot(circle.normal).abs() > tolerance
            || (circle.radius - radius).abs() > tolerance
        {
            return None;
        }
        let (rho, z) = self.rho_z(circle.center);
        if (rho - center).abs() > tolerance || z.abs() > tolerance {
            return None;
        }
        let e = if center > 0.0 {
            circle.center - self.axis.origin
        } else {
            a.cross(circle.normal)
        };
        let (x, y, _) = self.frame();
        Some(e.dot(y).atan2(e.dot(x)))
    }

    /// The surface moved by `transform`. A mirror turns angles the other way round, which
    /// the frame recomputed from the moved axis and angle zero takes care of; which side
    /// the normal is on stays.
    pub fn transformed(&self, transform: &Transform) -> Revolution {
        Revolution {
            axis: Axis {
                origin: transform.apply_point(self.axis.origin),
                direction: transform.apply_vector(self.axis.direction),
            },
            x_dir: transform.apply_vector(self.x_dir),
            meridian: self.meridian,
            outward: self.outward,
        }
    }

    /// The meridian in its own frame: a circle centred at the origin's height.
    fn own_curve(&self) -> PlaneCurve {
        match self.meridian {
            Meridian::Line { radius, slope } => PlaneCurve::Line { radius, slope },
            Meridian::Circle { center, radius } => PlaneCurve::Circle {
                center,
                height: 0.0,
                radius,
            },
        }
    }

    /// The other surface's meridian in this one's half-plane (heights from this one's
    /// origin, along this one's direction), when the two share their axis line.
    pub fn coaxial_meridian(&self, other: &Revolution, tolerance: f64) -> Option<PlaneCurve> {
        let (a, b) = (self.axis.direction, other.axis.direction);
        if a.cross(b).length() > 1e-9 {
            return None;
        }
        let d = other.axis.origin - self.axis.origin;
        if (d - a * d.dot(a)).length() > tolerance {
            return None;
        }
        let flip = a.dot(b) < 0.0;
        let shift = d.dot(a);
        Some(match other.meridian {
            Meridian::Line { radius, slope } => {
                // ρ = radius + slope w, with w the height along the other's axis from its
                // origin: w = ±(z − shift).
                let s = if flip { -slope } else { slope };
                PlaneCurve::Line {
                    radius: radius - s * shift,
                    slope: s,
                }
            }
            Meridian::Circle { center, radius } => PlaneCurve::Circle {
                center,
                height: shift,
                radius,
            },
        })
    }

    /// A sphere, if this is one centred on `axis`, turned about `axis` instead: the same
    /// surface, its normal the same way.
    pub fn sphere_about(&self, axis: &Axis, tolerance: f64) -> Option<Revolution> {
        let Meridian::Circle {
            center: 0.0,
            radius,
        } = self.meridian
        else {
            return None;
        };
        let d = axis.direction.normalized()?;
        let off = self.axis.origin - axis.origin;
        if (off - d * off.dot(d)).length() > tolerance {
            return None;
        }
        Some(Revolution {
            axis: Axis {
                origin: self.axis.origin,
                direction: d,
            },
            x_dir: perpendicular(d),
            meridian: Meridian::Circle {
                center: 0.0,
                radius,
            },
            outward: self.outward,
        })
    }

    /// This surface in the frame of `wanted` (its axis origin and direction, its angle
    /// zero): the same surface, its normal the same way. For a sphere or a torus `wanted`'s
    /// origin is its centre; for a cylinder or a cone any point of the axis. Itself when
    /// `wanted`'s axis is not along its own.
    pub fn reframed(&self, wanted: &Revolution) -> Revolution {
        let a = self.axis.direction;
        let d = wanted.axis.direction.normalized().unwrap_or(a);
        if a.cross(d).length() > 1e-9 {
            return *self;
        }
        let sigma = a.dot(d).signum();
        let shift = (wanted.axis.origin - self.axis.origin).dot(a);
        let meridian = match self.meridian {
            Meridian::Line { radius, slope } => Meridian::Line {
                radius: radius + slope * shift,
                slope: slope * sigma,
            },
            Meridian::Circle { center, radius } => {
                if shift.abs() > 1e-6 {
                    return *self;
                }
                Meridian::Circle { center, radius }
            }
        };
        let x_dir = (wanted.x_dir - d * wanted.x_dir.dot(d))
            .normalized()
            .unwrap_or(self.x_dir);
        Revolution {
            axis: Axis {
                origin: self.axis.origin + a * shift,
                direction: d,
            },
            x_dir,
            meridian,
            outward: self.outward,
        }
    }

    /// Whether the two lie on one surface with their normals the same way.
    pub fn same_as(&self, other: &Revolution, tolerance: f64) -> bool {
        let Some(m) = self.coaxial_meridian(other, tolerance) else {
            return false;
        };
        self.outward == other.outward && self.own_curve().same_as(&m, tolerance)
    }

    /// Where the meridians of this surface and of `other` (in this one's half-plane, as
    /// [`Revolution::coaxial_meridian`] gives it) cross, as `(ρ, z)` with `ρ > 0`: each the
    /// circle the two surfaces meet in. `None` when the meridians are one curve.
    pub fn meridian_crossings(
        &self,
        other: &PlaneCurve,
        tolerance: f64,
    ) -> Option<Vec<(f64, f64)>> {
        let own = self.own_curve();
        if own.same_as(other, tolerance) {
            return None;
        }
        let mut out = Vec::new();
        match (own, *other) {
            (
                PlaneCurve::Line {
                    radius: r1,
                    slope: s1,
                },
                PlaneCurve::Line {
                    radius: r2,
                    slope: s2,
                },
            ) => {
                if (s1 - s2).abs() > 1e-12 {
                    let z = (r2 - r1) / (s1 - s2);
                    out.push((r1 + s1 * z, z));
                }
            }
            (
                PlaneCurve::Line { radius, slope },
                PlaneCurve::Circle {
                    center,
                    height,
                    radius: r,
                },
            )
            | (
                PlaneCurve::Circle {
                    center,
                    height,
                    radius: r,
                },
                PlaneCurve::Line { radius, slope },
            ) => {
                // (radius + slope z − center)² + (z − height)² = r².
                let k = radius - center;
                let (qa, qb, qc) = (
                    1.0 + slope * slope,
                    2.0 * (slope * k - height),
                    k * k + height * height - r * r,
                );
                let disc = qb * qb - 4.0 * qa * qc;
                if disc >= -1e-12 * qb.abs().max(1.0).powi(2) {
                    let root = disc.max(0.0).sqrt();
                    // Roots closer than the tolerance are the one point where the line
                    // touches the circle: their square root turns rounding into two points
                    // off it.
                    let gap = root / qa * qa.sqrt();
                    let zs = if gap > tolerance {
                        vec![(-qb - root) / (2.0 * qa), (-qb + root) / (2.0 * qa)]
                    } else {
                        vec![-qb / (2.0 * qa)]
                    };
                    for z in zs {
                        out.push((radius + slope * z, z));
                    }
                }
            }
            (
                PlaneCurve::Circle {
                    center: c1,
                    height: h1,
                    radius: r1,
                },
                PlaneCurve::Circle {
                    center: c2,
                    height: h2,
                    radius: r2,
                },
            ) => {
                let (dr, dz) = (c2 - c1, h2 - h1);
                let d = (dr * dr + dz * dz).sqrt();
                if d <= tolerance || d > r1 + r2 + tolerance || d < (r1 - r2).abs() - tolerance {
                    return Some(out);
                }
                // Along the line of centres from the first, then square to it.
                let along = (r1 * r1 - r2 * r2 + d * d) / (2.0 * d);
                let half = (r1 * r1 - along * along).max(0.0).sqrt();
                let (ur, uz) = (dr / d, dz / d);
                let (br, bz) = (c1 + ur * along, h1 + uz * along);
                if half > tolerance {
                    out.push((br - uz * half, bz + ur * half));
                    out.push((br + uz * half, bz - ur * half));
                } else {
                    out.push((br, bz));
                }
            }
        }
        out.retain(|&(rho, _)| rho > tolerance);
        Some(out)
    }

    /// The `v` of the point of the meridian at `(ρ, z)`.
    pub fn v_of(&self, rho: f64, z: f64) -> f64 {
        match self.meridian {
            Meridian::Line { .. } => z,
            Meridian::Circle { center, .. } => z.atan2(rho - center),
        }
    }

    /// Where the ray `point + t dir` meets the surface: each `t`, in increasing order.
    /// A ray tangent to the surface may or may not give its double root, which changes no
    /// count of crossings from odd to even.
    pub fn ray_hits(&self, point: Point3, dir: Vec3) -> Vec<f64> {
        let a = self.axis.direction;
        let d = point - self.axis.origin;
        let (pz, qz) = (d.dot(a), dir.dot(a));
        let (p, q) = (d - a * pz, dir - a * qz);
        match self.meridian {
            Meridian::Line { radius, slope } => {
                // |p + t q|² = (radius + slope (pz + t qz))², on the nappe where that is ≥ 0.
                let r0 = radius + slope * pz;
                let qa = q.dot(q) - slope * slope * qz * qz;
                let qb = 2.0 * (p.dot(q) - slope * qz * r0);
                let qc = p.dot(p) - r0 * r0;
                let mut ts = quadratic_roots(qa, qb, qc);
                ts.retain(|&t| radius + slope * (pz + t * qz) >= -ON_AXIS * self.size().max(1.0));
                ts
            }
            Meridian::Circle {
                center: 0.0,
                radius,
            } => {
                let w = d;
                quadratic_roots(dir.dot(dir), 2.0 * w.dot(dir), w.dot(w) - radius * radius)
            }
            Meridian::Circle { center, radius } => torus_roots(d, dir, a, center, radius),
        }
    }
}

/// Where an edge starts and ends in a surface's `(θ, v)` plane, and whether it runs
/// straight between them there (along a line of constant `θ` or constant `v`) or bends (a
/// section, a circle that is neither a parallel nor a meridian).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ParamSegment {
    pub from: (f64, f64),
    pub to: (f64, f64),
    pub straight: bool,
}

/// `raw`, an angle, moved by whole turns to be nearest `near`.
pub(crate) fn unwrap(near: f64, raw: f64) -> f64 {
    raw + TAU * ((near - raw) / TAU).round()
}

/// An edge that bends across a surface of revolution's `(θ, v)` plane, followed there: its
/// points at even steps along it, `θ` (and a ring torus's `v`) unwrapped from where it
/// starts.
#[derive(Debug, Clone)]
pub(crate) struct ParamPath {
    points: Vec<(f64, f64)>,
    sweep: f64,
}

impl ParamPath {
    /// The steps a path is followed in.
    const STEPS: usize = 128;

    pub fn follow(
        surface: &Revolution,
        edge: &WalkEdge,
        from: (f64, f64),
        near_axis: f64,
    ) -> Result<ParamPath, String> {
        let mut points = Vec::with_capacity(Self::STEPS + 1);
        points.push(from);
        let (mut theta, mut v) = from;
        for i in 1..=Self::STEPS {
            let s = edge.sweep * i as f64 / Self::STEPS as f64;
            let p = if i == Self::STEPS {
                edge.b
            } else {
                edge.curve
                    .at(edge.a, edge.b, s)
                    .ok_or_else(|| "a section followed beyond its reach".to_owned())?
            };
            let (t, w) = surface.params(p);
            if surface.rho_z(p).0 > near_axis {
                theta = unwrap(theta, t);
            }
            v = if surface.v_periodic() {
                unwrap(v, w)
            } else {
                w
            };
            points.push((theta, v));
        }
        Ok(ParamPath {
            points,
            sweep: edge.sweep,
        })
    }

    pub fn end(&self) -> (f64, f64) {
        self.points.last().copied().unwrap_or((0.0, 0.0))
    }

    /// The path's samples, from its start to its end.
    pub fn points(&self) -> &[(f64, f64)] {
        &self.points
    }

    /// `(θ, v)` of the edge's point `s` along it, unwrapped next to the path there.
    pub fn at(&self, surface: &Revolution, edge: &WalkEdge, s: f64) -> (f64, f64) {
        let f = (s / self.sweep * Self::STEPS as f64).clamp(0.0, Self::STEPS as f64);
        let k = (f.floor() as usize).min(Self::STEPS - 1);
        let (t0, v0) = self.points[k];
        let (t1, v1) = self.points[k + 1];
        let w = f - k as f64;
        let near = (t0 + (t1 - t0) * w, v0 + (v1 - v0) * w);
        let Some(p) = edge.curve.at(edge.a, edge.b, s) else {
            return near;
        };
        let (t, v) = surface.params(p);
        let theta = if surface.rho_z(p).0 > 0.0 {
            unwrap(near.0, t)
        } else {
            near.0
        };
        let v = if surface.v_periodic() {
            unwrap(near.1, v)
        } else {
            v
        };
        (theta, v)
    }
}

/// One edge of a loop as [`walk`] takes it: its curve run the way the loop runs, its ends,
/// how far it turns (a circle's angle, a degenerate edge's turn), its middle and its
/// tolerance.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WalkEdge {
    pub curve: Curve,
    pub a: Point3,
    pub b: Point3,
    pub sweep: f64,
    pub mid: Point3,
    pub tolerance: f64,
}

/// A loop on a surface of revolution laid out in its `(θ, v)` plane: for each edge, in the
/// loop's order, where it starts and ends there, `θ` (and `v` round a ring torus's tube)
/// unwrapped as the loop goes round. Lines run along the surface's straight lines, circles
/// along its parallels or meridians, degenerate edges along a pole: each is straight there.
/// The walk starts at an edge whose start is off the axis, where it has an angle.
pub(crate) fn walk(
    surface: &Revolution,
    edges: &[WalkEdge],
    near_axis: f64,
) -> Result<Vec<ParamSegment>, String> {
    let n = edges.len();
    let off_axis = |p: Point3| surface.rho_z(p).0 > near_axis;
    let start = (0..n)
        .find(|&k| !edges[k].curve.is_degenerate() && off_axis(edges[k].a))
        .or_else(|| (0..n).find(|&k| !edges[k].curve.is_degenerate()))
        .ok_or_else(|| "a loop of nothing but degenerate edges".to_owned())?;
    let first = &edges[start];
    // At the axis the angle is the edge's own, from its middle.
    let theta0 = if off_axis(first.a) {
        surface.params(first.a).0
    } else {
        surface.params(first.mid).0
    };
    let v0 = surface.params(first.a).1;
    let (mut theta, mut v) = (theta0, v0);
    let (x, y, a) = surface.frame();
    let mut out = vec![
        ParamSegment {
            from: (0.0, 0.0),
            to: (0.0, 0.0),
            straight: true,
        };
        n
    ];
    for k in 0..n {
        let index = (start + k) % n;
        let edge = &edges[index];
        let tolerance = 10.0 * edge.tolerance;
        let from = (theta, v);
        let mut straight = true;
        match edge.curve {
            Curve::Line => {
                if !matches!(surface.meridian, Meridian::Line { .. }) {
                    return Err("a straight edge on a sphere or a torus".to_owned());
                }
                if off_axis(edge.b) {
                    let (t, _) = surface.params(edge.b);
                    let off = (t - theta).rem_euclid(TAU);
                    let off = off.min(TAU - off);
                    if off * surface.rho_z(edge.b).0 > tolerance {
                        return Err(
                            "a straight edge on a surface of revolution is not along it".to_owned()
                        );
                    }
                }
                v = surface.params(edge.b).1;
            }
            Curve::Circle(circle) => {
                if surface.parallel_v(&circle, tolerance).is_some() {
                    let mut turn = edge.sweep;
                    if circle.normal.dot(a) < 0.0 {
                        turn = -turn;
                    }
                    theta += turn;
                } else if surface.meridian_theta(&circle, tolerance).is_some() {
                    // `v` grows counter-clockwise about e(θ) × a.
                    let e = x * theta.cos() + y * theta.sin();
                    let mut dv = edge.sweep;
                    if circle.normal.dot(e.cross(a)) < 0.0 {
                        dv = -dv;
                    }
                    v += dv;
                } else {
                    (theta, v) = ParamPath::follow(surface, edge, from, near_axis)?.end();
                    straight = false;
                }
            }
            Curve::Section(_) | Curve::Traced(_) | Curve::Spline(_) => {
                (theta, v) = ParamPath::follow(surface, edge, from, near_axis)?.end();
                straight = false;
            }
            Curve::Degenerate { axis, turn } => {
                let mut t = turn;
                if axis.dot(a) < 0.0 {
                    t = -t;
                }
                theta += t;
            }
        }
        out[index] = ParamSegment {
            from,
            to: (theta, v),
            straight,
        };
    }
    if (theta - theta0).abs() > 1e-6 {
        return Err("a loop winds round its surface's axis".to_owned());
    }
    if (v - v0).abs() > 1e-6 * surface.size().max(1.0) {
        return Err("a loop winds round its surface's meridian".to_owned());
    }
    Ok(out)
}

/// The real roots of `a t² + b t + c`, in increasing order (one for a linear equation).
pub(crate) fn quadratic_roots(a: f64, b: f64, c: f64) -> Vec<f64> {
    let scale = a.abs().max(b.abs()).max(c.abs());
    if scale == 0.0 {
        return Vec::new();
    }
    if a.abs() <= 1e-14 * scale {
        if b.abs() <= 1e-14 * scale {
            return Vec::new();
        }
        return vec![-c / b];
    }
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return Vec::new();
    }
    // The root that does not cancel, then the other from their product.
    let root = disc.sqrt();
    let q = -0.5 * (b + b.signum() * root);
    let (t1, t2) = if q == 0.0 { (0.0, 0.0) } else { (q / a, c / q) };
    let (lo, hi) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
    vec![lo, hi]
}

/// Where the ray `d + t dir` (from the torus's centre) meets the torus of the axis `a`,
/// centre `center` from it and tube `radius`: `(|w|² + center² − radius²)² = 4 center² ρ²`.
/// Bracketed by sampling within the torus's bounding sphere and refined by bisection.
fn torus_roots(d: Vec3, dir: Vec3, a: Vec3, center: f64, radius: f64) -> Vec<f64> {
    let len = dir.length();
    if len == 0.0 {
        return Vec::new();
    }
    let f = |t: f64| {
        let w = d + dir * t;
        let z = w.dot(a);
        let rho2 = w.dot(w) - z * z;
        let s = w.dot(w) + center * center - radius * radius;
        s * s - 4.0 * center * center * rho2
    };
    // The stretch of the ray within the bounding sphere.
    let reach = center + radius;
    let span = quadratic_roots(dir.dot(dir), 2.0 * d.dot(dir), d.dot(d) - reach * reach);
    let [t0, t1] = span.as_slice() else {
        return Vec::new();
    };
    // A little beyond the sphere, so that a root where the ray touches it is inside.
    let pad = 1e-6 * (t1 - t0).abs().max(1.0);
    let (t0, t1) = (&(t0 - pad), &(t1 + pad));
    let steps = 256;
    let mut out = Vec::new();
    let mut prev_t = *t0;
    let mut prev_f = f(prev_t);
    for k in 1..=steps {
        let t = t0 + (t1 - t0) * k as f64 / steps as f64;
        let ft = f(t);
        if prev_f == 0.0 {
            out.push(prev_t);
        } else if prev_f.signum() != ft.signum() && ft != 0.0 {
            let (mut lo, mut hi, mut flo) = (prev_t, t, prev_f);
            for _ in 0..80 {
                let mid = 0.5 * (lo + hi);
                let fm = f(mid);
                if fm.signum() == flo.signum() {
                    lo = mid;
                    flo = fm;
                } else {
                    hi = mid;
                }
            }
            out.push(0.5 * (lo + hi));
        }
        prev_t = t;
        prev_f = ft;
    }
    out
}

// ---------------------------------------------------------------------------------------
// Area and mass integrals.

/// Gauss–Legendre nodes and weights on `[−1, 1]`.
pub(crate) static GAUSS: LazyLock<Vec<(f64, f64)>> = LazyLock::new(|| gauss_legendre(16));

/// The `n`-point Gauss–Legendre rule: the roots of the Legendre polynomial `P_n` by
/// Newton's method from Chebyshev guesses, and their weights `2 / ((1 − x²) P_n'(x)²)`.
pub(crate) fn gauss_legendre(n: usize) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut x = (PI * (i as f64 + 0.75) / (n as f64 + 0.5)).cos();
        let mut derivative = 1.0;
        for _ in 0..100 {
            // P_n(x) and P_n'(x) by the three-term recurrence.
            let (mut p0, mut p1) = (1.0, x);
            for k in 2..=n {
                let k = k as f64;
                let p2 = ((2.0 * k - 1.0) * x * p1 - (k - 1.0) * p0) / k;
                p0 = p1;
                p1 = p2;
            }
            derivative = n as f64 * (x * p1 - p0) / (x * x - 1.0);
            let step = p1 / derivative;
            x -= step;
            if step.abs() < 1e-16 {
                break;
            }
        }
        out.push((x, 2.0 / ((1.0 - x * x) * derivative * derivative)));
    }
    out
}

/// `∫ g` from `a` to `b` (negative when `b < a`), in pieces short enough for the rule to
/// be exact to rounding.
fn integrate(a: f64, b: f64, piece: f64, g: &impl Fn(f64) -> [f64; 9]) -> [f64; 9] {
    let mut total = [0.0; 9];
    if a == b {
        return total;
    }
    let pieces = ((b - a).abs() / piece).ceil().max(1.0) as usize;
    let h = (b - a) / pieces as f64;
    for k in 0..pieces {
        let (lo, hi) = (a + h * k as f64, a + h * (k + 1) as f64);
        let (mid, half) = ((lo + hi) / 2.0, (hi - lo) / 2.0);
        for &(x, w) in GAUSS.iter() {
            let values = g(mid + half * x);
            for (t, value) in total.iter_mut().zip(values) {
                *t += w * half * value;
            }
        }
    }
    total
}

/// `∫ f` from `a` to `b`, in pieces no longer than `piece`, by the Gauss–Legendre rule.
pub(crate) fn quadrature(a: f64, b: f64, piece: f64, f: &impl Fn(f64) -> f64) -> f64 {
    integrate(a, b, piece, &|x| {
        let mut out = [0.0; 9];
        out[0] = f(x);
        out
    })[0]
}

/// Antiderivatives of 1, cos, sin, cos², sin² and cos·sin at `θ`.
fn trig_antiderivatives(theta: f64) -> [f64; 6] {
    let (s, c) = theta.sin_cos();
    [
        theta,
        s,
        -c,
        theta / 2.0 + (2.0 * theta).sin() / 4.0,
        theta / 2.0 - (2.0 * theta).sin() / 4.0,
        s * s / 2.0,
    ]
}

/// The sums a face of a surface of revolution's integrals are made of: for each boundary
/// line of constant `θ` from `v0` to `v1`, `F_i(θ) ∫ g_j(v) dv`, `F_i` the antiderivatives
/// of [`trig_antiderivatives`] and `g_j` the functions of [`RevolutionSums::add_line`],
/// about a reference point.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RevolutionSums {
    sums: [[f64; 9]; 6],
    /// The reference point's offset from the surface's origin, along x, y and the axis.
    wc: f64,
    ws: f64,
    wa: f64,
}

impl RevolutionSums {
    /// Sums about `reference` (with `w = origin − reference`).
    pub fn new(surface: &Revolution, reference: Point3) -> Self {
        let (x, y, a) = surface.frame();
        let w = surface.axis.origin - reference;
        Self {
            sums: [[0.0; 9]; 6],
            wc: w.dot(x),
            ws: w.dot(y),
            wa: w.dot(a),
        }
    }

    /// The functions of `v` the integrals are made of, at `v`.
    fn g(&self, surface: &Revolution, v: f64) -> [f64; 9] {
        let (rho, z) = surface.meridian_at(v);
        let (dr, dz) = surface.meridian_tangent(v);
        let l = (dr * dr + dz * dz).sqrt();
        // n dA = ρ (z' e − ρ' a) dθ dv; (x − r)·n dA = (P (w·e) + Q) dθ dv.
        let p = rho * dz;
        let q = rho * (rho * dz - dr * self.wa - dr * z);
        [
            rho * l,
            rho * rho * l,
            rho * z * l,
            p,
            q,
            rho * p,
            rho * q,
            z * p,
            z * q,
        ]
    }

    /// Adds an edge that bends across the `(θ, v)` plane, followed along `path`:
    /// `∫ F(θ) g(v) dv` along it (`F` the antiderivatives, as for a line of constant `θ`) by
    /// Gauss–Legendre quadrature in its own parameter, `dv` from its points a hair either
    /// side.
    pub fn add_path(&mut self, surface: &Revolution, edge: &WalkEdge, path: &ParamPath) {
        let h = 1e-6 * edge.sweep.max(1e-9);
        let pieces = 32;
        let step = edge.sweep / f64::from(pieces);
        for k in 0..pieces {
            let (lo, hi) = (step * f64::from(k), step * f64::from(k + 1));
            let (mid, half) = ((lo + hi) / 2.0, (hi - lo) / 2.0);
            for &(x, w) in GAUSS.iter() {
                let s = mid + half * x;
                let (theta, v) = path.at(surface, edge, s);
                let dv =
                    (path.at(surface, edge, s + h).1 - path.at(surface, edge, s - h).1) / (2.0 * h);
                let g = self.g(surface, v);
                let f = trig_antiderivatives(theta);
                for (row, fi) in self.sums.iter_mut().zip(f) {
                    for (cell, gj) in row.iter_mut().zip(g) {
                        *cell += w * half * fi * gj * dv;
                    }
                }
            }
        }
    }

    /// Adds the boundary line at angle `theta` from `v0` to `v1`.
    pub fn add_line(&mut self, surface: &Revolution, theta: f64, v0: f64, v1: f64) {
        let g = |v: f64| self.g(surface, v);
        let piece = match surface.meridian {
            Meridian::Line { .. } => f64::INFINITY,
            Meridian::Circle { .. } => PI / 8.0,
        };
        let integrals = integrate(v0, v1, piece, &g);
        let f = trig_antiderivatives(theta);
        for (row, fi) in self.sums.iter_mut().zip(f) {
            for (cell, gj) in row.iter_mut().zip(integrals) {
                *cell += fi * gj;
            }
        }
    }

    fn s(&self, f: usize, g: usize) -> f64 {
        self.sums[f][g]
    }

    /// The face's integrals: `sums` taken as its boundary ran, counter-clockwise in
    /// `(θ, v)` for an outward face and clockwise for an inward one.
    pub fn integrals(&self, surface: &Revolution) -> crate::geometry::FaceIntegrals {
        const ONE: usize = 0;
        const COS: usize = 1;
        const SIN: usize = 2;
        const COS2: usize = 3;
        const SIN2: usize = 4;
        const CS: usize = 5;
        let (x, y, a) = surface.frame();
        let o = surface.axis.origin.to_vec();
        let (wc, ws) = (self.wc, self.ws);
        // The region's integrals are the boundary's for an outward face and their opposite
        // for an inward one; its normal's sign brings the same sign back to the flux.
        let sign = if surface.outward { 1.0 } else { -1.0 };
        let area = sign * self.s(ONE, 0);
        let first =
            (o * self.s(ONE, 0) + x * self.s(COS, 1) + y * self.s(SIN, 1) + a * self.s(ONE, 2))
                * sign;
        let flux = wc * self.s(COS, 3) + ws * self.s(SIN, 3) + self.s(ONE, 4);
        let w = Vec3::new(0.0, 0.0, 0.0) + x * wc + y * ws + a * self.wa;
        let flux_moment = w * flux
            + x * (wc * self.s(COS2, 5) + ws * self.s(CS, 5) + self.s(COS, 6))
            + y * (wc * self.s(CS, 5) + ws * self.s(SIN2, 5) + self.s(SIN, 6))
            + a * (wc * self.s(COS, 7) + ws * self.s(SIN, 7) + self.s(ONE, 8));
        crate::geometry::FaceIntegrals {
            area,
            first,
            flux,
            flux_moment,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::FaceIntegrals;

    fn about_z(meridian: Meridian) -> Revolution {
        Revolution {
            axis: Axis {
                origin: Point3::new(1.0, 2.0, 3.0),
                direction: Vec3::Z,
            },
            x_dir: Vec3::X,
            meridian,
            outward: true,
        }
    }

    /// A whole face from `v0` to `v1`: its boundary up the seam at 2π and down at 0.
    fn whole(surface: &Revolution, v0: f64, v1: f64, reference: Point3) -> FaceIntegrals {
        let mut sums = RevolutionSums::new(surface, reference);
        sums.add_line(surface, TAU, v0, v1);
        sums.add_line(surface, 0.0, v1, v0);
        sums.integrals(surface)
    }

    #[test]
    fn the_rule_integrates_polynomials_exactly() {
        let rule = gauss_legendre(16);
        let sum: f64 = rule.iter().map(|(_, w)| w).sum();
        assert!((sum - 2.0).abs() < 1e-14);
        let x30: f64 = rule.iter().map(|(x, w)| w * x.powi(30)).sum();
        assert!((x30 - 2.0 / 31.0).abs() < 1e-14);
    }

    #[test]
    fn a_sphere_has_its_area_and_volume() {
        let r = 3.0;
        let sphere = about_z(Meridian::Circle {
            center: 0.0,
            radius: r,
        });
        let reference = Point3::new(-2.0, 0.5, 7.0);
        let face = whole(&sphere, -PI / 2.0, PI / 2.0, reference);
        assert!((face.area - 4.0 * PI * r * r).abs() < 1e-9, "{}", face.area);
        let volume = face.flux / 3.0;
        assert!(
            (volume - 4.0 / 3.0 * PI * r.powi(3)).abs() < 1e-9,
            "{volume}"
        );
        let centre = reference + face.flux_moment * (0.25 / volume);
        assert!(
            centre.distance(Point3::new(1.0, 2.0, 3.0)) < 1e-9,
            "{centre:?}"
        );
        // The upper half's centroid is r / 2 above the centre.
        let half = whole(&sphere, 0.0, PI / 2.0, reference);
        let c = half.centroid(Point3::ORIGIN);
        assert!(
            c.distance(Point3::new(1.0, 2.0, 3.0 + r / 2.0)) < 1e-9,
            "{c:?}"
        );
    }

    #[test]
    fn a_torus_has_its_area_and_volume() {
        let (big, small) = (10.0, 3.0);
        let torus = about_z(Meridian::Circle {
            center: big,
            radius: small,
        });
        let face = whole(&torus, -PI, PI, Point3::ORIGIN);
        assert!((face.area - 4.0 * PI * PI * big * small).abs() < 1e-8);
        let volume = face.flux / 3.0;
        assert!((volume - 2.0 * PI * PI * big * small * small).abs() < 1e-8);
        let centre = face.flux_moment * (0.25 / volume);
        assert!(
            Point3::new(centre.x, centre.y, centre.z).distance(Point3::new(1.0, 2.0, 3.0)) < 1e-9
        );
    }

    #[test]
    fn a_cone_frustum_has_its_lateral_area() {
        // Radius 6 at height 0 down to 3 at height 10.
        let cone = about_z(Meridian::Line {
            radius: 6.0,
            slope: -0.3,
        });
        let face = whole(&cone, 0.0, 10.0, Point3::ORIGIN);
        let slant = (100.0f64 + 9.0).sqrt();
        assert!((face.area - PI * (6.0 + 3.0) * slant).abs() < 1e-9);
    }

    #[test]
    fn rays_meet_the_surfaces_where_they_are() {
        let torus = about_z(Meridian::Circle {
            center: 10.0,
            radius: 3.0,
        });
        // Along x through the centre: the tube 7 and 13 from it either side, at x = −12,
        // −6, 8 and 14, which the ray from x = −20 reaches at these distances.
        let hits = torus.ray_hits(Point3::new(-20.0, 2.0, 3.0), Vec3::X);
        assert_eq!(hits.len(), 4, "{hits:?}");
        for (h, e) in hits.iter().zip([8.0, 14.0, 28.0, 34.0]) {
            assert!((h - e).abs() < 1e-9, "{hits:?}");
        }
        let cone = about_z(Meridian::Line {
            radius: 0.0,
            slope: 0.5,
        });
        // From the axis up at height 4 (radius 2): one hit on the upper nappe.
        let hits = cone.ray_hits(Point3::new(1.0, 2.0, 7.0), Vec3::X);
        assert_eq!(hits.len(), 2);
        assert!(
            hits.iter().all(|t| (t.abs() - 2.0).abs() < 1e-9),
            "{hits:?}"
        );
        let hits = cone.ray_hits(Point3::new(1.0, 2.0, -1.0), Vec3::X);
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn coaxial_surfaces_meet_where_their_meridians_cross() {
        let sphere = about_z(Meridian::Circle {
            center: 0.0,
            radius: 5.0,
        });
        let cylinder = Revolution::cylinder(
            Axis {
                origin: Point3::new(1.0, 2.0, -4.0),
                direction: -Vec3::Z,
            },
            Vec3::X,
            3.0,
            true,
        );
        let m = sphere.coaxial_meridian(&cylinder, 1e-9).unwrap();
        let crossings = sphere.meridian_crossings(&m, 1e-9).unwrap();
        assert_eq!(crossings.len(), 2);
        for (rho, z) in crossings {
            assert!((rho - 3.0).abs() < 1e-9 && (z.abs() - 4.0).abs() < 1e-9);
        }
        // A cone through the cylinder's rim at height 1 above the sphere's centre.
        let cone = Revolution {
            axis: Axis {
                origin: Point3::new(1.0, 2.0, 3.0),
                direction: Vec3::Z,
            },
            x_dir: Vec3::X,
            meridian: Meridian::Line {
                radius: 2.0,
                slope: 1.0,
            },
            outward: true,
        };
        let m = cylinder.coaxial_meridian(&cone, 1e-9).unwrap();
        let crossings = cylinder.meridian_crossings(&m, 1e-9).unwrap();
        // In the cylinder's frame (height along −Z from z = −4): z = 3 + 1 is at −8.
        assert_eq!(crossings.len(), 1);
        assert!((crossings[0].1 + 8.0).abs() < 1e-9, "{crossings:?}");
    }
}
