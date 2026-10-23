//! Assembly mode: the component tree with its mates, the Mate command in the
//! PropertyManager, the viewport (picking, Alt+drag to move components), inserting parts,
//! saving and the bill of materials.
//!
//! The app is in assembly mode while `OxydeApp::asm` holds an assembly. Parts are edited in
//! their own documents: *Open Part* leaves the assembly, *Back to assembly* returns to it.
//!
//! The assembly has an origin and standard planes of its own (component 0,
//! [`ComponentId::ASSEMBLY`]), listed at the top of the tree and drawn in the viewport, so
//! that a component can be mated exactly where it should be. The Mate command collects
//! any number of mates before the parts move (as Creo's component placement does): each
//! pair is added to a list, and OK places the parts with all of them at once. By default
//! the parts are previewed where the mates put them as soon as both sides of a pair are
//! picked (Preferences › Graphics › Previews); off, they stay put until OK.

use std::path::Path;

use eframe::egui::{self, Color32, RichText, Stroke};
use oxyde_assembly::{
    Assembly, ComponentId, MateId, MateKind, MateRef, MateStatus, MateTarget, PrimitiveKind,
    Solution, bom, default_alignment_between,
};
use oxyde_geom::{Point3, Vec3};
use oxyde_model::{StandardAxis, StandardPlane};
use oxyde_render::{Pick, Scene, StandardView};

use crate::app::{Overlay, OverlayShape, OxydeApp, PendingAction, overlay_at};
use crate::assembly::{AsmSelected, AssemblyDoc, Occurrence};
use crate::dialogs::plane_name;
use crate::display::Item;
use crate::equation_tree::{
    FolderEvent, assembly_contents, equations_folder, part_contents, qualified,
};
use crate::icons::{self, Icon};
use crate::property_manager::{box_active, box_fill};
use crate::timeline::{Row, tree_row};

/// The tabs of the assembly command bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AsmTab {
    #[default]
    Assembly,
    Evaluate,
    /// The assembly drawn by the path tracer (`render_ui`).
    Render,
    /// The assembly's studies, its parts in contact (`simulation_ui`).
    Simulation,
    /// Generative design, which works on parts for now (`generative_ui`).
    Generative,
}

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
    /// *Lock rotation*, for a Concentric mate between two axes.
    pub lock_rotation: bool,
    /// Mates collected in this command, applied together on OK.
    pub pending: Vec<Draft>,
    /// Move the parts as mates are added, rather than on OK.
    pub preview: bool,
}

impl MateDialog {
    fn kind(&self) -> MateKind {
        self.kind_for(self.choice)
    }

    /// Everything the command would add: the list, and the pair being picked.
    fn drafts(&self) -> Vec<Draft> {
        let mut out = self.pending.clone();
        out.extend(self.draft());
        out
    }

    /// Moves the pair being picked into the list, ready for the next one.
    fn push_draft(&mut self) -> Option<Draft> {
        let draft = self.draft()?;
        self.pending.push(draft.clone());
        self.a = None;
        self.b = None;
        self.second = false;
        self.choice_set = false;
        self.aligned_set = false;
        self.lock_rotation = false;
        Some(draft)
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
            // Only Concentric keeps it; the panel shows it only where it applies.
            lock_rotation: self.lock_rotation && self.choice == MateChoice::Concentric,
        })
    }
}

/// A mate as the Mate command holds it before it is added.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    kind: MateKind,
    a: MateRef,
    b: MateRef,
    aligned: bool,
    lock_rotation: bool,
}

impl Draft {
    /// Makes `mate` this draft. A locked turn is taken again when the lock or the geometry
    /// changes.
    fn apply_to(&self, mate: &mut oxyde_assembly::Mate) {
        let same = mate.a == self.a && mate.b == self.b && mate.lock_rotation == self.lock_rotation;
        mate.kind = self.kind;
        mate.a = self.a.clone();
        mate.b = self.b.clone();
        mate.aligned = self.aligned;
        mate.lock_rotation = self.lock_rotation;
        if !same {
            mate.locked_angle = None;
        }
    }

    /// Adds this draft to `assembly` as a new mate.
    fn add_to(&self, assembly: &mut Assembly) -> MateId {
        let id = assembly.add_mate(self.kind, self.a.clone(), self.b.clone(), self.aligned);
        if let Some(m) = assembly.mate_mut(id) {
            m.lock_rotation = self.lock_rotation;
        }
        id
    }
}

/// The assembly as it would be with the mates being made, shown while the command is open
/// and its preview is on.
pub struct MatePreview {
    drafts: Vec<Draft>,
    pub assembly: Assembly,
    pub solution: Solution,
}

/// A row clicked in the assembly tree.
enum TreeAction {
    Select(AsmSelected),
    /// The assembly's own plane, selected (shown and Normal To).
    SelectPlane(StandardPlane),
    ToggleItem(Item),
    Target(MateRef),
    Toggle(ComponentId),
    /// Shows hidden components and hides shown ones.
    Hide(Vec<ComponentId>),
    Fix(Vec<ComponentId>, bool),
    /// Edit a part or sub-assembly in place, at any depth.
    OpenPart(Occurrence),
    Appearance(ComponentId),
    /// Opens the Materials window on the material of this component's part.
    EditMaterial(ComponentId),
    /// Gives the parts of these components a material of the library (`None`: none).
    Material(Vec<ComponentId>, Option<String>),
    /// Same for the part files inside a sub-assembly.
    MaterialPaths(Vec<std::path::PathBuf>, Option<String>),
    /// Appearance of a component inside a sub-assembly.
    InnerAppearance(Occurrence),
    /// Move Component, starting with this one selected.
    Move(ComponentId),
    /// Selects every component whose part has this material.
    SelectMaterial(String),
    DeleteComponents(Vec<ComponentId>),
    EditMate(MateId),
    FlipMate(MateId),
    SuppressMate(MateId, bool),
    DeleteMate(MateId),
    /// Open or close the Equations folder of the assembly (`None`) or of a part.
    ToggleEquations(Option<ComponentId>),
    /// A variable's quoted name, for the Equations window's field being typed in.
    InsertName(String),
    /// Tools › Equations.
    ManageEquations,
}

/// The row under a part saying what it is made of: `Material: Acier (STEP)`, or none.
/// The hover gives the density and where the material comes from. A click opens the
/// library's materials ([`material_popup`]).
pub(crate) fn material_row(
    ui: &mut egui::Ui,
    material: Option<&oxyde_model::Material>,
    indent: f32,
) -> crate::timeline::RowResponse {
    let (text, hover) = match material {
        Some(m) => {
            let tag = if m.is_from_step() { " (STEP)" } else { "" };
            let density = if m.has_density() {
                format!("{:.0} kg/m³", m.density)
            } else {
                "density unknown: no mass".to_owned()
            };
            let from = if m.is_from_step() {
                "named by the STEP file the part was imported from"
            } else {
                "from the Oxyde material library"
            };
            (
                RichText::new(format!("Material: {}{tag}", m.name)),
                format!(
                    "{}\n{density} · {from}\nSaved in the part file · click to change it",
                    m.name
                ),
            )
        }
        None => (
            RichText::new("Material: none").weak().italics(),
            "No material: click to give the part one. The mass and simulations need it.".to_owned(),
        ),
    };
    let row = tree_row(
        ui,
        Row {
            icon: Icon::Materials,
            cut: false,
            text,
            highlighted: false,
            hovered: false,
            badge: None,
            indent,
            expander: None,
            eye: None,
        },
    );
    crate::timeline::RowResponse {
        response: row.response.on_hover_text(hover),
        ..row
    }
}

/// The library's materials in a list below a material row, opened by clicking the row,
/// so a part gets its material in two clicks. Returns the choice as [`material_menu`]
/// does.
pub(crate) fn material_popup(
    ui: &mut egui::Ui,
    row: &egui::Response,
    names: &[String],
) -> Option<Option<String>> {
    let popup = row.id.with("material_popup");
    if row.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    let choice = egui::popup::popup_below_widget(
        ui,
        popup,
        row,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(200.0);
            material_menu(ui, names)
        },
    )
    .flatten();
    if choice.is_some() {
        ui.memory_mut(|m| m.close_popup());
    }
    choice
}

