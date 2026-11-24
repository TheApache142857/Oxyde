//! A closed triangle surface, and its smoothing.

use crate::math::{V3, add, cross, dot, scale, sub};

/// A closed triangle surface: the optimized shape.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Shape {
    pub positions: Vec<V3>,
    /// Wound counter-clockwise seen from outside.
    pub triangles: Vec<[u32; 3]>,
    /// The model face each triangle lies on: the label of the boundary facet it was cut
    /// from, or [`crate::NO_LABEL`] for the cut through the material.
    pub labels: Vec<u32>,
}

impl Shape {
    /// mm³: the volume it encloses, its triangles off its points left out.
    pub fn volume(&self) -> f64 {
        self.triangles
            .iter()
            .filter_map(|t| {
                let p = |i: u32| self.positions.get(i as usize).copied();
                Some(dot(p(t[0])?, cross(p(t[1])?, p(t[2])?)) / 6.0)
            })
            .sum()
    }

    /// Its first fault, if any ([`crate::surfaces::mesh_fault`]).
    pub fn fault(&self) -> Option<crate::surfaces::MeshFault> {
        crate::surfaces::mesh_fault(&self.positions, &self.triangles, &self.labels)
    }
}

/// Taubin's λ|μ smoothing: a shrinking Laplacian step, then a growing one, `rounds` times,
/// over the points not `fixed`.
pub fn smooth(shape: &mut Shape, fixed: &[bool], rounds: usize) {
    taubin(shape, fixed, rounds, false);
}

/// [`smooth`], the points next to a fixed one taking the shrinking steps only: each stays
/// among its neighbours, so where the fixed points lie on a flat face, it stays on its side
/// of the face (the growing step pushed some a few hundredths of a voxel through it).
pub fn smooth_holding(shape: &mut Shape, fixed: &[bool], rounds: usize) {
    taubin(shape, fixed, rounds, true);
}

/// Triangles off the points are left out.
fn taubin(shape: &mut Shape, fixed: &[bool], rounds: usize, hold_next: bool) {
    let n = shape.positions.len();
    let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); n];
    for t in shape
        .triangles
        .iter()
        .filter(|t| t.iter().all(|&i| (i as usize) < n))
    {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if let Some(list) = neighbours.get_mut(a as usize) {
                list.push(b);
            }
            if let Some(list) = neighbours.get_mut(b as usize) {
                list.push(a);
            }
        }
    }
    for list in &mut neighbours {
        list.sort_unstable();
        list.dedup();
    }
    let is_fixed = |i: usize| fixed.get(i).copied().unwrap_or(false);
    let next_to_fixed: Vec<bool> = neighbours
        .iter()
        .map(|list| hold_next && list.iter().any(|&j| is_fixed(j as usize)))
        .collect();
    let step = |positions: &mut Vec<V3>, factor: f64| {
        let next: Vec<V3> = positions
            .iter()
            .zip(&neighbours)
            .zip(&next_to_fixed)
            .enumerate()
            .map(|(i, ((&p, list), &next_to))| {
                if is_fixed(i) || list.is_empty() || factor < 0.0 && next_to {
                    return p;
                }
                let sum = list
                    .iter()
                    .filter_map(|&j| positions.get(j as usize))
                    .fold([0.0; 3], |s, &q| add(s, q));
                add(
                    p,
                    scale(sub(scale(sum, 1.0 / list.len() as f64), p), factor),
                )
            })
            .collect();
        *positions = next;
    };
    for _ in 0..rounds {
        step(&mut shape.positions, 0.5);
        step(&mut shape.positions, -0.53);
    }
}
