//! Global variables and equations, SolidWorks style.
//!
//! - A **global variable** is a name and an expression: `"Width" = 50`,
//!   `"Height" = "Width" / 2`.
//! - An **equation** drives a dimension: `"D1@Boss-Extrude1" = "Width" * 2`. A driven
//!   dimension cannot be edited by hand any more; it is set before every rebuild.
//!
//! Expressions hold stable references (see [`crate::expression`]), so renaming a feature, a
//! dimension or a variable never breaks an equation. A reference to something deleted is
//! kept by name and reported as an error, as SolidWorks shows broken equations in red.
//!
//! Evaluation follows dependencies, whatever the order of the lists; a circular reference is
//! an error on every equation involved.
//!
//! [`Equations`] is generic over what its equations drive: a part's dimensions here, an
//! assembly's mate values and part dimensions in `oxyde-assembly`. The lists, their text
//! form, linked equations files and the evaluation are the same for both.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;

use crate::dimensions::{Dimension, DimensionRef, DimensionUnit, list_dimensions};
use crate::error::ModelError;
use crate::expression::{Expr, GlobalId, Operand};
use crate::part::{Part, PartDefinition};

#[derive(Debug, Clone, PartialEq)]
pub struct GlobalVariable<D = DimensionRef> {
    pub id: GlobalId,
    pub name: String,
    pub expr: Expr<D>,
    /// Read from the document's linked equations file ([`Equations::linked_file`]): the
    /// document keeps a copy, refreshed from the file whenever it is opened.
    pub linked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Equation<D = DimensionRef> {
    /// The driven dimension; `None` if its name matched nothing when the equation was read
    /// from a file.
    pub target: Option<D>,
    /// Full name of the target as last known ("D1@Sketch1").
    pub target_name: String,
    pub expr: Expr<D>,
}

/// Every global variable and equation of a part, or, with another `D`, of an assembly.
#[derive(Debug, Clone, PartialEq)]
pub struct Equations<D = DimensionRef> {
    pub globals: Vec<GlobalVariable<D>>,
    pub equations: Vec<Equation<D>>,
    /// Id the next new global variable will get.
    pub next_global_id: u32,
    /// Path of the linked equations file, relative to the document's folder (SolidWorks'
    /// "Link to external file"): documents linked to the same file share its variables.
    pub linked_file: Option<String>,
}

impl<D> Default for Equations<D> {
    fn default() -> Self {
        Self {
            globals: Vec::new(),
            equations: Vec::new(),
            next_global_id: 1,
            linked_file: None,
        }
    }
}

/// The dimensions a set of equations reads and drives, in display units: millimetres,
/// degrees and counts.
pub trait Dimensions<D> {
    /// The current value of `dimension`, or `None` if it does not exist.
    fn value(&self, dimension: &D) -> Option<f64>;

    /// Sets `dimension` to `value`, as an equation drives it. Returns the value it now has
    /// (a count is rounded), or why it cannot take that value.
    fn drive(&mut self, dimension: &D, value: f64) -> Result<f64, String>;
}

/// Values found by the last evaluation, in the order of [`Equations`]' lists (in display
/// units), or why each could not be evaluated.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EquationResults {
    pub globals: Vec<Result<f64, String>>,
    pub equations: Vec<Result<f64, String>>,
}

impl<D> Equations<D> {
    pub fn is_empty(&self) -> bool {
        self.globals.is_empty() && self.equations.is_empty() && self.linked_file.is_none()
    }
}

/// What a quoted name refers to: a dimension (found by `dimension`) when it contains '@', as
/// every dimension's full name does, else a global variable.
fn resolve_name<D>(
    name: &str,
    globals: &[GlobalVariable<D>],
    dimension: &dyn Fn(&str) -> Option<D>,
) -> Option<Operand<D>> {
    if name.contains('@') {
        dimension(name).map(Operand::Dimension)
    } else {
        globals
            .iter()
            .find(|g| g.name == name)
            .map(|g| Operand::Global(g.id))
    }
}

impl<D: Clone + Eq + Hash> Equations<D> {
    /// Checks names and ids, and raises `next_global_id` above every id in use.
    pub fn validate(&mut self) -> Result<(), ModelError> {
        let invalid = |reason: String| Err(ModelError::InvalidDefinition(reason));
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for global in &self.globals {
            if global.id.0 == 0 || !ids.insert(global.id) {
                return invalid(format!("duplicate global variable id {}", global.id.0));
            }
            if !oxyde_sketch::is_valid_name(&global.name) || !names.insert(global.name.as_str()) {
                return invalid(format!(
                    "invalid or duplicate global variable name \"{}\"",
                    global.name
                ));
            }
        }
        let mut targets = HashSet::new();
        for equation in &self.equations {
            if equation.target.as_ref().is_some_and(|t| !targets.insert(t)) {
                return invalid(format!("two equations drive {}", equation.target_name));
            }
        }
        let highest = self.globals.iter().map(|g| g.id.0).max().unwrap_or(0);
        self.next_global_id = self.next_global_id.max(highest + 1);
        Ok(())
    }

