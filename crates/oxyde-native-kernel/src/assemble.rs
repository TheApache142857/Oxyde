//! Faces and solids from the oriented polygons a boolean keeps.
//!
//! The polygons are pieces: triangles of the input faces, split by the other solids'
//! planes. They are welded at their corners and gathered by the plane they lie in (and the
//! way they face), and the pieces of one plane are glued back into faces: every edge two
//! pieces share runs once each way and cancels, and what remains runs round the faces and
//! their holes. Faces of one plane that touch become one face, and an edge that only
//! continues another in a straight line is merged into it, as OpenCASCADE leaves its
//! booleans (`SimplifyResult`), so that both kernels give the same faces and names. The
//! faces are then joined into solids by the edges they share.

// Indices into the welded points, the pieces and the faces, all made here.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;
use std::f64::consts::TAU;

use oxyde_geom::{Plane, Point2, Point3, Vec3};
use oxyde_kernel::{FaceSource, OpHistory};

use crate::NativeError;
use crate::brep::{Edge, EdgeUse, Face, Shape, Solid, cyclic_pairs};
use crate::bsp::{Plane3, Polygon, Source};

/// Points closer than `tolerance` are one vertex, found through a grid of cells.
struct Welder {
    points: Vec<Point3>,
    cells: HashMap<[i64; 3], Vec<u32>>,
    tolerance: f64,
}

impl Welder {
    fn new(tolerance: f64) -> Self {
        Self {
            points: Vec::new(),
            cells: HashMap::new(),
            tolerance,
        }
    }

    fn cell(&self, p: Point3) -> [i64; 3] {
        let size = 2.0 * self.tolerance;
        [
            (p.x / size).floor() as i64,
            (p.y / size).floor() as i64,
            (p.z / size).floor() as i64,
        ]
    }

    /// The vertex at `p`: one already there, within the tolerance, or a new one.
    fn id(&mut self, p: Point3) -> u32 {
        let [x, y, z] = self.cell(p);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(ids) = self.cells.get(&[x + dx, y + dy, z + dz])
                        && let Some(&id) = ids
                            .iter()
                            .find(|&&id| self.points[id as usize].distance(p) <= self.tolerance)
                    {
                        return id;
                    }
                }
            }
        }
        let id = self.points.len() as u32;
        self.points.push(p);
        self.cells.entry([x, y, z]).or_default().push(id);
        id
    }
}

/// A polygon with its corners welded.
struct Piece {
    ids: Vec<u32>,
    plane: Plane3,
    source: Source,
}

/// The pieces lying in one plane and facing one way.
struct Group {
    plane: Plane3,
    pieces: Vec<usize>,
}

/// A face of the result: its outward normal, its loops of vertices (the outer one first,
/// counter-clockwise about the normal, then its holes) and the input faces it is made of.
struct FaceBuild {
    normal: Vec3,
    loops: Vec<Vec<u32>>,
    sources: Vec<Source>,
}

/// The solids bounded by `polygons`, and for each of their faces (numbered solid after
/// solid) the input faces it is made of, in the order of the inputs and their faces.
pub(crate) fn assemble(
    polygons: Vec<Polygon>,
    tolerance: f64,
) -> Result<(Shape, OpHistory), NativeError> {
    let mut welder = Welder::new(tolerance);
    let mut pieces = Vec::with_capacity(polygons.len());
    for polygon in polygons {
        let mut ids: Vec<u32> = Vec::with_capacity(polygon.points.len());
        for &p in &polygon.points {
            let id = welder.id(p);
            if ids.last() != Some(&id) {
                ids.push(id);
            }
        }
        while ids.len() > 1 && ids.first() == ids.last() {
            ids.pop();
        }
        if ids.len() >= 3 {
            pieces.push(Piece {
                ids,
                plane: polygon.plane,
                source: polygon.source,
            });
        }
    }
    let points = welder.points;

    let mut faces = Vec::new();
    for group in group_by_plane(&pieces, &points, tolerance) {
        faces.extend(glue(&group, &pieces, &points, tolerance)?);
    }
    split_at_vertices(&mut faces, &points, tolerance);
    merge_straight_edges(&mut faces, &points, tolerance);
    solids(&faces, &points)
}

