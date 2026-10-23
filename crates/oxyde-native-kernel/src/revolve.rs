//! Solids of revolution: the regions of a profile turned about an axis in its plane.
//!
//! Each segment of the profile sweeps a face: a line square to the axis a plane (a disc,
//! an annulus, a sector of one), a line along it a cylinder, a slanted one a cone, an arc
//! centred on the axis a sphere, any other arc a torus; a line on the axis sweeps nothing.
//! The topology is OpenCASCADE's (`BRepPrimAPI_MakeRevol`), so that both kernels give the
//! same names:
//!
//! - Each corner off the axis sweeps a circle, an arc for less than a whole turn, from the
//!   corner to its copy at the end angle; a corner on the axis that a cone or a sphere
//!   reaches stands for a degenerate edge there.
//! - Each segment off the axis is an edge where it is (and, for less than a whole turn, at
//!   the end angle too); for a whole turn, a plane's segment is no edge and a curved face's
//!   is its seam. A segment on the axis is an edge between the two caps.
//! - A surface's axis runs the way its parameter `v` grows along its segment: the segment's
//!   own way for a line, counter-clockwise in the meridian half-plane for an arc. Its origin
//!   is the foot on the axis of the segment's start (a line) or of its circle's centre (an
//!   arc), and its angle zero points from the axis to the profile.
//! - Faces come in the profile's order, then for less than a whole turn the start cap
//!   (the profile where it is) and the end cap.

// Indices into the vertices and edges made here, laid out by `revolve` itself.
#![allow(clippy::indexing_slicing)]

use std::f64::consts::TAU;

use oxyde_geom::{Axis, LINEAR_TOLERANCE, Plane, Point2, Point3, Profile, Transform, Vec3};
use oxyde_kernel::{FaceSource, OpHistory};

use crate::NativeError;
use crate::brep::{Edge, EdgeUse, Face, Shape, Solid, Vertex};
use crate::build::{PlanarLoop, Run, distance2, turn};
use crate::geometry::{Circle, Curve, Surface};
use crate::revolution::{Meridian, Revolution};

/// What a segment of the profile sweeps.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Sweeps {
    /// It lies on the axis: nothing.
    Nothing,
    /// A plane square to the axis.
    Plane,
    /// A cylinder, a cone, a sphere or a torus.
    Curved(Revolution),
}

/// A corner of the profile in the meridian half-plane.
#[derive(Debug, Clone, Copy)]
struct Corner {
    point: Point3,
    rho: f64,
    z: f64,
    on_axis: bool,
}

