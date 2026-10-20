//! Conjugate gradients on the voxel grid, preconditioned by geometric multigrid.
//!
//! - **The finest level** is never assembled: `y = K x` is gathered node by node from the
//!   eight voxels around each node, each a [`Brick`] scaled by its stiffness. Memory goes as
//!   a few numbers per voxel, and each value is summed in the same order on any number of
//!   threads.
//! - **Fixtures** project: a held node keeps only the motions its fixtures allow (`P`, the
//!   identity less the held directions), and the operator is `P K P + d (I − P)`, `d` the
//!   node's own stiffness, so that the held directions keep a stiffness of the right size
//!   on the coarse levels instead of none. Conjugate gradients stay in the allowed motions:
//!   the preconditioner's output is projected. Nodes no voxel with material touches are
//!   left out the same way.
//! - **Coarse levels** halve the grid along each axis that has two voxels or more. Their
//!   matrices are the Galerkin products `Pᵀ K P` of trilinear interpolation, which is local
//!   to a coarse voxel: each coarse voxel's 24 × 24 matrix is the sum of its eight children's
//!   seen through the interpolation, exact and computed one coarse voxel at a time. They
//!   cope with solid next to void, where averaged moduli would not. Coarse matrices are
//!   kept in single precision: they only precondition.
//! - **Smoothing** is Chebyshev's on block Jacobi (the node's 3 × 3 diagonal block), as in
//!   [`crate::amg`], the same before and after the coarse correction so the V-cycle is
//!   symmetric. The finest level's interval comes from the brick's own bound; the coarse
//!   levels' from a few power iterations.
//! - **The coarsest level** (a couple of hundred nodes) is factored densely.

// Indices into the grid's arrays, sized here from its dimensions.
#![allow(clippy::indexing_slicing)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::GenerativeError;
use super::brick::{Brick, DOFS, SIZE, corner, invert3};

const NONE: u32 = u32::MAX;
/// A level of at most this many nodes is factored and solved directly.
const COARSEST_NODES: usize = 220;
/// Chebyshev smoothing: degree, and how far under the top of the spectrum it damps.
const DEGREE: usize = 2;
const SPECTRUM_RATIO: f64 = 30.0;
/// Over the finest level's bound (the brick's own), and over the coarse levels' estimate.
const SPECTRUM_MARGIN: f64 = 1.1;
const COARSE_MARGIN: f64 = 1.2;
const POWER_STEPS: usize = 20;
/// A run of an output and the index of its first record, for the task that takes it.
type Slot<'a, T> = Mutex<Option<(usize, &'a mut [T])>>;

/// Calls `f(first, part)` on `out` cut into up to `threads` runs of whole records of
/// `width` values, `first` the index of a run's first record, each run `least` records at
/// least so the threads pay for themselves. Each record must depend on its index only, so
/// the result is the same on any number of threads.
pub(crate) fn par_fill<T: Send>(
    out: &mut [T],
    width: usize,
    threads: usize,
    least: usize,
    f: impl Fn(usize, &mut [T]) + Sync,
) {
    let records = out.len() / width.max(1);
    let threads = threads.min(records / least.max(1)).max(1);
    if threads == 1 {
        f(0, out);
        return;
    }
    let per = records.div_ceil(threads);
    // Each task takes its own run of `out`, handed over through its slot.
    let slots: Vec<Slot<'_, T>> = out
        .chunks_mut(per * width)
        .enumerate()
        .map(|(t, part)| Mutex::new(Some((t * per, part))))
        .collect();
    crate::pool::run(slots.len(), &|t| {
        let taken = slots.get(t).and_then(|s| {
            s.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        });
        if let Some((first, part)) = taken {
            f(first, part);
        }
    });
}

/// The grid of a level: its voxels along each axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Dims {
    pub cells: [usize; 3],
}

impl Dims {
    pub(crate) fn nodes(&self) -> [usize; 3] {
        self.cells.map(|n| n + 1)
    }

    pub(crate) fn node_count(&self) -> usize {
        self.nodes().iter().product()
    }

    pub(crate) fn cell_count(&self) -> usize {
        self.cells.iter().product()
    }

    pub(crate) fn node(&self, c: [usize; 3]) -> usize {
        let n = self.nodes();
        c[0] + n[0] * (c[1] + n[1] * c[2])
    }

    pub(crate) fn node_coords(&self, i: usize) -> [usize; 3] {
        let n = self.nodes();
        [i % n[0], (i / n[0]) % n[1], i / (n[0] * n[1])]
    }

    pub(crate) fn cell(&self, c: [usize; 3]) -> usize {
        c[0] + self.cells[0] * (c[1] + self.cells[1] * c[2])
    }

    pub(crate) fn cell_coords(&self, i: usize) -> [usize; 3] {
        let n = self.cells;
        [i % n[0], (i / n[0]) % n[1], i / (n[0] * n[1])]
    }

    /// The nodes of cell `e`, by [`corner`].
    pub(crate) fn cell_nodes(&self, e: usize) -> [usize; 8] {
        let c = self.cell_coords(e);
        std::array::from_fn(|a| {
            let k = corner(a);
            self.node([c[0] + k[0], c[1] + k[1], c[2] + k[2]])
        })
    }

    /// How far each corner of a cell is from its first, in node indices.
    pub(crate) fn corner_offsets(&self) -> [usize; 8] {
        let n = self.nodes();
        std::array::from_fn(|a| {
            let k = corner(a);
            k[0] + n[0] * (k[1] + n[1] * k[2])
        })
    }

    /// The cells around node `n`, each with `n`'s corner number in it.
    fn cells_around(&self, n: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let c = self.node_coords(n);
        (0..8).filter_map(move |a| {
            // Node n is corner `a` of the cell at `c − corner(a)`.
            let k = corner(a);
            let at = [
                c[0].checked_sub(k[0])?,
                c[1].checked_sub(k[1])?,
                c[2].checked_sub(k[2])?,
            ];
            (at[0] < self.cells[0] && at[1] < self.cells[1] && at[2] < self.cells[2])
                .then(|| (self.cell(at), a))
        })
    }
}

