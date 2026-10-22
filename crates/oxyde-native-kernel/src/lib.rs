//! Oxyde's own B-rep kernel, in Rust: the second backend of `oxyde-kernel`, beside
//! OpenCASCADE (`oxyde-occt`).
//!
//! It is written from the mathematics, depends only on `oxyde-geom` and `oxyde-kernel`, and is
//! MIT like the rest of Oxyde. Ideas and code may be adapted from SolidCraft's kernel
//! (MIT OR Apache-2.0) where they do not depend on truck, with its notice kept; never from
//! OpenCASCADE's source, which is LGPL. OpenCASCADE is used only as a black box: the tests in
//! `crates/oxyde-tests/tests/` compare what both kernels build. See `docs/ARCHITECTURE.md` §2
//! and §10 (Native kernel).
//!
//! What it builds so far are solids with flat faces and straight edges: boxes, extrusions of
//! profiles made of lines, their moved or mirrored copies, and booleans of them (fuse, cut,
//! common), with their topology, exact mass properties, bounds and display meshes. Every
//! other operation answers
//! [`NativeError::Unsupported`] until it is written, so a caller sees a failing feature with
//! a message, never a crash.

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
#![forbid(unsafe_code)]
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

mod assemble;
mod boolean;
mod brep;
mod bsp;
mod build;
mod tessellate;
mod triangulate;

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use oxyde_geom::{Axis, Path as SweepPath, Plane, Point3, Profile, Transform, Vec3};
use oxyde_kernel::{
    BooleanOp, FaceSource, Kernel, Mesh, OpHistory, OpResult, ShapeHandle, StepAssembly,
    StepColors, StepImport, StepMaterial, Topology, TopologyCounts,
};

use brep::Shape;

/// Error reported by the native kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeError {
    /// The operation, or this use of it, is not written yet: what the kernel cannot do,
    /// as in "extrude arcs and circles".
    Unsupported(&'static str),
    /// The arguments describe nothing the kernel can build (a box of no size, an
    /// extrusion along its own sketch plane); the message says why.
    Failed(String),
    /// A handle this kernel did not issue, or one already released.
    UnknownShape(u64),
    /// A panic inside the kernel, caught at the [`Kernel`] boundary, or a result that
    /// failed the kernel's own checks: a bug, not a geometric failure.
    Internal(String),
}

impl NativeError {
    /// Whether this is a bug in the kernel rather than a request it cannot meet.
    pub fn is_internal(&self) -> bool {
        matches!(self, NativeError::Internal(_))
    }
}

impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NativeError::Unsupported(operation) => {
                write!(f, "the native kernel cannot {operation} yet")
            }
            NativeError::Failed(message) => f.write_str(message),
            NativeError::UnknownShape(id) => write!(f, "unknown shape handle {id}"),
            NativeError::Internal(message) => {
                write!(f, "internal error in the native kernel: {message}")
            }
        }
    }
}

impl std::error::Error for NativeError {}

/// Runs a kernel operation, turning a panic inside it into [`NativeError::Internal`]:
/// a bug in the kernel must fail the feature, not take the application down.
fn guarded<T>(f: impl FnOnce() -> Result<T, NativeError>) -> Result<T, NativeError> {
    oxyde_kernel::catch_panic(f).unwrap_or_else(|message| Err(NativeError::Internal(message)))
}

/// A [`Kernel`] written in Rust, with no C++ underneath.
///
/// It owns the shapes it makes, by handle, until they are released. Its shapes stay with
/// the kernel that made them: it opens no workers yet ([`Kernel::open_worker`]).
#[derive(Debug, Default)]
pub struct NativeKernel {
    shapes: HashMap<u64, Shape>,
    /// The last handle issued; 0 is never one.
    last: u64,
}

impl NativeKernel {
    /// A kernel holding no shapes.
    pub fn new() -> Self {
        Self::default()
    }

    fn shape(&self, handle: ShapeHandle) -> Result<&Shape, NativeError> {
        self.shapes
            .get(&handle.raw())
            .ok_or(NativeError::UnknownShape(handle.raw()))
    }

