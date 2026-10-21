use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicBool;

use super::*;
use crate::mesher::box_surface;
use crate::solve::{FixtureKind, LoadKind};

const STEEL: Elastic = Elastic::new(200_000.0, 0.3, 250.0, 7850.0);

/// A box's surface, its sides labelled `10 × body + side` (side as `box_surface` numbers
/// them: −X, +X, −Y, +Y, −Z, +Z).
fn block(body: u32, lo: V3, hi: V3) -> SurfaceMesh {
    let mut s = box_surface(lo, hi);
    for l in &mut s.labels {
        *l += 10 * body;
    }
    s
}

/// A bracket 60 × 20 × 10 mm: a pad held on its −X face at one end, a small pad loaded
/// 1 kN down on its +X face at the other, the box between them empty.
fn bracket(voxel_size: f64) -> GenerativeStudy {
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
        // A quarter of the box's volume in steel.
        objective: Default::default(),
        mass_target: 0.25 * 60.0 * 20.0 * 10.0 * 7850.0 * 1e-9,
        member_size: None,
        max_iterations: 150,
        voxel_size,
        symmetry: None,
    }
}

fn run(study: &GenerativeStudy, method: Method, threads: usize) -> (Grid, Outcome) {
    let cancel = AtomicBool::new(false);
    let grid = Grid::build(study, threads, &cancel).unwrap();
    let options = SolveOptions {
        threads,
        ..SolveOptions::default()
    };
    let outcome = generate(&grid, study, method, &STEEL, &options, &cancel, &|_| {}).unwrap();
    (grid, outcome)
}

/// Whether the material (density at least half) joins every voxel the loads act through to
/// one the fixtures act through, voxel faces to voxel faces.
fn load_path(grid: &Grid, density: &[u8], from: &[usize], to: &[usize]) -> bool {
    let mut seen = vec![false; grid.voxel_count()];
    let mut queue: VecDeque<usize> = from
        .iter()
        .copied()
        .filter(|&v| density[v] >= 128)
        .collect();
    for &v in &queue {
        seen[v] = true;
    }
    while let Some(v) = queue.pop_front() {
        if to.contains(&v) {
            return true;
        }
        let c = grid.coords(v);
        for a in 0..3 {
            for step in [-1i64, 1] {
                let at = c[a] as i64 + step;
                if at < 0 || at >= grid.counts[a] as i64 {
                    continue;
                }
                let mut d = c;
                d[a] = at as usize;
                let w = grid.index(d);
                if !seen[w] && density[w] >= 128 {
                    seen[w] = true;
                    queue.push_back(w);
                }
            }
        }
    }
    false
}

fn voxels_in(grid: &Grid, lo: V3, hi: V3) -> Vec<usize> {
    (0..grid.voxel_count())
        .filter(|&v| {
            let c = grid.centre(v);
            (0..3).all(|a| c[a] > lo[a] && c[a] < hi[a])
        })
        .collect()
}

#[test]
fn the_grid_covers_the_box_and_sorts_the_voxels() {
    let mut study = bracket(1.0);
    study.space.obstacles.push(Obstacle {
        surface: block(2, [20.0, -5.0, 2.0], [30.0, 25.0, 8.0]),
        offset: 1.0,
    });
    let cancel = AtomicBool::new(false);
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    assert_eq!(grid.counts, [60, 20, 10]);
    assert!((grid.origin[0]).abs() < 1e-12 && (grid.origin[1]).abs() < 1e-12);
    let count = |k: Voxel| grid.kinds.iter().filter(|x| **x == k).count();
    // The pads: 4 × 20 × 10 and 4 × 8 × 4 voxels.
    assert_eq!(count(Voxel::Preserve), 800 + 128);
    // The obstacle grown by 1 mm: x 19..31, z 1..9 across the whole height.
    assert_eq!(count(Voxel::Obstacle), 12 * 20 * 8);
    assert_eq!(count(Voxel::Outside), 0);
    assert_eq!(count(Voxel::Design), 60 * 20 * 10 - 928 - 12 * 20 * 8);
    // Held and loaded nodes: the held face's 21 × 11 corners; the load's forces add up.
    assert_eq!(grid.cases[0].holds.len(), 21 * 11);
    let total = grid.cases[0]
        .forces
        .iter()
        .fold([0.0; 3], |s, (_, f)| crate::math::add(s, *f));
    assert!(
        (total[1] + 1000.0).abs() < 1e-9 && total[0].abs() < 1e-9,
        "{total:?}"
    );

    // The same box laid symmetric about a plane off its middle: grown to stay symmetric.
    study.symmetry = Some(Symmetry {
        point: [0.0, 0.0, 6.0],
        planes: [false, false, true],
    });
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    assert_eq!(grid.counts[2], 12);
    assert!((grid.origin[2]).abs() < 1e-12);

    // A starting shape keeps the material in it.
    study.symmetry = None;
    study.space.starting_shape = Some(block(3, [0.0, 0.0, 0.0], [60.0, 20.0, 5.0]));
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    assert!(grid.kinds.contains(&Voxel::Outside));
    for v in 0..grid.voxel_count() {
        if grid.kinds[v] == Voxel::Design {
            assert!(grid.centre(v)[2] < 5.0);
        }
    }
}

