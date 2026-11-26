//! An outcome's surface with fewer triangles, for the kernel: *Create Part* turns it into a
//! solid of one face per triangle, which a modelling kernel handles well up to some
//! thousands of faces, where the surface has hundreds of thousands of triangles.
//!
//! Quadric edge collapse (Garland and Heckbert, 1997): each point carries the sum of the
//! squared distances to the planes of the triangles around it (a quadric), and the edge
//! whose two ends can be merged into one point that strays least from those planes is
//! collapsed first, its point placed where the merged quadric is least, until the surface
//! has the triangles asked for. A collapse is skipped when it would make the surface other
//! than a closed manifold (the two ends sharing more neighbours than the edge's two
//! triangles give them, an end of three triangles only), turn a triangle over or leave one
//! so thin the kernel would skip it. Ties go to the lower indices, so the result is the same
//! on every run.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::math::{V3, add, cross, dot, norm, scale, sub};
use crate::shape::Shape;

/// A collapse turns no triangle further than this cosine from where it faced.
const MIN_TURN_COSINE: f64 = 0.2;
/// Added to a collapse's cost per mm² of its edge's length squared: among the collapses
/// that cost nothing (on a flat face, every one), the shortest edges go first, spreading
/// them over the face. Taken by the lowest numbers instead, they piled the face's points
/// onto a few, whose fans of thin triangles refused collapse after collapse: a plate's
/// outcome (70 000 triangles to 6 000) took 10 s, under half a second with this.
const LENGTH_COST: f64 = 1e-9;
/// Nor leaves one thinner than this ([`thickness`]). A kernel making a solid of the
/// triangles skips the flattest as degenerate (OpenCASCADE's when two edges are parallel
/// to a sine of 1e-7), which leaves a hole in the shell; this keeps every angle's sine over
/// a thousandth, far from that.
const MIN_THICKNESS: f64 = 1e-3;

/// How far a triangle is from flat: twice its area over its longest edge squared, which
/// no angle's sine is under (an equilateral triangle's is 0.87, a flat one's 0).
fn thickness(p: [V3; 3]) -> f64 {
    let n = norm(cross(sub(p[1], p[0]), sub(p[2], p[0])));
    let longest = [(p[0], p[1]), (p[1], p[2]), (p[2], p[0])]
        .iter()
        .map(|&(from, to)| {
            let e = sub(to, from);
            dot(e, e)
        })
        .fold(0.0, f64::max);
    if longest > 0.0 { n / longest } else { 0.0 }
}

/// `shape` with at most `target` triangles (as few as collapses can make when they cannot
/// reach it), closed and manifold as it was. Each triangle keeps its label.
pub fn decimate(shape: &Shape, target: usize) -> Shape {
    decimate_holding(shape, &[], target, f64::INFINITY)
}

/// [`decimate`], the points `held` (per point of `shape`; none past its end) never moving:
/// a collapse onto one leaves the merged point where it is, so points on an exact body's
/// face ([`super::exact_surface`]) stay on it and a flat face stays flat. And no collapse
/// moves the surface further than `tolerance` (mm: the merged point's distance from the
/// planes of the triangles it stands for, their root mean square weighted by area): when
/// the collapses within it cannot reach `target`, it stops short of it rather than wear
/// the shape away (gone through to the last collapse, the servo arm's screw hole ended a
/// skewed pentagon, two of its points merged into the counterbore's floor).
pub fn decimate_holding(shape: &Shape, held: &[bool], target: usize, tolerance: f64) -> Shape {
    let mut m = Mesh::new(shape);
    for (p, &given) in m.points.iter_mut().zip(held) {
        p.held = given;
    }
    m.tolerance = tolerance;
    if m.live_faces <= target {
        return shape.clone();
    }
    let mut heap = BinaryHeap::new();
    for (a, b) in m.edges() {
        if let Some(e) = m.candidate(a, b) {
            heap.push(e);
        }
    }
    while m.live_faces > target {
        let Some(e) = heap.pop() else {
            break;
        };
        let (a, b) = (e.a as usize, e.b as usize);
        if !m.unchanged(a, e.stamp_a) || !m.unchanged(b, e.stamp_b) {
            continue;
        }
        if !m.collapse(a, b, e.at) {
            continue;
        }
        for n in m.neighbours(a) {
            if let Some(e) = m.candidate(a.min(n), a.max(n)) {
                heap.push(e);
            }
        }
    }
    m.shape()
}

