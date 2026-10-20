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
//! triangles give them, an end of three triangles only) or turn a triangle over. Ties go to
//! the lower indices, so the result is the same on every run.

// Indices into the mesh's arrays, all sized here from the shape.
#![allow(clippy::indexing_slicing)]

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::math::{V3, add, cross, dot, norm, scale, sub};
use crate::topology::Shape;

/// A collapse turns no triangle further than this cosine from where it faced.
const MIN_TURN_COSINE: f64 = 0.2;

/// `shape` with at most `target` triangles (as few as collapses can make when they cannot
/// reach it), closed and manifold as it was. Each triangle keeps its label.
pub fn decimate(shape: &Shape, target: usize) -> Shape {
    let mut m = Mesh::new(shape);
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
        if m.dead[a] || m.dead[b] || m.stamp[a] != e.stamp_a || m.stamp[b] != e.stamp_b {
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
        while parent[v] != v {
            parent[v] = parent[parent[v]];
            v = parent[v];
        }
        v
    }
    let valid = |t: &[u32; 3]| t.iter().all(|&i| (i as usize) < n);
    for t in shape.triangles.iter().filter(|t| valid(t)) {
        for k in 1..3 {
            let (a, b) = (
                root(&mut parent, t[0] as usize),
                root(&mut parent, t[k] as usize),
            );
            if a != b {
                parent[a.max(b)] = a.min(b);
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
        if piece_of[r] == usize::MAX {
            piece_of[r] = pieces.len();
            pieces.push(Shape::default());
        }
        let out = &mut pieces[piece_of[r]];
        let ids = t.map(|i| {
            let i = i as usize;
            if slot[i] == usize::MAX {
                slot[i] = out.positions.len();
                out.positions.push(shape.positions[i]);
            }
            slot[i] as u32
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
    std::array::from_fn(|i| p[i] + q[i])
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
        for row in 0..3 {
            a[row][c] = r[row];
        }
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    };
    let v = [column(0) / det, column(1) / det, column(2) / det];
    v.iter().all(|x| x.is_finite()).then_some(v)
}

struct Mesh {
    positions: Vec<V3>,
    quadrics: Vec<Quadric>,
    faces: Vec<[u32; 3]>,
    labels: Vec<u32>,
    live: Vec<bool>,
    live_faces: usize,
    /// The live faces around each point.
    around: Vec<Vec<u32>>,
    /// Bumped whenever the point moves or its faces change.
    stamp: Vec<u32>,
    dead: Vec<bool>,
}

impl Mesh {
    fn new(shape: &Shape) -> Self {
        let n = shape.positions.len();
        let mut around = vec![Vec::new(); n];
        let mut quadrics = vec![[0.0; 10]; n];
        let mut faces = Vec::with_capacity(shape.triangles.len());
        let mut labels = Vec::with_capacity(shape.triangles.len());
        let given = shape
            .labels
            .iter()
            .copied()
            .chain(std::iter::repeat(crate::NO_LABEL));
        for (t, label) in shape.triangles.iter().zip(given) {
            // Triangles off the points, or with a point twice, are no surface.
            if t.iter().any(|&i| i as usize >= n) || t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
                continue;
            }
            faces.push(*t);
            labels.push(label);
            let id = (faces.len() - 1) as u32;
            for &i in t {
                around[i as usize].push(id);
            }
            let [p, q, r] = t.map(|i| shape.positions[i as usize]);
            let normal = cross(sub(q, p), sub(r, p));
            let area2 = norm(normal);
            if area2 > 0.0 {
                let unit = scale(normal, 1.0 / area2);
                let plane = plane_quadric(unit, -dot(unit, p), 0.5 * area2);
                for &i in t {
                    quadrics[i as usize] = quadric_sum(&quadrics[i as usize], &plane);
                }
            }
        }
        let live_faces = faces.len();
        Mesh {
            positions: shape.positions.clone(),
            quadrics,
            live: vec![true; faces.len()],
            faces,
            labels,
            live_faces,
            around,
            stamp: vec![0; n],
            dead: vec![false; n],
        }
    }

    /// Every edge once, its ends in order.
    fn edges(&self) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = self
            .faces
            .iter()
            .flat_map(|t| {
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
        let mut out: Vec<usize> = self.around[v]
            .iter()
            .flat_map(|&f| self.faces[f as usize])
            .map(|i| i as usize)
            .filter(|&i| i != v)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The collapse of edge `a b` (`a < b`), costed, its point where the merged quadric is
    /// least (else the better of the ends and the middle).
    fn candidate(&self, a: usize, b: usize) -> Option<Candidate> {
        if a == b || self.dead[a] || self.dead[b] {
            return None;
        }
        let q = quadric_sum(&self.quadrics[a], &self.quadrics[b]);
        let (pa, pb) = (self.positions[a], self.positions[b]);
        let middle = scale(add(pa, pb), 0.5);
        let length = norm(sub(pb, pa));
        // A minimum far off the edge (a nearly flat neighbourhood) is no better a place.
        let optimum = quadric_minimum(&q).filter(|v| norm(sub(*v, middle)) <= length);
        let at = match optimum {
            Some(v) => v,
            None => [pa, pb, middle]
                .into_iter()
                .min_by(|x, y| quadric_error(&q, *x).total_cmp(&quadric_error(&q, *y)))
                .unwrap_or(middle),
        };
        Some(Candidate {
            cost: quadric_error(&q, at).max(0.0),
            a: a as u32,
            b: b as u32,
            stamp_a: self.stamp[a],
            stamp_b: self.stamp[b],
            at,
        })
    }

    /// Merges `b` into `a` at `at`, when that keeps the surface a closed manifold and turns
    /// no triangle over; whether it did.
    fn collapse(&mut self, a: usize, b: usize, at: V3) -> bool {
        // The edge's own two triangles, and the points across them.
        let shared: Vec<u32> = self.around[a]
            .iter()
            .copied()
            .filter(|f| self.faces[*f as usize].contains(&(b as u32)))
            .collect();
        if shared.len() != 2 {
            return false;
        }
        let mut across: Vec<usize> = shared
            .iter()
            .flat_map(|&f| self.faces[f as usize])
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
        // No triangle turned over or flattened.
        for &v in &[a, b] {
            for &f in &self.around[v] {
                if shared.contains(&f) {
                    continue;
                }
                let t = self.faces[f as usize];
                let before = t.map(|i| self.positions[i as usize]);
                let after = t.map(|i| {
                    if i as usize == a || i as usize == b {
                        at
                    } else {
                        self.positions[i as usize]
                    }
                });
                let n0 = cross(sub(before[1], before[0]), sub(before[2], before[0]));
                let n1 = cross(sub(after[1], after[0]), sub(after[2], after[0]));
                let (l0, l1) = (norm(n0), norm(n1));
                if l1 <= 1e-12 * l0.max(f64::MIN_POSITIVE)
                    || dot(n0, n1) < MIN_TURN_COSINE * l0 * l1
                {
                    return false;
                }
            }
        }
        for &f in &shared {
            self.live[f as usize] = false;
            self.live_faces -= 1;
            for i in self.faces[f as usize] {
                self.around[i as usize].retain(|g| *g != f);
            }
        }
        let moved = std::mem::take(&mut self.around[b]);
        for f in moved {
            for i in &mut self.faces[f as usize] {
                if *i as usize == b {
                    *i = a as u32;
                }
            }
            self.around[a].push(f);
        }
        self.positions[a] = at;
        self.quadrics[a] = quadric_sum(&self.quadrics[a], &self.quadrics[b]);
        self.dead[b] = true;
        // Its edges are costed again; the others' costs did not change.
        self.stamp[a] += 1;
        true
    }

    /// The live points and triangles, renumbered.
    fn shape(&self) -> Shape {
        let mut index = vec![u32::MAX; self.positions.len()];
        let mut out = Shape::default();
        for (f, t) in self.faces.iter().enumerate() {
            if !self.live[f] {
                continue;
            }
            let ids = t.map(|i| {
                let i = i as usize;
                if index[i] == u32::MAX {
                    index[i] = out.positions.len() as u32;
                    out.positions.push(self.positions[i]);
                }
                index[i]
            });
            out.triangles.push(ids);
            out.labels.push(self.labels[f]);
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
        use crate::generative::{Grid, Voxel, surface};
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
