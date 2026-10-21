//! A solved case read back: the mesh, the velocity and pressure in every cell and on every
//! boundary face, and the goals' values.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::case::{Case, M_PER_MM};
use crate::cooling::{FLUID, KELVIN, P_REF, SOLID};
use crate::foam::{self, Field, FoamError};
use crate::mesh::PolyMesh;
use crate::study::{GoalKind, add, dot, norm, sub};

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
    /// A flow rate (m³/s, kg/s), a mean pressure (Pa), a temperature (°C) or a heat flow
    /// (W).
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
    /// A cooling study's fluid temperatures, °C: in each cell, and on each boundary face.
    pub temperature: Option<Vec<f64>>,
    pub boundary_temperature: Option<Vec<f64>>,
    /// A turbulent cooling study's turbulent thermal diffusivity on each boundary face,
    /// kg/(m·s) (the wall functions'): heat crosses a wall by (κ + cp αt) ∂T/∂n.
    pub boundary_alphat: Option<Vec<f64>>,
    /// A cooling study's solids.
    pub solid: Option<SolidResults>,
}

/// The temperatures in a cooling study's solids: one mesh holding every body.
#[derive(Debug, Clone, PartialEq)]
pub struct SolidResults {
    pub mesh: PolyMesh,
    /// Each cell's centre (mm) and volume (mm³).
    pub cell_centres: Vec<[f64; 3]>,
    pub cell_volumes: Vec<f64>,
    /// °C, in each cell and on each boundary face (in face order from the first).
    pub temperature: Vec<f64>,
    pub boundary_temperature: Vec<f64>,
    /// The model face each boundary face lies on.
    pub boundary_labels: Vec<Option<u32>>,
    /// Whether each boundary face is coupled to the fluid (a `wall_` patch).
    pub boundary_coupled: Vec<bool>,
    /// The body (the study's surface) each cell belongs to, from the bodies' zones.
    pub body: Vec<Option<usize>>,
}

impl FlowResults {
    /// Reads the last results of `case`.
    pub fn read(case: &Case) -> Result<Self, ReadError> {
        Self::read_folder(&case.dir, case.density)
    }

