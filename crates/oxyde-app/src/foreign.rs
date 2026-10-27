//! Other CAD programs' files (SolidWorks, Fusion 360, Inventor, CATIA...): translated to
//! STEP by cadmpeg when they are imported, then read like any STEP file. The part keeps
//! the STEP, so it never needs the original file, or cadmpeg, again.

use std::path::{Path, PathBuf};

use crate::jobs::Progress;

const STEP: &[&str] = &["step", "stp"];

/// What the import commands take, for their labels.
#[cfg(feature = "cadmpeg")]
pub(crate) const IMPORT_KIND: &str = "STEP or CAD file";
#[cfg(not(feature = "cadmpeg"))]
pub(crate) const IMPORT_KIND: &str = "STEP";

/// Says, under an import command, which other programs' files it takes.
#[cfg(feature = "cadmpeg")]
pub(crate) const FOREIGN_HINT: &str = "\nSolidWorks, Fusion 360, Inventor, CATIA, FreeCAD, Rhino, IGES, ACIS, NX and Creo files \
     are translated to STEP by cadmpeg; how much comes through depends on the format.";
#[cfg(not(feature = "cadmpeg"))]
pub(crate) const FOREIGN_HINT: &str = "";

/// The other programs' formats this build reads: a name and its extensions.
#[cfg(feature = "cadmpeg")]
fn foreign_formats() -> &'static [(&'static str, &'static [&'static str])] {
    oxyde_cadmpeg::FORMATS
}

#[cfg(not(feature = "cadmpeg"))]
fn foreign_formats() -> &'static [(&'static str, &'static [&'static str])] {
    &[]
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub(crate) fn is_step(path: &Path) -> bool {
    has_extension(path, STEP)
}

/// Whether `path` is another program's file that goes through cadmpeg.
pub(crate) fn is_foreign(path: &Path) -> bool {
    foreign_formats()
        .iter()
        .any(|(_, extensions)| has_extension(path, extensions))
}

/// Whether Import and Open take `path` as a file to import: STEP or another program's.
pub(crate) fn is_importable(path: &Path) -> bool {
    is_step(path) || is_foreign(path)
}

/// Every extension [`is_importable`] takes.
pub(crate) fn importable_extensions() -> Vec<&'static str> {
    let mut all = STEP.to_vec();
    for (_, extensions) in foreign_formats() {
        all.extend_from_slice(extensions);
    }
    all
}

/// Adds STEP and each format of the other programs as filters of `dialog`.
pub(crate) fn add_filters(mut dialog: rfd::FileDialog) -> rfd::FileDialog {
    dialog = dialog.add_filter("STEP", STEP);
    for (name, extensions) in foreign_formats() {
        dialog = dialog.add_filter(*name, extensions);
    }
    dialog
}

/// Asks for a file to import: everything importable first, then each format on its own.
pub(crate) fn pick_importable() -> Option<PathBuf> {
    let dialog = if foreign_formats().is_empty() {
        rfd::FileDialog::new()
    } else {
        rfd::FileDialog::new().add_filter(IMPORT_KIND, &importable_extensions()[..])
    };
    add_filters(dialog).pick_file()
}

/// The STEP contents of an importable file named `file_name`: the bytes as they are for
/// STEP, cadmpeg's translation for another program's file. With a note when the
/// translation left things behind, for the status line. Slow for a big file: call it in a
/// job.
pub(crate) fn step_contents(
    file_name: &str,
    bytes: Vec<u8>,
    progress: &Progress,
) -> Result<(Vec<u8>, Option<String>), String> {
    if !is_foreign(Path::new(file_name)) {
        return Ok((bytes, None));
    }
    translate(file_name, &bytes, progress)
}

#[cfg(feature = "cadmpeg")]
fn translate(
    file_name: &str,
    bytes: &[u8],
    progress: &Progress,
) -> Result<(Vec<u8>, Option<String>), String> {
    progress.say(format!("Translating {file_name} to STEP (cadmpeg)"));
    let converted = oxyde_cadmpeg::to_step(bytes).map_err(|e| e.to_string())?;
    let note = (converted.losses > 0).then(|| {
        format!(
            "translated by cadmpeg, which could not carry over {} item(s) of the {} file",
            converted.losses, converted.format
        )
    });
    Ok((converted.step, note))
}

#[cfg(not(feature = "cadmpeg"))]
fn translate(
    _file_name: &str,
    _bytes: &[u8],
    _progress: &Progress,
) -> Result<(Vec<u8>, Option<String>), String> {
    unreachable!("no foreign format without cadmpeg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_files_are_importable_in_any_case() {
        assert!(is_importable(Path::new("a/Bracket.STEP")));
        assert!(is_importable(Path::new("Bracket.stp")));
        assert!(!is_foreign(Path::new("Bracket.step")));
        assert!(!is_importable(Path::new("Bracket.oxyprt")));
    }

    #[cfg(feature = "cadmpeg")]
    #[test]
    fn other_programs_files_go_through_cadmpeg() {
        assert!(is_foreign(Path::new("Bracket.SLDPRT")));
        assert!(is_foreign(Path::new("Housing.f3d")));
        assert!(is_importable(Path::new("Housing.CATPart")));
        assert!(importable_extensions().contains(&"ipt"));
    }

    #[cfg(feature = "cadmpeg")]
    #[test]
    fn a_file_named_like_another_programs_but_not_one_is_refused() {
        let progress = Progress::default();
        let bytes = b"Not a SolidWorks part.\n".repeat(64);
        let error = step_contents("Bracket.sldprt", bytes, &progress).unwrap_err();
        assert_eq!(error, oxyde_cadmpeg::ConvertError::Unrecognized.to_string());
    }

    #[test]
    fn a_step_file_is_passed_on_untouched() {
        let progress = Progress::default();
        let bytes = b"ISO-10303-21;".to_vec();
        let (step, note) = step_contents("Bracket.step", bytes.clone(), &progress).unwrap();
        assert_eq!(step, bytes);
        assert_eq!(note, None);
    }
}