/// `shape`'s closed pieces, each with points of its own, the largest first: what the
/// kernel makes a solid of, one each. Pieces under a thousandth of the largest's volume
/// (specks the surface leaves) are left out.
pub fn solid_pieces(shape: &Shape) -> Vec<Shape> {
    let n = shape.positions.len();
    // Points joined by triangles, by union-find.
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut v: usize) -> usize {
        while let Some(&up) = parent.get(v)
            && up != v
        {
            let grand = parent.get(up).copied().unwrap_or(up);
            if let Some(p) = parent.get_mut(v) {
                *p = grand;
            }
            v = grand;
        }
        v
    }
    let valid = |t: &[u32; 3]| t.iter().all(|&i| (i as usize) < n);
    for t in shape.triangles.iter().filter(|t| valid(t)) {
        let [first, second, third] = t.map(|i| i as usize);
        for other in [second, third] {
            let (a, b) = (root(&mut parent, first), root(&mut parent, other));
            if a != b
                && let Some(p) = parent.get_mut(a.max(b))
            {
                *p = a.min(b);
            }
        }
    }
    let mut pieces: Vec<Shape> = Vec::new();
    let mut piece_of = vec![usize::MAX; n];
    let mut slot = vec![usize::MAX; n];
    for (f, t) in shape.triangles.iter().enumerate() {
        if !valid(t) {
            continue;
        }
        let r = root(&mut parent, t[0] as usize);
        let Some(piece) = piece_of.get_mut(r) else {
            continue;
        };
        if *piece == usize::MAX {
            *piece = pieces.len();
            pieces.push(Shape::default());
        }
        let Some(out) = pieces.get_mut(*piece) else {
            continue;
        };
        let ids = t.map(|i| {
            let i = i as usize;
            match (slot.get_mut(i), shape.positions.get(i)) {
                (Some(s), Some(&p)) => {
                    if *s == usize::MAX {
                        *s = out.positions.len();
                        out.positions.push(p);
                    }
                    *s as u32
                }
                _ => 0,
            }
        });
        out.triangles.push(ids);
        out.labels
            .push(shape.labels.get(f).copied().unwrap_or(crate::NO_LABEL));
    }
    let mut out: Vec<(f64, Shape)> = pieces
        .into_iter()
        .map(|s| (s.volume(), s))
        .filter(|(v, s)| *v > 0.0 && s.triangles.len() >= 4)
        .collect();
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    let largest = out.first().map_or(0.0, |(v, _)| *v);
    out.into_iter()
        .filter(|(v, _)| *v >= 1e-3 * largest)
        .map(|(_, s)| s)
        .collect()
}

/// A collapse waiting in the queue: the cost, the edge and where its point goes, valid while
/// neither end has changed since (their stamps).
struct Candidate {
    cost: f64,
    a: u32,
    b: u32,
    stamp_a: u32,
    stamp_b: u32,
    at: V3,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    /// The least cost first out of the (max-)heap, then the lowest indices.
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .total_cmp(&self.cost)
            .then_with(|| other.a.cmp(&self.a))
            .then_with(|| other.b.cmp(&self.b))
    }
}

/// A symmetric 4 × 4 quadric, `[a², ab, ac, ad, b², bc, bd, c², cd, d²]` of the planes
/// `ax + by + cz + d = 0` it sums.
type Quadric = [f64; 10];

fn plane_quadric(n: V3, d: f64, weight: f64) -> Quadric {
    let [a, b, c] = n;
    [
        a * a,
        a * b,
        a * c,
        a * d,
        b * b,
        b * c,
        b * d,
        c * c,
        c * d,
        d * d,
    ]
    .map(|v| v * weight)
}

fn quadric_sum(p: &Quadric, q: &Quadric) -> Quadric {
    let mut sum = *p;
    for (s, x) in sum.iter_mut().zip(q) {
        *s += x;
    }
    sum
}

