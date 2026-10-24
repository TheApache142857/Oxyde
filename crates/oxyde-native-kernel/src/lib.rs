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
//! What it builds so far: boxes, extrusions and revolves of profiles of lines, arcs and
//! circles (faces on planes, cylinders, cones, spheres, tori and B-spline surfaces; edges
//! on lines, circles, sections, the exact curves where a cylinder or a cone crosses another
//! surface, traced curves where other surfaces cross, B-spline curves, and degenerate edges
//! at a cone's apex or a sphere's pole), lofts, their moved or mirrored copies, booleans
//! (fuse, cut, common) of them, voids included, chamfers and fillets of their edges, shells of them, and
//! sweeps of profiles along paths of lines and arcs, with their topology, exact mass
//! properties, bounds and display meshes. Every other operation answers [`NativeError::Unsupported`] until it is written,
//! so a caller sees a failing feature with a message, never a crash. `docs/NATIVE_KERNEL.md`
//! describes how.

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

mod arrangement;
mod blend;
mod boolean;
mod brep;
mod build;
mod curved;
mod geometry;
mod loft;
mod nurbs;
mod offset;
mod revolution;
mod revolve;
mod spline_face;
mod step;
mod sweep;
mod tessellate;
mod trace;
mod triangulate;

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use oxyde_geom::{Axis, Path as SweepPath, Plane, Point3, Profile, Transform, Vec3};
use oxyde_kernel::{
    BooleanOp, ChamferSecond, FaceSource, Kernel, Mesh, OpHistory, OpResult, ShapeHandle,
    StepAssembly, StepColors, StepImport, StepMaterial, Topology, TopologyCounts,
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

/// The shapes of every native kernel in the process, by handle: a kernel opened on another
/// thread ([`Kernel::open_worker`]) shares them, as OpenCASCADE's sessions share its arena.
/// A shape never changes once made, so a kernel takes its own reference to one and the
/// lock is held only to find, add or drop a shape, never while an operation works.
static SHAPES: LazyLock<RwLock<HashMap<u64, Arc<Shape>>>> = LazyLock::new(Default::default);

/// The last handle issued, by any kernel; 0 is never one.
static LAST: AtomicU64 = AtomicU64::new(0);

/// The shapes for reading. A panic while the lock was held cannot have left the map half
/// changed (it is changed by single inserts and removals), so a poisoned lock is used as it
/// is.
fn shapes() -> RwLockReadGuard<'static, HashMap<u64, Arc<Shape>>> {
    SHAPES.read().unwrap_or_else(PoisonError::into_inner)
}

fn shapes_mut() -> RwLockWriteGuard<'static, HashMap<u64, Arc<Shape>>> {
    SHAPES.write().unwrap_or_else(PoisonError::into_inner)
}

/// A [`Kernel`] written in Rust, with no C++ underneath.
///
/// Its shapes are kept by handle until they are released, in a store every native kernel
/// of the process shares: a handle made by one is valid in any other, so a part can be
/// rebuilt on a worker thread ([`Kernel::open_worker`]) and drawn from the main one.
#[derive(Debug, Default)]
pub struct NativeKernel {
    _private: (),
}

impl NativeKernel {
    /// A kernel on the process's shapes.
    pub fn new() -> Self {
        Self::default()
    }

    fn shape(&self, handle: ShapeHandle) -> Result<Arc<Shape>, NativeError> {
        shapes()
            .get(&handle.raw())
            .cloned()
            .ok_or(NativeError::UnknownShape(handle.raw()))
    }

    /// Keeps a shape an operation made, once it has passed the checks every result must
    /// pass: an inconsistent solid is a bug, reported rather than handed on.
    fn insert(&mut self, shape: Shape) -> Result<ShapeHandle, NativeError> {
        shape.check().map_err(NativeError::Internal)?;
        let handle = LAST.fetch_add(1, Ordering::Relaxed) + 1;
        shapes_mut().insert(handle, Arc::new(shape));
        Ok(ShapeHandle::from_raw(handle))
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
        plane: &Plane,
        profile: &Profile,
        axis: &Axis,
        angle: f64,
    ) -> Result<OpResult, NativeError> {
        guarded(|| {
            let (shape, history) = revolve::revolve(plane, profile, axis, angle)?;
            Ok(OpResult {
                shape: self.insert(shape)?,
                history,
            })
        })
    }

