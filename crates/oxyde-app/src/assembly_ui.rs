//! Assembly mode: the component tree with its mates, the Mate command in the
//! PropertyManager, the viewport (picking, Alt+drag to move components), inserting parts,
//! saving and the bill of materials.
//!
//! The app is in assembly mode while `OxydeApp::asm` holds an assembly. Parts are edited in
//! their own documents: *Open Part* leaves the assembly, *Back to assembly* returns to it.

use std::path::Path;

use eframe::egui::{self, Color32, RichText, Stroke};
use oxyde_assembly::{
    Assembly, ComponentId, MateId, MateKind, MateRef, MateStatus, MateTarget, PrimitiveKind,
    Solution, bom, default_alignment,
};
use oxyde_geom::{Point3, Vec3};
use oxyde_model::{StandardAxis, StandardPlane};
use oxyde_render::Scene;

use crate::app::{OxydeApp, PendingAction};
use crate::assembly::{AsmSelected, AssemblyDoc};
use crate::dialogs::plane_name;
use crate::icons::{self, Icon};
use crate::property_manager::{box_active, box_fill};
use crate::timeline::{Row, tree_row};

/// The mate types offered, in the order shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MateChoice {
    Coincident,
    Concentric,
    Parallel,
    Perpendicular,
    Distance,
    Angle,
}

const CHOICES: [MateChoice; 6] = [
    MateChoice::Coincident,
    MateChoice::Concentric,
    MateChoice::Parallel,
    MateChoice::Perpendicular,
    MateChoice::Distance,
    MateChoice::Angle,
];

impl MateChoice {
    fn of(kind: MateKind) -> MateChoice {
        match kind {
            MateKind::Coincident => MateChoice::Coincident,
            MateKind::Concentric => MateChoice::Concentric,
            MateKind::Parallel => MateChoice::Parallel,
            MateKind::Perpendicular => MateChoice::Perpendicular,
            MateKind::Distance(_) => MateChoice::Distance,
            MateKind::Angle(_) => MateChoice::Angle,
        }
    }

    fn label(self) -> &'static str {
        match self {
            MateChoice::Coincident => "Coincident",
            MateChoice::Concentric => "Concentric",
            MateChoice::Parallel => "Parallel",
            MateChoice::Perpendicular => "Perpendicular",
            MateChoice::Distance => "Distance",
            MateChoice::Angle => "Angle",
        }
    }
}

/// The Mate command, open in the PropertyManager.
pub struct MateDialog {
    pub editing: Option<MateId>,
    pub a: Option<MateRef>,
    pub b: Option<MateRef>,
    /// The second selection box is the active one.
    pub second: bool,
    pub choice: MateChoice,
    /// The user picked the type: it no longer follows the geometry.
    pub choice_set: bool,
    /// mm.
    pub distance: f64,
    /// Degrees.
    pub angle: f64,
    pub aligned: bool,
    /// The user flipped the alignment: it no longer follows the geometry.
    pub aligned_set: bool,
}

impl MateDialog {
    fn kind(&self) -> MateKind {
        self.kind_for(self.choice)
    }

    fn kind_for(&self, choice: MateChoice) -> MateKind {
        match choice {
            MateChoice::Coincident => MateKind::Coincident,
            MateChoice::Concentric => MateKind::Concentric,
            MateChoice::Parallel => MateKind::Parallel,
            MateChoice::Perpendicular => MateKind::Perpendicular,
            MateChoice::Distance => MateKind::Distance(self.distance),
            MateChoice::Angle => MateKind::Angle(self.angle.to_radians()),
        }
    }

    /// The mate as it stands, once both sides are picked.
    fn draft(&self) -> Option<Draft> {
        Some(Draft {
            kind: self.kind(),
            a: self.a.clone()?,
            b: self.b.clone()?,
            aligned: self.aligned,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Draft {
    kind: MateKind,
    a: MateRef,
    b: MateRef,
    aligned: bool,
}

/// The assembly as it would be with the mate being made, shown while the command is open.
pub struct MatePreview {
    draft: Draft,
    pub assembly: Assembly,
    pub solution: Solution,
}

/// A row clicked in the assembly tree.
enum TreeAction {
    Select(AsmSelected),
    Target(MateRef),
    Toggle(ComponentId),
    Hide(ComponentId),
    Fix(ComponentId, bool),
    OpenPart(ComponentId),
    DeleteComponent(ComponentId),
    EditMate(MateId),
    FlipMate(MateId),
    SuppressMate(MateId, bool),
    DeleteMate(MateId),
}

fn standard_targets() -> [(MateTarget, &'static str, Icon); 4] {
    let plane = |p: StandardPlane| match p {
        StandardPlane::Front => "Front Plane",
        StandardPlane::Top => "Top Plane",
        StandardPlane::Right => "Right Plane",
    };
    [
        (
            MateTarget::Plane(StandardPlane::Front),
            plane(StandardPlane::Front),
            Icon::Plane,
        ),
        (
            MateTarget::Plane(StandardPlane::Top),
            plane(StandardPlane::Top),
            Icon::Plane,
        ),
        (
            MateTarget::Plane(StandardPlane::Right),
            plane(StandardPlane::Right),
            Icon::Plane,
        ),
        (MateTarget::Origin, "Origin", Icon::Origin),
    ]
}

/// "Face of Bracket<1>", "Top Plane of Pin<2>"…
fn describe_ref(doc: &AssemblyDoc, r: &MateRef) -> String {
    let what = match &r.target {
        MateTarget::Face(_) => "Face".to_owned(),
        MateTarget::Edge(_) => "Edge".to_owned(),
        MateTarget::Plane(p) => format!("{} Plane", plane_name(*p)),
        MateTarget::Axis(a) => format!(
            "{} axis",
            match a {
                StandardAxis::X => "X",
                StandardAxis::Y => "Y",
                StandardAxis::Z => "Z",
            }
        ),
        MateTarget::Origin => "Origin".to_owned(),
    };
    let owner = doc
        .component(r.component)
        .map_or("(missing)", |c| c.name.as_str());
    // A part inside a sub-assembly: "Face of Pin<2> in Arm<1>".
    match doc.inner_names(r.component, &r.path).last() {
        Some(inner) => format!("{what} of {inner} in {owner}"),
        None => format!("{what} of {owner}"),
    }
}

/// A selection box of the Mate command.
fn mate_box(ui: &mut egui::Ui, text: Option<String>, active: bool, hint: &str) -> (bool, bool) {
    let stroke = if active {
        Stroke::new(2.0, box_active())
    } else {
        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)
    };
    let (mut clicked, mut cleared) = (false, false);
    egui::Frame::none()
        .fill(if active {
            box_fill()
        } else {
            Color32::TRANSPARENT
        })
        .stroke(stroke)
        .rounding(3.0)
        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if text.is_some()
                    && icons::icon_button(ui, Icon::Cancel, 16.0, true)
                        .on_hover_text("Clear")
                        .clicked()
                {
                    cleared = true;
                }
                let label = match &text {
                    Some(t) => RichText::new(t),
                    None => RichText::new(hint).italics().weak(),
                };
                let label = ui.add(
                    egui::Label::new(label)
                        .truncate()
                        .sense(egui::Sense::click()),
                );
                let rest = ui.allocate_response(
                    egui::vec2(ui.available_width(), label.rect.height()),
                    egui::Sense::click(),
                );
                clicked = label.clicked() || rest.clicked();
            });
        });
    (clicked, cleared)
}

