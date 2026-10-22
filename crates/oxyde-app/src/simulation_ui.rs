//! The Simulation tab: studies of the part or assembly, as SolidWorks Simulation and
//! Fusion 360 run them (`docs/SIMULATION.md`). The tab starts on *New Study*, which makes
//! a static, frequency, thermal, transient thermal, thermal stress or topology study; a
//! model has any number of them, one shown at a time (`simulation_studies.rs`). Generative
//! studies are among them too, shown in the Generative Design tab only (`generative_ui.rs`).
//!
//! A frequency study takes fixtures and contacts, no loads: *Run* finds the lowest natural
//! frequencies of the model held by its fixtures (`oxyde_sim::solve_frequencies`), the panel
//! lists them with their mass participation, and the viewport shows the shape of the mode
//! picked, deformed and coloured by how far each point moves in it.
//!
//! A topology study takes a static study's fixtures, forces and pressures, and *Preserve
//! Region* faces where material must stay; *Run* takes material away where the loads do
//! not need it (`oxyde_sim::optimize`) until the target mass is left, and the viewport
//! shows the shape that remains, a smoothed surface cut at a density threshold, which
//! *Export STL* writes out.
//!
//! In a static study, select faces, then *Fixed Geometry*, *Roller/Slider*, *Force* or
//! *Pressure* makes a fixture or a load on them; *Gravity* adds the model's weight. A
//! thermal study takes *Temperature*, *Heat Power*, *Heat Flux*, *Convection*,
//! *Radiation* and *Internal Heat* instead; a thermal stress study takes both. The panel at the top of
//! the left side lists them with their values, the material's mechanical properties (its
//! own, else typical values for its name; a study can override them), the mesh settings
//! and the results. *Run* meshes the model
//! into tetrahedra and solves it on a thread of its own (`oxyde_sim`), then the viewport
//! shows the von Mises stress, the first principal stress, the displacement or the factor
//! of safety on the deformed model, in colour bands with a legend. *Mesh* shows the mesh
//! alone.
//!
//! The surfaces handed to the mesher are made on the UI thread when the model changes
//! (the kernel is not shared), labelled by face, so that fixtures and loads held by
//! persistent face names find their facets. Parts in an assembly are bonded where they
//! touch, SolidWorks' default contact.
//!
//! Nothing here is saved in the document: the study is the session's, the settings and
//! typed properties are preferences (`simulation` in `preferences.json`). Where studies
//! belong in the files is the owner's call (`docs/SIMULATION.md` §5).
//!
//! A flow study (`flow_ui.rs`, `docs/CFD.md`) takes inlets, static pressures, ideal walls
//! and goals on faces instead, and runs OpenFOAM on its own thread; its plots are the
//! pressure on the model's wetted faces and cut planes through the fluid.
//!
//! The mesh is Netgen's when the kernel has it (`Kernel::volume_mesh`, run on a worker
//! kernel over copies of the solids), else the lattice mesher's. Each solid is meshed on
//! its own and contacts tie them, as in Fusion 360: found automatically between faces of
//! different bodies that touch (`oxyde_sim::detect_contacts`), bonded unless changed, or
//! made by hand from the selection. Studies are saved as `.oxysim` and results as
//! `.oxyres` (`simulation_files.rs`).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use oxyde_assembly::Placement;
use oxyde_format::results::{Fingerprint, SolverInfo};
use oxyde_format::simulation::{
    CoolingSettings, FlowGoalKind, FlowSettings, GenerativeSettings, StudyType, TransientSettings,
};
use oxyde_geom::Transform;
use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{
    FaceMesh, FaceSource, Kernel, Mesh, MeshWatch, ShapeHandle, VolumeMesh, VolumeMeshError,
    VolumeMeshOptions,
};
use oxyde_model::{BodyId, FaceName, Material, Mechanical, Thermal};
use oxyde_occt::OcctKernel;
use oxyde_render::{Material as RenderMaterial, Scene};
use oxyde_sim::{
    ContactCandidate, ContactKind, Elastic, FixtureKind, FrequencyResults, FrequencyStudy,
    LoadKind, MeshOptions, MeshReport, NO_LABEL, Quantity, Results, SolveOptions, Summary,
    SurfaceMesh, TetMesh, ThermalLoadKind, ThermalMaterial, ThermalProps, ThermalResults,
    ThermalStudy, ThermalSummary, TopologyResults, TopologyStudy,
};
use serde::{Deserialize, Serialize};

use crate::app::{OxydeApp, Selected};
use crate::assembly::{AsmSelected, Occurrence};
use crate::simulation_files::StudyFile;
use crate::viewport::ViewportOutput;

// ---- Preferences ---------------------------------------------------------------------------

/// The elements' order, as SolidWorks names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementQuality {
    /// Ten-node tetrahedra.
    High,
    /// Four-node tetrahedra: quick, and far too stiff in bending.
    Draft,
}

/// What the plot shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlotChoice {
    VonMises,
    FirstPrincipal,
    Displacement,
    FactorOfSafety,
    /// The pressure between bodies in contact.
    ContactPressure,
    /// °C, of a thermal study.
    Temperature,
    /// W/m², the length of the heat flux.
    HeatFlux,
    /// A topology study's shape: the material kept.
    Shape,
    /// A frequency study's mode: how far each point moves in it, relative to the most.
    ModeShape,
    /// Pa, a flow study's static pressure on the model's faces the fluid wets.
    FlowSurfacePressure,
    /// m/s, a flow study's speed on a plane through the fluid.
    FlowCutVelocity,
    /// Pa, a flow study's static pressure on a plane through the fluid.
    FlowCutPressure,
    /// °C, a cooling study's temperature on a plane through the fluid and the solids.
    CoolingCutTemperature,
    /// °C, a cooling study's temperature on the model's faces.
    CoolingSurfaceTemperature,
    /// m/s, a flow study's trajectories: particles carried by the flow, animated, coloured
    /// by speed.
    FlowTrajectories,
}

impl PlotChoice {
    pub(crate) const ALL: [PlotChoice; 15] = [
        PlotChoice::Temperature,
        PlotChoice::HeatFlux,
        PlotChoice::VonMises,
        PlotChoice::FirstPrincipal,
        PlotChoice::Displacement,
        PlotChoice::FactorOfSafety,
        PlotChoice::ContactPressure,
        PlotChoice::Shape,
        PlotChoice::ModeShape,
        PlotChoice::CoolingCutTemperature,
        PlotChoice::CoolingSurfaceTemperature,
        PlotChoice::FlowCutVelocity,
        PlotChoice::FlowCutPressure,
        PlotChoice::FlowSurfacePressure,
        PlotChoice::FlowTrajectories,
    ];

    /// Whether it plots a thermal study's results.
    pub(crate) fn is_thermal(self) -> bool {
        matches!(self, PlotChoice::Temperature | PlotChoice::HeatFlux)
    }

    /// Whether it plots a flow or cooling study's results.
    pub(crate) fn is_flow(self) -> bool {
        matches!(
            self,
            PlotChoice::FlowSurfacePressure
                | PlotChoice::FlowCutVelocity
                | PlotChoice::FlowCutPressure
                | PlotChoice::CoolingCutTemperature
                | PlotChoice::CoolingSurfaceTemperature
                | PlotChoice::FlowTrajectories
        )
    }

    /// Whether it plots a cooling study's temperatures.
    pub(crate) fn is_cooling(self) -> bool {
        matches!(
            self,
            PlotChoice::CoolingCutTemperature | PlotChoice::CoolingSurfaceTemperature
        )
    }

    /// Whether it cuts a plane through the fluid.
    pub(crate) fn is_cut(self) -> bool {
        matches!(
            self,
            PlotChoice::FlowCutVelocity
                | PlotChoice::FlowCutPressure
                | PlotChoice::CoolingCutTemperature
        )
    }

    /// Whether a study of `t` has it.
    pub(crate) fn fits(self, t: StudyType) -> bool {
        match self {
            PlotChoice::Shape => t == StudyType::Topology,
            PlotChoice::ModeShape => t == StudyType::Frequency,
            p if p.is_flow() => t.is_flow() && (t.is_cooling() || !p.is_cooling()),
            p if p.is_thermal() => t.is_thermal(),
            _ => t.has_stresses(),
        }
    }

    fn quantity(self) -> Option<Quantity> {
        match self {
            PlotChoice::VonMises => Some(Quantity::VonMises),
            PlotChoice::FirstPrincipal => Some(Quantity::FirstPrincipal),
            PlotChoice::Displacement => Some(Quantity::Displacement),
            PlotChoice::FactorOfSafety => Some(Quantity::FactorOfSafety),
            PlotChoice::ContactPressure
            | PlotChoice::Temperature
            | PlotChoice::HeatFlux
            | PlotChoice::Shape
            | PlotChoice::ModeShape
            | PlotChoice::FlowSurfacePressure
            | PlotChoice::FlowCutVelocity
            | PlotChoice::FlowCutPressure
            | PlotChoice::CoolingCutTemperature
            | PlotChoice::CoolingSurfaceTemperature
            | PlotChoice::FlowTrajectories => None,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            PlotChoice::ContactPressure => "Contact pressure",
            PlotChoice::Temperature => "Temperature",
            PlotChoice::HeatFlux => "Heat flux",
            PlotChoice::Shape => "Shape",
            PlotChoice::ModeShape => "Mode shape",
            PlotChoice::FlowSurfacePressure => "Surface pressure",
            PlotChoice::FlowCutVelocity => "Velocity (cut plot)",
            PlotChoice::FlowCutPressure => "Pressure (cut plot)",
            PlotChoice::CoolingCutTemperature => "Temperature (cut plot)",
            PlotChoice::CoolingSurfaceTemperature => "Surface temperature",
            PlotChoice::FlowTrajectories => "Flow trajectories",
            other => other.quantity().map_or("", Quantity::label),
        }
    }

    pub(crate) fn unit(self) -> &'static str {
        match self {
            PlotChoice::ContactPressure => "MPa",
            PlotChoice::Temperature => "°C",
            PlotChoice::HeatFlux => "W/m²",
            // A mode's size is arbitrary: its shape is relative to its largest motion.
            PlotChoice::Shape | PlotChoice::ModeShape => "",
            PlotChoice::FlowSurfacePressure | PlotChoice::FlowCutPressure => "Pa",
            PlotChoice::FlowCutVelocity | PlotChoice::FlowTrajectories => "m/s",
            PlotChoice::CoolingCutTemperature | PlotChoice::CoolingSurfaceTemperature => "°C",
            other => other.quantity().map_or("", Quantity::unit),
        }
    }

    /// Its value at each node (contact pressure: 0 off the contacts' faces), of a static
    /// solve's results.
    fn values(self, results: &Results) -> Vec<f64> {
        match self.quantity() {
            Some(q) => q.values(results),
            None if results.contact_pressure.len() == results.von_mises.len() => {
                results.contact_pressure.clone()
            }
            None => vec![0.0; results.von_mises.len()],
        }
    }
}

/// How much the deformed shape is exaggerated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleChoice {
    /// The largest displacement shown at 5 % of the model's size.
    Automatic,
    /// As computed.
    True,
    Custom,
}

/// The Simulation tab's settings, kept in the preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SimulationPrefs {
    pub plot: PlotChoice,
    pub deformed: bool,
    pub scale: ScaleChoice,
    pub custom_scale: f64,
    /// Colour bands in the plot.
    pub bands: u32,
    /// The mesh's edges over the plot.
    pub show_mesh: bool,
    /// The most memory a study may take, GB: a mesh with more nodes than it allows is
    /// refused before it is solved (Preferences › Simulation).
    pub memory_budget_gb: f64,
    /// A topology study's shape is cut where the density is this, 0 to 1.
    pub shape_threshold: f64,
    /// The shape smoothed, rather than showing the mesh's facets.
    pub smooth_shape: bool,
    /// The whole model drawn faintly around a topology study's shape.
    pub show_original: bool,
    /// Generative studies solved on the graphics card when there is one.
    pub generative_gpu: bool,
    /// The OpenFOAM installation flow studies run with (Preferences › Simulation); empty:
    /// the newest found in the usual places.
    pub openfoam_dir: String,
    /// Processes a flow study's solver runs on; 0: one per core when OpenFOAM's MPI is
    /// installed, else one.
    pub flow_processes: u32,
    /// The axis a cut plot's plane is square to.
    pub cut_axis: CutAxis,
    /// Where the plane is along the fluid, 0 to 1.
    pub cut_position: f64,
    /// About how many paths a flow trajectories plot follows.
    pub trajectories: u32,
}

/// The axis a cut plot's plane is square to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutAxis {
    X,
    Y,
    Z,
}

impl CutAxis {
    pub(crate) const ALL: [CutAxis; 3] = [CutAxis::X, CutAxis::Y, CutAxis::Z];

    pub(crate) fn index(self) -> usize {
        match self {
            CutAxis::X => 0,
            CutAxis::Y => 1,
            CutAxis::Z => 2,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            CutAxis::X => "Square to X",
            CutAxis::Y => "Square to Y",
            CutAxis::Z => "Square to Z",
        }
    }
}

impl Default for SimulationPrefs {
    fn default() -> Self {
        Self {
            plot: PlotChoice::VonMises,
            deformed: true,
            scale: ScaleChoice::Automatic,
            custom_scale: 1.0,
            bands: 16,
            show_mesh: false,
            memory_budget_gb: 8.0,
            shape_threshold: 0.5,
            smooth_shape: true,
            show_original: true,
            generative_gpu: true,
            openfoam_dir: String::new(),
            flow_processes: 0,
            cut_axis: CutAxis::Z,
            cut_position: 0.5,
            trajectories: 100,
        }
    }
}

// ---- The study -----------------------------------------------------------------------------

/// A face of the model: of the part, or of a part occurrence in the assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimFace {
    pub occurrence: Option<Occurrence>,
    pub face: FaceName,
}

/// Along what a force pushes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoadDirection {
    /// Normal to each face, into it.
    Normal,
    X,
    Y,
    Z,
    /// Along another direction, read from a study file (unit length).
    Vector([f64; 3]),
}

impl LoadDirection {
    const ALL: [(LoadDirection, &'static str); 4] = [
        (LoadDirection::Normal, "Normal to the faces"),
        (LoadDirection::X, "Along X"),
        (LoadDirection::Y, "Along Y"),
        (LoadDirection::Z, "Along Z"),
    ];

    fn label(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(d, _)| *d == self)
            .map_or("Along its direction", |(_, l)| l)
    }

    pub(crate) fn vector(self) -> Option<[f64; 3]> {
        match self {
            LoadDirection::Normal => None,
            LoadDirection::X => Some([1.0, 0.0, 0.0]),
            LoadDirection::Y => Some([0.0, 1.0, 0.0]),
            LoadDirection::Z => Some([0.0, 0.0, 1.0]),
            LoadDirection::Vector(v) => Some(v),
        }
    }

    /// The direction of `newtons` along `v`, as the panel shows it: an axis (the force's
    /// sign carrying the way along it) when `v` is one, else the vector itself.
    pub(crate) fn of_vector(v: [f64; 3], newtons: f64) -> (LoadDirection, f64) {
        let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if !(length > 0.0 && length.is_finite()) {
            return (LoadDirection::Normal, newtons);
        }
        let u = [v[0] / length, v[1] / length, v[2] / length];
        for (c, direction) in
            u.into_iter()
                .zip([LoadDirection::X, LoadDirection::Y, LoadDirection::Z])
        {
            if (c.abs() - 1.0).abs() < 1e-9 {
                return (direction, newtons * c.signum());
            }
        }
        (LoadDirection::Vector(u), newtons)
    }
}

/// A fixture or a load.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    Fixed,
    Roller,
    /// N; a negative force along an axis pushes the other way.
    Force {
        newtons: f64,
        direction: LoadDirection,
    },
    /// MPa.
    Pressure {
        mpa: f64,
    },
    /// The faces held at a temperature, °C.
    Temperature {
        celsius: f64,
    },
    /// A total heat flow into the faces, W, spread by area.
    HeatPower {
        watts: f64,
    },
    /// A heat flux into the faces, W/m².
    HeatFlux {
        flux: f64,
    },
    /// Convection to a fluid: film coefficient W/(m²·K), fluid temperature °C.
    Convection {
        coefficient: f64,
        ambient: f64,
    },
    /// Radiation to the surroundings: emissivity 0 to 1, their temperature °C.
    Radiation {
        emissivity: f64,
        ambient: f64,
    },
    /// Heat made inside the bodies of the faces, W in all.
    InternalHeat {
        watts: f64,
    },
    /// A topology or generative study's preserved region: its material stays, on the faces
    /// and this deep under them, mm.
    Preserve {
        depth: f64,
    },
    /// A generative study's acceleration, m/s² in world coordinates (Y up): what weighs the
    /// material and the remote masses of the load cases holding it. No faces. (A static
    /// study's weight is its `gravity` switch.)
    Gravity {
        acceleration: [f64; 3],
    },
    /// A generative study's component carried by the faces: its mass, kg, and its centre
    /// of mass, mm. The load cases' gravity weighs it.
    RemoteMass {
        kg: f64,
        point: [f64; 3],
    },
    /// A flow's inlet: fluid coming in normal to the faces, m/s.
    InletVelocity {
        speed: f64,
    },
    /// A flow's inlet: m³/s through the faces, shared by area.
    InletVolumeFlow {
        rate: f64,
    },
    /// A flow's inlet: kg/s through the faces, shared by area.
    InletMassFlow {
        rate: f64,
    },
    /// An opening at a static pressure, Pa (gauge), where the fluid goes out.
    StaticPressure {
        pascals: f64,
    },
    /// A wall the fluid slips along without friction.
    IdealWall,
    /// What a flow study measures on the faces.
    Goal(FlowGoalKind),
}

/// The names items are numbered after, as SolidWorks names them.
pub(crate) const ITEM_BASES: [&str; 26] = [
    "Fixed",
    "Roller/Slider",
    "Force",
    "Pressure",
    "Temperature",
    "Heat Power",
    "Heat Flux",
    "Convection",
    "Radiation",
    "Internal Heat",
    "Preserve",
    "Gravity",
    "Remote Mass",
    "Contact",
    "Inlet Velocity",
    "Inlet Volume Flow",
    "Inlet Mass Flow",
    "Static Pressure",
    "Ideal Wall",
    "Force Goal",
    "Volume Flow Goal",
    "Mass Flow Goal",
    "Pressure Goal",
    "Max Temperature Goal",
    "Mean Temperature Goal",
    "Heat Flow Goal",
];

impl ItemKind {
    fn is_fixture(&self) -> bool {
        matches!(self, ItemKind::Fixed | ItemKind::Roller)
    }

    /// Whether it is a thermal load, which only thermal studies take.
    pub(crate) fn is_thermal(&self) -> bool {
        matches!(
            self,
            ItemKind::Temperature { .. }
                | ItemKind::HeatPower { .. }
                | ItemKind::HeatFlux { .. }
                | ItemKind::Convection { .. }
                | ItemKind::Radiation { .. }
                | ItemKind::InternalHeat { .. }
        )
    }

    /// Whether it is a flow study's condition or goal.
    pub(crate) fn is_flow(&self) -> bool {
        matches!(
            self,
            ItemKind::InletVelocity { .. }
                | ItemKind::InletVolumeFlow { .. }
                | ItemKind::InletMassFlow { .. }
                | ItemKind::StaticPressure { .. }
                | ItemKind::IdealWall
                | ItemKind::Goal(_)
        )
    }

    /// Whether it is a flow study's goal.
    pub(crate) fn is_goal(&self) -> bool {
        matches!(self, ItemKind::Goal(_))
    }

    /// Whether it is a goal only a cooling study measures: a temperature or heat.
    pub(crate) fn is_thermal_goal(&self) -> bool {
        matches!(self, ItemKind::Goal(kind) if kind.is_thermal())
    }

    pub(crate) fn base_name(&self) -> &'static str {
        match self {
            ItemKind::Fixed => "Fixed",
            ItemKind::Roller => "Roller/Slider",
            ItemKind::Force { .. } => "Force",
            ItemKind::Pressure { .. } => "Pressure",
            ItemKind::Temperature { .. } => "Temperature",
            ItemKind::HeatPower { .. } => "Heat Power",
            ItemKind::HeatFlux { .. } => "Heat Flux",
            ItemKind::Convection { .. } => "Convection",
            ItemKind::Radiation { .. } => "Radiation",
            ItemKind::InternalHeat { .. } => "Internal Heat",
            ItemKind::Preserve { .. } => "Preserve",
            ItemKind::Gravity { .. } => "Gravity",
            ItemKind::RemoteMass { .. } => "Remote Mass",
            ItemKind::InletVelocity { .. } => "Inlet Velocity",
            ItemKind::InletVolumeFlow { .. } => "Inlet Volume Flow",
            ItemKind::InletMassFlow { .. } => "Inlet Mass Flow",
            ItemKind::StaticPressure { .. } => "Static Pressure",
            ItemKind::IdealWall => "Ideal Wall",
            ItemKind::Goal(FlowGoalKind::Force) => "Force Goal",
            ItemKind::Goal(FlowGoalKind::VolumeFlow) => "Volume Flow Goal",
            ItemKind::Goal(FlowGoalKind::MassFlow) => "Mass Flow Goal",
            ItemKind::Goal(FlowGoalKind::MeanPressure) => "Pressure Goal",
            ItemKind::Goal(FlowGoalKind::MaxTemperature) => "Max Temperature Goal",
            ItemKind::Goal(FlowGoalKind::MeanTemperature) => "Mean Temperature Goal",
            ItemKind::Goal(FlowGoalKind::HeatFlow) => "Heat Flow Goal",
        }
    }

    /// Whether a study of `t` takes it. A cooling study takes a flow's conditions and goals,
    /// its own goals of temperature and heat, and the thermal loads but radiation.
    pub(crate) fn fits(&self, t: StudyType) -> bool {
        match self {
            ItemKind::Preserve { .. } => {
                matches!(t, StudyType::Topology | StudyType::Generative)
            }
            ItemKind::Gravity { .. } | ItemKind::RemoteMass { .. } => t == StudyType::Generative,
            k if k.is_flow() => t.is_flow() && (t.is_cooling() || !k.is_thermal_goal()),
            k if k.is_thermal() => {
                t.is_thermal() || (t.is_cooling() && !matches!(k, ItemKind::Radiation { .. }))
            }
            k if k.is_fixture() => t.takes_fixtures(),
            _ => t.is_structural(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StudyItem {
    /// `Fixed-1`, `Force-2`… as SolidWorks numbers them.
    pub name: String,
    pub kind: ItemKind,
    pub faces: Vec<SimFace>,
}

/// Which mesher makes the mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MesherChoice {
    /// Netgen, when Oxyde was built with it: graded, true to the faces.
    Netgen,
    /// Oxyde's own lattice mesher.
    Lattice,
}

/// How a study's model is meshed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StudyMesh {
    pub mesher: MesherChoice,
    pub quality: ElementQuality,
    /// A typed element size rather than one from the density.
    pub custom_size: bool,
    /// 0 coarse to 1 fine, when the element size is automatic.
    pub density: f32,
    /// mm, when typed.
    pub element_size: f64,
}

impl Default for StudyMesh {
    fn default() -> Self {
        Self {
            mesher: MesherChoice::Netgen,
            quality: ElementQuality::High,
            custom_size: false,
            density: 0.5,
            element_size: 2.0,
        }
    }
}

/// A contact between two sets of faces, as Fusion 360 lists them.
#[derive(Debug, Clone, PartialEq)]
pub struct StudyContact {
    /// `Contact-1`…
    pub name: String,
    pub kind: ContactKind,
    pub a: Vec<SimFace>,
    pub b: Vec<SimFace>,
    /// mm: the gap an offset bonded contact bridges (and the gap found, for the others).
    pub offset: f64,
    /// Found by Oxyde rather than made by hand; found again (and dropped when the faces no
    /// longer touch) by *Find Contacts*.
    pub automatic: bool,
}

impl StudyContact {
    fn kind_label(kind: ContactKind) -> &'static str {
        match kind {
            ContactKind::Bonded => "Bonded",
            ContactKind::Separation => "Separation",
            ContactKind::Sliding => "Sliding",
            ContactKind::Rough => "Rough",
            ContactKind::OffsetBonded => "Offset bonded",
        }
    }

    const KINDS: [(ContactKind, &'static str); 5] = [
        (ContactKind::Bonded, "The faces move together, glued"),
        (
            ContactKind::Separation,
            "The faces may part but not pass through each other; no friction",
        ),
        (
            ContactKind::Sliding,
            "The faces slide on each other without friction but stay in touch",
        ),
        (ContactKind::Rough, "The faces may part but not slide"),
        (
            ContactKind::OffsetBonded,
            "Glued across the gap between the faces",
        ),
    ];
}

/// What a topology study looks for; its preserved regions are items.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TopologyGoal {
    /// The share of the model's mass to keep, 0 to 1.
    pub target_mass: f64,
    /// mm: the thinnest member wanted; `None` for the finest the mesh allows.
    pub member_size: Option<f64>,
    /// Iterations at most, each one static solve.
    pub iterations: u32,
    /// Mirrored across the planes parallel to the Right, Top and Front planes through
    /// `symmetry_point`.
    pub symmetry: [bool; 3],
    /// mm: the point the symmetry planes pass through; the origin for the standard planes.
    pub symmetry_point: [f64; 3],
    /// The direction a mould is pulled off the shape along, in world coordinates; `None`
    /// for none.
    pub pull: Option<[f64; 3]>,
    /// mm: a two-part mould, pulled off both ways from the plane through this point across
    /// the pull direction; `None` for one way.
    pub parting: Option<[f64; 3]>,
    /// *Keep it in one piece*: pieces holding a fixture, a load or a kept region joined to
    /// the rest, loose bits taken away. On for new studies: a part is made in one piece.
    pub one_piece: bool,
}

impl Default for TopologyGoal {
    fn default() -> Self {
        let file = oxyde_format::simulation::TopologySettings::default();
        Self {
            target_mass: file.target_mass,
            member_size: file.member_size,
            iterations: file.iterations,
            symmetry: [false; 3],
            symmetry_point: [0.0; 3],
            pull: None,
            parting: None,
            one_piece: true,
        }
    }
}

/// The study: what holds the model and what loads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Study {
    /// `Static 1`, as SolidWorks names a study.
    pub name: String,
    pub study_type: StudyType,
    pub items: Vec<StudyItem>,
    /// The model's weight, along −Y (the world's down).
    pub gravity: bool,
    /// Mechanical properties this study uses instead of the material's, by material
    /// group (a material name, or `part:` and a document id for a part without one).
    pub overrides: BTreeMap<String, Mechanical>,
    /// Thermal properties this study uses instead of the material's, by material group.
    pub thermal_overrides: BTreeMap<String, Thermal>,
    /// A transient thermal study's duration, steps and start.
    pub transient: TransientSettings,
    /// The temperature a thermal stress study's model is free of stress at, °C.
    pub reference_temperature: f64,
    /// A topology study's target mass, member size and iterations.
    pub topology: TopologyGoal,
    /// A generative study's bodies, target, methods, materials and resolution, as the file
    /// has them.
    pub generative: GenerativeSettings,
    /// A frequency study's modes: how many natural frequencies it finds, the lowest first.
    pub modes: u32,
    pub contacts: Vec<StudyContact>,
    /// mm: how close faces of different bodies must be to be found in contact.
    pub contact_tolerance: f64,
    /// The contacts were looked for: *Run* does not look again by itself.
    pub contacts_found: bool,
    pub mesh: StudyMesh,
    /// A flow study's fluid, flow and solver settings.
    pub flow: FlowSettings,
    /// A cooling study's fluid temperature, thermal properties and gravity.
    pub cooling: CoolingSettings,
    /// Items made so far of each kind, for their names.
    pub(crate) made: BTreeMap<&'static str, u32>,
}

