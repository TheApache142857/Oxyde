//! The design space as a grid of cubic voxels, and the fixtures and loads on its nodes.
//!
//! - **The box**: around the preserve bodies (and the starting shape), grown by the margin
//!   when there is no starting shape, and made symmetric about each symmetry plane, which
//!   then falls between two layers of voxels: the mirror image of a voxel is a voxel.
//! - **Each voxel** is decided by its centre: *preserve* inside a preserve body, else
//!   *obstacle* inside an obstacle or within its offset, else *outside* when there is a
//!   starting shape and it is not in it, else *design*. Inside is the lattice mesher's test
//!   (`mesher::Surfaces`: crossings counted along lines of voxel centres). A centre well
//!   inside both a preserve body and an obstacle (by a quarter of a voxel) is refused.
//! - **Load cases**: each case's fixtures and loads are laid on the nodes apart, the
//!   study's every fixture and load one case when it gives none.
//! - **Fixtures and loads** are on faces of the preserve bodies. Each face is sampled every
//!   half voxel; each sample lands in the voxel holding it, which becomes a preserve voxel
//!   (a thin preserve body has no voxel centre inside it), or, in an obstacle voxel, in the
//!   nearest preserve voxel around. A fixture holds the corner of that voxel nearest its
//!   sample (all three directions, or a roller's normal); a load spreads its sample's share
//!   (by area) over the voxel's corners by trilinear weights.

// Indices into the grid's arrays, sized here from its counts.
#![allow(clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::multigrid::{Dims, par_fill};
use super::{DesignSpace, GenerativeError, GenerativeStudy};
use crate::bvh::{Bvh, Hit};
use crate::math::{V3, add, cross, dot, norm, normalize, scale, sub};
use crate::mesher::Surfaces;
use crate::solve::{FixtureKind, LoadKind};
use crate::topology::Symmetry;

/// The most voxels a grid may have: about 600 bytes each while generating.
pub const MAX_VOXELS: usize = 4_000_000;
/// The voxels the resolution's density asks for, from coarse (0) to fine (1).
const COARSE_VOXELS: f64 = 20_000.0;
const FINE_VOXELS: f64 = 2_000_000.0;
const NONE: u32 = u32::MAX;

/// What a voxel of the design space is; the values are those of the files (FORMAT.md §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Voxel {
    /// Material may go there.
    Design = 0,
    /// Always material.
    Preserve = 1,
    /// Never material: in an obstacle, or within its offset.
    Obstacle = 2,
    /// Never material: out of the starting shape.
    Outside = 3,
}

impl Voxel {
    pub fn from_u8(v: u8) -> Option<Voxel> {
        Some(match v {
            0 => Voxel::Design,
            1 => Voxel::Preserve,
            2 => Voxel::Obstacle,
            3 => Voxel::Outside,
            _ => return None,
        })
    }
}

/// The design space on a grid of voxels, with the fixtures and loads on its nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    /// mm: the corner of the first voxel, the lowest coordinates.
    pub origin: V3,
    /// mm: a voxel's edge.
    pub size: f64,
    /// Voxels along X, Y and Z.
    pub counts: [usize; 3],
    /// Per voxel, x fastest: voxel (i, j, k) is `i + nx (j + ny k)`.
    pub kinds: Vec<Voxel>,
    /// Each load case's fixtures and loads on the nodes.
    pub(crate) cases: Vec<CaseNodes>,
    /// The voxels fixtures and loads act through, in order.
    pub(crate) anchored: Vec<u32>,
}

/// A load case on the grid's nodes.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct CaseNodes {
    /// Held nodes in order, each with the motions it keeps (`I` less the held directions).
    pub(crate) holds: Vec<(u32, [f64; 9])>,
    /// Loaded nodes in order, each with its force, N.
    pub(crate) forces: Vec<(u32, V3)>,
}

impl Grid {
    /// A grid read back from a file: no fixtures or loads, enough to draw outcomes.
    /// `None` when the kinds do not fill the counts.
    pub fn from_parts(
        origin: V3,
        size: f64,
        counts: [usize; 3],
        kinds: Vec<Voxel>,
    ) -> Option<Grid> {
        let total = counts.iter().try_fold(1usize, |p, &n| p.checked_mul(n))?;
        (size.is_finite()
            && size > 0.0
            && origin.iter().all(|x| x.is_finite())
            && total == kinds.len())
        .then_some(Grid {
            origin,
            size,
            counts,
            kinds,
            cases: Vec::new(),
            anchored: Vec::new(),
        })
    }

