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

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use oxyde_geom::{
    Axis, Curve2, Loop, Path as SweepPath, Plane, Point2, Point3, Profile, ProfileSegment,
    Transform, Vec3,
};
use oxyde_kernel::{
    BooleanOp, ChamferSecond, CurveKind, Distance, EdgeInfo, FaceInfo, FaceMesh, FaceSource,
    Kernel, Mesh, MeshWatch, OpHistory, OpResult, ProjectedEdges, ShapeHandle, StepAssembly,
    StepColor, StepColors, StepImport, StepInstance, StepMaterial, StepNode, StepNodeContent,
    SubShape, SurfaceKind, Topology, TopologyCounts, VertexInfo, VolumeMesh, VolumeMeshError,
    VolumeMeshOptions,
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

    /// Version of the Netgen volume mesher built in, e.g. `"6.2.2401"`; `None` when this
    /// build has none.
    pub fn netgen_version() -> Option<String> {
        ffi::has_netgen().then(ffi::netgen_version)
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

    fn loft(
        &mut self,
        sections: &[(Plane, Loop)],
        ruled: bool,
    ) -> Result<Option<OpResult>, OcctError> {
        guarded(|| {
            let data: Vec<ffi::LoftSectionData> = sections
                .iter()
                .map(|(plane, contour)| ffi::LoftSectionData {
                    plane: plane_data(plane),
                    region: ffi::RegionData {
                        loops: vec![loop_data(contour)],
                    },
                })
                .collect();
            let result = self.session.pin_mut().loft(&data, ruled)?;
            Ok(Some(op_result(result)))
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

    fn solid_from_mesh(
        &mut self,
        points: &[[f64; 3]],
        triangles: &[[u32; 3]],
    ) -> Result<OpResult, OcctError> {
        guarded(|| {
            let points: Vec<f64> = points.iter().flatten().copied().collect();
            let triangles: Vec<u32> = triangles.iter().flatten().copied().collect();
            let data = self
                .session
                .pin_mut()
                .solid_from_mesh(&points, &triangles)?;
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

    fn chamfer_with(
        &mut self,
        shape: ShapeHandle,
        edges: &[(u32, u32)],
        first: f64,
        second: ChamferSecond,
    ) -> Result<Option<OpResult>, OcctError> {
        let (kind, value) = match second {
            ChamferSecond::Distance(d) => (0, d),
            ChamferSecond::Angle(a) => (1, a),
        };
        let (edge_ids, face_ids): (Vec<u32>, Vec<u32>) = edges.iter().copied().unzip();
        guarded(|| {
            let data = self.session.pin_mut().chamfer_with(
                shape.raw(),
                &edge_ids,
                &face_ids,
                first,
                kind,
                value,
            )?;
            Ok(Some(op_result(data)))
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

    fn distance(
        &self,
        a: ShapeHandle,
        a_part: SubShape,
        b: ShapeHandle,
        b_part: SubShape,
    ) -> Result<Option<Distance>, OcctError> {
        let part = |p: SubShape| match p {
            SubShape::Whole => (0, 0),
            SubShape::Face(i) => (1, i),
            SubShape::Edge(i) => (2, i),
            SubShape::Vertex(i) => (3, i),
        };
        guarded(|| {
            let ((ak, ai), (bk, bi)) = (part(a_part), part(b_part));
            let d = self.session.distance(a.raw(), ak, ai, b.raw(), bk, bi)?;
            Ok(Some(Distance {
                distance: d.distance,
                on_a: Point3::new(d.ax, d.ay, d.az),
                on_b: Point3::new(d.bx, d.by, d.bz),
            }))
        })
    }

    fn project_edges(
        &self,
        shapes: &[ShapeHandle],
        view: &Plane,
        deflection: f64,
    ) -> Result<Option<ProjectedEdges>, OcctError> {
        guarded(|| {
            let raw: Vec<u64> = shapes.iter().map(|s| s.raw()).collect();
            let data = self
                .session
                .project_edges(&raw, &plane_data(view), deflection.max(1e-4))?;
            let lines = |polylines: &[ffi::PolylineData]| -> Vec<Vec<Point2>> {
                polylines
                    .iter()
                    .map(|l| {
                        let xs = l.points.iter().step_by(2);
                        let ys = l.points.iter().skip(1).step_by(2);
                        xs.zip(ys).map(|(x, y)| Point2::new(*x, *y)).collect()
                    })
                    .collect()
            };
            Ok(Some(ProjectedEdges {
                visible: lines(&data.visible),
                hidden: lines(&data.hidden),
            }))
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

    fn has_volume_mesher(&self) -> bool {
        ffi::has_netgen()
    }

    fn volume_mesh(
        &self,
        shape: ShapeHandle,
        options: &VolumeMeshOptions,
    ) -> Result<VolumeMesh, VolumeMeshError> {
        let cancel = AtomicBool::new(false);
        let watch = MeshWatch {
            cancel: &cancel,
            progress: &|_, _| {},
        };
        self.volume_mesh_watched(shape, options, watch)
    }

    fn volume_mesh_watched(
        &self,
        shape: ShapeHandle,
        options: &VolumeMeshOptions,
        watch: MeshWatch<'_>,
    ) -> Result<VolumeMesh, VolumeMeshError> {
        if !ffi::has_netgen() {
            return Err(VolumeMeshError::Unsupported);
        }
        if watch.cancel.load(Ordering::Relaxed) {
            return Err(VolumeMeshError::Cancelled);
        }
        let failed = |e: OcctError| VolumeMeshError::Failed(e.message().to_owned());
        let monitor = guarded(|| Ok(ffi::new_mesh_monitor()?)).map_err(failed)?;
        let Some(monitor) = monitor.as_ref() else {
            return Err(VolumeMeshError::Failed("no mesh monitor".to_owned()));
        };
        let data = ffi::VolumeMeshOptionsData {
            max_size: options.max_size,
            min_size: options.min_size,
            grading: options.grading,
            curvature_safety: options.curvature_safety,
            segments_per_edge: options.segments_per_edge,
            second_order: options.second_order,
            optimize_steps: options.optimize_steps,
        };
        // Netgen has no callback: a thread of ours reads the monitor while the shim meshes
        // on this one, and passes a cancel on. Dropping `finished` ends it at once.
        let (finished, done) = mpsc::channel::<()>();
        let result = std::thread::scope(|scope| {
            let watcher = std::thread::Builder::new()
                .name("oxyde mesh watch".into())
                .spawn_scoped(scope, move || watch_mesh(monitor, watch, &done));
            let result = guarded(|| {
                let mesh = self.session.volume_mesh(shape.raw(), &data, monitor)?;
                volume_mesh_from(&mesh).map_err(OcctError::Occt)
            });
            drop(finished);
            // Without a watcher the mesh still runs, unwatched; a panic in it is its own.
            if let Ok(watcher) = watcher {
                let _ = watcher.join();
            }
            result
        });
        if watch.cancel.load(Ordering::Relaxed) {
            return Err(VolumeMeshError::Cancelled);
        }
        result.map_err(failed)
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

// ---- Volume mesh ---------------------------------------------------------------------------

/// Netgen's TET10 edge nodes (after the corners) are on the edges 01 02 03 12 13 23; this
/// is where each of `oxyde_kernel::TET_EDGES` (01 12 02 03 13 23) finds its node.
const NETGEN_TET_EDGE: [usize; 6] = [4, 7, 5, 6, 8, 9];

/// Netgen's tasks as parts of a whole mesh, in the order it runs them: the label the
/// progress shows, and where each starts and ends as a fraction of the whole. The shares
/// are rough, from timing an imported part of 87 faces at 19 000 nodes: optimising the
/// faces took a fifth of the time, filling the volume another, optimising it half.
const NETGEN_STAGES: [(&str, f64, f64); 6] = [
    ("Analysing the shape", 0.0, 0.03),
    ("Meshing the edges", 0.03, 0.05),
    ("Meshing the faces", 0.05, 0.1),
    ("Optimising the faces", 0.1, 0.3),
    ("Filling the volume", 0.3, 0.5),
    ("Optimising the volume", 0.5, 1.0),
];

/// Which of [`NETGEN_STAGES`] a Netgen task belongs to. Its top-level tasks are "Analyse
/// Edges", "Mesh Edges", "Mesh Surface", "Optimizing surface", "Volume meshing" and
/// "Volume optimization", and the steps inside them name what they work on; `None` for a
/// task that does not say.
fn netgen_stage(task: &str) -> Option<usize> {
    let task = task.to_ascii_lowercase();
    let optimising = task.contains("optimi");
    if task.contains("volume") {
        Some(if optimising { 5 } else { 4 })
    } else if task.contains("surface") {
        Some(if optimising { 3 } else { 2 })
    } else if task.starts_with("analyse") {
        Some(0)
    } else if task.contains("edge") {
        Some(1)
    } else {
        None
    }
}

/// Netgen's task and percent turned into one fraction of the whole mesh that never goes
/// back: Netgen starts its percent again at every task, and inner steps come and go.
#[derive(Default)]
struct NetgenProgress {
    stage: usize,
    /// Netgen's percent as the stage began, left over from the task before, until it
    /// moves.
    stale: Option<f64>,
    fraction: f64,
    reported: Option<(f64, usize)>,
}

impl NetgenProgress {
    /// The fraction and the stage's label, when either moved since the last report.
    fn update(&mut self, task: &str, percent: f64) -> Option<(f64, &'static str)> {
        if task.is_empty() {
            return None;
        }
        if let Some(stage) = netgen_stage(task)
            && stage > self.stage
        {
            self.stage = stage;
            self.stale = Some(percent);
        }
        if self.stale.is_some_and(|stale| stale != percent) {
            self.stale = None;
        }
        let &(label, start, end) = NETGEN_STAGES.get(self.stage)?;
        let within = if self.stale.is_some() || !percent.is_finite() {
            0.0
        } else {
            (percent / 100.0).clamp(0.0, 1.0)
        };
        self.fraction = self.fraction.max(start + (end - start) * within);
        let now = (self.fraction, self.stage);
        if self
            .reported
            .is_some_and(|(fraction, stage)| stage == now.1 && now.0 - fraction < 0.002)
        {
            return None;
        }
        self.reported = Some(now);
        Some((now.0, label))
    }
}

/// Reports the mesh's progress and passes a cancel on, every tenth of a second until
/// `finished` says the mesh is over.
fn watch_mesh(monitor: &ffi::MeshMonitor, watch: MeshWatch<'_>, finished: &Receiver<()>) {
    let mut progress = NetgenProgress::default();
    loop {
        if watch.cancel.load(Ordering::Relaxed) {
            monitor.request_stop();
        }
        if let Ok(p) = monitor.progress()
            && let Some((fraction, label)) = progress.update(&p.task, p.percent)
        {
            (watch.progress)(fraction, label);
        }
        match finished.recv_timeout(Duration::from_millis(100)) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Netgen's TRIG6 edge nodes are on the edges 12 02 01, each opposite the corner of its
/// number; this is where each of `oxyde_kernel::TRIANGLE_EDGES` (01 12 20) finds its node.
const NETGEN_TRIANGLE_EDGE: [usize; 3] = [5, 3, 4];

/// The mesh in the kernel's terms: Netgen's node order converted, every tetrahedron
/// positively oriented, every boundary triangle wound counter-clockwise seen from outside.
/// Netgen's own orientation conventions are not relied on: each element is checked.
fn volume_mesh_from(data: &ffi::VolumeMeshData) -> Result<VolumeMesh, String> {
    let nodes: Vec<[f64; 3]> = data.nodes.as_chunks::<3>().0.to_vec();
    let count = nodes.len();
    let node = |i: u32| -> Result<[f64; 3], String> {
        nodes
            .get(i as usize)
            .copied()
            .ok_or_else(|| format!("node {i} of {count} does not exist"))
    };

    let second_order = match (data.nodes_per_tet, data.nodes_per_triangle) {
        (4, 3) => false,
        (10, 6) => true,
        (t, f) => {
            return Err(format!(
                "{t}-node elements with {f}-node boundary triangles"
            ));
        }
    };

    let per_tet = data.nodes_per_tet as usize;
    let mut tets = Vec::with_capacity(data.tets.len() / per_tet);
    let mut tet_edges = Vec::new();
    for el in data.tets.chunks_exact(per_tet) {
        let mut corners = [at(el, 0)?, at(el, 1)?, at(el, 2)?, at(el, 3)?];
        let mut edges = [0u32; 6];
        if second_order {
            for (e, &netgen) in edges.iter_mut().zip(&NETGEN_TET_EDGE) {
                *e = at(el, netgen)?;
                node(*e)?;
            }
        }
        let p = [
            node(corners[0])?,
            node(corners[1])?,
            node(corners[2])?,
            node(corners[3])?,
        ];
        if triple(sub(p[1], p[0]), sub(p[2], p[0]), sub(p[3], p[0])) < 0.0 {
            // Swapping corners 1 and 2 swaps the edges 01 and 02, and 13 and 23.
            corners.swap(1, 2);
            edges.swap(0, 2);
            edges.swap(4, 5);
        }
        tets.push(corners);
        if second_order {
            tet_edges.push(edges);
        }
    }

    // The fourth corner of the element under a boundary triangle tells its inside.
    let mut opposite: HashMap<[u32; 3], u32> = HashMap::with_capacity(tets.len() * 2);
    for t in &tets {
        for (skip, &apex) in t.iter().enumerate() {
            let mut face = [0u32; 3];
            for (slot, &c) in face.iter_mut().zip(
                t.iter()
                    .enumerate()
                    .filter(|&(k, _)| k != skip)
                    .map(|(_, c)| c),
            ) {
                *slot = c;
            }
            face.sort_unstable();
            opposite.insert(face, apex);
        }
    }

    let per_triangle = data.nodes_per_triangle as usize;
    let mut triangles = Vec::with_capacity(data.triangles.len() / per_triangle);
    let mut triangle_edges = Vec::new();
    for el in data.triangles.chunks_exact(per_triangle) {
        let mut corners = [at(el, 0)?, at(el, 1)?, at(el, 2)?];
        let mut edges = [0u32; 3];
        if second_order {
            for (e, &netgen) in edges.iter_mut().zip(&NETGEN_TRIANGLE_EDGE) {
                *e = at(el, netgen)?;
                node(*e)?;
            }
        }
        let mut key = corners;
        key.sort_unstable();
        let inside = *opposite
            .get(&key)
            .ok_or("a boundary triangle that is no element's face")?;
        let p = [node(corners[0])?, node(corners[1])?, node(corners[2])?];
        let q = node(inside)?;
        if triple(sub(p[1], p[0]), sub(p[2], p[0]), sub(q, p[0])) > 0.0 {
            // The inside is in front: turn the triangle round. Swapping corners 1 and 2
            // swaps the edges 01 and 20.
            corners.swap(1, 2);
            edges.swap(0, 2);
        }
        triangles.push(corners);
        if second_order {
            triangle_edges.push(edges);
        }
    }
    if data.triangle_faces.len() != triangles.len() {
        return Err(format!(
            "{} face indices for {} boundary triangles",
            data.triangle_faces.len(),
            triangles.len()
        ));
    }

    Ok(VolumeMesh {
        nodes,
        tets,
        tet_edge_nodes: second_order.then_some(tet_edges),
        triangles,
        triangle_edge_nodes: second_order.then_some(triangle_edges),
        triangle_faces: data.triangle_faces.clone(),
    })
}

/// Node `k` of an element as the shim lists it.
fn at(element: &[u32], k: usize) -> Result<u32, String> {
    element
        .get(k)
        .copied()
        .ok_or_else(|| format!("an element with {} nodes", element.len()))
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// `a . (b x c)`: six times the signed volume of the tetrahedron on the edges `a`, `b`, `c`.
fn triple(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
        + a[2] * (b[0] * c[1] - b[1] * c[0])
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
    use super::{
        NetgenProgress, density_kg_per_m3, ffi, netgen_stage, sub, triple, volume_mesh_from,
    };
    use oxyde_kernel::{TET_EDGES, TRIANGLE_EDGES};

    /// One ten-node tetrahedron as Netgen could hand it over: inverted, with its four
    /// boundary triangles wound either way, edge nodes in Netgen's orders.
    #[test]
    fn netgen_meshes_come_out_oriented_and_in_kernel_order() {
        let corners = [
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let pairs = [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)];
        let mut nodes: Vec<[f64; 3]> = corners.to_vec();
        for (a, b) in pairs {
            let (p, q): ([f64; 3], [f64; 3]) = (corners[a], corners[b]);
            nodes.push([
                (p[0] + q[0]) / 2.0,
                (p[1] + q[1]) / 2.0,
                (p[2] + q[2]) / 2.0,
            ]);
        }
        let mid = |a: u32, b: u32| {
            let key = (a.min(b) as usize, a.max(b) as usize);
            4 + pairs.iter().position(|&p| p == key).unwrap() as u32
        };
        // TRIG6: corners, then the edges 12, 02, 01.
        let trig6 = |a: u32, b: u32, c: u32| [a, b, c, mid(b, c), mid(a, c), mid(a, b)];
        let triangles: Vec<u32> = [
            trig6(1, 2, 3),
            trig6(0, 2, 3),
            trig6(1, 0, 3),
            trig6(0, 1, 2),
        ]
        .concat();
        let data = ffi::VolumeMeshData {
            nodes: nodes.iter().flatten().copied().collect(),
            nodes_per_tet: 10,
            tets: (0..10).collect(),
            nodes_per_triangle: 6,
            triangles,
            triangle_faces: vec![0, 1, 2, 3],
        };
        let mesh = volume_mesh_from(&data).unwrap();
        let at = |n: u32| mesh.nodes[n as usize];
        let middle = |a: u32, b: u32| {
            let (p, q) = (at(a), at(b));
            [
                (p[0] + q[0]) / 2.0,
                (p[1] + q[1]) / 2.0,
                (p[2] + q[2]) / 2.0,
            ]
        };

        let t = mesh.tets[0];
        let p = t.map(at);
        assert!(triple(sub(p[1], p[0]), sub(p[2], p[0]), sub(p[3], p[0])) > 0.0);
        let edges = mesh.tet_edge_nodes.as_ref().unwrap()[0];
        for (k, &(a, b)) in TET_EDGES.iter().enumerate() {
            assert_eq!(at(edges[k]), middle(t[a], t[b]), "tet edge {k}");
        }

        let triangle_edges = mesh.triangle_edge_nodes.as_ref().unwrap();
        for (k, tri) in mesh.triangles.iter().enumerate() {
            let apex = *t.iter().find(|n| !tri.contains(n)).unwrap();
            let q = tri.map(at);
            assert!(triple(sub(q[1], q[0]), sub(q[2], q[0]), sub(at(apex), q[0])) < 0.0);
            for (e, &(a, b)) in TRIANGLE_EDGES.iter().enumerate() {
                assert_eq!(at(triangle_edges[k][e]), middle(tri[a], tri[b]));
            }
        }
        assert_eq!(mesh.triangle_faces, vec![0, 1, 2, 3]);
    }

    #[test]
    fn a_mesh_with_mismatched_orders_is_refused() {
        let data = ffi::VolumeMeshData {
            nodes: vec![0.0; 12],
            nodes_per_tet: 10,
            tets: Vec::new(),
            nodes_per_triangle: 3,
            triangles: Vec::new(),
            triangle_faces: Vec::new(),
        };
        assert!(volume_mesh_from(&data).is_err());
    }

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

    #[test]
    fn netgen_tasks_are_put_in_their_stage() {
        assert_eq!(netgen_stage("Analyse Edges"), Some(0));
        assert_eq!(netgen_stage("Mesh Edges"), Some(1));
        assert_eq!(netgen_stage("Mesh Surface"), Some(2));
        assert_eq!(netgen_stage("Optimizing surface"), Some(3));
        assert_eq!(netgen_stage("Volume meshing"), Some(4));
        assert_eq!(netgen_stage("Volume optimization"), Some(5));
        assert_eq!(netgen_stage("Optimize Volume: Swap Improve"), Some(5));
        assert_eq!(netgen_stage("Delaunay meshing"), None);
    }

    #[test]
    fn netgen_progress_never_goes_back() {
        let mut p = NetgenProgress::default();
        // Nothing before Netgen names a task.
        assert_eq!(p.update("", 0.0), None);
        let (f, label) = p.update("Analyse Edges", 50.0).unwrap();
        assert_eq!(label, "Analysing the shape");
        assert!(f > 0.0 && f < 0.03);
        // A new task's percent is the last one's until it moves: no jump to its end.
        let (f, label) = p.update("Mesh Surface", 50.0).unwrap();
        assert_eq!(label, "Meshing the faces");
        assert!((f - 0.05).abs() < 1e-9, "{f}");
        let (f, _) = p.update("Mesh Surface", 60.0).unwrap();
        assert!((f - 0.08).abs() < 1e-9, "{f}");
        // An inner step of an earlier stage, or a percent going down, moves nothing back.
        assert_eq!(p.update("EdgeSwapping (2D)", 10.0), None);
        let mut last = f;
        for (task, percent) in [
            ("Volume meshing", 0.0),
            ("Volume meshing", 90.0),
            ("Volume optimization", 90.0),
            ("Volume optimization", 30.0),
            ("Volume optimization", 100.0),
        ] {
            if let Some((f, _)) = p.update(task, percent) {
                assert!(f >= last && f <= 1.0, "{f} after {last}");
                last = f;
            }
        }
        assert!((last - 1.0).abs() < 1e-9, "{last}");
        assert!(p.update("Volume optimization", f64::NAN).is_none());
    }
}
