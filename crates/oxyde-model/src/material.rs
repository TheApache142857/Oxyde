//! Materials: a named appearance with the physical data of the stuff a part is made of, as
//! in SolidWorks' or Fusion's material libraries.
//!
//! A part has at most one material. Its appearance is what the part is drawn with where no
//! body or face appearance overrides it ([`crate::Appearances`] win over it), its density
//! gives the part's mass, and its name goes to the bill of materials. Materials come from a
//! library (the user's, saved next to the preferences) and are copied into the part when
//! applied, so a part file stays self-contained.

use crate::appearance::Appearance;

/// Where a material comes from, so that the ones Oxyde's library defines are told apart
/// from the ones an imported STEP file merely names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MaterialSource {
    /// Oxyde's material library: the starter set, or one the user made or edited.
    #[default]
    Library,
    /// Named by the STEP file a part was imported from (Fusion 360 and SolidWorks write
    /// the material of each body, with its density), as that file gave it.
    Step,
}

/// What a part is made of.
#[derive(Debug, Clone, PartialEq)]
pub struct Material {
    /// Unique within a library, e.g. `Aluminium 6061-T6`.
    pub name: String,
    /// Family, for grouping in the library: `Steel`, `Aluminium`, `Plastic`, `Wood`…
    pub category: String,
    /// How parts made of it look, unless an appearance overrides it.
    pub appearance: Appearance,
    /// Density in kg/m³; 0 when unknown (no mass can be computed from it).
    pub density: f64,
    /// Oxyde's library, or an imported STEP file.
    pub source: MaterialSource,
}

impl Material {
    /// The category imported STEP materials are filed under until the user changes it.
    pub const STEP_CATEGORY: &'static str = "Imported from STEP";

    pub fn new(name: &str, category: &str, appearance: Appearance, density: f64) -> Self {
        Self {
            name: name.to_owned(),
            category: category.to_owned(),
            appearance,
            density,
            source: MaterialSource::Library,
        }
        .clamped()
    }

    /// A material as a STEP file names it: `density` in kg/m³ (0 when the file gives
    /// none), the default look (the file's colours become appearances, which win over it).
    /// `None` when the name is unusable.
    pub fn from_step(name: &str, density: f64) -> Option<Self> {
        let name = name.trim();
        if !Self::is_valid_name(name) {
            return None;
        }
        Some(
            Self {
                name: name.to_owned(),
                category: Self::STEP_CATEGORY.to_owned(),
                appearance: Appearance::default(),
                density,
                source: MaterialSource::Step,
            }
            .clamped(),
        )
    }

    /// Whether the material was named by an imported STEP file rather than the library.
    pub fn is_from_step(&self) -> bool {
        self.source == MaterialSource::Step
    }

    /// Whether a mass can be computed from it: a known, positive density.
    pub fn has_density(&self) -> bool {
        self.density > 0.0
    }

    /// The density kept positive and finite, the appearance in range.
    pub fn clamped(mut self) -> Self {
        if !self.density.is_finite() || self.density < 0.0 {
            self.density = 0.0;
        }
        self.appearance = self.appearance.clamped();
        self
    }

    /// The mass, in kg, of `volume_mm3` cubic millimetres of it.
    pub fn mass_kg(&self, volume_mm3: f64) -> f64 {
        volume_mm3 * 1e-9 * self.density
    }

    /// Whether `name` can name a material: not empty, no surrounding spaces, no control
    /// characters.
    pub fn is_valid_name(name: &str) -> bool {
        !name.is_empty() && name.trim() == name && !name.chars().any(char::is_control)
    }
}

/// A set of materials with unique names, in display order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MaterialLibrary {
    materials: Vec<Material>,
}

