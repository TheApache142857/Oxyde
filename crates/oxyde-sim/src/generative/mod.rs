//! Generative design: shapes grown between bodies to keep and bodies to stay clear of, as
//! Fusion 360's generative design makes them (`docs/SIMULATION.md` §9).
//!
//! - **The design space** ([`Grid`]): the box around the preserve bodies cut into cubic
//!   voxels, each *preserve* (always material), *obstacle* or *outside* (never material) or
//!   *design*. Fixtures and loads act on the grid's nodes through the preserve voxels.
//! - **The elements** are the voxels themselves, eight-node bricks all alike, solved by
//!   conjugate gradients preconditioned by geometric multigrid, the finest level never
//!   assembled (`multigrid`): that is what lets the grid be fine.
//! - **The optimization** is SIMP as in [`crate::topology`]: a design density per voxel (or
//!   per group of voxels the manufacturing ties together), averaged over a sphere (the
//!   density filter: no member thinner than the minimum thickness), then pushed towards
//!   solid or void by a smoothed Heaviside projection whose sharpness `β` doubles as the
//!   design settles, so the shape comes out crisp rather than grey; the stiffness goes as
//!   the cube of the projected density. Optimality criteria move the design, the mass target
//!   met by the projected densities (linearised at each iteration, so it is met once the
//!   design settles).
//! - **Manufacturing** ([`Method`]): *symmetry* ties each voxel to its mirror images (the
//!   grid is laid symmetric about the planes, so the images are voxels); *2-axis cutting*
//!   ties every voxel of a column along the cutting direction together; *casting* caps each
//!   projected density at the one behind it along the ejection direction (one way, or both
//!   ways from a parting plane), so nothing hangs over void the mould could not leave.
//!   Material a method cannot avoid (behind a preserve voxel, in line with one) is made
//!   solid, and room it cannot use (in front of an obstacle, in line with one) void.
//! - [`surface`] cuts the densities into a smooth closed surface.

// Indices into the grid's arrays and the design's groups, all sized here from the grid.
#![allow(clippy::indexing_slicing)]

mod brick;
mod grid;
mod multigrid;
mod surface;

pub use grid::{Grid, MAX_VOXELS, Voxel, design_box, voxel_size};
pub use surface::surface;

use std::sync::atomic::{AtomicBool, Ordering};

use crate::material::Elastic;
use crate::math::V3;
use crate::mesher::SurfaceMesh;
use crate::solve::{Fixture, Load, SolveOptions, thread_count};
use crate::topology::Symmetry;
use brick::{Brick, DOFS, von_mises};
use multigrid::{Dims, Fine, par_fill};

/// The stiffness left in void voxels, as a fraction of the material's.
const VOID_STIFFNESS: f64 = 1e-6;
/// SIMP's penalty: stiffness goes as the projected density to this power.
const PENALTY: f64 = 3.0;
/// The projection's threshold, and its sharpness at the end; it starts at 1 and doubles
/// when the design settles, or after `BETA_STEP_AFTER` iterations at one sharpness (which
/// brings it to the end within 125 iterations).
const ETA: f64 = 0.5;
const BETA_MAX: f64 = 32.0;
const BETA_STEP_AFTER: usize = 25;
/// The design densities never go under this, so optimality criteria can bring any back.
const DESIGN_MIN: f64 = 1e-3;
/// The most a design density moves in one iteration, and from a sharpness of 8 on.
const MOVE_LIMIT: f64 = 0.2;
const LATE_MOVE_LIMIT: f64 = 0.1;
/// Settled, at the last sharpness: no design density moves by more than this, or, the
/// densities going back and forth at the move limit as optimality criteria do once the
/// shape is crisp, the compliance has changed by less than `STEADY_COMPLIANCE` over the
/// last `STEADY_OVER` iterations with the mass on target to `STEADY_MASS`.
const SETTLED_CHANGE: f64 = 0.01;
const STEADY_COMPLIANCE: f64 = 1e-3;
const STEADY_OVER: usize = 10;
const STEADY_MASS: f64 = 0.01;
/// The densities are kept every this many iterations, for the slider through them.
pub const SNAPSHOT_EVERY: usize = 10;
/// The iterations' solves stop at this residual, a fraction of the loads (as topology
/// studies' do), and give up after this many conjugate gradient iterations.
const SOLVE_TOLERANCE: f64 = 1e-5;
const SOLVE_ITERATIONS: usize = 2000;
const NONE: u32 = u32::MAX;

/// A body no material may go into.
#[derive(Debug, Clone, PartialEq)]
pub struct Obstacle {
    pub surface: SurfaceMesh,
    /// mm it is grown by on every side; 0 for none.
    pub offset: f64,
}

