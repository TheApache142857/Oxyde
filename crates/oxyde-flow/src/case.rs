//! Writing a [`FlowStudy`] as an OpenFOAM case (OpenCFD's line, v2606).
//!
//! The case meshes the fluid itself: `blockMesh` fills the computational domain with
//! cubes, `surfaceFeatureExtract` finds the surfaces' sharp edges, and `snappyHexMesh`
//! refines the cubes along the surfaces, keeps those on the fluid's side of a point
//! (`locationInMesh`: inside the cavity of an internal flow, in a corner of the domain for
//! an external one), snaps them onto the surfaces and grows layers along walls. Every model
//! face is a region of the STL file, so it becomes a boundary patch of its own, named after
//! its role and label (`wall_12`, `inlet_3`): conditions, goals and results all work face
//! by face. `simpleFoam` then solves the steady incompressible flow, laminar or with the
//! k-ω SST model, after `potentialFoam` has given it a start.
//!
//! Initial fields go to `0.orig` and are copied to `0` once the mesh is made, and the
//! solver's `controlDict` (with the goals' function objects) waits as
//! `controlDict.solve` until then: the meshing programs see neither.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::dict::{header, num, vector};
use crate::study::{
    ConditionKind, FlowStudy, FlowType, GoalKind, StudyError, Turbulence, cross, norm, sub,
};

/// Metres per millimetre: OpenFOAM's cases are in metres.
pub(crate) const M_PER_MM: f64 = 1e-3;

/// The name of the patch made from the computational domain's box.
pub const DOMAIN_PATCH: &str = "domain";

/// What a model face is to the flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// A wall the fluid sticks to.
    Wall,
    /// Fluid comes in through it.
    Inlet,
    /// An opening at a pressure.
    Outlet,
    /// A wall without friction.
    Slip,
}

impl Role {
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            Role::Wall => "wall",
            Role::Inlet => "inlet",
            Role::Outlet => "outlet",
            Role::Slip => "slip",
        }
    }
}

/// The boundary patch made from one model face.
#[derive(Debug, Clone, PartialEq)]
pub struct FacePatch {
    /// The patch's name in the case, such as `wall_12`.
    pub name: String,
    /// The model face.
    pub label: u32,
    pub role: Role,
    /// The face's area on the surfaces, m².
    pub area: f64,
}

/// A goal, as the case measures it.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseGoal {
    pub name: String,
    pub kind: GoalKind,
    pub labels: Vec<u32>,
    /// The function object that writes it while the solver runs (forces), whose output is
    /// `postProcessing/<object>/<start time>/force.dat`; other goals are worked out from the
    /// fields afterwards.
    pub object: Option<String>,
}

/// A case written to disk, with what running it and reading it back need.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    /// The case folder.
    pub dir: PathBuf,
    /// One patch per model face, in label order.
    pub patches: Vec<FacePatch>,
    /// The solver, `simpleFoam`.
    pub solver: String,
    /// Processes the solver runs on.
    pub processes: u32,
    /// The most iterations.
    pub iterations: u32,
    /// The fluid's density, kg/m³: OpenFOAM's incompressible pressures are divided by it.
    pub density: f64,
    /// Whether k-ω SST fields (`k`, `omega`, `nut`) are solved.
    pub turbulent: bool,
    /// An internal flow: its mesh must not reach the domain's box.
    pub internal: bool,
    pub goals: Vec<CaseGoal>,
    /// A cooling study, solved by `chtMultiRegionSimpleFoam` in a fluid and a solid region
    /// ([`crate::cooling`]): its regions' files are written from it once the mesh is split.
    /// `None` for a flow alone.
    pub cooling: Option<Box<FlowStudy>>,
}

impl Case {
    /// The patch made from a model face.
    pub fn patch_of(&self, label: u32) -> Option<&FacePatch> {
        self.patches.iter().find(|p| p.label == label)
    }

    /// The model face a patch was made from; `None` for the domain's patch.
    pub fn label_of(&self, patch: &str) -> Option<u32> {
        self.patches
            .iter()
            .find(|p| p.name == patch)
            .map(|p| p.label)
    }
}

/// Why a case could not be written.
#[derive(Debug)]
pub enum CaseError {
    /// The study cannot be meshed or solved as it stands.
    Study(StudyError),
    /// A file or folder could not be written.
    Io { path: PathBuf, error: io::Error },
    /// The case folder holds something, and it is not an OpenFOAM case of Oxyde's.
    InTheWay { path: PathBuf },
}

