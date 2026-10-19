//! The eight-node brick: a voxel of the grid as a trilinear element, with its stiffness
//! for a unit modulus and a unit edge. A cube of edge `h` and modulus `E` is `E h` times
//! as stiff: its strains go as `1 / h` and its volume as `h³`.
//!
//! Corner `a` of a brick is `(a & 1, (a >> 1) & 1, (a >> 2) & 1)`, x fastest, as the grid
//! numbers its nodes; the unknowns go corner by corner, x y z.

// Fixed-size arrays indexed by loop counters bounded by their length.
#![allow(clippy::indexing_slicing)]

/// Unknowns of a brick: three per corner.
pub(crate) const DOFS: usize = 24;
/// Values of a brick's matrix.
pub(crate) const SIZE: usize = DOFS * DOFS;

/// Corner `a`'s offset along each axis, 0 or 1.
pub(crate) const fn corner(a: usize) -> [usize; 3] {
    [a & 1, (a >> 1) & 1, (a >> 2) & 1]
}

/// What every voxel of one material shares.
#[derive(Debug, Clone)]
pub(crate) struct Brick {
    /// The stiffness for a unit modulus and a unit edge, row-major.
    pub k: [f64; SIZE],
    /// The strains at the centre for a unit edge, `[xx, yy, zz, γxy, γyz, γzx]`, 6 × 24.
    pub b: [f64; 6 * DOFS],
    /// The elasticity matrix for a unit modulus, 6 × 6.
    pub d: [f64; 36],
    /// The largest eigenvalue of `D⁻¹ K`, `D` the 3 × 3 blocks of `k`'s diagonal: no
    /// assembly of bricks has a larger one for its own block diagonal, which bounds the
    /// smoother's interval on the finest level without power iterations.
    pub spread: f64,
}

impl Brick {
    /// The brick of an isotropic material of Poisson's ratio `poisson`.
    pub(crate) fn new(poisson: f64) -> Brick {
        let d = elasticity(poisson);
        let mut k = [0.0; SIZE];
        let g = 0.5 / 3f64.sqrt();
        for p in 0..8 {
            let at = corner(p).map(|c| if c == 0 { 0.5 - g } else { 0.5 + g });
            let b = strains(at);
            // Each of the eight Gauss points weighs an eighth of the unit cube.
            let mut db = [0.0; 6 * DOFS];
            for r in 0..6 {
                for c in 0..DOFS {
                    db[r * DOFS + c] = (0..6).map(|s| d[r * 6 + s] * b[s * DOFS + c]).sum();
                }
            }
            for i in 0..DOFS {
                for j in 0..DOFS {
                    let v: f64 = (0..6).map(|r| b[r * DOFS + i] * db[r * DOFS + j]).sum();
                    k[i * DOFS + j] += v / 8.0;
                }
            }
        }
        // Exactly symmetric, whatever the sums' rounding.
        for i in 0..DOFS {
            for j in 0..i {
                let m = 0.5 * (k[i * DOFS + j] + k[j * DOFS + i]);
                k[i * DOFS + j] = m;
                k[j * DOFS + i] = m;
            }
        }
        let spread = spread(&k);
        Brick {
            k,
            b: strains([0.5; 3]),
            d,
            spread,
        }
    }

    /// The diagonal value of `k`: the same for every unknown of a cube.
    pub(crate) fn diagonal(&self) -> f64 {
        self.k[0]
    }
}

/// The elasticity matrix of an isotropic material for a unit modulus, engineering shears.
fn elasticity(poisson: f64) -> [f64; 36] {
    let nu = poisson;
    let lambda = nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
    let mu = 0.5 / (1.0 + nu);
    let mut d = [0.0; 36];
    for r in 0..3 {
        for c in 0..3 {
            d[r * 6 + c] = if r == c { lambda + 2.0 * mu } else { lambda };
        }
        d[(r + 3) * 6 + r + 3] = mu;
    }
    d
}

