//! The Simulation tab's path through the real kernel: a part's export meshes, labelled by
//! face, filled with tetrahedra by `oxyde_sim`'s mesher and solved, against beam theory
//! and, for the thermal studies, against conduction along a bar.

use oxyde_geom::Point2;
use oxyde_kernel::{Kernel, Mesh, VolumeMeshError, VolumeMeshOptions};
use oxyde_model::{
    Extrude, ExtrudeOp, FeatureKind, FeatureStatus, Part, Sketch, SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;
use oxyde_sim::{
    Contact, ContactKind, Elastic, Fixture, FixtureKind, Load, LoadKind, MeshOptions, SolveOptions,
    Study, SurfaceMesh, TetMesh, ThermalLoad, ThermalLoadKind, ThermalMaterial, ThermalStudy,
};
use std::sync::atomic::AtomicBool;

/// A 20 × 20 block of `length` along X from `x`, on the Front plane, extruded 20 along Z.
fn block(part: &mut Part, x: f64, length: f64, op: ExtrudeOp) {
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::new(x, 0.0), length, 20.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let mut extrude = Extrude::blind(s, op, 20.0);
    extrude.reverse = false;
    part.add_feature(FeatureKind::Extrude(extrude));
}

/// The part's solids as the mesher takes them, every face labelled by its index in the
/// part's merged topology, and those labels' faces lying flat at `x`.
fn surfaces(part: &Part, kernel: &OcctKernel) -> (Vec<SurfaceMesh>, Vec<(u32, f64)>) {
    let body = part.body().expect("a body");
    let mut out = Vec::new();
    let mut ends = Vec::new();
    for solid in &body.solids {
        let mesh: Mesh = kernel
            .tessellate_for_export(solid.shape, 0.05, 12f64.to_radians())
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
            let x = face.positions[0][0];
            if face.positions.iter().all(|p| (p[0] - x).abs() < 1e-4) {
                ends.push((label, f64::from(x)));
            }
        }
        out.push(s);
    }
    (out, ends)
}

/// Fixed at x = 0, 1 kN down on the face at x = 200: the tip's deflection. With
/// `separate`, each solid is meshed on its own and the touching faces found are bonded by
/// contacts; otherwise the solids are meshed together and share nodes where they touch.
fn cantilever_tip(part: &Part, kernel: &OcctKernel, separate: bool) -> f64 {
    let (solids, ends) = surfaces(part, kernel);
    let fixed: Vec<u32> = ends
        .iter()
        .filter(|(_, x)| x.abs() < 1e-3)
        .map(|e| e.0)
        .collect();
    let loaded: Vec<u32> = ends
        .iter()
        .filter(|(_, x)| (x - 200.0).abs() < 1e-3)
        .map(|e| e.0)
        .collect();
    assert_eq!((fixed.len(), loaded.len()), (1, 1), "{ends:?}");
    let options = MeshOptions {
        element_size: 4.0,
        separate_bodies: separate,
        ..MeshOptions::default()
    };
    let cancel = AtomicBool::new(false);
    let (mesh, report) = oxyde_sim::mesh_solids(&solids, &options, &cancel, &|_| {}).unwrap();
    assert!((report.model_volume - 80_000.0).abs() < 1.0, "{report:?}");
    assert!(report.volume > 0.9 * report.model_volume, "{report:?}");
    let contacts: Vec<Contact> = oxyde_sim::detect_contacts(&mesh, 0.01)
        .iter()
        .map(|c| c.contact(ContactKind::Bonded))
        .collect();
    if separate && solids.len() > 1 {
        assert!(!contacts.is_empty(), "no contact found between the bodies");
    }
    tip_deflection(&mesh, fixed, loaded, contacts, solids.len())
}

/// Fixed on the faces `fixed`, 1 kN down on `loaded`: the deflection of the node nearest
/// the middle of the end at x = 200.
fn tip_deflection(
    mesh: &TetMesh,
    fixed: Vec<u32>,
    loaded: Vec<u32>,
    contacts: Vec<Contact>,
    bodies: usize,
) -> f64 {
    let cancel = AtomicBool::new(false);
    let steel = Elastic::new(200_000.0, 0.3, 250.0, 7850.0);
    let study = Study {
        fixtures: vec![Fixture {
            kind: FixtureKind::Fixed,
            labels: fixed,
        }],
        loads: vec![Load {
            kind: LoadKind::Force {
                total: 1000.0,
                direction: Some([0.0, -1.0, 0.0]),
            },
            labels: loaded,
        }],
        gravity: None,
        contacts,
    };
    let materials = vec![steel; bodies];
    let r = oxyde_sim::solve(
        mesh,
        &materials,
        &study,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    )
    .unwrap();
    // The node nearest the middle of the loaded end.
    let tip = (0..mesh.nodes.len())
        .min_by(|&a, &b| {
            let d = |i: usize| {
                let p = mesh.nodes[i];
                (p[0] - 200.0).powi(2) + (p[1] - 10.0).powi(2) + (p[2] - 10.0).powi(2)
            };
            d(a).total_cmp(&d(b))
        })
        .unwrap();
    -r.displacement[tip][1]
}