impl OxydeApp {
    // ---- Opening, saving, leaving ----------------------------------------------------------

    pub(crate) fn is_modified(&self) -> bool {
        match &self.asm {
            Some(doc) => doc.modified,
            None => self.doc.modified,
        }
    }

    /// Switches to `doc`, loading its parts and solving its mates.
    pub(crate) fn enter_assembly(&mut self, mut doc: AssemblyDoc) {
        self.leave_assembly();
        self.dialog = None;
        doc.mesh_fraction = self.mesh_fraction();
        let notes = doc.load_parts(&mut self.kernel);
        doc.solve();
        let errors = doc.solution.as_ref().map_or(0, Solution::errors);
        self.status = match (notes.first(), errors) {
            (Some(note), _) => note.clone(),
            (None, 0) => format!("Assembly {}", doc.display_name()),
            (None, n) => format!("{n} mate(s) cannot be met"),
        };
        self.asm = Some(doc);
        self.asm_scene = None;
        self.viewport.request_fit();
    }

    /// Back to part mode, freeing the assembly's parts.
    pub(crate) fn leave_assembly(&mut self) {
        if let Some(mut doc) = self.asm.take() {
            doc.release(&mut self.kernel);
        }
        self.mate_dialog = None;
        self.mate_preview = None;
        self.show_bom = false;
        self.viewport.grab_enabled = false;
        self.scene_revision = None;
    }

    pub(crate) fn open_assembly(&mut self, path: &Path) {
        match oxyde_format::load_assembly(path) {
            Ok(file) => {
                self.enter_assembly(AssemblyDoc::from_file(file, Some(path.to_path_buf())));
                self.remember_recent(path);
                self.return_to = None;
            }
            Err(e) => self.status = format!("Could not open {}: {e}", path.display()),
        }
    }

    pub(crate) fn save_assembly(&mut self, choose: bool) -> bool {
        let Some(doc) = self.asm.as_ref() else {
            return false;
        };
        let path = match (&doc.path, choose) {
            (Some(path), false) => path.clone(),
            _ => {
                let Some(mut path) = rfd::FileDialog::new()
                    .add_filter("Oxyde assembly", &[oxyde_format::ASSEMBLY_EXTENSION])
                    .set_file_name(format!(
                        "{}.{}",
                        doc.display_name(),
                        oxyde_format::ASSEMBLY_EXTENSION
                    ))
                    .save_file()
                else {
                    return false;
                };
                if !has_extension(&path, oxyde_format::ASSEMBLY_EXTENSION) {
                    path.set_extension(oxyde_format::ASSEMBLY_EXTENSION);
                }
                path
            }
        };
        let doc = self.asm.as_mut().expect("checked above");
        // Part paths are stored relative to where the assembly is.
        doc.relocate(&path);
        match oxyde_format::save_assembly(&path, &doc.to_file()) {
            Ok(()) => {
                doc.path = Some(path.clone());
                doc.modified = false;
                self.remember_recent(&path);
                self.status = format!("Saved {}", path.display());
                true
            }
            Err(e) => {
                self.status = format!("Could not save {}: {e}", path.display());
                false
            }
        }
    }

    /// Opens a component's part for editing, remembering the assembly to come back to.
    pub(crate) fn open_component(&mut self, id: ComponentId) {
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let Some(path) = doc.sources.get(&id).cloned() else {
            return;
        };
        if doc.load_error(id).is_some() {
            self.status = format!("{} cannot be opened", path.display());
            return;
        }
        self.return_to = doc.path.clone();
        self.leave_assembly();
        self.open_path(&path);
        if self.return_to.is_none() {
            self.status = format!(
                "{}: save the assembly first to come back to it from the part",
                self.status
            );
        }
    }

    // ---- Commands --------------------------------------------------------------------------

