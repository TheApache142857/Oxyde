//! Simulation studies (`.oxysim`): what holds a part or an assembly, what loads it, how its
//! bodies touch, how it is meshed and which mechanical properties the study uses instead of
//! the materials' own.
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
use oxyde_model::{FaceName, Mechanical};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::schema::{FaceRefDto, MechanicalDto, face_dto, face_from};
use crate::{
    DocumentKind, FormatError, MANIFEST, expect_kind, from_json, manifest, new_document_id,
    read_entry, read_manifest, to_json, write_file,
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
}

impl Study {
    /// An empty static study.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            study_type: StudyType::Static,
            fixtures: Vec::new(),
            loads: Vec::new(),
            contacts: Vec::new(),
            mesh: MeshSettings::default(),
            materials: BTreeMap::new(),
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

/// Writes a simulation document as a ZIP archive (`.oxysim`).
pub fn write_simulation<W: Write + Seek>(
    writer: W,
    file: &SimulationFile,
) -> Result<W, FormatError> {
    let manifest = manifest(DocumentKind::Simulation, &file.document_id);
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
    #[serde(default)]
    mesh: MeshDto,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    materials: BTreeMap<String, MechanicalDto>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StudyTypeDto {
    Static,
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
        study_type: match study.study_type {
            StudyType::Static => StudyTypeDto::Static,
        },
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
        mesh: MeshDto {
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
        },
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

fn study_from(dto: StudyDto, kind: ModelKind) -> Result<Study, String> {
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
        };
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

    let size = match (dto.mesh.density, dto.mesh.element_size) {
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

    Ok(Study {
        name: dto.name,
        study_type: match dto.study_type {
            StudyTypeDto::Static => StudyType::Static,
        },
        fixtures,
        loads,
        contacts,
        mesh: MeshSettings {
            mesher: match dto.mesh.mesher {
                MesherDto::Netgen => Mesher::Netgen,
                MesherDto::Lattice => Mesher::Lattice,
            },
            quality: match dto.mesh.quality {
                QualityDto::High => MeshQuality::High,
                QualityDto::Draft => MeshQuality::Draft,
            },
            size,
        },
        materials,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FORMAT_VERSION;
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
        file.studies = vec![study, second];
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
                &format!("\"format_version\": {FORMAT_VERSION}"),
                &format!("\"format_version\": {}", FORMAT_VERSION + 1),
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
        refused(&|s| s.replace("2.5", "-1"), "element size");
        refused(&|s| s.replace("0.05", "0"), "tolerance");
        refused(
            &|s| s.replace("\"offset\": 0.4", "\"offset\": -0.4"),
            "offset",
        );
        refused(
            &|s| s.replace("\"direction\": \"normal\"", "\"direction\": [0, 0, 0]"),
            "zero vector",
        );
        // An unknown study or contact type is an error in version 1, as unknown features are.
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
