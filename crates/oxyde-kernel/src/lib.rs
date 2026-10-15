//! The `Kernel` trait: the boundary between Oxyde's modeling logic and a B-rep geometry kernel.
//!
//! A kernel does geometry: it builds solids and answers questions about them. It does not
//! remember anything between operations — keeping track of which face is which across a
//! feature timeline is the job of `oxyde-model`. To make that possible, every modeling
//! operation returns an [`OpHistory`] telling where each resulting face came from.
//!
//! See `docs/ARCHITECTURE.md` §4.

use std::path::Path;

use oxyde_geom::{Axis, Plane, Point3, Profile, Transform, Vec3};

/// Opaque reference to a shape owned by a kernel backend.
///
/// Handles are only meaningful for the kernel instance that issued them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShapeHandle(u64);

impl ShapeHandle {
    /// Wraps a backend-specific identifier. Intended for kernel backends only.
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The backend-specific identifier. Intended for kernel backends only.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Number of distinct topological entities of each kind in a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TopologyCounts {
    pub solids: usize,
    pub shells: usize,
    pub faces: usize,
    pub edges: usize,
    pub vertices: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    Plane,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    BSpline,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveKind {
    Line,
    Circle,
    Ellipse,
    BSpline,
    /// Zero-length edge, e.g. at the pole of a sphere.
    Degenerate,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FaceInfo {
    pub surface: SurfaceKind,
    /// Area in mm².
    pub area: f64,
    pub centroid: Point3,
    /// Outward unit normal, for planar faces only.
    pub normal: Option<Vec3>,
    /// Axis of revolution of a cylindrical, conical, spherical or toroidal face.
    pub axis: Option<Axis>,
    /// Indices into [`Topology::edges`].
    pub edges: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EdgeInfo {
    pub curve: CurveKind,
    /// Length in mm.
    pub length: f64,
    /// Point at the middle of the edge's parameter range.
    pub midpoint: Point3,
    /// For a straight edge, its line (from its start, along the edge); for a circular edge,
    /// the circle's axis (through its centre, along its normal).
    pub axis: Option<Axis>,
    /// Indices into [`Topology::vertices`]: two for an open edge, one for a closed edge.
    pub vertices: Vec<u32>,
    /// Indices into [`Topology::faces`] of the faces bounded by this edge.
    pub faces: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VertexInfo {
    pub point: Point3,
}

/// Faces, edges and vertices of a shape, with adjacency.
///
/// Indices are positions in these vectors. They are stable for a given shape, but carry no
/// meaning from one shape to the next: never store them across operations.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Topology {
    pub faces: Vec<FaceInfo>,
    pub edges: Vec<EdgeInfo>,
    pub vertices: Vec<VertexInfo>,
}

/// Where a face of an operation's result comes from.
///
/// `input` is the position of the input shape in the operation's arguments: for
/// [`Kernel::boolean`] 0 is the target and 1 the tool (1, 2… for the tools of
/// [`Kernel::boolean_many`]); for fillet and chamfer 0 is the shape.
/// `face`, `edge` and `vertex` index that input's [`Topology`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceSource {
    /// The face is an input face, unchanged or modified (trimmed, split, merged).
    InputFace { input: u32, face: u32 },
    /// The face was generated from an input edge (e.g. a fillet face).
    InputEdge { input: u32, edge: u32 },
    /// The face was generated from an input vertex (e.g. a corner blend).
    InputVertex { input: u32, vertex: u32 },
    /// The face is an offset copy of an input face (the inner wall of a shell).
    OffsetFace { input: u32, face: u32 },
    /// Side face swept (extruded or revolved) from the profile segment with this id.
    ProfileSegment(u32),
    /// Face lying on the profile plane, where the extrusion or revolution starts.
    StartCap,
    /// Face at the far end of the extrusion or revolution.
    EndCap,
}

/// For each face of an operation's result (same index as its [`Topology::faces`]), every
/// source the kernel reported. An empty list means the kernel could not tell.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OpHistory {
    pub faces: Vec<Vec<FaceSource>>,
}

/// The new shape created by an operation, and the history of its faces.
#[derive(Debug, Clone, PartialEq)]
pub struct OpResult {
    pub shape: ShapeHandle,
    pub history: OpHistory,
}

/// Triangles of one face, for display.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FaceMesh {
    pub positions: Vec<[f32; 3]>,
    /// Indices into `positions`, counter-clockwise seen from outside the solid.
    pub triangles: Vec<[u32; 3]>,
}

/// Display mesh of a shape: one [`FaceMesh`] per face and one polyline per edge, indexed
/// exactly like the shape's [`Topology`], so a triangle or a line under the mouse maps
/// straight back to a face or an edge.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mesh {
    pub faces: Vec<FaceMesh>,
    pub edges: Vec<Vec<[f32; 3]>>,
}

/// The product structure of a STEP file: its parts and (sub-)assemblies, each listed once
/// however many times it is used. Read by [`Kernel::read_step_assembly`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StepAssembly {
    pub nodes: Vec<StepNode>,
    /// Index into `nodes` of the top-level product.
    pub root: usize,
}