    /// The current name of a reference, or `None` if it no longer exists.
    fn name_of(
        &self,
        operand: &Operand<D>,
        dimension_name: &dyn Fn(&D) -> Option<String>,
    ) -> Option<String> {
        match operand {
            Operand::Global(id) => self
                .globals
                .iter()
                .find(|g| g.id == *id)
                .map(|g| g.name.clone()),
            Operand::Dimension(d) => dimension_name(d),
        }
    }

    /// Parses an expression typed by the user; every name must exist. `dimension` finds a
    /// dimension by its full name.
    pub fn parse(
        &self,
        text: &str,
        dimension: &dyn Fn(&str) -> Option<D>,
    ) -> Result<Expr<D>, ModelError> {
        let expr = Expr::parse(text, &|name| resolve_name(name, &self.globals, dimension))
            .map_err(ModelError::Expression)?;
        if let Some(name) = expr.unresolved().first() {
            return Err(ModelError::Expression(format!(
                "\"{name}\" is neither a global variable nor a dimension"
            )));
        }
        Ok(expr)
    }

    /// An expression written with the current names.
    pub fn expression_text(
        &self,
        expr: &Expr<D>,
        dimension_name: &dyn Fn(&D) -> Option<String>,
    ) -> String {
        let mut expr = expr.clone();
        expr.refresh_names(&|operand| self.name_of(operand, dimension_name));
        expr.to_string()
    }

    /// Updates the names remembered by expressions and equation targets, after a rename.
    pub fn refresh_names(&mut self, dimension_name: &dyn Fn(&D) -> Option<String>) {
        let names: Vec<(GlobalId, String)> = self
            .globals
            .iter()
            .map(|g| (g.id, g.name.clone()))
            .collect();
        let name_of = |operand: &Operand<D>| match operand {
            Operand::Global(id) => names
                .iter()
                .find(|(g, _)| g == id)
                .map(|(_, name)| name.clone()),
            Operand::Dimension(d) => dimension_name(d),
        };
        for global in &mut self.globals {
            global.expr.refresh_names(&name_of);
        }
        for equation in &mut self.equations {
            equation.expr.refresh_names(&name_of);
            if let Some(name) = equation.target.as_ref().and_then(dimension_name) {
                equation.target_name = name;
            }
        }
    }

    /// Resolves again the names that matched nothing when they were read: an assembly's
    /// equations name parts that are only known once they are loaded.
    pub fn resolve_missing(&mut self, dimension: &dyn Fn(&str) -> Option<D>) {
        let globals = self.globals.clone();
        let resolve = |name: &str| resolve_name(name, &globals, dimension);
        for global in &mut self.globals {
            global.expr.resolve_missing(&resolve);
        }
        let mut targets: HashSet<D> = self
            .equations
            .iter()
            .filter_map(|e| e.target.clone())
            .collect();
        for equation in &mut self.equations {
            equation.expr.resolve_missing(&resolve);
            if equation.target.is_none() {
                // A dimension is driven by one equation at most: a second one found for it
                // stays unresolved, and reported.
                equation.target =
                    dimension(&equation.target_name).filter(|t| targets.insert(t.clone()));
            }
        }
    }

    /// The equations written with names, as stored in files.
    pub fn to_text(&self, dimension_name: &dyn Fn(&D) -> Option<String>) -> EquationsText {
        EquationsText {
            globals: self
                .globals
                .iter()
                .map(|g| GlobalText {
                    id: g.id.0,
                    name: g.name.clone(),
                    expression: self.expression_text(&g.expr, dimension_name),
                    linked: g.linked,
                })
                .collect(),
            equations: self
                .equations
                .iter()
                .map(|e| EquationText {
                    dimension: e
                        .target
                        .as_ref()
                        .and_then(dimension_name)
                        .unwrap_or_else(|| e.target_name.clone()),
                    expression: self.expression_text(&e.expr, dimension_name),
                })
                .collect(),
            next_global_id: self.next_global_id,
            linked_file: self.linked_file.clone(),
        }
    }

    /// Equations read from text (e.g. from a file), `dimension` finding dimensions by full
    /// name. Names that match nothing are kept, and reported when the equations are
    /// evaluated; syntax errors and invalid variable names are refused.
    pub fn from_text(
        text: &EquationsText,
        dimension: &dyn Fn(&str) -> Option<D>,
    ) -> Result<Self, ModelError> {
        let mut globals: Vec<GlobalVariable<D>> = text
            .globals
            .iter()
            .map(|g| GlobalVariable {
                id: GlobalId(g.id),
                name: g.name.clone(),
                expr: Expr::Number(0.0),
                linked: g.linked,
            })
            .collect();
        let resolve = |name: &str| resolve_name(name, &globals, dimension);
        let mut exprs = Vec::with_capacity(text.globals.len());
        for g in &text.globals {
            exprs.push(
                Expr::parse(&g.expression, &resolve)
                    .map_err(|e| ModelError::Expression(format!("\"{}\": {e}", g.name)))?,
            );
        }
        let mut equations = Vec::with_capacity(text.equations.len());
        for e in &text.equations {
            let name = e.dimension.trim().trim_matches('"').to_owned();
            let target = match resolve(&name) {
                Some(Operand::Dimension(reference)) => Some(reference),
                _ => None,
            };
            let expr = Expr::parse(&e.expression, &resolve)
                .map_err(|err| ModelError::Expression(format!("\"{name}\": {err}")))?;
            equations.push(Equation {
                target,
                target_name: name,
                expr,
            });
        }
        for (global, expr) in globals.iter_mut().zip(exprs) {
            global.expr = expr;
        }
        let mut new = Equations {
            globals,
            equations,
            next_global_id: text.next_global_id,
            linked_file: text.linked_file.clone(),
        };
        new.validate()?;
        Ok(new)
    }