/// Where material may go, from the model's bodies.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DesignSpace {
    /// The bodies kept as they are, their faces labelled: fixtures and loads name them.
    pub preserve: Vec<SurfaceMesh>,
    pub obstacles: Vec<Obstacle>,
    /// A body the material starts in and stays inside; `None` for the box around the
    /// preserve bodies.
    pub starting_shape: Option<SurfaceMesh>,
    /// mm the box is grown by around the preserve bodies when there is no starting shape;
    /// `None` for a tenth of its largest side.
    pub margin: Option<f64>,
}

/// What a generative study asks, for one material and one way of making it.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerativeStudy {
    pub space: DesignSpace,
    /// On faces of the preserve bodies, by label.
    pub fixtures: Vec<Fixture>,
    pub loads: Vec<Load>,
    /// kg: the outcome's mass, preserve voxels included.
    pub mass_target: f64,
    /// mm: the thinnest member wanted; `None` for three voxels.
    pub member_size: Option<f64>,
    pub max_iterations: usize,
    /// mm: the voxels' edge ([`voxel_size`] gives it from a resolution).
    pub voxel_size: f64,
    /// Planes the outcome is mirrored about.
    pub symmetry: Option<Symmetry>,
}

/// One of the six directions along the axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Direction {
    /// 0 for X, 1 for Y, 2 for Z.
    pub axis: usize,
    pub positive: bool,
}

/// How an outcome is to be made.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Method {
    /// Any shape.
    Unrestricted,
    /// 2-axis cutting (a water jet, a laser): the shape the same all along the axis.
    Cutting { axis: usize },
    /// Cast in a mould pulled off along `direction`: going that way the shape never widens,
    /// so it has no undercut. With `parting`, a two-part mould pulled off both ways from the
    /// plane across the direction through that point.
    Casting {
        direction: Direction,
        parting: Option<V3>,
    },
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let axis = |a: usize| ["X", "Y", "Z"][a.min(2)];
        match self {
            Method::Unrestricted => write!(f, "unrestricted"),
            Method::Cutting { axis: a } => write!(f, "2-axis cutting along {}", axis(*a)),
            Method::Casting { direction, parting } => write!(
                f,
                "casting pulled {}{}{}",
                if parting.is_some() {
                    "both ways along "
                } else if direction.positive {
                    "towards +"
                } else {
                    "towards −"
                },
                axis(direction.axis),
                if parting.is_some() {
                    " from the parting plane"
                } else {
                    ""
                }
            ),
        }
    }
}

/// One iteration of an outcome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GenerativeStep {
    /// N·mm: the compliance of the design solved (the work of the loads; the lower, the
    /// stiffer).
    pub compliance: f64,
    /// kg: its mass.
    pub mass: f64,
    /// The largest change of a design density the update made.
    pub change: f64,
}

/// A shape generated for one material and one method.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Each voxel's density at the end before the projection, 0 (void) to 255 (solid), as
    /// the files keep it: the shape is where it is over half, as it is where the projected
    /// density is (the projection keeps half where it is). The projected densities are all
    /// but 0 or 1, and a surface cut from them would step from voxel to voxel; these vary
    /// smoothly over the filter's radius, and place it between the voxels' centres.
    pub density: Vec<u8>,
    /// Every [`SNAPSHOT_EVERY`]th iteration's densities, as `density`, by iteration from 1.
    pub snapshots: Vec<(usize, Vec<u8>)>,
    pub history: Vec<GenerativeStep>,
    /// The design settled before the iteration limit.
    pub converged: bool,
    /// kg.
    pub mass: f64,
    /// N·mm.
    pub compliance: f64,
    /// mm: the largest displacement of a node of the material.
    pub max_displacement: f64,
    /// MPa: the largest von Mises stress at the centres of solid voxels, but those the
    /// fixtures and loads act through (where stress is singular). Voxels' corners
    /// concentrate stress, so it is an estimate.
    pub max_von_mises: f64,
    /// The yield strength over it, when the material has one.
    pub min_factor_of_safety: Option<f64>,
    /// Conjugate gradient iterations, over every solve.
    pub solver_iterations: usize,
    /// mm: the density filter's radius.
    pub filter_radius: f64,
}

