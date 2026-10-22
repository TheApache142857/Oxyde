//! Triangles of a planar polygon with holes, for display.
//!
//! Each hole is joined to the outer boundary by a bridge, two coincident edges from the
//! hole's rightmost corner to a corner of the boundary it can see, which makes one polygon
//! that runs round the boundary and every hole; ears (corners whose triangle holds no other
//! corner) are then cut off one at a time. D. Eberly, *Triangulation by Ear Clipping* (2002).

// Indices into the ring being clipped and into the points it refers to, all made here
// from the face's own loops.
#![allow(clippy::indexing_slicing)]

use oxyde_geom::Point2;

use crate::brep::cyclic_pairs;

/// Twice the signed area of the triangle `o`, `a`, `b`: positive when it turns
/// counter-clockwise.
fn cross(o: Point2, a: Point2, b: Point2) -> f64 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

fn signed_area(points: &[Point2], ring: &[usize]) -> f64 {
    cyclic_pairs(ring)
        .map(|(i, j)| {
            let (a, b) = (points[i], points[j]);
            a.x * b.y - b.x * a.y
        })
        .sum::<f64>()
        / 2.0
}

/// Whether `p` is inside the triangle `a`, `b`, `c` or on its boundary, whichever way the
/// triangle turns. `eps` is the tolerance on twice an area.
fn in_triangle(a: Point2, b: Point2, c: Point2, p: Point2, eps: f64) -> bool {
    let sides = [cross(a, b, p), cross(b, c, p), cross(c, a, p)];
    let negative = sides.iter().any(|&s| s < -eps);
    let positive = sides.iter().any(|&s| s > eps);
    !(negative && positive)
}

/// Triangles covering the polygon whose outer boundary is `loops[0]` and whose holes are
/// the other loops (indices into `points`), counter-clockwise. The boundary may run either
/// way; holes are turned to run against it.
pub(crate) fn polygon_with_holes(
    points: &[Point2],
    loops: &[Vec<usize>],
) -> Result<Vec<[usize; 3]>, String> {
    let Some((outer, holes)) = loops.split_first() else {
        return Ok(Vec::new());
    };
    // Areas are compared to the size of the polygon, so that the tolerance means the same
    // for a washer and for a floor plate.
    let (mut lo, mut hi) = (
        Point2::new(f64::MAX, f64::MAX),
        Point2::new(f64::MIN, f64::MIN),
    );
    for &i in loops.iter().flatten() {
        let p = points[i];
        lo = Point2::new(lo.x.min(p.x), lo.y.min(p.y));
        hi = Point2::new(hi.x.max(p.x), hi.y.max(p.y));
    }
    let size = (hi.x - lo.x).max(hi.y - lo.y).max(f64::MIN_POSITIVE);
    let eps = 1e-12 * size * size;

    let mut ring = outer.clone();
    if signed_area(points, &ring) < 0.0 {
        ring.reverse();
    }
    let mut holes: Vec<Vec<usize>> = holes
        .iter()
        .filter(|h| !h.is_empty())
        .map(|h| {
            let mut h = h.clone();
            if signed_area(points, &h) > 0.0 {
                h.reverse();
            }
            h
        })
        .collect();
    // The hole reaching furthest right first: its bridge then crosses no other hole. The
    // sort is stable, so holes reaching as far keep their order.
    let reach = |h: &Vec<usize>| {
        h.iter()
            .map(|&i| points[i].x)
            .fold(f64::NEG_INFINITY, f64::max)
    };
    holes.sort_by(|a, b| reach(b).total_cmp(&reach(a)));
    for hole in &holes {
        bridge(points, &mut ring, hole, eps)?;
    }
    Ok(clip_ears(points, ring, eps))
}

