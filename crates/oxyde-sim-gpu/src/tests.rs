//! The card against the CPU. On a machine without a usable graphics card each test says so
//! and passes: the CPU's solve, which `oxyde-sim`'s own tests cover, is used there.

use std::sync::atomic::AtomicBool;

use super::*;
use oxyde_sim::generative::{
    CpuBackend, DesignSpace, GenerativeStudy, Grid, Method, Obstacle, Outcome, generate_on,
};
use oxyde_sim::{Elastic, Fixture, FixtureKind, Load, LoadKind, SolveOptions, SurfaceMesh};

const STEEL: Elastic = Elastic::new(200_000.0, 0.3, 250.0, 7850.0);

/// The card, or `None` on a machine without one; a card that will not take the solver
/// fails the test.
fn gpu() -> Option<GpuBackend> {
    match GpuBackend::new() {
        Ok(g) => {
            eprintln!("solving on {}", g.name());
            Some(g)
        }
        Err(Unavailable::NoCard(why)) => {
            eprintln!("no graphics card to test on: {why}");
            None
        }
        Err(Unavailable::Failed(why)) => panic!("the graphics card failed: {why}"),
    }
}

/// A box's surface, its sides labelled `10 × body + side` (−X, +X, −Y, +Y, −Z, +Z).
fn block(body: u32, lo: [f64; 3], hi: [f64; 3]) -> SurfaceMesh {
    let corner = |i: usize| {
        [
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        ]
    };
    let quads: [([u32; 4], u32); 6] = [
        ([0, 4, 6, 2], 0),
        ([1, 3, 7, 5], 1),
        ([0, 1, 5, 4], 2),
        ([2, 6, 7, 3], 3),
        ([0, 2, 3, 1], 4),
        ([4, 5, 7, 6], 5),
    ];
    let mut s = SurfaceMesh {
        positions: (0..8).map(corner).collect(),
        ..SurfaceMesh::default()
    };
    for (q, side) in quads {
        s.triangles.push([q[0], q[1], q[2]]);
        s.triangles.push([q[0], q[2], q[3]]);
        s.labels.extend([10 * body + side; 2]);
    }
    s
}

/// The bracket of `oxyde-sim`'s tests: a pad held at one end of a 60 × 20 × 10 mm box, a
/// small pad loaded 1 kN down at the other.
fn bracket(voxel_size: f64, iterations: usize) -> GenerativeStudy {
    GenerativeStudy {
        space: DesignSpace {
            preserve: vec![
                block(0, [0.0, 0.0, 0.0], [4.0, 20.0, 10.0]),
                block(1, [56.0, 6.0, 3.0], [60.0, 14.0, 7.0]),
            ],
            obstacles: Vec::new(),
            starting_shape: None,
            margin: Some(0.0),
        },
        fixtures: vec![Fixture {
            kind: FixtureKind::Fixed,
            labels: vec![0],
        }],
        loads: vec![Load {
            kind: LoadKind::Force {
                total: 1000.0,
                direction: Some([0.0, -1.0, 0.0]),
            },
            labels: vec![11],
        }],
        gravity: Vec::new(),
        remote_masses: Vec::new(),
        load_cases: Vec::new(),
        objective: Default::default(),
        mass_target: 0.25 * 60.0 * 20.0 * 10.0 * 7850.0 * 1e-9,
        member_size: None,
        max_iterations: iterations,
        voxel_size,
        symmetry: None,
        min_frequency: None,
    }
}

/// Solves each system on the card and on the CPU from the same start, and keeps how far
/// apart the displacements come out, as a share of the largest.
struct Both<'a> {
    gpu: &'a mut GpuBackend,
    worst: f64,
    solves: usize,
}

impl Backend for Both<'_> {
    fn solve(
        &mut self,
        system: &System<'_>,
        x: &mut [f64],
        tolerance: f64,
        max_iterations: usize,
        cancel: &AtomicBool,
    ) -> Result<Solved, GenerativeError> {
        // Both tight, so that what is compared is the answer, not where each stopped.
        let mut reference = x.to_vec();
        CpuBackend.solve(system, &mut reference, 1e-9, max_iterations, cancel)?;
        let mut tight = x.to_vec();
        self.gpu
            .solve(system, &mut tight, 1e-6, max_iterations, cancel)?;
        let scale = reference.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let apart = tight
            .iter()
            .zip(&reference)
            .fold(0.0f64, |m, (a, b)| m.max((a - b).abs()));
        self.worst = self.worst.max(apart / scale.max(f64::MIN_POSITIVE));
        // The optimization goes on with the card's own answer at its own tolerance.
        let solved = self
            .gpu
            .solve(system, x, tolerance, max_iterations, cancel)?;
        self.solves += 1;
        Ok(solved)
    }
}

fn run(study: &GenerativeStudy, backend: Option<&mut dyn Backend>) -> (Grid, Outcome) {
    let cancel = AtomicBool::new(false);
    let grid = Grid::build(study, 0, &cancel).unwrap();
    let outcome = generate_on(
        &grid,
        study,
        Method::Unrestricted,
        &STEEL,
        &SolveOptions::default(),
        backend,
        &cancel,
        &|_| {},
    )
    .unwrap();
    (grid, outcome)
}

