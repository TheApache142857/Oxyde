//! Other CAD programs' files, read through [cadmpeg](https://github.com/cadmpeg/cadmpeg)
//! (Apache-2.0) and handed on as STEP: the rest of the import is the STEP import's.
//!
//! cadmpeg decodes the file into its own B-rep document and writes that as STEP. How much
//! of a file comes through depends on the format; see [`FORMATS`] and cadmpeg's own
//! support levels.

use std::fmt;
use std::io::Cursor;
use std::path::Path;

use cadmpeg_core::decode::{DecodeArena, DecodeContext, View};
use cadmpeg_ir::codec::DecodeOptions;
use cadmpeg_ir::codec::write::{EncodeInput, target::TargetRequest};
use cadmpeg_ir::report::Severity;
use cadmpeg_ir::report::loss::LossNote;
use cadmpeg_registry::{DETECTION_PREFIX_LEN, Format, InputCatalog, ResolvedSource};

/// The formats offered in the open dialog: a name and its extensions. STEP itself is not
/// here: Oxyde reads it directly.
pub const FORMATS: &[(&str, &[&str])] = &[
    ("SolidWorks part", &["sldprt"]),
    ("Fusion 360", &["f3d"]),
    ("Inventor", &["ipt", "iam"]),
    ("CATIA V5 part", &["catpart"]),
    ("FreeCAD", &["fcstd"]),
    ("Rhino", &["3dm"]),
    ("IGES", &["igs", "iges"]),
    ("ACIS", &["sat", "sab", "smt", "smb"]),
    ("NX / Creo part", &["prt"]),
    ("Creo View", &["pvz"]),
];

/// Whether a file of this extension (no dot, any case) goes through cadmpeg.
pub fn handles_extension(extension: &str) -> bool {
    FORMATS
        .iter()
        .flat_map(|(_, extensions)| extensions.iter())
        .any(|e| e.eq_ignore_ascii_case(extension))
}

/// Whether a file goes through cadmpeg, judged by its extension.
pub fn handles_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(handles_extension)
}

/// Why a file could not be turned into STEP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertError {
    /// cadmpeg recognizes no format in the file.
    Unrecognized,
    /// cadmpeg recognized the format but could not read it, or not well enough to write
    /// STEP. The text is cadmpeg's.
    Failed(String),
    /// The file decoded but the STEP holds no faces: the file has none (a sketch, curves,
    /// a bare surface), or cadmpeg could not write the ones it has.
    NoGeometry,
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unrecognized => write!(f, "not a CAD format cadmpeg can read"),
            Self::Failed(why) => write!(f, "cadmpeg could not read the file: {why}"),
            Self::NoGeometry => write!(
                f,
                "cadmpeg found no faces or solids in the file that it can write as STEP yet"
            ),
        }
    }
}

impl std::error::Error for ConvertError {}

/// What came out of a conversion.
#[derive(Debug, Clone)]
pub struct Converted {
    /// The STEP file, ready for the STEP import.
    pub step: Vec<u8>,
    /// cadmpeg's name for the format it read, e.g. `sldprt`.
    pub format: String,
    /// How many things cadmpeg reported it could not carry over (0 when nothing was lost):
    /// those it could not read, and those it read but could not write as STEP. Only its
    /// warnings and worse count; its notes of information (native metadata, tolerances,
    /// appearance details STEP has no place for) come with every file and are left out.
    pub losses: usize,
}

/// Turns the bytes of a CAD file into a STEP file. Slow for big files: run it off the UI
/// thread.
pub fn to_step(bytes: &[u8]) -> Result<Converted, ConvertError> {
    let failed = |e: &dyn fmt::Display| ConvertError::Failed(e.to_string());
    let encoder = cadmpeg_registry::build_encoder(Format::Step);
    let options = DecodeOptions {
        depth: encoder.needs().request(),
        ..DecodeOptions::default()
    };
    let catalog = InputCatalog::with_builtins();
    let arena = DecodeArena::new();
    let ctx = DecodeContext::new(&arena, &options.policy, options.depth);
    let mut source = Cursor::new(bytes);
    let mut prefix =
        cadmpeg_container::compound::read_detection_prefix(&ctx, &mut source, DETECTION_PREFIX_LEN)
            .map_err(|e| failed(&e))?;
    let codec = match catalog
        .resolve_seekable_source(&ctx, View::over_retained(&prefix), &mut source, None)
        .map_err(|e| failed(&e))?
    {
        ResolvedSource::Native { codec, .. } => codec,
        _ => return Err(ConvertError::Unrecognized),
    };
    let format = codec.id().to_string();
    ctx.complete_input(&mut source, &mut prefix)
        .map_err(|e| failed(&e))?;
    let decoded = codec.decode_with_context(&ctx, View::over_retained(&prefix));
    ctx.finish_session().map_err(|e| failed(&e))?;
    let decoded = decoded.map_err(|e| failed(&e))?;
    let decode_losses = counted(&decoded.report().losses);
    let (ir, _report, fidelity) = decoded.into_parts();
    let plan = encoder
        .plan(
            EncodeInput::new(&ir, Some(&fidelity)),
            TargetRequest::Inherit,
        )
        .map_err(|e| failed(&e))?;
    let mut step = Vec::new();
    // The STEP encoder leaves out what STEP cannot carry, such as a face whose edges have
    // no 3D curve, and reports it here rather than failing.
    let export = plan.write_to(&mut step).map_err(|e| failed(&e))?;
    if !has_geometry(&step) {
        return Err(ConvertError::NoGeometry);
    }
    Ok(Converted {
        step,
        format,
        losses: decode_losses + counted(&export.losses),
    })
}

/// How many of cadmpeg's loss notes say something was approximated or left out.
fn counted(losses: &[LossNote]) -> usize {
    losses
        .iter()
        .filter(|loss| loss.severity >= Severity::Warning)
        .count()
}

/// Whether a STEP file holds any faces: cadmpeg writes a valid file without any for a
/// document it read no faces from, or only curves and unbounded surfaces. cadmpeg writes
/// B-rep faces as ADVANCED_FACE; the others are there for STEP written another way.
fn has_geometry(step: &[u8]) -> bool {
    const FACES: &[&[u8]] = &[b"ADVANCED_FACE", b"FACE_SURFACE", b"FACETED_BREP"];
    FACES
        .iter()
        .any(|face| step.windows(face.len()).any(|w| w == *face))
}
