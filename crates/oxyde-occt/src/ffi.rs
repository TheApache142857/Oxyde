//! Rust ⇄ C++ bridge to the OCCT shim in `cpp/`.
//!
//! Only plain data and opaque handles cross this boundary — never OCCT types.

#[cxx::bridge(namespace = "oxyde::occt")]
pub(crate) mod bridge {
    struct TopologyCounts {
        solids: u32,
        shells: u32,
        faces: u32,
        edges: u32,
        vertices: u32,
    }

    // ---- Profile input -------------------------------------------------------------------

    struct PlaneData {
        ox: f64,
        oy: f64,
        oz: f64,
        xx: f64,
        xy: f64,
        xz: f64,
        nx: f64,
        ny: f64,
        nz: f64,
    }

    /// A line in space: origin and unit direction.
    struct AxisData {
        ox: f64,
        oy: f64,
        oz: f64,
        dx: f64,
        dy: f64,
        dz: f64,
    }

    enum SegmentKind {
        /// (x0, y0) → (x1, y1)
        Line,
        /// (x0, y0) through (x1, y1) to (x2, y2)
        Arc,
        /// center (x0, y0), radius r
        Circle,
    }

    struct SegmentData {
        id: u32,
        kind: SegmentKind,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        r: f64,
    }

    struct LoopData {
        segments: Vec<SegmentData>,
    }

    /// First loop is the outer boundary, the others are holes.
    struct RegionData {
        loops: Vec<LoopData>,
    }

    // ---- Operation results ---------------------------------------------------------------

    enum SourceKind {
        InputFace,
        InputEdge,
        InputVertex,
        OffsetFace,
        ProfileSegment,
        StartCap,
        EndCap,
    }

    /// `input` and `index` are meaningful depending on `kind`
    /// (`index` is the segment id for `ProfileSegment`).
    struct FaceSourceData {
        kind: SourceKind,
        input: u32,
        index: u32,
    }

    struct FaceHistoryData {
        sources: Vec<FaceSourceData>,
    }

    struct OpResultData {
        shape: u64,
        /// One entry per face of the result, in topology order.
        faces: Vec<FaceHistoryData>,
    }

    // ---- Topology ------------------------------------------------------------------------

    enum SurfaceKind {
        Plane,
        Cylinder,
        Cone,
        Sphere,
        Torus,
        BSpline,
        Other,
    }

    enum CurveKind {
        Line,
        Circle,
        Ellipse,
        BSpline,
        Degenerate,
        Other,
    }

    struct FaceData {
        surface: SurfaceKind,
        area: f64,
        cx: f64,
        cy: f64,
        cz: f64,
        has_normal: bool,
        nx: f64,
        ny: f64,
        nz: f64,
        /// Axis of a surface of revolution.
        has_axis: bool,
        axis: AxisData,
        edges: Vec<u32>,
    }

    struct EdgeData {
        curve: CurveKind,
        length: f64,
        mx: f64,
        my: f64,
        mz: f64,
        /// Line of a straight edge, or axis of a circular one.
        has_axis: bool,
        axis: AxisData,
        vertices: Vec<u32>,
        faces: Vec<u32>,
    }

    struct VertexData {
        x: f64,
        y: f64,
        z: f64,
    }

    struct TopologyData {
        faces: Vec<FaceData>,
        edges: Vec<EdgeData>,
        vertices: Vec<VertexData>,
    }

    // ---- Display mesh ----------------------------------------------------------------------

    struct FaceMeshData {
        /// x, y, z per node.
        positions: Vec<f32>,
        /// Three node indices per triangle.
        indices: Vec<u32>,
    }

    struct EdgeLineData {
        /// x, y, z per point.
        points: Vec<f32>,
    }

    struct MeshData {
        faces: Vec<FaceMeshData>,
        edges: Vec<EdgeLineData>,
    }

    /// An axis-aligned box.
    struct BoundsData {
        xmin: f64,
        ymin: f64,
        zmin: f64,
        xmax: f64,
        ymax: f64,
        zmax: f64,
    }

    // ---- STEP colours -----------------------------------------------------------------------

    /// A colour of a solid or a face in a STEP file; `present` false where there is none.
    struct StepColorData {
        present: bool,
        r: u8,
        g: u8,
        b: u8,
        /// 0 = opaque, 1 = invisible.
        transparency: f64,
    }

    /// The colours of one shape: per solid (in `TopExp` order) and per face (indexed like
    /// the shape's topology).
    struct StepShapeColors {
        solids: Vec<StepColorData>,
        faces: Vec<StepColorData>,
    }

