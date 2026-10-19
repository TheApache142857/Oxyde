//! Topology optimization: the stiffest shape that keeps a given share of the model's mass,
//! as SolidWorks' topology study and Fusion 360's shape optimization find it.
//!
//! - **Densities.** Every element gets a density from 0 (void) to 1 (solid), the design
//!   variables, and its stiffness is the material's times `ε + ρ̃³ (1 − ε)` (SIMP, solid
//!   isotropic material with penalization: half a density gives an eighth of the
//!   stiffness, so material in between does not pay and the optimum comes out nearly solid
//!   or void). `ε` keeps the matrix positive definite where the material is gone.
//! - **The filter.** The densities the stiffness sees, `ρ̃`, are the design densities
//!   averaged over a sphere of radius `r`, the weights falling linearly to zero at `r`
//!   (a density filter, as Bruns and Tortorelli, and Bourdin, wrote it): no member comes
//!   out thinner than about `2r`, no checkerboard of solid and void elements, and a finer
//!   mesh gives the same shape, smoother.
//! - **The objective** is the compliance, the work the loads do (`Fᵀu`, twice the energy
//!   the part stores): least compliance is most stiffness. Its slope for an element is
//!   `−3 ρ̃² (1 − ε) uₑᵀ Kₑ uₑ`, `Kₑ` the element's stiffness at full density, carried back
//!   through the filter to the design densities.
//! - **The update** is by optimality criteria (Bendsøe): each density is multiplied by the
//!   square root of its slope over the slope of the mass, moving by at most 0.2, with the
//!   multiplier found by bisection so that the mass is exactly the target (the filtered
//!   mass is linear in the design densities, so its slopes are worked out once).
//! - **Kept regions**: the elements on the faces of fixtures and loads stay solid (else a
//!   load could be left on nothing), and so do those within a preserved region's depth of
//!   its faces.
//! - **Manufacturing controls**, as SolidWorks' topology study and Fusion 360's shape
//!   optimization have them. *Symmetry* makes the filter average around each element's
//!   mirror images as well as around its centre, so the filtered densities are a
//!   mirror-symmetric field whatever the loads, on any mesh. A *pull direction* caps each
//!   filtered density at the one behind it (against the direction, or toward a parting
//!   plane on either side of it), back to front, so no material hangs over void a mould
//!   could not be pulled out of; an element's slopes go to the one whose density it took.
//!   The mass is then linear in the design only while those stay the same, so its slopes
//!   are worked out again each iteration. Kept regions are mirrored too, and keep the
//!   elements behind them.
//! - Each iteration is one static solve ([`crate::solve`]), prepared once and started from
//!   the last one's displacements, so the later solves, nearly alike, take few iterations.
//!
//! [`shape`] turns the densities into a closed triangle surface at a threshold (marching
//! tetrahedra, capped where the shape meets the model's own faces), for the viewport and
//! for export; [`pieces`] tells how many pieces it falls into and what each holds. With
//! *Keep it in one piece* ([`TopologyStudy::one_piece`]), pieces that hold a fixture, a load
//! or a kept region but that the loads do not reach are joined to the rest by a bar as thick
//! as the thinnest member, along the path that needs the least material added, and loose
//! bits holding none of them are taken away: the minimum compliance has no reason to join
//! what carries no load, and a part has to be in one piece.

// Indices into the mesh (checked by `TetMesh::validate` when the study is prepared) and
// into arrays sized here, one value per element or per node.
#![allow(clippy::indexing_slicing)]

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::material::Elastic;
use crate::math::{V3, add, cross, dot, lerp, norm, normalize, scale, sub};
use crate::mesh::{FACES, TetMesh};
use crate::solve::{
    Preconditioner, Prepared, Problem, SolveError, SolveOptions, Study, element_energies,
    thread_count,
};

/// The stiffness left in void elements, as a fraction of the material's.
const VOID_STIFFNESS: f64 = 1e-6;
/// SIMP's penalty: stiffness goes as the density to this power.
const PENALTY: f64 = 3.0;
/// The residual the iterations' solves stop at, as a fraction of the loads: what their
/// slopes need, and a fifth fewer conjugate gradient iterations than a static study's
/// tolerance. The whole part's solve keeps the study's, its compliance being reported.
const ITERATION_TOLERANCE: f64 = 1e-5;
/// The most a design density moves in one iteration.
const MOVE_LIMIT: f64 = 0.2;
/// *Keep it in one piece* joins the shape cut at this density: the one the application
/// shows by default.
const JOIN_THRESHOLD: f64 = 0.5;
/// What a path through material already there costs, per volume, against 1 through void:
/// of the ways through the shape, the shortest.
const JOIN_LENGTH_COST: f64 = 1e-3;
/// The optimization has settled when no density moves by more than this…
const SETTLED_CHANGE: f64 = 0.01;
/// …or, after this many iterations, the compliance has stopped changing (by this fraction
/// over the last five iterations) with the mass on target.
const SETTLED_AFTER: usize = 15;
const SETTLED_COMPLIANCE: f64 = 5e-4;

/// Faces whose elements stay solid, and how far in.
#[derive(Debug, Clone, PartialEq)]
pub struct Preserve {
    /// The face labels.
    pub labels: Vec<u32>,
    /// mm: the elements whose centre is this close to the faces stay solid too; with 0,
    /// those touching the faces.
    pub depth: f64,
}

/// What a topology study holds: the fixtures, loads and contacts of a static study, the
/// share of the mass to keep and how the shape may look.
#[derive(Debug, Clone, PartialEq)]
pub struct TopologyStudy {
    /// Fixtures, loads and contacts. A weight would change with the shape, so `gravity`
    /// must be `None`.
    pub study: Study,
    /// The share of the model's mass to keep, between 0 and 1 (exclusive).
    pub target: f64,
    /// mm: the thinnest member wanted, at least three elements across (the filter's
    /// radius is half of it, and one and a half elements at least); `None` for three.
    pub member_size: Option<f64>,
    pub preserved: Vec<Preserve>,
    /// Iterations at most; each is one static solve.
    pub max_iterations: usize,
    /// Planes the shape is mirrored about; `None`, or no plane, for none.
    pub symmetry: Option<Symmetry>,
    /// The direction a mould is pulled off along; `None` for none.
    pub pull: Option<Pull>,
    /// *Keep it in one piece*: once optimized, the pieces of the shape cut at half density
    /// that hold a fixture, a load or a kept region are joined to the one the loads are on,
    /// and loose bits holding none of them taken away.
    pub one_piece: bool,
}

/// Planes the shape is mirrored about, through `point` and parallel to the standard planes:
/// SolidWorks' symmetry control, Fusion 360's symmetry planes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Symmetry {
    /// mm: the point the planes pass through; the origin for the standard planes.
    pub point: V3,
    /// Mirrored across the plane normal to X (the Right plane's), to Y (the Top plane's) and
    /// to Z (the Front plane's).
    pub planes: [bool; 3],
}

/// How a mould comes off the shape: no undercut along a direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pull {
    /// The direction the mould is pulled off along: going that way the shape never widens,
    /// as if it stood on its far side, the one the direction comes from. Only its direction
    /// counts.
    pub direction: V3,
    /// A two-part mould: pulled off both ways from the plane through this point across the
    /// direction, its parting plane; `None` for one way.
    pub parting: Option<V3>,
}

/// One iteration's design.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TopologyStep {
    /// N·mm: the compliance of the design solved.
    pub compliance: f64,
    /// Its mass as a share of the model's (filtered densities).
    pub mass_fraction: f64,
    /// The largest change of a design density the update made.
    pub change: f64,
}

/// The optimized densities and how they were reached.
#[derive(Debug, Clone, PartialEq)]
pub struct TopologyResults {
    /// The density of each element, 0 (void) to 1 (solid): the filtered densities of the
    /// last design solved.
    pub density: Vec<f64>,
    /// N·mm: the compliance of the whole model, before material was taken away.
    pub initial_compliance: f64,
    /// Each iteration, in order; the last is the design of `density`.
    pub history: Vec<TopologyStep>,
    /// The densities settled before the iteration limit.
    pub converged: bool,
    /// Conjugate gradient iterations, over every solve.
    pub solver_iterations: usize,
    /// mm: the filter's radius.
    pub filter_radius: f64,
    /// What *Keep it in one piece* did, when it was asked for.
    pub joining: Option<Joining>,
}

/// What *Keep it in one piece* did to the shape.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Joining {
    /// Pieces joined to the rest, and the mass added to join them, a share of the model's.
    pub joined: usize,
    pub added_mass: f64,
    /// Loose bits taken away (pieces holding no fixture, load or kept region), and their
    /// mass, a share of the model's.
    pub removed: usize,
    pub removed_mass: f64,
}

/// A connected piece of the shape cut at a threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    /// The labels of the model's faces it reaches, in order, without [`crate::NO_LABEL`].
    pub labels: Vec<u32>,
    /// mm³, about: the volume of the elements it is in, each by its share of corners in it.
    pub volume: f64,
}

impl TopologyResults {
    /// N·mm, of the last design.
    pub fn compliance(&self) -> f64 {
        self.history
            .last()
            .map_or(self.initial_compliance, |s| s.compliance)
    }

    /// The last design's mass as a share of the model's.
    pub fn mass_fraction(&self) -> f64 {
        self.history.last().map_or(1.0, |s| s.mass_fraction)
    }
}

/// Why a topology study could not be optimized.
#[derive(Debug, Clone, PartialEq)]
pub enum TopologyError {
    /// The static solve failed (or was cancelled).
    Solve(SolveError),
    /// The target is not a share between 0 and 1.
    BadTarget,
    /// The study has gravity, which the optimization does not take.
    Gravity,
    /// The kept regions alone hold this share of the mass, the target or more; `controls`
    /// when symmetry or a pull direction made them larger.
    KeptTooMuch { share: f64, controls: bool },
    /// Less than half of the model has a mirror image across a symmetry plane: the one
    /// normal to `axis` (0 for X, 1 Y, 2 Z) at `at` mm.
    NotSymmetric { axis: usize, at: f64 },
    /// The pull direction is zero.
    NoPullDirection,
    /// The pull direction crosses a symmetry plane, which a mould pulled one way cannot
    /// mirror (nor one pulled both ways from another parting plane).
    PullAcrossSymmetry,
}