#[test]
fn a_cantilever_part_deflects_as_beam_theory_says() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    block(&mut part, 0.0, 200.0, ExtrudeOp::Boss);
    part.rebuild(&mut kernel);
    assert!(
        part.timeline()
            .iter()
            .all(|e| e.status == FeatureStatus::Ok)
    );
    // PL³ / 3EI = 1000 · 200³ / (3 · 200 000 · 20⁴ / 12) = 1 mm, plus a little shear and
    // the mesher's bevelled edges.
    let delta = cantilever_tip(&part, &kernel, false);
    assert!(
        delta > 0.97 && delta < 1.12,
        "tip {delta} mm, beam theory 1 mm"
    );
}

#[test]
fn two_bodies_touching_are_bonded() {
    // The same beam in two bodies meeting at x = 100: held together where they touch, as
    // SolidWorks' default contact, it bends as one.
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    block(&mut part, 0.0, 100.0, ExtrudeOp::Boss);
    block(&mut part, 100.0, 100.0, ExtrudeOp::NewBody);
    part.rebuild(&mut kernel);
    assert_eq!(part.body().map(|b| b.solids.len()), Some(2));
    let delta = cantilever_tip(&part, &kernel, false);
    assert!(
        delta > 0.97 && delta < 1.12,
        "tip {delta} mm, beam theory 1 mm"
    );
}

#[test]
fn two_bodies_meshed_apart_are_bonded_by_a_detected_contact() {
    // The same two bodies, each meshed on its own: the faces where they touch are found
    // and bonded by a contact, and the beam bends as one again.
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    block(&mut part, 0.0, 100.0, ExtrudeOp::Boss);
    block(&mut part, 100.0, 100.0, ExtrudeOp::NewBody);
    part.rebuild(&mut kernel);
    assert_eq!(part.body().map(|b| b.solids.len()), Some(2));
    let delta = cantilever_tip(&part, &kernel, true);
    assert!(
        delta > 0.97 && delta < 1.12,
        "tip {delta} mm, beam theory 1 mm"
    );
}

#[test]
fn a_cantilever_meshed_by_netgen_deflects_as_beam_theory_says() {
    // As the Simulation tab meshes with Netgen: first-order from the kernel, each face's
    // triangles labelled by the face's index, made ten-node by the solver's crate.
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    block(&mut part, 0.0, 200.0, ExtrudeOp::Boss);
    part.rebuild(&mut kernel);
    let body = part.body().expect("a body");
    let solid = &body.solids[0];
    let options = VolumeMeshOptions {
        second_order: false,
        ..VolumeMeshOptions::with_size(4.0)
    };
    let v = match kernel.volume_mesh(solid.shape, &options) {
        Ok(v) => v,
        Err(VolumeMeshError::Unsupported) => {
            eprintln!("skipped: Oxyde was built without Netgen");
            return;
        }
        Err(e) => panic!("Netgen failed: {e}"),
    };
    let boundary: Vec<([u32; 3], u32)> = v
        .triangles
        .iter()
        .zip(&v.triangle_faces)
        .map(|(t, f)| (*t, solid.faces.start + f))
        .collect();
    let mut mesh = TetMesh::default();
    mesh.append_solid(&v.nodes, &v.tets, &boundary, 0);
    mesh.make_quadratic();
    assert!(mesh.validate().is_ok(), "{:?}", mesh.validate());
    assert!(
        (mesh.volume() - 80_000.0).abs() < 1e-6 * 80_000.0,
        "{}",
        mesh.volume()
    );
    // The end faces: those whose facets all lie at x = 0 or x = 200.
    let end = |x: f64| -> Vec<u32> {
        let mut labels: Vec<u32> = mesh
            .facets
            .iter()
            .filter(|f| {
                mesh.facet_corners(f)
                    .iter()
                    .all(|&i| (mesh.nodes[i as usize][0] - x).abs() < 1e-6)
            })
            .map(|f| f.label)
            .collect();
        labels.sort_unstable();
        labels.dedup();
        labels
    };
    let (fixed, loaded) = (end(0.0), end(200.0));
    assert_eq!((fixed.len(), loaded.len()), (1, 1));
    // Netgen keeps the beam's sharp edges, so only shear and the fixed end's Poisson
    // restraint separate it from beam theory's 1 mm.
    let delta = tip_deflection(&mesh, fixed, loaded, Vec::new(), 1);
    assert!(
        delta > 0.97 && delta < 1.06,
        "tip {delta} mm, beam theory 1 mm"
    );
}

