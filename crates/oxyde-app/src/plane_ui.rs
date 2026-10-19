//! The Plane command, as SolidWorks' Plane PropertyManager: up to three references, each a
//! plane or a flat face, a line (a straight edge, a sketch line, an axis), a curve or a
//! point, with the constraint that ties the new plane to it. Together they are one of the
//! model's plane constructions ([`RefPlane`]): at an offset, at an angle about a line,
//! through a line parallel or perpendicular to a plane, through three points, or normal to
//! a curve at a point. Until they are, the panel says what is missing.
//!
//! Clicks fill the active box, then move on to the next empty one; clicking something a
//! box holds takes it out. The constraint each reference starts with is the one the
//! others suggest (an edge picked beside a face turns the face to *At angle*), and can be
//! changed with the buttons under it.

use eframe::egui::{self, Color32, RichText, Stroke};
use oxyde_model::{
    AxisRef, CurveRef, EdgeName, FaceName, FeatureId, FeatureKind, FeatureParameter, PlaneRef,
    PointRef, RefPlane, VertexName,
};
use oxyde_sketch::{EntityId, Geometry};

use crate::app::{OxydeApp, Selected};
use crate::dialogs::{Dialog, plane_name};
use crate::features_ui::Choices;
use crate::property_manager::{PanelPick, box_active, box_fill, section};

/// Something a plane can be built from.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaneEntity {
    /// A standard or reference plane, or a flat face.
    Plane(PlaneRef),
    /// A model or reference axis, or the axis of a round face: a line.
    Axis(AxisRef),
    /// An edge: a curve, and a line too when it is straight (or a circle, by its axis).
    Edge {
        edge: EdgeName,
        line: bool,
    },
    /// A line, an arc or a circle of a sketch.
    SketchCurve {
        sketch: FeatureId,
        entity: u32,
        line: bool,
    },
    Point(PointRef),
}

impl PlaneEntity {
    /// The constraints it can take; the first is its usual one.
    fn constraints(&self) -> &'static [PlaneConstraint] {
        use PlaneConstraint as C;
        match self {
            PlaneEntity::Plane(_) => &[C::Offset, C::Angle, C::Parallel, C::Perpendicular],
            PlaneEntity::Axis(_) | PlaneEntity::Point(_) => &[C::Coincident],
            PlaneEntity::Edge { line: true, .. } | PlaneEntity::SketchCurve { line: true, .. } => {
                &[C::Coincident, C::Perpendicular]
            }
            PlaneEntity::Edge { .. } | PlaneEntity::SketchCurve { .. } => &[C::Perpendicular],
        }
    }

    fn plane(&self) -> Option<&PlaneRef> {
        match self {
            PlaneEntity::Plane(p) => Some(p),
            _ => None,
        }
    }

    /// The line it stands for, when it is one.
    fn line(&self) -> Option<AxisRef> {
        match self {
            PlaneEntity::Axis(axis) => Some(axis.clone()),
            PlaneEntity::Edge { edge, line: true } => Some(AxisRef::Edge(edge.clone())),
            PlaneEntity::SketchCurve {
                sketch,
                entity,
                line: true,
            } => Some(AxisRef::SketchLine {
                sketch: *sketch,
                entity: *entity,
            }),
            _ => None,
        }
    }

    fn curve(&self) -> Option<CurveRef> {
        match self {
            PlaneEntity::Edge { edge, .. } => Some(CurveRef::Edge(edge.clone())),
            PlaneEntity::SketchCurve { sketch, entity, .. } => Some(CurveRef::SketchCurve {
                sketch: *sketch,
                entity: *entity,
            }),
            _ => None,
        }
    }

    fn point(&self) -> Option<&PointRef> {
        match self {
            PlaneEntity::Point(p) => Some(p),
            _ => None,
        }
    }
}

/// How the new plane stands to a reference, as SolidWorks' constraint buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneConstraint {
    /// Parallel to a plane, at the offset distance.
    Offset,
    /// At the angle to a plane, about a line lying in it.
    Angle,
    /// Parallel to a plane, through a line.
    Parallel,
    /// Square to a plane through a line, or to a curve at a point.
    Perpendicular,
    /// Through a line or a point.
    Coincident,
}