    fn check_global_name(&self, id: Option<GlobalId>, name: &str) -> Result<(), ModelError> {
        if !oxyde_sketch::is_valid_name(name) {
            return Err(ModelError::InvalidName(name.to_owned()));
        }
        if self
            .globals
            .iter()
            .any(|g| Some(g.id) != id && g.name == name)
        {
            return Err(ModelError::DuplicateName(name.to_owned()));
        }
        Ok(())
    }

    /// Adds a global variable, e.g. `"Width" = 50`.
    pub fn add_global(&mut self, name: &str, expr: Expr<D>) -> Result<GlobalId, ModelError> {
        self.check_global_name(None, name)?;
        let id = GlobalId(self.next_global_id);
        self.next_global_id += 1;
        self.globals.push(GlobalVariable {
            id,
            name: name.to_owned(),
            expr,
            linked: false,
        });
        Ok(id)
    }

    /// Renames a global variable. The names remembered by expressions follow on the next
    /// [`Equations::refresh_names`].
    pub fn rename_global(&mut self, id: GlobalId, name: &str) -> Result<(), ModelError> {
        self.check_global_name(Some(id), name)?;
        self.global_mut(id)?.name = name.to_owned();
        Ok(())
    }

    pub fn set_global_expression(&mut self, id: GlobalId, expr: Expr<D>) -> Result<(), ModelError> {
        self.global_mut(id)?.expr = expr;
        Ok(())
    }

    /// Removes a global variable. Expressions using it keep its name and report an error.
    pub fn remove_global(&mut self, id: GlobalId) -> Result<(), ModelError> {
        let index = self
            .globals
            .iter()
            .position(|g| g.id == id)
            .ok_or_else(|| ModelError::Expression(format!("no global variable {}", id.0)))?;
        self.globals.remove(index);
        Ok(())
    }

    fn global_mut(&mut self, id: GlobalId) -> Result<&mut GlobalVariable<D>, ModelError> {
        self.globals
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or_else(|| ModelError::Expression(format!("no global variable {}", id.0)))
    }

    /// Whether an equation drives `target`.
    pub fn is_driven(&self, target: &D) -> bool {
        self.equations
            .iter()
            .any(|e| e.target.as_ref() == Some(target))
    }

    /// Adds an equation driving `target`, whose full name is `target_name`. A dimension can
    /// be driven by one equation only.
    pub fn add_equation(
        &mut self,
        target: D,
        target_name: String,
        expr: Expr<D>,
    ) -> Result<usize, ModelError> {
        if self.is_driven(&target) {
            return Err(ModelError::Expression(format!(
                "\"{target_name}\" is already driven by an equation"
            )));
        }
        self.equations.push(Equation {
            target: Some(target),
            target_name,
            expr,
        });
        Ok(self.equations.len() - 1)
    }

    pub fn set_equation_expression(
        &mut self,
        index: usize,
        expr: Expr<D>,
    ) -> Result<(), ModelError> {
        let equation = self
            .equations
            .get_mut(index)
            .ok_or_else(|| ModelError::Expression(format!("no equation {}", index + 1)))?;
        equation.expr = expr;
        Ok(())
    }

    /// Removes an equation; its dimension keeps its last value and becomes editable again.
    pub fn remove_equation(&mut self, index: usize) -> Result<(), ModelError> {
        if index >= self.equations.len() {
            return Err(ModelError::Expression(format!("no equation {}", index + 1)));
        }
        self.equations.remove(index);
        Ok(())
    }

