//! Simulation studies (`.oxysim`): what holds a part or an assembly, what loads it (forces
//! and pressures, or heat), how its bodies touch, how it is meshed and which mechanical and
//! thermal properties the study uses instead of the materials' own. A document holds any
//! number of studies of five types: static, thermal, transient thermal, thermal stress and
//! flow (a liquid or a gas through or around the model, solved by OpenFOAM: its fluid,
//! boundary conditions and goals; `docs/CFD.md`).
//!
//! A study document refers to the model it studies (a part or an assembly file, by relative
//! path and document id) and to its faces by persistent name, never by index, so a study
//! survives rebuilds and edits of the model like mates do. It holds no geometry and no
//! results: those go in a separate `.oxyres` document ([`crate::results`]).
//!
//! See `docs/FORMAT.md` §8 for the specification.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, Write};
use std::path::Path;

use oxyde_assembly::ComponentId;
use oxyde_model::{FaceName, Mechanical, Thermal};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::schema::{FaceRefDto, MechanicalDto, ThermalDto, face_dto, face_from};
use crate::{
    DocumentKind, FLOW_FORMAT_VERSION, FORMAT_VERSION, FormatError, MANIFEST,
    THERMAL_FORMAT_VERSION, expect_kind, from_json, manifest_version, new_document_id, read_entry,
    read_manifest, to_json, write_file,
};

/// File extension of simulation studies.
pub const SIMULATION_EXTENSION: &str = "oxysim";

const SIMULATION: &str = "simulation.json";

/// A simulation document as stored on disk: one model and the studies made on it.
#[derive(Debug, Clone, PartialEq)]
pub struct SimulationFile {
    /// Never changes for the life of the document; results find their study by it.
    pub document_id: String,
    pub model: StudiedModel,
    pub studies: Vec<Study>,
}

impl SimulationFile {
    /// A new study document on `model`, with a fresh document id and no study yet.
    pub fn new(model: StudiedModel) -> Self {
        Self {
            document_id: new_document_id(),
            model,
            studies: Vec::new(),
        }
    }
}

/// The part or assembly a study document is about.
#[derive(Debug, Clone, PartialEq)]
pub struct StudiedModel {
    pub kind: ModelKind,
    /// Relative to the study document's folder, with `/` separators.
    pub path: String,
    /// The model's document id, to find it again after a move or a rename.
    pub document_id: String,
}

/// Whether a study is about a part or an assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    Part,
    Assembly,
}

/// A face of the studied model: of the part, or of a part in the assembly.
#[derive(Debug, Clone, PartialEq)]
pub struct StudyFace {
    /// The assembly's component holding the face; `None` in a part study.
    pub component: Option<ComponentId>,
    /// Inside a sub-assembly: its component ids down to the part, as mate references
    /// store them.
    pub path: Vec<ComponentId>,
    pub face: FaceName,
}

impl StudyFace {
    /// A face of the studied part.
    pub fn part(face: FaceName) -> Self {
        Self {
            component: None,
            path: Vec::new(),
            face,
        }
    }

    /// A face of a part in the studied assembly.
    pub fn component(component: ComponentId, path: Vec<ComponentId>, face: FaceName) -> Self {
        Self {
            component: Some(component),
            path,
            face,
        }
    }
}

/// What a study computes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StudyType {
    /// Linear static stress: displacements, stresses and the factor of safety.
    #[default]
    Static,
    /// The temperatures once heat flow has settled (steady state).
    Thermal,
    /// The temperatures over time, from a uniform start ([`Study::transient`]).
    TransientThermal,
    /// The steady temperatures, then the stresses of the expansion they cause with the
    /// fixtures and loads of a static study ([`Study::reference_temperature`]).
    ThermalStress,
    /// The steady flow of a liquid or a gas through or around the model, solved by
    /// OpenFOAM ([`Study::flow`], [`Study::conditions`], [`Study::goals`]).
    Flow,
}

impl StudyType {
    /// Whether it takes fixtures, forces, pressures and gravity.
    pub fn is_structural(self) -> bool {
        matches!(self, StudyType::Static | StudyType::ThermalStress)
    }

    /// Whether it takes thermal loads.
    pub fn is_thermal(self) -> bool {
        matches!(
            self,
            StudyType::Thermal | StudyType::TransientThermal | StudyType::ThermalStress
        )
    }

    /// Whether it is a flow study: a fluid, boundary conditions and goals instead of
    /// fixtures and loads.
    pub fn is_flow(self) -> bool {
        self == StudyType::Flow
    }
}

/// How a transient thermal study marches in time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransientSettings {
    /// s.
    pub duration: f64,
    /// Time steps over the duration.
    pub steps: u32,
    /// The uniform temperature the model starts at, °C.
    pub initial_temperature: f64,
}

impl Default for TransientSettings {
    fn default() -> Self {
        Self {
            duration: 60.0,
            steps: 60,
            initial_temperature: 20.0,
        }
    }
}

/// One study: fixtures, loads, contacts, mesh settings and material overrides.
#[derive(Debug, Clone, PartialEq)]
pub struct Study {
    /// Unique in the document (`Static 1`, as SolidWorks names them).
    pub name: String,
    pub study_type: StudyType,
    pub fixtures: Vec<Fixture>,
    pub loads: Vec<Load>,
    pub contacts: Vec<Contact>,
    pub mesh: MeshSettings,
    /// Mechanical properties this study uses instead of the material's, by material
    /// group: a material name, or `part:` and a document id for a part without one.
    pub materials: BTreeMap<String, Mechanical>,
    /// Thermal properties this study uses instead of the material's, by material group.
    pub thermal_materials: BTreeMap<String, Thermal>,
    /// A transient thermal study's time steps; ignored by the other types.
    pub transient: TransientSettings,
    /// The temperature at which a thermal stress study's model is free of stress, °C;
    /// ignored by the other types.
    pub reference_temperature: f64,
    /// A flow study's fluid, flow and solver settings; ignored by the other types.
    pub flow: FlowSettings,
    /// A flow study's boundary conditions.
    pub conditions: Vec<FlowCondition>,
    /// A flow study's goals: what it measures.
    pub goals: Vec<FlowGoal>,
}

impl Study {
    /// An empty static study.
    pub fn new(name: &str) -> Self {
        Self::of_type(name, StudyType::Static)
    }

    /// An empty study of `study_type`.
    pub fn of_type(name: &str, study_type: StudyType) -> Self {
        Self {
            name: name.to_owned(),
            study_type,
            fixtures: Vec::new(),
            loads: Vec::new(),
            contacts: Vec::new(),
            mesh: MeshSettings::default(),
            materials: BTreeMap::new(),
            thermal_materials: BTreeMap::new(),
            transient: TransientSettings::default(),
            reference_temperature: 20.0,
            flow: FlowSettings::default(),
            conditions: Vec::new(),
            goals: Vec::new(),
        }
    }
}

/// How faces are held.
#[derive(Debug, Clone, PartialEq)]
pub struct Fixture {
    /// `Fixed-1`, `Roller/Slider-2`… unique among the study's fixtures, loads and contacts.
    pub name: String,
    pub kind: FixtureKind,
    pub faces: Vec<StudyFace>,
    /// Kept in the study but not applied.
    pub suppressed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureKind {
    /// No displacement at all.
    Fixed,
    /// Free to slide in the face's plane, not to move along its normal.
    Roller,
}

/// What pushes on the model.
#[derive(Debug, Clone, PartialEq)]
pub struct Load {
    /// `Force-1`, `Pressure-1`, `Gravity-1`… unique like a fixture's.
    pub name: String,
    pub kind: LoadKind,
    pub suppressed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoadKind {
    /// A total force in N, shared over the faces by area; negative pushes the other way.
    Force {
        faces: Vec<StudyFace>,
        newtons: f64,
        direction: ForceDirection,
    },
    /// MPa (N/mm²), along each face's normal, into it.
    Pressure { faces: Vec<StudyFace>, mpa: f64 },
    /// The model's weight: an acceleration in m/s² in world coordinates (Y up).
    Gravity { acceleration: [f64; 3] },
    /// The faces held at a temperature, °C.
    Temperature { faces: Vec<StudyFace>, celsius: f64 },
    /// A total heat flow into the faces, W, shared by area; negative takes heat out.
    HeatPower { faces: Vec<StudyFace>, watts: f64 },
    /// A heat flux into the faces, W/m²; negative takes heat out.
    HeatFlux { faces: Vec<StudyFace>, flux: f64 },
    /// Convection from the faces to a fluid at `ambient` °C, the film coefficient in
    /// W/(m²·K).
    Convection {
        faces: Vec<StudyFace>,
        coefficient: f64,
        ambient: f64,
    },
    /// Radiation from the faces to surroundings at `ambient` °C, an emissivity from 0 to 1.
    Radiation {
        faces: Vec<StudyFace>,
        emissivity: f64,
        ambient: f64,
    },
    /// Heat made inside the bodies the faces belong to, W in all, spread through their
    /// volume.
    InternalHeat { faces: Vec<StudyFace>, watts: f64 },
}

impl LoadKind {
    /// Whether it is a thermal load (a static study takes none of them).
    pub fn is_thermal(&self) -> bool {
        !matches!(
            self,
            LoadKind::Force { .. } | LoadKind::Pressure { .. } | LoadKind::Gravity { .. }
        )
    }
}

/// Along what a force pushes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ForceDirection {
    /// Normal to each face, into it.
    Normal,
    /// Along a direction in world coordinates (only its direction counts).
    Vector([f64; 3]),
}

/// How two sets of faces touch, as Fusion 360 names it.
#[derive(Debug, Clone, PartialEq)]
pub struct Contact {
    /// `Contact-1`… unique like a fixture's.
    pub name: String,
    pub kind: ContactKind,
    pub a: Vec<StudyFace>,
    pub b: Vec<StudyFace>,
    /// mm: the gap an [`ContactKind::OffsetBonded`] contact bridges; 0 for the others.
    pub offset: f64,
    pub detection: ContactDetection,
    pub suppressed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactKind {
    /// Glued: the faces move together.
    Bonded,
    /// The faces may part but not pass through each other; frictionless.
    Separation,
    /// The faces slide on each other without friction but stay in touch.
    Sliding,
    /// The faces may part but not slide.
    Rough,
    /// Glued across a gap of [`Contact::offset`].
    OffsetBonded,
}

/// How a contact's faces were chosen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContactDetection {
    /// Picked by the user.
    Manual,
    /// Found by Oxyde: faces of different bodies closer than `tolerance` mm.
    Automatic { tolerance: f64 },
}

/// How the model is meshed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshSettings {
    pub mesher: Mesher,
    pub quality: MeshQuality,
    pub size: MeshSize,
}

