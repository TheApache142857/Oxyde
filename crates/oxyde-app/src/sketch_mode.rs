//! Sketch mode: drawing, dragging and constraining a sketch in the viewport.
//!
//! The session edits a working copy of the sketch; it is written back to the timeline when
//! the user exits the sketch. Colours follow SolidWorks: blue geometry can still move,
//! black is fully defined, red takes part in conflicting relations.

use std::collections::HashSet;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Shape, Stroke, Vec2};
use oxyde_geom::{Plane, Point2};
use oxyde_model::FeatureId;
use oxyde_sketch::{
    ConstrainedSketch, Constraint, ConstraintState, Definition, EntityId, Geometry, RelationId,
    SolveReport,
};

use crate::viewport::{Viewport, ViewportOutput};

/// Clicking within this many points of an existing point reuses it.
const SNAP: f32 = 8.0;
/// Clicking within this many points of a curve selects it.
const PICK: f32 = 6.0;
/// Lines drawn within 3° of horizontal or vertical get that relation automatically.
const AUTO_SLOPE: f64 = 0.0524;
const UNDO_LIMIT: usize = 100;

const SELECTED: Color32 = Color32::from_rgb(255, 128, 0);
const HOVERED: Color32 = Color32::from_rgb(255, 170, 60);
const CONFLICT: Color32 = Color32::from_rgb(200, 40, 40);
const FULLY: Color32 = Color32::from_rgb(20, 20, 20);
const UNDER: Color32 = Color32::from_rgb(0, 90, 220);
const CONSTRUCTION: Color32 = Color32::from_rgb(110, 140, 190);
const PREVIEW: Color32 = Color32::from_rgb(120, 120, 120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Line,
    Rectangle,
    Circle,
    Arc,
}

impl Tool {
    pub const ALL: [(Tool, &'static str); 5] = [
        (Tool::Select, "Select"),
        (Tool::Line, "Line"),
        (Tool::Rectangle, "Rectangle"),
        (Tool::Circle, "Circle"),
        (Tool::Arc, "Arc"),
    ];

    fn hint(self) -> &'static str {
        match self {
            Tool::Select => "Click to select, drag to move, right-click for relations",
            Tool::Line => "Click points; click an existing point to close the contour",
            Tool::Rectangle => "Click two opposite corners",
            Tool::Circle => "Click the centre, then a point on the circle",
            Tool::Arc => "Click the centre, the start, then the end (counter-clockwise)",
        }
    }
}