impl PlaneConstraint {
    fn label(self) -> &'static str {
        match self {
            PlaneConstraint::Offset => "Offset",
            PlaneConstraint::Angle => "At angle",
            PlaneConstraint::Parallel => "Parallel",
            PlaneConstraint::Perpendicular => "Perpendicular",
            PlaneConstraint::Coincident => "Coincident",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            PlaneConstraint::Offset => "Parallel to this plane, at a distance",
            PlaneConstraint::Angle => {
                "At an angle to this plane, about a line lying in it (another reference)"
            }
            PlaneConstraint::Parallel => "Parallel to this plane, through a line",
            PlaneConstraint::Perpendicular => {
                "Square to this: to a plane through a line, or to a curve at a point"
            }
            PlaneConstraint::Coincident => "Through this line or point",
        }
    }
}

/// A box of the panel, filled.
#[derive(Debug, Clone)]
pub struct PlaneReference {
    pub entity: PlaneEntity,
    pub label: String,
    pub constraint: PlaneConstraint,
}

pub struct PlaneDialog {
    pub editing: Option<FeatureId>,
    /// Full names of the plane's offset and angle dimensions, when it has them.
    pub offset_dimension: Option<String>,
    pub angle_dimension: Option<String>,
    /// The first, second and third references.
    pub refs: [Option<PlaneReference>; 3],
    /// The box the next click fills.
    pub active: usize,
    /// Millimetres.
    pub offset: f64,
    /// Degrees.
    pub angle: f64,
    pub flip: bool,
    /// The planes and axes the boxes' lists offer.
    pub choices: Choices<PlaneEntity>,
}

const TITLES: [&str; 3] = ["First reference", "Second reference", "Third reference"];

impl PlaneDialog {
    /// The plane the references make, or what they still need.
    pub fn construction(&self) -> Result<RefPlane, String> {
        use PlaneConstraint as C;
        let refs: Vec<&PlaneReference> = self.refs.iter().flatten().collect();
        let planes: Vec<(&PlaneRef, C)> = refs
            .iter()
            .filter_map(|r| r.entity.plane().map(|p| (p, r.constraint)))
            .collect();
        let lines: Vec<AxisRef> = refs
            .iter()
            .filter(|r| r.constraint == C::Coincident)
            .filter_map(|r| r.entity.line())
            .collect();
        let curves: Vec<CurveRef> = refs
            .iter()
            .filter(|r| r.constraint == C::Perpendicular)
            .filter_map(|r| r.entity.curve())
            .collect();
        let points: Vec<PointRef> = refs
            .iter()
            .filter_map(|r| r.entity.point().cloned())
            .collect();
        let flip = self.flip;
        let n = refs.len();
        match (
            planes.as_slice(),
            lines.as_slice(),
            curves.as_slice(),
            points.as_slice(),
            n,
        ) {
            ([(base, C::Offset)], [], [], [], 1) => Ok(RefPlane::Offset {
                base: (*base).clone(),
                offset: self.offset,
                flip,
            }),
            ([(base, C::Angle)], [line], [], [], 2) => Ok(RefPlane::Angle {
                base: (*base).clone(),
                line: line.clone(),
                angle: self.angle.to_radians(),
                flip,
            }),
            ([(plane, c @ (C::Parallel | C::Perpendicular))], [line], [], [], 2) => {
                Ok(RefPlane::ThroughLine {
                    line: line.clone(),
                    plane: (*plane).clone(),
                    perpendicular: *c == C::Perpendicular,
                    flip,
                })
            }
            ([], [], [curve], [point], 2) => Ok(RefPlane::NormalToCurve {
                curve: curve.clone(),
                point: point.clone(),
                flip,
            }),
            ([], [], [], [a, b, c], 3) => Ok(RefPlane::ThreePoints {
                points: [a.clone(), b.clone(), c.clone()],
                flip,
            }),
            _ => Err(self.missing(&planes, lines.len(), curves.len(), points.len())),
        }
    }