/// A level's operator, as the cycle and the next level's Galerkin product see it.
pub(crate) trait Operator: Sync {
    fn dims(&self) -> Dims;
    /// `out`, node by node: `finish(node, the node's rows of A x, out's value there)`, in
    /// the same pass over the nodes as the product's sums (rows of nodes no element touches
    /// are the identity's).
    fn product(&self, x: &[f64], out: &mut [f64], threads: usize, finish: &Finish<'_>);
    /// `y = A x`.
    fn apply(&self, x: &[f64], y: &mut [f64], threads: usize) {
        self.product(x, y, threads, &|_, ax, _| ax);
    }
    /// Whether cell `e` has a matrix.
    fn active(&self, e: usize) -> bool;
    /// Whether cell `e`'s matrix is `S ⊗ I`, the same 8 × 8 matrix for each direction.
    fn is_scalar(&self, e: usize) -> bool;
    /// Cell `e`'s matrix as the operator sums it, into `out`.
    fn element(&self, e: usize, out: &mut [f64; SIZE]);
    /// Cell `e`'s `S`, when it is scalar.
    fn scalar(&self, e: usize) -> Option<[f64; 64]>;
    /// Node `n`'s diagonal block, `None` when no element touches it.
    fn block(&self, n: usize) -> Option<[f64; 9]>;
}

/// What a product does at each node with its rows of `A x` and the output's value there.
pub(crate) type Finish<'a> = dyn Fn(usize, [f64; 3], [f64; 3]) -> [f64; 3] + Sync + 'a;

/// The full cells' products, kept from one product to the next so their memory is not
/// asked of the system each time.
type Scratch = Mutex<Vec<f64>>;

/// What a cell is in a level's product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// No element.
    None,
    /// A full 24 × 24 matrix: its product is worked out once into a buffer.
    Full,
    /// `S ⊗ I`: its rows are worked out at each node as they are needed, a third of the
    /// work and no buffer to write and read back.
    Scalar,
}

/// `y`, node by node: each cell around a node gives its rows for that node (`term(cell, the
/// node's corner in it, the cell's first node)`, `None` for a cell without an element),
/// summed in the order of the node's corners, then `finish(node, sum, y's value there)`.
fn gather(
    dims: Dims,
    y: &mut [f64],
    threads: usize,
    term: impl Fn(usize, usize, usize) -> Option<[f64; 3]> + Sync,
    finish: impl Fn(usize, [f64; 3], [f64; 3]) -> [f64; 3] + Sync,
) {
    let cells = dims.cells;
    let offsets = dims.corner_offsets();
    let nodes = dims.nodes();
    par_fill(y, 3, threads, 4096, |first, part| {
        // The coordinates go along with the nodes, rather than out of a division each.
        let mut c = dims.node_coords(first);
        for (m, out) in part.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let n = first + m;
            if m > 0 {
                c[0] += 1;
                if c[0] == nodes[0] {
                    c[0] = 0;
                    c[1] += 1;
                    if c[1] == nodes[1] {
                        c[1] = 0;
                        c[2] += 1;
                    }
                }
            }
            let mut sum = [0.0; 3];
            for (a, offset) in offsets.iter().enumerate() {
                let k = corner(a);
                if (0..3).any(|d| c[d] < k[d] || c[d] - k[d] >= cells[d]) {
                    continue;
                }
                let e = dims.cell([c[0] - k[0], c[1] - k[1], c[2] - k[2]]);
                if let Some(v) = term(e, a, n - offset) {
                    for r in 0..3 {
                        sum[r] += v[r];
                    }
                }
            }
            *out = finish(n, sum, *out);
        }
    });
}

/// Corner `a`'s rows of `scale S ⊗ I` times `x`, for the cell whose first node is `base`.
fn scalar_rows<T: Copy + Into<f64>>(
    s: &[T],
    scale: f64,
    a: usize,
    base: usize,
    offsets: &[usize; 8],
    x: &[f64],
) -> [f64; 3] {
    let mut out = [0.0; 3];
    for (b, offset) in offsets.iter().enumerate() {
        let w = scale * s[8 * a + b].into();
        let nb = 3 * (base + offset);
        for r in 0..3 {
            out[r] += w * x[nb + r];
        }
    }
    out
}

/// `S ⊗ I` as a 24 × 24 matrix.
fn expand<T: Copy + Into<f64>>(s: &[T], scale: f64, out: &mut [f64; SIZE]) {
    out.fill(0.0);
    for a in 0..8 {
        for b in 0..8 {
            let v = scale * s[8 * a + b].into();
            for r in 0..3 {
                out[(3 * a + r) * DOFS + 3 * b + r] = v;
            }
        }
    }
}

/// `w = s K u` for a symmetric `k`, as a sum of its columns (its rows): the rows' sums run
/// side by side, which the compiler can vectorize.
fn product<T: Copy + Into<f64>>(k: &[T], s: f64, u: &[f64; DOFS], w: &mut [f64]) {
    w.fill(0.0);
    for (row, &uj) in k.as_chunks::<DOFS>().0.iter().zip(u) {
        let c = s * uj;
        for (o, kv) in w.iter_mut().zip(row) {
            *o += (*kv).into() * c;
        }
    }
}

/// The finest level: each voxel with material a brick scaled by its stiffness, each void
/// one (under `scalar_below`, and with no held corner) the brick's Laplacian scaled the same
/// way; held nodes projected.
pub(crate) struct Fine<'a> {
    dims: Dims,
    brick: &'a Brick,
    /// N/mm per voxel: its modulus times its edge; 0 for no material at all (no element).
    stiffness: &'a [f64],
    /// Per node, the index of its projector, or `u32::MAX` when it is free.
    held: &'a [u32],
    /// The motions each held node keeps, `I` less its held directions, row-major.
    projectors: &'a [[f64; 9]],
    kind: Vec<Kind>,
    /// The bricks in order, and each cell's place among them (`NONE` for the others).
    bricks: Vec<u32>,
    slot: Vec<u32>,
    /// Per node: whether a cell with material touches it.
    active: Vec<bool>,
    scratch: Scratch,
}

