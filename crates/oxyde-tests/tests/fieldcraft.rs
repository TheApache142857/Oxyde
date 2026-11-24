//! FieldCraft's tetrahedral mesher against the lattice mesher and Netgen on the study parts
//! of `simulation.rs` and `netgen.rs`: per part and mesher, the element count, the worst and
//! mean dihedral angles, the volume against the kernel's, the meshing time, and one static
//! study's largest displacement and von Mises stress. Built with the `fieldcraft` feature.

#![cfg(feature = "fieldcraft")]

use oxyde_geom::{Curve2, Plane, Point2, Point3, Profile, ProfileSegment, Vec3};
use oxyde_kernel::{CurveKind, Kernel, ShapeHandle, VolumeMeshError, VolumeMeshOptions};
use oxyde_occt::OcctKernel;
use oxyde_sim::{
    Elastic, Fixture, FixtureKind, Load, LoadKind, MeshOptions, SolveOptions, Study, SurfaceMesh,
    TetMesh,
};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// Chord deviation and angle of the surfaces the lattice mesher and FieldCraft fill, mm and
/// degrees, as `simulation.rs` tessellates its parts.
const DEFLECTION: f64 = 0.05;
const ANGLE: f64 = 12.0;
/// Within this distance of a plane, mm, a face lies on it.
const ON_PLANE: f64 = 1e-4;

/// A study part: its shape, element size, the planes its fixed and loaded faces lie on
/// (axis and coordinate), the load's direction, and a point at mid-span on the side the
/// load stretches with its bending stress by beam theory, MPa.
struct Case {
    name: &'static str,
    shape: ShapeHandle,
    size: f64,
    fixed: (usize, f64),
    loaded: (usize, f64),
    direction: [f64; 3],
    probe: [f64; 3],
    bending: f64,
}

fn cases(kernel: &mut OcctKernel) -> Vec<Case> {
    let beam = kernel
        .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(200.0, 20.0, 20.0))
        .unwrap();
    let block = kernel
        .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(40.0, 20.0, 10.0))
        .unwrap();
    let edges: Vec<u32> = kernel
        .topology(block)
        .unwrap()
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.curve == CurveKind::Line && e.axis.is_some_and(|a| a.direction.z.abs() > 0.99)
        })
        .map(|(k, _)| k as u32)
        .collect();
    let filleted = kernel.fillet(block, &edges, 4.0).unwrap().shape;
    let circle = Profile::from_curves(&[ProfileSegment {
        id: 0,
        curve: Curve2::Circle {
            center: Point2::new(0.0, 0.0),
            radius: 10.0,
        },
    }])
    .unwrap();
    let cylinder = kernel
        .extrude(&Plane::FRONT, &circle, Vec3::new(0.0, 0.0, 30.0))
        .unwrap()
        .shape;
    vec![
        Case {
            name: "cantilever 200x20x20",
            shape: beam,
            size: 4.0,
            fixed: (0, 0.0),
            loaded: (0, 200.0),
            direction: [0.0, -1.0, 0.0],
            probe: [100.0, 20.0, 10.0],
            bending: 1000.0 * 100.0 * 10.0 / (20f64.powi(4) / 12.0),
        },
        Case {
            name: "filleted box 40x20x10 r4",
            shape: filleted,
            size: 3.0,
            fixed: (0, 0.0),
            loaded: (0, 40.0),
            direction: [0.0, -1.0, 0.0],
            probe: [20.0, 20.0, 5.0],
            bending: 1000.0 * 20.0 * 10.0 / (10.0 * 20f64.powi(3) / 12.0),
        },
        Case {
            name: "cylinder r10 h30",
            shape: cylinder,
            size: 3.0,
            fixed: (2, 0.0),
            loaded: (2, 30.0),
            direction: [0.0, -1.0, 0.0],
            probe: [0.0, 10.0, 15.0],
            bending: 1000.0 * 15.0 * 10.0 / (std::f64::consts::PI * 10f64.powi(4) / 4.0),
        },
    ]
}

