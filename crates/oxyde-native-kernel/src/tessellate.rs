//! Display meshes: one triangle mesh per face and one polyline per edge, numbered like the
//! shape's topology.
//!
//! Planar faces of straight edges are exact. An arc is cut into chords no further from it
//! than the deflection asked for (and no longer than a twelfth of a turn); a planar face
//! with arcs is triangulated over those chords, and a cylinder's face is cut into strips at
//! the angles its arcs were cut at, each strip into quadrilaterals between the heights its
//! boundary crosses it at. Every mesh vertex on an edge is a point of that edge's polyline.

// Indices into the solid's own vertices, and into the points gathered here.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;

use oxyde_geom::{Point2, Point3};
use oxyde_kernel::{FaceMesh, Mesh};

use crate::brep::{Face, Shape, Solid};
use crate::geometry::{Curve, Cylinder, Surface};
use crate::triangulate;

/// The largest angle a chord of an arc may span, whatever the deflection.
const MAX_STEP: f64 = std::f64::consts::TAU / 12.0;

pub(crate) fn mesh(shape: &Shape, deflection: f64) -> Result<Mesh, String> {
    let mut mesh = Mesh::default();
    for solid in &shape.solids {
        let polylines: Vec<Vec<Point3>> = solid
            .edges
            .iter()
            .map(|edge| edge_polyline(solid, edge, deflection))
            .collect();
        for face in &solid.faces {
            mesh.faces.push(match &face.surface {
                Surface::Plane(_) => planar_mesh(face, &polylines)?,
                Surface::Cylinder(cylinder) => cylinder_mesh(solid, face, cylinder, &polylines)?,
            });
        }
        mesh.edges.extend(
            polylines
                .iter()
                .map(|line| line.iter().map(|&p| single(p)).collect()),
        );
    }
    Ok(mesh)
}

fn single(p: Point3) -> [f32; 3] {
    [p.x as f32, p.y as f32, p.z as f32]
}

/// The points of an edge from its start to its end, both included.
fn edge_polyline(solid: &Solid, edge: &crate::brep::Edge, deflection: f64) -> Vec<Point3> {
    let (a, b) = (solid.point(edge.start), solid.point(edge.end));
    match edge.curve {
        Curve::Line => vec![a, b],
        Curve::Circle(circle) => {
            let sweep = solid.sweep(edge);
            let d = deflection.max(1e-6);
            let step = if d < circle.radius {
                (2.0 * (1.0 - d / circle.radius).acos()).min(MAX_STEP)
            } else {
                MAX_STEP
            };
            let n = ((sweep / step).ceil() as usize).max(if edge.is_closed() { 3 } else { 1 });
            let start = circle.angle_of(a);
            let mut points: Vec<Point3> = (0..n)
                .map(|i| {
                    if i == 0 {
                        a
                    } else {
                        circle.point_at(start + sweep * i as f64 / n as f64)
                    }
                })
                .collect();
            points.push(b);
            points
        }
    }
}

/// The points of a loop in order, each once: every edge's polyline, its own way or
/// reversed, without its last point (the next edge's first).
fn loop_points(edges: &[crate::brep::EdgeUse], polylines: &[Vec<Point3>]) -> Vec<Point3> {
    let mut out = Vec::new();
    for &edge_use in edges {
        let line = &polylines[edge_use.edge as usize];
        if edge_use.reversed {
            out.extend(line.iter().rev().take(line.len() - 1));
        } else {
            out.extend(line.iter().take(line.len() - 1));
        }
    }
    out
}

/// The face's corners, each once (vertices of the solid), and triangles over them (indices
/// into the corners), triangulated in the face's plane: for the booleans, which take
/// planar faces of straight edges. The plane's frame is right-handed about the outward
/// normal, so triangles counter-clockwise in it are counter-clockwise seen from outside.
pub(crate) fn triangulate_face(
    solid: &Solid,
    face: &Face,
) -> Result<(Vec<u32>, Vec<[usize; 3]>), String> {
    let plane = face
        .surface
        .plane()
        .ok_or_else(|| "a curved face cannot be cut into polygons".to_owned())?;
    let mut corners: Vec<u32> = Vec::new();
    let mut loops: Vec<Vec<usize>> = Vec::with_capacity(face.loops.len());
    for edges in &face.loops {
        let mut ring = Vec::with_capacity(edges.len());
        for &edge_use in edges {
            let vertex = solid.start_of(edge_use);
            let local = match corners.iter().position(|&v| v == vertex) {
                Some(local) => local,
                None => {
                    corners.push(vertex);
                    corners.len() - 1
                }
            };
            ring.push(local);
        }
        loops.push(ring);
    }
    let (origin, x) = (plane.origin, plane.x_dir);
    let y = plane.normal.cross(x);
    let flat: Vec<Point2> = corners
        .iter()
        .map(|&v| {
            let d = solid.vertices[v as usize] - origin;
            Point2::new(d.dot(x), d.dot(y))
        })
        .collect();
    let triangles = triangulate::polygon_with_holes(&flat, &loops)?;
    Ok((corners, triangles))
}

