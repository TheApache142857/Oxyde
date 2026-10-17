//! Features: the entries of a part's timeline.

use std::collections::BTreeMap;
use std::fmt;

use oxyde_geom::{Axis, Plane, Point2};
use oxyde_sketch::ConstrainedSketch;

use crate::dimensions::DimensionUnit;
use crate::naming::{EdgeName, FaceName};

/// Stable identity of a feature within a part. Never reused, even after deletion, and
/// unaffected by renaming or reordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FeatureId(u32);

impl FeatureId {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Display for FeatureId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "F{}", self.0)
    }
}

/// Stable identity of a solid body of a part: the feature that created it, and which of
/// the bodies that feature created (a pattern of a new-body feature makes several).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BodyId {
    pub feature: FeatureId,
    pub index: u32,
}

impl BodyId {
    pub const fn new(feature: FeatureId, index: u32) -> Self {
        Self { feature, index }
    }
}

impl fmt::Display for BodyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.feature, self.index)
    }
}

/// A numeric parameter of a feature that is a dimension in its own right, e.g. the depth
/// of an extrusion ("D1@Boss-Extrude1").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FeatureParameter {
    /// Extrusion depth.
    Depth,
    /// Fillet radius.
    Radius,
    /// Chamfer distance.
    Distance,
    /// Revolve angle, or total angle of a circular pattern.
    Angle,
    /// Shell wall thickness.
    Thickness,
    /// Distance of a reference plane from its base.
    Offset,
    /// Spacing of a linear pattern, first direction.
    Spacing,
    /// Number of instances of a pattern (first direction), the seed included.
    Count,
    /// Spacing of a linear pattern, second direction.
    Spacing2,
    /// Number of instances in the second direction of a linear pattern.
    Count2,
}

const PARAMETERS: [(FeatureParameter, &str, &str); 10] = [
    (FeatureParameter::Depth, "depth", "Depth"),
    (FeatureParameter::Radius, "radius", "Radius"),
    (FeatureParameter::Distance, "distance", "Distance"),
    (FeatureParameter::Angle, "angle", "Angle"),
    (FeatureParameter::Thickness, "thickness", "Thickness"),
    (FeatureParameter::Offset, "offset", "Offset"),
    (FeatureParameter::Spacing, "spacing", "Spacing"),
    (FeatureParameter::Count, "count", "Instances"),
    (
        FeatureParameter::Spacing2,
        "spacing2",
        "Spacing (direction 2)",
    ),
    (
        FeatureParameter::Count2,
        "count2",
        "Instances (direction 2)",
    ),
];

impl FeatureParameter {
    /// Key used in files.
    pub fn key(self) -> &'static str {
        PARAMETERS
            .iter()
            .find(|(p, ..)| *p == self)
            .map_or("", |(_, key, _)| key)
    }

    pub fn from_key(key: &str) -> Option<Self> {
        PARAMETERS
            .iter()
            .find(|(_, k, _)| *k == key)
            .map(|(p, ..)| *p)
    }

    /// Whether the value is a length, an angle or a number of instances.
    pub fn unit(self) -> DimensionUnit {
        match self {
            FeatureParameter::Angle => DimensionUnit::Angle,
            FeatureParameter::Count | FeatureParameter::Count2 => DimensionUnit::Count,
            _ => DimensionUnit::Length,
        }
    }

    /// What the parameter is called in the user interface.
    pub fn label(self) -> &'static str {
        PARAMETERS
            .iter()
            .find(|(p, ..)| *p == self)
            .map_or("", |(.., label)| label)
    }
}

/// One entry of the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    pub(crate) id: FeatureId,
    pub(crate) name: String,
    pub(crate) suppressed: bool,
    pub(crate) kind: FeatureKind,
    /// User names of feature dimensions; absent = default name ("D1").
    pub(crate) parameter_names: BTreeMap<FeatureParameter, String>,
}

