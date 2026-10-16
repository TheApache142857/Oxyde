//! The JSON schema of `.oxy` files, version 1, and its conversion to and from the model.
//!
//! The file schema is deliberately separate from the in-memory model types: the model can
//! evolve freely while files stay readable, through explicit migrations.

use std::collections::{BTreeMap, HashMap};

use oxyde_geom::Point2;
use oxyde_model::{
    Appearance, Appearances, AxisRef, BodyId, CircularPattern, Combine, CombineOp, EdgeBlend, EdgeName, EquationText, Material,
    MaterialSource,
    Equations, EquationsText, Extrude, ExtrudeExtent, ExtrudeOp, FaceKey, FaceName, Feature,
    FeatureId, FeatureKind, FeatureParameter, FileData, GlobalText, Import, LinearPattern, Mirror,
    PartDefinition, PatternDirection, PlaneRef, RefAxis, RefPlane, Revolve, RevolveAxis, Shell,
    Sketch, SketchPlane, StandardAxis, StandardPlane, Texture, VertexName,
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
    /// Colours, transparency and textures; absent = the viewport's default everywhere.
    #[serde(default, skip_serializing_if = "AppearancesDto::is_empty")]
    pub appearances: AppearancesDto,
    /// What the part is made of, copied from a material library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<MaterialDto>,
}

// ---- Materials -----------------------------------------------------------------------------

/// A material: a named appearance with physical data. The same shape in a part's
/// `model.json` and in a material library file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MaterialDto {
    pub name: String,
    #[serde(default)]
    pub category: String,
    /// kg/m³.
    pub density: f64,
    pub appearance: AppearanceDto,
    /// `"step"` for a material named by an imported STEP file; absent for the library's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

pub(crate) fn material_dto(m: &Material, textures: &TextureTable) -> MaterialDto {
    MaterialDto {
        name: m.name.clone(),
        category: m.category.clone(),
        density: m.density,
        appearance: appearance_dto(&m.appearance, textures),
        source: match m.source {
            MaterialSource::Library => None,
            MaterialSource::Step => Some("step".to_owned()),
        },
    }
}

pub(crate) fn material_from(
    dto: MaterialDto,
    files: &HashMap<String, Vec<u8>>,
) -> Result<Material, String> {
    if !Material::is_valid_name(&dto.name) {
        return Err(format!("invalid material name \"{}\"", dto.name));
    }
    let source = match dto.source.as_deref() {
        None | Some("library") => MaterialSource::Library,
        Some("step") => MaterialSource::Step,
        Some(other) => return Err(format!("unknown material source \"{other}\"")),
    };
    Ok(Material {
        name: dto.name,
        category: dto.category,
        density: dto.density,
        appearance: appearance_from(dto.appearance, files)?,
        source,
    }
    .clamped())
}

// ---- Appearances ---------------------------------------------------------------------------

/// The appearances of a part: its own, and those of bodies and faces, which win over it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub(crate) struct AppearancesDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<AppearanceDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bodies: Vec<BodyAppearanceDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faces: Vec<FaceAppearanceDto>,
}