    /// Reads the last results in the case folder `dir`, of a fluid of `density` (kg/m³):
    /// what a results file points to. Each patch's model face comes from its name. A
    /// cooling case (its mesh split into a fluid and a solid region) is read with both.
    pub fn read_folder(dir: &Path, density: f64) -> Result<Self, ReadError> {
        if dir.join("constant").join(FLUID).join("polyMesh").is_dir() {
            return Self::read_cooling(dir, density);
        }
        let (time, _) = latest_time(dir).ok_or_else(|| ReadError::NoResults {
            dir: dir.to_path_buf(),
        })?;
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
        let pressure = kinematic.iter().map(|p| p * density).collect();
        let boundary_pressure = boundary_kinematic.iter().map(|p| p * density).collect();

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

        let boundary_labels = labels_of(&mesh);
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
            temperature: None,
            boundary_temperature: None,
            boundary_alphat: None,
            solid: None,
        })
    }

    /// A cooling case: the fluid region's flow and temperatures, and the solid's. Pressures
    /// there are absolute and fluxes are of mass.
    fn read_cooling(dir: &Path, density: f64) -> Result<Self, ReadError> {
        let (time, _) = latest_time(dir).ok_or_else(|| ReadError::NoResults {
            dir: dir.to_path_buf(),
        })?;
        let mesh = PolyMesh::read_region(dir, FLUID)?;
        let time_dir = dir.join(&time).join(FLUID);
        let file = |dir: &Path, name: &str| {
            let path = dir.join(name);
            fs::read(&path)
                .map(|text| (path.clone(), text))
                .map_err(|error| ReadError::Io { path, error })
        };
        let (u_path, u_text) = file(&time_dir, "U")?;
        let u = foam::vector_field(&u_text).map_err(|e| ReadError::parse(&u_path, e))?;
        let (p_path, p_text) = file(&time_dir, "p")?;
        let p = foam::scalar_field(&p_text).map_err(|e| ReadError::parse(&p_path, e))?;
        let (t_path, t_text) = file(&time_dir, "T")?;
        let t = foam::scalar_field(&t_text).map_err(|e| ReadError::parse(&t_path, e))?;
        let (velocity, boundary_velocity) = on_mesh(&mesh, &u, &u_path)?;
        let (absolute, boundary_absolute) = on_mesh(&mesh, &p, &p_path)?;
        let (kelvin, boundary_kelvin) = on_mesh(&mesh, &t, &t_path)?;
        let gauge = |v: Vec<f64>| v.into_iter().map(|p| p - P_REF).collect::<Vec<_>>();
        let celsius = |v: Vec<f64>| v.into_iter().map(|t| t - KELVIN).collect::<Vec<_>>();
        let phi_path = time_dir.join("phi");
        let boundary_flux = match fs::read(&phi_path) {
            Ok(text) => {
                let phi = foam::scalar_field(&text).map_err(|e| ReadError::parse(&phi_path, e))?;
                // Mass fluxes, kg/s.
                boundary_values(&mesh, &phi).map(|v| v.into_iter().map(|m| m / density).collect())
            }
            Err(_) => None,
        };
        let boundary_labels = labels_of(&mesh);
        let (cell_centres, cell_volumes) = mesh.cell_geometry();
        let alphat_path = time_dir.join("alphat");
        let boundary_alphat = match fs::read(&alphat_path) {
            Ok(text) => {
                let a = foam::scalar_field(&text).map_err(|e| ReadError::parse(&alphat_path, e))?;
                Some(on_mesh(&mesh, &a, &alphat_path)?.1)
            }
            Err(_) => None,
        };

        let solid_mesh = PolyMesh::read_region(dir, SOLID)?;
        let solid_dir = dir.join(&time).join(SOLID);
        let (st_path, st_text) = file(&solid_dir, "T")?;
        let st = foam::scalar_field(&st_text).map_err(|e| ReadError::parse(&st_path, e))?;
        let (solid_kelvin, solid_boundary_kelvin) = on_mesh(&solid_mesh, &st, &st_path)?;
        let mut body = vec![None; solid_mesh.cells];
        for (name, cells) in PolyMesh::cell_zones(dir, SOLID)? {
            let Some(k) = name
                .strip_prefix("body_")
                .and_then(|k| k.parse::<usize>().ok())
            else {
                continue;
            };
            for c in cells {
                if let Some(slot) = body.get_mut(c as usize) {
                    *slot = Some(k);
                }
            }
        }
        let (solid_centres, solid_volumes) = solid_mesh.cell_geometry();
        let first = solid_mesh.internal_faces();
        let mut boundary_coupled = vec![false; solid_mesh.faces() - first];
        for patch in solid_mesh
            .patches
            .iter()
            .filter(|p| p.name.starts_with("wall_"))
        {
            for f in patch.start..patch.start + patch.count {
                if let Some(slot) = boundary_coupled.get_mut(f - first) {
                    *slot = true;
                }
            }
        }
        let solid = SolidResults {
            boundary_labels: labels_of(&solid_mesh),
            boundary_coupled,
            cell_centres: solid_centres,
            cell_volumes: solid_volumes,
            temperature: celsius(solid_kelvin),
            boundary_temperature: celsius(solid_boundary_kelvin),
            body,
            mesh: solid_mesh,
        };
        Ok(Self {
            mesh,
            time,
            cell_centres,
            cell_volumes,
            velocity,
            pressure: gauge(absolute),
            boundary_velocity,
            boundary_pressure: gauge(boundary_absolute),
            boundary_flux,
            boundary_labels,
            temperature: Some(celsius(kelvin)),
            boundary_temperature: Some(celsius(boundary_kelvin)),
            boundary_alphat,
            solid: Some(solid),
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
                if goal.kind.is_thermal() {
                    return match self.thermal_goal(case, goal.kind, &goal.labels) {
                        Ok(value) => GoalValue::Scalar {
                            name,
                            kind: goal.kind,
                            value,
                        },
                        Err(reason) => missing(reason),
                    };
                }
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
                    GoalKind::MaxTemperature | GoalKind::MeanTemperature | GoalKind::HeatFlow => {
                        missing("not a flow's goal")
                    }
                }
            })
            .collect()
    }

    /// A temperature or heat goal of a cooling case on the faces of `labels`, measured in
    /// the solid: °C, or W leaving the solid.
    fn thermal_goal(
        &self,
        case: &Case,
        kind: GoalKind,
        labels: &[u32],
    ) -> Result<f64, &'static str> {
        let solid = self
            .solid
            .as_ref()
            .ok_or("only a cooling study measures heat")?;
        let study = case
            .cooling
            .as_deref()
            .ok_or("only a cooling study measures heat")?;
        let first = solid.mesh.internal_faces();
        let on: Vec<usize> = solid
            .boundary_labels
            .iter()
            .enumerate()
            .filter(|(_, l)| l.is_some_and(|l| labels.contains(&l)))
            .map(|(i, _)| first + i)
            .collect();
        match kind {
            GoalKind::MaxTemperature => {
                let body_of = study.body_of();
                let bodies: Vec<usize> = labels
                    .iter()
                    .filter_map(|l| body_of.get(l).copied())
                    .collect();
                let in_bodies = |c: usize| {
                    solid
                        .body
                        .get(c)
                        .copied()
                        .flatten()
                        .is_some_and(|b| bodies.contains(&b))
                };
                let cells = (0..solid.mesh.cells)
                    .filter(|&c| in_bodies(c))
                    .filter_map(|c| solid.temperature.get(c).copied());
                let faces = (first..solid.mesh.faces())
                    .filter(|&f| {
                        solid
                            .mesh
                            .owner
                            .get(f)
                            .is_some_and(|&c| in_bodies(c as usize))
                    })
                    .filter_map(|f| solid.boundary_temperature.get(f - first).copied());
                let hottest = cells.chain(faces).fold(f64::NEG_INFINITY, f64::max);
                if hottest.is_finite() {
                    Ok(hottest)
                } else {
                    Err("the mesh has none of its bodies")
                }
            }
            GoalKind::MeanTemperature => {
                let (mut sum, mut area) = (0.0, 0.0);
                for &f in &on {
                    let a = norm(solid.mesh.face_geometry(f).1);
                    sum += a * solid
                        .boundary_temperature
                        .get(f - first)
                        .copied()
                        .unwrap_or(0.0);
                    area += a;
                }
                if area > 0.0 {
                    Ok(sum / area)
                } else {
                    Err("the mesh has none of its faces")
                }
            }
            _ => {
                // Fourier's law across each face's half cell: what leaves the solid. Where
                // the fluid touches, on the fluid's side: a body that conducts well hardly
                // varies across its half cell, and its faces' temperatures are no measure
                // of so small a difference.
                let heat = study
                    .heat
                    .as_ref()
                    .ok_or("only a cooling study measures heat")?;
                if on.is_empty() {
                    return Err("the mesh has none of its faces");
                }
                let mut watts = self.heat_into_fluid(heat, labels);
                let coupled = |f: usize| {
                    solid
                        .boundary_coupled
                        .get(f - first)
                        .copied()
                        .unwrap_or(false)
                };
                for &f in on.iter().filter(|&&f| !coupled(f)) {
                    let Some(&cell) = solid.mesh.owner.get(f) else {
                        continue;
                    };
                    let cell = cell as usize;
                    let kappa = solid
                        .body
                        .get(cell)
                        .copied()
                        .flatten()
                        .and_then(|b| heat.solids.get(b))
                        .map_or(0.0, |s| s.conductivity);
                    let (centre, area) = solid.mesh.face_geometry(f);
                    let a = norm(area);
                    let Some(&c) = solid.cell_centres.get(cell) else {
                        continue;
                    };
                    let d = dot(sub(centre, c), area) / a.max(f64::MIN_POSITIVE);
                    let (Some(&tc), Some(&tf)) = (
                        solid.temperature.get(cell),
                        solid.boundary_temperature.get(f - first),
                    ) else {
                        continue;
                    };
                    if d > 0.0 {
                        // mm² / mm = mm: to metres.
                        watts += kappa * (tc - tf) * a / d * M_PER_MM;
                    }
                }
                Ok(watts)
            }
        }
    }
}

