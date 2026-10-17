//! Editing a component in place: a part or sub-assembly opened from the assembly that uses
//! it, with the rest of that assembly shown ghosted around it, in the component's own
//! coordinates. The assembly is set aside meanwhile (loaded, with its undo history and
//! selection) and File › Finish editing comes back to it, reading the edited files again.
//! A part at any depth can be opened straight from the top assembly (a part inside a
//! sub-assembly inside another), as SolidWorks does; editing can nest too, a sub-assembly
//! opened in place and one of its parts opened from there.
//!
//! Every document open for editing, set aside or not, holds an edit lock (see
//! [`crate::lockfile`]); a document another window is editing opens read-only, or is
//! taken over.

use std::path::{Path, PathBuf};

use eframe::egui;
use oxyde_kernel::Mesh;
use oxyde_render::Material;

use crate::app::{OxydeApp, PendingAction};
use crate::assembly::{AssemblyDoc, Occurrence};
use crate::dialogs::Dialog;
use crate::lockfile::LockInfo;

/// An assembly set aside while one of its components is edited.
pub(crate) struct Suspended {
    pub asm: AssemblyDoc,
    /// The component being edited: a top-level one, or one inside sub-assemblies.
    pub component: Occurrence,
    /// Its name as the tree reads it ("Wheel assembly / Axle"), for the banner and menus.
    pub component_name: String,
    /// The other components, in the edited component's coordinates, drawn ghosted.
    pub context: Mesh,
}

/// How a document is opened with respect to its edit lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockMode {
    /// Take the lock; ask when another window holds it.
    Lock,
    /// Another window holds the lock: open without one, and do not save over the file.
    ReadOnly,
    /// Take the lock from the other window.
    TakeOver,
}

/// What to open once the lock question is answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpenRequest {
    Path(PathBuf),
    Component(Occurrence),
}

/// The ghosted rest of the assembly.
fn ghost() -> Material {
    Material {
        color: Some([150, 160, 176]),
        opacity: 0.22,
        texture: None,
        texture_scale: 50.0,
    }
}

impl OxydeApp {
    /// Takes the edit lock on `path` as `mode` says. `Ok(read_only)` to go on;
    /// `Err(holder)` when another window edits the file and the user must choose.
    pub(crate) fn claim(&mut self, path: &Path, mode: LockMode) -> Result<bool, LockInfo> {
        match mode {
            LockMode::Lock => self.locks.acquire(path).map(|()| false),
            LockMode::ReadOnly => Ok(true),
            LockMode::TakeOver => {
                self.locks.force(path);
                Ok(false)
            }
        }
    }

    /// Asks what to do about a file another window is editing.
    pub(crate) fn ask_about_lock(&mut self, path: &Path, holder: LockInfo, request: OpenRequest) {
        self.open_dialog(Dialog::Locked {
            path: path.to_path_buf(),
            holder,
            request,
        });
    }

    /// Opens `request` as the lock dialog decided.
    pub(crate) fn open_locked(&mut self, request: OpenRequest, mode: LockMode) {
        match request {
            OpenRequest::Path(path) => self.open_path_with(&path, mode),
            OpenRequest::Component(o) => self.open_component_with(o, mode),
        }
    }

    /// Releases the locks of documents no longer open: the lock of the current document
    /// (unless read-only) and of every assembly set aside are kept.
    pub(crate) fn sync_locks(&mut self) {
        // A job holds a document meanwhile (an assembly being read again): its lock
        // stays until it is back.
        if self.busy() {
            return;
        }
        let mut keep: Vec<PathBuf> = self
            .edit_stack
            .iter()
            .filter(|s| !s.asm.read_only)
            .filter_map(|s| s.asm.path.clone())
            .collect();
        match &self.asm {
            Some(asm) if !asm.read_only => keep.extend(asm.path.clone()),
            Some(_) => {}
            None if !self.doc.read_only => keep.extend(self.doc.path.clone()),
            None => {}
        }
        self.locks.retain(&keep);
    }