/// A part or an assembly of a STEP file.
#[derive(Debug, Clone, PartialEq)]
pub struct StepNode {
    /// Product name as written by the exporting system (may be empty).
    pub name: String,
    pub content: StepNodeContent,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepNodeContent {
    /// A part: its solids alone, in its own coordinates, written as a STEP file that
    /// [`Kernel::import_step`] reads back.
    Part { step: Vec<u8> },
    /// An assembly: instances of other nodes.
    Assembly { instances: Vec<StepInstance> },
}

/// One use of a node inside an assembly.
#[derive(Debug, Clone, PartialEq)]
pub struct StepInstance {
    /// Index into [`StepAssembly::nodes`].
    pub node: usize,
    /// Instance name as written by the exporting system (may be empty).
    pub name: String,
    /// Where the node sits in the assembly: always a rotation and a translation. Mirrored
    /// or scaled instances are turned into parts of their own, with the motion applied.
    pub transform: Transform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BooleanOp {
    Fuse,
    Cut,
    Common,
}

/// A B-rep geometry kernel.
///
/// Operations never modify their inputs: they create new shapes, which the caller must
/// eventually [`release`](Kernel::release).
pub trait Kernel {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Axis-aligned box with one corner at `corner` and extents `size` (all components > 0).
    fn make_box(&mut self, corner: Point3, size: Vec3) -> Result<ShapeHandle, Self::Error>;

    /// Sweeps the regions of `profile`, drawn on `plane`, along `vector`.
    ///
    /// History sources: [`FaceSource::ProfileSegment`], [`FaceSource::StartCap`],
    /// [`FaceSource::EndCap`].
    fn extrude(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        vector: Vec3,
    ) -> Result<OpResult, Self::Error>;

    /// Revolves the regions of `profile`, drawn on `plane`, by `angle` radians
    /// (0 < angle ≤ 2π) about `axis`, which lies in the plane, counter-clockwise looking
    /// against the axis direction.
    ///
    /// History sources: [`FaceSource::ProfileSegment`], and for less than a full turn
    /// [`FaceSource::StartCap`] and [`FaceSource::EndCap`].
    fn revolve(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        axis: &Axis,
        angle: f64,
    ) -> Result<OpResult, Self::Error>;

    /// Hollows a solid: removes the given faces (indices into the shape's
    /// [`Topology::faces`]) and turns the remaining faces into walls of thickness
    /// `|offset|`, inside the solid for a negative offset, outside for a positive one.
    /// With no face removed the result is closed: the solid around a void (inward), or a
    /// skin around the solid (outward).
    ///
    /// History sources: [`FaceSource::InputFace`] for the original faces,
    /// [`FaceSource::OffsetFace`] for their offset copies, [`FaceSource::InputEdge`] and
    /// [`FaceSource::InputVertex`] for faces closing the walls.
    fn shell(
        &mut self,
        shape: ShapeHandle,
        removed_faces: &[u32],
        offset: f64,
    ) -> Result<OpResult, Self::Error>;

    /// A moved (or mirrored) copy of a shape. History: every face comes from the input face
    /// it is a copy of ([`FaceSource::InputFace`]).
    fn transform(
        &mut self,
        shape: ShapeHandle,
        transform: &Transform,
    ) -> Result<OpResult, Self::Error>;

    /// Boolean of `target` (input 0) with `tool` (input 1).
    fn boolean(
        &mut self,
        op: BooleanOp,
        target: ShapeHandle,
        tool: ShapeHandle,
    ) -> Result<OpResult, Self::Error> {
        self.boolean_many(op, target, &[tool])
    }

    /// Boolean of `target` (input 0) with several tools (inputs 1, 2…) in one operation,
    /// far faster than one boolean per tool: a fuse joins them all, a cut removes them all,
    /// a common keeps what the target shares with any of them.
    fn boolean_many(
        &mut self,
        op: BooleanOp,
        target: ShapeHandle,
        tools: &[ShapeHandle],
    ) -> Result<OpResult, Self::Error>;

    /// Rounds the given edges (indices into the shape's [`Topology::edges`]).
    fn fillet(
        &mut self,
        shape: ShapeHandle,
        edges: &[u32],
        radius: f64,
    ) -> Result<OpResult, Self::Error>;

    /// Bevels the given edges (indices into the shape's [`Topology::edges`]) symmetrically.
    fn chamfer(
        &mut self,
        shape: ShapeHandle,
        edges: &[u32],
        distance: f64,
    ) -> Result<OpResult, Self::Error>;

    fn topology(&self, shape: ShapeHandle) -> Result<Topology, Self::Error>;

    /// Display mesh; `deflection` is the maximum distance in mm between the mesh and the
    /// true surface.
    fn tessellate(&self, shape: ShapeHandle, deflection: f64) -> Result<Mesh, Self::Error>;

    /// Volume of a solid, in mm³.
    fn volume(&self, shape: ShapeHandle) -> Result<f64, Self::Error>;

    fn topology_counts(&self, shape: ShapeHandle) -> Result<TopologyCounts, Self::Error>;

    /// Writes `shapes` to a STEP file at `path`, in millimetres.
    fn export_step(&self, shapes: &[ShapeHandle], path: &Path) -> Result<(), Self::Error>;

    /// Reads the solids of a STEP file (its contents, not its path), in millimetres. Several
    /// solids come back as one compound shape; sewn surfaces are kept if they close a
    /// volume.
    fn import_step(&mut self, data: &[u8]) -> Result<ShapeHandle, Self::Error>;

    /// Reads the product structure of a STEP file (its contents): every distinct part, with
    /// its solids as a STEP file of its own, and every assembly with the placed instances
    /// of its children. Products without solids are left out. A file without assembly
    /// structure gives a single part.
    fn read_step_assembly(&mut self, data: &[u8]) -> Result<StepAssembly, Self::Error>;

    /// Frees a shape. Releasing an unknown or already released handle does nothing.
    fn release(&mut self, shape: ShapeHandle);
}
