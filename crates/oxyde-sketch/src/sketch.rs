//! Sketch entities, constraints, and how they map to solver unknowns and equations.

use std::collections::HashMap;
use std::f64::consts::{PI, TAU};
use std::fmt;

use oxyde_geom::{Curve2, Point2, ProfileSegment};

/// Identity of a sketch entity. The ids of lines, arcs and circles are also the profile
/// segment ids, and therefore name the faces an extrusion sweeps from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntityId(pub u32);

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Geometry of an entity. Lines, circles and arcs refer to point entities, so that
/// connected geometry shares points as in SolidWorks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Geometry {
    Point(Point2),
    Line {
        start: EntityId,
        end: EntityId,
    },
    Circle {
        center: EntityId,
        radius: f64,
    },
    /// Runs counter-clockwise from `start` to `end` around `center`. Its radius is implied:
    /// the solver keeps `start` and `end` at the same distance from `center`.
    Arc {
        center: EntityId,
        start: EntityId,
        end: EntityId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entity {
    pub id: EntityId,
    pub geometry: Geometry,
    /// Construction geometry is solved but not part of the profile.
    pub construction: bool,
}

/// Geometric relations and dimensions. Distances are in mm, angles in radians.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Constraint {
    /// The point does not move.
    Fixed(EntityId),
    Coincident(EntityId, EntityId),
    Horizontal(EntityId),
    Vertical(EntityId),
    Parallel(EntityId, EntityId),
    Perpendicular(EntityId, EntityId),
    EqualLength(EntityId, EntityId),
    /// Two circles or arcs.
    EqualRadius(EntityId, EntityId),
    /// Between two points.
    Distance(EntityId, EntityId, f64),
    /// Horizontal distance between two points.
    HorizontalDistance(EntityId, EntityId, f64),
    /// Vertical distance between two points.
    VerticalDistance(EntityId, EntityId, f64),
    /// Length of a line.
    Length(EntityId, f64),
    /// Radius of a circle or arc.
    Radius(EntityId, f64),
    /// Angle from the first line to the second, counter-clockwise.
    Angle(EntityId, EntityId, f64),
    /// A point on the infinite extension of a line.
    PointOnLine(EntityId, EntityId),
    /// A point on a circle or arc.
    PointOnCircle(EntityId, EntityId),
    /// A point at the middle of a line.
    Midpoint(EntityId, EntityId),
    /// A line tangent to a circle or arc.
    Tangent(EntityId, EntityId),
}

impl Constraint {
    /// Entities the constraint refers to.
    pub fn entities(&self) -> Vec<EntityId> {
        use Constraint::*;
        match *self {
            Fixed(a) | Horizontal(a) | Vertical(a) | Length(a, _) | Radius(a, _) => vec![a],
            Coincident(a, b)
            | Parallel(a, b)
            | Perpendicular(a, b)
            | EqualLength(a, b)
            | EqualRadius(a, b)
            | Distance(a, b, _)
            | HorizontalDistance(a, b, _)
            | VerticalDistance(a, b, _)
            | Angle(a, b, _)
            | PointOnLine(a, b)
            | PointOnCircle(a, b)
            | Midpoint(a, b)
            | Tangent(a, b) => vec![a, b],
        }
    }
}

/// Stable identity of a relation or dimension within its sketch. Never reused, so that
/// references to a dimension (from equations, drawings) cannot silently move to another one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelationId(pub u32);

impl fmt::Display for RelationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "R{}", self.0)
    }
}

impl Constraint {
    /// The value of a dimension (mm, or radians for angles); `None` for plain relations.
    pub fn value(&self) -> Option<f64> {
        use Constraint::*;
        match *self {
            Distance(_, _, v)
            | HorizontalDistance(_, _, v)
            | VerticalDistance(_, _, v)
            | Length(_, v)
            | Radius(_, v)
            | Angle(_, _, v) => Some(v),
            _ => None,
        }
    }

    pub fn is_dimension(&self) -> bool {
        self.value().is_some()
    }

    pub fn is_angle(&self) -> bool {
        matches!(self, Constraint::Angle(..))
    }
}

/// A relation or dimension of a sketch.
#[derive(Debug, Clone, PartialEq)]
pub struct Relation {
    pub id: RelationId,
    pub constraint: Constraint,
    /// For dimensions: the name used in equations ("D1", or a user name such as "Width"),
    /// unique within the sketch. `None` for relations without a value.
    pub name: Option<String>,
}