/// The temperature at the node nearest `(x, 10, 10)` of a 200 mm aluminium bar held at
/// 100 °C at x = 0 and convected to 20 °C at x = 200 (h = 50 W/m²·K), its sides insulated.
fn bar_temperature(part: &Part, kernel: &OcctKernel, separate: bool, x: f64) -> (f64, f64) {
    let (solids, ends) = surfaces(part, kernel);
    let at = |x0: f64| -> Vec<u32> {
        ends.iter()
            .filter(|(_, x)| (x - x0).abs() < 1e-3)
            .map(|e| e.0)
            .collect()
    };
    let options = MeshOptions {
        element_size: 4.0,
        separate_bodies: separate,
        ..MeshOptions::default()
    };
    let cancel = AtomicBool::new(false);
    let (mesh, _) = oxyde_sim::mesh_solids(&solids, &options, &cancel, &|_| {}).unwrap();
    let contacts: Vec<Contact> = oxyde_sim::detect_contacts(&mesh, 0.01)
        .iter()
        .map(|c| c.contact(ContactKind::Bonded))
        .collect();
    if separate && solids.len() > 1 {
        assert!(!contacts.is_empty(), "no contact found between the bodies");
    }
    let study = ThermalStudy {
        loads: vec![
            ThermalLoad {
                kind: ThermalLoadKind::Temperature { celsius: 100.0 },
                labels: at(0.0),
            },
            ThermalLoad {
                kind: ThermalLoadKind::Convection {
                    coefficient: 50.0,
                    ambient: 20.0,
                },
                labels: at(200.0),
            },
        ],
        contacts,
        ..ThermalStudy::default()
    };
    let aluminium = ThermalMaterial::new(167.0, 896.0, 2700.0);
    let r = oxyde_sim::solve_thermal(
        &mesh,
        &vec![aluminium; solids.len()],
        &study,
        &SolveOptions::default(),
        &cancel,
        &|_| {},
    )
    .unwrap();
    let near = |x: f64| {
        let i = (0..mesh.nodes.len())
            .min_by(|&a, &b| {
                let d = |i: usize| {
                    let p = mesh.nodes[i];
                    (p[0] - x).powi(2) + (p[1] - 10.0).powi(2) + (p[2] - 10.0).powi(2)
                };
                d(a).total_cmp(&d(b))
            })
            .unwrap();
        (mesh.nodes[i][0], r.temperature[i])
    };
    near(x)
}

/// The steady temperature along the bar of [`bar_temperature`]: the bar (L / kA) and the
/// film (1 / hA) in series.
fn bar_theory(x: f64) -> f64 {
    let (k, h, length) = (167.0, 50.0, 0.2);
    let area = 0.02 * 0.02;
    let power = 80.0 / (length / (k * area) + 1.0 / (h * area));
    100.0 - power * (x * 1e-3) / (k * area)
}

#[test]
fn a_bar_part_conducts_heat_as_theory_says() {
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    block(&mut part, 0.0, 200.0, ExtrudeOp::Boss);
    part.rebuild(&mut kernel);
    for x in [50.0, 150.0, 200.0] {
        let (at, t) = bar_temperature(&part, &kernel, false, x);
        let expected = bar_theory(at);
        assert!(
            (t - expected).abs() < 0.01 * (100.0 - 20.0),
            "{t} °C at x = {at}, theory {expected} °C"
        );
    }
}

#[test]
fn heat_crosses_a_detected_contact_between_bodies() {
    // The same bar as two bodies meshed apart: the contact found between them conducts.
    let mut kernel = OcctKernel::new();
    let mut part = Part::new();
    block(&mut part, 0.0, 100.0, ExtrudeOp::Boss);
    block(&mut part, 100.0, 100.0, ExtrudeOp::NewBody);
    part.rebuild(&mut kernel);
    assert_eq!(part.body().map(|b| b.solids.len()), Some(2));
    for x in [50.0, 150.0] {
        let (at, t) = bar_temperature(&part, &kernel, true, x);
        let expected = bar_theory(at);
        // The lattice mesher's bevelled edges where the bodies meet cost a little.
        assert!(
            (t - expected).abs() < 0.03 * (100.0 - 20.0),
            "{t} °C at x = {at}, theory {expected} °C"
        );
    }
}