impl<'a> Fine<'a> {
    /// The level for voxels of `stiffness` (N/mm), those under `scalar_below` void.
    pub(crate) fn new(
        dims: Dims,
        brick: &'a Brick,
        stiffness: &'a [f64],
        held: &'a [u32],
        projectors: &'a [[f64; 9]],
        scalar_below: f64,
    ) -> Fine<'a> {
        let mut kind = vec![Kind::None; dims.cell_count()];
        let mut bricks = Vec::new();
        let mut slot = vec![NONE; dims.cell_count()];
        for (e, s) in stiffness.iter().enumerate() {
            if *s <= 0.0 {
                continue;
            }
            let held_corner = || dims.cell_nodes(e).iter().any(|&n| held[n] != NONE);
            if *s < scalar_below && !held_corner() {
                kind[e] = Kind::Scalar;
            } else {
                kind[e] = Kind::Full;
                slot[e] = bricks.len() as u32;
                bricks.push(e as u32);
            }
        }
        let active = (0..dims.node_count())
            .map(|n| dims.cells_around(n).any(|(e, _)| kind[e] != Kind::None))
            .collect();
        Fine {
            dims,
            brick,
            stiffness,
            held,
            projectors,
            kind,
            bricks,
            slot,
            active,
            scratch: Mutex::new(Vec::new()),
        }
    }

    fn projector(&self, n: usize) -> Option<&[f64; 9]> {
        let p = self.held[n];
        (p != NONE)
            .then(|| self.projectors.get(p as usize))
            .flatten()
    }

    /// The stiffness given to node `n`'s held directions: its own diagonal.
    fn held_stiffness(&self, n: usize) -> f64 {
        let sum: f64 = self
            .dims
            .cells_around(n)
            .map(|(e, _)| self.stiffness[e])
            .sum();
        sum * self.brick.diagonal()
    }

    /// `v` kept to the motions allowed: projected at held nodes, zero at nodes touching no
    /// material.
    pub(crate) fn project(&self, v: &mut [f64]) {
        for n in 0..self.dims.node_count() {
            let x = &mut v[3 * n..3 * n + 3];
            if !self.active[n] {
                x.fill(0.0);
            } else if let Some(p) = self.projector(n) {
                let y = mat3(p, [x[0], x[1], x[2]]);
                x.copy_from_slice(&y);
            }
        }
    }

    /// Node `n`'s diagonal block `D` made `P D P + d (I − P)` when the node is held.
    fn project_block(&self, n: usize, block: &mut [f64; 9]) {
        let Some(p) = self.projector(n) else {
            return;
        };
        let mut pd = [0.0; 9];
        for r in 0..3 {
            for c in 0..3 {
                pd[r * 3 + c] = (0..3).map(|k| p[r * 3 + k] * block[k * 3 + c]).sum();
            }
        }
        let d = self.held_stiffness(n);
        for r in 0..3 {
            for c in 0..3 {
                let pdp: f64 = (0..3).map(|k| pd[r * 3 + k] * p[k * 3 + c]).sum();
                let identity = if r == c { 1.0 } else { 0.0 };
                block[r * 3 + c] = pdp + d * (identity - p[r * 3 + c]);
            }
        }
    }
}

fn mat3(m: &[f64; 9], v: [f64; 3]) -> [f64; 3] {
    [
        m[0] * v[0] + m[1] * v[1] + m[2] * v[2],
        m[3] * v[0] + m[4] * v[1] + m[5] * v[2],
        m[6] * v[0] + m[7] * v[1] + m[8] * v[2],
    ]
}

impl Operator for Fine<'_> {
    fn dims(&self) -> Dims {
        self.dims
    }

    fn product(&self, x: &[f64], out: &mut [f64], threads: usize, finish: &Finish<'_>) {
        let dims = self.dims;
        let offsets = dims.corner_offsets();
        let mut products = self.scratch.lock().unwrap_or_else(|e| e.into_inner());
        products.resize(DOFS * self.bricks.len(), 0.0);
        par_fill(&mut products, DOFS, threads, 512, |first, part| {
            let mut u = [0.0; DOFS];
            for (m, w) in part.as_chunks_mut::<DOFS>().0.iter_mut().enumerate() {
                let e = self.bricks[first + m] as usize;
                let base = dims.node(dims.cell_coords(e));
                for (b, offset) in offsets.iter().enumerate() {
                    let nb = base + offset;
                    let mut xb = [x[3 * nb], x[3 * nb + 1], x[3 * nb + 2]];
                    if let Some(p) = self.projector(nb) {
                        xb = mat3(p, xb);
                    }
                    u[3 * b..3 * b + 3].copy_from_slice(&xb);
                }
                product(&self.brick.k, self.stiffness[e], &u, w);
            }
        });
        let products = &products;
        let term = |e: usize, a: usize, base: usize| match self.kind[e] {
            Kind::None => None,
            Kind::Full => {
                let at = DOFS * self.slot[e] as usize + 3 * a;
                Some([products[at], products[at + 1], products[at + 2]])
            }
            // No held corner, so nothing to project.
            Kind::Scalar => Some(scalar_rows(
                &self.brick.laplacian,
                self.stiffness[e],
                a,
                base,
                &offsets,
                x,
            )),
        };
        let rows = |n: usize, sum: [f64; 3]| -> [f64; 3] {
            let xn = [x[3 * n], x[3 * n + 1], x[3 * n + 2]];
            if !self.active[n] {
                return xn;
            }
            match self.projector(n) {
                None => sum,
                Some(p) => {
                    let px = mat3(p, xn);
                    let kept = mat3(p, sum);
                    let d = self.held_stiffness(n);
                    std::array::from_fn(|r| kept[r] + d * (xn[r] - px[r]))
                }
            }
        };
        gather(dims, out, threads, term, |n, sum, old| {
            finish(n, rows(n, sum), old)
        });
    }

    fn active(&self, e: usize) -> bool {
        self.kind[e] != Kind::None
    }

    fn is_scalar(&self, e: usize) -> bool {
        self.kind[e] == Kind::Scalar
    }

    fn element(&self, e: usize, out: &mut [f64; SIZE]) {
        let s = self.stiffness[e];
        match self.kind[e] {
            Kind::None => {
                out.fill(0.0);
                return;
            }
            Kind::Scalar => {
                // No held corner: nothing more to do.
                expand(&self.brick.laplacian, s, out);
                return;
            }
            Kind::Full => {
                for (o, k) in out.iter_mut().zip(&self.brick.k) {
                    *o = s * k;
                }
            }
        }
        let nodes = self.dims.cell_nodes(e);
        for (a, &na) in nodes.iter().enumerate() {
            let Some(p) = self.projector(na) else {
                continue;
            };
            // Rows and columns of the held corner projected: P K P, P symmetric.
            for row in 0..DOFS {
                let v = [
                    out[row * DOFS + 3 * a],
                    out[row * DOFS + 3 * a + 1],
                    out[row * DOFS + 3 * a + 2],
                ];
                let w = mat3(p, v);
                out[row * DOFS + 3 * a..row * DOFS + 3 * a + 3].copy_from_slice(&w);
            }
            for col in 0..DOFS {
                let v = [
                    out[(3 * a) * DOFS + col],
                    out[(3 * a + 1) * DOFS + col],
                    out[(3 * a + 2) * DOFS + col],
                ];
                let w = mat3(p, v);
                for r in 0..3 {
                    out[(3 * a + r) * DOFS + col] = w[r];
                }
            }
            // The held directions' stiffness, shared among the cells around the node.
            let count = self
                .dims
                .cells_around(na)
                .filter(|&(c, _)| self.kind[c] != Kind::None)
                .count()
                .max(1);
            let d = self.held_stiffness(na) / count as f64;
            for r in 0..3 {
                for c in 0..3 {
                    let identity = if r == c { 1.0 } else { 0.0 };
                    out[(3 * a + r) * DOFS + 3 * a + c] += d * (identity - p[r * 3 + c]);
                }
            }
        }
    }

    fn scalar(&self, e: usize) -> Option<[f64; 64]> {
        let s = self.stiffness[e];
        (self.kind[e] == Kind::Scalar).then(|| self.brick.laplacian.map(|v| s * v))
    }

    fn block(&self, n: usize) -> Option<[f64; 9]> {
        if !self.active[n] {
            return None;
        }
        let mut block = [0.0; 9];
        for (e, a) in self.dims.cells_around(n) {
            let s = self.stiffness[e];
            match self.kind[e] {
                Kind::None => {}
                Kind::Full => {
                    for r in 0..3 {
                        for c in 0..3 {
                            block[r * 3 + c] += s * self.brick.k[(3 * a + r) * DOFS + 3 * a + c];
                        }
                    }
                }
                Kind::Scalar => {
                    for r in 0..3 {
                        block[r * 4] += s * self.brick.laplacian[9 * a];
                    }
                }
            }
        }
        self.project_block(n, &mut block);
        Some(block)
    }
}

