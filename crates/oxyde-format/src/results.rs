//! Simulation results (`.oxyres`): the mesh a study was solved on and the fields computed
//! at its nodes, kept apart from the study (`.oxysim`) so studies stay small and diff
//! cleanly while results can be thrown away and computed again.
//!
//! `results.json` says which study the results belong to, a fingerprint of the model they
//! were computed from (so stale results are detected when reopened), a summary, and
//! describes every binary entry (little-endian arrays) by name, type, count and components.
//!
//! See `docs/FORMAT.md` §9 for the specification.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::simulation::{Study, StudyFace, StudyFaceDto, study_face_dto, study_face_from};
use crate::{
    AssemblyFile, DocumentKind, FormatError, MANIFEST, PartFile, assembly_json, expect_kind,
    from_json, manifest, model_json, read_entry, read_manifest, to_json, write_file,
};

/// File extension of simulation results.
pub const RESULTS_EXTENSION: &str = "oxyres";

/// The label of a boundary facet on no face of the model.
pub const NO_LABEL: u32 = u32::MAX;

const RESULTS: &str = "results.json";

const NODES: &str = "mesh/nodes.bin";
const ELEMENTS: &str = "mesh/elements.bin";
const ELEMENT_BODIES: &str = "mesh/element_bodies.bin";
const FACET_ELEMENTS: &str = "mesh/facet_elements.bin";
const FACET_FACES: &str = "mesh/facet_faces.bin";
const FACET_LABELS: &str = "mesh/facet_labels.bin";
const DISPLACEMENT: &str = "fields/displacement.bin";
const STRESS: &str = "fields/stress.bin";
const VON_MISES: &str = "fields/von_mises.bin";
const YIELD_STRENGTH: &str = "fields/yield_strength.bin";
const CONTACT_PRESSURE: &str = "fields/contact_pressure.bin";

/// A results document as stored on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultsFile {
    /// The results document's own id.
    pub document_id: String,
    /// The document id of the `.oxysim` holding the study.
    pub simulation_id: String,
    /// The study's name in it.
    pub study: String,
    pub fingerprint: Fingerprint,
    pub summary: Summary,
    pub solver: SolverInfo,
    /// The solids of the mesh, indexed by [`ResultMesh::element_bodies`].
    pub bodies: Vec<ResultBody>,
    /// The model face each facet label stands for.
    pub labels: BTreeMap<u32, StudyFace>,
    pub mesh: ResultMesh,
    pub fields: NodalFields,
}

/// What the results were computed from, to tell whether they still hold.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Fingerprint {
    /// [`study_hash`] of the study as it was solved.
    pub study: String,
    /// Every part and assembly file the model is made of, sorted by document id.
    pub documents: Vec<DocumentHash>,
}

impl Fingerprint {
    /// A fingerprint with its documents in a stable order, each once.
    pub fn new(study: String, mut documents: Vec<DocumentHash>) -> Self {
        documents.sort();
        documents.dedup();
        Self { study, documents }
    }

    /// What changed between the model the results were computed from (`self`) and the
    /// model as it is now: the ids of the documents that changed, appeared or went away,
    /// sorted, and `"study"` first when the study itself changed. Empty: the results hold.
    pub fn changes(&self, now: &Fingerprint) -> Vec<String> {
        let then: BTreeMap<&str, &str> = self
            .documents
            .iter()
            .map(|d| (d.document_id.as_str(), d.hash.as_str()))
            .collect();
        let current: BTreeMap<&str, &str> = now
            .documents
            .iter()
            .map(|d| (d.document_id.as_str(), d.hash.as_str()))
            .collect();
        let ids: BTreeSet<&str> = then.keys().chain(current.keys()).copied().collect();
        let mut out = Vec::new();
        if self.study != now.study {
            out.push("study".to_owned());
        }
        out.extend(
            ids.into_iter()
                .filter(|id| then.get(id) != current.get(id))
                .map(str::to_owned),
        );
        out
    }
}

/// One document's share of a [`Fingerprint`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DocumentHash {
    pub document_id: String,
    /// [`content_hash`] of the document's `model.json` (a part) or `assembly.json` (an
    /// assembly: components, placements and mates) as Oxyde writes it.
    pub hash: String,
}

/// A stable hash of `bytes`, as text: `fnv1a128:` and 32 hex digits. The same bytes give
/// the same hash on every machine and in every version; it detects change, it does not
/// protect against tampering.
pub fn content_hash(bytes: &[u8]) -> String {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let hash = bytes.iter().fold(OFFSET, |hash, byte| {
        (hash ^ u128::from(*byte)).wrapping_mul(PRIME)
    });
    format!("fnv1a128:{hash:032x}")
}

