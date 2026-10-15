//! Small dense linear algebra, enough for sketch-sized systems (a few hundred unknowns).

/// Solves `a · x = b` by Gaussian elimination with partial pivoting.
/// Returns `None` when `a` is singular to working precision.
pub(crate) fn solve_linear(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    let scale = a
        .iter()
        .flatten()
        .fold(0.0f64, |m, v| m.max(v.abs()))
        .max(1e-300);
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() <= 1e-14 * scale {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..n {
            let (upper, lower) = a.split_at_mut(row);
            let (pivot_row, target) = (&upper[col], &mut lower[0]);
            let factor = target[col] / pivot_row[col];
            if factor != 0.0 {
                for k in col..n {
                    target[k] -= factor * pivot_row[k];
                }
                b[row] -= factor * b[col];
            }
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let mut sum = b[row];
        for k in row + 1..n {
            sum -= a[row][k] * x[k];
        }
        x[row] = sum / a[row][row];
    }
    Some(x)
}

/// Reduced row echelon form of a matrix with `columns` columns: one row per pivot.
pub(crate) struct Echelon {
    /// Pivot column of each row.
    pub pivots: Vec<usize>,
    pub rows: Vec<Vec<f64>>,
}

/// Row-reduces `rows`, treating entries at or below `tolerance` as zero. The reference the
/// incremental [`Basis`] is tested against.
#[cfg(test)]
pub(crate) fn rref(mut rows: Vec<Vec<f64>>, columns: usize, tolerance: f64) -> Echelon {
    let mut pivots = Vec::new();
    let mut rank = 0;
    for col in 0..columns {
        if rank == rows.len() {
            break;
        }
        let (best, value) = (rank..rows.len())
            .map(|i| (i, rows[i][col].abs()))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("non-empty range");
        if value <= tolerance {
            continue;
        }
        rows.swap(rank, best);
        let p = rows[rank][col];
        for v in rows[rank].iter_mut() {
            *v /= p;
        }
        let pivot_row = rows[rank].clone();
        for (i, row) in rows.iter_mut().enumerate() {
            if i == rank {
                continue;
            }
            let factor = row[col];
            if factor != 0.0 {
                for (v, pv) in row.iter_mut().zip(&pivot_row) {
                    *v -= factor * pv;
                }
            }
        }
        pivots.push(col);
        rank += 1;
    }
    rows.truncate(rank);
    Echelon { pivots, rows }
}

/// Rows added one at a time and kept reduced, telling whether each new row is independent
/// of the ones before it: the rank of a row reduction, built incrementally (each row costs
/// one pass over the rows already in, instead of reducing them all again).
pub(crate) struct Basis {
    tolerance: f64,
    /// Reduced rows with their pivot column: 1 there, and 0 at the pivot of every row
    /// added before them.
    rows: Vec<(usize, Vec<f64>)>,
}

impl Basis {
    pub fn new(tolerance: f64) -> Self {
        Self {
            tolerance,
            rows: Vec::new(),
        }
    }

    /// Adds `row`; true when it is independent of the rows already added (what is left of
    /// it after reduction counts as zero at or below the tolerance).
    pub fn add(&mut self, mut row: Vec<f64>) -> bool {
        for (pivot, reduced) in &self.rows {
            let factor = row[*pivot];
            if factor != 0.0 {
                for (v, r) in row.iter_mut().zip(reduced) {
                    *v -= factor * r;
                }
            }
        }
        let Some((pivot, size)) = row
            .iter()
            .map(|v| v.abs())
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(&b.1))
        else {
            return false;
        };
        if size <= self.tolerance {
            return false;
        }
        let p = row[pivot];
        row.iter_mut().for_each(|v| *v /= p);
        self.rows.push((pivot, row));
        true
    }

    /// Independent rows added so far.
    pub fn rank(&self) -> usize {
        self.rows.len()
    }

    /// The rows in reduced row echelon form (each pivot column zero in every other row),
    /// for [`null_space`].
    pub fn into_echelon(self) -> Echelon {
        let (pivots, mut rows): (Vec<usize>, Vec<Vec<f64>>) = self.rows.into_iter().unzip();
        // Each row is already zero at the pivots of the rows before it: clear the later ones,
        // last first.
        for i in (0..rows.len()).rev() {
            let (before, rest) = rows.split_at_mut(i);
            let row = &rest[0];
            for earlier in before.iter_mut() {
                let factor = earlier[pivots[i]];
                if factor != 0.0 {
                    for (v, r) in earlier.iter_mut().zip(row) {
                        *v -= factor * r;
                    }
                }
            }
        }
        Echelon { pivots, rows }
    }
}

/// Unit vectors spanning the null space of the matrix reduced to `echelon`.
pub(crate) fn null_space(echelon: &Echelon, columns: usize) -> Vec<Vec<f64>> {
    let mut basis = Vec::new();
    for free in (0..columns).filter(|c| !echelon.pivots.contains(c)) {
        let mut v = vec![0.0; columns];
        v[free] = 1.0;
        for (k, &pivot) in echelon.pivots.iter().enumerate() {
            v[pivot] = -echelon.rows[k][free];
        }
        let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        basis.push(v.into_iter().map(|x| x / norm).collect());
    }
    basis
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_a_small_system() {
        let a = vec![vec![2.0, 1.0], vec![1.0, 3.0]];
        let x = solve_linear(a, vec![3.0, 5.0]).unwrap();
        assert!((x[0] - 0.8).abs() < 1e-12 && (x[1] - 1.4).abs() < 1e-12);
    }

    #[test]
    fn singular_systems_are_detected() {
        let a = vec![vec![1.0, 2.0], vec![2.0, 4.0]];
        assert!(solve_linear(a, vec![1.0, 2.0]).is_none());
    }

    #[test]
    fn an_incremental_basis_finds_the_same_rank() {
        let rows = vec![
            vec![1.0, 2.0, 0.0, 1.0],
            vec![2.0, 4.0, 0.0, 2.0], // twice the first
            vec![0.0, 1.0, 1.0, 0.0],
            vec![1.0, 3.0, 1.0, 1.0], // first + third
            vec![0.0, 0.0, 0.0, 5.0],
        ];
        let mut basis = Basis::new(1e-12);
        let added: Vec<bool> = rows.iter().map(|r| basis.add(r.clone())).collect();
        assert_eq!(added, [true, false, true, false, true]);
        assert_eq!(rref(rows.clone(), 4, 1e-12).pivots.len(), 3);
        // Its null space is that of the matrix: one direction, orthogonal to every row.
        let null = null_space(&basis.into_echelon(), 4);
        assert_eq!(null.len(), 1);
        for row in &rows {
            let dot: f64 = row.iter().zip(&null[0]).map(|(a, b)| a * b).sum();
            assert!(dot.abs() < 1e-12);
        }
    }

    #[test]
    fn rank_and_null_space() {
        // Rank 1: the second row is twice the first.
        let m = vec![vec![1.0, 2.0, 0.0], vec![2.0, 4.0, 0.0]];
        let e = rref(m, 3, 1e-12);
        assert_eq!(e.pivots.len(), 1);
        let basis = null_space(&e, 3);
        assert_eq!(basis.len(), 2);
        for v in &basis {
            assert!((v[0] + 2.0 * v[1]).abs() < 1e-12);
        }
    }
}