impl Default for Study {
    fn default() -> Self {
        Self::of_type(StudyType::Static, "Static 1")
    }
}

impl Study {
    /// An empty study of `study_type` called `name`.
    pub(crate) fn of_type(study_type: StudyType, name: &str) -> Self {
        Self {
            name: name.to_owned(),
            study_type,
            items: Vec::new(),
            gravity: false,
            overrides: BTreeMap::new(),
            thermal_overrides: BTreeMap::new(),
            transient: TransientSettings::default(),
            reference_temperature: 20.0,
            topology: TopologyGoal::default(),
            generative: GenerativeSettings::default(),
            modes: oxyde_format::simulation::FrequencySettings::default().modes,
            contacts: Vec::new(),
            contact_tolerance: 0.1,
            contacts_found: false,
            mesh: StudyMesh {
                // A topology study's shape comes from its elements' densities, which
                // four-node elements give as well as ten-node ones with about a seventh of
                // the unknowns: every iteration that much quicker.
                quality: if study_type == StudyType::Topology {
                    ElementQuality::Draft
                } else {
                    ElementQuality::High
                },
                ..StudyMesh::default()
            },
            flow: FlowSettings::default(),
            cooling: CoolingSettings::default(),
            made: BTreeMap::new(),
        }
    }
}

impl Study {
    /// The counters behind the names, after items and contacts were read from a file, so
    /// a new one does not repeat a name.
    pub(crate) fn recount(&mut self) {
        let mut made: BTreeMap<&'static str, u32> = BTreeMap::new();
        let bases = ITEM_BASES;
        let names = self
            .items
            .iter()
            .map(|i| i.name.as_str())
            .chain(self.contacts.iter().map(|c| c.name.as_str()));
        for name in names {
            let Some((base, number)) = name.rsplit_once('-') else {
                continue;
            };
            let (Some(base), Ok(number)) = (bases.iter().find(|b| **b == base), number.parse())
            else {
                continue;
            };
            let count = made.entry(*base).or_insert(0);
            *count = (*count).max(number);
        }
        self.made = made;
    }

    fn next_name(&mut self, base: &'static str) -> String {
        let count = self.made.entry(base).or_insert(0);
        *count += 1;
        format!("{base}-{count}")
    }

    /// A contact made by hand.
    fn add_contact(&mut self, kind: ContactKind, a: Vec<SimFace>, b: Vec<SimFace>) -> usize {
        let name = self.next_name("Contact");
        self.contacts.push(StudyContact {
            name,
            kind,
            a,
            b,
            offset: 0.0,
            automatic: false,
        });
        self.contacts.len() - 1
    }

    /// The contacts found between faces (pairs and the gap between them): automatic
    /// contacts still found keep their kind, new ones are bonded, those no longer found
    /// go; contacts made by hand stay.
    fn merge_found(&mut self, found: Vec<(SimFace, SimFace, f64)>) {
        let old = std::mem::take(&mut self.contacts);
        let (automatic, manual): (Vec<StudyContact>, Vec<StudyContact>) =
            old.into_iter().partition(|c| c.automatic);
        self.contacts = manual;
        for (a, b, gap) in found {
            let same = |c: &&StudyContact| {
                (c.a == [a.clone()] && c.b == [b.clone()])
                    || (c.a == [b.clone()] && c.b == [a.clone()])
            };
            match automatic.iter().find(same) {
                Some(c) => self.contacts.push(c.clone()),
                None => {
                    let name = self.next_name("Contact");
                    self.contacts.push(StudyContact {
                        name,
                        kind: ContactKind::Bonded,
                        a: vec![a],
                        b: vec![b],
                        offset: gap,
                        automatic: true,
                    });
                }
            }
        }
        self.contacts_found = true;
    }

    pub(crate) fn add(&mut self, kind: ItemKind, faces: Vec<SimFace>) -> usize {
        let name = self.next_name(kind.base_name());
        self.items.push(StudyItem { name, kind, faces });
        self.items.len() - 1
    }
}

// ---- The surfaces --------------------------------------------------------------------------

/// A solid of the model handed to the mesher.
#[derive(Debug, Clone)]
pub(crate) struct BodyInfo {
    name: String,
    /// The part's body it is.
    pub(crate) id: Option<BodyId>,
    /// The part occurrence holding it, in an assembly.
    pub(crate) occurrence: Option<Occurrence>,
    /// Its material group: the material's name, or `part:` and the part's document id.
    key: String,
    material: Option<Material>,
    /// The part file it comes from (an assembly's part), to give it another material.
    path: Option<std::path::PathBuf>,
}

impl BodyInfo {
    /// The body as a study names it.
    pub(crate) fn study_body(&self) -> Option<oxyde_format::simulation::StudyBody> {
        let id = self.id?;
        Some(match &self.occurrence {
            Some(o) => oxyde_format::simulation::StudyBody::component(o.top, o.path.clone(), id),
            None => oxyde_format::simulation::StudyBody::part(id),
        })
    }
}

/// Where a solid of the model is, for a volume mesher that works on the shape itself.
#[derive(Debug, Clone)]
struct BodySource {
    /// The solid's shape (the part's, valid for the revision the surfaces were made for).
    shape: ShapeHandle,
    /// Where an assembly places it.
    placement: Option<Placement>,
    /// The label of each of its faces, in its own topology's order.
    face_labels: Vec<u32>,
}

/// The model's surfaces for the mesher, made for one revision of the document.
pub(crate) struct Surfaces {
    pub(crate) revision: u64,
    pub(crate) assembly: bool,
    pub(crate) solids: Vec<SurfaceMesh>,
    /// The face each label stands for.
    pub(crate) faces: Vec<SimFace>,
    /// The body (index into `bodies`) of each label.
    pub(crate) face_body: Vec<u32>,
    pub(crate) bodies: Vec<BodyInfo>,
    sources: Vec<BodySource>,
    /// mm³, mm² and mm.
    volume: f64,
    area: f64,
    pub(crate) diagonal: f64,
}

#[cfg(test)]
impl Surfaces {
    /// The surfaces of `solids` whose labels stand for `faces`, for tests: a body each
    /// (`Body 0`…, material group `test:0`…), without a material.
    pub(crate) fn of_solids(solids: Vec<SurfaceMesh>, faces: Vec<SimFace>) -> Self {
        let bodies = (0..solids.len())
            .map(|i| BodyInfo {
                name: format!("Body {i}"),
                id: None,
                occurrence: None,
                key: format!("test:{i}"),
                material: None,
                path: None,
            })
            .collect();
        Self {
            revision: 0,
            assembly: false,
            solids,
            faces,
            face_body: Vec::new(),
            bodies,
            sources: Vec::new(),
            volume: 0.0,
            area: 0.0,
            diagonal: 1.0,
        }
    }
}

#[cfg(test)]
impl Surfaces {
    /// The surfaces of a part's `bodies` as a test makes them: `solids` labelled by index
    /// into `faces`, `face_body` the body of each.
    pub(crate) fn for_test(
        solids: Vec<SurfaceMesh>,
        faces: Vec<SimFace>,
        face_body: Vec<u32>,
        bodies: Vec<(String, Option<BodyId>, Option<Material>)>,
    ) -> Surfaces {
        Surfaces {
            revision: 0,
            assembly: false,
            solids,
            faces,
            face_body,
            bodies: bodies
                .into_iter()
                .map(|(name, id, material)| BodyInfo {
                    key: material
                        .as_ref()
                        .map_or_else(|| "part:x".to_owned(), |m| m.name.clone()),
                    name,
                    id,
                    occurrence: None,
                    material,
                    path: None,
                })
                .collect(),
            sources: Vec::new(),
            volume: 0.0,
            area: 0.0,
            diagonal: 1.0,
        }
    }
}

impl Surfaces {
    /// Each solid's name, thermal conductivity (W/(m·K)) and density (kg/m³; 1000 where its
    /// material gives none, which a steady state never uses), in the order of `solids`; or
    /// which body has no conductivity.
    pub(crate) fn conductivities(&self, study: &Study) -> Result<Vec<(String, f64, f64)>, String> {
        self.bodies
            .iter()
            .map(|body| match thermal_material_of(study, body) {
                Some(t) if t.conductivity.is_finite() && t.conductivity > 0.0 => Ok((
                    body.name.clone(),
                    t.conductivity,
                    if t.density.is_finite() && t.density > 0.0 {
                        t.density
                    } else {
                        1000.0
                    },
                )),
                _ => Err(format!(
                    "{} has no known thermal conductivity: apply a material or type it in Material",
                    body.name
                )),
            })
            .collect()
    }

    /// The labels of `faces`.
    pub(crate) fn labels(&self, faces: &[SimFace]) -> Vec<u32> {
        self.faces
            .iter()
            .enumerate()
            .filter(|(_, f)| faces.contains(f))
            .map(|(i, _)| i as u32)
            .collect()
    }
}

/// A material group, for the panel's Material list.
#[derive(Debug, Clone, PartialEq)]
struct MaterialGroup {
    key: String,
    label: String,
    /// The properties the study uses and where they come from.
    effective: Option<(Mechanical, PropsSource)>,
    /// The thermal properties the study uses and where they come from.
    thermal: Option<(ThermalProps, PropsSource)>,
    density: f64,
    /// The part's own material can take the values (the open part has it).
    can_apply: bool,
    /// The group is a material (not a part that has none).
    has_material: bool,
    /// The bodies in the group, for the hover.
    bodies: Vec<String>,
    /// The part files the group's bodies come from (an assembly); empty for the open part.
    paths: Vec<std::path::PathBuf>,
}

/// Where a study's mechanical properties come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PropsSource {
    /// Typed in the study, over the material's.
    Study,
    /// The material's own.
    Material,
    /// Typical values for the material's name: it has none of its own.
    Typical,
}

// ---- The job -------------------------------------------------------------------------------

/// What a run is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunMode {
    /// Mesh and show the mesh.
    Mesh,
    /// Mesh, find the contacts and list them.
    FindContacts,
    /// Mesh and solve.
    Run,
}

pub(crate) struct Outcome {
    pub(crate) mesh: TetMesh,
    pub(crate) report: MeshReport,
    pub(crate) results: Option<Results>,
    pub(crate) thermal: Option<ThermalResults>,
    pub(crate) topology: Option<TopologyResults>,
    pub(crate) frequency: Option<FrequencyResults>,
    pub(crate) mesh_seconds: f64,
    pub(crate) solve_seconds: f64,
    /// Faces found facing each other, when contacts were looked for.
    pub(crate) candidates: Option<Vec<ContactCandidate>>,
    /// `netgen`, `lattice` or `openfoam`.
    pub(crate) mesher: &'static str,
    /// Why Netgen was asked for and not used.
    pub(crate) note: Option<String>,
    /// A flow study's run.
    pub(crate) flow: Option<crate::flow_ui::FlowDone>,
}

#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) stage: &'static str,
    pub(crate) fraction: f64,
    pub(crate) outcome: Option<Result<Outcome, String>>,
    /// A flow study's residuals so far, one record per iteration.
    pub(crate) residuals: Vec<Vec<(String, f64)>>,
}

pub(crate) struct SimJob {
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) shared: Arc<Mutex<Shared>>,
    pub(crate) revision: u64,
    pub(crate) assembly: bool,
    pub(crate) started: Instant,
    pub(crate) diagonal: f64,
    pub(crate) mode: RunMode,
    /// The face of each label and the name and material group of each body, as the run
    /// was made.
    pub(crate) faces: Vec<SimFace>,
    pub(crate) bodies: Vec<(String, String)>,
    /// The model and study as they were run, for the results file.
    pub(crate) fingerprint: Option<Fingerprint>,
    pub(crate) study_type: StudyType,
}

impl Drop for SimJob {
    /// The thread notices at its next check and ends; nothing waits for it.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// A mesh, and the results when the study was solved.
pub(crate) struct Done {
    /// Tells one run's plot from the next.
    pub(crate) id: u64,
    pub(crate) revision: u64,
    pub(crate) assembly: bool,
    pub(crate) mesh: TetMesh,
    pub(crate) report: MeshReport,
    /// What was solved: the study's type then.
    pub(crate) study_type: StudyType,
    /// Displacements and stresses (a static or thermal stress study).
    pub(crate) results: Option<Results>,
    pub(crate) summary: Option<Summary>,
    /// Temperatures and heat flux (a thermal, transient or thermal stress study).
    pub(crate) thermal: Option<ThermalResults>,
    pub(crate) thermal_summary: Option<ThermalSummary>,
    /// The densities of a topology study.
    pub(crate) topology: Option<TopologyResults>,
    /// The modes of a frequency study.
    pub(crate) frequency: Option<FrequencyResults>,
    pub(crate) mesh_seconds: f64,
    pub(crate) solve_seconds: f64,
    pub(crate) diagonal: f64,
    /// `netgen` or `lattice`.
    pub(crate) mesher: &'static str,
    /// The face each label of the mesh stands for.
    pub(crate) faces: BTreeMap<u32, SimFace>,
    /// Each body's name and material group.
    pub(crate) bodies: Vec<(String, String)>,
    /// The model and study as they were solved.
    pub(crate) fingerprint: Option<Fingerprint>,
    /// A flow study's run: its record, and the fields read from its case.
    pub(crate) flow: Option<crate::flow_ui::FlowDone>,
}

impl Done {
    /// Whether it holds results (not just a mesh).
    pub(crate) fn solved(&self) -> bool {
        self.results.is_some()
            || self.thermal.is_some()
            || self.topology.is_some()
            || self.frequency.is_some()
            || self.flow.is_some()
    }

    /// Whether it can draw `plot`.
    pub(crate) fn has(&self, plot: PlotChoice) -> bool {
        match plot {
            PlotChoice::Shape => self.topology.is_some(),
            PlotChoice::ModeShape => self.frequency.is_some(),
            p if p.is_flow() => self.flow.as_ref().is_some_and(|f| {
                f.fields
                    .as_ref()
                    .is_some_and(|r| !p.is_cooling() || r.temperature.is_some())
            }),
            p if p.is_thermal() => self.thermal.is_some(),
            _ => self.results.is_some(),
        }
    }

    /// `wanted` if it can draw it, else the first plot it can (temperature for thermal
    /// results, von Mises for static ones).
    pub(crate) fn plot_for(&self, wanted: PlotChoice) -> Option<PlotChoice> {
        if self.has(wanted) {
            return Some(wanted);
        }
        PlotChoice::ALL.into_iter().find(|p| self.has(*p))
    }

    /// The values of `plot` at each node: temperatures at `frame` of a transient study, the
    /// motion in mode `frame` of a frequency study.
    fn values(&self, plot: PlotChoice, frame: Option<usize>) -> Vec<f64> {
        match (plot, &self.thermal, &self.results) {
            (PlotChoice::Shape, _, _) => vec![0.0; self.mesh.nodes.len()],
            (PlotChoice::ModeShape, _, _) => frame
                .and_then(|k| self.frequency.as_ref()?.modes.get(k))
                .map_or_else(|| vec![0.0; self.mesh.nodes.len()], |m| m.magnitudes()),
            (PlotChoice::Temperature, Some(t), _) => frame
                .and_then(|f| t.frames.get(f))
                .map_or_else(|| t.temperature.clone(), |f| f.temperature.clone()),
            (PlotChoice::HeatFlux, Some(t), _) => t.heat_flux_magnitude(),
            (p, _, Some(r)) if !p.is_thermal() => p.values(r),
            _ => vec![0.0; self.mesh.nodes.len()],
        }
    }
}

#[cfg(test)]
impl Done {
    /// A run's outcome as a test makes it: `mesh`, one body, and a topology study's
    /// densities.
    pub(crate) fn for_test(
        mesh: TetMesh,
        study_type: StudyType,
        topology: Option<TopologyResults>,
    ) -> Done {
        let report = MeshReport {
            element_size: 0.0,
            nodes: mesh.nodes.len(),
            elements: mesh.tets.len(),
            volume: mesh.volume(),
            model_volume: mesh.volume(),
            dropped: 0,
        };
        Done {
            id: 0,
            revision: 0,
            assembly: false,
            mesh,
            report,
            study_type,
            results: None,
            summary: None,
            thermal: None,
            thermal_summary: None,
            topology,
            frequency: None,
            flow: None,
            mesh_seconds: 0.0,
            solve_seconds: 0.0,
            diagonal: 1.0,
            mesher: "lattice",
            faces: BTreeMap::new(),
            bodies: vec![("Body".to_owned(), "part:x".to_owned())],
            fingerprint: None,
        }
    }
}

/// What the viewport shows: anything else, and the plot is made again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlotKey {
    pub(crate) id: u64,
    pub(crate) plot: Option<PlotChoice>,
    pub(crate) scale: f64,
    pub(crate) bands: u32,
    pub(crate) show_mesh: bool,
    /// The transient study's time shown.
    pub(crate) frame: Option<usize>,
    /// A flow's cut plane: its axis and where it is, 0 to 1.
    pub(crate) cut: Option<(CutAxis, f64)>,
    /// A topology study's shape: the density it is cut at, smoothed, the model around it.
    pub(crate) threshold: f64,
    pub(crate) smooth: bool,
    pub(crate) ghost: bool,
}

/// The legend of the plot shown.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Legend {
    pub(crate) title: String,
    pub(crate) unit: &'static str,
    pub(crate) range: (f64, f64),
    pub(crate) bands: u32,
    pub(crate) reversed: bool,
    pub(crate) scale: f64,
    /// The node marked (deformed position) and its label.
    pub(crate) marker: Option<([f64; 3], String)>,
}

/// The Simulation tab's state.
#[derive(Default)]
pub struct SimulationView {
    /// The document the study is for (assembly or not, document id).
    document: Option<(bool, String)>,
    pub study: Study,
    /// Where the study is saved, once it is.
    pub(crate) file: Option<StudyFile>,
    pub(crate) surfaces: Option<Surfaces>,
    /// Why the surfaces could not be made for this revision (assembly or not): not tried
    /// again every frame.
    surfaces_failed: Option<(u64, bool, String)>,
    pub(crate) job: Option<SimJob>,
    pub(crate) done: Option<Done>,
    /// The plot (or the mesh) is shown over the model; off while setting up.
    pub(crate) show_plot: bool,
    pub(crate) shown: Option<PlotKey>,
    /// The flow result (its `Done` id) and axis the cut plane was last put through the
    /// middle of the model for: a new result or axis starts it there again.
    pub(crate) cut_centred: Option<(u64, CutAxis)>,
    pub(crate) legend: Option<Legend>,
    pub(crate) error: Option<String>,
    pub(crate) prefs_dirty: bool,
    /// The item picked in the panel, whose faces are selected.
    pub(crate) selected: Option<usize>,
    /// The contact picked in the panel.
    pub(crate) selected_contact: Option<usize>,
    /// Whether a study is open: the tab starts on *New Study* until there is one.
    pub(crate) started: bool,
    /// The model's other studies, in order, without the open one (which sits at `active`
    /// among them).
    pub(crate) others: Vec<crate::simulation_studies::Parked>,
    /// The Generative Design tab's state beside the open study: its outcomes, the
    /// generation running, what Explore shows.
    pub(crate) generative: crate::generative_ui::GenView,
    pub(crate) active: usize,
    /// The time of a transient study's results shown, an index into its frames.
    pub(crate) frame: usize,
    /// The mode of a frequency study's results shown, an index into its modes.
    pub(crate) mode: usize,
    /// While the frames play: when the last one was shown.
    pub(crate) playing: Option<Instant>,
    /// A flow's trajectories, as the viewport animates them.
    pub(crate) particles: Option<crate::flow_ui::Particles>,
    /// The open study's new name, while it is being renamed.
    pub(crate) renaming: Option<String>,
    /// The share of the mesh's volume the topology shape shown holds.
    pub(crate) shape_fraction: Option<f64>,
    /// The pieces the topology shape shown falls into, the largest first.
    pub(crate) shape_pieces: Vec<oxyde_sim::Piece>,
}

impl SimulationView {
    /// Forgets the study and its results: another document.
    fn reset(&mut self) {
        let dirty = self.prefs_dirty;
        *self = SimulationView {
            prefs_dirty: dirty,
            ..SimulationView::default()
        };
    }
}

/// The material groups of `bodies`, in order: one per material (or per part without
/// one), with the bodies and part files it covers, so that a material given to the group
/// goes to all of them.
fn material_groups_of(bodies: &[BodyInfo], study: &Study, assembly: bool) -> Vec<MaterialGroup> {
    let mut out: Vec<MaterialGroup> = Vec::new();
    for b in bodies {
        if let Some(g) = out.iter_mut().find(|g| g.key == b.key) {
            if !g.bodies.contains(&b.name) {
                g.bodies.push(b.name.clone());
            }
            if let Some(p) = &b.path
                && !g.paths.contains(p)
            {
                g.paths.push(p.clone());
            }
            continue;
        }
        let (label, density) = match &b.material {
            Some(m) => (m.name.clone(), m.density),
            None => (format!("{} (no material)", b.name), 0.0),
        };
        out.push(MaterialGroup {
            key: b.key.clone(),
            label,
            effective: mechanical_of(study, &b.key, b.material.as_ref()),
            thermal: thermal_of(study, &b.key, b.material.as_ref()),
            density,
            can_apply: !assembly && b.material.is_some(),
            has_material: b.material.is_some(),
            bodies: vec![b.name.clone()],
            paths: b.path.iter().cloned().collect(),
        });
    }
    out
}

/// The mechanical properties a study uses for a material group: its own override, else
/// the material's, else typical values for the material's name.
fn mechanical_of(
    study: &Study,
    key: &str,
    material: Option<&Material>,
) -> Option<(Mechanical, PropsSource)> {
    if let Some(p) = study.overrides.get(key) {
        return Some((*p, PropsSource::Study));
    }
    let m = material?;
    if let Some(p) = m.mechanical {
        return Some((p, PropsSource::Material));
    }
    let e = Elastic::guess(&m.name, &m.category)?;
    Some((
        Mechanical::new(e.youngs_modulus, e.poisson, e.yield_strength, 0.0),
        PropsSource::Typical,
    ))
}

/// The thermal properties a study uses for a material group: its own override, else the
/// material's, else typical values for the material's name.
fn thermal_of(
    study: &Study,
    key: &str,
    material: Option<&Material>,
) -> Option<(ThermalProps, PropsSource)> {
    let props = |t: &Thermal| ThermalProps::new(t.conductivity, t.specific_heat, t.expansion);
    if let Some(t) = study.thermal_overrides.get(key) {
        return Some((props(t), PropsSource::Study));
    }
    let m = material?;
    if let Some(t) = &m.thermal {
        return Some((props(t), PropsSource::Material));
    }
    ThermalProps::guess(&m.name, &m.category).map(|t| (t, PropsSource::Typical))
}

/// The solver's material for `body`: [`mechanical_of`] with the material's density, the
/// factor of safety measured against the yield strength, else the tensile strength, and
/// the expansion of [`thermal_of`] (0 when unknown).
fn elastic_of(study: &Study, body: &BodyInfo) -> Option<Elastic> {
    let density = body.material.as_ref().map_or(0.0, |m| m.density);
    let (p, _) = mechanical_of(study, &body.key, body.material.as_ref())?;
    let expansion =
        thermal_of(study, &body.key, body.material.as_ref()).map_or(0.0, |(t, _)| t.expansion);
    Some(
        Elastic::new(p.elastic_modulus, p.poisson_ratio, p.strength(), density)
            .with_expansion(expansion),
    )
}

/// The thermal solver's material for `body`: [`thermal_of`] with the material's density.
fn thermal_material_of(study: &Study, body: &BodyInfo) -> Option<ThermalMaterial> {
    let density = body.material.as_ref().map_or(0.0, |m| m.density);
    let (t, _) = thermal_of(study, &body.key, body.material.as_ref())?;
    Some(ThermalMaterial::new(
        t.conductivity,
        t.specific_heat,
        density,
    ))
}

/// Turns a kernel mesh (one solid, placed) into the mesher's surface, labelling each
/// face's triangles with `label(face index)`.
fn surface_of(mesh: &Mesh, label: impl Fn(usize) -> u32) -> SurfaceMesh {
    let mut s = SurfaceMesh::default();
    for (k, face) in mesh.faces.iter().enumerate() {
        let base = s.positions.len() as u32;
        s.positions.extend(
            face.positions
                .iter()
                .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]),
        );
        let l = label(k);
        for t in &face.triangles {
            s.triangles.push([base + t[0], base + t[1], base + t[2]]);
            s.labels.push(l);
        }
    }
    s
}

fn surface_area(s: &SurfaceMesh) -> f64 {
    s.triangles
        .iter()
        .filter_map(|t| {
            let p = |i: u32| s.positions.get(i as usize).copied();
            let (a, b, c) = (p(t[0])?, p(t[1])?, p(t[2])?);
            let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            Some(0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt())
        })
        .sum()
}

/// What a static solve takes at its peak per node of the mesh, bytes: the matrix, the
/// multigrid hierarchy, the vectors and the stresses. Measured on the lattice mesher's
/// cubes (a 100 mm cube, every core): ten-node elements 16 KB per node at 156 000 and
/// 337 000 nodes, four-node ones 24 KB at 79 000 and 203 000 (a node of a four-node mesh
/// is in more elements); rounded up.
const BYTES_PER_NODE_QUADRATIC: f64 = 17_000.0;
const BYTES_PER_NODE_LINEAR: f64 = 25_000.0;

/// The most nodes a mesh may have to stay within `budget_gb` of memory.
pub(crate) fn max_nodes(budget_gb: f64, quadratic: bool) -> usize {
    let per_node = if quadratic {
        BYTES_PER_NODE_QUADRATIC
    } else {
        BYTES_PER_NODE_LINEAR
    };
    let budget = if budget_gb.is_finite() {
        budget_gb.max(0.25)
    } else {
        8.0
    };
    (budget * 1e9 / per_node) as usize
}

/// Why a mesh with `nodes` nodes is refused, and what to do about it.
fn too_many_nodes(nodes: usize, budget_gb: f64, quadratic: bool) -> String {
    format!(
        "the mesh would have {nodes} nodes, more than the memory budget of {} GB allows \
         (about {} nodes): choose a larger element size, or raise the budget in \
         Preferences › Simulation",
        format_number(budget_gb),
        max_nodes(budget_gb, quadratic)
    )
}

// ---- The application side ------------------------------------------------------------------

impl OxydeApp {
    /// Whether the Simulation tab with one of its studies open, or the Generative Design
    /// tab with a generative study open, is open and in charge of the viewport.
    pub(crate) fn simulation_active(&self) -> bool {
        let simulation = self.simulation_tab_open();
        let generative = self.generative_study_open();
        let tab = (simulation && !generative) || (self.generative_tab_open() && generative);
        tab && self.sketch.is_none()
            && self.section.is_none()
            && self.mesh_export.is_none()
            && !self.busy()
    }

