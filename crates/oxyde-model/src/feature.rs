//! Features: the entries of a part's timeline.

use std::fmt;

use oxyde_geom::{Curve2, Plane, Point2};

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

/// One entry of the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    pub(crate) id: FeatureId,
    pub(crate) name: String,
    pub(crate) suppressed: bool,
    pub(crate) kind: FeatureKind,
}

impl Feature {
    pub fn id(&self) -> FeatureId {
        self.id
    }

    /// Display name, e.g. "Boss-Extrude1". Purely cosmetic: references use [`FeatureId`].
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SketchEntity {
    /// Unique within the sketch; names the faces an extrusion sweeps from this entity.
    pub id: u32,
    pub curve: Curve2,
    /// Construction geometry is ignored when building profiles.
    pub construction: bool,
}

/// A 2D sketch. Geometry is explicit for now; the constraint solver (M2) will compute it
/// from dimensions and relations.
#[derive(Debug, Clone, PartialEq)]
pub struct Sketch {
    pub plane: SketchPlane,
    pub entities: Vec<SketchEntity>,
}

impl Sketch {
    pub fn new(plane: SketchPlane) -> Self {
        Self {
            plane,
            entities: Vec::new(),
        }
    }

    fn next_id(&self) -> u32 {
        self.entities.iter().map(|e| e.id).max().unwrap_or(0) + 1
    }

    /// Adds a curve and returns its entity id.
    pub fn add(&mut self, curve: Curve2) -> u32 {
        let id = self.next_id();
        self.entities.push(SketchEntity {
            id,
            curve,
            construction: false,
        });
        id
    }

    pub fn line(&mut self, start: Point2, end: Point2) -> u32 {
        self.add(Curve2::Line { start, end })
    }

    pub fn circle(&mut self, center: Point2, radius: f64) -> u32 {
        self.add(Curve2::Circle { center, radius })
    }

    /// Axis-aligned rectangle; returns the ids of its bottom, right, top and left lines.
    pub fn rectangle(&mut self, corner: Point2, width: f64, height: f64) -> [u32; 4] {
        let (x0, y0) = (corner.x, corner.y);
        let (x1, y1) = (x0 + width, y0 + height);
        [
            self.line(Point2::new(x0, y0), Point2::new(x1, y0)),
            self.line(Point2::new(x1, y0), Point2::new(x1, y1)),
            self.line(Point2::new(x1, y1), Point2::new(x0, y1)),
            self.line(Point2::new(x0, y1), Point2::new(x0, y0)),
        ]
    }

    pub fn entity_mut(&mut self, id: u32) -> Option<&mut SketchEntity> {
        self.entities.iter_mut().find(|e| e.id == id)
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