    pub fn voxel_count(&self) -> usize {
        self.counts.iter().product()
    }

    pub(crate) fn dims(&self) -> Dims {
        Dims { cells: self.counts }
    }

    /// The index of voxel `(i, j, k)`.
    pub fn index(&self, c: [usize; 3]) -> usize {
        c[0] + self.counts[0] * (c[1] + self.counts[1] * c[2])
    }

    pub(crate) fn coords(&self, v: usize) -> [usize; 3] {
        let n = self.counts;
        [v % n[0], (v / n[0]) % n[1], v / (n[0] * n[1])]
    }

    /// mm: the centre of voxel `v`.
    pub fn centre(&self, v: usize) -> V3 {
        let c = self.coords(v);
        std::array::from_fn(|a| self.origin[a] + (c[a] as f64 + 0.5) * self.size)
    }

    /// The voxel holding `p`, the nearest one when it is outside the grid.
    fn voxel_at(&self, p: V3) -> [usize; 3] {
        std::array::from_fn(|a| {
            let t = ((p[a] - self.origin[a]) / self.size).floor();
            if t.is_finite() && t > 0.0 {
                (t as usize).min(self.counts[a] - 1)
            } else {
                0
            }
        })
    }

    /// Lays the study's design space on a grid of `study.voxel_size`, and its fixtures and
    /// loads on the grid's nodes.
    pub fn build(
        study: &GenerativeStudy,
        threads: usize,
        cancel: &AtomicBool,
    ) -> Result<Grid, GenerativeError> {
        let space = &study.space;
        let h = study.voxel_size;
        if !(h.is_finite() && h > 0.0) {
            return Err(GenerativeError::BadVoxelSize);
        }
        let symmetry = study.symmetry.filter(|s| s.planes.contains(&true));
        let (lo, hi) = design_box(space, symmetry.as_ref()).ok_or(GenerativeError::NoPreserve)?;
        let mut origin = [0.0; 3];
        let mut counts = [0usize; 3];
        for a in 0..3 {
            match symmetry.filter(|s| s.planes[a]) {
                Some(s) => {
                    let p = s.point[a];
                    let half = ((p - lo[a]).max(hi[a] - p) / h).ceil().max(1.0);
                    if !(half.is_finite() && half < 1e7) {
                        return Err(GenerativeError::BadVoxelSize);
                    }
                    origin[a] = p - half * h;
                    counts[a] = 2 * half as usize;
                }
                None => {
                    let n = ((hi[a] - lo[a]) / h).ceil().max(1.0);
                    if !(n.is_finite() && n < 1e7) {
                        return Err(GenerativeError::BadVoxelSize);
                    }
                    origin[a] = 0.5 * (lo[a] + hi[a]) - 0.5 * n * h;
                    counts[a] = n as usize;
                }
            }
        }
        let total = counts.iter().product::<usize>();
        if total > MAX_VOXELS {
            return Err(GenerativeError::TooManyVoxels { count: total });
        }
        let mut grid = Grid {
            origin,
            size: h,
            counts,
            kinds: Vec::new(),
            cases: Vec::new(),
            anchored: Vec::new(),
        };
        grid.classify(space, threads, cancel)?;
        grid.anchor(study, cancel)?;
        if !grid.kinds.contains(&Voxel::Design) {
            return Err(GenerativeError::NoDesignSpace);
        }
        Ok(grid)
    }