impl AppearancesDto {
    pub fn is_empty(&self) -> bool {
        self.part.is_none() && self.bodies.is_empty() && self.faces.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct BodyAppearanceDto {
    pub body: BodyIdDto,
    #[serde(flatten)]
    pub appearance: AppearanceDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FaceAppearanceDto {
    pub face: FaceRefDto,
    #[serde(flatten)]
    pub appearance: AppearanceDto,
}

/// A colour, a transparency and an optional texture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AppearanceDto {
    /// sRGB, 0–255.
    pub color: [u8; 3],
    /// 0 (opaque, the default) to 1 (invisible).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub transparency: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<TextureDto>,
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

/// An image repeated over the surface, kept in the archive under `textures/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TextureDto {
    /// The archive entry holding the image (PNG or JPEG).
    pub file: String,
    /// The image's original file name, for display.
    #[serde(default)]
    pub file_name: String,
    /// Millimetres covered by one repeat of the image.
    pub scale: f64,
}

/// The texture images of a document and the archive entries they are written to: each
/// image once, however many appearances use it.
#[derive(Debug, Default)]
pub struct TextureTable {
    entries: Vec<(FileData, String)>,
}

impl TextureTable {
    pub fn new<'a>(textures: impl IntoIterator<Item = &'a Texture>) -> Self {
        Self::in_folder("textures", textures)
    }

    /// Entries named `<folder>/<n>.<ext>`.
    pub fn in_folder<'a>(folder: &str, textures: impl IntoIterator<Item = &'a Texture>) -> Self {
        let mut table = Self::default();
        for t in textures {
            if table.entries.iter().any(|(d, _)| *d == t.data) {
                continue;
            }
            let extension = std::path::Path::new(&t.file_name)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .filter(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg"))
                .unwrap_or_else(|| "bin".to_owned());
            let name = format!("{folder}/{}.{}", table.entries.len() + 1, extension);
            table.entries.push((t.data.clone(), name));
        }
        table
    }

    /// The archive entries to write: name and contents.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.entries.iter().map(|(d, n)| (n.as_str(), d.bytes()))
    }

    fn entry(&self, data: &FileData) -> String {
        self.entries
            .iter()
            .find(|(d, _)| d == data)
            .map(|(_, n)| n.clone())
            .unwrap_or_default()
    }
}

pub(crate) fn appearance_dto(a: &Appearance, textures: &TextureTable) -> AppearanceDto {
    AppearanceDto {
        color: a.color,
        transparency: a.transparency,
        texture: a.texture.as_ref().map(|t| TextureDto {
            file: textures.entry(&t.data),
            file_name: t.file_name.clone(),
            scale: t.scale,
        }),
    }
}

/// `files` holds the archive's `textures/` entries by name.
pub(crate) fn appearance_from(
    dto: AppearanceDto,
    files: &HashMap<String, Vec<u8>>,
) -> Result<Appearance, String> {
    let texture = match dto.texture {
        Some(t) => Some(Texture {
            data: FileData::new(
                files
                    .get(&t.file)
                    .cloned()
                    .ok_or_else(|| format!("texture {} is missing from the archive", t.file))?,
            ),
            file_name: t.file_name,
            scale: t.scale,
        }),
        None => None,
    };
    Ok(Appearance {
        color: dto.color,
        transparency: dto.transparency,
        texture,
    }
    .clamped())
}

pub(crate) fn appearances_dto(a: &Appearances, textures: &TextureTable) -> AppearancesDto {
    AppearancesDto {
        part: a.part.as_ref().map(|p| appearance_dto(p, textures)),
        bodies: a
            .bodies
            .iter()
            .map(|(id, p)| BodyAppearanceDto {
                body: body_dto(id),
                appearance: appearance_dto(p, textures),
            })
            .collect(),
        faces: a
            .faces
            .iter()
            .map(|(name, p)| FaceAppearanceDto {
                face: face_dto(name),
                appearance: appearance_dto(p, textures),
            })
            .collect(),
    }
}

