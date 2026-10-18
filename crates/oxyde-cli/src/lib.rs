//! Oxyde without a window. Loads a `.oxyprt` part, rebuilds it through OpenCASCADE the way
//! the application does ([`Part::rebuild`] with an [`OcctKernel`]), and reports on the
//! result, so scripts, agents and continuous integration can replay a design headlessly:
//!
//! - `eval`: every feature's status and, per body, volume, surface area, centre of mass,
//!   bounds and topology counts, as text or JSON;
//! - `check`: the same measures compared with an `expected.json` within a relative
//!   tolerance, the seed of a reference oracle;
//! - `export`: the rebuilt body as STEP, IGES, binary STL, OBJ, 3MF or glTF (`.glb`);
//! - `snapshot`: a PNG from a standard view, drawn by the software renderer.
//!
//! The binary is a thin wrapper around [`run`]; tests call [`run`], or [`evaluate`],
//! [`check`], [`export`] and [`snapshot`] directly, without a process.
//!
//! Exit codes: 0 when everything is fine, 1 when a feature failed to rebuild or a check
//! did not pass, 2 when the command could not run at all (file not found, bad arguments).
//!
//! The JSON written by `eval --json` is an [`Evaluation`]; its field names are an
//! interface and do not change without a note in the changelog.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use oxyde_format::stl::{self, StlQuality, StlUnit};
use oxyde_format::{FormatError, Properties, load_part};
use oxyde_format::{drawing, gltf, obj, threemf};
use oxyde_kernel::{Kernel, Mesh, StepMaterial};
use oxyde_model::{FeatureStatus, Finish, ModelError, Part};
use oxyde_occt::{OcctError, OcctKernel};
use oxyde_render::{NORMAL_MESH_FRACTION, StandardView, Style};
use serde::{Deserialize, Serialize};

/// Everything is fine.
pub const EXIT_OK: i32 = 0;
/// A feature failed to rebuild, or a check did not pass.
pub const EXIT_FAILED: i32 = 1;
/// The command could not run: file not found, unreadable part, bad arguments.
pub const EXIT_ERROR: i32 = 2;

/// Why a command could not run.
#[derive(Debug, Clone, PartialEq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<FormatError> for Error {
    fn from(e: FormatError) -> Self {
        Error(e.to_string())
    }
}

impl From<ModelError> for Error {
    fn from(e: ModelError) -> Self {
        Error(e.to_string())
    }
}

impl From<OcctError> for Error {
    fn from(e: OcctError) -> Self {
        Error(e.to_string())
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error(e.to_string())
    }
}

impl From<png::EncodingError> for Error {
    fn from(e: png::EncodingError) -> Self {
        Error(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

// ---- Rebuilding ------------------------------------------------------------------------------

/// A part file rebuilt by its own OpenCASCADE kernel, which owns its shapes.
pub struct Rebuilt {
    pub path: PathBuf,
    pub document_id: String,
    pub properties: Properties,
    pub part: Part,
    pub kernel: OcctKernel,
}

impl Rebuilt {
    /// Status of every feature, in timeline order.
    pub fn features(&self) -> Vec<FeatureReport> {
        self.part
            .timeline()
            .into_iter()
            .map(|entry| {
                let (status, message) = match entry.status {
                    FeatureStatus::Ok => ("ok", None),
                    FeatureStatus::Warning(m) => ("warning", Some(m)),
                    FeatureStatus::Error(m) => ("error", Some(m)),
                    FeatureStatus::Suppressed => ("suppressed", None),
                    FeatureStatus::RolledBack => ("rolled_back", None),
                    FeatureStatus::Pending => ("pending", None),
                };
                FeatureReport {
                    name: entry.name,
                    type_name: entry.type_name.to_owned(),
                    status: status.to_owned(),
                    message,
                }
            })
            .collect()
    }

    /// The display mesh of every body, at the application's normal precision.
    pub fn display_mesh(&self) -> Result<Mesh> {
        let mut out = Mesh::default();
        let Some(body) = self.part.body() else {
            return Ok(out);
        };
        let deflection = oxyde_render::mesh_deflection(&body.topology, NORMAL_MESH_FRACTION);
        for solid in &body.solids {
            let mesh = self.kernel.tessellate(solid.shape, deflection)?;
            out.faces.extend(mesh.faces);
            out.edges.extend(mesh.edges);
        }
        Ok(out)
    }

    /// Diagonal of the box around every body, mm; 100 when there is no body.
    fn diagonal(&self) -> f64 {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for solid in self.part.body().map(|b| b.solids.as_slice()).unwrap_or(&[]) {
            if let Ok((a, b)) = self.kernel.bounds(solid.shape) {
                for (k, (a, b)) in [(a.x, b.x), (a.y, b.y), (a.z, b.z)].into_iter().enumerate() {
                    lo[k] = lo[k].min(a);
                    hi[k] = hi[k].max(b);
                }
            }
        }
        let d = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f64>().sqrt();
        if d.is_finite() && d > 0.0 { d } else { 100.0 }
    }
}

impl Drop for Rebuilt {
    fn drop(&mut self) {
        self.part.release(&mut self.kernel);
    }
}

/// Loads `path` and rebuilds every feature with a fresh OpenCASCADE kernel.
pub fn rebuild(path: &Path) -> Result<Rebuilt> {
    let file = load_part(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
    let mut kernel = OcctKernel::new();
    let mut part = Part::from_definition(file.definition)?;
    part.rebuild(&mut kernel);
    Ok(Rebuilt {
        path: path.to_path_buf(),
        document_id: file.document_id,
        properties: file.properties,
        part,
        kernel,
    })
}

// ---- Measures --------------------------------------------------------------------------------

/// One feature of the timeline and how its rebuild went.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureReport {
    pub name: String,
    /// SolidWorks-style type name: `Sketch`, `Extrude`, `Fillet`...
    #[serde(rename = "type")]
    pub type_name: String,
    /// `ok`, `warning`, `error`, `suppressed`, `rolled_back` or `pending`.
    pub status: String,
    /// The warning or the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl FeatureReport {
    pub fn failed(&self) -> bool {
        self.status == "error"
    }
}

/// An axis-aligned box, mm.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Bounds {
    pub fn diagonal(&self) -> f64 {
        (0..3)
            .map(|k| (self.max[k] - self.min[k]).powi(2))
            .sum::<f64>()
            .sqrt()
    }
}

/// What the kernel measures on a solid, or on every solid of a part together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measures {
    /// mm³.
    pub volume: f64,
    /// Surface area, mm².
    pub area: f64,
    /// Of a uniform density; for several bodies, weighted by volume.
    pub centre_of_mass: [f64; 3],
    pub bounds: Bounds,
    pub faces: usize,
    pub edges: usize,
    pub vertices: usize,
}

/// One solid body of the part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BodyReport {
    /// `Body 1`, `Body 2`... in the order of the Solid Bodies folder.
    pub name: String,
    /// The feature that made the body.
    pub created_by: String,
    #[serde(flatten)]
    pub measures: Measures,
}