    /// What the references still need, when they make no plane yet.
    fn missing(
        &self,
        planes: &[(&PlaneRef, PlaneConstraint)],
        lines: usize,
        curves: usize,
        points: usize,
    ) -> String {
        use PlaneConstraint as C;
        let n = self.refs.iter().flatten().count();
        let text = match (planes, lines, curves, points) {
            _ if n == 0 => "Select a plane or a flat face, a line, a curve or points",
            ([_, _, ..], ..) => "Use one plane or face only: take the others out",
            ([_], 0, 0, 1..) => {
                "A plane through a point and parallel to a plane is not offered yet: pick a line \
                 instead of the point"
            }
            ([(_, C::Offset)], ..) => {
                "An offset plane needs its plane alone: take the other references out, or \
                 choose At angle, Parallel or Perpendicular"
            }
            ([(_, C::Angle)], 0, ..) => {
                "Select the line to turn about: an edge, a sketch line or an axis lying in the \
                 plane"
            }
            ([(_, _)], 0, ..) => {
                "Select the line the plane goes through: an edge, a sketch line or an axis"
            }
            ([], 1, 0, 0) => {
                "Select a plane or a flat face: the new plane is at an angle, parallel or \
                 perpendicular to it"
            }
            ([], 1, 0, _) => {
                "A plane through a line and a point is not offered yet: make the curve \
                 Perpendicular for a plane normal to it at the point"
            }
            ([], 0, 1, 0) => {
                "Select the point where the plane crosses the curve: a vertex or a sketch point"
            }
            ([], 0, 0, 1) => "Select two more points, or a curve the plane is normal to",
            ([], 0, 0, 2) => "Select a third point",
            _ => "These references do not make a plane: check the constraints under them",
        };
        text.to_owned()
    }

    pub fn feature(&self) -> Result<FeatureKind, String> {
        self.construction().map(FeatureKind::Plane)
    }

    /// The sketch curves it holds, by sketch and entity id, to draw thicker.
    pub fn held_sketch_curves(&self) -> Vec<(FeatureId, u32)> {
        self.refs
            .iter()
            .flatten()
            .filter_map(|r| match &r.entity {
                PlaneEntity::SketchCurve { sketch, entity, .. }
                | PlaneEntity::Axis(AxisRef::SketchLine { sketch, entity }) => {
                    Some((*sketch, *entity))
                }
                _ => None,
            })
            .collect()
    }

