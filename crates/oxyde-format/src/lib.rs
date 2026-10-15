//! Reading and writing Oxyde documents: parts (`.oxyprt`, formerly `.oxy`) and assemblies
//! (`.oxyasm`); drawings (`.oxydrw`) come later.
//!
//! Every document is a ZIP archive starting with `manifest.json`, which says what kind of
//! document it is. A part holds `model.json` (the feature timeline, the source of truth),
//! `properties.json` (BOM-ready metadata) and, for import features, the imported files
//! under `imports/`. Geometry is never stored as the source of truth: it is rebuilt from
//! the timeline on load. An assembly holds `assembly.json`: its components (part files by
//! relative path and document id, with their placements) and mates.
//!
//! See `docs/FORMAT.md` for the specification.

mod assembly_schema;
mod schema;

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::path::Path;

use std::collections::HashMap;

use oxyde_assembly::Assembly;
use oxyde_model::{FeatureKind, ModelError, Part, PartDefinition};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use assembly_schema::AssemblyModel;
pub use schema::Properties;
use schema::{Manifest, PartModel, Units, import_entry};

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
}

impl DocumentKind {
    fn name(self) -> &'static str {
        match self {
            DocumentKind::Part => "part",
            DocumentKind::Assembly => "assembly",
            DocumentKind::Drawing => "drawing",
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
        other => {
            return Err(FormatError::WrongKind {
                expected: "part, assembly or drawing",
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
    // The manifest goes first so readers can identify the file cheaply.
    for (entry, bytes) in [
        (MANIFEST, to_json(MANIFEST, &manifest)?),
        (
            MODEL,
            to_json(MODEL, &PartModel::from_definition(&part.definition))?,
        ),
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
    Ok(zip.finish()?)
}

/// Reads a part document (`.oxyprt` or `.oxy`) and checks that its timeline is valid.
pub fn read_part<R: Read + Seek>(reader: R) -> Result<PartFile, FormatError> {
    let mut archive = ZipArchive::new(reader)?;
    let manifest = read_manifest(&mut archive)?;
    expect_kind(&manifest, DocumentKind::Part)?;

    let model: PartModel = from_json(MODEL, &read_entry(&mut archive, MODEL)?)?;
    let properties: Properties = from_json(PROPERTIES, &read_entry(&mut archive, PROPERTIES)?)?;
    let mut files = HashMap::new();
    let imports: Vec<String> = archive
        .file_names()
        .filter(|name| name.starts_with("imports/"))
        .map(str::to_owned)
        .collect();
    for name in imports {
        let mut bytes = Vec::new();
        archive.by_name(&name)?.read_to_end(&mut bytes)?;
        files.insert(name, bytes);
    }

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

/// Saves a part to `path`. The file is written next to the target first and then moved
/// into place, so a failed save never destroys the previous version.
pub fn save_part(path: &Path, part: &PartFile) -> Result<(), FormatError> {
    save_with(path, |file| write_part(file, part))
}

/// Saves an assembly to `path`, safely like [`save_part`].
pub fn save_assembly(path: &Path, assembly: &AssemblyFile) -> Result<(), FormatError> {
    save_with(path, |file| write_assembly(file, assembly))
}

type Writer = BufWriter<File>;

fn save_with(
    path: &Path,
    write: impl FnOnce(Writer) -> Result<Writer, FormatError>,
) -> Result<(), FormatError> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = std::path::PathBuf::from(temporary);

    let result = (|| {
        let mut file = write(BufWriter::new(File::create(&temporary)?))?;
        file.flush()?;
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
    for (entry, bytes) in [
        (MANIFEST, to_json(MANIFEST, &manifest)?),
        (
            ASSEMBLY,
            to_json(ASSEMBLY, &AssemblyModel::from_assembly(&assembly.assembly))?,
        ),
        (PROPERTIES, to_json(PROPERTIES, &assembly.properties)?),
    ] {
        zip.start_file(entry, options)?;
        zip.write_all(&bytes)?;
    }
    Ok(zip.finish()?)
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
    let assembly = model
        .into_assembly()
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

    use oxyde_model::{
        AxisRef, CircularPattern, EdgeBlend, EdgeName, Extrude, ExtrudeExtent, ExtrudeOp, FaceKey,
        FaceName, FeatureKind, LinearPattern, Mirror, PatternDirection, PlaneRef, RefAxis,
        RefPlane, Revolve, RevolveAxis, Shell, Sketch, SketchPlane, StandardAxis, StandardPlane,
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
            op: ExtrudeOp::Boss,
            axis: RevolveAxis::SketchLine(centerline.0),
            angle: PI,
            mid_plane: true,
            reverse: false,
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
                target: MateTarget::Face(face.clone()),
            },
            MateRef {
                component: pin,
                target: MateTarget::Axis(StandardAxis::Z),
            },
            true,
        );
        asm.add_mate(
            MateKind::Distance(4.5),
            MateRef {
                component: base,
                target: MateTarget::Plane(StandardPlane::Top),
            },
            MateRef {
                component: pin,
                target: MateTarget::Origin,
            },
            false,
        );
        asm.add_mate(
            MateKind::Angle(0.5),
            MateRef {
                component: base,
                target: MateTarget::Edge(EdgeName::between(face, FaceName::start_cap(feature))),
            },
            MateRef {
                component: pin,
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