    /// Whether the Simulation tab is the one open.
    pub(crate) fn simulation_tab_open(&self) -> bool {
        match self.asm {
            Some(_) => self.asm_tab == crate::assembly_ui::AsmTab::Simulation,
            None => self.tab == crate::app::CommandTab::Simulation,
        }
    }

    fn document_key(&self) -> (bool, String) {
        match &self.asm {
            Some(asm) => (true, asm.document_id.clone()),
            None => (false, self.doc.document_id.clone()),
        }
    }

    /// Every frame: forgets the study of a document that is gone, takes the job's
    /// progress, puts the usual scene back when the plot should not show, saves the
    /// settings once a drag is over.
    pub(crate) fn simulation_housekeeping(&mut self, ctx: &egui::Context) {
        let key = self.document_key();
        if self.simulation.document.as_ref() != Some(&key) {
            self.restore_simulation_scene();
            self.simulation.reset();
            self.simulation.document = Some(key);
            // The model's studies, when they were saved next to it.
            self.open_saved_studies();
        }
        self.poll_simulation(ctx);
        self.generative_housekeeping(ctx);
        self.advance_playback(ctx);
        self.advance_particles(ctx);
        if self.simulation.shown.is_some() && !self.plot_wanted() {
            self.restore_simulation_scene();
            ctx.request_repaint();
        }
        if self.simulation.prefs_dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.simulation.prefs_dirty = false;
            self.save_preferences();
        }
    }

    /// While a transient study plays: the next time every so often, back to the start
    /// after the end.
    fn advance_playback(&mut self, ctx: &egui::Context) {
        let Some(last) = self.simulation.playing else {
            return;
        };
        let frames = self
            .simulation
            .done
            .as_ref()
            .and_then(|d| d.thermal.as_ref())
            .map_or(0, |t| t.frames.len());
        if frames < 2 || !self.simulation_active() {
            self.simulation.playing = None;
            return;
        }
        let step = Duration::from_millis(120);
        if last.elapsed() >= step {
            self.simulation.frame = (self.simulation.frame + 1) % frames;
            self.simulation.playing = Some(Instant::now());
        }
        ctx.request_repaint_after(step);
    }

    /// Whether a plot or mesh should be on show now: the tab is open, it is asked for,
    /// and the result is of the model as it is.
    fn plot_wanted(&self) -> bool {
        self.simulation_active()
            && self.simulation.show_plot
            && self.simulation.done.as_ref().is_some_and(|d| {
                d.revision == self.document_revision() && d.assembly == self.asm.is_some()
            })
    }

    pub(crate) fn restore_simulation_scene(&mut self) {
        let generative = self.simulation.generative.take_shown();
        if self.simulation.shown.take().is_some() || generative {
            // The usual scene comes back on the next frame.
            self.viewport.pickable = None;
            self.scene_revision = None;
            self.asm_scene = None;
        }
        self.simulation.legend = None;
        self.simulation.shape_fraction = None;
        self.simulation.shape_pieces.clear();
    }

    fn poll_simulation(&mut self, ctx: &egui::Context) {
        let Some(job) = self.simulation.job.as_ref() else {
            return;
        };
        let outcome = match job.shared.lock() {
            Ok(mut s) => s.outcome.take(),
            Err(_) => Some(Err("the simulation thread stopped".to_owned())),
        };
        let Some(outcome) = outcome else {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        };
        let Some(job) = self.simulation.job.take() else {
            return;
        };
        match outcome {
            Ok(o) => {
                let summary = o.results.as_ref().map(Results::summary);
                let thermal_summary = o.thermal.as_ref().map(ThermalResults::summary);
                let seconds = o.mesh_seconds + o.solve_seconds;
                self.status = match (&summary, &thermal_summary) {
                    (Some(s), Some(t)) => format!(
                        "Study solved in {seconds:.1} s: {} to {}, von Mises up to {}",
                        format_value(t.min_temperature.0, "°C"),
                        format_value(t.max_temperature.0, "°C"),
                        format_value(s.max_von_mises.0, "MPa"),
                    ),
                    (Some(s), None) => format!(
                        "Study solved in {seconds:.1} s: von Mises up to {}, displacement up to {}",
                        format_value(s.max_von_mises.0, "MPa"),
                        format_value(s.max_displacement.0, "mm"),
                    ),
                    (None, Some(t)) => format!(
                        "Study solved in {seconds:.1} s: temperatures from {} to {}",
                        format_value(t.min_temperature.0, "°C"),
                        format_value(t.max_temperature.0, "°C"),
                    ),
                    (None, None) => format!(
                        "Meshed in {:.1} s: {} nodes, {} elements",
                        o.mesh_seconds, o.report.nodes, o.report.elements
                    ),
                };
                if let Some(t) = &o.topology {
                    self.status = topology_summary(t, seconds);
                }
                if let Some(f) = &o.frequency {
                    self.status = frequency_summary(f, seconds);
                }
                if let Some(f) = &o.flow {
                    self.status = crate::flow_ui::flow_status(f, seconds);
                }
                if let Some(note) = &o.note {
                    self.status =
                        format!("{} ({note}; meshed with the lattice mesher)", self.status);
                }
                if let Some(candidates) = &o.candidates {
                    let face = |l: u32| job.faces.get(l as usize).cloned();
                    let found: Vec<(SimFace, SimFace, f64)> = candidates
                        .iter()
                        .filter_map(|c| Some((face(c.a_label)?, face(c.b_label)?, c.gap)))
                        .collect();
                    let count = found.len();
                    self.simulation.study.merge_found(found);
                    if job.mode == RunMode::FindContacts {
                        self.status = format!("{count} contact(s) found between the bodies");
                    }
                }
                let solved = o.results.is_some()
                    || o.thermal.is_some()
                    || o.topology.is_some()
                    || o.frequency.is_some()
                    || o.flow.is_some();
                // Contacts Run found and bonded itself are now in the study it solved: the
                // fingerprint says so, unless the model changed meanwhile (then the
                // results are stale and are not saved).
                let fingerprint = if solved && o.candidates.is_some() {
                    (job.revision == self.document_revision())
                        .then(|| self.simulation_fingerprint().ok())
                        .flatten()
                } else {
                    job.fingerprint.clone()
                };
                self.simulation.done = Some(Done {
                    id: crate::document::next_revision(),
                    revision: job.revision,
                    assembly: job.assembly,
                    mesh: o.mesh,
                    report: o.report,
                    study_type: job.study_type,
                    results: o.results,
                    summary,
                    thermal_summary,
                    thermal: o.thermal,
                    topology: o.topology,
                    frequency: o.frequency,
                    mesh_seconds: o.mesh_seconds,
                    solve_seconds: o.solve_seconds,
                    diagonal: job.diagonal,
                    mesher: o.mesher,
                    faces: job
                        .faces
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(i, f)| (i as u32, f))
                        .collect(),
                    bodies: job.bodies.clone(),
                    fingerprint,
                    flow: o.flow,
                });
                self.simulation.error = None;
                self.simulation.show_plot = true;
                // A transient study opens on its last time, stopped.
                self.simulation.frame = self
                    .simulation
                    .done
                    .as_ref()
                    .and_then(|d| d.thermal.as_ref())
                    .map_or(0, |t| t.frames.len().saturating_sub(1));
                self.simulation.playing = None;
                // A frequency study opens on its first mode.
                self.simulation.mode = 0;
                // A saved study keeps its results beside it, as SolidWorks does, and is
                // saved with them, so the two agree (contacts found by Run included).
                if solved && self.simulation.file.is_some() {
                    let status = self.status.clone();
                    if self.save_study(false) && self.save_results(false) {
                        self.status = format!("{status}; results saved");
                    }
                }
            }
            Err(message) => {
                self.simulation.error = Some(message.clone());
                self.status = message;
            }
        }
        ctx.request_repaint();
    }

    /// The model's surfaces for the mesher, made again when the document changed.
    pub(crate) fn ensure_surfaces(&mut self) -> Result<(), String> {
        let (revision, assembly) = (self.document_revision(), self.asm.is_some());
        if self
            .simulation
            .surfaces
            .as_ref()
            .is_some_and(|s| s.revision == revision && s.assembly == assembly)
        {
            return Ok(());
        }
        if let Some((r, a, why)) = &self.simulation.surfaces_failed
            && (*r, *a) == (revision, assembly)
        {
            return Err(why.clone());
        }
        match self.make_surfaces() {
            Ok(surfaces) => {
                self.simulation.surfaces = Some(surfaces);
                self.simulation.surfaces_failed = None;
                Ok(())
            }
            Err(why) => {
                self.simulation.surfaces = None;
                self.simulation.surfaces_failed = Some((revision, assembly, why.clone()));
                Err(why)
            }
        }
    }

    fn make_surfaces(&self) -> Result<Surfaces, String> {
        let diagonal = self.model_diagonal();
        // Finer than any element, so the boundary is where the model is.
        let deflection = (diagonal * 5e-4).max(1e-3);
        let angle = 12f64.to_radians();
        let mut out = Surfaces {
            revision: self.document_revision(),
            assembly: self.asm.is_some(),
            solids: Vec::new(),
            faces: Vec::new(),
            face_body: Vec::new(),
            bodies: Vec::new(),
            sources: Vec::new(),
            volume: 0.0,
            area: 0.0,
            diagonal,
        };
        let add = |out: &mut Surfaces,
                   mesh: Mesh,
                   names: Vec<Option<FaceName>>,
                   occurrence: Option<Occurrence>,
                   body: BodyInfo,
                   shape: ShapeHandle,
                   placement: Option<Placement>| {
            let body_index = out.bodies.len() as u32;
            let mut labels = Vec::with_capacity(names.len());
            for name in names {
                match name {
                    Some(face) => {
                        labels.push(out.faces.len() as u32);
                        out.faces.push(SimFace {
                            occurrence: occurrence.clone(),
                            face,
                        });
                        out.face_body.push(body_index);
                    }
                    None => labels.push(NO_LABEL),
                }
            }
            let surface = surface_of(&mesh, |k| labels.get(k).copied().unwrap_or(NO_LABEL));
            out.volume += surface.volume().abs();
            out.area += surface_area(&surface);
            out.solids.push(surface);
            out.bodies.push(body);
            out.sources.push(BodySource {
                shape,
                placement,
                face_labels: labels,
            });
        };
        match &self.asm {
            None => {
                let part = &self.doc.part;
                let Some(body) = part.body() else {
                    return Err("There is no solid to study yet".to_owned());
                };
                let key = crate::render_ui::finish_key(part, &self.doc.document_id);
                let several = body.solids.len() > 1;
                for (s, solid) in body.solids.iter().enumerate() {
                    let mesh = self
                        .kernel
                        .tessellate_for_export(solid.shape, deflection, angle)
                        .map_err(|e| format!("Meshing the faces failed: {e}"))?;
                    let names = (0..mesh.faces.len() as u32)
                        .map(|k| body.naming.face(solid.faces.start + k).cloned())
                        .collect();
                    let name = if several {
                        format!("{} (body {})", self.doc.display_name(), s + 1)
                    } else {
                        self.doc.display_name()
                    };
                    add(
                        &mut out,
                        mesh,
                        names,
                        None,
                        BodyInfo {
                            name,
                            id: Some(solid.id),
                            occurrence: None,
                            key: key.clone(),
                            material: part.material().cloned(),
                            path: None,
                        },
                        solid.shape,
                        None,
                    );
                }
            }
            Some(asm) => {
                for (o, placement, loaded) in asm.leaves(&asm.assembly) {
                    let Some(body) = loaded.part.body() else {
                        continue;
                    };
                    let key = crate::render_ui::finish_key(&loaded.part, &loaded.document_id);
                    for solid in &body.solids {
                        let mesh = self
                            .kernel
                            .tessellate_for_export(solid.shape, deflection, angle)
                            .map_err(|e| format!("Meshing the faces failed: {e}"))?;
                        let mesh = oxyde_assembly::scene::placed(&mesh, &placement);
                        let names = (0..mesh.faces.len() as u32)
                            .map(|k| body.naming.face(solid.faces.start + k).cloned())
                            .collect();
                        add(
                            &mut out,
                            mesh,
                            names,
                            Some(o.clone()),
                            BodyInfo {
                                name: asm.occurrence_name(&o),
                                id: Some(solid.id),
                                occurrence: Some(o.clone()),
                                key: key.clone(),
                                material: loaded.part.material().cloned(),
                                path: asm.path_at(&o),
                            },
                            solid.shape,
                            Some(placement),
                        );
                    }
                }
            }
        }
        if out.solids.is_empty() {
            return Err("There is no solid to study yet".to_owned());
        }
        Ok(out)
    }

    /// The element size the mesh is made with: typed in, or from the density and the
    /// model's volume (finer for thin models, which would otherwise lose their walls).
    fn simulation_element_size(&self) -> Option<f64> {
        let p = &self.simulation.study.mesh;
        if p.custom_size {
            return (p.element_size.is_finite() && p.element_size > 0.0).then_some(p.element_size);
        }
        let s = self.simulation.surfaces.as_ref()?;
        if s.volume <= 0.0 || s.area <= 0.0 {
            return None;
        }
        let cells = 400.0 * 50f64.powf(f64::from(p.density.clamp(0.0, 1.0)));
        let by_volume = (s.volume / cells).cbrt();
        // About 2V/A is a wall's thickness: two elements or so across it.
        let by_thickness = 0.6 * 2.0 * s.volume / s.area;
        let h = by_volume.min(by_thickness).max(s.diagonal / 400.0);
        // Two significant figures, as one would type it.
        let magnitude = 10f64.powf(h.log10().floor() - 1.0);
        Some((h / magnitude).round() * magnitude)
    }

    /// The material groups met in the model, in order.
    fn material_groups(&self) -> Vec<MaterialGroup> {
        let Some(s) = self.simulation.surfaces.as_ref() else {
            return Vec::new();
        };
        material_groups_of(&s.bodies, &self.simulation.study, self.asm.is_some())
    }

    /// The faces selected now, as study faces.
    pub(crate) fn selected_sim_faces(&self) -> Vec<SimFace> {
        match &self.asm {
            Some(asm) => asm
                .selection
                .iter()
                .filter_map(|s| match s {
                    AsmSelected::Face(o, f) => Some(SimFace {
                        occurrence: Some(o.clone()),
                        face: f.clone(),
                    }),
                    _ => None,
                })
                .collect(),
            None => self
                .selection
                .iter()
                .filter_map(|s| match s {
                    Selected::Face(f) => Some(SimFace {
                        occurrence: None,
                        face: f.clone(),
                    }),
                    Selected::Edge(_) => None,
                })
                .collect(),
        }
    }

    /// Selects `faces` in the model, so the viewport highlights them.
    fn select_sim_faces(&mut self, faces: &[SimFace]) {
        match self.asm.as_mut() {
            Some(asm) => {
                asm.selection = faces
                    .iter()
                    .filter_map(|f| Some(AsmSelected::Face(f.occurrence.clone()?, f.face.clone())))
                    .collect();
            }
            None => {
                self.selection = faces
                    .iter()
                    .filter(|f| f.occurrence.is_none())
                    .map(|f| Selected::Face(f.face.clone()))
                    .collect();
            }
        }
    }

    /// A fixture or load on the selected faces (the command bar's buttons).
    pub(crate) fn add_study_item(&mut self, kind: ItemKind) {
        if !self.simulation.started || !kind.fits(self.simulation.study.study_type) {
            return;
        }
        let faces = self.selected_sim_faces();
        if faces.is_empty() {
            self.status = if self.simulation.shown.is_some() {
                format!(
                    "{}: select the faces on the model, then click it again",
                    kind.base_name()
                )
            } else {
                format!(
                    "{}: select the faces first, then click it",
                    kind.base_name()
                )
            };
            // The plot (or the outcome) hides, so the model is there to pick on.
            self.simulation.show_plot = false;
            self.simulation.generative.show = false;
            return;
        }
        let count = faces.len();
        let index = self.simulation.study.add(kind, faces);
        if self.simulation.study.study_type == StudyType::Generative {
            crate::generative_ui::join_load_cases(&mut self.simulation.study, index);
        }
        self.simulation.selected = Some(index);
        // The plot (or the outcome) hides, so the model is there to pick on.
        self.simulation.show_plot = false;
        self.simulation.generative.show = false;
        if let Some(item) = self.simulation.study.items.get(index) {
            self.status = format!("{} on {count} face(s)", item.name);
        }
    }

    /// The study as the solvers take it, with the materials, or what is missing.
    fn solver_study(&self, surfaces: &Surfaces, mesh_only: bool) -> Result<SolveInput, String> {
        let study = &self.simulation.study;
        let study_type = study.study_type;
        let mut contacts = Vec::new();
        if surfaces.bodies.len() > 1 {
            for c in &study.contacts {
                let (a, b) = (surfaces.labels(&c.a), surfaces.labels(&c.b));
                if (a.is_empty() || b.is_empty()) && !mesh_only {
                    return Err(format!(
                        "{} is on no face of the model any more: find the contacts again, or delete it",
                        c.name
                    ));
                }
                contacts.push(oxyde_sim::Contact {
                    kind: c.kind,
                    a_labels: a,
                    b_labels: b,
                    offset: c.offset.max(0.0),
                });
            }
        }
        let mut fixtures = Vec::new();
        let mut loads = Vec::new();
        let mut thermal_loads = Vec::new();
        let mut internal = Vec::new();
        let mut preserved = Vec::new();
        // A topology study's weight would change with its shape: it takes none.
        let gravity = study.gravity && study_type.takes_gravity();
        for item in study.items.iter().filter(|i| i.kind.fits(study_type)) {
            let labels = surfaces.labels(&item.faces);
            if labels.is_empty() && !mesh_only {
                return Err(format!(
                    "{} is on no face of the model any more: select its faces again",
                    item.name
                ));
            }
            let thermal = |kind| oxyde_sim::ThermalLoad {
                kind,
                labels: labels.clone(),
            };
            match item.kind {
                ItemKind::Fixed => fixtures.push(oxyde_sim::Fixture {
                    kind: FixtureKind::Fixed,
                    labels,
                }),
                ItemKind::Roller => fixtures.push(oxyde_sim::Fixture {
                    kind: FixtureKind::Roller,
                    labels,
                }),
                ItemKind::Force { newtons, direction } => loads.push(oxyde_sim::Load {
                    kind: LoadKind::Force {
                        total: newtons,
                        direction: direction.vector(),
                    },
                    labels,
                }),
                ItemKind::Pressure { mpa } => loads.push(oxyde_sim::Load {
                    kind: LoadKind::Pressure { value: mpa },
                    labels,
                }),
                ItemKind::Temperature { celsius } => {
                    thermal_loads.push(thermal(ThermalLoadKind::Temperature { celsius }));
                }
                ItemKind::HeatPower { watts } => {
                    thermal_loads.push(thermal(ThermalLoadKind::HeatPower { watts }));
                }
                ItemKind::HeatFlux { flux } => {
                    thermal_loads.push(thermal(ThermalLoadKind::HeatFlux { flux }));
                }
                ItemKind::Convection {
                    coefficient,
                    ambient,
                } => thermal_loads.push(thermal(ThermalLoadKind::Convection {
                    coefficient,
                    ambient,
                })),
                ItemKind::Radiation {
                    emissivity,
                    ambient,
                } => thermal_loads.push(thermal(ThermalLoadKind::Radiation {
                    emissivity,
                    ambient,
                })),
                ItemKind::InternalHeat { watts } => {
                    // The bodies the faces belong to.
                    let mut bodies: Vec<u32> = labels
                        .iter()
                        .filter_map(|&l| surfaces.face_body.get(l as usize).copied())
                        .collect();
                    bodies.sort_unstable();
                    bodies.dedup();
                    internal.push(oxyde_sim::InternalHeat { bodies, watts });
                }
                ItemKind::Preserve { depth } => preserved.push(oxyde_sim::Preserve {
                    labels,
                    depth: depth.max(0.0),
                }),
                // Generative studies' alone, which the Generative Design tab solves.
                ItemKind::Gravity { .. } | ItemKind::RemoteMass { .. } => {}
                ItemKind::InletVelocity { .. }
                | ItemKind::InletVolumeFlow { .. }
                | ItemKind::InletMassFlow { .. }
                | ItemKind::StaticPressure { .. }
                | ItemKind::IdealWall
                | ItemKind::Goal(_) => {}
            }
        }
        if !mesh_only {
            if study_type.takes_fixtures() && fixtures.is_empty() {
                return Err(
                    "The model is not held: select faces and add Fixed Geometry or a Roller/Slider"
                        .to_owned(),
                );
            }
            if study_type == StudyType::Static && loads.is_empty() && !gravity {
                return Err(
                    "Nothing loads the model: select faces and add a Force or a Pressure, or turn on Gravity"
                        .to_owned(),
                );
            }
            if study_type == StudyType::Topology && loads.is_empty() {
                return Err(
                    "Nothing loads the model: select faces and add a Force or a Pressure"
                        .to_owned(),
                );
            }
            if study_type.is_thermal() && thermal_loads.is_empty() && internal.is_empty() {
                return Err(
                    "Nothing heats or cools the model: select faces and add a Temperature, a Heat Power or Flux, a Convection or a Radiation"
                        .to_owned(),
                );
            }
        }
        let structural = oxyde_sim::Study {
            fixtures,
            loads,
            gravity: gravity.then_some([0.0, -9.81, 0.0]),
            contacts: contacts.clone(),
        };
        let transient = (study_type == StudyType::TransientThermal).then(|| oxyde_sim::Transient {
            duration: study.transient.duration,
            steps: study.transient.steps.max(1) as usize,
            initial: study.transient.initial_temperature,
            frames: TRANSIENT_FRAMES,
        });
        let thermal = ThermalStudy {
            loads: thermal_loads,
            internal,
            contacts,
            transient,
        };
        let elastic = || -> Result<Vec<Elastic>, String> {
            let mut out = Vec::with_capacity(surfaces.bodies.len());
            for body in &surfaces.bodies {
                match elastic_of(study, body) {
                    Some(e) if e.is_valid() => out.push(e),
                    _ if mesh_only => out.push(Elastic::new(1.0, 0.3, 0.0, 0.0)),
                    Some(_) => {
                        return Err(format!(
                            "The properties of {} are not usable: check them in Material",
                            body.name
                        ));
                    }
                    None => {
                        return Err(format!(
                            "{} has no known material: apply one (right-click › Material) or type its properties in Material",
                            body.name
                        ));
                    }
                }
                if gravity && !mesh_only && body.material.as_ref().is_none_or(|m| m.density <= 0.0)
                {
                    return Err(format!(
                        "Gravity needs a density: {} has none (apply a material)",
                        body.name
                    ));
                }
                // A body without mass does not vibrate.
                if study_type == StudyType::Frequency
                    && !mesh_only
                    && body.material.as_ref().is_none_or(|m| m.density <= 0.0)
                {
                    return Err(format!(
                        "A frequency study needs a density: {} has none (apply a material)",
                        body.name
                    ));
                }
            }
            Ok(out)
        };
        let conducting = || -> Result<Vec<ThermalMaterial>, String> {
            let mut out = Vec::with_capacity(surfaces.bodies.len());
            for body in &surfaces.bodies {
                match thermal_material_of(study, body) {
                    _ if mesh_only => out.push(ThermalMaterial::new(1.0, 1.0, 1.0)),
                    Some(t) if !t.is_valid() => {
                        return Err(format!(
                            "The thermal properties of {} are not usable: check them in Material",
                            body.name
                        ));
                    }
                    Some(t) if study_type == StudyType::TransientThermal && !t.stores_heat() => {
                        return Err(format!(
                            "{} needs a specific heat and a density for a transient study: type them in Material, or apply a material",
                            body.name
                        ));
                    }
                    Some(t) => out.push(t),
                    None => {
                        return Err(format!(
                            "{} has no known thermal conductivity: apply a material or type it in Material",
                            body.name
                        ));
                    }
                }
            }
            Ok(out)
        };
        Ok(match study_type {
            StudyType::Static => SolveInput::Static {
                study: structural,
                materials: elastic()?,
            },
            StudyType::Thermal | StudyType::TransientThermal => SolveInput::Thermal {
                study: thermal,
                materials: conducting()?,
            },
            StudyType::ThermalStress => SolveInput::ThermalStress {
                thermal,
                thermal_materials: conducting()?,
                study: structural,
                materials: elastic()?,
                reference: study.reference_temperature,
            },
            StudyType::Topology => SolveInput::Topology {
                study: TopologyStudy {
                    study: structural,
                    target: study.topology.target_mass,
                    member_size: study.topology.member_size,
                    preserved,
                    max_iterations: study.topology.iterations.max(1) as usize,
                    symmetry: study.topology.symmetry.contains(&true).then_some(
                        oxyde_sim::Symmetry {
                            point: study.topology.symmetry_point,
                            planes: study.topology.symmetry,
                        },
                    ),
                    pull: study.topology.pull.map(|direction| oxyde_sim::Pull {
                        direction,
                        parting: study.topology.parting,
                    }),
                    one_piece: study.topology.one_piece,
                },
                materials: elastic()?,
            },
            StudyType::Frequency => SolveInput::Frequency {
                study: FrequencyStudy {
                    fixtures: structural.fixtures,
                    contacts: structural.contacts,
                    modes: study.modes.clamp(1, oxyde_sim::MAX_MODES as u32) as usize,
                },
                materials: elastic()?,
            },
            StudyType::Generative => {
                return Err("A generative study is not solved in the Simulation tab".to_owned());
            }
            // Flow and cooling studies run OpenFOAM (`flow_ui.rs`), never these solvers.
            StudyType::Flow | StudyType::Cooling => {
                return Err(
                    "A flow study is solved by OpenFOAM, not meshed into tetrahedra".to_owned(),
                );
            }
        })
    }

    /// Run, Mesh or Find Contacts: meshes the model and, for Run, solves the study, on a
    /// thread.
    pub(crate) fn run_simulation(&mut self, mode: RunMode) {
        self.simulation.error = None;
        if self.simulation.study.study_type.is_flow() {
            if mode == RunMode::Run {
                self.run_flow();
            }
            return;
        }
        if let Err(e) = self.ensure_surfaces() {
            self.simulation.error = Some(e);
            return;
        }
        let Some(h) = self.simulation_element_size() else {
            self.simulation.error = Some("The element size is not a positive length".to_owned());
            return;
        };
        let mesh_only = mode != RunMode::Run;
        let Some(surfaces) = self.simulation.surfaces.as_ref() else {
            return;
        };
        if mode == RunMode::FindContacts && surfaces.bodies.len() < 2 {
            self.status = "Contacts are between bodies: this model has one".to_owned();
            return;
        }
        let solve = match self.solver_study(surfaces, mesh_only) {
            Ok(s) => s,
            Err(e) => {
                self.simulation.error = Some(e.clone());
                self.status = e;
                return;
            }
        };
        let several = surfaces.bodies.len() > 1;
        let sim_study = &self.simulation.study;
        // Contacts are looked for when asked, and once by Run itself when there are none.
        let detect = (several
            && (mode == RunMode::FindContacts
                || (mode == RunMode::Run
                    && sim_study.contacts.is_empty()
                    && !sim_study.contacts_found)))
            .then_some(sim_study.contact_tolerance.max(1e-6));
        let quadratic = sim_study.mesh.quality == ElementQuality::High;
        let use_netgen = sim_study.mesh.mesher == MesherChoice::Netgen;
        let input = ThreadInput {
            solids: surfaces.solids.clone(),
            netgen: Vec::new(),
            options: MeshOptions {
                element_size: h,
                quadratic,
                max_nodes: max_nodes(self.prefs.simulation.memory_budget_gb, quadratic),
                separate_bodies: true,
            },
            solve,
            names: surfaces.bodies.iter().map(|b| b.name.clone()).collect(),
            mode,
            detect,
            budget_gb: self.prefs.simulation.memory_budget_gb,
        };
        let study_type = sim_study.study_type;
        let faces = surfaces.faces.clone();
        let bodies: Vec<(String, String)> = surfaces
            .bodies
            .iter()
            .map(|b| (b.name.clone(), b.key.clone()))
            .collect();
        let sources = surfaces.sources.clone();
        let diagonal = surfaces.diagonal;
        let mut input = input;
        if use_netgen {
            input.netgen = self.netgen_inputs(&sources);
        }
        let fingerprint = if mode == RunMode::Run {
            self.simulation_fingerprint().ok()
        } else {
            None
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared {
            stage: "Meshing",
            ..Shared::default()
        }));
        let (c, s) = (cancel.clone(), shared.clone());
        let spawned = std::thread::Builder::new()
            .name("oxyde simulation".into())
            .spawn(move || {
                let result = oxyde_kernel::catch_panic(|| study_thread(input, &c, &s));
                let outcome = match result {
                    Ok(r) => r,
                    Err(message) => Err(format!("Internal error in the simulation: {message}")),
                };
                if let Ok(mut shared) = s.lock() {
                    shared.outcome = Some(outcome);
                }
            });
        if let Err(e) = spawned {
            self.simulation.error = Some(format!("The simulation thread could not start: {e}"));
            return;
        }
        self.simulation.job = Some(SimJob {
            cancel,
            shared,
            revision: self.document_revision(),
            assembly: self.asm.is_some(),
            started: Instant::now(),
            diagonal,
            mode,
            faces,
            bodies,
            fingerprint,
            study_type,
        });
        self.status = match mode {
            RunMode::Mesh => "Meshing…",
            RunMode::FindContacts => "Looking for contacts…",
            RunMode::Run if study_type == StudyType::Topology => "Optimizing the shape…",
            RunMode::Run if study_type == StudyType::Frequency => {
                "Finding the natural frequencies…"
            }
            RunMode::Run => "Running the study…",
        }
        .to_owned();
    }

    /// Copies of the solids, placed, for Netgen on the study's thread: the thread meshes
    /// and frees them, so the part can change meanwhile. Empty when a copy fails (the
    /// lattice mesher is used then).
    fn netgen_inputs(&mut self, sources: &[BodySource]) -> Vec<NetgenInput> {
        let mut out = Vec::new();
        for (body, source) in sources.iter().enumerate() {
            let transform = source
                .placement
                .as_ref()
                .map_or(Transform::IDENTITY, Placement::transform);
            let copy = match self.kernel.transform(source.shape, &transform) {
                Ok(c) => c,
                Err(_) => {
                    for i in &out {
                        let i: &NetgenInput = i;
                        self.kernel.release(i.shape);
                    }
                    return Vec::new();
                }
            };
            // The copy's faces, by their history, to the original's labels.
            let face_labels = copy
                .history
                .faces
                .iter()
                .map(|sources| {
                    sources
                        .iter()
                        .find_map(|s| match s {
                            FaceSource::InputFace { face, .. } => {
                                source.face_labels.get(*face as usize).copied()
                            }
                            _ => None,
                        })
                        .unwrap_or(NO_LABEL)
                })
                .collect();
            out.push(NetgenInput {
                shape: copy.shape,
                face_labels,
                body: body as u32,
            });
        }
        out
    }

    /// Results read from a file, shown as if just solved.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn show_loaded_results(
        &mut self,
        mesh: TetMesh,
        study_type: StudyType,
        results: Option<Results>,
        thermal: Option<ThermalResults>,
        topology: Option<TopologyResults>,
        frequency: Option<FrequencyResults>,
        faces: BTreeMap<u32, SimFace>,
        bodies: Vec<(String, String)>,
        fingerprint: Fingerprint,
        solver: &SolverInfo,
    ) {
        let report = MeshReport {
            element_size: 0.0,
            nodes: mesh.nodes.len(),
            elements: mesh.tets.len(),
            volume: mesh.volume(),
            model_volume: mesh.volume(),
            dropped: 0,
        };
        let diagonal = {
            let (lo, hi) = mesh.nodes.iter().fold(
                ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]),
                |(lo, hi), p| {
                    (
                        [lo[0].min(p[0]), lo[1].min(p[1]), lo[2].min(p[2])],
                        [hi[0].max(p[0]), hi[1].max(p[1]), hi[2].max(p[2])],
                    )
                },
            );
            let d = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2))
                .sqrt();
            if d.is_finite() { d } else { 1.0 }
        };
        let summary = results.as_ref().map(Results::summary);
        let thermal_summary = thermal.as_ref().map(ThermalResults::summary);
        self.restore_simulation_scene();
        self.simulation.frame = thermal
            .as_ref()
            .map_or(0, |t| t.frames.len().saturating_sub(1));
        self.simulation.playing = None;
        self.simulation.mode = 0;
        self.simulation.done = Some(Done {
            id: crate::document::next_revision(),
            revision: self.document_revision(),
            assembly: self.asm.is_some(),
            mesh,
            report,
            study_type,
            results,
            summary,
            thermal,
            thermal_summary,
            topology,
            frequency,
            mesh_seconds: 0.0,
            solve_seconds: solver.seconds,
            diagonal,
            mesher: if solver.mesher == "netgen" {
                "netgen"
            } else {
                "lattice"
            },
            faces,
            bodies,
            fingerprint: Some(fingerprint),
            flow: None,
        });
        self.simulation.error = None;
        self.simulation.show_plot = true;
    }

    /// Manual Contact: the selected faces of one body against those of another.
    fn add_manual_contact(&mut self) {
        if let Err(e) = self.ensure_surfaces() {
            self.status = e;
            return;
        }
        let faces = self.selected_sim_faces();
        let Some(surfaces) = self.simulation.surfaces.as_ref() else {
            return;
        };
        let mut sides: Vec<(u32, Vec<SimFace>)> = Vec::new();
        for f in faces {
            let Some(label) = surfaces.faces.iter().position(|g| *g == f) else {
                continue;
            };
            let Some(&body) = surfaces.face_body.get(label) else {
                continue;
            };
            match sides.iter_mut().find(|(b, _)| *b == body) {
                Some((_, list)) => list.push(f),
                None => sides.push((body, vec![f])),
            }
        }
        if sides.len() != 2 {
            self.status =
                "Manual Contact: select faces on exactly two bodies, then click it".to_owned();
            self.simulation.show_plot = false;
            return;
        }
        let b = sides.pop().map(|(_, f)| f).unwrap_or_default();
        let a = sides.pop().map(|(_, f)| f).unwrap_or_default();
        let index = self.simulation.study.add_contact(ContactKind::Bonded, a, b);
        self.simulation.selected_contact = Some(index);
        self.simulation.show_plot = false;
        if let Some(c) = self.simulation.study.contacts.get(index) {
            self.status = format!("{} added (bonded): change its type in the panel", c.name);
        }
    }

    pub(crate) fn cancel_simulation(&mut self) {
        if self.simulation.job.take().is_some() {
            self.status = "Study cancelled".to_owned();
        }
    }

    /// Before the viewport is drawn: the plot (or the mesh) in place of the model, made
    /// again when the result or the plot settings changed.
    pub(crate) fn simulation_scene(&mut self) {
        if !self.plot_wanted() {
            return;
        }
        if self
            .simulation
            .done
            .as_ref()
            .is_some_and(|d| d.flow.is_some())
        {
            self.flow_scene();
            return;
        }
        let prefs = self.prefs.simulation.clone();
        let Some(done) = self.simulation.done.as_ref() else {
            return;
        };
        let scale = match (&done.results, &done.summary, &done.frequency) {
            (Some(_), Some(s), _) if prefs.deformed => match prefs.scale {
                ScaleChoice::True => 1.0,
                ScaleChoice::Custom => prefs.custom_scale.max(0.0),
                ScaleChoice::Automatic if s.max_displacement.0 > 0.0 => {
                    0.05 * done.diagonal / s.max_displacement.0
                }
                ScaleChoice::Automatic => 1.0,
            },
            // A mode's largest motion is 1: its true scale is 1 mm.
            (None, _, Some(_)) if prefs.deformed => match prefs.scale {
                ScaleChoice::True => 1.0,
                ScaleChoice::Custom => prefs.custom_scale.max(0.0),
                ScaleChoice::Automatic => 0.05 * done.diagonal,
            },
            _ => 0.0,
        };
        let plot = done.plot_for(prefs.plot);
        // The plot shown is the one chosen from now on, so the command bar lights it.
        if let Some(p) = plot
            && p != prefs.plot
        {
            self.prefs.simulation.plot = p;
            self.simulation.prefs_dirty = true;
        }
        if plot == Some(PlotChoice::Shape) {
            let key = PlotKey {
                id: done.id,
                plot,
                scale: 0.0,
                bands: 0,
                show_mesh: false,
                frame: None,
                cut: None,
                threshold: prefs.shape_threshold.clamp(0.01, 0.99),
                smooth: prefs.smooth_shape,
                ghost: prefs.show_original,
            };
            if self.simulation.shown == Some(key) {
                return;
            }
            let Some(t) = done.topology.as_ref() else {
                return;
            };
            let (scene, fraction) =
                shape_scene(&done.mesh, &t.density, key.threshold, key.smooth, key.ghost);
            let pieces = oxyde_sim::pieces(&done.mesh, &t.density, key.threshold);
            self.show_scene(scene);
            self.simulation.shown = Some(key);
            self.simulation.legend = None;
            self.simulation.shape_fraction = Some(fraction);
            self.simulation.shape_pieces = pieces;
            return;
        }
        // A transient study's temperatures at the time picked, or a frequency study's mode.
        let frame = done
            .thermal
            .as_ref()
            .filter(|t| !t.frames.is_empty() && plot == Some(PlotChoice::Temperature))
            .map(|t| self.simulation.frame.min(t.frames.len() - 1))
            .or_else(|| {
                done.frequency
                    .as_ref()
                    .filter(|f| !f.modes.is_empty() && plot == Some(PlotChoice::ModeShape))
                    .map(|f| self.simulation.mode.min(f.modes.len() - 1))
            });
        let key = PlotKey {
            id: done.id,
            plot,
            scale,
            bands: prefs.bands.clamp(2, 64),
            show_mesh: prefs.show_mesh || !done.solved(),
            frame,
            threshold: 0.0,
            smooth: false,
            ghost: false,
            cut: None,
        };
        if self.simulation.shown == Some(key) {
            return;
        }
        let mesh = &done.mesh;
        // What deforms the model: the displacements solved, or the mode shown.
        let motion: Option<&[[f64; 3]]> = match (&done.results, &done.frequency) {
            (Some(r), _) => Some(r.displacement.as_slice()),
            (None, Some(f)) => key
                .frame
                .and_then(|k| f.modes.get(k))
                .map(|m| m.shape.as_slice()),
            _ => None,
        };
        let positions: Vec<[f64; 3]> = match motion {
            Some(motion) if scale > 0.0 => mesh
                .nodes
                .iter()
                .zip(motion)
                .map(|(p, u)| {
                    [
                        p[0] + scale * u[0],
                        p[1] + scale * u[1],
                        p[2] + scale * u[2],
                    ]
                })
                .collect(),
            _ => mesh.nodes.clone(),
        };
        let (groups, colors, legend) = match key.plot {
            Some(plot) => {
                let values = done.values(plot, key.frame);
                let finite = || values.iter().copied().filter(|v| v.is_finite());
                let lo = finite().fold(f64::INFINITY, f64::min);
                let hi = finite().fold(f64::NEG_INFINITY, f64::max);
                let (lo, hi) = if lo.is_finite() { (lo, hi) } else { (0.0, 1.0) };
                // While a transient study plays, the colours keep their meaning: the
                // range is the whole history's.
                let (lo, hi) = match (key.frame, &done.thermal) {
                    (Some(_), Some(t)) => t
                        .frames
                        .iter()
                        .flat_map(|f| f.temperature.iter().copied())
                        .filter(|v| v.is_finite())
                        .fold((lo, hi), |(lo, hi), v| (lo.min(v), hi.max(v))),
                    _ => (lo, hi),
                };
                let reversed = plot == PlotChoice::FactorOfSafety;
                // Factors of safety far above the lowest say nothing more: SolidWorks too
                // shows them up to a cap.
                let hi = if reversed {
                    hi.min((10.0f64).max(lo * 2.0))
                } else {
                    hi
                };
                let groups =
                    oxyde_sim::bands(mesh, &positions, &values, (lo, hi), key.bands as usize);
                let colors: Vec<[u8; 3]> = groups
                    .iter()
                    .map(|g| {
                        let t = (f64::from(g.band) + 0.5) / f64::from(key.bands);
                        oxyde_sim::rainbow(if reversed { 1.0 - t } else { t })
                    })
                    .collect();
                let marker_node = match (plot, done.summary) {
                    (PlotChoice::Displacement, Some(s)) => Some((s.max_displacement.1, "Max")),
                    (PlotChoice::FactorOfSafety, Some(s)) => {
                        s.min_factor_of_safety.map(|(_, n)| (n, "Min"))
                    }
                    (PlotChoice::VonMises, Some(s)) => Some((s.max_von_mises.1, "Max")),
                    (
                        PlotChoice::FirstPrincipal
                        | PlotChoice::ContactPressure
                        | PlotChoice::Temperature
                        | PlotChoice::HeatFlux
                        | PlotChoice::ModeShape,
                        _,
                    ) => values
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| v.is_finite())
                        .max_by(|a, b| a.1.total_cmp(b.1))
                        .map(|(i, _)| (i as u32, "Max")),
                    _ => None,
                };
                let marker = marker_node.and_then(|(n, word)| {
                    let p = *positions.get(n as usize)?;
                    let v = *values.get(n as usize)?;
                    Some((p, format!("{word}: {}", format_value(v, plot.unit()))))
                });
                let title = match (key.frame, &done.thermal) {
                    (Some(k), _) if plot == PlotChoice::ModeShape => {
                        let hz = done
                            .frequency
                            .as_ref()
                            .and_then(|f| f.modes.get(k))
                            .map_or(0.0, |m| m.frequency);
                        format!("Mode {}: {}", k + 1, format_value(hz, "Hz"))
                    }
                    (Some(f), Some(t)) => format!(
                        "{} at {}",
                        plot.label(),
                        format_value(t.frames.get(f).map_or(0.0, |f| f.time), "s")
                    ),
                    _ => plot.label().to_owned(),
                };
                let legend = Legend {
                    title,
                    unit: plot.unit(),
                    range: (lo, hi),
                    bands: key.bands,
                    reversed,
                    scale,
                    marker,
                };
                (groups, colors, Some(legend))
            }
            _ => {
                // The mesh alone, in the model's colour.
                let zero = vec![0.0; mesh.nodes.len()];
                let groups = oxyde_sim::bands(mesh, &positions, &zero, (0.0, 0.0), 1);
                let colors = vec![[200, 205, 212]; groups.len()];
                (groups, colors, None)
            }
        };
        let kernel_mesh = Mesh {
            faces: groups
                .into_iter()
                .map(|g| FaceMesh {
                    positions: g.positions,
                    triangles: g.triangles,
                })
                .collect(),
            edges: if key.show_mesh {
                oxyde_sim::mesh_edges(mesh, &positions)
            } else {
                Vec::new()
            },
        };
        let materials: Vec<RenderMaterial> = colors
            .into_iter()
            .map(|c| RenderMaterial {
                color: Some(c),
                ..RenderMaterial::default()
            })
            .collect();
        let mut scene = Scene::from_mesh(&kernel_mesh);
        scene.set_materials(materials, Vec::new());
        self.show_scene(scene);
        self.simulation.shown = Some(key);
        self.simulation.legend = legend;
        self.simulation.shape_fraction = None;
        self.simulation.shape_pieces.clear();
    }

    /// `scene` in the viewport in place of the model's, until the plot hides.
    pub(crate) fn show_scene(&mut self, scene: Scene) {
        // Looked at, not picked: the faces are the plot's, not the model's.
        self.viewport.pickable = Some((0, 0));
        self.viewport.set_scene(scene, false);
        // The usual scene is not rebuilt over the plot while the document stays as it is.
        match self.asm.as_ref() {
            Some(asm) => self.asm_scene = Some(asm.revision),
            None => self.scene_revision = Some(self.scene_key()),
        }
    }

    /// Export STL: the topology study's shape as the panel sets it (threshold, smoothing),
    /// to a binary STL file.
    fn export_shape(&mut self) {
        let prefs = &self.prefs.simulation;
        let shape = self.simulation.done.as_ref().and_then(|d| {
            let t = d.topology.as_ref()?;
            Some(oxyde_sim::shape(
                &d.mesh,
                &t.density,
                prefs.shape_threshold.clamp(0.01, 0.99),
                if prefs.smooth_shape {
                    SHAPE_SMOOTHING
                } else {
                    0
                },
            ))
        });
        let Some(shape) = shape else {
            self.status = "Nothing to export: run the topology study first".to_owned();
            return;
        };
        if shape.triangles.is_empty() {
            self.status = "Nothing is denser than the threshold: lower it in the panel".to_owned();
            return;
        }
        let name = format!("{} shape", self.simulation.study.name);
        let folder = match &self.asm {
            Some(asm) => asm.path.clone(),
            None => self.doc.path.clone(),
        };
        let mut dialog = rfd::FileDialog::new()
            .add_filter("STL", &["stl"])
            .set_file_name(format!("{name}.stl"));
        if let Some(dir) = folder.as_deref().and_then(std::path::Path::parent) {
            dialog = dialog.set_directory(dir);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        if !crate::assembly_ui::has_extension(&path, "stl") {
            path.set_extension("stl");
        }
        let mesh = Mesh {
            faces: vec![FaceMesh {
                positions: shape
                    .positions
                    .iter()
                    .map(|p| p.map(|v| v as f32))
                    .collect(),
                triangles: shape.triangles,
            }],
            edges: Vec::new(),
        };
        let triangles = oxyde_format::stl::triangles(&mesh, 1.0);
        self.status = match oxyde_format::stl::save(&path, &name, &triangles, true) {
            Ok(()) => format!(
                "Exported the shape ({} triangles) to {}",
                triangles.len(),
                path.display()
            ),
            Err(e) => format!("Could not write {}: {e}", path.display()),
        };
    }

    /// After the viewport is drawn: the fixtures and loads as symbols on their faces
    /// while setting up, or the plot's legend; the progress in the corner.
    pub(crate) fn simulation_over_viewport(&self, ui: &egui::Ui, output: &ViewportOutput) {
        if !self.simulation_active() {
            return;
        }
        let painter = ui.painter_at(output.rect);
        if self.simulation.shown.is_some() {
            self.paint_trajectories(&painter, output);
            if let Some(legend) = &self.simulation.legend {
                self.paint_legend(&painter, output, legend);
            }
        } else {
            self.paint_study_symbols(&painter, output);
        }
        self.paint_simulation_hud(&painter, output.rect);
    }

    /// Where a study face is in the world: its centre and, for a flat face, its outward
    /// normal.
    fn sim_face_place(&self, f: &SimFace) -> Option<(Point3, Option<Vec3>)> {
        let info = |body: &oxyde_model::Body| -> Option<(Point3, Option<Vec3>)> {
            let i = body.naming.find_face(&f.face)?;
            let face = body.topology.faces.get(i as usize)?;
            Some((face.centroid, face.normal))
        };
        match (&self.asm, &f.occurrence) {
            (None, None) => info(self.doc.part.body()?),
            (Some(asm), Some(o)) => {
                let (_, placement, loaded) = asm
                    .leaves(&asm.assembly)
                    .into_iter()
                    .find(|(leaf, _, _)| leaf == o)?;
                let (c, n) = info(loaded.part.body()?)?;
                Some((
                    placement.apply_point(c),
                    n.map(|n| placement.apply_vector(n)),
                ))
            }
            _ => None,
        }
    }

    fn paint_study_symbols(&self, painter: &egui::Painter, output: &ViewportOutput) {
        let to_screen = |p: Point3| self.viewport.to_screen(p, output);
        // The surfaces' size, not the kernel's exact bounds, which are slow to ask for
        // every frame.
        let reach = self
            .simulation
            .surfaces
            .as_ref()
            .map_or(100.0, |s| s.diagonal)
            .max(1e-6)
            * 0.05;
        // A screen direction of `length` points for the world direction `d` at `c`.
        let screen_dir = |c: Point3, d: Vec3, length: f32| -> Option<egui::Vec2> {
            let a = to_screen(c);
            let b = to_screen(Point3::new(
                c.x + d.x * reach,
                c.y + d.y * reach,
                c.z + d.z * reach,
            ));
            let v = b - a;
            (v.length() > 1e-3).then(|| v.normalized() * length)
        };
        let fixture_color = egui::Color32::from_rgb(40, 170, 70);
        let load_color = egui::Color32::from_rgb(160, 60, 200);
        let heat_color = egui::Color32::from_rgb(235, 120, 25);
        let hot_color = egui::Color32::from_rgb(215, 45, 35);
        let cool_color = egui::Color32::from_rgb(40, 125, 215);
        let keep_color = egui::Color32::from_rgb(25, 150, 150);
        let flow_color = egui::Color32::from_rgb(25, 140, 230);
        let goal_color = egui::Color32::from_rgb(232, 145, 40);
        if !self.simulation.started {
            return;
        }
        for (index, item) in self.simulation.study.items.iter().enumerate() {
            let selected = self.simulation.selected == Some(index);
            let width = if selected { 2.6_f32 } else { 1.8_f32 };
            for face in &item.faces {
                let Some((c, normal)) = self.sim_face_place(face) else {
                    continue;
                };
                let tip = to_screen(c);
                match &item.kind {
                    ItemKind::Fixed | ItemKind::Roller => {
                        // Arrows pointing at the face from outside, as SolidWorks
                        // draws a fixture, or a small anchor where the face is curved.
                        let back = normal.and_then(|n| screen_dir(c, n, 22.0));
                        let stroke = egui::Stroke::new(width, fixture_color);
                        match back {
                            Some(v) => {
                                let side = egui::vec2(-v.y, v.x) * 0.35;
                                for offset in [-1.0f32, 0.0, 1.0] {
                                    let t = tip + side * offset;
                                    paint_arrow(painter, t + v, t, stroke);
                                }
                            }
                            None => {
                                painter.circle_stroke(tip, 6.0, stroke);
                            }
                        }
                        if item.kind == ItemKind::Roller {
                            painter.circle_stroke(tip, 4.0, stroke);
                        }
                    }
                    ItemKind::Force { newtons, direction } => {
                        let stroke = egui::Stroke::new(width, load_color);
                        let push = match direction.vector() {
                            Some(d) => {
                                let s = newtons.signum();
                                Some(Vec3::new(d[0] * s, d[1] * s, d[2] * s))
                            }
                            // Into the face: drawn from outside.
                            None => normal.map(|n| Vec3::new(-n.x, -n.y, -n.z)),
                        };
                        match push.and_then(|d| screen_dir(c, d, 36.0)) {
                            Some(v) => paint_arrow(painter, tip - v, tip, stroke),
                            None => {
                                painter.circle_filled(tip, 4.0, load_color);
                            }
                        }
                    }
                    ItemKind::Pressure { mpa } => {
                        let stroke = egui::Stroke::new(width, load_color);
                        let push = normal.map(|n| {
                            let s = mpa.signum();
                            Vec3::new(-n.x * s, -n.y * s, -n.z * s)
                        });
                        match push.and_then(|d| screen_dir(c, d, 26.0)) {
                            Some(v) => {
                                let side = egui::vec2(-v.y, v.x) * 0.5;
                                for offset in [-1.0f32, 0.0, 1.0] {
                                    let t = tip + side * offset;
                                    paint_arrow(painter, t - v, t, stroke);
                                }
                            }
                            None => {
                                painter.circle_filled(tip, 4.0, load_color);
                            }
                        }
                    }
                    ItemKind::Temperature { .. } => {
                        // A thermometer's bulb on the face.
                        painter.circle_filled(tip, 5.0, hot_color);
                        painter.circle_stroke(tip, 7.5, egui::Stroke::new(width, hot_color));
                    }
                    ItemKind::HeatPower { watts: q } | ItemKind::HeatFlux { flux: q } => {
                        // Arrows into the face (out of it when heat is taken away).
                        let stroke = egui::Stroke::new(width, heat_color);
                        let into = normal.map(|n| {
                            let s = q.signum();
                            Vec3::new(-n.x * s, -n.y * s, -n.z * s)
                        });
                        match into.and_then(|d| screen_dir(c, d, 26.0)) {
                            Some(v) => {
                                let side = egui::vec2(-v.y, v.x) * 0.5;
                                for offset in [-1.0f32, 1.0] {
                                    let t = tip + side * offset;
                                    paint_arrow(painter, t - v, t, stroke);
                                }
                            }
                            None => {
                                painter.circle_filled(tip, 4.0, heat_color);
                            }
                        }
                    }
                    ItemKind::Convection { .. } | ItemKind::Radiation { .. } => {
                        // Arrows leaving the face, blue for a fluid, orange for radiation.
                        let color = if matches!(item.kind, ItemKind::Convection { .. }) {
                            cool_color
                        } else {
                            heat_color
                        };
                        let stroke = egui::Stroke::new(width, color);
                        match normal.and_then(|n| screen_dir(c, n, 26.0)) {
                            Some(v) => {
                                let side = egui::vec2(-v.y, v.x) * 0.5;
                                for offset in [-1.0f32, 0.0, 1.0] {
                                    let t = tip + side * offset;
                                    paint_arrow(painter, t, t + v, stroke);
                                }
                            }
                            None => {
                                painter.circle_stroke(tip, 6.0, stroke);
                            }
                        }
                    }
                    ItemKind::InternalHeat { .. } => {
                        painter.circle_filled(tip, 6.0, heat_color);
                        painter.circle_filled(tip, 2.5, egui::Color32::from_rgb(255, 225, 120));
                    }
                    ItemKind::Preserve { .. } => {
                        // A teal square on the face: its material stays.
                        let square = egui::Rect::from_center_size(tip, egui::vec2(11.0, 11.0));
                        painter.rect_filled(square, 2.0, keep_color);
                        painter.rect_stroke(
                            square,
                            2.0,
                            egui::Stroke::new(width * 0.7, egui::Color32::WHITE),
                        );
                    }
                    ItemKind::RemoteMass { .. } => {
                        // A weight hung on the face: a ring round a dot.
                        painter.circle_filled(tip, 3.5, load_color);
                        painter.circle_stroke(tip, 7.5, egui::Stroke::new(width, load_color));
                    }
                    // No faces: never reached.
                    ItemKind::Gravity { .. } => {}
                    ItemKind::InletVelocity { .. }
                    | ItemKind::InletVolumeFlow { .. }
                    | ItemKind::InletMassFlow { .. } => {
                        // The fluid coming in: arrows along the face's normal, which points
                        // out of the lid into the fluid.
                        let stroke = egui::Stroke::new(width, flow_color);
                        match normal.and_then(|n| screen_dir(c, n, 26.0)) {
                            Some(v) => {
                                let side = egui::vec2(-v.y, v.x) * 0.5;
                                for offset in [-1.0f32, 0.0, 1.0] {
                                    let t = tip + side * offset;
                                    paint_arrow(painter, t - v, t, stroke);
                                }
                            }
                            None => {
                                painter.circle_filled(tip, 4.0, flow_color);
                            }
                        }
                    }
                    ItemKind::StaticPressure { .. } => {
                        // The fluid going out: arrows leaving the face, out of the fluid.
                        let stroke = egui::Stroke::new(width, flow_color);
                        match normal.and_then(|n| screen_dir(c, n, 26.0)) {
                            Some(v) => {
                                let side = egui::vec2(-v.y, v.x) * 0.5;
                                for offset in [-1.0f32, 0.0, 1.0] {
                                    let t = tip + side * offset;
                                    paint_arrow(painter, t, t - v, stroke);
                                }
                            }
                            None => {
                                painter.circle_stroke(tip, 6.0, stroke);
                            }
                        }
                    }
                    ItemKind::IdealWall => {
                        let stroke = egui::Stroke::new(width, flow_color);
                        painter.circle_stroke(tip, 6.0, stroke);
                        painter.line_segment(
                            [tip - egui::vec2(9.0, 0.0), tip + egui::vec2(9.0, 0.0)],
                            stroke,
                        );
                    }
                    ItemKind::Goal(_) => {
                        // A flag on the face.
                        let stroke = egui::Stroke::new(width, goal_color);
                        let top = tip - egui::vec2(0.0, 20.0);
                        painter.line_segment([tip, top], stroke);
                        painter.add(egui::Shape::convex_polygon(
                            vec![top, top + egui::vec2(11.0, 3.5), top + egui::vec2(0.0, 7.0)],
                            goal_color,
                            egui::Stroke::NONE,
                        ));
                    }
                }
            }
        }
    }

    fn paint_legend(&self, painter: &egui::Painter, output: &ViewportOutput, legend: &Legend) {
        let rect = output.rect;
        let text = if crate::theme::view_dark() {
            egui::Color32::from_gray(230)
        } else {
            egui::Color32::from_gray(25)
        };
        let font = egui::FontId::proportional(11.5);
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 120.0, rect.top() + 48.0),
            egui::vec2(18.0, (rect.height() * 0.45).clamp(120.0, 320.0)),
        );
        let title = if legend.unit.is_empty() {
            legend.title.clone()
        } else {
            format!("{} ({})", legend.title, legend.unit)
        };
        painter.text(
            egui::pos2(bar.left(), bar.top() - 20.0),
            egui::Align2::LEFT_BOTTOM,
            title,
            egui::FontId::proportional(12.5),
            text,
        );
        // Highest values at the top, as SolidWorks draws its legend.
        let n = legend.bands.max(1);
        let h = bar.height() / n as f32;
        for k in 0..n {
            let t = (f64::from(k) + 0.5) / f64::from(n);
            let color = oxyde_sim::rainbow(if legend.reversed { 1.0 - t } else { t });
            let y = bar.bottom() - h * (k + 1) as f32;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(bar.left(), y),
                    egui::vec2(bar.width(), h + 0.5),
                ),
                0.0,
                egui::Color32::from_rgb(color[0], color[1], color[2]),
            );
        }
        painter.rect_stroke(bar, 0.0, egui::Stroke::new(1.0_f32, text));
        let (lo, hi) = legend.range;
        let ticks = 6;
        for k in 0..=ticks {
            let f = k as f64 / f64::from(ticks);
            let v = lo + (hi - lo) * f;
            let y = bar.bottom() - bar.height() * f as f32;
            painter.line_segment(
                [egui::pos2(bar.right(), y), egui::pos2(bar.right() + 4.0, y)],
                egui::Stroke::new(1.0_f32, text),
            );
            painter.text(
                egui::pos2(bar.right() + 7.0, y),
                egui::Align2::LEFT_CENTER,
                format_number(v),
                font.clone(),
                text,
            );
        }
        if legend.scale > 0.0 {
            painter.text(
                egui::pos2(bar.left(), bar.bottom() + 10.0),
                egui::Align2::LEFT_TOP,
                format!("Deformation scale: {}", format_number(legend.scale)),
                font.clone(),
                text,
            );
        }
        if let Some((p, label)) = &legend.marker {
            let at = self
                .viewport
                .to_screen(Point3::new(p[0], p[1], p[2]), output);
            if rect.contains(at) {
                painter.circle(
                    at,
                    4.0,
                    egui::Color32::WHITE,
                    egui::Stroke::new(1.5_f32, egui::Color32::BLACK),
                );
                let galley = painter.layout_no_wrap(label.clone(), font, egui::Color32::BLACK);
                let box_rect = egui::Rect::from_min_size(
                    at + egui::vec2(10.0, -galley.size().y - 10.0),
                    galley.size() + egui::vec2(8.0, 4.0),
                );
                painter.line_segment(
                    [at, box_rect.left_bottom()],
                    egui::Stroke::new(1.0_f32, egui::Color32::BLACK),
                );
                painter.rect(
                    box_rect,
                    2.0,
                    egui::Color32::from_rgb(255, 255, 225),
                    egui::Stroke::new(1.0_f32, egui::Color32::BLACK),
                );
                painter.galley(
                    box_rect.min + egui::vec2(4.0, 2.0),
                    galley,
                    egui::Color32::BLACK,
                );
            }
        }
    }

    fn paint_simulation_hud(&self, painter: &egui::Painter, rect: egui::Rect) {
        let s = &self.simulation;
        let text = if let Some(job) = &s.job {
            let (stage, fraction) = job
                .shared
                .lock()
                .map_or(("Working", 0.0), |sh| (sh.stage, sh.fraction));
            format!(
                "{stage}… {:.0} % ({:.0} s)",
                fraction * 100.0,
                job.started.elapsed().as_secs_f64()
            )
        } else if let Some(e) = &s.error {
            e.clone()
        } else if s
            .done
            .as_ref()
            .is_some_and(|d| d.revision != self.document_revision())
        {
            "The model changed: run the study again".to_owned()
        } else if s.shown.is_none() && s.done.is_some() {
            "Plot hidden: Show Results in the panel, or a plot button".to_owned()
        } else if !s.started {
            "New Study: pick what to study in the panel".to_owned()
        } else if let (Some(_), Some(fraction)) = (&s.shown, s.shape_fraction) {
            let pieces = if s.shape_pieces.len() > 1 {
                format!(", in {} pieces (see the panel)", s.shape_pieces.len())
            } else {
                String::new()
            };
            format!(
                "Cut at {:.0} % density: the shape holds {:.0} % of the volume{pieces}",
                self.prefs.simulation.shape_threshold * 100.0,
                fraction * 100.0
            )
        } else if s.done.is_none() {
            if s.study.study_type.is_flow() {
                crate::flow_ui::setup_hint(&s.study).to_owned()
            } else if s.study.study_type == StudyType::Topology {
                "Select faces, add fixtures, loads and regions to keep, then Run".to_owned()
            } else if s.study.study_type == StudyType::Frequency {
                "Select faces, add fixtures, then Run".to_owned()
            } else if s.study.study_type.is_structural() {
                "Select faces, add fixtures and loads, then Run".to_owned()
            } else {
                "Select faces, add temperatures, heat and cooling, then Run".to_owned()
            }
        } else {
            return;
        };
        let pos = rect.left_bottom() + egui::vec2(10.0, -10.0);
        let galley =
            painter.layout_no_wrap(text, egui::FontId::proportional(12.0), egui::Color32::WHITE);
        let bg = egui::Rect::from_min_size(
            pos - egui::vec2(6.0, galley.size().y + 4.0),
            galley.size() + egui::vec2(12.0, 8.0),
        );
        painter.rect_filled(bg, 4.0, egui::Color32::from_black_alpha(150));
        painter.galley(
            pos - egui::vec2(0.0, galley.size().y),
            galley,
            egui::Color32::WHITE,
        );
    }

    pub(crate) fn show_plot(&mut self, plot: Option<PlotChoice>) {
        if let Some(plot) = plot
            && self.prefs.simulation.plot != plot
        {
            self.prefs.simulation.plot = plot;
            self.simulation.prefs_dirty = true;
        }
        self.simulation.show_plot = true;
    }

    /// The Simulation tab's buttons: New Study, then those of the open study's type.
    pub(crate) fn simulation_tab(&mut self, ui: &mut egui::Ui) {
        use crate::icons::{Icon, command_button};
        let running = self.simulation.job.is_some();
        // New Study, a menu of the study types, as Fusion 360's first button.
        let response = command_button(ui, Icon::NewStudy, "New Study", !running, false)
            .on_hover_text("A new study of the model: static stress, thermal, transient thermal or thermal stress");
        crate::icons::dropdown_marker(ui, &response);
        let popup = ui.make_persistent_id("simulation_new_study");
        if response.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup));
        }
        let picked = egui::popup::popup_below_widget(
            ui,
            popup,
            &response,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(170.0);
                crate::simulation_studies::new_study_menu(ui)
            },
        )
        .flatten();
        if let Some(t) = picked {
            self.new_study(t);
        }
        if !self.simulation.started || self.generative_study_open() {
            return;
        }
        self.study_buttons(ui);
    }

    /// The buttons of the open study's type: its fixtures and loads, contacts, Mesh and
    /// Run, its plots.
    fn study_buttons(&mut self, ui: &mut egui::Ui) {
        use crate::icons::{Icon, command_button};
        let has_model = match &self.asm {
            Some(asm) => !asm.leaves(&asm.assembly).is_empty(),
            None => self.doc.part.body().is_some(),
        };
        let running = self.simulation.job.is_some();
        let study_type = self.simulation.study.study_type;
        let has_faces = !self.selected_sim_faces().is_empty();
        let enabled = has_model && !running;
        if study_type.is_flow() {
            self.flow_tab(ui, has_model, running, has_faces);
            return;
        }
        let item_button = |app: &mut OxydeApp,
                           ui: &mut egui::Ui,
                           icon: Icon,
                           label: &str,
                           kind: ItemKind,
                           hint: &str| {
            let response = command_button(ui, icon, label, enabled, false);
            let response = if has_faces {
                response.on_hover_text(hint)
            } else {
                response.on_hover_text(format!("{hint}\n(select faces first)"))
            };
            if response.clicked() {
                app.add_study_item(kind);
            }
        };
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        if study_type.takes_fixtures() {
            item_button(
                self,
                ui,
                Icon::Fixture,
                "Fixed Geometry",
                ItemKind::Fixed,
                "Hold the selected faces where they are: no motion at all",
            );
            item_button(
                self,
                ui,
                Icon::Roller,
                "Roller/Slider",
                ItemKind::Roller,
                "Let the selected faces slide along themselves, but not move off them",
            );
        }
        if study_type.is_structural() {
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(2.0);
            item_button(
                self,
                ui,
                Icon::Force,
                "Force",
                ItemKind::Force {
                    newtons: 100.0,
                    direction: LoadDirection::Normal,
                },
                "A force on the selected faces, spread by area (set its value in the panel)",
            );
            item_button(
                self,
                ui,
                Icon::Pressure,
                "Pressure",
                ItemKind::Pressure { mpa: 1.0 },
                "A pressure on the selected faces, pressing into them",
            );
            if study_type.takes_gravity()
                && command_button(
                    ui,
                    Icon::Gravity,
                    "Gravity",
                    enabled,
                    self.simulation.study.gravity,
                )
                .on_hover_text(
                    "The model's own weight, from its materials' densities, downwards (−Y)",
                )
                .clicked()
            {
                self.simulation.study.gravity = !self.simulation.study.gravity;
                self.simulation.show_plot = false;
            }
        }
        if study_type == StudyType::Topology {
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(2.0);
            item_button(
                self,
                ui,
                Icon::Preserve,
                "Preserve Region",
                ItemKind::Preserve { depth: 0.0 },
                "Keep the material of the selected faces, and as deep under them as set in the \
                 panel: mounting faces, bolt holes. The faces of fixtures and loads keep theirs \
                 anyway",
            );
        }
        if study_type.is_thermal() {
            if study_type.is_structural() {
                ui.add_space(2.0);
                ui.separator();
                ui.add_space(2.0);
            }
            let thermal: [(Icon, &str, ItemKind, &str); 6] = [
                (
                    Icon::Temperature,
                    "Temperature",
                    ItemKind::Temperature { celsius: 100.0 },
                    "Hold the selected faces at a temperature",
                ),
                (
                    Icon::HeatPower,
                    "Heat Power",
                    ItemKind::HeatPower { watts: 10.0 },
                    "Heat flowing in through the selected faces, a total in watts spread by area",
                ),
                (
                    Icon::HeatFlux,
                    "Heat Flux",
                    ItemKind::HeatFlux { flux: 1000.0 },
                    "Heat flowing in through the selected faces, in watts per square metre",
                ),
                (
                    Icon::Convection,
                    "Convection",
                    ItemKind::Convection {
                        coefficient: 25.0,
                        ambient: 20.0,
                    },
                    "The selected faces lose heat to air or a fluid (25 W/m²·K is still air, 100 to 1000 forced)",
                ),
                (
                    Icon::Radiation,
                    "Radiation",
                    ItemKind::Radiation {
                        emissivity: 0.9,
                        ambient: 20.0,
                    },
                    "The selected faces radiate heat to their surroundings",
                ),
                (
                    Icon::InternalHeat,
                    "Internal Heat",
                    ItemKind::InternalHeat { watts: 5.0 },
                    "Heat made inside the bodies of the selected faces (a motor's losses, a resistor)",
                ),
            ];
            for (icon, label, kind, hint) in thermal {
                item_button(self, ui, icon, label, kind, hint);
            }
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        if command_button(ui, Icon::Mate, "Find Contacts", enabled, false)
            .on_hover_text(
                "Find the faces of different bodies that touch and list them as contacts \
                     (bonded until changed in the panel), as Fusion 360 does",
            )
            .clicked()
        {
            self.run_simulation(RunMode::FindContacts);
        }
        if command_button(ui, Icon::Interference, "Manual Contact", enabled, false)
            .on_hover_text(
                "A contact between the selected faces: those of one body against those of another",
            )
            .clicked()
        {
            self.add_manual_contact();
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        if command_button(ui, Icon::Mesh, "Mesh", enabled, false)
            .on_hover_text("Fill the model with tetrahedra and show the mesh")
            .clicked()
        {
            self.run_simulation(RunMode::Mesh);
        }
        if running {
            if command_button(ui, Icon::Cancel, "Cancel", true, false)
                .on_hover_text("Stop the study")
                .clicked()
            {
                self.cancel_simulation();
            }
        } else if command_button(ui, Icon::RunStudy, "Run", has_model, false)
            .on_hover_text(match study_type {
                StudyType::Topology => {
                    "Mesh the model and find the stiffest shape that keeps the target mass"
                }
                StudyType::Frequency => "Mesh the model and find its natural frequencies",
                _ => "Mesh the model and solve the study",
            })
            .clicked()
        {
            self.run_simulation(RunMode::Run);
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        let done = self
            .simulation
            .done
            .as_ref()
            .filter(|d| d.solved() && d.revision == self.document_revision());
        let plots = [
            (
                Icon::TemperaturePlot,
                "Temperature",
                PlotChoice::Temperature,
                "The temperature, °C",
            ),
            (
                Icon::HeatFluxPlot,
                "Heat Flux",
                PlotChoice::HeatFlux,
                "How much heat flows through each point, W/m²",
            ),
            (
                Icon::Stress,
                "Stress",
                PlotChoice::VonMises,
                "The von Mises stress",
            ),
            (
                Icon::Displacement,
                "Displacement",
                PlotChoice::Displacement,
                "How far each point moves",
            ),
            (
                Icon::Safety,
                "Factor of Safety",
                PlotChoice::FactorOfSafety,
                "The yield strength over the von Mises stress: below 1, the part yields",
            ),
            (
                Icon::Topology,
                "Shape",
                PlotChoice::Shape,
                "The material kept: the shape cut where the density is the panel's threshold",
            ),
            (
                Icon::Displacement,
                "Mode Shape",
                PlotChoice::ModeShape,
                "How the model vibrates at the mode picked in the panel",
            ),
        ];
        let mut show = None;
        for (icon, label, plot, hint) in plots {
            if !plot.fits(study_type) {
                continue;
            }
            let available = done.is_some_and(|d| d.has(plot));
            let active = available
                && self.simulation.show_plot
                && (self.prefs.simulation.plot == plot
                    || (plot == PlotChoice::VonMises
                        && self.prefs.simulation.plot == PlotChoice::FirstPrincipal));
            if command_button(ui, icon, label, available, active)
                .on_hover_text(hint)
                .clicked()
            {
                show = Some(plot);
            }
        }
        let can_export = done.is_some_and(|d| d.topology.is_some());
        if study_type == StudyType::Topology
            && command_button(ui, Icon::ExportMesh, "Export STL", can_export, false)
                .on_hover_text(
                    "Write the shape as shown to an STL file: to print it, or to model a part over it",
                )
                .clicked()
        {
            self.export_shape();
        }
        if let Some(plot) = show {
            self.show_plot(Some(plot));
        }
    }

    /// What a fixture's or load's row asked for: its faces selected, its faces replaced by
    /// the selection, or it deleted. The plot, or the outcome, hides: the model is there to
    /// pick on.
    pub(crate) fn study_item_action(&mut self, action: PanelAction) {
        self.simulation.show_plot = false;
        self.simulation.generative.show = false;
        match action {
            PanelAction::Select(i) => {
                self.simulation.selected = Some(i);
                let faces = self
                    .simulation
                    .study
                    .items
                    .get(i)
                    .map(|item| item.faces.clone())
                    .unwrap_or_default();
                self.select_sim_faces(&faces);
            }
            PanelAction::UseSelection(i) => {
                let faces = self.selected_sim_faces();
                if faces.is_empty() {
                    self.status = "Select the faces first".to_owned();
                } else if let Some(item) = self.simulation.study.items.get_mut(i) {
                    self.status = format!("{} now on {} face(s)", item.name, faces.len());
                    item.faces = faces;
                }
            }
            PanelAction::Delete(i) => {
                if i < self.simulation.study.items.len() {
                    let gone = self.simulation.study.items.remove(i);
                    crate::generative_ui::leave_load_cases(&mut self.simulation.study, &gone.name);
                }
                self.simulation.selected = None;
            }
            _ => {}
        }
    }

    /// The study, at the top of the left panel while the Simulation tab is open.
    pub(crate) fn simulation_panel(&mut self, ui: &mut egui::Ui) {
        // Made here (not when running) so the panel can show sizes and materials.
        let surface_error = if self.simulation_active() {
            self.ensure_surfaces().err()
        } else {
            None
        };
        let before = self.prefs.simulation.clone();
        let groups = self.material_groups();
        let material_names: Vec<String> = self
            .materials
            .materials()
            .iter()
            .map(|m| m.name.clone())
            .collect();
        let element_size = self.simulation_element_size();
        let mut action: Option<PanelAction> = None;
        let max_height = (ui.available_height() * 0.7).max(160.0);
        if !self.simulation.started || self.generative_study_open() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.new_study_chooser(ui);
            });
            ui.add_space(4.0);
            return;
        }
        let study_type = self.simulation.study.study_type;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.study_bar(ui);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.simulation.job.is_some() {
                        if ui.button("Cancel").clicked() {
                            action = Some(PanelAction::Cancel);
                        }
                    } else if ui.button("Run").clicked() {
                        action = Some(PanelAction::Run);
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                let study_name = self.simulation.file.as_ref().map_or_else(
                    || "not saved".to_owned(),
                    |f| {
                        f.path
                            .file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                    },
                );
                ui.label(egui::RichText::new(study_name).small().weak());
                if ui
                    .small_button("Open…")
                    .on_hover_text("Open a study (.oxysim) of this model")
                    .clicked()
                {
                    action = Some(PanelAction::OpenStudy);
                }
                if ui
                    .small_button("Save")
                    .on_hover_text("Save the study (.oxysim) next to the model; its results go beside it")
                    .clicked()
                {
                    action = Some(PanelAction::SaveStudy);
                }
                if ui
                    .small_button("Open results…")
                    .on_hover_text("Open results (.oxyres) of this study, if the model is unchanged")
                    .clicked()
                {
                    action = Some(PanelAction::OpenResults);
                }
                let solved = self.simulation.done.as_ref().is_some_and(Done::solved);
                if ui
                    .add_enabled(solved, egui::Button::new("Save results…").small())
                    .clicked()
                {
                    action = Some(PanelAction::SaveResults);
                }
            });
            if let Some(job) = &self.simulation.job {
                let (stage, fraction) = job
                    .shared
                    .lock()
                    .map_or(("Working", 0.0), |s| (s.stage, s.fraction));
                ui.add(
                    egui::ProgressBar::new(fraction as f32)
                        .text(format!("{stage}… {:.0} %", fraction * 100.0)),
                );
            }
            for e in self.simulation.error.iter().chain(&surface_error) {
                ui.label(
                    egui::RichText::new(e)
                        .small()
                        .color(egui::Color32::from_rgb(210, 60, 60)),
                );
            }
            if study_type.is_flow() {
                if study_type.is_cooling() {
                    // The bodies conduct heat: their materials' conductivities, or typed in.
                    let study = &mut self.simulation.study;
                    let unknown = groups.iter().any(|g| g.thermal.is_none());
                    egui::CollapsingHeader::new("Material")
                        .default_open(unknown)
                        .show(ui, |ui| {
                            if groups.is_empty() {
                                ui.label(egui::RichText::new("Nothing to study yet").weak());
                            }
                            for (i, g) in groups.iter().enumerate() {
                                if let Some(a) = material_rows(
                                    ui,
                                    study_type,
                                    &mut study.overrides,
                                    &mut study.thermal_overrides,
                                    g,
                                    i,
                                    &material_names,
                                ) {
                                    action = Some(a);
                                }
                            }
                        });
                }
                if let Some(a) = self.flow_panel(ui, max_height) {
                    action = Some(a);
                }
                return;
            }
            let p = &mut self.prefs.simulation;
            egui::ScrollArea::vertical()
                .id_salt("simulation_panel")
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let study = &mut self.simulation.study;
                    let selected = &mut self.simulation.selected;
                    egui::CollapsingHeader::new("Material")
                        .default_open(true)
                        .show(ui, |ui| {
                            if groups.is_empty() {
                                ui.label(egui::RichText::new("Nothing to study yet").weak());
                            }
                            for (i, g) in groups.iter().enumerate() {
                                if let Some(a) = material_rows(
                                    ui,
                                    study_type,
                                    &mut study.overrides,
                                    &mut study.thermal_overrides,
                                    g,
                                    i,
                                    &material_names,
                                ) {
                                    action = Some(a);
                                }
                            }
                        });
                    if study_type.is_thermal() {
                        egui::CollapsingHeader::new("Thermal loads")
                            .default_open(true)
                            .show(ui, |ui| {
                                let mut any = false;
                                for (i, item) in study.items.iter_mut().enumerate() {
                                    if !item.kind.is_thermal() {
                                        continue;
                                    }
                                    any = true;
                                    item_row(ui, i, item, selected, &mut action);
                                }
                                if !any {
                                    ui.label(
                                        egui::RichText::new(
                                            "Select faces, then Temperature, Heat Power, Heat Flux, Convection, Radiation or Internal Heat. Faces with none are insulated.",
                                        )
                                        .small()
                                        .weak(),
                                    );
                                }
                                if study_type == StudyType::TransientThermal {
                                    ui.separator();
                                    if transient_rows(ui, &mut study.transient) {
                                        action.get_or_insert(PanelAction::Edited);
                                    }
                                }
                                if study_type == StudyType::ThermalStress {
                                    ui.separator();
                                    ui.horizontal(|ui| {
                                        ui.label("Stress-free at");
                                        if ui
                                            .add(
                                                egui::DragValue::new(&mut study.reference_temperature)
                                                    .range(-273.15..=5000.0)
                                                    .speed(0.5)
                                                    .suffix(" °C"),
                                            )
                                            .on_hover_text(
                                                "The temperature at which the model has no stress: it expands from there",
                                            )
                                            .changed()
                                        {
                                            action.get_or_insert(PanelAction::Edited);
                                        }
                                    });
                                }
                            });
                    }
                    if study_type.takes_fixtures() {
                    egui::CollapsingHeader::new("Fixtures")
                        .default_open(true)
                        .show(ui, |ui| {
                            let mut any = false;
                            for (i, item) in study.items.iter_mut().enumerate() {
                                if !item.kind.is_fixture() {
                                    continue;
                                }
                                any = true;
                                item_row(ui, i, item, selected, &mut action);
                            }
                            if !any {
                                ui.label(
                                    egui::RichText::new(
                                        "Select faces, then Fixed Geometry or Roller/Slider",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                        });
                    if study_type == StudyType::Frequency {
                        egui::CollapsingHeader::new("Options")
                            .default_open(true)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label("Modes");
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut study.modes)
                                                .range(1..=oxyde_sim::MAX_MODES as u32)
                                                .speed(0.1),
                                        )
                                        .on_hover_text(
                                            "How many natural frequencies to find, the lowest first",
                                        )
                                        .changed()
                                    {
                                        action.get_or_insert(PanelAction::Edited);
                                    }
                                });
                                ui.label(
                                    egui::RichText::new(
                                        "No loads: the model vibrates freely, held by its fixtures alone",
                                    )
                                    .small()
                                    .weak(),
                                );
                            });
                    }
                    if study_type.is_structural() {
                    egui::CollapsingHeader::new("External loads")
                        .default_open(true)
                        .show(ui, |ui| {
                            let mut any = false;
                            for (i, item) in study.items.iter_mut().enumerate() {
                                if item.kind.is_fixture()
                                    || matches!(item.kind, ItemKind::Preserve { .. })
                                {
                                    continue;
                                }
                                any = true;
                                item_row(ui, i, item, selected, &mut action);
                            }
                            if study_type.takes_gravity()
                                && ui
                                    .checkbox(&mut study.gravity, "Gravity (9.81 m/s², −Y)")
                                    .on_hover_text(
                                        "The model's weight, from its materials' densities",
                                    )
                                    .changed()
                            {
                                action.get_or_insert(PanelAction::Edited);
                            }
                            if !any && !(study.gravity && study_type.takes_gravity()) {
                                let hint = if study_type == StudyType::ThermalStress {
                                    "Optional: the expansion loads the model. Select faces, then Force or Pressure"
                                } else {
                                    "Select faces, then Force or Pressure"
                                };
                                ui.label(egui::RichText::new(hint).small().weak());
                            }
                        });
                    }
                    }
                    if study_type == StudyType::Topology {
                        egui::CollapsingHeader::new("Target")
                            .default_open(true)
                            .show(ui, |ui| {
                                if topology_goal_rows(ui, &mut study.topology, element_size) {
                                    action.get_or_insert(PanelAction::Edited);
                                }
                            });
                        egui::CollapsingHeader::new("Preserved regions")
                            .default_open(true)
                            .show(ui, |ui| {
                                let mut any = false;
                                for (i, item) in study.items.iter_mut().enumerate() {
                                    if !matches!(item.kind, ItemKind::Preserve { .. }) {
                                        continue;
                                    }
                                    any = true;
                                    item_row(ui, i, item, selected, &mut action);
                                }
                                if !any {
                                    ui.label(
                                        egui::RichText::new(
                                            "The faces of fixtures and loads keep their material. \
                                             To keep more (mounting faces, bolt holes), select \
                                             faces, then Preserve Region",
                                        )
                                        .small()
                                        .weak(),
                                    );
                                }
                            });
                    }
                    let selected_contact = &mut self.simulation.selected_contact;
                    egui::CollapsingHeader::new("Contacts")
                        .default_open(true)
                        .show(ui, |ui| {
                            for (i, c) in study.contacts.iter_mut().enumerate() {
                                contact_row(ui, i, c, selected_contact, &mut action);
                            }
                            if study.contacts.is_empty() {
                                ui.label(
                                    egui::RichText::new(
                                        "Bodies touching are found on Run (or Find Contacts) and bonded; \
                                         change a contact's type here",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                            if study_type.is_thermal() && !study.contacts.is_empty() {
                                ui.label(
                                    egui::RichText::new(
                                        "Heat crosses every contact as if bonded, whatever its type",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                            if study_type == StudyType::Frequency && !study.contacts.is_empty() {
                                ui.label(
                                    egui::RichText::new(
                                        "Every contact stays closed while the model vibrates: bonded, or holding along its normal",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                            ui.horizontal(|ui| {
                                ui.label("Tolerance");
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut study.contact_tolerance)
                                            .range(0.0..=100.0)
                                            .speed(0.01)
                                            .suffix(" mm"),
                                    )
                                    .on_hover_text(
                                        "Faces of different bodies closer than this are found in contact",
                                    )
                                    .changed()
                                {
                                    action.get_or_insert(PanelAction::Edited);
                                }
                                if ui.small_button("Find contacts").clicked() {
                                    action = Some(PanelAction::FindContacts);
                                }
                            });
                        });
                    let m = &mut study.mesh;
                    egui::CollapsingHeader::new("Mesh")
                        .default_open(false)
                        .show(ui, |ui| {
                            egui::Grid::new("simulation_mesh")
                                .num_columns(2)
                                .spacing([10.0, 4.0])
                                .show(ui, |ui| {
                                    ui.label("Mesher");
                                    egui::ComboBox::from_id_salt("simulation_mesher")
                                        .selected_text(match m.mesher {
                                            MesherChoice::Netgen => "Netgen",
                                            MesherChoice::Lattice => "Lattice",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut m.mesher,
                                                MesherChoice::Netgen,
                                                "Netgen",
                                            )
                                            .on_hover_text(
                                                "Graded and true to the faces; the lattice mesher is used when Oxyde was built without it",
                                            );
                                            ui.selectable_value(
                                                &mut m.mesher,
                                                MesherChoice::Lattice,
                                                "Lattice",
                                            )
                                            .on_hover_text(
                                                "Oxyde's own: elements of one size, edges bevelled",
                                            );
                                        });
                                    ui.end_row();
                                    ui.label("Quality");
                                    egui::ComboBox::from_id_salt("simulation_quality")
                                        .selected_text(match m.quality {
                                            ElementQuality::High => "High (10 nodes)",
                                            ElementQuality::Draft => "Draft (4 nodes)",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut m.quality,
                                                ElementQuality::High,
                                                "High (10 nodes)",
                                            )
                                            .on_hover_text("Quadratic elements: right in bending");
                                            ui.selectable_value(
                                                &mut m.quality,
                                                ElementQuality::Draft,
                                                "Draft (4 nodes)",
                                            )
                                            .on_hover_text(
                                                "Linear elements: quick, far too stiff in bending",
                                            );
                                        });
                                    ui.end_row();
                                    ui.label("Density");
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new("Coarse").small());
                                        // Moving the slider gives the size back to it.
                                        if ui
                                            .add(
                                                egui::Slider::new(&mut m.density, 0.0..=1.0)
                                                    .show_value(false),
                                            )
                                            .changed()
                                        {
                                            m.custom_size = false;
                                        }
                                        ui.label(egui::RichText::new("Fine").small());
                                    });
                                    ui.end_row();
                                    // The size in use, from the slider or typed; typing one
                                    // goes finer (or coarser) than the slider reaches.
                                    ui.label("Max element size");
                                    ui.horizontal(|ui| {
                                        let mut size = element_size.unwrap_or(m.element_size);
                                        let speed = size * 0.01;
                                        let typed = ui
                                            .add(
                                                egui::DragValue::new(&mut size)
                                                    .range(0.01..=1000.0)
                                                    .speed(speed)
                                                    .max_decimals(3)
                                                    .suffix(" mm"),
                                            )
                                            .on_hover_text(
                                                "The longest edge an element may have. Netgen \
                                                 makes smaller ones near small features and \
                                                 curved faces. Type a value to go finer than \
                                                 the slider.",
                                            )
                                            .changed();
                                        if typed {
                                            m.custom_size = true;
                                            m.element_size = size;
                                        }
                                        if m.custom_size {
                                            if ui
                                                .small_button("Automatic")
                                                .on_hover_text(
                                                    "Back to the size the density slider gives",
                                                )
                                                .clicked()
                                            {
                                                m.custom_size = false;
                                            }
                                        } else {
                                            ui.label(
                                                egui::RichText::new("from the slider")
                                                    .small()
                                                    .weak(),
                                            );
                                        }
                                    });
                                    ui.end_row();
                                });
                            if study_type == StudyType::Topology
                                && m.quality == ElementQuality::High
                            {
                                ui.label(
                                    egui::RichText::new(
                                        "Ten-node elements make every iteration about seven \
                                         times as long as four-node ones, which give a \
                                         topology study's shape as well: Draft is enough here",
                                    )
                                    .small()
                                    .color(egui::Color32::from_rgb(210, 140, 30)),
                                );
                            }
                            if ui.button("Mesh only").clicked() {
                                action = Some(PanelAction::Mesh);
                            }
                            if let Some(d) = &self.simulation.done {
                                let r = &d.report;
                                let fill = if r.model_volume > 0.0 {
                                    100.0 * r.volume / r.model_volume
                                } else {
                                    100.0
                                };
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} nodes, {} elements of {} mm, in {:.1} s",
                                        r.nodes,
                                        r.elements,
                                        format_number(r.element_size),
                                        d.mesh_seconds
                                    ))
                                    .small(),
                                );
                                let fill_text = format!("The mesh fills {fill:.1} % of the model");
                                if fill < 95.0 {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{fill_text}: thin features are lost. Use smaller elements."
                                        ))
                                        .small()
                                        .color(egui::Color32::from_rgb(210, 140, 30)),
                                    );
                                } else {
                                    ui.label(egui::RichText::new(fill_text).small().weak());
                                }
                            }
                        });
                    egui::CollapsingHeader::new("Results")
                        .default_open(true)
                        .show(ui, |ui| {
                            let done = self.simulation.done.as_ref();
                            if let Some(t) = done.and_then(|d| d.thermal_summary) {
                                ui.label(format!(
                                    "Temperature from {} to {}",
                                    format_value(t.min_temperature.0, "°C"),
                                    format_value(t.max_temperature.0, "°C")
                                ));
                                ui.label(format!(
                                    "Heat flux up to {}",
                                    format_value(t.max_heat_flux.0, "W/m²")
                                ));
                            }
                            // A transient study's time, and its playback.
                            if let Some(frames) = done
                                .and_then(|d| d.thermal.as_ref())
                                .map(|t| &t.frames)
                                .filter(|f| f.len() > 1)
                            {
                                let last = frames.len() - 1;
                                let frame = &mut self.simulation.frame;
                                *frame = (*frame).min(last);
                                ui.horizontal(|ui| {
                                    let playing = self.simulation.playing.is_some();
                                    if ui
                                        .small_button(if playing { "⏸" } else { "▶" })
                                        .on_hover_text(if playing { "Pause" } else { "Play the temperatures over time" })
                                        .clicked()
                                    {
                                        action = Some(PanelAction::Play(!playing));
                                    }
                                    let mut at = *frame;
                                    if ui
                                        .add(egui::Slider::new(&mut at, 0..=last).show_value(false))
                                        .on_hover_text("The time shown")
                                        .changed()
                                    {
                                        *frame = at;
                                        action = Some(PanelAction::Play(false));
                                    }
                                    if let Some(f) = frames.get(*frame) {
                                        ui.label(format_value(f.time, "s"));
                                    }
                                });
                            }
                            if let Some(t) = done.and_then(|d| d.topology.as_ref()) {
                                ui.label(format!(
                                    "{:.0} % of the mass kept (the target: {:.0} %)",
                                    t.mass_fraction() * 100.0,
                                    study.topology.target_mass * 100.0
                                ));
                                ui.label(format!(
                                    "{} times as flexible as the whole part",
                                    format_number(
                                        t.compliance() / t.initial_compliance.max(f64::MIN_POSITIVE)
                                    )
                                ))
                                .on_hover_text(
                                    "Its compliance (the work the loads do) over the whole \
                                     part's: what the material taken away costs in stiffness",
                                );
                                let settled = if t.converged {
                                    "settled".to_owned()
                                } else {
                                    "stopped at the limit: more iterations may still improve it"
                                        .to_owned()
                                };
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} iterations, {settled}",
                                        t.history.len()
                                    ))
                                    .small()
                                    .weak(),
                                );
                                if let Some(text) = t.joining.as_ref().and_then(joining_text) {
                                    ui.label(egui::RichText::new(text).small());
                                }
                                let warning = done.map_or_else(Vec::new, |d| {
                                    piece_warning(study, &d.faces, &self.simulation.shape_pieces)
                                });
                                for (k, line) in warning.iter().enumerate() {
                                    let text = egui::RichText::new(line).small();
                                    ui.label(if k == 0 {
                                        text.color(egui::Color32::from_rgb(210, 140, 30))
                                    } else {
                                        text
                                    });
                                }
                            }
                            if let Some(f) = done.and_then(|d| d.frequency.as_ref()) {
                                let shown = self.simulation.mode.min(f.modes.len().saturating_sub(1));
                                egui::Grid::new("frequency_modes")
                                    .num_columns(3)
                                    .spacing([10.0, 2.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        ui.label(egui::RichText::new("Mode").small().strong());
                                        ui.label(egui::RichText::new("Frequency").small().strong());
                                        ui.label(
                                            egui::RichText::new("Mass X / Y / Z")
                                                .small()
                                                .strong(),
                                        )
                                        .on_hover_text(
                                            "The share of the model's mass each mode moves along X, Y and Z (its effective mass, as SolidWorks lists the mass participation)",
                                        );
                                        ui.end_row();
                                        for (k, m) in f.modes.iter().enumerate() {
                                            if ui
                                                .selectable_label(k == shown, format!("{}", k + 1))
                                                .on_hover_text("Show this mode's shape")
                                                .clicked()
                                            {
                                                action = Some(PanelAction::PickMode(k));
                                            }
                                            ui.label(format_value(m.frequency, "Hz"));
                                            let p = m.participation;
                                            ui.label(format!(
                                                "{:.0} / {:.0} / {:.0} %",
                                                p[0] * 100.0,
                                                p[1] * 100.0,
                                                p[2] * 100.0
                                            ));
                                            ui.end_row();
                                        }
                                    });
                                let total = f.modes.iter().fold([0.0; 3], |[x, y, z], m| {
                                    let [px, py, pz] = m.participation;
                                    [x + px, y + py, z + pz]
                                });
                                ui.label(
                                    egui::RichText::new(format!(
                                        "The {} modes move {:.0} / {:.0} / {:.0} % of the mass along X / Y / Z; more modes reach more of it",
                                        f.modes.len(),
                                        total[0] * 100.0,
                                        total[1] * 100.0,
                                        total[2] * 100.0
                                    ))
                                    .small()
                                    .weak(),
                                );
                            }
                            match done.and_then(|d| d.summary.map(|s| (d, s))) {
                                Some((d, s)) => {
                                    ui.label(format!(
                                        "von Mises stress up to {}",
                                        format_value(s.max_von_mises.0, "MPa")
                                    ));
                                    ui.label(format!(
                                        "Displacement up to {}",
                                        format_value(s.max_displacement.0, "mm")
                                    ));
                                    match s.min_factor_of_safety {
                                        Some((f, _)) => {
                                            let text = format!(
                                                "Factor of safety at least {}",
                                                format_number(f)
                                            );
                                            if f < 1.0 {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{text}: the part yields"
                                                    ))
                                                    .color(egui::Color32::from_rgb(210, 60, 60)),
                                                );
                                            } else {
                                                ui.label(text);
                                            }
                                        }
                                        None => {
                                            ui.label(
                                                egui::RichText::new(
                                                    "Factor of safety unknown: no yield strength",
                                                )
                                                .weak(),
                                            );
                                        }
                                    }
                                    if let Some(r) = &d.results
                                        && !r.contacts.is_empty()
                                        && !r.contact_solve.converged
                                    {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "The contacts did not settle (a gap of {} mm is left): treat the results with care",
                                                format_number(r.contact_solve.violation)
                                            ))
                                            .small()
                                            .color(egui::Color32::from_rgb(210, 140, 30)),
                                        );
                                    }
                                }
                                None if done.is_some_and(Done::solved) => {}
                                None => {
                                    ui.label(egui::RichText::new("Not run yet").weak());
                                }
                            }
                            if let Some(d) = done.filter(|d| d.solved()) {
                                let iterations = d.results.as_ref().map_or(0, |r| r.iterations)
                                    + d.thermal.as_ref().map_or(0, |t| t.iterations)
                                    + d.topology.as_ref().map_or(0, |t| t.solver_iterations)
                                    + d.frequency.as_ref().map_or(0, |f| f.iterations);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Solved in {:.1} s, {iterations} iterations, {} mesh",
                                        d.solve_seconds,
                                        if d.mesher == "netgen" { "Netgen" } else { "lattice" }
                                    ))
                                    .small()
                                    .weak(),
                                );
                            }
                            let mut shown = self.simulation.show_plot;
                            if ui
                                .add_enabled(done.is_some(), egui::Checkbox::new(&mut shown, "Show results"))
                                .changed()
                            {
                                action = Some(PanelAction::ShowPlot(shown));
                            }
                            if study_type == StudyType::Topology {
                                egui::Grid::new("topology_plot")
                                    .num_columns(2)
                                    .spacing([10.0, 4.0])
                                    .show(ui, |ui| {
                                        ui.label("Threshold");
                                        ui.add(
                                            egui::Slider::new(&mut p.shape_threshold, 0.05..=0.95)
                                                .fixed_decimals(2),
                                        )
                                        .on_hover_text(
                                            "Where the shape is cut: material denser than this is \
                                             kept. Higher gives a lighter, thinner shape",
                                        );
                                        ui.end_row();
                                        ui.label("");
                                        ui.checkbox(&mut p.smooth_shape, "Smooth");
                                        ui.end_row();
                                        ui.label("");
                                        ui.checkbox(&mut p.show_original, "Show the whole part");
                                        ui.end_row();
                                    });
                                if let Some(f) = self.simulation.shape_fraction {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "The shape holds {:.0} % of the volume",
                                            f * 100.0
                                        ))
                                        .small(),
                                    );
                                }
                                if ui
                                    .add_enabled(
                                        done.is_some_and(|d| d.topology.is_some()),
                                        egui::Button::new("Export STL…"),
                                    )
                                    .on_hover_text(
                                        "Write the shape as shown to an STL file: to print it, \
                                         or to model a part over it",
                                    )
                                    .clicked()
                                {
                                    action = Some(PanelAction::ExportShape);
                                }
                            } else {
                            egui::Grid::new("simulation_plot")
                                .num_columns(2)
                                .spacing([10.0, 4.0])
                                .show(ui, |ui| {
                                    ui.label("Plot");
                                    // The plots of the study's type.
                                    let fits = |c: PlotChoice| c.fits(study_type);
                                    // The plot shown, else the one chosen if the study
                                    // has it, else its first.
                                    let shown = done
                                        .and_then(|d| d.plot_for(p.plot))
                                        .or_else(|| Some(p.plot).filter(|c| fits(*c)))
                                        .or_else(|| PlotChoice::ALL.into_iter().find(|c| fits(*c)))
                                        .unwrap_or(p.plot);
                                    egui::ComboBox::from_id_salt("simulation_plot_choice")
                                        .selected_text(shown.label())
                                        .show_ui(ui, |ui| {
                                            for c in PlotChoice::ALL.into_iter().filter(|c| fits(*c)) {
                                                ui.selectable_value(
                                                    &mut p.plot,
                                                    c,
                                                    c.label(),
                                                );
                                            }
                                        });
                                    ui.end_row();
                                    ui.label("Deformed");
                                    ui.horizontal(|ui| {
                                        ui.checkbox(&mut p.deformed, "");
                                        ui.add_enabled_ui(p.deformed, |ui| {
                                            egui::ComboBox::from_id_salt("simulation_scale")
                                                .selected_text(match p.scale {
                                                    ScaleChoice::Automatic => "Automatic",
                                                    ScaleChoice::True => "True scale",
                                                    ScaleChoice::Custom => "Scale",
                                                })
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(
                                                        &mut p.scale,
                                                        ScaleChoice::Automatic,
                                                        "Automatic",
                                                    );
                                                    ui.selectable_value(
                                                        &mut p.scale,
                                                        ScaleChoice::True,
                                                        "True scale",
                                                    );
                                                    ui.selectable_value(
                                                        &mut p.scale,
                                                        ScaleChoice::Custom,
                                                        "Scale",
                                                    );
                                                });
                                            if p.scale == ScaleChoice::Custom {
                                                ui.add(
                                                    egui::DragValue::new(&mut p.custom_scale)
                                                        .range(0.0..=1e6)
                                                        .speed(1.0),
                                                );
                                            }
                                        });
                                    });
                                    ui.end_row();
                                    ui.label("Bands");
                                    ui.add(egui::Slider::new(&mut p.bands, 4..=64));
                                    ui.end_row();
                                    ui.label("");
                                    ui.checkbox(&mut p.show_mesh, "Show the mesh");
                                    ui.end_row();
                                });
                            }
                        });
                });
        });
        ui.add_space(4.0);
        if self.prefs.simulation != before {
            self.simulation.prefs_dirty = true;
            // A new plot setting shows the plot.
            if self.prefs.simulation.plot != before.plot {
                self.simulation.show_plot = true;
            }
        }
        match action {
            Some(PanelAction::Run) => self.run_simulation(RunMode::Run),
            Some(PanelAction::Mesh) => self.run_simulation(RunMode::Mesh),
            Some(PanelAction::FindContacts) => self.run_simulation(RunMode::FindContacts),
            Some(PanelAction::OpenStudy) => self.open_study(),
            Some(PanelAction::SaveStudy) => {
                self.save_study(false);
            }
            Some(PanelAction::OpenResults) => self.open_results(),
            Some(PanelAction::ExportShape) => self.export_shape(),
            Some(PanelAction::SaveResults) => {
                self.save_results(true);
            }
            Some(PanelAction::SelectContact(i)) => {
                self.simulation.selected_contact = Some(i);
                self.simulation.show_plot = false;
                let faces = self
                    .simulation
                    .study
                    .contacts
                    .get(i)
                    .map(|c| c.a.iter().chain(&c.b).cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                self.select_sim_faces(&faces);
            }
            Some(PanelAction::DeleteContact(i)) => {
                if i < self.simulation.study.contacts.len() {
                    self.simulation.study.contacts.remove(i);
                }
                self.simulation.selected_contact = None;
                self.simulation.show_plot = false;
            }
            Some(PanelAction::Cancel) => self.cancel_simulation(),
            Some(
                a
                @ (PanelAction::Select(_) | PanelAction::UseSelection(_) | PanelAction::Delete(_)),
            ) => self.study_item_action(a),
            Some(PanelAction::ShowPlot(on)) => self.simulation.show_plot = on,
            Some(PanelAction::PickMode(k)) => {
                self.simulation.mode = k;
                self.show_plot(Some(PlotChoice::ModeShape));
            }
            Some(PanelAction::Play(on)) => {
                self.simulation.playing = on.then(Instant::now);
                if on {
                    // Playing shows the temperatures.
                    self.show_plot(Some(PlotChoice::Temperature));
                }
            }
            Some(PanelAction::ApplyMaterial(i)) => {
                let Some(g) = groups.get(i) else {
                    return;
                };
                let values = self.simulation.study.overrides.get(&g.key).copied();
                let thermal = self.simulation.study.thermal_overrides.get(&g.key).copied();
                if let Some(mut m) = self.doc.part.material().cloned()
                    && (values.is_some() || thermal.is_some())
                {
                    if values.is_some() {
                        m.mechanical = values;
                    }
                    if thermal.is_some() {
                        m.thermal = thermal;
                    }
                    // One undo step; the results go stale with the document's revision.
                    self.set_part_material(Some(m), false);
                    self.simulation.study.overrides.remove(&g.key);
                    self.simulation.study.thermal_overrides.remove(&g.key);
                }
            }
            Some(PanelAction::SetMaterial(i, choice)) => {
                let Some(g) = groups.get(i) else {
                    return;
                };
                let name = choice.and_then(|k| material_names.get(k).cloned());
                if name.is_none() && !g.has_material {
                    return;
                }
                // The model changes, so the plot goes stale and the panel asks for a run.
                self.simulation.show_plot = false;
                if self.asm.is_some() {
                    self.apply_material_to_paths(&g.paths, name);
                } else {
                    match name {
                        Some(name) => self.apply_material_to_part(&name),
                        None => self.set_part_material(None, false),
                    }
                }
            }
            Some(PanelAction::Edited) => self.simulation.show_plot = false,
            None => {}
        }
    }
}

