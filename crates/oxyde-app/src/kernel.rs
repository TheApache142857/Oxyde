//! The kernel the application builds parts with: OpenCASCADE (`oxyde-occt`) or Oxyde's own
//! (`oxyde-native-kernel`), each compiled in by a Cargo feature (`occt`, `native`, both by
//! default) and one chosen when the application starts, for the whole session: by
//! `--kernel occt|native`, else by Preferences › Debug › Kernel (docs/ARCHITECTURE.md §10,
//! Native kernel). Changing the preference takes effect at the next start, so that every
//! shape of a session comes from one kernel.
//!
//! A part file records the kernel that built it (`PartKernel`, FORMAT.md §2). Opened by the
//! other one, the part is rebuilt by the kernel the session has, with a warning: the two may
//! cut faces differently, so a reference to a face or an edge may find another or none.

use std::sync::OnceLock;

use oxyde_format::PartKernel;
use oxyde_geom::{Axis, Loop, Path as SweepPath, Plane, Point3, Profile, Transform, Vec3};
use oxyde_kernel::{
    BooleanOp, ChamferSecond, Distance, EdgeTangent, Kernel, Mesh, MeshWatch, OpResult,
    ProjectedEdges, ShapeHandle, StepAssembly, StepColors, StepImport, StepMaterial, SubShape,
    Topology, TopologyCounts, VolumeMesh, VolumeMeshError, VolumeMeshOptions,
};
#[cfg(feature = "native")]
use oxyde_native_kernel::{NativeError, NativeKernel};
#[cfg(feature = "occt")]
use oxyde_occt::{OcctError, OcctKernel};
use serde::{Deserialize, Serialize};

#[cfg(not(any(feature = "occt", feature = "native")))]
compile_error!("Oxyde needs a kernel: build it with the `occt` feature, `native`, or both");

/// Which kernel builds the parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelKind {
    /// OpenCASCADE.
    Occt,
    /// Oxyde's own, still being written: what it cannot build yet fails with a message.
    Native,
}

impl Default for KernelKind {
    /// OpenCASCADE when it is compiled in: it builds everything.
    fn default() -> Self {
        if cfg!(feature = "occt") {
            KernelKind::Occt
        } else {
            KernelKind::Native
        }
    }
}

impl KernelKind {
    pub const ALL: [KernelKind; 2] = [KernelKind::Occt, KernelKind::Native];

    /// Its name for people.
    pub fn label(self) -> &'static str {
        match self {
            KernelKind::Occt => "OpenCASCADE",
            KernelKind::Native => "Oxyde's own (experimental)",
        }
    }

    /// Whether this build has it.
    pub fn available(self) -> bool {
        match self {
            KernelKind::Occt => cfg!(feature = "occt"),
            KernelKind::Native => cfg!(feature = "native"),
        }
    }

    /// As a part file records it.
    pub fn file_kernel(self) -> PartKernel {
        match self {
            KernelKind::Occt => PartKernel::Occt,
            KernelKind::Native => PartKernel::Native,
        }
    }

    /// The kind a `--kernel` argument names.
    pub fn parse(name: &str) -> Option<KernelKind> {
        match name {
            "occt" | "opencascade" => Some(KernelKind::Occt),
            "native" => Some(KernelKind::Native),
            _ => None,
        }
    }
}

/// The kernel of this session, chosen once ([`choose`]).
static CHOSEN: OnceLock<KernelKind> = OnceLock::new();

/// Makes `wanted` the session's kernel, or the one this build has when it lacks it; the
/// first choice holds. Returns the kernel chosen.
pub fn choose(wanted: KernelKind) -> KernelKind {
    let kind = if wanted.available() {
        wanted
    } else {
        KernelKind::default()
    };
    *CHOSEN.get_or_init(|| kind)
}

/// The session's kernel: the one chosen, or the default before any choice (in tests).
pub fn chosen() -> KernelKind {
    CHOSEN.get().copied().unwrap_or_default()
}

/// The session's kernel for people, with its version: in the About box and `--version`.
pub fn description() -> String {
    match chosen() {
        #[cfg(feature = "occt")]
        KernelKind::Occt => format!("OpenCASCADE {}", OcctKernel::occt_version()),
        kind => kind.label().to_owned(),
    }
}

/// What to tell the user about a part file built by `built_by`, rebuilt by `using`: nothing
/// when they are the same kernel.
pub fn rebuilt_warning(built_by: &PartKernel, using: KernelKind) -> Option<String> {
    let using_file = using.file_kernel();
    if *built_by == using_file {
        return None;
    }
    let was = match built_by {
        PartKernel::Occt => "OpenCASCADE".to_owned(),
        PartKernel::Native => "Oxyde's own kernel".to_owned(),
        PartKernel::Other(name) => format!("a kernel named \"{name}\""),
    };
    let now = match using {
        KernelKind::Occt => "OpenCASCADE",
        KernelKind::Native => "Oxyde's own kernel",
    };
    Some(format!(
        "built by {was}, rebuilt by {now}: check its features, faces and edges may be cut \
         and named differently"
    ))
}

/// The session's kernel: whichever of the compiled-in kernels was chosen, as one type.
pub enum AppKernel {
    #[cfg(feature = "occt")]
    Occt(OcctKernel),
    #[cfg(feature = "native")]
    Native(NativeKernel),
}

/// An error of the session's kernel.
#[derive(Debug)]
pub enum AppKernelError {
    #[cfg(feature = "occt")]
    Occt(OcctError),
    #[cfg(feature = "native")]
    Native(NativeError),
}

