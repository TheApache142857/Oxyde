//! Sketch mode: drawing, dragging, trimming and constraining a sketch in the viewport.
//!
//! The session edits a working copy of the sketch; it is written back to the timeline when
//! the user exits the sketch. Colours follow SolidWorks: blue geometry can still move,
//! black is fully defined, red takes part in conflicting relations.
//!
//! While drawing, the cursor snaps to existing points (the origin among them), to the end
//! points, middles and centres of the part's edges lying in the sketch plane, to the middle
//! of lines, to crossings and onto curves; the new point keeps that relation (coincident,
//! fixed on the part, midpoint, on curve). Convert Entities copies part edges into the
//! sketch.

use std::collections::HashSet;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Shape, Stroke, Vec2};
use oxyde_geom::{Plane, Point2};
use oxyde_model::FeatureId;
use oxyde_sketch::{
    ConstrainedSketch, Constraint, ConstraintState, Definition, EntityId, Geometry, RelationId,
    SolveReport,
};

use crate::icons::Icon;
use crate::model_refs::{ModelCurve, ModelGeometry, ModelPointKind};
use crate::viewport::{Viewport, ViewportOutput};

/// The cursor snaps to a point or a crossing within this many points.
const SNAP: f32 = 8.0;
/// Clicking within this many points of a curve selects it (or snaps onto it).
const PICK: f32 = 6.0;
/// Lines drawn within 3° of horizontal or vertical get that relation automatically.
const AUTO_SLOPE: f64 = 0.0524;
/// Two lines within this angle (radians) are dimensioned as parallel.
const PARALLEL: f64 = 0.01;
const UNDO_LIMIT: usize = 100;

/// A colour for drawing over the viewport: `light` on a light background, `dark` on a
/// dark one.
fn on_view(light: [u8; 3], dark: [u8; 3]) -> Color32 {
    let [r, g, b] = if crate::theme::view_dark() {
        dark
    } else {
        light
    };
    Color32::from_rgb(r, g, b)
}

fn ink_selected() -> Color32 {
    on_view([255, 128, 0], [255, 160, 40])
}
fn ink_hovered() -> Color32 {
    on_view([255, 170, 60], [255, 196, 110])
}
fn ink_conflict() -> Color32 {
    on_view([200, 40, 40], [239, 83, 80])
}
fn ink_fully() -> Color32 {
    on_view([20, 20, 20], [236, 241, 246])
}
fn ink_under() -> Color32 {
    on_view([0, 90, 220], [79, 195, 247])
}
fn ink_construction() -> Color32 {
    on_view([110, 140, 190], [132, 164, 206])
}
fn ink_preview() -> Color32 {
    on_view([120, 120, 120], [170, 180, 190])
}
fn ink_dimension() -> Color32 {
    on_view([90, 90, 90], [200, 210, 220])
}
fn ink_snapped() -> Color32 {
    on_view([230, 120, 0], [255, 170, 60])
}
fn ink_trimmed() -> Color32 {
    on_view([220, 40, 40], [239, 83, 80])
}
fn ink_model() -> Color32 {
    on_view([130, 130, 130], [150, 162, 175])
}
/// Behind dimension values, so they read over the geometry in either theme.
fn label_background() -> Color32 {
    let [r, g, b] = if crate::theme::view_dark() {
        [28, 38, 50]
    } else {
        [255, 255, 255]
    };
    Color32::from_rgba_unmultiplied(r, g, b, 220)
}
fn label_border() -> Color32 {
    on_view([160, 160, 160], [95, 110, 128])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Dimension,
    Line,
    Centerline,
    Rectangle,
    CenterRectangle,
    /// Straight slot, centre to centre: two half circles joined by tangent lines.
    Slot,
    Circle,
    Arc,
    /// Arc through three clicks: its start, its end, and a point on it.
    ThreePointArc,
    /// Arc from the end of a line, tangent to it.
    TangentArc,
    /// Regular polygon around a centre, its corners on a construction circle.
    Polygon,
    /// A lone sketch point (a hole's position, a reference).
    Point,
    Trim,
    /// Extend Entities: a line or arc lengthened to the next curve it meets.
    Extend,
    Fillet,
    Offset,
    Convert,
    Mirror,
}

impl Tool {
    /// The tools of the Sketch tab, in order, with icon and label.
    pub const ALL: [(Tool, Icon, &'static str); 19] = [
        (Tool::Select, Icon::Select, "Select"),
        (Tool::Dimension, Icon::SmartDimension, "Smart Dimension"),
        (Tool::Line, Icon::Line, "Line"),
        (Tool::Centerline, Icon::Centerline, "Centerline"),
        (Tool::Rectangle, Icon::Rectangle, "Corner Rectangle"),
        (
            Tool::CenterRectangle,
            Icon::CenterRectangle,
            "Center Rectangle",
        ),
        (Tool::Slot, Icon::Slot, "Straight Slot"),
        (Tool::Circle, Icon::Circle, "Circle"),
        (Tool::Arc, Icon::Arc, "Centerpoint Arc"),
        (Tool::ThreePointArc, Icon::ThreePointArc, "3 Point Arc"),
        (Tool::TangentArc, Icon::TangentArc, "Tangent Arc"),
        (Tool::Polygon, Icon::Polygon, "Polygon"),
        (Tool::Point, Icon::Point, "Point"),
        (Tool::Trim, Icon::Trim, "Trim"),
        (Tool::Extend, Icon::Extend, "Extend"),
        (Tool::Fillet, Icon::SketchFillet, "Sketch Fillet"),
        (Tool::Offset, Icon::Offset, "Offset Entities"),
        (Tool::Convert, Icon::Convert, "Convert Entities"),
        (Tool::Mirror, Icon::Mirror, "Mirror Entities"),
    ];

    /// Tools that draw new geometry.
    pub fn draws(self) -> bool {
        matches!(
            self,
            Tool::Line
                | Tool::Centerline
                | Tool::Rectangle
                | Tool::CenterRectangle
                | Tool::Slot
                | Tool::Circle
                | Tool::Arc
                | Tool::ThreePointArc
                | Tool::TangentArc
                | Tool::Polygon
                | Tool::Point
        )
    }

    fn hint(self) -> &'static str {
        match self {
            Tool::Select => "Click to select, drag to move, right-click for relations",
            Tool::Dimension => {
                "Click one entity (then empty space) or two entities to dimension them"
            }
            Tool::Line | Tool::Centerline => {
                "Click points; click an existing point to close the contour"
            }
            Tool::Rectangle => "Click two opposite corners",
            Tool::CenterRectangle => "Click the centre (e.g. the origin), then a corner",
            Tool::Circle => "Click the centre, then a point on the circle",
            Tool::Slot => "Click the centre of one end, then of the other, then the width",
            Tool::ThreePointArc => "Click the start, the end, then a point the arc goes through",
            Tool::TangentArc => "Click the end of a line, then where the arc ends",
            Tool::Polygon => {
                "Click the centre, then a corner (the number of sides is in the tool window)"
            }
            Tool::Point => "Click to place a point; it keeps what it snapped to",
            Tool::Arc => "Click the centre, the start, then the end; the arc goes the way you turn",
            Tool::Trim => "Click the piece of a curve to remove, up to the nearest crossings",
            Tool::Extend => "Click a line or an arc near the end to lengthen up to the next curve",
            Tool::Fillet => {
                "Click the corner point where two lines meet, or the two lines one after the other"
            }
            Tool::Offset => {
                "Click a curve to select its chain (Ctrl+click adds), then click on the side to offset to"
            }
            Tool::Convert => {
                "Click an edge of the part lying in the sketch plane to copy it into the sketch; Ctrl+click copies the whole outline it is part of"
            }
            Tool::Mirror => {
                "Select what to mirror (Ctrl+click), then click the line or centerline to mirror about"
            }
        }
    }
}

/// What the cursor snapped to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Snap {
    Point(EntityId),
    /// The middle of a line.
    Midpoint(EntityId),
    /// Where two curves cross.
    Crossing(EntityId, EntityId),
    /// Somewhere on a curve.
    OnCurve(EntityId),
    /// An end point, middle or centre of a part edge: the new point is fixed there.
    Model,
}

/// A point placed by the current tool, created only when the shape is finished.
#[derive(Debug, Clone, Copy)]
struct Pending {
    at: Point2,
    snap: Option<Snap>,
}

#[derive(Debug, Clone, Copy)]
enum DragKind {
    Point(EntityId),
    Line(EntityId, EntityId),
    /// Centre, start and end: the whole arc moves.
    Arc(EntityId, EntityId, EntityId),
    Radius(EntityId),
}

#[derive(Debug, Clone, Copy)]
struct Drag {
    kind: DragKind,
    last: Point2,
}

/// What the viewport asks of the application.
pub enum SketchRequest {
    /// The user clicked a dimension label: edit that relation's value.
    EditDimension(RelationId),
    /// The Smart Dimension tool picked what to dimension: ask for the value.
    NewDimension(DimensionOption),
}

/// A dimension offered for the current selection.
pub struct DimensionOption {
    pub label: &'static str,
    /// The relation, holding the current measurement as its value.
    pub relation: Constraint,
    /// Value to show the user (degrees for angles).
    pub shown: f64,
    pub angle: bool,
}

pub struct SketchSession {
    pub feature: FeatureId,
    pub name: String,
    pub plane: Plane,
    pub sketch: ConstrainedSketch,
    /// The sketch as it was when editing started.
    pub original: ConstrainedSketch,
    /// It had no geometry (besides the origin) when editing started.
    pub original_empty: bool,
    /// The part's edges in the sketch plane, to snap to and convert.
    pub model: ModelGeometry,
    pub report: Option<SolveReport>,
    pub tool: Tool,
    pub selection: Vec<EntityId>,
    /// Dimensions selected by clicking their label (Delete removes them).
    pub selected_dimensions: Vec<RelationId>,
    /// Created by the "Sketch" command (as opposed to editing an existing sketch).
    pub new_sketch: bool,
    /// Whether the document had unsaved changes before a new sketch was started: what it
    /// goes back to when the sketch is left with nothing drawn.
    pub unsaved_before: bool,
    /// Where the rollback bar was before editing started.
    pub restore_rollback: Option<FeatureId>,
    pub message: Option<String>,
    /// Dimensions set by equations: shown with Σ, not editable here.
    pub driven: HashSet<RelationId>,
    pending: Vec<Pending>,
    hover: Option<EntityId>,
    /// Part edge under the cursor (Convert Entities), by index in `model.curves`.
    model_hover: Option<usize>,
    drag: Option<Drag>,
    labels: Vec<(Rect, RelationId)>,
    history: Vec<ConstrainedSketch>,
    /// What Undo took back, for Redo; any new change forgets it.
    undone: Vec<ConstrainedSketch>,
    /// `undone` while a change is tried, given back if the change is refused.
    redo_kept: Vec<ConstrainedSketch>,
    /// The radius and distance typed in the Sketch Fillet and Offset Entities windows.
    pub fillet_text: String,
    pub offset_text: String,
    /// The number of sides of the next polygon.
    pub polygon_sides: u32,
    /// While a centerpoint arc's end is placed: the pointer's last angle about its centre,
    /// and how far it has turned since the start (negative: clockwise).
    arc_turn: Option<(f64, f64)>,
    /// The last fillet arc and its radius: the next fillet of the same radius is made
    /// equal to it instead of getting a dimension of its own.
    last_fillet: Option<(EntityId, f64)>,
    /// What the tool would make at the cursor, drawn dashed.
    preview: Vec<Vec<Point2>>,
    /// The curves a dimension value being typed would move, where it would move them,
    /// drawn dashed.
    value_preview: Vec<Vec<Point2>>,
}

impl SketchSession {
    pub fn new(
        feature: FeatureId,
        name: String,
        plane: Plane,
        sketch: ConstrainedSketch,
        new_sketch: bool,
        restore_rollback: Option<FeatureId>,
    ) -> Self {
        let original_empty = sketch
            .entities()
            .iter()
            .all(|e| Some(e.id) == sketch.origin());
        // The origin point sits where the model origin falls in the sketch plane.
        let mut sketch = sketch;
        let v = oxyde_geom::Point3::ORIGIN - plane.origin;
        sketch.ensure_origin(Point2::new(v.dot(plane.x_dir), v.dot(plane.y_dir())));
        let mut session = Self {
            feature,
            name,
            plane,
            original: sketch.clone(),
            original_empty,
            model: ModelGeometry::default(),
            sketch,
            report: None,
            tool: if new_sketch { Tool::Line } else { Tool::Select },
            selection: Vec::new(),
            selected_dimensions: Vec::new(),
            new_sketch,
            unsaved_before: false,
            restore_rollback,
            message: None,
            driven: HashSet::new(),
            pending: Vec::new(),
            hover: None,
            model_hover: None,
            drag: None,
            labels: Vec::new(),
            history: Vec::new(),
            undone: Vec::new(),
            redo_kept: Vec::new(),
            fillet_text: "5".to_owned(),
            offset_text: "10".to_owned(),
            polygon_sides: 6,
            arc_turn: None,
            last_fillet: None,
            preview: Vec::new(),
            value_preview: Vec::new(),
        };
        session.solve();
        session
    }

    pub fn changed(&self) -> bool {
        self.sketch != self.original
    }

    /// Solves the working sketch. On failure the geometry is left as it was.
    fn solve(&mut self) -> bool {
        match self.sketch.solve() {
            Ok(report) => {
                let converged = report.converged;
                self.report = Some(report);
                converged
            }
            Err(e) => {
                self.message = Some(e.to_string());
                false
            }
        }
    }

    /// Whether the last solve found this relation implied by the others.
    fn is_redundant(&self, id: RelationId) -> bool {
        self.report
            .as_ref()
            .and_then(|r| r.state(id))
            .is_some_and(|s| s == ConstraintState::Redundant)
    }

    /// Records the sketch before a change, for Undo. What Undo took back is set aside: a
    /// change made forgets it, a change refused (`drop_checkpoint`) gives it back.
    fn checkpoint(&mut self) {
        self.history.push(self.sketch.clone());
        if self.history.len() > UNDO_LIMIT {
            self.history.remove(0);
        }
        self.redo_kept = std::mem::take(&mut self.undone);
        self.message = None;
    }

    /// A change refused after its checkpoint: the checkpoint goes and Redo has what it had
    /// before (the caller puts the sketch back).
    fn drop_checkpoint(&mut self) {
        self.history.pop();
        self.undone = std::mem::take(&mut self.redo_kept);
    }

    /// A change refused: the sketch as it was at its checkpoint, which goes.
    fn refuse_change(&mut self) {
        if let Some(previous) = self.history.pop() {
            self.sketch = previous;
        }
        self.undone = std::mem::take(&mut self.redo_kept);
        self.selection.clear();
        self.pending.clear();
        self.solve();
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.history.pop() else {
            return false;
        };
        self.undone
            .push(std::mem::replace(&mut self.sketch, previous));
        self.selection.clear();
        self.pending.clear();
        self.solve();
        true
    }

