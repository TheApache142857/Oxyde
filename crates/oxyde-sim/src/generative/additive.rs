//! Additive manufacturing's overhang, as Langelaar's filter makes it (2016; in three
//! dimensions, 2017): printed layer by layer from the build plate up, a voxel can only be as
//! dense as what holds it up, the 3 × 3 voxels under it, which keeps every overhang within
//! 45° of the build direction on a cubic grid.
//!
//! Going up from the plate, each voxel's printed density is the smooth minimum of its
//! filtered density and the support under it, the smooth maximum of the printed densities
//! of those nine voxels (fewer at the grid's sides); the first layer stands on the plate.
//! Preserve voxels are printed whatever holds them, void ones never. The slopes go back down
//! the layers by the chain rule (the filter's adjoint), so the optimization sees what a
//! change costs in the layers it holds up.

// Indices into the grid's arrays, all sized here from the grid.
#![allow(clippy::indexing_slicing)]

use super::{Direction, Grid, State};

/// The smooth maximum's exponent, and the density at which it is exact (Langelaar's).
const P: f64 = 40.0;
const XI0: f64 = 0.5;
/// The smooth minimum's smoothing.
const EPSILON: f64 = 1e-4;

/// Printing along a build direction on a grid.
pub(super) struct Printing<'a> {
    grid: &'a Grid,
    /// The axis printed along, and whether the plate is at its low end.
    axis: usize,
    positive: bool,
}

/// What [`Printing::forward`] gives: the printed densities, and the support under each
/// voxel (its smooth maximum), which the slopes need.
pub(super) struct Printed {
    pub printed: Vec<f64>,
    support: Vec<f64>,
}

impl<'a> Printing<'a> {
    pub(super) fn new(grid: &'a Grid, direction: Direction) -> Self {
        Printing {
            grid,
            axis: direction.axis.min(2),
            positive: direction.positive,
        }
    }

    /// The layers' coordinates along the axis, from the plate up.
    fn layers(&self) -> Vec<usize> {
        let n = self.grid.counts[self.axis];
        if self.positive {
            (0..n).collect()
        } else {
            (0..n).rev().collect()
        }
    }

    /// The voxels of layer `t`.
    fn layer(&self, t: usize) -> impl Iterator<Item = usize> + '_ {
        let (a, b, c) = (self.axis, (self.axis + 1) % 3, (self.axis + 2) % 3);
        let n = self.grid.counts;
        (0..n[c]).flat_map(move |k| {
            (0..n[b]).map(move |j| {
                let mut at = [0; 3];
                at[a] = t;
                at[b] = j;
                at[c] = k;
                self.grid.index(at)
            })
        })
    }

    /// The voxels holding up voxel `v` (not on the plate): the 3 × 3 in the layer under it.
    fn supports(&self, v: usize) -> impl Iterator<Item = usize> + '_ {
        let (a, b, c) = (self.axis, (self.axis + 1) % 3, (self.axis + 2) % 3);
        let n = self.grid.counts;
        let at = self.grid.coords(v);
        let below = if self.positive {
            at[a].checked_sub(1)
        } else {
            Some(at[a] + 1).filter(|t| *t < n[a])
        };
        below.into_iter().flat_map(move |under| {
            (-1i64..=1).flat_map(move |dk| {
                (-1i64..=1).filter_map(move |dj| {
                    let j = at[b] as i64 + dj;
                    let k = at[c] as i64 + dk;
                    if j < 0 || k < 0 || j >= n[b] as i64 || k >= n[c] as i64 {
                        return None;
                    }
                    let mut w = [0; 3];
                    w[a] = under;
                    w[b] = j as usize;
                    w[c] = k as usize;
                    Some(self.grid.index(w))
                })
            })
        })
    }

    /// The printed densities of `filtered` (the free voxels'; preserve ones printed solid,
    /// void ones empty).
    pub(super) fn forward(&self, filtered: &[f64], state: &[State]) -> Printed {
        let count = filtered.len();
        let mut printed = vec![0.0; count];
        let mut support = vec![0.0; count];
        for (i, t) in self.layers().into_iter().enumerate() {
            let voxels: Vec<usize> = self.layer(t).collect();
            for v in voxels {
                printed[v] = match state[v] {
                    State::Solid => 1.0,
                    State::Void => 0.0,
                    State::Free if i == 0 => filtered[v],
                    State::Free => {
                        let s = smax(self.supports(v).map(|w| printed[w]));
                        support[v] = s;
                        smin(filtered[v], s)
                    }
                };
            }
        }
        Printed { printed, support }
    }

    /// The slopes `slopes` (for the printed densities) carried back to the filtered ones,
    /// down the layers.
    pub(super) fn backward(
        &self,
        filtered: &[f64],
        state: &[State],
        p: &Printed,
        slopes: &[f64],
    ) -> Vec<f64> {
        let mut carried = slopes.to_vec();
        let mut out = vec![0.0; filtered.len()];
        let layers = self.layers();
        for (i, &t) in layers.iter().enumerate().rev() {
            let voxels: Vec<usize> = self.layer(t).collect();
            for v in voxels {
                if state[v] != State::Free {
                    continue;
                }
                if i == 0 {
                    out[v] = carried[v];
                    continue;
                }
                let (dx, ds) = smin_slopes(filtered[v], p.support[v]);
                out[v] = carried[v] * dx;
                let down = carried[v] * ds;
                if down == 0.0 {
                    continue;
                }
                let supports: Vec<usize> = self.supports(v).collect();
                for (w, slope) in supports
                    .iter()
                    .zip(smax_slopes(supports.iter().map(|&w| p.printed[w])))
                {
                    carried[*w] += down * slope;
                }
            }
        }
        out
    }
}