impl FlowResults {
    /// The heat entering the fluid through its walls on `labels`, W: (κ + cp αt) ∂T/∂n across
    /// each face's half cell.
    fn heat_into_fluid(&self, heat: &crate::study::Heat, labels: &[u32]) -> f64 {
        let (Some(cells), Some(faces)) = (&self.temperature, &self.boundary_temperature) else {
            return 0.0;
        };
        let first = self.mesh.internal_faces();
        let mut watts = 0.0;
        for patch in self
            .mesh
            .patches
            .iter()
            .filter(|p| p.name.starts_with("wall_"))
        {
            for f in patch.start..patch.start + patch.count {
                let on = self
                    .boundary_labels
                    .get(f - first)
                    .copied()
                    .flatten()
                    .is_some_and(|l| labels.contains(&l));
                let Some(&cell) = self.mesh.owner.get(f).filter(|_| on) else {
                    continue;
                };
                let cell = cell as usize;
                let (centre, area) = self.mesh.face_geometry(f);
                let a = norm(area);
                let (Some(&c), Some(&tc), Some(&tf)) = (
                    self.cell_centres.get(cell),
                    cells.get(cell),
                    faces.get(f - first),
                ) else {
                    continue;
                };
                let d = dot(sub(centre, c), area) / a.max(f64::MIN_POSITIVE);
                let alphat = self
                    .boundary_alphat
                    .as_ref()
                    .and_then(|v| v.get(f - first).copied())
                    .unwrap_or(0.0);
                let kappa = heat.conductivity + heat.specific_heat * alphat;
                if d > 0.0 {
                    watts += kappa * (tf - tc) * a / d * M_PER_MM;
                }
            }
        }
        watts
    }
}