fn planar_mesh(face: &Face, polylines: &[Vec<Point3>]) -> Result<FaceMesh, String> {
    let plane = face
        .surface
        .plane()
        .ok_or_else(|| "not a planar face".to_owned())?;
    let mut positions: Vec<Point3> = Vec::new();
    let mut loops: Vec<Vec<usize>> = Vec::with_capacity(face.loops.len());
    for edges in &face.loops {
        let points = loop_points(edges, polylines);
        let first = positions.len();
        loops.push((first..first + points.len()).collect());
        positions.extend(points);
    }
    let (origin, x) = (plane.origin, plane.x_dir);
    let y = plane.normal.cross(x);
    let flat: Vec<Point2> = positions
        .iter()
        .map(|&p| {
            let d = p - origin;
            Point2::new(d.dot(x), d.dot(y))
        })
        .collect();
    let triangles = triangulate::polygon_with_holes(&flat, &loops)?;
    Ok(FaceMesh {
        positions: positions.iter().map(|&p| single(p)).collect(),
        triangles: triangles
            .iter()
            .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
            .collect(),
    })
}

/// A cylinder's face in strips: its boundary walked in the `(angle, height)` plane (the
/// angle unwrapped as the loop goes round), cut at every angle a point of it is at; in each
/// strip the boundary's arcs cross at heights that pair up into quadrilaterals inside the
/// face.
fn cylinder_mesh(
    solid: &Solid,
    face: &Face,
    cylinder: &Cylinder,
    polylines: &[Vec<Point3>],
) -> Result<FaceMesh, String> {
    // Arcs of the boundary as (from angle, to angle, height).
    let mut arcs: Vec<(f64, f64, f64)> = Vec::new();
    let mut angles: Vec<f64> = Vec::new();
    for edges in &face.loops {
        let Some(&first) = edges.first() else {
            continue;
        };
        let (mut angle, _) = cylinder.coordinates(solid.point(solid.start_of(first)));
        for &edge_use in edges {
            let edge = &solid.edges[edge_use.edge as usize];
            let mut line = polylines[edge_use.edge as usize].clone();
            if edge_use.reversed {
                line.reverse();
            }
            match edge.curve {
                Curve::Line => angles.push(angle),
                Curve::Circle(circle) => {
                    let mut turn = solid.sweep(edge);
                    if circle.normal.dot(cylinder.axis.direction) < 0.0 {
                        turn = -turn;
                    }
                    if edge_use.reversed {
                        turn = -turn;
                    }
                    let (_, height) = cylinder.coordinates(line[0]);
                    let n = line.len() - 1;
                    for i in 0..n {
                        let a0 = angle + turn * i as f64 / n as f64;
                        let a1 = angle + turn * (i + 1) as f64 / n as f64;
                        arcs.push((a0, a1, height));
                        angles.push(a0);
                    }
                    angle += turn;
                    angles.push(angle);
                }
            }
        }
    }
    angles.sort_by(f64::total_cmp);
    angles.dedup_by(|a, b| (*a - *b).abs() <= 1e-12);

    let mut positions: Vec<Point3> = Vec::new();
    let mut index: HashMap<(usize, u64), u32> = HashMap::new();
    let mut at = |strip_edge: usize, angle: f64, height: f64, positions: &mut Vec<Point3>| -> u32 {
        *index
            .entry((strip_edge, height.to_bits()))
            .or_insert_with(|| {
                positions.push(cylinder.point(angle, height));
                positions.len() as u32 - 1
            })
    };
    let mut triangles = Vec::new();
    for (i, pair) in angles.windows(2).enumerate() {
        let (a0, a1) = (pair[0], pair[1]);
        let mid = (a0 + a1) / 2.0;
        let mut heights: Vec<f64> = arcs
            .iter()
            .filter(|(from, to, _)| from.min(*to) < mid && mid < from.max(*to))
            .map(|&(_, _, h)| h)
            .collect();
        heights.sort_by(f64::total_cmp);
        if !heights.len().is_multiple_of(2) {
            return Err("a cylinder's face does not close across a strip".to_owned());
        }
        for h in heights.chunks(2) {
            let corners = [
                at(i, a0, h[0], &mut positions),
                at(i + 1, a1, h[0], &mut positions),
                at(i + 1, a1, h[1], &mut positions),
                at(i, a0, h[1], &mut positions),
            ];
            // Counter-clockwise in (angle, height) faces away from the axis.
            let quad = if cylinder.outward {
                [
                    [corners[0], corners[1], corners[2]],
                    [corners[0], corners[2], corners[3]],
                ]
            } else {
                [
                    [corners[0], corners[2], corners[1]],
                    [corners[0], corners[3], corners[2]],
                ]
            };
            triangles.extend(quad);
        }
    }
    Ok(FaceMesh {
        positions: positions.iter().map(|&p| single(p)).collect(),
        triangles,
    })
}
