//! The `Kernel` trait: the boundary between Oxyde's modeling logic and a B-rep geometry kernel.
//!
//! A kernel does geometry: it builds solids and answers questions about them. It does not
//! remember anything between operations — keeping track of which face is which across a
//! feature timeline is the job of `oxyde-model`. To make that possible, every modeling
//! operation returns an [`OpHistory`] telling where each resulting face came from.
//!
//! See `docs/ARCHITECTURE.md` §4.

use std::path::Path;

use oxyde_geom::{Plane, Point3, Profile, Vec3};

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
/// [`Kernel::boolean`] 0 is the target and 1 the tool; for fillet and chamfer 0 is the shape.
/// `face`, `edge` and `vertex` index that input's [`Topology`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceSource {
    /// The face is an input face, unchanged or modified (trimmed, split, merged).
    InputFace { input: u32, face: u32 },
    /// The face was generated from an input edge (e.g. a fillet face).
    InputEdge { input: u32, edge: u32 },
    /// The face was generated from an input vertex (e.g. a corner blend).
    InputVertex { input: u32, vertex: u32 },
    /// Side face swept from the profile segment with this id.
    ProfileSegment(u32),
    /// Face lying on the profile plane, where the extrusion starts.
    StartCap,
    /// Face at the far end of the extrusion.
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

    /// Boolean of `target` (input 0) with `tool` (input 1).
    fn boolean(
        &mut self,
        op: BooleanOp,
        target: ShapeHandle,
        tool: ShapeHandle,
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

    /// Volume of a solid, in mm³.
    fn volume(&self, shape: ShapeHandle) -> Result<f64, Self::Error>;

    fn topology_counts(&self, shape: ShapeHandle) -> Result<TopologyCounts, Self::Error>;

    /// Writes `shapes` to a STEP file at `path`, in millimetres.
    fn export_step(&self, shapes: &[ShapeHandle], path: &Path) -> Result<(), Self::Error>;

    /// Frees a shape. Releasing an unknown or already released handle does nothing.
    fn release(&mut self, shape: ShapeHandle);
}
