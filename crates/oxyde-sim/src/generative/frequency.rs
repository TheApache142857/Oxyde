//! The frequency limit: the design's lowest natural frequencies, held by the first load
//! case's fixtures, kept at or above a minimum, as Fusion 360's generative design offers a
//! modal frequency limit and NASA's brackets keep their first mode clear of the launch's.
//!
//! - **The eigenproblem** `K φ = λ M φ` (`λ = ω²`) on the grid: `K` the iteration's own
//!   stiffness, never assembled ([`Fine`]), `M` lumped (an eighth of each voxel's mass at
//!   each of its corners), solved for its lowest pairs by LOBPCG ([`crate::eigen`])
//!   preconditioned by one V-cycle of the static solve's multigrid ([`Cycle`]), and
//!   warm-started from the last iteration's modes, which change little from one to the next.
//!   With a backend that takes it (a graphics card's, [`Backend::eigen_space`]), the whole
//!   eigensolve runs there: its vectors, products, cycles and sums; only the dot products
//!   come back.
//! - **The mass** of a voxel goes as its projected density, but below [`MASS_KNEE`] as the
//!   sixth power of it, scaled to meet it there (Du and Olhoff, 2007): the stiffness goes as
//!   the cube, so near-void regions, lighter than they are weak, would otherwise vibrate on
//!   their own at frequencies near zero and hide the structure's.
//! - **The slopes**: for `φ` normalised to `φᵀ M φ = 1`, `dλ/dρ = φₑᵀ (dK/dρ − λ dM/dρ) φₑ`
//!   voxel by voxel. The lowest few modes are limited, each a constraint of its own, so the
//!   limit holds when two of them swap places as the design changes.

use std::sync::atomic::AtomicBool;

use super::brick::Brick;
use super::element_energies;
use super::multigrid::{Cycle, Dims, Fine, Operator};
use super::{Backend, GenerativeError, System};
use crate::eigen::{self, EigenError, EigenOptions, EigenProblem};

/// The modes a frequency limit keeps above it, the lowest first.
pub(super) const LIMITED_MODES: usize = 3;
/// Vectors beyond them in the eigensolver's block, to speed the last of them up.
const MODE_GUARD: usize = 2;
/// The eigensolver's tolerance, as `crate::eigen` measures it, and its iterations at most,
/// each optimization iteration.
const MODE_TOLERANCE: f64 = 1e-6;
const MODE_ITERATIONS: usize = 300;
/// Below this projected density, a voxel's mass goes as the sixth power of its density.
pub(super) const MASS_KNEE: f64 = 0.1;
/// The least share of its solid mass a voxel with an element keeps, so that the mass is
/// nowhere zero where there is stiffness.
const MASS_FLOOR: f64 = 1e-9;

/// The lowest natural modes of a design.
pub(super) struct Modes {
    /// `ω²`, s⁻², ascending.
    pub values: Vec<f64>,
    /// Normalised to `φᵀ M φ = 1`, M in tonnes.
    pub vectors: Vec<Vec<f64>>,
}

/// A voxel's share of its solid mass in the frequencies at projected density `r`, and its
/// slope: the density above [`MASS_KNEE`], its sixth power scaled to meet it below.
pub(super) fn mass_share(r: f64) -> (f64, f64) {
    if r > MASS_KNEE {
        (r, 1.0)
    } else {
        let r = r.max(0.0);
        // c MASS_KNEE⁶ = MASS_KNEE.
        let c = MASS_KNEE.powi(-5);
        ((c * r.powi(6)).max(MASS_FLOOR), 6.0 * c * r.powi(5))
    }
}

/// Per node, its lumped mass: an eighth of each voxel's of `masses` at each of its corners,
/// voxel by voxel in order.
pub(super) fn nodal_masses(dims: Dims, masses: &[f64]) -> Vec<f64> {
    let offsets = dims.corner_offsets();
    let mut out = vec![0.0; dims.node_count()];
    for (v, m) in masses.iter().enumerate() {
        if *m == 0.0 {
            continue;
        }
        let base = dims.node(dims.cell_coords(v));
        for off in &offsets {
            out[base + off] += m / 8.0;
        }
    }
    out
}

