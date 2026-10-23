//! Flow and cooling studies run through a real OpenFOAM (OpenCFD's v2606), against closed
//! forms and correlations.
//!
//! Each test is skipped, with a note, when no OpenFOAM is installed. Where it is not,
//! OpenCFD's image runs them (docs/CFD.md, *Testing*): build the test binary with
//! `cargo test -p oxyde-flow --test openfoam --no-run`, then run it inside
//! `opencfd/openfoam-run:2606` with `--test-threads=1` (each run uses every core it is
//! given and a few hundred megabytes). Cases are written under `OXYDE_FLOW_TEST_DIR` when
//! it is set, and kept there; under the system's temporary folder otherwise, and removed.

use std::f64::consts::PI;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use oxyde_flow::trajectory::{self, End, Seeding};
use oxyde_flow::{
    Condition, ConditionKind, Domain, FlowResults, FlowStudy, FlowType, Fluid, Goal, GoalKind,
    GoalValue, Heat, HeatLoad, HeatLoadKind, Installation, MeshSettings, Solid, SolverSettings,
    Surface, Turbulence, drag, run, write_case,
};

fn openfoam() -> Option<Installation> {
    match Installation::find() {
        Ok(found) => {
            eprintln!("OpenFOAM {} in {}", found.version(), found.root().display());
            Some(found)
        }
        Err(e) => {
            eprintln!("skipped: {e}");
            None
        }
    }
}

/// A folder for a test's case, and whether to remove it afterwards.
fn case_dir(name: &str) -> (PathBuf, bool) {
    match std::env::var_os("OXYDE_FLOW_TEST_DIR") {
        Some(dir) => (PathBuf::from(dir).join(name), false),
        None => (
            std::env::temp_dir().join(format!("oxyde-flow-{name}-{}", std::process::id())),
            true,
        ),
    }
}

/// A closed cylinder along z from 0 to `length` (mm): its side is label 0, the disc at
/// z = 0 label 1, the disc at z = `length` label 2. Normals point out.
fn cylinder(radius: f64, length: f64, segments: u32, rings: u32) -> Surface {
    let mut s = Surface::default();
    let angle = |j: u32| 2.0 * PI * f64::from(j % segments) / f64::from(segments);
    for i in 0..=rings {
        let z = length * f64::from(i) / f64::from(rings);
        for j in 0..segments {
            let a = angle(j);
            s.positions.push([radius * a.cos(), radius * a.sin(), z]);
        }
    }
    let at = |i: u32, j: u32| i * segments + j % segments;
    for i in 0..rings {
        for j in 0..segments {
            let (a, b, c, d) = (at(i, j), at(i, j + 1), at(i + 1, j + 1), at(i + 1, j));
            s.triangles.extend([[a, b, c], [a, c, d]]);
            s.labels.extend([0, 0]);
        }
    }
    for (ring, z, label) in [(0, 0.0, 1), (rings, length, 2)] {
        let centre = s.positions.len() as u32;
        s.positions.push([0.0, 0.0, z]);
        for j in 0..segments {
            let (p, q) = (at(ring, j), at(ring, j + 1));
            // Seen from outside: clockwise about +z at the bottom, counter-clockwise on top.
            s.triangles.push(if label == 1 {
                [centre, q, p]
            } else {
                [centre, p, q]
            });
            s.labels.push(label);
        }
    }
    s
}

