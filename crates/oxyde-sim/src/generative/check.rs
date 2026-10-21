//! The check of a part made of an outcome (*Create Part*): its surface filled with ten-node
//! tetrahedra by the lattice mesher, the preserve bodies' faces found on it, each load case
//! of the study solved on it by the static solver, and, when the study limits it, its
//! lowest natural frequency found by the frequency study's solver (`crate::modal`). The outcome's numbers come from
//! voxels, whose corners concentrate stress and whose steps the smoothing and simplifying
//! take away; these are the part's own, to set against them (`docs/SIMULATION.md` §9).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

use super::GenerativeStudy;
use crate::bvh::Bvh;
use crate::material::Elastic;
use crate::math::{V3, add, cross, dot, normalize, scale, sub};
use crate::mesher::{MeshError, MeshOptions, SurfaceMesh, mesh_solids};
use crate::modal::{FrequencyStudy, solve_frequencies};
use crate::solve::{self, Load, LoadKind, SolveError, SolveOptions, Study};
use crate::topology::Shape;

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
    /// MPa: the largest von Mises stress over the load cases, but at the nodes of the faces
    /// the case's fixtures and loads act on (where stress is singular), as the outcome
    /// leaves out the voxels they act through.
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
/// [`super::solid_pieces`] gives them, one per solid), `material` the outcome's. `progress`
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
    let solids = labelled(study, pieces);
    if cancel.load(Ordering::Relaxed) {
        return Err(CheckError::Cancelled);
    }

    // The elements: a voxel, larger where the part would need too many.
    let volume: f64 = solids.iter().map(SurfaceMesh::volume).sum();
    let (lo, hi) = bounds(&solids);
    let box_volume: f64 = (0..3).map(|a| (hi[a] - lo[a]).max(voxel)).product();
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
            loads.push(load.clone());
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
        // Stress is singular where the case's fixtures and loads act: their faces' nodes
        // are left out of the largest stress.
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
        let mut singular = vec![false; mesh.nodes.len()];
        for facet in mesh.facets.iter().filter(|f| acting.contains(&f.label)) {
            let mids = mesh.facet_edge_nodes(facet).into_iter().flatten();
            for n in mesh.facet_corners(facet).into_iter().chain(mids) {
                if let Some(s) = singular.get_mut(n as usize) {
                    *s = true;
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

/// The pieces as the mesher takes them, each triangle labelled with the preserve body's
/// face it lies on, or [`crate::NO_LABEL`].
fn labelled(study: &GenerativeStudy, pieces: &[Shape]) -> Vec<SurfaceMesh> {
    let mut faces: Vec<[V3; 3]> = Vec::new();
    let mut labels: Vec<u32> = Vec::new();
    for s in &study.space.preserve {
        for (t, &label) in s.triangles.iter().zip(&s.labels) {
            let p = |i: u32| s.positions.get(i as usize).copied();
            if let (Some(a), Some(b), Some(c)) = (p(t[0]), p(t[1]), p(t[2])) {
                faces.push([a, b, c]);
                labels.push(label);
            }
        }
    }
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
                    label.unwrap_or(crate::NO_LABEL)
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
        lo = std::array::from_fn(|a| lo[a].min(p[a]));
        hi = std::array::from_fn(|a| hi[a].max(p[a]));
    }
    if lo[0] > hi[0] {
        return ([0.0; 3], [0.0; 3]);
    }
    (lo, hi)
}