/// The quadric's value at `v`: the weighted sum of squared distances to its planes.
fn quadric_error(q: &Quadric, v: V3) -> f64 {
    let [x, y, z] = v;
    q[0] * x * x
        + 2.0 * q[1] * x * y
        + 2.0 * q[2] * x * z
        + 2.0 * q[3] * x
        + q[4] * y * y
        + 2.0 * q[5] * y * z
        + 2.0 * q[6] * y
        + q[7] * z * z
        + 2.0 * q[8] * z
        + q[9]
}

/// Where the quadric is least, when that is a point (its 3 × 3 part well conditioned).
fn quadric_minimum(q: &Quadric) -> Option<V3> {
    let m = [[q[0], q[1], q[2]], [q[1], q[4], q[5]], [q[2], q[5], q[7]]];
    let r = [-q[3], -q[6], -q[8]];
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let scale_of = (q[0] + q[4] + q[7]).abs().max(f64::MIN_POSITIVE);
    if det.abs() <= 1e-9 * scale_of * scale_of * scale_of {
        return None;
    }
    // Cramer's rule.
    let column = |c: usize| -> f64 {
        let mut a = m;
        for (row, &value) in a.iter_mut().zip(&r) {
            if let Some(x) = row.get_mut(c) {
                *x = value;
            }
        }
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    };
    let v = [column(0) / det, column(1) / det, column(2) / det];
    v.iter().all(|x| x.is_finite()).then_some(v)
}

/// A point of the mesh.
struct Point {
    position: V3,
    quadric: Quadric,
    /// The live faces around it.
    around: Vec<u32>,
    /// Bumped whenever it moves or its faces change.
    stamp: u32,
    dead: bool,
    /// It stays where it is.
    held: bool,
    /// The areas its quadric is weighted by, summed.
    weight: f64,
}

/// A triangle of the mesh.
struct Face {
    corners: [u32; 3],
    label: u32,
    live: bool,
}

struct Mesh {
    points: Vec<Point>,
    faces: Vec<Face>,
    live_faces: usize,
    /// mm: the furthest a collapse may move the surface.
    tolerance: f64,
}

