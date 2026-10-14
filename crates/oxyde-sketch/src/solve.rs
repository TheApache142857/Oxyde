//! Solving a sketch and analysing what is left free.
//!
//! The unknowns are point coordinates and circle radii; every relation contributes one or
//! more equations `r(x) = 0`. Steps are minimum-norm Gauss–Newton steps
//! `Δ = −Jᵀ (J Jᵀ + μI)⁻¹ r`: among all moves that satisfy the linearised equations, the
//! smallest one. An under-defined sketch therefore changes as little as possible, which is
//! what a user dragging geometry expects. `μ` grows (towards gradient descent) when a step
//! does not reduce the error, which also copes with redundant and conflicting relations.

use crate::linalg::{null_space, rref, solve_linear};
use crate::sketch::{ConstrainedSketch, EntityId, Layout, RelationId, SketchError};

/// State of one relation after solving, as colour-coded in SolidWorks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintState {
    Satisfied,
    /// Satisfied, but implied by the relations before it (over-defining).
    Redundant,
    /// Cannot be satisfied together with the others.
    Conflicting,
}

/// Whether an entity can still move (blue in SolidWorks) or is fully defined (black).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Definition {
    Under,
    Fully,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SolveReport {
    /// All relations are satisfied. When false, the sketch was left unchanged.
    pub converged: bool,
    /// Norm of the remaining equation errors.
    pub residual: f64,
    pub iterations: usize,
    /// Independent ways the sketch can still move.
    pub degrees_of_freedom: usize,
    /// State of every relation, in the order of `ConstrainedSketch::relations`.
    pub relations: Vec<(RelationId, ConstraintState)>,
    /// Per entity, in the order of `ConstrainedSketch::entities`.
    pub entities: Vec<(EntityId, Definition)>,
}

impl SolveReport {
    pub fn is_fully_defined(&self) -> bool {
        self.converged && self.degrees_of_freedom == 0
    }

    pub fn state(&self, id: RelationId) -> Option<ConstraintState> {
        self.relations
            .iter()
            .find(|(r, _)| *r == id)
            .map(|(_, s)| *s)
    }

    /// Number of relations in a given state.
    pub fn count(&self, state: ConstraintState) -> usize {
        self.relations.iter().filter(|(_, s)| *s == state).count()
    }

    pub fn definition(&self, id: EntityId) -> Option<Definition> {
        self.entities
            .iter()
            .find(|(e, _)| *e == id)
            .map(|(_, d)| *d)
    }
}

const SOLVED: f64 = 1e-9;
const SATISFIED: f64 = 1e-6;
const MAX_ITERATIONS: usize = 100;

fn norm(r: &[f64]) -> f64 {
    r.iter().map(|v| v * v).sum::<f64>().sqrt()
}

impl ConstrainedSketch {
    /// Moves the geometry to satisfy every relation, then reports degrees of freedom and
    /// the state of each entity and relation. On failure the geometry is left unchanged.
    pub fn solve(&mut self) -> Result<SolveReport, SketchError> {
        let layout = self.layout()?;
        let free = layout.free_params();
        let mut params = layout.params.clone();

        let (iterations, converged) = self.iterate(&layout, &free, &mut params);
        let report = self.analyse(&layout, &free, &params, iterations, converged);
        if converged {
            self.store(&layout, &params);
        }
        Ok(report)
    }

    fn evaluate(&self, layout: &Layout, params: &[f64]) -> (Vec<f64>, Vec<Option<usize>>) {
        let mut r = Vec::new();
        let mut owners = Vec::new();
        self.residuals(layout, params, &mut r, &mut owners);
        (r, owners)
    }

    /// Central-difference Jacobian with respect to the free parameters.
    fn jacobian(&self, layout: &Layout, free: &[usize], params: &[f64]) -> Vec<Vec<f64>> {
        let mut work = params.to_vec();
        let rows = self.evaluate(layout, params).0.len();
        let mut j = vec![vec![0.0; free.len()]; rows];
        for (col, &i) in free.iter().enumerate() {
            let original = work[i];
            let h = 1e-7 * original.abs().max(1.0);
            work[i] = original + h;
            let plus = self.evaluate(layout, &work).0;
            work[i] = original - h;
            let minus = self.evaluate(layout, &work).0;
            work[i] = original;
            for row in 0..rows {
                j[row][col] = (plus[row] - minus[row]) / (2.0 * h);
            }
        }
        j
    }

