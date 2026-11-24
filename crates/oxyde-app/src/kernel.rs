//! The kernel the application builds parts with, chosen once when the application starts, for
//! the whole session: by `--kernel <name>`, else by Preferences › Debug › Kernel. Changing the
//! preference takes effect at the next start, so that every shape of a session comes from one
//! kernel. OpenCASCADE (`oxyde-occt`) is the default; SolidCraft's kernel, solidcore
//! (`oxyde-solidcore`), is the second choice in builds with the `solidcore` feature (the
//! spike). [`AppKernel`] hands every [`Kernel`] call to the chosen one (docs/ARCHITECTURE.md
//! §2 and §10).

use std::sync::OnceLock;

use oxyde_format::PartKernel;
use oxyde_geom::{Axis, Loop, Path as SweepPath, Plane, Point3, Profile, Transform, Vec3};
use oxyde_kernel::{
    BooleanOp, ChamferSecond, Distance, EdgeCurve, EdgeTangent, Kernel, Mesh, MeshWatch, OpResult,
    ProjectedEdges, ShapeHandle, StepAssembly, StepColors, StepImport, StepMaterial, SubShape,
    Topology, TopologyCounts, VolumeMesh, VolumeMeshError, VolumeMeshOptions,
};
use oxyde_occt::{OcctError, OcctKernel};
#[cfg(feature = "solidcore")]
use oxyde_solidcore::{SolidcoreError, SolidcoreKernel};
use serde::{Deserialize, Serialize};

/// Which kernel builds the parts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KernelKind {
    /// OpenCASCADE.
    #[default]
    Occt,
    /// SolidCraft's kernel: boxes, extrusions, booleans and moves only (the spike).
    #[cfg(feature = "solidcore")]
    Solidcore,
}

impl Serialize for KernelKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

/// A kernel this build does not have (written by a build with another feature) reads as the
/// default rather than failing, which would lose every other preference with it.
impl<'de> Deserialize<'de> for KernelKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(KernelKind::parse(&name).unwrap_or_default())
    }
}

impl KernelKind {
    pub const ALL: &[KernelKind] = &[
        KernelKind::Occt,
        #[cfg(feature = "solidcore")]
        KernelKind::Solidcore,
    ];

