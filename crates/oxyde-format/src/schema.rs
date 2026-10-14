//! The JSON schema of `.oxy` files, version 1, and its conversion to and from the model.
//!
//! The file schema is deliberately separate from the in-memory model types: the model can
//! evolve freely while files stay readable, through explicit migrations.

use std::collections::BTreeMap;

use oxyde_geom::Point2;
use oxyde_model::{
    EdgeBlend, EdgeName, EquationText, Equations, EquationsText, Extrude, ExtrudeExtent, ExtrudeOp,
    FaceKey, FaceName, Feature, FeatureId, FeatureKind, FeatureParameter, GlobalText,
    PartDefinition, Sketch, SketchPlane, StandardPlane, VertexName,
};
use oxyde_sketch::{
    ConstrainedSketch, Constraint, Counters, Entity, EntityId, Geometry, Relation, RelationId,
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
    /// `"Width" = 50`: names are unique, ids never reused.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global_variables: Vec<GlobalDto>,
    /// `"D1@Sketch1" = "Width" * 2`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub equations: Vec<EquationDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_global_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct GlobalDto {
    pub id: u32,
    pub name: String,
    pub expression: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct EquationDto {
    /// Full name of the driven dimension.
    pub dimension: String,
    pub expression: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FeatureDto {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub suppressed: bool,
    /// Renamed feature dimensions, by parameter ("depth": "Thickness").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dimension_names: BTreeMap<String, String>,
    #[serde(flatten)]
    pub kind: KindDto,
}

/// A sketch relation with its stable id and, for dimensions, its name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RelationEntryDto {
    pub id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub relation: RelationDto,
}

/// Next ids to hand out in a sketch, so that ids are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct CountersDto {
    pub entity: u32,
    pub relation: u32,
    pub dimension: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum KindDto {
    Sketch {
        plane: PlaneDto,
        entities: Vec<EntityDto>,
        #[serde(default)]
        relations: Vec<RelationEntryDto>,
        #[serde(default)]
        next_ids: Option<CountersDto>,
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
    pub geometry: GeometryDto,
}

/// Lines, circles and arcs refer to point entities by id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum GeometryDto {
    Point { at: [f64; 2] },
    Line { start: u32, end: u32 },
    Circle { center: u32, radius: f64 },
    Arc { center: u32, start: u32, end: u32 },
}

/// Relations and dimensions. Lengths in mm, angles in radians.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum RelationDto {
    Fixed { point: u32 },
    Coincident { a: u32, b: u32 },
    Horizontal { line: u32 },
    Vertical { line: u32 },
    Parallel { a: u32, b: u32 },
    Perpendicular { a: u32, b: u32 },
    EqualLength { a: u32, b: u32 },
    EqualRadius { a: u32, b: u32 },
    Distance { a: u32, b: u32, value: f64 },
    HorizontalDistance { a: u32, b: u32, value: f64 },
    VerticalDistance { a: u32, b: u32, value: f64 },
    Length { line: u32, value: f64 },
    Radius { curve: u32, value: f64 },
    Angle { a: u32, b: u32, value: f64 },
    PointOnLine { point: u32, line: u32 },
    PointOnCircle { point: u32, curve: u32 },
    Midpoint { point: u32, line: u32 },
    Tangent { line: u32, curve: u32 },
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
        let text = definition.equations_text();
        Self {
            next_feature_id: definition.next_id,
            rollback: definition.rollback,
            features: definition.features.iter().map(feature_dto).collect(),
            next_global_id: (text.next_global_id > 1).then_some(text.next_global_id),
            global_variables: text
                .globals
                .into_iter()
                .map(|g| GlobalDto {
                    id: g.id,
                    name: g.name,
                    expression: g.expression,
                })
                .collect(),
            equations: text
                .equations
                .into_iter()
                .map(|e| EquationDto {
                    dimension: e.dimension,
                    expression: e.expression,
                })
                .collect(),
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
            entities: sketch.geometry.entities().iter().map(entity_dto).collect(),
            relations: sketch
                .geometry
                .relations()
                .iter()
                .map(|r| RelationEntryDto {
                    id: r.id.0,
                    name: r.name.clone(),
                    relation: relation_dto(&r.constraint),
                })
                .collect(),
            next_ids: {
                let c = sketch.geometry.counters();
                Some(CountersDto {
                    entity: c.entity,
                    relation: c.relation,
                    dimension: c.dimension,
                })
            },
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
        dimension_names: feature
            .parameter_names()
            .iter()
            .map(|(p, name)| (p.key().to_owned(), name.clone()))
            .collect(),
        kind,
    }
}