/// The fingerprint of a part: its timeline as `model.json` holds it.
pub fn part_hash(part: &PartFile) -> Result<DocumentHash, FormatError> {
    Ok(DocumentHash {
        document_id: part.document_id.clone(),
        hash: content_hash(&model_json(part)?),
    })
}

/// The fingerprint of an assembly: its components, placements and mates as
/// `assembly.json` holds them. Its parts and sub-assemblies have their own.
pub fn assembly_hash(assembly: &AssemblyFile) -> Result<DocumentHash, FormatError> {
    Ok(DocumentHash {
        document_id: assembly.document_id.clone(),
        hash: content_hash(&assembly_json(assembly)?),
    })
}

/// The fingerprint of a study: its fixtures, loads, contacts, mesh settings and material
/// overrides as `simulation.json` holds them.
pub fn study_hash(study: &Study) -> Result<String, FormatError> {
    Ok(content_hash(&crate::simulation::study_json(study)?))
}

/// The headline numbers, each with the node where it is reached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// MPa.
    pub max_von_mises: Extreme,
    /// mm, the length of the displacement.
    pub max_displacement: Extreme,
    /// Absent when no material of the model has a known strength.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_factor_of_safety: Option<Extreme>,
}

/// A value and the node (an index into the mesh's nodes) where it is reached.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Extreme {
    pub value: f64,
    pub node: u32,
}

/// How the results were computed, for the record.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SolverInfo {
    /// The program and its version: `oxyde 0.0.6`.
    pub program: String,
    /// The mesher used: `netgen` or `lattice`.
    pub mesher: String,
    /// The solution method, free text (`pcg`, `pcg+amg`, `direct`…).
    pub method: String,
    #[serde(default)]
    pub iterations: u64,
    /// The relative residual reached.
    #[serde(default)]
    pub residual: f64,
    /// Wall time of meshing and solving.
    #[serde(default)]
    pub seconds: f64,
}

/// A solid of the mesh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultBody {
    /// As shown in the study tree: the body or the component.
    pub name: String,
    /// Its material group: a material name, or `part:` and a document id.
    pub material: String,
}

/// The tetrahedral mesh the study was solved on.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResultMesh {
    /// mm, world coordinates.
    pub nodes: Vec<[f64; 3]>,
    pub elements: Elements,
    /// The body of each element, an index into [`ResultsFile::bodies`].
    pub element_bodies: Vec<u32>,
    /// The element faces on the mesh's boundary.
    pub facets: Vec<ResultFacet>,
}

/// Elements, by node index. Corners are positively oriented: the fourth on the side the
/// first three wind counter-clockwise around.
#[derive(Debug, Clone, PartialEq)]
pub enum Elements {
    /// Four-node tetrahedra.
    Linear(Vec<[u32; 4]>),
    /// Ten-node tetrahedra: the four corners, then the middles of the edges 0-1, 1-2, 0-2,
    /// 0-3, 1-3 and 2-3.
    Quadratic(Vec<[u32; 10]>),
}

impl Default for Elements {
    fn default() -> Self {
        Elements::Linear(Vec::new())
    }
}

impl Elements {
    pub fn len(&self) -> usize {
        match self {
            Elements::Linear(e) => e.len(),
            Elements::Quadratic(e) => e.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 4 or 10.
    pub fn nodes_per_element(&self) -> usize {
        match self {
            Elements::Linear(_) => 4,
            Elements::Quadratic(_) => 10,
        }
    }

    fn nodes(&self) -> Box<dyn Iterator<Item = u32> + '_> {
        match self {
            Elements::Linear(e) => Box::new(e.iter().flatten().copied()),
            Elements::Quadratic(e) => Box::new(e.iter().flatten().copied()),
        }
    }
}

/// A face of an element on the mesh's boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultFacet {
    pub element: u32,
    /// The corner it is opposite, 0 to 3.
    pub face: u8,
    /// The model face it lies on (a key of [`ResultsFile::labels`]), or [`NO_LABEL`].
    pub label: u32,
}

/// The fields computed at the mesh's nodes, one value per node.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodalFields {
    /// mm.
    pub displacement: Vec<[f64; 3]>,
    /// MPa: xx, yy, zz, xy, yz, zx.
    pub stress: Vec<[f64; 6]>,
    /// MPa.
    pub von_mises: Vec<f64>,
    /// MPa: the strength the factor of safety is measured against at the node (the yield
    /// strength, else the tensile strength); 0 when unknown.
    pub yield_strength: Vec<f64>,
    /// MPa, normal pressure between bodies in contact; absent without contacts.
    pub contact_pressure: Option<Vec<f64>>,
}

