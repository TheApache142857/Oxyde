//! Application state and the top-level layout: menus, command bar, panels, status bar.

use std::path::{Path, PathBuf};

use eframe::egui;
use oxyde_kernel::{Kernel, SurfaceKind, Topology};
use oxyde_model::{
    EdgeName, ExtrudeOp, FaceName, FeatureId, FeatureKind, FeatureStatus, StandardPlane,
};
use oxyde_occt::OcctKernel;
use oxyde_render::{Pick, Scene, StandardView};

use crate::dialogs::{BlendDialog, Dialog, ExtrudeDialog, PlaneChoice, SketchDialog, SketchShape};
use crate::document::Document;
use crate::viewport::Viewport;

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
    Open,
    Demo,
    Exit,
}

pub struct OxydeApp {
    pub(crate) kernel: OcctKernel,
    pub(crate) doc: Document,
    pub(crate) viewport: Viewport,
    pub(crate) selected_feature: Option<FeatureId>,
    pub(crate) selected_plane: Option<StandardPlane>,
    pub(crate) selection: Vec<Selected>,
    pub(crate) dialog: Option<Dialog>,
    pub(crate) status: String,
    /// Document revision the viewport currently shows.
    scene_revision: Option<u64>,
    /// Status-bar summary, cached per document revision.
    summary: Option<(u64, String)>,
    window_title: String,
    allow_close: bool,
}