    /// Evaluates every global variable and equation, in dependency order whatever the order
    /// of the lists, and drives the dimensions of the equations through `dimensions`.
    pub fn evaluate(&self, dimensions: &mut dyn Dimensions<D>) -> EquationResults {
        // Nodes: the global variables, then the equations.
        let global_count = self.globals.len();
        let global_node: HashMap<GlobalId, usize> = self
            .globals
            .iter()
            .enumerate()
            .map(|(i, g)| (g.id, i))
            .collect();
        let mut driven_node: HashMap<&D, usize> = HashMap::new();
        for (j, equation) in self.equations.iter().enumerate() {
            if let Some(target) = &equation.target {
                driven_node.entry(target).or_insert(global_count + j);
            }
        }
        let exprs: Vec<&Expr<D>> = self
            .globals
            .iter()
            .map(|g| &g.expr)
            .chain(self.equations.iter().map(|e| &e.expr))
            .collect();
        let count = exprs.len();
        let mut indegree = vec![0usize; count];
        let mut users: Vec<Vec<usize>> = vec![Vec::new(); count];
        for (node, expr) in exprs.iter().enumerate() {
            for operand in expr.operands() {
                let dependency = match &operand {
                    Operand::Global(id) => global_node.get(id).copied(),
                    Operand::Dimension(d) => driven_node.get(d).copied(),
                };
                if let Some(list) = dependency.and_then(|d| users.get_mut(d)) {
                    list.push(node);
                    if let Some(n) = indegree.get_mut(node) {
                        *n += 1;
                    }
                }
            }
        }
        let mut queue: VecDeque<usize> = (0..count)
            .filter(|&i| indegree.get(i) == Some(&0))
            .collect();
        let mut order = Vec::with_capacity(count);
        while let Some(node) = queue.pop_front() {
            order.push(node);
            for &user in users.get(node).map_or(&[][..], Vec::as_slice) {
                if let Some(n) = indegree.get_mut(user) {
                    *n -= 1;
                    if *n == 0 {
                        queue.push_back(user);
                    }
                }
            }
        }

        let mut results: Vec<Option<Result<f64, String>>> = vec![None; count];
        let mut global_values: HashMap<GlobalId, Result<f64, String>> = HashMap::new();
        // Dimensions driven so far, with the value each now has.
        let mut driven: HashMap<D, f64> = HashMap::new();
        let mut failed: HashSet<D> = HashSet::new();
        for node in order {
            let Some(expr) = exprs.get(node) else {
                continue;
            };
            let value = {
                let reader: &dyn Dimensions<D> = dimensions;
                expr.evaluate(&mut |operand, name| match operand {
                    Operand::Global(id) => match global_values.get(id) {
                        Some(Ok(v)) => Ok(*v),
                        Some(Err(_)) => Err(format!("\"{name}\" has an error")),
                        None => Err(format!("\"{name}\" does not exist")),
                    },
                    Operand::Dimension(d) => {
                        if failed.contains(d) {
                            return Err(format!("\"{name}\" has an error"));
                        }
                        driven
                            .get(d)
                            .copied()
                            .or_else(|| reader.value(d))
                            .ok_or_else(|| format!("\"{name}\" does not exist"))
                    }
                })
            };
            if let Some(global) = self.globals.get(node) {
                global_values.insert(global.id, value.clone());
                if let Some(slot) = results.get_mut(node) {
                    *slot = Some(value);
                }
                continue;
            }
            let Some(equation) = self.equations.get(node - global_count) else {
                continue;
            };
            let outcome = value.and_then(|shown| {
                let missing = || format!("\"{}\" does not exist", equation.target_name);
                let target = equation.target.as_ref().ok_or_else(missing)?;
                dimensions.value(target).ok_or_else(missing)?;
                let now = dimensions.drive(target, shown)?;
                driven.insert(target.clone(), now);
                Ok(shown)
            });
            if let (Err(_), Some(target)) = (&outcome, &equation.target) {
                failed.insert(target.clone());
            }
            if let Some(slot) = results.get_mut(node) {
                *slot = Some(outcome);
            }
        }

        let mut results = results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err("circular reference (or depends on one)".to_owned())));
        EquationResults {
            globals: results.by_ref().take(global_count).collect(),
            equations: results.collect(),
        }
    }
}

// ---- Text form, for files ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct GlobalText {
    pub id: u32,
    pub name: String,
    pub expression: String,
    pub linked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EquationText {
    /// Full name of the driven dimension, e.g. "D1@Sketch1".
    pub dimension: String,
    pub expression: String,
}

/// Equations written with names, as stored in files.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EquationsText {
    pub globals: Vec<GlobalText>,
    pub equations: Vec<EquationText>,
    pub next_global_id: u32,
    pub linked_file: Option<String>,
}

/// One line of a linked equations file: `"Width" = 233`.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkedValue {
    pub name: String,
    pub expression: String,
}

impl EquationsText {
    /// Links to the equations file `file`, or unlinks with `None`: the variables then stay,
    /// as ordinary ones, with their last values.
    pub fn link(&mut self, file: Option<String>) {
        if file.is_none() {
            for g in &mut self.globals {
                g.linked = false;
            }
        }
        self.linked_file = file;
    }

    /// Takes the variables read from the linked equations file: a variable of the same name
    /// gets the file's expression and becomes linked, one not there yet is added. Variables
    /// the file no longer lists keep their last values but are no longer linked.
    pub fn apply_linked_values(&mut self, values: &[LinkedValue]) {
        for g in &mut self.globals {
            g.linked = false;
        }
        for value in values {
            let name = value.name.trim().trim_matches('"').trim();
            match self.globals.iter_mut().find(|g| g.name == name) {
                Some(g) => {
                    g.expression = value.expression.clone();
                    g.linked = true;
                }
                None => {
                    self.globals.push(GlobalText {
                        id: self.next_global_id,
                        name: name.to_owned(),
                        expression: value.expression.clone(),
                        linked: true,
                    });
                    self.next_global_id += 1;
                }
            }
        }
    }

    /// The linked variables, as the equations file writes them.
    pub fn linked_values(&self) -> Vec<LinkedValue> {
        self.globals
            .iter()
            .filter(|g| g.linked)
            .map(|g| LinkedValue {
                name: g.name.clone(),
                expression: g.expression.clone(),
            })
            .collect()
    }
}