#[test]
fn a_study_set_up_wrong_is_refused() {
    let cancel = AtomicBool::new(false);
    let mut study = bracket(1.0);
    study.loads[0].labels = vec![42];
    assert_eq!(
        Grid::build(&study, 0, &cancel),
        Err(GenerativeError::FaceNotOnPreserve { label: 42 })
    );
    let mut study = bracket(1.0);
    study.space.obstacles.push(Obstacle {
        surface: block(2, [1.0, 1.0, 1.0], [3.0, 3.0, 3.0]),
        offset: 0.0,
    });
    assert_eq!(
        Grid::build(&study, 0, &cancel),
        Err(GenerativeError::ObstacleOverPreserve {
            obstacle: 0,
            preserve: 0
        })
    );
    let mut study = bracket(1.0);
    study.fixtures.clear();
    assert_eq!(
        Grid::build(&study, 0, &cancel),
        Err(GenerativeError::NotHeld)
    );
    let study = bracket(0.001);
    assert!(matches!(
        Grid::build(&study, 0, &cancel),
        Err(GenerativeError::TooManyVoxels { .. })
    ));

    // A target out of reach.
    let mut study = bracket(2.0);
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    study.mass_target = 1e-6;
    let run = |study: &GenerativeStudy, method| {
        generate(
            &grid,
            study,
            method,
            &STEEL,
            &SolveOptions::default(),
            &cancel,
            &|_| {},
        )
    };
    assert!(matches!(
        run(&study, Method::Unrestricted),
        Err(GenerativeError::BadTarget { .. })
    ));
    // 2-axis cutting along X with an obstacle in line with the loaded pad.
    let mut study = bracket(2.0);
    study.space.obstacles.push(Obstacle {
        surface: block(2, [30.0, 8.0, 4.0], [34.0, 12.0, 6.0]),
        offset: 0.0,
    });
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    let cutting = generate(
        &grid,
        &study,
        Method::Cutting { axis: 0 },
        &STEEL,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    );
    assert!(
        matches!(cutting, Err(GenerativeError::NotMakeable { .. })),
        "{cutting:?}"
    );
    // A one-way pull across a symmetry plane.
    let mut study = bracket(2.0);
    study.symmetry = Some(Symmetry {
        point: [0.0, 0.0, 5.0],
        planes: [false, false, true],
    });
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    let pull = Method::Casting {
        direction: Direction {
            axis: 2,
            positive: true,
        },
        parting: None,
    };
    let refused = generate(
        &grid,
        &study,
        pull,
        &STEEL,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    );
    assert_eq!(refused, Err(GenerativeError::PullAcrossSymmetry));
    // Printed across a symmetry plane, likewise.
    let printed = generate(
        &grid,
        &study,
        Method::Additive {
            direction: Direction {
                axis: 2,
                positive: true,
            },
        },
        &STEEL,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    );
    assert_eq!(printed, Err(GenerativeError::BuildAcrossSymmetry));
}