/// Next ids to hand out. Stored with the sketch so ids stay unique across saves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    pub entity: u32,
    pub relation: u32,
    /// Number of the next default dimension name ("D3").
    pub dimension: u32,
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            entity: 1,
            relation: 1,
            dimension: 1,
        }
    }
}

/// Whether `name` can name a feature or a dimension: not empty, no surrounding spaces, and
/// none of the characters that structure full names (`@`) or equations (`"`).
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && !name.chars().any(|c| c == '@' || c == '"' || c.is_control())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SketchError {
    UnknownEntity(EntityId),
    DuplicateId(EntityId),
    UnknownRelation(RelationId),
    DuplicateRelation(RelationId),
    /// A dimension name that is invalid or already used in the sketch.
    BadName(String),
    /// A relation (by index) or entity refers to an entity of the wrong kind.
    WrongKind {
        constraint: Option<usize>,
        entity: EntityId,
        expected: &'static str,
    },
}

impl fmt::Display for SketchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SketchError::UnknownEntity(id) => write!(f, "no sketch entity {id}"),
            SketchError::DuplicateId(id) => write!(f, "sketch entity id {id} is used twice"),
            SketchError::UnknownRelation(id) => write!(f, "no sketch relation {id}"),
            SketchError::DuplicateRelation(id) => {
                write!(f, "sketch relation id {id} is used twice")
            }
            SketchError::BadName(name) => {
                write!(f, "\"{name}\" cannot be used as a dimension name here")
            }
            SketchError::WrongKind {
                constraint: Some(c),
                entity,
                expected,
            } => write!(f, "relation {c}: entity {entity} should be a {expected}"),
            SketchError::WrongKind {
                constraint: None,
                entity,
                expected,
            } => write!(f, "entity {entity} should be a {expected}"),
        }
    }
}

impl std::error::Error for SketchError {}

/// A sketch with its relations and dimensions.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConstrainedSketch {
    pub(crate) entities: Vec<Entity>,
    pub(crate) relations: Vec<Relation>,
    pub(crate) counters: Counters,
}

