//! The boundary representation the native kernel builds and answers questions about.
//!
//! Faces lie on planes or cylinders ([`Surface`]) and edges run along lines or circles
//! ([`Curve`]): boxes, extrusions of profiles of lines and arcs, their moved or mirrored
//! copies, and booleans of the straight-sided ones. A closed circle is one edge with one
//! vertex; a whole cylinder's face is cut open along a seam, an edge it runs along once
//! each way, as OpenCASCADE has it.
//!
//! Faces, edges and vertices are numbered solid by solid, in the order an operation made
//! them, so the same operation on the same input gives the same numbering every time.

// Indices into a solid's own vertices, edges and faces, which this crate builds and
// `Solid::check` verifies in the tests; a panic all the same is caught at the `Kernel`
// boundary (lib.rs) and reported as an internal error.
#![allow(clippy::indexing_slicing)]

use oxyde_geom::{Axis, LINEAR_TOLERANCE, Point3, Transform, Vec3};
use oxyde_kernel::{
    CurveKind, EdgeInfo, FaceInfo, SurfaceKind, Topology, TopologyCounts, VertexInfo,
};

use crate::geometry::{AngleHeightSums, Curve, FaceIntegrals, PlaneSums, Surface};

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

/// An edge from vertex `start` to vertex `end` along `curve`; a closed circle starts and
/// ends at the same vertex.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Edge {
    pub start: u32,
    pub end: u32,
    pub curve: Curve,
}

impl Edge {
    pub fn line(start: u32, end: u32) -> Self {
        Self {
            start,
            end,
            curve: Curve::Line,
        }
    }

    pub fn is_closed(&self) -> bool {
        self.start == self.end
    }
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

/// A face: its surface (a plane's normal pointing out of the solid; a cylinder saying
/// which way its normal points) and its boundary loops: the outer one first,
/// counter-clockwise seen from outside, then its holes, clockwise. A whole cylinder's
/// single loop runs along one circle, up its seam, back along the other circle and down
/// the seam.
#[derive(Debug, Clone)]
pub(crate) struct Face {
    pub surface: Surface,
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

    pub fn point(&self, vertex: u32) -> Point3 {
        self.vertices[vertex as usize]
    }

    /// Whether every face is a plane and every edge a line: what the BSP booleans take.
    pub fn is_straight(&self) -> bool {
        self.faces
            .iter()
            .all(|f| matches!(f.surface, Surface::Plane(_)))
            && self.edges.iter().all(|e| e.curve == Curve::Line)
    }

    /// The angle a circle edge turns through, `2π` for a closed one; 0 for a line.
    pub fn sweep(&self, edge: &Edge) -> f64 {
        match edge.curve {
            Curve::Line => 0.0,
            Curve::Circle(circle) => circle.sweep(
                self.point(edge.start),
                self.point(edge.end),
                edge.is_closed(),
            ),
        }
    }

    /// The face's area and mass integrals about `reference` ([`FaceIntegrals`]).
    pub fn face_integrals(&self, face: &Face, reference: Point3) -> Result<FaceIntegrals, String> {
        match &face.surface {
            Surface::Plane(plane) => {
                let (x, n) = (plane.x_dir, plane.normal);
                let y = n.cross(x);
                let local = |p: Point3| {
                    let d = p - plane.origin;
                    (d.dot(x), d.dot(y))
                };
                let mut sums = PlaneSums::default();
                for edge_use in face.loops.iter().flatten() {
                    let edge = &self.edges[edge_use.edge as usize];
                    let (p, q) = (
                        local(self.point(self.start_of(*edge_use))),
                        local(self.point(self.end_of(*edge_use))),
                    );
                    match edge.curve {
                        Curve::Line => sums.add_line(p, q),
                        Curve::Circle(circle) => {
                            let c = local(circle.center);
                            let t0 = (p.1 - c.1).atan2(p.0 - c.0);
                            // Counter-clockwise in the plane's coordinates is about its normal.
                            let mut turn = self.sweep(edge);
                            if circle.normal.dot(n) < 0.0 {
                                turn = -turn;
                            }
                            if edge_use.reversed {
                                turn = -turn;
                            }
                            sums.add_arc(c, circle.radius, t0, t0 + turn);
                        }
                    }
                }
                let (area, mx, my) = sums.area_and_moment();
                let first = plane.origin.to_vec() * area + x * mx + y * my;
                Ok(FaceIntegrals::of_plane(plane, area, first, reference))
            }
            Surface::Cylinder(cylinder) => {
                let mut sums = AngleHeightSums::default();
                let tolerance = 10.0 * LINEAR_TOLERANCE * self.scale().max(1.0);
                for edges in &face.loops {
                    let Some(&first) = edges.first() else {
                        continue;
                    };
                    let (start_angle, mut height) =
                        cylinder.coordinates(self.point(self.start_of(first)));
                    let mut angle = start_angle;
                    for &edge_use in edges {
                        let edge = &self.edges[edge_use.edge as usize];
                        let end = self.point(self.end_of(edge_use));
                        let (end_angle, end_height) = cylinder.coordinates(end);
                        match edge.curve {
                            Curve::Line => {
                                // Along the axis: the angle stays, up to a whole turn.
                                let off = (end_angle - angle).rem_euclid(std::f64::consts::TAU);
                                let off = off.min(std::f64::consts::TAU - off);
                                if off * cylinder.radius > tolerance {
                                    return Err(
                                        "a straight edge on a cylinder is not along its axis"
                                            .to_owned(),
                                    );
                                }
                                sums.add_line(angle, height, end_height);
                                height = end_height;
                            }
                            Curve::Circle(circle) => {
                                if !cylinder.holds_circle(&circle) {
                                    return Err(
                                        "a circle on a cylinder is not across its axis".to_owned()
                                    );
                                }
                                let mut turn = self.sweep(edge);
                                if circle.normal.dot(cylinder.axis.direction) < 0.0 {
                                    turn = -turn;
                                }
                                if edge_use.reversed {
                                    turn = -turn;
                                }
                                angle += turn;
                            }
                        }
                    }
                    if (angle - start_angle).abs() > 1e-6 {
                        return Err("a loop winds round a cylinder".to_owned());
                    }
                }
                Ok(FaceIntegrals::of_cylinder(cylinder, &sums, reference))
            }
        }
    }