impl std::fmt::Display for TopologyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TopologyError::Solve(e) => e.fmt(f),
            TopologyError::BadTarget => {
                write!(
                    f,
                    "the mass to keep is not between 0 and 100 % of the model's"
                )
            }
            TopologyError::Gravity => {
                write!(
                    f,
                    "a topology study takes no gravity: its weight would change with the shape"
                )
            }
            TopologyError::KeptTooMuch { share, controls } => write!(
                f,
                "the kept regions alone are {:.0} % of the mass{}, the target or more: keep more mass, or keep less",
                share * 100.0,
                if *controls {
                    " with their mirror images and the material behind them along the pull direction"
                } else {
                    ""
                }
            ),
            TopologyError::NotSymmetric { axis, at } => write!(
                f,
                "the model is not symmetric about the plane {} = {at} mm: less than half of it has a mirror image across it. Move the plane, or take it off",
                ["X", "Y", "Z"][(*axis).min(2)]
            ),
            TopologyError::NoPullDirection => write!(f, "the pull direction is zero"),
            TopologyError::PullAcrossSymmetry => write!(
                f,
                "the pull direction crosses a symmetry plane, and a mould pulled one way cannot give a mirrored shape: pull both ways from that plane, or take it off"
            ),
        }
    }
}

impl From<SolveError> for TopologyError {
    fn from(e: SolveError) -> Self {
        TopologyError::Solve(e)
    }
}

/// Optimizes the study on the mesh, `materials[b]` being body `b`'s (their densities, when
/// every body has one, weigh the mass; else the volume does). `progress` hears the fraction
/// done, from 0 to 1, as the iteration limit counts it.
pub fn optimize(
    mesh: &TetMesh,
    materials: &[Elastic],
    study: &TopologyStudy,
    options: &SolveOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<TopologyResults, TopologyError> {
    if !(study.target > 0.0 && study.target < 1.0) {
        return Err(TopologyError::BadTarget);
    }
    if study.study.gravity.is_some() {
        return Err(TopologyError::Gravity);
    }
    let pull = match study.pull {
        Some(p) => Some((
            normalize(p.direction).ok_or(TopologyError::NoPullDirection)?,
            p.parting,
        )),
        None => None,
    };
    let symmetry = study.symmetry.filter(|s| s.planes.contains(&true));
    let crossed = symmetry.as_ref().zip(pull);
    if crossed.is_some_and(|(s, (direction, parting))| !pull_keeps_symmetry(s, direction, parting))
    {
        return Err(TopologyError::PullAcrossSymmetry);
    }
    let full_options = SolveOptions {
        tolerance: options.tolerance.max(1e-7),
        ..*options
    };
    let step_options = SolveOptions {
        tolerance: options.tolerance.max(ITERATION_TOLERANCE),
        ..*options
    };
    let problem = Problem {
        mesh,
        materials,
        study: &study.study,
        options: &full_options,
        strain: None,
    };
    let step = Problem {
        options: &step_options,
        ..problem
    };
    let prepared = Prepared::new(&problem, Preconditioner::Multigrid)?;
    let elements = mesh.tets.len();
    let threads = thread_count(options.threads);
    let volume: Vec<f64> = prepared.geometries.iter().map(|g| g.volume.abs()).collect();
    let centre: Vec<V3> = mesh
        .tets
        .iter()
        .map(|t| {
            let p = t.map(|i| mesh.nodes[i as usize]);
            scale(add(add(p[0], p[1]), add(p[2], p[3])), 0.25)
        })
        .collect();
    // A regular tetrahedron of edge `a` has the volume `a³ / 6√2`.
    let size = volume
        .iter()
        .map(|v| (6.0 * std::f64::consts::SQRT_2 * v).cbrt())
        .sum::<f64>()
        / elements.max(1) as f64;
    let radius = study
        .member_size
        .filter(|d| d.is_finite() && *d > 0.0)
        .map_or(1.5 * size, |d| 0.5 * d)
        .max(1.5 * size);
    let weighed = mesh.bodies.iter().all(|&b| {
        materials
            .get(b as usize)
            .is_some_and(|m| m.density.is_finite() && m.density > 0.0)
    });
    let mass: Vec<f64> = (0..elements)
        .map(|e| {
            let density = if weighed {
                materials[mesh.bodies[e] as usize].density
            } else {
                1.0
            };
            volume[e] * density
        })
        .collect();
    let total: f64 = mass.iter().sum();
    let filter = Filter::new(&centre, radius, threads, symmetry.as_ref());
    let mut kept = kept_elements(mesh, study, size);
    // The controls: the kept regions mirrored like the shape; the elements in order along
    // the pull, the kept regions keeping what is behind them.
    if let Some(s) = &symmetry {
        mirror_kept(&filter, &volume, s, size, &mut kept)?;
    }
    let chain = pull.map(|(direction, parting)| Chain::new(&filter, direction, parting, size));
    if let Some(chain) = &chain {
        chain.shadow(&mut kept);
    }

    // The filter's normalisation and the slopes of the mass, worked out once: the
    // filtered mass is linear in the design densities.
    let norms = filter.gather(&volume);
    let design_mass: Vec<f64> = (0..elements)
        .map(|e| if kept[e] { 0.0 } else { mass[e] / norms[e] })
        .collect();
    let gathered = filter.gather(&design_mass);
    let slope: Vec<f64> = (0..elements).map(|j| volume[j] * gathered[j]).collect();
    // The mass that does not depend on the design: the kept elements' own, and what the
    // kept elements (solid) lend their neighbours through the filter.
    let fixed: f64 = (0..elements)
        .filter(|&j| kept[j])
        .map(|j| slope[j] + mass[j])
        .sum();
    let target = study.target * total;
    let free_slope: f64 = (0..elements).filter(|&j| !kept[j]).map(|j| slope[j]).sum();
    if fixed >= target || free_slope <= 0.0 {
        return Err(TopologyError::KeptTooMuch {
            share: (fixed / total.max(f64::MIN_POSITIVE)).min(1.0),
            controls: symmetry.is_some() || chain.is_some(),
        });
    }
    let budget = target - fixed;
    let start = (budget / free_slope).clamp(0.0, 1.0);
    let mut design: Vec<f64> = kept.iter().map(|&k| if k { 1.0 } else { start }).collect();
    let physical = |design: &[f64]| -> Vec<f64> {
        let weighted: Vec<f64> = design.iter().zip(&volume).map(|(d, v)| d * v).collect();
        filter
            .gather(&weighted)
            .iter()
            .zip(&norms)
            .zip(&kept)
            .map(|((s, n), &k)| if k { 1.0 } else { (s / n).clamp(0.0, 1.0) })
            .collect()
    };

    // The whole model first: the compliance to compare with, and a check of the study
    // before the long part.
    let full = prepared.solve(&problem, None, None, cancel, &|f| progress(0.05 * f))?;
    let energies = element_energies(mesh, materials, &prepared.geometries, &full.displacement);
    let initial_compliance: f64 = energies.iter().sum();
    let mut solver_iterations = full.iterations;
    let mut last = full.displacement;

    let iterations = study.max_iterations.max(1);
    let mut history: Vec<TopologyStep> = Vec::new();
    let mut density = vec![1.0; elements];
    let mut converged = false;
    for iteration in 0..iterations {
        if cancel.load(Ordering::Relaxed) {
            return Err(SolveError::Cancelled.into());
        }
        let mut rho = physical(&design);
        // Along a pull direction no density above the one behind it, each taken from the
        // element it now comes from.
        let source = chain.as_ref().map(|c| c.cap(&mut rho));
        let stiffness: Vec<f64> = rho
            .iter()
            .map(|r| VOID_STIFFNESS + r.powf(PENALTY) * (1.0 - VOID_STIFFNESS))
            .collect();
        let at = |f: f64| {
            progress(0.05 + 0.95 * (iteration as f64 + f) / iterations as f64);
        };
        let solved = prepared.solve(&step, Some(&stiffness), Some(&last), cancel, &at)?;
        solver_iterations += solved.iterations;
        let energies =
            element_energies(mesh, materials, &prepared.geometries, &solved.displacement);
        let compliance: f64 = energies.iter().zip(&stiffness).map(|(e, k)| e * k).sum();
        let solved_mass: f64 = rho.iter().zip(&mass).map(|(r, m)| r * m).sum();
        let mass_fraction = solved_mass / total;
        // The compliance's slopes for the densities solved, carried to the filtered
        // densities they were taken from, then to the design ones.
        let mut slopes: Vec<f64> = (0..elements)
            .map(|e| {
                if kept[e] {
                    0.0
                } else {
                    let d = PENALTY * rho[e].powf(PENALTY - 1.0) * (1.0 - VOID_STIFFNESS);
                    -d * energies[e]
                }
            })
            .collect();
        if let Some(source) = &source {
            slopes = carry(&slopes, source);
        }
        let per_norm: Vec<f64> = (0..elements)
            .map(|e| if kept[e] { 0.0 } else { slopes[e] / norms[e] })
            .collect();
        let gathered = filter.gather(&per_norm);
        let sensitivity: Vec<f64> = (0..elements).map(|j| volume[j] * gathered[j]).collect();
        // With a pull direction the mass is linear in the design densities only while each
        // element takes its density from the same one: its slopes, and the budget of the
        // design densities, are worked out again for the elements they come from now.
        let (slope_now, budget_now) = match &source {
            None => (Cow::Borrowed(slope.as_slice()), budget),
            Some(source) => {
                let own: Vec<f64> = (0..elements)
                    .map(|e| if kept[e] { 0.0 } else { mass[e] })
                    .collect();
                let taken = carry(&own, source);
                let per_norm: Vec<f64> = (0..elements)
                    .map(|k| if kept[k] { 0.0 } else { taken[k] / norms[k] })
                    .collect();
                let gathered = filter.gather(&per_norm);
                let now: Vec<f64> = (0..elements).map(|j| volume[j] * gathered[j]).collect();
                let free_mass: f64 = (0..elements)
                    .filter(|&j| !kept[j])
                    .map(|j| now[j] * design[j])
                    .sum();
                (Cow::Owned(now), target - (solved_mass - free_mass))
            }
        };
        let ratio = ratios(&sensitivity, &slope_now, &kept);
        let next = optimality_update(&design, &ratio, &slope_now, budget_now);
        let change = design
            .iter()
            .zip(&next)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        history.push(TopologyStep {
            compliance,
            mass_fraction,
            change,
        });
        design = next;
        density = rho;
        last = solved.displacement;
        let steady = history.len() > SETTLED_AFTER && {
            let recent = &history[history.len() - 6..];
            let (lo, hi) = recent.iter().fold((f64::INFINITY, 0.0f64), |(lo, hi), s| {
                (lo.min(s.compliance), hi.max(s.compliance))
            });
            hi - lo <= SETTLED_COMPLIANCE * hi && (mass_fraction - study.target).abs() < 5e-3
        };
        if change < SETTLED_CHANGE || steady {
            converged = true;
            break;
        }
    }
    let joining = study.one_piece.then(|| {
        let mut anchors: Vec<u32> = study
            .study
            .fixtures
            .iter()
            .flat_map(|f| f.labels.iter())
            .chain(study.preserved.iter().flat_map(|p| p.labels.iter()))
            .chain(study.study.loads.iter().flat_map(|l| l.labels.iter()))
            .copied()
            .collect();
        anchors.sort_unstable();
        anchors.dedup();
        let mut loads: Vec<u32> = study
            .study
            .loads
            .iter()
            .flat_map(|l| l.labels.iter())
            .copied()
            .collect();
        loads.sort_unstable();
        loads.dedup();
        let joint = Joint {
            mesh,
            filter: &filter,
            volume: &volume,
            mass: &mass,
            size,
            symmetry: symmetry.as_ref(),
            chain: chain.as_ref(),
        };
        joint.join(&anchors, &loads, &mut density)
    });
    progress(1.0);
    Ok(TopologyResults {
        density,
        initial_compliance,
        history,
        converged,
        solver_iterations,
        filter_radius: radius,
        joining,
    })
}

/// What joining the pieces of a shape works with.
struct Joint<'a> {
    mesh: &'a TetMesh,
    filter: &'a Filter<'a>,
    volume: &'a [f64],
    mass: &'a [f64],
    /// mm: the elements' typical edge.
    size: f64,
    symmetry: Option<&'a Symmetry>,
    chain: Option<&'a Chain>,
}