/// What `eval` reports; `eval --json` writes it as is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evaluation {
    pub file: String,
    /// No feature failed to rebuild.
    pub ok: bool,
    pub features: Vec<FeatureReport>,
    pub bodies: Vec<BodyReport>,
    /// Every body together; `null` for a part without a solid.
    pub total: Option<Measures>,
}

impl Evaluation {
    pub fn failed_features(&self) -> impl Iterator<Item = &FeatureReport> {
        self.features.iter().filter(|f| f.failed())
    }
}

/// Loads, rebuilds and measures a part.
pub fn evaluate(path: &Path) -> Result<Evaluation> {
    measure(&rebuild(path)?)
}

/// Measures a rebuilt part.
pub fn measure(rebuilt: &Rebuilt) -> Result<Evaluation> {
    let features = rebuilt.features();
    let ok = !features.iter().any(FeatureReport::failed);
    let mut bodies = Vec::new();
    if let Some(body) = rebuilt.part.body() {
        for (i, solid) in body.solids.iter().enumerate() {
            let kernel = &rebuilt.kernel;
            let volume = kernel.volume(solid.shape)?;
            let c = kernel.centre_of_mass(solid.shape)?;
            let (lo, hi) = kernel.bounds(solid.shape)?;
            let counts = kernel.topology_counts(solid.shape)?;
            let area = kernel
                .topology(solid.shape)?
                .faces
                .iter()
                .map(|f| f.area)
                .sum::<f64>();
            bodies.push(BodyReport {
                name: format!("Body {}", i + 1),
                created_by: rebuilt
                    .part
                    .feature(solid.id.feature)
                    .map(|f| f.name().to_owned())
                    .unwrap_or_default(),
                measures: Measures {
                    volume,
                    area,
                    centre_of_mass: [c.x, c.y, c.z],
                    bounds: Bounds {
                        min: [lo.x, lo.y, lo.z],
                        max: [hi.x, hi.y, hi.z],
                    },
                    faces: counts.faces,
                    edges: counts.edges,
                    vertices: counts.vertices,
                },
            });
        }
    }
    let total = total_of(bodies.iter().map(|b| &b.measures));
    Ok(Evaluation {
        file: rebuilt.path.display().to_string(),
        ok,
        features,
        bodies,
        total,
    })
}

/// The measures of several bodies together: sums, the union of the boxes, and the centre
/// of mass weighted by volume.
fn total_of<'a>(bodies: impl Iterator<Item = &'a Measures>) -> Option<Measures> {
    let mut total: Option<Measures> = None;
    let mut moment = [0.0; 3];
    for m in bodies {
        for k in 0..3 {
            moment[k] += m.volume * m.centre_of_mass[k];
        }
        match &mut total {
            None => total = Some(m.clone()),
            Some(t) => {
                t.volume += m.volume;
                t.area += m.area;
                for k in 0..3 {
                    t.bounds.min[k] = t.bounds.min[k].min(m.bounds.min[k]);
                    t.bounds.max[k] = t.bounds.max[k].max(m.bounds.max[k]);
                }
                t.faces += m.faces;
                t.edges += m.edges;
                t.vertices += m.vertices;
            }
        }
    }
    if let Some(t) = &mut total {
        let volume = t.volume;
        if volume > 0.0 {
            t.centre_of_mass = moment.map(|x| x / volume);
        }
    }
    total
}

