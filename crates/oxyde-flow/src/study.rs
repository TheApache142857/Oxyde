//! A flow study as plain data: what the application hands to [`crate::write_case`].
//!
//! Positions and lengths are in millimetres, as everywhere in Oxyde; everything else is SI.
//! Faces are named by labels (plain numbers): the application keeps the table from labels
//! to persistent face names, as it does for the static and thermal studies.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A closed triangle mesh bounding the fluid: a solid of the model, or a lid closing an
/// opening, each triangle labelled with the model face it lies on.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Surface {
    /// Corner positions, mm.
    pub positions: Vec<[f64; 3]>,
    /// The corners of each triangle, counter-clockwise seen from outside the solid.
    pub triangles: Vec<[u32; 3]>,
    /// The model face each triangle lies on, one per triangle.
    pub labels: Vec<u32>,
}

impl Surface {
    /// The corners of triangle `t`, or `None` when it or one of its corners does not exist.
    pub fn corners(&self, t: usize) -> Option<[[f64; 3]; 3]> {
        let [a, b, c] = *self.triangles.get(t)?;
        Some([
            *self.positions.get(a as usize)?,
            *self.positions.get(b as usize)?,
            *self.positions.get(c as usize)?,
        ])
    }

    /// The area of each label's triangles, mm².
    pub fn areas(&self) -> BTreeMap<u32, f64> {
        let mut areas = BTreeMap::new();
        for (t, &label) in self.labels.iter().enumerate() {
            if let Some([a, b, c]) = self.corners(t) {
                let n = cross(sub(b, a), sub(c, a));
                *areas.entry(label).or_insert(0.0) += 0.5 * norm(n);
            }
        }
        areas
    }

    /// The smallest box holding every corner, mm; `None` for a surface without corners.
    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        let first = *self.positions.first()?;
        let mut min = first;
        let mut max = first;
        for &p in &self.positions {
            min = zip(min, p, f64::min);
            max = zip(max, p, f64::max);
        }
        Some((min, max))
    }
}

/// Where the fluid is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlowType {
    /// The fluid fills the cavity of the model that holds `point` (mm), closed by lids where
    /// it enters and leaves. The point must lie at least a cell away from every surface.
    Internal { point: [f64; 3] },
    /// The fluid flows around the model, through the computational domain, at `velocity`
    /// (m/s) far from it.
    External { velocity: [f64; 3] },
}

/// The fluid, with constant properties: the flow is incompressible.
#[derive(Debug, Clone, PartialEq)]
pub struct Fluid {
    /// Its name, for reports.
    pub name: String,
    /// Density, kg/m³.
    pub density: f64,
    /// Dynamic viscosity, Pa·s.
    pub viscosity: f64,
}

impl Fluid {
    /// Air at 20 °C and 101 325 Pa.
    pub fn air() -> Self {
        Self {
            name: "Air".to_string(),
            density: 1.204,
            viscosity: 1.825e-5,
        }
    }

    /// Water at 20 °C.
    pub fn water() -> Self {
        Self {
            name: "Water".to_string(),
            density: 998.2,
            viscosity: 1.002e-3,
        }
    }

    /// Kinematic viscosity, m²/s: what OpenFOAM's incompressible solvers take.
    pub fn kinematic_viscosity(&self) -> f64 {
        self.viscosity / self.density
    }
}

/// Laminar or turbulent flow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Turbulence {
    /// No turbulence model.
    Laminar,
    /// Reynolds-averaged turbulence with Menter's k-ω SST model, and wall functions that
    /// accept any distance from the wall to the first cell (Spalding's law). `intensity`
    /// (0.05 for 5 %) and `length` (mm) describe the turbulence of the fluid coming in.
    KOmegaSst { intensity: f64, length: f64 },
}

/// A boundary condition on model faces. Faces in no condition are walls the fluid sticks
/// to (no slip), as SolidWorks Flow Simulation has them.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub kind: ConditionKind,
    /// The faces it acts on.
    pub labels: Vec<u32>,
}

