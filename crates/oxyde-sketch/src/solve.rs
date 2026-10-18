//! Solving a sketch and analysing what is left free.
//!
//! The unknowns are point coordinates and circle radii; every relation contributes one or
//! more equations `r(x) = 0`. Steps are minimum-norm Gauss–Newton steps
//! `Δ = −Jᵀ (J Jᵀ + μI)⁻¹ r`: among all moves that satisfy the linearised equations, the
//! smallest one. An under-defined sketch therefore changes as little as possible, which is
//! what a user dragging geometry expects. `μ` grows (towards gradient descent) when a step
//! does not reduce the error, which also copes with redundant and conflicting relations.
//!
//! The equations are first split into clusters that share no unknown (fixed points are not
//! unknowns, so they tie nothing together), and each cluster is solved and analysed on its
//! own. A sketch is usually many small clusters (separate contours, each held by fixed
//! points or dimensions to the origin): dense linear algebra on each costs a tiny fraction
//! of the same work on the whole sketch, and the results are the same, since clusters do
//! not influence each other.

use crate::linalg::{Basis, null_space, solve_linear};
use crate::sketch::{
    ConstrainedSketch, Constraint, Dual, EntityId, Eval, Geometry, Layout, RelationId, Scalar,
    SketchError,
};

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
    /// Iterations of the slowest cluster of equations.
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

/// Equations that share unknowns, and those unknowns: solved and analysed together.
#[derive(Debug, Default)]
struct Cluster {
    /// Free parameters, as indices into the parameter vector.
    params: Vec<usize>,
    /// Arcs whose implicit equal-radius equation belongs to the cluster.
    arcs: Vec<EntityId>,
    /// Relations (indices into the sketch's relations) whose equations belong to it, in
    /// order.
    relations: Vec<usize>,
    /// Per equation (the arcs', then the relations', as `residuals` lists them): the
    /// parameters it involves, as positions in `params`. Every other derivative is zero.
    columns: Vec<Vec<usize>>,
}

impl Cluster {
    fn equations(&self) -> usize {
        self.arcs.len() + self.relations.len()
    }
}

/// The Jacobian of a cluster's equations, dense, with where its nonzero entries can be.
struct Jacobian {
    rows: Vec<Vec<f64>>,
    /// Per column, the rows that can have a nonzero entry in it.
    column_rows: Vec<Vec<usize>>,
}

/// A cluster's equations linearised at a point: the Jacobian `J` and `J Jᵀ`.
struct Linearized {
    jacobian: Jacobian,
    jjt: Vec<Vec<f64>>,
    /// The largest entry of `J`, for scaling the damping.
    largest: f64,
}

impl Linearized {
    /// The minimum-norm step `−Jᵀ (J Jᵀ + μI)⁻¹ r`, or `None` when the system is singular.
    fn step(&self, r: &[f64], mu: f64) -> Option<Vec<f64>> {
        let mut a = self.jjt.clone();
        for (p, row) in a.iter_mut().enumerate() {
            row[p] += mu;
        }
        let y = solve_linear(a, r.to_vec())?;
        let j = &self.jacobian.rows;
        Some(
            self.jacobian
                .column_rows
                .iter()
                .enumerate()
                .map(|(c, rows)| -rows.iter().map(|&row| j[row][c] * y[row]).sum::<f64>())
                .collect(),
        )
    }
}

/// Union–find over the free parameters, by their position in the list of free parameters.
struct Sets(Vec<usize>);

