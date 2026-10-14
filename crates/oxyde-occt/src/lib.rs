//! OpenCASCADE backend for `oxyde-kernel`.
//!
//! Talks to OCCT through a thin C++ shim (`cpp/`) bridged with `cxx`. No OCCT type crosses
//! the boundary: Rust holds opaque shape handles. See `docs/ARCHITECTURE.md` §3.

mod ffi;

use std::fmt;
use std::path::Path;

use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{Kernel, ShapeHandle, TopologyCounts};

/// Error reported by OCCT or by the shim around it.
#[derive(Debug, Clone)]
pub struct OcctError(String);

impl OcctError {
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OcctError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OpenCASCADE: {}", self.0)
    }
}

impl std::error::Error for OcctError {}

impl From<cxx::Exception> for OcctError {
    fn from(e: cxx::Exception) -> Self {
        Self(e.what().to_owned())
    }
}

/// A [`Kernel`] backed by OpenCASCADE.
///
/// Not `Send`: OCCT calls stay on the thread that created the kernel. A dedicated kernel
/// thread will be introduced with the application (see `docs/ARCHITECTURE.md` §3).
pub struct OcctKernel {
    session: cxx::UniquePtr<ffi::Session>,
}

impl OcctKernel {
    pub fn new() -> Self {
        Self {
            session: ffi::new_session(),
        }
    }

    /// Version of OpenCASCADE this crate was compiled against, e.g. `"7.8.1"`.
    pub fn occt_version() -> String {
        ffi::occt_version()
    }
}

impl Default for OcctKernel {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel for OcctKernel {
    type Error = OcctError;

    fn make_box(&mut self, corner: Point3, size: Vec3) -> Result<ShapeHandle, OcctError> {
        let id = self
            .session
            .pin_mut()
            .make_box(corner.x, corner.y, corner.z, size.x, size.y, size.z)?;
        Ok(ShapeHandle::from_raw(id))
    }

    fn volume(&self, shape: ShapeHandle) -> Result<f64, OcctError> {
        Ok(self.session.volume(shape.raw())?)
    }

    fn topology_counts(&self, shape: ShapeHandle) -> Result<TopologyCounts, OcctError> {
        let c = self.session.topology_counts(shape.raw())?;
        Ok(TopologyCounts {
            solids: c.solids as usize,
            shells: c.shells as usize,
            faces: c.faces as usize,
            edges: c.edges as usize,
            vertices: c.vertices as usize,
        })
    }

    fn export_step(&self, shapes: &[ShapeHandle], path: &Path) -> Result<(), OcctError> {
        let path = path
            .to_str()
            .ok_or_else(|| OcctError(format!("path is not valid UTF-8: {}", path.display())))?;
        let ids: Vec<u64> = shapes.iter().map(|s| s.raw()).collect();
        self.session.export_step(&ids, path)?;
        Ok(())
    }

    fn release(&mut self, shape: ShapeHandle) {
        self.session.pin_mut().release(shape.raw());
    }
}