fn group_by_plane(pieces: &[Piece], points: &[Point3], tolerance: f64) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (i, piece) in pieces.iter().enumerate() {
        let fits = |group: &Group| {
            group.plane.normal.dot(piece.plane.normal) > 1.0 - 1e-9
                && piece
                    .ids
                    .iter()
                    .all(|&v| group.plane.distance(points[v as usize]).abs() <= tolerance)
        };
        match groups.iter().position(fits) {
            Some(k) => groups[k].pieces.push(i),
            None => groups.push(Group {
                plane: piece.plane,
                pieces: vec![i],
            }),
        }
    }
    groups
}

/// The edge from `a` to `b` cut where any of `corners` lies on it: the pieces from one
/// corner to the next, in order.
fn split_edge(
    a: u32,
    b: u32,
    corners: &[u32],
    points: &[Point3],
    tolerance: f64,
) -> Vec<(u32, u32)> {
    let (pa, pb) = (points[a as usize], points[b as usize]);
    let d = pb - pa;
    let length2 = d.dot(d);
    let mut on: Vec<(f64, u32)> = if length2 > 0.0 {
        corners
            .iter()
            .filter(|&&c| c != a && c != b)
            .filter_map(|&c| {
                let p = points[c as usize];
                let t = (p - pa).dot(d) / length2;
                let off = (p - (pa + d * t)).length();
                (t > 0.0 && t < 1.0 && off <= tolerance).then_some((t, c))
            })
            .collect()
    } else {
        Vec::new()
    };
    on.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut chain = Vec::with_capacity(on.len() + 2);
    chain.push(a);
    chain.extend(on.iter().map(|&(_, c)| c));
    chain.push(b);
    chain.windows(2).map(|w| (w[0], w[1])).collect()
}

/// Where a point is with respect to a polygon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Where {
    Inside,
    Outside,
    On,
}

fn distance_to_segment(p: Point2, a: Point2, b: Point2) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length2 = dx * dx + dy * dy;
    let t = if length2 > 0.0 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / length2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.x - (a.x + dx * t)).hypot(p.y - (a.y + dy * t))
}

fn locate(polygon: &[Point2], p: Point2, tolerance: f64) -> Where {
    let mut inside = false;
    for (a, b) in cyclic_pairs(polygon) {
        if distance_to_segment(p, a, b) <= tolerance {
            return Where::On;
        }
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y);
            if x > p.x {
                inside = !inside;
            }
        }
    }
    if inside {
        Where::Inside
    } else {
        Where::Outside
    }
}

