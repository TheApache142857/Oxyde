//! The Simulation tab: a linear static study of the part or assembly, as SolidWorks
//! Simulation runs one (`docs/SIMULATION.md`).
//!
//! Select faces, then *Fixed Geometry*, *Roller/Slider*, *Force* or *Pressure* makes a
//! fixture or a load on them; *Gravity* adds the model's weight. The panel at the top of
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
use oxyde_geom::Transform;
use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{
    FaceMesh, FaceSource, Kernel, Mesh, ShapeHandle, VolumeMesh, VolumeMeshError, VolumeMeshOptions,
};
use oxyde_model::{FaceName, Material, Mechanical};
use oxyde_occt::OcctKernel;
use oxyde_render::{Material as RenderMaterial, Scene};
use oxyde_sim::{
    ContactCandidate, ContactKind, Elastic, FixtureKind, LoadKind, MeshOptions, MeshReport,
    NO_LABEL, Quantity, Results, SolveOptions, Summary, SurfaceMesh, TetMesh,
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
}

impl PlotChoice {
    const ALL: [PlotChoice; 5] = [
        PlotChoice::VonMises,
        PlotChoice::FirstPrincipal,
        PlotChoice::Displacement,
        PlotChoice::FactorOfSafety,
        PlotChoice::ContactPressure,
    ];

    fn quantity(self) -> Option<Quantity> {
        match self {
            PlotChoice::VonMises => Some(Quantity::VonMises),
            PlotChoice::FirstPrincipal => Some(Quantity::FirstPrincipal),
            PlotChoice::Displacement => Some(Quantity::Displacement),
            PlotChoice::FactorOfSafety => Some(Quantity::FactorOfSafety),
            PlotChoice::ContactPressure => None,
        }
    }

    fn label(self) -> &'static str {
        self.quantity().map_or("Contact pressure", Quantity::label)
    }

    fn unit(self) -> &'static str {
        self.quantity().map_or("MPa", Quantity::unit)
    }

    /// Its value at each node (contact pressure: 0 off the contacts' faces).
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
}

impl ItemKind {
    fn is_fixture(&self) -> bool {
        matches!(self, ItemKind::Fixed | ItemKind::Roller)
    }

