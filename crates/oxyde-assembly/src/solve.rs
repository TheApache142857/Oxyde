use std::collections::HashMap;

use oxyde_geom::Vec3;

use crate::mate::{closest_alignment, residuals};
use crate::{Assembly, ComponentId, MateId, MateRef, Placement, Primitive};

/// Whether a mate holds after solving.
#[derive(Debug, Clone, PartialEq)]
pub enum MateStatus {
    Ok,
    /// The mate cannot be met: its geometry is missing, or it fights other mates.
    Error(String),
}

/// How a component can still move once the mates hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentFreedom {
    pub component: ComponentId,
    /// Independent ways it can still move (0–6): 0 means fully defined.
    pub degrees: usize,
}

/// The result of [`solve`]: new placements, and how each mate and component fared.
#[derive(Debug, Clone, PartialEq)]
pub struct Solution {
    pub placements: Vec<(ComponentId, Placement)>,
    pub mates: Vec<(MateId, MateStatus)>,
    pub freedom: Vec<ComponentFreedom>,
}

impl Solution {
    pub fn status(&self, mate: MateId) -> Option<&MateStatus> {
        self.mates
            .iter()
            .find(|(id, _)| *id == mate)
            .map(|(_, s)| s)
    }

    pub fn degrees(&self, component: ComponentId) -> Option<usize> {
        self.freedom
            .iter()
            .find(|f| f.component == component)
            .map(|f| f.degrees)
    }

    pub fn errors(&self) -> usize {
        self.mates
            .iter()
            .filter(|(_, s)| matches!(s, MateStatus::Error(_)))
            .count()
    }
}

/// A mate whose two sides could be resolved, ready to solve.
struct Active {
    id: MateId,
    kind: crate::MateKind,
    aligned: bool,
    a: (ComponentId, Primitive),
    b: (ComponentId, Primitive),
}

/// Largest acceptable error of a held mate (mm, or mm-equivalents for directions).
const TOLERANCE: f64 = 1e-6;
const ITERATIONS: usize = 200;

