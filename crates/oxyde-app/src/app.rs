//! Application state and the top-level layout: menus, command bar, panels, status bar.
//!
//! Commands work the SolidWorks way: select first (a sketch in the timeline, edges in the
//! viewport…), then click the command; it opens in the PropertyManager with that selection,
//! and further clicks add to it.

use std::path::{Path, PathBuf};

use eframe::egui;
use oxyde_geom::{Plane, Point3, Vec3};
use oxyde_kernel::{Kernel, SurfaceKind, Topology};
use oxyde_model::{
    EdgeName, ExtrudeOp, FaceName, FeatureId, FeatureKind, FeatureStatus, FileData, Import,
    ReferenceGeometry, Sketch, SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;
use oxyde_render::{Pick, Scene};

use crate::dialogs::{BlendDialog, Dialog, DimensionTarget, ExtrudeDialog, plane_name};
use crate::document::Document;
use crate::equations_ui::EquationsWindow;
use crate::features_ui::PatternKind;
use crate::property_manager::PanelPick;
use crate::sketch_mode::{self, SketchRequest, SketchSession, Tool};
use crate::viewport::{Viewport, ViewportOutput};

/// A face or edge selected in the viewport, held by persistent name so that the selection
/// survives rebuilds.
#[derive(Debug, Clone, PartialEq)]
pub enum Selected {
    Face(FaceName),
    Edge(EdgeName),
}

/// An action that would discard unsaved changes, waiting for confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingAction {
    New,
    NewAssembly,
    Open,
    /// Leave the assembly to edit a component's part.
    OpenComponent(oxyde_assembly::ComponentId),
    /// Back to the assembly a part was opened from.
    BackToAssembly,
    /// A new part from a STEP file.
    OpenStep,
    /// Part and assembly files from a STEP assembly.
    ImportStepAssembly,
    Demo,
    DemoFlange,
    Exit,
}

/// A feature command, from the command bar or the Insert menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Sketch,
    /// Adds or removes material; chosen in the PropertyManager.
    Extrude,
    Revolve,
    /// Fillet (true) or chamfer (false).
    Blend(bool),
    Shell,
    Pattern(PatternKind),
    Combine,
    RefPlane,
    RefAxis,
}

/// The tabs of the command bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTab {
    Features,
    Sketch,
    Evaluate,
}

/// A plane or axis drawn over the viewport, which can be clicked.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Overlay {
    Plane(StandardPlane),
    Reference(FeatureId),
}

/// Where an overlay is on screen.
enum OverlayShape {
    Quad([egui::Pos2; 4]),
    Segment(egui::Pos2, egui::Pos2),
    /// Curves of a sketch.
    Polylines(Vec<Vec<egui::Pos2>>),
}

pub struct OxydeApp {
    pub(crate) kernel: OcctKernel,
    pub(crate) doc: Document,
    pub(crate) viewport: Viewport,
    pub(crate) selected_feature: Option<FeatureId>,
    pub(crate) selected_plane: Option<StandardPlane>,
    /// A body selected in the Solid Bodies folder.
    pub(crate) selected_body: Option<oxyde_model::BodyId>,
    /// The Solid Bodies folder is open in the timeline.
    pub(crate) bodies_expanded: bool,
    pub(crate) selection: Vec<Selected>,
    pub(crate) dialog: Option<Dialog>,
    /// The sketch being edited, when in sketch mode.
    pub(crate) sketch: Option<SketchSession>,
    /// Tools › Equations window (equations, global variables and dimensions).
    pub(crate) equations: EquationsWindow,
    pub(crate) status: String,
    pub(crate) tab: CommandTab,
    /// A sketch tool clicked outside sketch mode: picked once the new sketch opens.
    pub(crate) pending_tool: Option<Tool>,
    pub(crate) show_mass_properties: bool,
    pub(crate) show_measure: bool,
    /// Which planes, axes, sketches and origins are drawn.
    pub(crate) visibility: crate::display::Visibility,
    /// Features expanded in the timeline (showing the sketch they absorb).
    pub(crate) expanded: std::collections::HashSet<FeatureId>,
    /// Document revision the viewport currently shows.
    pub(crate) scene_revision: Option<u64>,
    /// Status-bar summary, cached per document revision.
    summary: Option<(u64, String)>,
    window_title: String,
    allow_close: bool,
    /// The open assembly; the app is in assembly mode while there is one.
    pub(crate) asm: Option<crate::assembly::AssemblyDoc>,
    pub(crate) mate_dialog: Option<crate::assembly_ui::MateDialog>,
    pub(crate) mate_preview: Option<crate::assembly_ui::MatePreview>,
    pub(crate) show_bom: bool,
    pub(crate) bom_mode: oxyde_assembly::bom::BomMode,
    /// Assembly revision the viewport shows.
    pub(crate) asm_scene: Option<u64>,
    /// The component being moved with Alt+drag.
    pub(crate) grabbing: Option<oxyde_assembly::ComponentId>,
    /// A component whose part should open once unsaved changes are dealt with.
    pub(crate) pending_open_component: Option<oxyde_assembly::ComponentId>,
    /// The assembly a part was opened from, to go back to.
    pub(crate) return_to: Option<PathBuf>,
    pub(crate) prefs: crate::prefs::Preferences,
    pub(crate) prefs_window: crate::prefs_ui::PrefsWindow,
    pub(crate) debug_stats: crate::prefs_ui::DebugStats,
    /// Where the viewport was last drawn, for the debug overlay.
    pub(crate) viewport_rect: egui::Rect,
    pub(crate) splash: crate::splash::Splash,
}