#[test]
fn load_cases_are_laid_apart_and_carried_together() {
    // The bracket's down load, and a second case pushing the small pad sideways (+Z).
    let mut study = bracket(2.0);
    study.loads.push(Load {
        kind: LoadKind::Force {
            total: 600.0,
            direction: Some([0.0, 0.0, 1.0]),
        },
        labels: vec![11],
    });
    let down = LoadCase {
        fixtures: vec![0],
        loads: vec![0],
        ..LoadCase::default()
    };
    let sideways = LoadCase {
        fixtures: vec![0],
        loads: vec![1],
        ..LoadCase::default()
    };
    study.load_cases = vec![down.clone(), sideways];
    let cancel = AtomicBool::new(false);
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    assert_eq!(grid.cases.len(), 2);
    let total = |c: usize| {
        grid.cases[c]
            .forces
            .iter()
            .fold([0.0; 3], |s, (_, f)| crate::math::add(s, *f))
    };
    assert!((total(0)[1] + 1000.0).abs() < 1e-9 && total(0)[2].abs() < 1e-9);
    assert!((total(1)[2] - 600.0).abs() < 1e-9 && total(1)[1].abs() < 1e-9);
    assert_eq!(grid.cases[0].holds, grid.cases[1].holds);
    // The design that carries both works harder in all than the one for the down load
    // alone: its compliance is the two cases' sum.
    let both = generate(
        &grid,
        &study,
        Method::Unrestricted,
        &STEEL,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    )
    .unwrap();
    study.load_cases = vec![down];
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    assert_eq!(grid.cases.len(), 1);
    let alone = generate(
        &grid,
        &study,
        Method::Unrestricted,
        &STEEL,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    )
    .unwrap();
    assert!(both.compliance > alone.compliance);
    // On target as a single case is on this coarse grid (2 mm voxels): to a few percent.
    for outcome in [&both, &alone] {
        assert!(
            (outcome.mass - study.mass_target).abs() < 0.06 * study.mass_target,
            "mass {} for {}",
            outcome.mass,
            study.mass_target
        );
    }
    // A case that holds nothing is refused.
    study.load_cases = vec![LoadCase {
        fixtures: Vec::new(),
        loads: vec![0],
        ..LoadCase::default()
    }];
    assert_eq!(
        Grid::build(&study, 0, &cancel).unwrap_err(),
        GenerativeError::NotHeld
    );
}

#[test]
fn a_remote_mass_weighs_on_its_faces_as_a_force_at_its_point() {
    // 2 kg carried by the small pad's +X face (x = 60), its centre 10 mm beyond it, under
    // 10 g down: the nodes take 196.2 N down in all, and no moment about the mass's point
    // (the force acts there), so a moment about the face's centre.
    let mut study = bracket(1.0);
    study.loads.clear();
    study.gravity = vec![[0.0, -98.1, 0.0]];
    let point = [70.0, 10.0, 5.0];
    study.remote_masses = vec![RemoteMass {
        mass: 2.0,
        point,
        labels: vec![11],
    }];
    let cancel = AtomicBool::new(false);
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    let case = &grid.cases[0];
    assert_eq!(case.acceleration, [0.0, -98.1, 0.0]);
    let dims = grid.dims();
    let at = |n: u32| {
        let c = dims.node_coords(n as usize);
        std::array::from_fn::<f64, 3, _>(|a| grid.origin[a] + c[a] as f64 * grid.size)
    };
    let (mut total, mut moment) = ([0.0; 3], [0.0; 3]);
    for (n, f) in &case.forces {
        total = crate::math::add(total, *f);
        let r = crate::math::sub(at(*n), point);
        moment = crate::math::add(moment, crate::math::cross(r, *f));
    }
    assert!(
        (total[1] + 196.2).abs() < 1e-9 && total[0].abs() < 1e-9 && total[2].abs() < 1e-9,
        "{total:?}"
    );
    let size = crate::math::norm(moment);
    assert!(size < 1e-8 * 196.2 * 10.0, "{moment:?}");
    // About the face's centre (60, 10, 5) the weight turns it: 10 mm × 196.2 N about Z.
    let mut turn = [0.0; 3];
    for (n, f) in &case.forces {
        let r = crate::math::sub(at(*n), [60.0, 10.0, 5.0]);
        turn = crate::math::add(turn, crate::math::cross(r, *f));
    }
    assert!((turn[2] + 1962.0).abs() < 1e-6, "{turn:?}");

    // Nothing but weight, and no remote mass: still loaded.
    study.remote_masses.clear();
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    assert!(grid.cases[0].forces.is_empty());
    // A remote mass with no mass is refused.
    study.remote_masses = vec![RemoteMass {
        mass: 0.0,
        point,
        labels: vec![11],
    }];
    assert_eq!(
        Grid::build(&study, 0, &cancel),
        Err(GenerativeError::BadRemoteMass { index: 0 })
    );
}

