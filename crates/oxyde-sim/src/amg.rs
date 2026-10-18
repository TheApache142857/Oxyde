//! A smoothed-aggregation algebraic multigrid preconditioner for the stiffness matrix
//! (Vaněk, Mandel and Brezina, 1996): what makes conjugate gradients in [`crate::solve`]
//! converge in tens of iterations instead of thousands. The same hierarchy serves the
//! conduction matrix of [`crate::thermal`], with one mode (a uniform temperature) instead
//! of the six rigid motions.
//!
//! - **Aggregates**: each node is grouped with its strongly coupled neighbours (a block
//!   counts when its norm is a fair fraction of the two diagonal blocks'), greedily and in
//!   node order, so every solve of a mesh groups it the same way. Nodes coupled to nothing,
//!   such as fixed ones, join no aggregate: the smoother alone handles them.
//! - **The tentative prolongator** maps an aggregate's six coarse unknowns onto its nodes
//!   through the six rigid motions (three translations, three turns), orthonormalized
//!   aggregate by aggregate (a local QR); the coarse level's rigid motions are the `R` of
//!   that QR. A motion an aggregate cannot carry (a turn about the line of a row of nodes)
//!   gets a zero column, and its coarse unknown a diagonal term coupled to nothing.
//! - **Smoothing** the prolongator by one damped block-Jacobi step widens each aggregate's
//!   reach by a ring of nodes; that is what makes the coarse levels good for elasticity.
//!   The coarse matrices are `Pᵀ A P`, in 6 × 6 blocks, down to a few hundred unknowns
//!   solved by a dense Cholesky factorization.
//! - **The cycle** is a V-cycle with Chebyshev smoothing on block-Jacobi, the same
//!   polynomial before and after the coarse correction, so the preconditioner is symmetric
//!   and positive, as conjugate gradients need. The large products run on every core,
//!   each thread owning a range of rows, so the result does not depend on the thread count.

// Indices into matrices and vectors whose sizes are set here.
#![allow(clippy::indexing_slicing)]

/// Unknowns at or under which a level is solved directly.
const COARSE_UNKNOWNS: usize = 600;
/// The largest coarsest level factored densely; a hierarchy that cannot get under it is
/// refused, and the solve falls back to block-Jacobi.
const DIRECT_LIMIT: usize = 3000;
const MAX_LEVELS: usize = 12;
/// A block couples two nodes strongly when its norm is at least this fraction of the
/// geometric mean of their diagonal blocks' norms.
const STRENGTH: f64 = 0.08;
/// Chebyshev smoothing: the polynomial's degree, and how far under the largest eigenvalue
/// of `D⁻¹A` it damps (the upper part of the spectrum is the smoother's job, the rest the
/// coarse levels').
const DEGREE: usize = 2;
const SPECTRUM_RATIO: f64 = 30.0;
/// Power iterations estimating the largest eigenvalue of `D⁻¹A`, and the margin over it.
const POWER_STEPS: usize = 15;
const SPECTRUM_MARGIN: f64 = 1.1;
/// Matrices with fewer values are multiplied on one thread.
const PARALLEL_VALUES: usize = 1 << 18;
/// Rigid motions: three translations, three turns; the near-null space of elasticity.
pub(crate) const RIGID_MODES: usize = 6;
const NONE: u32 = u32::MAX;

/// A sparse matrix of dense `r × c` blocks, row-major, row by row (block CSR).
struct Bsr {
    r: usize,
    c: usize,
    row_start: Vec<usize>,
    columns: Vec<u32>,
    values: Vec<f64>,
}

impl Bsr {
    fn view(&self) -> BsrRef<'_> {
        BsrRef {
            r: self.r,
            c: self.c,
            row_start: &self.row_start,
            columns: &self.columns,
            values: &self.values,
        }
    }
}

/// A borrowed [`Bsr`]: the finest level is the caller's matrix.
#[derive(Clone, Copy)]
pub(crate) struct BsrRef<'a> {
    pub r: usize,
    pub c: usize,
    pub row_start: &'a [usize],
    pub columns: &'a [u32],
    pub values: &'a [f64],
}

impl<'a> BsrRef<'a> {
    fn rows(&self) -> usize {
        self.row_start.len().saturating_sub(1)
    }

    fn row(&self, i: usize) -> std::ops::Range<usize> {
        self.row_start[i]..self.row_start[i + 1]
    }

    fn block(&self, k: usize) -> &'a [f64] {
        let size = self.r * self.c;
        &self.values[k * size..(k + 1) * size]
    }

    /// The index of the diagonal block of row `i`.
    fn diagonal(&self, i: usize) -> Option<usize> {
        let span = self.row(i);
        let k = self.columns[span.clone()].binary_search(&(i as u32)).ok()?;
        Some(span.start + k)
    }
}

// ---- Threads ----------------------------------------------------------------------------