/// What the panel asks for, done once it is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PanelAction {
    Run,
    Mesh,
    Cancel,
    /// An item picked: its faces selected.
    Select(usize),
    /// An item's faces replaced by the selection.
    UseSelection(usize),
    Delete(usize),
    ShowPlot(bool),
    /// Play (or pause) a transient study's temperatures.
    Play(bool),
    /// A frequency study's mode picked: its shape shown.
    PickMode(usize),
    /// A value of the study changed: the plot hides, it is out of date.
    Edited,
    /// The study's properties for a material group written to the part's material.
    ApplyMaterial(usize),
    /// A material group's parts given a material of the library (its index in the
    /// library's list), or none.
    SetMaterial(usize, Option<usize>),
    FindContacts,
    OpenStudy,
    SaveStudy,
    OpenResults,
    SaveResults,
    /// Export a topology study's shape as STL.
    ExportShape,
    /// A contact picked: its faces selected.
    SelectContact(usize),
    DeleteContact(usize),
}

/// One contact in the panel: its name (click to select its faces), its type, its gap.
fn contact_row(
    ui: &mut egui::Ui,
    index: usize,
    c: &mut StudyContact,
    selected: &mut Option<usize>,
    action: &mut Option<PanelAction>,
) {
    ui.horizontal(|ui| {
        if ui
            .selectable_label(*selected == Some(index), &c.name)
            .on_hover_text("Select its faces in the model")
            .clicked()
        {
            *action = Some(PanelAction::SelectContact(index));
        }
        let mut changed = false;
        egui::ComboBox::from_id_salt(("simulation_contact", index))
            .selected_text(StudyContact::kind_label(c.kind))
            .width(110.0)
            .show_ui(ui, |ui| {
                for (kind, hint) in StudyContact::KINDS {
                    changed |= ui
                        .selectable_value(&mut c.kind, kind, StudyContact::kind_label(kind))
                        .on_hover_text(hint)
                        .changed();
                }
            });
        if c.kind == ContactKind::OffsetBonded {
            changed |= ui
                .add(
                    egui::DragValue::new(&mut c.offset)
                        .range(0.0..=1000.0)
                        .speed(0.01)
                        .suffix(" mm"),
                )
                .on_hover_text("The gap between the faces")
                .changed();
        }
        if changed {
            action.get_or_insert(PanelAction::Edited);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("×").on_hover_text("Delete").clicked() {
                *action = Some(PanelAction::DeleteContact(index));
            }
            if c.automatic {
                ui.label(egui::RichText::new("found").small().weak())
                    .on_hover_text("Found by Oxyde between faces that touch");
            }
        });
    });
}

