//! The open assembly's equations, as the application has them (`oxyde-app`'s
//! `assembly_equations.rs`): global variables, and equations driving mate values and the
//! dimensions and variables of its parts. Each instance of a part is loaded on its own
//! here, so driving one drives every copy of its file; the files changed are saved with the
//! assembly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use oxyde_assembly::{ComponentId, ComponentTree, PartValues};
use oxyde_kernel::Mesh;
use oxyde_model::LinkedValue;
use oxyde_occt::OcctKernel;
use serde_json::{Value, json};

use crate::args::{self, Result};
use crate::assembly::{AssemblyDoc, Loaded, LoadedPart};
use crate::geometry::Solids;
use crate::session::Session;
use crate::tools::ToolOutput;

type Tree = BTreeMap<ComponentId, Loaded>;

/// What `path` leads to: a top-level component, then down through sub-assemblies.
fn node_at<'a>(loaded: &'a Tree, path: &[ComponentId]) -> Option<&'a Loaded> {
    let (first, rest) = path.split_first()?;
    let mut node = loaded.get(first)?;
    for id in rest {
        let Loaded::Assembly(sub) = node else {
            return None;
        };
        node = sub.children.get(id)?;
    }
    Some(node)
}

fn part_at<'a>(loaded: &'a Tree, path: &[ComponentId]) -> Option<&'a LoadedPart> {
    match node_at(loaded, path)? {
        Loaded::Part(p) => Some(p),
        Loaded::Assembly(_) => None,
    }
}

/// Every loaded part, at every depth.
fn each_part(loaded: &mut Tree, f: &mut dyn FnMut(&mut LoadedPart)) {
    for node in loaded.values_mut() {
        match node {
            Loaded::Part(p) => f(p),
            Loaded::Assembly(sub) => each_part(&mut sub.children, f),
        }
    }
}

/// The loaded sub-assemblies, as the equations name what is inside them.
pub(crate) struct LoadedTree<'a>(&'a Tree);

impl ComponentTree for LoadedTree<'_> {
    fn components(&self, path: &[ComponentId]) -> Option<Vec<(ComponentId, String)>> {
        let Loaded::Assembly(sub) = node_at(self.0, path)? else {
            return None;
        };
        Some(
            sub.file
                .assembly
                .components
                .iter()
                .map(|c| (c.id, c.name.clone()))
                .collect(),
        )
    }
}

/// The loaded parts, as the equations read and drive them.
struct LoadedParts<'a> {
    loaded: &'a mut Tree,
    /// Files this evaluation changed.
    changed: BTreeSet<PathBuf>,
}

impl PartValues for LoadedParts<'_> {
    fn file(&self, path: &[ComponentId]) -> Option<PathBuf> {
        part_at(self.loaded, path).map(|p| p.file.clone())
    }

    fn value(&self, path: &[ComponentId], name: &str) -> Option<f64> {
        part_at(self.loaded, path)?.part.value_of(name)
    }

    fn drive(&mut self, path: &[ComponentId], name: &str, value: f64) -> Result<f64> {
        let file = part_at(self.loaded, path)
            .ok_or("the component is not a loaded part")?
            .file
            .clone();
        let mut outcome: Result<f64> = Err("the component is not a loaded part".to_owned());
        let mut changed = false;
        each_part(self.loaded, &mut |p| {
            if p.file == file {
                let result = p.part.drive(name, value);
                if let Ok((_, true)) = result {
                    changed = true;
                }
                outcome = result.map(|(now, _)| now);
            }
        });
        let now = outcome?;
        if changed {
            self.changed.insert(file);
        }
        Ok(now)
    }
}

/// Rounds of rebuilding driven parts and evaluating again, as in the application.
const MAX_ROUNDS: usize = 3;

/// Rebuilds a part and meshes it again.
fn rebuild(p: &mut LoadedPart, kernel: &mut OcctKernel) {
    p.part.rebuild(kernel);
    let (mesh, bounds) = match p.part.body() {
        Some(body) => {
            let solids = Solids::of_body(body);
            (
                solids.mesh(kernel).unwrap_or_default(),
                solids.bounds(kernel),
            )
        }
        None => (Mesh::default(), None),
    };
    p.mesh = mesh;
    p.bounds = bounds;
}

