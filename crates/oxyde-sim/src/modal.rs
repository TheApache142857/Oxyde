//! A frequency study, as SolidWorks has it: the lowest natural frequencies of the model
//! held by its fixtures, and the shape it vibrates in at each.
//!
//! - **Stiffness** is the static study's matrix ([`crate::solve`]): the same elements,
//!   fixtures and nodes' frames. Contacts are their ties as a static solve starts them, all
//!   closed, so a separation, sliding or rough contact holds as it does at rest: a linear
//!   modal analysis has no contact that opens.
//! - **Mass** is consistent with the elements (the exact integrals of their shape functions,
//!   `solve::element_mass`); millimetres and newtons make the tonne the unit of mass.
//!   Held directions have no mass, so their modes never appear.
//! - **The eigenproblem** `K φ = ω² M φ` is solved for its lowest pairs by LOBPCG
//!   ([`crate::eigen`]) preconditioned by the static solve's multigrid on `K`
//!   (block-Jacobi when the hierarchy cannot be built), with a few guard vectors beyond the
//!   modes asked for.
//! - **The results**: each mode's frequency `f = ω / 2π` in hertz, its shape scaled so that
//!   the largest displacement is 1 (a mode's size is arbitrary: only its shape means
//!   anything), and its effective mass along X, Y and Z as a share of the model's mass, as
//!   SolidWorks lists the mass participation. For a mode `φ` normalised to `φᵀ M φ = 1`, the
//!   effective mass along `d` is `(φᵀ M r_d)²`, `r_d` every node moving one along `d`; `M r_d`
//!   is each node's share of the weight along `d`, which is what the rows of `M` sum to.

// Indices into the mesh (checked when the problem is prepared) and arrays sized here.
#![allow(clippy::indexing_slicing)]

use std::sync::atomic::{AtomicBool, Ordering};

use crate::contact::Contact;
use crate::eigen::{self, EigenError, EigenOptions, EigenProblem};
use crate::material::Elastic;
use crate::math::{V3, norm, scale};
use crate::mesh::TetMesh;
use crate::solve::{Fixture, Prepared, Problem, SolveError, SolveOptions, Study};

/// The most modes a study may ask for.
pub const MAX_MODES: usize = 50;
/// A mode has converged when its residual is under this share of `‖K φ‖ + ω² ‖M φ‖`: its
/// frequency is then exact to far better than a millionth.
const TOLERANCE: f64 = 1e-6;
/// LOBPCG iterations at most; multigrid makes it converge in tens.
const MAX_ITERATIONS: usize = 600;

/// What a frequency study holds: fixtures and contacts, no loads.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FrequencyStudy {
    pub fixtures: Vec<Fixture>,
    /// Contacts between bodies meshed apart, all closed (bonded, or held along their
    /// normal for those that only press).
    pub contacts: Vec<Contact>,
    /// The modes wanted, the lowest first: 1 to [`MAX_MODES`].
    pub modes: usize,
}

/// A natural mode of vibration.
#[derive(Debug, Clone, PartialEq)]
pub struct Mode {
    /// Hz.
    pub frequency: f64,
    /// `ω²`, s⁻².
    pub eigenvalue: f64,
    /// Each node's displacement in the mode, the largest of length 1.
    pub shape: Vec<V3>,
    /// The mode's effective mass along X, Y and Z, as a share of the model's mass.
    pub participation: V3,
}

/// The results of a frequency study.
#[derive(Debug, Clone, PartialEq)]
pub struct FrequencyResults {
    /// The lowest first.
    pub modes: Vec<Mode>,
    /// kg: the model's mass, which the participation is a share of.
    pub mass: f64,
    /// LOBPCG iterations.
    pub iterations: usize,
    /// The largest residual of the modes at the end, as their tolerance measures it.
    pub residual: f64,
}

impl Mode {
    /// The length of each node's displacement in the mode: the plot of its shape.
    pub fn magnitudes(&self) -> Vec<f64> {
        self.shape.iter().map(|u| norm(*u)).collect()
    }
}

