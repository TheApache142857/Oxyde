//! Sketch mode: drawing, dragging, trimming and constraining a sketch in the viewport.
//!
//! The session edits a working copy of the sketch; it is written back to the timeline when
//! the user exits the sketch. Colours follow SolidWorks: blue geometry can still move,
//! black is fully defined, red takes part in conflicting relations.
//!
//! While drawing, the cursor snaps to existing points, to the middle of lines, to crossings
//! and onto curves; the new point keeps that relation (coincident, midpoint, on curve).

use std::collections::HashSet;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Shape, Stroke, Vec2};
use oxyde_geom::{Plane, Point2};
use oxyde_model::FeatureId;
use oxyde_sketch::{
    ConstrainedSketch, Constraint, ConstraintState, Definition, EntityId, Geometry, RelationId,
    SolveReport,
};

use crate::icons::Icon;
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

const SELECTED: Color32 = Color32::from_rgb(255, 128, 0);
const HOVERED: Color32 = Color32::from_rgb(255, 170, 60);
const CONFLICT: Color32 = Color32::from_rgb(200, 40, 40);
const FULLY: Color32 = Color32::from_rgb(20, 20, 20);
const UNDER: Color32 = Color32::from_rgb(0, 90, 220);
const CONSTRUCTION: Color32 = Color32::from_rgb(110, 140, 190);
const PREVIEW: Color32 = Color32::from_rgb(120, 120, 120);
const DIMENSION: Color32 = Color32::from_rgb(90, 90, 90);
const SNAPPED: Color32 = Color32::from_rgb(230, 120, 0);
const TRIMMED: Color32 = Color32::from_rgb(220, 40, 40);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Dimension,
    Line,
    Centerline,
    Rectangle,
    Circle,
    Arc,
    Trim,
}

