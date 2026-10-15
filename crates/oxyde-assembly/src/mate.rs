use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::CurveKind;
use oxyde_model::{Body, EdgeName, FaceName, StandardAxis, StandardPlane};

use crate::{ComponentId, MateId, Placement};

/// How two pieces of geometry are held together.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MateKind {
    /// Faces or planes touching, a point on a face, an edge along another…
    Coincident,
    /// Shared axis: holes on pins, shafts in bores.
    Concentric,
    Parallel,
    Perpendicular,
    /// Gap between parallel planes or faces, or between points (mm).
    Distance(f64),
    /// Angle between two planes or two axes (radians).
    Angle(f64),
}

impl MateKind {
    pub fn label(&self) -> &'static str {
        match self {
            MateKind::Coincident => "Coincident",
            MateKind::Concentric => "Concentric",
            MateKind::Parallel => "Parallel",
            MateKind::Perpendicular => "Perpendicular",
            MateKind::Distance(_) => "Distance",
            MateKind::Angle(_) => "Angle",
        }
    }

    /// Whether this kind of mate means something between these two kinds of geometry.
    pub fn applies(&self, a: PrimitiveKind, b: PrimitiveKind) -> bool {
        use PrimitiveKind::{Circle, Line, Plane, Point};
        let pair = |x: PrimitiveKind, y: PrimitiveKind| (a == x && b == y) || (a == y && b == x);
        match self {
            MateKind::Coincident => true,
            MateKind::Concentric => {
                matches!(a, Line | Circle | Point)
                    && matches!(b, Line | Circle | Point)
                    && !(a == Point && b == Point)
            }
            MateKind::Parallel | MateKind::Perpendicular => a != Point && b != Point,
            MateKind::Distance(_) => {
                pair(Plane, Plane)
                    || pair(Plane, Point)
                    || pair(Point, Point)
                    || pair(Plane, Circle)
                    || pair(Line, Line)
            }
            MateKind::Angle(_) => a != Point && b != Point,
        }
    }

    /// Whether the mate's alignment (aligned / anti-aligned) matters.
    pub fn has_alignment(&self, a: PrimitiveKind, b: PrimitiveKind) -> bool {
        use PrimitiveKind::{Circle, Plane};
        let oriented = |k: PrimitiveKind| matches!(k, Plane | Circle);
        match self {
            MateKind::Coincident
            | MateKind::Parallel
            | MateKind::Distance(_)
            | MateKind::Angle(_) => oriented(a) && oriented(b),
            MateKind::Concentric => false,
            MateKind::Perpendicular => false,
        }
    }
}

/// What a mate refers to, inside one component.
#[derive(Debug, Clone, PartialEq)]
pub enum MateTarget {
    Face(FaceName),
    Edge(EdgeName),
    Plane(StandardPlane),
    Axis(StandardAxis),
    Origin,
}

/// A mate reference: geometry of one component.
///
/// When the component is a sub-assembly, `path` leads to the part inside it that holds
/// the face or edge: a component of the sub-assembly, then a component of that one if it
/// is itself an assembly, and so on. Empty for a part, or for a sub-assembly's own planes
/// and origin.
#[derive(Debug, Clone, PartialEq)]
pub struct MateRef {
    pub component: ComponentId,
    pub path: Vec<ComponentId>,
    pub target: MateTarget,
}

