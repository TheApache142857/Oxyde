//! OpenCASCADE backend for `oxyde-kernel`.
//!
//! Talks to OCCT through a thin C++ shim (`cpp/`) bridged with `cxx`. No OCCT type crosses
//! the boundary: Rust holds opaque shape handles. See `docs/ARCHITECTURE.md` §3.

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

mod ffi;

use std::fmt;
use std::path::Path;

use oxyde_geom::{
    Axis, Curve2, Loop, Path as SweepPath, Plane, Point3, Profile, ProfileSegment, Transform, Vec3,
};
use oxyde_kernel::{
    BooleanOp, CurveKind, EdgeInfo, FaceInfo, FaceMesh, FaceSource, Kernel, Mesh, OpHistory,
    OpResult, ShapeHandle, StepAssembly, StepColor, StepColors, StepImport, StepInstance,
    StepMaterial, StepNode, StepNodeContent, SurfaceKind, Topology, TopologyCounts, VertexInfo,
};

/// Error reported by OCCT or by the shim around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcctError {
    /// OpenCASCADE refused or failed the operation (its exception message), or the shim
    /// found the arguments unusable.
    Occt(String),
    /// A Rust panic inside the kernel call, caught at the `Kernel` boundary: a bug, not a
    /// geometric failure. The message is the panic's.
    Internal(String),
}

impl OcctError {
    pub fn message(&self) -> &str {
        match self {
            OcctError::Occt(message) | OcctError::Internal(message) => message,
        }
    }

    /// Whether this is a caught panic rather than a geometric failure.
    pub fn is_internal(&self) -> bool {
        matches!(self, OcctError::Internal(_))
    }
}

impl fmt::Display for OcctError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OcctError::Occt(message) => write!(f, "OpenCASCADE: {message}"),
            OcctError::Internal(message) => write!(f, "internal error in the kernel: {message}"),
        }
    }
}

impl std::error::Error for OcctError {}

impl From<cxx::Exception> for OcctError {
    fn from(e: cxx::Exception) -> Self {
        Self::Occt(e.what().to_owned())
    }
}

/// A [`Kernel`] backed by OpenCASCADE.
///
/// Not `Send`: a kernel stays on the thread that created it. Its shapes do not: they live
/// in one arena shared by every kernel of the process, so another thread opens a kernel of
/// its own ([`Kernel::open_worker`]) and the handles it makes are valid everywhere. OCCT's
/// modelling algorithms are safe on different shapes from different threads; its STEP
/// translators are not, and the shim runs them one at a time (`docs/ARCHITECTURE.md` §3).
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

/// Runs one kernel operation with a net under it: a Rust panic inside (an index out of
/// range in the data marshalled to or from the shim, say) comes back as
/// [`OcctError::Internal`] instead of unwinding into the caller. OCCT's own exceptions
/// never get this far: the shim turns them into `Err` already.
fn guarded<T>(f: impl FnOnce() -> Result<T, OcctError>) -> Result<T, OcctError> {
    oxyde_kernel::catch_panic(f).unwrap_or_else(|message| Err(OcctError::Internal(message)))
}

impl Kernel for OcctKernel {
    type Error = OcctError;

    fn make_box(&mut self, corner: Point3, size: Vec3) -> Result<ShapeHandle, OcctError> {
        guarded(|| {
            let id = self
                .session
                .pin_mut()
                .make_box(corner.x, corner.y, corner.z, size.x, size.y, size.z)?;
            Ok(ShapeHandle::from_raw(id))
        })
    }

    fn extrude(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        vector: Vec3,
    ) -> Result<OpResult, OcctError> {
        guarded(|| {
            let regions = regions_data(profile);
            let data = self.session.pin_mut().extrude(
                &plane_data(plane),
                &regions,
                vector.x,
                vector.y,
                vector.z,
            )?;
            Ok(op_result(data))
        })
    }

    fn revolve(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        axis: &Axis,
        angle: f64,
    ) -> Result<OpResult, OcctError> {
        guarded(|| {
            let regions = regions_data(profile);
            let data = self.session.pin_mut().revolve(
                &plane_data(plane),
                &regions,
                &axis_data(axis),
                angle,
            )?;
            Ok(op_result(data))
        })
    }

    fn sweep(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        path_plane: &Plane,
        path: &SweepPath,
    ) -> Result<OpResult, OcctError> {
        guarded(|| {
            let regions = regions_data(profile);
            let segments: Vec<ffi::SegmentData> = path.segments.iter().map(segment_data).collect();
            let data = self.session.pin_mut().sweep(
                &plane_data(plane),
                &regions,
                &plane_data(path_plane),
                &segments,
            )?;
            Ok(op_result(data))
        })
    }

