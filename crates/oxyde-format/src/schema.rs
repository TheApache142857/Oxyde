//! The JSON schema of `.oxy` files, version 1, and its conversion to and from the model.
//!
//! The file schema is deliberately separate from the in-memory model types: the model can
//! evolve freely while files stay readable, through explicit migrations.

use std::collections::BTreeMap;

use oxyde_geom::{Curve2, Point2};
use oxyde_model::{
    EdgeBlend, EdgeName, Extrude, ExtrudeExtent, ExtrudeOp, FaceKey, FaceName, Feature, FeatureId,
    FeatureKind, PartDefinition, Sketch, SketchEntity, SketchPlane, StandardPlane, VertexName,
};
use serde::{Deserialize, Serialize};

// ---- manifest.json -------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub format: String,
    pub format_version: u32,
    pub kind: String,
    pub document_id: String,
    pub units: Units,
    pub created_by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Units {
    pub length: String,
    pub angle: String,
}

// ---- properties.json -----------------------------------------------------------------------

/// Part metadata, readable by BOM tools without understanding geometry.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Properties {
    #[serde(default)]
    pub part_number: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub material: String,
    /// Free-form properties.
    #[serde(default)]
    pub custom: BTreeMap<String, String>,
    /// Volume of the part at save time, in mm³. Informational only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_mm3: Option<f64>,
}

