//! Sweeps: the regions of a profile carried along a path of lines and arcs.
//!
//! The profile keeps its angle to the path as it goes (OpenCASCADE's corrected Frenet
//! frame), which along a path in one plane is the path's own turn: along a line the profile
//! is carried straight, along an arc turned about the arc's axis. So each segment of the
//! path sweeps an extrusion or a revolve of the profile where that segment starts, and the
//! pieces are glued where one ends and the next begins, the profile's faces there dropped
//! and their edges shared, each piece's faces as they are.
//! Each segment of the profile makes one face per segment of the path
//! ([`FaceSource::ProfileSegment`]), as OpenCASCADE's do: along a line a plane or a cylinder
//! whose axis runs the way the path does, from the profile's plane, along an arc what a
//! revolve makes, its tori about the axis the path turns about, the way it turns; an open path has the profile at its start ([`FaceSource::StartCap`]) and
//! at its end ([`FaceSource::EndCap`]), a closed one makes a ring.
//!
//! Not taken: a path with a sharp corner (OpenCASCADE's frame jumps there and its sweep
//! after the corner has no volume), and a profile an arc of the path does not turn about an
//! axis in its plane (not square to the path where the arc starts, or not where the path
//! does).

// Indices into the solids glued here and their faces' own numbering.
#![allow(clippy::indexing_slicing)]

use std::f64::consts::TAU;

use oxyde_geom::{Axis, Curve2, LINEAR_TOLERANCE, Path, Plane, Point3, Profile, Transform, Vec3};
use oxyde_kernel::{FaceSource, OpHistory};

use crate::NativeError;
use crate::brep::{Edge, EdgeUse, Face, Shape, Solid, Vertex};
use crate::build;
use crate::geometry::{Curve, Surface};
use crate::revolution::{Meridian, Revolution};
use crate::revolve;

/// A segment of the path in space, the way the path runs.
#[derive(Debug, Clone, Copy)]
enum Leg {
    Line {
        from: Point3,
        to: Point3,
    },
    /// Round `axis` (counter-clockwise about its direction) by `angle`, from `from`.
    Arc {
        axis: Axis,
        angle: f64,
        from: Point3,
    },
}

impl Leg {
    /// The path's direction where the leg starts and where it ends.
    fn tangents(&self) -> Option<(Vec3, Vec3)> {
        match *self {
            Leg::Line { from, to } => {
                let d = (to - from).normalized()?;
                Some((d, d))
            }
            Leg::Arc { axis, angle, from } => {
                let a = axis.direction;
                let start = a.cross(from - axis.origin).normalized()?;
                let turn = Transform::rotation(&axis, angle);
                Some((start, turn.apply_vector(start)))
            }
        }
    }

    /// The motion that carries the profile from where the leg starts to where it ends.
    fn motion(&self) -> Transform {
        match *self {
            Leg::Line { from, to } => Transform::translation(to - from),
            Leg::Arc { axis, angle, .. } => Transform::rotation(&axis, angle),
        }
    }
}

/// The path's segments in space.
fn legs(path_plane: &Plane, path: &Path) -> Result<Vec<Leg>, NativeError> {
    let failed = |m: &str| NativeError::Failed(m.to_owned());
    let normal = path_plane
        .normal
        .normalized()
        .ok_or_else(|| failed("the path's plane has no normal"))?;
    let at = |p: oxyde_geom::Point2| path_plane.to_world(p);
    path.segments
        .iter()
        .map(|segment| match segment.curve {
            Curve2::Line { start, end } => Ok(Leg::Line {
                from: at(start),
                to: at(end),
            }),
            Curve2::Arc { start, mid, end } => {
                let center = build::circumcentre(start, mid, end)
                    .ok_or_else(|| failed("an arc of the path is a straight line"))?;
                let ccw =
                    (mid.x - start.x) * (end.y - mid.y) - (mid.y - start.y) * (end.x - mid.x) > 0.0;
                let angle = build::turn(center, start, end, ccw);
                Ok(Leg::Arc {
                    axis: Axis {
                        origin: at(center),
                        direction: if ccw { normal } else { -normal },
                    },
                    angle,
                    from: at(start),
                })
            }
            // A whole circle starts at angle zero in the plane's coordinates.
            Curve2::Circle { center, radius } => Ok(Leg::Arc {
                axis: Axis {
                    origin: at(center),
                    direction: normal,
                },
                angle: TAU,
                from: at(oxyde_geom::Point2::new(center.x + radius, center.y)),
            }),
        })
        .collect()
}