impl OxydeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, file: Option<PathBuf>) -> Self {
        crate::theme::load_fonts(&cc.egui_ctx);
        let mut app = Self {
            kernel: OcctKernel::new(),
            doc: Document::new(),
            viewport: Viewport::new(),
            selected_feature: None,
            selected_plane: None,
            selected_body: None,
            bodies_expanded: true,
            selection: Vec::new(),
            dialog: None,
            sketch: None,
            equations: EquationsWindow::new(),
            status: "Ready".to_owned(),
            tab: CommandTab::Features,
            pending_tool: None,
            show_mass_properties: false,
            show_measure: false,
            visibility: Default::default(),
            expanded: Default::default(),
            scene_revision: None,
            summary: None,
            window_title: String::new(),
            allow_close: false,
            asm: None,
            mate_dialog: None,
            mate_preview: None,
            show_bom: false,
            bom_mode: oxyde_assembly::bom::BomMode::TopLevel,
            asm_scene: None,
            grabbing: None,
            pending_open_component: None,
            return_to: None,
            prefs: crate::prefs::Preferences::load(),
            prefs_window: Default::default(),
            debug_stats: Default::default(),
            viewport_rect: egui::Rect::NOTHING,
            // The file to open, if any, is opened by the splash screen's last step.
            splash: crate::splash::Splash::new(file.clone()),
        };
        app.apply_preferences(&cc.egui_ctx);
        if let Some(state) = &cc.wgpu_render_state {
            let info = state.adapter.get_info();
            app.viewport.enable_gpu(
                crate::gpu::GpuViewport::new(
                    state,
                    if app.prefs.graphics.antialiasing {
                        crate::gpu::DEFAULT_SAMPLES
                    } else {
                        1
                    },
                ),
                format!("GPU: {} ({:?})", info.name, info.backend),
            );
        }
        let _ = file;
        app
    }

    // ---- Model changes -----------------------------------------------------------------------

    pub(crate) fn rebuild(&mut self) {
        let start = std::time::Instant::now();
        self.doc.rebuild(&mut self.kernel);
        self.debug_stats.rebuild = Some(start.elapsed().as_secs_f64());
    }

    /// Adds a feature at the rollback bar as one undo step, rebuilds and selects it.
    pub(crate) fn add_feature(&mut self, kind: FeatureKind) -> FeatureId {
        self.doc.checkpoint();
        let id = self.doc.part.add_feature(kind);
        self.rebuild();
        self.selected_feature = Some(id);
        self.selection.clear();
        let name = self
            .doc
            .part
            .feature(id)
            .map(|f| f.name().to_owned())
            .unwrap_or_default();
        self.status = match self.doc.part.status(id) {
            Some(FeatureStatus::Error(message)) => format!("{name} failed: {message}"),
            Some(FeatureStatus::Warning(message)) => format!("{name}: {message}"),
            _ => format!("Added {name}"),
        };
        id
    }

    pub(crate) fn delete_feature(&mut self, id: FeatureId) {
        let dependents = self.doc.part.dependents(id);
        if !dependents.is_empty() {
            let names: Vec<String> = dependents
                .iter()
                .filter_map(|d| self.doc.part.feature(*d).map(|f| f.name().to_owned()))
                .collect();
            self.status = format!("Cannot delete: used by {}", names.join(", "));
            return;
        }
        self.doc.checkpoint();
        if self.doc.part.delete(id).is_ok() {
            if self.selected_feature == Some(id) {
                self.selected_feature = None;
            }
            self.rebuild();
        }
    }

    pub(crate) fn undo(&mut self) {
        if self.doc.undo(&mut self.kernel) {
            self.status = "Undone".to_owned();
        }
    }

    pub(crate) fn redo(&mut self) {
        if self.doc.redo(&mut self.kernel) {
            self.status = "Redone".to_owned();
        }
    }

    // ---- Selection ---------------------------------------------------------------------------

    pub(crate) fn selected_edges(&self) -> Vec<EdgeName> {
        self.selection
            .iter()
            .filter_map(|s| match s {
                Selected::Edge(e) => Some(e.clone()),
                Selected::Face(_) => None,
            })
            .collect()
    }

    /// The first selected face, if it is planar (usable as a sketch plane).
    fn selected_planar_face(&self) -> Option<FaceName> {
        let body = self.doc.part.body()?;
        self.selection.iter().find_map(|s| match s {
            Selected::Face(name) => {
                let index = body.naming.find_face(name)?;
                let face = body.topology.faces.get(index as usize)?;
                (face.surface == SurfaceKind::Plane).then(|| name.clone())
            }
            Selected::Edge(_) => None,
        })
    }

    fn selection_picks(&self) -> Vec<Pick> {
        let Some(body) = self.doc.part.body() else {
            return Vec::new();
        };
        let mut picks: Vec<Pick> = self
            .selection
            .iter()
            .filter_map(|s| match s {
                Selected::Face(name) => body.naming.find_face(name).map(Pick::Face),
                Selected::Edge(name) => body.naming.find_edge(name).map(Pick::Edge),
            })
            .collect();
        if let Some(solid) = self.selected_body.and_then(|id| body.solid(id)) {
            picks.extend(solid.faces.clone().map(Pick::Face));
        }
        picks
    }

    fn click(&mut self, pick: Option<Pick>, additive: bool) {
        let item = self.doc.part.body().and_then(|body| match pick {
            Some(Pick::Face(i)) => body.naming.face(i).cloned().map(Selected::Face),
            Some(Pick::Edge(i)) => body.naming.edge(i).cloned().map(Selected::Edge),
            None => None,
        });
        match item {
            None => {
                if !additive {
                    self.selection.clear();
                    self.selected_plane = None;
                }
            }
            Some(item) => {
                self.status = match &item {
                    Selected::Face(name) => format!("Face {name}"),
                    Selected::Edge(name) => format!("Edge {name}"),
                };
                if additive {
                    if let Some(position) = self.selection.iter().position(|s| *s == item) {
                        self.selection.remove(position);
                    } else {
                        self.selection.push(item);
                    }
                } else {
                    self.selection = vec![item];
                    self.selected_plane = None;
                }
            }
        }
    }

    /// A face or edge clicked in the viewport, as offered to an open command.
    fn panel_pick(&self, pick: Option<Pick>) -> Option<PanelPick> {
        let body = self.doc.part.body()?;
        match pick? {
            Pick::Face(i) => body.naming.face(i).cloned().map(PanelPick::Face),
            Pick::Edge(i) => body.naming.edge(i).cloned().map(PanelPick::Edge),
        }
    }

    // ---- Commands ----------------------------------------------------------------------------

    pub(crate) fn sketches(&self) -> Vec<(FeatureId, String)> {
        let active = self
            .doc
            .part
            .rollback_position()
            .unwrap_or(self.doc.part.features().len());
        self.doc.part.features()[..active.min(self.doc.part.features().len())]
            .iter()
            .filter(|f| matches!(f.kind(), FeatureKind::Sketch(_)))
            .map(|f| (f.id(), f.name().to_owned()))
            .collect()
    }

    /// The sketch an extrude or revolve starts with: the one selected in the timeline, else
    /// the latest sketch nothing uses yet. `None` leaves the PropertyManager waiting for one.
    pub(crate) fn sketch_to_use(&self) -> Option<FeatureId> {
        let sketches = self.sketches();
        if let Some(id) = self.selected_feature {
            if sketches.iter().any(|(s, _)| *s == id) {
                return Some(id);
            }
        }
        let used = |id: FeatureId| {
            self.doc.part.features().iter().any(|f| match f.kind() {
                FeatureKind::Extrude(e) => e.sketch == id,
                FeatureKind::Revolve(r) => r.sketch == id,
                _ => false,
            })
        };
        sketches
            .iter()
            .rev()
            .map(|(id, _)| *id)
            .find(|id| !used(*id))
    }

    /// "Sketch" command: on the selected flat face, else the selected plane (standard or
    /// reference), else the PropertyManager asks for one.
    pub(crate) fn begin_sketch(&mut self, tool: Option<Tool>) {
        self.pending_tool = tool;
        let reference_plane = self.selected_feature.filter(|id| {
            matches!(
                self.doc.part.reference(*id),
                Some(ReferenceGeometry::Plane(_))
            )
        });
        if let Some(face) = self.selected_planar_face() {
            self.start_new_sketch(SketchPlane::Face(face));
        } else if let Some(plane) = self.selected_plane {
            self.start_new_sketch(SketchPlane::Standard(plane));
        } else if let Some(id) = reference_plane {
            self.start_new_sketch(SketchPlane::Reference(id));
        } else {
            self.dialog = Some(Dialog::ChoosePlane);
            self.status = "Select a plane or a flat face for the sketch".to_owned();
        }
    }

    pub(crate) fn start_new_sketch(&mut self, plane: SketchPlane) {
        let id = self.add_feature(FeatureKind::Sketch(Sketch::new(plane)));
        self.edit_sketch(id, true);
    }

    /// Enters sketch mode on a sketch feature. Later features are rolled back while editing.
    pub(crate) fn edit_sketch(&mut self, id: FeatureId, new_sketch: bool) {
        let part = &self.doc.part;
        let Some(index) = part.index_of(id) else {
            return;
        };
        let Some(FeatureKind::Sketch(sketch)) = part.feature(id).map(|f| f.kind().clone()) else {
            return;
        };
        let name = part
            .feature(id)
            .map(|f| f.name().to_owned())
            .unwrap_or_default();
        let restore = part
            .rollback_position()
            .and_then(|active| part.features().get(active).map(|f| f.id()));
        let after = part.features().get(index + 1).map(|f| f.id());

        let _ = self.doc.part.roll_back_to(after);
        self.rebuild();
        let Some(plane) = self.doc.part.sketch_result(id).map(|r| r.plane) else {
            let _ = self.doc.part.roll_back_to(restore);
            self.rebuild();
            self.status = format!("Cannot edit {name}: its plane is missing");
            return;
        };
        // Turn to the sketch and centre it, as SolidWorks does; an empty sketch frames the part.
        let points: Vec<oxyde_geom::Point3> = sketch
            .geometry
            .entities()
            .iter()
            .filter_map(|e| match e.geometry {
                oxyde_sketch::Geometry::Point(p) if !e.construction => Some(plane.to_world(p)),
                _ => None,
            })
            .collect();
        let bounds = (points.len() >= 2).then(|| {
            let mut lo = points[0];
            let mut hi = points[0];
            for p in &points {
                lo = oxyde_geom::Point3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
                hi = oxyde_geom::Point3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
            }
            (lo, hi)
        });
        self.viewport.look_at_plane_framing(&plane, bounds);
        self.selection.clear();
        self.selected_plane = None;
        self.selected_feature = Some(id);
        self.dialog = None;
        let mut session = SketchSession::new(id, name, plane, sketch.geometry, new_sketch, restore);
        session.driven = self
            .doc
            .part
            .equations()
            .equations
            .iter()
            .filter_map(|e| match e.target {
                Some(oxyde_model::DimensionRef {
                    feature,
                    key: oxyde_model::DimensionKey::Sketch(relation),
                }) if feature == id => Some(relation),
                _ => None,
            })
            .collect();
        if let Some(body) = self.doc.part.body() {
            session.model = crate::model_refs::ModelGeometry::from_body(&body.topology, &plane);
        }
        if let Some(tool) = self.pending_tool.take() {
            session.set_tool(tool);
        }
        self.sketch = Some(session);
        self.tab = CommandTab::Sketch;
    }

    /// Leaves sketch mode, keeping or discarding the changes. The sketch stays selected, so
    /// that Extrude or Revolve picks it up.
    pub(crate) fn finish_sketch(&mut self, keep: bool) {
        let Some(session) = self.sketch.take() else {
            return;
        };
        let _ = self.doc.part.roll_back_to(session.restore_rollback);
        let id = session.feature;
        if keep && session.changed() {
            self.doc.checkpoint();
            if let Ok(FeatureKind::Sketch(sketch)) = self.doc.part.edit(id) {
                sketch.geometry = session.sketch.clone();
            }
        }
        if !keep && session.new_sketch && session.original_empty {
            // Cancelling a brand-new sketch removes it, as SolidWorks does.
            let _ = self.doc.part.delete(id);
            self.selected_feature = None;
        }
        self.rebuild();
        self.viewport.request_fit();
        self.tab = CommandTab::Features;
        self.status = if keep {
            format!(
                "Closed {}. With it selected, click Extrude or Revolve to make a solid",
                session.name
            )
        } else {
            format!("Discarded changes to {}", session.name)
        };
    }

    /// Replaces a feature's definition as one undo step and rebuilds.
    pub(crate) fn replace_feature(&mut self, id: FeatureId, kind: FeatureKind) {
        let name = self
            .doc
            .part
            .feature(id)
            .map(|f| f.name().to_owned())
            .unwrap_or_default();
        let before = self.doc.part.definition();
        if let Err(e) = self.doc.part.replace_feature(id, kind) {
            self.status = format!("{name} was not changed: {e}");
            return;
        }
        self.doc.checkpoint_from(before);
        self.rebuild();
        self.status = match self.doc.part.status(id) {
            Some(FeatureStatus::Error(message)) => format!("{name} failed: {message}"),
            _ => format!("Updated {name}"),
        };
    }

    fn begin_extrude(&mut self) {
        let sketch = self.sketch_to_use();
        self.dialog = Some(Dialog::Extrude(ExtrudeDialog {
            editing: None,
            dimension: None,
            sketch,
            op: ExtrudeOp::Boss,
            depth: 10.0,
            mid_plane: false,
            reverse: false,
            bodies: Vec::new(),
        }));
        if sketch.is_none() {
            self.status = "Select the sketch to extrude in the tree".to_owned();
        }
    }

    fn begin_blend(&mut self, fillet: bool) {
        let edges = self.selected_edges();
        if edges.is_empty() {
            self.status = "Click the edges to round in the graphics area".to_owned();
        }
        self.dialog = Some(Dialog::Blend(BlendDialog {
            editing: None,
            dimension: None,
            fillet,
            size: if fillet { 2.0 } else { 1.0 },
            edges,
        }));
    }

    pub(crate) fn run(&mut self, command: Command) {
        if self.sketch.is_some() && command != Command::Sketch {
            self.finish_sketch(true);
        }
        self.selected_plane = self.selected_plane.filter(|_| {
            matches!(
                command,
                Command::Sketch | Command::RefPlane | Command::Pattern(PatternKind::Mirror)
            )
        });
        match command {
            Command::Sketch => self.begin_sketch(None),
            Command::Extrude => self.begin_extrude(),
            Command::Revolve => self.begin_revolve(),
            Command::Blend(fillet) => self.begin_blend(fillet),
            Command::Shell => self.begin_shell(),
            Command::Pattern(kind) => self.begin_pattern(kind),
            Command::Combine => self.open_combine(None, None),
            Command::RefPlane => self.open_ref_plane(None, None),
            Command::RefAxis => self.open_ref_axis(None, None),
        }
    }

    // ---- Files -------------------------------------------------------------------------------

    pub(crate) fn guard(&mut self, action: PendingAction, ctx: &egui::Context) {
        if self.is_modified() {
            self.dialog = Some(Dialog::Discard(action));
        } else {
            self.perform(action, ctx);
        }
    }

    pub(crate) fn perform(&mut self, action: PendingAction, ctx: &egui::Context) {
        use oxyde_format::{ASSEMBLY_EXTENSION, LEGACY_PART_EXTENSION, PART_EXTENSION};
        match action {
            PendingAction::New => {
                self.leave_assembly();
                self.replace_document(Document::new());
                self.status = "New part".to_owned();
            }
            PendingAction::NewAssembly => {
                self.enter_assembly(crate::assembly::AssemblyDoc::new());
                self.status = "New assembly: Insert Component adds a part file".to_owned();
            }
            PendingAction::Open => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        "Oxyde document or STEP",
                        &[
                            PART_EXTENSION,
                            ASSEMBLY_EXTENSION,
                            LEGACY_PART_EXTENSION,
                            "step",
                            "stp",
                        ],
                    )
                    .add_filter("Oxyde part", &[PART_EXTENSION, LEGACY_PART_EXTENSION])
                    .add_filter("Oxyde assembly", &[ASSEMBLY_EXTENSION])
                    .add_filter("STEP", &["step", "stp"])
                    .pick_file()
                {
                    self.open_path(&path);
                }
            }
            PendingAction::OpenComponent(id) => self.open_component(id),
            PendingAction::BackToAssembly => {
                if let Some(path) = self.return_to.clone() {
                    self.open_assembly(&path);
                }
            }
            PendingAction::OpenStep => {
                if let Some(path) = pick_step() {
                    self.leave_assembly();
                    self.open_step_as_part(&path);
                }
            }
            PendingAction::ImportStepAssembly => {
                if let Some(path) = pick_step() {
                    self.import_step_assembly(&path);
                }
            }
            PendingAction::DemoFlange => {
                self.leave_assembly();
                let mut doc = Document::new();
                doc.part = crate::demo::flange();
                self.replace_document(doc);
                self.status = "Demo flange loaded".to_owned();
                self.report_errors();
            }
            PendingAction::Demo => {
                self.leave_assembly();
                let mut doc = Document::new();
                doc.part = crate::demo::bracket();
                self.replace_document(doc);
                self.status = "Demo bracket loaded".to_owned();
                self.report_errors();
            }
            PendingAction::Exit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn replace_document(&mut self, doc: Document) {
        self.doc.release(&mut self.kernel);
        self.doc = doc;
        self.sketch = None;
        self.dialog = None;
        self.selection.clear();
        self.selected_feature = None;
        self.selected_plane = None;
        self.scene_revision = None;
        self.tab = CommandTab::Features;
        self.viewport.request_fit();
        self.rebuild();
    }

    pub(crate) fn open_path(&mut self, path: &Path) {
        if crate::assembly_ui::is_assembly(path) {
            self.open_assembly(path);
            return;
        }
        self.leave_assembly();
        if is_step(path) {
            self.open_step_as_part(path);
            return;
        }
        let loaded = oxyde_format::load_part(path)
            .map_err(|e| e.to_string())
            .and_then(|file| Document::from_file(file, Some(path.to_path_buf())));
        match loaded {
            Ok(doc) => {
                self.replace_document(doc);
                self.remember_recent(path);
                self.status = format!("Opened {}", path.display());
                self.report_errors();
            }
            Err(e) => self.status = format!("Could not open {}: {e}", path.display()),
        }
    }

    /// A new part whose first feature imports `path`.
    fn open_step_as_part(&mut self, path: &Path) {
        let mut doc = Document::new();
        doc.untitled_name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
        self.replace_document(doc);
        if self.import_step_file(path) {
            // Opening is not an edit: nothing to undo, nothing unsaved yet.
            self.doc.modified = false;
            self.doc.clear_history();
        }
    }

    /// File › Import STEP assembly: one part file per distinct part of `path` (an
    /// Imported feature holding its solids) and one assembly file per assembly, written
    /// into a folder the user picks; then opens the top-level document.
    fn import_step_assembly(&mut self, path: &Path) {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.status = format!("Could not read {}: {e}", path.display());
                return;
            }
        };
        let source = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "import.step".to_owned());
        let plan = match self.kernel.read_step_assembly(&bytes) {
            Ok(tree) => oxyde_format::step_import::plan_step_import(&tree, &source)
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(e) => {
                self.status = format!("Could not import {source}: {e}");
                return;
            }
        };
        let mut dialog = rfd::FileDialog::new().set_title(format!(
            "Folder for the {} part(s) and {} assembly file(s) of {source}",
            plan.part_count(),
            plan.assembly_count()
        ));
        if let Some(parent) = path.parent() {
            dialog = dialog.set_directory(parent);
        }
        let Some(dir) = dialog.pick_folder() else {
            return;
        };
        let existing = plan.existing_files(&dir);
        if !existing.is_empty() {
            let names: Vec<String> = existing
                .iter()
                .take(10)
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
            let more = existing.len().saturating_sub(names.len());
            let mut text = format!(
                "These files already exist and will be replaced:\n\n{}",
                names.join("\n")
            );
            if more > 0 {
                text += &format!("\n…and {more} more");
            }
            let answer = rfd::MessageDialog::new()
                .set_title("Replace files?")
                .set_description(text)
                .set_buttons(rfd::MessageButtons::YesNo)
                .set_level(rfd::MessageLevel::Warning)
                .show();
            if answer != rfd::MessageDialogResult::Yes {
                return;
            }
        }
        match plan.write(&dir) {
            Ok(top) => {
                self.open_path(&top);
                self.status = format!(
                    "Imported {source}: {} part(s), {} assembly file(s) in {}",
                    plan.part_count(),
                    plan.assembly_count(),
                    dir.display()
                );
            }
            Err(e) => self.status = format!("Could not write the imported files: {e}"),
        }
    }

    /// File › Import STEP: adds the file's solids to the part as an "Imported" feature.
    pub(crate) fn import_step(&mut self) {
        if let Some(path) = pick_step() {
            self.finish_sketch(true);
            self.import_step_file(&path);
        }
    }

    /// Reads `path` into an "Imported" feature at the rollback bar. Returns whether the
    /// geometry could be read.
    fn import_step_file(&mut self, path: &Path) -> bool {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.status = format!("Could not read {}: {e}", path.display());
                return false;
            }
        };
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "import.step".to_owned());
        let id = self.add_feature(FeatureKind::Import(Import {
            file_name: file_name.clone(),
            data: FileData::new(bytes),
        }));
        self.viewport.request_fit();
        match self.doc.part.status(id) {
            Some(FeatureStatus::Error(message)) => {
                self.status = format!("Import failed: {message}");
                false
            }
            _ => {
                self.status = format!("Imported {file_name}");
                true
            }
        }
    }

    fn report_errors(&mut self) {
        let failed = self
            .doc
            .part
            .timeline()
            .iter()
            .filter(|e| matches!(e.status, FeatureStatus::Error(_)))
            .count();
        if failed > 0 {
            self.status = format!("Rebuilt with {failed} feature error(s)");
        }
    }

    /// Saves to the current file, or asks for one. Returns whether the part was saved.
    pub(crate) fn save(&mut self) -> bool {
        // A sketch being edited is closed (kept) first, so the file has it.
        self.finish_sketch(true);
        match self.doc.path.clone() {
            Some(path) => self.save_to(&path),
            None => self.save_as(),
        }
    }

    pub(crate) fn save_as(&mut self) -> bool {
        use oxyde_format::{LEGACY_PART_EXTENSION, PART_EXTENSION};
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("Oxyde part", &[PART_EXTENSION])
            .set_file_name(format!("{}.{PART_EXTENSION}", self.doc.display_name()))
            .save_file()
        else {
            return false;
        };
        let known = [PART_EXTENSION, LEGACY_PART_EXTENSION]
            .iter()
            .any(|e| crate::assembly_ui::has_extension(&path, e));
        if !known {
            path.set_extension(PART_EXTENSION);
        }
        self.save_to(&path)
    }

    fn save_to(&mut self, path: &Path) -> bool {
        let mut file = self.doc.to_file();
        file.properties.volume_mm3 = self
            .doc
            .part
            .body()
            .and_then(|body| bodies_volume(&self.kernel, body).ok());
        match oxyde_format::save_part(path, &file) {
            Ok(()) => {
                self.doc.path = Some(path.to_path_buf());
                self.remember_recent(path);
                self.doc.properties.volume_mm3 = file.properties.volume_mm3;
                self.doc.modified = false;
                self.status = format!("Saved {}", path.display());
                true
            }
            Err(e) => {
                self.status = format!("Could not save {}: {e}", path.display());
                false
            }
        }
    }

    pub(crate) fn export_step(&mut self) {
        let Some(shapes) = self.doc.part.body().map(|b| b.shapes()) else {
            self.status = "Nothing to export".to_owned();
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("STEP", &["step", "stp"])
            .set_file_name(format!("{}.step", self.doc.display_name()))
            .save_file()
        else {
            return;
        };
        self.status = match self.kernel.export_step(&shapes, &path) {
            Ok(()) => format!("Exported {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    // ---- Layout ------------------------------------------------------------------------------

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let revision = self.doc.revision;
        if self.summary.as_ref().map(|(r, _)| *r) != Some(revision) {
            let text = match self.doc.part.body() {
                Some(body) => {
                    let volume = bodies_volume(&self.kernel, body)
                        .map_or_else(|_| "?".to_owned(), |v| format!("{v:.1}"));
                    format!(
                        "Volume {volume} mm³   Faces {}   Edges {}",
                        body.topology.faces.len(),
                        body.topology.edges.len()
                    )
                }
                None => "No body".to_owned(),
            };
            self.summary = Some((revision, text));
        }
        let status = match &self.sketch {
            Some(session) => session.status(),
            None => self.status.clone(),
        };
        ui.horizontal(|ui| {
            ui.label(status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((_, text)) = &self.summary {
                    ui.label(text);
                }
            });
        });
    }

    fn viewport_ui(&mut self, ui: &mut egui::Ui) {
        self.refresh_scene();
        if self.sketch.is_some() {
            let output = self.viewport.ui(ui, Vec::new(), None, true);
            self.viewport_rect = output.rect;
            let request = match self.sketch.as_mut() {
                Some(session) => sketch_mode::interact(session, &self.viewport, ui, &output),
                None => None,
            };
            match request {
                Some(SketchRequest::EditDimension(index)) => self.open_dimension_editor(index),
                Some(SketchRequest::NewDimension(option)) => {
                    self.dialog = Some(Dialog::Dimension {
                        target: DimensionTarget::New(option.relation),
                        label: option.label.trim_end_matches('…').to_owned(),
                        name: None,
                        text: crate::dialogs::format_value(option.shown),
                        angle: option.angle,
                        fresh: true,
                    });
                }
                None => {}
            }
            self.sketch_context_menu(&output);
            self.heads_up_toolbar(ui.ctx(), output.rect);
            return;
        }
        let picks = self.panel_picks().unwrap_or_else(|| self.selection_picks());
        let hint = (self.doc.part.body().is_none() && self.dialog.is_none() && !self.splash.open)
            .then_some(
                "Start with a sketch (Sketch tab), then Extrude it — or File › Open demo bracket",
            );
        let output = self.viewport.ui(ui, picks, hint, false);
        self.viewport_rect = output.rect;
        let overlays = self.draw_references(ui, &output);
        if let Some(pick) = output.clicked {
            let position = output.response.interact_pointer_pos();
            let overlay = position.and_then(|p| overlay_at(&overlays, p, pick.is_some()));
            match overlay {
                Some(Overlay::Plane(plane)) => {
                    if !self.offer_to_panel(PanelPick::Plane(plane)) {
                        self.selected_plane = Some(plane);
                        self.selected_feature = None;
                        self.selection.clear();
                    }
                }
                Some(Overlay::Reference(id)) => {
                    if !self.offer_to_panel(PanelPick::Feature(id)) {
                        self.selected_feature = Some(id);
                        self.selected_plane = None;
                    }
                }
                None => {
                    let taken = match self.panel_pick(pick) {
                        Some(panel_pick) => self.offer_to_panel(panel_pick),
                        None => false,
                    };
                    let panel_open = self.dialog.as_ref().is_some_and(|d| d.is_panel());
                    if !taken && !panel_open {
                        self.click(pick, output.additive);
                    }
                }
            }
        }
        output.response.context_menu(|ui| {
            if ui.button("Sketch").clicked() {
                ui.close_menu();
                self.run(Command::Sketch);
            }
            let has_edges = self
                .selection
                .iter()
                .any(|s| matches!(s, Selected::Edge(_)));
            if ui
                .add_enabled(has_edges, egui::Button::new("Fillet"))
                .clicked()
            {
                ui.close_menu();
                self.run(Command::Blend(true));
            }
            if ui
                .add_enabled(has_edges, egui::Button::new("Chamfer"))
                .clicked()
            {
                ui.close_menu();
                self.run(Command::Blend(false));
            }
            ui.separator();
            if ui.button("Zoom to fit").clicked() {
                ui.close_menu();
                self.viewport.request_fit();
            }
            if ui.button("Clear selection").clicked() {
                ui.close_menu();
                self.selection.clear();
                self.selected_plane = None;
            }
        });
        self.heads_up_toolbar(ui.ctx(), output.rect);
    }

    fn open_dimension_editor(&mut self, id: oxyde_sketch::RelationId) {
        let Some(session) = &self.sketch else {
            return;
        };
        let Some(relation) = session.sketch.relation(id) else {
            return;
        };
        let Some((value, angle)) = sketch_mode::dimension_value(&relation.constraint) else {
            return;
        };
        if session.driven.contains(&id) {
            self.status = format!(
                "{} is driven by an equation (Tools › Equations)",
                session.dimension_full_name(id).unwrap_or_default()
            );
            return;
        }
        self.dialog = Some(Dialog::Dimension {
            target: DimensionTarget::Existing(id),
            label: session
                .dimension_full_name(id)
                .unwrap_or_else(|| "Dimension".to_owned()),
            name: relation.name.clone(),
            text: crate::dialogs::format_value(value),
            angle,
            fresh: true,
        });
    }

    /// Renames a feature as one undo step; refused names leave everything unchanged.
    pub(crate) fn rename_feature(
        &mut self,
        id: FeatureId,
        name: String,
    ) -> Result<(), oxyde_model::ModelError> {
        if self.doc.part.feature(id).map(|f| f.name()) == Some(name.as_str()) {
            return Ok(());
        }
        let mut trial = oxyde_model::Part::from_definition(self.doc.part.definition())?;
        trial.rename(id, name.clone())?;
        self.doc.checkpoint();
        self.doc.part.rename(id, name)
    }

    /// Right-click in sketch mode: relations and dimensions for the selection.
    fn sketch_context_menu(&mut self, output: &ViewportOutput) {
        let mut exit = false;
        let mut dialog = None;
        let sketch = &mut self.sketch;
        output.response.context_menu(|ui| {
            let Some(session) = sketch.as_mut() else {
                return;
            };
            let relations = session.relation_options();
            let dimensions = session.dimension_options();
            if session.selection.is_empty() {
                ui.label(egui::RichText::new("Select geometry to add relations").weak());
            }
            for (label, relation) in relations {
                if ui.button(label).clicked() {
                    session.add_relation(relation);
                    ui.close_menu();
                }
            }
            if !dimensions.is_empty() {
                ui.separator();
            }
            for option in dimensions {
                if ui.button(option.label).clicked() {
                    dialog = Some(Dialog::Dimension {
                        target: DimensionTarget::New(option.relation),
                        label: option.label.trim_end_matches('…').to_owned(),
                        name: None,
                        text: crate::dialogs::format_value(option.shown),
                        angle: option.angle,
                        fresh: true,
                    });
                    ui.close_menu();
                }
            }
            if session.has_curve_selected() {
                ui.separator();
                if ui.button("Construction geometry on/off").clicked() {
                    session.toggle_construction();
                    ui.close_menu();
                }
            }
            if !session.selected_dimensions.is_empty() {
                let label = if session.selected_dimensions.len() == 1 {
                    "Delete dimension"
                } else {
                    "Delete dimensions"
                };
                if ui.button(label).clicked() {
                    session.delete_selection();
                    ui.close_menu();
                }
            }
            if !session.selection.is_empty() && ui.button("Delete").clicked() {
                session.delete_selection();
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Exit sketch").clicked() {
                exit = true;
                ui.close_menu();
            }
        });
        if dialog.is_some() {
            self.dialog = dialog;
        }
        if exit {
            self.finish_sketch(true);
        }
    }

    /// Centre and size of the part, for sizing planes and axes drawn over it.
    fn display_extent(&self) -> (Point3, f64) {
        let Some(body) = self.doc.part.body() else {
            return (Point3::ORIGIN, 50.0);
        };
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for p in body.topology.vertices.iter().map(|v| v.point) {
            for (k, value) in [p.x, p.y, p.z].into_iter().enumerate() {
                lo[k] = lo[k].min(value);
                hi[k] = hi[k].max(value);
            }
        }
        if !lo[0].is_finite() {
            return (Point3::ORIGIN, 50.0);
        }
        let c = Point3::new(
            (lo[0] + hi[0]) / 2.0,
            (lo[1] + hi[1]) / 2.0,
            (lo[2] + hi[2]) / 2.0,
        );
        let d = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f64>().sqrt();
        (c, (d * 0.6).max(10.0))
    }

    /// Whether the open command can use a plane clicked in the viewport.
    fn wants_planes(&self) -> bool {
        match &self.dialog {
            Some(Dialog::ChoosePlane | Dialog::RefPlane(_)) => true,
            Some(Dialog::Pattern(d)) => d.kind == PatternKind::Mirror,
            Some(Dialog::RefAxis(d)) => d.two_planes,
            _ => false,
        }
    }

    /// Draws the standard planes (when one is selected or the open command wants a plane),
    /// the reference planes (translucent squares) and axes (dash-dot lines) over the image,
    /// sized to the part. Returns them for clicking.
    fn draw_references(
        &self,
        ui: &egui::Ui,
        output: &ViewportOutput,
    ) -> Vec<(Overlay, OverlayShape)> {
        let mut overlays = Vec::new();
        let (center, size) = self.display_extent();
        let painter = ui.painter_at(output.rect);
        let to_screen = |p: Point3| self.viewport.to_screen(p, output);
        let pointer = output.response.hover_pos();
        let label_font = egui::FontId::proportional(11.0);

        let draw_plane = |plane: Plane, name: String, selected: bool| -> OverlayShape {
            let to_center = center - plane.origin;
            let c = center - plane.normal * to_center.dot(plane.normal);
            let (u, v): (Vec3, Vec3) = (plane.x_dir * size, plane.y_dir() * size);
            let corners: Vec<egui::Pos2> = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                .into_iter()
                .map(|(a, b)| to_screen(c + u * a + v * b))
                .collect();
            let quad = [corners[0], corners[1], corners[2], corners[3]];
            let hovered = pointer.is_some_and(|p| near_outline(&quad, p) || inside(&quad, p))
                && self.wants_planes();
            let color = if selected || hovered {
                egui::Color32::from_rgb(230, 120, 0)
            } else {
                if crate::theme::view_dark() {
                    egui::Color32::from_rgb(110, 170, 235)
                } else {
                    egui::Color32::from_rgb(60, 120, 200)
                }
            };
            painter.add(egui::Shape::convex_polygon(
                corners,
                color.gamma_multiply(0.12),
                egui::Stroke::new(if selected || hovered { 2.0 } else { 1.0 }, color),
            ));
            painter.text(
                quad[3],
                egui::Align2::LEFT_BOTTOM,
                name,
                label_font.clone(),
                color,
            );
            OverlayShape::Quad(quad)
        };

        let all_planes = self.wants_planes();
        for standard in [
            StandardPlane::Front,
            StandardPlane::Top,
            StandardPlane::Right,
        ] {
            let selected = self.selected_plane == Some(standard);
            let shown = self.item_visible(crate::display::Item::Plane(standard));
            if all_planes || selected || shown {
                let shape = draw_plane(
                    standard.plane(),
                    format!("{} Plane", plane_name(standard)),
                    selected,
                );
                overlays.push((Overlay::Plane(standard), shape));
            }
        }
        let highlighted = self.panel_features();
        for (id, geometry) in self.doc.part.references() {
            let selected = self.selected_feature == Some(id) || highlighted.contains(&id);
            let shown = self.item_visible(crate::display::Item::Feature(id));
            let wanted = matches!(geometry, ReferenceGeometry::Plane(_)) && all_planes;
            if !(selected || shown || wanted) {
                continue;
            }
            let name = self
                .doc
                .part
                .feature(id)
                .map(|f| f.name().to_owned())
                .unwrap_or_default();
            match geometry {
                ReferenceGeometry::Plane(p) => {
                    let shape = draw_plane(p, name, selected);
                    overlays.push((Overlay::Reference(id), shape));
                }
                ReferenceGeometry::Axis(axis) => {
                    let color = if selected {
                        egui::Color32::from_rgb(230, 120, 0)
                    } else {
                        if crate::theme::view_dark() {
                            egui::Color32::from_rgb(110, 170, 235)
                        } else {
                            egui::Color32::from_rgb(60, 120, 200)
                        }
                    };
                    let t = (center - axis.origin).dot(axis.direction);
                    let c = axis.origin + axis.direction * t;
                    let a = to_screen(c - axis.direction * size);
                    let b = to_screen(c + axis.direction * size);
                    painter.extend(egui::Shape::dashed_line(
                        &[a, b],
                        egui::Stroke::new(if selected { 2.0 } else { 1.2 }, color),
                        10.0,
                        4.0,
                    ));
                    painter.text(
                        b,
                        egui::Align2::LEFT_CENTER,
                        name,
                        label_font.clone(),
                        color,
                    );
                    overlays.push((Overlay::Reference(id), OverlayShape::Segment(a, b)));
                }
            }
        }

        // Sketches: shown ones, the selected one and those the open command uses.
        for feature in self.doc.part.features() {
            let id = feature.id();
            if !matches!(feature.kind(), FeatureKind::Sketch(_)) {
                continue;
            }
            let selected = self.selected_feature == Some(id) || highlighted.contains(&id);
            if !(selected || self.item_visible(crate::display::Item::Feature(id))) {
                continue;
            }
            let Some(result) = self.doc.part.sketch_result(id) else {
                continue;
            };
            let hovered_color = egui::Color32::from_rgb(230, 120, 0);
            let mut lines = Vec::new();
            for (points, construction) in crate::display::sketch_polylines(&result.solved) {
                let path: Vec<egui::Pos2> = points
                    .into_iter()
                    .map(|p| to_screen(result.plane.to_world(p)))
                    .collect();
                let color = if selected {
                    hovered_color
                } else {
                    if crate::theme::view_dark() {
                        egui::Color32::from_rgb(130, 175, 235)
                    } else {
                        egui::Color32::from_rgb(40, 70, 150)
                    }
                };
                let stroke = egui::Stroke::new(if selected { 2.0 } else { 1.3 }, color);
                if construction {
                    painter.extend(egui::Shape::dashed_line(&path, stroke, 6.0, 4.0));
                } else {
                    painter.add(egui::Shape::line(path.clone(), stroke));
                }
                lines.push(path);
            }
            overlays.push((Overlay::Reference(id), OverlayShape::Polylines(lines)));
        }

        // The origin: three short axes of fixed screen length.
        if self.item_visible(crate::display::Item::Origin) {
            let o = to_screen(Point3::ORIGIN);
            for (axis, color) in [
                (Vec3::X, egui::Color32::from_rgb(210, 50, 50)),
                (Vec3::Y, egui::Color32::from_rgb(40, 150, 60)),
                (Vec3::Z, egui::Color32::from_rgb(50, 90, 220)),
            ] {
                let d = to_screen(Point3::ORIGIN + axis) - o;
                if d.length() > 1e-6 {
                    let tip = o + d.normalized() * 22.0;
                    painter.line_segment([o, tip], egui::Stroke::new(1.6, color));
                }
            }
            painter.circle(
                o,
                3.0,
                egui::Color32::from_rgb(200, 60, 60),
                egui::Stroke::new(1.0, egui::Color32::WHITE),
            );
        }
        overlays
    }

    /// Re-tessellates the body after each rebuild.
    fn refresh_scene(&mut self) {
        if self.scene_revision == Some(self.doc.revision) {
            return;
        }
        self.scene_revision = Some(self.doc.revision);
        let first = self.viewport.is_empty();
        let Some(body) = self.doc.part.body() else {
            self.viewport.set_scene(Scene::default(), false);
            return;
        };
        match tessellate_bodies(&self.kernel, body, self.mesh_fraction()) {
            Ok(mesh) => self.viewport.set_scene(Scene::from_mesh(&mesh), first),
            Err(e) => self.status = format!("Display failed: {e}"),
        }
    }

    /// The plane "Normal To" turns the view to: the sketch's, else the selected plane or
    /// flat face.
    pub(crate) fn normal_to_target(&self) -> Option<Plane> {
        if let Some(session) = &self.sketch {
            return Some(session.plane);
        }
        if let Some(plane) = self.selected_plane {
            return Some(plane.plane());
        }
        if let Some(Some(ReferenceGeometry::Plane(p))) =
            self.selected_feature.map(|id| self.doc.part.reference(id))
        {
            return Some(p);
        }
        let face = self.selected_planar_face()?;
        let body = self.doc.part.body()?;
        let info = body
            .topology
            .faces
            .get(body.naming.find_face(&face)? as usize)?;
        let normal = info.normal?;
        let helper = if normal.x.abs() < 0.9 {
            Vec3::X
        } else {
            Vec3::Y
        };
        let x_dir = helper - normal * helper.dot(normal);
        let length = x_dir.dot(x_dir).sqrt();
        Some(Plane {
            origin: info.centroid,
            x_dir: x_dir * (1.0 / length),
            normal,
        })
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        self.dispatch_shortcuts(ctx);

        // Escape is not configurable: it always cancels.
        if self.dialog.is_none() && !ctx.wants_keyboard_input() {
            let escape = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            if !escape {
                return;
            }
            if let Some(session) = self.sketch.as_mut() {
                if !session.cancel_operation() {
                    self.finish_sketch(true);
                }
                return;
            }
            self.selection.clear();
            self.selected_plane = None;
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && self.is_modified() && !self.allow_close
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::Discard(PendingAction::Exit));
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let name = match &self.asm {
            Some(doc) => doc.display_name(),
            None => self.doc.display_name(),
        };
        let title = format!(
            "{name}{} - Oxyde",
            if self.is_modified() { " *" } else { "" }
        );
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }
}

fn is_step(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("step") || e.eq_ignore_ascii_case("stp"))
}