impl Mesh {
    fn new(shape: &Shape) -> Self {
        let mut points: Vec<Point> = shape
            .positions
            .iter()
            .map(|&position| Point {
                position,
                quadric: [0.0; 10],
                around: Vec::new(),
                stamp: 0,
                dead: false,
                held: false,
                weight: 0.0,
            })
            .collect();
        let mut faces = Vec::with_capacity(shape.triangles.len());
        let given = shape
            .labels
            .iter()
            .copied()
            .chain(std::iter::repeat(crate::NO_LABEL));
        for (t, label) in shape.triangles.iter().zip(given) {
            // Triangles off the points, or with a point twice, are no surface.
            let [Some(p), Some(q), Some(r)] = t.map(|i| shape.positions.get(i as usize).copied())
            else {
                continue;
            };
            if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
                continue;
            }
            faces.push(Face {
                corners: *t,
                label,
                live: true,
            });
            let id = (faces.len() - 1) as u32;
            let normal = cross(sub(q, p), sub(r, p));
            let area2 = norm(normal);
            let plane = (area2 > 0.0).then(|| {
                let unit = scale(normal, 1.0 / area2);
                plane_quadric(unit, -dot(unit, p), 0.5 * area2)
            });
            for &i in t {
                let Some(point) = points.get_mut(i as usize) else {
                    continue;
                };
                point.around.push(id);
                if let Some(plane) = &plane {
                    point.quadric = quadric_sum(&point.quadric, plane);
                    point.weight += 0.5 * area2;
                }
            }
        }
        Mesh {
            points,
            live_faces: faces.len(),
            faces,
            tolerance: f64::INFINITY,
        }
    }

    /// Whether point `v` is live and has not changed since its stamp was `stamp`.
    fn unchanged(&self, v: usize, stamp: u32) -> bool {
        self.points
            .get(v)
            .is_some_and(|p| !p.dead && p.stamp == stamp)
    }

    fn corners(&self, f: u32) -> Option<[u32; 3]> {
        self.faces.get(f as usize).map(|face| face.corners)
    }

    fn position(&self, i: u32) -> Option<V3> {
        self.points.get(i as usize).map(|p| p.position)
    }

    /// Every edge once, its ends in order.
    fn edges(&self) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = self
            .faces
            .iter()
            .flat_map(|face| {
                let t = face.corners;
                [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])]
                    .map(|(a, b)| (a.min(b) as usize, a.max(b) as usize))
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The points joined to `v` by an edge, in order.
    fn neighbours(&self, v: usize) -> Vec<usize> {
        let Some(point) = self.points.get(v) else {
            return Vec::new();
        };
        let mut out: Vec<usize> = point
            .around
            .iter()
            .filter_map(|&f| self.corners(f))
            .flatten()
            .map(|i| i as usize)
            .filter(|&i| i != v)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The collapse of edge `a b` (`a < b`), costed, its point where the merged quadric is
    /// least (else the better of the ends and the middle), or at a held end; none when it
    /// would move the surface further than the tolerance.
    fn candidate(&self, a: usize, b: usize) -> Option<Candidate> {
        let (first, second) = (self.points.get(a)?, self.points.get(b)?);
        if a == b || first.dead || second.dead {
            return None;
        }
        let q = quadric_sum(&first.quadric, &second.quadric);
        let (pa, pb) = (first.position, second.position);
        let middle = scale(add(pa, pb), 0.5);
        let length = norm(sub(pb, pa));
        // A minimum far off the edge (a nearly flat neighbourhood) is no better a place.
        let optimum = quadric_minimum(&q).filter(|v| norm(sub(*v, middle)) <= length);
        let better = |x: &V3, y: &V3| quadric_error(&q, *x).total_cmp(&quadric_error(&q, *y));
        let at = match (first.held, second.held, optimum) {
            (true, true, _) => [pa, pb].into_iter().min_by(better).unwrap_or(pa),
            (true, false, _) => pa,
            (false, true, _) => pb,
            (false, false, Some(v)) => v,
            (false, false, None) => [pa, pb, middle]
                .into_iter()
                .min_by(better)
                .unwrap_or(middle),
        };
        let error = quadric_error(&q, at).max(0.0);
        if error > self.tolerance * self.tolerance * (first.weight + second.weight) {
            return None;
        }
        Some(Candidate {
            cost: error + LENGTH_COST * length * length,
            a: a as u32,
            b: b as u32,
            stamp_a: first.stamp,
            stamp_b: second.stamp,
            at,
        })
    }

    /// Merges `b` into `a` at `at`, when that keeps the surface a closed manifold and turns
    /// no triangle over; whether it did.
    fn collapse(&mut self, a: usize, b: usize, at: V3) -> bool {
        let (Some(first), Some(second)) = (self.points.get(a), self.points.get(b)) else {
            return false;
        };
        let (quadric_b, held_b, weight_b) = (second.quadric, second.held, second.weight);
        // The edge's own two triangles, and the points across them.
        let shared: Vec<u32> = first
            .around
            .iter()
            .copied()
            .filter(|&f| self.corners(f).is_some_and(|t| t.contains(&(b as u32))))
            .collect();
        if shared.len() != 2 {
            return false;
        }
        let mut across: Vec<usize> = shared
            .iter()
            .filter_map(|&f| self.corners(f))
            .flatten()
            .map(|i| i as usize)
            .filter(|&i| i != a && i != b)
            .collect();
        across.sort_unstable();
        across.dedup();
        let (na, nb) = (self.neighbours(a), self.neighbours(b));
        let common: Vec<usize> = na.iter().copied().filter(|v| nb.contains(v)).collect();
        if common != across || na.len() <= 3 || nb.len() <= 3 {
            return false;
        }
        // No triangle turned over, flattened or left a sliver (but one that was thinner
        // still, which it may stay).
        for point in [first, second] {
            for &f in &point.around {
                if shared.contains(&f) {
                    continue;
                }
                let Some(t) = self.corners(f) else {
                    return false;
                };
                let [Some(p0), Some(p1), Some(p2)] = t.map(|i| self.position(i)) else {
                    return false;
                };
                let before = [p0, p1, p2];
                let mut after = before;
                for (x, &i) in after.iter_mut().zip(&t) {
                    if i as usize == a || i as usize == b {
                        *x = at;
                    }
                }
                let n0 = cross(sub(before[1], before[0]), sub(before[2], before[0]));
                let n1 = cross(sub(after[1], after[0]), sub(after[2], after[0]));
                let (l0, l1) = (norm(n0), norm(n1));
                if l1 <= 1e-12 * l0.max(f64::MIN_POSITIVE)
                    || dot(n0, n1) < MIN_TURN_COSINE * l0 * l1
                    || thickness(after) < MIN_THICKNESS.min(thickness(before))
                {
                    return false;
                }
            }
        }
        for &f in &shared {
            let Some(face) = self.faces.get_mut(f as usize) else {
                continue;
            };
            face.live = false;
            self.live_faces -= 1;
            for i in face.corners {
                if let Some(point) = self.points.get_mut(i as usize) {
                    point.around.retain(|g| *g != f);
                }
            }
        }
        let moved = self
            .points
            .get_mut(b)
            .map(|point| std::mem::take(&mut point.around))
            .unwrap_or_default();
        for &f in &moved {
            if let Some(face) = self.faces.get_mut(f as usize) {
                for i in &mut face.corners {
                    if *i as usize == b {
                        *i = a as u32;
                    }
                }
            }
        }
        if let Some(point) = self.points.get_mut(b) {
            point.dead = true;
        }
        if let Some(point) = self.points.get_mut(a) {
            point.around.extend(moved);
            point.position = at;
            point.quadric = quadric_sum(&point.quadric, &quadric_b);
            point.held |= held_b;
            point.weight += weight_b;
            // Its edges are costed again; the others' costs did not change.
            point.stamp += 1;
        }
        true
    }

    /// The live points and triangles, renumbered.
    fn shape(&self) -> Shape {
        let mut index = vec![u32::MAX; self.points.len()];
        let mut out = Shape::default();
        for face in self.faces.iter().filter(|face| face.live) {
            let ids = face.corners.map(|i| {
                let i = i as usize;
                match (index.get_mut(i), self.points.get(i)) {
                    (Some(slot), Some(point)) => {
                        if *slot == u32::MAX {
                            *slot = out.positions.len() as u32;
                            out.positions.push(point.position);
                        }
                        *slot
                    }
                    _ => 0,
                }
            });
            out.triangles.push(ids);
            out.labels.push(face.label);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A sphere of radius `r`: an icosahedron split `levels` times, its points pushed onto
    /// the sphere, wound outwards.
    fn sphere(r: f64, levels: usize) -> Shape {
        let t = (1.0 + 5f64.sqrt()) / 2.0;
        let mut positions: Vec<V3> = [
            [-1.0, t, 0.0],
            [1.0, t, 0.0],
            [-1.0, -t, 0.0],
            [1.0, -t, 0.0],
            [0.0, -1.0, t],
            [0.0, 1.0, t],
            [0.0, -1.0, -t],
            [0.0, 1.0, -t],
            [t, 0.0, -1.0],
            [t, 0.0, 1.0],
            [-t, 0.0, -1.0],
            [-t, 0.0, 1.0],
        ]
        .to_vec();
        let mut triangles: Vec<[u32; 3]> = vec![
            [0, 11, 5],
            [0, 5, 1],
            [0, 1, 7],
            [0, 7, 10],
            [0, 10, 11],
            [1, 5, 9],
            [5, 11, 4],
            [11, 10, 2],
            [10, 7, 6],
            [7, 1, 8],
            [3, 9, 4],
            [3, 4, 2],
            [3, 2, 6],
            [3, 6, 8],
            [3, 8, 9],
            [4, 9, 5],
            [2, 4, 11],
            [6, 2, 10],
            [8, 6, 7],
            [9, 8, 1],
        ];
        for _ in 0..levels {
            let mut middles: HashMap<(u32, u32), u32> = HashMap::new();
            let mut next = Vec::new();
            for t in &triangles {
                let mut mid = |a: u32, b: u32| -> u32 {
                    *middles.entry((a.min(b), a.max(b))).or_insert_with(|| {
                        positions.push(scale(
                            add(positions[a as usize], positions[b as usize]),
                            0.5,
                        ));
                        (positions.len() - 1) as u32
                    })
                };
                let (ab, bc, ca) = (mid(t[0], t[1]), mid(t[1], t[2]), mid(t[2], t[0]));
                next.extend([[t[0], ab, ca], [t[1], bc, ab], [t[2], ca, bc], [ab, bc, ca]]);
            }
            triangles = next;
        }
        for p in &mut positions {
            *p = scale(*p, r / norm(*p));
        }
        let labels = vec![crate::NO_LABEL; triangles.len()];
        Shape {
            positions,
            triangles,
            labels,
        }
    }

    /// Every edge has exactly two triangles, one each way round.
    fn closed_manifold(s: &Shape) -> bool {
        let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
        for t in &s.triangles {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a, b)).or_insert(0) += 1;
            }
        }
        edges
            .iter()
            .all(|(&(a, b), &n)| n == 1 && edges.get(&(b, a)) == Some(&1))
    }

    #[test]
    fn a_sphere_keeps_its_shape_with_a_tenth_of_its_triangles() {
        let s = sphere(10.0, 5);
        assert_eq!(s.triangles.len(), 20 * 4usize.pow(5));
        assert!(closed_manifold(&s));
        let fewer = decimate(&s, 2000);
        assert!(fewer.triangles.len() <= 2000 && fewer.triangles.len() > 1800);
        assert!(closed_manifold(&fewer));
        assert_eq!(fewer.labels.len(), fewer.triangles.len());
        // Its points stay on the sphere and its volume with it.
        let worst = fewer
            .positions
            .iter()
            .map(|p| (norm(*p) - 10.0).abs())
            .fold(0.0, f64::max);
        assert!(worst < 0.1, "{worst}");
        let ratio = fewer.volume() / s.volume();
        assert!((ratio - 1.0).abs() < 0.01, "{ratio}");
        // The same on every run.
        assert_eq!(decimate(&s, 2000), fewer);
        // Asked for no fewer, it is left as it is.
        assert_eq!(decimate(&s, 1_000_000), s);
    }

    #[test]
    fn an_outcome_surface_is_simplified_into_a_closed_manifold() {
        use crate::{Grid, Voxel, surface};
        // A bar 12 voxels long, 4 × 4 across, its densities ramping at the ends.
        let counts = [16, 8, 8];
        let grid = Grid::from_parts(
            [0.0; 3],
            1.0,
            counts,
            vec![Voxel::Design; counts.iter().product()],
        )
        .unwrap();
        let density: Vec<u8> = (0..grid.voxel_count())
            .map(|v| {
                let c = grid.centre(v);
                let inside = (2.0..14.0).contains(&c[0])
                    && (2.0..6.0).contains(&c[1])
                    && (2.0..6.0).contains(&c[2]);
                if inside { 255 } else { 0 }
            })
            .collect();
        let s = surface(&grid, &density, 12);
        assert!(closed_manifold(&s));
        let fewer = decimate(&s, s.triangles.len() / 4);
        assert!(closed_manifold(&fewer));
        assert!(fewer.triangles.len() <= s.triangles.len() / 4);
        let ratio = fewer.volume() / s.volume();
        assert!((ratio - 1.0).abs() < 0.05, "{ratio}");
    }

    #[test]
    fn a_tolerance_keeps_the_shape_where_the_target_would_wear_it() {
        use crate::{Grid, Voxel, surface};
        // A plate with a hole through it cut from voxels, unsmoothed: every point on a
        // plane, so collapses within a face cost nothing and across an edge a lot.
        let counts = [24, 12, 8];
        let grid = Grid::from_parts(
            [0.0; 3],
            0.5,
            counts,
            vec![Voxel::Design; counts.iter().product()],
        )
        .unwrap();
        let density: Vec<u8> = (0..grid.voxel_count())
            .map(|v| {
                let c = grid.centre(v);
                let plate = (1.0..11.0).contains(&c[0])
                    && (1.0..5.0).contains(&c[1])
                    && (1.0..3.0).contains(&c[2]);
                let hole = (c[0] - 6.0).hypot(c[1] - 3.0) < 1.2;
                if plate && !hole { 255 } else { 0 }
            })
            .collect();
        let s = surface(&grid, &density, 0);
        let volume = s.volume();
        // Within a hundredth of a millimetre: the faces simplified, the shape kept.
        let kept = decimate_holding(&s, &[], 40, 0.01);
        assert!(closed_manifold(&kept));
        assert!(kept.triangles.len() < s.triangles.len() / 4);
        assert!((kept.volume() - volume).abs() < 1e-9 * volume);
        // With none, on to the target: worn.
        let worn = decimate(&s, 40);
        assert!(worn.triangles.len() < kept.triangles.len());
        let change = (worn.volume() - volume).abs() / volume;
        assert!(change > 1e-3, "{change}");
    }

    /// The thinnest triangle's twice area over its longest edge squared.
    fn thinnest(s: &Shape) -> f64 {
        s.triangles
            .iter()
            .map(|t| thickness(t.map(|i| s.positions[i as usize])))
            .fold(f64::INFINITY, f64::min)
    }

    /// Flat faces and straight creases, collapsed far: no triangle left as thin as a
    /// kernel would skip (the servo arm's outcome lost faces in OpenCASCADE that way).
    #[test]
    fn a_flat_sided_surface_is_simplified_without_slivers() {
        use crate::{Grid, Voxel, surface};
        let counts = [30, 30, 8];
        let grid = Grid::from_parts(
            [0.0; 3],
            0.5,
            counts,
            vec![Voxel::Design; counts.iter().product()],
        )
        .unwrap();
        // An L-shaped plate: before slivers were refused, a quarter of its triangles left one
        // at 1.4e-4.
        let density: Vec<u8> = (0..grid.voxel_count())
            .map(|v| {
                let c = grid.centre(v);
                let plate = (1.0..14.0).contains(&c[0])
                    && (1.0..14.0).contains(&c[1])
                    && (1.0..3.0).contains(&c[2]);
                let notch = c[0] > 5.0 && c[1] > 5.0;
                if plate && !notch { 255 } else { 0 }
            })
            .collect();
        let s = surface(&grid, &density, 12);
        assert!(thinnest(&s) >= MIN_THICKNESS);
        for k in [2, 4, 8, 16, 32] {
            let target = s.triangles.len() / k;
            let fewer = decimate(&s, target);
            assert!(closed_manifold(&fewer));
            assert!(fewer.triangles.len() <= target);
            let thin = thinnest(&fewer);
            assert!(thin >= MIN_THICKNESS, "{target}: {thin:e}");
        }
    }

    #[test]
    fn a_surface_falls_into_its_pieces_the_largest_first() {
        let small = sphere(2.0, 2);
        let large = sphere(10.0, 3);
        let mut both = small.clone();
        let offset = both.positions.len() as u32;
        both.positions
            .extend(large.positions.iter().map(|p| add(*p, [50.0, 0.0, 0.0])));
        both.triangles
            .extend(large.triangles.iter().map(|t| t.map(|i| i + offset)));
        both.labels.extend(large.labels.iter().copied());
        let pieces = solid_pieces(&both);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].triangles.len(), large.triangles.len());
        assert_eq!(pieces[1].positions.len(), small.positions.len());
        assert!(pieces.iter().all(closed_manifold));
        // A speck a thousandth of the largest's size goes.
        let mut speck = both.clone();
        let tiny = sphere(0.5, 1);
        let offset = speck.positions.len() as u32;
        speck.positions.extend(tiny.positions.iter().copied());
        speck
            .triangles
            .extend(tiny.triangles.iter().map(|t| t.map(|i| i + offset)));
        speck.labels.extend(tiny.labels.iter().copied());
        assert_eq!(solid_pieces(&speck).len(), 2);
    }
}
