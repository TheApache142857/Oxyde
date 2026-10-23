//! What the server holds between tool calls: the part being modelled, the reference it is
//! compared with, the assembly, and the kernel that owns their shapes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oxyde_format::{PartFile, Properties, load_part, save_part};
use oxyde_geom::Transform;
use oxyde_kernel::{Kernel, ShapeHandle, Topology};
use oxyde_model::{FeatureId, FeatureKind, FeatureStatus, FileData, Import, Part, PartDefinition};
use oxyde_occt::OcctKernel;
use serde_json::{Value, json};

use crate::args::Set;
use crate::args::{self, Result};
use crate::assembly::AssemblyDoc;
use crate::geometry::{self, SolidRange, Solids};

/// Undo snapshots kept per part.
const UNDO_DEPTH: usize = 200;

/// The part being modelled.
pub(crate) struct PartDoc {
    /// Where it was opened from or last saved to; `None` until saved.
    pub path: Option<PathBuf>,
    pub document_id: String,
    pub properties: Properties,
    pub part: Part,
    /// Definitions before each change, newest last.
    pub undo: Vec<PartDefinition>,
    /// The labels given to sketch entities by the `sketch` tool, per sketch: label →
    /// entity id. Not saved: a reopened part is referred to by entity ids.
    pub labels: BTreeMap<FeatureId, BTreeMap<String, u32>>,
}

/// A part or a STEP file to reproduce: read-only, its solids moved by `transform` (to
/// centre it on the origin, say) as copies the session owns.
pub(crate) struct Reference {
    pub path: PathBuf,
    pub part: Part,
    pub topology: Topology,
    pub solids: Vec<SolidRange>,
}

impl Reference {
    pub fn solids(&self) -> Solids<'_> {
        Solids {
            solids: self.solids.clone(),
            topology: &self.topology,
            naming: None,
        }
    }
}

