//! A bounding volume hierarchy over the scene's triangles: boxes in boxes, so a ray
//! looks at a few dozen triangles instead of all of them.
//!
//! Built top-down with the surface area heuristic over 12 bins per axis (Wald, "On fast
//! construction of SAH-based bounding volume hierarchies"), flattened into one array the
//! way it is walked. The build is deterministic: the same triangles give the same tree.

use super::vector::V;

/// Triangles in a leaf before the heuristic is even asked.
const MIN_LEAF: usize = 2;
/// Triangles a leaf may hold when no split pays.
const MAX_LEAF: usize = 8;
const BINS: usize = 12;

/// An axis-aligned box; the empty box has `lo` above `hi`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub lo: V,
    pub hi: V,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb {
        lo: V::splat(f32::INFINITY),
        hi: V::splat(f32::NEG_INFINITY),
    };

    pub fn grow(self, p: V) -> Aabb {
        Aabb {
            lo: self.lo.min(p),
            hi: self.hi.max(p),
        }
    }

    pub fn union(self, o: Aabb) -> Aabb {
        Aabb {
            lo: self.lo.min(o.lo),
            hi: self.hi.max(o.hi),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lo.x > self.hi.x
    }

    /// Half the surface area, which is all the heuristic compares.
    pub fn half_area(&self) -> f32 {
        if self.is_empty() {
            return 0.0;
        }
        let d = self.hi - self.lo;
        d.x * d.y + d.y * d.z + d.z * d.x
    }

    pub fn centre(&self) -> V {
        (self.lo + self.hi) * 0.5
    }

    /// Where the ray from `origin` with inverse direction `inv` enters the box, if it
    /// does before `t_max`.
    fn entry(&self, origin: V, inv: V, t_max: f32) -> Option<f32> {
        let t0 = (self.lo - origin) * inv;
        let t1 = (self.hi - origin) * inv;
        let near = t0.min(t1).max_component().max(0.0);
        let far = t0.max(t1);
        let far = far.x.min(far.y).min(far.z).min(t_max);
        (near <= far).then_some(near)
    }
}

#[derive(Debug, Clone, Copy)]
struct Node {
    bounds: Aabb,
    /// A leaf's first triangle, or an inner node's left child (its right child is
    /// `right`).
    first: u32,
    /// Triangles in a leaf; 0 for an inner node.
    count: u32,
    right: u32,
}

/// The hierarchy. Leaves refer to triangles by position in the order [`Bvh::build`]
/// returns, which the scene puts its triangles in.
#[derive(Debug, Clone, Default)]
pub struct Bvh {
    nodes: Vec<Node>,
}

impl Bvh {
    /// Builds the tree over boxes `bounds` (one per triangle). Returns it and the order to
    /// put the triangles in: leaf ranges index that order.
    pub fn build(bounds: &[Aabb]) -> (Bvh, Vec<u32>) {
        let mut order: Vec<u32> = (0..bounds.len() as u32).collect();
        let centres: Vec<V> = bounds.iter().map(Aabb::centre).collect();
        let mut nodes = Vec::with_capacity(bounds.len().max(1) * 2);
        nodes.push(Node {
            bounds: Aabb::EMPTY,
            first: 0,
            count: 0,
            right: 0,
        });
        let mut work = vec![(0usize, 0usize, bounds.len())];
        while let Some((node, start, end)) = work.pop() {
            let Some(range) = order.get_mut(start..end) else {
                continue;
            };
            let mut box_all = Aabb::EMPTY;
            let mut box_centres = Aabb::EMPTY;
            for &i in range.iter() {
                if let (Some(b), Some(c)) = (bounds.get(i as usize), centres.get(i as usize)) {
                    box_all = box_all.union(*b);
                    box_centres = box_centres.grow(*c);
                }
            }
            let count = end - start;
            let leaf = |nodes: &mut Vec<Node>| {
                if let Some(n) = nodes.get_mut(node) {
                    *n = Node {
                        bounds: box_all,
                        first: start as u32,
                        count: count as u32,
                        right: 0,
                    };
                }
            };
            if count <= MIN_LEAF {
                leaf(&mut nodes);
                continue;
            }
            let split = best_split(range, bounds, &centres, &box_centres);
            let leaf_cost = count as f32;
            let mid = match split {
                Some((axis, at, cost)) if cost < leaf_cost || count > MAX_LEAF => {
                    let mid = start
                        + partition(range, |i| {
                            centres.get(i as usize).is_some_and(|c| c.axis(axis) < at)
                        });
                    if mid == start || mid == end {
                        median_split(range, &centres, &box_centres);
                        start + count / 2
                    } else {
                        mid
                    }
                }
                None if count > MAX_LEAF => {
                    // Every centre in one point: split anyhow, the leaves stay small.
                    median_split(range, &centres, &box_centres);
                    start + count / 2
                }
                _ => {
                    leaf(&mut nodes);
                    continue;
                }
            };
            let left = nodes.len();
            nodes.push(Node {
                bounds: Aabb::EMPTY,
                first: 0,
                count: 0,
                right: 0,
            });
            let right = nodes.len();
            nodes.push(Node {
                bounds: Aabb::EMPTY,
                first: 0,
                count: 0,
                right: 0,
            });
            if let Some(n) = nodes.get_mut(node) {
                *n = Node {
                    bounds: box_all,
                    first: left as u32,
                    count: 0,
                    right: right as u32,
                };
            }
            work.push((right, mid, end));
            work.push((left, start, mid));
        }
        (Bvh { nodes }, order)
    }

