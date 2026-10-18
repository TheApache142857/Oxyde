use crate::{Plane, Point3, Vec3};

/// An infinite line with a direction: a revolve axis, a pattern axis or direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    pub origin: Point3,
    /// Unit length.
    pub direction: Vec3,
}

impl Axis {
    /// The model X, Y and Z axes, through the origin.
    pub const X: Axis = Axis {
        origin: Point3::ORIGIN,
        direction: Vec3::X,
    };
    pub const Y: Axis = Axis {
        origin: Point3::ORIGIN,
        direction: Vec3::Y,
    };
    pub const Z: Axis = Axis {
        origin: Point3::ORIGIN,
        direction: Vec3::Z,
    };

    /// `None` if `direction` has no length.
    pub fn new(origin: Point3, direction: Vec3) -> Option<Axis> {
        Some(Axis {
            origin,
            direction: direction.normalized()?,
        })
    }

    pub fn reversed(self) -> Axis {
        Axis {
            origin: self.origin,
            direction: -self.direction,
        }
    }

    /// Distance from a point to the line.
    pub fn distance_to(&self, p: Point3) -> f64 {
        let v = p - self.origin;
        (v - self.direction * v.dot(self.direction)).length()
    }

    /// The line where two planes meet, or `None` if they are parallel.
    pub fn intersection(a: &Plane, b: &Plane) -> Option<Axis> {
        let direction = a.normal.cross(b.normal).normalized()?;
        // The point of the line closest to the origin: solve n·p = d for both planes with
        // p in the span of the two normals.
        let (n1, n2) = (a.normal, b.normal);
        let (d1, d2) = (n1.dot(a.origin.to_vec()), n2.dot(b.origin.to_vec()));
        let n12 = n1.dot(n2);
        let det = 1.0 - n12 * n12;
        let c1 = (d1 - d2 * n12) / det;
        let c2 = (d2 - d1 * n12) / det;
        let origin = Point3::ORIGIN + n1 * c1 + n2 * c2;
        Some(Axis { origin, direction })
    }
}

/// A rigid motion or a mirror: `p' = M p + t`, stored as three rows `[m0 m1 m2 t]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub rows: [[f64; 4]; 3],
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        rows: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ],
    };

    pub fn translation(v: Vec3) -> Transform {
        let mut t = Self::IDENTITY;
        t.rows[0][3] = v.x;
        t.rows[1][3] = v.y;
        t.rows[2][3] = v.z;
        t
    }

    /// Rotation by `angle` radians about `axis`, counter-clockwise looking against the
    /// axis direction (right-hand rule).
    pub fn rotation(axis: &Axis, angle: f64) -> Transform {
        let k = axis.direction;
        let (s, c) = angle.sin_cos();
        let v = 1.0 - c;
        let m = [
            [
                c + k.x * k.x * v,
                k.x * k.y * v - k.z * s,
                k.x * k.z * v + k.y * s,
            ],
            [
                k.y * k.x * v + k.z * s,
                c + k.y * k.y * v,
                k.y * k.z * v - k.x * s,
            ],
            [
                k.z * k.x * v - k.y * s,
                k.z * k.y * v + k.x * s,
                c + k.z * k.z * v,
            ],
        ];
        Self::from_matrix_about(m, axis.origin)
    }

    /// The rigid motion that takes the world's frame to `plane`'s own: x along its
    /// `x_dir`, y along its `y_dir`, z along its normal, the origin to its origin. A shape
    /// made about the world origin is placed on the plane this way.
    pub fn from_plane(plane: &Plane) -> Transform {
        let (x, y, n, o) = (plane.x_dir, plane.y_dir(), plane.normal, plane.origin);
        Transform {
            rows: [
                [x.x, y.x, n.x, o.x],
                [x.y, y.y, n.y, o.y],
                [x.z, y.z, n.z, o.z],
            ],
        }
    }

    /// Mirror image through a plane.
    pub fn mirror(plane: &Plane) -> Transform {
        let n = plane.normal;
        let m = [
            [1.0 - 2.0 * n.x * n.x, -2.0 * n.x * n.y, -2.0 * n.x * n.z],
            [-2.0 * n.y * n.x, 1.0 - 2.0 * n.y * n.y, -2.0 * n.y * n.z],
            [-2.0 * n.z * n.x, -2.0 * n.z * n.y, 1.0 - 2.0 * n.z * n.z],
        ];
        Self::from_matrix_about(m, plane.origin)
    }

    /// `M` applied around a fixed point: `p' = M (p − c) + c`.
    fn from_matrix_about(m: [[f64; 3]; 3], c: Point3) -> Transform {
        let mut rows = [[0.0; 4]; 3];
        for (i, row) in rows.iter_mut().enumerate() {
            row[..3].copy_from_slice(&m[i]);
            row[3] = [c.x, c.y, c.z][i] - (m[i][0] * c.x + m[i][1] * c.y + m[i][2] * c.z);
        }
        Transform { rows }
    }

    pub fn apply_point(&self, p: Point3) -> Point3 {
        let r = &self.rows;
        Point3::new(
            r[0][0] * p.x + r[0][1] * p.y + r[0][2] * p.z + r[0][3],
            r[1][0] * p.x + r[1][1] * p.y + r[1][2] * p.z + r[1][3],
            r[2][0] * p.x + r[2][1] * p.y + r[2][2] * p.z + r[2][3],
        )
    }

    pub fn apply_vector(&self, v: Vec3) -> Vec3 {
        let r = &self.rows;
        Vec3::new(
            r[0][0] * v.x + r[0][1] * v.y + r[0][2] * v.z,
            r[1][0] * v.x + r[1][1] * v.y + r[1][2] * v.z,
            r[2][0] * v.x + r[2][1] * v.y + r[2][2] * v.z,
        )
    }

    /// This transform followed by `next`.
    pub fn then(&self, next: &Transform) -> Transform {
        let (a, b) = (&next.rows, &self.rows);
        let mut rows = [[0.0; 4]; 3];
        for i in 0..3 {
            for j in 0..4 {
                let mut sum = (0..3).map(|k| a[i][k] * b[k][j]).sum::<f64>();
                if j == 3 {
                    sum += a[i][3];
                }
                rows[i][j] = sum;
            }
        }
        Transform { rows }
    }

    /// True for a mirror (the transform turns shapes inside out).
    pub fn is_mirror(&self) -> bool {
        let m = &self.rows;
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        det < 0.0
    }
}