/// The regions of `profile`, drawn on `plane`, swept along `path`, drawn on `path_plane`.
pub(crate) fn sweep(
    plane: &Plane,
    profile: &Profile,
    path_plane: &Plane,
    path: &Path,
) -> Result<(Shape, OpHistory), NativeError> {
    let legs = legs(path_plane, path)?;
    if legs.is_empty() {
        return Err(NativeError::Failed("the path has no curve".to_owned()));
    }
    // The path must turn smoothly where its segments meet.
    let pairs = if path.closed {
        legs.len()
    } else {
        legs.len() - 1
    };
    for i in 0..pairs {
        let (Some((_, out)), Some((into, _))) =
            (legs[i].tangents(), legs[(i + 1) % legs.len()].tangents())
        else {
            return Err(NativeError::Failed(
                "a segment of the path has no length".to_owned(),
            ));
        };
        if legs.len() > 1 && (out - into).length() > 1e-6 {
            return Err(NativeError::Failed(
                "the path turns sharply where two of its curves meet: a sweep needs a path \
                 that turns smoothly"
                    .to_owned(),
            ));
        }
    }
    let normal = plane
        .normal
        .normalized()
        .ok_or_else(|| NativeError::Failed("the profile's plane has no normal".to_owned()))?;
    let mut motion = Transform::IDENTITY;
    let mut pieces: Vec<(Shape, OpHistory)> = Vec::with_capacity(legs.len());
    for leg in &legs {
        let here = Plane {
            origin: motion.apply_point(plane.origin),
            x_dir: motion.apply_vector(plane.x_dir),
            normal: motion.apply_vector(normal),
        };
        let piece = match *leg {
            Leg::Line { from, to } => {
                let vector = to - from;
                let (mut shape, history) = build::extrude(&here, profile, vector)?;
                // Along the path, from the profile's plane: an extrusion's cylinder runs the
                // other way.
                let along = vector.normalized().unwrap_or(here.normal);
                for solid in &mut shape.solids {
                    for face in &mut solid.faces {
                        if let Surface::Revolution(r) = &mut face.surface
                            && r.axis.direction.dot(along) < 0.0
                        {
                            *r = r.reframed(&Revolution {
                                axis: Axis {
                                    origin: r.axis.origin,
                                    direction: along,
                                },
                                ..*r
                            });
                        }
                    }
                }
                (shape, history)
            }
            Leg::Arc { axis, angle, .. } => {
                // The profile must lie in a plane holding the arc's axis.
                let a = axis.direction;
                if here.normal.dot(a).abs() > 1e-9
                    || (axis.origin - here.origin).dot(here.normal).abs() > LINEAR_TOLERANCE
                {
                    return Err(NativeError::Unsupported(
                        "sweep a profile along an arc it is not square to where the arc starts",
                    ));
                }
                let (mut shape, history) = revolve::revolve(&here, profile, &axis, angle)?;
                // A torus runs about the path's turn, the way the path goes round.
                for solid in &mut shape.solids {
                    for face in &mut solid.faces {
                        if let Surface::Revolution(r) = &mut face.surface
                            && matches!(r.meridian, Meridian::Circle { center, .. } if center > 0.0)
                            && r.axis.direction.dot(a) < 0.0
                        {
                            *r = r.reframed(&Revolution {
                                axis: Axis {
                                    origin: r.axis.origin,
                                    direction: a,
                                },
                                ..*r
                            });
                        }
                    }
                }
                (shape, history)
            }
        };
        pieces.push(piece);
        motion = motion.then(&leg.motion());
    }
    let last = pieces.len() - 1;
    // What each piece's faces are named: their profile segments; the start cap the first
    // piece's and the end cap the last's, on an open path.
    let named = |k: usize, sources: &[FaceSource]| -> Vec<FaceSource> {
        sources
            .iter()
            .copied()
            .filter(|s| match s {
                FaceSource::StartCap => k == 0 && !path.closed,
                FaceSource::EndCap => k == last && !path.closed,
                _ => true,
            })
            .collect()
    };
    if pieces.len() == 1 {
        let (shape, history) = pieces.swap_remove(0);
        let history = OpHistory {
            faces: history.faces.iter().map(|s| named(0, s)).collect(),
        };
        return Ok((shape, history));
    }
    // Glue the pieces where one ends and the next begins, region by region: the caps
    // there dropped, their edges and corners shared.
    let regions = pieces[0].0.solids.len();
    if pieces
        .iter()
        .any(|(shape, _)| shape.solids.len() != regions)
    {
        return Err(NativeError::Internal(
            "the pieces of a sweep have different regions".to_owned(),
        ));
    }
    let mut out = Shape::default();
    let mut history = OpHistory::default();
    for r in 0..regions {
        let mut parts = Vec::with_capacity(pieces.len());
        for (k, (shape, own)) in pieces.iter().enumerate() {
            let before: usize = shape.solids[..r].iter().map(|s| s.faces.len()).sum();
            let solid = &shape.solids[r];
            let sources = &own.faces[before..before + solid.faces.len()];
            let drop_start = k > 0 || path.closed;
            let drop_end = k < last || path.closed;
            let keep: Vec<bool> = sources
                .iter()
                .map(|s| {
                    !(drop_start && s.contains(&FaceSource::StartCap)
                        || drop_end && s.contains(&FaceSource::EndCap))
                })
                .collect();
            let names: Vec<Vec<FaceSource>> = sources.iter().map(|s| named(k, s)).collect();
            parts.push((solid, keep, names));
        }
        let (solid, names) = glue(&parts)?;
        out.solids.push(solid);
        history.faces.extend(names);
    }
    Ok((out, history))
}

