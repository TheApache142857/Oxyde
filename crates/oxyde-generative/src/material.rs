//! Elastic and thermal properties of a material, and guesses of them from the material's
//! name.
//!
//! Oxyde's materials may carry their mechanical and thermal properties; for those that do
//! not (older files, materials named by STEP files), the Simulation tab starts from
//! [`Elastic::guess`] and [`ThermalProps::guess`]: the starter library's materials by
//! name, then families by keyword, with typical handbook values. The user can type others
//! in.

/// An isotropic linear elastic material.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Elastic {
    /// Young's modulus, MPa (N/mm²).
    pub youngs_modulus: f64,
    /// Poisson's ratio, between −1 and 0.5 (exclusive).
    pub poisson: f64,
    /// Yield strength, MPa; 0 when unknown (no factor of safety then). For brittle
    /// materials, the tensile strength.
    pub yield_strength: f64,
    /// kg/m³, for the weight; 0 when unknown.
    pub density: f64,
    /// Coefficient of linear thermal expansion, 1/K, for a thermal stress study; 0 when
    /// unknown (no expansion).
    pub expansion: f64,
}

impl Elastic {
    pub const fn new(youngs_modulus: f64, poisson: f64, yield_strength: f64, density: f64) -> Self {
        Self {
            youngs_modulus,
            poisson,
            yield_strength,
            density,
            expansion: 0.0,
        }
    }

    /// The same material expanding by `expansion` per kelvin.
    pub const fn with_expansion(mut self, expansion: f64) -> Self {
        self.expansion = expansion;
        self
    }

    /// Whether the solver can use it: a positive modulus and a Poisson's ratio that keeps
    /// the material stable.
    pub fn is_valid(&self) -> bool {
        self.youngs_modulus.is_finite()
            && self.youngs_modulus > 0.0
            && self.poisson.is_finite()
            && self.poisson > -0.999
            && self.poisson < 0.4999
            && self.yield_strength.is_finite()
            && self.yield_strength >= 0.0
            && self.density.is_finite()
            && self.density >= 0.0
            && self.expansion.is_finite()
    }

    /// Lamé's λ and μ (the shear modulus), MPa.
    pub fn lame(&self) -> (f64, f64) {
        let (e, nu) = (self.youngs_modulus, self.poisson);
        let lambda = e * nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
        let mu = e / (2.0 * (1.0 + nu));
        (lambda, mu)
    }

    /// Typical properties for a material called `name` in `category`: the starter
    /// library's materials by name, else by family keywords (in English, French and
    /// German, as `oxyde_model::Finish` guesses looks). `None`
    /// when nothing matches. The density is left at 0: the material's own is used.
    pub fn guess(name: &str, category: &str) -> Option<Elastic> {
        guess_from(name, category, NAMED, FAMILIES)
    }
}

/// How a material conducts, stores and expands with heat: conductivity in W/(m·K),
/// specific heat in J/(kg·K), linear expansion in 1/K.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermalProps {
    pub conductivity: f64,
    pub specific_heat: f64,
    pub expansion: f64,
}

impl ThermalProps {
    pub const fn new(conductivity: f64, specific_heat: f64, expansion: f64) -> Self {
        Self {
            conductivity,
            specific_heat,
            expansion,
        }
    }

    /// Typical properties for a material called `name` in `category`, found as
    /// [`Elastic::guess`] finds them. `None` when nothing matches.
    pub fn guess(name: &str, category: &str) -> Option<ThermalProps> {
        guess_from(name, category, NAMED_THERMAL, THERMAL_FAMILIES)
    }
}

/// `named` by exact name (ignoring case), else the first of `families` with a keyword in
/// the category or the name. Short keywords (`pla`, `pet`, `abs`) are whole words, so that
/// "plastic" is not PLA; longer ones, and ones with a sign (`fr-4`), match anywhere.
fn guess_from<T: Copy>(
    name: &str,
    category: &str,
    named: &[(&str, T)],
    families: &[(&[&str], T)],
) -> Option<T> {
    if let Some((_, e)) = named
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name.trim()))
    {
        return Some(*e);
    }
    let text = format!("{category} {name}").to_lowercase();
    let tokens: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let matches = |w: &&str| {
        if w.chars().count() <= 4 && w.chars().all(char::is_alphanumeric) {
            tokens.contains(w)
        } else {
            text.contains(*w)
        }
    };
    families
        .iter()
        .find(|(words, _)| words.iter().any(matches))
        .map(|(_, e)| *e)
}