impl Plane {
    /// The plane moved by a rigid motion (not a mirror, which would make it left-handed).
    pub fn transformed(&self, t: &Transform) -> Plane {
        Plane {
            origin: t.apply_point(self.origin),
            x_dir: t.apply_vector(self.x_dir),
            normal: t.apply_vector(self.normal),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point3, b: Point3) -> bool {
        a.distance(b) < 1e-9
    }

    #[test]
    fn rotation_follows_the_right_hand_rule() {
        let axis = Axis::new(Point3::new(1.0, 0.0, 0.0), Vec3::Z).unwrap();
        let quarter = Transform::rotation(&axis, std::f64::consts::FRAC_PI_2);
        assert!(close(
            quarter.apply_point(Point3::new(2.0, 0.0, 5.0)),
            Point3::new(1.0, 1.0, 5.0)
        ));
        assert!(!quarter.is_mirror());
    }

    #[test]
    fn mirror_and_composition() {
        let plane = Plane::RIGHT.offset(10.0);
        let m = Transform::mirror(&plane);
        assert!(close(
            m.apply_point(Point3::new(3.0, 1.0, 2.0)),
            Point3::new(17.0, 1.0, 2.0)
        ));
        assert!(m.is_mirror());
        let twice = m.then(&m);
        assert!(close(
            twice.apply_point(Point3::new(3.0, 1.0, 2.0)),
            Point3::new(3.0, 1.0, 2.0)
        ));
        let moved = Transform::translation(Vec3::X).then(&m);
        assert!(close(
            moved.apply_point(Point3::ORIGIN),
            Point3::new(19.0, 0.0, 0.0)
        ));
    }

    #[test]
    fn two_planes_meet_along_a_line() {
        let axis =
            Axis::intersection(&Plane::FRONT.offset(5.0), &Plane::RIGHT.offset(2.0)).unwrap();
        assert!(axis.direction.cross(Vec3::Y).length() < 1e-12);
        assert!(axis.distance_to(Point3::new(2.0, 7.0, 5.0)) < 1e-9);
        assert!(Axis::intersection(&Plane::FRONT, &Plane::FRONT.offset(1.0)).is_none());
    }
}
