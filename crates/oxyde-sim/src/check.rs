//! The check of a part made of an outcome (*Create Part*): its surface filled with ten-node
//! tetrahedra by the lattice mesher, the preserve bodies' faces found on it, each load case
//! of the study solved on it by the static solver, and, when the study limits it, its
//! lowest natural frequency found by the frequency study's solver (`crate::modal`). The outcome's numbers come from
//! voxels, whose corners concentrate stress and whose steps the smoothing and simplifying
//! take away; these are the part's own, to set against them (`docs/SIMULATION.md` §9).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::mesher::{MeshError, MeshOptions, mesh_solids};
use crate::modal::{FrequencyStudy, solve_frequencies};
use crate::solve::{self, SolveError, Study};
use oxyde_generative::GenerativeStudy;
use oxyde_generative::bvh::Bvh;
use oxyde_generative::material::Elastic;
use oxyde_generative::math::{V3, add, cross, dot, norm, normalize, scale, sub};
use oxyde_generative::shape::Shape;
use oxyde_generative::study::{Load, LoadKind, SolveOptions};
use oxyde_generative::surfaces::SurfaceMesh;

/// About this many nodes at most in the check's mesh: elements a voxel where that keeps
/// under it, larger on a large part, so the check takes seconds to a minute or two.
const CHECK_NODES: f64 = 400_000.0;
/// The lattice points the mesher may lay over the part's box, kept under its limit.
const CHECK_LATTICE: f64 = 6_000_000.0;
/// The finest elements, as a share of a voxel: ten-node elements that size put three or more
/// across the thinnest members the density filter leaves, and finer ones add nodes, not
/// truth the outcome's surface has.
const FINEST: f64 = 1.0;
/// Times the element size is grown when the mesher finds the mesh too large.
const RETRIES: usize = 6;
/// A triangle of the part lies on a preserve body's face when its corners are within this
/// share of a voxel of the face (they are held on it: `exact_surface`)…
const CORNER_SLACK: f64 = 0.05;
/// …its centre within this share (a simplified triangle's chord across a curved face)…
const CENTRE_SLACK: f64 = 0.3;
/// …and its normal within about 45° of the face's.
const NORMAL_AGREEMENT: f64 = 0.7;

/// What the check found: the part's numbers, as the outcome has them.
#[derive(Debug, Clone, PartialEq)]
pub struct PartCheck {
    /// mm: the elements' size.
    pub element_size: f64,
    pub nodes: usize,
    pub elements: usize,
    /// The mesh's volume over the part's: under one where members thinner than an element
    /// were lost to the mesh, and the part checked is weaker than the one made.
    pub volume_kept: f64,
    /// kg: the mesh's.
    pub mass: f64,
    /// N·mm: the work of the loads, summed over the load cases.
    pub compliance: f64,
    /// mm: the largest displacement over the load cases.
    pub max_displacement: f64,
    /// MPa: the largest von Mises stress over the load cases, but in the elements touching
    /// the faces the case's fixtures and loads act on (where stress is singular), as the
    /// outcome leaves out the voxels they act through.
    pub max_von_mises: f64,
    /// The yield strength over it, when the material has one.
    pub min_factor_of_safety: Option<f64>,
    /// Hz: the lowest natural frequency, held by the first load case's fixtures, when the
    /// study limits it.
    pub frequency: Option<f64>,
}

/// What the check needs found on the part and could not find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    Fixture,
    Load,
    RemoteMass,
}

