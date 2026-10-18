//! Reading and writing Oxyde documents: parts (`.oxyprt`, formerly `.oxy`), assemblies
//! (`.oxyasm`), simulation studies (`.oxysim`, [`simulation`]) and their results (`.oxyres`,
//! [`results`]); drawings (`.oxydrw`) come later.
//!
//! Every document is a ZIP archive starting with `manifest.json`, which says what kind of
//! document it is. A part holds `model.json` (the feature timeline, the source of truth),
//! `properties.json` (BOM-ready metadata) and, for import features, the imported files
//! under `imports/`. Geometry is never stored as the source of truth: it is rebuilt from
//! the timeline on load. An assembly holds `assembly.json`: its components (part files by
//! relative path and document id, with their placements) and mates.
//!
//! See `docs/FORMAT.md` for the specification.

// Never crash: a panic in a modelling or UI path would take the unsaved document with it.
// Return an error or fall back to a safe default instead (docs/ARCHITECTURE.md, "Never crash").
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]
#![warn(clippy::indexing_slicing)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing
    )
)]

mod assembly_schema;
pub mod gltf;
pub mod material_library;
pub mod obj;
pub mod results;
mod schema;
pub mod simulation;
pub mod step_import;
pub mod stl;
pub mod threemf;

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Cursor, Read, Seek, Write};
use std::path::Path;

use std::collections::HashMap;

use oxyde_assembly::Assembly;
use oxyde_model::{FeatureKind, ModelError, Part, PartDefinition};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use assembly_schema::AssemblyModel;
pub use schema::Properties;
use schema::{Manifest, PartModel, TextureTable, Units, import_entry};

/// The `format_version` written by this crate, and the newest it can read.
pub const FORMAT_VERSION: u32 = 1;

const FORMAT_NAME: &str = "oxyde";
const MANIFEST: &str = "manifest.json";
const MODEL: &str = "model.json";
const PROPERTIES: &str = "properties.json";
const ASSEMBLY: &str = "assembly.json";

/// File extension of parts.
pub const PART_EXTENSION: &str = "oxyprt";
/// File extension of parts saved before assemblies existed; still opened.
pub const LEGACY_PART_EXTENSION: &str = "oxy";
/// File extension of assemblies.
pub const ASSEMBLY_EXTENSION: &str = "oxyasm";
/// File extension reserved for drawings.
pub const DRAWING_EXTENSION: &str = "oxydrw";

/// What an Oxyde file holds, from its manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Part,
    Assembly,
    Drawing,
    /// Simulation studies on a part or an assembly (`.oxysim`).
    Simulation,
    /// The results of a simulation study (`.oxyres`).
    SimulationResults,
}

impl DocumentKind {
    fn name(self) -> &'static str {
        match self {
            DocumentKind::Part => "part",
            DocumentKind::Assembly => "assembly",
            DocumentKind::Drawing => "drawing",
            DocumentKind::Simulation => "simulation",
            DocumentKind::SimulationResults => "simulation_results",
        }
    }
}

/// A part document as stored on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct PartFile {
    /// Never changes for the life of the document; assemblies find parts by it.
    pub document_id: String,
    pub definition: PartDefinition,
    pub properties: Properties,
}

impl PartFile {
    /// A new, empty part with a fresh document id.
    pub fn new() -> Self {
        Self {
            document_id: new_document_id(),
            definition: Part::new().definition(),
            properties: Properties::default(),
        }
    }
}

impl Default for PartFile {
    fn default() -> Self {
        Self::new()
    }
}

/// A fresh random document id (UUID v4).
pub fn new_document_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug)]
pub enum FormatError {
    Io(io::Error),
    /// Not a readable ZIP archive, or an entry is missing.
    Archive(zip::result::ZipError),
    /// An entry is not valid JSON for this format.
    Json {
        entry: &'static str,
        source: serde_json::Error,
    },
    /// The archive is not an Oxyde document.
    NotOxyde,
    /// Written by a newer version of Oxyde.
    UnsupportedVersion {
        found: u32,
        supported: u32,
    },
    /// An Oxyde document of another kind than the one expected.
    WrongKind {
        expected: &'static str,
        found: String,
    },
    /// The timeline in the file is inconsistent.
    InvalidModel(ModelError),
    /// The assembly in the file is inconsistent.
    InvalidAssembly(String),
    /// A material library file is inconsistent.
    InvalidLibrary(String),
    /// A simulation study file is inconsistent.
    InvalidSimulation(String),
    /// A simulation results file is inconsistent: a binary entry of the wrong length, an
    /// index out of range.
    InvalidResults(String),
    /// What would be saved does not read back (nothing was written).
    Unreadable(Box<FormatError>),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::Io(e) => write!(f, "{e}"),
            FormatError::Archive(e) => write!(f, "not a valid Oxyde archive: {e}"),
            FormatError::Json { entry, source } => write!(f, "{entry}: {source}"),
            FormatError::NotOxyde => write!(f, "not an Oxyde document"),
            FormatError::UnsupportedVersion { found, supported } => write!(
                f,
                "this file uses format version {found}; this version of Oxyde reads up to {supported}"
            ),
            FormatError::WrongKind { expected, found } => {
                write!(f, "expected a {expected}, found a {found} document")
            }
            FormatError::InvalidModel(e) => write!(f, "{e}"),
            FormatError::InvalidAssembly(e) => write!(f, "invalid assembly: {e}"),
            FormatError::InvalidLibrary(e) => write!(f, "invalid material library: {e}"),
            FormatError::InvalidSimulation(e) => write!(f, "invalid simulation study: {e}"),
            FormatError::InvalidResults(e) => write!(f, "invalid simulation results: {e}"),
            FormatError::Unreadable(e) => {
                write!(f, "the file would not open again, so it was not saved: {e}")
            }
        }
    }
}

