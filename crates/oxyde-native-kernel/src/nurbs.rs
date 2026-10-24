//! B-spline curves and surfaces, rational or not (NURBS), written from the mathematics (the
//! algorithms of Piegl and Tiller's *The NURBS Book*): evaluation with derivatives, knot
//! insertion, the nearest point, where a line crosses a surface, and the polynomial form
//! OpenCASCADE gives a circular arc where a loft needs one ([`polynomial_arc`], observed as
//! a black box).
//!
//! The kernel's curves and surfaces are small values that copy; a B-spline's poles are not.
//! Each B-spline made is kept once for the life of the process ([`intern_curve`],
//! [`intern_surface`]; the same one made again is found again), and the kernel refers to it
//! with the rigid motion that places it ([`SplineCurve`], [`SplineSurface`]), so moving or
//! patterning a part never copies its B-splines.

// Indices into knots and poles, sized here.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Mutex, OnceLock, PoisonError};

use oxyde_geom::{Point3, Transform, Vec3};

/// A homogeneous point `(w x, w y, w z, w)`.
pub(crate) type H = [f64; 4];

pub(crate) fn homogeneous(p: Point3, w: f64) -> H {
    [p.x * w, p.y * w, p.z * w, w]
}

pub(crate) fn euclidean(h: H) -> Point3 {
    Point3::new(h[0] / h[3], h[1] / h[3], h[2] / h[3])
}

fn lerp(a: H, b: H, t: f64) -> H {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

fn binomial(n: usize, k: usize) -> f64 {
    let mut out = 1.0;
    for i in 0..k {
        out = out * (n - i) as f64 / (i + 1) as f64;
    }
    out
}

/// The knot span holding `u`: the `i` with `knots[i] <= u < knots[i + 1]`, `degree <= i <
/// count`, the last span that is not empty at the end of the domain and the first at its
/// start.
pub(crate) fn find_span(knots: &[f64], degree: usize, count: usize, u: f64) -> usize {
    let n = count - 1;
    if u >= knots[n + 1] {
        let mut i = n;
        while i > degree && knots[i] >= knots[i + 1] {
            i -= 1;
        }
        return i;
    }
    if u <= knots[degree] {
        let mut i = degree;
        while i < n && knots[i] >= knots[i + 1] {
            i += 1;
        }
        return i;
    }
    let (mut lo, mut hi) = (degree, n + 1);
    let mut mid = (lo + hi) / 2;
    while u < knots[mid] || u >= knots[mid + 1] {
        if u < knots[mid] {
            hi = mid;
        } else {
            lo = mid;
        }
        mid = (lo + hi) / 2;
    }
    mid
}

/// The highest degree a B-spline of the kernel's may have (OpenCASCADE's is 25).
pub(crate) const MAX_DEGREE: usize = 31;

/// Room for the basis functions of the highest degree.
const W: usize = MAX_DEGREE + 1;

/// The basis functions not zero at `u` in `span`, and their derivatives up to `order` (at
/// most 2): `out[k][j]` is the `k`-th derivative of `N_{span − degree + j}` (the NURBS
/// Book's A2.3), on the stack, its table sized to the degree.
pub(crate) fn basis_derivatives(
    knots: &[f64],
    span: usize,
    u: f64,
    degree: usize,
    order: usize,
) -> [[f64; W]; 3] {
    if degree < 8 {
        basis_derivatives_in::<8>(knots, span, u, degree, order)
    } else if degree < 16 {
        basis_derivatives_in::<16>(knots, span, u, degree, order)
    } else {
        basis_derivatives_in::<W>(knots, span, u, degree, order)
    }
}

fn basis_derivatives_in<const N: usize>(
    knots: &[f64],
    span: usize,
    u: f64,
    degree: usize,
    order: usize,
) -> [[f64; W]; 3] {
    let p = degree;
    let mut ndu = [[0.0f64; N]; N];
    let mut left = [0.0f64; N];
    let mut right = [0.0f64; N];
    ndu[0][0] = 1.0;
    for j in 1..=p {
        left[j] = u - knots[span + 1 - j];
        right[j] = knots[span + j] - u;
        let mut saved = 0.0;
        for r in 0..j {
            ndu[j][r] = right[r + 1] + left[j - r];
            let temp = ndu[r][j - 1] / ndu[j][r];
            ndu[r][j] = saved + right[r + 1] * temp;
            saved = left[j - r] * temp;
        }
        ndu[j][j] = saved;
    }
    let mut out = [[0.0f64; W]; 3];
    for j in 0..=p {
        out[0][j] = ndu[j][p];
    }
    let top = order.min(p).min(2);
    if top == 0 {
        return out;
    }
    let mut a = [[0.0f64; N]; 2];
    for r in 0..=p {
        let (mut s1, mut s2) = (0usize, 1usize);
        a[0][0] = 1.0;
        for k in 1..=top {
            let mut d = 0.0;
            let rk = r as isize - k as isize;
            let pk = p - k;
            if rk >= 0 {
                a[s2][0] = a[s1][0] / ndu[pk + 1][rk as usize];
                d = a[s2][0] * ndu[rk as usize][pk];
            }
            let j1 = if rk >= -1 { 1 } else { (-rk) as usize };
            let j2 = if r as isize - 1 <= pk as isize {
                k - 1
            } else {
                p - r
            };
            for j in j1..=j2 {
                let row = (rk + j as isize) as usize;
                a[s2][j] = (a[s1][j] - a[s1][j - 1]) / ndu[pk + 1][row];
                d += a[s2][j] * ndu[row][pk];
            }
            if r <= pk {
                a[s2][k] = -a[s1][k - 1] / ndu[pk + 1][r];
                d += a[s2][k] * ndu[r][pk];
            }
            out[k][r] = d;
            std::mem::swap(&mut s1, &mut s2);
        }
    }
    let mut factor = p as f64;
    for (k, row) in out.iter_mut().enumerate().take(top + 1).skip(1) {
        for value in row.iter_mut().take(p + 1) {
            *value *= factor;
        }
        factor *= (p - k) as f64;
    }
    out
}

// ---------------------------------------------------------------------------------------
// Curves.

/// A B-spline curve: its degree, its knots (as many as its poles and the degree and one),
/// its poles and, for a rational one, their weights.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BSplineCurve {
    pub degree: usize,
    pub knots: Vec<f64>,
    pub poles: Vec<Point3>,
    pub weights: Option<Vec<f64>>,
}