/// Points this close are one where pieces are glued.
const WELD: f64 = 10.0 * LINEAR_TOLERANCE;

/// A solid to glue: which of its faces to keep, and what each is named.
type Glued<'a> = (&'a Solid, Vec<bool>, Vec<Vec<FaceSource>>);

/// Solids that meet face to face glued into one: the faces of each that `keep` says,
/// their corners welded and the edges they share taken once.
fn glue(parts: &[Glued<'_>]) -> Result<(Solid, Vec<Vec<FaceSource>>), NativeError> {
    let mut points: Vec<Point3> = Vec::new();
    let mut index = |p: Point3| -> u32 {
        if let Some(i) = points.iter().position(|q| q.distance(p) <= WELD) {
            return i as u32;
        }
        points.push(p);
        points.len() as u32 - 1
    };
    let mut edges: Vec<Edge> = Vec::new();
    let mut middles: Vec<Option<Point3>> = Vec::new();
    let mut faces: Vec<Face> = Vec::new();
    let mut names: Vec<Vec<FaceSource>> = Vec::new();
    for (solid, keep, own) in parts {
        // Each of this solid's edges, as a glued one.
        let mut glued: Vec<Option<(u32, bool)>> = vec![None; solid.edges.len()];
        for (face, (&kept, name)) in solid.faces.iter().zip(keep.iter().zip(own)) {
            if !kept {
                continue;
            }
            let mut loops = Vec::with_capacity(face.loops.len());
            for uses in &face.loops {
                let mut out = Vec::with_capacity(uses.len());
                for u in uses {
                    let (e, flip) = match glued[u.edge as usize] {
                        Some(found) => found,
                        None => {
                            let edge = &solid.edges[u.edge as usize];
                            let (i, j) =
                                (index(solid.point(edge.start)), index(solid.point(edge.end)));
                            let middle =
                                (!edge.curve.is_degenerate()).then(|| solid.midpoint(edge));
                            let existing = edges.iter().enumerate().find(|(k, other)| {
                                let ends = (other.start, other.end);
                                (ends == (i, j) || ends == (j, i))
                                    && match (middle, middles[*k]) {
                                        (Some(a), Some(b)) => a.distance(b) <= 100.0 * WELD,
                                        _ => false,
                                    }
                            });
                            let found = match existing {
                                Some((k, other)) => {
                                    // Run the other way when it starts at the other end, or
                                    // for a closed one turns the other way.
                                    let flip = if other.start == other.end {
                                        !same_way(&other.curve, &edge.curve)
                                    } else {
                                        other.start != i
                                    };
                                    (k as u32, flip)
                                }
                                None => {
                                    edges.push(Edge::new(i, j, edge.curve));
                                    middles.push(middle);
                                    (edges.len() as u32 - 1, false)
                                }
                            };
                            glued[u.edge as usize] = Some(found);
                            found
                        }
                    };
                    out.push(EdgeUse {
                        edge: e,
                        reversed: u.reversed != flip,
                    });
                }
                loops.push(out);
            }
            faces.push(Face {
                surface: face.surface,
                loops,
            });
            names.push(name.clone());
        }
    }
    let mut solid = Solid::closed(points.into_iter().map(Vertex::new).collect(), edges, faces);
    solid.settle_tolerances().map_err(NativeError::Internal)?;
    Ok((solid, names))
}

/// Whether two closed curves turn the same way round.
fn same_way(a: &Curve, b: &Curve) -> bool {
    match (a, b) {
        (Curve::Circle(s), Curve::Circle(t)) => s.normal.dot(t.normal) > 0.0,
        (Curve::Section(s), Curve::Section(t)) => s.ccw == t.ccw,
        _ => true,
    }
}
