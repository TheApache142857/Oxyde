//! A solved case read back: the mesh, the velocity and pressure in every cell and on every
//! boundary face, and the goals' values.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::case::{Case, M_PER_MM};
use crate::foam::{self, Field, FoamError};
use crate::mesh::PolyMesh;
use crate::study::{GoalKind, add, dot, norm};

/// Why results could not be read.
#[derive(Debug)]
pub enum ReadError {
    /// The case has no results: no time folder after the start.
    NoResults { dir: PathBuf },
    /// A file could not be read.
    Io { path: PathBuf, error: io::Error },
    /// A file is not what OpenFOAM writes.
    Parse { path: PathBuf, error: FoamError },
    /// The files do not fit together.
    Inconsistent { path: PathBuf, detail: String },
}

impl ReadError {
    pub(crate) fn parse(path: &Path, error: FoamError) -> Self {
        ReadError::Parse {
            path: path.to_path_buf(),
            error,
        }
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::NoResults { dir } => write!(f, "{} holds no results yet", dir.display()),
            ReadError::Io { path, error } => write!(f, "{}: {error}", path.display()),
            ReadError::Parse { path, error } => write!(f, "{}: {error}", path.display()),
            ReadError::Inconsistent { path, detail } => write!(f, "{}: {detail}", path.display()),
        }
    }
}

impl std::error::Error for ReadError {}

/// A goal's value.
#[derive(Debug, Clone, PartialEq)]
pub enum GoalValue {
    /// A flow rate (m³/s, kg/s) or a mean pressure (Pa).
    Scalar {
        name: String,
        kind: GoalKind,
        value: f64,
    },
    /// A force, N: the total and its pressure and friction parts.
    Force {
        name: String,
        total: [f64; 3],
        pressure: [f64; 3],
        viscous: [f64; 3],
    },
    /// The goal could not be measured.
    Missing { name: String, reason: String },
}

/// The flow in a solved case.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowResults {
    pub mesh: PolyMesh,
    /// The time folder read: the last one written.
    pub time: String,
    /// Each cell's centre (mm) and volume (mm³).
    pub cell_centres: Vec<[f64; 3]>,
    pub cell_volumes: Vec<f64>,
    /// Each cell's velocity (m/s) and gauge pressure (Pa).
    pub velocity: Vec<[f64; 3]>,
    pub pressure: Vec<f64>,
    /// The same on each boundary face, in face order from the first boundary face.
    pub boundary_velocity: Vec<[f64; 3]>,
    pub boundary_pressure: Vec<f64>,
    /// The volume flow out through each boundary face, m³/s, as the solver balanced it
    /// (its `phi`); `None` when the case has no such file.
    pub boundary_flux: Option<Vec<f64>>,
    /// The model face each boundary face lies on; `None` on the domain's box.
    pub boundary_labels: Vec<Option<u32>>,
}

impl FlowResults {
    /// Reads the last results of `case`.
    pub fn read(case: &Case) -> Result<Self, ReadError> {
        let dir = &case.dir;
        let (time, _) =
            latest_time(dir).ok_or_else(|| ReadError::NoResults { dir: dir.clone() })?;
        let mesh = PolyMesh::read(dir)?;
        let time_dir = dir.join(&time);
        let file = |name: &str| {
            let path = time_dir.join(name);
            fs::read(&path)
                .map(|text| (path.clone(), text))
                .map_err(|error| ReadError::Io { path, error })
        };
        let (u_path, u_text) = file("U")?;
        let u = foam::vector_field(&u_text).map_err(|e| ReadError::parse(&u_path, e))?;
        let (p_path, p_text) = file("p")?;
        let p = foam::scalar_field(&p_text).map_err(|e| ReadError::parse(&p_path, e))?;

        let (velocity, boundary_velocity) = on_mesh(&mesh, &u, &u_path)?;
        let (kinematic, boundary_kinematic) = on_mesh(&mesh, &p, &p_path)?;
        // Incompressible solvers hold the pressure divided by the density.
        let pressure = kinematic.iter().map(|p| p * case.density).collect();
        let boundary_pressure = boundary_kinematic
            .iter()
            .map(|p| p * case.density)
            .collect();

        // The fluxes the solver balanced: exact where velocities on the boundary are only
        // their cells' (an outlet's).
        let phi_path = time_dir.join("phi");
        let boundary_flux = match fs::read(&phi_path) {
            Ok(text) => {
                let phi = foam::scalar_field(&text).map_err(|e| ReadError::parse(&phi_path, e))?;
                boundary_values(&mesh, &phi)
            }
            Err(_) => None,
        };

        let first = mesh.internal_faces();
        let mut boundary_labels = vec![None; mesh.faces() - first];
        for patch in &mesh.patches {
            let label = case.label_of(&patch.name);
            for f in patch.start..patch.start + patch.count {
                if let Some(slot) = boundary_labels.get_mut(f - first) {
                    *slot = label;
                }
            }
        }
        let (cell_centres, cell_volumes) = mesh.cell_geometry();
        Ok(Self {
            mesh,
            time,
            cell_centres,
            cell_volumes,
            velocity,
            pressure,
            boundary_velocity,
            boundary_pressure,
            boundary_flux,
            boundary_labels,
        })
    }