impl AssemblyDoc {
    pub(crate) fn tree(&self) -> LoadedTree<'_> {
        LoadedTree(&self.loaded)
    }

    /// Evaluates the equations, driving mates and parts, and rebuilds the parts they
    /// changed; an equation reading a driven part is evaluated again after its rebuild.
    pub fn evaluate_equations(&mut self, kernel: &mut OcctKernel) {
        for round in 1..=MAX_ROUNDS {
            let AssemblyDoc { file, loaded, .. } = self;
            if file.assembly.equations.is_empty() {
                self.equation_results = Default::default();
                return;
            }
            file.assembly.resolve_equation_names(&LoadedTree(loaded));
            let mut parts = LoadedParts {
                loaded,
                changed: BTreeSet::new(),
            };
            let results = file.assembly.evaluate_equations(&mut parts);
            let changed = parts.changed;
            self.equation_results = results;
            if changed.is_empty() {
                return;
            }
            each_part(&mut self.loaded, &mut |p| {
                if changed.contains(&p.file) {
                    rebuild(p, kernel);
                }
            });
            self.changed_parts.extend(changed);
            if round == MAX_ROUNDS {
                return;
            }
        }
    }

    /// Gives the parts linked to the equations file `file` its new `values`, rebuilding
    /// those that change; they are saved with the assembly.
    fn refresh_linked_parts(
        &mut self,
        file: &Path,
        values: &[LinkedValue],
        kernel: &mut OcctKernel,
    ) {
        let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let target = canonical(file);
        let mut changed = BTreeSet::new();
        each_part(&mut self.loaded, &mut |p| {
            let Some(linked) = p.part.equations().linked_file.clone() else {
                return;
            };
            let at = oxyde_format::equations_file::linked_file_path(&p.file, &linked);
            if canonical(&at) != target {
                return;
            }
            let before = p.part.definition();
            if p.part.apply_linked_values(values).is_ok() && p.part.definition() != before {
                rebuild(p, kernel);
                changed.insert(p.file.clone());
            }
        });
        self.changed_parts.extend(changed);
    }

    /// Saves the part files the assembly changed, each from one of its loaded copies.
    /// Returns the files saved.
    pub fn save_changed_parts(&mut self) -> Result<Vec<String>> {
        let mut saved = Vec::new();
        let paths: Vec<PathBuf> = self.changed_parts.iter().cloned().collect();
        for path in paths {
            let mut definition = None;
            each_part(&mut self.loaded, &mut |p| {
                if definition.is_none() && p.file == path {
                    definition = Some(p.part.definition());
                }
            });
            let Some(definition) = definition else {
                continue;
            };
            let mut file =
                oxyde_format::load_part(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            file.definition = definition;
            oxyde_format::save_part(&path, &file)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            self.changed_parts.remove(&path);
            saved.push(path.display().to_string());
        }
        Ok(saved)
    }

    /// Where an equations file named in a tool is: relative to the assembly's folder.
    fn equations_file_path(&self, file: &str) -> PathBuf {
        if Path::new(file).is_absolute() {
            file.into()
        } else {
            self.dir.join(file)
        }
    }

    /// The equations, as the tools report them.
    pub fn equations_report(&self) -> Value {
        let assembly = &self.file.assembly;
        let tree = self.tree();
        let text = assembly.equations_text(&tree);
        let results = &self.equation_results;
        let value = |result: Option<&std::result::Result<f64, String>>| match result {
            Some(Ok(v)) => json!(args::round(*v)),
            Some(Err(e)) => json!({ "error": e }),
            None => Value::Null,
        };
        json!({
            "variables": text.globals.iter().enumerate().map(|(i, g)| {
                let mut row = json!([g.name, g.expression, value(results.globals.get(i))]);
                if let (true, Some(list)) = (g.linked, row.as_array_mut()) {
                    list.push(json!("linked"));
                }
                row
            }).collect::<Vec<_>>(),
            "equations_file": text.linked_file,
            "equations": text.equations.iter().enumerate().map(|(i, e)| json!([e.dimension, e.expression, value(results.equations.get(i))])).collect::<Vec<_>>(),
            "parts_to_save": self.changed_parts.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        })
    }
}

fn doc_mut(session: &mut Session) -> Result<&mut AssemblyDoc> {
    session
        .assembly
        .as_mut()
        .ok_or_else(|| "no assembly is open: use new_assembly or open_assembly".to_owned())
}

/// Evaluates the equations and reports them, with the mates solved.
fn report(session: &mut Session) -> Result<ToolOutput> {
    crate::assembling::solve(session)?;
    Ok(ToolOutput::json(&doc_mut(session)?.equations_report()))
}