/// The entries of a Material menu: the library's materials and "None". Returns the choice
/// (`Some(None)` for no material) once one is clicked.
pub(crate) fn material_menu(ui: &mut egui::Ui, names: &[String]) -> Option<Option<String>> {
    let mut choice = None;
    egui::ScrollArea::vertical()
        .max_height(300.0)
        .show(ui, |ui| {
            for name in names {
                if ui.button(name).clicked() {
                    choice = Some(Some(name.clone()));
                    ui.close_menu();
                }
            }
            if names.is_empty() {
                ui.label(RichText::new("The library is empty").weak());
            }
            ui.separator();
            if ui.button("None").clicked() {
                choice = Some(None);
                ui.close_menu();
            }
        });
    choice
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

/// A component's name; the assembly itself for its own planes and origin.
fn component_label(doc: &AssemblyDoc, id: ComponentId) -> String {
    if id == ComponentId::ASSEMBLY {
        return format!("{} (assembly)", doc.display_name());
    }
    doc.component(id)
        .map_or_else(|| "(missing)".to_owned(), |c| c.name.clone())
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
    let owner = component_label(doc, r.component);
    // A part inside a sub-assembly: "Face of Pin<2> in Arm<1>".
    match doc.inner_names(r.component, &r.path).last() {
        Some(inner) => format!("{what} of {inner} in {owner}"),
        None => format!("{what} of {owner}"),
    }
}

/// "Coincident: Face of A<1> — Top Plane of B<1>", for the list of mates to add.
fn describe_draft(doc: &AssemblyDoc, d: &Draft) -> String {
    format!(
        "{}: {} — {}{}",
        d.kind.label(),
        describe_ref(doc, &d.a),
        describe_ref(doc, &d.b),
        if d.lock_rotation {
            ", rotation locked"
        } else {
            ""
        }
    )
}

/// A selection box of the Mate command.
fn mate_box(ui: &mut egui::Ui, text: Option<String>, active: bool, hint: &str) -> (bool, bool) {
    let stroke = if active {
        Stroke::new(2.0_f32, box_active())
    } else {
        Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color)
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

    /// Whether the open document has changes to save: a part's include the sketch being
    /// edited, whose drawing is written into the part only when it is left (closing the
    /// window or opening another file meanwhile must still ask).
    pub(crate) fn is_modified(&self) -> bool {
        match &self.asm {
            Some(doc) => doc.modified,
            None => self.doc.modified || self.sketch.as_ref().is_some_and(|s| s.changed()),
        }
    }

    /// Switches to `doc`, loading its parts and solving its mates on this thread (a new,
    /// empty assembly; files go through [`OxydeApp::open_assembly_job`]).
    pub(crate) fn enter_assembly(&mut self, mut doc: AssemblyDoc) {
        doc.mesh_fraction = self.mesh_fraction();
        let notes = doc.load_parts(&mut self.kernel);
        doc.solve();
        self.enter_assembly_loaded(doc, notes);
    }

    /// Switches to `doc`, its parts loaded and its mates solved.
    pub(crate) fn enter_assembly_loaded(&mut self, doc: AssemblyDoc, notes: Vec<String>) {
        self.leave_assembly();
        self.close_dialog();
        self.mesh_export = None;
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
        self.leave_assembly_panels();
    }

    /// Closes the assembly's panels and previews, keeping the assembly.
    pub(crate) fn leave_assembly_panels(&mut self) {
        self.mate_dialog = None;
        self.mate_preview = None;
        self.show_bom = false;
        self.show_mass_properties = false;
        self.show_measure = false;
        self.interference = None;
        self.asm_plane = None;
        self.asm_scroll_to = None;
        self.asm_tab = AsmTab::Assembly;
        self.viewport.grab_enabled = false;
        self.viewport.grab_without_alt = false;
        self.move_mode = false;
        self.scene_revision = None;
    }

    /// Opens an assembly file, taking its edit lock as `mode` says.
    pub(crate) fn open_assembly_with(&mut self, path: &Path, mode: crate::edit_in_place::LockMode) {
        let read_only = match self.claim(path, mode) {
            Ok(read_only) => read_only,
            Err(holder) => {
                self.ask_about_lock(
                    path,
                    holder,
                    crate::edit_in_place::OpenRequest::Path(path.to_path_buf()),
                );
                return;
            }
        };
        match oxyde_format::load_assembly(path) {
            Ok(file) => {
                let mut doc = AssemblyDoc::from_file(file, Some(path.to_path_buf()));
                doc.read_only = read_only;
                // Its parts are loaded off the UI thread; entered when they are.
                self.open_assembly_job(doc, read_only);
            }
            Err(e) => self.status = format!("Could not open {}: {e}", path.display()),
        }
    }

    pub(crate) fn save_assembly(&mut self, choose: bool) -> bool {
        let Some(doc) = self.asm.as_ref() else {
            return false;
        };
        let path = match (&doc.path, choose || doc.read_only) {
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
        let new_name = self
            .asm
            .as_ref()
            .is_some_and(|d| d.path.as_deref() != Some(path.as_path()) || d.read_only);
        if new_name {
            if let Err(holder) = self.locks.acquire(&path) {
                self.status = format!(
                    "Cannot save over {}: it is being edited by {}",
                    path.display(),
                    holder.describe()
                );
                return false;
            }
        }
        let Some(doc) = self.asm.as_mut() else {
            return false;
        };
        // Under another name it is another document (see `save_as` for parts).
        let original_id = doc.document_id.clone();
        if doc.path.as_deref().is_some_and(|p| p != path) {
            doc.document_id = oxyde_format::new_document_id();
        }
        // Part paths are stored relative to where the assembly is.
        doc.relocate(&path);
        match oxyde_format::save_assembly(&path, &doc.to_file()) {
            Ok(()) => {
                doc.path = Some(path.clone());
                doc.modified = false;
                doc.read_only = false;
                // The parts its equations changed are saved with it, as SolidWorks does,
                // with what it drives in them; after Save as, by the new file.
                if doc.document_id != original_id {
                    doc.take_over_driven(&original_id);
                } else {
                    doc.record_driven();
                }
                let (saved, notes) = doc.save_changed_parts();
                self.remember_recent(&path);
                self.status = format!("Saved {}", path.display());
                if saved > 0 {
                    let plural = if saved == 1 { "" } else { "s" };
                    self.status.push_str(&format!(" and {saved} part{plural}"));
                }
                if !notes.is_empty() {
                    self.status
                        .push_str(&format!("; not saved: {}", notes.join("; ")));
                }
                true
            }
            Err(e) => {
                doc.document_id = original_id;
                self.status = format!("Could not save {}: {e}", path.display());
                false
            }
        }
    }

    /// Opens a component's part or sub-assembly for editing in place.
    pub(crate) fn open_component(&mut self, o: Occurrence) {
        self.open_component_with(o, crate::edit_in_place::LockMode::Lock);
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
            self.insert_part(&path, None);
        }
    }

    /// Inserts the part or assembly file at `path`: at `placement`, fixed, or in a free
    /// spot to be mated.
    pub(crate) fn insert_part(
        &mut self,
        path: &Path,
        placement: Option<oxyde_assembly::Placement>,
    ) {
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
        let fixed_at = placement;
        let placement = placement.unwrap_or_else(|| doc.free_spot(doc.file_bounds(path)));
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
        if fixed_at.is_some()
            && let Some(c) = doc.assembly.component_mut(id)
        {
            c.fixed = true;
        }
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
            lock_rotation: false,
            pending: Vec::new(),
            preview: self.prefs.previews.mates,
        };
        for r in refs.into_iter().take(2) {
            self.mate_pick(&mut dialog, r);
        }
        self.mate_dialog = Some(dialog);
        self.mate_preview = None;
        if let Some(doc) = self.asm.as_mut() {
            doc.selection.clear();
        }
        self.status = if self.prefs.previews.mates {
            "Pick a face, edge or plane on each of two components: the parts are shown where \
             the mate puts them; add as many mates as needed, then OK"
        } else {
            "Pick a face, edge or plane on each of two components; add as many mates as \
             needed, then OK places the parts"
        }
        .to_owned();
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
            lock_rotation: mate.lock_rotation,
            pending: Vec::new(),
            preview: true,
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
                // Between the faces or edges of two solids, square faces start touching.
                let solid =
                    |r: &MateRef| matches!(r.target, MateTarget::Face(_) | MateTarget::Edge(_));
                d.aligned = default_alignment_between(&pa, &pb, solid(a) && solid(b));
            }
        }
    }

    /// Adds every mate the command collected (or changes the one being edited) and
    /// solves once, so the parts move with all of them holding.
    fn apply_mate(&mut self, d: MateDialog) {
        let drafts = d.drafts();
        if drafts.is_empty() {
            self.mate_preview = None;
            self.bump_asm();
            return;
        }
        // The preview is reused only if it shows exactly these mates.
        let preview = self.mate_preview.take().filter(|p| p.drafts == drafts);
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        doc.checkpoint();
        let mut ids = Vec::new();
        for draft in drafts {
            let id = match d.editing {
                Some(id) => {
                    if let Some(m) = doc.assembly.mate_mut(id) {
                        draft.apply_to(m);
                    }
                    id
                }
                None => draft.add_to(&mut doc.assembly),
            };
            ids.push(id);
        }
        match preview {
            // The preview already holds the solved placements.
            Some(p) if ids.iter().all(|id| p.assembly.mate(*id).is_some()) => {
                doc.assembly = p.assembly;
                doc.solution = Some(p.solution);
                doc.revision = crate::document::next_revision();
            }
            _ => doc.solve(),
        }
        let names: Vec<String> = ids
            .iter()
            .filter_map(|id| doc.assembly.mate(*id).map(|m| m.name.clone()))
            .collect();
        let errors: Vec<String> = ids
            .iter()
            .filter_map(
                |id| match doc.solution.as_ref().and_then(|s| s.status(*id)) {
                    Some(MateStatus::Error(e)) => Some(format!(
                        "{}: {e}",
                        doc.assembly.mate(*id).map_or("?", |m| m.name.as_str())
                    )),
                    _ => None,
                },
            )
            .collect();
        self.status = if errors.is_empty() {
            match d.editing {
                Some(_) => format!("{} changed", names.join(", ")),
                None => format!("Added {}", names.join(", ")),
            }
        } else {
            errors.join("; ")
        };
    }

    /// Keeps the preview in step with the Mate command, while its preview is on.
    fn refresh_mate_preview(&mut self) {
        let drafts = match self.mate_dialog.as_ref() {
            Some(d) if d.preview => d.drafts(),
            _ => Vec::new(),
        };
        if drafts.is_empty() {
            if self.mate_preview.take().is_some() {
                self.bump_asm();
            }
            return;
        }
        if self
            .mate_preview
            .as_ref()
            .is_some_and(|p| p.drafts == drafts)
        {
            return;
        }
        let editing = self.mate_dialog.as_ref().and_then(|d| d.editing);
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let mut assembly = doc.assembly.clone();
        for draft in &drafts {
            match editing.and_then(|id| assembly.mate_mut(id)) {
                Some(m) => draft.apply_to(m),
                None => {
                    draft.add_to(&mut assembly);
                }
            }
        }
        let solution = doc.solve_assembly(&mut assembly);
        self.mate_preview = Some(MatePreview {
            drafts,
            assembly,
            solution,
        });
        self.bump_asm();
    }

    fn bump_asm(&mut self) {
        if let Some(doc) = self.asm.as_mut() {
            doc.revision = crate::document::next_revision();
        }
    }

    /// Move Component on or off: while on, a plain left-drag in the viewport moves the
    /// component under the pointer (Shift+drag turns it), as SolidWorks' Move Component.
    pub(crate) fn toggle_move_mode(&mut self) {
        self.move_mode = !self.move_mode;
        self.move_handle = None;
        self.handle_shapes.clear();
        self.status = if self.move_mode {
            "Move Component: drag a component, or an arrow of its handle along X, Y or Z; \
             Shift+drag turns it; Esc ends"
                .to_owned()
        } else {
            "Move Component ended".to_owned()
        };
    }

    pub(crate) fn set_fixed(&mut self, ids: &[ComponentId], fixed: bool) {
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        doc.checkpoint();
        for id in ids {
            if let Some(c) = doc.assembly.component_mut(*id) {
                c.fixed = fixed;
            }
        }
        doc.solve();
        if ids.len() > 1 {
            self.status = format!(
                "{} components {}",
                ids.len(),
                if fixed { "fixed" } else { "floating" }
            );
        }
    }

    /// Right-click › Material on components: gives every part under them the library
    /// material `name` (or no material), in memory and in their files, since parts are
    /// documents of their own.
    pub(crate) fn apply_component_material(
        &mut self,
        targets: &[ComponentId],
        name: Option<String>,
    ) {
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        for id in targets {
            for p in doc.part_paths_under(*id) {
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
        self.apply_material_to_paths(&paths, name);
    }

    /// Gives the part files `paths` a material of the library (`None`: none): the files
    /// are rewritten and the loaded copies updated. A part with appearances of its own
    /// asks first, since the material's look replaces them.
    pub(crate) fn apply_material_to_paths(
        &mut self,
        paths: &[std::path::PathBuf],
        name: Option<String>,
    ) {
        let material = match &name {
            Some(n) => match self.materials.get(n) {
                Some(m) => Some(m.clone()),
                None => {
                    self.status = format!("{n} is not in the material library");
                    return;
                }
            },
            None => None,
        };
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        if paths.is_empty() {
            self.status = "No part file to give a material to".to_owned();
            return;
        }
        let with_appearances = paths
            .iter()
            .filter(|p| {
                matches!(doc.docs.get(*p), Some(Ok(crate::assembly::Loaded::Part(part)))
                    if !part.part.appearances().is_empty())
            })
            .count();
        if let (Some(name), true) = (name, with_appearances > 0) {
            self.confirm = Some(crate::materials_ui::Confirm::Material {
                paths: paths.to_vec(),
                name,
                with_appearances,
            });
            return;
        }
        self.write_material_to_paths(paths, material, false);
    }

    /// Writes `material` into the part files `paths` and their loaded copies; with
    /// `clear`, their own appearances go too.
    pub(crate) fn write_material_to_paths(
        &mut self,
        paths: &[std::path::PathBuf],
        material: Option<oxyde_model::Material>,
        clear: bool,
    ) {
        let name = material.as_ref().map(|m| m.name.clone());
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        let mut done = 0;
        let mut failed = Vec::new();
        for path in paths {
            // A part open for editing in another window is left to it, as Save is.
            if let Some(holder) = crate::lockfile::held_elsewhere(path) {
                failed.push(format!(
                    "{}: being edited by {}",
                    path.display(),
                    holder.describe()
                ));
                continue;
            }
            let built = match doc.docs.get(path) {
                Some(Ok(crate::assembly::Loaded::Part(p))) => Some(p.part.definition()),
                _ => None,
            };
            // The file first: a part that cannot be saved is left as it was.
            let saved = oxyde_format::load_part(path).and_then(|mut file| {
                // A part file the import of a STEP assembly wrote and nobody rebuilt on
                // its own still asks its import to apply the STEP colours and material on
                // the next rebuild, where the loaded copy has them already: they go into
                // the file as the loaded copy has them, or a material removed here would
                // come back on reopening.
                if let Some(built) = &built {
                    file.definition.settle_imports(built);
                }
                let old_name = file.definition.material.as_ref().map(|m| m.name.clone());
                file.definition.material = material.clone();
                // The loaded copy shows its imported STEP colours as appearances, but the
                // file may not hold them yet: it still asks the import to apply them on
                // the next rebuild, which would cover the material again on reopening.
                if clear {
                    file.definition.clear_appearances();
                }
                file.properties.material = match &material {
                    Some(m) => m.name.clone(),
                    // A removed material clears the property only when it was the
                    // material's own name.
                    None if old_name.as_deref() == Some(file.properties.material.as_str()) => {
                        String::new()
                    }
                    None => file.properties.material.clone(),
                };
                oxyde_format::save_part(path, &file)
            });
            match saved {
                Ok(()) => {
                    if let Some(Ok(crate::assembly::Loaded::Part(p))) = doc.docs.get_mut(path) {
                        let old_name = p.part.material().map(|m| m.name.clone());
                        p.part.set_material(material.clone());
                        if clear {
                            *p.part.appearances_mut() = Default::default();
                        }
                        match &material {
                            Some(m) => p.properties.material = m.name.clone(),
                            None if old_name.as_deref() == Some(p.properties.material.as_str()) => {
                                p.properties.material.clear();
                            }
                            None => {}
                        }
                    }
                    done += 1;
                }
                Err(e) => failed.push(format!("{}: {e}", path.display())),
            }
        }
        doc.revision = crate::document::next_revision();
        self.refresh_materials();
        self.status = match (&name, failed.is_empty()) {
            (Some(n), true) => format!("{n} applied to {done} part file(s)"),
            (None, true) => format!("Material removed from {done} part file(s)"),
            (_, false) => format!("{done} part file(s) changed; failed: {}", failed.join("; ")),
        };
    }

    /// Selects every top-level component that is, or holds, a part made of `name`.
    pub(crate) fn select_parts_with_material(&mut self, name: &str) {
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        let mut selection = Vec::new();
        for c in &doc.assembly.components {
            let has = doc.part_paths_under(c.id).iter().any(|p| {
                matches!(
                    doc.docs.get(p),
                    Some(Ok(crate::assembly::Loaded::Part(part)))
                        if part.part.material().is_some_and(|m| m.name == name)
                )
            });
            if has {
                selection.push(AsmSelected::Component(c.id));
            }
        }
        let count = selection.len();
        doc.selection = selection;
        doc.revision = crate::document::next_revision();
        self.status = format!("{count} component(s) made of {name}");
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
        // Mates go with the components they hold: said, as SolidWorks lists them.
        let mut mates_gone: Vec<String> = Vec::new();
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
                    mates_gone.extend(doc.assembly.remove_component(*c));
                    removed += 1;
                }
                _ => {}
            }
        }
        if removed > 0 {
            doc.solve();
            self.status = if mates_gone.is_empty() {
                format!("Deleted {removed} item(s)")
            } else {
                format!(
                    "Deleted {removed} item(s), and the mates that held them: {} (Undo brings them back)",
                    mates_gone.join(", ")
                )
            };
        }
    }

    pub(crate) fn reload_assembly_parts(&mut self) {
        if self.asm.is_some() && !self.busy() {
            self.reload_assembly_job();
        }
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
                self.open_recent_menu(ui, &ctx);
                if ui
                    .button("Import STEP assembly…")
                    .on_hover_text("One part file per part and one assembly file per assembly")
                    .clicked()
                {
                    ui.close_menu();
                    self.guard(PendingAction::ImportStepAssembly, &ctx);
                }
                self.finish_editing_item(ui);
                ui.separator();
                if ui.button("Save").clicked() {
                    ui.close_menu();
                    self.save_assembly(false);
                }
                if ui.button("Save as…").clicked() {
                    ui.close_menu();
                    self.save_assembly(true);
                }
                self.export_menu(ui);
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
            ui.menu_button("View", |ui| {
                for (label, view) in [
                    ("Front (Ctrl+1)", StandardView::Front),
                    ("Top (Ctrl+5)", StandardView::Top),
                    ("Right (Ctrl+4)", StandardView::Right),
                    ("Isometric (Ctrl+7)", StandardView::Isometric),
                ] {
                    if ui.button(label).clicked() {
                        ui.close_menu();
                        self.viewport.set_view(view);
                    }
                }
                ui.separator();
                if ui.button("Zoom to fit (F)").clicked() {
                    ui.close_menu();
                    self.viewport.request_fit();
                }
                ui.separator();
                self.constrained_orbit_item(ui);
                ui.separator();
                ui.menu_button("Display Style", |ui| {
                    if self.display_style_items(ui) {
                        ui.close_menu();
                    }
                });
                self.floor_reflections_item(ui);
                self.ambient_occlusion_item(ui);
                ui.menu_button("Hide/Show", |ui| self.visibility_menu(ui));
            });
            ui.menu_button("Tools", |ui| {
                if ui.button("Mass properties").clicked() {
                    ui.close_menu();
                    self.show_mass_properties = true;
                }
                if ui.button("Measure").clicked() {
                    ui.close_menu();
                    self.show_measure = true;
                }
                ui.separator();
                if ui
                    .button("Equations…")
                    .on_hover_text(
                        "Global variables, and equations driving mates and part dimensions",
                    )
                    .clicked()
                {
                    ui.close_menu();
                    self.equations
                        .show(crate::equations_ui::EquationsTab::Equations);
                }
                ui.separator();
                if ui
                    .button("Materials…")
                    .on_hover_text(
                        "The material library; editing a material changes every part made of it",
                    )
                    .clicked()
                {
                    ui.close_menu();
                    self.open_materials_window();
                }
                ui.separator();
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
                    let help = self.prefs.navigation.mouse.help();
                    self.status = format!(
                        "Orbit: {}. Pan: {}. Middle double-click fits. Alt+left-drag moves a \
                         component, Alt+Shift+left-drag turns it",
                        help[0].1, help[1].1
                    );
                }
                if ui.button("About Oxyde").clicked() {
                    ui.close_menu();
                    self.open_dialog(crate::dialogs::Dialog::About);
                }
            });
        });
    }

    pub(crate) fn assembly_command_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            for (tab, label) in [
                (AsmTab::Assembly, "Assembly"),
                (AsmTab::Evaluate, "Evaluate"),
                (AsmTab::Render, "Render"),
                (AsmTab::Simulation, "Simulation"),
                (AsmTab::Generative, "Generative Design"),
            ] {
                let text = RichText::new(label).size(13.0);
                let text = if self.asm_tab == tab {
                    text.strong()
                } else {
                    text
                };
                if ui.selectable_label(self.asm_tab == tab, text).clicked() {
                    self.asm_tab = tab;
                    if tab == AsmTab::Generative {
                        self.enter_generative();
                    }
                    if tab == AsmTab::Simulation {
                        self.enter_simulation();
                    }
                }
            }
            self.assistant_toggle(ui);
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
        if self.asm_tab == AsmTab::Evaluate {
            ui.horizontal(|ui| self.assembly_evaluate_tab(ui));
            return;
        }
        if self.asm_tab == AsmTab::Render {
            ui.horizontal(|ui| self.render_tab(ui));
            return;
        }
        if self.asm_tab == AsmTab::Simulation {
            ui.horizontal(|ui| self.simulation_tab(ui));
            return;
        }
        if self.asm_tab == AsmTab::Generative {
            ui.horizontal(|ui| self.generative_tab(ui));
            return;
        }
        // Fix/Float: every selected component, named after the first.
        let selected = self.asm.as_ref().and_then(|d| {
            let ids: Vec<ComponentId> = d
                .selection
                .iter()
                .filter_map(AsmSelected::component)
                .collect();
            let first = d.component(*ids.first()?)?;
            Some((ids, first.fixed))
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
            let any = self
                .asm
                .as_ref()
                .is_some_and(|d| !d.assembly.components.is_empty());
            if icons::command_button(ui, Icon::Move, "Move\nComponent", any, self.move_mode)
                .on_hover_text(
                    "Drag a component to move it freely, Shift+drag to turn it, before \
                     mating it (Esc ends; Alt+drag does the same at any time)",
                )
                .clicked()
                && any
            {
                self.toggle_move_mode();
            }
            let fix_label = match &selected {
                Some((_, true)) => "Float",
                _ => "Fix",
            };
            let fix = icons::command_button(ui, Icon::Part, fix_label, selected.is_some(), false)
                .on_hover_text(
                    "A fixed component never moves; a floating one follows its mates \
                     (every selected component)",
                );
            if fix.clicked() {
                if let Some((ids, fixed)) = &selected {
                    self.set_fixed(ids, !fixed);
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
            if icons::command_button(
                ui,
                Icon::Materials,
                "Materials",
                true,
                self.materials_window.open,
            )
            .on_hover_text(
                "Materials and their appearances: edit one and every part made of it \
                 follows; right-click a component for its own material or appearance",
            )
            .clicked()
            {
                self.materials_window.open = !self.materials_window.open;
            }
            if icons::command_button(ui, Icon::Equations, "Equations", true, self.equations.open)
                .on_hover_text("Global variables, and equations driving mates and part dimensions")
                .clicked()
            {
                self.equations
                    .show(crate::equations_ui::EquationsTab::Equations);
            }
        });
    }

    /// The Evaluate tab of an assembly: mass properties of the whole, measurements of
    /// the selection, and reloading the part files.
    fn assembly_evaluate_tab(&mut self, ui: &mut egui::Ui) {
        let has_parts = self
            .asm
            .as_ref()
            .is_some_and(|d| !d.assembly.components.is_empty());
        if icons::command_button(
            ui,
            Icon::MassProperties,
            "Mass Properties",
            has_parts,
            self.show_mass_properties,
        )
        .on_hover_text("Volume and size of the whole assembly, counts of parts and mates")
        .clicked()
        {
            self.show_mass_properties = !self.show_mass_properties;
        }
        if icons::command_button(ui, Icon::Measure, "Measure", has_parts, self.show_measure)
            .on_hover_text(
                "Areas and lengths of the selected faces and edges, the angle and distance \
                 between two of them",
            )
            .clicked()
        {
            self.show_measure = !self.show_measure;
        }
        self.section_view_button(ui, has_parts);
        let two_parts = self
            .asm
            .as_ref()
            .is_some_and(|d| d.leaves(&d.assembly).len() >= 2);
        self.interference_button(ui, two_parts);
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        if icons::command_button(ui, Icon::Rebuild, "Reload\nParts", true, false)
            .on_hover_text("Read the part files again, after editing them (Ctrl+B)")
            .clicked()
        {
            self.reload_assembly_parts();
        }
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
        if self.asm_tab == AsmTab::Render {
            self.render_panel(ui);
        }
        if self.asm_tab == AsmTab::Simulation {
            self.simulation_panel(ui);
        }
        if self.asm_tab == AsmTab::Generative {
            self.generative_panel(ui);
        }
        self.mate_manager(ui);
        self.refresh_mate_preview();
        self.property_manager(ui);
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
        let driven = d.editing.is_some_and(|id| doc.assembly.is_mate_driven(id));
        let text_a = d.a.as_ref().map(|r| describe_ref(doc, r));
        let text_b = d.b.as_ref().map(|r| describe_ref(doc, r));
        // What the preview says about the mates being made: the edited one, else the
        // ones beyond the assembly's own.
        let preview_error = self.mate_preview.as_ref().and_then(|p| {
            let new: Vec<MateId> = match d.editing {
                Some(id) => vec![id],
                None => p
                    .assembly
                    .mates
                    .iter()
                    .skip(doc.assembly.mates.len())
                    .map(|m| m.id)
                    .collect(),
            };
            new.iter().find_map(|id| match p.solution.status(*id) {
                Some(MateStatus::Error(e)) => Some(format!(
                    "{}: {e}",
                    p.assembly.mate(*id).map_or("?", |m| m.name.as_str())
                )),
                _ => None,
            })
        });
        let pending: Vec<String> = d.pending.iter().map(|p| describe_draft(doc, p)).collect();
        let mut answer = None;
        let mut add = false;
        let mut remove = None;
        let mut preview_changed = false;
        egui::Frame::none()
            .fill(ui.visuals().faint_bg_color)
            .stroke(Stroke::new(
                1.0_f32,
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
                        let can_ok = ready || !d.pending.is_empty();
                        let ok = icons::icon_button(ui, Icon::Ok, 24.0, can_ok);
                        if ok
                            .on_hover_text(if d.editing.is_some() {
                                "OK (Enter)"
                            } else if can_ok {
                                "OK: add the mates and place the parts"
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
                    // A value an equation drives is changed in Tools › Equations.
                    MateChoice::Distance | MateChoice::Angle if driven => {
                        ui.horizontal(|ui| {
                            let (label, text) = if d.choice == MateChoice::Distance {
                                ("Distance", format!("Σ {} mm", d.distance))
                            } else {
                                ("Angle", format!("Σ {}°", d.angle))
                            };
                            ui.label(label);
                            ui.label(text).on_hover_text(
                                "Driven by an equation of the assembly (Tools › Equations)",
                            );
                        });
                    }
                    MateChoice::Distance => {
                        ui.horizontal(|ui| {
                            ui.label("Distance");
                            ui.add(
                                egui::DragValue::new(&mut d.distance)
                                    .speed(0.1)
                                    .range(0.0..=crate::dialogs::MAX_LENGTH)
                                    .suffix(" mm")
                                    .custom_parser(crate::dialogs::length_parser),
                            );
                        });
                    }
                    MateChoice::Angle => {
                        ui.horizontal(|ui| {
                            ui.label("Angle");
                            ui.add(
                                egui::DragValue::new(&mut d.angle)
                                    .speed(0.5)
                                    .range(0.0..=180.0)
                                    .suffix("°")
                                    .custom_parser(crate::dialogs::angle_parser),
                            );
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
                    if d.kind().can_lock_rotation(a, b) {
                        ui.checkbox(&mut d.lock_rotation, "Lock rotation")
                            .on_hover_text(
                                "Keeps the parts from turning about the shared axis, as they \
                                 are now: a screw or a foot is then held by this and one \
                                 Coincident mate",
                            );
                    }
                }
                if d.editing.is_none() {
                    ui.add_space(6.0);
                    crate::property_manager::section(ui, "Mates to add");
                    for (i, text) in pending.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if icons::icon_button(ui, Icon::Cancel, 16.0, true)
                                .on_hover_text("Remove from the list")
                                .clicked()
                            {
                                remove = Some(i);
                            }
                            ui.add(egui::Label::new(text).truncate());
                        });
                    }
                    if pending.is_empty() {
                        let text = if d.preview {
                            "None yet."
                        } else {
                            "None yet: the parts stay put until OK."
                        };
                        ui.label(RichText::new(text).italics().weak());
                    }
                    ui.add_space(2.0);
                    if ui
                        .add_enabled(ready, egui::Button::new("Add this mate (Enter)"))
                        .on_hover_text(
                            "Keep this mate in the list and pick the next pair; OK adds them \
                             all and places the parts",
                        )
                        .clicked()
                    {
                        add = true;
                    }
                }
                ui.add_space(4.0);
                if ui
                    .checkbox(
                        &mut d.preview,
                        "Preview: move the parts as mates are picked",
                    )
                    .on_hover_text(
                        "The parts go where the mates put them as soon as both sides are \
                         picked. Off, they stay where they are until OK, so every mate can \
                         be picked where the parts sit now",
                    )
                    .changed()
                {
                    preview_changed = true;
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
        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            // Editing: done. Otherwise Enter keeps the mate and goes on to the next pair;
            // with nothing picked and a list waiting, it is OK.
            if ready && d.editing.is_some() {
                answer = Some(true);
            } else if ready {
                add = true;
            } else if !d.pending.is_empty() && d.a.is_none() && d.b.is_none() {
                answer = Some(true);
            }
        }
        if let Some(i) = remove {
            if i < d.pending.len() {
                d.pending.remove(i);
            }
        }
        if preview_changed {
            self.prefs.previews.mates = d.preview;
            self.save_preferences();
        }
        if d.a.is_some() && d.b.is_some() {
            self.mate_defaults(&mut d);
        }
        if add && answer.is_none() {
            if let Some(draft) = d.push_draft() {
                let n = d.pending.len();
                self.status = format!(
                    "{} kept ({n} in the list): pick the next pair, or OK to place the parts",
                    draft.kind.label()
                );
            }
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
        self.edit_in_place_banner(ui);
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let mating = self.mate_dialog.is_some();
        // A field of the Equations window is being typed in: variables click into it.
        let inserting = self.variables_clickable();
        let material_names: Vec<String> = self
            .materials
            .materials()
            .iter()
            .map(|m| m.name.clone())
            .collect();
        let mut action = None;
        // What the pointer is over in the tree, lit up in the viewport.
        let mut hovered: Vec<Hovered> = Vec::new();
        let mut hovered_refs: Vec<MateRef> = Vec::new();
        // And the other way round: the component under the pointer in the viewport.
        let under_pointer = self
            .viewport
            .hovered()
            .and_then(|p| doc.picked(p))
            .and_then(|s| s.component());
        let scroll_to = self.asm_scroll_to;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                // The assembly's own planes and origin: fixed, to mate the first parts to.
                for plane in [
                    StandardPlane::Front,
                    StandardPlane::Top,
                    StandardPlane::Right,
                ] {
                    let item = Item::Plane(plane);
                    let row = tree_row(
                        ui,
                        Row {
                            icon: Icon::Plane,
                            cut: false,
                            text: RichText::new(format!("{} Plane", plane_name(plane))),
                            highlighted: self.asm_plane == Some(plane),
                            hovered: false,
                            badge: None,
                            indent: 0.0,
                            expander: None,
                            eye: Some(self.item_shown(item)),
                        },
                    );
                    let response = row.response.on_hover_text(if mating {
                        "The assembly's own plane: click to use it in the mate"
                    } else {
                        "The assembly's own plane, which never moves"
                    });
                    if row.eye_clicked {
                        action = Some(TreeAction::ToggleItem(item));
                    } else if response.clicked() {
                        action = Some(if mating {
                            TreeAction::Target(MateRef::new(
                                ComponentId::ASSEMBLY,
                                MateTarget::Plane(plane),
                            ))
                        } else {
                            TreeAction::SelectPlane(plane)
                        });
                    }
                }
                let origin = tree_row(
                    ui,
                    Row {
                        icon: Icon::Origin,
                        cut: false,
                        text: RichText::new("Origin"),
                        highlighted: false,
                        hovered: false,
                        badge: None,
                        indent: 0.0,
                        expander: None,
                        eye: Some(self.item_shown(Item::Origin)),
                    },
                );
                let response = origin.response.on_hover_text(if mating {
                    "The assembly's origin: click to use it in the mate"
                } else {
                    "The assembly's origin, which never moves"
                });
                if origin.eye_clicked {
                    action = Some(TreeAction::ToggleItem(Item::Origin));
                } else if response.clicked() && mating {
                    action = Some(TreeAction::Target(MateRef::new(
                        ComponentId::ASSEMBLY,
                        MateTarget::Origin,
                    )));
                }
                // The assembly's global variables and equations, as SolidWorks' Equations
                // folder.
                let contents = assembly_contents(doc, false);
                if !contents.is_empty() {
                    let open = self.asm_equations_expanded || inserting;
                    match equations_folder(ui, &contents, 0.0, open, inserting, true) {
                        Some(FolderEvent::Toggle) => {
                            action = Some(TreeAction::ToggleEquations(None));
                        }
                        Some(FolderEvent::Manage) => action = Some(TreeAction::ManageEquations),
                        Some(FolderEvent::Insert(name)) => {
                            action = Some(TreeAction::InsertName(name));
                        }
                        None => {}
                    }
                }
                ui.add_space(3.0);
                ui.separator();
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
                    // External references, as SolidWorks marks them: "->" for a part this
                    // assembly drives, "->?" for one another assembly does.
                    let external = match doc.loaded(c.id) {
                        Some(crate::assembly::Loaded::Part(p)) => {
                            let drivers = p.part.driven_by();
                            if drivers.iter().any(|d| d.assembly_id == doc.document_id) {
                                " ->"
                            } else if drivers.is_empty() {
                                ""
                            } else {
                                " ->?"
                            }
                        }
                        _ => "",
                    };
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
                            text: RichText::new(format!("{prefix}{}{external}", c.name)),
                            highlighted: selected,
                            hovered: under_pointer == Some(c.id),
                            badge: error.map(|_| Icon::Error),
                            indent: 0.0,
                            expander: Some(expanded),
                            eye: Some(!c.hidden),
                        },
                    );
                    let id = c.id;
                    if scroll_to == Some(id) {
                        row.response.scroll_to_me(Some(egui::Align::Center));
                    }
                    if row.response.hovered() {
                        hovered.push(Hovered::Item(AsmSelected::Component(id)));
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
                        action = Some(TreeAction::Hide(vec![id]));
                    } else if row.response.double_clicked() {
                        action = Some(TreeAction::OpenPart(Occurrence::top(id)));
                    } else if row.response.clicked() && !mating {
                        action = Some(TreeAction::Select(AsmSelected::Component(id)));
                    }
                    // Fix, Hide, Material and Delete act on every selected component when
                    // this one is among them, else on this one alone.
                    let mut targets: Vec<ComponentId> = doc
                        .selection
                        .iter()
                        .filter_map(AsmSelected::component)
                        .collect();
                    if !targets.contains(&id) {
                        targets = vec![id];
                    }
                    let count = targets.len();
                    let counted = |label: &str| {
                        if count > 1 {
                            format!("{label} ({count} components)")
                        } else {
                            label.to_owned()
                        }
                    };
                    let response = row.response.on_hover_text(hover);
                    response.context_menu(|ui| {
                        let open = if sub.is_some() {
                            "Edit sub-assembly in place"
                        } else {
                            "Edit part in place"
                        };
                        if ui
                            .button(open)
                            .on_hover_text(
                                "Opens the file for editing, with the rest of the assembly \
                                 shown around it; File › Finish editing comes back here",
                            )
                            .clicked()
                        {
                            action = Some(TreeAction::OpenPart(Occurrence::top(id)));
                            ui.close_menu();
                        }
                        if ui
                            .button("Move component")
                            .on_hover_text("Drag it in the graphics area; Shift+drag turns it")
                            .clicked()
                        {
                            action = Some(TreeAction::Move(id));
                            ui.close_menu();
                        }
                        if ui
                            .button(counted(if fixed { "Float" } else { "Fix" }))
                            .clicked()
                        {
                            action = Some(TreeAction::Fix(targets.clone(), !fixed));
                            ui.close_menu();
                        }
                        if ui.button(counted("Hide / Show")).clicked() {
                            action = Some(TreeAction::Hide(targets.clone()));
                            ui.close_menu();
                        }
                        if ui.button("Appearance…").clicked() {
                            action = Some(TreeAction::Appearance(id));
                            ui.close_menu();
                        }
                        ui.menu_button("Material", |ui| {
                            if count > 1 {
                                ui.label(
                                    RichText::new(format!("{count} components")).small().weak(),
                                );
                            }
                            if let Some(choice) = material_menu(ui, &material_names) {
                                action = Some(TreeAction::Material(targets.clone(), choice));
                            }
                        });
                        if let Some(name) = doc.component_material(id) {
                            if ui
                                .button("Select parts with this material")
                                .on_hover_text(name)
                                .clicked()
                            {
                                action = Some(TreeAction::SelectMaterial(name.to_owned()));
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        if ui.button(counted("Delete")).clicked() {
                            action = Some(TreeAction::DeleteComponents(targets.clone()));
                            ui.close_menu();
                        }
                    });
                    // What the part is made of, as saved in its file: always shown under
                    // it, as SolidWorks shows Material in a part's tree, and a click away
                    // from changing.
                    if sub.is_none() && error.is_none() {
                        let material = doc.component_material_details(id);
                        let row = material_row(ui, material, 16.0);
                        if let Some(choice) = material_popup(ui, &row.response, &material_names) {
                            action = Some(TreeAction::Material(vec![id], choice));
                        }
                        if row.response.double_clicked() {
                            action = Some(TreeAction::EditMaterial(id));
                        }
                        row.response.context_menu(|ui| {
                            ui.menu_button("Change material", |ui| {
                                if let Some(choice) = material_menu(ui, &material_names) {
                                    action = Some(TreeAction::Material(vec![id], choice));
                                }
                            });
                            if let Some(m) = material {
                                if ui.button("Edit in the Materials window…").clicked() {
                                    action = Some(TreeAction::EditMaterial(id));
                                    ui.close_menu();
                                }
                                if ui.button("Select parts with this material").clicked() {
                                    action = Some(TreeAction::SelectMaterial(m.name.clone()));
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                    if expanded {
                        // The part's own variables and equations, as in its tree.
                        // Named as the assembly's equations name them, to click into one.
                        if let Some(crate::assembly::Loaded::Part(p)) = doc.loaded(id) {
                            let contents =
                                part_contents(&p.part, &|name| qualified(doc, &[id], name));
                            if !contents.is_empty() {
                                let open = self.asm_part_equations_expanded.contains(&id);
                                match equations_folder(ui, &contents, 16.0, open, inserting, false)
                                {
                                    Some(FolderEvent::Toggle) => {
                                        action = Some(TreeAction::ToggleEquations(Some(id)));
                                    }
                                    Some(FolderEvent::Insert(name)) => {
                                        action = Some(TreeAction::InsertName(name));
                                    }
                                    Some(FolderEvent::Manage) | None => {}
                                }
                            }
                        }
                        for (target, label, icon) in standard_targets() {
                            let row = tree_row(
                                ui,
                                Row {
                                    icon,
                                    cut: false,
                                    text: RichText::new(label),
                                    highlighted: false,
                                    hovered: false,
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
                        // What the sub-assembly holds, as it was saved: its parts take a
                        // material or an appearance from here, the rest by opening it.
                        if let Some(sub) = sub {
                            let mut inner = SubTree {
                                doc,
                                materials: &material_names,
                                top: id,
                                action: &mut action,
                                hovered: &mut hovered,
                            };
                            inner.show(ui, sub, &c.name, &mut Vec::new(), 16.0, 0);
                        }
                        // The mates this component takes part in.
                        for m in doc
                            .assembly
                            .mates
                            .iter()
                            .filter(|m| m.a.component == id || m.b.component == id)
                        {
                            let (clicked, hovered_mate) = mate_row(ui, doc, m, 16.0, mating);
                            if let Some(a) = clicked {
                                action = Some(a);
                            }
                            if hovered_mate {
                                hovered_refs.extend([m.a.clone(), m.b.clone()]);
                            }
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
                        hovered: false,
                        badge: None,
                        indent: 0.0,
                        expander: None,
                        eye: None,
                    },
                );
                for m in &doc.assembly.mates {
                    let (clicked, hovered_mate) = mate_row(ui, doc, m, 16.0, mating);
                    if let Some(a) = clicked {
                        action = Some(a);
                    }
                    if hovered_mate {
                        hovered_refs.extend([m.a.clone(), m.b.clone()]);
                    }
                }
            });
        let mut preselect = doc.picks_of(hovered.iter().filter_map(|h| match h {
            Hovered::Item(item) => Some(item),
            Hovered::Picks(_) => None,
        }));
        for h in &hovered {
            if let Hovered::Picks(picks) = h {
                preselect.extend(picks.iter().copied());
            }
        }
        for r in &hovered_refs {
            preselect.extend(doc.picks_of_ref(r));
        }
        self.viewport.preselect = preselect;
        // Scrolled while drawing; once is enough.
        self.asm_scroll_to = None;
        let (additive, range) =
            ui.input(|i| (i.modifiers.ctrl || i.modifiers.command, i.modifiers.shift));
        if let Some(action) = action {
            self.tree_action(action, additive, range);
        }
    }

    fn tree_action(&mut self, action: TreeAction, additive: bool, range: bool) {
        match action {
            TreeAction::Select(item) => {
                if let Some(doc) = self.asm.as_mut() {
                    if range {
                        select_range(doc, item, additive);
                    } else {
                        select(&mut doc.selection, item, additive);
                    }
                    if doc.selection.len() > 1 {
                        self.status = format!("{} items selected", doc.selection.len());
                    }
                }
                self.asm_plane = None;
            }
            TreeAction::SelectPlane(plane) => {
                self.asm_plane = (self.asm_plane != Some(plane)).then_some(plane);
                if let Some(doc) = self.asm.as_mut() {
                    doc.selection.clear();
                }
            }
            TreeAction::ToggleItem(item) => self.toggle_item(item),
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
            TreeAction::ToggleEquations(None) => {
                self.asm_equations_expanded = !self.asm_equations_expanded;
            }
            TreeAction::ToggleEquations(Some(id)) => {
                if !self.asm_part_equations_expanded.remove(&id) {
                    self.asm_part_equations_expanded.insert(id);
                }
            }
            TreeAction::ManageEquations => {
                self.equations
                    .show(crate::equations_ui::EquationsTab::Equations);
            }
            TreeAction::InsertName(name) => self.insert_into_equation_field(&name),
            TreeAction::Hide(ids) => {
                if let Some(doc) = self.asm.as_mut() {
                    doc.checkpoint();
                    for id in ids {
                        if let Some(c) = doc.assembly.component_mut(id) {
                            c.hidden = !c.hidden;
                        }
                    }
                    doc.revision = crate::document::next_revision();
                }
            }
            TreeAction::Fix(ids, fixed) => self.set_fixed(&ids, fixed),
            TreeAction::OpenPart(o) => self.pending_open_component = Some(o),
            TreeAction::Appearance(id) => self.begin_component_appearance(id),
            TreeAction::EditMaterial(id) => {
                let material = self
                    .asm
                    .as_ref()
                    .and_then(|doc| doc.component_material_details(id).cloned());
                match material {
                    Some(m) => self.edit_material(m),
                    None => self.open_materials_window(),
                }
            }
            TreeAction::Material(targets, name) => self.apply_component_material(&targets, name),
            TreeAction::MaterialPaths(paths, name) => self.apply_material_to_paths(&paths, name),
            TreeAction::InnerAppearance(o) => self.begin_inner_appearance(o),
            TreeAction::Move(id) => {
                if let Some(doc) = self.asm.as_mut() {
                    doc.selection = vec![AsmSelected::Component(id)];
                }
                if !self.move_mode {
                    self.toggle_move_mode();
                }
            }
            TreeAction::SelectMaterial(name) => self.select_parts_with_material(&name),
            TreeAction::DeleteComponents(ids) => {
                if let Some(doc) = self.asm.as_mut() {
                    doc.checkpoint();
                    let mut removed = Vec::new();
                    for id in &ids {
                        removed.extend(doc.assembly.remove_component(*id));
                    }
                    doc.selection.clear();
                    doc.solve();
                    let what = if ids.len() == 1 {
                        "Component deleted".to_owned()
                    } else {
                        format!("{} components deleted", ids.len())
                    };
                    self.status = if removed.is_empty() {
                        what
                    } else {
                        format!("{what}, with {}", removed.join(", "))
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
        self.simulation_scene();
        self.generative_scene();
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
        self.viewport.grab_without_alt = self.move_mode && self.mate_dialog.is_none();
        if self.move_mode && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.toggle_move_mode();
        }
        // A press on the move handle (as drawn last frame) is the handle's, not the view's.
        let hover = ui.ctx().pointer_hover_pos();
        self.viewport.pointer_taken = self.move_handle.is_some()
            || hover.is_some_and(|p| handle_arrow_at(&self.handle_shapes, p).is_some());
        let output = self.viewport.ui(ui, picks, hint, false);
        self.viewport_rect = output.rect;
        let overlays = self.draw_assembly_references(ui, &output);
        self.move_handle_ui(ui, &output);
        self.render_over_viewport(ui, &output);
        self.simulation_over_viewport(ui, &output);

        if let Some(grab) = output.grab {
            self.grab_component(&grab);
        } else {
            self.grabbing = None;
        }
        if let Some(pick) = output.clicked {
            let position = output.response.interact_pointer_pos();
            let overlay = position.and_then(|p| overlay_at(&overlays, p, pick.is_some()));
            let item = pick.and_then(|p| self.asm.as_ref().and_then(|d| d.picked(p)));
            if let Some(mut d) = self.mate_dialog.take() {
                let target = match overlay {
                    Some(Overlay::Plane(plane)) => Some(MateRef::new(
                        ComponentId::ASSEMBLY,
                        MateTarget::Plane(plane),
                    )),
                    _ => item.as_ref().and_then(AsmSelected::mate_ref),
                };
                if let Some(r) = target {
                    self.mate_pick(&mut d, r);
                }
                self.mate_dialog = Some(d);
            } else if let Some(Overlay::Plane(plane)) = overlay {
                self.asm_plane = Some(plane);
                if let Some(doc) = self.asm.as_mut() {
                    doc.selection.clear();
                }
            } else if let Some(doc) = self.asm.as_mut() {
                self.asm_plane = None;
                match item {
                    Some(item) => {
                        // The tree shows the component it belongs to.
                        self.asm_scroll_to = item.component();
                        select(&mut doc.selection, item, output.additive);
                    }
                    None if !output.additive => doc.selection.clear(),
                    None => {}
                }
            }
        }
        // A selection box takes whole components, unless the Mate command is collecting.
        if let (Some(b), None) = (output.box_select, &self.mate_dialog) {
            let picks = self.viewport.picks_in(&b);
            if let Some(doc) = self.asm.as_mut() {
                let mut components: Vec<ComponentId> = Vec::new();
                for pick in picks {
                    if let Some(c) = doc.picked(pick).and_then(|s| s.component()) {
                        if !components.contains(&c) {
                            components.push(c);
                        }
                    }
                }
                if !output.additive {
                    doc.selection.clear();
                }
                for c in components {
                    let item = AsmSelected::Component(c);
                    if !doc.selection.contains(&item) {
                        doc.selection.push(item);
                    }
                }
                self.asm_plane = None;
                self.status = format!("{} component(s) selected", doc.selection.len());
            }
        }
        // Right-click on the part: what can be done with its component. The part under
        // the pointer may lie deep in a sub-assembly: that part is the one edited.
        let hovered = self
            .asm
            .as_ref()
            .and_then(|d| d.selection.iter().find_map(AsmSelected::component));
        let hovered_part = self.asm.as_ref().and_then(|d| {
            d.selection.iter().find_map(|s| match s {
                AsmSelected::Face(o, _) | AsmSelected::Edge(o, _) => Some(o.clone()),
                AsmSelected::Component(c) => Some(Occurrence::top(*c)),
                AsmSelected::Mate(_) => None,
            })
        });
        output.response.context_menu(|ui| match hovered {
            Some(id) => {
                if let Some(o) = hovered_part.clone() {
                    let label = if o.path.is_empty() {
                        "Edit part in place".to_owned()
                    } else {
                        format!(
                            "Edit {} in place",
                            self.asm
                                .as_ref()
                                .map_or_else(String::new, |d| d.occurrence_name(&o))
                        )
                    };
                    if ui.button(label).clicked() {
                        self.pending_open_component = Some(o);
                        ui.close_menu();
                    }
                }
                let fixed = self
                    .asm
                    .as_ref()
                    .and_then(|d| d.component(id))
                    .is_some_and(|c| c.fixed);
                let targets: Vec<ComponentId> = self
                    .asm
                    .as_ref()
                    .map(|d| {
                        d.selection
                            .iter()
                            .filter_map(AsmSelected::component)
                            .collect()
                    })
                    .unwrap_or_default();
                let label = if targets.len() > 1 {
                    format!(
                        "{} ({} components)",
                        if fixed { "Float" } else { "Fix" },
                        targets.len()
                    )
                } else {
                    (if fixed { "Float" } else { "Fix" }).to_owned()
                };
                if ui.button(label).clicked() {
                    self.set_fixed(&targets, !fixed);
                    ui.close_menu();
                }
                if ui.button("Mate…").clicked() {
                    self.begin_mate();
                    ui.close_menu();
                }
                if ui
                    .button("Move component")
                    .on_hover_text("Drag it to move it, Shift+drag to turn it; Esc ends")
                    .clicked()
                {
                    if !self.move_mode {
                        self.toggle_move_mode();
                    }
                    ui.close_menu();
                }
                if ui.button("Appearance…").clicked() {
                    self.begin_component_appearance(id);
                    ui.close_menu();
                }
                let targets: Vec<ComponentId> = self
                    .asm
                    .as_ref()
                    .map(|d| {
                        d.selection
                            .iter()
                            .filter_map(AsmSelected::component)
                            .collect()
                    })
                    .unwrap_or_default();
                let names: Vec<String> = self
                    .materials
                    .materials()
                    .iter()
                    .map(|m| m.name.clone())
                    .collect();
                let mut choice = None;
                ui.menu_button("Material", |ui| {
                    if targets.len() > 1 {
                        ui.label(
                            RichText::new(format!("{} components", targets.len()))
                                .small()
                                .weak(),
                        );
                    }
                    choice = material_menu(ui, &names);
                });
                if let Some(choice) = choice {
                    self.apply_component_material(&targets, choice);
                }
                let material = self
                    .asm
                    .as_ref()
                    .and_then(|d| d.component_material(id).map(str::to_owned));
                if let Some(name) = material {
                    if ui
                        .button("Select parts with this material")
                        .on_hover_text(&name)
                        .clicked()
                    {
                        self.select_parts_with_material(&name);
                        ui.close_menu();
                    }
                }
            }
            None => {
                ui.label(RichText::new("Select a component first").weak());
            }
        });
        let ctx = ui.ctx().clone();
        if let Some(o) = self.pending_open_component.take() {
            self.guard(PendingAction::OpenComponent(o), &ctx);
        }
        self.heads_up_toolbar(&ctx, output.rect);
    }

    /// Draws the assembly's own origin and standard planes over the image (the planes when
    /// shown, selected, or while the Mate command can take them), sized to the assembly.
    /// Returns the planes for clicking.
    fn draw_assembly_references(
        &self,
        ui: &egui::Ui,
        output: &crate::viewport::ViewportOutput,
    ) -> Vec<(Overlay, OverlayShape)> {
        let mut overlays = Vec::new();
        let Some(doc) = self.asm.as_ref() else {
            return overlays;
        };
        let (center, size) = match doc.bounds(&doc.assembly) {
            Some((lo, hi)) => (
                Point3::new(
                    (lo.x + hi.x) / 2.0,
                    (lo.y + hi.y) / 2.0,
                    (lo.z + hi.z) / 2.0,
                ),
                ((hi - lo).length() * 0.6).max(10.0),
            ),
            None => (Point3::ORIGIN, 50.0),
        };
        let painter = ui.painter_at(output.rect);
        let to_screen = |p: Point3| self.viewport.to_screen(p, output);
        let pointer = output.response.hover_pos();
        let mating = self.mate_dialog.is_some();
        for plane in [
            StandardPlane::Front,
            StandardPlane::Top,
            StandardPlane::Right,
        ] {
            let selected = self.asm_plane == Some(plane);
            if !(mating || selected || self.item_visible(Item::Plane(plane))) {
                continue;
            }
            let geometry = plane.plane();
            let quad = crate::app::plane_quad(&to_screen, &geometry, center, size);
            let hovered = mating
                && pointer.is_some_and(|p| {
                    crate::app::near_outline(&quad, p) || crate::app::inside(&quad, p)
                });
            let quad = crate::app::paint_plane(
                &painter,
                &to_screen,
                &geometry,
                center,
                size,
                &format!("{} Plane", plane_name(plane)),
                selected || hovered,
            );
            overlays.push((Overlay::Plane(plane), OverlayShape::Quad(quad)));
        }
        if self.item_visible(Item::Origin) {
            crate::app::paint_origin(&painter, &to_screen);
        }
        overlays
    }

    /// The XYZ handle of Move Component, as Creo's drag handle or Cura's: three arrows
    /// along the assembly's axes at the selected component (the one being dragged, else
    /// the first selected free one). Dragging an arrow slides the component along that
    /// axis alone, the mates pulling it back as with a plain drag.
    fn move_handle_ui(&mut self, ui: &egui::Ui, output: &crate::viewport::ViewportOutput) {
        self.handle_shapes.clear();
        if !self.move_mode || self.mate_dialog.is_some() {
            self.move_handle = None;
            return;
        }
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let id = match self.move_handle {
            Some(drag) => Some(drag.component),
            None => self.grabbing.or_else(|| {
                doc.selection
                    .iter()
                    .filter_map(|s| s.component())
                    .find(|c| *c != ComponentId::ASSEMBLY)
            }),
        };
        let Some(id) = id else {
            return;
        };
        let (Some(component), Some((lo, hi))) = (doc.component(id), doc.component_bounds(id))
        else {
            self.move_handle = None;
            return;
        };
        if component.fixed {
            return;
        }
        let center = component.placement.apply_point(Point3::new(
            (lo.x + hi.x) / 2.0,
            (lo.y + hi.y) / 2.0,
            (lo.z + hi.z) / 2.0,
        ));
        let to_screen = |p: Point3| self.viewport.to_screen(p, output);
        let origin = to_screen(center);
        // Arrows of a fixed length on screen; an axis seen end-on is left out.
        let mut arrows = Vec::new();
        for (axis, direction) in HANDLE_AXES.into_iter().enumerate() {
            let d = to_screen(center + direction) - origin;
            if d.length() < 1e-4 {
                continue;
            }
            arrows.push(HandleArrow {
                axis,
                from: origin,
                to: origin + d.normalized() * HANDLE_LENGTH,
            });
        }
        let response = &output.response;
        let pointer = response
            .hover_pos()
            .or_else(|| response.interact_pointer_pos());
        let hot = match self.move_handle {
            Some(drag) => Some(drag.axis),
            None => pointer
                .and_then(|p| handle_arrow_at(&arrows, p))
                .map(|a| a.axis),
        };

        // Dragging an arrow.
        if self.move_handle.is_none()
            && response.drag_started_by(egui::PointerButton::Primary)
            && let (Some(axis), Some(at)) = (hot, response.interact_pointer_pos())
        {
            if let Some(doc) = self.asm.as_mut() {
                doc.checkpoint();
            }
            self.move_handle = Some(HandleDrag {
                component: id,
                axis,
                last: at,
            });
        }
        if let Some(mut drag) = self.move_handle {
            match response.interact_pointer_pos() {
                Some(at) if response.dragged_by(egui::PointerButton::Primary) => {
                    let direction = HANDLE_AXES[drag.axis];
                    let mm = axis_shift(at - drag.last, to_screen(center + direction) - origin);
                    drag.last = at;
                    self.move_handle = Some(drag);
                    if let Some(doc) = self.asm.as_mut() {
                        if let Some(c) = doc.assembly.component_mut(id) {
                            c.placement = c.placement.moved(Vec3::ZERO, center, direction * mm);
                        }
                        doc.solve();
                    }
                }
                _ => self.move_handle = None,
            }
        }

        let painter = ui.painter_at(output.rect);
        paint_move_handle(&painter, &arrows, hot);
        self.handle_shapes = arrows;
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
        // While a job reads the parts, the picture stays as it was.
        if self.busy() {
            return;
        }
        let Some(doc) = self.asm.as_mut() else {
            return;
        };
        // Parts the assembly's equations changed (on undo, a mate edited…) are rebuilt
        // before they are drawn.
        if doc.has_pending_parts() {
            doc.settle(&mut self.kernel);
        }
        if self.asm_scene == Some(doc.revision) {
            return;
        }
        self.asm_scene = Some(doc.revision);
        let first = self.viewport.is_empty();
        let assembly = match &self.mate_preview {
            Some(p) => p.assembly.clone(),
            None => doc.assembly.clone(),
        };
        let mut mesh = doc.scene_mesh(&assembly);
        let (mut materials, textures) = self.assembly_materials(&assembly);
        self.tint_generative_roles(&mut materials);
        self.add_context(&mut mesh, &mut materials);
        let own = self.own_mesh_counts(&mesh);
        let before = (mesh.faces.len() as u32, mesh.edges.len() as u32);
        // The red volumes of Interference Detection come last and cannot be picked.
        self.viewport.pickable = if self.add_interferences(&mut mesh, &mut materials) {
            Some(own.unwrap_or(before))
        } else {
            own
        };
        let mut scene = Scene::from_mesh(&mesh);
        scene.set_materials(materials, textures);
        self.viewport.set_scene(scene, first);
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
                let max_height = (ctx.screen_rect().height() * 0.55).max(120.0);
                egui::ScrollArea::both()
                    .max_height(max_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
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
/// Something the pointer is over in the tree: an item of the assembly, or the faces of
/// a component inside a sub-assembly.
enum Hovered {
    Item(AsmSelected),
    Picks(Vec<Pick>),
}

/// One mate's row, with its menu. Returns what was clicked and whether the pointer is
/// over it.
fn mate_row(
    ui: &mut egui::Ui,
    doc: &AssemblyDoc,
    m: &oxyde_assembly::Mate,
    indent: f32,
    mating: bool,
) -> (Option<TreeAction>, bool) {
    let mut action = None;
    let status = doc.solution.as_ref().and_then(|s| s.status(m.id));
    let error = match status {
        Some(MateStatus::Error(e)) => Some(e.clone()),
        _ => None,
    };
    let side = |id: ComponentId| {
        if id == ComponentId::ASSEMBLY {
            "assembly".to_owned()
        } else {
            component_label(doc, id)
        }
    };
    let text = RichText::new(format!(
        "{} ({}, {})",
        m.name,
        side(m.a.component),
        side(m.b.component),
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
            hovered: false,
            badge: error.as_ref().map(|_| Icon::Error),
            indent,
            expander: None,
            eye: None,
        },
    );
    let id = m.id;
    let hovered = row.response.hovered();
    let suppressed = m.suppressed;
    // A concentric or perpendicular mate holds either way round: nothing to flip.
    let flippable = match (doc.local(&m.a), doc.local(&m.b)) {
        (Ok(a), Ok(b)) => m.kind.has_alignment(a.kind(), b.kind()),
        _ => true,
    };
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
        if flippable && ui.button("Flip alignment").clicked() {
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
    (action, hovered)
}

/// The rows of what a sub-assembly holds, under its component in the tree.
struct SubTree<'a> {
    doc: &'a AssemblyDoc,
    materials: &'a [String],
    /// The top-level component the sub-assembly is.
    top: ComponentId,
    action: &'a mut Option<TreeAction>,
    hovered: &'a mut Vec<Hovered>,
}

impl SubTree<'_> {
    /// `path`: the components leading to `sub` inside the top-level one.
    fn show(
        &mut self,
        ui: &mut egui::Ui,
        sub: &crate::assembly::LoadedAssembly,
        sub_name: &str,
        path: &mut Vec<ComponentId>,
        indent: f32,
        depth: usize,
    ) {
        if depth > 8 {
            return;
        }
        let doc = self.doc;
        for c in &sub.assembly.components {
            let file = sub.sources.get(&c.id);
            let inner = file
                .and_then(|p| doc.docs.get(p))
                .and_then(|d| d.as_ref().ok());
            let nested = match inner {
                Some(crate::assembly::Loaded::Assembly(a)) => Some(a),
                _ => None,
            };
            path.push(c.id);
            let occurrence = Occurrence {
                top: self.top,
                path: path.clone(),
            };
            let row = tree_row(
                ui,
                Row {
                    icon: if nested.is_some() {
                        Icon::Assembly
                    } else {
                        Icon::Part
                    },
                    cut: false,
                    text: RichText::new(&c.name),
                    highlighted: false,
                    hovered: false,
                    badge: inner.is_none().then_some(Icon::Error),
                    indent,
                    expander: None,
                    eye: None,
                },
            );
            if row.response.hovered() {
                self.hovered
                    .push(Hovered::Picks(doc.picks_under(self.top, path)));
            }
            if row.response.double_clicked() {
                *self.action = Some(TreeAction::OpenPart(occurrence.clone()));
            }
            let (material, is_part) = match inner {
                Some(crate::assembly::Loaded::Part(p)) => (p.part.material(), true),
                _ => (None, false),
            };
            let material_details = material;
            let material = material.map(|m| m.name.as_str());
            let hover = match material {
                Some(m) => format!("{}\nIn {sub_name}, made of {m}", c.path),
                None => format!("{}\nIn {sub_name}", c.path),
            };
            let response = row.response.on_hover_text(hover);
            let paths: Vec<std::path::PathBuf> =
                file.map(|p| doc.part_paths_at(p)).unwrap_or_default();
            response.context_menu(|ui| {
                ui.label(RichText::new(format!("{} in {sub_name}", c.name)).small().weak());
                if ui
                    .button(if nested.is_some() {
                        "Edit sub-assembly in place"
                    } else {
                        "Edit part in place"
                    })
                    .on_hover_text(
                        "Opens the file for editing, with the rest of the whole assembly \
                         shown around it; File › Finish editing comes back here",
                    )
                    .clicked()
                {
                    *self.action = Some(TreeAction::OpenPart(occurrence.clone()));
                    ui.close_menu();
                }
                if ui
                    .button("Appearance…")
                    .on_hover_text("Colour, transparency or texture of this component, written into the sub-assembly's file")
                    .clicked()
                {
                    *self.action = Some(TreeAction::InnerAppearance(occurrence.clone()));
                    ui.close_menu();
                }
                ui.add_enabled_ui(!paths.is_empty(), |ui| {
                    ui.menu_button("Material", |ui| {
                        if let Some(choice) = material_menu(ui, self.materials) {
                            *self.action = Some(TreeAction::MaterialPaths(paths.clone(), choice));
                        }
                    });
                });
                if let Some(name) = material {
                    if ui
                        .button("Select parts with this material")
                        .on_hover_text(name)
                        .clicked()
                    {
                        *self.action = Some(TreeAction::SelectMaterial(name.to_owned()));
                        ui.close_menu();
                    }
                }
                ui.separator();
                ui.label(
                    RichText::new(
                        "Mates and placement inside the sub-assembly: edit the sub-assembly \
                         in place.",
                    )
                    .small()
                    .weak(),
                );
            });
            if is_part {
                // What the part is made of, as saved in its file.
                let row = material_row(ui, material_details, indent + 16.0);
                if !paths.is_empty()
                    && let Some(choice) = material_popup(ui, &row.response, self.materials)
                {
                    *self.action = Some(TreeAction::MaterialPaths(paths.clone(), choice));
                }
                row.response.context_menu(|ui| {
                    ui.add_enabled_ui(!paths.is_empty(), |ui| {
                        ui.menu_button("Change material", |ui| {
                            if let Some(choice) = material_menu(ui, self.materials) {
                                *self.action =
                                    Some(TreeAction::MaterialPaths(paths.clone(), choice));
                            }
                        });
                    });
                    if let Some(name) = material {
                        if ui.button("Select parts with this material").clicked() {
                            *self.action = Some(TreeAction::SelectMaterial(name.to_owned()));
                            ui.close_menu();
                        }
                    }
                });
            }
            if let Some(nested) = nested {
                self.show(ui, nested, &c.name, path, indent + 16.0, depth + 1);
            }
            path.pop();
        }
        // The sub-assembly's own mates, as it was saved.
        for m in &sub.assembly.mates {
            let side = |id: ComponentId| {
                if id == ComponentId::ASSEMBLY {
                    sub_name.to_owned()
                } else {
                    sub.assembly
                        .component(id)
                        .map_or_else(|| "(missing)".to_owned(), |c| c.name.clone())
                }
            };
            let text = RichText::new(format!(
                "{} ({}, {})",
                m.name,
                side(m.a.component),
                side(m.b.component)
            ))
            .weak();
            tree_row(
                ui,
                Row {
                    icon: Icon::Mate,
                    cut: false,
                    text,
                    highlighted: false,
                    hovered: false,
                    badge: None,
                    indent,
                    expander: None,
                    eye: None,
                },
            )
            .response
            .on_hover_text(format!(
                "Mate inside {sub_name}: Edit sub-assembly in place to change it"
            ));
        }
    }
}

/// Selects `item`, or toggles it with Ctrl.
/// Shift+click: selects everything in the tree between the last selected item of the same
/// kind (a component or a mate) and `item`, keeping the previous selection with Ctrl.
fn select_range(doc: &mut AssemblyDoc, item: AsmSelected, additive: bool) {
    let range: Option<Vec<AsmSelected>> = match &item {
        AsmSelected::Component(id) => {
            let order: Vec<ComponentId> = doc.assembly.components.iter().map(|c| c.id).collect();
            let anchor = doc.selection.iter().rev().find_map(|s| match s {
                AsmSelected::Component(c) => Some(*c),
                _ => None,
            });
            between(&order, anchor, *id)
                .map(|ids| ids.into_iter().map(AsmSelected::Component).collect())
        }
        AsmSelected::Mate(id) => {
            let order: Vec<MateId> = doc.assembly.mates.iter().map(|m| m.id).collect();
            let anchor = doc.selection.iter().rev().find_map(|s| match s {
                AsmSelected::Mate(m) => Some(*m),
                _ => None,
            });
            between(&order, anchor, *id).map(|ids| ids.into_iter().map(AsmSelected::Mate).collect())
        }
        _ => None,
    };
    match range {
        Some(items) => {
            if !additive {
                doc.selection.clear();
            }
            for item in items {
                if !doc.selection.contains(&item) {
                    doc.selection.push(item);
                }
            }
        }
        None => select(&mut doc.selection, item, additive),
    }
}

/// The items of `order` from `anchor` to `end`, both included, whichever comes first; `None`
/// without an anchor in the list.
fn between<T: Copy + PartialEq>(order: &[T], anchor: Option<T>, end: T) -> Option<Vec<T>> {
    let a = order.iter().position(|x| Some(*x) == anchor)?;
    let b = order.iter().position(|x| *x == end)?;
    Some(order[a.min(b)..=a.max(b)].to_vec())
}

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
/// The axes of the move handle, in assembly coordinates.
const HANDLE_AXES: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];
/// Length of the handle's arrows on screen, in points.
const HANDLE_LENGTH: f32 = 80.0;
/// How near the pointer must be to an arrow to take it, in points.
const HANDLE_REACH: f32 = 9.0;

/// An arrow of the move handle as drawn, for hit testing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HandleArrow {
    /// 0, 1, 2 for X, Y, Z.
    pub axis: usize,
    pub from: egui::Pos2,
    pub to: egui::Pos2,
}

/// A drag of one arrow of the move handle.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HandleDrag {
    pub component: ComponentId,
    pub axis: usize,
    /// Where the pointer was when the component was last moved.
    pub last: egui::Pos2,
}

/// The arrow under a screen position, the nearest when several are.
pub(crate) fn handle_arrow_at(arrows: &[HandleArrow], at: egui::Pos2) -> Option<HandleArrow> {
    arrows
        .iter()
        .map(|a| (a, crate::sketch_mode::segment_distance(at, a.from, a.to)))
        .filter(|(_, d)| *d <= HANDLE_REACH)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(a, _)| *a)
}

/// How far a pointer movement slides along an axis, in mm: the movement projected on the
/// axis as it appears on screen, where `per_mm` is one millimetre of the axis on screen.
pub(crate) fn axis_shift(delta: egui::Vec2, per_mm: egui::Vec2) -> f64 {
    let length2 = per_mm.length_sq();
    if length2 < 1e-12 {
        return 0.0;
    }
    (delta.dot(per_mm) / length2) as f64
}

fn handle_color(axis: usize, hot: bool) -> Color32 {
    if hot {
        return Color32::from_rgb(255, 190, 40);
    }
    match axis {
        0 => Color32::from_rgb(210, 50, 50),
        1 => Color32::from_rgb(40, 150, 60),
        _ => Color32::from_rgb(50, 90, 220),
    }
}

/// Draws the move handle: arrows with heads and letters, the held or hovered one in
/// yellow, and a knob at the centre.
fn paint_move_handle(painter: &egui::Painter, arrows: &[HandleArrow], hot: Option<usize>) {
    let halo = Color32::from_rgba_unmultiplied(255, 255, 255, 150);
    for arrow in arrows {
        let d = (arrow.to - arrow.from).normalized();
        let side = egui::vec2(-d.y, d.x);
        let is_hot = hot == Some(arrow.axis);
        let color = handle_color(arrow.axis, is_hot);
        let width = if is_hot { 4.5_f32 } else { 3.0_f32 };
        let shaft = arrow.to - d * 10.0;
        painter.line_segment([arrow.from, shaft], Stroke::new(width + 2.5, halo));
        painter.line_segment([arrow.from, shaft], Stroke::new(width, color));
        painter.add(egui::Shape::convex_polygon(
            vec![arrow.to, shaft + side * 5.5, shaft - side * 5.5],
            color,
            Stroke::new(1.0_f32, halo),
        ));
        painter.text(
            arrow.to + d * 11.0,
            egui::Align2::CENTER_CENTER,
            ["X", "Y", "Z"][arrow.axis.min(2)],
            egui::FontId::proportional(13.0),
            color,
        );
    }
    if let Some(first) = arrows.first() {
        painter.circle(
            first.from,
            4.5,
            Color32::WHITE,
            Stroke::new(1.5_f32, Color32::from_gray(70)),
        );
    }
}

pub(crate) fn is_assembly(path: &Path) -> bool {
    has_extension(path, oxyde_format::ASSEMBLY_EXTENSION)
        || oxyde_format::document_kind(path)
            .is_ok_and(|(kind, _)| kind == oxyde_format::DocumentKind::Assembly)
}

#[cfg(test)]
mod handle_tests {
    use super::*;

    #[test]
    fn the_nearest_arrow_within_reach_is_taken() {
        let o = egui::pos2(100.0, 100.0);
        let arrows = [
            HandleArrow {
                axis: 0,
                from: o,
                to: egui::pos2(180.0, 100.0),
            },
            HandleArrow {
                axis: 1,
                from: o,
                to: egui::pos2(100.0, 20.0),
            },
        ];
        assert_eq!(
            handle_arrow_at(&arrows, egui::pos2(150.0, 104.0)).map(|a| a.axis),
            Some(0)
        );
        assert_eq!(
            handle_arrow_at(&arrows, egui::pos2(97.0, 50.0)).map(|a| a.axis),
            Some(1)
        );
        assert_eq!(handle_arrow_at(&arrows, egui::pos2(150.0, 120.0)), None);
        // At the shared origin the first arrow wins the tie.
        assert_eq!(handle_arrow_at(&arrows, o).map(|a| a.axis), Some(0));
    }

    #[test]
    fn a_pointer_movement_slides_by_its_projection_on_the_axis() {
        // One millimetre is 4 points to the right and 3 down.
        let per_mm = egui::vec2(4.0, 3.0);
        assert!((axis_shift(egui::vec2(8.0, 6.0), per_mm) - 2.0).abs() < 1e-9);
        assert!(
            (axis_shift(egui::vec2(-3.0, 4.0), per_mm)).abs() < 1e-9,
            "across: nothing"
        );
        assert!((axis_shift(egui::vec2(-4.0, -3.0), per_mm) + 1.0).abs() < 1e-9);
        assert_eq!(axis_shift(egui::vec2(5.0, 5.0), egui::Vec2::ZERO), 0.0);
    }
}
