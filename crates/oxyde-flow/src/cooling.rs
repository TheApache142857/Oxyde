//! Writing a cooling study (a [`FlowStudy`] with [`Heat`]) as an OpenFOAM case for
//! `chtMultiRegionSimpleFoam`: heat carried by the fluid and conducted through the solids,
//! the fluid and the solids meshed together.
//!
//! **Meshing.** Each solid is an STL file of its own (`body_<k>.stl`) with a region per model
//! face (`wall_<label>`). `snappyHexMesh` meshes the domain's box, puts the cells inside each
//! solid in a cell zone (`body_<k>`) and the faces between cells on a model face in a face
//! zone of their own (`body_<k>_wall_<label>`). `topoSet` then gathers the cells reached from
//! the fluid's point without crossing a solid in a `fluid` zone, and the cells left (around
//! an internal flow's model, or pockets the solids close off) in an `outside` zone, and
//! `splitMeshRegions` splits the mesh by zone into the regions `fluid`, `solid` (every body,
//! so that bodies that touch share faces of one mesh) and `outside`, which is not solved. It
//! makes a coupled pair of patches per model face between the fluid and the solid.
//!
//! **Finishing** ([`finish`], between `splitMeshRegions` and the solver) renames those patches
//! after their face and role: on the fluid's side `wall_12` (coupled to the solid's `wall_12`),
//! `inlet_3`, `outlet_7`, `slip_9`; on the solid's side `wall_12`, `free_3` (under an opening:
//! no heat crosses it) and `outer_15` (facing `outside`: adiabatic, or what a heat load says).
//! Then it writes every region's files for those patches.
//!
//! **The solid** is one region of a multi-component mixture: a species per material, its mass
//! fraction one in the cells of its bodies (`setFields`, from the bodies' zones), so heat
//! crosses from body to body inside one matrix. Bodies as regions of their own would be
//! coupled only from one iteration to the next, which converges hundreds of times slower where
//! they touch. The species share one specific heat, which a steady state never uses, so that
//! the enthalpy is continuous where the temperature is; conductivities meet harmonically
//! across a change of material. OpenFOAM's mixture mishandles mass fractions of exactly zero:
//! the other species keep [`FLOOR`].
//!
//! **The fluid** has constant properties and, with gravity, a density that falls as it warms
//! (Boussinesq's approximation): warmer fluid rises. Its pressures are absolute, around
//! [`P_REF`].

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::case::{
    Case, CaseError, DOMAIN_PATCH, M_PER_MM, Role, area_share, block_mesh_dict, decompose_par_dict,
    entry, flow_rate_inlet, location_in_mesh, mesh_quality_dict, metres, put, reference_speed,
    turbulence_properties,
};
use crate::dict::{header, num, vector};
use crate::foam::{Lexer, header as read_header};
use crate::run::RunError;
use crate::study::{
    ConditionKind, FlowStudy, FlowType, Heat, HeatLoadKind, Surface, Turbulence, cross, norm, sub,
};

/// The fluid's region.
pub const FLUID: &str = "fluid";
/// The solids' region.
pub const SOLID: &str = "solid";
/// The cells neither fluid nor solid: not solved.
pub const OUTSIDE: &str = "outside";

/// The reference pressure, Pa: OpenFOAM's pressures in a cooling case are absolute.
pub const P_REF: f64 = 1e5;

/// The mass fraction of the species a solid cell is not made of.
pub const FLOOR: f64 = 1e-8;

/// °C to K.
pub const KELVIN: f64 = 273.15;

/// The specific heat every solid species shares, J/(kg·K): a steady state never uses it.
const SOLID_CP: f64 = 1000.0;

/// A body's cell zone and STL file name.
pub fn body(k: usize) -> String {
    format!("body_{k}")
}

/// A material's species name in the solid's mixture.
fn species(m: usize) -> String {
    format!("material_{m}")
}

/// The materials of the solids, by conductivity and density, and each body's.
pub(crate) fn materials(heat: &Heat) -> (Vec<(f64, f64)>, Vec<usize>) {
    let mut materials: Vec<(f64, f64)> = Vec::new();
    let mut of_body = Vec::with_capacity(heat.solids.len());
    for solid in &heat.solids {
        let key = (solid.conductivity, solid.density);
        let m = match materials.iter().position(|&m| m == key) {
            Some(m) => m,
            None => {
                materials.push(key);
                materials.len() - 1
            }
        };
        of_body.push(m);
    }
    (materials, of_body)
}

/// Writes what meshing a cooling case takes, and the solver's `controlDict`; the regions'
/// files wait for [`finish`].
pub(crate) fn write(case: &Case) -> Result<(), CaseError> {
    let Some(study) = case.cooling.as_deref() else {
        return Ok(());
    };
    let dir = &case.dir;
    for (k, surface) in study.surfaces.iter().enumerate() {
        put(
            &dir.join(format!("constant/triSurface/{}.stl", body(k))),
            &stl(surface),
        )?;
    }
    put(&dir.join("system/blockMeshDict"), &block_mesh_dict(study))?;
    put(
        &dir.join("system/surfaceFeatureExtractDict"),
        &features_dict(study),
    )?;
    put(&dir.join("system/snappyHexMeshDict"), &snappy_dict(study))?;
    put(&dir.join("system/meshQualityDict"), &mesh_quality_dict())?;
    put(&dir.join("system/topoSetDict"), &topo_set_dict(study))?;
    put(&dir.join("system/controlDict"), &control_dict(case, false))?;
    put(
        &dir.join("system/controlDict.solve"),
        &control_dict(case, true),
    )?;
    // The meshing programs read the case's schemes and solution controls, if not the solver.
    put(&dir.join("system/fvSchemes"), &solid_schemes())?;
    put(
        &dir.join("system/fvSolution"),
        &header("dictionary", "fvSolution"),
    )?;
    put(
        &dir.join("system/decomposeParDict"),
        &decompose_par_dict(study),
    )?;
    put(&dir.join("case.foam"), "")
}

/// A solid's surface as an ASCII STL file in metres, a `solid` (region) per model face.
fn stl(surface: &Surface) -> String {
    let mut by_label: BTreeMap<u32, Vec<[[f64; 3]; 3]>> = BTreeMap::new();
    for (t, &label) in surface.labels.iter().enumerate() {
        if let Some(corners) = surface.corners(t) {
            by_label.entry(label).or_default().push(corners);
        }
    }
    let mut s = String::new();
    for (label, triangles) in by_label {
        let _ = writeln!(s, "solid wall_{label}");
        for [a, b, c] in triangles {
            let n = cross(sub(b, a), sub(c, a));
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
                let m = metres(p);
                let _ = writeln!(s, "      vertex {} {} {}", num(m[0]), num(m[1]), num(m[2]));
            }
            s.push_str("    endloop\n  endfacet\n");
        }
        let _ = writeln!(s, "endsolid wall_{label}");
    }
    s
}

fn features_dict(study: &FlowStudy) -> String {
    let mut s = header("dictionary", "surfaceFeatureExtractDict");
    for k in 0..study.surfaces.len() {
        let _ = writeln!(
            s,
            "{}.stl\n{{\n    extractionMethod    extractFromSurface;\n    includedAngle       150;\n    subsetFeatures\n    {{\n        nonManifoldEdges    no;\n        openEdges           yes;\n    }}\n    writeObj            no;\n}}\n",
            body(k)
        );
    }
    s
}

fn snappy_dict(study: &FlowStudy) -> String {
    let mesh = &study.mesh;
    let bodies = study.surfaces.len();
    let mut s = header("dictionary", "snappyHexMeshDict");
    s.push_str(
        "castellatedMesh true;\nsnap            true;\naddLayers       false;\n\ngeometry\n{\n",
    );
    for k in 0..bodies {
        let _ = writeln!(
            s,
            "    {0}.stl {{ type triSurfaceMesh; name {0}; }}",
            body(k)
        );
    }
    s.push_str("}\n\ncastellatedMeshControls\n{\n");
    let _ = writeln!(s, "    maxLocalCells {};", mesh.max_cells);
    let _ = writeln!(s, "    maxGlobalCells {};", mesh.max_cells);
    s.push_str(
        "    minRefinementCells 10;\n    maxLoadUnbalance 0.10;\n    nCellsBetweenLevels 3;\n    features\n    (\n",
    );
    for k in 0..bodies {
        let _ = writeln!(
            s,
            "        {{ file \"{}.eMesh\"; level {}; }}",
            body(k),
            mesh.feature_level
        );
    }
    s.push_str("    );\n    refinementSurfaces\n    {\n");
    let level = mesh.surface_level;
    for k in 0..bodies {
        // The cells inside the body are its zone; each of its faces a face zone of its own.
        let _ = writeln!(
            s,
            "        {0}\n        {{\n            level ({level} {level});\n            cellZone {0};\n            cellZoneInside inside;\n            faceZoneNaming region;\n        }}",
            body(k)
        );
    }
    s.push_str("    }\n    resolveFeatureAngle 30;\n    refinementRegions\n    {\n    }\n");
    let _ = writeln!(s, "    locationInMesh {};", vector(location_in_mesh(study)));
    s.push_str("    allowFreeStandingZoneFaces false;\n}\n\n");
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
    multiRegionFeatureSnap true;
}