/// Why a part could not be checked.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckError {
    /// The part's surface could not be meshed.
    Mesh(MeshError),
    /// None of the faces of the study's fixture, load or remote mass (by index, from 0) are
    /// on the part.
    NotOnPart {
        what: Missing,
        index: usize,
    },
    /// A load case (from 0) could not be solved.
    Solve {
        case: usize,
        error: SolveError,
    },
    /// The part's natural frequencies could not be found.
    Frequency(SolveError),
    Cancelled,
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::Mesh(e) => write!(f, "the part could not be meshed: {e}"),
            CheckError::NotOnPart { what, index } => {
                let what = match what {
                    Missing::Fixture => "fixture",
                    Missing::Load => "load",
                    Missing::RemoteMass => "remote mass",
                };
                write!(
                    f,
                    "the faces of the study's {what} {} were not found on the part",
                    index + 1
                )
            }
            CheckError::Solve { case, error } => {
                write!(f, "load case {} could not be solved: {error}", case + 1)
            }
            CheckError::Frequency(error) => {
                write!(f, "its natural frequencies could not be found: {error}")
            }
            CheckError::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Checks the part made of an outcome of `study`: `pieces` are its surfaces (as
/// [`oxyde_generative::solid_pieces`] gives them, one per solid), `material` the outcome's. `progress`
/// hears what is being done and the fraction done, from 0 to 1.
pub fn check_part(
    study: &GenerativeStudy,
    pieces: &[Shape],
    material: &Elastic,
    cancel: &AtomicBool,
    progress: &dyn Fn(&str, f64),
) -> Result<PartCheck, CheckError> {
    let voxel = study.voxel_size;
    progress("Finding the faces kept on the part", 0.0);
    let kept = kept_faces(study);
    let solids = labelled(study, &kept, pieces);
    if cancel.load(Ordering::Relaxed) {
        return Err(CheckError::Cancelled);
    }

    // The elements: a voxel, larger where the part would need too many.
    let volume: f64 = solids.iter().map(SurfaceMesh::volume).sum();
    let (lo, hi) = bounds(&solids);
    let box_volume: f64 = hi
        .iter()
        .zip(&lo)
        .map(|(h, l)| (h - l).max(voxel))
        .product();
    let mut size = (FINEST * voxel)
        .max((8.0 * volume.max(0.0) / CHECK_NODES).cbrt())
        .max((box_volume / CHECK_LATTICE).cbrt());
    let meshing = |f: f64| progress("Meshing the part", 0.3 * f);
    let mut tries = 0;
    let (mesh, report) = loop {
        let options = MeshOptions {
            element_size: size,
            quadratic: true,
            max_nodes: (2.0 * CHECK_NODES) as usize,
            separate_bodies: false,
        };
        match mesh_solids(&solids, &options, cancel, &meshing) {
            Ok(made) => break made,
            Err(MeshError::TooManyNodes(_) | MeshError::TooFine(_)) if tries < RETRIES => {
                tries += 1;
                size *= 1.25;
            }
            Err(MeshError::Cancelled) => return Err(CheckError::Cancelled),
            Err(e) => return Err(CheckError::Mesh(e)),
        }
    };
    let materials = vec![*material; solids.len().max(1)];

    let cases = study.cases();
    // The solves' share of the progress, the frequency's after them.
    let solving = if study.min_frequency.is_some() {
        0.55
    } else {
        0.7
    };
    let mut compliance = 0.0;
    let mut max_displacement = 0.0f64;
    let mut max_von_mises = 0.0f64;
    let on_part = |labels: &[u32]| mesh.facets.iter().any(|f| labels.contains(&f.label));
    for (c, case) in cases.iter().enumerate() {
        let mut fixtures = Vec::new();
        let mut loads = Vec::new();
        for &i in &case.fixtures {
            let Some(fixture) = study.fixtures.get(i) else {
                continue;
            };
            if !on_part(&fixture.labels) {
                return Err(CheckError::NotOnPart {
                    what: Missing::Fixture,
                    index: i,
                });
            }
            fixtures.push(fixture.clone());
        }
        for &i in &case.loads {
            let Some(load) = study.loads.get(i) else {
                continue;
            };
            if !on_part(&load.labels) {
                return Err(CheckError::NotOnPart {
                    what: Missing::Load,
                    index: i,
                });
            }
            loads.push(through_centre(load, &kept));
        }
        // m/s²: the case's accelerations, summed, which weigh the part and its remote
        // masses.
        let acceleration = case
            .gravity
            .iter()
            .filter_map(|&g| study.gravity.get(g))
            .filter(|a| a.iter().all(|x| x.is_finite()))
            .fold([0.0; 3], |sum, a| add(sum, *a));
        let weighs = dot(acceleration, acceleration) > 0.0;
        for &i in &case.remote_masses {
            let Some(remote) = study.remote_masses.get(i) else {
                continue;
            };
            if !on_part(&remote.labels) {
                return Err(CheckError::NotOnPart {
                    what: Missing::RemoteMass,
                    index: i,
                });
            }
            if weighs {
                loads.push(Load {
                    // kg × m/s² = N.
                    kind: LoadKind::Remote {
                        force: scale(acceleration, remote.mass),
                        point: remote.point,
                    },
                    labels: remote.labels.clone(),
                });
            }
        }
        // Stress is singular where the case's fixtures and loads act: their faces are left
        // out of the largest stress.
        let acting: BTreeSet<u32> = fixtures
            .iter()
            .flat_map(|f| f.labels.iter())
            .chain(loads.iter().flat_map(|l| l.labels.iter()))
            .copied()
            .collect();
        let static_study = Study {
            fixtures,
            loads,
            gravity: weighs.then_some(acceleration),
            contacts: Vec::new(),
        };
        let share = solving / cases.len() as f64;
        let start = 0.3 + share * c as f64;
        let what = if cases.len() > 1 {
            format!("Solving load case {} of {}", c + 1, cases.len())
        } else {
            "Solving".to_owned()
        };
        let results = solve::solve(
            &mesh,
            &materials,
            &static_study,
            &SolveOptions::default(),
            cancel,
            &|f| progress(&what, start + share * f),
        )
        .map_err(|error| match error {
            SolveError::Cancelled => CheckError::Cancelled,
            error => CheckError::Solve { case: c, error },
        })?;
        let forces = solve::load_vector(&mesh, &materials, &static_study);
        compliance += forces
            .iter()
            .zip(&results.displacement)
            .map(|(f, u)| dot(*f, *u))
            .sum::<f64>();
        max_displacement = max_displacement.max(results.summary().max_displacement.0);
        let mut on_faces = vec![false; mesh.nodes.len()];
        for facet in mesh.facets.iter().filter(|f| acting.contains(&f.label)) {
            let mids = mesh.facet_edge_nodes(facet).into_iter().flatten();
            for n in mesh.facet_corners(facet).into_iter().chain(mids) {
                if let Some(s) = on_faces.get_mut(n as usize) {
                    *s = true;
                }
            }
        }
        // And the elements touching those faces, a voxel deep as the outcome's: where the
        // mesh bevels a face's edges, a fixture stops short of them, and the stress is
        // singular where it stops.
        let mut singular = on_faces.clone();
        for e in 0..mesh.tets.len() {
            let (nodes, count) = mesh.element_nodes(e);
            let nodes = nodes.get(..count).unwrap_or(&[]);
            if nodes
                .iter()
                .any(|&n| on_faces.get(n as usize).copied().unwrap_or(false))
            {
                for &n in nodes {
                    if let Some(s) = singular.get_mut(n as usize) {
                        *s = true;
                    }
                }
            }
        }
        let peak = results
            .von_mises
            .iter()
            .zip(&singular)
            .filter(|(_, s)| !**s)
            .map(|(v, _)| *v)
            .fold(0.0f64, f64::max);
        max_von_mises = max_von_mises.max(peak);
    }
    // The lowest natural frequency, held by the first case's fixtures, as the outcome's.
    let frequency = match (study.min_frequency, cases.first()) {
        (Some(_), Some(case)) => {
            let start = 0.3 + solving;
            let frequency_study = FrequencyStudy {
                fixtures: case
                    .fixtures
                    .iter()
                    .filter_map(|&i| study.fixtures.get(i).cloned())
                    .collect(),
                contacts: Vec::new(),
                modes: 1,
            };
            let found = solve_frequencies(
                &mesh,
                &materials,
                &frequency_study,
                &SolveOptions::default(),
                cancel,
                &|f| progress("Finding its lowest frequency", start + (1.0 - start) * f),
            )
            .map_err(|error| match error {
                SolveError::Cancelled => CheckError::Cancelled,
                error => CheckError::Frequency(error),
            })?;
            found.modes.first().map(|m| m.frequency)
        }
        _ => None,
    };
    progress("Done", 1.0);
    let min_factor_of_safety = (material.yield_strength > 0.0 && max_von_mises > 0.0)
        .then(|| material.yield_strength / max_von_mises);
    let mesh_volume = mesh.volume();
    Ok(PartCheck {
        element_size: report.element_size,
        nodes: report.nodes,
        elements: report.elements,
        volume_kept: if report.model_volume > 0.0 {
            report.volume / report.model_volume
        } else {
            1.0
        },
        // kg/m³ × mm³ = 1e-9 kg.
        mass: material.density * mesh_volume * 1e-9,
        compliance,
        max_displacement,
        max_von_mises,
        min_factor_of_safety,
        frequency,
    })
}

/// The faces fixtures and loads may be on, by triangle with its label: every face of the
/// preserve bodies, and the faces of the preserve regions on their bodies.
fn kept_faces(study: &GenerativeStudy) -> Vec<([V3; 3], u32)> {
    let space = &study.space;
    let regions: BTreeSet<u32> = space
        .regions
        .iter()
        .flat_map(|r| r.labels.iter())
        .copied()
        .collect();
    let bodies = space
        .preserve
        .iter()
        .map(|s| (s, None))
        .chain(space.region_bodies.iter().map(|s| (s, Some(&regions))));
    let mut out = Vec::new();
    for (s, only) in bodies {
        for (t, &label) in s.triangles.iter().zip(&s.labels) {
            if only.is_some_and(|only| !only.contains(&label)) {
                continue;
            }
            let p = |i: u32| s.positions.get(i as usize).copied();
            if let (Some(a), Some(b), Some(c)) = (p(t[0]), p(t[1]), p(t[2])) {
                out.push(([a, b, c], label));
            }
        }
    }
    out
}

/// A force along a direction applied through the centre of the faces it was set on, as a
/// remote force there. The check's mesh bevels the part's edges at the scale of an element,
/// unevenly round a face, which takes a band off some sides of the face and so moves the
/// centre of the load: on a slender member the moment that makes bends it, as the part
/// would not be. Through the faces' own centre the load keeps the force and the moment the
/// study gives it. Other loads are left as they are: a pressure's resultant on a bore is
/// nothing, and the pressure is the load.
fn through_centre(load: &Load, kept: &[([V3; 3], u32)]) -> Load {
    let LoadKind::Force {
        total,
        direction: Some(direction),
    } = load.kind
    else {
        return load.clone();
    };
    let Some(direction) = normalize(direction) else {
        return load.clone();
    };
    let (mut area, mut moment) = (0.0, [0.0; 3]);
    for (t, label) in kept {
        if !load.labels.contains(label) {
            continue;
        }
        let a = 0.5 * norm(cross(sub(t[1], t[0]), sub(t[2], t[0])));
        let centre = scale(add(add(t[0], t[1]), t[2]), 1.0 / 3.0);
        area += a;
        moment = add(moment, scale(centre, a));
    }
    if area.is_nan() || area <= 0.0 {
        return load.clone();
    }
    Load {
        kind: LoadKind::Remote {
            force: scale(direction, total),
            point: scale(moment, 1.0 / area),
        },
        labels: load.labels.clone(),
    }
}

/// The pieces as the mesher takes them, each triangle labelled with the kept face
/// ([`kept_faces`]) it lies on, or [`oxyde_generative::NO_LABEL`].
fn labelled(
    study: &GenerativeStudy,
    kept: &[([V3; 3], u32)],
    pieces: &[Shape],
) -> Vec<SurfaceMesh> {
    let faces: Vec<[V3; 3]> = kept.iter().map(|(t, _)| *t).collect();
    let labels: Vec<u32> = kept.iter().map(|(_, l)| *l).collect();
    let tree = (!faces.is_empty()).then(|| Bvh::new(faces));
    let voxel = study.voxel_size;
    let near = |p: V3, slack: f64| -> Option<u32> {
        let (t, d2) = tree.as_ref()?.nearest(p)?;
        (d2 <= (slack * voxel).powi(2)).then_some(t)
    };
    let on_face = |tri: [V3; 3]| -> Option<u32> {
        let normal = normalize(cross(sub(tri[1], tri[0]), sub(tri[2], tri[0])))?;
        let centre = scale(add(add(tri[0], tri[1]), tri[2]), 1.0 / 3.0);
        let t = near(centre, CENTRE_SLACK)?;
        let face = tree.as_ref()?.triangle(t)?;
        let face_normal = normalize(cross(sub(face[1], face[0]), sub(face[2], face[0])))?;
        if dot(normal, face_normal) < NORMAL_AGREEMENT {
            return None;
        }
        if !tri.iter().all(|&p| near(p, CORNER_SLACK).is_some()) {
            return None;
        }
        labels.get(t as usize).copied()
    };
    pieces
        .iter()
        .map(|piece| {
            let face_labels = piece
                .triangles
                .iter()
                .map(|t| {
                    let p = |i: u32| piece.positions.get(i as usize).copied();
                    let label = match (p(t[0]), p(t[1]), p(t[2])) {
                        (Some(a), Some(b), Some(c)) => on_face([a, b, c]),
                        _ => None,
                    };
                    label.unwrap_or(oxyde_generative::NO_LABEL)
                })
                .collect();
            SurfaceMesh {
                positions: piece.positions.clone(),
                triangles: piece.triangles.clone(),
                labels: face_labels,
            }
        })
        .collect()
}

/// The corners of the box around the solids' points.
fn bounds(solids: &[SurfaceMesh]) -> (V3, V3) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in solids.iter().flat_map(|s| s.positions.iter()) {
        for ((l, h), v) in lo.iter_mut().zip(hi.iter_mut()).zip(p) {
            *l = l.min(*v);
            *h = h.max(*v);
        }
    }
    if lo[0] > hi[0] {
        return ([0.0; 3], [0.0; 3]);
    }
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use oxyde_generative::material::Elastic;
    use oxyde_generative::math::V3;
    use oxyde_generative::study::{Fixture, FixtureKind, Load, LoadKind};
    use oxyde_generative::surfaces::{SurfaceMesh, box_surface};
    use oxyde_generative::{DesignSpace, GenerativeStudy, PreserveRegion};

    use super::*;

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

    /// A bar 100 × 10 × 10 mm kept whole (its own preserve body), fixed on its −X face, pulled
    /// 10 kN along +X on its +X face; and the part made of it, the same box, its faces
    /// unlabelled as an outcome's surface comes.
    fn bar_part() -> (GenerativeStudy, oxyde_generative::shape::Shape) {
        let (lo, hi) = ([0.0; 3], [100.0, 10.0, 10.0]);
        let study = GenerativeStudy {
            space: DesignSpace {
                preserve: vec![block(0, lo, hi)],
                margin: [Some(0.0); 3],
                ..DesignSpace::default()
            },
            fixtures: vec![Fixture {
                kind: FixtureKind::Fixed,
                labels: vec![0],
            }],
            loads: vec![Load {
                kind: LoadKind::Force {
                    total: 10_000.0,
                    direction: Some([1.0, 0.0, 0.0]),
                },
                labels: vec![1],
            }],
            gravity: Vec::new(),
            remote_masses: Vec::new(),
            load_cases: Vec::new(),
            objective: Default::default(),
            mass_target: 1.0,
            member_size: None,
            max_iterations: 1,
            voxel_size: 2.0,
            symmetry: None,
            min_frequency: None,
        };
        let surface = box_surface(lo, hi);
        let part = oxyde_generative::shape::Shape {
            labels: vec![oxyde_generative::NO_LABEL; surface.triangles.len()],
            positions: surface.positions,
            triangles: surface.triangles,
        };
        (study, part)
    }

    #[test]
    fn a_part_is_checked_on_its_own_mesh() {
        let (study, part) = bar_part();
        // No Poisson's ratio: the fixed end does not hold the bar from narrowing, so the stress
        // is P/A = 100 MPa throughout.
        let material = Elastic::new(200_000.0, 0.0, 250.0, 7850.0);
        let cancel = AtomicBool::new(false);
        let check = check_part(&study, &[part], &material, &cancel, &|_, _| {}).unwrap();
        // The end moves PL/EA = 0.05 mm, the load's work is 500 N·mm, and 10 cm³ of steel
        // weighs 78.5 g. The mesher bevels the box's edges at an element's scale (here a voxel,
        // a fifth of the bar's side), which takes some of its section and leaves a band of the
        // fixed end free, so the end's corners move up to a sixth more than the theory's; the
        // load goes through the end's centre whatever the bevels take of it, else the bar
        // bends and its end moves twice as far.
        let near =
            |value: f64, expected: f64, share: f64| (value - expected).abs() <= share * expected;
        assert!(near(check.max_displacement, 0.05, 0.2), "{check:?}");
        assert!(near(check.compliance, 500.0, 0.06), "{check:?}");
        assert!(near(check.mass, 0.0785, 0.05), "{check:?}");
        assert!((90.0..=150.0).contains(&check.max_von_mises), "{check:?}");
        let fos = check.min_factor_of_safety.unwrap();
        assert!((fos - 250.0 / check.max_von_mises).abs() < 1e-12);
        assert!(
            check.volume_kept > 0.95 && check.volume_kept <= 1.0 + 1e-9,
            "{check:?}"
        );
        assert!(check.element_size >= study.voxel_size);
        assert!(check.nodes > 0 && check.elements > 0);
    }

    #[test]
    fn a_part_kept_by_preserve_regions_is_checked_on_their_faces() {
        // The same bar with no preserve body: its ends kept as regions of its own body, the
        // fixture and the load on them.
        let (mut study, part) = bar_part();
        let body = study.space.preserve.remove(0);
        study.space.regions = vec![PreserveRegion {
            body: 0,
            labels: vec![0, 1],
            depth: 2.0,
        }];
        study.space.region_bodies = vec![body];
        let material = Elastic::new(200_000.0, 0.0, 250.0, 7850.0);
        let cancel = AtomicBool::new(false);
        let check = check_part(&study, &[part], &material, &cancel, &|_, _| {}).unwrap();
        let near =
            |value: f64, expected: f64, share: f64| (value - expected).abs() <= share * expected;
        assert!(near(check.compliance, 500.0, 0.06), "{check:?}");
        assert!(near(check.max_displacement, 0.05, 0.2), "{check:?}");
    }

    #[test]
    fn a_part_off_the_kept_faces_is_not_checked() {
        let (study, mut part) = bar_part();
        // Moved 1 mm across: none of its faces lie on the bar's.
        for p in &mut part.positions {
            p[1] += 1.0;
        }
        let cancel = AtomicBool::new(false);
        let error = check_part(&study, &[part], &STEEL, &cancel, &|_, _| {}).unwrap_err();
        assert_eq!(
            error,
            CheckError::NotOnPart {
                what: Missing::Fixture,
                index: 0
            }
        );
    }
}