impl fmt::Display for CaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaseError::Study(e) => write!(f, "{e}"),
            CaseError::Io { path, error } => {
                write!(f, "could not write {}: {error}", path.display())
            }
            CaseError::InTheWay { path } => write!(
                f,
                "{} is in the way: it is not an OpenFOAM case of Oxyde's",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CaseError {}

impl From<StudyError> for CaseError {
    fn from(e: StudyError) -> Self {
        CaseError::Study(e)
    }
}

/// Writes `study` as an OpenFOAM case in `dir`, which is made if needed. What an earlier
/// case left there (mesh, results, logs, the files written here) is removed first; nothing
/// else in the folder is touched. A folder that holds something and is not a case of
/// Oxyde's is refused.
pub fn write_case(dir: &Path, study: &FlowStudy) -> Result<Case, CaseError> {
    study.check()?;
    clear_outputs(dir)?;

    let condition_of = study.condition_of();
    let areas = study.areas();
    let patches: Vec<FacePatch> = study
        .labels()
        .into_iter()
        .map(|label| {
            let role = match condition_of
                .get(&label)
                .and_then(|&c| study.conditions.get(c))
                .map(|c| c.kind)
            {
                None => Role::Wall,
                Some(
                    ConditionKind::InletVelocity { .. }
                    | ConditionKind::InletVolumeFlow { .. }
                    | ConditionKind::InletMassFlow { .. },
                ) => Role::Inlet,
                Some(ConditionKind::OutletPressure { .. }) => Role::Outlet,
                Some(ConditionKind::IdealWall) => Role::Slip,
            };
            FacePatch {
                name: format!("{}_{label}", role.prefix()),
                label,
                role,
                area: areas.get(&label).copied().unwrap_or(0.0) * M_PER_MM * M_PER_MM,
            }
        })
        .collect();

    let goals: Vec<CaseGoal> = study
        .goals
        .iter()
        .enumerate()
        .map(|(i, g)| CaseGoal {
            name: g.name.clone(),
            kind: g.kind,
            labels: g.labels.clone(),
            object: (g.kind == GoalKind::Force).then(|| format!("force_{i}")),
        })
        .collect();

    let case = Case {
        dir: dir.to_path_buf(),
        patches,
        solver: if study.heat.is_some() {
            "chtMultiRegionSimpleFoam"
        } else {
            "simpleFoam"
        }
        .to_string(),
        processes: study.solver.processes,
        iterations: study.solver.iterations,
        density: study.fluid.density,
        turbulent: matches!(study.turbulence, Turbulence::KOmegaSst { .. }),
        internal: matches!(study.flow_type, FlowType::Internal { .. }),
        goals,
        cooling: study.heat.is_some().then(|| Box::new(study.clone())),
    };
    if study.heat.is_some() {
        crate::cooling::write(&case)?;
        return Ok(case);
    }

    put(
        &dir.join("constant/triSurface/model.stl"),
        &stl(study, &case),
    )?;
    put(&dir.join("system/blockMeshDict"), &block_mesh_dict(study))?;
    put(
        &dir.join("system/surfaceFeatureExtractDict"),
        &surface_feature_extract_dict(),
    )?;
    put(
        &dir.join("system/snappyHexMeshDict"),
        &snappy_hex_mesh_dict(study, &case),
    )?;
    put(&dir.join("system/meshQualityDict"), &mesh_quality_dict())?;
    put(
        &dir.join("system/controlDict"),
        &control_dict(study, &case, false),
    )?;
    put(
        &dir.join("system/controlDict.solve"),
        &control_dict(study, &case, true),
    )?;
    put(&dir.join("system/fvSchemes"), &fv_schemes())?;
    put(&dir.join("system/fvSolution"), &fv_solution(study))?;
    put(
        &dir.join("system/decomposeParDict"),
        &decompose_par_dict(study),
    )?;
    put(
        &dir.join("constant/transportProperties"),
        &transport_properties(study),
    )?;
    put(
        &dir.join("constant/turbulenceProperties"),
        &turbulence_properties(study),
    )?;
    for (name, text) in fields(study, &case) {
        put(&dir.join("0.orig").join(name), &text)?;
    }
    // An empty file by which ParaView opens the case.
    put(&dir.join("case.foam"), "")?;
    Ok(case)
}

/// Removes what an earlier case left in `dir`: the files this module writes, the mesh,
/// time folders, processor folders, post-processing output and logs. Only an empty folder
/// or one this module wrote (`case.foam` marks it) is cleared: what goes is picked by name,
/// which a folder of the user's could share.
pub(crate) fn clear_outputs(dir: &Path) -> Result<(), CaseError> {
    let io_err = |path: &Path, error| CaseError::Io {
        path: path.to_path_buf(),
        error,
    };
    if !dir.exists() {
        return fs::create_dir_all(dir).map_err(|e| io_err(dir, e));
    }
    if !dir.join("case.foam").is_file() {
        let mut entries = fs::read_dir(dir).map_err(|e| io_err(dir, e))?;
        if entries.next().is_some() {
            return Err(CaseError::InTheWay {
                path: dir.to_path_buf(),
            });
        }
    }
    for sub in [
        "constant/polyMesh",
        "constant/triSurface",
        "constant/extendedFeatureEdgeMesh",
        "constant/fluid",
        "constant/solid",
        "constant/outside",
        "system",
        "0.orig",
        "postProcessing",
    ] {
        let path = dir.join(sub);
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(|e| io_err(&path, e))?;
        }
    }
    let entries = fs::read_dir(dir).map_err(|e| io_err(dir, e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // OpenFOAM names time folders by their time: `0`, `100`, `0.5`, `1e-05`.
        let time_dir =
            name.starts_with(|c: char| c.is_ascii_digit()) && name.parse::<f64>().is_ok();
        let processor_dir = name
            .strip_prefix("processor")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if path.is_dir() && (time_dir || processor_dir) {
            fs::remove_dir_all(&path).map_err(|e| io_err(&path, e))?;
        } else if path.is_file() && (name.starts_with("log.") || name == "case.foam") {
            fs::remove_file(&path).map_err(|e| io_err(&path, e))?;
        }
    }
    Ok(())
}

pub(crate) fn put(path: &Path, text: &str) -> Result<(), CaseError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| CaseError::Io {
            path: parent.to_path_buf(),
            error,
        })?;
    }
    fs::write(path, text).map_err(|error| CaseError::Io {
        path: path.to_path_buf(),
        error,
    })
}

pub(crate) fn metres(p: [f64; 3]) -> [f64; 3] {
    [p[0] * M_PER_MM, p[1] * M_PER_MM, p[2] * M_PER_MM]
}