/// A point placed by the current tool, created only when the shape is finished.
#[derive(Debug, Clone, Copy)]
struct Pending {
    at: Point2,
    existing: Option<EntityId>,
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
    }

    /// Escape: abandons the shape in progress, then leaves the tool. Returns false when
    /// there was nothing to cancel.
    pub fn cancel_operation(&mut self) -> bool {
        if !self.pending.is_empty() {
            self.pending.clear();
            true
        } else if self.tool != Tool::Select {
            self.tool = Tool::Select;
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

    /// Dimensions that apply to the selection, with their current measurement.
    pub fn dimension_options(&self) -> Vec<DimensionOption> {
        use Constraint::*;
        let (p, l, r) = self.selected_kinds();
        let s = &self.sketch;
        let option = |label, relation, shown, angle| DimensionOption {
            label,
            relation,
            shown,
            angle,
        };
        match (p.len(), l.len(), r.len()) {
            (0, 1, 0) => {
                let length = s.line_length(l[0]).unwrap_or(0.0);
                vec![option("Length…", Length(l[0], length), length, false)]
            }
            (2, 0, 0) => {
                let (a, b) = match (s.position(p[0]), s.position(p[1])) {
                    (Some(a), Some(b)) => (a, b),
                    _ => return Vec::new(),
                };
                let (d, dx, dy) = (a.distance(b), (b.x - a.x).abs(), (b.y - a.y).abs());
                vec![
                    option("Distance…", Distance(p[0], p[1], d), d, false),
                    option(
                        "Horizontal distance…",
                        HorizontalDistance(p[0], p[1], dx),
                        dx,
                        false,
                    ),
                    option(
                        "Vertical distance…",
                        VerticalDistance(p[0], p[1], dy),
                        dy,
                        false,
                    ),
                ]
            }
            (0, 0, 1) => {
                let radius = s.radius_of(r[0]).unwrap_or(0.0);
                vec![option("Radius…", Radius(r[0], radius), radius, false)]
            }
            (0, 2, 0) => {
                let angle = s.angle_between(l[0], l[1]).unwrap_or(0.0);
                vec![option(
                    "Angle…",
                    Angle(l[0], l[1], angle),
                    angle.to_degrees(),
                    true,
                )]
            }
            _ => Vec::new(),
        }
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

    /// The entity under a screen position: points first, then curves.
    fn hit(&self, pos: Pos2, to_screen: &dyn Fn(Point2) -> Pos2) -> Option<EntityId> {
        if let Some(point) = self.nearest_point(pos, to_screen) {
            return Some(point);
        }
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
                    Geometry::Arc { center, start, .. } => {
                        round_distance(pos, to_screen(at(center)), to_screen(at(start)))
                    }
                };
                Some((e.id, distance))
            })
            .filter(|(_, d)| *d < PICK)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
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
                if let Some(Geometry::Circle { center, .. }) =
                    self.sketch.entity(circle).map(|e| e.geometry)
                {
                    if let Some(c) = self.sketch.position(center) {
                        self.sketch
                            .set_circle_radius(circle, c.distance(to).max(1e-3));
                    }
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

    fn realize(&mut self, p: Pending) -> EntityId {
        match p.existing {
            Some(id) => id,
            None => self.sketch.point(p.at),
        }
    }

    fn click_tool(&mut self, at: Point2, snapped: Option<EntityId>) {
        let p = Pending {
            at: snapped
                .and_then(|id| self.sketch.position(id))
                .unwrap_or(at),
            existing: snapped,
        };
        match self.tool {
            Tool::Select => {}
            Tool::Line => match self.pending.first().copied() {
                None => self.pending = vec![p],
                Some(start) => {
                    let same = (start.existing.is_some() && start.existing == p.existing)
                        || start.at.distance(p.at) < 1e-9;
                    if same {
                        return;
                    }
                    self.checkpoint();
                    let a = self.realize(start);
                    let b = self.realize(p);
                    let line = self.sketch.line_between(a, b);
                    let auto = self.auto_relation(line, start.at, p.at);
                    if !self.solve() {
                        if let Some(id) = auto {
                            self.sketch.remove_relation(id);
                            self.solve();
                        }
                    }
                    // Landing on an existing point closes the contour and ends the chain.
                    self.pending = if p.existing.is_some() {
                        Vec::new()
                    } else {
                        vec![Pending {
                            at: p.at,
                            existing: Some(b),
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
                        self.sketch.rectangle(Point2::new(x0, y0), x1 - x0, y1 - y0);
                        self.solve();
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
                        self.sketch.circle_around(c, radius);
                        self.solve();
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
                    self.solve();
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

    fn draw(
        &mut self,
        painter: &egui::Painter,
        to_screen: &dyn Fn(Point2) -> Pos2,
        cursor: Option<Point2>,
        pointer: Option<Pos2>,
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
                Geometry::Arc { center, start, end } => {
                    let (c, s, f) = (at(center), at(start), at(end));
                    let a0 = (s.y - c.y).atan2(s.x - c.x);
                    let a1 = (f.y - c.y).atan2(f.x - c.x);
                    let sweep = (a1 - a0).rem_euclid(std::f64::consts::TAU);
                    arc_points(c, c.distance(s), a0, sweep, 48)
                        .into_iter()
                        .map(to_screen)
                        .collect()
                }
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
            if let Some((a, b)) = self.sketch.endpoints(line) {
                let mid = to_screen(Point2::new(
                    (at(a).x + at(b).x) / 2.0,
                    (at(a).y + at(b).y) / 2.0,
                ));
                painter.text(
                    mid + Vec2::new(5.0, -5.0),
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
            if let Some((anchor, text)) = self.dimension_label(&relation.constraint, to_screen) {
                let text = if self.driven.contains(&relation.id) {
                    format!("Σ {text}")
                } else {
                    text
                };
                let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), FULLY);
                let rect = Rect::from_center_size(anchor, galley.size() + Vec2::new(8.0, 4.0));
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

        // The shape in progress.
        let dashed = Stroke::new(1.2, PREVIEW);
        if let (Some(first), Some(cursor)) = (self.pending.first(), cursor) {
            match self.tool {
                Tool::Line => {
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
                Tool::Select => {}
            }
        }
    }

    fn dimension_label(
        &self,
        c: &Constraint,
        to_screen: &dyn Fn(Point2) -> Pos2,
    ) -> Option<(Pos2, String)> {
        use Constraint::*;
        let s = &self.sketch;
        let at = |id| s.position(id);
        let middle = |a: Point2, b: Point2| Point2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        match *c {
            Length(line, value) => {
                let (a, b) = s.endpoints(line)?;
                let (a, b) = (at(a)?, at(b)?);
                let (sa, sb) = (to_screen(a), to_screen(b));
                let along = sb - sa;
                let offset = if along.length() > 1e-6 {
                    Vec2::new(-along.y, along.x).normalized() * 14.0
                } else {
                    Vec2::new(0.0, -14.0)
                };
                Some((to_screen(middle(a, b)) + offset, format_length(value)))
            }
            Distance(p, q, value)
            | HorizontalDistance(p, q, value)
            | VerticalDistance(p, q, value) => {
                let m = middle(at(p)?, at(q)?);
                Some((to_screen(m) + Vec2::new(0.0, -12.0), format_length(value)))
            }
            Radius(round, value) => {
                let center = match s.entity(round)?.geometry {
                    Geometry::Circle { center, .. } | Geometry::Arc { center, .. } => at(center)?,
                    _ => return None,
                };
                let rim = Point2::new(
                    center.x + value * std::f64::consts::FRAC_1_SQRT_2,
                    center.y + value * std::f64::consts::FRAC_1_SQRT_2,
                );
                Some((
                    to_screen(rim) + Vec2::new(12.0, -8.0),
                    format!("R{}", format_length(value)),
                ))
            }
            Angle(first, _, value) => {
                let (a, b) = s.endpoints(first)?;
                let m = middle(at(a)?, at(b)?);
                Some((
                    to_screen(m) + Vec2::new(16.0, -16.0),
                    format!("{:.1}°", value.to_degrees()),
                ))
            }
            _ => None,
        }
    }
}

/// The value of a dimension, as shown to the user (degrees for angles).
pub fn dimension_value(c: &Constraint) -> Option<(f64, bool)> {
    use Constraint::*;
    match *c {
        Distance(_, _, v)
        | HorizontalDistance(_, _, v)
        | VerticalDistance(_, _, v)
        | Length(_, v)
        | Radius(_, v) => Some((v, false)),
        Angle(_, _, v) => Some((v.to_degrees(), true)),
        _ => None,
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

    session.hover = match (session.drag, pointer) {
        (None, Some(p)) => session.hit(p, &to_screen),
        _ => None,
    };

    if response.clicked() {
        if let Some(pos) = pointer {
            if session.tool == Tool::Select {
                let label = session
                    .labels
                    .iter()
                    .find(|(r, _)| r.contains(pos))
                    .map(|(_, id)| *id);
                match label {
                    Some(id) => request = Some(SketchRequest::EditDimension(id)),
                    None => {
                        let hit = session.hit(pos, &to_screen);
                        session.click_select(hit, additive);
                    }
                }
            } else if let Some(at) = to_plane(pos) {
                let snapped = session.nearest_point(pos, &to_screen);
                session.click_tool(at, snapped);
            }
        }
    }

    let painter = ui.painter_at(out.rect);
    session.draw(&painter, &to_screen, pointer.and_then(to_plane), pointer);
    request
}
