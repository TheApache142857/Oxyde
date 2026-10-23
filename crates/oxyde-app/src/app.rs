//! Application state and the top-level layout: menus, command bar, panels, status bar.
//!
//! Commands work the SolidWorks way: select first (a sketch in the timeline, edges in the
//! viewport…), then click the command; it opens in the PropertyManager with that selection,
//! and further clicks add to it.

use std::path::{Path, PathBuf};

use eframe::egui;
use oxyde_geom::{Plane, Point3, Vec3};
use oxyde_kernel::{Kernel, SurfaceKind};
use oxyde_model::{
    EdgeName, FaceName, FeatureId, FeatureKind, FeatureStatus, ReferenceGeometry, Sketch,
    SketchPlane, StandardPlane,
};
use oxyde_occt::OcctKernel;
use oxyde_render::{Pick, Scene};

use crate::dialogs::{BlendDialog, Dialog, DimensionTarget, plane_name};
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingAction {
    New,
    NewAssembly,
    Open,
    /// File › Open recent: a document opened or saved lately, by its path.
    OpenRecent(std::path::PathBuf),
    /// Edit a part or sub-assembly of the assembly in place, at any depth (see
    /// `edit_in_place`).
    OpenComponent(crate::assembly::Occurrence),
    /// Back to the assembly a component was opened from.
    FinishEdit,
    /// A new part from a STEP file.
    OpenStep,
    /// Part and assembly files from a STEP assembly.
    ImportStepAssembly,
    Demo,
    DemoFlange,
    Exit,
}

impl PendingAction {
    /// Whether the action closes every open document, including the assemblies set
    /// aside while a component is edited in place: those are finished first.
    pub fn leaves_everything(&self) -> bool {
        !matches!(
            self,
            PendingAction::OpenComponent(_) | PendingAction::FinishEdit
        )
    }
}

/// A feature command, from the command bar or the Insert menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Sketch,
    /// Adds or removes material; chosen in the PropertyManager.
    Extrude,
    Revolve,
    Sweep,
    Loft,
    /// Fillet (true) or chamfer (false).
    Blend(bool),
    Shell,
    /// Hole Wizard: holes at the points of a sketch.
    Hole,
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
    /// The model drawn by the path tracer (`render_ui`).
    Render,
    /// The model's studies: static, thermal and topology (`simulation_ui`).
    Simulation,
    /// The model's generative studies: shapes grown between bodies to keep, as Fusion
    /// 360's Generative Design workspace has them (`generative_ui`).
    Generative,
}

/// A plane or axis drawn over the viewport, which can be clicked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Overlay {
    Plane(StandardPlane),
    Reference(FeatureId),
    /// A corner of the shown body, by index in its topology: a point for the Plane command.
    Vertex(u32),
    /// A point of a sketch, by entity id.
    SketchPoint(FeatureId, u32),
    /// A line, an arc or a circle of a sketch, by entity id.
    SketchCurve(FeatureId, u32),
}

/// Where an overlay is on screen.
pub(crate) enum OverlayShape {
    Quad([egui::Pos2; 4]),
    Segment(egui::Pos2, egui::Pos2),
    /// Curves of a sketch.
    Polylines(Vec<Vec<egui::Pos2>>),
    /// A point.
    Dot(egui::Pos2),
}

/// The colour of planes, axes and sketches drawn over the viewport.
pub(crate) fn overlay_color(strong: bool) -> egui::Color32 {
    if strong {
        egui::Color32::from_rgb(230, 120, 0)
    } else if crate::theme::view_dark() {
        egui::Color32::from_rgb(110, 170, 235)
    } else {
        egui::Color32::from_rgb(60, 120, 200)
    }
}

/// Paints a plane as a translucent square of half-size `size` around the projection of
/// `center` onto it, with its name; returns the square for clicking.
pub(crate) fn paint_plane(
    painter: &egui::Painter,
    to_screen: &dyn Fn(Point3) -> egui::Pos2,
    plane: &Plane,
    center: Point3,
    size: f64,
    name: &str,
    strong: bool,
) -> [egui::Pos2; 4] {
    let to_center = center - plane.origin;
    let c = center - plane.normal * to_center.dot(plane.normal);
    let (u, v): (Vec3, Vec3) = (plane.x_dir * size, plane.y_dir() * size);
    let corners: Vec<egui::Pos2> = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
        .into_iter()
        .map(|(a, b)| to_screen(c + u * a + v * b))
        .collect();
    let quad = [corners[0], corners[1], corners[2], corners[3]];
    let color = overlay_color(strong);
    painter.add(egui::Shape::convex_polygon(
        corners,
        color.gamma_multiply(0.12),
        egui::Stroke::new(if strong { 2.0_f32 } else { 1.0_f32 }, color),
    ));
    painter.text(
        quad[3],
        egui::Align2::LEFT_BOTTOM,
        name,
        egui::FontId::proportional(11.0),
        color,
    );
    quad
}

/// Paints an origin as three short axes of fixed screen length.
pub(crate) fn paint_origin(painter: &egui::Painter, to_screen: &dyn Fn(Point3) -> egui::Pos2) {
    let o = to_screen(Point3::ORIGIN);
    for (axis, color) in [
        (Vec3::X, egui::Color32::from_rgb(210, 50, 50)),
        (Vec3::Y, egui::Color32::from_rgb(40, 150, 60)),
        (Vec3::Z, egui::Color32::from_rgb(50, 90, 220)),
    ] {
        let d = to_screen(Point3::ORIGIN + axis) - o;
        if d.length() > 1e-6 {
            let tip = o + d.normalized() * 22.0;
            painter.line_segment([o, tip], egui::Stroke::new(1.6_f32, color));
        }
    }
    painter.circle(
        o,
        3.0,
        egui::Color32::from_rgb(200, 60, 60),
        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
    );
}