    pub(crate) fn insert_component(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "Oxyde part or assembly",
                &[
                    oxyde_format::PART_EXTENSION,
                    oxyde_format::ASSEMBLY_EXTENSION,
                    oxyde_format::LEGACY_PART_EXTENSION,
                ],
            )
            .pick_files()
        else {
            return;
        };
        for path in paths {
            self.insert_part(&path);
        }
    }

    fn insert_part(&mut self, path: &Path) {
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        // Read again from disk: the file may have changed since it was last used.
        if let Some(Ok(crate::assembly::Loaded::Part(mut old))) = doc.docs.remove(path) {
            old.part.release(&mut self.kernel);
        }
        if let Err(e) = doc.load_file(path, &mut self.kernel) {
            self.status = format!("Could not insert: {e}");
            return;
        }
        let placement = doc.free_spot(doc.file_bounds(path));
        let document_id = doc
            .docs
            .get(path)
            .and_then(|d| d.as_ref().ok())
            .map(|d| d.document_id().to_owned())
            .unwrap_or_default();
        doc.checkpoint();
        let stem = path.file_stem().map_or_else(
            || "Component".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
        let stored = doc.stored_path(path);
        let id = doc
            .assembly
            .add_component(&stem, stored, document_id, placement);
        doc.load_parts(&mut self.kernel);
        doc.solve();
        doc.selection = vec![AsmSelected::Component(id)];
        let name = doc
            .component(id)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let fixed = doc.component(id).is_some_and(|c| c.fixed);
        self.status = if fixed {
            format!("Inserted {name} (fixed: the first component holds the others)")
        } else {
            format!("Inserted {name}: add mates to place it, or Alt+drag it")
        };
        if doc.assembly.components.len() == 1 {
            self.viewport.request_fit();
        }
    }

    pub(crate) fn begin_mate(&mut self) {
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        // Two faces or edges selected beforehand fill both boxes.
        let refs: Vec<MateRef> = doc
            .selection
            .iter()
            .filter_map(AsmSelected::mate_ref)
            .collect();
        let mut dialog = MateDialog {
            editing: None,
            a: None,
            b: None,
            second: false,
            choice: MateChoice::Coincident,
            choice_set: false,
            distance: 10.0,
            angle: 90.0,
            aligned: true,
            aligned_set: false,
        };
        for r in refs.into_iter().take(2) {
            self.mate_pick(&mut dialog, r);
        }
        self.mate_dialog = Some(dialog);
        self.mate_preview = None;
        if let Some(doc) = self.asm.as_mut() {
            doc.selection.clear();
        }
    }

    fn edit_mate(&mut self, id: MateId) {
        let Some(mate) = self.asm.as_ref().and_then(|d| d.assembly.mate(id).cloned()) else {
            return;
        };
        let (distance, angle) = match mate.kind {
            MateKind::Distance(d) => (d, 90.0),
            MateKind::Angle(a) => (10.0, a.to_degrees()),
            _ => (10.0, 90.0),
        };
        self.mate_dialog = Some(MateDialog {
            editing: Some(id),
            a: Some(mate.a),
            b: Some(mate.b),
            second: true,
            choice: MateChoice::of(mate.kind),
            choice_set: true,
            distance,
            angle,
            aligned: mate.aligned,
            aligned_set: true,
        });
        self.mate_preview = None;
    }

    /// A face, edge, plane or origin clicked while the Mate command is open.
    fn mate_pick(&mut self, d: &mut MateDialog, r: MateRef) {
        let other = if d.second { &d.a } else { &d.b };
        if other.as_ref().is_some_and(|o| o.component == r.component) {
            self.status = "Pick the other side on another component".to_owned();
            return;
        }
        if d.second {
            d.b = Some(r);
        } else {
            d.a = Some(r);
            if d.b.is_none() {
                d.second = true;
            }
        }
        self.mate_defaults(d);
    }

    /// The type and alignment that fit the picked geometry, unless the user chose them.
    fn mate_defaults(&self, d: &mut MateDialog) {
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let (Some(a), Some(b)) = (&d.a, &d.b) else {
            return;
        };
        let (Ok(la), Ok(lb)) = (doc.local(a), doc.local(b)) else {
            return;
        };
        let (ka, kb) = (la.kind(), lb.kind());
        if !d.choice_set || !d.kind().applies(ka, kb) {
            // Round faces and circular edges turn about an axis: concentric. Anything
            // else (flat faces, straight edges, planes) starts coincident.
            let round = |r: &MateRef, k: PrimitiveKind| {
                k == PrimitiveKind::Circle
                    || (k == PrimitiveKind::Line && matches!(r.target, MateTarget::Face(_)))
            };
            d.choice = if round(a, ka) && round(b, kb) {
                MateChoice::Concentric
            } else {
                MateChoice::Coincident
            };
        }
        if !d.aligned_set {
            if let (Some(pa), Some(pb)) =
                (doc.placed(&doc.assembly, a), doc.placed(&doc.assembly, b))
            {
                d.aligned = default_alignment(&pa, &pb);
            }
        }
    }

    fn apply_mate(&mut self, d: MateDialog) {
        let Some(draft) = d.draft() else {
            return;
        };
        // The preview is reused only if it shows exactly this mate.
        let preview = self.mate_preview.take().filter(|p| p.draft == draft);
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        doc.checkpoint();
        let id = match d.editing {
            Some(id) => {
                if let Some(m) = doc.assembly.mate_mut(id) {
                    m.kind = draft.kind;
                    m.a = draft.a;
                    m.b = draft.b;
                    m.aligned = draft.aligned;
                }
                id
            }
            None => doc
                .assembly
                .add_mate(draft.kind, draft.a, draft.b, draft.aligned),
        };
        match preview {
            // The preview already holds the solved placements.
            Some(p) if p.assembly.mate(id).is_some() => {
                doc.assembly = p.assembly;
                doc.solution = Some(p.solution);
                doc.revision += 1;
            }
            _ => doc.solve(),
        }
        let name = doc
            .assembly
            .mate(id)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        self.status = match doc.solution.as_ref().and_then(|s| s.status(id)) {
            Some(MateStatus::Error(e)) => format!("{name}: {e}"),
            _ => format!("{name} added"),
        };
    }

    /// Keeps the preview in step with the Mate command.
    fn refresh_mate_preview(&mut self) {
        let Some(draft) = self.mate_dialog.as_ref().and_then(MateDialog::draft) else {
            if self.mate_preview.take().is_some() {
                self.bump_asm();
            }
            return;
        };
        if self.mate_preview.as_ref().is_some_and(|p| p.draft == draft) {
            return;
        }
        let editing = self.mate_dialog.as_ref().and_then(|d| d.editing);
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let mut assembly = doc.assembly.clone();
        match editing.and_then(|id| assembly.mate_mut(id)) {
            Some(m) => {
                m.kind = draft.kind;
                m.a = draft.a.clone();
                m.b = draft.b.clone();
                m.aligned = draft.aligned;
            }
            None => {
                assembly.add_mate(draft.kind, draft.a.clone(), draft.b.clone(), draft.aligned);
            }
        }
        let solution = doc.solve_assembly(&mut assembly);
        self.mate_preview = Some(MatePreview {
            draft,
            assembly,
            solution,
        });
        self.bump_asm();
    }

    fn bump_asm(&mut self) {
        if let Some(doc) = self.asm.as_mut() {
            doc.revision += 1;
        }
    }

    pub(crate) fn toggle_fixed(&mut self, id: ComponentId, fixed: bool) {
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        doc.checkpoint();
        if let Some(c) = doc.assembly.component_mut(id) {
            c.fixed = fixed;
        }
        doc.solve();
    }

    pub(crate) fn delete_selected(&mut self) {
        if self.mate_dialog.is_some() {
            return;
        }
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        let selection = std::mem::take(&mut doc.selection);
        let mut removed = 0;
        for item in &selection {
            match item {
                AsmSelected::Mate(m) => {
                    if removed == 0 {
                        doc.checkpoint();
                    }
                    doc.assembly.remove_mate(*m);
                    removed += 1;
                }
                AsmSelected::Component(c) => {
                    if removed == 0 {
                        doc.checkpoint();
                    }
                    doc.assembly.remove_component(*c);
                    removed += 1;
                }
                _ => {}
            }
        }
        if removed > 0 {
            doc.solve();
            self.status = format!("Deleted {removed} item(s)");
        }
    }

    pub(crate) fn reload_assembly_parts(&mut self) {
        let fraction = self.mesh_fraction();
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        doc.mesh_fraction = fraction;
        let notes = doc.reload_parts(&mut self.kernel);
        doc.solve();
        self.status = notes
            .into_iter()
            .next()
            .unwrap_or_else(|| "Parts reloaded".to_owned());
    }

    pub(crate) fn asm_undo(&mut self) {
        if let Some(doc) = self.asm.as_mut() {
            if doc.undo() {
                doc.load_parts(&mut self.kernel);
                doc.solve();
            }
        }
    }

    pub(crate) fn asm_redo(&mut self) {
        if let Some(doc) = self.asm.as_mut() {
            if doc.redo() {
                doc.load_parts(&mut self.kernel);
                doc.solve();
            }
        }
    }

    // ---- Layout ----------------------------------------------------------------------------

    pub(crate) fn assembly_menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New part").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::New, &ctx);
                }
                if ui.button("New assembly").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::NewAssembly, &ctx);
                }
                if ui.button("Open…").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::Open, &ctx);
                }
                if ui
                    .button("Import STEP assembly…")
                    .on_hover_text("One part file per part and one assembly file per assembly")
                    .clicked()
                {
                    ui.close_menu();
                    self.guard(PendingAction::ImportStepAssembly, &ctx);
                }
                ui.separator();
                if ui.button("Save").clicked() {
                    ui.close_menu();
                    self.save_assembly(false);
                }
                if ui.button("Save as…").clicked() {
                    ui.close_menu();
                    self.save_assembly(true);
                }
                ui.separator();
                if ui.button("Insert component…").clicked() {
                    ui.close_menu();
                    self.insert_component();
                }
                if ui.button("Bill of materials…").clicked() {
                    ui.close_menu();
                    self.show_bom = true;
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::Exit, &ctx);
                }
            });
            ui.menu_button("Edit", |ui| {
                let (undo, redo) = self
                    .asm
                    .as_ref()
                    .map_or((false, false), |d| (d.can_undo(), d.can_redo()));
                if ui.add_enabled(undo, egui::Button::new("Undo")).clicked() {
                    ui.close_menu();
                    self.asm_undo();
                }
                if ui.add_enabled(redo, egui::Button::new("Redo")).clicked() {
                    ui.close_menu();
                    self.asm_redo();
                }
                ui.separator();
                if ui.button("Delete (Del)").clicked() {
                    ui.close_menu();
                    self.delete_selected();
                }
                if ui.button("Reload parts (Ctrl+B)").clicked() {
                    ui.close_menu();
                    self.reload_assembly_parts();
                }
            });
            ui.menu_button("Tools", |ui| {
                if ui.button("Preferences…").clicked() {
                    ui.close_menu();
                    self.open_preferences(crate::prefs_ui::PrefsTab::Graphics);
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts").clicked() {
                    ui.close_menu();
                    self.open_preferences(crate::prefs_ui::PrefsTab::Shortcuts);
                }
                if ui.button("Assembly mouse controls").clicked() {
                    ui.close_menu();
                    self.status = "Alt+drag moves a component, Alt+Shift+drag turns it; mates \
                                   pull it back where they hold"
                        .to_owned();
                }
                if ui.button("About Oxyde").clicked() {
                    ui.close_menu();
                    self.dialog = Some(crate::dialogs::Dialog::About);
                }
            });
        });
    }

    pub(crate) fn assembly_command_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let _ = ui.selectable_label(true, RichText::new("Assembly").size(13.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (undo, redo) = self
                    .asm
                    .as_ref()
                    .map_or((false, false), |d| (d.can_undo(), d.can_redo()));
                if icons::icon_button(ui, Icon::Redo, 22.0, redo)
                    .on_hover_text("Redo (Ctrl+Y)")
                    .clicked()
                {
                    self.asm_redo();
                }
                if icons::icon_button(ui, Icon::Undo, 22.0, undo)
                    .on_hover_text("Undo (Ctrl+Z)")
                    .clicked()
                {
                    self.asm_undo();
                }
            });
        });
        ui.add_space(2.0);
        let selected = self.asm.as_ref().and_then(|d| {
            d.selection
                .iter()
                .find_map(AsmSelected::component)
                .and_then(|c| d.component(c))
                .map(|c| (c.id, c.fixed))
        });
        let has_two = self
            .asm
            .as_ref()
            .is_some_and(|d| d.assembly.components.len() >= 2);
        let mating = self
            .mate_dialog
            .as_ref()
            .is_some_and(|d| d.editing.is_none());
        ui.horizontal(|ui| {
            if icons::command_button(ui, Icon::InsertComponent, "Insert\nComponent", true, false)
                .on_hover_text("Add a part file to the assembly")
                .clicked()
            {
                self.insert_component();
            }
            if icons::command_button(ui, Icon::Mate, "Mate", has_two, mating)
                .on_hover_text(if has_two {
                    "Hold two components together: pick a face, edge or plane on each"
                } else {
                    "Mate\n(insert two components first)"
                })
                .clicked()
                && has_two
            {
                self.begin_mate();
            }
            let fix_label = match selected {
                Some((_, true)) => "Float",
                _ => "Fix",
            };
            let fix = icons::command_button(ui, Icon::Part, fix_label, selected.is_some(), false)
                .on_hover_text("A fixed component never moves; a floating one follows its mates");
            if fix.clicked() {
                if let Some((id, fixed)) = selected {
                    self.toggle_fixed(id, !fixed);
                }
            }
            if icons::command_button(ui, Icon::Rebuild, "Reload\nParts", true, false)
                .on_hover_text("Read the part files again, after editing them (Ctrl+B)")
                .clicked()
            {
                self.reload_assembly_parts();
            }
            if icons::command_button(
                ui,
                Icon::BillOfMaterials,
                "Bill of\nMaterials",
                true,
                self.show_bom,
            )
            .on_hover_text("Parts and quantities, exported as CSV")
            .clicked()
            {
                self.show_bom = !self.show_bom;
            }
        });
    }

    pub(crate) fn assembly_status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(&self.status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(doc) = &self.asm {
                    let errors = doc.solution.as_ref().map_or(0, Solution::errors);
                    let text = format!(
                        "{} component(s), {} mate(s){}",
                        doc.assembly.components.len(),
                        doc.assembly.mates.len(),
                        if errors > 0 {
                            format!(", {errors} in error")
                        } else {
                            String::new()
                        }
                    );
                    ui.label(text);
                }
            });
        });
    }

    /// The PropertyManager (when Mate is open) and the assembly tree.
    pub(crate) fn assembly_panel(&mut self, ui: &mut egui::Ui) {
        self.mate_manager(ui);
        self.refresh_mate_preview();
        self.assembly_tree(ui);
    }

    fn mate_manager(&mut self, ui: &mut egui::Ui) {
        let Some(mut d) = self.mate_dialog.take() else {
            return;
        };
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let kinds = match (&d.a, &d.b) {
            (Some(a), Some(b)) => match (doc.local(a), doc.local(b)) {
                (Ok(a), Ok(b)) => Some((a.kind(), b.kind())),
                _ => None,
            },
            _ => None,
        };
        let ready = d.a.is_some() && d.b.is_some();
        let text_a = d.a.as_ref().map(|r| describe_ref(doc, r));
        let text_b = d.b.as_ref().map(|r| describe_ref(doc, r));
        let preview_error = self.mate_preview.as_ref().and_then(|p| {
            let id = d
                .editing
                .or_else(|| p.assembly.mates.last().map(|m| m.id))?;
            match p.solution.status(id) {
                Some(MateStatus::Error(e)) => Some(e.clone()),
                _ => None,
            }
        });
        let mut answer = None;
        egui::Frame::none()
            .fill(ui.visuals().faint_bg_color)
            .stroke(Stroke::new(
                1.0,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
            .rounding(4.0)
            .inner_margin(egui::Margin::same(8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    icons::small(ui, Icon::Mate, 22.0);
                    let title = if d.editing.is_some() {
                        "Edit Mate"
                    } else {
                        "Mate"
                    };
                    ui.label(RichText::new(title).strong().size(15.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::icon_button(ui, Icon::Cancel, 24.0, true)
                            .on_hover_text("Cancel (Esc)")
                            .clicked()
                        {
                            answer = Some(false);
                        }
                        let ok = icons::icon_button(ui, Icon::Ok, 24.0, ready);
                        if ok
                            .on_hover_text(if ready {
                                "OK (Enter)"
                            } else {
                                "Pick geometry on two components first"
                            })
                            .clicked()
                        {
                            answer = Some(true);
                        }
                    });
                });
                ui.separator();
                crate::property_manager::section(ui, "Entities to mate");
                let hint = "Click a face, an edge, or a plane in the tree";
                let (click_a, clear_a) = mate_box(ui, text_a, !d.second, hint);
                ui.add_space(2.0);
                let (click_b, clear_b) = mate_box(ui, text_b, d.second, hint);
                if click_a || clear_a {
                    d.second = false;
                }
                if click_b || clear_b {
                    d.second = true;
                }
                if clear_a {
                    d.a = None;
                }
                if clear_b {
                    d.b = None;
                }
                ui.add_space(6.0);
                crate::property_manager::section(ui, "Mate type");
                ui.horizontal_wrapped(|ui| {
                    for choice in CHOICES {
                        let fits = kinds.is_none_or(|(a, b)| d.kind_for(choice).applies(a, b));
                        let response = ui.add_enabled(
                            fits,
                            egui::SelectableLabel::new(d.choice == choice, choice.label()),
                        );
                        if response.clicked() {
                            d.choice = choice;
                            d.choice_set = true;
                        }
                    }
                });
                match d.choice {
                    MateChoice::Distance => {
                        ui.horizontal(|ui| {
                            ui.label("Distance");
                            ui.add(
                                egui::DragValue::new(&mut d.distance)
                                    .speed(0.1)
                                    .suffix(" mm"),
                            );
                            d.distance = d.distance.max(0.0);
                        });
                    }
                    MateChoice::Angle => {
                        ui.horizontal(|ui| {
                            ui.label("Angle");
                            ui.add(egui::DragValue::new(&mut d.angle).speed(0.5).suffix("°"));
                            d.angle = d.angle.clamp(0.0, 180.0);
                        });
                    }
                    _ => {}
                }
                if let Some((a, b)) = kinds {
                    if d.kind().has_alignment(a, b) {
                        ui.horizontal(|ui| {
                            ui.label("Alignment");
                            let before = d.aligned;
                            ui.radio_value(&mut d.aligned, true, "Aligned");
                            ui.radio_value(&mut d.aligned, false, "Anti-aligned");
                            if d.aligned != before {
                                d.aligned_set = true;
                            }
                        });
                    }
                }
                if let Some(e) = &preview_error {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        icons::small(ui, Icon::Error, 16.0);
                        ui.label(RichText::new(e).color(crate::theme::tokens().err));
                    });
                }
            });
        ui.add_space(6.0);
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            answer = Some(false);
        }
        if ready && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            answer = Some(true);
        }
        if d.a.is_some() && d.b.is_some() {
            self.mate_defaults(&mut d);
        }
        match answer {
            None => self.mate_dialog = Some(d),
            Some(true) => self.apply_mate(d),
            Some(false) => {
                self.mate_preview = None;
                self.bump_asm();
            }
        }
    }

    fn assembly_tree(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            icons::small(ui, Icon::Assembly, 22.0);
            ui.heading(doc.display_name());
        });
        ui.add_space(2.0);
        let mating = self.mate_dialog.is_some();
        let mut action = None;
        // What the pointer is over in the tree, lit up in the viewport.
        let mut hovered: Vec<AsmSelected> = Vec::new();
        let mut hovered_refs: Vec<MateRef> = Vec::new();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                if doc.assembly.components.is_empty() {
                    ui.label(
                        RichText::new("Insert Component adds a part file.")
                            .italics()
                            .weak(),
                    );
                }
                for c in &doc.assembly.components {
                    let degrees = doc.solution.as_ref().and_then(|s| s.degrees(c.id));
                    let prefix = if c.fixed {
                        "(f) "
                    } else if degrees.is_some_and(|d| d > 0) {
                        "(-) "
                    } else {
                        ""
                    };
                    let error = doc.load_error(c.id);
                    let selected = doc.selection.contains(&AsmSelected::Component(c.id));
                    let expanded = doc.expanded.contains(&c.id);
                    let sub = match doc.loaded(c.id) {
                        Some(crate::assembly::Loaded::Assembly(sub)) => Some(sub),
                        _ => None,
                    };
                    let row = tree_row(
                        ui,
                        Row {
                            icon: if sub.is_some() {
                                Icon::Assembly
                            } else {
                                Icon::Part
                            },
                            cut: false,
                            text: RichText::new(format!("{prefix}{}", c.name)),
                            highlighted: selected,
                            badge: error.map(|_| Icon::Error),
                            indent: 0.0,
                            expander: Some(expanded),
                            eye: Some(!c.hidden),
                        },
                    );
                    let id = c.id;
                    if row.response.hovered() {
                        hovered.push(AsmSelected::Component(id));
                    }
                    let fixed = c.fixed;
                    let hover = match (error, degrees) {
                        (Some(e), _) => e.to_owned(),
                        (None, _) if fixed => format!("{}\nFixed", c.path),
                        (None, Some(0)) => format!("{}\nFully defined by its mates", c.path),
                        (None, Some(d)) => {
                            format!("{}\nCan still move: {d} degree(s) of freedom", c.path)
                        }
                        (None, None) => c.path.clone(),
                    };
                    if row.expand_clicked {
                        action = Some(TreeAction::Toggle(id));
                    } else if row.eye_clicked {
                        action = Some(TreeAction::Hide(id));
                    } else if row.response.double_clicked() {
                        action = Some(TreeAction::OpenPart(id));
                    } else if row.response.clicked() && !mating {
                        action = Some(TreeAction::Select(AsmSelected::Component(id)));
                    }
                    let response = row.response.on_hover_text(hover);
                    response.context_menu(|ui| {
                        let open = if sub.is_some() {
                            "Open sub-assembly"
                        } else {
                            "Open part"
                        };
                        if ui.button(open).clicked() {
                            action = Some(TreeAction::OpenPart(id));
                            ui.close_menu();
                        }
                        if ui.button(if fixed { "Float" } else { "Fix" }).clicked() {
                            action = Some(TreeAction::Fix(id, !fixed));
                            ui.close_menu();
                        }
                        if ui.button("Hide / Show").clicked() {
                            action = Some(TreeAction::Hide(id));
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Delete").clicked() {
                            action = Some(TreeAction::DeleteComponent(id));
                            ui.close_menu();
                        }
                    });
                    if expanded {
                        for (target, label, icon) in standard_targets() {
                            let row = tree_row(
                                ui,
                                Row {
                                    icon,
                                    cut: false,
                                    text: RichText::new(label),
                                    highlighted: false,
                                    badge: None,
                                    indent: 16.0,
                                    expander: None,
                                    eye: None,
                                },
                            );
                            let response = if mating {
                                row.response.on_hover_text("Click to use it in the mate")
                            } else {
                                row.response
                            };
                            if response.clicked() && mating {
                                action = Some(TreeAction::Target(MateRef::new(id, target)));
                            }
                        }
                        // What the sub-assembly holds, as it was saved: edited by opening it.
                        if let Some(sub) = sub {
                            sub_tree(ui, doc, sub, 16.0, 0);
                        }
                    }
                }
                ui.add_space(4.0);
                tree_row(
                    ui,
                    Row {
                        icon: Icon::Mate,
                        cut: false,
                        text: RichText::new("Mates").strong(),
                        highlighted: false,
                        badge: None,
                        indent: 0.0,
                        expander: None,
                        eye: None,
                    },
                );
                for m in &doc.assembly.mates {
                    let status = doc.solution.as_ref().and_then(|s| s.status(m.id));
                    let error = match status {
                        Some(MateStatus::Error(e)) => Some(e.clone()),
                        _ => None,
                    };
                    let text = RichText::new(format!(
                        "{} ({}, {})",
                        m.name,
                        doc.component(m.a.component)
                            .map_or("?", |c| c.name.as_str()),
                        doc.component(m.b.component)
                            .map_or("?", |c| c.name.as_str()),
                    ));
                    let text = if m.suppressed {
                        text.weak()
                    } else if error.is_some() {
                        text.color(crate::theme::tokens().err)
                    } else {
                        text
                    };
                    let row = tree_row(
                        ui,
                        Row {
                            icon: Icon::Mate,
                            cut: false,
                            text,
                            highlighted: doc.selection.contains(&AsmSelected::Mate(m.id)),
                            badge: error.as_ref().map(|_| Icon::Error),
                            indent: 16.0,
                            expander: None,
                            eye: None,
                        },
                    );
                    let id = m.id;
                    if row.response.hovered() {
                        hovered_refs.extend([m.a.clone(), m.b.clone()]);
                    }
                    let suppressed = m.suppressed;
                    if row.response.double_clicked() {
                        action = Some(TreeAction::EditMate(id));
                    } else if row.response.clicked() && !mating {
                        action = Some(TreeAction::Select(AsmSelected::Mate(id)));
                    }
                    let mut response = row.response;
                    if let Some(e) = &error {
                        response = response.on_hover_text(e);
                    }
                    response.context_menu(|ui| {
                        if ui.button("Edit mate").clicked() {
                            action = Some(TreeAction::EditMate(id));
                            ui.close_menu();
                        }
                        if ui.button("Flip alignment").clicked() {
                            action = Some(TreeAction::FlipMate(id));
                            ui.close_menu();
                        }
                        let label = if suppressed { "Unsuppress" } else { "Suppress" };
                        if ui.button(label).clicked() {
                            action = Some(TreeAction::SuppressMate(id, !suppressed));
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Delete").clicked() {
                            action = Some(TreeAction::DeleteMate(id));
                            ui.close_menu();
                        }
                    });
                }
            });
        let mut preselect = doc.picks_of(&hovered);
        for r in &hovered_refs {
            preselect.extend(doc.picks_of_ref(r));
        }
        self.viewport.preselect = preselect;
        let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
        if let Some(action) = action {
            self.tree_action(action, additive);
        }
    }

    fn tree_action(&mut self, action: TreeAction, additive: bool) {
        match action {
            TreeAction::Select(item) => {
                if let Some(doc) = self.asm.as_mut() {
                    select(&mut doc.selection, item, additive);
                }
            }
            TreeAction::Target(r) => {
                if let Some(mut d) = self.mate_dialog.take() {
                    self.mate_pick(&mut d, r);
                    self.mate_dialog = Some(d);
                }
            }
            TreeAction::Toggle(id) => {
                if let Some(doc) = self.asm.as_mut() {
                    if !doc.expanded.remove(&id) {
                        doc.expanded.insert(id);
                    }
                }
            }
            TreeAction::Hide(id) => {
                if let Some(doc) = self.asm.as_mut() {
                    doc.checkpoint();
                    if let Some(c) = doc.assembly.component_mut(id) {
                        c.hidden = !c.hidden;
                    }
                    doc.revision += 1;
                }
            }
            TreeAction::Fix(id, fixed) => self.toggle_fixed(id, fixed),
            TreeAction::OpenPart(id) => self.pending_open_component = Some(id),
            TreeAction::DeleteComponent(id) => {
                if let Some(doc) = self.asm.as_mut() {
                    doc.checkpoint();
                    let removed = doc.assembly.remove_component(id);
                    doc.selection.clear();
                    doc.solve();
                    self.status = if removed.is_empty() {
                        "Component deleted".to_owned()
                    } else {
                        format!("Component deleted, with {}", removed.join(", "))
                    };
                }
            }
            TreeAction::EditMate(id) => self.edit_mate(id),
            TreeAction::FlipMate(id) => {
                if let Some(doc) = self.asm.as_mut() {
                    doc.checkpoint();
                    if let Some(m) = doc.assembly.mate_mut(id) {
                        m.aligned = !m.aligned;
                    }
                    doc.solve();
                }
            }
            TreeAction::SuppressMate(id, suppressed) => {
                if let Some(doc) = self.asm.as_mut() {
                    doc.checkpoint();
                    if let Some(m) = doc.assembly.mate_mut(id) {
                        m.suppressed = suppressed;
                    }
                    doc.solve();
                }
            }
            TreeAction::DeleteMate(id) => {
                if let Some(doc) = self.asm.as_mut() {
                    doc.checkpoint();
                    doc.assembly.remove_mate(id);
                    doc.selection.retain(|s| *s != AsmSelected::Mate(id));
                    doc.solve();
                }
            }
        }
    }

    // ---- Viewport --------------------------------------------------------------------------

    pub(crate) fn assembly_viewport(&mut self, ui: &mut egui::Ui) {
        self.refresh_assembly_scene();
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let picks = match &self.mate_dialog {
            Some(d) => [&d.a, &d.b]
                .into_iter()
                .flatten()
                .flat_map(|r| doc.picks_of_ref(r))
                .collect(),
            None => doc.picks_of(&doc.selection),
        };
        let hint = doc
            .assembly
            .components
            .is_empty()
            .then_some("Insert Component adds a part — then Mate holds parts together");
        self.viewport.grab_enabled = true;
        let output = self.viewport.ui(ui, picks, hint, false);
        self.viewport_rect = output.rect;

        if let Some(grab) = output.grab {
            self.grab_component(&grab);
        } else {
            self.grabbing = None;
        }
        if let Some(pick) = output.clicked {
            let item = pick.and_then(|p| self.asm.as_ref().and_then(|d| d.picked(p)));
            if let Some(mut d) = self.mate_dialog.take() {
                if let Some(r) = item.as_ref().and_then(AsmSelected::mate_ref) {
                    self.mate_pick(&mut d, r);
                }
                self.mate_dialog = Some(d);
            } else if let Some(doc) = self.asm.as_mut() {
                match item {
                    Some(item) => select(&mut doc.selection, item, output.additive),
                    None if !output.additive => doc.selection.clear(),
                    None => {}
                }
            }
        }
        // Right-click on the part: what can be done with its component.
        let hovered = self
            .asm
            .as_ref()
            .and_then(|d| d.selection.iter().find_map(AsmSelected::component));
        output.response.context_menu(|ui| match hovered {
            Some(id) => {
                if ui.button("Open part").clicked() {
                    self.pending_open_component = Some(id);
                    ui.close_menu();
                }
                let fixed = self
                    .asm
                    .as_ref()
                    .and_then(|d| d.component(id))
                    .is_some_and(|c| c.fixed);
                if ui.button(if fixed { "Float" } else { "Fix" }).clicked() {
                    self.toggle_fixed(id, !fixed);
                    ui.close_menu();
                }
                if ui.button("Mate…").clicked() {
                    self.begin_mate();
                    ui.close_menu();
                }
            }
            None => {
                ui.label(RichText::new("Select a component first").weak());
            }
        });
        let ctx = ui.ctx().clone();
        if let Some(id) = self.pending_open_component.take() {
            self.guard(PendingAction::OpenComponent(id), &ctx);
        }
    }

    /// Alt+drag: slides (or turns) the grabbed component, then lets the mates pull it back.
    fn grab_component(&mut self, grab: &crate::viewport::Grab) {
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        let component = match self.grabbing {
            Some(c) => Some(c),
            None => grab
                .start
                .and_then(|p| doc.picked(p))
                .and_then(|s| s.component()),
        };
        let Some(id) = component else {
            return;
        };
        let Some((fixed, name, placement)) = doc
            .component(id)
            .map(|c| (c.fixed, c.name.clone(), c.placement))
        else {
            return;
        };
        if fixed {
            if self.grabbing.is_none() {
                self.status = format!("{name} is fixed: Float it to move it");
            }
            self.grabbing = Some(id);
            return;
        }
        if self.grabbing.is_none() {
            doc.checkpoint();
            self.grabbing = Some(id);
        }
        let v = |a: [f64; 3]| Vec3::new(a[0], a[1], a[2]);
        let (right, up) = (v(grab.right), v(grab.up));
        let center = doc.component_bounds(id).map_or(Point3::ORIGIN, |(lo, hi)| {
            Point3::new(
                (lo.x + hi.x) / 2.0,
                (lo.y + hi.y) / 2.0,
                (lo.z + hi.z) / 2.0,
            )
        });
        let center = placement.apply_point(center);
        let moved = if grab.rotate {
            // About a full turn for a drag across the assembly.
            let k = std::f64::consts::TAU / doc.size();
            placement.moved(
                up * (grab.along[0] * k) - right * (grab.along[1] * k),
                center,
                Vec3::ZERO,
            )
        } else {
            placement.moved(
                Vec3::ZERO,
                center,
                right * grab.along[0] + up * grab.along[1],
            )
        };
        if let Some(c) = doc.assembly.component_mut(id) {
            c.placement = moved;
        }
        doc.solve();
    }

    fn refresh_assembly_scene(&mut self) {
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        if self.asm_scene == Some(doc.revision) {
            return;
        }
        self.asm_scene = Some(doc.revision);
        let first = self.viewport.is_empty();
        let assembly = match &self.mate_preview {
            Some(p) => p.assembly.clone(),
            None => doc.assembly.clone(),
        };
        let mesh = doc.scene_mesh(&assembly);
        self.viewport.set_scene(Scene::from_mesh(&mesh), first);
    }

    pub(crate) fn assembly_shortcuts(&mut self, ctx: &egui::Context) {
        self.dispatch_shortcuts(ctx);
    }

    // ---- Bill of materials -----------------------------------------------------------------

    fn bom_rows(&self) -> Vec<Vec<String>> {
        let Some(doc) = self.asm.as_ref() else {
            return Vec::new();
        };
        let mut rows = vec![
            [
                "Item",
                "Part number",
                "Description",
                "Material",
                "File",
                "Quantity",
            ]
            .map(str::to_owned)
            .to_vec(),
        ];
        let open = |c: &oxyde_assembly::Component, parent: &str| doc.bom_open(c, parent);
        for row in bom::rows(&doc.assembly, "", self.bom_mode, &open) {
            let properties = doc
                .docs
                .get(Path::new(&row.key))
                .and_then(|d| d.as_ref().ok())
                .map(|d| d.properties().clone())
                .unwrap_or_default();
            let file = Path::new(&row.key)
                .file_name()
                .map_or_else(|| row.key.clone(), |n| n.to_string_lossy().into_owned());
            let indent = "    ".repeat(row.level);
            rows.push(vec![
                format!("{indent}{}", row.item),
                properties.part_number,
                properties.description,
                properties.material,
                file,
                row.quantity.to_string(),
            ]);
        }
        rows
    }

    pub(crate) fn bom_window(&mut self, ctx: &egui::Context) {
        if !self.show_bom || self.asm.is_none() {
            return;
        }
        let rows = self.bom_rows();
        let mut open = true;
        let mut export = false;
        egui::Window::new("Bill of materials")
            .open(&mut open)
            .resizable(true)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    use bom::BomMode;
                    ui.label("Show");
                    ui.radio_value(&mut self.bom_mode, BomMode::TopLevel, "Top level")
                        .on_hover_text("Sub-assemblies as one line");
                    ui.radio_value(&mut self.bom_mode, BomMode::PartsOnly, "Parts only")
                        .on_hover_text(
                            "Every part, through every sub-assembly, with total quantities",
                        );
                    ui.radio_value(&mut self.bom_mode, BomMode::Indented, "Indented")
                        .on_hover_text("Sub-assemblies followed by their own parts");
                });
                ui.add_space(4.0);
                egui::Grid::new("bom")
                    .striped(true)
                    .num_columns(6)
                    .show(ui, |ui| {
                        for (i, row) in rows.iter().enumerate() {
                            for cell in row {
                                if i == 0 {
                                    ui.label(RichText::new(cell).strong());
                                } else {
                                    ui.label(cell);
                                }
                            }
                            ui.end_row();
                        }
                    });
                if rows.len() == 1 {
                    ui.label(RichText::new("No components yet.").italics().weak());
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Part number, description and material come from each part's \
                         File › Part properties.",
                    )
                    .small()
                    .weak(),
                );
                if ui.button("Export CSV…").clicked() {
                    export = true;
                }
            });
        if !open {
            self.show_bom = false;
        }
        if export {
            let name = self
                .asm
                .as_ref()
                .map_or_else(|| "Assembly".to_owned(), |d| d.display_name());
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("CSV", &["csv"])
                .set_file_name(format!("{name} BOM.csv"))
                .save_file()
            {
                self.status = match std::fs::write(&path, bom::to_csv(&rows)) {
                    Ok(()) => format!("Exported {}", path.display()),
                    Err(e) => format!("Could not write {}: {e}", path.display()),
                };
            }
        }
    }
}

