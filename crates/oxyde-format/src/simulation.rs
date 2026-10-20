//! Simulation studies (`.oxysim`): what holds a part or an assembly, what loads it (forces
//! and pressures, or heat), how its bodies touch, how it is meshed and which mechanical and
//! thermal properties the study uses instead of the materials' own. A document holds any
//! number of studies of six types: static, thermal, transient thermal, thermal stress,
//! topology (the lightest shape that carries a static study's loads) and generative design
//! (shapes grown between bodies to keep, for several materials and ways of making them).
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
use oxyde_model::{BodyId, FaceName, Material, Mechanical, Thermal};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::schema::{
    BodyIdDto, FaceRefDto, MaterialDto, MechanicalDto, TextureTable, ThermalDto, body_dto,
    body_from, face_dto, face_from, material_dto, material_from,
};
use crate::{
    DocumentKind, FORMAT_VERSION, FormatError, MANIFEST, SIMULATION_FORMAT_VERSION, expect_kind,
    from_json, manifest_version, new_document_id, read_entry, read_manifest, to_json, write_file,
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
    /// Topology optimization: the stiffest shape that keeps a share of the model's mass
    /// under fixtures, forces and pressures ([`Study::topology`]).
    Topology,
    /// Generative design: shapes grown between bodies to keep and bodies to stay clear of,
    /// for several materials and ways of making them ([`Study::generative`]). Only a part
    /// has them.
    Generative,
}

impl StudyType {
    /// Whether it takes fixtures, forces and pressures.
    pub fn is_structural(self) -> bool {
        matches!(
            self,
            StudyType::Static
                | StudyType::ThermalStress
                | StudyType::Topology
                | StudyType::Generative
        )
    }

    /// Whether it takes gravity: topology and generative studies do not, their weight
    /// changing with the shape.
    pub fn takes_gravity(self) -> bool {
        matches!(self, StudyType::Static | StudyType::ThermalStress)
    }

    /// Whether its results are displacements and stresses (topology and generative
    /// studies' are densities).
    pub fn has_stresses(self) -> bool {
        matches!(self, StudyType::Static | StudyType::ThermalStress)
    }

    /// Whether it takes thermal loads.
    pub fn is_thermal(self) -> bool {
        matches!(
            self,
            StudyType::Thermal | StudyType::TransientThermal | StudyType::ThermalStress
        )
    }
}

/// What a topology study looks for.
#[derive(Debug, Clone, PartialEq)]
pub struct TopologySettings {
    /// The share of the model's mass to keep, between 0 and 1 (both excluded).
    pub target_mass: f64,
    /// mm: the thinnest member wanted; `None` for the finest the mesh allows.
    pub member_size: Option<f64>,
    /// Iterations at most, each one static solve.
    pub iterations: u32,
    /// Faces whose material stays.
    pub preserved: Vec<PreservedRegion>,
    /// Planes the shape is mirrored about; `None` for none.
    pub symmetry: Option<SymmetryPlanes>,
    /// The direction a mould is pulled off the shape along; `None` for none.
    pub pull: Option<PullDirection>,
    /// *Keep it in one piece*: pieces of the shape holding a fixture, a load or a preserved
    /// region joined to the one the loads are on, loose bits taken away.
    pub one_piece: bool,
}

impl Default for TopologySettings {
    fn default() -> Self {
        Self {
            target_mass: 0.5,
            member_size: None,
            iterations: 60,
            preserved: Vec::new(),
            symmetry: None,
            pull: None,
            one_piece: false,
        }
    }
}

/// Planes a topology or generative study's shape is mirrored about: through `point`,
/// parallel to the standard planes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SymmetryPlanes {
    /// Parallel to the Right plane (normal to X), the Top plane (Y) and the Front plane
    /// (Z); at least one.
    pub planes: [bool; 3],
    /// mm, in world coordinates; the origin for the standard planes themselves.
    pub point: [f64; 3],
}

/// The direction a mould is pulled off a topology study's shape along: going that way the
/// shape never widens, so it has no undercut.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PullDirection {
    /// In world coordinates (Y up); only its direction counts, not zero.
    pub direction: [f64; 3],
    /// mm: a two-part mould, pulled off both ways from the plane through this point across
    /// the direction; `None` for one way.
    pub parting: Option<[f64; 3]>,
}

/// Faces a topology study keeps material under: the elements on them, and those within
/// `depth` of them.
#[derive(Debug, Clone, PartialEq)]
pub struct PreservedRegion {
    /// `Preserve-1`… unique like a fixture's.
    pub name: String,
    pub faces: Vec<StudyFace>,
    /// mm, 0 for the elements on the faces only.
    pub depth: f64,
    pub suppressed: bool,
}

/// What a generative study grows, as Fusion 360 has it: shapes between the bodies to keep and
/// those to stay clear of, as stiff as can be for a mass, for each material and each way of
/// making them.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerativeSettings {
    /// Bodies kept as they are; at least one. Fixtures and loads are on their faces.
    pub preserve: Vec<StudyBody>,
    /// Bodies no material may go into.
    pub obstacles: Vec<GenerativeObstacle>,
    /// A body the material starts in and stays inside; `None` for the box around the
    /// preserve bodies, grown by [`GenerativeSettings::margin`].
    pub starting_shape: Option<StudyBody>,
    /// mm the box around the preserve bodies is grown by on every side, without a starting
    /// shape; `None` for a tenth of the box's largest side.
    pub margin: Option<f64>,
    pub objective: GenerativeObjective,
    /// kg: every outcome's mass, preserve bodies included, whatever its material, when
    /// maximizing stiffness; positive (0 in a new study until it is set, and while
    /// minimizing mass, which does not need it).
    pub mass_target: f64,
    /// What each outcome's estimated factor of safety is held against: its stress kept
    /// under the material's strength over it when minimizing mass.
    pub safety_factor: f64,
    /// mm: the most an outcome may move when minimizing mass; `None` for no limit.
    pub displacement_limit: Option<f64>,
    /// mm: the minimum thickness; `None` for three voxels.
    pub member_size: Option<f64>,
    /// Iterations at most, for each outcome.
    pub iterations: u32,
    /// The voxels' size, as a mesh's elements': from coarse (0) to fine (1) relative to the
    /// design space, or an edge in mm.
    pub resolution: MeshSize,
    /// Planes the shapes are mirrored about; `None` for none.
    pub symmetry: Option<SymmetryPlanes>,
    /// The ways of making the shapes; at least one, no kind twice.
    pub methods: Vec<GenerativeMethod>,
    /// The materials to generate for, each with a density and mechanical properties, names
    /// unique; empty for the part's own material. Copied in, so the study never depends on
    /// a library; without their textures (a study document holds no images).
    pub materials: Vec<Material>,
    /// The load cases, each carried in turn; empty for one case of every fixture and load.
    pub load_cases: Vec<LoadCase>,
}

/// Fixtures and loads a generative study's outcomes carry at once, as Fusion 360's load
/// cases: the stiffest shape for every case, its compliance their sum.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadCase {
    /// `Load Case 1`…, unique in the study.
    pub name: String,
    /// The names of the study's fixtures it holds, at least one.
    pub fixtures: Vec<String>,
    /// The names of the study's loads it holds, at least one.
    pub loads: Vec<String>,
}

impl Default for GenerativeSettings {
    fn default() -> Self {
        Self {
            preserve: Vec::new(),
            obstacles: Vec::new(),
            starting_shape: None,
            margin: None,
            objective: GenerativeObjective::MaxStiffness,
            mass_target: 0.0,
            safety_factor: 2.0,
            displacement_limit: None,
            member_size: None,
            iterations: 150,
            resolution: MeshSize::Automatic { density: 0.5 },
            symmetry: None,
            methods: vec![GenerativeMethod::Unrestricted],
            materials: Vec::new(),
            load_cases: Vec::new(),
        }
    }
}

/// A body of the studied model: of the part, or of a part in the assembly, named as faces
/// are ([`StudyFace`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StudyBody {
    /// The assembly's component holding it; `None` in a part study.
    pub component: Option<ComponentId>,
    /// Inside a sub-assembly: its component ids down to the part.
    pub path: Vec<ComponentId>,
    pub body: BodyId,
}

