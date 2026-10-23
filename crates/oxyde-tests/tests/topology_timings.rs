//! A topology study of a real part, timed: the part (`OXYDE_TOPOLOGY_PART`, one solid)
//! rebuilt through OpenCASCADE and meshed by Netgen as the Topology tab meshes it, the first
//! topology study of its `.oxysim` (`OXYDE_TOPOLOGY_STUDY`) applied, and a few iterations
//! run. `cargo test -p oxyde-tests --test topology_timings -- --ignored --nocapture`, with:
//!
//! - `OXYDE_TOPOLOGY_ITERATIONS` (3 by default), `OXYDE_TOPOLOGY_SIZE` (the element size,
//!   mm; the study's when absent), `OXYDE_TOPOLOGY_LINEAR` (four-node elements whatever the
//!   study says), `OXYDE_TOPOLOGY_TOLERANCE` (the solver's), `OXYDE_TOPOLOGY_TARGET`;
//! - the study's faces replaced by face indices: `OXYDE_TOPOLOGY_FIXED="79"`,
//!   `OXYDE_TOPOLOGY_LOAD="1"` (200 N along −Y), `OXYDE_TOPOLOGY_KEEP="1:3,79:5"` (face and
//!   depth); `OXYDE_TOPOLOGY_FACES` lists the part's faces to pick them;
//! - `OXYDE_TOPOLOGY_ONE_PIECE` for *Keep it in one piece*;
//! - `OXYDE_TOPOLOGY_MESH_ONLY` to stop after meshing, `OXYDE_TOPOLOGY_OBJ` to write the
//!   shape cut at half density to an OBJ file.
//!
//! On the Servo Arm of the owner's tests (45 × 12 × 6 mm, a 24-tooth spline), the times that
//! made topology studies mesh with four-node elements by default: docs/SIMULATION.md §8.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use oxyde_format::simulation::{
    FixtureKind as FileFixture, ForceDirection, LoadKind as FileLoad, MeshQuality, MeshSize,
};
use oxyde_kernel::{Kernel, VolumeMeshOptions};
use oxyde_model::Part;
use oxyde_occt::OcctKernel;
use oxyde_sim::{
    Elastic, Fixture, FixtureKind, Load, LoadKind, Preserve, SolveOptions, Study, TetMesh,
    TopologyStudy,
};

