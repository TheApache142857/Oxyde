//! Display meshes: one triangle mesh per face and one polyline per edge, numbered like the
//! shape's topology.
//!
//! Planar faces of straight edges are exact. An arc is cut into chords no further from it
//! than the deflection asked for (and no longer than a twelfth of a turn); a planar face
//! with arcs is triangulated over those chords. A face on a surface of revolution is meshed
//! in its `(θ, v)` plane: cut into strips at every angle a point of its boundary is at and
//! at steps fine enough for the deflection, each strip into trapezoids between the boundary's
//! chords that cross it, each trapezoid stitched into triangles between its two sides
//! (rows of points on the surface, or the points of a boundary edge lying there). Every mesh
//! vertex on an edge is a point of that edge's polyline or of one of its chords, so faces
//! meet without gaps.

// Indices into the solid's own vertices, and into the points gathered here.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;

use oxyde_geom::{Point2, Point3};
use oxyde_kernel::{FaceMesh, Mesh};

use crate::brep::{EdgeUse, Face, ParamUse, Shape, Solid};
use crate::geometry::Surface;
use crate::revolution::{Meridian, Revolution, unwrap};
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
                Surface::Revolution(surface) => {
                    revolution_mesh(solid, face, surface, &polylines, deflection)?
                }
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

/// The angle between points of a circle of `radius` whose chord is at most `deflection`
/// from it, at most [`MAX_STEP`].
fn angular_step(radius: f64, deflection: f64) -> f64 {
    let d = deflection.max(1e-6);
    if d < radius {
        (2.0 * (1.0 - d / radius).acos()).min(MAX_STEP)
    } else {
        MAX_STEP
    }
}

/// The points of an edge from its start to its end, both included: a degenerate edge's
/// one point.
fn edge_polyline(solid: &Solid, edge: &crate::brep::Edge, deflection: f64) -> Vec<Point3> {
    edge.curve.polyline(
        solid.point(edge.start),
        solid.point(edge.end),
        edge.is_closed(),
        deflection,
    )
}

/// An edge's polyline the way a loop runs along it.
fn use_polyline(edge_use: EdgeUse, polylines: &[Vec<Point3>]) -> Vec<Point3> {
    let mut line = polylines[edge_use.edge as usize].clone();
    if edge_use.reversed {
        line.reverse();
    }
    line
}

