//! The `Kernel` trait: the boundary between Oxyde's modeling logic and a B-rep geometry kernel.
//!
//! Solid operations, topology queries, operation history and STEP/BREP exchange.
//! See `docs/ARCHITECTURE.md` §4.
//!
//! The trait grows milestone by milestone; M1 covers only what is needed to build a
//! primitive solid, inspect it and export it to STEP.

use std::path::Path;

use oxyde_geom::{Point3, Vec3};

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

/// A B-rep geometry kernel.
pub trait Kernel {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Axis-aligned box with one corner at `corner` and extents `size` (all components > 0).
    fn make_box(&mut self, corner: Point3, size: Vec3) -> Result<ShapeHandle, Self::Error>;

    /// Volume of a solid, in mm³.
    fn volume(&self, shape: ShapeHandle) -> Result<f64, Self::Error>;

    fn topology_counts(&self, shape: ShapeHandle) -> Result<TopologyCounts, Self::Error>;

    /// Writes `shapes` to a STEP file at `path`, in millimetres.
    fn export_step(&self, shapes: &[ShapeHandle], path: &Path) -> Result<(), Self::Error>;

    /// Frees a shape. Releasing an unknown or already released handle does nothing.
    fn release(&mut self, shape: ShapeHandle);
}