    /// The boundary faces (mesh face numbers) on any of `labels`.
    fn faces_on<'a>(&'a self, labels: &'a [u32]) -> impl Iterator<Item = usize> + 'a {
        let first = self.mesh.internal_faces();
        self.boundary_labels
            .iter()
            .enumerate()
            .filter(move |(_, l)| l.is_some_and(|l| labels.contains(&l)))
            .map(move |(i, _)| first + i)
    }

    /// The volume flow out of the fluid through the faces of `labels`, m³/s; negative
    /// where it comes in.
    pub fn volume_flow(&self, labels: &[u32]) -> f64 {
        let first = self.mesh.internal_faces();
        if let Some(flux) = &self.boundary_flux {
            return self
                .faces_on(labels)
                .filter_map(|f| flux.get(f - first))
                .sum();
        }
        self.faces_on(labels)
            .map(|f| {
                let (_, area) = self.mesh.face_geometry(f);
                let u = self
                    .boundary_velocity
                    .get(f - first)
                    .copied()
                    .unwrap_or_default();
                dot(u, area) * M_PER_MM * M_PER_MM
            })
            .sum()
    }

    /// The area-weighted mean gauge pressure on the faces of `labels`, Pa; `None` when
    /// the mesh has none of their faces.
    pub fn mean_pressure(&self, labels: &[u32]) -> Option<f64> {
        let first = self.mesh.internal_faces();
        let (mut sum, mut area) = (0.0, 0.0);
        for f in self.faces_on(labels) {
            let a = norm(self.mesh.face_geometry(f).1);
            sum += a * self
                .boundary_pressure
                .get(f - first)
                .copied()
                .unwrap_or(0.0);
            area += a;
        }
        (area > 0.0).then(|| sum / area)
    }

    /// The area of the mesh's faces on `labels`, m²: how well the mesh covers them.
    pub fn area(&self, labels: &[u32]) -> f64 {
        self.faces_on(labels)
            .map(|f| norm(self.mesh.face_geometry(f).1) * M_PER_MM * M_PER_MM)
            .sum()
    }

    /// The value of every goal of `case`.
    pub fn goals(&self, case: &Case) -> Vec<GoalValue> {
        case.goals
            .iter()
            .map(|goal| {
                let name = goal.name.clone();
                let missing = |reason: &str| GoalValue::Missing {
                    name: name.clone(),
                    reason: reason.to_string(),
                };
                if self.area(&goal.labels) <= 0.0 {
                    return missing("the mesh has none of its faces");
                }
                match goal.kind {
                    GoalKind::Force => match goal.object.as_deref().map(|o| forces(&case.dir, o)) {
                        Some(Ok((pressure, viscous))) => GoalValue::Force {
                            name,
                            total: add(pressure, viscous),
                            pressure,
                            viscous,
                        },
                        Some(Err(reason)) => missing(&reason),
                        None => missing("no forces were written"),
                    },
                    GoalKind::VolumeFlow => GoalValue::Scalar {
                        name,
                        kind: goal.kind,
                        value: self.volume_flow(&goal.labels),
                    },
                    GoalKind::MassFlow => GoalValue::Scalar {
                        name,
                        kind: goal.kind,
                        value: self.volume_flow(&goal.labels) * case.density,
                    },
                    GoalKind::MeanPressure => match self.mean_pressure(&goal.labels) {
                        Some(value) => GoalValue::Scalar {
                            name,
                            kind: goal.kind,
                            value,
                        },
                        None => missing("the mesh has none of its faces"),
                    },
                }
            })
            .collect()
    }
}

/// The last time folder of a case after the start (`0`), by its name and time.
pub fn latest_time(dir: &Path) -> Option<(String, f64)> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let time = name.parse::<f64>().ok()?;
            (time > 0.0 && time.is_finite()).then_some((name, time))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// A field's values in the cells and on the boundary faces, in face order. A patch whose