    fn shell(
        &mut self,
        shape: ShapeHandle,
        removed_faces: &[u32],
        offset: f64,
    ) -> Result<OpResult, OcctError> {
        guarded(|| {
            let data = self
                .session
                .pin_mut()
                .shell(shape.raw(), removed_faces, offset)?;
            Ok(op_result(data))
        })
    }

    fn transform(
        &mut self,
        shape: ShapeHandle,
        transform: &Transform,
    ) -> Result<OpResult, OcctError> {
        guarded(|| {
            let rows: Vec<f64> = transform.rows.iter().flatten().copied().collect();
            let data = self.session.pin_mut().transform(shape.raw(), &rows)?;
            Ok(op_result(data))
        })
    }

    fn boolean_many(
        &mut self,
        op: BooleanOp,
        target: ShapeHandle,
        tools: &[ShapeHandle],
    ) -> Result<OpResult, OcctError> {
        guarded(|| {
            let kind = match op {
                BooleanOp::Fuse => 0,
                BooleanOp::Cut => 1,
                BooleanOp::Common => 2,
            };
            let tools: Vec<u64> = tools.iter().map(|t| t.raw()).collect();
            let data = self
                .session
                .pin_mut()
                .boolean_op(kind, target.raw(), &tools)?;
            Ok(op_result(data))
        })
    }

    fn fillet(
        &mut self,
        shape: ShapeHandle,
        edges: &[u32],
        radius: f64,
    ) -> Result<OpResult, OcctError> {
        guarded(|| {
            let data = self.session.pin_mut().fillet(shape.raw(), edges, radius)?;
            Ok(op_result(data))
        })
    }

    fn chamfer(
        &mut self,
        shape: ShapeHandle,
        edges: &[u32],
        distance: f64,
    ) -> Result<OpResult, OcctError> {
        guarded(|| {
            let data = self
                .session
                .pin_mut()
                .chamfer(shape.raw(), edges, distance)?;
            Ok(op_result(data))
        })
    }

    fn topology(&self, shape: ShapeHandle) -> Result<Topology, OcctError> {
        guarded(|| Ok(topology_from(self.session.topology(shape.raw())?)))
    }

    fn tessellate(&self, shape: ShapeHandle, deflection: f64) -> Result<Mesh, OcctError> {
        guarded(|| Ok(mesh_from(self.session.tessellate(shape.raw(), deflection)?)))
    }

    fn tessellate_for_export(
        &self,
        shape: ShapeHandle,
        deflection: f64,
        angle: f64,
    ) -> Result<Mesh, OcctError> {
        guarded(|| {
            Ok(mesh_from(self.session.tessellate_with(
                shape.raw(),
                deflection,
                angle,
            )?))
        })
    }

    fn volume(&self, shape: ShapeHandle) -> Result<f64, OcctError> {
        guarded(|| Ok(self.session.volume(shape.raw())?))
    }

    fn centre_of_mass(&self, shape: ShapeHandle) -> Result<Point3, OcctError> {
        let c = self.session.centre_of_mass(shape.raw())?;
        Ok(Point3::new(c.x, c.y, c.z))
    }

    fn bounds(&self, shape: ShapeHandle) -> Result<(Point3, Point3), OcctError> {
        guarded(|| {
            let b = self.session.bounds(shape.raw())?;
            Ok((
                Point3::new(b.xmin, b.ymin, b.zmin),
                Point3::new(b.xmax, b.ymax, b.zmax),
            ))
        })
    }

    fn topology_counts(&self, shape: ShapeHandle) -> Result<TopologyCounts, OcctError> {
        guarded(|| {
            let c = self.session.topology_counts(shape.raw())?;
            Ok(TopologyCounts {
                solids: c.solids as usize,
                shells: c.shells as usize,
                faces: c.faces as usize,
                edges: c.edges as usize,
                vertices: c.vertices as usize,
            })
        })
    }

