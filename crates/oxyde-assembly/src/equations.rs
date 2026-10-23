//! The assembly's own equations, as in SolidWorks: global variables, and equations driving
//! the values of its Distance and Angle mates (`"D1@Distance1" = "Gap" * 2`) and the
//! dimensions and global variables of its parts (`"D1@Sketch1@Top panel<1>.Part" =
//! "Width" - 6`).
//!
//! The lists, their text form, linked equations files and the evaluation are
//! `oxyde_model::Equations`'; this module says what an assembly's names refer to. Parts are
//! read and driven through [`PartValues`], which the application implements on the files it
//! loaded: a part driven by the assembly changes, and is saved with it.

use std::collections::HashMap;
use std::path::PathBuf;

use oxyde_model::{
    Dimensions, EquationResults, Equations, EquationsText, Expr, LinkedValue, ModelError,
};

use crate::{Assembly, ComponentId, Mate, MateId, MateKind};

/// What an assembly's equations drive and read, besides its global variables.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AsmDimension {
    /// The value of a Distance mate (mm) or an Angle mate (degrees): `D1@Distance1`.
    Mate(MateId),
    /// A dimension (`D1@Sketch1`) or a global variable (`Width`) of a part of the assembly.
    /// `path` leads to the part: a top-level component, then down through sub-assemblies.
    /// Written as SolidWorks does, `D1@Sketch1@Top panel<1>.Part`, and through a
    /// sub-assembly `D1@Sketch1@SSD<1>.Part@SSD holder<1>.Assembly`.
    Part {
        path: Vec<ComponentId>,
        name: String,
    },
}

/// How a component is named in an equation, after its instance name.
const PART: &str = ".Part";
const ASSEMBLY: &str = ".Assembly";

/// The components inside an assembly's sub-assemblies, for names reaching into them. Only
/// the application knows them, once it has loaded the files.
pub trait ComponentTree {
    /// The components of the sub-assembly `path` leads to (never empty), by id and name;
    /// `None` when that is not a loaded sub-assembly.
    fn components(&self, path: &[ComponentId]) -> Option<Vec<(ComponentId, String)>>;
}

/// No sub-assembly is known: names reach the assembly's own components only.
pub struct TopLevelOnly;

impl ComponentTree for TopLevelOnly {
    fn components(&self, _: &[ComponentId]) -> Option<Vec<(ComponentId, String)>> {
        None
    }
}

/// The parts of an assembly, as its equations read and drive them.
pub trait PartValues {
    /// The file of the part `path` leads to. Every instance of a part shares its file, so
    /// an evaluation gives each value of a file one value only.
    fn file(&self, path: &[ComponentId]) -> Option<PathBuf>;

    /// The value of `name` (a dimension `D1@Sketch1`, or a global variable) in the part
    /// `path` leads to, in display units; `None` if there is no such part or value.
    fn value(&self, path: &[ComponentId], name: &str) -> Option<f64>;

    /// Drives it to `value`. Returns the value it now has, or why it cannot be driven.
    fn drive(&mut self, path: &[ComponentId], name: &str, value: f64) -> Result<f64, String>;
}

/// No part is loaded: equations reaching into parts report them missing.
pub struct NoParts;

impl PartValues for NoParts {
    fn file(&self, _: &[ComponentId]) -> Option<PathBuf> {
        None
    }

    fn value(&self, _: &[ComponentId], _: &str) -> Option<f64> {
        None
    }

    fn drive(&mut self, _: &[ComponentId], name: &str, _: f64) -> Result<f64, String> {
        Err(format!("\"{name}\": the part is not loaded"))
    }
}

/// A Distance or Angle mate's dimension, as the Equations window lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct MateDimension {
    pub mate: MateId,
    /// `D1@Distance1`.
    pub full_name: String,
    /// mm, or degrees.
    pub value: f64,
    pub angle: bool,
    /// Set by an equation, so not editable by hand.
    pub driven: bool,
}

/// A mate's value in display units: mm, or degrees.
fn mate_value(mate: &Mate) -> Option<f64> {
    match mate.kind {
        MateKind::Distance(d) => Some(d),
        MateKind::Angle(a) => Some(a.to_degrees()),
        _ => None,
    }
}

/// The assembly's mates and parts, as its equations read and drive them.
struct AssemblyDimensions<'a> {
    mates: &'a mut [Mate],
    parts: &'a mut dyn PartValues,
    /// What the evaluation drove in each part file, by name.
    driven: HashMap<(PathBuf, String), f64>,
}