/// The strain–displacement matrix of the unit cube at `at` (in it, 0 to 1 along each axis).
fn strains(at: [f64; 3]) -> [f64; 6 * DOFS] {
    let mut b = [0.0; 6 * DOFS];
    for a in 0..8 {
        let c = corner(a);
        let f = |axis: usize| {
            if c[axis] == 0 {
                1.0 - at[axis]
            } else {
                at[axis]
            }
        };
        let s = |axis: usize| if c[axis] == 0 { -1.0 } else { 1.0 };
        let g = [s(0) * f(1) * f(2), f(0) * s(1) * f(2), f(0) * f(1) * s(2)];
        let col = 3 * a;
        b[col] = g[0];
        b[DOFS + col + 1] = g[1];
        b[2 * DOFS + col + 2] = g[2];
        // γxy
        b[3 * DOFS + col] = g[1];
        b[3 * DOFS + col + 1] = g[0];
        // γyz
        b[4 * DOFS + col + 1] = g[2];
        b[4 * DOFS + col + 2] = g[1];
        // γzx
        b[5 * DOFS + col] = g[2];
        b[5 * DOFS + col + 2] = g[0];
    }
    b
}

/// The largest eigenvalue of `D⁻¹ k`, `D` its diagonal 3 × 3 blocks, by power iteration
/// from a fixed start (deterministic), with a margin.
fn spread(k: &[f64; SIZE]) -> f64 {
    let mut dinv = [[0.0; 9]; 8];
    for (a, out) in dinv.iter_mut().enumerate() {
        let mut block = [0.0; 9];
        for r in 0..3 {
            for c in 0..3 {
                block[r * 3 + c] = k[(3 * a + r) * DOFS + 3 * a + c];
            }
        }
        *out = invert3(&block).unwrap_or([0.0; 9]);
    }
    let mut v: [f64; DOFS] = std::array::from_fn(|i| 1.0 + ((i * 7) % 5) as f64 * 0.37);
    let mut lambda = 0.0;
    for _ in 0..200 {
        let mut kv = [0.0; DOFS];
        for (i, out) in kv.iter_mut().enumerate() {
            *out = (0..DOFS).map(|j| k[i * DOFS + j] * v[j]).sum();
        }
        let mut w = [0.0; DOFS];
        for a in 0..8 {
            for r in 0..3 {
                w[3 * a + r] = (0..3).map(|c| dinv[a][r * 3 + c] * kv[3 * a + c]).sum();
            }
        }
        let n = w.iter().map(|x| x * x).sum::<f64>().sqrt();
        if !(n > 0.0 && n.is_finite()) {
            break;
        }
        lambda = n / v.iter().map(|x| x * x).sum::<f64>().sqrt();
        v = w.map(|x| x / n);
    }
    lambda
}

/// The inverse of a 3 × 3 matrix, row-major; `None` when it is singular.
pub(crate) fn invert3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let c = [
        m[4] * m[8] - m[5] * m[7],
        m[5] * m[6] - m[3] * m[8],
        m[3] * m[7] - m[4] * m[6],
    ];
    let det = m[0] * c[0] + m[1] * c[1] + m[2] * c[2];
    let scale = m.iter().fold(0.0f64, |s, x| s.max(x.abs()));
    if !(det.abs() > 1e-14 * scale * scale * scale && det.is_finite()) {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        c[0] * inv,
        (m[2] * m[7] - m[1] * m[8]) * inv,
        (m[1] * m[5] - m[2] * m[4]) * inv,
        c[1] * inv,
        (m[0] * m[8] - m[2] * m[6]) * inv,
        (m[2] * m[3] - m[0] * m[5]) * inv,
        c[2] * inv,
        (m[1] * m[6] - m[0] * m[7]) * inv,
        (m[0] * m[4] - m[1] * m[3]) * inv,
    ])
}

