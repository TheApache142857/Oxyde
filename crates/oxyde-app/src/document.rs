//! The open part document: its timeline, metadata, file path and undo history.

use std::path::{Path, PathBuf};

use oxyde_format::{PartFile, Properties};
use oxyde_kernel::Kernel;
use oxyde_model::{FeatureId, Part, PartDefinition};

const UNDO_LIMIT: usize = 200;

/// A revision number no other state of any document has had: views caching what they show
/// by revision can never take a newly opened document for the one before it.
pub(crate) fn next_revision() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub struct Document {
    pub part: Part,
    pub properties: Properties,
    pub document_id: String,
    pub path: Option<PathBuf>,
    /// Name for a document not saved yet (a part made from a STEP file is named after it).
    pub untitled_name: Option<String>,
    /// Changed since the last save.
    pub modified: bool,
    /// Opened while another window edits the file: Save asks for another name.
    pub read_only: bool,
    /// Changed by every rebuild, so views know when to refresh; never the same for two
    /// documents (see [`next_revision`]).
    pub revision: u64,
    undo: Vec<PartDefinition>,
    redo: Vec<PartDefinition>,
    /// Feature currently edited in the properties panel: consecutive edits of the same
    /// feature (e.g. dragging a value) make a single undo step.
    editing: Option<FeatureId>,
}

impl Document {
    pub fn new() -> Self {
        Self::from_file(PartFile::new(), None).expect("an empty part is valid")
    }

    pub fn from_file(file: PartFile, path: Option<PathBuf>) -> Result<Self, String> {
        let part = Part::from_definition(file.definition).map_err(|e| e.to_string())?;
        Ok(Self {
            part,
            properties: file.properties,
            document_id: file.document_id,
            path,
            untitled_name: None,
            modified: false,
            read_only: false,
            revision: next_revision(),
            undo: Vec::new(),
            redo: Vec::new(),
            editing: None,
        })
    }

    pub fn to_file(&self) -> PartFile {
        PartFile {
            document_id: self.document_id.clone(),
            definition: self.part.definition(),
            properties: self.properties.clone(),
        }
    }

    /// File name without extension, or "Untitled".
    pub fn display_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_stem)
            .map(|s| s.to_string_lossy().into_owned())
            .or_else(|| self.untitled_name.clone())
            .unwrap_or_else(|| "Untitled".to_owned())
    }

    /// Forgets the undo and redo history (e.g. after opening a file).
    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.editing = None;
    }

    /// Records the current state before a change, as one undo step.
    pub fn checkpoint(&mut self) {
        self.push_undo(self.part.definition());
        self.editing = None;
    }

    /// Records `before`, the definition just before a change already made, as one undo step.
    pub fn checkpoint_from(&mut self, before: PartDefinition) {
        self.push_undo(before);
        self.editing = None;
    }

    /// Like [`Document::checkpoint`], but successive edits of the same feature share one step.
    pub fn checkpoint_edit(&mut self, feature: FeatureId) {
        if self.editing != Some(feature) {
            self.push_undo(self.part.definition());
            self.editing = Some(feature);
        }
        self.modified = true;
    }

    /// Ends the current continuous edit.
    pub fn end_edit(&mut self) {
        self.editing = None;
    }

    fn push_undo(&mut self, definition: PartDefinition) {
        self.undo.push(definition);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.modified = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo<K: Kernel>(&mut self, kernel: &mut K) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.part.definition());
        self.restore(previous, kernel);
        true
    }

    pub fn redo<K: Kernel>(&mut self, kernel: &mut K) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.part.definition());
        self.restore(next, kernel);
        true
    }

    fn restore<K: Kernel>(&mut self, definition: PartDefinition, kernel: &mut K) {
        // Snapshots come from valid parts, so this cannot fail in practice.
        if let Ok(part) = Part::from_definition(definition) {
            self.part.release(kernel);
            self.part = part;
        }
        self.editing = None;
        self.modified = true;
        self.rebuild(kernel);
    }

    pub fn rebuild<K: Kernel>(&mut self, kernel: &mut K) {
        self.part.rebuild(kernel);
        self.revision = next_revision();
    }

    /// Frees the kernel shapes held by this document.
    pub fn release<K: Kernel>(&mut self, kernel: &mut K) {
        self.part.release(kernel);
    }
}