    /// Decides every voxel by its centre.
    fn classify(
        &mut self,
        space: &DesignSpace,
        threads: usize,
        cancel: &AtomicBool,
    ) -> Result<(), GenerativeError> {
        const PRESERVE: u8 = 1;
        const OBSTACLE: u8 = 2;
        const OFFSET: u8 = 4;
        const OUTSIDE: u8 = 8;
        let preserve = Surfaces::new(&space.preserve);
        let obstacle_surfaces: Vec<_> = space.obstacles.iter().map(|o| o.surface.clone()).collect();
        let obstacles = Surfaces::new(&obstacle_surfaces);
        let start = space
            .starting_shape
            .as_ref()
            .map(|s| Surfaces::new(std::slice::from_ref(s)));
        let (nx, h) = (self.counts[0], self.size);
        // A line from a point must reach past every surface for its crossings to pair up.
        let top: V3 = std::array::from_fn(|a| self.origin[a] + self.counts[a] as f64 * h);
        let every = space
            .preserve
            .iter()
            .chain(obstacle_surfaces.iter())
            .chain(space.starting_shape.iter())
            .flat_map(|s| s.positions.iter().copied())
            .chain([self.origin, top]);
        let reach = bounds(every).map_or(h, |(lo, hi)| 2.0 * norm(sub(hi, lo)) + h);
        let mut flags = vec![0u8; self.voxel_count()];
        let grid = &*self;
        par_fill(&mut flags, nx, threads, 4, |first, part| {
            let mut hits: Vec<Hit> = Vec::new();
            for (m, line) in part.chunks_exact_mut(nx).enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let v0 = (first + m) * nx;
                let start_point = grid.centre(v0);
                let step = [h, 0.0, 0.0];
                let inside = |s: &Surfaces, hits: &mut Vec<Hit>| -> Vec<bool> {
                    match s.along_line(start_point, step, nx, hits) {
                        Some(states) => states.iter().map(|&s| s != NONE).collect(),
                        None => (0..nx)
                            .map(|i| {
                                let p = add(start_point, scale(step, i as f64));
                                s.containing(p, reach, hits) != NONE
                            })
                            .collect(),
                    }
                };
                let p = inside(&preserve, &mut hits);
                let o = inside(&obstacles, &mut hits);
                let st = start.as_ref().map(|s| inside(s, &mut hits));
                for (i, f) in line.iter_mut().enumerate() {
                    *f = u8::from(p[i]) * PRESERVE
                        + u8::from(o[i]) * OBSTACLE
                        + u8::from(st.as_ref().is_some_and(|s| !s[i])) * OUTSIDE;
                }
            }
        });
        if cancel.load(Ordering::Relaxed) {
            return Err(GenerativeError::Cancelled);
        }
        // Offsets: centres within an obstacle's offset of its surface.
        for obstacle in &space.obstacles {
            let offset = obstacle.offset;
            if !(offset.is_finite() && offset > 0.0) {
                continue;
            }
            let triangles = triangles_of(&obstacle.surface);
            let Some((blo, bhi)) = bounds(triangles.iter().flatten().copied()) else {
                continue;
            };
            let bvh = Bvh::new(triangles);
            let lo = sub(blo, [offset; 3]);
            let hi = add(bhi, [offset; 3]);
            par_fill(&mut flags, nx, threads, 4, |first, part| {
                for (m, line) in part.chunks_exact_mut(nx).enumerate() {
                    let v0 = (first + m) * nx;
                    let c = grid.centre(v0);
                    if c[1] < lo[1] || c[1] > hi[1] || c[2] < lo[2] || c[2] > hi[2] {
                        continue;
                    }
                    for (i, f) in line.iter_mut().enumerate() {
                        let x = c[0] + i as f64 * h;
                        if x < lo[0] || x > hi[0] || *f & (OBSTACLE | PRESERVE) != 0 {
                            continue;
                        }
                        if bvh
                            .nearest([x, c[1], c[2]])
                            .is_some_and(|(_, d2)| d2 <= offset * offset)
                        {
                            *f |= OFFSET;
                        }
                    }
                }
            });
        }
        // A centre well inside a preserve body and an obstacle both.
        let mut hits = Vec::new();
        for (v, &f) in flags.iter().enumerate() {
            if f & (PRESERVE | OBSTACLE) != (PRESERVE | OBSTACLE) {
                continue;
            }
            let c = self.centre(v);
            let deep = |s: &Surfaces| {
                s.bvh
                    .nearest(c)
                    .is_some_and(|(_, d2)| d2 >= (0.25 * h) * (0.25 * h))
            };
            if deep(&preserve) && deep(&obstacles) {
                return Err(GenerativeError::ObstacleOverPreserve {
                    obstacle: obstacles.containing(c, reach, &mut hits) as usize,
                    preserve: preserve.containing(c, reach, &mut hits) as usize,
                });
            }
        }
        self.kinds = flags
            .iter()
            .map(|&f| {
                if f & PRESERVE != 0 {
                    Voxel::Preserve
                } else if f & (OBSTACLE | OFFSET) != 0 {
                    Voxel::Obstacle
                } else if f & OUTSIDE != 0 {
                    Voxel::Outside
                } else {
                    Voxel::Design
                }
            })
            .collect();
        Ok(())
    }

    /// The fixtures and loads, from samples of their faces.
    fn anchor(
        &mut self,
        study: &GenerativeStudy,
        cancel: &AtomicBool,
    ) -> Result<(), GenerativeError> {
        // Every face named, by label: the triangles of the preserve bodies it is.
        let mut faces: BTreeMap<u32, Vec<[V3; 3]>> = BTreeMap::new();
        let wanted: Vec<u32> = study
            .fixtures
            .iter()
            .flat_map(|f| f.labels.iter())
            .chain(study.loads.iter().flat_map(|l| l.labels.iter()))
            .copied()
            .collect();
        for mesh in &study.space.preserve {
            for (t, &label) in mesh.triangles.iter().zip(&mesh.labels) {
                if !wanted.contains(&label) {
                    continue;
                }
                let p = |i: u32| mesh.positions.get(i as usize).copied();
                if let (Some(a), Some(b), Some(c)) = (p(t[0]), p(t[1]), p(t[2])) {
                    faces.entry(label).or_default().push([a, b, c]);
                }
            }
        }
        if let Some(&label) = wanted.iter().find(|l| !faces.contains_key(l)) {
            return Err(GenerativeError::FaceNotOnPreserve { label });
        }
        let spacing = 0.5 * self.size;
        let dims = self.dims();
        // Each fixture's held nodes and each load's forces, apart: the cases mix them.
        let mut fixture_holds: Vec<BTreeMap<u32, (bool, Vec<V3>)>> = Vec::new();
        let mut load_forces: Vec<BTreeMap<u32, V3>> = Vec::new();
        let mut anchored: Vec<u32> = Vec::new();
        for fixture in &study.fixtures {
            let mut holds: BTreeMap<u32, (bool, Vec<V3>)> = BTreeMap::new();
            let fixed = fixture.kind == FixtureKind::Fixed;
            for label in &fixture.labels {
                for tri in faces.get(label).into_iter().flatten() {
                    let Some(normal) = normalize(cross(sub(tri[1], tri[0]), sub(tri[2], tri[0])))
                    else {
                        continue;
                    };
                    for (p, _) in samples(tri, spacing) {
                        let v = self.anchor_voxel(p);
                        anchored.push(v as u32);
                        let c = self.coords(v);
                        let k: [usize; 3] = std::array::from_fn(|a| {
                            let t = p[a] - (self.origin[a] + c[a] as f64 * self.size);
                            usize::from(t > 0.5 * self.size)
                        });
                        let node = dims.node([c[0] + k[0], c[1] + k[1], c[2] + k[2]]) as u32;
                        let entry = holds.entry(node).or_insert((false, Vec::new()));
                        entry.0 |= fixed;
                        if !fixed {
                            entry.1.push(normal);
                        }
                    }
                }
            }
            fixture_holds.push(holds);
            if cancel.load(Ordering::Relaxed) {
                return Err(GenerativeError::Cancelled);
            }
        }
        for load in &study.loads {
            let mut forces: BTreeMap<u32, V3> = BTreeMap::new();
            let triangles: Vec<&[V3; 3]> = load
                .labels
                .iter()
                .flat_map(|l| faces.get(l).into_iter().flatten())
                .collect();
            let area: f64 = triangles.iter().map(|t| triangle_area(t)).sum();
            if !area.is_finite() || area <= 0.0 {
                load_forces.push(forces);
                continue;
            }
            for tri in triangles {
                let Some(normal) = normalize(cross(sub(tri[1], tri[0]), sub(tri[2], tri[0])))
                else {
                    continue;
                };
                // The traction, N/mm², as the static solver takes it.
                let traction = match load.kind {
                    LoadKind::Force { total, direction } => match direction.and_then(normalize) {
                        Some(d) => scale(d, total / area),
                        None => scale(normal, -total / area),
                    },
                    LoadKind::Pressure { value } => scale(normal, -value),
                };
                for (p, a) in samples(tri, spacing) {
                    let v = self.anchor_voxel(p);
                    anchored.push(v as u32);
                    let c = self.coords(v);
                    let t: [f64; 3] = std::array::from_fn(|x| {
                        ((p[x] - (self.origin[x] + c[x] as f64 * self.size)) / self.size)
                            .clamp(0.0, 1.0)
                    });
                    for corner in 0..8 {
                        let k = super::brick::corner(corner);
                        let w: f64 = (0..3)
                            .map(|x| if k[x] == 1 { t[x] } else { 1.0 - t[x] })
                            .product();
                        if w == 0.0 {
                            continue;
                        }
                        let node = dims.node([c[0] + k[0], c[1] + k[1], c[2] + k[2]]) as u32;
                        let f = forces.entry(node).or_insert([0.0; 3]);
                        *f = add(*f, scale(traction, a * w));
                    }
                }
            }
            load_forces.push(forces);
        }
        anchored.sort_unstable();
        anchored.dedup();
        self.anchored = anchored;
        self.cases = Vec::new();
        for case in study.cases() {
            let mut holds: BTreeMap<u32, (bool, Vec<V3>)> = BTreeMap::new();
            for map in case.fixtures.iter().filter_map(|&f| fixture_holds.get(f)) {
                for (node, (fixed, normals)) in map {
                    let entry = holds.entry(*node).or_insert((false, Vec::new()));
                    entry.0 |= fixed;
                    entry.1.extend(normals);
                }
            }
            let mut forces: BTreeMap<u32, V3> = BTreeMap::new();
            for map in case.loads.iter().filter_map(|&l| load_forces.get(l)) {
                for (node, f) in map {
                    let sum = forces.entry(*node).or_insert([0.0; 3]);
                    *sum = add(*sum, *f);
                }
            }
            let nodes = CaseNodes {
                holds: holds
                    .into_iter()
                    .map(|(node, (fixed, normals))| (node, kept_motions(fixed, &normals)))
                    .collect(),
                forces: forces.into_iter().filter(|(_, f)| norm(*f) > 0.0).collect(),
            };
            if nodes.holds.is_empty() {
                return Err(GenerativeError::NotHeld);
            }
            if nodes.forces.is_empty() {
                return Err(GenerativeError::NoLoads);
            }
            self.cases.push(nodes);
        }
        Ok(())
    }

    /// The voxel a sample of a fixture's or a load's face at `p` acts through: the one
    /// holding it, made a preserve voxel, or, when that is an obstacle, the nearest preserve
    /// voxel around it.
    fn anchor_voxel(&mut self, p: V3) -> usize {
        let c = self.voxel_at(p);
        let v = self.index(c);
        if self.kinds[v] == Voxel::Obstacle {
            let mut best: Option<(f64, usize)> = None;
            for dz in -1i64..=1 {
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        let at = [c[0] as i64 + dx, c[1] as i64 + dy, c[2] as i64 + dz];
                        if (0..3).any(|a| at[a] < 0 || at[a] >= self.counts[a] as i64) {
                            continue;
                        }
                        let w = self.index(at.map(|x| x as usize));
                        if self.kinds[w] != Voxel::Preserve {
                            continue;
                        }
                        let d = norm(sub(self.centre(w), p));
                        if best.is_none_or(|(b, _)| d < b) {
                            best = Some((d, w));
                        }
                    }
                }
            }
            if let Some((_, w)) = best {
                return w;
            }
        }
        self.kinds[v] = Voxel::Preserve;
        v
    }
}