/// The surfaces as one ASCII STL file in metres, one `solid` (region) per model face.
fn stl(study: &FlowStudy, case: &Case) -> String {
    // Triangles grouped by label, in label order.
    let mut by_label: BTreeMap<u32, Vec<[[f64; 3]; 3]>> = BTreeMap::new();
    for surface in &study.surfaces {
        for (t, &label) in surface.labels.iter().enumerate() {
            if let Some(corners) = surface.corners(t) {
                by_label.entry(label).or_default().push(corners);
            }
        }
    }
    let mut s = String::new();
    for patch in &case.patches {
        let Some(triangles) = by_label.get(&patch.label) else {
            continue;
        };
        let _ = writeln!(s, "solid {}", patch.name);
        for [a, b, c] in triangles {
            let n = cross(sub(*b, *a), sub(*c, *a));
            let len = norm(n);
            let n = if len > 0.0 {
                [n[0] / len, n[1] / len, n[2] / len]
            } else {
                [0.0; 3]
            };
            let _ = writeln!(
                s,
                "  facet normal {} {} {}",
                num(n[0]),
                num(n[1]),
                num(n[2])
            );
            s.push_str("    outer loop\n");
            for p in [a, b, c] {
                let m = metres(*p);
                let _ = writeln!(s, "      vertex {} {} {}", num(m[0]), num(m[1]), num(m[2]));
            }
            s.push_str("    endloop\n  endfacet\n");
        }
        let _ = writeln!(s, "endsolid {}", patch.name);
    }
    s
}

/// The computational domain cut into cubes of the cell size. The box grows to a whole
/// number of cubes along each axis.
pub(crate) fn block_mesh_dict(study: &FlowStudy) -> String {
    let n = study.background_cells();
    let h = study.mesh.cell_size;
    let min = study.domain.min;
    let max = [
        min[0] + n[0] as f64 * h,
        min[1] + n[1] as f64 * h,
        min[2] + n[2] as f64 * h,
    ];
    let (lo, hi) = (metres(min), metres(max));
    let mut s = header("dictionary", "blockMeshDict");
    s.push_str("scale 1;\n\nvertices\n(\n");
    for k in 0..8 {
        let x = if k == 1 || k == 2 || k == 5 || k == 6 {
            hi[0]
        } else {
            lo[0]
        };
        let y = if k == 2 || k == 3 || k == 6 || k == 7 {
            hi[1]
        } else {
            lo[1]
        };
        let z = if k >= 4 { hi[2] } else { lo[2] };
        let _ = writeln!(s, "    {}", vector([x, y, z]));
    }
    s.push_str(");\n\nblocks\n(\n");
    let _ = writeln!(
        s,
        "    hex (0 1 2 3 4 5 6 7) ({} {} {}) simpleGrading (1 1 1)",
        n[0], n[1], n[2]
    );
    s.push_str(");\n\nedges\n(\n);\n\nboundary\n(\n");
    // Inside an internal flow the box is meshed away; were the cavity to leak into it, its
    // faces would be walls, and wall functions need the patch to be one.
    let kind = match study.flow_type {
        FlowType::Internal { .. } => "wall",
        FlowType::External { .. } => "patch",
    };
    let _ = writeln!(s, "    {DOMAIN_PATCH}");
    let _ = writeln!(s, "    {{\n        type {kind};\n        faces\n        (");
    for face in [
        "(0 3 2 1)",
        "(4 5 6 7)",
        "(0 4 7 3)",
        "(2 6 5 1)",
        "(1 5 4 0)",
        "(3 7 6 2)",
    ] {
        let _ = writeln!(s, "            {face}");
    }
    s.push_str("        );\n    }\n);\n\nmergePatchPairs\n(\n);\n");
    s
}

fn surface_feature_extract_dict() -> String {
    let mut s = header("dictionary", "surfaceFeatureExtractDict");
    s.push_str(
        "model.stl
{
    extractionMethod    extractFromSurface;
    // Edges whose faces meet at less than this are sharp, and so are the edges between
    // two regions, which are the model's own face boundaries.
    includedAngle       150;
    subsetFeatures
    {
        nonManifoldEdges    no;
        openEdges           yes;
    }
    writeObj            no;
}
",
    );
    s
}

/// The point the mesh keeps the fluid side of, in metres: inside the cavity for an
/// internal flow, inside the domain's first cube for an external one. It is moved off the
/// planes cell faces lie on by a small odd fraction of the finest cell.
pub(crate) fn location_in_mesh(study: &FlowStudy) -> [f64; 3] {
    let fine = study.mesh.finest();
    let p = match study.flow_type {
        FlowType::Internal { point } => [
            point[0] + 0.0137 * fine,
            point[1] + 0.0271 * fine,
            point[2] + 0.0193 * fine,
        ],
        FlowType::External { .. } => {
            let h = study.mesh.cell_size;
            let min = study.domain.min;
            [
                min[0] + 0.5137 * h,
                min[1] + 0.5271 * h,
                min[2] + 0.5193 * h,
            ]
        }
    };
    metres(p)
}

