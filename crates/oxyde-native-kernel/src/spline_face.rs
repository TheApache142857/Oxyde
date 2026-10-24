//! Faces on B-spline surfaces: their boundaries laid out in the surface's `(u, v)` plane,
//! their area and mass integrals there, and where they bulge out between their edges.
//!
//! An edge's points are taken onto the surface by the nearest point, each search started
//! from where the edge's previous point was, `u` (and `v`) unwrapped as the loop goes round a
//! surface that closes on itself. The integrals are Green's theorem in `(u, v)`:
//! `∬ f du dv = ∮ F dv`, `F(u, v) = ∫ f(t, v) dt` from the start of the domain, both by
//! Gauss–Legendre quadrature, the inner one over each knot span, the outer one along each
//! edge, its `dv` from the edge's tangent taken onto the surface's.

// Indices into samples gathered here.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Mutex, PoisonError};

use oxyde_geom::{Point3, Vec3};

use crate::brep::{EdgeUse, Face, Solid};
use crate::geometry::{FaceIntegrals, SplineSurface, component};
use crate::revolution::{GAUSS, WalkEdge};

/// One edge use of a face on a B-spline surface, laid out in the surface's `(u, v)` plane.
#[derive(Debug, Clone)]
pub(crate) struct UvUse {
    pub edge_use: EdgeUse,
    pub walk: WalkEdge,
    /// Points along the use: how far along it, its `u` and `v`, unwrapped from the loop's
    /// start.
    pub samples: Vec<(f64, f64, f64)>,
}

/// The period along `d` of a surface that closes that way.
fn period(surface: &SplineSurface, d: usize) -> Option<f64> {
    surface.data.closed[d].then(|| {
        let (a, b) = surface.domain(d);
        b - a
    })
}

/// `x` moved by whole periods to be nearest `near`.
fn unwrap(x: f64, near: f64, period: Option<f64>) -> f64 {
    match period {
        Some(p) => x + p * ((near - x) / p).round(),
        None => x,
    }
}

/// `x` taken round into the domain `(a, b)` of a surface that closes that way.
fn wrap(x: f64, (a, b): (f64, f64), closed: bool) -> f64 {
    if closed {
        a + (x - a).rem_euclid(b - a)
    } else {
        x
    }
}

/// The parameters of `p` on the surface, its search started from `near` (unwrapped), and
/// unwrapped to be nearest it.
pub(crate) fn uv_near(surface: &SplineSurface, p: Point3, near: (f64, f64)) -> (f64, f64) {
    let hint = (
        wrap(near.0, surface.domain(0), surface.data.closed[0]),
        wrap(near.1, surface.domain(1), surface.data.closed[1]),
    );
    let (u, v) = surface.params_near(p, Some(hint));
    (
        unwrap(u, near.0, period(surface, 0)),
        unwrap(v, near.1, period(surface, 1)),
    )
}

impl UvUse {
    /// `(u, v)` of `p`, the use's point at `s`, its search started from the samples about
    /// it.
    fn uv_of(&self, surface: &SplineSurface, s: f64, p: Point3) -> (f64, f64) {
        let k = self
            .samples
            .partition_point(|&(t, _, _)| t <= s)
            .clamp(1, self.samples.len().max(2) - 1);
        let (s0, u0, v0) = self.samples[k - 1];
        let (s1, u1, v1) = self.samples[k.min(self.samples.len() - 1)];
        let f = if s1 > s0 { (s - s0) / (s1 - s0) } else { 0.0 };
        uv_near(surface, p, (u0 + (u1 - u0) * f, v0 + (v1 - v0) * f))
    }

