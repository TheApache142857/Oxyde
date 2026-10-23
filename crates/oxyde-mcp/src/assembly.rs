//! The assembly being put together: its file, and its components' parts and
//! sub-assemblies loaded (rebuilt and meshed) for pictures, mates and measures.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use oxyde_assembly::{ComponentId, MateRef, MateTarget, Placement, Primitive};
use oxyde_format::{AssemblyFile, DocumentKind, document_kind, load_assembly, load_part};
use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{Kernel, Mesh};
use oxyde_model::{Part, StandardAxis, StandardPlane};
use oxyde_occt::OcctKernel;
use serde_json::Value;

use crate::args::{self, Result};
use crate::geometry::Solids;

/// A part file rebuilt and meshed.
pub(crate) struct LoadedPart {
    /// The part file: instances of a part are loaded one by one, and share it.
    pub file: PathBuf,
    pub part: Part,
    pub mesh: Mesh,
    pub bounds: Option<(Point3, Point3)>,
}

/// A sub-assembly file and what its components load.
pub(crate) struct LoadedAssembly {
    pub file: AssemblyFile,
    pub children: BTreeMap<ComponentId, Loaded>,
}

pub(crate) enum Loaded {
    Part(Box<LoadedPart>),
    Assembly(Box<LoadedAssembly>),
}

/// A component's look on the way to a part: its colour and transparency.
pub(crate) type Look = Option<([u8; 3], f64)>;

/// A part occurrence: the part, where it sits, its look and its path of component names.
pub(crate) type Leaf<'a> = (&'a LoadedPart, Placement, Look, String);

impl Loaded {
    fn release(self, kernel: &mut OcctKernel) {
        match self {
            Loaded::Part(mut p) => p.part.release(kernel),
            Loaded::Assembly(a) => {
                for (_, child) in a.children {
                    child.release(kernel);
                }
            }
        }
    }
}

/// The assembly being worked on.
pub(crate) struct AssemblyDoc {
    pub path: Option<PathBuf>,
    pub file: AssemblyFile,
    /// The folder component paths are relative to.
    pub dir: PathBuf,
    pub loaded: BTreeMap<ComponentId, Loaded>,
    /// The last evaluation of the assembly's equations.
    pub equation_results: oxyde_model::EquationResults,
    /// Part files the assembly's equations changed, saved with it.
    pub changed_parts: BTreeSet<PathBuf>,
}

/// Sub-assemblies nest at most this deep, so a file that contains itself is refused.
const MAX_DEPTH: usize = 16;

impl AssemblyDoc {
    pub fn release(self, kernel: &mut OcctKernel) {
        for (_, loaded) in self.loaded {
            loaded.release(kernel);
        }
    }

    /// Loads every component that is not loaded yet; returns the ones that failed.
    pub fn load_all(&mut self, kernel: &mut OcctKernel) -> Vec<String> {
        let mut failures = Vec::new();
        let components: Vec<(ComponentId, String)> = self
            .file
            .assembly
            .components
            .iter()
            .map(|c| (c.id, c.path.clone()))
            .collect();
        for (id, path) in components {
            if self.loaded.contains_key(&id) {
                continue;
            }
            match load(kernel, &self.dir.join(&path), 0) {
                Ok(loaded) => {
                    self.loaded.insert(id, loaded);
                }
                Err(e) => failures.push(format!("{path}: {e}")),
            }
        }
        failures
    }