    /// Keeps a shape an operation made, once it has passed the checks every result must
    /// pass: an inconsistent solid is a bug, reported rather than handed on.
    fn insert(&mut self, shape: Shape) -> Result<ShapeHandle, NativeError> {
        shape.check().map_err(NativeError::Internal)?;
        self.last += 1;
        self.shapes.insert(self.last, shape);
        Ok(ShapeHandle::from_raw(self.last))
    }
}

impl Kernel for NativeKernel {
    type Error = NativeError;

    fn make_box(&mut self, corner: Point3, size: Vec3) -> Result<ShapeHandle, NativeError> {
        guarded(|| {
            let shape = build::make_box(corner, size)?;
            self.insert(shape)
        })
    }

    fn extrude(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        vector: Vec3,
    ) -> Result<OpResult, NativeError> {
        guarded(|| {
            let (shape, history) = build::extrude(plane, profile, vector)?;
            Ok(OpResult {
                shape: self.insert(shape)?,
                history,
            })
        })
    }

    fn revolve(
        &mut self,
        _plane: &Plane,
        _profile: &Profile,
        _axis: &Axis,
        _angle: f64,
    ) -> Result<OpResult, NativeError> {
        Err(NativeError::Unsupported("revolve"))
    }

    fn sweep(
        &mut self,
        _plane: &Plane,
        _profile: &Profile,
        _path_plane: &Plane,
        _path: &SweepPath,
    ) -> Result<OpResult, NativeError> {
        Err(NativeError::Unsupported("sweep"))
    }

    fn shell(
        &mut self,
        _shape: ShapeHandle,
        _removed_faces: &[u32],
        _offset: f64,
    ) -> Result<OpResult, NativeError> {
        Err(NativeError::Unsupported("shell"))
    }

    fn transform(
        &mut self,
        shape: ShapeHandle,
        transform: &Transform,
    ) -> Result<OpResult, NativeError> {
        guarded(|| {
            let moved = self.shape(shape)?.transformed(transform);
            // Faces keep their order: each is the copy of the input face at its index.
            let history = OpHistory {
                faces: (0..moved.face_count() as u32)
                    .map(|face| vec![FaceSource::InputFace { input: 0, face }])
                    .collect(),
            };
            Ok(OpResult {
                shape: self.insert(moved)?,
                history,
            })
        })
    }

    fn boolean_many(
        &mut self,
        op: BooleanOp,
        target: ShapeHandle,
        tools: &[ShapeHandle],
    ) -> Result<OpResult, NativeError> {
        guarded(|| {
            let tool_shapes = tools
                .iter()
                .map(|&tool| self.shape(tool))
                .collect::<Result<Vec<&Shape>, _>>()?;
            let (shape, history) = boolean::boolean(op, self.shape(target)?, &tool_shapes)?;
            Ok(OpResult {
                shape: self.insert(shape)?,
                history,
            })
        })
    }

    fn solid_from_mesh(
        &mut self,
        _points: &[[f64; 3]],
        _triangles: &[[u32; 3]],
    ) -> Result<OpResult, NativeError> {
        Err(NativeError::Unsupported("make a solid from a mesh"))
    }

    fn fillet(
        &mut self,
        _shape: ShapeHandle,
        _edges: &[u32],
        _radius: f64,
    ) -> Result<OpResult, NativeError> {
        Err(NativeError::Unsupported("fillet"))
    }

    fn chamfer(
        &mut self,
        _shape: ShapeHandle,
        _edges: &[u32],
        _distance: f64,
    ) -> Result<OpResult, NativeError> {
        Err(NativeError::Unsupported("chamfer"))
    }

    fn topology(&self, shape: ShapeHandle) -> Result<Topology, NativeError> {
        guarded(|| Ok(self.shape(shape)?.topology()))
    }

    /// Exact whatever `deflection` asks: every face is planar so far.
    fn tessellate(&self, shape: ShapeHandle, _deflection: f64) -> Result<Mesh, NativeError> {
        guarded(|| tessellate::mesh(self.shape(shape)?).map_err(NativeError::Internal))
    }