    fn sweep(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        path_plane: &Plane,
        path: &SweepPath,
    ) -> Result<OpResult, NativeError> {
        guarded(|| {
            let (shape, history) = sweep::sweep(plane, profile, path_plane, path)?;
            Ok(OpResult {
                shape: self.insert(shape)?,
                history,
            })
        })
    }

    fn loft(
        &mut self,
        sections: &[(Plane, oxyde_geom::Loop)],
        ruled: bool,
    ) -> Result<Option<OpResult>, NativeError> {
        guarded(|| {
            let (shape, history) = loft::loft(sections, ruled)?;
            Ok(Some(OpResult {
                shape: self.insert(shape)?,
                history,
            }))
        })
    }

    fn shell(
        &mut self,
        shape: ShapeHandle,
        removed_faces: &[u32],
        offset: f64,
    ) -> Result<OpResult, NativeError> {
        guarded(|| {
            let input = self.shape(shape)?;
            let (shape, history) = offset::shell(&input, removed_faces, offset)?;
            Ok(OpResult {
                shape: self.insert(shape)?,
                history,
            })
        })
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
                .collect::<Result<Vec<Arc<Shape>>, _>>()?;
            let tool_shapes: Vec<&Shape> = tool_shapes.iter().map(Arc::as_ref).collect();
            let target = self.shape(target)?;
            let (shape, history) = boolean::boolean(op, &target, &tool_shapes)?;
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
        shape: ShapeHandle,
        edges: &[u32],
        radius: f64,
    ) -> Result<OpResult, NativeError> {
        guarded(|| {
            let input = self.shape(shape)?;
            let edges: Vec<(u32, Option<u32>)> = edges.iter().map(|&e| (e, None)).collect();
            let (shape, history) =
                blend::blend(&input, &edges, blend::Blend::Fillet { radius }, false)?;
            Ok(OpResult {
                shape: self.insert(shape)?,
                history,
            })
        })
    }

    fn chamfer(
        &mut self,
        shape: ShapeHandle,
        edges: &[u32],
        distance: f64,
    ) -> Result<OpResult, NativeError> {
        guarded(|| {
            let input = self.shape(shape)?;
            let edges: Vec<(u32, Option<u32>)> = edges.iter().map(|&e| (e, None)).collect();
            let chamfer = blend::Blend::Chamfer {
                first: distance,
                second: ChamferSecond::Distance(distance),
            };
            let (shape, history) = blend::blend(&input, &edges, chamfer, false)?;
            Ok(OpResult {
                shape: self.insert(shape)?,
                history,
            })
        })
    }

    fn chamfer_with(
        &mut self,
        shape: ShapeHandle,
        edges: &[(u32, u32)],
        first: f64,
        second: ChamferSecond,
    ) -> Result<Option<OpResult>, NativeError> {
        guarded(|| {
            let input = self.shape(shape)?;
            let edges: Vec<(u32, Option<u32>)> = edges.iter().map(|&(e, f)| (e, Some(f))).collect();
            let chamfer = blend::Blend::Chamfer { first, second };
            let (shape, history) = blend::blend(&input, &edges, chamfer, true)?;
            Ok(Some(OpResult {
                shape: self.insert(shape)?,
                history,
            }))
        })
    }

    fn topology(&self, shape: ShapeHandle) -> Result<Topology, NativeError> {
        guarded(|| self.shape(shape)?.topology().map_err(NativeError::Internal))
    }