addLayersControls
{
    relativeSizes true;
    layers
    {
    }
    expansionRatio 1.2;
    finalLayerThickness 0.4;
    minThickness 0.1;
    nGrow 0;
    featureAngle 130;
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

/// The fluid's cells (reached from its point without crossing a solid) and the rest of the
/// unzoned ones, each in a zone, so that the mesh splits by zone alone.
fn topo_set_dict(study: &FlowStudy) -> String {
    let zones: Vec<String> = (0..study.surfaces.len()).map(body).collect();
    let mut s = header("dictionary", "topoSetDict");
    s.push_str("actions\n(\n");
    let _ = writeln!(
        s,
        "    {{ name solids; type cellSet; action new; source zoneToCell; zones ({}); }}",
        zones.join(" ")
    );
    s.push_str(
        "    { name unzoned; type cellSet; action new; source cellToCell; sets (solids); }\n    { name unzoned; type cellSet; action invert; }\n",
    );
    let _ = writeln!(
        s,
        "    {{ name {FLUID}; type cellSet; action new; source regionToCell; set unzoned; insidePoints ({}); }}",
        vector(location_in_mesh(study))
    );
    let _ = writeln!(
        s,
        "    {{ name {FLUID}; type cellZoneSet; action new; source setToCellZone; set {FLUID}; }}"
    );
    let _ = writeln!(
        s,
        "    {{ name {OUTSIDE}; type cellSet; action new; source cellToCell; sets (unzoned); }}"
    );
    let _ = writeln!(
        s,
        "    {{ name {OUTSIDE}; type cellSet; action subtract; source cellToCell; sets ({FLUID}); }}"
    );
    let _ = writeln!(
        s,
        "    {{ name {OUTSIDE}; type cellZoneSet; action new; source setToCellZone; set {OUTSIDE}; }}"
    );
    s.push_str(");\n");
    s
}

/// The arguments of `splitMeshRegions`: by zone, a patch per face zone, every body in one
/// solid region.
pub fn split_arguments(case: &Case) -> Vec<String> {
    let bodies = case.cooling.as_deref().map_or(0, |s| s.surfaces.len());
    let zones: Vec<String> = (0..bodies).map(body).collect();
    vec![
        "-cellZonesOnly".to_string(),
        "-useFaceZones".to_string(),
        "-addZones".to_string(),
        format!("(({SOLID} {}))", zones.join(" ")),
        "-overwrite".to_string(),
    ]
}

/// Whether the solid needs `setFields` for its materials' mass fractions.
pub fn has_mixture(case: &Case) -> bool {
    case.cooling
        .as_deref()
        .and_then(|s| s.heat.as_ref())
        .is_some_and(|h| materials(h).0.len() > 1)
}

/// The `controlDict`: the meshing programs' (`solve` false), or the solver's, which re-reads
/// it while it runs (so that [`crate::run`] can stop it once converged) and measures forces.
fn control_dict(case: &Case, solve: bool) -> String {
    let n = case.iterations;
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
",
    );
    let _ = writeln!(
        s,
        "runTimeModifiable {};",
        if solve { "true" } else { "false" }
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
            let _ = writeln!(s, "        region          {FLUID};");
            let _ = writeln!(s, "        patches         ({});", names.join(" "));
            s.push_str("        rho             rho;\n");
            let _ = writeln!(s, "        pRef            {};", num(P_REF));
            s.push_str("        CofR            (0 0 0);\n    }\n");
        }
        s.push_str("}\n");
    }
    s
}

/// A boundary patch of a region, as [`finish`] leaves it.
#[derive(Debug, Clone, PartialEq)]
pub struct RegionPatch {
    pub name: String,
    /// `wall`, `patch`, `mappedWall`.
    pub kind: String,
    pub faces: usize,
    pub start: usize,
    /// The model face it lies on; `None` for the domain's box.
    pub label: Option<u32>,
}

/// After `splitMeshRegions`: renames the fluid's and the solid's patches after their face
/// and role, checks that an internal flow's fluid stayed inside the model, and writes every
/// region's files.
pub fn finish(case: &Case) -> Result<(), RunError> {
    let Some(study) = case.cooling.as_deref() else {
        return Ok(());
    };
    let Some(heat) = &study.heat else {
        return Ok(());
    };
    let dir = &case.dir;
    let roles: BTreeMap<u32, Role> = case.patches.iter().map(|p| (p.label, p.role)).collect();
    // A body with one face has its face zone named after the body alone.
    let mut single = BTreeMap::new();
    for (k, surface) in study.surfaces.iter().enumerate() {
        let mut labels: Vec<u32> = surface.labels.clone();
        labels.sort_unstable();
        labels.dedup();
        if let [label] = labels.as_slice() {
            single.insert(k, *label);
        }
    }
    let fluid = rename(dir, FLUID, &roles, &single)?;
    let solid = rename(dir, SOLID, &roles, &single)?;
    if fluid.iter().all(|p| p.faces == 0) {
        return Err(RunError::NoFluid);
    }
    if case.internal {
        let faces: usize = fluid
            .iter()
            .filter(|p| p.name == DOMAIN_PATCH)
            .map(|p| p.faces)
            .sum();
        if faces > 0 {
            return Err(RunError::Leak { faces });
        }
    }
    // `splitMeshRegions` leaves each region a field of the cells' region, under the patches'
    // old names: nothing needs it, and decomposing it would fail.
    for region in [FLUID, SOLID] {
        let field = dir.join("0").join(region).join("cellToRegion");
        if field.is_file() {
            fs::remove_file(&field).map_err(|error| RunError::Io { path: field, error })?;
        }
    }
    // The cells outside are not solved, and no patch is coupled to them any more: their
    // mesh would only fill the disk (round an internal flow's model, most of the cells).
    for base in ["constant", "0", "system"] {
        let folder = dir.join(base).join(OUTSIDE);
        if folder.is_dir() {
            fs::remove_dir_all(&folder).map_err(|error| RunError::Io {
                path: folder,
                error,
            })?;
        }
    }
    write_regions(case, study, heat, &fluid, &solid)
}

/// The renamed patch of `name` in `region`, its type, and the region and patch it is coupled
/// to. Names from `splitMeshRegions` are `body_<k>_wall_<label>_<region>_to_<other>`, or
/// `body_<k>_<region>_to_<other>` for a body of one face (`single` gives its label). A
/// coupled patch it cannot place becomes a plain wall.
fn renamed(
    region: &str,
    name: &str,
    kind: &str,
    roles: &BTreeMap<u32, Role>,
    single: &BTreeMap<usize, u32>,
) -> (String, String, Option<u32>, Option<(String, String)>) {
    let placed = zone_patch(name).and_then(|(body, label, other)| {
        label
            .or_else(|| single.get(&body).copied())
            .map(|l| (l, other))
    });
    let Some((label, other)) = placed else {
        let kind = if kind == "mappedWall" { "wall" } else { kind };
        return (name.to_string(), kind.to_string(), None, None);
    };
    let role = roles.get(&label).copied().unwrap_or(Role::Wall);
    let wall = format!("wall_{label}");
    if region == FLUID && other == SOLID {
        return match role {
            Role::Wall => (
                wall.clone(),
                "mappedWall".to_string(),
                Some(label),
                Some((SOLID.to_string(), wall)),
            ),
            Role::Inlet | Role::Outlet => (
                format!("{}_{label}", role.prefix()),
                "patch".to_string(),
                Some(label),
                None,
            ),
            Role::Slip => (
                format!("slip_{label}"),
                "wall".to_string(),
                Some(label),
                None,
            ),
        };
    }
    if region == SOLID && other == FLUID {
        return match role {
            Role::Wall => (
                wall.clone(),
                "mappedWall".to_string(),
                Some(label),
                Some((FLUID.to_string(), wall)),
            ),
            _ => (
                format!("free_{label}"),
                "wall".to_string(),
                Some(label),
                None,
            ),
        };
    }
    (
        format!("outer_{label}"),
        "wall".to_string(),
        Some(label),
        None,
    )
}