impl Feature {
    /// Builds a feature with a given identity, e.g. when loading a file. Within a part, use
    /// `Part::add_feature`, which assigns ids and names.
    pub fn new(
        id: FeatureId,
        name: impl Into<String>,
        suppressed: bool,
        kind: FeatureKind,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            suppressed,
            kind,
            parameter_names: BTreeMap::new(),
        }
    }

    /// Sets the user names of feature dimensions (e.g. when loading a file).
    pub fn with_parameter_names(mut self, names: BTreeMap<FeatureParameter, String>) -> Self {
        self.parameter_names = names;
        self
    }

    /// User names of feature dimensions that differ from the default.
    pub fn parameter_names(&self) -> &BTreeMap<FeatureParameter, String> {
        &self.parameter_names
    }

    /// The name of one of the feature's dimensions ("D1", "D2"… unless renamed).
    pub fn parameter_name(&self, parameter: FeatureParameter) -> String {
        self.parameter_names
            .get(&parameter)
            .cloned()
            .unwrap_or_else(|| self.kind.default_parameter_name(parameter))
    }

    /// After the definition changed: forgets user names of dimensions it no longer has, and
    /// user names that now clash with another dimension's default name ("D3" given to the
    /// spacing before a second direction brought its own D3).
    pub(crate) fn tidy_parameter_names(&mut self) {
        let parameters: Vec<FeatureParameter> =
            self.kind.parameters().iter().map(|(p, _)| *p).collect();
        self.parameter_names.retain(|p, _| parameters.contains(p));
        // Each pass drops one clashing name, back to its default (defaults never clash).
        loop {
            let clash = self.parameter_names.iter().find_map(|(p, name)| {
                parameters
                    .iter()
                    .any(|q| q != p && self.parameter_name(*q) == *name)
                    .then_some(*p)
            });
            match clash {
                Some(p) => {
                    self.parameter_names.remove(&p);
                }
                None => break,
            }
        }
    }

    pub fn id(&self) -> FeatureId {
        self.id
    }

    /// Display name, e.g. "Boss-Extrude1". Unique within the part; it appears in dimension
    /// names ("D1@Boss-Extrude1"), but references are always stored by [`FeatureId`], so
    /// renaming a feature never breaks anything.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_suppressed(&self) -> bool {
        self.suppressed
    }

    pub fn kind(&self) -> &FeatureKind {
        &self.kind
    }
}

/// The contents of an imported file. Copies share the bytes, so that undo snapshots of the
/// timeline stay cheap.
#[derive(Clone, PartialEq)]
pub struct FileData(std::sync::Arc<Vec<u8>>);

impl FileData {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(std::sync::Arc::new(bytes))
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for FileData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FileData({} bytes)", self.0.len())
    }
}

/// Solid geometry imported from a STEP file ("Imported1"). The file's contents are kept in
/// the part, so the part no longer needs the original file. Added to the body like a boss.
#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    /// Name of the original file, for display.
    pub file_name: String,
    pub data: FileData,
    /// Whether the colours the file gives its solids and faces are still to be turned
    /// into appearances of the part: true for a fresh import, false once the first
    /// rebuild has done it (the appearances are then the user's to change or remove).
    pub apply_colors: bool,
}