/// The shape's surface as the lattice mesher takes it, each face labelled by its index.
fn surface(kernel: &OcctKernel, shape: ShapeHandle) -> SurfaceMesh {
    let mesh = kernel
        .tessellate_for_export(shape, DEFLECTION, ANGLE.to_radians())
        .unwrap();
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
            s.labels.push(k as u32);
        }
    }
    s
}

/// The labels of the faces whose every triangle lies on the plane `axis` = `at`.
fn faces_on(s: &SurfaceMesh, (axis, at): (usize, f64)) -> Vec<u32> {
    let mut labels: Vec<u32> = s.labels.clone();
    labels.sort_unstable();
    labels.dedup();
    labels.retain(|&l| {
        s.triangles
            .iter()
            .zip(&s.labels)
            .filter(|(_, m)| **m == l)
            .all(|(t, _)| {
                t.iter()
                    .all(|&n| (s.positions[n as usize][axis] - at).abs() < ON_PLANE)
            })
    });
    labels
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The six dihedral angles of a tetrahedron, degrees.
fn dihedrals(p: [[f64; 3]; 4]) -> [f64; 6] {
    const EDGES: [(usize, usize, usize, usize); 6] = [
        (0, 1, 2, 3),
        (0, 2, 1, 3),
        (0, 3, 1, 2),
        (1, 2, 0, 3),
        (1, 3, 0, 2),
        (2, 3, 0, 1),
    ];
    EDGES.map(|(a, b, c, d)| {
        let e = sub(p[b], p[a]);
        let across = |q: [f64; 3]| {
            let v = sub(q, p[a]);
            let t = dot(v, e) / dot(e, e);
            [v[0] - t * e[0], v[1] - t * e[1], v[2] - t * e[2]]
        };
        let (u, v) = (across(p[c]), across(p[d]));
        (dot(u, v) / (dot(u, u) * dot(v, v)).sqrt())
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees()
    })
}

/// The smallest and largest dihedral angles of the mesh, and the mean of each element's
/// smallest, degrees.
fn angles(mesh: &TetMesh) -> (f64, f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = 0.0f64;
    let mut sum = 0.0;
    for t in &mesh.tets {
        let d = dihedrals(t.map(|n| mesh.nodes[n as usize]));
        let least = d.iter().copied().fold(f64::INFINITY, f64::min);
        lo = lo.min(least);
        hi = d.iter().copied().fold(hi, f64::max);
        sum += least;
    }
    (lo, hi, sum / mesh.tets.len() as f64)
}

/// Fixed on `fixed`, 1 kN along `direction` on `loaded`, steel: the largest displacement,
/// the largest von Mises stress at a node, and the von Mises stress at the node nearest
/// `probe`.
fn study(
    mesh: &TetMesh,
    fixed: Vec<u32>,
    loaded: Vec<u32>,
    direction: [f64; 3],
    probe: [f64; 3],
) -> Outcome {
    let study = Study {
        fixtures: vec![Fixture {
            kind: FixtureKind::Fixed,
            labels: fixed,
        }],
        loads: vec![Load {
            kind: LoadKind::Force {
                total: 1000.0,
                direction: Some(direction),
            },
            labels: loaded,
        }],
        gravity: None,
        contacts: Vec::new(),
    };
    let steel = Elastic::new(200_000.0, 0.3, 250.0, 7850.0);
    let cancel = AtomicBool::new(false);
    let r = oxyde_sim::solve(
        mesh,
        &[steel],
        &study,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    )
    .unwrap();
    let displacement = r
        .displacement
        .iter()
        .map(|u| dot(*u, *u).sqrt())
        .fold(0.0, f64::max);
    let peak = (0..mesh.nodes.len())
        .max_by(|&a, &b| r.von_mises[a].total_cmp(&r.von_mises[b]))
        .unwrap();
    let distance = |n: usize| {
        let d = sub(mesh.nodes[n], probe);
        dot(d, d)
    };
    let nearest = (0..mesh.nodes.len())
        .min_by(|&a, &b| distance(a).total_cmp(&distance(b)))
        .unwrap();
    Outcome {
        displacement,
        peak: r.von_mises[peak],
        peak_at: mesh.nodes[peak],
        mid_span: r.von_mises[nearest],
    }
}

/// What [`study`] found: the largest displacement, mm, the largest von Mises stress at a
/// node and where, MPa and mm, and the von Mises stress at the node nearest the probe.
struct Outcome {
    displacement: f64,
    peak: f64,
    peak_at: [f64; 3],
    mid_span: f64,
}

#[derive(Clone, Copy, Debug)]
enum Mesher {
    Lattice,
    Netgen,
    FieldCraft,
}

/// The mesher's ten-node mesh of `case` and its meshing time, s, or why there is none.
fn mesh_with(
    mesher: Mesher,
    kernel: &OcctKernel,
    case: &Case,
    s: &SurfaceMesh,
) -> Result<(TetMesh, f64), String> {
    let start = Instant::now();
    let mesh = match mesher {
        Mesher::Lattice => {
            let options = MeshOptions {
                element_size: case.size,
                ..MeshOptions::default()
            };
            let cancel = AtomicBool::new(false);
            oxyde_sim::mesh_solids(std::slice::from_ref(s), &options, &cancel, &|_| {})
                .map_err(|e| format!("{e:?}"))?
                .0
        }
        Mesher::Netgen => {
            let options = VolumeMeshOptions {
                second_order: false,
                ..VolumeMeshOptions::with_size(case.size)
            };
            let v = match kernel.volume_mesh(case.shape, &options) {
                Ok(v) => v,
                Err(VolumeMeshError::Unsupported) => {
                    return Err("this build has no Netgen".into());
                }
                Err(e) => return Err(e.to_string()),
            };
            let boundary: Vec<([u32; 3], u32)> = v
                .triangles
                .iter()
                .zip(&v.triangle_faces)
                .map(|(t, f)| (*t, *f))
                .collect();
            let mut mesh = TetMesh::default();
            mesh.append_solid(&v.nodes, &v.tets, &boundary, 0);
            mesh.make_quadratic();
            mesh
        }
        Mesher::FieldCraft => {
            let mut mesh = TetMesh::default();
            oxyde_sim::fieldcraft::append_quality_mesh(&mut mesh, s, case.size, 0)
                .map_err(|e| e.to_string())?;
            mesh.make_quadratic();
            mesh
        }
    };
    Ok((mesh, start.elapsed().as_secs_f64()))
}

#[test]
#[ignore = "a comparison run: --features fieldcraft --test fieldcraft -- --ignored --nocapture"]
fn fieldcraft_against_the_lattice_mesher_and_netgen() {
    let mut kernel = OcctKernel::new();
    for case in cases(&mut kernel) {
        let s = surface(&kernel, case.shape);
        let fixed = faces_on(&s, case.fixed);
        let loaded = faces_on(&s, case.loaded);
        assert_eq!((fixed.len(), loaded.len()), (1, 1), "{}", case.name);
        let exact = kernel.volume(case.shape).unwrap();
        for mesher in [Mesher::Lattice, Mesher::Netgen, Mesher::FieldCraft] {
            match mesh_with(mesher, &kernel, &case, &s) {
                Ok((mesh, seconds)) => {
                    let valid = mesh.validate();
                    let (lo, hi, mean) = angles(&mesh);
                    let error = 100.0 * (mesh.volume() - exact) / exact;
                    let o = study(
                        &mesh,
                        fixed.clone(),
                        loaded.clone(),
                        case.direction,
                        case.probe,
                    );
                    eprintln!(
                        "{} | {mesher:?} | {} elements | dihedral {lo:.1} to {hi:.1}, \
                         mean least {mean:.1} | volume {error:+.3} % | {seconds:.2} s | \
                         max u {:.5} mm | max vm {:.2} MPa at {:.2?} | mid-span vm {:.2} MPa \
                         (beam {:.2}) | {valid:?}",
                        case.name,
                        mesh.tets.len(),
                        o.displacement,
                        o.peak,
                        o.peak_at,
                        o.mid_span,
                        case.bending,
                    );
                    if matches!(mesher, Mesher::FieldCraft) {
                        assert!(valid.is_ok(), "{}: {valid:?}", case.name);
                    }
                }
                Err(why) => eprintln!("{} | {mesher:?} | no mesh: {why}", case.name),
            }
        }
    }
}