/// Writes a results document as a ZIP archive (`.oxyres`), after checking it the way a
/// reader would.
pub fn write_results<W: Write + Seek>(writer: W, file: &ResultsFile) -> Result<W, FormatError> {
    validate(file).map_err(FormatError::InvalidResults)?;
    let mesh = &file.mesh;
    let fields = &file.fields;
    let mut entries: Vec<(EntryDto, Vec<u8>)> = vec![
        entry(
            NODES,
            Dtype::F64,
            3,
            mesh.nodes.len(),
            f64s(mesh.nodes.iter().flatten()),
        ),
        match &mesh.elements {
            Elements::Linear(e) => {
                entry(ELEMENTS, Dtype::U32, 4, e.len(), u32s(e.iter().flatten()))
            }
            Elements::Quadratic(e) => {
                entry(ELEMENTS, Dtype::U32, 10, e.len(), u32s(e.iter().flatten()))
            }
        },
        entry(
            ELEMENT_BODIES,
            Dtype::U32,
            1,
            mesh.element_bodies.len(),
            u32s(mesh.element_bodies.iter()),
        ),
        entry(
            FACET_ELEMENTS,
            Dtype::U32,
            1,
            mesh.facets.len(),
            u32s(mesh.facets.iter().map(|f| &f.element)),
        ),
        entry(
            FACET_FACES,
            Dtype::U8,
            1,
            mesh.facets.len(),
            mesh.facets.iter().map(|f| f.face).collect(),
        ),
        entry(
            FACET_LABELS,
            Dtype::U32,
            1,
            mesh.facets.len(),
            u32s(mesh.facets.iter().map(|f| &f.label)),
        ),
        entry(
            DISPLACEMENT,
            Dtype::F64,
            3,
            fields.displacement.len(),
            f64s(fields.displacement.iter().flatten()),
        ),
        entry(
            STRESS,
            Dtype::F64,
            6,
            fields.stress.len(),
            f64s(fields.stress.iter().flatten()),
        ),
        entry(
            VON_MISES,
            Dtype::F64,
            1,
            fields.von_mises.len(),
            f64s(fields.von_mises.iter()),
        ),
        entry(
            YIELD_STRENGTH,
            Dtype::F64,
            1,
            fields.yield_strength.len(),
            f64s(fields.yield_strength.iter()),
        ),
    ];
    if let Some(p) = &fields.contact_pressure {
        entries.push(entry(
            CONTACT_PRESSURE,
            Dtype::F64,
            1,
            p.len(),
            f64s(p.iter()),
        ));
    }

    let model = ResultsDto {
        simulation: file.simulation_id.clone(),
        study: file.study.clone(),
        fingerprint: file.fingerprint.clone(),
        units: ResultUnits::default(),
        summary: file.summary.clone(),
        solver: file.solver.clone(),
        bodies: file.bodies.clone(),
        labels: file
            .labels
            .iter()
            .map(|(label, face)| LabelDto {
                label: *label,
                face: study_face_dto(face),
            })
            .collect(),
        entries: entries.iter().map(|(e, _)| e.clone()).collect(),
    };

    let manifest = manifest(DocumentKind::SimulationResults, &file.document_id);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut zip = ZipWriter::new(writer);
    for (name, bytes) in [
        (MANIFEST, to_json(MANIFEST, &manifest)?),
        (RESULTS, to_json(RESULTS, &model)?),
    ] {
        zip.start_file(name, options)?;
        zip.write_all(&bytes)?;
    }
    for (e, bytes) in &entries {
        zip.start_file(e.name.as_str(), options)?;
        zip.write_all(bytes)?;
    }
    Ok(zip.finish()?)
}