pub(crate) fn revolve(
    plane: &Plane,
    profile: &Profile,
    axis: &Axis,
    angle: f64,
) -> Result<(Shape, OpHistory), NativeError> {
    let failed = |message: &str| NativeError::Failed(message.to_owned());
    if angle.is_nan() || angle <= 1e-9 || angle > TAU + 1e-9 {
        return Err(failed(
            "the angle must be more than 0 and at most 360 degrees",
        ));
    }
    if profile.regions.is_empty() {
        return Err(failed("the profile has no region to revolve"));
    }
    let full = angle >= TAU - 1e-9;
    let turn = if full { TAU } else { angle };
    let normal = plane
        .normal
        .normalized()
        .ok_or_else(|| failed("the sketch plane has no normal"))?;
    let x_plane = plane
        .x_dir
        .normalized()
        .ok_or_else(|| failed("the sketch plane has no x direction"))?;
    let plane = Plane {
        origin: plane.origin,
        x_dir: x_plane,
        normal,
    };
    let a = axis
        .direction
        .normalized()
        .ok_or_else(|| failed("the axis has no direction"))?;
    let o = axis.origin;
    if a.dot(normal).abs() > 1e-9 || (o - plane.origin).dot(normal).abs() > LINEAR_TOLERANCE {
        return Err(failed("the axis does not lie in the sketch plane"));
    }
    let contours: Vec<Vec<PlanarLoop>> = profile
        .regions
        .iter()
        .map(|region| {
            std::iter::once(&region.outer)
                .chain(&region.holes)
                .map(PlanarLoop::of)
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<_, _>>()?;
    // Angle zero points from the axis to the profile, on whichever side it is.
    let across = normal.cross(a);
    let mut reach = (f64::INFINITY, f64::NEG_INFINITY);
    for contour in contours.iter().flatten() {
        for s in contour_reach(&plane, contour, o, across) {
            reach = (reach.0.min(s), reach.1.max(s));
        }
    }
    let x_dir = if reach.0 >= -LINEAR_TOLERANCE {
        across
    } else if reach.1 <= LINEAR_TOLERANCE {
        -across
    } else {
        return Err(failed(
            "the profile crosses the axis: it must lie on one side of it",
        ));
    };
    // The meridian half-plane's (ρ, z) turns the sketch plane's way when this is positive.
    let same_way = x_dir.cross(a).dot(normal) > 0.0;
    let frame = Frame {
        plane,
        o,
        a,
        x_dir,
        same_way,
        turn,
        full,
    };
    let mut shape = Shape::default();
    let mut history = OpHistory::default();
    for loops in &contours {
        let (solid, sources) = frame.solid(loops)?;
        shape.solids.push(solid);
        history
            .faces
            .extend(sources.into_iter().map(|source| vec![source]));
    }
    Ok((shape, history))
}

/// How far across the axis (along `across`, from `o`) a contour's corners are and its arcs
/// reach between them: enough to tell which side of the axis it is on.
fn contour_reach(plane: &Plane, contour: &PlanarLoop, o: Point3, across: Vec3) -> Vec<f64> {
    let mut out = Vec::new();
    let n = contour.corners.len();
    // `across` in the plane's coordinates: an arc reaches furthest along it at this angle,
    // and least half a turn on.
    let (ax, ay) = (across.dot(plane.x_dir), across.dot(plane.y_dir()));
    let peak = ay.atan2(ax);
    for k in 0..n {
        let (p, _, run) = contour.corners[k];
        out.push((plane.to_world(p) - o).dot(across));
        if let Run::Arc { center, ccw } = run {
            let q = contour.corners[(k + 1) % n].0;
            let r = distance2(p, center);
            let start = (p.y - center.y).atan2(p.x - center.x);
            let sweep = turn(center, p, q, ccw);
            for t in [peak, peak + std::f64::consts::PI] {
                let along = if ccw { t - start } else { start - t }.rem_euclid(TAU);
                if along < sweep {
                    let e = Point2::new(center.x + r * t.cos(), center.y + r * t.sin());
                    out.push((plane.to_world(e) - o).dot(across));
                }
            }
        }
    }
    out
}

/// Where the revolve happens: the sketch plane, the axis, angle zero and the turn.
struct Frame {
    plane: Plane,
    o: Point3,
    a: Vec3,
    x_dir: Vec3,
    same_way: bool,
    turn: f64,
    full: bool,
}

impl Frame {
    fn corner(&self, p: Point2) -> Corner {
        let point = self.plane.to_world(p);
        let d = point - self.o;
        let rho = d.dot(self.x_dir);
        Corner {
            point,
            rho,
            z: d.dot(self.a),
            on_axis: rho.abs() <= LINEAR_TOLERANCE,
        }
    }

    /// A contour's signed area in the meridian half-plane: positive counter-clockwise.
    fn area_rz(&self, contour: &PlanarLoop) -> f64 {
        let area = contour.signed_area();
        if self.same_way { area } else { -area }
    }

    /// The circle an arc of the profile runs along, in space.
    fn arc_circle(&self, a: Point2, center: Point2, ccw: bool) -> Circle {
        Circle {
            center: self.plane.to_world(center),
            normal: if ccw {
                self.plane.normal
            } else {
                -self.plane.normal
            },
            radius: distance2(a, center),
        }
    }

    /// What the segment from corner `p` to corner `q` sweeps, running as `run` from `start`
    /// (`p` in the sketch's coordinates).
    fn sweeps(
        &self,
        p: &Corner,
        q: &Corner,
        run: Run,
        start: Point2,
    ) -> Result<Sweeps, NativeError> {
        let failed = |message: &str| NativeError::Failed(message.to_owned());
        match run {
            Run::Line => {
                if p.on_axis && q.on_axis {
                    return Ok(Sweeps::Nothing);
                }
                let (dr, dz) = (q.rho - p.rho, q.z - p.z);
                let length = (dr * dr + dz * dz).sqrt();
                if dz.abs() <= 1e-12 * length.max(1.0) {
                    return Ok(Sweeps::Plane);
                }
                // The axis runs the segment's way along it, from the foot of its start.
                let up = dz > 0.0;
                let slope = dr / dz.abs();
                Ok(Sweeps::Curved(Revolution {
                    axis: Axis {
                        origin: self.o + self.a * p.z,
                        direction: if up { self.a } else { -self.a },
                    },
                    x_dir: self.x_dir,
                    meridian: Meridian::Line {
                        radius: p.rho,
                        slope: if slope.abs() <= 1e-12 { 0.0 } else { slope },
                    },
                    outward: true,
                }))
            }
            Run::Arc { center, ccw } => {
                let c = self.corner(center);
                let radius = distance2(start, center);
                if c.rho > LINEAR_TOLERANCE && radius >= c.rho - LINEAR_TOLERANCE {
                    return Err(NativeError::Unsupported(
                        "revolve an arc whose circle reaches the axis without being centred on it",
                    ));
                }
                // Counter-clockwise in the meridian half-plane, v grows along the arc.
                let ccw_rz = ccw == self.same_way;
                if radius <= LINEAR_TOLERANCE {
                    return Err(failed("an arc of the profile has no radius"));
                }
                Ok(Sweeps::Curved(Revolution {
                    axis: Axis {
                        origin: self.o + self.a * c.z,
                        direction: if ccw_rz { self.a } else { -self.a },
                    },
                    x_dir: self.x_dir,
                    meridian: Meridian::Circle {
                        center: if c.on_axis { 0.0 } else { c.rho },
                        radius,
                    },
                    outward: true,
                }))
            }
        }
    }

    /// The solid one region (its outer contour and its holes) sweeps, and the source of
    /// each of its faces.
    fn solid(&self, loops: &[PlanarLoop]) -> Result<(Solid, Vec<FaceSource>), NativeError> {
        let failed = |message: &str| NativeError::Failed(message.to_owned());
        let rotation = Transform::rotation(
            &Axis {
                origin: self.o,
                direction: self.a,
            },
            self.turn,
        );
        let mut solid = Solid::default();
        // Per contour: each corner's circle or degenerate edge; each segment's edges at
        // angle zero and at the end; what it sweeps.
        struct Built {
            corners: Vec<Corner>,
            round: Vec<Option<u32>>,
            sweeps: Vec<Sweeps>,
            at_start: Vec<Option<u32>>,
            at_end: Vec<Option<u32>>,
            interior_left: bool,
        }
        let mut built: Vec<Built> = Vec::with_capacity(loops.len());
        for (index, contour) in loops.iter().enumerate() {
            let n = contour.corners.len();
            let corners: Vec<Corner> = contour.corners.iter().map(|c| self.corner(c.0)).collect();
            let mut sweeps = Vec::with_capacity(n);
            for k in 0..n {
                let (start, _, run) = contour.corners[k];
                sweeps.push(self.sweeps(&corners[k], &corners[(k + 1) % n], run, start)?);
            }
            // A corner on the axis that a curved face reaches is a pole or an apex; one
            // between two segments off the axis would pinch the solid there.
            let mut pole = vec![false; n];
            for k in 0..n {
                if !corners[k].on_axis {
                    continue;
                }
                let before = sweeps[(k + n - 1) % n];
                let after = sweeps[k];
                if before != Sweeps::Nothing && after != Sweeps::Nothing && n > 1 {
                    return Err(NativeError::Unsupported(
                        "revolve a profile that touches the axis at a corner only",
                    ));
                }
                pole[k] = matches!(before, Sweeps::Curved(_)) || matches!(after, Sweeps::Curved(_));
            }
            let mut start_vertex = vec![None; n];
            let mut end_vertex = vec![None; n];
            let mut round = vec![None; n];
            for k in 0..n {
                let c = corners[k];
                if c.on_axis {
                    // A vertex for a pole, and between the caps for less than a whole turn.
                    if pole[k] || !self.full {
                        let v = push_vertex(&mut solid, c.point);
                        start_vertex[k] = Some(v);
                        end_vertex[k] = Some(v);
                        if pole[k] {
                            solid.edges.push(Edge::new(
                                v,
                                v,
                                Curve::Degenerate {
                                    axis: self.a,
                                    turn: self.turn,
                                },
                            ));
                            round[k] = Some(solid.edges.len() as u32 - 1);
                        }
                    }
                } else {
                    let v0 = push_vertex(&mut solid, c.point);
                    let v1 = if self.full {
                        v0
                    } else {
                        push_vertex(&mut solid, rotation.apply_point(c.point))
                    };
                    start_vertex[k] = Some(v0);
                    end_vertex[k] = Some(v1);
                    solid.edges.push(Edge::new(
                        v0,
                        v1,
                        Curve::Circle(Circle {
                            center: self.o + self.a * c.z,
                            normal: self.a,
                            radius: c.rho,
                        }),
                    ));
                    round[k] = Some(solid.edges.len() as u32 - 1);
                }
            }
            let mut at_start = vec![None; n];
            let mut at_end = vec![None; n];
            for k in 0..n {
                let next = (k + 1) % n;
                let (p2, _, run) = contour.corners[k];
                let curve = match run {
                    Run::Line => Curve::Line,
                    Run::Arc { center, ccw } => Curve::Circle(self.arc_circle(p2, center, ccw)),
                };
                let wanted_at_start = match sweeps[k] {
                    Sweeps::Nothing | Sweeps::Plane => !self.full,
                    Sweeps::Curved(_) => true,
                };
                if !wanted_at_start {
                    continue;
                }
                let (Some(s), Some(e)) = (start_vertex[k], start_vertex[next]) else {
                    return Err(NativeError::Internal(
                        "a profile edge without its corners".to_owned(),
                    ));
                };
                solid.edges.push(Edge::new(s, e, curve));
                at_start[k] = Some(solid.edges.len() as u32 - 1);
                if !self.full && sweeps[k] != Sweeps::Nothing {
                    let (Some(s), Some(e)) = (end_vertex[k], end_vertex[next]) else {
                        return Err(NativeError::Internal(
                            "a profile edge without its corners".to_owned(),
                        ));
                    };
                    solid
                        .edges
                        .push(Edge::new(s, e, curve.transformed(&rotation)));
                    at_end[k] = Some(solid.edges.len() as u32 - 1);
                } else {
                    // For a whole turn the segment ends where it starts (a seam); on the axis
                    // it is the one edge both caps share.
                    at_end[k] = at_start[k];
                }
            }
            let area = self.area_rz(contour);
            if area.abs() <= LINEAR_TOLERANCE * LINEAR_TOLERANCE {
                return Err(failed("a contour of the profile encloses no area"));
            }
            built.push(Built {
                corners,
                round,
                sweeps,
                at_start,
                at_end,
                interior_left: (index == 0) == (area > 0.0),
            });
        }

        let mut sources = Vec::new();
        let internal = |m: &str| NativeError::Internal(m.to_owned());
        for (b, contour) in built.iter().zip(loops) {
            let n = b.corners.len();
            for k in 0..n {
                let next = (k + 1) % n;
                let id = contour.corners[k].1;
                match b.sweeps[k] {
                    Sweeps::Nothing => continue,
                    Sweeps::Curved(surface) => {
                        let flipped = surface.axis.direction.dot(self.a) < 0.0;
                        let outward = b.interior_left != flipped;
                        let bottom =
                            b.round[k].ok_or_else(|| internal("a corner without its circle"))?;
                        let top =
                            b.round[next].ok_or_else(|| internal("a corner without its circle"))?;
                        let (s0, s1) = (
                            b.at_start[k].ok_or_else(|| internal("a segment without its edge"))?,
                            b.at_end[k].ok_or_else(|| internal("a segment without its edge"))?,
                        );
                        // Counter-clockwise in (θ, v): along the start corner's circle, up
                        // the segment, back along the end corner's circle, down the segment.
                        let mut uses = if flipped {
                            vec![
                                EdgeUse::backward(bottom),
                                EdgeUse::forward(s0),
                                EdgeUse::forward(top),
                                EdgeUse::backward(s1),
                            ]
                        } else {
                            vec![
                                EdgeUse::forward(bottom),
                                EdgeUse::forward(s1),
                                EdgeUse::backward(top),
                                EdgeUse::backward(s0),
                            ]
                        };
                        if !outward {
                            uses = uses.iter().rev().map(|u| u.flipped()).collect();
                        }
                        solid.faces.push(Face {
                            surface: Surface::Revolution(Revolution { outward, ..surface }),
                            loops: vec![uses],
                        });
                    }
                    Sweeps::Plane => {
                        let (p, q) = (b.corners[k], b.corners[next]);
                        let outwards = q.rho > p.rho;
                        // The normal: to the right of the segment in the meridian half-plane
                        // when the region is on its left.
                        let up_normal = if b.interior_left { !outwards } else { outwards };
                        let normal = if up_normal { self.a } else { -self.a };
                        let (inner, outer) = if outwards { (k, next) } else { (next, k) };
                        let mut loops = Vec::new();
                        if self.full {
                            // An annulus or a disc: its outer circle counter-clockwise about
                            // the normal, its inner one clockwise.
                            let ccw = |e: u32| {
                                if up_normal {
                                    EdgeUse::forward(e)
                                } else {
                                    EdgeUse::backward(e)
                                }
                            };
                            let outer_circle = b.round[outer]
                                .ok_or_else(|| internal("a corner without its circle"))?;
                            loops.push(vec![ccw(outer_circle)]);
                            if !b.corners[inner].on_axis {
                                let inner_circle = b.round[inner]
                                    .ok_or_else(|| internal("a corner without its circle"))?;
                                loops.push(vec![ccw(inner_circle).flipped()]);
                            }
                        } else {
                            // A sector, counter-clockwise about the axis: out along the
                            // segment at angle zero, round the outer arc, back along the
                            // segment at the end, round the inner arc back.
                            let (s0, s1) = (
                                b.at_start[k]
                                    .ok_or_else(|| internal("a segment without its edge"))?,
                                b.at_end[k]
                                    .ok_or_else(|| internal("a segment without its edge"))?,
                            );
                            let along = |e: u32, out: bool| {
                                if out == outwards {
                                    EdgeUse::forward(e)
                                } else {
                                    EdgeUse::backward(e)
                                }
                            };
                            let mut uses = vec![
                                along(s0, true),
                                EdgeUse::forward(
                                    b.round[outer]
                                        .ok_or_else(|| internal("a corner without its circle"))?,
                                ),
                                along(s1, false),
                            ];
                            if !b.corners[inner].on_axis {
                                uses.push(EdgeUse::backward(
                                    b.round[inner]
                                        .ok_or_else(|| internal("a corner without its circle"))?,
                                ));
                            }
                            if !up_normal {
                                uses = uses.iter().rev().map(|u| u.flipped()).collect();
                            }
                            loops.push(uses);
                        }
                        solid.faces.push(Face {
                            surface: Surface::Plane(Plane {
                                origin: self.o + self.a * p.z,
                                x_dir: self.x_dir,
                                normal,
                            }),
                            loops,
                        });
                    }
                }
                sources.push(FaceSource::ProfileSegment(id));
            }
        }
        if !self.full {
            // The caps: the profile where it is, facing against the turn, and its copy at
            // the end, facing along it.
            let start_normal = -(self.a.cross(self.x_dir));
            for (cap, normal, source) in [
                (0, start_normal, FaceSource::StartCap),
                (1, rotation.apply_vector(-start_normal), FaceSource::EndCap),
            ] {
                let mut cap_loops = Vec::new();
                for (index, (b, contour)) in built.iter().zip(loops).enumerate() {
                    // The contour's turn about this cap's normal, in space.
                    let mut area = contour.signed_area();
                    if self.plane.normal.dot(start_normal) < 0.0 {
                        area = -area;
                    }
                    if cap == 1 {
                        area = -area;
                    }
                    let counter_clockwise = area > 0.0;
                    let wanted = index == 0;
                    let edges = if cap == 0 { &b.at_start } else { &b.at_end };
                    let mut uses: Vec<EdgeUse> = edges
                        .iter()
                        .map(|e| e.map(EdgeUse::forward))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| internal("a cap without its edges"))?;
                    if counter_clockwise != wanted {
                        uses = uses.iter().rev().map(|u| u.flipped()).collect();
                    }
                    cap_loops.push(uses);
                }
                let origin = if cap == 0 {
                    self.plane.origin
                } else {
                    rotation.apply_point(self.plane.origin)
                };
                solid.faces.push(Face {
                    surface: Surface::Plane(Plane {
                        origin,
                        x_dir: if cap == 0 {
                            self.a
                        } else {
                            rotation.apply_vector(self.a)
                        },
                        normal,
                    }),
                    loops: cap_loops,
                });
                sources.push(source);
            }
        }
        solid.settle_tolerances().map_err(NativeError::Internal)?;
        Ok((solid, sources))
    }
}

fn push_vertex(solid: &mut Solid, p: Point3) -> u32 {
    solid.vertices.push(Vertex::new(p));
    solid.vertices.len() as u32 - 1
}