    /// The features it holds (reference planes and axes, sketches), to highlight in the
    /// tree and the graphics area.
    pub fn features(&self) -> Vec<FeatureId> {
        self.refs
            .iter()
            .flatten()
            .filter_map(|r| match &r.entity {
                PlaneEntity::Plane(PlaneRef::Reference(id))
                | PlaneEntity::Axis(AxisRef::Reference(id))
                | PlaneEntity::Axis(AxisRef::SketchLine { sketch: id, .. })
                | PlaneEntity::SketchCurve { sketch: id, .. }
                | PlaneEntity::Point(PointRef::SketchPoint { sketch: id, .. }) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// What `flip` does for the references as they stand.
    fn flip_label(&self) -> &'static str {
        let constraints = || self.refs.iter().flatten().map(|r| r.constraint);
        if constraints().any(|c| c == PlaneConstraint::Offset) {
            "Flip offset"
        } else if constraints().any(|c| c == PlaneConstraint::Angle) {
            "Flip angle"
        } else {
            "Flip normal"
        }
    }

    /// Puts `entity` in the active box, or takes it out of the box holding it, as a click
    /// does in SolidWorks; then moves on to the next empty box.
    pub fn take(&mut self, entity: PlaneEntity, label: String) {
        if let Some(i) = self
            .refs
            .iter()
            .position(|r| r.as_ref().is_some_and(|r| r.entity == entity))
        {
            self.put(i, None);
            self.active = i;
            return;
        }
        let slot = self.active.min(self.refs.len() - 1);
        self.put(slot, None);
        let constraint = self.usual_constraint(&entity);
        self.put(
            slot,
            Some(PlaneReference {
                entity,
                label,
                constraint,
            }),
        );
        self.suggest_from(slot);
        self.active = (1..=self.refs.len())
            .map(|k| (slot + k) % self.refs.len())
            .find(|&i| self.held(i).is_none())
            .unwrap_or(slot);
    }

    /// What box `slot` holds.
    pub fn held(&self, slot: usize) -> Option<&PlaneReference> {
        self.refs.get(slot).and_then(Option::as_ref)
    }

    /// Fills or empties box `slot`.
    pub fn put(&mut self, slot: usize, reference: Option<PlaneReference>) {
        if let Some(r) = self.refs.get_mut(slot) {
            *r = reference;
        }
    }

    /// The constraint a new reference starts with, given the others.
    fn usual_constraint(&self, entity: &PlaneEntity) -> PlaneConstraint {
        use PlaneConstraint as C;
        let others = || self.refs.iter().flatten();
        match entity {
            PlaneEntity::Plane(_) => {
                if others().any(|r| r.entity.line().is_some() && r.constraint == C::Coincident) {
                    C::Angle
                } else {
                    C::Offset
                }
            }
            PlaneEntity::Edge { line: true, .. } | PlaneEntity::SketchCurve { line: true, .. }
                if others().any(|r| r.entity.point().is_some()) =>
            {
                C::Perpendicular
            }
            other => other
                .constraints()
                .first()
                .copied()
                .unwrap_or(C::Coincident),
        }
    }

    /// The others' constraints the reference in `slot` suggests: a line turns an offset
    /// plane into one at an angle about it; a point turns a line into a curve the plane is
    /// normal to, when there is no plane.
    fn suggest_from(&mut self, slot: usize) {
        use PlaneConstraint as C;
        let Some(new) = self.held(slot).map(|r| (r.entity.clone(), r.constraint)) else {
            return;
        };
        let has_plane = self
            .refs
            .iter()
            .flatten()
            .any(|r| r.entity.plane().is_some());
        for (i, other) in self.refs.iter_mut().enumerate() {
            let Some(other) = other.as_mut().filter(|_| i != slot) else {
                continue;
            };
            let line = new.0.line().is_some() && new.1 == C::Coincident;
            if line && other.constraint == C::Offset {
                other.constraint = C::Angle;
            }
            if new.0.point().is_some()
                && !has_plane
                && other.constraint == C::Coincident
                && other.entity.curve().is_some()
            {
                other.constraint = C::Perpendicular;
            }
        }
    }
}

impl OxydeApp {
    /// What a reference is called in its box: "Top Plane", "Face of Boss-Extrude1",
    /// "Line 7 of Sketch2".
    fn plane_entity_label(&self, entity: &PlaneEntity) -> String {
        let made_by = |face: Option<&FaceName>| {
            face.and_then(|f| self.doc.part.feature(f.key.feature()))
                .map(|f| format!(" of {}", f.name()))
                .unwrap_or_default()
        };
        let feature = |id: FeatureId| {
            self.doc
                .part
                .feature(id)
                .map(|f| f.name().to_owned())
                .unwrap_or_else(|| "(missing)".to_owned())
        };
        let sketch_entity = |sketch: FeatureId, entity: u32| {
            let kind = match self.doc.part.feature(sketch).map(|f| f.kind()) {
                Some(FeatureKind::Sketch(s)) => match s.geometry.entity(EntityId(entity)) {
                    Some(e) => match e.geometry {
                        Geometry::Point(_) => "Point",
                        Geometry::Line { .. } if e.construction => "Centerline",
                        Geometry::Line { .. } => "Line",
                        Geometry::Circle { .. } => "Circle",
                        Geometry::Arc { .. } => "Arc",
                    },
                    None => "(deleted)",
                },
                _ => "Entity",
            };
            format!("{kind} {entity} of {}", feature(sketch))
        };
        match entity {
            PlaneEntity::Plane(PlaneRef::Standard(p)) => format!("{} Plane", plane_name(*p)),
            PlaneEntity::Plane(PlaneRef::Reference(id))
            | PlaneEntity::Axis(AxisRef::Reference(id)) => feature(*id),
            PlaneEntity::Plane(PlaneRef::Face(f)) | PlaneEntity::Axis(AxisRef::Face(f)) => {
                format!("Face{}", made_by(Some(f)))
            }
            PlaneEntity::Axis(AxisRef::Standard(axis)) => format!("{axis:?} axis"),
            PlaneEntity::Axis(AxisRef::Edge(e)) | PlaneEntity::Edge { edge: e, .. } => {
                format!("Edge{}", made_by(e.faces.first()))
            }
            PlaneEntity::Axis(AxisRef::SketchLine { sketch, entity }) => {
                sketch_entity(*sketch, *entity)
            }
            PlaneEntity::SketchCurve { sketch, entity, .. } => sketch_entity(*sketch, *entity),
            PlaneEntity::Point(PointRef::Vertex(v)) => {
                format!("Vertex{}", made_by(v.faces.first()))
            }
            PlaneEntity::Point(PointRef::SketchPoint { sketch, entity }) => {
                sketch_entity(*sketch, *entity)
            }
        }
    }

    /// Whether an edge is straight or circular: a line as well as a curve.
    fn edge_is_line(&self, edge: &EdgeName) -> bool {
        self.shown_body().is_some_and(|body| {
            body.naming
                .find_edge(edge)
                .and_then(|i| body.topology.edges.get(i as usize))
                .is_some_and(|e| e.axis.is_some())
        })
    }

    fn sketch_curve_entity(&self, sketch: FeatureId, entity: u32) -> Option<PlaneEntity> {
        let FeatureKind::Sketch(s) = self.doc.part.feature(sketch)?.kind() else {
            return None;
        };
        let line = match s.geometry.entity(EntityId(entity))?.geometry {
            Geometry::Point(_) => return None,
            Geometry::Line { .. } => true,
            Geometry::Circle { .. } | Geometry::Arc { .. } => false,
        };
        Some(PlaneEntity::SketchCurve {
            sketch,
            entity,
            line,
        })
    }

    /// What a click stands for in the Plane command, if it can use it.
    fn plane_entity_of_pick(
        &self,
        pick: &PanelPick,
        editing: Option<FeatureId>,
    ) -> Option<PlaneEntity> {
        let before = |id: FeatureId| self.usable(id, editing);
        Some(match pick {
            PanelPick::Plane(p) => PlaneEntity::Plane(PlaneRef::Standard(*p)),
            PanelPick::Face(f) => {
                let body = self.shown_body()?;
                let face = body
                    .topology
                    .faces
                    .get(body.naming.find_face(f)? as usize)?;
                if face.normal.is_some() {
                    PlaneEntity::Plane(PlaneRef::Face(f.clone()))
                } else {
                    face.axis?;
                    PlaneEntity::Axis(AxisRef::Face(f.clone()))
                }
            }
            PanelPick::Edge(e) => PlaneEntity::Edge {
                edge: e.clone(),
                line: self.edge_is_line(e),
            },
            PanelPick::Vertex(v) => PlaneEntity::Point(PointRef::Vertex(v.clone())),
            PanelPick::SketchPoint(sketch, entity) if before(*sketch) => {
                PlaneEntity::Point(PointRef::SketchPoint {
                    sketch: *sketch,
                    entity: *entity,
                })
            }
            PanelPick::SketchCurve(sketch, entity) if before(*sketch) => {
                self.sketch_curve_entity(*sketch, *entity)?
            }
            PanelPick::Feature(id) if before(*id) => match self.doc.part.feature(*id)?.kind() {
                FeatureKind::Plane(_) => PlaneEntity::Plane(PlaneRef::Reference(*id)),
                FeatureKind::Axis(_) => PlaneEntity::Axis(AxisRef::Reference(*id)),
                _ => return None,
            },
            _ => return None,
        })
    }

    /// A click offered to the Plane command; true when it took it.
    pub(crate) fn plane_pick(&self, d: &mut PlaneDialog, pick: &PanelPick) -> bool {
        match self.plane_entity_of_pick(pick, d.editing) {
            Some(entity) => {
                let label = self.plane_entity_label(&entity);
                d.take(entity, label);
                true
            }
            None => false,
        }
    }

    /// The planes and axes the boxes' lists offer: the standard ones and the reference
    /// geometry before the plane.
    fn plane_reference_choices(&self, editing: Option<FeatureId>) -> Choices<PlaneEntity> {
        let planes = self
            .plane_choices(editing)
            .into_iter()
            .filter(|(_, p)| !matches!(p, PlaneRef::Face(_)))
            .map(|(label, p)| (label, PlaneEntity::Plane(p)));
        let axes = self
            .axis_choices(editing)
            .into_iter()
            .filter(|(_, a)| matches!(a, AxisRef::Standard(_) | AxisRef::Reference(_)))
            .map(|(label, a)| (label, PlaneEntity::Axis(a)));
        planes.chain(axes).collect()
    }

    /// The Plane command: a new plane from what is selected (`existing` = `None`), or the
    /// plane `editing` as `existing` defines it.
    pub(crate) fn open_ref_plane(
        &mut self,
        editing: Option<FeatureId>,
        existing: Option<&RefPlane>,
    ) {
        use PlaneConstraint as C;
        let parameter = |p| editing.and_then(|id| self.parameter_full_name(id, p));
        let mut d = PlaneDialog {
            editing,
            offset_dimension: parameter(FeatureParameter::Offset),
            angle_dimension: parameter(FeatureParameter::Angle),
            refs: [None, None, None],
            active: 0,
            offset: 10.0,
            angle: 30.0,
            flip: existing.is_some_and(RefPlane::flipped),
            choices: self.plane_reference_choices(editing),
        };
        let line_entity = |axis: &AxisRef| match axis {
            AxisRef::Edge(edge) => PlaneEntity::Edge {
                edge: edge.clone(),
                line: true,
            },
            AxisRef::SketchLine { sketch, entity } => PlaneEntity::SketchCurve {
                sketch: *sketch,
                entity: *entity,
                line: true,
            },
            other => PlaneEntity::Axis(other.clone()),
        };
        let set = |d: &mut PlaneDialog, slot: usize, entity: PlaneEntity, constraint| {
            let label = self.plane_entity_label(&entity);
            d.put(
                slot,
                Some(PlaneReference {
                    entity,
                    label,
                    constraint,
                }),
            );
        };
        match existing {
            Some(RefPlane::Offset { base, offset, .. }) => {
                d.offset = *offset;
                set(&mut d, 0, PlaneEntity::Plane(base.clone()), C::Offset);
            }
            Some(RefPlane::Angle {
                base, line, angle, ..
            }) => {
                d.angle = angle.to_degrees();
                set(&mut d, 0, PlaneEntity::Plane(base.clone()), C::Angle);
                set(&mut d, 1, line_entity(line), C::Coincident);
            }
            Some(RefPlane::ThroughLine {
                line,
                plane,
                perpendicular,
                ..
            }) => {
                set(&mut d, 0, line_entity(line), C::Coincident);
                let c = if *perpendicular {
                    C::Perpendicular
                } else {
                    C::Parallel
                };
                set(&mut d, 1, PlaneEntity::Plane(plane.clone()), c);
            }
            Some(RefPlane::ThreePoints { points, .. }) => {
                for (slot, p) in points.iter().enumerate() {
                    set(&mut d, slot, PlaneEntity::Point(p.clone()), C::Coincident);
                }
            }
            Some(RefPlane::NormalToCurve { curve, point, .. }) => {
                let curve = match curve {
                    CurveRef::Edge(edge) => PlaneEntity::Edge {
                        edge: edge.clone(),
                        line: self.edge_is_line(edge),
                    },
                    CurveRef::SketchCurve { sketch, entity } => self
                        .sketch_curve_entity(*sketch, *entity)
                        .unwrap_or(PlaneEntity::SketchCurve {
                            sketch: *sketch,
                            entity: *entity,
                            line: false,
                        }),
                };
                set(&mut d, 0, curve, C::Perpendicular);
                set(&mut d, 1, PlaneEntity::Point(point.clone()), C::Coincident);
            }
            None => {
                // From what is selected: the plane in the tree or the graphics area, then
                // the faces and edges, in the order they were picked.
                let mut picks = Vec::new();
                if let Some(plane) = self.selected_plane {
                    picks.push(PanelPick::Plane(plane));
                }
                if let Some(id) = self.selected_feature {
                    picks.push(PanelPick::Feature(id));
                }
                picks.extend(self.selection.iter().map(|s| match s {
                    Selected::Face(f) => PanelPick::Face(f.clone()),
                    Selected::Edge(e) => PanelPick::Edge(e.clone()),
                }));
                for pick in picks.iter().take(3) {
                    self.plane_pick(&mut d, pick);
                }
            }
        }
        if existing.is_some() {
            d.active = d.refs.iter().position(Option::is_none).unwrap_or(0);
        }
        self.open_dialog(Dialog::RefPlane(d));
    }

    pub(crate) fn apply_ref_plane(&mut self, d: PlaneDialog) {
        match d.feature() {
            Ok(kind) => self.add_or_replace(d.editing, kind),
            Err(missing) => self.status = missing,
        }
    }

    /// The faces and edges the Plane command holds, to highlight.
    pub(crate) fn plane_dialog_picks(&self, d: &PlaneDialog) -> Vec<oxyde_render::Pick> {
        let Some(body) = self.shown_body() else {
            return Vec::new();
        };
        d.refs
            .iter()
            .flatten()
            .filter_map(|r| match &r.entity {
                PlaneEntity::Plane(PlaneRef::Face(f)) | PlaneEntity::Axis(AxisRef::Face(f)) => {
                    body.naming.find_face(f).map(oxyde_render::Pick::Face)
                }
                PlaneEntity::Edge { edge, .. } | PlaneEntity::Axis(AxisRef::Edge(edge)) => {
                    body.naming.find_edge(edge).map(oxyde_render::Pick::Edge)
                }
                _ => None,
            })
            .collect()
    }

    /// The points the Plane command holds, in model space, to draw as dots.
    pub(crate) fn plane_dialog_points(&self, d: &PlaneDialog) -> Vec<oxyde_geom::Point3> {
        d.refs
            .iter()
            .flatten()
            .filter_map(|r| match r.entity.point()? {
                PointRef::Vertex(v) => self.vertex_position(v),
                PointRef::SketchPoint { sketch, entity } => {
                    let result = self.doc.part.sketch_result(*sketch)?;
                    let p = result.solved.position(EntityId(*entity))?;
                    Some(result.plane.to_world(p))
                }
            })
            .collect()
    }

    pub(crate) fn vertex_position(&self, vertex: &VertexName) -> Option<oxyde_geom::Point3> {
        let body = self.shown_body()?;
        let i = body.naming.find_vertex(vertex)?;
        body.topology.vertices.get(i as usize).map(|v| v.point)
    }
}

/// The Plane command's panel.
pub(crate) fn ref_plane_body(ui: &mut egui::Ui, d: &mut PlaneDialog) {
    // As SolidWorks' message box: fully defined, or what is missing.
    match d.construction() {
        Ok(_) => {
            ui.horizontal(|ui| {
                crate::icons::small(ui, crate::icons::Icon::Ok, 16.0);
                ui.label("Fully defined");
            });
        }
        Err(missing) => {
            ui.label(RichText::new(missing).italics());
        }
    }
    for (slot, title) in TITLES.into_iter().enumerate() {
        // The next box shows once the one before it is filled, as in SolidWorks.
        if slot > 0 && d.held(slot).is_none() && d.held(slot - 1).is_none() {
            break;
        }
        section(ui, title);
        let active = d.active == slot;
        match slot_box(ui, slot, d.held(slot), active, &d.choices) {
            SlotAction::Activate => d.active = slot,
            SlotAction::Clear => {
                d.put(slot, None);
                d.active = slot;
            }
            SlotAction::Choose(i) => {
                if let Some((label, entity)) = d.choices.get(i).cloned() {
                    d.active = slot;
                    d.take(entity, label);
                }
            }
            SlotAction::None => {}
        }
        let Some(reference) = d.refs.get_mut(slot).and_then(Option::as_mut) else {
            continue;
        };
        ui.horizontal_wrapped(|ui| {
            for &c in reference.entity.constraints() {
                if ui
                    .selectable_label(reference.constraint == c, c.label())
                    .on_hover_text(c.hint())
                    .clicked()
                {
                    reference.constraint = c;
                }
            }
        });
        match reference.constraint {
            PlaneConstraint::Offset => {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut d.offset)
                            .speed(0.1)
                            .range(-crate::dialogs::MAX_LENGTH..=crate::dialogs::MAX_LENGTH)
                            .suffix(" mm")
                            .custom_parser(crate::dialogs::length_parser),
                    );
                    note(ui, d.offset_dimension.as_ref());
                });
            }
            PlaneConstraint::Angle => {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut d.angle)
                            .speed(1.0)
                            .range(0.0..=360.0)
                            .suffix("°")
                            .custom_parser(crate::dialogs::angle_parser),
                    );
                    note(ui, d.angle_dimension.as_ref());
                });
            }
            _ => {}
        }
    }
    ui.add_space(4.0);
    let label = d.flip_label();
    ui.checkbox(&mut d.flip, label);
}