/// The lowest [`LIMITED_MODES`] modes of `fine` (its held nodes the fixtures') with the
/// nodes' masses `nodal` (tonnes), from `start` (the last iteration's) when there is one; on
/// `backend` when it takes the eigensolve, else on the CPU.
pub(super) fn lowest_modes(
    fine: &Fine,
    nodal: &[f64],
    start: Option<&[Vec<f64>]>,
    backend: Option<&mut (dyn Backend + '_)>,
    cancel: &AtomicBool,
    threads: usize,
) -> Result<Modes, GenerativeError> {
    let options = EigenOptions {
        count: LIMITED_MODES,
        guard: MODE_GUARD,
        tolerance: MODE_TOLERANCE,
        max_iterations: MODE_ITERATIONS,
    };
    // Out of iterations, the modes as far as they got: the next iteration starts from them.
    let failed = |e: EigenError| match e {
        EigenError::Cancelled => GenerativeError::Cancelled,
        EigenError::Degenerate => GenerativeError::Solver(
            "the design space has too few voxels for its natural frequencies".to_owned(),
        ),
        EigenError::Failed(why) => GenerativeError::Solver(why),
    };
    if let Some(backend) = backend {
        // No loads: the system is the stiffness and the fixtures.
        let system = System {
            fine,
            loads: vec![0.0; 3 * nodal.len()],
            threads,
        };
        if let Some(space) = backend.eigen_space(&system, nodal) {
            let mut space = space?;
            let found = eigen::smallest_in(space.as_mut(), &options, start, cancel, &|_| {})
                .map_err(failed)?;
            return Ok(Modes {
                values: found.values,
                vectors: found.vectors,
            });
        }
    }
    let mut cycle = Cycle::new(fine, threads)?;
    let apply_a = |x: &[f64], y: &mut [f64]| {
        fine.apply(x, y, threads);
        fine.project(y);
    };
    let apply_b = |x: &[f64], y: &mut [f64]| {
        let pairs = y
            .as_chunks_mut::<3>()
            .0
            .iter_mut()
            .zip(x.as_chunks::<3>().0);
        for ((out, input), m) in pairs.zip(nodal) {
            for (o, i) in out.iter_mut().zip(input) {
                *o = m * i;
            }
        }
        fine.project(y);
    };
    let project = |v: &mut [f64]| fine.project(v);
    let mut precondition = |r: &[f64], z: &mut [f64]| cycle.apply(fine, r, z);
    let mut problem = EigenProblem {
        size: 3 * nodal.len(),
        apply_a: &apply_a,
        apply_b: &apply_b,
        precondition: &mut precondition,
        project: &project,
        progress: &|_| {},
    };
    let found = eigen::smallest(&mut problem, &options, start, cancel).map_err(failed)?;
    Ok(Modes {
        values: found.values,
        vectors: found.vectors,
    })
}

/// Per voxel, the slope of the eigenvalue `lambda` of mode `phi` (normalised to `φᵀ M φ = 1`)
/// for its projected density: `stiffness_slope(v)` times `φₑᵀ K₀ φₑ` (the unit brick's)
/// less `lambda` times `mass_slope(v)` (tonnes) times `Σ |φ|² / 8` over its corners.
#[allow(clippy::too_many_arguments)]
pub(super) fn eigenvalue_slopes(
    dims: Dims,
    brick: &Brick,
    phi: &[f64],
    stiffness: &[f64],
    lambda: f64,
    stiffness_slope: &dyn Fn(usize) -> f64,
    mass_slope: &dyn Fn(usize) -> f64,
    threads: usize,
) -> Vec<f64> {
    let energies = element_energies(dims, brick, phi, stiffness, threads);
    let offsets = dims.corner_offsets();
    (0..dims.cell_count())
        .map(|v| {
            let base = dims.node(dims.cell_coords(v));
            let squares: f64 = offsets
                .iter()
                .map(|off| {
                    let n = base + off;
                    phi[3 * n..3 * n + 3].iter().map(|x| x * x).sum::<f64>()
                })
                .sum();
            stiffness_slope(v) * energies[v] - lambda * mass_slope(v) * squares / 8.0
        })
        .collect()
}

/// Hz of `ω²`.
pub(super) fn hertz(lambda: f64) -> f64 {
    lambda.max(0.0).sqrt() / (2.0 * std::f64::consts::PI)
}