/// The body, the label (when the name has one) and the other region of a patch named
/// `body_<k>_wall_<label>_<a>_to_<b>` or `body_<k>_<a>_to_<b>`.
fn zone_patch(name: &str) -> Option<(usize, Option<u32>, String)> {
    let digits = |s: &str| s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let rest = name.strip_prefix("body_")?;
    let n = digits(rest);
    let body = rest.get(..n)?.parse().ok()?;
    let rest = rest.get(n..)?;
    let (label, rest) = match rest.strip_prefix("_wall_") {
        Some(after) => {
            let n = digits(after);
            (Some(after.get(..n)?.parse().ok()?), after.get(n..)?)
        }
        None => (None, rest),
    };
    let (_, other) = rest.strip_prefix('_')?.split_once("_to_")?;
    Some((body, label, other.to_string()))
}

/// Rewrites `constant/<region>/polyMesh/boundary` with the patches renamed and retyped; the
/// faces stay where they are.
fn rename(
    dir: &Path,
    region: &str,
    roles: &BTreeMap<u32, Role>,
    single: &BTreeMap<usize, u32>,
) -> Result<Vec<RegionPatch>, RunError> {
    let path = dir
        .join("constant")
        .join(region)
        .join("polyMesh")
        .join("boundary");
    let io = |error| RunError::Io {
        path: path.clone(),
        error,
    };
    let text = fs::read(&path).map_err(io)?;
    let parsed = boundary_entries(&text).ok_or_else(|| RunError::Io {
        path: path.clone(),
        error: std::io::Error::new(std::io::ErrorKind::InvalidData, "not a boundary file"),
    })?;
    let mut patches = Vec::new();
    let mut out = header("polyBoundaryMesh", "boundary");
    let _ = writeln!(out, "{}\n(", parsed.len());
    for (name, kind, faces, start) in parsed {
        let (new, kind, label, coupled) = renamed(region, &name, &kind, roles, single);
        let _ = writeln!(out, "    {new}\n    {{\n        type            {kind};");
        if kind != "patch" {
            out.push_str("        inGroups        List<word> 1(wall);\n");
        }
        let _ = writeln!(
            out,
            "        nFaces          {faces};\n        startFace       {start};"
        );
        if let Some((other, patch)) = &coupled {
            let _ = writeln!(
                out,
                "        sampleMode      nearestPatchFace;\n        sampleRegion    {other};\n        samplePatch     {patch};"
            );
        }
        out.push_str("    }\n");
        patches.push(RegionPatch {
            name: new,
            kind,
            faces,
            start,
            label,
        });
    }
    out.push_str(")\n");
    fs::write(&path, out).map_err(io)?;
    Ok(patches)
}

/// Each entry of a `boundary` file: name, type, faces and first face.
fn boundary_entries(text: &[u8]) -> Option<Vec<(String, String, usize, usize)>> {
    let mut lex = Lexer::new(text);
    read_header(&mut lex).ok()?;
    lex.list(|l| {
        let name = l.word()?.to_string();
        l.expect(b'{')?;
        let (mut kind, mut faces, mut start) = (String::new(), 0, 0);
        while !l.eat(b'}') {
            match l.word()? {
                "type" => {
                    kind = l.word()?.to_string();
                    l.expect(b';')?;
                }
                "nFaces" => {
                    faces = l.label()?;
                    l.expect(b';')?;
                }
                "startFace" => {
                    start = l.label()?;
                    l.expect(b';')?;
                }
                _ => l.skip_value()?,
            }
        }
        Ok((name, kind, faces, start))
    })
    .ok()
}

/// The cells owning `patches`' faces, from the region's `owner` file.
fn cells_next_to(dir: &Path, region: &str, patches: &[&RegionPatch]) -> Result<Vec<u32>, RunError> {
    let path = dir
        .join("constant")
        .join(region)
        .join("polyMesh")
        .join("owner");
    let text = fs::read(&path).map_err(|error| RunError::Io {
        path: path.clone(),
        error,
    })?;
    let mut lex = Lexer::new(&text);
    let owner = read_header(&mut lex)
        .and_then(|_| lex.list(|l| l.label().map(|i| i as u32)))
        .map_err(|e| RunError::Io {
            path: path.clone(),
            error: std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()),
        })?;
    let mut cells: Vec<u32> = patches
        .iter()
        .flat_map(|p| p.start..p.start + p.faces)
        .filter_map(|f| owner.get(f).copied())
        .collect();
    cells.sort_unstable();
    cells.dedup();
    Ok(cells)
}

/// Every region's files once its patches are known.
fn write_regions(
    case: &Case,
    study: &FlowStudy,
    heat: &Heat,
    fluid: &[RegionPatch],
    solid: &[RegionPatch],
) -> Result<(), RunError> {
    let dir = &case.dir;
    let put = |path: &str, text: &str| {
        crate::case::put(&dir.join(path), text).map_err(|e| match e {
            CaseError::Io { path, error } => RunError::Io { path, error },
            CaseError::Study(e) => RunError::Io {
                path: dir.join(path),
                error: std::io::Error::other(e.to_string()),
            },
        })
    };
    let mut regions = header("dictionary", "regionProperties");
    let _ = writeln!(
        regions,
        "regions\n(\n    fluid ({FLUID})\n    solid ({SOLID})\n);"
    );
    put("constant/regionProperties", &regions)?;
    let mut g = header("uniformDimensionedVectorField", "g");
    let _ = writeln!(
        g,
        "dimensions      [0 1 -2 0 0 0 0];\nvalue           {};",
        vector(heat.gravity.unwrap_or([0.0; 3]))
    );
    put("constant/g", &g)?;
    let radiation = {
        let mut s = header("dictionary", "radiationProperties");
        s.push_str("radiation       off;\nradiationModel  none;\n");
        s
    };
    for region in [FLUID, SOLID] {
        put(
            &format!("constant/{region}/radiationProperties"),
            &radiation,
        )?;
    }
    put(
        &format!("constant/{FLUID}/thermophysicalProperties"),
        &fluid_thermo(study, heat),
    )?;
    put(
        &format!("constant/{FLUID}/turbulenceProperties"),
        &turbulence_properties(study),
    )?;
    // `decomposePar -allRegions` reads each region's own.
    for region in [FLUID, SOLID] {
        put(
            &format!("system/{region}/decomposeParDict"),
            &decompose_par_dict(study),
        )?;
    }
    put(&format!("system/{FLUID}/fvSchemes"), &fluid_schemes())?;
    put(
        &format!("system/{FLUID}/fvSolution"),
        &fluid_solution(study),
    )?;

    let (materials, of_body) = materials(heat);
    put(
        &format!("constant/{SOLID}/thermophysicalProperties"),
        &solid_thermo(&materials),
    )?;
    put(&format!("system/{SOLID}/fvSchemes"), &solid_schemes())?;
    put(&format!("system/{SOLID}/fvSolution"), &solid_solution())?;
    if materials.len() > 1 {
        put(
            &format!("system/{SOLID}/setFieldsDict"),
            &set_fields_dict(&materials, &of_body),
        )?;
    }

    // Heat sources: in bodies, or in the solid's cells along faces.
    let mut sources = String::new();
    let volumes = study.volumes();
    let body_of = study.body_of();
    let areas = study.areas();
    for (i, load) in heat.loads.iter().enumerate() {
        match load.kind {
            HeatLoadKind::Internal { watts } => {
                let mut bodies: Vec<usize> = load
                    .labels
                    .iter()
                    .filter_map(|l| body_of.get(l).copied())
                    .collect();
                bodies.sort_unstable();
                bodies.dedup();
                let total: f64 = bodies.iter().filter_map(|&b| volumes.get(b)).sum();
                for &b in &bodies {
                    let share = match volumes.get(b) {
                        Some(v) if total > 0.0 => v / total,
                        _ => 1.0 / bodies.len() as f64,
                    };
                    source(
                        &mut sources,
                        &format!("heat_{i}_{b}"),
                        &format!("selectionMode   cellZone;\ncellZone        {};", body(b)),
                        watts * share,
                    );
                }
            }
            HeatLoadKind::Power { .. } | HeatLoadKind::Flux { .. } => {
                let watts = match load.kind {
                    HeatLoadKind::Flux { flux } => {
                        let area: f64 = load.labels.iter().filter_map(|l| areas.get(l)).sum();
                        flux * area * M_PER_MM * M_PER_MM
                    }
                    HeatLoadKind::Power { watts } => watts,
                    _ => 0.0,
                };
                let on: Vec<&RegionPatch> = solid
                    .iter()
                    .filter(|p| p.label.is_some_and(|l| load.labels.contains(&l)))
                    .collect();
                let cells = cells_next_to(dir, SOLID, &on)?;
                if cells.is_empty() {
                    return Err(RunError::LoadOffMesh { load: i });
                }
                let set = format!("faces_{i}");
                let mut text = header("cellSet", &set);
                let _ = writeln!(text, "{}\n(", cells.len());
                for c in cells {
                    let _ = writeln!(text, "{c}");
                }
                text.push_str(")\n");
                put(&format!("constant/{SOLID}/polyMesh/sets/{set}"), &text)?;
                source(
                    &mut sources,
                    &format!("heat_{i}"),
                    &format!("selectionMode   cellSet;\ncellSet         {set};"),
                    watts,
                );
            }
            HeatLoadKind::Temperature { .. } | HeatLoadKind::Convection { .. } => {}
        }
    }
    let mut options = header("dictionary", "fvOptions");
    options.push_str(&sources);
    put(&format!("system/{SOLID}/fvOptions"), &options)?;

    // With gravity the fluid starts at rest under its weight: `chtMultiRegionSimpleFoam`
    // makes `p_rgh = p − ρ g·x` from `p`, so a uniform `p` would leave the weight of the
    // whole fluid unbalanced on the first iteration.
    let hydrostatic = match heat.gravity {
        Some(g) => Some(hydrostatic_pressure(dir, study.fluid.density, g)?),
        None => None,
    };
    for (name, text) in fluid_fields(case, study, heat, fluid, hydrostatic.as_ref()) {
        put(&format!("0/{FLUID}/{name}"), &text)?;
    }
    for (name, text) in solid_fields(heat, solid, &materials) {
        put(&format!("0/{SOLID}/{name}"), &text)?;
    }
    Ok(())
}