impl StudyBody {
    /// A body of the studied part.
    pub fn part(body: BodyId) -> Self {
        Self {
            component: None,
            path: Vec::new(),
            body,
        }
    }

    /// A body of a part in the studied assembly.
    pub fn component(component: ComponentId, path: Vec<ComponentId>, body: BodyId) -> Self {
        Self {
            component: Some(component),
            path,
            body,
        }
    }
}

impl std::fmt::Display for StudyBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(c) = self.component {
            write!(f, "component {}", c.0)?;
            for p in &self.path {
                write!(f, "/{}", p.0)?;
            }
            write!(f, " ")?;
        }
        write!(f, "{}", self.body)
    }
}

/// A body a generative study's material stays out of.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerativeObstacle {
    pub body: StudyBody,
    /// mm it is grown by on every side; 0 for none.
    pub offset: f64,
}

/// What a generative study looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GenerativeObjective {
    /// The stiffest shape for the mass target.
    #[default]
    MaxStiffness,
    /// The lightest shape that keeps its stress under the material's strength over the
    /// safety factor, and its displacement under the limit when there is one.
    MinMass,
}

/// One of the six directions along the world axes (Y up).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AxisDirection {
    #[serde(rename = "+x")]
    PlusX,
    #[serde(rename = "-x")]
    MinusX,
    #[serde(rename = "+y")]
    PlusY,
    #[serde(rename = "-y")]
    MinusY,
    #[serde(rename = "+z")]
    PlusZ,
    #[serde(rename = "-z")]
    MinusZ,
}

impl AxisDirection {
    /// The direction along axis `axis` (0 for X, 1 for Y, 2 for Z; Z beyond), one way.
    pub fn new(axis: usize, positive: bool) -> Self {
        match (axis, positive) {
            (0, true) => AxisDirection::PlusX,
            (0, false) => AxisDirection::MinusX,
            (1, true) => AxisDirection::PlusY,
            (1, false) => AxisDirection::MinusY,
            (_, true) => AxisDirection::PlusZ,
            (_, false) => AxisDirection::MinusZ,
        }
    }

    /// 0 for X, 1 for Y, 2 for Z.
    pub fn axis(self) -> usize {
        match self {
            AxisDirection::PlusX | AxisDirection::MinusX => 0,
            AxisDirection::PlusY | AxisDirection::MinusY => 1,
            AxisDirection::PlusZ | AxisDirection::MinusZ => 2,
        }
    }

    /// Whether it goes the way of its axis.
    pub fn is_positive(self) -> bool {
        matches!(
            self,
            AxisDirection::PlusX | AxisDirection::PlusY | AxisDirection::PlusZ
        )
    }

    /// As the file writes it: `+x`, `-y`…
    pub fn name(self) -> &'static str {
        match self {
            AxisDirection::PlusX => "+x",
            AxisDirection::MinusX => "-x",
            AxisDirection::PlusY => "+y",
            AxisDirection::MinusY => "-y",
            AxisDirection::PlusZ => "+z",
            AxisDirection::MinusZ => "-z",
        }
    }
}

/// A way of making a generative study's shapes, along one or more directions: each direction
/// is an outcome of its own ([`GenerativeMethod::outcomes`]).
#[derive(Debug, Clone, PartialEq)]
pub enum GenerativeMethod {
    /// No constraint.
    Unrestricted,
    /// Printed in layers along each direction, nothing overhanging more than 45° from it
    /// (the only overhang for now).
    Additive { directions: Vec<AxisDirection> },
    /// 2-axis cutting: the shape the same all along each direction, whose sign does not
    /// count.
    Cutting { directions: Vec<AxisDirection> },
    /// Cast in a mould pulled off along each direction, one way, or both ways from the plane
    /// across it through `parting` (mm).
    Casting {
        directions: Vec<AxisDirection>,
        parting: Option<[f64; 3]>,
    },
    /// 3-axis milled from each direction's side (one outcome each): no undercut seen from
    /// the tool, no pocket narrower than `tool_diameter` (mm) when given.
    Milling {
        directions: Vec<AxisDirection>,
        tool_diameter: Option<f64>,
    },
}

impl GenerativeMethod {
    /// Its name in the file: `unrestricted`, `additive`, `cutting`, `casting` or `milling`.
    pub fn name(&self) -> &'static str {
        match self {
            GenerativeMethod::Unrestricted => "unrestricted",
            GenerativeMethod::Additive { .. } => "additive",
            GenerativeMethod::Cutting { .. } => "cutting",
            GenerativeMethod::Casting { .. } => "casting",
            GenerativeMethod::Milling { .. } => "milling",
        }
    }

    /// The method of each outcome it makes, one per direction, in order.
    pub fn outcomes(&self) -> Vec<OutcomeMethod> {
        match self {
            GenerativeMethod::Unrestricted => vec![OutcomeMethod::Unrestricted],
            GenerativeMethod::Additive { directions } => directions
                .iter()
                .map(|&direction| OutcomeMethod::Additive { direction })
                .collect(),
            GenerativeMethod::Cutting { directions } => directions
                .iter()
                .map(|&direction| OutcomeMethod::Cutting { direction })
                .collect(),
            GenerativeMethod::Casting {
                directions,
                parting,
            } => directions
                .iter()
                .map(|&direction| OutcomeMethod::Casting {
                    direction,
                    parting: *parting,
                })
                .collect(),
            GenerativeMethod::Milling {
                directions,
                tool_diameter,
            } => directions
                .iter()
                .map(|&direction| OutcomeMethod::Milling {
                    direction,
                    tool_diameter: *tool_diameter,
                })
                .collect(),
        }
    }
}

/// How one outcome of a generative study is made: a [`GenerativeMethod`] with one direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutcomeMethod {
    Unrestricted,
    /// Printed in layers along `direction`, nothing overhanging more than 45° from it.
    Additive {
        direction: AxisDirection,
    },
    /// The shape the same all along `direction`.
    Cutting {
        direction: AxisDirection,
    },
    /// A mould pulled off along `direction`, or both ways from `parting` (mm).
    Casting {
        direction: AxisDirection,
        parting: Option<[f64; 3]>,
    },
    /// Milled from the side `direction` points to, with a tool of `tool_diameter` (mm).
    Milling {
        direction: AxisDirection,
        tool_diameter: Option<f64>,
    },
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
    /// A topology study's target, member size and preserved regions; ignored by the other
    /// types.
    pub topology: TopologySettings,
    /// A generative study's bodies, target, methods and materials; ignored by the other
    /// types.
    pub generative: GenerativeSettings,
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
            topology: TopologySettings::default(),
            generative: GenerativeSettings::default(),
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

/// The `format_version` `file` is written with: the oldest that knows every study type in
/// it (readers of an older version would refuse the newer types anyway), so a document of
/// static studies still opens in version 1 readers.
pub fn simulation_version(file: &SimulationFile) -> u32 {
    file.studies
        .iter()
        .map(|s| type_version(s.study_type))
        .max()
        .unwrap_or(FORMAT_VERSION)
}