    /// Opens a component's file for editing in place, at any depth: the assembly is set
    /// aside and the part or sub-assembly becomes the document, with everything else
    /// ghosted around it in that component's own coordinates.
    pub(crate) fn open_component_with(&mut self, o: Occurrence, mode: LockMode) {
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let Some(path) = doc.path_at(&o) else {
            self.status = "The component's file is not loaded".to_owned();
            return;
        };
        if matches!(doc.docs.get(&path), Some(Err(_)) | None) {
            self.status = format!("{} cannot be opened", path.display());
            return;
        }
        let read_only = match self.claim(&path, mode) {
            Ok(read_only) => read_only,
            Err(holder) => {
                self.ask_about_lock(&path, holder, OpenRequest::Component(o));
                return;
            }
        };
        let Some(parent) = self.asm.take() else {
            return;
        };
        let component_name = parent.occurrence_name(&o);
        let context = parent.context_mesh(&o);
        let parent_name = parent.display_name();
        self.edit_stack.push(Suspended {
            asm: parent,
            component: o,
            component_name: component_name.clone(),
            context,
        });
        // The assembly's panels and previews are reset; its parts stay loaded.
        self.leave_assembly();
        let mode = if read_only {
            LockMode::ReadOnly
        } else {
            LockMode::Lock
        };
        self.open_path_with(&path, mode);
        self.status = format!(
            "Editing {component_name} in place{}: File › Finish editing goes back to {parent_name}",
            if read_only { " (read-only)" } else { "" }
        );
    }

    /// The component being edited in place, if any: its instance name and the name of
    /// the assembly it is edited in.
    pub(crate) fn editing_in(&self) -> Option<(String, String)> {
        let s = self.edit_stack.last()?;
        Some((s.component_name.clone(), s.asm.display_name()))
    }

    /// Back to the assembly a component was opened from, reading its files again.
    pub(crate) fn finish_edit(&mut self, _ctx: &egui::Context) {
        let Some(suspended) = self.edit_stack.pop() else {
            return;
        };
        // The edited document stays on screen while the assembly's changed files (the
        // edited one, usually) are read again off the UI thread; `assembly_back` follows.
        self.sketch = None;
        self.close_dialog();
        self.assembly_back_job(suspended);
    }

    /// The assembly set aside is back, its changed files read again.
    pub(crate) fn assembly_back(
        &mut self,
        suspended: Suspended,
        notes: Vec<String>,
        ctx: &egui::Context,
    ) {
        if self.asm.is_some() {
            self.leave_assembly();
        } else {
            self.close_dialog();
            self.doc.release(&mut self.kernel);
            self.doc = crate::document::Document::new();
            self.sketch = None;
            self.mesh_export = None;
            self.selection.clear();
            self.selected_feature = None;
            self.selected_features.clear();
            self.selected_plane = None;
            self.scene_revision = None;
        }
        self.viewport.pickable = None;
        let parent = suspended.asm;
        let name = parent.display_name();
        // Still there after the reload: a component the user deleted from the sub-assembly
        // file is gone from `sources`, which `loaded` tolerates.
        self.asm = Some(parent);
        self.asm_scene = None;
        self.asm_scroll_to = Some(suspended.component.top);
        self.viewport.request_fit();
        self.status = match notes.first() {
            Some(note) => note.clone(),
            None => format!("Back to {name}"),
        };
        self.sync_locks();
        if let Some(next) = self.after_unwind.take() {
            self.guard(next, ctx);
        }
    }

    /// Frees the assemblies set aside, when everything is closed without finishing.
    pub(crate) fn drop_edit_stack(&mut self) {
        for mut s in self.edit_stack.drain(..) {
            s.asm.release(&mut self.kernel);
        }
        self.after_unwind = None;
    }

    /// Whether any open document, set aside or not, has unsaved changes.
    pub(crate) fn any_modified(&self) -> bool {
        self.is_modified() || self.edit_stack.iter().any(|s| s.asm.modified)
    }

    /// Appends the ghosted rest of the assembly to a scene mesh and its materials, while
    /// a component is edited in place. The ghost faces come after the document's own, so
    /// nothing of the document is renumbered; they cannot be picked.
    pub(crate) fn add_context(&self, mesh: &mut Mesh, materials: &mut Vec<Material>) {
        let Some(s) = self.edit_stack.last() else {
            return;
        };
        if s.context.faces.is_empty() {
            return;
        }
        let own = mesh.faces.len();
        mesh.faces.extend(s.context.faces.iter().cloned());
        self.add_context_materials(own, materials);
    }

