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

/// Reduced row echelon form of a matrix with `columns` columns.
pub(crate) struct Echelon {
    pub rank: usize,
    /// Pivot column of each of the first `rank` rows.
    pub pivots: Vec<usize>,
    pub rows: Vec<Vec<f64>>,
}

/// Row-reduces `rows`, treating entries at or below `tolerance` as zero.
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
    Echelon { rank, pivots, rows }
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
    fn rank_and_null_space() {
        // Rank 1: the second row is twice the first.
        let m = vec![vec![1.0, 2.0, 0.0], vec![2.0, 4.0, 0.0]];
        let e = rref(m, 3, 1e-12);
        assert_eq!(e.rank, 1);
        let basis = null_space(&e, 3);
        assert_eq!(basis.len(), 2);
        for v in &basis {
            assert!((v[0] + 2.0 * v[1]).abs() < 1e-12);
        }
    }
}