fn write_measures(out: &mut dyn Write, indent: &str, m: &Measures) -> io::Result<()> {
    let c = m.centre_of_mass;
    let (lo, hi) = (m.bounds.min, m.bounds.max);
    writeln!(out, "{indent}volume          {:.4} mm^3", m.volume)?;
    writeln!(out, "{indent}area            {:.4} mm^2", m.area)?;
    writeln!(
        out,
        "{indent}centre of mass  {:.4}  {:.4}  {:.4}",
        c[0], c[1], c[2]
    )?;
    writeln!(
        out,
        "{indent}bounds          {:.4}  {:.4}  {:.4}  to  {:.4}  {:.4}  {:.4}",
        lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]
    )?;
    writeln!(
        out,
        "{indent}faces / edges / vertices  {} / {} / {}",
        m.faces, m.edges, m.vertices
    )
}

/// `eval`'s text report.
pub fn write_evaluation(out: &mut dyn Write, eval: &Evaluation) -> io::Result<()> {
    writeln!(out, "{}", eval.file)?;
    writeln!(out, "Features ({}):", eval.features.len())?;
    for f in &eval.features {
        let status = match &f.message {
            Some(m) => format!("{}: {m}", f.status),
            None => f.status.clone(),
        };
        writeln!(out, "  {:<24} {:<16} {status}", f.name, f.type_name)?;
    }
    writeln!(out, "Bodies: {}", eval.bodies.len())?;
    for b in &eval.bodies {
        writeln!(out, "  {} ({})", b.name, b.created_by)?;
        write_measures(out, "    ", &b.measures)?;
    }
    if let (Some(total), true) = (&eval.total, eval.bodies.len() > 1) {
        writeln!(out, "  Total ({} bodies)", eval.bodies.len())?;
        write_measures(out, "    ", total)?;
    }
    let failed = eval.failed_features().count();
    match failed {
        0 => writeln!(out, "Result: ok"),
        1 => writeln!(out, "Result: 1 feature failed"),
        n => writeln!(out, "Result: {n} features failed"),
    }
}

// ---- Check -----------------------------------------------------------------------------------

/// An `expected.json`: the measures of the whole part to compare with, each optional.
/// Counts must match exactly; the others within the tolerance (see [`compare`]).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bodies: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub centre_of_mass: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faces: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edges: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertices: Option<usize>,
}

impl Expected {
    /// What `eval` found, as expectations: the file a passing `check` reads back.
    pub fn from_evaluation(eval: &Evaluation) -> Self {
        let Some(t) = &eval.total else {
            return Expected {
                bodies: Some(0),
                ..Default::default()
            };
        };
        Expected {
            bodies: Some(eval.bodies.len()),
            volume: Some(t.volume),
            area: Some(t.area),
            centre_of_mass: Some(t.centre_of_mass),
            bounds: Some(t.bounds),
            faces: Some(t.faces),
            edges: Some(t.edges),
            vertices: Some(t.vertices),
        }
    }
}

/// One line of the pass/fail table.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckRow {
    pub measure: String,
    pub expected: String,
    pub actual: String,
    /// Relative difference, where it means something.
    pub error: Option<f64>,
    pub pass: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckReport {
    pub rows: Vec<CheckRow>,
}

impl CheckReport {
    pub fn passed(&self) -> bool {
        self.rows.iter().all(|r| r.pass)
    }

    pub fn failures(&self) -> impl Iterator<Item = &CheckRow> {
        self.rows.iter().filter(|r| !r.pass)
    }
}