/// A sphere of `radius` (mm) around `centre`, label 0: an icosahedron split `levels` times.
fn sphere(radius: f64, centre: [f64; 3], levels: u32) -> Surface {
    let t = (1.0 + 5f64.sqrt()) / 2.0;
    let mut points: Vec<[f64; 3]> = vec![
        [-1.0, t, 0.0],
        [1.0, t, 0.0],
        [-1.0, -t, 0.0],
        [1.0, -t, 0.0],
        [0.0, -1.0, t],
        [0.0, 1.0, t],
        [0.0, -1.0, -t],
        [0.0, 1.0, -t],
        [t, 0.0, -1.0],
        [t, 0.0, 1.0],
        [-t, 0.0, -1.0],
        [-t, 0.0, 1.0],
    ];
    let mut faces: Vec<[u32; 3]> = vec![
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    let unit = |p: [f64; 3]| {
        let n = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        [p[0] / n, p[1] / n, p[2] / n]
    };
    for p in &mut points {
        *p = unit(*p);
    }
    for _ in 0..levels {
        let mut midpoints = std::collections::HashMap::new();
        let mut split = Vec::new();
        for [a, b, c] in faces {
            let mut middle = |i: u32, j: u32| {
                *midpoints.entry((i.min(j), i.max(j))).or_insert_with(|| {
                    let (p, q) = (points[i as usize], points[j as usize]);
                    points.push(unit([p[0] + q[0], p[1] + q[1], p[2] + q[2]]));
                    points.len() as u32 - 1
                })
            };
            let (ab, bc, ca) = (middle(a, b), middle(b, c), middle(c, a));
            split.extend([[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]);
        }
        faces = split;
    }
    Surface {
        positions: points
            .iter()
            .map(|p| {
                [
                    centre[0] + radius * p[0],
                    centre[1] + radius * p[1],
                    centre[2] + radius * p[2],
                ]
            })
            .collect(),
        labels: vec![0; faces.len()],
        triangles: faces,
    }
}

/// Runs a study and reads its results, printing the run's progress now and then.
fn solve(
    install: &Installation,
    name: &str,
    study: &FlowStudy,
) -> (oxyde_flow::Case, oxyde_flow::RunReport, FlowResults, bool) {
    let (dir, remove) = case_dir(name);
    let case = write_case(&dir, study).expect("the case is written");
    let cancel = AtomicBool::new(false);
    let mut last_stage = None;
    let report = run(&case, install, &cancel, &mut |p| {
        if last_stage != Some(p.stage) {
            eprintln!("{name}: {}", p.stage.describe());
            last_stage = Some(p.stage);
        }
        if p.iteration > 0 && p.iteration % 100 == 0 && !p.residuals.is_empty() {
            eprintln!("{name}: iteration {} {:?}", p.iteration, p.residuals);
        }
    })
    .unwrap_or_else(|e| panic!("{name} ran: {e}"));
    eprintln!(
        "{name}: {} cells, mesh ok {:?}, {} iterations, converged {}",
        report.cells.unwrap_or(0),
        report.mesh_ok,
        report.iterations,
        report.converged
    );
    let results = FlowResults::read(&case).expect("the results are read");
    (case, report, results, remove)
}

/// Laminar flow in a pipe (Hagen–Poiseuille): water through a 10 mm bore, 100 mm long,
/// at Re = 20. Past the entrance the pressure falls by 8 μ U / R² per metre, and the
/// velocity on the axis is twice the mean.
#[test]
fn laminar_pipe_flow_matches_hagen_poiseuille() {
    let Some(install) = openfoam() else { return };
    laminar_pipe(&install, "pipe", 1);
}

/// The same pipe, the mesh split between two processes: `decomposePar`, the solvers under
/// MPI, `reconstructPar`.
#[test]
fn laminar_pipe_flow_on_two_processes() {
    let Some(install) = openfoam() else { return };
    laminar_pipe(&install, "pipe-parallel", 2);
}

fn laminar_pipe(install: &Installation, name: &str, processes: u32) {
    let (radius, length) = (5.0, 100.0);
    let fluid = Fluid::water();
    let mean = 0.002; // m/s
    let area = PI * (radius * 1e-3) * (radius * 1e-3);
    let rate = mean * area;
    let surface = cylinder(radius, length, 64, 20);
    let bounds = surface.bounds().unwrap();
    let study = FlowStudy {
        surfaces: vec![surface],
        flow_type: FlowType::Internal {
            point: [0.0, 0.0, length / 2.0],
        },
        domain: Domain::internal(bounds, 2.0),
        fluid: fluid.clone(),
        turbulence: Turbulence::Laminar,
        conditions: vec![
            Condition {
                kind: ConditionKind::InletVolumeFlow { rate },
                labels: vec![1],
            },
            Condition {
                kind: ConditionKind::OutletPressure { pressure: 0.0 },
                labels: vec![2],
            },
        ],
        goals: vec![
            Goal {
                name: "Into the inlet".to_string(),
                kind: GoalKind::VolumeFlow,
                labels: vec![1],
            },
            Goal {
                name: "Out of the outlet".to_string(),
                kind: GoalKind::VolumeFlow,
                labels: vec![2],
            },
            Goal {
                name: "Inlet pressure".to_string(),
                kind: GoalKind::MeanPressure,
                labels: vec![1],
            },
            Goal {
                name: "Wall".to_string(),
                kind: GoalKind::Force,
                labels: vec![0],
            },
        ],
        mesh: MeshSettings::new(1.0),
        solver: SolverSettings {
            iterations: 2000,
            residual: 1e-5,
            processes,
        },
        heat: None,
    };
    let (case, report, results, remove) = solve(install, name, &study);
    assert!(report.converged, "the flow converged");

    // Mass is conserved: what comes in leaves.
    let goals = results.goals(&case);
    let scalar = |i: usize| match &goals[i] {
        GoalValue::Scalar { value, .. } => *value,
        other => panic!("{other:?}"),
    };
    let (inflow, outflow) = (scalar(0), scalar(1));
    eprintln!("flow in {inflow:e} m³/s, out {outflow:e} m³/s, set {rate:e}");
    assert!(
        (inflow + rate).abs() < 1e-3 * rate,
        "{inflow} in, {rate} set"
    );
    assert!((outflow - rate).abs() < 1e-3 * rate, "{outflow} out");

    // The pressure gradient past the entrance: a least-squares line through the cells'
    // pressures from z = 30 mm to 85 mm.
    let (mut n, mut sz, mut sp, mut szz, mut szp) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (c, centre) in results.cell_centres.iter().enumerate() {
        let z = centre[2] * 1e-3;
        if !(0.030..0.085).contains(&z) {
            continue;
        }
        let w = results.cell_volumes[c];
        let p = results.pressure[c];
        n += w;
        sz += w * z;
        sp += w * p;
        szz += w * z * z;
        szp += w * z * p;
    }
    let gradient = (n * szp - sz * sp) / (n * szz - sz * sz);
    let expected = -8.0 * fluid.viscosity * mean / (radius * 1e-3).powi(2);
    let error = gradient / expected - 1.0;
    eprintln!(
        "pressure gradient {gradient:.5} Pa/m, Hagen–Poiseuille {expected:.5}, {:+.2} %",
        100.0 * error
    );
    assert!(
        error.abs() < 0.05,
        "gradient {gradient}, expected {expected}"
    );

    // The velocity on the axis, past the entrance: twice the mean.
    let axis = results
        .cell_centres
        .iter()
        .zip(&results.velocity)
        .filter(|(c, _)| (0.03e3..0.085e3).contains(&c[2]) && c[0].hypot(c[1]) < 1.0)
        .map(|(_, u)| u[2])
        .fold(0.0, f64::max);
    eprintln!(
        "velocity on the axis {axis:.6} m/s, twice the mean {:.6}",
        2.0 * mean
    );
    assert!((axis / (2.0 * mean) - 1.0).abs() < 0.05, "axis {axis}");

    // The fluid drags the wall along the flow (+z), by about the pressure drop over the
    // bore's area.
    match &goals[3] {
        GoalValue::Force { total, viscous, .. } => {
            let drop = scalar(2);
            eprintln!(
                "wall force {total:?} N, friction {viscous:?} N, inlet pressure {drop:.5} Pa"
            );
            assert!(viscous[2] > 0.0);
            assert!((total[2] / (drop * area) - 1.0).abs() < 0.25, "{total:?}");
        }
        other => panic!("{other:?}"),
    }
    if remove {
        let _ = std::fs::remove_dir_all(&case.dir);
    }
}

/// External flow past a sphere at Re = 20 (air): the drag coefficient of Clift, Grace and
/// Weber's correlation, Cd = 24/Re (1 + 0.1935 Re^0.6305) = 2.73. The domain reaches five
/// diameters round the sphere and ten behind it, which still confines the flow a little.
/// The same Cd comes from the drag module on the mesh's silhouette of the sphere, and the
/// flow's trajectories from upstream pass round it.
#[test]
fn sphere_drag_at_re_20() {
    let Some(install) = openfoam() else { return };
    let diameter = 10.0; // mm
    let fluid = Fluid::air();
    let reynolds = 20.0;
    let speed = reynolds * fluid.kinematic_viscosity() / (diameter * 1e-3);
    let surface = sphere(diameter / 2.0, [0.0; 3], 3);
    let d = diameter;
    let study = FlowStudy {
        surfaces: vec![surface],
        flow_type: FlowType::External {
            velocity: [speed, 0.0, 0.0],
        },
        domain: Domain {
            min: [-5.0 * d, -5.0 * d, -5.0 * d],
            max: [10.0 * d, 5.0 * d, 5.0 * d],
        },
        fluid: fluid.clone(),
        turbulence: Turbulence::Laminar,
        conditions: vec![],
        goals: vec![Goal {
            name: "Drag".to_string(),
            kind: GoalKind::Force,
            labels: vec![0],
        }],
        mesh: MeshSettings {
            cell_size: 5.0,
            surface_level: 3,
            feature_level: 3,
            layers: 0,
            max_cells: 1_000_000,
        },
        solver: SolverSettings {
            iterations: 1500,
            residual: 1e-5,
            processes: 1,
        },
        heat: None,
    };
    let (case, report, results, remove) = solve(&install, "sphere", &study);
    assert!(report.converged, "the flow converged");
    let goals = results.goals(&case);
    let GoalValue::Force { total, .. } = &goals[0] else {
        panic!("{:?}", goals[0]);
    };
    let frontal = PI * (d * 1e-3 / 2.0).powi(2);
    let cd = total[0] / (0.5 * fluid.density * speed * speed * frontal);
    let expected = 24.0 / reynolds * (1.0 + 0.1935 * reynolds.powf(0.6305));
    eprintln!(
        "drag {total:?} N, Cd {cd:.3}, Clift {expected:.3}, {:+.1} %",
        100.0 * (cd / expected - 1.0)
    );
    assert!((cd / expected - 1.0).abs() < 0.15, "Cd {cd}");
    // No sideways force on a symmetric body.
    assert!(total[1].abs() < 0.05 * total[0] && total[2].abs() < 0.05 * total[0]);

    // The application's Cd: on the silhouette of the mesh's faces on the sphere.
    let velocity = [speed, 0.0, 0.0];
    let area = drag::frontal_area(&results, velocity).expect("the sphere has a silhouette");
    let disc = PI * (d / 2.0).powi(2);
    let cd_shown = drag::drag_coefficient(*total, velocity, fluid.density, area).unwrap();
    eprintln!(
        "frontal area {area:.2} mm², πr² {disc:.2} mm² ({:+.2} %); Cd shown {cd_shown:.3}",
        100.0 * (area / disc - 1.0)
    );
    assert!((area / disc - 1.0).abs() < 0.03, "frontal area {area} mm²");
    assert!((cd_shown / expected - 1.0).abs() < 0.15, "Cd {cd_shown}");
    assert!((cd_shown * area / (cd * disc) - 1.0).abs() < 1e-9);

    // Trajectories from a plane upstream: round the sphere and out of the domain behind.
    let started = std::time::Instant::now();
    let paths = trajectory::trajectories(&results, Seeding::Upstream { velocity }, 100);
    let count = |end: End| paths.iter().filter(|p| p.end == end).count();
    eprintln!(
        "{} trajectories in {:.2} s: {} left, {} on the wall, {} stalled, {} too long, {} lost",
        paths.len(),
        started.elapsed().as_secs_f64(),
        count(End::Left),
        count(End::Wall),
        count(End::Stalled),
        count(End::Length),
        count(End::Lost)
    );
    assert!(paths.len() >= 90, "{} trajectories", paths.len());
    assert!(count(End::Left) * 10 >= paths.len() * 8);
    assert!(count(End::Lost) * 20 <= paths.len());
    let radius = |p: [f64; 3]| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
    let finest = study.mesh.finest();
    let mut round = 0;
    for path in &paths {
        let seed = path.points[0];
        // Upstream, half a diameter before the sphere.
        assert!((seed[0] + d).abs() < 0.5, "{seed:?}");
        for (p, s) in path.points.iter().zip(&path.speeds) {
            // The mesh's faces lie within a fraction of a cell of the sphere.
            assert!(
                radius(*p) > d / 2.0 - 0.5 * finest,
                "{p:?} inside the sphere"
            );
            assert!(s.is_finite() && *s < 2.0 * speed, "{s} m/s");
        }
        assert!(path.times.windows(2).all(|w| w[1] >= w[0]));
        // Seeded in front of the sphere, carried round it to behind.
        let off_axis = (seed[1] * seed[1] + seed[2] * seed[2]).sqrt();
        if off_axis < d / 2.0 && path.end == End::Left && path.points.last().unwrap()[0] > d {
            round += 1;
        }
    }
    let in_front = paths
        .iter()
        .filter(|p| (p.points[0][1].powi(2) + p.points[0][2].powi(2)).sqrt() < d / 2.0)
        .count();
    eprintln!("{round} of the {in_front} seeded in front of the sphere went round it");
    assert!(in_front > 0 && round * 2 >= in_front);
    if remove {
        let _ = std::fs::remove_dir_all(&case.dir);
    }
}

/// Turbulent flow (k-ω SST, Re = 20 000) through the same pipe: the run converges and the
/// pressure drop is of the order Blasius gives for a developed flow,
/// f = 0.316 Re^-¼ over L/D = 10, the entrance adding to it.
#[test]
fn turbulent_pipe_flow_runs() {
    let Some(install) = openfoam() else { return };
    let (radius, length) = (5.0, 100.0);
    let fluid = Fluid::water();
    let mean = 2.0; // m/s
    let area = PI * (radius * 1e-3) * (radius * 1e-3);
    let surface = cylinder(radius, length, 64, 20);
    let bounds = surface.bounds().unwrap();
    let mut mesh = MeshSettings::new(1.0);
    mesh.layers = 3;
    let study = FlowStudy {
        surfaces: vec![surface],
        flow_type: FlowType::Internal {
            point: [0.0, 0.0, length / 2.0],
        },
        domain: Domain::internal(bounds, 2.0),
        fluid: fluid.clone(),
        turbulence: Turbulence::KOmegaSst {
            intensity: 0.05,
            length: 0.7,
        },
        conditions: vec![
            Condition {
                kind: ConditionKind::InletVelocity { speed: mean },
                labels: vec![1],
            },
            Condition {
                kind: ConditionKind::OutletPressure { pressure: 0.0 },
                labels: vec![2],
            },
        ],
        goals: vec![Goal {
            name: "Inlet pressure".to_string(),
            kind: GoalKind::MeanPressure,
            labels: vec![1],
        }],
        mesh,
        solver: SolverSettings {
            iterations: 800,
            residual: 1e-4,
            processes: 1,
        },
        heat: None,
    };
    let (case, report, results, remove) = solve(&install, "turbulent", &study);
    let goals = results.goals(&case);
    let GoalValue::Scalar { value: drop, .. } = goals[0] else {
        panic!("{:?}", goals[0]);
    };
    let reynolds = mean * 2.0 * radius * 1e-3 / fluid.kinematic_viscosity();
    let blasius = 0.316
        * reynolds.powf(-0.25)
        * (length / (2.0 * radius))
        * 0.5
        * fluid.density
        * mean
        * mean;
    eprintln!(
        "Re {reynolds:.0}, converged {}, inlet pressure {drop:.1} Pa, Blasius developed {blasius:.1} Pa, ratio {:.2}",
        report.converged,
        drop / blasius
    );
    assert!(drop > 0.5 * blasius && drop < 3.0 * blasius, "{drop} Pa");
    let inflow = results.volume_flow(&[1]);
    assert!(
        (inflow + mean * area).abs() < 0.02 * mean * area,
        "{inflow}"
    );
    if remove {
        let _ = std::fs::remove_dir_all(&case.dir);
    }
}

/// A box from `lo` to `hi` (mm), its sides labelled `first`… in the order −x, +x, −y, +y,
/// −z, +z. Normals point out.
fn cuboid(lo: [f64; 3], hi: [f64; 3], first: u32) -> Surface {
    let corner = |i: u32| {
        [
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        ]
    };
    let mut s = Surface {
        positions: (0..8).map(corner).collect(),
        ..Surface::default()
    };
    for (side, [a, b, c, d]) in [
        [0, 4, 6, 2],
        [1, 3, 7, 5],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 2, 3, 1],
        [4, 5, 7, 6],
    ]
    .into_iter()
    .enumerate()
    {
        s.triangles.extend([[a, b, c], [a, c, d]]);
        s.labels.extend([first + side as u32; 2]);
    }
    s
}

fn scalar_goal(goals: &[GoalValue], i: usize) -> f64 {
    match &goals[i] {
        GoalValue::Scalar { value, .. } => *value,
        other => panic!("{other:?}"),
    }
}

/// A cold plate: water at 0.1 m/s through a 4 mm square channel along an aluminium plate
/// (four bars round the channel), closed by plastic lids, with a 10 W ceramic chip on top.
/// At steady state the water carries the chip's heat away: m cp ΔT at the outlet, and what
/// leaves the channel's walls, are 10 W. Two processes: every region decomposed and put back.
#[test]
fn a_cold_plate_carries_its_chip_heat_away() {
    let Some(install) = openfoam() else { return };
    let aluminium = |name: &str| Solid {
        name: name.to_string(),
        conductivity: 200.0,
        density: 2700.0,
    };
    let surfaces = vec![
        cuboid([-20.0, 0.0, -10.0], [20.0, 3.0, 10.0], 0),
        cuboid([-20.0, 7.0, -10.0], [20.0, 10.0, 10.0], 6),
        cuboid([-20.0, 3.0, -10.0], [20.0, 7.0, -2.0], 12),
        cuboid([-20.0, 3.0, 2.0], [20.0, 7.0, 10.0], 18),
        cuboid([-21.0, 3.0, -2.0], [-20.0, 7.0, 2.0], 24),
        cuboid([20.0, 3.0, -2.0], [21.0, 7.0, 2.0], 30),
        cuboid([-5.0, 10.0, -5.0], [5.0, 12.0, 5.0], 36),
    ];
    let lid = |name: &str| Solid {
        name: name.to_string(),
        conductivity: 0.2,
        density: 1200.0,
    };
    let solids = vec![
        aluminium("Bottom"),
        aluminium("Top"),
        aluminium("Front"),
        aluminium("Back"),
        lid("Inlet lid"),
        lid("Outlet lid"),
        Solid {
            name: "Chip".to_string(),
            conductivity: 20.0,
            density: 3000.0,
        },
    ];
    let (inlet, outlet, chip_top) = (25, 30, 39);
    let channel = vec![3, 8, 17, 22];
    let speed = 0.1;
    let mut study = FlowStudy {
        surfaces,
        flow_type: FlowType::Internal {
            point: [0.3, 5.3, 0.3],
        },
        domain: Domain::internal(([-21.0, 0.0, -10.0], [21.0, 12.0, 10.0]), 2.0),
        fluid: Fluid::water(),
        turbulence: Turbulence::Laminar,
        conditions: vec![
            Condition {
                kind: ConditionKind::InletVelocity { speed },
                labels: vec![inlet],
            },
            Condition {
                kind: ConditionKind::OutletPressure { pressure: 0.0 },
                labels: vec![outlet],
            },
        ],
        goals: vec![
            Goal {
                name: "Into the water".to_string(),
                kind: GoalKind::HeatFlow,
                labels: channel.clone(),
            },
            Goal {
                name: "Chip".to_string(),
                kind: GoalKind::MaxTemperature,
                labels: vec![chip_top],
            },
            Goal {
                name: "Out".to_string(),
                kind: GoalKind::MassFlow,
                labels: vec![outlet],
            },
        ],
        mesh: MeshSettings::new(1.0),
        solver: SolverSettings {
            iterations: 600,
            residual: 1e-4,
            processes: 2,
        },
        heat: Some(Heat {
            solids,
            loads: vec![HeatLoad {
                kind: HeatLoadKind::Internal { watts: 10.0 },
                labels: vec![chip_top],
            }],
            ..Heat::water()
        }),
    };
    study.mesh.surface_level = 1;
    let (case, report, results, remove) = solve(&install, "cold-plate", &study);
    assert!(report.converged, "the run converged");
    let goals = results.goals(&case);
    let (into_water, chip, mass) = (
        scalar_goal(&goals, 0),
        scalar_goal(&goals, 1),
        scalar_goal(&goals, 2),
    );
    // What the outlet carries: the mass flux times cp times the rise over 20 °C.
    let heat = study.heat.as_ref().unwrap();
    let first = results.mesh.internal_faces();
    let temperatures = results.boundary_temperature.as_ref().unwrap();
    let flux = results.boundary_flux.as_ref().unwrap();
    let carried: f64 = results
        .boundary_labels
        .iter()
        .enumerate()
        .filter(|(_, l)| **l == Some(outlet))
        .map(|(i, _)| {
            flux[i]
                * study.fluid.density
                * heat.specific_heat
                * (temperatures[i] - heat.temperature)
        })
        .sum();
    let expected_mass = study.fluid.density * speed * 16e-6;
    eprintln!(
        "cold plate: {} cells; into the water {into_water:.4} W, carried out {carried:.4} W, \
         chip {chip:.2} °C, {mass:.5} kg/s out ({expected_mass:.5} set); first face {first}",
        report.cells.unwrap_or(0)
    );
    assert!((carried - 10.0).abs() < 0.1, "carried {carried} W");
    assert!((into_water - 10.0).abs() < 0.3, "walls {into_water} W");
    assert!((mass / expected_mass - 1.0).abs() < 0.01, "{mass} kg/s");
    assert!(
        chip > heat.temperature + 1.0 && chip < 100.0,
        "chip {chip} °C"
    );
    if remove {
        let _ = std::fs::remove_dir_all(&case.dir);
    }
}

/// The same cold plate with the water at 2 m/s (Re 8000, k-ω SST), its 10 W let in through
/// the chip's top face (Heat Power) and its bottom face losing heat to 20 °C surroundings
/// (Convection, 50 W/m²·K): what the water carries and what the bottom loses add up to 10 W.
#[test]
fn a_turbulent_cold_plate_balances_its_heat() {
    let Some(install) = openfoam() else { return };
    let aluminium = |name: &str| Solid {
        name: name.to_string(),
        conductivity: 200.0,
        density: 2700.0,
    };
    let lid = |name: &str| Solid {
        name: name.to_string(),
        conductivity: 0.2,
        density: 1200.0,
    };
    let surfaces = vec![
        cuboid([-20.0, 0.0, -10.0], [20.0, 3.0, 10.0], 0),
        cuboid([-20.0, 7.0, -10.0], [20.0, 10.0, 10.0], 6),
        cuboid([-20.0, 3.0, -10.0], [20.0, 7.0, -2.0], 12),
        cuboid([-20.0, 3.0, 2.0], [20.0, 7.0, 10.0], 18),
        cuboid([-21.0, 3.0, -2.0], [-20.0, 7.0, 2.0], 24),
        cuboid([20.0, 3.0, -2.0], [21.0, 7.0, 2.0], 30),
        cuboid([-5.0, 10.0, -5.0], [5.0, 12.0, 5.0], 36),
    ];
    let solids = vec![
        aluminium("Bottom"),
        aluminium("Top"),
        aluminium("Front"),
        aluminium("Back"),
        lid("Inlet lid"),
        lid("Outlet lid"),
        Solid {
            name: "Chip".to_string(),
            conductivity: 20.0,
            density: 3000.0,
        },
    ];
    let (inlet, outlet, chip_top, bottom) = (25, 30, 39, 2);
    let mut study = FlowStudy {
        surfaces,
        flow_type: FlowType::Internal {
            point: [0.3, 5.3, 0.3],
        },
        domain: Domain::internal(([-21.0, 0.0, -10.0], [21.0, 12.0, 10.0]), 2.0),
        fluid: Fluid::water(),
        turbulence: Turbulence::KOmegaSst {
            intensity: 0.05,
            length: 0.3,
        },
        conditions: vec![
            Condition {
                kind: ConditionKind::InletVelocity { speed: 2.0 },
                labels: vec![inlet],
            },
            Condition {
                kind: ConditionKind::OutletPressure { pressure: 0.0 },
                labels: vec![outlet],
            },
        ],
        goals: vec![
            Goal {
                name: "Into the water".to_string(),
                kind: GoalKind::HeatFlow,
                labels: vec![3, 8, 17, 22],
            },
            Goal {
                name: "Off the bottom".to_string(),
                kind: GoalKind::HeatFlow,
                labels: vec![bottom],
            },
            Goal {
                name: "Chip".to_string(),
                kind: GoalKind::MaxTemperature,
                labels: vec![chip_top],
            },
        ],
        mesh: MeshSettings::new(1.0),
        solver: SolverSettings {
            iterations: 800,
            residual: 1e-4,
            processes: 1,
        },
        heat: Some(Heat {
            solids,
            loads: vec![
                HeatLoad {
                    kind: HeatLoadKind::Power { watts: 10.0 },
                    labels: vec![chip_top],
                },
                HeatLoad {
                    kind: HeatLoadKind::Convection {
                        coefficient: 50.0,
                        ambient: 20.0,
                    },
                    labels: vec![bottom],
                },
            ],
            ..Heat::water()
        }),
    };
    study.mesh.surface_level = 1;
    let (case, report, results, remove) = solve(&install, "turbulent-plate", &study);
    let goals = results.goals(&case);
    let (into_water, off_bottom, chip) = (
        scalar_goal(&goals, 0),
        scalar_goal(&goals, 1),
        scalar_goal(&goals, 2),
    );
    let heat = study.heat.as_ref().unwrap();
    let temperatures = results.boundary_temperature.as_ref().unwrap();
    let flux = results.boundary_flux.as_ref().unwrap();
    let carried: f64 = results
        .boundary_labels
        .iter()
        .enumerate()
        .filter(|(_, l)| **l == Some(outlet))
        .map(|(i, _)| {
            flux[i]
                * study.fluid.density
                * heat.specific_heat
                * (temperatures[i] - heat.temperature)
        })
        .sum();
    eprintln!(
        "turbulent plate: converged {}, into the water {into_water:.4} W, carried out {carried:.4} W, \
         off the bottom {off_bottom:.4} W, chip {chip:.2} °C",
        report.converged
    );
    assert!(report.converged, "the run converged");
    assert!(off_bottom > 0.0, "the bottom loses heat: {off_bottom} W");
    assert!(
        (into_water + off_bottom - 10.0).abs() < 0.2,
        "{into_water} + {off_bottom} W"
    );
    assert!((carried - into_water).abs() < 0.1, "carried {carried} W");
    if remove {
        let _ = std::fs::remove_dir_all(&case.dir);
    }
}

/// A heated copper sphere (10 mm) in a stream of air at Re = 20: its Nusselt number
/// against Clift, Grace and Weber's correlation, Nu = 1 + (1 + 1/(Re Pr))^⅓ Re^0.41 Pr^⅓
/// = 4.12 (Whitaker's gives 3.94, Ranz and Marshall's 4.39).
#[test]
fn a_heated_sphere_in_a_stream_matches_its_nusselt_number() {
    let Some(install) = openfoam() else { return };
    let d = 10.0; // mm
    let fluid = Fluid::air();
    let heat = Heat::air();
    let reynolds = 20.0;
    let speed = reynolds * fluid.kinematic_viscosity() / (d * 1e-3);
    let watts = 0.033;
    let study = FlowStudy {
        surfaces: vec![sphere(d / 2.0, [0.0; 3], 3)],
        flow_type: FlowType::External {
            velocity: [speed, 0.0, 0.0],
        },
        domain: Domain {
            min: [-5.0 * d, -5.0 * d, -5.0 * d],
            max: [10.0 * d, 5.0 * d, 5.0 * d],
        },
        fluid: fluid.clone(),
        turbulence: Turbulence::Laminar,
        conditions: vec![],
        goals: vec![
            Goal {
                name: "Surface".to_string(),
                kind: GoalKind::MeanTemperature,
                labels: vec![0],
            },
            Goal {
                name: "Out".to_string(),
                kind: GoalKind::HeatFlow,
                labels: vec![0],
            },
        ],
        mesh: MeshSettings {
            cell_size: 5.0,
            surface_level: 3,
            feature_level: 3,
            layers: 0,
            max_cells: 1_000_000,
        },
        solver: SolverSettings {
            iterations: 1500,
            residual: 1e-5,
            processes: 1,
        },
        heat: Some(Heat {
            solids: vec![Solid {
                name: "Copper".to_string(),
                conductivity: 400.0,
                density: 8960.0,
            }],
            loads: vec![HeatLoad {
                kind: HeatLoadKind::Internal { watts },
                labels: vec![0],
            }],
            ..heat.clone()
        }),
    };
    let (case, report, results, remove) = solve(&install, "hot-sphere", &study);
    assert!(report.converged, "the run converged");
    let goals = results.goals(&case);
    let (surface, out) = (scalar_goal(&goals, 0), scalar_goal(&goals, 1));
    let area = PI * (d * 1e-3).powi(2);
    let nusselt = watts * d * 1e-3 / (area * heat.conductivity * (surface - heat.temperature));
    let pr = heat.specific_heat * fluid.viscosity / heat.conductivity;
    let clift = 1.0
        + (1.0 + 1.0 / (reynolds * pr)).powf(1.0 / 3.0) * reynolds.powf(0.41) * pr.powf(1.0 / 3.0);
    eprintln!(
        "hot sphere: surface {surface:.3} °C, {out:.5} W out of {watts}, Nu {nusselt:.3}, \
         Clift {clift:.3}, {:+.1} %",
        100.0 * (nusselt / clift - 1.0)
    );
    assert!((out / watts - 1.0).abs() < 0.03, "{out} W out");
    assert!((nusselt / clift - 1.0).abs() < 0.12, "Nu {nusselt}");
    if remove {
        let _ = std::fs::remove_dir_all(&case.dir);
    }
}

/// The same sphere, 20 mm, in still air with gravity: natural convection. Churchill's
/// correlation, Nu = 2 + 0.589 Ra^¼ / (1 + (0.469/Pr)^(9/16))^(4/9), for the Rayleigh
/// number of the sphere's rise over the air (the domain reaches four diameters round it and
/// eight above, for the plume).
#[test]
fn a_heated_sphere_in_still_air_matches_churchill() {
    let Some(install) = openfoam() else { return };
    let d = 20.0; // mm
    let fluid = Fluid::air();
    let heat = Heat::air();
    let watts = 0.5;
    let study = FlowStudy {
        surfaces: vec![sphere(d / 2.0, [0.0; 3], 3)],
        flow_type: FlowType::External { velocity: [0.0; 3] },
        domain: Domain {
            min: [-4.0 * d, -4.0 * d, -4.0 * d],
            max: [4.0 * d, 8.0 * d, 4.0 * d],
        },
        fluid: fluid.clone(),
        turbulence: Turbulence::Laminar,
        conditions: vec![],
        goals: vec![Goal {
            name: "Surface".to_string(),
            kind: GoalKind::MeanTemperature,
            labels: vec![0],
        }],
        mesh: MeshSettings {
            cell_size: 8.0,
            surface_level: 3,
            feature_level: 3,
            layers: 0,
            max_cells: 1_000_000,
        },
        solver: SolverSettings {
            iterations: 1500,
            residual: 1e-4,
            processes: 1,
        },
        heat: Some(Heat {
            gravity: Some([0.0, -9.81, 0.0]),
            solids: vec![Solid {
                name: "Copper".to_string(),
                conductivity: 400.0,
                density: 8960.0,
            }],
            loads: vec![HeatLoad {
                kind: HeatLoadKind::Internal { watts },
                labels: vec![0],
            }],
            ..heat.clone()
        }),
    };
    let (case, report, results, remove) = solve(&install, "still-sphere", &study);
    let goals = results.goals(&case);
    let surface = scalar_goal(&goals, 0);
    let rise = surface - heat.temperature;
    let area = PI * (d * 1e-3).powi(2);
    let nusselt = watts * d * 1e-3 / (area * heat.conductivity * rise);
    let nu = fluid.kinematic_viscosity();
    let alpha = heat.conductivity / (fluid.density * heat.specific_heat);
    let rayleigh = 9.81 * heat.expansion * rise * (d * 1e-3).powi(3) / (nu * alpha);
    let pr = nu / alpha;
    let churchill =
        2.0 + 0.589 * rayleigh.powf(0.25) / (1.0 + (0.469 / pr).powf(9.0 / 16.0)).powf(4.0 / 9.0);
    eprintln!(
        "still sphere: converged {}, surface {surface:.2} °C, Ra {rayleigh:.0}, Nu {nusselt:.3}, \
         Churchill {churchill:.3}, {:+.1} %",
        report.converged,
        100.0 * (nusselt / churchill - 1.0)
    );
    // The plume never quite stops moving: the run ends once the sphere's temperature has
    // settled.
    assert!(report.converged, "the temperatures settled");
    assert!(report.iterations < 1000, "{} iterations", report.iterations);
    assert!((nusselt / churchill - 1.0).abs() < 0.2, "Nu {nusselt}");
    if remove {
        let _ = std::fs::remove_dir_all(&case.dir);
    }
}