/// The components of a sub-assembly, indented under it in the tree (read only).
fn sub_tree(
    ui: &mut egui::Ui,
    doc: &AssemblyDoc,
    sub: &crate::assembly::LoadedAssembly,
    indent: f32,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    for c in &sub.assembly.components {
        let inner = sub
            .sources
            .get(&c.id)
            .and_then(|p| doc.docs.get(p))
            .and_then(|d| d.as_ref().ok());
        let nested = match inner {
            Some(crate::assembly::Loaded::Assembly(a)) => Some(a),
            _ => None,
        };
        tree_row(
            ui,
            Row {
                icon: if nested.is_some() {
                    Icon::Assembly
                } else {
                    Icon::Part
                },
                cut: false,
                text: RichText::new(&c.name).weak(),
                highlighted: false,
                badge: inner.is_none().then_some(Icon::Error),
                indent,
                expander: None,
                eye: None,
            },
        )
        .response
        .on_hover_text("Part of the sub-assembly: open the sub-assembly to change it");
        if let Some(nested) = nested {
            sub_tree(ui, doc, nested, indent + 16.0, depth + 1);
        }
    }
}

/// Selects `item`, or toggles it with Ctrl.
fn select(selection: &mut Vec<AsmSelected>, item: AsmSelected, additive: bool) {
    if additive {
        match selection.iter().position(|s| *s == item) {
            Some(i) => {
                selection.remove(i);
            }
            None => selection.push(item),
        }
    } else {
        *selection = vec![item];
    }
}

pub(crate) fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

/// Whether a file is an assembly: by extension, else by its manifest.
pub(crate) fn is_assembly(path: &Path) -> bool {
    has_extension(path, oxyde_format::ASSEMBLY_EXTENSION)
        || oxyde_format::document_kind(path)
            .is_ok_and(|(kind, _)| kind == oxyde_format::DocumentKind::Assembly)
}
