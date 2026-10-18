//! A linear static study: the stiffness matrix of the mesh, fixtures and loads, the
//! displacements by preconditioned conjugate gradients, and the stresses.
//!
//! - **Elements**: four-node tetrahedra (constant strain, one integration point) or
//!   ten-node ones (four points, exact for straight-sided elements). The matrix is held
//!   in 3 × 3 blocks, one per pair of nodes sharing an element, and assembled on every
//!   core, each thread owning a range of rows.
//! - **Fixtures** are exact: a fixed node loses its three unknowns; a node on roller
//!   faces has its unknowns turned into a frame whose first directions are the faces'
//!   normals, and loses those.
//! - **Loads** are consistent with the elements: a uniform traction on a six-node
//!   triangle loads its edge nodes only (a third of the force each), a weight loads a
//!   ten-node element's corners by −1/20 and its edge nodes by 1/5 of its own weight.
//! - **The solve** is conjugate gradients preconditioned by smoothed-aggregation
//!   multigrid ([`crate::amg`]), built on the six rigid motions of the nodes, the matrix
//!   products on every core; the inverse of each node's diagonal block (block-Jacobi)
//!   takes over if the multigrid hierarchy cannot be built or does not converge. Before
//!   it, each connected piece of the mesh is checked for rigid motions its fixtures
//!   leave free, and pieces tied by contacts are checked together.
//! - **Contacts** between bodies meshed apart ([`crate::contact`]) are augmented
//!   Lagrangian ties: a penalty in the matrix, which stays symmetric positive definite,
//!   and multipliers in the loads, updated between solves until the ties hold (and, for
//!   separation and rough, until no node opens or closes).
//! - **Stresses** are evaluated at each element's nodes and averaged at the nodes.

// Indices into the mesh (checked by `TetMesh::validate` first) and into arrays sized here.
#![allow(clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::amg::{BsrRef, Multigrid};
use crate::contact::{self, Contact, ContactResult, ContactSolve, Multipliers, Tie};
use crate::material::Elastic;
use crate::math::{
    IDENTITY, M3, V3, add, cross, dot, inverse, mat_mul, mat_t_vec, mat_vec, norm, normalize,
    scale, sub, symmetric_eigenvalues, transpose,
};
use crate::mesh::{EDGES, TetMesh};