impl MaterialLibrary {
    /// The starter library: common engineering materials with their densities and
    /// a plausible look. Densities are typical handbook values in kg/m³.
    pub fn standard() -> Self {
        let plain = |rgb: [u8; 3]| Appearance::colored(rgb);
        let glassy = |rgb: [u8; 3], transparency: f64| Appearance {
            color: rgb,
            transparency,
            texture: None,
        };
        let list = [
            ("Plain carbon steel (AISI 1020)", "Steel", plain([150, 152, 158]), 7870.0),
            ("Alloy steel (AISI 4140)", "Steel", plain([128, 130, 136]), 7850.0),
            ("Stainless steel (AISI 304)", "Steel", plain([188, 190, 195]), 8000.0),
            ("Cast iron (grey)", "Iron", plain([95, 97, 100]), 7200.0),
            ("Aluminium 6061-T6", "Aluminium", plain([200, 203, 208]), 2700.0),
            ("Aluminium 7075-T6", "Aluminium", plain([206, 208, 213]), 2810.0),
            ("Aluminium (cast, A356)", "Aluminium", plain([180, 183, 188]), 2670.0),
            ("Brass (C260)", "Copper alloy", plain([205, 170, 80]), 8530.0),
            ("Bronze (C905)", "Copper alloy", plain([170, 115, 70]), 8800.0),
            ("Copper (C110)", "Copper alloy", plain([190, 110, 80]), 8940.0),
            ("Titanium (Ti-6Al-4V)", "Titanium", plain([160, 160, 166]), 4430.0),
            ("ABS", "Plastic", plain([235, 235, 230]), 1040.0),
            ("PLA", "Plastic", plain([225, 228, 222]), 1240.0),
            ("Nylon 6/6", "Plastic", plain([228, 226, 212]), 1140.0),
            ("Polycarbonate", "Plastic", glassy([205, 225, 235], 0.35), 1200.0),
            ("Acrylic (PMMA)", "Plastic", glassy([215, 230, 240], 0.5), 1180.0),
            ("Black plastic (PA, filled)", "Plastic", plain([40, 40, 42]), 1150.0),
            ("Rubber (natural)", "Rubber", plain([50, 50, 50]), 930.0),
            ("Silicone rubber", "Rubber", plain([225, 215, 205]), 1150.0),
            ("Glass (soda-lime)", "Glass", glassy([180, 220, 235], 0.7), 2500.0),
            ("Oak", "Wood", plain([170, 120, 70]), 750.0),
            ("Pine", "Wood", plain([210, 175, 110]), 500.0),
            ("Plywood (birch)", "Wood", plain([200, 165, 105]), 680.0),
            ("PCB (FR-4)", "Composite", plain([30, 110, 60]), 1850.0),
            ("Carbon fibre (epoxy)", "Composite", plain([45, 45, 50]), 1600.0),
        ];
        let mut library = Self::default();
        for (name, category, appearance, density) in list {
            library.insert(Material::new(name, category, appearance, density));
        }
        library
    }

    pub fn materials(&self) -> &[Material] {
        &self.materials
    }

    pub fn is_empty(&self) -> bool {
        self.materials.is_empty()
    }

    pub fn len(&self) -> usize {
        self.materials.len()
    }

    pub fn get(&self, name: &str) -> Option<&Material> {
        self.materials.iter().find(|m| m.name == name)
    }

    /// Adds a material, or replaces the one of the same name (keeping its place).
    pub fn insert(&mut self, material: Material) {
        let material = material.clamped();
        match self.materials.iter_mut().find(|m| m.name == material.name) {
            Some(existing) => *existing = material,
            None => self.materials.push(material),
        }
    }

    /// Removes the material of that name; whether there was one.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.materials.len();
        self.materials.retain(|m| m.name != name);
        self.materials.len() != before
    }

    /// The categories in use, in order of first appearance.
    pub fn categories(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for m in &self.materials {
            if !out.contains(&m.category.as_str()) {
                out.push(&m.category);
            }
        }
        out
    }

    /// Replaces the whole list (names made unique by keeping the last of each).
    pub fn from_materials(materials: Vec<Material>) -> Self {
        let mut library = Self::default();
        for m in materials {
            library.insert(m);
        }
        library
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mass_follows_density_and_volume() {
        let steel = Material::new("Steel", "Steel", Appearance::default(), 7870.0);
        // A 10 × 10 × 10 mm cube: 1 cm³ of steel is 7.87 g.
        assert!((steel.mass_kg(1000.0) - 0.00787).abs() < 1e-12);
        let bad = Material::new("x", "", Appearance::default(), f64::NAN);
        assert_eq!(bad.density, 0.0);
    }

    #[test]
    fn library_keeps_names_unique_and_lists_categories() {
        let mut lib = MaterialLibrary::standard();
        let count = lib.len();
        assert!(lib.get("Aluminium 6061-T6").is_some());
        lib.insert(Material::new(
            "Aluminium 6061-T6",
            "Aluminium",
            Appearance::colored([1, 2, 3]),
            2.0,
        ));
        assert_eq!(lib.len(), count);
        assert_eq!(lib.get("Aluminium 6061-T6").unwrap().density, 2.0);
        assert!(lib.categories().contains(&"Plastic"));
        assert!(lib.remove("PLA"));
        assert!(!lib.remove("PLA"));
        assert!(Material::is_valid_name("Oak"));
        assert!(!Material::is_valid_name(" Oak"));
        assert!(!Material::is_valid_name(""));
    }

    #[test]
    fn step_materials_are_told_apart_from_the_library() {
        let steel = Material::from_step(" Acier ", 7850.0).unwrap();
        assert_eq!(steel.name, "Acier");
        assert!(steel.is_from_step() && steel.has_density());
        assert_eq!(steel.category, Material::STEP_CATEGORY);
        let unknown = Material::from_step("Eau", 0.0).unwrap();
        assert!(!unknown.has_density());
        assert!(Material::from_step("  ", 1.0).is_none());
        let library = Material::new("Oak", "Wood", Appearance::default(), 750.0);
        assert!(!library.is_from_step());
        // Both can live in one library, told apart by their source.
        let mut lib = MaterialLibrary::default();
        lib.insert(steel.clone());
        lib.insert(library);
        assert!(lib.get("Acier").unwrap().is_from_step());
        assert!(!lib.get("Oak").unwrap().is_from_step());
    }
}