    fn iterate(&self, layout: &Layout, free: &[usize], params: &mut Vec<f64>) -> (usize, bool) {
        let mut r = self.evaluate(layout, params).0;
        let mut error = norm(&r);
        for iteration in 0..MAX_ITERATIONS {
            if error < SOLVED {
                return (iteration, true);
            }
            if free.is_empty() {
                return (iteration, false);
            }
            let j = self.jacobian(layout, free, params);
            let m = r.len();
            let largest = j.iter().flatten().fold(0.0f64, |a, v| a.max(v.abs()));
            let mut mu = 1e-12 * (1.0 + largest * largest);
            let mut improved = false;

            for _ in 0..12 {
                let mut a = vec![vec![0.0; m]; m];
                for p in 0..m {
                    for q in p..m {
                        let v: f64 = j[p].iter().zip(&j[q]).map(|(x, y)| x * y).sum();
                        a[p][q] = v;
                        a[q][p] = v;
                    }
                    a[p][p] += mu;
                }
                if let Some(y) = solve_linear(a, r.clone()) {
                    let step: Vec<f64> = (0..free.len())
                        .map(|c| -(0..m).map(|row| j[row][c] * y[row]).sum::<f64>())
                        .collect();
                    let mut alpha = 1.0;
                    for _ in 0..16 {
                        let mut trial = params.clone();
                        for (c, &i) in free.iter().enumerate() {
                            trial[i] += alpha * step[c];
                        }
                        let trial_r = self.evaluate(layout, &trial).0;
                        let trial_error = norm(&trial_r);
                        if trial_error < error {
                            *params = trial;
                            r = trial_r;
                            error = trial_error;
                            improved = true;
                            break;
                        }
                        alpha *= 0.5;
                    }
                }
                if improved {
                    break;
                }
                mu = (mu * 1e3).max(1e-9);
            }
            if !improved {
                return (iteration, error < SOLVED);
            }
        }
        (MAX_ITERATIONS, error < SOLVED)
    }

    fn analyse(
        &self,
        layout: &Layout,
        free: &[usize],
        params: &[f64],
        iterations: usize,
        converged: bool,
    ) -> SolveReport {
        let (r, owners) = self.evaluate(layout, params);
        let n = free.len();
        let j = self.jacobian(layout, free, params);
        let largest = j.iter().flatten().fold(0.0f64, |a, v| a.max(v.abs()));
        let tolerance = 1e-8 * largest.max(1.0);

        let echelon = rref(j.clone(), n, tolerance);
        let null = null_space(&echelon, n);

        // A parameter is determined when no remaining motion changes it.
        let mut determined = vec![true; params.len()];
        for (c, &i) in free.iter().enumerate() {
            determined[i] = null.iter().all(|v| v[c].abs() < 1e-6);
        }
        let entities = self
            .entities
            .iter()
            .map(|e| {
                let fully = layout.params_of(e.id).iter().all(|&i| determined[i]);
                (
                    e.id,
                    if fully {
                        Definition::Fully
                    } else {
                        Definition::Under
                    },
                )
            })
            .collect();

        // Relations are taken in order: one that adds no independent equation is implied
        // by the ones before it.
        let mut rows: Vec<Vec<f64>> = owners
            .iter()
            .zip(&j)
            .filter(|(owner, _)| owner.is_none())
            .map(|(_, row)| row.clone())
            .collect();
        let mut rank = rref(rows.clone(), n, tolerance).rank;
        let mut relations = Vec::with_capacity(self.relations.len());
        for (index, relation) in self.relations.iter().enumerate() {
            let own: Vec<usize> = (0..owners.len())
                .filter(|&k| owners[k] == Some(index))
                .collect();
            if own.is_empty() {
                relations.push((relation.id, ConstraintState::Satisfied));
                continue;
            }
            rows.extend(own.iter().map(|&k| j[k].clone()));
            let new_rank = rref(rows.clone(), n, tolerance).rank;
            let independent = new_rank == rank + own.len();
            rank = new_rank;
            let unsatisfied = own.iter().any(|&k| r[k].abs() > SATISFIED);
            let state = if unsatisfied {
                ConstraintState::Conflicting
            } else if independent {
                ConstraintState::Satisfied
            } else {
                ConstraintState::Redundant
            };
            relations.push((relation.id, state));
        }

        SolveReport {
            converged,
            residual: norm(&r),
            iterations,
            degrees_of_freedom: n - echelon.rank,
            relations,
            entities,
        }
    }
}