    /// `(u, v)` at `s` and its rate along the use, from the edge's tangent taken onto the
    /// surface's.
    fn uv_rate(&self, surface: &SplineSurface, s: f64) -> ((f64, f64), (f64, f64)) {
        let w = &self.walk;
        let (p, tangent) = w.curve.point_rate(w.a, w.b, w.sweep, s);
        let (u, v) = self.uv_of(surface, s, p);
        let d = surface.derivatives(
            wrap(u, surface.domain(0), surface.data.closed[0]),
            wrap(v, surface.domain(1), surface.data.closed[1]),
            1,
        );
        let (su, sv) = (d[1][0], d[0][1]);
        let (a11, a12, a22) = (su.dot(su), su.dot(sv), sv.dot(sv));
        let (b1, b2) = (su.dot(tangent), sv.dot(tangent));
        let det = a11 * a22 - a12 * a12;
        if det.abs() < 1e-300 {
            return ((u, v), (0.0, 0.0));
        }
        (
            (u, v),
            ((b1 * a22 - b2 * a12) / det, (a11 * b2 - a12 * b1) / det),
        )
    }

    /// Whether `v` stays the same all along it (within `tol` of its range).
    fn along_u(&self, tol: f64) -> bool {
        let v0 = self.samples.first().map_or(0.0, |s| s.2);
        self.samples.iter().all(|s| (s.2 - v0).abs() <= tol)
    }

    /// Where along it the curve under it changes from one smooth piece to the next.
    fn breaks(&self) -> Vec<f64> {
        self.walk.curve.smooth_breaks(self.walk.a, self.walk.sweep)
    }
}

/// How many points along each edge the layout starts its searches from.
const SAMPLES: usize = 32;

/// The face's loops laid out in its surface's `(u, v)` plane, each after the first (the
/// holes) moved by whole periods to lie about the first.
pub(crate) fn uv_loops(
    solid: &Solid,
    face: &Face,
    surface: &SplineSurface,
) -> Result<Vec<Vec<UvUse>>, String> {
    let mut out: Vec<Vec<UvUse>> = Vec::with_capacity(face.loops.len());
    for uses in &face.loops {
        let mut laid: Vec<UvUse> = Vec::with_capacity(uses.len());
        let mut near: Option<(f64, f64)> = None;
        for &u in uses {
            let walk = solid.walk_edge(u);
            let n = SAMPLES;
            let mut samples = Vec::with_capacity(n + 1);
            for i in 0..=n {
                let s = walk.sweep * i as f64 / n as f64;
                let p = walk.curve.at(walk.a, walk.b, s).unwrap_or(walk.a);
                let uv = match near {
                    Some(prev) => uv_near(surface, p, prev),
                    None => surface.params(p),
                };
                samples.push((s, uv.0, uv.1));
                near = Some(uv);
            }
            laid.push(UvUse {
                edge_use: u,
                walk,
                samples,
            });
        }
        if let Some(outer) = out.first() {
            let middle = |l: &[UvUse]| {
                let pts: Vec<(f64, f64)> = l
                    .iter()
                    .flat_map(|u| u.samples.iter().map(|s| (s.1, s.2)))
                    .collect();
                let n = pts.len().max(1) as f64;
                (
                    pts.iter().map(|p| p.0).sum::<f64>() / n,
                    pts.iter().map(|p| p.1).sum::<f64>() / n,
                )
            };
            let (om, m) = (middle(outer), middle(&laid));
            let shift = |d: usize, from: f64, to: f64| {
                period(surface, d).map_or(0.0, |p| p * ((to - from) / p).round())
            };
            let (du, dv) = (shift(0, m.0, om.0), shift(1, m.1, om.1));
            for u in &mut laid {
                for s in &mut u.samples {
                    s.1 += du;
                    s.2 += dv;
                }
            }
        }
        out.push(laid);
    }
    Ok(out)
}

/// The loops as closed polygons of `(u, v)`.
pub(crate) fn uv_polygons(loops: &[Vec<UvUse>]) -> Vec<Vec<(f64, f64)>> {
    loops
        .iter()
        .map(|l| {
            l.iter()
                .flat_map(|u| u.samples[..u.samples.len() - 1].iter().map(|s| (s.1, s.2)))
                .collect()
        })
        .collect()
}