/// The faces the pieces of one plane make.
fn glue(
    group: &Group,
    pieces: &[Piece],
    points: &[Point3],
    tolerance: f64,
) -> Result<Vec<FaceBuild>, NativeError> {
    let internal = |what: &str| NativeError::Internal(what.to_owned());
    let anchor = group
        .pieces
        .first()
        .and_then(|&i| pieces[i].ids.first())
        .map(|&v| points[v as usize])
        .ok_or_else(|| internal("an empty group of faces"))?;
    let frame = Plane::from_point_normal(anchor, group.plane.normal)
        .ok_or_else(|| internal("a face without a normal"))?;
    let (x_dir, y_dir) = (frame.x_dir, frame.y_dir());
    let flat = |v: u32| {
        let d = points[v as usize] - frame.origin;
        Point2::new(d.dot(x_dir), d.dot(y_dir))
    };

    // Every edge of every piece, cut where other corners of the plane lie on it; an edge
    // two pieces share runs once each way, and the two cancel.
    let mut corners: Vec<u32> = group
        .pieces
        .iter()
        .flat_map(|&i| pieces[i].ids.iter().copied())
        .collect();
    corners.sort_unstable();
    corners.dedup();
    let mut edges: Vec<(u32, u32)> = Vec::new();
    let mut alive: Vec<bool> = Vec::new();
    let mut open: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for &i in &group.pieces {
        for (a, b) in cyclic_pairs(&pieces[i].ids) {
            for (u, v) in split_edge(a, b, &corners, points, tolerance) {
                if let Some(k) = open.get_mut(&(v, u)).and_then(Vec::pop) {
                    alive[k] = false;
                } else {
                    open.entry((u, v)).or_default().push(edges.len());
                    edges.push((u, v));
                    alive.push(true);
                }
            }
        }
    }
    let live: Vec<(u32, u32)> = edges
        .iter()
        .zip(&alive)
        .filter(|&(_, &alive)| alive)
        .map(|(&edge, _)| edge)
        .collect();

    // The edges left run round the faces, each with the face on its left. Where several
    // leave one corner (two faces touching there), the next is the first one met turning
    // clockwise from the way back, which keeps each loop round one face.
    let mut leaving: HashMap<u32, Vec<usize>> = HashMap::new();
    for (k, &(a, _)) in live.iter().enumerate() {
        leaving.entry(a).or_default().push(k);
    }
    let direction = |a: u32, b: u32| {
        let (p, q) = (flat(a), flat(b));
        (q.y - p.y).atan2(q.x - p.x)
    };
    let mut used = vec![false; live.len()];
    let mut rings: Vec<Vec<u32>> = Vec::new();
    while let Some(start) = used.iter().position(|u| !u) {
        let first = live[start].0;
        let mut ring = Vec::new();
        let mut at = start;
        loop {
            used[at] = true;
            let (a, b) = live[at];
            ring.push(a);
            if b == first {
                break;
            }
            if ring.len() > live.len() {
                return Err(internal("a face's boundary does not close"));
            }
            let back = direction(b, a);
            let turn = |k: usize| {
                let t = (back - direction(b, live[k].1)).rem_euclid(TAU);
                if t <= 1e-12 { TAU } else { t }
            };
            let next = leaving.get(&b).and_then(|list| {
                list.iter()
                    .copied()
                    .filter(|&k| !used[k])
                    .min_by(|&x, &y| turn(x).total_cmp(&turn(y)))
            });
            match next {
                Some(k) => at = k,
                None => return Err(internal("a face's boundary does not close")),
            }
        }
        rings.push(ring);
    }

    // Loops running counter-clockwise bound faces; the others are holes, each in the
    // smallest face around it.
    let flat_ring = |ring: &[u32]| -> Vec<Point2> { ring.iter().map(|&v| flat(v)).collect() };
    let area = |ring: &[Point2]| {
        cyclic_pairs(ring)
            .map(|(p, q)| p.x * q.y - q.x * p.y)
            .sum::<f64>()
            / 2.0
    };
    let mut outers: Vec<(Vec<u32>, Vec<Point2>, f64)> = Vec::new();
    let mut holes: Vec<(Vec<u32>, Vec<Point2>)> = Vec::new();
    for ring in rings {
        let polygon = flat_ring(&ring);
        let a = area(&polygon);
        // A loop round nothing: two edges that cancel but were cut differently.
        if a.abs() <= tolerance * tolerance {
            continue;
        }
        if a > 0.0 {
            outers.push((ring, polygon, a));
        } else {
            holes.push((ring, polygon));
        }
    }
    let mut faces: Vec<FaceBuild> = outers
        .iter()
        .map(|(ring, _, _)| FaceBuild {
            normal: group.plane.normal,
            loops: vec![ring.clone()],
            sources: Vec::new(),
        })
        .collect();
    let mut hole_polygons: Vec<Vec<Vec<Point2>>> = vec![Vec::new(); outers.len()];
    for (ring, polygon) in holes {
        let inside = |outer: &[Point2]| {
            polygon
                .iter()
                .map(|&p| locate(outer, p, tolerance))
                .find(|&w| w != Where::On)
                .is_none_or(|w| w == Where::Inside)
        };
        let owner = outers
            .iter()
            .enumerate()
            .filter(|(_, (_, outer, _))| inside(outer))
            .min_by(|x, y| x.1.2.total_cmp(&y.1.2))
            .map(|(k, _)| k)
            .ok_or_else(|| internal("a hole of a face lies in no face"))?;
        faces[owner].loops.push(ring);
        hole_polygons[owner].push(polygon);
    }

    // Each piece is part of the face its middle is in.
    for &i in &group.pieces {
        let piece = &pieces[i];
        let n = piece.ids.len() as f64;
        let (sx, sy) = piece.ids.iter().fold((0.0, 0.0), |(x, y), &v| {
            let p = flat(v);
            (x + p.x, y + p.y)
        });
        let middle = Point2::new(sx / n, sy / n);
        let owner = outers.iter().enumerate().position(|(k, (_, outer, _))| {
            locate(outer, middle, tolerance) != Where::Outside
                && hole_polygons[k]
                    .iter()
                    .all(|hole| locate(hole, middle, tolerance) != Where::Inside)
        });
        if let Some(k) = owner {
            faces[k].sources.push(piece.source);
        }
    }
    for face in &mut faces {
        face.sources.sort_unstable();
        face.sources.dedup();
    }
    Ok(faces)
}

