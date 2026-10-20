//! Generative design through the real kernel: a part of three bodies modelled through
//! OpenCASCADE (two rings to keep, a post to stay clear of), its design space laid on a
//! grid, an outcome grown between the rings and made a solid, as Create Part does.
//!
//! An ignored test times the same bracket at a resolution:
//! `cargo test -p oxyde-tests --test generative -- --ignored --nocapture`, with
//! `OXYDE_GENERATIVE_DENSITY` (the resolution, 0 coarse to 1 fine; 0.5 by default),
//! `OXYDE_GENERATIVE_ITERATIONS` (30 by default), `OXYDE_GENERATIVE_THREADS`,
//! `OXYDE_GENERATIVE_GPU` (solve on the graphics card) and `OXYDE_GENERATIVE_OBJ` (a file to
//! write the outcome's smoothed surface to).

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use oxyde_geom::Point2;
use oxyde_kernel::{Kernel, Mesh, ShapeHandle, SurfaceKind};
use oxyde_model::{Extrude, ExtrudeOp, FeatureKind, Part, Sketch, SketchPlane, StandardPlane};
use oxyde_occt::OcctKernel;
use oxyde_sim::generative::{
    Backend, DesignSpace, GenerativeStudy, Grid, Method, Obstacle, Outcome, Voxel,
    decimate_holding, exact_surface, generate, generate_on, solid_pieces, surface, voxel_size,
};
use oxyde_sim::{Elastic, Fixture, FixtureKind, Load, LoadKind, SolveOptions, SurfaceMesh};
use oxyde_sim_gpu::GpuBackend;

const STEEL: Elastic = Elastic::new(200_000.0, 0.3, 250.0, 7850.0);

/// A body of its own: `profile` drawn on `plane`, extruded `depth`.
fn body(part: &mut Part, plane: StandardPlane, profile: impl FnOnce(&mut Sketch), depth: f64) {
    let mut sketch = Sketch::new(SketchPlane::Standard(plane));
    profile(&mut sketch);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::NewBody,
        depth,
    )));
}

/// The bracket: a ring (radii 10 and 5) at the origin and a smaller one (8 and 4) at
/// (80, 30), both 10 thick along Z, and a post of radius 6 standing along Y at x = 40
/// between them. Each solid's surface, its faces labelled by their index in the part's
/// topology, and each ring's bore (its smaller cylindrical face).
struct Bracket {
    kernel: OcctKernel,
    solids: Vec<SurfaceMesh>,
    bores: [u32; 2],
}

fn bracket() -> Bracket {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    body(
        &mut part,
        StandardPlane::Front,
        |s| {
            s.circle(Point2::new(0.0, 0.0), 10.0);
            s.circle(Point2::new(0.0, 0.0), 5.0);
        },
        10.0,
    );
    body(
        &mut part,
        StandardPlane::Front,
        |s| {
            s.circle(Point2::new(80.0, 30.0), 8.0);
            s.circle(Point2::new(80.0, 30.0), 4.0);
        },
        10.0,
    );
    body(
        &mut part,
        StandardPlane::Top,
        |s| {
            s.circle(Point2::new(40.0, -5.0), 6.0);
        },
        60.0,
    );
    part.rebuild(&mut kernel);
    let body = part.body().expect("a body");
    assert_eq!(body.solids.len(), 3);
    let mut solids = Vec::new();
    let mut bores = [0u32; 2];
    for (b, solid) in body.solids.iter().enumerate() {
        let mesh: Mesh = kernel
            .tessellate_for_export(solid.shape, 0.02, 10f64.to_radians())
            .expect("a mesh");
        let mut s = SurfaceMesh::default();
        for (k, face) in mesh.faces.iter().enumerate() {
            let label = solid.faces.start + k as u32;
            let base = s.positions.len() as u32;
            s.positions.extend(
                face.positions
                    .iter()
                    .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]),
            );
            for t in &face.triangles {
                s.triangles.push([base + t[0], base + t[1], base + t[2]]);
                s.labels.push(label);
            }
        }
        solids.push(s);
        if b < 2 {
            bores[b] = solid
                .faces
                .clone()
                .filter(|&f| body.topology.faces[f as usize].surface == SurfaceKind::Cylinder)
                .min_by(|&a, &c| {
                    let area = |f: u32| body.topology.faces[f as usize].area;
                    area(a).total_cmp(&area(c))
                })
                .expect("a bore");
        }
    }
    Bracket {
        kernel,
        solids,
        bores,
    }
}