impl OxydeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, file: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        let mut app = Self {
            kernel: OcctKernel::new(),
            doc: Document::new(),
            viewport: Viewport::new(),
            selected_feature: None,
            selected_plane: None,
            selection: Vec::new(),
            dialog: None,
            status: "Ready".to_owned(),
            scene_revision: None,
            summary: None,
            window_title: String::new(),
            allow_close: false,
        };
        if let Some(path) = file {
            app.open_path(&path);
        }
        app
    }

    // ---- Model changes -----------------------------------------------------------------------

    pub(crate) fn rebuild(&mut self) {
        self.doc.rebuild(&mut self.kernel);
    }

    /// Adds a feature at the rollback bar as one undo step, rebuilds and selects it.
    pub(crate) fn add_feature(&mut self, kind: FeatureKind) -> FeatureId {
        self.doc.checkpoint();
        let id = self.doc.part.add_feature(kind);
        self.rebuild();
        self.selected_feature = Some(id);
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

    fn undo(&mut self) {
        if self.doc.undo(&mut self.kernel) {
            self.status = "Undone".to_owned();
        }
    }

    fn redo(&mut self) {
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
        self.selection
            .iter()
            .filter_map(|s| match s {
                Selected::Face(name) => body.naming.find_face(name).map(Pick::Face),
                Selected::Edge(name) => body.naming.find_edge(name).map(Pick::Edge),
            })
            .collect()
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
                }
            }
        }
    }

    // ---- Commands ----------------------------------------------------------------------------

    fn sketches(&self) -> Vec<(FeatureId, String)> {
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

    fn begin_sketch(&mut self) {
        let face = self.selected_planar_face();
        let plane = match (&face, self.selected_plane) {
            (Some(face), _) => PlaneChoice::Face(face.clone()),
            (None, Some(plane)) => PlaneChoice::Standard(plane),
            (None, None) => PlaneChoice::Standard(StandardPlane::Front),
        };
        self.dialog = Some(Dialog::Sketch(SketchDialog {
            plane,
            face,
            shape: SketchShape::Rectangle,
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 20.0,
            radius: 5.0,
        }));
    }

    fn begin_extrude(&mut self, op: ExtrudeOp) {
        let selected_sketch = self.selected_feature.filter(|id| {
            self.doc
                .part
                .feature(*id)
                .is_some_and(|f| matches!(f.kind(), FeatureKind::Sketch(_)))
        });
        let sketch = selected_sketch.or_else(|| self.sketches().last().map(|(id, _)| *id));
        match sketch {
            Some(sketch) => self.begin_extrude_of(sketch, op),
            None => self.status = "Create a sketch first".to_owned(),
        }
    }

    pub(crate) fn begin_extrude_of(&mut self, sketch: FeatureId, op: ExtrudeOp) {
        self.dialog = Some(Dialog::Extrude(ExtrudeDialog {
            sketch,
            sketches: self.sketches(),
            op,
            depth: 10.0,
            mid_plane: false,
            reverse: false,
        }));
    }

    fn begin_blend(&mut self, fillet: bool) {
        let edges = self.selected_edges();
        if edges.is_empty() {
            self.status = "Select edges in the viewport first".to_owned();
            return;
        }
        self.dialog = Some(Dialog::Blend(BlendDialog {
            fillet,
            size: if fillet { 2.0 } else { 1.0 },
            edges,
        }));
    }

    // ---- Files -------------------------------------------------------------------------------

    fn guard(&mut self, action: PendingAction, ctx: &egui::Context) {
        if self.doc.modified {
            self.dialog = Some(Dialog::Discard(action));
        } else {
            self.perform(action, ctx);
        }
    }

    pub(crate) fn perform(&mut self, action: PendingAction, ctx: &egui::Context) {
        match action {
            PendingAction::New => {
                self.replace_document(Document::new());
                self.status = "New part".to_owned();
            }
            PendingAction::Open => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Oxyde part", &["oxy"])
                    .pick_file()
                {
                    self.open_path(&path);
                }
            }
            PendingAction::Demo => {
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
        self.selection.clear();
        self.selected_feature = None;
        self.scene_revision = None;
        self.viewport.request_fit();
        self.rebuild();
    }

    pub(crate) fn open_path(&mut self, path: &Path) {
        let loaded = oxyde_format::load_part(path)
            .map_err(|e| e.to_string())
            .and_then(|file| Document::from_file(file, Some(path.to_path_buf())));
        match loaded {
            Ok(doc) => {
                self.replace_document(doc);
                self.status = format!("Opened {}", path.display());
                self.report_errors();
            }
            Err(e) => self.status = format!("Could not open {}: {e}", path.display()),
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
        match self.doc.path.clone() {
            Some(path) => self.save_to(&path),
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) -> bool {
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("Oxyde part", &["oxy"])
            .set_file_name(format!("{}.oxy", self.doc.display_name()))
            .save_file()
        else {
            return false;
        };
        let has_extension = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("oxy"));
        if !has_extension {
            path.set_extension("oxy");
        }
        self.save_to(&path)
    }

    fn save_to(&mut self, path: &Path) -> bool {
        let mut file = self.doc.to_file();
        file.properties.volume_mm3 = self
            .doc
            .part
            .body()
            .and_then(|body| self.kernel.volume(body.shape).ok());
        match oxyde_format::save_part(path, &file) {
            Ok(()) => {
                self.doc.path = Some(path.to_path_buf());
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

    fn export_step(&mut self) {
        let Some(shape) = self.doc.part.body().map(|b| b.shape) else {
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
        self.status = match self.kernel.export_step(&[shape], &path) {
            Ok(()) => format!("Exported {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    // ---- Layout ------------------------------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::New, &ctx);
                }
                if ui.button("Open…").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::Open, &ctx);
                }
                if ui.button("Open demo bracket").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::Demo, &ctx);
                }
                ui.separator();
                if ui.button("Save").clicked() {
                    ui.close_menu();
                    self.save();
                }
                if ui.button("Save as…").clicked() {
                    ui.close_menu();
                    self.save_as();
                }
                if ui.button("Export STEP…").clicked() {
                    ui.close_menu();
                    self.export_step();
                }
                ui.separator();
                if ui.button("Part properties…").clicked() {
                    ui.close_menu();
                    self.dialog = Some(Dialog::PartProperties(self.doc.properties.clone()));
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    ui.close_menu();
                    self.guard(PendingAction::Exit, &ctx);
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(self.doc.can_undo(), egui::Button::new("Undo"))
                    .clicked()
                {
                    ui.close_menu();
                    self.undo();
                }
                if ui
                    .add_enabled(self.doc.can_redo(), egui::Button::new("Redo"))
                    .clicked()
                {
                    ui.close_menu();
                    self.redo();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        self.selected_feature.is_some(),
                        egui::Button::new("Delete feature"),
                    )
                    .clicked()
                {
                    ui.close_menu();
                    if let Some(id) = self.selected_feature {
                        self.delete_feature(id);
                    }
                }
                if ui.button("Rebuild all").clicked() {
                    ui.close_menu();
                    self.doc.release(&mut self.kernel);
                    self.rebuild();
                }
            });
            ui.menu_button("View", |ui| {
                for (label, view) in VIEWS {
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
            });
            ui.menu_button("Help", |ui| {
                if ui.button("About Oxyde").clicked() {
                    ui.close_menu();
                    self.dialog = Some(Dialog::About);
                }
            });
        });
    }

    fn command_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("Sketch")
                .on_hover_text("New sketch on a plane or on the selected face")
                .clicked()
            {
                self.begin_sketch();
            }
            let has_sketch = !self.sketches().is_empty();
            if ui
                .add_enabled(has_sketch, egui::Button::new("Extruded Boss"))
                .clicked()
            {
                self.begin_extrude(ExtrudeOp::Boss);
            }
            if ui
                .add_enabled(has_sketch, egui::Button::new("Extruded Cut"))
                .clicked()
            {
                self.begin_extrude(ExtrudeOp::Cut);
            }
            let has_edges = self
                .selection
                .iter()
                .any(|s| matches!(s, Selected::Edge(_)));
            if ui
                .add_enabled(has_edges, egui::Button::new("Fillet"))
                .on_disabled_hover_text("Select edges in the viewport first")
                .clicked()
            {
                self.begin_blend(true);
            }
            if ui
                .add_enabled(has_edges, egui::Button::new("Chamfer"))
                .on_disabled_hover_text("Select edges in the viewport first")
                .clicked()
            {
                self.begin_blend(false);
            }
            ui.separator();
            for (label, view) in VIEWS {
                if ui.button(label).clicked() {
                    self.viewport.set_view(view);
                }
            }
            if ui.button("Fit").clicked() {
                self.viewport.request_fit();
            }
            ui.separator();
            if ui
                .add_enabled(self.doc.can_undo(), egui::Button::new("Undo"))
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(self.doc.can_redo(), egui::Button::new("Redo"))
                .clicked()
            {
                self.redo();
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let revision = self.doc.revision;
        if self.summary.as_ref().map(|(r, _)| *r) != Some(revision) {
            let text = match self.doc.part.body() {
                Some(body) => {
                    let volume = self
                        .kernel
                        .volume(body.shape)
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
        ui.horizontal(|ui| {
            ui.label(&self.status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((_, text)) = &self.summary {
                    ui.label(text);
                }
            });
        });
    }

    fn viewport_ui(&mut self, ui: &mut egui::Ui) {
        self.refresh_scene();
        let picks = self.selection_picks();
        let hint = self
            .doc
            .part
            .body()
            .is_none()
            .then_some("Start with Sketch, then Extruded Boss — or File › Open demo bracket");
        let output = self.viewport.ui(ui, picks, hint);
        if let Some(pick) = output.clicked {
            self.click(pick, output.additive);
        }
        output.response.context_menu(|ui| {
            if ui.button("Sketch").clicked() {
                ui.close_menu();
                self.begin_sketch();
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
                self.begin_blend(true);
            }
            if ui
                .add_enabled(has_edges, egui::Button::new("Chamfer"))
                .clicked()
            {
                ui.close_menu();
                self.begin_blend(false);
            }
            ui.separator();
            if ui.button("Zoom to fit").clicked() {
                ui.close_menu();
                self.viewport.request_fit();
            }
            if ui.button("Clear selection").clicked() {
                ui.close_menu();
                self.selection.clear();
            }
        });
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
        match self
            .kernel
            .tessellate(body.shape, deflection(&body.topology))
        {
            Ok(mesh) => self.viewport.set_scene(Scene::from_mesh(&mesh), first),
            Err(e) => self.status = format!("Display failed: {e}"),
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let command = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let (redo_shift, undo, redo, save, open, new) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&KeyboardShortcut::new(
                    Modifiers::COMMAND | Modifiers::SHIFT,
                    Key::Z,
                )),
                i.consume_shortcut(&command(Key::Z)),
                i.consume_shortcut(&command(Key::Y)),
                i.consume_shortcut(&command(Key::S)),
                i.consume_shortcut(&command(Key::O)),
                i.consume_shortcut(&command(Key::N)),
            )
        });
        if undo {
            self.undo();
        }
        if redo || redo_shift {
            self.redo();
        }
        if save {
            self.save();
        }
        if open {
            self.guard(PendingAction::Open, ctx);
        }
        if new {
            self.guard(PendingAction::New, ctx);
        }

        // Plain keys only when no text field has the focus.
        if self.dialog.is_none() && !ctx.wants_keyboard_input() {
            let (fit, delete, escape) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::F),
                    i.consume_key(Modifiers::NONE, Key::Delete),
                    i.consume_key(Modifiers::NONE, Key::Escape),
                )
            });
            if fit {
                self.viewport.request_fit();
            }
            if delete {
                if let Some(id) = self.selected_feature {
                    self.delete_feature(id);
                }
            }
            if escape {
                self.selection.clear();
            }
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && self.doc.modified && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::Discard(PendingAction::Exit));
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = format!(
            "{}{} - Oxyde",
            self.doc.display_name(),
            if self.doc.modified { " *" } else { "" }
        );
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }
}

const VIEWS: [(&str, StandardView); 4] = [
    ("Front", StandardView::Front),
    ("Top", StandardView::Top),
    ("Right", StandardView::Right),
    ("Isometric", StandardView::Isometric),
];

/// Mesh precision: about a thousandth of the part size.
fn deflection(topology: &Topology) -> f64 {
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
        (diagonal * 0.001).clamp(0.002, 5.0)
    } else {
        0.05
    }
}

impl eframe::App for OxydeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_close_request(ctx);
        self.handle_shortcuts(ctx);

        egui::TopBottomPanel::top("menu").show(ctx, |ui| self.menu_bar(ui));
        egui::TopBottomPanel::top("commands").show(ctx, |ui| self.command_bar(ui));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.status_bar(ui));
        egui::SidePanel::left("feature_manager")
            .resizable(true)
            .default_width(290.0)
            .min_width(200.0)
            .show(ctx, |ui| {
                self.timeline_ui(ui);
                ui.separator();
                self.properties_ui(ui);
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| self.viewport_ui(ui));

        self.dialogs_ui(ctx);
        self.update_title(ctx);
    }
}

impl Drop for OxydeApp {
    fn drop(&mut self) {
        self.doc.release(&mut self.kernel);
    }
}