pub struct OxydeApp {
    pub(crate) kernel: OcctKernel,
    pub(crate) doc: Document,
    pub(crate) viewport: Viewport,
    pub(crate) selected_feature: Option<FeatureId>,
    /// Several features selected in the timeline (Ctrl+click, Shift+click), including
    /// `selected_feature`, which anchors it; empty for a plain selection.
    pub(crate) selected_features: Vec<FeatureId>,
    pub(crate) selected_plane: Option<StandardPlane>,
    /// A body selected in the Solid Bodies folder.
    pub(crate) selected_body: Option<oxyde_model::BodyId>,
    /// The Solid Bodies folder is open in the timeline.
    pub(crate) bodies_expanded: bool,
    /// Which tab of the left panel is shown: the feature tree or the configurations.
    pub(crate) left_tab: crate::configurations_ui::LeftTab,
    pub(crate) configurations_panel: crate::configurations_ui::ConfigurationsPanel,
    pub(crate) selection: Vec<Selected>,
    pub(crate) dialog: Option<Dialog>,
    /// The sketch being edited, when in sketch mode.
    pub(crate) sketch: Option<SketchSession>,
    /// Tools › Equations window (equations, global variables and dimensions).
    pub(crate) equations: EquationsWindow,
    /// The part's Equations folder is open in its tree.
    pub(crate) part_equations_expanded: bool,
    /// The assembly's Equations folder is open in its tree, and those of these components.
    pub(crate) asm_equations_expanded: bool,
    pub(crate) asm_part_equations_expanded: std::collections::HashSet<oxyde_assembly::ComponentId>,
    /// Nodes open in the tree of the assembly a part is edited in (`in_place_tree.rs`).
    pub(crate) place_expanded: std::collections::HashSet<crate::in_place_tree::Node>,
    pub(crate) status: String,
    pub(crate) tab: CommandTab,
    /// A sketch tool clicked outside sketch mode: picked once the new sketch opens.
    pub(crate) pending_tool: Option<Tool>,
    /// The sketch open is the hole positions of the Hole Wizard, which opens on leaving it.
    pub(crate) hole_positions: bool,
    pub(crate) show_mass_properties: bool,
    pub(crate) show_measure: bool,
    /// The distance Measure found between the two selected faces or edges, for the
    /// document revision and selection it was measured for.
    pub(crate) measure_cache: Option<crate::evaluate::MeasureCache>,
    pub(crate) asm_measure_cache: Option<crate::evaluate::AsmMeasureCache>,
    /// Evaluate › Interference Detection, while its window is open (assemblies).
    pub(crate) interference: Option<crate::interference::InterferenceWindow>,
    /// Evaluate › Section View, while its window is open.
    pub(crate) section: Option<crate::section_view::SectionView>,
    /// Which planes, axes, sketches and origins are drawn.
    pub(crate) visibility: crate::display::Visibility,
    /// Features expanded in the timeline (showing the sketch they absorb).
    pub(crate) expanded: std::collections::HashSet<FeatureId>,
    /// A feature the timeline scrolls to on the next frame (selected in the viewport).
    pub(crate) scroll_to_feature: Option<FeatureId>,
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
    /// The open feature command's feature, built and shown in the viewport.
    pub(crate) preview: Option<crate::preview::Preview>,
    /// The assembly command bar's tab.
    pub(crate) asm_tab: crate::assembly_ui::AsmTab,
    /// The assembly's own plane selected in its tree or the viewport.
    pub(crate) asm_plane: Option<StandardPlane>,
    /// A component the assembly tree scrolls to on the next frame.
    pub(crate) asm_scroll_to: Option<oxyde_assembly::ComponentId>,
    pub(crate) show_bom: bool,
    pub(crate) bom_mode: oxyde_assembly::bom::BomMode,
    /// Assembly revision the viewport shows.
    pub(crate) asm_scene: Option<u64>,
    /// The component being moved with Alt+drag.
    pub(crate) grabbing: Option<oxyde_assembly::ComponentId>,
    /// Move Component is on: a plain left-drag moves a component (Shift turns it).
    pub(crate) move_mode: bool,
    /// An arrow of the move handle being dragged.
    pub(crate) move_handle: Option<crate::assembly_ui::HandleDrag>,
    /// The move handle's arrows as drawn last frame, to know what the pointer is on.
    pub(crate) handle_shapes: Vec<crate::assembly_ui::HandleArrow>,
    /// A component whose part should open once unsaved changes are dealt with.
    pub(crate) pending_open_component: Option<crate::assembly::Occurrence>,
    /// The assemblies set aside while a component is edited in place, outermost first.
    pub(crate) edit_stack: Vec<crate::edit_in_place::Suspended>,
    /// An action to take once editing in place is finished, level by level.
    pub(crate) after_unwind: Option<PendingAction>,
    /// The edit locks this window holds (one per open document).
    pub(crate) locks: crate::lockfile::Locks,
    /// File › Export STL, while its window is open.
    pub(crate) mesh_export: Option<crate::mesh_export::MeshExportDialog>,
    /// File › Export › Image: the picture settings window.
    pub(crate) image_export: Option<crate::export::ImageExportDialog>,
    /// File › Export › Drawing: the sheet's settings and its views.
    pub(crate) drawing_export: Option<crate::drawing_export::DrawingExport>,
    /// The Render tab: its render and the meshes it is made from.
    pub(crate) render: crate::render_ui::RenderView,
    /// The Simulation tab: the study, its mesh and results.
    pub(crate) simulation: crate::simulation_ui::SimulationView,
    /// The GPU device egui draws with, when the window has one: the Render tab traces on
    /// it.
    pub(crate) wgpu_state: Option<eframe::egui_wgpu::RenderState>,
    pub(crate) prefs: crate::prefs::Preferences,
    pub(crate) prefs_window: crate::prefs_ui::PrefsWindow,
    pub(crate) debug_stats: crate::prefs_ui::DebugStats,
    /// Where the viewport was last drawn, for the debug overlay.
    pub(crate) viewport_rect: egui::Rect,
    pub(crate) splash: crate::splash::Splash,
    /// Decoded texture images of the open documents.
    pub(crate) texture_cache: crate::appearance_ui::TextureCache,
    /// The user's material library (`materials.json` next to the preferences).
    pub(crate) materials: oxyde_model::MaterialLibrary,
    /// Tools › Materials…: the library editor.
    pub(crate) materials_window: crate::materials_ui::MaterialsWindow,
    /// A question asked before something is done (over every other window).
    pub(crate) confirm: Option<crate::materials_ui::Confirm>,
    /// Work going on off the UI thread, with its progress window.
    pub(crate) job: Option<crate::jobs::Job>,
    /// The assistant panel (the *MCP* button): an agent modelling on this document.
    pub(crate) assistant: crate::assistant::Assistant,
    /// A bug caught (a panic in a rebuild or in a frame), shown until dismissed.
    pub(crate) internal_error: Option<crate::dialogs::InternalErrorReport>,
    /// Frames in a row that ended in a caught panic.
    failed_frames: u32,
}

