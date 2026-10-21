//! An outcome's surface: where its densities, interpolated between the voxels' centres,
//! are half.
//!
//! The lattice cut is the voxels' centres, with a layer of empty ones all around the grid,
//! so the surface is closed and material reaching the grid's side ends on it. Between a
//! solid voxel and a void one the surface passes midway, on the face they share, so a
//! member one voxel thin keeps its place. Each cell between centres is cut into the six
//! tetrahedra of its Kuhn split, which neighbouring cells share faces with, and each
//! tetrahedron by marching tetrahedra: no ambiguous case, one point per edge crossed,
//! shared by the triangles on either side. The threshold, 127.5, is no byte, so no centre
//! lies on the surface and no triangle is flat. Taubin's smoothing then takes the voxels'
//! steps away.
//!
//! For *Create Part*, [`exact_surface`] puts the crossings where the design space's bodies
//! meet the material on those bodies' faces instead, and leaves them there while it smooths.

// Indices into the lattice, sized here.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;

use super::DesignSpace;
use super::grid::{Grid, Voxel, triangles_of};
use crate::bvh::{Bvh, Hit};
use crate::math::{V3, add, cross, dot, lerp, scale, sub};
use crate::mesh::kuhn_tets;
use crate::mesher::SurfaceMesh;
use crate::topology::{Shape, smooth_holding};

/// Half of a byte's range: a voxel is in the material from 128 up.
const THRESHOLD: f64 = 127.5;
/// A crossing put on an exact body stays this share of its edge from the lattice points,
/// so no two crossings around a point meet and no triangle is flat.
const MIN_SHARE: f64 = 0.02;

/// The surface of `density` (one byte per voxel of `grid`, as [`super::Outcome`] has it),
/// smoothed by `smoothing` rounds of Taubin's smoothing. Empty when the densities do not
/// fit the grid.
pub fn surface(grid: &Grid, density: &[u8], smoothing: usize) -> Shape {
    cut(grid, density, smoothing, &mut |_, _, _, _| None).0
}

/// [`surface`], meeting the design space's bodies on their faces: where a lattice edge runs
/// from the material out of a preserve body (or a region's body) into empty design space or
/// a hole, out of the starting shape, or into an obstacle, the surface crosses it where that
/// body's surface does (as
/// its triangles have it), not where the densities say, and keeps that point through the
/// smoothing. Its faces there lie on the bodies' faces (their tessellations', to a fiftieth
/// of the edge), so *Create Part* makes the part's solid of it alone: no boolean with the
/// exact bodies, which OpenCASCADE takes minutes over, or fails, on a fine outcome whose
/// faces lie a hair from theirs. An obstacle's offset keeps the material from its surface,
/// and is no surface to meet: there, the densities place the crossing. With it, per point,
/// whether it is on a body's face, for [`super::decimate_holding`] to leave it there; the
/// points next to those take only the smoothing's shrinking steps, which keep them on their
/// side of a flat face.
pub fn exact_surface(
    grid: &Grid,
    density: &[u8],
    smoothing: usize,
    space: &DesignSpace,
) -> (Shape, Vec<bool>) {
    let tree = |meshes: &mut dyn Iterator<Item = &SurfaceMesh>| -> Option<Bvh> {
        let triangles: Vec<[V3; 3]> = meshes.flat_map(triangles_of).collect();
        (!triangles.is_empty()).then(|| Bvh::new(triangles))
    };
    // A region's material ends on its body's faces, as a preserve body's on its own: a
    // lattice edge out of it, inside the body, crosses none.
    let preserve = tree(&mut space.preserve.iter().chain(&space.region_bodies));
    let start = tree(&mut space.starting_shape.iter());
    let obstacles = tree(&mut space.obstacles.iter().map(|o| &o.surface));
    let n = grid.counts;
    let size = n.map(|c| c + 2);
    // A lattice point's voxel; `None` past the grid's sides.
    let kind = |at: usize| -> Option<Voxel> {
        let c = [
            at % size[0],
            (at / size[0]) % size[1],
            at / (size[0] * size[1]),
        ];
        (0..3)
            .all(|a| (1..=n[a]).contains(&c[a]))
            .then(|| grid.kinds[grid.index(c.map(|x| x - 1))])
    };
    let mut hits: Vec<Hit> = Vec::new();
    let mut exact = |inside: usize, outside: usize, p: V3, q: V3| -> Option<f64> {
        // Out of a preserve body first: its faces may lie on the starting shape's.
        let through = [
            (kind(inside) == Some(Voxel::Preserve))
                .then_some(preserve.as_ref())
                .flatten(),
            match kind(outside) {
                None | Some(Voxel::Outside) => start.as_ref(),
                Some(Voxel::Obstacle) => obstacles.as_ref(),
                _ => None,
            },
        ];
        through.into_iter().flatten().find_map(|bodies| {
            bodies.segment_hits(p, q, &mut hits);
            hits.iter()
                .map(|h| h.t)
                .filter(|t| (0.0..=1.0).contains(t))
                .min_by(f64::total_cmp)
        })
    };
    cut(grid, density, smoothing, &mut exact)
}