/// `set_variable` on the assembly: adds or changes one of its global variables. A linked
/// variable is changed in the equations file, which the parts linked to it read.
pub(crate) fn set_variable(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let name = args::str(args, "name")?.to_owned();
    let text = args::str(args, "expression")?.to_owned();
    let Session {
        assembly, kernel, ..
    } = session;
    let doc = assembly.as_mut().ok_or("no assembly is open")?;
    let equations = &doc.file.assembly.equations;
    let linked = equations
        .globals
        .iter()
        .any(|g| g.name == name && g.linked)
        .then(|| equations.linked_file.clone())
        .flatten();
    let AssemblyDoc { file, loaded, .. } = doc;
    let tree = LoadedTree(loaded);
    let expr = file
        .assembly
        .parse_expression(&text, &tree)
        .map_err(|e| e.to_string())?;
    if let Some(linked) = linked {
        let path = doc.equations_file_path(&linked);
        let io = |e: oxyde_format::FormatError| format!("{}: {e}", path.display());
        let value = LinkedValue {
            name,
            expression: text,
        };
        oxyde_format::equations_file::update_equations_file(&path, &[value]).map_err(io)?;
        let values = oxyde_format::equations_file::read_equations_file(&path).map_err(io)?;
        doc.file
            .assembly
            .apply_linked_values(&values, &LoadedTree(&doc.loaded))
            .map_err(|e| e.to_string())?;
        doc.refresh_linked_parts(&path, &values, kernel);
        return report(session);
    }
    let equations = &mut file.assembly.equations;
    match equations
        .globals
        .iter()
        .find(|g| g.name == name)
        .map(|g| g.id)
    {
        Some(id) => equations.set_global_expression(id, expr),
        None => equations.add_global(&name, expr).map(|_| ()),
    }
    .map_err(|e| e.to_string())?;
    report(session)
}

/// `set_equation` on the assembly: drives a mate value (`"D1@Distance1"`) or a part's
/// dimension or variable (`"D1@Sketch1@Top panel<1>.Part"`).
pub(crate) fn set_equation(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let dimension = args::str(args, "dimension")?.to_owned();
    let text = args::str(args, "expression")?.to_owned();
    let doc = doc_mut(session)?;
    let AssemblyDoc { file, loaded, .. } = doc;
    let tree = LoadedTree(loaded);
    let assembly = &mut file.assembly;
    let target = assembly
        .find_dimension(&dimension, &tree)
        .ok_or_else(|| format!("no dimension \"{dimension}\" (a mate's D1@Distance1, or a part's D1@Sketch1@Part<1>.Part)"))?;
    let name = assembly
        .dimension_name(&target, &tree)
        .unwrap_or_else(|| dimension.clone());
    let expr = assembly
        .parse_expression(&text, &tree)
        .map_err(|e| e.to_string())?;
    let existing = assembly
        .equations
        .equations
        .iter()
        .position(|e| e.target.as_ref() == Some(&target));
    match existing {
        Some(index) => assembly.equations.set_equation_expression(index, expr),
        None => assembly
            .equations
            .add_equation(target, name, expr)
            .map(|_| ()),
    }
    .map_err(|e| e.to_string())?;
    report(session)
}

/// `link_equations` on the assembly: links it to an equations file shared with parts,
/// writing `variables` into the file first; `"file": null` unlinks.
pub(crate) fn link_equations(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let file = match args::get(args, "file")? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        _ => return Err("\"file\" must be a path or null".to_owned()),
    };
    let doc = doc_mut(session)?;
    let values = match &file {
        None => Vec::new(),
        Some(file) => {
            let path = doc.equations_file_path(file);
            if let Some(variables) = args::opt(args, "variables") {
                let variables = variables
                    .as_object()
                    .ok_or("\"variables\" must be an object: {\"Width\": \"233\"}")?;
                let mut values = Vec::new();
                for (name, expression) in variables {
                    let expression = match expression {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        _ => return Err(format!("\"{name}\": give an expression")),
                    };
                    values.push(LinkedValue {
                        name: name.clone(),
                        expression,
                    });
                }
                oxyde_format::equations_file::update_equations_file(&path, &values)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
            }
            oxyde_format::equations_file::read_equations_file(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?
        }
    };
    let AssemblyDoc {
        file: document,
        loaded,
        ..
    } = doc;
    document
        .assembly
        .link_equations(file, &values, &LoadedTree(loaded))
        .map_err(|e| e.to_string())?;
    report(session)
}