/// A coarse level: per cell a full matrix or a scalar one, in single precision.
struct Stored {
    dims: Dims,
    kind: Vec<Kind>,
    /// Per cell, the index of its matrix among the full or the scalar ones.
    index: Vec<u32>,
    /// The cell of each full matrix.
    full: Vec<u32>,
    matrices: Vec<f32>,
    scalars: Vec<f32>,
    /// Per node: whether a cell with a matrix touches it.
    active: Vec<bool>,
    scratch: Scratch,
}

impl Operator for Stored {
    fn dims(&self) -> Dims {
        self.dims
    }

    fn product(&self, x: &[f64], out: &mut [f64], threads: usize, finish: &Finish<'_>) {
        let dims = self.dims;
        let offsets = dims.corner_offsets();
        let mut products = self.scratch.lock().unwrap_or_else(|e| e.into_inner());
        products.resize(DOFS * self.full.len(), 0.0);
        par_fill(&mut products, DOFS, threads, 512, |first, part| {
            let mut u = [0.0; DOFS];
            for (m, w) in part.as_chunks_mut::<DOFS>().0.iter_mut().enumerate() {
                let slot = first + m;
                let base = dims.node(dims.cell_coords(self.full[slot] as usize));
                for (b, offset) in offsets.iter().enumerate() {
                    let nb = base + offset;
                    u[3 * b..3 * b + 3].copy_from_slice(&x[3 * nb..3 * nb + 3]);
                }
                product(&self.matrices[slot * SIZE..(slot + 1) * SIZE], 1.0, &u, w);
            }
        });
        let products = &products;
        let term = |e: usize, a: usize, base: usize| {
            let i = self.index[e] as usize;
            match self.kind[e] {
                Kind::None => None,
                Kind::Full => {
                    let at = DOFS * i + 3 * a;
                    Some([products[at], products[at + 1], products[at + 2]])
                }
                Kind::Scalar => Some(scalar_rows(
                    &self.scalars[64 * i..64 * (i + 1)],
                    1.0,
                    a,
                    base,
                    &offsets,
                    x,
                )),
            }
        };
        gather(dims, out, threads, term, |n, sum, old| {
            let rows = if self.active[n] {
                sum
            } else {
                [x[3 * n], x[3 * n + 1], x[3 * n + 2]]
            };
            finish(n, rows, old)
        });
    }

    fn active(&self, e: usize) -> bool {
        self.kind[e] != Kind::None
    }

    fn is_scalar(&self, e: usize) -> bool {
        self.kind[e] == Kind::Scalar
    }

    fn element(&self, e: usize, out: &mut [f64; SIZE]) {
        let i = self.index[e] as usize;
        match self.kind[e] {
            Kind::None => out.fill(0.0),
            Kind::Full => {
                for (o, v) in out.iter_mut().zip(&self.matrices[i * SIZE..(i + 1) * SIZE]) {
                    *o = f64::from(*v);
                }
            }
            Kind::Scalar => expand(&self.scalars[64 * i..64 * (i + 1)], 1.0, out),
        }
    }

    fn scalar(&self, e: usize) -> Option<[f64; 64]> {
        let i = self.index[e] as usize;
        (self.kind[e] == Kind::Scalar)
            .then(|| std::array::from_fn(|k| f64::from(self.scalars[64 * i + k])))
    }

    fn block(&self, n: usize) -> Option<[f64; 9]> {
        if !self.active[n] {
            return None;
        }
        let mut block = [0.0; 9];
        for (e, a) in self.dims.cells_around(n) {
            let i = self.index[e] as usize;
            match self.kind[e] {
                Kind::None => {}
                Kind::Full => {
                    let k = &self.matrices[i * SIZE..(i + 1) * SIZE];
                    for r in 0..3 {
                        for c in 0..3 {
                            block[r * 3 + c] += f64::from(k[(3 * a + r) * DOFS + 3 * a + c]);
                        }
                    }
                }
                Kind::Scalar => {
                    let d = f64::from(self.scalars[64 * i + 9 * a]);
                    for r in 0..3 {
                        block[r * 4] += d;
                    }
                }
            }
        }
        Some(block)
    }
}

/// Trilinear interpolation's weight, along one axis, of a coarse cell's corner `coarse`
/// (0 or 1) at a fine cell's corner `fine` (0 or 1), the fine cell being child `child` of
/// `factor` along that axis.
fn axis_weight(factor: usize, child: usize, fine: usize, coarse: usize) -> f64 {
    let t = if factor == 2 { child + fine } else { fine };
    match (factor, t, coarse) {
        (2, 0, 0) | (2, 2, 1) | (1, 0, 0) | (1, 1, 1) => 1.0,
        (2, 1, _) => 0.5,
        _ => 0.0,
    }
}

/// `W[a][A]`: coarse corner `A`'s weight at the fine corner `a` of child `child`.
fn weights(factor: [usize; 3], child: [usize; 3]) -> [[f64; 8]; 8] {
    std::array::from_fn(|a| {
        let fa = corner(a);
        std::array::from_fn(|big| {
            let ca = corner(big);
            (0..3)
                .map(|x| axis_weight(factor[x], child[x], fa[x], ca[x]))
                .product()
        })
    })
}