/// Whether `(u, v)` is inside the polygons, by the parity of their crossings of a line from
/// it along `u`.
pub(crate) fn uv_inside(polygons: &[Vec<(f64, f64)>], u: f64, v: f64) -> bool {
    let mut crossings = 0;
    for p in polygons {
        for (k, &(u0, v0)) in p.iter().enumerate() {
            let (u1, v1) = p[(k + 1) % p.len()];
            if (v0 > v) != (v1 > v) {
                let at = u0 + (v - v0) / (v1 - v0) * (u1 - u0);
                if at > u {
                    crossings += 1;
                }
            }
        }
    }
    crossings % 2 == 1
}

/// How many integrals a face's are made of: its area, `∫ x dA`, `∫ n dA`, `∫ (x − c) nᵀ dA`
/// and `∫ (x − c) ((x − c)·n) dA` about a point `c` of the face, from which its
/// [`FaceIntegrals`] about any point follow.
const N: usize = 19;

/// The integrands at `(u, v)` about `c`, through `S_u × S_v` (the face's normal or the
/// other way): the area element's size, the first moment's, the normal, the moments.
fn integrands(surface: &SplineSurface, u: f64, v: f64, c: Point3) -> [f64; N] {
    let d = surface.derivatives(u, v, 1);
    let n = d[1][0].cross(d[0][1]);
    let a = n.length();
    let x = d[0][0];
    let y = x - c.to_vec();
    let flux = y.dot(n);
    [
        a,
        x.x * a,
        x.y * a,
        x.z * a,
        n.x,
        n.y,
        n.z,
        y.x * n.x,
        y.x * n.y,
        y.x * n.z,
        y.y * n.x,
        y.y * n.y,
        y.y * n.z,
        y.z * n.x,
        y.z * n.y,
        y.z * n.z,
        y.x * flux,
        y.y * flux,
        y.z * flux,
    ]
}

/// `∫ f(t, v) dt` from the start of the domain to `u` (wrapping round a surface that closes
/// along `u`), over each knot span in halves.
fn inner(surface: &SplineSurface, u: f64, v: f64, c: Point3) -> [f64; N] {
    let (a, b) = surface.domain(0);
    let closed = surface.data.closed[0];
    let breaks = surface.data.surface.breaks(0);
    let l = b - a;
    let mut total = [0.0; N];
    let (lo, hi, sign) = if u >= a { (a, u, 1.0) } else { (u, a, -1.0) };
    if hi <= lo {
        return total;
    }
    // The span ends between lo and hi, taken round.
    let mut cuts: Vec<f64> = vec![lo, hi];
    let first = if closed {
        ((lo - a) / l).floor() as i64
    } else {
        0
    };
    let last = if closed {
        ((hi - a) / l).ceil() as i64
    } else {
        0
    };
    for k in first..=last {
        for &x in &breaks {
            let y = x + k as f64 * l;
            if y > lo && y < hi {
                cuts.push(y);
            }
        }
    }
    cuts.sort_by(f64::total_cmp);
    let vv = wrap(v, surface.domain(1), surface.data.closed[1]);
    for w in cuts.windows(2) {
        let (x0, x1) = (w[0], w[1]);
        if x1 - x0 <= 0.0 {
            continue;
        }
        let pieces = 2;
        let h = (x1 - x0) / f64::from(pieces);
        for k in 0..pieces {
            let (p0, p1) = (x0 + h * f64::from(k), x0 + h * f64::from(k + 1));
            let (mid, half) = ((p0 + p1) / 2.0, (p1 - p0) / 2.0);
            for &(xq, wq) in GAUSS.iter() {
                let t = wrap(mid + half * xq, (a, b), closed);
                let f = integrands(surface, t.clamp(a, b), vv, c);
                for (acc, value) in total.iter_mut().zip(f) {
                    *acc += wq * half * value * sign;
                }
            }
        }
    }
    total
}

/// A face as the cache of integrals knows it: its surface, each edge use's ends, middle
/// and extent, as bits.
type Key = Vec<u64>;

/// The faces of one hash the cache holds, each with its moments.
type Bucket = Vec<(Key, Moments)>;

/// A face's integrals about the point `c` of it ([`N`]), its area and its moment taken
/// the way its normal points.
#[derive(Debug, Clone, Copy)]
struct Moments {
    c: Point3,
    v: [f64; N],
}