#[test]
fn weight_goes_to_the_corners_and_fades_under_the_knee() {
    let dims = Dims { cells: [2, 1, 1] };
    let mut forces = vec![0.0; 3 * dims.node_count()];
    add_weight(dims, &[1.0, 3.0], [0.0, -9.81, 0.0], &mut forces);
    let total: f64 = forces.iter().skip(1).step_by(3).sum();
    assert!((total + 4.0 * 9.81).abs() < 1e-12);
    // A field the same at every corner works as its value against the acceleration.
    let field: Vec<f64> = (0..dims.node_count())
        .flat_map(|_| [0.5, -2.0, 1.0])
        .collect();
    let work = corner_work(dims, &field, [0.0, -9.81, 0.0]);
    assert!(work.iter().all(|w| (w - 19.62).abs() < 1e-12), "{work:?}");
    // The weight's share: the density above the knee, its cube scaled to meet it below.
    assert_eq!(weight_share(0.5), (0.5, 1.0));
    let (below, _) = weight_share(WEIGHT_KNEE - 1e-12);
    assert!((below - WEIGHT_KNEE).abs() < 1e-10);
    let (tiny, _) = weight_share(0.01);
    assert!((tiny - 1e-4).abs() < 1e-15);
    let h = 1e-7;
    let (_, slope) = weight_share(0.05);
    let fd = (weight_share(0.05 + h).0 - weight_share(0.05 - h).0) / (2.0 * h);
    assert!((slope - fd).abs() < 1e-6);
}

#[test]
fn a_bracket_carries_its_own_weight_and_a_remote_mass() {
    // No force: the bracket holds 1 kg off the small pad's +X face and its own weight,
    // under 50 g down. The stiffest bracket of the target mass still joins the pads.
    let mut study = bracket(2.0);
    study.loads.clear();
    study.gravity = vec![[0.0, -490.5, 0.0]];
    study.remote_masses = vec![RemoteMass {
        mass: 1.0,
        point: [66.0, 10.0, 5.0],
        labels: vec![11],
    }];
    let (grid, outcome) = run(&study, Method::Unrestricted, 0);
    assert!(outcome.compliance > 0.0);
    assert!(
        outcome.mass <= 1.06 * study.mass_target,
        "mass {} for {}",
        outcome.mass,
        study.mass_target
    );
    let loaded = voxels_in(&grid, [56.0, 6.0, 3.0], [60.0, 14.0, 7.0]);
    let held = voxels_in(&grid, [0.0, 0.0, 0.0], [4.0, 20.0, 10.0]);
    assert!(load_path(&grid, &outcome.density, &loaded, &held));
    // Minimizing mass under the same weight keeps the stress under the limit.
    study.objective = Objective::MinMass {
        safety_factor: 2.0,
        displacement_limit: None,
    };
    let (_, light) = run(&study, Method::Unrestricted, 0);
    assert!(
        light.max_von_mises <= 1.05 * 125.0,
        "{} MPa",
        light.max_von_mises
    );
}

#[test]
fn the_lightest_bracket_keeps_its_stress_under_the_limit() {
    // Steel (250 MPa yield) at a safety factor of 2: 125 MPa allowed.
    let mut study = bracket(2.0);
    let limit = |safety_factor, displacement_limit| Objective::MinMass {
        safety_factor,
        displacement_limit,
    };
    study.objective = limit(2.0, None);
    let (grid, light) = run(&study, Method::Unrestricted, 0);
    let space = grid.kinds.iter().filter(|k| **k != Voxel::Obstacle).count() as f64;
    let full = space * 8.0 * 7850e-9;
    assert!(
        light.max_von_mises <= 1.05 * 125.0,
        "{} MPa",
        light.max_von_mises
    );
    assert!(light.mass < 0.5 * full, "{} kg of {full}", light.mass);
    // A displacement limit tighter than the stress gives: heavier, and under it.
    study.objective = limit(2.0, Some(0.08));
    let (_, stiff) = run(&study, Method::Unrestricted, 0);
    assert!(light.max_displacement > 0.1);
    assert!(
        stiff.max_displacement <= 1.05 * 0.08,
        "{} mm",
        stiff.max_displacement
    );
    assert!(stiff.max_von_mises <= 1.05 * 125.0);
    assert!(stiff.mass > light.mass);
    // A safety factor of 4 (62.5 MPa) is more than even the full design space meets: the
    // design stays full, and its stress says so.
    study.objective = limit(4.0, None);
    let (_, heavy) = run(&study, Method::Unrestricted, 0);
    assert!(heavy.mass > 0.95 * full && heavy.max_von_mises > 62.5);
    // Without a strength there is nothing to keep the stress under.
    let cancel = AtomicBool::new(false);
    let weak = Elastic::new(200_000.0, 0.3, 0.0, 7850.0);
    let refused = generate(
        &grid,
        &study,
        Method::Unrestricted,
        &weak,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    );
    assert_eq!(refused, Err(GenerativeError::NoStrength));
}

