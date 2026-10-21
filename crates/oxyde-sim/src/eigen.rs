//! The smallest eigenpairs of `A x = λ B x`, `A` and `B` symmetric and `B` positive
//! definite on the unknowns kept, by the locally optimal block preconditioned conjugate
//! gradient method (LOBPCG; Knyazev, 2001): what a frequency study needs of its stiffness
//! and mass ([`crate::modal`]), and what a frequency limit in generative design needs of
//! the voxel grid's.
//!
//! - **The operators are closures** on flat vectors, so the assembled matrices of a mesh
//!   and the matrix-free bricks of a grid can both be handed in. `project` keeps a vector
//!   to the unknowns solved for (held ones zeroed), so the modes of held unknowns never
//!   show.
//! - **Each iteration** takes the block of approximate eigenvectors `X`, the preconditioned
//!   residuals `W` of the pairs not yet converged and the last step's directions `P` of the
//!   same pairs, makes the three B-orthonormal together (modified Gram–Schmidt, twice,
//!   dropping a vector that depends on the others: the basis selection of Hetmaniuk and
//!   Lehoucq, 2006, which keeps the small problem well conditioned where the Cholesky
//!   factorization of the Gram matrix of `[X, W, P]` fails as the pairs converge), and
//!   solves the small symmetric eigenproblem of `A` on that basis (Rayleigh–Ritz, by cyclic
//!   Jacobi rotations). Its lowest pairs are the next `X`; the part of them that came from
//!   `W` and `P` is the next `P`. Every combination is applied to the vectors and to their
//!   products by `A` and `B` together, so an iteration costs one product by each and one
//!   preconditioning per pair not converged.
//! - **Guard vectors** beyond those wanted speed the last of them up: the block then holds
//!   the next eigenvalues too. A pair is converged when its residual `‖A x − λ B x‖` is under
//!   `tolerance` times `‖A x‖ + |λ| ‖B x‖` (a backward error, as meaningful for a zero
//!   eigenvalue as for a large one); it then stops being expanded (soft locking) but still
//!   takes part in the Rayleigh–Ritz.
//! - **Deterministic**: a fixed pseudo-random start when none is given, and every sum in a
//!   fixed order, so a run repeats exactly.

// Indices into the blocks and the small dense matrices, all sized here.
#![allow(clippy::indexing_slicing, clippy::needless_range_loop)]

use std::sync::atomic::{AtomicBool, Ordering};

/// A vector B-orthogonalized against the others is dropped when what is left of it is under
/// this share of its B-norm: it depends on them.
const DEPENDENT: f64 = 1e-8;
/// The block's B-orthonormality is restored every this many iterations, against rounding.
const REORTHONORMALIZE_EVERY: usize = 10;
/// Cyclic Jacobi: sweeps at most, and the size of what is off the diagonal, relative to the
/// whole matrix, it stops at.
const JACOBI_SWEEPS: usize = 100;
const JACOBI_TOLERANCE: f64 = 1e-15;
/// Attempts at a start block of independent vectors.
const START_ATTEMPTS: usize = 4;

/// The pencil `A − λ B`, as closures on vectors of `size` values.
pub(crate) struct EigenProblem<'a> {
    /// The length of a vector.
    pub size: usize,
    /// `y = A x`.
    pub apply_a: &'a dyn Fn(&[f64], &mut [f64]),
    /// `y = B x`.
    pub apply_b: &'a dyn Fn(&[f64], &mut [f64]),
    /// `z ≈ A⁻¹ r`: a symmetric positive preconditioner (a multigrid cycle, block-Jacobi).
    pub precondition: &'a mut dyn FnMut(&[f64], &mut [f64]),
    /// Keeps a vector to the unknowns solved for: zero at the held ones.
    pub project: &'a dyn Fn(&mut [f64]),
    /// Hears how far the wanted pairs are from converging, from 0 to 1.
    pub progress: &'a dyn Fn(f64),
}

/// What [`smallest`] looks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EigenOptions {
    /// The pairs wanted, lowest first.
    pub count: usize,
    /// Vectors more in the block, to speed the last wanted ones up.
    pub guard: usize,
    /// A pair is converged when `‖A x − λ B x‖ ≤ tolerance (‖A x‖ + |λ| ‖B x‖)`.
    pub tolerance: f64,
    pub max_iterations: usize,
}