impl Moments {
    /// The face's integrals about `r`: with `d = c − r`, `∫ (x − r)·n = tr M + d·∫n` and
    /// `∫ (x − r)((x − r)·n) = Q + M d + d tr M + d (d·∫n)`.
    fn about(&self, r: Point3) -> FaceIntegrals {
        let v = &self.v;
        let d = self.c - r;
        let normal = Vec3::new(v[4], v[5], v[6]);
        let m = |i: usize, j: usize| v[7 + 3 * i + j];
        let trace = m(0, 0) + m(1, 1) + m(2, 2);
        let md = Vec3::new(
            m(0, 0) * d.x + m(0, 1) * d.y + m(0, 2) * d.z,
            m(1, 0) * d.x + m(1, 1) * d.y + m(1, 2) * d.z,
            m(2, 0) * d.x + m(2, 1) * d.y + m(2, 2) * d.z,
        );
        let q = Vec3::new(v[16], v[17], v[18]);
        FaceIntegrals {
            area: v[0],
            first: Vec3::new(v[1], v[2], v[3]),
            flux: trace + d.dot(normal),
            flux_moment: q + md + d * trace + d * d.dot(normal),
        }
    }
}

/// The integrals of the B-spline faces taken lately, by a hash of the face, the face itself
/// kept to tell faces of one hash apart: a face's integrals are asked for again and again
/// (checked, measured, its solid's mass), about one point or another.
static INTEGRALS: LazyLock<Mutex<HashMap<u64, Bucket>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// How many faces the cache holds before it starts again.
const CACHE_SIZE: usize = 4096;

fn key_of(solid: &Solid, face: &Face, surface: &SplineSurface) -> Key {
    let mut key = vec![surface.data.hash, u64::from(surface.outward)];
    for row in surface.place.rows {
        key.extend(row.iter().map(|x| x.to_bits()));
    }
    let point =
        |key: &mut Key, p: Point3| key.extend([p.x.to_bits(), p.y.to_bits(), p.z.to_bits()]);
    for uses in &face.loops {
        key.push(u64::MAX);
        for &u in uses {
            let w = solid.walk_edge(u);
            point(&mut key, w.a);
            point(&mut key, w.b);
            point(&mut key, w.mid);
            key.push(w.sweep.to_bits());
        }
    }
    key
}

/// The face's area and mass integrals about `reference` ([`FaceIntegrals`]), its
/// [`Moments`] remembered.
pub(crate) fn integrals(
    solid: &Solid,
    face: &Face,
    surface: &SplineSurface,
    reference: Point3,
) -> Result<FaceIntegrals, String> {
    let key = key_of(solid, face, surface);
    let hash = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        h.finish()
    };
    if let Some(found) = INTEGRALS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&hash)
        .and_then(|bucket| bucket.iter().find(|(k, _)| *k == key).map(|(_, m)| *m))
    {
        return Ok(found.about(reference));
    }
    let moments = moments_of(solid, face, surface)?;
    let mut map = INTEGRALS.lock().unwrap_or_else(PoisonError::into_inner);
    if map.len() >= CACHE_SIZE {
        map.clear();
    }
    map.entry(hash).or_default().push((key, moments));
    Ok(moments.about(reference))
}