/// Why an outcome could not be generated.
#[derive(Debug, Clone, PartialEq)]
pub enum GenerativeError {
    /// No preserve body, or one without a surface.
    NoPreserve,
    /// The voxel size is not a positive length.
    BadVoxelSize,
    TooManyVoxels {
        count: usize,
    },
    /// No voxel is left for material to go in.
    NoDesignSpace,
    /// An obstacle reaches well into a preserve body (their indices).
    ObstacleOverPreserve {
        obstacle: usize,
        preserve: usize,
    },
    /// A fixture or a load is on a face of no preserve body.
    FaceNotOnPreserve {
        label: u32,
    },
    /// No fixture holds anything.
    NotHeld,
    /// The loads act on nothing.
    NoLoads,
    /// The mass target is not between the preserve voxels' mass and the whole design
    /// space's, kg.
    BadTarget {
        min: f64,
        max: f64,
    },
    /// The material has no usable modulus, Poisson's ratio or density.
    BadMaterial,
    /// The method cannot make the design space: a preserve body and an obstacle in line
    /// along its direction, the one behind the other, at `at` (mm).
    NotMakeable {
        method: Method,
        at: V3,
    },
    /// A casting direction crosses a symmetry plane that is not its parting plane.
    PullAcrossSymmetry,
    NotConverged {
        iterations: usize,
        residual: f64,
    },
    /// The solver failed: why.
    Solver(String),
    Cancelled,
}

impl std::fmt::Display for GenerativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenerativeError::NoPreserve => write!(
                f,
                "the study has no preserve body: give one or more bodies the Preserve role"
            ),
            GenerativeError::BadVoxelSize => {
                write!(f, "the resolution does not give a usable voxel size")
            }
            GenerativeError::TooManyVoxels { count } => write!(
                f,
                "the design space would need {count} voxels, more than {MAX_VOXELS}: lower the resolution"
            ),
            GenerativeError::NoDesignSpace => write!(
                f,
                "no room is left for material: the obstacles and preserve bodies fill the design space"
            ),
            GenerativeError::ObstacleOverPreserve { obstacle, preserve } => write!(
                f,
                "obstacle {} reaches into preserve body {}: material cannot be both kept and kept out",
                obstacle + 1,
                preserve + 1
            ),
            GenerativeError::FaceNotOnPreserve { .. } => write!(
                f,
                "a fixture or a load is on a face of a body that is not a preserve body: put them on preserve bodies"
            ),
            GenerativeError::NotHeld => write!(
                f,
                "nothing holds the design: add a fixture on a preserve body"
            ),
            GenerativeError::NoLoads => write!(
                f,
                "no load acts on the design: add a force or a pressure on a preserve body"
            ),
            GenerativeError::BadTarget { min, max } => write!(
                f,
                "the mass target must be between {:.3} kg (the preserve bodies) and {:.3} kg (the whole design space)",
                min, max
            ),
            GenerativeError::BadMaterial => {
                write!(f, "the material has no usable stiffness or density")
            }
            GenerativeError::NotMakeable { method, at } => write!(
                f,
                "{method} cannot make this design space: a preserve body and an obstacle are in line along its direction near ({:.1}, {:.1}, {:.1}) mm",
                at[0], at[1], at[2]
            ),
            GenerativeError::PullAcrossSymmetry => write!(
                f,
                "the casting direction crosses a symmetry plane, and a mould pulled one way cannot give a mirrored shape: pull both ways from that plane, or take it off"
            ),
            GenerativeError::NotConverged {
                iterations,
                residual,
            } => write!(
                f,
                "the solve did not converge ({iterations} iterations, residual {residual:.1e}): the design may be held too loosely"
            ),
            GenerativeError::Solver(why) => write!(f, "the solver failed: {why}"),
            GenerativeError::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// What a voxel is to the optimization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// A design density decides it.
    Free,
    /// Always material.
    Solid,
    /// Never material, and no element.
    Void,
}