impl Dimensions<AsmDimension> for AssemblyDimensions<'_> {
    fn value(&self, dimension: &AsmDimension) -> Option<f64> {
        match dimension {
            AsmDimension::Mate(id) => mate_value(self.mates.iter().find(|m| m.id == *id)?),
            AsmDimension::Part { path, name } => self.parts.value(path, name),
        }
    }

    fn drive(&mut self, dimension: &AsmDimension, value: f64) -> Result<f64, String> {
        match dimension {
            AsmDimension::Mate(id) => {
                let mate = self
                    .mates
                    .iter_mut()
                    .find(|m| m.id == *id)
                    .ok_or_else(|| "the mate does not exist".to_owned())?;
                match &mut mate.kind {
                    MateKind::Distance(d) => {
                        if value < 0.0 {
                            return Err(format!(
                                "{value} mm: a distance cannot be negative (flip the mate instead)"
                            ));
                        }
                        *d = value;
                    }
                    MateKind::Angle(a) => {
                        if !(0.0..=180.0).contains(&value) {
                            return Err(format!("{value}°: an angle mate is from 0° to 180°"));
                        }
                        *a = value.to_radians();
                    }
                    _ => return Err(format!("{} has no value", mate.name)),
                }
                Ok(value)
            }
            AsmDimension::Part { path, name } => {
                let file = self
                    .parts
                    .file(path)
                    .ok_or_else(|| "the part is not loaded".to_owned())?;
                let key = (file, name.clone());
                let differs = |before: &f64| (before - value).abs() > 1e-9 * value.abs().max(1.0);
                if let Some(before) = self.driven.get(&key).filter(|b| differs(b)) {
                    return Err(format!(
                        "another instance of the part already drives \"{name}\" to \
                         {before}: every instance of a part shares its file"
                    ));
                }
                let now = self.parts.drive(path, name, value)?;
                self.driven.insert(key, now);
                Ok(now)
            }
        }
    }
}

impl Assembly {
    /// The components of a level: the assembly's own for an empty `path`, else those of the
    /// sub-assembly it leads to, as far as `tree` knows them.
    fn level(
        &self,
        path: &[ComponentId],
        tree: &dyn ComponentTree,
    ) -> Option<Vec<(ComponentId, String)>> {
        if path.is_empty() {
            Some(
                self.components
                    .iter()
                    .map(|c| (c.id, c.name.clone()))
                    .collect(),
            )
        } else {
            tree.components(path)
        }
    }

    /// What a full name in an equation refers to: `D1@Distance1`, or a part's
    /// `D1@Sketch1@Top panel<1>.Part` (`…@Sub<1>.Assembly` through sub-assemblies).
    pub fn find_dimension(&self, name: &str, tree: &dyn ComponentTree) -> Option<AsmDimension> {
        let name = name.trim().trim_matches('"');
        if name.ends_with(PART) || name.ends_with(ASSEMBLY) {
            return self.find_part_value(name, tree);
        }
        let (dimension, mate) = name.split_once('@')?;
        if dimension != "D1" {
            return None;
        }
        self.mates
            .iter()
            .find(|m| m.name == mate && mate_value(m).is_some())
            .map(|m| AsmDimension::Mate(m.id))
    }

    /// A part's dimension or variable, read from the outside in: the last component named
    /// is one of the assembly's own, each one before it inside the previous one. Component
    /// names may hold '@', so each is matched whole against the components of its level.
    fn find_part_value(&self, name: &str, tree: &dyn ComponentTree) -> Option<AsmDimension> {
        let mut path = Vec::new();
        let mut rest = name;
        loop {
            let level = self.level(&path, tree)?;
            // The longest component name that ends what is left, so that "Big Foot<1>"
            // wins over a "Foot<1>" that would leave "Big" behind.
            let mut best: Option<(ComponentId, usize, bool)> = None;
            for (id, component) in &level {
                for (suffix, is_part) in [(PART, true), (ASSEMBLY, false)] {
                    let tail = format!("@{component}{suffix}");
                    if rest.len() > tail.len() && rest.ends_with(&tail) {
                        let cut = rest.len() - tail.len();
                        if best.is_none_or(|(_, c, _)| cut < c) {
                            best = Some((*id, cut, is_part));
                        }
                    }
                }
            }
            let (id, cut, is_part) = best?;
            path.push(id);
            rest = rest.get(..cut)?;
            if is_part {
                // A dimension `D1@Sketch1` has one '@', a global variable none.
                return (!rest.is_empty() && rest.matches('@').count() <= 1).then(|| {
                    AsmDimension::Part {
                        path,
                        name: rest.to_owned(),
                    }
                });
            }
        }
    }

    /// The full name of what an equation refers to, with the current names; `None` when it
    /// no longer exists (a deleted mate) or a name along the way is not known.
    pub fn dimension_name(
        &self,
        dimension: &AsmDimension,
        tree: &dyn ComponentTree,
    ) -> Option<String> {
        match dimension {
            AsmDimension::Mate(id) => self
                .mate(*id)
                .filter(|m| mate_value(m).is_some())
                .map(|m| format!("D1@{}", m.name)),
            AsmDimension::Part { path, name } => {
                let mut names = Vec::with_capacity(path.len());
                for (depth, id) in path.iter().enumerate() {
                    let level = self.level(path.get(..depth)?, tree)?;
                    let (_, component) = level.into_iter().find(|(c, _)| c == id)?;
                    names.push(component);
                }
                // Innermost first: the part, then each sub-assembly holding it.
                let mut out = name.clone();
                for (depth, component) in names.iter().enumerate().rev() {
                    let suffix = if depth + 1 == names.len() {
                        PART
                    } else {
                        ASSEMBLY
                    };
                    out.push_str(&format!("@{component}{suffix}"));
                }
                Some(out)
            }
        }
    }

