//! The boundary representation the native kernel builds and answers questions about.
//!
//! For now every face is planar and every edge straight: boxes, extrusions of profiles made
//! of lines, and their moved or mirrored copies. Curved surfaces and edges come with circles
//! and arcs (docs/ARCHITECTURE.md §10, Native kernel, step 4).
//!
//! Faces, edges and vertices are numbered solid by solid, in the order an operation made
//! them, so the same operation on the same input gives the same numbering every time.

// Indices into a solid's own vertices, edges and faces, which this crate builds and
// `Solid::check` verifies in the tests; a panic all the same is caught at the `Kernel`
// boundary (lib.rs) and reported as an internal error.
#![allow(clippy::indexing_slicing)]

use oxyde_geom::{Axis, LINEAR_TOLERANCE, Plane, Point3, Transform, Vec3};
use oxyde_kernel::{
    CurveKind, EdgeInfo, FaceInfo, SurfaceKind, Topology, TopologyCounts, VertexInfo,
};

/// What an operation makes: its solids, in order. Several solids (the regions of one
/// profile, extruded) make one shape, as a compound does in OpenCASCADE.
#[derive(Debug, Clone, Default)]
pub(crate) struct Shape {
    pub solids: Vec<Solid>,
}

/// One closed, connected solid, bounded by a single shell (solids with voids come later).
#[derive(Debug, Clone, Default)]
pub(crate) struct Solid {
    pub vertices: Vec<Point3>,
    pub edges: Vec<Edge>,
    pub faces: Vec<Face>,
}

/// A straight edge from vertex `start` to vertex `end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Edge {
    pub start: u32,
    pub end: u32,
}

/// An edge as a loop runs along it: its own way, or `reversed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EdgeUse {
    pub edge: u32,
    pub reversed: bool,
}

impl EdgeUse {
    pub fn forward(edge: u32) -> Self {
        Self {
            edge,
            reversed: false,
        }
    }

    pub fn backward(edge: u32) -> Self {
        Self {
            edge,
            reversed: true,
        }
    }

    fn flipped(self) -> Self {
        Self {
            edge: self.edge,
            reversed: !self.reversed,
        }
    }
}

/// A planar face: its plane, with the normal pointing out of the solid, and its boundary
/// loops: the outer one first, counter-clockwise seen from outside (looking against the
/// normal), then its holes, clockwise.
#[derive(Debug, Clone)]
pub(crate) struct Face {
    pub plane: Plane,
    pub loops: Vec<Vec<EdgeUse>>,
}

impl Solid {
    /// The vertex a loop leaves from when it runs along `edge_use`.
    pub fn start_of(&self, edge_use: EdgeUse) -> u32 {
        let edge = self.edges[edge_use.edge as usize];
        if edge_use.reversed {
            edge.end
        } else {
            edge.start
        }
    }

    /// The vertex a loop arrives at when it runs along `edge_use`.
    pub fn end_of(&self, edge_use: EdgeUse) -> u32 {
        self.start_of(edge_use.flipped())
    }

    fn point(&self, vertex: u32) -> Point3 {
        self.vertices[vertex as usize]
    }

    /// The corners of a loop, in its order.
    pub fn loop_points(&self, edges: &[EdgeUse]) -> Vec<Point3> {
        edges
            .iter()
            .map(|&u| self.point(self.start_of(u)))
            .collect()
    }

    /// Area and centroid of a face: the sum of the signed triangles that fan out from one of
    /// its corners over every loop, so holes count negatively. Exact for a planar face.
    fn face_area_centroid(&self, face: &Face) -> (f64, Point3) {
        let normal = face.plane.normal;
        let Some(apex) = face
            .loops
            .first()
            .and_then(|l| l.first())
            .map(|&u| self.point(self.start_of(u)))
        else {
            return (0.0, face.plane.origin);
        };
        let mut area = 0.0;
        let mut moment = Vec3::ZERO;
        for points in face.loops.iter().map(|l| self.loop_points(l)) {
            for (a, b) in cyclic_pairs(&points) {
                let signed = (a - apex).cross(b - apex).dot(normal) / 2.0;
                area += signed;
                moment = moment + (apex.to_vec() + a.to_vec() + b.to_vec()) * (signed / 3.0);
            }
        }
        if area.abs() > 0.0 {
            let c = moment * (1.0 / area);
            (area, Point3::new(c.x, c.y, c.z))
        } else {
            (area, apex)
        }
    }