/// The von Mises stress of `[xx, yy, zz, xy, yz, zx]`.
pub(crate) fn von_mises(s: &[f64; 6]) -> f64 {
    let d = (s[0] - s[1]).powi(2) + (s[1] - s[2]).powi(2) + (s[2] - s[0]).powi(2);
    (0.5 * d + 3.0 * (s[3] * s[3] + s[4] * s[4] + s[5] * s[5])).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn energy(k: &[f64; SIZE], u: &[f64; DOFS]) -> f64 {
        (0..DOFS)
            .map(|i| u[i] * (0..DOFS).map(|j| k[i * DOFS + j] * u[j]).sum::<f64>())
            .sum()
    }

    #[test]
    fn rigid_motions_cost_nothing_and_a_uniform_strain_costs_what_the_material_says() {
        let brick = Brick::new(0.3);
        // Translations and turns.
        let motions: [fn([f64; 3]) -> [f64; 3]; 6] = [
            |_| [1.0, 0.0, 0.0],
            |_| [0.0, 1.0, 0.0],
            |_| [0.0, 0.0, 1.0],
            |p| [-p[1], p[0], 0.0],
            |p| [0.0, -p[2], p[1]],
            |p| [p[2], 0.0, -p[0]],
        ];
        for m in motions {
            let mut u = [0.0; DOFS];
            for a in 0..8 {
                let v = m(corner(a).map(|c| c as f64));
                u[3 * a..3 * a + 3].copy_from_slice(&v);
            }
            assert!(energy(&brick.k, &u).abs() < 1e-12);
        }
        // u = (0.01 x, −0.003 y, 0.002 z + 0.004 x): the energy is εᵀ D ε over the volume,
        // trilinear elements carrying linear fields exactly.
        let mut u = [0.0; DOFS];
        for a in 0..8 {
            let p = corner(a).map(|c| c as f64);
            u[3 * a] = 0.01 * p[0];
            u[3 * a + 1] = -0.003 * p[1];
            u[3 * a + 2] = 0.002 * p[2] + 0.004 * p[0];
        }
        let strain = [0.01, -0.003, 0.002, 0.0, 0.0, 0.004];
        let expected: f64 = (0..6)
            .map(|r| strain[r] * (0..6).map(|c| brick.d[r * 6 + c] * strain[c]).sum::<f64>())
            .sum();
        assert!((energy(&brick.k, &u) - expected).abs() < 1e-12 * expected.abs().max(1.0));
        // The strains at the centre are those strains.
        for (r, &s) in strain.iter().enumerate() {
            let got: f64 = (0..DOFS).map(|c| brick.b[r * DOFS + c] * u[c]).sum();
            assert!((got - s).abs() < 1e-12, "{r}: {got} {s}");
        }
        // A cube's diagonal is the same for every unknown.
        for i in 0..DOFS {
            assert!((brick.k[i * DOFS + i] - brick.diagonal()).abs() < 1e-12);
        }
        assert!(brick.spread > 1.0 && brick.spread < 8.0, "{}", brick.spread);
    }

    #[test]
    fn inverse_and_von_mises() {
        let m = [4.0, 1.0, 0.5, 1.0, 3.0, 0.2, 0.5, 0.2, 2.0];
        let inv = invert3(&m).unwrap();
        for r in 0..3 {
            for c in 0..3 {
                let v: f64 = (0..3).map(|k| m[r * 3 + k] * inv[k * 3 + c]).sum();
                assert!((v - if r == c { 1.0 } else { 0.0 }).abs() < 1e-12);
            }
        }
        assert!(invert3(&[1.0, 2.0, 3.0, 2.0, 4.0, 6.0, 0.0, 0.0, 1.0]).is_none());
        assert!((von_mises(&[100.0, 0.0, 0.0, 0.0, 0.0, 0.0]) - 100.0).abs() < 1e-12);
        assert!((von_mises(&[0.0, 0.0, 0.0, 10.0, 0.0, 0.0]) - 10.0 * 3f64.sqrt()).abs() < 1e-12);
    }
}