impl OxydeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, file: Option<PathBuf>) -> Self {
        crate::theme::load_fonts(&cc.egui_ctx);
        let mut app = Self::blank(crate::prefs::Preferences::load(), file);
        let (materials, problem) = crate::materials::load();
        app.materials = materials;
        if let Some(problem) = problem {
            app.status = problem;
        }
        app.apply_preferences(&cc.egui_ctx);
        app.wgpu_state = cc.wgpu_render_state.clone();
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
        app
    }

    /// The application before it has a window: an empty part, the standard materials, no
    /// GPU, and nothing applied to egui yet. Tests build it to draw panels headlessly.
    pub(crate) fn blank(prefs: crate::prefs::Preferences, file: Option<PathBuf>) -> Self {
        Self {
            kernel: OcctKernel::new(),
            doc: Document::new(),
            viewport: Viewport::new(),
            selected_feature: None,
            selected_features: Vec::new(),
            selected_plane: None,
            selected_body: None,
            bodies_expanded: true,
            left_tab: crate::configurations_ui::LeftTab::Features,
            configurations_panel: Default::default(),
            selection: Vec::new(),
            dialog: None,
            sketch: None,
            equations: EquationsWindow::new(),
            part_equations_expanded: false,
            asm_equations_expanded: false,
            asm_part_equations_expanded: std::collections::HashSet::new(),
            place_expanded: std::collections::HashSet::new(),
            status: "Ready".to_owned(),
            tab: CommandTab::Features,
            pending_tool: None,
            hole_positions: false,
            show_mass_properties: false,
            show_measure: false,
            measure_cache: None,
            asm_measure_cache: None,
            interference: None,
            section: None,
            visibility: Default::default(),
            expanded: Default::default(),
            scroll_to_feature: None,
            scene_revision: None,
            summary: None,
            window_title: String::new(),
            allow_close: false,
            asm: None,
            mate_dialog: None,
            mate_preview: None,
            preview: None,
            asm_tab: Default::default(),
            asm_plane: None,
            asm_scroll_to: None,
            show_bom: false,
            bom_mode: oxyde_assembly::bom::BomMode::TopLevel,
            asm_scene: None,
            grabbing: None,
            move_mode: false,
            move_handle: None,
            handle_shapes: Vec::new(),
            pending_open_component: None,
            edit_stack: Vec::new(),
            after_unwind: None,
            locks: Default::default(),
            mesh_export: None,
            image_export: None,
            drawing_export: None,
            render: Default::default(),
            simulation: Default::default(),
            wgpu_state: None,
            prefs,
            prefs_window: Default::default(),
            debug_stats: Default::default(),
            viewport_rect: egui::Rect::NOTHING,
            // The file to open, if any, is opened by the splash screen's last step.
            splash: crate::splash::Splash::new(file),
            texture_cache: Default::default(),
            materials: oxyde_model::MaterialLibrary::standard(),
            materials_window: Default::default(),
            confirm: None,
            job: None,
            assistant: Default::default(),
            internal_error: None,
            failed_frames: 0,
        }
    }

    // ---- Model changes -----------------------------------------------------------------------

    pub(crate) fn rebuild(&mut self) {
        let start = std::time::Instant::now();
        if let Err(e) = self.doc.rebuild(&mut self.kernel) {
            self.report_internal_error(e);
        }
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

    /// Deletes several features as one undo step, later ones first so that a feature and
    /// the sketch it uses can go together. Nothing is deleted if a feature outside the set
    /// still uses one of them.
    pub(crate) fn delete_features(&mut self, ids: &[FeatureId]) {
        if ids.len() == 1 {
            self.delete_feature(ids[0]);
            return;
        }
        let position = |id: FeatureId| self.doc.part.index_of(id);
        let mut blocked: Vec<String> = Vec::new();
        for id in ids {
            for d in self.doc.part.dependents(*id) {
                if !ids.contains(&d) {
                    if let Some(f) = self.doc.part.feature(d) {
                        blocked.push(f.name().to_owned());
                    }
                }
            }
        }
        if !blocked.is_empty() {
            blocked.sort();
            blocked.dedup();
            self.status = format!("Cannot delete: used by {}", blocked.join(", "));
            return;
        }
        let mut sorted: Vec<FeatureId> = ids.to_vec();
        sorted.sort_by_key(|id| std::cmp::Reverse(position(*id)));
        self.doc.checkpoint();
        let mut deleted = 0;
        for id in sorted {
            if self.doc.part.delete(id).is_ok() {
                deleted += 1;
            }
        }
        self.selected_feature = None;
        self.selected_features.clear();
        self.rebuild();
        self.status = format!("Deleted {deleted} features");
    }

    pub(crate) fn undo(&mut self) {
        match self.doc.undo(&mut self.kernel) {
            Ok(true) => self.status = "Undone".to_owned(),
            Ok(false) => {}
            Err(e) => self.report_internal_error(e),
        }
    }

    pub(crate) fn redo(&mut self) {
        match self.doc.redo(&mut self.kernel) {
            Ok(true) => self.status = "Redone".to_owned(),
            Ok(false) => {}
            Err(e) => self.report_internal_error(e),
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
    pub(crate) fn selected_planar_face(&self) -> Option<FaceName> {
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
        let Some(body) = self.shown_body() else {
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
        let item = self.shown_body().and_then(|body| match pick {
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
                // The timeline shows the feature that made it.
                self.scroll_to_feature = self.selection_creator();
            }
        }
    }

    /// Faces and edges caught in a selection box: handed to the open command, else
    /// selected (added to the selection with Ctrl).
    fn box_select(&mut self, picks: Vec<Pick>, additive: bool) {
        if self.dialog.as_ref().is_some_and(|d| d.is_panel()) {
            for pick in picks {
                if let Some(panel_pick) = self.panel_pick(Some(pick)) {
                    self.offer_to_panel(panel_pick);
                }
            }
            return;
        }
        let Some(body) = self.shown_body() else {
            return;
        };
        let items: Vec<Selected> = picks
            .iter()
            .filter_map(|p| match p {
                Pick::Face(i) => body.naming.face(*i).cloned().map(Selected::Face),
                Pick::Edge(i) => body.naming.edge(*i).cloned().map(Selected::Edge),
            })
            .collect();
        if !additive {
            self.selection.clear();
            self.selected_plane = None;
        }
        for item in items {
            if !self.selection.contains(&item) {
                self.selection.push(item);
            }
        }
        let faces = self
            .selection
            .iter()
            .filter(|s| matches!(s, Selected::Face(_)))
            .count();
        let edges = self.selection.len() - faces;
        self.status = format!("{faces} face(s) and {edges} edge(s) selected");
        self.scroll_to_feature = self.selection_creator();
    }

    /// A face or edge clicked in the viewport, as offered to an open command.
    fn panel_pick(&self, pick: Option<Pick>) -> Option<PanelPick> {
        let body = self.shown_body()?;
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

    /// The sketch an extrude, a revolve or a sweep starts with: the one selected in the
    /// timeline, else the latest sketch nothing uses yet. `None` leaves the PropertyManager
    /// waiting for one.
    pub(crate) fn sketch_to_use(&self) -> Option<FeatureId> {
        let sketches = self.sketches();
        if let Some(id) = self.selected_feature {
            if sketches.iter().any(|(s, _)| *s == id) {
                return Some(id);
            }
        }
        let used = |id: FeatureId| {
            self.doc
                .part
                .features()
                .iter()
                .any(|f| f.kind().swept_sketches().contains(&id))
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
            self.open_dialog(Dialog::ChoosePlane);
            self.status = "Select a plane or a flat face for the sketch".to_owned();
        }
    }

    pub(crate) fn start_new_sketch(&mut self, plane: SketchPlane) {
        let unsaved = self.doc.modified;
        let id = self.add_feature(FeatureKind::Sketch(Sketch::new(plane)));
        self.edit_sketch(id, true);
        if let Some(session) = self.sketch.as_mut() {
            session.unsaved_before = unsaved;
        }
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
            let why = match self.doc.part.status(id) {
                Some(FeatureStatus::Error(message)) => message,
                _ => "its plane is missing".to_owned(),
            };
            self.status = format!("Cannot edit {name}: {why}");
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
        self.close_dialog();
        let mut session = SketchSession::new(id, name, plane, sketch.geometry, new_sketch, restore);
        session.driven = self
            .doc
            .part
            .dimensions()
            .into_iter()
            .filter(|d| d.driven)
            .filter_map(|d| match d.reference {
                oxyde_model::DimensionRef {
                    feature,
                    key: oxyde_model::DimensionKey::Sketch(relation),
                } if feature == id => Some(relation),
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
    /// that Extrude, Revolve or Sweep picks it up.
    pub(crate) fn finish_sketch(&mut self, keep: bool) {
        let Some(session) = self.sketch.take() else {
            return;
        };
        let _ = self.doc.part.roll_back_to(session.restore_rollback);
        let id = session.feature;
        // A brand-new sketch with nothing drawn (or whose drawing is discarded) goes, as
        // SolidWorks removes a cancelled sketch: adding it was no edit either, so the
        // undo step and the unsaved mark it left go with it.
        let empty = session.new_sketch && session.original_empty && (!keep || !session.changed());
        if keep && !empty && session.changed() {
            self.doc.checkpoint();
            if let Ok(FeatureKind::Sketch(sketch)) = self.doc.part.edit(id) {
                sketch.geometry = session.sketch.clone();
            }
        }
        if empty {
            let _ = self.doc.part.delete(id);
            if self.doc.drop_undo_if_current() {
                self.doc.modified = session.unsaved_before;
            }
            self.selected_feature = None;
        }
        self.rebuild();
        self.viewport.request_fit();
        self.tab = CommandTab::Features;
        // The Hole Wizard's positions are drawn: on to the holes.
        if std::mem::take(&mut self.hole_positions) && keep && !empty {
            self.selected_feature = Some(id);
            self.begin_hole();
            return;
        }
        self.status = if empty {
            format!("Left {} with nothing drawn: it is removed", session.name)
        } else if keep {
            format!(
                "Closed {}. With it selected, click Extrude, Revolve or Sweep to make a solid",
                session.name
            )
        } else {
            format!("Discarded changes to {}", session.name)
        };
    }

    /// Writes the sketch being edited into the part as it is drawn now, without leaving
    /// it: one undo step, as leaving it would make, with the rollback bar where it was
    /// before the sketch was opened (the snapshot is the part as the user sees it).
    pub(crate) fn commit_sketch(&mut self) {
        let Some(session) = self.sketch.as_mut() else {
            return;
        };
        if !session.changed() {
            return;
        }
        let id = session.feature;
        let editing_from = self
            .doc
            .part
            .index_of(id)
            .and_then(|i| self.doc.part.features().get(i + 1))
            .map(|f| f.id());
        let _ = self.doc.part.roll_back_to(session.restore_rollback);
        self.doc.checkpoint();
        if let Ok(FeatureKind::Sketch(sketch)) = self.doc.part.edit(id) {
            sketch.geometry = session.sketch.clone();
        }
        let _ = self.doc.part.roll_back_to(editing_from);
        // What is written is no longer a change of the session; and a new sketch with
        // something in it is no longer an empty one to remove on leaving.
        session.original = session.sketch.clone();
        session.original_empty = false;
        self.rebuild();
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
        self.open_extrude(None, sketch, None);
        if sketch.is_none() {
            self.status = "Select the sketch to extrude in the tree".to_owned();
        }
    }

    fn begin_blend(&mut self, fillet: bool) {
        let edges = self.selected_edges();
        if edges.is_empty() {
            self.status = "Click the edges to round in the graphics area".to_owned();
        }
        self.open_dialog(Dialog::Blend(BlendDialog::new(fillet, edges)));
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
            Command::Sweep => self.begin_sweep(),
            Command::Loft => self.open_loft(None, None),
            Command::Blend(fillet) => self.begin_blend(fillet),
            Command::Shell => self.begin_shell(),
            Command::Hole => self.begin_hole(),
            Command::Pattern(kind) => self.begin_pattern(kind),
            Command::Combine => self.open_combine(None, None),
            Command::RefPlane => self.open_ref_plane(None, None),
            Command::RefAxis => self.open_ref_axis(None, None),
        }
    }

    // ---- Files -------------------------------------------------------------------------------

    pub(crate) fn guard(&mut self, action: PendingAction, ctx: &egui::Context) {
        // Components edited in place are finished first, innermost first, each asking
        // about its unsaved changes; the action follows once the main assembly is back.
        if action.leaves_everything() && !self.edit_stack.is_empty() {
            self.after_unwind = Some(action);
            self.guard(PendingAction::FinishEdit, ctx);
            return;
        }
        // Editing a component in place sets the assembly aside, changes and all.
        if matches!(action, PendingAction::OpenComponent(_)) {
            self.perform(action, ctx);
            return;
        }
        if self.is_modified() {
            self.open_dialog(Dialog::Discard(action));
        } else {
            self.perform(action, ctx);
        }
    }

    pub(crate) fn perform(&mut self, action: PendingAction, ctx: &egui::Context) {
        use oxyde_format::{ASSEMBLY_EXTENSION, LEGACY_PART_EXTENSION, PART_EXTENSION};
        if action.leaves_everything() {
            // Normally finished one by one by `guard`; freed here if that was bypassed.
            self.drop_edit_stack();
        }
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
            PendingAction::OpenRecent(path) => {
                if path.exists() {
                    self.open_path(&path);
                } else {
                    // Moved or deleted since: say so and forget it.
                    self.status = format!("{} is no longer there", path.display());
                    self.prefs.recent.retain(|p| *p != path);
                    let _ = self.prefs.save();
                }
            }
            PendingAction::OpenComponent(o) => self.open_component(o),
            PendingAction::FinishEdit => self.finish_edit(ctx),
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
        self.replace_document_built(doc);
        self.rebuild();
    }

    /// Switches to `doc`, already rebuilt (by a job, or empty).
    pub(crate) fn replace_document_built(&mut self, doc: Document) {
        self.close_dialog();
        self.doc.release(&mut self.kernel);
        self.doc = doc;
        self.sketch = None;
        self.hole_positions = false;
        self.mesh_export = None;
        self.render.clear();
        self.selection.clear();
        self.selected_feature = None;
        self.selected_features.clear();
        self.selected_plane = None;
        self.selected_body = None;
        // Feature ids mean something else in the new document.
        self.expanded.clear();
        self.visibility.forget_items();
        self.scene_revision = None;
        self.tab = CommandTab::Features;
        self.viewport.request_fit();
    }

    pub(crate) fn open_path(&mut self, path: &Path) {
        self.open_path_with(path, crate::edit_in_place::LockMode::Lock);
    }

    /// Opens a part, assembly or STEP file, taking its edit lock as `mode` says (a STEP
    /// file is read into a new, unsaved part and needs none).
    pub(crate) fn open_path_with(&mut self, path: &Path, mode: crate::edit_in_place::LockMode) {
        if crate::assembly_ui::is_assembly(path) {
            self.open_assembly_with(path, mode);
            return;
        }
        if is_step(path) {
            self.leave_assembly();
            self.open_step_as_part(path);
            return;
        }
        let read_only = match self.claim(path, mode) {
            Ok(read_only) => read_only,
            Err(holder) => {
                // The assembly stays until the question is answered: Cancel keeps it.
                self.ask_about_lock(
                    path,
                    holder,
                    crate::edit_in_place::OpenRequest::Path(path.to_path_buf()),
                );
                return;
            }
        };
        self.leave_assembly();
        // Read and rebuilt off the UI thread; shown when done.
        self.open_part_job(path, read_only);
    }

    /// A new part whose first feature imports `path`: read off the UI thread.
    fn open_step_as_part(&mut self, path: &Path) {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_owned());
        let empty = oxyde_format::PartFile::new().definition;
        self.import_step_job(path, empty, Some(name));
    }

    /// A part rebuilt with an Imported feature, back from the job: a new document when
    /// `fresh` names it, else the open part with the feature added as one undo step.
    pub(crate) fn part_imported(
        &mut self,
        part: oxyde_model::Part,
        file_name: &str,
        fresh: Option<String>,
    ) {
        let id = part.timeline().last().map(|e| e.id);
        match fresh {
            Some(name) => {
                let mut doc = Document::new();
                doc.untitled_name = Some(name);
                doc.part = part;
                self.replace_document_built(doc);
                // Opening is not an edit: nothing to undo, nothing unsaved yet.
                self.doc.modified = false;
                self.doc.clear_history();
            }
            None => {
                let before = self.doc.part.definition();
                self.doc.part.release(&mut self.kernel);
                self.doc.part = part;
                self.doc.checkpoint_from(before);
                self.doc.revision = crate::document::next_revision();
                self.selected_feature = id;
                self.selection.clear();
            }
        }
        self.viewport.request_fit();
        self.status = match id.and_then(|id| self.doc.part.status(id)) {
            Some(FeatureStatus::Error(message)) => format!("Import failed: {message}"),
            _ => format!("Imported {file_name}"),
        };
    }

    /// File › Import STEP assembly: one part file per distinct part of `path` (an
    /// Imported feature holding its solids) and one assembly file per assembly, written
    /// into a folder the user picks; then opens the top-level document. The file is
    /// read off the UI thread first.
    fn import_step_assembly(&mut self, path: &Path) {
        self.plan_step_job(path);
    }

    /// The files of a STEP import, planned: asks for the folder, writes them and opens
    /// the top-level document.
    pub(crate) fn write_step_plan(
        &mut self,
        plan: &oxyde_format::step_import::ImportPlan,
        path: &Path,
        source: &str,
    ) {
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

    /// File › Import STEP: adds the file's solids to the part as an "Imported" feature at
    /// the rollback bar, read and rebuilt off the UI thread.
    pub(crate) fn import_step(&mut self) {
        if let Some(path) = pick_step() {
            self.finish_sketch(true);
            let definition = self.doc.part.definition();
            self.import_step_job(&path, definition, None);
        }
    }

    pub(crate) fn report_errors(&mut self) {
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
        // A sketch being edited goes into the file as it is drawn now, and stays open, as
        // in SolidWorks.
        self.commit_sketch();
        match self.doc.path.clone() {
            Some(path) if !self.doc.read_only => self.save_to(&path),
            _ => self.save_as(),
        }
    }

    pub(crate) fn save_as(&mut self) -> bool {
        use oxyde_format::{LEGACY_PART_EXTENSION, PART_EXTENSION};
        self.commit_sketch();
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
        // Saved under another name, it is another document, as in SolidWorks: with the
        // same id, an assembly would take it for the original (in its bill of materials,
        // or when the original goes missing).
        let copy = self.doc.path.as_deref().is_some_and(|p| p != path);
        let original_id = self.doc.document_id.clone();
        if copy {
            self.doc.document_id = oxyde_format::new_document_id();
        }
        let saved = self.save_to(&path);
        if !saved {
            self.doc.document_id = original_id;
        }
        saved
    }

    fn save_to(&mut self, path: &Path) -> bool {
        // A new name: the file must not be one another window edits.
        if self.doc.path.as_deref() != Some(path) || self.doc.read_only {
            if let Err(holder) = self.locks.acquire(path) {
                self.status = format!(
                    "Cannot save over {}: it is being edited by {}",
                    path.display(),
                    holder.describe()
                );
                return false;
            }
        }
        let mut file = self.doc.to_file();
        match &self.sketch {
            // In a sketch, the features after it are rolled back only while it is
            // edited: the file has the bar where it was, and the volume of the last save.
            Some(session) => {
                file.definition.rollback = session
                    .restore_rollback
                    .and_then(|id| self.doc.part.index_of(id));
            }
            None => {
                file.properties.volume_mm3 = self
                    .doc
                    .part
                    .body()
                    .and_then(|body| bodies_volume(&self.kernel, body).ok());
            }
        }
        match oxyde_format::save_part(path, &file) {
            Ok(()) => {
                self.doc.path = Some(path.to_path_buf());
                self.doc.read_only = false;
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
        self.sync_preview(ui.ctx());
        self.refresh_scene();
        self.simulation_scene();
        self.generative_scene();
        if self.sketch.is_some() {
            // A dimension's value is previewed only while its window is open.
            if !matches!(self.dialog, Some(Dialog::Dimension { .. }))
                && let Some(session) = self.sketch.as_mut()
            {
                session.clear_value_preview();
            }
            let output = self.viewport.ui(ui, Vec::new(), None, true);
            self.viewport_rect = output.rect;
            let request = match self.sketch.as_mut() {
                Some(session) => sketch_mode::interact(session, &self.viewport, ui, &output),
                None => None,
            };
            match request {
                Some(SketchRequest::EditDimension(index)) => self.open_dimension_editor(index),
                Some(SketchRequest::NewDimension(option)) => {
                    self.open_dialog(Dialog::Dimension {
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
        // Only for an empty part: once there is a sketch or a failed feature, the hint would
        // cover the geometry and contradict the status bar.
        let hint = (self.doc.part.features().is_empty()
            && self.dialog.is_none()
            && !self.splash.open)
            .then_some(
                "Start with a sketch (Sketch tab), then Extrude it — or File › Open demo bracket",
            );
        let output = self.viewport.ui(ui, picks, hint, false);
        self.viewport_rect = output.rect;
        let overlays = self.draw_references(ui, &output);
        self.render_over_viewport(ui, &output);
        self.simulation_over_viewport(ui, &output);
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
                // Points and sketch curves are only offered while the Plane command is open.
                Some(Overlay::Vertex(i)) => {
                    let name = self.shown_body().and_then(|b| b.naming.vertex(i).cloned());
                    if let Some(name) = name {
                        self.offer_to_panel(PanelPick::Vertex(name));
                    }
                }
                Some(Overlay::SketchPoint(id, entity)) => {
                    self.offer_to_panel(PanelPick::SketchPoint(id, entity));
                }
                Some(Overlay::SketchCurve(id, entity)) => {
                    self.offer_to_panel(PanelPick::SketchCurve(id, entity));
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
        if let Some(b) = output.box_select {
            let picks = self.viewport.picks_in(&b);
            self.box_select(picks, output.additive);
        }
        // A right-click selects the face or edge under the pointer first, as in
        // SolidWorks, so that the menu is about what the pointer is on.
        if output.response.secondary_clicked() && self.dialog.is_none() {
            let item = self.viewport.hovered().and_then(|pick| {
                let body = self.shown_body()?;
                match pick {
                    Pick::Face(i) => body.naming.face(i).cloned().map(Selected::Face),
                    Pick::Edge(i) => body.naming.edge(i).cloned().map(Selected::Edge),
                }
            });
            if let Some(item) = item.filter(|item| !self.selection.contains(item)) {
                self.selection = vec![item];
                self.selected_plane = None;
                self.scroll_to_feature = self.selection_creator();
            }
        }
        // The feature that made what is selected, and the sketch it sweeps.
        let made_by: Vec<(FeatureId, String)> = self
            .selection_creator()
            .and_then(|id| self.doc.part.feature(id))
            .filter(|f| !matches!(f.kind(), FeatureKind::Import(_)))
            .map(|f| {
                std::iter::once(f.id())
                    .chain(f.kind().swept_sketches())
                    .filter_map(|id| Some((id, self.doc.part.feature(id)?.name().to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        output.response.context_menu(|ui| {
            for (id, name) in &made_by {
                if ui.button(format!("Edit {name}")).clicked() {
                    ui.close_menu();
                    self.edit_feature(*id);
                }
            }
            if !made_by.is_empty() {
                ui.separator();
            }
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
            let has_body = self.doc.part.body().is_some();
            if ui
                .add_enabled(has_body, egui::Button::new("Appearance…"))
                .on_hover_text("Colour, transparency or texture of the selected faces, or the part")
                .clicked()
            {
                ui.close_menu();
                self.begin_appearance();
            }
            if ui
                .add_enabled(has_body, egui::Button::new("Material…"))
                .on_hover_text("What the part is made of: its look and its density")
                .clicked()
            {
                ui.close_menu();
                self.begin_material();
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
        let label = session
            .dimension_full_name(id)
            .unwrap_or_else(|| "Dimension".to_owned());
        let name = relation.name.clone();
        // Driven: the box shows its equation, to change or to type a value over.
        let text = if session.driven.contains(&id) {
            let reference = oxyde_model::DimensionRef {
                feature: session.feature,
                key: oxyde_model::DimensionKey::Sketch(id),
            };
            match self.dimension_equation_text(reference) {
                Ok(text) => text,
                Err(e) => {
                    self.status = e;
                    return;
                }
            }
        } else {
            crate::dialogs::format_value(value)
        };
        self.open_dialog(Dialog::Dimension {
            target: DimensionTarget::Existing(id),
            label,
            name,
            text,
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
                    session.add_relations(&relation);
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
            Some(Dialog::Extrude(d)) => {
                d.end == crate::dialogs::EndCondition::UpToSurface
                    && (d.up_to_next || d.up_to.is_none())
            }
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
            // Hovered: lit up like a selected one, when a command can take it.
            let quad = plane_quad(&to_screen, &plane, center, size);
            let hovered = pointer.is_some_and(|p| near_outline(&quad, p) || inside(&quad, p))
                && self.wants_planes();
            OverlayShape::Quad(paint_plane(
                &painter,
                &to_screen,
                &plane,
                center,
                size,
                &name,
                selected || hovered,
            ))
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
        let draw_reference =
            |geometry: ReferenceGeometry, name: String, selected: bool| match geometry {
                ReferenceGeometry::Plane(p) => draw_plane(p, name, selected),
                ReferenceGeometry::Axis(axis) => {
                    let color = overlay_color(selected);
                    let t = (center - axis.origin).dot(axis.direction);
                    let c = axis.origin + axis.direction * t;
                    let a = to_screen(c - axis.direction * size);
                    let b = to_screen(c + axis.direction * size);
                    painter.extend(egui::Shape::dashed_line(
                        &[a, b],
                        egui::Stroke::new(if selected { 2.0_f32 } else { 1.2_f32 }, color),
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
                    OverlayShape::Segment(a, b)
                }
            };
        // The plane or axis the open command makes, where it would be: in place of the
        // one it edits, or as a new one (which cannot be clicked yet).
        let references = self.doc.part.references();
        let (previewed, new_reference) = match self.preview_reference() {
            // An edited plane that does not build now is not among the references.
            Some((Some(id), _, geometry)) if references.iter().any(|(r, _)| *r == id) => {
                (Some((id, geometry)), None)
            }
            Some((_, name, geometry)) => (None, Some((name, geometry))),
            None => (None, None),
        };
        let highlighted = self.panel_features();
        for (id, geometry) in references {
            let (geometry, edited) = match previewed {
                Some((edited, preview)) if edited == id => (preview, true),
                _ => (geometry, false),
            };
            let selected = edited || self.selected_feature == Some(id) || highlighted.contains(&id);
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
            let shape = draw_reference(geometry, name, selected);
            overlays.push((Overlay::Reference(id), shape));
        }
        if let Some((name, geometry)) = new_reference {
            draw_reference(geometry, name, true);
        }

        // The Plane command takes points and the curves of sketches one by one: each curve
        // is clicked on its own, and the point nearest the pointer is drawn and clicked.
        let plane_dialog = match &self.dialog {
            Some(Dialog::RefPlane(d)) => Some(d),
            _ => None,
        };
        let held_curves: Vec<(FeatureId, u32)> = plane_dialog
            .map(|d| d.held_sketch_curves())
            .unwrap_or_default();
        let mut entity_overlays = Vec::new();
        let mut points: Vec<(Overlay, egui::Pos2)> = Vec::new();

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
            for (entity, curve, construction) in crate::display::sketch_curves(&result.solved) {
                let path: Vec<egui::Pos2> = curve
                    .into_iter()
                    .map(|p| to_screen(result.plane.to_world(p)))
                    .collect();
                if plane_dialog.is_some() {
                    if held_curves.contains(&(id, entity)) {
                        painter.add(egui::Shape::line(
                            path.clone(),
                            egui::Stroke::new(3.5_f32, overlay_color(true)),
                        ));
                    }
                    entity_overlays.push((
                        Overlay::SketchCurve(id, entity),
                        OverlayShape::Polylines(vec![path.clone()]),
                    ));
                }
                let color = if selected {
                    hovered_color
                } else {
                    if crate::theme::view_dark() {
                        egui::Color32::from_rgb(130, 175, 235)
                    } else {
                        egui::Color32::from_rgb(40, 70, 150)
                    }
                };
                let stroke = egui::Stroke::new(if selected { 2.0_f32 } else { 1.3_f32 }, color);
                if construction {
                    painter.extend(egui::Shape::dashed_line(&path, stroke, 6.0, 4.0));
                } else {
                    painter.add(egui::Shape::line(path.clone(), stroke));
                }
                lines.push(path);
            }
            // Its lone points, as small crosses (hole positions): clicked like its curves.
            let ink = if selected {
                hovered_color
            } else {
                overlay_color(false)
            };
            for p in crate::display::lone_points(&result.solved) {
                let c = to_screen(result.plane.to_world(p));
                let stroke = egui::Stroke::new(1.4_f32, ink);
                painter.line_segment([c - egui::vec2(4.0, 0.0), c + egui::vec2(4.0, 0.0)], stroke);
                painter.line_segment([c - egui::vec2(0.0, 4.0), c + egui::vec2(0.0, 4.0)], stroke);
                lines.push(vec![c, c]);
            }
            if plane_dialog.is_some() {
                for e in result.solved.entities() {
                    if let oxyde_sketch::Geometry::Point(p) = e.geometry {
                        let at = to_screen(result.plane.to_world(p));
                        points.push((Overlay::SketchPoint(id, e.id.0), at));
                    }
                }
            }
            overlays.push((Overlay::Reference(id), OverlayShape::Polylines(lines)));
        }

        if let Some(d) = plane_dialog {
            if let Some(body) = self.shown_body() {
                for (i, v) in body.topology.vertices.iter().enumerate() {
                    points.push((Overlay::Vertex(i as u32), to_screen(v.point)));
                }
            }
            // The points it holds, then the one the pointer is on, which takes the click
            // before the edge or face under it.
            for p in self.plane_dialog_points(d) {
                painter.circle_filled(to_screen(p), 4.5, overlay_color(true));
            }
            let nearest = pointer.and_then(|at| {
                points
                    .iter()
                    .map(|(overlay, p)| (p.distance(at), *overlay, *p))
                    .filter(|(distance, ..)| *distance < POINT_PICK_RADIUS)
                    .min_by(|a, b| a.0.total_cmp(&b.0))
            });
            let mut front = Vec::new();
            if let Some((_, overlay, p)) = nearest {
                painter.circle_filled(p, 4.5, crate::theme::tokens().accent);
                painter.circle_stroke(
                    p,
                    6.5,
                    egui::Stroke::new(1.0_f32, crate::theme::tokens().accent),
                );
                front.push((overlay, OverlayShape::Dot(p)));
            }
            front.append(&mut entity_overlays);
            front.append(&mut overlays);
            overlays = front;
        }

        // The origin: three short axes of fixed screen length.
        if self.item_visible(crate::display::Item::Origin) {
            paint_origin(&painter, &to_screen);
        }
        overlays
    }

    /// Re-tessellates the body after each rebuild, and the open command's preview.
    fn refresh_scene(&mut self) {
        // While a job reads a document, the picture stays as it was (the assembly a
        // component was edited in, say, until it is back).
        let wanted = self.scene_key();
        if self.busy() || self.scene_revision == Some(wanted) {
            return;
        }
        self.scene_revision = Some(wanted);
        let start = std::time::Instant::now();
        let first = self.viewport.is_empty();
        let Some(body) = self.shown_body() else {
            self.viewport.pickable = None;
            self.viewport.set_scene(Scene::default(), false);
            return;
        };
        match tessellate_bodies(&self.kernel, body, self.mesh_fraction()) {
            Ok(mut mesh) => {
                let (mut materials, textures) = self.part_materials();
                self.tint_generative_roles(&mut materials);
                self.add_context(&mut mesh, &mut materials);
                self.viewport.pickable = self.own_mesh_counts(&mesh);
                let mut scene = Scene::from_mesh(&mesh);
                scene.set_materials(materials, textures);
                self.viewport.set_scene(scene, first);
            }
            Err(e) => self.status = format!("Display failed: {e}"),
        }
        // A preview slow to show is not built again while a value is dragged.
        if wanted != self.doc.revision {
            self.preview_shown_in(start.elapsed().as_secs_f64());
        }
    }

    /// The plane "Normal To" turns the view to: the sketch's, else the selected plane or
    /// flat face; in an assembly, its selected plane.
    pub(crate) fn normal_to_target(&self) -> Option<Plane> {
        if self.asm.is_some() {
            return self.asm_plane.map(|p| p.plane());
        }
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
        if ctx.input(|i| i.viewport().close_requested())
            && !self.allow_close
            && (self.any_modified() || !self.edit_stack.is_empty())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.guard(PendingAction::Exit, ctx);
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let (name, read_only) = match &self.asm {
            Some(doc) => (doc.display_name(), doc.read_only),
            None => (self.doc.display_name(), self.doc.read_only),
        };
        let context = self
            .editing_in()
            .map(|(_, parent)| format!(" (in {parent})"))
            .unwrap_or_default();
        let title = format!(
            "{name}{context}{}{} - Oxyde",
            if read_only { " (read-only)" } else { "" },
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

/// The screen square a plane is drawn as (see [`paint_plane`]).
pub(crate) fn plane_quad(
    to_screen: &dyn Fn(Point3) -> egui::Pos2,
    plane: &Plane,
    center: Point3,
    size: f64,
) -> [egui::Pos2; 4] {
    let to_center = center - plane.origin;
    let c = center - plane.normal * to_center.dot(plane.normal);
    let (u, v): (Vec3, Vec3) = (plane.x_dir * size, plane.y_dir() * size);
    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(a, b)| to_screen(c + u * a + v * b))
}

/// Whether `p` is inside a convex quad.
pub(crate) fn inside(quad: &[egui::Pos2; 4], p: egui::Pos2) -> bool {
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

pub(crate) fn near_outline(quad: &[egui::Pos2; 4], p: egui::Pos2) -> bool {
    (0..4).any(|k| segment_distance(p, quad[k], quad[(k + 1) % 4]) < 6.0)
}

/// The overlay clicked at `p`: an outline or axis near the pointer first; inside a plane
/// only when the click did not land on the part.
pub(crate) fn overlay_at(
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
        OverlayShape::Dot(c) => c.distance(p) < POINT_PICK_RADIUS,
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

/// How near the pointer a point must be, in pixels, to be picked rather than the edge or
/// face under it.
const POINT_PICK_RADIUS: f32 = 8.0;

/// Mesh precision: about a thousandth of the part size.
/// The meshes of every body of a part, one after the other, so that faces and edges are
/// numbered as in `body.topology`.
pub(crate) fn tessellate_bodies<K: Kernel>(
    kernel: &K,
    body: &oxyde_model::Body,
    fraction: f64,
) -> Result<oxyde_kernel::Mesh, K::Error> {
    let deflection = oxyde_render::mesh_deflection(&body.topology, fraction);
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

impl eframe::App for OxydeApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // The last net: a panic anywhere in a frame (a command applied, a panel drawn)
        // ends the frame, not the application. See `recover_frame`.
        let undo_before = self.doc.undo_depth();
        match oxyde_kernel::catch_panic(|| self.frame(ctx, frame)) {
            Ok(()) => self.failed_frames = 0,
            Err(message) => {
                self.recover_frame(message, undo_before);
                ctx.request_repaint();
            }
        }
        self.internal_error_window(ctx);
    }
}

impl OxydeApp {
    /// After a panic got out of a frame: takes back the change the frame recorded, if it
    /// recorded one before failing, and tells the user. A frame that fails again and
    /// again fails because of what is open, so the sketch session and the command are
    /// closed on the third time, which leaves the menus to save from.
    fn recover_frame(&mut self, message: String, undo_before: usize) {
        self.failed_frames += 1;
        let undone = self.doc.undo_depth() > undo_before
            && matches!(
                oxyde_kernel::catch_panic(|| self.doc.discard_last_change(&mut self.kernel)),
                Ok(Ok(true))
            );
        if self.failed_frames >= 3 {
            if oxyde_kernel::catch_panic(|| self.finish_sketch(false)).is_err() {
                self.sketch = None;
            }
            if oxyde_kernel::catch_panic(|| self.close_dialog()).is_err() {
                self.dialog = None;
            }
            self.confirm = None;
        }
        self.report_internal_error(crate::document::InternalError { message, undone });
    }

    fn frame(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_close_request(ctx);
        self.splash_step(ctx);
        self.sync_locks();
        self.render_housekeeping(ctx);
        self.simulation_housekeeping(ctx);
        self.serve_assistant();
        let busy = self.busy();
        if self.asm.is_some() {
            self.clear_preview();
            if !busy {
                self.assembly_shortcuts(ctx);
            }
            egui::TopBottomPanel::top("menu").show(ctx, |ui| self.assembly_menu_bar(ui));
            egui::TopBottomPanel::top("commands").show(ctx, |ui| self.assembly_command_bar(ui));
            egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.assembly_status_bar(ui));
            egui::SidePanel::left("feature_manager")
                .resizable(true)
                .default_width(290.0)
                .min_width(220.0)
                .show(ctx, |ui| self.assembly_panel(ui));
            self.assistant_panel(ctx);
            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show(ctx, |ui| self.assembly_viewport(ui));
            self.bom_window(ctx);
            self.assembly_equations_window(ctx);
            self.evaluate_windows(ctx);
            self.interference_window(ctx);
            self.mesh_export_window(ctx);
            self.image_export_window(ctx);
            self.drawing_export_window(ctx);
            self.materials_window(ctx);
            self.preferences_window(ctx);
            self.debug_overlay(ctx, self.viewport_rect);
            self.dialogs_ui(ctx);
            self.confirm_window(ctx);
            self.job_window(ctx);
            self.splash_ui(ctx);
            self.update_title(ctx);
            return;
        }
        self.viewport.grab_enabled = false;
        if !busy {
            self.handle_shortcuts(ctx);
        }

        egui::TopBottomPanel::top("menu").show(ctx, |ui| self.menu_bar(ui));
        egui::TopBottomPanel::top("commands").show(ctx, |ui| self.command_bar(ui));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.status_bar(ui));
        egui::SidePanel::left("feature_manager")
            .resizable(true)
            .default_width(290.0)
            .min_width(220.0)
            .show(ctx, |ui| {
                if self.tab == CommandTab::Render && self.sketch.is_none() {
                    self.render_panel(ui);
                }
                if self.tab == CommandTab::Simulation && self.sketch.is_none() {
                    self.simulation_panel(ui);
                }
                if self.tab == CommandTab::Generative && self.sketch.is_none() {
                    self.generative_panel(ui);
                }
                self.property_manager(ui);
                self.left_panel_tabs(ui);
                match self.left_tab {
                    crate::configurations_ui::LeftTab::Features => self.timeline_ui(ui),
                    crate::configurations_ui::LeftTab::Configurations => self.configurations_ui(ui),
                }
            });
        self.assistant_panel(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| self.viewport_ui(ui));

        self.equations_window(ctx);
        self.evaluate_windows(ctx);
        self.mesh_export_window(ctx);
        self.image_export_window(ctx);
        self.drawing_export_window(ctx);
        self.materials_window(ctx);
        self.preferences_window(ctx);
        self.debug_overlay(ctx, self.viewport_rect);
        self.dialogs_ui(ctx);
        self.confirm_window(ctx);
        self.job_window(ctx);
        self.splash_ui(ctx);
        self.update_title(ctx);
    }
}

impl Drop for OxydeApp {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.join();
        }
        self.leave_assembly();
        self.drop_edit_stack();
        self.clear_preview();
        self.doc.release(&mut self.kernel);
        self.locks.release_all();
    }
}