impl BSplineCurve {
    /// A curve, checked: at least `degree + 1` poles, knots that never decrease and number
    /// one more than the poles and the degree, positive weights, one for each pole.
    pub fn new(
        degree: usize,
        knots: Vec<f64>,
        poles: Vec<Point3>,
        weights: Option<Vec<f64>>,
    ) -> Result<BSplineCurve, String> {
        if degree == 0 || degree > MAX_DEGREE || poles.len() < degree + 1 {
            return Err(format!(
                "a B-spline curve needs a degree from 1 to {MAX_DEGREE} and more poles than it"
            ));
        }
        if knots.len() != poles.len() + degree + 1 || knots.windows(2).any(|w| w[1] < w[0]) {
            return Err("a B-spline curve's knots do not fit its poles".to_owned());
        }
        if knots[degree] >= knots[poles.len()] {
            return Err("a B-spline curve with no domain".to_owned());
        }
        if let Some(w) = &weights
            && (w.len() != poles.len() || w.iter().any(|&w| w.is_nan() || w <= 0.0))
        {
            return Err("a B-spline curve's weights do not fit its poles".to_owned());
        }
        if poles
            .iter()
            .any(|p| !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite()))
        {
            return Err("a B-spline curve with a pole that is not a point".to_owned());
        }
        Ok(BSplineCurve {
            degree,
            knots,
            poles,
            weights,
        })
    }

    /// The polynomial curve of one span over `[0, 1]` with these poles (a Bézier curve).
    pub fn bezier(poles: Vec<Point3>) -> Result<BSplineCurve, String> {
        let degree = poles.len().saturating_sub(1);
        let knots = std::iter::repeat_n(0.0, degree + 1)
            .chain(std::iter::repeat_n(1.0, degree + 1))
            .collect();
        BSplineCurve::new(degree, knots, poles, None)
    }

    pub fn domain(&self) -> (f64, f64) {
        (self.knots[self.degree], self.knots[self.poles.len()])
    }

    fn weight(&self, i: usize) -> f64 {
        self.weights.as_ref().map_or(1.0, |w| w[i])
    }

    pub fn homogeneous_poles(&self) -> Vec<H> {
        (0..self.poles.len())
            .map(|i| homogeneous(self.poles[i], self.weight(i)))
            .collect()
    }

    fn from_homogeneous(degree: usize, knots: Vec<f64>, poles: &[H], rational: bool) -> Self {
        BSplineCurve {
            degree,
            knots,
            poles: poles.iter().map(|&h| euclidean(h)).collect(),
            weights: rational.then(|| poles.iter().map(|h| h[3]).collect()),
        }
    }

    /// The point at `u` and its derivatives up to `order`: `out[0]` is the point (as the
    /// vector from the origin), `out[k]` the `k`-th derivative.
    pub fn derivatives(&self, u: f64, order: usize) -> Vec<Vec3> {
        let p = self.degree;
        let span = find_span(&self.knots, p, self.poles.len(), u);
        let ders = basis_derivatives(&self.knots, span, u, p, order);
        let mut aw = vec![[0.0; 4]; order + 1];
        for (k, row) in ders.iter().enumerate().take(order.min(2) + 1) {
            for (j, &n) in row.iter().enumerate().take(p + 1) {
                let i = span - p + j;
                let h = homogeneous(self.poles[i], self.weight(i));
                for c in 0..4 {
                    aw[k][c] += n * h[c];
                }
            }
        }
        let mut out: Vec<Vec3> = Vec::with_capacity(order + 1);
        for k in 0..=order {
            let mut v = Vec3::new(aw[k][0], aw[k][1], aw[k][2]);
            for i in 1..=k {
                v = v - out[k - i] * (binomial(k, i) * aw[i][3]);
            }
            out.push(v * (1.0 / aw[0][3]));
        }
        out
    }

    pub fn point(&self, u: f64) -> Point3 {
        let v = self.derivatives(u, 0)[0];
        Point3::new(v.x, v.y, v.z)
    }

    /// The same curve with `u` inserted as a knot `times` more times (as many as keep its
    /// multiplicity at most the degree), by the NURBS Book's A5.1.
    pub fn with_knot(&self, u: f64, times: usize) -> BSplineCurve {
        let p = self.degree;
        let count = self.poles.len();
        let s = self.knots.iter().filter(|&&k| k == u).count();
        let r = times.min(p.saturating_sub(s));
        if r == 0 {
            return self.clone();
        }
        let k = find_span(&self.knots, p, count, u);
        let pw = self.homogeneous_poles();
        let mut knots = Vec::with_capacity(self.knots.len() + r);
        knots.extend_from_slice(&self.knots[..=k]);
        knots.extend(std::iter::repeat_n(u, r));
        knots.extend_from_slice(&self.knots[k + 1..]);
        let mut q = vec![[0.0; 4]; count + r];
        q[..=k - p].copy_from_slice(&pw[..=k - p]);
        q[(k - s + r)..(count + r)].copy_from_slice(&pw[(k - s)..count]);
        let mut rw: Vec<H> = (0..=p - s).map(|i| pw[k - p + i]).collect();
        let mut l = k - p;
        for j in 1..=r {
            l = k - p + j;
            for i in 0..=(p - j - s) {
                let alpha = (u - self.knots[l + i]) / (self.knots[i + k + 1] - self.knots[l + i]);
                rw[i] = lerp(rw[i], rw[i + 1], alpha);
            }
            q[l] = rw[0];
            q[k + r - j - s] = rw[p - j - s];
        }
        if l + 1 < k - s {
            q[(l + 1)..(k - s)].copy_from_slice(&rw[1..(k - s - l)]);
        }
        BSplineCurve::from_homogeneous(p, knots, &q, self.weights.is_some())
    }

    /// The parameters where a knot starts a span that is not empty, the domain's ends
    /// included.
    pub fn breaks(&self) -> Vec<f64> {
        let (a, b) = self.domain();
        let mut out = vec![a];
        for &k in &self.knots {
            if k > a && k < b && out.last().is_none_or(|&l| k > l) {
                out.push(k);
            }
        }
        out.push(b);
        out
    }

    /// Whether it ends where it starts.
    pub fn is_closed(&self) -> bool {
        let (a, b) = self.domain();
        let size = self.size();
        self.point(a).distance(self.point(b)) <= 1e-9 * size.max(1.0)
    }

    /// The largest coordinate of its poles, in absolute value.
    fn size(&self) -> f64 {
        self.poles
            .iter()
            .map(|p| p.x.abs().max(p.y.abs()).max(p.z.abs()))
            .fold(0.0, f64::max)
    }

    /// Its length from `u0` to `u1`, by Gauss–Legendre quadrature of its speed over each
    /// span, in quarters.
    pub fn length(&self, u0: f64, u1: f64) -> f64 {
        let (lo, hi) = if u0 <= u1 { (u0, u1) } else { (u1, u0) };
        let mut total = 0.0;
        let breaks = self.breaks();
        for w in breaks.windows(2) {
            let (a, b) = (w[0].max(lo), w[1].min(hi));
            if b > a {
                total += crate::revolution::quadrature(a, b, (w[1] - w[0]) / 4.0, &|u| {
                    self.derivatives(u, 1)[1].length()
                });
            }
        }
        total
    }

    /// The parameter of the curve's point nearest `p`, from the nearest of points spread
    /// along each span, by Newton's method on `(C(u) − p)·C′(u) = 0`.
    pub fn nearest(&self, p: Point3, samples: &[(f64, Point3)], closed: bool) -> f64 {
        let (a, b) = self.domain();
        let mut seeds: Vec<(f64, f64)> = samples.iter().map(|&(u, q)| (q.distance(p), u)).collect();
        seeds.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut best = (f64::INFINITY, a);
        for &(_, seed) in seeds.iter().take(3) {
            let mut u = seed;
            for _ in 0..40 {
                let d = self.derivatives(u, 2);
                let r = d[0] - p.to_vec();
                let f = r.dot(d[1]);
                let df = d[1].dot(d[1]) + r.dot(d[2]);
                if df.abs() < 1e-300 {
                    break;
                }
                let mut next = u - f / df;
                if closed {
                    let l = b - a;
                    next = a + (next - a).rem_euclid(l);
                } else {
                    next = next.clamp(a, b);
                }
                let step = (next - u).abs();
                u = next;
                if step <= 1e-15 * (b - a).max(1.0) {
                    break;
                }
            }
            let dist = self.point(u).distance(p);
            if dist < best.0 {
                best = (dist, u);
            }
        }
        best.1
    }

    /// Points along it to start searches from: the degree's worth and a few more in each span.
    pub fn sample(&self) -> Vec<(f64, Point3)> {
        let per = 2 * (self.degree + 1);
        let breaks = self.breaks();
        let mut out = Vec::new();
        for w in breaks.windows(2) {
            for i in 0..per {
                let u = w[0] + (w[1] - w[0]) * i as f64 / per as f64;
                out.push((u, self.point(u)));
            }
        }
        if let Some(&b) = breaks.last() {
            out.push((b, self.point(b)));
        }
        out
    }
}

// ---------------------------------------------------------------------------------------
// Surfaces.

/// A B-spline surface: its degree and knots along `u` and along `v`, its poles (`count[0]`
/// rows along `u` of `count[1]` along `v`: pole `(i, j)` at `i * count[1] + j`) and, for a
/// rational one, their weights.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BSplineSurface {
    pub degree: [usize; 2],
    pub knots: [Vec<f64>; 2],
    pub count: [usize; 2],
    pub poles: Vec<Point3>,
    pub weights: Option<Vec<f64>>,
}

/// A surface's point at `(u, v)` and its derivatives: `d[k][l]` is `∂^(k+l) S / ∂u^k ∂v^l`
/// (`d[0][0]` the point as a vector), for `k + l` up to 2.
pub(crate) type SurfaceDerivatives = [[Vec3; 3]; 3];