    /// Its name in preferences and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            KernelKind::Occt => "occt",
            #[cfg(feature = "solidcore")]
            KernelKind::Solidcore => "solidcore",
        }
    }

    /// Its name for people.
    pub fn label(self) -> &'static str {
        match self {
            KernelKind::Occt => "OpenCASCADE",
            #[cfg(feature = "solidcore")]
            KernelKind::Solidcore => "solidcore (SolidCraft's kernel, spike)",
        }
    }

    /// As a part file records it.
    pub fn file_kernel(self) -> PartKernel {
        match self {
            KernelKind::Occt => PartKernel::Occt,
            #[cfg(feature = "solidcore")]
            KernelKind::Solidcore => PartKernel::Solidcore,
        }
    }

    /// The kind a `--kernel` argument names.
    pub fn parse(name: &str) -> Option<KernelKind> {
        match name {
            "occt" | "opencascade" => Some(KernelKind::Occt),
            #[cfg(feature = "solidcore")]
            "solidcore" => Some(KernelKind::Solidcore),
            _ => None,
        }
    }

    /// The names `--kernel` takes, for its error message.
    pub fn names() -> String {
        KernelKind::ALL
            .iter()
            .map(|kind| kind.name())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The kernel of this session, chosen once ([`choose`]).
static CHOSEN: OnceLock<KernelKind> = OnceLock::new();

/// Makes `wanted` the session's kernel; the first choice holds. Returns the kernel chosen.
pub fn choose(wanted: KernelKind) -> KernelKind {
    *CHOSEN.get_or_init(|| wanted)
}

/// The session's kernel: the one chosen, or the default before any choice (in tests).
pub fn chosen() -> KernelKind {
    CHOSEN.get().copied().unwrap_or_default()
}

/// The session's kernel for people, with its version: in the About box and `--version`.
pub fn description() -> String {
    match chosen() {
        KernelKind::Occt => format!("OpenCASCADE {}", OcctKernel::occt_version()),
        #[cfg(feature = "solidcore")]
        KernelKind::Solidcore => KernelKind::Solidcore.label().to_owned(),
    }
}

/// What to tell the user about a part file built by `built_by`, rebuilt by `using`: nothing
/// when they are the same kernel.
pub fn rebuilt_warning(built_by: &PartKernel, using: KernelKind) -> Option<String> {
    if *built_by == using.file_kernel() {
        return None;
    }
    let was = match built_by {
        PartKernel::Occt => "OpenCASCADE".to_owned(),
        PartKernel::Solidcore => "solidcore".to_owned(),
        PartKernel::Other(name) => format!("a kernel named \"{name}\""),
    };
    Some(format!(
        "built by {was}, rebuilt by {}: check its features, faces and edges may be cut \
         and named differently",
        using.label()
    ))
}

/// The session's kernel: whichever kernel was chosen, as one type.
pub enum AppKernel {
    Occt(OcctKernel),
    #[cfg(feature = "solidcore")]
    Solidcore(SolidcoreKernel),
}

/// An error of the session's kernel.
#[derive(Debug)]
pub enum AppKernelError {
    Occt(OcctError),
    #[cfg(feature = "solidcore")]
    Solidcore(SolidcoreError),
}

impl std::fmt::Display for AppKernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppKernelError::Occt(e) => e.fmt(f),
            #[cfg(feature = "solidcore")]
            AppKernelError::Solidcore(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for AppKernelError {}

impl AppKernel {
    /// A kernel of `kind`.
    pub fn new(kind: KernelKind) -> AppKernel {
        match kind {
            KernelKind::Occt => AppKernel::Occt(OcctKernel::new()),
            #[cfg(feature = "solidcore")]
            KernelKind::Solidcore => AppKernel::Solidcore(SolidcoreKernel::new()),
        }
    }

    /// The OpenCASCADE kernel inside, for the code that works with it alone (the assistant
    /// panel's MCP session); none when the session runs another kernel.
    pub fn occt_mut(&mut self) -> Option<&mut OcctKernel> {
        match self {
            AppKernel::Occt(k) => Some(k),
            #[cfg(feature = "solidcore")]
            AppKernel::Solidcore(_) => None,
        }
    }

    /// Which kernel it is.
    pub fn kind(&self) -> KernelKind {
        match self {
            AppKernel::Occt(_) => KernelKind::Occt,
            #[cfg(feature = "solidcore")]
            AppKernel::Solidcore(_) => KernelKind::Solidcore,
        }
    }
}

/// Calls the same method on whichever kernel it is, its error made the session's.
macro_rules! each {
    ($self:expr, $k:ident => $call:expr) => {
        match $self {
            AppKernel::Occt($k) => $call.map_err(AppKernelError::Occt),
            #[cfg(feature = "solidcore")]
            AppKernel::Solidcore($k) => $call.map_err(AppKernelError::Solidcore),
        }
    };
}

/// Calls the same method on whichever kernel it is, as it returns.
macro_rules! each_plain {
    ($self:expr, $k:ident => $call:expr) => {
        match $self {
            AppKernel::Occt($k) => $call,
            #[cfg(feature = "solidcore")]
            AppKernel::Solidcore($k) => $call,
        }
    };
}

impl Kernel for AppKernel {
    type Error = AppKernelError;

    fn make_box(&mut self, corner: Point3, size: Vec3) -> Result<ShapeHandle, Self::Error> {
        each!(self, k => k.make_box(corner, size))
    }

    fn extrude(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        vector: Vec3,
    ) -> Result<OpResult, Self::Error> {
        each!(self, k => k.extrude(plane, profile, vector))
    }

    fn revolve(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        axis: &Axis,
        angle: f64,
    ) -> Result<OpResult, Self::Error> {
        each!(self, k => k.revolve(plane, profile, axis, angle))
    }

    fn sweep(
        &mut self,
        plane: &Plane,
        profile: &Profile,
        path_plane: &Plane,
        path: &SweepPath,
    ) -> Result<OpResult, Self::Error> {
        each!(self, k => k.sweep(plane, profile, path_plane, path))
    }

    fn loft(
        &mut self,
        sections: &[(Plane, Loop)],
        ruled: bool,
    ) -> Result<Option<OpResult>, Self::Error> {
        each!(self, k => k.loft(sections, ruled))
    }

    fn shell(
        &mut self,
        shape: ShapeHandle,
        removed_faces: &[u32],
        offset: f64,
    ) -> Result<OpResult, Self::Error> {
        each!(self, k => k.shell(shape, removed_faces, offset))
    }

    fn transform(
        &mut self,
        shape: ShapeHandle,
        transform: &Transform,
    ) -> Result<OpResult, Self::Error> {
        each!(self, k => k.transform(shape, transform))
    }

    fn boolean(
        &mut self,
        op: BooleanOp,
        target: ShapeHandle,
        tool: ShapeHandle,
    ) -> Result<OpResult, Self::Error> {
        each!(self, k => k.boolean(op, target, tool))
    }

    fn boolean_many(
        &mut self,
        op: BooleanOp,
        target: ShapeHandle,
        tools: &[ShapeHandle],
    ) -> Result<OpResult, Self::Error> {
        each!(self, k => k.boolean_many(op, target, tools))
    }

    fn solid_from_mesh(
        &mut self,
        points: &[[f64; 3]],
        triangles: &[[u32; 3]],
    ) -> Result<OpResult, Self::Error> {
        each!(self, k => k.solid_from_mesh(points, triangles))
    }

    fn fillet(
        &mut self,
        shape: ShapeHandle,
        edges: &[u32],
        radius: f64,
    ) -> Result<OpResult, Self::Error> {
        each!(self, k => k.fillet(shape, edges, radius))
    }

    fn chamfer(
        &mut self,
        shape: ShapeHandle,
        edges: &[u32],
        distance: f64,
    ) -> Result<OpResult, Self::Error> {
        each!(self, k => k.chamfer(shape, edges, distance))
    }

    fn chamfer_with(
        &mut self,
        shape: ShapeHandle,
        edges: &[(u32, u32)],
        first: f64,
        second: ChamferSecond,
    ) -> Result<Option<OpResult>, Self::Error> {
        each!(self, k => k.chamfer_with(shape, edges, first, second))
    }

    fn topology(&self, shape: ShapeHandle) -> Result<Topology, Self::Error> {
        each!(self, k => k.topology(shape))
    }

    fn tessellate(&self, shape: ShapeHandle, deflection: f64) -> Result<Mesh, Self::Error> {
        each!(self, k => k.tessellate(shape, deflection))
    }

    fn tessellate_for_export(
        &self,
        shape: ShapeHandle,
        deflection: f64,
        angle: f64,
    ) -> Result<Mesh, Self::Error> {
        each!(self, k => k.tessellate_for_export(shape, deflection, angle))
    }

    fn volume(&self, shape: ShapeHandle) -> Result<f64, Self::Error> {
        each!(self, k => k.volume(shape))
    }

    fn centre_of_mass(&self, shape: ShapeHandle) -> Result<Point3, Self::Error> {
        each!(self, k => k.centre_of_mass(shape))
    }

    fn bounds(&self, shape: ShapeHandle) -> Result<(Point3, Point3), Self::Error> {
        each!(self, k => k.bounds(shape))
    }

    fn topology_counts(&self, shape: ShapeHandle) -> Result<TopologyCounts, Self::Error> {
        each!(self, k => k.topology_counts(shape))
    }

    fn distance(
        &self,
        a: ShapeHandle,
        a_part: SubShape,
        b: ShapeHandle,
        b_part: SubShape,
    ) -> Result<Option<Distance>, Self::Error> {
        each!(self, k => k.distance(a, a_part, b, b_part))
    }

    fn edge_tangent(
        &self,
        shape: ShapeHandle,
        edge: u32,
        near: Point3,
    ) -> Result<Option<EdgeTangent>, Self::Error> {
        each!(self, k => k.edge_tangent(shape, edge, near))
    }

    fn edge_curve(&self, shape: ShapeHandle, edge: u32) -> Result<Option<EdgeCurve>, Self::Error> {
        each!(self, k => k.edge_curve(shape, edge))
    }

    fn project_edges(
        &self,
        shapes: &[ShapeHandle],
        view: &Plane,
        deflection: f64,
    ) -> Result<Option<ProjectedEdges>, Self::Error> {
        each!(self, k => k.project_edges(shapes, view, deflection))
    }

    fn export_step(
        &self,
        shapes: &[ShapeHandle],
        colors: &[StepColors],
        materials: &[Option<StepMaterial>],
        path: &std::path::Path,
    ) -> Result<(), Self::Error> {
        each!(self, k => k.export_step(shapes, colors, materials, path))
    }

    fn export_iges(
        &self,
        shapes: &[ShapeHandle],
        path: &std::path::Path,
    ) -> Result<(), Self::Error> {
        each!(self, k => k.export_iges(shapes, path))
    }

    fn import_step(&mut self, data: &[u8]) -> Result<StepImport, Self::Error> {
        each!(self, k => k.import_step(data))
    }

    fn read_step_assembly(&mut self, data: &[u8]) -> Result<StepAssembly, Self::Error> {
        each!(self, k => k.read_step_assembly(data))
    }

    fn has_volume_mesher(&self) -> bool {
        each_plain!(self, k => k.has_volume_mesher())
    }

    fn volume_mesh(
        &self,
        shape: ShapeHandle,
        options: &VolumeMeshOptions,
    ) -> Result<VolumeMesh, VolumeMeshError> {
        each_plain!(self, k => k.volume_mesh(shape, options))
    }

    fn volume_mesh_watched(
        &self,
        shape: ShapeHandle,
        options: &VolumeMeshOptions,
        watch: MeshWatch<'_>,
    ) -> Result<VolumeMesh, VolumeMeshError> {
        each_plain!(self, k => k.volume_mesh_watched(shape, options, watch))
    }

    fn release(&mut self, shape: ShapeHandle) {
        each_plain!(self, k => k.release(shape))
    }

    /// A worker of the session's kernel ([`chosen`]), sharing its shapes.
    fn open_worker() -> Option<Self> {
        match chosen() {
            KernelKind::Occt => OcctKernel::open_worker().map(AppKernel::Occt),
            // The spike runs its jobs on the calling thread.
            #[cfg(feature = "solidcore")]
            KernelKind::Solidcore => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_built_by_another_kernel_says_so() {
        assert_eq!(rebuilt_warning(&PartKernel::Occt, KernelKind::Occt), None);
        let warning =
            rebuilt_warning(&PartKernel::Other("native".to_owned()), KernelKind::Occt).unwrap();
        assert!(
            warning.starts_with("built by a kernel named \"native\", rebuilt by OpenCASCADE"),
            "{warning}"
        );
        assert_eq!(KernelKind::Occt.file_kernel(), PartKernel::Occt);
        // What a session saves says which kernel built it.
        assert_eq!(
            crate::document::Document::new().to_file().kernel,
            chosen().file_kernel()
        );
    }

    #[test]
    fn kinds_parse_by_name() {
        assert_eq!(KernelKind::parse("occt"), Some(KernelKind::Occt));
        assert_eq!(KernelKind::parse("opencascade"), Some(KernelKind::Occt));
        assert_eq!(KernelKind::parse("parasolid"), None);
        assert!(KernelKind::names().starts_with("occt"));
        assert_eq!(KernelKind::default(), KernelKind::Occt);
    }

    #[test]
    fn the_preference_reads_the_kernel_by_name() {
        let kind: KernelKind = serde_json::from_str("\"occt\"").unwrap();
        assert_eq!(kind, KernelKind::Occt);
        assert_eq!(
            serde_json::to_string(&KernelKind::Occt).unwrap(),
            "\"occt\""
        );
        // Preferences saved before the choice existed open with the default kernel.
        let prefs: crate::prefs::Preferences = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs.kernel, KernelKind::Occt);
        let prefs: crate::prefs::Preferences =
            serde_json::from_str(r#"{"kernel":"occt"}"#).unwrap();
        assert_eq!(prefs.kernel, KernelKind::Occt);
    }

    #[test]
    fn the_session_kernel_builds_through_the_chosen_kernel() {
        let mut kernel = AppKernel::new(chosen());
        assert_eq!(kernel.kind(), KernelKind::Occt);
        assert!(description().starts_with("OpenCASCADE "));
        let shape = kernel
            .make_box(Point3::ORIGIN, Vec3::new(2.0, 3.0, 4.0))
            .unwrap();
        assert!((kernel.volume(shape).unwrap() - 24.0).abs() < 1e-6);
        // A worker shares the session kernel's shapes.
        let worker = AppKernel::open_worker().expect("OpenCASCADE opens workers");
        assert!((worker.volume(shape).unwrap() - 24.0).abs() < 1e-6);
        kernel.release(shape);
    }

    #[cfg(feature = "solidcore")]
    #[test]
    fn solidcore_is_chosen_by_name_and_builds_a_box() {
        assert_eq!(KernelKind::parse("solidcore"), Some(KernelKind::Solidcore));
        assert_eq!(KernelKind::Solidcore.file_kernel(), PartKernel::Solidcore);
        let mut kernel = AppKernel::new(KernelKind::Solidcore);
        assert!(kernel.occt_mut().is_none());
        let shape = kernel
            .make_box(Point3::ORIGIN, Vec3::new(2.0, 3.0, 4.0))
            .unwrap();
        assert!((kernel.volume(shape).unwrap() - 24.0).abs() < 1e-6);
        let error = kernel.fillet(shape, &[0], 0.5).unwrap_err();
        assert_eq!(
            error.to_string(),
            "the solidcore kernel cannot fillet yet (spike)"
        );
    }
}
