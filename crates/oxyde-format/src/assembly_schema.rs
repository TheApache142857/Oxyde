//! The JSON schema of `assembly.json` in `.oxyasm` files, version 1.

use oxyde_assembly::{
    Assembly, Component, ComponentId, Mate, MateId, MateKind, MateRef, MateTarget, Placement,
    TopLevelOnly,
};
use oxyde_geom::Vec3;
use oxyde_model::{EquationText, EquationsText, GlobalText, StandardAxis, StandardPlane};
use serde::{Deserialize, Serialize};

use std::collections::HashMap;

use crate::schema::{
    AppearanceDto, EdgeRefDto, EquationDto, FaceRefDto, GlobalDto, StandardAxisDto,
    StandardPlaneDto, TextureTable, appearance_dto, appearance_from, edge_dto, edge_from, face_dto,
    face_from,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AssemblyModel {
    pub next_component_id: u32,
    pub next_mate_id: u32,
    pub components: Vec<ComponentDto>,
    #[serde(default)]
    pub mates: Vec<MateDto>,
    /// `"Gap" = 2`: names are unique, ids never reused.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global_variables: Vec<GlobalDto>,
    /// `"D1@Distance1" = "Gap" * 2`, `"D1@Sketch1@Top panel<1>.Part" = "Width" - 6`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub equations: Vec<EquationDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_global_id: Option<u32>,
    /// Linked equations file, relative to the assembly's folder; absent = none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equations_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ComponentDto {
    pub id: u32,
    pub name: String,
    /// Relative to the assembly's folder, `/` separators.
    pub path: String,
    #[serde(default)]
    pub document_id: String,
    /// Rows of the rotation matrix.
    pub rotation: [[f64; 3]; 3],
    pub translation: [f64; 3],
    #[serde(default)]
    pub fixed: bool,
    #[serde(default)]
    pub hidden: bool,
    /// Drawn with this instead of the part's own appearances, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<AppearanceDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MateDto {
    pub id: u32,
    pub name: String,
    #[serde(flatten)]
    pub kind: MateKindDto,
    pub a: MateRefDto,
    pub b: MateRefDto,
    #[serde(default)]
    pub aligned: bool,
    #[serde(default)]
    pub suppressed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum MateKindDto {
    Coincident,
    Concentric,
    Parallel,
    Perpendicular,
    /// mm.
    Distance {
        value: f64,
    },
    /// Radians.
    Angle {
        value: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MateRefDto {
    pub component: u32,
    /// Inside a sub-assembly: its component ids down to the part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<u32>,
    #[serde(flatten)]
    pub target: TargetDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TargetDto {
    Face(FaceRefDto),
    Edge(EdgeRefDto),
    Plane(StandardPlaneDto),
    Axis(StandardAxisDto),
    Origin(bool),
}

impl AssemblyModel {
    pub fn from_assembly(assembly: &Assembly, textures: &TextureTable) -> Self {
        // Names inside sub-assemblies are written as the application last resolved them.
        let text = assembly.equations_text(&TopLevelOnly);
        AssemblyModel {
            next_component_id: assembly.next_component_id,
            next_mate_id: assembly.next_mate_id,
            components: assembly
                .components
                .iter()
                .map(|c| ComponentDto {
                    id: c.id.0,
                    name: c.name.clone(),
                    path: c.path.clone(),
                    document_id: c.document_id.clone(),
                    rotation: c.placement.rotation,
                    translation: [
                        c.placement.translation.x,
                        c.placement.translation.y,
                        c.placement.translation.z,
                    ],
                    fixed: c.fixed,
                    hidden: c.hidden,
                    appearance: c.appearance.as_ref().map(|a| appearance_dto(a, textures)),
                })
                .collect(),
            mates: assembly
                .mates
                .iter()
                .map(|m| MateDto {
                    id: m.id.0,
                    name: m.name.clone(),
                    kind: match m.kind {
                        MateKind::Coincident => MateKindDto::Coincident,
                        MateKind::Concentric => MateKindDto::Concentric,
                        MateKind::Parallel => MateKindDto::Parallel,
                        MateKind::Perpendicular => MateKindDto::Perpendicular,
                        MateKind::Distance(value) => MateKindDto::Distance { value },
                        MateKind::Angle(value) => MateKindDto::Angle { value },
                    },
                    a: ref_dto(&m.a),
                    b: ref_dto(&m.b),
                    aligned: m.aligned,
                    suppressed: m.suppressed,
                })
                .collect(),
            next_global_id: (text.next_global_id > 1).then_some(text.next_global_id),
            equations_file: text.linked_file.clone(),
            global_variables: text
                .globals
                .into_iter()
                .map(|g| GlobalDto {
                    id: g.id,
                    name: g.name,
                    expression: g.expression,
                    linked: g.linked,
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

    /// The assembly, checked: unique ids and names, mates between existing components.
    /// `files` holds the archive's `textures/` entries by name.
    pub fn into_assembly(self, files: &HashMap<String, Vec<u8>>) -> Result<Assembly, String> {
        let components: Vec<Component> = self
            .components
            .into_iter()
            .map(|c| {
                let stored = Placement {
                    rotation: c.rotation,
                    translation: Vec3::new(c.translation[0], c.translation[1], c.translation[2]),
                };
                // A rotation (to 1e-6), not a scaling or a mirror; kept exactly as stored.
                let placement = Placement::from_transform(&stored.transform())
                    .map(|_| stored)
                    .ok_or_else(|| {
                        format!(
                            "component {} is not placed by a rotation and a translation",
                            c.name
                        )
                    })?;
                let appearance = match c.appearance {
                    Some(a) => Some(
                        appearance_from(a, files)
                            .map_err(|e| format!("component {}: {e}", c.name))?,
                    ),
                    None => None,
                };
                Ok(Component {
                    id: ComponentId(c.id),
                    name: c.name,
                    path: c.path,
                    document_id: c.document_id,
                    placement,
                    fixed: c.fixed,
                    hidden: c.hidden,
                    appearance,
                })
            })
            .collect::<Result<_, String>>()?;
        let mates: Vec<Mate> = self
            .mates
            .into_iter()
            .map(|m| Mate {
                id: MateId(m.id),
                name: m.name,
                kind: match m.kind {
                    MateKindDto::Coincident => MateKind::Coincident,
                    MateKindDto::Concentric => MateKind::Concentric,
                    MateKindDto::Parallel => MateKind::Parallel,
                    MateKindDto::Perpendicular => MateKind::Perpendicular,
                    MateKindDto::Distance { value } => MateKind::Distance(value),
                    MateKindDto::Angle { value } => MateKind::Angle(value),
                },
                a: ref_from(m.a),
                b: ref_from(m.b),
                aligned: m.aligned,
                suppressed: m.suppressed,
            })
            .collect();

        let mut seen = std::collections::HashSet::new();
        for c in &components {
            if !seen.insert(c.id) {
                return Err(format!("component id {} is used twice", c.id.0));
            }
            if c.id.0 >= self.next_component_id {
                return Err(format!(
                    "component id {} is not below next_component_id",
                    c.id.0
                ));
            }
            if components.iter().filter(|o| o.name == c.name).count() > 1 {
                return Err(format!("component name {} is used twice", c.name));
            }
            if c.path.is_empty() {
                return Err(format!("component {} has no path", c.name));
            }
        }
        let mut seen = std::collections::HashSet::new();
        for m in &mates {
            if !seen.insert(m.id) {
                return Err(format!("mate id {} is used twice", m.id.0));
            }
            if m.id.0 >= self.next_mate_id {
                return Err(format!("mate id {} is not below next_mate_id", m.id.0));
            }
            for side in [&m.a, &m.b] {
                // Component 0 is the assembly itself (its own planes and origin).
                if side.component != ComponentId::ASSEMBLY
                    && !components.iter().any(|c| c.id == side.component)
                {
                    return Err(format!(
                        "mate {} refers to component {}, which does not exist",
                        m.name, side.component.0
                    ));
                }
            }
        }
        let mut assembly = Assembly {
            components,
            mates,
            next_component_id: self.next_component_id,
            next_mate_id: self.next_mate_id,
            equations: Default::default(),
        };
        // Names reaching into sub-assemblies are found once the application has loaded
        // them (`Assembly::resolve_equation_names`).
        assembly
            .set_equations_text(
                EquationsText {
                    globals: self
                        .global_variables
                        .into_iter()
                        .map(|g| GlobalText {
                            id: g.id,
                            name: g.name,
                            expression: g.expression,
                            linked: g.linked,
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
                    linked_file: self.equations_file,
                },
                &TopLevelOnly,
            )
            .map_err(|e| format!("equations: {e}"))?;
        Ok(assembly)
    }
}

fn ref_dto(r: &MateRef) -> MateRefDto {
    MateRefDto {
        component: r.component.0,
        path: r.path.iter().map(|c| c.0).collect(),
        target: match &r.target {
            MateTarget::Face(f) => TargetDto::Face(face_dto(f)),
            MateTarget::Edge(e) => TargetDto::Edge(edge_dto(e)),
            MateTarget::Plane(p) => TargetDto::Plane(match p {
                StandardPlane::Front => StandardPlaneDto::Front,
                StandardPlane::Top => StandardPlaneDto::Top,
                StandardPlane::Right => StandardPlaneDto::Right,
            }),
            MateTarget::Axis(a) => TargetDto::Axis(match a {
                StandardAxis::X => StandardAxisDto::X,
                StandardAxis::Y => StandardAxisDto::Y,
                StandardAxis::Z => StandardAxisDto::Z,
            }),
            MateTarget::Origin => TargetDto::Origin(true),
        },
    }
}

fn ref_from(dto: MateRefDto) -> MateRef {
    MateRef {
        component: ComponentId(dto.component),
        path: dto.path.into_iter().map(ComponentId).collect(),
        target: match dto.target {
            TargetDto::Face(f) => MateTarget::Face(face_from(f)),
            TargetDto::Edge(e) => MateTarget::Edge(edge_from(e)),
            TargetDto::Plane(p) => MateTarget::Plane(match p {
                StandardPlaneDto::Front => StandardPlane::Front,
                StandardPlaneDto::Top => StandardPlane::Top,
                StandardPlaneDto::Right => StandardPlane::Right,
            }),
            TargetDto::Axis(a) => MateTarget::Axis(match a {
                StandardAxisDto::X => StandardAxis::X,
                StandardAxisDto::Y => StandardAxis::Y,
                StandardAxisDto::Z => StandardAxis::Z,
            }),
            TargetDto::Origin(_) => MateTarget::Origin,
        },
    }
}