/// One fixture or load in the panel: its name (click to select its faces), its value,
/// its faces.
pub(crate) fn item_row(
    ui: &mut egui::Ui,
    index: usize,
    item: &mut StudyItem,
    selected: &mut Option<usize>,
    action: &mut Option<PanelAction>,
) {
    // Gravity acts on the whole design, on no faces.
    let on_faces = !matches!(item.kind, ItemKind::Gravity { .. });
    ui.horizontal(|ui| {
        let is_selected = *selected == Some(index);
        if ui
            .selectable_label(is_selected, &item.name)
            .on_hover_text(if on_faces {
                "Select its faces in the model"
            } else {
                "It weighs the whole design"
            })
            .clicked()
        {
            *action = Some(PanelAction::Select(index));
        }
        let what = if on_faces {
            format!("{} face(s)", item.faces.len())
        } else {
            "the material and the remote masses".to_owned()
        };
        ui.label(egui::RichText::new(what).small().weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("×").on_hover_text("Delete").clicked() {
                *action = Some(PanelAction::Delete(index));
            }
            if on_faces
                && ui
                    .small_button("Use selection")
                    .on_hover_text("Put it on the faces selected now instead")
                    .clicked()
            {
                *action = Some(PanelAction::UseSelection(index));
            }
        });
    });
    let mut changed = false;
    match &mut item.kind {
        ItemKind::Fixed | ItemKind::Roller => {}
        ItemKind::Force { newtons, direction } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(egui::DragValue::new(newtons).speed(1.0).suffix(" N"))
                    .on_hover_text("The total force, spread over the faces by area; negative pushes the other way")
                    .changed();
                egui::ComboBox::from_id_salt(("simulation_direction", index))
                    .selected_text(direction.label())
                    .show_ui(ui, |ui| {
                        for (d, label) in LoadDirection::ALL {
                            changed |= ui.selectable_value(direction, d, label).changed();
                        }
                    });
            });
        }
        ItemKind::Pressure { mpa } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(egui::DragValue::new(mpa).speed(0.01).suffix(" MPa"))
                    .on_hover_text("Normal to the faces, pressing into them; 1 MPa = 10 bar")
                    .changed();
            });
        }
        ItemKind::Temperature { celsius } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(
                        egui::DragValue::new(celsius)
                            .range(-273.15..=5000.0)
                            .speed(0.5)
                            .suffix(" °C"),
                    )
                    .on_hover_text("The faces are held at this temperature")
                    .changed();
            });
        }
        ItemKind::HeatPower { watts } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(egui::DragValue::new(watts).speed(0.1).suffix(" W"))
                    .on_hover_text(
                        "The heat flowing in, in all, spread over the faces by area; negative takes heat out",
                    )
                    .changed();
            });
        }
        ItemKind::HeatFlux { flux } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(egui::DragValue::new(flux).speed(10.0).suffix(" W/m²"))
                    .on_hover_text("The heat flowing in per area; negative takes heat out")
                    .changed();
            });
        }
        ItemKind::Convection {
            coefficient,
            ambient,
        } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(
                        egui::DragValue::new(coefficient)
                            .range(0.0..=1e6)
                            .speed(0.5)
                            .suffix(" W/m²·K"),
                    )
                    .on_hover_text(
                        "The film coefficient: about 5 to 25 in still air, 25 to 250 in moving air, 100 to 20 000 in water",
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(ambient)
                            .range(-273.15..=5000.0)
                            .speed(0.5)
                            .prefix("to ")
                            .suffix(" °C"),
                    )
                    .on_hover_text("The fluid's temperature")
                    .changed();
            });
        }
        ItemKind::Radiation {
            emissivity,
            ambient,
        } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(
                        egui::DragValue::new(emissivity)
                            .range(0.0..=1.0)
                            .speed(0.01)
                            .prefix("ε "),
                    )
                    .on_hover_text(
                        "The emissivity: about 0.05 for polished metal, 0.3 for oxidized aluminium, 0.9 for paint",
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(ambient)
                            .range(-273.15..=5000.0)
                            .speed(0.5)
                            .prefix("to ")
                            .suffix(" °C"),
                    )
                    .on_hover_text("The surroundings' temperature")
                    .changed();
            });
        }
        ItemKind::InternalHeat { watts } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(egui::DragValue::new(watts).speed(0.1).suffix(" W"))
                    .on_hover_text(
                        "The heat made inside the bodies of the faces, in all, spread through their volume",
                    )
                    .changed();
            });
        }
        ItemKind::Preserve { depth } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(
                        egui::DragValue::new(depth)
                            .range(0.0..=1e4)
                            .speed(0.1)
                            .prefix("depth ")
                            .suffix(" mm"),
                    )
                    .on_hover_text(
                        "How deep under the faces the material stays; 0 keeps only what is on \
                         them (an element, or a voxel, deep)",
                    )
                    .changed();
            });
        }
        ItemKind::Gravity { acceleration } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                for (value, axis) in acceleration.iter_mut().zip(["X ", "Y ", "Z "]) {
                    changed |= ui
                        .add(
                            egui::DragValue::new(value)
                                .speed(0.1)
                                .max_decimals(3)
                                .prefix(axis)
                                .suffix(" m/s²"),
                        )
                        .on_hover_text(
                            "The acceleration, world axes (Y up): 9.81 m/s² down is gravity; a \
                             shock of 10 g is ten times that",
                        )
                        .changed();
                }
            });
            let g = (acceleration[0].powi(2) + acceleration[1].powi(2) + acceleration[2].powi(2))
                .sqrt()
                / 9.81;
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new(format!("{g:.2} g")).small().weak());
                if ui
                    .small_button("1 g down")
                    .on_hover_text("Gravity: 9.81 m/s² along −Y")
                    .clicked()
                {
                    *acceleration = [0.0, -9.81, 0.0];
                    changed = true;
                }
            });
        }
        ItemKind::RemoteMass { kg, point } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(
                        egui::DragValue::new(kg)
                            .range(0.0..=1e6)
                            .speed(0.01)
                            .max_decimals(4)
                            .suffix(" kg"),
                    )
                    .on_hover_text(
                        "The component's mass: the load cases' gravity weighs it, and its weight \
                         reaches the faces as a force and a moment",
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new("at").small());
                for v in point.iter_mut() {
                    changed |= ui
                        .add(egui::DragValue::new(v).speed(0.5).suffix(" mm"))
                        .on_hover_text("Its centre of mass, world coordinates")
                        .changed();
                }
            });
        }
        ItemKind::InletVelocity { speed } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(
                        egui::DragValue::new(speed)
                            .range(0.0..=1e4)
                            .speed(0.01)
                            .max_decimals(4)
                            .suffix(" m/s"),
                    )
                    .on_hover_text(
                        "The fluid comes in through the faces at this speed, square to them",
                    )
                    .changed();
            });
        }
        ItemKind::InletVolumeFlow { rate } => {
            // Typed in litres a minute, kept in m³/s.
            let mut litres = *rate * 60_000.0;
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                if ui
                    .add(
                        egui::DragValue::new(&mut litres)
                            .range(0.0..=1e9)
                            .speed(0.1)
                            .max_decimals(4)
                            .suffix(" L/min"),
                    )
                    .on_hover_text(
                        "The volume coming in through the faces, shared between them by area (1 m³/h = 16.67 L/min)",
                    )
                    .changed()
                {
                    *rate = litres / 60_000.0;
                    changed = true;
                }
            });
        }
        ItemKind::InletMassFlow { rate } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(
                        egui::DragValue::new(rate)
                            .range(0.0..=1e6)
                            .speed(0.001)
                            .max_decimals(6)
                            .suffix(" kg/s"),
                    )
                    .on_hover_text(
                        "The mass coming in through the faces, shared between them by area",
                    )
                    .changed();
            });
        }
        ItemKind::StaticPressure { pascals } => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                changed |= ui
                    .add(egui::DragValue::new(pascals).speed(1.0).suffix(" Pa"))
                    .on_hover_text(
                        "The static pressure at the opening, above the surroundings' (0 is atmospheric); the fluid goes out through it",
                    )
                    .changed();
            });
        }
        ItemKind::IdealWall => {}
        ItemKind::Goal(kind) => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(crate::flow_ui::goal_hint(*kind))
                        .small()
                        .weak(),
                );
            });
        }
    }
    if changed {
        action.get_or_insert(PanelAction::Edited);
    }
}