/// The model face each boundary face of `mesh` lies on, from its patch's name.
fn labels_of(mesh: &PolyMesh) -> Vec<Option<u32>> {
    let first = mesh.internal_faces();
    let mut labels = vec![None; mesh.faces() - first];
    for patch in &mesh.patches {
        let label = label_of_patch(&patch.name);
        for f in patch.start..patch.start + patch.count {
            if let Some(slot) = labels.get_mut(f - first) {
                *slot = label;
            }
        }
    }
    labels
}

/// The model face a patch was made from, by its name ([`crate::case`] names them after
/// their role and label: `wall_12` is face 12; a cooling case's solid also has `free_3` and
/// `outer_15`); `None` for the domain's patch.
pub fn label_of_patch(name: &str) -> Option<u32> {
    let (role, label) = name.rsplit_once('_')?;
    if !matches!(
        role,
        "wall" | "inlet" | "outlet" | "slip" | "free" | "outer"
    ) {
        return None;
    }
    label.parse().ok()
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
/// `force.dat`, in the last folder it wrote (under the fluid's region in a cooling case).
fn forces(dir: &Path, object: &str) -> Result<([f64; 3], [f64; 3]), String> {
    let plain = dir.join("postProcessing").join(object);
    let base = if plain.is_dir() {
        plain
    } else {
        dir.join("postProcessing").join(FLUID).join(object)
    };
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
            internal: true,
            cooling: None,
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
    fn patches_name_their_faces() {
        assert_eq!(label_of_patch("wall_12"), Some(12));
        assert_eq!(label_of_patch("inlet_0"), Some(0));
        assert_eq!(label_of_patch("outlet_7"), Some(7));
        assert_eq!(label_of_patch("slip_3"), Some(3));
        assert_eq!(label_of_patch("free_8"), Some(8));
        assert_eq!(label_of_patch("outer_15"), Some(15));
        assert_eq!(label_of_patch("domain"), None);
        assert_eq!(label_of_patch("wall_x"), None);
        assert_eq!(label_of_patch("other_4"), None);
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