    /// The ghost materials after the document's own `own_faces`: for refreshing the
    /// materials without re-meshing (the Appearance panel), so the ghosts stay ghosts.
    pub(crate) fn add_context_materials(&self, own_faces: usize, materials: &mut Vec<Material>) {
        let Some(s) = self.edit_stack.last() else {
            return;
        };
        if s.context.faces.is_empty() {
            return;
        }
        materials.resize(own_faces, Material::default());
        materials.extend(std::iter::repeat_n(ghost(), s.context.faces.len()));
    }

    /// The faces and edges of the document itself, before the ghosted context: what can
    /// be picked.
    pub(crate) fn own_mesh_counts(&self, mesh: &Mesh) -> Option<(u32, u32)> {
        let s = self.edit_stack.last()?;
        Some((
            (mesh.faces.len() - s.context.faces.len()) as u32,
            mesh.edges.len() as u32,
        ))
    }

    /// The line under the tree's heading while editing in place, with the way back.
    pub(crate) fn edit_in_place_banner(&mut self, ui: &mut egui::Ui) {
        let Some((component, parent)) = self.editing_in() else {
            return;
        };
        let read_only = match &self.asm {
            Some(asm) => asm.read_only,
            None => self.doc.read_only,
        };
        let mut finish = false;
        egui::Frame::none()
            .fill(ui.visuals().selection.bg_fill.gamma_multiply(0.25))
            .rounding(4.0)
            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "Editing {component} in {parent}{}",
                            if read_only { " (read-only)" } else { "" }
                        ))
                        .small(),
                    );
                    if ui
                        .small_button("Finish editing")
                        .on_hover_text(format!(
                            "Back to {parent}, which reads the edited file again"
                        ))
                        .clicked()
                    {
                        finish = true;
                    }
                });
            });
        ui.add_space(2.0);
        if finish {
            let ctx = ui.ctx().clone();
            self.guard(PendingAction::FinishEdit, &ctx);
        }
    }

    /// File › Finish editing, in the part and assembly menus.
    pub(crate) fn finish_editing_item(&mut self, ui: &mut egui::Ui) {
        let Some((component, parent)) = self.editing_in() else {
            return;
        };
        if ui
            .button(format!("Finish editing {component}"))
            .on_hover_text(format!(
                "Back to {parent}, which reads the edited file again"
            ))
            .clicked()
        {
            ui.close_menu();
            let ctx = ui.ctx().clone();
            self.guard(PendingAction::FinishEdit, &ctx);
        }
    }

    /// The window asking what to do about a file another window is editing. Returns the
    /// chosen mode, or `None` while open; `Some(None)` cancels.
    pub(crate) fn locked_window(
        ctx: &egui::Context,
        path: &Path,
        holder: &LockInfo,
    ) -> Option<Option<LockMode>> {
        let mut answer = None;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        crate::dialogs::window("File already being edited").show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(format!(
                "{name} is being edited in another Oxyde window ({}).",
                holder.describe()
            ));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Open it read-only to look at it (Save then asks for another name), \
                     or take the file over: the other window keeps its own changes and \
                     will not see yours until it reads the file again.",
                )
                .small()
                .weak(),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Open read-only").clicked() {
                    answer = Some(Some(LockMode::ReadOnly));
                }
                if ui.button("Take over").clicked() {
                    answer = Some(Some(LockMode::TakeOver));
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(None);
                }
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(None);
            }
        });
        answer
    }
}

impl AssemblyDoc {
    /// The parts of everything but `except` (a component at any depth, and what is under
    /// it), in `except`'s own coordinates: the context shown ghosted around a component
    /// edited in place.
    pub fn context_mesh(&self, except: &Occurrence) -> Mesh {
        let Some(placement) = self.occurrence_placement(except) else {
            return Mesh::default();
        };
        let inverse = placement.inverse();
        let leaves = self.leaves(&self.assembly);
        let under_except = |o: &Occurrence| o.top == except.top && o.path.starts_with(&except.path);
        let placed: Vec<(usize, oxyde_assembly::Placement, &Mesh)> = leaves
            .iter()
            .filter(|(o, _, _)| !under_except(o))
            .filter_map(|(_, p, part)| Some((0, inverse.compose(p), part.mesh.as_ref()?)))
            .collect();
        let (mut mesh, _) =
            oxyde_assembly::scene::combine(placed.iter().map(|(k, p, m)| (*k, p, *m)));
        // Ghosts show their shape by their faces alone.
        mesh.edges.clear();
        mesh
    }
}