/// The lowest eigenpairs found.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Eigenpairs {
    /// Ascending.
    pub values: Vec<f64>,
    /// B-orthonormal: `vᵢᵀ B vⱼ = δᵢⱼ`.
    pub vectors: Vec<Vec<f64>>,
    pub iterations: usize,
    /// Every wanted pair met the tolerance.
    pub converged: bool,
    /// The largest residual of the wanted pairs at the end, as the tolerance measures it.
    pub residual: f64,
}

/// Why no eigenpairs could be found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EigenError {
    Cancelled,
    /// No block of independent vectors could be made: fewer unknowns are kept than pairs
    /// are wanted, or `B` is zero on them.
    Degenerate,
}

/// A vector with its products by `A` and `B` (`av` empty until it is computed), combined
/// together so the three stay consistent without new products.
#[derive(Debug, Clone)]
struct Column {
    v: Vec<f64>,
    av: Vec<f64>,
    bv: Vec<f64>,
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn norm(a: &[f64]) -> f64 {
    dot(a, a).sqrt()
}

/// `y += c x`.
fn axpy(y: &mut [f64], c: f64, x: &[f64]) {
    for (y, x) in y.iter_mut().zip(x) {
        *y += c * x;
    }
}

fn scale(v: &mut [f64], c: f64) {
    for x in v {
        *x *= c;
    }
}

/// `f(v)` in a new vector.
fn apply(f: &dyn Fn(&[f64], &mut [f64]), v: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; v.len()];
    f(v, &mut out);
    out
}

/// The lowest `options.count` eigenpairs of `problem`, from the vectors of `start` (a
/// previous solve's, for a problem that changed a little) and pseudo-random ones for the
/// rest of the block. Out of iterations, the pairs as far as they got, `converged` false.
pub(crate) fn smallest(
    problem: &mut EigenProblem<'_>,
    options: &EigenOptions,
    start: Option<&[Vec<f64>]>,
    cancel: &AtomicBool,
) -> Result<Eigenpairs, EigenError> {
    let n = problem.size;
    let count = options.count.min(n);
    if count == 0 {
        return Ok(Eigenpairs {
            values: Vec::new(),
            vectors: Vec::new(),
            iterations: 0,
            converged: true,
            residual: 0.0,
        });
    }
    // The pairs wanted and the guard; a third of the unknowns at most, so that the basis of
    // three blocks is no larger than the space (all of it, for a tiny problem).
    let block = (count + options.guard).min((n / 3).max(count)).min(n);

    // The start: the vectors given, then pseudo-random ones, B-orthonormal.
    let mut x: Vec<Column> = Vec::with_capacity(block);
    let mut seed = 0u64;
    for attempt in 0..START_ATTEMPTS {
        let given: &[Vec<f64>] = if attempt == 0 {
            start.unwrap_or(&[])
        } else {
            &[]
        };
        let mut candidates = Vec::with_capacity(block - x.len());
        for k in 0..block - x.len() {
            let mut v = match given.get(k) {
                Some(g) if g.len() == n => g.clone(),
                _ => {
                    seed += 1;
                    random_vector(n, seed)
                }
            };
            (problem.project)(&mut v);
            let bv = apply(problem.apply_b, &v);
            candidates.push(Column {
                v,
                av: Vec::new(),
                bv,
            });
        }
        let kept = orthonormalize(&[x.as_slice()], candidates);
        x.extend(kept);
        if x.len() == block {
            break;
        }
    }
    if x.len() < count {
        return Err(EigenError::Degenerate);
    }
    for c in &mut x {
        c.av = apply(problem.apply_a, &c.v);
    }
    let (mut values, mut x, _) = {
        let basis: Vec<&Column> = x.iter().collect();
        rayleigh_ritz(&basis, basis.len(), basis.len(), n)
    };
    // The last step's directions, one per vector of the block (none at first).
    let mut directions: Vec<Column> = Vec::new();
    let mut residual = f64::INFINITY;
    for iteration in 0..options.max_iterations {
        if cancel.load(Ordering::Relaxed) {
            return Err(EigenError::Cancelled);
        }
        // The residuals, and the pairs not yet converged.
        let mut active: Vec<usize> = Vec::new();
        let mut residuals: Vec<Vec<f64>> = Vec::new();
        let mut worst = 0.0f64;
        for (i, c) in x.iter().enumerate() {
            let lambda = values[i];
            let r: Vec<f64> =
                c.av.iter()
                    .zip(&c.bv)
                    .map(|(a, b)| a - lambda * b)
                    .collect();
            let size = norm(&c.av) + lambda.abs() * norm(&c.bv);
            let relative = if size > 0.0 { norm(&r) / size } else { 0.0 };
            let relative = if relative.is_finite() {
                relative
            } else {
                f64::INFINITY
            };
            if i < count {
                worst = worst.max(relative);
            }
            if relative > options.tolerance {
                active.push(i);
                residuals.push(r);
            }
        }
        residual = worst;
        let done = if worst > options.tolerance && options.tolerance > 0.0 {
            ((1.0 / worst).ln() / (1.0 / options.tolerance).ln()).clamp(0.0, 1.0)
        } else {
            1.0
        };
        (problem.progress)(done);
        if worst <= options.tolerance {
            return Ok(finish(values, x, count, iteration, true, residual));
        }

        // The preconditioned residuals, B-orthonormal to the block and to each other, with
        // their products by A.
        let mut w = Vec::with_capacity(residuals.len());
        for r in &residuals {
            let mut z = vec![0.0; n];
            (problem.precondition)(r, &mut z);
            (problem.project)(&mut z);
            let bz = apply(problem.apply_b, &z);
            w.push(Column {
                v: z,
                av: Vec::new(),
                bv: bz,
            });
        }
        let mut w = orthonormalize(&[x.as_slice()], w);
        for c in &mut w {
            c.av = apply(problem.apply_a, &c.v);
        }
        // The last directions of the same pairs, B-orthonormal to both.
        let last: Vec<Column> = active
            .iter()
            .filter_map(|&i| directions.get(i).cloned())
            .collect();
        let p = orthonormalize(&[x.as_slice(), w.as_slice()], last);
        if w.is_empty() && p.is_empty() {
            // Nothing new to search: the residuals lie in the block already.
            return Ok(finish(values, x, count, iteration, false, residual));
        }
        let (next_values, next_x, next_p) = {
            let basis: Vec<&Column> = x.iter().chain(&w).chain(&p).collect();
            rayleigh_ritz(&basis, x.len(), x.len(), n)
        };
        values = next_values;
        x = next_x;
        directions = next_p;

        if (iteration + 1) % REORTHONORMALIZE_EVERY == 0 {
            let before = x.len();
            x = orthonormalize(&[], x);
            if x.len() < count {
                return Err(EigenError::Degenerate);
            }
            if x.len() != before {
                let basis: Vec<&Column> = x.iter().collect();
                let (v, again, _) = rayleigh_ritz(&basis, basis.len(), basis.len(), n);
                values = v;
                x = again;
                directions.clear();
            }
        }
    }
    Ok(finish(
        values,
        x,
        count,
        options.max_iterations,
        false,
        residual,
    ))
}