// ---- Parts ---------------------------------------------------------------------------------

/// Full name of a part dimension, from the list of the part's dimensions.
fn dimension_name(dimensions: &[Dimension]) -> impl Fn(&DimensionRef) -> Option<String> + '_ {
    move |reference| {
        dimensions
            .iter()
            .find(|d| d.reference == *reference)
            .map(|d| d.full_name.clone())
    }
}

/// A part dimension by full name.
fn dimension_named(dimensions: &[Dimension]) -> impl Fn(&str) -> Option<DimensionRef> + '_ {
    move |name| {
        dimensions
            .iter()
            .find(|d| d.full_name == name)
            .map(|d| d.reference)
    }
}

impl PartDefinition {
    /// The equations written with the current names, e.g. for saving.
    pub fn equations_text(&self) -> EquationsText {
        let dimensions = list_dimensions(&self.features, &self.equations);
        self.equations.to_text(&dimension_name(&dimensions))
    }

    /// Replaces the equations with ones read from text (e.g. from a file). Names that match
    /// nothing are kept, and reported when the equations are evaluated; syntax errors and
    /// invalid variable names are refused.
    pub fn set_equations_text(&mut self, text: EquationsText) -> Result<(), ModelError> {
        // Driven flags do not matter for name resolution.
        let dimensions = list_dimensions(&self.features, &Equations::default());
        self.equations = Equations::from_text(&text, &dimension_named(&dimensions))?;
        Ok(())
    }

    /// Links the part to the equations file `file` (relative to the part's folder) and takes
    /// its variables, as [`Self::apply_linked_values`] does. `None` unlinks: the variables
    /// stay, as ordinary ones, with their last values.
    pub fn link_equations(
        &mut self,
        file: Option<String>,
        values: &[LinkedValue],
    ) -> Result<(), ModelError> {
        let mut text = self.equations_text();
        let linking = file.is_some();
        text.link(file);
        self.set_equations_text(text)?;
        if linking {
            self.apply_linked_values(values)?;
        }
        Ok(())
    }

    /// Takes the variables read from the linked equations file (see
    /// [`EquationsText::apply_linked_values`]), all at once, so the file's variables may
    /// refer to each other. A variable an assembly drives (`driven_by`) is left as the
    /// assembly set it: the assembly's value wins over the file's.
    pub fn apply_linked_values(&mut self, values: &[LinkedValue]) -> Result<(), ModelError> {
        let values: Vec<LinkedValue> = values
            .iter()
            .filter(|v| {
                let name = v.name.trim().trim_matches('"').trim();
                !self.driven_by.iter().any(|d| d.name == name)
            })
            .cloned()
            .collect();
        let mut text = self.equations_text();
        text.apply_linked_values(&values);
        self.set_equations_text(text)
    }

    /// The linked variables, as the equations file writes them.
    pub fn linked_values(&self) -> Vec<LinkedValue> {
        self.equations_text().linked_values()
    }
}

/// A part's dimensions as its equations read and drive them.
struct PartDimensions<'a> {
    part: &'a mut Part,
    /// Every dimension's value (internal units) and unit, as it is now.
    values: HashMap<DimensionRef, (f64, DimensionUnit)>,
}

impl Dimensions<DimensionRef> for PartDimensions<'_> {
    fn value(&self, dimension: &DimensionRef) -> Option<f64> {
        self.values
            .get(dimension)
            .map(|(value, unit)| unit.to_display(*value))
    }

    fn drive(&mut self, dimension: &DimensionRef, shown: f64) -> Result<f64, String> {
        let &(current, unit) = self
            .values
            .get(dimension)
            .ok_or_else(|| "the dimension does not exist".to_owned())?;
        let value = unit.from_display(shown)?;
        if (value - current).abs() > 1e-12 * value.abs().max(1.0) {
            self.part
                .set_dimension(*dimension, value)
                .map_err(|e| e.to_string())?;
            self.values.insert(*dimension, (value, unit));
        }
        Ok(unit.to_display(value))
    }
}

impl Part {
    /// [`PartDefinition::link_equations`] on the part; the next rebuild evaluates them.
    pub fn link_equations(
        &mut self,
        file: Option<String>,
        values: &[LinkedValue],
    ) -> Result<(), ModelError> {
        let mut definition = self.definition();
        definition.link_equations(file, values)?;
        self.equations = definition.equations;
        Ok(())
    }

    /// [`PartDefinition::apply_linked_values`] on the part; the next rebuild evaluates them.
    pub fn apply_linked_values(&mut self, values: &[LinkedValue]) -> Result<(), ModelError> {
        let mut definition = self.definition();
        definition.apply_linked_values(values)?;
        self.equations = definition.equations;
        Ok(())
    }

    pub fn equations(&self) -> &Equations {
        &self.equations
    }

    /// Results of the last evaluation (done at the start of every rebuild).
    pub fn equation_results(&self) -> &EquationResults {
        &self.equation_results
    }

    /// Parses an expression typed by the user; every name must exist.
    pub fn parse_expression(&self, text: &str) -> Result<Expr, ModelError> {
        let dimensions = self.dimensions();
        self.equations.parse(text, &dimension_named(&dimensions))
    }