/// Compares the whole part's measures with `expected`. Volume and area pass within
/// `tol` of the expected value; a coordinate (centre of mass, bounds) within `tol` of the
/// part's size, the diagonal of the expected bounds (else of the measured ones), since a
/// coordinate can be 0; counts must match exactly; every feature must have rebuilt.
pub fn compare(eval: &Evaluation, expected: &Expected, tol: f64) -> CheckReport {
    let mut rows = Vec::new();
    let failed = eval.failed_features().count();
    rows.push(CheckRow {
        measure: "features".to_owned(),
        expected: "all rebuilt".to_owned(),
        actual: match failed {
            0 => "all rebuilt".to_owned(),
            n => format!("{n} failed"),
        },
        error: None,
        pass: failed == 0,
    });
    let mut count = |name: &str, want: Option<usize>, got: Option<usize>| {
        if let Some(want) = want {
            rows.push(CheckRow {
                measure: name.to_owned(),
                expected: want.to_string(),
                actual: got.map_or("none".to_owned(), |g| g.to_string()),
                error: None,
                pass: got == Some(want),
            });
        }
    };
    let total = eval.total.as_ref();
    count("bodies", expected.bodies, Some(eval.bodies.len()));
    count("faces", expected.faces, total.map(|t| t.faces));
    count("edges", expected.edges, total.map(|t| t.edges));
    count("vertices", expected.vertices, total.map(|t| t.vertices));

    let size = expected
        .bounds
        .map(|b| b.diagonal())
        .or_else(|| total.map(|t| t.bounds.diagonal()))
        .filter(|d| *d > 0.0)
        .unwrap_or(1.0);
    let mut value = |name: String, want: Option<f64>, got: Option<f64>, scale: f64| {
        let Some(want) = want else { return };
        let (actual, error, pass) = match got {
            Some(got) => {
                let error = (got - want).abs() / scale;
                (format!("{got:.6}"), Some(error), error <= tol)
            }
            None => ("none".to_owned(), None, false),
        };
        rows.push(CheckRow {
            measure: name,
            expected: format!("{want:.6}"),
            actual,
            error,
            pass,
        });
    };
    let relative = |want: f64| if want.abs() > 0.0 { want.abs() } else { 1.0 };
    if let Some(want) = expected.volume {
        value(
            "volume".to_owned(),
            Some(want),
            total.map(|t| t.volume),
            relative(want),
        );
    }
    if let Some(want) = expected.area {
        value(
            "area".to_owned(),
            Some(want),
            total.map(|t| t.area),
            relative(want),
        );
    }
    let axes = ["x", "y", "z"];
    if let Some(want) = expected.centre_of_mass {
        for (k, axis) in axes.iter().enumerate() {
            value(
                format!("centre of mass {axis}"),
                Some(want[k]),
                total.map(|t| t.centre_of_mass[k]),
                size,
            );
        }
    }
    if let Some(want) = expected.bounds {
        for (k, axis) in axes.iter().enumerate() {
            value(
                format!("bounds min {axis}"),
                Some(want.min[k]),
                total.map(|t| t.bounds.min[k]),
                size,
            );
        }
        for (k, axis) in axes.iter().enumerate() {
            value(
                format!("bounds max {axis}"),
                Some(want.max[k]),
                total.map(|t| t.bounds.max[k]),
                size,
            );
        }
    }
    CheckReport { rows }
}

/// Rebuilds `path` and compares it with the `expected.json` at `expected`.
pub fn check(path: &Path, expected: &Path, tol: f64) -> Result<CheckReport> {
    let text = std::fs::read_to_string(expected)
        .map_err(|e| Error(format!("{}: {e}", expected.display())))?;
    let expected: Expected =
        serde_json::from_str(&text).map_err(|e| Error(format!("{}: {e}", expected.display())))?;
    Ok(compare(&evaluate(path)?, &expected, tol))
}

/// `check`'s pass/fail table.
pub fn write_check(out: &mut dyn Write, report: &CheckReport) -> io::Result<()> {
    writeln!(
        out,
        "{:<18} {:>16} {:>16} {:>10}  {}",
        "measure", "expected", "actual", "error", "result"
    )?;
    for r in &report.rows {
        let error = r.error.map_or(String::new(), |e| format!("{e:.2e}"));
        writeln!(
            out,
            "{:<18} {:>16} {:>16} {:>10}  {}",
            r.measure,
            r.expected,
            r.actual,
            error,
            if r.pass { "pass" } else { "FAIL" }
        )?;
    }
    let failed = report.failures().count();
    match failed {
        0 => writeln!(out, "Result: pass ({} checks)", report.rows.len()),
        n => writeln!(out, "Result: FAIL ({n} of {} checks)", report.rows.len()),
    }
}

// ---- Export ----------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// STEP AP214 with colours and material.
    Step,
    /// IGES 5.3, the solids as B-rep.
    Iges,
    /// Binary STL in millimetres, at the application's Normal quality.
    Stl,
    /// Wavefront OBJ in millimetres, one object per body, with normals.
    Obj,
    /// 3MF in millimetres, one named, coloured object per body.
    ThreeMf,
    /// glTF binary in metres, one named object per body, with a physically based
    /// material per appearance (the finish guessed from the material, as the Render tab
    /// does by default).
    Gltf,
}

impl ExportFormat {
    /// From the file's extension: `.step`/`.stp`, `.igs`/`.iges`, `.stl`, `.obj`, `.3mf`
    /// or `.glb`.
    pub fn of(path: &Path) -> Result<Self> {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("step" | "stp") => Ok(ExportFormat::Step),
            Some("igs" | "iges") => Ok(ExportFormat::Iges),
            Some("stl") => Ok(ExportFormat::Stl),
            Some("obj") => Ok(ExportFormat::Obj),
            Some("3mf") => Ok(ExportFormat::ThreeMf),
            Some("glb") => Ok(ExportFormat::Gltf),
            _ => Err(Error(format!(
                "{}: the output must be a .step, .stp, .igs, .iges, .stl, .obj, .3mf or .glb file",
                path.display()
            ))),
        }
    }
}