/// The first `count` pairs of the block.
fn finish(
    values: Vec<f64>,
    x: Vec<Column>,
    count: usize,
    iterations: usize,
    converged: bool,
    residual: f64,
) -> Eigenpairs {
    Eigenpairs {
        values: values.into_iter().take(count).collect(),
        vectors: x.into_iter().take(count).map(|c| c.v).collect(),
        iterations,
        converged,
        residual,
    }
}

/// `candidates` made B-orthonormal to the B-orthonormal blocks of `basis` and among
/// themselves, by modified Gram–Schmidt twice: each loses its components along the others
/// (its `A` product too when it has one, the basis then having theirs); one left with under
/// [`DEPENDENT`] of its B-norm is dropped. The candidates kept, in order.
fn orthonormalize(basis: &[&[Column]], candidates: Vec<Column>) -> Vec<Column> {
    let mut out: Vec<Column> = Vec::with_capacity(candidates.len());
    for mut c in candidates {
        let start = dot(&c.v, &c.bv);
        if !(start > 0.0 && start.is_finite()) {
            continue;
        }
        for _ in 0..2 {
            for u in basis.iter().flat_map(|b| b.iter()).chain(out.iter()) {
                let k = dot(&u.bv, &c.v);
                if k == 0.0 {
                    continue;
                }
                axpy(&mut c.v, -k, &u.v);
                axpy(&mut c.bv, -k, &u.bv);
                if !c.av.is_empty() {
                    axpy(&mut c.av, -k, &u.av);
                }
            }
        }
        let left = dot(&c.v, &c.bv);
        if !(left > DEPENDENT * DEPENDENT * start && left.is_finite()) {
            continue;
        }
        let s = 1.0 / left.sqrt();
        scale(&mut c.v, s);
        scale(&mut c.bv, s);
        scale(&mut c.av, s);
        out.push(c);
    }
    out
}