impl MateRef {
    /// Geometry of the component itself.
    pub fn new(component: ComponentId, target: MateTarget) -> Self {
        Self {
            component,
            path: Vec::new(),
            target,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mate {
    pub id: MateId,
    /// Unique in the assembly: `Coincident1`, `Concentric2`…
    pub name: String,
    pub kind: MateKind,
    pub a: MateRef,
    pub b: MateRef,
    /// Normals (or axes) pointing the same way rather than facing each other.
    pub aligned: bool,
    pub suppressed: bool,
}

/// Geometry a mate works on, in a component's own coordinates or in the assembly's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Primitive {
    /// A flat face or a plane, `normal` unit length (outward for a face).
    Plane {
        origin: Point3,
        normal: Vec3,
    },
    /// A straight edge, an axis, or the axis of a round face.
    Line {
        origin: Point3,
        direction: Vec3,
    },
    /// A circular edge: its centre and the normal of its plane.
    Circle {
        center: Point3,
        normal: Vec3,
    },
    Point(Point3),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveKind {
    Plane,
    Line,
    Circle,
    Point,
}

impl Primitive {
    pub fn kind(&self) -> PrimitiveKind {
        match self {
            Primitive::Plane { .. } => PrimitiveKind::Plane,
            Primitive::Line { .. } => PrimitiveKind::Line,
            Primitive::Circle { .. } => PrimitiveKind::Circle,
            Primitive::Point(_) => PrimitiveKind::Point,
        }
    }

    pub fn placed(&self, placement: &Placement) -> Primitive {
        let p = |q: Point3| placement.apply_point(q);
        let v = |w: Vec3| placement.apply_vector(w);
        match *self {
            Primitive::Plane { origin, normal } => Primitive::Plane {
                origin: p(origin),
                normal: v(normal),
            },
            Primitive::Line { origin, direction } => Primitive::Line {
                origin: p(origin),
                direction: v(direction),
            },
            Primitive::Circle { center, normal } => Primitive::Circle {
                center: p(center),
                normal: v(normal),
            },
            Primitive::Point(q) => Primitive::Point(p(q)),
        }
    }

    /// The point that stands for it: the plane's origin, the circle's centre…
    fn anchor(&self) -> Point3 {
        match *self {
            Primitive::Plane { origin, .. } | Primitive::Line { origin, .. } => origin,
            Primitive::Circle { center, .. } => center,
            Primitive::Point(p) => p,
        }
    }

    /// Normal of a plane or circle, direction of a line.
    fn direction(&self) -> Option<Vec3> {
        match *self {
            Primitive::Plane { normal, .. } | Primitive::Circle { normal, .. } => Some(normal),
            Primitive::Line { direction, .. } => Some(direction),
            Primitive::Point(_) => None,
        }
    }

    /// The axis it turns about, for concentric mates.
    fn axis(&self) -> Option<(Point3, Vec3)> {
        match *self {
            Primitive::Line { origin, direction } => Some((origin, direction)),
            Primitive::Circle { center, normal } => Some((center, normal)),
            _ => None,
        }
    }
}

/// The geometry `target` stands for in a part, in the part's own coordinates.
pub fn resolve(target: &MateTarget, body: Option<&Body>) -> Result<Primitive, String> {
    match target {
        MateTarget::Plane(p) => {
            let plane = p.plane();
            Ok(Primitive::Plane {
                origin: plane.origin,
                normal: plane.normal,
            })
        }
        MateTarget::Axis(a) => {
            let axis = a.axis();
            Ok(Primitive::Line {
                origin: axis.origin,
                direction: axis.direction,
            })
        }
        MateTarget::Origin => Ok(Primitive::Point(Point3::ORIGIN)),
        MateTarget::Face(name) => {
            let body = body.ok_or("the part has no geometry")?;
            let index = body
                .naming
                .find_face(name)
                .ok_or_else(|| format!("face {name} no longer exists"))?;
            let face = &body.topology.faces[index as usize];
            if let Some(normal) = face.normal {
                Ok(Primitive::Plane {
                    origin: face.centroid,
                    normal,
                })
            } else if let Some(axis) = face.axis {
                Ok(Primitive::Line {
                    origin: axis.origin,
                    direction: axis.direction,
                })
            } else {
                Err("only flat, cylindrical and conical faces can be mated".to_owned())
            }
        }
        MateTarget::Edge(name) => {
            let body = body.ok_or("the part has no geometry")?;
            let index = body
                .naming
                .find_edge(name)
                .ok_or_else(|| format!("edge {name} no longer exists"))?;
            let edge = &body.topology.edges[index as usize];
            match (edge.curve, edge.axis) {
                (CurveKind::Line, Some(axis)) => Ok(Primitive::Line {
                    origin: axis.origin,
                    direction: axis.direction,
                }),
                (CurveKind::Circle, Some(axis)) => Ok(Primitive::Circle {
                    center: axis.origin,
                    normal: axis.direction,
                }),
                _ => Err("only straight and circular edges can be mated".to_owned()),
            }
        }
    }
}

/// Whether `aligned` makes the mate closest to how the geometry sits now: what a new
/// mate starts with.
pub(crate) fn closest_alignment(a: &Primitive, b: &Primitive) -> bool {
    match (a.direction(), b.direction()) {
        (Some(u), Some(v)) => u.dot(v) >= 0.0,
        _ => true,
    }
}

/// Signed distance from `p` to the plane through `origin` with unit `normal`.
fn height(p: Point3, origin: Point3, normal: Vec3) -> f64 {
    (p - origin).dot(normal)
}

/// Part of `p − origin` across the line.
fn off_line(p: Point3, origin: Point3, direction: Vec3) -> Vec3 {
    let d = p - origin;
    d - direction * d.dot(direction)
}

fn push3(out: &mut Vec<f64>, v: Vec3, scale: f64) {
    out.extend([v.x * scale, v.y * scale, v.z * scale]);
}

/// Equations that are all zero when the mate holds, given both sides in assembly
/// coordinates. Directions are scaled by `size` so that they weigh like lengths.
pub(crate) fn residuals(
    kind: MateKind,
    aligned: bool,
    a: &Primitive,
    b: &Primitive,
    size: f64,
    out: &mut Vec<f64>,
) -> Result<(), String> {
    use Primitive as P;
    let sign = if aligned { 1.0 } else { -1.0 };
    let invalid = || {
        Err(format!(
            "a {} mate does not apply to {} and {}",
            kind.label().to_lowercase(),
            describe(a),
            describe(b)
        ))
    };
    if !kind.applies(a.kind(), b.kind()) {
        return invalid();
    }
    // Same direction, or opposite.
    let same = |u: Vec3, v: Vec3, out: &mut Vec<f64>| push3(out, v - u * sign, size);
    // Parallel either way.
    let parallel = |u: Vec3, v: Vec3, out: &mut Vec<f64>| push3(out, u.cross(v), size);
    match kind {
        MateKind::Coincident => match (*a, *b) {
            (
                P::Plane { origin, normal },
                P::Plane {
                    origin: o2,
                    normal: n2,
                },
            ) => {
                same(normal, n2, out);
                out.push(height(o2, origin, normal));
            }
            (P::Plane { origin, normal }, P::Point(p))
            | (P::Point(p), P::Plane { origin, normal }) => {
                out.push(height(p, origin, normal));
            }
            (
                P::Plane { origin, normal },
                P::Line {
                    origin: lo,
                    direction,
                },
            )
            | (
                P::Line {
                    origin: lo,
                    direction,
                },
                P::Plane { origin, normal },
            ) => {
                out.push(normal.dot(direction) * size);
                out.push(height(lo, origin, normal));
            }
            (P::Plane { origin, normal }, P::Circle { center, normal: cn })
            | (P::Circle { center, normal: cn }, P::Plane { origin, normal }) => {
                // The circle lies in the plane.
                if matches!(a, P::Plane { .. }) {
                    same(normal, cn, out);
                } else {
                    same(cn, normal, out);
                }
                out.push(height(center, origin, normal));
            }
            (P::Point(p), P::Point(q)) => push3(out, q - p, 1.0),
            (P::Line { origin, direction }, P::Point(p))
            | (P::Point(p), P::Line { origin, direction }) => {
                push3(out, off_line(p, origin, direction), 1.0)
            }
            (P::Circle { center, .. }, P::Point(p)) | (P::Point(p), P::Circle { center, .. }) => {
                push3(out, p - center, 1.0)
            }
            (
                P::Line { origin, direction },
                P::Line {
                    origin: o2,
                    direction: d2,
                },
            ) => {
                parallel(direction, d2, out);
                push3(out, off_line(o2, origin, direction), 1.0);
            }
            (
                P::Circle { center, normal },
                P::Circle {
                    center: c2,
                    normal: n2,
                },
            ) => {
                same(normal, n2, out);
                push3(out, c2 - center, 1.0);
            }
            (P::Line { origin, direction }, P::Circle { center, normal })
            | (P::Circle { center, normal }, P::Line { origin, direction }) => {
                // The edge through the circle's centre, along its axis.
                parallel(direction, normal, out);
                push3(out, off_line(center, origin, direction), 1.0);
            }
        },
        MateKind::Concentric => {
            let axis = |p: &Primitive| p.axis();
            match (axis(a), axis(b)) {
                (Some((o, d)), Some((o2, d2))) => {
                    parallel(d, d2, out);
                    push3(out, off_line(o2, o, d), 1.0);
                }
                (Some((o, d)), None) => push3(out, off_line(b.anchor(), o, d), 1.0),
                (None, Some((o, d))) => push3(out, off_line(a.anchor(), o, d), 1.0),
                (None, None) => return invalid(),
            }
        }
        MateKind::Parallel | MateKind::Perpendicular => {
            let (u, v) = (a.direction().unwrap(), b.direction().unwrap());
            let has = |k: PrimitiveKind| a.kind() == k || b.kind() == k;
            // A line and a plane compare the line with the plane's normal: the other way
            // round.
            let crossed = has(PrimitiveKind::Plane) && has(PrimitiveKind::Line);
            match (kind == MateKind::Parallel) != crossed {
                true if kind.has_alignment(a.kind(), b.kind()) => same(u, v, out),
                true => parallel(u, v, out),
                false => out.push(u.dot(v) * size),
            }
        }
        MateKind::Distance(d) => match (*a, *b) {
            (
                P::Plane { origin, normal },
                P::Plane {
                    origin: o2,
                    normal: n2,
                },
            ) => {
                same(normal, n2, out);
                out.push(height(o2, origin, normal).abs() - d);
            }
            (P::Plane { origin, normal }, other) | (other, P::Plane { origin, normal }) => {
                if let (P::Circle { normal: cn, .. }, true) = (other, matches!(a, P::Plane { .. }))
                {
                    same(normal, cn, out);
                } else if let P::Circle { normal: cn, .. } = other {
                    same(cn, normal, out);
                }
                out.push(height(other.anchor(), origin, normal).abs() - d);
            }
            (
                P::Line { origin, direction },
                P::Line {
                    origin: o2,
                    direction: d2,
                },
            ) => {
                parallel(direction, d2, out);
                out.push(off_line(o2, origin, direction).length() - d);
            }
            _ => out.push((b.anchor() - a.anchor()).length() - d),
        },
        MateKind::Angle(angle) => {
            let (u, v) = (a.direction().unwrap(), b.direction().unwrap());
            let s = if kind.has_alignment(a.kind(), b.kind()) {
                sign
            } else {
                1.0
            };
            out.push((u.dot(v) * s - angle.cos()) * size);
        }
    }
    Ok(())
}

fn describe(p: &Primitive) -> &'static str {
    match p {
        Primitive::Plane { .. } => "a plane",
        Primitive::Line { .. } => "a line",
        Primitive::Circle { .. } => "a circle",
        Primitive::Point(_) => "a point",
    }
}