impl Joint<'_> {
    /// Joins the pieces of `density`'s shape (cut at [`JOIN_THRESHOLD`]) that reach one of
    /// the `anchors` labels to the largest one reaching a `loads` label, one at a time, the
    /// largest first, by the cheapest path through the elements (material missing, then
    /// length) grown to the filter's radius, mirrored and shadowed along the pull like the
    /// shape; and takes away the pieces reaching no anchor. Both sorted.
    fn join(&self, anchors: &[u32], loads: &[u32], density: &mut [f64]) -> Joining {
        let mesh = self.mesh;
        let elements = mesh.tets.len();
        let total: f64 = self.mass.iter().sum::<f64>().max(f64::MIN_POSITIVE);
        let neighbours = face_neighbours(mesh);
        let mut joining = Joining::default();
        // Each pass takes the loose bits away or joins one piece; a bridge that joined
        // nothing ends it rather than being laid again.
        for _ in 0..elements.min(1000) {
            let rho = corner_densities(mesh, density);
            let (corner, count) = piece_of_corners(mesh, &rho, JOIN_THRESHOLD);
            if count <= 1 {
                break;
            }
            let element = piece_of_elements(mesh, &corner);
            let described = describe(mesh, &corner, count);
            let reaches = |p: usize, labels: &[u32]| {
                described[p]
                    .labels
                    .iter()
                    .any(|l| labels.binary_search(l).is_ok())
            };
            let loose: Vec<bool> = (0..count).map(|p| !reaches(p, anchors)).collect();
            if loose.contains(&true) && loose.contains(&false) {
                for e in 0..elements {
                    let p = element[e];
                    if p != NONE && loose[p as usize] {
                        joining.removed_mass += self.mass[e] * density[e] / total;
                        density[e] = 0.0;
                    }
                }
                joining.removed += loose.iter().filter(|l| **l).count();
                continue;
            }
            let larger = |a: &usize, b: &usize| {
                described[*a]
                    .volume
                    .total_cmp(&described[*b].volume)
                    .then(b.cmp(a))
            };
            let Some(main) = (0..count)
                .filter(|&p| reaches(p, loads))
                .max_by(larger)
                .or_else(|| (0..count).max_by(larger))
            else {
                break;
            };
            let Some(other) = (0..count).filter(|&p| p != main).max_by(larger) else {
                break;
            };
            let cost = |e: usize| self.volume[e] * ((1.0 - density[e]).max(0.0) + JOIN_LENGTH_COST);
            let Some(path) = cheapest_path(
                &neighbours,
                cost,
                |e| element[e] == other as u32,
                |e| element[e] == main as u32,
            ) else {
                break;
            };
            // The path grown into a bar as thick as the thinnest member.
            let filter = self.filter;
            let mut bridge = vec![false; elements];
            for &e in &path {
                let c = filter.centres[e as usize];
                filter.around(c, |j| {
                    if norm(sub(c, filter.centres[j as usize])) <= filter.radius {
                        bridge[j as usize] = true;
                    }
                });
            }
            if let Some(s) = self.symmetry {
                // The planes were checked when the study was prepared.
                let _ = mirror_kept(filter, self.volume, s, self.size, &mut bridge);
            }
            if let Some(chain) = self.chain {
                chain.shadow(&mut bridge);
            }
            let mut added = 0.0;
            for (e, _) in bridge.iter().enumerate().filter(|(_, b)| **b) {
                added += self.mass[e] * (1.0 - density[e]).max(0.0);
                density[e] = 1.0;
            }
            if added <= 0.0 {
                break;
            }
            joining.joined += 1;
            joining.added_mass += added / total;
        }
        joining
    }
}

/// Each element's neighbours across its four faces, in [`FACES`] order, [`NONE`] on the
/// boundary.
fn face_neighbours(mesh: &TetMesh) -> Vec<[u32; 4]> {
    let mut out = vec![[NONE; 4]; mesh.tets.len()];
    let mut open: HashMap<[u32; 3], (u32, usize)> = HashMap::new();
    for (e, t) in mesh.tets.iter().enumerate() {
        for (k, face) in FACES.iter().enumerate() {
            let mut key = face.map(|i| t[i]);
            key.sort_unstable();
            match open.remove(&key) {
                Some((other, side)) => {
                    out[e][k] = other;
                    out[other as usize][side] = e as u32;
                }
                None => {
                    open.insert(key, (e as u32, k));
                }
            }
        }
    }
    out
}

/// The cheapest chain of face neighbours from an element where `from` holds to one where
/// `to` does, entering element `e` costing `cost(e)` (not negative): Dijkstra's, ties going
/// to the lower index. The elements after the first, in order.
fn cheapest_path(
    neighbours: &[[u32; 4]],
    cost: impl Fn(usize) -> f64,
    from: impl Fn(usize) -> bool,
    to: impl Fn(usize) -> bool,
) -> Option<Vec<u32>> {
    let n = neighbours.len();
    let mut best = vec![f64::INFINITY; n];
    let mut previous = vec![NONE; n];
    // Costs are not negative, so their bits order as they do.
    let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
    for e in (0..n).filter(|&e| from(e)) {
        best[e] = 0.0;
        heap.push(Reverse((0, e as u32)));
    }
    while let Some(Reverse((bits, e))) = heap.pop() {
        let (d, e) = (f64::from_bits(bits), e as usize);
        if d > best[e] {
            continue;
        }
        if to(e) {
            let mut path = vec![e as u32];
            let mut at = e;
            while previous[at] != NONE {
                at = previous[at] as usize;
                path.push(at as u32);
            }
            path.reverse();
            return Some(path);
        }
        for &j in neighbours[e].iter().filter(|&&j| j != NONE) {
            let next = d + cost(j as usize);
            if next < best[j as usize] {
                best[j as usize] = next;
                previous[j as usize] = e as u32;
                heap.push(Reverse((next.to_bits(), j)));
            }
        }
    }
    None
}

/// The pieces of the shape cut at `threshold`, as [`shape`] cuts it, the largest first. The
/// material above the threshold in an element is convex (the density varies linearly in
/// it), so corners above it in one element are in one piece: the pieces are those corners,
/// joined through the elements they share.
pub fn pieces(mesh: &TetMesh, density: &[f64], threshold: f64) -> Vec<Piece> {
    if density.len() != mesh.tets.len() || mesh.validate().is_err() {
        return Vec::new();
    }
    let rho = corner_densities(mesh, density);
    let (corner, count) = piece_of_corners(mesh, &rho, threshold);
    let mut out = describe(mesh, &corner, count);
    out.sort_by(|a, b| b.volume.total_cmp(&a.volume));
    out
}

/// The elements' densities averaged at their corners, by volume.
fn corner_densities(mesh: &TetMesh, density: &[f64]) -> Vec<f64> {
    let n = mesh.nodes.len();
    let mut sum = vec![0.0; n];
    let mut weight = vec![0.0; n];
    for (e, t) in mesh.tets.iter().enumerate() {
        let v = mesh.tet_volume(e).unwrap_or(0.0).abs();
        for &i in t {
            sum[i as usize] += v * density[e];
            weight[i as usize] += v;
        }
    }
    sum.iter()
        .zip(&weight)
        .map(|(s, w)| if *w > 0.0 { s / w } else { 0.0 })
        .collect()
}