/// `Σ coefficient(k) · columns[k]`, with the products.
fn combine(columns: &[&Column], coefficient: impl Fn(usize) -> f64, n: usize) -> Column {
    let mut out = Column {
        v: vec![0.0; n],
        av: vec![0.0; n],
        bv: vec![0.0; n],
    };
    for (k, c) in columns.iter().enumerate() {
        let y = coefficient(k);
        if y == 0.0 {
            continue;
        }
        axpy(&mut out.v, y, &c.v);
        axpy(&mut out.av, y, &c.av);
        axpy(&mut out.bv, y, &c.bv);
    }
    out
}

/// Rayleigh–Ritz on the B-orthonormal `basis` (each column with its products): the lowest
/// `keep` eigenpairs of `Sᵀ A S`, as values and vectors, and the part of each vector that
/// comes from the columns from `split` on (the next search directions).
fn rayleigh_ritz(
    basis: &[&Column],
    keep: usize,
    split: usize,
    n: usize,
) -> (Vec<f64>, Vec<Column>, Vec<Column>) {
    let s = basis.len();
    let mut g = vec![0.0; s * s];
    for i in 0..s {
        for j in i..s {
            // Symmetric to rounding: the mean of the two products.
            let v = 0.5 * (dot(&basis[i].v, &basis[j].av) + dot(&basis[j].v, &basis[i].av));
            g[i * s + j] = v;
            g[j * s + i] = v;
        }
    }
    let (theta, y) = symmetric_eigen(&g, s);
    let keep = keep.min(s);
    let mut x = Vec::with_capacity(keep);
    let mut p = Vec::with_capacity(keep);
    for j in 0..keep {
        x.push(combine(basis, |k| y[k * s + j], n));
        if split < s {
            p.push(combine(&basis[split..], |k| y[(split + k) * s + j], n));
        }
    }
    (theta.into_iter().take(keep).collect(), x, p)
}