    /// The material a STEP file gives a part (its `material name` and `density`
    /// properties, as Fusion 360 and SolidWorks write them per body). `present` false: none.
    struct StepMaterialData {
        present: bool,
        name: String,
        description: String,
        /// kg/m³; 0 when the file gives none.
        density: f64,
    }

    struct StepImportData {
        shape: u64,
        colors: StepShapeColors,
        material: StepMaterialData,
    }

    // ---- STEP assemblies --------------------------------------------------------------------

    struct StepInstanceData {
        /// Index into `StepAssemblyData::nodes`.
        node: u32,
        name: String,
        /// 3 × 4 rigid motion, row-major.
        rows: Vec<f64>,
    }

    struct StepNodeData {
        name: String,
        is_assembly: bool,
        /// A part's solids as a STEP file; empty when it has none.
        step: Vec<u8>,
        /// What the file says the part is made of.
        material: StepMaterialData,
        instances: Vec<StepInstanceData>,
    }

    struct StepAssemblyData {
        nodes: Vec<StepNodeData>,
        root: u32,
    }

    unsafe extern "C++" {
        include!("oxyde-occt/cpp/shim.h");

        /// A kernel for one thread. Shapes live in an arena shared by every session of the
        /// process, so a handle is valid in any of them.
        type Session;

        fn new_session() -> UniquePtr<Session>;

        /// Version of the OCCT headers the shim was compiled against, e.g. "7.8.1".
        fn occt_version() -> String;

        fn make_box(
            self: Pin<&mut Session>,
            x: f64,
            y: f64,
            z: f64,
            dx: f64,
            dy: f64,
            dz: f64,
        ) -> Result<u64>;

        fn extrude(
            self: Pin<&mut Session>,
            plane: &PlaneData,
            regions: &[RegionData],
            dx: f64,
            dy: f64,
            dz: f64,
        ) -> Result<OpResultData>;

        /// Revolves by `angle` radians about `axis` (right-hand rule).
        fn revolve(
            self: Pin<&mut Session>,
            plane: &PlaneData,
            regions: &[RegionData],
            axis: &AxisData,
            angle: f64,
        ) -> Result<OpResultData>;

        /// Thick solid: removes `faces`, offsets the rest by `offset` (negative = inward).
        fn shell(
            self: Pin<&mut Session>,
            shape: u64,
            faces: &[u32],
            offset: f64,
        ) -> Result<OpResultData>;

        /// Copy moved by the 3 × 4 matrix `rows` (row-major), which may be a mirror.
        fn transform(self: Pin<&mut Session>, shape: u64, rows: &[f64]) -> Result<OpResultData>;

        /// `kind`: 0 = fuse, 1 = cut, 2 = common; all `tools` at once.
        fn boolean_op(
            self: Pin<&mut Session>,
            kind: u8,
            target: u64,
            tools: &[u64],
        ) -> Result<OpResultData>;

        fn fillet(
            self: Pin<&mut Session>,
            shape: u64,
            edges: &[u32],
            radius: f64,
        ) -> Result<OpResultData>;

        fn chamfer(
            self: Pin<&mut Session>,
            shape: u64,
            edges: &[u32],
            distance: f64,
        ) -> Result<OpResultData>;

        fn release(self: Pin<&mut Session>, shape: u64);

        fn topology(self: &Session, shape: u64) -> Result<TopologyData>;

        fn tessellate(self: &Session, shape: u64, deflection: f64) -> Result<MeshData>;

        /// A fresh mesh with both a linear and an angular deflection (radians), for
        /// export; a triangulation made earlier is thrown away first.
        fn tessellate_with(
            self: &Session,
            shape: u64,
            deflection: f64,
            angle: f64,
        ) -> Result<MeshData>;

        fn volume(self: &Session, shape: u64) -> Result<f64>;

        fn bounds(self: &Session, shape: u64) -> Result<BoundsData>;

        fn topology_counts(self: &Session, shape: u64) -> Result<TopologyCounts>;

        /// Writes the shapes with their colours (`colors` indexed like `shapes`, or empty)
        /// and materials (`materials` indexed like `shapes`, or empty).
        fn export_step(
            self: &Session,
            shapes: &[u64],
            colors: &[StepShapeColors],
            materials: &[StepMaterialData],
            path: &str,
        ) -> Result<()>;

        /// Reads the solids of a STEP file held in memory, in millimetres, with their
        /// colours.
        fn import_step(self: Pin<&mut Session>, data: &[u8]) -> Result<StepImportData>;

        /// Reads the product structure of a STEP file held in memory: every part once, as
        /// its own STEP file, and every assembly with its placed instances. Children come
        /// before their parents.
        fn read_step_assembly(self: Pin<&mut Session>, data: &[u8]) -> Result<StepAssemblyData>;
    }
}

pub(crate) use bridge::*;