/// A topology study's target mass, thinnest member, iterations and manufacturing controls
/// (symmetry planes, a pull direction); whether one changed. `element_size` is the mesh's,
/// to start a typed member size from.
fn topology_goal_rows(
    ui: &mut egui::Ui,
    goal: &mut TopologyGoal,
    element_size: Option<f64>,
) -> bool {
    let mut changed = false;
    egui::Grid::new("topology_goal")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            ui.label("Mass to keep");
            let mut percent = goal.target_mass * 100.0;
            if ui
                .add(
                    egui::DragValue::new(&mut percent)
                        .range(1.0..=99.0)
                        .speed(0.5)
                        .max_decimals(1)
                        .suffix(" %"),
                )
                .on_hover_text(
                    "The share of the model's mass the shape keeps; the rest is taken where the \
                     loads need it least",
                )
                .changed()
            {
                goal.target_mass = percent / 100.0;
                changed = true;
            }
            ui.end_row();
            ui.label("Thinnest member");
            ui.horizontal(|ui| match goal.member_size.as_mut() {
                Some(size) => {
                    changed |= ui
                        .add(
                            egui::DragValue::new(size)
                                .range(0.01..=1e4)
                                .speed(0.1)
                                .max_decimals(2)
                                .suffix(" mm"),
                        )
                        .on_hover_text(
                            "No bar or wall of the shape comes out thinner than about this; it \
                             cannot be finer than three elements",
                        )
                        .changed();
                    if ui
                        .small_button("Automatic")
                        .on_hover_text("As fine as the mesh allows: about three elements")
                        .clicked()
                    {
                        goal.member_size = None;
                        changed = true;
                    }
                }
                None => {
                    ui.label(egui::RichText::new("from the mesh").small().weak());
                    if ui
                        .small_button("Set")
                        .on_hover_text("Type the thinnest member wanted, for a sturdier shape")
                        .clicked()
                    {
                        let h = element_size.unwrap_or(1.0);
                        goal.member_size = Some((4.0 * h * 100.0).round() / 100.0);
                        changed = true;
                    }
                }
            });
            ui.end_row();
            ui.label("Iterations");
            changed |= ui
                .add(egui::DragValue::new(&mut goal.iterations).range(1..=1000))
                .on_hover_text(
                    "At most; each is a static solve. The optimization stops sooner once the \
                     shape has settled",
                )
                .changed();
            ui.end_row();
            ui.label("");
            changed |= ui
                .checkbox(&mut goal.one_piece, "Keep it in one piece")
                .on_hover_text(
                    "Once optimized, the pieces that hold a fixture, a load or a preserved \
                     region but that the loads do not reach are joined to the rest by a bar as \
                     thick as the thinnest member, where it adds least material; loose bits \
                     holding none of them are taken away. Without it, material nothing loads \
                     can be left floating",
                )
                .changed();
            ui.end_row();
            changed |= manufacturing_rows(ui, goal);
        });
    changed
}