fn note(ui: &mut egui::Ui, name: Option<&String>) {
    if let Some(name) = name {
        ui.label(RichText::new(name).small().weak());
    }
}

enum SlotAction {
    None,
    /// The box was clicked: it takes the next click.
    Activate,
    /// Its reference was taken out.
    Clear,
    /// A plane or an axis was chosen from its list.
    Choose(usize),
}

/// One reference box: what it holds (or a hint), a button taking it out, and a list of
/// the planes and axes. The active box, outlined, takes the next click in the graphics
/// area or the tree.
fn slot_box(
    ui: &mut egui::Ui,
    slot: usize,
    reference: Option<&PlaneReference>,
    active: bool,
    choices: &Choices<PlaneEntity>,
) -> SlotAction {
    let mut action = SlotAction::None;
    let stroke = if active {
        Stroke::new(2.0_f32, box_active())
    } else {
        Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color)
    };
    ui.push_id(("plane_reference", slot), |ui| {
        egui::Frame::none()
            .fill(if active {
                box_fill()
            } else {
                Color32::TRANSPARENT
            })
            .stroke(stroke)
            .rounding(3.0)
            .inner_margin(egui::Margin::symmetric(6.0, 4.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                // `horizontal` first: a bare right-to-left layout takes all the height left
                // in the panel (see `reference_box`).
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("⏷", |ui| {
                            for (i, (label, _)) in choices.iter().enumerate() {
                                if ui.selectable_label(false, label).clicked() {
                                    action = SlotAction::Choose(i);
                                    ui.close_menu();
                                }
                            }
                        })
                        .response
                        .on_hover_text("Choose a plane or an axis from the list");
                        if reference.is_some()
                            && crate::icons::icon_button(ui, crate::icons::Icon::Cancel, 16.0, true)
                                .on_hover_text("Take it out")
                                .clicked()
                        {
                            action = SlotAction::Clear;
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let text = match reference {
                                Some(r) => RichText::new(&r.label),
                                None => RichText::new(
                                    "Select a plane, a face, an edge, a sketch line or a point",
                                )
                                .italics()
                                .weak(),
                            };
                            let label = ui.add(
                                egui::Label::new(text)
                                    .truncate()
                                    .sense(egui::Sense::click()),
                            );
                            let rest = ui.allocate_response(
                                egui::vec2(ui.available_width(), label.rect.height()),
                                egui::Sense::click(),
                            );
                            let hover = if active {
                                "Click in the graphics area or the tree to fill this in"
                            } else {
                                "Click here, then in the graphics area or the tree"
                            };
                            if label.on_hover_text(hover).clicked()
                                || rest.on_hover_text(hover).clicked()
                            {
                                action = SlotAction::Activate;
                            }
                        });
                    });
                });
            });
    });
    action
}