fn study(bracket: &Bracket, density: f64) -> GenerativeStudy {
    let space = DesignSpace {
        preserve: bracket.solids[..2].to_vec(),
        obstacles: vec![Obstacle {
            surface: bracket.solids[2].clone(),
            offset: 1.0,
        }],
        starting_shape: None,
        ..DesignSpace::default()
    };
    let voxel_size = voxel_size(&space, None, density).expect("a voxel size");
    GenerativeStudy {
        space,
        fixtures: vec![Fixture {
            kind: FixtureKind::Fixed,
            labels: vec![bracket.bores[0]],
        }],
        loads: vec![Load {
            kind: LoadKind::Force {
                total: 2000.0,
                direction: Some([0.0, -1.0, 0.0]),
            },
            labels: vec![bracket.bores[1]],
        }],
        load_cases: Vec::new(),
        objective: Default::default(),
        mass_target: 0.12,
        member_size: None,
        max_iterations: 40,
        voxel_size,
        symmetry: None,
    }
}

/// Voxels of at least half density joined face to face from `from` to `to`.
fn joined(grid: &Grid, density: &[u8], from: &[usize], to: &[usize]) -> bool {
    let mut seen = vec![false; grid.voxel_count()];
    let mut queue: VecDeque<usize> = from
        .iter()
        .copied()
        .filter(|&v| density[v] >= 128)
        .collect();
    for &v in &queue {
        seen[v] = true;
    }
    let n = grid.counts;
    while let Some(v) = queue.pop_front() {
        if to.contains(&v) {
            return true;
        }
        let c = [v % n[0], (v / n[0]) % n[1], v / (n[0] * n[1])];
        for a in 0..3 {
            for step in [-1i64, 1] {
                let at = c[a] as i64 + step;
                if at < 0 || at >= n[a] as i64 {
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

/// The preserve voxels nearer `centre` (in X and Y) than `radius`.
fn ring_voxels(grid: &Grid, centre: [f64; 2], radius: f64) -> Vec<usize> {
    (0..grid.voxel_count())
        .filter(|&v| {
            let c = grid.centre(v);
            grid.kinds[v] == Voxel::Preserve && (c[0] - centre[0]).hypot(c[1] - centre[1]) < radius
        })
        .collect()
}

#[test]
fn a_bracket_is_grown_between_its_rings_around_the_post() {
    let bracket = bracket();
    let study = study(&bracket, 0.0);
    let cancel = AtomicBool::new(false);
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    assert!(grid.kinds.contains(&Voxel::Obstacle));
    let outcome = generate(
        &grid,
        &study,
        Method::Unrestricted,
        &STEEL,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    )
    .unwrap();
    assert!(
        (outcome.mass - study.mass_target).abs() < 0.03 * study.mass_target,
        "{} kg",
        outcome.mass
    );
    let (first, last) = (outcome.history[0], *outcome.history.last().unwrap());
    assert!(last.compliance < first.compliance);
    for v in 0..grid.voxel_count() {
        match grid.kinds[v] {
            Voxel::Obstacle | Voxel::Outside => assert_eq!(outcome.density[v], 0),
            Voxel::Preserve => assert_eq!(outcome.density[v], 255),
            Voxel::Design => {}
        }
    }
    let held = ring_voxels(&grid, [0.0, 0.0], 12.0);
    let loaded = ring_voxels(&grid, [80.0, 30.0], 10.0);
    assert!(!held.is_empty() && !loaded.is_empty());
    assert!(joined(&grid, &outcome.density, &loaded, &held));
    let shape = surface(&grid, &outcome.density, 10);
    assert!(!shape.triangles.is_empty() && shape.volume() > 0.0);
}

/// Create Part's work on an outcome: its surface cut to meet the rings and the post on their
/// faces, simplified, a solid made of each piece, written as STEP and read back.
#[test]
fn an_outcome_becomes_a_solid_with_the_rings() {
    let mut bracket = bracket();
    let mut study = study(&bracket, 0.0);
    study.max_iterations = 30;
    let cancel = AtomicBool::new(false);
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    let outcome = generate(
        &grid,
        &study,
        Method::Unrestricted,
        &STEEL,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    )
    .unwrap();
    let (shape, held) = exact_surface(&grid, &outcome.density, 12, &study.space);

    let start = Instant::now();
    let fewer = decimate_holding(&shape, &held, 6000, 0.25 * grid.size);
    let pieces = solid_pieces(&fewer);
    eprintln!(
        "{} triangles to {} in {} piece(s), {:.2} s",
        shape.triangles.len(),
        fewer.triangles.len(),
        pieces.len(),
        start.elapsed().as_secs_f64()
    );
    // The loaded ring joined to the held one: one piece.
    assert_eq!(pieces.len(), 1);
    assert!(fewer.triangles.len() <= 6000);
    let start = Instant::now();
    let kernel = &mut bracket.kernel;
    let solid = kernel
        .solid_from_mesh(&pieces[0].positions, &pieces[0].triangles)
        .unwrap()
        .shape;
    let volume = kernel.volume(solid).unwrap();
    assert!(
        (volume - pieces[0].volume()).abs() < 1e-6 * volume,
        "{volume}"
    );
    eprintln!("solid: {:.2} s", start.elapsed().as_secs_f64());
    // The rings' flat faces are met where they are, at z = 0 and 10 exactly, which the
    // surface cut by the densities (half a voxel off, smoothed) never is; simplifying
    // leaves those points there.
    let on = |s: &oxyde_sim::Shape, z: f64| s.positions.iter().filter(|p| p[2] == z).count();
    assert!(on(&shape, 0.0) > 20 && on(&shape, 10.0) > 20);
    assert!(on(&fewer, 0.0) > 0 && on(&fewer, 10.0) > 0);
    let plain = surface(&grid, &outcome.density, 12);
    assert_eq!(on(&plain, 0.0) + on(&plain, 10.0), 0);

    // Written as STEP and read back, as the part's Imported feature keeps it.
    let path = std::env::temp_dir().join(format!("oxyde-outcome-{}.step", std::process::id()));
    kernel.export_step(&[solid], &[], &[], &path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let read = kernel.import_step(&bytes).unwrap();
    let back = kernel.volume(read.shape).unwrap();
    assert!((back - volume).abs() < 1e-3 * volume, "{back} {volume}");
}

/// A solid's surface, its faces labelled by their index in the part's topology from
/// `first`.
fn labelled_surface(kernel: &OcctKernel, shape: ShapeHandle, first: u32) -> SurfaceMesh {
    let mesh: Mesh = kernel
        .tessellate_for_export(shape, 0.02, 10f64.to_radians())
        .expect("a mesh");
    let mut s = SurfaceMesh::default();
    for (k, face) in mesh.faces.iter().enumerate() {
        let base = s.positions.len() as u32;
        s.positions.extend(
            face.positions
                .iter()
                .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]),
        );
        for t in &face.triangles {
            s.triangles.push([base + t[0], base + t[1], base + t[2]]);
            s.labels.push(first + k as u32);
        }
    }
    s
}

/// The servo arm's case: preserve bodies whose faces lie on the starting shape's, as when
/// they are cut from it. Two rings 10 thick in a plate as thick, bored where they are; the
/// outcome's surface meets the plate's and the rings' faces where they are, so the part is
/// one solid exactly as thick as the plate, with no boolean. (Joined to the exact rings,
/// a surface cut by the densities lay a hair from their faces, and on the servo arm at
/// 0.15 mm OpenCASCADE's fuse ran for a quarter of an hour and left slivers.)
#[test]
fn an_outcome_cut_from_its_starting_shape_meets_its_faces() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    let ring = |part: &mut Part, x: f64| {
        body(
            part,
            StandardPlane::Front,
            |s| {
                s.circle(Point2::new(x, 15.0), 8.0);
                s.circle(Point2::new(x, 15.0), 4.0);
            },
            10.0,
        )
    };
    ring(&mut part, 0.0);
    ring(&mut part, 80.0);
    body(
        &mut part,
        StandardPlane::Front,
        |s| {
            s.rectangle(Point2::new(-12.0, 3.0), 104.0, 24.0);
            s.circle(Point2::new(0.0, 15.0), 4.0);
            s.circle(Point2::new(80.0, 15.0), 4.0);
        },
        10.0,
    );
    part.rebuild(&mut kernel);
    let body = part.body().expect("a body");
    assert_eq!(body.solids.len(), 3);
    let surfaces: Vec<SurfaceMesh> = body
        .solids
        .iter()
        .map(|s| labelled_surface(&kernel, s.shape, s.faces.start))
        .collect();
    let bore = |b: usize| {
        body.solids[b]
            .faces
            .clone()
            .filter(|&f| body.topology.faces[f as usize].surface == SurfaceKind::Cylinder)
            .min_by(|&a, &c| {
                let area = |f: u32| body.topology.faces[f as usize].area;
                area(a).total_cmp(&area(c))
            })
            .expect("a bore")
    };
    let study = GenerativeStudy {
        space: DesignSpace {
            preserve: surfaces[..2].to_vec(),
            obstacles: Vec::new(),
            starting_shape: Some(surfaces[2].clone()),
            ..DesignSpace::default()
        },
        fixtures: vec![Fixture {
            kind: FixtureKind::Fixed,
            labels: vec![bore(0)],
        }],
        loads: vec![Load {
            kind: LoadKind::Force {
                total: 2000.0,
                direction: Some([0.0, -1.0, 0.0]),
            },
            labels: vec![bore(1)],
        }],
        load_cases: Vec::new(),
        objective: Default::default(),
        mass_target: 0.07,
        member_size: None,
        max_iterations: 30,
        voxel_size: 1.0,
        symmetry: None,
    };
    let cancel = AtomicBool::new(false);
    let grid = Grid::build(&study, 0, &cancel).unwrap();
    assert!(grid.kinds.contains(&Voxel::Outside));
    let outcome = generate(
        &grid,
        &study,
        Method::Unrestricted,
        &STEEL,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    )
    .unwrap();
    let start = Instant::now();
    let (shape, held) = exact_surface(&grid, &outcome.density, 12, &study.space);
    let pieces = solid_pieces(&decimate_holding(&shape, &held, 6000, 0.25 * grid.size));
    assert_eq!(pieces.len(), 1);
    let solid = kernel
        .solid_from_mesh(&pieces[0].positions, &pieces[0].triangles)
        .unwrap()
        .shape;
    eprintln!("a solid in {:.2} s", start.elapsed().as_secs_f64());
    let plate = body.solids[2].shape;
    let (lo, hi) = kernel.bounds(solid).unwrap();
    let (plate_lo, plate_hi) = kernel.bounds(plate).unwrap();
    assert!((lo.z - plate_lo.z).abs() < 1e-9 && (hi.z - plate_hi.z).abs() < 1e-9);
    // Within the plate, across too.
    for (a, b) in [(lo.x, plate_lo.x), (lo.y, plate_lo.y)] {
        assert!(a >= b - 1e-9);
    }
    for (a, b) in [(hi.x, plate_hi.x), (hi.y, plate_hi.y)] {
        assert!(a <= b + 1e-9);
    }
    // The outcome's mass is the target's, rings included.
    let volume = kernel.volume(solid).unwrap();
    let target = study.mass_target / (STEEL.density * 1e-9);
    assert!(
        (volume / target - 1.0).abs() < 0.15,
        "{volume} mm³ for {target}"
    );
}

fn write_obj(path: &str, shape: &oxyde_sim::Shape) -> std::io::Result<()> {
    use std::fmt::Write as _;
    let mut s = String::new();
    for p in &shape.positions {
        let _ = writeln!(s, "v {} {} {}", p[0], p[1], p[2]);
    }
    for t in &shape.triangles {
        let _ = writeln!(s, "f {} {} {}", t[0] + 1, t[1] + 1, t[2] + 1);
    }
    std::fs::write(path, s)
}

#[test]
#[ignore = "a timing run: OXYDE_GENERATIVE_DENSITY, _ITERATIONS, _THREADS, _OBJ"]
fn generative_timings() {
    let env = |name: &str| std::env::var(name).ok();
    let density: f64 = env("OXYDE_GENERATIVE_DENSITY")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.5);
    let iterations: usize = env("OXYDE_GENERATIVE_ITERATIONS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let threads: usize = env("OXYDE_GENERATIVE_THREADS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let bracket = bracket();
    let mut study = study(&bracket, density);
    study.max_iterations = iterations;
    let cancel = AtomicBool::new(false);
    let start = Instant::now();
    let grid = Grid::build(&study, threads, &cancel).unwrap();
    let design = grid.kinds.iter().filter(|k| **k == Voxel::Design).count();
    eprintln!(
        "grid: {:?} voxels of {:.3} mm ({} in all, {design} design), {:.2} s",
        grid.counts,
        grid.size,
        grid.voxel_count(),
        start.elapsed().as_secs_f64()
    );
    let start = Instant::now();
    let last = std::cell::Cell::new(0.0f64);
    let progress = |f: f64| {
        let now = start.elapsed().as_secs_f64();
        eprintln!(
            "  {:5.1} %  {:7.2} s  (+{:.2} s)",
            100.0 * f,
            now,
            now - last.get()
        );
        last.set(now);
    };
    let options = SolveOptions {
        threads,
        ..SolveOptions::default()
    };
    let mut card = env("OXYDE_GENERATIVE_GPU").map(|_| GpuBackend::new().unwrap());
    if let Some(card) = &card {
        eprintln!("on {}", card.name());
    }
    let backend = card.as_mut().map(|c| c as &mut dyn Backend);
    let outcome: Outcome = generate_on(
        &grid,
        &study,
        Method::Unrestricted,
        &STEEL,
        &options,
        backend,
        &cancel,
        &progress,
    )
    .unwrap();
    let seconds = start.elapsed().as_secs_f64();
    eprintln!(
        "{} iterations in {seconds:.1} s ({:.2} s each), {} conjugate gradient iterations ({:.1} per solve), converged {}",
        outcome.history.len(),
        seconds / outcome.history.len() as f64,
        outcome.solver_iterations,
        outcome.solver_iterations as f64 / outcome.history.len() as f64,
        outcome.converged
    );
    for (i, step) in outcome.history.iter().enumerate() {
        if (i + 1) % 10 == 0 || i + 1 == outcome.history.len() {
            eprintln!(
                "  {:4}: compliance {:9.2} N·mm, mass {:.4} kg, change {:.3}",
                i + 1,
                step.compliance,
                step.mass,
                step.change
            );
        }
    }
    eprintln!(
        "mass {:.4} kg (target {}), compliance {:.2} N·mm, displacement {:.4} mm, von Mises {:.1} MPa",
        outcome.mass,
        study.mass_target,
        outcome.compliance,
        outcome.max_displacement,
        outcome.max_von_mises
    );
    if let Some(path) = env("OXYDE_GENERATIVE_OBJ") {
        let start = Instant::now();
        let shape = surface(&grid, &outcome.density, 12);
        eprintln!(
            "surface: {} triangles, {:.0} mm³, {:.2} s",
            shape.triangles.len(),
            shape.volume(),
            start.elapsed().as_secs_f64()
        );
        write_obj(&path, &shape).unwrap();
    }
}