    /// The box around everything.
    pub fn bounds(&self) -> Aabb {
        self.nodes.first().map_or(Aabb::EMPTY, |n| n.bounds)
    }

    /// Walks the leaves the ray from `origin` along `dir` reaches before `t_max`, nearest
    /// first. `leaf(first, count, &mut t_max)` tests a leaf's triangles; it may lower
    /// `t_max` (a nearer hit found), and returns false to stop the walk.
    pub fn traverse(
        &self,
        origin: V,
        dir: V,
        mut t_max: f32,
        mut leaf: impl FnMut(u32, u32, &mut f32) -> bool,
    ) {
        let inv = dir.map(|c| 1.0 / c);
        let Some(root) = self.nodes.first() else {
            return;
        };
        if root.bounds.is_empty() || root.bounds.entry(origin, inv, t_max).is_none() {
            return;
        }
        let mut stack = [0u32; 64];
        let mut depth = 1usize;
        while depth > 0 {
            depth -= 1;
            let Some(&index) = stack.get(depth) else {
                break;
            };
            let Some(node) = self.nodes.get(index as usize) else {
                break;
            };
            if node.count > 0 {
                if !leaf(node.first, node.count, &mut t_max) {
                    return;
                }
                continue;
            }
            // Children always come after their parent: anything else is not a tree, and
            // walking it could loop forever.
            if node.first <= index || node.right <= index {
                continue;
            }
            let (Some(a), Some(b)) = (
                self.nodes.get(node.first as usize),
                self.nodes.get(node.right as usize),
            ) else {
                continue;
            };
            let ta = a.bounds.entry(origin, inv, t_max);
            let tb = b.bounds.entry(origin, inv, t_max);
            // Push the farther child first so the nearer is walked first.
            let mut push = |i: u32| {
                if let Some(slot) = stack.get_mut(depth) {
                    *slot = i;
                    depth += 1;
                }
            };
            match (ta, tb) {
                (Some(ta), Some(tb)) => {
                    if ta <= tb {
                        push(node.right);
                        push(node.first);
                    } else {
                        push(node.first);
                        push(node.right);
                    }
                }
                (Some(_), None) => push(node.first),
                (None, Some(_)) => push(node.right),
                (None, None) => {}
            }
        }
    }