fn pick_step() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("STEP", &["step", "stp"])
        .pick_file()
}

/// Whether `p` is inside a convex quad.
fn inside(quad: &[egui::Pos2; 4], p: egui::Pos2) -> bool {
    let mut sign = 0.0f32;
    for k in 0..4 {
        let (a, b) = (quad[k], quad[(k + 1) % 4]);
        let cross = (b - a).x * (p - a).y - (b - a).y * (p - a).x;
        if cross.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

fn segment_distance(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 {
        ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - (a + ab * t)).length()
}

fn near_outline(quad: &[egui::Pos2; 4], p: egui::Pos2) -> bool {
    (0..4).any(|k| segment_distance(p, quad[k], quad[(k + 1) % 4]) < 6.0)
}

/// The overlay clicked at `p`: an outline or axis near the pointer first; inside a plane
/// only when the click did not land on the part.
fn overlay_at(
    overlays: &[(Overlay, OverlayShape)],
    p: egui::Pos2,
    on_part: bool,
) -> Option<Overlay> {
    let near = overlays.iter().find(|(_, shape)| match shape {
        OverlayShape::Quad(q) => near_outline(q, p),
        OverlayShape::Segment(a, b) => segment_distance(p, *a, *b) < 6.0,
        OverlayShape::Polylines(lines) => lines.iter().any(|line| {
            line.windows(2)
                .any(|w| segment_distance(p, w[0], w[1]) < 5.0)
        }),
    });
    if let Some((overlay, _)) = near {
        return Some(*overlay);
    }
    if on_part {
        return None;
    }
    overlays.iter().find_map(|(overlay, shape)| match shape {
        OverlayShape::Quad(q) if inside(q, p) => Some(*overlay),
        _ => None,
    })
}

/// Mesh precision: about a thousandth of the part size.
/// The meshes of every body of a part, one after the other, so that faces and edges are
/// numbered as in `body.topology`.
pub(crate) fn tessellate_bodies<K: Kernel>(
    kernel: &K,
    body: &oxyde_model::Body,
    fraction: f64,
) -> Result<oxyde_kernel::Mesh, K::Error> {
    let deflection = deflection(&body.topology, fraction);
    let mut out = oxyde_kernel::Mesh {
        faces: Vec::new(),
        edges: Vec::new(),
    };
    for solid in &body.solids {
        let mesh = kernel.tessellate(solid.shape, deflection)?;
        out.faces.extend(mesh.faces);
        out.edges.extend(mesh.edges);
    }
    Ok(out)
}

/// Volume of every body together, mm³.
pub(crate) fn bodies_volume<K: Kernel>(
    kernel: &K,
    body: &oxyde_model::Body,
) -> Result<f64, K::Error> {
    body.solids.iter().map(|s| kernel.volume(s.shape)).sum()
}

/// Mesh precision: `fraction` of the part size (about a thousandth by default).
pub(crate) fn deflection(topology: &Topology, fraction: f64) -> f64 {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let points = topology
        .vertices
        .iter()
        .map(|v| v.point)
        .chain(topology.faces.iter().map(|f| f.centroid));
    for p in points {
        for (k, value) in [p.x, p.y, p.z].into_iter().enumerate() {
            lo[k] = lo[k].min(value);
            hi[k] = hi[k].max(value);
        }
    }
    let diagonal = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f64>().sqrt();
    if diagonal.is_finite() && diagonal > 0.0 {
        (diagonal * fraction).clamp(0.0005, 5.0)
    } else {
        0.05
    }
}

impl eframe::App for OxydeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_close_request(ctx);
        self.splash_step(ctx);
        if self.asm.is_some() {
            self.assembly_shortcuts(ctx);
            egui::TopBottomPanel::top("menu").show(ctx, |ui| self.assembly_menu_bar(ui));
            egui::TopBottomPanel::top("commands").show(ctx, |ui| self.assembly_command_bar(ui));
            egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.assembly_status_bar(ui));
            egui::SidePanel::left("feature_manager")
                .resizable(true)
                .default_width(290.0)
                .min_width(220.0)
                .show(ctx, |ui| self.assembly_panel(ui));
            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show(ctx, |ui| self.assembly_viewport(ui));
            self.bom_window(ctx);
            self.preferences_window(ctx);
            self.debug_overlay(ctx, self.viewport_rect);
            self.dialogs_ui(ctx);
            self.splash_ui(ctx);
            self.update_title(ctx);
            return;
        }
        self.viewport.grab_enabled = false;
        self.handle_shortcuts(ctx);

        egui::TopBottomPanel::top("menu").show(ctx, |ui| self.menu_bar(ui));
        egui::TopBottomPanel::top("commands").show(ctx, |ui| self.command_bar(ui));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.status_bar(ui));
        egui::SidePanel::left("feature_manager")
            .resizable(true)
            .default_width(290.0)
            .min_width(220.0)
            .show(ctx, |ui| {
                self.property_manager(ui);
                self.timeline_ui(ui);
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| self.viewport_ui(ui));

        self.equations_window(ctx);
        self.evaluate_windows(ctx);
        self.preferences_window(ctx);
        self.debug_overlay(ctx, self.viewport_rect);
        self.dialogs_ui(ctx);
        self.splash_ui(ctx);
        self.update_title(ctx);
    }
}

impl Drop for OxydeApp {
    fn drop(&mut self) {
        self.leave_assembly();
        self.doc.release(&mut self.kernel);
    }
}