/// Each corner's piece ([`NONE`] below the threshold), numbered in the order of their
/// first corner, and how many there are.
fn piece_of_corners(mesh: &TetMesh, rho: &[f64], threshold: f64) -> (Vec<u32>, usize) {
    let n = rho.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(parent: &mut [u32], mut i: u32) -> u32 {
        while parent[i as usize] != i {
            let up = parent[parent[i as usize] as usize];
            parent[i as usize] = up;
            i = up;
        }
        i
    }
    for t in &mesh.tets {
        let mut first = NONE;
        for &i in t.iter().filter(|&&i| rho[i as usize] >= threshold) {
            if first == NONE {
                first = i;
                continue;
            }
            let (a, b) = (find(&mut parent, first), find(&mut parent, i));
            if a != b {
                parent[a.max(b) as usize] = a.min(b);
            }
        }
    }
    let mut number = vec![NONE; n];
    let mut out = vec![NONE; n];
    let mut count = 0;
    for i in 0..n {
        if rho[i] < threshold {
            continue;
        }
        let root = find(&mut parent, i as u32) as usize;
        if number[root] == NONE {
            number[root] = count;
            count += 1;
        }
        out[i] = number[root];
    }
    (out, count as usize)
}

/// Each element's piece: the one its corners above the threshold are in, [`NONE`] when
/// none is.
fn piece_of_elements(mesh: &TetMesh, corner: &[u32]) -> Vec<u32> {
    mesh.tets
        .iter()
        .map(|t| {
            t.iter()
                .map(|&i| corner[i as usize])
                .find(|&p| p != NONE)
                .unwrap_or(NONE)
        })
        .collect()
}

/// The pieces' labels and volumes, by their numbers.
fn describe(mesh: &TetMesh, corner: &[u32], count: usize) -> Vec<Piece> {
    let mut out = vec![
        Piece {
            labels: Vec::new(),
            volume: 0.0,
        };
        count
    ];
    for (e, t) in mesh.tets.iter().enumerate() {
        let inside: Vec<u32> = t
            .iter()
            .map(|&i| corner[i as usize])
            .filter(|&p| p != NONE)
            .collect();
        if let Some(&p) = inside.first() {
            let v = mesh.tet_volume(e).unwrap_or(0.0).abs();
            out[p as usize].volume += v * inside.len() as f64 / 4.0;
        }
    }
    for f in mesh.facets.iter().filter(|f| f.label != crate::NO_LABEL) {
        if let Some(p) = mesh
            .facet_corners(f)
            .iter()
            .map(|&i| corner[i as usize])
            .find(|&p| p != NONE)
        {
            out[p as usize].labels.push(f.label);
        }
    }
    for piece in &mut out {
        piece.labels.sort_unstable();
        piece.labels.dedup();
    }
    out
}

/// Each design density's ratio of its compliance slope to its mass slope (for growing it),
/// `None` for those that stay as they are: the kept ones and those with no mass slope.
fn ratios(sensitivity: &[f64], slope: &[f64], kept: &[bool]) -> Vec<Option<f64>> {
    (0..sensitivity.len())
        .map(|j| (!kept[j] && slope[j] > 0.0).then(|| (-sensitivity[j]).max(0.0) / slope[j]))
        .collect()
}

/// The next design by optimality criteria: each density with a `ratio` times the square
/// root of it over `λ`, within the move limit and 0 to 1, `λ` found by bisection (on its
/// logarithm, the slopes' scale being the model's) so that their mass, by `slope`, is
/// `budget`.
fn optimality_update(
    design: &[f64],
    ratio: &[Option<f64>],
    slope: &[f64],
    budget: f64,
) -> Vec<f64> {
    let largest = ratio.iter().flatten().copied().fold(0.0, f64::max);
    if !(largest > 0.0 && largest.is_finite()) {
        return design.to_vec();
    }
    let at = |lambda: f64| -> Vec<f64> {
        design
            .iter()
            .zip(ratio)
            .map(|(&d, r)| match r {
                None => d,
                Some(r) => {
                    let lo = (d - MOVE_LIMIT).max(0.0);
                    let hi = (d + MOVE_LIMIT).min(1.0);
                    (d * (r / lambda).sqrt()).clamp(lo, hi)
                }
            })
            .collect()
    };
    let mass_of = |next: &[f64]| -> f64 {
        (0..next.len())
            .filter(|&j| ratio[j].is_some())
            .map(|j| slope[j] * next[j])
            .sum()
    };
    // The mass falls as λ grows: from every density up by the move limit to every one down.
    let (mut lo, mut hi) = ((largest * 1e-12).ln(), (largest * 1e12).ln());
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        if mass_of(&at(mid.exp())) > budget {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at((0.5 * (lo + hi)).exp())
}

/// Whether a pull along `direction` (of unit length), one way or both ways from a plane
/// through `parting`, gives shapes mirrored across the symmetry planes: it runs along each
/// plane, or straight across one that is its parting plane.
fn pull_keeps_symmetry(symmetry: &Symmetry, direction: V3, parting: Option<V3>) -> bool {
    (0..3).filter(|&a| symmetry.planes[a]).all(|a| {
        let across = direction[a].abs();
        across < 1e-9
            || (across > 1.0 - 1e-9
                && parting.is_some_and(|p| (p[a] - symmetry.point[a]).abs() < 1e-6))
    })
}

/// Each element's value added to its `source`'s: slopes carried back to the elements the
/// densities were taken from.
fn carry(values: &[f64], source: &[u32]) -> Vec<f64> {
    let mut out = vec![0.0; values.len()];
    for (v, &s) in values.iter().zip(source) {
        out[s as usize] += v;
    }
    out
}

/// No element.
const NONE: u32 = u32::MAX;

/// The kept regions mirrored: an element is kept when the one whose centre is nearest its
/// mirror image across a plane (within three quarters of an element) is, plane after plane.
/// A plane across which less than half of the model has a mirror image is refused: the
/// model is not symmetric about it (most often, not modelled about the origin).
fn mirror_kept(
    filter: &Filter,
    volume: &[f64],
    symmetry: &Symmetry,
    size: f64,
    kept: &mut [bool],
) -> Result<(), TopologyError> {
    let total: f64 = volume.iter().sum();
    for axis in (0..3).filter(|&a| symmetry.planes[a]) {
        let at = symmetry.point[axis];
        let partner = filter.each(|e| {
            let mut image = filter.centres[e];
            image[axis] = 2.0 * at - image[axis];
            filter.best(image, 0.75 * size, |j| {
                Some(norm(sub(image, filter.centres[j])))
            })
        });
        let mirrored: f64 = partner
            .iter()
            .zip(volume)
            .filter(|(p, _)| p.is_some())
            .map(|(_, v)| v)
            .sum();
        if mirrored < 0.5 * total {
            return Err(TopologyError::NotSymmetric { axis, at });
        }
        let before = kept.to_vec();
        for (k, p) in kept.iter_mut().zip(&partner) {
            if let Some(p) = *p {
                *k |= before[p as usize];
            }
        }
    }
    Ok(())
}

/// A pull direction's order: each element's neighbour behind it, against the direction (or
/// toward the parting plane, on the same side of it), and the elements from the back to the
/// front.
struct Chain {
    /// The element behind each, or [`NONE`] at the back.
    behind: Vec<u32>,
    order: Vec<u32>,
}

impl Chain {
    /// `direction` of unit length. The element behind another is the one whose centre is
    /// closest to the line through its centre along the direction (the nearer one on a
    /// tie), among those about an element behind it: on a structured mesh, the same
    /// element of the cell behind.
    fn new(filter: &Filter, direction: V3, parting: Option<V3>, size: f64) -> Chain {
        let centres = filter.centres;
        let offset = |c: V3| parting.map_or(dot(c, direction), |p| dot(sub(c, p), direction));
        // How far each centre is from the back: along the direction, or from the parting
        // plane, on either side of it.
        let along: Vec<f64> = centres
            .iter()
            .map(|&c| {
                let t = offset(c);
                if parting.is_some() { t.abs() } else { t }
            })
            .collect();
        let positive = |c: V3| parting.is_none() || offset(c) >= 0.0;
        let behind = filter.each(|e| {
            let c = centres[e];
            let side = positive(c);
            let forward = if side {
                direction
            } else {
                scale(direction, -1.0)
            };
            filter
                .best(sub(c, scale(forward, size)), 0.75 * size, |j| {
                    let gap = along[e] - along[j];
                    if gap < 0.3 * size || positive(centres[j]) != side {
                        return None;
                    }
                    let d = sub(c, centres[j]);
                    let lateral = norm(sub(d, scale(forward, dot(d, forward))));
                    Some(lateral + 0.1 * gap)
                })
                .unwrap_or(NONE)
        });
        let mut order: Vec<u32> = (0..centres.len() as u32).collect();
        order.sort_by(|&a, &b| {
            along[a as usize]
                .total_cmp(&along[b as usize])
                .then(a.cmp(&b))
        });
        Chain { behind, order }
    }

    /// Caps each density at the one behind it, from the back to the front, and returns the
    /// element each density now comes from.
    fn cap(&self, rho: &mut [f64]) -> Vec<u32> {
        let mut source: Vec<u32> = (0..rho.len() as u32).collect();
        for &e in &self.order {
            let (e, b) = (e as usize, self.behind[e as usize]);
            if b != NONE && rho[b as usize] < rho[e] {
                rho[e] = rho[b as usize];
                source[e] = source[b as usize];
            }
        }
        source
    }

    /// The elements behind kept ones are kept too: a mould could not reach in between. So
    /// a kept element is never capped.
    fn shadow(&self, kept: &mut [bool]) {
        for &e in self.order.iter().rev() {
            let b = self.behind[e as usize];
            if b != NONE && kept[e as usize] {
                kept[b as usize] = true;
            }
        }
    }
}

/// The elements that stay solid: those with a face on a fixture's or a load's faces, and
/// those within a preserved region's depth of its faces. `size` is the elements' typical
/// edge, for the search grid.
fn kept_elements(mesh: &TetMesh, study: &TopologyStudy, size: f64) -> Vec<bool> {
    let mut kept = vec![false; mesh.tets.len()];
    let touching: Vec<&[u32]> = study
        .study
        .fixtures
        .iter()
        .map(|f| f.labels.as_slice())
        .chain(study.study.loads.iter().map(|l| l.labels.as_slice()))
        .chain(study.preserved.iter().map(|p| p.labels.as_slice()))
        .collect();
    for f in &mesh.facets {
        if touching.iter().any(|labels| labels.contains(&f.label)) {
            kept[f.tet as usize] = true;
        }
    }
    for p in study.preserved.iter().filter(|p| p.depth > 0.0) {
        let depth = p.depth;
        let triangles: Vec<[V3; 3]> = mesh
            .facets
            .iter()
            .filter(|f| p.labels.contains(&f.label))
            .map(|f| mesh.facet_corners(f).map(|i| mesh.nodes[i as usize]))
            .collect();
        if triangles.is_empty() {
            continue;
        }
        // The triangles in the cells of a grid their boxes, grown by the depth, overlap.
        let cell = depth.max(size).max(1e-9);
        let index = |v: f64| (v / cell).floor() as i64;
        let mut cells: BTreeMap<[i64; 3], Vec<u32>> = BTreeMap::new();
        for (k, t) in triangles.iter().enumerate() {
            let lo: [i64; 3] =
                std::array::from_fn(|a| index(t[0][a].min(t[1][a]).min(t[2][a]) - depth));
            let hi: [i64; 3] =
                std::array::from_fn(|a| index(t[0][a].max(t[1][a]).max(t[2][a]) + depth));
            for x in lo[0]..=hi[0] {
                for y in lo[1]..=hi[1] {
                    for z in lo[2]..=hi[2] {
                        cells.entry([x, y, z]).or_default().push(k as u32);
                    }
                }
            }
        }
        for (e, t) in mesh.tets.iter().enumerate() {
            if kept[e] {
                continue;
            }
            let q = t.map(|i| mesh.nodes[i as usize]);
            let c = scale(add(add(q[0], q[1]), add(q[2], q[3])), 0.25);
            let Some(near) = cells.get(&c.map(index)) else {
                continue;
            };
            kept[e] = near.iter().any(|&k| {
                let [a, b, d] = triangles[k as usize];
                point_triangle_distance(c, a, b, d) <= depth
            });
        }
    }
    kept
}

/// The distance from `p` to the triangle `a b c` (Ericson, *Real-Time Collision
/// Detection*, 5.1.5).
fn point_triangle_distance(p: V3, a: V3, b: V3, c: V3) -> f64 {
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return norm(ap);
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return norm(bp);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return norm(sub(p, add(a, scale(ab, v))));
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return norm(cp);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return norm(sub(p, add(a, scale(ac, w))));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return norm(sub(p, add(b, scale(sub(c, b), w))));
    }
    let denominator = va + vb + vc;
    if denominator.abs() < f64::MIN_POSITIVE {
        return norm(ap);
    }
    let (v, w) = (vb / denominator, vc / denominator);
    norm(sub(p, add(a, add(scale(ab, v), scale(ac, w)))))
}

