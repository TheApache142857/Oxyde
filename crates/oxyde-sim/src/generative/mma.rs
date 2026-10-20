//! The method of moving asymptotes (Svanberg, 1987; the 2007 version of the subproblem and
//! its primal-dual solver), for *Minimize mass*: many design variables, a few constraints.
//!
//! Each step replaces the objective and the constraints by convex approximations built on
//! two asymptotes per variable, which move in when the variable oscillates and out when it
//! goes one way, and solves that subproblem exactly by a primal-dual interior point method
//! on its few dual variables. The problem is the standard one: minimize `f0(x)` subject to
//! `fᵢ(x) ≤ 0`, `xmin ≤ x ≤ xmax`, with Svanberg's artificial variables (`a0 = 1`,
//! `aᵢ = 0`, `cᵢ = 1000`, `dᵢ = 1`) that keep the subproblem feasible.

// Indices into vectors all sized here from the variables and the constraints; one index
// runs through several of them, as Svanberg's formulas are written.
#![allow(clippy::indexing_slicing, clippy::needless_range_loop)]

/// The asymptotes' start, and how they move.
const ASYMPTOTE_START: f64 = 0.5;
const ASYMPTOTE_IN: f64 = 0.7;
const ASYMPTOTE_OUT: f64 = 1.2;
const ALBEFA: f64 = 0.1;
const RAA0: f64 = 1e-5;
/// The artificial variables' costs (Svanberg's choice for a standard problem).
const C: f64 = 1000.0;
const D: f64 = 1.0;

/// The state MMA carries from one step to the next: the last two designs and the
/// asymptotes.
#[derive(Debug, Clone, Default)]
pub(super) struct Mma {
    xold1: Vec<f64>,
    xold2: Vec<f64>,
    low: Vec<f64>,
    upp: Vec<f64>,
    step: usize,
}

impl Mma {
    /// The next design from `x`, within `xmin..xmax` and `move_limit` of it: `f0` and its
    /// slopes `df0`, the constraints' values `f` (each ≤ 0 when met) and their slopes `df`
    /// (one vector per constraint).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn update(
        &mut self,
        x: &[f64],
        xmin: f64,
        xmax: f64,
        move_limit: f64,
        df0: &[f64],
        f: &[f64],
        df: &[Vec<f64>],
    ) -> Vec<f64> {
        let n = x.len();
        let m = f.len();
        self.step += 1;
        let span = (xmax - xmin).max(1e-5);
        if self.step <= 2 || self.low.len() != n {
            self.low = x.iter().map(|xj| xj - ASYMPTOTE_START * span).collect();
            self.upp = x.iter().map(|xj| xj + ASYMPTOTE_START * span).collect();
        } else {
            for j in 0..n {
                let zz = (x[j] - self.xold1[j]) * (self.xold1[j] - self.xold2[j]);
                let factor = if zz > 0.0 {
                    ASYMPTOTE_OUT
                } else if zz < 0.0 {
                    ASYMPTOTE_IN
                } else {
                    1.0
                };
                let low = x[j] - factor * (self.xold1[j] - self.low[j]);
                let upp = x[j] + factor * (self.upp[j] - self.xold1[j]);
                self.low[j] = low.clamp(x[j] - 10.0 * span, x[j] - 0.01 * span);
                self.upp[j] = upp.clamp(x[j] + 0.01 * span, x[j] + 10.0 * span);
            }
        }
        let alfa: Vec<f64> = (0..n)
            .map(|j| {
                (self.low[j] + ALBEFA * (x[j] - self.low[j]))
                    .max(x[j] - move_limit * span)
                    .max(xmin)
            })
            .collect();
        let beta: Vec<f64> = (0..n)
            .map(|j| {
                (self.upp[j] - ALBEFA * (self.upp[j] - x[j]))
                    .min(x[j] + move_limit * span)
                    .min(xmax)
            })
            .collect();
        // The approximations' coefficients.
        let mut p0 = vec![0.0; n];
        let mut q0 = vec![0.0; n];
        let mut pm = vec![vec![0.0; n]; m];
        let mut qm = vec![vec![0.0; n]; m];
        let mut b = vec![0.0; m];
        for j in 0..n {
            let ux = self.upp[j] - x[j];
            let xl = x[j] - self.low[j];
            let (ux2, xl2) = (ux * ux, xl * xl);
            let (p, q) = (df0[j].max(0.0), (-df0[j]).max(0.0));
            let pq = 0.001 * (p + q) + RAA0 / span;
            p0[j] = (p + pq) * ux2;
            q0[j] = (q + pq) * xl2;
            for i in 0..m {
                let g = df[i][j];
                let (p, q) = (g.max(0.0), (-g).max(0.0));
                let pq = 0.001 * (p + q) + RAA0 / span;
                pm[i][j] = (p + pq) * ux2;
                qm[i][j] = (q + pq) * xl2;
                b[i] += pm[i][j] / ux + qm[i][j] / xl;
            }
        }
        for i in 0..m {
            b[i] -= f[i];
        }
        let next = subsolve(&Subproblem {
            low: &self.low,
            upp: &self.upp,
            alfa: &alfa,
            beta: &beta,
            p0: &p0,
            q0: &q0,
            p: &pm,
            q: &qm,
            b: &b,
        });
        self.xold2 = std::mem::replace(&mut self.xold1, x.to_vec());
        if self.xold2.is_empty() {
            self.xold2 = x.to_vec();
        }
        next
    }
}