    fn export_step(
        &self,
        shapes: &[ShapeHandle],
        colors: &[StepColors],
        materials: &[Option<StepMaterial>],
        path: &Path,
    ) -> Result<(), OcctError> {
        guarded(|| {
            let path = path.to_str().ok_or_else(|| {
                OcctError::Occt(format!("path is not valid UTF-8: {}", path.display()))
            })?;
            if !colors.is_empty() && colors.len() != shapes.len() {
                return Err(OcctError::Occt(format!(
                    "{} colour sets for {} shapes",
                    colors.len(),
                    shapes.len()
                )));
            }
            if !materials.is_empty() && materials.len() != shapes.len() {
                return Err(OcctError::Occt(format!(
                    "{} materials for {} shapes",
                    materials.len(),
                    shapes.len()
                )));
            }
            let ids: Vec<u64> = shapes.iter().map(|s| s.raw()).collect();
            let colors: Vec<ffi::StepShapeColors> = colors.iter().map(shape_colors_data).collect();
            let materials: Vec<ffi::StepMaterialData> = materials
                .iter()
                .map(|m| material_data(m.as_ref()))
                .collect();
            self.session.export_step(&ids, &colors, &materials, path)?;
            Ok(())
        })
    }

    fn export_iges(&self, shapes: &[ShapeHandle], path: &Path) -> Result<(), OcctError> {
        guarded(|| {
            let path = path.to_str().ok_or_else(|| {
                OcctError::Occt(format!("path is not valid UTF-8: {}", path.display()))
            })?;
            let ids: Vec<u64> = shapes.iter().map(|s| s.raw()).collect();
            self.session.export_iges(&ids, path)?;
            Ok(())
        })
    }

    fn import_step(&mut self, data: &[u8]) -> Result<StepImport, OcctError> {
        guarded(|| {
            let imported = self.session.pin_mut().import_step(data)?;
            Ok(StepImport {
                shape: ShapeHandle::from_raw(imported.shape),
                colors: shape_colors(imported.colors),
                material: material_from(imported.material),
            })
        })
    }

    fn read_step_assembly(&mut self, data: &[u8]) -> Result<StepAssembly, OcctError> {
        guarded(|| {
            let tree = self.session.pin_mut().read_step_assembly(data)?;
            step_assembly(tree)
        })
    }

    fn release(&mut self, shape: ShapeHandle) {
        // Nothing to report to: a shape that could not be freed is leaked, not fatal.
        let _ = oxyde_kernel::catch_panic(|| self.session.pin_mut().release(shape.raw()));
    }

    fn open_worker() -> Option<Self> {
        oxyde_kernel::catch_panic(Self::new).ok()
    }
}

// ---- STEP colours --------------------------------------------------------------------------

fn color_data(color: Option<StepColor>) -> ffi::StepColorData {
    match color {
        Some(c) => ffi::StepColorData {
            present: true,
            r: c.rgb[0],
            g: c.rgb[1],
            b: c.rgb[2],
            transparency: c.transparency,
        },
        None => ffi::StepColorData {
            present: false,
            r: 0,
            g: 0,
            b: 0,
            transparency: 0.0,
        },
    }
}

fn color_from(data: &ffi::StepColorData) -> Option<StepColor> {
    data.present.then(|| StepColor {
        rgb: [data.r, data.g, data.b],
        transparency: data.transparency.clamp(0.0, 1.0),
    })
}

fn material_data(material: Option<&StepMaterial>) -> ffi::StepMaterialData {
    match material {
        Some(m) => ffi::StepMaterialData {
            present: true,
            name: m.name.clone(),
            description: m.description.clone(),
            density: if m.density.is_finite() && m.density > 0.0 {
                m.density
            } else {
                0.0
            },
        },
        None => ffi::StepMaterialData {
            present: false,
            name: String::new(),
            description: String::new(),
            density: 0.0,
        },
    }
}

/// The material the reader found, its density settled in kg/m³.
///
/// A STEP file carries the density with its unit (Fusion 360 and SolidWorks write
/// kilograms per cubic metre) and OpenCASCADE's reader converts it to its own grams per
/// cubic millimetre: steel's 7850 kg/m³ arrives as 0.00785. A file OpenCASCADE itself
/// wrote comes back unconverted. The unit is told from the size of the number, which works
/// for every material denser than air and lighter than anything (10 to 30 000 kg/m³):
/// under 0.03 it is g/mm³, under 50 g/cm³, above kg/m³.
fn material_from(data: ffi::StepMaterialData) -> Option<StepMaterial> {
    if !data.present || data.name.trim().is_empty() {
        return None;
    }
    let density = density_kg_per_m3(data.density);
    Some(StepMaterial {
        name: data.name,
        description: data.description,
        density,
    })
}