    /// Every part occurrence: the part, where it sits in the assembly, and the colour of
    /// the outermost component appearance on the way to it.
    pub fn leaves(&self) -> Vec<Leaf<'_>> {
        let mut out = Vec::new();
        for c in &self.file.assembly.components {
            if c.hidden {
                continue;
            }
            if let Some(loaded) = self.loaded.get(&c.id) {
                let color = c.appearance.as_ref().map(|a| (a.color, a.transparency));
                collect(loaded, c.placement, color, c.name.clone(), 0, &mut out);
            }
        }
        out
    }

    /// The component a tool names: by name (`"tray<1>"`), id, or name without the
    /// instance number when there is one such component.
    pub fn component(&self, value: &Value) -> Result<ComponentId> {
        let components = &self.file.assembly.components;
        if let Some(n) = value.as_u64() {
            if n == 0 {
                return Ok(ComponentId::ASSEMBLY);
            }
            return components
                .iter()
                .find(|c| u64::from(c.id.0) == n)
                .map(|c| c.id)
                .ok_or_else(|| format!("no component with id {n}"));
        }
        let name = value
            .as_str()
            .ok_or("a component is named by its name or id")?;
        if name == "assembly" {
            return Ok(ComponentId::ASSEMBLY);
        }
        if let Some(c) = components.iter().find(|c| c.name == name) {
            return Ok(c.id);
        }
        let stem: Vec<_> = components
            .iter()
            .filter(|c| c.name.split('<').next() == Some(name))
            .collect();
        match stem.as_slice() {
            [one] => Ok(one.id),
            [] => Err(format!("no component named \"{name}\"")),
            _ => Err(format!(
                "several components are called \"{name}\": give the instance, \"{name}<1>\""
            )),
        }
    }

    /// Mate geometry in the component's own coordinates.
    pub fn local(&self, r: &MateRef) -> std::result::Result<Primitive, String> {
        if r.component == ComponentId::ASSEMBLY {
            return oxyde_assembly::resolve(&r.target, None);
        }
        let mut loaded = self
            .loaded
            .get(&r.component)
            .ok_or("the component's file could not be opened")?;
        let mut placement = Placement::IDENTITY;
        for step in &r.path {
            let Loaded::Assembly(a) = loaded else {
                return Err("the mate's path goes into a part".to_owned());
            };
            let c = a
                .file
                .assembly
                .component(*step)
                .ok_or("the mate's path names a missing component")?;
            placement = placement.compose(&c.placement);
            loaded = a
                .children
                .get(step)
                .ok_or("a component of the path is not loaded")?;
        }
        let primitive = match (loaded, &r.target) {
            (Loaded::Part(p), target) => oxyde_assembly::resolve(target, p.part.body())?,
            (
                Loaded::Assembly(_),
                t @ (MateTarget::Plane(_) | MateTarget::Axis(_) | MateTarget::Origin),
            ) => oxyde_assembly::resolve(t, None)?,
            (Loaded::Assembly(_), _) => {
                return Err("faces and edges are picked on a part of the sub-assembly".to_owned());
            }
        };
        Ok(primitive.placed(&placement))
    }

    /// About the size of the assembly, mm.
    pub fn size(&self) -> f64 {
        let mut lo = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
        let mut hi = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (part, placement, _, _) in self.leaves() {
            if let Some((a, b)) = part.bounds {
                for p in corners(a, b) {
                    let q = placement.apply_point(p);
                    lo = Point3::new(lo.x.min(q.x), lo.y.min(q.y), lo.z.min(q.z));
                    hi = Point3::new(hi.x.max(q.x), hi.y.max(q.y), hi.z.max(q.z));
                }
            }
        }
        let d = (hi - lo).length();
        if d.is_finite() && d > 0.0 { d } else { 100.0 }
    }

    /// The middle of a component's geometry, in its own coordinates.
    pub fn center(&self, id: ComponentId) -> Point3 {
        match self.loaded.get(&id) {
            Some(Loaded::Part(part)) => part.bounds.map_or(Point3::ORIGIN, |(a, b)| {
                Point3::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0, (a.z + b.z) / 2.0)
            }),
            _ => Point3::ORIGIN,
        }
    }
}

fn corners(a: Point3, b: Point3) -> [Point3; 8] {
    [
        Point3::new(a.x, a.y, a.z),
        Point3::new(b.x, a.y, a.z),
        Point3::new(a.x, b.y, a.z),
        Point3::new(b.x, b.y, a.z),
        Point3::new(a.x, a.y, b.z),
        Point3::new(b.x, a.y, b.z),
        Point3::new(a.x, b.y, b.z),
        Point3::new(b.x, b.y, b.z),
    ]
}

fn collect<'a>(
    loaded: &'a Loaded,
    placement: Placement,
    color: Option<([u8; 3], f64)>,
    name: String,
    depth: usize,
    out: &mut Vec<Leaf<'a>>,
) {
    match loaded {
        Loaded::Part(p) => out.push((p, placement, color, name)),
        Loaded::Assembly(a) if depth < MAX_DEPTH => {
            for c in &a.file.assembly.components {
                if c.hidden {
                    continue;
                }
                if let Some(child) = a.children.get(&c.id) {
                    let inner = color.or(c.appearance.as_ref().map(|x| (x.color, x.transparency)));
                    collect(
                        child,
                        placement.compose(&c.placement),
                        inner,
                        format!("{name}/{}", c.name),
                        depth + 1,
                        out,
                    );
                }
            }
        }
        Loaded::Assembly(_) => {}
    }
}