/// The axes' names, for the planes' positions and the pull directions.
const AXES: [&str; 3] = ["X", "Y", "Z"];

/// A topology study's symmetry planes and pull direction, as rows of the Target grid;
/// whether one changed.
fn manufacturing_rows(ui: &mut egui::Ui, goal: &mut TopologyGoal) -> bool {
    let mut changed = false;
    ui.label("Symmetry");
    ui.horizontal(|ui| {
        for (on, name) in goal.symmetry.iter_mut().zip(["Right", "Top", "Front"]) {
            changed |= ui
                .checkbox(on, name)
                .on_hover_text(format!(
                    "Mirror the shape across the {name} plane, or the plane parallel to it at \
                     the position below, whatever the loads; the kept regions are mirrored too"
                ))
                .changed();
        }
    });
    ui.end_row();
    if goal.symmetry.contains(&true) {
        ui.label("Planes at");
        ui.horizontal(|ui| {
            let planes = goal.symmetry_point.iter_mut().zip(goal.symmetry).zip(AXES);
            for ((at, _), axis) in planes.filter(|((_, on), _)| *on) {
                changed |= ui
                    .add(
                        egui::DragValue::new(at)
                            .speed(0.1)
                            .max_decimals(3)
                            .prefix(format!("{axis} "))
                            .suffix(" mm"),
                    )
                    .on_hover_text("Where the plane crosses its axis; 0 for the standard plane")
                    .changed();
            }
        });
        ui.end_row();
    }
    ui.label("Pull direction");
    // The six directions along the axes; another one, read from a file, is shown as it is.
    let axis_of = |d: [f64; 3]| -> Option<(usize, f64)> {
        let mut along = d.iter().enumerate().filter(|(_, v)| **v != 0.0);
        match (along.next(), along.next()) {
            (Some((a, v)), None) => Some((a, v.signum())),
            _ => None,
        }
    };
    let label = |d: Option<[f64; 3]>| match d {
        None => "None".to_owned(),
        Some(d) => axis_of(d)
            .and_then(|(a, sign)| {
                let name = AXES.get(a)?;
                Some(format!(
                    "{}{name}",
                    if sign > 0.0 { "+" } else { "\u{2212}" }
                ))
            })
            .unwrap_or_else(|| "Custom".to_owned()),
    };
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("topology_pull")
            .selected_text(label(goal.pull))
            .show_ui(ui, |ui| {
                let along = (0..3).flat_map(|a| {
                    [1.0, -1.0]
                        .map(|sign| Some(std::array::from_fn(|b| if b == a { sign } else { 0.0 })))
                });
                for choice in std::iter::once(None).chain(along) {
                    let selected = goal.pull.and_then(axis_of) == choice.and_then(axis_of)
                        && goal.pull.is_some() == choice.is_some();
                    if ui.selectable_label(selected, label(choice)).clicked() && !selected {
                        goal.pull = choice;
                        changed = true;
                    }
                }
            })
            .response
            .on_hover_text(
                "The direction a mould is pulled off the shape along: going that way the shape \
                 never widens, so it has no undercut, as if it stood on its far side",
            );
        if goal.pull.is_some() {
            let mut both = goal.parting.is_some();
            if ui
                .checkbox(&mut both, "Both ways")
                .on_hover_text(
                    "A two-part mould: pulled off both ways from a parting plane across the \
                     direction, the shape never widening going away from it",
                )
                .changed()
            {
                goal.parting = both.then_some(goal.symmetry_point);
                changed = true;
            }
        }
    });
    ui.end_row();
    // A two-way pull along an axis: where its parting plane crosses it.
    let parting = goal
        .pull
        .and_then(axis_of)
        .zip(goal.parting.as_mut())
        .and_then(|((a, _), p)| Some((p.get_mut(a)?, AXES.get(a)?)));
    if let Some((at, axis)) = parting {
        ui.label("Parting plane at");
        changed |= ui
            .add(
                egui::DragValue::new(at)
                    .speed(0.1)
                    .max_decimals(3)
                    .prefix(format!("{axis} "))
                    .suffix(" mm"),
            )
            .on_hover_text(
                "Where the parting plane crosses the pull's axis; on a symmetry plane across \
                 the pull, it must be that plane",
            )
            .changed();
        ui.end_row();
    }
    changed
}

/// A transient study's time settings; whether one changed.
fn transient_rows(ui: &mut egui::Ui, t: &mut TransientSettings) -> bool {
    let mut changed = false;
    egui::Grid::new("simulation_transient")
        .num_columns(2)
        .spacing([10.0, 2.0])
        .show(ui, |ui| {
            ui.label("Duration");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut t.duration)
                        .range(1e-3..=1e9)
                        .speed(1.0)
                        .suffix(" s"),
                )
                .on_hover_text("How long the study follows the temperatures")
                .changed();
            ui.end_row();
            ui.label("Time steps");
            changed |= ui
                .add(egui::DragValue::new(&mut t.steps).range(1..=100_000).speed(1.0))
                .on_hover_text(
                    "More steps follow fast changes better; each is a solve (the results keep about a hundred of them)",
                )
                .changed();
            ui.end_row();
            ui.label("Starting at");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut t.initial_temperature)
                        .range(-273.15..=5000.0)
                        .speed(0.5)
                        .suffix(" °C"),
                )
                .on_hover_text("The whole model's temperature at the start")
                .changed();
            ui.end_row();
        });
    changed
}

/// Where properties come from, for the panel.
fn source_note(source: Option<PropsSource>) -> &'static str {
    match source {
        Some(PropsSource::Study) => "this study's",
        Some(PropsSource::Material) => "the material's",
        Some(PropsSource::Typical) => "typical values",
        None => "unknown",
    }
}

/// A material group's properties as a study of `study_type` uses them, mechanical and
/// thermal, and where they come from. Typing a value makes it the study's own. *Change*
/// gives the group's parts another material of the library (`names`), *Apply to the
/// material* keeps the typed values in the part's material; either is returned for the
/// group `index`.
#[allow(clippy::too_many_arguments)]
fn material_rows(
    ui: &mut egui::Ui,
    study_type: StudyType,
    overrides: &mut BTreeMap<String, Mechanical>,
    thermal_overrides: &mut BTreeMap<String, Thermal>,
    g: &MaterialGroup,
    index: usize,
    names: &[String],
) -> Option<PanelAction> {
    let mut action = None;
    let source = g.effective.map(|(_, s)| s);
    let thermal_source = g.thermal.map(|(_, s)| s);
    // A cooling study's bodies conduct heat as a thermal study's do.
    let thermal = study_type.is_thermal() || study_type.is_cooling();
    let typed = (study_type.takes_fixtures() && source == Some(PropsSource::Study))
        || (thermal && thermal_source == Some(PropsSource::Study));
    ui.horizontal(|ui| {
        let parts = if g.bodies.len() > 1 {
            format!("{} bodies: {}", g.bodies.len(), g.bodies.join(", "))
        } else {
            g.bodies.join(", ")
        };
        ui.label(egui::RichText::new(&g.label).strong())
            .on_hover_text(parts);
        ui.menu_button(if g.has_material { "Change" } else { "Choose" }, |ui| {
            if g.bodies.len() > 1 {
                ui.label(
                    egui::RichText::new(format!("For its {} bodies", g.bodies.len()))
                        .small()
                        .weak(),
                );
            }
            if let Some(choice) = crate::assembly_ui::material_menu(ui, names) {
                let index_of = |n: &String| names.iter().position(|m| m == n);
                action = Some(PanelAction::SetMaterial(
                    index,
                    choice.as_ref().and_then(index_of),
                ));
            }
        })
        .response
        .on_hover_text(if g.paths.is_empty() {
            "Give the part a material of the library (one undo step)"
        } else {
            "Give these parts a material of the library (their files are saved)"
        });
        let note = if study_type.takes_fixtures() {
            source_note(source)
        } else {
            source_note(thermal_source)
        };
        ui.label(egui::RichText::new(note).small().weak());
        if typed {
            if ui
                .small_button("Reset")
                .on_hover_text("Back to the material's own properties")
                .clicked()
            {
                overrides.remove(&g.key);
                thermal_overrides.remove(&g.key);
            }
            if g.can_apply
                && ui
                    .small_button("Apply to the material")
                    .on_hover_text("Keep these values in the part's material (one undo step)")
                    .clicked()
            {
                action = Some(PanelAction::ApplyMaterial(index));
            }
        }
    });
    if thermal {
        thermal_material_rows(ui, study_type, thermal_overrides, g);
    }
    if !study_type.takes_fixtures() {
        return action;
    }
    if study_type == StudyType::Frequency && g.density <= 0.0 {
        ui.label(
            egui::RichText::new("A frequency study needs a density: apply a material")
                .small()
                .color(egui::Color32::from_rgb(210, 140, 30)),
        );
    }
    let mut values = g
        .effective
        .map_or(Mechanical::new(0.0, 0.3, 0.0, 0.0), |(p, _)| p);
    let mut changed = false;
    egui::Grid::new(("simulation_material", &g.key))
        .num_columns(2)
        .spacing([10.0, 2.0])
        .show(ui, |ui| {
            ui.label("Elastic modulus");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut values.elastic_modulus)
                        .range(0.0..=1e7)
                        .speed(100.0)
                        .suffix(" MPa"),
                )
                .changed();
            ui.end_row();
            ui.label("Poisson's ratio");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut values.poisson_ratio)
                        .range(-0.99..=0.4999)
                        .speed(0.005)
                        .max_decimals(3),
                )
                .changed();
            ui.end_row();
            ui.label("Yield strength");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut values.yield_strength)
                        .range(0.0..=1e5)
                        .speed(1.0)
                        .suffix(" MPa"),
                )
                .on_hover_text("For the factor of safety; 0 when it has none")
                .changed();
            ui.end_row();
            ui.label("Tensile strength");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut values.tensile_strength)
                        .range(0.0..=1e5)
                        .speed(1.0)
                        .suffix(" MPa"),
                )
                .on_hover_text("For the factor of safety of a material with no yield strength")
                .changed();
            ui.end_row();
            if g.density > 0.0 {
                ui.label("Density");
                ui.label(format!("{} kg/m³", format_number(g.density)));
                ui.end_row();
            }
        });
    if source.is_none() && !changed {
        ui.label(
            egui::RichText::new("Unknown material: apply one, or type its properties")
                .small()
                .color(egui::Color32::from_rgb(210, 140, 30)),
        );
    }
    if changed {
        overrides.insert(g.key.clone(), values);
    }
    action
}

/// A material group's thermal properties: the conductivity, and what the study's type
/// also needs (the specific heat for a transient study, the expansion for thermal stress).
fn thermal_material_rows(
    ui: &mut egui::Ui,
    study_type: StudyType,
    overrides: &mut BTreeMap<String, Thermal>,
    g: &MaterialGroup,
) {
    let mut values = g.thermal.map_or(Thermal::new(0.0, 0.0, 0.0), |(t, _)| {
        Thermal::new(t.conductivity, t.specific_heat, t.expansion)
    });
    let mut changed = false;
    // Micrometres per metre and kelvin, as libraries list it.
    let mut expansion = values.expansion * 1e6;
    egui::Grid::new(("simulation_thermal_material", &g.key))
        .num_columns(2)
        .spacing([10.0, 2.0])
        .show(ui, |ui| {
            ui.label("Conductivity");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut values.conductivity)
                        .range(0.0..=5000.0)
                        .speed(0.1)
                        .suffix(" W/m·K"),
                )
                .changed();
            ui.end_row();
            if study_type == StudyType::TransientThermal {
                ui.label("Specific heat");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut values.specific_heat)
                            .range(0.0..=1e5)
                            .speed(1.0)
                            .suffix(" J/kg·K"),
                    )
                    .on_hover_text("With the density, how much heat it stores")
                    .changed();
                ui.end_row();
            }
            if study_type == StudyType::ThermalStress {
                ui.label("Expansion");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut expansion)
                            .range(-100.0..=1000.0)
                            .speed(0.1)
                            .suffix(" µm/m·K"),
                    )
                    .on_hover_text(
                        "How much it grows per kelvin: 12 µm/m·K for steel, 23 for aluminium",
                    )
                    .changed();
                ui.end_row();
            }
        });
    if g.thermal.is_none() && !changed {
        ui.label(
            egui::RichText::new("Unknown conductivity: apply a material, or type it")
                .small()
                .color(egui::Color32::from_rgb(210, 140, 30)),
        );
    }
    if changed {
        values.expansion = expansion * 1e-6;
        overrides.insert(g.key.clone(), values);
    }
}

pub(crate) fn paint_arrow(
    painter: &egui::Painter,
    from: egui::Pos2,
    to: egui::Pos2,
    stroke: egui::Stroke,
) {
    let d = to - from;
    if d.length() < 1e-3 {
        return;
    }
    let u = d.normalized();
    let side = egui::vec2(-u.y, u.x);
    let head = 7.0;
    let back = to - u * head;
    painter.line_segment([from, back], stroke);
    painter.add(egui::Shape::convex_polygon(
        vec![to, back + side * head * 0.45, back - side * head * 0.45],
        stroke.color,
        egui::Stroke::NONE,
    ));
}

