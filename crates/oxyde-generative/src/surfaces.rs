//! The model's solids as closed triangle meshes, and the hierarchy they are searched in.

use std::collections::BTreeSet;

use crate::bvh::{Bvh, Hit};
use crate::math::{V3, add, cross, norm, scale, sub, tet_volume6};

pub const NONE: u32 = u32::MAX;

/// The boundary of one solid: a closed triangle mesh, its triangles counter-clockwise
/// seen from outside, each with the label of the model face it belongs to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SurfaceMesh {
    pub positions: Vec<V3>,
    pub triangles: Vec<[u32; 3]>,
    /// One per triangle.
    pub labels: Vec<u32>,
}

/// What is wrong with a triangle mesh handed to the crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshFault {
    /// Triangle `triangle` names a point past the positions.
    PointOutOfRange { triangle: usize },
    /// Not one label per triangle.
    LabelCount { labels: usize, triangles: usize },
    /// Point `point` is not at a finite position.
    NotFinite { point: usize },
}

/// The first fault of the mesh of `positions`, `triangles` and `labels`, if any.
pub fn mesh_fault(positions: &[V3], triangles: &[[u32; 3]], labels: &[u32]) -> Option<MeshFault> {
    if labels.len() != triangles.len() {
        return Some(MeshFault::LabelCount {
            labels: labels.len(),
            triangles: triangles.len(),
        });
    }
    if let Some(point) = positions
        .iter()
        .position(|p| p.iter().any(|x| !x.is_finite()))
    {
        return Some(MeshFault::NotFinite { point });
    }
    triangles
        .iter()
        .position(|t| t.iter().any(|&i| i as usize >= positions.len()))
        .map(|triangle| MeshFault::PointOutOfRange { triangle })
}

impl SurfaceMesh {
    /// Its first fault, if any ([`mesh_fault`]).
    pub fn fault(&self) -> Option<MeshFault> {
        mesh_fault(&self.positions, &self.triangles, &self.labels)
    }

    /// The volume it encloses (by the divergence theorem), mm³.
    pub fn volume(&self) -> f64 {
        self.triangles
            .iter()
            .filter_map(|t| {
                let p = |i: u32| self.positions.get(i as usize).copied();
                Some(tet_volume6([0.0; 3], p(t[0])?, p(t[1])?, p(t[2])?) / 6.0)
            })
            .sum()
    }
}

/// The surfaces, gathered into one hierarchy.
pub struct Surfaces {
    pub bvh: Bvh,
    /// Per triangle of the hierarchy: its solid and its label.
    pub solid: Vec<u32>,
    pub label: Vec<u32>,
}