/// The points of a loop in order, each once: every edge's polyline, its own way or
/// reversed, without its last point (the next edge's first).
fn loop_points(edges: &[EdgeUse], polylines: &[Vec<Point3>]) -> Vec<Point3> {
    let mut out = Vec::new();
    for &edge_use in edges {
        let line = use_polyline(edge_use, polylines);
        out.extend(line.iter().take(line.len().saturating_sub(1)));
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
            let d = solid.point(v) - origin;
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

/// A piece of a face's boundary between two of its polyline's points that is not along
/// the face's `θ`: from `a` to `b` in `(θ, v)`, from `pa` to `pb` in space.
#[derive(Debug, Clone, Copy)]
struct Chord {
    a: (f64, f64),
    b: (f64, f64),
    pa: Point3,
    pb: Point3,
}

impl Chord {
    /// Its `v` and its point at `θ` (between its ends), along it.
    fn at(&self, theta: f64) -> (f64, Point3) {
        let t = ((theta - self.a.0) / (self.b.0 - self.a.0)).clamp(0.0, 1.0);
        (
            self.a.1 + (self.b.1 - self.a.1) * t,
            self.pa + (self.pb - self.pa) * t,
        )
    }

    fn spans(&self, theta: f64) -> bool {
        self.a.0.min(self.b.0) < theta && theta < self.a.0.max(self.b.0)
    }
}

/// A piece of a face's boundary along a line of constant `θ`: its points with their `v`.
#[derive(Debug, Clone)]
struct Side {
    theta: f64,
    points: Vec<(f64, Point3)>,
}

/// The mesh vertices of one face, each once.
#[derive(Default)]
struct Vertices {
    positions: Vec<Point3>,
    index: HashMap<[u64; 3], u32>,
}

impl Vertices {
    fn at(&mut self, p: Point3) -> u32 {
        let key = [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
        *self.index.entry(key).or_insert_with(|| {
            self.positions.push(p);
            self.positions.len() as u32 - 1
        })
    }
}

/// A face of a surface of revolution meshed in its `(θ, v)` plane (see the module's
/// documentation).
fn revolution_mesh(
    solid: &Solid,
    face: &Face,
    surface: &Revolution,
    polylines: &[Vec<Point3>],
    deflection: f64,
) -> Result<FaceMesh, String> {
    let loops = solid.param_loops(face, surface)?;
    let mut chords: Vec<Chord> = Vec::new();
    let mut sides: Vec<Side> = Vec::new();
    let mut rho_max: f64 = 0.0;
    for u in loops.iter().flatten() {
        let line = use_polyline(u.edge_use, polylines);
        for p in &line {
            rho_max = rho_max.max(surface.rho_z(*p).0);
        }
        let n = line.len().saturating_sub(1).max(1);
        // A straight use's points are evenly along it in (θ, v); a bending one's are where
        // they are, unwrapped from its start.
        let bent: Vec<(f64, f64)> = if u.straight {
            Vec::new()
        } else {
            let mut near = u.from;
            line.iter()
                .map(|&p| {
                    let (t, v) = surface.params(p);
                    let theta = if surface.rho_z(p).0 > 1e-9 {
                        unwrap(near.0, t)
                    } else {
                        near.0
                    };
                    let v = if surface.v_periodic() {
                        unwrap(near.1, v)
                    } else {
                        v
                    };
                    near = (theta, v);
                    near
                })
                .collect()
        };
        let param = |i: usize| -> (f64, f64) {
            if let Some(&p) = bent.get(i) {
                return p;
            }
            let t = i as f64 / n as f64;
            (
                u.from.0 + (u.to.0 - u.from.0) * t,
                u.from.1 + (u.to.1 - u.from.1) * t,
            )
        };
        let point = |i: usize| line[i.min(line.len() - 1)];
        if is_vertical(u) {
            sides.push(Side {
                theta: u.from.0,
                points: (0..=n).map(|i| (param(i).1, point(i))).collect(),
            });
        } else {
            for i in 0..n {
                if (param(i + 1).0 - param(i).0).abs() > 1e-12 {
                    chords.push(Chord {
                        a: param(i),
                        b: param(i + 1),
                        pa: point(i),
                        pb: point(i + 1),
                    });
                }
            }
        }
    }

    // The strips' edges: every boundary point's angle, then steps between them fine enough
    // for the widest circle of the face.
    let mut thetas: Vec<f64> = chords
        .iter()
        .flat_map(|c| [c.a.0, c.b.0])
        .chain(sides.iter().map(|s| s.theta))
        .collect();
    thetas.sort_by(f64::total_cmp);
    thetas.dedup_by(|a, b| (*a - *b).abs() <= 1e-12);
    let step = angular_step(rho_max.max(1e-9), deflection);
    let mut breaks = Vec::with_capacity(thetas.len());
    for pair in thetas.windows(2) {
        let (t0, t1) = (pair[0], pair[1]);
        let k = ((t1 - t0) / step).ceil().max(1.0) as usize;
        for i in 0..k {
            breaks.push(t0 + (t1 - t0) * i as f64 / k as f64);
        }
    }
    if let Some(&last) = thetas.last() {
        breaks.push(last);
    }

    // Rows of points inside the face, along its meridian when that is curved.
    let rows: Vec<f64> = match surface.meridian {
        Meridian::Line { .. } => Vec::new(),
        Meridian::Circle { radius, .. } => {
            let (lo, hi) = chords
                .iter()
                .flat_map(|c| [c.a.1, c.b.1])
                .chain(sides.iter().flat_map(|s| s.points.iter().map(|p| p.0)))
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(v), hi.max(v))
                });
            let step = angular_step(radius, deflection);
            let k = ((hi - lo) / step).ceil().max(1.0) as usize;
            (1..k)
                .map(|i| lo + (hi - lo) * i as f64 / k as f64)
                .collect()
        }
    };

    let mut vertices = Vertices::default();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    for (strip, pair) in breaks.windows(2).enumerate() {
        let (ta, tb) = (pair[0], pair[1]);
        let mid = (ta + tb) / 2.0;
        let mut crossing: Vec<&Chord> = chords.iter().filter(|c| c.spans(mid)).collect();
        crossing.sort_by(|p, q| p.at(mid).0.total_cmp(&q.at(mid).0));
        if !crossing.len().is_multiple_of(2) {
            return Err(format!(
                "a face of revolution does not close across its strip {strip}"
            ));
        }
        for pair in crossing.chunks(2) {
            let (lo, hi) = (pair[0], pair[1]);
            let chain = |theta: f64, vertices: &mut Vertices| -> Vec<(f64, u32)> {
                let (v_lo, p_lo) = lo.at(theta);
                let (v_hi, p_hi) = hi.at(theta);
                let mut out = vec![(v_lo, vertices.at(p_lo))];
                // A boundary edge along this angle, over the stretch between the chords.
                let side = sides.iter().find(|s| {
                    let (s_lo, s_hi) = s
                        .points
                        .iter()
                        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &(v, _)| {
                            (a.min(v), b.max(v))
                        });
                    (s.theta - theta).abs() <= 1e-12 && s_lo < v_hi - 1e-12 && s_hi > v_lo + 1e-12
                });
                match side {
                    Some(side) => {
                        let mut inner: Vec<(f64, Point3)> = side
                            .points
                            .iter()
                            .copied()
                            .filter(|&(v, _)| v > v_lo + 1e-12 && v < v_hi - 1e-12)
                            .collect();
                        inner.sort_by(|p, q| p.0.total_cmp(&q.0));
                        out.extend(inner.into_iter().map(|(v, p)| (v, vertices.at(p))));
                    }
                    None => {
                        for &v in rows.iter().filter(|&&v| v > v_lo + 1e-9 && v < v_hi - 1e-9) {
                            out.push((v, vertices.at(surface.point(theta, v))));
                        }
                    }
                }
                out.push((v_hi, vertices.at(p_hi)));
                out
            };
            let left = chain(ta, &mut vertices);
            let right = chain(tb, &mut vertices);
            stitch(&left, &right, surface.outward, &mut triangles);
        }
    }
    Ok(FaceMesh {
        positions: vertices.positions.iter().map(|&p| single(p)).collect(),
        triangles,
    })
}

fn is_vertical(u: &ParamUse) -> bool {
    u.is_vertical()
}

/// Triangles between two chains of points from bottom to top, `left` at the smaller
/// angle: counter-clockwise in `(θ, v)`, which faces along the surface's own normal, so
/// turned round for a face whose normal is the other way. Triangles with two corners at one
/// point (at a pole) are left out.
fn stitch(left: &[(f64, u32)], right: &[(f64, u32)], outward: bool, out: &mut Vec<[u32; 3]>) {
    let (mut i, mut j) = (0, 0);
    let mut push = |a: u32, b: u32, c: u32| {
        if a != b && b != c && a != c {
            out.push(if outward { [a, b, c] } else { [a, c, b] });
        }
    };
    while i + 1 < left.len() || j + 1 < right.len() {
        let advance_left = if i + 1 >= left.len() {
            false
        } else if j + 1 >= right.len() {
            true
        } else {
            left[i + 1].0 <= right[j + 1].0
        };
        if advance_left {
            push(left[i].1, right[j].1, left[i + 1].1);
            i += 1;
        } else {
            push(left[i].1, right[j].1, right[j + 1].1);
            j += 1;
        }
    }
}
