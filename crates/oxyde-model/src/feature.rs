//! Features: the entries of a part's timeline.

use std::collections::BTreeMap;
use std::fmt;

use oxyde_geom::{Plane, Point2};
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
}

impl FeatureParameter {
    /// Key used in files.
    pub fn key(self) -> &'static str {
        match self {
            FeatureParameter::Depth => "depth",
            FeatureParameter::Radius => "radius",
            FeatureParameter::Distance => "distance",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "depth" => Some(FeatureParameter::Depth),
            "radius" => Some(FeatureParameter::Radius),
            "distance" => Some(FeatureParameter::Distance),
            _ => None,
        }
    }

    /// Whether the value is a length, an angle or a number of instances.
    pub fn unit(self) -> DimensionUnit {
        match self {
            FeatureParameter::Depth | FeatureParameter::Radius | FeatureParameter::Distance => {
                DimensionUnit::Length
            }
        }
    }

    /// What the parameter is called in the user interface.
    pub fn label(self) -> &'static str {
        match self {
            FeatureParameter::Depth => "Depth",
            FeatureParameter::Radius => "Radius",
            FeatureParameter::Distance => "Distance",
        }
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

#[derive(Debug, Clone, PartialEq)]
pub enum FeatureKind {
    Sketch(Sketch),
    Extrude(Extrude),
    Fillet(EdgeBlend),
    Chamfer(EdgeBlend),
}

impl FeatureKind {
    /// SolidWorks-style type name, used for automatic feature names.
    pub fn type_name(&self) -> &'static str {
        match self {
            FeatureKind::Sketch(_) => "Sketch",
            FeatureKind::Extrude(e) => match e.op {
                ExtrudeOp::Boss => "Boss-Extrude",
                ExtrudeOp::Cut => "Cut-Extrude",
            },
            FeatureKind::Fillet(_) => "Fillet",
            FeatureKind::Chamfer(_) => "Chamfer",
        }
    }

    /// The feature's own dimensions and their values (mm).
    pub fn parameters(&self) -> Vec<(FeatureParameter, f64)> {
        match self {
            FeatureKind::Sketch(_) => Vec::new(),
            FeatureKind::Extrude(e) => vec![(FeatureParameter::Depth, e.extent.depth())],
            FeatureKind::Fillet(b) => vec![(FeatureParameter::Radius, b.size)],
            FeatureKind::Chamfer(b) => vec![(FeatureParameter::Distance, b.size)],
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
    pub fn set_parameter(&mut self, parameter: FeatureParameter, value: f64) -> bool {
        match (self, parameter) {
            (FeatureKind::Extrude(e), FeatureParameter::Depth) => {
                e.extent = match e.extent {
                    ExtrudeExtent::Blind { .. } => ExtrudeExtent::Blind { depth: value },
                    ExtrudeExtent::MidPlane { .. } => ExtrudeExtent::MidPlane { depth: value },
                };
                true
            }
            (FeatureKind::Fillet(b), FeatureParameter::Radius)
            | (FeatureKind::Chamfer(b), FeatureParameter::Distance) => {
                b.size = value;
                true
            }
            _ => false,
        }
    }

    /// Features this one refers to: its sketch, or the features that created the faces and
    /// edges it is attached to. Sorted, without duplicates.
    pub fn dependencies(&self) -> Vec<FeatureId> {
        let mut out = Vec::new();
        match self {
            FeatureKind::Sketch(s) => {
                if let SketchPlane::Face(face) = &s.plane {
                    face.key.collect_features(&mut out);
                }
            }
            FeatureKind::Extrude(e) => out.push(e.sketch),
            FeatureKind::Fillet(b) | FeatureKind::Chamfer(b) => {
                for edge in &b.edges {
                    edge.faces
                        .iter()
                        .for_each(|f| f.key.collect_features(&mut out));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// The three reference planes every part starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Where a sketch is drawn.
#[derive(Debug, Clone, PartialEq)]
pub enum SketchPlane {
    Standard(StandardPlane),
    /// A planar face of the body as it is just before the sketch in the timeline.
    Face(FaceName),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtrudeOp {
    /// Adds material, merged with the existing body.
    Boss,
    /// Removes material from the existing body.
    Cut,
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
}

impl Extrude {
    pub fn blind(sketch: FeatureId, op: ExtrudeOp, depth: f64) -> Self {
        Self {
            sketch,
            op,
            extent: ExtrudeExtent::Blind { depth },
            reverse: false,
        }
    }
}

/// Fillet (`size` = radius) or chamfer (`size` = distance) on a set of edges.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeBlend {
    pub edges: Vec<EdgeName>,
    pub size: f64,
}