/// values the file does not hold (zero gradient) takes its cells'.
fn on_mesh<T: Clone + Default>(
    mesh: &PolyMesh,
    field: &Field<T>,
    path: &Path,
) -> Result<(Vec<T>, Vec<T>), ReadError> {
    let inconsistent = |detail: String| ReadError::Inconsistent {
        path: path.to_path_buf(),
        detail,
    };
    let cells = field.internal.expand(mesh.cells).ok_or_else(|| {
        inconsistent(format!(
            "its cell values do not number the mesh's {} cells",
            mesh.cells
        ))
    })?;
    let first = mesh.internal_faces();
    let mut boundary = vec![T::default(); mesh.faces() - first];
    for patch in &mesh.patches {
        let given = field
            .boundary
            .iter()
            .find(|p| p.name == patch.name)
            .and_then(|p| p.value.as_ref())
            .and_then(|v| v.expand(patch.count));
        for (i, f) in (patch.start..patch.start + patch.count).enumerate() {
            let value = match &given {
                Some(values) => values.get(i).cloned(),
                None => mesh
                    .owner
                    .get(f)
                    .and_then(|&c| cells.get(c as usize))
                    .cloned(),
            };
            if let (Some(slot), Some(value)) = (boundary.get_mut(f - first), value) {
                *slot = value;
            }
        }
    }
    Ok((cells, boundary))
}

/// A face field's values on the boundary faces, in face order; `None` when a patch with
/// faces has none in the file.
fn boundary_values(mesh: &PolyMesh, field: &Field<f64>) -> Option<Vec<f64>> {
    let first = mesh.internal_faces();
    let mut boundary = vec![0.0; mesh.faces() - first];
    for patch in &mesh.patches {
        if patch.count == 0 {
            continue;
        }
        let values = field
            .boundary
            .iter()
            .find(|p| p.name == patch.name)?
            .value
            .as_ref()?
            .expand(patch.count)?;
        for (value, f) in values.into_iter().zip(patch.start..) {
            *boundary.get_mut(f - first)? = value;
        }
    }
    Some(boundary)
}

/// The pressure and friction forces of a `forces` function object, N: the last line of its
/// `force.dat`, in the last folder it wrote.
fn forces(dir: &Path, object: &str) -> Result<([f64; 3], [f64; 3]), String> {
    let base = dir.join("postProcessing").join(object);
    let (start, _) = latest_time_any(&base).ok_or("no forces were written")?;
    let path = base.join(start).join("force.dat");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .ok_or("the forces file is empty")?;
    // `time total(3) pressure(3) viscous(3)`; older versions bracket them without the total.
    let numbers: Vec<f64> = line
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .filter(|w| !w.is_empty())
        .filter_map(|w| w.parse().ok())
        .collect();
    let has_total = text
        .lines()
        .any(|l| l.starts_with('#') && l.contains("total_x"));
    let at = |i: usize| -> Option<[f64; 3]> {
        Some([*numbers.get(i)?, *numbers.get(i + 1)?, *numbers.get(i + 2)?])
    };
    let (pressure, viscous) = if has_total {
        (at(4), at(7))
    } else {
        (at(1), at(4))
    };
    match (pressure, viscous) {
        (Some(p), Some(v)) => Ok((p, v)),
        _ => Err(format!("the forces file has an unknown layout: {line}")),
    }
}