impl std::fmt::Display for AppKernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "occt")]
            AppKernelError::Occt(e) => e.fmt(f),
            #[cfg(feature = "native")]
            AppKernelError::Native(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for AppKernelError {}

impl AppKernel {
    /// A kernel of `kind`, or of the kind this build has when it lacks it.
    pub fn new(kind: KernelKind) -> AppKernel {
        match kind {
            #[cfg(feature = "native")]
            KernelKind::Native => AppKernel::Native(NativeKernel::new()),
            #[cfg(feature = "occt")]
            _ => AppKernel::Occt(OcctKernel::new()),
            #[cfg(not(feature = "occt"))]
            _ => AppKernel::Native(NativeKernel::new()),
        }
    }

    /// Which kernel it is.
    pub fn kind(&self) -> KernelKind {
        match self {
            #[cfg(feature = "occt")]
            AppKernel::Occt(_) => KernelKind::Occt,
            #[cfg(feature = "native")]
            AppKernel::Native(_) => KernelKind::Native,
        }
    }
}

/// Calls the same method on whichever kernel it is, its error made the session's.
macro_rules! each {
    ($self:expr, $k:ident => $call:expr) => {
        match $self {
            #[cfg(feature = "occt")]
            AppKernel::Occt($k) => $call.map_err(AppKernelError::Occt),
            #[cfg(feature = "native")]
            AppKernel::Native($k) => $call.map_err(AppKernelError::Native),
        }
    };
}

/// Calls the same method on whichever kernel it is, as it returns.
macro_rules! each_plain {
    ($self:expr, $k:ident => $call:expr) => {
        match $self {
            #[cfg(feature = "occt")]
            AppKernel::Occt($k) => $call,
            #[cfg(feature = "native")]
            AppKernel::Native($k) => $call,
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

    /// A worker of the session's kernel ([`chosen`]): one that shares its shapes, as the
    /// kernel offers one (OpenCASCADE does; Oxyde's own does not yet, and its jobs then run
    /// on the calling thread).
    fn open_worker() -> Option<Self> {
        match chosen() {
            #[cfg(feature = "native")]
            KernelKind::Native => NativeKernel::open_worker().map(AppKernel::Native),
            #[cfg(feature = "occt")]
            _ => OcctKernel::open_worker().map(AppKernel::Occt),
            #[cfg(not(feature = "occt"))]
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_rebuilt_by_another_kernel_says_so() {
        assert_eq!(rebuilt_warning(&PartKernel::Occt, KernelKind::Occt), None);
        assert_eq!(
            rebuilt_warning(&PartKernel::Native, KernelKind::Native),
            None
        );
        let warning = rebuilt_warning(&PartKernel::Occt, KernelKind::Native).unwrap();
        assert!(warning.starts_with("built by OpenCASCADE, rebuilt by Oxyde's own kernel"));
        let warning =
            rebuilt_warning(&PartKernel::Other("later".to_owned()), KernelKind::Occt).unwrap();
        assert!(warning.contains("a kernel named \"later\""));
    }

    #[test]
    fn kinds_parse_and_say_whether_they_are_compiled_in() {
        assert_eq!(KernelKind::parse("native"), Some(KernelKind::Native));
        assert_eq!(KernelKind::parse("occt"), Some(KernelKind::Occt));
        assert_eq!(KernelKind::parse("parasolid"), None);
        assert_eq!(KernelKind::Occt.available(), cfg!(feature = "occt"));
        assert_eq!(KernelKind::Native.available(), cfg!(feature = "native"));
        assert!(KernelKind::default().available());
        assert_eq!(KernelKind::Native.file_kernel(), PartKernel::Native);
    }

    #[test]
    fn a_part_built_by_the_other_kernel_opens_with_a_warning() {
        let session = KernelKind::default();
        let other = match session {
            KernelKind::Occt => PartKernel::Native,
            KernelKind::Native => PartKernel::Occt,
        };
        let dir = std::env::temp_dir().join(format!("oxyde-kernel-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut kernel = AppKernel::new(session);
        for (name, built_by, warned) in [
            ("same.oxyprt", session.file_kernel(), false),
            ("other.oxyprt", other, true),
        ] {
            let path = dir.join(name);
            let file = oxyde_format::PartFile {
                kernel: built_by,
                ..oxyde_format::PartFile::new()
            };
            oxyde_format::save_part(&path, &file).unwrap();
            let loaded = crate::assembly::load(&path, &mut kernel, 0.5).unwrap();
            assert_eq!(loaded.warning.is_some(), warned, "{name}");
        }
        // What a session saves says which kernel built it.
        assert_eq!(
            crate::document::Document::new().to_file().kernel,
            session.file_kernel()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "native")]
    #[test]
    fn the_native_kernel_builds_through_the_session_kernel() {
        let mut kernel = AppKernel::new(KernelKind::Native);
        assert_eq!(kernel.kind(), KernelKind::Native);
        let shape = kernel
            .make_box(Point3::ORIGIN, Vec3::new(2.0, 3.0, 4.0))
            .unwrap();
        assert!((kernel.volume(shape).unwrap() - 24.0).abs() < 1e-9);
        // What it cannot build yet is an error with the native kernel's message.
        let tetrahedron = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let error = kernel
            .solid_from_mesh(&tetrahedron, &[[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]])
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "the native kernel cannot make a solid from a mesh yet"
        );
        assert!(!kernel.has_volume_mesher());
        kernel.release(shape);
    }
}