/// Reads a results document and checks it: every binary entry present with the length its
/// description gives, every index in range.
pub fn read_results<R: Read + Seek>(reader: R) -> Result<ResultsFile, FormatError> {
    let mut archive = ZipArchive::new(reader)?;
    let manifest = read_manifest(&mut archive)?;
    expect_kind(&manifest, DocumentKind::SimulationResults)?;
    let dto: ResultsDto = from_json(RESULTS, &read_entry(&mut archive, RESULTS)?)?;
    let invalid = FormatError::InvalidResults;
    if dto.units != ResultUnits::default() {
        return Err(invalid(format!(
            "units {} {} {} (version 1 has mm, N and MPa only)",
            dto.units.length, dto.units.force, dto.units.stress
        )));
    }

    let mut described = BTreeMap::new();
    for e in &dto.entries {
        if described.insert(e.name.as_str(), e).is_some() {
            return Err(invalid(format!("{} is described twice", e.name)));
        }
    }
    let mut array = |name: &'static str,
                     dtype: Dtype,
                     components: &[u32]|
     -> Result<Option<(Vec<u8>, u32)>, FormatError> {
        let Some(e) = described.get(name) else {
            return Ok(None);
        };
        if e.dtype != dtype.name() || !components.contains(&e.components) {
            return Err(invalid(format!(
                "{name} is {} × {}, expected {} × {}",
                e.dtype,
                e.components,
                dtype.name(),
                components
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(" or ")
            )));
        }
        let expected = e
            .count
            .checked_mul(u64::from(e.components))
            .and_then(|n| n.checked_mul(dtype.size()));
        let bytes = match archive.by_name(name) {
            Ok(mut file) => {
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                bytes
            }
            Err(zip::result::ZipError::FileNotFound) => {
                return Err(invalid(format!("{name} is described but missing")));
            }
            Err(other) => return Err(other.into()),
        };
        if expected != Some(bytes.len() as u64) {
            return Err(invalid(format!(
                "{name} holds {} bytes, expected {} × {} {}",
                bytes.len(),
                e.count,
                e.components,
                dtype.name()
            )));
        }
        Ok(Some((bytes, e.components)))
    };
    let mut required = |name: &'static str, dtype: Dtype, components: &[u32]| {
        array(name, dtype, components)?.ok_or_else(|| invalid(format!("{name} is missing")))
    };

    let nodes = triples(&required(NODES, Dtype::F64, &[3])?.0);
    let elements = match required(ELEMENTS, Dtype::U32, &[4, 10])? {
        (bytes, 4) => Elements::Linear(fixed(&read_u32s(&bytes))),
        (bytes, _) => Elements::Quadratic(fixed(&read_u32s(&bytes))),
    };
    let element_bodies = read_u32s(&required(ELEMENT_BODIES, Dtype::U32, &[1])?.0);
    let facet_elements = read_u32s(&required(FACET_ELEMENTS, Dtype::U32, &[1])?.0);
    let facet_faces = required(FACET_FACES, Dtype::U8, &[1])?.0;
    let facet_labels = read_u32s(&required(FACET_LABELS, Dtype::U32, &[1])?.0);
    if facet_faces.len() != facet_elements.len() || facet_labels.len() != facet_elements.len() {
        return Err(invalid(
            "the facet arrays have different lengths".to_owned(),
        ));
    }
    let facets = facet_elements
        .iter()
        .zip(&facet_faces)
        .zip(&facet_labels)
        .map(|((&element, &face), &label)| ResultFacet {
            element,
            face,
            label,
        })
        .collect();
    let displacement = triples(&required(DISPLACEMENT, Dtype::F64, &[3])?.0);
    let stress = fixed(&read_f64s(&required(STRESS, Dtype::F64, &[6])?.0));
    let von_mises = read_f64s(&required(VON_MISES, Dtype::F64, &[1])?.0);
    let yield_strength = read_f64s(&required(YIELD_STRENGTH, Dtype::F64, &[1])?.0);
    let contact_pressure = array(CONTACT_PRESSURE, Dtype::F64, &[1])?.map(|(b, _)| read_f64s(&b));

    let mut labels = BTreeMap::new();
    for l in dto.labels {
        let face = study_face_from(l.face, None)
            .map_err(|e| invalid(format!("label {}: {e}", l.label)))?;
        if l.label == NO_LABEL || labels.insert(l.label, face).is_some() {
            return Err(invalid(format!(
                "label {} is reserved or used twice",
                l.label
            )));
        }
    }

    let file = ResultsFile {
        document_id: manifest.document_id,
        simulation_id: dto.simulation,
        study: dto.study,
        fingerprint: dto.fingerprint,
        summary: dto.summary,
        solver: dto.solver,
        bodies: dto.bodies,
        labels,
        mesh: ResultMesh {
            nodes,
            elements,
            element_bodies,
            facets,
        },
        fields: NodalFields {
            displacement,
            stress,
            von_mises,
            yield_strength,
            contact_pressure,
        },
    };
    validate(&file).map_err(invalid)?;
    Ok(file)
}

/// Saves a results document to `path` through a temporary file, like [`crate::save_part`].
pub fn save_results(path: &Path, file: &ResultsFile) -> Result<(), FormatError> {
    let bytes = write_results(Cursor::new(Vec::new()), file)?.into_inner();
    write_file(path, &bytes)
}

/// Loads a results document from `path`.
pub fn load_results(path: &Path) -> Result<ResultsFile, FormatError> {
    read_results(BufReader::new(File::open(path)?))
}