/// A server session.
pub struct Session {
    pub(crate) kernel: OcctKernel,
    pub(crate) part: Option<PartDoc>,
    pub(crate) reference: Option<Reference>,
    pub(crate) assembly: Option<AssemblyDoc>,
    /// Where pictures go when a tool is given no path.
    pub(crate) out_dir: PathBuf,
    pub(crate) pictures: u32,
    /// Working on an Oxyde window's document ([`Session::for_window`]).
    pub(crate) window: bool,
    /// The window's part as last given back, while lent part by part.
    pub(crate) window_part: Option<crate::window::WindowState>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            kernel: OcctKernel::new(),
            part: None,
            reference: None,
            assembly: None,
            out_dir: std::env::temp_dir().join("oxyde-mcp"),
            pictures: 0,
            window: false,
            window_part: None,
        }
    }

    pub(crate) fn doc(&self) -> Result<&PartDoc> {
        self.part
            .as_ref()
            .ok_or_else(|| "no part is open: use new_part or open_part first".to_owned())
    }

    pub(crate) fn doc_mut(&mut self) -> Result<&mut PartDoc> {
        self.part
            .as_mut()
            .ok_or_else(|| "no part is open: use new_part or open_part first".to_owned())
    }

    /// Replaces the open part, freeing the old one's shapes.
    pub(crate) fn set_part(&mut self, mut doc: PartDoc) {
        doc.part.rebuild(&mut self.kernel);
        if let Some(mut old) = self.part.replace(doc) {
            old.part.release(&mut self.kernel);
        }
    }

    pub(crate) fn new_part(&mut self, path: Option<PathBuf>, properties: Properties) {
        let file = PartFile::new();
        self.set_part(PartDoc {
            path,
            document_id: file.document_id,
            properties,
            part: Part::new(),
            undo: Vec::new(),
            labels: BTreeMap::new(),
        });
    }

    pub(crate) fn open_part(&mut self, path: &Path) -> Result<()> {
        let file = load_part(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let part = Part::from_definition(file.definition).map_err(|e| e.to_string())?;
        self.set_part(PartDoc {
            path: Some(path.to_path_buf()),
            document_id: file.document_id,
            properties: file.properties,
            part,
            undo: Vec::new(),
            labels: BTreeMap::new(),
        });
        Ok(())
    }

    pub(crate) fn save_part(&mut self, path: Option<PathBuf>) -> Result<PathBuf> {
        let doc = self.doc()?;
        let path = path
            .or_else(|| doc.path.clone())
            .ok_or_else(|| "the part has never been saved: give a path".to_owned())?;
        let volume = doc.part.body().map(|body| {
            body.solids
                .iter()
                .filter_map(|s| self.kernel.volume(s.shape).ok())
                .sum::<f64>()
        });
        let doc = self.doc_mut()?;
        doc.properties.volume_mm3 = volume;
        if let Some(material) = doc.part.material() {
            doc.properties.material = material.name.clone();
        }
        let file = PartFile {
            document_id: doc.document_id.clone(),
            definition: doc.part.definition(),
            properties: doc.properties.clone(),
        };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        save_part(&path, &file).map_err(|e| format!("{}: {e}", path.display()))?;
        doc.path = Some(path.clone());
        Ok(path)
    }

    /// Runs `change` on the open part as one undoable step, then rebuilds. A change that
    /// fails leaves the part as it was.
    pub(crate) fn change<T>(
        &mut self,
        change: impl FnOnce(&mut PartDoc) -> Result<T>,
    ) -> Result<T> {
        let doc = self.doc_mut()?;
        let before = doc.part.definition();
        let labels = doc.labels.clone();
        match change(doc) {
            Ok(value) => {
                doc.undo.push(before);
                if doc.undo.len() > UNDO_DEPTH {
                    doc.undo.remove(0);
                }
                self.rebuild();
                Ok(value)
            }
            Err(e) => {
                self.restore(before, labels);
                Err(e)
            }
        }
    }

    /// Puts a definition back (after a failed change, or for undo).
    pub(crate) fn restore(
        &mut self,
        definition: PartDefinition,
        labels: BTreeMap<FeatureId, BTreeMap<String, u32>>,
    ) {
        let Some(doc) = self.part.as_mut() else {
            return;
        };
        doc.part.release(&mut self.kernel);
        // The definition came from this part, so it is valid; else what is there stays.
        if let Ok(part) = Part::from_definition(definition) {
            doc.part = part;
        }
        doc.labels = labels;
        self.rebuild();
    }

    pub(crate) fn undo(&mut self) -> Result<()> {
        let doc = self.doc_mut()?;
        let previous = doc.undo.pop().ok_or_else(|| "nothing to undo".to_owned())?;
        let labels = doc.labels.clone();
        self.restore(previous, labels);
        Ok(())
    }

    pub(crate) fn rebuild(&mut self) {
        if let Some(doc) = self.part.as_mut() {
            doc.part.rebuild(&mut self.kernel);
        }
    }

    /// Opens a part (`.oxyprt`) or a STEP file as the reference, its solids moved by
    /// `transform`.
    pub(crate) fn open_reference(&mut self, path: &Path, transform: Transform) -> Result<()> {
        let is_step = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "step" | "stp"));
        let mut part = if is_step {
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut part = Part::new();
            part.add_feature(FeatureKind::Import(Import::new(name, FileData::new(bytes))));
            part
        } else {
            let file = load_part(path).map_err(|e| format!("{}: {e}", path.display()))?;
            Part::from_definition(file.definition).map_err(|e| e.to_string())?
        };
        part.rebuild(&mut self.kernel);
        let shapes: Vec<ShapeHandle> = part.body().map(|b| b.shapes()).unwrap_or_default();
        if shapes.is_empty() {
            part.release(&mut self.kernel);
            return Err(format!("{} has no solid", path.display()));
        }
        let mut placed = Vec::new();
        for shape in shapes {
            let result = self.kernel.transform(shape, &transform);
            let copy = match result {
                Ok(r) => r.shape,
                Err(e) => {
                    for (s, _) in placed {
                        self.kernel.release(s);
                    }
                    part.release(&mut self.kernel);
                    return Err(e.to_string());
                }
            };
            let topology = self.kernel.topology(copy).map_err(|e| e.to_string())?;
            placed.push((copy, topology));
        }
        let (topology, solids) = geometry::merge(&placed);
        self.close_reference();
        self.reference = Some(Reference {
            path: path.to_path_buf(),
            part,
            topology,
            solids,
        });
        Ok(())
    }

    pub(crate) fn close_reference(&mut self) {
        if let Some(mut old) = self.reference.take() {
            for solid in &old.solids {
                self.kernel.release(solid.shape);
            }
            old.part.release(&mut self.kernel);
        }
    }

    pub(crate) fn reference(&self) -> Result<&Reference> {
        self.reference
            .as_ref()
            .ok_or_else(|| "no reference is open: use open_reference first".to_owned())
    }

    /// Where the next picture goes when the tool is given no path.
    pub(crate) fn next_picture(&mut self, stem: &str) -> PathBuf {
        self.pictures += 1;
        self.out_dir.join(format!("{stem}-{}.png", self.pictures))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(mut doc) = self.part.take() {
            doc.part.release(&mut self.kernel);
        }
        self.close_reference();
        if let Some(assembly) = self.assembly.take() {
            assembly.release(&mut self.kernel);
        }
    }
}