/// Row ranges holding about equal numbers of blocks, one per thread; a single range when
/// `values` is too few for threads to pay.
fn row_ranges(row_start: &[usize], threads: usize, values: usize) -> Vec<(usize, usize)> {
    let rows = row_start.len().saturating_sub(1);
    let threads = if values < PARALLEL_VALUES {
        1
    } else {
        threads.max(1)
    };
    let total = row_start[rows];
    let mut out = Vec::with_capacity(threads);
    let mut start = 0;
    for t in 1..=threads {
        let target = total * t / threads;
        let mut end = start;
        while end < rows && (row_start[end] < target || t == threads) {
            end += 1;
        }
        out.push((start, end));
        start = end;
    }
    out.retain(|&(a, b)| b > a);
    out
}

/// Calls `f(r0, r1, part)` for each range on its own thread (inline for one), `part`
/// being the rows' share of `out`, `width` values per row.
fn for_rows(
    ranges: &[(usize, usize)],
    width: usize,
    out: &mut [f64],
    f: impl Fn(usize, usize, &mut [f64]) + Sync,
) {
    if let [(r0, r1)] = ranges {
        f(*r0, *r1, &mut out[r0 * width..r1 * width]);
        return;
    }
    std::thread::scope(|s| {
        let mut rest = out;
        let mut taken = 0;
        for &(r0, r1) in ranges {
            let (mine, tail) = std::mem::take(&mut rest).split_at_mut((r1 - taken) * width);
            let mine = &mut mine[(r0 - taken) * width..];
            taken = r1;
            rest = tail;
            let f = &f;
            s.spawn(move || f(r0, r1, mine));
        }
    });
}