    fn volume(&self, shape: ShapeHandle) -> Result<f64, NativeError> {
        guarded(|| Ok(self.shape(shape)?.volume()))
    }

    fn centre_of_mass(&self, shape: ShapeHandle) -> Result<Point3, NativeError> {
        guarded(|| {
            self.shape(shape)?
                .centre_of_mass()
                .ok_or_else(|| NativeError::Failed("the shape has no volume".to_owned()))
        })
    }

    fn bounds(&self, shape: ShapeHandle) -> Result<(Point3, Point3), NativeError> {
        guarded(|| {
            self.shape(shape)?
                .bounds()
                .ok_or_else(|| NativeError::Failed("the shape is empty".to_owned()))
        })
    }

    fn topology_counts(&self, shape: ShapeHandle) -> Result<TopologyCounts, NativeError> {
        guarded(|| Ok(self.shape(shape)?.counts()))
    }

    fn export_step(
        &self,
        _shapes: &[ShapeHandle],
        _colors: &[StepColors],
        _materials: &[Option<StepMaterial>],
        _path: &Path,
    ) -> Result<(), NativeError> {
        Err(NativeError::Unsupported("write STEP files"))
    }

    fn export_iges(&self, _shapes: &[ShapeHandle], _path: &Path) -> Result<(), NativeError> {
        Err(NativeError::Unsupported("write IGES files"))
    }

    fn import_step(&mut self, _data: &[u8]) -> Result<StepImport, NativeError> {
        Err(NativeError::Unsupported("read STEP files"))
    }

    fn read_step_assembly(&mut self, _data: &[u8]) -> Result<StepAssembly, NativeError> {
        Err(NativeError::Unsupported("read STEP assemblies"))
    }

    fn release(&mut self, shape: ShapeHandle) {
        self.shapes.remove(&shape.raw());
    }
}

#[cfg(test)]
mod tests {
    use oxyde_geom::{Curve2, Point2, ProfileSegment};

    use super::*;

    fn line(id: u32, start: (f64, f64), end: (f64, f64)) -> ProfileSegment {
        ProfileSegment {
            id,
            curve: Curve2::Line {
                start: Point2::new(start.0, start.1),
                end: Point2::new(end.0, end.1),
            },
        }
    }

    #[test]
    fn an_extrusion_names_its_faces_after_the_profile() {
        let mut kernel = NativeKernel::new();
        let profile = Profile::from_curves(&[
            line(1, (0.0, 0.0), (4.0, 0.0)),
            line(2, (4.0, 0.0), (4.0, 3.0)),
            line(3, (4.0, 3.0), (0.0, 3.0)),
            line(4, (0.0, 3.0), (0.0, 0.0)),
        ])
        .unwrap();
        let result = kernel
            .extrude(&Plane::FRONT, &profile, Vec3::new(0.0, 0.0, -2.0))
            .unwrap();
        let topology = kernel.topology(result.shape).unwrap();
        assert_eq!(topology.faces.len(), result.history.faces.len());
        let mut sources: Vec<String> = result
            .history
            .faces
            .iter()
            .map(|s| format!("{s:?}"))
            .collect();
        sources.sort();
        assert_eq!(
            sources,
            [
                "[EndCap]",
                "[ProfileSegment(1)]",
                "[ProfileSegment(2)]",
                "[ProfileSegment(3)]",
                "[ProfileSegment(4)]",
                "[StartCap]",
            ]
        );
        // Every edge between two faces, the start cap on the sketch plane looking up.
        assert!(topology.edges.iter().all(|e| e.faces.len() == 2));
        let start = result
            .history
            .faces
            .iter()
            .position(|s| s == &[FaceSource::StartCap])
            .unwrap();
        assert_eq!(topology.faces[start].normal, Some(Vec3::Z));
        assert!((kernel.volume(result.shape).unwrap() - 24.0).abs() < 1e-9);
        let (lo, hi) = kernel.bounds(result.shape).unwrap();
        assert_eq!((lo.z, hi.z), (-2.0, 0.0));
        kernel.release(result.shape);
        assert_eq!(
            kernel.volume(result.shape),
            Err(NativeError::UnknownShape(result.shape.raw()))
        );
    }

