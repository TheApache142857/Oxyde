//! Small vector and matrix helpers on plain arrays.

// Fixed-size arrays indexed by loop counters bounded by their length.
#![allow(clippy::indexing_slicing)]

/// A point or a vector, in millimetres unless said otherwise.
pub type V3 = [f64; 3];

pub(crate) fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub(crate) fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn scale(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}

pub(crate) fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

/// `a` scaled to unit length, or `None` when it has none.
pub(crate) fn normalize(a: V3) -> Option<V3> {
    let n = norm(a);
    (n > 1e-300 && n.is_finite()).then(|| scale(a, 1.0 / n))
}

pub(crate) fn lerp(a: V3, b: V3, t: f64) -> V3 {
    add(a, scale(sub(b, a), t))
}

/// Six times the signed volume of the tetrahedron `a b c d`: positive when `d` lies on the
/// side `(b − a) × (c − a)` points to.
pub(crate) fn tet_volume6(a: V3, b: V3, c: V3, d: V3) -> f64 {
    dot(cross(sub(b, a), sub(c, a)), sub(d, a))
}

/// A 3 × 3 matrix, row-major.
pub(crate) type M3 = [f64; 9];

pub(crate) const IDENTITY: M3 = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

pub(crate) fn mat_vec(m: &M3, v: V3) -> V3 {
    [
        m[0] * v[0] + m[1] * v[1] + m[2] * v[2],
        m[3] * v[0] + m[4] * v[1] + m[5] * v[2],
        m[6] * v[0] + m[7] * v[1] + m[8] * v[2],
    ]
}

/// `mᵀ v`.
pub(crate) fn mat_t_vec(m: &M3, v: V3) -> V3 {
    [
        m[0] * v[0] + m[3] * v[1] + m[6] * v[2],
        m[1] * v[0] + m[4] * v[1] + m[7] * v[2],
        m[2] * v[0] + m[5] * v[1] + m[8] * v[2],
    ]
}

pub(crate) fn mat_mul(a: &M3, b: &M3) -> M3 {
    let mut out = [0.0; 9];
    for i in 0..3 {
        for j in 0..3 {
            out[i * 3 + j] = (0..3).map(|k| a[i * 3 + k] * b[k * 3 + j]).sum();
        }
    }
    out
}

pub(crate) fn transpose(a: &M3) -> M3 {
    [a[0], a[3], a[6], a[1], a[4], a[7], a[2], a[5], a[8]]
}

/// The inverse of `m`, or `None` when it is singular.
pub(crate) fn inverse(m: &M3) -> Option<M3> {
    let c = [
        m[4] * m[8] - m[5] * m[7],
        m[5] * m[6] - m[3] * m[8],
        m[3] * m[7] - m[4] * m[6],
    ];
    let det = m[0] * c[0] + m[1] * c[1] + m[2] * c[2];
    let size = m.iter().fold(0.0f64, |a, &x| a.max(x.abs()));
    if !(det.abs() > 1e-300 && det.abs() > 1e-14 * size * size * size) {
        return None;
    }
    let k = 1.0 / det;
    Some([
        c[0] * k,
        (m[2] * m[7] - m[1] * m[8]) * k,
        (m[1] * m[5] - m[2] * m[4]) * k,
        c[1] * k,
        (m[0] * m[8] - m[2] * m[6]) * k,
        (m[2] * m[3] - m[0] * m[5]) * k,
        c[2] * k,
        (m[1] * m[6] - m[0] * m[7]) * k,
        (m[0] * m[4] - m[1] * m[3]) * k,
    ])
}

/// The eigenvalues of a symmetric matrix of size `N` (row-major), by cyclic Jacobi
/// rotations, in no particular order.
pub(crate) fn symmetric_eigenvalues<const N: usize>(m: [[f64; N]; N]) -> [f64; N] {
    let mut a = m;
    for _sweep in 0..60 {
        let off: f64 = (0..N)
            .flat_map(|i| (0..N).filter(move |&j| j != i).map(move |j| (i, j)))
            .map(|(i, j)| a[i][j] * a[i][j])
            .sum();
        let diag: f64 = (0..N).map(|i| a[i][i] * a[i][i]).sum();
        if off <= 1e-30 * diag.max(1e-300) {
            break;
        }
        for p in 0..N {
            for q in p + 1..N {
                if a[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for row in a.iter_mut() {
                    let (x, y) = (row[p], row[q]);
                    row[p] = c * x - s * y;
                    row[q] = s * x + c * y;
                }
                let (rp, rq) = (a[p], a[q]);
                a[p] = std::array::from_fn(|k| c * rp[k] - s * rq[k]);
                a[q] = std::array::from_fn(|k| s * rp[k] + c * rq[k]);
            }
        }
    }
    let mut out = [0.0; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = a[i][i];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_and_eigenvalues() {
        let m = [4.0, 1.0, 0.5, 1.0, 3.0, 0.2, 0.5, 0.2, 2.0];
        let inv = inverse(&m).unwrap();
        let p = mat_mul(&m, &inv);
        for (i, v) in p.iter().enumerate() {
            let expected = if i % 4 == 0 { 1.0 } else { 0.0 };
            assert!((v - expected).abs() < 1e-12);
        }
        assert!(inverse(&[1.0, 2.0, 3.0, 2.0, 4.0, 6.0, 0.0, 0.0, 1.0]).is_none());
        let mut e = symmetric_eigenvalues([[2.0, 1.0, 0.0], [1.0, 2.0, 0.0], [0.0, 0.0, 5.0]]);
        e.sort_by(f64::total_cmp);
        assert!((e[0] - 1.0).abs() < 1e-12);
        assert!((e[1] - 3.0).abs() < 1e-12);
        assert!((e[2] - 5.0).abs() < 1e-12);
    }
}