/// How a fixture holds its faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureKind {
    /// No motion at all (SolidWorks' *Fixed Geometry*).
    Fixed,
    /// No motion normal to the faces; free to slide along them (*Roller/Slider*).
    Roller,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fixture {
    pub kind: FixtureKind,
    /// The face labels it holds.
    pub labels: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoadKind {
    /// A total force in newtons, spread over the faces by area: along `direction` (any
    /// length), or, when `None`, normal to each face, pushing into it.
    Force { total: f64, direction: Option<V3> },
    /// MPa, normal to the faces, pushing into them when positive.
    Pressure { value: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Load {
    pub kind: LoadKind,
    pub labels: Vec<u32>,
}

/// What a linear static study holds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Study {
    pub fixtures: Vec<Fixture>,
    pub loads: Vec<Load>,
    /// The acceleration of gravity, m/s², or `None` for no weight.
    pub gravity: Option<V3>,
    /// Contacts between bodies meshed apart. Bodies meshed together share their nodes
    /// where they touch and need none.
    pub contacts: Vec<Contact>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolveOptions {
    /// The solve stops when the residual is this fraction of the loads.
    pub tolerance: f64,
    pub max_iterations: usize,
    /// Threads; 0 for every core.
    pub threads: usize,
}

impl Default for SolveOptions {
    fn default() -> Self {
        Self {
            tolerance: 1e-8,
            max_iterations: 40_000,
            threads: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SolveError {
    /// The mesh is not usable: why.
    Invalid(String),
    /// The material of this body is missing or not usable.
    BadMaterial {
        body: u32,
    },
    /// The piece of the mesh holding this body can move freely: it needs fixtures (or
    /// to touch a part that has some).
    NotHeld {
        body: u32,
    },
    /// The loads act on nothing: no face they name is on the mesh, and no weight.
    NoLoads,
    /// The solve did not reach its tolerance.
    NotConverged {
        iterations: usize,
        residual: f64,
    },
    Cancelled,
}

impl std::fmt::Display for SolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SolveError::Invalid(why) => write!(f, "the mesh cannot be used: {why}"),
            SolveError::BadMaterial { body } => {
                write!(f, "body {body} has no usable material properties")
            }
            SolveError::NotHeld { body } => write!(
                f,
                "body {body} is not held: it could move freely (add fixtures, or make it touch a held part)"
            ),
            SolveError::NoLoads => write!(f, "the loads act on no face of the mesh"),
            SolveError::NotConverged {
                iterations,
                residual,
            } => write!(
                f,
                "the solve did not converge ({iterations} iterations, residual {residual:.1e}): the model may be held too loosely"
            ),
            SolveError::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// The largest values of a study's results and where they are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    /// MPa, and the node.
    pub max_von_mises: (f64, u32),
    /// mm, and the node.
    pub max_displacement: (f64, u32),
    /// The lowest factor of safety and its node, when the yield strengths are known.
    pub min_factor_of_safety: Option<(f64, u32)>,
}

/// The results of a linear static study, per node of the mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct Results {
    /// mm.
    pub displacement: Vec<V3>,
    /// MPa, `[xx, yy, zz, xy, yz, zx]`, averaged over the elements around each node.
    pub stress: Vec<[f64; 6]>,
    pub von_mises: Vec<f64>,
    /// The lowest yield strength of the elements around each node, MPa (0 unknown).
    pub yield_strength: Vec<f64>,
    /// Conjugate gradient iterations, over every solve.
    pub iterations: usize,
    /// The residual reached, as a fraction of the loads.
    pub residual: f64,
    /// MPa, pressing positive (negative where a bonded contact pulls), at the nodes of
    /// the contacts' faces; empty when the study has no contacts.
    pub contact_pressure: Vec<f64>,
    /// One per contact of the study, in its order.
    pub contacts: Vec<ContactResult>,
    pub contact_solve: ContactSolve,
}

impl Results {
    /// The yield strength over the von Mises stress, as SolidWorks' default criterion;
    /// `None` where the yield strength is unknown. Unstressed nodes get infinity.
    pub fn factor_of_safety(&self, node: usize) -> Option<f64> {
        let y = *self.yield_strength.get(node)?;
        if y <= 0.0 {
            return None;
        }
        let s = *self.von_mises.get(node)?;
        Some(if s > 0.0 { y / s } else { f64::INFINITY })
    }

    /// The largest principal stress at each node, MPa.
    pub fn first_principal(&self) -> Vec<f64> {
        self.stress
            .iter()
            .map(|s| {
                let e = symmetric_eigenvalues([
                    [s[0], s[3], s[5]],
                    [s[3], s[1], s[4]],
                    [s[5], s[4], s[2]],
                ]);
                e.into_iter().fold(f64::NEG_INFINITY, f64::max)
            })
            .collect()
    }

    pub fn summary(&self) -> Summary {
        let arg_max = |values: &mut dyn Iterator<Item = f64>| {
            values.enumerate().fold((0.0f64, 0u32), |best, (i, v)| {
                if v > best.0 { (v, i as u32) } else { best }
            })
        };
        let max_von_mises = arg_max(&mut self.von_mises.iter().copied());
        let max_displacement = arg_max(&mut self.displacement.iter().map(|&u| norm(u)));
        let min_factor_of_safety = (0..self.von_mises.len())
            .filter_map(|i| self.factor_of_safety(i).map(|f| (f, i as u32)))
            .min_by(|a, b| a.0.total_cmp(&b.0));
        Summary {
            max_von_mises,
            max_displacement,
            min_factor_of_safety,
        }
    }
}

/// The von Mises stress of `[xx, yy, zz, xy, yz, zx]`.
pub(crate) fn von_mises(s: &[f64; 6]) -> f64 {
    let d = (s[0] - s[1]).powi(2) + (s[1] - s[2]).powi(2) + (s[2] - s[0]).powi(2);
    (0.5 * d + 3.0 * (s[3] * s[3] + s[4] * s[4] + s[5] * s[5])).sqrt()
}

// ---- Elements ---------------------------------------------------------------------------

/// The barycentric gradients and the volume of a straight-sided element.
pub(crate) struct Geometry {
    pub grad: [V3; 4],
    pub volume: f64,
}

pub(crate) fn geometry(mesh: &TetMesh, e: usize) -> Option<Geometry> {
    let t = mesh.tets[e];
    let x = t.map(|i| mesh.nodes[i as usize]);
    let (a, b, c) = (sub(x[1], x[0]), sub(x[2], x[0]), sub(x[3], x[0]));
    // Columns: the edges from corner 0.
    let j = [a[0], b[0], c[0], a[1], b[1], c[1], a[2], b[2], c[2]];
    let inv = inverse(&j)?;
    let g1 = [inv[0], inv[1], inv[2]];
    let g2 = [inv[3], inv[4], inv[5]];
    let g3 = [inv[6], inv[7], inv[8]];
    let g0 = scale(add(add(g1, g2), g3), -1.0);
    let volume = dot(cross(a, b), c) / 6.0;
    Some(Geometry {
        grad: [g0, g1, g2, g3],
        volume,
    })
}

/// The gradients of the element's shape functions at barycentric coordinates `l`.
pub(crate) fn shape_gradients(
    g: &Geometry,
    l: [f64; 4],
    quadratic: bool,
    out: &mut [V3; 10],
) -> usize {
    if !quadratic {
        out[..4].copy_from_slice(&g.grad);
        return 4;
    }
    for i in 0..4 {
        out[i] = scale(g.grad[i], 4.0 * l[i] - 1.0);
    }
    for (k, &(i, j)) in EDGES.iter().enumerate() {
        out[4 + k] = scale(add(scale(g.grad[j], l[i]), scale(g.grad[i], l[j])), 4.0);
    }
    10
}

/// Integration points (barycentric) and their weight as a fraction of the volume.
pub(crate) fn integration_points(quadratic: bool) -> Vec<([f64; 4], f64)> {
    if !quadratic {
        return vec![([0.25; 4], 1.0)];
    }
    let (a, b) = (0.585_410_196_624_968_5, 0.138_196_601_125_010_5);
    vec![
        ([a, b, b, b], 0.25),
        ([b, a, b, b], 0.25),
        ([b, b, a, b], 0.25),
        ([b, b, b, a], 0.25),
    ]
}

/// The element's barycentric coordinates at each of its nodes.
pub(crate) fn node_points(quadratic: bool) -> Vec<[f64; 4]> {
    let mut out: Vec<[f64; 4]> = (0..4)
        .map(|i| {
            let mut l = [0.0; 4];
            l[i] = 1.0;
            l
        })
        .collect();
    if quadratic {
        for &(i, j) in &EDGES {
            let mut l = [0.0; 4];
            l[i] = 0.5;
            l[j] = 0.5;
            out.push(l);
        }
    }
    out
}

/// Adds the element's stiffness block `(a, b)` contributions for every pair to `add`,
/// called with local node numbers and the 3 × 3 block.
fn element_stiffness(
    g: &Geometry,
    quadratic: bool,
    material: &Elastic,
    mut add_block: impl FnMut(usize, usize, &M3),
) {
    let (lambda, mu) = material.lame();
    let n = if quadratic { 10 } else { 4 };
    let mut blocks = [[0.0f64; 9]; 100];
    let mut grads = [[0.0; 3]; 10];
    for (l, w) in integration_points(quadratic) {
        shape_gradients(g, l, quadratic, &mut grads);
        let weight = w * g.volume;
        for a in 0..n {
            let ga = grads[a];
            for b in 0..n {
                let gb = grads[b];
                let ab = dot(ga, gb);
                let block = &mut blocks[a * 10 + b];
                for k in 0..3 {
                    for m in 0..3 {
                        let mut v = lambda * ga[k] * gb[m] + mu * ga[m] * gb[k];
                        if k == m {
                            v += mu * ab;
                        }
                        block[k * 3 + m] += weight * v;
                    }
                }
            }
        }
    }
    for a in 0..n {
        for b in 0..n {
            add_block(a, b, &blocks[a * 10 + b]);
        }
    }
}

// ---- The matrix ---------------------------------------------------------------------------

/// A symmetric matrix in 3 × 3 blocks, row by row.
struct BlockMatrix {
    row_start: Vec<usize>,
    columns: Vec<u32>,
    blocks: Vec<M3>,
}

impl BlockMatrix {
    /// The pattern: a block for every pair of nodes sharing an element or a contact tie.
    fn pattern(mesh: &TetMesh, ties: &[Tie]) -> BlockMatrix {
        let n = mesh.nodes.len();
        let mut pairs: Vec<u64> = Vec::new();
        for e in 0..mesh.tets.len() {
            let (nodes, count) = mesh.element_nodes(e);
            for &a in &nodes[..count] {
                for &b in &nodes[..count] {
                    pairs.push((u64::from(a) << 32) | u64::from(b));
                }
            }
        }
        for tie in ties {
            for (a, _) in tie.terms() {
                for (b, _) in tie.terms() {
                    pairs.push((u64::from(a) << 32) | u64::from(b));
                }
            }
        }
        pairs.sort_unstable();
        pairs.dedup();
        let mut row_start = vec![0usize; n + 1];
        for &p in &pairs {
            row_start[(p >> 32) as usize + 1] += 1;
        }
        for i in 0..n {
            row_start[i + 1] += row_start[i];
        }
        let columns = pairs.iter().map(|&p| p as u32).collect();
        let blocks = vec![[0.0; 9]; pairs.len()];
        BlockMatrix {
            row_start,
            columns,
            blocks,
        }
    }

    fn rows(&self) -> usize {
        self.row_start.len() - 1
    }

    /// Adds `block` (global directions) to block `(row, col)` of the matrix in the nodes'
    /// frames, leaving out the held directions.
    fn add_framed(&mut self, constraints: &Constraints, row: usize, col: u32, block: &M3) {
        let span = self.row_start[row]..self.row_start[row + 1];
        let Ok(k) = self.columns[span.clone()].binary_search(&col) else {
            return;
        };
        let col = col as usize;
        let (fr, fc) = (constraints.frame[row], constraints.frame[col]);
        let t = if fr.is_some() || fc.is_some() {
            mat_mul(
                &mat_mul(&transpose(&fr.unwrap_or(IDENTITY)), block),
                &fc.unwrap_or(IDENTITY),
            )
        } else {
            *block
        };
        let (hr, hc) = (constraints.held[row], constraints.held[col]);
        let slot = &mut self.blocks[span.start + k];
        for i in 0..3 {
            for j in 0..3 {
                if hr & (1 << i) == 0 && hc & (1 << j) == 0 {
                    slot[i * 3 + j] += t[i * 3 + j];
                }
            }
        }
    }

    /// Adds (`sign` 1) or takes away (−1) a contact tie's penalty, `ε w Gᵀ P G`.
    fn add_tie(&mut self, constraints: &Constraints, tie: &Tie, sign: f64) {
        let k = sign * tie.penalty * tie.area;
        let n = tie.normal;
        let p: M3 = if tie.kind.ties_tangent() {
            IDENTITY
        } else {
            [
                n[0] * n[0],
                n[0] * n[1],
                n[0] * n[2],
                n[1] * n[0],
                n[1] * n[1],
                n[1] * n[2],
                n[2] * n[0],
                n[2] * n[1],
                n[2] * n[2],
            ]
        };
        for (a, ca) in tie.terms() {
            for (b, cb) in tie.terms() {
                let block = p.map(|v| v * k * ca * cb);
                self.add_framed(constraints, a as usize, b, &block);
            }
        }
    }

    fn view(&self) -> BsrRef<'_> {
        BsrRef {
            r: 3,
            c: 3,
            row_start: &self.row_start,
            columns: &self.columns,
            values: self.blocks.as_flattened(),
        }
    }
}

pub(crate) fn thread_count(wanted: usize) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    if wanted == 0 {
        cores
    } else {
        wanted.min(cores)
    }
    .max(1)
}

/// Row ranges of about equal numbers of blocks, one per thread.
fn row_ranges(m: &BlockMatrix, threads: usize) -> Vec<(usize, usize)> {
    let rows = m.rows();
    let total = m.blocks.len().max(1);
    let mut out = Vec::with_capacity(threads);
    let mut start = 0;
    for t in 1..=threads {
        let target = total * t / threads;
        let mut end = start;
        while end < rows && (m.row_start[end] < target || t == threads) {
            end += 1;
        }
        out.push((start, end));
        start = end;
    }
    out.retain(|&(a, b)| b > a);
    out
}

/// `y = A x`, on several threads when it pays.
fn multiply(m: &BlockMatrix, ranges: &[(usize, usize)], x: &[V3], y: &mut [V3]) {
    let rows_of = |(r0, r1): (usize, usize), out: &mut [V3]| {
        for (row, o) in (r0..r1).zip(out.iter_mut()) {
            let mut sum = [0.0; 3];
            for k in m.row_start[row]..m.row_start[row + 1] {
                let v = mat_vec(&m.blocks[k], x[m.columns[k] as usize]);
                sum = add(sum, v);
            }
            *o = sum;
        }
    };
    if ranges.len() <= 1 || m.rows() < 4000 {
        rows_of((0, m.rows()), y);
        return;
    }
    std::thread::scope(|s| {
        let mut rest = y;
        let mut taken = 0;
        for &(r0, r1) in ranges {
            let (mine, tail) = std::mem::take(&mut rest).split_at_mut(r1 - taken);
            let mine = &mut mine[r0 - taken..];
            taken = r1;
            rest = tail;
            s.spawn(move || rows_of((r0, r1), mine));
        }
    });
}

// ---- Fixtures --------------------------------------------------------------------------

/// Each node's frame (columns: its local directions) and which of the local directions
/// are held.
struct Constraints {
    frame: Vec<Option<M3>>,
    held: Vec<u8>,
}

impl Constraints {
    fn of(mesh: &TetMesh, study: &Study) -> Constraints {
        let n = mesh.nodes.len();
        let mut fixed = vec![false; n];
        // Per node, clusters of roller normals (sum of unit normals).
        let mut normals: Vec<Vec<V3>> = vec![Vec::new(); n];
        for fixture in &study.fixtures {
            for f in mesh
                .facets
                .iter()
                .filter(|f| fixture.labels.contains(&f.label))
            {
                let nodes = facet_nodes(mesh, f);
                match fixture.kind {
                    FixtureKind::Fixed => {
                        for &i in &nodes {
                            fixed[i as usize] = true;
                        }
                    }
                    FixtureKind::Roller => {
                        let Some(normal) = normalize(mesh.facet_area_normal(f)) else {
                            continue;
                        };
                        for &i in &nodes {
                            let clusters = &mut normals[i as usize];
                            // Facets of one curved face differ by a few degrees: one
                            // direction for all of them.
                            match clusters
                                .iter_mut()
                                .find(|c| normalize(**c).is_some_and(|c| dot(c, normal) > 0.85))
                            {
                                Some(c) => *c = add(*c, normal),
                                None => clusters.push(normal),
                            }
                        }
                    }
                }
            }
        }
        let mut frame = vec![None; n];
        let mut held = vec![0u8; n];
        for i in 0..n {
            if fixed[i] {
                held[i] = 0b111;
                continue;
            }
            if normals[i].is_empty() {
                continue;
            }
            let mut basis: Vec<V3> = Vec::new();
            for c in &normals[i] {
                let mut v = normalize(*c).unwrap_or([1.0, 0.0, 0.0]);
                for b in &basis {
                    v = sub(v, scale(*b, dot(v, *b)));
                }
                if norm(v) > 0.1
                    && let Some(v) = normalize(v)
                {
                    basis.push(v);
                }
                if basis.len() == 3 {
                    break;
                }
            }
            let rank = basis.len();
            // Complete the frame.
            while basis.len() < 3 {
                let last = basis[basis.len() - 1];
                let candidate = match basis.len() {
                    1 => {
                        let helper = if last[0].abs() < 0.9 {
                            [1.0, 0.0, 0.0]
                        } else {
                            [0.0, 1.0, 0.0]
                        };
                        cross(last, helper)
                    }
                    _ => cross(basis[0], basis[1]),
                };
                basis.push(normalize(candidate).unwrap_or([0.0, 0.0, 1.0]));
            }
            let (e1, e2, e3) = (basis[0], basis[1], basis[2]);
            frame[i] = Some([
                e1[0], e2[0], e3[0], e1[1], e2[1], e3[1], e1[2], e2[2], e3[2],
            ]);
            held[i] = ((1u16 << rank) - 1) as u8;
        }
        Constraints { frame, held }
    }

    /// The global directions held at node `i`.
    fn directions(&self, i: usize) -> Vec<V3> {
        let frame = self.frame[i].unwrap_or(IDENTITY);
        (0..3)
            .filter(|k| self.held[i] & (1 << k) != 0)
            .map(|k| [frame[k], frame[3 + k], frame[6 + k]])
            .collect()
    }
}

/// Every node of a facet: its corners, and its edge nodes for ten-node elements.
pub(crate) fn facet_nodes(mesh: &TetMesh, f: &crate::Facet) -> Vec<u32> {
    let mut out = mesh.facet_corners(f).to_vec();
    if let Some(mids) = mesh.facet_edge_nodes(f) {
        out.extend(mids);
    }
    out
}

/// Connected pieces of the mesh: each node's piece.
pub(crate) fn pieces(mesh: &TetMesh) -> Vec<u32> {
    let n = mesh.nodes.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(parent: &mut [u32], mut i: u32) -> u32 {
        while parent[i as usize] != i {
            let up = parent[parent[i as usize] as usize];
            parent[i as usize] = up;
            i = up;
        }
        i
    }
    for e in 0..mesh.tets.len() {
        let (nodes, count) = mesh.element_nodes(e);
        for &other in &nodes[1..count] {
            let (a, b) = (find(&mut parent, nodes[0]), find(&mut parent, other));
            if a != b {
                parent[a.max(b) as usize] = a.min(b);
            }
        }
    }
    (0..n as u32).map(|i| find(&mut parent, i)).collect()
}

/// The first body whose piece of the mesh its fixtures leave free to move, if any: the
/// six rigid motions (three translations, three turns) of each piece against every held
/// direction. Pieces tied by contacts are checked together: a tie holds the difference
/// of the motions of its two sides, so a piece tied to a held one is held too (along the
/// tie's directions only, for sliding and separation).
fn free_piece(mesh: &TetMesh, constraints: &Constraints, ties: &[Tie]) -> Option<u32> {
    let piece = pieces(mesh);
    let mut roots: Vec<u32> = piece.clone();
    roots.sort_unstable();
    roots.dedup();
    let count = roots.len();
    let node_piece: Vec<usize> = piece
        .iter()
        .map(|&root| roots.binary_search(&root).unwrap_or(0))
        .collect();
    let mut lo = vec![[f64::INFINITY; 3]; count];
    let mut hi = vec![[f64::NEG_INFINITY; 3]; count];
    for (i, p) in mesh.nodes.iter().enumerate() {
        let k = node_piece[i];
        for a in 0..3 {
            lo[k][a] = lo[k][a].min(p[a]);
            hi[k][a] = hi[k][a].max(p[a]);
        }
    }
    let centre: Vec<V3> = (0..count).map(|k| scale(add(lo[k], hi[k]), 0.5)).collect();
    let size: Vec<f64> = (0..count)
        .map(|k| norm(sub(hi[k], lo[k])).max(1e-12))
        .collect();
    // The motion of piece `k`'s six rigid modes at `p` along `d`.
    let motion = |k: usize, p: V3, d: V3| -> [f64; 6] {
        let r = scale(sub(p, centre[k]), 1.0 / size[k]);
        let rn = cross(r, d);
        [d[0], d[1], d[2], rn[0], rn[1], rn[2]]
    };

    // Pieces tied together form a cluster.
    let mut parent: Vec<usize> = (0..count).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for tie in ties {
        let s = node_piece[tie.slave as usize];
        for (m, _) in tie.terms() {
            let (a, b) = (
                find(&mut parent, s),
                find(&mut parent, node_piece[m as usize]),
            );
            if a != b {
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    let cluster: Vec<usize> = (0..count).map(|k| find(&mut parent, k)).collect();
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (k, &c) in cluster.iter().enumerate() {
        members.entry(c).or_default().push(k);
    }
    let mut slot = vec![0usize; count];
    for list in members.values() {
        for (s, &k) in list.iter().enumerate() {
            slot[k] = s;
        }
    }
    // Clusters of more pieces than this are not checked (their matrix would be large):
    // a free one then shows as a solve that does not converge.
    const MAX_PIECES: usize = 200;
    let mut gram: BTreeMap<usize, Vec<f64>> = members
        .iter()
        .filter(|(_, list)| list.len() <= MAX_PIECES)
        .map(|(&c, list)| (c, vec![0.0; 36 * list.len() * list.len()]))
        .collect();
    let mut add_row = |c: usize, entries: &[(usize, f64)]| {
        let Some(g) = gram.get_mut(&c) else {
            return;
        };
        let dim = 6 * members.get(&c).map_or(0, Vec::len);
        for &(i, a) in entries {
            for &(j, b) in entries {
                g[i * dim + j] += a * b;
            }
        }
    };
    let mut entries: Vec<(usize, f64)> = Vec::new();
    for (i, p) in mesh.nodes.iter().enumerate() {
        if constraints.held[i] == 0 {
            continue;
        }
        let k = node_piece[i];
        for d in constraints.directions(i) {
            entries.clear();
            let m = motion(k, *p, d);
            entries.extend((0..6).map(|a| (6 * slot[k] + a, m[a])));
            add_row(cluster[k], &entries);
        }
    }
    for tie in ties {
        let c = cluster[node_piece[tie.slave as usize]];
        for d in tie.directions() {
            entries.clear();
            for (node, coefficient) in tie.terms() {
                let k = node_piece[node as usize];
                let m = motion(k, mesh.nodes[node as usize], d);
                for (a, value) in m.iter().enumerate() {
                    let index = 6 * slot[k] + a;
                    match entries.iter_mut().find(|e| e.0 == index) {
                        Some(e) => e.1 += coefficient * value,
                        None => entries.push((index, coefficient * value)),
                    }
                }
            }
            add_row(c, &entries);
        }
    }
    // The body of each piece's first element, to name it.
    let mut body = vec![None; count];
    for (e, t) in mesh.tets.iter().enumerate() {
        let k = node_piece[t[0] as usize];
        if body[k].is_none() {
            body[k] = Some(mesh.bodies[e]);
        }
    }
    for (c, g) in &gram {
        let Some(list) = members.get(c) else {
            continue;
        };
        // A cluster of nodes in no element has nothing to hold.
        if list.iter().all(|&k| body[k].is_none()) {
            continue;
        }
        if let Some(free) = free_unknown(g, 6 * list.len()) {
            let k = list[free / 6];
            if let Some(b) = body[k].or_else(|| list.iter().find_map(|&k| body[k])) {
                return Some(b);
            }
        }
    }
    None
}

/// A rigid motion left free by the symmetric positive semidefinite `g` (`dim × dim`): an
/// unknown its pivoted Cholesky factorization, after scaling to a unit diagonal, cannot
/// take as a pivot. `None` when `g` is positive definite.
fn free_unknown(g: &[f64], dim: usize) -> Option<usize> {
    let diagonal: Vec<f64> = (0..dim).map(|i| g[i * dim + i]).collect();
    let largest = diagonal.iter().fold(0.0f64, |m, &v| m.max(v));
    if !(largest > 0.0 && largest.is_finite()) {
        return Some(0);
    }
    if let Some(i) = diagonal
        .iter()
        .position(|&v| v.is_nan() || v <= 1e-14 * largest)
    {
        return Some(i);
    }
    let mut a: Vec<f64> = (0..dim * dim)
        .map(|k| g[k] / (diagonal[k / dim] * diagonal[k % dim]).sqrt())
        .collect();
    let mut done = vec![false; dim];
    for _ in 0..dim {
        let pivot = (0..dim)
            .filter(|&k| !done[k])
            .max_by(|&x, &y| a[x * dim + x].total_cmp(&a[y * dim + y]))?;
        let value = a[pivot * dim + pivot];
        if value.is_nan() || value <= 1e-9 {
            return Some(pivot);
        }
        done[pivot] = true;
        for i in 0..dim {
            if done[i] {
                continue;
            }
            let f = a[i * dim + pivot] / value;
            if f == 0.0 {
                continue;
            }
            for j in 0..dim {
                if !done[j] {
                    a[i * dim + j] -= f * a[pivot * dim + j];
                }
            }
        }
    }
    None
}

// ---- The study --------------------------------------------------------------------------

/// Solves the study on the mesh, `materials[b]` being body `b`'s. `progress` hears the
/// fraction done, from 0 to 1.
pub fn solve(
    mesh: &TetMesh,
    materials: &[Elastic],
    study: &Study,
    options: &SolveOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<Results, SolveError> {
    let problem = Problem {
        mesh,
        materials,
        study,
        options,
        strain: None,
    };
    solve_with(&problem, Preconditioner::Multigrid, cancel, progress)
}

/// Temperatures that expand the model: each node's, and the temperature at which the
/// model is free of stress, °C.
#[derive(Debug, Clone, PartialEq)]
pub struct ThermalStrain {
    pub temperature: Vec<f64>,
    pub reference: f64,
}

/// [`solve`] with the model expanded by `strain` (a thermal stress study): each element
/// grows by `α (T − T_ref)` in every direction, `α` its material's
/// [`Elastic::expansion`]. A part free to grow does so without stress; held, it is
/// stressed by `E α ΔT / (1 − 2ν)` when held all round.
pub fn solve_thermal_stress(
    mesh: &TetMesh,
    materials: &[Elastic],
    study: &Study,
    strain: &ThermalStrain,
    options: &SolveOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<Results, SolveError> {
    let problem = Problem {
        mesh,
        materials,
        study,
        options,
        strain: Some(strain),
    };
    solve_with(&problem, Preconditioner::Multigrid, cancel, progress)
}

/// What conjugate gradients are preconditioned with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Preconditioner {
    /// Smoothed-aggregation multigrid ([`crate::amg`]), falling back to block-Jacobi.
    Multigrid,
    /// The inverse of each node's diagonal block. Only the tests ask for it; a solve
    /// falls back to it on its own.
    #[cfg_attr(not(test), allow(dead_code))]
    BlockJacobi,
}

/// A study on a mesh, with its materials and the solver's options.
struct Problem<'a> {
    mesh: &'a TetMesh,
    materials: &'a [Elastic],
    study: &'a Study,
    options: &'a SolveOptions,
    /// Temperatures expanding the model, for a thermal stress study.
    strain: Option<&'a ThermalStrain>,
}

fn solve_with(
    problem: &Problem,
    preconditioner: Preconditioner,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<Results, SolveError> {
    let Problem {
        mesh,
        materials,
        study,
        options,
        strain,
    } = *problem;
    mesh.validate().map_err(SolveError::Invalid)?;
    if let Some(strain) = strain
        && (strain.temperature.len() != mesh.nodes.len()
            || !strain.reference.is_finite()
            || strain.temperature.iter().any(|t| !t.is_finite()))
    {
        return Err(SolveError::Invalid(
            "its temperatures do not match its nodes".to_owned(),
        ));
    }
    if mesh.tets.is_empty() {
        return Err(SolveError::Invalid("it has no elements".to_owned()));
    }
    for &b in &mesh.bodies {
        if !materials.get(b as usize).is_some_and(Elastic::is_valid) {
            return Err(SolveError::BadMaterial { body: b });
        }
    }
    let quadratic = mesh.is_quadratic();
    let n = mesh.nodes.len();
    let constraints = Constraints::of(mesh, study);
    let pairing = contact::pair(mesh, &study.contacts, materials);
    let ties = &pairing.ties;
    if let Some(body) = free_piece(mesh, &constraints, ties) {
        return Err(SolveError::NotHeld { body });
    }
    let mut force = load_vector(mesh, materials, study);
    if let Some(strain) = strain {
        add_thermal_load(mesh, materials, strain, &mut force);
    }
    let load_size = force.iter().map(|f| dot(*f, *f)).sum::<f64>().sqrt();
    if !(load_size > 0.0 && load_size.is_finite()) {
        return Err(SolveError::NoLoads);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(SolveError::Cancelled);
    }
    progress(0.02);

    // ---- The matrix, row ranges on threads.
    let mut matrix = BlockMatrix::pattern(mesh, ties);
    let threads = thread_count(options.threads);
    let ranges = row_ranges(&matrix, threads);
    let geometries: Vec<Option<Geometry>> =
        (0..mesh.tets.len()).map(|e| geometry(mesh, e)).collect();
    if geometries.iter().any(Option::is_none) {
        return Err(SolveError::Invalid("an element is flat".to_owned()));
    }
    {
        let BlockMatrix {
            row_start,
            columns,
            blocks,
        } = &mut matrix;
        let (row_start, columns) = (&*row_start, &*columns);
        let constraints = &constraints;
        let geometries = &geometries;
        std::thread::scope(|s| {
            let mut rest: &mut [M3] = blocks;
            let mut taken = 0usize;
            for &(r0, r1) in &ranges {
                let (b0, b1) = (row_start[r0], row_start[r1]);
                let (mine, tail) = std::mem::take(&mut rest).split_at_mut(b1 - taken);
                let mine = &mut mine[b0 - taken..];
                taken = b1;
                rest = tail;
                s.spawn(move || {
                    for e in 0..mesh.tets.len() {
                        let (nodes, count) = mesh.element_nodes(e);
                        let nodes = &nodes[..count];
                        if !nodes.iter().any(|&i| (r0..r1).contains(&(i as usize))) {
                            continue;
                        }
                        let Some(g) = &geometries[e] else {
                            continue;
                        };
                        let material = &materials[mesh.bodies[e] as usize];
                        element_stiffness(g, quadratic, material, |a, b, block| {
                            let (row, col) = (nodes[a] as usize, nodes[b]);
                            if !(r0..r1).contains(&row) {
                                return;
                            }
                            let span = row_start[row]..row_start[row + 1];
                            if let Ok(k) = columns[span.clone()].binary_search(&col) {
                                let slot = &mut mine[span.start + k - b0];
                                for (s, v) in slot.iter_mut().zip(block) {
                                    *s += v;
                                }
                            }
                        });
                    }
                    // Into each node's frame, and the held directions out.
                    for row in r0..r1 {
                        for k in row_start[row]..row_start[row + 1] {
                            let col = columns[k] as usize;
                            let block = &mut mine[k - b0];
                            let (fr, fc) = (constraints.frame[row], constraints.frame[col]);
                            if fr.is_some() || fc.is_some() {
                                let left = transpose(&fr.unwrap_or(IDENTITY));
                                *block = mat_mul(&mat_mul(&left, block), &fc.unwrap_or(IDENTITY));
                            }
                            let (hr, hc) = (constraints.held[row], constraints.held[col]);
                            if hr | hc != 0 {
                                for i in 0..3 {
                                    for j in 0..3 {
                                        if hr & (1 << i) != 0 || hc & (1 << j) != 0 {
                                            block[i * 3 + j] =
                                                if row == col && i == j { 1.0 } else { 0.0 };
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            }
        });
    }
    // The contacts' penalties, for the ties that start closed (all of them).
    let mut multipliers = Multipliers::new(ties);
    let mut in_matrix = multipliers.active.clone();
    for (tie, _) in ties.iter().zip(&in_matrix).filter(|(_, on)| **on) {
        matrix.add_tie(&constraints, tie, 1.0);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(SolveError::Cancelled);
    }
    progress(0.1);

    // ---- Solves: one without contacts; with them, augmented Lagrangian updates until
    // the ties hold, the matrix changing when a node opens or closes.
    let modes =
        (preconditioner == Preconditioner::Multigrid).then(|| rigid_motions(mesh, &constraints));
    let goal = options.tolerance.max(1e-15);
    let mut x = vec![[0.0; 3]; n];
    let mut displacement = vec![[0.0; 3]; n];
    let mut iterations = 0;
    let mut residual;
    let mut contact_solve = ContactSolve {
        converged: ties.is_empty(),
        ..ContactSolve::default()
    };
    let mut use_multigrid = modes.is_some();
    'matrices: loop {
        let mut linear = Linear::new(&matrix, &ranges, modes.as_deref(), use_multigrid, threads);
        if cancel.load(Ordering::Relaxed) {
            return Err(SolveError::Cancelled);
        }
        if contact_solve.iterations == 0 {
            progress(0.15);
        }
        loop {
            let mut f = force.clone();
            multipliers.add_forces(ties, &mut f);
            let rhs = into_frames(&constraints, &f);
            let solves = contact_solve.iterations;
            let stage = |done: f64| progress(solve_progress(!ties.is_empty(), solves, done));
            let (spent, reached) =
                linear.solve(&rhs, &mut x, goal, options.max_iterations, cancel, &stage);
            iterations += spent;
            residual = reached;
            match linear.stop.take() {
                None => {}
                Some(SolveError::NotConverged { residual, .. }) => {
                    return Err(SolveError::NotConverged {
                        iterations,
                        residual,
                    });
                }
                Some(other) => return Err(other),
            }
            use_multigrid = linear.multigrid.is_some();
            displacement = out_of_frames(&constraints, &x);
            if ties.is_empty() {
                break 'matrices;
            }
            contact_solve.iterations += 1;
            let largest = displacement.iter().map(|u| norm(*u)).fold(0.0, f64::max);
            let tolerance = CONTACT_TOLERANCE * largest;
            let (gap, penetration) = multipliers.update(ties, &displacement);
            contact_solve.violation = gap.max(penetration);
            // The ties hold for this state: then nodes open or close, and the matrix
            // changes, or the contacts have converged.
            if gap <= tolerance {
                if !multipliers.decide(&pairing, &displacement, tolerance) {
                    contact_solve.converged = true;
                    break 'matrices;
                }
                if contact_solve.rebuilds >= MAX_CONTACT_REBUILDS {
                    break 'matrices;
                }
                break;
            }
            if contact_solve.iterations >= MAX_CONTACT_SOLVES {
                break 'matrices;
            }
        }
        // A node opened or closed: its penalty in or out of the matrix.
        drop(linear);
        for (k, tie) in ties.iter().enumerate() {
            if in_matrix[k] != multipliers.active[k] {
                let sign = if multipliers.active[k] { 1.0 } else { -1.0 };
                matrix.add_tie(&constraints, tie, sign);
                in_matrix[k] = multipliers.active[k];
            }
        }
        contact_solve.rebuilds += 1;
    }
    let (contacts, contact_pressure) = if study.contacts.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        contact::outcome(mesh, &pairing, &multipliers)
    };

    // ---- Stresses at the nodes, averaged.
    let mut sum = vec![[0.0f64; 6]; n];
    let mut count = vec![0u32; n];
    let mut yield_strength = vec![f64::INFINITY; n];
    let points = node_points(quadratic);
    let mut grads = [[0.0; 3]; 10];
    for e in 0..mesh.tets.len() {
        let Some(g) = &geometries[e] else {
            continue;
        };
        let material = &materials[mesh.bodies[e] as usize];
        let (lambda, mu) = material.lame();
        let (nodes, n_nodes) = mesh.element_nodes(e);
        for (local, l) in points.iter().enumerate() {
            shape_gradients(g, *l, quadratic, &mut grads);
            let mut h = [[0.0f64; 3]; 3];
            for a in 0..n_nodes {
                let u = displacement[nodes[a] as usize];
                for i in 0..3 {
                    for j in 0..3 {
                        h[i][j] += u[i] * grads[a][j];
                    }
                }
            }
            let e = |i: usize, j: usize| 0.5 * (h[i][j] + h[j][i]);
            let trace = h[0][0] + h[1][1] + h[2][2];
            let node = nodes[local] as usize;
            // The expansion's share of the strain carries no stress.
            let thermal = strain.map_or(0.0, |t| {
                (3.0 * lambda + 2.0 * mu) * material.expansion * (t.temperature[node] - t.reference)
            });
            let s = [
                lambda * trace + 2.0 * mu * e(0, 0) - thermal,
                lambda * trace + 2.0 * mu * e(1, 1) - thermal,
                lambda * trace + 2.0 * mu * e(2, 2) - thermal,
                2.0 * mu * e(0, 1),
                2.0 * mu * e(1, 2),
                2.0 * mu * e(2, 0),
            ];
            for k in 0..6 {
                sum[node][k] += s[k];
            }
            count[node] += 1;
            yield_strength[node] = yield_strength[node].min(material.yield_strength);
        }
    }
    let stress: Vec<[f64; 6]> = sum
        .iter()
        .zip(&count)
        .map(|(s, &c)| {
            if c == 0 {
                [0.0; 6]
            } else {
                s.map(|v| v / f64::from(c))
            }
        })
        .collect();
    let von_mises = stress.iter().map(von_mises).collect();
    let yield_strength = yield_strength
        .into_iter()
        .map(|y| if y.is_finite() { y } else { 0.0 })
        .collect();
    progress(1.0);
    Ok(Results {
        displacement,
        stress,
        von_mises,
        yield_strength,
        iterations,
        residual,
        contact_pressure,
        contacts,
        contact_solve,
    })
}

/// The contacts' iterations stop when no constraint is violated by more than this
/// fraction of the largest displacement.
const CONTACT_TOLERANCE: f64 = 1e-5;
/// At most this many solves for the contacts, and this many changes of the matrix.
const MAX_CONTACT_SOLVES: usize = 100;
const MAX_CONTACT_REBUILDS: usize = 30;

/// The progress of solve `solve` (from 0) when `done` of it is done: the whole span for a
/// single solve; with contacts, each solve gets a smaller share of what is left.
fn solve_progress(contacts: bool, solve: usize, done: f64) -> f64 {
    if !contacts {
        return 0.15 + 0.8 * done;
    }
    let at = |k: usize| 1.0 - 0.6f64.powi(k.min(1000) as i32);
    let (a, b) = (at(solve), at(solve + 1));
    0.15 + 0.8 * (a + (b - a) * done)
}

/// Forces into the nodes' frames, held directions zeroed.
fn into_frames(constraints: &Constraints, force: &[V3]) -> Vec<V3> {
    force
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let mut v = match constraints.frame[i] {
                Some(fr) => mat_t_vec(&fr, *f),
                None => *f,
            };
            for (k, c) in v.iter_mut().enumerate() {
                if constraints.held[i] & (1 << k) != 0 {
                    *c = 0.0;
                }
            }
            v
        })
        .collect()
}

/// Displacements out of the nodes' frames, into global directions.
fn out_of_frames(constraints: &Constraints, x: &[V3]) -> Vec<V3> {
    x.iter()
        .enumerate()
        .map(|(i, u)| match constraints.frame[i] {
            Some(fr) => mat_vec(&fr, *u),
            None => *u,
        })
        .collect()
}

/// Solves with one matrix: conjugate gradients preconditioned by multigrid, or by
/// block-Jacobi when the multigrid hierarchy cannot be built or does not converge (and
/// from then on).
struct Linear<'a> {
    matrix: &'a BlockMatrix,
    ranges: &'a [(usize, usize)],
    multigrid: Option<Multigrid<'a>>,
    jacobi: Option<Vec<M3>>,
    /// Why the last solve stopped short, if it did.
    stop: Option<SolveError>,
}

impl<'a> Linear<'a> {
    fn new(
        matrix: &'a BlockMatrix,
        ranges: &'a [(usize, usize)],
        modes: Option<&[f64]>,
        use_multigrid: bool,
        threads: usize,
    ) -> Linear<'a> {
        let multigrid = match modes {
            Some(modes) if use_multigrid => {
                Multigrid::new(matrix.view(), modes, crate::amg::RIGID_MODES, threads).ok()
            }
            _ => None,
        };
        Linear {
            matrix,
            ranges,
            multigrid,
            jacobi: None,
            stop: None,
        }
    }

    /// Improves `x` (in the nodes' frames) towards the solution for `rhs`: the
    /// iterations spent and the residual reached. When it stops short, `stop` says why.
    fn solve(
        &mut self,
        rhs: &[V3],
        x: &mut [V3],
        goal: f64,
        max_iterations: usize,
        cancel: &AtomicBool,
        progress: &dyn Fn(f64),
    ) -> (usize, f64) {
        let cg = ConjugateGradients {
            matrix: self.matrix,
            ranges: self.ranges,
            rhs,
            goal,
            cancel,
            progress,
        };
        let mut spent = 0;
        if let Some(multigrid) = &mut self.multigrid {
            let limit = MULTIGRID_ITERATIONS.min(max_iterations);
            let outcome = {
                let mut precondition = |r: &[V3], z: &mut [V3]| {
                    multigrid.apply(r.as_flattened(), z.as_flattened_mut());
                };
                cg.run(x, limit, &mut precondition)
            };
            match outcome {
                Ok(done) => return done,
                Err(CgStop::Cancelled) => {
                    self.stop = Some(SolveError::Cancelled);
                    return (0, f64::INFINITY);
                }
                Err(CgStop::Stalled {
                    iterations: stalled,
                    ..
                }) => {
                    // Block-Jacobi takes over from where multigrid got to, unless that is
                    // not a number.
                    spent = stalled;
                    if !x.iter().flatten().all(|v| v.is_finite()) {
                        x.fill([0.0; 3]);
                    }
                    self.multigrid = None;
                }
            }
        }
        let matrix = self.matrix;
        let jacobi = self.jacobi.get_or_insert_with(|| {
            (0..matrix.rows())
                .map(|row| {
                    let span = matrix.row_start[row]..matrix.row_start[row + 1];
                    match matrix.columns[span.clone()].binary_search(&(row as u32)) {
                        Ok(k) => inverse(&matrix.blocks[span.start + k]).unwrap_or(IDENTITY),
                        Err(_) => IDENTITY,
                    }
                })
                .collect()
        });
        let mut precondition = |r: &[V3], z: &mut [V3]| {
            for ((z, r), d) in z.iter_mut().zip(r).zip(jacobi.iter()) {
                *z = mat_vec(d, *r);
            }
        };
        match cg.run(x, max_iterations.saturating_sub(spent), &mut precondition) {
            Ok((done, residual)) => (spent + done, residual),
            Err(CgStop::Cancelled) => {
                self.stop = Some(SolveError::Cancelled);
                (spent, f64::INFINITY)
            }
            Err(CgStop::Stalled {
                iterations,
                residual,
            }) => {
                self.stop = Some(SolveError::NotConverged {
                    iterations: spent + iterations,
                    residual,
                });
                (spent + iterations, residual)
            }
        }
    }
}

/// Iterations multigrid gets before block-Jacobi takes over: it needs tens, so hundreds
/// mean something is wrong with the hierarchy.
const MULTIGRID_ITERATIONS: usize = 500;

/// Why conjugate gradients stopped short of the tolerance.
enum CgStop {
    Cancelled,
    /// Out of iterations, or a breakdown (a matrix or preconditioner that is not
    /// positive definite).
    Stalled {
        iterations: usize,
        residual: f64,
    },
}

/// Preconditioned conjugate gradients on the assembled matrix.
struct ConjugateGradients<'a> {
    matrix: &'a BlockMatrix,
    ranges: &'a [(usize, usize)],
    rhs: &'a [V3],
    /// The residual to reach, as a fraction of the right-hand side.
    goal: f64,
    cancel: &'a AtomicBool,
    /// Hears the fraction of the way to the goal.
    progress: &'a dyn Fn(f64),
}

impl ConjugateGradients<'_> {
    /// Improves `x` until the residual is under the goal: the iterations it took and the
    /// residual reached.
    fn run(
        &self,
        x: &mut [V3],
        limit: usize,
        precondition: &mut dyn FnMut(&[V3], &mut [V3]),
    ) -> Result<(usize, f64), CgStop> {
        let n = x.len();
        let dot_all = |a: &[V3], b: &[V3]| a.iter().zip(b).map(|(x, y)| dot(*x, *y)).sum::<f64>();
        let rhs_size = dot_all(self.rhs, self.rhs).sqrt();
        if rhs_size <= 0.0 {
            x.fill([0.0; 3]);
            return Ok((0, 0.0));
        }
        let mut r = vec![[0.0; 3]; n];
        multiply(self.matrix, self.ranges, x, &mut r);
        for (r, b) in r.iter_mut().zip(self.rhs) {
            *r = sub(*b, *r);
        }
        let mut residual = dot_all(&r, &r).sqrt() / rhs_size;
        let mut z = vec![[0.0; 3]; n];
        precondition(&r, &mut z);
        let mut p = z.clone();
        let mut q = vec![[0.0; 3]; n];
        let mut rz = dot_all(&r, &z);
        let log_goal = (1.0 / self.goal).ln();
        let mut iterations = 0;
        while residual > self.goal {
            let stalled = CgStop::Stalled {
                iterations,
                residual,
            };
            if iterations >= limit || !(rz > 0.0 && rz.is_finite()) {
                return Err(stalled);
            }
            multiply(self.matrix, self.ranges, &p, &mut q);
            let pq = dot_all(&p, &q);
            if !(pq > 0.0 && pq.is_finite()) {
                // Only a matrix that is not positive definite does that: something moves
                // freely after all.
                return Err(stalled);
            }
            let alpha = rz / pq;
            for i in 0..n {
                x[i] = add(x[i], scale(p[i], alpha));
                r[i] = sub(r[i], scale(q[i], alpha));
            }
            residual = dot_all(&r, &r).sqrt() / rhs_size;
            iterations += 1;
            precondition(&r, &mut z);
            let rz_next = dot_all(&r, &z);
            let beta = rz_next / rz;
            rz = rz_next;
            for i in 0..n {
                p[i] = add(z[i], scale(p[i], beta));
            }
            if iterations % 4 == 0 {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(CgStop::Cancelled);
                }
                let done = ((1.0 / residual.max(self.goal)).ln() / log_goal).clamp(0.0, 1.0);
                (self.progress)(done);
            }
        }
        Ok((iterations, residual))
    }
}

/// The six rigid motions (three translations, three turns about the middle of the
/// mesh) at each node, in its frame, held directions zeroed: multigrid's near-null
/// space, `3 × 6` per node, row-major.
fn rigid_motions(mesh: &TetMesh, constraints: &Constraints) -> Vec<f64> {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in &mesh.nodes {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let centre = scale(add(lo, hi), 0.5);
    let mut out = vec![0.0; mesh.nodes.len() * 18];
    for (i, p) in mesh.nodes.iter().enumerate() {
        let r = sub(*p, centre);
        let motions: [V3; 6] = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, -r[2], r[1]],
            [r[2], 0.0, -r[0]],
            [-r[1], r[0], 0.0],
        ];
        for (j, v) in motions.iter().enumerate() {
            let v = match constraints.frame[i] {
                Some(frame) => mat_t_vec(&frame, *v),
                None => *v,
            };
            for d in 0..3 {
                if constraints.held[i] & (1 << d) == 0 {
                    out[(i * 3 + d) * 6 + j] = v[d];
                }
            }
        }
    }
    out
}

/// Adds to `force` the nodal forces that expand each element by `α ΔT`: `∫ ∇Nₐ σₜ dV`,
/// `σₜ = (3λ + 2μ) α ΔT` the hydrostatic stress that would hold the expansion back, with
/// the temperature interpolated by the element's shape functions.
fn add_thermal_load(
    mesh: &TetMesh,
    materials: &[Elastic],
    strain: &ThermalStrain,
    force: &mut [V3],
) {
    let quadratic = mesh.is_quadratic();
    let mut grads = [[0.0; 3]; 10];
    let mut shapes = [0.0; 10];
    let points = integration_points(quadratic);
    for e in 0..mesh.tets.len() {
        let material = &materials[mesh.bodies[e] as usize];
        if material.expansion == 0.0 {
            continue;
        }
        let Some(g) = geometry(mesh, e) else {
            continue;
        };
        let (lambda, mu) = material.lame();
        let (nodes, count) = mesh.element_nodes(e);
        for &(l, w) in &points {
            shape_gradients(&g, l, quadratic, &mut grads);
            crate::thermal::shape_values(l, quadratic, &mut shapes);
            let t: f64 = (0..count)
                .map(|a| shapes[a] * strain.temperature[nodes[a] as usize])
                .sum();
            let sigma = (3.0 * lambda + 2.0 * mu) * material.expansion * (t - strain.reference);
            let weight = w * g.volume * sigma;
            for a in 0..count {
                let i = nodes[a] as usize;
                force[i] = add(force[i], scale(grads[a], weight));
            }
        }
    }
}

/// The nodal forces of the loads and the weight, N, in global directions.
fn load_vector(mesh: &TetMesh, materials: &[Elastic], study: &Study) -> Vec<V3> {
    let n = mesh.nodes.len();
    let mut f = vec![[0.0; 3]; n];
    let quadratic = mesh.is_quadratic();
    for load in &study.loads {
        let facets: Vec<&crate::Facet> = mesh
            .facets
            .iter()
            .filter(|f| load.labels.contains(&f.label))
            .collect();
        let area: f64 = facets.iter().map(|f| norm(mesh.facet_area_normal(f))).sum();
        if facets.is_empty() || area <= 0.0 {
            continue;
        }
        for facet in facets {
            let an = mesh.facet_area_normal(facet);
            let a = norm(an);
            let Some(normal) = normalize(an) else {
                continue;
            };
            // The traction, N/mm².
            let traction = match load.kind {
                LoadKind::Force { total, direction } => match direction.and_then(normalize) {
                    Some(d) => scale(d, total / area),
                    None => scale(normal, -total / area),
                },
                LoadKind::Pressure { value } => scale(normal, -value),
            };
            let share = scale(traction, a / 3.0);
            let nodes = match mesh.facet_edge_nodes(facet) {
                Some(mids) if quadratic => mids,
                _ => mesh.facet_corners(facet),
            };
            for i in nodes {
                f[i as usize] = add(f[i as usize], share);
            }
        }
    }
    if let Some(g) = study.gravity {
        for e in 0..mesh.tets.len() {
            let Some(volume) = mesh.tet_volume(e) else {
                continue;
            };
            let density = materials
                .get(mesh.bodies[e] as usize)
                .map_or(0.0, |m| m.density);
            // kg/m³ × m/s² = N/m³ = 1e-9 N/mm³.
            let weight = scale(g, density * volume * 1e-9);
            let (nodes, count) = mesh.element_nodes(e);
            for (k, &i) in nodes[..count].iter().enumerate() {
                let share = match (quadratic, k < 4) {
                    (false, _) => 0.25,
                    (true, true) => -0.05,
                    (true, false) => 0.2,
                };
                f[i as usize] = add(f[i as usize], scale(weight, share));
            }
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEEL: Elastic = Elastic::new(200_000.0, 0.3, 250.0, 7850.0);

    fn run(mesh: &TetMesh, study: &Study) -> Results {
        let cancel = AtomicBool::new(false);
        solve(
            mesh,
            &[STEEL],
            study,
            &SolveOptions::default(),
            &cancel,
            &|_| {},
        )
        .unwrap()
    }

    fn run_with(mesh: &TetMesh, study: &Study, preconditioner: Preconditioner) -> Results {
        let cancel = AtomicBool::new(false);
        let problem = Problem {
            mesh,
            materials: &[STEEL],
            study,
            options: &SolveOptions::default(),
            strain: None,
        };
        solve_with(&problem, preconditioner, &cancel, &|_| {}).unwrap()
    }

    /// The node nearest `p`.
    fn node_at(mesh: &TetMesh, p: V3) -> usize {
        (0..mesh.nodes.len())
            .min_by(|&a, &b| norm(sub(mesh.nodes[a], p)).total_cmp(&norm(sub(mesh.nodes[b], p))))
            .unwrap()
    }

    #[test]
    fn a_bar_in_tension() {
        // 100 × 10 × 10 mm, held by rollers on its end (and two more faces, so it
        // cannot slide), 10 kN pulling the other end: σ = P/A = 100 MPa everywhere,
        // the end moves PL/EA = 0.05 mm, the sides draw in by ν.
        let mut mesh = TetMesh::structured_box([0.0; 3], [100.0, 10.0, 10.0], [10, 2, 2]);
        mesh.make_quadratic();
        let study = Study {
            fixtures: vec![Fixture {
                kind: FixtureKind::Roller,
                labels: vec![0, 2, 4],
            }],
            loads: vec![Load {
                kind: LoadKind::Force {
                    total: 10_000.0,
                    direction: Some([1.0, 0.0, 0.0]),
                },
                labels: vec![1],
            }],
            gravity: None,
            contacts: Vec::new(),
        };
        let r = run(&mesh, &study);
        for i in 0..mesh.nodes.len() {
            let x = mesh.nodes[i][0];
            assert!(
                (r.displacement[i][0] - 0.05 * x / 100.0).abs() < 1e-6,
                "{i}"
            );
            assert!((r.stress[i][0] - 100.0).abs() < 1e-3, "{:?}", r.stress[i]);
            assert!((r.von_mises[i] - 100.0).abs() < 1e-3);
        }
        let corner = node_at(&mesh, [100.0, 10.0, 10.0]);
        // Lateral strain −ν σ/E over 10 mm.
        assert!((r.displacement[corner][1] + 0.3 * 100.0 / 200_000.0 * 10.0).abs() < 1e-7);
        let s = r.summary();
        assert!((s.max_displacement.0 - norm(r.displacement[corner])).abs() < 1e-9);
        let fos = s.min_factor_of_safety.unwrap().0;
        assert!((fos - 2.5).abs() < 1e-4);
        assert!((r.first_principal()[corner] - 100.0).abs() < 1e-3);
    }

    #[test]
    fn a_pressure_on_the_end_compresses_the_bar() {
        let mesh = TetMesh::structured_box([0.0; 3], [50.0, 10.0, 10.0], [5, 2, 2]);
        let study = Study {
            fixtures: vec![Fixture {
                kind: FixtureKind::Roller,
                labels: vec![0, 2, 4],
            }],
            loads: vec![Load {
                kind: LoadKind::Pressure { value: 20.0 },
                labels: vec![1],
            }],
            gravity: None,
            contacts: Vec::new(),
        };
        // Linear elements are exact for a uniform stress too.
        let r = run(&mesh, &study);
        let end = node_at(&mesh, [50.0, 5.0, 5.0]);
        assert!((r.displacement[end][0] + 20.0 * 50.0 / 200_000.0).abs() < 1e-8);
        assert!((r.stress[end][0] + 20.0).abs() < 1e-6);
    }

    /// A cantilever 200 × 20 × 10 mm, fixed at x = 0, 1 kN down (−Y) on its free end.
    fn cantilever(n: [usize; 3], quadratic: bool) -> (TetMesh, Results) {
        let mut mesh = TetMesh::structured_box([0.0; 3], [200.0, 20.0, 10.0], n);
        if quadratic {
            mesh.make_quadratic();
        }
        let study = Study {
            fixtures: vec![Fixture {
                kind: FixtureKind::Fixed,
                labels: vec![0],
            }],
            loads: vec![Load {
                kind: LoadKind::Force {
                    total: 1000.0,
                    direction: Some([0.0, -1.0, 0.0]),
                },
                labels: vec![1],
            }],
            gravity: None,
            contacts: Vec::new(),
        };
        let r = run(&mesh, &study);
        (mesh, r)
    }

    #[test]
    fn a_cantilever_bends_as_beam_theory_says() {
        let (mesh, r) = cantilever([20, 4, 2], true);
        // δ = PL³ / 3EI with I = b h³ / 12 = 10 · 20³ / 12; plus shear, which adds
        // about 3 (1 + ν) (h/L)² / 2.5…: under 1 % here.
        let i = 10.0 * 20.0f64.powi(3) / 12.0;
        let expected = 1000.0 * 200.0f64.powi(3) / (3.0 * 200_000.0 * i);
        let tip = node_at(&mesh, [200.0, 10.0, 5.0]);
        let delta = -r.displacement[tip][1];
        assert!(
            (delta - expected).abs() < 0.03 * expected,
            "tip {delta} mm, beam theory {expected} mm"
        );
        // σ = M c / I at mid-span, top fibre: M = P · 100 mm, c = 10 mm: 150 MPa,
        // tension on top.
        let top = node_at(&mesh, [100.0, 20.0, 5.0]);
        let sigma = 1000.0 * 100.0 * 10.0 / i;
        assert!(
            (r.stress[top][0] - sigma).abs() < 0.05 * sigma,
            "σ {} MPa, beam theory {sigma} MPa",
            r.stress[top][0]
        );
        let bottom = node_at(&mesh, [100.0, 0.0, 5.0]);
        assert!((r.stress[bottom][0] + sigma).abs() < 0.05 * sigma);
        // The neutral axis carries no bending stress.
        let middle = node_at(&mesh, [100.0, 10.0, 5.0]);
        assert!(r.stress[middle][0].abs() < 0.05 * sigma);
        assert!(r.residual <= 1e-8);
    }

    #[test]
    fn a_cantilever_meshed_by_the_mesher_is_close_to_beam_theory() {
        // The lattice mesher bevels the beam's long edges, which lowers I by a few per
        // cent: the tip goes a little further than beam theory says.
        let surface = crate::mesher::box_surface([0.0; 3], [200.0, 20.0, 20.0]);
        let options = crate::MeshOptions {
            element_size: 4.0,
            ..crate::MeshOptions::default()
        };
        let cancel = AtomicBool::new(false);
        let (mesh, _) = crate::mesh_solids(&[surface], &options, &cancel, &|_| {}).unwrap();
        let study = Study {
            fixtures: vec![Fixture {
                kind: FixtureKind::Fixed,
                labels: vec![0],
            }],
            loads: vec![Load {
                kind: LoadKind::Force {
                    total: 1000.0,
                    direction: Some([0.0, -1.0, 0.0]),
                },
                labels: vec![1],
            }],
            gravity: None,
            contacts: Vec::new(),
        };
        let r = run(&mesh, &study);
        let i = 20.0f64.powi(4) / 12.0;
        let expected = 1000.0 * 200.0f64.powi(3) / (3.0 * 200_000.0 * i);
        let tip = node_at(&mesh, [200.0, 10.0, 10.0]);
        let delta = -r.displacement[tip][1];
        assert!(
            delta > 0.97 * expected && delta < 1.12 * expected,
            "tip {delta} mm, beam theory {expected} mm"
        );
        let top = node_at(&mesh, [100.0, 20.0, 10.0]);
        let sigma = 1000.0 * 100.0 * 10.0 / i;
        assert!(
            (r.stress[top][0] - sigma).abs() < 0.12 * sigma,
            "σ {} MPa, beam theory {sigma} MPa",
            r.stress[top][0]
        );
    }

    /// The speed of the solve on the mesher's cantilever, 20 × 20 × 200 mm, at the two
    /// element sizes `docs/SIMULATION.md` §6 quotes (about 20 000 and 70 000 nodes):
    /// `cargo test -p oxyde-sim --lib -- --ignored --nocapture solve_timings`.
    #[test]
    #[ignore = "a timing run, not a check"]
    fn solve_timings() {
        for size in [4.0, 2.5] {
            let surface = crate::mesher::box_surface([0.0; 3], [200.0, 20.0, 20.0]);
            let options = crate::MeshOptions {
                element_size: size,
                ..crate::MeshOptions::default()
            };
            let cancel = AtomicBool::new(false);
            let (mesh, _) = crate::mesh_solids(&[surface], &options, &cancel, &|_| {}).unwrap();
            let study = Study {
                fixtures: vec![Fixture {
                    kind: FixtureKind::Fixed,
                    labels: vec![0],
                }],
                loads: vec![Load {
                    kind: LoadKind::Force {
                        total: 1000.0,
                        direction: Some([0.0, -1.0, 0.0]),
                    },
                    labels: vec![1],
                }],
                gravity: None,
                contacts: Vec::new(),
            };
            let tip = node_at(&mesh, [200.0, 10.0, 10.0]);
            for preconditioner in [Preconditioner::Multigrid, Preconditioner::BlockJacobi] {
                let start = std::time::Instant::now();
                let r = run_with(&mesh, &study, preconditioner);
                eprintln!(
                    "element size {size}, {preconditioner:?}: {} nodes, {} iterations, {:.2} s, tip {:.6} mm, max von Mises {:.3} MPa",
                    mesh.nodes.len(),
                    r.iterations,
                    start.elapsed().as_secs_f64(),
                    -r.displacement[tip][1],
                    r.summary().max_von_mises.0
                );
            }
        }
    }

    #[test]
    fn multigrid_and_block_jacobi_agree() {
        // A beam held by rollers on three faces with its weight and a force on its end,
        // ten-node elements: the rollers' frames, held directions and both kinds of load.
        let mut mesh = TetMesh::structured_box([0.0; 3], [120.0, 12.0, 8.0], [30, 4, 3]);
        mesh.make_quadratic();
        let study = Study {
            fixtures: vec![
                Fixture {
                    kind: FixtureKind::Roller,
                    labels: vec![0, 2],
                },
                Fixture {
                    kind: FixtureKind::Fixed,
                    labels: vec![4],
                },
            ],
            loads: vec![Load {
                kind: LoadKind::Force {
                    total: 300.0,
                    direction: Some([0.3, -1.0, 0.2]),
                },
                labels: vec![1],
            }],
            gravity: Some([0.0, -9.81, 0.0]),
            contacts: Vec::new(),
        };
        let a = run_with(&mesh, &study, Preconditioner::Multigrid);
        let b = run_with(&mesh, &study, Preconditioner::BlockJacobi);
        let size = b.displacement.iter().map(|u| norm(*u)).fold(0.0, f64::max);
        for (u, v) in a.displacement.iter().zip(&b.displacement) {
            assert!(norm(sub(*u, *v)) < 1e-6 * size, "{u:?} vs {v:?}");
        }
        assert!(
            a.iterations * 4 < b.iterations,
            "{} vs {}",
            a.iterations,
            b.iterations
        );
    }

    #[test]
    fn linear_elements_are_too_stiff_in_bending() {
        let (mesh, r) = cantilever([20, 4, 2], false);
        let i = 10.0 * 20.0f64.powi(3) / 12.0;
        let expected = 1000.0 * 200.0f64.powi(3) / (3.0 * 200_000.0 * i);
        let tip = node_at(&mesh, [200.0, 10.0, 5.0]);
        let delta = -r.displacement[tip][1];
        // Shear locking: well under the right answer, which is why ten-node elements are
        // the default.
        assert!(
            delta < 0.8 * expected && delta > 0.1 * expected,
            "{delta} vs {expected}"
        );
    }

    #[test]
    fn weight_hangs_the_bar() {
        // A bar hanging from its top face: the stress at the top is ρ g L, the end
        // moves ρ g L² / 2E.
        let mut mesh = TetMesh::structured_box([0.0; 3], [10.0, 1000.0, 10.0], [2, 20, 2]);
        mesh.make_quadratic();
        let study = Study {
            fixtures: vec![Fixture {
                kind: FixtureKind::Roller,
                labels: vec![3, 0, 4],
            }],
            loads: Vec::new(),
            gravity: Some([0.0, -9.81, 0.0]),
            contacts: Vec::new(),
        };
        let r = run(&mesh, &study);
        let rho_g = 7850.0 * 9.81 * 1e-9;
        let bottom = node_at(&mesh, [10.0, 0.0, 10.0]);
        let expected = rho_g * 1000.0f64.powi(2) / (2.0 * 200_000.0);
        assert!(
            (r.displacement[bottom][1] + expected).abs() < 0.01 * expected,
            "{:?} vs {expected}",
            r.displacement[bottom]
        );
        let top = node_at(&mesh, [5.0, 1000.0, 5.0]);
        assert!((r.stress[top][1] - rho_g * 1000.0).abs() < 0.02 * rho_g * 1000.0);
    }

    #[test]
    fn a_free_body_is_refused_and_so_are_missing_loads() {
        let mesh = TetMesh::structured_box([0.0; 3], [10.0, 10.0, 10.0], [2, 2, 2]);
        let cancel = AtomicBool::new(false);
        let options = SolveOptions::default();
        let load = Load {
            kind: LoadKind::Pressure { value: 1.0 },
            labels: vec![1],
        };
        let free = Study {
            fixtures: Vec::new(),
            loads: vec![load.clone()],
            gravity: None,
            contacts: Vec::new(),
        };
        assert_eq!(
            solve(&mesh, &[STEEL], &free, &options, &cancel, &|_| {}),
            Err(SolveError::NotHeld { body: 0 })
        );
        // One roller face only: it can still slide and turn in that plane.
        let sliding = Study {
            fixtures: vec![Fixture {
                kind: FixtureKind::Roller,
                labels: vec![0],
            }],
            ..free.clone()
        };
        assert!(matches!(
            solve(&mesh, &[STEEL], &sliding, &options, &cancel, &|_| {}),
            Err(SolveError::NotHeld { .. })
        ));
        let unloaded = Study {
            fixtures: vec![Fixture {
                kind: FixtureKind::Fixed,
                labels: vec![0],
            }],
            loads: vec![Load {
                labels: vec![77],
                ..load
            }],
            gravity: None,
            contacts: Vec::new(),
        };
        assert_eq!(
            solve(&mesh, &[STEEL], &unloaded, &options, &cancel, &|_| {}),
            Err(SolveError::NoLoads)
        );
        let bad = Elastic::new(0.0, 0.3, 0.0, 0.0);
        assert_eq!(
            solve(&mesh, &[bad], &unloaded, &options, &cancel, &|_| {}),
            Err(SolveError::BadMaterial { body: 0 })
        );
    }

    #[test]
    fn a_heated_bar_grows_freely_and_pushes_when_held() {
        // A steel bar 100 K above its stress-free temperature.
        let steel = STEEL.with_expansion(12e-6);
        let mut mesh = TetMesh::structured_box([0.0; 3], [100.0, 10.0, 10.0], [10, 2, 2]);
        mesh.make_quadratic();
        let strain = ThermalStrain {
            temperature: vec![120.0; mesh.nodes.len()],
            reference: 20.0,
        };
        let cancel = AtomicBool::new(false);
        let solve_held = |fixtures: Vec<Fixture>| {
            let study = Study {
                fixtures,
                loads: Vec::new(),
                gravity: None,
                contacts: Vec::new(),
            };
            solve_thermal_stress(
                &mesh,
                &[steel],
                &study,
                &strain,
                &SolveOptions::default(),
                &cancel,
                &|_| {},
            )
            .unwrap()
        };
        // Held on one end only by rollers on three faces (free to grow, not to move):
        // it grows α ΔT L = 0.12 mm and carries no stress.
        let roller = |label| Fixture {
            kind: FixtureKind::Roller,
            labels: vec![label],
        };
        let free = solve_held(vec![roller(0), roller(2), roller(4)]);
        let tip = node_at(&mesh, [100.0, 10.0, 10.0]);
        assert!((free.displacement[tip][0] - 0.12).abs() < 1e-6);
        assert!((free.displacement[tip][1] - 12e-6 * 100.0 * 10.0).abs() < 1e-6);
        let largest = free.von_mises.iter().fold(0.0f64, |m, &s| m.max(s));
        assert!(largest < 1e-3, "a free bar is unstressed: {largest} MPa");
        // Both ends on rollers along X: it cannot grow along X, σxx = −E α ΔT = −240 MPa.
        let held = solve_held(vec![roller(0), roller(1), roller(2), roller(4)]);
        let mid = node_at(&mesh, [50.0, 5.0, 5.0]);
        assert!(
            (held.stress[mid][0] + 240.0).abs() < 1e-3 * 240.0,
            "{:?}",
            held.stress[mid]
        );
        assert!(held.stress[mid][1].abs() < 1e-3 && held.stress[mid][2].abs() < 1e-3);
        // Without expansion, nothing loads it.
        let study = Study {
            fixtures: vec![roller(0), roller(1), roller(2), roller(4)],
            loads: Vec::new(),
            gravity: None,
            contacts: Vec::new(),
        };
        assert_eq!(
            solve_thermal_stress(
                &mesh,
                &[STEEL],
                &study,
                &strain,
                &SolveOptions::default(),
                &cancel,
                &|_| {},
            ),
            Err(SolveError::NoLoads)
        );
    }

    #[test]
    fn a_normal_force_pushes_into_the_face() {
        // The same as a pressure of F / A.
        let mesh = TetMesh::structured_box([0.0; 3], [20.0, 10.0, 10.0], [4, 2, 2]);
        let fixtures = vec![Fixture {
            kind: FixtureKind::Fixed,
            labels: vec![0],
        }];
        let force = Study {
            fixtures: fixtures.clone(),
            loads: vec![Load {
                kind: LoadKind::Force {
                    total: 500.0,
                    direction: None,
                },
                labels: vec![1],
            }],
            gravity: None,
            contacts: Vec::new(),
        };
        let pressure = Study {
            fixtures,
            loads: vec![Load {
                kind: LoadKind::Pressure { value: 5.0 },
                labels: vec![1],
            }],
            gravity: None,
            contacts: Vec::new(),
        };
        let (a, b) = (run(&mesh, &force), run(&mesh, &pressure));
        for (u, v) in a.displacement.iter().zip(&b.displacement) {
            assert!(norm(sub(*u, *v)) < 1e-12);
        }
        assert!(a.displacement.iter().all(|u| u[0] <= 1e-12));
    }
}