/// Writes the rebuilt body of `rebuilt` to `out`, in the format its extension names. A
/// part without a solid is an error.
pub fn export_rebuilt(rebuilt: &Rebuilt, out: &Path) -> Result<ExportFormat> {
    let format = ExportFormat::of(out)?;
    let Some(body) = rebuilt.part.body() else {
        return Err(Error("nothing to export: the part has no solid".to_owned()));
    };
    let kernel = &rebuilt.kernel;
    let name = rebuilt
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".to_owned());
    let io_error = |e: std::io::Error| Error(format!("{}: {e}", out.display()));
    match format {
        ExportFormat::Step => {
            let shapes = body.shapes();
            let colors = rebuilt.part.step_colors(body);
            // The part's material goes with every solid, as its name and density.
            let material = rebuilt.part.material().map(|m| StepMaterial {
                name: m.name.clone(),
                description: m.category.clone(),
                density: m.density,
            });
            let materials = vec![material; shapes.len()];
            kernel.export_step(&shapes, &colors, &materials, out)?;
        }
        ExportFormat::Iges => {
            kernel.export_iges(&body.shapes(), out)?;
        }
        ExportFormat::Stl | ExportFormat::Obj | ExportFormat::ThreeMf | ExportFormat::Gltf => {
            let (deflection, angle) = StlQuality::Normal
                .settings(rebuilt.diagonal())
                .unwrap_or((0.05, 20.0));
            // One mesh per body, named after the file (and the body when there are
            // several), with the body's colour.
            let colors = rebuilt.part.step_colors(body);
            let mut bodies: Vec<(String, Mesh, Option<[u8; 3]>)> = Vec::new();
            for (i, solid) in body.solids.iter().enumerate() {
                let mesh =
                    kernel.tessellate_for_export(solid.shape, deflection, angle.to_radians())?;
                let color = colors
                    .get(i)
                    .and_then(|c| c.solids.first().copied().flatten())
                    .map(|c| c.rgb);
                let object = if body.solids.len() > 1 {
                    format!("{name} Body {}", i + 1)
                } else {
                    name.clone()
                };
                bodies.push((object, mesh, color));
            }
            match format {
                ExportFormat::Stl => {
                    let mut mesh = Mesh::default();
                    for (_, m, _) in &bodies {
                        mesh.faces.extend(m.faces.iter().cloned());
                        mesh.edges.extend(m.edges.iter().cloned());
                    }
                    let triangles = stl::triangles(&mesh, StlUnit::Millimetre.per_mm());
                    stl::save(out, &name, &triangles, true).map_err(io_error)?;
                }
                ExportFormat::Gltf => {
                    // One material per distinct appearance and finish, face by face.
                    let mut materials: Vec<gltf::GltfMaterial> = Vec::new();
                    let mut faces: Vec<Vec<usize>> = Vec::new();
                    for solid in &body.solids {
                        let indices = solid
                            .faces
                            .clone()
                            .map(|k| {
                                let part = &rebuilt.part;
                                let appearance = part.face_appearance(body, k);
                                let finish = Finish::guess(part.material(), appearance);
                                let m = gltf::GltfMaterial::from_finish(
                                    appearance.map_or(Style::default().face, |a| a.color),
                                    appearance.map_or(0.0, |a| a.transparency),
                                    finish,
                                );
                                match materials.iter().position(|x| *x == m) {
                                    Some(i) => i,
                                    None => {
                                        materials.push(m);
                                        materials.len() - 1
                                    }
                                }
                            })
                            .collect();
                        faces.push(indices);
                    }
                    let objects: Vec<gltf::GltfObject<'_>> = bodies
                        .iter()
                        .zip(&faces)
                        .map(|((n, m, _), f)| gltf::GltfObject {
                            name: n,
                            mesh: m,
                            face_materials: f,
                        })
                        .collect();
                    let generator = format!("Oxyde {}", env!("CARGO_PKG_VERSION"));
                    gltf::save(out, &objects, &materials, &generator).map_err(io_error)?;
                }
                ExportFormat::Obj => {
                    let meshes: Vec<obj::NamedMesh<'_>> = bodies
                        .iter()
                        .map(|(n, m, _)| obj::NamedMesh { name: n, mesh: m })
                        .collect();
                    obj::save(out, &meshes, 1.0).map_err(io_error)?;
                }
                _ => {
                    let meshes: Vec<threemf::NamedMesh<'_>> = bodies
                        .iter()
                        .map(|(n, m, c)| threemf::NamedMesh {
                            name: n,
                            mesh: m,
                            color: *c,
                        })
                        .collect();
                    threemf::save(out, &meshes, StlUnit::Millimetre).map_err(io_error)?;
                }
            }
        }
    }
    Ok(format)
}

/// Rebuilds `path` and writes its body to `out` (the format by extension). Returns the
/// features' statuses as well, since a body built from failed features is worth a look.
pub fn export(path: &Path, out: &Path) -> Result<(ExportFormat, Vec<FeatureReport>)> {
    let rebuilt = rebuild(path)?;
    let format = export_rebuilt(&rebuilt, out)?;
    Ok((format, rebuilt.features()))
}

// ---- Snapshot --------------------------------------------------------------------------------