/// What a [`Condition`] holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConditionKind {
    /// Fluid entering normal to the faces at `speed`, m/s.
    InletVelocity { speed: f64 },
    /// Fluid entering through the faces at `rate`, m³/s, shared between them by area.
    InletVolumeFlow { rate: f64 },
    /// Fluid entering through the faces at `rate`, kg/s, shared between them by area.
    InletMassFlow { rate: f64 },
    /// An opening at a static `pressure`, Pa (gauge): the fluid leaves through it, or comes
    /// back in where it must.
    OutletPressure { pressure: f64 },
    /// A wall the fluid slips along without friction (SolidWorks' *ideal wall*); also a
    /// plane of symmetry.
    IdealWall,
}

/// A value the study reports at the end, as SolidWorks' surface goals.
#[derive(Debug, Clone, PartialEq)]
pub struct Goal {
    /// Its name, for reports.
    pub name: String,
    pub kind: GoalKind,
    /// The faces it is measured on.
    pub labels: Vec<u32>,
}

/// What a [`Goal`] measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalKind {
    /// The force the fluid exerts on the faces, N: pressure and friction.
    Force,
    /// The volume flow out of the fluid through the faces, m³/s; negative where it enters.
    VolumeFlow,
    /// The mass flow out of the fluid through the faces, kg/s; negative where it enters.
    MassFlow,
    /// The area-weighted mean static pressure on the faces, Pa (gauge).
    MeanPressure,
}

/// The computational domain: a box, mm, cut into the background mesh's cubes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Domain {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Domain {
    /// The domain of an internal flow: the model's box `(min, max)` with `margin` (mm) all
    /// round, so that the cavity lies inside it.
    pub fn internal(bounds: ([f64; 3], [f64; 3]), margin: f64) -> Self {
        let (min, max) = bounds;
        Self {
            min: [min[0] - margin, min[1] - margin, min[2] - margin],
            max: [max[0] + margin, max[1] + margin, max[2] + margin],
        }
    }

    /// The domain of an external flow around the model's box `(min, max)`: twice the
    /// model's largest size around it, and five times downstream of it, following the
    /// direction of `velocity`.
    pub fn external(bounds: ([f64; 3], [f64; 3]), velocity: [f64; 3]) -> Self {
        let (min, max) = bounds;
        let size = sub(max, min).into_iter().fold(0.0, f64::max);
        let speed = norm(velocity);
        // The flow's direction, each component from −1 to 1.
        let along = velocity.map(|v| if speed > 0.0 { v / speed } else { 0.0 });
        Self {
            min: zip(min, along, |m, a| m - size * (2.0 + 3.0 * (-a).max(0.0))),
            max: zip(max, along, |m, a| m + size * (2.0 + 3.0 * a.max(0.0))),
        }
    }

    /// Whether `p` lies inside the box.
    pub fn contains(&self, p: [f64; 3]) -> bool {
        all(p, self.min, |p, lo| p > lo) && all(p, self.max, |p, hi| p < hi)
    }
}

/// How the fluid is meshed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshSettings {
    /// The edge of the background mesh's cubes, mm.
    pub cell_size: f64,
    /// How many times the cells along the model's surfaces are halved.
    pub surface_level: u32,
    /// How many times the cells along the model's sharp edges are halved.
    pub feature_level: u32,
    /// Thin layers of cells grown along walls; 0 for none.
    pub layers: u32,
    /// The most cells the mesh may have, which bounds the memory a run takes.
    pub max_cells: u64,
}

impl MeshSettings {
    /// Cubes of `cell_size` (mm), halved once along surfaces and edges, no layers, and at
    /// most two million cells.
    pub fn new(cell_size: f64) -> Self {
        Self {
            cell_size,
            surface_level: 1,
            feature_level: 1,
            layers: 0,
            max_cells: 2_000_000,
        }
    }

    /// The edge of the finest cells, mm.
    pub fn finest(&self) -> f64 {
        let level = self.surface_level.max(self.feature_level).min(30);
        self.cell_size / f64::from(1u32 << level)
    }
}