impl ConstrainedSketch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a sketch from stored entities and relations, checking every reference and
    /// name. `counters` are raised if needed so that new ids never collide.
    pub fn from_parts(
        entities: Vec<Entity>,
        relations: Vec<Relation>,
        counters: Counters,
    ) -> Result<Self, SketchError> {
        let mut seen = std::collections::HashSet::new();
        for e in &entities {
            if !seen.insert(e.id) {
                return Err(SketchError::DuplicateId(e.id));
            }
        }
        let mut seen = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for r in &relations {
            if !seen.insert(r.id) {
                return Err(SketchError::DuplicateRelation(r.id));
            }
            match &r.name {
                Some(name) if !r.constraint.is_dimension() || !is_valid_name(name) => {
                    return Err(SketchError::BadName(name.clone()));
                }
                Some(name) if !names.insert(name.clone()) => {
                    return Err(SketchError::BadName(name.clone()));
                }
                _ => {}
            }
        }
        let highest_entity = entities.iter().map(|e| e.id.0).max().unwrap_or(0);
        let highest_relation = relations.iter().map(|r| r.id.0).max().unwrap_or(0);
        let highest_dimension = relations
            .iter()
            .filter_map(|r| default_number(r.name.as_deref()?))
            .max()
            .unwrap_or(0);
        let sketch = Self {
            entities,
            relations,
            counters: Counters {
                entity: counters.entity.max(highest_entity + 1),
                relation: counters.relation.max(highest_relation + 1),
                dimension: counters.dimension.max(highest_dimension + 1),
            },
        };
        sketch.layout()?;
        // Dimensions stored without a name get the next default one.
        let mut sketch = sketch;
        for index in 0..sketch.relations.len() {
            if sketch.relations[index].constraint.is_dimension()
                && sketch.relations[index].name.is_none()
            {
                let name = sketch.next_dimension_name();
                sketch.relations[index].name = Some(name);
            }
        }
        Ok(sketch)
    }

    /// The next free default dimension name ("D4"), advancing the counter.
    fn next_dimension_name(&mut self) -> String {
        // Skip numbers a user may have taken by renaming another dimension "D7".
        loop {
            let candidate = format!("D{}", self.counters.dimension);
            self.counters.dimension += 1;
            if self.dimension_named(&candidate).is_none() {
                return candidate;
            }
        }
    }

    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    pub fn relations(&self) -> &[Relation] {
        &self.relations
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    pub fn entity(&self, id: EntityId) -> Option<&Entity> {
        self.entities.iter().find(|e| e.id == id)
    }

    pub fn relation(&self, id: RelationId) -> Option<&Relation> {
        self.relations.iter().find(|r| r.id == id)
    }

    fn push(&mut self, geometry: Geometry) -> EntityId {
        let id = EntityId(self.counters.entity);
        self.counters.entity += 1;
        self.entities.push(Entity {
            id,
            geometry,
            construction: false,
        });
        id
    }

    pub fn point(&mut self, at: Point2) -> EntityId {
        self.push(Geometry::Point(at))
    }

    /// A line between two existing points.
    pub fn line_between(&mut self, start: EntityId, end: EntityId) -> EntityId {
        self.push(Geometry::Line { start, end })
    }

    /// A line with two new end points.
    pub fn line(&mut self, start: Point2, end: Point2) -> EntityId {
        let (a, b) = (self.point(start), self.point(end));
        self.line_between(a, b)
    }

    /// A circle with a new centre point.
    pub fn circle(&mut self, center: Point2, radius: f64) -> EntityId {
        let center = self.point(center);
        self.push(Geometry::Circle { center, radius })
    }

    /// A circle around an existing centre point.
    pub fn circle_around(&mut self, center: EntityId, radius: f64) -> EntityId {
        self.push(Geometry::Circle { center, radius })
    }

    /// A counter-clockwise arc on existing centre, start and end points.
    pub fn arc_between(&mut self, center: EntityId, start: EntityId, end: EntityId) -> EntityId {
        self.push(Geometry::Arc { center, start, end })
    }

    /// A counter-clockwise arc with new centre, start and end points.
    pub fn arc(&mut self, center: Point2, start: Point2, end: Point2) -> EntityId {
        let (c, s, e) = (self.point(center), self.point(start), self.point(end));
        self.push(Geometry::Arc {
            center: c,
            start: s,
            end: e,
        })
    }

    /// A rectangle of four lines sharing their corners, with horizontal and vertical
    /// relations added as SolidWorks does. Returns the bottom, right, top and left lines.
    pub fn rectangle(&mut self, corner: Point2, width: f64, height: f64) -> [EntityId; 4] {
        let (x0, y0) = (corner.x, corner.y);
        let (x1, y1) = (x0 + width, y0 + height);
        let p = [
            self.point(Point2::new(x0, y0)),
            self.point(Point2::new(x1, y0)),
            self.point(Point2::new(x1, y1)),
            self.point(Point2::new(x0, y1)),
        ];
        let lines = [
            self.line_between(p[0], p[1]),
            self.line_between(p[1], p[2]),
            self.line_between(p[2], p[3]),
            self.line_between(p[3], p[0]),
        ];
        self.constrain(Constraint::Horizontal(lines[0]));
        self.constrain(Constraint::Vertical(lines[1]));
        self.constrain(Constraint::Horizontal(lines[2]));
        self.constrain(Constraint::Vertical(lines[3]));
        lines
    }

    /// Adds a relation or dimension. Dimensions get the next default name ("D1", "D2"…).
    pub fn constrain(&mut self, constraint: Constraint) -> RelationId {
        let id = RelationId(self.counters.relation);
        self.counters.relation += 1;
        let name = if constraint.is_dimension() {
            Some(self.next_dimension_name())
        } else {
            None
        };
        self.relations.push(Relation {
            id,
            constraint,
            name,
        });
        id
    }

    pub fn remove_relation(&mut self, id: RelationId) -> Option<Relation> {
        let index = self.relations.iter().position(|r| r.id == id)?;
        Some(self.relations.remove(index))
    }

    /// Replaces the value of a dimension; returns false for relations without a value.
    pub fn set_dimension(&mut self, id: RelationId, value: f64) -> bool {
        use Constraint::*;
        match self
            .relations
            .iter_mut()
            .find(|r| r.id == id)
            .map(|r| &mut r.constraint)
        {
            Some(
                Distance(_, _, v)
                | HorizontalDistance(_, _, v)
                | VerticalDistance(_, _, v)
                | Length(_, v)
                | Radius(_, v)
                | Angle(_, _, v),
            ) => {
                *v = value;
                true
            }
            _ => false,
        }
    }

    /// The dimensions of the sketch, in creation order.
    pub fn dimensions(&self) -> impl Iterator<Item = &Relation> {
        self.relations
            .iter()
            .filter(|r| r.constraint.is_dimension())
    }

    pub fn dimension_named(&self, name: &str) -> Option<&Relation> {
        self.relations
            .iter()
            .find(|r| r.name.as_deref() == Some(name))
    }

    /// Renames a dimension; the name must be valid and unused in this sketch.
    pub fn rename_dimension(&mut self, id: RelationId, name: &str) -> Result<(), SketchError> {
        let relation = self.relation(id).ok_or(SketchError::UnknownRelation(id))?;
        if !relation.constraint.is_dimension() || !is_valid_name(name) {
            return Err(SketchError::BadName(name.to_owned()));
        }
        if self.dimension_named(name).is_some_and(|r| r.id != id) {
            return Err(SketchError::BadName(name.to_owned()));
        }
        if let Some(r) = self.relations.iter_mut().find(|r| r.id == id) {
            r.name = Some(name.to_owned());
        }
        Ok(())
    }

    /// Deletes entities, along with the curves built on deleted points, the relations that
    /// refer to anything deleted, and end points no longer used by any curve.
    pub fn remove(&mut self, ids: &[EntityId]) {
        let mut doomed: std::collections::HashSet<EntityId> = ids.iter().copied().collect();
        // Curves standing on a deleted point go too.
        for e in &self.entities {
            if curve_points(&e.geometry).iter().any(|p| doomed.contains(p)) {
                doomed.insert(e.id);
            }
        }
        // Points that only served deleted curves go too.
        let candidates: Vec<EntityId> = self
            .entities
            .iter()
            .filter(|e| doomed.contains(&e.id))
            .flat_map(|e| curve_points(&e.geometry))
            .collect();
        for p in candidates {
            let still_used = self
                .entities
                .iter()
                .any(|e| !doomed.contains(&e.id) && curve_points(&e.geometry).contains(&p));
            if !still_used {
                doomed.insert(p);
            }
        }
        self.entities.retain(|e| !doomed.contains(&e.id));
        self.relations.retain(|r| {
            r.constraint
                .entities()
                .iter()
                .all(|id| !doomed.contains(id))
        });
    }

    /// Whether a `Fixed` relation holds this point.
    pub fn is_fixed(&self, point: EntityId) -> bool {
        self.relations
            .iter()
            .any(|r| matches!(r.constraint, Constraint::Fixed(p) if p == point))
    }

    /// Current length of a line.
    pub fn line_length(&self, line: EntityId) -> Option<f64> {
        let (a, b) = self.endpoints(line)?;
        Some(self.position(a)?.distance(self.position(b)?))
    }

    /// Current radius of a circle or arc.
    pub fn radius_of(&self, round: EntityId) -> Option<f64> {
        match self.entity(round)?.geometry {
            Geometry::Circle { radius, .. } => Some(radius),
            Geometry::Arc { center, start, .. } => {
                Some(self.position(center)?.distance(self.position(start)?))
            }
            _ => None,
        }
    }

    /// Current counter-clockwise angle from one line to another, in radians, in (−π, π].
    pub fn angle_between(&self, first: EntityId, second: EntityId) -> Option<f64> {
        let direction = |l| {
            let (a, b) = self.endpoints(l)?;
            let (a, b) = (self.position(a)?, self.position(b)?);
            Some((b.y - a.y).atan2(b.x - a.x))
        };
        let between = direction(second)? - direction(first)?;
        Some(-((PI - between).rem_euclid(TAU) - PI))
    }

    pub fn set_construction(&mut self, id: EntityId, construction: bool) {
        if let Some(e) = self.entities.iter_mut().find(|e| e.id == id) {
            e.construction = construction;
        }
    }

    /// Position of a point entity.
    pub fn position(&self, id: EntityId) -> Option<Point2> {
        match self.entity(id)?.geometry {
            Geometry::Point(p) => Some(p),
            _ => None,
        }
    }

    /// Moves a point, e.g. while dragging; solve afterwards to restore the relations.
    pub fn move_point(&mut self, id: EntityId, to: Point2) -> bool {
        match self.entities.iter_mut().find(|e| e.id == id) {
            Some(Entity {
                geometry: Geometry::Point(p),
                ..
            }) => {
                *p = to;
                true
            }
            _ => false,
        }
    }

    /// Changes a circle's radius, e.g. while dragging its edge; solve afterwards.
    pub fn set_circle_radius(&mut self, circle: EntityId, value: f64) -> bool {
        match self.entities.iter_mut().find(|e| e.id == circle) {
            Some(Entity {
                geometry: Geometry::Circle { radius, .. },
                ..
            }) => {
                *radius = value;
                true
            }
            _ => false,
        }
    }

    /// End points of a line.
    pub fn endpoints(&self, line: EntityId) -> Option<(EntityId, EntityId)> {
        match self.entity(line)?.geometry {
            Geometry::Line { start, end } => Some((start, end)),
            _ => None,
        }
    }

    /// Lines, arcs and circles that are not construction geometry, as profile curves
    /// (segment id = entity id), ready for `oxyde_geom::Profile::from_curves`.
    pub fn profile_segments(&self) -> Vec<ProfileSegment> {
        let at = |id| self.position(id).unwrap_or_default();
        self.entities
            .iter()
            .filter(|e| !e.construction)
            .filter_map(|e| {
                let curve = match e.geometry {
                    Geometry::Point(_) => return None,
                    Geometry::Line { start, end } => Curve2::Line {
                        start: at(start),
                        end: at(end),
                    },
                    Geometry::Circle { center, radius } => Curve2::Circle {
                        center: at(center),
                        radius,
                    },
                    Geometry::Arc { center, start, end } => {
                        arc_curve(at(center), at(start), at(end))
                    }
                };
                Some(ProfileSegment { id: e.id.0, curve })
            })
            .collect()
    }

    // ---- Unknowns and equations ------------------------------------------------------------

    /// Checks references and builds the parameter layout.
    pub(crate) fn layout(&self) -> Result<Layout, SketchError> {
        let mut layout = Layout::default();
        for e in &self.entities {
            match e.geometry {
                Geometry::Point(p) => {
                    layout.point.insert(e.id, layout.params.len());
                    layout.params.extend([p.x, p.y]);
                }
                Geometry::Circle { radius, .. } => {
                    layout.radius.insert(e.id, layout.params.len());
                    layout.params.push(radius);
                }
                _ => {}
            }
            layout.kind.insert(e.id, e.geometry);
        }
        // Entity references.
        for e in &self.entities {
            for p in curve_points(&e.geometry) {
                layout.expect(p, Kind::Point, None)?;
            }
        }
        // Constraint references.
        for (index, c) in self.relations.iter().map(|r| &r.constraint).enumerate() {
            use Constraint::*;
            let i = Some(index);
            match *c {
                Fixed(p) => layout.expect(p, Kind::Point, i)?,
                Coincident(a, b)
                | Distance(a, b, _)
                | HorizontalDistance(a, b, _)
                | VerticalDistance(a, b, _) => {
                    layout.expect(a, Kind::Point, i)?;
                    layout.expect(b, Kind::Point, i)?;
                }
                Horizontal(l) | Vertical(l) | Length(l, _) => layout.expect(l, Kind::Line, i)?,
                Parallel(a, b) | Perpendicular(a, b) | EqualLength(a, b) | Angle(a, b, _) => {
                    layout.expect(a, Kind::Line, i)?;
                    layout.expect(b, Kind::Line, i)?;
                }
                EqualRadius(a, b) => {
                    layout.expect(a, Kind::Round, i)?;
                    layout.expect(b, Kind::Round, i)?;
                }
                Radius(r, _) => layout.expect(r, Kind::Round, i)?,
                PointOnLine(p, l) | Midpoint(p, l) => {
                    layout.expect(p, Kind::Point, i)?;
                    layout.expect(l, Kind::Line, i)?;
                }
                PointOnCircle(p, r) => {
                    layout.expect(p, Kind::Point, i)?;
                    layout.expect(r, Kind::Round, i)?;
                }
                Tangent(l, r) => {
                    layout.expect(l, Kind::Line, i)?;
                    layout.expect(r, Kind::Round, i)?;
                }
            }
            if let Fixed(p) = *c {
                layout.fixed.push(p);
            }
        }
        Ok(layout)
    }

    /// Writes solved parameters back into the entities.
    pub(crate) fn store(&mut self, layout: &Layout, params: &[f64]) {
        for e in &mut self.entities {
            match &mut e.geometry {
                Geometry::Point(p) => {
                    let i = layout.point[&e.id];
                    *p = Point2::new(params[i], params[i + 1]);
                }
                Geometry::Circle { radius, .. } => *radius = params[layout.radius[&e.id]],
                _ => {}
            }
        }
    }

    /// Appends the residuals of every equation; `owners` receives, per residual, the index
    /// of its constraint (`None` for the implicit equal-radius equation of arcs).
    pub(crate) fn residuals(
        &self,
        layout: &Layout,
        params: &[f64],
        out: &mut Vec<f64>,
        owners: &mut Vec<Option<usize>>,
    ) {
        let eval = Eval { layout, params };
        for e in &self.entities {
            if let Geometry::Arc { center, start, end } = e.geometry {
                let c = eval.point(center);
                out.push(dist(c, eval.point(start)) - dist(c, eval.point(end)));
                owners.push(None);
            }
        }
        for (index, c) in self.relations.iter().map(|r| &r.constraint).enumerate() {
            let before = out.len();
            eval.constraint(c, out);
            owners.extend(std::iter::repeat_n(Some(index), out.len() - before));
        }
    }
}