/// Loads a part or an assembly file, rebuilding parts and following sub-assemblies.
pub(crate) fn load(kernel: &mut OcctKernel, path: &Path, depth: usize) -> Result<Loaded> {
    if depth > MAX_DEPTH {
        return Err("sub-assemblies nest too deep (does an assembly contain itself?)".to_owned());
    }
    let (kind, _) = document_kind(path).map_err(|e| format!("{}: {e}", path.display()))?;
    match kind {
        DocumentKind::Part => {
            let file = load_part(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut part = Part::from_definition(file.definition).map_err(|e| e.to_string())?;
            part.rebuild(kernel);
            let (mesh, bounds) = match part.body() {
                Some(body) => {
                    let solids = Solids::of_body(body);
                    (solids.mesh(kernel)?, solids.bounds(kernel))
                }
                None => (Mesh::default(), None),
            };
            Ok(Loaded::Part(Box::new(LoadedPart {
                file: path.to_path_buf(),
                part,
                mesh,
                bounds,
            })))
        }
        DocumentKind::Assembly => {
            let file = load_assembly(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let mut children = BTreeMap::new();
            for c in &file.assembly.components {
                if let Ok(child) = load(kernel, &dir.join(&c.path), depth + 1) {
                    children.insert(c.id, child);
                }
            }
            Ok(Loaded::Assembly(Box::new(LoadedAssembly {
                file,
                children,
            })))
        }
        _ => Err(format!(
            "{} is neither a part nor an assembly",
            path.display()
        )),
    }
}

/// A rotation: a 3 x 3 matrix (rows), `{"axis": "x" | [x, y, z], "angle": deg}`, or a
/// list of those applied one after the other.
pub(crate) fn rotation(value: Option<&Value>) -> Result<[[f64; 3]; 3]> {
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let Some(value) = value else {
        return Ok(identity);
    };
    // A matrix: three rows of three numbers.
    if let Some(rows) = value.as_array()
        && rows.len() == 3
        && rows.iter().all(|r| {
            r.as_array()
                .is_some_and(|r| r.len() == 3 && r.iter().all(Value::is_number))
        })
    {
        let mut m = identity;
        for (i, row) in rows.iter().enumerate() {
            let p = args::point3(row, "a rotation row")?;
            if let Some(out) = m.get_mut(i) {
                *out = [p.x, p.y, p.z];
            }
        }
        return Ok(m);
    }
    let mut m = identity;
    for step in args::list(value) {
        let axis = crate::select::direction(args::get(step, "axis")?)?;
        let angle = args::f64(step, "angle")?.to_radians();
        let r = oxyde_geom::Transform::rotation(
            &oxyde_geom::Axis {
                origin: Point3::ORIGIN,
                direction: axis,
            },
            angle,
        );
        let turn = [
            [r.rows[0][0], r.rows[0][1], r.rows[0][2]],
            [r.rows[1][0], r.rows[1][1], r.rows[1][2]],
            [r.rows[2][0], r.rows[2][1], r.rows[2][2]],
        ];
        m = multiply(&turn, &m);
    }
    Ok(m)
}

fn multiply(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3)
                .map(|k| {
                    a.get(i).and_then(|r| r.get(k)).copied().unwrap_or(0.0)
                        * b.get(k).and_then(|r| r.get(j)).copied().unwrap_or(0.0)
                })
                .sum();
        }
    }
    out
}

/// A placement from `position` and `rotation` arguments.
pub(crate) fn placement(args: &Value) -> Result<Placement> {
    let translation = match args::opt(args, "position") {
        Some(p) => args::vec3(p, "\"position\"")?,
        None => Vec3::ZERO,
    };
    Ok(Placement {
        rotation: rotation(args::opt(args, "rotation"))?,
        translation,
    })
}