/// A number with about four significant figures, without trailing noise.
pub(crate) fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return "∞".to_owned();
    }
    let a = v.abs();
    if a != 0.0 && !(1e-3..1e6).contains(&a) {
        return format!("{v:.3e}");
    }
    let decimals = if a >= 1000.0 {
        0
    } else if a >= 100.0 {
        1
    } else if a >= 10.0 {
        2
    } else if a >= 1.0 {
        3
    } else {
        (3 - a.log10().floor() as i32).clamp(3, 6) as usize
    };
    let text = format!("{v:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

pub(crate) fn format_value(v: f64, unit: &str) -> String {
    if unit.is_empty() {
        format_number(v)
    } else {
        format!("{} {unit}", format_number(v))
    }
}

/// A solid for Netgen: a copy of its shape, placed, which the thread frees.
struct NetgenInput {
    shape: ShapeHandle,
    /// The label of each of the copy's faces.
    face_labels: Vec<u32>,
    body: u32,
}

/// What the study's thread works from.
struct ThreadInput {
    /// The lattice mesher's surfaces, also the fallback when Netgen is not there.
    solids: Vec<SurfaceMesh>,
    /// Netgen's solids; empty to use the lattice mesher.
    netgen: Vec<NetgenInput>,
    options: MeshOptions,
    solve: SolveInput,
    names: Vec<String>,
    mode: RunMode,
    /// Look for contacts within this tolerance, mm.
    detect: Option<f64>,
    /// Preferences › Simulation's memory budget, GB, for the messages.
    budget_gb: f64,
}

/// Temperatures kept of a transient study, the start and the end included: enough to
/// play it smoothly, few enough to save.
const TRANSIENT_FRAMES: usize = 101;

/// What the study's thread solves once the model is meshed, with the materials of each
/// body, by the study's type.
enum SolveInput {
    Static {
        study: oxyde_sim::Study,
        materials: Vec<Elastic>,
    },
    Thermal {
        study: ThermalStudy,
        materials: Vec<ThermalMaterial>,
    },
    /// The steady temperatures, then the static solve of the expansion they cause.
    ThermalStress {
        thermal: ThermalStudy,
        thermal_materials: Vec<ThermalMaterial>,
        study: oxyde_sim::Study,
        materials: Vec<Elastic>,
        /// °C.
        reference: f64,
    },
    /// The static study solved again and again as material is taken away.
    Topology {
        study: TopologyStudy,
        materials: Vec<Elastic>,
    },
    /// The natural frequencies and mode shapes.
    Frequency {
        study: FrequencyStudy,
        materials: Vec<Elastic>,
    },
}

impl SolveInput {
    /// The contacts Run found and bonded itself, in every solve it makes.
    fn set_contacts(&mut self, contacts: Vec<oxyde_sim::Contact>) {
        match self {
            SolveInput::Static { study, .. } => study.contacts = contacts,
            SolveInput::Thermal { study, .. } => study.contacts = contacts,
            SolveInput::Topology { study, .. } => study.study.contacts = contacts,
            SolveInput::Frequency { study, .. } => study.contacts = contacts,
            SolveInput::ThermalStress { thermal, study, .. } => {
                thermal.contacts = contacts.clone();
                study.contacts = contacts;
            }
        }
    }
}

/// The solids meshed by Netgen on a worker kernel, each on its own, into one mesh whose
/// boundary facets carry the faces' labels; the copies are freed whatever happens. First
/// order from Netgen, made ten-node by `TetMesh::make_quadratic` when asked: the solver's
/// ten-node elements are straight-sided. `progress` hears how far the whole is and what
/// Netgen is doing, while it does it, and `cancel` stops Netgen where it is.
fn netgen_mesh(
    inputs: &[NetgenInput],
    h: f64,
    quadratic: bool,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f64, &'static str) + Sync),
) -> Result<TetMesh, VolumeMeshError> {
    let Some(mut kernel) = OcctKernel::open_worker() else {
        return Err(VolumeMeshError::Failed(
            "no kernel could be opened on the study's thread".to_owned(),
        ));
    };
    let options = VolumeMeshOptions {
        second_order: false,
        ..VolumeMeshOptions::with_size(h)
    };
    let mut mesh = TetMesh::default();
    let mut result = Ok(());
    let count = inputs.len().max(1) as f64;
    for (k, input) in inputs.iter().enumerate() {
        if result.is_ok() {
            let watch = MeshWatch {
                cancel,
                progress: &|f, stage| progress((k as f64 + f) / count, stage),
            };
            match kernel.volume_mesh_watched(input.shape, &options, watch) {
                Ok(v) => append_volume_mesh(&mut mesh, &v, input),
                Err(e) => result = Err(e),
            }
        }
        kernel.release(input.shape);
    }
    result?;
    if quadratic {
        mesh.make_quadratic();
    }
    mesh.validate().map_err(VolumeMeshError::Failed)?;
    Ok(mesh)
}

/// Adds one solid's Netgen mesh to `mesh`: its nodes, its elements in body `input.body`,
/// and the element faces on its boundary, labelled by the face each lies on.
fn append_volume_mesh(mesh: &mut TetMesh, v: &VolumeMesh, input: &NetgenInput) {
    let boundary: Vec<([u32; 3], u32)> = v
        .triangles
        .iter()
        .zip(&v.triangle_faces)
        .map(|(t, face)| {
            let label = input
                .face_labels
                .get(*face as usize)
                .copied()
                .unwrap_or(NO_LABEL);
            (*t, label)
        })
        .collect();
    mesh.append_solid(&v.nodes, &v.tets, &boundary, input.body);
}

/// The study's work on its own thread: the mesh, the contacts looked for, the solve.
fn study_thread(
    input: ThreadInput,
    cancel: &AtomicBool,
    shared: &Mutex<Shared>,
) -> Result<Outcome, String> {
    let ThreadInput {
        solids,
        netgen,
        options,
        mut solve,
        names,
        mode,
        detect,
        budget_gb,
    } = input;
    let report_progress = |stage: &'static str, fraction: f64| {
        if let Ok(mut s) = shared.lock() {
            s.stage = stage;
            s.fraction = fraction.clamp(0.0, 1.0);
        }
    };
    let mesh_only = mode != RunMode::Run;
    let span = if mesh_only { 1.0 } else { 0.3 };
    let start = Instant::now();
    let model_volume: f64 = solids.iter().map(|s| s.volume().abs()).sum();
    let mut note = None;
    let mut netgen_result = None;
    if !netgen.is_empty() {
        match netgen_mesh(
            &netgen,
            options.element_size,
            options.quadratic,
            cancel,
            &|f, stage| report_progress(stage, f * span),
        ) {
            // Netgen has no limit of its own: the budget is checked on what it made.
            Ok(mesh) if mesh.nodes.len() > options.max_nodes => {
                return Err(format!(
                    "Meshing failed: {}",
                    too_many_nodes(mesh.nodes.len(), budget_gb, options.quadratic)
                ));
            }
            Ok(mesh) => netgen_result = Some(mesh),
            Err(VolumeMeshError::Unsupported) => {
                note = Some("this Oxyde was built without Netgen".to_owned());
            }
            Err(VolumeMeshError::Failed(why)) => note = Some(format!("Netgen failed: {why}")),
            Err(VolumeMeshError::Cancelled) => return Err("cancelled".to_owned()),
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".to_owned());
    }
    let (mesh, report, mesher) = match netgen_result {
        Some(mesh) => {
            let report = MeshReport {
                element_size: options.element_size,
                nodes: mesh.nodes.len(),
                elements: mesh.tets.len(),
                volume: mesh.volume(),
                model_volume,
                dropped: 0,
            };
            (mesh, report, "netgen")
        }
        None => {
            let (mesh, report) = oxyde_sim::mesh_solids(&solids, &options, cancel, &|f| {
                report_progress("Meshing", f * span)
            })
            .map_err(|e| match e {
                oxyde_sim::MeshError::TooManyNodes(n) => format!(
                    "Meshing failed: {}",
                    too_many_nodes(n, budget_gb, options.quadratic)
                ),
                e => format!("Meshing failed: {e}"),
            })?;
            (mesh, report, "lattice")
        }
    };
    let mesh_seconds = start.elapsed().as_secs_f64();
    let candidates = detect.map(|tolerance| oxyde_sim::detect_contacts(&mesh, tolerance));
    if mode == RunMode::Run
        && let Some(found) = &candidates
    {
        // Run's own search: what it finds is bonded, as listed afterwards.
        solve.set_contacts(
            found
                .iter()
                .map(|c| c.contact(ContactKind::Bonded))
                .collect(),
        );
    }
    if mesh_only {
        return Ok(Outcome {
            mesh,
            report,
            results: None,
            thermal: None,
            topology: None,
            frequency: None,
            mesh_seconds,
            solve_seconds: 0.0,
            candidates,
            mesher,
            note,
            flow: None,
        });
    }
    report_progress("Solving", span);
    let start = Instant::now();
    let name = |b: u32| {
        names
            .get(b as usize)
            .cloned()
            .unwrap_or_else(|| format!("Body {b}"))
    };
    let solving = |from: f64, to: f64| {
        move |f: f64| report_progress("Solving", span + (1.0 - span) * (from + (to - from) * f))
    };
    let static_error = |e: oxyde_sim::SolveError| {
        match e {
        oxyde_sim::SolveError::NotHeld { body } => format!(
            "{} is not held: it could move freely. Add a fixture to it, or a contact with a held part.",
            name(body)
        ),
        oxyde_sim::SolveError::BadMaterial { body } => {
            format!("{} has no usable material properties", name(body))
        }
        oxyde_sim::SolveError::NoLoads => {
            "The loads act on no face of the mesh: select their faces again, or use smaller elements"
                .to_owned()
        }
        other => format!("The study failed: {other}"),
    }
    };
    let thermal_error = |e: oxyde_sim::ThermalError| match e {
        oxyde_sim::ThermalError::NotAnchored { body } => format!(
            "Nothing sets the temperature of {}: add a Temperature, a Convection or a Radiation on it, or a contact with a part that has one",
            name(body)
        ),
        oxyde_sim::ThermalError::BadMaterial { body } => {
            format!("{} has no usable thermal conductivity", name(body))
        }
        oxyde_sim::ThermalError::NoHeatCapacity { body } => format!(
            "{} needs a specific heat and a density for a transient study",
            name(body)
        ),
        oxyde_sim::ThermalError::BadStudy(why) => capitalized(&why),
        other => format!("The study failed: {other}"),
    };
    let options = SolveOptions::default();
    let (results, thermal, topology, frequency) = match &solve {
        SolveInput::Static { study, materials } => {
            let r = oxyde_sim::solve(
                &mesh,
                materials,
                study,
                &options,
                cancel,
                &solving(0.0, 1.0),
            )
            .map_err(static_error)?;
            (Some(r), None, None, None)
        }
        SolveInput::Frequency { study, materials } => {
            let f = oxyde_sim::solve_frequencies(
                &mesh,
                materials,
                study,
                &options,
                cancel,
                &solving(0.0, 1.0),
            )
            .map_err(static_error)?;
            (None, None, None, Some(f))
        }
        SolveInput::Topology { study, materials } => {
            let optimizing = |f: f64| report_progress("Optimizing", span + (1.0 - span) * f);
            let t = oxyde_sim::optimize(&mesh, materials, study, &options, cancel, &optimizing)
                .map_err(|e| match e {
                    oxyde_sim::TopologyError::Solve(e) => static_error(e),
                    other => capitalized(&other.to_string()),
                })?;
            (None, None, Some(t), None)
        }
        SolveInput::Thermal { study, materials } => {
            let t = oxyde_sim::solve_thermal(
                &mesh,
                materials,
                study,
                &options,
                cancel,
                &solving(0.0, 1.0),
            )
            .map_err(thermal_error)?;
            (None, Some(t), None, None)
        }
        SolveInput::ThermalStress {
            thermal,
            thermal_materials,
            study,
            materials,
            reference,
        } => {
            let t = oxyde_sim::solve_thermal(
                &mesh,
                thermal_materials,
                thermal,
                &options,
                cancel,
                &solving(0.0, 0.4),
            )
            .map_err(thermal_error)?;
            let strain = oxyde_sim::ThermalStrain {
                temperature: t.temperature.clone(),
                reference: *reference,
            };
            let r = oxyde_sim::solve_thermal_stress(
                &mesh,
                materials,
                study,
                &strain,
                &options,
                cancel,
                &solving(0.4, 1.0),
            )
            .map_err(static_error)?;
            (Some(r), Some(t), None, None)
        }
    };
    Ok(Outcome {
        mesh,
        report,
        results,
        thermal,
        topology,
        frequency,
        mesh_seconds,
        solve_seconds: start.elapsed().as_secs_f64(),
        candidates,
        mesher,
        note,
        flow: None,
    })
}

/// Rounds of smoothing a topology study's shape gets, shown and exported.
const SHAPE_SMOOTHING: usize = 8;
/// The shape's colour, and the whole model's faint one around it.
const SHAPE_COLOR: [u8; 3] = [110, 165, 225];
const GHOST_COLOR: [u8; 3] = [200, 205, 212];
const GHOST_OPACITY: f32 = 0.12;

/// A topology study's shape as a scene: the surface where the density is `threshold`, in
/// one colour, and the whole model faintly around it when `ghost`; with the share of the
/// mesh's volume the shape holds.
fn shape_scene(
    mesh: &TetMesh,
    density: &[f64],
    threshold: f64,
    smooth: bool,
    ghost: bool,
) -> (Scene, f64) {
    let shape = oxyde_sim::shape(
        mesh,
        density,
        threshold,
        if smooth { SHAPE_SMOOTHING } else { 0 },
    );
    let fraction = shape.volume() / mesh.volume().max(f64::MIN_POSITIVE);
    let mut faces = faces_by_label(&shape.positions, &shape.triangles, &shape.labels);
    let mut materials = vec![
        RenderMaterial {
            color: Some(SHAPE_COLOR),
            ..RenderMaterial::default()
        };
        faces.len()
    ];
    if ghost {
        let triangles: Vec<[u32; 3]> = mesh.facets.iter().map(|f| mesh.facet_corners(f)).collect();
        let labels: Vec<u32> = mesh.facets.iter().map(|f| f.label).collect();
        let around = faces_by_label(&mesh.nodes, &triangles, &labels);
        materials.extend(std::iter::repeat_n(
            RenderMaterial {
                color: Some(GHOST_COLOR),
                opacity: GHOST_OPACITY,
                ..RenderMaterial::default()
            },
            around.len(),
        ));
        faces.extend(around);
    }
    let mut scene = Scene::from_mesh(&Mesh {
        faces,
        edges: Vec::new(),
    });
    scene.set_materials(materials, Vec::new());
    (scene, fraction)
}

/// Triangles grouped into faces by label, each face with points of its own: the model's
/// faces are shaded flat to their edges, as in the model, and the cut through the material
/// (`NO_LABEL`, last) smoothly.
fn faces_by_label(positions: &[[f64; 3]], triangles: &[[u32; 3]], labels: &[u32]) -> Vec<FaceMesh> {
    let mut groups: BTreeMap<u32, (FaceMesh, BTreeMap<u32, u32>)> = BTreeMap::new();
    for (t, &label) in triangles.iter().zip(labels) {
        let (face, local) = groups.entry(label).or_insert_with(|| {
            (
                FaceMesh {
                    positions: Vec::new(),
                    triangles: Vec::new(),
                },
                BTreeMap::new(),
            )
        });
        let corners = t.map(|i| {
            *local.entry(i).or_insert_with(|| {
                let p = positions.get(i as usize).copied().unwrap_or_default();
                face.positions.push(p.map(|v| v as f32));
                (face.positions.len() - 1) as u32
            })
        });
        face.triangles.push(corners);
    }
    groups.into_values().map(|(face, _)| face).collect()
}

/// The status line after a frequency study: its lowest frequency.
fn frequency_summary(f: &FrequencyResults, seconds: f64) -> String {
    match f.modes.first() {
        Some(m) => format!(
            "Study solved in {seconds:.1} s: {} modes, the first at {}",
            f.modes.len(),
            format_value(m.frequency, "Hz")
        ),
        None => format!("Study solved in {seconds:.1} s: no mode found"),
    }
}

/// One line on a topology study's outcome, for the status bar.
fn topology_summary(t: &TopologyResults, seconds: f64) -> String {
    let flexible = t.compliance() / t.initial_compliance.max(f64::MIN_POSITIVE);
    let joined = t
        .joining
        .as_ref()
        .and_then(joining_text)
        .map_or_else(String::new, |text| format!("; {}", text.to_lowercase()));
    format!(
        "Topology optimized in {seconds:.1} s: {:.0} % of the mass kept, {} times as flexible as the whole part, {} iterations{}{joined}",
        t.mass_fraction() * 100.0,
        format_number(flexible),
        t.history.len(),
        if t.converged {
            ""
        } else {
            " (the limit: the shape may still change)"
        }
    )
}

/// What *Keep it in one piece* did, in a line; `None` when it had nothing to do.
fn joining_text(j: &oxyde_sim::Joining) -> Option<String> {
    let mut parts = Vec::new();
    if j.joined > 0 {
        parts.push(format!(
            "{} {} to the rest ({:+.1} % of the mass)",
            j.joined,
            if j.joined == 1 {
                "piece joined"
            } else {
                "pieces joined"
            },
            j.added_mass * 100.0
        ));
    }
    if j.removed > 0 {
        parts.push(format!(
            "{} loose {} taken away ({:.1} % of the mass)",
            j.removed,
            if j.removed == 1 { "bit" } else { "bits" },
            j.removed_mass * 100.0
        ));
    }
    (!parts.is_empty()).then(|| format!("Kept in one piece: {}", parts.join(", ")))
}

/// What the panel says of a topology shape that falls into pieces: their number, then the
/// fixtures, loads and preserved regions on each piece the loads do not reach, and how many
/// loose bits hold none of them. Empty for a shape in one piece. `faces` gives each label's
/// face.
fn piece_warning(
    study: &Study,
    faces: &BTreeMap<u32, SimFace>,
    pieces: &[oxyde_sim::Piece],
) -> Vec<String> {
    if pieces.len() < 2 {
        return Vec::new();
    }
    // Each item's labels, and whether it is a load.
    let items: Vec<(&str, bool, Vec<u32>)> = study
        .items
        .iter()
        .filter(|i| i.kind.fits(study.study_type))
        .map(|i| {
            let labels = faces
                .iter()
                .filter(|(_, f)| i.faces.contains(f))
                .map(|(l, _)| *l)
                .collect();
            let load = matches!(i.kind, ItemKind::Force { .. } | ItemKind::Pressure { .. });
            (i.name.as_str(), load, labels)
        })
        .collect();
    let on = |p: &oxyde_sim::Piece| -> Vec<(&str, bool)> {
        items
            .iter()
            .filter(|(_, _, labels)| labels.iter().any(|l| p.labels.binary_search(l).is_ok()))
            .map(|(name, load, _)| (*name, *load))
            .collect()
    };
    // The piece the loads are on: the largest that has one (they come largest first).
    let main = pieces
        .iter()
        .position(|p| on(p).iter().any(|(_, load)| *load))
        .unwrap_or(0);
    let mut lines = vec![format!(
        "The shape is in {} pieces at this threshold:",
        pieces.len()
    )];
    let mut loose = 0;
    for (k, p) in pieces.iter().enumerate().filter(|(k, _)| *k != main) {
        let _ = k;
        let names: Vec<&str> = on(p).iter().map(|(name, _)| *name).collect();
        match names.as_slice() {
            [] => loose += 1,
            [one] => lines.push(format!("{one} is on a piece the loads do not reach")),
            [first @ .., last] => lines.push(format!(
                "{} and {last} are on a piece the loads do not reach",
                first.join(", ")
            )),
        }
    }
    if loose > 0 {
        lines.push(format!(
            "{loose} loose {} holding none of them",
            if loose == 1 { "bit" } else { "bits" }
        ));
    }
    lines.push(if study.topology.one_piece {
        "Kept in one piece at 50 %: lower the threshold to see it whole".to_owned()
    } else {
        "Keep it in one piece (Target) joins them; or load what should stay joined".to_owned()
    });
    lines
}

/// `text` with its first letter in capitals, for a message of its own.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_round_trip_and_fill_in_defaults() {
        let p = SimulationPrefs {
            plot: PlotChoice::FactorOfSafety,
            ..SimulationPrefs::default()
        };
        let text = serde_json::to_string(&p).unwrap();
        let back: SimulationPrefs = serde_json::from_str(&text).unwrap();
        assert_eq!(back, p);
        let partial: SimulationPrefs = serde_json::from_str(r#"{ "bands": 8 }"#).unwrap();
        assert_eq!(partial.bands, 8);
        assert!(partial.deformed);
        // Preferences saved before topology studies get their settings.
        assert_eq!(partial.shape_threshold, 0.5);
        assert!(partial.smooth_shape && partial.show_original);
        let shape: SimulationPrefs = serde_json::from_str(r#"{ "plot": "shape" }"#).unwrap();
        assert_eq!(shape.plot, PlotChoice::Shape);
    }

    #[test]
    fn a_new_study_meshes_with_netgen_in_high_quality() {
        let study = Study::default();
        assert_eq!(study.name, "Static 1");
        assert_eq!(study.mesh.mesher, MesherChoice::Netgen);
        assert_eq!(study.mesh.quality, ElementQuality::High);
    }

    #[test]
    fn forces_along_an_axis_read_back_as_that_axis() {
        assert_eq!(
            LoadDirection::of_vector([0.0, -2.0, 0.0], 100.0),
            (LoadDirection::Y, -100.0)
        );
        let (d, n) = LoadDirection::of_vector([1.0, 1.0, 0.0], 5.0);
        assert_eq!(n, 5.0);
        let v = d.vector().unwrap();
        assert!((v[0] - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
        assert_eq!(
            LoadDirection::of_vector([0.0; 3], 1.0).0,
            LoadDirection::Normal
        );
    }

    /// The side face swept from segment `segment` of feature `feature`, or its end cap.
    fn face(feature: u32, segment: Option<u32>) -> SimFace {
        let id = oxyde_model::FeatureId::from_raw(feature);
        SimFace {
            occurrence: None,
            face: match segment {
                Some(s) => FaceName::profile(id, s),
                None => FaceName::end_cap(id),
            },
        }
    }

    #[test]
    fn contacts_found_again_keep_their_type_and_manual_ones_stay() {
        let mut study = Study::default();
        study.add_contact(ContactKind::Rough, vec![face(1, None)], vec![face(2, None)]);
        study.merge_found(vec![
            (face(1, Some(0)), face(2, Some(0)), 0.0),
            (face(1, Some(1)), face(2, Some(1)), 0.02),
        ]);
        assert_eq!(study.contacts.len(), 3);
        assert!(study.contacts_found);
        // The user makes one separation; found again (sides swapped), it stays so.
        study.contacts[1].kind = ContactKind::Separation;
        study.merge_found(vec![(face(2, Some(0)), face(1, Some(0)), 0.0)]);
        let kinds: Vec<(ContactKind, bool)> = study
            .contacts
            .iter()
            .map(|c| (c.kind, c.automatic))
            .collect();
        assert_eq!(
            kinds,
            [(ContactKind::Rough, false), (ContactKind::Separation, true)]
        );
        let names: Vec<&str> = study.contacts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Contact-1", "Contact-2"]);
    }

    #[test]
    fn items_are_numbered_per_kind() {
        let mut study = Study::default();
        study.add(ItemKind::Fixed, Vec::new());
        study.add(ItemKind::Pressure { mpa: 1.0 }, Vec::new());
        study.add(ItemKind::Fixed, Vec::new());
        study.add(ItemKind::Preserve { depth: 2.0 }, Vec::new());
        let names: Vec<&str> = study.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Fixed-1", "Pressure-1", "Fixed-2", "Preserve-1"]);
        // Read back from a file, the counters go on from the names.
        study.made.clear();
        study.recount();
        study.add(ItemKind::Preserve { depth: 0.0 }, Vec::new());
        assert_eq!(study.items[4].name, "Preserve-2");
    }

    #[test]
    fn a_topology_study_takes_fixtures_loads_and_preserved_regions_but_no_gravity() {
        let t = StudyType::Topology;
        assert!(ItemKind::Fixed.fits(t) && ItemKind::Pressure { mpa: 1.0 }.fits(t));
        assert!(ItemKind::Preserve { depth: 1.0 }.fits(t));
        assert!(!ItemKind::Temperature { celsius: 20.0 }.fits(t));
        assert!(!ItemKind::Preserve { depth: 1.0 }.fits(StudyType::Static));
        assert!(!t.takes_gravity() && StudyType::Static.takes_gravity());
        // Its plot is the shape; the stress plots are the others'.
        assert!(PlotChoice::Shape.fits(t) && !PlotChoice::VonMises.fits(t));
        assert!(!PlotChoice::Shape.fits(StudyType::Static));
        let study = Study::of_type(t, "Topology 1");
        assert_eq!(study.topology, TopologyGoal::default());
        // Four-node elements: the densities are the elements', and every iteration is a
        // seventh of the work. A part is made in one piece.
        assert_eq!(study.mesh.quality, ElementQuality::Draft);
        assert!(study.topology.one_piece);
        assert_eq!(Study::default().mesh.quality, ElementQuality::High);
        assert!(study.topology.target_mass > 0.0 && study.topology.target_mass < 1.0);
    }

    #[test]
    fn a_shape_in_pieces_says_what_the_loads_do_not_reach() {
        let mut study = Study::of_type(StudyType::Topology, "Topology 1");
        study.add(ItemKind::Fixed, vec![face(1, None)]);
        study.add(
            ItemKind::Force {
                newtons: 10.0,
                direction: LoadDirection::Y,
            },
            vec![face(2, None)],
        );
        study.add(ItemKind::Fixed, vec![face(3, None)]);
        study.add(ItemKind::Preserve { depth: 1.0 }, vec![face(4, None)]);
        let faces: BTreeMap<u32, SimFace> = (1..=4).map(|k| (k * 10, face(k, None))).collect();
        let piece = |labels: &[u32], volume: f64| oxyde_sim::Piece {
            labels: labels.to_vec(),
            volume,
        };
        // In one piece, nothing to say.
        assert!(piece_warning(&study, &faces, &[piece(&[10, 20, 30, 40], 9.0)]).is_empty());
        // The held and loaded piece, a held one the load does not reach, and a loose bit.
        let lines = piece_warning(
            &study,
            &faces,
            &[
                piece(&[30, 40], 12.0),
                piece(&[10, 20], 8.0),
                piece(&[], 0.5),
            ],
        );
        assert_eq!(lines[0], "The shape is in 3 pieces at this threshold:");
        assert_eq!(
            lines[1],
            "Fixed-2 and Preserve-1 are on a piece the loads do not reach"
        );
        assert_eq!(lines[2], "1 loose bit holding none of them");
        assert!(lines[3].starts_with("Kept in one piece"));
        study.topology.one_piece = false;
        let lines = piece_warning(&study, &faces, &[piece(&[10, 20], 8.0), piece(&[40], 1.0)]);
        assert_eq!(lines[1], "Preserve-1 is on a piece the loads do not reach");
        assert!(lines[2].starts_with("Keep it in one piece (Target) joins them"));
    }

    #[test]
    fn what_joining_did_reads_in_a_line() {
        let none = oxyde_sim::Joining::default();
        assert_eq!(joining_text(&none), None);
        let both = oxyde_sim::Joining {
            joined: 1,
            added_mass: 0.0123,
            removed: 2,
            removed_mass: 0.004,
        };
        assert_eq!(
            joining_text(&both).as_deref(),
            Some(
                "Kept in one piece: 1 piece joined to the rest (+1.2 % of the mass), 2 loose bits taken away (0.4 % of the mass)"
            )
        );
    }

    #[test]
    fn a_shape_scene_holds_the_shape_and_the_model_around_it() {
        let mesh = TetMesh::structured_box([0.0; 3], [10.0, 4.0, 2.0], [5, 2, 1]);
        let density: Vec<f64> = (0..mesh.tets.len())
            .map(|e| if e < mesh.tets.len() / 2 { 1.0 } else { 0.0 })
            .collect();
        let (_, fraction) = shape_scene(&mesh, &density, 0.5, true, true);
        assert!(fraction > 0.2 && fraction < 0.8, "{fraction}");
        let (_, all) = shape_scene(&mesh, &vec![1.0; mesh.tets.len()], 0.5, false, false);
        assert!((all - 1.0).abs() < 1e-9, "{all}");
    }

    #[test]
    fn the_study_overrides_the_material_which_beats_the_guess() {
        let library = oxyde_model::MaterialLibrary::standard();
        let steel = library
            .get("Plain carbon steel (AISI 1020)")
            .unwrap()
            .clone();
        let body = BodyInfo {
            name: "Bracket".into(),
            id: None,
            occurrence: None,
            key: steel.name.clone(),
            material: Some(steel.clone()),
            path: None,
        };
        let mut study = Study::default();
        let e = elastic_of(&study, &body).unwrap();
        assert_eq!((e.youngs_modulus, e.yield_strength), (200_000.0, 351.6));
        assert_eq!(e.density, 7870.0);
        // A material without properties of its own: typical values for its name.
        let mut bare = steel.clone();
        bare.mechanical = None;
        bare.name = "Some steel".into();
        assert_eq!(
            mechanical_of(&study, "Some steel", Some(&bare)).map(|(_, s)| s),
            Some(PropsSource::Typical)
        );
        study
            .overrides
            .insert(body.key.clone(), Mechanical::new(1.0, 0.2, 0.0, 3.0));
        let e = elastic_of(&study, &body).unwrap();
        // No yield strength: measured against the tensile strength.
        assert_eq!(
            (e.youngs_modulus, e.poisson, e.yield_strength, e.density),
            (1.0, 0.2, 3.0, 7870.0)
        );
        let none = BodyInfo {
            material: None,
            key: "part:x".into(),
            ..body
        };
        assert!(elastic_of(&Study::default(), &none).is_none());
    }

    #[test]
    fn a_material_group_knows_its_parts() {
        let library = oxyde_model::MaterialLibrary::standard();
        let steel = library
            .get("Plain carbon steel (AISI 1020)")
            .unwrap()
            .clone();
        let body = |name: &str, path: &str, material: Option<&Material>| BodyInfo {
            name: name.into(),
            id: None,
            occurrence: None,
            key: material.map_or_else(|| format!("part:{name}"), |m| m.name.clone()),
            material: material.cloned(),
            path: Some(path.into()),
        };
        let bodies = [
            body("Plate<1>", "plate.oxyprt", Some(&steel)),
            body("Bolt<1>", "bolt.oxyprt", None),
            body("Plate<2>", "plate.oxyprt", Some(&steel)),
            body("Bracket<1>", "bracket.oxyprt", Some(&steel)),
        ];
        let groups = material_groups_of(&bodies, &Study::default(), true);
        assert_eq!(groups.len(), 2);
        // Changing the steel line changes every steel part, each file once.
        assert_eq!(groups[0].bodies, ["Plate<1>", "Plate<2>", "Bracket<1>"]);
        let paths: Vec<std::path::PathBuf> = vec!["plate.oxyprt".into(), "bracket.oxyprt".into()];
        assert_eq!(groups[0].paths, paths);
        assert!(groups[0].has_material && !groups[0].can_apply);
        assert_eq!(groups[1].label, "Bolt<1> (no material)");
        assert!(!groups[1].has_material);
        // The open part: no file to rewrite, the part takes the material itself.
        let open = [BodyInfo {
            path: None,
            ..body("Plate", "", Some(&steel))
        }];
        let groups = material_groups_of(&open, &Study::default(), false);
        assert!(groups[0].paths.is_empty() && groups[0].can_apply);
    }

    #[test]
    fn the_memory_budget_sets_the_node_limit() {
        // 8 GB: about 470 000 nodes of ten-node elements, 320 000 of four-node ones.
        assert_eq!(max_nodes(8.0, true), 470_588);
        assert_eq!(max_nodes(8.0, false), 320_000);
        assert_eq!(max_nodes(16.0, true), 2 * max_nodes(8.0, true));
        // Nonsense falls back to something usable.
        assert_eq!(max_nodes(f64::NAN, true), max_nodes(8.0, true));
        assert_eq!(max_nodes(0.0, true), max_nodes(0.25, true));
        let why = too_many_nodes(1_590_020, 8.0, true);
        assert!(
            why.contains("1590020 nodes") && why.contains("8 GB"),
            "{why}"
        );
        assert!(why.contains("Preferences › Simulation"), "{why}");
    }

    #[test]
    fn numbers_read_well() {
        assert_eq!(format_number(150.0), "150");
        assert_eq!(format_number(1.06487), "1.065");
        assert_eq!(format_number(0.0123456), "0.01235");
        assert_eq!(format_number(12345.6), "12346");
        assert_eq!(format_number(2.5e-5), "2.500e-5");
        assert_eq!(format_number(f64::INFINITY), "∞");
        assert_eq!(format_value(3.0, "MPa"), "3 MPa");
    }

    #[test]
    fn surfaces_carry_the_face_labels() {
        let mesh = Mesh {
            faces: vec![
                FaceMesh {
                    positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                    triangles: vec![[0, 1, 2]],
                },
                FaceMesh {
                    positions: vec![[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                    triangles: vec![[0, 1, 2]],
                },
            ],
            edges: Vec::new(),
        };
        let s = surface_of(&mesh, |k| 10 + k as u32);
        assert_eq!(s.labels, [10, 11]);
        assert_eq!(s.triangles[1], [3, 4, 5]);
        assert!((surface_area(&s) - 1.0).abs() < 1e-12);
    }
}