/// The eigenvalues of the symmetric `a` (`n × n`, row-major), ascending, and its
/// eigenvectors as the columns of the matrix returned (row-major), by cyclic Jacobi
/// rotations: slow for large matrices, accurate to rounding and simple for the small ones
/// of a Rayleigh–Ritz step.
fn symmetric_eigen(a: &[f64], n: usize) -> (Vec<f64>, Vec<f64>) {
    let mut a = a.to_vec();
    let mut v = vec![0.0; n * n];
    for i in 0..n {
        v[i * n + i] = 1.0;
    }
    let total = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    for _ in 0..JACOBI_SWEEPS {
        let mut off = 0.0;
        for i in 0..n {
            for j in 0..n {
                if i != j {
                    off += a[i * n + j] * a[i * n + j];
                }
            }
        }
        if off.sqrt() <= JACOBI_TOLERANCE * total {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                let apq = a[p * n + q];
                if apq == 0.0 {
                    continue;
                }
                // The rotation that zeroes a[p][q]: cot 2φ = θ, t = tan φ, the smaller root.
                let theta = (a[q * n + q] - a[p * n + p]) / (2.0 * apq);
                let t = if theta.abs() > 1e150 {
                    0.5 / theta
                } else {
                    theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt())
                };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let (akp, akq) = (a[k * n + p], a[k * n + q]);
                    a[k * n + p] = c * akp - s * akq;
                    a[k * n + q] = s * akp + c * akq;
                }
                for k in 0..n {
                    let (apk, aqk) = (a[p * n + k], a[q * n + k]);
                    a[p * n + k] = c * apk - s * aqk;
                    a[q * n + k] = s * apk + c * aqk;
                }
                for k in 0..n {
                    let (vkp, vkq) = (v[k * n + p], v[k * n + q]);
                    v[k * n + p] = c * vkp - s * vkq;
                    v[k * n + q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&i, &j| a[i * n + i].total_cmp(&a[j * n + j]).then(i.cmp(&j)));
    let values = order.iter().map(|&i| a[i * n + i]).collect();
    let mut vectors = vec![0.0; n * n];
    for (column, &i) in order.iter().enumerate() {
        for r in 0..n {
            vectors[r * n + column] = v[r * n + i];
        }
    }
    (values, vectors)
}

/// `n` pseudo-random values between −½ and ½, the same for the same `seed` (xorshift64*).
fn random_vector(n: usize, seed: u64) -> Vec<f64> {
    let mut state = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x2545_F491_4F6C_DD1D)
        | 1;
    (0..n)
        .map(|_| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let r = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
            (r >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A symmetric tridiagonal matrix times `x`: `diagonal` on it, `off` beside it.
    fn tridiagonal(diagonal: f64, off: f64, x: &[f64], y: &mut [f64]) {
        let n = x.len();
        for i in 0..n {
            let mut s = diagonal * x[i];
            if i > 0 {
                s += off * x[i - 1];
            }
            if i + 1 < n {
                s += off * x[i + 1];
            }
            y[i] = s;
        }
    }

    fn options(count: usize) -> EigenOptions {
        EigenOptions {
            count,
            guard: 3,
            tolerance: 1e-9,
            max_iterations: 2000,
        }
    }

    /// Whether `vectors` are B-orthonormal for `b`.
    fn b_orthonormal(vectors: &[Vec<f64>], b: &dyn Fn(&[f64], &mut [f64])) -> bool {
        vectors.iter().enumerate().all(|(i, u)| {
            let bu = apply(b, u);
            vectors.iter().enumerate().all(|(j, v)| {
                let expected = if i == j { 1.0 } else { 0.0 };
                (dot(v, &bu) - expected).abs() < 1e-9
            })
        })
    }

    #[test]
    fn jacobi_rotations_diagonalize_a_symmetric_matrix() {
        let a = [4.0, 1.0, 0.5, 1.0, 3.0, 0.2, 0.5, 0.2, 2.0];
        let (values, vectors) = symmetric_eigen(&a, 3);
        assert!(values.windows(2).all(|w| w[0] <= w[1]));
        for j in 0..3 {
            for i in 0..3 {
                let av: f64 = (0..3).map(|k| a[i * 3 + k] * vectors[k * 3 + j]).sum();
                assert!(
                    (av - values[j] * vectors[i * 3 + j]).abs() < 1e-12,
                    "{i} {j}"
                );
            }
        }
        let trace: f64 = values.iter().sum();
        assert!((trace - 9.0).abs() < 1e-12);
    }

    #[test]
    fn a_diagonal_pencil_gives_its_smallest_values() {
        let n = 40;
        let d: Vec<f64> = (0..n).map(|i| 1.0 + i as f64).collect();
        let a = |x: &[f64], y: &mut [f64]| {
            for i in 0..x.len() {
                y[i] = d[i] * x[i];
            }
        };
        let b = |x: &[f64], y: &mut [f64]| y.copy_from_slice(x);
        let mut precondition = |r: &[f64], z: &mut [f64]| {
            for i in 0..r.len() {
                z[i] = r[i] / d[i];
            }
        };
        let mut problem = EigenProblem {
            size: n,
            apply_a: &a,
            apply_b: &b,
            precondition: &mut precondition,
            project: &|_: &mut [f64]| {},
            progress: &|_| {},
        };
        let cancel = AtomicBool::new(false);
        let found = smallest(&mut problem, &options(4), None, &cancel).unwrap();
        assert!(found.converged);
        for (k, v) in found.values.iter().enumerate() {
            assert!((v - (k as f64 + 1.0)).abs() < 1e-8, "{k}: {v}");
        }
        assert!(b_orthonormal(&found.vectors, &b));
    }

    #[test]
    fn a_string_vibrates_at_its_known_frequencies() {
        // The 1-D Laplacian, held at both ends: eigenvalues 2 − 2 cos(kπ/(n+1)); with the
        // consistent mass of linear elements, (1 4 1)/6, 6 (1 − cos θ)/(2 + cos θ).
        let n = 60;
        let a = |x: &[f64], y: &mut [f64]| tridiagonal(2.0, -1.0, x, y);
        let identity = |x: &[f64], y: &mut [f64]| y.copy_from_slice(x);
        let mass = |x: &[f64], y: &mut [f64]| tridiagonal(4.0 / 6.0, 1.0 / 6.0, x, y);
        let cancel = AtomicBool::new(false);
        for (b, consistent) in [
            (&identity as &dyn Fn(&[f64], &mut [f64]), false),
            (&mass as &dyn Fn(&[f64], &mut [f64]), true),
        ] {
            let mut precondition = |r: &[f64], z: &mut [f64]| {
                for i in 0..r.len() {
                    z[i] = r[i] / 2.0;
                }
            };
            let mut problem = EigenProblem {
                size: n,
                apply_a: &a,
                apply_b: b,
                precondition: &mut precondition,
                project: &|_: &mut [f64]| {},
                progress: &|_| {},
            };
            let found = smallest(&mut problem, &options(5), None, &cancel).unwrap();
            assert!(found.converged, "{}", found.residual);
            for (k, v) in found.values.iter().enumerate() {
                let theta = (k as f64 + 1.0) * std::f64::consts::PI / (n as f64 + 1.0);
                let expected = if consistent {
                    6.0 * (1.0 - theta.cos()) / (2.0 + theta.cos())
                } else {
                    2.0 - 2.0 * theta.cos()
                };
                assert!(
                    (v - expected).abs() < 1e-8 * expected.max(1.0),
                    "{k}: {v} {expected}"
                );
            }
            assert!(b_orthonormal(&found.vectors, b));
        }
    }

    #[test]
    fn repeated_values_are_all_found_and_held_unknowns_never_move() {
        // 1 three times, then 2, 3…; unknown 0 held (its diagonal 0.5 would be the lowest).
        let n = 30;
        let d: Vec<f64> = (0..n)
            .map(|i| match i {
                0 => 0.5,
                1..=3 => 1.0,
                _ => i as f64 - 2.0,
            })
            .collect();
        let a = |x: &[f64], y: &mut [f64]| {
            for i in 0..x.len() {
                y[i] = d[i] * x[i];
            }
        };
        let b = |x: &[f64], y: &mut [f64]| y.copy_from_slice(x);
        let project = |v: &mut [f64]| v[0] = 0.0;
        let mut precondition = |r: &[f64], z: &mut [f64]| {
            for i in 0..r.len() {
                z[i] = r[i] / d[i];
            }
        };
        let mut problem = EigenProblem {
            size: n,
            apply_a: &a,
            apply_b: &b,
            precondition: &mut precondition,
            project: &project,
            progress: &|_| {},
        };
        let cancel = AtomicBool::new(false);
        let found = smallest(&mut problem, &options(4), None, &cancel).unwrap();
        assert!(found.converged);
        for (v, expected) in found.values.iter().zip([1.0, 1.0, 1.0, 2.0]) {
            assert!((v - expected).abs() < 1e-8, "{v} {expected}");
        }
        assert!(found.vectors.iter().all(|v| v[0] == 0.0));
        assert!(b_orthonormal(&found.vectors, &b));
    }

    #[test]
    fn a_warm_start_converges_sooner_and_the_same_run_repeats() {
        let n = 80;
        let cancel = AtomicBool::new(false);
        let solve = |stiffer: f64, start: Option<&[Vec<f64>]>| {
            let a = |x: &[f64], y: &mut [f64]| tridiagonal(2.0 * stiffer, -stiffer, x, y);
            let b = |x: &[f64], y: &mut [f64]| tridiagonal(4.0 / 6.0, 1.0 / 6.0, x, y);
            let mut precondition = |r: &[f64], z: &mut [f64]| {
                for i in 0..r.len() {
                    z[i] = r[i] / (2.0 * stiffer);
                }
            };
            let mut problem = EigenProblem {
                size: n,
                apply_a: &a,
                apply_b: &b,
                precondition: &mut precondition,
                project: &|_: &mut [f64]| {},
                progress: &|_| {},
            };
            smallest(&mut problem, &options(3), start, &cancel).unwrap()
        };
        let cold = solve(1.0, None);
        assert_eq!(cold, solve(1.0, None));
        // A little stiffer: the same vectors, eigenvalues 1 % higher.
        let warm = solve(1.01, Some(cold.vectors.as_slice()));
        assert!(warm.converged && cold.converged);
        assert!(
            warm.iterations < cold.iterations,
            "{} {}",
            warm.iterations,
            cold.iterations
        );
        for (w, c) in warm.values.iter().zip(&cold.values) {
            assert!((w / c - 1.01).abs() < 1e-7);
        }
    }

    #[test]
    fn a_cancelled_solve_says_so() {
        let n = 20;
        let a = |x: &[f64], y: &mut [f64]| tridiagonal(2.0, -1.0, x, y);
        let b = |x: &[f64], y: &mut [f64]| y.copy_from_slice(x);
        let mut precondition = |r: &[f64], z: &mut [f64]| z.copy_from_slice(r);
        let mut problem = EigenProblem {
            size: n,
            apply_a: &a,
            apply_b: &b,
            precondition: &mut precondition,
            project: &|_: &mut [f64]| {},
            progress: &|_| {},
        };
        let cancel = AtomicBool::new(true);
        assert_eq!(
            smallest(&mut problem, &options(2), None, &cancel),
            Err(EigenError::Cancelled)
        );
    }
}