impl BSplineSurface {
    pub fn new(
        degree: [usize; 2],
        knots: [Vec<f64>; 2],
        count: [usize; 2],
        poles: Vec<Point3>,
        weights: Option<Vec<f64>>,
    ) -> Result<BSplineSurface, String> {
        for d in 0..2 {
            if degree[d] == 0 || degree[d] > MAX_DEGREE || count[d] < degree[d] + 1 {
                return Err(format!(
                    "a B-spline surface needs degrees from 1 to {MAX_DEGREE} and more poles than them"
                ));
            }
            let k = &knots[d];
            if k.len() != count[d] + degree[d] + 1 || k.windows(2).any(|w| w[1] < w[0]) {
                return Err("a B-spline surface's knots do not fit its poles".to_owned());
            }
            if k[degree[d]] >= k[count[d]] {
                return Err("a B-spline surface with no domain".to_owned());
            }
        }
        if poles.len() != count[0] * count[1] {
            return Err("a B-spline surface's poles do not fit its counts".to_owned());
        }
        if let Some(w) = &weights
            && (w.len() != poles.len() || w.iter().any(|&w| w.is_nan() || w <= 0.0))
        {
            return Err("a B-spline surface's weights do not fit its poles".to_owned());
        }
        if poles
            .iter()
            .any(|p| !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite()))
        {
            return Err("a B-spline surface with a pole that is not a point".to_owned());
        }
        Ok(BSplineSurface {
            degree,
            knots,
            count,
            poles,
            weights,
        })
    }

    /// The domain along `u` (`d = 0`) or `v` (`d = 1`).
    pub fn domain(&self, d: usize) -> (f64, f64) {
        (self.knots[d][self.degree[d]], self.knots[d][self.count[d]])
    }

    fn weight(&self, k: usize) -> f64 {
        self.weights.as_ref().map_or(1.0, |w| w[k])
    }

    fn pole_h(&self, i: usize, j: usize) -> H {
        let k = i * self.count[1] + j;
        homogeneous(self.poles[k], self.weight(k))
    }

    /// The point at `(u, v)` and its derivatives up to `order` (at most 2) all together.
    pub fn derivatives(&self, u: f64, v: f64, order: usize) -> SurfaceDerivatives {
        let order = order.min(2);
        let [pu, pv] = self.degree;
        let su = find_span(&self.knots[0], pu, self.count[0], u);
        let sv = find_span(&self.knots[1], pv, self.count[1], v);
        let nu = basis_derivatives(&self.knots[0], su, u, pu, order);
        let nv = basis_derivatives(&self.knots[1], sv, v, pv, order);
        // Along v first, for each row of the active poles: r[i][l] = Σ_j N_v^(l)(j) P_ij.
        let mut r = [[[0.0f64; 4]; 3]; W];
        for (i, row) in r.iter_mut().enumerate().take(pu + 1) {
            for j in 0..=pv {
                let h = self.pole_h(su - pu + i, sv - pv + j);
                for (acc, n) in row.iter_mut().zip(&nv).take(order + 1) {
                    let b = n[j];
                    for c in 0..4 {
                        acc[c] += b * h[c];
                    }
                }
            }
        }
        // Then along u.
        let mut a = [[[0.0f64; 4]; 3]; 3];
        for k in 0..=order {
            for l in 0..=(order - k) {
                let mut acc = [0.0; 4];
                for (i, row) in r.iter().enumerate().take(pu + 1) {
                    let b = nu[k][i];
                    for c in 0..4 {
                        acc[c] += b * row[l][c];
                    }
                }
                a[k][l] = acc;
            }
        }
        let mut s = [[Vec3::ZERO; 3]; 3];
        let w = |k: usize, l: usize| a[k][l][3];
        for k in 0..=order {
            for l in 0..=(order - k) {
                let mut v = Vec3::new(a[k][l][0], a[k][l][1], a[k][l][2]);
                for j in 1..=l {
                    v = v - s[k][l - j] * (binomial(l, j) * w(0, j));
                }
                for i in 1..=k {
                    v = v - s[k - i][l] * (binomial(k, i) * w(i, 0));
                    let mut v2 = Vec3::ZERO;
                    for j in 1..=l {
                        v2 = v2 + s[k - i][l - j] * (binomial(l, j) * w(i, j));
                    }
                    v = v - v2 * binomial(k, i);
                }
                s[k][l] = v * (1.0 / w(0, 0));
            }
        }
        s
    }

    pub fn point(&self, u: f64, v: f64) -> Point3 {
        let d = self.derivatives(u, v, 0)[0][0];
        Point3::new(d.x, d.y, d.z)
    }

    /// The curve of constant `v` (`d = 0`, running along `u`) or of constant `u` (`d = 1`,
    /// along `v`) through `at`.
    pub fn iso(&self, d: usize, at: f64) -> BSplineCurve {
        let other = 1 - d;
        let p = self.degree[other];
        let span = find_span(&self.knots[other], p, self.count[other], at);
        let n = basis_derivatives(&self.knots[other], span, at, p, 0);
        let count = self.count[d];
        let poles: Vec<H> = (0..count)
            .map(|i| {
                let mut acc = [0.0; 4];
                for (j, &b) in n[0].iter().enumerate().take(p + 1) {
                    let h = if d == 0 {
                        self.pole_h(i, span - p + j)
                    } else {
                        self.pole_h(span - p + j, i)
                    };
                    for c in 0..4 {
                        acc[c] += b * h[c];
                    }
                }
                acc
            })
            .collect();
        BSplineCurve::from_homogeneous(
            self.degree[d],
            self.knots[d].clone(),
            &poles,
            self.weights.is_some(),
        )
    }

    pub fn breaks(&self, d: usize) -> Vec<f64> {
        let (a, b) = self.domain(d);
        let mut out = vec![a];
        for &k in &self.knots[d] {
            if k > a && k < b && out.last().is_none_or(|&l| k > l) {
                out.push(k);
            }
        }
        out.push(b);
        out
    }

    /// The largest coordinate of its poles, in absolute value.
    pub fn size(&self) -> f64 {
        self.poles
            .iter()
            .map(|p| p.x.abs().max(p.y.abs()).max(p.z.abs()))
            .fold(0.0, f64::max)
    }

    /// Whether its two edges across `d` (`u` at its two ends for `d = 0`) are one curve,
    /// so that the surface closes round on itself that way.
    pub fn closes(&self, d: usize) -> bool {
        let (a, b) = self.domain(d);
        let (lo, hi) = self.domain(1 - d);
        let tol = 1e-9 * self.size().max(1.0);
        (0..=16).all(|i| {
            let t = lo + (hi - lo) * f64::from(i) / 16.0;
            let (p, q) = if d == 0 {
                (self.point(a, t), self.point(b, t))
            } else {
                (self.point(t, a), self.point(t, b))
            };
            p.distance(q) <= tol
        })
    }

    /// The surface cut into its polynomial (or rational) pieces of one span each way.
    fn patches(&self) -> Vec<Patch> {
        let [pu, pv] = self.degree;
        let mut out = Vec::new();
        let bu = self.breaks(0);
        let bv = self.breaks(1);
        for wu in bu.windows(2) {
            for wv in bv.windows(2) {
                let (u0, u1, v0, v1) = (wu[0], wu[1], wv[0], wv[1]);
                // Each pole of the piece from the surface's iso-curves cut down to the span:
                // rows along v at the Bézier points of u.
                let mut poles = vec![[0.0; 4]; (pu + 1) * (pv + 1)];
                let rows = self.bezier_rows(u0, u1, 0);
                for (i, row) in rows.iter().enumerate() {
                    let curve =
                        BSplineCurve::from_homogeneous(pv, self.knots[1].clone(), row, true);
                    let piece = bezier_of(&curve, v0, v1);
                    for (j, h) in piece.into_iter().enumerate() {
                        poles[i * (pv + 1) + j] = h;
                    }
                }
                out.push(Patch {
                    u: (u0, u1),
                    v: (v0, v1),
                    degree: [pu, pv],
                    poles,
                });
            }
        }
        out
    }

    /// For the span `[t0, t1]` along `d`, the homogeneous poles of each of the `degree + 1`
    /// curves that make the surface's piece there: the surface's poles combined along `d`
    /// as the span's Bézier form of that direction.
    fn bezier_rows(&self, t0: f64, t1: f64, d: usize) -> Vec<Vec<H>> {
        let other = 1 - d;
        let cols = self.count[other];
        let mut rows = vec![Vec::with_capacity(cols); self.degree[d] + 1];
        for j in 0..cols {
            let line: Vec<H> = (0..self.count[d])
                .map(|i| {
                    if d == 0 {
                        self.pole_h(i, j)
                    } else {
                        self.pole_h(j, i)
                    }
                })
                .collect();
            let curve =
                BSplineCurve::from_homogeneous(self.degree[d], self.knots[d].clone(), &line, true);
            for (i, h) in bezier_of(&curve, t0, t1).into_iter().enumerate() {
                rows[i].push(h);
            }
        }
        rows
    }
}

/// The homogeneous Bézier poles of a curve's piece from `t0` to `t1`, ends of one of its
/// spans: the knots there raised to the degree, the poles of the span between them.
fn bezier_of(curve: &BSplineCurve, t0: f64, t1: f64) -> Vec<H> {
    let p = curve.degree;
    let c = curve.with_knot(t0, p).with_knot(t1, p);
    let k = find_span(&c.knots, p, c.poles.len(), 0.5 * (t0 + t1));
    let pw = c.homogeneous_poles();
    (0..=p).map(|j| pw[k - p + j]).collect()
}

/// A polynomial (or rational) piece of a surface over `u` × `v`, by its Bézier poles.
#[derive(Debug, Clone)]
struct Patch {
    u: (f64, f64),
    v: (f64, f64),
    degree: [usize; 2],
    poles: Vec<H>,
}

impl Patch {
    fn bounds(&self) -> (Point3, Point3) {
        let mut lo = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
        let mut hi = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &h in &self.poles {
            let p = euclidean(h);
            lo = Point3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
            hi = Point3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
        }
        (lo, hi)
    }

    /// The two halves of the patch along `d`, by de Casteljau's construction.
    fn halves(&self, d: usize) -> (Patch, Patch) {
        let [pu, pv] = self.degree;
        let (n, m) = (pu + 1, pv + 1);
        let mut a = self.poles.clone();
        let mut b = self.poles.clone();
        let lines = if d == 0 { m } else { n };
        let len = if d == 0 { n } else { m };
        for l in 0..lines {
            let idx = |k: usize| if d == 0 { k * m + l } else { l * m + k };
            let mut work: Vec<H> = (0..len).map(|k| self.poles[idx(k)]).collect();
            let mut left = vec![[0.0; 4]; len];
            let mut right = vec![[0.0; 4]; len];
            for r in 0..len {
                left[r] = work[0];
                right[len - 1 - r] = work[len - 1 - r];
                for k in 0..(len - 1 - r) {
                    work[k] = lerp(work[k], work[k + 1], 0.5);
                }
            }
            for k in 0..len {
                a[idx(k)] = left[k];
                b[idx(k)] = right[k];
            }
        }
        let (mut first, mut second) = (self.clone(), self.clone());
        first.poles = a;
        second.poles = b;
        if d == 0 {
            let mid = (self.u.0 + self.u.1) / 2.0;
            first.u = (self.u.0, mid);
            second.u = (mid, self.u.1);
        } else {
            let mid = (self.v.0 + self.v.1) / 2.0;
            first.v = (self.v.0, mid);
            second.v = (mid, self.v.1);
        }
        (first, second)
    }
}

