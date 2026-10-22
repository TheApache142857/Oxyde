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
//! This is the skeleton: every operation answers [`NativeError::Unsupported`] until it is
//! written, so a caller sees a failing feature with a message, never a crash.

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

use std::fmt;
use std::path::Path;

use oxyde_geom::{Axis, Path as SweepPath, Plane, Point3, Profile, Transform, Vec3};
use oxyde_kernel::{
    BooleanOp, Kernel, Mesh, OpResult, ShapeHandle, StepAssembly, StepColors, StepImport,
    StepMaterial, Topology, TopologyCounts,
};

/// Error reported by the native kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeError {
    /// The operation is not written yet; the name is the [`Kernel`] method's.
    Unsupported(&'static str),
}

impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NativeError::Unsupported(operation) => {
                write!(f, "the native kernel cannot {operation} yet")
            }
        }
    }
}

impl std::error::Error for NativeError {}

/// A [`Kernel`] written in Rust, with no C++ underneath.
#[derive(Debug, Default)]
pub struct NativeKernel {}

impl NativeKernel {
    /// A kernel holding no shapes.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Kernel for NativeKernel {
    type Error = NativeError;

    fn make_box(&mut self, _corner: Point3, _size: Vec3) -> Result<ShapeHandle, NativeError> {
        Err(NativeError::Unsupported("make_box"))
    }

    fn extrude(
        &mut self,
        _plane: &Plane,
        _profile: &Profile,
        _vector: Vec3,
    ) -> Result<OpResult, NativeError> {
        Err(NativeError::Unsupported("extrude"))
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
        _shape: ShapeHandle,
        _transform: &Transform,
    ) -> Result<OpResult, NativeError> {
        Err(NativeError::Unsupported("transform"))
    }

    fn boolean_many(
        &mut self,
        _op: BooleanOp,
        _target: ShapeHandle,
        _tools: &[ShapeHandle],
    ) -> Result<OpResult, NativeError> {
        Err(NativeError::Unsupported("boolean"))
    }

    fn solid_from_mesh(
        &mut self,
        _points: &[[f64; 3]],
        _triangles: &[[u32; 3]],
    ) -> Result<OpResult, NativeError> {
        Err(NativeError::Unsupported("solid_from_mesh"))
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

    fn topology(&self, _shape: ShapeHandle) -> Result<Topology, NativeError> {
        Err(NativeError::Unsupported("topology"))
    }

    fn tessellate(&self, _shape: ShapeHandle, _deflection: f64) -> Result<Mesh, NativeError> {
        Err(NativeError::Unsupported("tessellate"))
    }

    fn volume(&self, _shape: ShapeHandle) -> Result<f64, NativeError> {
        Err(NativeError::Unsupported("volume"))
    }

    fn centre_of_mass(&self, _shape: ShapeHandle) -> Result<Point3, NativeError> {
        Err(NativeError::Unsupported("centre_of_mass"))
    }

    fn bounds(&self, _shape: ShapeHandle) -> Result<(Point3, Point3), NativeError> {
        Err(NativeError::Unsupported("bounds"))
    }

    fn topology_counts(&self, _shape: ShapeHandle) -> Result<TopologyCounts, NativeError> {
        Err(NativeError::Unsupported("topology_counts"))
    }

    fn export_step(
        &self,
        _shapes: &[ShapeHandle],
        _colors: &[StepColors],
        _materials: &[Option<StepMaterial>],
        _path: &Path,
    ) -> Result<(), NativeError> {
        Err(NativeError::Unsupported("export_step"))
    }

    fn export_iges(&self, _shapes: &[ShapeHandle], _path: &Path) -> Result<(), NativeError> {
        Err(NativeError::Unsupported("export_iges"))
    }

    fn import_step(&mut self, _data: &[u8]) -> Result<StepImport, NativeError> {
        Err(NativeError::Unsupported("import_step"))
    }

    fn read_step_assembly(&mut self, _data: &[u8]) -> Result<StepAssembly, NativeError> {
        Err(NativeError::Unsupported("read_step_assembly"))
    }

    fn release(&mut self, _shape: ShapeHandle) {
        // No shape is ever made yet, so there is nothing to free.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwritten_operations_fail_with_their_name() {
        let mut kernel = NativeKernel::new();
        let error = kernel
            .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0))
            .unwrap_err();
        assert_eq!(error, NativeError::Unsupported("make_box"));
        assert_eq!(error.to_string(), "the native kernel cannot make_box yet");
        let shape = ShapeHandle::from_raw(1);
        assert!(kernel.volume(shape).is_err());
        assert!(kernel.tessellate(shape, 0.1).is_err());
        // Releasing a handle the kernel never made does nothing.
        kernel.release(shape);
    }
}