/// Joins `hole` (clockwise) to `ring` (counter-clockwise) through the hole's rightmost
/// corner and a corner of the ring it can see.
fn bridge(
    points: &[Point2],
    ring: &mut Vec<usize>,
    hole: &[usize],
    eps: f64,
) -> Result<(), String> {
    // M: the hole's rightmost corner (the lowest of those, on a tie).
    let mut m_at = 0;
    for (k, &v) in hole.iter().enumerate() {
        let (p, q) = (points[v], points[hole[m_at]]);
        if p.x > q.x || (p.x == q.x && p.y < q.y) {
            m_at = k;
        }
    }
    let m = points[hole[m_at]];

    // The nearest edge of the ring that a ray from M to the right meets. Horizontal edges
    // are left out: where the ray runs along one, it meets the edges at its ends too.
    let n = ring.len();
    let mut nearest: Option<(f64, usize)> = None;
    for (k, (ia, ib)) in cyclic_pairs(ring.as_slice()).enumerate() {
        let (a, b) = (points[ia], points[ib]);
        if a.y == b.y || (a.y > m.y && b.y > m.y) || (a.y < m.y && b.y < m.y) {
            continue;
        }
        let x = a.x + (m.y - a.y) * (b.x - a.x) / (b.y - a.y);
        if x >= m.x && nearest.is_none_or(|(best, _)| x < best) {
            nearest = Some((x, k));
        }
    }
    let Some((x, k)) = nearest else {
        return Err("a hole of a face is not inside its outer boundary".to_owned());
    };
    let (ka, kb) = (k, (k + 1) % n);
    let (a, b) = (points[ring[ka]], points[ring[kb]]);

    // Where the ray meets a corner, that corner is visible. Otherwise the end of the edge
    // furthest right is, unless corners of the ring that turn inwards stand in the
    // triangle between M, the point met and that end: then the one at the smallest angle
    // to the ray is (the nearest of those, on a tie).
    let mut at = if a.y == m.y {
        ka
    } else if b.y == m.y {
        kb
    } else if a.x > b.x {
        ka
    } else {
        kb
    };
    if a.y != m.y && b.y != m.y {
        let met = Point2::new(x, m.y);
        let p = points[ring[at]];
        let mut best: Option<(f64, f64, usize)> = None;
        for (j, &corner_index) in ring.iter().enumerate() {
            if corner_index == ring[at] {
                continue;
            }
            let v = points[corner_index];
            let (prev, next) = (points[ring[(j + n - 1) % n]], points[ring[(j + 1) % n]]);
            if cross(prev, v, next) >= 0.0 || !in_triangle(m, met, p, v, eps) {
                continue;
            }
            let (dx, dy) = (v.x - m.x, v.y - m.y);
            if dx <= 0.0 {
                continue;
            }
            let slope = dy.abs() / dx;
            let distance = dx.hypot(dy);
            let better = best.is_none_or(|(s, d, _)| slope < s || (slope == s && distance < d));
            if better {
                best = Some((slope, distance, j));
            }
        }
        if let Some((_, _, j)) = best {
            at = j;
        }
    }

    // ..., P, M, the hole round to M again, M, P, ...
    let mut joined = Vec::with_capacity(n + hole.len() + 2);
    joined.extend_from_slice(&ring[..=at]);
    joined.extend((0..hole.len()).map(|t| hole[(m_at + t) % hole.len()]));
    joined.push(hole[m_at]);
    joined.push(ring[at]);
    joined.extend_from_slice(&ring[at + 1..]);
    *ring = joined;
    Ok(())
}

/// Twice the signed area of the corner of `ring` at position `i` with its neighbours.
fn corner(points: &[Point2], ring: &[usize], i: usize) -> f64 {
    let n = ring.len();
    cross(
        points[ring[(i + n - 1) % n]],
        points[ring[i]],
        points[ring[(i + 1) % n]],
    )
}

/// Whether the corner of `ring` at position `i` is an ear: it turns counter-clockwise and
/// no other corner lies in its triangle. Corners at the same point (the two ends of a
/// bridge) are the same index and do not count.
fn is_ear(points: &[Point2], ring: &[usize], i: usize, eps: f64) -> bool {
    let n = ring.len();
    let (ia, ib, ic) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
    let (a, b, c) = (points[ia], points[ib], points[ic]);
    cross(a, b, c) > eps
        && ring
            .iter()
            .all(|&j| j == ia || j == ib || j == ic || !in_triangle(a, b, c, points[j], eps))
}