/// Where the line `o + t d` enters and leaves the box, if it meets it.
fn line_box(o: Point3, d: Vec3, lo: Point3, hi: Point3, pad: f64) -> bool {
    let mut t0 = f64::NEG_INFINITY;
    let mut t1 = f64::INFINITY;
    for (oc, dc, l, h) in [
        (o.x, d.x, lo.x - pad, hi.x + pad),
        (o.y, d.y, lo.y - pad, hi.y + pad),
        (o.z, d.z, lo.z - pad, hi.z + pad),
    ] {
        if dc.abs() < 1e-300 {
            if oc < l || oc > h {
                return false;
            }
        } else {
            let (a, b) = ((l - oc) / dc, (h - oc) / dc);
            let (a, b) = if a <= b { (a, b) } else { (b, a) };
            t0 = t0.max(a);
            t1 = t1.min(b);
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------------------
// What the kernel holds: each B-spline once, placed.

/// A B-spline curve kept for the life of the process, with what searches on it start from.
#[derive(Debug)]
pub(crate) struct CurveData {
    pub curve: BSplineCurve,
    pub closed: bool,
    samples: OnceLock<Vec<(f64, Point3)>>,
}

impl CurveData {
    pub fn samples(&self) -> &[(f64, Point3)] {
        self.samples.get_or_init(|| self.curve.sample())
    }
}

/// A B-spline surface kept for the life of the process, with what searches on it start
/// from: a grid of its points and its pieces.
#[derive(Debug)]
pub(crate) struct SurfaceData {
    pub surface: BSplineSurface,
    pub hash: u64,
    /// Whether it closes round on itself along `u`, along `v`.
    pub closed: [bool; 2],
    grid: OnceLock<Vec<(f64, f64, Point3)>>,
    patches: OnceLock<Vec<Patch>>,
    scale: OnceLock<f64>,
}

fn hash_of(degree: &[usize], knots: &[&[f64]], poles: &[Point3], weights: Option<&[f64]>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    degree.hash(&mut h);
    for k in knots {
        k.len().hash(&mut h);
        for x in k.iter() {
            x.to_bits().hash(&mut h);
        }
    }
    for p in poles {
        [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()].hash(&mut h);
    }
    if let Some(w) = weights {
        for x in w {
            x.to_bits().hash(&mut h);
        }
    }
    h.finish()
}

static CURVES: LazyLock<Mutex<HashMap<u64, Vec<&'static CurveData>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SURFACES: LazyLock<Mutex<HashMap<u64, Vec<&'static SurfaceData>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The curve as the process keeps it: the one kept already when it was made before.
pub(crate) fn intern_curve(curve: BSplineCurve) -> &'static CurveData {
    let hash = hash_of(
        &[curve.degree],
        &[&curve.knots],
        &curve.poles,
        curve.weights.as_deref(),
    );
    let mut map = CURVES.lock().unwrap_or_else(PoisonError::into_inner);
    let bucket = map.entry(hash).or_default();
    if let Some(&found) = bucket.iter().find(|d| d.curve == curve) {
        return found;
    }
    let closed = curve.is_closed();
    let data: &'static CurveData = Box::leak(Box::new(CurveData {
        curve,
        closed,
        samples: OnceLock::new(),
    }));
    bucket.push(data);
    data
}

/// The surface as the process keeps it: the one kept already when it was made before.
pub(crate) fn intern_surface(surface: BSplineSurface) -> &'static SurfaceData {
    let hash = hash_of(
        &surface.degree,
        &[&surface.knots[0], &surface.knots[1]],
        &surface.poles,
        surface.weights.as_deref(),
    );
    let mut map = SURFACES.lock().unwrap_or_else(PoisonError::into_inner);
    let bucket = map.entry(hash).or_default();
    if let Some(&found) = bucket.iter().find(|d| d.surface == surface) {
        return found;
    }
    let closed = [surface.closes(0), surface.closes(1)];
    let data: &'static SurfaceData = Box::leak(Box::new(SurfaceData {
        surface,
        hash,
        closed,
        grid: OnceLock::new(),
        patches: OnceLock::new(),
        scale: OnceLock::new(),
    }));
    bucket.push(data);
    data
}

impl SurfaceData {
    /// Points spread over each span both ways, to start searches from.
    fn grid(&self) -> &[(f64, f64, Point3)] {
        self.grid.get_or_init(|| {
            let s = &self.surface;
            let mut params = [Vec::new(), Vec::new()];
            for (d, list) in params.iter_mut().enumerate() {
                let per = (s.degree[d] + 2).max(4);
                let breaks = s.breaks(d);
                for w in breaks.windows(2) {
                    for i in 0..per {
                        list.push(w[0] + (w[1] - w[0]) * i as f64 / per as f64);
                    }
                }
                if let Some(&b) = breaks.last() {
                    list.push(b);
                }
            }
            let mut out = Vec::with_capacity(params[0].len() * params[1].len());
            for &u in &params[0] {
                for &v in &params[1] {
                    out.push((u, v, s.point(u, v)));
                }
            }
            out
        })
    }

    fn patches(&self) -> &[Patch] {
        self.patches.get_or_init(|| self.surface.patches())
    }

    /// The least of its iso-curves' lengths through the middle of its domain, over 2π: a
    /// loft of circles' radius, about.
    pub fn scale(&self) -> f64 {
        *self.scale.get_or_init(|| {
            let s = &self.surface;
            let length = |d: usize| {
                let (c0, c1) = s.domain(1 - d);
                let iso = s.iso(d, 0.5 * (c0 + c1));
                let (a, b) = iso.domain();
                iso.length(a, b)
            };
            (length(0).min(length(1)) / std::f64::consts::TAU).max(1e-3)
        })
    }

    /// The parameters of the surface's point nearest `p` (in the surface's own frame): from
    /// `hint` and the nearest points of its grid, by Newton's method on the gradient of the
    /// squared distance, round the domain where the surface closes, and a tenth of it past
    /// an open edge, where the surface's last span runs on: so that the distance changes
    /// sign smoothly across the surface near its edges too.
    pub fn nearest(&self, p: Point3, hint: Option<(f64, f64)>) -> (f64, f64) {
        let s = &self.surface;
        let (ua, ub) = s.domain(0);
        let (va, vb) = s.domain(1);
        // The nearest points of the grid, in one pass: the three nearest, and those of the
        // next five nearly as near (on a narrow band the grid's points across it are all
        // about as near, and only those on the right side lead to the nearest point).
        let mut seeds = [(f64::INFINITY, (ua, va)); 8];
        for &(u, v, q) in self.grid() {
            let d = q.distance(p);
            if d < seeds[7].0 {
                seeds[7] = (d, (u, v));
                for k in (1..8).rev() {
                    if seeds[k].0 < seeds[k - 1].0 {
                        seeds.swap(k, k - 1);
                    } else {
                        break;
                    }
                }
            }
        }
        let nearest_sample = seeds[0].0;
        let mut starts: Vec<(f64, f64)> = hint.into_iter().collect();
        starts.extend(
            seeds
                .iter()
                .enumerate()
                .filter(|(k, s)| s.0.is_finite() && (*k < 3 || s.0 <= 1.5 * nearest_sample))
                .map(|(_, s)| s.1),
        );
        // Within the domain (or round it where the surface closes), not on the run past an
        // open edge, where a surface that bends sharply there can come back near itself.
        let inside = |(u, v): (f64, f64)| {
            let (tu, tv) = (1e-9 * (ub - ua), 1e-9 * (vb - va));
            (self.closed[0] || (u >= ua - tu && u <= ub + tu))
                && (self.closed[1] || (v >= va - tv && v <= vb + tv))
        };
        let mut best = (f64::INFINITY, (ua, va));
        let fit = |x: f64, a: f64, b: f64, closed: bool| {
            if closed {
                a + (x - a).rem_euclid(b - a)
            } else {
                let m = 0.1 * (b - a);
                x.clamp(a - m, b + m)
            }
        };
        let range = ub - ua + vb - va;
        for (u0, v0) in starts {
            let (mut u, mut v) = (u0, v0);
            let mut converged = false;
            for _ in 0..40 {
                let d = s.derivatives(u, v, 2);
                let r = d[0][0] - p.to_vec();
                let (su, sv) = (d[1][0], d[0][1]);
                let f = [r.dot(su), r.dot(sv)];
                let j00 = su.dot(su) + r.dot(d[2][0]);
                let j01 = su.dot(sv) + r.dot(d[1][1]);
                let j11 = sv.dot(sv) + r.dot(d[0][2]);
                let det = j00 * j11 - j01 * j01;
                let (du, dv) = if det > 1e-300 && j00 > 0.0 {
                    (
                        (j11 * f[0] - j01 * f[1]) / det,
                        (j00 * f[1] - j01 * f[0]) / det,
                    )
                } else {
                    // Not a minimum's basin: a gradient step instead.
                    let g = su.dot(su).max(sv.dot(sv)).max(1e-300);
                    (f[0] / g, f[1] / g)
                };
                // A long step halved while it leads further off: across a surface bent
                // sharply, or stretched far more one way than the other, Newton's step
                // overshoots.
                let here = r.length();
                let mut t = 1.0;
                let (mut nu, mut nv, mut step);
                loop {
                    nu = fit(u - t * du, ua, ub, self.closed[0]);
                    nv = fit(v - t * dv, va, vb, self.closed[1]);
                    step = (nu - u).abs() + (nv - v).abs();
                    if step <= 1e-9 * range || t < 1e-6 {
                        break;
                    }
                    if s.point(nu, nv).distance(p) <= here * (1.0 + 1e-12) {
                        break;
                    }
                    t *= 0.5;
                }
                u = nu;
                v = nv;
                // Quadratic convergence: a step this small leaves the last one's rounding.
                if step <= 1e-12 * range {
                    converged = true;
                    break;
                }
            }
            let dist = s.point(u, v).distance(p);
            if dist < best.0 {
                best = (dist, (u, v));
            }
            // From the hint, there and as near as the nearest of the grid's points or
            // nearer, on the surface itself: the nearest point, without searching from the
            // grid.
            if hint.is_some() && converged && best.0 <= nearest_sample && inside(best.1) {
                break;
            }
        }
        best.1
    }

