//! Features: the entries of a part's timeline.

use std::collections::BTreeMap;
use std::fmt;

use oxyde_geom::{Axis, Plane, Point2};
use oxyde_sketch::ConstrainedSketch;

use crate::dimensions::DimensionUnit;
use crate::naming::{EdgeName, FaceName, VertexName};

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
    /// Diameter of a hole.
    Diameter,
    /// Diameter of a hole's counterbore or countersink.
    CounterDiameter,
    /// Depth of a hole's counterbore.
    CounterDepth,
    /// A chamfer's distance on its other side.
    Distance2,
}

const PARAMETERS: [(FeatureParameter, &str, &str); 14] = [
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
    (FeatureParameter::Diameter, "diameter", "Diameter"),
    (
        FeatureParameter::CounterDiameter,
        "counter_diameter",
        "Counterbore or countersink diameter",
    ),
    (
        FeatureParameter::CounterDepth,
        "counter_depth",
        "Counterbore depth",
    ),
    (FeatureParameter::Distance2, "distance2", "Second distance"),
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
    Hole(Hole),
    Loft(Loft),
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
            FeatureKind::Hole(_) => "Hole",
            FeatureKind::Loft(l) => match l.op {
                ExtrudeOp::Boss | ExtrudeOp::NewBody => "Loft",
                ExtrudeOp::Cut => "Cut-Loft",
                ExtrudeOp::Intersect => "Intersect-Loft",
            },
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
            | FeatureKind::Combine(_)
            | FeatureKind::Loft(_) => Vec::new(),
            FeatureKind::Extrude(e) => e
                .extent
                .depth()
                .map(|d| (P::Depth, d))
                .into_iter()
                .collect(),
            FeatureKind::Revolve(r) => vec![(P::Angle, r.angle)],
            FeatureKind::Fillet(b) => vec![(P::Radius, b.size)],
            FeatureKind::Chamfer(b) => {
                let mut out = vec![(P::Distance, b.size)];
                match b.chamfer {
                    ChamferShape::Equal => {}
                    ChamferShape::TwoDistances { distance, .. } => {
                        out.push((P::Distance2, distance));
                    }
                    ChamferShape::AngleDistance { angle, .. } => out.push((P::Angle, angle)),
                }
                out
            }
            FeatureKind::Shell(s) => vec![(P::Thickness, s.thickness)],
            FeatureKind::Plane(p) => match p {
                RefPlane::Offset { offset, .. } => vec![(P::Offset, *offset)],
                RefPlane::Angle { angle, .. } => vec![(P::Angle, *angle)],
                RefPlane::ThroughLine { .. }
                | RefPlane::ThreePoints { .. }
                | RefPlane::NormalToCurve { .. } => Vec::new(),
            },
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
            FeatureKind::Hole(h) => {
                let mut out = vec![(P::Diameter, h.diameter)];
                match h.kind {
                    HoleKind::Simple => {}
                    HoleKind::Counterbore { diameter, depth } => {
                        out.push((P::CounterDiameter, diameter));
                        out.push((P::CounterDepth, depth));
                    }
                    HoleKind::Countersink { diameter, angle } => {
                        out.push((P::CounterDiameter, diameter));
                        out.push((P::Angle, angle));
                    }
                }
                out.extend(h.depth.map(|d| (P::Depth, d)));
                out
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
            FeatureKind::Extrude(_) => vec![P::Depth],
            FeatureKind::Chamfer(_) => vec![P::Distance, P::Distance2, P::Angle],
            FeatureKind::Hole(_) => vec![
                P::Diameter,
                P::CounterDiameter,
                P::CounterDepth,
                P::Angle,
                P::Depth,
            ],
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
            (FeatureKind::Extrude(e), P::Depth) => match &mut e.extent {
                ExtrudeExtent::Blind { depth } | ExtrudeExtent::MidPlane { depth } => {
                    *depth = value;
                }
                ExtrudeExtent::ThroughAll { .. } | ExtrudeExtent::UpToSurface { .. } => {
                    return false;
                }
            },
            (FeatureKind::Revolve(r), P::Angle) => r.angle = value,
            (FeatureKind::Fillet(b), P::Radius) | (FeatureKind::Chamfer(b), P::Distance) => {
                b.size = value;
            }
            (FeatureKind::Chamfer(b), P::Distance2) => match &mut b.chamfer {
                ChamferShape::TwoDistances { distance, .. } => *distance = value,
                _ => return false,
            },
            (FeatureKind::Chamfer(b), P::Angle) => match &mut b.chamfer {
                ChamferShape::AngleDistance { angle, .. } => *angle = value,
                _ => return false,
            },
            (FeatureKind::Shell(s), P::Thickness) => s.thickness = value,
            (FeatureKind::Plane(RefPlane::Offset { offset, .. }), P::Offset) => *offset = value,
            (FeatureKind::Plane(RefPlane::Angle { angle, .. }), P::Angle) => *angle = value,
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
            (FeatureKind::Hole(h), P::Diameter) => h.diameter = value,
            (FeatureKind::Hole(h), P::Depth) => match &mut h.depth {
                Some(depth) => *depth = value,
                None => return false,
            },
            (FeatureKind::Hole(h), P::CounterDiameter) => match &mut h.kind {
                HoleKind::Counterbore { diameter, .. } | HoleKind::Countersink { diameter, .. } => {
                    *diameter = value;
                }
                HoleKind::Simple => return false,
            },
            (FeatureKind::Hole(h), P::CounterDepth) => match &mut h.kind {
                HoleKind::Counterbore { depth, .. } => *depth = value,
                _ => return false,
            },
            (FeatureKind::Hole(h), P::Angle) => match &mut h.kind {
                HoleKind::Countersink { angle, .. } => *angle = value,
                _ => return false,
            },
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
                if let ExtrudeExtent::UpToSurface { plane } = &e.extent {
                    plane.collect_features(&mut out);
                }
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
            FeatureKind::Plane(p) => p.collect_features(&mut out),
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
                out.extend(m.bodies.iter().map(|b| b.feature));
            }
            FeatureKind::Import(_) => {}
            FeatureKind::Combine(c) => {
                out.push(c.target.feature);
                out.extend(c.tools.iter().map(|b| b.feature));
            }
            FeatureKind::Hole(h) => {
                out.push(h.sketch);
                out.extend(h.bodies.iter().map(|b| b.feature));
            }
            FeatureKind::Loft(l) => {
                out.extend(&l.profiles);
                out.extend(l.bodies.iter().map(|b| b.feature));
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
            FeatureKind::Extrude(_)
                | FeatureKind::Revolve(_)
                | FeatureKind::Sweep(_)
                | FeatureKind::Hole(_)
                | FeatureKind::Loft(_)
        )
    }

    /// The sketches the feature sweeps, if it is an extrusion, a revolve or a sweep: its
    /// profile first. The timeline shows them under the feature.
    pub fn swept_sketches(&self) -> Vec<FeatureId> {
        match self {
            FeatureKind::Extrude(e) => vec![e.sketch],
            FeatureKind::Revolve(r) => vec![r.sketch],
            FeatureKind::Sweep(s) => vec![s.profile, s.path],
            FeatureKind::Hole(h) => vec![h.sketch],
            FeatureKind::Loft(l) => l.profiles.clone(),
            _ => Vec::new(),
        }
    }

    /// What the feature does with the solid it sweeps, if it sweeps one.
    pub fn sweep_op(&self) -> Option<ExtrudeOp> {
        match self {
            FeatureKind::Extrude(e) => Some(e.op),
            FeatureKind::Revolve(r) => Some(r.op),
            FeatureKind::Sweep(s) => Some(s.op),
            FeatureKind::Hole(_) => Some(ExtrudeOp::Cut),
            FeatureKind::Loft(l) => Some(l.op),
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
    /// A line of a sketch (a centerline or not), by its entity id, running from its start
    /// point to its end point.
    SketchLine {
        sketch: FeatureId,
        entity: u32,
    },
}

impl AxisRef {
    pub fn collect_features(&self, out: &mut Vec<FeatureId>) {
        match self {
            AxisRef::Standard(_) => {}
            AxisRef::Reference(id) => out.push(*id),
            AxisRef::Edge(edge) => edge.faces.iter().for_each(|f| f.key.collect_features(out)),
            AxisRef::Face(face) => face.key.collect_features(out),
            AxisRef::SketchLine { sketch, .. } => out.push(*sketch),
        }
    }
}

/// A point picked for a feature: a corner of the body or a point of a sketch.
#[derive(Debug, Clone, PartialEq)]
pub enum PointRef {
    /// A vertex of the body, named by the faces meeting there.
    Vertex(VertexName),
    /// A point of a sketch (the end of a line, the centre of a circle, a lone point), by
    /// its entity id.
    SketchPoint { sketch: FeatureId, entity: u32 },
}

impl PointRef {
    pub fn collect_features(&self, out: &mut Vec<FeatureId>) {
        match self {
            PointRef::Vertex(vertex) => vertex
                .faces
                .iter()
                .for_each(|f| f.key.collect_features(out)),
            PointRef::SketchPoint { sketch, .. } => out.push(*sketch),
        }
    }
}

/// A curve picked for a feature: an edge of the body or a curve of a sketch.
#[derive(Debug, Clone, PartialEq)]
pub enum CurveRef {
    /// An edge of any kind: straight, circular or any other curve.
    Edge(EdgeName),
    /// A line, an arc or a circle of a sketch, by its entity id.
    SketchCurve { sketch: FeatureId, entity: u32 },
}

impl CurveRef {
    pub fn collect_features(&self, out: &mut Vec<FeatureId>) {
        match self {
            CurveRef::Edge(edge) => edge.faces.iter().for_each(|f| f.key.collect_features(out)),
            CurveRef::SketchCurve { sketch, .. } => out.push(*sketch),
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

/// Where an extrusion ends: SolidWorks' end conditions.
#[derive(Debug, Clone, PartialEq)]
pub enum ExtrudeExtent {
    /// From the sketch plane, `depth` in one direction.
    Blind { depth: f64 },
    /// `depth` in total, centred on the sketch plane.
    MidPlane { depth: f64 },
    /// From the sketch plane through everything the part holds in that direction (Through
    /// All); with `both`, through everything on either side of the sketch plane (Through
    /// All - Both). A cut through all goes a little further, so no skin is left.
    ThroughAll { both: bool },
    /// From the sketch plane up to a plane: a flat face of the part (its plane, extended
    /// beyond the face), a reference plane or a standard one (Up To Surface). It goes
    /// towards the plane, whichever side it is on.
    UpToSurface { plane: PlaneRef },
}

impl ExtrudeExtent {
    /// The depth of a blind or mid-plane extrusion; the other end conditions have none.
    pub fn depth(&self) -> Option<f64> {
        match self {
            ExtrudeExtent::Blind { depth } | ExtrudeExtent::MidPlane { depth } => Some(*depth),
            ExtrudeExtent::ThroughAll { .. } | ExtrudeExtent::UpToSurface { .. } => None,
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
    /// A chamfer's shape (a fillet's is always [`ChamferShape::Equal`]).
    pub chamfer: ChamferShape,
}

/// How a chamfer bevels its edges, as SolidWorks' chamfer types.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ChamferShape {
    /// `size` along both faces: a 45° bevel.
    #[default]
    Equal,
    /// `size` along one face of each edge and `distance` along the other.
    TwoDistances { distance: f64, flip: bool },
    /// `size` along one face of each edge, the bevel at `angle` (radians) from it.
    AngleDistance { angle: f64, flip: bool },
}

impl ChamferShape {
    /// Whether the faces swap: `size` then runs along the second face of each edge's
    /// name rather than the first (names are sorted, so this is stable).
    pub fn flipped(self) -> bool {
        match self {
            ChamferShape::Equal => false,
            ChamferShape::TwoDistances { flip, .. } | ChamferShape::AngleDistance { flip, .. } => {
                flip
            }
        }
    }
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

/// Reference plane: the most used constructions of SolidWorks' Plane command. Every one
/// refers to geometry by persistent name, so the plane follows it through rebuilds.
#[derive(Debug, Clone, PartialEq)]
pub enum RefPlane {
    /// Parallel to a plane or a flat face, `offset` (mm) along its normal; `flip` puts it
    /// on the other side. Its frame is the base's, moved.
    Offset {
        base: PlaneRef,
        offset: f64,
        flip: bool,
    },
    /// Through a line lying in a plane or a flat face, turned about that line by `angle`
    /// (radians) from it: counter-clockwise looking against the line's direction (the
    /// right-hand rule), clockwise with `flip`. Its frame is the base's, turned with it, so
    /// a sketch on it turns as the angle changes.
    Angle {
        base: PlaneRef,
        line: AxisRef,
        angle: f64,
        flip: bool,
    },
    /// Through a line (a straight edge, a sketch line, an axis), parallel or perpendicular
    /// to a plane or a flat face. Parallel, its normal is the plane's; perpendicular, it is
    /// the plane turned a quarter turn about the line's shadow on it, as for
    /// [`RefPlane::Angle`]. `flip` reverses the normal.
    ThroughLine {
        line: AxisRef,
        plane: PlaneRef,
        perpendicular: bool,
        flip: bool,
    },
    /// Through three points: its origin at the first, its x axis towards the second, its
    /// normal the side from which the three run counter-clockwise (`flip` reverses it).
    ThreePoints { points: [PointRef; 3], flip: bool },
    /// Through a point, normal to a curve where the curve passes closest to the point (at
    /// the point, when it is on the curve: the end of a sweep path, a vertex of an edge).
    /// Its normal runs along the curve (`flip` reverses it); its origin is the point.
    NormalToCurve {
        curve: CurveRef,
        point: PointRef,
        flip: bool,
    },
}

impl RefPlane {
    /// A plane `offset` (mm) from `base`.
    pub fn offset(base: PlaneRef, offset: f64) -> Self {
        RefPlane::Offset {
            base,
            offset,
            flip: false,
        }
    }

    /// Whether the plane is flipped: on the other side, turned the other way, or with its
    /// normal reversed, depending on its construction.
    pub fn flipped(&self) -> bool {
        match self {
            RefPlane::Offset { flip, .. }
            | RefPlane::Angle { flip, .. }
            | RefPlane::ThroughLine { flip, .. }
            | RefPlane::ThreePoints { flip, .. }
            | RefPlane::NormalToCurve { flip, .. } => *flip,
        }
    }

    pub fn collect_features(&self, out: &mut Vec<FeatureId>) {
        match self {
            RefPlane::Offset { base, .. } => base.collect_features(out),
            RefPlane::Angle { base, line, .. } => {
                base.collect_features(out);
                line.collect_features(out);
            }
            RefPlane::ThroughLine { line, plane, .. } => {
                line.collect_features(out);
                plane.collect_features(out);
            }
            RefPlane::ThreePoints { points, .. } => {
                points.iter().for_each(|p| p.collect_features(out));
            }
            RefPlane::NormalToCurve { curve, point, .. } => {
                curve.collect_features(out);
                point.collect_features(out);
            }
        }
    }
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

/// Mirror image of features, or of whole bodies, through a plane.
#[derive(Debug, Clone, PartialEq)]
pub struct Mirror {
    /// Features whose tool bodies are copied (extrusions, revolves, sweeps, holes).
    pub seeds: Vec<FeatureId>,
    pub plane: PlaneRef,
    /// Bodies mirrored whole, each joined to its mirror image (SolidWorks' Bodies to
    /// Mirror with Merge solids): half a symmetric part made whole. A mirror copies
    /// features or bodies, not both.
    pub bodies: Vec<BodyId>,
}

/// A solid lofted through the closed contours of several sketches, in order (SolidWorks'
/// Loft): a transition from a square to a circle, a tapered handle.
#[derive(Debug, Clone, PartialEq)]
pub struct Loft {
    /// The sketches lofted through, in order, each one closed contour (no holes).
    pub profiles: Vec<FeatureId>,
    pub op: ExtrudeOp,
    /// Straight from one contour to the next, rather than smooth through them all.
    pub ruled: bool,
    /// Bodies it joins, cuts or intersects; empty = automatic, as for [`Extrude`].
    pub bodies: Vec<BodyId>,
}

/// The shape of a hole's mouth, as SolidWorks' Hole Wizard types.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HoleKind {
    /// A plain drilled hole (clearance or tap drill).
    Simple,
    /// A counterbore for a socket head: its diameter and depth (mm).
    Counterbore { diameter: f64, depth: f64 },
    /// A countersink for a flat head: its diameter at the face (mm) and its included angle
    /// (radians, 90° for ISO screws).
    Countersink { diameter: f64, angle: f64 },
}

/// Holes drilled at the points of a sketch (Hole Wizard): one at each lone point of the
/// sketch (not the ends or centres of its curves, nor its origin), along the sketch's
/// normal into the part, cutting the bodies like a Cut-Extrude.
#[derive(Debug, Clone, PartialEq)]
pub struct Hole {
    /// The sketch whose points place the holes, usually on a face of the part.
    pub sketch: FeatureId,
    pub kind: HoleKind,
    /// Of the hole itself (mm).
    pub diameter: f64,
    /// How deep the full diameter goes from the sketch plane (mm), with a 118° drill point
    /// under it; `None` goes through everything (no point).
    pub depth: Option<f64>,
    /// Drill out of the face instead of into it (along the sketch normal).
    pub reverse: bool,
    /// Bodies it cuts; empty = every body.
    pub bodies: Vec<BodyId>,
}