/// The motions a held node keeps: none when a fixture holds it fixed, else the identity
/// less each roller face's normal (at most three directions, those `normals` add).
fn kept_motions(fixed: bool, normals: &[V3]) -> [f64; 9] {
    if fixed {
        return [0.0; 9];
    }
    let mut p = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let mut held: Vec<V3> = Vec::new();
    for &n in normals {
        // What of this normal the directions held so far do not cover.
        let rest = held.iter().fold(n, |r, d| sub(r, scale(*d, dot(r, *d))));
        if norm(rest) > 0.5
            && held.len() < 3
            && let Some(d) = normalize(rest)
        {
            held.push(d);
        }
    }
    for d in &held {
        for r in 0..3 {
            for c in 0..3 {
                p[r * 3 + c] -= d[r] * d[c];
            }
        }
    }
    p
}

/// The box the grid covers before it is cut into voxels and made symmetric: around the
/// preserve bodies and the starting shape, grown by the margin when there is no starting
/// shape, then grown to be symmetric about each plane of `symmetry`. `None` without any
/// preserve body.
pub fn design_box(space: &DesignSpace, symmetry: Option<&Symmetry>) -> Option<(V3, V3)> {
    let points = space
        .preserve
        .iter()
        .flat_map(|s| s.positions.iter())
        .chain(space.starting_shape.iter().flat_map(|s| s.positions.iter()))
        .copied()
        .filter(|p| p.iter().all(|x| x.is_finite()));
    let (mut lo, mut hi) = bounds(points)?;
    if space.preserve.iter().all(|s| s.triangles.is_empty()) {
        return None;
    }
    if space.starting_shape.is_none() {
        let largest = (0..3).map(|a| hi[a] - lo[a]).fold(0.0, f64::max);
        let margin = space
            .margin
            .filter(|m| m.is_finite() && *m >= 0.0)
            .unwrap_or(0.1 * largest);
        lo = sub(lo, [margin; 3]);
        hi = add(hi, [margin; 3]);
    }
    if let Some(s) = symmetry {
        for a in 0..3 {
            if s.planes[a] {
                let p = s.point[a];
                let (l, u) = (lo[a].min(2.0 * p - hi[a]), hi[a].max(2.0 * p - lo[a]));
                lo[a] = l;
                hi[a] = u;
            }
        }
    }
    Some((lo, hi))
}