/// The density filter's neighbourhoods: the element centres in the cells of a grid as
/// wide as the radius, so the neighbours of an element are in the 27 cells around its own.
struct Filter<'a> {
    centres: &'a [V3],
    radius: f64,
    /// The symmetry's reflections, each as twice the planes' positions along the axes it
    /// flips: the identity first, then every combination of the planes.
    images: Vec<[Option<f64>; 3]>,
    origin: V3,
    /// Element indices sorted by cell, then by index, and their cells' keys in the same
    /// order.
    order: Vec<u32>,
    keys: Vec<u64>,
    threads: usize,
}

impl<'a> Filter<'a> {
    fn new(
        centres: &'a [V3],
        radius: f64,
        threads: usize,
        symmetry: Option<&Symmetry>,
    ) -> Filter<'a> {
        let origin = centres.iter().fold([f64::INFINITY; 3], |lo, c| {
            [lo[0].min(c[0]), lo[1].min(c[1]), lo[2].min(c[2])]
        });
        let planes: Vec<(usize, f64)> = symmetry.map_or(Vec::new(), |s| {
            (0..3)
                .filter(|&a| s.planes[a])
                .map(|a| (a, 2.0 * s.point[a]))
                .collect()
        });
        let images = (0..1usize << planes.len())
            .map(|flips| {
                let mut image = [None; 3];
                for (k, &(a, twice)) in planes.iter().enumerate() {
                    if flips >> k & 1 == 1 {
                        image[a] = Some(twice);
                    }
                }
                image
            })
            .collect();
        let mut filter = Filter {
            centres,
            radius,
            images,
            origin,
            order: Vec::new(),
            keys: Vec::new(),
            threads: threads.max(1),
        };
        let mut pairs: Vec<(u64, u32)> = centres
            .iter()
            .enumerate()
            .map(|(e, c)| (filter.key(filter.cell(*c)), e as u32))
            .collect();
        pairs.sort_unstable();
        filter.keys = pairs.iter().map(|p| p.0).collect();
        filter.order = pairs.iter().map(|p| p.1).collect();
        filter
    }

    fn cell(&self, c: V3) -> [i64; 3] {
        std::array::from_fn(|a| ((c[a] - self.origin[a]) / self.radius).floor() as i64)
    }

    /// Cells are numbered 21 bits per axis; a model 2 million radii long would wrap,
    /// which only costs neighbours checked for nothing.
    fn key(&self, cell: [i64; 3]) -> u64 {
        let part = |v: i64| (v.max(0) as u64) & 0x1f_ffff;
        part(cell[0]) | (part(cell[1]) << 21) | (part(cell[2]) << 42)
    }

    /// The elements whose centres are in the 27 cells around `p`'s, in the order of the
    /// cells, then of their indices: every one within the radius of `p`, and some farther.
    fn around(&self, p: V3, mut visit: impl FnMut(u32)) {
        let home = self.cell(p);
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let cell = [home[0] + dx, home[1] + dy, home[2] + dz];
                    if cell.iter().any(|&v| v < 0) {
                        continue;
                    }
                    let key = self.key(cell);
                    let from = self.keys.partition_point(|&k| k < key);
                    let to = self.keys.partition_point(|&k| k <= key);
                    for &j in &self.order[from..to] {
                        visit(j);
                    }
                }
            }
        }
    }

    /// For each element `i`, `Σⱼ wᵢⱼ xⱼ` on several threads, with
    /// `wᵢⱼ = Σ_g max(0, r − |g cᵢ − cⱼ|)` over the reflections `g` of the symmetry (the
    /// identity alone without one). A reflection moves both centres alike and undoes
    /// itself, so the weights are symmetric and the same sum carries slopes back through
    /// the filter; and a filtered density, this sum over the same one of the volumes, is the
    /// same at mirror images, on any mesh.
    fn gather(&self, x: &[f64]) -> Vec<f64> {
        self.each(|i| {
            let mut sum = 0.0;
            for image in &self.images {
                let c: V3 = std::array::from_fn(|a| {
                    image[a].map_or(self.centres[i][a], |twice| twice - self.centres[i][a])
                });
                self.around(c, |j| {
                    let d = norm(sub(c, self.centres[j as usize]));
                    if d < self.radius {
                        sum += (self.radius - d) * x[j as usize];
                    }
                });
            }
            sum
        })
    }

    /// Of the elements whose centres are within `within` of `p` (at most the radius), the
    /// one of least `score` (`None` to pass over one); on a tie the one of lowest index.
    fn best(&self, p: V3, within: f64, score: impl Fn(usize) -> Option<f64>) -> Option<u32> {
        let mut best: Option<(f64, u32)> = None;
        self.around(p, |j| {
            if norm(sub(p, self.centres[j as usize])) > within {
                return;
            }
            let better = |s: &f64| best.is_none_or(|(b, k)| *s < b || (*s == b && j < k));
            if let Some(s) = score(j as usize).filter(better) {
                best = Some((s, j));
            }
        });
        best.map(|(_, j)| j)
    }

    /// `f` of each element, on several threads; the same on any number of them.
    fn each<T: Default + Clone + Send>(&self, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
        let n = self.centres.len();
        let mut out = vec![T::default(); n];
        if self.threads <= 1 || n < 4096 {
            for (i, o) in out.iter_mut().enumerate() {
                *o = f(i);
            }
            return out;
        }
        let chunk = n.div_ceil(self.threads).max(1);
        std::thread::scope(|s| {
            for (k, part) in out.chunks_mut(chunk).enumerate() {
                let f = &f;
                s.spawn(move || {
                    for (i, o) in part.iter_mut().enumerate() {
                        *o = f(k * chunk + i);
                    }
                });
            }
        });
        out
    }
}

// ---- The shape ------------------------------------------------------------------------------

/// A closed triangle surface: the optimized shape.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Shape {
    pub positions: Vec<V3>,
    /// Wound counter-clockwise seen from outside.
    pub triangles: Vec<[u32; 3]>,
    /// The model face each triangle lies on: the label of the boundary facet it was cut
    /// from, or [`crate::NO_LABEL`] for the cut through the material.
    pub labels: Vec<u32>,
}

impl Shape {
    /// mm³: the volume it encloses.
    pub fn volume(&self) -> f64 {
        self.triangles
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| self.positions[i as usize]);
                dot(a, cross(b, c)) / 6.0
            })
            .sum()
    }
}

