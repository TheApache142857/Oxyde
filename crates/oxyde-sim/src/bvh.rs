//! A bounding volume hierarchy over triangles in double precision, for the mesher: where a
//! segment crosses the surface, and which triangle is nearest a point.
//!
//! Built top-down by splitting at the median centre along the longest axis, so the build
//! is deterministic and needs no tuning.

// Indices into the hierarchy's own arrays, made by its own build.
#![allow(clippy::indexing_slicing)]

use crate::math::{V3, cross, dot, sub};

const LEAF: usize = 4;

#[derive(Debug, Clone, Copy)]
struct Node {
    lo: V3,
    hi: V3,
    /// Leaf: the first triangle in `order`; inner node: the right child (the left one
    /// follows the node).
    start: u32,
    /// Leaf: how many triangles; inner node: 0.
    count: u32,
}

/// Where a segment crosses a triangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Hit {
    /// Along the segment, 0 at its start and 1 at its end.
    pub t: f64,
    pub triangle: u32,
    /// The triangle's normal points the way the segment goes.
    pub leaving: bool,
}

pub(crate) struct Bvh {
    nodes: Vec<Node>,
    triangles: Vec<[V3; 3]>,
    /// Triangle indices in leaf order.
    order: Vec<u32>,
}

impl Bvh {
    pub fn new(triangles: Vec<[V3; 3]>) -> Bvh {
        let mut order: Vec<u32> = (0..triangles.len() as u32).collect();
        let centres: Vec<V3> = triangles
            .iter()
            .map(|t| {
                [
                    (t[0][0] + t[1][0] + t[2][0]) / 3.0,
                    (t[0][1] + t[1][1] + t[2][1]) / 3.0,
                    (t[0][2] + t[1][2] + t[2][2]) / 3.0,
                ]
            })
            .collect();
        let mut nodes = Vec::with_capacity(2 * triangles.len() / LEAF + 1);
        if !triangles.is_empty() {
            build(&mut nodes, &mut order, 0, &triangles, &centres);
        }
        Bvh {
            nodes,
            triangles,
            order,
        }
    }

    pub fn triangle(&self, i: u32) -> Option<&[V3; 3]> {
        self.triangles.get(i as usize)
    }

    /// Every crossing of the segment from `p` to `q` with a triangle, in no order. Hits
    /// on a triangle's edge are kept (with a little slack), so a segment through an edge
    /// shared by two triangles hits both; callers merge such pairs.
    pub fn segment_hits(&self, p: V3, q: V3, out: &mut Vec<Hit>) {
        out.clear();
        if self.nodes.is_empty() {
            return;
        }
        let d = sub(q, p);
        let inv = d.map(|c| if c == 0.0 { f64::INFINITY } else { 1.0 / c });
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let node = self.nodes[i];
            if !segment_meets_box(p, inv, &node) {
                continue;
            }
            if node.count == 0 {
                stack.push(i + 1);
                stack.push(node.start as usize);
                continue;
            }
            for &tri in &self.order[node.start as usize..(node.start + node.count) as usize] {
                if let Some(hit) = intersect(p, d, &self.triangles[tri as usize], tri) {
                    out.push(hit);
                }
            }
        }
    }

    /// The triangle nearest `p` and the squared distance to it.
    pub fn nearest(&self, p: V3) -> Option<(u32, f64)> {
        if self.nodes.is_empty() {
            return None;
        }
        let mut best: Option<(u32, f64)> = None;
        let mut stack = vec![(0usize, box_distance2(p, &self.nodes[0]))];
        while let Some((i, d2)) = stack.pop() {
            if best.is_some_and(|(_, b)| d2 >= b) {
                continue;
            }
            let node = self.nodes[i];
            if node.count == 0 {
                let (l, r) = (i + 1, node.start as usize);
                let (dl, dr) = (
                    box_distance2(p, &self.nodes[l]),
                    box_distance2(p, &self.nodes[r]),
                );
                // The nearer box last, so it is looked at first.
                if dl < dr {
                    stack.push((r, dr));
                    stack.push((l, dl));
                } else {
                    stack.push((l, dl));
                    stack.push((r, dr));
                }
                continue;
            }
            for &tri in &self.order[node.start as usize..(node.start + node.count) as usize] {
                let d = point_triangle_distance2(p, &self.triangles[tri as usize]);
                if best.is_none_or(|(_, b)| d < b) {
                    best = Some((tri, d));
                }
            }
        }
        best
    }
}