    /// Volume and first moment of the solid about `reference`: the sum of the signed
    /// tetrahedra from `reference` to the triangles that fan out over each face's loops
    /// (the divergence theorem). Positive for a solid whose faces point outward.
    fn mass_about(&self, reference: Point3) -> (f64, Vec3) {
        let mut volume = 0.0;
        let mut moment = Vec3::ZERO;
        for face in &self.faces {
            let Some(apex) = face
                .loops
                .first()
                .and_then(|l| l.first())
                .map(|&u| self.point(self.start_of(u)))
            else {
                continue;
            };
            let c = apex - reference;
            for points in face.loops.iter().map(|l| self.loop_points(l)) {
                for (a, b) in cyclic_pairs(&points) {
                    let (a, b) = (a - reference, b - reference);
                    let tet = c.dot(a.cross(b)) / 6.0;
                    volume += tet;
                    // The tetrahedron's centroid, from the reference: (0 + c + a + b) / 4.
                    moment = moment + (c + a + b) * (tet / 4.0);
                }
            }
        }
        (volume, moment)
    }

    /// Whether the solid is a closed, consistent boundary: every loop closes, every edge is
    /// run along exactly once each way, every face's corners lie on its plane and every
    /// normal has unit length. What a result must be before it is used.
    pub fn check(&self) -> Result<(), String> {
        let vertices = self.vertices.len() as u32;
        if let Some(e) = self
            .edges
            .iter()
            .position(|e| e.start >= vertices || e.end >= vertices)
        {
            return Err(format!("edge {e} ends at a vertex that does not exist"));
        }
        let tolerance = LINEAR_TOLERANCE * self.scale().max(1.0);
        let mut uses = vec![(0u32, 0u32); self.edges.len()];
        for (f, face) in self.faces.iter().enumerate() {
            if (face.plane.normal.length() - 1.0).abs() > 1e-9 {
                return Err(format!("face {f}: the normal is not a unit vector"));
            }
            if face.loops.is_empty() {
                return Err(format!("face {f} has no boundary"));
            }
            for (l, edges) in face.loops.iter().enumerate() {
                if edges.len() < 2 {
                    return Err(format!("face {f}, loop {l}: fewer than two edges"));
                }
                if let Some(u) = edges.iter().find(|u| u.edge as usize >= self.edges.len()) {
                    return Err(format!("face {f}, loop {l}: no edge {}", u.edge));
                }
                for (k, (a, b)) in cyclic_pairs(edges).enumerate() {
                    if self.end_of(a) != self.start_of(b) {
                        return Err(format!("face {f}, loop {l} is open after its edge {k}"));
                    }
                    let count = &mut uses[a.edge as usize];
                    if a.reversed {
                        count.1 += 1;
                    } else {
                        count.0 += 1;
                    }
                    let off = (self.point(self.start_of(a)) - face.plane.origin)
                        .dot(face.plane.normal)
                        .abs();
                    if off > tolerance {
                        return Err(format!(
                            "face {f}: a corner lies {off} mm off the face's plane"
                        ));
                    }
                }
            }
        }
        for (e, count) in uses.iter().enumerate() {
            if *count != (1, 1) {
                return Err(format!(
                    "edge {e} is run along {} times its own way and {} times the other",
                    count.0, count.1
                ));
            }
        }
        let mut used = vec![false; self.vertices.len()];
        for edge in &self.edges {
            used[edge.start as usize] = true;
            used[edge.end as usize] = true;
        }
        if let Some(v) = used.iter().position(|u| !u) {
            return Err(format!("vertex {v} is on no edge"));
        }
        Ok(())
    }

    /// The largest coordinate in absolute value: the size tolerances are scaled by.
    fn scale(&self) -> f64 {
        self.vertices
            .iter()
            .map(|p| p.x.abs().max(p.y.abs()).max(p.z.abs()))
            .fold(0.0, f64::max)
    }