    /// Volume and first moment of the solid about `reference`, by the divergence theorem.
    /// Positive for a solid whose faces point outward.
    fn mass_about(&self, reference: Point3) -> Result<(f64, Vec3), String> {
        let mut volume = 0.0;
        let mut moment = Vec3::ZERO;
        for face in &self.faces {
            let integrals = self.face_integrals(face, reference)?;
            volume += integrals.flux / 3.0;
            moment = moment + integrals.flux_moment * 0.25;
        }
        Ok((volume, moment))
    }

    /// Whether the solid is a closed, consistent boundary: every loop closes, every edge is
    /// run along exactly once each way, every face's corners lie on its surface, every
    /// circle edge's ends on its circle, and every normal has unit length. What a result
    /// must be before it is used.
    pub fn check(&self) -> Result<(), String> {
        let vertices = self.vertices.len() as u32;
        if let Some(e) = self
            .edges
            .iter()
            .position(|e| e.start >= vertices || e.end >= vertices)
        {
            return Err(format!("edge {e} ends at a vertex that does not exist"));
        }
        // Corners welded by a boolean may sit up to its welding tolerance off a surface.
        let tolerance = 10.0 * LINEAR_TOLERANCE * self.scale().max(1.0);
        for (e, edge) in self.edges.iter().enumerate() {
            match edge.curve {
                Curve::Line if edge.is_closed() => {
                    return Err(format!("edge {e} is a line that starts where it ends"));
                }
                Curve::Line => {}
                Curve::Circle(circle) => {
                    if (circle.normal.length() - 1.0).abs() > 1e-9 || circle.radius <= 0.0 {
                        return Err(format!(
                            "edge {e}: its circle has no proper normal or radius"
                        ));
                    }
                    for v in [edge.start, edge.end] {
                        let d = self.point(v) - circle.center;
                        let off = d.dot(circle.normal).abs()
                            + ((d - circle.normal * d.dot(circle.normal)).length() - circle.radius)
                                .abs();
                        if off > tolerance {
                            return Err(format!("edge {e}: an end lies {off} mm off its circle"));
                        }
                    }
                }
            }
        }
        let mut uses = vec![(0u32, 0u32); self.edges.len()];
        for (f, face) in self.faces.iter().enumerate() {
            match &face.surface {
                Surface::Plane(plane) => {
                    if (plane.normal.length() - 1.0).abs() > 1e-9 {
                        return Err(format!("face {f}: the normal is not a unit vector"));
                    }
                }
                Surface::Cylinder(cylinder) => {
                    let (x, _, a) = cylinder.frame();
                    if (a.length() - 1.0).abs() > 1e-9
                        || (x.length() - 1.0).abs() > 1e-9
                        || x.dot(a).abs() > 1e-9
                        || cylinder.radius <= 0.0
                    {
                        return Err(format!(
                            "face {f}: its cylinder has no proper frame or radius"
                        ));
                    }
                }
            }
            if face.loops.is_empty() {
                return Err(format!("face {f} has no boundary"));
            }
            for (l, edges) in face.loops.iter().enumerate() {
                if let Some(u) = edges.iter().find(|u| u.edge as usize >= self.edges.len()) {
                    return Err(format!("face {f}, loop {l}: no edge {}", u.edge));
                }
                let single_closed =
                    edges.len() == 1 && self.edges[edges[0].edge as usize].is_closed();
                if edges.len() < 2 && !single_closed {
                    return Err(format!("face {f}, loop {l}: fewer than two edges"));
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
                    let off = face.surface.distance(self.point(self.start_of(a)));
                    if off > tolerance {
                        return Err(format!(
                            "face {f}: a corner lies {off} mm off the face's surface"
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
        // Every face's integrals can be taken (a cylinder's boundary along its axis and
        // across it, no loop winding round it).
        for (f, face) in self.faces.iter().enumerate() {
            self.face_integrals(face, Point3::ORIGIN)
                .map_err(|e| format!("face {f}: {e}"))?;
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
            edges: self
                .edges
                .iter()
                .map(|edge| Edge {
                    start: edge.start,
                    end: edge.end,
                    curve: match edge.curve {
                        Curve::Line => Curve::Line,
                        Curve::Circle(circle) => Curve::Circle(circle.transformed(transform)),
                    },
                })
                .collect(),
            faces: self
                .faces
                .iter()
                .map(|face| Face {
                    surface: face.surface.transformed(transform),
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

    /// Whether every solid is [`Solid::is_straight`].
    pub fn is_straight(&self) -> bool {
        self.solids.iter().all(Solid::is_straight)
    }

    /// Faces, edges and vertices with their adjacency, numbered solid after solid.
    pub fn topology(&self) -> Result<Topology, String> {
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
                let integrals = solid.face_integrals(face, Point3::ORIGIN)?;
                let fallback = solid.vertices.first().copied().unwrap_or(Point3::ORIGIN);
                let (surface, normal, axis) = match &face.surface {
                    Surface::Plane(plane) => (SurfaceKind::Plane, Some(plane.normal), None),
                    Surface::Cylinder(cylinder) => {
                        (SurfaceKind::Cylinder, None, Some(cylinder.axis))
                    }
                };
                topology.faces.push(FaceInfo {
                    surface,
                    area: integrals.area,
                    centroid: integrals.centroid(fallback),
                    normal,
                    axis,
                    edges,
                });
            }
            for (edge, faces) in solid.edges.iter().zip(edge_faces) {
                let (a, b) = (solid.point(edge.start), solid.point(edge.end));
                let vertices = if edge.is_closed() {
                    vec![v0 + edge.start]
                } else {
                    vec![v0 + edge.start, v0 + edge.end]
                };
                topology.edges.push(match edge.curve {
                    Curve::Line => EdgeInfo {
                        curve: CurveKind::Line,
                        length: a.distance(b),
                        midpoint: a + (b - a) * 0.5,
                        axis: Axis::new(a, b - a),
                        vertices,
                        faces,
                    },
                    Curve::Circle(circle) => {
                        let sweep = solid.sweep(edge);
                        EdgeInfo {
                            curve: CurveKind::Circle,
                            length: circle.radius * sweep,
                            midpoint: circle.point_at(circle.angle_of(a) + sweep / 2.0),
                            axis: Axis::new(circle.center, circle.normal),
                            vertices,
                            faces,
                        }
                    }
                });
            }
            topology
                .vertices
                .extend(solid.vertices.iter().map(|&point| VertexInfo { point }));
        }
        Ok(topology)
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

    /// The smallest box along the model axes holding the shape: its vertices and the
    /// furthest points its arcs reach between them, which bound a face of a plane or a
    /// cylinder too (along a cylinder's axis it is straight). `None` for a shape with no
    /// vertex.
    pub fn bounds(&self) -> Option<(Point3, Point3)> {
        let mut points: Vec<Point3> = Vec::new();
        for solid in &self.solids {
            points.extend(solid.vertices.iter().copied());
            for edge in &solid.edges {
                if let Curve::Circle(circle) = edge.curve {
                    let start = circle.angle_of(solid.point(edge.start));
                    points.extend(circle.extremes(start, solid.sweep(edge)));
                }
            }
        }
        let mut points = points.into_iter();
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

    /// The volume of every solid together, and its first moment about the reference point.
    fn mass(&self) -> Result<(Point3, f64, Vec3), String> {
        let Some(reference) = self.reference() else {
            return Ok((Point3::ORIGIN, 0.0, Vec3::ZERO));
        };
        let mut volume = 0.0;
        let mut moment = Vec3::ZERO;
        for solid in &self.solids {
            let (v, m) = solid.mass_about(reference)?;
            volume += v;
            moment = moment + m;
        }
        Ok((reference, volume, moment))
    }

    pub fn volume(&self) -> Result<f64, String> {
        Ok(self.mass()?.1)
    }

    /// The centre of mass of every solid together, of uniform density. `None` when the
    /// shape has no volume.
    pub fn centre_of_mass(&self) -> Result<Option<Point3>, String> {
        let (reference, volume, moment) = self.mass()?;
        Ok((volume.abs() > 0.0).then(|| reference + moment * (1.0 / volume)))
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