/// The first `format_version` that has studies (and results) of type `t`: 1 static, 2 the
/// thermal ones, 3 topology, 4 generative.
pub(crate) fn type_version(t: StudyType) -> u32 {
    match t {
        StudyType::Static => FORMAT_VERSION,
        StudyType::Thermal | StudyType::TransientThermal | StudyType::ThermalStress => 2,
        StudyType::Topology => 3,
        StudyType::Generative => SIMULATION_FORMAT_VERSION,
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
    /// Absent: the defaults. Never in a generative study, whose grid is in its block.
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
    /// Only in a topology study.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    topology: Option<TopologyDto>,
    /// Only in a generative study, and required there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generative: Option<GenerativeDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct GenerativeDto {
    /// At least one.
    #[serde(default)]
    preserve: Vec<StudyBodyDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    obstacles: Vec<ObstacleDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    starting_shape: Option<StudyBodyDto>,
    /// mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    margin: Option<f64>,
    #[serde(default)]
    objective: ObjectiveDto,
    /// kg; required when maximizing stiffness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mass_target: Option<f64>,
    #[serde(default = "default_safety_factor")]
    safety_factor: f64,
    /// mm; only when minimizing mass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    displacement_limit: Option<f64>,
    /// mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    member_size: Option<f64>,
    #[serde(default = "default_generative_iterations")]
    iterations: u32,
    #[serde(default)]
    resolution: ResolutionDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    symmetry: Option<SymmetryDto>,
    /// At least one.
    #[serde(default)]
    methods: Vec<MethodDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    materials: Vec<MaterialDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    load_cases: Vec<LoadCaseDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LoadCaseDto {
    name: String,
    #[serde(default)]
    fixtures: Vec<String>,
    #[serde(default)]
    loads: Vec<String>,
}

/// A body, written as a face's component and path are: only in assemblies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StudyBodyDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    component: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    path: Vec<u32>,
    feature: u32,
    #[serde(default)]
    index: u32,
}

fn study_body_dto(b: &StudyBody) -> StudyBodyDto {
    let id: BodyIdDto = body_dto(&b.body);
    StudyBodyDto {
        component: b.component.map(|c| c.0),
        path: b.path.iter().map(|c| c.0).collect(),
        feature: id.feature,
        index: id.index,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ObstacleDto {
    body: StudyBodyDto,
    /// mm.
    #[serde(default, skip_serializing_if = "is_zero")]
    offset: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ObjectiveDto {
    #[default]
    MaxStiffness,
    MinMass,
}

/// The voxels' size: as `mesh`'s, a density or an element size, not both.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
struct ResolutionDto {
    /// 0 to 1; 0.5 when both are absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    density: Option<f64>,
    /// mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    element_size: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MethodDto {
    Unrestricted,
    Additive {
        #[serde(default)]
        directions: Vec<AxisDirection>,
        /// Degrees; 45, the only value for now.
        #[serde(default = "default_overhang")]
        overhang: f64,
    },
    Cutting {
        #[serde(default)]
        directions: Vec<AxisDirection>,
    },
    Casting {
        #[serde(default)]
        directions: Vec<AxisDirection>,
        /// mm.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parting: Option<[f64; 3]>,
    },
    Milling {
        #[serde(default)]
        directions: Vec<AxisDirection>,
        /// mm.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_diameter: Option<f64>,
    },
}

/// Degrees: what an additive method's overhang is, the only value for now.
pub(crate) const ADDITIVE_OVERHANG: f64 = 45.0;

fn default_overhang() -> f64 {
    ADDITIVE_OVERHANG
}

fn default_safety_factor() -> f64 {
    GenerativeSettings::default().safety_factor
}

fn default_generative_iterations() -> u32 {
    GenerativeSettings::default().iterations
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct TopologyDto {
    /// 0 to 1.
    #[serde(default = "default_target_mass")]
    target_mass: f64,
    /// mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    member_size: Option<f64>,
    #[serde(default = "default_iterations")]
    iterations: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    preserved: Vec<PreservedDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    symmetry: Option<SymmetryDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pull: Option<PullDto>,
    #[serde(default, skip_serializing_if = "is_false")]
    one_piece: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SymmetryDto {
    planes: Vec<StandardPlaneDto>,
    /// mm.
    #[serde(default)]
    point: [f64; 3],
}

/// A standard plane, by its SolidWorks name.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StandardPlaneDto {
    Right,
    Top,
    Front,
}

const STANDARD_PLANES: [StandardPlaneDto; 3] = [
    StandardPlaneDto::Right,
    StandardPlaneDto::Top,
    StandardPlaneDto::Front,
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PullDto {
    direction: [f64; 3],
    /// mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parting: Option<[f64; 3]>,
}

fn default_target_mass() -> f64 {
    TopologySettings::default().target_mass
}

fn default_iterations() -> u32 {
    TopologySettings::default().iterations
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PreservedDto {
    name: String,
    #[serde(default)]
    faces: Vec<StudyFaceDto>,
    /// mm.
    #[serde(default, skip_serializing_if = "is_zero")]
    depth: f64,
    #[serde(default, skip_serializing_if = "is_false")]
    suppressed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StudyTypeDto {
    #[default]
    Static,
    Thermal,
    TransientThermal,
    ThermalStress,
    Topology,
    Generative,
}

impl StudyTypeDto {
    pub(crate) fn of(t: StudyType) -> Self {
        match t {
            StudyType::Static => StudyTypeDto::Static,
            StudyType::Thermal => StudyTypeDto::Thermal,
            StudyType::TransientThermal => StudyTypeDto::TransientThermal,
            StudyType::ThermalStress => StudyTypeDto::ThermalStress,
            StudyType::Topology => StudyTypeDto::Topology,
            StudyType::Generative => StudyTypeDto::Generative,
        }
    }

    pub(crate) fn study_type(self) -> StudyType {
        match self {
            StudyTypeDto::Static => StudyType::Static,
            StudyTypeDto::Thermal => StudyType::Thermal,
            StudyTypeDto::TransientThermal => StudyType::TransientThermal,
            StudyTypeDto::ThermalStress => StudyType::ThermalStress,
            StudyTypeDto::Topology => StudyType::Topology,
            StudyTypeDto::Generative => StudyType::Generative,
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
        mesh: (study.study_type != StudyType::Generative).then(|| {
            let (density, element_size) = size_dto(study.mesh.size);
            MeshDto {
                mesher: match study.mesh.mesher {
                    Mesher::Netgen => MesherDto::Netgen,
                    Mesher::Lattice => MesherDto::Lattice,
                },
                quality: match study.mesh.quality {
                    MeshQuality::High => QualityDto::High,
                    MeshQuality::Draft => QualityDto::Draft,
                },
                density,
                element_size,
            }
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
        topology: (study.study_type == StudyType::Topology).then(|| {
            let t = &study.topology;
            TopologyDto {
                target_mass: t.target_mass,
                member_size: t.member_size,
                iterations: t.iterations,
                preserved: t
                    .preserved
                    .iter()
                    .map(|p| PreservedDto {
                        name: p.name.clone(),
                        faces: faces_dto(&p.faces),
                        depth: p.depth,
                        suppressed: p.suppressed,
                    })
                    .collect(),
                symmetry: symmetry_dto(t.symmetry),
                pull: t.pull.map(|p| PullDto {
                    direction: p.direction,
                    parting: p.parting,
                }),
                one_piece: t.one_piece,
            }
        }),
        generative: (study.study_type == StudyType::Generative)
            .then(|| generative_dto(&study.generative)),
    }
}

/// A mesh's or a grid's size: a density or an element size.
fn size_dto(size: MeshSize) -> (Option<f64>, Option<f64>) {
    match size {
        MeshSize::Automatic { density } => (Some(density), None),
        MeshSize::ElementSize(size) => (None, Some(size)),
    }
}

/// Planes without one on are none.
fn symmetry_dto(symmetry: Option<SymmetryPlanes>) -> Option<SymmetryDto> {
    symmetry
        .filter(|s| s.planes.contains(&true))
        .map(|s| SymmetryDto {
            planes: STANDARD_PLANES
                .iter()
                .zip(s.planes)
                .filter(|(_, on)| *on)
                .map(|(p, _)| *p)
                .collect(),
            point: s.point,
        })
}

fn generative_dto(g: &GenerativeSettings) -> GenerativeDto {
    let (density, element_size) = size_dto(g.resolution);
    // A study document holds no images: its materials go without their textures.
    let no_textures = TextureTable::default();
    GenerativeDto {
        preserve: g.preserve.iter().map(study_body_dto).collect(),
        obstacles: g
            .obstacles
            .iter()
            .map(|o| ObstacleDto {
                body: study_body_dto(&o.body),
                offset: o.offset,
            })
            .collect(),
        starting_shape: g.starting_shape.as_ref().map(study_body_dto),
        margin: g.margin,
        objective: match g.objective {
            GenerativeObjective::MaxStiffness => ObjectiveDto::MaxStiffness,
            GenerativeObjective::MinMass => ObjectiveDto::MinMass,
        },
        // Minimizing mass needs no target: one is kept only when set.
        mass_target: (g.objective == GenerativeObjective::MaxStiffness || g.mass_target > 0.0)
            .then_some(g.mass_target),
        safety_factor: g.safety_factor,
        displacement_limit: g.displacement_limit,
        member_size: g.member_size,
        iterations: g.iterations,
        resolution: ResolutionDto {
            density,
            element_size,
        },
        symmetry: symmetry_dto(g.symmetry),
        methods: g
            .methods
            .iter()
            .map(|m| match m {
                GenerativeMethod::Unrestricted => MethodDto::Unrestricted,
                GenerativeMethod::Additive { directions } => MethodDto::Additive {
                    directions: directions.clone(),
                    overhang: ADDITIVE_OVERHANG,
                },
                GenerativeMethod::Cutting { directions } => MethodDto::Cutting {
                    directions: directions.clone(),
                },
                GenerativeMethod::Casting {
                    directions,
                    parting,
                } => MethodDto::Casting {
                    directions: directions.clone(),
                    parting: *parting,
                },
                GenerativeMethod::Milling {
                    directions,
                    tool_diameter,
                } => MethodDto::Milling {
                    directions: directions.clone(),
                    tool_diameter: *tool_diameter,
                },
            })
            .collect(),
        materials: g
            .materials
            .iter()
            .map(|m| {
                let mut dto = material_dto(m, &no_textures);
                dto.appearance.texture = None;
                dto
            })
            .collect(),
        load_cases: g
            .load_cases
            .iter()
            .map(|c| LoadCaseDto {
                name: c.name.clone(),
                fixtures: c.fixtures.clone(),
                loads: c.loads.clone(),
            })
            .collect(),
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

    // A generative study's grid and materials are in its block; it has no contacts.
    if study_type == StudyType::Generative {
        if !dto.contacts.is_empty() {
            return Err("a generative study has contacts".to_owned());
        }
        if dto.mesh.is_some() {
            return Err("a generative study has mesh settings".to_owned());
        }
        if !dto.materials.is_empty() || !dto.thermal_materials.is_empty() {
            return Err("a generative study has material overrides".to_owned());
        }
    }
    if !study_type.is_structural() && !dto.fixtures.is_empty() {
        return Err("a thermal study has fixtures".to_owned());
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
        if matches!(load_kind, LoadKind::Gravity { .. }) && !study_type.takes_gravity() {
            return Err(item(if study_type == StudyType::Generative {
                "a generative study has gravity".to_owned()
            } else {
                "a topology study has gravity".to_owned()
            }));
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
    let size = size_from(mesh.density, mesh.element_size, "the mesh")?;

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
    let topology = match (study_type, dto.topology) {
        (StudyType::Topology, Some(t)) => {
            let target_mass = finite(t.target_mass, "the target mass")?;
            if !(target_mass > 0.0 && target_mass < 1.0) {
                return Err("the target mass is not between 0 and 1".to_owned());
            }
            let member_size = positive_size(t.member_size, "the member size")?;
            if !(1..=10_000).contains(&t.iterations) {
                return Err("the iterations are not from 1 to 10 000".to_owned());
            }
            let mut preserved = Vec::with_capacity(t.preserved.len());
            for p in t.preserved {
                named(&p.name)?;
                let item = |e: String| format!("{}: {e}", p.name);
                let depth = finite(p.depth, "the depth").map_err(item)?;
                if depth < 0.0 {
                    return Err(item("the depth is negative".to_owned()));
                }
                preserved.push(PreservedRegion {
                    faces: faces_from(p.faces, kind).map_err(item)?,
                    depth,
                    suppressed: p.suppressed,
                    name: p.name,
                });
            }
            let symmetry = symmetry_from(t.symmetry)?;
            let pull = match t.pull {
                Some(p) => {
                    let direction = finite_vector(p.direction, "the pull direction")?;
                    if direction.iter().all(|x| *x == 0.0) {
                        return Err("the pull direction is a zero vector".to_owned());
                    }
                    Some(PullDirection {
                        direction,
                        parting: match p.parting {
                            Some(q) => Some(finite_vector(q, "the parting point")?),
                            None => None,
                        },
                    })
                }
                None => None,
            };
            TopologySettings {
                target_mass,
                member_size,
                iterations: t.iterations,
                preserved,
                symmetry,
                pull,
                one_piece: t.one_piece,
            }
        }
        (StudyType::Topology, None) => TopologySettings::default(),
        (_, Some(_)) => return Err("only a topology study has a target mass".to_owned()),
        (_, None) => TopologySettings::default(),
    };
    let generative = match (study_type, dto.generative) {
        (StudyType::Generative, Some(g)) => generative_from(g, kind, &fixtures, &loads)?,
        (StudyType::Generative, None) => {
            return Err("a generative study has no generative block".to_owned());
        }
        (_, Some(_)) => return Err("only a generative study has a generative block".to_owned()),
        (_, None) => GenerativeSettings::default(),
    };

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
        topology,
        generative,
    })
}

/// A mesh's or a grid's size (`what`: `the mesh`, `the resolution`): a density from 0 to 1
/// (0.5 when neither is given) or a positive element size, not both.
fn size_from(
    density: Option<f64>,
    element_size: Option<f64>,
    what: &str,
) -> Result<MeshSize, String> {
    match (density, element_size) {
        (Some(_), Some(_)) => Err(format!("{what} has both a density and an element size")),
        (density, None) => {
            let density = finite(density.unwrap_or(0.5), &format!("{what}'s density"))?;
            if !(0.0..=1.0).contains(&density) {
                return Err(format!("{what}'s density is not between 0 and 1"));
            }
            Ok(MeshSize::Automatic { density })
        }
        (None, Some(size)) => {
            let size = finite(size, &format!("{what}'s element size"))?;
            if size <= 0.0 {
                return Err(format!("{what}'s element size is not positive"));
            }
            Ok(MeshSize::ElementSize(size))
        }
    }
}

fn symmetry_from(dto: Option<SymmetryDto>) -> Result<Option<SymmetryPlanes>, String> {
    let Some(s) = dto else {
        return Ok(None);
    };
    if s.planes.is_empty() {
        return Err("the symmetry has no plane".to_owned());
    }
    Ok(Some(SymmetryPlanes {
        planes: STANDARD_PLANES.map(|p| s.planes.contains(&p)),
        point: finite_vector(s.point, "the symmetry point")?,
    }))
}

/// A positive size in mm, or none.
fn positive_size(value: Option<f64>, what: &str) -> Result<Option<f64>, String> {
    match value {
        Some(v) => {
            let v = finite(v, what)?;
            if v <= 0.0 {
                return Err(format!("{what} is not positive"));
            }
            Ok(Some(v))
        }
        None => Ok(None),
    }
}

/// The directions of a method that has some: at least one, none twice; a cutting method's
/// sign does not count, so `+y` and `-y` are the same direction there.
fn directions_from(
    directions: Vec<AxisDirection>,
    method: &str,
    signed: bool,
) -> Result<Vec<AxisDirection>, String> {
    if directions.is_empty() {
        return Err(format!("the {method} method has no direction"));
    }
    let mut seen = BTreeSet::new();
    for d in &directions {
        let key = if signed {
            *d
        } else {
            AxisDirection::new(d.axis(), true)
        };
        if !seen.insert(key) {
            return Err(format!(
                "the {method} method has the direction {} twice",
                d.name()
            ));
        }
    }
    Ok(directions)
}

fn generative_from(
    dto: GenerativeDto,
    kind: ModelKind,
    fixtures: &[Fixture],
    loads: &[Load],
) -> Result<GenerativeSettings, String> {
    // A body has one role at most, and names a component exactly in an assembly.
    let mut bodies = BTreeSet::new();
    let mut body = |dto: StudyBodyDto| -> Result<StudyBody, String> {
        match (kind, dto.component) {
            (ModelKind::Part, Some(_)) => {
                return Err("a body of a part names a component".to_owned());
            }
            (ModelKind::Assembly, None) => {
                return Err("a body of an assembly names no component".to_owned());
            }
            (_, None) if !dto.path.is_empty() => {
                return Err("a body has a component path but no component".to_owned());
            }
            (_, Some(0)) => return Err("a body names component 0".to_owned()),
            _ => {}
        }
        let b = StudyBody {
            component: dto.component.map(ComponentId),
            path: dto.path.into_iter().map(ComponentId).collect(),
            body: body_from(BodyIdDto {
                feature: dto.feature,
                index: dto.index,
            }),
        };
        if bodies.insert(b.clone()) {
            Ok(b)
        } else {
            Err(format!("the body {b} is listed twice"))
        }
    };
    if dto.preserve.is_empty() {
        return Err("the generative study has no body to preserve".to_owned());
    }
    let preserve = dto
        .preserve
        .into_iter()
        .map(&mut body)
        .collect::<Result<Vec<_>, _>>()?;
    let mut obstacles = Vec::with_capacity(dto.obstacles.len());
    for o in dto.obstacles {
        let offset = finite(o.offset, "an obstacle's offset")?;
        if offset < 0.0 {
            return Err("an obstacle's offset is negative".to_owned());
        }
        obstacles.push(GenerativeObstacle {
            body: body(o.body)?,
            offset,
        });
    }
    let starting_shape = dto.starting_shape.map(&mut body).transpose()?;
    let margin = match dto.margin {
        Some(m) => {
            let m = finite(m, "the margin")?;
            if m < 0.0 {
                return Err("the margin is negative".to_owned());
            }
            Some(m)
        }
        None => None,
    };
    let objective = match dto.objective {
        ObjectiveDto::MaxStiffness => GenerativeObjective::MaxStiffness,
        ObjectiveDto::MinMass => GenerativeObjective::MinMass,
    };
    let mass_target = match (objective, dto.mass_target) {
        (GenerativeObjective::MaxStiffness, None) => {
            return Err("maximizing stiffness needs a mass target".to_owned());
        }
        (_, Some(m)) => {
            let m = finite(m, "the mass target")?;
            if m <= 0.0 {
                return Err("the mass target is not positive".to_owned());
            }
            m
        }
        (GenerativeObjective::MinMass, None) => 0.0,
    };
    let displacement_limit = match (objective, dto.displacement_limit) {
        (GenerativeObjective::MaxStiffness, Some(_)) => {
            return Err("only minimizing mass takes a displacement limit".to_owned());
        }
        (_, limit) => positive_size(limit, "the displacement limit")?,
    };
    let safety_factor = finite(dto.safety_factor, "the safety factor")?;
    if safety_factor <= 0.0 {
        return Err("the safety factor is not positive".to_owned());
    }
    if !(1..=10_000).contains(&dto.iterations) {
        return Err("the iterations are not from 1 to 10 000".to_owned());
    }

    if dto.methods.is_empty() {
        return Err("the generative study has no manufacturing method".to_owned());
    }
    let mut kinds = BTreeSet::new();
    let mut methods = Vec::with_capacity(dto.methods.len());
    for m in dto.methods {
        let method = match m {
            MethodDto::Unrestricted => GenerativeMethod::Unrestricted,
            MethodDto::Additive {
                directions,
                overhang,
            } => {
                if overhang != ADDITIVE_OVERHANG {
                    return Err(format!(
                        "the additive method's overhang is {overhang}°, not {ADDITIVE_OVERHANG}°"
                    ));
                }
                GenerativeMethod::Additive {
                    directions: directions_from(directions, "additive", true)?,
                }
            }
            MethodDto::Cutting { directions } => GenerativeMethod::Cutting {
                directions: directions_from(directions, "cutting", false)?,
            },
            MethodDto::Milling {
                directions,
                tool_diameter,
            } => GenerativeMethod::Milling {
                directions: directions_from(directions, "milling", true)?,
                tool_diameter: positive_size(tool_diameter, "the tool diameter")?,
            },
            MethodDto::Casting {
                directions,
                parting,
            } => GenerativeMethod::Casting {
                directions: directions_from(directions, "casting", true)?,
                parting: parting
                    .map(|p| finite_vector(p, "the parting point"))
                    .transpose()?,
            },
        };
        if !kinds.insert(method.name()) {
            return Err(format!("two methods are {}", method.name()));
        }
        methods.push(method);
    }

    let mut names = BTreeSet::new();
    let mut materials = Vec::with_capacity(dto.materials.len());
    for mut m in dto.materials {
        // A study document holds no images.
        m.appearance.texture = None;
        let material = material_from(m, &Default::default())?;
        if !names.insert(material.name.clone()) {
            return Err(format!("two materials are named \"{}\"", material.name));
        }
        if material.density <= 0.0 {
            return Err(format!("the material \"{}\" has no density", material.name));
        }
        if material.mechanical.is_none() {
            return Err(format!(
                "the material \"{}\" has no mechanical properties",
                material.name
            ));
        }
        materials.push(material);
    }

    let mut case_names = BTreeSet::new();
    let mut load_cases = Vec::with_capacity(dto.load_cases.len());
    for c in dto.load_cases {
        if c.name.trim().is_empty() {
            return Err("a load case has no name".to_owned());
        }
        if !case_names.insert(c.name.clone()) {
            return Err(format!("two load cases are named \"{}\"", c.name));
        }
        let item = |e: String| format!("{}: {e}", c.name);
        if c.fixtures.is_empty() || c.loads.is_empty() {
            return Err(item("a load case needs a fixture and a load".to_owned()));
        }
        let check = |names: &[String], known: &[&str], what: &str| -> Result<(), String> {
            let mut seen = BTreeSet::new();
            for n in names {
                if !known.contains(&n.as_str()) {
                    return Err(format!("\"{n}\" is not one of the study's {what}"));
                }
                if !seen.insert(n) {
                    return Err(format!("\"{n}\" is listed twice"));
                }
            }
            Ok(())
        };
        let fixture_names: Vec<&str> = fixtures.iter().map(|f| f.name.as_str()).collect();
        let load_names: Vec<&str> = loads.iter().map(|l| l.name.as_str()).collect();
        check(&c.fixtures, &fixture_names, "fixtures").map_err(item)?;
        check(&c.loads, &load_names, "loads").map_err(item)?;
        load_cases.push(LoadCase {
            name: c.name,
            fixtures: c.fixtures,
            loads: c.loads,
        });
    }

    Ok(GenerativeSettings {
        preserve,
        obstacles,
        starting_shape,
        margin,
        objective,
        mass_target,
        safety_factor,
        displacement_limit,
        member_size: positive_size(dto.member_size, "the member size")?,
        iterations: dto.iterations,
        resolution: size_from(
            dto.resolution.density,
            dto.resolution.element_size,
            "the resolution",
        )?,
        symmetry: symmetry_from(dto.symmetry)?,
        methods,
        materials,
        load_cases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let mut topology = Study::of_type("Topology 1", StudyType::Topology);
        topology.fixtures.push(Fixture {
            name: "Fixed-1".to_owned(),
            kind: FixtureKind::Fixed,
            faces: vec![base(2)],
            suppressed: false,
        });
        topology.loads.push(Load {
            name: "Force-1".to_owned(),
            kind: LoadKind::Force {
                faces: vec![base(5)],
                newtons: 500.0,
                direction: ForceDirection::Vector([0.0, -1.0, 0.0]),
            },
            suppressed: false,
        });
        topology.topology = TopologySettings {
            target_mass: 0.3,
            member_size: Some(4.0),
            iterations: 80,
            preserved: vec![
                PreservedRegion {
                    name: "Preserve-1".to_owned(),
                    faces: vec![base(6)],
                    depth: 2.0,
                    suppressed: false,
                },
                PreservedRegion {
                    name: "Preserve-2".to_owned(),
                    faces: vec![base(7)],
                    depth: 0.0,
                    suppressed: true,
                },
            ],
            symmetry: Some(SymmetryPlanes {
                planes: [true, false, true],
                point: [30.0, 0.0, 5.0],
            }),
            pull: Some(PullDirection {
                direction: [0.0, 1.0, 0.0],
                parting: Some([0.0, 12.5, 0.0]),
            }),
            one_piece: true,
        };
        file.studies = vec![study, second, thermal, stress, topology];
        file
    }

    /// A part's study document with a generative study using every setting, and a static
    /// study.
    fn generative_sample() -> SimulationFile {
        let mut file = SimulationFile::new(StudiedModel {
            kind: ModelKind::Part,
            path: "bracket.oxyprt".to_owned(),
            document_id: "66666666-6666-6666-6666-666666666666".to_owned(),
        });
        let body =
            |feature, index| StudyBody::part(BodyId::new(FeatureId::from_raw(feature), index));
        let material = |name: &str, density, modulus| Material {
            name: name.to_owned(),
            category: "Metal".to_owned(),
            appearance: oxyde_model::Appearance {
                color: [200, 203, 208],
                transparency: 0.0,
                texture: None,
            },
            density,
            source: oxyde_model::MaterialSource::Library,
            mechanical: Some(Mechanical::new(modulus, 0.33, 275.0, 310.0)),
            thermal: None,
        };
        let mut study = Study::of_type("Generative 1", StudyType::Generative);
        study.fixtures.push(Fixture {
            name: "Fixed-1".to_owned(),
            kind: FixtureKind::Fixed,
            faces: vec![StudyFace::part(face(3))],
            suppressed: false,
        });
        study.loads.push(Load {
            name: "Force-1".to_owned(),
            kind: LoadKind::Force {
                faces: vec![StudyFace::part(face(9))],
                newtons: 800.0,
                direction: ForceDirection::Vector([0.0, -1.0, 0.0]),
            },
            suppressed: false,
        });
        study.generative = GenerativeSettings {
            preserve: vec![body(3, 0), body(9, 1)],
            obstacles: vec![
                GenerativeObstacle {
                    body: body(12, 0),
                    offset: 1.5,
                },
                GenerativeObstacle {
                    body: body(13, 0),
                    offset: 0.0,
                },
            ],
            starting_shape: Some(body(15, 0)),
            margin: Some(4.0),
            objective: GenerativeObjective::MaxStiffness,
            mass_target: 0.25,
            safety_factor: 2.5,
            displacement_limit: None,
            member_size: Some(4.0),
            iterations: 120,
            resolution: MeshSize::ElementSize(0.8),
            symmetry: Some(SymmetryPlanes {
                planes: [true, false, false],
                point: [0.0, 0.0, 0.0],
            }),
            methods: vec![
                GenerativeMethod::Unrestricted,
                GenerativeMethod::Additive {
                    directions: vec![AxisDirection::PlusZ, AxisDirection::PlusY],
                },
                GenerativeMethod::Cutting {
                    directions: vec![AxisDirection::MinusY],
                },
                GenerativeMethod::Casting {
                    directions: vec![AxisDirection::PlusY, AxisDirection::MinusX],
                    parting: Some([0.0, 10.0, 0.0]),
                },
                GenerativeMethod::Milling {
                    directions: vec![AxisDirection::PlusZ],
                    tool_diameter: Some(6.0),
                },
            ],
            materials: vec![
                material("Aluminium 6061-T6", 2700.0, 68900.0),
                material("Titanium (Ti-6Al-4V)", 4430.0, 113800.0),
            ],
            load_cases: vec![
                LoadCase {
                    name: "Load Case 1".to_owned(),
                    fixtures: vec!["Fixed-1".to_owned()],
                    loads: vec!["Force-1".to_owned()],
                },
                LoadCase {
                    name: "Load Case 2".to_owned(),
                    fixtures: vec!["Fixed-1".to_owned()],
                    loads: vec!["Force-2".to_owned()],
                },
            ],
        };
        study.loads.push(Load {
            name: "Force-2".to_owned(),
            kind: LoadKind::Pressure {
                faces: vec![StudyFace::part(face(9))],
                mpa: 0.5,
            },
            suppressed: false,
        });
        file.studies = vec![study, Study::new("Static 1")];
        file
    }

    fn bytes(file: &SimulationFile) -> Vec<u8> {
        write_simulation(Cursor::new(Vec::new()), file)
            .unwrap()
            .into_inner()
    }

    #[test]
    fn generative_studies_round_trip() {
        let file = generative_sample();
        assert_eq!(read_simulation(Cursor::new(bytes(&file))).unwrap(), file);
        assert_eq!(simulation_version(&file), 4);

        // The defaults, with the bodies and the target a study cannot do without.
        let mut plain = generative_sample();
        let g = &mut plain.studies[0].generative;
        *g = GenerativeSettings {
            preserve: g.preserve.clone(),
            mass_target: 1.5,
            ..GenerativeSettings::default()
        };
        assert_eq!(read_simulation(Cursor::new(bytes(&plain))).unwrap(), plain);
        let v = json(&plain);
        let g = &v["studies"][0]["generative"];
        // serde_json's maps are sorted.
        assert_eq!(
            g.as_object().unwrap().keys().collect::<Vec<_>>(),
            [
                "iterations",
                "mass_target",
                "methods",
                "objective",
                "preserve",
                "resolution",
                "safety_factor"
            ]
        );
        assert_eq!(
            g["methods"],
            serde_json::json!([{ "type": "unrestricted" }])
        );
        assert_eq!(g["resolution"], serde_json::json!({ "density": 0.5 }));

        // The splitting of methods into outcomes, one per direction.
        let methods = &file.studies[0].generative.methods;
        assert_eq!(
            methods
                .iter()
                .flat_map(GenerativeMethod::outcomes)
                .collect::<Vec<_>>(),
            [
                OutcomeMethod::Unrestricted,
                OutcomeMethod::Additive {
                    direction: AxisDirection::PlusZ
                },
                OutcomeMethod::Additive {
                    direction: AxisDirection::PlusY
                },
                OutcomeMethod::Cutting {
                    direction: AxisDirection::MinusY
                },
                OutcomeMethod::Casting {
                    direction: AxisDirection::PlusY,
                    parting: Some([0.0, 10.0, 0.0])
                },
                OutcomeMethod::Casting {
                    direction: AxisDirection::MinusX,
                    parting: Some([0.0, 10.0, 0.0])
                },
                OutcomeMethod::Milling {
                    direction: AxisDirection::PlusZ,
                    tool_diameter: Some(6.0)
                },
            ]
        );
        // In an assembly, its bodies name their components.
        let mut assembly = generative_sample();
        assembly.model.kind = ModelKind::Assembly;
        assembly.studies.truncate(1);
        let s = &mut assembly.studies[0];
        let on = |f: &mut StudyFace| f.component = Some(ComponentId(2));
        s.fixtures
            .iter_mut()
            .flat_map(|f| f.faces.iter_mut())
            .for_each(on);
        for l in &mut s.loads {
            if let LoadKind::Force { faces, .. } | LoadKind::Pressure { faces, .. } = &mut l.kind {
                faces.iter_mut().for_each(on);
            }
        }
        let g = &mut s.generative;
        let place = |b: &mut StudyBody| {
            b.component = Some(ComponentId(2));
            b.path = vec![ComponentId(5)];
        };
        g.preserve.iter_mut().for_each(place);
        g.obstacles.iter_mut().for_each(|o| place(&mut o.body));
        g.starting_shape.iter_mut().for_each(place);
        assert_eq!(
            read_simulation(Cursor::new(bytes(&assembly))).unwrap(),
            assembly
        );
        let v = json(&assembly);
        assert_eq!(
            v["studies"][0]["generative"]["preserve"][0],
            serde_json::json!({ "component": 2, "path": [5], "feature": 3, "index": 0 })
        );

        // Minimizing mass: no target needed, a displacement limit taken.
        let mut light = generative_sample();
        let g = &mut light.studies[0].generative;
        g.objective = GenerativeObjective::MinMass;
        g.mass_target = 0.0;
        g.displacement_limit = Some(0.2);
        assert_eq!(read_simulation(Cursor::new(bytes(&light))).unwrap(), light);
        let v = json(&light);
        let g = &v["studies"][0]["generative"];
        assert_eq!(g["objective"], "min_mass");
        assert!(g.get("mass_target").is_none());
        assert_eq!(g["displacement_limit"], 0.2);
        for d in [
            AxisDirection::MinusX,
            AxisDirection::PlusY,
            AxisDirection::MinusZ,
        ] {
            assert_eq!(AxisDirection::new(d.axis(), d.is_positive()), d);
        }
    }

    #[test]
    fn the_generative_json_is_as_documented() {
        let v = json(&generative_sample());
        let study = &v["studies"][0];
        assert_eq!(study["type"], "generative");
        // No mesh, contacts or overrides: the grid and the materials are in the block.
        assert!(study.get("mesh").is_none() && study.get("materials").is_none());
        assert!(v["studies"][1].get("mesh").is_some());
        assert!(v["studies"][1].get("generative").is_none());
        let g = &study["generative"];
        assert_eq!(
            g["preserve"],
            serde_json::json!([{ "feature": 3, "index": 0 }, { "feature": 9, "index": 1 }])
        );
        assert_eq!(
            g["obstacles"],
            serde_json::json!([
                { "body": { "feature": 12, "index": 0 }, "offset": 1.5 },
                { "body": { "feature": 13, "index": 0 } }
            ])
        );
        assert_eq!(g["starting_shape"]["feature"], 15);
        assert_eq!(g["margin"], 4.0);
        assert_eq!(g["objective"], "max_stiffness");
        assert_eq!(g["mass_target"], 0.25);
        assert_eq!(g["safety_factor"], 2.5);
        assert_eq!(g["member_size"], 4.0);
        assert_eq!(g["iterations"], 120);
        assert_eq!(g["resolution"], serde_json::json!({ "element_size": 0.8 }));
        assert_eq!(
            g["symmetry"],
            serde_json::json!({ "planes": ["right"], "point": [0.0, 0.0, 0.0] })
        );
        assert_eq!(
            g["methods"],
            serde_json::json!([
                { "type": "unrestricted" },
                { "type": "additive", "directions": ["+z", "+y"], "overhang": 45.0 },
                { "type": "cutting", "directions": ["-y"] },
                { "type": "casting", "directions": ["+y", "-x"], "parting": [0.0, 10.0, 0.0] },
                { "type": "milling", "directions": ["+z"], "tool_diameter": 6.0 }
            ])
        );
        assert_eq!(
            g["load_cases"][1],
            serde_json::json!({ "name": "Load Case 2", "fixtures": ["Fixed-1"], "loads": ["Force-2"] })
        );
        let aluminium = &g["materials"][0];
        assert_eq!(aluminium["name"], "Aluminium 6061-T6");
        assert_eq!(aluminium["density"], 2700.0);
        assert_eq!(
            aluminium["appearance"]["color"],
            serde_json::json!([200, 203, 208])
        );
        assert_eq!(aluminium["mechanical"]["elastic_modulus"], 68900.0);
        assert_eq!(g["materials"][1]["name"], "Titanium (Ti-6Al-4V)");
    }

    #[test]
    fn a_generative_studys_materials_go_without_their_textures() {
        let plain = generative_sample();
        let mut file = plain.clone();
        let texture = oxyde_model::Texture {
            data: oxyde_model::FileData::new(vec![1, 2, 3]),
            file_name: "brushed.png".to_owned(),
            scale: 20.0,
        };
        file.studies[0].generative.materials[0].appearance.texture = Some(texture);
        let v = json(&file);
        assert!(
            v["studies"][0]["generative"]["materials"][0]["appearance"]
                .get("texture")
                .is_none()
        );
        let read = read_simulation(Cursor::new(bytes(&file))).unwrap();
        assert_eq!(read, plain);
        // One written by hand has its texture ignored, not refused.
        let with = edited(&bytes(&plain), SIMULATION, |s| {
            s.replacen(
                "\"color\": [",
                "\"texture\": { \"file\": \"textures/1.png\", \"scale\": 5 }, \"color\": [",
                1,
            )
        });
        assert_eq!(read_simulation(Cursor::new(with)).unwrap(), read);
    }

    #[test]
    fn inconsistent_generative_studies_are_refused() {
        let good = bytes(&generative_sample());
        let refused = |edit: &dyn Fn(&mut serde_json::Value), why: &str| {
            let bytes = edited(&good, SIMULATION, |s| {
                let mut v: serde_json::Value = serde_json::from_str(s).unwrap();
                edit(&mut v);
                serde_json::to_string(&v).unwrap()
            });
            match read_simulation(Cursor::new(bytes)) {
                Err(FormatError::InvalidSimulation(e)) => assert!(e.contains(why), "{e}"),
                other => panic!("expected an invalid study ({why}), got {other:?}"),
            }
        };
        fn study(v: &mut serde_json::Value) -> &mut serde_json::Value {
            &mut v["studies"][0]
        }
        fn block(v: &mut serde_json::Value) -> &mut serde_json::Value {
            &mut v["studies"][0]["generative"]
        }

        // What the type takes.
        refused(
            &|v| {
                let g = study(v)["generative"].clone();
                v["studies"][1]["generative"] = g;
            },
            "only a generative study has a generative block",
        );
        refused(
            &|v| {
                study(v).as_object_mut().unwrap().remove("generative");
            },
            "no generative block",
        );
        // In an assembly, bodies name their components as faces do.
        refused(
            &|v| {
                v["model"]["kind"] = "assembly".into();
                v["studies"].as_array_mut().unwrap().truncate(1);
                for item in ["fixtures", "loads"] {
                    for i in 0..study(v)[item].as_array().unwrap().len() {
                        study(v)[item][i]["faces"][0]["component"] = 1.into();
                    }
                }
            },
            "a body of an assembly names no component",
        );
        refused(
            &|v| block(v)["preserve"][0]["component"] = 2.into(),
            "a body of a part names a component",
        );
        refused(
            &|v| {
                study(v)["contacts"] =
                    serde_json::json!([{ "name": "Contact-1", "type": "bonded" }]);
            },
            "has contacts",
        );
        refused(
            &|v| study(v)["mesh"] = serde_json::json!({ "density": 0.5 }),
            "mesh settings",
        );
        refused(
            &|v| {
                study(v)["materials"] = serde_json::json!({
                    "Steel": { "elastic_modulus": 200000, "poisson_ratio": 0.3 }
                });
            },
            "material overrides",
        );
        refused(
            &|v| {
                study(v)["loads"][0] = serde_json::json!({
                    "name": "Gravity-1", "type": "gravity", "acceleration": [0, -9.81, 0]
                });
            },
            "a generative study has gravity",
        );
        refused(
            &|v| study(v)["topology"] = serde_json::json!({ "target_mass": 0.5 }),
            "only a topology study",
        );

        // The bodies.
        refused(
            &|v| block(v)["preserve"] = serde_json::json!([]),
            "no body to preserve",
        );
        refused(
            &|v| block(v)["starting_shape"] = serde_json::json!({ "feature": 9, "index": 1 }),
            "the body F9.1 is listed twice",
        );
        refused(
            &|v| block(v)["preserve"][0]["path"] = serde_json::json!([3]),
            "a component path but no component",
        );
        refused(
            &|v| block(v)["obstacles"][0]["offset"] = (-1.0).into(),
            "offset is negative",
        );
        refused(
            &|v| block(v)["margin"] = (-1.0).into(),
            "margin is negative",
        );

        // The numbers.
        refused(
            &|v| block(v)["mass_target"] = 0.0.into(),
            "mass target is not positive",
        );
        refused(
            &|v| block(v)["safety_factor"] = 0.0.into(),
            "safety factor is not positive",
        );
        refused(
            &|v| block(v)["member_size"] = (-4.0).into(),
            "member size is not positive",
        );
        refused(&|v| block(v)["iterations"] = 0.into(), "iterations");
        refused(
            &|v| block(v)["resolution"] = serde_json::json!({ "density": 0.5, "element_size": 1 }),
            "the resolution has both",
        );
        refused(
            &|v| block(v)["resolution"] = serde_json::json!({ "density": 1.5 }),
            "the resolution's density is not between 0 and 1",
        );
        refused(
            &|v| block(v)["symmetry"]["planes"] = serde_json::json!([]),
            "no plane",
        );

        // The methods.
        refused(
            &|v| block(v)["methods"] = serde_json::json!([]),
            "no manufacturing method",
        );
        refused(
            &|v| {
                block(v)["methods"] = serde_json::json!([
                    { "type": "cutting", "directions": ["+x"] },
                    { "type": "cutting", "directions": ["+y"] }
                ]);
            },
            "two methods are cutting",
        );
        refused(
            &|v| block(v)["methods"][1]["directions"] = serde_json::json!([]),
            "the additive method has no direction",
        );
        refused(
            &|v| block(v)["methods"][1]["directions"] = serde_json::json!(["+z", "+z"]),
            "the direction +z twice",
        );
        // A cutting direction's sign does not count; an additive one's does.
        refused(
            &|v| block(v)["methods"][2]["directions"] = serde_json::json!(["-y", "+y"]),
            "the cutting method has the direction +y twice",
        );
        refused(
            &|v| block(v)["methods"][1]["overhang"] = 30.0.into(),
            "overhang is 30°",
        );

        // The load cases.
        refused(
            &|v| block(v)["load_cases"][1]["name"] = "Load Case 1".into(),
            "two load cases",
        );
        refused(
            &|v| block(v)["load_cases"][1]["loads"] = serde_json::json!(["Force-9"]),
            "\"Force-9\" is not one of the study's loads",
        );
        refused(
            &|v| block(v)["load_cases"][0]["fixtures"] = serde_json::json!(["Force-1"]),
            "not one of the study's fixtures",
        );
        refused(
            &|v| block(v)["load_cases"][0]["loads"] = serde_json::json!([]),
            "needs a fixture and a load",
        );
        refused(
            &|v| block(v)["load_cases"][0]["loads"] = serde_json::json!(["Force-1", "Force-1"]),
            "listed twice",
        );

        // The materials.
        refused(
            &|v| block(v)["materials"][1]["name"] = "Aluminium 6061-T6".into(),
            "two materials",
        );
        refused(
            &|v| block(v)["materials"][1]["density"] = 0.0.into(),
            "has no density",
        );
        refused(
            &|v| {
                block(v)["materials"][1]
                    .as_object_mut()
                    .unwrap()
                    .remove("mechanical");
            },
            "no mechanical properties",
        );
        // A mechanical block no solver could use counts as none.
        refused(
            &|v| block(v)["materials"][1]["mechanical"]["poisson_ratio"] = 0.7.into(),
            "no mechanical properties",
        );

        // The objectives' limits.
        refused(
            &|v| {
                block(v).as_object_mut().unwrap().remove("mass_target");
            },
            "needs a mass target",
        );
        refused(
            &|v| block(v)["displacement_limit"] = 0.5.into(),
            "only minimizing mass takes a displacement limit",
        );
        refused(
            &|v| {
                block(v)["objective"] = "min_mass".into();
                block(v)["displacement_limit"] = (-1.0).into();
            },
            "displacement limit is not positive",
        );
        refused(
            &|v| block(v)["methods"][4]["tool_diameter"] = 0.0.into(),
            "tool diameter is not positive",
        );
        refused(
            &|v| block(v)["methods"][4]["directions"] = serde_json::json!([]),
            "the milling method has no direction",
        );

        // What is not a number, a direction or an objective is not JSON of a study.
        for edit in [
            (|v: &mut serde_json::Value| block(v)["objective"] = "max_strength".into())
                as fn(&mut serde_json::Value),
            |v| block(v)["methods"][2]["directions"] = serde_json::json!(["y"]),
        ] {
            let bytes = edited(&good, SIMULATION, |s| {
                let mut v: serde_json::Value = serde_json::from_str(s).unwrap();
                edit(&mut v);
                serde_json::to_string(&v).unwrap()
            });
            assert!(matches!(
                read_simulation(Cursor::new(bytes)),
                Err(FormatError::Json { .. })
            ));
        }

        // Hand-written: the defaults, and the overhang may be left out.
        let read = read_simulation(Cursor::new(edited(&good, SIMULATION, |s| {
            let mut v: serde_json::Value = serde_json::from_str(s).unwrap();
            let g = block(&mut v).as_object_mut().unwrap();
            for key in ["safety_factor", "iterations", "resolution", "objective"] {
                g.remove(key);
            }
            g["methods"][1].as_object_mut().unwrap().remove("overhang");
            serde_json::to_string(&v).unwrap()
        })))
        .unwrap();
        let g = &read.studies[0].generative;
        let defaults = GenerativeSettings::default();
        assert_eq!(
            (g.safety_factor, g.iterations, g.resolution, g.objective),
            (
                defaults.safety_factor,
                defaults.iterations,
                defaults.resolution,
                defaults.objective
            )
        );
        assert_eq!(g.methods, generative_sample().studies[0].generative.methods);
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
        assert!((0..4).all(|i| v["studies"][i].get("topology").is_none()));
        let topology = &v["studies"][4];
        assert_eq!(topology["type"], "topology");
        let t = &topology["topology"];
        assert_eq!(t["target_mass"], 0.3);
        assert_eq!(t["member_size"], 4.0);
        assert_eq!(t["iterations"], 80);
        assert_eq!(t["one_piece"], true);
        assert_eq!(t["preserved"][0]["name"], "Preserve-1");
        assert_eq!(t["preserved"][0]["depth"], 2.0);
        assert_eq!(
            t["preserved"][0]["faces"][0]["face"]["key"]["type"],
            "end_cap"
        );
        assert!(t["preserved"][1].get("depth").is_none());
        assert_eq!(t["preserved"][1]["suppressed"], true);
        assert_eq!(
            t["symmetry"],
            serde_json::json!({ "planes": ["right", "front"], "point": [30.0, 0.0, 5.0] })
        );
        assert_eq!(
            t["pull"],
            serde_json::json!({ "direction": [0.0, 1.0, 0.0], "parting": [0.0, 12.5, 0.0] })
        );
    }

    #[test]
    fn documents_are_written_with_the_oldest_version_that_holds_them() {
        assert_eq!(simulation_version(&generative_sample()), 4);
        let mut file = sample();
        assert_eq!(simulation_version(&file), 3);
        file.studies.truncate(4);
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
                &format!("\"format_version\": {}", simulation_version(&sample())),
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
        // The first study has gravity.
        refused(
            &|s| s.replacen("\"type\": \"static\"", "\"type\": \"topology\"", 1),
            "a topology study has gravity",
        );
        refused(
            &|s| s.replace("\"type\": \"topology\"", "\"type\": \"static\""),
            "only a topology study",
        );
        refused(
            &|s| s.replace("\"target_mass\": 0.3", "\"target_mass\": 1.0"),
            "target mass",
        );
        refused(
            &|s| s.replace("\"member_size\": 4.0", "\"member_size\": 0.0"),
            "member size",
        );
        refused(
            &|s| s.replace("\"depth\": 2.0", "\"depth\": -2.0"),
            "depth is negative",
        );
        refused(&|s| s.replace("Preserve-1", "Force-1"), "two items");
        // The topology study's controls, edited in the JSON.
        let control = |edit: fn(&mut serde_json::Value)| {
            move |s: &str| {
                let mut v: serde_json::Value = serde_json::from_str(s).unwrap();
                edit(&mut v["studies"][4]["topology"]);
                serde_json::to_string(&v).unwrap()
            }
        };
        refused(
            &control(|t| t["symmetry"]["planes"] = serde_json::json!([])),
            "no plane",
        );
        refused(
            &control(|t| t["pull"]["direction"] = serde_json::json!([0, 0, 0])),
            "pull direction is a zero vector",
        );
        assert!(matches!(
            read_simulation(Cursor::new(edited(
                &good,
                SIMULATION,
                control(|t| t["symmetry"]["planes"] = serde_json::json!(["left"]))
            ))),
            Err(FormatError::Json { .. })
        ));
        // Without them, a study has neither.
        let plain = read_simulation(Cursor::new(edited(
            &good,
            SIMULATION,
            control(|t| {
                let t = t.as_object_mut().unwrap();
                t.remove("symmetry");
                t.remove("pull");
            }),
        )))
        .unwrap();
        let t = &plain.studies[4].topology;
        assert_eq!((t.symmetry, t.pull), (None, None));
        // An unknown study or contact type is an error, as unknown features are.
        assert!(matches!(
            read_simulation(Cursor::new(
                edited(&good, SIMULATION, |s| s.replace("\"static\"", "\"modal\""))
            )),
            Err(FormatError::Json { .. })
        ));
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