    /// Where the line `o + t d` (in the surface's frame) crosses the surface: each `t` and
    /// the parameters there, found on the surface's pieces halved while their poles' boxes
    /// meet the line, then by Newton's method.
    pub fn line_hits(&self, o: Point3, d: Vec3) -> Vec<(f64, f64, f64)> {
        let s = &self.surface;
        let scale = s.size().max(1.0);
        let pad = 1e-9 * scale;
        let mut out: Vec<(f64, f64, f64)> = Vec::new();
        let mut stack: Vec<(Patch, u32)> = self.patches().iter().map(|p| (p.clone(), 0)).collect();
        while let Some((patch, depth)) = stack.pop() {
            let (lo, hi) = patch.bounds();
            if !line_box(o, d, lo, hi, pad) {
                continue;
            }
            let size = lo.distance(hi);
            if depth < 14 && size > 1e-3 * scale {
                let dir = if depth % 2 == 0 { 0 } else { 1 };
                let (a, b) = patch.halves(dir);
                stack.push((a, depth + 1));
                stack.push((b, depth + 1));
                continue;
            }
            // Newton's method on S(u, v) − (o + t d) from the piece's middle.
            let (mut u, mut v) = ((patch.u.0 + patch.u.1) / 2.0, (patch.v.0 + patch.v.1) / 2.0);
            let q = s.point(u, v);
            let mut t = (q - o).dot(d) / d.dot(d);
            let mut converged = false;
            for _ in 0..30 {
                let der = s.derivatives(u, v, 1);
                let f = der[0][0] - (o.to_vec() + d * t);
                if f.length() <= 1e-12 * scale {
                    converged = true;
                    break;
                }
                let (su, sv) = (der[1][0], der[0][1]);
                let rows = [su, sv, d * -1.0];
                // Solve [su sv −d] x = −f by Cramer's rule (columns su, sv, −d).
                let det = rows[0].dot(rows[1].cross(rows[2]));
                if det.abs() < 1e-300 {
                    break;
                }
                let m = f * -1.0;
                let x0 = m.dot(rows[1].cross(rows[2])) / det;
                let x1 = rows[0].dot(m.cross(rows[2])) / det;
                let x2 = rows[0].dot(rows[1].cross(m)) / det;
                u += x0;
                v += x1;
                t += x2;
                // Leaving the piece far: not this piece's crossing.
                let (wu, wv) = (patch.u.1 - patch.u.0, patch.v.1 - patch.v.0);
                if u < patch.u.0 - wu
                    || u > patch.u.1 + wu
                    || v < patch.v.0 - wv
                    || v > patch.v.1 + wv
                {
                    break;
                }
            }
            if !converged {
                continue;
            }
            let (ua, ub) = s.domain(0);
            let (va, vb) = s.domain(1);
            let eps = 1e-9;
            if u < ua - eps || u > ub + eps || v < va - eps || v > vb + eps {
                continue;
            }
            let (u, v) = (u.clamp(ua, ub), v.clamp(va, vb));
            let near = |a: &(f64, f64, f64)| {
                ((a.0 - t).abs() * d.length() <= 1e-7 * scale)
                    || ((a.1 - u).abs() <= 1e-9 * (ub - ua) && (a.2 - v).abs() <= 1e-9 * (vb - va))
            };
            if !out.iter().any(near) {
                out.push((t, u, v));
            }
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out
    }
}

/// The inverse of a rigid motion or a mirror.
pub(crate) fn inverse(t: &Transform) -> Transform {
    let r = &t.rows;
    let mut rows = [[0.0; 4]; 3];
    for i in 0..3 {
        for j in 0..3 {
            rows[i][j] = r[j][i];
        }
        rows[i][3] = -(r[0][i] * r[0][3] + r[1][i] * r[1][3] + r[2][i] * r[2][3]);
    }
    Transform { rows }
}

/// A point looked up on a placed curve: the curve's data, the point's bits, the placement's.
type ParamKey = (usize, [u64; 3], [[u64; 4]; 3]);

thread_local! {
    /// The parameters of points looked up lately on B-spline curves ([`SplineCurve::param_of`]).
    static PARAMS: std::cell::RefCell<Vec<(ParamKey, f64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A B-spline curve where the kernel places it, and the way an edge on it runs: as its
/// parameter grows when `up`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SplineCurve {
    pub data: &'static CurveData,
    pub place: Transform,
    pub up: bool,
}

impl PartialEq for SplineCurve {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.data, other.data) && self.place == other.place && self.up == other.up
    }
}

impl SplineCurve {
    pub fn new(curve: BSplineCurve) -> SplineCurve {
        SplineCurve {
            data: intern_curve(curve),
            place: Transform::IDENTITY,
            up: true,
        }
    }

    pub fn domain(&self) -> (f64, f64) {
        self.data.curve.domain()
    }

    pub fn closed(&self) -> bool {
        self.data.closed
    }

    pub fn point_at(&self, u: f64) -> Point3 {
        self.place.apply_point(self.data.curve.point(u))
    }

    /// The point and its derivatives along the parameter (not the way an edge runs).
    pub fn derivatives(&self, u: f64, order: usize) -> Vec<Vec3> {
        let mut d = self.data.curve.derivatives(u, order);
        if let Some(first) = d.first_mut() {
            let p = self
                .place
                .apply_point(Point3::new(first.x, first.y, first.z));
            *first = p.to_vec();
        }
        for v in d.iter_mut().skip(1) {
            *v = self.place.apply_vector(*v);
        }
        d
    }

    /// The parameter of the point nearest `p`: remembered for the points looked up lately,
    /// as an edge is followed from its start many times over.
    pub fn param_of(&self, p: Point3) -> f64 {
        let key = (
            std::ptr::from_ref(self.data) as usize,
            [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()],
            self.place.rows.map(|r| r.map(f64::to_bits)),
        );
        if let Some(u) = PARAMS.with(|m| m.borrow().iter().find(|e| e.0 == key).map(|e| e.1)) {
            return u;
        }
        let local = inverse(&self.place).apply_point(p);
        let u = self
            .data
            .curve
            .nearest(local, self.data.samples(), self.data.closed);
        PARAMS.with(|m| {
            let mut m = m.borrow_mut();
            if m.len() >= 64 {
                m.remove(0);
            }
            m.push((key, u));
        });
        u
    }

    pub fn distance(&self, p: Point3) -> f64 {
        self.point_at(self.param_of(p)).distance(p)
    }

    pub fn transformed(&self, t: &Transform) -> SplineCurve {
        SplineCurve {
            place: self.place.then(t),
            ..*self
        }
    }

    pub fn reversed(&self) -> SplineCurve {
        SplineCurve {
            up: !self.up,
            ..*self
        }
    }

    fn period(&self) -> f64 {
        let (a, b) = self.domain();
        b - a
    }

    /// How far along an edge from `start` the point `p` is, in the curve's parameter, its
    /// way: in `[0, period)` round a closed curve.
    pub fn turned(&self, start: Point3, p: Point3) -> f64 {
        let (u0, u1) = (self.param_of(start), self.param_of(p));
        let d = if self.up { u1 - u0 } else { u0 - u1 };
        if self.closed() {
            d.rem_euclid(self.period())
        } else {
            d
        }
    }

    /// How far an edge from `start` to `end` runs in the curve's parameter: the whole
    /// domain for a closed one.
    pub fn sweep(&self, start: Point3, end: Point3, closed: bool) -> f64 {
        if closed {
            return self.period();
        }
        let t = self.turned(start, end);
        if self.closed() && t <= 1e-12 * self.period() {
            self.period()
        } else {
            t.abs()
        }
    }

    /// The parameter `s` along an edge from `start`, its way.
    pub fn param_along(&self, start: Point3, s: f64) -> f64 {
        let u0 = self.param_of(start);
        let u = if self.up { u0 + s } else { u0 - s };
        if self.closed() {
            let (a, _) = self.domain();
            a + (u - a).rem_euclid(self.period())
        } else {
            u
        }
    }

    pub fn along(&self, start: Point3, s: f64) -> Point3 {
        self.point_at(self.param_along(start, s))
    }

    /// Its length from `start` along `extent` of its parameter, its way.
    pub fn length_from(&self, start: Point3, extent: f64) -> f64 {
        let u0 = self.param_of(start);
        let (a, b) = self.domain();
        let c = &self.data.curve;
        if self.up {
            let end = u0 + extent;
            if end <= b + 1e-12 {
                c.length(u0, end.min(b))
            } else {
                c.length(u0, b) + c.length(a, a + (end - b))
            }
        } else {
            let end = u0 - extent;
            if end >= a - 1e-12 {
                c.length(end.max(a), u0)
            } else {
                c.length(a, u0) + c.length(b - (a - end), b)
            }
        }
    }

