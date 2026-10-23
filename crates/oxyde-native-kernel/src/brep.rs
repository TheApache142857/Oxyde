//! The boundary representation the native kernel builds and answers questions about.
//!
//! Faces lie on planes or on surfaces of revolution (cylinders, cones, spheres, tori:
//! [`Surface`]) and edges run along lines or circles ([`Curve`]), or stand at a point where
//! a surface of revolution meets its axis (a degenerate edge, at a cone's apex or a
//! sphere's pole). A closed circle is one edge with one vertex; a face that goes round its
//! axis whole is cut open along a seam, an edge it runs along once each way, as OpenCASCADE
//! has it.
//!
//! Every vertex and every edge carries a tolerance, as OpenCASCADE's do: how far the
//! geometry meeting there may be from the vertex's point or the edge's curve. A result's
//! tolerances are measured ([`Solid::settle_tolerances`]): the largest distance between a
//! vertex and the surfaces and curves meeting at it, between an edge and the surfaces of its
//! faces, never less than [`BASE_TOLERANCE`].
//!
//! A solid is bounded by its outer shell and the shells of the voids inside it.
//!
//! Faces, edges and vertices are numbered solid by solid, in the order an operation made
//! them, so the same operation on the same input gives the same numbering every time.

// Indices into a solid's own vertices, edges and faces, which this crate builds and
// `Solid::check` verifies in the tests; a panic all the same is caught at the `Kernel`
// boundary (lib.rs) and reported as an internal error.
#![allow(clippy::indexing_slicing)]

use std::f64::consts::PI;

use oxyde_geom::{Axis, LINEAR_TOLERANCE, Point3, Transform, Vec3};
use oxyde_kernel::{
    CurveKind, EdgeInfo, FaceInfo, SurfaceKind, Topology, TopologyCounts, VertexInfo,
};

use crate::geometry::{Curve, FaceIntegrals, PlaneSums, Surface};
use crate::revolution::{Meridian, ParamPath, Revolution, RevolutionSums, WalkEdge, walk};

/// The least tolerance of a vertex or an edge: what exact constructions are held to.
pub(crate) const BASE_TOLERANCE: f64 = LINEAR_TOLERANCE;

/// The largest tolerance a result may need: geometry further apart than this was not meant
/// to meet, and the operation that made it is refused as a bug.
pub(crate) const MAX_TOLERANCE: f64 = 1e-3;

/// What an operation makes: its solids, in order. Several solids (the regions of one
/// profile, extruded) make one shape, as a compound does in OpenCASCADE.
#[derive(Debug, Clone, Default)]
pub(crate) struct Shape {
    pub solids: Vec<Solid>,
}

/// One solid: its vertices, edges and faces, and its shells.
#[derive(Debug, Clone, Default)]
pub(crate) struct Solid {
    pub vertices: Vec<Vertex>,
    pub edges: Vec<Edge>,
    pub faces: Vec<Face>,
    /// The faces of each shell, the outer one first, then each void's (a closed shell whose
    /// normals point into the void). Empty for a solid of one shell, every face in it.
    pub shells: Vec<Vec<u32>>,
}

/// A vertex: its point and its tolerance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Vertex {
    pub point: Point3,
    pub tolerance: f64,
}

impl Vertex {
    pub fn new(point: Point3) -> Self {
        Self {
            point,
            tolerance: BASE_TOLERANCE,
        }
    }
}

/// An edge from vertex `start` to vertex `end` along `curve`; a closed circle and a
/// degenerate edge start and end at the same vertex.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Edge {
    pub start: u32,
    pub end: u32,
    pub curve: Curve,
    pub tolerance: f64,
}

impl Edge {
    pub fn new(start: u32, end: u32, curve: Curve) -> Self {
        Self {
            start,
            end,
            curve,
            tolerance: BASE_TOLERANCE,
        }
    }