    /// Puts back what the last Undo took away.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.undone.pop() else {
            return false;
        };
        self.history.push(std::mem::replace(&mut self.sketch, next));
        self.selection.clear();
        self.pending.clear();
        self.solve();
        true
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.pending.clear();
        self.arc_turn = None;
        self.preview.clear();
        self.last_fillet = None;
        // Mirror and Offset keep the selection: it is what gets mirrored or offset.
        if tool == Tool::Dimension
            || tool.draws()
            || matches!(tool, Tool::Trim | Tool::Extend | Tool::Fillet)
        {
            self.selection.clear();
        }
    }

    /// Mirror Entities: a click on a line mirrors the selection about it; other clicks (and
    /// Ctrl+clicks) build the selection.
    fn click_mirror(&mut self, hit: Option<EntityId>, additive: bool) {
        let is_line = |id: EntityId| {
            matches!(
                self.sketch.entity(id).map(|e| e.geometry),
                Some(Geometry::Line { .. })
            )
        };
        let others = self.selection.iter().any(|s| Some(*s) != hit);
        match hit {
            Some(line) if !additive && is_line(line) && others => {
                let entities: Vec<EntityId> = self
                    .selection
                    .iter()
                    .copied()
                    .filter(|s| *s != line)
                    .collect();
                self.checkpoint();
                match self.sketch.mirror(&entities, line) {
                    Some(_) if self.solve() => {
                        self.selection.clear();
                        self.message = Some(format!("Mirrored {} entities", entities.len()));
                    }
                    _ => {
                        self.refuse_change();
                        self.message =
                            Some("That mirror conflicts with the existing relations".to_owned());
                    }
                }
            }
            _ => self.click_select(hit, true),
        }
    }

    /// Escape: abandons the shape in progress, then leaves the tool. Returns false when
    /// there was nothing to cancel.
    pub fn cancel_operation(&mut self) -> bool {
        if !self.pending.is_empty() {
            self.pending.clear();
            self.arc_turn = None;
            true
        } else if self.tool != Tool::Select {
            self.tool = Tool::Select;
            self.selection.clear();
            true
        } else if !self.selection.is_empty() || !self.selected_dimensions.is_empty() {
            self.selection.clear();
            self.selected_dimensions.clear();
            true
        } else {
            false
        }
    }

    /// One line for the status bar.
    pub fn status(&self) -> String {
        if let Some(message) = &self.message {
            return message.clone();
        }
        let definition = match &self.report {
            None => String::new(),
            Some(r) if !r.converged => "Over defined: relations conflict".to_owned(),
            Some(r) if r.degrees_of_freedom == 0 => "Fully defined".to_owned(),
            Some(r) => format!(
                "Under defined ({} degrees of freedom)",
                r.degrees_of_freedom
            ),
        };
        format!("Editing {}: {definition}. {}", self.name, self.tool.hint())
    }

    // ---- Editing -----------------------------------------------------------------------------

    /// Adds relations that make one together (Collinear is parallel and on the line) as
    /// one step, or none of them when they conflict with the existing ones.
    pub fn add_relations(&mut self, relations: &[Constraint]) -> bool {
        self.checkpoint();
        let ids: Vec<RelationId> = relations
            .iter()
            .map(|r| self.sketch.constrain(*r))
            .collect();
        if self.solve() {
            true
        } else {
            for id in ids {
                self.sketch.remove_relation(id);
            }
            self.drop_checkpoint();
            self.solve();
            self.message = Some("That relation conflicts with the existing ones".to_owned());
            false
        }
    }

    /// Adds a dimension with the given value (mm, or radians for angles).
    pub fn add_dimension(&mut self, relation: Constraint, value: f64) -> bool {
        self.checkpoint();
        let previous = self.history.last().cloned().unwrap_or_default();
        let id = self.sketch.constrain(relation);
        self.sketch.set_dimension(id, value);
        if self.solve() {
            true
        } else {
            self.sketch = previous;
            self.drop_checkpoint();
            self.solve();
            self.message = Some("That dimension conflicts with the existing relations".to_owned());
            false
        }
    }

    /// Changes the value of an existing dimension.
    pub fn set_dimension(&mut self, id: RelationId, value: f64) -> bool {
        self.checkpoint();
        let previous = self.history.last().cloned().unwrap_or_default();
        if !self.sketch.set_dimension(id, value) {
            self.drop_checkpoint();
            return false;
        }
        if self.solve() {
            true
        } else {
            self.sketch = previous;
            self.drop_checkpoint();
            self.solve();
            self.message =
                Some("That value cannot be reached with the existing relations".to_owned());
            false
        }
    }

    /// Shows, dashed, where a new dimension of `value` (mm, or radians for angles) would
    /// move the geometry. Returns false when the relations cannot meet it.
    pub fn preview_new_dimension(&mut self, relation: Constraint, value: f64) -> bool {
        self.preview_change(|s| {
            let id = s.constrain(relation);
            s.set_dimension(id, value)
        })
    }

    /// Shows, dashed, where dimension `id` set to `value` would move the geometry. Returns
    /// false when the relations cannot meet it.
    pub fn preview_set_dimension(&mut self, id: RelationId, value: f64) -> bool {
        self.preview_change(|s| s.set_dimension(id, value))
    }

    /// The dimension value preview, as drawn.
    pub fn value_preview(&self) -> &[Vec<Point2>] {
        &self.value_preview
    }

    pub fn clear_value_preview(&mut self) {
        self.value_preview.clear();
    }

    /// Solves a copy of the sketch with `change` made, and keeps the curves it moves as
    /// the value preview.
    fn preview_change(&mut self, change: impl FnOnce(&mut ConstrainedSketch) -> bool) -> bool {
        let mut trial = self.sketch.clone();
        let met = change(&mut trial) && trial.solve().is_ok_and(|r| r.converged);
        self.value_preview = if met {
            moved_curves(&self.sketch, &trial)
        } else {
            Vec::new()
        };
        met
    }

    /// The copies Mirror Entities would make of the selection, with `line` under the
    /// cursor as the line to mirror about.
    fn mirror_preview_for(&self, line: EntityId) -> Vec<Vec<Point2>> {
        let is_line = matches!(
            self.sketch.entity(line).map(|e| e.geometry),
            Some(Geometry::Line { .. })
        );
        let entities: Vec<EntityId> = self
            .selection
            .iter()
            .copied()
            .filter(|s| *s != line)
            .collect();
        if !is_line || entities.is_empty() {
            return Vec::new();
        }
        let mut trial = self.sketch.clone();
        let Some(copies) = trial.mirror(&entities, line) else {
            return Vec::new();
        };
        // Where the symmetric relations put the copies (as made, if they cannot).
        let _ = trial.solve();
        copies
            .iter()
            .filter_map(|id| curve_path(&trial, trial.entity(*id)?.geometry))
            .collect()
    }

    /// Renames a dimension of this sketch ("D1" → "Width").
    pub fn rename_dimension(&mut self, id: RelationId, name: &str) -> bool {
        let current = self.sketch.relation(id).and_then(|r| r.name.clone());
        if current.as_deref() == Some(name) {
            return true;
        }
        self.checkpoint();
        match self.sketch.rename_dimension(id, name) {
            Ok(()) => true,
            Err(e) => {
                self.drop_checkpoint();
                self.message = Some(e.to_string());
                false
            }
        }
    }

    /// Full name of one of this sketch's dimensions, e.g. "D1@Sketch2".
    pub fn dimension_full_name(&self, id: RelationId) -> Option<String> {
        let name = self.sketch.relation(id)?.name.as_deref()?;
        Some(oxyde_model::full_name(name, &self.name))
    }

    /// Deletes the selected geometry and dimensions.
    pub fn delete_selection(&mut self) {
        if self.selection.is_empty() && self.selected_dimensions.is_empty() {
            return;
        }
        self.checkpoint();
        for id in std::mem::take(&mut self.selected_dimensions) {
            self.sketch.remove_relation(id);
        }
        let doomed = std::mem::take(&mut self.selection);
        self.sketch.remove(&doomed);
        self.solve();
    }

    /// Selects a dimension by its label (Ctrl adds to the selection).
    fn click_dimension_label(&mut self, id: RelationId, additive: bool) {
        self.selection.clear();
        if additive {
            match self.selected_dimensions.iter().position(|d| *d == id) {
                Some(i) => {
                    self.selected_dimensions.remove(i);
                }
                None => self.selected_dimensions.push(id),
            }
        } else {
            self.selected_dimensions = vec![id];
        }
    }

    pub fn toggle_construction(&mut self) {
        let curves: Vec<(EntityId, bool)> = self
            .selection
            .iter()
            .filter_map(|id| self.sketch.entity(*id))
            .filter(|e| !matches!(e.geometry, Geometry::Point(_)))
            .map(|e| (e.id, e.construction))
            .collect();
        if curves.is_empty() {
            self.message = Some("Select lines, circles or arcs first".to_owned());
            return;
        }
        self.checkpoint();
        for (id, construction) in curves {
            self.sketch.set_construction(id, !construction);
        }
    }

    pub fn has_curve_selected(&self) -> bool {
        self.selection.iter().any(|id| {
            self.sketch
                .entity(*id)
                .is_some_and(|e| !matches!(e.geometry, Geometry::Point(_)))
        })
    }

    /// Removes the piece of `curve` under `at`, back to the nearest crossings.
    fn trim(&mut self, curve: EntityId, at: Point2) {
        self.checkpoint();
        let previous = self.sketch.clone();
        self.sketch.trim(curve, at);
        if !self.solve() {
            self.sketch = previous;
            self.drop_checkpoint();
            self.solve();
            self.message = Some("Trimming there conflicts with the existing relations".to_owned());
        }
        self.selection.clear();
    }

    /// Lengthens `curve` at the end nearer to `at` up to the next curve it meets.
    fn extend(&mut self, curve: EntityId, at: Point2) {
        self.checkpoint();
        let previous = self.sketch.clone();
        if !self.sketch.extend(curve, at) {
            self.drop_checkpoint();
            self.message = Some(
                "Nothing to extend to: that end meets no curve, or another curve is joined to it"
                    .to_owned(),
            );
            return;
        }
        if !self.solve() {
            self.sketch = previous;
            self.drop_checkpoint();
            self.solve();
            self.message = Some("Extending there conflicts with the existing relations".to_owned());
        }
        self.selection.clear();
    }

    /// The radius typed in the Sketch Fillet window.
    fn fillet_radius(&self) -> Option<f64> {
        parse_length(&self.fillet_text)
    }

    /// The distance typed in the Offset Entities window.
    fn offset_distance(&self) -> Option<f64> {
        parse_length(&self.offset_text)
    }

    /// What Sketch Fillet works on under the cursor: a corner point where two lines meet,
    /// else a line.
    fn fillet_hit(&self, pos: Pos2, to_screen: &dyn Fn(Point2) -> Pos2) -> Option<EntityId> {
        if let Some(point) = self.nearest_point(pos, to_screen)
            && self.sketch.corner_lines(point).is_some()
        {
            return Some(point);
        }
        self.curve_hit(pos, to_screen).filter(|id| {
            matches!(
                self.sketch.entity(*id).map(|e| e.geometry),
                Some(Geometry::Line { .. })
            )
        })
    }

    /// The arc Sketch Fillet would make for the entity under the cursor.
    fn fillet_preview_for(&self, hit: EntityId) -> Vec<Vec<Point2>> {
        let Some(radius) = self.fillet_radius() else {
            return Vec::new();
        };
        let pair = match self.sketch.corner_lines(hit) {
            Some(pair) => Some(pair),
            None => self
                .selection
                .first()
                .copied()
                .filter(|first| self.sketch.shared_corner(*first, hit).is_some())
                .map(|first| (first, hit)),
        };
        pair.and_then(|(a, b)| self.sketch.fillet_preview(a, b, radius))
            .into_iter()
            .collect()
    }

    /// Sketch Fillet: a click on a corner point rounds it; a click on a line, then on a
    /// line meeting it, rounds their corner.
    fn click_fillet(&mut self, hit: Option<EntityId>) {
        let Some(id) = hit else {
            self.selection.clear();
            return;
        };
        if let Some((a, b)) = self.sketch.corner_lines(id) {
            self.selection.clear();
            self.apply_fillet(a, b);
            return;
        }
        match self.selection.first().copied() {
            Some(first) if first != id && self.sketch.shared_corner(first, id).is_some() => {
                self.selection.clear();
                self.apply_fillet(first, id);
            }
            _ => self.selection = vec![id],
        }
    }

    fn apply_fillet(&mut self, first: EntityId, second: EntityId) {
        let Some(radius) = self.fillet_radius() else {
            self.message = Some("Type a radius in the Sketch Fillet window".to_owned());
            return;
        };
        self.checkpoint();
        let previous = self.sketch.clone();
        let Some(arc) = self.sketch.fillet(first, second, radius) else {
            self.drop_checkpoint();
            self.message = Some("No room there for a fillet of that radius".to_owned());
            return;
        };
        // The first fillet gets the radius as a dimension; the next ones of the same radius
        // are made equal to it, as SolidWorks does.
        match self.last_fillet {
            Some((earlier, r)) if r == radius && self.sketch.entity(earlier).is_some() => {
                self.sketch.constrain(Constraint::EqualRadius(earlier, arc));
            }
            _ => {
                self.sketch.constrain(Constraint::Radius(arc, radius));
                self.last_fillet = Some((arc, radius));
            }
        }
        if !self.solve() {
            self.sketch = previous;
            self.drop_checkpoint();
            self.solve();
            self.message = Some("That fillet conflicts with the existing relations".to_owned());
        }
    }

    /// Offset Entities: a click on a curve selects the curves joined to it (Ctrl+click adds
    /// or removes a chain); a click elsewhere makes the offset on that side of the selection.
    fn click_offset(&mut self, hit: Option<EntityId>, additive: bool, cursor: Option<Point2>) {
        match hit {
            Some(id) => {
                let chain = self.sketch.chain_of(id);
                if chain.is_empty() {
                    return;
                }
                if !additive {
                    self.selection = chain;
                } else if chain.iter().all(|c| self.selection.contains(c)) {
                    self.selection.retain(|s| !chain.contains(s));
                } else {
                    for c in chain {
                        if !self.selection.contains(&c) {
                            self.selection.push(c);
                        }
                    }
                }
            }
            None => {
                if let (false, Some(at)) = (self.selection.is_empty(), cursor) {
                    self.apply_offset(at);
                }
            }
        }
    }

    /// The signed distance that puts the offset of the selection on the cursor's side.
    fn offset_towards(&self, cursor: Point2) -> Option<f64> {
        let by = self.offset_distance()?;
        let nearest = |sign: f64| -> Option<f64> {
            self.sketch
                .offset_preview(&self.selection, sign * by)?
                .iter()
                .flat_map(|polyline| polyline.windows(2))
                .map(|w| plane_segment_distance(cursor, w[0], w[1]))
                .min_by(f64::total_cmp)
        };
        match (nearest(1.0), nearest(-1.0)) {
            (Some(left), Some(right)) => Some(if left <= right { by } else { -by }),
            (Some(_), None) => Some(by),
            (None, Some(_)) => Some(-by),
            (None, None) => None,
        }
    }

    fn apply_offset(&mut self, cursor: Point2) {
        let Some(by) = self.offset_towards(cursor) else {
            self.message = Some(if self.offset_distance().is_none() {
                "Type a distance in the Offset Entities window".to_owned()
            } else {
                "Those entities cannot be offset: select curves joined end to end".to_owned()
            });
            return;
        };
        self.checkpoint();
        let previous = self.sketch.clone();
        match self.sketch.offset(&self.selection, by) {
            Some(created) if self.solve() => {
                self.message = Some(format!("Offset {} entities", created.len()));
                self.selection.clear();
            }
            _ => {
                self.sketch = previous;
                self.drop_checkpoint();
                self.solve();
                self.message = Some("That offset conflicts with the existing relations".to_owned());
            }
        }
    }

    /// Selected points, lines, and circles/arcs.
    fn selected_kinds(&self) -> (Vec<EntityId>, Vec<EntityId>, Vec<EntityId>) {
        let (mut points, mut lines, mut rounds) = (Vec::new(), Vec::new(), Vec::new());
        for id in &self.selection {
            match self.sketch.entity(*id).map(|e| e.geometry) {
                Some(Geometry::Point(_)) => points.push(*id),
                Some(Geometry::Line { .. }) => lines.push(*id),
                Some(Geometry::Circle { .. } | Geometry::Arc { .. }) => rounds.push(*id),
                None => {}
            }
        }
        (points, lines, rounds)
    }

    /// Relations without a value that apply to the selection, each made of one relation
    /// or several (Collinear, Fix on a curve).
    pub fn relation_options(&self) -> Vec<(&'static str, Vec<Constraint>)> {
        use Constraint::*;
        let (p, l, r) = self.selected_kinds();
        let one = |label, c| (label, vec![c]);
        // Fixing a curve fixes the points it stands on, as SolidWorks does.
        let fix_points = |id: EntityId| -> Vec<Constraint> {
            let points = match self.sketch.entity(id).map(|e| e.geometry) {
                Some(Geometry::Line { start, end }) => vec![start, end],
                Some(Geometry::Arc { center, start, end }) => vec![center, start, end],
                _ => Vec::new(),
            };
            points
                .into_iter()
                .filter(|p| !self.sketch.is_fixed(*p))
                .map(Fixed)
                .collect()
        };
        match (p.as_slice(), l.as_slice(), r.as_slice()) {
            ([a], [], []) => vec![one("Fix", Fixed(*a))],
            ([a, b], [], []) => vec![
                one("Coincident", Coincident(*a, *b)),
                one("Horizontal", HorizontalPoints(*a, *b)),
                one("Vertical", VerticalPoints(*a, *b)),
            ],
            ([], [a], []) => {
                let mut out = vec![
                    one("Horizontal", Horizontal(*a)),
                    one("Vertical", Vertical(*a)),
                ];
                let fix = fix_points(*a);
                if !fix.is_empty() {
                    out.push(("Fix", fix));
                }
                out
            }
            ([], [a, b], []) => {
                let mut out = vec![
                    one("Parallel", Parallel(*a, *b)),
                    one("Perpendicular", Perpendicular(*a, *b)),
                    one("Equal", EqualLength(*a, *b)),
                ];
                // Collinear: parallel, and an end of the second (one it does not share
                // with the first) on the first.
                let ends = |line| self.sketch.endpoints(line);
                if let (Some((s0, s1)), Some((t0, t1))) = (ends(*a), ends(*b))
                    && let Some(free) = [t0, t1].into_iter().find(|t| *t != s0 && *t != s1)
                {
                    out.push(("Collinear", vec![Parallel(*a, *b), PointOnLine(free, *a)]));
                }
                out
            }
            ([], [], [a]) => {
                let fix = fix_points(*a);
                if fix.is_empty() {
                    Vec::new()
                } else {
                    vec![("Fix", fix)]
                }
            }
            ([], [], [a, b]) => {
                let mut out = vec![one("Equal", EqualRadius(*a, *b))];
                // Concentric: one centre, as SolidWorks ties the two centre points.
                if let (Some(x), Some(y)) = (self.center_of(*a), self.center_of(*b))
                    && x != y
                {
                    out.push(one("Concentric", Coincident(x, y)));
                }
                out
            }
            ([], [a], [b]) => vec![one("Tangent", Tangent(*a, *b))],
            ([a], [b], []) => vec![
                one("On line", PointOnLine(*a, *b)),
                one("Midpoint", Midpoint(*a, *b)),
            ],
            ([a], [], [b]) => vec![one("On circle", PointOnCircle(*a, *b))],
            ([a, b], [c], []) => vec![one("Symmetric", Symmetric(*a, *b, *c))],
            _ => Vec::new(),
        }
    }

    /// The centre point of a circle or arc.
    fn center_of(&self, round: EntityId) -> Option<EntityId> {
        match self.sketch.entity(round)?.geometry {
            Geometry::Circle { center, .. } | Geometry::Arc { center, .. } => Some(center),
            _ => None,
        }
    }

    /// Dimensions that apply to the selection, with their current measurement. For two
    /// entities, the first option is the one Smart Dimension picks.
    pub fn dimension_options(&self) -> Vec<DimensionOption> {
        dimension_options(&self.sketch, &self.selection)
    }

    // ---- Picking -----------------------------------------------------------------------------

    fn nearest_point(&self, pos: Pos2, to_screen: &dyn Fn(Point2) -> Pos2) -> Option<EntityId> {
        self.sketch
            .entities()
            .iter()
            .filter_map(|e| match e.geometry {
                Geometry::Point(p) => Some((e.id, (to_screen(p) - pos).length())),
                _ => None,
            })
            .filter(|(_, d)| *d < SNAP)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }

    /// The curve under a screen position.
    fn curve_hit(&self, pos: Pos2, to_screen: &dyn Fn(Point2) -> Pos2) -> Option<EntityId> {
        let at = |id| self.sketch.position(id).unwrap_or_default();
        self.sketch
            .entities()
            .iter()
            .filter_map(|e| {
                let distance = match e.geometry {
                    Geometry::Point(_) => return None,
                    Geometry::Line { start, end } => {
                        segment_distance(pos, to_screen(at(start)), to_screen(at(end)))
                    }
                    Geometry::Circle { center, radius } => {
                        let c = at(center);
                        let rim = to_screen(Point2::new(c.x + radius, c.y));
                        round_distance(pos, to_screen(c), rim)
                    }
                    Geometry::Arc { center, start, end } => {
                        let (c, s, f) = (at(center), at(start), at(end));
                        let path: Vec<Pos2> =
                            arc_path(c, s, f).into_iter().map(to_screen).collect();
                        path.windows(2)
                            .map(|w| segment_distance(pos, w[0], w[1]))
                            .fold(f32::INFINITY, f32::min)
                    }
                };
                Some((e.id, distance))
            })
            .filter(|(_, d)| *d < PICK)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }

    /// The entity under a screen position: points first, then curves.
    fn hit(&self, pos: Pos2, to_screen: &dyn Fn(Point2) -> Pos2) -> Option<EntityId> {
        self.nearest_point(pos, to_screen)
            .or_else(|| self.curve_hit(pos, to_screen))
    }

    /// Where a click at `pos` lands while drawing: an existing point, the middle of a line,
    /// a crossing, a point on a curve, or just the cursor.
    fn snap(
        &self,
        pos: Pos2,
        at: Point2,
        to_screen: &dyn Fn(Point2) -> Pos2,
    ) -> (Point2, Option<Snap>) {
        if let Some(id) = self.nearest_point(pos, to_screen) {
            return (
                self.sketch.position(id).unwrap_or(at),
                Some(Snap::Point(id)),
            );
        }
        let near = |p: Point2| (to_screen(p) - pos).length();
        let on_model = self
            .model
            .points
            .iter()
            .map(|(p, _)| (*p, near(*p)))
            .filter(|(_, d)| *d < SNAP)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((p, _)) = on_model {
            return (p, Some(Snap::Model));
        }
        let middle = self
            .sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, Geometry::Line { .. }))
            .filter_map(|e| Some((e.id, self.sketch.midpoint_of(e.id)?)))
            .map(|(id, p)| (id, p, near(p)))
            .filter(|(_, _, d)| *d < SNAP)
            .min_by(|a, b| a.2.total_cmp(&b.2));
        if let Some((line, p, _)) = middle {
            return (p, Some(Snap::Midpoint(line)));
        }
        let crossing = self
            .sketch
            .intersections()
            .into_iter()
            .map(|(a, b, p)| (a, b, p, near(p)))
            .filter(|(.., d)| *d < SNAP)
            .min_by(|x, y| x.3.total_cmp(&y.3));
        if let Some((a, b, p, _)) = crossing {
            return (p, Some(Snap::Crossing(a, b)));
        }
        if let Some(curve) = self.curve_hit(pos, to_screen) {
            if let Some(p) = self.sketch.closest_point(curve, at) {
                return (p, Some(Snap::OnCurve(curve)));
            }
        }
        (at, None)
    }

    fn click_select(&mut self, hit: Option<EntityId>, additive: bool) {
        if !additive {
            self.selected_dimensions.clear();
        }
        match hit {
            Some(id) if additive => {
                if let Some(i) = self.selection.iter().position(|s| *s == id) {
                    self.selection.remove(i);
                } else {
                    self.selection.push(id);
                }
            }
            Some(id) => self.selection = vec![id],
            None if !additive => self.selection.clear(),
            None => {}
        }
    }

    /// Smart Dimension: the first click picks an entity; a second entity, or a click on
    /// empty space, gives the dimension to ask for.
    fn click_dimension(&mut self, hit: Option<EntityId>) -> Option<DimensionOption> {
        match (hit, self.selection.len()) {
            (Some(id), 0) => {
                self.selection.push(id);
                // A single circle needs no second click.
                let round = matches!(
                    self.sketch.entity(id).map(|e| e.geometry),
                    Some(Geometry::Circle { .. } | Geometry::Arc { .. })
                );
                if round {
                    let option = self.dimension_options().into_iter().next();
                    self.selection.clear();
                    return option;
                }
                None
            }
            (Some(id), _) => {
                let first = self.selection[0];
                self.selection = if id == first {
                    vec![id]
                } else {
                    vec![first, id]
                };
                let option = self.dimension_options().into_iter().next();
                self.selection.clear();
                option
            }
            (None, 1) => {
                let option = self.dimension_options().into_iter().next();
                self.selection.clear();
                option
            }
            (None, _) => {
                self.selection.clear();
                None
            }
        }
    }

    // ---- Dragging ----------------------------------------------------------------------------

    fn start_drag(&mut self, id: EntityId, at: Point2) {
        let kind = match self.sketch.entity(id).map(|e| e.geometry) {
            Some(Geometry::Point(_)) if !self.sketch.is_fixed(id) => DragKind::Point(id),
            Some(Geometry::Line { start, end }) => DragKind::Line(start, end),
            Some(Geometry::Arc { center, start, end }) => DragKind::Arc(center, start, end),
            Some(Geometry::Circle { .. }) => DragKind::Radius(id),
            _ => return,
        };
        self.checkpoint();
        self.drag = Some(Drag { kind, last: at });
    }

    fn drag_to(&mut self, to: Point2) {
        let Some(mut drag) = self.drag else {
            return;
        };
        let before = self.sketch.clone();
        // What moves with the cursor. Fixed points never do (a line from the origin turns
        // about it).
        let mut moved = Vec::new();
        match drag.kind {
            DragKind::Point(p) => {
                self.sketch.move_point(p, to);
                moved.push(p);
            }
            DragKind::Line(..) | DragKind::Arc(..) => {
                let points = match drag.kind {
                    DragKind::Line(a, b) => vec![a, b],
                    DragKind::Arc(c, s, e) => vec![c, s, e],
                    _ => Vec::new(),
                };
                let (dx, dy) = (to.x - drag.last.x, to.y - drag.last.y);
                for p in points {
                    if self.sketch.is_fixed(p) {
                        continue;
                    }
                    if let Some(q) = self.sketch.position(p) {
                        self.sketch.move_point(p, Point2::new(q.x + dx, q.y + dy));
                        moved.push(p);
                    }
                }
            }
            DragKind::Radius(circle) => {
                if let Some(c) = self.center_of(circle).and_then(|c| self.sketch.position(c)) {
                    self.sketch
                        .set_circle_radius(circle, c.distance(to).max(1e-3));
                }
            }
        }
        // The moved points stay under the cursor while the rest follows, when the relations
        // allow it; otherwise they go as near as the relations let them.
        if !moved.is_empty() {
            let _ = self.sketch.solve_holding(&moved);
        }
        match self.sketch.solve() {
            Ok(report) if report.converged => {
                self.report = Some(report);
                drag.last = to;
                self.drag = Some(drag);
            }
            // The relations do not allow that move: stay where we were.
            _ => self.sketch = before,
        }
    }

    // ---- Drawing tools -----------------------------------------------------------------------

    /// Ties a point to what it was snapped to.
    fn attach(&mut self, point: EntityId, snap: Option<Snap>) {
        let on = |sketch: &ConstrainedSketch, curve: EntityId| match sketch
            .entity(curve)
            .map(|e| e.geometry)
        {
            Some(Geometry::Line { .. }) => Constraint::PointOnLine(point, curve),
            _ => Constraint::PointOnCircle(point, curve),
        };
        let relations = match snap {
            None => Vec::new(),
            Some(Snap::Point(existing)) if existing != point => {
                vec![Constraint::Coincident(point, existing)]
            }
            Some(Snap::Point(_)) => Vec::new(),
            Some(Snap::Midpoint(line)) => vec![Constraint::Midpoint(point, line)],
            Some(Snap::Crossing(a, b)) => vec![on(&self.sketch, a), on(&self.sketch, b)],
            Some(Snap::OnCurve(curve)) => vec![on(&self.sketch, curve)],
            Some(Snap::Model) => vec![Constraint::Fixed(point)],
        };
        for relation in relations {
            self.sketch.constrain(relation);
        }
    }

    /// The point entity for a pending point: the existing one it snapped to, or a new one.
    fn realize(&mut self, p: Pending) -> EntityId {
        match p.snap {
            Some(Snap::Point(id)) => id,
            snap => {
                let id = self.sketch.point(p.at);
                self.attach(id, snap);
                id
            }
        }
    }

    /// Solves after adding a shape; if the snapped relations cannot all hold, drops them
    /// rather than the shape.
    fn solve_new_shape(&mut self, relations_before: usize) {
        if self.solve() {
            return;
        }
        let added: Vec<RelationId> = self.sketch.relations()[relations_before..]
            .iter()
            .map(|r| r.id)
            .collect();
        for id in added {
            self.sketch.remove_relation(id);
        }
        self.solve();
    }

    fn click_tool(&mut self, p: Pending) {
        let relations_before = self.sketch.relations().len();
        match self.tool {
            Tool::Select
            | Tool::Dimension
            | Tool::Trim
            | Tool::Extend
            | Tool::Fillet
            | Tool::Offset
            | Tool::Convert
            | Tool::Mirror => {}
            Tool::CenterRectangle => match self.pending.first().copied() {
                None => self.pending = vec![p],
                Some(center) => {
                    let (dx, dy) = ((p.at.x - center.at.x).abs(), (p.at.y - center.at.y).abs());
                    if dx > 1e-6 && dy > 1e-6 {
                        self.checkpoint();
                        let lines = self.sketch.rectangle(
                            Point2::new(center.at.x - dx, center.at.y - dy),
                            2.0 * dx,
                            2.0 * dy,
                        );
                        let corners: Vec<EntityId> = lines
                            .iter()
                            .filter_map(|l| self.sketch.endpoints(*l).map(|(a, _)| a))
                            .collect();
                        // Construction diagonals, crossing at the centre point.
                        let d1 = self.sketch.line_between(corners[0], corners[2]);
                        let d2 = self.sketch.line_between(corners[1], corners[3]);
                        self.sketch.set_construction(d1, true);
                        self.sketch.set_construction(d2, true);
                        let c = self.realize(center);
                        // With the sides horizontal and vertical, one diagonal is enough.
                        self.sketch.constrain(Constraint::Midpoint(c, d1));
                        let corner = corners.iter().copied().find(|q| {
                            self.sketch
                                .position(*q)
                                .is_some_and(|at| at.distance(p.at) < 1e-9)
                        });
                        if let Some(corner) = corner {
                            self.attach(corner, p.snap);
                        }
                        self.solve_new_shape(relations_before);
                    }
                    self.pending.clear();
                }
            },
            Tool::Line | Tool::Centerline => match self.pending.first().copied() {
                None => self.pending = vec![p],
                Some(start) => {
                    let same = (start.snap.is_some()
                        && matches!(start.snap, Some(Snap::Point(_)))
                        && start.snap == p.snap)
                        || start.at.distance(p.at) < 1e-9;
                    if same {
                        return;
                    }
                    self.checkpoint();
                    let a = self.realize(start);
                    let b = self.realize(p);
                    let line = self.sketch.line_between(a, b);
                    if self.tool == Tool::Centerline {
                        self.sketch.set_construction(line, true);
                    }
                    let auto = self.auto_relation(line, start.at, p.at);
                    if self.solve() {
                        // A line between two midpoints of a rectangle's sides is already
                        // horizontal or vertical: the inferred relation would only
                        // over-define the sketch, so it is not kept.
                        if let Some(id) = auto.filter(|&id| self.is_redundant(id)) {
                            self.sketch.remove_relation(id);
                            self.solve();
                        }
                    } else {
                        if let Some(id) = auto {
                            self.sketch.remove_relation(id);
                        }
                        self.solve_new_shape(relations_before);
                    }
                    // Landing on an existing point closes the contour and ends the chain.
                    let closes = matches!(p.snap, Some(Snap::Point(_)));
                    self.pending = if closes || self.tool == Tool::Centerline {
                        Vec::new()
                    } else {
                        vec![Pending {
                            at: p.at,
                            snap: Some(Snap::Point(b)),
                        }]
                    };
                }
            },
            Tool::Rectangle => match self.pending.first().copied() {
                None => self.pending = vec![p],
                Some(first) => {
                    let (x0, x1) = (first.at.x.min(p.at.x), first.at.x.max(p.at.x));
                    let (y0, y1) = (first.at.y.min(p.at.y), first.at.y.max(p.at.y));
                    if x1 - x0 > 1e-6 && y1 - y0 > 1e-6 {
                        self.checkpoint();
                        let lines = self.sketch.rectangle(Point2::new(x0, y0), x1 - x0, y1 - y0);
                        // The two clicked corners keep what they snapped to.
                        let corners: Vec<EntityId> = lines
                            .iter()
                            .filter_map(|l| self.sketch.endpoints(*l).map(|(a, _)| a))
                            .collect();
                        for clicked in [first, p] {
                            let corner = corners.iter().copied().find(|c| {
                                self.sketch
                                    .position(*c)
                                    .is_some_and(|q| q.distance(clicked.at) < 1e-9)
                            });
                            if let Some(corner) = corner {
                                self.attach(corner, clicked.snap);
                            }
                        }
                        self.solve_new_shape(relations_before);
                    }
                    self.pending.clear();
                }
            },
            Tool::Point => {
                // On an existing point there is one already.
                if !matches!(p.snap, Some(Snap::Point(_))) {
                    self.checkpoint();
                    self.realize(p);
                    self.solve_new_shape(relations_before);
                }
            }
            Tool::Slot => match *self.pending.as_slice() {
                [a, b] => {
                    if let Some(radius) = slot_radius(a.at, b.at, p.at) {
                        self.checkpoint();
                        self.slot(a, b, radius);
                        self.solve_new_shape(relations_before);
                    }
                    self.pending.clear();
                }
                // The two centres, apart.
                [a] if a.at.distance(p.at) <= 1e-6 => {}
                _ => self.pending.push(p),
            },
            Tool::ThreePointArc => match *self.pending.as_slice() {
                [start, end] => {
                    if let Some((center, ccw)) = arc_through(start.at, end.at, p.at) {
                        self.checkpoint();
                        let s = self.realize(start);
                        let e = self.realize(end);
                        let c = self.sketch.point(center);
                        // Arcs run counter-clockwise: from the end when the arc turns the
                        // other way.
                        if ccw {
                            self.sketch.arc_between(c, s, e);
                        } else {
                            self.sketch.arc_between(c, e, s);
                        }
                        self.solve_new_shape(relations_before);
                    }
                    self.pending.clear();
                }
                [start] if start.at.distance(p.at) <= 1e-6 => {}
                _ => self.pending.push(p),
            },
            Tool::TangentArc => match self.pending.first().copied() {
                None => match p.snap {
                    Some(Snap::Point(from)) if self.tangent_from(from).is_some() => {
                        self.pending = vec![p];
                    }
                    _ => {
                        self.message =
                            Some("A tangent arc starts on the end of a line: click one".to_owned());
                    }
                },
                Some(start) => {
                    let line = match start.snap {
                        Some(Snap::Point(from)) => self.tangent_from(from).map(|t| (from, t)),
                        _ => None,
                    };
                    let Some((from, (line, direction))) = line else {
                        self.pending.clear();
                        return;
                    };
                    match tangent_arc_center(start.at, direction, p.at) {
                        Some((center, ccw)) => {
                            self.checkpoint();
                            let end = self.realize(p);
                            let c = self.sketch.point(center);
                            let arc = if ccw {
                                self.sketch.arc_between(c, from, end)
                            } else {
                                self.sketch.arc_between(c, end, from)
                            };
                            self.sketch.constrain(Constraint::Tangent(line, arc));
                            self.solve_new_shape(relations_before);
                        }
                        None => {
                            self.message = Some(
                                "That end is straight ahead of the line: draw a line there"
                                    .to_owned(),
                            );
                        }
                    }
                    self.pending.clear();
                }
            },
            Tool::Polygon => match self.pending.first().copied() {
                None => self.pending = vec![p],
                Some(center) => {
                    if center.at.distance(p.at) > 1e-6 {
                        self.checkpoint();
                        self.polygon(center, p);
                        self.solve_new_shape(relations_before);
                    }
                    self.pending.clear();
                }
            },
            Tool::Circle => match self.pending.first().copied() {
                None => self.pending = vec![p],
                Some(center) => {
                    let radius = center.at.distance(p.at);
                    if radius > 1e-6 {
                        self.checkpoint();
                        let c = self.realize(center);
                        let circle = self.sketch.circle_around(c, radius);
                        // A circle through an existing point stays on it.
                        if let Some(Snap::Point(q)) = p.snap {
                            self.sketch.constrain(Constraint::PointOnCircle(q, circle));
                        }
                        self.solve_new_shape(relations_before);
                    }
                    self.pending.clear();
                }
            },
            Tool::Arc => {
                let [center, start] = *self.pending.as_slice() else {
                    if self
                        .pending
                        .first()
                        .is_none_or(|c| c.at.distance(p.at) > 1e-6)
                    {
                        self.pending.push(p);
                        // From the start, the way the pointer turns sets the arc's way.
                        if let [center, start] = *self.pending.as_slice() {
                            let angle = (start.at.y - center.at.y).atan2(start.at.x - center.at.x);
                            self.arc_turn = Some((angle, 0.0));
                        }
                    }
                    return;
                };
                // Arcs run counter-clockwise: one drawn clockwise runs from its end.
                let clockwise = self.arc_turn.take().is_some_and(|(_, turned)| turned < 0.0);
                let arc = |sketch: &mut ConstrainedSketch, c, s, e| {
                    if clockwise {
                        sketch.arc_between(c, e, s)
                    } else {
                        sketch.arc_between(c, s, e)
                    }
                };
                let radius = center.at.distance(start.at);
                let (dx, dy) = (p.at.x - center.at.x, p.at.y - center.at.y);
                let length = (dx * dx + dy * dy).sqrt();
                if radius > 1e-6 && length > 1e-9 {
                    self.checkpoint();
                    let end_at = Point2::new(
                        center.at.x + dx / length * radius,
                        center.at.y + dy / length * radius,
                    );
                    let c = self.realize(center);
                    let s = self.realize(start);
                    // The end joins the point it snapped to, as a line's end does, when
                    // the relations allow it (the arc's ends must stay at one distance
                    // from its centre); else it is a new point, tied to what it snapped
                    // to (the middle of a line, a curve…) where that is.
                    let before = self.sketch.clone();
                    let joined = match p.snap {
                        Some(Snap::Point(q)) if q != c && q != s => {
                            arc(&mut self.sketch, c, s, q);
                            self.solve()
                        }
                        _ => false,
                    };
                    if !joined {
                        self.sketch = before;
                        let (at, snap) = match p.snap {
                            Some(Snap::Point(_)) | None => (end_at, None),
                            other => (p.at, other),
                        };
                        let e = self.sketch.point(at);
                        self.attach(e, snap);
                        arc(&mut self.sketch, c, s, e);
                        self.solve_new_shape(relations_before);
                    }
                }
                self.pending.clear();
            }
        }
    }

    /// Follows the pointer round a centerpoint arc's centre while its end is placed: the
    /// way it turns from the start is the way the arc goes, as in SolidWorks.
    fn track_arc_turn(&mut self, cursor: Point2) {
        let (Tool::Arc, [center, _]) = (self.tool, self.pending.as_slice()) else {
            return;
        };
        let Some((last, turned)) = self.arc_turn else {
            return;
        };
        let angle = (cursor.y - center.at.y).atan2(cursor.x - center.at.x);
        let step = (angle - last + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU)
            - std::f64::consts::PI;
        self.arc_turn = Some((angle, turned + step));
    }

    /// The line a tangent arc starting at `point` continues, and the direction it leaves
    /// in: the one line ending there.
    fn tangent_from(&self, point: EntityId) -> Option<(EntityId, Point2)> {
        let mut lines = self
            .sketch
            .entities()
            .iter()
            .filter_map(|e| match e.geometry {
                Geometry::Line { start, end } if start == point || end == point => {
                    Some((e.id, if end == point { start } else { end }))
                }
                _ => None,
            });
        let (line, other) = lines.next()?;
        if lines.next().is_some() {
            return None;
        }
        let (p, q) = (self.sketch.position(point)?, self.sketch.position(other)?);
        let length = p.distance(q);
        (length > 1e-9).then(|| {
            (
                line,
                Point2::new((p.x - q.x) / length, (p.y - q.y) / length),
            )
        })
    }

    /// A straight slot from centre `a` to centre `b`, `radius` on each side of the line
    /// between them: two half circles joined by lines tangent to them, the radii equal, and
    /// a construction centerline between the centres (dimension it for the slot's length),
    /// as SolidWorks' centre-to-centre slot.
    fn slot(&mut self, a: Pending, b: Pending, radius: f64) {
        let [a_left, a_right, b_right, b_left] = slot_corners(a.at, b.at, radius);
        let c1 = self.realize(a);
        let c2 = self.realize(b);
        let (a_left, a_right) = (self.sketch.point(a_left), self.sketch.point(a_right));
        let (b_left, b_right) = (self.sketch.point(b_left), self.sketch.point(b_right));
        // Arcs run counter-clockwise: round a's end from its left side to its right, b's
        // from its right side to its left.
        let end_a = self.sketch.arc_between(c1, a_left, a_right);
        let end_b = self.sketch.arc_between(c2, b_right, b_left);
        let right = self.sketch.line_between(a_right, b_right);
        let left = self.sketch.line_between(b_left, a_left);
        for (line, arc) in [(right, end_a), (right, end_b), (left, end_b), (left, end_a)] {
            self.sketch.constrain(Constraint::Tangent(line, arc));
        }
        self.sketch.constrain(Constraint::EqualRadius(end_a, end_b));
        let axis = self.sketch.line_between(c1, c2);
        self.sketch.set_construction(axis, true);
        self.auto_relation(axis, a.at, b.at);
    }

    /// A regular polygon of `polygon_sides` sides around `center` with a corner at
    /// `corner`: its corners on a construction circle (dimension it for the size), its
    /// sides equal, as SolidWorks' inscribed polygon.
    fn polygon(&mut self, center: Pending, corner: Pending) {
        let points = polygon_corners(center.at, corner.at, self.polygon_sides);
        let centre = self.realize(center);
        let circle = self
            .sketch
            .circle_around(centre, center.at.distance(corner.at));
        self.sketch.set_construction(circle, true);
        let mut corners = Vec::with_capacity(points.len());
        for (k, at) in points.into_iter().enumerate() {
            corners.push(if k == 0 {
                self.realize(corner)
            } else {
                self.sketch.point(at)
            });
        }
        let next = corners.iter().cycle().skip(1);
        let sides: Vec<EntityId> = corners
            .iter()
            .zip(next)
            .map(|(a, b)| self.sketch.line_between(*a, *b))
            .collect();
        for corner in &corners {
            self.sketch
                .constrain(Constraint::PointOnCircle(*corner, circle));
        }
        for pair in sides.windows(2) {
            if let [a, b] = pair {
                self.sketch.constrain(Constraint::EqualLength(*a, *b));
            }
        }
    }

    /// The part edge under a screen position.
    fn model_hit(&self, pos: Pos2, to_screen: &dyn Fn(Point2) -> Pos2) -> Option<usize> {
        self.model
            .curves
            .iter()
            .enumerate()
            .map(|(i, curve)| {
                let path: Vec<Pos2> = model_path(curve).into_iter().map(to_screen).collect();
                let d = path
                    .windows(2)
                    .map(|w| segment_distance(pos, w[0], w[1]))
                    .fold(f32::INFINITY, f32::min);
                (i, d)
            })
            .filter(|(_, d)| *d < PICK)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// A point fixed at `at`: an existing one there, or a new one.
    fn fixed_point_at(&mut self, at: Point2) -> EntityId {
        let existing = self
            .sketch
            .entities()
            .iter()
            .find(|e| matches!(e.geometry, Geometry::Point(p) if p.distance(at) < 1e-9))
            .map(|e| e.id);
        match existing {
            Some(id) => {
                if !self.sketch.is_fixed(id) {
                    self.sketch.constrain(Constraint::Fixed(id));
                }
                id
            }
            None => {
                let id = self.sketch.point(at);
                self.sketch.constrain(Constraint::Fixed(id));
                id
            }
        }
    }

    /// Convert Entities with Ctrl: every part edge joined end to end with edge `index` (a
    /// face's outline), as one step.
    fn convert_chain(&mut self, index: usize) {
        let chain = self.model.chain_of(index);
        self.checkpoint();
        for i in &chain {
            self.add_converted(*i);
        }
        if self.solve() {
            self.message = Some(format!("Converted {} edges", chain.len()));
        } else {
            self.refuse_change();
            self.message =
                Some("Those edges cannot be added with the existing relations".to_owned());
        }
    }

    /// Convert Entities: copies a part edge into the sketch, fixed where it is. Edges
    /// converted one after the other share their end points.
    fn convert(&mut self, index: usize) {
        if self.model.curves.get(index).is_none() {
            return;
        }
        self.checkpoint();
        self.add_converted(index);
        if !self.solve() {
            self.refuse_change();
            self.message = Some("That edge cannot be added with the existing relations".to_owned());
        }
    }

    /// The copy of part edge `index`, fixed where it is, sharing end points with what is
    /// there already; not solved.
    fn add_converted(&mut self, index: usize) {
        let Some(curve) = self.model.curves.get(index).copied() else {
            return;
        };
        match curve {
            ModelCurve::Line(a, b) => {
                let (pa, pb) = (self.fixed_point_at(a), self.fixed_point_at(b));
                self.sketch.line_between(pa, pb);
            }
            ModelCurve::Circle(center, radius) => {
                let c = self.fixed_point_at(center);
                let circle = self.sketch.circle_around(c, radius);
                // A fixed point on the rim holds the radius without a dimension.
                let rim = self.fixed_point_at(Point2::new(center.x + radius, center.y));
                self.sketch.set_construction(rim, true);
                self.sketch
                    .constrain(Constraint::PointOnCircle(rim, circle));
            }
            ModelCurve::Arc { center, start, end } => {
                let (c, s, e) = (
                    self.fixed_point_at(center),
                    self.fixed_point_at(start),
                    self.fixed_point_at(end),
                );
                self.sketch.arc_between(c, s, e);
            }
        }
    }

    /// Adds Horizontal or Vertical to a nearly horizontal or vertical new line.
    fn auto_relation(&mut self, line: EntityId, a: Point2, b: Point2) -> Option<RelationId> {
        let (dx, dy) = ((b.x - a.x).abs(), (b.y - a.y).abs());
        if dy <= AUTO_SLOPE * dx {
            Some(self.sketch.constrain(Constraint::Horizontal(line)))
        } else if dx <= AUTO_SLOPE * dy {
            Some(self.sketch.constrain(Constraint::Vertical(line)))
        } else {
            None
        }
    }

    // ---- Drawing -----------------------------------------------------------------------------

    fn conflicting(&self) -> Vec<EntityId> {
        let Some(report) = &self.report else {
            return Vec::new();
        };
        report
            .relations
            .iter()
            .filter(|(_, state)| *state == ConstraintState::Conflicting)
            .filter_map(|(id, _)| self.sketch.relation(*id))
            .flat_map(|r| r.constraint.entities())
            .collect()
    }

    fn color(&self, id: EntityId, construction: bool, conflicting: &[EntityId]) -> Color32 {
        if self.selection.contains(&id) {
            ink_selected()
        } else if self.hover == Some(id) {
            ink_hovered()
        } else if conflicting.contains(&id) {
            ink_conflict()
        } else if construction {
            ink_construction()
        } else {
            match self.report.as_ref().and_then(|r| r.definition(id)) {
                Some(Definition::Fully) => ink_fully(),
                _ => ink_under(),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &mut self,
        painter: &egui::Painter,
        to_screen: &dyn Fn(Point2) -> Pos2,
        cursor: Option<Point2>,
        pointer: Option<Pos2>,
        snapped: Option<(Point2, Option<Snap>)>,
        trimmed: Option<Vec<Point2>>,
    ) {
        let conflicting = self.conflicting();
        let origin_id = self.sketch.origin();

        // Part edges: centre marks, and the edge Convert Entities would copy.
        for (p, kind) in &self.model.points {
            if *kind == ModelPointKind::Center {
                let c = to_screen(*p);
                let stroke = Stroke::new(1.0_f32, ink_model());
                painter.line_segment([c - Vec2::new(5.0, 0.0), c + Vec2::new(5.0, 0.0)], stroke);
                painter.line_segment([c - Vec2::new(0.0, 5.0), c + Vec2::new(0.0, 5.0)], stroke);
            }
        }
        if let Some(curve) = self.model_hover.and_then(|i| self.model.curves.get(i)) {
            let path: Vec<Pos2> = model_path(curve).into_iter().map(to_screen).collect();
            painter.add(Shape::line(path, Stroke::new(3.0_f32, ink_hovered())));
        }

        // Sketch origin: a point that can be picked, with its axes.
        let origin_at = origin_id
            .and_then(|id| self.sketch.position(id))
            .unwrap_or_default();
        let origin = to_screen(origin_at);
        for (axis, color) in [
            (Point2::new(1.0, 0.0), Color32::from_rgb(210, 50, 50)),
            (Point2::new(0.0, 1.0), Color32::from_rgb(40, 150, 60)),
        ] {
            let d = to_screen(Point2::new(origin_at.x + axis.x, origin_at.y + axis.y)) - origin;
            if d.length() > 1e-6 {
                painter.line_segment(
                    [origin, origin + d.normalized() * 18.0],
                    Stroke::new(1.5_f32, color),
                );
            }
        }
        if let Some(id) = origin_id {
            let color = if self.selection.contains(&id) {
                ink_selected()
            } else if self.hover == Some(id) {
                ink_hovered()
            } else {
                Color32::from_rgb(200, 60, 60)
            };
            painter.circle(origin, 3.5, color, Stroke::new(1.0_f32, Color32::WHITE));
        }

        for e in self.sketch.entities() {
            let color = self.color(e.id, e.construction, &conflicting);
            let width = if self.selection.contains(&e.id) || self.hover == Some(e.id) {
                2.5_f32
            } else {
                1.6_f32
            };
            let stroke = Stroke::new(width, color);
            let Some(path) = curve_path(&self.sketch, e.geometry) else {
                continue;
            };
            let path: Vec<Pos2> = path.into_iter().map(to_screen).collect();
            if e.construction {
                painter.extend(Shape::dashed_line(&path, stroke, 6.0, 4.0));
            } else {
                painter.add(Shape::line(path, stroke));
            }
        }

        for e in self.sketch.entities() {
            if Some(e.id) == origin_id {
                continue;
            }
            if let Geometry::Point(p) = e.geometry {
                let color = self.color(e.id, false, &conflicting);
                let size = if self.selection.contains(&e.id) || self.hover == Some(e.id) {
                    7.0
                } else {
                    5.0
                };
                painter.rect_filled(
                    Rect::from_center_size(to_screen(p), Vec2::splat(size)),
                    0.0,
                    color,
                );
            }
        }

        // Relation glyphs: by a line's middle, or by both points a relation lines up.
        for relation in self.sketch.relations() {
            let (at, glyph) = match relation.constraint {
                Constraint::Horizontal(l) => {
                    (self.sketch.midpoint_of(l).into_iter().collect(), "H")
                }
                Constraint::Vertical(l) => (self.sketch.midpoint_of(l).into_iter().collect(), "V"),
                Constraint::HorizontalPoints(a, b) => (
                    [a, b]
                        .iter()
                        .filter_map(|p| self.sketch.position(*p))
                        .collect(),
                    "H",
                ),
                Constraint::VerticalPoints(a, b) => (
                    [a, b]
                        .iter()
                        .filter_map(|p| self.sketch.position(*p))
                        .collect(),
                    "V",
                ),
                _ => continue,
            };
            let at: Vec<Point2> = at;
            for p in at {
                painter.text(
                    to_screen(p) + Vec2::new(5.0, -5.0),
                    Align2::LEFT_BOTTOM,
                    glyph,
                    FontId::proportional(10.0),
                    ink_preview(),
                );
            }
        }

        // Dimensions, clickable.
        self.labels.clear();
        let mut labels = Vec::new();
        let mut hovered_label = None;
        for relation in self.sketch.relations() {
            let Some(graphics) = self.dimension_graphics(&relation.constraint, to_screen) else {
                continue;
            };
            let stroke = Stroke::new(1.0_f32, ink_dimension());
            for segment in &graphics.lines {
                painter.line_segment(*segment, stroke);
            }
            for (tip, direction) in &graphics.arrows {
                arrow_head(painter, *tip, *direction, ink_dimension());
            }
            let text = if self.driven.contains(&relation.id) {
                format!("Σ {}", graphics.text)
            } else {
                graphics.text
            };
            let selected = self.selected_dimensions.contains(&relation.id);
            let ink = if selected {
                ink_selected()
            } else {
                ink_fully()
            };
            let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), ink);
            let rect = Rect::from_center_size(graphics.anchor, galley.size() + Vec2::new(8.0, 4.0));
            painter.rect_filled(rect, 3.0, label_background());
            let border = if selected {
                Stroke::new(2.0_f32, ink_selected())
            } else {
                Stroke::new(1.0_f32, label_border())
            };
            painter.rect_stroke(rect, 3.0, border);
            painter.galley(rect.center() - galley.size() / 2.0, galley, ink);
            if pointer.is_some_and(|p| rect.contains(p)) {
                hovered_label = Some((rect, relation.id));
            }
            labels.push((rect, relation.id));
        }
        self.labels = labels;
        // Hovering a dimension shows its full name, as used in equations.
        if let Some((rect, id)) = hovered_label {
            if let Some(name) = self.dimension_full_name(id) {
                painter.text(
                    rect.center_top() - Vec2::new(0.0, 3.0),
                    Align2::CENTER_BOTTOM,
                    name,
                    FontId::proportional(11.0),
                    ink_under(),
                );
            }
        }

        // The piece the Trim tool would remove.
        if let Some(piece) = trimmed {
            let path: Vec<Pos2> = piece.into_iter().map(to_screen).collect();
            painter.extend(Shape::dashed_line(
                &path,
                Stroke::new(2.5_f32, ink_trimmed()),
                5.0,
                3.0,
            ));
        }

        // Sketch Fillet's arc, Offset or Mirror Entities' copies, as they would be made, and
        // the curves a dimension value being typed would move.
        for polyline in self.preview.iter().chain(&self.value_preview) {
            let path: Vec<Pos2> = polyline.iter().map(|p| to_screen(*p)).collect();
            painter.extend(Shape::dashed_line(
                &path,
                Stroke::new(1.5_f32, ink_preview()),
                5.0,
                3.0,
            ));
        }

        // The shape in progress.
        let cursor = snapped.map(|(p, _)| p).or(cursor);
        let dashed = Stroke::new(1.2_f32, ink_preview());
        if let (Some(first), Some(cursor)) = (self.pending.first(), cursor) {
            match self.tool {
                Tool::Line | Tool::Centerline => {
                    painter.extend(Shape::dashed_line(
                        &[to_screen(first.at), to_screen(cursor)],
                        dashed,
                        5.0,
                        3.0,
                    ));
                }
                Tool::CenterRectangle => {
                    let (dx, dy) = (cursor.x - first.at.x, cursor.y - first.at.y);
                    let c = first.at;
                    let corners = [
                        Point2::new(c.x - dx, c.y - dy),
                        Point2::new(c.x + dx, c.y - dy),
                        Point2::new(c.x + dx, c.y + dy),
                        Point2::new(c.x - dx, c.y + dy),
                        Point2::new(c.x - dx, c.y - dy),
                    ];
                    let path: Vec<Pos2> = corners.into_iter().map(to_screen).collect();
                    painter.extend(Shape::dashed_line(&path, dashed, 5.0, 3.0));
                    painter.extend(Shape::dashed_line(
                        &[path[0], path[2]],
                        Stroke::new(1.0_f32, ink_construction()),
                        4.0,
                        3.0,
                    ));
                }
                Tool::Rectangle => {
                    let corners = [
                        first.at,
                        Point2::new(cursor.x, first.at.y),
                        cursor,
                        Point2::new(first.at.x, cursor.y),
                        first.at,
                    ];
                    let path: Vec<Pos2> = corners.into_iter().map(to_screen).collect();
                    painter.extend(Shape::dashed_line(&path, dashed, 5.0, 3.0));
                }
                Tool::Slot => {
                    let path: Vec<Point2> = match self.pending.get(1) {
                        None => vec![first.at, cursor],
                        Some(end) => slot_radius(first.at, end.at, cursor)
                            .map(|r| slot_outline(first.at, end.at, r))
                            .unwrap_or_else(|| vec![first.at, end.at]),
                    };
                    let path: Vec<Pos2> = path.into_iter().map(to_screen).collect();
                    painter.extend(Shape::dashed_line(&path, dashed, 5.0, 3.0));
                }
                Tool::ThreePointArc => {
                    let path: Vec<Point2> = match self.pending.get(1) {
                        None => vec![first.at, cursor],
                        Some(end) => match arc_through(first.at, end.at, cursor) {
                            Some((c, true)) => arc_path(c, first.at, end.at),
                            Some((c, false)) => arc_path(c, end.at, first.at),
                            None => vec![first.at, end.at],
                        },
                    };
                    let path: Vec<Pos2> = path.into_iter().map(to_screen).collect();
                    painter.extend(Shape::dashed_line(&path, dashed, 5.0, 3.0));
                }
                Tool::TangentArc => {
                    let direction = match first.snap {
                        Some(Snap::Point(from)) => self.tangent_from(from).map(|(_, d)| d),
                        _ => None,
                    };
                    let path: Vec<Point2> =
                        match direction.and_then(|d| tangent_arc_center(first.at, d, cursor)) {
                            Some((c, true)) => arc_path(c, first.at, cursor),
                            Some((c, false)) => arc_path(c, cursor, first.at),
                            None => vec![first.at, cursor],
                        };
                    let path: Vec<Pos2> = path.into_iter().map(to_screen).collect();
                    painter.extend(Shape::dashed_line(&path, dashed, 5.0, 3.0));
                }
                Tool::Polygon => {
                    let mut path = polygon_corners(first.at, cursor, self.polygon_sides);
                    if let Some(start) = path.first().copied() {
                        path.push(start);
                    }
                    let path: Vec<Pos2> = path.into_iter().map(to_screen).collect();
                    painter.extend(Shape::dashed_line(&path, dashed, 5.0, 3.0));
                }
                Tool::Circle => {
                    let path: Vec<Pos2> = arc_points(
                        first.at,
                        first.at.distance(cursor),
                        0.0,
                        std::f64::consts::TAU,
                        72,
                    )
                    .into_iter()
                    .map(to_screen)
                    .collect();
                    painter.extend(Shape::dashed_line(&path, dashed, 5.0, 3.0));
                }
                Tool::Arc => {
                    let path: Vec<Pos2> = match self.pending.get(1) {
                        None => vec![to_screen(first.at), to_screen(cursor)],
                        Some(start) => {
                            let c = first.at;
                            let a0 = (start.at.y - c.y).atan2(start.at.x - c.x);
                            let a1 = (cursor.y - c.y).atan2(cursor.x - c.x);
                            let ccw = (a1 - a0).rem_euclid(std::f64::consts::TAU);
                            let clockwise = self.arc_turn.is_some_and(|(_, t)| t < 0.0);
                            let sweep = if clockwise {
                                ccw - std::f64::consts::TAU
                            } else {
                                ccw
                            };
                            arc_points(c, c.distance(start.at), a0, sweep, 48)
                                .into_iter()
                                .map(to_screen)
                                .collect()
                        }
                    };
                    painter.extend(Shape::dashed_line(&path, dashed, 5.0, 3.0));
                }
                Tool::Select
                | Tool::Dimension
                | Tool::Point
                | Tool::Trim
                | Tool::Extend
                | Tool::Fillet
                | Tool::Offset
                | Tool::Convert
                | Tool::Mirror => {}
            }
        }

        // What the cursor snapped to.
        if let Some((p, Some(snap))) = snapped {
            draw_snap(painter, to_screen(p), snap);
        }
    }

    /// The lines, arrows and label of a dimension, in screen space.
    fn dimension_graphics(
        &self,
        c: &Constraint,
        to_screen: &dyn Fn(Point2) -> Pos2,
    ) -> Option<DimensionGraphics> {
        use Constraint::*;
        let s = &self.sketch;
        let at = |id| s.position(id);
        let middle = |a: Pos2, b: Pos2| a + (b - a) / 2.0;
        // A dimension line between two screen points, offset sideways by `offset`.
        let aligned = |a: Pos2, b: Pos2, offset: Vec2, text: String| {
            let (da, db) = (a + offset, b + offset);
            let along = db - da;
            let mut g = DimensionGraphics {
                anchor: middle(da, db) + offset.normalized() * 2.0,
                text,
                lines: Vec::new(),
                arrows: Vec::new(),
            };
            if offset.length() > 0.5 {
                g.lines.push([a, da + offset.normalized() * 3.0]);
                g.lines.push([b, db + offset.normalized() * 3.0]);
            }
            if along.length() > 1.0 {
                g.lines.push([da, db]);
                g.arrows.push((da, -along.normalized()));
                g.arrows.push((db, along.normalized()));
            }
            g
        };
        let side = |a: Pos2, b: Pos2| {
            let d = b - a;
            if d.length() > 1e-3 {
                Vec2::new(d.y, -d.x).normalized() * 18.0
            } else {
                Vec2::new(0.0, -18.0)
            }
        };
        match *c {
            Length(line, value) => {
                let (a, b) = s.endpoints(line)?;
                let (a, b) = (to_screen(at(a)?), to_screen(at(b)?));
                // Outside the shape: on the side away from the middle of the sketch.
                let points: Vec<Pos2> = s
                    .entities()
                    .iter()
                    .filter_map(|e| match e.geometry {
                        Geometry::Point(p) => Some(to_screen(p)),
                        _ => None,
                    })
                    .collect();
                let n = points.len().max(1) as f32;
                let centre = points
                    .iter()
                    .fold(Pos2::ZERO, |sum, p| sum + p.to_vec2() / n);
                let mut offset = side(a, b);
                let m = middle(a, b);
                if ((m + offset) - centre).length() < ((m - offset) - centre).length() {
                    offset = -offset;
                }
                Some(aligned(a, b, offset, format_length(value)))
            }
            Distance(p, q, value) => {
                let (a, b) = (to_screen(at(p)?), to_screen(at(q)?));
                Some(aligned(a, b, Vec2::ZERO, format_length(value)))
            }
            HorizontalDistance(p, q, value) | VerticalDistance(p, q, value) => {
                let (pa, pb) = (at(p)?, at(q)?);
                let horizontal = matches!(c, HorizontalDistance(..));
                // Measured along the sketch's x (or y) axis, drawn beyond the farther point.
                let (ea, eb) = if horizontal {
                    let y = pa.y.max(pb.y);
                    (Point2::new(pa.x, y), Point2::new(pb.x, y))
                } else {
                    let x = pa.x.max(pb.x);
                    (Point2::new(x, pa.y), Point2::new(x, pb.y))
                };
                let (a, b) = (to_screen(ea), to_screen(eb));
                let towards = if horizontal {
                    to_screen(Point2::new(ea.x, ea.y + 1.0)) - a
                } else {
                    to_screen(Point2::new(ea.x + 1.0, ea.y)) - a
                };
                let offset = if towards.length() > 1e-6 {
                    towards.normalized() * 18.0
                } else {
                    Vec2::new(0.0, -18.0)
                };
                let mut g = aligned(a, b, offset, format_length(value));
                g.lines.push([to_screen(pa), a]);
                g.lines.push([to_screen(pb), b]);
                Some(g)
            }
            PointLineDistance(p, line, value) => {
                let point = at(p)?;
                let (a, b) = s.endpoints(line)?;
                let foot = perpendicular_foot(point, at(a)?, at(b)?);
                Some(aligned(
                    to_screen(point),
                    to_screen(foot),
                    Vec2::ZERO,
                    format_length(value),
                ))
            }
            LineDistance(first, second, value) => {
                let m = s.midpoint_of(first)?;
                let (a, b) = s.endpoints(second)?;
                let foot = perpendicular_foot(m, at(a)?, at(b)?);
                let mut g = aligned(
                    to_screen(m),
                    to_screen(foot),
                    Vec2::ZERO,
                    format_length(value),
                );
                // Extend the second line to the foot if it falls beyond it.
                g.lines
                    .push([to_screen(s.closest_point(second, foot)?), to_screen(foot)]);
                Some(g)
            }
            Diameter(round, value) => {
                // Across the circle through its centre, at 45°, arrows at both ends.
                let center = at(self.center_of(round)?)?;
                let r = value / 2.0 * std::f64::consts::FRAC_1_SQRT_2;
                let (a, b) = (
                    to_screen(Point2::new(center.x - r, center.y - r)),
                    to_screen(Point2::new(center.x + r, center.y + r)),
                );
                let out = (b - a).normalized();
                Some(DimensionGraphics {
                    anchor: b + out * 24.0,
                    text: format!("Ø{}", format_length(value)),
                    lines: vec![[a, b + out * 12.0]],
                    arrows: vec![(a, -out), (b, out)],
                })
            }
            Radius(round, value) => {
                let center = at(self.center_of(round)?)?;
                let rim = Point2::new(
                    center.x + value * std::f64::consts::FRAC_1_SQRT_2,
                    center.y + value * std::f64::consts::FRAC_1_SQRT_2,
                );
                let (c, r) = (to_screen(center), to_screen(rim));
                let out = (r - c).normalized();
                Some(DimensionGraphics {
                    anchor: r + out * 22.0,
                    text: format!("R{}", format_length(value)),
                    lines: vec![[c, r + out * 12.0]],
                    arrows: vec![(r, out)],
                })
            }
            Angle(first, _, value) => {
                let m = s.midpoint_of(first)?;
                Some(DimensionGraphics {
                    anchor: to_screen(m) + Vec2::new(16.0, -16.0),
                    text: format!("{:.1}°", value.to_degrees().abs()),
                    lines: Vec::new(),
                    arrows: Vec::new(),
                })
            }
            _ => None,
        }
    }
}