/// `∫ f` from `a` to `b` (each of its values), by the Gauss–Legendre rule over the pieces
/// between `cuts` (each at least as many as `pieces` of the whole). Without cuts, each
/// piece halved while its halves' sum differs from it by more than `tolerance` (for each
/// value), down to a 64th of it; with them (where `f` is smooth between them, and only
/// there), each piece taken whole.
fn adaptive(
    f: &impl Fn(f64) -> [f64; N],
    a: f64,
    b: f64,
    cuts: &[f64],
    pieces: u32,
    tolerance: &[f64; N],
) -> [f64; N] {
    let rule = |lo: f64, hi: f64| -> [f64; N] {
        let (mid, half) = ((lo + hi) / 2.0, (hi - lo) / 2.0);
        let mut out = [0.0; N];
        for &(x, w) in GAUSS.iter() {
            for (o, value) in out.iter_mut().zip(f(mid + half * x)) {
                *o += w * half * value;
            }
        }
        out
    };
    let mut ends: Vec<f64> = vec![a];
    ends.extend(cuts.iter().copied().filter(|&c| c > a && c < b));
    ends.push(b);
    let h = (b - a) / f64::from(pieces);
    let mut stack: Vec<(f64, f64, [f64; N], u32)> = Vec::new();
    for w in ends.windows(2) {
        let k = ((w[1] - w[0]) / h).ceil().max(1.0) as u32;
        let step = (w[1] - w[0]) / f64::from(k);
        for i in 0..k {
            let (lo, hi) = (w[0] + step * f64::from(i), w[0] + step * f64::from(i + 1));
            stack.push((lo, hi, rule(lo, hi), 0));
        }
    }
    let mut total = [0.0; N];
    if !cuts.is_empty() {
        for (_, _, whole, _) in stack {
            for (t, v) in total.iter_mut().zip(whole) {
                *t += v;
            }
        }
        return total;
    }
    while let Some((lo, hi, whole, depth)) = stack.pop() {
        let mid = (lo + hi) / 2.0;
        let (left, right) = (rule(lo, mid), rule(mid, hi));
        let halves: [f64; N] = std::array::from_fn(|i| left[i] + right[i]);
        let close = (0..N).all(|i| (halves[i] - whole[i]).abs() <= tolerance[i]);
        if close || depth >= 6 {
            for (t, v) in total.iter_mut().zip(halves) {
                *t += v;
            }
        } else {
            stack.push((lo, mid, left, depth + 1));
            stack.push((mid, hi, right, depth + 1));
        }
    }
    total
}