#[test]
fn a_milled_bracket_has_no_undercut_seen_from_the_tool() {
    // Milled from +Z with a 6 mm tool: no voxel denser than the one under it.
    let study = bracket(2.0);
    let milled = Method::Milling {
        sides: Sides::single(Direction {
            axis: 2,
            positive: true,
        }),
        tool_diameter: Some(6.0),
    };
    let (grid, outcome) = run(&study, milled, 0);
    for v in 0..grid.voxel_count() {
        let c = grid.coords(v);
        if c[2] > 0 {
            let below = grid.index([c[0], c[1], c[2] - 1]);
            assert!(outcome.density[v] <= outcome.density[below], "{c:?}");
        }
    }
    // The tool's radius widens the filter past the default 1.5 voxels.
    assert!((outcome.filter_radius - 3.0).abs() < 1e-12);
    assert!((outcome.mass - study.mass_target).abs() < 0.06 * study.mass_target);
}

#[test]
fn a_bracket_milled_from_both_sides_has_no_undercut_seen_from_both() {
    // Milled from +Z and −Z at once, as Autodesk's example mills from +Y and −Y: along
    // every column across Z the material is one run, no void between two solid voxels
    // (each voxel is either reached from above, from below, or stays).
    let study = bracket(2.0);
    let up = Direction {
        axis: 2,
        positive: true,
    };
    let down = Direction {
        axis: 2,
        positive: false,
    };
    let milled = Method::Milling {
        sides: Sides::of(&[up, down]),
        tool_diameter: None,
    };
    assert_eq!(milled.to_string(), "3-axis milling from +Z and −Z");
    let (grid, outcome) = run(&study, milled, 0);
    let n = grid.counts;
    for j in 0..n[1] {
        for i in 0..n[0] {
            let column: Vec<u8> = (0..n[2])
                .map(|k| outcome.density[grid.index([i, j, k])])
                .collect();
            // Rising to its densest voxel, then falling: never down then up again.
            let peak = (0..column.len())
                .max_by_key(|&k| (column[k], std::cmp::Reverse(k)))
                .unwrap();
            assert!(
                column[..=peak].windows(2).all(|w| w[0] <= w[1])
                    && column[peak..].windows(2).all(|w| w[0] >= w[1]),
                "({i}, {j}): {column:?}"
            );
        }
    }
    assert!((outcome.mass - study.mass_target).abs() < 0.06 * study.mass_target);

    // From one side only across a symmetry plane: refused; from both, it keeps the mirror.
    let mut study = bracket(2.0);
    study.symmetry = Some(Symmetry {
        point: [0.0, 0.0, 5.0],
        planes: [false, false, true],
    });
    let cancel = AtomicBool::new(false);
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    let one = Method::Milling {
        sides: Sides::of(&[
            up,
            Direction {
                axis: 1,
                positive: true,
            },
        ]),
        tool_diameter: None,
    };
    let refused = generate(
        &grid,
        &study,
        one,
        &STEEL,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    );
    assert_eq!(refused, Err(GenerativeError::PullAcrossSymmetry));
}

#[test]
fn an_obstacle_is_reached_from_the_nearer_side() {
    // A block between the pads, an obstacle from z = 0 to 3: milled from +Z and −Z, the
    // tool reaches it from below (nothing in front of it there), so nothing above it is
    // made void; milled from +Z alone, the column above it is.
    let mut study = bracket(1.0);
    study.space.obstacles.push(Obstacle {
        surface: block(2, [20.0, 8.0, 0.0], [30.0, 12.0, 3.0]),
        offset: 0.0,
    });
    let cancel = AtomicBool::new(false);
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    let up = Direction {
        axis: 2,
        positive: true,
    };
    let down = Direction {
        axis: 2,
        positive: false,
    };
    let above = grid.index([25, 10, 6]);
    assert_eq!(grid.kinds[above], Voxel::Design);
    let both = states(
        &grid,
        Method::Milling {
            sides: Sides::of(&[up, down]),
            tool_diameter: None,
        },
        None,
        [false; 3],
    )
    .unwrap();
    assert_eq!(both[above], State::Free);
    let top = states(
        &grid,
        Method::Milling {
            sides: Sides::single(up),
            tool_diameter: None,
        },
        None,
        [false; 3],
    )
    .unwrap();
    assert_eq!(top[above], State::Void);
}

