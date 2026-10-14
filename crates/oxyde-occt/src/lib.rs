//! OpenCASCADE backend for `oxyde-kernel`.
//!
//! Talks to OCCT through a thin C++ shim (`cpp/`) bridged with `cxx`. No OCCT type crosses
//! the boundary: Rust holds opaque shape handles. See `docs/ARCHITECTURE.md` §3.

mod ffi;

use std::fmt;
use std::path::Path;

use oxyde_geom::{Curve2, Loop, Plane, Point3, Profile, ProfileSegment, Vec3};
use oxyde_kernel::{
    BooleanOp, CurveKind, EdgeInfo, FaceInfo, FaceSource, Kernel, OpHistory, OpResult, ShapeHandle,
    SurfaceKind, Topology, TopologyCounts, VertexInfo,
};

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

    fn extrude(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        vector: Vec3,
    ) -> Result<OpResult, OcctError> {
        let regions = regions_data(profile);
        let data = self.session.pin_mut().extrude(
            &plane_data(plane),
            &regions,
            vector.x,
            vector.y,
            vector.z,
        )?;
        Ok(op_result(data))
    }

    fn boolean(
        &mut self,
        op: BooleanOp,
        target: ShapeHandle,
        tool: ShapeHandle,
    ) -> Result<OpResult, OcctError> {
        let kind = match op {
            BooleanOp::Fuse => 0,
            BooleanOp::Cut => 1,
            BooleanOp::Common => 2,
        };
        let data = self
            .session
            .pin_mut()
            .boolean_op(kind, target.raw(), tool.raw())?;
        Ok(op_result(data))
    }

    fn fillet(
        &mut self,
        shape: ShapeHandle,
        edges: &[u32],
        radius: f64,
    ) -> Result<OpResult, OcctError> {
        let data = self.session.pin_mut().fillet(shape.raw(), edges, radius)?;
        Ok(op_result(data))
    }

    fn chamfer(
        &mut self,
        shape: ShapeHandle,
        edges: &[u32],
        distance: f64,
    ) -> Result<OpResult, OcctError> {
        let data = self
            .session
            .pin_mut()
            .chamfer(shape.raw(), edges, distance)?;
        Ok(op_result(data))
    }

    fn topology(&self, shape: ShapeHandle) -> Result<Topology, OcctError> {
        Ok(topology_from(self.session.topology(shape.raw())?))
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

// ---- Rust → C++ ----------------------------------------------------------------------------

fn plane_data(p: &Plane) -> ffi::PlaneData {
    ffi::PlaneData {
        ox: p.origin.x,
        oy: p.origin.y,
        oz: p.origin.z,
        xx: p.x_dir.x,
        xy: p.x_dir.y,
        xz: p.x_dir.z,
        nx: p.normal.x,
        ny: p.normal.y,
        nz: p.normal.z,
    }
}

fn segment_data(segment: &ProfileSegment) -> ffi::SegmentData {
    let mut data = ffi::SegmentData {
        id: segment.id,
        kind: ffi::SegmentKind::Line,
        x0: 0.0,
        y0: 0.0,
        x1: 0.0,
        y1: 0.0,
        x2: 0.0,
        y2: 0.0,
        r: 0.0,
    };
    match segment.curve {
        Curve2::Line { start, end } => {
            data.kind = ffi::SegmentKind::Line;
            (data.x0, data.y0, data.x1, data.y1) = (start.x, start.y, end.x, end.y);
        }
        Curve2::Arc { start, mid, end } => {
            data.kind = ffi::SegmentKind::Arc;
            (data.x0, data.y0, data.x1, data.y1) = (start.x, start.y, mid.x, mid.y);
            (data.x2, data.y2) = (end.x, end.y);
        }
        Curve2::Circle { center, radius } => {
            data.kind = ffi::SegmentKind::Circle;
            (data.x0, data.y0, data.r) = (center.x, center.y, radius);
        }
    }
    data
}

fn loop_data(l: &Loop) -> ffi::LoopData {
    ffi::LoopData {
        segments: l.segments.iter().map(segment_data).collect(),
    }
}

fn regions_data(profile: &Profile) -> Vec<ffi::RegionData> {
    profile
        .regions
        .iter()
        .map(|region| ffi::RegionData {
            loops: std::iter::once(&region.outer)
                .chain(&region.holes)
                .map(loop_data)
                .collect(),
        })
        .collect()
}

// ---- C++ → Rust ----------------------------------------------------------------------------

fn op_result(data: ffi::OpResultData) -> OpResult {
    OpResult {
        shape: ShapeHandle::from_raw(data.shape),
        history: OpHistory {
            faces: data
                .faces
                .into_iter()
                .map(|face| face.sources.iter().filter_map(face_source).collect())
                .collect(),
        },
    }
}

fn face_source(s: &ffi::FaceSourceData) -> Option<FaceSource> {
    Some(match s.kind {
        ffi::SourceKind::InputFace => FaceSource::InputFace {
            input: s.input,
            face: s.index,
        },
        ffi::SourceKind::InputEdge => FaceSource::InputEdge {
            input: s.input,
            edge: s.index,
        },
        ffi::SourceKind::InputVertex => FaceSource::InputVertex {
            input: s.input,
            vertex: s.index,
        },
        ffi::SourceKind::ProfileSegment => FaceSource::ProfileSegment(s.index),
        ffi::SourceKind::StartCap => FaceSource::StartCap,
        ffi::SourceKind::EndCap => FaceSource::EndCap,
        _ => return None,
    })
}

fn topology_from(data: ffi::TopologyData) -> Topology {
    Topology {
        faces: data
            .faces
            .into_iter()
            .map(|f| FaceInfo {
                surface: surface_kind(f.surface),
                area: f.area,
                centroid: Point3::new(f.cx, f.cy, f.cz),
                normal: f.has_normal.then(|| Vec3::new(f.nx, f.ny, f.nz)),
                edges: f.edges,
            })
            .collect(),
        edges: data
            .edges
            .into_iter()
            .map(|e| EdgeInfo {
                curve: curve_kind(e.curve),
                length: e.length,
                midpoint: Point3::new(e.mx, e.my, e.mz),
                vertices: e.vertices,
                faces: e.faces,
            })
            .collect(),
        vertices: data
            .vertices
            .into_iter()
            .map(|v| VertexInfo {
                point: Point3::new(v.x, v.y, v.z),
            })
            .collect(),
    }
}

fn surface_kind(kind: ffi::SurfaceKind) -> SurfaceKind {
    match kind {
        ffi::SurfaceKind::Plane => SurfaceKind::Plane,
        ffi::SurfaceKind::Cylinder => SurfaceKind::Cylinder,
        ffi::SurfaceKind::Cone => SurfaceKind::Cone,
        ffi::SurfaceKind::Sphere => SurfaceKind::Sphere,
        ffi::SurfaceKind::Torus => SurfaceKind::Torus,
        ffi::SurfaceKind::BSpline => SurfaceKind::BSpline,
        _ => SurfaceKind::Other,
    }
}

fn curve_kind(kind: ffi::CurveKind) -> CurveKind {
    match kind {
        ffi::CurveKind::Line => CurveKind::Line,
        ffi::CurveKind::Circle => CurveKind::Circle,
        ffi::CurveKind::Ellipse => CurveKind::Ellipse,
        ffi::CurveKind::BSpline => CurveKind::BSpline,
        ffi::CurveKind::Degenerate => CurveKind::Degenerate,
        _ => CurveKind::Other,
    }
}