/// The next level down: `Pᵀ A P`, coarse cell by coarse cell. A coarse cell over nothing but
/// scalar cells is scalar too (`Wᵀ (S ⊗ I) W = (Wᵀ S W) ⊗ I`, `W` the same for each
/// direction), so void stays cheap all the way down.
fn galerkin(op: &dyn Operator, factor: [usize; 3], threads: usize) -> Stored {
    let fine = op.dims();
    let dims = Dims {
        cells: std::array::from_fn(|a| fine.cells[a].div_ceil(factor[a])),
    };
    let children = |coarse: usize| {
        let c = dims.cell_coords(coarse);
        (0..8).filter_map(move |s| {
            let k = corner(s);
            if (0..3).any(|a| k[a] >= factor[a]) {
                return None;
            }
            let at: [usize; 3] = std::array::from_fn(|a| c[a] * factor[a] + k[a]);
            (0..3)
                .all(|a| at[a] < fine.cells[a])
                .then(|| (fine.cell(at), k))
        })
    };
    let mut kind = vec![Kind::None; dims.cell_count()];
    let mut index = vec![NONE; dims.cell_count()];
    let mut full: Vec<usize> = Vec::new();
    let mut scalar: Vec<usize> = Vec::new();
    for e in 0..dims.cell_count() {
        let mut any = false;
        let mut all_scalar = true;
        for (f, _) in children(e) {
            if op.active(f) {
                any = true;
                all_scalar &= op.is_scalar(f);
            }
        }
        if !any {
            continue;
        }
        if all_scalar {
            kind[e] = Kind::Scalar;
            index[e] = scalar.len() as u32;
            scalar.push(e);
        } else {
            kind[e] = Kind::Full;
            index[e] = full.len() as u32;
            full.push(e);
        }
    }
    let mut matrices = vec![0.0f32; full.len() * SIZE];
    par_fill(&mut matrices, SIZE, threads, 16, |first, part| {
        let mut k = [0.0; SIZE];
        let mut t = [0.0; SIZE];
        let mut sum = [0.0; SIZE];
        for (m, out) in part.as_chunks_mut::<SIZE>().0.iter_mut().enumerate() {
            sum.fill(0.0);
            for (f, child) in children(full[first + m]) {
                if !op.active(f) {
                    continue;
                }
                op.element(f, &mut k);
                let w = weights(factor, child);
                // T = K W, then Wᵀ T.
                t.fill(0.0);
                for row in 0..DOFS {
                    for (b, wb) in w.iter().enumerate() {
                        let kb = &k[row * DOFS + 3 * b..row * DOFS + 3 * b + 3];
                        for (big, &weight) in wb.iter().enumerate() {
                            if weight == 0.0 {
                                continue;
                            }
                            for c in 0..3 {
                                t[row * DOFS + 3 * big + c] += weight * kb[c];
                            }
                        }
                    }
                }
                for (a, wa) in w.iter().enumerate() {
                    for (big, &weight) in wa.iter().enumerate() {
                        if weight == 0.0 {
                            continue;
                        }
                        for r in 0..3 {
                            let from = (3 * a + r) * DOFS;
                            let to = (3 * big + r) * DOFS;
                            for col in 0..DOFS {
                                sum[to + col] += weight * t[from + col];
                            }
                        }
                    }
                }
            }
            for i in 0..DOFS {
                for j in 0..DOFS {
                    out[i * DOFS + j] = (0.5 * (sum[i * DOFS + j] + sum[j * DOFS + i])) as f32;
                }
            }
        }
    });
    let mut scalars = vec![0.0f32; scalar.len() * 64];
    par_fill(&mut scalars, 64, threads, 256, |first, part| {
        for (m, out) in part.as_chunks_mut::<64>().0.iter_mut().enumerate() {
            let mut sum = [0.0; 64];
            for (f, child) in children(scalar[first + m]) {
                let Some(s) = op.scalar(f) else {
                    continue;
                };
                let w = weights(factor, child);
                // Wᵀ S W.
                for big in 0..8 {
                    for small in 0..8 {
                        let mut v = 0.0;
                        for a in 0..8 {
                            if w[a][big] == 0.0 {
                                continue;
                            }
                            let sw: f64 = (0..8).map(|b| s[8 * a + b] * w[b][small]).sum();
                            v += w[a][big] * sw;
                        }
                        sum[8 * big + small] += v;
                    }
                }
            }
            for i in 0..8 {
                for j in 0..8 {
                    out[8 * i + j] = (0.5 * (sum[8 * i + j] + sum[8 * j + i])) as f32;
                }
            }
        }
    });
    let mut active = vec![false; dims.node_count()];
    for (n, a) in active.iter_mut().enumerate() {
        *a = dims.cells_around(n).any(|(e, _)| kind[e] != Kind::None);
    }
    Stored {
        dims,
        kind,
        index,
        full: full.iter().map(|&e| e as u32).collect(),
        matrices,
        scalars,
        active,
        scratch: Mutex::new(Vec::new()),
    }
}

/// `x_c = Pᵀ x_f`: the fine residual gathered onto the coarse nodes.
fn restrict(
    fine: Dims,
    coarse: Dims,
    factor: [usize; 3],
    x: &[f64],
    out: &mut [f64],
    threads: usize,
) {
    let fine_nodes = fine.nodes();
    par_fill(out, 3, threads, 4096, |first, part| {
        for (m, o) in part.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let c = coarse.node_coords(first + m);
            let span = |a: usize| -> ([usize; 3], [f64; 3], usize) {
                if factor[a] == 1 {
                    ([c[a], 0, 0], [1.0, 0.0, 0.0], 1)
                } else {
                    let mid = 2 * c[a];
                    let mut idx = [0; 3];
                    let mut w = [0.0; 3];
                    let mut len = 0;
                    for (f, weight) in [(mid.wrapping_sub(1), 0.5), (mid, 1.0), (mid + 1, 0.5)] {
                        if f < fine_nodes[a] {
                            idx[len] = f;
                            w[len] = weight;
                            len += 1;
                        }
                    }
                    (idx, w, len)
                }
            };
            let (sx, wx, lx) = span(0);
            let (sy, wy, ly) = span(1);
            let (sz, wz, lz) = span(2);
            let mut sum = [0.0; 3];
            for z in 0..lz {
                for y in 0..ly {
                    for xx in 0..lx {
                        let w = wx[xx] * wy[y] * wz[z];
                        let f = fine.node([sx[xx], sy[y], sz[z]]);
                        for r in 0..3 {
                            sum[r] += w * x[3 * f + r];
                        }
                    }
                }
            }
            o.copy_from_slice(&sum);
        }
    });
}

/// `x_f += P x_c`: the coarse correction interpolated onto the fine nodes.
fn prolong_add(
    fine: Dims,
    coarse: Dims,
    factor: [usize; 3],
    xc: &[f64],
    x: &mut [f64],
    threads: usize,
) {
    par_fill(x, 3, threads, 8192, |first, part| {
        for (m, o) in part.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let f = fine.node_coords(first + m);
            let span = |a: usize| -> ([usize; 2], [f64; 2], usize) {
                if factor[a] == 1 {
                    ([f[a], 0], [1.0, 0.0], 1)
                } else if f[a].is_multiple_of(2) {
                    ([f[a] / 2, 0], [1.0, 0.0], 1)
                } else {
                    ([f[a] / 2, f[a] / 2 + 1], [0.5, 0.5], 2)
                }
            };
            let (sx, wx, lx) = span(0);
            let (sy, wy, ly) = span(1);
            let (sz, wz, lz) = span(2);
            for z in 0..lz {
                for y in 0..ly {
                    for xx in 0..lx {
                        let w = wx[xx] * wy[y] * wz[z];
                        let c = coarse.node([sx[xx], sy[y], sz[z]]);
                        for r in 0..3 {
                            o[r] += w * xc[3 * c + r];
                        }
                    }
                }
            }
        }
    });
}