/// Draws the rebuilt part from `view` into a `width` x `height` PNG at `out`. A part
/// without a solid gives an empty viewport.
pub fn snapshot_rebuilt(
    rebuilt: &Rebuilt,
    out: &Path,
    view: StandardView,
    width: usize,
    height: usize,
) -> Result<()> {
    let mesh = rebuilt.display_mesh()?;
    let frame = oxyde_render::snapshot(&mesh, view, width, height, &Style::default());
    let file = std::fs::File::create(out).map_err(|e| Error(format!("{}: {e}", out.display())))?;
    let mut encoder = png::Encoder::new(io::BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&frame.rgba)?;
    writer.finish()?;
    Ok(())
}

/// Rebuilds `path` and draws it to `out`; returns the features' statuses as well.
pub fn snapshot(
    path: &Path,
    out: &Path,
    view: StandardView,
    width: usize,
    height: usize,
) -> Result<Vec<FeatureReport>> {
    let rebuilt = rebuild(path)?;
    snapshot_rebuilt(&rebuilt, out, view, width, height)?;
    Ok(rebuilt.features())
}

// ---- Drawing ---------------------------------------------------------------------------------

/// The standard views of the rebuilt part, hidden lines removed, laid out on a sheet:
/// what File › Export › Drawing makes, titled after the file.
pub fn drawing_sheet(
    rebuilt: &Rebuilt,
    size: drawing::SheetSize,
    projection: drawing::Projection,
    show_hidden: bool,
) -> Result<drawing::Sheet> {
    let body = rebuilt
        .part
        .body()
        .ok_or_else(|| Error("the part has no solid to draw".to_owned()))?;
    let shapes = body.shapes();
    let mut size_mm: f64 = 1.0;
    for shape in &shapes {
        let (lo, hi) = rebuilt.kernel.bounds(*shape)?;
        size_mm = size_mm.max(lo.distance(hi));
    }
    let deflection = (size_mm * 1e-3).max(0.005);
    let mut views = Vec::new();
    for kind in drawing::ViewKind::ALL {
        let edges = rebuilt
            .kernel
            .project_edges(&shapes, &kind.plane(), deflection)?
            .ok_or_else(|| Error("the kernel cannot remove hidden lines".to_owned()))?;
        let lines = |lines: Vec<Vec<oxyde_geom::Point2>>| {
            lines
                .into_iter()
                .map(|l| l.into_iter().map(|p| [p.x, p.y]).collect())
                .collect()
        };
        views.push((
            kind,
            drawing::ViewEdges {
                visible: lines(edges.visible),
                hidden: lines(edges.hidden),
            },
        ));
    }
    let title = drawing::TitleBlock {
        title: rebuilt
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        part_number: rebuilt.properties.part_number.clone(),
        material: rebuilt
            .part
            .material()
            .map_or_else(|| rebuilt.properties.material.clone(), |m| m.name.clone()),
        drawn_by: String::new(),
        date: drawing::today(),
    };
    Ok(drawing::layout(
        views,
        size,
        projection,
        None,
        title,
        show_hidden,
    ))
}

/// Rebuilds `path` and writes its drawing to `out`, PDF, SVG or DXF by the extension;
/// returns the features' statuses and the sheet's scale.
pub fn drawing(
    path: &Path,
    out: &Path,
    size: drawing::SheetSize,
    projection: drawing::Projection,
    show_hidden: bool,
) -> Result<(Vec<FeatureReport>, f64)> {
    let rebuilt = rebuild(path)?;
    let sheet = drawing_sheet(&rebuilt, size, projection, show_hidden)?;
    let extension = out
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let bytes = match extension.as_str() {
        "pdf" => drawing::to_pdf(&sheet),
        "svg" => drawing::to_svg(&sheet).into_bytes(),
        "dxf" => drawing::to_dxf(&sheet).into_bytes(),
        other => {
            return Err(Error(format!(
                "cannot write a drawing as .{other}: use .pdf, .svg or .dxf"
            )));
        }
    };
    std::fs::write(out, bytes).map_err(|e| Error(format!("{}: {e}", out.display())))?;
    Ok((rebuilt.features(), sheet.scale))
}

/// A sheet size, as `--sheet` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SheetArg {
    A4,
    A3,
    A2,
    A1,
    A0,
}

impl From<SheetArg> for drawing::SheetSize {
    fn from(sheet: SheetArg) -> Self {
        match sheet {
            SheetArg::A4 => drawing::SheetSize::A4,
            SheetArg::A3 => drawing::SheetSize::A3,
            SheetArg::A2 => drawing::SheetSize::A2,
            SheetArg::A1 => drawing::SheetSize::A1,
            SheetArg::A0 => drawing::SheetSize::A0,
        }
    }
}

// ---- Command line ----------------------------------------------------------------------------