    /// Points from `start` along `extent` to `end`, both ends included, each chord within
    /// `deflection` of the curve: steps of at most a sixteenth of each span, halved until
    /// each middle is close enough to its chord.
    pub fn polyline(
        &self,
        start: Point3,
        end: Point3,
        extent: f64,
        deflection: f64,
    ) -> Vec<Point3> {
        let spans = self.data.curve.breaks().len().saturating_sub(1).max(1);
        let n = (16 * spans).max(2);
        let at = |s: f64| self.along(start, s);
        let mut out = vec![start];
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
            let (p0, p1, pm) = (at(s0), at(s1), at((s0 + s1) / 2.0));
            let chord = p0 + (p1 - p0) * 0.5;
            if pm.distance(chord) > deflection && s1 - s0 > 1e-9 * extent.max(1e-12) {
                stack.push(((s0 + s1) / 2.0, s1));
                stack.push((s0, (s0 + s1) / 2.0));
            } else if s1 < extent {
                out.push(p1);
            }
        }
        out.push(end);
        out
    }

    /// Along each model axis, the extremes an edge from `start` along `extent` reaches
    /// between its ends: among points spread along it, narrowed by Newton's method on the
    /// coordinate's derivative.
    pub fn extremes(&self, start: Point3, extent: f64) -> Vec<Point3> {
        let spans = self.data.curve.breaks().len().saturating_sub(1).max(1);
        let n = 8 * (self.data.curve.degree + 1) * spans;
        let samples: Vec<(f64, Point3)> = (0..=n)
            .map(|i| {
                let s = extent * i as f64 / n as f64;
                (s, self.along(start, s))
            })
            .collect();
        let mut out = Vec::new();
        for k in 0..3 {
            let coord = |p: Point3| crate::geometry::component(p.to_vec(), k);
            for w in samples.windows(3) {
                let (a, m, b) = (coord(w[0].1), coord(w[1].1), coord(w[2].1));
                if !((m > a && m >= b) || (m < a && m <= b)) {
                    continue;
                }
                let sign = if self.up { 1.0 } else { -1.0 };
                let mut s = w[1].0;
                for _ in 0..40 {
                    let u = self.param_along(start, s);
                    let d = self.derivatives(u, 2);
                    let (d1, d2) = (
                        crate::geometry::component(d[1], k) * sign,
                        crate::geometry::component(d[2], k),
                    );
                    if d2.abs() < 1e-300 {
                        break;
                    }
                    let next = (s - d1 / d2).clamp(w[0].0, w[2].0);
                    let step = (next - s).abs();
                    s = next;
                    if step <= 1e-15 * extent.max(1.0) {
                        break;
                    }
                }
                if s > 0.0 && s < extent {
                    out.push(self.along(start, s));
                }
            }
        }
        out
    }
}

thread_local! {
    /// Where the last search for a nearest point ended, and on which surface.
    static LAST: std::cell::Cell<Option<(usize, (f64, f64))>> = const { std::cell::Cell::new(None) };
}

/// Whether two B-spline curves through the same points run the same way: the same curve
/// the same way, or else their tangents at a point of the first pointing the same way.
pub(crate) fn same_way(s: &SplineCurve, t: &SplineCurve) -> bool {
    if std::ptr::eq(s.data, t.data) && s.place == t.place {
        return s.up == t.up;
    }
    let (a, b) = s.domain();
    let u = 0.5 * (a + b);
    let ds = s.derivatives(u, 1);
    let p = Point3::new(ds[0].x, ds[0].y, ds[0].z);
    let dt = t.derivatives(t.param_of(p), 1);
    let sign = |c: &SplineCurve| if c.up { 1.0 } else { -1.0 };
    ds[1].dot(dt[1]) * sign(s) * sign(t) > 0.0
}

/// A B-spline surface where the kernel places it, and which way its face's normal points:
/// along `S_u × S_v` (as placed) when `outward`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SplineSurface {
    pub data: &'static SurfaceData,
    pub place: Transform,
    pub outward: bool,
}

impl PartialEq for SplineSurface {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.data, other.data)
            && self.place == other.place
            && self.outward == other.outward
    }
}

impl SplineSurface {
    pub fn new(surface: BSplineSurface, outward: bool) -> SplineSurface {
        SplineSurface {
            data: intern_surface(surface),
            place: Transform::IDENTITY,
            outward,
        }
    }

    pub fn domain(&self, d: usize) -> (f64, f64) {
        self.data.surface.domain(d)
    }

    pub fn point(&self, u: f64, v: f64) -> Point3 {
        self.place.apply_point(self.data.surface.point(u, v))
    }

    /// The point and its derivatives up to `order` (at most 2), placed.
    pub fn derivatives(&self, u: f64, v: f64, order: usize) -> SurfaceDerivatives {
        let mut d = self.data.surface.derivatives(u, v, order);
        let p = d[0][0];
        d[0][0] = self.place.apply_point(Point3::new(p.x, p.y, p.z)).to_vec();
        for (k, row) in d.iter_mut().enumerate() {
            for (l, x) in row.iter_mut().enumerate() {
                if k + l > 0 {
                    *x = self.place.apply_vector(*x);
                }
            }
        }
        d
    }

    /// The face's unit normal at `(u, v)`, out of its solid; none where the surface is
    /// singular.
    pub fn normal(&self, u: f64, v: f64) -> Option<Vec3> {
        let d = self.derivatives(u, v, 1);
        let n = d[1][0].cross(d[0][1]).normalized()?;
        Some(if self.outward { n } else { -n })
    }

    /// The parameters of the point nearest `p`, the search started from `hint` too (or,
    /// without one, from where the last search on this surface ended: points are mostly
    /// looked up one beside another).
    pub fn params_near(&self, p: Point3, hint: Option<(f64, f64)>) -> (f64, f64) {
        let local = inverse(&self.place).apply_point(p);
        let id = std::ptr::from_ref(self.data) as usize;
        let hint = hint.or_else(|| LAST.with(|l| l.get().filter(|e| e.0 == id).map(|e| e.1)));
        let found = self.data.nearest(local, hint);
        LAST.with(|l| l.set(Some((id, found))));
        found
    }

    pub fn params(&self, p: Point3) -> (f64, f64) {
        self.params_near(p, None)
    }

    pub fn distance(&self, p: Point3) -> f64 {
        let (u, v) = self.params(p);
        self.point(u, v).distance(p)
    }

    /// `p`'s distance from the surface, positive on the side the face's normal points to.
    pub fn signed_distance(&self, p: Point3) -> f64 {
        let (u, v) = self.params(p);
        let q = self.point(u, v);
        let d = p - q;
        match self.normal(u, v) {
            Some(n) => {
                let along = d.dot(n);
                if along >= 0.0 {
                    d.length()
                } else {
                    -d.length()
                }
            }
            None => d.length(),
        }
    }

    /// [`SplineSurface::signed_distance`] and [`SplineSurface::normal_at`] from one search.
    pub fn signed_distance_normal(&self, p: Point3) -> (f64, Vec3) {
        let (u, v) = self.params(p);
        let d = self.derivatives(u, v, 1);
        let q = Point3::new(d[0][0].x, d[0][0].y, d[0][0].z);
        let n = d[1][0]
            .cross(d[0][1])
            .normalized()
            .map(|n| if self.outward { n } else { -n });
        let off = p - q;
        match n {
            Some(n) => (
                if off.dot(n) >= 0.0 {
                    off.length()
                } else {
                    -off.length()
                },
                n,
            ),
            None => (off.length(), Vec3::Z),
        }
    }

    /// The normal at the point nearest `p`.
    pub fn normal_at(&self, p: Point3) -> Vec3 {
        let (u, v) = self.params(p);
        self.normal(u, v).unwrap_or(Vec3::Z)
    }

    /// Each `t` where the line `point + t dir` crosses the surface, in increasing order.
    pub fn line_hits(&self, point: Point3, dir: Vec3) -> Vec<f64> {
        let inv = inverse(&self.place);
        self.data
            .line_hits(inv.apply_point(point), inv.apply_vector(dir))
            .into_iter()
            .map(|h| h.0)
            .collect()
    }

    /// The surface moved by `t`. A mirror turns `S_u × S_v` the other way round, so the
    /// face keeps its outward normal by turning `outward`.
    pub fn transformed(&self, t: &Transform) -> SplineSurface {
        SplineSurface {
            place: self.place.then(t),
            outward: self.outward != t.is_mirror(),
            ..*self
        }
    }

    pub fn reversed(&self) -> SplineSurface {
        SplineSurface {
            outward: !self.outward,
            ..*self
        }
    }

    /// A length it is about the size of, round its bends: what steps along it are scaled
    /// by.
    pub fn scale(&self) -> f64 {
        self.data.scale()
    }