/// Generates the outcome of `study` on `grid` (from [`Grid::build`] of the same study) for
/// one material and one method. `progress` hears the fraction done, from 0 to 1, as the
/// iteration limit counts it.
pub fn generate(
    grid: &Grid,
    study: &GenerativeStudy,
    method: Method,
    material: &Elastic,
    options: &SolveOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<Outcome, GenerativeError> {
    let modulus = material.youngs_modulus;
    if !(modulus.is_finite()
        && modulus > 0.0
        && material.poisson > -1.0
        && material.poisson < 0.5
        && material.density.is_finite()
        && material.density > 0.0)
    {
        return Err(GenerativeError::BadMaterial);
    }
    let threads = thread_count(options.threads);
    let h = grid.size;
    let voxels = grid.voxel_count();
    let symmetric = grid_symmetry(grid, study);
    let state = states(grid, method, study.symmetry.as_ref(), symmetric)?;
    let (group, groups) = groups(grid, method, &state, symmetric);

    let radius = study
        .member_size
        .filter(|d| d.is_finite() && *d > 0.0)
        .map_or(1.5 * h, |d| 0.5 * d)
        .max(1.5 * h);
    // 2-axis cutting filters across the cut only, so whole columns stay alike.
    let flat = match method {
        Method::Cutting { axis } => Some(axis.min(2)),
        _ => None,
    };
    let filter = Filter::new(grid, &state, radius, flat, threads);
    let chains = match method {
        Method::Casting { direction, parting } => Some(casting_chains(grid, direction, parting)),
        _ => None,
    };

    // kg per voxel: kg/m³ × mm³ × 1e-9.
    let voxel_mass = material.density * h * h * h * 1e-9;
    let solid = state.iter().filter(|s| **s == State::Solid).count();
    let free = state.iter().filter(|s| **s == State::Free).count();
    let min = solid as f64 * voxel_mass;
    let max = (solid + free) as f64 * voxel_mass;
    let target = study.mass_target;
    if !(target > min && target < max) {
        return Err(GenerativeError::BadTarget { min, max });
    }

    let brick = Brick::new(material.poisson);
    let dims = grid.dims();
    let nodes = dims.node_count();
    let mut held = vec![NONE; nodes];
    let mut projectors = Vec::with_capacity(grid.holds.len());
    for (node, p) in &grid.holds {
        if let Some(slot) = held.get_mut(*node as usize) {
            *slot = projectors.len() as u32;
            projectors.push(*p);
        }
    }
    let mut loads = vec![0.0; 3 * nodes];
    for (node, f) in &grid.forces {
        let n = *node as usize;
        if n < nodes {
            loads[3 * n..3 * n + 3].copy_from_slice(f);
        }
    }

    // The design starts uniform, at the density whose projection meets the target.
    let mut beta = 1.0;
    let share = (target - min) / (max - min);
    let t = (beta * ETA).tanh();
    let start = (ETA + ((2.0 * share - 1.0) * t).atanh() / beta).clamp(DESIGN_MIN, 1.0);
    let mut x = vec![start; groups];
    let mut displacement = vec![0.0; 3 * nodes];
    let iterations = study.max_iterations.max(1);
    let mut history: Vec<GenerativeStep> = Vec::new();
    let mut snapshots: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut converged = false;
    let mut at_beta = 0;
    let mut solver_iterations = 0;
    let mut solved_rho: Vec<f64> = Vec::new();
    let mut solved_field: Vec<f64> = Vec::new();
    let mut compliance = 0.0;

    for iteration in 0..iterations {
        if cancel.load(Ordering::Relaxed) {
            return Err(GenerativeError::Cancelled);
        }
        // Design, filtered, capped along a casting direction, projected.
        let design: Vec<f64> = (0..voxels)
            .map(|v| match state[v] {
                State::Free => x[group[v] as usize],
                State::Solid => 1.0,
                State::Void => 0.0,
            })
            .collect();
        let filtered = filter.forward(&design, threads);
        let (capped, source) = match &chains {
            Some(chains) => cap(chains, &state, &filtered),
            None => (filtered, Vec::new()),
        };
        let rho: Vec<f64> = (0..voxels)
            .map(|v| match state[v] {
                State::Free => project(capped[v], beta),
                State::Solid => 1.0,
                State::Void => 0.0,
            })
            .collect();
        let stiffness: Vec<f64> = (0..voxels)
            .map(|v| match state[v] {
                State::Free => {
                    modulus * h * (VOID_STIFFNESS + rho[v].powf(PENALTY) * (1.0 - VOID_STIFFNESS))
                }
                State::Solid => modulus * h,
                State::Void => 0.0,
            })
            .collect();
        let fine = Fine::new(dims, &brick, &stiffness, &held, &projectors);
        let solved = multigrid::solve(
            &fine,
            &loads,
            &mut displacement,
            options.tolerance.max(SOLVE_TOLERANCE),
            SOLVE_ITERATIONS,
            cancel,
            threads,
        )?;
        solver_iterations += solved.iterations;
        let energies = element_energies(dims, &brick, &displacement, &stiffness, threads);
        // The work of the loads on the displacements solved.
        compliance = loads.iter().zip(&displacement).map(|(f, u)| f * u).sum();
        let mass_now: f64 = rho.iter().sum::<f64>() * voxel_mass;

        // The slopes of the compliance and the mass for the projected densities, carried
        // back through the projection, the cap and the filter to the design groups.
        let back = |per_voxel: &dyn Fn(usize) -> f64| -> Vec<f64> {
            let mut s: Vec<f64> = (0..voxels)
                .map(|v| {
                    if state[v] == State::Free {
                        per_voxel(v) * project_slope(capped[v], beta)
                    } else {
                        0.0
                    }
                })
                .collect();
            if !source.is_empty() {
                let mut carried = vec![0.0; voxels];
                for v in 0..voxels {
                    if state[v] == State::Free {
                        carried[source[v] as usize] += s[v];
                    }
                }
                s = carried;
            }
            let per_design = filter.backward(&s, threads);
            let mut out = vec![0.0; groups];
            for v in 0..voxels {
                if state[v] == State::Free {
                    out[group[v] as usize] += per_design[v];
                }
            }
            out
        };
        let dc = back(&|v| {
            -PENALTY
                * rho[v].powf(PENALTY - 1.0)
                * (1.0 - VOID_STIFFNESS)
                * modulus
                * h
                * energies[v]
        });
        let dm = back(&|_| voxel_mass);
        let budget = target - mass_now + dm.iter().zip(&x).map(|(m, x)| m * x).sum::<f64>();
        let limit = if beta >= 8.0 {
            LATE_MOVE_LIMIT
        } else {
            MOVE_LIMIT
        };
        let next = optimality_update(&x, &dc, &dm, budget, limit);
        let change = x
            .iter()
            .zip(&next)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        history.push(GenerativeStep {
            compliance,
            mass: mass_now,
            change,
        });
        if (iteration + 1) % SNAPSHOT_EVERY == 0 {
            snapshots.push((iteration + 1, bytes(&capped, &state)));
        }
        solved_rho = rho;
        solved_field = capped;
        x = next;
        at_beta += 1;
        progress((iteration + 1) as f64 / iterations as f64);
        let steady = beta >= BETA_MAX && at_beta >= STEADY_OVER && {
            let recent = &history[history.len() - STEADY_OVER..];
            let (lo, hi) = recent.iter().fold((f64::INFINITY, 0.0f64), |(lo, hi), s| {
                (lo.min(s.compliance), hi.max(s.compliance))
            });
            hi - lo <= STEADY_COMPLIANCE * hi && (mass_now - target).abs() <= STEADY_MASS * target
        };
        if (beta >= BETA_MAX && change < SETTLED_CHANGE) || steady {
            converged = true;
            break;
        }
        if beta < BETA_MAX && (change < SETTLED_CHANGE || at_beta >= BETA_STEP_AFTER) {
            beta *= 2.0;
            at_beta = 0;
        }
    }

    // What the last design solved weighs, how far it moves and how hard it is worked.
    let material_voxel =
        |v: usize| state[v] == State::Solid || (state[v] == State::Free && solved_rho[v] >= 0.5);
    let mass = solved_rho.iter().sum::<f64>() * voxel_mass;
    let mut max_displacement = 0.0f64;
    let mut max_von_mises = 0.0f64;
    let offsets = dims.corner_offsets();
    for v in (0..voxels).filter(|&v| material_voxel(v)) {
        let c = grid.coords(v);
        let base = dims.node(c);
        let mut u = [0.0; DOFS];
        for (a, off) in offsets.iter().enumerate() {
            let n = base + off;
            u[3 * a..3 * a + 3].copy_from_slice(&displacement[3 * n..3 * n + 3]);
            let d = (u[3 * a].powi(2) + u[3 * a + 1].powi(2) + u[3 * a + 2].powi(2)).sqrt();
            max_displacement = max_displacement.max(d);
        }
        if grid.anchored.binary_search(&(v as u32)).is_ok() {
            continue;
        }
        let strain: [f64; 6] = std::array::from_fn(|r| {
            (0..DOFS).map(|k| brick.b[r * DOFS + k] * u[k]).sum::<f64>() / h
        });
        let stress: [f64; 6] = std::array::from_fn(|r| {
            modulus * (0..6).map(|k| brick.d[r * 6 + k] * strain[k]).sum::<f64>()
        });
        max_von_mises = max_von_mises.max(von_mises(&stress));
    }
    let min_factor_of_safety = (material.yield_strength > 0.0 && max_von_mises > 0.0)
        .then(|| material.yield_strength / max_von_mises);
    progress(1.0);
    Ok(Outcome {
        density: bytes(&solved_field, &state),
        snapshots,
        history,
        converged,
        mass,
        compliance,
        max_displacement,
        max_von_mises,
        min_factor_of_safety,
        solver_iterations,
        filter_radius: radius,
    })
}

/// Densities as the files keep them: 0 to 255.
fn bytes(rho: &[f64], state: &[State]) -> Vec<u8> {
    rho.iter()
        .zip(state)
        .map(|(r, s)| match s {
            State::Solid => 255,
            State::Void => 0,
            State::Free => (r.clamp(0.0, 1.0) * 255.0).round() as u8,
        })
        .collect()
}

/// The smoothed Heaviside projection at sharpness `beta`, and its slope.
fn project(x: f64, beta: f64) -> f64 {
    let t = (beta * ETA).tanh();
    (t + (beta * (x - ETA)).tanh()) / (t + (beta * (1.0 - ETA)).tanh())
}

fn project_slope(x: f64, beta: f64) -> f64 {
    let t = (beta * ETA).tanh();
    let th = (beta * (x - ETA)).tanh();
    beta * (1.0 - th * th) / (t + (beta * (1.0 - ETA)).tanh())
}

/// The next design by optimality criteria: each group's density times the square root of
/// its compliance slope over `λ` times its mass slope, within the move limit and
/// `DESIGN_MIN` to 1, `λ` found by bisection (on its logarithm) so that the mass,
/// linearised by `dm`, is `budget`.
fn optimality_update(x: &[f64], dc: &[f64], dm: &[f64], budget: f64, limit: f64) -> Vec<f64> {
    let largest_dm = dm.iter().copied().fold(0.0, f64::max);
    let ratio: Vec<Option<f64>> = dc
        .iter()
        .zip(dm)
        .map(|(c, m)| (*m > 1e-12 * largest_dm).then(|| (-c).max(0.0) / m))
        .collect();
    let largest = ratio.iter().flatten().copied().fold(0.0, f64::max);
    if !(largest > 0.0 && largest.is_finite()) {
        return x.to_vec();
    }
    let at = |lambda: f64| -> Vec<f64> {
        x.iter()
            .zip(&ratio)
            .map(|(&d, r)| match r {
                None => d,
                Some(r) => {
                    let lo = (d - limit).max(DESIGN_MIN);
                    let hi = (d + limit).min(1.0);
                    (d * (r / lambda).sqrt()).clamp(lo, hi)
                }
            })
            .collect()
    };
    let mass_of = |next: &[f64]| -> f64 { next.iter().zip(dm).map(|(x, m)| x * m).sum() };
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

/// `uₑᵀ K uₑ` of each voxel with an element, for a unit modulus and edge (the brick's own).
fn element_energies(
    dims: Dims,
    brick: &Brick,
    u: &[f64],
    stiffness: &[f64],
    threads: usize,
) -> Vec<f64> {
    let offsets = dims.corner_offsets();
    let mut out = vec![0.0; dims.cell_count()];
    par_fill(&mut out, 1, threads, 2048, |first, part| {
        let mut ue = [0.0; DOFS];
        for (m, o) in part.iter_mut().enumerate() {
            let e = first + m;
            if stiffness[e] <= 0.0 {
                continue;
            }
            let base = dims.node(dims.cell_coords(e));
            for (a, off) in offsets.iter().enumerate() {
                let n = base + off;
                ue[3 * a..3 * a + 3].copy_from_slice(&u[3 * n..3 * n + 3]);
            }
            // K u by columns (K is symmetric), then u · K u.
            let mut ku = [0.0; DOFS];
            for (row, &uj) in brick.k.as_chunks::<DOFS>().0.iter().zip(&ue) {
                for (o, kv) in ku.iter_mut().zip(row) {
                    *o += kv * uj;
                }
            }
            *o = ku.iter().zip(&ue).map(|(a, b)| a * b).sum();
        }
    });
    out
}

/// The symmetry planes the grid was laid out for: those of the study whose plane is the
/// grid's middle along its axis.
fn grid_symmetry(grid: &Grid, study: &GenerativeStudy) -> [bool; 3] {
    let Some(s) = study.symmetry else {
        return [false; 3];
    };
    std::array::from_fn(|a| {
        let middle = grid.origin[a] + 0.5 * grid.counts[a] as f64 * grid.size;
        s.planes[a]
            && grid.counts[a].is_multiple_of(2)
            && (middle - s.point[a]).abs() < 1e-6 * grid.size.max(1.0)
    })
}

/// Voxel `v`'s mirror images across the planes (itself included).
fn mirror_images(grid: &Grid, v: usize, planes: [bool; 3]) -> Vec<usize> {
    let c = grid.coords(v);
    let mut out = vec![c];
    for a in 0..3 {
        if planes[a] {
            let more: Vec<[usize; 3]> = out
                .iter()
                .map(|p| {
                    let mut m = *p;
                    m[a] = grid.counts[a] - 1 - p[a];
                    m
                })
                .collect();
            out.extend(more);
        }
    }
    let mut out: Vec<usize> = out.into_iter().map(|c| grid.index(c)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The voxels of each line of the grid along `axis`, in order.
fn lines(grid: &Grid, axis: usize) -> impl Iterator<Item = Vec<usize>> + '_ {
    let n = grid.counts;
    let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
    (0..n[b]).flat_map(move |j| {
        (0..n[a]).map(move |i| {
            (0..n[axis])
                .map(|k| {
                    let mut c = [0; 3];
                    c[axis] = k;
                    c[a] = i;
                    c[b] = j;
                    grid.index(c)
                })
                .collect()
        })
    })
}

/// The chains a casting direction caps along: each from the back to the front.
fn casting_chains(grid: &Grid, direction: Direction, parting: Option<V3>) -> Vec<Vec<usize>> {
    let axis = direction.axis.min(2);
    let mut out = Vec::new();
    for line in lines(grid, axis) {
        match parting {
            None => {
                let mut line = line;
                if !direction.positive {
                    line.reverse();
                }
                out.push(line);
            }
            Some(p) => {
                // On each side, from the parting plane outwards.
                let (front, back): (Vec<usize>, Vec<usize>) = line
                    .into_iter()
                    .partition(|&v| grid.centre(v)[axis] >= p[axis]);
                let mut back = back;
                back.reverse();
                for chain in [front, back] {
                    if !chain.is_empty() {
                        out.push(chain);
                    }
                }
            }
        }
    }
    out
}

/// What each voxel is to the optimization, once symmetry and the method have made the
/// design space one they can give.
fn states(
    grid: &Grid,
    method: Method,
    symmetry: Option<&Symmetry>,
    symmetric: [bool; 3],
) -> Result<Vec<State>, GenerativeError> {
    let mut state: Vec<State> = grid
        .kinds
        .iter()
        .map(|k| match k {
            Voxel::Design => State::Free,
            Voxel::Preserve => State::Solid,
            Voxel::Obstacle | Voxel::Outside => State::Void,
        })
        .collect();
    if let (Method::Casting { direction, parting }, Some(s)) = (method, symmetry) {
        // A pull along a plane keeps the mirror; across one only from it both ways.
        let a = direction.axis.min(2);
        if s.planes[a] && !parting.is_some_and(|p| (p[a] - s.point[a]).abs() < 1e-6) {
            return Err(GenerativeError::PullAcrossSymmetry);
        }
    }
    if symmetric.contains(&true) {
        for v in 0..state.len() {
            let images = mirror_images(grid, v, symmetric);
            if images.first() != Some(&v) {
                continue;
            }
            let forced = if images.iter().any(|&w| state[w] == State::Void) {
                Some(State::Void)
            } else if images.iter().any(|&w| state[w] == State::Solid) {
                Some(State::Solid)
            } else {
                None
            };
            if let Some(f) = forced {
                for w in images {
                    if state[w] == State::Free {
                        state[w] = f;
                    }
                }
            }
        }
    }
    let not_makeable = |v: usize| GenerativeError::NotMakeable {
        method,
        at: grid.centre(v),
    };
    match method {
        Method::Unrestricted => {}
        Method::Cutting { axis } => {
            for line in lines(grid, axis.min(2)) {
                let void = line.iter().find(|&&v| state[v] == State::Void);
                let solid = line.iter().find(|&&v| state[v] == State::Solid);
                let forced = match (void, solid) {
                    (Some(_), Some(&s)) => return Err(not_makeable(s)),
                    (Some(_), None) => State::Void,
                    (None, Some(_)) => State::Solid,
                    (None, None) => continue,
                };
                for v in line {
                    if state[v] == State::Free {
                        state[v] = forced;
                    }
                }
            }
        }
        Method::Casting { direction, parting } => {
            for chain in casting_chains(grid, direction, parting) {
                // In front of a void, void; behind a solid, solid; a solid in front of a
                // void is an undercut.
                let mut void = false;
                for &v in &chain {
                    match state[v] {
                        State::Void => void = true,
                        State::Solid if void => return Err(not_makeable(v)),
                        State::Free if void => state[v] = State::Void,
                        _ => {}
                    }
                }
                let mut solid = false;
                for &v in chain.iter().rev() {
                    match state[v] {
                        State::Solid => solid = true,
                        State::Free if solid => state[v] = State::Solid,
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(state)
}

/// Each free voxel's design group (mirror images, and with 2-axis cutting whole columns,
/// share one), `NONE` for the others, numbered in voxel order; and how many groups.
fn groups(grid: &Grid, method: Method, state: &[State], symmetric: [bool; 3]) -> (Vec<u32>, usize) {
    let n = state.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(parent: &mut [u32], mut v: u32) -> u32 {
        while parent[v as usize] != v {
            let up = parent[parent[v as usize] as usize];
            parent[v as usize] = up;
            v = up;
        }
        v
    }
    fn union(parent: &mut [u32], a: usize, b: usize) {
        let (ra, rb) = (find(parent, a as u32), find(parent, b as u32));
        if ra != rb {
            let (lo, hi) = (ra.min(rb), ra.max(rb));
            parent[hi as usize] = lo;
        }
    }
    if symmetric.contains(&true) {
        for v in 0..n {
            if state[v] != State::Free {
                continue;
            }
            for w in mirror_images(grid, v, symmetric) {
                if w > v && state[w] == State::Free {
                    union(&mut parent, v, w);
                }
            }
        }
    }
    if let Method::Cutting { axis } = method {
        for line in lines(grid, axis.min(2)) {
            let free: Vec<usize> = line
                .into_iter()
                .filter(|&v| state[v] == State::Free)
                .collect();
            for pair in free.windows(2) {
                union(&mut parent, pair[0], pair[1]);
            }
        }
    }
    let mut id = vec![NONE; n];
    let mut count = 0usize;
    let mut out = vec![NONE; n];
    for v in 0..n {
        if state[v] != State::Free {
            continue;
        }
        let root = find(&mut parent, v as u32) as usize;
        if id[root] == NONE {
            id[root] = count as u32;
            count += 1;
        }
        out[v] = id[root];
    }
    (out, count)
}

/// Densities capped along each chain at the one behind them, each with the voxel whose
/// filtered density it took.
fn cap(chains: &[Vec<usize>], state: &[State], filtered: &[f64]) -> (Vec<f64>, Vec<u32>) {
    let mut out = filtered.to_vec();
    let mut source: Vec<u32> = (0..filtered.len() as u32).collect();
    for chain in chains {
        let mut behind: Option<(f64, u32)> = None;
        for &v in chain {
            match state[v] {
                State::Void => {
                    out[v] = 0.0;
                    behind = Some((0.0, v as u32));
                }
                State::Solid => {
                    out[v] = 1.0;
                    behind = Some((1.0, v as u32));
                }
                State::Free => {
                    if let Some((b, s)) = behind
                        && b < filtered[v]
                    {
                        out[v] = b;
                        source[v] = s;
                    }
                    behind = Some((out[v], source[v]));
                }
            }
        }
    }
    (out, source)
}

/// The density filter on the grid: each free voxel's density the design averaged over a
/// sphere of radius `r` (a disc across the axis it is flat along), weights `r − d`, over
/// the voxels that are not void.
struct Filter<'a> {
    grid: &'a Grid,
    state: &'a [State],
    /// Offsets in voxels within the radius, and their weights.
    offsets: Vec<([i64; 3], f64)>,
    norms: Vec<f64>,
}

impl<'a> Filter<'a> {
    fn new(
        grid: &'a Grid,
        state: &'a [State],
        radius: f64,
        flat: Option<usize>,
        threads: usize,
    ) -> Filter<'a> {
        let reach = (radius / grid.size).ceil() as i64;
        let mut offsets = Vec::new();
        for dz in -reach..=reach {
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    let d = grid.size * ((dx * dx + dy * dy + dz * dz) as f64).sqrt();
                    if d < radius && flat.is_none_or(|a| [dx, dy, dz][a] == 0) {
                        offsets.push(([dx, dy, dz], radius - d));
                    }
                }
            }
        }
        let mut filter = Filter {
            grid,
            state,
            offsets,
            norms: Vec::new(),
        };
        let ones = vec![1.0; grid.voxel_count()];
        filter.norms = filter.sum(&ones, threads);
        filter
    }

    /// `Σ w x` over the stencil around each free voxel, the void ones left out (0 for the
    /// voxels that are not free).
    fn sum(&self, x: &[f64], threads: usize) -> Vec<f64> {
        let grid = self.grid;
        let n = grid.counts.map(|c| c as i64);
        let mut out = vec![0.0; grid.voxel_count()];
        par_fill(&mut out, 1, threads, 2048, |first, part| {
            for (m, o) in part.iter_mut().enumerate() {
                let v = first + m;
                if self.state[v] != State::Free {
                    continue;
                }
                let c = grid.coords(v).map(|x| x as i64);
                let mut s = 0.0;
                for (off, w) in &self.offsets {
                    let at = [c[0] + off[0], c[1] + off[1], c[2] + off[2]];
                    if (0..3).any(|a| at[a] < 0 || at[a] >= n[a]) {
                        continue;
                    }
                    let j = (at[0] + n[0] * (at[1] + n[1] * at[2])) as usize;
                    if self.state[j] != State::Void {
                        s += w * x[j];
                    }
                }
                *o = s;
            }
        });
        out
    }

    /// The filtered densities of `design` (1 at solid voxels).
    fn forward(&self, design: &[f64], threads: usize) -> Vec<f64> {
        let sums = self.sum(design, threads);
        sums.iter()
            .zip(&self.norms)
            .zip(self.state)
            .map(|((s, n), st)| {
                if *st == State::Free && *n > 0.0 {
                    s / n
                } else {
                    0.0
                }
            })
            .collect()
    }

    /// The filter's transpose: slopes for the filtered densities carried to the design.
    /// The weights are symmetric, so it is the same sum over the slopes divided by each
    /// voxel's norm.
    fn backward(&self, slopes: &[f64], threads: usize) -> Vec<f64> {
        let scaled: Vec<f64> = slopes
            .iter()
            .zip(&self.norms)
            .zip(self.state)
            .map(|((s, n), st)| {
                if *st == State::Free && *n > 0.0 {
                    s / n
                } else {
                    0.0
                }
            })
            .collect();
        self.sum(&scaled, threads)
    }
}

#[cfg(test)]
mod tests;