#[derive(Debug, Parser)]
#[command(
    name = "oxyde-cli",
    version,
    about = "Oxyde parts from the command line: rebuild, measure, check, export and picture a .oxyprt without the application."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Rebuild a part and print every feature's status and each body's volume, area,
    /// centre of mass, bounds and face/edge/vertex counts. Exit 1 if a feature failed.
    Eval {
        /// The .oxyprt file.
        part: PathBuf,
        /// One JSON object instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Rebuild a part and compare its measures with an expected.json (volume, area,
    /// centre_of_mass, bounds, bodies, faces, edges, vertices; each optional). Exit 1
    /// on any failure.
    Check {
        /// The .oxyprt file.
        part: PathBuf,
        /// The expected.json to compare with.
        #[arg(long)]
        expect: PathBuf,
        /// Relative tolerance for volume and area, and for coordinates relative to the
        /// part's size.
        #[arg(long, default_value_t = 1e-3)]
        tol: f64,
    },
    /// Rebuild a part and write its body as STEP (.step, .stp), IGES (.igs, .iges), binary
    /// STL (.stl), OBJ (.obj), 3MF (.3mf) or glTF (.glb). Exit 1 if a feature failed (the
    /// file is still written).
    Export {
        /// The .oxyprt file.
        part: PathBuf,
        /// Where to write; the extension picks the format.
        #[arg(long)]
        out: PathBuf,
    },
    /// Rebuild a part and draw it to a PNG with the software renderer. Exit 1 if a
    /// feature failed (the picture is still written).
    Snapshot {
        /// The .oxyprt file.
        part: PathBuf,
        /// The PNG to write.
        #[arg(long)]
        out: PathBuf,
        #[arg(long, value_enum, default_value_t = View::Iso)]
        view: View,
        #[arg(long, default_value_t = 1024)]
        width: usize,
        #[arg(long, default_value_t = 768)]
        height: usize,
    },
    /// Rebuild a part and write its front, top, right and isometric views on a sheet with
    /// a title block, hidden lines dashed: PDF (.pdf), SVG (.svg) or DXF (.dxf, at the
    /// views' true size). Exit 1 if a feature failed (the drawing is still written).
    Drawing {
        /// The .oxyprt file.
        part: PathBuf,
        /// Where to write; the extension picks the format.
        #[arg(long)]
        out: PathBuf,
        #[arg(long, value_enum, default_value_t = SheetArg::A4)]
        sheet: SheetArg,
        /// Third angle projection (top view above the front) instead of first angle.
        #[arg(long)]
        third_angle: bool,
        /// Leave the hidden lines out.
        #[arg(long)]
        no_hidden: bool,
    },
}

/// A standard view, as `--view` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum View {
    Iso,
    Front,
    Back,
    Top,
    Bottom,
    Left,
    Right,
}

impl From<View> for StandardView {
    fn from(view: View) -> Self {
        match view {
            View::Iso => StandardView::Isometric,
            View::Front => StandardView::Front,
            View::Back => StandardView::Back,
            View::Top => StandardView::Top,
            View::Bottom => StandardView::Bottom,
            View::Left => StandardView::Left,
            View::Right => StandardView::Right,
        }
    }
}

/// Lists the features that failed, one per line.
fn write_failures(out: &mut dyn Write, features: &[FeatureReport]) -> io::Result<bool> {
    let mut failed = false;
    for f in features.iter().filter(|f| f.failed()) {
        failed = true;
        writeln!(
            out,
            "{}: error: {}",
            f.name,
            f.message.as_deref().unwrap_or("")
        )?;
    }
    Ok(failed)
}

fn execute(command: Command, out: &mut dyn Write) -> Result<i32> {
    match command {
        Command::Eval { part, json } => {
            let eval = evaluate(&part)?;
            if json {
                serde_json::to_writer_pretty(&mut *out, &eval)?;
                writeln!(out)?;
            } else {
                write_evaluation(out, &eval)?;
            }
            Ok(if eval.ok { EXIT_OK } else { EXIT_FAILED })
        }
        Command::Check { part, expect, tol } => {
            let report = check(&part, &expect, tol)?;
            write_check(out, &report)?;
            Ok(if report.passed() {
                EXIT_OK
            } else {
                EXIT_FAILED
            })
        }
        Command::Export { part, out: file } => {
            let (format, features) = export(&part, &file)?;
            let failed = write_failures(out, &features)?;
            writeln!(out, "wrote {} ({format:?})", file.display())?;
            Ok(if failed { EXIT_FAILED } else { EXIT_OK })
        }
        Command::Snapshot {
            part,
            out: file,
            view,
            width,
            height,
        } => {
            let features = snapshot(&part, &file, view.into(), width, height)?;
            let failed = write_failures(out, &features)?;
            writeln!(out, "wrote {} ({width}x{height})", file.display())?;
            Ok(if failed { EXIT_FAILED } else { EXIT_OK })
        }
        Command::Drawing {
            part,
            out: file,
            sheet,
            third_angle,
            no_hidden,
        } => {
            let projection = if third_angle {
                drawing::Projection::ThirdAngle
            } else {
                drawing::Projection::FirstAngle
            };
            let (features, scale) = drawing(&part, &file, sheet.into(), projection, !no_hidden)?;
            let failed = write_failures(out, &features)?;
            writeln!(
                out,
                "wrote {} (scale {})",
                file.display(),
                drawing::scale_text(scale)
            )?;
            Ok(if failed { EXIT_FAILED } else { EXIT_OK })
        }
    }
}