/// The surface where the density is `threshold` (from 0 to 1): the elements' densities
/// averaged at their corners (by volume), cut by marching tetrahedra, and the model's own
/// boundary kept where the density is above the threshold, so the surface is closed. Then
/// `smoothing` rounds of Taubin's smoothing (which keeps the volume) move the cut's
/// points, never those on the model's faces.
pub fn shape(mesh: &TetMesh, density: &[f64], threshold: f64, smoothing: usize) -> Shape {
    if density.len() != mesh.tets.len() || mesh.validate().is_err() {
        return Shape::default();
    }
    let rho = corner_densities(mesh, density);
    let mut cut = Cutter {
        mesh,
        rho: &rho,
        threshold,
        out: Shape::default(),
        edge_points: HashMap::new(),
        node_points: HashMap::new(),
    };

    for t in &mesh.tets {
        let (ins, outs): (Vec<u32>, Vec<u32>) = t.iter().partition(|&&i| cut.inside(i));
        if ins.is_empty() || outs.is_empty() {
            continue;
        }
        let centre = |list: &[u32]| {
            let s = list
                .iter()
                .fold([0.0; 3], |s, &i| add(s, mesh.nodes[i as usize]));
            scale(s, 1.0 / list.len() as f64)
        };
        let outward = sub(centre(&outs), centre(&ins));
        match ins.len() {
            1 => cut.triangle(
                [(ins[0], outs[0]), (ins[0], outs[1]), (ins[0], outs[2])],
                outward,
            ),
            3 => cut.triangle(
                [(ins[0], outs[0]), (ins[1], outs[0]), (ins[2], outs[0])],
                outward,
            ),
            _ => {
                let (a, b, c, d) = (ins[0], ins[1], outs[0], outs[1]);
                cut.triangle([(a, c), (a, d), (b, d)], outward);
                cut.triangle([(a, c), (b, d), (b, c)], outward);
            }
        }
    }

    // The model's faces where the shape reaches them: each boundary facet cut to its part
    // above the threshold, a polygon fanned into triangles, wound as the facet is (outward).
    let mut fixed_points: Vec<u32> = Vec::new();
    for f in &mesh.facets {
        let corners = mesh.facet_corners(f);
        if !corners.iter().any(|&i| cut.inside(i)) {
            continue;
        }
        let mut polygon: Vec<u32> = Vec::with_capacity(4);
        for k in 0..3 {
            let (a, b) = (corners[k], corners[(k + 1) % 3]);
            if cut.inside(a) {
                polygon.push(cut.node(a));
            }
            if cut.inside(a) != cut.inside(b) {
                polygon.push(cut.crossing(a, b));
            }
        }
        fixed_points.extend(&polygon);
        for k in 1..polygon.len().saturating_sub(1) {
            cut.out
                .triangles
                .push([polygon[0], polygon[k], polygon[k + 1]]);
            cut.out.labels.push(f.label);
        }
    }

    let mut out = cut.out;
    if smoothing > 0 {
        let mut fixed = vec![false; out.positions.len()];
        for &p in &fixed_points {
            fixed[p as usize] = true;
        }
        smooth(&mut out, &fixed, smoothing);
    }
    out
}

/// The surface being cut out of a mesh: its points, one per node kept on the model's
/// boundary and one per edge the surface crosses, shared by every triangle on them so the
/// surface is closed.
struct Cutter<'a> {
    mesh: &'a TetMesh,
    /// The density at each node.
    rho: &'a [f64],
    threshold: f64,
    out: Shape,
    edge_points: HashMap<(u32, u32), u32>,
    node_points: HashMap<u32, u32>,
}

impl Cutter<'_> {
    fn inside(&self, node: u32) -> bool {
        self.rho[node as usize] >= self.threshold
    }

    /// The point where the density along the edge `a b` is the threshold.
    fn crossing(&mut self, a: u32, b: u32) -> u32 {
        let key = (a.min(b), a.max(b));
        if let Some(&p) = self.edge_points.get(&key) {
            return p;
        }
        let (ra, rb) = (self.rho[key.0 as usize], self.rho[key.1 as usize]);
        let t = if (rb - ra).abs() > 1e-12 {
            ((self.threshold - ra) / (rb - ra)).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let p = lerp(
            self.mesh.nodes[key.0 as usize],
            self.mesh.nodes[key.1 as usize],
            t,
        );
        self.out.positions.push(p);
        let id = (self.out.positions.len() - 1) as u32;
        self.edge_points.insert(key, id);
        id
    }

    /// The point at a node.
    fn node(&mut self, a: u32) -> u32 {
        if let Some(&p) = self.node_points.get(&a) {
            return p;
        }
        self.out.positions.push(self.mesh.nodes[a as usize]);
        let id = (self.out.positions.len() - 1) as u32;
        self.node_points.insert(a, id);
        id
    }

    /// A triangle through the crossings of three edges, its normal along `outward`. Which
    /// way it turns is worked out from the edges' middles, not the crossings, which can
    /// fall together (at a node exactly on the threshold) and leave no direction.
    fn triangle(&mut self, edges: [(u32, u32); 3], outward: V3) {
        let ids = edges.map(|(a, b)| self.crossing(a, b));
        let [p, q, r] = edges.map(|(a, b)| {
            lerp(
                self.mesh.nodes[a as usize],
                self.mesh.nodes[b as usize],
                0.5,
            )
        });
        let normal = cross(sub(q, p), sub(r, p));
        self.out.triangles.push(if dot(normal, outward) < 0.0 {
            [ids[0], ids[2], ids[1]]
        } else {
            ids
        });
        self.out.labels.push(crate::NO_LABEL);
    }
}

