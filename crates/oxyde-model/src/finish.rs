//! Finishes: how a surface reflects light in a render (matte or glossy plastic, brushed
//! or polished metal, glass, rubber), on top of the colour an [`Appearance`] gives it.
//!
//! The viewport ignores them; the path tracer and the glTF export use them. A finish is
//! not stored in the part: it is guessed from the part's material (its category and its
//! name, so that a STEP file's `Acier` is steel too) and from how see-through the
//! appearance is, and the user can choose another one per material in the application's
//! preferences. The parameters are those of the metallic-roughness model glTF and
//! Blender's Principled BSDF use, so both routes render the same thing.

use crate::appearance::Appearance;
use crate::material::Material;

/// A kind of surface, from matte to mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Finish {
    /// Plastic or paint with no shine: wood, cast parts, 3D prints.
    Matte,
    /// A soft sheen: most moulded plastics, composites.
    Satin,
    /// Glossy plastic or lacquer.
    Glossy,
    /// Machined or brushed metal: blurred reflections.
    BrushedMetal,
    /// Polished or chromed metal: sharp reflections.
    PolishedMetal,
    /// Clear glass or plastic: light goes through and bends.
    Glass,
    /// Rubber: dark and dull.
    Rubber,
}

/// The parameters of a finish in the metallic-roughness model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FinishParams {
    /// 0 = a dielectric (plastic, paint, glass), 1 = a metal, whose reflections take
    /// its colour.
    pub metallic: f64,
    /// 0 = mirror-smooth, 1 = fully diffuse reflections.
    pub roughness: f64,
    /// 0 = opaque, 1 = light goes through (glass).
    pub transmission: f64,
    /// Index of refraction, for transmission and for the reflections of dielectrics.
    pub ior: f64,
}

impl Finish {
    /// Every finish, in the order a list shows them, with its label.
    pub const ALL: [(Finish, &'static str); 7] = [
        (Finish::Matte, "Matte"),
        (Finish::Satin, "Satin"),
        (Finish::Glossy, "Glossy"),
        (Finish::BrushedMetal, "Brushed metal"),
        (Finish::PolishedMetal, "Polished metal"),
        (Finish::Glass, "Glass"),
        (Finish::Rubber, "Rubber"),
    ];

    pub fn label(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(f, _)| *f == self)
            .map_or("Satin", |(_, label)| label)
    }