/// A level's block Jacobi and the top of its smoothing interval.
struct Smoothing {
    dinv: Vec<[f64; 9]>,
    upper: f64,
}

const IDENTITY: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

fn diagonal_inverses(op: &dyn Operator) -> Vec<[f64; 9]> {
    (0..op.dims().node_count())
        .map(|n| op.block(n).and_then(|b| invert3(&b)).unwrap_or(IDENTITY))
        .collect()
}

/// `out = c1 out + c2 D⁻¹ r`.
fn apply_dinv(dinv: &[[f64; 9]], r: &[f64], out: &mut [f64], c1: f64, c2: f64, threads: usize) {
    par_fill(out, 3, threads, 8192, |first, part| {
        for (m, o) in part.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let n = first + m;
            let v = mat3(&dinv[n], [r[3 * n], r[3 * n + 1], r[3 * n + 2]]);
            for k in 0..3 {
                o[k] = c1 * o[k] + c2 * v[k];
            }
        }
    });
}

/// The largest eigenvalue of `D⁻¹ A`, by power iteration. The start is pseudo-random (and
/// the same every time), so that it has some of every mode: from a smooth start, a mode
/// held to a few nodes (where a coarse level's cells end next to void) was missed, the
/// interval came out short, the smoother amplified that mode and conjugate gradients took
/// hundreds of iterations.
fn top_of_spectrum(op: &dyn Operator, dinv: &[[f64; 9]], threads: usize) -> f64 {
    let n = op.dims().node_count() * 3;
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut v: Vec<f64> = (0..n)
        .map(|_| {
            // A linear congruential generator (Knuth's MMIX constants).
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        })
        .collect();
    let mut av = vec![0.0; n];
    let mut w = vec![0.0; n];
    let mut lambda = 1.0;
    for _ in 0..POWER_STEPS {
        op.apply(&v, &mut av, threads);
        apply_dinv(dinv, &av, &mut w, 0.0, 1.0, threads);
        let nw = norm(&w, threads);
        let nv = norm(&v, threads);
        if !(nw > 0.0 && nv > 0.0 && nw.is_finite()) {
            break;
        }
        lambda = nw / nv;
        for (v, w) in v.iter_mut().zip(&w) {
            *v = w / nw;
        }
    }
    lambda.max(1.0)
}

/// Chebyshev smoothing of `A x = b`, from zero when `zero`. Each step is one product whose
/// pass over the nodes also forms the correction, `d = c₁ d + c₂ D⁻¹ (b − A x)`, then
/// `x += d`.
fn smooth(
    op: &dyn Operator,
    s: &Smoothing,
    b: &[f64],
    x: &mut [f64],
    zero: bool,
    d: &mut [f64],
    threads: usize,
) {
    let upper = s.upper;
    let lower = upper / SPECTRUM_RATIO;
    let theta = 0.5 * (upper + lower);
    let delta = 0.5 * (upper - lower);
    let sigma = theta / delta;
    let mut rho = 1.0 / sigma;
    for step in 0..DEGREE {
        let (c1, c2) = if step == 0 {
            (0.0, 1.0 / theta)
        } else {
            let next = 1.0 / (2.0 * sigma - rho);
            let c = (next * rho, 2.0 * next / delta);
            rho = next;
            c
        };
        if step == 0 && zero {
            // x is zero: the residual is b, and x becomes the correction.
            apply_dinv(&s.dinv, b, d, 0.0, c2, threads);
            x.copy_from_slice(d);
            continue;
        }
        let dinv = &s.dinv;
        op.product(x, d, threads, &|n, ax, old| {
            let r = [b[3 * n] - ax[0], b[3 * n + 1] - ax[1], b[3 * n + 2] - ax[2]];
            let v = mat3(&dinv[n], r);
            std::array::from_fn(|k| c1 * old[k] + c2 * v[k])
        });
        let d = &*d;
        par_fill(x, 1, threads, 65536, |first, part| {
            for (m, v) in part.iter_mut().enumerate() {
                *v += d[first + m];
            }
        });
    }
}

/// The coarsest level, factored.
struct Dense {
    n: usize,
    l: Vec<f64>,
}

impl Dense {
    fn factor(op: &dyn Operator) -> Result<Dense, GenerativeError> {
        let dims = op.dims();
        let n = dims.node_count() * 3;
        let mut a = vec![0.0; n * n];
        let mut k = [0.0; SIZE];
        let mut touched = vec![false; dims.node_count()];
        for e in 0..dims.cell_count() {
            if !op.active(e) {
                continue;
            }
            op.element(e, &mut k);
            let nodes = dims.cell_nodes(e);
            for (i, &ni) in nodes.iter().enumerate() {
                touched[ni] = true;
                for (j, &nj) in nodes.iter().enumerate() {
                    for r in 0..3 {
                        for c in 0..3 {
                            a[(3 * ni + r) * n + 3 * nj + c] += k[(3 * i + r) * DOFS + 3 * j + c];
                        }
                    }
                }
            }
        }
        for (node, t) in touched.iter().enumerate() {
            if !t {
                for r in 0..3 {
                    a[(3 * node + r) * n + 3 * node + r] = 1.0;
                }
            }
        }
        // A tiny shift if rounding leaves it short of positive definite.
        let largest = (0..n).map(|i| a[i * n + i]).fold(0.0f64, f64::max);
        for shift in [0.0, 1e-12, 1e-9] {
            if let Some(l) = cholesky(&a, n, shift * largest) {
                return Ok(Dense { n, l });
            }
        }
        Err(GenerativeError::Solver(
            "the coarsest level is not positive definite".to_owned(),
        ))
    }

    fn solve(&self, b: &[f64], x: &mut [f64]) {
        let (n, l) = (self.n, &self.l);
        for i in 0..n {
            let s: f64 = b[i] - (0..i).map(|k| l[i * n + k] * x[k]).sum::<f64>();
            x[i] = s / l[i * n + i];
        }
        for i in (0..n).rev() {
            let mut s = x[i];
            for k in i + 1..n {
                s -= l[k * n + i] * x[k];
            }
            x[i] = s / l[i * n + i];
        }
    }
}

fn cholesky(a: &[f64], n: usize, shift: f64) -> Option<Vec<f64>> {
    let mut l = a.to_vec();
    for i in 0..n {
        l[i * n + i] += shift;
    }
    for j in 0..n {
        let mut d = l[j * n + j];
        for k in 0..j {
            d -= l[j * n + k] * l[j * n + k];
        }
        if !(d > 0.0 && d.is_finite()) {
            return None;
        }
        let d = d.sqrt();
        l[j * n + j] = d;
        for i in j + 1..n {
            let mut s = l[i * n + j];
            for k in 0..j {
                s -= l[i * n + k] * l[j * n + k];
            }
            l[i * n + j] = s / d;
        }
    }
    Some(l)
}