/// The voxel edge, mm, for a resolution's `density` from coarse (0) to fine (1): about
/// 20 000 voxels in the box at 0, 200 000 at 0.5 and two million at 1.
pub fn voxel_size(space: &DesignSpace, symmetry: Option<&Symmetry>, density: f64) -> Option<f64> {
    let (lo, hi) = design_box(space, symmetry)?;
    let volume: f64 = (0..3).map(|a| (hi[a] - lo[a]).max(1e-9)).product();
    let d = density.clamp(0.0, 1.0);
    let count = COARSE_VOXELS * (FINE_VOXELS / COARSE_VOXELS).powf(d);
    let h = (volume / count).cbrt();
    (h.is_finite() && h > 0.0).then_some(h)
}

fn bounds(points: impl Iterator<Item = V3>) -> Option<(V3, V3)> {
    let mut out: Option<(V3, V3)> = None;
    for p in points {
        out = Some(match out {
            None => (p, p),
            Some((lo, hi)) => (
                std::array::from_fn(|a| lo[a].min(p[a])),
                std::array::from_fn(|a| hi[a].max(p[a])),
            ),
        });
    }
    out
}

/// A surface's triangles, by their corners.
pub(super) fn triangles_of(s: &crate::mesher::SurfaceMesh) -> Vec<[V3; 3]> {
    s.triangles
        .iter()
        .filter_map(|t| {
            let p = |i: u32| s.positions.get(i as usize).copied();
            Some([p(t[0])?, p(t[1])?, p(t[2])?])
        })
        .collect()
}