/// MMA's subproblem: minimize `Σⱼ p0ⱼ/(uⱼ − xⱼ) + q0ⱼ/(xⱼ − lⱼ) + z + Σᵢ (c yᵢ + d yᵢ²/2)`
/// subject to `Σⱼ pᵢⱼ/(uⱼ − xⱼ) + qᵢⱼ/(xⱼ − lⱼ) − yᵢ ≤ bᵢ`, `αⱼ ≤ xⱼ ≤ βⱼ`, `y, z ≥ 0`.
struct Subproblem<'a> {
    low: &'a [f64],
    upp: &'a [f64],
    alfa: &'a [f64],
    beta: &'a [f64],
    p0: &'a [f64],
    q0: &'a [f64],
    p: &'a [Vec<f64>],
    q: &'a [Vec<f64>],
    b: &'a [f64],
}

/// The interior point method's variables.
#[derive(Clone)]
struct Point {
    x: Vec<f64>,
    y: Vec<f64>,
    z: f64,
    lam: Vec<f64>,
    xsi: Vec<f64>,
    eta: Vec<f64>,
    mu: Vec<f64>,
    zet: f64,
    s: Vec<f64>,
}

impl Subproblem<'_> {
    /// `p0 + Pᵀλ` and `q0 + Qᵀλ` at `x`'s variables.
    fn plam_qlam(&self, lam: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let n = self.p0.len();
        let mut plam = self.p0.to_vec();
        let mut qlam = self.q0.to_vec();
        for (i, l) in lam.iter().enumerate() {
            for j in 0..n {
                plam[j] += self.p[i][j] * l;
                qlam[j] += self.q[i][j] * l;
            }
        }
        (plam, qlam)
    }

    /// `Σⱼ pᵢⱼ/(uⱼ − xⱼ) + qᵢⱼ/(xⱼ − lⱼ)` for each constraint.
    fn gvec(&self, x: &[f64]) -> Vec<f64> {
        self.p
            .iter()
            .zip(self.q)
            .map(|(p, q)| {
                (0..x.len())
                    .map(|j| p[j] / (self.upp[j] - x[j]) + q[j] / (x[j] - self.low[j]))
                    .sum()
            })
            .collect()
    }

    /// The residual of the perturbed optimality conditions at `pt`.
    fn residual(&self, pt: &Point, epsi: f64) -> Vec<f64> {
        let n = pt.x.len();
        let (plam, qlam) = self.plam_qlam(&pt.lam);
        let gvec = self.gvec(&pt.x);
        let mut r = Vec::with_capacity(4 * n + 4 * pt.lam.len() + 2);
        for j in 0..n {
            let ux = self.upp[j] - pt.x[j];
            let xl = pt.x[j] - self.low[j];
            r.push(plam[j] / (ux * ux) - qlam[j] / (xl * xl) - pt.xsi[j] + pt.eta[j]);
        }
        for i in 0..pt.y.len() {
            r.push(C + D * pt.y[i] - pt.mu[i] - pt.lam[i]);
        }
        // a0 = 1, a = 0.
        r.push(1.0 - pt.zet);
        for i in 0..pt.lam.len() {
            r.push(gvec[i] - pt.y[i] + pt.s[i] - self.b[i]);
        }
        for j in 0..n {
            r.push(pt.xsi[j] * (pt.x[j] - self.alfa[j]) - epsi);
            r.push(pt.eta[j] * (self.beta[j] - pt.x[j]) - epsi);
        }
        for i in 0..pt.y.len() {
            r.push(pt.mu[i] * pt.y[i] - epsi);
            r.push(pt.lam[i] * pt.s[i] - epsi);
        }
        r.push(pt.zet * pt.z - epsi);
        r
    }
}