#[test]
#[ignore = "a timing run on a part named by OXYDE_TOPOLOGY_PART"]
fn topology_timings() {
    let (Ok(part_path), Ok(study_path)) = (
        std::env::var("OXYDE_TOPOLOGY_PART"),
        std::env::var("OXYDE_TOPOLOGY_STUDY"),
    ) else {
        return;
    };
    let mut kernel = OcctKernel::new();
    let file = oxyde_format::load_part(Path::new(&part_path)).unwrap();
    let mut part = Part::from_definition(file.definition).unwrap();
    part.rebuild(&mut kernel);
    let body = part.body().unwrap();
    assert_eq!(body.solids.len(), 1, "one solid only");
    let solid = &body.solids[0];
    let sim = oxyde_format::simulation::load_simulation(Path::new(&study_path)).unwrap();
    let file_study = sim
        .studies
        .iter()
        .find(|s| s.study_type == oxyde_format::simulation::StudyType::Topology)
        .unwrap();
    let size = std::env::var("OXYDE_TOPOLOGY_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .or(match file_study.mesh.size {
            MeshSize::ElementSize(h) => Some(h),
            MeshSize::Automatic { .. } => None,
        })
        .unwrap_or(1.0);
    let start = Instant::now();
    let options = VolumeMeshOptions {
        second_order: false,
        ..VolumeMeshOptions::with_size(size)
    };
    let v = kernel.volume_mesh(solid.shape, &options).unwrap();
    let mut mesh = TetMesh::default();
    let boundary: Vec<([u32; 3], u32)> = v
        .triangles
        .iter()
        .zip(&v.triangle_faces)
        .map(|(t, f)| (*t, *f))
        .collect();
    mesh.append_solid(&v.nodes, &v.tets, &boundary, 0);
    let linear = std::env::var_os("OXYDE_TOPOLOGY_LINEAR").is_some();
    if file_study.mesh.quality == MeshQuality::High && !linear {
        mesh.make_quadratic();
    }
    eprintln!(
        "mesh: {size} mm, {} nodes, {} elements, {:.1} s",
        mesh.nodes.len(),
        mesh.tets.len(),
        start.elapsed().as_secs_f64()
    );
    if std::env::var_os("OXYDE_TOPOLOGY_MESH_ONLY").is_some() {
        return;
    }
    let label = |f: &oxyde_format::simulation::StudyFace| -> u32 {
        let k = body.naming.find_face(&f.face).unwrap();
        let info = &body.topology.faces[k as usize];
        eprintln!(
            "face {k}: {:?} area {:.2} at ({:.2}, {:.2}, {:.2}) normal {:?}",
            info.surface, info.area, info.centroid.x, info.centroid.y, info.centroid.z, info.normal
        );
        k - solid.faces.start
    };
    let (lo, hi) = mesh
        .nodes
        .iter()
        .fold(([f64::MAX; 3], [f64::MIN; 3]), |(lo, hi), p| {
            (
                [lo[0].min(p[0]), lo[1].min(p[1]), lo[2].min(p[2])],
                [hi[0].max(p[0]), hi[1].max(p[1]), hi[2].max(p[2])],
            )
        });
    eprintln!("bounds {lo:?} {hi:?}, {} faces", solid.faces.len());
    if std::env::var_os("OXYDE_TOPOLOGY_FACES").is_some() {
        for k in solid.faces.clone() {
            let info = &body.topology.faces[k as usize];
            if info.area > 2.0 {
                eprintln!(
                    "  {k}: {:?} area {:.1} at ({:.1}, {:.1}, {:.1}) axis {:?}",
                    info.surface,
                    info.area,
                    info.centroid.x,
                    info.centroid.y,
                    info.centroid.z,
                    info.axis.as_ref().map(|a| a.origin)
                );
            }
        }
    }
    let fixtures = file_study
        .fixtures
        .iter()
        .filter(|f| !f.suppressed)
        .map(|f| Fixture {
            kind: match f.kind {
                FileFixture::Fixed => FixtureKind::Fixed,
                FileFixture::Roller => FixtureKind::Roller,
            },
            labels: f.faces.iter().map(label).collect(),
        })
        .collect();
    let loads = file_study
        .loads
        .iter()
        .filter(|l| !l.suppressed)
        .filter_map(|l| match &l.kind {
            FileLoad::Force {
                faces,
                newtons,
                direction,
            } => Some(Load {
                kind: LoadKind::Force {
                    total: *newtons,
                    direction: match direction {
                        ForceDirection::Normal => None,
                        ForceDirection::Vector(v) => Some(*v),
                    },
                },
                labels: faces.iter().map(label).collect(),
            }),
            FileLoad::Pressure { faces, mpa } => Some(Load {
                kind: LoadKind::Pressure { value: *mpa },
                labels: faces.iter().map(label).collect(),
            }),
            _ => None,
        })
        .collect();
    let t = &file_study.topology;
    // Faces by index instead of the file's: OXYDE_TOPOLOGY_FIXED="79",
    // OXYDE_TOPOLOGY_LOAD="1", OXYDE_TOPOLOGY_KEEP="1:3,79:5".
    let list = |name: &str| -> Option<Vec<u32>> {
        let text = std::env::var(name).ok()?;
        Some(
            text.split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect(),
        )
    };
    let (fixtures, loads) = match (list("OXYDE_TOPOLOGY_FIXED"), list("OXYDE_TOPOLOGY_LOAD")) {
        (Some(fixed), Some(load)) => (
            vec![Fixture {
                kind: FixtureKind::Fixed,
                labels: fixed,
            }],
            vec![Load {
                kind: LoadKind::Force {
                    total: -200.0,
                    direction: Some([0.0, 1.0, 0.0]),
                },
                labels: load,
            }],
        ),
        _ => (fixtures, loads),
    };
    let keep: Option<Vec<Preserve>> = std::env::var("OXYDE_TOPOLOGY_KEEP").ok().map(|text| {
        text.split(',')
            .filter_map(|s| {
                let (face, depth) = s.split_once(':')?;
                Some(Preserve {
                    labels: vec![face.trim().parse().ok()?],
                    depth: depth.trim().parse().ok()?,
                })
            })
            .collect()
    });
    let target = std::env::var("OXYDE_TOPOLOGY_TARGET")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(t.target_mass);
    let study = TopologyStudy {
        study: Study {
            fixtures,
            loads,
            gravity: None,
            contacts: Vec::new(),
        },
        target,
        member_size: t.member_size,
        preserved: keep.unwrap_or_else(|| {
            t.preserved
                .iter()
                .filter(|p| !p.suppressed)
                .map(|p| Preserve {
                    labels: p.faces.iter().map(label).collect(),
                    depth: p.depth,
                })
                .collect()
        }),
        max_iterations: std::env::var("OXYDE_TOPOLOGY_ITERATIONS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3),
        symmetry: None,
        pull: None,
        one_piece: std::env::var_os("OXYDE_TOPOLOGY_ONE_PIECE").is_some(),
    };
    let abs = Elastic::new(2_000.0, 0.39, 40.0, 1_050.0);
    let start = Instant::now();
    let r = oxyde_sim::optimize(
        &mesh,
        &[abs],
        &study,
        &SolveOptions {
            tolerance: std::env::var("OXYDE_TOPOLOGY_TOLERANCE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1e-8),
            ..SolveOptions::default()
        },
        &AtomicBool::new(false),
        &|_| {},
    )
    .unwrap();
    eprintln!(
        "optimized: {} iterations in {:.1} s, {} solver iterations, radius {:.3} mm",
        r.history.len(),
        start.elapsed().as_secs_f64(),
        r.solver_iterations,
        r.filter_radius
    );
    let pieces = oxyde_sim::pieces(&mesh, &r.density, 0.5);
    eprintln!(
        "{} piece(s): {:?}; joining {:?}",
        pieces.len(),
        pieces
            .iter()
            .map(|p| (p.labels.clone(), p.volume.round()))
            .collect::<Vec<_>>(),
        r.joining
    );
    if let Ok(path) = std::env::var("OXYDE_TOPOLOGY_OBJ") {
        let s = oxyde_sim::shape(&mesh, &r.density, 0.5, 8);
        let mut text = String::new();
        for p in &s.positions {
            text.push_str(&format!("v {} {} {}\n", p[0], p[1], p[2]));
        }
        for t in &s.triangles {
            text.push_str(&format!("f {} {} {}\n", t[0] + 1, t[1] + 1, t[2] + 1));
        }
        std::fs::write(path, text).unwrap();
    }
}