#[cfg(test)]
mod tests {
    use oxyde_geom::Point2;
    use oxyde_model::{Extrude, ExtrudeOp, FeatureKind, Part, Sketch, SketchPlane, StandardPlane};
    use oxyde_sketch::{Constraint, EntityId};
    use serde_json::json;

    use super::*;

    /// A 40 × 20 plate, 10 thick, on the Front plane, saved at `path`.
    fn save_plate(path: &Path) {
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
        part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            10.0,
        )));
        let file = oxyde_format::PartFile {
            document_id: oxyde_format::new_document_id(),
            definition: part.definition(),
            properties: Default::default(),
        };
        oxyde_format::save_part(path, &file).unwrap();
    }

    fn call(session: &mut Session, name: &str, args: Value) -> Value {
        let out =
            crate::tools::call(session, name, &args).unwrap_or_else(|e| panic!("{name}: {e}"));
        serde_json::from_str(&out.text).unwrap_or(Value::String(out.text))
    }

    fn depth(path: &Path) -> f64 {
        let part =
            Part::from_definition(oxyde_format::load_part(path).unwrap().definition).unwrap();
        let depth = part.find_dimension("D1@Boss-Extrude1").unwrap();
        part.dimension(depth).unwrap().value
    }

    #[test]
    fn assembly_equations_drive_mates_and_parts_through_the_tools() {
        let dir = std::env::temp_dir().join(format!("oxyde-mcp-asm-eq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plate = dir.join("Plate.oxyprt");
        save_plate(&plate);
        let mut session = Session::new();
        let path = dir.join("Plates.oxyasm").display().to_string();
        call(&mut session, "new_assembly", json!({ "path": path }));
        call(
            &mut session,
            "add_component",
            json!({ "file": "Plate.oxyprt" }),
        );
        call(
            &mut session,
            "add_component",
            json!({ "file": "Plate.oxyprt", "position": [0, 0, 30] }),
        );
        call(
            &mut session,
            "add_mate",
            json!({ "kind": "distance", "value": 25,
                    "a": { "component": "Plate<1>", "plane": "front" },
                    "b": { "component": "Plate<2>", "plane": "front" } }),
        );

        call(
            &mut session,
            "set_variable",
            json!({ "name": "Gap", "expression": "4", "assembly": true }),
        );
        let out = call(
            &mut session,
            "set_equation",
            json!({ "dimension": "D1@Distance1", "expression": "\"Gap\" * 3", "assembly": true }),
        );
        assert_eq!(out["equations"][0][2], 12.0, "{out}");
        let out = call(
            &mut session,
            "set_equation",
            json!({ "dimension": "D1@Boss-Extrude1@Plate<1>.Part", "expression": "\"Gap\" + 1",
                    "assembly": true }),
        );
        assert_eq!(out["equations"][1][2], 5.0, "{out}");
        assert_eq!(
            out["parts_to_save"].as_array().map(Vec::len),
            Some(1),
            "{out}"
        );
        // Both instances are the same file, so both changed.
        let info = call(&mut session, "assembly_info", json!({}));
        assert_eq!(info["volume"], 2.0 * 40.0 * 20.0 * 5.0, "{info}");
        assert!(info["equations"]["variables"][0][0] == "Gap", "{info}");

        let out = call(&mut session, "save_assembly", json!({}));
        assert_eq!(
            out["parts_saved"].as_array().map(Vec::len),
            Some(1),
            "{out}"
        );
        assert_eq!(depth(&plate), 5.0);

        // Linked to an equations file, the file's value drives the assembly and its parts.
        let out = call(
            &mut session,
            "link_equations",
            json!({ "file": "Case.txt", "variables": { "Gap": "6" }, "assembly": true }),
        );
        assert_eq!(out["equations_file"], "Case.txt", "{out}");
        assert_eq!(out["equations"][1][2], 7.0, "{out}");
        let text = std::fs::read_to_string(dir.join("Case.txt")).unwrap();
        assert!(text.contains("\"Gap\" = 6"), "{text}");
        let out = call(
            &mut session,
            "set_variable",
            json!({ "name": "Gap", "expression": "2", "assembly": true }),
        );
        assert_eq!(out["equations"][0][2], 6.0, "{out}");
        let text = std::fs::read_to_string(dir.join("Case.txt")).unwrap();
        assert!(text.contains("\"Gap\" = 2"), "{text}");
        call(&mut session, "save_assembly", json!({}));
        assert_eq!(depth(&plate), 3.0);
        drop(session);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