/// The starter library (`oxyde_model::MaterialLibrary::standard`): E (MPa), ν, yield or
/// tensile strength (MPa). Annealed or common tempers, as SolidWorks' library lists them.
const NAMED: &[(&str, Elastic)] = &[
    (
        "Plain carbon steel (AISI 1020)",
        Elastic::new(200_000.0, 0.29, 351.6, 0.0),
    ),
    (
        "Alloy steel (AISI 4140)",
        Elastic::new(205_000.0, 0.29, 415.0, 0.0),
    ),
    (
        "Stainless steel (AISI 304)",
        Elastic::new(193_000.0, 0.29, 215.0, 0.0),
    ),
    (
        "Cast iron (grey)",
        Elastic::new(100_000.0, 0.26, 150.0, 0.0),
    ),
    (
        "Aluminium 6061-T6",
        Elastic::new(68_900.0, 0.33, 275.0, 0.0),
    ),
    (
        "Aluminium 7075-T6",
        Elastic::new(71_700.0, 0.33, 503.0, 0.0),
    ),
    (
        "Aluminium (cast, A356)",
        Elastic::new(72_400.0, 0.33, 165.0, 0.0),
    ),
    ("Brass (C260)", Elastic::new(110_000.0, 0.34, 200.0, 0.0)),
    ("Bronze (C905)", Elastic::new(103_000.0, 0.34, 152.0, 0.0)),
    ("Copper (C110)", Elastic::new(115_000.0, 0.31, 70.0, 0.0)),
    (
        "Titanium (Ti-6Al-4V)",
        Elastic::new(113_800.0, 0.342, 880.0, 0.0),
    ),
    ("ABS", Elastic::new(2_200.0, 0.35, 40.0, 0.0)),
    ("PLA", Elastic::new(3_500.0, 0.36, 50.0, 0.0)),
    ("Nylon 6/6", Elastic::new(2_500.0, 0.39, 60.0, 0.0)),
    ("Polycarbonate", Elastic::new(2_380.0, 0.37, 62.0, 0.0)),
    ("Acrylic (PMMA)", Elastic::new(3_000.0, 0.37, 70.0, 0.0)),
    (
        "Black plastic (PA, filled)",
        Elastic::new(6_000.0, 0.35, 100.0, 0.0),
    ),
    ("Rubber (natural)", Elastic::new(2.0, 0.49, 20.0, 0.0)),
    ("Silicone rubber", Elastic::new(3.0, 0.49, 7.0, 0.0)),
    ("Glass (soda-lime)", Elastic::new(72_000.0, 0.22, 40.0, 0.0)),
    ("Oak", Elastic::new(11_000.0, 0.35, 40.0, 0.0)),
    ("Pine", Elastic::new(9_000.0, 0.35, 35.0, 0.0)),
    ("Plywood (birch)", Elastic::new(8_000.0, 0.3, 30.0, 0.0)),
    ("PCB (FR-4)", Elastic::new(22_000.0, 0.15, 250.0, 0.0)),
    (
        "Carbon fibre (epoxy)",
        Elastic::new(60_000.0, 0.3, 500.0, 0.0),
    ),
];