/// `f(r0, r1)` for each range on its own thread, the results in range order.
fn map_rows<R: Send>(
    ranges: &[(usize, usize)],
    f: impl Fn(usize, usize) -> R + Sync,
) -> Result<Vec<R>, String> {
    if ranges.len() <= 1 {
        return Ok(ranges.iter().map(|&(a, b)| f(a, b)).collect());
    }
    std::thread::scope(|s| {
        let handles: Vec<_> = ranges
            .iter()
            .map(|&(a, b)| {
                let f = &f;
                s.spawn(move || f(a, b))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().map_err(|_| "a multigrid thread failed".to_owned()))
            .collect()
    })
}

// ---- Products -----------------------------------------------------------------------------

/// Rows `r0..r1` of `A x` into `out`, for `R × C` blocks.
fn rows_times<const R: usize, const C: usize>(
    a: BsrRef,
    x: &[f64],
    r0: usize,
    r1: usize,
    out: &mut [f64],
) {
    for row in r0..r1 {
        let mut sum = [0.0; R];
        for k in a.row(row) {
            let col = a.columns[k] as usize;
            let xb = &x[col * C..col * C + C];
            let block = &a.values[k * R * C..(k + 1) * R * C];
            for (i, s) in sum.iter_mut().enumerate() {
                let line = &block[i * C..i * C + C];
                *s += line.iter().zip(xb).map(|(m, v)| m * v).sum::<f64>();
            }
        }
        out[(row - r0) * R..(row - r0 + 1) * R].copy_from_slice(&sum);
    }
}

/// Rows `r0..r1` of `A x` into `out`, for blocks of any size.
fn rows_times_any(a: BsrRef, x: &[f64], r0: usize, r1: usize, out: &mut [f64]) {
    let (r, c) = (a.r, a.c);
    for (row, o) in (r0..r1).zip(out.chunks_exact_mut(r)) {
        o.fill(0.0);
        for k in a.row(row) {
            let col = a.columns[k] as usize;
            let xb = &x[col * c..col * c + c];
            let block = a.block(k);
            for (i, s) in o.iter_mut().enumerate() {
                *s += block[i * c..i * c + c]
                    .iter()
                    .zip(xb)
                    .map(|(m, v)| m * v)
                    .sum::<f64>();
            }
        }
    }
}

/// `y = A x`.
fn multiply(a: BsrRef, ranges: &[(usize, usize)], x: &[f64], y: &mut [f64]) {
    match (a.r, a.c) {
        (3, 3) => for_rows(ranges, 3, y, |r0, r1, o| {
            rows_times::<3, 3>(a, x, r0, r1, o)
        }),
        (3, 6) => for_rows(ranges, 3, y, |r0, r1, o| {
            rows_times::<3, 6>(a, x, r0, r1, o)
        }),
        (6, 6) => for_rows(ranges, 6, y, |r0, r1, o| {
            rows_times::<6, 6>(a, x, r0, r1, o)
        }),
        (1, 1) => for_rows(ranges, 1, y, |r0, r1, o| {
            rows_times::<1, 1>(a, x, r0, r1, o)
        }),
        _ => for_rows(ranges, a.r, y, |r0, r1, o| rows_times_any(a, x, r0, r1, o)),
    }
}

/// Where each column's blocks are: for column `j`, `(row, block index)` in row order.
struct Columns {
    start: Vec<usize>,
    entries: Vec<(u32, usize)>,
}

fn columns_of(m: BsrRef, count: usize) -> Columns {
    let mut start = vec![0usize; count + 1];
    for &c in m.columns {
        start[c as usize + 1] += 1;
    }
    for j in 0..count {
        start[j + 1] += start[j];
    }
    let mut fill = start.clone();
    let mut entries = vec![(0u32, 0usize); m.columns.len()];
    for row in 0..m.rows() {
        for k in m.row(row) {
            let c = m.columns[k] as usize;
            entries[fill[c]] = (row as u32, k);
            fill[c] += 1;
        }
    }
    Columns { start, entries }
}

/// `y = Pᵀ x`, column by column of `P` (row by row of `y`), so threads write apart.
fn multiply_transposed(
    p: BsrRef,
    columns: &Columns,
    ranges: &[(usize, usize)],
    x: &[f64],
    y: &mut [f64],
) {
    let (r, c) = (p.r, p.c);
    for_rows(ranges, c, y, |j0, j1, out| {
        for (j, o) in (j0..j1).zip(out.chunks_exact_mut(c)) {
            o.fill(0.0);
            for &(row, k) in &columns.entries[columns.start[j]..columns.start[j + 1]] {
                let block = p.block(k);
                let xr = &x[row as usize * r..row as usize * r + r];
                for (a, &xa) in xr.iter().enumerate() {
                    for (s, m) in o.iter_mut().zip(&block[a * c..a * c + c]) {
                        *s += m * xa;
                    }
                }
            }
        }
    });
}

/// `out += a b`, `a` being `m × k` and `b` `k × n`, row-major.
fn mul_add(out: &mut [f64], a: &[f64], b: &[f64], m: usize, k: usize, n: usize) {
    for i in 0..m {
        let row = &mut out[i * n..i * n + n];
        for l in 0..k {
            let f = a[i * k + l];
            if f == 0.0 {
                continue;
            }
            for (o, v) in row.iter_mut().zip(&b[l * n..l * n + n]) {
                *o += f * v;
            }
        }
    }
}

/// `out += aᵀ b`, `a` being `k × m` and `b` `k × n`, row-major.
fn mul_t_add(out: &mut [f64], a: &[f64], b: &[f64], m: usize, k: usize, n: usize) {
    for l in 0..k {
        let line = &b[l * n..l * n + n];
        for i in 0..m {
            let f = a[l * m + i];
            if f == 0.0 {
                continue;
            }
            for (o, v) in out[i * n..i * n + n].iter_mut().zip(line) {
                *o += f * v;
            }
        }
    }
}

/// Builds the rows of a block matrix one after the other, adding into blocks found by
/// column (Gustavson's sparse product). Each row's blocks end sorted by column.
struct RowBuilder {
    size: usize,
    /// Per column, the position of its block in the current row (stale when before
    /// `begin`).
    marker: Vec<usize>,
    begin: usize,
    row_start: Vec<usize>,
    columns: Vec<u32>,
    values: Vec<f64>,
    order: Vec<usize>,
    scratch: Vec<f64>,
}

impl RowBuilder {
    fn new(column_count: usize, size: usize) -> RowBuilder {
        RowBuilder {
            size,
            marker: vec![usize::MAX; column_count],
            begin: 0,
            row_start: vec![0],
            columns: Vec::new(),
            values: Vec::new(),
            order: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// The current row's block in column `col`, zero when new.
    fn slot(&mut self, col: u32) -> &mut [f64] {
        let mut p = self.marker[col as usize];
        if p == usize::MAX || p < self.begin {
            p = self.columns.len();
            self.columns.push(col);
            self.values.resize(self.values.len() + self.size, 0.0);
            self.marker[col as usize] = p;
        }
        &mut self.values[p * self.size..(p + 1) * self.size]
    }

    /// The current row's blocks, with their columns.
    fn row_mut(&mut self) -> impl Iterator<Item = (u32, &mut [f64])> {
        let size = self.size;
        self.columns[self.begin..]
            .iter()
            .copied()
            .zip(self.values[self.begin * size..].chunks_exact_mut(size))
    }

    fn finish_row(&mut self) {
        let (begin, end, size) = (self.begin, self.columns.len(), self.size);
        if !self.columns[begin..end].is_sorted() {
            self.order.clear();
            self.order.extend(begin..end);
            let columns = &self.columns;
            self.order.sort_unstable_by_key(|&p| columns[p]);
            self.scratch.clear();
            for &p in &self.order {
                self.scratch
                    .extend_from_slice(&self.values[p * size..(p + 1) * size]);
            }
            self.values[begin * size..end * size].copy_from_slice(&self.scratch);
            let sorted: Vec<u32> = self.order.iter().map(|&p| self.columns[p]).collect();
            self.columns[begin..end].copy_from_slice(&sorted);
        }
        self.row_start.push(end);
        self.begin = end;
    }
}

/// One matrix of the builders' rows, in order.
fn concat(parts: Vec<RowBuilder>, r: usize, c: usize) -> Bsr {
    let blocks = parts.iter().map(|p| p.columns.len()).sum();
    let mut row_start = vec![0usize];
    let mut columns = Vec::with_capacity(blocks);
    let mut values = Vec::with_capacity(blocks * r * c);
    for part in parts {
        let offset = columns.len();
        row_start.extend(part.row_start[1..].iter().map(|s| s + offset));
        columns.extend_from_slice(&part.columns);
        values.extend_from_slice(&part.values);
    }
    Bsr {
        r,
        c,
        row_start,
        columns,
        values,
    }
}

// ---- Setup ------------------------------------------------------------------------------

/// Inverts the `n × n` matrix `m` into `out` by Gauss–Jordan elimination with partial
/// pivoting; `false` when it is singular.
fn invert(m: &[f64], n: usize, out: &mut [f64]) -> bool {
    let mut a = [0.0f64; 36];
    if n > 6 {
        return false;
    }
    a[..n * n].copy_from_slice(m);
    out.fill(0.0);
    for i in 0..n {
        out[i * n + i] = 1.0;
    }
    let size = m.iter().fold(0.0f64, |s, v| s.max(v.abs()));
    for col in 0..n {
        let mut pivot = col;
        for row in col + 1..n {
            if a[row * n + col].abs() > a[pivot * n + col].abs() {
                pivot = row;
            }
        }
        let p = a[pivot * n + col];
        if !(p.abs() > 1e-14 * size && p.is_finite()) {
            return false;
        }
        if pivot != col {
            for k in 0..n {
                a.swap(pivot * n + k, col * n + k);
                out.swap(pivot * n + k, col * n + k);
            }
        }
        let inv = 1.0 / p;
        for k in 0..n {
            a[col * n + k] *= inv;
            out[col * n + k] *= inv;
        }
        for row in 0..n {
            let f = a[row * n + col];
            if row == col || f == 0.0 {
                continue;
            }
            for k in 0..n {
                a[row * n + k] -= f * a[col * n + k];
                out[row * n + k] -= f * out[col * n + k];
            }
        }
    }
    true
}

/// The inverse of every diagonal block.
fn diagonal_inverses(a: BsrRef) -> Result<Vec<f64>, String> {
    let b = a.r;
    let mut out = vec![0.0; a.rows() * b * b];
    for (i, inv) in out.chunks_exact_mut(b * b).enumerate() {
        let ok = a.diagonal(i).is_some_and(|k| invert(a.block(k), b, inv));
        if !ok {
            return Err(format!("diagonal block {i} is singular"));
        }
    }
    Ok(out)
}

/// `out = c1 out + c2 D⁻¹ r`, block by block (`out` is ignored when `c1` is 0).
fn diagonal_step(dinv: &[f64], b: usize, r: &[f64], out: &mut [f64], c1: f64, c2: f64) {
    for ((o, r), m) in out
        .chunks_exact_mut(b)
        .zip(r.chunks_exact(b))
        .zip(dinv.chunks_exact(b * b))
    {
        let mut z = [0.0; 6];
        for (i, z) in z[..b].iter_mut().enumerate() {
            *z = m[i * b..i * b + b]
                .iter()
                .zip(r)
                .map(|(m, v)| m * v)
                .sum::<f64>();
        }
        for (o, z) in o.iter_mut().zip(z) {
            *o = if c1 == 0.0 { c2 * z } else { c1 * *o + c2 * z };
        }
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The largest eigenvalue of `D⁻¹A`, by power iterations and the Rayleigh quotient in the
/// `A` inner product (from under: callers add a margin).
fn spectral_radius(a: BsrRef, ranges: &[(usize, usize)], dinv: &[f64]) -> Result<f64, String> {
    let n = a.rows() * a.r;
    // A fixed start, varied enough to hold some of every eigenvector.
    let mut v: Vec<f64> = (0..n)
        .map(|k| 1.0 + ((k * 7919) % 1009) as f64 / 1009.0)
        .collect();
    let mut y = vec![0.0; n];
    let mut w = vec![0.0; n];
    let mut lambda = 0.0;
    for _ in 0..POWER_STEPS {
        multiply(a, ranges, &v, &mut y);
        diagonal_step(dinv, a.r, &y, &mut w, 0.0, 1.0);
        let (num, den) = (dot(&y, &w), dot(&v, &y));
        if !(num > 0.0 && den > 0.0 && num.is_finite() && den.is_finite()) {
            return Err("the matrix is not positive definite".to_owned());
        }
        lambda = num / den;
        let s = 1.0 / dot(&w, &w).sqrt();
        for (v, w) in v.iter_mut().zip(&w) {
            *v = w * s;
        }
    }
    Ok(lambda)
}

/// Each node's aggregate (`NONE` for nodes coupled strongly to nothing), and how many
/// aggregates there are. The usual three passes: whole neighbourhoods first, then the
/// nodes left join a neighbouring aggregate, then what is still left groups together.
fn aggregate(a: BsrRef) -> (Vec<u32>, usize) {
    let n = a.rows();
    let norm2 = |k: usize| a.block(k).iter().map(|v| v * v).sum::<f64>();
    let diag: Vec<f64> = (0..n)
        .map(|i| a.diagonal(i).map_or(0.0, |k| norm2(k).sqrt()))
        .collect();
    let mut strong_start = vec![0usize; n + 1];
    let mut strong = Vec::new();
    for i in 0..n {
        for k in a.row(i) {
            let j = a.columns[k] as usize;
            if j == i {
                continue;
            }
            let v = norm2(k);
            if v > 0.0 && v >= STRENGTH * STRENGTH * diag[i] * diag[j] {
                strong.push(j as u32);
            }
        }
        strong_start[i + 1] = strong.len();
    }
    let neighbours = |i: usize| &strong[strong_start[i]..strong_start[i + 1]];
    let mut agg = vec![NONE; n];
    let mut count = 0u32;
    for i in 0..n {
        let nb = neighbours(i);
        if agg[i] != NONE || nb.is_empty() || nb.iter().any(|&j| agg[j as usize] != NONE) {
            continue;
        }
        agg[i] = count;
        for &j in nb {
            agg[j as usize] = count;
        }
        count += 1;
    }
    let first = agg.clone();
    for (i, a) in agg.iter_mut().enumerate() {
        if *a != NONE {
            continue;
        }
        if let Some(&j) = neighbours(i).iter().find(|&&j| first[j as usize] != NONE) {
            *a = first[j as usize];
        }
    }
    for i in 0..n {
        let nb = neighbours(i);
        if agg[i] != NONE || nb.is_empty() {
            continue;
        }
        agg[i] = count;
        for &j in nb {
            if agg[j as usize] == NONE {
                agg[j as usize] = count;
            }
        }
        count += 1;
    }
    (agg, count as usize)
}

/// The tentative prolongator, a `b × mc` block per node (zero for nodes in no
/// aggregate), and the coarse level's modes, `mc × mc` per aggregate: the local QR of each
/// aggregate's rows of `modes` (`mc` columns).
fn tentative(
    agg: &[u32],
    count: usize,
    b: usize,
    mc: usize,
    modes: &[f64],
) -> (Vec<f64>, Vec<f64>) {
    let mut start = vec![0usize; count + 1];
    for &a in agg {
        if a != NONE {
            start[a as usize + 1] += 1;
        }
    }
    for a in 0..count {
        start[a + 1] += start[a];
    }
    let mut fill = start.clone();
    let mut members = vec![0usize; start[count]];
    for (i, &a) in agg.iter().enumerate() {
        if a != NONE {
            members[fill[a as usize]] = i;
            fill[a as usize] += 1;
        }
    }
    let width = b * mc;
    let mut t = vec![0.0; agg.len() * width];
    let mut coarse = vec![0.0; count * mc * mc];
    // Column by column: column `j` is `q[j * rows..(j + 1) * rows]`.
    let mut q: Vec<f64> = Vec::new();
    for a in 0..count {
        let nodes = &members[start[a]..start[a + 1]];
        let rows = nodes.len() * b;
        q.clear();
        q.resize(rows * mc, 0.0);
        for (m, &node) in nodes.iter().enumerate() {
            for d in 0..b {
                for j in 0..mc {
                    q[j * rows + m * b + d] = modes[(node * b + d) * mc + j];
                }
            }
        }
        let r = &mut coarse[a * mc * mc..(a + 1) * mc * mc];
        for j in 0..mc {
            let (done, rest) = q.split_at_mut(j * rows);
            let v = &mut rest[..rows];
            let before = dot(v, v).sqrt();
            // Modified Gram–Schmidt, twice for orthogonality to the last digits.
            for _ in 0..2 {
                for i in 0..j {
                    let qi = &done[i * rows..(i + 1) * rows];
                    let d = dot(qi, v);
                    r[i * mc + j] += d;
                    for (v, q) in v.iter_mut().zip(qi) {
                        *v -= d * q;
                    }
                }
            }
            let after = dot(v, v).sqrt();
            if after > 1e-10 * before && after > 0.0 {
                r[j * mc + j] = after;
                for x in v.iter_mut() {
                    *x /= after;
                }
            } else {
                // A motion the others already make here: no coarse unknown for it.
                v.fill(0.0);
            }
        }
        for (m, &node) in nodes.iter().enumerate() {
            for d in 0..b {
                for j in 0..mc {
                    t[(node * b + d) * mc + j] = q[j * rows + m * b + d];
                }
            }
        }
    }
    (t, coarse)
}

/// The smoothed prolongator `P = (I − ω D⁻¹ A) T`, `T` holding `b × mc` per node.
fn prolongator(
    a: BsrRef,
    ranges: &[(usize, usize)],
    dinv: &[f64],
    omega: f64,
    agg: &[u32],
    t: &[f64],
    count: usize,
) -> Result<Bsr, String> {
    let b = a.r;
    let mc = t.len() / (agg.len() * b).max(1);
    let width = b * mc;
    let parts = map_rows(ranges, |r0, r1| {
        let mut rows = RowBuilder::new(count, width);
        let mut tmp = [0.0; 36];
        for i in r0..r1 {
            for k in a.row(i) {
                let j = a.columns[k] as usize;
                if agg[j] == NONE {
                    continue;
                }
                let tj = &t[j * width..(j + 1) * width];
                mul_add(rows.slot(agg[j]), a.block(k), tj, b, b, mc);
            }
            let di = &dinv[i * b * b..(i + 1) * b * b];
            let ti = &t[i * width..(i + 1) * width];
            for (col, block) in rows.row_mut() {
                tmp[..width].fill(0.0);
                mul_add(&mut tmp[..width], di, block, b, b, mc);
                for (o, v) in block.iter_mut().zip(&tmp[..width]) {
                    *o = -omega * v;
                }
                if col == agg[i] {
                    for (o, v) in block.iter_mut().zip(ti) {
                        *o += v;
                    }
                }
            }
            rows.finish_row();
        }
        rows
    })?;
    Ok(concat(parts, b, mc))
}

/// `A P`.
fn times_prolongator(
    a: BsrRef,
    ranges: &[(usize, usize)],
    p: BsrRef,
    count: usize,
) -> Result<Bsr, String> {
    let mc = p.c;
    let b = a.r;
    let parts = map_rows(ranges, |r0, r1| {
        let mut rows = RowBuilder::new(count, b * mc);
        for i in r0..r1 {
            for k in a.row(i) {
                let j = a.columns[k] as usize;
                let block = a.block(k);
                for kp in p.row(j) {
                    mul_add(rows.slot(p.columns[kp]), block, p.block(kp), b, b, mc);
                }
            }
            rows.finish_row();
        }
        rows
    })?;
    Ok(concat(parts, b, mc))
}

/// The coarse matrix `Pᵀ (A P)`, row by row of the coarse level.
fn galerkin(
    p: BsrRef,
    columns: &Columns,
    ranges: &[(usize, usize)],
    ap: BsrRef,
    count: usize,
) -> Result<Bsr, String> {
    let mc = p.c;
    let b = p.r;
    let parts = map_rows(ranges, |c0, c1| {
        let mut rows = RowBuilder::new(count, mc * mc);
        for c in c0..c1 {
            for &(i, kp) in &columns.entries[columns.start[c]..columns.start[c + 1]] {
                let pb = p.block(kp);
                for k in ap.row(i as usize) {
                    mul_t_add(rows.slot(ap.columns[k]), pb, ap.block(k), mc, b, mc);
                }
            }
            rows.finish_row();
        }
        rows
    })?;
    let mut ac = concat(parts, mc, mc);
    // A rigid motion an aggregate could not carry left a zero row and column: a diagonal
    // keeps the coarse matrix invertible without coupling the unknown to anything. It is
    // the mean of the block's other diagonal terms, so that it weighs no more than they
    // do in the next level's strength of connection.
    for i in 0..count {
        let Some(k) = ac.view().diagonal(i) else {
            return Err(format!("coarse row {i} has no diagonal"));
        };
        let block = &mut ac.values[k * mc * mc..(k + 1) * mc * mc];
        let live: Vec<f64> = (0..mc)
            .map(|d| block[d * mc + d])
            .filter(|&v| v != 0.0)
            .collect();
        let fill = if live.is_empty() {
            1.0
        } else {
            live.iter().sum::<f64>() / live.len() as f64
        };
        for d in 0..mc {
            if block[d * mc + d] == 0.0 {
                block[d * mc + d] = fill;
            }
        }
    }
    Ok(ac)
}

/// A dense Cholesky factorization: the coarsest level's solve.
struct Dense {
    n: usize,
    /// The lower factor, row-major.
    l: Vec<f64>,
}

impl Dense {
    fn factor(a: BsrRef) -> Result<Dense, String> {
        let (b, rows) = (a.r, a.rows());
        let n = rows * b;
        let mut l = vec![0.0; n * n];
        for i in 0..rows {
            for k in a.row(i) {
                let j = a.columns[k] as usize;
                let block = a.block(k);
                for p in 0..b {
                    for q in 0..b {
                        l[(i * b + p) * n + j * b + q] = block[p * b + q];
                    }
                }
            }
        }
        for j in 0..n {
            let mut d = l[j * n + j];
            for k in 0..j {
                d -= l[j * n + k] * l[j * n + k];
            }
            if !(d > 0.0 && d.is_finite()) {
                return Err("the coarsest matrix is not positive definite".to_owned());
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
        Ok(Dense { n, l })
    }

    fn solve(&self, b: &[f64], x: &mut [f64]) {
        let (n, l) = (self.n, &self.l);
        for i in 0..n {
            let s = b[i] - dot(&l[i * n..i * n + i], &x[..i]);
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

// ---- The hierarchy and the cycle -----------------------------------------------------------

struct Level {
    /// This level's matrix; `None` on the finest, which is the caller's.
    a: Option<Bsr>,
    ranges: Vec<(usize, usize)>,
    dinv: Vec<f64>,
    /// Over the largest eigenvalue of `D⁻¹A`: the top of the smoother's interval.
    upper: f64,
    /// From the next level to this one.
    p: Bsr,
    p_ranges: Vec<(usize, usize)>,
    pt: Columns,
    pt_ranges: Vec<(usize, usize)>,
}

/// Vectors one level of the cycle works in.
struct Work {
    r: Vec<f64>,
    d: Vec<f64>,
    /// The next level's right-hand side and solution.
    coarse_b: Vec<f64>,
    coarse_x: Vec<f64>,
}

pub(crate) struct Multigrid<'a> {
    fine: BsrRef<'a>,
    levels: Vec<Level>,
    coarsest: Dense,
    work: Vec<Work>,
}

impl<'a> Multigrid<'a> {
    /// The hierarchy for the symmetric positive definite `fine`, whose near-null space
    /// is `modes`: `mode_count` columns per unknown (`fine.r × mode_count` per row of
    /// blocks, row-major), the six rigid motions for elasticity, a uniform value for
    /// conduction. `Err` says why it could not be built; the caller then falls back to
    /// (block-)Jacobi.
    pub(crate) fn new(
        fine: BsrRef<'a>,
        modes: &[f64],
        mode_count: usize,
        threads: usize,
    ) -> Result<Multigrid<'a>, String> {
        let mc = mode_count;
        if fine.r != fine.c
            || fine.r > 6
            || !(1..=6).contains(&mc)
            || modes.len() != fine.rows() * fine.r * mc
        {
            return Err("the matrix and its rigid motions do not match".to_owned());
        }
        let mut levels: Vec<Level> = Vec::new();
        let mut owned: Option<Bsr> = None;
        let mut modes = modes.to_vec();
        loop {
            let a = owned.as_ref().map_or(fine, Bsr::view);
            let unknowns = a.rows() * a.r;
            if unknowns <= COARSE_UNKNOWNS || levels.len() >= MAX_LEVELS {
                break;
            }
            let ranges = row_ranges(a.row_start, threads, a.values.len());
            let dinv = diagonal_inverses(a)?;
            let lambda = spectral_radius(a, &ranges, &dinv)?;
            let (agg, count) = aggregate(a);
            if count == 0 || count * 2 > a.rows() {
                // Coarsening has stalled.
                break;
            }
            let (t, coarse_modes) = tentative(&agg, count, a.r, mc, &modes);
            let p = prolongator(a, &ranges, &dinv, 4.0 / (3.0 * lambda), &agg, &t, count)?;
            let pt = columns_of(p.view(), count);
            let ap = times_prolongator(a, &ranges, p.view(), count)?;
            let pt_ranges = row_ranges(&pt.start, threads, ap.values.len());
            let ac = galerkin(p.view(), &pt, &pt_ranges, ap.view(), count)?;
            drop(ap);
            let p_ranges = row_ranges(&p.row_start, threads, p.values.len());
            let pt_ranges = row_ranges(&pt.start, threads, p.values.len());
            levels.push(Level {
                a: owned.take(),
                ranges,
                dinv,
                upper: SPECTRUM_MARGIN * lambda,
                p,
                p_ranges,
                pt,
                pt_ranges,
            });
            owned = Some(ac);
            modes = coarse_modes;
        }
        let a = owned.as_ref().map_or(fine, Bsr::view);
        if a.rows() * a.r > DIRECT_LIMIT {
            return Err(format!(
                "the coarsest level has {} unknowns, too many to factor",
                a.rows() * a.r
            ));
        }
        let coarsest = Dense::factor(a)?;
        let mut work = Vec::with_capacity(levels.len());
        for (l, level) in levels.iter().enumerate() {
            let n = level.a.as_ref().map_or(fine, Bsr::view).rows() * level.p.r;
            let coarse = match levels.get(l + 1) {
                Some(next) => next.a.as_ref().map_or(0, |a| a.view().rows() * mc),
                None => coarsest.n,
            };
            work.push(Work {
                r: vec![0.0; n],
                d: vec![0.0; n],
                coarse_b: vec![0.0; coarse],
                coarse_x: vec![0.0; coarse],
            });
        }
        Ok(Multigrid {
            fine,
            levels,
            coarsest,
            work,
        })
    }

    /// How many levels, the coarsest included.
    #[cfg(test)]
    fn depth(&self) -> usize {
        self.levels.len() + 1
    }

    /// `x = M⁻¹ b`: one V-cycle from zero.
    pub(crate) fn apply(&mut self, b: &[f64], x: &mut [f64]) {
        cycle(
            self.fine,
            &self.levels,
            &self.coarsest,
            &mut self.work,
            0,
            b,
            x,
        );
    }
}

fn cycle(
    fine: BsrRef,
    levels: &[Level],
    coarsest: &Dense,
    work: &mut [Work],
    l: usize,
    b: &[f64],
    x: &mut [f64],
) {
    let (Some(level), Some((w, rest))) = (levels.get(l), work.split_first_mut()) else {
        coarsest.solve(b, x);
        return;
    };
    let a = level.a.as_ref().map_or(fine, Bsr::view);
    smooth(a, level, b, x, true, &mut w.r, &mut w.d);
    multiply(a, &level.ranges, x, &mut w.r);
    for (r, b) in w.r.iter_mut().zip(b) {
        *r = b - *r;
    }
    let p = level.p.view();
    multiply_transposed(p, &level.pt, &level.pt_ranges, &w.r, &mut w.coarse_b);
    cycle(
        fine,
        levels,
        coarsest,
        rest,
        l + 1,
        &w.coarse_b,
        &mut w.coarse_x,
    );
    multiply(p, &level.p_ranges, &w.coarse_x, &mut w.d);
    for (x, d) in x.iter_mut().zip(&w.d) {
        *x += d;
    }
    smooth(a, level, b, x, false, &mut w.r, &mut w.d);
}

/// Chebyshev smoothing of `A x = b` on block-Jacobi, over `[upper / 30, upper]`; from
/// zero when `zero`, whatever `x` holds.
fn smooth(
    a: BsrRef,
    level: &Level,
    b: &[f64],
    x: &mut [f64],
    zero: bool,
    r: &mut [f64],
    d: &mut [f64],
) {
    let upper = level.upper;
    let lower = upper / SPECTRUM_RATIO;
    let theta = 0.5 * (upper + lower);
    let delta = 0.5 * (upper - lower);
    let sigma = theta / delta;
    let mut rho = 1.0 / sigma;
    for step in 0..DEGREE {
        if step == 0 && zero {
            r.copy_from_slice(b);
        } else {
            multiply(a, &level.ranges, x, r);
            for (r, b) in r.iter_mut().zip(b) {
                *r = b - *r;
            }
        }
        let (c1, c2) = if step == 0 {
            (0.0, 1.0 / theta)
        } else {
            let next = 1.0 / (2.0 * sigma - rho);
            let c = (next * rho, 2.0 * next / delta);
            rho = next;
            c
        };
        diagonal_step(&level.dinv, a.r, r, d, c1, c2);
        if step == 0 && zero {
            x.copy_from_slice(d);
        } else {
            for (x, d) in x.iter_mut().zip(d.iter()) {
                *x += d;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: usize = RIGID_MODES;

    /// The 1D Laplacian in 3 × 3 blocks (each unknown its own chain), `n` nodes, ends held.
    fn chain(n: usize) -> Bsr {
        let mut row_start = vec![0];
        let mut columns = Vec::new();
        let mut values = Vec::new();
        let id = |k: f64| [k, 0.0, 0.0, 0.0, k, 0.0, 0.0, 0.0, k];
        for i in 0..n {
            if i > 0 {
                columns.push((i - 1) as u32);
                values.extend(id(-1.0));
            }
            columns.push(i as u32);
            values.extend(id(2.0));
            if i + 1 < n {
                columns.push((i + 1) as u32);
                values.extend(id(-1.0));
            }
            row_start.push(columns.len());
        }
        Bsr {
            r: 3,
            c: 3,
            row_start,
            columns,
            values,
        }
    }

    /// A chain's hierarchy, with translations only for rigid motions (turns would need
    /// coordinates).
    fn chain_multigrid(a: &Bsr) -> Multigrid<'_> {
        let n = a.view().rows();
        let mut modes = vec![0.0; n * 3 * MODES];
        for i in 0..n {
            for d in 0..3 {
                modes[(i * 3 + d) * MODES + d] = 1.0;
            }
        }
        Multigrid::new(a.view(), &modes, MODES, 4).unwrap()
    }

    #[test]
    fn cycles_converge_on_a_chain_at_every_depth() {
        let n = 2000;
        let a = chain(n);
        let mut m = chain_multigrid(&a);
        // 2000 nodes, then 667, 223 (1338 unknowns), 75 (450): factored there.
        assert_eq!(m.depth(), 4);
        // The stationary iteration x += M⁻¹ (b − A x) gains a steady factor per cycle,
        // however many levels: about 0.6 with this smoother on a chain (elasticity in 3D
        // does far better).
        let b: Vec<f64> = (0..n * 3).map(|k| ((k % 17) as f64) - 8.0).collect();
        let mut x = vec![0.0; n * 3];
        let mut r = b.clone();
        let mut z = vec![0.0; n * 3];
        let ranges = row_ranges(&a.row_start, 1, 0);
        let first = dot(&r, &r).sqrt();
        for _ in 0..30 {
            m.apply(&r, &mut z);
            for (x, z) in x.iter_mut().zip(&z) {
                *x += z;
            }
            multiply(a.view(), &ranges, &x, &mut r);
            for (r, b) in r.iter_mut().zip(&b) {
                *r = b - *r;
            }
        }
        assert!(dot(&r, &r).sqrt() < 1e-6 * first);
    }

    #[test]
    fn the_cycle_is_symmetric() {
        // Conjugate gradients need u · M⁻¹v = v · M⁻¹u.
        let n = 1500;
        let a = chain(n);
        let mut m = chain_multigrid(&a);
        let u: Vec<f64> = (0..n * 3).map(|k| ((k * 37 % 101) as f64) - 50.0).collect();
        let v: Vec<f64> = (0..n * 3).map(|k| ((k * 53 % 89) as f64) - 44.0).collect();
        let (mut mu, mut mv) = (vec![0.0; n * 3], vec![0.0; n * 3]);
        m.apply(&u, &mut mu);
        m.apply(&v, &mut mv);
        let (a, b) = (dot(&u, &mv), dot(&v, &mu));
        assert!((a - b).abs() < 1e-10 * a.abs().max(b.abs()), "{a} vs {b}");
    }

    #[test]
    fn a_small_inverse() {
        let m = [4.0, 1.0, 0.0, 1.0, 3.0, 1.0, 0.0, 1.0, 2.0];
        let mut inv = [0.0; 9];
        assert!(invert(&m, 3, &mut inv));
        let mut product = [0.0; 9];
        mul_add(&mut product, &m, &inv, 3, 3, 3);
        for i in 0..3 {
            for j in 0..3 {
                let e = if i == j { 1.0 } else { 0.0 };
                assert!((product[i * 3 + j] - e).abs() < 1e-14);
            }
        }
        assert!(!invert(&[1.0, 2.0, 2.0, 4.0], 2, &mut [0.0; 4]));
    }
}