fn triangle_area(t: &[V3; 3]) -> f64 {
    0.5 * norm(cross(sub(t[1], t[0]), sub(t[2], t[0])))
}

/// Points spread over a triangle no further apart than `spacing`, each with its share of
/// the area: the centroids of the triangle cut into `n²` equal ones.
fn samples(t: &[V3; 3], spacing: f64) -> Vec<(V3, f64)> {
    let longest = [
        norm(sub(t[1], t[0])),
        norm(sub(t[2], t[1])),
        norm(sub(t[0], t[2])),
    ]
    .into_iter()
    .fold(0.0, f64::max);
    let n = (longest / spacing).ceil().clamp(1.0, 4096.0) as usize;
    let share = triangle_area(t) / (n * n) as f64;
    let at = |u: f64, v: f64| {
        let w = 1.0 - u - v;
        add(add(scale(t[0], w), scale(t[1], u)), scale(t[2], v))
    };
    let step = 1.0 / n as f64;
    let mut out = Vec::with_capacity(n * n);
    for i in 0..n {
        for j in 0..n - i {
            out.push((
                at((i as f64 + 1.0 / 3.0) * step, (j as f64 + 1.0 / 3.0) * step),
                share,
            ));
            if i + j + 2 <= n {
                out.push((
                    at((i as f64 + 2.0 / 3.0) * step, (j as f64 + 2.0 / 3.0) * step),
                    share,
                ));
            }
        }
    }
    out
}