impl Default for MeshSettings {
    fn default() -> Self {
        Self {
            mesher: Mesher::Netgen,
            quality: MeshQuality::High,
            size: MeshSize::Automatic { density: 0.5 },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mesher {
    /// Netgen's Delaunay and advancing-front mesher: graded, follows the faces.
    Netgen,
    /// Oxyde's own lattice mesher (isosurface stuffing).
    Lattice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshQuality {
    /// Ten-node (quadratic) tetrahedra.
    High,
    /// Four-node (linear) tetrahedra.
    Draft,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MeshSize {
    /// From coarse (0) to fine (1), relative to the model's size.
    Automatic { density: f64 },
    /// A target element size, mm.
    ElementSize(f64),
}

/// A flow study's fluid, flow and solver settings.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowSettings {
    pub analysis: FlowAnalysis,
    pub fluid: Fluid,
    pub turbulence: FlowTurbulence,
    /// The mesh's resolution, from 1 (coarse) to 7 (fine), as SolidWorks' *level of
    /// initial mesh*.
    pub mesh_level: u32,
    /// The most iterations of the solver.
    pub iterations: u32,
    /// The flow has converged when every equation's residual is under this.
    pub residual: f64,
}

impl Default for FlowSettings {
    fn default() -> Self {
        Self {
            analysis: FlowAnalysis::Internal,
            fluid: Fluid::water(),
            turbulence: FlowTurbulence::Laminar,
            mesh_level: 3,
            iterations: 500,
            residual: 1e-4,
        }
    }
}

/// Where the fluid flows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlowAnalysis {
    /// Through the model's cavity, closed by lids where the fluid comes in and goes out.
    Internal,
    /// Around the model, at `velocity` (m/s, world coordinates) far from it.
    External { velocity: [f64; 3] },
}

/// A fluid, with constant properties: the flow is incompressible.
#[derive(Debug, Clone, PartialEq)]
pub struct Fluid {
    pub name: String,
    /// kg/m³.
    pub density: f64,
    /// Dynamic viscosity, Pa·s.
    pub viscosity: f64,
}

impl Fluid {
    /// Water at 20 °C.
    pub fn water() -> Self {
        Self {
            name: "Water".to_owned(),
            density: 998.2,
            viscosity: 1.002e-3,
        }
    }

    /// Air at 20 °C and 101 325 Pa.
    pub fn air() -> Self {
        Self {
            name: "Air".to_owned(),
            density: 1.204,
            viscosity: 1.825e-5,
        }
    }
}

/// Laminar or turbulent flow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlowTurbulence {
    Laminar,
    /// Turbulent, with the k-ω SST model. The turbulence of the fluid coming in: an
    /// `intensity` (0.05 for 5 %) and a `length` scale, mm.
    Turbulent {
        intensity: f64,
        length: f64,
    },
}

/// A boundary condition of a flow study. Faces in none are walls the fluid sticks to.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowCondition {
    /// `Inlet Velocity-1`… unique among the study's conditions and goals.
    pub name: String,
    pub kind: FlowConditionKind,
    pub faces: Vec<StudyFace>,
    /// Kept in the study but not applied.
    pub suppressed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlowConditionKind {
    /// Fluid coming in normal to the faces at `speed`, m/s.
    InletVelocity { speed: f64 },
    /// Fluid coming in through the faces, m³/s, shared between them by area.
    InletVolumeFlow { rate: f64 },
    /// Fluid coming in through the faces, kg/s, shared between them by area.
    InletMassFlow { rate: f64 },
    /// An opening at a static `pressure`, Pa (gauge), where the fluid goes out.
    StaticPressure { pressure: f64 },
    /// A wall the fluid slips along without friction.
    IdealWall,
}

/// A value a flow study measures on model faces, as SolidWorks' surface goals.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowGoal {
    /// Unique like a condition's.
    pub name: String,
    pub kind: FlowGoalKind,
    pub faces: Vec<StudyFace>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowGoalKind {
    /// The force of the fluid on the faces, N: pressure and friction.
    Force,
    /// The volume flow out of the fluid through the faces, m³/s (negative coming in).
    VolumeFlow,
    /// The mass flow out of the fluid through the faces, kg/s (negative coming in).
    MassFlow,
    /// The area-weighted mean static pressure on the faces, Pa (gauge).
    MeanPressure,
}

/// The `format_version` `file` is written with: the oldest that holds its studies' types,
/// so that older readers still open what they can. 1 for static studies only, 2 with a
/// thermal one, 5 with a flow one.
pub fn simulation_version(file: &SimulationFile) -> u32 {
    file.studies
        .iter()
        .map(|s| study_version(s.study_type))
        .max()
        .unwrap_or(FORMAT_VERSION)
}

/// The `format_version` a study of `t` (and its results) needs.
pub(crate) fn study_version(t: StudyType) -> u32 {
    match t {
        StudyType::Static => FORMAT_VERSION,
        StudyType::Thermal | StudyType::TransientThermal | StudyType::ThermalStress => {
            THERMAL_FORMAT_VERSION
        }
        StudyType::Flow => FLOW_FORMAT_VERSION,
    }
}

/// Writes a simulation document as a ZIP archive (`.oxysim`).
pub fn write_simulation<W: Write + Seek>(
    writer: W,
    file: &SimulationFile,
) -> Result<W, FormatError> {
    let manifest = manifest_version(
        DocumentKind::Simulation,
        &file.document_id,
        simulation_version(file),
    );
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut zip = ZipWriter::new(writer);
    for (entry, bytes) in [
        (MANIFEST, to_json(MANIFEST, &manifest)?),
        (SIMULATION, to_json(SIMULATION, &simulation_dto(file))?),
    ] {
        zip.start_file(entry, options)?;
        zip.write_all(&bytes)?;
    }
    Ok(zip.finish()?)
}

/// Reads a simulation document and checks it: unique names, finite values in range, faces
/// that name a component exactly when the model is an assembly. The model is not opened.
pub fn read_simulation<R: Read + Seek>(reader: R) -> Result<SimulationFile, FormatError> {
    let mut archive = ZipArchive::new(reader)?;
    let manifest = read_manifest(&mut archive)?;
    expect_kind(&manifest, DocumentKind::Simulation)?;
    let dto: SimulationDto = from_json(SIMULATION, &read_entry(&mut archive, SIMULATION)?)?;
    let (model, studies) = simulation_from(dto).map_err(FormatError::InvalidSimulation)?;
    Ok(SimulationFile {
        document_id: manifest.document_id,
        model,
        studies,
    })
}

/// Saves a simulation document to `path`, safely like [`crate::save_part`]: what would not
/// read back is refused before the previous version is touched.
pub fn save_simulation(path: &Path, file: &SimulationFile) -> Result<(), FormatError> {
    let bytes = write_simulation(Cursor::new(Vec::new()), file)?.into_inner();
    read_simulation(Cursor::new(&bytes)).map_err(|e| FormatError::Unreadable(Box::new(e)))?;
    write_file(path, &bytes)
}

/// Loads a simulation document from `path`.
pub fn load_simulation(path: &Path) -> Result<SimulationFile, FormatError> {
    read_simulation(BufReader::new(File::open(path)?))
}

/// The study as `simulation.json` holds it, for a fingerprint of the study results were
/// computed for ([`crate::results::study_hash`]).
pub(crate) fn study_json(study: &Study) -> Result<Vec<u8>, FormatError> {
    to_json(SIMULATION, &study_dto(study))
}

// ---- simulation.json -----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SimulationDto {
    model: ModelDto,
    #[serde(default)]
    studies: Vec<StudyDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ModelDto {
    kind: ModelKindDto,
    path: String,
    document_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelKindDto {
    Part,
    Assembly,
}

/// A face reference, written like a mate's: `component` and `path` only in assemblies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct StudyFaceDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<u32>,
    pub face: FaceRefDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StudyDto {
    name: String,
    #[serde(rename = "type")]
    study_type: StudyTypeDto,
    #[serde(default)]
    fixtures: Vec<FixtureDto>,
    #[serde(default)]
    loads: Vec<LoadDto>,
    #[serde(default)]
    contacts: Vec<ContactDto>,
    /// Absent in a flow study, whose `flow.mesh_level` sets its mesh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mesh: Option<MeshDto>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    materials: BTreeMap<String, MechanicalDto>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    thermal_materials: BTreeMap<String, ThermalDto>,
    /// Only in a transient thermal study.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transient: Option<TransientDto>,
    /// °C; only in a thermal stress study.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reference_temperature: Option<f64>,
    /// Only in a flow study.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flow: Option<FlowDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    conditions: Vec<ConditionDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    goals: Vec<GoalDto>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StudyTypeDto {
    #[default]
    Static,
    Thermal,
    TransientThermal,
    ThermalStress,
    Flow,
}

impl StudyTypeDto {
    pub(crate) fn of(t: StudyType) -> Self {
        match t {
            StudyType::Static => StudyTypeDto::Static,
            StudyType::Thermal => StudyTypeDto::Thermal,
            StudyType::TransientThermal => StudyTypeDto::TransientThermal,
            StudyType::ThermalStress => StudyTypeDto::ThermalStress,
            StudyType::Flow => StudyTypeDto::Flow,
        }
    }

    pub(crate) fn study_type(self) -> StudyType {
        match self {
            StudyTypeDto::Static => StudyType::Static,
            StudyTypeDto::Thermal => StudyType::Thermal,
            StudyTypeDto::TransientThermal => StudyType::TransientThermal,
            StudyTypeDto::ThermalStress => StudyType::ThermalStress,
            StudyTypeDto::Flow => StudyType::Flow,
        }
    }

    pub(crate) fn is_static(&self) -> bool {
        *self == StudyTypeDto::Static
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct TransientDto {
    /// s.
    duration: f64,
    steps: u32,
    /// °C.
    #[serde(default = "default_initial")]
    initial_temperature: f64,
}

fn default_initial() -> f64 {
    20.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FixtureDto {
    name: String,
    #[serde(rename = "type")]
    kind: FixtureKindDto,
    #[serde(default)]
    faces: Vec<StudyFaceDto>,
    #[serde(default, skip_serializing_if = "is_false")]
    suppressed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureKindDto {
    Fixed,
    Roller,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LoadDto {
    name: String,
    #[serde(flatten)]
    kind: LoadKindDto,
    #[serde(default, skip_serializing_if = "is_false")]
    suppressed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LoadKindDto {
    /// N.
    Force {
        #[serde(default)]
        faces: Vec<StudyFaceDto>,
        force: f64,
        direction: DirectionDto,
    },
    /// MPa.
    Pressure {
        #[serde(default)]
        faces: Vec<StudyFaceDto>,
        pressure: f64,
    },
    /// m/s².
    Gravity { acceleration: [f64; 3] },
    /// °C.
    Temperature {
        #[serde(default)]
        faces: Vec<StudyFaceDto>,
        temperature: f64,
    },
    /// W.
    HeatPower {
        #[serde(default)]
        faces: Vec<StudyFaceDto>,
        power: f64,
    },
    /// W/m².
    HeatFlux {
        #[serde(default)]
        faces: Vec<StudyFaceDto>,
        flux: f64,
    },
    /// W/(m²·K) and °C.
    Convection {
        #[serde(default)]
        faces: Vec<StudyFaceDto>,
        coefficient: f64,
        ambient: f64,
    },
    /// 0 to 1, and °C.
    Radiation {
        #[serde(default)]
        faces: Vec<StudyFaceDto>,
        emissivity: f64,
        ambient: f64,
    },
    /// W, in the bodies of the faces.
    InternalHeat {
        #[serde(default)]
        faces: Vec<StudyFaceDto>,
        power: f64,
    },
}

/// `"normal"` or a vector.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum DirectionDto {
    Named(DirectionName),
    Vector([f64; 3]),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DirectionName {
    Normal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ContactDto {
    name: String,
    #[serde(rename = "type")]
    kind: ContactKindDto,
    #[serde(default)]
    a: Vec<StudyFaceDto>,
    #[serde(default)]
    b: Vec<StudyFaceDto>,
    /// mm.
    #[serde(default, skip_serializing_if = "is_zero")]
    offset: f64,
    #[serde(default, skip_serializing_if = "is_false")]
    automatic: bool,
    /// mm; only for automatic contacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tolerance: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    suppressed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContactKindDto {
    Bonded,
    Separation,
    Sliding,
    Rough,
    OffsetBonded,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
struct MeshDto {
    #[serde(default)]
    mesher: MesherDto,
    #[serde(default)]
    quality: QualityDto,
    /// 0 to 1; absent with `element_size`, 0.5 when both are absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    density: Option<f64>,
    /// mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    element_size: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MesherDto {
    #[default]
    Netgen,
    Lattice,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum QualityDto {
    #[default]
    High,
    Draft,
}

/// A flow study's settings: `velocity` only in an external flow, `intensity` and `length`
/// only in a turbulent one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FlowDto {
    analysis: AnalysisDto,
    /// m/s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    velocity: Option<[f64; 3]>,
    fluid: FluidDto,
    turbulence: TurbulenceDto,
    /// 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    intensity: Option<f64>,
    /// mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    length: Option<f64>,
    #[serde(default = "default_mesh_level")]
    mesh_level: u32,
    #[serde(default = "default_iterations")]
    iterations: u32,
    #[serde(default = "default_residual")]
    residual: f64,
}

fn default_mesh_level() -> u32 {
    FlowSettings::default().mesh_level
}

fn default_iterations() -> u32 {
    FlowSettings::default().iterations
}

fn default_residual() -> f64 {
    FlowSettings::default().residual
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AnalysisDto {
    Internal,
    External,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FluidDto {
    name: String,
    /// kg/m³.
    density: f64,
    /// Pa·s.
    viscosity: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TurbulenceDto {
    Laminar,
    Turbulent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ConditionDto {
    name: String,
    #[serde(flatten)]
    kind: ConditionKindDto,
    #[serde(default)]
    faces: Vec<StudyFaceDto>,
    #[serde(default, skip_serializing_if = "is_false")]
    suppressed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ConditionKindDto {
    /// m/s.
    InletVelocity {
        velocity: f64,
    },
    /// m³/s.
    InletVolumeFlow {
        volume_flow: f64,
    },
    /// kg/s.
    InletMassFlow {
        mass_flow: f64,
    },
    /// Pa.
    StaticPressure {
        pressure: f64,
    },
    IdealWall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct GoalDto {
    name: String,
    #[serde(rename = "type")]
    kind: GoalKindDto,
    #[serde(default)]
    faces: Vec<StudyFaceDto>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoalKindDto {
    Force,
    VolumeFlow,
    MassFlow,
    MeanPressure,
}

impl GoalKindDto {
    pub(crate) fn of(k: FlowGoalKind) -> Self {
        match k {
            FlowGoalKind::Force => GoalKindDto::Force,
            FlowGoalKind::VolumeFlow => GoalKindDto::VolumeFlow,
            FlowGoalKind::MassFlow => GoalKindDto::MassFlow,
            FlowGoalKind::MeanPressure => GoalKindDto::MeanPressure,
        }
    }

    pub(crate) fn kind(self) -> FlowGoalKind {
        match self {
            GoalKindDto::Force => FlowGoalKind::Force,
            GoalKindDto::VolumeFlow => FlowGoalKind::VolumeFlow,
            GoalKindDto::MassFlow => FlowGoalKind::MassFlow,
            GoalKindDto::MeanPressure => FlowGoalKind::MeanPressure,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

// ---- To the file ---------------------------------------------------------------------------

fn simulation_dto(file: &SimulationFile) -> SimulationDto {
    SimulationDto {
        model: ModelDto {
            kind: match file.model.kind {
                ModelKind::Part => ModelKindDto::Part,
                ModelKind::Assembly => ModelKindDto::Assembly,
            },
            path: file.model.path.clone(),
            document_id: file.model.document_id.clone(),
        },
        studies: file.studies.iter().map(study_dto).collect(),
    }
}

pub(crate) fn study_face_dto(face: &StudyFace) -> StudyFaceDto {
    StudyFaceDto {
        component: face.component.map(|c| c.0),
        path: face.path.iter().map(|c| c.0).collect(),
        face: face_dto(&face.face),
    }
}

fn faces_dto(faces: &[StudyFace]) -> Vec<StudyFaceDto> {
    faces.iter().map(study_face_dto).collect()
}

fn study_dto(study: &Study) -> StudyDto {
    StudyDto {
        name: study.name.clone(),
        study_type: StudyTypeDto::of(study.study_type),
        fixtures: study
            .fixtures
            .iter()
            .map(|f| FixtureDto {
                name: f.name.clone(),
                kind: match f.kind {
                    FixtureKind::Fixed => FixtureKindDto::Fixed,
                    FixtureKind::Roller => FixtureKindDto::Roller,
                },
                faces: faces_dto(&f.faces),
                suppressed: f.suppressed,
            })
            .collect(),
        loads: study
            .loads
            .iter()
            .map(|l| LoadDto {
                name: l.name.clone(),
                kind: match &l.kind {
                    LoadKind::Force {
                        faces,
                        newtons,
                        direction,
                    } => LoadKindDto::Force {
                        faces: faces_dto(faces),
                        force: *newtons,
                        direction: match direction {
                            ForceDirection::Normal => DirectionDto::Named(DirectionName::Normal),
                            ForceDirection::Vector(v) => DirectionDto::Vector(*v),
                        },
                    },
                    LoadKind::Pressure { faces, mpa } => LoadKindDto::Pressure {
                        faces: faces_dto(faces),
                        pressure: *mpa,
                    },
                    LoadKind::Gravity { acceleration } => LoadKindDto::Gravity {
                        acceleration: *acceleration,
                    },
                    LoadKind::Temperature { faces, celsius } => LoadKindDto::Temperature {
                        faces: faces_dto(faces),
                        temperature: *celsius,
                    },
                    LoadKind::HeatPower { faces, watts } => LoadKindDto::HeatPower {
                        faces: faces_dto(faces),
                        power: *watts,
                    },
                    LoadKind::HeatFlux { faces, flux } => LoadKindDto::HeatFlux {
                        faces: faces_dto(faces),
                        flux: *flux,
                    },
                    LoadKind::Convection {
                        faces,
                        coefficient,
                        ambient,
                    } => LoadKindDto::Convection {
                        faces: faces_dto(faces),
                        coefficient: *coefficient,
                        ambient: *ambient,
                    },
                    LoadKind::Radiation {
                        faces,
                        emissivity,
                        ambient,
                    } => LoadKindDto::Radiation {
                        faces: faces_dto(faces),
                        emissivity: *emissivity,
                        ambient: *ambient,
                    },
                    LoadKind::InternalHeat { faces, watts } => LoadKindDto::InternalHeat {
                        faces: faces_dto(faces),
                        power: *watts,
                    },
                },
                suppressed: l.suppressed,
            })
            .collect(),
        contacts: study
            .contacts
            .iter()
            .map(|c| ContactDto {
                name: c.name.clone(),
                kind: match c.kind {
                    ContactKind::Bonded => ContactKindDto::Bonded,
                    ContactKind::Separation => ContactKindDto::Separation,
                    ContactKind::Sliding => ContactKindDto::Sliding,
                    ContactKind::Rough => ContactKindDto::Rough,
                    ContactKind::OffsetBonded => ContactKindDto::OffsetBonded,
                },
                a: faces_dto(&c.a),
                b: faces_dto(&c.b),
                offset: c.offset,
                automatic: matches!(c.detection, ContactDetection::Automatic { .. }),
                tolerance: match c.detection {
                    ContactDetection::Manual => None,
                    ContactDetection::Automatic { tolerance } => Some(tolerance),
                },
                suppressed: c.suppressed,
            })
            .collect(),
        mesh: (!study.study_type.is_flow()).then_some(MeshDto {
            mesher: match study.mesh.mesher {
                Mesher::Netgen => MesherDto::Netgen,
                Mesher::Lattice => MesherDto::Lattice,
            },
            quality: match study.mesh.quality {
                MeshQuality::High => QualityDto::High,
                MeshQuality::Draft => QualityDto::Draft,
            },
            density: match study.mesh.size {
                MeshSize::Automatic { density } => Some(density),
                MeshSize::ElementSize(_) => None,
            },
            element_size: match study.mesh.size {
                MeshSize::Automatic { .. } => None,
                MeshSize::ElementSize(size) => Some(size),
            },
        }),
        materials: study
            .materials
            .iter()
            .map(|(group, m)| {
                (
                    group.clone(),
                    MechanicalDto {
                        elastic_modulus: m.elastic_modulus,
                        poisson_ratio: m.poisson_ratio,
                        yield_strength: m.yield_strength,
                        tensile_strength: m.tensile_strength,
                    },
                )
            })
            .collect(),
        thermal_materials: study
            .thermal_materials
            .iter()
            .map(|(group, t)| {
                (
                    group.clone(),
                    ThermalDto {
                        conductivity: t.conductivity,
                        specific_heat: t.specific_heat,
                        expansion: t.expansion,
                    },
                )
            })
            .collect(),
        transient: (study.study_type == StudyType::TransientThermal).then_some(TransientDto {
            duration: study.transient.duration,
            steps: study.transient.steps,
            initial_temperature: study.transient.initial_temperature,
        }),
        reference_temperature: (study.study_type == StudyType::ThermalStress)
            .then_some(study.reference_temperature),
        flow: study.study_type.is_flow().then(|| flow_dto(&study.flow)),
        conditions: if study.study_type.is_flow() {
            study.conditions.iter().map(condition_dto).collect()
        } else {
            Vec::new()
        },
        goals: if study.study_type.is_flow() {
            study
                .goals
                .iter()
                .map(|g| GoalDto {
                    name: g.name.clone(),
                    kind: GoalKindDto::of(g.kind),
                    faces: faces_dto(&g.faces),
                })
                .collect()
        } else {
            Vec::new()
        },
    }
}

fn flow_dto(flow: &FlowSettings) -> FlowDto {
    let (analysis, velocity) = match flow.analysis {
        FlowAnalysis::Internal => (AnalysisDto::Internal, None),
        FlowAnalysis::External { velocity } => (AnalysisDto::External, Some(velocity)),
    };
    let (turbulence, intensity, length) = match flow.turbulence {
        FlowTurbulence::Laminar => (TurbulenceDto::Laminar, None, None),
        FlowTurbulence::Turbulent { intensity, length } => {
            (TurbulenceDto::Turbulent, Some(intensity), Some(length))
        }
    };
    FlowDto {
        analysis,
        velocity,
        fluid: FluidDto {
            name: flow.fluid.name.clone(),
            density: flow.fluid.density,
            viscosity: flow.fluid.viscosity,
        },
        turbulence,
        intensity,
        length,
        mesh_level: flow.mesh_level,
        iterations: flow.iterations,
        residual: flow.residual,
    }
}

fn condition_dto(c: &FlowCondition) -> ConditionDto {
    ConditionDto {
        name: c.name.clone(),
        kind: match c.kind {
            FlowConditionKind::InletVelocity { speed } => {
                ConditionKindDto::InletVelocity { velocity: speed }
            }
            FlowConditionKind::InletVolumeFlow { rate } => {
                ConditionKindDto::InletVolumeFlow { volume_flow: rate }
            }
            FlowConditionKind::InletMassFlow { rate } => {
                ConditionKindDto::InletMassFlow { mass_flow: rate }
            }
            FlowConditionKind::StaticPressure { pressure } => {
                ConditionKindDto::StaticPressure { pressure }
            }
            FlowConditionKind::IdealWall => ConditionKindDto::IdealWall,
        },
        faces: faces_dto(&c.faces),
        suppressed: c.suppressed,
    }
}

// ---- From the file -------------------------------------------------------------------------

fn simulation_from(dto: SimulationDto) -> Result<(StudiedModel, Vec<Study>), String> {
    if dto.model.path.is_empty() {
        return Err("the model has no path".to_owned());
    }
    let kind = match dto.model.kind {
        ModelKindDto::Part => ModelKind::Part,
        ModelKindDto::Assembly => ModelKind::Assembly,
    };
    let mut names = BTreeSet::new();
    let mut studies = Vec::with_capacity(dto.studies.len());
    for study in dto.studies {
        if study.name.trim().is_empty() {
            return Err("a study has no name".to_owned());
        }
        if !names.insert(study.name.clone()) {
            return Err(format!("two studies are named \"{}\"", study.name));
        }
        let name = study.name.clone();
        studies.push(study_from(study, kind).map_err(|e| format!("study \"{name}\": {e}"))?);
    }
    Ok((
        StudiedModel {
            kind,
            path: dto.model.path,
            document_id: dto.model.document_id,
        },
        studies,
    ))
}

/// A face reference, checked against the kind of model: a part's faces name no component,
/// an assembly's name one of its components (from 1: component 0 is the assembly itself,
/// which has no faces).
pub(crate) fn study_face_from(
    dto: StudyFaceDto,
    kind: Option<ModelKind>,
) -> Result<StudyFace, String> {
    match (kind, dto.component) {
        (Some(ModelKind::Part), Some(_)) => {
            return Err("a face of a part names a component".to_owned());
        }
        (Some(ModelKind::Assembly), None) => {
            return Err("a face of an assembly names no component".to_owned());
        }
        (_, None) if !dto.path.is_empty() => {
            return Err("a face has a component path but no component".to_owned());
        }
        (_, Some(0)) => return Err("a face names component 0".to_owned()),
        _ => {}
    }
    Ok(StudyFace {
        component: dto.component.map(ComponentId),
        path: dto.path.into_iter().map(ComponentId).collect(),
        face: face_from(dto.face),
    })
}

fn faces_from(faces: Vec<StudyFaceDto>, kind: ModelKind) -> Result<Vec<StudyFace>, String> {
    faces
        .into_iter()
        .map(|f| study_face_from(f, Some(kind)))
        .collect()
}

fn finite(value: f64, what: &str) -> Result<f64, String> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(format!("{what} is not a finite number"))
    }
}

fn finite_vector(v: [f64; 3], what: &str) -> Result<[f64; 3], String> {
    if v.iter().all(|x| x.is_finite()) {
        Ok(v)
    } else {
        Err(format!("{what} is not a finite vector"))
    }
}

/// A temperature in °C, not under absolute zero.
fn celsius(value: f64, what: &str) -> Result<f64, String> {
    let value = finite(value, what)?;
    if value < -273.15 {
        return Err(format!("{what} is below absolute zero"));
    }
    Ok(value)
}

fn study_from(dto: StudyDto, kind: ModelKind) -> Result<Study, String> {
    let kind_of_model = kind;
    let study_type = dto.study_type.study_type();
    // Fixtures, loads and contacts share one set of names, as in SolidWorks' study tree.
    let mut names = BTreeSet::new();
    let mut named = |name: &str| -> Result<(), String> {
        if name.trim().is_empty() {
            Err("an item has no name".to_owned())
        } else if names.insert(name.to_owned()) {
            Ok(())
        } else {
            Err(format!("two items are named \"{name}\""))
        }
    };

    if !study_type.is_structural() && !dto.fixtures.is_empty() {
        return Err(if study_type.is_flow() {
            "a flow study has fixtures".to_owned()
        } else {
            "a thermal study has fixtures".to_owned()
        });
    }
    if study_type.is_flow() && !dto.loads.is_empty() {
        return Err("a flow study has loads".to_owned());
    }
    if study_type.is_flow() && !dto.contacts.is_empty() {
        return Err("a flow study has contacts".to_owned());
    }
    if !study_type.is_flow()
        && (dto.flow.is_some() || !dto.conditions.is_empty() || !dto.goals.is_empty())
    {
        return Err("only a flow study has a fluid, boundary conditions or goals".to_owned());
    }
    let mut fixtures = Vec::with_capacity(dto.fixtures.len());
    for f in dto.fixtures {
        named(&f.name)?;
        fixtures.push(Fixture {
            kind: match f.kind {
                FixtureKindDto::Fixed => FixtureKind::Fixed,
                FixtureKindDto::Roller => FixtureKind::Roller,
            },
            faces: faces_from(f.faces, kind).map_err(|e| format!("{}: {e}", f.name))?,
            suppressed: f.suppressed,
            name: f.name,
        });
    }

    let mut loads = Vec::with_capacity(dto.loads.len());
    for l in dto.loads {
        named(&l.name)?;
        let item = |e: String| format!("{}: {e}", l.name);
        let load_kind = match l.kind {
            LoadKindDto::Force {
                faces,
                force,
                direction,
            } => LoadKind::Force {
                faces: faces_from(faces, kind).map_err(item)?,
                newtons: finite(force, "the force").map_err(item)?,
                direction: match direction {
                    DirectionDto::Named(DirectionName::Normal) => ForceDirection::Normal,
                    DirectionDto::Vector(v) => {
                        let v = finite_vector(v, "the direction").map_err(item)?;
                        if v.iter().all(|x| *x == 0.0) {
                            return Err(item("the direction is a zero vector".to_owned()));
                        }
                        ForceDirection::Vector(v)
                    }
                },
            },
            LoadKindDto::Pressure { faces, pressure } => LoadKind::Pressure {
                faces: faces_from(faces, kind).map_err(item)?,
                mpa: finite(pressure, "the pressure").map_err(item)?,
            },
            LoadKindDto::Gravity { acceleration } => LoadKind::Gravity {
                acceleration: finite_vector(acceleration, "the acceleration").map_err(item)?,
            },
            LoadKindDto::Temperature { faces, temperature } => LoadKind::Temperature {
                faces: faces_from(faces, kind).map_err(item)?,
                celsius: celsius(temperature, "the temperature").map_err(item)?,
            },
            LoadKindDto::HeatPower { faces, power } => LoadKind::HeatPower {
                faces: faces_from(faces, kind).map_err(item)?,
                watts: finite(power, "the power").map_err(item)?,
            },
            LoadKindDto::HeatFlux { faces, flux } => LoadKind::HeatFlux {
                faces: faces_from(faces, kind).map_err(item)?,
                flux: finite(flux, "the flux").map_err(item)?,
            },
            LoadKindDto::Convection {
                faces,
                coefficient,
                ambient,
            } => {
                let coefficient = finite(coefficient, "the film coefficient").map_err(item)?;
                if coefficient < 0.0 {
                    return Err(item("the film coefficient is negative".to_owned()));
                }
                LoadKind::Convection {
                    faces: faces_from(faces, kind).map_err(item)?,
                    coefficient,
                    ambient: celsius(ambient, "the fluid's temperature").map_err(item)?,
                }
            }
            LoadKindDto::Radiation {
                faces,
                emissivity,
                ambient,
            } => {
                let emissivity = finite(emissivity, "the emissivity").map_err(item)?;
                if !(0.0..=1.0).contains(&emissivity) {
                    return Err(item("the emissivity is not between 0 and 1".to_owned()));
                }
                LoadKind::Radiation {
                    faces: faces_from(faces, kind).map_err(item)?,
                    emissivity,
                    ambient: celsius(ambient, "the surroundings' temperature").map_err(item)?,
                }
            }
            LoadKindDto::InternalHeat { faces, power } => LoadKind::InternalHeat {
                faces: faces_from(faces, kind).map_err(item)?,
                watts: finite(power, "the power").map_err(item)?,
            },
        };
        if load_kind.is_thermal() && !study_type.is_thermal() {
            return Err(item("a static study has a thermal load".to_owned()));
        }
        if !load_kind.is_thermal() && !study_type.is_structural() {
            return Err(item(
                "a thermal study has a force, a pressure or gravity".to_owned(),
            ));
        }
        loads.push(Load {
            name: l.name,
            kind: load_kind,
            suppressed: l.suppressed,
        });
    }

    let mut contacts = Vec::with_capacity(dto.contacts.len());
    for c in dto.contacts {
        named(&c.name)?;
        let item = |e: String| format!("{}: {e}", c.name);
        let offset = finite(c.offset, "the offset").map_err(item)?;
        if offset < 0.0 {
            return Err(item("the offset is negative".to_owned()));
        }
        let detection = match (c.automatic, c.tolerance) {
            (false, None) => ContactDetection::Manual,
            (false, Some(_)) => {
                return Err(item("a manual contact has a tolerance".to_owned()));
            }
            (true, None) => return Err(item("an automatic contact has no tolerance".to_owned())),
            (true, Some(t)) => {
                let tolerance = finite(t, "the tolerance").map_err(item)?;
                if tolerance <= 0.0 {
                    return Err(item("the tolerance is not positive".to_owned()));
                }
                ContactDetection::Automatic { tolerance }
            }
        };
        contacts.push(Contact {
            kind: match c.kind {
                ContactKindDto::Bonded => ContactKind::Bonded,
                ContactKindDto::Separation => ContactKind::Separation,
                ContactKindDto::Sliding => ContactKind::Sliding,
                ContactKindDto::Rough => ContactKind::Rough,
                ContactKindDto::OffsetBonded => ContactKind::OffsetBonded,
            },
            a: faces_from(c.a, kind).map_err(item)?,
            b: faces_from(c.b, kind).map_err(item)?,
            offset,
            detection,
            suppressed: c.suppressed,
            name: c.name,
        });
    }

    let mesh = dto.mesh.unwrap_or_default();
    let size = match (mesh.density, mesh.element_size) {
        (Some(_), Some(_)) => {
            return Err("the mesh has both a density and an element size".to_owned());
        }
        (density, None) => {
            let density = finite(density.unwrap_or(0.5), "the mesh density")?;
            if !(0.0..=1.0).contains(&density) {
                return Err("the mesh density is not between 0 and 1".to_owned());
            }
            MeshSize::Automatic { density }
        }
        (None, Some(size)) => {
            let size = finite(size, "the element size")?;
            if size <= 0.0 {
                return Err("the element size is not positive".to_owned());
            }
            MeshSize::ElementSize(size)
        }
    };

    let mut materials = BTreeMap::new();
    for (group, p) in dto.materials {
        if group.is_empty() {
            return Err("a material override names no material".to_owned());
        }
        let mechanical = Mechanical::new(
            p.elastic_modulus,
            p.poisson_ratio,
            p.yield_strength,
            p.tensile_strength,
        );
        // Values a solver could not use are dropped, as in a material's own block: the
        // material's values apply instead.
        if mechanical.is_valid() {
            materials.insert(group, mechanical);
        }
    }

    let mut thermal_materials = BTreeMap::new();
    for (group, t) in dto.thermal_materials {
        if group.is_empty() {
            return Err("a material override names no material".to_owned());
        }
        let thermal = Thermal::new(t.conductivity, t.specific_heat, t.expansion);
        // As for mechanical overrides: unusable values are dropped.
        if thermal.is_valid() {
            thermal_materials.insert(group, thermal);
        }
    }
    let transient = match (study_type, dto.transient) {
        (StudyType::TransientThermal, Some(t)) => {
            let duration = finite(t.duration, "the duration")?;
            if duration <= 0.0 {
                return Err("the duration is not positive".to_owned());
            }
            if !(1..=100_000).contains(&t.steps) {
                return Err("the time steps are not from 1 to 100 000".to_owned());
            }
            TransientSettings {
                duration,
                steps: t.steps,
                initial_temperature: celsius(t.initial_temperature, "the initial temperature")?,
            }
        }
        (StudyType::TransientThermal, None) => TransientSettings::default(),
        (_, Some(_)) => return Err("only a transient thermal study has time steps".to_owned()),
        (_, None) => TransientSettings::default(),
    };
    let reference_temperature = match (study_type, dto.reference_temperature) {
        (StudyType::ThermalStress, Some(t)) => celsius(t, "the reference temperature")?,
        (_, Some(_)) => {
            return Err("only a thermal stress study has a reference temperature".to_owned());
        }
        (_, None) => 20.0,
    };

    let flow = match dto.flow {
        Some(f) => flow_from(f)?,
        None => FlowSettings::default(),
    };
    let mut conditions = Vec::with_capacity(dto.conditions.len());
    for c in dto.conditions {
        named(&c.name)?;
        let item = |e: String| format!("{}: {e}", c.name);
        let not_negative = |value: f64, what: &str| -> Result<f64, String> {
            let value = finite(value, what)?;
            if value < 0.0 {
                return Err(format!("{what} is negative"));
            }
            Ok(value)
        };
        let kind = match c.kind {
            ConditionKindDto::InletVelocity { velocity } => FlowConditionKind::InletVelocity {
                speed: not_negative(velocity, "the velocity").map_err(item)?,
            },
            ConditionKindDto::InletVolumeFlow { volume_flow } => {
                FlowConditionKind::InletVolumeFlow {
                    rate: not_negative(volume_flow, "the volume flow").map_err(item)?,
                }
            }
            ConditionKindDto::InletMassFlow { mass_flow } => FlowConditionKind::InletMassFlow {
                rate: not_negative(mass_flow, "the mass flow").map_err(item)?,
            },
            ConditionKindDto::StaticPressure { pressure } => FlowConditionKind::StaticPressure {
                pressure: finite(pressure, "the pressure").map_err(item)?,
            },
            ConditionKindDto::IdealWall => FlowConditionKind::IdealWall,
        };
        conditions.push(FlowCondition {
            kind,
            faces: faces_from(c.faces, kind_of_model).map_err(item)?,
            suppressed: c.suppressed,
            name: c.name,
        });
    }
    let mut goals = Vec::with_capacity(dto.goals.len());
    for g in dto.goals {
        named(&g.name)?;
        goals.push(FlowGoal {
            kind: g.kind.kind(),
            faces: faces_from(g.faces, kind_of_model).map_err(|e| format!("{}: {e}", g.name))?,
            name: g.name,
        });
    }

    Ok(Study {
        name: dto.name,
        study_type,
        fixtures,
        loads,
        contacts,
        mesh: MeshSettings {
            mesher: match mesh.mesher {
                MesherDto::Netgen => Mesher::Netgen,
                MesherDto::Lattice => Mesher::Lattice,
            },
            quality: match mesh.quality {
                QualityDto::High => MeshQuality::High,
                QualityDto::Draft => MeshQuality::Draft,
            },
            size,
        },
        materials,
        thermal_materials,
        transient,
        reference_temperature,
        flow,
        conditions,
        goals,
    })
}

/// A flow study's settings, checked.
fn flow_from(f: FlowDto) -> Result<FlowSettings, String> {
    let analysis = match (f.analysis, f.velocity) {
        (AnalysisDto::Internal, None) => FlowAnalysis::Internal,
        (AnalysisDto::Internal, Some(_)) => {
            return Err("an internal flow has a free stream's velocity".to_owned());
        }
        (AnalysisDto::External, Some(v)) => FlowAnalysis::External {
            velocity: finite_vector(v, "the free stream's velocity")?,
        },
        (AnalysisDto::External, None) => {
            return Err("an external flow has no free stream's velocity".to_owned());
        }
    };
    let density = finite(f.fluid.density, "the fluid's density")?;
    let viscosity = finite(f.fluid.viscosity, "the fluid's viscosity")?;
    if density <= 0.0 || viscosity <= 0.0 {
        return Err("the fluid's density and viscosity are not positive".to_owned());
    }
    let turbulence = match (f.turbulence, f.intensity, f.length) {
        (TurbulenceDto::Laminar, None, None) => FlowTurbulence::Laminar,
        (TurbulenceDto::Laminar, _, _) => {
            return Err("a laminar flow has a turbulence intensity or length".to_owned());
        }
        (TurbulenceDto::Turbulent, Some(intensity), Some(length)) => {
            let intensity = finite(intensity, "the turbulence intensity")?;
            let length = finite(length, "the turbulence length")?;
            if !(intensity > 0.0 && intensity <= 1.0) {
                return Err("the turbulence intensity is not above 0 and up to 1".to_owned());
            }
            if length <= 0.0 {
                return Err("the turbulence length is not positive".to_owned());
            }
            FlowTurbulence::Turbulent { intensity, length }
        }
        (TurbulenceDto::Turbulent, _, _) => {
            return Err("a turbulent flow needs a turbulence intensity and length".to_owned());
        }
    };
    if !(1..=7).contains(&f.mesh_level) {
        return Err("the mesh level is not from 1 to 7".to_owned());
    }
    if !(1..=100_000).contains(&f.iterations) {
        return Err("the iterations are not from 1 to 100 000".to_owned());
    }
    let residual = finite(f.residual, "the residual")?;
    if residual <= 0.0 {
        return Err("the residual is not positive".to_owned());
    }
    Ok(FlowSettings {
        analysis,
        fluid: Fluid {
            name: f.fluid.name,
            density,
            viscosity,
        },
        turbulence,
        mesh_level: f.mesh_level,
        iterations: f.iterations,
        residual,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SIMULATION_FORMAT_VERSION;
    use oxyde_model::FeatureId;

    fn face(feature: u32) -> FaceName {
        FaceName::end_cap(FeatureId::from_raw(feature))
    }

    /// An assembly study touching every kind of item.
    fn sample() -> SimulationFile {
        let mut file = SimulationFile::new(StudiedModel {
            kind: ModelKind::Assembly,
            path: "robot.oxyasm".to_owned(),
            document_id: "33333333-3333-3333-3333-333333333333".to_owned(),
        });
        let base = |f| StudyFace::component(ComponentId(1), Vec::new(), face(f));
        let deep = StudyFace::component(
            ComponentId(3),
            vec![ComponentId(2), ComponentId(1)],
            FaceName::profile(FeatureId::from_raw(2), 4),
        );
        let mut study = Study::new("Static 1");
        study.fixtures = vec![
            Fixture {
                name: "Fixed-1".to_owned(),
                kind: FixtureKind::Fixed,
                faces: vec![base(2)],
                suppressed: false,
            },
            Fixture {
                name: "Roller/Slider-1".to_owned(),
                kind: FixtureKind::Roller,
                faces: vec![deep.clone()],
                suppressed: true,
            },
        ];
        study.loads = vec![
            Load {
                name: "Force-1".to_owned(),
                kind: LoadKind::Force {
                    faces: vec![base(5), deep.clone()],
                    newtons: -250.0,
                    direction: ForceDirection::Vector([0.0, 1.0, 0.0]),
                },
                suppressed: false,
            },
            Load {
                name: "Force-2".to_owned(),
                kind: LoadKind::Force {
                    faces: vec![base(6)],
                    newtons: 10.5,
                    direction: ForceDirection::Normal,
                },
                suppressed: false,
            },
            Load {
                name: "Pressure-1".to_owned(),
                kind: LoadKind::Pressure {
                    faces: vec![base(7)],
                    mpa: 0.2,
                },
                suppressed: false,
            },
            Load {
                name: "Gravity-1".to_owned(),
                kind: LoadKind::Gravity {
                    acceleration: [0.0, -9.81, 0.0],
                },
                suppressed: false,
            },
        ];
        study.contacts = vec![
            Contact {
                name: "Contact-1".to_owned(),
                kind: ContactKind::Bonded,
                a: vec![base(8)],
                b: vec![deep.clone()],
                offset: 0.0,
                detection: ContactDetection::Automatic { tolerance: 0.05 },
                suppressed: false,
            },
            Contact {
                name: "Contact-2".to_owned(),
                kind: ContactKind::OffsetBonded,
                a: vec![base(9)],
                b: vec![deep],
                offset: 0.4,
                detection: ContactDetection::Manual,
                suppressed: false,
            },
        ];
        study.mesh = MeshSettings {
            mesher: Mesher::Lattice,
            quality: MeshQuality::Draft,
            size: MeshSize::ElementSize(2.5),
        };
        study.materials.insert(
            "Aluminium 6061-T6".to_owned(),
            Mechanical::new(68900.0, 0.33, 275.0, 310.0),
        );
        study.materials.insert(
            "part:11111111-1111-1111-1111-111111111111".to_owned(),
            Mechanical::new(2000.0, 0.35, 0.0, 0.0),
        );
        let mut thermal = Study::of_type("Transient 1", StudyType::TransientThermal);
        thermal.transient = TransientSettings {
            duration: 120.0,
            steps: 40,
            initial_temperature: 25.0,
        };
        thermal.loads = vec![
            Load {
                name: "Temperature-1".to_owned(),
                kind: LoadKind::Temperature {
                    faces: vec![base(2)],
                    celsius: 80.0,
                },
                suppressed: false,
            },
            Load {
                name: "Heat Power-1".to_owned(),
                kind: LoadKind::HeatPower {
                    faces: vec![base(3)],
                    watts: 12.5,
                },
                suppressed: false,
            },
            Load {
                name: "Heat Flux-1".to_owned(),
                kind: LoadKind::HeatFlux {
                    faces: vec![base(4)],
                    flux: -1500.0,
                },
                suppressed: false,
            },
            Load {
                name: "Convection-1".to_owned(),
                kind: LoadKind::Convection {
                    faces: vec![base(5)],
                    coefficient: 25.0,
                    ambient: 20.0,
                },
                suppressed: false,
            },
            Load {
                name: "Radiation-1".to_owned(),
                kind: LoadKind::Radiation {
                    faces: vec![base(6)],
                    emissivity: 0.9,
                    ambient: 15.0,
                },
                suppressed: true,
            },
            Load {
                name: "Internal Heat-1".to_owned(),
                kind: LoadKind::InternalHeat {
                    faces: vec![base(7)],
                    watts: 5.0,
                },
                suppressed: false,
            },
        ];
        thermal.thermal_materials.insert(
            "Copper (C110)".to_owned(),
            Thermal::new(391.0, 385.0, 17e-6),
        );
        let mut stress = Study::of_type("Thermal Stress 1", StudyType::ThermalStress);
        stress.reference_temperature = 22.0;
        stress.fixtures.push(Fixture {
            name: "Fixed-1".to_owned(),
            kind: FixtureKind::Fixed,
            faces: vec![base(2)],
            suppressed: false,
        });
        stress.loads.push(Load {
            name: "Temperature-1".to_owned(),
            kind: LoadKind::Temperature {
                faces: vec![base(3)],
                celsius: 150.0,
            },
            suppressed: false,
        });
        let mut second = Study::new("Static 2");
        second.contacts.push(Contact {
            name: "Contact-1".to_owned(),
            kind: ContactKind::Separation,
            a: Vec::new(),
            b: Vec::new(),
            offset: 0.0,
            detection: ContactDetection::Manual,
            suppressed: false,
        });
        file.studies = vec![study, second, thermal, stress];
        file
    }

    fn bytes(file: &SimulationFile) -> Vec<u8> {
        write_simulation(Cursor::new(Vec::new()), file)
            .unwrap()
            .into_inner()
    }

    fn json(file: &SimulationFile) -> serde_json::Value {
        let mut archive = ZipArchive::new(Cursor::new(bytes(file))).unwrap();
        assert_eq!(archive.by_index(0).unwrap().name(), MANIFEST);
        serde_json::from_slice(&read_entry(&mut archive, SIMULATION).unwrap()).unwrap()
    }

    /// `bytes` with one entry's text changed.
    fn edited(bytes: &[u8], entry: &str, edit: impl Fn(&str) -> String) -> Vec<u8> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let name = file.name().to_owned();
            let mut content = Vec::new();
            file.read_to_end(&mut content).unwrap();
            if name == entry {
                content = edit(std::str::from_utf8(&content).unwrap()).into_bytes();
            }
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(&content).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn studies_round_trip() {
        let file = sample();
        assert_eq!(read_simulation(Cursor::new(bytes(&file))).unwrap(), file);
        // And a part study, with the default mesh settings.
        let mut part = SimulationFile::new(StudiedModel {
            kind: ModelKind::Part,
            path: "parts/bracket.oxyprt".to_owned(),
            document_id: "44444444-4444-4444-4444-444444444444".to_owned(),
        });
        let mut study = Study::new("Static 1");
        study.fixtures.push(Fixture {
            name: "Fixed-1".to_owned(),
            kind: FixtureKind::Fixed,
            faces: vec![StudyFace::part(face(2))],
            suppressed: false,
        });
        part.studies.push(study);
        assert_eq!(read_simulation(Cursor::new(bytes(&part))).unwrap(), part);
    }

    #[test]
    fn the_json_is_as_documented() {
        let v = json(&sample());
        assert_eq!(v["model"]["kind"], "assembly");
        let study = &v["studies"][0];
        assert_eq!(study["type"], "static");
        assert_eq!(study["fixtures"][1]["type"], "roller");
        assert_eq!(study["fixtures"][1]["suppressed"], true);
        assert!(study["fixtures"][0].get("suppressed").is_none());
        assert_eq!(study["fixtures"][1]["faces"][0]["component"], 3);
        assert_eq!(
            study["fixtures"][1]["faces"][0]["path"],
            serde_json::json!([2, 1])
        );
        assert_eq!(
            study["fixtures"][0]["faces"][0]["face"]["key"]["type"],
            "end_cap"
        );
        assert_eq!(study["loads"][0]["type"], "force");
        assert_eq!(study["loads"][0]["force"], -250.0);
        assert_eq!(
            study["loads"][0]["direction"],
            serde_json::json!([0.0, 1.0, 0.0])
        );
        assert_eq!(study["loads"][1]["direction"], "normal");
        assert_eq!(study["loads"][2]["pressure"], 0.2);
        assert_eq!(study["loads"][3]["type"], "gravity");
        assert_eq!(study["contacts"][0]["automatic"], true);
        assert_eq!(study["contacts"][0]["tolerance"], 0.05);
        assert_eq!(study["contacts"][1]["type"], "offset_bonded");
        assert_eq!(study["contacts"][1]["offset"], 0.4);
        assert_eq!(study["mesh"]["mesher"], "lattice");
        assert_eq!(study["mesh"]["element_size"], 2.5);
        assert!(study["mesh"].get("density").is_none());
        assert_eq!(
            study["materials"]["Aluminium 6061-T6"]["elastic_modulus"],
            68900.0
        );
        // Unknown strengths are left out, as in a material's own block.
        let part_group = &study["materials"]["part:11111111-1111-1111-1111-111111111111"];
        assert!(part_group.get("yield_strength").is_none());
        assert!(study.get("transient").is_none() && study.get("reference_temperature").is_none());

        let thermal = &v["studies"][2];
        assert_eq!(thermal["type"], "transient_thermal");
        assert_eq!(thermal["transient"]["duration"], 120.0);
        assert_eq!(thermal["transient"]["steps"], 40);
        assert_eq!(thermal["transient"]["initial_temperature"], 25.0);
        let loads = &thermal["loads"];
        assert_eq!(loads[0]["type"], "temperature");
        assert_eq!(loads[0]["temperature"], 80.0);
        assert_eq!(loads[1]["type"], "heat_power");
        assert_eq!(loads[1]["power"], 12.5);
        assert_eq!(loads[2]["type"], "heat_flux");
        assert_eq!(loads[2]["flux"], -1500.0);
        assert_eq!(loads[3]["type"], "convection");
        assert_eq!(
            (&loads[3]["coefficient"], &loads[3]["ambient"]),
            (&serde_json::json!(25.0), &serde_json::json!(20.0))
        );
        assert_eq!(loads[4]["type"], "radiation");
        assert_eq!(loads[4]["emissivity"], 0.9);
        assert_eq!(loads[5]["type"], "internal_heat");
        assert_eq!(
            thermal["thermal_materials"]["Copper (C110)"]["conductivity"],
            391.0
        );
        let stress = &v["studies"][3];
        assert_eq!(stress["type"], "thermal_stress");
        assert_eq!(stress["reference_temperature"], 22.0);
    }

    #[test]
    fn static_studies_are_written_as_version_1() {
        let mut file = sample();
        assert_eq!(simulation_version(&file), 2);
        file.studies.truncate(2);
        assert_eq!(simulation_version(&file), 1);
        let mut archive = ZipArchive::new(Cursor::new(bytes(&file))).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&read_entry(&mut archive, MANIFEST).unwrap()).unwrap();
        assert_eq!(manifest["format_version"], 1);
    }

    #[test]
    fn hand_written_studies_get_defaults() {
        let file = SimulationFile::new(StudiedModel {
            kind: ModelKind::Part,
            path: "a.oxyprt".to_owned(),
            document_id: "x".to_owned(),
        });
        let text = r#"{ "model": { "kind": "part", "path": "a.oxyprt", "document_id": "x" },
            "studies": [ { "name": "Static 1", "type": "static", "future": 1,
              "loads": [ { "name": "Force-1", "type": "force", "force": 5,
                "direction": "normal",
                "faces": [ { "face": { "key": { "type": "end_cap", "feature": 2 } } } ] } ] } ] }"#;
        let read = read_simulation(Cursor::new(edited(&bytes(&file), SIMULATION, |_| {
            text.to_owned()
        })))
        .unwrap();
        let study = &read.studies[0];
        assert_eq!(study.mesh, MeshSettings::default());
        assert!(study.fixtures.is_empty() && study.contacts.is_empty());
        assert_eq!(
            study.loads[0].kind,
            LoadKind::Force {
                faces: vec![StudyFace::part(face(2))],
                newtons: 5.0,
                direction: ForceDirection::Normal,
            }
        );
    }

    #[test]
    fn newer_versions_and_other_kinds_are_refused() {
        let good = bytes(&sample());
        let newer = edited(&good, MANIFEST, |m| {
            m.replace(
                &format!("\"format_version\": {THERMAL_FORMAT_VERSION}"),
                &format!("\"format_version\": {}", SIMULATION_FORMAT_VERSION + 1),
            )
        });
        assert!(matches!(
            read_simulation(Cursor::new(newer)),
            Err(FormatError::UnsupportedVersion { .. })
        ));
        let part = crate::write_part(Cursor::new(Vec::new()), &crate::PartFile::new())
            .unwrap()
            .into_inner();
        match read_simulation(Cursor::new(part)) {
            Err(FormatError::WrongKind { found, .. }) => assert_eq!(found, "part"),
            other => panic!("expected the wrong kind, got {other:?}"),
        }
        match crate::read_part(Cursor::new(good)) {
            Err(FormatError::WrongKind { found, .. }) => assert_eq!(found, "simulation"),
            other => panic!("expected the wrong kind, got {other:?}"),
        }
    }

    #[test]
    fn inconsistent_studies_are_refused() {
        let good = bytes(&sample());
        let refused = |edit: &dyn Fn(&str) -> String, why: &str| match read_simulation(Cursor::new(
            edited(&good, SIMULATION, edit),
        )) {
            Err(FormatError::InvalidSimulation(e)) => assert!(e.contains(why), "{e}"),
            other => panic!("expected an invalid study ({why}), got {other:?}"),
        };
        refused(&|s| s.replace("Force-2", "Force-1"), "two items");
        refused(&|s| s.replace("Static 2", "Static 1"), "two studies");
        refused(
            &|s| s.replace("\"assembly\"", "\"part\""),
            "names a component",
        );
        refused(
            &|s| s.replace("\"element_size\": 2.5", "\"element_size\": -1"),
            "element size",
        );
        refused(&|s| s.replace("0.05", "0"), "tolerance");
        refused(
            &|s| s.replace("\"offset\": 0.4", "\"offset\": -0.4"),
            "offset",
        );
        refused(
            &|s| s.replace("\"direction\": \"normal\"", "\"direction\": [0, 0, 0]"),
            "zero vector",
        );
        refused(
            &|s| s.replace("\"emissivity\": 0.9", "\"emissivity\": 1.5"),
            "emissivity",
        );
        refused(
            &|s| s.replace("\"temperature\": 80.0", "\"temperature\": -300.0"),
            "absolute zero",
        );
        refused(
            &|s| s.replace("\"steps\": 40", "\"steps\": 0"),
            "time steps",
        );
        refused(
            &|s| s.replace("\"type\": \"transient_thermal\"", "\"type\": \"static\""),
            "thermal load",
        );
        refused(
            &|s| s.replace("\"type\": \"thermal_stress\"", "\"type\": \"thermal\""),
            "fixtures",
        );
        // An unknown study or contact type is an error, as unknown features are.
        assert!(matches!(
            read_simulation(Cursor::new(
                edited(&good, SIMULATION, |s| s.replace("\"static\"", "\"modal\""))
            )),
            Err(FormatError::Json { .. })
        ));
    }

    /// A part's flow studies: an internal one with every condition and goal, an external
    /// one, turbulent, with a goal.
    fn flow_sample() -> SimulationFile {
        let mut file = SimulationFile::new(StudiedModel {
            kind: ModelKind::Part,
            path: "valve.oxyprt".to_owned(),
            document_id: "66666666-6666-6666-6666-666666666666".to_owned(),
        });
        let on = |f| vec![StudyFace::part(face(f))];
        let mut internal = Study::of_type("Flow 1", StudyType::Flow);
        internal.conditions = vec![
            FlowCondition {
                name: "Inlet Velocity-1".to_owned(),
                kind: FlowConditionKind::InletVelocity { speed: 2.5 },
                faces: on(2),
                suppressed: false,
            },
            FlowCondition {
                name: "Inlet Volume Flow-1".to_owned(),
                kind: FlowConditionKind::InletVolumeFlow { rate: 1e-4 },
                faces: on(3),
                suppressed: true,
            },
            FlowCondition {
                name: "Inlet Mass Flow-1".to_owned(),
                kind: FlowConditionKind::InletMassFlow { rate: 0.1 },
                faces: on(4),
                suppressed: false,
            },
            FlowCondition {
                name: "Static Pressure-1".to_owned(),
                kind: FlowConditionKind::StaticPressure { pressure: -150.0 },
                faces: on(5),
                suppressed: false,
            },
            FlowCondition {
                name: "Ideal Wall-1".to_owned(),
                kind: FlowConditionKind::IdealWall,
                faces: on(6),
                suppressed: false,
            },
        ];
        internal.goals = vec![
            FlowGoal {
                name: "Pressure Goal-1".to_owned(),
                kind: FlowGoalKind::MeanPressure,
                faces: on(2),
            },
            FlowGoal {
                name: "Flow Goal-1".to_owned(),
                kind: FlowGoalKind::VolumeFlow,
                faces: on(5),
            },
            FlowGoal {
                name: "Mass Flow Goal-1".to_owned(),
                kind: FlowGoalKind::MassFlow,
                faces: on(5),
            },
        ];
        let mut external = Study::of_type("Flow 2", StudyType::Flow);
        external.flow = FlowSettings {
            analysis: FlowAnalysis::External {
                velocity: [12.0, 0.0, -0.5],
            },
            fluid: Fluid::air(),
            turbulence: FlowTurbulence::Turbulent {
                intensity: 0.02,
                length: 3.0,
            },
            mesh_level: 5,
            iterations: 1200,
            residual: 1e-5,
        };
        external.goals.push(FlowGoal {
            name: "Force Goal-1".to_owned(),
            kind: FlowGoalKind::Force,
            faces: on(7),
        });
        file.studies = vec![internal, external];
        file
    }

    #[test]
    fn flow_studies_round_trip_as_version_5() {
        let file = flow_sample();
        assert_eq!(read_simulation(Cursor::new(bytes(&file))).unwrap(), file);
        assert_eq!(simulation_version(&file), 5);
        let mut both = sample();
        both.studies.extend(flow_sample().studies);
        assert_eq!(simulation_version(&both), 5);
        let mut archive = ZipArchive::new(Cursor::new(bytes(&file))).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&read_entry(&mut archive, MANIFEST).unwrap()).unwrap();
        assert_eq!(manifest["format_version"], 5);
    }

    #[test]
    fn flow_studies_are_written_as_documented() {
        let v = json(&flow_sample());
        let internal = &v["studies"][0];
        assert_eq!(internal["type"], "flow");
        // A flow study has its own mesh setting, and none of the other types' items.
        assert!(internal.get("mesh").is_none());
        assert!(
            internal
                .get("fixtures")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        let flow = &internal["flow"];
        assert_eq!(flow["analysis"], "internal");
        assert!(flow.get("velocity").is_none());
        assert_eq!(flow["fluid"]["name"], "Water");
        assert_eq!(flow["fluid"]["density"], 998.2);
        assert_eq!(flow["fluid"]["viscosity"], 1.002e-3);
        assert_eq!(flow["turbulence"], "laminar");
        assert!(flow.get("intensity").is_none());
        assert_eq!(flow["mesh_level"], 3);
        assert_eq!(flow["iterations"], 500);
        assert_eq!(flow["residual"], 1e-4);
        let c = &internal["conditions"];
        assert_eq!(c[0]["type"], "inlet_velocity");
        assert_eq!(c[0]["velocity"], 2.5);
        assert_eq!(c[0]["faces"][0]["face"]["key"]["type"], "end_cap");
        assert_eq!(c[1]["type"], "inlet_volume_flow");
        assert_eq!(c[1]["volume_flow"], 1e-4);
        assert_eq!(c[1]["suppressed"], true);
        assert_eq!(c[2]["type"], "inlet_mass_flow");
        assert_eq!(c[2]["mass_flow"], 0.1);
        assert_eq!(c[3]["type"], "static_pressure");
        assert_eq!(c[3]["pressure"], -150.0);
        assert_eq!(c[4]["type"], "ideal_wall");
        assert_eq!(internal["goals"][0]["type"], "mean_pressure");
        assert_eq!(internal["goals"][1]["type"], "volume_flow");
        assert_eq!(internal["goals"][2]["type"], "mass_flow");
        let external = &v["studies"][1];
        assert_eq!(external["flow"]["analysis"], "external");
        assert_eq!(
            external["flow"]["velocity"],
            serde_json::json!([12.0, 0.0, -0.5])
        );
        assert_eq!(external["flow"]["turbulence"], "turbulent");
        assert_eq!(external["flow"]["intensity"], 0.02);
        assert_eq!(external["flow"]["length"], 3.0);
        assert_eq!(external["goals"][0]["type"], "force");
        assert!(external.get("conditions").is_none());

        // The other types are written exactly as before: no flow keys, their mesh.
        let v = json(&sample());
        for study in v["studies"].as_array().unwrap() {
            assert!(study.get("flow").is_none() && study.get("conditions").is_none());
            assert!(study.get("goals").is_none() && study.get("mesh").is_some());
        }
    }

    #[test]
    fn inconsistent_flow_studies_are_refused() {
        let good = bytes(&flow_sample());
        let refused = |edit: &dyn Fn(&str) -> String, why: &str| match read_simulation(Cursor::new(
            edited(&good, SIMULATION, edit),
        )) {
            Err(FormatError::InvalidSimulation(e)) => assert!(e.contains(why), "{e}"),
            other => panic!("expected an invalid study ({why}), got {other:?}"),
        };
        refused(
            &|s| s.replace("\"mesh_level\": 5", "\"mesh_level\": 9"),
            "mesh level",
        );
        refused(
            &|s| s.replace("\"velocity\": 2.5", "\"velocity\": -2.5"),
            "velocity is negative",
        );
        refused(
            &|s| s.replace("\"density\": 998.2", "\"density\": 0"),
            "density",
        );
        refused(
            &|s| s.replace("\"intensity\": 0.02", "\"intensity\": 2"),
            "intensity",
        );
        refused(
            &|s| s.replace("\"analysis\": \"internal\"", "\"analysis\": \"external\""),
            "no free stream",
        );
        refused(
            &|s| {
                s.replace(
                    "\"turbulence\": \"laminar\"",
                    "\"turbulence\": \"turbulent\"",
                )
            },
            "needs a turbulence intensity",
        );
        refused(
            &|s| s.replace("Flow Goal-1", "Inlet Velocity-1"),
            "two items",
        );
        refused(
            &|s| s.replacen("\"type\": \"flow\"", "\"type\": \"static\"", 1),
            "only a flow study",
        );
        // A flow study holds no fixtures, loads or contacts.
        let mut fixed = flow_sample();
        fixed.studies[0].fixtures.push(Fixture {
            name: "Fixed-1".to_owned(),
            kind: FixtureKind::Fixed,
            faces: Vec::new(),
            suppressed: false,
        });
        match read_simulation(Cursor::new(bytes(&fixed))) {
            Err(FormatError::InvalidSimulation(e)) => {
                assert!(e.contains("flow study has fixtures"), "{e}")
            }
            other => panic!("expected fixtures refused, got {other:?}"),
        }
    }

    #[test]
    fn unusable_material_overrides_are_dropped() {
        let good = bytes(&sample());
        let read = read_simulation(Cursor::new(edited(&good, SIMULATION, |s| {
            s.replace("\"poisson_ratio\": 0.35", "\"poisson_ratio\": 0.7")
        })))
        .unwrap();
        assert_eq!(
            read.studies[0].materials.keys().collect::<Vec<_>>(),
            ["Aluminium 6061-T6"]
        );
    }

    #[test]
    fn save_and_load_through_the_file_system() {
        let path = std::env::temp_dir().join(format!("oxyde-study-{}.oxysim", std::process::id()));
        let file = sample();
        save_simulation(&path, &file).unwrap();
        let loaded = load_simulation(&path);
        let kind = crate::document_kind(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.unwrap(), file);
        assert_eq!(
            kind.unwrap(),
            (DocumentKind::Simulation, file.document_id.clone())
        );

        // A value JSON cannot hold is refused before anything is written.
        let mut bad = sample();
        if let LoadKind::Pressure { mpa, .. } = &mut bad.studies[0].loads[2].kind {
            *mpa = f64::NAN;
        }
        assert!(matches!(
            save_simulation(&path, &bad),
            Err(FormatError::Unreadable(_))
        ));
        assert!(!path.exists());
    }
}