/// The number in a default dimension name ("D12" → 12).
fn default_number(name: &str) -> Option<u32> {
    name.strip_prefix('D')?.parse().ok()
}

/// Points a curve stands on (none for a point).
pub(crate) fn curve_points(geometry: &Geometry) -> Vec<EntityId> {
    match *geometry {
        Geometry::Point(_) => Vec::new(),
        Geometry::Line { start, end } => vec![start, end],
        Geometry::Circle { center, .. } => vec![center],
        Geometry::Arc { center, start, end } => vec![center, start, end],
    }
}

/// Counter-clockwise arc from centre-based to three-point form.
fn arc_curve(center: Point2, start: Point2, end: Point2) -> Curve2 {
    let a0 = (start.y - center.y).atan2(start.x - center.x);
    let a1 = (end.y - center.y).atan2(end.x - center.x);
    let mut sweep = (a1 - a0).rem_euclid(TAU);
    if sweep < 1e-12 {
        sweep = TAU;
    }
    let r = (dist(center, start) + dist(center, end)) / 2.0;
    let am = a0 + sweep / 2.0;
    Curve2::Arc {
        start,
        mid: Point2::new(center.x + r * am.cos(), center.y + r * am.sin()),
        end,
    }
}

fn dist(a: Point2, b: Point2) -> f64 {
    a.distance(b)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Point,
    Line,
    /// Circle or arc.
    Round,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Point => "point",
            Kind::Line => "line",
            Kind::Round => "circle or arc",
        }
    }

    fn matches(self, geometry: &Geometry) -> bool {
        matches!(
            (self, geometry),
            (Kind::Point, Geometry::Point(_))
                | (Kind::Line, Geometry::Line { .. })
                | (Kind::Round, Geometry::Circle { .. } | Geometry::Arc { .. })
        )
    }
}