fn norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

fn max_abs(v: &[f64]) -> f64 {
    v.iter().fold(0.0, |m, x| m.max(x.abs()))
}

/// Solves `a x = b` in place by Gaussian elimination with partial pivoting (the system is
/// the dual one: as small as the constraints are few).
fn solve_dense(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Vec<f64> {
    let k = b.len();
    for col in 0..k {
        let pivot = (col..k)
            .max_by(|&r, &s| a[r][col].abs().total_cmp(&a[s][col].abs()))
            .unwrap_or(col);
        a.swap(col, pivot);
        b.swap(col, pivot);
        let d = a[col][col];
        if d == 0.0 {
            continue;
        }
        for row in col + 1..k {
            let f = a[row][col] / d;
            if f == 0.0 {
                continue;
            }
            for c in col..k {
                a[row][c] -= f * a[col][c];
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = vec![0.0; k];
    for row in (0..k).rev() {
        let s: f64 = (row + 1..k).map(|c| a[row][c] * x[c]).sum();
        x[row] = if a[row][row] == 0.0 {
            0.0
        } else {
            (b[row] - s) / a[row][row]
        };
    }
    x
}

/// Svanberg's `subsolv`: the subproblem solved by a primal-dual Newton method, the barrier
/// parameter lowered tenfold until it is under 10⁻⁷. Returns the new design.
fn subsolve(sp: &Subproblem<'_>) -> Vec<f64> {
    let n = sp.p0.len();
    let m = sp.b.len();
    let mut epsi = 1.0;
    let x: Vec<f64> = (0..n).map(|j| 0.5 * (sp.alfa[j] + sp.beta[j])).collect();
    let mut pt = Point {
        xsi: (0..n)
            .map(|j| (1.0 / (x[j] - sp.alfa[j])).max(1.0))
            .collect(),
        eta: (0..n)
            .map(|j| (1.0 / (sp.beta[j] - x[j])).max(1.0))
            .collect(),
        x,
        y: vec![1.0; m],
        z: 1.0,
        lam: vec![1.0; m],
        mu: vec![(0.5 * C).max(1.0); m],
        zet: 1.0,
        s: vec![1.0; m],
    };
    while epsi > 1e-7 {
        let mut residual = sp.residual(&pt, epsi);
        let mut residual_norm = norm(&residual);
        let mut tries = 0;
        while max_abs(&residual) > 0.9 * epsi && tries < 200 {
            tries += 1;
            let (plam, qlam) = sp.plam_qlam(&pt.lam);
            let gvec = sp.gvec(&pt.x);
            // GG: m × n, the constraints' slopes in x.
            let mut gg = vec![vec![0.0; n]; m];
            let mut delx = vec![0.0; n];
            let mut diagx = vec![0.0; n];
            for j in 0..n {
                let ux = sp.upp[j] - pt.x[j];
                let xl = pt.x[j] - sp.low[j];
                let (ux2, xl2) = (ux * ux, xl * xl);
                for i in 0..m {
                    gg[i][j] = sp.p[i][j] / ux2 - sp.q[i][j] / xl2;
                }
                let dpsidx = plam[j] / ux2 - qlam[j] / xl2;
                delx[j] = dpsidx - epsi / (pt.x[j] - sp.alfa[j]) + epsi / (sp.beta[j] - pt.x[j]);
                diagx[j] = 2.0 * (plam[j] / (ux2 * ux) + qlam[j] / (xl2 * xl))
                    + pt.xsi[j] / (pt.x[j] - sp.alfa[j])
                    + pt.eta[j] / (sp.beta[j] - pt.x[j]);
            }
            let dely: Vec<f64> = (0..m)
                .map(|i| C + D * pt.y[i] - pt.lam[i] - epsi / pt.y[i])
                .collect();
            let delz = 1.0 - epsi / pt.z;
            let dellam: Vec<f64> = (0..m)
                .map(|i| gvec[i] - pt.y[i] - sp.b[i] + epsi / pt.lam[i])
                .collect();
            let diagy: Vec<f64> = (0..m).map(|i| D + pt.mu[i] / pt.y[i]).collect();
            let diaglamyi: Vec<f64> = (0..m)
                .map(|i| pt.s[i] / pt.lam[i] + 1.0 / diagy[i])
                .collect();
            // The dual system, (m + 1) × (m + 1): a = 0, so z's row stands apart.
            let mut big = vec![vec![0.0; m + 1]; m + 1];
            let mut rhs = vec![0.0; m + 1];
            for i in 0..m {
                big[i][i] = diaglamyi[i];
                for k in 0..m {
                    big[i][k] += (0..n).map(|j| gg[i][j] * gg[k][j] / diagx[j]).sum::<f64>();
                }
                rhs[i] = dellam[i] + dely[i] / diagy[i]
                    - (0..n).map(|j| gg[i][j] * delx[j] / diagx[j]).sum::<f64>();
            }
            big[m][m] = -pt.zet / pt.z;
            rhs[m] = delz;
            let solution = solve_dense(big, rhs);
            let dlam = &solution[..m];
            let dz = solution[m];
            let dx: Vec<f64> = (0..n)
                .map(|j| {
                    let g: f64 = (0..m).map(|i| gg[i][j] * dlam[i]).sum();
                    -delx[j] / diagx[j] - g / diagx[j]
                })
                .collect();
            let dy: Vec<f64> = (0..m)
                .map(|i| -dely[i] / diagy[i] + dlam[i] / diagy[i])
                .collect();
            let dxsi: Vec<f64> = (0..n)
                .map(|j| {
                    let d = pt.x[j] - sp.alfa[j];
                    -pt.xsi[j] + epsi / d - pt.xsi[j] * dx[j] / d
                })
                .collect();
            let deta: Vec<f64> = (0..n)
                .map(|j| {
                    let d = sp.beta[j] - pt.x[j];
                    -pt.eta[j] + epsi / d + pt.eta[j] * dx[j] / d
                })
                .collect();
            let dmu: Vec<f64> = (0..m)
                .map(|i| -pt.mu[i] + epsi / pt.y[i] - pt.mu[i] * dy[i] / pt.y[i])
                .collect();
            let dzet = -pt.zet + epsi / pt.z - pt.zet * dz / pt.z;
            let ds: Vec<f64> = (0..m)
                .map(|i| -pt.s[i] + epsi / pt.lam[i] - pt.s[i] * dlam[i] / pt.lam[i])
                .collect();
            // The longest step keeping every variable inside its bounds.
            let mut most: f64 = 1.0;
            let mut push = |value: f64, step: f64| {
                let r = -1.01 * step / value;
                if r > most {
                    most = r;
                }
            };
            for i in 0..m {
                push(pt.y[i], dy[i]);
                push(pt.lam[i], dlam[i]);
                push(pt.mu[i], dmu[i]);
                push(pt.s[i], ds[i]);
            }
            push(pt.z, dz);
            push(pt.zet, dzet);
            for j in 0..n {
                push(pt.xsi[j], dxsi[j]);
                push(pt.eta[j], deta[j]);
                push(pt.x[j] - sp.alfa[j], dx[j]);
                push(sp.beta[j] - pt.x[j], -dx[j]);
            }
            let mut step = 1.0 / most;
            let old = pt.clone();
            let mut tries_back = 0;
            loop {
                tries_back += 1;
                let add = |a: &[f64], d: &[f64]| -> Vec<f64> {
                    a.iter().zip(d).map(|(a, d)| a + step * d).collect()
                };
                pt = Point {
                    x: add(&old.x, &dx),
                    y: add(&old.y, &dy),
                    z: old.z + step * dz,
                    lam: add(&old.lam, dlam),
                    xsi: add(&old.xsi, &dxsi),
                    eta: add(&old.eta, &deta),
                    mu: add(&old.mu, &dmu),
                    zet: old.zet + step * dzet,
                    s: add(&old.s, &ds),
                };
                residual = sp.residual(&pt, epsi);
                let new_norm = norm(&residual);
                if new_norm <= residual_norm || tries_back >= 50 {
                    residual_norm = new_norm;
                    break;
                }
                step /= 2.0;
            }
        }
        epsi *= 0.1;
    }
    pt.x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Svanberg's first test problem: minimize `x₁² + x₂² + x₃²` subject to
    /// `(x₁ − 5)² + (x₂ − 2)² + (x₃ − 1)² ≤ 9` and `(x₁ − 3)² + (x₂ − 4)² + (x₃ − 3)² ≤ 9`,
    /// `0 ≤ x ≤ 5`; the optimum is about (2.0175, 1.7800, 1.2375).
    #[test]
    fn the_test_problem_converges_to_its_optimum() {
        let mut mma = Mma::default();
        let mut x = vec![4.0, 3.0, 2.0];
        for _ in 0..60 {
            let df0: Vec<f64> = x.iter().map(|v| 2.0 * v).collect();
            let c1 = [5.0, 2.0, 1.0];
            let c2 = [3.0, 4.0, 3.0];
            let g =
                |c: [f64; 3]| -> f64 { (0..3).map(|j| (x[j] - c[j]).powi(2)).sum::<f64>() - 9.0 };
            let dg = |c: [f64; 3]| -> Vec<f64> { (0..3).map(|j| 2.0 * (x[j] - c[j])).collect() };
            let f = [g(c1), g(c2)];
            let df = vec![dg(c1), dg(c2)];
            x = mma.update(&x, 0.0, 5.0, 1.0, &df0, &f, &df);
        }
        let expected = [2.0175, 1.7800, 1.2375];
        for j in 0..3 {
            assert!((x[j] - expected[j]).abs() < 2e-3, "{x:?}");
        }
    }

    #[test]
    fn a_linear_objective_meets_its_constraint() {
        // Minimize Σ x over 50 variables with Σ 1/x ≤ 100: every x = 0.5 at the optimum.
        let n = 50;
        let mut mma = Mma::default();
        let mut x = vec![1.0; n];
        for _ in 0..80 {
            let df0 = vec![1.0; n];
            let f = [x.iter().map(|v| 1.0 / v).sum::<f64>() - 100.0];
            let df = vec![x.iter().map(|v| -1.0 / (v * v)).collect::<Vec<_>>()];
            x = mma.update(&x, 0.01, 1.0, 0.2, &df0, &f, &df);
        }
        assert!(x.iter().all(|v| (v - 0.5).abs() < 1e-3), "{:?}", &x[..3]);
    }
}