/// Moves the components that are not fixed until the mates hold, starting from where they
/// are, so that the answer is the nearest one: what dragging a part expects.
///
/// `local` gives the geometry a mate refers to in its component's own coordinates (see
/// [`crate::resolve`]). `size` is about the size of the assembly in mm; it balances
/// angles against distances.
pub fn solve(
    assembly: &Assembly,
    local: impl Fn(&MateRef) -> Result<Primitive, String>,
    size: f64,
) -> Solution {
    let size = if size.is_finite() && size > 0.0 {
        size
    } else {
        100.0
    };
    let mut statuses = Vec::new();
    let mut active = Vec::new();
    for mate in &assembly.mates {
        if mate.suppressed {
            continue;
        }
        let known = |r: &MateRef| assembly.component(r.component).is_some();
        if !known(&mate.a) || !known(&mate.b) {
            statuses.push((
                mate.id,
                MateStatus::Error("a component is missing".to_owned()),
            ));
            continue;
        }
        if mate.a.component == mate.b.component {
            statuses.push((
                mate.id,
                MateStatus::Error("both sides are on the same component".to_owned()),
            ));
            continue;
        }
        match (local(&mate.a), local(&mate.b)) {
            (Ok(a), Ok(b)) => active.push(Active {
                id: mate.id,
                kind: mate.kind,
                aligned: mate.aligned,
                a: (mate.a.component, a),
                b: (mate.b.component, b),
            }),
            (Err(e), _) | (_, Err(e)) => statuses.push((mate.id, MateStatus::Error(e))),
        }
    }

    // Mates that cannot apply to their geometry at all are reported, not solved.
    active.retain(|m| {
        let mut scratch = Vec::new();
        match residuals(m.kind, m.aligned, &m.a.1, &m.b.1, size, &mut scratch) {
            Ok(()) => true,
            Err(e) => {
                statuses.push((m.id, MateStatus::Error(e)));
                false
            }
        }
    });

    // Groups of mates tied together through components that may move (fixed components tie
    // nothing): each group is solved on its own, which costs far less than one system for
    // the whole assembly and gives the same answer.
    let fixed = |c: ComponentId| assembly.component(c).is_some_and(|c| c.fixed);
    let mut group: Vec<usize> = (0..active.len()).collect();
    fn root(group: &mut [usize], mut i: usize) -> usize {
        while group[i] != i {
            group[i] = group[group[i]];
            i = group[i];
        }
        i
    }
    let mut owner: HashMap<ComponentId, usize> = HashMap::new();
    for (i, m) in active.iter().enumerate() {
        for c in [m.a.0, m.b.0] {
            if fixed(c) {
                continue;
            }
            match owner.get(&c) {
                Some(&j) => {
                    let (a, b) = (root(&mut group, i), root(&mut group, j));
                    group[a.max(b)] = a.min(b);
                }
                None => {
                    owner.insert(c, i);
                }
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut slot: HashMap<usize, usize> = HashMap::new();
    for i in 0..active.len() {
        let r = root(&mut group, i);
        let g = *slot.entry(r).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(i);
    }

    let mut placements = Vec::new();
    let mut freedom: HashMap<ComponentId, usize> = HashMap::new();
    for members in &groups {
        let mates: Vec<&Active> = members.iter().map(|&i| &active[i]).collect();
        let solved = solve_group(assembly, &mates, size);
        placements.extend(solved.placements);
        statuses.extend(solved.statuses);
        freedom.extend(solved.freedom);
    }
    // Keep the assembly's order of mates.
    statuses.sort_by_key(|(id, _)| assembly.mates.iter().position(|m| m.id == *id));
    let freedom = assembly
        .components
        .iter()
        .map(|c| ComponentFreedom {
            component: c.id,
            degrees: if c.fixed {
                0
            } else {
                freedom.get(&c.id).copied().unwrap_or(6)
            },
        })
        .collect();
    Solution {
        placements,
        mates: statuses,
        freedom,
    }
}

/// What solving one group of mates gives.
struct Solved {
    placements: Vec<(ComponentId, Placement)>,
    statuses: Vec<(MateId, MateStatus)>,
    /// Degrees of freedom of each component that may move.
    freedom: Vec<(ComponentId, usize)>,
}

/// Solves mates that share no moving component with any other mate.
fn solve_group(assembly: &Assembly, active: &[&Active], size: f64) -> Solved {
    // Unknowns: a small rotation and a shift for each component that may move.
    let mut moving: Vec<ComponentId> = Vec::new();
    for m in active {
        for c in [m.a.0, m.b.0] {
            let fixed = assembly.component(c).is_some_and(|c| c.fixed);
            if !fixed && !moving.contains(&c) {
                moving.push(c);
            }
        }
    }
    let base: HashMap<ComponentId, Placement> = active
        .iter()
        .flat_map(|m| [m.a.0, m.b.0])
        .filter_map(|c| assembly.component(c).map(|c| (c.id, c.placement)))
        .collect();
    let placements = |x: &[f64]| -> HashMap<ComponentId, Placement> {
        let mut out = base.clone();
        for (i, c) in moving.iter().enumerate() {
            let p = &base[c];
            let v = &x[6 * i..6 * i + 6];
            let center = oxyde_geom::Point3::ORIGIN + p.translation;
            out.insert(
                *c,
                p.moved(
                    Vec3::new(v[0], v[1], v[2]),
                    center,
                    Vec3::new(v[3], v[4], v[5]),
                ),
            );
        }
        out
    };
    // Residuals of each mate, and where they start in the full vector.
    let evaluate = |x: &[f64]| -> (Vec<f64>, Vec<(usize, usize)>) {
        let placed = placements(x);
        let mut r = Vec::new();
        let mut spans = Vec::new();
        for m in active {
            let start = r.len();
            let a = m.a.1.placed(&placed[&m.a.0]);
            let b = m.b.1.placed(&placed[&m.b.0]);
            // Errors were ruled out when the mate was accepted.
            let _ = residuals(m.kind, m.aligned, &a, &b, size, &mut r);
            spans.push((start, r.len()));
        }
        (r, spans)
    };

    let n = 6 * moving.len();
    // Residuals of the mates in `mask` only.
    let objective = |x: &[f64], mask: &[bool]| -> Vec<f64> {
        let (r, spans) = evaluate(x);
        spans
            .iter()
            .zip(mask)
            .filter(|(_, on)| **on)
            .flat_map(|((s, e), _)| r[*s..*e].to_vec())
            .collect()
    };
    let limit = TOLERANCE * size.max(1.0);
    // How far each mate is from holding.
    let misses = |x: &[f64]| -> Vec<f64> {
        let (r, spans) = evaluate(x);
        spans
            .iter()
            .map(|(s, e)| r[*s..*e].iter().fold(0.0f64, |w, v| w.max(v.abs())))
            .collect()
    };
    let run = |x: &mut Vec<f64>, mask: &[bool]| {
        if n > 0 {
            levenberg_marquardt(x, &|x| objective(x, mask), size);
        }
    };

    let mut mask = vec![true; active.len()];
    let mut x = vec![0.0; n];
    run(&mut x, &mask);
    if misses(&x).iter().any(|m| *m > limit) {
        // Something fights: add the mates one at a time, in order, and leave out each
        // one that cannot hold with those before it, as SolidWorks blames the newest.
        mask = vec![false; active.len()];
        x = vec![0.0; n];
        for i in 0..active.len() {
            mask[i] = true;
            let mut trial = x.clone();
            run(&mut trial, &mask);
            let miss = misses(&trial);
            if mask.iter().zip(&miss).all(|(on, m)| !on || *m <= limit) {
                x = trial;
            } else {
                mask[i] = false;
            }
        }
    }
    let miss = misses(&x);
    let statuses = active
        .iter()
        .zip(&mask)
        .zip(&miss)
        .map(|((m, on), worst)| {
            let status = if *on {
                MateStatus::Ok
            } else {
                MateStatus::Error(format!(
                    "cannot be met together with the other mates (off by {worst:.3})"
                ))
            };
            (m.id, status)
        })
        .collect();

    // What each component can still do, among the mates that hold: the motions of the
    // whole group that keep them (the null space of their Jacobian), seen on that
    // component. A component held only by one that can still move moves with it.
    let motions = if n > 0 {
        null_space(&numeric_jacobian(&x, &|x| objective(x, &mask)), n)
    } else {
        Vec::new()
    };
    let freedom = moving
        .iter()
        .enumerate()
        .map(|(k, c)| {
            let own: Vec<Vec<f64>> = motions
                .iter()
                .map(|v| v[6 * k..6 * k + 6].to_vec())
                .collect();
            (*c, rank(&own, 6))
        })
        .collect();

    let placed = placements(&x);
    Solved {
        placements: moving.iter().map(|c| (*c, placed[c])).collect(),
        statuses,
        freedom,
    }
}

/// The alignment a new mate between `a` and `b` (in assembly coordinates) should start
/// with: the one closest to how they sit now.
pub fn default_alignment(a: &Primitive, b: &Primitive) -> bool {
    closest_alignment(a, b)
}

fn cost(r: &[f64]) -> f64 {
    r.iter().map(|v| v * v).sum()
}

fn numeric_jacobian(x: &[f64], f: &dyn Fn(&[f64]) -> Vec<f64>) -> Vec<Vec<f64>> {
    let r0 = f(x);
    let mut j = vec![vec![0.0; x.len()]; r0.len()];
    let mut probe = x.to_vec();
    for k in 0..x.len() {
        let h = 1e-7;
        probe[k] = x[k] + h;
        let plus = f(&probe);
        probe[k] = x[k] - h;
        let minus = f(&probe);
        probe[k] = x[k];
        for (row, (p, m)) in j.iter_mut().zip(plus.iter().zip(&minus)) {
            row[k] = (p - m) / (2.0 * h);
        }
    }
    j
}

fn levenberg_marquardt(x: &mut [f64], f: &dyn Fn(&[f64]) -> Vec<f64>, size: f64) {
    let n = x.len();
    let mut r = f(x);
    let mut current = cost(&r);
    let mut lambda = 1e-3;
    let done = (TOLERANCE * size.max(1.0) * 1e-3).powi(2);
    for _ in 0..ITERATIONS {
        if current <= done {
            break;
        }
        let j = numeric_jacobian(x, f);
        // Normal equations: (JᵀJ + λ diag) δ = −Jᵀr.
        let mut a = vec![vec![0.0; n]; n];
        let mut g = vec![0.0; n];
        for (row, ri) in j.iter().zip(&r) {
            for p in 0..n {
                if row[p] == 0.0 {
                    continue;
                }
                g[p] -= row[p] * ri;
                for q in 0..n {
                    a[p][q] += row[p] * row[q];
                }
            }
        }
        let mut improved = false;
        for _ in 0..12 {
            let mut m = a.clone();
            for (p, row) in m.iter_mut().enumerate() {
                row[p] += lambda * (a[p][p] + 1e-9);
            }
            let Some(step) = solve_linear(m, g.clone()) else {
                lambda *= 10.0;
                continue;
            };
            let trial: Vec<f64> = x.iter().zip(&step).map(|(a, b)| a + b).collect();
            let rt = f(&trial);
            let ct = cost(&rt);
            if ct < current {
                let small = step.iter().all(|s| s.abs() < 1e-14);
                x.copy_from_slice(&trial);
                r = rt;
                current = ct;
                lambda = (lambda / 3.0).max(1e-12);
                improved = !small;
                break;
            }
            lambda *= 4.0;
        }
        if !improved {
            break;
        }
    }
}

/// Gaussian elimination with partial pivoting.
fn solve_linear(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-300 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..n {
            let factor = a[row][col] / a[col][col];
            if factor == 0.0 {
                continue;
            }
            let (top, bottom) = a.split_at_mut(row);
            for (target, source) in bottom[0][col..].iter_mut().zip(&top[col][col..]) {
                *target -= factor * source;
            }
            b[row] -= factor * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let sum: f64 = (row + 1..n).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - sum) / a[row][row];
    }
    Some(x)
}

/// Orthonormal basis of the span of `rows` (vectors of length `width`), by Gram–Schmidt;
/// what is left of a row below `1e-6` of the largest entry counts as nothing.
fn orthonormal_basis(rows: &[Vec<f64>], width: usize) -> Vec<Vec<f64>> {
    let scale = rows
        .iter()
        .flatten()
        .fold(0.0f64, |m, v| m.max(v.abs()))
        .max(1e-12);
    let mut basis: Vec<Vec<f64>> = Vec::new();
    for row in rows {
        let mut v = row.clone();
        // Twice, so that what is left is orthogonal to working precision.
        for _ in 0..2 {
            for b in &basis {
                let d: f64 = v.iter().zip(b).map(|(x, y)| x * y).sum();
                v.iter_mut().zip(b).for_each(|(x, y)| *x -= d * y);
            }
        }
        let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm > 1e-6 * scale {
            basis.push(v.into_iter().map(|x| x / norm).collect());
            if basis.len() == width {
                break;
            }
        }
    }
    basis
}

/// Numerical rank of a set of row vectors of length `width`.
fn rank(rows: &[Vec<f64>], width: usize) -> usize {
    orthonormal_basis(rows, width).len()
}

/// Orthonormal basis of the vectors `x` (length `width`) with `J x = 0`: the complement
/// of the rows' span, from the unit vectors.
fn null_space(jacobian: &[Vec<f64>], width: usize) -> Vec<Vec<f64>> {
    let mut basis = orthonormal_basis(jacobian, width);
    let rank = basis.len();
    let units: Vec<Vec<f64>> = (0..width)
        .map(|i| {
            let mut e = vec![0.0; width];
            e[i] = 1.0;
            e
        })
        .collect();
    for e in &units {
        if basis.len() == width {
            break;
        }
        let mut v = e.clone();
        for _ in 0..2 {
            for b in &basis {
                let d: f64 = v.iter().zip(b).map(|(x, y)| x * y).sum();
                v.iter_mut().zip(b).for_each(|(x, y)| *x -= d * y);
            }
        }
        let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm > 1e-6 {
            basis.push(v.into_iter().map(|x| x / norm).collect());
        }
    }
    basis.split_off(rank)
}