/// Where each entity's unknowns live in the parameter vector.
#[derive(Debug, Default)]
pub(crate) struct Layout {
    /// All parameters, initial values.
    pub params: Vec<f64>,
    /// Index of a point's x (y follows).
    pub point: HashMap<EntityId, usize>,
    /// Index of a circle's radius.
    pub radius: HashMap<EntityId, usize>,
    pub kind: HashMap<EntityId, Geometry>,
    /// Points held by a `Fixed` constraint: their parameters are not unknowns.
    pub fixed: Vec<EntityId>,
}

impl Layout {
    fn expect(
        &self,
        id: EntityId,
        kind: Kind,
        constraint: Option<usize>,
    ) -> Result<(), SketchError> {
        let geometry = self.kind.get(&id).ok_or(SketchError::UnknownEntity(id))?;
        if kind.matches(geometry) {
            Ok(())
        } else {
            Err(SketchError::WrongKind {
                constraint,
                entity: id,
                expected: kind.name(),
            })
        }
    }

    /// Indices of the parameters the solver may change.
    pub fn free_params(&self) -> Vec<usize> {
        let mut fixed = vec![false; self.params.len()];
        for p in &self.fixed {
            let i = self.point[p];
            fixed[i] = true;
            fixed[i + 1] = true;
        }
        (0..self.params.len()).filter(|&i| !fixed[i]).collect()
    }

