//! Solids made from scratch: boxes and extrusions (prisms over a planar profile of lines
//! and arcs).

// Indices into the vertices and edges made here, laid out by `prism` itself.
#![allow(clippy::indexing_slicing)]

use std::f64::consts::TAU;

use oxyde_geom::{Axis, Curve2, LINEAR_TOLERANCE, Loop, Plane, Point2, Point3, Profile, Vec3};
use oxyde_kernel::{FaceSource, OpHistory};

use crate::NativeError;
use crate::brep::{Edge, EdgeUse, Face, Shape, Solid, Vertex, cyclic_pairs};
use crate::geometry::{Circle, Curve, Surface};
use crate::revolution::Revolution;

/// How a segment of a [`PlanarLoop`] runs from its corner to the next one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Run {
    Line,
    /// Along the circle centred on `center`, counter-clockwise in the plane's coordinates
    /// when `ccw`. From a corner to itself, a whole circle.
    Arc {
        center: Point2,
        ccw: bool,
    },
}

/// A closed contour in a plane's coordinates: each corner starts the segment with that id,
/// which runs to the next corner (the last one back to the first; a lone corner back to
/// itself, round a whole circle).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlanarLoop {
    pub corners: Vec<(Point2, u32, Run)>,
}

/// The centre of the circle through three points, `None` when they are in line.
fn circumcentre(a: Point2, b: Point2, c: Point2) -> Option<Point2> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() <= f64::EPSILON {
        return None;
    }
    let (a2, b2, c2) = (
        a.x * a.x + a.y * a.y,
        b.x * b.x + b.y * b.y,
        c.x * c.x + c.y * c.y,
    );
    Some(Point2::new(
        (a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d,
        (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d,
    ))
}

pub(crate) fn distance2(a: Point2, b: Point2) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// The angle from `a` to `b` about `center`, turning counter-clockwise when `ccw`, in
/// `(0, 2π]`: a whole turn from a point back to itself.
pub(crate) fn turn(center: Point2, a: Point2, b: Point2, ccw: bool) -> f64 {
    let angle = |p: Point2| (p.y - center.y).atan2(p.x - center.x);
    let raw = if ccw {
        angle(b) - angle(a)
    } else {
        angle(a) - angle(b)
    };
    let t = raw.rem_euclid(TAU);
    if t <= 1e-12 { TAU } else { t }
}

impl PlanarLoop {
    /// The contour of a profile loop.
    pub(crate) fn of(contour: &Loop) -> Result<PlanarLoop, NativeError> {
        let corners = contour
            .segments
            .iter()
            .map(|segment| match segment.curve {
                Curve2::Line { start, .. } => Ok((start, segment.id, Run::Line)),
                Curve2::Arc { start, mid, end } => {
                    let center = circumcentre(start, mid, end).ok_or_else(|| {
                        NativeError::Failed("an arc of the profile is a straight line".to_owned())
                    })?;
                    let ccw = (mid.x - start.x) * (end.y - mid.y)
                        - (mid.y - start.y) * (end.x - mid.x)
                        > 0.0;
                    Ok((start, segment.id, Run::Arc { center, ccw }))
                }
                // A whole circle starts at angle zero in the plane's coordinates, where
                // OpenCASCADE puts its one vertex.
                Curve2::Circle { center, radius } => Ok((
                    Point2::new(center.x + radius, center.y),
                    segment.id,
                    Run::Arc { center, ccw: true },
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let straight = corners.iter().all(|c| c.2 == Run::Line);
        if corners.is_empty() || (straight && corners.len() < 3) {
            return Err(NativeError::Failed(
                "a contour needs three corners at least".to_owned(),
            ));
        }
        Ok(PlanarLoop { corners })
    }

    /// Enclosed area, positive when the contour runs counter-clockwise: the polygon of its
    /// corners and, for each arc, the circular segment between it and its chord.
    pub(crate) fn signed_area(&self) -> f64 {
        cyclic_pairs(&self.corners)
            .map(|((a, _, run), (b, _, _))| {
                let chord = (a.x * b.y - b.x * a.y) / 2.0;
                match run {
                    Run::Line => chord,
                    Run::Arc { center, ccw } => {
                        let r = distance2(a, center);
                        let t = turn(center, a, b, ccw);
                        let segment = r * r / 2.0 * (t - t.sin());
                        chord + if ccw { segment } else { -segment }
                    }
                }
            })
            .sum::<f64>()
    }

    /// The same contour run the other way, each segment keeping its id.
    fn reversed(&self) -> PlanarLoop {
        let n = self.corners.len();
        // Segment `n - 1 - k`, reversed, runs from the corner after it (`n - k`, the first
        // corner for k = 0) to the corner it started from.
        PlanarLoop {
            corners: (0..n)
                .map(|k| {
                    let (_, id, run) = self.corners[n - 1 - k];
                    let run = match run {
                        Run::Line => Run::Line,
                        Run::Arc { center, ccw } => Run::Arc { center, ccw: !ccw },
                    };
                    (self.corners[(n - k) % n].0, id, run)
                })
                .collect(),
        }
    }
}

/// A box with one corner at `corner` and extents `size`.
pub(crate) fn make_box(corner: Point3, size: Vec3) -> Result<Shape, NativeError> {
    let positive = |v: f64| v.is_finite() && v > LINEAR_TOLERANCE;
    if !(positive(size.x) && positive(size.y) && positive(size.z)) {
        return Err(NativeError::Failed(
            "a box needs a positive size along every axis".to_owned(),
        ));
    }
    let base = Plane {
        origin: corner,
        x_dir: Vec3::X,
        normal: Vec3::Z,
    };
    let rectangle = PlanarLoop {
        corners: vec![
            (Point2::new(0.0, 0.0), 0, Run::Line),
            (Point2::new(size.x, 0.0), 1, Run::Line),
            (Point2::new(size.x, size.y), 2, Run::Line),
            (Point2::new(0.0, size.y), 3, Run::Line),
        ],
    };
    let (solid, _) = prism(&base, &rectangle, &[], Vec3::new(0.0, 0.0, size.z))?;
    Ok(Shape {
        solids: vec![solid],
    })
}

/// Each region of `profile`, drawn on `plane`, swept along `vector`: one solid per region,
/// the faces of each in the order of [`prism`].
pub(crate) fn extrude(
    plane: &Plane,
    profile: &Profile,
    vector: Vec3,
) -> Result<(Shape, OpHistory), NativeError> {
    if profile.regions.is_empty() {
        return Err(NativeError::Failed(
            "the profile has no region to extrude".to_owned(),
        ));
    }
    let mut shape = Shape::default();
    let mut history = OpHistory::default();
    for region in &profile.regions {
        let outer = PlanarLoop::of(&region.outer)?;
        let holes = region
            .holes
            .iter()
            .map(PlanarLoop::of)
            .collect::<Result<Vec<_>, _>>()?;
        let (solid, sources) = prism(plane, &outer, &holes, vector)?;
        shape.solids.push(solid);
        history
            .faces
            .extend(sources.into_iter().map(|source| vec![source]));
    }
    Ok((shape, history))
}

/// The prism swept by a face (an outer contour and its holes, in `plane`'s coordinates)
/// along `vector`, and the source of each of its faces.
///
/// Faces: the side faces of the outer contour, then those of each hole, in contour order
/// ([`FaceSource::ProfileSegment`]): a plane along a line, a cylinder along an arc; the face
/// on the sketch plane ([`FaceSource::StartCap`]); the face at the far end
/// ([`FaceSource::EndCap`]). The solid lies on the side of the plane `vector` points to,
/// whichever way that is, so it is never inside out.
fn prism(
    plane: &Plane,
    outer: &PlanarLoop,
    holes: &[PlanarLoop],
    vector: Vec3,
) -> Result<(Solid, Vec<FaceSource>), NativeError> {
    let failed = |message: &str| NativeError::Failed(message.to_owned());
    let normal = plane
        .normal
        .normalized()
        .ok_or_else(|| failed("the sketch plane has no normal"))?;
    let x_dir = plane
        .x_dir
        .normalized()
        .ok_or_else(|| failed("the sketch plane has no x direction"))?;
    let plane = Plane {
        origin: plane.origin,
        x_dir,
        normal,
    };
    let along = vector.dot(normal);
    if !along.is_finite() || along.abs() <= LINEAR_TOLERANCE {
        return Err(failed(
            "the extrusion has no length across its sketch plane",
        ));
    }
    let curved = std::iter::once(outer)
        .chain(holes)
        .flat_map(|c| &c.corners)
        .any(|c| c.2 != Run::Line);
    let sweep_dir = vector.normalized().unwrap_or(normal);
    if curved && sweep_dir.cross(normal).length() > 1e-9 {
        return Err(NativeError::Unsupported(
            "extrude arcs along a direction slanted to their sketch plane",
        ));
    }
    // `up` is the side the solid is on. Contours are turned to run counter-clockwise about
    // it (holes clockwise), which is counter-clockwise in the plane's coordinates when `up`
    // is the plane's normal and clockwise when it is the other way.
    let up = if along > 0.0 { normal } else { -normal };
    let about_up = |contour: &PlanarLoop| contour.signed_area() * along.signum();
    let mut contours = Vec::with_capacity(1 + holes.len());
    for (i, contour) in std::iter::once(outer).chain(holes).enumerate() {
        let turned = about_up(contour);
        if turned.abs() <= LINEAR_TOLERANCE * LINEAR_TOLERANCE {
            return Err(failed("a contour of the profile encloses no area"));
        }
        let counter_clockwise = i == 0;
        contours.push(if (turned > 0.0) == counter_clockwise {
            contour.clone()
        } else {
            contour.reversed()
        });
    }

    // An arc's circle in space: counter-clockwise in the plane's coordinates is about its
    // normal.
    let circle_of = |a: Point2, center: Point2, ccw: bool, offset: Vec3| Circle {
        center: plane.to_world(center) + offset,
        normal: if ccw { normal } else { -normal },
        radius: distance2(a, center),
    };

    // Per contour, corner k has its vertex on the sketch plane at `2k` and the one at the
    // far end at `2k + 1`; segment k has its edge on the sketch plane at `3k`, its edge at
    // the far end at `3k + 1`, and the edge from corner k across at `3k + 2`. Each contour's
    // numbers start after the previous contour's.
    let mut solid = Solid::default();
    let mut starts = Vec::with_capacity(contours.len());
    for contour in &contours {
        let (v0, e0) = (solid.vertices.len() as u32, solid.edges.len() as u32);
        starts.push((v0, e0));
        let n = contour.corners.len() as u32;
        for &(p, _, _) in &contour.corners {
            let bottom = plane.to_world(p);
            solid.vertices.push(Vertex::new(bottom));
            solid.vertices.push(Vertex::new(bottom + vector));
        }
        for (k, &(p, _, run)) in (0..n).zip(&contour.corners) {
            let next = (k + 1) % n;
            let (b, t) = (v0 + 2 * k, v0 + 2 * k + 1);
            let (b_next, t_next) = (v0 + 2 * next, v0 + 2 * next + 1);
            let (bottom, top) = match run {
                Run::Line => (Curve::Line, Curve::Line),
                Run::Arc { center, ccw } => (
                    Curve::Circle(circle_of(p, center, ccw, Vec3::ZERO)),
                    Curve::Circle(circle_of(p, center, ccw, vector)),
                ),
            };
            solid.edges.push(Edge::new(b, b_next, bottom));
            solid.edges.push(Edge::new(t, t_next, top));
            solid.edges.push(Edge::line(b, t));
        }
    }

    let mut sources = Vec::new();
    for (contour, &(v0, e0)) in contours.iter().zip(&starts) {
        let n = contour.corners.len() as u32;
        for (k, &(p, id, run)) in (0..n).zip(&contour.corners) {
            let next = (k + 1) % n;
            let (bottom, top) = (e0 + 3 * k, e0 + 3 * k + 1);
            let (across, across_next) = (e0 + 3 * k + 2, e0 + 3 * next + 2);
            let a = solid.point(v0 + 2 * k);
            let b = solid.point(v0 + 2 * next);
            let surface = match run {
                Run::Line => {
                    let along_edge = (b - a)
                        .normalized()
                        .ok_or_else(|| failed("a segment of the profile has no length"))?;
                    // Seen from outside, the face runs along the segment, up across the next
                    // corner, back along the far edge and down across this corner:
                    // counter-clockwise about the segment's direction crossed with the sweep.
                    let outward = along_edge.cross(vector).normalized().ok_or_else(|| {
                        failed("a segment of the profile runs along the extrusion")
                    })?;
                    Surface::Plane(Plane {
                        origin: a,
                        x_dir: along_edge,
                        normal: outward,
                    })
                }
                Run::Arc { center, ccw } => {
                    // The contour runs counter-clockwise about `up` round the material (holes
                    // the other way): an arc turning that way bulges out of it, its normal
                    // pointing away from its axis. The axis points against the sweep, through
                    // the centre on the sketch plane, as OpenCASCADE reports it.
                    let ccw_about_up = ccw == (up.dot(normal) > 0.0);
                    let radius = distance2(p, center);
                    if radius <= LINEAR_TOLERANCE {
                        return Err(failed("an arc of the profile has no radius"));
                    }
                    Surface::Revolution(Revolution::cylinder(
                        Axis {
                            origin: plane.to_world(center),
                            direction: -sweep_dir,
                        },
                        plane.x_dir,
                        radius,
                        ccw_about_up,
                    ))
                }
            };
            solid.faces.push(Face {
                surface,
                loops: vec![vec![
                    EdgeUse::forward(bottom),
                    EdgeUse::forward(across_next),
                    EdgeUse::backward(top),
                    EdgeUse::backward(across),
                ]],
            });
            sources.push(FaceSource::ProfileSegment(id));
        }
    }

    // The face on the sketch plane looks away from `up`: its contours run the other way.
    solid.faces.push(Face {
        surface: Surface::Plane(Plane {
            origin: plane.origin,
            x_dir: plane.x_dir,
            normal: -up,
        }),
        loops: contours
            .iter()
            .zip(&starts)
            .map(|(contour, &(_, e0))| {
                (0..contour.corners.len() as u32)
                    .rev()
                    .map(|k| EdgeUse::backward(e0 + 3 * k))
                    .collect()
            })
            .collect(),
    });
    sources.push(FaceSource::StartCap);
    solid.faces.push(Face {
        surface: Surface::Plane(Plane {
            origin: plane.origin + vector,
            x_dir: plane.x_dir,
            normal: up,
        }),
        loops: contours
            .iter()
            .zip(&starts)
            .map(|(contour, &(_, e0))| {
                (0..contour.corners.len() as u32)
                    .map(|k| EdgeUse::forward(e0 + 3 * k + 1))
                    .collect()
            })
            .collect(),
    });
    sources.push(FaceSource::EndCap);
    Ok((solid, sources))
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    fn rectangle(w: f64, h: f64) -> PlanarLoop {
        PlanarLoop {
            corners: vec![
                (Point2::new(0.0, 0.0), 1, Run::Line),
                (Point2::new(w, 0.0), 2, Run::Line),
                (Point2::new(w, h), 3, Run::Line),
                (Point2::new(0.0, h), 4, Run::Line),
            ],
        }
    }

    fn circle(cx: f64, cy: f64, r: f64, id: u32) -> PlanarLoop {
        PlanarLoop {
            corners: vec![(
                Point2::new(cx + r, cy),
                id,
                Run::Arc {
                    center: Point2::new(cx, cy),
                    ccw: true,
                },
            )],
        }
    }

    #[test]
    fn reversing_a_contour_keeps_each_segment_between_its_corners() {
        let r = rectangle(2.0, 1.0).reversed();
        // Segment 4 ran from (0, 1) to (0, 0): reversed, it starts at (0, 0).
        assert_eq!(
            r.corners,
            vec![
                (Point2::new(0.0, 0.0), 4, Run::Line),
                (Point2::new(0.0, 1.0), 3, Run::Line),
                (Point2::new(2.0, 1.0), 2, Run::Line),
                (Point2::new(2.0, 0.0), 1, Run::Line),
            ]
        );
        assert!(r.signed_area() < 0.0);
        assert_eq!(r.reversed(), rectangle(2.0, 1.0));
    }

    #[test]
    fn a_prism_is_closed_whichever_way_it_goes() {
        let hole = PlanarLoop {
            corners: vec![
                (Point2::new(1.0, 1.0), 5, Run::Line),
                (Point2::new(1.0, 2.0), 6, Run::Line),
                (Point2::new(3.0, 2.0), 7, Run::Line),
                (Point2::new(3.0, 1.0), 8, Run::Line),
            ],
        };
        for (outer, depth) in [
            (rectangle(4.0, 3.0), 2.0),
            (rectangle(4.0, 3.0), -2.0),
            (rectangle(4.0, 3.0).reversed(), 2.0),
        ] {
            let (solid, sources) = prism(
                &Plane::TOP,
                &outer,
                std::slice::from_ref(&hole),
                Plane::TOP.normal * depth,
            )
            .unwrap();
            solid.check().unwrap();
            assert_eq!(solid.faces.len(), 10);
            assert_eq!(sources.len(), 10);
            let shape = Shape {
                solids: vec![solid],
            };
            assert!((shape.volume().unwrap() - (12.0 - 2.0) * 2.0).abs() < 1e-9);
        }
    }

    #[test]
    fn a_cylinder_and_a_plate_with_a_round_hole() {
        for depth in [3.0, -3.0] {
            let (solid, _) = prism(
                &Plane::FRONT,
                &circle(1.0, 2.0, 2.0, 7),
                &[],
                Plane::FRONT.normal * depth,
            )
            .unwrap();
            solid.check().unwrap();
            // A side, two caps; the seam and two circles; one vertex at each end.
            assert_eq!(
                (solid.faces.len(), solid.edges.len(), solid.vertices.len()),
                (3, 3, 2)
            );
            let shape = Shape {
                solids: vec![solid],
            };
            assert!((shape.volume().unwrap() - 4.0 * PI * 3.0).abs() < 1e-9);
            let c = shape.centre_of_mass().unwrap().unwrap();
            assert!(c.distance(Point3::new(1.0, 2.0, depth / 2.0)) < 1e-9);
            let (lo, hi) = shape.bounds().unwrap();
            assert!(lo.distance(Point3::new(-1.0, 0.0, depth.min(0.0))) < 1e-9);
            assert!(hi.distance(Point3::new(3.0, 4.0, depth.max(0.0))) < 1e-9);
        }
        let (solid, _) = prism(
            &Plane::TOP,
            &rectangle(10.0, 8.0),
            &[circle(5.0, 4.0, 2.0, 9)],
            Plane::TOP.normal * 2.0,
        )
        .unwrap();
        solid.check().unwrap();
        let shape = Shape {
            solids: vec![solid],
        };
        assert!((shape.volume().unwrap() - (80.0 - 4.0 * PI) * 2.0).abs() < 1e-9);
        let topology = shape.topology().unwrap();
        let hole = topology.faces.iter().find(|f| f.normal.is_none()).unwrap();
        assert!((hole.area - 2.0 * PI * 2.0 * 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_slot_has_two_half_cylinders() {
        // Two lines and two half circles, run clockwise to check the turning round.
        let slot = PlanarLoop {
            corners: vec![
                (Point2::new(0.0, 0.0), 1, Run::Line),
                (
                    Point2::new(4.0, 0.0),
                    2,
                    Run::Arc {
                        center: Point2::new(4.0, 1.0),
                        ccw: true,
                    },
                ),
                (Point2::new(4.0, 2.0), 3, Run::Line),
                (
                    Point2::new(0.0, 2.0),
                    4,
                    Run::Arc {
                        center: Point2::new(0.0, 1.0),
                        ccw: true,
                    },
                ),
            ],
        };
        assert!((slot.signed_area() - (8.0 + PI)).abs() < 1e-12);
        for contour in [slot.clone(), slot.reversed()] {
            let (solid, _) = prism(&Plane::FRONT, &contour, &[], Vec3::new(0.0, 0.0, 1.5)).unwrap();
            solid.check().unwrap();
            let shape = Shape {
                solids: vec![solid],
            };
            assert!((shape.volume().unwrap() - (8.0 + PI) * 1.5).abs() < 1e-9);
            assert_eq!(shape.counts().edges, 12);
        }
    }

    #[test]
    fn a_box_is_where_it_is_asked_for() {
        let shape = make_box(Point3::new(1.0, 2.0, 3.0), Vec3::new(4.0, 5.0, 6.0)).unwrap();
        shape.check().unwrap();
        assert!((shape.volume().unwrap() - 120.0).abs() < 1e-9);
        let (lo, hi) = shape.bounds().unwrap();
        assert_eq!(lo, Point3::new(1.0, 2.0, 3.0));
        assert_eq!(hi, Point3::new(5.0, 7.0, 9.0));
        let c = shape.centre_of_mass().unwrap().unwrap();
        assert!(c.distance(Point3::new(3.0, 4.5, 6.0)) < 1e-9);
        let counts = shape.counts();
        assert_eq!((counts.faces, counts.edges, counts.vertices), (6, 12, 8));
        assert!(make_box(Point3::ORIGIN, Vec3::new(1.0, 0.0, 1.0)).is_err());
    }
}