/// The last time folder in `dir`, including `0`.
fn latest_time_any(dir: &Path) -> Option<(String, f64)> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let time = name.parse::<f64>().ok()?;
            time.is_finite().then_some((name, time))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::CaseGoal;
    use crate::case::{FacePatch, Role};
    use crate::mesh::tests::write_two_cubes;

    fn case(dir: &Path) -> Case {
        Case {
            dir: dir.to_path_buf(),
            patches: vec![
                FacePatch {
                    name: "inlet_0".to_string(),
                    label: 0,
                    role: Role::Inlet,
                    area: 1.0,
                },
                FacePatch {
                    name: "wall_1".to_string(),
                    label: 1,
                    role: Role::Wall,
                    area: 9.0,
                },
            ],
            solver: "simpleFoam".to_string(),
            processes: 1,
            iterations: 10,
            density: 1000.0,
            turbulent: false,
            goals: vec![
                CaseGoal {
                    name: "In".to_string(),
                    kind: GoalKind::VolumeFlow,
                    labels: vec![0],
                    object: None,
                },
                CaseGoal {
                    name: "Inlet pressure".to_string(),
                    kind: GoalKind::MeanPressure,
                    labels: vec![0],
                    object: None,
                },
                CaseGoal {
                    name: "Drag".to_string(),
                    kind: GoalKind::Force,
                    labels: vec![1],
                    object: Some("force_2".to_string()),
                },
            ],
        }
    }

    /// Two 1 m cubes; 2 m/s coming in through the inlet (x = 0); the kinematic pressure
    /// 0.3 and 0.1 in the cells, the walls taking their cells' (zero gradient).
    fn write_results(dir: &Path) {
        write_two_cubes(dir, false);
        let t = dir.join("12");
        fs::create_dir_all(&t).unwrap();
        fs::create_dir_all(dir.join("0")).unwrap();
        fs::write(
            t.join("U"),
            "FoamFile { format ascii; class volVectorField; object U; }
internalField nonuniform List<vector> 2((2 0 0) (2 0 0));
boundaryField
{
    domain { type noSlip; }
    inlet_0 { type fixedValue; value uniform (2 0 0); }
    wall_1 { type noSlip; value uniform (0 0 0); }
}",
        )
        .unwrap();
        fs::write(
            t.join("p"),
            "FoamFile { format ascii; class volScalarField; object p; }
internalField nonuniform List<scalar> 2(0.3 0.1);
boundaryField
{
    domain { type zeroGradient; }
    inlet_0 { type zeroGradient; }
    wall_1 { type zeroGradient; }
}",
        )
        .unwrap();
        fs::write(
            t.join("phi"),
            "FoamFile { format ascii; class surfaceScalarField; object phi; }
internalField nonuniform List<scalar> 1(1.9);
boundaryField
{
    domain { type calculated; value nonuniform List<scalar> 0(); }
    inlet_0 { type calculated; value uniform -1.9; }
    wall_1 { type calculated; value uniform 0; }
}",
        )
        .unwrap();
        let forces = dir.join("postProcessing/force_2/0");
        fs::create_dir_all(&forces).unwrap();
        fs::write(
            forces.join("force.dat"),
            "# Force\n# CofR : (0 0 0)\n#\n# Time total_x total_y total_z pressure_x pressure_y pressure_z viscous_x viscous_y viscous_z\n\
             11 1 0 0 0.9 0 0 0.1 0 0\n12 2 0 0 1.5 0 0 0.5 0 0\n",
        )
        .unwrap();
    }

    #[test]
    fn results_are_read_onto_the_mesh() {
        let dir = std::env::temp_dir().join(format!("oxyde-flow-results-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_results(&dir);
        let case = case(&dir);
        let r = FlowResults::read(&case).unwrap();
        assert_eq!(r.time, "12");
        assert_eq!(r.pressure, vec![300.0, 100.0]);
        assert_eq!(r.boundary_labels.len(), 10);
        assert_eq!(r.boundary_labels[0], Some(0));
        assert_eq!(r.boundary_labels[1], Some(1));
        // The wall at x = 2 belongs to the second cell.
        assert_eq!(r.boundary_pressure[1], 100.0);

        // The solver's own flux through the inlet: 1.9 m³/s in, where the cells' 2 m/s
        // over 1 m² would make 2.
        assert!((r.volume_flow(&[0]) + 1.9).abs() < 1e-9);
        let mut without = r.clone();
        without.boundary_flux = None;
        assert!((without.volume_flow(&[0]) + 2.0).abs() < 1e-9);
        assert!((r.area(&[1]) - 9.0).abs() < 1e-9);
        let goals = r.goals(&case);
        assert_eq!(
            goals[0],
            GoalValue::Scalar {
                name: "In".to_string(),
                kind: GoalKind::VolumeFlow,
                value: -1.9
            }
        );
        assert!(
            matches!(goals[1], GoalValue::Scalar { value, .. } if (value - 300.0).abs() < 1e-9)
        );
        assert_eq!(
            goals[2],
            GoalValue::Force {
                name: "Drag".to_string(),
                total: [2.0, 0.0, 0.0],
                pressure: [1.5, 0.0, 0.0],
                viscous: [0.5, 0.0, 0.0],
            }
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_case_without_results_says_so() {
        let dir = std::env::temp_dir().join(format!("oxyde-flow-noresults-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_two_cubes(&dir, false);
        fs::create_dir_all(dir.join("0")).unwrap();
        assert!(matches!(
            FlowResults::read(&case(&dir)),
            Err(ReadError::NoResults { .. })
        ));
        let _ = fs::remove_dir_all(&dir);
    }
}