/// A mate reference from `{"component": …, "face" | "edge": selector}` or
/// `{"component": …, "plane": "front" | "top" | "right"}`, `"axis": "x"…`, `"origin": true`.
/// `"path": [names or ids]` reaches into a sub-assembly component: the components of the
/// sub-assembly, then of its sub-assemblies, down to the part whose geometry is meant;
/// selectors then work in that part's own coordinates.
pub(crate) fn mate_ref(doc: &AssemblyDoc, kernel: &OcctKernel, value: &Value) -> Result<MateRef> {
    let component = doc.component(args::get(value, "component")?)?;
    let mut path = Vec::new();
    let mut loaded = doc.loaded.get(&component);
    if let Some(steps) = args::opt(value, "path") {
        let steps = steps
            .as_array()
            .ok_or("\"path\" must be a list of component names or ids")?;
        for step in steps {
            let Some(Loaded::Assembly(inner)) = loaded else {
                return Err("\"path\" goes on past a part".to_owned());
            };
            let id = component_in(&inner.file.assembly, step)?;
            path.push(id);
            loaded = inner.children.get(&id);
        }
    }
    let target = if let Some(plane) = args::opt_str(value, "plane")? {
        MateTarget::Plane(match plane {
            "front" => StandardPlane::Front,
            "top" => StandardPlane::Top,
            "right" => StandardPlane::Right,
            other => return Err(format!("unknown plane \"{other}\"")),
        })
    } else if let Some(axis) = args::opt_str(value, "axis")? {
        MateTarget::Axis(match axis {
            "x" => StandardAxis::X,
            "y" => StandardAxis::Y,
            "z" => StandardAxis::Z,
            other => return Err(format!("unknown axis \"{other}\"")),
        })
    } else if args::bool(value, "origin", false)? {
        MateTarget::Origin
    } else {
        let part = match loaded {
            Some(Loaded::Part(p)) => p,
            Some(Loaded::Assembly(_)) => {
                return Err(
                    "that component is a sub-assembly: give \"path\" down to one of its \
                     parts, or mate it by its planes or origin"
                        .to_owned(),
                );
            }
            None => return Err("that component's file is not loaded".to_owned()),
        };
        let body = part.part.body().ok_or("that part has no solid")?;
        let solids = Solids::of_body(body);
        let scope = crate::select::Scope {
            solids: &solids,
            mesh: &part.mesh,
            part: Some(&part.part),
        };
        let _ = kernel;
        if let Some(face) = args::opt(value, "face") {
            let picked = scope.select(crate::select::Kind::Face, face)?;
            let names = scope.face_names(&picked)?;
            MateTarget::Face(names.into_iter().next().ok_or("no face")?)
        } else if let Some(edge) = args::opt(value, "edge") {
            let picked = scope.select(crate::select::Kind::Edge, edge)?;
            let names = scope.edge_names(&picked)?;
            MateTarget::Edge(names.into_iter().next().ok_or("no edge")?)
        } else {
            return Err(
                "a mate side needs \"face\", \"edge\", \"plane\", \"axis\" or \"origin\""
                    .to_owned(),
            );
        }
    };
    Ok(MateRef {
        component,
        path,
        target,
    })
}

/// A component of `assembly` by id or name ("PSU body<1>", or "PSU body" when unique).
fn component_in(assembly: &oxyde_assembly::Assembly, value: &Value) -> Result<ComponentId> {
    if let Some(n) = value.as_u64() {
        return assembly
            .components
            .iter()
            .find(|c| u64::from(c.id.0) == n)
            .map(|c| c.id)
            .ok_or_else(|| format!("no component {n} in the sub-assembly"));
    }
    let name = value
        .as_str()
        .ok_or("a path step is a component name or id")?;
    let matches: Vec<ComponentId> = assembly
        .components
        .iter()
        .filter(|c| c.name == name || c.name.split('<').next() == Some(name))
        .map(|c| c.id)
        .collect();
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("no component named \"{name}\" in the sub-assembly")),
        _ => Err(format!(
            "several components are called \"{name}\": give \"{name}<1>\""
        )),
    }
}

/// The component path, relative to the assembly's folder when it is inside it.
pub(crate) fn relative(dir: &Path, file: &Path) -> String {
    let absolute = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (dir, file) = (absolute(dir), absolute(file));
    match file.strip_prefix(&dir) {
        Ok(rest) => rest.to_string_lossy().replace('\\', "/"),
        Err(_) => file.to_string_lossy().into_owned(),
    }
}

/// Meshes of every visible part occurrence, placed.
pub(crate) fn placed_meshes(doc: &AssemblyDoc) -> Vec<(Mesh, Look, String)> {
    doc.leaves()
        .into_iter()
        .map(|(part, placement, color, name)| {
            (
                oxyde_assembly::scene::placed(&part.mesh, &placement),
                color,
                name,
            )
        })
        .collect()
}

/// Every visible part occurrence's solids, placed, as copies the caller releases.
pub(crate) fn placed_solids(
    kernel: &mut OcctKernel,
    doc: &AssemblyDoc,
) -> Vec<(String, Vec<oxyde_kernel::ShapeHandle>)> {
    let mut out = Vec::new();
    for (part, placement, _, name) in doc.leaves() {
        let Some(body) = part.part.body() else {
            continue;
        };
        let transform = placement.transform();
        let copies: Vec<_> = body
            .shapes()
            .into_iter()
            .filter_map(|s| kernel.transform(s, &transform).ok().map(|r| r.shape))
            .collect();
        out.push((name, copies));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rotations_compose_in_order() {
        let r = rotation(Some(&json!({"axis": "z", "angle": 90}))).unwrap();
        // +x goes to +y.
        assert!((r[1][0] - 1.0).abs() < 1e-12 && r[0][0].abs() < 1e-12);
        let r = rotation(Some(
            &json!([{"axis": "z", "angle": 90}, {"axis": "x", "angle": 90}]),
        ))
        .unwrap();
        // +x → +y → +z.
        assert!((r[2][0] - 1.0).abs() < 1e-12);
        let m = rotation(Some(&json!([[1, 0, 0], [0, 0, -1], [0, 1, 0]]))).unwrap();
        assert_eq!(m[1][2], -1.0);
    }
}
