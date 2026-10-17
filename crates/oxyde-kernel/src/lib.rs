//! The `Kernel` trait: the boundary between Oxyde's modeling logic and a B-rep geometry kernel.
//!
//! A kernel does geometry: it builds solids and answers questions about them. It does not
//! remember anything between operations — keeping track of which face is which across a
//! feature timeline is the job of `oxyde-model`. To make that possible, every modeling
//! operation returns an [`OpHistory`] telling where each resulting face came from.
//!
//! See `docs/ARCHITECTURE.md` §4.

// Never crash: a panic in a modelling or UI path would take the unsaved document with it.
// Return an error or fall back to a safe default instead (docs/ARCHITECTURE.md, "Never crash").
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]
#![warn(clippy::indexing_slicing)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing
    )
)]

use std::path::Path;

use oxyde_geom::{Axis, Path as SweepPath, Plane, Point3, Profile, Transform, Vec3};

/// Runs `f`, turning a panic inside it into an error carrying the panic's message.
///
/// The net under every kernel call and every feature build: a bug in a backend or in the
/// modelling code becomes an error the caller reports, instead of unwinding through the
/// application and taking the unsaved document with it. The panic is still printed to
/// stderr by the default hook, so nothing is hidden from a developer. See
/// `docs/ARCHITECTURE.md`, "Never crash".
pub fn catch_panic<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .map_err(|payload| panic_message(payload.as_ref()))
}

/// The message a panic carried: the text of `panic!("…")`, of a failed `unwrap` or of an
/// index out of range, or a stand-in when it carried something else.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic without a message".to_owned()
    }
}

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

/// A colour as STEP carries it: sRGB and how see-through it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepColor {
    /// sRGB, 0–255 per channel.
    pub rgb: [u8; 3],
    /// 0 = opaque, 1 = invisible.
    pub transparency: f64,
}

/// The colours of one shape as a STEP file carries them: one per solid (in the order of
/// the shape's solids) and one per face (indexed like the shape's [`Topology`]). `None`
/// where the file gives none. A face without a colour of its own takes its solid's when
/// read, so `faces` is the colour each face is drawn with.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StepColors {
    pub solids: Vec<Option<StepColor>>,
    pub faces: Vec<Option<StepColor>>,
}

impl StepColors {
    pub fn is_empty(&self) -> bool {
        self.solids.iter().all(Option::is_none) && self.faces.iter().all(Option::is_none)
    }
}

/// The material a STEP file gives a part: its `material name` and `density` properties
/// (AP214/AP242), as Fusion 360 and SolidWorks write them for every body.
#[derive(Debug, Clone, PartialEq)]
pub struct StepMaterial {
    pub name: String,
    pub description: String,
    /// kg/m³; 0 when the file gives none.
    pub density: f64,
}

/// What [`Kernel::import_step`] reads: the solids as one shape, their colours, and the
/// material the file names for them (the first product's, when there are several).
#[derive(Debug, Clone, PartialEq)]
pub struct StepImport {
    pub shape: ShapeHandle,
    pub colors: StepColors,
    pub material: Option<StepMaterial>,
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
    /// A part: its solids alone, in its own coordinates, with their colours and material,
    /// written as a STEP file that [`Kernel::import_step`] reads back.
    Part {
        step: Vec<u8>,
        /// What the file says the part is made of.
        material: Option<StepMaterial>,
    },
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

    /// Sweeps the regions of `profile`, drawn on `plane`, along `path`, drawn on
    /// `path_plane`: the profile is carried from the path's start to its end, kept at the
    /// same angle to the path (a corrected Frenet frame), so a round profile swept along a
    /// bend gives a pipe. A closed path (a full circle, a rounded rectangle) gives a ring
    /// with no caps.
    ///
    /// History sources: [`FaceSource::ProfileSegment`] (one face per profile segment and
    /// path segment), and for an open path [`FaceSource::StartCap`] and
    /// [`FaceSource::EndCap`].
    fn sweep(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        path_plane: &Plane,
        path: &SweepPath,
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

    /// A fresh mesh for export (STL): at most `deflection` mm from the true surface and
    /// at most `angle` radians between the normals at the ends of a triangle's edge, so
    /// that small curved features keep their shape. Unlike [`Kernel::tessellate`], a mesh
    /// made earlier with other settings is not reused.
    fn tessellate_for_export(
        &self,
        shape: ShapeHandle,
        deflection: f64,
        angle: f64,
    ) -> Result<Mesh, Self::Error> {
        let _ = angle;
        self.tessellate(shape, deflection)
    }

    /// Volume of a solid, in mm³.
    fn volume(&self, shape: ShapeHandle) -> Result<f64, Self::Error>;

    /// Centre of mass of a solid of uniform density, in mm.
    fn centre_of_mass(&self, shape: ShapeHandle) -> Result<Point3, Self::Error>;

    /// The smallest box along the model axes holding the shape, `(min, max)`: exact, not
    /// from a mesh, and without tolerance added.
    fn bounds(&self, shape: ShapeHandle) -> Result<(Point3, Point3), Self::Error>;

    fn topology_counts(&self, shape: ShapeHandle) -> Result<TopologyCounts, Self::Error>;

    /// Writes `shapes` to a STEP file at `path`, in millimetres, with their colours
    /// (`colors` is indexed like `shapes`; empty for none) as STEP colours per solid and
    /// per face, and their materials (`materials` indexed like `shapes`; empty for none) as
    /// STEP material names and densities, which other CAD systems read back.
    fn export_step(
        &self,
        shapes: &[ShapeHandle],
        colors: &[StepColors],
        materials: &[Option<StepMaterial>],
        path: &Path,
    ) -> Result<(), Self::Error>;

    /// Writes `shapes` to an IGES 5.3 file at `path`, in millimetres, as B-rep solids
    /// (IGES carries no colours or materials worth writing).
    fn export_iges(&self, shapes: &[ShapeHandle], path: &Path) -> Result<(), Self::Error>;

    /// Reads the solids of a STEP file (its contents, not its path), in millimetres, with
    /// the colours the file gives them. Several solids come back as one compound shape;
    /// sewn surfaces are kept if they close a volume.
    fn import_step(&mut self, data: &[u8]) -> Result<StepImport, Self::Error>;

    /// Reads the product structure of a STEP file (its contents): every distinct part, with
    /// its solids as a STEP file of its own, and every assembly with the placed instances
    /// of its children. Products without solids are left out. A file without assembly
    /// structure gives a single part.
    fn read_step_assembly(&mut self, data: &[u8]) -> Result<StepAssembly, Self::Error>;

    /// Frees a shape. Releasing an unknown or already released handle does nothing.
    fn release(&mut self, shape: ShapeHandle);

    /// Opens a kernel on the calling thread that shares this kernel's shapes: a handle made
    /// by either is valid in the other, so a part can be rebuilt on a worker thread and its
    /// shapes drawn, or released, from the main one. `None` when the kernel's shapes are
    /// bound to the thread that made them. Two kernels must never work on the same shape
    /// at the same time: a part stays with one thread until it is handed over.
    fn open_worker() -> Option<Self>
    where
        Self: Sized,
    {
        None
    }
}