    fn base_name(&self) -> &'static str {
        match self {
            ItemKind::Fixed => "Fixed",
            ItemKind::Roller => "Roller/Slider",
            ItemKind::Force { .. } => "Force",
            ItemKind::Pressure { .. } => "Pressure",
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

/// The study: what holds the model and what loads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Study {
    /// `Static 1`, as SolidWorks names a study.
    pub name: String,
    pub items: Vec<StudyItem>,
    /// The model's weight, along −Y (the world's down).
    pub gravity: bool,
    /// Mechanical properties this study uses instead of the material's, by material
    /// group (a material name, or `part:` and a document id for a part without one).
    pub overrides: BTreeMap<String, Mechanical>,
    pub contacts: Vec<StudyContact>,
    /// mm: how close faces of different bodies must be to be found in contact.
    pub contact_tolerance: f64,
    /// The contacts were looked for: *Run* does not look again by itself.
    pub contacts_found: bool,
    pub mesh: StudyMesh,
    /// Items made so far of each kind, for their names.
    pub(crate) made: BTreeMap<&'static str, u32>,
}

impl Default for Study {
    fn default() -> Self {
        Self {
            name: "Static 1".to_owned(),
            items: Vec::new(),
            gravity: false,
            overrides: BTreeMap::new(),
            contacts: Vec::new(),
            contact_tolerance: 0.1,
            contacts_found: false,
            mesh: StudyMesh::default(),
            made: BTreeMap::new(),
        }
    }
}

impl Study {
    /// The counters behind the names, after items and contacts were read from a file, so
    /// a new one does not repeat a name.
    pub(crate) fn recount(&mut self) {
        let mut made: BTreeMap<&'static str, u32> = BTreeMap::new();
        let bases = ["Fixed", "Roller/Slider", "Force", "Pressure", "Contact"];
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

    fn add(&mut self, kind: ItemKind, faces: Vec<SimFace>) -> usize {
        let name = self.next_name(kind.base_name());
        self.items.push(StudyItem { name, kind, faces });
        self.items.len() - 1
    }
}

// ---- The surfaces --------------------------------------------------------------------------

/// A solid of the model handed to the mesher.
#[derive(Debug, Clone)]
struct BodyInfo {
    name: String,
    /// Its material group: the material's name, or `part:` and the part's document id.
    key: String,
    material: Option<Material>,
    /// The part file it comes from (an assembly's part), to give it another material.
    path: Option<std::path::PathBuf>,
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
struct Surfaces {
    revision: u64,
    assembly: bool,
    solids: Vec<SurfaceMesh>,
    /// The face each label stands for.
    faces: Vec<SimFace>,
    /// The body (index into `bodies`) of each label.
    face_body: Vec<u32>,
    bodies: Vec<BodyInfo>,
    sources: Vec<BodySource>,
    /// mm³, mm² and mm.
    volume: f64,
    area: f64,
    diagonal: f64,
}

impl Surfaces {
    /// The labels of `faces`.
    fn labels(&self, faces: &[SimFace]) -> Vec<u32> {
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

struct Outcome {
    mesh: TetMesh,
    report: MeshReport,
    results: Option<Results>,
    mesh_seconds: f64,
    solve_seconds: f64,
    /// Faces found facing each other, when contacts were looked for.
    candidates: Option<Vec<ContactCandidate>>,
    /// `netgen` or `lattice`.
    mesher: &'static str,
    /// Why Netgen was asked for and not used.
    note: Option<String>,
}

#[derive(Default)]
struct Shared {
    stage: &'static str,
    fraction: f64,
    outcome: Option<Result<Outcome, String>>,
}

struct SimJob {
    cancel: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    revision: u64,
    assembly: bool,
    started: Instant,
    diagonal: f64,
    mode: RunMode,
    /// The face of each label and the name and material group of each body, as the run
    /// was made.
    faces: Vec<SimFace>,
    bodies: Vec<(String, String)>,
    /// The model and study as they were run, for the results file.
    fingerprint: Option<Fingerprint>,
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
    id: u64,
    revision: u64,
    assembly: bool,
    pub(crate) mesh: TetMesh,
    report: MeshReport,
    pub(crate) results: Option<Results>,
    summary: Option<Summary>,
    pub(crate) mesh_seconds: f64,
    pub(crate) solve_seconds: f64,
    diagonal: f64,
    /// `netgen` or `lattice`.
    pub(crate) mesher: &'static str,
    /// The face each label of the mesh stands for.
    pub(crate) faces: BTreeMap<u32, SimFace>,
    /// Each body's name and material group.
    pub(crate) bodies: Vec<(String, String)>,
    /// The model and study as they were solved.
    pub(crate) fingerprint: Option<Fingerprint>,
}

/// What the viewport shows: anything else, and the plot is made again.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PlotKey {
    id: u64,
    plot: Option<PlotChoice>,
    scale: f64,
    bands: u32,
    show_mesh: bool,
}

/// The legend of the plot shown.
#[derive(Debug, Clone, PartialEq)]
struct Legend {
    title: String,
    unit: &'static str,
    range: (f64, f64),
    bands: u32,
    reversed: bool,
    scale: f64,
    /// The node marked (deformed position) and its label.
    marker: Option<([f64; 3], String)>,
}

/// The Simulation tab's state.
#[derive(Default)]
pub struct SimulationView {
    /// The document the study is for (assembly or not, document id).
    document: Option<(bool, String)>,
    pub study: Study,
    /// Where the study is saved, once it is.
    pub(crate) file: Option<StudyFile>,
    surfaces: Option<Surfaces>,
    /// Why the surfaces could not be made for this revision (assembly or not): not tried
    /// again every frame.
    surfaces_failed: Option<(u64, bool, String)>,
    job: Option<SimJob>,
    pub(crate) done: Option<Done>,
    /// The plot (or the mesh) is shown over the model; off while setting up.
    show_plot: bool,
    shown: Option<PlotKey>,
    legend: Option<Legend>,
    error: Option<String>,
    prefs_dirty: bool,
    /// The item picked in the panel, whose faces are selected.
    selected: Option<usize>,
    /// The contact picked in the panel.
    selected_contact: Option<usize>,
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

/// The solver's material for `body`: [`mechanical_of`] with the material's density, the
/// factor of safety measured against the yield strength, else the tensile strength.
fn elastic_of(study: &Study, body: &BodyInfo) -> Option<Elastic> {
    let density = body.material.as_ref().map_or(0.0, |m| m.density);
    let (p, _) = mechanical_of(study, &body.key, body.material.as_ref())?;
    Some(Elastic::new(
        p.elastic_modulus,
        p.poisson_ratio,
        p.strength(),
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

/// More nodes than this and the mesh is refused: the matrix alone would pass 500 MB.
const MAX_NODES: usize = 250_000;

// ---- The application side ------------------------------------------------------------------

impl OxydeApp {
    /// Whether the Simulation tab is open and in charge of the viewport.
    pub(crate) fn simulation_active(&self) -> bool {
        let tab = match self.asm {
            Some(_) => self.asm_tab == crate::assembly_ui::AsmTab::Simulation,
            None => self.tab == crate::app::CommandTab::Simulation,
        };
        tab && self.sketch.is_none()
            && self.section.is_none()
            && self.mesh_export.is_none()
            && !self.busy()
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
        }
        self.poll_simulation(ctx);
        if self.simulation.shown.is_some() && !self.plot_wanted() {
            self.restore_simulation_scene();
            ctx.request_repaint();
        }
        if self.simulation.prefs_dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.simulation.prefs_dirty = false;
            self.save_preferences();
        }
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

    fn restore_simulation_scene(&mut self) {
        if self.simulation.shown.take().is_some() {
            // The usual scene comes back on the next frame.
            self.viewport.pickable = None;
            self.scene_revision = None;
            self.asm_scene = None;
        }
        self.simulation.legend = None;
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
                self.status = match (&o.results, &summary) {
                    (Some(_), Some(s)) => format!(
                        "Study solved in {:.1} s: von Mises up to {}, displacement up to {}",
                        o.mesh_seconds + o.solve_seconds,
                        format_value(s.max_von_mises.0, "MPa"),
                        format_value(s.max_displacement.0, "mm"),
                    ),
                    _ => format!(
                        "Meshed in {:.1} s: {} nodes, {} elements",
                        o.mesh_seconds, o.report.nodes, o.report.elements
                    ),
                };
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
                let solved = o.results.is_some();
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
                    results: o.results,
                    summary,
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
                });
                self.simulation.error = None;
                self.simulation.show_plot = true;
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
    fn ensure_surfaces(&mut self) -> Result<(), String> {
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
    fn selected_sim_faces(&self) -> Vec<SimFace> {
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
    fn add_study_item(&mut self, kind: ItemKind) {
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
            // The plot hides, so the model is there to pick on.
            self.simulation.show_plot = false;
            return;
        }
        let count = faces.len();
        let index = self.simulation.study.add(kind, faces);
        self.simulation.selected = Some(index);
        // The plot hides, so the model is there to pick on.
        self.simulation.show_plot = false;
        if let Some(item) = self.simulation.study.items.get(index) {
            self.status = format!("{} on {count} face(s)", item.name);
        }
    }

    /// The study as the solver takes it, with the materials, or what is missing.
    fn solver_study(
        &self,
        surfaces: &Surfaces,
        mesh_only: bool,
    ) -> Result<(oxyde_sim::Study, Vec<Elastic>), String> {
        let study = &self.simulation.study;
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
        for item in &study.items {
            let labels = surfaces.labels(&item.faces);
            if labels.is_empty() && !mesh_only {
                return Err(format!(
                    "{} is on no face of the model any more: select its faces again",
                    item.name
                ));
            }
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
            }
        }
        if !mesh_only {
            if fixtures.is_empty() {
                return Err(
                    "The model is not held: select faces and add Fixed Geometry or a Roller/Slider"
                        .to_owned(),
                );
            }
            if loads.is_empty() && !study.gravity {
                return Err(
                    "Nothing loads the model: select faces and add a Force or a Pressure, or turn on Gravity"
                        .to_owned(),
                );
            }
        }
        let mut materials = Vec::with_capacity(surfaces.bodies.len());
        for body in &surfaces.bodies {
            match elastic_of(study, body) {
                Some(e) if e.is_valid() => materials.push(e),
                _ if mesh_only => materials.push(Elastic::new(1.0, 0.3, 0.0, 0.0)),
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
            if study.gravity
                && !mesh_only
                && body.material.as_ref().is_none_or(|m| m.density <= 0.0)
            {
                return Err(format!(
                    "Gravity needs a density: {} has none (apply a material)",
                    body.name
                ));
            }
        }
        Ok((
            oxyde_sim::Study {
                fixtures,
                loads,
                gravity: study.gravity.then_some([0.0, -9.81, 0.0]),
                contacts,
            },
            materials,
        ))
    }

    /// Run, Mesh or Find Contacts: meshes the model and, for Run, solves the study, on a
    /// thread.
    pub(crate) fn run_simulation(&mut self, mode: RunMode) {
        self.simulation.error = None;
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
        let (study, materials) = match self.solver_study(surfaces, mesh_only) {
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
                max_nodes: MAX_NODES,
                separate_bodies: true,
            },
            study,
            materials,
            names: surfaces.bodies.iter().map(|b| b.name.clone()).collect(),
            mode,
            detect,
        };
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
        });
        self.status = match mode {
            RunMode::Mesh => "Meshing…",
            RunMode::FindContacts => "Looking for contacts…",
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

    /// Replaces the study (one read from a file): its results and the plot go.
    pub(crate) fn set_study(&mut self, study: Study) {
        self.simulation.job = None;
        self.restore_simulation_scene();
        self.simulation.done = None;
        self.simulation.selected = None;
        self.simulation.selected_contact = None;
        self.simulation.study = study;
        self.simulation.show_plot = false;
    }

    /// Results read from a file, shown as if just solved.
    pub(crate) fn show_loaded_results(
        &mut self,
        mesh: TetMesh,
        results: Results,
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
        let summary = Some(results.summary());
        self.restore_simulation_scene();
        self.simulation.done = Some(Done {
            id: crate::document::next_revision(),
            revision: self.document_revision(),
            assembly: self.asm.is_some(),
            mesh,
            report,
            results: Some(results),
            summary,
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

    fn cancel_simulation(&mut self) {
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
        let prefs = self.prefs.simulation.clone();
        let Some(done) = self.simulation.done.as_ref() else {
            return;
        };
        let scale = match (&done.results, &done.summary) {
            (Some(_), Some(s)) if prefs.deformed => match prefs.scale {
                ScaleChoice::True => 1.0,
                ScaleChoice::Custom => prefs.custom_scale.max(0.0),
                ScaleChoice::Automatic if s.max_displacement.0 > 0.0 => {
                    0.05 * done.diagonal / s.max_displacement.0
                }
                ScaleChoice::Automatic => 1.0,
            },
            _ => 0.0,
        };
        let key = PlotKey {
            id: done.id,
            plot: done.results.as_ref().map(|_| prefs.plot),
            scale,
            bands: prefs.bands.clamp(2, 64),
            show_mesh: prefs.show_mesh || done.results.is_none(),
        };
        if self.simulation.shown == Some(key) {
            return;
        }
        let mesh = &done.mesh;
        let positions: Vec<[f64; 3]> = match &done.results {
            Some(r) if scale > 0.0 => mesh
                .nodes
                .iter()
                .zip(&r.displacement)
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
        let (groups, colors, legend) = match (&done.results, key.plot) {
            (Some(results), Some(plot)) => {
                let values = plot.values(results);
                let finite = || values.iter().copied().filter(|v| v.is_finite());
                let lo = finite().fold(f64::INFINITY, f64::min);
                let hi = finite().fold(f64::NEG_INFINITY, f64::max);
                let (lo, hi) = if lo.is_finite() { (lo, hi) } else { (0.0, 1.0) };
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
                    (PlotChoice::FirstPrincipal | PlotChoice::ContactPressure, _) => values
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
                let legend = Legend {
                    title: plot.label().to_owned(),
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
        // Looked at, not picked: the faces are the plot's, not the model's.
        self.viewport.pickable = Some((0, 0));
        self.viewport.set_scene(scene, false);
        // The usual scene is not rebuilt over the plot while the document stays as it is.
        match self.asm.as_ref() {
            Some(asm) => self.asm_scene = Some(asm.revision),
            None => self.scene_revision = Some(self.scene_key()),
        }
        self.simulation.shown = Some(key);
        self.simulation.legend = legend;
    }

    /// After the viewport is drawn: the fixtures and loads as symbols on their faces
    /// while setting up, or the plot's legend; the progress in the corner.
    pub(crate) fn simulation_over_viewport(&self, ui: &egui::Ui, output: &ViewportOutput) {
        if !self.simulation_active() {
            return;
        }
        let painter = ui.painter_at(output.rect);
        if self.simulation.shown.is_some() {
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
        } else if s.done.is_none() {
            "Select faces, add fixtures and loads, then Run".to_owned()
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

    fn show_plot(&mut self, plot: Option<PlotChoice>) {
        if let Some(plot) = plot
            && self.prefs.simulation.plot != plot
        {
            self.prefs.simulation.plot = plot;
            self.simulation.prefs_dirty = true;
        }
        self.simulation.show_plot = true;
    }

    /// The Simulation tab's buttons.
    pub(crate) fn simulation_tab(&mut self, ui: &mut egui::Ui) {
        use crate::icons::{Icon, command_button};
        let has_model = match &self.asm {
            Some(asm) => !asm.leaves(&asm.assembly).is_empty(),
            None => self.doc.part.body().is_some(),
        };
        let has_faces = !self.selected_sim_faces().is_empty();
        let running = self.simulation.job.is_some();
        let items: [(Icon, &str, ItemKind, &str); 4] = [
            (
                Icon::Fixture,
                "Fixed Geometry",
                ItemKind::Fixed,
                "Hold the selected faces where they are: no motion at all",
            ),
            (
                Icon::Roller,
                "Roller/Slider",
                ItemKind::Roller,
                "Let the selected faces slide along themselves, but not move off them",
            ),
            (
                Icon::Force,
                "Force",
                ItemKind::Force {
                    newtons: 100.0,
                    direction: LoadDirection::Normal,
                },
                "A force on the selected faces, spread by area (set its value in the panel)",
            ),
            (
                Icon::Pressure,
                "Pressure",
                ItemKind::Pressure { mpa: 1.0 },
                "A pressure on the selected faces, pressing into them",
            ),
        ];
        for (k, (icon, label, kind, hint)) in items.into_iter().enumerate() {
            if k == 2 {
                ui.add_space(2.0);
                ui.separator();
                ui.add_space(2.0);
            }
            let response = command_button(ui, icon, label, has_model && !running, false);
            let response = if has_faces {
                response.on_hover_text(hint)
            } else {
                response.on_hover_text(format!("{hint}\n(select faces first)"))
            };
            if response.clicked() {
                self.add_study_item(kind);
            }
        }
        if command_button(
            ui,
            Icon::Gravity,
            "Gravity",
            has_model && !running,
            self.simulation.study.gravity,
        )
        .on_hover_text("The model's own weight, from its materials' densities, downwards (−Y)")
        .clicked()
        {
            self.simulation.study.gravity = !self.simulation.study.gravity;
            self.simulation.show_plot = false;
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        if command_button(
            ui,
            Icon::Mate,
            "Find Contacts",
            has_model && !running,
            false,
        )
        .on_hover_text(
            "Find the faces of different bodies that touch and list them as contacts \
                 (bonded until changed in the panel), as Fusion 360 does",
        )
        .clicked()
        {
            self.run_simulation(RunMode::FindContacts);
        }
        if command_button(
            ui,
            Icon::Interference,
            "Manual Contact",
            has_model && !running,
            false,
        )
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
        if command_button(ui, Icon::Mesh, "Mesh", has_model && !running, false)
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
            .on_hover_text("Mesh the model and solve the study")
            .clicked()
        {
            self.run_simulation(RunMode::Run);
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        let solved = self
            .simulation
            .done
            .as_ref()
            .is_some_and(|d| d.results.is_some() && d.revision == self.document_revision());
        let plots = [
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
        ];
        for (icon, label, plot, hint) in plots {
            let active = solved
                && self.simulation.show_plot
                && (self.prefs.simulation.plot == plot
                    || (plot == PlotChoice::VonMises
                        && self.prefs.simulation.plot == PlotChoice::FirstPrincipal));
            if command_button(ui, icon, label, solved, active)
                .on_hover_text(hint)
                .clicked()
            {
                self.show_plot(Some(plot));
            }
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
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.strong("Static study");
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
                let solved = self.simulation.done.as_ref().is_some_and(|d| d.results.is_some());
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
                                if let Some(a) =
                                    material_rows(ui, &mut study.overrides, g, i, &material_names)
                                {
                                    action = Some(a);
                                }
                            }
                        });
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
                    egui::CollapsingHeader::new("External loads")
                        .default_open(true)
                        .show(ui, |ui| {
                            let mut any = false;
                            for (i, item) in study.items.iter_mut().enumerate() {
                                if item.kind.is_fixture() {
                                    continue;
                                }
                                any = true;
                                item_row(ui, i, item, selected, &mut action);
                            }
                            if ui
                                .checkbox(&mut study.gravity, "Gravity (9.81 m/s², −Y)")
                                .on_hover_text("The model's weight, from its materials' densities")
                                .changed()
                            {
                                action.get_or_insert(PanelAction::Edited);
                            }
                            if !any && !study.gravity {
                                ui.label(
                                    egui::RichText::new("Select faces, then Force or Pressure")
                                        .small()
                                        .weak(),
                                );
                            }
                        });
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
                                    ui.label("Element size");
                                    ui.horizontal(|ui| {
                                        ui.radio_value(&mut m.custom_size, false, "Automatic");
                                        ui.radio_value(&mut m.custom_size, true, "Typed");
                                    });
                                    ui.end_row();
                                    if m.custom_size {
                                        ui.label("");
                                        ui.add(
                                            egui::DragValue::new(&mut m.element_size)
                                                .range(0.01..=1000.0)
                                                .speed(0.05)
                                                .suffix(" mm"),
                                        );
                                        ui.end_row();
                                    } else {
                                        ui.label("Density");
                                        ui.horizontal(|ui| {
                                            // Room left for the box beside the slider,
                                            // as the panel is narrow.
                                            ui.spacing_mut().slider_width =
                                                (ui.available_width() - 140.0).clamp(50.0, 160.0);
                                            ui.label(egui::RichText::new("Coarse").small());
                                            ui.add(
                                                egui::Slider::new(&mut m.density, 0.0..=1.0)
                                                    .show_value(false),
                                            );
                                            ui.label(egui::RichText::new("Fine").small());
                                            // The same density as a number to type: 0 %
                                            // the coarsest mesh, 100 % the finest.
                                            ui.add(
                                                egui::DragValue::new(&mut m.density)
                                                    .range(0.0..=1.0)
                                                    .speed(0.005)
                                                    .custom_formatter(|v, _| {
                                                        format!("{:.0} %", v * 100.0)
                                                    })
                                                    .custom_parser(|s| {
                                                        s.trim()
                                                            .trim_end_matches('%')
                                                            .trim()
                                                            .parse::<f64>()
                                                            .ok()
                                                            .map(|v| v / 100.0)
                                                    }),
                                            )
                                            .on_hover_text(
                                                "0 % is the coarsest mesh, 100 % the finest",
                                            );
                                        });
                                        ui.end_row();
                                        if let Some(h) = element_size {
                                            ui.label("");
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "Elements of about {} mm",
                                                    format_number(h)
                                                ))
                                                .small()
                                                .weak(),
                                            );
                                            ui.end_row();
                                        }
                                    }
                                });
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
                                    if let Some(r) = &d.results {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Solved in {:.1} s, {} iterations, {} mesh",
                                                d.solve_seconds,
                                                r.iterations,
                                                if d.mesher == "netgen" { "Netgen" } else { "lattice" }
                                            ))
                                            .small()
                                            .weak(),
                                        );
                                        if !r.contacts.is_empty() && !r.contact_solve.converged {
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
                                }
                                None => {
                                    ui.label(egui::RichText::new("Not run yet").weak());
                                }
                            }
                            let mut shown = self.simulation.show_plot;
                            if ui
                                .add_enabled(done.is_some(), egui::Checkbox::new(&mut shown, "Show results"))
                                .changed()
                            {
                                action = Some(PanelAction::ShowPlot(shown));
                            }
                            egui::Grid::new("simulation_plot")
                                .num_columns(2)
                                .spacing([10.0, 4.0])
                                .show(ui, |ui| {
                                    ui.label("Plot");
                                    egui::ComboBox::from_id_salt("simulation_plot_choice")
                                        .selected_text(p.plot.label())
                                        .show_ui(ui, |ui| {
                                            for c in PlotChoice::ALL {
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
            Some(PanelAction::Select(i)) => {
                self.simulation.selected = Some(i);
                self.simulation.show_plot = false;
                let faces = self
                    .simulation
                    .study
                    .items
                    .get(i)
                    .map(|item| item.faces.clone())
                    .unwrap_or_default();
                self.select_sim_faces(&faces);
            }
            Some(PanelAction::UseSelection(i)) => {
                let faces = self.selected_sim_faces();
                if faces.is_empty() {
                    self.status = "Select the faces first".to_owned();
                } else if let Some(item) = self.simulation.study.items.get_mut(i) {
                    self.status = format!("{} now on {} face(s)", item.name, faces.len());
                    item.faces = faces;
                }
                self.simulation.show_plot = false;
            }
            Some(PanelAction::Delete(i)) => {
                if i < self.simulation.study.items.len() {
                    self.simulation.study.items.remove(i);
                }
                self.simulation.selected = None;
                self.simulation.show_plot = false;
            }
            Some(PanelAction::ShowPlot(on)) => self.simulation.show_plot = on,
            Some(PanelAction::ApplyMaterial(i)) => {
                let Some(g) = groups.get(i) else {
                    return;
                };
                let values = self.simulation.study.overrides.get(&g.key).copied();
                if let (Some(values), Some(mut m)) = (values, self.doc.part.material().cloned()) {
                    m.mechanical = Some(values);
                    // One undo step; the results go stale with the document's revision.
                    self.set_part_material(Some(m), false);
                    self.simulation.study.overrides.remove(&g.key);
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
enum PanelAction {
    Run,
    Mesh,
    Cancel,
    /// An item picked: its faces selected.
    Select(usize),
    /// An item's faces replaced by the selection.
    UseSelection(usize),
    Delete(usize),
    ShowPlot(bool),
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
fn item_row(
    ui: &mut egui::Ui,
    index: usize,
    item: &mut StudyItem,
    selected: &mut Option<usize>,
    action: &mut Option<PanelAction>,
) {
    ui.horizontal(|ui| {
        let is_selected = *selected == Some(index);
        if ui
            .selectable_label(is_selected, &item.name)
            .on_hover_text("Select its faces in the model")
            .clicked()
        {
            *action = Some(PanelAction::Select(index));
        }
        ui.label(
            egui::RichText::new(format!("{} face(s)", item.faces.len()))
                .small()
                .weak(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("×").on_hover_text("Delete").clicked() {
                *action = Some(PanelAction::Delete(index));
            }
            if ui
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
    }
    if changed {
        action.get_or_insert(PanelAction::Edited);
    }
}

/// A material group's mechanical properties: what the study uses and where it comes
/// from. Typing a value makes it the study's own. *Change* gives the group's parts
/// another material of the library (`names`), *Apply to the material* keeps the typed
/// values in the part's material; either is returned for the group `index`.
fn material_rows(
    ui: &mut egui::Ui,
    overrides: &mut BTreeMap<String, Mechanical>,
    g: &MaterialGroup,
    index: usize,
    names: &[String],
) -> Option<PanelAction> {
    let mut action = None;
    let source = g.effective.map(|(_, s)| s);
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
        let note = match source {
            Some(PropsSource::Study) => "this study's",
            Some(PropsSource::Material) => "the material's",
            Some(PropsSource::Typical) => "typical values",
            None => "unknown",
        };
        ui.label(egui::RichText::new(note).small().weak());
        if source == Some(PropsSource::Study) {
            if ui
                .small_button("Reset")
                .on_hover_text("Back to the material's own properties")
                .clicked()
            {
                overrides.remove(&g.key);
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

fn paint_arrow(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, stroke: egui::Stroke) {
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
fn format_number(v: f64) -> String {
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

fn format_value(v: f64, unit: &str) -> String {
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
    study: oxyde_sim::Study,
    materials: Vec<Elastic>,
    names: Vec<String>,
    mode: RunMode,
    /// Look for contacts within this tolerance, mm.
    detect: Option<f64>,
}

/// The solids meshed by Netgen on a worker kernel, each on its own, into one mesh whose
/// boundary facets carry the faces' labels; the copies are freed whatever happens. First
/// order from Netgen, made ten-node by `TetMesh::make_quadratic` when asked: the solver's
/// ten-node elements are straight-sided.
fn netgen_mesh(
    inputs: &[NetgenInput],
    h: f64,
    quadratic: bool,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
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
    for (k, input) in inputs.iter().enumerate() {
        if result.is_ok() {
            if cancel.load(Ordering::Relaxed) {
                result = Err(VolumeMeshError::Failed("cancelled".to_owned()));
            } else {
                match kernel.volume_mesh(input.shape, &options) {
                    Ok(v) => append_volume_mesh(&mut mesh, &v, input),
                    Err(e) => result = Err(e),
                }
            }
        }
        kernel.release(input.shape);
        progress((k + 1) as f64 / inputs.len().max(1) as f64);
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
        mut study,
        materials,
        names,
        mode,
        detect,
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
            &|f| report_progress("Meshing (Netgen)", f * span),
        ) {
            Ok(mesh) => netgen_result = Some(mesh),
            Err(VolumeMeshError::Unsupported) => {
                note = Some("this Oxyde was built without Netgen".to_owned());
            }
            Err(VolumeMeshError::Failed(why)) => note = Some(format!("Netgen failed: {why}")),
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
            .map_err(|e| format!("Meshing failed: {e}"))?;
            (mesh, report, "lattice")
        }
    };
    let mesh_seconds = start.elapsed().as_secs_f64();
    let candidates = detect.map(|tolerance| oxyde_sim::detect_contacts(&mesh, tolerance));
    if mode == RunMode::Run
        && let Some(found) = &candidates
    {
        // Run's own search: what it finds is bonded, as listed afterwards.
        study.contacts = found
            .iter()
            .map(|c| c.contact(ContactKind::Bonded))
            .collect();
    }
    if mesh_only {
        return Ok(Outcome {
            mesh,
            report,
            results: None,
            mesh_seconds,
            solve_seconds: 0.0,
            candidates,
            mesher,
            note,
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
    let results = oxyde_sim::solve(
        &mesh,
        &materials,
        &study,
        &SolveOptions::default(),
        cancel,
        &|f| report_progress("Solving", span + (1.0 - span) * f),
    )
    .map_err(|e| match e {
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
    })?;
    Ok(Outcome {
        mesh,
        report,
        results: Some(results),
        mesh_seconds,
        solve_seconds: start.elapsed().as_secs_f64(),
        candidates,
        mesher,
        note,
    })
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
        let names: Vec<&str> = study.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Fixed-1", "Pressure-1", "Fixed-2"]);
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