#[cfg(test)]
mod tests {
    use oxyde_model::{FaceName, StandardPlane};

    use super::*;

    fn dialog() -> PlaneDialog {
        PlaneDialog {
            editing: None,
            offset_dimension: None,
            angle_dimension: None,
            refs: [None, None, None],
            active: 0,
            offset: 10.0,
            angle: 30.0,
            flip: false,
            choices: Vec::new(),
        }
    }

    fn top() -> PlaneEntity {
        PlaneEntity::Plane(PlaneRef::Standard(StandardPlane::Top))
    }

    fn edge(line: bool) -> PlaneEntity {
        let id = FeatureId::from_raw(2);
        PlaneEntity::Edge {
            edge: EdgeName::between(FaceName::profile(id, 1), FaceName::end_cap(id)),
            line,
        }
    }

    fn vertex(n: u32) -> PlaneEntity {
        PlaneEntity::Point(PointRef::SketchPoint {
            sketch: FeatureId::from_raw(1),
            entity: n,
        })
    }

    #[test]
    fn a_plane_alone_is_an_offset_plane() {
        let mut d = dialog();
        assert!(d.construction().is_err());
        d.take(top(), "Top Plane".into());
        assert!(matches!(d.construction(), Ok(RefPlane::Offset { offset, .. }) if offset == 10.0));
        assert_eq!(d.active, 1);
    }

