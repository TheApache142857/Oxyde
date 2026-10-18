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
        assert!(text.contains("\"elastic_modulus\": 68900"));
        let back = load_material_library(&path).unwrap();
        assert_eq!(back, library);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mechanical_properties_are_optional_and_checked() {
        let dir = std::env::temp_dir().join(format!("oxyde-materials-mech-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("materials.json");
        std::fs::write(
            &path,
            br#"{ "format": "oxyde-materials", "format_version": 1, "materials": [
                { "name": "Old", "density": 1000, "appearance": { "color": [1, 2, 3] } },
                { "name": "Glass", "density": 2500, "appearance": { "color": [1, 2, 3] },
                  "mechanical": { "elastic_modulus": 72000, "poisson_ratio": 0.22, "tensile_strength": 40 } },
                { "name": "Odd", "density": 1, "appearance": { "color": [1, 2, 3] },
                  "mechanical": { "elastic_modulus": 1, "poisson_ratio": 0.7 } }
            ] }"#,
        )
        .unwrap();
        let library = load_material_library(&path).unwrap();
        assert!(library.get("Old").unwrap().mechanical.is_none());
        let glass = library.get("Glass").unwrap().mechanical.unwrap();
        assert_eq!((glass.yield_strength, glass.tensile_strength), (0.0, 40.0));
        // A Poisson's ratio no solid has: as if the file said nothing.
        assert!(library.get("Odd").unwrap().mechanical.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thermal_properties_are_optional_and_checked() {
        let dir = std::env::temp_dir().join(format!("oxyde-materials-heat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("materials.json");
        std::fs::write(
            &path,
            br#"{ "format": "oxyde-materials", "format_version": 1, "materials": [
                { "name": "Old", "density": 1000, "appearance": { "color": [1, 2, 3] } },
                { "name": "Copper", "density": 8940, "appearance": { "color": [1, 2, 3] },
                  "thermal": { "conductivity": 391, "specific_heat": 385, "expansion": 1.7e-5 } },
                { "name": "Some", "density": 1, "appearance": { "color": [1, 2, 3] },
                  "thermal": { "conductivity": 2.5 } },
                { "name": "Odd", "density": 1, "appearance": { "color": [1, 2, 3] },
                  "thermal": { "conductivity": -1 } }
            ] }"#,
        )
        .unwrap();
        let library = load_material_library(&path).unwrap();
        assert!(library.get("Old").unwrap().thermal.is_none());
        let copper = library.get("Copper").unwrap().thermal.unwrap();
        assert_eq!(
            (copper.conductivity, copper.specific_heat, copper.expansion),
            (391.0, 385.0, 1.7e-5)
        );
        // Only the conductivity is required; the rest is unknown.
        let some = library.get("Some").unwrap().thermal.unwrap();
        assert_eq!((some.specific_heat, some.expansion), (0.0, 0.0));
        // A conductivity no material has: as if the file said nothing.
        assert!(library.get("Odd").unwrap().thermal.is_none());
        // And back: written as read.
        let out = dir.join("again.json");
        save_material_library(&out, &library).unwrap();
        assert_eq!(load_material_library(&out).unwrap(), library);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_files_are_refused() {
        let dir = std::env::temp_dir().join(format!("oxyde-materials-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("materials.json");
        std::fs::write(&path, b"{\"format\": \"something\", \"format_version\": 1}").unwrap();
        assert!(matches!(
            load_material_library(&path),
            Err(FormatError::NotOxyde)
        ));
        std::fs::write(
            &path,
            b"{\"format\": \"oxyde-materials\", \"format_version\": 99}",
        )
        .unwrap();
        assert!(matches!(
            load_material_library(&path),
            Err(FormatError::UnsupportedVersion { found: 99, .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