/// The pressure of a fluid of `density` at rest under gravity `g`, Pa: [`P_REF`] at the
/// origin, in each cell of the fluid's mesh and on each of its boundary faces (from the
/// first).
fn hydrostatic_pressure(
    dir: &Path,
    density: f64,
    g: [f64; 3],
) -> Result<(Vec<f64>, Vec<f64>), RunError> {
    let mesh = crate::mesh::PolyMesh::read_region(dir, FLUID).map_err(|e| RunError::Io {
        path: dir.join("constant").join(FLUID).join("polyMesh"),
        error: std::io::Error::other(e.to_string()),
    })?;
    let at = |x: [f64; 3]| P_REF + density * crate::study::dot(g, x) * M_PER_MM;
    let (centres, _) = mesh.cell_geometry();
    let cells = centres.into_iter().map(at).collect();
    let faces = (mesh.internal_faces()..mesh.faces())
        .map(|f| at(mesh.face_geometry(f).0))
        .collect();
    Ok((cells, faces))
}

/// A heat source of `watts` in the cells `selection` picks.
fn source(s: &mut String, name: &str, selection: &str, watts: f64) {
    let _ = writeln!(
        s,
        "{name}\n{{\n    type            scalarSemiImplicitSource;"
    );
    for line in selection.lines() {
        let _ = writeln!(s, "    {line}");
    }
    let _ = writeln!(
        s,
        "    volumeMode      absolute;\n    sources\n    {{\n        h\n        {{\n            explicit    {};\n            implicit    0;\n        }}\n    }}\n}}\n",
        num(watts)
    );
}

fn fluid_thermo(study: &FlowStudy, heat: &Heat) -> String {
    let fluid = &study.fluid;
    let buoyant = heat.gravity.is_some() && heat.expansion > 0.0;
    let mut s = header("dictionary", "thermophysicalProperties");
    let _ = writeln!(
        s,
        "thermoType\n{{\n    type            heRhoThermo;\n    mixture         pureMixture;\n    transport       const;\n    thermo          hConst;\n    equationOfState {};\n    specie          specie;\n    energy          sensibleEnthalpy;\n}}\n",
        if buoyant { "Boussinesq" } else { "rhoConst" }
    );
    s.push_str("mixture\n{\n    specie\n    {\n        molWeight   28.96;\n    }\n");
    if buoyant {
        let _ = writeln!(
            s,
            "    equationOfState\n    {{\n        rho0        {};\n        T0          {};\n        beta        {};\n    }}",
            num(fluid.density),
            num(heat.temperature + KELVIN),
            num(heat.expansion)
        );
    } else {
        let _ = writeln!(
            s,
            "    equationOfState\n    {{\n        rho         {};\n    }}",
            num(fluid.density)
        );
    }
    // Prandtl's number from the conductivity.
    let prandtl = heat.specific_heat * fluid.viscosity / heat.conductivity;
    let _ = writeln!(
        s,
        "    thermodynamics\n    {{\n        Cp          {};\n        Hf          0;\n    }}\n    transport\n    {{\n        mu          {};\n        Pr          {};\n    }}\n}}",
        num(heat.specific_heat),
        num(fluid.viscosity),
        num(prandtl)
    );
    s
}

fn solid_thermo(materials: &[(f64, f64)]) -> String {
    let mut s = header("dictionary", "thermophysicalProperties");
    let mixture = if materials.len() > 1 {
        "multiComponentMixture"
    } else {
        "pureMixture"
    };
    let _ = writeln!(
        s,
        "thermoType\n{{\n    type            heSolidThermo;\n    mixture         {mixture};\n    transport       constIso;\n    thermo          hConst;\n    equationOfState rhoConst;\n    specie          specie;\n    energy          sensibleEnthalpy;\n}}\n"
    );
    let block = |name: &str, (kappa, rho): (f64, f64)| {
        format!(
            "{name}\n{{\n    specie\n    {{\n        molWeight   50;\n    }}\n    transport\n    {{\n        kappa       {};\n    }}\n    thermodynamics\n    {{\n        Hf          0;\n        Cp          {};\n    }}\n    equationOfState\n    {{\n        rho         {};\n    }}\n}}\n",
            num(kappa),
            num(SOLID_CP),
            num(rho)
        )
    };
    if materials.len() > 1 {
        let names: Vec<String> = (0..materials.len()).map(species).collect();
        let _ = writeln!(s, "species ({});\n", names.join(" "));
        let _ = writeln!(s, "inertSpecie {};\n", species(0));
        for (m, &material) in materials.iter().enumerate() {
            s.push_str(&block(&species(m), material));
            s.push('\n');
        }
    } else {
        s.push_str(&block(
            "mixture",
            materials.first().copied().unwrap_or((1.0, 1.0)),
        ));
    }
    s
}

fn set_fields_dict(materials: &[(f64, f64)], of_body: &[usize]) -> String {
    let n = materials.len();
    let mut s = header("dictionary", "setFieldsDict");
    s.push_str("defaultFieldValues\n(\n");
    for m in 0..n {
        let _ = writeln!(s, "    volScalarFieldValue {} {}", species(m), num(FLOOR));
    }
    s.push_str(");\n\nregions\n(\n");
    let full = 1.0 - (n as f64 - 1.0) * FLOOR;
    for m in 0..n {
        let zones: Vec<String> = of_body
            .iter()
            .enumerate()
            .filter(|&(_, &b)| b == m)
            .map(|(k, _)| body(k))
            .collect();
        if zones.is_empty() {
            continue;
        }
        let _ = writeln!(
            s,
            "    zoneToCell\n    {{\n        zones ({});\n        fieldValues ( volScalarFieldValue {} {} );\n    }}",
            zones.join(" "),
            species(m),
            num(full)
        );
    }
    s.push_str(");\n");
    s
}