    /// A stable name for settings files: `brushed_metal`.
    pub fn id(self) -> &'static str {
        match self {
            Finish::Matte => "matte",
            Finish::Satin => "satin",
            Finish::Glossy => "glossy",
            Finish::BrushedMetal => "brushed_metal",
            Finish::PolishedMetal => "polished_metal",
            Finish::Glass => "glass",
            Finish::Rubber => "rubber",
        }
    }

    /// The finish an [`Finish::id`] names.
    pub fn from_id(id: &str) -> Option<Finish> {
        Self::ALL.iter().map(|(f, _)| *f).find(|f| f.id() == id)
    }

    pub fn params(self) -> FinishParams {
        let (metallic, roughness, transmission) = match self {
            Finish::Matte => (0.0, 0.75, 0.0),
            Finish::Satin => (0.0, 0.42, 0.0),
            Finish::Glossy => (0.0, 0.12, 0.0),
            Finish::BrushedMetal => (1.0, 0.32, 0.0),
            Finish::PolishedMetal => (1.0, 0.06, 0.0),
            Finish::Glass => (0.0, 0.0, 1.0),
            Finish::Rubber => (0.0, 0.9, 0.0),
        };
        FinishParams {
            metallic,
            roughness,
            transmission,
            ior: 1.5,
        }
    }

    /// The colour to render a surface of colour `color` (sRGB) with in this finish: the
    /// colour itself, except for glass, whose appearance colour is chosen to look right
    /// half see-through in the viewport and would tint clear glass far too strongly, so
    /// it is taken much lighter.
    pub fn render_color(self, color: [u8; 3]) -> [u8; 3] {
        if self == Finish::Glass {
            color.map(|c| 255 - ((255 - u16::from(c)) * 2 / 5) as u8)
        } else {
            color
        }
    }

    pub fn is_metal(self) -> bool {
        matches!(self, Finish::BrushedMetal | Finish::PolishedMetal)
    }

    /// The finish a material suggests, from its category and name: metals are brushed
    /// (polished when the name says chrome, polished or mirror), plastics satin, wood
    /// matte, rubber rubber, glass glass; `None` when nothing matches.
    pub fn of_material(material: &Material) -> Option<Finish> {
        let text = format!("{} {}", material.category, material.name).to_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
        if has(&["glass", "verre", "glas "]) {
            return Some(Finish::Glass);
        }
        if has(&["rubber", "caoutchouc", "gummi", "elastomer", "silicone"]) {
            return Some(Finish::Rubber);
        }
        if has(&[
            "steel",
            "acier",
            "stahl",
            "inox",
            "iron",
            "fonte",
            "alumin",
            "copper",
            "cuivre",
            "brass",
            "laiton",
            "bronze",
            "titan",
            "metal",
            "métal",
            "zinc",
            "nickel",
            "chrome",
            "magnesium",
        ]) {
            return Some(if has(&["chrome", "polished", "poli", "mirror"]) {
                Finish::PolishedMetal
            } else {
                Finish::BrushedMetal
            });
        }
        if has(&["wood", "bois", "holz", "oak", "pine", "plywood", "chêne"]) {
            return Some(Finish::Matte);
        }
        if has(&[
            "plastic",
            "plastique",
            "kunststoff",
            "abs",
            "pla",
            "nylon",
            "pa ",
            "polyam",
            "polycarb",
            "pmma",
            "acryl",
            "composite",
            "carbon",
            "fr-4",
            "epoxy",
            "pom",
            "delrin",
            "pet",
            "pp ",
            "polyprop",
            "polyeth",
        ]) {
            return Some(Finish::Satin);
        }
        None
    }

    /// The finish to render a surface with when the user chose none: glass for a
    /// see-through appearance (a third or more), else the material's, else satin.
    pub fn guess(material: Option<&Material>, appearance: Option<&Appearance>) -> Finish {
        if appearance.is_some_and(|a| a.transparency >= 0.3) {
            return Finish::Glass;
        }
        material
            .and_then(Finish::of_material)
            .unwrap_or(Finish::Satin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(name: &str, category: &str) -> Material {
        Material::new(name, category, Appearance::default(), 1000.0)
    }

    #[test]
    fn the_starter_library_gets_plausible_finishes() {
        let library = crate::MaterialLibrary::standard();
        for m in library.materials() {
            let finish = Finish::guess(Some(m), Some(&m.appearance));
            let expected = match m.category.as_str() {
                "Steel" | "Iron" | "Aluminium" | "Copper alloy" | "Titanium" => {
                    Finish::BrushedMetal
                }
                "Rubber" => Finish::Rubber,
                "Wood" => Finish::Matte,
                "Glass" => Finish::Glass,
                // Clear plastics are glass-like; the others satin.
                _ if m.appearance.transparency >= 0.3 => Finish::Glass,
                _ => Finish::Satin,
            };
            assert_eq!(finish, expected, "{}", m.name);
        }
    }

    #[test]
    fn step_names_and_defaults() {
        let acier = material("Acier", Material::STEP_CATEGORY);
        assert_eq!(Finish::of_material(&acier), Some(Finish::BrushedMetal));
        let chrome = material("Chrome-plated steel", "Steel");
        assert_eq!(Finish::of_material(&chrome), Some(Finish::PolishedMetal));
        let unknown = material("Unobtainium", "");
        assert_eq!(Finish::of_material(&unknown), None);
        assert_eq!(Finish::guess(Some(&unknown), None), Finish::Satin);
        assert_eq!(Finish::guess(None, None), Finish::Satin);
        let clear = Appearance {
            transparency: 0.5,
            ..Appearance::default()
        };
        assert_eq!(Finish::guess(Some(&acier), Some(&clear)), Finish::Glass);
    }

    #[test]
    fn ids_round_trip_and_params_are_in_range() {
        for (finish, label) in Finish::ALL {
            assert_eq!(Finish::from_id(finish.id()), Some(finish));
            assert_eq!(finish.label(), label);
            let p = finish.params();
            for v in [p.metallic, p.roughness, p.transmission] {
                assert!((0.0..=1.0).contains(&v));
            }
            assert_eq!(finish.is_metal(), p.metallic > 0.5);
        }
        assert_eq!(Finish::from_id("velvet"), None);
        assert_eq!(Finish::Satin.render_color([10, 20, 30]), [10, 20, 30]);
        assert!(Finish::Glass.render_color([0, 0, 0])[0] > 150);
    }
}