/// How one dimension is drawn.
struct DimensionGraphics {
    anchor: Pos2,
    text: String,
    lines: Vec<[Pos2; 2]>,
    /// Arrow tips and the direction they point.
    arrows: Vec<(Pos2, Vec2)>,
}

/// Dimensions that apply to `selection` (points, lines, circles), with their current
/// measurement. For two entities, the first option is the one Smart Dimension picks.
fn dimension_options(s: &ConstrainedSketch, selection: &[EntityId]) -> Vec<DimensionOption> {
    use Constraint::*;
    let kind = |id: EntityId| s.entity(id).map(|e| e.geometry);
    let center = |id: EntityId| match kind(id)? {
        Geometry::Circle { center, .. } | Geometry::Arc { center, .. } => Some(center),
        _ => None,
    };
    let option = |label, relation, shown, angle| DimensionOption {
        label,
        relation,
        shown,
        angle,
    };
    let point_distance = |p: EntityId, q: EntityId| -> Vec<DimensionOption> {
        let (Some(a), Some(b)) = (s.position(p), s.position(q)) else {
            return Vec::new();
        };
        let (d, dx, dy) = (a.distance(b), (b.x - a.x).abs(), (b.y - a.y).abs());
        let aligned = option("Distance…", Distance(p, q, d), d, false);
        let horizontal = option(
            "Horizontal distance…",
            HorizontalDistance(p, q, dx),
            dx,
            false,
        );
        let vertical = option("Vertical distance…", VerticalDistance(p, q, dy), dy, false);
        // Smart Dimension: points lined up get the horizontal or vertical distance.
        if dy < 1e-9 * d.max(1.0) {
            vec![horizontal, aligned, vertical]
        } else if dx < 1e-9 * d.max(1.0) {
            vec![vertical, aligned, horizontal]
        } else {
            vec![aligned, horizontal, vertical]
        }
    };
    let point_line = |p: EntityId, l: EntityId| -> Vec<DimensionOption> {
        let (Some(point), Some((a, b))) = (s.position(p), s.endpoints(l)) else {
            return Vec::new();
        };
        let (Some(a), Some(b)) = (s.position(a), s.position(b)) else {
            return Vec::new();
        };
        let d = point.distance(perpendicular_foot(point, a, b));
        vec![option("Distance…", PointLineDistance(p, l, d), d, false)]
    };

    match selection {
        [one] => match kind(*one) {
            Some(Geometry::Line { .. }) => {
                let length = s.line_length(*one).unwrap_or(0.0);
                vec![option("Length…", Length(*one, length), length, false)]
            }
            // A circle is dimensioned by its diameter, an arc by its radius, as in
            // SolidWorks; the other is offered next.
            Some(geometry @ (Geometry::Circle { .. } | Geometry::Arc { .. })) => {
                let radius = s.radius_of(*one).unwrap_or(0.0);
                let by_radius = option("Radius…", Radius(*one, radius), radius, false);
                let by_diameter = option(
                    "Diameter…",
                    Diameter(*one, 2.0 * radius),
                    2.0 * radius,
                    false,
                );
                if matches!(geometry, Geometry::Circle { .. }) {
                    vec![by_diameter, by_radius]
                } else {
                    vec![by_radius, by_diameter]
                }
            }
            _ => Vec::new(),
        },
        [a, b] => match (kind(*a), kind(*b)) {
            (Some(Geometry::Point(_)), Some(Geometry::Point(_))) => point_distance(*a, *b),
            (Some(Geometry::Point(_)), Some(Geometry::Line { .. })) => point_line(*a, *b),
            (Some(Geometry::Line { .. }), Some(Geometry::Point(_))) => point_line(*b, *a),
            (Some(Geometry::Line { .. }), Some(Geometry::Line { .. })) => {
                let angle = s.angle_between(*a, *b).unwrap_or(0.0);
                let folded = angle.abs().min(std::f64::consts::PI - angle.abs());
                let angle_option = option(
                    "Angle…",
                    Angle(*a, *b, angle),
                    angle.to_degrees().abs(),
                    true,
                );
                if folded < PARALLEL {
                    let distance = s.midpoint_of(*a).and_then(|m| {
                        let (p, q) = s.endpoints(*b)?;
                        let foot = perpendicular_foot(m, s.position(p)?, s.position(q)?);
                        Some(m.distance(foot))
                    });
                    match distance {
                        Some(d) => vec![option("Distance…", LineDistance(*a, *b, d), d, false)],
                        None => vec![angle_option],
                    }
                } else {
                    vec![angle_option]
                }
            }
            // Circles and arcs are dimensioned from their centre.
            (Some(Geometry::Point(_)), Some(_)) => match center(*b) {
                Some(c) => point_distance(*a, c),
                None => Vec::new(),
            },
            (Some(_), Some(Geometry::Point(_))) => match center(*a) {
                Some(c) => point_distance(c, *b),
                None => Vec::new(),
            },
            (Some(Geometry::Line { .. }), Some(_)) => match center(*b) {
                Some(c) => point_line(c, *a),
                None => Vec::new(),
            },
            (Some(_), Some(Geometry::Line { .. })) => match center(*a) {
                Some(c) => point_line(c, *b),
                None => Vec::new(),
            },
            (Some(_), Some(_)) => match (center(*a), center(*b)) {
                (Some(p), Some(q)) => point_distance(p, q),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The centre of the arc from `start` to `end` through `through`, and whether it runs
/// counter-clockwise from `start` to `end`. `None` when the three points are in line.
fn arc_through(start: Point2, end: Point2, through: Point2) -> Option<(Point2, bool)> {
    let (a, b, c) = (start, end, through);
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    let scale = a.distance(b).max(a.distance(c)).max(1e-9);
    if d.abs() < 1e-9 * scale * scale {
        return None;
    }
    let (a2, b2, c2) = (
        a.x * a.x + a.y * a.y,
        b.x * b.x + b.y * b.y,
        c.x * c.x + c.y * c.y,
    );
    let center = Point2::new(
        (a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d,
        (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d,
    );
    let angle = |p: Point2| (p.y - center.y).atan2(p.x - center.x);
    let tau = std::f64::consts::TAU;
    let sweep = (angle(end) - angle(start)).rem_euclid(tau);
    let to_through = (angle(through) - angle(start)).rem_euclid(tau);
    Some((center, to_through < sweep))
}

/// The centre of the arc leaving `start` along the unit `direction` and ending at `end`,
/// and whether it turns counter-clockwise (left). `None` when `end` is straight ahead or
/// behind, which no arc tangent there reaches.
fn tangent_arc_center(start: Point2, direction: Point2, end: Point2) -> Option<(Point2, bool)> {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    // To the left of the direction.
    let (nx, ny) = (-direction.y, direction.x);
    let across = dx * nx + dy * ny;
    let length2 = dx * dx + dy * dy;
    if across.abs() < 1e-9 * length2.sqrt().max(1e-9) {
        return None;
    }
    let r = length2 / (2.0 * across);
    Some((Point2::new(start.x + nx * r, start.y + ny * r), r > 0.0))
}

/// The most sides a polygon gets.
const MAX_SIDES: u32 = 64;

/// The half width of a slot from centre `a` to centre `b` whose side goes through `at`:
/// its distance from the line between the centres. `None` on that line.
fn slot_radius(a: Point2, b: Point2, at: Point2) -> Option<f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length = dx.hypot(dy);
    if length < 1e-9 {
        return None;
    }
    let r = ((at.x - a.x) * dy - (at.y - a.y) * dx).abs() / length;
    (r > 1e-6).then_some(r)
}

/// The corners of a slot from `a` to `b`: by `a`, left then right of the way from `a` to
/// `b`; by `b`, right then left (going round it counter-clockwise).
fn slot_corners(a: Point2, b: Point2, radius: f64) -> [Point2; 4] {
    let length = (b.x - a.x).hypot(b.y - a.y).max(1e-12);
    let (nx, ny) = (
        -(b.y - a.y) / length * radius,
        (b.x - a.x) / length * radius,
    );
    let side = |c: Point2, s: f64| Point2::new(c.x + s * nx, c.y + s * ny);
    [side(a, 1.0), side(a, -1.0), side(b, -1.0), side(b, 1.0)]
}

/// A slot's outline, closed, for its preview.
fn slot_outline(a: Point2, b: Point2, radius: f64) -> Vec<Point2> {
    let [a_left, a_right, b_right, b_left] = slot_corners(a, b, radius);
    let mut path = arc_path(a, a_left, a_right);
    path.extend(arc_path(b, b_right, b_left));
    path.push(a_left);
    path
}

/// The corners of a regular polygon of `sides` sides (3 to `MAX_SIDES`) around `center`,
/// counter-clockwise from `corner`.
fn polygon_corners(center: Point2, corner: Point2, sides: u32) -> Vec<Point2> {
    let n = sides.clamp(3, MAX_SIDES);
    let radius = center.distance(corner);
    let start = (corner.y - center.y).atan2(corner.x - center.x);
    (0..n)
        .map(|k| {
            let a = start + std::f64::consts::TAU * f64::from(k) / f64::from(n);
            Point2::new(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
}

fn perpendicular_foot(p: Point2, a: Point2, b: Point2) -> Point2 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length2 = dx * dx + dy * dy;
    if length2 < 1e-24 {
        return a;
    }
    let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / length2;
    Point2::new(a.x + dx * t, a.y + dy * t)
}

/// Points along a part edge, for drawing and picking.
fn model_path(curve: &ModelCurve) -> Vec<Point2> {
    match *curve {
        ModelCurve::Line(a, b) => vec![a, b],
        ModelCurve::Circle(c, r) => arc_points(c, r, 0.0, std::f64::consts::TAU, 72),
        ModelCurve::Arc { center, start, end } => arc_path(center, start, end),
    }
}

fn arrow_head(painter: &egui::Painter, tip: Pos2, direction: Vec2, color: Color32) {
    let back = tip - direction * 7.0;
    let side = Vec2::new(-direction.y, direction.x) * 2.8;
    painter.add(Shape::convex_polygon(
        vec![tip, back + side, back - side],
        color,
        Stroke::NONE,
    ));
}

/// Marks what the cursor snapped to, as SolidWorks' inference glyphs do.
fn draw_snap(painter: &egui::Painter, at: Pos2, snap: Snap) {
    let stroke = Stroke::new(1.8_f32, ink_snapped());
    match snap {
        Snap::Point(_) => {
            painter.rect_stroke(Rect::from_center_size(at, Vec2::splat(11.0)), 0.0, stroke);
        }
        Snap::Midpoint(_) => {
            let r = 7.0;
            painter.add(Shape::closed_line(
                vec![
                    at + Vec2::new(0.0, -r),
                    at + Vec2::new(r * 0.87, r * 0.5),
                    at + Vec2::new(-r * 0.87, r * 0.5),
                ],
                stroke,
            ));
        }
        Snap::Crossing(..) => {
            let r = 6.0;
            painter.line_segment([at + Vec2::new(-r, -r), at + Vec2::new(r, r)], stroke);
            painter.line_segment([at + Vec2::new(-r, r), at + Vec2::new(r, -r)], stroke);
        }
        Snap::OnCurve(_) => {
            painter.circle_stroke(at, 5.0, stroke);
        }
        Snap::Model => {
            // A diamond: fixed on the part.
            let r = 6.0;
            painter.add(Shape::closed_line(
                vec![
                    at + Vec2::new(0.0, -r),
                    at + Vec2::new(r, 0.0),
                    at + Vec2::new(0.0, r),
                    at + Vec2::new(-r, 0.0),
                ],
                stroke,
            ));
        }
    }
}

/// The value of a dimension, as shown to the user: degrees for angles, without the
/// sign the model keeps for which way round the angle goes (see [`typed_value`]).
pub fn dimension_value(c: &Constraint) -> Option<(f64, bool)> {
    let value = c.value()?;
    if c.is_angle() {
        Some((value.to_degrees().abs(), true))
    } else {
        Some((value, false))
    }
}

/// A value typed for a dimension, in the model's units: radians for an angle, on the
/// same side as `current` (the value the dimension has now), since the angle is shown
/// unsigned; a negative number typed puts it on the other side.
pub fn typed_value(c: &Constraint, typed: f64) -> f64 {
    if !c.is_angle() {
        return typed;
    }
    let radians = typed.to_radians();
    if c.value().is_some_and(|current| current < 0.0) {
        -radians
    } else {
        radians
    }
}

fn format_length(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn arc_points(center: Point2, radius: f64, start: f64, sweep: f64, segments: usize) -> Vec<Point2> {
    (0..=segments)
        .map(|k| {
            let a = start + sweep * k as f64 / segments as f64;
            Point2::new(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
}

/// Points along a counter-clockwise arc from `start` to `end` around `center`.
fn arc_path(center: Point2, start: Point2, end: Point2) -> Vec<Point2> {
    let a0 = (start.y - center.y).atan2(start.x - center.x);
    let a1 = (end.y - center.y).atan2(end.x - center.x);
    let sweep = (a1 - a0).rem_euclid(std::f64::consts::TAU);
    arc_points(center, center.distance(start), a0, sweep, 48)
}

/// The polyline of a curve of `sketch`, in plane coordinates; `None` for a point.
fn curve_path(sketch: &ConstrainedSketch, geometry: Geometry) -> Option<Vec<Point2>> {
    let at = |id| sketch.position(id).unwrap_or_default();
    match geometry {
        Geometry::Point(_) => None,
        Geometry::Line { start, end } => Some(vec![at(start), at(end)]),
        Geometry::Circle { center, radius } => Some(arc_points(
            at(center),
            radius,
            0.0,
            std::f64::consts::TAU,
            72,
        )),
        Geometry::Arc { center, start, end } => Some(arc_path(at(center), at(start), at(end))),
    }
}

/// The curves of `after` that are not where they are in `before` (or not in it at all),
/// as polylines: what a change moves.
fn moved_curves(before: &ConstrainedSketch, after: &ConstrainedSketch) -> Vec<Vec<Point2>> {
    after
        .entities()
        .iter()
        .filter_map(|e| {
            let path = curve_path(after, e.geometry)?;
            let old = before
                .entity(e.id)
                .and_then(|old| curve_path(before, old.geometry));
            let same = old.is_some_and(|old| {
                old.len() == path.len() && old.iter().zip(&path).all(|(a, b)| a.distance(*b) < 1e-7)
            });
            (!same).then_some(path)
        })
        .collect()
}

pub(crate) fn segment_distance(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 {
        ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - (a + ab * t)).length()
}

fn round_distance(p: Pos2, center: Pos2, rim: Pos2) -> f32 {
    ((p - center).length() - (rim - center).length()).abs()
}

/// Handles the mouse in the viewport while a sketch is being edited, and draws the sketch.
pub fn interact(
    session: &mut SketchSession,
    viewport: &Viewport,
    ui: &egui::Ui,
    out: &ViewportOutput,
) -> Option<SketchRequest> {
    let plane = session.plane;
    let to_screen = |p: Point2| viewport.to_screen(plane.to_world(p), out);
    let to_plane = |pos: Pos2| viewport.to_plane(pos, out, &plane);
    let response = &out.response;
    let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
    let pointer = response
        .hover_pos()
        .or_else(|| response.interact_pointer_pos());
    let mut request = None;

    if session.tool == Tool::Select {
        if response.drag_started_by(egui::PointerButton::Primary) {
            // Where the button went down: egui reports the drag only once the pointer has
            // moved past its click distance, and a quick drag is by then off the point it
            // grabbed (and a dragged line would lag the cursor by that much).
            let press = ui
                .input(|i| i.pointer.press_origin())
                .or_else(|| response.interact_pointer_pos());
            if let Some(press) = press {
                let hit = session.hit(press, &to_screen);
                if let (Some(id), Some(at)) = (hit, to_plane(press)) {
                    session.start_drag(id, at);
                }
            }
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            if let Some(to) = pointer.and_then(to_plane) {
                session.drag_to(to);
            }
        }
        if response.drag_stopped() {
            session.drag = None;
        }
    }

    session.model_hover = match (session.tool, pointer) {
        (Tool::Convert, Some(p)) => session.model_hit(p, &to_screen),
        _ => None,
    };
    session.hover = match (session.drag, pointer, session.tool) {
        (_, _, Tool::Convert) => None,
        (None, Some(p), Tool::Trim | Tool::Extend | Tool::Offset) => {
            session.curve_hit(p, &to_screen)
        }
        (None, Some(p), Tool::Fillet) => session.fillet_hit(p, &to_screen),
        (None, Some(p), tool) if !tool.draws() => session.hit(p, &to_screen),
        _ => None,
    };

    // Drawing tools snap the cursor; Trim previews the piece it would remove.
    let cursor = pointer.and_then(to_plane);
    let snapped = match (session.tool.draws(), pointer, cursor) {
        (true, Some(pos), Some(at)) => Some(session.snap(pos, at, &to_screen)),
        _ => None,
    };
    let trimmed = match (session.tool, session.hover, cursor) {
        (Tool::Trim, Some(curve), Some(at)) => session.sketch.trim_preview(curve, at),
        _ => None,
    };

    if let Some(at) = snapped.map(|(p, _)| p).or(cursor) {
        session.track_arc_turn(at);
    }

    session.preview = match (session.tool, session.hover, cursor) {
        (Tool::Fillet, Some(id), _) => session.fillet_preview_for(id),
        (Tool::Extend, Some(id), Some(at)) => {
            session.sketch.extend_preview(id, at).into_iter().collect()
        }
        (Tool::Mirror, Some(id), _) if !additive => session.mirror_preview_for(id),
        (Tool::Offset, _, Some(at)) if !session.selection.is_empty() => session
            .offset_towards(at)
            .and_then(|by| session.sketch.offset_preview(&session.selection, by))
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    // Dimension labels: a click selects, a double click edits the value.
    let label_at = |pos: Pos2| {
        session
            .labels
            .iter()
            .find(|(r, _)| r.contains(pos))
            .map(|(_, id)| *id)
    };
    if response.double_clicked() {
        if let Some(id) = pointer.and_then(label_at) {
            if matches!(session.tool, Tool::Select | Tool::Dimension) {
                request = Some(SketchRequest::EditDimension(id));
            }
        }
    }
    if response.clicked() && request.is_none() {
        if let Some(pos) = pointer {
            let label = label_at(pos);
            match session.tool {
                Tool::Select | Tool::Dimension if label.is_some() => {
                    if let Some(id) = label {
                        session.click_dimension_label(id, additive);
                    }
                }
                Tool::Select => {
                    let hit = session.hit(pos, &to_screen);
                    session.click_select(hit, additive);
                }
                Tool::Dimension => {
                    let hit = session.hit(pos, &to_screen);
                    request = session
                        .click_dimension(hit)
                        .map(SketchRequest::NewDimension);
                }
                Tool::Trim => {
                    if let (Some(curve), Some(at)) = (session.hover, cursor) {
                        session.trim(curve, at);
                    }
                }
                Tool::Extend => {
                    if let (Some(curve), Some(at)) = (session.hover, cursor) {
                        session.extend(curve, at);
                    }
                }
                Tool::Convert => {
                    if let Some(index) = session.model_hover {
                        // Ctrl+click: the whole outline the edge is part of.
                        if additive {
                            session.convert_chain(index);
                        } else {
                            session.convert(index);
                        }
                    }
                }
                Tool::Mirror => {
                    let hit = session.hit(pos, &to_screen);
                    session.click_mirror(hit, additive);
                }
                Tool::Fillet => {
                    let hit = session.hover;
                    session.click_fillet(hit);
                }
                Tool::Offset => {
                    let hit = session.hover;
                    session.click_offset(hit, additive, cursor);
                }
                _ => {
                    if let Some((at, snap)) = snapped {
                        session.click_tool(Pending { at, snap });
                    }
                }
            }
        }
    }

    tool_window(session, ui.ctx(), out.rect);
    let painter = ui.painter_at(out.rect);
    session.draw(&painter, &to_screen, cursor, pointer, snapped, trimmed);
    request
}

/// The window of a tool with a value: the fillet radius or the offset distance.
fn tool_window(session: &mut SketchSession, ctx: &egui::Context, viewport: Rect) {
    if session.tool == Tool::Polygon {
        egui::Window::new("Polygon")
            .id(egui::Id::new("sketch_tool_window"))
            .fixed_pos(viewport.left_top() + Vec2::new(12.0, 12.0))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Sides");
                    ui.add(egui::DragValue::new(&mut session.polygon_sides).range(3..=MAX_SIDES));
                });
                ui.label(
                    egui::RichText::new("Click the centre, then a corner")
                        .weak()
                        .small(),
                );
            });
        return;
    }
    let (title, label, hint) = match session.tool {
        Tool::Fillet => (
            "Sketch Fillet",
            "Radius",
            "Click a corner point, or two lines meeting there",
        ),
        Tool::Offset => (
            "Offset Entities",
            "Distance",
            "Click a curve for its chain, then the side to offset to",
        ),
        _ => return,
    };
    let text = match session.tool {
        Tool::Fillet => &mut session.fillet_text,
        _ => &mut session.offset_text,
    };
    egui::Window::new(title)
        .id(egui::Id::new("sketch_tool_window"))
        .fixed_pos(viewport.left_top() + Vec2::new(12.0, 12.0))
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(label);
                ui.add(egui::TextEdit::singleline(text).desired_width(70.0));
                ui.label("mm");
            });
            if parse_length(text).is_none() {
                ui.label(
                    egui::RichText::new("Type a length")
                        .small()
                        .color(ui.visuals().error_fg_color),
                );
            }
            ui.label(egui::RichText::new(hint).weak().small());
        });
}

/// A positive length typed by the user, in mm: a number or arithmetic, with units
/// (`quantity`).
fn parse_length(text: &str) -> Option<f64> {
    crate::quantity::parse(text, crate::quantity::Quantity::Length).filter(|v| *v > 0.0)
}

/// Distance from a point to a segment, in the sketch plane.
fn plane_segment_distance(p: Point2, a: Point2, b: Point2) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length2 = dx * dx + dy * dy;
    let t = if length2 < 1e-24 {
        0.0
    } else {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / length2).clamp(0.0, 1.0)
    };
    p.distance(Point2::new(a.x + dx * t, a.y + dy * t))
}

#[cfg(test)]
mod tests {
    use oxyde_geom::{Plane, Point2};
    use oxyde_model::FeatureId;
    use oxyde_sketch::{ConstrainedSketch, Constraint, EntityId, Geometry};

    use super::{Pending, SketchSession, Snap, Tool, dimension_options};

    fn session(sketch: ConstrainedSketch) -> SketchSession {
        SketchSession::new(
            FeatureId::from_raw(1),
            "Sketch1".to_owned(),
            Plane::FRONT,
            sketch,
            false,
            None,
        )
    }

    fn has(s: &SketchSession, wanted: impl Fn(&Constraint) -> bool) -> bool {
        s.sketch.relations().iter().any(|r| wanted(&r.constraint))
    }

    #[test]
    fn smart_dimension_picks_the_dimension_for_each_pair() {
        let mut s = ConstrainedSketch::new();
        let a = s.line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0));
        let b = s.line(Point2::new(0.0, 5.0), Point2::new(10.0, 5.0));
        let c = s.line(Point2::new(0.0, 0.0), Point2::new(5.0, 5.0));
        let p = s.point(Point2::new(3.0, 8.0));
        let circle = s.circle(Point2::new(20.0, 0.0), 2.0);
        let other = s.circle(Point2::new(20.0, 10.0), 1.0);
        let arc = s.arc(
            Point2::new(40.0, 0.0),
            Point2::new(43.0, 0.0),
            Point2::new(40.0, 3.0),
        );

        let first = |sel: &[EntityId]| dimension_options(&s, sel).into_iter().next().unwrap();
        let d = first(&[a, b]);
        assert!(
            matches!(d.relation, Constraint::LineDistance(x, y, v) if x == a && y == b && (v - 5.0).abs() < 1e-9)
        );
        assert!(matches!(first(&[a, c]).relation, Constraint::Angle(..)));
        assert!(
            matches!(first(&[p, a]).relation, Constraint::PointLineDistance(q, l, v) if q == p && l == a && (v - 8.0).abs() < 1e-9)
        );
        assert!(matches!(
            first(&[a, p]).relation,
            Constraint::PointLineDistance(..)
        ));
        let centers = first(&[circle, other]);
        assert!(
            matches!(centers.relation, Constraint::VerticalDistance(..)),
            "lined-up centres"
        );
        assert!((centers.shown - 10.0).abs() < 1e-9);
        assert!(matches!(
            first(&[a, circle]).relation,
            Constraint::PointLineDistance(..)
        ));
        // A circle by its diameter, an arc by its radius.
        assert!(
            matches!(first(&[circle]).relation, Constraint::Diameter(c, d) if c == circle && (d - 4.0).abs() < 1e-9)
        );
        assert!(matches!(first(&[arc]).relation, Constraint::Radius(..)));
        assert!(matches!(first(&[a]).relation, Constraint::Length(..)));
    }

    #[test]
    fn smart_dimension_clicks() {
        let mut s = ConstrainedSketch::new();
        let a = s.line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0));
        let b = s.line(Point2::new(0.0, 5.0), Point2::new(10.0, 5.0));
        let circle = s.circle(Point2::new(20.0, 0.0), 2.0);
        let mut session = session(s);
        session.set_tool(Tool::Dimension);
        // A line, then empty space: its length.
        assert!(session.click_dimension(Some(a)).is_none());
        let length = session.click_dimension(None).unwrap();
        assert!(matches!(length.relation, Constraint::Length(..)));
        // Two lines: their distance.
        assert!(session.click_dimension(Some(a)).is_none());
        let distance = session.click_dimension(Some(b)).unwrap();
        assert!(matches!(distance.relation, Constraint::LineDistance(..)));
        // A circle at once: its diameter, which drives the circle.
        let diameter = session.click_dimension(Some(circle)).unwrap();
        assert!(matches!(diameter.relation, Constraint::Diameter(..)));
        assert!(session.selection.is_empty());
        assert!(session.add_dimension(diameter.relation, 10.0));
        let radius = session.sketch.radius_of(circle).unwrap();
        assert!((radius - 5.0).abs() < 1e-9, "{radius}");
    }

    #[test]
    fn drawing_keeps_what_points_snapped_to() {
        let mut s = ConstrainedSketch::new();
        let base = s.line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0));
        let up = s.line(Point2::new(4.0, -5.0), Point2::new(4.0, 5.0));
        let mut session = session(s);
        session.set_tool(Tool::Line);
        // From the middle of `base` to the crossing of the two lines.
        session.click_tool(Pending {
            at: Point2::new(5.0, 0.0),
            snap: Some(Snap::Midpoint(base)),
        });
        session.click_tool(Pending {
            at: Point2::new(4.0, 0.0),
            snap: Some(Snap::Crossing(base, up)),
        });
        assert!(has(
            &session,
            |c| matches!(c, Constraint::Midpoint(_, l) if *l == base)
        ));
        assert!(has(
            &session,
            |c| matches!(c, Constraint::PointOnLine(_, l) if *l == up)
        ));
        assert!(session.report.as_ref().unwrap().converged);
    }

    #[test]
    fn a_centerline_between_two_midpoints_keeps_no_implied_relation() {
        let mut s = ConstrainedSketch::new();
        let [bottom, right, top, left] = s.rectangle(Point2::new(-25.0, -15.0), 50.0, 30.0);
        let mut session = session(s);
        session.set_tool(Tool::Centerline);
        // Top middle to bottom middle: vertical already, so Vertical is not added.
        session.click_tool(Pending {
            at: Point2::new(0.0, 15.0),
            snap: Some(Snap::Midpoint(top)),
        });
        session.click_tool(Pending {
            at: Point2::new(0.0, -15.0),
            snap: Some(Snap::Midpoint(bottom)),
        });
        // Right middle to left middle, likewise horizontal.
        session.click_tool(Pending {
            at: Point2::new(25.0, 0.0),
            snap: Some(Snap::Midpoint(right)),
        });
        session.click_tool(Pending {
            at: Point2::new(-25.0, 0.0),
            snap: Some(Snap::Midpoint(left)),
        });
        let midpoints = session
            .sketch
            .relations()
            .iter()
            .filter(|r| matches!(r.constraint, Constraint::Midpoint(..)))
            .count();
        assert_eq!(midpoints, 4);
        assert!(!has(
            &session,
            |c| matches!(c, Constraint::Vertical(l) | Constraint::Horizontal(l) if *l != bottom && *l != right && *l != top && *l != left)
        ));
        let report = session.report.as_ref().unwrap();
        assert!(report.converged);
        assert_eq!(report.count(oxyde_sketch::ConstraintState::Redundant), 0);
        // A free-standing nearly vertical centerline still gets its relation.
        session.click_tool(Pending {
            at: Point2::new(40.0, 0.0),
            snap: None,
        });
        session.click_tool(Pending {
            at: Point2::new(40.1, 10.0),
            snap: None,
        });
        assert_eq!(
            session
                .sketch
                .relations()
                .iter()
                .filter(|r| matches!(r.constraint, Constraint::Vertical(..)))
                .count(),
            3
        );
    }

    #[test]
    fn centerlines_are_construction_and_rectangles_attach_their_corners() {
        let mut s = ConstrainedSketch::new();
        let corner = s.point(Point2::new(0.0, 0.0));
        let mut session = session(s);
        session.set_tool(Tool::Centerline);
        session.click_tool(Pending {
            at: Point2::new(0.0, -5.0),
            snap: None,
        });
        session.click_tool(Pending {
            at: Point2::new(0.0, 5.0),
            snap: None,
        });
        let centerline = session
            .sketch
            .entities()
            .iter()
            .find(|e| matches!(e.geometry, Geometry::Line { .. }))
            .unwrap();
        assert!(centerline.construction);

        session.set_tool(Tool::Rectangle);
        session.click_tool(Pending {
            at: Point2::new(0.0, 0.0),
            snap: Some(Snap::Point(corner)),
        });
        session.click_tool(Pending {
            at: Point2::new(6.0, 4.0),
            snap: None,
        });
        assert!(has(
            &session,
            |c| matches!(c, Constraint::Coincident(_, p) if *p == corner)
        ));
        assert!(session.report.as_ref().unwrap().converged);
    }

    #[test]
    fn the_origin_is_a_point_to_build_on() {
        let mut session = session(ConstrainedSketch::new());
        let origin = session.sketch.origin().expect("every sketch has an origin");
        assert!(!session.changed(), "adding the origin is not an edit");
        // A centre rectangle snapped to the origin is centred on it.
        session.set_tool(Tool::CenterRectangle);
        session.click_tool(Pending {
            at: Point2::ORIGIN,
            snap: Some(Snap::Point(origin)),
        });
        session.click_tool(Pending {
            at: Point2::new(10.0, 5.0),
            snap: None,
        });
        assert!(has(
            &session,
            |c| matches!(c, Constraint::Midpoint(p, _) if *p == origin)
        ));
        let report = session.report.as_ref().unwrap();
        assert!(report.converged);
        // Two dimensions define it fully: width and height.
        let lines: Vec<EntityId> = session
            .sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, Geometry::Line { .. }) && !e.construction)
            .map(|e| e.id)
            .collect();
        assert!(session.add_dimension(Constraint::Length(lines[0], 30.0), 30.0));
        assert!(session.add_dimension(Constraint::Length(lines[1], 12.0), 12.0));
        assert_eq!(session.report.as_ref().unwrap().degrees_of_freedom, 0);
        // It cannot be deleted.
        session.selection = vec![origin];
        session.delete_selection();
        assert_eq!(session.sketch.origin(), Some(origin));
    }

    #[test]
    fn dragging_keeps_fixed_points_and_follows_the_cursor() {
        let mut sketch = ConstrainedSketch::new();
        let origin = sketch.ensure_origin(Point2::ORIGIN);
        let tip = sketch.point(Point2::new(10.0, 0.0));
        let line = sketch.line_between(origin, tip);
        let mut turning = session(sketch);
        // Dragging the line turns it about the origin, which does not move.
        turning.start_drag(line, Point2::new(5.0, 0.0));
        turning.drag_to(Point2::new(5.0, 5.0));
        assert_eq!(turning.sketch.position(origin), Some(Point2::ORIGIN));
        assert_eq!(turning.sketch.position(tip), Some(Point2::new(10.0, 5.0)));

        // A rectangle's corner stays under the cursor; its neighbours keep it rectangular.
        let mut sketch = ConstrainedSketch::new();
        sketch.ensure_origin(Point2::ORIGIN);
        let [bottom, right, ..] = sketch.rectangle(Point2::new(1.0, 1.0), 10.0, 5.0);
        let corner = sketch.endpoints(right).unwrap().1;
        let mut stretching = session(sketch);
        stretching.start_drag(corner, Point2::new(11.0, 6.0));
        stretching.drag_to(Point2::new(14.0, 9.0));
        let at = stretching.sketch.position(corner).unwrap();
        assert!(at.distance(Point2::new(14.0, 9.0)) < 1e-6, "{at:?}");
        let (a, b) = stretching.sketch.endpoints(bottom).unwrap();
        let (a, b) = (
            stretching.sketch.position(a).unwrap(),
            stretching.sketch.position(b).unwrap(),
        );
        assert!(
            (a.y - b.y).abs() < 1e-6 && (b.x - 14.0).abs() < 1e-6,
            "{a:?} {b:?}"
        );
        assert_eq!(stretching.report.as_ref().unwrap().degrees_of_freedom, 4);
    }

    #[test]
    fn part_edges_can_be_snapped_to_and_converted() {
        use crate::model_refs::{ModelCurve, ModelGeometry, ModelPointKind};
        let mut session = session(ConstrainedSketch::new());
        session.model = ModelGeometry {
            points: vec![(Point2::new(20.0, 10.0), ModelPointKind::Center)],
            curves: vec![
                ModelCurve::Circle(Point2::new(20.0, 10.0), 4.0),
                ModelCurve::Line(Point2::new(0.0, 0.0), Point2::new(30.0, 0.0)),
            ],
        };
        // A circle centred on the hole: its centre is fixed there.
        session.set_tool(Tool::Circle);
        session.click_tool(Pending {
            at: Point2::new(20.0, 10.0),
            snap: Some(Snap::Model),
        });
        session.click_tool(Pending {
            at: Point2::new(22.0, 10.0),
            snap: None,
        });
        assert!(has(&session, |c| matches!(c, Constraint::Fixed(_))));
        // Convert the hole and the edge.
        session.convert(0);
        session.convert(1);
        let circles = session
            .sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, Geometry::Circle { .. }))
            .count();
        assert_eq!(circles, 2);
        let report = session.report.as_ref().unwrap();
        assert!(report.converged);
        // The converted circle and line cannot move; only the drawn circle's radius can.
        assert_eq!(report.degrees_of_freedom, 1);

        // Ctrl+click: a face's whole outline at once, its corners shared.
        let mut outline = super::SketchSession::new(
            FeatureId::from_raw(2),
            "Sketch2".to_owned(),
            Plane::FRONT,
            ConstrainedSketch::new(),
            false,
            None,
        );
        let corners = [
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            Point2::new(10.0, 5.0),
            Point2::new(0.0, 5.0),
        ];
        let mut curves: Vec<ModelCurve> = corners
            .iter()
            .zip(corners.iter().cycle().skip(1))
            .map(|(a, b)| ModelCurve::Line(*a, *b))
            .collect();
        curves.push(ModelCurve::Circle(Point2::new(30.0, 0.0), 2.0));
        outline.model = ModelGeometry {
            points: Vec::new(),
            curves,
        };
        outline.convert_chain(1);
        let lines = outline
            .sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, Geometry::Line { .. }))
            .count();
        assert_eq!(lines, 4, "the rectangle, not the circle apart from it");
        let profile = oxyde_geom::Profile::from_curves(&outline.sketch.profile_segments()).unwrap();
        assert_eq!(profile.regions.len(), 1, "a closed outline");
        assert!(outline.undo(), "one step");
        assert!(outline.sketch.profile_segments().is_empty());
    }

    #[test]
    fn mirror_tool_mirrors_the_selection_about_the_clicked_line() {
        let mut s = ConstrainedSketch::new();
        let axis = s.line(Point2::new(0.0, -5.0), Point2::new(0.0, 5.0));
        let circle = s.circle(Point2::new(4.0, 0.0), 1.0);
        let mut session = session(s);
        session.set_tool(Tool::Mirror);
        session.click_mirror(Some(circle), false);
        assert_eq!(session.selection, vec![circle]);
        session.click_mirror(Some(axis), false);
        assert!(session.selection.is_empty());
        assert!(has(
            &session,
            |c| matches!(c, Constraint::Symmetric(_, _, l) if *l == axis)
        ));
        let circles = session
            .sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, Geometry::Circle { .. }))
            .count();
        assert_eq!(circles, 2);
        assert!(session.report.as_ref().unwrap().converged);
    }

    #[test]
    fn sketch_fillet_dimensions_the_first_arc_and_makes_the_next_equal() {
        let mut s = ConstrainedSketch::new();
        let [bottom, right, top, left] = s.rectangle(Point2::ORIGIN, 20.0, 10.0);
        let mut session = session(s);
        session.set_tool(Tool::Fillet);
        session.fillet_text = "3".to_owned();
        let corner = session.sketch.shared_corner(right, top).unwrap();
        session.click_fillet(Some(corner));
        assert!(has(
            &session,
            |c| matches!(c, Constraint::Radius(_, r) if (*r - 3.0).abs() < 1e-9)
        ));
        // Two lines, one after the other.
        session.click_fillet(Some(left));
        assert_eq!(session.selection, vec![left]);
        session.click_fillet(Some(bottom));
        assert!(session.selection.is_empty());
        assert!(has(&session, |c| matches!(c, Constraint::EqualRadius(..))));
        let arcs = session
            .sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, Geometry::Arc { .. }))
            .count();
        assert_eq!(arcs, 2);
        let report = session.report.as_ref().unwrap();
        assert!(report.converged);
        assert_eq!(
            report.degrees_of_freedom, 4,
            "position and size; the fillets are tied"
        );
        // No room: refused, nothing changed.
        let before = session.sketch.clone();
        session.fillet_text = "50".to_owned();
        session.click_fillet(Some(session.sketch.shared_corner(bottom, right).unwrap()));
        assert_eq!(session.sketch, before);
        assert!(session.message.is_some());
    }

    #[test]
    fn offset_tool_selects_a_chain_and_offsets_towards_the_click() {
        let mut s = ConstrainedSketch::new();
        let [_, right, top, _] = s.rectangle(Point2::ORIGIN, 20.0, 10.0);
        let mut session = session(s);
        session.set_tool(Tool::Offset);
        session.offset_text = "2".to_owned();
        session.click_offset(Some(top), false, None);
        assert_eq!(session.selection.len(), 4);
        // Ctrl+click on a selected chain takes it out again, and back in.
        session.click_offset(Some(top), true, None);
        assert!(session.selection.is_empty());
        session.click_offset(Some(top), true, None);
        assert_eq!(session.selection.len(), 4);
        // A click inside: the copy goes inside.
        session.click_offset(None, false, Some(Point2::new(10.0, 5.0)));
        assert!(session.selection.is_empty());
        let lines = session
            .sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, Geometry::Line { .. }))
            .count();
        assert_eq!(lines, 8);
        assert!(session.sketch.entities().iter().any(
            |e| matches!(e.geometry, Geometry::Point(p) if p.distance(Point2::new(2.0, 2.0)) < 1e-9)
        ));
        assert!(has(&session, |c| matches!(c, Constraint::LineDistance(..))));
        assert!(session.report.as_ref().unwrap().converged);
        assert!(session.undo());
        let lines = session
            .sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, Geometry::Line { .. }))
            .count();
        assert_eq!(lines, 4);
        // Redo puts the offset back; a new change forgets what was undone.
        assert!(session.redo());
        assert_eq!(
            session
                .sketch
                .entities()
                .iter()
                .filter(|e| matches!(e.geometry, Geometry::Line { .. }))
                .count(),
            8
        );
        // A refused change keeps Redo; a change made forgets it.
        assert!(session.undo());
        session.fillet_text = "50".to_owned();
        let corner = session.sketch.shared_corner(right, top);
        session.click_fillet(corner);
        assert!(session.message.is_some(), "no room for that fillet");
        assert!(session.redo(), "Redo survives a refused change");
        assert!(session.undo());
        session.selection = vec![top];
        session.toggle_construction();
        assert!(!session.redo());
    }

    #[test]
    fn an_arc_ends_on_the_point_it_snapped_to() {
        let mut s = ConstrainedSketch::new();
        // A slot's straight sides; the arc closes one end onto the top line's end.
        let bottom = s.line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0));
        let top = s.line(Point2::new(0.0, 6.0), Point2::new(10.0, 6.0));
        let (_, bottom_end) = s.endpoints(bottom).unwrap();
        let (_, top_end) = s.endpoints(top).unwrap();
        let mut slot = session(s);
        slot.set_tool(Tool::Arc);
        slot.click_tool(Pending {
            at: Point2::new(10.0, 3.0),
            snap: None,
        });
        slot.click_tool(Pending {
            at: Point2::new(10.0, 0.0),
            snap: Some(Snap::Point(bottom_end)),
        });
        slot.click_tool(Pending {
            at: Point2::new(10.0, 6.0),
            snap: Some(Snap::Point(top_end)),
        });
        let arc = slot
            .sketch
            .entities()
            .iter()
            .find_map(|e| match e.geometry {
                Geometry::Arc { start, end, .. } => Some((start, end)),
                _ => None,
            })
            .expect("an arc was drawn");
        assert_eq!(arc, (bottom_end, top_end), "the arc shares the lines' ends");
        assert!(slot.report.as_ref().unwrap().converged);

        // Turned clockwise from the start: the arc goes that way, from its end.
        let mut s = ConstrainedSketch::new();
        s.point(Point2::new(50.0, 50.0));
        let mut clockwise = session(s);
        clockwise.set_tool(Tool::Arc);
        for at in [Point2::ORIGIN, Point2::new(5.0, 0.0)] {
            clockwise.click_tool(Pending { at, snap: None });
        }
        for (x, y) in [(4.0, -3.0), (0.0, -5.0)] {
            clockwise.track_arc_turn(Point2::new(x, y));
        }
        clockwise.click_tool(Pending {
            at: Point2::new(0.0, -5.0),
            snap: None,
        });
        let (start, end) = clockwise
            .sketch
            .entities()
            .iter()
            .find_map(|e| match e.geometry {
                Geometry::Arc { start, end, .. } => Some((start, end)),
                _ => None,
            })
            .unwrap();
        let at = |id| clockwise.sketch.position(id).unwrap();
        assert!(
            at(start).distance(Point2::new(0.0, -5.0)) < 1e-9,
            "a quarter, not three"
        );
        assert!(at(end).distance(Point2::new(5.0, 0.0)) < 1e-9);

        // Snapped to the middle of a line instead: a new point, kept there.
        let mut s = ConstrainedSketch::new();
        let side = s.line(Point2::new(5.0, -5.0), Point2::new(5.0, 5.0));
        let mut other = session(s);
        other.set_tool(Tool::Arc);
        for (at, snap) in [
            (Point2::ORIGIN, None),
            (Point2::new(0.0, 5.0), None),
            (Point2::new(5.0, 0.0), Some(Snap::Midpoint(side))),
        ] {
            other.click_tool(Pending { at, snap });
        }
        assert!(has(
            &other,
            |c| matches!(c, Constraint::Midpoint(_, l) if *l == side)
        ));
        assert!(other.report.as_ref().unwrap().converged);
    }

    #[test]
    fn angles_are_shown_and_typed_without_their_sign() {
        use super::{dimension_value, typed_value};
        let clockwise = Constraint::Angle(EntityId(1), EntityId(2), -0.5);
        assert_eq!(dimension_value(&clockwise).unwrap().0, 0.5f64.to_degrees());
        // Typed back, the angle stays on its side; a negative number flips it.
        assert!((typed_value(&clockwise, 30.0) + 30f64.to_radians()).abs() < 1e-12);
        assert!((typed_value(&clockwise, -30.0) - 30f64.to_radians()).abs() < 1e-12);
        let counter = Constraint::Angle(EntityId(1), EntityId(2), 0.5);
        assert!((typed_value(&counter, 30.0) - 30f64.to_radians()).abs() < 1e-12);
        // Lengths are what they are.
        assert_eq!(typed_value(&Constraint::Length(EntityId(1), 4.0), 7.0), 7.0);
    }

    #[test]
    fn mirror_shows_its_copies_before_the_click() {
        let mut s = ConstrainedSketch::new();
        let axis = s.line(Point2::new(0.0, -5.0), Point2::new(0.0, 5.0));
        let circle = s.circle(Point2::new(4.0, 0.0), 1.0);
        let mut session = session(s);
        session.set_tool(Tool::Mirror);
        session.click_mirror(Some(circle), false);
        let before = session.sketch.clone();
        let copies = session.mirror_preview_for(axis);
        assert_eq!(copies.len(), 1);
        // The circle on the other side of the line.
        let (lo, hi) = copies[0]
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                (lo.min(p.x), hi.max(p.x))
            });
        assert!(((lo + hi) / 2.0 + 4.0).abs() < 1e-6, "{lo}..{hi}");
        // Nothing is made until the click, and only a line is mirrored about.
        assert_eq!(session.sketch, before);
        assert!(session.mirror_preview_for(circle).is_empty());
    }

    #[test]
    fn a_dimension_value_is_shown_before_it_is_set() {
        let mut s = ConstrainedSketch::new();
        let line = s.line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0));
        let mut session = session(s);
        let before = session.sketch.clone();
        let length = |path: &[Point2]| path[0].distance(path[1]);

        assert!(session.preview_new_dimension(Constraint::Length(line, 10.0), 20.0));
        let moved = session.value_preview();
        assert_eq!(moved.len(), 1);
        assert!((length(&moved[0]) - 20.0).abs() < 1e-6);
        assert_eq!(session.sketch, before);
        // The value it already has moves nothing.
        assert!(session.preview_new_dimension(Constraint::Length(line, 10.0), 10.0));
        assert!(session.value_preview().is_empty());

        // An existing dimension, typed over.
        assert!(session.add_dimension(Constraint::Length(line, 10.0), 10.0));
        let id = session.sketch.relations().last().unwrap().id;
        assert!(session.preview_set_dimension(id, 4.0));
        assert!((length(&session.value_preview()[0]) - 4.0).abs() < 1e-6);

        // Both ends fixed: no other length can be met, and nothing is shown.
        let (a, b) = session.sketch.endpoints(line).unwrap();
        assert!(session.add_relations(&[Constraint::Fixed(a)]));
        assert!(session.add_relations(&[Constraint::Fixed(b)]));
        assert!(!session.preview_set_dimension(id, 4.0));
        assert!(session.value_preview().is_empty());
    }

    #[test]
    fn an_arc_is_dragged_as_a_whole() {
        let mut s = ConstrainedSketch::new();
        let arc = s.arc(
            Point2::new(10.0, 0.0),
            Point2::new(15.0, 0.0),
            Point2::new(10.0, 5.0),
        );
        let Geometry::Arc { center, start, end } = s.entity(arc).unwrap().geometry else {
            panic!("not an arc");
        };
        let mut moving = session(s);
        moving.start_drag(arc, Point2::new(13.5, 3.5));
        moving.drag_to(Point2::new(23.5, 1.5));
        assert_eq!(
            moving.sketch.position(center),
            Some(Point2::new(20.0, -2.0))
        );
        assert_eq!(moving.sketch.position(start), Some(Point2::new(25.0, -2.0)));
        assert_eq!(moving.sketch.position(end), Some(Point2::new(20.0, 3.0)));
        // Two circles offer Concentric: their centres made one.
        moving.sketch.circle(Point2::new(0.0, 0.0), 2.0);
        let circle = moving.sketch.circle(Point2::new(1.0, 1.0), 3.0);
        moving.selection = vec![arc, circle];
        let options = moving.relation_options();
        assert!(options.iter().any(|(label, _)| *label == "Concentric"));
    }

    #[test]
    fn points_line_up_lines_go_collinear_and_curves_are_fixed() {
        let mut s = ConstrainedSketch::new();
        let p = s.point(Point2::new(0.0, 0.0));
        let q = s.point(Point2::new(10.0, 3.0));
        let a = s.line(Point2::new(0.0, 10.0), Point2::new(10.0, 10.0));
        let b = s.line(Point2::new(15.0, 12.0), Point2::new(25.0, 13.0));
        let mut session = session(s);
        let apply = |session: &mut SketchSession, selection: Vec<EntityId>, label: &str| {
            session.selection = selection;
            let options = session.relation_options();
            let (_, relations) = options
                .into_iter()
                .find(|(l, _)| *l == label)
                .unwrap_or_else(|| panic!("{label} offered"));
            assert!(session.add_relations(&relations), "{label}");
        };
        apply(&mut session, vec![p, q], "Horizontal");
        let (pp, qq) = (
            session.sketch.position(p).unwrap(),
            session.sketch.position(q).unwrap(),
        );
        assert!((pp.y - qq.y).abs() < 1e-9);
        apply(&mut session, vec![a, b], "Collinear");
        let at = |id| session.sketch.position(id).unwrap();
        let (a0, a1) = session.sketch.endpoints(a).unwrap();
        let (b0, b1) = session.sketch.endpoints(b).unwrap();
        for end in [b0, b1] {
            // On the line through a's ends.
            let (s, e, p) = (at(a0), at(a1), at(end));
            let across = ((e.x - s.x) * (p.y - s.y) - (e.y - s.y) * (p.x - s.x)) / s.distance(e);
            assert!(across.abs() < 1e-6, "{p:?} is {across} off the line");
        }
        // Fixing the line fixes both its ends: it no longer moves.
        apply(&mut session, vec![a], "Fix");
        let (a0, a1) = session.sketch.endpoints(a).unwrap();
        assert!(session.sketch.is_fixed(a0) && session.sketch.is_fixed(a1));
        assert_eq!(
            session.report.as_ref().unwrap().definition(a),
            Some(oxyde_sketch::Definition::Fully)
        );
        // One undo takes the two relations of Fix away together.
        assert!(session.undo());
        assert!(!session.sketch.is_fixed(a0) && !session.sketch.is_fixed(a1));
    }

    #[test]
    fn three_point_and_tangent_arcs() {
        let mut s = ConstrainedSketch::new();
        let base = s.line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0));
        let (_, tip) = s.endpoints(base).unwrap();
        let mut session = session(s);
        // From (20, 0) to (30, 0) bulging down through (25, -5): a half circle about
        // (25, 0), running counter-clockwise from (20, 0).
        session.set_tool(Tool::ThreePointArc);
        for at in [
            Point2::new(20.0, 0.0),
            Point2::new(30.0, 0.0),
            Point2::new(25.0, -5.0),
        ] {
            session.click_tool(Pending { at, snap: None });
        }
        let arcs: Vec<(EntityId, EntityId, EntityId)> = session
            .sketch
            .entities()
            .iter()
            .filter_map(|e| match e.geometry {
                Geometry::Arc { center, start, end } => Some((center, start, end)),
                _ => None,
            })
            .collect();
        let [(c, start, _)] = arcs.as_slice() else {
            panic!("one arc: {arcs:?}")
        };
        let at = |s: &SketchSession, id| s.sketch.position(id).unwrap();
        assert!(at(&session, *c).distance(Point2::new(25.0, 0.0)) < 1e-9);
        assert!(at(&session, *start).distance(Point2::new(20.0, 0.0)) < 1e-9);

        // From the line's end at (10, 0), on to (10, 10): a half circle turning left,
        // tangent to the line.
        session.set_tool(Tool::TangentArc);
        session.click_tool(Pending {
            at: Point2::new(10.0, 0.0),
            snap: Some(Snap::Point(tip)),
        });
        session.click_tool(Pending {
            at: Point2::new(10.0, 10.0),
            snap: None,
        });
        assert!(has(
            &session,
            |r| matches!(r, Constraint::Tangent(l, _) if *l == base)
        ));
        let report = session.report.as_ref().unwrap();
        assert!(report.converged);
        let tangent = session
            .sketch
            .entities()
            .iter()
            .find_map(|e| match e.geometry {
                Geometry::Arc { center, start, .. } if start == tip => Some(center),
                _ => None,
            })
            .expect("an arc counter-clockwise from the line's end");
        assert!(at(&session, tangent).distance(Point2::new(10.0, 5.0)) < 1e-6);
        // Straight ahead of the line: no arc.
        let before = session.sketch.clone();
        session.click_tool(Pending {
            at: Point2::new(10.0, 0.0),
            snap: Some(Snap::Point(tip)),
        });
        session.click_tool(Pending {
            at: Point2::new(20.0, 0.0),
            snap: None,
        });
        assert_eq!(session.sketch, before);
    }

    /// The closed contours of the session's sketch, and their area.
    fn profile_area(session: &SketchSession) -> (usize, f64) {
        let profile = oxyde_geom::Profile::from_curves(&session.sketch.profile_segments())
            .expect("a closed profile");
        let area = profile
            .regions
            .iter()
            .map(|r| r.outer.signed_area().abs())
            .sum();
        (profile.regions.len(), area)
    }

    #[test]
    fn a_slot_is_two_half_circles_joined_by_tangent_lines() {
        let mut session = session(ConstrainedSketch::new());
        session.set_tool(Tool::Slot);
        for at in [
            Point2::new(0.0, 0.0),
            Point2::new(20.0, 0.0),
            Point2::new(10.0, 4.0),
        ] {
            session.click_tool(Pending { at, snap: None });
        }
        let count = |f: fn(&Geometry) -> bool, construction: bool| {
            session
                .sketch
                .entities()
                .iter()
                .filter(|e| f(&e.geometry) && e.construction == construction)
                .count()
        };
        assert_eq!(count(|g| matches!(g, Geometry::Arc { .. }), false), 2);
        assert_eq!(count(|g| matches!(g, Geometry::Line { .. }), false), 2);
        assert_eq!(count(|g| matches!(g, Geometry::Line { .. }), true), 1);
        let report = session.report.as_ref().unwrap();
        assert!(report.converged);
        // Two centres and a width, the centerline horizontal.
        assert_eq!(report.degrees_of_freedom, 4);
        let (regions, area) = profile_area(&session);
        assert_eq!(regions, 1);
        let want = 20.0 * 8.0 + std::f64::consts::PI * 16.0;
        assert!((area - want).abs() < 0.05 * want, "{area} vs {want}");
        // One radius dimension sizes both ends.
        let arc = session
            .sketch
            .entities()
            .iter()
            .find(|e| matches!(e.geometry, Geometry::Arc { .. }))
            .unwrap()
            .id;
        assert!(session.add_dimension(Constraint::Radius(arc, 5.0), 5.0));
        let radii: Vec<f64> = session
            .sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, Geometry::Arc { .. }))
            .map(|e| session.sketch.radius_of(e.id).unwrap())
            .collect();
        assert!(radii.iter().all(|r| (r - 5.0).abs() < 1e-6), "{radii:?}");
    }

    #[test]
    fn a_polygon_has_equal_sides_on_its_construction_circle() {
        let mut session = session(ConstrainedSketch::new());
        let origin = session.sketch.origin().unwrap();
        session.set_tool(Tool::Polygon);
        session.polygon_sides = 6;
        session.click_tool(Pending {
            at: Point2::ORIGIN,
            snap: Some(Snap::Point(origin)),
        });
        session.click_tool(Pending {
            at: Point2::new(10.0, 0.0),
            snap: None,
        });
        let lines: Vec<EntityId> = session
            .sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, Geometry::Line { .. }))
            .map(|e| e.id)
            .collect();
        assert_eq!(lines.len(), 6);
        for line in &lines {
            let length = session.sketch.line_length(*line).unwrap();
            assert!((length - 10.0).abs() < 1e-6, "{length}");
        }
        let report = session.report.as_ref().unwrap();
        assert!(report.converged);
        // On the origin: its size and how it is turned are left.
        assert_eq!(report.degrees_of_freedom, 2);
        let (regions, area) = profile_area(&session);
        assert_eq!(regions, 1);
        let hexagon = 1.5 * 3f64.sqrt() * 100.0;
        assert!((area - hexagon).abs() < 1e-6, "{area}");
    }

    #[test]
    fn a_point_keeps_what_it_snapped_to() {
        let mut s = ConstrainedSketch::new();
        let line = s.line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0));
        let mut session = session(s);
        session.set_tool(Tool::Point);
        session.click_tool(Pending {
            at: Point2::new(4.0, 0.0),
            snap: Some(Snap::OnCurve(line)),
        });
        session.click_tool(Pending {
            at: Point2::new(3.0, 7.0),
            snap: None,
        });
        assert!(has(
            &session,
            |c| matches!(c, Constraint::PointOnLine(_, l) if *l == line)
        ));
        let points = session
            .sketch
            .entities()
            .iter()
            .filter(|e| matches!(e.geometry, Geometry::Point(_)))
            .count();
        // The origin, the line's two ends and the two new points.
        assert_eq!(points, 5);
        assert!(session.report.as_ref().unwrap().converged);
    }

    #[test]
    fn extending_in_the_session_can_be_undone() {
        let mut s = ConstrainedSketch::new();
        let h = s.line(Point2::new(0.0, 0.0), Point2::new(4.0, 0.0));
        s.line(Point2::new(8.0, -3.0), Point2::new(8.0, 3.0));
        let mut session = session(s);
        let before = session.sketch.clone();
        session.set_tool(Tool::Extend);
        // Behind the start: nothing to meet, nothing changes, nothing to undo.
        session.extend(h, Point2::new(0.5, 0.0));
        assert_eq!(session.sketch, before);
        assert!(session.message.is_some());
        session.extend(h, Point2::new(3.5, 0.0));
        let (_, end) = session.sketch.endpoints(h).unwrap();
        let at = session.sketch.position(end).unwrap();
        assert!((at.x - 8.0).abs() < 1e-7 && at.y.abs() < 1e-7, "{at:?}");
        assert!(session.undo());
        assert_eq!(session.sketch, before);
        assert!(!session.undo(), "the refused extension made no undo step");
    }

    #[test]
    fn trimming_in_the_session_can_be_undone() {
        let mut s = ConstrainedSketch::new();
        let h = s.line(Point2::new(-10.0, 0.0), Point2::new(10.0, 0.0));
        s.line(Point2::new(0.0, -10.0), Point2::new(0.0, 10.0));
        let mut session = session(s);
        let before = session.sketch.clone();
        session.trim(h, Point2::new(5.0, 0.0));
        assert_ne!(session.sketch, before);
        assert!(session.undo());
        assert_eq!(session.sketch, before);
    }
}