fn fluid_schemes() -> String {
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
    grad(h)         cellLimited Gauss linear 1;
    grad(k)         cellLimited Gauss linear 1;
    grad(omega)     cellLimited Gauss linear 1;
}

divSchemes
{
    default         none;
    div(phi,U)      bounded Gauss linearUpwindV grad(U);
    div(phi,h)      bounded Gauss linearUpwind grad(h);
    div(phi,K)      bounded Gauss upwind;
    div(phi,k)      bounded Gauss upwind;
    div(phi,omega)  bounded Gauss upwind;
    div(((rho*nuEff)*dev2(T(grad(U))))) Gauss linear;
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

fn fluid_solution(study: &FlowStudy) -> String {
    let rho = study.fluid.density;
    let mut s = header("dictionary", "fvSolution");
    s.push_str(
        "solvers
{
    \"rho.*\"
    {
        solver          PCG;
        preconditioner  DIC;
        tolerance       0;
        relTol          0;
    }

    p_rgh
    {
        solver          GAMG;
        smoother        GaussSeidel;
        tolerance       1e-8;
        relTol          0.01;
        // Near convergence the solve can stall far short of relTol: what it reaches in a
        // hundred cycles is enough for the next iteration.
        maxIter         100;
    }

    \"(U|k|omega)\"
    {
        solver          PBiCGStab;
        preconditioner  DILU;
        tolerance       1e-9;
        relTol          0.1;
    }

    h
    {
        solver          PBiCGStab;
        preconditioner  DILU;
        tolerance       1e-9;
        relTol          0.01;
    }
}

SIMPLE
{
    momentumPredictor yes;
    nNonOrthogonalCorrectors 1;
    pRefCell        0;
",
    );
    let _ = writeln!(s, "    pRefValue       {};", num(P_REF));
    // The density stays near its reference: a bound against a stray iteration.
    let _ = writeln!(
        s,
        "    rhoMin          {};\n    rhoMax          {};\n}}",
        num(rho * 0.5),
        num(rho * 2.0)
    );
    s.push_str(
        "
relaxationFactors
{
    fields
    {
        rho             1;
        p_rgh           0.3;
    }
    equations
    {
        U               0.7;
        h               1;
        \"(k|omega)\"     0.7;
    }
}
",
    );
    s
}

fn solid_schemes() -> String {
    let mut s = header("dictionary", "fvSchemes");
    s.push_str(
        "ddtSchemes
{
    default         steadyState;
}

gradSchemes
{
    default         Gauss linear;
}

divSchemes
{
    default         none;
}

laplacianSchemes
{
    default         Gauss linear limited corrected 0.5;
    // Conductivities meet harmonically where one material touches another.
    laplacian(alpha,h) Gauss harmonic limited corrected 0.5;
}

interpolationSchemes
{
    default         linear;
}

snGradSchemes
{
    default         limited corrected 0.5;
}
",
    );
    s
}

fn solid_solution() -> String {
    let mut s = header("dictionary", "fvSolution");
    s.push_str(
        "solvers
{
    h
    {
        solver          PCG;
        preconditioner  DIC;
        tolerance       1e-9;
        relTol          0.01;
    }

    \"rho.*\"
    {
        solver          PCG;
        preconditioner  DIC;
        tolerance       0;
        relTol          0;
    }
}

SIMPLE
{
    nNonOrthogonalCorrectors 1;
}

relaxationFactors
{
    equations
    {
        h               1;
    }
}
",
    );
    s
}

/// A list of values as a field holds them: `nonuniform List<scalar> N(…)`.
fn values(list: &[f64]) -> String {
    let mut s = format!("nonuniform List<scalar> {}\n(\n", list.len());
    for v in list {
        let _ = writeln!(s, "{}", num(*v));
    }
    s.push(')');
    s
}

/// A field file's beginning: header, dimensions, uniform internal value.
fn field_start(class: &str, name: &str, dimensions: &str, internal: &str) -> String {
    let mut s = header(class, name);
    let _ = writeln!(
        s,
        "dimensions      {dimensions};\n\ninternalField   uniform {internal};\n\nboundaryField\n{{"
    );
    s
}

/// The temperature a face load holds `label` at, °C; or `None`.
fn held_temperature(heat: &Heat, label: u32) -> Option<f64> {
    heat.loads.iter().find_map(|l| match l.kind {
        HeatLoadKind::Temperature { celsius } if l.labels.contains(&label) => Some(celsius),
        _ => None,
    })
}

/// The convection a face load gives `label`: coefficient, ambient °C.
fn convection(heat: &Heat, label: u32) -> Option<(f64, f64)> {
    heat.loads.iter().find_map(|l| match l.kind {
        HeatLoadKind::Convection {
            coefficient,
            ambient,
        } if l.labels.contains(&label) => Some((coefficient, ambient)),
        _ => None,
    })
}

fn coupled(method: &str, t0: &str) -> String {
    format!(
        "type            compressible::turbulentTemperatureRadCoupledMixed;\nTnbr            T;\nkappaMethod     {method};\nvalue           uniform {t0};"
    )
}

/// The fluid's starting fields.
fn fluid_fields(
    case: &Case,
    study: &FlowStudy,
    heat: &Heat,
    patches: &[RegionPatch],
    hydrostatic: Option<&(Vec<f64>, Vec<f64>)>,
) -> Vec<(&'static str, String)> {
    let rho = study.fluid.density;
    let t_in = heat.temperature + KELVIN;
    let t0 = num(t_in);
    let p0 = num(P_REF);
    let external = match study.flow_type {
        FlowType::External { velocity } => Some(velocity),
        FlowType::Internal { .. } => None,
    };
    let stream = external.filter(|v| norm(*v) > 0.0);
    let condition_of = study.condition_of();
    let condition = |label: u32| {
        condition_of
            .get(&label)
            .and_then(|&c| study.conditions.get(c))
    };
    let face_patch = |label: u32| case.patch_of(label);

    let mut u = field_start(
        "volVectorField",
        "U",
        "[0 1 -1 0 0 0 0]",
        &vector(external.unwrap_or([0.0; 3])),
    );
    let mut p_rgh = field_start("volScalarField", "p_rgh", "[1 -1 -2 0 0 0 0]", &p0);
    let mut p = match hydrostatic {
        Some((cells, _)) => {
            let mut s = header("volScalarField", "p");
            let _ = writeln!(
                s,
                "dimensions      [1 -1 -2 0 0 0 0];\n\ninternalField   {};\n\nboundaryField\n{{",
                values(cells)
            );
            s
        }
        None => field_start("volScalarField", "p", "[1 -1 -2 0 0 0 0]", &p0),
    };
    let mut t = field_start("volScalarField", "T", "[0 0 0 1 0 0 0]", &t0);
    let first_boundary = patches.iter().map(|q| q.start).min().unwrap_or(0);
    // `p` on a patch: the reference, or the fluid at rest's.
    let calculated = |patch: &RegionPatch| match hydrostatic {
        Some((_, faces)) => {
            let from = patch.start.saturating_sub(first_boundary);
            let on = faces.get(from..from + patch.faces).unwrap_or(&[]);
            format!(
                "type            calculated;\nvalue           {};",
                values(on)
            )
        }
        None => format!("type            calculated;\nvalue           uniform {p0};"),
    };
    let flux_pressure =
        format!("type            fixedFluxPressure;\nvalue           uniform {p0};");
    let open_t = format!(
        "type            inletOutlet;\ninletValue      uniform {t0};\nvalue           uniform {t0};"
    );
    for patch in patches {
        let name = patch.name.as_str();
        if name == DOMAIN_PATCH || patch.label.is_none() {
            match stream {
                Some(v) => {
                    entry(
                        &mut u,
                        name,
                        &format!(
                            "type            freestreamVelocity;\nfreestreamValue uniform {0};\nvalue           uniform {0};",
                            vector(v)
                        ),
                    );
                    entry(
                        &mut p_rgh,
                        name,
                        &format!(
                            "type            freestreamPressure;\nfreestreamValue uniform {p0};\nvalue           uniform {p0};"
                        ),
                    );
                    entry(&mut t, name, &open_t);
                }
                None if external.is_some() => {
                    // Still surroundings: the fluid comes and goes as buoyancy drives it, at
                    // the pressure of the fluid at rest (a uniform `p_rgh`).
                    entry(
                        &mut u,
                        name,
                        "type            pressureInletOutletVelocity;\nvalue           uniform (0 0 0);",
                    );
                    entry(
                        &mut p_rgh,
                        name,
                        &format!(
                            "type            totalPressure;\np0              uniform {p0};\nvalue           uniform {p0};"
                        ),
                    );
                    entry(&mut t, name, &open_t);
                }
                None => {
                    entry(&mut u, name, "type            noSlip;");
                    entry(&mut p_rgh, name, &flux_pressure);
                    entry(&mut t, name, "type            zeroGradient;");
                }
            }
            entry(&mut p, name, &calculated(patch));
            continue;
        }
        let label = patch.label.unwrap_or_default();
        let kind = condition(label).map(|c| c.kind);
        let (u_bc, p_bc, t_bc) = match kind {
            None => (
                "type            noSlip;".to_string(),
                flux_pressure.clone(),
                match held_temperature(heat, label) {
                    Some(c) => format!(
                        "type            fixedValue;\nvalue           uniform {};",
                        num(c + KELVIN)
                    ),
                    None => coupled("fluidThermo", &t0),
                },
            ),
            Some(ConditionKind::InletVelocity { speed }) => (
                format!(
                    "type            surfaceNormalFixedValue;\nrefValue        uniform {};",
                    num(-speed)
                ),
                flux_pressure.clone(),
                format!("type            fixedValue;\nvalue           uniform {t0};"),
            ),
            Some(ConditionKind::InletVolumeFlow { rate }) => (
                match (condition(label), face_patch(label)) {
                    (Some(c), Some(fp)) => flow_rate_inlet(rate * area_share(case, &c.labels, fp)),
                    _ => flow_rate_inlet(0.0),
                },
                flux_pressure.clone(),
                format!("type            fixedValue;\nvalue           uniform {t0};"),
            ),
            Some(ConditionKind::InletMassFlow { rate }) => (
                match (condition(label), face_patch(label)) {
                    (Some(c), Some(fp)) => {
                        flow_rate_inlet(rate / rho * area_share(case, &c.labels, fp))
                    }
                    _ => flow_rate_inlet(0.0),
                },
                flux_pressure.clone(),
                format!("type            fixedValue;\nvalue           uniform {t0};"),
            ),
            Some(ConditionKind::OutletPressure { pressure }) => (
                "type            pressureInletOutletVelocity;\nvalue           uniform (0 0 0);"
                    .to_string(),
                // Above the surroundings' pressure, which with gravity is the fluid at rest's:
                // a uniform `p_rgh`.
                format!(
                    "type            fixedValue;\nvalue           uniform {};",
                    num(P_REF + pressure)
                ),
                open_t.clone(),
            ),
            Some(ConditionKind::IdealWall) => (
                "type            slip;".to_string(),
                flux_pressure.clone(),
                "type            zeroGradient;".to_string(),
            ),
        };
        entry(&mut u, name, &u_bc);
        entry(&mut p_rgh, name, &p_bc);
        entry(&mut p, name, &calculated(patch));
        entry(&mut t, name, &t_bc);
    }
    for f in [&mut u, &mut p_rgh, &mut p, &mut t] {
        f.push_str("}\n");
    }
    let mut out = vec![("U", u), ("p_rgh", p_rgh), ("p", p), ("T", t)];

    if let Turbulence::KOmegaSst { intensity, length } = study.turbulence {
        let speed = reference_speed(study, case);
        let k0 = (1.5 * (intensity * speed).powi(2)).max(1e-10);
        let mixing = length * M_PER_MM;
        let w0 = (k0.sqrt() / (0.09f64.powf(0.25) * mixing)).max(1e-6);
        let (k0s, w0s) = (num(k0), num(w0));
        let mut k = field_start("volScalarField", "k", "[0 2 -2 0 0 0 0]", &k0s);
        let mut omega = field_start("volScalarField", "omega", "[0 0 -1 0 0 0 0]", &w0s);
        let mut nut = field_start("volScalarField", "nut", "[0 2 -1 0 0 0 0]", "0");
        let mut alphat = field_start("volScalarField", "alphat", "[1 -1 -1 0 0 0 0]", "0");
        let k_wall = format!("type            kqRWallFunction;\nvalue           uniform {k0s};");
        let w_wall = format!("type            omegaWallFunction;\nvalue           uniform {w0s};");
        let nut_wall = "type            nutUSpaldingWallFunction;\nvalue           uniform 0;";
        let alphat_wall = "type            compressible::alphatWallFunction;\nPrt             0.85;\nvalue           uniform 0;";
        let k_open = format!(
            "type            inletOutlet;\ninletValue      uniform {k0s};\nvalue           uniform {k0s};"
        );
        let w_open = format!(
            "type            inletOutlet;\ninletValue      uniform {w0s};\nvalue           uniform {w0s};"
        );
        let calculated = "type            calculated;\nvalue           uniform 0;";
        let zero_gradient = "type            zeroGradient;";
        for patch in patches {
            let name = patch.name.as_str();
            let role = patch.label.map(|l| match condition(l).map(|c| c.kind) {
                None => Role::Wall,
                Some(
                    ConditionKind::InletVelocity { .. }
                    | ConditionKind::InletVolumeFlow { .. }
                    | ConditionKind::InletMassFlow { .. },
                ) => Role::Inlet,
                Some(ConditionKind::OutletPressure { .. }) => Role::Outlet,
                Some(ConditionKind::IdealWall) => Role::Slip,
            });
            match role {
                None if external.is_some() => {
                    entry(&mut k, name, &k_open);
                    entry(&mut omega, name, &w_open);
                    entry(&mut nut, name, calculated);
                    entry(&mut alphat, name, calculated);
                }
                None | Some(Role::Wall) => {
                    entry(&mut k, name, &k_wall);
                    entry(&mut omega, name, &w_wall);
                    entry(&mut nut, name, nut_wall);
                    entry(&mut alphat, name, alphat_wall);
                }
                Some(Role::Inlet) => {
                    entry(
                        &mut k,
                        name,
                        &format!(
                            "type            turbulentIntensityKineticEnergyInlet;\nintensity       {};\nvalue           uniform {k0s};",
                            num(intensity)
                        ),
                    );
                    entry(
                        &mut omega,
                        name,
                        &format!(
                            "type            turbulentMixingLengthFrequencyInlet;\nmixingLength    {};\nvalue           uniform {w0s};",
                            num(mixing)
                        ),
                    );
                    entry(&mut nut, name, calculated);
                    entry(&mut alphat, name, calculated);
                }
                Some(Role::Outlet) => {
                    entry(&mut k, name, &k_open);
                    entry(&mut omega, name, &w_open);
                    entry(&mut nut, name, calculated);
                    entry(&mut alphat, name, calculated);
                }
                Some(Role::Slip) => {
                    entry(&mut k, name, zero_gradient);
                    entry(&mut omega, name, zero_gradient);
                    entry(&mut nut, name, calculated);
                    entry(&mut alphat, name, calculated);
                }
            }
        }
        for f in [&mut k, &mut omega, &mut nut, &mut alphat] {
            f.push_str("}\n");
        }
        out.push(("k", k));
        out.push(("omega", omega));
        out.push(("nut", nut));
        out.push(("alphat", alphat));
    }
    out
}

/// The solid's starting fields: its temperature, the pressure its thermophysics reads, and
/// each material's mass fraction (which `setFields` fills from the bodies' zones).
fn solid_fields(
    heat: &Heat,
    patches: &[RegionPatch],
    materials: &[(f64, f64)],
) -> Vec<(String, String)> {
    let t0 = num(heat.temperature + KELVIN);
    let p0 = num(P_REF);
    let mut t = field_start("volScalarField", "T", "[0 0 0 1 0 0 0]", &t0);
    let mut p = field_start("volScalarField", "p", "[1 -1 -2 0 0 0 0]", &p0);
    for patch in patches {
        let name = patch.name.as_str();
        let held = patch.label.and_then(|l| held_temperature(heat, l));
        let bc = match (held, patch.kind.as_str(), patch.label) {
            (Some(c), _, _) => format!(
                "type            fixedValue;\nvalue           uniform {};",
                num(c + KELVIN)
            ),
            (None, "mappedWall", _) => coupled("solidThermo", &t0),
            (None, _, Some(label)) if name.starts_with("outer_") => match convection(heat, label) {
                Some((h, ambient)) => format!(
                    "type            externalWallHeatFluxTemperature;\nmode            coefficient;\nh               uniform {};\nTa              constant {};\nkappaMethod     solidThermo;\nvalue           uniform {t0};",
                    num(h),
                    num(ambient + KELVIN)
                ),
                None => "type            zeroGradient;".to_string(),
            },
            _ => "type            zeroGradient;".to_string(),
        };
        entry(&mut t, name, &bc);
        entry(
            &mut p,
            name,
            &format!("type            calculated;\nvalue           uniform {p0};"),
        );
    }
    t.push_str("}\n");
    p.push_str("}\n");
    let mut out = vec![("T".to_string(), t), ("p".to_string(), p)];
    if materials.len() > 1 {
        for m in 0..materials.len() {
            let name = species(m);
            let mut y = field_start("volScalarField", &name, "[0 0 0 0 0 0 0]", &num(FLOOR));
            entry(&mut y, "\".*\"", "type            zeroGradient;");
            y.push_str("}\n");
            out.push((name, y));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::write_case;
    use crate::study::{
        Condition, Domain, Fluid, Goal, GoalKind, HeatLoad, MeshSettings, Solid, SolverSettings,
        cube,
    };

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("oxyde-cooling-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// A 10 mm cube of aluminium with a 5 mm cube of ceramic on its +y side making 2 W, in a
    /// stream of air along x.
    pub(crate) fn study() -> FlowStudy {
        let base = cube(10.0, [0.0; 3]);
        let chip = cube(5.0, [0.0, 7.5, 0.0]);
        let mut chip = chip;
        for l in &mut chip.labels {
            *l += 6;
        }
        let surfaces = vec![base, chip];
        let mut study = FlowStudy {
            surfaces,
            flow_type: FlowType::External {
                velocity: [1.0, 0.0, 0.0],
            },
            domain: Domain::internal(([0.0; 3], [1.0; 3]), 1.0),
            fluid: Fluid::air(),
            turbulence: Turbulence::Laminar,
            conditions: vec![],
            goals: vec![
                Goal {
                    name: "Chip".to_string(),
                    kind: GoalKind::MaxTemperature,
                    labels: vec![9],
                },
                Goal {
                    name: "Drag".to_string(),
                    kind: GoalKind::Force,
                    labels: vec![0, 1],
                },
            ],
            mesh: MeshSettings::new(5.0),
            solver: SolverSettings::default(),
            heat: Some(Heat {
                solids: vec![
                    Solid {
                        name: "Base".to_string(),
                        conductivity: 200.0,
                        density: 2700.0,
                    },
                    Solid {
                        name: "Chip".to_string(),
                        conductivity: 20.0,
                        density: 3000.0,
                    },
                ],
                loads: vec![HeatLoad {
                    kind: HeatLoadKind::Internal { watts: 2.0 },
                    labels: vec![7],
                }],
                ..Heat::air()
            }),
        };
        study.domain = Domain::external(study.bounds().unwrap(), [1.0, 0.0, 0.0]);
        study
    }

    fn read(dir: &Path, file: &str) -> String {
        fs::read_to_string(dir.join(file)).unwrap()
    }

    #[test]
    fn a_cooling_case_meshes_each_body_as_a_zone() {
        let dir = scratch("mesh");
        let case = write_case(&dir, &study()).unwrap();
        assert_eq!(case.solver, "chtMultiRegionSimpleFoam");
        assert!(case.cooling.is_some());
        let stl = read(&dir, "constant/triSurface/body_1.stl");
        assert_eq!(stl.matches("endsolid").count(), 6);
        assert!(stl.contains("solid wall_9\n"));
        assert!(!dir.join("constant/triSurface/model.stl").exists());
        let snappy = read(&dir, "system/snappyHexMeshDict");
        assert!(snappy.contains("cellZone body_0;"));
        assert!(snappy.contains("faceZoneNaming region;"));
        assert!(snappy.contains("{ file \"body_1.eMesh\"; level 1; }"));
        let topo = read(&dir, "system/topoSetDict");
        assert!(topo.contains("zones (body_0 body_1);"));
        assert!(topo.contains("source regionToCell; set unzoned;"));
        assert_eq!(
            split_arguments(&case),
            [
                "-cellZonesOnly",
                "-useFaceZones",
                "-addZones",
                "((solid body_0 body_1))",
                "-overwrite"
            ]
        );
        assert!(has_mixture(&case));
        let solve = read(&dir, "system/controlDict.solve");
        assert!(solve.contains("runTimeModifiable true;"));
        assert!(solve.contains("region          fluid;"));
        assert!(solve.contains("patches         (wall_0 wall_1);"));
        assert!(solve.contains("pRef            100000;"));
        assert!(read(&dir, "system/controlDict").contains("runTimeModifiable false;"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn zone_patches_name_their_face_and_neighbour() {
        assert_eq!(
            zone_patch("body_0_wall_12_fluid_to_solid"),
            Some((0, Some(12), "solid".to_string()))
        );
        assert_eq!(
            zone_patch("body_3_wall_7_solid_to_outside"),
            Some((3, Some(7), "outside".to_string()))
        );
        // A body of one face: its zone is named after the body.
        assert_eq!(
            zone_patch("body_11_fluid_to_solid"),
            Some((11, None, "solid".to_string()))
        );
        assert_eq!(zone_patch("domain"), None);
        let roles = BTreeMap::from([(3, Role::Inlet), (4, Role::Outlet)]);
        let single = BTreeMap::from([(11, 40)]);
        let r = |region, name| renamed(region, name, "mappedWall", &roles, &single);
        assert_eq!(r(FLUID, "body_11_fluid_to_solid").0, "wall_40");
        assert_eq!(r(SOLID, "body_11_solid_to_fluid").3.unwrap().1, "wall_40");
        // Unknown and coupled: a plain wall, rather than a coupling to nowhere.
        assert_eq!(
            r(SOLID, "body_2_solid_to_fluid"),
            (
                "body_2_solid_to_fluid".to_string(),
                "wall".to_string(),
                None,
                None
            )
        );
        assert_eq!(
            r(FLUID, "body_0_wall_12_fluid_to_solid"),
            (
                "wall_12".to_string(),
                "mappedWall".to_string(),
                Some(12),
                Some(("solid".to_string(), "wall_12".to_string()))
            )
        );
        assert_eq!(r(FLUID, "body_1_wall_3_fluid_to_solid").0, "inlet_3");
        assert_eq!(r(FLUID, "body_1_wall_3_fluid_to_solid").1, "patch");
        assert_eq!(r(SOLID, "body_1_wall_3_solid_to_fluid").0, "free_3");
        assert_eq!(r(SOLID, "body_1_wall_4_solid_to_fluid").1, "wall");
        assert_eq!(r(SOLID, "body_0_wall_5_solid_to_outside").0, "outer_5");
        assert_eq!(r(FLUID, "domain").0, "domain");
    }

    /// A split mesh's boundary files, as `splitMeshRegions` writes them.
    fn write_split(dir: &Path, fluid: &[(&str, &str, usize)], solid: &[(&str, &str, usize)]) {
        for (region, patches) in [(FLUID, fluid), (SOLID, solid)] {
            let mesh = dir.join("constant").join(region).join("polyMesh");
            fs::create_dir_all(&mesh).unwrap();
            let mut s = String::from(
                "FoamFile { version 2.0; format ascii; class polyBoundaryMesh; object boundary; }\n",
            );
            let _ = writeln!(s, "{}\n(", patches.len());
            let mut start = 10;
            for (name, kind, n) in patches {
                let _ = writeln!(
                    s,
                    "    {name}\n    {{\n        type {kind};\n        inGroups 1(wall);\n        nFaces {n};\n        startFace {start};\n        sampleMode nearestPatchFace;\n    }}"
                );
                start += n;
            }
            s.push_str(")\n");
            fs::write(mesh.join("boundary"), s).unwrap();
            // Faces 10.. owned by cells 0, 1, 2…
            let owner: Vec<String> = (0..start).map(|f| (f % 7).to_string()).collect();
            fs::write(
                mesh.join("owner"),
                format!(
                    "FoamFile {{ format ascii; class labelList; object owner; }}\n{}\n({})\n",
                    owner.len(),
                    owner.join(" ")
                ),
            )
            .unwrap();
        }
    }

    /// The two unit cubes of `mesh::tests` as the fluid's mesh, their first boundary face on
    /// face 0 of the body and the other nine on face 1, as `splitMeshRegions` names them.
    fn cubes_as_fluid(dir: &Path) {
        let scratch = dir.join("cubes");
        crate::mesh::tests::write_two_cubes(&scratch, false);
        let from = scratch.join("constant/polyMesh");
        let to = dir.join("constant").join(FLUID).join("polyMesh");
        for file in ["points", "faces", "owner", "neighbour"] {
            fs::copy(from.join(file), to.join(file)).unwrap();
        }
        fs::write(
            to.join("boundary"),
            "FoamFile { format ascii; class polyBoundaryMesh; object boundary; }
3
(
    domain { type wall; nFaces 0; startFace 1; }
    body_0_wall_0_fluid_to_solid { type mappedWall; nFaces 1; startFace 1; }
    body_0_wall_1_fluid_to_solid { type mappedWall; nFaces 9; startFace 2; }
)
",
        )
        .unwrap();
        let _ = fs::remove_dir_all(&scratch);
    }

    #[test]
    fn finishing_renames_patches_and_writes_the_regions() {
        let dir = scratch("finish");
        let mut s = study();
        s.heat.as_mut().unwrap().loads.push(HeatLoad {
            kind: HeatLoadKind::Flux { flux: 1000.0 },
            labels: vec![2],
        });
        let case = write_case(&dir, &s).unwrap();
        write_split(
            &dir,
            &[
                ("domain", "patch", 40),
                ("body_0_wall_2_fluid_to_solid", "mappedWall", 4),
                ("body_1_wall_9_fluid_to_solid", "mappedWall", 3),
            ],
            &[
                ("body_0_wall_2_solid_to_fluid", "mappedWall", 4),
                ("body_1_wall_9_solid_to_fluid", "mappedWall", 3),
            ],
        );
        finish(&case).unwrap();
        let boundary = read(&dir, "constant/fluid/polyMesh/boundary");
        assert!(boundary.contains("    wall_2\n    {\n        type            mappedWall;"));
        assert!(boundary.contains("samplePatch     wall_9;"));
        assert!(boundary.contains("sampleRegion    solid;"));
        assert!(boundary.contains("    domain\n"));
        let solid = read(&dir, "constant/solid/polyMesh/boundary");
        assert!(solid.contains("sampleRegion    fluid;"));

        let regions = read(&dir, "constant/regionProperties");
        assert!(regions.contains("fluid (fluid)") && regions.contains("solid (solid)"));
        let thermo = read(&dir, "constant/solid/thermophysicalProperties");
        assert!(thermo.contains("multiComponentMixture"));
        assert!(thermo.contains("species (material_0 material_1);"));
        assert!(thermo.contains("kappa       20;"));
        let set = read(&dir, "system/solid/setFieldsDict");
        assert!(set.contains("zones (body_1);"));
        assert!(set.contains("volScalarFieldValue material_0 1e-8"));
        let options = read(&dir, "system/solid/fvOptions");
        assert!(options.contains("cellZone        body_1;"));
        assert!(options.contains("explicit    2;"));
        // 1000 W/m² over the 1 cm² side: 0.1 W in the cells next to it.
        assert!(options.contains("cellSet         faces_1;"));
        assert!(options.contains("explicit    0.1;"));
        assert!(dir.join("constant/solid/polyMesh/sets/faces_1").exists());
        let t = read(&dir, "0/fluid/T");
        assert!(t.contains("turbulentTemperatureRadCoupledMixed"));
        assert!(t.contains("kappaMethod     fluidThermo;"));
        assert!(t.contains("inletOutlet"));
        let u = read(&dir, "0/fluid/U");
        assert!(u.contains("freestreamVelocity"));
        let p = read(&dir, "0/fluid/p_rgh");
        assert!(p.contains("freestreamPressure"));
        let st = read(&dir, "0/solid/T");
        assert!(st.contains("kappaMethod     solidThermo;"));
        assert!(dir.join("0/solid/material_1").exists());
        let fluid = read(&dir, "constant/fluid/thermophysicalProperties");
        assert!(fluid.contains("rhoConst"), "no gravity: constant density");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_leaking_internal_flow_and_openings_on_the_solid() {
        let dir = scratch("internal");
        let mut s = study();
        s.flow_type = FlowType::Internal {
            point: [0.0, 0.0, 0.0],
        };
        s.conditions = vec![
            Condition {
                kind: ConditionKind::InletVelocity { speed: 0.5 },
                labels: vec![0],
            },
            Condition {
                kind: ConditionKind::OutletPressure { pressure: 10.0 },
                labels: vec![1],
            },
        ];
        s.domain = Domain::internal(s.bounds().unwrap(), 10.0);
        if let Some(h) = s.heat.as_mut() {
            h.gravity = Some([0.0, -9.81, 0.0]);
            h.loads.push(HeatLoad {
                kind: HeatLoadKind::Convection {
                    coefficient: 10.0,
                    ambient: 30.0,
                },
                labels: vec![5],
            });
        }
        let case = write_case(&dir, &s).unwrap();
        write_split(
            &dir,
            &[
                ("domain", "wall", 0),
                ("body_0_wall_0_fluid_to_solid", "mappedWall", 4),
                ("body_0_wall_1_fluid_to_solid", "mappedWall", 4),
            ],
            &[
                ("body_0_wall_0_solid_to_fluid", "mappedWall", 4),
                ("body_0_wall_1_solid_to_fluid", "mappedWall", 4),
                ("body_0_wall_5_solid_to_outside", "mappedWall", 4),
            ],
        );
        cubes_as_fluid(&dir);
        finish(&case).unwrap();
        let boundary = read(&dir, "constant/fluid/polyMesh/boundary");
        assert!(boundary.contains("    inlet_0\n    {\n        type            patch;"));
        assert!(boundary.contains("    outlet_1\n"));
        let solid = read(&dir, "constant/solid/polyMesh/boundary");
        assert!(solid.contains("    free_0\n    {\n        type            wall;"));
        assert!(solid.contains("    outer_5\n"));
        let p = read(&dir, "0/fluid/p_rgh");
        assert!(p.contains("    outlet_1\n    {\n        type            fixedValue;\n        value           uniform 100010;"));
        // With gravity, `p` starts as the fluid at rest's: the cubes' centres are 0.5 m up,
        // 1.204 kg/m³ × 9.81 m/s² below 100 000 Pa.
        let p = read(&dir, "0/fluid/p");
        assert!(
            p.contains("internalField   nonuniform List<scalar> 2"),
            "{p}"
        );
        let field = crate::foam::scalar_field(p.as_bytes()).unwrap();
        let cells = field.internal.expand(2).unwrap();
        let expected = P_REF - 1.204 * 9.81 * 0.5;
        assert!(
            cells.iter().all(|c| (c - expected).abs() < 1e-6),
            "{cells:?}"
        );
        assert!(
            read(&dir, "0/fluid/p_rgh").contains("domain"),
            "every patch has an entry"
        );
        let st = read(&dir, "0/solid/T");
        assert!(st.contains("externalWallHeatFluxTemperature"));
        assert!(st.contains("Ta              constant 303.15;"));
        let fluid = read(&dir, "constant/fluid/thermophysicalProperties");
        assert!(fluid.contains("Boussinesq"));
        assert!(fluid.contains("T0          293.15;"));
        assert!(read(&dir, "constant/g").contains("(0 -9.81 0)"));

        // The fluid reaching the domain's box: a leak.
        write_split(
            &dir,
            &[
                ("domain", "wall", 12),
                ("body_0_wall_0_fluid_to_solid", "mappedWall", 4),
            ],
            &[("body_0_wall_0_solid_to_fluid", "mappedWall", 4)],
        );
        assert!(matches!(finish(&case), Err(RunError::Leak { faces: 12 })));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_material_needs_no_mixture() {
        let mut s = study();
        if let Some(h) = s.heat.as_mut() {
            h.solids[1].conductivity = 200.0;
            h.solids[1].density = 2700.0;
            let (materials, of_body) = materials(h);
            assert_eq!(materials, [(200.0, 2700.0)]);
            assert_eq!(of_body, [0, 0]);
        }
        let thermo = solid_thermo(&[(200.0, 2700.0)]);
        assert!(thermo.contains("pureMixture"));
        assert!(thermo.contains("mixture\n{"));
    }
}
