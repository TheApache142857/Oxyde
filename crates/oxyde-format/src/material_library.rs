//! The material library: the user's materials, saved as `materials.json` next to the
//! preferences (`%APPDATA%\Oxyde\materials.json` on Windows), with any texture images in a
//! `textures/` folder beside it.
//!
//! The file is plain JSON so it can be edited or shared by hand:
//!
//! ```json
//! {
//!   "format": "oxyde-materials",
//!   "format_version": 1,
//!   "materials": [
//!     { "name": "Aluminium 6061-T6", "category": "Aluminium", "density": 2700,
//!       "appearance": { "color": [200, 203, 208] } }
//!   ]
//! }
//! ```
//!
//! A material's `appearance` has the same shape as in a part's `model.json` (FORMAT.md §3),
//! with `texture.file` relative to the library file's folder. Parts never refer to the
//! library: a material applied to a part is copied into it.

use std::collections::HashMap;
use std::path::Path;

use oxyde_model::MaterialLibrary;
use serde::{Deserialize, Serialize};

use crate::schema::{MaterialDto, TextureTable, material_dto, material_from};
use crate::{FormatError, from_json, to_json};

const LIBRARY: &str = "materials.json";
const FORMAT_NAME: &str = "oxyde-materials";
/// The `format_version` written, and the newest read.
pub const LIBRARY_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LibraryFile {
    format: String,
    format_version: u32,
    #[serde(default)]
    materials: Vec<MaterialDto>,
}

/// Reads a material library; texture images are read from the file's folder.
pub fn load_material_library(path: &Path) -> Result<MaterialLibrary, FormatError> {
    let bytes = std::fs::read(path)?;
    let file: LibraryFile = from_json(LIBRARY, &bytes)?;
    if file.format != FORMAT_NAME {
        return Err(FormatError::NotOxyde);
    }
    if file.format_version > LIBRARY_VERSION {
        return Err(FormatError::UnsupportedVersion {
            found: file.format_version,
            supported: LIBRARY_VERSION,
        });
    }
    let folder = path.parent().unwrap_or_else(|| Path::new(""));
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    for m in &file.materials {
        if let Some(t) = &m.appearance.texture {
            if !files.contains_key(&t.file) {
                if let Ok(bytes) = std::fs::read(folder.join(&t.file)) {
                    files.insert(t.file.clone(), bytes);
                }
            }
        }
    }
    let mut materials = Vec::with_capacity(file.materials.len());
    for mut dto in file.materials {
        // An image that is gone leaves the material plain rather than losing it.
        if dto
            .appearance
            .texture
            .as_ref()
            .is_some_and(|t| !files.contains_key(&t.file))
        {
            dto.appearance.texture = None;
        }
        materials.push(material_from(dto, &files).map_err(FormatError::InvalidLibrary)?);
    }
    Ok(MaterialLibrary::from_materials(materials))
}

/// Writes a material library to `path` (its folder is created), with texture images in
/// `textures/` beside it.
pub fn save_material_library(path: &Path, library: &MaterialLibrary) -> Result<(), FormatError> {
    let textures = TextureTable::new(
        library
            .materials()
            .iter()
            .filter_map(|m| m.appearance.texture.as_ref()),
    );
    let file = LibraryFile {
        format: FORMAT_NAME.to_owned(),
        format_version: LIBRARY_VERSION,
        materials: library
            .materials()
            .iter()
            .map(|m| material_dto(m, &textures))
            .collect(),
    };
    let bytes = to_json(LIBRARY, &file)?;
    let folder = path.parent().unwrap_or_else(|| Path::new(""));
    if !folder.as_os_str().is_empty() {
        std::fs::create_dir_all(folder)?;
    }
    for (name, image) in textures.entries() {
        let target = folder.join(name);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(target, image)?;
    }
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxyde_model::{Appearance, FileData, Material, Texture};

    #[test]
    fn library_round_trips_with_textures() {
        let dir = std::env::temp_dir().join(format!("oxyde-materials-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("materials.json");
        let mut library = MaterialLibrary::standard();
        library.insert(Material::new(
            "Oak (textured)",
            "Wood",
            Appearance {
                color: [170, 120, 70],
                transparency: 0.0,
                texture: Some(Texture {
                    file_name: "oak.png".into(),
                    data: FileData::new(vec![0x89, b'P', b'N', b'G']),
                    scale: 80.0,
                }),
            },
            750.0,
        ));
        save_material_library(&path, &library).unwrap();
        assert!(dir.join("textures").join("1.png").exists());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"format\": \"oxyde-materials\""));
        assert!(text.contains("\"density\": 2700"));
        let back = load_material_library(&path).unwrap();
        assert_eq!(back, library);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_files_are_refused() {
        let dir = std::env::temp_dir().join(format!("oxyde-materials-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("materials.json");
        std::fs::write(&path, b"{\"format\": \"something\", \"format_version\": 1}").unwrap();
        assert!(matches!(load_material_library(&path), Err(FormatError::NotOxyde)));
        std::fs::write(&path, b"{\"format\": \"oxyde-materials\", \"format_version\": 99}").unwrap();
        assert!(matches!(
            load_material_library(&path),
            Err(FormatError::UnsupportedVersion { found: 99, .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