/// The surface of `density`, its crossings put where `exact` says (the share of the edge
/// from the lattice point in the material, given the two points and their places) or, when
/// it says nothing, where the densities are half; and per point, whether `exact` put it.
fn cut(
    grid: &Grid,
    density: &[u8],
    smoothing: usize,
    exact: &mut dyn FnMut(usize, usize, V3, V3) -> Option<f64>,
) -> (Shape, Vec<bool>) {
    if density.len() != grid.voxel_count() {
        return (Shape::default(), Vec::new());
    }
    let n = grid.counts;
    // The lattice: lattice point (i, j, k) is voxel (i − 1, j − 1, k − 1), the points
    // beyond the grid empty.
    let size = n.map(|c| c + 2);
    let index = |c: [usize; 3]| c[0] + size[0] * (c[1] + size[1] * c[2]);
    let count = size.iter().product::<usize>();
    let mut value = vec![0u8; count];
    for k in 0..n[2] {
        for j in 0..n[1] {
            for i in 0..n[0] {
                value[index([i + 1, j + 1, k + 1])] = density[grid.index([i, j, k])];
            }
        }
    }
    let position = |at: usize| -> V3 {
        let c = [
            at % size[0],
            (at / size[0]) % size[1],
            at / (size[0] * size[1]),
        ];
        std::array::from_fn(|a| grid.origin[a] + (c[a] as f64 - 0.5) * grid.size)
    };
    let inside = |at: usize| f64::from(value[at]) > THRESHOLD;
    let mut cut = Cutter {
        value: &value,
        position: &position,
        exact,
        out: Shape::default(),
        fixed: Vec::new(),
        points: HashMap::new(),
    };
    for k in 0..size[2] - 1 {
        for j in 0..size[1] - 1 {
            for i in 0..size[0] - 1 {
                let at = |d: [usize; 3]| index([i + d[0], j + d[1], k + d[2]]);
                let corners: [usize; 8] =
                    std::array::from_fn(|d| at([d & 1, (d >> 1) & 1, (d >> 2) & 1]));
                let first = inside(corners[0]);
                if corners.iter().all(|&c| inside(c) == first) {
                    continue;
                }
                for tet in kuhn_tets(at) {
                    let (ins, outs): (Vec<usize>, Vec<usize>) =
                        tet.iter().partition(|&&c| inside(c));
                    if ins.is_empty() || outs.is_empty() {
                        continue;
                    }
                    let centre = |list: &[usize]| {
                        let s = list.iter().fold([0.0; 3], |s, &c| add(s, position(c)));
                        scale(s, 1.0 / list.len() as f64)
                    };
                    let outward = sub(centre(&outs), centre(&ins));
                    match ins.len() {
                        1 => cut.triangle(
                            [(ins[0], outs[0]), (ins[0], outs[1]), (ins[0], outs[2])],
                            outward,
                        ),
                        3 => cut.triangle(
                            [(ins[0], outs[0]), (ins[1], outs[0]), (ins[2], outs[0])],
                            outward,
                        ),
                        _ => {
                            let (a, b, c, d) = (ins[0], ins[1], outs[0], outs[1]);
                            cut.triangle([(a, c), (a, d), (b, d)], outward);
                            cut.triangle([(a, c), (b, d), (b, c)], outward);
                        }
                    }
                }
            }
        }
    }
    let (mut out, fixed) = (cut.out, cut.fixed);
    if smoothing > 0 {
        smooth_holding(&mut out, &fixed, smoothing);
    }
    (out, fixed)
}

/// The surface being cut: one point per lattice edge crossed.
struct Cutter<'a> {
    value: &'a [u8],
    position: &'a dyn Fn(usize) -> V3,
    exact: &'a mut dyn FnMut(usize, usize, V3, V3) -> Option<f64>,
    out: Shape,
    /// Per point: put on an exact body, so the smoothing leaves it.
    fixed: Vec<bool>,
    points: HashMap<(usize, usize), u32>,
}

impl Cutter<'_> {
    fn crossing(&mut self, a: usize, b: usize) -> u32 {
        let key = (a.min(b), a.max(b));
        if let Some(&p) = self.points.get(&key) {
            return p;
        }
        // The ends are on either side of the threshold, so they differ.
        let (va, vb) = (f64::from(self.value[key.0]), f64::from(self.value[key.1]));
        let (pa, pb) = ((self.position)(key.0), (self.position)(key.1));
        let (inside, outside, pi, po) = if va > THRESHOLD {
            (key.0, key.1, pa, pb)
        } else {
            (key.1, key.0, pb, pa)
        };
        let (p, fixed) = match (self.exact)(inside, outside, pi, po) {
            Some(t) => (lerp(pi, po, t.clamp(MIN_SHARE, 1.0 - MIN_SHARE)), true),
            None => {
                let t = ((THRESHOLD - va) / (vb - va)).clamp(0.0, 1.0);
                (lerp(pa, pb, t), false)
            }
        };
        self.out.positions.push(p);
        self.fixed.push(fixed);
        let id = (self.out.positions.len() - 1) as u32;
        self.points.insert(key, id);
        id
    }

    /// A triangle through the crossings of three edges, its normal along `outward`, which
    /// way it turns worked out from the edges' middles.
    fn triangle(&mut self, edges: [(usize, usize); 3], outward: V3) {
        let ids = edges.map(|(a, b)| self.crossing(a, b));
        let [p, q, r] = edges.map(|(a, b)| lerp((self.position)(a), (self.position)(b), 0.5));
        let normal = cross(sub(q, p), sub(r, p));
        self.out.triangles.push(if dot(normal, outward) < 0.0 {
            [ids[0], ids[2], ids[1]]
        } else {
            ids
        });
        self.out.labels.push(crate::NO_LABEL);
    }
}