    #[cfg(test)]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

/// The cheapest binned split of `range`: its axis, the centre coordinate below which a
/// triangle goes left, and its cost in triangle tests (relative to the parent's box).
fn best_split(
    range: &[u32],
    bounds: &[Aabb],
    centres: &[V],
    box_centres: &Aabb,
) -> Option<(usize, f32, f32)> {
    let mut best: Option<(usize, f32, f32)> = None;
    let mut parent = Aabb::EMPTY;
    for &i in range {
        if let Some(b) = bounds.get(i as usize) {
            parent = parent.union(*b);
        }
    }
    let parent_area = parent.half_area().max(1e-30);
    for axis in 0..3 {
        let lo = box_centres.lo.axis(axis);
        let extent = box_centres.hi.axis(axis) - lo;
        if extent <= 1e-12 || !extent.is_finite() {
            continue;
        }
        let mut bins = [(Aabb::EMPTY, 0u32); BINS];
        let scale = BINS as f32 / extent;
        for &i in range {
            let (Some(b), Some(c)) = (bounds.get(i as usize), centres.get(i as usize)) else {
                continue;
            };
            let k = (((c.axis(axis) - lo) * scale) as usize).min(BINS - 1);
            if let Some(bin) = bins.get_mut(k) {
                bin.0 = bin.0.union(*b);
                bin.1 += 1;
            }
        }
        // Areas and counts left of each boundary, then right of it.
        let mut left = [(0.0f32, 0u32); BINS];
        let mut acc = (Aabb::EMPTY, 0u32);
        for (k, bin) in bins.iter().enumerate() {
            acc = (acc.0.union(bin.0), acc.1 + bin.1);
            if let Some(slot) = left.get_mut(k) {
                *slot = (acc.0.half_area(), acc.1);
            }
        }
        let mut acc = (Aabb::EMPTY, 0u32);
        for k in (1..BINS).rev() {
            if let Some(bin) = bins.get(k) {
                acc = (acc.0.union(bin.0), acc.1 + bin.1);
            }
            let Some(&(left_area, left_count)) = left.get(k - 1) else {
                continue;
            };
            if left_count == 0 || acc.1 == 0 {
                continue;
            }
            let cost = 0.5
                + (left_area * left_count as f32 + acc.0.half_area() * acc.1 as f32) / parent_area;
            if best.is_none_or(|(_, _, c)| cost < c) {
                best = Some((axis, lo + k as f32 / scale, cost));
            }
        }
    }
    best
}

/// Moves the indices for which `left` holds to the front; returns how many there are.
fn partition(range: &mut [u32], left: impl Fn(u32) -> bool) -> usize {
    let mut first_right = 0;
    for k in 0..range.len() {
        let Some(&i) = range.get(k) else {
            break;
        };
        if left(i) {
            range.swap(k, first_right);
            first_right += 1;
        }
    }
    first_right
}

/// Halves `range` along the longest axis of its centres: the first half below the
/// median.
fn median_split(range: &mut [u32], centres: &[V], box_centres: &Aabb) {
    let d = box_centres.hi - box_centres.lo;
    let axis = if d.x >= d.y && d.x >= d.z {
        0
    } else if d.y >= d.z {
        1
    } else {
        2
    };
    let key = |i: &u32| centres.get(*i as usize).map_or(0.0, |c| c.axis(axis));
    let mid = range.len() / 2;
    if mid < range.len() {
        range.select_nth_unstable_by(mid, |a, b| key(a).total_cmp(&key(b)).then(a.cmp(b)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxes(n: usize) -> Vec<Aabb> {
        // A deterministic scatter of small boxes.
        let mut out = Vec::new();
        let mut s = 12345u32;
        let mut next = || {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (s >> 8) as f32 / 16_777_216.0
        };
        for _ in 0..n {
            let c = V::new(next() * 100.0, next() * 100.0, next() * 100.0);
            out.push(Aabb::EMPTY.grow(c - V::splat(0.5)).grow(c + V::splat(0.5)));
        }
        out
    }

    #[test]
    fn every_triangle_is_in_exactly_one_leaf_and_inside_its_boxes() {
        let b = boxes(1000);
        let (bvh, order) = Bvh::build(&b);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..1000).collect::<Vec<u32>>());
        let mut seen = vec![0; 1000];
        for node in &bvh.nodes {
            if node.count > 0 {
                assert!(node.count as usize <= MAX_LEAF);
                for k in node.first..node.first + node.count {
                    let tri = order[k as usize] as usize;
                    seen[tri] += 1;
                    let inner = b[tri];
                    assert!(node.bounds.lo.x <= inner.lo.x && node.bounds.hi.y >= inner.hi.y);
                }
            }
        }
        assert!(seen.iter().all(|&s| s == 1));
        // Deterministic.
        let (again, order_again) = Bvh::build(&b);
        assert_eq!(order, order_again);
        assert_eq!(bvh.node_count(), again.node_count());
    }

    #[test]
    fn coincident_centres_still_make_small_leaves() {
        let b = vec![Aabb::EMPTY.grow(V::ZERO).grow(V::ONE); 100];
        let (bvh, order) = Bvh::build(&b);
        assert_eq!(order.len(), 100);
        assert!(bvh.nodes.iter().all(|n| n.count as usize <= MAX_LEAF));
    }

    #[test]
    fn traversal_reaches_the_leaf_a_ray_crosses() {
        let b = boxes(500);
        let (bvh, order) = Bvh::build(&b);
        // Aim at the centre of box 42 from far away.
        let target = b[42].centre();
        let origin = V::new(-50.0, target.y, target.z);
        let mut found = false;
        bvh.traverse(
            origin,
            V::new(1.0, 0.0, 0.0),
            f32::INFINITY,
            |first, count, _| {
                for k in first..first + count {
                    if order[k as usize] == 42 {
                        found = true;
                    }
                }
                true
            },
        );
        assert!(found);
        // An empty tree is walked without trouble.
        let (empty, _) = Bvh::build(&[]);
        empty.traverse(V::ZERO, V::Y, 1.0, |_, _, _| panic!("no leaf"));
    }
}