/// Vectors a level of the cycle works in.
struct Work {
    r: Vec<f64>,
    d: Vec<f64>,
    coarse_b: Vec<f64>,
    coarse_x: Vec<f64>,
}

/// The multigrid hierarchy below a finest level.
struct Hierarchy {
    /// Levels below the finest, finest first.
    coarse: Vec<Stored>,
    /// Smoothing of every level but the coarsest, the finest first.
    smoothing: Vec<Smoothing>,
    /// From each level to the next.
    factors: Vec<[usize; 3]>,
    coarsest: Dense,
    work: Vec<Work>,
}

impl Hierarchy {
    fn new(fine: &Fine, threads: usize) -> Result<Hierarchy, GenerativeError> {
        let mut coarse: Vec<Stored> = Vec::new();
        let mut smoothing: Vec<Smoothing> = Vec::new();
        let mut factors: Vec<[usize; 3]> = Vec::new();
        loop {
            let op: &dyn Operator = match coarse.last() {
                Some(c) => c,
                None => fine,
            };
            let dims = op.dims();
            if dims.node_count() <= COARSEST_NODES || dims.cells == [1, 1, 1] {
                break;
            }
            let dinv = diagonal_inverses(op);
            let upper = if coarse.is_empty() {
                // The bricks bound it; the identity rows have 1.
                SPECTRUM_MARGIN * fine.brick.spread.max(1.0)
            } else {
                COARSE_MARGIN * top_of_spectrum(op, &dinv, threads)
            };
            smoothing.push(Smoothing { dinv, upper });
            let factor = dims.cells.map(|n| if n >= 2 { 2 } else { 1 });
            let next = galerkin(op, factor, threads);
            factors.push(factor);
            coarse.push(next);
        }
        let last: &dyn Operator = match coarse.last() {
            Some(c) => c,
            None => fine,
        };
        let coarsest = Dense::factor(last)?;
        let mut work = Vec::with_capacity(factors.len());
        let mut size = fine.dims.node_count() * 3;
        for c in &coarse {
            let next = c.dims.node_count() * 3;
            work.push(Work {
                r: vec![0.0; size],
                d: vec![0.0; size],
                coarse_b: vec![0.0; next],
                coarse_x: vec![0.0; next],
            });
            size = next;
        }
        Ok(Hierarchy {
            coarse,
            smoothing,
            factors,
            coarsest,
            work,
        })
    }