/// Taubin's λ|μ smoothing: a shrinking Laplacian step, then a growing one, `rounds` times,
/// over the points not `fixed`.
pub(crate) fn smooth(shape: &mut Shape, fixed: &[bool], rounds: usize) {
    let n = shape.positions.len();
    let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); n];
    for t in &shape.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            neighbours[a as usize].push(b);
            neighbours[b as usize].push(a);
        }
    }
    for list in &mut neighbours {
        list.sort_unstable();
        list.dedup();
    }
    let step = |positions: &mut Vec<V3>, factor: f64| {
        let next: Vec<V3> = (0..n)
            .map(|i| {
                let p = positions[i];
                let list = &neighbours[i];
                if fixed[i] || list.is_empty() {
                    return p;
                }
                let mean = scale(
                    list.iter()
                        .fold([0.0; 3], |s, &j| add(s, positions[j as usize])),
                    1.0 / list.len() as f64,
                );
                add(p, scale(sub(mean, p), factor))
            })
            .collect();
        *positions = next;
    };
    for _ in 0..rounds {
        step(&mut shape.positions, 0.5);
        step(&mut shape.positions, -0.53);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solve::{Fixture, FixtureKind, Load, LoadKind};

    const STEEL: Elastic = Elastic::new(200_000.0, 0.3, 250.0, 7850.0);

    /// Every edge of a closed, consistently wound surface is used once each way.
    fn is_closed(shape: &Shape) -> bool {
        let mut directed: HashMap<(u32, u32), u32> = HashMap::new();
        for t in &shape.triangles {
            for k in 0..3 {
                *directed.entry((t[k], t[(k + 1) % 3])).or_insert(0) += 1;
            }
        }
        directed
            .iter()
            .all(|(&(a, b), &count)| count == 1 && directed.get(&(b, a)) == Some(&1))
    }

    /// A 60 × 20 × 10 mm cantilever held at x = 0, a 1 kN load down on a strip of its
    /// free end's face, label 7 (the end's middle third).
    fn cantilever(n: [usize; 3]) -> (TetMesh, TopologyStudy) {
        let mut mesh = TetMesh::structured_box([0.0; 3], [60.0, 20.0, 10.0], n);
        for f in 0..mesh.facets.len() {
            let facet = mesh.facets[f];
            if facet.label != 1 {
                continue;
            }
            let c = mesh
                .facet_corners(&facet)
                .map(|i| mesh.nodes[i as usize])
                .iter()
                .fold([0.0; 3], |s, p| add(s, scale(*p, 1.0 / 3.0)));
            if (c[1] - 10.0).abs() < 20.0 / 6.0 {
                mesh.facets[f].label = 7;
            }
        }
        let study = TopologyStudy {
            study: Study {
                fixtures: vec![Fixture {
                    kind: FixtureKind::Fixed,
                    labels: vec![0],
                }],
                loads: vec![Load {
                    kind: LoadKind::Force {
                        total: 1000.0,
                        direction: Some([0.0, -1.0, 0.0]),
                    },
                    labels: vec![7],
                }],
                gravity: None,
                contacts: Vec::new(),
            },
            target: 0.4,
            member_size: None,
            preserved: Vec::new(),
            max_iterations: 40,
            symmetry: None,
            pull: None,
            one_piece: false,
        };
        (mesh, study)
    }

    fn run(mesh: &TetMesh, study: &TopologyStudy) -> TopologyResults {
        optimize(
            mesh,
            &[STEEL],
            study,
            &SolveOptions::default(),
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap()
    }

    #[test]
    fn a_cantilever_keeps_its_mass_target_and_gets_as_stiff_as_it_can() {
        let (mesh, study) = cantilever([24, 8, 4]);
        let r = run(&mesh, &study);
        // On target, a little stiffness lost for 60 % of the material gone, and far stiffer
        // than the same mass spread evenly (0.4³ of the stiffness: 15.6 times the
        // compliance).
        assert!(
            (r.mass_fraction() - 0.4).abs() < 0.01,
            "{}",
            r.mass_fraction()
        );
        let ratio = r.compliance() / r.initial_compliance;
        assert!(ratio > 1.0 && ratio < 0.25 * 15.6, "{ratio}");
        // The shape cut at half density holds about the mass kept.
        let s = shape(&mesh, &r.density, 0.5, 4);
        assert!(is_closed(&s));
        let kept = s.volume() / mesh.volume();
        assert!((kept - 0.4).abs() < 0.06, "{kept}");
        // The compliance fell from the first design on.
        let first = r.history.first().unwrap().compliance;
        assert!(
            r.compliance() < 0.6 * first,
            "{first} to {}",
            r.compliance()
        );
        // Held and loaded faces stay solid.
        for f in mesh.facets.iter().filter(|f| f.label == 0 || f.label == 7) {
            assert_eq!(r.density[f.tet as usize], 1.0);
        }
    }

    /// Optimizes a finer cantilever (37 000 elements, 60 iterations) and writes its shape to
    /// the OBJ file named by `OXYDE_TOPOLOGY_OBJ`, to look at, with the time it took:
    /// `cargo test -p oxyde-sim --lib -- --ignored --nocapture topology_obj`.
    #[test]
    #[ignore]
    fn topology_obj() {
        let Ok(path) = std::env::var("OXYDE_TOPOLOGY_OBJ") else {
            return;
        };
        let (mesh, mut study) = cantilever([48, 16, 8]);
        study.max_iterations = 60;
        let start = std::time::Instant::now();
        let r = run(&mesh, &study);
        let s = shape(&mesh, &r.density, 0.5, 8);
        eprintln!(
            "{} elements, {} iterations in {:.1} s, {:.2} times as flexible as the whole \
             part, the shape {:.1} % of the volume",
            mesh.tets.len(),
            r.history.len(),
            start.elapsed().as_secs_f64(),
            r.compliance() / r.initial_compliance,
            100.0 * s.volume() / mesh.volume()
        );
        let mut text = String::new();
        for p in &s.positions {
            text.push_str(&format!("v {} {} {}\n", p[0], p[1], p[2]));
        }
        for t in &s.triangles {
            text.push_str(&format!("f {} {} {}\n", t[0] + 1, t[1] + 1, t[2] + 1));
        }
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn the_same_study_gives_the_same_shape_on_one_thread_or_many() {
        let (small, mut study) = cantilever([12, 4, 2]);
        study.max_iterations = 8;
        // With symmetry and a pull direction too, on a mesh large enough for the threads.
        let (large, mut controlled) = cantilever([20, 6, 6]);
        controlled.max_iterations = 3;
        controlled.symmetry = Some(Symmetry {
            point: [0.0, 0.0, 5.0],
            planes: [false, false, true],
        });
        controlled.pull = Some(Pull {
            direction: [0.0, 0.0, 1.0],
            parting: Some([0.0, 0.0, 5.0]),
        });
        for (mesh, study) in [(&small, &study), (&large, &controlled)] {
            let many = run(mesh, study);
            let one = optimize(
                mesh,
                &[STEEL],
                study,
                &SolveOptions {
                    threads: 1,
                    ..SolveOptions::default()
                },
                &AtomicBool::new(false),
                &|_| {},
            )
            .unwrap();
            assert_eq!(many.history.len(), one.history.len());
            for (a, b) in many.density.iter().zip(&one.density) {
                assert!((a - b).abs() < 1e-9, "{a} {b}");
            }
        }
    }

    #[test]
    fn preserved_regions_stay_solid_and_too_much_kept_is_refused() {
        let (mesh, mut study) = cantilever([12, 4, 2]);
        study.max_iterations = 3;
        // The top face (+Y, label 3), 3 mm deep.
        study.preserved.push(Preserve {
            labels: vec![3],
            depth: 3.0,
        });
        let r = run(&mesh, &study);
        for (e, t) in mesh.tets.iter().enumerate() {
            let y = t.iter().map(|&i| mesh.nodes[i as usize][1]).sum::<f64>() / 4.0;
            if y > 20.0 - 3.0 {
                assert_eq!(r.density[e], 1.0, "element {e} at y = {y}");
            }
        }
        study.preserved[0].depth = 15.0;
        match optimize(
            &mesh,
            &[STEEL],
            &study,
            &SolveOptions::default(),
            &AtomicBool::new(false),
            &|_| {},
        ) {
            Err(TopologyError::KeptTooMuch { share, controls }) => {
                assert!(share > 0.4 && !controls, "{share}")
            }
            other => panic!("expected the kept regions refused, got {other:?}"),
        }
        study.target = 1.0;
        assert!(matches!(
            optimize(
                &mesh,
                &[STEEL],
                &study,
                &SolveOptions::default(),
                &AtomicBool::new(false),
                &|_| {}
            ),
            Err(TopologyError::BadTarget)
        ));
    }

    /// A box from the origin to `size` meshed as [`TetMesh::structured_box`] does it, but
    /// with the cells of the upper half along each axis mirrored, so the mesh itself is
    /// symmetric about the box's middle planes (`n` even). Faces labelled as there.
    fn mirrored_box(size: V3, n: [usize; 3]) -> TetMesh {
        let (nx, ny) = (n[0] + 1, n[1] + 1);
        let index = |i: [usize; 3]| (i[0] + nx * (i[1] + ny * i[2])) as u32;
        let mut mesh = TetMesh::default();
        for k in 0..=n[2] {
            for j in 0..=n[1] {
                for i in 0..=n[0] {
                    let at = [i, j, k];
                    mesh.nodes.push(std::array::from_fn(|a| {
                        size[a] * at[a] as f64 / n[a] as f64
                    }));
                }
            }
        }
        for k in 0..n[2] {
            for j in 0..n[1] {
                for i in 0..n[0] {
                    let cell = [i, j, k];
                    let corner = |d: [usize; 3]| {
                        index(std::array::from_fn(|a| {
                            if 2 * cell[a] >= n[a] {
                                cell[a] + 1 - d[a]
                            } else {
                                cell[a] + d[a]
                            }
                        }))
                    };
                    for tet in crate::mesh::kuhn_tets(corner) {
                        mesh.push_tet(tet, 0);
                    }
                }
            }
        }
        mesh.find_boundary(|c| {
            let centre: V3 = std::array::from_fn(|a| (c[0][a] + c[1][a] + c[2][a]) / 3.0);
            (0..6)
                .find(|&s| {
                    let side = if s % 2 == 0 { 0.0 } else { size[s / 2] };
                    (centre[s / 2] - side).abs() < 1e-9
                })
                .map_or(crate::NO_LABEL, |s| s as u32)
        });
        mesh
    }

    fn centres(mesh: &TetMesh) -> Vec<V3> {
        mesh.tets
            .iter()
            .map(|t| {
                t.iter().fold([0.0; 3], |s, &i| {
                    add(s, scale(mesh.nodes[i as usize], 0.25))
                })
            })
            .collect()
    }

    /// The element whose centre is each element's moved by `f`, if there is one.
    fn moved(mesh: &TetMesh, f: impl Fn(V3) -> V3) -> Vec<Option<usize>> {
        let key = |c: V3| c.map(|v| (v * 1e6).round() as i64);
        let centres = centres(mesh);
        let index: BTreeMap<[i64; 3], usize> = centres
            .iter()
            .enumerate()
            .map(|(e, &c)| (key(c), e))
            .collect();
        centres
            .iter()
            .map(|&c| index.get(&key(f(c))).copied())
            .collect()
    }

    /// The largest difference of density between mirror images.
    fn lopsided(r: &TopologyResults, mirror: &[Option<usize>]) -> f64 {
        mirror
            .iter()
            .enumerate()
            .map(|(e, m)| (r.density[e] - r.density[m.unwrap()]).abs())
            .fold(0.0, f64::max)
    }

    #[test]
    fn symmetry_mirrors_the_shape_whatever_the_loads() {
        // A 60 × 20 × 10 mm beam on a symmetric mesh, held at both ends and loaded down on
        // its top face near one end only (label 7, x < 20).
        let mut mesh = mirrored_box([60.0, 20.0, 10.0], [24, 8, 2]);
        for f in 0..mesh.facets.len() {
            let facet = mesh.facets[f];
            let corners = mesh.facet_corners(&facet).map(|i| mesh.nodes[i as usize]);
            if facet.label == 3 && corners.iter().all(|c| c[0] <= 20.0) {
                mesh.facets[f].label = 7;
            }
        }
        let (_, mut study) = cantilever([1, 1, 1]);
        study.study.fixtures[0].labels = vec![0, 1];
        study.max_iterations = 4;
        let across = moved(&mesh, |c| [60.0 - c[0], c[1], c[2]]);
        let through = moved(&mesh, |c| [c[0], c[1], 10.0 - c[2]]);
        // Free, the shape follows the load to its end.
        let free = run(&mesh, &study);
        assert!(
            lopsided(&free, &across) > 0.1,
            "{}",
            lopsided(&free, &across)
        );
        // Mirrored across the Right plane moved to the middle (x = 30) and the Front plane
        // moved to the middle of the thickness (z = 5): the same on both sides, to rounding.
        study.max_iterations = 15;
        study.symmetry = Some(Symmetry {
            point: [30.0, 0.0, 5.0],
            planes: [true, false, true],
        });
        let r = run(&mesh, &study);
        assert!(lopsided(&r, &across) < 1e-9, "{}", lopsided(&r, &across));
        assert!(lopsided(&r, &through) < 1e-9, "{}", lopsided(&r, &through));
        assert!(
            (r.mass_fraction() - 0.4).abs() < 0.01,
            "{}",
            r.mass_fraction()
        );
        assert!(r.compliance() < r.history[0].compliance);
        // The loaded faces keep their material, and so do their mirror images.
        for f in mesh.facets.iter().filter(|f| f.label == 7) {
            let e = f.tet as usize;
            assert_eq!((r.density[e], r.density[across[e].unwrap()]), (1.0, 1.0));
        }
        // A plane the model is not symmetric about is refused: the Right plane itself.
        study.symmetry = Some(Symmetry {
            point: [0.0; 3],
            planes: [true, false, false],
        });
        match optimize(
            &mesh,
            &[STEEL],
            &study,
            &SolveOptions::default(),
            &AtomicBool::new(false),
            &|_| {},
        ) {
            Err(TopologyError::NotSymmetric { axis: 0, at }) => assert_eq!(at, 0.0),
            other => panic!("expected the plane refused, got {other:?}"),
        }
    }

    #[test]
    fn a_pull_direction_leaves_no_undercut() {
        let (mesh, mut study) = cantilever([24, 8, 2]);
        study.max_iterations = 20;
        // Each element and the one above it (the same element of the cell above).
        let above = moved(&mesh, |c| add(c, [0.0, 2.5, 0.0]));
        let pairs: Vec<(usize, usize)> = above
            .iter()
            .enumerate()
            .filter_map(|(e, a)| a.map(|a| (e, a)))
            .collect();
        // How much denser an element gets than the one behind it, at most.
        let undercut = |r: &TopologyResults, pairs: &[(usize, usize)]| {
            pairs
                .iter()
                .map(|&(back, front)| r.density[front] - r.density[back])
                .fold(f64::NEG_INFINITY, f64::max)
        };
        // Free, the cantilever's truss has material over holes: undercuts for a mould
        // pulled up.
        let free = run(&mesh, &study);
        assert!(undercut(&free, &pairs) > 0.2, "{}", undercut(&free, &pairs));
        // Pulled up, the shape standing on its bottom face: going up, no density grows.
        study.pull = Some(Pull {
            direction: [0.0, 2.0, 0.0],
            parting: None,
        });
        let up = run(&mesh, &study);
        assert!(undercut(&up, &pairs) <= 0.0, "{}", undercut(&up, &pairs));
        assert!(
            (up.mass_fraction() - 0.4).abs() < 0.02,
            "{}",
            up.mass_fraction()
        );
        assert!(up.compliance() < up.history[0].compliance);
        // Pulled both ways from the middle plane (y = 10): no density grows going away from
        // it, up or down.
        study.pull = Some(Pull {
            direction: [0.0, -1.0, 0.0],
            parting: Some([0.0, 10.0, 0.0]),
        });
        let both = run(&mesh, &study);
        let centre = centres(&mesh);
        let y = |e: usize| centre[e][1];
        let away: Vec<(usize, usize)> = pairs
            .iter()
            .filter_map(|&(low, high)| {
                if y(low) > 10.0 {
                    Some((low, high))
                } else if y(high) < 10.0 {
                    Some((high, low))
                } else {
                    None
                }
            })
            .collect();
        assert!(undercut(&both, &away) <= 0.0, "{}", undercut(&both, &away));
        assert!(undercut(&both, &pairs) > 0.2, "{}", undercut(&both, &pairs));
        // A mould pulled one way across a symmetry plane is refused, and no direction.
        study.symmetry = Some(Symmetry {
            point: [0.0, 10.0, 0.0],
            planes: [false, true, false],
        });
        study.pull = Some(Pull {
            direction: [0.0, 1.0, 0.0],
            parting: None,
        });
        let refused = |study: &TopologyStudy| {
            optimize(
                &mesh,
                &[STEEL],
                study,
                &SolveOptions::default(),
                &AtomicBool::new(false),
                &|_| {},
            )
            .err()
        };
        assert_eq!(refused(&study), Some(TopologyError::PullAcrossSymmetry));
        study.pull = Some(Pull {
            direction: [0.0; 3],
            parting: None,
        });
        assert_eq!(refused(&study), Some(TopologyError::NoPullDirection));
    }

    #[test]
    fn a_full_density_gives_the_whole_boundary_closed() {
        let mesh = TetMesh::structured_box([0.0; 3], [10.0, 4.0, 2.0], [5, 2, 1]);
        let full = vec![1.0; mesh.tets.len()];
        let s = shape(&mesh, &full, 0.5, 0);
        assert!(is_closed(&s));
        assert!((s.volume() - 80.0).abs() < 1e-9, "{}", s.volume());
        // All of it the model's own faces, each triangle with its face's label.
        assert_eq!(s.labels.len(), s.triangles.len());
        assert!(s.labels.iter().all(|&l| l < 6));
        // On ten-node meshes too: the corners carry the shape.
        let mut quadratic = mesh.clone();
        quadratic.make_quadratic();
        let s = shape(&quadratic, &full, 0.5, 3);
        assert!(is_closed(&s) && (s.volume() - 80.0).abs() < 1e-9);
        // Nothing above the threshold, nothing at all.
        assert!(
            shape(&mesh, &vec![0.0; mesh.tets.len()], 0.5, 0)
                .triangles
                .is_empty()
        );
    }

    #[test]
    fn a_cut_shape_is_closed_and_holds_what_is_above_the_threshold() {
        let mesh = TetMesh::structured_box([0.0; 3], [20.0, 10.0, 10.0], [20, 10, 10]);
        // Solid where x < 7 or inside a ball at the far end: a closed surface with an
        // isolated piece, cut across elements.
        let density: Vec<f64> = mesh
            .tets
            .iter()
            .map(|t| {
                let c = t.iter().fold([0.0; 3], |s, &i| {
                    add(s, scale(mesh.nodes[i as usize], 0.25))
                });
                let ball = norm(sub(c, [15.0, 5.0, 5.0])) < 3.5;
                if c[0] < 7.0 || ball { 1.0 } else { 0.0 }
            })
            .collect();
        for smoothing in [0, 5] {
            let s = shape(&mesh, &density, 0.5, smoothing);
            assert!(is_closed(&s), "smoothing {smoothing}");
            assert_eq!(s.labels.len(), s.triangles.len());
            assert!(s.labels.contains(&crate::NO_LABEL) && s.labels.contains(&0));
            let solid: f64 = density
                .iter()
                .enumerate()
                .map(|(e, d)| d * mesh.tet_volume(e).unwrap())
                .sum();
            // The cut runs through the elements next to the solid ones, so the volume is
            // the solid's give or take half an element layer around it.
            let v = s.volume();
            assert!((v - solid).abs() < 0.2 * solid, "{v} for {solid}");
        }
    }

    /// A 30 × 10 × 10 mm box of 2 mm cells, solid where `solid` says of an element's
    /// centre: its mesh and densities.
    fn blocks(solid: impl Fn(V3) -> bool) -> (TetMesh, Vec<f64>) {
        let mesh = TetMesh::structured_box([0.0; 3], [30.0, 10.0, 10.0], [15, 5, 5]);
        let density = centres(&mesh)
            .iter()
            .map(|&c| if solid(c) { 1.0 } else { 0.0 })
            .collect();
        (mesh, density)
    }

    #[test]
    fn pieces_tell_what_each_holds() {
        // The ends, apart: one on the −X face (label 0), one on +X (1), both on the sides.
        let (mesh, density) = blocks(|c| c[0] < 8.0 || c[0] > 22.0);
        let p = pieces(&mesh, &density, 0.5);
        assert_eq!(p.len(), 2);
        assert!(p[0].labels.contains(&0) != p[1].labels.contains(&0));
        assert!(
            p.iter()
                .all(|p| p.labels.contains(&2) && !p.labels.contains(&crate::NO_LABEL))
        );
        // Each holds about its share of the box: 8 mm of 30, give or take a layer.
        assert!(p.iter().all(|p| (p.volume - 800.0).abs() < 200.0), "{p:?}");
        // Joined in the middle, one piece.
        let (mesh, density) = blocks(|c| c[0] < 8.0 || c[0] > 22.0 || c[1] < 4.0);
        assert_eq!(pieces(&mesh, &density, 0.5).len(), 1);
    }

    #[test]
    fn a_piece_the_loads_do_not_reach_is_joined_and_loose_bits_go() {
        // The held end (−X), the loaded end (+X) apart from it, and a loose bit on top in
        // between that holds nothing.
        let (mesh, mut density) = blocks(|c| {
            c[0] < 8.0
                || c[0] > 22.0
                || ((13.0..17.0).contains(&c[0]) && c[1] > 7.0 && (3.0..7.0).contains(&c[2]))
        });
        assert_eq!(pieces(&mesh, &density, 0.5).len(), 3);
        let centre = centres(&mesh);
        let volume: Vec<f64> = (0..mesh.tets.len())
            .map(|e| mesh.tet_volume(e).unwrap())
            .collect();
        let filter = Filter::new(&centre, 3.0, 1, None);
        let joint = Joint {
            mesh: &mesh,
            filter: &filter,
            volume: &volume,
            mass: &volume,
            size: 2.0,
            symmetry: None,
            chain: None,
        };
        let joining = joint.join(&[0, 1], &[1], &mut density);
        assert_eq!((joining.joined, joining.removed), (1, 1), "{joining:?}");
        let after = pieces(&mesh, &density, 0.5);
        assert_eq!(after.len(), 1);
        assert!(after[0].labels.contains(&0) && after[0].labels.contains(&1));
        // A bar about 6 mm thick across the 14 mm gap: a small share of the box.
        assert!(
            joining.added_mass > 0.02 && joining.added_mass < 0.25,
            "{joining:?}"
        );
        assert!(joining.removed_mass > 0.0 && joining.removed_mass < 0.05);
        // The joined shape is closed, in one piece.
        assert!(is_closed(&shape(&mesh, &density, 0.5, 0)));
    }

    #[test]
    fn a_cantilever_kept_in_one_piece_comes_out_in_one_piece() {
        // Ten iterations on a coarse mesh: a grey design, whose cut at half density falls
        // apart without the joining.
        let (mesh, mut study) = cantilever([12, 4, 2]);
        study.max_iterations = 10;
        let loose = run(&mesh, &study);
        assert!(loose.joining.is_none());
        study.one_piece = true;
        let r = run(&mesh, &study);
        let joining = r.joining.unwrap();
        let after = pieces(&mesh, &r.density, 0.5);
        assert_eq!(after.len(), 1, "{joining:?}");
        // The held end (label 0) and the loaded strip (7) in it. The bar is as thick as the
        // thinnest member, here 16 mm in a 20 × 10 mm beam: some of its mass.
        assert!(after[0].labels.contains(&0) && after[0].labels.contains(&7));
        assert!(joining.added_mass < 0.3, "{joining:?}");
        // Joining changes nothing where the shape was already in one piece.
        let before = pieces(&mesh, &loose.density, 0.5).len();
        assert_eq!(joining.joined + joining.removed == 0, before == 1);
    }

    #[test]
    fn distances_to_a_triangle() {
        let (a, b, c) = ([0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]);
        let d = |p: V3| point_triangle_distance(p, a, b, c);
        assert!((d([1.0, 1.0, 2.0]) - 2.0).abs() < 1e-12);
        assert!((d([-3.0, -4.0, 0.0]) - 5.0).abs() < 1e-12);
        assert!((d([2.0, -1.0, 0.0]) - 1.0).abs() < 1e-12);
        assert!((d([3.0, 3.0, 0.0]) - (2.0f64).sqrt()).abs() < 1e-12);
    }
}