    #[test]
    fn a_mirrored_box_is_still_closed_and_outward() {
        let mut kernel = NativeKernel::new();
        let shape = kernel
            .make_box(Point3::new(1.0, 0.0, 0.0), Vec3::new(2.0, 3.0, 4.0))
            .unwrap();
        let mirror = Transform::mirror(&Plane::RIGHT);
        let mirrored = kernel.transform(shape, &mirror).unwrap();
        assert!((kernel.volume(mirrored.shape).unwrap() - 24.0).abs() < 1e-9);
        let c = kernel.centre_of_mass(mirrored.shape).unwrap();
        assert!(c.distance(Point3::new(-2.0, 1.5, 2.0)) < 1e-9);
        let mesh = kernel.tessellate(mirrored.shape, 0.1).unwrap();
        assert_eq!(mesh.faces.len(), 6);
        assert!(mesh.faces.iter().all(|f| f.triangles.len() == 2));
    }

    fn block(
        kernel: &mut NativeKernel,
        corner: (f64, f64, f64),
        size: (f64, f64, f64),
    ) -> ShapeHandle {
        kernel
            .make_box(
                Point3::new(corner.0, corner.1, corner.2),
                Vec3::new(size.0, size.1, size.2),
            )
            .unwrap()
    }

    #[test]
    fn booleans_of_boxes_have_the_volumes_they_should() {
        let mut kernel = NativeKernel::new();
        let a = block(&mut kernel, (0.0, 0.0, 0.0), (4.0, 4.0, 4.0));
        // Overlapping a corner, and lying on a's top, facing the other way.
        let b = block(&mut kernel, (2.0, 2.0, 2.0), (4.0, 4.0, 4.0));
        let on_top = block(&mut kernel, (1.0, 1.0, 4.0), (2.0, 2.0, 1.0));
        // Through a, flush with its top and its bottom.
        let through = block(&mut kernel, (1.0, 1.0, 0.0), (1.0, 1.0, 4.0));
        let cases = [
            (BooleanOp::Fuse, b, 64.0 + 64.0 - 8.0),
            (BooleanOp::Cut, b, 64.0 - 8.0),
            (BooleanOp::Common, b, 8.0),
            (BooleanOp::Fuse, on_top, 64.0 + 4.0),
            (BooleanOp::Cut, through, 64.0 - 4.0),
        ];
        for (op, tool, volume) in cases {
            let result = kernel.boolean(op, a, tool).unwrap();
            let got = kernel.volume(result.shape).unwrap();
            assert!(
                (got - volume).abs() < 1e-9,
                "{op:?}: {got} instead of {volume}"
            );
            let topology = kernel.topology(result.shape).unwrap();
            assert_eq!(topology.faces.len(), result.history.faces.len());
            assert!(
                result
                    .history
                    .faces
                    .iter()
                    .all(|sources| !sources.is_empty()),
                "{op:?}: a face with no source"
            );
            kernel.release(result.shape);
        }
        // A box fused with a box on its top makes one solid whose top has the small box's
        // footprint taken out, and whose shared faces are gone.
        let fused = kernel.boolean(BooleanOp::Fuse, a, on_top).unwrap();
        let counts = kernel.topology_counts(fused.shape).unwrap();
        assert_eq!((counts.solids, counts.faces), (1, 11));
        // Cutting a through hole leaves one solid with four more faces.
        let holed = kernel.boolean(BooleanOp::Cut, a, through).unwrap();
        let counts = kernel.topology_counts(holed.shape).unwrap();
        assert_eq!((counts.solids, counts.faces), (1, 10));
    }