impl Import {
    /// A fresh import of `data`, whose colours become appearances on the first rebuild.
    pub fn new(file_name: impl Into<String>, data: FileData) -> Self {
        Self {
            file_name: file_name.into(),
            data,
            apply_colors: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FeatureKind {
    Sketch(Sketch),
    Extrude(Extrude),
    Revolve(Revolve),
    Sweep(Sweep),
    Fillet(EdgeBlend),
    Chamfer(EdgeBlend),
    Shell(Shell),
    /// Reference plane.
    Plane(RefPlane),
    /// Reference axis.
    Axis(RefAxis),
    LinearPattern(LinearPattern),
    CircularPattern(CircularPattern),
    Mirror(Mirror),
    Import(Import),
    Combine(Combine),
}

impl FeatureKind {
    /// SolidWorks-style type name, used for automatic feature names.
    pub fn type_name(&self) -> &'static str {
        match self {
            FeatureKind::Sketch(_) => "Sketch",
            FeatureKind::Extrude(e) => match e.op {
                ExtrudeOp::Boss | ExtrudeOp::NewBody => "Boss-Extrude",
                ExtrudeOp::Cut => "Cut-Extrude",
                ExtrudeOp::Intersect => "Intersect-Extrude",
            },
            FeatureKind::Revolve(r) => match r.op {
                ExtrudeOp::Boss | ExtrudeOp::NewBody => "Revolve",
                ExtrudeOp::Cut => "Cut-Revolve",
                ExtrudeOp::Intersect => "Intersect-Revolve",
            },
            FeatureKind::Sweep(s) => match s.op {
                ExtrudeOp::Boss | ExtrudeOp::NewBody => "Sweep",
                ExtrudeOp::Cut => "Cut-Sweep",
                ExtrudeOp::Intersect => "Intersect-Sweep",
            },
            FeatureKind::Fillet(_) => "Fillet",
            FeatureKind::Chamfer(_) => "Chamfer",
            FeatureKind::Shell(_) => "Shell",
            FeatureKind::Plane(_) => "Plane",
            FeatureKind::Axis(_) => "Axis",
            FeatureKind::LinearPattern(_) => "LPattern",
            FeatureKind::CircularPattern(_) => "CirPattern",
            FeatureKind::Mirror(_) => "Mirror",
            FeatureKind::Import(_) => "Imported",
            FeatureKind::Combine(_) => "Combine",
        }
    }

    /// The feature's own dimensions and their values (mm, radians, or counts). Optional
    /// parameters come last, so that default names ("D1", "D2"…) never shift.
    pub fn parameters(&self) -> Vec<(FeatureParameter, f64)> {
        use FeatureParameter as P;
        match self {
            FeatureKind::Sketch(_)
            | FeatureKind::Sweep(_)
            | FeatureKind::Axis(_)
            | FeatureKind::Mirror(_)
            | FeatureKind::Import(_)
            | FeatureKind::Combine(_) => Vec::new(),
            FeatureKind::Extrude(e) => vec![(P::Depth, e.extent.depth())],
            FeatureKind::Revolve(r) => vec![(P::Angle, r.angle)],
            FeatureKind::Fillet(b) => vec![(P::Radius, b.size)],
            FeatureKind::Chamfer(b) => vec![(P::Distance, b.size)],
            FeatureKind::Shell(s) => vec![(P::Thickness, s.thickness)],
            FeatureKind::Plane(p) => vec![(P::Offset, p.offset)],
            FeatureKind::LinearPattern(l) => {
                let mut out = vec![
                    (P::Spacing, l.first.spacing),
                    (P::Count, f64::from(l.first.count)),
                ];
                if let Some(second) = &l.second {
                    out.push((P::Spacing2, second.spacing));
                    out.push((P::Count2, f64::from(second.count)));
                }
                out
            }
            FeatureKind::CircularPattern(c) => {
                vec![(P::Angle, c.angle), (P::Count, f64::from(c.count))]
            }
        }
    }

    /// Every parameter this kind of feature can have, optional ones included, in the order
    /// of [`FeatureKind::parameters`]; so the one at position `i` is named "D{i+1}" by
    /// default whenever it exists.
    pub(crate) fn possible_parameters(&self) -> Vec<FeatureParameter> {
        use FeatureParameter as P;
        match self {
            FeatureKind::LinearPattern(_) => vec![P::Spacing, P::Count, P::Spacing2, P::Count2],
            other => other.parameters().into_iter().map(|(p, _)| p).collect(),
        }
    }

    /// Default name of one of the feature's dimensions, numbered in the order of
    /// [`FeatureKind::parameters`] as in SolidWorks: "D1", "D2"…
    pub fn default_parameter_name(&self, parameter: FeatureParameter) -> String {
        let index = self
            .parameters()
            .iter()
            .position(|(p, _)| *p == parameter)
            .unwrap_or(0);
        format!("D{}", index + 1)
    }

    /// Changes one of the feature's dimensions; false if the feature has no such parameter.
    /// Counts are rounded, and at least 1.
    pub fn set_parameter(&mut self, parameter: FeatureParameter, value: f64) -> bool {
        use FeatureParameter as P;
        let count = || value.round().max(1.0).min(f64::from(u32::MAX)) as u32;
        match (self, parameter) {
            (FeatureKind::Extrude(e), P::Depth) => {
                e.extent = match e.extent {
                    ExtrudeExtent::Blind { .. } => ExtrudeExtent::Blind { depth: value },
                    ExtrudeExtent::MidPlane { .. } => ExtrudeExtent::MidPlane { depth: value },
                };
            }
            (FeatureKind::Revolve(r), P::Angle) => r.angle = value,
            (FeatureKind::Fillet(b), P::Radius) | (FeatureKind::Chamfer(b), P::Distance) => {
                b.size = value;
            }
            (FeatureKind::Shell(s), P::Thickness) => s.thickness = value,
            (FeatureKind::Plane(p), P::Offset) => p.offset = value,
            (FeatureKind::LinearPattern(l), P::Spacing) => l.first.spacing = value,
            (FeatureKind::LinearPattern(l), P::Count) => l.first.count = count(),
            (FeatureKind::LinearPattern(l), P::Spacing2) => match &mut l.second {
                Some(second) => second.spacing = value,
                None => return false,
            },
            (FeatureKind::LinearPattern(l), P::Count2) => match &mut l.second {
                Some(second) => second.count = count(),
                None => return false,
            },
            (FeatureKind::CircularPattern(c), P::Angle) => c.angle = value,
            (FeatureKind::CircularPattern(c), P::Count) => c.count = count(),
            _ => return false,
        }
        true
    }

    /// Features this one refers to: its sketch, its seeds, its reference geometry, or the
    /// features that created the faces and edges it is attached to. Sorted, without
    /// duplicates.
    pub fn dependencies(&self) -> Vec<FeatureId> {
        let mut out = Vec::new();
        let faces = |faces: &[FaceName], out: &mut Vec<FeatureId>| {
            faces.iter().for_each(|f| f.key.collect_features(out));
        };
        match self {
            FeatureKind::Sketch(s) => match &s.plane {
                SketchPlane::Standard(_) => {}
                SketchPlane::Face(face) => face.key.collect_features(&mut out),
                SketchPlane::Reference(plane) => out.push(*plane),
            },
            FeatureKind::Extrude(e) => {
                out.push(e.sketch);
                out.extend(e.bodies.iter().map(|b| b.feature));
            }
            FeatureKind::Revolve(r) => {
                out.push(r.sketch);
                out.extend(r.bodies.iter().map(|b| b.feature));
                if let RevolveAxis::Axis(axis) = &r.axis {
                    axis.collect_features(&mut out);
                }
            }
            FeatureKind::Sweep(s) => {
                out.push(s.profile);
                out.push(s.path);
                out.extend(s.bodies.iter().map(|b| b.feature));
            }
            FeatureKind::Fillet(b) | FeatureKind::Chamfer(b) => {
                for edge in &b.edges {
                    faces(&edge.faces, &mut out);
                }
            }
            FeatureKind::Shell(s) => faces(&s.faces, &mut out),
            FeatureKind::Plane(p) => p.base.collect_features(&mut out),
            FeatureKind::Axis(RefAxis::Along(axis)) => axis.collect_features(&mut out),
            FeatureKind::Axis(RefAxis::TwoPlanes(a, b)) => {
                a.collect_features(&mut out);
                b.collect_features(&mut out);
            }
            FeatureKind::LinearPattern(l) => {
                out.extend(&l.seeds);
                l.first.axis.collect_features(&mut out);
                if let Some(second) = &l.second {
                    second.axis.collect_features(&mut out);
                }
            }
            FeatureKind::CircularPattern(c) => {
                out.extend(&c.seeds);
                c.axis.collect_features(&mut out);
            }
            FeatureKind::Mirror(m) => {
                out.extend(&m.seeds);
                m.plane.collect_features(&mut out);
            }
            FeatureKind::Import(_) => {}
            FeatureKind::Combine(c) => {
                out.push(c.target.feature);
                out.extend(c.tools.iter().map(|b| b.feature));
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Whether the feature can be the seed of a pattern or a mirror: it adds or removes
    /// material with a tool body.
    pub fn is_patternable(&self) -> bool {
        matches!(
            self,
            FeatureKind::Extrude(_) | FeatureKind::Revolve(_) | FeatureKind::Sweep(_)
        )
    }

    /// The sketches the feature sweeps, if it is an extrusion, a revolve or a sweep: its
    /// profile first. The timeline shows them under the feature.
    pub fn swept_sketches(&self) -> Vec<FeatureId> {
        match self {
            FeatureKind::Extrude(e) => vec![e.sketch],
            FeatureKind::Revolve(r) => vec![r.sketch],
            FeatureKind::Sweep(s) => vec![s.profile, s.path],
            _ => Vec::new(),
        }
    }

    /// What the feature does with the solid it sweeps, if it sweeps one.
    pub fn sweep_op(&self) -> Option<ExtrudeOp> {
        match self {
            FeatureKind::Extrude(e) => Some(e.op),
            FeatureKind::Revolve(r) => Some(r.op),
            FeatureKind::Sweep(s) => Some(s.op),
            _ => None,
        }
    }
}

/// The three reference planes every part starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandardPlane {
    Front,
    Top,
    Right,
}

impl StandardPlane {
    pub fn plane(self) -> Plane {
        match self {
            StandardPlane::Front => Plane::FRONT,
            StandardPlane::Top => Plane::TOP,
            StandardPlane::Right => Plane::RIGHT,
        }
    }
}

/// The three model axes (SolidWorks shows them as temporary axes through the origin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardAxis {
    X,
    Y,
    Z,
}

impl StandardAxis {
    pub fn axis(self) -> Axis {
        match self {
            StandardAxis::X => Axis::X,
            StandardAxis::Y => Axis::Y,
            StandardAxis::Z => Axis::Z,
        }
    }
}

/// Where a sketch is drawn.
#[derive(Debug, Clone, PartialEq)]
pub enum SketchPlane {
    Standard(StandardPlane),
    /// A planar face of the body as it is just before the sketch in the timeline.
    Face(FaceName),
    /// A reference plane feature.
    Reference(FeatureId),
}

/// A plane picked for a feature: mirror plane, base of a reference plane…
#[derive(Debug, Clone, PartialEq)]
pub enum PlaneRef {
    Standard(StandardPlane),
    /// A planar face of the body.
    Face(FaceName),
    /// A reference plane feature.
    Reference(FeatureId),
}

impl PlaneRef {
    pub fn collect_features(&self, out: &mut Vec<FeatureId>) {
        match self {
            PlaneRef::Standard(_) => {}
            PlaneRef::Face(face) => face.key.collect_features(out),
            PlaneRef::Reference(id) => out.push(*id),
        }
    }
}

/// An axis or a direction picked for a feature: revolve axis, pattern direction…
#[derive(Debug, Clone, PartialEq)]
pub enum AxisRef {
    Standard(StandardAxis),
    /// A reference axis feature.
    Reference(FeatureId),
    /// A straight edge (its line) or a circular edge (its axis).
    Edge(EdgeName),
    /// The axis of a cylindrical or conical face, or the normal of a planar face.
    Face(FaceName),
}

impl AxisRef {
    pub fn collect_features(&self, out: &mut Vec<FeatureId>) {
        match self {
            AxisRef::Standard(_) => {}
            AxisRef::Reference(id) => out.push(*id),
            AxisRef::Edge(edge) => edge.faces.iter().for_each(|f| f.key.collect_features(out)),
            AxisRef::Face(face) => face.key.collect_features(out),
        }
    }
}

/// A 2D sketch: geometry with relations and dimensions, on a plane or a face.
///
/// The ids of its lines, arcs and circles name the faces that extrusions sweep from them.
#[derive(Debug, Clone, PartialEq)]
pub struct Sketch {
    pub plane: SketchPlane,
    pub geometry: ConstrainedSketch,
}

impl Sketch {
    pub fn new(plane: SketchPlane) -> Self {
        Self {
            plane,
            geometry: ConstrainedSketch::new(),
        }
    }

    /// A line with two new end points; returns the line's id.
    pub fn line(&mut self, start: Point2, end: Point2) -> u32 {
        self.geometry.line(start, end).0
    }

    /// A circle with a new centre point; returns the circle's id.
    pub fn circle(&mut self, center: Point2, radius: f64) -> u32 {
        self.geometry.circle(center, radius).0
    }

    /// An axis-aligned rectangle with its horizontal and vertical relations. Returns the
    /// ids of its bottom, right, top and left lines.
    pub fn rectangle(&mut self, corner: Point2, width: f64, height: f64) -> [u32; 4] {
        self.geometry
            .rectangle(corner, width, height)
            .map(|id| id.0)
    }
}

/// What an extrusion, a revolve or a sweep does with the solid it sweeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtrudeOp {
    /// Adds material: joined to a body (the first boss of a part makes the first body).
    Boss,
    /// Removes material from bodies.
    Cut,
    /// Keeps only what the sweep and a body share.
    Intersect,
    /// A separate body, joined to nothing.
    NewBody,
}

impl ExtrudeOp {
    /// Whether the feature adds material in some way (its sweep goes along the sketch
    /// normal by default; cuts go against it).
    pub fn adds(self) -> bool {
        matches!(self, ExtrudeOp::Boss | ExtrudeOp::NewBody)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExtrudeExtent {
    /// From the sketch plane, `depth` in one direction.
    Blind { depth: f64 },
    /// `depth` in total, centred on the sketch plane.
    MidPlane { depth: f64 },
}

impl ExtrudeExtent {
    pub fn depth(self) -> f64 {
        match self {
            ExtrudeExtent::Blind { depth } | ExtrudeExtent::MidPlane { depth } => depth,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Extrude {
    pub sketch: FeatureId,
    pub op: ExtrudeOp,
    pub extent: ExtrudeExtent,
    /// Blind direction: bosses go along the sketch normal and cuts against it (into the
    /// face they are sketched on); `reverse` flips that.
    pub reverse: bool,
    /// Bodies it joins, cuts or intersects; empty = automatic (see [`ExtrudeOp`]): a join
    /// or intersection goes to the body the sketch is on, else the newest body; a cut goes
    /// through every body.
    pub bodies: Vec<BodyId>,
}

impl Extrude {
    pub fn blind(sketch: FeatureId, op: ExtrudeOp, depth: f64) -> Self {
        Self {
            sketch,
            op,
            extent: ExtrudeExtent::Blind { depth },
            reverse: false,
            bodies: Vec::new(),
        }
    }
}

/// Fillet (`size` = radius) or chamfer (`size` = distance) on a set of edges.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeBlend {
    pub edges: Vec<EdgeName>,
    pub size: f64,
}

/// What a revolve turns about.
#[derive(Debug, Clone, PartialEq)]
pub enum RevolveAxis {
    /// A line of the revolve's own sketch (usually a construction centerline), by entity id.
    SketchLine(u32),
    Axis(AxisRef),
}

/// Revolved boss or cut: the sketch's closed regions turned about an axis in its plane.
#[derive(Debug, Clone, PartialEq)]
pub struct Revolve {
    pub sketch: FeatureId,
    pub op: ExtrudeOp,
    pub axis: RevolveAxis,
    /// Radians, more than 0 and at most 2π.
    pub angle: f64,
    /// Half the angle on each side of the sketch plane.
    pub mid_plane: bool,
    /// Turn the other way.
    pub reverse: bool,
    /// Bodies it joins, cuts or intersects; empty = automatic, as for [`Extrude`].
    pub bodies: Vec<BodyId>,
}

/// Swept boss or cut: the closed regions of one sketch carried along the open or closed
/// chain of curves of another (SolidWorks' Sweep with "follow path").
#[derive(Debug, Clone, PartialEq)]
pub struct Sweep {
    /// The sketch whose closed regions are swept.
    pub profile: FeatureId,
    /// The sketch whose curves make the path: one chain, open or closed.
    pub path: FeatureId,
    pub op: ExtrudeOp,
    /// Bodies it joins, cuts or intersects; empty = automatic, as for [`Extrude`].
    pub bodies: Vec<BodyId>,
}

/// How a Combine feature puts bodies together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombineOp {
    /// The tools are added to the target.
    Join,
    /// The tools are removed from the target.
    Cut,
    /// Only what the target and the tools share is kept.
    Intersect,
}

/// Boolean operation between bodies of the part (Fusion's Combine): the target body is
/// changed by the tool bodies, which disappear unless kept.
#[derive(Debug, Clone, PartialEq)]
pub struct Combine {
    pub target: BodyId,
    pub tools: Vec<BodyId>,
    pub op: CombineOp,
    pub keep_tools: bool,
}

/// Hollows the part: removes the selected faces and leaves walls of the given thickness.
#[derive(Debug, Clone, PartialEq)]
pub struct Shell {
    /// Faces to remove (open). None makes a closed hollow part.
    pub faces: Vec<FaceName>,
    pub thickness: f64,
    /// Walls outside the original faces instead of inside.
    pub outward: bool,
}

/// Reference plane at a distance from another plane or a planar face.
#[derive(Debug, Clone, PartialEq)]
pub struct RefPlane {
    pub base: PlaneRef,
    /// Distance along the base normal (mm).
    pub offset: f64,
    /// Offset to the other side.
    pub flip: bool,
}

/// Reference axis.
#[derive(Debug, Clone, PartialEq)]
pub enum RefAxis {
    /// Along an edge, a face's axis, a model axis…
    Along(AxisRef),
    /// Where two planes meet.
    TwoPlanes(PlaneRef, PlaneRef),
}

/// One direction of a linear pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternDirection {
    pub axis: AxisRef,
    /// Distance between instances (mm).
    pub spacing: f64,
    /// Number of instances in this direction, the seed included.
    pub count: u32,
    pub reverse: bool,
}

/// Copies of features along one or two directions.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearPattern {
    /// Extrusions and revolves to copy.
    pub seeds: Vec<FeatureId>,
    pub first: PatternDirection,
    pub second: Option<PatternDirection>,
}

/// Copies of features around an axis.
#[derive(Debug, Clone, PartialEq)]
pub struct CircularPattern {
    pub seeds: Vec<FeatureId>,
    pub axis: AxisRef,
    /// Radians: the total angle with equal spacing, else the angle between instances.
    pub angle: f64,
    /// Number of instances, the seed included.
    pub count: u32,
    pub equal_spacing: bool,
    pub reverse: bool,
}

/// Mirror image of features through a plane.
#[derive(Debug, Clone, PartialEq)]
pub struct Mirror {
    pub seeds: Vec<FeatureId>,
    pub plane: PlaneRef,
}