fn snappy_hex_mesh_dict(study: &FlowStudy, case: &Case) -> String {
    let mesh = &study.mesh;
    let layers = mesh.layers > 0 && case.patches.iter().any(|p| p.role == Role::Wall);
    let mut s = header("dictionary", "snappyHexMeshDict");
    s.push_str("castellatedMesh true;\nsnap            true;\n");
    let _ = writeln!(
        s,
        "addLayers       {};\n",
        if layers { "true" } else { "false" }
    );

    s.push_str("geometry\n{\n    model.stl\n    {\n        type triSurfaceMesh;\n        name model;\n        regions\n        {\n");
    for p in &case.patches {
        let _ = writeln!(s, "            {0} {{ name {0}; }}", p.name);
    }
    s.push_str("        }\n    }\n}\n\n");

    let level = mesh.surface_level;
    s.push_str("castellatedMeshControls\n{\n");
    let _ = writeln!(s, "    maxLocalCells {};", mesh.max_cells);
    let _ = writeln!(s, "    maxGlobalCells {};", mesh.max_cells);
    s.push_str(
        "    minRefinementCells 10;\n    maxLoadUnbalance 0.10;\n    nCellsBetweenLevels 3;\n",
    );
    let _ = writeln!(
        s,
        "    features\n    (\n        {{ file \"model.eMesh\"; level {}; }}\n    );",
        mesh.feature_level
    );
    s.push_str("    refinementSurfaces\n    {\n        model\n        {\n");
    let _ = writeln!(s, "            level ({level} {level});");
    s.push_str("            regions\n            {\n");
    for p in &case.patches {
        let kind = if p.role == Role::Wall {
            "wall"
        } else {
            "patch"
        };
        let _ = writeln!(
            s,
            "                {} {{ level ({level} {level}); patchInfo {{ type {kind}; }} }}",
            p.name
        );
    }
    s.push_str("            }\n        }\n    }\n");
    s.push_str("    resolveFeatureAngle 30;\n    refinementRegions\n    {\n    }\n");
    let _ = writeln!(s, "    locationInMesh {};", vector(location_in_mesh(study)));
    s.push_str("    allowFreeStandingZoneFaces true;\n}\n\n");

    s.push_str(
        "snapControls
{
    nSmoothPatch 3;
    tolerance 2.0;
    nSolveIter 50;
    nRelaxIter 5;
    nFeatureSnapIter 10;
    implicitFeatureSnap false;
    explicitFeatureSnap true;
    multiRegionFeatureSnap false;
}

addLayersControls
{
    relativeSizes true;
    layers
    {
",
    );
    if layers {
        let _ = writeln!(
            s,
            "        \"wall_.*\" {{ nSurfaceLayers {}; }}",
            mesh.layers
        );
    }
    s.push_str(
        "    }
    expansionRatio 1.2;
    finalLayerThickness 0.4;
    minThickness 0.1;
    nGrow 0;
    featureAngle 130;
    slipFeatureAngle 30;
    nRelaxIter 3;
    nSmoothSurfaceNormals 1;
    nSmoothNormals 3;
    nSmoothThickness 10;
    maxFaceThicknessRatio 0.5;
    maxThicknessToMedialRatio 0.3;
    minMedialAxisAngle 90;
    nBufferCellsNoExtrude 0;
    nLayerIter 50;
}

meshQualityControls
{
    #include \"meshQualityDict\"
}

mergeTolerance 1e-6;
",
    );
    s
}

pub(crate) fn mesh_quality_dict() -> String {
    let mut s = header("dictionary", "meshQualityDict");
    s.push_str(
        "maxNonOrtho 65;
maxBoundarySkewness 20;
maxInternalSkewness 4;
maxConcave 80;
minVol 1e-13;
minTetQuality 1e-15;
minArea -1;
minTwist 0.02;
minDeterminant 0.001;
minFaceWeight 0.05;
minVolRatio 0.01;
minTriangleTwist -1;
nSmoothScale 4;
errorReduction 0.75;
relaxed
{
    maxNonOrtho 75;
}
",
    );
    s
}

/// The `controlDict`: the meshing programs' (`solve` false), or the solver's, with the
/// goals' function objects.
fn control_dict(study: &FlowStudy, case: &Case, solve: bool) -> String {
    let n = study.solver.iterations;
    let mut s = header("dictionary", "controlDict");
    let _ = writeln!(s, "application     {};", case.solver);
    s.push_str("startFrom       latestTime;\nstartTime       0;\nstopAt          endTime;\n");
    let _ = writeln!(s, "endTime         {n};");
    s.push_str("deltaT          1;\nwriteControl    timeStep;\n");
    let _ = writeln!(s, "writeInterval   {n};");
    s.push_str(
        "purgeWrite      0;
writeFormat     ascii;
writePrecision  10;
writeCompression off;
timeFormat      general;
timePrecision   6;
runTimeModifiable false;
",
    );
    if solve {
        s.push_str("\nfunctions\n{\n");
        for goal in &case.goals {
            let Some(object) = &goal.object else {
                continue;
            };
            let names: Vec<&str> = goal
                .labels
                .iter()
                .filter_map(|&l| case.patch_of(l).map(|p| p.name.as_str()))
                .collect();
            let _ = writeln!(s, "    {object}\n    {{");
            s.push_str(
                "        type            forces;
        libs            (forces);
        writeControl    timeStep;
        writeInterval   1;
        log             false;
",
            );
            let _ = writeln!(s, "        patches         ({});", names.join(" "));
            s.push_str("        rho             rhoInf;\n");
            let _ = writeln!(s, "        rhoInf          {};", num(case.density));
            s.push_str("        CofR            (0 0 0);\n    }\n");
        }
        s.push_str("}\n");
    }
    s
}

fn fv_schemes() -> String {
    let mut s = header("dictionary", "fvSchemes");
    s.push_str(
        "ddtSchemes
{
    default         steadyState;
}

gradSchemes
{
    default         Gauss linear;
    grad(U)         cellLimited Gauss linear 1;
    grad(k)         cellLimited Gauss linear 1;
    grad(omega)     cellLimited Gauss linear 1;
}

divSchemes
{
    default         none;
    div(phi,U)      bounded Gauss linearUpwindV grad(U);
    div(phi,k)      bounded Gauss upwind;
    div(phi,omega)  bounded Gauss upwind;
    div((nuEff*dev2(T(grad(U))))) Gauss linear;
}

laplacianSchemes
{
    default         Gauss linear limited corrected 0.5;
}

interpolationSchemes
{
    default         linear;
}

snGradSchemes
{
    default         limited corrected 0.5;
}

wallDist
{
    method          meshWave;
}
",
    );
    s
}

fn fv_solution(study: &FlowStudy) -> String {
    let r = num(study.solver.residual);
    let mut s = header("dictionary", "fvSolution");
    s.push_str(
        "solvers
{
    p
    {
        solver          GAMG;
        smoother        GaussSeidel;
        tolerance       1e-7;
        relTol          0.05;
    }

    Phi
    {
        $p;
    }

    \"(U|k|omega)\"
    {
        solver          smoothSolver;
        smoother        GaussSeidel;
        nSweeps         2;
        tolerance       1e-8;
        relTol          0.1;
    }
}

SIMPLE
{
    nNonOrthogonalCorrectors 1;
    consistent      yes;
    residualControl
    {
",
    );
    let _ = writeln!(s, "        p               {r};");
    let _ = writeln!(s, "        U               {r};");
    let _ = writeln!(s, "        \"(k|omega)\"     {r};");
    s.push_str(
        "    }
}

potentialFlow
{
    nNonOrthogonalCorrectors 5;
}

relaxationFactors
{
    equations
    {
        U               0.8;
        \"(k|omega)\"     0.7;
    }
}
",
    );
    s
}

pub(crate) fn decompose_par_dict(study: &FlowStudy) -> String {
    let mut s = header("dictionary", "decomposeParDict");
    let _ = writeln!(s, "numberOfSubdomains {};", study.solver.processes);
    s.push_str("method          scotch;\n");
    s
}

fn transport_properties(study: &FlowStudy) -> String {
    let mut s = header("dictionary", "transportProperties");
    s.push_str("transportModel  Newtonian;\n");
    let _ = writeln!(
        s,
        "nu              {};",
        num(study.fluid.kinematic_viscosity())
    );
    s
}

pub(crate) fn turbulence_properties(study: &FlowStudy) -> String {
    let mut s = header("dictionary", "turbulenceProperties");
    match study.turbulence {
        Turbulence::Laminar => s.push_str("simulationType  laminar;\n"),
        Turbulence::KOmegaSst { .. } => s.push_str(
            "simulationType  RAS;

RAS
{
    RASModel        kOmegaSST;
    turbulence      on;
    printCoeffs     on;
}
",
        ),
    }
    s
}

/// A typical speed of the flow, m/s: the free stream's, or the fastest inlet's. It sets the
/// starting turbulence.
pub(crate) fn reference_speed(study: &FlowStudy, case: &Case) -> f64 {
    let speed = match study.flow_type {
        FlowType::External { velocity } => norm(velocity),
        FlowType::Internal { .. } => study
            .conditions
            .iter()
            .map(|c| {
                let area: f64 = c
                    .labels
                    .iter()
                    .filter_map(|&l| case.patch_of(l).map(|p| p.area))
                    .sum();
                match c.kind {
                    ConditionKind::InletVelocity { speed } => speed,
                    ConditionKind::InletVolumeFlow { rate } if area > 0.0 => rate / area,
                    ConditionKind::InletMassFlow { rate } if area > 0.0 => {
                        rate / study.fluid.density / area
                    }
                    _ => 0.0,
                }
            })
            .fold(0.0, f64::max),
    };
    if speed > 0.0 && speed.is_finite() {
        speed
    } else {
        1.0
    }
}

/// The share of `labels`' total area that `patch` has: a condition's flow rate is shared
/// between its faces by area.
pub(crate) fn area_share(case: &Case, labels: &[u32], patch: &FacePatch) -> f64 {
    let total: f64 = labels
        .iter()
        .filter_map(|&l| case.patch_of(l).map(|q| q.area))
        .sum();
    if total > 0.0 { patch.area / total } else { 0.0 }
}

/// Velocity entering a face at `rate`, m³/s.
pub(crate) fn flow_rate_inlet(rate: f64) -> String {
    format!(
        "type            flowRateInletVelocity;\nvolumetricFlowRate {};\nextrapolateProfile no;\nvalue           uniform (0 0 0);",
        num(rate)
    )
}

/// One boundary condition entry.
pub(crate) fn entry(s: &mut String, patch: &str, body: &str) {
    let _ = writeln!(s, "    {patch}\n    {{");
    for line in body.lines() {
        let _ = writeln!(s, "        {line}");
    }
    s.push_str("    }\n");
}

/// The starting fields, by file name.
fn fields(study: &FlowStudy, case: &Case) -> Vec<(&'static str, String)> {
    let rho = study.fluid.density;
    let external = match study.flow_type {
        FlowType::External { velocity } => Some(velocity),
        FlowType::Internal { .. } => None,
    };
    let condition_of = study.condition_of();
    let kind_of = |label: u32| {
        condition_of
            .get(&label)
            .and_then(|&c| study.conditions.get(c))
            .map(|c| (c.kind, c))
    };

    // U, m/s.
    let u0 = external.unwrap_or([0.0; 3]);
    let mut u = header("volVectorField", "U");
    u.push_str("dimensions      [0 1 -1 0 0 0 0];\n\n");
    let _ = writeln!(
        u,
        "internalField   uniform {};\n\nboundaryField\n{{",
        vector(u0)
    );
    match external {
        Some(v) => entry(
            &mut u,
            DOMAIN_PATCH,
            &format!(
                "type            freestreamVelocity;\nfreestreamValue uniform {0};\nvalue           uniform {0};",
                vector(v)
            ),
        ),
        None => entry(&mut u, DOMAIN_PATCH, "type            noSlip;"),
    }
    for p in &case.patches {
        let body = match kind_of(p.label) {
            None => "type            noSlip;".to_string(),
            Some((ConditionKind::InletVelocity { speed }, _)) => format!(
                "type            surfaceNormalFixedValue;\nrefValue        uniform {};",
                num(-speed)
            ),
            Some((ConditionKind::InletVolumeFlow { rate }, c)) => {
                flow_rate_inlet(rate * area_share(case, &c.labels, p))
            }
            Some((ConditionKind::InletMassFlow { rate }, c)) => {
                flow_rate_inlet(rate / rho * area_share(case, &c.labels, p))
            }
            Some((ConditionKind::OutletPressure { .. }, _)) => {
                "type            pressureInletOutletVelocity;\nvalue           uniform (0 0 0);"
                    .to_string()
            }
            Some((ConditionKind::IdealWall, _)) => "type            slip;".to_string(),
        };
        entry(&mut u, &p.name, &body);
    }
    u.push_str("}\n");

    // p, the pressure over the density, m²/s².
    let mut p_field = header("volScalarField", "p");
    p_field.push_str(
        "dimensions      [0 2 -2 0 0 0 0];\n\ninternalField   uniform 0;\n\nboundaryField\n{\n",
    );
    match external {
        Some(_) => entry(
            &mut p_field,
            DOMAIN_PATCH,
            "type            freestreamPressure;\nfreestreamValue uniform 0;\nvalue           uniform 0;",
        ),
        None => entry(&mut p_field, DOMAIN_PATCH, "type            zeroGradient;"),
    }
    for p in &case.patches {
        let body = match kind_of(p.label) {
            Some((ConditionKind::OutletPressure { pressure }, _)) => format!(
                "type            fixedValue;\nvalue           uniform {};",
                num(pressure / rho)
            ),
            _ => "type            zeroGradient;".to_string(),
        };
        entry(&mut p_field, &p.name, &body);
    }
    p_field.push_str("}\n");

    let mut out = vec![("U", u), ("p", p_field)];

    if let Turbulence::KOmegaSst { intensity, length } = study.turbulence {
        let speed = reference_speed(study, case);
        let k0 = (1.5 * (intensity * speed).powi(2)).max(1e-10);
        let mixing = length * M_PER_MM;
        // ω = √k / (Cμ^¼ ℓ), Cμ = 0.09.
        let w0 = (k0.sqrt() / (0.09f64.powf(0.25) * mixing)).max(1e-6);
        let (k0s, w0s) = (num(k0), num(w0));

        let mut k = header("volScalarField", "k");
        k.push_str("dimensions      [0 2 -2 0 0 0 0];\n\n");
        let _ = writeln!(k, "internalField   uniform {k0s};\n\nboundaryField\n{{");
        let mut omega = header("volScalarField", "omega");
        omega.push_str("dimensions      [0 0 -1 0 0 0 0];\n\n");
        let _ = writeln!(omega, "internalField   uniform {w0s};\n\nboundaryField\n{{");
        let mut nut = header("volScalarField", "nut");
        nut.push_str(
            "dimensions      [0 2 -1 0 0 0 0];\n\ninternalField   uniform 0;\n\nboundaryField\n{\n",
        );

        let k_wall = format!("type            kqRWallFunction;\nvalue           uniform {k0s};");
        let w_wall = format!("type            omegaWallFunction;\nvalue           uniform {w0s};");
        let nut_wall = "type            nutUSpaldingWallFunction;\nvalue           uniform 0;";
        let k_open = format!(
            "type            inletOutlet;\ninletValue      uniform {k0s};\nvalue           uniform {k0s};"
        );
        let w_open = format!(
            "type            inletOutlet;\ninletValue      uniform {w0s};\nvalue           uniform {w0s};"
        );
        let calculated = "type            calculated;\nvalue           uniform 0;";
        let zero_gradient = "type            zeroGradient;";

        match external {
            Some(_) => {
                entry(&mut k, DOMAIN_PATCH, &k_open);
                entry(&mut omega, DOMAIN_PATCH, &w_open);
                entry(&mut nut, DOMAIN_PATCH, calculated);
            }
            None => {
                entry(&mut k, DOMAIN_PATCH, &k_wall);
                entry(&mut omega, DOMAIN_PATCH, &w_wall);
                entry(&mut nut, DOMAIN_PATCH, nut_wall);
            }
        }
        for p in &case.patches {
            match p.role {
                Role::Wall => {
                    entry(&mut k, &p.name, &k_wall);
                    entry(&mut omega, &p.name, &w_wall);
                    entry(&mut nut, &p.name, nut_wall);
                }
                Role::Inlet => {
                    entry(
                        &mut k,
                        &p.name,
                        &format!(
                            "type            turbulentIntensityKineticEnergyInlet;\nintensity       {};\nvalue           uniform {k0s};",
                            num(intensity)
                        ),
                    );
                    entry(
                        &mut omega,
                        &p.name,
                        &format!(
                            "type            turbulentMixingLengthFrequencyInlet;\nmixingLength    {};\nvalue           uniform {w0s};",
                            num(mixing)
                        ),
                    );
                    entry(&mut nut, &p.name, calculated);
                }
                Role::Outlet => {
                    entry(&mut k, &p.name, &k_open);
                    entry(&mut omega, &p.name, &w_open);
                    entry(&mut nut, &p.name, calculated);
                }
                Role::Slip => {
                    entry(&mut k, &p.name, zero_gradient);
                    entry(&mut omega, &p.name, zero_gradient);
                    entry(&mut nut, &p.name, calculated);
                }
            }
        }
        for f in [&mut k, &mut omega, &mut nut] {
            f.push_str("}\n");
        }
        out.push(("k", k));
        out.push(("omega", omega));
        out.push(("nut", nut));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::study::{
        Condition, Domain, Fluid, Goal, MeshSettings, SolverSettings, Surface, cube,
    };

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oxyde-flow-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// A 10 mm box whose −x side lets 1 cm³/s in and +x side out at 100 Pa.
    fn study() -> FlowStudy {
        let surface: Surface = cube(10.0, [0.0; 3]);
        let bounds = surface.bounds().unwrap();
        FlowStudy {
            surfaces: vec![surface],
            flow_type: FlowType::Internal { point: [0.0; 3] },
            domain: Domain::internal(bounds, 2.0),
            fluid: Fluid::water(),
            turbulence: Turbulence::Laminar,
            conditions: vec![
                Condition {
                    kind: ConditionKind::InletVolumeFlow { rate: 1e-6 },
                    labels: vec![0],
                },
                Condition {
                    kind: ConditionKind::OutletPressure { pressure: 100.0 },
                    labels: vec![1],
                },
                Condition {
                    kind: ConditionKind::IdealWall,
                    labels: vec![2],
                },
            ],
            goals: vec![Goal {
                name: "Drag".to_string(),
                kind: GoalKind::Force,
                labels: vec![3, 4],
            }],
            mesh: MeshSettings::new(1.0),
            solver: SolverSettings::default(),
            heat: None,
        }
    }

    fn read(dir: &Path, file: &str) -> String {
        fs::read_to_string(dir.join(file)).unwrap()
    }

    /// The `locationInMesh` of a written case, mm.
    fn location(dir: &Path) -> [f64; 3] {
        let snappy = read(dir, "system/snappyHexMeshDict");
        let line = snappy
            .lines()
            .find(|l| l.contains("locationInMesh"))
            .unwrap();
        let inner = line.split('(').nth(1).unwrap().split(')').next().unwrap();
        let v: Vec<f64> = inner
            .split_whitespace()
            .map(|x| x.parse::<f64>().unwrap() / M_PER_MM)
            .collect();
        [v[0], v[1], v[2]]
    }

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-9)
    }

    #[test]
    fn a_case_has_a_patch_per_face_named_by_its_role() {
        let dir = scratch("patches");
        let case = write_case(&dir, &study()).unwrap();
        let names: Vec<&str> = case.patches.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "inlet_0", "outlet_1", "slip_2", "wall_3", "wall_4", "wall_5"
            ]
        );
        // 10 mm × 10 mm = 1e-4 m².
        assert!((case.patches[0].area - 1e-4).abs() < 1e-12);
        assert_eq!(case.label_of("wall_4"), Some(4));
        assert_eq!(case.label_of(DOMAIN_PATCH), None);

        let stl = read(&dir, "constant/triSurface/model.stl");
        assert_eq!(stl.matches("endsolid").count(), 6);
        assert!(stl.contains("solid inlet_0\n"));
        // Millimetres became metres.
        assert!(stl.contains("vertex -0.005 -0.005 -0.005"));

        let snappy = read(&dir, "system/snappyHexMeshDict");
        assert!(snappy.contains("wall_3 { name wall_3; }"));
        assert!(snappy.contains("wall_3 { level (1 1); patchInfo { type wall; } }"));
        assert!(snappy.contains("inlet_0 { level (1 1); patchInfo { type patch; } }"));
        assert!(snappy.contains("addLayers       false;"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_background_mesh_is_whole_cubes_and_the_point_is_off_the_grid() {
        let dir = scratch("block");
        let mut s = study();
        s.domain.max[0] += 0.4; // 14.4 mm: rounds up to 15 cubes
        write_case(&dir, &s).unwrap();
        let block = read(&dir, "system/blockMeshDict");
        assert!(
            block.contains("hex (0 1 2 3 4 5 6 7) (15 14 14)"),
            "{block}"
        );
        assert!(block.contains("(-0.007 -0.007 -0.007)"));
        assert!(block.contains("(0.008 0.007 0.007)"));
        // A small odd fraction of the finest cell (0.5 mm) past the point given.
        let at = location(&dir);
        assert!(close(at, [0.00685, 0.01355, 0.00965]), "{at:?}");
        assert!(
            block.contains("type wall;"),
            "the box is a wall in internal flows"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn conditions_become_boundary_conditions() {
        let dir = scratch("fields");
        let mut s = study();
        // Two faces share the inlet's flow by area: the second is a 10 × 10 side too.
        s.conditions[0].labels = vec![0, 5];
        let case = write_case(&dir, &s).unwrap();
        assert_eq!(case.patch_of(5).unwrap().role, Role::Inlet);
        let u = read(&dir, "0.orig/U");
        assert_eq!(u.matches("volumetricFlowRate 5e-7;").count(), 2, "{u}");
        assert!(u.contains("    slip_2\n    {\n        type            slip;"));
        assert!(u.contains("    wall_3\n    {\n        type            noSlip;"));
        assert!(u.contains("pressureInletOutletVelocity"));
        let p = read(&dir, "0.orig/p");
        // 100 Pa over 998.2 kg/m³.
        let expected = format!("value           uniform {};", num(100.0 / 998.2));
        assert!(p.contains(&expected), "{p}");
        assert!(!dir.join("0.orig/k").exists());
        let turbulence = read(&dir, "constant/turbulenceProperties");
        assert!(turbulence.contains("simulationType  laminar;"));
        let transport = read(&dir, "constant/transportProperties");
        assert!(transport.contains(&format!("nu              {};", num(1.002e-3 / 998.2))));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn turbulent_cases_start_from_the_inlet_turbulence() {
        let dir = scratch("sst");
        let mut s = study();
        s.turbulence = Turbulence::KOmegaSst {
            intensity: 0.05,
            length: 1.0,
        };
        s.mesh.layers = 3;
        let case = write_case(&dir, &s).unwrap();
        assert!(case.turbulent);
        // 1 cm³/s through 1 cm²: 0.01 m/s; k = 1.5 (0.05 × 0.01)² = 3.75e-7.
        let k = read(&dir, "0.orig/k");
        let k0: f64 = k
            .lines()
            .find_map(|l| l.strip_prefix("internalField   uniform "))
            .and_then(|v| v.trim_end_matches(';').parse().ok())
            .unwrap();
        assert!((k0 / 3.75e-7 - 1.0).abs() < 1e-9, "{k0}");
        assert!(k.contains("turbulentIntensityKineticEnergyInlet"));
        let omega = read(&dir, "0.orig/omega");
        assert!(omega.contains("mixingLength    0.001;"));
        let nut = read(&dir, "0.orig/nut");
        assert!(nut.contains("nutUSpaldingWallFunction"));
        let snappy = read(&dir, "system/snappyHexMeshDict");
        assert!(snappy.contains("addLayers       true;"));
        assert!(snappy.contains("\"wall_.*\" { nSurfaceLayers 3; }"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn external_flows_have_a_free_stream_all_round() {
        let dir = scratch("external");
        let mut s = study();
        s.flow_type = FlowType::External {
            velocity: [2.0, 0.0, 0.0],
        };
        s.conditions.clear();
        s.domain = Domain::external(s.bounds().unwrap(), [2.0, 0.0, 0.0]);
        s.mesh = MeshSettings::new(5.0);
        let case = write_case(&dir, &s).unwrap();
        assert!(case.patches.iter().all(|p| p.role == Role::Wall));
        let u = read(&dir, "0.orig/U");
        assert!(u.contains("internalField   uniform (2 0 0);"));
        assert!(u.contains("freestreamVelocity"));
        let p = read(&dir, "0.orig/p");
        assert!(p.contains("freestreamPressure"));
        // The domain starts twice the cube's size (20 mm) before its −5 mm side, and the
        // point sits about half a cube (5 mm) inside that corner.
        let at = location(&dir);
        assert!(close(at, [-22.4315, -22.3645, -22.4035]), "{at:?}");
        let block = read(&dir, "system/blockMeshDict");
        assert!(block.contains("type patch;"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn force_goals_are_function_objects_of_the_solver_only() {
        let dir = scratch("goals");
        let case = write_case(&dir, &study()).unwrap();
        assert_eq!(case.goals[0].object.as_deref(), Some("force_0"));
        let meshing = read(&dir, "system/controlDict");
        assert!(!meshing.contains("functions"));
        let solving = read(&dir, "system/controlDict.solve");
        assert!(solving.contains("patches         (wall_3 wall_4);"));
        assert!(solving.contains("rhoInf          998.2;"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_write_clears_what_the_first_run_left() {
        let dir = scratch("clear");
        write_case(&dir, &study()).unwrap();
        for leftover in ["100", "processor0", "postProcessing", "constant/polyMesh"] {
            fs::create_dir_all(dir.join(leftover)).unwrap();
        }
        fs::write(dir.join("log.simpleFoam"), "old").unwrap();
        fs::write(dir.join("notes.txt"), "mine").unwrap();
        fs::create_dir_all(dir.join("processorStuff")).unwrap();
        write_case(&dir, &study()).unwrap();
        for gone in [
            "100",
            "processor0",
            "postProcessing",
            "constant/polyMesh",
            "log.simpleFoam",
        ] {
            assert!(!dir.join(gone).exists(), "{gone}");
        }
        assert!(dir.join("notes.txt").exists());
        assert!(dir.join("processorStuff").exists());
        assert!(dir.join("0.orig/U").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A folder with something in it that is not a case of Oxyde's is refused untouched,
    /// even when it holds folders a case would have cleared.
    #[test]
    fn a_folder_that_is_not_a_case_is_left_alone() {
        let dir = scratch("inway");
        fs::create_dir_all(dir.join("system")).unwrap();
        fs::create_dir_all(dir.join("100")).unwrap();
        fs::write(dir.join("notes.txt"), "mine").unwrap();
        assert!(matches!(
            write_case(&dir, &study()),
            Err(CaseError::InTheWay { .. })
        ));
        for kept in ["system", "100", "notes.txt"] {
            assert!(dir.join(kept).exists(), "{kept}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_study_writes_nothing() {
        let dir = scratch("bad");
        let mut s = study();
        s.conditions.remove(1);
        assert!(matches!(
            write_case(&dir, &s),
            Err(CaseError::Study(StudyError::NoPressureOpening))
        ));
        assert!(!dir.exists());
    }
}