/// Whether a face's loops go round the whole of its surface's domain (within a millionth of
/// it): a loft's faces, bounded by the surface's own edges.
fn whole_domain(loops: &[Vec<UvUse>], surface: &SplineSurface) -> bool {
    let [(ua, ub), (va, vb)] = [surface.domain(0), surface.domain(1)];
    let (tu, tv) = (1e-6 * (ub - ua), 1e-6 * (vb - va));
    if loops.len() != 1 {
        return false;
    }
    let on_edge = |u: f64, v: f64| {
        (u - ua).abs() <= tu || (u - ub).abs() <= tu || (v - va).abs() <= tv || (v - vb).abs() <= tv
    };
    let polygons = uv_polygons(loops);
    let Some(polygon) = polygons.first() else {
        return false;
    };
    if !polygon.iter().all(|&(u, v)| on_edge(u, v)) {
        return false;
    }
    let n = polygon.len();
    let area: f64 = (0..n)
        .map(|i| {
            let (a, b) = (polygon[i], polygon[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        / 2.0;
    (area.abs() - (ub - ua) * (vb - va)).abs() <= 1e-6 * (ub - ua) * (vb - va)
}

/// The integrals over the whole of the surface's domain: across `u` by Gauss–Legendre over
/// each knot span in halves, along `v` over each span adaptively ([`adaptive`]).
fn domain_moments(surface: &SplineSurface, c: Point3, tolerance: &[f64; N]) -> [f64; N] {
    let mut us: Vec<(f64, f64)> = Vec::new();
    for w in surface.data.surface.breaks(0).windows(2) {
        let h = (w[1] - w[0]) / 2.0;
        for k in 0..2 {
            let (lo, hi) = (w[0] + h * f64::from(k), w[0] + h * f64::from(k + 1));
            let (mid, half) = ((lo + hi) / 2.0, (hi - lo) / 2.0);
            us.extend(GAUSS.iter().map(|&(x, wq)| (mid + half * x, wq * half)));
        }
    }
    let across = |v: f64| -> [f64; N] {
        let mut out = [0.0; N];
        for &(u, wu) in &us {
            let f = integrands(surface, u, v, c);
            for (acc, value) in out.iter_mut().zip(f) {
                *acc += wu * value;
            }
        }
        out
    };
    let mut g = [0.0; N];
    for w in surface.data.surface.breaks(1).windows(2) {
        let sum = adaptive(&across, w[0], w[1], &[], 2, tolerance);
        for (acc, value) in g.iter_mut().zip(sum) {
            *acc += value;
        }
    }
    g
}

/// How close a face's integrals are taken: a part in 10¹¹ (above the rounding of the
/// edges' rates across the surface) of what the face's size makes them, its area of the
/// square of its size, its normal's too, its moments of the cube and the fourth power. And
/// the point of the face they are taken about.
fn tolerance_of(loops: &[Vec<UvUse>]) -> ([f64; N], Point3) {
    let (mut lo, mut hi) = (
        Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY),
        Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY),
    );
    for u in loops.iter().flatten() {
        for &(s, _, _) in &u.samples {
            let w = &u.walk;
            let p = w.curve.at(w.a, w.b, s).unwrap_or(w.a);
            lo = Point3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
            hi = Point3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
        }
    }
    let c = loops
        .first()
        .and_then(|l| l.first())
        .map_or(Point3::ORIGIN, |u| u.walk.a);
    let size = lo.distance(hi).max(1e-6);
    let far = c.to_vec().length().max(size);
    let (l2, l3, l4) = (size * size, size * size * far, size * size * size * far);
    let mut t = [l2 * 1e-11; N];
    for (i, x) in t.iter_mut().enumerate() {
        *x = 1e-11
            * match i {
                0 | 4..=6 => l2,
                1..=3 | 7..=15 => l3,
                _ => l4,
            };
    }
    (t, c)
}

fn moments_of(solid: &Solid, face: &Face, surface: &SplineSurface) -> Result<Moments, String> {
    let loops = uv_loops(solid, face, surface)?;
    let (tolerance, c) = tolerance_of(&loops);
    // Area-like integrals come out as the loops run: negative for a face whose normal is
    // the other way than S_u × S_v; the flux-like ones, through the normal, either way.
    let sign = if surface.outward { 1.0 } else { -1.0 };
    let orient = |mut g: [f64; N], area_sign: f64, flux_sign: f64| {
        for (i, x) in g.iter_mut().enumerate() {
            *x *= if i < 4 { area_sign } else { flux_sign };
        }
        Moments { c, v: g }
    };
    if whole_domain(&loops, surface) {
        return Ok(orient(domain_moments(surface, c, &tolerance), 1.0, sign));
    }
    let (va, vb) = surface.domain(1);
    let mut g = [0.0; N];
    for u in loops.iter().flatten() {
        if u.along_u(1e-12 * (vb - va)) {
            continue;
        }
        let extent = u.walk.sweep;
        let along = |x: f64| -> [f64; N] {
            let ((uu, vv), (_, dv)) = u.uv_rate(surface, x);
            if dv == 0.0 {
                return [0.0; N];
            }
            inner(surface, uu, vv, c).map(|f| f * dv)
        };
        let sum = adaptive(&along, 0.0, extent, &u.breaks(), 4, &tolerance);
        for (acc, value) in g.iter_mut().zip(sum) {
            *acc += value;
        }
    }
    Ok(orient(g, sign, 1.0))
}

/// Points of the face, inside it, where its surface reaches furthest along a model axis
/// between its edges: from the highest and lowest of a grid of its points, by Newton's
/// method on the coordinate's gradient.
pub(crate) fn bulges(solid: &Solid, face: &Face, surface: &SplineSurface) -> Vec<Point3> {
    let Ok(loops) = uv_loops(solid, face, surface) else {
        return Vec::new();
    };
    let polygons = uv_polygons(&loops);
    let (mut ulo, mut uhi, mut vlo, mut vhi) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for &(u, v) in polygons.iter().flatten() {
        ulo = ulo.min(u);
        uhi = uhi.max(u);
        vlo = vlo.min(v);
        vhi = vhi.max(v);
    }
    if !(uhi > ulo && vhi > vlo) {
        return Vec::new();
    }
    let n = 32;
    let at = |i: usize, j: usize| {
        (
            ulo + (uhi - ulo) * i as f64 / n as f64,
            vlo + (vhi - vlo) * j as f64 / n as f64,
        )
    };
    let place = |u: f64, v: f64| {
        (
            wrap(u, surface.domain(0), surface.data.closed[0]),
            wrap(v, surface.domain(1), surface.data.closed[1]),
        )
    };
    let grid: Vec<Vec<Point3>> = (0..=n)
        .map(|i| {
            (0..=n)
                .map(|j| {
                    let (u, v) = at(i, j);
                    let (u, v) = place(u, v);
                    surface.point(u, v)
                })
                .collect()
        })
        .collect();
    let mut out = Vec::new();
    for k in 0..3 {
        for sign in [1.0, -1.0] {
            let value = |p: Point3| sign * component(p.to_vec(), k);
            for i in 1..n {
                for j in 1..n {
                    let c = value(grid[i][j]);
                    let higher = [(0, 1), (2, 1), (1, 0), (1, 2)]
                        .iter()
                        .all(|&(di, dj)| c >= value(grid[i + di - 1][j + dj - 1]));
                    if !higher {
                        continue;
                    }
                    let (mut u, mut v) = at(i, j);
                    let mut converged = false;
                    for _ in 0..30 {
                        let (pu, pv) = place(u, v);
                        let d = surface.derivatives(pu, pv, 2);
                        let g = (component(d[1][0], k), component(d[0][1], k));
                        let (h11, h12, h22) = (
                            component(d[2][0], k),
                            component(d[1][1], k),
                            component(d[0][2], k),
                        );
                        let det = h11 * h22 - h12 * h12;
                        if det.abs() < 1e-300 {
                            break;
                        }
                        let du = (g.0 * h22 - g.1 * h12) / det;
                        let dv = (h11 * g.1 - h12 * g.0) / det;
                        u -= du;
                        v -= dv;
                        if du.abs() + dv.abs() <= 1e-14 * (uhi - ulo + vhi - vlo) {
                            converged = true;
                            break;
                        }
                    }
                    if converged
                        && u > ulo
                        && u < uhi
                        && v > vlo
                        && v < vhi
                        && uv_inside(&polygons, u, v)
                    {
                        let (pu, pv) = place(u, v);
                        out.push(surface.point(pu, pv));
                    }
                }
            }
        }
    }
    out
}

/// Steps along `u` and `v` fine enough for chords within `deflection` of the surface over
/// the range given: from the largest second derivatives over a grid of it.
pub(crate) fn steps(
    surface: &SplineSurface,
    (ulo, uhi): (f64, f64),
    (vlo, vhi): (f64, f64),
    deflection: f64,
) -> (f64, f64) {
    let n = 8;
    let (mut suu, mut svv) = (0.0f64, 0.0f64);
    let (mut su_min, mut sv_min) = (f64::INFINITY, f64::INFINITY);
    for i in 0..=n {
        for j in 0..=n {
            let u = wrap(
                ulo + (uhi - ulo) * f64::from(i) / f64::from(n),
                surface.domain(0),
                surface.data.closed[0],
            );
            let v = wrap(
                vlo + (vhi - vlo) * f64::from(j) / f64::from(n),
                surface.domain(1),
                surface.data.closed[1],
            );
            let d = surface.derivatives(u, v, 2);
            suu = suu.max(d[2][0].length());
            svv = svv.max(d[0][2].length());
            su_min = su_min.min(d[1][0].length());
            sv_min = sv_min.min(d[0][1].length());
        }
    }
    let d = deflection.max(1e-6);
    // Within the deflection, and turning no more than a twelfth of a turn a step, as an
    // arc's chords do.
    let step = |second: f64, speed: f64, range: f64| {
        let s = if second > 1e-300 {
            (8.0 * d / second)
                .sqrt()
                .min(std::f64::consts::TAU / 12.0 * speed / second)
        } else {
            range
        };
        s.clamp(range / 256.0, range.max(1e-300) / 2.0)
    };
    (step(suu, su_min, uhi - ulo), step(svv, sv_min, vhi - vlo))
}