/// Solves the frequency study on the mesh, `materials[b]` being body `b`'s (with a
/// density). `progress` hears the fraction done, from 0 to 1.
pub fn solve_frequencies(
    mesh: &TetMesh,
    materials: &[Elastic],
    study: &FrequencyStudy,
    options: &SolveOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<FrequencyResults, SolveError> {
    let structural = Study {
        fixtures: study.fixtures.clone(),
        loads: Vec::new(),
        gravity: None,
        contacts: study.contacts.clone(),
    };
    let problem = Problem {
        mesh,
        materials,
        study: &structural,
        options,
        strain: None,
    };
    // The mesh, the materials and the fixtures checked: a piece free to move has modes at
    // 0 Hz that would hide the others.
    let prepared = Prepared::unloaded(&problem)?;
    // A body without mass cannot vibrate.
    for &b in &mesh.bodies {
        if !materials
            .get(b as usize)
            .is_some_and(|m| m.density.is_finite() && m.density > 0.0)
        {
            return Err(SolveError::BadMaterial { body: b });
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(SolveError::Cancelled);
    }
    progress(0.02);
    let mut stiffness = prepared.stiffness(&problem, None);
    prepared.add_closed_ties(&mut stiffness);
    let mass = prepared.mass(&problem);
    progress(0.1);

    let count = study.modes.clamp(1, MAX_MODES);
    let mut cycle = prepared.cycle(&stiffness);
    let apply_k = |x: &[f64], y: &mut [f64]| prepared.multiply(&stiffness, x, y);
    let apply_m = |x: &[f64], y: &mut [f64]| prepared.multiply(&mass, x, y);
    let project = |v: &mut [f64]| prepared.project_held(v);
    let mut precondition = |r: &[f64], z: &mut [f64]| cycle.apply(r, z);
    let report = |f: f64| progress(0.1 + 0.85 * f);
    let mut eigenproblem = EigenProblem {
        size: mesh.nodes.len() * 3,
        apply_a: &apply_k,
        apply_b: &apply_m,
        precondition: &mut precondition,
        project: &project,
        progress: &report,
    };
    let eigen_options = EigenOptions {
        count,
        guard: (count / 4).clamp(2, 8),
        tolerance: TOLERANCE.max(options.tolerance),
        max_iterations: MAX_ITERATIONS,
    };
    let found =
        eigen::smallest(&mut eigenproblem, &eigen_options, None, cancel).map_err(|e| match e {
            EigenError::Cancelled => SolveError::Cancelled,
            EigenError::Degenerate => SolveError::Invalid(
                "it is too coarse for the modes asked for: ask for fewer, or mesh it finer"
                    .to_owned(),
            ),
        })?;
    if !found.converged {
        return Err(SolveError::NotConverged {
            iterations: found.iterations,
            residual: found.residual,
        });
    }

    // Each node's share of the model's weight: what `M r_d` is along each direction.
    let quadratic = mesh.is_quadratic();
    let mut nodal_mass = vec![0.0; mesh.nodes.len()];
    let mut total = 0.0;
    for (e, g) in prepared.geometries.iter().enumerate() {
        let density = materials
            .get(mesh.bodies[e] as usize)
            .map_or(0.0, |m| m.density);
        let element = density * 1e-12 * g.volume;
        total += element;
        let (nodes, count) = mesh.element_nodes(e);
        for (k, &i) in nodes[..count].iter().enumerate() {
            let share = match (quadratic, k < 4) {
                (false, _) => 0.25,
                (true, true) => -0.05,
                (true, false) => 0.2,
            };
            nodal_mass[i as usize] += share * element;
        }
    }
    let modes = found
        .values
        .iter()
        .zip(&found.vectors)
        .map(|(&lambda, vector)| {
            let normalised = prepared.global(vector);
            let mut gamma = [0.0; 3];
            for (u, m) in normalised.iter().zip(&nodal_mass) {
                for d in 0..3 {
                    gamma[d] += u[d] * m;
                }
            }
            let participation = if total > 0.0 {
                gamma.map(|g| g * g / total)
            } else {
                [0.0; 3]
            };
            let largest = normalised.iter().map(|u| norm(*u)).fold(0.0, f64::max);
            let shape = if largest > 0.0 {
                normalised
                    .iter()
                    .map(|u| scale(*u, 1.0 / largest))
                    .collect()
            } else {
                normalised
            };
            let eigenvalue = lambda.max(0.0);
            Mode {
                frequency: eigenvalue.sqrt() / (2.0 * std::f64::consts::PI),
                eigenvalue,
                shape,
                participation,
            }
        })
        .collect();
    progress(1.0);
    Ok(FrequencyResults {
        modes,
        mass: total * 1000.0,
        iterations: found.iterations,
        residual: found.residual,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solve::FixtureKind;

    const STEEL: Elastic = Elastic::new(200_000.0, 0.3, 250.0, 7850.0);

    fn run(mesh: &TetMesh, study: &FrequencyStudy) -> FrequencyResults {
        let cancel = AtomicBool::new(false);
        solve_frequencies(
            mesh,
            &[STEEL],
            study,
            &SolveOptions::default(),
            &cancel,
            &|_| {},
        )
        .unwrap()
    }

    /// A cantilever 200 × 20 × 10 mm, fixed at x = 0.
    fn cantilever(modes: usize) -> (TetMesh, FrequencyStudy) {
        let mut mesh = TetMesh::structured_box([0.0; 3], [200.0, 20.0, 10.0], [20, 4, 2]);
        mesh.make_quadratic();
        let study = FrequencyStudy {
            fixtures: vec![Fixture {
                kind: FixtureKind::Fixed,
                labels: vec![0],
            }],
            contacts: Vec::new(),
            modes,
        };
        (mesh, study)
    }

    /// The first bending frequency of a cantilever by Euler–Bernoulli: `(1.8751)² / 2π ·
    /// √(EI / ρAL⁴)`, the section `width` across the bending and `depth` along it.
    fn beam_frequency(width: f64, depth: f64) -> f64 {
        let (e, rho, length) = (200_000.0, 7850.0 * 1e-12, 200.0);
        let i = width * depth.powi(3) / 12.0;
        let area = width * depth;
        let beta = 1.875_104_068_7f64;
        beta * beta / (2.0 * std::f64::consts::PI) * (e * i / (rho * area * length.powi(4))).sqrt()
    }

    #[test]
    fn a_cantilever_vibrates_as_beam_theory_says() {
        let (mesh, study) = cantilever(4);
        let r = run(&mesh, &study);
        assert_eq!(r.modes.len(), 4);
        assert!(r.modes.windows(2).all(|w| w[0].frequency <= w[1].frequency));
        // The first mode bends across the thin side (Z), the second across the wide one
        // (Y); slender (L/h of 20 and 10), so shear and rotary inertia change them by
        // under a couple of percent.
        let thin = beam_frequency(20.0, 10.0);
        let wide = beam_frequency(10.0, 20.0);
        let (first, second) = (r.modes[0].frequency, r.modes[1].frequency);
        assert!(
            (first - thin).abs() < 0.04 * thin,
            "{first} Hz, beam theory {thin} Hz"
        );
        assert!(
            (second - wide).abs() < 0.04 * wide,
            "{second} Hz, beam theory {wide} Hz"
        );
        // Each shape's largest displacement is 1; the first moves along Z, at the tip.
        for m in &r.modes {
            let largest = m.shape.iter().map(|u| norm(*u)).fold(0.0, f64::max);
            assert!((largest - 1.0).abs() < 1e-12);
        }
        let tip = r.modes[0]
            .shape
            .iter()
            .zip(&mesh.nodes)
            .filter(|(_, p)| p[0] > 199.0)
            .map(|(u, _)| u[2].abs())
            .fold(0.0, f64::max);
        assert!(tip > 0.99, "{tip}");
        // A cantilever's first mode carries about 61 % of its mass along its motion.
        let p = r.modes[0].participation;
        assert!(
            p[2] > 0.5 && p[2] < 0.7 && p[0] < 0.01 && p[1] < 0.01,
            "{p:?}"
        );
        assert!((r.mass - 200.0 * 20.0 * 10.0 * 7850.0 * 1e-9).abs() < 1e-9);
        // The same run repeats exactly.
        assert_eq!(r, run(&mesh, &study));
    }

    #[test]
    fn the_modes_are_orthogonal_through_the_mass() {
        // A coarser beam of four-node elements: the consistent mass makes its modes
        // orthogonal, whatever the elements.
        let mesh = TetMesh::structured_box([0.0; 3], [100.0, 10.0, 10.0], [10, 2, 2]);
        let study = FrequencyStudy {
            fixtures: vec![Fixture {
                kind: FixtureKind::Fixed,
                labels: vec![0],
            }],
            contacts: Vec::new(),
            modes: 3,
        };
        let cancel = AtomicBool::new(false);
        let structural = Study {
            fixtures: study.fixtures.clone(),
            ..Study::default()
        };
        let options = SolveOptions::default();
        let problem = Problem {
            mesh: &mesh,
            materials: &[STEEL],
            study: &structural,
            options: &options,
            strain: None,
        };
        let prepared = Prepared::unloaded(&problem).unwrap();
        let mass = prepared.mass(&problem);
        // The mass matrix adds up to the model's mass along each direction, less what the
        // held face's directions hold.
        let ones = vec![1.0; mesh.nodes.len() * 3];
        let mut m1 = vec![0.0; ones.len()];
        prepared.multiply(&mass, &ones, &mut m1);
        let whole = 100.0 * 10.0 * 10.0 * 7850.0 * 1e-12;
        let sum: f64 = m1.iter().sum();
        assert!(
            sum > 2.4 * whole && sum < 3.0 * whole,
            "{sum} of {}",
            3.0 * whole
        );
        let r = solve_frequencies(&mesh, &[STEEL], &study, &options, &cancel, &|_| {}).unwrap();
        // The shapes, scaled back to unit mass, are M-orthonormal.
        let unit: Vec<Vec<f64>> = r
            .modes
            .iter()
            .map(|m| {
                let v: Vec<f64> = m.shape.iter().flatten().copied().collect();
                let mut mv = vec![0.0; v.len()];
                prepared.multiply(&mass, &v, &mut mv);
                let size = v.iter().zip(&mv).map(|(a, b)| a * b).sum::<f64>().sqrt();
                v.iter().map(|x| x / size).collect()
            })
            .collect();
        for (i, u) in unit.iter().enumerate() {
            let mut mu = vec![0.0; u.len()];
            prepared.multiply(&mass, u, &mut mu);
            for (j, v) in unit.iter().enumerate() {
                let p: f64 = v.iter().zip(&mu).map(|(a, b)| a * b).sum();
                let expected = if i == j { 1.0 } else { 0.0 };
                assert!((p - expected).abs() < 1e-6, "{i} {j}: {p}");
            }
        }
    }

    #[test]
    fn a_free_body_or_a_massless_one_is_refused() {
        let (mesh, mut study) = cantilever(2);
        let cancel = AtomicBool::new(false);
        let options = SolveOptions::default();
        let massless = Elastic::new(200_000.0, 0.3, 250.0, 0.0);
        assert_eq!(
            solve_frequencies(&mesh, &[massless], &study, &options, &cancel, &|_| {}),
            Err(SolveError::BadMaterial { body: 0 })
        );
        study.fixtures.clear();
        assert_eq!(
            solve_frequencies(&mesh, &[STEEL], &study, &options, &cancel, &|_| {}),
            Err(SolveError::NotHeld { body: 0 })
        );
        let stop = AtomicBool::new(true);
        let (mesh, study) = cantilever(2);
        assert_eq!(
            solve_frequencies(&mesh, &[STEEL], &study, &options, &stop, &|_| {}),
            Err(SolveError::Cancelled)
        );
    }
}