impl std::error::Error for FormatError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FormatError::Io(e) => Some(e),
            FormatError::Archive(e) => Some(e),
            FormatError::Json { source, .. } => Some(source),
            FormatError::InvalidModel(e) => Some(e),
            FormatError::Unreadable(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<io::Error> for FormatError {
    fn from(e: io::Error) -> Self {
        FormatError::Io(e)
    }
}

impl From<zip::result::ZipError> for FormatError {
    fn from(e: zip::result::ZipError) -> Self {
        FormatError::Archive(e)
    }
}

fn to_json<T: serde::Serialize>(entry: &'static str, value: &T) -> Result<Vec<u8>, FormatError> {
    serde_json::to_vec_pretty(value).map_err(|source| FormatError::Json { entry, source })
}

fn from_json<T: serde::de::DeserializeOwned>(
    entry: &'static str,
    bytes: &[u8],
) -> Result<T, FormatError> {
    serde_json::from_slice(bytes).map_err(|source| FormatError::Json { entry, source })
}

fn manifest(kind: DocumentKind, document_id: &str) -> Manifest {
    Manifest {
        format: FORMAT_NAME.to_owned(),
        format_version: FORMAT_VERSION,
        kind: kind.name().to_owned(),
        document_id: document_id.to_owned(),
        units: Units {
            length: "mm".to_owned(),
            angle: "deg".to_owned(),
        },
        created_by: format!("oxyde {}", env!("CARGO_PKG_VERSION")),
    }
}

/// Reads and checks the manifest: an Oxyde document this version can read.
fn read_manifest<R: Read + Seek>(archive: &mut ZipArchive<R>) -> Result<Manifest, FormatError> {
    let manifest: Manifest =
        from_json(MANIFEST, &read_entry(archive, MANIFEST)?).map_err(|e| match e {
            // Something that is not even shaped like our manifest is not an Oxyde file.
            FormatError::Json { .. } => FormatError::NotOxyde,
            other => other,
        })?;
    if manifest.format != FORMAT_NAME {
        return Err(FormatError::NotOxyde);
    }
    if manifest.format_version > FORMAT_VERSION {
        return Err(FormatError::UnsupportedVersion {
            found: manifest.format_version,
            supported: FORMAT_VERSION,
        });
    }
    Ok(manifest)
}

fn expect_kind(manifest: &Manifest, kind: DocumentKind) -> Result<(), FormatError> {
    if manifest.kind == kind.name() {
        Ok(())
    } else {
        Err(FormatError::WrongKind {
            expected: kind.name(),
            found: manifest.kind.clone(),
        })
    }
}

/// What kind of document `path` is, and its document id, reading only the manifest.
pub fn document_kind(path: &Path) -> Result<(DocumentKind, String), FormatError> {
    let mut archive = ZipArchive::new(BufReader::new(File::open(path)?))?;
    let manifest = read_manifest(&mut archive)?;
    let kind = match manifest.kind.as_str() {
        "part" => DocumentKind::Part,
        "assembly" => DocumentKind::Assembly,
        "drawing" => DocumentKind::Drawing,
        "simulation" => DocumentKind::Simulation,
        "simulation_results" => DocumentKind::SimulationResults,
        other => {
            return Err(FormatError::WrongKind {
                expected: "part, assembly, drawing, simulation or simulation results",
                found: other.to_owned(),
            });
        }
    };
    Ok((kind, manifest.document_id))
}

/// Writes a part document as a ZIP archive (`.oxyprt`).
pub fn write_part<W: Write + Seek>(writer: W, part: &PartFile) -> Result<W, FormatError> {
    let manifest = manifest(DocumentKind::Part, &part.document_id);

    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut zip = ZipWriter::new(writer);
    let textures = TextureTable::new(part_textures(&part.definition));
    // The manifest goes first so readers can identify the file cheaply.
    for (entry, bytes) in [
        (MANIFEST, to_json(MANIFEST, &manifest)?),
        (MODEL, part_model_json(part, &textures)?),
        (PROPERTIES, to_json(PROPERTIES, &part.properties)?),
    ] {
        zip.start_file(entry, options)?;
        zip.write_all(&bytes)?;
    }
    // Imported files, as they were read.
    for feature in &part.definition.features {
        if let FeatureKind::Import(import) = feature.kind() {
            zip.start_file(import_entry(feature.id()), options)?;
            zip.write_all(import.data.bytes())?;
        }
    }
    write_textures(&mut zip, &textures, options)?;
    Ok(zip.finish()?)
}

/// `model.json` of a part, as saved.
fn part_model_json(part: &PartFile, textures: &TextureTable) -> Result<Vec<u8>, FormatError> {
    to_json(
        MODEL,
        &PartModel::from_definition(&part.definition, textures),
    )
}

/// `model.json` of a part, exactly as [`write_part`] saves it, for its fingerprint.
pub(crate) fn model_json(part: &PartFile) -> Result<Vec<u8>, FormatError> {
    part_model_json(part, &TextureTable::new(part_textures(&part.definition)))
}

/// Texture images are already compressed: stored as they are.
/// Every texture image a part uses: in its appearances and in its material.
fn part_textures(definition: &PartDefinition) -> Vec<&oxyde_model::Texture> {
    let mut out = definition.appearances.textures();
    if let Some(t) = definition
        .material
        .as_ref()
        .and_then(|m| m.appearance.texture.as_ref())
    {
        if !out.iter().any(|o| o.data == t.data) {
            out.push(t);
        }
    }
    out
}

fn write_textures<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    textures: &TextureTable,
    options: SimpleFileOptions,
) -> Result<(), FormatError> {
    for (name, bytes) in textures.entries() {
        zip.start_file(name, options.compression_method(CompressionMethod::Stored))?;
        zip.write_all(bytes)?;
    }
    Ok(())
}