impl Tool {
    /// The tools of the Sketch tab, in order, with icon and label.
    pub const ALL: [(Tool, Icon, &'static str); 8] = [
        (Tool::Select, Icon::Select, "Select"),
        (Tool::Dimension, Icon::SmartDimension, "Smart Dimension"),
        (Tool::Line, Icon::Line, "Line"),
        (Tool::Centerline, Icon::Centerline, "Centerline"),
        (Tool::Rectangle, Icon::Rectangle, "Rectangle"),
        (Tool::Circle, Icon::Circle, "Circle"),
        (Tool::Arc, Icon::Arc, "Arc"),
        (Tool::Trim, Icon::Trim, "Trim"),
    ];

    /// Tools that draw new geometry.
    pub fn draws(self) -> bool {
        matches!(
            self,
            Tool::Line | Tool::Centerline | Tool::Rectangle | Tool::Circle | Tool::Arc
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
            Tool::Circle => "Click the centre, then a point on the circle",
            Tool::Arc => "Click the centre, the start, then the end (counter-clockwise)",
            Tool::Trim => "Click the piece of a curve to remove, up to the nearest crossings",
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
    pub report: Option<SolveReport>,
    pub tool: Tool,
    pub selection: Vec<EntityId>,
    /// Created by the "Sketch" command (as opposed to editing an existing sketch).
    pub new_sketch: bool,
    /// Where the rollback bar was before editing started.
    pub restore_rollback: Option<FeatureId>,
    pub message: Option<String>,
    /// Dimensions set by equations: shown with Σ, not editable here.
    pub driven: HashSet<RelationId>,
    pending: Vec<Pending>,
    hover: Option<EntityId>,
    drag: Option<Drag>,
    labels: Vec<(Rect, RelationId)>,
    history: Vec<ConstrainedSketch>,
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
        let mut session = Self {
            feature,
            name,
            plane,
            original: sketch.clone(),
            sketch,
            report: None,
            tool: if new_sketch { Tool::Line } else { Tool::Select },
            selection: Vec::new(),
            new_sketch,
            restore_rollback,
            message: None,
            driven: HashSet::new(),
            pending: Vec::new(),
            hover: None,
            drag: None,
            labels: Vec::new(),
            history: Vec::new(),
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

    fn checkpoint(&mut self) {
        self.history.push(self.sketch.clone());
        if self.history.len() > UNDO_LIMIT {
            self.history.remove(0);
        }
        self.message = None;
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.history.pop() else {
            return false;
        };
        self.sketch = previous;
        self.selection.clear();
        self.pending.clear();
        self.solve();
        true
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.pending.clear();
        if tool == Tool::Dimension || tool.draws() || tool == Tool::Trim {
            self.selection.clear();
        }
    }

    /// Escape: abandons the shape in progress, then leaves the tool. Returns false when
    /// there was nothing to cancel.
    pub fn cancel_operation(&mut self) -> bool {
        if !self.pending.is_empty() {
            self.pending.clear();
            true
        } else if self.tool != Tool::Select {
            self.tool = Tool::Select;
            self.selection.clear();
            true
        } else if !self.selection.is_empty() {
            self.selection.clear();
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

    /// Adds a relation, or refuses it when it conflicts with the existing ones.
    pub fn add_relation(&mut self, relation: Constraint) -> bool {
        self.checkpoint();
        let id = self.sketch.constrain(relation);
        if self.solve() {
            true
        } else {
            self.sketch.remove_relation(id);
            self.history.pop();
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
            self.history.pop();
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
            self.history.pop();
            return false;
        }
        if self.solve() {
            true
        } else {
            self.sketch = previous;
            self.history.pop();
            self.solve();
            self.message =
                Some("That value cannot be reached with the existing relations".to_owned());
            false
        }
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
                self.history.pop();
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

    pub fn delete_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        self.checkpoint();
        let doomed = std::mem::take(&mut self.selection);
        self.sketch.remove(&doomed);
        self.solve();
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
            self.history.pop();
            self.solve();
            self.message = Some("Trimming there conflicts with the existing relations".to_owned());
        }
        self.selection.clear();
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

    /// Relations without a value that apply to the selection.
    pub fn relation_options(&self) -> Vec<(&'static str, Constraint)> {
        use Constraint::*;
        let (p, l, r) = self.selected_kinds();
        match (p.len(), l.len(), r.len()) {
            (1, 0, 0) => vec![("Fix", Fixed(p[0]))],
            (2, 0, 0) => vec![("Coincident", Coincident(p[0], p[1]))],
            (0, 1, 0) => vec![
                ("Horizontal", Horizontal(l[0])),
                ("Vertical", Vertical(l[0])),
            ],
            (0, 2, 0) => vec![
                ("Parallel", Parallel(l[0], l[1])),
                ("Perpendicular", Perpendicular(l[0], l[1])),
                ("Equal", EqualLength(l[0], l[1])),
            ],
            (0, 0, 2) => vec![("Equal", EqualRadius(r[0], r[1]))],
            (0, 1, 1) => vec![("Tangent", Tangent(l[0], r[0]))],
            (1, 1, 0) => vec![
                ("On line", PointOnLine(p[0], l[0])),
                ("Midpoint", Midpoint(p[0], l[0])),
            ],
            (1, 0, 1) => vec![("On circle", PointOnCircle(p[0], r[0]))],
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
        match drag.kind {
            DragKind::Point(p) => {
                self.sketch.move_point(p, to);
            }
            DragKind::Line(a, b) => {
                let (dx, dy) = (to.x - drag.last.x, to.y - drag.last.y);
                for p in [a, b] {
                    if let Some(q) = self.sketch.position(p) {
                        self.sketch.move_point(p, Point2::new(q.x + dx, q.y + dy));
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
            Tool::Select | Tool::Dimension | Tool::Trim => {}
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
                    if !self.solve() {
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
                if self.pending.len() < 2 {
                    if self
                        .pending
                        .first()
                        .is_none_or(|c| c.at.distance(p.at) > 1e-6)
                    {
                        self.pending.push(p);
                    }
                    return;
                }
                let (center, start) = (self.pending[0], self.pending[1]);
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
                    let e = self.sketch.point(end_at);
                    self.sketch.arc_between(c, s, e);
                    self.solve_new_shape(relations_before);
                }
                self.pending.clear();
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
            SELECTED
        } else if self.hover == Some(id) {
            HOVERED
        } else if conflicting.contains(&id) {
            CONFLICT
        } else if construction {
            CONSTRUCTION
        } else {
            match self.report.as_ref().and_then(|r| r.definition(id)) {
                Some(Definition::Fully) => FULLY,
                _ => UNDER,
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
        let at = |id| self.sketch.position(id).unwrap_or_default();

        // Sketch origin and axes.
        let origin = to_screen(Point2::ORIGIN);
        for (axis, color) in [
            (Point2::new(1.0, 0.0), Color32::from_rgb(210, 50, 50)),
            (Point2::new(0.0, 1.0), Color32::from_rgb(40, 150, 60)),
        ] {
            let d = to_screen(axis) - origin;
            if d.length() > 1e-6 {
                painter.line_segment(
                    [origin, origin + d.normalized() * 18.0],
                    Stroke::new(1.5, color),
                );
            }
        }

        for e in self.sketch.entities() {
            let color = self.color(e.id, e.construction, &conflicting);
            let width = if self.selection.contains(&e.id) || self.hover == Some(e.id) {
                2.5
            } else {
                1.6
            };
            let stroke = Stroke::new(width, color);
            let path: Vec<Pos2> = match e.geometry {
                Geometry::Point(_) => continue,
                Geometry::Line { start, end } => vec![to_screen(at(start)), to_screen(at(end))],
                Geometry::Circle { center, radius } => {
                    arc_points(at(center), radius, 0.0, std::f64::consts::TAU, 72)
                        .into_iter()
                        .map(to_screen)
                        .collect()
                }
                Geometry::Arc { center, start, end } => arc_path(at(center), at(start), at(end))
                    .into_iter()
                    .map(to_screen)
                    .collect(),
            };
            if e.construction {
                painter.extend(Shape::dashed_line(&path, stroke, 6.0, 4.0));
            } else {
                painter.add(Shape::line(path, stroke));
            }
        }

        for e in self.sketch.entities() {
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

        // Relation glyphs.
        for relation in self.sketch.relations() {
            let (line, glyph) = match relation.constraint {
                Constraint::Horizontal(l) => (l, "H"),
                Constraint::Vertical(l) => (l, "V"),
                _ => continue,
            };
            if let Some(mid) = self.sketch.midpoint_of(line) {
                painter.text(
                    to_screen(mid) + Vec2::new(5.0, -5.0),
                    Align2::LEFT_BOTTOM,
                    glyph,
                    FontId::proportional(10.0),
                    PREVIEW,
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
            let stroke = Stroke::new(1.0, DIMENSION);
            for segment in &graphics.lines {
                painter.line_segment(*segment, stroke);
            }
            for (tip, direction) in &graphics.arrows {
                arrow_head(painter, *tip, *direction, DIMENSION);
            }
            let text = if self.driven.contains(&relation.id) {
                format!("Σ {}", graphics.text)
            } else {
                graphics.text
            };
            let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), FULLY);
            let rect = Rect::from_center_size(graphics.anchor, galley.size() + Vec2::new(8.0, 4.0));
            painter.rect_filled(
                rect,
                3.0,
                Color32::from_rgba_unmultiplied(255, 255, 255, 220),
            );
            painter.rect_stroke(rect, 3.0, Stroke::new(1.0, Color32::from_gray(160)));
            painter.galley(rect.center() - galley.size() / 2.0, galley, FULLY);
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
                    UNDER,
                );
            }
        }

        // The piece the Trim tool would remove.
        if let Some(piece) = trimmed {
            let path: Vec<Pos2> = piece.into_iter().map(to_screen).collect();
            painter.extend(Shape::dashed_line(
                &path,
                Stroke::new(2.5, TRIMMED),
                5.0,
                3.0,
            ));
        }

        // The shape in progress.
        let cursor = snapped.map(|(p, _)| p).or(cursor);
        let dashed = Stroke::new(1.2, PREVIEW);
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
                            let sweep = (a1 - a0).rem_euclid(std::f64::consts::TAU);
                            arc_points(c, c.distance(start.at), a0, sweep, 48)
                                .into_iter()
                                .map(to_screen)
                                .collect()
                        }
                    };
                    painter.extend(Shape::dashed_line(&path, dashed, 5.0, 3.0));
                }
                Tool::Select | Tool::Dimension | Tool::Trim => {}
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
                    text: format!("{:.1}°", value.to_degrees()),
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
            Some(Geometry::Circle { .. } | Geometry::Arc { .. }) => {
                let radius = s.radius_of(*one).unwrap_or(0.0);
                vec![option("Radius…", Radius(*one, radius), radius, false)]
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
                let angle_option = option("Angle…", Angle(*a, *b, angle), angle.to_degrees(), true);
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

fn perpendicular_foot(p: Point2, a: Point2, b: Point2) -> Point2 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length2 = dx * dx + dy * dy;
    if length2 < 1e-24 {
        return a;
    }
    let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / length2;
    Point2::new(a.x + dx * t, a.y + dy * t)
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
    let stroke = Stroke::new(1.8, SNAPPED);
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
    }
}

/// The value of a dimension, as shown to the user (degrees for angles).
pub fn dimension_value(c: &Constraint) -> Option<(f64, bool)> {
    let value = c.value()?;
    if c.is_angle() {
        Some((value.to_degrees(), true))
    } else {
        Some((value, false))
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

fn segment_distance(p: Pos2, a: Pos2, b: Pos2) -> f32 {
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
            if let Some(press) = response.interact_pointer_pos() {
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

    session.hover = match (session.drag, pointer, session.tool) {
        (None, Some(p), Tool::Trim) => session.curve_hit(p, &to_screen),
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

    if response.clicked() {
        if let Some(pos) = pointer {
            let label = session
                .labels
                .iter()
                .find(|(r, _)| r.contains(pos))
                .map(|(_, id)| *id);
            match session.tool {
                Tool::Select | Tool::Dimension if label.is_some() => {
                    request = label.map(SketchRequest::EditDimension);
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
                _ => {
                    if let Some((at, snap)) = snapped {
                        session.click_tool(Pending { at, snap });
                    }
                }
            }
        }
    }

    let painter = ui.painter_at(out.rect);
    session.draw(&painter, &to_screen, cursor, pointer, snapped, trimmed);
    request
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
        assert!(matches!(first(&[circle]).relation, Constraint::Radius(..)));
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
        // A circle at once: its radius.
        let radius = session.click_dimension(Some(circle)).unwrap();
        assert!(matches!(radius.relation, Constraint::Radius(..)));
        assert!(session.selection.is_empty());
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