/// Cuts every edge of every face where a corner of another face lies on it, so that each
/// edge ends where the faces on either side of it have corners.
fn split_at_vertices(faces: &mut [FaceBuild], points: &[Point3], tolerance: f64) {
    let mut corners: Vec<u32> = faces
        .iter()
        .flat_map(|f| f.loops.iter().flatten().copied())
        .collect();
    corners.sort_unstable();
    corners.dedup();
    for face in faces.iter_mut() {
        for ring in &mut face.loops {
            let split: Vec<u32> = cyclic_pairs(ring.as_slice())
                .flat_map(|(a, b)| split_edge(a, b, &corners, points, tolerance))
                .map(|(u, _)| u)
                .collect();
            *ring = split;
        }
    }
}

/// Whether `v` lies on the segment from `a` to `b`, within `tolerance`.
fn between(a: Point3, v: Point3, b: Point3, tolerance: f64) -> bool {
    let d = b - a;
    let length2 = d.dot(d);
    if length2 <= 0.0 {
        return false;
    }
    let t = (v - a).dot(d) / length2;
    t > 0.0 && t < 1.0 && (v - (a + d * t)).length() <= tolerance
}

/// Removes the corners that only two faces share and that lie in line with their
/// neighbours: the two edges on either side become one.
fn merge_straight_edges(faces: &mut [FaceBuild], points: &[Point3], tolerance: f64) {
    let mut faces_at: HashMap<u32, usize> = HashMap::new();
    for face in faces.iter() {
        let mut corners: Vec<u32> = face.loops.iter().flatten().copied().collect();
        corners.sort_unstable();
        corners.dedup();
        for v in corners {
            *faces_at.entry(v).or_default() += 1;
        }
    }
    let mut straight: HashMap<u32, bool> = HashMap::new();
    for face in faces.iter() {
        for ring in &face.loops {
            let n = ring.len();
            for (k, &v) in ring.iter().enumerate() {
                let (prev, next) = (ring[(k + n - 1) % n], ring[(k + 1) % n]);
                let in_line = between(
                    points[prev as usize],
                    points[v as usize],
                    points[next as usize],
                    tolerance,
                );
                let entry = straight.entry(v).or_insert(true);
                *entry = *entry && in_line;
            }
        }
    }
    let removable =
        |v: &u32| faces_at.get(v) == Some(&2) && straight.get(v).copied().unwrap_or(false);
    for face in faces.iter_mut() {
        for ring in &mut face.loops {
            let kept: Vec<u32> = ring.iter().copied().filter(|v| !removable(v)).collect();
            if kept.len() >= 3 {
                *ring = kept;
            }
        }
    }
}

/// The vertex of `solid` for welded point `v`, added the first time it is met.
fn local_vertex(
    map: &mut HashMap<u32, u32>,
    vertices: &mut Vec<Point3>,
    points: &[Point3],
    v: u32,
) -> u32 {
    *map.entry(v).or_insert_with(|| {
        vertices.push(points[v as usize]);
        vertices.len() as u32 - 1
    })
}