    /// An expression written with the current names.
    pub fn expression_text(&self, expr: &Expr) -> String {
        let dimensions = self.dimensions();
        self.equations
            .expression_text(expr, &dimension_name(&dimensions))
    }

    /// Current full name of an equation's driven dimension.
    pub fn equation_target_name(&self, equation: &Equation) -> String {
        equation
            .target
            .and_then(|t| self.dimension(t))
            .map_or_else(|| equation.target_name.clone(), |d| d.full_name)
    }

    /// Adds a global variable, e.g. `"Width" = 50`.
    pub fn add_global(&mut self, name: &str, expr: Expr) -> Result<GlobalId, ModelError> {
        self.equations.add_global(name, expr)
    }

    /// Renames a global variable; the equations using it follow. One an assembly drives is
    /// refused: the assembly names it.
    pub fn rename_global(&mut self, id: GlobalId, name: &str) -> Result<(), ModelError> {
        self.check_global_not_driven(id)?;
        self.equations.rename_global(id, name)?;
        self.refresh_equation_names();
        Ok(())
    }

    /// Changes a global variable's expression; one an assembly drives is refused.
    pub fn set_global_expression(&mut self, id: GlobalId, expr: Expr) -> Result<(), ModelError> {
        self.check_global_not_driven(id)?;
        self.equations.set_global_expression(id, expr)
    }

    /// Removes a global variable. Expressions using it keep its name and report an error.
    /// One an assembly drives is refused.
    pub fn remove_global(&mut self, id: GlobalId) -> Result<(), ModelError> {
        self.check_global_not_driven(id)?;
        self.equations.remove_global(id)
    }

    /// Adds an equation driving `target`. A dimension can be driven by one equation only,
    /// the part's or an assembly's.
    pub fn add_equation(&mut self, target: DimensionRef, expr: Expr) -> Result<usize, ModelError> {
        let dimension = self
            .dimension(target)
            .ok_or_else(|| ModelError::UnknownDimension(format!("{target:?}")))?;
        self.check_not_driven_by_assembly(&dimension.full_name)?;
        self.equations
            .add_equation(target, dimension.full_name, expr)
    }

    pub fn set_equation_expression(&mut self, index: usize, expr: Expr) -> Result<(), ModelError> {
        self.equations.set_equation_expression(index, expr)
    }

    /// Removes an equation; its dimension keeps its last value and becomes editable again.
    pub fn remove_equation(&mut self, index: usize) -> Result<(), ModelError> {
        self.equations.remove_equation(index)
    }

    /// The value of a dimension (`D1@Sketch1`) or a global variable (`Width`) in display
    /// units, as an assembly's equations read it: as of the last rebuild.
    pub fn value_of(&self, name: &str) -> Option<f64> {
        let name = name.trim().trim_matches('"');
        if name.contains('@') {
            let dimension = self.dimension(self.find_dimension(name)?)?;
            return Some(dimension.unit.to_display(dimension.value));
        }
        let index = self.equations.globals.iter().position(|g| g.name == name)?;
        self.equation_results
            .globals
            .get(index)?
            .as_ref()
            .ok()
            .copied()
    }

    /// Sets a dimension (`D1@Sketch1`) or a global variable (`Width`) to `value`, in display
    /// units, as the equation of the assembly `assembly_id` drives it. Returns the value it
    /// now has (a count is rounded) and whether the part changed: it then needs a rebuild,
    /// and saving. A value another assembly drives is refused: one assembly at a time. The
    /// assembly's value wins over the part's own: the part's equation on a dimension goes,
    /// and a variable of its linked equations file is no longer read from the file, as an
    /// in-context value wins in SolidWorks.
    pub fn drive(
        &mut self,
        name: &str,
        value: f64,
        assembly_id: &str,
    ) -> Result<(f64, bool), String> {
        let name = name.trim().trim_matches('"');
        let missing = || format!("\"{name}\" does not exist in the part");
        if let Some(other) = self.driver(name).filter(|d| d.assembly_id != assembly_id) {
            return Err(format!(
                "\"{name}\" is driven by {}: release it in the part first",
                other.assembly_name()
            ));
        }
        if name.contains('@') {
            let reference = self.find_dimension(name).ok_or_else(missing)?;
            let own = self
                .equations
                .equations
                .iter()
                .position(|e| e.target == Some(reference));
            if let Some(index) = own {
                self.equations
                    .remove_equation(index)
                    .map_err(|e| e.to_string())?;
            }
            let dimension = self.dimension(reference).ok_or_else(missing)?;
            let new = dimension.unit.from_display(value)?;
            let changed =
                own.is_some() || (new - dimension.value).abs() > 1e-12 * new.abs().max(1.0);
            if changed {
                self.set_dimension(reference, new)
                    .map_err(|e| e.to_string())?;
            }
            return Ok((dimension.unit.to_display(new), changed));
        }
        // A dimension refuses this in `set_dimension`; a variable would print as "inf" and
        // not read back.
        if !value.is_finite() {
            return Err(format!("{value} is not a number"));
        }
        let global = self
            .equations
            .globals
            .iter_mut()
            .find(|g| g.name == name)
            .ok_or_else(missing)?;
        let changed = global.linked || global.expr != Expr::Number(value);
        global.linked = false;
        global.expr = Expr::Number(value);
        Ok((value, changed))
    }