    pub fn line(start: u32, end: u32) -> Self {
        Self::new(start, end, Curve::Line)
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

    pub fn flipped(self) -> Self {
        Self {
            edge: self.edge,
            reversed: !self.reversed,
        }
    }
}

/// A face: its surface (its normal pointing out of the solid) and its boundary loops: the
/// outer one first, counter-clockwise seen from outside, then its holes, clockwise. A face
/// that goes round its axis whole has a single loop: along one circle, up its seam, back
/// along the other circle (or a degenerate edge at a pole or an apex) and down the seam.
#[derive(Debug, Clone)]
pub(crate) struct Face {
    pub surface: Surface,
    pub loops: Vec<Vec<EdgeUse>>,
}

/// One edge use of a face on a surface of revolution, laid out in the surface's `(θ, v)`
/// plane: from `from` to `to`, `θ` unwrapped as the loop goes round. What the kernel builds
/// so far meets its surfaces of revolution in lines of constant `θ` (meridians, a cone's or a
/// cylinder's straight lines) and lines of constant `v` (parallels, poles), so each use is a
/// straight segment there.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ParamUse {
    pub edge_use: EdgeUse,
    pub from: (f64, f64),
    pub to: (f64, f64),
    /// Whether it runs straight from `from` to `to` (along a line of constant `θ` or `v`)
    /// rather than bending there.
    pub straight: bool,
}

impl ParamUse {
    /// Whether it runs along a line of constant `θ`.
    pub fn is_vertical(&self) -> bool {
        self.straight && (self.to.0 - self.from.0).abs() <= 1e-12
    }

    /// Whether it runs along a line of constant `v`.
    pub fn is_horizontal(&self) -> bool {
        self.straight && (self.to.1 - self.from.1).abs() <= 1e-12
    }
}

/// Points this close to a surface's axis have no angle about it.
const NEAR_AXIS: f64 = 10.0 * BASE_TOLERANCE;

