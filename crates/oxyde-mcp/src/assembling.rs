//! The assembly tools: components inserted, placed and mated, and the assembly saved.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oxyde_assembly::{ComponentId, MateKind, MateRef, Placement};
use oxyde_format::{AssemblyFile, document_kind, load_assembly, save_assembly as write_assembly};
use oxyde_kernel::Kernel;
use serde_json::{Value, json};

use crate::args::Set;
use crate::args::{self, Result};
use crate::assembly::{self, AssemblyDoc, Loaded};
use crate::session::Session;
use crate::tools::ToolOutput;

fn doc(session: &Session) -> Result<&AssemblyDoc> {
    session
        .assembly
        .as_ref()
        .ok_or_else(|| "no assembly is open: use new_assembly or open_assembly".to_owned())
}

fn set_assembly(session: &mut Session, mut doc: AssemblyDoc) -> Vec<String> {
    let failures = doc.load_all(&mut session.kernel);
    if let Some(old) = session.assembly.replace(doc) {
        old.release(&mut session.kernel);
    }
    failures
}

pub(crate) fn new_assembly(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let path = args::path(args, "path")?;
    let mut file = AssemblyFile::new();
    file.properties.part_number = args::opt_str(args, "part_number")?
        .map(str::to_owned)
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    set_assembly(
        session,
        AssemblyDoc {
            path: Some(path),
            file,
            dir,
            loaded: BTreeMap::new(),
            equation_results: Default::default(),
            changed_parts: Default::default(),
        },
    );
    Ok(ToolOutput::text("new assembly"))
}

