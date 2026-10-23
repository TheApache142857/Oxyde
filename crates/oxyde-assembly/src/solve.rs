use std::collections::HashMap;

use oxyde_geom::{Point3, Vec3};

use crate::mate::{
    Lock, closest_alignment, lock_angle, lock_directions, lock_residuals, residuals,
};
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
/// [`Assembly::apply_solution`] puts it in the assembly.
#[derive(Debug, Clone, PartialEq)]
pub struct Solution {
    pub placements: Vec<(ComponentId, Placement)>,
    pub mates: Vec<(MateId, MateStatus)>,
    pub freedom: Vec<ComponentFreedom>,
    /// The turn each mate with *Lock rotation* held ([`crate::Mate::locked_angle`]), to
    /// keep for the next solve.
    pub locks: Vec<(MateId, f64)>,
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
#[derive(Clone)]
struct Active {
    id: MateId,
    kind: crate::MateKind,
    aligned: bool,
    /// The way round a parallel-either-way mate is solved: see [`residuals`].
    fold: bool,
    a: (ComponentId, Primitive),
    b: (ComponentId, Primitive),
    /// *Lock rotation*, on a Concentric mate between two axes.
    lock: Option<Lock>,
}

/// Largest acceptable error of a held mate (mm, or mm-equivalents for directions).
const TOLERANCE: f64 = 1e-6;
const ITERATIONS: usize = 200;

/// Moves the components that are not fixed until the mates hold, starting from where they
/// are, so that the answer is the nearest one: what dragging a part expects. Each
/// component turns about its own origin; see [`solve_about`] to choose the point.
///
/// `local` gives the geometry a mate refers to in its component's own coordinates (see
/// [`crate::resolve`]). `size` is about the size of the assembly in mm; it balances
/// angles against distances.
pub fn solve(
    assembly: &Assembly,
    local: impl Fn(&MateRef) -> Result<Primitive, String>,
    size: f64,
) -> Solution {
    solve_about(assembly, local, |_| Point3::ORIGIN, size)
}

/// [`solve`], with each component turning about `center` (a point in its own
/// coordinates: the middle of its geometry, say). The solution is the same; which one is
/// found is not: a part modelled far from its origin, turned about that origin, would
/// swing right across the assembly to meet a mate it could have met by turning in place.
pub fn solve_about(
    assembly: &Assembly,
    local: impl Fn(&MateRef) -> Result<Primitive, String>,
    center: impl Fn(ComponentId) -> Point3,
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
        let known = |r: &MateRef| assembly.knows(r.component);
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
            (Ok(a), Ok(b)) => {
                let pa = assembly.placement_of(mate.a.component).unwrap_or_default();
                let pb = assembly.placement_of(mate.b.component).unwrap_or_default();
                let fold = closest_alignment(&a.placed(&pa), &b.placed(&pb));
                // The turn held is the one kept from before, else where the parts are now.
                let lock = (mate.lock_rotation && mate.kind.can_lock_rotation(a.kind(), b.kind()))
                    .then(|| lock_directions(&a, &b))
                    .flatten()
                    .and_then(|across| {
                        let angle = match mate.locked_angle {
                            Some(angle) => angle,
                            None => lock_angle(&a.placed(&pa), across, &pa, &pb)?,
                        };
                        Some(Lock {
                            a: across.0,
                            b: across.1,
                            angle,
                        })
                    });
                active.push(Active {
                    id: mate.id,
                    kind: mate.kind,
                    aligned: mate.aligned,
                    fold,
                    a: (mate.a.component, a),
                    b: (mate.b.component, b),
                    lock,
                })
            }
            (Err(e), _) | (_, Err(e)) => statuses.push((mate.id, MateStatus::Error(e))),
        }
    }

    // Mates that cannot apply to their geometry at all are reported, not solved.
    active.retain(|m| {
        let mut scratch = Vec::new();
        match residuals(m.kind, m.aligned, m.fold, &m.a.1, &m.b.1, size, &mut scratch) {
            Ok(()) => true,
            Err(e) => {
                statuses.push((m.id, MateStatus::Error(e)));
                false
            }
        }
    });

    let locks = active
        .iter()
        .filter_map(|m| m.lock.map(|lock| (m.id, lock.angle)))
        .collect();

    // Groups of mates tied together through components that may move (fixed components tie
    // nothing): each group is solved on its own, which costs far less than one system for
    // the whole assembly and gives the same answer.
    let fixed = |c: ComponentId| assembly.is_fixed(c);
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
        let solved = solve_group(assembly, &mates, &center, size);
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
        locks,
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
fn solve_group(
    assembly: &Assembly,
    active: &[&Active],
    center: &dyn Fn(ComponentId) -> Point3,
    size: f64,
) -> Solved {
    // Unknowns: a small rotation and a shift for each component that may move.
    let mut moving: Vec<ComponentId> = Vec::new();
    for m in active {
        for c in [m.a.0, m.b.0] {
            if !assembly.is_fixed(c) && !moving.contains(&c) {
                moving.push(c);
            }
        }
    }
    let mut base: HashMap<ComponentId, Placement> = active
        .iter()
        .flat_map(|m| [m.a.0, m.b.0])
        .filter_map(|c| assembly.placement_of(c).map(|p| (c, p)))
        .collect();
    let mut active: Vec<Active> = active.iter().map(|m| (*m).clone()).collect();
    half_turns(&mut active, &mut base, &moving, center, size);
    let active = &active;
    // Where each moving component turns, in the assembly.
    let centers: Vec<Point3> = moving
        .iter()
        .map(|c| base[c].apply_point(center(*c)))
        .collect();
    let placements = |x: &[f64]| -> HashMap<ComponentId, Placement> {
        let mut out = base.clone();
        for (i, c) in moving.iter().enumerate() {
            let p = &base[c];
            let v = &x[6 * i..6 * i + 6];
            out.insert(
                *c,
                p.moved(
                    Vec3::new(v[0], v[1], v[2]),
                    centers[i],
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
            let _ = residuals(m.kind, m.aligned, m.fold, &a, &b, size, &mut r);
            if let Some(lock) = &m.lock {
                match (placed.get(&m.a.0), placed.get(&m.b.0)) {
                    (Some(pa), Some(pb)) => lock_residuals(lock, &a, pa, pb, size, &mut r),
                    // Never: both sides are placed. The lock's three rows stay.
                    _ => r.extend([0.0; 3]),
                }
            }
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
            } else if n == 0 {
                MateStatus::Error(format!(
                    "both sides are fixed: Float one of them (off by {worst:.3})"
                ))
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

/// [`default_alignment`] for a mate between the faces or edges of two solids: when the
/// directions start square (as a screw beside its hole does) nothing is closest, and two
/// faces of solids nearly always touch, normals opposite — anti-aligned, as SolidWorks
/// starts a coincident mate between faces. Reference planes and axes keep the closest.
pub fn default_alignment_between(a: &Primitive, b: &Primitive, solids: bool) -> bool {
    match (a.direction(), b.direction()) {
        (Some(u), Some(v)) => {
            let d = u.dot(v);
            if solids { d >= 0.5 } else { d >= 0.0 }
        }
        _ => true,
    }
}

/// A mate whose alignment is the opposite of how its sides sit now (an alignment just
/// flipped, say) needs a half turn, which no small step finds: the error is highest
/// there, and an axis mate on the same component holds it either way round, so the two
/// cancel out for the whole first quarter turn. The component is turned half a turn
/// first, about the axis through its centre that keeps the other mates best (an axis it
/// is concentric with, when that lies across the mated normal), and the solve goes on
/// from there. The folds of the either-way mates are then taken again.
fn half_turns(
    active: &mut [Active],
    base: &mut HashMap<ComponentId, Placement>,
    moving: &[ComponentId],
    center: &dyn Fn(ComponentId) -> Point3,
    size: f64,
) {
    use std::f64::consts::PI;
    let placed = |m: &Active, base: &HashMap<ComponentId, Placement>| {
        (m.a.1.placed(&base[&m.a.0]), m.b.1.placed(&base[&m.b.0]))
    };
    let total = |active: &[Active], base: &HashMap<ComponentId, Placement>| -> f64 {
        let mut r = Vec::new();
        for m in active {
            let (a, b) = placed(m, base);
            let fold = closest_alignment(&a, &b);
            let _ = residuals(m.kind, m.aligned, fold, &a, &b, size, &mut r);
            // The turn that keeps a lock is the better one.
            if let (Some(lock), Some(pa), Some(pb)) = (&m.lock, base.get(&m.a.0), base.get(&m.b.0))
            {
                lock_residuals(lock, &a, pa, pb, size, &mut r);
            }
        }
        cost(&r)
    };
    let mut turned = Vec::new();
    for i in 0..active.len() {
        let m = &active[i];
        if !m.kind.has_alignment(m.a.1.kind(), m.b.1.kind())
            || matches!(m.kind, crate::MateKind::Angle(_))
        {
            continue;
        }
        let (a, b) = placed(m, base);
        let (Some(u), Some(v)) = (a.direction(), b.direction()) else {
            continue;
        };
        let sign = if m.aligned { 1.0 } else { -1.0 };
        if u.dot(v) * sign > -0.5 {
            continue;
        }
        // The side that turns: the second, when it can move.
        let (c, stays) = if moving.contains(&m.b.0) {
            (m.b.0, u)
        } else if moving.contains(&m.a.0) {
            (m.a.0, v)
        } else {
            continue;
        };
        if turned.contains(&c) {
            continue;
        }
        // Axes to turn about: across the normal that stays, from the directions of the
        // other mates on this component, then any.
        let across = |d: Vec3| {
            let w = d - stays * d.dot(stays);
            (w.length() > 0.5).then(|| w * (1.0 / w.length()))
        };
        let mut axes: Vec<Vec3> = active
            .iter()
            .filter(|o| o.id != m.id && (o.a.0 == c || o.b.0 == c))
            .flat_map(|o| {
                let (oa, ob) = placed(o, base);
                [oa.direction(), ob.direction()]
            })
            .flatten()
            .filter_map(across)
            .collect();
        let any = if stays.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        axes.extend(across(any));
        let pivot = base[&c].apply_point(center(c));
        let best = axes
            .iter()
            .map(|axis| {
                let mut trial = base.clone();
                trial.insert(c, base[&c].moved(*axis * PI, pivot, Vec3::ZERO));
                let cost = total(active, &trial);
                (cost, trial)
            })
            .min_by(|x, y| x.0.total_cmp(&y.0));
        if let Some((cost, trial)) = best {
            if cost < total(active, base) {
                *base = trial;
                turned.push(c);
            }
        }
    }
    if !turned.is_empty() {
        for m in active.iter_mut() {
            let (a, b) = placed(m, base);
            m.fold = closest_alignment(&a, &b);
        }
    }
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
    let mut r = f(x);
    let mut current = cost(&r);
    let mut lambda = 1e-3;
    let done = (TOLERANCE * size.max(1.0) * 1e-3).powi(2);
    for _ in 0..ITERATIONS {
        if current <= done {
            break;
        }
        let mut step = descend(x, f, &numeric_jacobian(x, f), &r, current, &mut lambda);
        if step.is_none() {
            // Every way looks level from here although the mates do not hold: a mate that
            // starts on a symmetric spot (faces in line for an angle, points together for
            // a distance, as inserted parts often are) has no slope there. The slopes a
            // little way off show a way out; the step is still taken from here.
            let aside: Vec<f64> = x
                .iter()
                .enumerate()
                .map(|(k, v)| v + nudge(k, size))
                .collect();
            lambda = 1e-3;
            step = descend(x, f, &numeric_jacobian(&aside, f), &r, current, &mut lambda);
        }
        let Some((trial, rt, ct)) = step else {
            break;
        };
        let small = trial
            .iter()
            .zip(x.iter())
            .all(|(t, v)| (t - v).abs() < 1e-14);
        x.copy_from_slice(&trial);
        r = rt;
        current = ct;
        if small {
            break;
        }
    }
}

/// A damped Gauss–Newton step from `x` with Jacobian `j`, damped further until it lowers
/// the cost: the new point, its residuals and its cost, or `None` if it never does.
fn descend(
    x: &[f64],
    f: &dyn Fn(&[f64]) -> Vec<f64>,
    j: &[Vec<f64>],
    r: &[f64],
    current: f64,
    lambda: &mut f64,
) -> Option<(Vec<f64>, Vec<f64>, f64)> {
    let n = x.len();
    // Normal equations: (JᵀJ + λ diag) δ = −Jᵀr.
    let mut a = vec![vec![0.0; n]; n];
    let mut g = vec![0.0; n];
    for (row, ri) in j.iter().zip(r) {
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
    for _ in 0..12 {
        let mut m = a.clone();
        for (p, row) in m.iter_mut().enumerate() {
            row[p] += *lambda * (a[p][p] + 1e-9);
        }
        let Some(step) = solve_linear(m, g.clone()) else {
            *lambda *= 10.0;
            continue;
        };
        let trial: Vec<f64> = x.iter().zip(&step).map(|(a, b)| a + b).collect();
        let rt = f(&trial);
        let ct = cost(&rt);
        if ct < current {
            *lambda = (*lambda / 3.0).max(1e-12);
            return Some((trial, rt, ct));
        }
        *lambda *= 4.0;
    }
    None
}

/// A small fixed offset for unknown `k` (each component has a turn in radians, then a shift
/// in mm), lopsided so that no symmetric spot stays symmetric.
fn nudge(k: usize, size: f64) -> f64 {
    let wobble = ((k as f64 + 1.0) * 0.618_033_988_749_895).fract() - 0.5;
    let scale = if k % 6 < 3 { 1e-3 } else { 1e-3 * size };
    wobble * scale
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