/// Families by keyword, most specific first.
const FAMILIES: &[(&[&str], Elastic)] = &[
    (
        &["stainless", "inox", "inoxydable", "edelstahl", "rostfrei"],
        Elastic::new(193_000.0, 0.29, 215.0, 0.0),
    ),
    (
        &["cast iron", "fonte", "gusseisen", "grey iron", "gray iron"],
        Elastic::new(100_000.0, 0.26, 150.0, 0.0),
    ),
    (
        &["steel", "acier", "stahl"],
        Elastic::new(200_000.0, 0.29, 351.6, 0.0),
    ),
    (
        &["iron", "eisen"],
        Elastic::new(100_000.0, 0.26, 150.0, 0.0),
    ),
    (&["titan"], Elastic::new(113_800.0, 0.342, 880.0, 0.0)),
    (&["7075"], Elastic::new(71_700.0, 0.33, 503.0, 0.0)),
    (&["alumin", "alu"], Elastic::new(68_900.0, 0.33, 275.0, 0.0)),
    (&["magnesium"], Elastic::new(45_000.0, 0.35, 200.0, 0.0)),
    (
        &["brass", "laiton", "messing"],
        Elastic::new(110_000.0, 0.34, 200.0, 0.0),
    ),
    (&["bronze"], Elastic::new(103_000.0, 0.34, 152.0, 0.0)),
    (
        &["copper", "cuivre", "kupfer"],
        Elastic::new(115_000.0, 0.31, 70.0, 0.0),
    ),
    (&["zinc", "zamak"], Elastic::new(96_000.0, 0.27, 220.0, 0.0)),
    (&["silicone"], Elastic::new(3.0, 0.49, 7.0, 0.0)),
    (
        &["rubber", "caoutchouc", "gummi", "elastomer"],
        Elastic::new(2.0, 0.49, 20.0, 0.0),
    ),
    (
        &["glass", "verre", "glas"],
        Elastic::new(72_000.0, 0.22, 40.0, 0.0),
    ),
    (
        &["carbon", "cfrp", "carbone"],
        Elastic::new(60_000.0, 0.3, 500.0, 0.0),
    ),
    (
        &["fr-4", "fr4", "pcb", "epoxy"],
        Elastic::new(22_000.0, 0.15, 250.0, 0.0),
    ),
    (&["polycarbonate"], Elastic::new(2_380.0, 0.37, 62.0, 0.0)),
    (
        &["acrylic", "pmma", "plexi"],
        Elastic::new(3_000.0, 0.37, 70.0, 0.0),
    ),
    (
        &["nylon", "polyamide", "pa6", "pa12"],
        Elastic::new(2_500.0, 0.39, 60.0, 0.0),
    ),
    (&["pla"], Elastic::new(3_500.0, 0.36, 50.0, 0.0)),
    (&["petg", "pet"], Elastic::new(2_100.0, 0.38, 50.0, 0.0)),
    (
        &["polypropylene", "pp"],
        Elastic::new(1_500.0, 0.42, 30.0, 0.0),
    ),
    (
        &["polyethylene", "hdpe", "pe"],
        Elastic::new(1_000.0, 0.42, 25.0, 0.0),
    ),
    (
        &["pom", "acetal", "delrin"],
        Elastic::new(2_900.0, 0.35, 65.0, 0.0),
    ),
    (
        &["abs", "plastic", "plastique", "kunststoff", "polymer"],
        Elastic::new(2_200.0, 0.35, 40.0, 0.0),
    ),
    (
        &["oak", "chêne", "chene", "eiche"],
        Elastic::new(11_000.0, 0.35, 40.0, 0.0),
    ),
    (
        &["plywood", "contreplaqué", "sperrholz"],
        Elastic::new(8_000.0, 0.3, 30.0, 0.0),
    ),
    (
        &["wood", "bois", "holz", "pine", "pin"],
        Elastic::new(9_000.0, 0.35, 35.0, 0.0),
    ),
];

/// The starter library's thermal properties (`oxyde_model::MaterialLibrary::standard`).
const NAMED_THERMAL: &[(&str, ThermalProps)] = &[
    (
        "Plain carbon steel (AISI 1020)",
        ThermalProps::new(51.9, 486.0, 11.7e-6),
    ),
    (
        "Alloy steel (AISI 4140)",
        ThermalProps::new(42.6, 473.0, 12.3e-6),
    ),
    (
        "Stainless steel (AISI 304)",
        ThermalProps::new(16.2, 500.0, 17.3e-6),
    ),
    ("Cast iron (grey)", ThermalProps::new(46.0, 490.0, 10.8e-6)),
    (
        "Aluminium 6061-T6",
        ThermalProps::new(167.0, 896.0, 23.6e-6),
    ),
    (
        "Aluminium 7075-T6",
        ThermalProps::new(130.0, 960.0, 23.4e-6),
    ),
    (
        "Aluminium (cast, A356)",
        ThermalProps::new(151.0, 963.0, 21.4e-6),
    ),
    ("Brass (C260)", ThermalProps::new(120.0, 375.0, 19.9e-6)),
    ("Bronze (C905)", ThermalProps::new(75.0, 376.0, 18.0e-6)),
    ("Copper (C110)", ThermalProps::new(391.0, 385.0, 17.0e-6)),
    (
        "Titanium (Ti-6Al-4V)",
        ThermalProps::new(6.7, 526.0, 8.6e-6),
    ),
    ("ABS", ThermalProps::new(0.17, 1400.0, 90.0e-6)),
    ("PLA", ThermalProps::new(0.13, 1800.0, 68.0e-6)),
    ("Nylon 6/6", ThermalProps::new(0.25, 1670.0, 80.0e-6)),
    ("Polycarbonate", ThermalProps::new(0.20, 1200.0, 65.0e-6)),
    ("Acrylic (PMMA)", ThermalProps::new(0.19, 1470.0, 70.0e-6)),
    (
        "Black plastic (PA, filled)",
        ThermalProps::new(0.35, 1500.0, 30.0e-6),
    ),
    (
        "Rubber (natural)",
        ThermalProps::new(0.13, 1900.0, 220.0e-6),
    ),
    ("Silicone rubber", ThermalProps::new(0.22, 1250.0, 250.0e-6)),
    ("Glass (soda-lime)", ThermalProps::new(1.0, 840.0, 9.0e-6)),
    ("Oak", ThermalProps::new(0.17, 2380.0, 5.0e-6)),
    ("Pine", ThermalProps::new(0.12, 2300.0, 5.0e-6)),
    ("Plywood (birch)", ThermalProps::new(0.13, 1800.0, 6.0e-6)),
    ("PCB (FR-4)", ThermalProps::new(0.29, 1100.0, 14.0e-6)),
    (
        "Carbon fibre (epoxy)",
        ThermalProps::new(5.0, 1000.0, 2.0e-6),
    ),
];