fn clip_ears(points: &[Point2], mut ring: Vec<usize>, eps: f64) -> Vec<[usize; 3]> {
    let mut triangles = Vec::with_capacity(ring.len().saturating_sub(2));
    while ring.len() > 3 {
        let n = ring.len();
        let triangle = |i: usize| [ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]];
        if let Some(i) = (0..n).find(|&i| is_ear(points, &ring, i, eps)) {
            triangles.push(triangle(i));
            ring.remove(i);
            continue;
        }
        // No ear: only from rounding, or a face that is not simple. A corner in line with
        // its neighbours goes without a triangle (it covers nothing); failing that, the
        // most convex corner is cut anyway, so that the face still shows.
        if let Some(i) = (0..n).find(|&i| corner(points, &ring, i).abs() <= eps) {
            ring.remove(i);
            continue;
        }
        let i = (0..n)
            .max_by(|&a, &b| corner(points, &ring, a).total_cmp(&corner(points, &ring, b)))
            .unwrap_or(0);
        if corner(points, &ring, i) > eps {
            triangles.push(triangle(i));
        }
        ring.remove(i);
    }
    if ring.len() == 3 && corner(points, &ring, 1) > eps {
        triangles.push([ring[0], ring[1], ring[2]]);
    }
    triangles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(points: &[Point2], triangles: &[[usize; 3]]) -> f64 {
        triangles
            .iter()
            .map(|t| {
                let a = cross(points[t[0]], points[t[1]], points[t[2]]) / 2.0;
                assert!(a > 0.0, "a triangle turns clockwise: {t:?}");
                a
            })
            .sum()
    }

    fn p(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    #[test]
    fn a_concave_polygon_is_covered_exactly() {
        // An L, given clockwise.
        let points = [
            p(0.0, 0.0),
            p(0.0, 3.0),
            p(1.0, 3.0),
            p(1.0, 1.0),
            p(4.0, 1.0),
            p(4.0, 0.0),
        ];
        let t = polygon_with_holes(&points, &[vec![0, 1, 2, 3, 4, 5]]).unwrap();
        assert_eq!(t.len(), 4);
        assert!((area(&points, &t) - 6.0).abs() < 1e-12);
    }

    #[test]
    fn holes_are_left_out() {
        // A 10 × 6 plate with two square holes side by side, one at an odd angle, and a
        // corner in line with its neighbours on the bottom edge.
        let points = [
            p(0.0, 0.0),
            p(5.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 6.0),
            p(0.0, 6.0),
            p(1.0, 1.0),
            p(3.0, 1.0),
            p(3.0, 3.0),
            p(1.0, 3.0),
            p(7.0, 2.0),
            p(8.0, 3.0),
            p(7.0, 4.0),
            p(6.0, 3.0),
        ];
        let loops = vec![vec![0, 1, 2, 3, 4], vec![5, 6, 7, 8], vec![9, 10, 11, 12]];
        let t = polygon_with_holes(&points, &loops).unwrap();
        assert!((area(&points, &t) - (60.0 - 4.0 - 2.0)).abs() < 1e-9);
        // At most n - 2 + 2h triangles: the corner in line may go without one of its own.
        assert!(t.len() <= 13 - 2 + 2 * 2, "{} triangles", t.len());
    }

    #[test]
    fn a_hole_outside_the_face_is_refused() {
        let points = [
            p(0.0, 0.0),
            p(1.0, 0.0),
            p(1.0, 1.0),
            p(0.0, 1.0),
            p(5.0, 5.0),
            p(6.0, 5.0),
            p(6.0, 6.0),
        ];
        assert!(polygon_with_holes(&points, &[vec![0, 1, 2, 3], vec![4, 5, 6]]).is_err());
    }
}