#[test]
fn a_printed_bracket_holds_itself_up() {
    // Printed up +Y from a plate at y = 0: the arm reaching out to the loaded pad would
    // overhang, unrestricted.
    let study = bracket(1.5);
    let up = Method::Additive {
        direction: Direction {
            axis: 1,
            positive: true,
        },
    };
    let (grid, outcome) = run(&study, up, 0);
    assert!(
        (outcome.mass - study.mass_target).abs() < 0.05 * study.mass_target,
        "mass {} for {}",
        outcome.mass,
        study.mass_target
    );
    // Every voxel of the material above the plate stands on the one under it or one of
    // that one's four neighbours across the layer: nothing overhangs more than 45°, along
    // the diagonals of the layer too.
    let n = grid.counts;
    let (mut material, mut hanging) = (0, 0);
    for v in 0..grid.voxel_count() {
        let c = grid.coords(v);
        if c[1] == 0 || outcome.density[v] < 128 || grid.kinds[v] == Voxel::Preserve {
            continue;
        }
        material += 1;
        let held = (-1i64..=1).any(|dz| {
            (-1i64..=1).any(|dx| {
                let (x, z) = (c[0] as i64 + dx, c[2] as i64 + dz);
                (dx == 0 || dz == 0)
                    && x >= 0
                    && z >= 0
                    && x < n[0] as i64
                    && z < n[2] as i64
                    && outcome.density[grid.index([x as usize, c[1] - 1, z as usize])] >= 128
            })
        });
        if !held {
            hanging += 1;
        }
    }
    assert!(material > 0);
    assert!(
        hanging * 100 <= material,
        "{hanging} of {material} voxels hang"
    );
}

#[test]
fn a_bracket_grows_a_crisp_load_path_at_its_mass_target() {
    let study = bracket(1.25);
    let (grid, outcome) = run(&study, Method::Unrestricted, 0);
    let last = outcome.history.last().unwrap();
    let first = outcome.history[0];
    assert!(
        (outcome.mass - study.mass_target).abs() < 0.02 * study.mass_target,
        "mass {} for {}",
        outcome.mass,
        study.mass_target
    );
    assert!(last.compliance < first.compliance, "{first:?} {last:?}");
    // The loaded pad is joined to the held one.
    let loaded = voxels_in(&grid, [56.0, 6.0, 3.0], [60.0, 14.0, 7.0]);
    let held = voxels_in(&grid, [0.0, 0.0, 0.0], [4.0, 20.0, 10.0]);
    assert!(load_path(&grid, &outcome.density, &loaded, &held));
    assert!(outcome.max_displacement > 0.0 && outcome.max_von_mises > 0.0);
    assert!(outcome.min_factor_of_safety.is_some());
    // Settled crisp: few voxels neither solid nor void.
    assert!(
        outcome.grayness < GRAYNESS_SETTLED,
        "grayness {}",
        outcome.grayness
    );
    assert_eq!(
        outcome.snapshots.len(),
        outcome.history.len() / SNAPSHOT_EVERY
    );
    assert!(outcome.filter_radius >= 1.5 * 1.25);
    // Preserve voxels stay solid.
    for v in 0..grid.voxel_count() {
        if grid.kinds[v] == Voxel::Preserve {
            assert_eq!(outcome.density[v], 255);
        }
    }

    // Solid or void, not grey: the voxels over half hold the material the mass says (grey
    // material would weigh without being over half, or be over half without weighing).
    let solid = outcome.density.iter().filter(|&&d| d >= 128).count() as f64 * grid.size.powi(3);
    let material = outcome.mass / (STEEL.density * 1e-9);
    assert!(
        (solid - material).abs() < 0.05 * material,
        "{solid} {material}"
    );
    // Its surface is closed and holds most of them: members two or three voxels thick lose
    // their edges to the cut's bevels.
    let shape = surface(&grid, &outcome.density, 8);
    assert!(is_closed(&shape));
    assert!(
        shape.volume() < solid && shape.volume() > 0.75 * solid,
        "{} {}",
        shape.volume(),
        solid
    );
}

#[test]
fn the_same_outcome_on_any_number_of_threads() {
    // Large enough for the products to run on several threads.
    let mut study = bracket(1.0);
    study.max_iterations = 25;
    let (_, one) = run(&study, Method::Unrestricted, 1);
    let (_, many) = run(&study, Method::Unrestricted, 8);
    assert_eq!(one, many);
}