    /// Planar faces of straight edges exactly; arcs and cylinders within `deflection`.
    fn tessellate(&self, shape: ShapeHandle, deflection: f64) -> Result<Mesh, NativeError> {
        guarded(|| {
            let shape = self.shape(shape)?;
            tessellate::mesh(&shape, deflection).map_err(NativeError::Internal)
        })
    }

    fn volume(&self, shape: ShapeHandle) -> Result<f64, NativeError> {
        guarded(|| self.shape(shape)?.volume().map_err(NativeError::Internal))
    }

    fn centre_of_mass(&self, shape: ShapeHandle) -> Result<Point3, NativeError> {
        guarded(|| {
            self.shape(shape)?
                .centre_of_mass()
                .map_err(NativeError::Internal)?
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
        shapes: &[ShapeHandle],
        colors: &[StepColors],
        materials: &[Option<StepMaterial>],
        path: &Path,
    ) -> Result<(), NativeError> {
        guarded(|| {
            if !colors.is_empty() && colors.len() != shapes.len() {
                return Err(NativeError::Failed(format!(
                    "{} colour sets for {} shapes",
                    colors.len(),
                    shapes.len()
                )));
            }
            if !materials.is_empty() && materials.len() != shapes.len() {
                return Err(NativeError::Failed(format!(
                    "{} materials for {} shapes",
                    materials.len(),
                    shapes.len()
                )));
            }
            let held: Vec<Arc<Shape>> = shapes
                .iter()
                .map(|&s| self.shape(s))
                .collect::<Result<_, _>>()?;
            let refs: Vec<&Shape> = held.iter().map(|s| s.as_ref()).collect();
            let names: Vec<String> = (0..refs.len()).map(|_| "SOLID".to_owned()).collect();
            let text = step::write::write(&refs, &names, colors, materials)?;
            std::fs::write(path, text)
                .map_err(|e| NativeError::Failed(format!("cannot write {}: {e}", path.display())))
        })
    }

    fn export_iges(&self, _shapes: &[ShapeHandle], _path: &Path) -> Result<(), NativeError> {
        Err(NativeError::Unsupported("write IGES files"))
    }

    fn import_step(&mut self, data: &[u8]) -> Result<StepImport, NativeError> {
        guarded(|| {
            let imported = step::read::import(data).map_err(NativeError::Failed)?;
            let shape = self.insert(imported.shape)?;
            Ok(StepImport {
                shape,
                colors: imported.colors,
                material: imported.material,
            })
        })
    }

    fn read_step_assembly(&mut self, data: &[u8]) -> Result<StepAssembly, NativeError> {
        guarded(|| step::read::assembly(data).map_err(NativeError::Failed))
    }

    fn release(&mut self, shape: ShapeHandle) {
        shapes_mut().remove(&shape.raw());
    }

    /// Every native kernel shares the process's shapes, so a worker is just another one.
    fn open_worker() -> Option<Self> {
        Some(NativeKernel::new())
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

    fn cylinder(
        kernel: &mut NativeKernel,
        at: (f64, f64),
        radius: f64,
        z: (f64, f64),
    ) -> ShapeHandle {
        let circle = Profile::from_curves(&[ProfileSegment {
            id: 1,
            curve: Curve2::Circle {
                center: Point2::new(at.0, at.1),
                radius,
            },
        }])
        .unwrap();
        let plane = Plane {
            origin: Point3::new(0.0, 0.0, z.0),
            x_dir: Vec3::X,
            normal: Vec3::Z,
        };
        kernel
            .extrude(&plane, &circle, Vec3::new(0.0, 0.0, z.1 - z.0))
            .unwrap()
            .shape
    }

    #[test]
    fn booleans_with_cylinders_have_the_volumes_they_should() {
        use std::f64::consts::PI;
        let mut kernel = NativeKernel::new();
        let a = block(&mut kernel, (0.0, 0.0, 0.0), (40.0, 30.0, 10.0));
        let through = cylinder(&mut kernel, (20.0, 15.0), 6.0, (-5.0, 15.0));
        let over_edge = cylinder(&mut kernel, (40.0, 15.0), 6.0, (-5.0, 15.0));
        let on_top = cylinder(&mut kernel, (38.0, 15.0), 6.0, (10.0, 15.0));
        let cases = [
            (
                BooleanOp::Cut,
                through,
                12_000.0 - 36.0 * PI * 10.0,
                (7, 15, 10),
            ),
            (
                BooleanOp::Cut,
                over_edge,
                12_000.0 - 18.0 * PI * 10.0,
                (8, 18, 12),
            ),
            (BooleanOp::Common, through, 36.0 * PI * 10.0, (3, 3, 2)),
            (
                BooleanOp::Fuse,
                on_top,
                12_000.0 + 36.0 * PI * 5.0,
                (9, 19, 12),
            ),
        ];
        for (op, tool, volume, (faces, edges, vertices)) in cases {
            let result = kernel.boolean(op, a, tool).unwrap();
            let got = kernel.volume(result.shape).unwrap();
            assert!(
                (got - volume).abs() < 1e-6,
                "{op:?}: {got} instead of {volume}"
            );
            let counts = kernel.topology_counts(result.shape).unwrap();
            assert_eq!(
                (counts.faces, counts.edges, counts.vertices),
                (faces, edges, vertices),
                "{op:?}"
            );
            assert!(result.history.faces.iter().all(|s| !s.is_empty()));
            kernel.release(result.shape);
        }
    }

    #[test]
    fn what_is_not_written_yet_fails_with_a_message() {
        let mut kernel = NativeKernel::new();
        let shape = kernel
            .make_box(Point3::ORIGIN, Vec3::new(1.0, 1.0, 1.0))
            .unwrap();
        assert!(matches!(
            kernel.solid_from_mesh(&[], &[]),
            Err(NativeError::Unsupported(_))
        ));
        let _ = shape;
        assert!(matches!(
            kernel.make_box(Point3::ORIGIN, Vec3::new(1.0, 0.0, 1.0)),
            Err(NativeError::Failed(_))
        ));
    }

    #[test]
    fn a_solid_cut_from_inside_another_leaves_a_void() {
        let mut kernel = NativeKernel::new();
        let outer = kernel
            .make_box(Point3::ORIGIN, Vec3::new(10.0, 10.0, 10.0))
            .unwrap();
        let inner = kernel
            .make_box(Point3::new(4.0, 4.0, 4.0), Vec3::new(2.0, 2.0, 2.0))
            .unwrap();
        let hollow = kernel.boolean(BooleanOp::Cut, outer, inner).unwrap();
        assert!((kernel.volume(hollow.shape).unwrap() - 992.0).abs() < 1e-9);
        let counts = kernel.topology_counts(hollow.shape).unwrap();
        assert_eq!(
            (counts.solids, counts.shells, counts.faces),
            (1, 2, 12),
            "{counts:?}"
        );
        let c = kernel.centre_of_mass(hollow.shape).unwrap();
        assert!(c.distance(Point3::new(5.0, 5.0, 5.0)) < 1e-9, "{c:?}");
        // A ball inside the box, cut from it: a round void.
        let ball = Profile::from_curves(&[
            ProfileSegment {
                id: 1,
                curve: Curve2::Arc {
                    start: Point2::new(5.0, 2.0),
                    mid: Point2::new(8.0, 5.0),
                    end: Point2::new(5.0, 8.0),
                },
            },
            line(2, (5.0, 8.0), (5.0, 2.0)),
        ])
        .unwrap();
        let axis = oxyde_geom::Axis::new(Point3::new(5.0, 5.0, 5.0), Vec3::Y).unwrap();
        let front_at_5 = Plane {
            origin: Point3::new(0.0, 0.0, 5.0),
            ..Plane::FRONT
        };
        let sphere = kernel
            .revolve(&front_at_5, &ball, &axis, std::f64::consts::TAU)
            .unwrap();
        let hollow = kernel.boolean(BooleanOp::Cut, outer, sphere.shape).unwrap();
        let expected = 1000.0 - 4.0 / 3.0 * std::f64::consts::PI * 27.0;
        assert!((kernel.volume(hollow.shape).unwrap() - expected).abs() < 1e-9);
        assert_eq!(kernel.topology_counts(hollow.shape).unwrap().shells, 2);
        // Fused, the ball disappears into the box.
        let fused = kernel
            .boolean(BooleanOp::Fuse, outer, sphere.shape)
            .unwrap();
        assert!((kernel.volume(fused.shape).unwrap() - 1000.0).abs() < 1e-9);
        assert_eq!(kernel.topology_counts(fused.shape).unwrap().faces, 6);
    }

    #[test]
    fn display_meshes_of_revolved_faces_face_out_and_cover_them() {
        let mut kernel = NativeKernel::new();
        let axis = oxyde_geom::Axis::new(Point3::ORIGIN, Vec3::Y).unwrap();
        let ball = Profile::from_curves(&[
            ProfileSegment {
                id: 1,
                curve: Curve2::Arc {
                    start: Point2::new(0.0, -5.0),
                    mid: Point2::new(5.0, 0.0),
                    end: Point2::new(0.0, 5.0),
                },
            },
            line(2, (0.0, 5.0), (0.0, -5.0)),
        ])
        .unwrap();
        let torus = Profile::from_curves(&[ProfileSegment {
            id: 1,
            curve: Curve2::Circle {
                center: Point2::new(10.0, 2.0),
                radius: 3.0,
            },
        }])
        .unwrap();
        let cone = Profile::from_curves(&[
            line(1, (0.0, 0.0), (5.0, 0.0)),
            line(2, (5.0, 0.0), (0.0, 10.0)),
            line(3, (0.0, 10.0), (0.0, 0.0)),
        ])
        .unwrap();
        for (name, profile, angle) in [
            ("ball", &ball, std::f64::consts::TAU),
            ("quarter ball", &ball, std::f64::consts::FRAC_PI_2),
            ("torus", &torus, std::f64::consts::TAU),
            ("cone", &cone, std::f64::consts::TAU),
            ("cone sector", &cone, 2.0),
        ] {
            let solid = kernel
                .revolve(&Plane::FRONT, profile, &axis, angle)
                .unwrap();
            let topology = kernel.topology(solid.shape).unwrap();
            let deflection = 0.01;
            let mesh = kernel.tessellate(solid.shape, deflection).unwrap();
            assert_eq!(mesh.faces.len(), topology.faces.len(), "{name}");
            let centre = kernel.centre_of_mass(solid.shape).unwrap();
            for (f, (face, info)) in mesh.faces.iter().zip(&topology.faces).enumerate() {
                let mut area = 0.0;
                for t in &face.triangles {
                    let p = |k: u32| {
                        let [x, y, z] = face.positions[k as usize];
                        Point3::new(f64::from(x), f64::from(y), f64::from(z))
                    };
                    let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                    let twice = (b - a).cross(c - a);
                    // Every one of these solids is convex: its faces look away from its centre.
                    let middle = Point3::new(
                        (a.x + b.x + c.x) / 3.0,
                        (a.y + b.y + c.y) / 3.0,
                        (a.z + b.z + c.z) / 3.0,
                    );
                    if name != "torus" {
                        assert!(
                            twice.dot(middle - centre) > -1e-9,
                            "{name}, face {f}: a triangle faces inwards"
                        );
                    }
                    area += twice.length() / 2.0;
                }
                // Chords a deflection off the surface both ways round a sphere or a torus
                // lose a share of the area of about twice what they lose round a cylinder.
                assert!(
                    (area - info.area).abs() <= 5e-3 * info.area,
                    "{name}, face {f}: the mesh covers {area} mm², the face {}",
                    info.area
                );
            }
            kernel.release(solid.shape);
        }
    }
}