// ---- model.json ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PartModel {
    pub next_feature_id: u32,
    /// Number of active features; absent or null = all.
    #[serde(default)]
    pub rollback: Option<usize>,
    pub features: Vec<FeatureDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FeatureDto {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub suppressed: bool,
    #[serde(flatten)]
    pub kind: KindDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum KindDto {
    Sketch {
        plane: PlaneDto,
        entities: Vec<EntityDto>,
    },
    Extrude {
        sketch: u32,
        operation: OperationDto,
        extent: ExtentDto,
        #[serde(default)]
        reverse: bool,
    },
    Fillet {
        edges: Vec<EdgeRefDto>,
        radius: f64,
    },
    Chamfer {
        edges: Vec<EdgeRefDto>,
        distance: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlaneDto {
    Standard(StandardPlaneDto),
    Face(FaceRefDto),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StandardPlaneDto {
    Front,
    Top,
    Right,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct EntityDto {
    pub id: u32,
    #[serde(default)]
    pub construction: bool,
    #[serde(flatten)]
    pub curve: CurveDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum CurveDto {
    Line {
        start: [f64; 2],
        end: [f64; 2],
    },
    Arc {
        start: [f64; 2],
        mid: [f64; 2],
        end: [f64; 2],
    },
    Circle {
        center: [f64; 2],
        radius: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OperationDto {
    Boss,
    Cut,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ExtentDto {
    Blind { depth: f64 },
    MidPlane { depth: f64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FaceRefDto {
    pub key: FaceKeyDto,
    #[serde(default)]
    pub part: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum FaceKeyDto {
    Profile {
        feature: u32,
        segment: u32,
    },
    StartCap {
        feature: u32,
    },
    EndCap {
        feature: u32,
    },
    FromEdge {
        feature: u32,
        edge: Box<EdgeRefDto>,
    },
    FromVertex {
        feature: u32,
        vertex: Box<VertexRefDto>,
    },
    Unknown {
        feature: u32,
        index: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct EdgeRefDto {
    pub faces: Vec<FaceRefDto>,
    #[serde(default)]
    pub part: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct VertexRefDto {
    pub faces: Vec<FaceRefDto>,
    #[serde(default)]
    pub part: u32,
}

// ---- Model → file --------------------------------------------------------------------------

impl PartModel {
    pub fn from_definition(definition: &PartDefinition) -> Self {
        Self {
            next_feature_id: definition.next_id,
            rollback: definition.rollback,
            features: definition.features.iter().map(feature_dto).collect(),
        }
    }
}

fn point(p: Point2) -> [f64; 2] {
    [p.x, p.y]
}

fn feature_dto(feature: &Feature) -> FeatureDto {
    let kind = match feature.kind() {
        FeatureKind::Sketch(sketch) => KindDto::Sketch {
            plane: match &sketch.plane {
                SketchPlane::Standard(StandardPlane::Front) => {
                    PlaneDto::Standard(StandardPlaneDto::Front)
                }
                SketchPlane::Standard(StandardPlane::Top) => {
                    PlaneDto::Standard(StandardPlaneDto::Top)
                }
                SketchPlane::Standard(StandardPlane::Right) => {
                    PlaneDto::Standard(StandardPlaneDto::Right)
                }
                SketchPlane::Face(face) => PlaneDto::Face(face_dto(face)),
            },
            entities: sketch
                .entities
                .iter()
                .map(|e| EntityDto {
                    id: e.id,
                    construction: e.construction,
                    curve: match e.curve {
                        Curve2::Line { start, end } => CurveDto::Line {
                            start: point(start),
                            end: point(end),
                        },
                        Curve2::Arc { start, mid, end } => CurveDto::Arc {
                            start: point(start),
                            mid: point(mid),
                            end: point(end),
                        },
                        Curve2::Circle { center, radius } => CurveDto::Circle {
                            center: point(center),
                            radius,
                        },
                    },
                })
                .collect(),
        },
        FeatureKind::Extrude(extrude) => KindDto::Extrude {
            sketch: extrude.sketch.raw(),
            operation: match extrude.op {
                ExtrudeOp::Boss => OperationDto::Boss,
                ExtrudeOp::Cut => OperationDto::Cut,
            },
            extent: match extrude.extent {
                ExtrudeExtent::Blind { depth } => ExtentDto::Blind { depth },
                ExtrudeExtent::MidPlane { depth } => ExtentDto::MidPlane { depth },
            },
            reverse: extrude.reverse,
        },
        FeatureKind::Fillet(blend) => KindDto::Fillet {
            edges: blend.edges.iter().map(edge_dto).collect(),
            radius: blend.size,
        },
        FeatureKind::Chamfer(blend) => KindDto::Chamfer {
            edges: blend.edges.iter().map(edge_dto).collect(),
            distance: blend.size,
        },
    };
    FeatureDto {
        id: feature.id().raw(),
        name: feature.name().to_owned(),
        suppressed: feature.is_suppressed(),
        kind,
    }
}

fn face_dto(face: &FaceName) -> FaceRefDto {
    let key = match &face.key {
        FaceKey::Profile { feature, segment } => FaceKeyDto::Profile {
            feature: feature.raw(),
            segment: *segment,
        },
        FaceKey::StartCap { feature } => FaceKeyDto::StartCap {
            feature: feature.raw(),
        },
        FaceKey::EndCap { feature } => FaceKeyDto::EndCap {
            feature: feature.raw(),
        },
        FaceKey::FromEdge { feature, edge } => FaceKeyDto::FromEdge {
            feature: feature.raw(),
            edge: Box::new(edge_dto(edge)),
        },
        FaceKey::FromVertex { feature, vertex } => FaceKeyDto::FromVertex {
            feature: feature.raw(),
            vertex: Box::new(VertexRefDto {
                faces: vertex.faces.iter().map(face_dto).collect(),
                part: vertex.part,
            }),
        },
        FaceKey::Unknown { feature, index } => FaceKeyDto::Unknown {
            feature: feature.raw(),
            index: *index,
        },
    };
    FaceRefDto {
        key,
        part: face.part,
    }
}

fn edge_dto(edge: &EdgeName) -> EdgeRefDto {
    EdgeRefDto {
        faces: edge.faces.iter().map(face_dto).collect(),
        part: edge.part,
    }
}

// ---- File → model --------------------------------------------------------------------------

impl PartModel {
    pub fn into_definition(self) -> PartDefinition {
        PartDefinition {
            features: self.features.into_iter().map(feature_from).collect(),
            rollback: self.rollback,
            next_id: self.next_feature_id,
        }
    }
}

fn point_from(p: [f64; 2]) -> Point2 {
    Point2::new(p[0], p[1])
}

fn feature_from(dto: FeatureDto) -> Feature {
    let kind = match dto.kind {
        KindDto::Sketch { plane, entities } => FeatureKind::Sketch(Sketch {
            plane: match plane {
                PlaneDto::Standard(StandardPlaneDto::Front) => {
                    SketchPlane::Standard(StandardPlane::Front)
                }
                PlaneDto::Standard(StandardPlaneDto::Top) => {
                    SketchPlane::Standard(StandardPlane::Top)
                }
                PlaneDto::Standard(StandardPlaneDto::Right) => {
                    SketchPlane::Standard(StandardPlane::Right)
                }
                PlaneDto::Face(face) => SketchPlane::Face(face_from(face)),
            },
            entities: entities
                .into_iter()
                .map(|e| SketchEntity {
                    id: e.id,
                    construction: e.construction,
                    curve: match e.curve {
                        CurveDto::Line { start, end } => Curve2::Line {
                            start: point_from(start),
                            end: point_from(end),
                        },
                        CurveDto::Arc { start, mid, end } => Curve2::Arc {
                            start: point_from(start),
                            mid: point_from(mid),
                            end: point_from(end),
                        },
                        CurveDto::Circle { center, radius } => Curve2::Circle {
                            center: point_from(center),
                            radius,
                        },
                    },
                })
                .collect(),
        }),
        KindDto::Extrude {
            sketch,
            operation,
            extent,
            reverse,
        } => FeatureKind::Extrude(Extrude {
            sketch: FeatureId::from_raw(sketch),
            op: match operation {
                OperationDto::Boss => ExtrudeOp::Boss,
                OperationDto::Cut => ExtrudeOp::Cut,
            },
            extent: match extent {
                ExtentDto::Blind { depth } => ExtrudeExtent::Blind { depth },
                ExtentDto::MidPlane { depth } => ExtrudeExtent::MidPlane { depth },
            },
            reverse,
        }),
        KindDto::Fillet { edges, radius } => FeatureKind::Fillet(EdgeBlend {
            edges: edges.into_iter().map(edge_from).collect(),
            size: radius,
        }),
        KindDto::Chamfer { edges, distance } => FeatureKind::Chamfer(EdgeBlend {
            edges: edges.into_iter().map(edge_from).collect(),
            size: distance,
        }),
    };
    Feature::new(FeatureId::from_raw(dto.id), dto.name, dto.suppressed, kind)
}

fn face_from(dto: FaceRefDto) -> FaceName {
    let key = match dto.key {
        FaceKeyDto::Profile { feature, segment } => FaceKey::Profile {
            feature: FeatureId::from_raw(feature),
            segment,
        },
        FaceKeyDto::StartCap { feature } => FaceKey::StartCap {
            feature: FeatureId::from_raw(feature),
        },
        FaceKeyDto::EndCap { feature } => FaceKey::EndCap {
            feature: FeatureId::from_raw(feature),
        },
        FaceKeyDto::FromEdge { feature, edge } => FaceKey::FromEdge {
            feature: FeatureId::from_raw(feature),
            edge: edge_from(*edge),
        },
        FaceKeyDto::FromVertex { feature, vertex } => FaceKey::FromVertex {
            feature: FeatureId::from_raw(feature),
            vertex: VertexName {
                faces: sorted(vertex.faces.into_iter().map(face_from).collect()),
                part: vertex.part,
            },
        },
        FaceKeyDto::Unknown { feature, index } => FaceKey::Unknown {
            feature: FeatureId::from_raw(feature),
            index,
        },
    };
    FaceName {
        key,
        part: dto.part,
    }
}

fn edge_from(dto: EdgeRefDto) -> EdgeName {
    EdgeName {
        faces: sorted(dto.faces.into_iter().map(face_from).collect()),
        part: dto.part,
    }
}

/// Names compare as sorted face lists; files written by hand may not be sorted.
fn sorted(mut faces: Vec<FaceName>) -> Vec<FaceName> {
    faces.sort();
    faces.dedup();
    faces
}
