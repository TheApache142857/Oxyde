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
use crate::geometry::{SplineSurface, Surface};
use crate::revolution::{Meridian, Revolution, unwrap};
use crate::triangulate;

/// The largest angle a chord of an arc may span, whatever the deflection.
const MAX_STEP: f64 = std::f64::consts::TAU / 12.0;

pub(crate) fn mesh(shape: &Shape, deflection: f64) -> Result<Mesh, String> {
    let mut mesh = Mesh::default();
    for (i, solid) in shape.solids.iter().enumerate() {
        let polylines: Vec<Vec<Point3>> = solid
            .edges
            .iter()
            .map(|edge| edge_polyline(solid, edge, deflection))
            .collect();
        for (k, face) in solid.faces.iter().enumerate() {
            let face_mesh = match &face.surface {
                Surface::Plane(_) => planar_mesh(face, &polylines),
                Surface::Revolution(surface) => {
                    revolution_mesh(solid, face, surface, &polylines, deflection)
                }
                Surface::Spline(surface) => {
                    spline_mesh(solid, face, surface, &polylines, deflection)
                }
            };
            mesh.faces
                .push(face_mesh.map_err(|e| format!("solid {i}, face {k}: {e}"))?);
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

/// An edge use of a B-spline face: its polyline, each point's `(u, v)`, the loop it is in.
struct UvUse {
    in_loop: usize,
    line: Vec<Point3>,
    params: Vec<(f64, f64)>,
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
    // The loops as polygons in (θ, v), for when the strips cannot be made.
    let mut polygons: Vec<Vec<((f64, f64), Point3)>> = Vec::with_capacity(loops.len());
    for (k, u) in loops
        .iter()
        .enumerate()
        .flat_map(|(k, l)| l.iter().map(move |u| (k, u)))
    {
        if polygons.len() <= k {
            polygons.push(Vec::new());
        }
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
            let mut laid: Vec<(f64, f64)> = line
                .iter()
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
                .collect();
            // Its ends where the walk round the face puts them, which the straight uses
            // beside it run from and to: the corners' own angles may be off by as much as
            // their tolerances (a file's geometry), the difference spread along it.
            if let (Some(&first), Some(&last)) = (laid.first(), laid.last()) {
                let (d0, d1) = (
                    (u.from.0 - first.0, u.from.1 - first.1),
                    (u.to.0 - last.0, u.to.1 - last.1),
                );
                let k = laid.len().saturating_sub(1).max(1) as f64;
                for (i, q) in laid.iter_mut().enumerate() {
                    let t = i as f64 / k;
                    q.0 += d0.0 * (1.0 - t) + d1.0 * t;
                    q.1 += d0.1 * (1.0 - t) + d1.1 * t;
                }
            }
            laid
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
        polygons[k].extend((0..n).map(|i| (param(i), point(i))));
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

    let point = |t: f64, v: f64| surface.point(t, v);
    strip_mesh(&chords, &sides, &breaks, &rows, &point, surface.outward).or_else(|_| {
        // Lengths across and along: θ by the face's widest circle, v by the meridian's.
        let along = match surface.meridian {
            Meridian::Line { slope, .. } => (1.0 + slope * slope).sqrt(),
            Meridian::Circle { radius, .. } => radius,
        };
        refined_mesh(
            &polygons,
            (rho_max.max(1e-9), along),
            &point,
            deflection,
            surface.outward,
        )
    })
}

/// The triangles of a face laid out in a parameter plane `(t, v)`: in strips between the
/// `breaks` along `t`, each strip in trapezoids between the boundary's `chords` that cross
/// it, each trapezoid stitched between its two sides, rows of points at the `rows` along
/// `v` (`point` gives them) or the points of a boundary `side` lying there. Triangles run
/// counter-clockwise in `(t, v)` when `outward`.
fn strip_mesh(
    chords: &[Chord],
    sides: &[Side],
    breaks: &[f64],
    rows: &[f64],
    point: &dyn Fn(f64, f64) -> Point3,
    outward: bool,
) -> Result<FaceMesh, String> {
    let mut vertices = Vertices::default();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    for (strip, pair) in breaks.windows(2).enumerate() {
        let (ta, tb) = (pair[0], pair[1]);
        let mid = (ta + tb) / 2.0;
        let mut crossing: Vec<&Chord> = chords.iter().filter(|c| c.spans(mid)).collect();
        crossing.sort_by(|p, q| p.at(mid).0.total_cmp(&q.at(mid).0));
        if !crossing.len().is_multiple_of(2) {
            return Err(format!(
                "a curved face does not close across its strip {strip}"
            ));
        }
        for pair in crossing.chunks(2) {
            let (lo, hi) = (pair[0], pair[1]);
            let chain = |theta: f64, vertices: &mut Vertices| -> Vec<(f64, u32)> {
                let (v_lo, p_lo) = lo.at(theta);
                let (v_hi, p_hi) = hi.at(theta);
                let mut out = vec![(v_lo, vertices.at(p_lo))];
                // The boundary edges along this angle, over the stretch between the chords:
                // one, or several end to end (a side split by a vertex).
                let along: Vec<&Side> = sides
                    .iter()
                    .filter(|s| {
                        let (s_lo, s_hi) = s
                            .points
                            .iter()
                            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &(v, _)| {
                                (a.min(v), b.max(v))
                            });
                        (s.theta - theta).abs() <= 1e-12
                            && s_lo < v_hi - 1e-12
                            && s_hi > v_lo + 1e-12
                    })
                    .collect();
                if along.is_empty() {
                    for &v in rows.iter().filter(|&&v| v > v_lo + 1e-9 && v < v_hi - 1e-9) {
                        out.push((v, vertices.at(point(theta, v))));
                    }
                } else {
                    let mut inner: Vec<(f64, Point3)> = along
                        .iter()
                        .flat_map(|s| s.points.iter().copied())
                        .filter(|&(v, _)| v > v_lo + 1e-12 && v < v_hi - 1e-12)
                        .collect();
                    inner.sort_by(|p, q| p.0.total_cmp(&q.0));
                    // Where two of them meet, their shared end once.
                    inner.dedup_by(|p, q| p.1 == q.1);
                    out.extend(inner.into_iter().map(|(v, p)| (v, vertices.at(p))));
                }
                out.push((v_hi, vertices.at(p_hi)));
                out
            };
            let left = chain(ta, &mut vertices);
            let right = chain(tb, &mut vertices);
            stitch(&left, &right, outward, &mut triangles);
        }
    }
    Ok(FaceMesh {
        positions: vertices.positions.iter().map(|&p| single(p)).collect(),
        triangles,
    })
}

/// A face of a B-spline surface meshed in its `(u, v)` plane, as a face of revolution is
/// in its `(θ, v)` ([`strip_mesh`]): its edges' points taken onto the surface, strips at
/// every `u` a point of them is at and at steps fine enough for the deflection, rows along
/// `v` likewise.
fn spline_mesh(
    solid: &Solid,
    face: &Face,
    surface: &SplineSurface,
    polylines: &[Vec<Point3>],
    deflection: f64,
) -> Result<FaceMesh, String> {
    let loops = crate::spline_face::uv_loops(solid, face, surface)?;
    let mut chords: Vec<Chord> = Vec::new();
    let mut sides: Vec<Side> = Vec::new();
    let (mut ulo, mut uhi, mut vlo, mut vhi) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    let (ua, ub) = surface.domain(0);
    let mut uses: Vec<UvUse> = Vec::new();
    for (k, u) in loops
        .iter()
        .enumerate()
        .flat_map(|(k, l)| l.iter().map(move |u| (k, u)))
    {
        let line = use_polyline(u.edge_use, polylines);
        let mut near = u.samples.first().map_or((0.0, 0.0), |s| (s.1, s.2));
        let params: Vec<(f64, f64)> = line
            .iter()
            .map(|&p| {
                near = crate::spline_face::uv_near(surface, p, near);
                near
            })
            .collect();
        for &(pu, pv) in &params {
            ulo = ulo.min(pu);
            uhi = uhi.max(pu);
            vlo = vlo.min(pv);
            vhi = vhi.max(pv);
        }
        uses.push(UvUse {
            in_loop: k,
            line,
            params,
        });
    }
    if !(uhi > ulo && vhi > vlo) {
        return Err("a B-spline face with no extent".to_owned());
    }
    let (du, dv) = crate::spline_face::steps(surface, (ulo, uhi), (vlo, vhi), deflection);
    // A polyline along one `u` within a thousandth of a step is a side of the face, at that
    // one `u`, and so are the ends of the polylines that meet it: an edge along a side of
    // the surface's domain projects onto it only within the edge's tolerance, and a side
    // taken point by point as chords cut the face into a strip at every point.
    let along = 1e-3 * du;
    let mut ends: Vec<(Point3, f64)> = Vec::new();
    let mut is_side = vec![false; uses.len()];
    for (side, UvUse { line, params, .. }) in is_side.iter_mut().zip(&mut uses) {
        let (lo, hi) = params
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| {
                (a.min(p.0), b.max(p.0))
            });
        if hi - lo <= along {
            // At the `u` of a side it continues (one side of the domain in several edges).
            let u0 = [line.first(), line.last()]
                .into_iter()
                .flatten()
                .find_map(|q| {
                    ends.iter()
                        .find(|(p, u)| p == q && *u >= lo - along && *u <= hi + along)
                })
                .map(|&(_, u)| u)
                .unwrap_or(0.5 * (lo + hi));
            for p in params.iter_mut() {
                p.0 = u0;
            }
            ends.extend(
                [line.first(), line.last()]
                    .into_iter()
                    .flatten()
                    .map(|&q| (q, u0)),
            );
            *side = true;
        }
    }
    for UvUse { line, params, .. } in uses
        .iter_mut()
        .zip(&is_side)
        .filter(|(_, side)| !**side)
        .map(|(u, _)| u)
    {
        // Only the end lying at that side: on a surface closed along `u`, a curve round it
        // ends at the seam a turn from where it began.
        for i in [0, line.len().saturating_sub(1)] {
            let u = params[i].0;
            if let Some(&(_, u0)) = ends
                .iter()
                .find(|(q, u0)| *q == line[i] && (u - u0).abs() <= along)
            {
                params[i].0 = u0;
            }
        }
    }
    let mut polygons: Vec<Vec<((f64, f64), Point3)>> = Vec::with_capacity(loops.len());
    for (
        UvUse {
            in_loop,
            line,
            params,
        },
        &side,
    ) in uses.iter().zip(&is_side)
    {
        if polygons.len() <= *in_loop {
            polygons.push(Vec::new());
        }
        polygons[*in_loop].extend(
            params
                .iter()
                .zip(line)
                .take(line.len().saturating_sub(1))
                .map(|(&a, &b)| (a, b)),
        );
        if side {
            sides.push(Side {
                theta: params.first().map_or(0.0, |p| p.0),
                points: params.iter().zip(line).map(|(p, &q)| (p.1, q)).collect(),
            });
        } else {
            for i in 0..line.len().saturating_sub(1) {
                if (params[i + 1].0 - params[i].0).abs() > 1e-12 * (ub - ua) {
                    chords.push(Chord {
                        a: params[i],
                        b: params[i + 1],
                        pa: line[i],
                        pb: line[i + 1],
                    });
                }
            }
        }
    }
    let mut us: Vec<f64> = chords
        .iter()
        .flat_map(|c| [c.a.0, c.b.0])
        .chain(sides.iter().map(|s| s.theta))
        .collect();
    us.sort_by(f64::total_cmp);
    us.dedup_by(|a, b| (*a - *b).abs() <= 1e-12);
    let mut breaks = Vec::with_capacity(us.len());
    for pair in us.windows(2) {
        let (t0, t1) = (pair[0], pair[1]);
        let k = ((t1 - t0) / du).ceil().max(1.0) as usize;
        for i in 0..k {
            breaks.push(t0 + (t1 - t0) * i as f64 / k as f64);
        }
    }
    if let Some(&last) = us.last() {
        breaks.push(last);
    }
    let k = ((vhi - vlo) / dv).ceil().max(1.0) as usize;
    let rows: Vec<f64> = (1..k)
        .map(|i| vlo + (vhi - vlo) * i as f64 / k as f64)
        .collect();
    let wrap = |x: f64, (a, b): (f64, f64), closed: bool| {
        if closed {
            a + (x - a).rem_euclid(b - a)
        } else {
            x
        }
    };
    let point = |u: f64, v: f64| {
        surface.point(
            wrap(u, surface.domain(0), surface.data.closed[0]),
            wrap(v, surface.domain(1), surface.data.closed[1]),
        )
    };
    strip_mesh(&chords, &sides, &breaks, &rows, &point, surface.outward).or_else(|_| {
        // Lengths across and along: the surface's rates at the face's middle.
        let d = surface.derivatives(
            wrap(0.5 * (ulo + uhi), surface.domain(0), surface.data.closed[0]),
            wrap(0.5 * (vlo + vhi), surface.domain(1), surface.data.closed[1]),
            1,
        );
        refined_mesh(
            &polygons,
            (d[1][0].length().max(1e-9), d[0][1].length().max(1e-9)),
            &point,
            deflection,
            surface.outward,
        )
    })
}

/// A face's mesh from its loops laid out as polygons in its parameter plane (each point's
/// parameters and its place): the polygons triangulated (ear clipping, the parameters
/// scaled by `scale` to be lengths), then each triangle's sides inside the face halved
/// where their middle lies further than `deflection` from the surface (`point`), a
/// triangle with no such side but standing off the surface at its centroid split there, a
/// few times over. The boundary is never cut, so the face meets its neighbours' meshes as
/// the strips' do. What a face the strips cannot mesh gets (a file's face bounded by
/// curves its writer fitted, a corner or two apart).
fn refined_mesh(
    polygons: &[Vec<((f64, f64), Point3)>],
    scale: (f64, f64),
    point: &dyn Fn(f64, f64) -> Point3,
    deflection: f64,
    outward: bool,
) -> Result<FaceMesh, String> {
    let mut params: Vec<(f64, f64)> = Vec::new();
    let mut positions: Vec<Point3> = Vec::new();
    let mut loops: Vec<Vec<usize>> = Vec::with_capacity(polygons.len());
    let mut boundary: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    for polygon in polygons {
        let first = positions.len();
        let n = polygon.len();
        for &(q, p) in polygon {
            params.push(q);
            positions.push(p);
        }
        for i in 0..n {
            let (a, b) = (first + i, first + (i + 1) % n);
            boundary.insert((a.min(b), a.max(b)));
        }
        loops.push((first..first + n).collect());
    }
    let flat: Vec<Point2> = params
        .iter()
        .map(|&(u, v)| Point2::new(u * scale.0, v * scale.1))
        .collect();
    let mut triangles = triangulate::polygon_with_holes(&flat, &loops)?;
    let tolerance = deflection.max(1e-6);
    let mut midpoints: HashMap<(usize, usize), usize> = HashMap::new();
    for _ in 0..8 {
        if triangles.len() > 200_000 {
            break;
        }
        // The sides to halve: inside the face, their middle off the surface.
        let mut split: HashMap<(usize, usize), usize> = HashMap::new();
        let mut centroids: Vec<usize> = Vec::new();
        for (t, tri) in triangles.iter().enumerate() {
            let mut any = false;
            for k in 0..3 {
                let (a, b) = (tri[k], tri[(k + 1) % 3]);
                let key = (a.min(b), a.max(b));
                if boundary.contains(&key) {
                    continue;
                }
                if split.contains_key(&key) {
                    any = true;
                    continue;
                }
                let m = (
                    (params[a].0 + params[b].0) / 2.0,
                    (params[a].1 + params[b].1) / 2.0,
                );
                let chord = positions[a] + (positions[b] - positions[a]) * 0.5;
                let on = point(m.0, m.1);
                if on.distance(chord) > tolerance {
                    let index = *midpoints.entry(key).or_insert_with(|| {
                        params.push(m);
                        positions.push(on);
                        params.len() - 1
                    });
                    split.insert(key, index);
                    any = true;
                }
            }
            if !any {
                let [a, b, c] = *tri;
                let m = (
                    (params[a].0 + params[b].0 + params[c].0) / 3.0,
                    (params[a].1 + params[b].1 + params[c].1) / 3.0,
                );
                let flat_centre = Point3::new(
                    (positions[a].x + positions[b].x + positions[c].x) / 3.0,
                    (positions[a].y + positions[b].y + positions[c].y) / 3.0,
                    (positions[a].z + positions[b].z + positions[c].z) / 3.0,
                );
                if point(m.0, m.1).distance(flat_centre) > tolerance {
                    centroids.push(t);
                }
            }
        }
        if split.is_empty() && centroids.is_empty() {
            break;
        }
        let mut next: Vec<[usize; 3]> = Vec::with_capacity(triangles.len() * 2);
        for (t, tri) in triangles.iter().enumerate() {
            let [a, b, c] = *tri;
            let mid = |p: usize, q: usize| split.get(&(p.min(q), p.max(q))).copied();
            match (mid(a, b), mid(b, c), mid(c, a)) {
                (None, None, None) => {
                    if centroids.binary_search(&t).is_ok() {
                        let m = (
                            (params[a].0 + params[b].0 + params[c].0) / 3.0,
                            (params[a].1 + params[b].1 + params[c].1) / 3.0,
                        );
                        params.push(m);
                        positions.push(point(m.0, m.1));
                        let o = params.len() - 1;
                        next.extend([[a, b, o], [b, c, o], [c, a, o]]);
                    } else {
                        next.push(*tri);
                    }
                }
                (Some(x), None, None) => next.extend([[a, x, c], [x, b, c]]),
                (None, Some(y), None) => next.extend([[a, b, y], [a, y, c]]),
                (None, None, Some(z)) => next.extend([[a, b, z], [z, b, c]]),
                (Some(x), Some(y), None) => next.extend([[a, x, c], [x, y, c], [x, b, y]]),
                (None, Some(y), Some(z)) => next.extend([[a, b, z], [z, b, y], [z, y, c]]),
                (Some(x), None, Some(z)) => next.extend([[a, x, z], [x, b, c], [x, c, z]]),
                (Some(x), Some(y), Some(z)) => {
                    next.extend([[a, x, z], [x, b, y], [z, y, c], [x, y, z]]);
                }
            }
        }
        triangles = next;
    }
    Ok(FaceMesh {
        positions: positions.iter().map(|&p| single(p)).collect(),
        triangles: triangles
            .iter()
            .map(|t| {
                // Counter-clockwise in the parameters faces along the surface's own normal.
                if outward {
                    [t[0] as u32, t[1] as u32, t[2] as u32]
                } else {
                    [t[0] as u32, t[2] as u32, t[1] as u32]
                }
            })
            .collect(),
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