fn root(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// The faces joined by their shared edges into solids, in the order of their first faces.
fn solids(faces: &[FaceBuild], points: &[Point3]) -> Result<(Shape, OpHistory), NativeError> {
    // Edges by their two ends, each run once each way.
    let mut edge_of: HashMap<(u32, u32), usize> = HashMap::new();
    let mut edges: Vec<(u32, u32)> = Vec::new();
    let mut runs: Vec<(u32, u32)> = Vec::new();
    let mut face_uses: Vec<Vec<Vec<(usize, bool)>>> = Vec::with_capacity(faces.len());
    for face in faces {
        let mut loops = Vec::with_capacity(face.loops.len());
        for ring in &face.loops {
            let mut uses = Vec::with_capacity(ring.len());
            for (a, b) in cyclic_pairs(ring) {
                let e = *edge_of.entry((a.min(b), a.max(b))).or_insert_with(|| {
                    edges.push((a, b));
                    runs.push((0, 0));
                    edges.len() - 1
                });
                let reversed = edges[e].0 != a;
                if reversed {
                    runs[e].1 += 1;
                } else {
                    runs[e].0 += 1;
                }
                uses.push((e, reversed));
            }
            loops.push(uses);
        }
        face_uses.push(loops);
    }
    if let Some(&(forward, backward)) = runs.iter().find(|&&r| r != (1, 1)) {
        return Err(if forward + backward > 2 {
            NativeError::Unsupported("join solids that would touch along an edge")
        } else {
            NativeError::Internal("the result is not closed along an edge".to_owned())
        });
    }

    // Faces sharing an edge are in one solid.
    let mut parent: Vec<usize> = (0..faces.len()).collect();
    let mut first_face: Vec<Option<usize>> = vec![None; edges.len()];
    for (f, loops) in face_uses.iter().enumerate() {
        for &(e, _) in loops.iter().flatten() {
            match first_face[e] {
                None => first_face[e] = Some(f),
                Some(other) => {
                    let (x, y) = (root(&mut parent, f), root(&mut parent, other));
                    if x != y {
                        parent[x.max(y)] = x.min(y);
                    }
                }
            }
        }
    }
    let mut component_of: HashMap<usize, usize> = HashMap::new();
    let mut members: Vec<Vec<usize>> = Vec::new();
    for f in 0..faces.len() {
        let r = root(&mut parent, f);
        let c = *component_of.entry(r).or_insert_with(|| {
            members.push(Vec::new());
            members.len() - 1
        });
        members[c].push(f);
    }

    let mut shape = Shape::default();
    let mut history = OpHistory::default();
    for member_faces in &members {
        let mut solid = Solid::default();
        let mut vertex_map: HashMap<u32, u32> = HashMap::new();
        let mut edge_map: HashMap<usize, u32> = HashMap::new();
        let mut sources: Vec<Vec<FaceSource>> = Vec::with_capacity(member_faces.len());
        for &f in member_faces {
            let face = &faces[f];
            let mut loops = Vec::with_capacity(face.loops.len());
            for uses in &face_uses[f] {
                let mut local_uses = Vec::with_capacity(uses.len());
                for &(e, reversed) in uses {
                    let edge = match edge_map.get(&e) {
                        Some(&local) => local,
                        None => {
                            let (a, b) = edges[e];
                            let start =
                                local_vertex(&mut vertex_map, &mut solid.vertices, points, a);
                            let end = local_vertex(&mut vertex_map, &mut solid.vertices, points, b);
                            solid.edges.push(Edge { start, end });
                            let local = solid.edges.len() as u32 - 1;
                            edge_map.insert(e, local);
                            local
                        }
                    };
                    local_uses.push(EdgeUse { edge, reversed });
                }
                loops.push(local_uses);
            }
            let anchor = face
                .loops
                .first()
                .and_then(|ring| ring.first())
                .map(|&v| points[v as usize])
                .ok_or_else(|| NativeError::Internal("a face without corners".to_owned()))?;
            let plane = Plane::from_point_normal(anchor, face.normal)
                .ok_or_else(|| NativeError::Internal("a face without a normal".to_owned()))?;
            solid.faces.push(Face { plane, loops });
            sources.push(
                face.sources
                    .iter()
                    .map(|&(input, index)| FaceSource::InputFace { input, face: index })
                    .collect(),
            );
        }
        let piece = Shape {
            solids: vec![solid],
        };
        // A shell facing inwards bounds a void inside another solid.
        if piece.volume() < 0.0 {
            return Err(NativeError::Unsupported("leave a void inside a solid"));
        }
        shape.solids.extend(piece.solids);
        history.faces.extend(sources);
    }
    Ok((shape, history))
}