#[test]
fn manufacturing_methods_shape_the_outcome() {
    let mut study = bracket(1.25);
    study.max_iterations = 60;

    // Mirrored about the bracket's middle plane across Z.
    study.symmetry = Some(Symmetry {
        point: [0.0, 0.0, 5.0],
        planes: [false, false, true],
    });
    let (grid, outcome) = run(&study, Method::Unrestricted, 0);
    for v in 0..grid.voxel_count() {
        let mut c = grid.coords(v);
        c[2] = grid.counts[2] - 1 - c[2];
        assert_eq!(outcome.density[v], outcome.density[grid.index(c)]);
    }

    // 2-axis cutting along Z: every column the same.
    study.symmetry = None;
    let (grid, outcome) = run(&study, Method::Cutting { axis: 2 }, 0);
    for v in 0..grid.voxel_count() {
        let mut c = grid.coords(v);
        c[2] = 0;
        assert_eq!(outcome.density[v], outcome.density[grid.index(c)], "{v}");
    }

    // Cast, pulled towards +Z: no voxel denser than the one under it; and both ways from
    // the middle plane: none denser than the one nearer the plane.
    let up = Method::Casting {
        direction: Direction {
            axis: 2,
            positive: true,
        },
        parting: None,
    };
    let (grid, outcome) = run(&study, up, 0);
    for v in 0..grid.voxel_count() {
        let c = grid.coords(v);
        if c[2] > 0 {
            let below = grid.index([c[0], c[1], c[2] - 1]);
            assert!(outcome.density[v] <= outcome.density[below], "{c:?}");
        }
    }
    let both = Method::Casting {
        direction: Direction {
            axis: 2,
            positive: true,
        },
        parting: Some([0.0, 0.0, 5.0]),
    };
    let (grid, outcome) = run(&study, both, 0);
    for v in 0..grid.voxel_count() {
        let c = grid.coords(v);
        let z = grid.centre(v)[2];
        let inner = if z > 5.0 && c[2] > 0 {
            Some(grid.index([c[0], c[1], c[2] - 1]))
        } else if z < 5.0 && c[2] + 1 < grid.counts[2] {
            Some(grid.index([c[0], c[1], c[2] + 1]))
        } else {
            None
        };
        if let Some(w) = inner
            && (grid.centre(w)[2] - 5.0).abs() < (z - 5.0).abs()
        {
            assert!(outcome.density[v] <= outcome.density[w], "{c:?}");
        }
    }
}

#[test]
fn grayness_counts_the_design_voxels_neither_solid_nor_void() {
    let state = [State::Free, State::Free, State::Solid, State::Void];
    assert_eq!(grayness(&[0.0, 1.0, 1.0, 0.0], &state), 0.0);
    assert!((grayness(&[0.5, 0.5, 1.0, 0.0], &state) - 1.0).abs() < 1e-12);
    // Half the design voxels at a half: half as grey. Preserve and void voxels left out.
    assert!((grayness(&[0.5, 1.0, 0.3, 0.7], &state) - 0.5).abs() < 1e-12);
    assert_eq!(grayness(&[1.0, 0.0], &[State::Solid, State::Void]), 0.0);
}

/// Every edge of a closed, consistently wound surface is used once each way.
fn is_closed(shape: &crate::topology::Shape) -> bool {
    let mut directed: HashMap<(u32, u32), u32> = HashMap::new();
    for t in &shape.triangles {
        for k in 0..3 {
            *directed.entry((t[k], t[(k + 1) % 3])).or_insert(0) += 1;
        }
    }
    !shape.triangles.is_empty()
        && directed
            .iter()
            .all(|(&(a, b), &count)| count == 1 && directed.get(&(b, a)) == Some(&1))
}

#[test]
fn surfaces_are_closed_and_hold_the_material() {
    // A 6 × 6 × 6 block of solid voxels in a 10 × 10 × 10 grid, and one touching the
    // grid's side.
    let grid = Grid::from_parts([0.0; 3], 2.0, [10, 10, 10], vec![Voxel::Design; 1000]).unwrap();
    for lo in [[2usize, 2, 2], [0, 2, 2]] {
        let mut density = vec![0u8; 1000];
        for k in lo[2]..lo[2] + 6 {
            for j in lo[1]..lo[1] + 6 {
                for i in lo[0]..lo[0] + 6 {
                    density[grid.index([i, j, k])] = 255;
                }
            }
        }
        let raw = surface(&grid, &density, 0);
        assert!(is_closed(&raw));
        // Cut at the voxels' faces, the edges and corners bevelled by up to a voxel.
        let cube = 12.0f64.powi(3);
        assert!(
            raw.volume() < cube && raw.volume() > 0.8 * cube,
            "{}",
            raw.volume()
        );
        let smooth = surface(&grid, &density, 10);
        assert!(is_closed(&smooth));
        assert!((smooth.volume() - raw.volume()).abs() < 0.1 * cube);
    }
    // Nothing: no surface; a grid it does not fit: none either.
    assert!(surface(&grid, &[0u8; 1000], 4).triangles.is_empty());
    assert!(surface(&grid, &[0u8; 10], 4).triangles.is_empty());
}