impl Surfaces {
    pub fn new<'a>(solids: impl IntoIterator<Item = &'a SurfaceMesh>) -> Surfaces {
        let mut triangles = Vec::new();
        let mut solid = Vec::new();
        let mut label = Vec::new();
        for (s, mesh) in solids.into_iter().enumerate() {
            for (k, t) in mesh.triangles.iter().enumerate() {
                let p = |i: u32| mesh.positions.get(i as usize).copied();
                let (Some(a), Some(b), Some(c)) = (p(t[0]), p(t[1]), p(t[2])) else {
                    continue;
                };
                if [a, b, c].iter().flatten().any(|x| !x.is_finite())
                    || norm(cross(sub(b, a), sub(c, a))) == 0.0
                {
                    continue;
                }
                triangles.push([a, b, c]);
                solid.push(s as u32);
                label.push(mesh.labels.get(k).copied().unwrap_or(crate::NO_LABEL));
            }
        }
        Surfaces {
            bvh: Bvh::new(triangles),
            solid,
            label,
        }
    }

    /// The solid and the label of the hierarchy's triangle `triangle`.
    pub fn solid_and_label(&self, triangle: u32) -> Option<(u32, u32)> {
        let t = triangle as usize;
        Some((*self.solid.get(t)?, *self.label.get(t)?))
    }

    /// The solids containing each point along a line, by the parity of crossings: per
    /// point, the lowest-numbered solid containing it (`NONE` outside). `None` when the
    /// line's crossings do not pair up (a hole in a surface, or a graze).
    pub fn along_line(
        &self,
        start: V3,
        step: V3,
        points: usize,
        hits: &mut Vec<Hit>,
    ) -> Option<Vec<u32>> {
        // From a step before the first point to a step after the last.
        let p = sub(start, step);
        let q = add(start, scale(step, points as f64));
        self.bvh.segment_hits(p, q, hits);
        let span = (points + 1) as f64;
        let mut crossings: Vec<(u32, f64, bool)> = hits
            .iter()
            .filter_map(|h| Some((*self.solid.get(h.triangle as usize)?, h.t * span, h.leaving)))
            .collect();
        crossings.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        // A line through an edge or a corner of the surface mesh hits each triangle
        // there: those count once.
        let mut events: Vec<(f64, u32)> = Vec::with_capacity(crossings.len());
        let mut last: Option<(u32, f64, bool)> = None;
        for c in crossings {
            if let Some(l) = last
                && l.0 == c.0
                && l.2 == c.2
                && (c.1 - l.1).abs() < 1e-7
            {
                continue;
            }
            events.push((c.1, c.0));
            last = Some(c);
        }
        events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut inside: BTreeSet<u32> = BTreeSet::new();
        let mut out = vec![NONE; points];
        let mut e = 0;
        for (i, o) in out.iter_mut().enumerate() {
            // Point i sits at 1 + i steps from the line's start.
            let at = 1.0 + i as f64;
            while e < events.len() && events[e].0 < at {
                let s = events[e].1;
                if !inside.remove(&s) {
                    inside.insert(s);
                }
                e += 1;
            }
            *o = inside.first().copied().unwrap_or(NONE);
        }
        while e < events.len() {
            let s = events[e].1;
            if !inside.remove(&s) {
                inside.insert(s);
            }
            e += 1;
        }
        inside.is_empty().then_some(out)
    }

    /// Whether `p` is inside any solid (along +X, and then the other axes if that line
    /// does not pair up).
    pub fn contains(&self, p: V3, reach: f64, hits: &mut Vec<Hit>) -> bool {
        self.containing(p, reach, hits) != NONE
    }

    /// The lowest-numbered solid containing `p`, `u32::MAX` for none (along +X, and then
    /// the other axes if that line does not pair up).
    pub fn containing(&self, p: V3, reach: f64, hits: &mut Vec<Hit>) -> u32 {
        for axis in 0..3 {
            let mut step = [0.0; 3];
            step[axis] = reach;
            if let Some(states) = self.along_line(p, step, 1, hits) {
                return states.first().copied().unwrap_or(NONE);
            }
        }
        NONE
    }
}

/// A closed box surface, its sides labelled 0 to 5: −X, +X, −Y, +Y, −Z, +Z. Tests and
/// examples build their solids from it.
pub fn box_surface(lo: V3, hi: V3) -> SurfaceMesh {
    let corner = |i: usize| {
        [
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        ]
    };
    // Quads counter-clockwise from outside, with their label.
    let quads: [([u32; 4], u32); 6] = [
        ([0, 4, 6, 2], 0),
        ([1, 3, 7, 5], 1),
        ([0, 1, 5, 4], 2),
        ([2, 6, 7, 3], 3),
        ([0, 2, 3, 1], 4),
        ([4, 5, 7, 6], 5),
    ];
    let mut s = SurfaceMesh {
        positions: (0..8).map(corner).collect(),
        ..SurfaceMesh::default()
    };
    for (q, label) in quads {
        s.triangles.push([q[0], q[1], q[2]]);
        s.triangles.push([q[0], q[2], q[3]]);
        s.labels.extend([label, label]);
    }
    s
}