/// Finds a feature by name (`"Boss-Extrude1"`, case-insensitive if no exact match) or id
/// (a number, or `"F7"`).
pub(crate) fn feature_id(part: &Part, value: &Value) -> Result<FeatureId> {
    if let Some(n) = value.as_u64() {
        let id = FeatureId::from_raw(n as u32);
        return part
            .feature(id)
            .map(|f| f.id())
            .ok_or_else(|| format!("no feature with id {n}"));
    }
    let name = value
        .as_str()
        .ok_or_else(|| "a feature is named by its name or its id".to_owned())?;
    if let Some(f) = part.features().iter().find(|f| f.name() == name) {
        return Ok(f.id());
    }
    if let Some(f) = part
        .features()
        .iter()
        .find(|f| f.name().eq_ignore_ascii_case(name))
    {
        return Ok(f.id());
    }
    if let Some(raw) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
        && let Some(f) = part.feature(FeatureId::from_raw(raw))
    {
        return Ok(f.id());
    }
    Err(format!("no feature named \"{name}\""))
}

/// The status of every feature that is not fine, and of `focus` whatever it is.
pub(crate) fn status_report(part: &Part, focus: &[FeatureId]) -> Value {
    let mut problems = Vec::new();
    let mut focused = Vec::new();
    for entry in part.timeline() {
        let (status, message) = match &entry.status {
            FeatureStatus::Ok => ("ok", None),
            FeatureStatus::Warning(m) => ("warning", Some(m.clone())),
            FeatureStatus::Error(m) => ("error", Some(m.clone())),
            FeatureStatus::Suppressed => ("suppressed", None),
            FeatureStatus::RolledBack => ("rolled back", None),
            FeatureStatus::Pending => ("pending", None),
        };
        let mut row = json!({ "feature": entry.name, "id": entry.id.raw(), "status": status });
        if let Some(message) = message {
            row.set("message", json!(message));
        }
        if entry.under_defined {
            row.set("under_defined", json!(true));
        }
        if focus.contains(&entry.id) {
            focused.push(row.clone());
        }
        if matches!(
            entry.status,
            FeatureStatus::Error(_) | FeatureStatus::Warning(_)
        ) && !focus.contains(&entry.id)
        {
            problems.push(row);
        }
    }
    let mut out = json!({ "features": focused });
    if !problems.is_empty() {
        out.set("other_problems", Value::Array(problems));
    }
    out
}

/// Volume, bounds and counts of the part's bodies, for tool results.
pub(crate) fn body_summary(kernel: &OcctKernel, part: &Part) -> Value {
    let Some(body) = part.body() else {
        return json!({ "bodies": 0 });
    };
    let shapes = body.shapes();
    let volume: f64 = shapes.iter().filter_map(|s| kernel.volume(*s).ok()).sum();
    let mut out = json!({
        "bodies": shapes.len(),
        "volume": args::round(volume),
        "faces": body.topology.faces.len(),
        "edges": body.topology.edges.len(),
    });
    if let Some((lo, hi)) = geometry::bounds(kernel, &shapes) {
        out.set(
            "bounds",
            json!({ "min": args::round3(lo), "max": args::round3(hi),
            "size": args::round_vec(hi - lo) }),
        );
    }
    out
}