/// Runs the command line `args` (the program name first, as `std::env::args_os` gives
/// them), writing to `out` and `err`, and returns the exit code.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            let text = e.to_string();
            let _ = if e.use_stderr() {
                write!(err, "{text}")
            } else {
                write!(out, "{text}")
            };
            return e.exit_code();
        }
    };
    match execute(cli.command, out) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            EXIT_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measures(volume: f64, c: [f64; 3]) -> Measures {
        Measures {
            volume,
            area: 1.0,
            centre_of_mass: c,
            bounds: Bounds {
                min: [0.0; 3],
                max: [10.0; 3],
            },
            faces: 6,
            edges: 12,
            vertices: 8,
        }
    }

    fn evaluation(total: Option<Measures>, ok: bool) -> Evaluation {
        Evaluation {
            file: "x.oxyprt".to_owned(),
            ok,
            features: vec![FeatureReport {
                name: "Boss-Extrude1".to_owned(),
                type_name: "Extrude".to_owned(),
                status: if ok { "ok" } else { "error" }.to_owned(),
                message: (!ok).then(|| "no profile".to_owned()),
            }],
            bodies: total
                .iter()
                .map(|m| BodyReport {
                    name: "Body 1".to_owned(),
                    created_by: "Boss-Extrude1".to_owned(),
                    measures: m.clone(),
                })
                .collect(),
            total,
        }
    }

    #[test]
    fn totals_weight_the_centre_of_mass_by_volume() {
        let a = measures(1.0, [0.0, 0.0, 0.0]);
        let b = measures(3.0, [4.0, 0.0, 0.0]);
        let t = total_of([&a, &b].into_iter()).unwrap();
        assert_eq!(t.volume, 4.0);
        assert_eq!(t.centre_of_mass, [3.0, 0.0, 0.0]);
        assert_eq!(t.faces, 12);
        assert!(total_of(std::iter::empty()).is_none());
    }

    #[test]
    fn check_is_relative_for_volume_and_by_size_for_coordinates() {
        let eval = evaluation(Some(measures(1000.0, [5.0, 5.0, 0.0])), true);
        let mut expected = Expected::from_evaluation(&eval);
        assert!(compare(&eval, &expected, 1e-3).passed());
        // 0.05 % off the volume passes at 1e-3, not at 1e-4.
        expected.volume = Some(1000.5);
        assert!(compare(&eval, &expected, 1e-3).passed());
        assert!(!compare(&eval, &expected, 1e-4).passed());
        // A coordinate at 0 is judged against the part's size (diagonal 17.3 mm).
        expected.volume = Some(1000.0);
        expected.centre_of_mass = Some([5.0, 5.0, 0.01]);
        assert!(compare(&eval, &expected, 1e-3).passed());
        assert!(!compare(&eval, &expected, 1e-4).passed());
        // Counts are exact.
        expected.faces = Some(7);
        let report = compare(&eval, &expected, 1e-3);
        assert_eq!(
            report
                .failures()
                .map(|r| r.measure.as_str())
                .collect::<Vec<_>>(),
            ["faces"]
        );
    }

    #[test]
    fn a_failed_feature_fails_the_check_and_the_evaluation() {
        let eval = evaluation(Some(measures(1000.0, [5.0; 3])), false);
        assert_eq!(eval.failed_features().count(), 1);
        let report = compare(&eval, &Expected::default(), 1e-3);
        assert_eq!(
            report
                .failures()
                .map(|r| r.measure.as_str())
                .collect::<Vec<_>>(),
            ["features"]
        );
        let empty = evaluation(None, true);
        let report = compare(
            &empty,
            &Expected {
                bodies: Some(1),
                ..Default::default()
            },
            1e-3,
        );
        assert!(!report.passed());
    }

    #[test]
    fn json_field_names_are_stable() {
        let eval = evaluation(Some(measures(1000.0, [5.0; 3])), true);
        let json: serde_json::Value = serde_json::to_value(&eval).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["bodies", "features", "file", "ok", "total"]);
        let body = &json["bodies"][0];
        for key in [
            "name",
            "created_by",
            "volume",
            "area",
            "centre_of_mass",
            "bounds",
            "faces",
            "edges",
            "vertices",
        ] {
            assert!(!body[key].is_null(), "{key}");
        }
        assert_eq!(json["features"][0]["type"], "Extrude");
        let back: Evaluation = serde_json::from_value(json).unwrap();
        assert_eq!(back, eval);
    }

    #[test]
    fn usage_errors_exit_2_and_help_exits_0() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run(["oxyde-cli", "frobnicate"], &mut out, &mut err), 2);
        assert!(!err.is_empty());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run(["oxyde-cli", "--help"], &mut out, &mut err), 0);
        assert!(String::from_utf8(out).unwrap().contains("snapshot"));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            run(
                ["oxyde-cli", "eval", "does-not-exist.oxyprt"],
                &mut out,
                &mut err
            ),
            EXIT_ERROR
        );
        assert!(String::from_utf8(err).unwrap().starts_with("error: "));
    }
}