    #[test]
    fn an_edge_beside_a_plane_turns_it_to_an_angle() {
        let mut d = dialog();
        d.take(top(), "Top Plane".into());
        d.take(edge(true), "Edge".into());
        match d.construction() {
            Ok(RefPlane::Angle { angle, .. }) => {
                assert!((angle - 30f64.to_radians()).abs() < 1e-12)
            }
            other => panic!("expected an angle plane, got {other:?}"),
        }
        // Parallel to the plane through the edge instead.
        d.refs[0].as_mut().unwrap().constraint = PlaneConstraint::Parallel;
        assert!(matches!(
            d.construction(),
            Ok(RefPlane::ThroughLine {
                perpendicular: false,
                ..
            })
        ));
    }

    #[test]
    fn a_line_then_a_plane_starts_at_an_angle_too() {
        let mut d = dialog();
        d.take(edge(true), "Edge".into());
        assert!(d.construction().is_err());
        d.take(top(), "Top Plane".into());
        assert!(matches!(d.construction(), Ok(RefPlane::Angle { .. })));
    }

    #[test]
    fn three_points_and_a_curve_with_a_point() {
        let mut d = dialog();
        for n in 1..=3 {
            d.take(vertex(n), format!("Point {n}"));
        }
        match d.construction() {
            Ok(RefPlane::ThreePoints { points, .. }) => {
                assert_eq!(points[2], vertex(3).point().cloned().unwrap())
            }
            other => panic!("expected three points, got {other:?}"),
        }
        // Clicking a point held takes it out.
        d.take(vertex(2), String::new());
        assert!(d.refs[1].is_none() && d.active == 1);

        let mut d = dialog();
        d.take(edge(true), "Edge".into());
        d.take(vertex(1), "Point".into());
        assert!(matches!(
            d.construction(),
            Ok(RefPlane::NormalToCurve { .. })
        ));
        let mut d = dialog();
        d.take(edge(false), "Edge".into());
        assert_eq!(
            d.refs[0].as_ref().unwrap().constraint,
            PlaneConstraint::Perpendicular
        );
    }
}