#[test]
fn the_card_solves_as_the_cpu_does() {
    let Some(mut card) = gpu() else {
        return;
    };
    // Coarse enough for one level, and fine enough for several; and with a post across the
    // bracket, whose void cells and untouched nodes are where conjugate gradients in single
    // precision drifted.
    for (voxel, post) in [(2.0, false), (1.0, false), (0.8, true)] {
        let mut study = bracket(voxel, 12);
        if post {
            study.space.obstacles.push(Obstacle {
                surface: block(2, [28.0, -5.0, 3.0], [32.0, 25.0, 7.0]),
                offset: 0.0,
            });
        }
        let mut both = Both {
            gpu: &mut card,
            worst: 0.0,
            solves: 0,
        };
        run(&study, Some(&mut both));
        assert_eq!(both.solves, 12);
        // Refined in double precision on the CPU, the card's answer is the CPU's.
        assert!(both.worst < 1e-5, "{voxel} mm: {}", both.worst);
    }
}

#[test]
fn an_outcome_on_the_card_is_the_cpus() {
    let Some(mut card) = gpu() else {
        return;
    };
    let study = bracket(1.25, 150);
    let (_, cpu) = run(&study, None);
    let (grid, on_card) = run(&study, Some(&mut card));
    let near = |a: f64, b: f64, share: f64| (a - b).abs() <= share * b.abs();
    assert!(
        near(on_card.mass, study.mass_target, 0.02),
        "{}",
        on_card.mass
    );
    assert!(
        near(on_card.compliance, cpu.compliance, 0.02),
        "{} {}",
        on_card.compliance,
        cpu.compliance
    );
    // The same shape, but for a few voxels on its surface.
    let differ = (0..grid.voxel_count())
        .filter(|&v| (on_card.density[v] >= 128) != (cpu.density[v] >= 128))
        .count();
    assert!(
        differ * 50 < grid.voxel_count(),
        "{differ} of {}",
        grid.voxel_count()
    );
}

#[test]
fn the_card_repeats_itself() {
    let Some(mut card) = gpu() else {
        return;
    };
    let study = bracket(1.0, 20);
    let (_, first) = run(&study, Some(&mut card));
    let (_, second) = run(&study, Some(&mut card));
    assert_eq!(first, second);
}

/// Static solves on the CPU, the frequency limit's eigensolve on the card.
struct EigenOnCard<'a> {
    gpu: &'a mut GpuBackend,
    spaces: usize,
}

impl Backend for EigenOnCard<'_> {
    fn solve(
        &mut self,
        system: &System<'_>,
        x: &mut [f64],
        tolerance: f64,
        max_iterations: usize,
        cancel: &AtomicBool,
    ) -> Result<Solved, GenerativeError> {
        CpuBackend.solve(system, x, tolerance, max_iterations, cancel)
    }

    fn eigen_space<'s>(
        &'s mut self,
        system: &System<'_>,
        nodal: &[f64],
    ) -> Option<Result<Box<dyn EigenSpace + 's>, GenerativeError>> {
        self.spaces += 1;
        self.gpu.eigen_space(system, nodal)
    }
}

#[test]
fn the_card_finds_the_cpus_natural_frequencies() {
    let Some(mut card) = gpu() else {
        return;
    };
    // A limit too low to act, so the designs stay alike: the lowest frequency the card
    // finds is the CPU's, to the eigensolve's tolerance.
    let mut study = bracket(1.25, 8);
    study.min_frequency = Some(1.0);
    let (_, cpu) = run(&study, None);
    let mut eigen = EigenOnCard {
        gpu: &mut card,
        spaces: 0,
    };
    let (_, on_card) = run(&study, Some(&mut eigen));
    assert_eq!(eigen.spaces, on_card.history.len());
    let (card_hz, cpu_hz) = (on_card.frequency.unwrap(), cpu.frequency.unwrap());
    assert!(
        (card_hz - cpu_hz).abs() <= 1e-6 * cpu_hz,
        "{card_hz} Hz on the card, {cpu_hz} Hz on the CPU"
    );
    // The static solves on the card too: close, the designs a little apart.
    let (_, all) = run(&study, Some(&mut card));
    let all_hz = all.frequency.unwrap();
    assert!(
        (all_hz - cpu_hz).abs() <= 1e-2 * cpu_hz,
        "{all_hz} Hz all on the card, {cpu_hz} Hz on the CPU"
    );
}

#[test]
fn the_cards_eigensolve_repeats_itself() {
    let Some(mut card) = gpu() else {
        return;
    };
    let mut study = bracket(1.0, 6);
    study.min_frequency = Some(1.0);
    let (_, first) = run(&study, Some(&mut card));
    let (_, second) = run(&study, Some(&mut card));
    assert!(first.frequency.is_some());
    assert_eq!(first, second);
}

#[test]
fn every_kernel_has_its_source() {
    for k in K::ALL {
        let source = k.source().unwrap();
        assert!(source.contains("fn main"), "{}", k.name());
    }
}
