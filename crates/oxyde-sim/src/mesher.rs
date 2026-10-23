//! A tetrahedral mesher for closed triangle meshes: isosurface stuffing (Labelle and
//! Shewchuk, "Isosurface Stuffing: Fast Tetrahedral Meshes with Good Dihedral Angles",
//! 2007) on a cubic lattice split into Kuhn tetrahedra.
//!
//! 1. A lattice of spacing `element_size` covers the model with a cell to spare. Each
//!    lattice point is inside or outside, by the parity of surface crossings along
//!    lines of the lattice in the three directions (two of three decide, so a line
//!    grazing an edge of the surface mesh is outvoted).
//! 2. Each lattice edge with one end inside and one outside is cut where it meets the
//!    surface: an exact segment–triangle hit, which also gives the face it lies on.
//! 3. Lattice points close to a cut (within a quarter of the edge) move onto it, so no
//!    element is a sliver squeezed between a point and the surface.
//! 4. Each lattice tetrahedron keeps its inside part, split into tetrahedra by stencils:
//!    a tetrahedron, a pyramid or a prism, whose quadrilateral faces are split through
//!    their lowest-numbered corner, so neighbours agree and the mesh conforms.
//!
//! Every boundary point lies on the surface mesh. Features thinner than an element are
//! lost (the report's volume shows it) and sharp edges are bevelled at the scale of an
//! element: what Netgen would do better (`docs/SIMULATION.md` §2).
//!
//! Several solids are meshed each on its own by default ([`MeshOptions::separate_bodies`]):
//! one [`TetMesh`] whose bodies share no node, each with its own boundary where it touches
//! another, tied through contacts ([`crate::contact`]). Meshed together instead, solids that
//! touch share nodes (bonded), each element in the solid it lies in.

// Indices into the lattice and the mesh being made, all built here.
#![allow(clippy::indexing_slicing)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::bvh::{Bvh, Hit};
use crate::math::{V3, add, cross, lerp, norm, scale, sub, tet_volume6};
use crate::mesh::{TetMesh, kuhn_tets};

/// The boundary of one solid: a closed triangle mesh, its triangles counter-clockwise
/// seen from outside, each with the label of the model face it belongs to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SurfaceMesh {
    pub positions: Vec<V3>,
    pub triangles: Vec<[u32; 3]>,
    /// One per triangle.
    pub labels: Vec<u32>,
}

impl SurfaceMesh {
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshOptions {
    /// The lattice spacing, mm: about the length of an element's edges.
    pub element_size: f64,
    /// Ten-node elements (SolidWorks' *high quality*) rather than four-node ones.
    pub quadratic: bool,
    /// More nodes than this and the mesh is refused, to keep memory in bounds.
    pub max_nodes: usize,
    /// Each solid meshed on its own, its bodies sharing no node, so that contacts decide
    /// how they interact (the default); otherwise all together, touching solids sharing
    /// their nodes (bonded).
    pub separate_bodies: bool,
}

impl Default for MeshOptions {
    fn default() -> Self {
        Self {
            element_size: 1.0,
            quadratic: true,
            max_nodes: 1_500_000,
            separate_bodies: true,
        }
    }
}

/// What came out of the mesher.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshReport {
    pub element_size: f64,
    pub nodes: usize,
    pub elements: usize,
    /// The mesh's volume, mm³.
    pub volume: f64,
    /// The volume the surfaces enclose, mm³: the two differ by features lost or bevelled.
    pub model_volume: f64,
    /// Elements left out because they came out flat.
    pub dropped: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MeshError {
    /// No triangles to mesh.
    Empty,
    /// The element size is not a positive number.
    BadSize,
    /// The lattice would have this many points: the element size is too small.
    TooFine(u64),
    /// This many nodes, more than allowed.
    TooManyNodes(usize),
    /// No lattice point fell inside: the element size is too large for the model, or
    /// the surface is not closed.
    NothingInside,
    Cancelled,
}

impl std::fmt::Display for MeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeshError::Empty => write!(f, "there is nothing to mesh"),
            MeshError::BadSize => write!(f, "the element size must be a positive length"),
            MeshError::TooFine(n) => write!(
                f,
                "the element size is too small for the model ({:.0} million lattice points)",
                *n as f64 / 1e6
            ),
            MeshError::TooManyNodes(n) => write!(
                f,
                "the mesh would have {n} nodes, too many: choose a larger element size"
            ),
            MeshError::NothingInside => write!(
                f,
                "no element fits inside the model: choose a smaller element size"
            ),
            MeshError::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// The most lattice points the mesher works with (about 60 MB of bookkeeping).
const MAX_LATTICE: u64 = 8_000_000;
/// How close, as a fraction of a lattice edge, a point must be to a cut to move onto it.
const SNAP: f64 = 0.25;
/// The seven directions of a Kuhn lattice's edges from a point to its neighbours above.
const DIRS: [[usize; 3]; 7] = [
    [1, 0, 0],
    [0, 1, 0],
    [0, 0, 1],
    [1, 1, 0],
    [0, 1, 1],
    [1, 0, 1],
    [1, 1, 1],
];

const NONE: u32 = u32::MAX;

/// Where a lattice edge crosses the surface.
#[derive(Debug, Clone, Copy)]
struct Cut {
    /// The edge's ends (lattice indices, `a < b`).
    a: u32,
    b: u32,
    /// From `a` (0) to `b` (1).
    t: f64,
    point: V3,
    region: u32,
}

/// The lattice: points, their state and where they are.
struct Lattice {
    origin: V3,
    h: f64,
    /// Points along each axis.
    n: [usize; 3],
}

impl Lattice {
    fn index(&self, i: usize, j: usize, k: usize) -> u32 {
        (i + self.n[0] * (j + self.n[1] * k)) as u32
    }