/// The archive's binary entries (imported files, texture images) by name.
fn read_files<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
) -> Result<HashMap<String, Vec<u8>>, FormatError> {
    let names: Vec<String> = archive
        .file_names()
        .filter(|name| name.starts_with("imports/") || name.starts_with("textures/"))
        .map(str::to_owned)
        .collect();
    let mut files = HashMap::new();
    for name in names {
        let mut bytes = Vec::new();
        archive.by_name(&name)?.read_to_end(&mut bytes)?;
        files.insert(name, bytes);
    }
    Ok(files)
}

/// Reads a part document (`.oxyprt` or `.oxy`) and checks that its timeline is valid.
pub fn read_part<R: Read + Seek>(reader: R) -> Result<PartFile, FormatError> {
    let mut archive = ZipArchive::new(reader)?;
    let manifest = read_manifest(&mut archive)?;
    expect_kind(&manifest, DocumentKind::Part)?;

    let model: PartModel = from_json(MODEL, &read_entry(&mut archive, MODEL)?)?;
    let properties: Properties = from_json(PROPERTIES, &read_entry(&mut archive, PROPERTIES)?)?;
    let files = read_files(&mut archive)?;

    let definition = model
        .into_definition(&files)
        .map_err(|e| FormatError::InvalidModel(ModelError::InvalidDefinition(e)))?;
    // Validate now, so a broken file is reported at open time.
    let definition = Part::from_definition(definition)
        .map_err(FormatError::InvalidModel)?
        .definition();

    Ok(PartFile {
        document_id: manifest.document_id,
        definition,
        properties,
    })
}

fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &'static str,
) -> Result<Vec<u8>, FormatError> {
    let mut entry = archive.by_name(name)?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Saves a part to `path`. What is written is read back first: a part that would not open
/// again (a value that JSON cannot hold, an inconsistent timeline) is refused before the
/// previous version is touched. The file is then written next to the target, flushed to
/// disk and moved into place, so a failed save never destroys the previous version.
pub fn save_part(path: &Path, part: &PartFile) -> Result<(), FormatError> {
    let bytes = write_part(Cursor::new(Vec::new()), part)?.into_inner();
    read_part(Cursor::new(&bytes)).map_err(|e| FormatError::Unreadable(Box::new(e)))?;
    write_file(path, &bytes)
}

/// Saves an assembly to `path`, safely like [`save_part`].
pub fn save_assembly(path: &Path, assembly: &AssemblyFile) -> Result<(), FormatError> {
    let bytes = write_assembly(Cursor::new(Vec::new()), assembly)?.into_inner();
    read_assembly(Cursor::new(&bytes)).map_err(|e| FormatError::Unreadable(Box::new(e)))?;
    write_file(path, &bytes)
}

/// Writes `bytes` to `path` through a temporary file in the same folder, synced to disk
/// before it replaces the previous version.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), FormatError> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = std::path::PathBuf::from(temporary);

    let result = (|| {
        let mut file = BufWriter::new(File::create(&temporary)?);
        file.write_all(bytes)?;
        let file = file.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Loads a part from `path`.
pub fn load_part(path: &Path) -> Result<PartFile, FormatError> {
    read_part(BufReader::new(File::open(path)?))
}

/// An assembly document as stored on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct AssemblyFile {
    pub document_id: String,
    pub assembly: Assembly,
    pub properties: Properties,
}

impl AssemblyFile {
    /// A new, empty assembly with a fresh document id.
    pub fn new() -> Self {
        Self {
            document_id: new_document_id(),
            assembly: Assembly::new(),
            properties: Properties::default(),
        }
    }
}

impl Default for AssemblyFile {
    fn default() -> Self {
        Self::new()
    }
}

/// Writes an assembly document as a ZIP archive (`.oxyasm`).
pub fn write_assembly<W: Write + Seek>(
    writer: W,
    assembly: &AssemblyFile,
) -> Result<W, FormatError> {
    let manifest = manifest(DocumentKind::Assembly, &assembly.document_id);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut zip = ZipWriter::new(writer);
    let textures = assembly_textures(assembly);
    for (entry, bytes) in [
        (MANIFEST, to_json(MANIFEST, &manifest)?),
        (ASSEMBLY, assembly_model_json(assembly, &textures)?),
        (PROPERTIES, to_json(PROPERTIES, &assembly.properties)?),
    ] {
        zip.start_file(entry, options)?;
        zip.write_all(&bytes)?;
    }
    write_textures(&mut zip, &textures, options)?;
    Ok(zip.finish()?)
}

fn assembly_textures(assembly: &AssemblyFile) -> TextureTable {
    TextureTable::new(
        assembly
            .assembly
            .components
            .iter()
            .filter_map(|c| c.appearance.as_ref()?.texture.as_ref()),
    )
}

fn assembly_model_json(
    assembly: &AssemblyFile,
    textures: &TextureTable,
) -> Result<Vec<u8>, FormatError> {
    to_json(
        ASSEMBLY,
        &AssemblyModel::from_assembly(&assembly.assembly, textures),
    )
}

/// `assembly.json` of an assembly, exactly as [`write_assembly`] saves it, for its
/// fingerprint.
pub(crate) fn assembly_json(assembly: &AssemblyFile) -> Result<Vec<u8>, FormatError> {
    assembly_model_json(assembly, &assembly_textures(assembly))
}

/// Reads an assembly document and checks it: unique ids, mates between its components.
/// The part files it refers to are not opened.
pub fn read_assembly<R: Read + Seek>(reader: R) -> Result<AssemblyFile, FormatError> {
    let mut archive = ZipArchive::new(reader)?;
    let manifest = read_manifest(&mut archive)?;
    expect_kind(&manifest, DocumentKind::Assembly)?;
    let model: AssemblyModel = from_json(ASSEMBLY, &read_entry(&mut archive, ASSEMBLY)?)?;
    let has_properties = archive.file_names().any(|name| name == PROPERTIES);
    let properties: Properties = if has_properties {
        from_json(PROPERTIES, &read_entry(&mut archive, PROPERTIES)?)?
    } else {
        Properties::default()
    };
    let files = read_files(&mut archive)?;
    let assembly = model
        .into_assembly(&files)
        .map_err(FormatError::InvalidAssembly)?;
    Ok(AssemblyFile {
        document_id: manifest.document_id,
        assembly,
        properties,
    })
}

/// Loads an assembly from `path`.
pub fn load_assembly(path: &Path) -> Result<AssemblyFile, FormatError> {
    read_assembly(BufReader::new(File::open(path)?))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use oxyde_geom::Point2;
    use std::f64::consts::{PI, TAU};

    use oxyde_model::Sweep;
    use oxyde_model::{
        AxisRef, BodyId, CircularPattern, Combine, CombineOp, EdgeBlend, EdgeName, Extrude,
        ExtrudeExtent, ExtrudeOp, FaceKey, FaceName, FeatureKind, LinearPattern, Mirror,
        PatternDirection, PlaneRef, RefAxis, RefPlane, Revolve, RevolveAxis, Shell, Sketch,
        SketchPlane, StandardAxis, StandardPlane,
    };
    use oxyde_sketch::Constraint;

    use super::*;

    fn sample() -> PartFile {
        let mut part = Part::new();
        let mut base = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
        base.rectangle(Point2::new(-20.0, -10.0), 40.0, 20.0);
        let s1 = part.add_feature(FeatureKind::Sketch(base));
        let boss = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            10.0,
        )));

        let mut top = Sketch::new(SketchPlane::Face(FaceName::end_cap(boss)));
        let circle = top.geometry.circle(Point2::new(0.0, 0.0), 3.0);
        let guide = top.geometry.arc(
            Point2::new(6.0, 0.0),
            Point2::new(7.0, 0.0),
            Point2::new(6.0, 1.0),
        );
        top.geometry.set_construction(guide, true);
        let hole_radius = top.geometry.constrain(Constraint::Radius(circle, 3.0));
        top.geometry.rename_dimension(hole_radius, "Hole").unwrap();
        top.geometry
            .constrain(Constraint::EqualRadius(circle, guide));
        let s2 = part.add_feature(FeatureKind::Sketch(top));
        let hole = part.add_feature(FeatureKind::Extrude(Extrude {
            sketch: s2,
            op: ExtrudeOp::Cut,
            extent: ExtrudeExtent::MidPlane { depth: 4.0 },
            reverse: true,
            bodies: vec![BodyId::new(boss, 0)],
        }));

        let edge = EdgeName::between(FaceName::profile(boss, 2), FaceName::end_cap(boss));
        let round = part.add_feature(FeatureKind::Fillet(EdgeBlend {
            edges: vec![edge.clone()],
            size: 1.5,
        }));
        // A chamfer on an edge of the fillet face: nested names.
        let nested =
            EdgeName::between(FaceName::from_edge(round, edge), FaceName::profile(hole, 1));
        let bevel = part.add_feature(FeatureKind::Chamfer(EdgeBlend {
            edges: vec![nested],
            size: 0.5,
        }));
        part.set_suppressed(bevel, true).unwrap();

        // Reference geometry, a revolve, patterns, a mirror and a shell.
        let plane = part.add_feature(FeatureKind::Plane(RefPlane {
            base: PlaneRef::Standard(StandardPlane::Top),
            offset: 25.0,
            flip: false,
        }));
        let axis = part.add_feature(FeatureKind::Axis(RefAxis::TwoPlanes(
            PlaneRef::Standard(StandardPlane::Front),
            PlaneRef::Reference(plane),
        )));
        let mut profile = Sketch::new(SketchPlane::Reference(plane));
        let centerline = profile
            .geometry
            .line(Point2::new(0.0, -5.0), Point2::new(0.0, 5.0));
        profile.geometry.set_construction(centerline, true);
        profile.rectangle(Point2::new(2.0, -1.0), 3.0, 2.0);
        let s3 = part.add_feature(FeatureKind::Sketch(profile));
        let ring = part.add_feature(FeatureKind::Revolve(Revolve {
            sketch: s3,
            op: ExtrudeOp::NewBody,
            axis: RevolveAxis::SketchLine(centerline.0),
            angle: PI,
            mid_plane: true,
            reverse: false,
            bodies: Vec::new(),
        }));
        let row = part.add_feature(FeatureKind::LinearPattern(LinearPattern {
            seeds: vec![hole],
            first: PatternDirection {
                axis: AxisRef::Edge(EdgeName::between(
                    FaceName::profile(boss, 1),
                    FaceName::end_cap(boss),
                )),
                spacing: 8.0,
                count: 3,
                reverse: false,
            },
            second: Some(PatternDirection {
                axis: AxisRef::Standard(StandardAxis::Y),
                spacing: 5.0,
                count: 2,
                reverse: true,
            }),
        }));
        part.add_feature(FeatureKind::CircularPattern(CircularPattern {
            seeds: vec![ring],
            axis: AxisRef::Reference(axis),
            angle: TAU,
            count: 4,
            equal_spacing: true,
            reverse: false,
        }));
        part.add_feature(FeatureKind::Mirror(Mirror {
            seeds: vec![hole, ring],
            plane: PlaneRef::Face(FaceName::start_cap(boss)),
        }));
        // Bodies: the ring was made as a body of its own; combined into the first.
        part.add_feature(FeatureKind::Combine(Combine {
            target: BodyId::new(boss, 0),
            tools: vec![BodyId::new(ring, 0)],
            op: CombineOp::Join,
            keep_tools: true,
        }));
        // A sweep: a round profile along a path of another sketch.
        let mut round = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
        round.circle(Point2::new(40.0, 0.0), 1.0);
        let s4 = part.add_feature(FeatureKind::Sketch(round));
        let mut bend = Sketch::new(SketchPlane::Standard(StandardPlane::Top));
        bend.geometry
            .line(Point2::new(40.0, 0.0), Point2::new(40.0, -5.0));
        bend.geometry.arc(
            Point2::new(45.0, -5.0),
            Point2::new(40.0, -5.0),
            Point2::new(45.0, -10.0),
        );
        let s5 = part.add_feature(FeatureKind::Sketch(bend));
        part.add_feature(FeatureKind::Sweep(Sweep {
            profile: s4,
            path: s5,
            op: ExtrudeOp::NewBody,
            bodies: vec![BodyId::new(boss, 0)],
        }));
        let shell = part.add_feature(FeatureKind::Shell(Shell {
            faces: vec![FaceName::new(FaceKey::Instance {
                feature: row,
                instance: [1, 0],
                face: Box::new(FaceName::profile(hole, 1)),
            })],
            thickness: 1.0,
            outward: false,
        }));
        part.add_feature(FeatureKind::Fillet(EdgeBlend {
            edges: vec![EdgeName::between(
                FaceName::new(FaceKey::Offset {
                    feature: shell,
                    face: Box::new(FaceName::end_cap(boss)),
                }),
                FaceName::profile(boss, 2),
            )],
            size: 0.3,
        }));
        let depth = part.find_dimension("D1@Boss-Extrude1").unwrap();
        part.rename_dimension(depth, "Thickness").unwrap();
        // Equations, stored by name.
        let thick = part.parse_expression("12").unwrap();
        part.add_global("Thick", thick).unwrap();
        let expr = part.parse_expression("\"Thick\"").unwrap();
        part.add_equation(depth, expr).unwrap();
        let hole = part.find_dimension("Hole@Sketch2").unwrap();
        let expr = part
            .parse_expression("\"Thickness@Boss-Extrude1\" / 4")
            .unwrap();
        part.add_equation(hole, expr).unwrap();
        part.roll_back_to(Some(bevel)).unwrap();

        let mut properties = Properties {
            part_number: "OXY-0001".to_owned(),
            description: "Bracket".to_owned(),
            material: "Aluminium 6061-T6".to_owned(),
            volume_mm3: Some(7859.25),
            ..Properties::default()
        };
        properties
            .custom
            .insert("finish".to_owned(), "anodized".to_owned());

        PartFile {
            document_id: new_document_id(),
            definition: part.definition(),
            properties,
        }
    }

    fn round_trip(part: &PartFile) -> PartFile {
        let bytes = write_part(Cursor::new(Vec::new()), part)
            .unwrap()
            .into_inner();
        read_part(Cursor::new(bytes)).unwrap()
    }

    #[test]
    fn part_round_trips_exactly() {
        let part = sample();
        assert_eq!(round_trip(&part), part);
    }

    #[test]
    fn a_sketch_origin_is_stored_only_once_known() {
        // A sketch from a file older than the origin record, holding a point Convert
        // Entities fixed on a circle's rim and nothing yet at the origin. Saving must not
        // record that point as the origin: opening the sketch would move it there.
        let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
        let rim = sketch.geometry.point(Point2::new(5.0, 0.0));
        sketch.geometry.set_construction(rim, true);
        sketch.geometry.constrain(Constraint::Fixed(rim));
        let mut part = Part::new();
        part.add_feature(FeatureKind::Sketch(sketch));
        let file = PartFile {
            document_id: new_document_id(),
            definition: part.definition(),
            properties: Properties::default(),
        };
        let read = round_trip(&file);
        assert_eq!(read, file);
        let FeatureKind::Sketch(sketch) = read.definition.features[0].kind() else {
            panic!("not a sketch");
        };
        let mut geometry = sketch.geometry.clone();
        assert_ne!(geometry.ensure_origin(Point2::ORIGIN), rim);
        assert_eq!(geometry.position(rim), Some(Point2::new(5.0, 0.0)));
    }

    #[test]
    fn empty_part_round_trips() {
        let part = PartFile::new();
        assert_eq!(round_trip(&part), part);
    }

    #[test]
    fn manifest_comes_first_and_is_readable() {
        let bytes = write_part(Cursor::new(Vec::new()), &sample())
            .unwrap()
            .into_inner();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(archive.by_index(0).unwrap().name(), MANIFEST);
        let manifest: serde_json::Value =
            serde_json::from_slice(&read_entry(&mut archive, MANIFEST).unwrap()).unwrap();
        assert_eq!(manifest["format"], "oxyde");
        assert_eq!(manifest["format_version"], FORMAT_VERSION);
        assert_eq!(manifest["kind"], "part");
    }

    fn archive_with(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    const MANIFEST_V1: &str = r#"{"format":"oxyde","format_version":1,"kind":"part",
        "document_id":"x","units":{"length":"mm","angle":"deg"},"created_by":"hand"}"#;

    #[test]
    fn hand_written_file_with_defaults_is_accepted() {
        let model = r#"{
            "next_feature_id": 3,
            "features": [
                { "id": 1, "name": "Sketch1", "type": "sketch",
                  "plane": { "standard": "top" },
                  "entities": [
                    { "id": 1, "type": "point", "at": [0, 0] },
                    { "id": 2, "type": "circle", "center": 1, "radius": 5 } ],
                  "relations": [ { "id": 1, "type": "radius", "curve": 2, "value": 5 } ] },
                { "id": 2, "name": "Boss-Extrude1", "type": "extrude", "sketch": 1,
                  "operation": "boss", "extent": { "type": "blind", "depth": 2 } }
            ]
        }"#;
        let bytes = archive_with(&[(MANIFEST, MANIFEST_V1), (MODEL, model), (PROPERTIES, "{}")]);
        let part = read_part(Cursor::new(bytes)).unwrap();
        assert_eq!(part.definition.features.len(), 2);
        // Without stored names or counters, dimensions get default names on load.
        let loaded = Part::from_definition(part.definition.clone()).unwrap();
        let names: Vec<String> = loaded
            .dimensions()
            .into_iter()
            .map(|d| d.full_name)
            .collect();
        assert_eq!(names, ["D1@Sketch1", "D1@Boss-Extrude1"]);
        assert_eq!(part.properties, Properties::default());
    }

    #[test]
    fn equations_are_stored_as_text() {
        let bytes = write_part(Cursor::new(Vec::new()), &sample())
            .unwrap()
            .into_inner();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let model: serde_json::Value =
            serde_json::from_slice(&read_entry(&mut archive, MODEL).unwrap()).unwrap();
        assert_eq!(model["global_variables"][0]["name"], "Thick");
        assert_eq!(model["global_variables"][0]["expression"], "12");
        assert_eq!(
            model["equations"][0]["dimension"],
            "Thickness@Boss-Extrude1"
        );
        assert_eq!(model["equations"][0]["expression"], "\"Thick\"");
        assert_eq!(
            model["equations"][1]["expression"],
            "\"Thickness@Boss-Extrude1\" / 4"
        );
    }

    #[test]
    fn equations_with_syntax_errors_are_refused() {
        let model = r#"{ "next_feature_id": 1, "features": [],
            "equations": [ { "dimension": "D1@Sketch1", "expression": "2 *" } ] }"#;
        let bytes = archive_with(&[(MANIFEST, MANIFEST_V1), (MODEL, model), (PROPERTIES, "{}")]);
        assert!(matches!(
            read_part(Cursor::new(bytes)),
            Err(FormatError::InvalidModel(_))
        ));
    }

    #[test]
    fn newer_versions_are_refused() {
        let manifest = MANIFEST_V1.replace("\"format_version\":1", "\"format_version\":2");
        let bytes = archive_with(&[(MANIFEST, &manifest)]);
        assert!(matches!(
            read_part(Cursor::new(bytes)),
            Err(FormatError::UnsupportedVersion {
                found: 2,
                supported: 1
            })
        ));
    }

    #[test]
    fn foreign_archives_are_refused() {
        let bytes = archive_with(&[(MANIFEST, r#"{"hello":"world"}"#)]);
        assert!(matches!(
            read_part(Cursor::new(bytes)),
            Err(FormatError::NotOxyde)
        ));
        assert!(matches!(
            read_part(Cursor::new(b"not a zip".to_vec())),
            Err(FormatError::Archive(_))
        ));
    }

    #[test]
    fn inconsistent_timelines_are_refused() {
        let model = r#"{ "next_feature_id": 2, "features": [
            { "id": 1, "name": "Boss-Extrude1", "type": "extrude", "sketch": 9,
              "operation": "boss", "extent": { "type": "blind", "depth": 2 } } ] }"#;
        let bytes = archive_with(&[(MANIFEST, MANIFEST_V1), (MODEL, model), (PROPERTIES, "{}")]);
        assert!(matches!(
            read_part(Cursor::new(bytes)),
            Err(FormatError::InvalidModel(_))
        ));
    }

    #[test]
    fn sketches_with_bad_references_are_refused() {
        let model = r#"{ "next_feature_id": 2, "features": [
            { "id": 1, "name": "Sketch1", "type": "sketch", "plane": { "standard": "front" },
              "entities": [ { "id": 1, "type": "line", "start": 7, "end": 8 } ] } ] }"#;
        let bytes = archive_with(&[(MANIFEST, MANIFEST_V1), (MODEL, model), (PROPERTIES, "{}")]);
        match read_part(Cursor::new(bytes)) {
            Err(FormatError::InvalidModel(e)) => assert!(e.to_string().contains("Sketch1")),
            other => panic!("expected an invalid model, got {other:?}"),
        }
    }

    #[test]
    fn save_and_load_through_the_file_system() {
        let path = std::env::temp_dir().join(format!("oxyde-format-{}.oxy", std::process::id()));
        let part = sample();
        save_part(&path, &part).unwrap();
        let loaded = load_part(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, part);
    }

    #[test]
    fn a_part_that_would_not_open_again_is_not_saved() {
        let path = std::env::temp_dir().join(format!(
            "oxyde-format-unreadable-{}.oxyprt",
            std::process::id()
        ));
        let good = sample();
        save_part(&path, &good).unwrap();
        // JSON has no infinity: serde_json would write null, which does not read back.
        let mut part = Part::from_definition(good.definition.clone()).unwrap();
        let boss = part.features()[1].id();
        if let Ok(FeatureKind::Extrude(e)) = part.edit(boss) {
            e.extent = ExtrudeExtent::Blind {
                depth: f64::INFINITY,
            };
        }
        let bad = PartFile {
            definition: part.definition(),
            ..good.clone()
        };
        assert!(matches!(
            save_part(&path, &bad),
            Err(FormatError::Unreadable(_))
        ));
        // The previous version is untouched, and no temporary file is left behind.
        assert_eq!(load_part(&path).unwrap(), good);
        let mut temporary = path.clone().into_os_string();
        temporary.push(".tmp");
        assert!(!std::path::Path::new(&temporary).exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn placements_must_be_rotations() {
        let mut file = sample_assembly();
        file.assembly.components[1].placement.rotation[0][0] = 2.0;
        let bytes = write_assembly(Cursor::new(Vec::new()), &file)
            .unwrap()
            .into_inner();
        match read_assembly(Cursor::new(bytes)) {
            Err(FormatError::InvalidAssembly(e)) => assert!(e.contains("rotation"), "{e}"),
            other => panic!("expected an invalid assembly, got {other:?}"),
        }
    }

    fn sample_assembly() -> AssemblyFile {
        use oxyde_assembly::{MateKind, MateRef, MateTarget, Placement};
        use oxyde_geom::Point3;
        let mut file = AssemblyFile::new();
        let asm = &mut file.assembly;
        let base = asm.add_component(
            "Base",
            "parts/base.oxyprt".into(),
            "11111111-1111-1111-1111-111111111111".into(),
            Placement::IDENTITY,
        );
        let pin = asm.add_component(
            "Pin",
            "pin.oxyprt".into(),
            "22222222-2222-2222-2222-222222222222".into(),
            Placement::IDENTITY.moved(
                oxyde_geom::Vec3::new(0.1, 0.2, 0.3),
                Point3::ORIGIN,
                oxyde_geom::Vec3::new(1.0, 2.0, 3.0),
            ),
        );
        let feature = sample().definition.features[1].id();
        let face = FaceName::end_cap(feature);
        asm.add_mate(
            MateKind::Concentric,
            MateRef {
                component: base,
                path: Vec::new(),
                target: MateTarget::Face(face.clone()),
            },
            MateRef {
                component: pin,
                path: Vec::new(),
                target: MateTarget::Axis(StandardAxis::Z),
            },
            true,
        );
        asm.add_mate(
            MateKind::Distance(4.5),
            MateRef {
                component: base,
                path: Vec::new(),
                target: MateTarget::Plane(StandardPlane::Top),
            },
            MateRef {
                component: pin,
                path: Vec::new(),
                target: MateTarget::Origin,
            },
            false,
        );
        asm.add_mate(
            MateKind::Angle(0.5),
            MateRef {
                component: base,
                path: Vec::new(),
                target: MateTarget::Edge(EdgeName::between(face, FaceName::start_cap(feature))),
            },
            MateRef {
                component: pin,
                path: vec![
                    oxyde_assembly::ComponentId(3),
                    oxyde_assembly::ComponentId(1),
                ],
                target: MateTarget::Plane(StandardPlane::Right),
            },
            true,
        );
        file.properties.part_number = "ASM-1".into();
        file
    }

    #[test]
    fn assemblies_round_trip() {
        let file = sample_assembly();
        let bytes = write_assembly(Cursor::new(Vec::new()), &file)
            .unwrap()
            .into_inner();
        assert_eq!(read_assembly(Cursor::new(bytes.clone())).unwrap(), file);
        // A part reader refuses it, and says why.
        match read_part(Cursor::new(bytes)) {
            Err(FormatError::WrongKind { found, .. }) => assert_eq!(found, "assembly"),
            other => panic!("expected the wrong kind, got {other:?}"),
        }
    }

    #[test]
    fn material_round_trips_in_model_json() {
        use oxyde_model::{Appearance, FileData, Material, Texture};
        let mut file = sample();
        file.definition.material = Some(Material::new(
            "Oak",
            "Wood",
            Appearance {
                color: [170, 120, 70],
                transparency: 0.0,
                texture: Some(Texture {
                    file_name: "oak.png".into(),
                    data: FileData::new(b"PNG, not really".to_vec()),
                    scale: 80.0,
                }),
            },
            750.0,
        ));
        let bytes = write_part(Cursor::new(Vec::new()), &file)
            .unwrap()
            .into_inner();
        let mut archive = ZipArchive::new(Cursor::new(&bytes)).unwrap();
        let mut model = String::new();
        archive
            .by_name(MODEL)
            .unwrap()
            .read_to_string(&mut model)
            .unwrap();
        assert!(model.contains("\"material\": {"), "{model}");
        assert!(model.contains("\"density\": 750"), "{model}");
        assert!(model.contains("\"file\": \"textures/1.png\""), "{model}");
        drop(archive);
        let back = read_part(Cursor::new(&bytes)).unwrap();
        assert_eq!(back.definition.material, file.definition.material);

        // A part without a material writes none.
        let plain = sample();
        let bytes = write_part(Cursor::new(Vec::new()), &plain)
            .unwrap()
            .into_inner();
        let mut archive = ZipArchive::new(Cursor::new(&bytes)).unwrap();
        let mut model = String::new();
        archive
            .by_name(MODEL)
            .unwrap()
            .read_to_string(&mut model)
            .unwrap();
        assert!(!model.contains("material"));
    }

    #[test]
    fn appearances_and_textures_round_trip() {
        use oxyde_model::{Appearance, FileData, Texture};
        let mut file = sample();
        let boss = file.definition.features[1].id();
        let wood = Texture {
            file_name: "oak.PNG".into(),
            data: FileData::new(b"PNG, not really".to_vec()),
            scale: 80.0,
        };
        let steel = Texture {
            file_name: "brushed.jpg".into(),
            data: FileData::new(b"JPEG, not really".to_vec()),
            scale: 25.0,
        };
        let a = &mut file.definition.appearances;
        a.set_part(Some(Appearance {
            color: [200, 160, 90],
            transparency: 0.0,
            texture: Some(wood.clone()),
        }));
        a.set_body(
            BodyId::new(boss, 0),
            Some(Appearance {
                color: [120, 125, 130],
                transparency: 0.4,
                texture: Some(steel),
            }),
        );
        // The same image twice is stored once.
        a.set_face(
            &FaceName::end_cap(boss),
            Some(Appearance {
                color: [10, 20, 30],
                transparency: 0.0,
                texture: Some(Texture {
                    file_name: "copy of oak.png".into(),
                    ..wood
                }),
            }),
        );
        let bytes = write_part(Cursor::new(Vec::new()), &file)
            .unwrap()
            .into_inner();
        let mut archive = ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
        let mut names: Vec<&str> = archive
            .file_names()
            .filter(|n| n.starts_with("textures/"))
            .collect();
        names.sort();
        assert_eq!(names, ["textures/1.png", "textures/2.jpg"]);
        let model = String::from_utf8(read_entry(&mut archive, MODEL).unwrap()).unwrap();
        assert!(model.contains("\"file\": \"textures/1.png\""));
        assert!(model.contains("\"transparency\": 0.4"));
        assert_eq!(read_part(Cursor::new(bytes)).unwrap(), file);

        // A part without appearances writes none.
        let plain = write_part(Cursor::new(Vec::new()), &sample())
            .unwrap()
            .into_inner();
        let mut archive = ZipArchive::new(Cursor::new(plain)).unwrap();
        assert!(archive.file_names().all(|n| !n.starts_with("textures/")));
        let model = String::from_utf8(read_entry(&mut archive, MODEL).unwrap()).unwrap();
        assert!(!model.contains("appearances"));
    }

    #[test]
    fn component_appearances_round_trip() {
        use oxyde_model::{Appearance, FileData, Texture};
        let mut file = sample_assembly();
        file.assembly.components[1].appearance = Some(Appearance {
            color: [255, 0, 0],
            transparency: 0.25,
            texture: Some(Texture {
                file_name: "red.png".into(),
                data: FileData::new(vec![1, 2, 3, 4]),
                scale: 10.0,
            }),
        });
        let bytes = write_assembly(Cursor::new(Vec::new()), &file)
            .unwrap()
            .into_inner();
        let archive = ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
        assert!(archive.file_names().any(|n| n == "textures/1.png"));
        assert_eq!(read_assembly(Cursor::new(bytes)).unwrap(), file);
    }

    #[test]
    fn document_kind_reads_the_manifest() {
        let dir = std::env::temp_dir();
        let asm = dir.join(format!("oxyde-kind-{}.oxyasm", std::process::id()));
        let prt = dir.join(format!("oxyde-kind-{}.oxyprt", std::process::id()));
        let file = sample_assembly();
        save_assembly(&asm, &file).unwrap();
        save_part(&prt, &sample()).unwrap();
        let kinds = (document_kind(&asm), document_kind(&prt));
        let loaded = load_assembly(&asm);
        let _ = std::fs::remove_file(&asm);
        let _ = std::fs::remove_file(&prt);
        assert_eq!(
            kinds.0.unwrap(),
            (DocumentKind::Assembly, file.document_id.clone())
        );
        assert_eq!(kinds.1.unwrap().0, DocumentKind::Part);
        assert_eq!(loaded.unwrap(), file);
    }

    #[test]
    fn assemblies_with_dangling_mates_are_refused() {
        let manifest = MANIFEST_V1.replace("\"part\"", "\"assembly\"");
        let model = r#"{ "next_component_id": 2, "next_mate_id": 2,
            "components": [ { "id": 1, "name": "A<1>", "path": "a.oxyprt",
              "rotation": [[1,0,0],[0,1,0],[0,0,1]], "translation": [0,0,0], "fixed": true } ],
            "mates": [ { "id": 1, "name": "Coincident1", "type": "coincident",
              "a": { "component": 1, "origin": true }, "b": { "component": 7, "origin": true } } ] }"#;
        let bytes = archive_with(&[(MANIFEST, &manifest), (ASSEMBLY, model)]);
        match read_assembly(Cursor::new(bytes)) {
            Err(FormatError::InvalidAssembly(e)) => assert!(e.contains("component 7"), "{e}"),
            other => panic!("expected an invalid assembly, got {other:?}"),
        }
    }
}