fn entity_dto(e: &Entity) -> EntityDto {
    EntityDto {
        id: e.id.0,
        construction: e.construction,
        geometry: match e.geometry {
            Geometry::Point(p) => GeometryDto::Point { at: point(p) },
            Geometry::Line { start, end } => GeometryDto::Line {
                start: start.0,
                end: end.0,
            },
            Geometry::Circle { center, radius } => GeometryDto::Circle {
                center: center.0,
                radius,
            },
            Geometry::Arc { center, start, end } => GeometryDto::Arc {
                center: center.0,
                start: start.0,
                end: end.0,
            },
        },
    }
}

fn relation_dto(c: &Constraint) -> RelationDto {
    use Constraint as C;
    use RelationDto as R;
    match *c {
        C::Fixed(p) => R::Fixed { point: p.0 },
        C::Coincident(a, b) => R::Coincident { a: a.0, b: b.0 },
        C::Horizontal(l) => R::Horizontal { line: l.0 },
        C::Vertical(l) => R::Vertical { line: l.0 },
        C::Parallel(a, b) => R::Parallel { a: a.0, b: b.0 },
        C::Perpendicular(a, b) => R::Perpendicular { a: a.0, b: b.0 },
        C::EqualLength(a, b) => R::EqualLength { a: a.0, b: b.0 },
        C::EqualRadius(a, b) => R::EqualRadius { a: a.0, b: b.0 },
        C::Distance(a, b, value) => R::Distance {
            a: a.0,
            b: b.0,
            value,
        },
        C::HorizontalDistance(a, b, value) => R::HorizontalDistance {
            a: a.0,
            b: b.0,
            value,
        },
        C::VerticalDistance(a, b, value) => R::VerticalDistance {
            a: a.0,
            b: b.0,
            value,
        },
        C::Length(l, value) => R::Length { line: l.0, value },
        C::Radius(r, value) => R::Radius { curve: r.0, value },
        C::Angle(a, b, value) => R::Angle {
            a: a.0,
            b: b.0,
            value,
        },
        C::PointOnLine(p, l) => R::PointOnLine {
            point: p.0,
            line: l.0,
        },
        C::PointOnCircle(p, r) => R::PointOnCircle {
            point: p.0,
            curve: r.0,
        },
        C::Midpoint(p, l) => R::Midpoint {
            point: p.0,
            line: l.0,
        },
        C::Tangent(l, r) => R::Tangent {
            line: l.0,
            curve: r.0,
        },
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
    /// Converts to a model definition; fails on an inconsistent sketch.
    pub fn into_definition(self) -> Result<PartDefinition, String> {
        let mut definition = PartDefinition {
            features: self
                .features
                .into_iter()
                .map(feature_from)
                .collect::<Result<_, _>>()?,
            rollback: self.rollback,
            next_id: self.next_feature_id,
            equations: Equations::default(),
        };
        definition
            .set_equations_text(EquationsText {
                globals: self
                    .global_variables
                    .into_iter()
                    .map(|g| GlobalText {
                        id: g.id,
                        name: g.name,
                        expression: g.expression,
                    })
                    .collect(),
                equations: self
                    .equations
                    .into_iter()
                    .map(|e| EquationText {
                        dimension: e.dimension,
                        expression: e.expression,
                    })
                    .collect(),
                next_global_id: self.next_global_id.unwrap_or(1),
            })
            .map_err(|e| format!("equations: {e}"))?;
        Ok(definition)
    }
}

fn point_from(p: [f64; 2]) -> Point2 {
    Point2::new(p[0], p[1])
}

fn entity_from(dto: EntityDto) -> Entity {
    let id = EntityId;
    Entity {
        id: id(dto.id),
        construction: dto.construction,
        geometry: match dto.geometry {
            GeometryDto::Point { at } => Geometry::Point(point_from(at)),
            GeometryDto::Line { start, end } => Geometry::Line {
                start: id(start),
                end: id(end),
            },
            GeometryDto::Circle { center, radius } => Geometry::Circle {
                center: id(center),
                radius,
            },
            GeometryDto::Arc { center, start, end } => Geometry::Arc {
                center: id(center),
                start: id(start),
                end: id(end),
            },
        },
    }
}

fn relation_from(dto: RelationDto) -> Constraint {
    use Constraint as C;
    use RelationDto as R;
    let id = EntityId;
    match dto {
        R::Fixed { point } => C::Fixed(id(point)),
        R::Coincident { a, b } => C::Coincident(id(a), id(b)),
        R::Horizontal { line } => C::Horizontal(id(line)),
        R::Vertical { line } => C::Vertical(id(line)),
        R::Parallel { a, b } => C::Parallel(id(a), id(b)),
        R::Perpendicular { a, b } => C::Perpendicular(id(a), id(b)),
        R::EqualLength { a, b } => C::EqualLength(id(a), id(b)),
        R::EqualRadius { a, b } => C::EqualRadius(id(a), id(b)),
        R::Distance { a, b, value } => C::Distance(id(a), id(b), value),
        R::HorizontalDistance { a, b, value } => C::HorizontalDistance(id(a), id(b), value),
        R::VerticalDistance { a, b, value } => C::VerticalDistance(id(a), id(b), value),
        R::Length { line, value } => C::Length(id(line), value),
        R::Radius { curve, value } => C::Radius(id(curve), value),
        R::Angle { a, b, value } => C::Angle(id(a), id(b), value),
        R::PointOnLine { point, line } => C::PointOnLine(id(point), id(line)),
        R::PointOnCircle { point, curve } => C::PointOnCircle(id(point), id(curve)),
        R::Midpoint { point, line } => C::Midpoint(id(point), id(line)),
        R::Tangent { line, curve } => C::Tangent(id(line), id(curve)),
    }
}

fn feature_from(dto: FeatureDto) -> Result<Feature, String> {
    let kind = match dto.kind {
        KindDto::Sketch {
            plane,
            entities,
            relations,
            next_ids,
        } => {
            let counters = next_ids.map_or_else(Counters::default, |c| Counters {
                entity: c.entity,
                relation: c.relation,
                dimension: c.dimension,
            });
            let geometry = ConstrainedSketch::from_parts(
                entities.into_iter().map(entity_from).collect(),
                relations
                    .into_iter()
                    .map(|r| Relation {
                        id: RelationId(r.id),
                        name: r.name,
                        constraint: relation_from(r.relation),
                    })
                    .collect(),
                counters,
            )
            .map_err(|e| format!("{}: {e}", dto.name))?;
            FeatureKind::Sketch(Sketch {
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
                geometry,
            })
        }
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
    let mut parameter_names = BTreeMap::new();
    for (key, name) in dto.dimension_names {
        let parameter = FeatureParameter::from_key(&key)
            .ok_or_else(|| format!("{}: unknown dimension \"{key}\"", dto.name))?;
        parameter_names.insert(parameter, name);
    }
    Ok(
        Feature::new(FeatureId::from_raw(dto.id), dto.name, dto.suppressed, kind)
            .with_parameter_names(parameter_names),
    )
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