/// What a reader requires beyond the shapes of the arrays: consistent counts, indices in
/// range, summary values JSON can hold.
fn validate(file: &ResultsFile) -> Result<(), String> {
    let mesh = &file.mesh;
    let fields = &file.fields;
    let nodes = mesh.nodes.len();
    let elements = mesh.elements.len();
    if mesh.element_bodies.len() != elements {
        return Err(format!(
            "{} element bodies for {elements} elements",
            mesh.element_bodies.len()
        ));
    }
    for (name, count) in [
        ("displacement", fields.displacement.len()),
        ("stress", fields.stress.len()),
        ("von_mises", fields.von_mises.len()),
        ("yield_strength", fields.yield_strength.len()),
        (
            "contact_pressure",
            fields.contact_pressure.as_ref().map_or(nodes, Vec::len),
        ),
    ] {
        if count != nodes {
            return Err(format!("{count} {name} values for {nodes} nodes"));
        }
    }
    if mesh.elements.nodes().any(|n| n as usize >= nodes) {
        return Err(format!(
            "an element refers to a node past the {nodes} nodes"
        ));
    }
    if mesh
        .element_bodies
        .iter()
        .any(|&b| b as usize >= file.bodies.len())
    {
        return Err(format!(
            "an element belongs to a body past the {} bodies",
            file.bodies.len()
        ));
    }
    for f in &mesh.facets {
        if f.element as usize >= elements || f.face > 3 {
            return Err(format!(
                "facet {} of element {} is not in the mesh",
                f.face, f.element
            ));
        }
        if f.label != NO_LABEL && !file.labels.contains_key(&f.label) {
            return Err(format!("facet label {} is not in the label table", f.label));
        }
    }
    let summary = &file.summary;
    for (name, extreme) in [
        ("max_von_mises", Some(&summary.max_von_mises)),
        ("max_displacement", Some(&summary.max_displacement)),
        (
            "min_factor_of_safety",
            summary.min_factor_of_safety.as_ref(),
        ),
    ] {
        let Some(extreme) = extreme else { continue };
        if !extreme.value.is_finite() {
            return Err(format!("{name} is not a finite number"));
        }
        if extreme.node as usize >= nodes {
            return Err(format!(
                "{name} is at node {}, past the {nodes} nodes",
                extreme.node
            ));
        }
    }
    if !file.solver.residual.is_finite() || !file.solver.seconds.is_finite() {
        return Err("the solver's residual or time is not a finite number".to_owned());
    }
    Ok(())
}

// ---- results.json --------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ResultsDto {
    simulation: String,
    study: String,
    fingerprint: Fingerprint,
    units: ResultUnits,
    summary: Summary,
    solver: SolverInfo,
    #[serde(default)]
    bodies: Vec<ResultBody>,
    #[serde(default)]
    labels: Vec<LabelDto>,
    entries: Vec<EntryDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ResultUnits {
    length: String,
    force: String,
    stress: String,
}

impl Default for ResultUnits {
    fn default() -> Self {
        Self {
            length: "mm".to_owned(),
            force: "N".to_owned(),
            stress: "MPa".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LabelDto {
    label: u32,
    #[serde(flatten)]
    face: StudyFaceDto,
}

/// A binary entry's description.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct EntryDto {
    name: String,
    dtype: String,
    /// Records (nodes, elements, facets), each of `components` values.
    count: u64,
    components: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dtype {
    F64,
    U32,
    U8,
}

impl Dtype {
    fn name(self) -> &'static str {
        match self {
            Dtype::F64 => "f64",
            Dtype::U32 => "u32",
            Dtype::U8 => "u8",
        }
    }

    fn size(self) -> u64 {
        match self {
            Dtype::F64 => 8,
            Dtype::U32 => 4,
            Dtype::U8 => 1,
        }
    }
}

fn entry(
    name: &str,
    dtype: Dtype,
    components: u32,
    count: usize,
    bytes: Vec<u8>,
) -> (EntryDto, Vec<u8>) {
    (
        EntryDto {
            name: name.to_owned(),
            dtype: dtype.name().to_owned(),
            count: count as u64,
            components,
        },
        bytes,
    )
}

fn f64s<'a>(values: impl Iterator<Item = &'a f64>) -> Vec<u8> {
    values.flat_map(|v| v.to_le_bytes()).collect()
}

fn u32s<'a>(values: impl Iterator<Item = &'a u32>) -> Vec<u8> {
    values.flat_map(|v| v.to_le_bytes()).collect()
}