    /// The equations written with names, as stored in files. Names reaching into
    /// sub-assemblies that `tree` does not know are written as they were last known.
    pub fn equations_text(&self, tree: &dyn ComponentTree) -> EquationsText {
        self.equations.to_text(&|d| self.dimension_name(d, tree))
    }

    /// Replaces the equations with ones read from text. Names that match nothing yet (a part
    /// inside a sub-assembly not loaded) are kept, to be found by
    /// [`Assembly::resolve_equation_names`] or reported when the equations are evaluated.
    pub fn set_equations_text(
        &mut self,
        text: EquationsText,
        tree: &dyn ComponentTree,
    ) -> Result<(), ModelError> {
        self.equations = Equations::from_text(&text, &|name| self.find_dimension(name, tree))?;
        Ok(())
    }

    /// Finds the names that matched nothing when the equations were read, now that `tree`
    /// knows the sub-assemblies, and refreshes the others' names.
    pub fn resolve_equation_names(&mut self, tree: &dyn ComponentTree) {
        if self.equations.is_empty() {
            return;
        }
        let mut equations = std::mem::take(&mut self.equations);
        equations.resolve_missing(&|name| self.find_dimension(name, tree));
        equations.refresh_names(&|d| self.dimension_name(d, tree));
        self.equations = equations;
    }

    /// Links the assembly to the equations file `file` (relative to the assembly's folder)
    /// and takes its variables; `None` unlinks, keeping the variables as the assembly's own.
    pub fn link_equations(
        &mut self,
        file: Option<String>,
        values: &[LinkedValue],
        tree: &dyn ComponentTree,
    ) -> Result<(), ModelError> {
        let mut text = self.equations_text(tree);
        let linking = file.is_some();
        text.link(file);
        if linking {
            text.apply_linked_values(values);
        }
        self.set_equations_text(text, tree)
    }

    /// Takes the variables read from the linked equations file, as a part does
    /// (`oxyde_model::EquationsText::apply_linked_values`).
    pub fn apply_linked_values(
        &mut self,
        values: &[LinkedValue],
        tree: &dyn ComponentTree,
    ) -> Result<(), ModelError> {
        let mut text = self.equations_text(tree);
        text.apply_linked_values(values);
        self.set_equations_text(text, tree)
    }

    /// The linked variables, as the equations file writes them.
    pub fn linked_values(&self) -> Vec<LinkedValue> {
        self.equations_text(&TopLevelOnly).linked_values()
    }

    /// Parses an expression typed by the user; every name must exist.
    pub fn parse_expression(
        &self,
        text: &str,
        tree: &dyn ComponentTree,
    ) -> Result<Expr<AsmDimension>, ModelError> {
        self.equations
            .parse(text, &|name| self.find_dimension(name, tree))
    }

    /// An expression written with the current names.
    pub fn expression_text(&self, expr: &Expr<AsmDimension>, tree: &dyn ComponentTree) -> String {
        self.equations
            .expression_text(expr, &|d| self.dimension_name(d, tree))
    }

    /// Whether an equation sets this mate's value.
    pub fn is_mate_driven(&self, id: MateId) -> bool {
        self.equations.is_driven(&AsmDimension::Mate(id))
    }

    /// The dimension of every Distance and Angle mate, in the order of the mates.
    pub fn mate_dimensions(&self) -> Vec<MateDimension> {
        self.mates
            .iter()
            .filter_map(|m| {
                Some(MateDimension {
                    mate: m.id,
                    full_name: format!("D1@{}", m.name),
                    value: mate_value(m)?,
                    angle: matches!(m.kind, MateKind::Angle(_)),
                    driven: self.is_mate_driven(m.id),
                })
            })
            .collect()
    }

    /// Evaluates the global variables and equations in dependency order, setting the values
    /// of the driven mates and driving the parts through `parts`. A value a mate cannot take
    /// (a negative distance, an angle outside 0–180°) is an error on its equation, and the
    /// mate keeps its value.
    pub fn evaluate_equations(&mut self, parts: &mut dyn PartValues) -> EquationResults {
        if self.equations.is_empty() {
            return EquationResults::default();
        }
        let equations = std::mem::take(&mut self.equations);
        let results = equations.evaluate(&mut AssemblyDimensions {
            mates: &mut self.mates,
            parts,
            driven: HashMap::new(),
        });
        self.equations = equations;
        results
    }
}