    /// Its curve of constant `v` (`d = 0`) or of constant `u` (`d = 1`) through `at`, placed.
    pub fn iso(&self, d: usize, at: f64) -> SplineCurve {
        SplineCurve {
            data: intern_curve(self.data.surface.iso(d, at)),
            place: self.place,
            up: true,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Polynomial forms.

/// The Bernstein polynomial `B_i^n(t)`.
pub(crate) fn bernstein(n: usize, i: usize, t: f64) -> f64 {
    binomial(n, i) * t.powi(i as i32) * (1.0 - t).powi((n - i) as i32)
}

/// The point at `t` of the Bézier curve of these poles, by de Casteljau's construction.
pub(crate) fn bezier_point(poles: &[Point3], t: f64) -> Point3 {
    let mut work: Vec<Point3> = poles.to_vec();
    let n = work.len();
    for r in 1..n {
        for k in 0..(n - r) {
            work[k] = work[k] + (work[k + 1] - work[k]) * t;
        }
    }
    work.first().copied().unwrap_or(Point3::ORIGIN)
}

/// The poles of the same Bézier curve one degree higher.
pub(crate) fn elevated(poles: &[Point3]) -> Vec<Point3> {
    let n = poles.len().saturating_sub(1);
    let mut out = Vec::with_capacity(n + 2);
    if let Some(&first) = poles.first() {
        out.push(first);
    }
    for i in 1..=n {
        let a = i as f64 / (n + 1) as f64;
        out.push(poles[i - 1] + (poles[i] - poles[i - 1]) * (1.0 - a));
    }
    if let Some(&last) = poles.last() {
        out.push(last);
    }
    out
}

/// The least-squares solution of `a x = b` (rows of `a`, rows of `b`), by Householder's
/// reflections: `None` when `a` has not full rank.
// Matrix algebra, by row and column.
#[allow(clippy::needless_range_loop)]
fn least_squares(mut a: Vec<Vec<f64>>, mut b: Vec<Vec<f64>>) -> Option<Vec<Vec<f64>>> {
    let rows = a.len();
    let cols = a.first().map_or(0, Vec::len);
    let k = b.first().map_or(0, Vec::len);
    for c in 0..cols {
        let norm = (c..rows).map(|r| a[r][c] * a[r][c]).sum::<f64>().sqrt();
        if norm < 1e-300 {
            return None;
        }
        let alpha = if a[c][c] > 0.0 { -norm } else { norm };
        let mut v: Vec<f64> = (c..rows).map(|r| a[r][c]).collect();
        v[0] -= alpha;
        let vv: f64 = v.iter().map(|x| x * x).sum();
        if vv < 1e-300 {
            continue;
        }
        for j in c..cols {
            let dot: f64 = (c..rows).map(|r| v[r - c] * a[r][j]).sum();
            let f = 2.0 * dot / vv;
            for r in c..rows {
                a[r][j] -= f * v[r - c];
            }
        }
        for j in 0..k {
            let dot: f64 = (c..rows).map(|r| v[r - c] * b[r][j]).sum();
            let f = 2.0 * dot / vv;
            for r in c..rows {
                b[r][j] -= f * v[r - c];
            }
        }
    }
    let mut x = vec![vec![0.0; k]; cols];
    for c in (0..cols).rev() {
        if a[c][c].abs() < 1e-300 {
            return None;
        }
        for j in 0..k {
            let mut s = b[c][j];
            for i in (c + 1)..cols {
                s -= a[c][i] * x[i][j];
            }
            x[c][j] = s / a[c][c];
        }
    }
    Some(x)
}

/// `x` with `a x = b` (each row of `b` a right-hand side's entries), by Gaussian
/// elimination with partial pivoting: `None` when `a` is singular.
// Matrix algebra, by row and column.
#[allow(clippy::needless_range_loop)]
pub(crate) fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<Vec<f64>>) -> Option<Vec<Vec<f64>>> {
    let n = a.len();
    for c in 0..n {
        let pivot = (c..n).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[pivot][c].abs() < 1e-300 {
            return None;
        }
        a.swap(c, pivot);
        b.swap(c, pivot);
        for r in (c + 1)..n {
            let f = a[r][c] / a[c][c];
            if f == 0.0 {
                continue;
            }
            for j in c..n {
                a[r][j] -= f * a[c][j];
            }
            for j in 0..b[r].len() {
                b[r][j] -= f * b[c][j];
            }
        }
    }
    let k = b.first().map_or(0, Vec::len);
    let mut x = vec![vec![0.0; k]; n];
    for c in (0..n).rev() {
        for j in 0..k {
            let mut s = b[c][j];
            for i in (c + 1)..n {
                s -= a[c][i] * x[i][j];
            }
            x[c][j] = s / a[c][c];
        }
    }
    Some(x)
}

/// The cubic B-spline through `points` (at least two), each at its share of the polygon
/// through them (chord length), the knots averaged from those shares (the NURBS Book's A9.1):
/// a line for two points, a parabola for three. The collocation system is banded and
/// totally positive, solved without pivoting in time linear in the points.
pub(crate) fn interpolate(points: &[Point3]) -> Option<BSplineCurve> {
    let n = points.len();
    if n < 2 {
        return None;
    }
    let degree = 3.min(n - 1);
    let mut t = vec![0.0; n];
    for i in 1..n {
        t[i] = t[i - 1] + points[i].distance(points[i - 1]);
    }
    let total = t[n - 1];
    if total.is_nan() || total <= 0.0 {
        return None;
    }
    for x in &mut t {
        *x /= total;
    }
    let p = degree;
    let mut knots = vec![0.0; p + 1];
    for j in 1..n - p {
        knots.push(t[j..j + p].iter().sum::<f64>() / p as f64);
    }
    knots.extend(std::iter::repeat_n(1.0, p + 1));
    // Row k: the basis functions at t[k], in the columns span − p ..= span.
    let mut rows: Vec<(usize, Vec<f64>)> = Vec::with_capacity(n);
    for &tk in &t {
        let span = find_span(&knots, p, n, tk);
        let b = basis_derivatives(&knots, span, tk, p, 0);
        rows.push((span - p, b[0][..=p].to_vec()));
    }
    // Banded elimination: each row's entries from its first column on.
    let mut rhs: Vec<Vec3> = points.iter().map(|q| q.to_vec()).collect();
    let width = 2 * p + 1;
    let mut band: Vec<(usize, Vec<f64>)> = rows
        .into_iter()
        .map(|(first, b)| {
            let mut v = vec![0.0; width];
            v[..b.len()].copy_from_slice(&b);
            (first, v)
        })
        .collect();
    for k in 0..n {
        let (first_k, row_k) = band[k].clone();
        let col = k;
        let pivot = row_k.get(col.checked_sub(first_k)?).copied()?;
        if pivot.abs() < 1e-300 {
            return None;
        }
        for r in (k + 1)..n.min(k + width) {
            let (first_r, ref mut row_r) = band[r];
            let Some(off) = col.checked_sub(first_r) else {
                continue;
            };
            if off >= width || row_r[off] == 0.0 {
                continue;
            }
            let f = row_r[off] / pivot;
            for c in col..(first_k + width).min(n) {
                let (i_k, i_r) = (c - first_k, c.checked_sub(first_r));
                if let Some(i_r) = i_r
                    && i_r < width
                    && i_k < width
                {
                    row_r[i_r] -= f * row_k[i_k];
                }
            }
            rhs[r] = rhs[r] - rhs[k] * f;
        }
    }
    let mut x = vec![Vec3::ZERO; n];
    for k in (0..n).rev() {
        let (first, row) = &band[k];
        let mut s = rhs[k];
        for (c, &xc) in x
            .iter()
            .enumerate()
            .take((first + width).min(n))
            .skip(k + 1)
        {
            if let Some(i) = c.checked_sub(*first)
                && i < width
            {
                s = s - xc * row[i];
            }
        }
        x[k] = s * (1.0 / row[k - first]);
    }
    Some(BSplineCurve {
        degree: p,
        knots,
        poles: x.iter().map(|v| Point3::new(v.x, v.y, v.z)).collect(),
        weights: None,
    })
}

/// A cubic B-spline within `tolerance` of the curve `at(s)` for `s` from 0 to `extent`:
/// through more and more points of it, evenly along `s`, until the curve's points halfway
/// between them are that close to it (at most 4096 pieces).
pub(crate) fn fit(
    at: &impl Fn(f64) -> Point3,
    extent: f64,
    tolerance: f64,
) -> Option<BSplineCurve> {
    let mut n = 16usize;
    loop {
        let points: Vec<Point3> = (0..=n).map(|i| at(extent * i as f64 / n as f64)).collect();
        let curve = interpolate(&points)?;
        let samples = curve.sample();
        let worst = (0..n)
            .map(|i| {
                let p = at(extent * (i as f64 + 0.5) / n as f64);
                curve.point(curve.nearest(p, &samples, false)).distance(p)
            })
            .fold(0.0, f64::max);
        if worst <= tolerance || n >= 4096 {
            return Some(curve);
        }
        n *= 2;
    }
}

/// The tolerance OpenCASCADE's loft keeps the polynomial form of an arc within (observed).
pub(crate) const ARC_TOLERANCE: f64 = 1e-7;

/// The highest degree it goes to before halving the arc (observed).
pub(crate) const ARC_MAX_DEGREE: usize = 14;

static FIT_RULE: LazyLock<Vec<(f64, f64)>> =
    LazyLock::new(|| crate::revolution::gauss_legendre(64));

/// The polynomial form OpenCASCADE's loft gives the circular arc about `center` in the
/// plane of the unit vectors `x` and `y`, of `radius`, from angle `start` turning `sweep`
/// (towards `y`): as observed of it as a black box, the Bézier pieces of equal angle, as
/// few as can be (one, else halves, and so on), each the polynomial of the least degree
/// from 3 to 14 within [`ARC_TOLERANCE`] of the arc at its point of the same fraction of
/// its angle (at a thousand and one points), with the ends and their tangents exact and
/// the rest the least-squares fit over the piece.
pub(crate) fn polynomial_arc(
    center: Point3,
    x: Vec3,
    y: Vec3,
    radius: f64,
    start: f64,
    sweep: f64,
) -> Vec<Vec<Point3>> {
    let at = |a: f64| center + (x * a.cos() + y * a.sin()) * radius;
    for n in 3..=ARC_MAX_DEGREE {
        if let Some(poles) = arc_fit(
            &at,
            &|a: f64| (x * -a.sin() + y * a.cos()) * radius,
            start,
            sweep,
            n,
        ) && arc_error(&poles, &at, start, sweep) <= ARC_TOLERANCE
        {
            return vec![poles];
        }
    }
    if sweep.abs() < 1e-6 {
        // Too small to need halving; the end and tangent conditions alone.
        return arc_fit(
            &at,
            &|a: f64| (x * -a.sin() + y * a.cos()) * radius,
            start,
            sweep,
            3,
        )
        .into_iter()
        .collect();
    }
    let half = sweep / 2.0;
    let mut out = polynomial_arc(center, x, y, radius, start, half);
    out.extend(polynomial_arc(center, x, y, radius, start + half, half));
    out
}

/// The degree-`n` fit of the arc `at(start + sweep t)` for `t` in `[0, 1]`, its ends and
/// their tangents (`tangent(a)` the derivative along the angle) exact.
fn arc_fit(
    at: &impl Fn(f64) -> Point3,
    tangent: &impl Fn(f64) -> Vec3,
    start: f64,
    sweep: f64,
    n: usize,
) -> Option<Vec<Point3>> {
    let end = start + sweep;
    let mut poles = vec![Point3::ORIGIN; n + 1];
    poles[0] = at(start);
    poles[n] = at(end);
    poles[1] = poles[0] + tangent(start) * (sweep / n as f64);
    poles[n - 1] = poles[n] - tangent(end) * (sweep / n as f64);
    let fixed = [0, 1, n - 1, n];
    let free: Vec<usize> = (2..n.saturating_sub(1)).collect();
    if free.is_empty() {
        return Some(poles);
    }
    let mut a = Vec::with_capacity(FIT_RULE.len());
    let mut b = Vec::with_capacity(FIT_RULE.len());
    for &(xq, wq) in FIT_RULE.iter() {
        let t = (xq + 1.0) / 2.0;
        let w = (wq / 2.0).sqrt();
        let target = at(start + sweep * t);
        let mut rest = target.to_vec();
        for &i in &fixed {
            rest = rest - poles[i].to_vec() * bernstein(n, i, t);
        }
        a.push(
            free.iter()
                .map(|&i| bernstein(n, i, t) * w)
                .collect::<Vec<f64>>(),
        );
        b.push(vec![rest.x * w, rest.y * w, rest.z * w]);
    }
    let x = least_squares(a, b)?;
    for (k, &i) in free.iter().enumerate() {
        poles[i] = Point3::new(x[k][0], x[k][1], x[k][2]);
    }
    Some(poles)
}

/// How far the Bézier curve of `poles` strays from the arc, point for point by fraction of
/// its angle, at a thousand and one points.
fn arc_error(poles: &[Point3], at: &impl Fn(f64) -> Point3, start: f64, sweep: f64) -> f64 {
    (0..=1000)
        .map(|i| {
            let t = f64::from(i) / 1000.0;
            bezier_point(poles, t).distance(at(start + sweep * t))
        })
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use std::f64::consts::{FRAC_PI_4, PI, TAU};

    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    /// The rational quadratic quarter circle of radius 2 in the xy plane.
    fn quarter() -> BSplineCurve {
        let w = std::f64::consts::FRAC_1_SQRT_2;
        BSplineCurve::new(
            2,
            vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec![
                Point3::new(2.0, 0.0, 0.0),
                Point3::new(2.0, 2.0, 0.0),
                Point3::new(0.0, 2.0, 0.0),
            ],
            Some(vec![1.0, w, 1.0]),
        )
        .unwrap()
    }

    #[test]
    fn a_rational_quarter_circle_stays_on_its_circle_and_inserting_knots_keeps_it() {
        let c = quarter();
        let refined = c.with_knot(0.3, 1).with_knot(0.7, 2);
        for i in 0..=20 {
            let u = f64::from(i) / 20.0;
            let p = c.point(u);
            assert!(close(p.to_vec().length(), 2.0, 1e-14), "{p:?}");
            assert!(p.distance(refined.point(u)) < 1e-14);
        }
        // Derivatives against differences.
        let h = 1e-6;
        for &u in &[0.1, 0.5, 0.93] {
            let d = c.derivatives(u, 2);
            let fd = (c.point(u + h) - c.point(u - h)) * (0.5 / h);
            assert!((d[1] - fd).length() < 1e-7);
            let fd2 = (c.derivatives(u + h, 1)[1] - c.derivatives(u - h, 1)[1]) * (0.5 / h);
            assert!((d[2] - fd2).length() < 1e-6);
        }
        assert!(close(c.length(0.0, 1.0), PI, 1e-12));
        let p = Point3::new(3.0, 3.0, 1.0);
        let u = c.nearest(p, &c.sample(), false);
        assert!(
            c.point(u)
                .distance(Point3::new(2.0f64.sqrt(), 2.0f64.sqrt(), 0.0))
                < 1e-12
        );
    }

    #[test]
    fn a_surface_s_derivatives_and_nearest_points() {
        // A rational quarter cylinder (radius 2, along z from 0 to 5) as a surface.
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let ring = [
            Point3::new(2.0, 0.0, 0.0),
            Point3::new(2.0, 2.0, 0.0),
            Point3::new(0.0, 2.0, 0.0),
        ];
        let mut poles = Vec::new();
        let mut weights = Vec::new();
        for (i, p) in ring.iter().enumerate() {
            for z in [0.0, 5.0] {
                poles.push(Point3::new(p.x, p.y, z));
                weights.push(if i == 1 { w } else { 1.0 });
            }
        }
        let s = BSplineSurface::new(
            [2, 1],
            [vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0], vec![0.0, 0.0, 1.0, 1.0]],
            [3, 2],
            poles,
            Some(weights),
        )
        .unwrap();
        let h = 1e-6;
        for &(u, v) in &[(0.2, 0.3), (0.7, 0.9)] {
            let d = s.derivatives(u, v, 2);
            let p = d[0][0];
            assert!(close((p.x * p.x + p.y * p.y).sqrt(), 2.0, 1e-14));
            let du = (s.point(u + h, v) - s.point(u - h, v)) * (0.5 / h);
            let dv = (s.point(u, v + h) - s.point(u, v - h)) * (0.5 / h);
            assert!((d[1][0] - du).length() < 1e-7 && (d[0][1] - dv).length() < 1e-7);
            let duu =
                (s.derivatives(u + h, v, 1)[1][0] - s.derivatives(u - h, v, 1)[1][0]) * (0.5 / h);
            let duv =
                (s.derivatives(u, v + h, 1)[1][0] - s.derivatives(u, v - h, 1)[1][0]) * (0.5 / h);
            assert!((d[2][0] - duu).length() < 1e-6 && (d[1][1] - duv).length() < 1e-6);
        }
        let data = intern_surface(s.clone());
        let (u, v) = data.nearest(Point3::new(3.0, 3.0, 2.0), None);
        let q = s.point(u, v);
        assert!(
            q.distance(Point3::new(2.0f64.sqrt(), 2.0f64.sqrt(), 2.0)) < 1e-10,
            "{q:?}"
        );
        // A line through the quarter cylinder along x at y = 1, z = 1 crosses it once.
        let hits = data.line_hits(Point3::new(-1.0, 1.0, 1.0), Vec3::X);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(close(hits[0].0, 1.0 + 3.0f64.sqrt(), 1e-10));
        // Interning finds the same surface again.
        assert!(std::ptr::eq(data, intern_surface(s)));
    }

    #[test]
    fn arcs_take_the_polynomial_form_opencascade_gives_them() {
        // Poles OpenCASCADE's loft made of a 45° arc of radius 8 and a whole circle of
        // radius 10 (probed), to the digits it printed.
        let p45 = polynomial_arc(Point3::ORIGIN, Vec3::X, Vec3::Y, 8.0, 0.0, FRAC_PI_4);
        let want45 = [
            (8.0, 0.0),
            (8.0, 0.897597901026),
            (7.88250466051, 1.79519578365),
            (7.64751434731, 2.67433766416),
            (7.29865155171, 3.5165669567),
            (6.84316761036, 4.30437738601),
            (6.29155181209, 5.0221566869),
            (5.65685424949, 5.65685424949),
        ];
        assert_eq!(p45.len(), 1);
        assert_eq!(p45[0].len(), want45.len());
        for (p, w) in p45[0].iter().zip(want45) {
            assert!(
                close(p.x, w.0, 1e-10) && close(p.y, w.1, 1e-10),
                "{p:?} {w:?}"
            );
        }
        let full = polynomial_arc(Point3::ORIGIN, Vec3::X, Vec3::Y, 10.0, 0.0, TAU);
        assert_eq!(full.len(), 1);
        assert_eq!(full[0].len(), 15);
        let want = [
            (10.0, 0.0),
            (10.0, 4.48798950513),
            (7.830856388, 8.97598066687),
            (3.49256639097, 12.3281934657),
            (-2.36610614737, 13.4089759582),
            (-8.44775660407, 11.4897686101),
            (-13.0904170165, 6.6588524526),
            (-14.8382860205, 0.0),
        ];
        for (p, w) in full[0].iter().zip(want) {
            assert!(
                close(p.x, w.0, 1e-9) && close(p.y, w.1, 1e-9),
                "{p:?} {w:?}"
            );
        }
        // The degrees it chose for arcs of these angles and radii.
        for (r, deg, want) in [
            (0.1, 5.0, 3),
            (1.0, 20.0, 5),
            (10.0, 90.0, 8),
            (10.0, 180.0, 10),
            (100.0, 240.0, 13),
            (1000.0, 270.0, 14),
        ] {
            let p = polynomial_arc(
                Point3::ORIGIN,
                Vec3::X,
                Vec3::Y,
                r,
                0.0,
                f64::to_radians(deg),
            );
            assert_eq!(p.len(), 1);
            assert_eq!(p[0].len() - 1, want, "radius {r}, {deg}°");
        }
        // A whole circle of radius 100 needs two halves of degree 11.
        let big = polynomial_arc(Point3::ORIGIN, Vec3::X, Vec3::Y, 100.0, 0.0, TAU);
        assert_eq!(big.len(), 2);
        assert!(big.iter().all(|p| p.len() == 12));
        assert!(
            close(big[0][1].y, 28.5599332144451, 1e-8),
            "{:?}",
            big[0][1]
        );
    }

    #[test]
    fn raising_a_bezier_s_degree_keeps_its_curve() {
        let poles = vec![
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(1.0, 2.0, 0.0),
            Point3::new(3.0, -1.0, 1.0),
        ];
        let up = elevated(&elevated(&poles));
        assert_eq!(up.len(), 5);
        for i in 0..=10 {
            let t = f64::from(i) / 10.0;
            assert!(bezier_point(&poles, t).distance(bezier_point(&up, t)) < 1e-14);
        }
    }
}