    /// `x = M⁻¹ b`: one V-cycle from zero.
    fn apply(&mut self, fine: &Fine, b: &[f64], x: &mut [f64], threads: usize) {
        let Hierarchy {
            coarse,
            smoothing,
            factors,
            coarsest,
            work,
        } = self;
        cycle(
            fine, coarse, smoothing, factors, coarsest, work, 0, b, x, threads,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn cycle(
    fine: &Fine,
    coarse: &[Stored],
    smoothing: &[Smoothing],
    factors: &[[usize; 3]],
    coarsest: &Dense,
    work: &mut [Work],
    l: usize,
    b: &[f64],
    x: &mut [f64],
    threads: usize,
) {
    let (Some(s), Some(&factor), Some((w, rest))) =
        (smoothing.get(l), factors.get(l), work.split_first_mut())
    else {
        coarsest.solve(b, x);
        return;
    };
    let op: &dyn Operator = if l == 0 { fine } else { &coarse[l - 1] };
    let next = &coarse[l];
    smooth(op, s, b, x, true, &mut w.d, threads);
    op.product(x, &mut w.r, threads, &|n, ax, _| {
        [b[3 * n] - ax[0], b[3 * n + 1] - ax[1], b[3 * n + 2] - ax[2]]
    });
    restrict(op.dims(), next.dims, factor, &w.r, &mut w.coarse_b, threads);
    w.coarse_x.fill(0.0);
    cycle(
        fine,
        coarse,
        smoothing,
        factors,
        coarsest,
        rest,
        l + 1,
        &w.coarse_b,
        &mut w.coarse_x,
        threads,
    );
    prolong_add(op.dims(), next.dims, factor, &w.coarse_x, x, threads);
    smooth(op, s, b, x, false, &mut w.d, threads);
}

/// Values per block of a dot product: each block summed on its own, the blocks in order,
/// so the sum is the same on any number of threads.
const DOT_BLOCK: usize = 8192;

fn dot(a: &[f64], b: &[f64], threads: usize) -> f64 {
    let mut blocks = vec![0.0; a.len().div_ceil(DOT_BLOCK)];
    par_fill(&mut blocks, 1, threads, 8, |first, part| {
        for (m, out) in part.iter_mut().enumerate() {
            let at = (first + m) * DOT_BLOCK;
            let end = (at + DOT_BLOCK).min(a.len());
            *out = a[at..end].iter().zip(&b[at..end]).map(|(x, y)| x * y).sum();
        }
    });
    blocks.iter().sum()
}

fn norm(a: &[f64], threads: usize) -> f64 {
    dot(a, a, threads).sqrt()
}

/// `y += c x`.
fn axpy(y: &mut [f64], c: f64, x: &[f64], threads: usize) {
    par_fill(y, 1, threads, 65536, |first, part| {
        for (m, v) in part.iter_mut().enumerate() {
            *v += c * x[first + m];
        }
    });
}

/// How a solve went.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Solved {
    pub iterations: usize,
    /// The residual reached, a fraction of the loads.
    pub residual: f64,
}

/// Solves `A x = b` on the finest level by preconditioned conjugate gradients, from `x`
/// (the last solve's displacements, kept to the allowed motions), to a residual of
/// `tolerance` times the loads.
pub(crate) fn solve(
    fine: &Fine,
    b: &[f64],
    x: &mut [f64],
    tolerance: f64,
    max_iterations: usize,
    cancel: &AtomicBool,
    threads: usize,
) -> Result<Solved, GenerativeError> {
    let n = fine.dims.node_count() * 3;
    let mut hierarchy = Hierarchy::new(fine, threads)?;
    fine.project(x);
    let mut loads = b.to_vec();
    fine.project(&mut loads);
    let scale = norm(&loads, threads);
    if scale == 0.0 {
        x.fill(0.0);
        return Ok(Solved {
            iterations: 0,
            residual: 0.0,
        });
    }
    let mut r = vec![0.0; n];
    fine.apply(x, &mut r, threads);
    for (r, b) in r.iter_mut().zip(&loads) {
        *r = b - *r;
    }
    let mut z = vec![0.0; n];
    hierarchy.apply(fine, &r, &mut z, threads);
    fine.project(&mut z);
    let mut p = z.clone();
    let mut rz = dot(&r, &z, threads);
    let mut q = vec![0.0; n];
    let mut residual = norm(&r, threads) / scale;
    for iteration in 0..max_iterations {
        if residual <= tolerance {
            return Ok(Solved {
                iterations: iteration,
                residual,
            });
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(GenerativeError::Cancelled);
        }
        fine.apply(&p, &mut q, threads);
        let pq = dot(&p, &q, threads);
        if !(pq > 0.0 && pq.is_finite()) {
            break;
        }
        let alpha = rz / pq;
        axpy(x, alpha, &p, threads);
        axpy(&mut r, -alpha, &q, threads);
        residual = norm(&r, threads) / scale;
        hierarchy.apply(fine, &r, &mut z, threads);
        fine.project(&mut z);
        let next = dot(&r, &z, threads);
        let beta = next / rz;
        rz = next;
        par_fill(&mut p, 1, threads, 65536, |first, part| {
            for (m, v) in part.iter_mut().enumerate() {
                *v = z[first + m] + beta * *v;
            }
        });
    }
    if residual <= tolerance {
        return Ok(Solved {
            iterations: max_iterations,
            residual,
        });
    }
    Err(GenerativeError::NotConverged {
        iterations: max_iterations,
        residual,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A beam of `cells` voxels of edge 1, held at x = 0, loaded down at its far end.
    fn beam(
        cells: [usize; 3],
        stiffness: impl Fn([usize; 3]) -> f64,
    ) -> (Dims, Vec<f64>, Vec<u32>, Vec<f64>) {
        let dims = Dims { cells };
        let s: Vec<f64> = (0..dims.cell_count())
            .map(|e| stiffness(dims.cell_coords(e)))
            .collect();
        let mut held = vec![NONE; dims.node_count()];
        let mut b = vec![0.0; dims.node_count() * 3];
        for n in 0..dims.node_count() {
            let c = dims.node_coords(n);
            if c[0] == 0 {
                held[n] = 0;
            }
            if c[0] == cells[0] {
                b[3 * n + 1] = -1.0;
            }
        }
        (dims, s, held, b)
    }

    fn assembled(fine: &Fine) -> Vec<f64> {
        let n = fine.dims.node_count() * 3;
        let mut a = vec![0.0; n * n];
        let mut col = vec![0.0; n];
        let mut out = vec![0.0; n];
        for j in 0..n {
            col.fill(0.0);
            col[j] = 1.0;
            fine.apply(&col, &mut out, 1);
            for i in 0..n {
                a[i * n + j] = out[i];
            }
        }
        a
    }

    #[test]
    fn the_operator_is_its_elements_summed_and_symmetric() {
        let brick = Brick::new(0.3);
        // A cell with nothing, a nearly void one, the others of varied stiffness.
        let (dims, s, mut held, _) = beam([3, 2, 2], |c| match c {
            [1, 1, 0] => 0.0,
            [2, 0, 1] => 1e-7,
            _ => 1.0 + c[0] as f64,
        });
        // A roller node too: held along (1, 1, 0) / √2 only.
        let roller = [0.5, -0.5, 0.0, -0.5, 0.5, 0.0, 0.0, 0.0, 1.0];
        let target = dims.node([2, 1, 1]);
        held[target] = 1;
        let projectors = [[0.0; 9], roller];
        let fine = Fine::new(dims, &brick, &s, &held, &projectors, 1e-6);
        let a = assembled(&fine);
        let n = dims.node_count() * 3;
        // From the elements.
        let mut sum = vec![0.0; n * n];
        let mut k = [0.0; SIZE];
        for e in 0..dims.cell_count() {
            if !fine.active(e) {
                continue;
            }
            fine.element(e, &mut k);
            let nodes = dims.cell_nodes(e);
            for (i, &ni) in nodes.iter().enumerate() {
                for (j, &nj) in nodes.iter().enumerate() {
                    for r in 0..3 {
                        for c in 0..3 {
                            sum[(3 * ni + r) * n + 3 * nj + c] += k[(3 * i + r) * DOFS + 3 * j + c];
                        }
                    }
                }
            }
        }
        for i in 0..n {
            for j in 0..n {
                assert!(
                    (a[i * n + j] - a[j * n + i]).abs() < 1e-12,
                    "symmetric at {i} {j}"
                );
                assert!((a[i * n + j] - sum[i * n + j]).abs() < 1e-12, "{i} {j}");
            }
        }
        // Diagonal blocks agree with block().
        for node in 0..dims.node_count() {
            if let Some(b) = fine.block(node) {
                for r in 0..3 {
                    for c in 0..3 {
                        assert!(
                            (b[r * 3 + c] - a[(3 * node + r) * n + 3 * node + c]).abs() < 1e-12
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn conjugate_gradients_match_a_direct_solve_and_converge_quickly_with_voids() {
        let brick = Brick::new(0.3);
        let cancel = AtomicBool::new(false);
        // Small enough to factor: the same answer as a dense solve.
        let (dims, s, held, b) = beam([6, 2, 2], |_| 1.0);
        let projectors = [[0.0; 9]];
        let fine = Fine::new(dims, &brick, &s, &held, &projectors, 0.0);
        let mut x = vec![0.0; b.len()];
        solve(&fine, &b, &mut x, 1e-10, 200, &cancel, 1).unwrap();
        let a = assembled(&fine);
        let n = b.len();
        let mut loads = b.clone();
        fine.project(&mut loads);
        let l = cholesky(&a, n, 0.0).unwrap();
        let dense = Dense { n, l };
        let mut exact = vec![0.0; n];
        dense.solve(&loads, &mut exact);
        let scale = exact.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        for i in 0..n {
            assert!(
                (x[i] - exact[i]).abs() < 1e-7 * scale,
                "{i}: {} {}",
                x[i],
                exact[i]
            );
        }

        // A larger beam, every other slab of voxels nearly void: multigrid keeps the
        // iterations few, and the answer is the same on one thread and on several.
        let (dims, s, held, b) = beam([48, 12, 10], |c| {
            if c[1] >= 4 && c[1] < 8 && c[0] % 4 == 2 {
                1e-6
            } else {
                1.0
            }
        });
        let fine = Fine::new(dims, &brick, &s, &held, &projectors, 1e-5);
        let mut one = vec![0.0; b.len()];
        let solved = solve(&fine, &b, &mut one, 1e-8, 400, &cancel, 1).unwrap();
        assert!(solved.iterations < 80, "{solved:?}");
        let mut many = vec![0.0; b.len()];
        solve(&fine, &b, &mut many, 1e-8, 400, &cancel, 8).unwrap();
        assert_eq!(one, many);
        // Held nodes stay put.
        for node in 0..dims.node_count() {
            if dims.node_coords(node)[0] == 0 {
                assert_eq!(&one[3 * node..3 * node + 3], &[0.0; 3]);
            }
        }
    }
}