/// A density as a STEP reader may give it (g/mm³, g/cm³ or kg/m³), in kg/m³; 0 when
/// unknown or absurd.
fn density_kg_per_m3(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        0.0
    } else if value < 0.03 {
        value * 1e6
    } else if value < 50.0 {
        value * 1e3
    } else {
        value
    }
}

fn shape_colors_data(colors: &StepColors) -> ffi::StepShapeColors {
    ffi::StepShapeColors {
        solids: colors.solids.iter().map(|c| color_data(*c)).collect(),
        faces: colors.faces.iter().map(|c| color_data(*c)).collect(),
    }
}

fn shape_colors(data: ffi::StepShapeColors) -> StepColors {
    StepColors {
        solids: data.solids.iter().map(color_from).collect(),
        faces: data.faces.iter().map(color_from).collect(),
    }
}

// ---- C++ → Rust: STEP assemblies ----------------------------------------------------------

fn step_assembly(tree: ffi::StepAssemblyData) -> Result<StepAssembly, OcctError> {
    let count = tree.nodes.len();
    let index = |i: u32| -> Result<usize, OcctError> {
        let i = i as usize;
        if i < count {
            Ok(i)
        } else {
            Err(OcctError::Occt(format!("STEP node {i} out of {count}")))
        }
    };
    let root = index(tree.root)?;
    let mut nodes = Vec::with_capacity(count);
    for node in tree.nodes {
        let content = if node.is_assembly {
            let mut instances = Vec::with_capacity(node.instances.len());
            for instance in node.instances {
                let r = &instance.rows;
                if r.len() != 12 {
                    return Err(OcctError::Occt(format!(
                        "placement with {} values",
                        r.len()
                    )));
                }
                instances.push(StepInstance {
                    node: index(instance.node)?,
                    name: instance.name,
                    transform: Transform {
                        rows: [
                            [r[0], r[1], r[2], r[3]],
                            [r[4], r[5], r[6], r[7]],
                            [r[8], r[9], r[10], r[11]],
                        ],
                    },
                });
            }
            StepNodeContent::Assembly { instances }
        } else {
            StepNodeContent::Part {
                step: node.step,
                material: material_from(node.material),
            }
        };
        nodes.push(StepNode {
            name: node.name,
            content,
        });
    }
    Ok(StepAssembly { nodes, root })
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

fn axis_data(a: &Axis) -> ffi::AxisData {
    ffi::AxisData {
        ox: a.origin.x,
        oy: a.origin.y,
        oz: a.origin.z,
        dx: a.direction.x,
        dy: a.direction.y,
        dz: a.direction.z,
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
        ffi::SourceKind::OffsetFace => FaceSource::OffsetFace {
            input: s.input,
            face: s.index,
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
                axis: axis_from(f.has_axis, &f.axis),
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
                axis: axis_from(e.has_axis, &e.axis),
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

fn axis_from(present: bool, a: &ffi::AxisData) -> Option<Axis> {
    if !present {
        return None;
    }
    Axis::new(Point3::new(a.ox, a.oy, a.oz), Vec3::new(a.dx, a.dy, a.dz))
}

fn points3(flat: &[f32]) -> Vec<[f32; 3]> {
    flat.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect()
}

fn mesh_from(data: ffi::MeshData) -> Mesh {
    Mesh {
        faces: data
            .faces
            .iter()
            .map(|f| FaceMesh {
                positions: points3(&f.positions),
                triangles: f
                    .indices
                    .chunks_exact(3)
                    .map(|t| [t[0], t[1], t[2]])
                    .collect(),
            })
            .collect(),
        edges: data.edges.iter().map(|e| points3(&e.points)).collect(),
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

#[cfg(test)]
mod tests {
    use super::density_kg_per_m3;

    #[test]
    fn densities_come_back_in_kilograms_per_cubic_metre() {
        // Steel, as OpenCASCADE converts a Fusion 360 file (g/mm³), as g/cm³, and as is.
        assert!((density_kg_per_m3(0.00785) - 7850.0).abs() < 1e-6);
        assert!((density_kg_per_m3(7.85) - 7850.0).abs() < 1e-9);
        assert!((density_kg_per_m3(7850.0) - 7850.0).abs() < 1e-9);
        // Water and a light foam.
        assert!((density_kg_per_m3(0.0009983) - 998.3).abs() < 1e-6);
        assert!((density_kg_per_m3(0.00003) - 30.0).abs() < 1e-6);
        assert_eq!(density_kg_per_m3(0.0), 0.0);
        assert_eq!(density_kg_per_m3(f64::NAN), 0.0);
    }
}