    fn coords(&self, v: u32) -> [usize; 3] {
        let v = v as usize;
        let i = v % self.n[0];
        let j = (v / self.n[0]) % self.n[1];
        let k = v / (self.n[0] * self.n[1]);
        [i, j, k]
    }

    fn position(&self, v: u32) -> V3 {
        let c = self.coords(v);
        [
            self.origin[0] + c[0] as f64 * self.h,
            self.origin[1] + c[1] as f64 * self.h,
            self.origin[2] + c[2] as f64 * self.h,
        ]
    }

    fn count(&self) -> usize {
        self.n[0] * self.n[1] * self.n[2]
    }
}

/// The surfaces, gathered into one hierarchy.
pub(crate) struct Surfaces {
    pub(crate) bvh: Bvh,
    /// Per triangle of the hierarchy: its solid and its label.
    solid: Vec<u32>,
    label: Vec<u32>,
}

impl Surfaces {
    pub(crate) fn new<'a>(solids: impl IntoIterator<Item = &'a SurfaceMesh>) -> Surfaces {
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
    pub(crate) fn solid_and_label(&self, triangle: u32) -> Option<(u32, u32)> {
        let t = triangle as usize;
        Some((*self.solid.get(t)?, *self.label.get(t)?))
    }

    /// The solids containing each point along a line, by the parity of crossings: per
    /// point, the lowest-numbered solid containing it (`NONE` outside). `None` when the
    /// line's crossings do not pair up (a hole in a surface, or a graze).
    pub(crate) fn along_line(
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
    fn contains(&self, p: V3, reach: f64, hits: &mut Vec<Hit>) -> bool {
        self.containing(p, reach, hits) != NONE
    }

    /// The lowest-numbered solid containing `p`, `u32::MAX` for none (along +X, and then
    /// the other axes if that line does not pair up).
    pub(crate) fn containing(&self, p: V3, reach: f64, hits: &mut Vec<Hit>) -> u32 {
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

/// Fills the solids with tetrahedra. Element `e` of the result belongs to solid
/// `bodies[e]`; boundary facets carry the label of the surface triangle nearest them.
/// With [`MeshOptions::separate_bodies`] each solid is meshed on its own (a solid too thin
/// for any element is left out, as it would be meshed together). `progress` hears the
/// fraction done, from 0 to 1.
pub fn mesh_solids(
    solids: &[SurfaceMesh],
    options: &MeshOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<(TetMesh, MeshReport), MeshError> {
    if !options.separate_bodies || solids.len() <= 1 {
        return mesh_together(solids, options, cancel, progress);
    }
    if !(options.element_size.is_finite() && options.element_size > 0.0) {
        return Err(MeshError::BadSize);
    }
    let count = solids.len() as f64;
    let mut mesh = TetMesh::default();
    let mut dropped = 0;
    let mut first_error = None;
    for (s, solid) in solids.iter().enumerate() {
        let part = |f: f64| progress((s as f64 + f) / count);
        let one = MeshOptions {
            quadratic: false,
            ..*options
        };
        match mesh_together(std::slice::from_ref(solid), &one, cancel, &part) {
            Ok((mut piece, report)) => {
                piece.bodies.fill(s as u32);
                dropped += report.dropped;
                mesh.append(piece);
                if mesh.nodes.len() > options.max_nodes {
                    return Err(MeshError::TooManyNodes(mesh.nodes.len()));
                }
            }
            // A solid with nothing to mesh, or too thin for an element, drops out; the
            // report's volume shows it.
            Err(e @ (MeshError::Empty | MeshError::NothingInside)) => {
                first_error.get_or_insert(e);
            }
            Err(e) => return Err(e),
        }
    }
    if mesh.tets.is_empty() {
        return Err(first_error.unwrap_or(MeshError::Empty));
    }
    if options.quadratic {
        mesh.make_quadratic();
    }
    if mesh.nodes.len() > options.max_nodes {
        return Err(MeshError::TooManyNodes(mesh.nodes.len()));
    }
    progress(1.0);
    let report = MeshReport {
        element_size: options.element_size,
        nodes: mesh.nodes.len(),
        elements: mesh.tets.len(),
        volume: mesh.volume(),
        model_volume: solids.iter().map(SurfaceMesh::volume).sum(),
        dropped,
    };
    Ok((mesh, report))
}

/// Meshes the solids together, on one lattice: solids that touch share nodes.
fn mesh_together(
    solids: &[SurfaceMesh],
    options: &MeshOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<(TetMesh, MeshReport), MeshError> {
    let h = options.element_size;
    if !(h.is_finite() && h > 0.0) {
        return Err(MeshError::BadSize);
    }
    let surfaces = Surfaces::new(solids);
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for k in 0..surfaces.solid.len() {
        if let Some(t) = surfaces.bvh.triangle(k as u32) {
            for p in t {
                for a in 0..3 {
                    lo[a] = lo[a].min(p[a]);
                    hi[a] = hi[a].max(p[a]);
                }
            }
        }
    }
    if surfaces.solid.is_empty() || !lo[0].is_finite() {
        return Err(MeshError::Empty);
    }
    // A cell to spare all round, shifted by odd fractions so that lattice points do not
    // fall on the model's faces, which are often on round numbers.
    const SHIFT: V3 = [0.1234, 0.2345, 0.3456];
    let origin = [
        lo[0] - h * (1.0 + SHIFT[0]),
        lo[1] - h * (1.0 + SHIFT[1]),
        lo[2] - h * (1.0 + SHIFT[2]),
    ];
    let n = [0, 1, 2].map(|a| ((hi[a] - origin[a]) / h).ceil() as u64 + 2);
    let total = n[0].saturating_mul(n[1]).saturating_mul(n[2]);
    if total > MAX_LATTICE {
        return Err(MeshError::TooFine(total));
    }
    let lattice = Lattice {
        origin,
        h,
        n: n.map(|k| k as usize),
    };
    let points = lattice.count();
    let cancelled = || cancel.load(Ordering::Relaxed);

    // ---- 1. Inside or outside, two lines of three deciding.
    let mut votes = vec![0u8; points];
    let mut valid = vec![0u8; points];
    let mut region = vec![NONE; points];
    let mut hits = Vec::new();
    for axis in 0..3 {
        let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
        let mut step = [0.0; 3];
        step[axis] = h;
        for k in 0..lattice.n[c] {
            if cancelled() {
                return Err(MeshError::Cancelled);
            }
            for j in 0..lattice.n[b] {
                let mut ijk = [0usize; 3];
                ijk[b] = j;
                ijk[c] = k;
                let first = lattice.index(ijk[0], ijk[1], ijk[2]);
                let Some(states) =
                    surfaces.along_line(lattice.position(first), step, lattice.n[axis], &mut hits)
                else {
                    continue;
                };
                for (i, s) in states.into_iter().enumerate() {
                    ijk[axis] = i;
                    let v = lattice.index(ijk[0], ijk[1], ijk[2]) as usize;
                    valid[v] += 1;
                    if s != NONE {
                        votes[v] += 1;
                        if region[v] == NONE {
                            region[v] = s;
                        }
                    }
                }
            }
        }
        progress(0.1 * (axis + 1) as f64);
    }
    // +1 inside, −1 outside, 0 on the surface (once moved there).
    let mut state: Vec<i8> = (0..points)
        .map(|v| if 2 * votes[v] > valid[v] { 1 } else { -1 })
        .collect();
    drop(votes);
    drop(valid);
    if !state.contains(&1) {
        return Err(MeshError::NothingInside);
    }

    // ---- 2. Cuts on the edges between inside and outside.
    let mut cuts: Vec<Cut> = Vec::new();
    let mut cut_of: HashMap<(u32, u32), u32> = HashMap::new();
    for v in 0..points as u32 {
        if v % 65_536 == 0 && cancelled() {
            return Err(MeshError::Cancelled);
        }
        let c = lattice.coords(v);
        for d in DIRS {
            let w = [c[0] + d[0], c[1] + d[1], c[2] + d[2]];
            if w[0] >= lattice.n[0] || w[1] >= lattice.n[1] || w[2] >= lattice.n[2] {
                continue;
            }
            let w = lattice.index(w[0], w[1], w[2]);
            if state[v as usize] == state[w as usize] {
                continue;
            }
            let (pa, pb) = (lattice.position(v), lattice.position(w));
            let a_inside = state[v as usize] > 0;
            let (p_in, p_out) = if a_inside { (pa, pb) } else { (pb, pa) };
            surfaces.bvh.segment_hits(p_in, p_out, &mut hits);
            // The last crossing before the outside end: where the outside begins.
            let best = hits.iter().max_by(|x, y| x.t.total_cmp(&y.t)).copied();
            let (t_in, tri) = match best {
                Some(hit) => (hit.t, Some(hit.triangle)),
                // The votes and this edge disagree: halfway, on the nearest face.
                None => {
                    let mid = lerp(p_in, p_out, 0.5);
                    (0.5, surfaces.bvh.nearest(mid).map(|(t, _)| t))
                }
            };
            let point = lerp(p_in, p_out, t_in);
            let solid = tri
                .and_then(|t| surfaces.solid.get(t as usize).copied())
                .unwrap_or(0);
            let t = if a_inside { t_in } else { 1.0 - t_in };
            cut_of.insert((v, w), cuts.len() as u32);
            cuts.push(Cut {
                a: v,
                b: w,
                t,
                point,
                region: solid,
            });
        }
    }
    progress(0.4);

    // ---- 3. Points close to a cut move onto it, all at once.
    let mut snaps: BTreeMap<u32, (f64, u32)> = BTreeMap::new();
    for (k, cut) in cuts.iter().enumerate() {
        let length = norm(sub(lattice.position(cut.b), lattice.position(cut.a)));
        for (v, along) in [(cut.a, cut.t), (cut.b, 1.0 - cut.t)] {
            if along < SNAP {
                let d = along * length;
                let entry = snaps.entry(v).or_insert((f64::INFINITY, NONE));
                if d < entry.0 {
                    *entry = (d, k as u32);
                }
            }
        }
    }
    let mut moved: HashMap<u32, (V3, u32)> = HashMap::new();
    for (&v, &(_, k)) in &snaps {
        if let Some(cut) = cuts.get(k as usize) {
            state[v as usize] = 0;
            moved.insert(v, (cut.point, cut.region));
        }
    }
    drop(snaps);

    // ---- 4. The stencils, cell by cell.
    let lattice_key = |v: u32| u64::from(v);
    let cut_key = |k: u32| points as u64 + u64::from(k);
    let position_of = |key: u64| -> V3 {
        if key < points as u64 {
            let v = key as u32;
            moved.get(&v).map_or_else(|| lattice.position(v), |m| m.0)
        } else {
            cuts.get((key - points as u64) as usize)
                .map_or([0.0; 3], |c| c.point)
        }
    };
    let region_of = |key: u64| -> u32 {
        if key < points as u64 {
            let v = key as u32;
            match moved.get(&v) {
                Some(m) => m.1,
                None => region[v as usize],
            }
        } else {
            cuts.get((key - points as u64) as usize)
                .map_or(NONE, |c| c.region)
        }
    };
    let mut mesh = TetMesh::default();
    let mut node_of: HashMap<u64, u32> = HashMap::new();
    let mut dropped = 0usize;
    let min_volume6 = 1e-9 * h * h * h;
    let cells = [lattice.n[0] - 1, lattice.n[1] - 1, lattice.n[2] - 1];
    for k in 0..cells[2] {
        if cancelled() {
            return Err(MeshError::Cancelled);
        }
        progress(0.4 + 0.5 * k as f64 / cells[2].max(1) as f64);
        for j in 0..cells[1] {
            for i in 0..cells[0] {
                let corner = |d: [usize; 3]| lattice.index(i + d[0], j + d[1], k + d[2]);
                let all_out = (0..8).all(|b| {
                    let v = corner([b & 1, (b >> 1) & 1, (b >> 2) & 1]);
                    state[v as usize] < 0
                });
                if all_out {
                    continue;
                }
                for tet in kuhn_tets(corner) {
                    let states = tet.map(|v| state[v as usize]);
                    let cut = |a: usize, b: usize| -> Option<u64> {
                        let (x, y) = (tet[a].min(tet[b]), tet[a].max(tet[b]));
                        cut_of.get(&(x, y)).map(|&c| cut_key(c))
                    };
                    let Some(pieces) = stencil(&tet, &states, &cut, lattice_key) else {
                        dropped += 1;
                        continue;
                    };
                    let pieces = match pieces {
                        Pieces::Maybe(t) => {
                            // Four points on the surface: kept if its middle is inside.
                            let centre = scale(
                                t.iter().map(|&key| position_of(key)).fold([0.0; 3], add),
                                0.25,
                            );
                            if surfaces.contains(
                                centre,
                                2.0 * h * cells.len() as f64 * 1e3,
                                &mut hits,
                            ) {
                                vec![t]
                            } else {
                                Vec::new()
                            }
                        }
                        Pieces::Tets(t) => t,
                    };
                    for piece in pieces {
                        let p = piece.map(position_of);
                        if tet_volume6(p[0], p[1], p[2], p[3]).abs() < min_volume6 {
                            dropped += 1;
                            continue;
                        }
                        // The element's solid: its first point inside one, else the
                        // first on a surface.
                        let body = piece
                            .iter()
                            .map(|&key| region_of(key))
                            .find(|&r| r != NONE)
                            .unwrap_or(0);
                        let ids = piece.map(|key| {
                            *node_of.entry(key).or_insert_with(|| {
                                mesh.nodes.push(position_of(key));
                                (mesh.nodes.len() - 1) as u32
                            })
                        });
                        mesh.push_tet(ids, body);
                    }
                }
            }
        }
    }
    drop(node_of);
    if mesh.tets.is_empty() {
        return Err(MeshError::NothingInside);
    }

    // ---- 5. The boundary, labelled by the nearest face, and the edge nodes.
    let far = 0.5 * h;
    mesh.find_boundary(|c| {
        let centre = scale(add(add(c[0], c[1]), c[2]), 1.0 / 3.0);
        match surfaces.bvh.nearest(centre) {
            Some((tri, d2)) if d2 <= far * far => surfaces
                .label
                .get(tri as usize)
                .copied()
                .unwrap_or(crate::NO_LABEL),
            _ => crate::NO_LABEL,
        }
    });
    if options.quadratic {
        mesh.make_quadratic();
    }
    if mesh.nodes.len() > options.max_nodes {
        return Err(MeshError::TooManyNodes(mesh.nodes.len()));
    }
    progress(1.0);
    let report = MeshReport {
        element_size: h,
        nodes: mesh.nodes.len(),
        elements: mesh.tets.len(),
        volume: mesh.volume(),
        model_volume: solids.iter().map(SurfaceMesh::volume).sum(),
        dropped,
    };
    Ok((mesh, report))
}

/// What a lattice tetrahedron keeps.
enum Pieces {
    Tets(Vec<[u64; 4]>),
    /// All four corners on the surface: kept only if it lies inside.
    Maybe([u64; 4]),
}

/// The inside part of the lattice tetrahedron `tet` with corner `states`, as
/// tetrahedra of point keys. `None` when a cut it needs is missing.
fn stencil(
    tet: &[u32; 4],
    states: &[i8; 4],
    cut: &dyn Fn(usize, usize) -> Option<u64>,
    lattice_key: impl Fn(u32) -> u64,
) -> Option<Pieces> {
    let mut plus = Vec::with_capacity(4);
    let mut zero = Vec::with_capacity(4);
    let mut minus = Vec::with_capacity(4);
    for (i, &s) in states.iter().enumerate() {
        match s {
            1 => plus.push(i),
            0 => zero.push(i),
            _ => minus.push(i),
        }
    }
    let p = |i: usize| lattice_key(tet[i]);
    let whole = [p(0), p(1), p(2), p(3)];
    let pieces = match (plus.as_slice(), zero.as_slice(), minus.as_slice()) {
        ([], z, _) if z.len() == 4 => return Some(Pieces::Maybe(whole)),
        ([], _, _) => Vec::new(),
        (_, _, []) => vec![whole],
        (&[a], &[], &[m0, m1, m2]) => vec![[p(a), cut(a, m0)?, cut(a, m1)?, cut(a, m2)?]],
        (&[a], &[z0], &[m0, m1]) => vec![[p(a), p(z0), cut(a, m0)?, cut(a, m1)?]],
        (&[a], &[z0, z1], &[m0]) => vec![[p(a), p(z0), p(z1), cut(a, m0)?]],
        (&[a, b], &[], &[m0, m1]) => prism(
            [p(a), cut(a, m0)?, cut(a, m1)?],
            [p(b), cut(b, m0)?, cut(b, m1)?],
        ),
        (&[a, b], &[z0], &[m0]) => pyramid([p(a), p(b), cut(b, m0)?, cut(a, m0)?], p(z0)),
        (&[a, b, c], &[], &[m0]) => {
            prism([p(a), p(b), p(c)], [cut(a, m0)?, cut(b, m0)?, cut(c, m0)?])
        }
        _ => return None,
    };
    Some(Pieces::Tets(pieces))
}

/// A prism (bottom and top triangles, `bottom[i]` joined to `top[i]`) as three
/// tetrahedra, each quadrilateral split through its lowest key (Dompierre et al., "How to
/// subdivide pyramids, prisms and hexahedra into tetrahedra", 1999), so that a
/// neighbour sharing a quadrilateral splits it the same way.
fn prism(bottom: [u64; 3], top: [u64; 3]) -> Vec<[u64; 4]> {
    let all = [bottom[0], bottom[1], bottom[2], top[0], top[1], top[2]];
    let lowest = (0..6).min_by_key(|&i| all[i]).unwrap_or(0);
    let (b, t) = if lowest < 3 {
        (bottom, top)
    } else {
        (top, bottom)
    };
    let r = lowest % 3;
    let b = [b[r], b[(r + 1) % 3], b[(r + 2) % 3]];
    let t = [t[r], t[(r + 1) % 3], t[(r + 2) % 3]];
    // The quadrilateral away from b[0]: b1 b2 t2 t1.
    let quad_lowest = [b[1], b[2], t[2], t[1]].into_iter().min().unwrap_or(b[1]);
    if quad_lowest == b[1] || quad_lowest == t[2] {
        vec![
            [b[0], b[1], b[2], t[2]],
            [b[0], b[1], t[2], t[1]],
            [b[0], t[1], t[2], t[0]],
        ]
    } else {
        vec![
            [b[0], b[1], b[2], t[1]],
            [b[0], b[2], t[2], t[1]],
            [b[0], t[1], t[2], t[0]],
        ]
    }
}

/// A pyramid (base corners in order round the base, and the apex) as two tetrahedra, the
/// base split through its lowest key.
fn pyramid(base: [u64; 4], apex: u64) -> Vec<[u64; 4]> {
    let lowest = (0..4).min_by_key(|&i| base[i]).unwrap_or(0);
    if lowest % 2 == 0 {
        vec![
            [apex, base[0], base[1], base[2]],
            [apex, base[0], base[2], base[3]],
        ]
    } else {
        vec![
            [apex, base[1], base[2], base[3]],
            [apex, base[1], base[3], base[0]],
        ]
    }
}

/// A closed box surface, labelled by side as [`TetMesh::structured_box`] labels its
/// boundary, for tests.
#[cfg(test)]
pub(crate) fn box_surface(lo: V3, hi: V3) -> SurfaceMesh {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::dot;

    fn no_progress(_: f64) {}

    /// A closed cylinder along Z, `segments` round, labelled 0 for the side, 1 bottom,
    /// 2 top.
    fn cylinder(radius: f64, height: f64, segments: usize) -> SurfaceMesh {
        let mut s = SurfaceMesh::default();
        for k in 0..segments {
            let a = std::f64::consts::TAU * k as f64 / segments as f64;
            s.positions.push([radius * a.cos(), radius * a.sin(), 0.0]);
            s.positions
                .push([radius * a.cos(), radius * a.sin(), height]);
        }
        let (bottom, top) = (s.positions.len() as u32, s.positions.len() as u32 + 1);
        s.positions.push([0.0, 0.0, 0.0]);
        s.positions.push([0.0, 0.0, height]);
        for k in 0..segments as u32 {
            let next = (k + 1) % segments as u32;
            let (b0, t0, b1, t1) = (2 * k, 2 * k + 1, 2 * next, 2 * next + 1);
            s.triangles.push([b0, b1, t1]);
            s.triangles.push([b0, t1, t0]);
            s.labels.extend([0, 0]);
            s.triangles.push([bottom, b1, b0]);
            s.labels.push(1);
            s.triangles.push([top, t0, t1]);
            s.labels.push(2);
        }
        s
    }

    fn check_conforming(mesh: &TetMesh) {
        // Every interior face is shared by exactly two elements and every boundary
        // facet lies on the surface: the boundary is closed (each boundary edge on
        // exactly two facets).
        let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
        for f in &mesh.facets {
            let c = mesh.facet_corners(f);
            for (a, b) in [(c[0], c[1]), (c[1], c[2]), (c[2], c[0])] {
                *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
        assert!(
            edges.values().all(|&n| n == 2),
            "the boundary is not closed"
        );
    }

    #[test]
    fn a_box_is_filled_with_positive_elements() {
        let surface = box_surface([0.0; 3], [20.0, 10.0, 6.0]);
        assert!((surface.volume() - 1200.0).abs() < 1e-9);
        let options = MeshOptions {
            element_size: 2.0,
            quadratic: false,
            ..MeshOptions::default()
        };
        let cancel = AtomicBool::new(false);
        let (mesh, report) = mesh_solids(&[surface], &options, &cancel, &no_progress).unwrap();
        assert!(mesh.validate().is_ok(), "{:?}", mesh.validate());
        assert_eq!(report.elements, mesh.tets.len());
        // Bevelled edges and corners lose a little.
        assert!(report.volume < 1200.0 + 1e-6, "{report:?}");
        assert!(report.volume > 0.9 * 1200.0, "{report:?}");
        check_conforming(&mesh);
        // Each side's facets point out of it, taken together (the bevels along the
        // box's edges are terraced), and cover about its area.
        let mut area = [0.0; 6];
        let mut normal = [[0.0; 3]; 6];
        for f in &mesh.facets {
            assert_ne!(f.label, crate::NO_LABEL);
            let an = mesh.facet_area_normal(f);
            area[f.label as usize] += norm(an);
            normal[f.label as usize] = add(normal[f.label as usize], an);
        }
        for (label, n) in normal.iter().enumerate() {
            let axis = label / 2;
            let sign = if label % 2 == 0 { -1.0 } else { 1.0 };
            assert!(n[axis] * sign > 0.95 * norm(*n), "{label}: {n:?}");
        }
        assert!(area[0] > 0.8 * 60.0 && area[0] < 1.1 * 60.0, "{area:?}");
        assert!(area[5] > 0.8 * 200.0 && area[5] < 1.1 * 200.0, "{area:?}");
        // Every boundary point is on the box's surface.
        for f in &mesh.facets {
            for i in mesh.facet_corners(f) {
                let p = mesh.nodes[i as usize];
                let d = (0..3)
                    .map(|a| (p[a] - 0.0).abs().min((p[a] - [20.0, 10.0, 6.0][a]).abs()))
                    .fold(f64::INFINITY, f64::min);
                assert!(d < 1e-9, "{p:?}");
            }
        }
    }

    #[test]
    fn a_cylinder_meshes_to_its_volume_and_is_deterministic() {
        let surface = cylinder(5.0, 12.0, 64);
        let cancel = AtomicBool::new(false);
        let options = MeshOptions {
            element_size: 1.5,
            quadratic: true,
            ..MeshOptions::default()
        };
        let (mesh, report) = mesh_solids(
            std::slice::from_ref(&surface),
            &options,
            &cancel,
            &no_progress,
        )
        .unwrap();
        assert!(mesh.validate().is_ok());
        assert!(mesh.is_quadratic());
        let v = surface.volume();
        assert!((report.volume - v).abs() < 0.05 * v, "{report:?} vs {v}");
        check_conforming(&mesh);
        // The side's facets point away from the axis.
        let mut labels = [0usize; 3];
        for f in &mesh.facets {
            labels[f.label as usize] += 1;
            if f.label == 0 {
                let c = mesh.facet_corners(f).map(|i| mesh.nodes[i as usize]);
                let centre = scale(add(add(c[0], c[1]), c[2]), 1.0 / 3.0);
                let an = mesh.facet_area_normal(f);
                assert!(dot(an, [centre[0], centre[1], 0.0]) > 0.0);
            }
        }
        assert!(labels.iter().all(|&n| n > 0), "{labels:?}");
        let (again, _) = mesh_solids(&[surface], &options, &cancel, &no_progress).unwrap();
        assert_eq!(mesh, again);
    }

    #[test]
    fn touching_solids_share_nodes_and_keep_their_bodies() {
        let a = box_surface([0.0; 3], [10.0, 10.0, 10.0]);
        let b = box_surface([10.0, 0.0, 0.0], [20.0, 10.0, 10.0]);
        let cancel = AtomicBool::new(false);
        let options = MeshOptions {
            element_size: 2.0,
            quadratic: false,
            separate_bodies: false,
            ..MeshOptions::default()
        };
        let (mesh, _) = mesh_solids(&[a, b], &options, &cancel, &no_progress).unwrap();
        assert!(mesh.validate().is_ok());
        // One piece: no boundary facet on the plane where they meet, inside the box.
        for f in &mesh.facets {
            let c = mesh.facet_corners(f).map(|i| mesh.nodes[i as usize]);
            let on_seam = c.iter().all(|p| (p[0] - 10.0).abs() < 1e-9);
            let inner = c
                .iter()
                .all(|p| p[1] > 1.0 && p[1] < 9.0 && p[2] > 1.0 && p[2] < 9.0);
            assert!(!(on_seam && inner), "a facet on the seam");
        }
        for (e, t) in mesh.tets.iter().enumerate() {
            let x = t.iter().map(|&i| mesh.nodes[i as usize][0]).sum::<f64>() / 4.0;
            let expected = if x < 10.0 { 0 } else { 1 };
            // Elements across the seam may go either way.
            if (x - 10.0).abs() > 2.0 {
                assert_eq!(mesh.bodies[e], expected);
            }
        }
    }

    #[test]
    fn solids_meshed_apart_share_no_node_and_each_has_its_boundary() {
        let a = box_surface([0.0; 3], [10.0, 10.0, 10.0]);
        let mut b = box_surface([10.0, 0.0, 0.0], [20.0, 10.0, 10.0]);
        for l in &mut b.labels {
            *l += 10;
        }
        let cancel = AtomicBool::new(false);
        let options = MeshOptions {
            element_size: 2.0,
            ..MeshOptions::default()
        };
        assert!(options.separate_bodies);
        let (mesh, report) =
            mesh_solids(&[a.clone(), b.clone()], &options, &cancel, &no_progress).unwrap();
        assert!(mesh.validate().is_ok());
        assert!(mesh.is_quadratic());
        assert_eq!(report.nodes, mesh.nodes.len());
        // No node in both bodies.
        let mut body_of = vec![None; mesh.nodes.len()];
        for (e, t) in mesh.tets.iter().enumerate() {
            let (nodes, count) = mesh.element_nodes(e);
            for &i in &nodes[..count] {
                let seen = body_of[i as usize].get_or_insert(mesh.bodies[e]);
                assert_eq!(*seen, mesh.bodies[e], "node {i} of element {e} {t:?}");
            }
        }
        // Each body is closed on its own, and the seam has facets on both sides, labelled
        // by their own solid's face.
        check_conforming(&mesh);
        let mut seam = [0.0f64; 2];
        for f in &mesh.facets {
            let c = mesh.facet_corners(f).map(|i| mesh.nodes[i as usize]);
            if c.iter().all(|p| (p[0] - 10.0).abs() < 1e-9) {
                let body = mesh.facet_body(f) as usize;
                assert_eq!(f.label, [1, 10][body]);
                seam[body] += norm(mesh.facet_area_normal(f));
            }
        }
        // Less the bevels along the seam's rim, an element size wide.
        assert!(seam.iter().all(|&s| s > 0.6 * 100.0), "{seam:?}");
        // Deterministic, and each body as it would be on its own.
        let (again, _) = mesh_solids(&[a.clone(), b], &options, &cancel, &no_progress).unwrap();
        assert_eq!(mesh, again);
        let (alone, _) = mesh_solids(&[a], &options, &cancel, &no_progress).unwrap();
        assert_eq!(
            alone.tets.len(),
            mesh.bodies.iter().filter(|&&b| b == 0).count()
        );
    }

    #[test]
    fn too_fine_and_cancelled() {
        let surface = box_surface([0.0; 3], [1000.0, 1000.0, 1000.0]);
        let cancel = AtomicBool::new(false);
        let fine = MeshOptions {
            element_size: 1.0,
            ..MeshOptions::default()
        };
        assert!(matches!(
            mesh_solids(std::slice::from_ref(&surface), &fine, &cancel, &no_progress),
            Err(MeshError::TooFine(_))
        ));
        cancel.store(true, Ordering::Relaxed);
        let coarse = MeshOptions {
            element_size: 100.0,
            ..MeshOptions::default()
        };
        assert_eq!(
            mesh_solids(&[surface], &coarse, &cancel, &no_progress),
            Err(MeshError::Cancelled)
        );
    }
}
