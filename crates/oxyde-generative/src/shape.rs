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
    /// mm³: the volume it encloses.
    pub fn volume(&self) -> f64 {
        self.triangles
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| self.positions[i as usize]);
                dot(a, cross(b, c)) / 6.0
            })
            .sum()
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

fn taubin(shape: &mut Shape, fixed: &[bool], rounds: usize, hold_next: bool) {
    let n = shape.positions.len();
    let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); n];
    for t in &shape.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            neighbours[a as usize].push(b);
            neighbours[b as usize].push(a);
        }
    }
    for list in &mut neighbours {
        list.sort_unstable();
        list.dedup();
    }
    let is_fixed = |i: usize| fixed.get(i).copied().unwrap_or(false);
    let next_to_fixed: Vec<bool> = (0..n)
        .map(|i| hold_next && neighbours[i].iter().any(|&j| is_fixed(j as usize)))
        .collect();
    let step = |positions: &mut Vec<V3>, factor: f64| {
        let next: Vec<V3> = (0..n)
            .map(|i| {
                let p = positions[i];
                let list = &neighbours[i];
                if is_fixed(i) || list.is_empty() || factor < 0.0 && next_to_fixed[i] {
                    return p;
                }
                let mean = scale(
                    list.iter()
                        .fold([0.0; 3], |s, &j| add(s, positions[j as usize])),
                    1.0 / list.len() as f64,
                );
                add(p, scale(sub(mean, p), factor))
            })
            .collect();
        *positions = next;
    };
    for _ in 0..rounds {
        step(&mut shape.positions, 0.5);
        step(&mut shape.positions, -0.53);
    }
}