#[test]
fn an_exact_surface_meets_the_bodies_on_their_faces() {
    // 1 mm voxels; the starting shape a box from 0.3 to 8.7 along X and 0.3 to 6.7 across,
    // its faces between the voxels' centres; a preserve box at its −X end to x = 3.4; a bar
    // of material from the preserve box to x = 6, empty design space beyond.
    let (start_lo, start_hi) = ([0.3; 3], [8.7, 6.7, 6.7]);
    let (keep_lo, keep_hi) = ([0.3; 3], [3.4, 6.7, 6.7]);
    let counts = [10, 8, 8];
    let blank = Grid::from_parts([0.0; 3], 1.0, counts, vec![Voxel::Design; 640]).unwrap();
    let within = |c: V3, lo: V3, hi: V3| (0..3).all(|a| c[a] > lo[a] && c[a] < hi[a]);
    let mut kinds = vec![Voxel::Design; 640];
    let mut density = vec![0u8; 640];
    for v in 0..640 {
        let c = blank.centre(v);
        if within(c, keep_lo, keep_hi) {
            kinds[v] = Voxel::Preserve;
            density[v] = 255;
        } else if !within(c, start_lo, start_hi) {
            kinds[v] = Voxel::Outside;
        } else if c[0] < 6.0 && (2.0..6.0).contains(&c[1]) && (2.0..6.0).contains(&c[2]) {
            density[v] = 255;
        }
    }
    let grid = Grid::from_parts([0.0; 3], 1.0, counts, kinds).unwrap();
    let space = DesignSpace {
        preserve: vec![box_surface(keep_lo, keep_hi)],
        obstacles: Vec::new(),
        starting_shape: Some(box_surface(start_lo, start_hi)),
        margin: None,
    };
    let plain = surface(&grid, &density, 10);
    let (exact, held) = exact_surface(&grid, &density, 10, &space);
    assert!(is_closed(&exact));
    assert_eq!(held.len(), exact.positions.len());
    let lowest = |s: &crate::topology::Shape, a: usize| {
        s.positions
            .iter()
            .map(|p| p[a])
            .fold(f64::INFINITY, f64::min)
    };
    let highest = |s: &crate::topology::Shape, a: usize| {
        s.positions
            .iter()
            .map(|p| p[a])
            .fold(f64::NEG_INFINITY, f64::max)
    };
    // Cut by the densities, the outcome stops midway between centres, at 0; cut exactly, it
    // is as big as the boxes, through the smoothing.
    assert!(lowest(&plain, 0).abs() < 0.1, "{}", lowest(&plain, 0));
    for a in 0..3 {
        assert_eq!(lowest(&exact, a), 0.3);
    }
    assert_eq!(highest(&exact, 1), 6.7);
    assert_eq!(highest(&exact, 2), 6.7);
    // The preserve box's face to the empty space beside the bar is met where it is; the
    // bar's free end, by the densities.
    let on = |x: f64| exact.positions.iter().filter(|p| p[0] == x).count();
    assert!(on(3.4) > 10, "{}", on(3.4));
    assert!(highest(&exact, 0) < 6.5);
    // Nothing of it is out of the starting shape.
    let inside = |s: &crate::topology::Shape| {
        s.positions
            .iter()
            .all(|p| (0..3).all(|a| p[a] >= start_lo[a] && p[a] <= start_hi[a]))
    };
    assert!(inside(&exact));
    // Simplified, holding the points on the faces: still on them, still inside.
    let fewer = decimate_holding(&exact, &held, exact.triangles.len() / 8, f64::INFINITY);
    assert!(is_closed(&fewer));
    assert!(fewer.triangles.len() <= exact.triangles.len() / 8);
    for a in 0..3 {
        assert_eq!(lowest(&fewer, a), 0.3);
    }
    assert_eq!(highest(&fewer, 1), 6.7);
    assert!(inside(&fewer));
}