pub(crate) fn appearances_from(
    dto: AppearancesDto,
    files: &HashMap<String, Vec<u8>>,
) -> Result<Appearances, String> {
    let mut out = Appearances::default();
    if let Some(p) = dto.part {
        out.set_part(Some(appearance_from(p, files)?));
    }
    for b in dto.bodies {
        out.set_body(body_from(b.body), Some(appearance_from(b.appearance, files)?));
    }
    for f in dto.faces {
        out.set_face(&face_from(f.face), Some(appearance_from(f.appearance, files)?));
    }
    Ok(out)
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
        /// The entity id of the sketch origin.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<u32>,
    },
    Extrude {
        sketch: u32,
        operation: OperationDto,
        extent: ExtentDto,
        #[serde(default)]
        reverse: bool,
        /// Bodies it works on; absent or empty = automatic.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        bodies: Vec<BodyIdDto>,
    },
    Revolve {
        sketch: u32,
        operation: OperationDto,
        axis: RevolveAxisDto,
        /// Radians.
        angle: f64,
        #[serde(default)]
        mid_plane: bool,
        #[serde(default)]
        reverse: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        bodies: Vec<BodyIdDto>,
    },
    Fillet {
        edges: Vec<EdgeRefDto>,
        radius: f64,
    },
    Chamfer {
        edges: Vec<EdgeRefDto>,
        distance: f64,
    },
    Shell {
        #[serde(default)]
        faces: Vec<FaceRefDto>,
        thickness: f64,
        #[serde(default)]
        outward: bool,
    },
    /// Reference plane.
    Plane {
        base: PlaneDto,
        offset: f64,
        #[serde(default)]
        flip: bool,
    },
    /// Reference axis.
    Axis {
        axis: RefAxisDto,
    },
    LinearPattern {
        seeds: Vec<u32>,
        direction: DirectionDto,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction2: Option<DirectionDto>,
    },
    CircularPattern {
        seeds: Vec<u32>,
        axis: AxisRefDto,
        /// Radians.
        angle: f64,
        count: u32,
        #[serde(default)]
        equal_spacing: bool,
        #[serde(default)]
        reverse: bool,
    },
    Mirror {
        seeds: Vec<u32>,
        plane: PlaneDto,
    },
    /// Geometry imported from a STEP file, stored in the archive at `file`.
    Import {
        file_name: String,
        file: String,
        /// The file's colours are still to become appearances (a part written before its
        /// first rebuild, by the assembly import).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        apply_colors: bool,
    },
    /// Boolean between bodies of the part.
    Combine {
        target: BodyIdDto,
        tools: Vec<BodyIdDto>,
        operation: CombineOpDto,
        #[serde(default)]
        keep_tools: bool,
    },
}

/// A body: the feature that made it, and which of its bodies.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct BodyIdDto {
    pub feature: u32,
    #[serde(default)]
    pub index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CombineOpDto {
    Join,
    Cut,
    Intersect,
}

fn body_dto(id: &BodyId) -> BodyIdDto {
    BodyIdDto {
        feature: id.feature.raw(),
        index: id.index,
    }
}

fn body_from(dto: BodyIdDto) -> BodyId {
    BodyId::new(FeatureId::from_raw(dto.feature), dto.index)
}

/// Where the STEP file of an import feature is kept in the archive.
pub(crate) fn import_entry(feature: FeatureId) -> String {
    format!("imports/{}.step", feature.raw())
}

