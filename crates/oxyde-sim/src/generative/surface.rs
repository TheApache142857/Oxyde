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

// Indices into the lattice, sized here.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;

use super::grid::Grid;
use crate::math::{V3, add, cross, dot, lerp, scale, sub};
use crate::mesh::kuhn_tets;
use crate::topology::{Shape, smooth};

/// Half of a byte's range: a voxel is in the material from 128 up.
const THRESHOLD: f64 = 127.5;

/// The surface of `density` (one byte per voxel of `grid`, as [`super::Outcome`] has it),
/// smoothed by `smoothing` rounds of Taubin's smoothing. Empty when the densities do not
/// fit the grid.
pub fn surface(grid: &Grid, density: &[u8], smoothing: usize) -> Shape {
    if density.len() != grid.voxel_count() {
        return Shape::default();
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
        out: Shape::default(),
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
    let mut out = cut.out;
    if smoothing > 0 {
        let fixed = vec![false; out.positions.len()];
        smooth(&mut out, &fixed, smoothing);
    }
    out
}

/// The surface being cut: one point per lattice edge crossed.
struct Cutter<'a> {
    value: &'a [u8],
    position: &'a dyn Fn(usize) -> V3,
    out: Shape,
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
        let t = ((THRESHOLD - va) / (vb - va)).clamp(0.0, 1.0);
        let p = lerp((self.position)(key.0), (self.position)(key.1), t);
        self.out.positions.push(p);
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