    #[test]
    fn two_boxes_side_by_side_fuse_into_one_with_six_faces() {
        let mut kernel = NativeKernel::new();
        let a = block(&mut kernel, (0.0, 0.0, 0.0), (4.0, 2.0, 1.0));
        let b = block(&mut kernel, (2.0, 0.0, 0.0), (4.0, 2.0, 1.0));
        let fused = kernel.boolean(BooleanOp::Fuse, a, b).unwrap();
        let counts = kernel.topology_counts(fused.shape).unwrap();
        assert_eq!(
            (counts.solids, counts.faces, counts.edges, counts.vertices),
            (1, 6, 12, 8)
        );
        assert!((kernel.volume(fused.shape).unwrap() - 12.0).abs() < 1e-9);
        // The faces in one plane are one face, made of a face of each box.
        let top = kernel
            .topology(fused.shape)
            .unwrap()
            .faces
            .iter()
            .position(|f| f.normal == Some(Vec3::Z))
            .unwrap();
        assert_eq!(
            fused.history.faces[top],
            [
                FaceSource::InputFace { input: 0, face: 5 },
                FaceSource::InputFace { input: 1, face: 5 },
            ]
        );
    }

    #[test]
    fn a_cut_through_the_middle_leaves_two_solids_and_a_cut_of_everything_none() {
        let mut kernel = NativeKernel::new();
        let a = block(&mut kernel, (0.0, 0.0, 0.0), (6.0, 2.0, 2.0));
        let slot = block(&mut kernel, (2.0, -1.0, -1.0), (2.0, 4.0, 4.0));
        let cut = kernel.boolean(BooleanOp::Cut, a, slot).unwrap();
        let counts = kernel.topology_counts(cut.shape).unwrap();
        assert_eq!((counts.solids, counts.faces), (2, 12));
        assert!((kernel.volume(cut.shape).unwrap() - 16.0).abs() < 1e-9);
        let all = block(&mut kernel, (-1.0, -1.0, -1.0), (8.0, 4.0, 4.0));
        let gone = kernel.boolean(BooleanOp::Cut, a, all).unwrap();
        assert_eq!(kernel.topology_counts(gone.shape).unwrap().solids, 0);
        assert!(gone.history.faces.is_empty());
    }

    #[test]
    fn several_tools_are_cut_at_once() {
        let mut kernel = NativeKernel::new();
        let a = block(&mut kernel, (0.0, 0.0, 0.0), (10.0, 4.0, 2.0));
        let holes: Vec<ShapeHandle> = (0..3)
            .map(|i| {
                block(
                    &mut kernel,
                    (1.0 + 3.0 * f64::from(i), 1.0, -1.0),
                    (1.0, 1.0, 4.0),
                )
            })
            .collect();
        let cut = kernel.boolean_many(BooleanOp::Cut, a, &holes).unwrap();
        assert!((kernel.volume(cut.shape).unwrap() - (80.0 - 3.0 * 2.0)).abs() < 1e-9);
        // The walls of each hole come from its own tool.
        let tools: Vec<u32> = cut
            .history
            .faces
            .iter()
            .flatten()
            .map(|s| match s {
                FaceSource::InputFace { input, .. } => *input,
                _ => u32::MAX,
            })
            .collect();
        for input in 0..=3 {
            assert!(tools.contains(&input), "nothing from input {input}");
        }
    }

    #[test]
    fn what_is_not_written_yet_fails_with_a_message() {
        let mut kernel = NativeKernel::new();
        let circle = Profile::from_curves(&[ProfileSegment {
            id: 1,
            curve: Curve2::Circle {
                center: Point2::new(0.0, 0.0),
                radius: 2.0,
            },
        }])
        .unwrap();
        let error = kernel.extrude(&Plane::FRONT, &circle, Vec3::Z).unwrap_err();
        assert_eq!(
            error.to_string(),
            "the native kernel cannot extrude arcs and circles yet"
        );
        let shape = kernel
            .make_box(Point3::ORIGIN, Vec3::new(1.0, 1.0, 1.0))
            .unwrap();
        assert!(matches!(
            kernel.fillet(shape, &[0], 1.0),
            Err(NativeError::Unsupported(_))
        ));
        assert!(matches!(
            kernel.make_box(Point3::ORIGIN, Vec3::new(1.0, 0.0, 1.0)),
            Err(NativeError::Failed(_))
        ));
    }
}