    /// Parameters an entity depends on.
    pub fn params_of(&self, id: EntityId) -> Vec<usize> {
        let point = |p: EntityId| {
            let i = self.point[&p];
            [i, i + 1]
        };
        match self.kind[&id] {
            Geometry::Point(_) => point(id).to_vec(),
            Geometry::Line { start, end } => [point(start), point(end)].concat(),
            Geometry::Circle { center, .. } => {
                let mut v = point(center).to_vec();
                v.push(self.radius[&id]);
                v
            }
            Geometry::Arc { center, start, end } => {
                [point(center), point(start), point(end)].concat()
            }
        }
    }
}

struct Eval<'a> {
    layout: &'a Layout,
    params: &'a [f64],
}

impl Eval<'_> {
    fn point(&self, id: EntityId) -> Point2 {
        let i = self.layout.point[&id];
        Point2::new(self.params[i], self.params[i + 1])
    }

    fn line(&self, id: EntityId) -> (Point2, Point2) {
        match self.layout.kind[&id] {
            Geometry::Line { start, end } => (self.point(start), self.point(end)),
            _ => unreachable!("validated by layout()"),
        }
    }

    /// Centre and radius of a circle or arc.
    fn round(&self, id: EntityId) -> (Point2, f64) {
        match self.layout.kind[&id] {
            Geometry::Circle { center, .. } => {
                (self.point(center), self.params[self.layout.radius[&id]])
            }
            Geometry::Arc { center, start, .. } => {
                let c = self.point(center);
                (c, dist(c, self.point(start)))
            }
            _ => unreachable!("validated by layout()"),
        }
    }

    fn constraint(&self, c: &Constraint, out: &mut Vec<f64>) {
        use Constraint::*;
        let direction = |l| {
            let (s, e) = self.line(l);
            (e.x - s.x, e.y - s.y)
        };
        let length = |(x, y): (f64, f64)| (x * x + y * y).sqrt().max(1e-12);
        match *c {
            // Fixed points are not unknowns; nothing to solve.
            Fixed(_) => {}
            Coincident(a, b) => {
                let (a, b) = (self.point(a), self.point(b));
                out.extend([a.x - b.x, a.y - b.y]);
            }
            Horizontal(l) => out.push(direction(l).1),
            Vertical(l) => out.push(direction(l).0),
            Parallel(a, b) => {
                let (u, v) = (direction(a), direction(b));
                out.push((u.0 * v.1 - u.1 * v.0) / (length(u) * length(v)));
            }
            Perpendicular(a, b) => {
                let (u, v) = (direction(a), direction(b));
                out.push((u.0 * v.0 + u.1 * v.1) / (length(u) * length(v)));
            }
            EqualLength(a, b) => out.push(length(direction(a)) - length(direction(b))),
            EqualRadius(a, b) => out.push(self.round(a).1 - self.round(b).1),
            Distance(a, b, d) => out.push(dist(self.point(a), self.point(b)) - d),
            HorizontalDistance(a, b, d) => out.push((self.point(b).x - self.point(a).x).abs() - d),
            VerticalDistance(a, b, d) => out.push((self.point(b).y - self.point(a).y).abs() - d),
            Length(l, d) => out.push(length(direction(l)) - d),
            Radius(r, value) => out.push(self.round(r).1 - value),
            Angle(a, b, angle) => {
                let (u, v) = (direction(a), direction(b));
                let between = v.1.atan2(v.0) - u.1.atan2(u.0) - angle;
                out.push((between + PI).rem_euclid(TAU) - PI);
            }
            PointOnLine(p, l) => {
                let (s, e) = self.line(l);
                let p = self.point(p);
                let d = (e.x - s.x, e.y - s.y);
                out.push((d.0 * (p.y - s.y) - d.1 * (p.x - s.x)) / length(d));
            }
            PointOnCircle(p, r) => {
                let (c, radius) = self.round(r);
                out.push(dist(self.point(p), c) - radius);
            }
            Midpoint(p, l) => {
                let (s, e) = self.line(l);
                let p = self.point(p);
                out.extend([p.x - (s.x + e.x) / 2.0, p.y - (s.y + e.y) / 2.0]);
            }
            Tangent(l, r) => {
                let (s, e) = self.line(l);
                let (c, radius) = self.round(r);
                let d = (e.x - s.x, e.y - s.y);
                let distance = (d.0 * (c.y - s.y) - d.1 * (c.x - s.x)).abs() / length(d);
                out.push(distance - radius);
            }
        }
    }
}
