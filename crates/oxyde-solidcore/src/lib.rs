//! SolidCraft's kernel, solidcore (Apache-2.0, `tangency-labs/solidcraft`), behind Oxyde's
//! [`Kernel`] trait: the spike. Boxes, extrusions, booleans, transforms, topology,
//! tessellation, measures and face history are built with solidcore; every other operation
//! fails with a plain message, so a feature that needs it fails in the timeline as usual.
//!
//! Shapes are solidcore-ops [`Body`]s in an arena, named by [`ShapeHandle`]. A face's history
//! rides on it as a kernel attribute ([`label`]) through solidcore's own edits, so persistent
//! names work as they do with OpenCASCADE. Topology, measures and meshes are read from the
//! body's record, whose order is the face, edge and vertex numbering Oxyde sees.

// Never crash: an error is returned instead (docs/ARCHITECTURE.md, "Never crash").
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

mod convert;
mod label;

use std::collections::HashMap;
use std::fmt;
use std::path::Path as FilePath;

use oxyde_geom::{Axis, Loop, Path as SweepPath, Plane, Point3, Profile, Transform, Vec3};
use oxyde_kernel::{
    BooleanOp, CurveKind, EdgeInfo, FaceInfo, FaceMesh, FaceSource, Kernel, Mesh, OpHistory,
    OpResult, ShapeHandle, StepAssembly, StepColors, StepImport, StepMaterial, SurfaceKind,
    Topology, TopologyCounts, VertexInfo,
};
use solidcore::EntityRef;
use solidcore_ops::{Body, Order};
use solidcore_record::brep::{Brep as Record, Curve, Loop as RecordLoop, Shell, Surface};

use crate::label::Label;

/// The relative accuracy measures (areas, volumes, centroids) are asked for.
const MEASURE_ACCURACY: f64 = 1e-7;

/// What went wrong in the solidcore kernel.
#[derive(Debug, Clone, PartialEq)]
pub struct SolidcoreError(String);

impl fmt::Display for SolidcoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SolidcoreError {}

impl From<solidcore_ops::Error> for SolidcoreError {
    fn from(e: solidcore_ops::Error) -> Self {
        SolidcoreError(format!("solidcore: {e}"))
    }
}

impl From<solidcore::SolidError> for SolidcoreError {
    fn from(e: solidcore::SolidError) -> Self {
        SolidcoreError(format!("solidcore: {e}"))
    }
}

/// An operation the spike does not build yet.
fn not_yet(what: &str) -> SolidcoreError {
    SolidcoreError(format!("the solidcore kernel cannot {what} yet (spike)"))
}

/// What a profile curve that has no solidcore segment fails an extrusion with.
fn unbuilt(why: convert::Unbuilt) -> SolidcoreError {
    match why {
        convert::Unbuilt::NoCircle => {
            SolidcoreError("a profile arc has no circle through it".to_owned())
        }
        convert::Unbuilt::Kind => not_yet("extrude this kind of sketch curve"),
    }
}

type Result<T> = std::result::Result<T, SolidcoreError>;

/// Oxyde's kernel on solidcore.
#[derive(Default)]
pub struct SolidcoreKernel {
    bodies: HashMap<u64, Body>,
    next: u64,
}

impl SolidcoreKernel {
    pub fn new() -> Self {
        Self::default()
    }

    fn body(&self, shape: ShapeHandle) -> Result<&Body> {
        self.bodies
            .get(&shape.raw())
            .ok_or_else(|| SolidcoreError("no such shape in the solidcore kernel".to_owned()))
    }

    fn keep(&mut self, body: Body) -> ShapeHandle {
        self.next += 1;
        self.bodies.insert(self.next, body);
        ShapeHandle::from_raw(self.next)
    }

    /// A copy of `shape` whose every face carries `InputFace { input, face }` with its own
    /// index, so the result of an operation on it says where its faces came from.
    fn labelled_copy(&self, shape: ShapeHandle, input: u32) -> Result<Body> {
        let copy = self.body(shape)?.copy()?;
        let (_, order) = copy.to_record()?;
        {
            let mut brep = copy.kernel();
            for (i, f) in order.faces.iter().enumerate() {
                let face = u32::try_from(i).unwrap_or(u32::MAX);
                label::attach(
                    &mut brep,
                    EntityRef::Face(*f),
                    Label::one(FaceSource::InputFace { input, face }),
                )?;
            }
        }
        Ok(copy)
    }

