//! Solids made from scratch: boxes and extrusions (prisms over a planar profile).

// Indices into the vertices and edges made here, laid out by `prism` itself.
#![allow(clippy::indexing_slicing)]

use oxyde_geom::{Curve2, LINEAR_TOLERANCE, Loop, Plane, Point2, Point3, Profile, Vec3};
use oxyde_kernel::{FaceSource, OpHistory};

use crate::NativeError;
use crate::brep::{Edge, EdgeUse, Face, Shape, Solid, cyclic_pairs};

/// A closed contour of straight segments in a plane's coordinates: each corner starts the
/// segment with that id, which runs to the next corner (the last one back to the first).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlanarLoop {
    pub corners: Vec<(Point2, u32)>,
}

impl PlanarLoop {
    /// The contour of a profile loop, if it is made of lines only.
    fn of(contour: &Loop) -> Result<PlanarLoop, NativeError> {
        let corners = contour
            .segments
            .iter()
            .map(|segment| match segment.curve {
                Curve2::Line { start, .. } => Ok((start, segment.id)),
                Curve2::Arc { .. } | Curve2::Circle { .. } => {
                    Err(NativeError::Unsupported("extrude arcs and circles"))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        if corners.len() < 3 {
            return Err(NativeError::Failed(
                "a contour needs three corners at least".to_owned(),
            ));
        }
        Ok(PlanarLoop { corners })
    }

    /// Enclosed area, positive when the contour runs counter-clockwise.
    fn signed_area(&self) -> f64 {
        cyclic_pairs(&self.corners)
            .map(|((a, _), (b, _))| a.x * b.y - b.x * a.y)
            .sum::<f64>()
            / 2.0
    }

    /// The same contour run the other way, each segment keeping its id.
    fn reversed(&self) -> PlanarLoop {
        let n = self.corners.len();
        // Segment `n - 1 - k`, reversed, runs from the corner after it (`n - k`, the first
        // corner for k = 0) to the corner it started from.
        PlanarLoop {
            corners: (0..n)
                .map(|k| (self.corners[(n - k) % n].0, self.corners[n - 1 - k].1))
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
            (Point2::new(0.0, 0.0), 0),
            (Point2::new(size.x, 0.0), 1),
            (Point2::new(size.x, size.y), 2),
            (Point2::new(0.0, size.y), 3),
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
/// ([`FaceSource::ProfileSegment`]); the face on the sketch plane ([`FaceSource::StartCap`]);
/// the face at the far end ([`FaceSource::EndCap`]). The solid lies on the side of the plane
/// `vector` points to, whichever way that is, so it is never inside out.
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
    // `up` is the side the solid is on. Contours are turned to run counter-clockwise about
    // it (holes clockwise), which is counter-clockwise in the plane's coordinates when `up`
    // is the plane's normal and clockwise when it is the other way.
    let up = if along > 0.0 { normal } else { -normal };
    let about_up = |contour: &PlanarLoop| contour.signed_area() * along.signum();
    let mut contours = Vec::with_capacity(1 + holes.len());
    for (i, contour) in std::iter::once(outer).chain(holes).enumerate() {
        let turn = about_up(contour);
        if turn.abs() <= LINEAR_TOLERANCE * LINEAR_TOLERANCE {
            return Err(failed("a contour of the profile encloses no area"));
        }
        let counter_clockwise = i == 0;
        contours.push(if (turn > 0.0) == counter_clockwise {
            contour.clone()
        } else {
            contour.reversed()
        });
    }

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
        for &(p, _) in &contour.corners {
            let bottom = plane.to_world(p);
            solid.vertices.push(bottom);
            solid.vertices.push(bottom + vector);
        }
        for k in 0..n {
            let next = (k + 1) % n;
            let (b, t) = (v0 + 2 * k, v0 + 2 * k + 1);
            let (b_next, t_next) = (v0 + 2 * next, v0 + 2 * next + 1);
            solid.edges.push(Edge {
                start: b,
                end: b_next,
            });
            solid.edges.push(Edge {
                start: t,
                end: t_next,
            });
            solid.edges.push(Edge { start: b, end: t });
        }
    }

    let mut sources = Vec::new();
    for (contour, &(v0, e0)) in contours.iter().zip(&starts) {
        let n = contour.corners.len() as u32;
        for (k, &(_, id)) in (0..n).zip(&contour.corners) {
            let next = (k + 1) % n;
            let (bottom, top) = (e0 + 3 * k, e0 + 3 * k + 1);
            let (across, across_next) = (e0 + 3 * k + 2, e0 + 3 * next + 2);
            let a = solid.vertices[(v0 + 2 * k) as usize];
            let b = solid.vertices[(v0 + 2 * next) as usize];
            let along_edge = (b - a)
                .normalized()
                .ok_or_else(|| failed("a segment of the profile has no length"))?;
            // Seen from outside, the face runs along the segment, up across the next corner,
            // back along the far edge and down across this corner: counter-clockwise about
            // the segment's direction crossed with the sweep.
            let outward = along_edge
                .cross(vector)
                .normalized()
                .ok_or_else(|| failed("a segment of the profile runs along the extrusion"))?;
            solid.faces.push(Face {
                plane: Plane {
                    origin: a,
                    x_dir: along_edge,
                    normal: outward,
                },
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
        plane: Plane {
            origin: plane.origin,
            x_dir: plane.x_dir,
            normal: -up,
        },
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
        plane: Plane {
            origin: plane.origin + vector,
            x_dir: plane.x_dir,
            normal: up,
        },
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
    use super::*;

    fn rectangle(w: f64, h: f64) -> PlanarLoop {
        PlanarLoop {
            corners: vec![
                (Point2::new(0.0, 0.0), 1),
                (Point2::new(w, 0.0), 2),
                (Point2::new(w, h), 3),
                (Point2::new(0.0, h), 4),
            ],
        }
    }

    #[test]
    fn reversing_a_contour_keeps_each_segment_between_its_corners() {
        let r = rectangle(2.0, 1.0).reversed();
        // Segment 4 ran from (0, 1) to (0, 0): reversed, it starts at (0, 0).
        assert_eq!(
            r.corners,
            vec![
                (Point2::new(0.0, 0.0), 4),
                (Point2::new(0.0, 1.0), 3),
                (Point2::new(2.0, 1.0), 2),
                (Point2::new(2.0, 0.0), 1),
            ]
        );
        assert!(r.signed_area() < 0.0);
        assert_eq!(r.reversed(), rectangle(2.0, 1.0));
    }

    #[test]
    fn a_prism_is_closed_whichever_way_it_goes() {
        let hole = PlanarLoop {
            corners: vec![
                (Point2::new(1.0, 1.0), 5),
                (Point2::new(1.0, 2.0), 6),
                (Point2::new(3.0, 2.0), 7),
                (Point2::new(3.0, 1.0), 8),
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
            assert!((shape.volume() - (12.0 - 2.0) * 2.0).abs() < 1e-9);
        }
    }

    #[test]
    fn a_box_is_where_it_is_asked_for() {
        let shape = make_box(Point3::new(1.0, 2.0, 3.0), Vec3::new(4.0, 5.0, 6.0)).unwrap();
        shape.check().unwrap();
        assert!((shape.volume() - 120.0).abs() < 1e-9);
        let (lo, hi) = shape.bounds().unwrap();
        assert_eq!(lo, Point3::new(1.0, 2.0, 3.0));
        assert_eq!(hi, Point3::new(5.0, 7.0, 9.0));
        let c = shape.centre_of_mass().unwrap();
        assert!(c.distance(Point3::new(3.0, 4.5, 6.0)) < 1e-9);
        let counts = shape.counts();
        assert_eq!((counts.faces, counts.edges, counts.vertices), (6, 12, 8));
        assert!(make_box(Point3::ORIGIN, Vec3::new(1.0, 0.0, 1.0)).is_err());
    }
}
