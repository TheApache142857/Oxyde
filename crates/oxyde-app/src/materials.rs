//! The user's material library on disk: `materials.json` next to the preferences
//! (`%APPDATA%\Oxyde\materials.json` on Windows), written the first time a material is saved.
//! Until then, and when the file cannot be read, the starter set is used.

use std::path::PathBuf;

use oxyde_model::MaterialLibrary;

/// Where the library file lives.
pub fn path() -> Option<PathBuf> {
    crate::prefs::path().map(|p| p.with_file_name("materials.json"))
}

/// The saved library; the starter set when there is no file yet. An unreadable file gives
/// the starter set and the reason, so that it is not silently overwritten.
pub fn load() -> (MaterialLibrary, Option<String>) {
    match path() {
        Some(p) if p.exists() => match oxyde_format::material_library::load_material_library(&p)
        {
            Ok(library) => (library, None),
            Err(e) => (
                MaterialLibrary::standard(),
                Some(format!("Could not read {}: {e}", p.display())),
            ),
        },
        _ => (MaterialLibrary::standard(), None),
    }
}

pub fn save(library: &MaterialLibrary) -> Result<PathBuf, String> {
    let path = path().ok_or("no configuration folder")?;
    oxyde_format::material_library::save_material_library(&path, library)
        .map_err(|e| e.to_string())?;
    Ok(path)
}