/// The exponent that makes the smooth maximum of `n` values all at [`XI0`] exactly that.
fn exponent(n: usize) -> f64 {
    P + (n.max(1) as f64).ln() / XI0.ln()
}

/// Langelaar's smooth maximum: `(Σ xᴾ)^(1/Q)`.
fn smax(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), x| (s + x.max(0.0).powf(P), n + 1));
    if sum <= 0.0 {
        return 0.0;
    }
    sum.powf(1.0 / exponent(n))
}

/// The smooth maximum's slope for each value.
fn smax_slopes(values: impl Iterator<Item = f64> + Clone) -> Vec<f64> {
    let (sum, n) = values
        .clone()
        .fold((0.0, 0usize), |(s, n), x| (s + x.max(0.0).powf(P), n + 1));
    if sum <= 0.0 {
        return values.map(|_| 0.0).collect();
    }
    let q = exponent(n);
    let outer = sum.powf(1.0 / q - 1.0) * P / q;
    values.map(|x| outer * x.max(0.0).powf(P - 1.0)).collect()
}

/// The smooth minimum of `x` and `s`.
fn smin(x: f64, s: f64) -> f64 {
    0.5 * (x + s - ((x - s) * (x - s) + EPSILON).sqrt() + EPSILON.sqrt())
}

/// Its slopes for `x` and for `s`.
fn smin_slopes(x: f64, s: f64) -> (f64, f64) {
    let r = (x - s) / ((x - s) * (x - s) + EPSILON).sqrt();
    (0.5 * (1.0 - r), 0.5 * (1.0 + r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generative::Voxel;

    #[test]
    fn the_smooth_bounds_are_close_and_their_slopes_right() {
        // Nine supports at the threshold give it exactly; one solid support holds a voxel up.
        assert!((smax(std::iter::repeat_n(XI0, 9)) - XI0).abs() < 1e-12);
        assert!(smax([1.0, 0.0, 0.0].into_iter()) > 0.95);
        assert!(smax([0.0; 9].into_iter()) == 0.0);
        assert!((smin(1.0, 1.0) - 1.0).abs() < 1e-12);
        assert!(smin(0.8, 0.0) < 0.01 && (smin(0.3, 1.0) - 0.3).abs() < 0.01);
        // Slopes against finite differences.
        let h = 1e-6;
        for (x, s) in [(0.3, 0.6), (0.7, 0.2), (0.5, 0.5)] {
            let (dx, ds) = smin_slopes(x, s);
            assert!((dx - (smin(x + h, s) - smin(x - h, s)) / (2.0 * h)).abs() < 1e-5);
            assert!((ds - (smin(x, s + h) - smin(x, s - h)) / (2.0 * h)).abs() < 1e-5);
        }
        let values = [0.9, 0.4, 0.7];
        let slopes = smax_slopes(values.into_iter());
        for i in 0..3 {
            let mut up = values;
            up[i] += h;
            let mut down = values;
            down[i] -= h;
            let fd = (smax(up.into_iter()) - smax(down.into_iter())) / (2.0 * h);
            assert!((slopes[i] - fd).abs() < 1e-4 * fd.abs().max(1.0), "{i}");
        }
    }

    #[test]
    fn nothing_prints_over_air_and_the_slopes_match_the_densities() {
        // A column of 6 × 3 × 3: printed up +Y from y = 0.
        let counts = [3, 6, 3];
        let grid = Grid::from_parts(
            [0.0; 3],
            1.0,
            counts,
            vec![Voxel::Design; counts.iter().product()],
        )
        .unwrap();
        let state = vec![State::Free; grid.voxel_count()];
        let printing = Printing::new(
            &grid,
            Direction {
                axis: 1,
                positive: true,
            },
        );
        // Solid everywhere but an empty layer at y = 2: nothing above it prints.
        let filtered: Vec<f64> = (0..grid.voxel_count())
            .map(|v| if grid.coords(v)[1] == 2 { 0.0 } else { 1.0 })
            .collect();
        let p = printing.forward(&filtered, &state);
        for v in 0..grid.voxel_count() {
            let y = grid.coords(v)[1];
            if y < 2 {
                assert!(p.printed[v] > 0.99, "{v}");
            } else {
                // The smooth minimum leaves a trace, gone once projected.
                assert!(p.printed[v] < 0.05, "{v}: {}", p.printed[v]);
            }
        }
        // The adjoint against finite differences of Σ wᵢ ξᵢ.
        let filtered: Vec<f64> = (0..grid.voxel_count())
            .map(|v| 0.3 + 0.6 * ((v * 7919) % 13) as f64 / 13.0)
            .collect();
        let weights: Vec<f64> = (0..grid.voxel_count())
            .map(|v| 1.0 + (v % 5) as f64)
            .collect();
        let total = |f: &[f64]| -> f64 {
            printing
                .forward(f, &state)
                .printed
                .iter()
                .zip(&weights)
                .map(|(x, w)| x * w)
                .sum()
        };
        let p = printing.forward(&filtered, &state);
        let slopes = printing.backward(&filtered, &state, &p, &weights);
        let h = 1e-6;
        for v in [0, 13, 27, 40, 53] {
            let mut up = filtered.clone();
            up[v] += h;
            let mut down = filtered.clone();
            down[v] -= h;
            let fd = (total(&up) - total(&down)) / (2.0 * h);
            assert!(
                (slopes[v] - fd).abs() < 1e-5 * fd.abs().max(1.0),
                "{v}: {} {fd}",
                slopes[v]
            );
        }
    }
}