    /// The solid moved by `transform`. A mirror turns every loop around, so that the
    /// faces still run counter-clockwise about their (mirrored, still outward) normals.
    fn transformed(&self, transform: &Transform) -> Solid {
        let mirror = transform.is_mirror();
        Solid {
            vertices: self
                .vertices
                .iter()
                .map(|&p| transform.apply_point(p))
                .collect(),
            edges: self.edges.clone(),
            faces: self
                .faces
                .iter()
                .map(|face| Face {
                    plane: Plane {
                        origin: transform.apply_point(face.plane.origin),
                        x_dir: transform.apply_vector(face.plane.x_dir),
                        normal: transform.apply_vector(face.plane.normal),
                    },
                    loops: face
                        .loops
                        .iter()
                        .map(|edges| {
                            if mirror {
                                edges.iter().rev().map(|u| u.flipped()).collect()
                            } else {
                                edges.clone()
                            }
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

impl Shape {
    pub fn face_count(&self) -> usize {
        self.solids.iter().map(|s| s.faces.len()).sum()
    }

    /// Faces, edges and vertices with their adjacency, numbered solid after solid.
    pub fn topology(&self) -> Topology {
        let mut topology = Topology::default();
        for solid in &self.solids {
            let f0 = topology.faces.len() as u32;
            let e0 = topology.edges.len() as u32;
            let v0 = topology.vertices.len() as u32;
            let mut edge_faces: Vec<Vec<u32>> = vec![Vec::new(); solid.edges.len()];
            for (f, face) in solid.faces.iter().enumerate() {
                let f = f0 + f as u32;
                let mut edges = Vec::new();
                for edge_use in face.loops.iter().flatten() {
                    let e = e0 + edge_use.edge;
                    if !edges.contains(&e) {
                        edges.push(e);
                    }
                    let faces = &mut edge_faces[edge_use.edge as usize];
                    if !faces.contains(&f) {
                        faces.push(f);
                    }
                }
                let (area, centroid) = solid.face_area_centroid(face);
                topology.faces.push(FaceInfo {
                    surface: SurfaceKind::Plane,
                    area,
                    centroid,
                    normal: Some(face.plane.normal),
                    axis: None,
                    edges,
                });
            }
            for (edge, faces) in solid.edges.iter().zip(edge_faces) {
                let (a, b) = (solid.point(edge.start), solid.point(edge.end));
                topology.edges.push(EdgeInfo {
                    curve: CurveKind::Line,
                    length: a.distance(b),
                    midpoint: a + (b - a) * 0.5,
                    axis: Axis::new(a, b - a),
                    vertices: vec![v0 + edge.start, v0 + edge.end],
                    faces,
                });
            }
            topology
                .vertices
                .extend(solid.vertices.iter().map(|&point| VertexInfo { point }));
        }
        topology
    }

    pub fn counts(&self) -> TopologyCounts {
        TopologyCounts {
            solids: self.solids.len(),
            shells: self.solids.len(),
            faces: self.face_count(),
            edges: self.solids.iter().map(|s| s.edges.len()).sum(),
            vertices: self.solids.iter().map(|s| s.vertices.len()).sum(),
        }
    }

    /// The smallest box along the model axes holding every vertex: exact, since every edge
    /// is straight. `None` for a shape with no vertex.
    pub fn bounds(&self) -> Option<(Point3, Point3)> {
        let mut points = self.solids.iter().flat_map(|s| s.vertices.iter().copied());
        let first = points.next()?;
        Some(points.fold((first, first), |(lo, hi), p| {
            (
                Point3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)),
                Point3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)),
            )
        }))
    }

    /// A point of the shape the mass integrals are taken about: near the geometry, so that
    /// large coordinates do not eat into their precision.
    fn reference(&self) -> Option<Point3> {
        self.solids.iter().find_map(|s| s.vertices.first().copied())
    }

    pub fn volume(&self) -> f64 {
        let Some(reference) = self.reference() else {
            return 0.0;
        };
        self.solids.iter().map(|s| s.mass_about(reference).0).sum()
    }

    /// The centre of mass of every solid together, of uniform density. `None` when the
    /// shape has no volume.
    pub fn centre_of_mass(&self) -> Option<Point3> {
        let reference = self.reference()?;
        let (volume, moment) = self
            .solids
            .iter()
            .map(|s| s.mass_about(reference))
            .fold((0.0, Vec3::ZERO), |(v, m), (sv, sm)| (v + sv, m + sm));
        (volume.abs() > 0.0).then(|| reference + moment * (1.0 / volume))
    }

    pub fn transformed(&self, transform: &Transform) -> Shape {
        Shape {
            solids: self
                .solids
                .iter()
                .map(|s| s.transformed(transform))
                .collect(),
        }
    }

    /// [`Solid::check`] for every solid, the first problem with the solid's index.
    pub fn check(&self) -> Result<(), String> {
        for (i, solid) in self.solids.iter().enumerate() {
            solid.check().map_err(|e| format!("solid {i}: {e}"))?;
        }
        Ok(())
    }
}

/// Each item with the next one, the last with the first.
pub(crate) fn cyclic_pairs<T: Copy>(items: &[T]) -> impl Iterator<Item = (T, T)> + '_ {
    items
        .iter()
        .zip(items.iter().cycle().skip(1))
        .map(|(&a, &b)| (a, b))
}