/// How the flow is solved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolverSettings {
    /// The most iterations.
    pub iterations: u32,
    /// The flow has converged when every equation's initial residual falls under this.
    pub residual: f64,
    /// Processes the solver runs on, through MPI; 1 runs it as one process.
    pub processes: u32,
}

impl Default for SolverSettings {
    fn default() -> Self {
        Self {
            iterations: 1000,
            residual: 1e-4,
            processes: 1,
        }
    }
}

/// A flow study: everything a case is written from.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowStudy {
    /// The closed surfaces bounding the fluid: the model's solids and any lids.
    pub surfaces: Vec<Surface>,
    pub flow_type: FlowType,
    pub domain: Domain,
    pub fluid: Fluid,
    pub turbulence: Turbulence,
    pub conditions: Vec<Condition>,
    pub goals: Vec<Goal>,
    pub mesh: MeshSettings,
    pub solver: SolverSettings,
}

impl FlowStudy {
    /// Every label carried by a triangle, in order.
    pub fn labels(&self) -> BTreeSet<u32> {
        self.surfaces
            .iter()
            .flat_map(|s| s.labels.iter().copied())
            .collect()
    }

    /// The smallest box holding every surface, mm.
    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        let mut boxes = self.surfaces.iter().filter_map(Surface::bounds);
        let (mut min, mut max) = boxes.next()?;
        for (lo, hi) in boxes {
            min = zip(min, lo, f64::min);
            max = zip(max, hi, f64::max);
        }
        Some((min, max))
    }

    /// The area of each label's triangles over every surface, mm².
    pub fn areas(&self) -> BTreeMap<u32, f64> {
        let mut areas = BTreeMap::new();
        for surface in &self.surfaces {
            for (label, area) in surface.areas() {
                *areas.entry(label).or_insert(0.0) += area;
            }
        }
        areas
    }

    /// The condition acting on each label; labels in none are walls.
    pub fn condition_of(&self) -> BTreeMap<u32, usize> {
        let mut of = BTreeMap::new();
        for (index, condition) in self.conditions.iter().enumerate() {
            for &label in &condition.labels {
                of.entry(label).or_insert(index);
            }
        }
        of
    }

    /// The background mesh's cubes along each axis.
    pub fn background_cells(&self) -> [u64; 3] {
        let h = self.mesh.cell_size;
        sub(self.domain.max, self.domain.min).map(|extent| {
            if extent.is_finite() && h > 0.0 {
                // Saturating: an absurd ratio is caught by the cell limit.
                ((extent / h).ceil() as u64).max(1)
            } else {
                1
            }
        })
    }

    /// Checks that a case can be written from the study, with a message for the user when
    /// it cannot.
    pub fn check(&self) -> Result<(), StudyError> {
        if self.surfaces.is_empty() || self.surfaces.iter().all(|s| s.triangles.is_empty()) {
            return Err(StudyError::NoSurface);
        }
        for (index, surface) in self.surfaces.iter().enumerate() {
            let corners_ok = (0..surface.triangles.len()).all(|t| surface.corners(t).is_some());
            let finite = surface
                .positions
                .iter()
                .all(|p| p.iter().all(|x| x.is_finite()));
            if surface.labels.len() != surface.triangles.len() || !corners_ok || !finite {
                return Err(StudyError::BadSurface { surface: index });
            }
        }
        let labels = self.labels();
        let mut taken = BTreeSet::new();
        for condition in &self.conditions {
            if condition.labels.is_empty() {
                return Err(StudyError::NoFaces);
            }
            for &label in &condition.labels {
                if !labels.contains(&label) {
                    return Err(StudyError::UnknownLabel { label });
                }
                if !taken.insert(label) {
                    return Err(StudyError::LabelInTwoConditions { label });
                }
            }
            let value_ok = match condition.kind {
                ConditionKind::InletVelocity { speed } => speed.is_finite() && speed >= 0.0,
                ConditionKind::InletVolumeFlow { rate } | ConditionKind::InletMassFlow { rate } => {
                    rate.is_finite() && rate >= 0.0
                }
                ConditionKind::OutletPressure { pressure } => pressure.is_finite(),
                ConditionKind::IdealWall => true,
            };
            if !value_ok {
                return Err(StudyError::BadValue {
                    what: "a condition's value",
                });
            }
        }
        for goal in &self.goals {
            if goal.labels.is_empty() {
                return Err(StudyError::NoFaces);
            }
            if let Some(&label) = goal.labels.iter().find(|l| !labels.contains(l)) {
                return Err(StudyError::UnknownLabel { label });
            }
        }
        let fluid = &self.fluid;
        if !(fluid.density.is_finite() && fluid.density > 0.0)
            || !(fluid.viscosity.is_finite() && fluid.viscosity > 0.0)
        {
            return Err(StudyError::BadValue {
                what: "the fluid's density or viscosity",
            });
        }
        let turbulence_ok = match self.turbulence {
            Turbulence::Laminar => true,
            Turbulence::KOmegaSst { intensity, length } => {
                intensity.is_finite() && intensity > 0.0 && length.is_finite() && length > 0.0
            }
        };
        if !turbulence_ok {
            return Err(StudyError::BadValue {
                what: "the turbulence intensity or length",
            });
        }
        let domain = &self.domain;
        if !all(domain.min, domain.max, |lo, hi| {
            lo.is_finite() && hi.is_finite() && lo < hi
        }) {
            return Err(StudyError::BadDomain);
        }
        let mesh = &self.mesh;
        if !(mesh.cell_size.is_finite() && mesh.cell_size > 0.0) || mesh.max_cells == 0 {
            return Err(StudyError::BadValue {
                what: "the mesh's cell size",
            });
        }
        if mesh.surface_level > 10 || mesh.feature_level > 10 || mesh.layers > 20 {
            return Err(StudyError::BadValue {
                what: "the mesh's refinement",
            });
        }
        let solver = &self.solver;
        if solver.iterations == 0
            || solver.processes == 0
            || !(solver.residual.is_finite() && solver.residual > 0.0)
        {
            return Err(StudyError::BadValue {
                what: "the solver's settings",
            });
        }
        let cells = self
            .background_cells()
            .iter()
            .fold(1u64, |a, &n| a.saturating_mul(n));
        if cells > mesh.max_cells {
            return Err(StudyError::TooManyCells {
                cells,
                max: mesh.max_cells,
            });
        }
        match self.flow_type {
            FlowType::Internal { point } => {
                if !domain.contains(point) {
                    return Err(StudyError::PointOutsideDomain);
                }
                let has_pressure = self
                    .conditions
                    .iter()
                    .any(|c| matches!(c.kind, ConditionKind::OutletPressure { .. }));
                if !has_pressure {
                    return Err(StudyError::NoPressureOpening);
                }
            }
            FlowType::External { velocity } => {
                if !velocity.iter().all(|v| v.is_finite()) {
                    return Err(StudyError::BadValue {
                        what: "the free stream's velocity",
                    });
                }
                // The point meshed from sits in the domain's corner cell: the model must leave
                // a cell of margin on every side.
                if let Some((min, max)) = self.bounds() {
                    let margin = mesh.cell_size;
                    let inside = all(min, domain.min, |m, d| m > d + margin)
                        && all(max, domain.max, |m, d| m < d - margin);
                    if !inside {
                        return Err(StudyError::ModelOutsideDomain);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Why a case cannot be written from a study.
#[derive(Debug, Clone, PartialEq)]
pub enum StudyError {
    /// There is no surface to mesh.
    NoSurface,
    /// A surface has a triangle without a label or a corner that does not exist, or a
    /// position that is not a number.
    BadSurface { surface: usize },
    /// A condition or goal has no face.
    NoFaces,
    /// A condition or goal names a face no surface has.
    UnknownLabel { label: u32 },
    /// A face is in two conditions.
    LabelInTwoConditions { label: u32 },
    /// An internal flow without an opening at a pressure: its pressure level is undefined.
    NoPressureOpening,
    /// A value is out of range.
    BadValue { what: &'static str },
    /// The computational domain is not a box.
    BadDomain,
    /// The point inside an internal flow's cavity is outside the computational domain.
    PointOutsideDomain,
    /// The model does not fit inside an external flow's computational domain.
    ModelOutsideDomain,
    /// The background mesh alone would have more cells than allowed.
    TooManyCells { cells: u64, max: u64 },
}

impl fmt::Display for StudyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StudyError::NoSurface => write!(f, "there is nothing to put in the flow"),
            StudyError::BadSurface { surface } => {
                write!(
                    f,
                    "surface {} of the model is not a valid mesh",
                    surface + 1
                )
            }
            StudyError::NoFaces => write!(f, "a condition or a goal has no face"),
            StudyError::UnknownLabel { label } => {
                write!(f, "face {label} of a condition or goal is not in the model")
            }
            StudyError::LabelInTwoConditions { label } => {
                write!(f, "face {label} is in two conditions")
            }
            StudyError::NoPressureOpening => write!(
                f,
                "an internal flow needs an opening at a pressure (an outlet) on at least one face"
            ),
            StudyError::BadValue { what } => write!(f, "{what} is out of range"),
            StudyError::BadDomain => write!(f, "the computational domain is not a box"),
            StudyError::PointOutsideDomain => write!(
                f,
                "the point inside the fluid is outside the computational domain"
            ),
            StudyError::ModelOutsideDomain => write!(
                f,
                "the model does not fit inside the computational domain with a cell to spare"
            ),
            StudyError::TooManyCells { cells, max } => write!(
                f,
                "the computational domain would start with {cells} cells, more than the {max} allowed: \
                 make the cells larger or the domain smaller"
            ),
        }
    }
}

impl std::error::Error for StudyError {}

/// `f` of each pair of components.
pub(crate) fn zip(a: [f64; 3], b: [f64; 3], f: impl Fn(f64, f64) -> f64) -> [f64; 3] {
    let ([a0, a1, a2], [b0, b1, b2]) = (a, b);
    [f(a0, b0), f(a1, b1), f(a2, b2)]
}

/// Whether `f` holds for each pair of components.
pub(crate) fn all(a: [f64; 3], b: [f64; 3], f: impl Fn(f64, f64) -> bool) -> bool {
    let ([a0, a1, a2], [b0, b1, b2]) = (a, b);
    f(a0, b0) && f(a1, b1) && f(a2, b2)
}

pub(crate) fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub(crate) fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub(crate) fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// A cube of `size` (mm) around `centre`, with one label per side: 0 −x, 1 +x, 2 −y, 3 +y, 4 −z, 5 +z.
#[cfg(test)]
pub(crate) fn cube(size: f64, centre: [f64; 3]) -> Surface {
    let h = size / 2.0;
    let mut positions = Vec::new();
    for i in 0..8u32 {
        let x = if i & 1 == 0 { -h } else { h };
        let y = if i & 2 == 0 { -h } else { h };
        let z = if i & 4 == 0 { -h } else { h };
        positions.push([centre[0] + x, centre[1] + y, centre[2] + z]);
    }
    // Each side as two triangles, counter-clockwise seen from outside.
    let quads: [([u32; 4], u32); 6] = [
        ([0, 4, 6, 2], 0),
        ([1, 3, 7, 5], 1),
        ([0, 1, 5, 4], 2),
        ([2, 6, 7, 3], 3),
        ([0, 2, 3, 1], 4),
        ([4, 5, 7, 6], 5),
    ];
    let mut triangles = Vec::new();
    let mut labels = Vec::new();
    for (q, label) in quads {
        triangles.push([q[0], q[1], q[2]]);
        triangles.push([q[0], q[2], q[3]]);
        labels.extend([label, label]);
    }
    Surface {
        positions,
        triangles,
        labels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn study() -> FlowStudy {
        let surface = cube(10.0, [0.0; 3]);
        let bounds = surface.bounds().unwrap();
        FlowStudy {
            surfaces: vec![surface],
            flow_type: FlowType::Internal { point: [0.0; 3] },
            domain: Domain::internal(bounds, 2.0),
            fluid: Fluid::water(),
            turbulence: Turbulence::Laminar,
            conditions: vec![
                Condition {
                    kind: ConditionKind::InletVolumeFlow { rate: 1e-6 },
                    labels: vec![0],
                },
                Condition {
                    kind: ConditionKind::OutletPressure { pressure: 0.0 },
                    labels: vec![1],
                },
            ],
            goals: vec![],
            mesh: MeshSettings::new(1.0),
            solver: SolverSettings::default(),
        }
    }

    #[test]
    fn a_cube_has_six_unit_sides_and_its_volume_outward() {
        let surface = cube(2.0, [1.0, 2.0, 3.0]);
        let areas = surface.areas();
        assert_eq!(areas.len(), 6);
        for area in areas.values() {
            assert!((area - 4.0).abs() < 1e-12);
        }
        // Outward normals: the divergence theorem gives the volume with a positive sign.
        let volume: f64 = (0..surface.triangles.len())
            .map(|t| {
                let [a, b, c] = surface.corners(t).unwrap();
                dot(a, cross(b, c)) / 6.0
            })
            .sum();
        assert!((volume - 8.0).abs() < 1e-9, "{volume}");
        assert_eq!(surface.bounds(), Some(([0.0, 1.0, 2.0], [2.0, 3.0, 4.0])));
    }

    #[test]
    fn a_good_study_passes_its_check() {
        assert_eq!(study().check(), Ok(()));
        assert_eq!(study().background_cells(), [14, 14, 14]);
    }

    #[test]
    fn checks_name_what_is_wrong() {
        let mut s = study();
        s.conditions[1].labels = vec![0];
        assert_eq!(
            s.check(),
            Err(StudyError::LabelInTwoConditions { label: 0 })
        );

        let mut s = study();
        s.conditions[0].labels = vec![42];
        assert_eq!(s.check(), Err(StudyError::UnknownLabel { label: 42 }));

        let mut s = study();
        s.conditions.remove(1);
        assert_eq!(s.check(), Err(StudyError::NoPressureOpening));

        let mut s = study();
        s.flow_type = FlowType::Internal {
            point: [100.0, 0.0, 0.0],
        };
        assert_eq!(s.check(), Err(StudyError::PointOutsideDomain));

        let mut s = study();
        s.mesh.cell_size = 0.01;
        assert!(matches!(s.check(), Err(StudyError::TooManyCells { .. })));

        let mut s = study();
        s.fluid.viscosity = 0.0;
        assert!(matches!(s.check(), Err(StudyError::BadValue { .. })));

        let mut s = study();
        s.surfaces[0].labels.pop();
        assert_eq!(s.check(), Err(StudyError::BadSurface { surface: 0 }));
    }

    #[test]
    fn an_external_domain_reaches_further_downstream() {
        let bounds = ([0.0; 3], [10.0, 5.0, 2.0]);
        let d = Domain::external(bounds, [3.0, 0.0, 0.0]);
        assert_eq!(d.min, [-20.0, -20.0, -20.0]);
        assert_eq!(d.max, [60.0, 25.0, 22.0]);
        let d = Domain::external(bounds, [0.0, -1.0, 0.0]);
        assert_eq!(d.min[1], -50.0);
        assert_eq!(d.max[1], 25.0);

        let mut s = study();
        s.flow_type = FlowType::External {
            velocity: [1.0, 0.0, 0.0],
        };
        s.conditions.clear();
        // The internal domain leaves 2 mm round the model: less than a 2.5 mm cell.
        s.mesh.cell_size = 2.5;
        assert_eq!(s.check(), Err(StudyError::ModelOutsideDomain));
        s.domain = Domain::external(s.bounds().unwrap(), [1.0, 0.0, 0.0]);
        s.mesh.cell_size = 5.0;
        assert_eq!(s.check(), Ok(()));
    }

    #[test]
    fn the_finest_cells_halve_per_level() {
        let mut m = MeshSettings::new(8.0);
        m.surface_level = 2;
        m.feature_level = 3;
        assert_eq!(m.finest(), 1.0);
    }
}