    /// Keeps `body` and reads its faces' history from their labels.
    fn result(&mut self, body: Body) -> Result<OpResult> {
        let (_, order) = body.to_record()?;
        let faces = {
            let brep = body.kernel();
            order
                .faces
                .iter()
                .map(|f| label::read(&brep, EntityRef::Face(*f)))
                .collect()
        };
        Ok(OpResult {
            shape: self.keep(body),
            history: OpHistory { faces },
        })
    }
}

fn point(p: [f64; 3]) -> Point3 {
    Point3::new(p[0], p[1], p[2])
}

fn vec3(p: [f64; 3]) -> Vec3 {
    Vec3::new(p[0], p[1], p[2])
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// A frame's axis: its origin, along its x × y.
fn frame_axis(frame: &solidcore_record::brep::Frame) -> Option<Axis> {
    let d = cross(frame.x_axis, frame.y_axis);
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    (len > 0.0).then(|| Axis {
        origin: point(frame.origin),
        direction: vec3([d[0] / len, d[1] / len, d[2] / len]),
    })
}

fn surface_kind(s: &Surface) -> (SurfaceKind, Option<Axis>) {
    match s {
        Surface::Plane { .. } | Surface::PlaneThrough { .. } => (SurfaceKind::Plane, None),
        Surface::Cylinder { frame, .. } => (SurfaceKind::Cylinder, frame_axis(frame)),
        Surface::Cone { frame, .. } => (SurfaceKind::Cone, frame_axis(frame)),
        Surface::Sphere { frame, .. } => (SurfaceKind::Sphere, frame_axis(frame)),
        Surface::Torus { frame, .. } => (SurfaceKind::Torus, frame_axis(frame)),
        Surface::Spline { .. } | Surface::RationalSpline { .. } => (SurfaceKind::BSpline, None),
        _ => (SurfaceKind::Other, None),
    }
}

fn curve_kind(c: &Curve) -> (CurveKind, Option<Axis>) {
    match c {
        Curve::Line { .. } => (CurveKind::Line, None),
        Curve::Circle { frame, .. } => (CurveKind::Circle, frame_axis(frame)),
        Curve::Ellipse { frame, .. } => (CurveKind::Ellipse, frame_axis(frame)),
        Curve::Spline { .. } | Curve::RationalSpline { .. } => (CurveKind::BSpline, None),
        _ => (CurveKind::Other, None),
    }
}

/// The topology Oxyde sees, in the record's order.
fn topology(body: &Body, record: &Record, order: &Order) -> Result<Topology> {
    let mut faces = Vec::with_capacity(record.faces.len());
    for (i, f) in record.faces.iter().enumerate() {
        let (surface, axis) = surface_kind(&f.surface);
        let c = body.face_centroid(order, i, MEASURE_ACCURACY)?;
        let normal = body.face_plane_normal(order, i)?.map(|n| vec3(n.value));
        let mut edges: Vec<u32> = f
            .loops
            .iter()
            .flat_map(|l| match l {
                RecordLoop::Edges { edges } => edges.iter().map(|e| e.edge).collect::<Vec<_>>(),
                RecordLoop::Vertex { .. } => Vec::new(),
            })
            .collect();
        edges.dedup();
        faces.push(FaceInfo {
            surface,
            area: c.area,
            centroid: point(c.centroid),
            normal,
            axis,
            edges,
        });
    }
    let mut edges = Vec::with_capacity(record.edges.len());
    for (i, e) in record.edges.iter().enumerate() {
        let (curve, axis) = curve_kind(&e.curve);
        let mut on: Vec<u32> = e.uses.iter().map(|u| u.face).collect();
        on.sort_unstable();
        on.dedup();
        let mut vertices = e.vertices.to_vec();
        vertices.dedup();
        edges.push(EdgeInfo {
            curve,
            length: body.edge_length(order, i, MEASURE_ACCURACY)?,
            midpoint: point(body.edge_middle(order, i)?),
            axis,
            vertices,
            faces: on,
        });
    }
    let vertices = record
        .vertices
        .iter()
        .map(|v| VertexInfo {
            point: point(v.point),
        })
        .collect();
    Ok(Topology {
        faces,
        edges,
        vertices,
    })
}

impl Kernel for SolidcoreKernel {
    type Error = SolidcoreError;

    fn make_box(&mut self, corner: Point3, size: Vec3) -> Result<ShapeHandle> {
        let body =
            solidcore_ops::make_box([corner.x, corner.y, corner.z], [size.x, size.y, size.z])?;
        Ok(self.keep(body))
    }

    fn extrude(&mut self, plane: &Plane, profile: &Profile, vector: Vec3) -> Result<OpResult> {
        let n = plane.normal;
        let along = vector.x * n.x + vector.y * n.y + vector.z * n.z;
        let off = Vec3::new(
            vector.x - n.x * along,
            vector.y - n.y * along,
            vector.z - n.z * along,
        );
        if off.length() > 1e-9 * vector.length().max(1.0) {
            return Err(not_yet("extrude at a slant to the sketch plane"));
        }
        let regions = convert::regions(profile).map_err(unbuilt)?;
        let (start, end) = if along >= 0.0 {
            (0.0, along)
        } else {
            (along, 0.0)
        };
        let body = solidcore_ops::prism::extrude_planar_regions(
            &convert::plane(plane),
            &regions,
            start,
            end,
        )?;
        // Name the faces after the profile and the caps, by where they lie.
        let (record, order) = body.to_record()?;
        {
            let middle = |e: usize| body.edge_middle(&order, e).ok();
            let sources: Vec<_> = (0..order.faces.len())
                .map(|i| convert::extrusion_source(&record, i, plane, profile, start, end, &middle))
                .collect();
            let mut brep = body.kernel();
            for (f, source) in order.faces.iter().zip(sources) {
                // A backwards extrusion starts at its far end: its caps swap.
                let source = match (source, along >= 0.0) {
                    (Some(FaceSource::StartCap), false) => Some(FaceSource::EndCap),
                    (Some(FaceSource::EndCap), false) => Some(FaceSource::StartCap),
                    (s, _) => s,
                };
                if let Some(source) = source {
                    label::attach(&mut brep, EntityRef::Face(*f), Label::one(source))?;
                }
            }
        }
        self.result(body)
    }

    fn revolve(&mut self, _: &Plane, _: &Profile, _: &Axis, _: f64) -> Result<OpResult> {
        Err(not_yet("revolve"))
    }

    fn sweep(&mut self, _: &Plane, _: &Profile, _: &Plane, _: &SweepPath) -> Result<OpResult> {
        Err(not_yet("sweep"))
    }

    fn loft(&mut self, _: &[(Plane, Loop)], _: bool) -> Result<Option<OpResult>> {
        Err(not_yet("loft"))
    }

    fn shell(&mut self, _: ShapeHandle, _: &[u32], _: f64) -> Result<OpResult> {
        Err(not_yet("shell"))
    }

    fn transform(&mut self, shape: ShapeHandle, transform: &Transform) -> Result<OpResult> {
        let body = self.labelled_copy(shape, 0)?;
        // Oxyde's rows map column vectors (p' = R p + t); solidcore's map row vectors
        // (p' = p A + t): its rows 0-2 are the images of x, y and z, its row 3 the translation.
        let [r0, r1, r2] = transform.rows;
        body.transform(&[
            [r0[0], r1[0], r2[0], 0.0],
            [r0[1], r1[1], r2[1], 0.0],
            [r0[2], r1[2], r2[2], 0.0],
            [r0[3], r1[3], r2[3], 1.0],
        ])?;
        self.result(body)
    }

    fn boolean_many(
        &mut self,
        op: BooleanOp,
        target: ShapeHandle,
        tools: &[ShapeHandle],
    ) -> Result<OpResult> {
        let kind = match op {
            BooleanOp::Fuse => solidcore_ops::BooleanOp::Union,
            BooleanOp::Cut => solidcore_ops::BooleanOp::Subtract,
            BooleanOp::Common => solidcore_ops::BooleanOp::Intersect,
        };
        let mut acc = self.labelled_copy(target, 0)?;
        for (k, tool) in tools.iter().enumerate() {
            let input = u32::try_from(k + 1).unwrap_or(u32::MAX);
            let tool = self.labelled_copy(*tool, input)?;
            acc = solidcore_ops::boolean::boolean(acc, tool, kind)?
                .ok_or_else(|| SolidcoreError("the boolean left nothing".to_owned()))?;
        }
        self.result(acc)
    }

    fn solid_from_mesh(&mut self, _: &[[f64; 3]], _: &[[u32; 3]]) -> Result<OpResult> {
        Err(not_yet("make a solid from a mesh"))
    }

    fn fillet(&mut self, _: ShapeHandle, _: &[u32], _: f64) -> Result<OpResult> {
        Err(not_yet("fillet"))
    }

    fn chamfer(&mut self, _: ShapeHandle, _: &[u32], _: f64) -> Result<OpResult> {
        Err(not_yet("chamfer"))
    }

    fn topology(&self, shape: ShapeHandle) -> Result<Topology> {
        let body = self.body(shape)?;
        let (record, order) = body.to_record()?;
        topology(body, &record, &order)
    }

    fn tessellate(&self, shape: ShapeHandle, deflection: f64) -> Result<Mesh> {
        let body = self.body(shape)?;
        let (record, _) = body.to_record()?;
        let (lo, hi) = body.bounds()?;
        let size = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2))
            .sqrt()
            .max(1e-6);
        let tolerance = solidcore_tess::MeshTolerance::chord(deflection.max(1e-6));
        let (plan, _) = solidcore_tess::plan(&record, &tolerance, size)
            .map_err(|e| SolidcoreError(format!("solidcore tessellation: {e}")))?;
        let (mesh, _) = plan.evaluate();
        // One mesh with a face index per triangle, split into Oxyde's mesh per face.
        let mut faces: Vec<FaceMesh> = (0..record.faces.len())
            .map(|_| FaceMesh {
                positions: Vec::new(),
                triangles: Vec::new(),
            })
            .collect();
        let mut index: Vec<HashMap<u32, u32>> = vec![HashMap::new(); record.faces.len()];
        for (t, face) in mesh.triangles.iter().zip(&mesh.tri_face) {
            let (Some(out), Some(map)) =
                (faces.get_mut(*face as usize), index.get_mut(*face as usize))
            else {
                continue;
            };
            let mut tri = [0u32; 3];
            for (slot, &v) in tri.iter_mut().zip(t) {
                let Some(p) = mesh.positions.get(v as usize) else {
                    continue;
                };
                *slot = *map.entry(v).or_insert_with(|| {
                    out.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                    u32::try_from(out.positions.len() - 1).unwrap_or(u32::MAX)
                });
            }
            out.triangles.push(tri);
        }
        let edges = mesh
            .edges
            .iter()
            .map(|e| {
                e.iter()
                    .map(|p| [p.x as f32, p.y as f32, p.z as f32])
                    .collect()
            })
            .collect();
        Ok(Mesh { faces, edges })
    }

    fn volume(&self, shape: ShapeHandle) -> Result<f64> {
        Ok(self.body(shape)?.volume(MEASURE_ACCURACY)?)
    }

    fn centre_of_mass(&self, shape: ShapeHandle) -> Result<Point3> {
        let m = self.body(shape)?.mass_properties(MEASURE_ACCURACY)?;
        Ok(point(m.centroid))
    }

    fn bounds(&self, shape: ShapeHandle) -> Result<(Point3, Point3)> {
        let (lo, hi) = self.body(shape)?.bounds()?;
        Ok((point(lo), point(hi)))
    }

    fn topology_counts(&self, shape: ShapeHandle) -> Result<TopologyCounts> {
        let body = self.body(shape)?;
        let (record, _) = body.to_record()?;
        Ok(TopologyCounts {
            solids: body.solid_count()?,
            shells: record
                .shells
                .iter()
                .filter(|s| matches!(s, Shell::Faces { .. }))
                .count(),
            faces: record.faces.len(),
            edges: record.edges.len(),
            vertices: record.vertices.len(),
        })
    }

    fn export_step(
        &self,
        _: &[ShapeHandle],
        _: &[StepColors],
        _: &[Option<StepMaterial>],
        _: &FilePath,
    ) -> Result<()> {
        Err(not_yet("export STEP"))
    }

    fn export_iges(&self, _: &[ShapeHandle], _: &FilePath) -> Result<()> {
        Err(not_yet("export IGES"))
    }

    fn import_step(&mut self, _: &[u8]) -> Result<StepImport> {
        Err(not_yet("import STEP"))
    }

    fn read_step_assembly(&mut self, _: &[u8]) -> Result<StepAssembly> {
        Err(not_yet("read a STEP assembly"))
    }

    fn release(&mut self, shape: ShapeHandle) {
        self.bodies.remove(&shape.raw());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_curve_the_adapter_does_not_build_fails_like_any_unsupported_operation() {
        assert_eq!(
            unbuilt(convert::Unbuilt::Kind).to_string(),
            "the solidcore kernel cannot extrude this kind of sketch curve yet (spike)"
        );
        assert_eq!(
            unbuilt(convert::Unbuilt::NoCircle).to_string(),
            "a profile arc has no circle through it"
        );
    }
}