/// A sketch plane, or any plane a feature refers to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlaneDto {
    Standard(StandardPlaneDto),
    Face(FaceRefDto),
    /// A reference plane feature, by id.
    Reference(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StandardAxisDto {
    X,
    Y,
    Z,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AxisRefDto {
    Standard(StandardAxisDto),
    /// A reference axis feature, by id.
    Reference(u32),
    Edge(EdgeRefDto),
    Face(FaceRefDto),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RevolveAxisDto {
    /// A line of the revolve's sketch, by entity id.
    SketchLine(u32),
    Axis(AxisRefDto),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RefAxisDto {
    Along(AxisRefDto),
    TwoPlanes([PlaneDto; 2]),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DirectionDto {
    pub axis: AxisRefDto,
    pub spacing: f64,
    pub count: u32,
    #[serde(default)]
    pub reverse: bool,
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
    PointLineDistance { point: u32, line: u32, value: f64 },
    LineDistance { a: u32, b: u32, value: f64 },
    Symmetric { a: u32, b: u32, line: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OperationDto {
    Boss,
    Cut,
    Intersect,
    NewBody,
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
    Offset {
        feature: u32,
        face: Box<FaceRefDto>,
    },
    Instance {
        feature: u32,
        instance: [u32; 2],
        face: Box<FaceRefDto>,
    },
    Imported {
        feature: u32,
        index: u32,
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
    pub fn from_definition(definition: &PartDefinition, textures: &TextureTable) -> Self {
        let text = definition.equations_text();
        Self {
            appearances: appearances_dto(&definition.appearances, textures),
            material: definition
                .material
                .as_ref()
                .map(|m| material_dto(m, textures)),
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
            plane: plane_dto(&match &sketch.plane {
                SketchPlane::Standard(standard) => PlaneRef::Standard(*standard),
                SketchPlane::Face(face) => PlaneRef::Face(face.clone()),
                SketchPlane::Reference(id) => PlaneRef::Reference(*id),
            }),
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
            origin: sketch.geometry.recorded_origin().map(|id| id.0),
        },
        FeatureKind::Extrude(extrude) => KindDto::Extrude {
            sketch: extrude.sketch.raw(),
            operation: operation_dto(extrude.op),
            extent: match extrude.extent {
                ExtrudeExtent::Blind { depth } => ExtentDto::Blind { depth },
                ExtrudeExtent::MidPlane { depth } => ExtentDto::MidPlane { depth },
            },
            reverse: extrude.reverse,
            bodies: extrude.bodies.iter().map(body_dto).collect(),
        },
        FeatureKind::Revolve(revolve) => KindDto::Revolve {
            sketch: revolve.sketch.raw(),
            operation: operation_dto(revolve.op),
            axis: match &revolve.axis {
                RevolveAxis::SketchLine(line) => RevolveAxisDto::SketchLine(*line),
                RevolveAxis::Axis(axis) => RevolveAxisDto::Axis(axis_dto(axis)),
            },
            angle: revolve.angle,
            mid_plane: revolve.mid_plane,
            reverse: revolve.reverse,
            bodies: revolve.bodies.iter().map(body_dto).collect(),
        },
        FeatureKind::Fillet(blend) => KindDto::Fillet {
            edges: blend.edges.iter().map(edge_dto).collect(),
            radius: blend.size,
        },
        FeatureKind::Chamfer(blend) => KindDto::Chamfer {
            edges: blend.edges.iter().map(edge_dto).collect(),
            distance: blend.size,
        },
        FeatureKind::Shell(shell) => KindDto::Shell {
            faces: shell.faces.iter().map(face_dto).collect(),
            thickness: shell.thickness,
            outward: shell.outward,
        },
        FeatureKind::Plane(plane) => KindDto::Plane {
            base: plane_dto(&plane.base),
            offset: plane.offset,
            flip: plane.flip,
        },
        FeatureKind::Axis(axis) => KindDto::Axis {
            axis: match axis {
                RefAxis::Along(axis) => RefAxisDto::Along(axis_dto(axis)),
                RefAxis::TwoPlanes(a, b) => RefAxisDto::TwoPlanes([plane_dto(a), plane_dto(b)]),
            },
        },
        FeatureKind::LinearPattern(pattern) => KindDto::LinearPattern {
            seeds: ids(&pattern.seeds),
            direction: direction_dto(&pattern.first),
            direction2: pattern.second.as_ref().map(direction_dto),
        },
        FeatureKind::CircularPattern(pattern) => KindDto::CircularPattern {
            seeds: ids(&pattern.seeds),
            axis: axis_dto(&pattern.axis),
            angle: pattern.angle,
            count: pattern.count,
            equal_spacing: pattern.equal_spacing,
            reverse: pattern.reverse,
        },
        FeatureKind::Mirror(mirror) => KindDto::Mirror {
            seeds: ids(&mirror.seeds),
            plane: plane_dto(&mirror.plane),
        },
        FeatureKind::Import(import) => KindDto::Import {
            file_name: import.file_name.clone(),
            file: import_entry(feature.id()),
            apply_colors: import.apply_colors,
        },
        FeatureKind::Combine(combine) => KindDto::Combine {
            target: body_dto(&combine.target),
            tools: combine.tools.iter().map(body_dto).collect(),
            operation: match combine.op {
                CombineOp::Join => CombineOpDto::Join,
                CombineOp::Cut => CombineOpDto::Cut,
                CombineOp::Intersect => CombineOpDto::Intersect,
            },
            keep_tools: combine.keep_tools,
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

fn ids(features: &[FeatureId]) -> Vec<u32> {
    features.iter().map(|f| f.raw()).collect()
}

fn operation_dto(op: ExtrudeOp) -> OperationDto {
    match op {
        ExtrudeOp::Boss => OperationDto::Boss,
        ExtrudeOp::Cut => OperationDto::Cut,
        ExtrudeOp::Intersect => OperationDto::Intersect,
        ExtrudeOp::NewBody => OperationDto::NewBody,
    }
}

fn plane_dto(plane: &PlaneRef) -> PlaneDto {
    match plane {
        PlaneRef::Standard(StandardPlane::Front) => PlaneDto::Standard(StandardPlaneDto::Front),
        PlaneRef::Standard(StandardPlane::Top) => PlaneDto::Standard(StandardPlaneDto::Top),
        PlaneRef::Standard(StandardPlane::Right) => PlaneDto::Standard(StandardPlaneDto::Right),
        PlaneRef::Face(face) => PlaneDto::Face(face_dto(face)),
        PlaneRef::Reference(id) => PlaneDto::Reference(id.raw()),
    }
}

fn axis_dto(axis: &AxisRef) -> AxisRefDto {
    match axis {
        AxisRef::Standard(StandardAxis::X) => AxisRefDto::Standard(StandardAxisDto::X),
        AxisRef::Standard(StandardAxis::Y) => AxisRefDto::Standard(StandardAxisDto::Y),
        AxisRef::Standard(StandardAxis::Z) => AxisRefDto::Standard(StandardAxisDto::Z),
        AxisRef::Reference(id) => AxisRefDto::Reference(id.raw()),
        AxisRef::Edge(edge) => AxisRefDto::Edge(edge_dto(edge)),
        AxisRef::Face(face) => AxisRefDto::Face(face_dto(face)),
    }
}

fn direction_dto(direction: &PatternDirection) -> DirectionDto {
    DirectionDto {
        axis: axis_dto(&direction.axis),
        spacing: direction.spacing,
        count: direction.count,
        reverse: direction.reverse,
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
        C::PointLineDistance(p, l, value) => R::PointLineDistance {
            point: p.0,
            line: l.0,
            value,
        },
        C::LineDistance(a, b, value) => R::LineDistance {
            a: a.0,
            b: b.0,
            value,
        },
        C::Symmetric(a, b, line) => R::Symmetric {
            a: a.0,
            b: b.0,
            line: line.0,
        },
    }
}

pub(crate) fn face_dto(face: &FaceName) -> FaceRefDto {
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
        FaceKey::Offset { feature, face } => FaceKeyDto::Offset {
            feature: feature.raw(),
            face: Box::new(face_dto(face)),
        },
        FaceKey::Instance {
            feature,
            instance,
            face,
        } => FaceKeyDto::Instance {
            feature: feature.raw(),
            instance: *instance,
            face: Box::new(face_dto(face)),
        },
        FaceKey::Imported { feature, index } => FaceKeyDto::Imported {
            feature: feature.raw(),
            index: *index,
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

pub(crate) fn edge_dto(edge: &EdgeName) -> EdgeRefDto {
    EdgeRefDto {
        faces: edge.faces.iter().map(face_dto).collect(),
        part: edge.part,
    }
}

// ---- File → model --------------------------------------------------------------------------

impl PartModel {
    /// Converts to a model definition; fails on an inconsistent sketch or a missing
    /// imported file. `files` holds the archive's other entries (imported files) by name.
    pub fn into_definition(
        self,
        files: &HashMap<String, Vec<u8>>,
    ) -> Result<PartDefinition, String> {
        let mut definition = PartDefinition {
            features: self
                .features
                .into_iter()
                .map(|f| feature_from(f, files))
                .collect::<Result<_, _>>()?,
            rollback: self.rollback,
            next_id: self.next_feature_id,
            equations: Equations::default(),
            appearances: appearances_from(self.appearances, files)
                .map_err(|e| format!("appearances: {e}"))?,
            material: self
                .material
                .map(|m| material_from(m, files))
                .transpose()
                .map_err(|e| format!("material: {e}"))?,
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
        R::PointLineDistance { point, line, value } => {
            C::PointLineDistance(id(point), id(line), value)
        }
        R::LineDistance { a, b, value } => C::LineDistance(id(a), id(b), value),
        R::Symmetric { a, b, line } => C::Symmetric(id(a), id(b), id(line)),
    }
}

fn feature_from(dto: FeatureDto, files: &HashMap<String, Vec<u8>>) -> Result<Feature, String> {
    let kind = match dto.kind {
        KindDto::Sketch {
            plane,
            entities,
            relations,
            next_ids,
            origin,
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
            .and_then(|sketch| sketch.with_origin(origin.map(EntityId)))
            .map_err(|e| format!("{}: {e}", dto.name))?;
            FeatureKind::Sketch(Sketch {
                plane: match plane_from(plane) {
                    PlaneRef::Standard(standard) => SketchPlane::Standard(standard),
                    PlaneRef::Face(face) => SketchPlane::Face(face),
                    PlaneRef::Reference(id) => SketchPlane::Reference(id),
                },
                geometry,
            })
        }
        KindDto::Extrude {
            sketch,
            operation,
            extent,
            reverse,
            bodies,
        } => FeatureKind::Extrude(Extrude {
            sketch: FeatureId::from_raw(sketch),
            op: operation_from(operation),
            extent: match extent {
                ExtentDto::Blind { depth } => ExtrudeExtent::Blind { depth },
                ExtentDto::MidPlane { depth } => ExtrudeExtent::MidPlane { depth },
            },
            reverse,
            bodies: bodies.into_iter().map(body_from).collect(),
        }),
        KindDto::Fillet { edges, radius } => FeatureKind::Fillet(EdgeBlend {
            edges: edges.into_iter().map(edge_from).collect(),
            size: radius,
        }),
        KindDto::Chamfer { edges, distance } => FeatureKind::Chamfer(EdgeBlend {
            edges: edges.into_iter().map(edge_from).collect(),
            size: distance,
        }),
        KindDto::Revolve {
            sketch,
            operation,
            axis,
            angle,
            mid_plane,
            reverse,
            bodies,
        } => FeatureKind::Revolve(Revolve {
            sketch: FeatureId::from_raw(sketch),
            op: operation_from(operation),
            axis: match axis {
                RevolveAxisDto::SketchLine(line) => RevolveAxis::SketchLine(line),
                RevolveAxisDto::Axis(axis) => RevolveAxis::Axis(axis_from(axis)),
            },
            angle,
            mid_plane,
            reverse,
            bodies: bodies.into_iter().map(body_from).collect(),
        }),
        KindDto::Shell {
            faces,
            thickness,
            outward,
        } => FeatureKind::Shell(Shell {
            faces: faces.into_iter().map(face_from).collect(),
            thickness,
            outward,
        }),
        KindDto::Plane { base, offset, flip } => FeatureKind::Plane(RefPlane {
            base: plane_from(base),
            offset,
            flip,
        }),
        KindDto::Axis { axis } => FeatureKind::Axis(match axis {
            RefAxisDto::Along(axis) => RefAxis::Along(axis_from(axis)),
            RefAxisDto::TwoPlanes([a, b]) => RefAxis::TwoPlanes(plane_from(a), plane_from(b)),
        }),
        KindDto::LinearPattern {
            seeds,
            direction,
            direction2,
        } => FeatureKind::LinearPattern(LinearPattern {
            seeds: ids_from(seeds),
            first: direction_from(direction),
            second: direction2.map(direction_from),
        }),
        KindDto::CircularPattern {
            seeds,
            axis,
            angle,
            count,
            equal_spacing,
            reverse,
        } => FeatureKind::CircularPattern(CircularPattern {
            seeds: ids_from(seeds),
            axis: axis_from(axis),
            angle,
            count,
            equal_spacing,
            reverse,
        }),
        KindDto::Mirror { seeds, plane } => FeatureKind::Mirror(Mirror {
            seeds: ids_from(seeds),
            plane: plane_from(plane),
        }),
        KindDto::Import {
            file_name,
            file,
            apply_colors,
        } => {
            let bytes = files
                .get(&file)
                .ok_or_else(|| format!("{}: the imported file {file} is missing", dto.name))?;
            FeatureKind::Import(Import {
                file_name,
                data: FileData::new(bytes.clone()),
                apply_colors,
            })
        }
        KindDto::Combine {
            target,
            tools,
            operation,
            keep_tools,
        } => FeatureKind::Combine(Combine {
            target: body_from(target),
            tools: tools.into_iter().map(body_from).collect(),
            op: match operation {
                CombineOpDto::Join => CombineOp::Join,
                CombineOpDto::Cut => CombineOp::Cut,
                CombineOpDto::Intersect => CombineOp::Intersect,
            },
            keep_tools,
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

fn ids_from(ids: Vec<u32>) -> Vec<FeatureId> {
    ids.into_iter().map(FeatureId::from_raw).collect()
}

fn operation_from(operation: OperationDto) -> ExtrudeOp {
    match operation {
        OperationDto::Boss => ExtrudeOp::Boss,
        OperationDto::Cut => ExtrudeOp::Cut,
        OperationDto::Intersect => ExtrudeOp::Intersect,
        OperationDto::NewBody => ExtrudeOp::NewBody,
    }
}

fn plane_from(dto: PlaneDto) -> PlaneRef {
    match dto {
        PlaneDto::Standard(StandardPlaneDto::Front) => PlaneRef::Standard(StandardPlane::Front),
        PlaneDto::Standard(StandardPlaneDto::Top) => PlaneRef::Standard(StandardPlane::Top),
        PlaneDto::Standard(StandardPlaneDto::Right) => PlaneRef::Standard(StandardPlane::Right),
        PlaneDto::Face(face) => PlaneRef::Face(face_from(face)),
        PlaneDto::Reference(id) => PlaneRef::Reference(FeatureId::from_raw(id)),
    }
}

fn axis_from(dto: AxisRefDto) -> AxisRef {
    match dto {
        AxisRefDto::Standard(StandardAxisDto::X) => AxisRef::Standard(StandardAxis::X),
        AxisRefDto::Standard(StandardAxisDto::Y) => AxisRef::Standard(StandardAxis::Y),
        AxisRefDto::Standard(StandardAxisDto::Z) => AxisRef::Standard(StandardAxis::Z),
        AxisRefDto::Reference(id) => AxisRef::Reference(FeatureId::from_raw(id)),
        AxisRefDto::Edge(edge) => AxisRef::Edge(edge_from(edge)),
        AxisRefDto::Face(face) => AxisRef::Face(face_from(face)),
    }
}

fn direction_from(dto: DirectionDto) -> PatternDirection {
    PatternDirection {
        axis: axis_from(dto.axis),
        spacing: dto.spacing,
        count: dto.count,
        reverse: dto.reverse,
    }
}

pub(crate) fn face_from(dto: FaceRefDto) -> FaceName {
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
        FaceKeyDto::Offset { feature, face } => FaceKey::Offset {
            feature: FeatureId::from_raw(feature),
            face: Box::new(face_from(*face)),
        },
        FaceKeyDto::Instance {
            feature,
            instance,
            face,
        } => FaceKey::Instance {
            feature: FeatureId::from_raw(feature),
            instance,
            face: Box::new(face_from(*face)),
        },
        FaceKeyDto::Imported { feature, index } => FaceKey::Imported {
            feature: FeatureId::from_raw(feature),
            index,
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

pub(crate) fn edge_from(dto: EdgeRefDto) -> EdgeName {
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