pub(crate) fn open_assembly(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let path = args::path(args, "path")?;
    let file = load_assembly(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let failures = set_assembly(
        session,
        AssemblyDoc {
            path: Some(path),
            file,
            dir,
            loaded: BTreeMap::new(),
            equation_results: Default::default(),
            changed_parts: Default::default(),
        },
    );
    // As the application does: the parts may have changed since the assembly was saved (a
    // linked equations file, a part edited on its own), and the mates move what follows.
    solve(session)?;
    let mut out = info(session)?;
    if !failures.is_empty() {
        out.set("not_loaded", json!(failures));
    }
    Ok(ToolOutput::json(&out))
}

/// Evaluates the assembly's equations (driving mates and parts), then solves the mates and
/// moves the components where they say.
pub(crate) fn solve(session: &mut Session) -> Result<oxyde_assembly::Solution> {
    let doc = session.assembly.as_mut().ok_or("no assembly is open")?;
    doc.evaluate_equations(&mut session.kernel);
    let mut assembly = std::mem::take(&mut doc.file.assembly);
    let size = doc.size();
    let solution =
        oxyde_assembly::solve_about(&assembly, |r| doc.local(r), |c| doc.center(c), size);
    assembly.apply_solution(&solution);
    doc.file.assembly = assembly;
    Ok(solution)
}

pub(crate) fn save_assembly(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let doc = session.assembly.as_mut().ok_or("no assembly is open")?;
    let path = match args::opt_str(args, "path")? {
        Some(p) => PathBuf::from(p),
        None => doc.path.clone().ok_or("give a path")?,
    };
    write_assembly(&path, &doc.file).map_err(|e| format!("{}: {e}", path.display()))?;
    doc.path = Some(path.clone());
    // The parts its equations changed are saved with it, as the application does.
    let parts = doc.save_changed_parts()?;
    Ok(ToolOutput::json(
        &json!({ "saved": path.display().to_string(), "parts_saved": parts }),
    ))
}

pub(crate) fn add_component(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let placement = assembly::placement(args)?;
    let fixed = args::opt(args, "fixed")
        .map(|_| args::bool(args, "fixed", false))
        .transpose()?;
    let name = args::opt_str(args, "name")?.map(str::to_owned);
    let file = args::path(args, "file")?;
    let dir = doc(session)?.dir.clone();
    let absolute = if file.is_absolute() {
        file.clone()
    } else {
        dir.join(&file)
    };
    let (_, document_id) =
        document_kind(&absolute).map_err(|e| format!("{}: {e}", absolute.display()))?;
    let loaded = assembly::load(&mut session.kernel, &absolute, 0)?;
    let relative = assembly::relative(&dir, &absolute);
    let stem = absolute
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Component".to_owned());
    let doc = session.assembly.as_mut().ok_or("no assembly is open")?;
    let id = doc
        .file
        .assembly
        .add_component(&stem, relative, document_id, placement);
    if let Some(c) = doc.file.assembly.component_mut(id) {
        if let Some(fixed) = fixed {
            c.fixed = fixed;
        }
        if let Some(name) = name {
            c.name = name;
        }
        apply_look(c, args)?;
    }
    doc.loaded.insert(id, loaded);
    let component = doc.file.assembly.component(id).map(|c| c.name.clone());
    Ok(ToolOutput::json(
        &json!({ "component": component, "id": id.0 }),
    ))
}

pub(crate) fn place_component(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let doc = session.assembly.as_mut().ok_or("no assembly is open")?;
    let id = doc.component(args::get(args, "component")?)?;
    let c = doc
        .file
        .assembly
        .component_mut(id)
        .ok_or("no such component")?;
    if args::opt(args, "position").is_some() || args::opt(args, "rotation").is_some() {
        let mut placement = c.placement;
        if let Some(p) = args::opt(args, "position") {
            placement.translation = args::vec3(p, "\"position\"")?;
        }
        if args::opt(args, "rotation").is_some() {
            placement.rotation = assembly::rotation(args::opt(args, "rotation"))?;
        }
        c.placement = placement;
    }
    if args::opt(args, "fixed").is_some() {
        c.fixed = args::bool(args, "fixed", false)?;
    }
    apply_look(c, args)?;
    Ok(ToolOutput::json(&component_json(c)))
}

/// `hidden`, and `color` (`[r, g, b]`, or null to clear) with `transparency`, from a
/// tool's arguments onto a component.
fn apply_look(c: &mut oxyde_assembly::Component, args: &Value) -> Result<()> {
    if args::opt(args, "hidden").is_some() {
        c.hidden = args::bool(args, "hidden", false)?;
    }
    match args.get("color") {
        None => {}
        Some(Value::Null) => c.appearance = None,
        Some(color) => {
            let p = args::point3(color, "\"color\"")?;
            let byte = |v: f64| v.round().clamp(0.0, 255.0) as u8;
            c.appearance = Some(oxyde_model::Appearance {
                color: [byte(p.x), byte(p.y), byte(p.z)],
                transparency: args::opt_f64(args, "transparency")?
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
                texture: None,
            });
        }
    }
    Ok(())
}

pub(crate) fn remove_component(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let doc = session.assembly.as_mut().ok_or("no assembly is open")?;
    let id = doc.component(args::get(args, "component")?)?;
    let removed = doc.file.assembly.remove_component(id);
    if let Some(loaded) = doc.loaded.remove(&id) {
        release(loaded, &mut session.kernel);
    }
    Ok(ToolOutput::json(
        &json!({ "removed": id.0, "mates_removed": removed }),
    ))
}

fn release(loaded: Loaded, kernel: &mut oxyde_occt::OcctKernel) {
    match loaded {
        Loaded::Part(mut p) => p.part.release(kernel),
        Loaded::Assembly(a) => {
            for (_, child) in a.children {
                release(child, kernel);
            }
        }
    }
}

pub(crate) fn add_mate(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let kind = match args::str(args, "kind")? {
        "coincident" => MateKind::Coincident,
        "concentric" => MateKind::Concentric,
        "parallel" => MateKind::Parallel,
        "perpendicular" => MateKind::Perpendicular,
        "distance" => MateKind::Distance(args::f64(args, "value")?),
        "angle" => MateKind::Angle(args::f64(args, "value")?.to_radians()),
        other => return Err(format!("unknown mate kind \"{other}\"")),
    };
    let doc_ref = doc(session)?;
    let a: MateRef = assembly::mate_ref(doc_ref, &session.kernel, args::get(args, "a")?)?;
    let b: MateRef = assembly::mate_ref(doc_ref, &session.kernel, args::get(args, "b")?)?;
    let aligned = match args::opt(args, "aligned") {
        Some(v) => v.as_bool().ok_or("\"aligned\" must be true or false")?,
        None => {
            let placed = |r: &MateRef| {
                let primitive = doc_ref.local(r).ok()?;
                let placement = doc_ref
                    .file
                    .assembly
                    .placement_of(r.component)
                    .unwrap_or(Placement::IDENTITY);
                Some(primitive.placed(&placement))
            };
            match (placed(&a), placed(&b)) {
                (Some(pa), Some(pb)) => oxyde_assembly::default_alignment(&pa, &pb),
                _ => false,
            }
        }
    };
    let lock_rotation = match args::opt(args, "lock_rotation") {
        Some(v) => v
            .as_bool()
            .ok_or("\"lock_rotation\" must be true or false")?,
        None => false,
    };
    if lock_rotation {
        let kinds = (doc_ref.local(&a), doc_ref.local(&b));
        let (Ok(pa), Ok(pb)) = kinds else {
            return Err("\"lock_rotation\" needs both sides' geometry".to_owned());
        };
        if !kind.can_lock_rotation(pa.kind(), pb.kind()) {
            return Err(
                "\"lock_rotation\" is for a concentric mate between two axes (round faces, \
                 circular or straight edges, axes)"
                    .to_owned(),
            );
        }
    }
    let doc = session.assembly.as_mut().ok_or("no assembly is open")?;
    let id = doc.file.assembly.add_mate(kind, a, b, aligned);
    if let Some(m) = doc.file.assembly.mate_mut(id) {
        m.lock_rotation = lock_rotation;
    }
    let solution = solve(session)?;
    let doc = session.assembly.as_ref().ok_or("no assembly is open")?;
    let status = solution
        .status(id)
        .map(|s| format!("{s:?}"))
        .unwrap_or_else(|| "unknown".to_owned());
    let name = doc.file.assembly.mate(id).map(|m| m.name.clone());
    Ok(ToolOutput::json(
        &json!({ "mate": name, "status": status, "errors": solution.errors() }),
    ))
}

fn component_json(c: &oxyde_assembly::Component) -> Value {
    json!({
        "id": c.id.0,
        "name": c.name,
        "file": c.path,
        "position": args::round_vec(c.placement.translation),
        "rotation": c.placement.rotation.iter().map(|r| r.iter().map(|v| args::round(*v)).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "fixed": c.fixed,
        "hidden": c.hidden,
    })
}

fn info(session: &mut Session) -> Result<Value> {
    let doc = doc(session)?;
    let components: Vec<Value> = doc
        .file
        .assembly
        .components
        .iter()
        .map(component_json)
        .collect();
    let mates: Vec<Value> = doc
        .file
        .assembly
        .mates
        .iter()
        .map(|m| {
            let mut mate = json!({ "name": m.name, "kind": format!("{:?}", m.kind), "a": m.a.component.0, "b": m.b.component.0 });
            if let (true, Some(fields)) = (m.lock_rotation, mate.as_object_mut()) {
                fields.insert("lock_rotation".to_owned(), json!(true));
            }
            mate
        })
        .collect();
    let mut volume = 0.0;
    for (part, placement, _, _) in doc.leaves() {
        let _ = placement;
        if let Some(body) = part.part.body() {
            volume += body
                .shapes()
                .iter()
                .filter_map(|s| session.kernel.volume(*s).ok())
                .sum::<f64>();
        }
    }
    Ok(json!({
        "path": doc.path.as_ref().map(|p| p.display().to_string()),
        "components": components,
        "mates": mates,
        "parts": doc.leaves().len(),
        "volume": args::round(volume),
        "size": args::round(doc.size()),
        "assembly_id": ComponentId::ASSEMBLY.0,
    }))
}

/// The assembly, with what the mates leave free: `"free"` gives each component that can
/// still move its degrees of freedom (none listed: fully defined), `"mate_errors"` the mates
/// that cannot be met.
pub(crate) fn assembly_info(session: &mut Session) -> Result<ToolOutput> {
    let solution = solve(session)?;
    let mut out = info(session)?;
    let doc = doc(session)?;
    let name = |id: ComponentId| {
        doc.file
            .assembly
            .component(id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| id.0.to_string())
    };
    let free: serde_json::Map<String, Value> = solution
        .freedom
        .iter()
        .filter(|f| f.degrees > 0)
        .map(|f| (name(f.component), json!(f.degrees)))
        .collect();
    out.set("free", Value::Object(free));
    let errors: Vec<Value> = solution
        .mates
        .iter()
        .filter_map(|(id, status)| match status {
            oxyde_assembly::MateStatus::Error(e) => {
                let mate = doc.file.assembly.mate(*id).map(|m| m.name.clone());
                Some(json!([mate, e]))
            }
            oxyde_assembly::MateStatus::Ok => None,
        })
        .collect();
    out.set("mate_errors", json!(errors));
    if !doc.file.assembly.equations.is_empty() {
        out.set("equations", doc.equations_report());
    }
    Ok(ToolOutput::json(&out))
}
