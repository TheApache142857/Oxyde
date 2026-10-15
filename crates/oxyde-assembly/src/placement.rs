use oxyde_geom::{Point3, Transform, Vec3};

/// Where a component sits: a rotation then a translation, `p' = R p + t`. Never a mirror.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Rows of an orthonormal matrix with determinant +1.
    pub rotation: [[f64; 3]; 3],
    pub translation: Vec3,
}

impl Placement {
    pub const IDENTITY: Placement = Placement {
        rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        translation: Vec3::ZERO,
    };

    pub fn from_translation(translation: Vec3) -> Placement {
        Placement {
            translation,
            ..Placement::IDENTITY
        }
    }

    pub fn transform(&self) -> Transform {
        let r = &self.rotation;
        let t = self.translation;
        Transform {
            rows: [
                [r[0][0], r[0][1], r[0][2], t.x],
                [r[1][0], r[1][1], r[1][2], t.y],
                [r[2][0], r[2][1], r[2][2], t.z],
            ],
        }
    }

    /// The placement doing what `t` does, if `t` is a rotation and a translation (no
    /// mirror, no scaling, within `1e-6`). Re-orthonormalized.
    pub fn from_transform(t: &Transform) -> Option<Placement> {
        let rows = &t.rows;
        let rotation = [0, 1, 2].map(|i| [rows[i][0], rows[i][1], rows[i][2]]);
        let rotation_t = [0, 1, 2].map(|i| [rotation[0][i], rotation[1][i], rotation[2][i]]);
        let product = multiply(&rotation, &rotation_t);
        for (i, row) in product.iter().enumerate() {
            for (j, value) in row.iter().enumerate() {
                let expected = if i == j { 1.0 } else { 0.0 };
                if !value.is_finite() || (value - expected).abs() > 1e-6 {
                    return None;
                }
            }
        }
        let r = rotation;
        let det = r[0][0] * (r[1][1] * r[2][2] - r[1][2] * r[2][1])
            - r[0][1] * (r[1][0] * r[2][2] - r[1][2] * r[2][0])
            + r[0][2] * (r[1][0] * r[2][1] - r[1][1] * r[2][0]);
        let translation = Vec3::new(rows[0][3], rows[1][3], rows[2][3]);
        if det <= 0.0 || !translation.length().is_finite() {
            return None;
        }
        Some(Placement {
            rotation: orthonormalize(rotation),
            translation,
        })
    }

    pub fn apply_point(&self, p: Point3) -> Point3 {
        Point3::ORIGIN + self.apply_vector(p.to_vec()) + self.translation
    }

    pub fn apply_vector(&self, v: Vec3) -> Vec3 {
        let r = &self.rotation;
        Vec3::new(
            r[0][0] * v.x + r[0][1] * v.y + r[0][2] * v.z,
            r[1][0] * v.x + r[1][1] * v.y + r[1][2] * v.z,
            r[2][0] * v.x + r[2][1] * v.y + r[2][2] * v.z,
        )
    }

    /// `inner` (a placement inside a sub-assembly) seen from where this one puts the
    /// sub-assembly: `self(inner(p))`.
    pub fn compose(&self, inner: &Placement) -> Placement {
        Placement {
            rotation: multiply(&self.rotation, &inner.rotation),
            translation: self.apply_vector(inner.translation) + self.translation,
        }
    }

    /// Rotated by `rotation_vector` (axis × angle, radians) about `center`, then moved by
    /// `shift`, all in assembly coordinates.
    pub fn moved(&self, rotation_vector: Vec3, center: Point3, shift: Vec3) -> Placement {
        let q = rotation_matrix(rotation_vector);
        let rotation = multiply(&q, &self.rotation);
        // p' = Q (R p + t − c) + c + shift
        let c = center.to_vec();
        let translation = apply(&q, self.translation - c) + c + shift;
        Placement {
            rotation: orthonormalize(rotation),
            translation,
        }
    }
}

impl Default for Placement {
    fn default() -> Self {
        Placement::IDENTITY
    }
}

/// Rodrigues' formula.
pub(crate) fn rotation_matrix(w: Vec3) -> [[f64; 3]; 3] {
    let angle = w.length();
    if angle < 1e-15 {
        return Placement::IDENTITY.rotation;
    }
    let k = w * (1.0 / angle);
    let (s, c) = angle.sin_cos();
    let v = 1.0 - c;
    [
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
    ]
}

fn multiply(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn apply(m: &[[f64; 3]; 3], v: Vec3) -> Vec3 {
    Vec3::new(
        m[0][0] * v.x + m[0][1] * v.y + m[0][2] * v.z,
        m[1][0] * v.x + m[1][1] * v.y + m[1][2] * v.z,
        m[2][0] * v.x + m[2][1] * v.y + m[2][2] * v.z,
    )
}

/// Removes the drift that many small rotations accumulate (Gram–Schmidt on the rows).
fn orthonormalize(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let row = |i: usize| Vec3::new(m[i][0], m[i][1], m[i][2]);
    let x = row(0).normalized().unwrap_or(Vec3::X);
    let y = row(1) - x * row(1).dot(x);
    let y = y.normalized().unwrap_or(Vec3::Y);
    let z = x.cross(y);
    [[x.x, x.y, x.z], [y.x, y.y, y.z], [z.x, z.y, z.z]]
}