    /// Updates the names remembered by expressions, after a rename.
    pub(crate) fn refresh_equation_names(&mut self) {
        if self.equations.is_empty() {
            return;
        }
        let dimensions = self.dimensions();
        self.equations.refresh_names(&dimension_name(&dimensions));
    }

    /// Evaluates every global variable and equation, in dependency order, and sets the
    /// driven dimensions. Only dimensions whose value changes mark features for rebuild.
    pub(crate) fn evaluate_equations(&mut self) {
        if self.equations.is_empty() {
            self.equation_results = EquationResults::default();
            return;
        }
        self.refresh_equation_names();
        let values = self
            .dimensions()
            .iter()
            .map(|d| (d.reference, (d.value, d.unit)))
            .collect();
        let equations = self.equations.clone();
        let results = equations.evaluate(&mut PartDimensions { part: self, values });
        self.equation_results = results;
    }
}

#[cfg(test)]
mod tests {
    use oxyde_geom::Point2;
    use oxyde_sketch::{Constraint, EntityId};

    use super::*;
    use crate::dimensions::DimensionKey;
    use crate::feature::{
        Extrude, ExtrudeOp, FeatureId, FeatureKind, FeatureParameter, Sketch, SketchPlane,
        StandardPlane,
    };

    /// Sketch1 (40 × 20 rectangle with width and height dimensions) and Boss-Extrude1.
    fn block() -> (Part, FeatureId, FeatureId) {
        let mut part = Part::new();
        let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
        let lines = sketch.rectangle(Point2::ORIGIN, 40.0, 20.0);
        sketch
            .geometry
            .constrain(Constraint::Length(EntityId(lines[0]), 40.0));
        sketch
            .geometry
            .constrain(Constraint::Length(EntityId(lines[1]), 20.0));
        let s1 = part.add_feature(FeatureKind::Sketch(sketch));
        let boss = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            10.0,
        )));
        (part, s1, boss)
    }

    fn value(part: &Part, full_name: &str) -> f64 {
        let reference = part.find_dimension(full_name).unwrap();
        part.dimension(reference).unwrap().value
    }

    fn equation(part: &mut Part, target: &str, text: &str) {
        let target = part.find_dimension(target).unwrap();
        let expr = part.parse_expression(text).unwrap();
        part.add_equation(target, expr).unwrap();
    }

    #[test]
    fn globals_and_equations_drive_dimensions() {
        let (mut part, ..) = block();
        let expr = part.parse_expression("50").unwrap();
        part.add_global("Width", expr).unwrap();
        // Listed before the variable it uses: order does not matter.
        equation(&mut part, "D2@Sketch1", "\"D1@Sketch1\" / 2");
        equation(&mut part, "D1@Sketch1", "\"Width\" * 2");
        equation(&mut part, "D1@Boss-Extrude1", "\"D2@Sketch1\" / 5");
        part.evaluate_equations();

        assert_eq!(value(&part, "D1@Sketch1"), 100.0);
        assert_eq!(value(&part, "D2@Sketch1"), 50.0);
        assert_eq!(value(&part, "D1@Boss-Extrude1"), 10.0);
        assert_eq!(part.equation_results().globals, vec![Ok(50.0)]);
        assert_eq!(
            part.equation_results().equations,
            vec![Ok(50.0), Ok(100.0), Ok(10.0)]
        );
        let d1 = part.find_dimension("D1@Sketch1").unwrap();
        assert!(part.is_driven(d1));
        assert!(part.dimension(d1).unwrap().driven);
    }

    fn linked(name: &str, expression: &str) -> LinkedValue {
        LinkedValue {
            name: name.to_owned(),
            expression: expression.to_owned(),
        }
    }

    #[test]
    fn linked_values_drive_dimensions_and_follow_their_file() {
        let (mut part, ..) = block();
        let expr = part.parse_expression("7").unwrap();
        part.add_global("Own", expr).unwrap();
        part.link_equations(
            Some("Case.txt".to_owned()),
            &[linked("Width", "50"), linked("Half", "\"Width\" / 2")],
        )
        .unwrap();
        equation(&mut part, "D1@Sketch1", "\"Width\"");
        equation(&mut part, "D2@Sketch1", "\"Half\"");
        part.evaluate_equations();
        assert_eq!(value(&part, "D1@Sketch1"), 50.0);
        assert_eq!(value(&part, "D2@Sketch1"), 25.0);
        let flags: Vec<_> = part
            .equations()
            .globals
            .iter()
            .map(|g| (g.name.as_str(), g.linked))
            .collect();
        assert_eq!(flags, [("Own", false), ("Width", true), ("Half", true)]);

        // The file changes: its values come in; one it no longer lists stays, unlinked.
        part.apply_linked_values(&[linked("Width", "80")]).unwrap();
        part.evaluate_equations();
        assert_eq!(value(&part, "D1@Sketch1"), 80.0);
        assert_eq!(value(&part, "D2@Sketch1"), 40.0);
        let half = part.equations().globals.iter().find(|g| g.name == "Half");
        assert!(half.is_some_and(|g| !g.linked));
        assert_eq!(part.definition().linked_values(), [linked("Width", "80")]);

        // Unlinking keeps every variable, with its last value.
        part.link_equations(None, &[]).unwrap();
        part.evaluate_equations();
        assert_eq!(value(&part, "D1@Sketch1"), 80.0);
        assert!(part.equations().linked_file.is_none());
        assert!(part.equations().globals.iter().all(|g| !g.linked));
    }

    #[test]
    fn renames_follow_into_equations() {
        let (mut part, s1, _) = block();
        let expr = part.parse_expression("50").unwrap();
        let width = part.add_global("Width", expr).unwrap();
        equation(&mut part, "D1@Boss-Extrude1", "\"Width\" + \"D2@Sketch1\"");

        part.rename(s1, "Base").unwrap();
        part.rename_global(width, "Breadth").unwrap();
        let height = part.find_dimension("D2@Base").unwrap();
        part.rename_dimension(height, "Height").unwrap();

        let text = part.definition().equations_text();
        assert_eq!(text.globals[0].name, "Breadth");
        assert_eq!(text.equations[0].dimension, "D1@Boss-Extrude1");
        assert_eq!(
            text.equations[0].expression,
            "\"Breadth\" + \"Height@Base\""
        );
        part.evaluate_equations();
        assert_eq!(value(&part, "D1@Boss-Extrude1"), 70.0);
    }

    #[test]
    fn errors_are_reported_per_equation() {
        let (mut part, ..) = block();
        equation(&mut part, "D1@Sketch1", "\"D2@Sketch1\" * 2");
        equation(&mut part, "D2@Sketch1", "\"D1@Sketch1\" / 2");
        equation(&mut part, "D1@Boss-Extrude1", "5");
        part.evaluate_equations();
        let results = &part.equation_results().equations;
        assert!(results[0].as_ref().unwrap_err().contains("circular"));
        assert!(results[1].as_ref().unwrap_err().contains("circular"));
        assert_eq!(results[2], Ok(5.0));

        // Unknown names are refused when typed.
        assert!(part.parse_expression("\"Nope\" + 1").is_err());
        assert!(part.parse_expression("\"D9@Sketch1\"").is_err());
        // A dimension can only be driven once.
        let depth = part.find_dimension("D1@Boss-Extrude1").unwrap();
        assert!(part.add_equation(depth, Expr::Number(1.0)).is_err());
        // Global names follow the same rules as other names.
        assert!(part.add_global("a@b", Expr::Number(1.0)).is_err());
        part.add_global("Width", Expr::Number(1.0)).unwrap();
        assert!(part.add_global("Width", Expr::Number(2.0)).is_err());
    }

    #[test]
    fn angles_are_in_degrees_in_equations() {
        let mut part = Part::new();
        let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
        let a = sketch.line(Point2::ORIGIN, Point2::new(10.0, 0.0));
        let b = sketch.line(Point2::ORIGIN, Point2::new(0.0, 10.0));
        sketch.geometry.constrain(Constraint::Angle(
            EntityId(a),
            EntityId(b),
            std::f64::consts::FRAC_PI_2,
        ));
        part.add_feature(FeatureKind::Sketch(sketch));
        assert_eq!(
            part.parse_expression("\"D1@Sketch1\" / 2")
                .unwrap()
                .evaluate(&mut |_, _| Ok(90.0))
                .unwrap(),
            45.0
        );
        equation(&mut part, "D1@Sketch1", "30");
        part.evaluate_equations();
        let angle = value(&part, "D1@Sketch1");
        assert!((angle - 30f64.to_radians()).abs() < 1e-12);
    }

    #[test]
    fn text_form_round_trips_and_keeps_broken_references() {
        let (mut part, _, boss) = block();
        let expr = part.parse_expression("25").unwrap();
        part.add_global("Width", expr).unwrap();
        equation(&mut part, "D1@Sketch1", "\"Width\" * 2");
        let definition = part.definition();
        let text = definition.equations_text();

        let mut restored = definition.clone();
        restored.equations = Equations::default();
        restored.set_equations_text(text.clone()).unwrap();
        assert_eq!(restored.equations, definition.equations);

        // An equation whose dimension is gone is kept, and reported.
        let mut broken = text;
        broken.equations[0].dimension = "D7@Sketch1".to_owned();
        broken.equations.push(EquationText {
            dimension: "D1@Boss-Extrude1".to_owned(),
            expression: "\"D8@Sketch1\" + 1".to_owned(),
        });
        let mut definition = part.definition();
        definition.set_equations_text(broken).unwrap();
        let mut part = Part::from_definition(definition).unwrap();
        part.evaluate_equations();
        let results = &part.equation_results().equations;
        assert!(results[0].as_ref().unwrap_err().contains("D7@Sketch1"));
        assert!(results[1].as_ref().unwrap_err().contains("D8@Sketch1"));
        assert_eq!(
            part.equations().equations[1].target,
            Some(DimensionRef {
                feature: boss,
                key: DimensionKey::Feature(FeatureParameter::Depth)
            })
        );
    }
}