fn read_f64s(bytes: &[u8]) -> Vec<f64> {
    let (chunks, _) = bytes.as_chunks::<8>();
    chunks.iter().map(|c| f64::from_le_bytes(*c)).collect()
}

fn read_u32s(bytes: &[u8]) -> Vec<u32> {
    let (chunks, _) = bytes.as_chunks::<4>();
    chunks.iter().map(|c| u32::from_le_bytes(*c)).collect()
}

fn triples(bytes: &[u8]) -> Vec<[f64; 3]> {
    fixed(&read_f64s(bytes))
}

/// Groups of `N` values; the lengths were checked, so nothing is left over.
fn fixed<T: Copy, const N: usize>(values: &[T]) -> Vec<[T; N]> {
    values.as_chunks::<N>().0.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FORMAT_VERSION;
    use oxyde_assembly::ComponentId;
    use oxyde_model::{FaceName, FeatureId};

    /// Two linear tetrahedra sharing a face, the second of another body.
    fn sample() -> ResultsFile {
        let nodes = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
        ];
        let n = nodes.len();
        let mut labels = BTreeMap::new();
        labels.insert(
            0,
            StudyFace::component(
                ComponentId(1),
                vec![ComponentId(4)],
                FaceName::end_cap(FeatureId::from_raw(2)),
            ),
        );
        labels.insert(
            3,
            StudyFace::component(
                ComponentId(2),
                Vec::new(),
                FaceName::start_cap(FeatureId::from_raw(2)),
            ),
        );
        ResultsFile {
            document_id: crate::new_document_id(),
            simulation_id: "55555555-5555-5555-5555-555555555555".to_owned(),
            study: "Static 1".to_owned(),
            fingerprint: Fingerprint::new(
                "fnv1a128:00".to_owned(),
                vec![
                    DocumentHash {
                        document_id: "b".to_owned(),
                        hash: content_hash(b"b"),
                    },
                    DocumentHash {
                        document_id: "a".to_owned(),
                        hash: content_hash(b"a"),
                    },
                ],
            ),
            summary: Summary {
                max_von_mises: Extreme {
                    value: 212.5,
                    node: 4,
                },
                max_displacement: Extreme {
                    value: 0.125,
                    node: 3,
                },
                min_factor_of_safety: Some(Extreme {
                    value: 1.25,
                    node: 4,
                }),
            },
            solver: SolverInfo {
                program: "oxyde 0.0.6".to_owned(),
                mesher: "lattice".to_owned(),
                method: "pcg".to_owned(),
                iterations: 42,
                residual: 1e-9,
                seconds: 0.5,
            },
            bodies: vec![
                ResultBody {
                    name: "Base<1>".to_owned(),
                    material: "Aluminium 6061-T6".to_owned(),
                },
                ResultBody {
                    name: "Pin<1>".to_owned(),
                    material: "part:22222222-2222-2222-2222-222222222222".to_owned(),
                },
            ],
            labels,
            mesh: ResultMesh {
                nodes,
                elements: Elements::Linear(vec![[0, 1, 2, 3], [1, 2, 3, 4]]),
                element_bodies: vec![0, 1],
                facets: vec![
                    ResultFacet {
                        element: 0,
                        face: 3,
                        label: 0,
                    },
                    ResultFacet {
                        element: 1,
                        face: 0,
                        label: 3,
                    },
                    ResultFacet {
                        element: 1,
                        face: 1,
                        label: NO_LABEL,
                    },
                ],
            },
            fields: NodalFields {
                displacement: (0..n).map(|i| [i as f64, -0.5, 1e-3]).collect(),
                stress: (0..n)
                    .map(|i| [i as f64, 1.0, 2.0, 3.0, 4.0, -5.0])
                    .collect(),
                von_mises: (0..n).map(|i| 10.0 * i as f64).collect(),
                yield_strength: vec![275.0; n],
                contact_pressure: Some(vec![0.0, 0.5, 0.5, 0.5, 0.0]),
            },
        }
    }

    fn bytes(file: &ResultsFile) -> Vec<u8> {
        write_results(Cursor::new(Vec::new()), file)
            .unwrap()
            .into_inner()
    }

    /// `bytes` with one entry changed.
    fn edited(bytes: &[u8], entry: &str, edit: impl Fn(Vec<u8>) -> Vec<u8>) -> Vec<u8> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let name = file.name().to_owned();
            let mut content = Vec::new();
            file.read_to_end(&mut content).unwrap();
            if name == entry {
                content = edit(content);
            }
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(&content).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn text_edit(f: impl Fn(&str) -> String) -> impl Fn(Vec<u8>) -> Vec<u8> {
        move |b| f(std::str::from_utf8(&b).unwrap()).into_bytes()
    }

    #[test]
    fn results_round_trip() {
        let file = sample();
        assert_eq!(read_results(Cursor::new(bytes(&file))).unwrap(), file);

        // Ten-node elements, no contact pressure, no factor of safety.
        let mut quadratic = sample();
        quadratic.mesh.elements = Elements::Quadratic(vec![
            [0, 1, 2, 3, 4, 4, 4, 4, 4, 4],
            [1, 2, 3, 4, 0, 0, 0, 0, 0, 0],
        ]);
        quadratic.fields.contact_pressure = None;
        quadratic.summary.min_factor_of_safety = None;
        let written = bytes(&quadratic);
        assert_eq!(read_results(Cursor::new(&written)).unwrap(), quadratic);
        let archive = ZipArchive::new(Cursor::new(&written)).unwrap();
        assert!(archive.file_names().all(|n| n != CONTACT_PRESSURE));
    }

    #[test]
    fn the_layout_is_as_documented() {
        let written = bytes(&sample());
        let mut archive = ZipArchive::new(Cursor::new(&written)).unwrap();
        assert_eq!(archive.by_index(0).unwrap().name(), MANIFEST);
        assert_eq!(archive.by_index(1).unwrap().name(), RESULTS);
        let json: serde_json::Value =
            serde_json::from_slice(&read_entry(&mut archive, RESULTS).unwrap()).unwrap();
        assert_eq!(json["units"]["stress"], "MPa");
        assert_eq!(json["fingerprint"]["documents"][0]["document_id"], "a");
        assert_eq!(json["labels"][1]["label"], 3);
        assert_eq!(json["labels"][1]["component"], 2);
        assert_eq!(json["labels"][0]["path"], serde_json::json!([4]));
        let stress = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == STRESS)
            .unwrap();
        assert_eq!(stress["dtype"], "f64");
        assert_eq!(stress["count"], 5);
        assert_eq!(stress["components"], 6);
        // Little-endian, record after record.
        let mut nodes = Vec::new();
        archive
            .by_name(NODES)
            .unwrap()
            .read_to_end(&mut nodes)
            .unwrap();
        assert_eq!(nodes.len(), 5 * 3 * 8);
        assert_eq!(nodes[24..32], 1.0f64.to_le_bytes());
        let mut faces = Vec::new();
        archive
            .by_name(FACET_FACES)
            .unwrap()
            .read_to_end(&mut faces)
            .unwrap();
        assert_eq!(faces, [3, 0, 1]);
    }

    #[test]
    fn truncated_and_inconsistent_entries_are_refused() {
        let good = bytes(&sample());
        let refused = |bytes: Vec<u8>, why: &str| match read_results(Cursor::new(bytes)) {
            Err(FormatError::InvalidResults(e)) => assert!(e.contains(why), "{e}"),
            other => panic!("expected invalid results ({why}), got {other:?}"),
        };
        refused(
            edited(&good, VON_MISES, |mut b| {
                b.truncate(b.len() - 3);
                b
            }),
            "fields/von_mises.bin holds 37 bytes",
        );
        refused(
            edited(&good, ELEMENTS, |mut b| {
                b[0] = 9;
                b
            }),
            "past the 5 nodes",
        );
        refused(
            edited(&good, FACET_LABELS, |_| u32s([7u32, 7, 7].iter())),
            "label 7",
        );
        refused(
            edited(
                &good,
                RESULTS,
                text_edit(|s| s.replacen("\"components\": 3", "\"components\": 2", 1)),
            ),
            "mesh/nodes.bin is f64 × 2",
        );
        refused(
            edited(
                &good,
                RESULTS,
                text_edit(|s| s.replace("\"count\": 2,", "\"count\": 3,")),
            ),
            "holds",
        );
        // A described entry that is not in the archive.
        let mut archive = ZipArchive::new(Cursor::new(&good)).unwrap();
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            if file.name() == STRESS {
                continue;
            }
            let name = file.name().to_owned();
            let mut content = Vec::new();
            file.read_to_end(&mut content).unwrap();
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(&content).unwrap();
        }
        refused(zip.finish().unwrap().into_inner(), "described but missing");
    }

    #[test]
    fn unknown_entries_are_ignored() {
        let file = sample();
        let good = bytes(&file);
        let mut archive = ZipArchive::new(Cursor::new(&good)).unwrap();
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).unwrap();
            let name = entry.name().to_owned();
            let mut content = Vec::new();
            entry.read_to_end(&mut content).unwrap();
            if name == RESULTS {
                // A described entry of a later version, and an unknown key.
                content = String::from_utf8(content)
                    .unwrap()
                    .replacen(
                        "\"entries\": [",
                        "\"future\": 1, \"entries\": [ { \"name\": \"fields/strain.bin\", \"dtype\": \"f64\", \"count\": 5, \"components\": 6 },",
                        1,
                    )
                    .into_bytes();
            }
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(&content).unwrap();
        }
        zip.start_file("fields/strain.bin", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&[0; 5 * 6 * 8]).unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert_eq!(read_results(Cursor::new(bytes)).unwrap(), file);
    }

    #[test]
    fn newer_versions_and_other_kinds_are_refused() {
        let good = bytes(&sample());
        let newer = edited(
            &good,
            MANIFEST,
            text_edit(|m| {
                m.replace(
                    &format!("\"format_version\": {FORMAT_VERSION}"),
                    &format!("\"format_version\": {}", FORMAT_VERSION + 1),
                )
            }),
        );
        assert!(matches!(
            read_results(Cursor::new(newer)),
            Err(FormatError::UnsupportedVersion { .. })
        ));
        let other = edited(
            &good,
            MANIFEST,
            text_edit(|m| m.replace("\"simulation_results\"", "\"simulation\"")),
        );
        match read_results(Cursor::new(other)) {
            Err(FormatError::WrongKind { found, .. }) => assert_eq!(found, "simulation"),
            other => panic!("expected the wrong kind, got {other:?}"),
        }
        let units = edited(
            &good,
            RESULTS,
            text_edit(|s| s.replace("\"MPa\"", "\"Pa\"")),
        );
        assert!(matches!(
            read_results(Cursor::new(units)),
            Err(FormatError::InvalidResults(_))
        ));
    }

    #[test]
    fn what_a_reader_would_refuse_is_not_written() {
        let mut file = sample();
        file.summary.max_von_mises.value = f64::INFINITY;
        assert!(matches!(
            write_results(Cursor::new(Vec::new()), &file),
            Err(FormatError::InvalidResults(_))
        ));
        let mut file = sample();
        file.fields.von_mises.pop();
        assert!(matches!(
            write_results(Cursor::new(Vec::new()), &file),
            Err(FormatError::InvalidResults(_))
        ));
    }

    #[test]
    fn save_and_load_through_the_file_system() {
        let path =
            std::env::temp_dir().join(format!("oxyde-results-{}.oxyres", std::process::id()));
        let file = sample();
        save_results(&path, &file).unwrap();
        let loaded = load_results(&path);
        let kind = crate::document_kind(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.unwrap(), file);
        assert_eq!(kind.unwrap().0, DocumentKind::SimulationResults);
    }

    #[test]
    fn hashes_are_stable_and_detect_change() {
        // FNV-1a 128 of nothing is its offset basis; of "a", the published test vector.
        assert_eq!(
            content_hash(b""),
            "fnv1a128:6c62272e07bb014262b821756295c58d"
        );
        assert_eq!(
            content_hash(b"a"),
            "fnv1a128:d228cb696f1a8caf78912b704e4a8964"
        );

        let part = crate::PartFile::new();
        let first = part_hash(&part).unwrap();
        assert_eq!(part_hash(&part.clone()).unwrap(), first);
        // The hash is of model.json exactly as saved.
        let saved = crate::write_part(Cursor::new(Vec::new()), &part)
            .unwrap()
            .into_inner();
        let mut archive = ZipArchive::new(Cursor::new(saved)).unwrap();
        assert_eq!(
            first.hash,
            content_hash(&read_entry(&mut archive, crate::MODEL).unwrap())
        );

        let mut changed = part.clone();
        changed.definition.next_id += 1;
        let then = Fingerprint::new(String::new(), vec![first.clone()]);
        let now = Fingerprint::new(String::new(), vec![part_hash(&changed).unwrap()]);
        assert_eq!(then.changes(&then), Vec::<String>::new());
        assert_eq!(then.changes(&now), std::slice::from_ref(&part.document_id));

        let assembly = crate::AssemblyFile::new();
        let asm = assembly_hash(&assembly).unwrap();
        let both = Fingerprint::new("s".to_owned(), vec![asm.clone(), first.clone()]);
        assert_eq!(
            Fingerprint::new("t".to_owned(), vec![first]).changes(&both),
            ["study".to_owned(), asm.document_id]
        );

        let study = crate::simulation::Study::new("Static 1");
        let mut heavier = study.clone();
        heavier.mesh.quality = crate::simulation::MeshQuality::Draft;
        assert_eq!(study_hash(&study).unwrap(), study_hash(&study).unwrap());
        assert_ne!(study_hash(&study).unwrap(), study_hash(&heavier).unwrap());
    }
}