impl Solid {
    /// A solid of one shell.
    pub fn closed(vertices: Vec<Vertex>, edges: Vec<Edge>, faces: Vec<Face>) -> Self {
        Self {
            vertices,
            edges,
            faces,
            shells: Vec::new(),
        }
    }

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
        self.vertices[vertex as usize].point
    }

    /// Whether every face is a plane and every edge a line: what the BSP booleans take.
    pub fn is_straight(&self) -> bool {
        self.faces
            .iter()
            .all(|f| matches!(f.surface, Surface::Plane(_)))
            && self.edges.iter().all(|e| e.curve == Curve::Line)
            && self.shells.len() <= 1
    }

    /// The angle a circle's or a section's edge turns through, `2π` for a closed one; a
    /// degenerate edge's turn; 0 for a line.
    pub fn sweep(&self, edge: &Edge) -> f64 {
        match edge.curve {
            Curve::Line => 0.0,
            curve => curve.extent(
                self.point(edge.start),
                self.point(edge.end),
                edge.is_closed(),
            ),
        }
    }

    /// How far an edge runs: its length for a line, its sweep otherwise.
    fn extent(&self, edge: &Edge) -> f64 {
        edge.curve.extent(
            self.point(edge.start),
            self.point(edge.end),
            edge.is_closed(),
        )
    }

    /// The point `s` along an edge (as [`Curve::extent`] measures it).
    pub fn edge_point(&self, edge: &Edge, s: f64) -> Point3 {
        let (a, b) = (self.point(edge.start), self.point(edge.end));
        edge.curve.at(a, b, s).unwrap_or(a)
    }

    /// The point halfway along an edge.
    pub fn midpoint(&self, edge: &Edge) -> Point3 {
        self.edge_point(edge, self.extent(edge) / 2.0)
    }

    /// How far `p` lies off an edge's curve (its point, for a degenerate edge).
    fn off_curve(&self, edge: &Edge, p: Point3) -> f64 {
        edge.curve
            .distance(self.point(edge.start), self.point(edge.end), p)
    }

    /// An edge use as the walk round a face of revolution takes it.
    pub fn walk_edge(&self, u: EdgeUse) -> WalkEdge {
        let edge = &self.edges[u.edge as usize];
        WalkEdge {
            curve: if u.reversed {
                edge.curve.reversed()
            } else {
                edge.curve
            },
            a: self.point(self.start_of(u)),
            b: self.point(self.end_of(u)),
            sweep: self.sweep(edge),
            mid: self.midpoint(edge),
            tolerance: edge.tolerance.max(BASE_TOLERANCE),
        }
    }

    /// A loop of a face on a surface of revolution laid out in the surface's `(θ, v)` plane
    /// ([`crate::revolution::walk`]), each use a segment from where the previous one ended.
    pub fn param_loop(
        &self,
        uses: &[EdgeUse],
        surface: &Revolution,
    ) -> Result<Vec<ParamUse>, String> {
        let edges: Vec<WalkEdge> = uses.iter().map(|&u| self.walk_edge(u)).collect();
        let walked = walk(surface, &edges, NEAR_AXIS)?;
        Ok(uses
            .iter()
            .zip(walked)
            .map(|(&edge_use, w)| ParamUse {
                edge_use,
                from: w.from,
                to: w.to,
                straight: w.straight,
            })
            .collect())
    }

    /// Every loop of a face on a surface of revolution, laid out in its `(θ, v)` plane, each
    /// after the first (the holes) moved by whole turns to lie about the first.
    pub fn param_loops(
        &self,
        face: &Face,
        surface: &Revolution,
    ) -> Result<Vec<Vec<ParamUse>>, String> {
        let tau = std::f64::consts::TAU;
        let middle = |uses: &[ParamUse]| {
            let n = uses.len().max(1) as f64;
            let t = uses.iter().map(|u| u.from.0).sum::<f64>() / n;
            let v = uses.iter().map(|u| u.from.1).sum::<f64>() / n;
            (t, v)
        };
        let mut out: Vec<Vec<ParamUse>> = Vec::with_capacity(face.loops.len());
        for uses in &face.loops {
            let mut walked = self.param_loop(uses, surface)?;
            if let Some(outer) = out.first() {
                let (ot, ov) = middle(outer);
                let (t, v) = middle(&walked);
                let dt = tau * ((ot - t) / tau).round();
                let dv = if surface.v_periodic() {
                    tau * ((ov - v) / tau).round()
                } else {
                    0.0
                };
                for u in &mut walked {
                    u.from = (u.from.0 + dt, u.from.1 + dv);
                    u.to = (u.to.0 + dt, u.to.1 + dv);
                }
            }
            out.push(walked);
        }
        Ok(out)
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
                        Curve::Degenerate { .. } => {}
                        Curve::Section(_) => {
                            let w = self.walk_edge(*edge_use);
                            sums.add_path(w.sweep, &|s| {
                                local(w.curve.at(w.a, w.b, s).unwrap_or(w.a))
                            });
                        }
                    }
                }
                let (area, mx, my) = sums.area_and_moment();
                let first = plane.origin.to_vec() * area + x * mx + y * my;
                Ok(FaceIntegrals::of_plane(plane, area, first, reference))
            }
            Surface::Revolution(surface) => {
                let mut sums = RevolutionSums::new(surface, reference);
                for uses in self.param_loops(face, surface)? {
                    for u in uses {
                        if !u.straight {
                            let w = self.walk_edge(u.edge_use);
                            let path = ParamPath::follow(surface, &w, u.from, NEAR_AXIS)?;
                            sums.add_path(surface, &w, &path);
                        } else if u.is_vertical() {
                            sums.add_line(surface, u.from.0, u.from.1, u.to.1);
                        } else if !u.is_horizontal() {
                            return Err(
                                "an edge across a surface of revolution that is neither a meridian nor a parallel"
                                    .to_owned(),
                            );
                        }
                    }
                }
                Ok(sums.integrals(surface))
            }
        }
    }

    /// Volume and first moment of the solid about `reference`, by the divergence theorem.
    /// Positive for a solid whose faces point outward; a void's shell counts negative.
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

    /// The vertices and edges of a solid given the tolerances they need: for a vertex, the
    /// furthest the surfaces of the faces round it and the curves of the edges at it are
    /// from its point; for an edge, the furthest the surfaces of its faces are from its
    /// curve, at its ends and a few points between. Never less than [`BASE_TOLERANCE`];
    /// more than [`MAX_TOLERANCE`] is an error, a sign of geometry that does not meet.
    pub fn settle_tolerances(&mut self) -> Result<(), String> {
        let mut vertex_tolerances = vec![BASE_TOLERANCE; self.vertices.len()];
        let mut edge_tolerances = vec![BASE_TOLERANCE; self.edges.len()];
        for face in &self.faces {
            for &u in face.loops.iter().flatten() {
                let edge = &self.edges[u.edge as usize];
                for v in [edge.start, edge.end] {
                    let off = face.surface.distance(self.point(v));
                    let t = &mut vertex_tolerances[v as usize];
                    *t = t.max(off);
                }
                let extent = self.extent(edge);
                let samples: Vec<Point3> = (0..=8)
                    .map(|i| self.edge_point(edge, extent * f64::from(i) / 8.0))
                    .collect();
                let off = samples
                    .iter()
                    .map(|&p| face.surface.distance(p))
                    .fold(0.0, f64::max);
                let t = &mut edge_tolerances[u.edge as usize];
                *t = t.max(off);
            }
        }
        for edge in &self.edges {
            for v in [edge.start, edge.end] {
                let off = self.off_curve(edge, self.point(v));
                let t = &mut vertex_tolerances[v as usize];
                *t = t.max(off);
            }
        }
        if let Some((v, t)) = vertex_tolerances
            .iter()
            .enumerate()
            .find(|(_, t)| **t > MAX_TOLERANCE)
        {
            return Err(format!("vertex {v} lies {t} mm off what meets there"));
        }
        if let Some((e, t)) = edge_tolerances
            .iter()
            .enumerate()
            .find(|(_, t)| **t > MAX_TOLERANCE)
        {
            return Err(format!("edge {e} lies {t} mm off its faces"));
        }
        for (vertex, t) in self.vertices.iter_mut().zip(vertex_tolerances) {
            vertex.tolerance = t;
        }
        for (edge, t) in self.edges.iter_mut().zip(edge_tolerances) {
            edge.tolerance = t;
        }
        Ok(())
    }

    /// Whether the solid is a closed, consistent boundary: every loop closes, every edge is
    /// run along exactly once each way (a degenerate edge once), every face's corners lie
    /// on its surface and every edge's ends on its curve within their tolerances, every
    /// normal has unit length, every shell is closed on its own. What a result must be
    /// before it is used.
    pub fn check(&self) -> Result<(), String> {
        let vertices = self.vertices.len() as u32;
        if let Some(e) = self
            .edges
            .iter()
            .position(|e| e.start >= vertices || e.end >= vertices)
        {
            return Err(format!("edge {e} ends at a vertex that does not exist"));
        }
        // Rounding in coordinates this large.
        let slack = 1e-9 * self.scale().max(1.0);
        let within = |v: u32, extra: f64| self.vertices[v as usize].tolerance + extra + slack;
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
                        let off = circle.distance(self.point(v));
                        if off > within(v, edge.tolerance) {
                            return Err(format!("edge {e}: an end lies {off} mm off its circle"));
                        }
                    }
                }
                Curve::Section(section) => {
                    for v in [edge.start, edge.end] {
                        let off = section.distance(self.point(v));
                        if off > within(v, edge.tolerance) {
                            return Err(format!("edge {e}: an end lies {off} mm off its curve"));
                        }
                    }
                }
                Curve::Degenerate { axis, turn } => {
                    if !edge.is_closed() {
                        return Err(format!("edge {e} is degenerate but has two ends"));
                    }
                    if (axis.length() - 1.0).abs() > 1e-9
                        || !(turn > 0.0 && turn <= std::f64::consts::TAU + 1e-9)
                    {
                        return Err(format!("edge {e}: a degenerate edge with no proper turn"));
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
                Surface::Revolution(surface) => {
                    let (x, _, a) = surface.frame();
                    let proper = match surface.meridian {
                        Meridian::Line { radius, slope } => {
                            radius.is_finite()
                                && slope.is_finite()
                                && (slope != 0.0 || radius > 0.0)
                        }
                        Meridian::Circle { center, radius } => center >= 0.0 && radius > 0.0,
                    };
                    if (a.length() - 1.0).abs() > 1e-9
                        || (x.length() - 1.0).abs() > 1e-9
                        || x.dot(a).abs() > 1e-9
                        || !proper
                    {
                        return Err(format!(
                            "face {f}: its surface of revolution has no proper frame or meridian"
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
                    let v = self.start_of(a);
                    let off = face.surface.distance(self.point(v));
                    if off > within(v, 0.0) {
                        return Err(format!(
                            "face {f}: a corner lies {off} mm off the face's surface"
                        ));
                    }
                }
            }
        }
        for (e, count) in uses.iter().enumerate() {
            let once = if self.edges[e].curve.is_degenerate() {
                count.0 + count.1 == 1
            } else {
                *count == (1, 1)
            };
            if !once {
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
        // Every face in one shell, and no edge between two shells.
        if !self.shells.is_empty() {
            let mut shell_of = vec![usize::MAX; self.faces.len()];
            for (s, faces) in self.shells.iter().enumerate() {
                for &f in faces {
                    let Some(slot) = shell_of.get_mut(f as usize) else {
                        return Err(format!("shell {s} holds a face that does not exist"));
                    };
                    if *slot != usize::MAX {
                        return Err(format!("face {f} is in two shells"));
                    }
                    *slot = s;
                }
            }
            if let Some(f) = shell_of.iter().position(|&s| s == usize::MAX) {
                return Err(format!("face {f} is in no shell"));
            }
            let mut shell_of_edge: Vec<Option<usize>> = vec![None; self.edges.len()];
            for (f, face) in self.faces.iter().enumerate() {
                for u in face.loops.iter().flatten() {
                    match shell_of_edge[u.edge as usize] {
                        Some(s) if s != shell_of[f] => {
                            return Err(format!("edge {} joins two shells", u.edge));
                        }
                        _ => shell_of_edge[u.edge as usize] = Some(shell_of[f]),
                    }
                }
            }
        }
        // Every face's integrals can be taken (a surface of revolution's boundary along its
        // meridians and parallels, no loop winding round it).
        for (f, face) in self.faces.iter().enumerate() {
            self.face_integrals(face, Point3::ORIGIN)
                .map_err(|e| format!("face {f}: {e}"))?;
        }
        Ok(())
    }

    /// The largest coordinate in absolute value: the size rounding is scaled by.
    fn scale(&self) -> f64 {
        self.vertices
            .iter()
            .map(|v| v.point.x.abs().max(v.point.y.abs()).max(v.point.z.abs()))
            .fold(0.0, f64::max)
    }

    /// The solid moved by `transform`. A mirror turns every loop around, so that the
    /// faces still run counter-clockwise about their (mirrored, still outward) normals.
    pub fn transformed(&self, transform: &Transform) -> Solid {
        let mirror = transform.is_mirror();
        Solid {
            vertices: self
                .vertices
                .iter()
                .map(|v| Vertex {
                    point: transform.apply_point(v.point),
                    tolerance: v.tolerance,
                })
                .collect(),
            edges: self
                .edges
                .iter()
                .map(|edge| Edge {
                    curve: edge.curve.transformed(transform),
                    ..*edge
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
            shells: self.shells.clone(),
        }
    }

    /// The loops of a face on a surface of revolution as closed polygons of its `(θ, v)`
    /// plane: each straight use's start, and the points a bending one is followed along.
    pub fn param_polygons(
        &self,
        face: &Face,
        surface: &Revolution,
    ) -> Result<Vec<Vec<(f64, f64)>>, String> {
        let mut out = Vec::new();
        for uses in self.param_loops(face, surface)? {
            let mut polygon = Vec::new();
            for u in uses {
                if u.straight {
                    polygon.push(u.from);
                } else {
                    let w = self.walk_edge(u.edge_use);
                    let path = ParamPath::follow(surface, &w, u.from, NEAR_AXIS)?;
                    let points = path.points();
                    polygon.extend_from_slice(&points[..points.len() - 1]);
                }
            }
            out.push(polygon);
        }
        Ok(out)
    }

    /// Points of the face, inside it, where its surface reaches furthest along a model axis
    /// between its edges: a sphere's or a torus's bulge, which its edges may not reach.
    pub fn bulges(&self, face: &Face) -> Vec<Point3> {
        let Surface::Revolution(surface) = &face.surface else {
            return Vec::new();
        };
        if !matches!(surface.meridian, Meridian::Circle { .. }) {
            return Vec::new();
        }
        let Ok(loops) = self.param_polygons(face, surface) else {
            return Vec::new();
        };
        let (x, y, a) = surface.frame();
        let mut out = Vec::new();
        for d in [Vec3::X, Vec3::Y, Vec3::Z] {
            let (dx, dy, dz) = (d.dot(x), d.dot(y), d.dot(a));
            let h = (dx * dx + dy * dy).sqrt();
            if h > 1e-12 {
                // Along the meridian at θ the height along d is h (center + radius cos v) +
                // dz radius sin v, at its extremes where tan v = dz / h.
                let theta = dy.atan2(dx);
                for (t, hh) in [(theta, h), (theta + PI, -h)] {
                    let v = dz.atan2(hh);
                    for v in [v, v + PI] {
                        // Any turn of v round the meridian circle is the same point.
                        if param_inside(&loops, t, v, true) {
                            out.push(surface.point(t, v));
                        }
                    }
                }
            } else {
                // Along the axis: the top and the bottom of the meridian circle, the same
                // height all round.
                for v in [PI / 2.0, -PI / 2.0] {
                    if let Some(t) = param_row(&loops, v, true) {
                        out.push(surface.point(t, v));
                    }
                }
            }
        }
        out
    }
}

/// The edges of closed polygons, each from a corner to the next.
fn polygon_edges(
    polygons: &[Vec<(f64, f64)>],
) -> impl Iterator<Item = ((f64, f64), (f64, f64))> + '_ {
    polygons.iter().flat_map(|p| cyclic_pairs(p))
}

/// Whether `(θ, v)` (or a copy of it a whole turn away, round the axis or, with
/// `v_periodic`, round the meridian circle) is inside the polygons, by the parity of their
/// crossings of a line from it along `θ`.
fn param_inside(polygons: &[Vec<(f64, f64)>], theta: f64, v: f64, v_periodic: bool) -> bool {
    let tau = std::f64::consts::TAU;
    let v_shifts: &[f64] = if v_periodic {
        &[-1.0, 0.0, 1.0]
    } else {
        &[0.0]
    };
    for kt in -2..=2 {
        for &kv in v_shifts {
            let (t, w) = (theta + f64::from(kt) * tau, v + kv * tau);
            let mut crossings = 0;
            for ((t0, v0), (t1, v1)) in polygon_edges(polygons) {
                if (v0 > w) != (v1 > w) {
                    let at = t0 + (w - v0) / (v1 - v0) * (t1 - t0);
                    if at > t {
                        crossings += 1;
                    }
                }
            }
            if crossings % 2 == 1 {
                return true;
            }
        }
    }
    false
}

/// An angle where the line of constant `v` runs inside the polygons, if it does.
fn param_row(polygons: &[Vec<(f64, f64)>], v: f64, v_periodic: bool) -> Option<f64> {
    let tau = std::f64::consts::TAU;
    let v_shifts: &[f64] = if v_periodic {
        &[-1.0, 0.0, 1.0]
    } else {
        &[0.0]
    };
    for &kv in v_shifts {
        let w = v + kv * tau;
        let mut at: Vec<f64> = polygon_edges(polygons)
            .filter(|((_, v0), (_, v1))| (*v0 > w) != (*v1 > w))
            .map(|((t0, v0), (t1, v1))| t0 + (w - v0) / (v1 - v0) * (t1 - t0))
            .collect();
        at.sort_by(f64::total_cmp);
        if let [a, b, ..] = at.as_slice() {
            return Some((a + b) / 2.0);
        }
    }
    None
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
                let fallback = solid.vertices.first().map_or(Point3::ORIGIN, |v| v.point);
                let (surface, normal, axis) = match &face.surface {
                    Surface::Plane(plane) => (SurfaceKind::Plane, Some(plane.normal), None),
                    Surface::Revolution(r) => (r.kind(), None, Some(r.axis)),
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
                    Curve::Circle(circle) => EdgeInfo {
                        curve: CurveKind::Circle,
                        length: circle.radius * solid.sweep(edge),
                        midpoint: solid.midpoint(edge),
                        axis: Axis::new(circle.center, circle.normal),
                        vertices,
                        faces,
                    },
                    Curve::Degenerate { .. } => EdgeInfo {
                        curve: CurveKind::Degenerate,
                        length: 0.0,
                        midpoint: a,
                        axis: None,
                        vertices,
                        faces,
                    },
                    Curve::Section(section) => EdgeInfo {
                        curve: if section.is_ellipse() {
                            CurveKind::Ellipse
                        } else {
                            CurveKind::BSpline
                        },
                        length: section.length(a, solid.sweep(edge)),
                        midpoint: solid.midpoint(edge),
                        axis: None,
                        vertices,
                        faces,
                    },
                });
            }
            topology
                .vertices
                .extend(solid.vertices.iter().map(|v| VertexInfo { point: v.point }));
        }
        Ok(topology)
    }

    pub fn counts(&self) -> TopologyCounts {
        TopologyCounts {
            solids: self.solids.len(),
            shells: self.solids.iter().map(|s| s.shells.len().max(1)).sum(),
            faces: self.face_count(),
            edges: self.solids.iter().map(|s| s.edges.len()).sum(),
            vertices: self.solids.iter().map(|s| s.vertices.len()).sum(),
        }
    }

    /// The smallest box along the model axes holding the shape: its vertices, the furthest
    /// points its arcs reach between them, and where a sphere's or a torus's face bulges
    /// out between its edges (a plane, a cylinder or a cone reaches its extremes on its
    /// edges). `None` for a shape with no vertex.
    pub fn bounds(&self) -> Option<(Point3, Point3)> {
        let mut points: Vec<Point3> = Vec::new();
        for solid in &self.solids {
            points.extend(solid.vertices.iter().map(|v| v.point));
            for edge in &solid.edges {
                match edge.curve {
                    Curve::Circle(circle) => {
                        let start = circle.angle_of(solid.point(edge.start));
                        points.extend(circle.extremes(start, solid.sweep(edge)));
                    }
                    Curve::Section(section) => {
                        points.extend(section.extremes(solid.point(edge.start), solid.sweep(edge)));
                    }
                    _ => {}
                }
            }
            for face in &solid.faces {
                points.extend(solid.bulges(face));
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
        self.solids
            .iter()
            .find_map(|s| s.vertices.first().map(|v| v.point))
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