/// Thermal families by keyword, most specific first, as [`FAMILIES`].
const THERMAL_FAMILIES: &[(&[&str], ThermalProps)] = &[
    (
        &["stainless", "inox", "inoxydable", "edelstahl", "rostfrei"],
        ThermalProps::new(16.2, 500.0, 17.3e-6),
    ),
    (
        &["cast iron", "fonte", "gusseisen", "grey iron", "gray iron"],
        ThermalProps::new(46.0, 490.0, 10.8e-6),
    ),
    (
        &["steel", "acier", "stahl"],
        ThermalProps::new(51.9, 486.0, 11.7e-6),
    ),
    (&["iron", "eisen"], ThermalProps::new(46.0, 490.0, 10.8e-6)),
    (&["titan"], ThermalProps::new(6.7, 526.0, 8.6e-6)),
    (&["7075"], ThermalProps::new(130.0, 960.0, 23.4e-6)),
    (&["alumin", "alu"], ThermalProps::new(167.0, 896.0, 23.6e-6)),
    (&["magnesium"], ThermalProps::new(96.0, 1020.0, 26.0e-6)),
    (
        &["brass", "laiton", "messing"],
        ThermalProps::new(120.0, 375.0, 19.9e-6),
    ),
    (&["bronze"], ThermalProps::new(75.0, 376.0, 18.0e-6)),
    (
        &["copper", "cuivre", "kupfer"],
        ThermalProps::new(391.0, 385.0, 17.0e-6),
    ),
    (&["zinc", "zamak"], ThermalProps::new(113.0, 419.0, 27.4e-6)),
    (&["silicone"], ThermalProps::new(0.22, 1250.0, 250.0e-6)),
    (
        &["rubber", "caoutchouc", "gummi", "elastomer"],
        ThermalProps::new(0.13, 1900.0, 220.0e-6),
    ),
    (
        &["glass", "verre", "glas"],
        ThermalProps::new(1.0, 840.0, 9.0e-6),
    ),
    (
        &["carbon", "cfrp", "carbone"],
        ThermalProps::new(5.0, 1000.0, 2.0e-6),
    ),
    (
        &["fr-4", "fr4", "pcb", "epoxy"],
        ThermalProps::new(0.29, 1100.0, 14.0e-6),
    ),
    (&["polycarbonate"], ThermalProps::new(0.20, 1200.0, 65.0e-6)),
    (
        &["acrylic", "pmma", "plexi"],
        ThermalProps::new(0.19, 1470.0, 70.0e-6),
    ),
    (
        &["nylon", "polyamide", "pa6", "pa12"],
        ThermalProps::new(0.25, 1670.0, 80.0e-6),
    ),
    (&["pla"], ThermalProps::new(0.13, 1800.0, 68.0e-6)),
    (&["petg", "pet"], ThermalProps::new(0.2, 1200.0, 60.0e-6)),
    (
        &["polypropylene", "pp"],
        ThermalProps::new(0.22, 1900.0, 100.0e-6),
    ),
    (
        &["polyethylene", "hdpe", "pe"],
        ThermalProps::new(0.45, 1900.0, 120.0e-6),
    ),
    (
        &["pom", "acetal", "delrin"],
        ThermalProps::new(0.31, 1460.0, 110.0e-6),
    ),
    (
        &["abs", "plastic", "plastique", "kunststoff", "polymer"],
        ThermalProps::new(0.17, 1400.0, 90.0e-6),
    ),
    (
        &["oak", "chêne", "chene", "eiche"],
        ThermalProps::new(0.17, 2380.0, 5.0e-6),
    ),
    (
        &["plywood", "contreplaqué", "sperrholz"],
        ThermalProps::new(0.13, 1800.0, 6.0e-6),
    ),
    (
        &["wood", "bois", "holz", "pine", "pin"],
        ThermalProps::new(0.12, 2300.0, 5.0e-6),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thermal_guesses_by_name_then_family() {
        let copper = ThermalProps::guess("Copper (C110)", "Copper alloy").unwrap();
        assert_eq!(copper.conductivity, 391.0);
        let inox = ThermalProps::guess("Acier inoxydable", "Imported from STEP").unwrap();
        assert_eq!(inox.conductivity, 16.2);
        let al = ThermalProps::guess("AlMg3", "Aluminium").unwrap();
        assert_eq!(al.expansion, 23.6e-6);
        assert!(ThermalProps::guess("Unobtainium", "Imported from STEP").is_none());
        // Every material the elastic tables know, the thermal ones know too.
        for (name, _) in NAMED {
            assert!(NAMED_THERMAL.iter().any(|(n, _)| n == name), "{name}");
        }
        assert_eq!(FAMILIES.len(), THERMAL_FAMILIES.len());
        for ((a, _), (b, _)) in FAMILIES.iter().zip(THERMAL_FAMILIES) {
            assert_eq!(a, b);
        }
        for t in NAMED_THERMAL
            .iter()
            .map(|(_, t)| t)
            .chain(THERMAL_FAMILIES.iter().map(|(_, t)| t))
        {
            assert!(t.conductivity > 0.0 && t.specific_heat > 0.0 && t.expansion > 0.0);
        }
    }

    #[test]
    fn guesses_by_name_then_family() {
        let s = Elastic::guess("Plain carbon steel (AISI 1020)", "Steel").unwrap();
        assert_eq!(s.youngs_modulus, 200_000.0);
        let a = Elastic::guess("Aluminium 6061-T6", "Aluminium").unwrap();
        assert!((a.youngs_modulus - 68_900.0).abs() < 1.0);
        // A STEP material name.
        let inox = Elastic::guess("Acier inoxydable", "Imported from STEP").unwrap();
        assert_eq!(inox.youngs_modulus, 193_000.0);
        let al = Elastic::guess("AlMg3", "Aluminium").unwrap();
        assert_eq!(al.yield_strength, 275.0);
        assert!(Elastic::guess("Unobtainium", "Imported from STEP").is_none());
        // Short keywords are words: a plastic is not PLA, a 7075 alloy is found.
        let plastic = Elastic::guess("Grey plastic", "Plastic").unwrap();
        assert_eq!(plastic.youngs_modulus, 2_200.0);
        let pla = Elastic::guess("PLA", "Imported from STEP").unwrap();
        assert_eq!(pla.youngs_modulus, 3_500.0);
        let zicral = Elastic::guess("7075-T651", "Imported from STEP").unwrap();
        assert_eq!(zicral.yield_strength, 503.0);
        let fr4 = Elastic::guess("FR-4 board", "Imported from STEP").unwrap();
        assert_eq!(fr4.poisson, 0.15);
        for (_, e) in NAMED {
            assert!(e.is_valid());
        }
        for (_, e) in FAMILIES {
            assert!(e.is_valid());
        }
    }

    #[test]
    fn lame_constants() {
        let e = Elastic::new(210_000.0, 0.3, 250.0, 7850.0);
        let (l, m) = e.lame();
        assert!((m - 80_769.23).abs() < 0.01);
        assert!((l - 121_153.85).abs() < 0.01);
        assert!(!Elastic::new(1.0, 0.5, 0.0, 0.0).is_valid());
    }
}