fn build(nodes: &mut Vec<Node>, order: &mut [u32], start: usize, tris: &[[V3; 3]], c: &[V3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for &t in order.iter() {
        for p in &tris[t as usize] {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    let here = nodes.len();
    nodes.push(Node {
        lo,
        hi,
        start: start as u32,
        count: order.len() as u32,
    });
    if order.len() <= LEAF {
        return;
    }
    let mut clo = [f64::INFINITY; 3];
    let mut chi = [f64::NEG_INFINITY; 3];
    for &t in order.iter() {
        for k in 0..3 {
            clo[k] = clo[k].min(c[t as usize][k]);
            chi[k] = chi[k].max(c[t as usize][k]);
        }
    }
    let axis = (0..3)
        .max_by(|&a, &b| (chi[a] - clo[a]).total_cmp(&(chi[b] - clo[b])))
        .unwrap_or(0);
    let mid = order.len() / 2;
    order.select_nth_unstable_by(mid, |&a, &b| {
        c[a as usize][axis]
            .total_cmp(&c[b as usize][axis])
            .then(a.cmp(&b))
    });
    let (left, right) = order.split_at_mut(mid);
    build(nodes, left, start, tris, c);
    let right_index = nodes.len();
    build(nodes, right, start + mid, tris, c);
    nodes[here].start = right_index as u32;
    nodes[here].count = 0;
}

fn segment_meets_box(p: V3, inv: V3, node: &Node) -> bool {
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for k in 0..3 {
        let slack = 1e-9 * (node.hi[k] - node.lo[k]).abs().max(1e-9);
        let (lo, hi) = (node.lo[k] - slack, node.hi[k] + slack);
        if inv[k].is_infinite() {
            if p[k] < lo || p[k] > hi {
                return false;
            }
            continue;
        }
        let (a, b) = ((lo - p[k]) * inv[k], (hi - p[k]) * inv[k]);
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        t0 = t0.max(a);
        t1 = t1.min(b);
        if t0 > t1 {
            return false;
        }
    }
    true
}

/// Möller and Trumbore's test, with a little slack at the edges.
fn intersect(p: V3, d: V3, tri: &[V3; 3], index: u32) -> Option<Hit> {
    const SLACK: f64 = 1e-10;
    let e1 = sub(tri[1], tri[0]);
    let e2 = sub(tri[2], tri[0]);
    let h = cross(d, e2);
    let det = dot(e1, h);
    let scale = dot(e1, e1).sqrt() * dot(e2, e2).sqrt() * dot(d, d).sqrt();
    if det.abs() <= 1e-14 * scale || scale == 0.0 {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(p, tri[0]);
    let u = dot(s, h) * inv;
    if !(-SLACK..=1.0 + SLACK).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(d, q) * inv;
    if v < -SLACK || u + v > 1.0 + SLACK {
        return None;
    }
    let t = dot(e2, q) * inv;
    if !(0.0..=1.0).contains(&t) {
        return None;
    }
    Some(Hit {
        t,
        triangle: index,
        // The normal e1 × e2 points along d exactly when det < 0 (h = d × e2).
        leaving: det < 0.0,
    })
}

fn box_distance2(p: V3, node: &Node) -> f64 {
    (0..3)
        .map(|k| {
            let d = (node.lo[k] - p[k]).max(p[k] - node.hi[k]).max(0.0);
            d * d
        })
        .sum()
}

/// The squared distance from `p` to the triangle (Ericson, *Real-Time Collision
/// Detection*, 5.1.5).
pub(crate) fn point_triangle_distance2(p: V3, t: &[V3; 3]) -> f64 {
    let closest = closest_point(p, t);
    let d = sub(p, closest);
    dot(d, d)
}

pub(crate) fn closest_point(p: V3, t: &[V3; 3]) -> V3 {
    let [a, b, c] = *t;
    let ab = sub(b, a);
    let ac = sub(c, a);
    let ap = sub(p, a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = sub(p, b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return crate::math::lerp(a, b, v);
    }
    let cp = sub(p, c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return crate::math::lerp(a, c, w);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return crate::math::lerp(b, c, w);
    }
    let denom = va + vb + vc;
    if denom.abs() < 1e-300 {
        return a;
    }
    let v = vb / denom;
    let w = vc / denom;
    crate::math::add(
        a,
        crate::math::add(crate::math::scale(ab, v), crate::math::scale(ac, w)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(n: usize) -> Vec<[V3; 3]> {
        let mut out = Vec::new();
        for i in 0..n {
            for j in 0..n {
                let (x, y) = (i as f64, j as f64);
                out.push([[x, y, 0.0], [x + 1.0, y, 0.0], [x + 1.0, y + 1.0, 0.0]]);
                out.push([[x, y, 0.0], [x + 1.0, y + 1.0, 0.0], [x, y + 1.0, 0.0]]);
            }
        }
        out
    }

    #[test]
    fn segments_and_nearest_triangles() {
        let bvh = Bvh::new(grid(10));
        let mut hits = Vec::new();
        bvh.segment_hits([3.3, 4.6, -1.0], [3.3, 4.6, 3.0], &mut hits);
        assert_eq!(hits.len(), 1);
        assert!((hits[0].t - 0.25).abs() < 1e-12);
        // Normals are +Z, the segment goes up: it leaves.
        assert!(hits[0].leaving);
        bvh.segment_hits([3.3, 4.6, 3.0], [3.3, 4.6, 2.0], &mut hits);
        assert!(hits.is_empty());
        // Through a shared edge: both triangles.
        bvh.segment_hits([3.5, 3.5, 1.0], [3.5, 3.5, -1.0], &mut hits);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| !h.leaving));
        let (tri, d2) = bvh.nearest([2.2, 7.9, 0.5]).unwrap();
        assert!((d2 - 0.25).abs() < 1e-12);
        let t = bvh.triangle(tri).unwrap();
        assert!(
            t.iter()
                .all(|p| (p[0] - 2.5).abs() <= 0.5 && (p[1] - 7.5).abs() <= 0.5)
        );
        let (_, d2) = bvh.nearest([-1.0, -1.0, 0.0]).unwrap();
        assert!((d2 - 2.0).abs() < 1e-12);
    }
}