impl Sets {
    fn find(&mut self, mut k: usize) -> usize {
        while self.0[k] != k {
            self.0[k] = self.0[self.0[k]];
            k = self.0[k];
        }
        k
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

impl ConstrainedSketch {
    /// Moves the geometry to satisfy every relation, then reports degrees of freedom and
    /// the state of each entity and relation. On failure the geometry is left unchanged.
    pub fn solve(&mut self) -> Result<SolveReport, SketchError> {
        self.solve_holding(&[])
    }

    /// Like [`solve`](Self::solve), with the points `held` kept where they are as if they
    /// were fixed: dragging holds the dragged point under the cursor this way while the
    /// rest follows. The report counts them as fixed.
    pub fn solve_holding(&mut self, held: &[EntityId]) -> Result<SolveReport, SketchError> {
        let mut layout = self.layout()?;
        for &point in held {
            if layout.point.contains_key(&point) && !layout.fixed.contains(&point) {
                layout.fixed.push(point);
            }
        }
        let free = layout.free_params();
        let mut params = layout.params.clone();
        let clusters = self.clusters(&layout, &free);

        let mut iterations = 0;
        let mut converged = true;
        for cluster in &clusters {
            let (used, done) = self.iterate(&layout, cluster, &mut params);
            iterations = iterations.max(used);
            converged &= done;
        }
        let report = self.analyse(&layout, &clusters, &params, iterations, converged);
        if converged {
            self.store(&layout, &params);
        }
        Ok(report)
    }

    /// Splits the equations into clusters sharing no free parameter. Equations on fixed
    /// points only make clusters of their own, without unknowns; free parameters no
    /// equation touches too.
    fn clusters(&self, layout: &Layout, free: &[usize]) -> Vec<Cluster> {
        let mut slot = vec![usize::MAX; layout.params.len()];
        for (k, &i) in free.iter().enumerate() {
            slot[i] = k;
        }
        let free_of = |entities: &[EntityId]| -> Vec<usize> {
            let mut out: Vec<usize> = entities
                .iter()
                .flat_map(|e| layout.params_of(*e))
                .map(|i| slot[i])
                .filter(|&k| k != usize::MAX)
                .collect();
            out.sort_unstable();
            out.dedup();
            out
        };
        // Each equation (an arc's, or a relation's) with the free parameters it involves.
        enum Equation {
            Arc(EntityId),
            Relation(usize),
        }
        let mut equations: Vec<(Equation, Vec<usize>)> = Vec::new();
        for e in &self.entities {
            if matches!(e.geometry, Geometry::Arc { .. }) {
                equations.push((Equation::Arc(e.id), free_of(&[e.id])));
            }
        }
        for (index, relation) in self.relations.iter().enumerate() {
            // Fixed points are left out of the unknowns: no equation.
            if !matches!(relation.constraint, Constraint::Fixed(_)) {
                let entities = relation.constraint.entities();
                equations.push((Equation::Relation(index), free_of(&entities)));
            }
        }

        let mut sets = Sets((0..free.len()).collect());
        for (_, involved) in &equations {
            for pair in involved.windows(2) {
                sets.union(pair[0], pair[1]);
            }
        }
        // One cluster per set of parameters, numbered by its first parameter.
        let mut cluster_of = vec![usize::MAX; free.len()];
        // Position of each free parameter in its cluster's parameters.
        let mut column = vec![0; free.len()];
        let mut clusters: Vec<Cluster> = Vec::new();
        for k in 0..free.len() {
            let root = sets.find(k);
            if cluster_of[root] == usize::MAX {
                cluster_of[root] = clusters.len();
                clusters.push(Cluster::default());
            }
            let cluster = &mut clusters[cluster_of[root]];
            column[k] = cluster.params.len();
            cluster.params.push(free[k]);
        }
        // Arcs' equations come first in every cluster, as `residuals` lists them.
        let mut arc_columns: Vec<Vec<Vec<usize>>> = vec![Vec::new(); clusters.len()];
        let mut relation_columns: Vec<Vec<Vec<usize>>> = vec![Vec::new(); clusters.len()];
        for (equation, involved) in equations {
            let target = match involved.first() {
                Some(&k) => cluster_of[sets.find(k)],
                None => {
                    clusters.push(Cluster::default());
                    arc_columns.push(Vec::new());
                    relation_columns.push(Vec::new());
                    clusters.len() - 1
                }
            };
            let columns = involved.iter().map(|&k| column[k]).collect();
            match equation {
                Equation::Arc(id) => {
                    clusters[target].arcs.push(id);
                    arc_columns[target].push(columns);
                }
                Equation::Relation(index) => {
                    clusters[target].relations.push(index);
                    relation_columns[target].push(columns);
                }
            }
        }
        for ((cluster, arcs), relations) in
            clusters.iter_mut().zip(arc_columns).zip(relation_columns)
        {
            cluster.columns = arcs.into_iter().chain(relations).collect();
        }
        clusters
    }

    /// Appends the residuals of equation `e` of a cluster (numbered as in `columns`).
    fn equation<T: Scalar, P: Fn(usize) -> T>(
        &self,
        eval: &Eval<'_, T, P>,
        cluster: &Cluster,
        e: usize,
        out: &mut Vec<T>,
    ) {
        let arcs = cluster.arcs.len();
        let (arc, relation) = if e < arcs {
            (&cluster.arcs[e..e + 1], &[][..])
        } else {
            (&[][..], &cluster.relations[e - arcs..e - arcs + 1])
        };
        self.residuals_with(eval, arc, relation, out, None);
    }

    fn evaluate(&self, layout: &Layout, cluster: &Cluster, params: &[f64]) -> Vec<f64> {
        let mut r = Vec::new();
        self.residuals(
            layout,
            params,
            &cluster.arcs,
            &cluster.relations,
            &mut r,
            None,
        );
        r
    }

    /// The exact Jacobian of the cluster's equations with respect to its parameters,
    /// differentiated with dual numbers. Each derivative only evaluates the equation it is
    /// in, so the cost grows with the size of the cluster, not its square.
    fn jacobian(&self, layout: &Layout, cluster: &Cluster, params: &[f64]) -> Jacobian {
        let n = cluster.params.len();
        let values = Eval {
            layout,
            param: |i: usize| params[i],
        };
        // First row of each equation.
        let mut first = Vec::with_capacity(cluster.equations() + 1);
        first.push(0);
        let mut scratch = Vec::new();
        for e in 0..cluster.equations() {
            scratch.clear();
            self.equation(&values, cluster, e, &mut scratch);
            first.push(first[e] + scratch.len());
        }
        let m = first[cluster.equations()];

        let mut rows = vec![vec![0.0; n]; m];
        let mut column_rows: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut out = Vec::new();
        for (e, columns) in cluster.columns.iter().enumerate() {
            for &col in columns {
                column_rows[col].extend(first[e]..first[e + 1]);
                let seed = cluster.params[col];
                let along = Eval {
                    layout,
                    param: |i: usize| Dual {
                        value: params[i],
                        derivative: if i == seed { 1.0 } else { 0.0 },
                    },
                };
                out.clear();
                self.equation(&along, cluster, e, &mut out);
                for (k, v) in out.iter().enumerate() {
                    rows[first[e] + k][col] = v.derivative;
                }
            }
        }
        Jacobian { rows, column_rows }
    }

    /// The cluster's equations linearised at `params`, ready for Gauss–Newton steps.
    fn linearize(&self, layout: &Layout, cluster: &Cluster, params: &[f64]) -> Linearized {
        let jacobian = self.jacobian(layout, cluster, params);
        let j = &jacobian.rows;
        let m = j.len();
        let largest = j.iter().flatten().fold(0.0f64, |a, v| a.max(v.abs()));
        // J Jᵀ, summed over the nonzero entries of each column.
        let mut jjt = vec![vec![0.0; m]; m];
        for (c, rows) in jacobian.column_rows.iter().enumerate() {
            for (x, &p) in rows.iter().enumerate() {
                for &q in &rows[x..] {
                    let v = j[p][c] * j[q][c];
                    jjt[p][q] += v;
                    if p != q {
                        jjt[q][p] += v;
                    }
                }
            }
        }
        Linearized {
            jacobian,
            jjt,
            largest,
        }
    }

    /// Solves one cluster, changing only its parameters in `params`.
    fn iterate(&self, layout: &Layout, cluster: &Cluster, params: &mut [f64]) -> (usize, bool) {
        let free = &cluster.params;
        let mut r = self.evaluate(layout, cluster, params);
        let mut error = norm(&r);
        for iteration in 0..MAX_ITERATIONS {
            if error < SOLVED {
                return (iteration, true);
            }
            if free.is_empty() {
                return (iteration, false);
            }
            let linearized = self.linearize(layout, cluster, params);
            let mut mu = 1e-12 * (1.0 + linearized.largest * linearized.largest);
            let mut improved = false;
            let start: Vec<f64> = free.iter().map(|&i| params[i]).collect();

            for attempt in 0..12 {
                if let Some(step) = linearized.step(&r, mu) {
                    let mut alpha = 1.0;
                    for _ in 0..16 {
                        for (c, &i) in free.iter().enumerate() {
                            params[i] = start[c] + alpha * step[c];
                        }
                        let trial_r = self.evaluate(layout, cluster, params);
                        let trial_error = norm(&trial_r);
                        if trial_error < error {
                            r = trial_r;
                            error = trial_error;
                            improved = true;
                            break;
                        }
                        // Watchdog: a full step can overshoot on one strongly nonlinear
                        // relation (a line turned a long way swings far from a point that
                        // must stay near it) and still land where the next step converges.
                        // Halving it instead would only crawl.
                        if alpha == 1.0 && attempt == 0 {
                            let next = self.linearize(layout, cluster, params);
                            if let Some(second) = next.step(&trial_r, mu) {
                                for (c, &i) in free.iter().enumerate() {
                                    params[i] += second[c];
                                }
                                let second_r = self.evaluate(layout, cluster, params);
                                let second_error = norm(&second_r);
                                if second_error < error {
                                    r = second_r;
                                    error = second_error;
                                    improved = true;
                                    break;
                                }
                            }
                        }
                        alpha *= 0.5;
                    }
                }
                if improved {
                    break;
                }
                for (c, &i) in free.iter().enumerate() {
                    params[i] = start[c];
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
        clusters: &[Cluster],
        params: &[f64],
        iterations: usize,
        converged: bool,
    ) -> SolveReport {
        let mut determined = vec![true; params.len()];
        let mut states = vec![ConstraintState::Satisfied; self.relations.len()];
        let mut degrees_of_freedom = 0;
        let mut residual = 0.0;

        for cluster in clusters {
            let mut r = Vec::new();
            let mut owners = Vec::new();
            self.residuals(
                layout,
                params,
                &cluster.arcs,
                &cluster.relations,
                &mut r,
                Some(&mut owners),
            );
            residual += r.iter().map(|v| v * v).sum::<f64>();
            let n = cluster.params.len();
            let j = self.jacobian(layout, cluster, params).rows;
            let largest = j.iter().flatten().fold(0.0f64, |a, v| a.max(v.abs()));
            let tolerance = 1e-8 * largest.max(1.0);

            // Relations are taken in order (after the arcs' own equations): one that adds no
            // independent equation is implied by the ones before it. The same rows give the
            // rank, so the degrees of freedom always agree with the relations' states.
            let mut basis = Basis::new(tolerance);
            let mut k = 0;
            while k < owners.len() && owners[k].is_none() {
                basis.add(j[k].clone());
                k += 1;
            }
            for &index in &cluster.relations {
                let own: Vec<usize> = (k..owners.len())
                    .take_while(|&row| owners[row] == Some(index))
                    .collect();
                k += own.len();
                if own.is_empty() {
                    continue;
                }
                let mut independent = true;
                for &row in &own {
                    independent &= basis.add(j[row].clone());
                }
                let unsatisfied = own.iter().any(|&row| r[row].abs() > SATISFIED);
                states[index] = if unsatisfied {
                    ConstraintState::Conflicting
                } else if independent {
                    ConstraintState::Satisfied
                } else {
                    ConstraintState::Redundant
                };
            }
            degrees_of_freedom += n - basis.rank();
            // A parameter is determined when no remaining motion changes it.
            let null = null_space(&basis.into_echelon(), n);
            for (c, &i) in cluster.params.iter().enumerate() {
                determined[i] = null.iter().all(|v| v[c].abs() < 1e-6);
            }
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
        SolveReport {
            converged,
            residual: residual.sqrt(),
            iterations,
            degrees_of_freedom,
            relations: self
                .relations
                .iter()
                .zip(states)
                .map(|(relation, state)| (relation.id, state))
                .collect(),
            entities,
        }
    }
}

#[cfg(test)]
mod tests {
    use oxyde_geom::Point2;

    use crate::sketch::{ConstrainedSketch, Constraint::*, Dual, Eval};

    /// Every kind of relation, on geometry in general position.
    fn every_relation() -> ConstrainedSketch {
        let mut s = ConstrainedSketch::new();
        let a = s.line(Point2::new(1.0, 2.0), Point2::new(9.0, 5.0));
        let b = s.line(Point2::new(-3.0, 4.0), Point2::new(2.0, 11.0));
        let c = s.circle(Point2::new(5.0, -6.0), 2.5);
        let arc = s.arc(
            Point2::new(-8.0, -2.0),
            Point2::new(-5.0, -1.0),
            Point2::new(-9.0, 1.0),
        );
        let p = s.point(Point2::new(3.5, -1.5));
        let q = s.point(Point2::new(-2.0, -4.5));
        // A line ending on the arc: tangent there is measured differently.
        let crate::Geometry::Arc { end, .. } = s.entity(arc).unwrap().geometry else {
            panic!()
        };
        let tip = s.point(Point2::new(-12.0, 3.0));
        let joined = s.line_between(end, tip);
        for constraint in [
            Tangent(joined, arc),
            Coincident(p, q),
            Horizontal(a),
            Vertical(b),
            HorizontalPoints(p, q),
            VerticalPoints(q, p),
            Parallel(a, b),
            Perpendicular(a, b),
            EqualLength(a, b),
            EqualRadius(c, arc),
            Distance(p, q, 3.0),
            HorizontalDistance(p, q, 2.0),
            VerticalDistance(p, q, 2.0),
            Length(a, 4.0),
            Radius(arc, 2.0),
            Diameter(c, 5.0),
            Angle(a, b, 0.7),
            PointOnLine(p, a),
            PointOnCircle(q, c),
            Midpoint(p, b),
            Tangent(a, c),
            Tangent(b, arc),
            PointLineDistance(q, b, 1.5),
            LineDistance(a, b, 2.0),
            Symmetric(p, q, a),
            EqualDistance(a, b, b, a),
        ] {
            s.constrain(constraint);
        }
        s
    }

    #[test]
    fn exact_derivatives_match_finite_differences() {
        let sketch = every_relation();
        let layout = sketch.layout().unwrap();
        let params = layout.params.clone();
        let arcs: Vec<_> = sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, crate::Geometry::Arc { .. }))
            .map(|e| e.id)
            .collect();
        let relations: Vec<usize> = (0..sketch.relations().len()).collect();
        let value = |params: &[f64]| {
            let mut out = Vec::new();
            sketch.residuals(&layout, params, &arcs, &relations, &mut out, None);
            out
        };
        for seed in 0..params.len() {
            let eval = Eval {
                layout: &layout,
                param: |i: usize| Dual {
                    value: params[i],
                    derivative: if i == seed { 1.0 } else { 0.0 },
                },
            };
            let mut exact = Vec::new();
            sketch.residuals_with(&eval, &arcs, &relations, &mut exact, None);
            let h = 1e-6;
            let (mut plus, mut minus) = (params.clone(), params.clone());
            plus[seed] += h;
            minus[seed] -= h;
            let (fp, fm) = (value(&plus), value(&minus));
            for (row, d) in exact.iter().enumerate() {
                let numeric = (fp[row] - fm[row]) / (2.0 * h);
                assert!(
                    (d.derivative - numeric).abs() < 1e-6 * numeric.abs().max(1.0),
                    "row {row}, parameter {seed}: exact {} numeric {numeric}",
                    d.derivative
                );
                assert_eq!(d.value, value(&params)[row]);
            }
        }
    }
}
