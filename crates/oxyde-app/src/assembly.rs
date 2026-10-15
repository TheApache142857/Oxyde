//! The open assembly: the files of its components (parts and sub-assemblies, at any
//! depth) loaded and rebuilt, the mates solved, the scene shown in the viewport, the
//! selection and the undo history.
//!
//! Sub-assemblies are rigid: their components sit where their own file put them. Only the
//! top-level components move when the mates are solved.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use oxyde_assembly::scene::{self, MeshRange};
use oxyde_assembly::{
    Assembly, Component, ComponentId, MateId, MateRef, MateTarget, Placement, Primitive, Solution,
    paths,
};
use oxyde_format::{AssemblyFile, Properties};
use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{Kernel, Mesh};
use oxyde_model::{EdgeName, FaceName, Part};
use oxyde_render::Pick;

const UNDO_LIMIT: usize = 200;

/// A part file used by the assembly, rebuilt.
pub struct LoadedPart {
    pub document_id: String,
    pub properties: Properties,
    pub part: Part,
    pub mesh: Option<Mesh>,
    /// Corners of the part's bounding box, in its own coordinates.
    pub bounds: Option<(Point3, Point3)>,
}

/// A sub-assembly file used by the assembly.
pub struct LoadedAssembly {
    pub document_id: String,
    pub properties: Properties,
    pub assembly: Assembly,
    /// Where each of its components' files is.
    pub sources: HashMap<ComponentId, PathBuf>,
}

/// A component's file, loaded.
pub enum Loaded {
    Part(LoadedPart),
    Assembly(LoadedAssembly),
}

impl Loaded {
    pub fn properties(&self) -> &Properties {
        match self {
            Loaded::Part(p) => &p.properties,
            Loaded::Assembly(a) => &a.properties,
        }
    }

    pub fn document_id(&self) -> &str {
        match self {
            Loaded::Part(p) => &p.document_id,
            Loaded::Assembly(a) => &a.document_id,
        }
    }
}

/// A part as it appears in the assembly: a top-level component, and the components that
/// lead to it inside sub-assemblies (none for a part placed directly).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Occurrence {
    pub top: ComponentId,
    pub path: Vec<ComponentId>,
}

impl Occurrence {
    pub fn top(top: ComponentId) -> Self {
        Self {
            top,
            path: Vec::new(),
        }
    }
}

/// What can be selected in an assembly.
#[derive(Debug, Clone, PartialEq)]
pub enum AsmSelected {
    Component(ComponentId),
    Face(Occurrence, FaceName),
    Edge(Occurrence, EdgeName),
    Mate(MateId),
}

impl AsmSelected {
    /// The top-level component it belongs to.
    pub fn component(&self) -> Option<ComponentId> {
        match self {
            AsmSelected::Component(c) => Some(*c),
            AsmSelected::Face(o, _) | AsmSelected::Edge(o, _) => Some(o.top),
            AsmSelected::Mate(_) => None,
        }
    }

    /// The face or edge as a mate reference.
    pub fn mate_ref(&self) -> Option<MateRef> {
        let (o, target) = match self {
            AsmSelected::Face(o, f) => (o, MateTarget::Face(f.clone())),
            AsmSelected::Edge(o, e) => (o, MateTarget::Edge(e.clone())),
            _ => return None,
        };
        Some(MateRef {
            component: o.top,
            path: o.path.clone(),
            target,
        })
    }
}

pub struct AssemblyDoc {
    pub assembly: Assembly,
    pub properties: Properties,
    pub document_id: String,
    pub path: Option<PathBuf>,
    pub modified: bool,
    /// Bumped whenever what the viewport shows changes.
    pub revision: u64,
    /// Component files (parts and sub-assemblies, at every depth) by absolute path; `Err`
    /// when one could not be opened.
    pub docs: HashMap<PathBuf, Result<Loaded, String>>,
    /// Where each top-level component's file is.
    pub sources: HashMap<ComponentId, PathBuf>,
    /// The last solve: mate statuses and freedom.
    pub solution: Option<Solution>,
    pub selection: Vec<AsmSelected>,
    /// Components expanded in the tree.
    pub expanded: HashSet<ComponentId>,
    /// The combined mesh's faces and edges, per part occurrence, from the last scene.
    pub ranges: Vec<MeshRange<Occurrence>>,
    /// Mesh precision for parts, as a fraction of their size (from the preferences).
    pub mesh_fraction: f64,
    /// How long the last solve took, seconds.
    pub last_solve: Option<f64>,
    /// Volume of each part file (mm³), computed when Mass Properties first asks.
    pub volumes: HashMap<PathBuf, Option<f64>>,
    undo: Vec<Assembly>,
    redo: Vec<Assembly>,
}

/// Deeper than this, an assembly is surely including itself.
const MAX_DEPTH: usize = 32;

impl AssemblyDoc {
    pub fn new() -> Self {
        Self::from_file(AssemblyFile::new(), None)
    }

    pub fn from_file(file: AssemblyFile, path: Option<PathBuf>) -> Self {
        Self {
            assembly: file.assembly,
            properties: file.properties,
            document_id: file.document_id,
            path,
            modified: false,
            revision: crate::document::next_revision(),
            docs: HashMap::new(),
            sources: HashMap::new(),
            solution: None,
            selection: Vec::new(),
            expanded: HashSet::new(),
            ranges: Vec::new(),
            mesh_fraction: 0.001,
            last_solve: None,
            volumes: HashMap::new(),
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn to_file(&self) -> AssemblyFile {
        AssemblyFile {
            document_id: self.document_id.clone(),
            assembly: self.assembly.clone(),
            properties: self.properties.clone(),
        }
    }

    pub fn display_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_stem)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Assembly".to_owned())
    }

    pub fn folder(&self) -> Option<PathBuf> {
        self.path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    }

    // ---- Loading -------------------------------------------------------------------------

    /// Opens the component files not loaded yet, sub-assemblies with what they hold. A file
    /// that moved is looked for, by document id, in its assembly's folder. Returns messages
    /// about files found elsewhere or that could not be opened.
    pub fn load_parts<K: Kernel>(&mut self, kernel: &mut K) -> Vec<String> {
        let mut notes = Vec::new();
        let folder = self.folder();
        // The assembly itself is on the stack: a component may not contain it.
        let mut stack: Vec<PathBuf> = self.path.iter().cloned().collect();
        for i in 0..self.assembly.components.len() {
            let component = &self.assembly.components[i];
            let (path, moved) = locate(component, folder.as_deref());
            if let Some(found) = moved {
                notes.push(format!(
                    "{}: {} was not found, using {}",
                    component.name,
                    component.path,
                    found.display()
                ));
                self.assembly.components[i].path = paths::stored(&found, folder.as_deref());
                self.modified = true;
            }
            let component = &self.assembly.components[i];
            load_into(
                &mut self.docs,
                &path,
                &mut stack,
                kernel,
                &mut notes,
                &component.name,
                self.mesh_fraction,
            );
            self.sources.insert(component.id, path);
        }
        self.revision = crate::document::next_revision();
        notes
    }

    /// Reads every file again: after a part or sub-assembly was edited and saved.
    pub fn reload_parts<K: Kernel>(&mut self, kernel: &mut K) -> Vec<String> {
        self.release(kernel);
        self.load_parts(kernel)
    }

    pub fn release<K: Kernel>(&mut self, kernel: &mut K) {
        for (_, loaded) in self.docs.drain() {
            if let Ok(Loaded::Part(mut part)) = loaded {
                part.part.release(kernel);
            }
        }
        self.sources.clear();
        self.volumes.clear();
    }

    /// The loaded file of a top-level component.
    pub fn loaded(&self, component: ComponentId) -> Option<&Loaded> {
        let path = self.sources.get(&component)?;
        self.docs.get(path)?.as_ref().ok()
    }

    /// Why a top-level component's file could not be opened.
    pub fn load_error(&self, component: ComponentId) -> Option<&str> {
        let path = self.sources.get(&component)?;
        match self.docs.get(path) {
            Some(Ok(_)) => None,
            Some(Err(e)) => Some(e),
            // Never stored: it is this assembly, or holds it.
            None => Some("this file contains the assembly itself"),
        }
    }

    /// Follows `path` down from a top-level component: the file reached, the placement of
    /// that file in the component's coordinates, and the names along the way.
    fn walk(
        &self,
        top: ComponentId,
        path: &[ComponentId],
    ) -> Option<(&Loaded, Placement, Vec<String>)> {
        let mut loaded = self.loaded(top)?;
        let mut placement = Placement::IDENTITY;
        let mut names = Vec::new();
        for id in path {
            let Loaded::Assembly(sub) = loaded else {
                return None;
            };
            let c = sub.assembly.component(*id)?;
            placement = placement.compose(&c.placement);
            names.push(c.name.clone());
            loaded = self.docs.get(sub.sources.get(id)?)?.as_ref().ok()?;
        }
        Some((loaded, placement, names))
    }

    /// The part an occurrence shows.
    pub fn part_at(&self, o: &Occurrence) -> Option<&LoadedPart> {
        match self.walk(o.top, &o.path)?.0 {
            Loaded::Part(p) => Some(p),
            Loaded::Assembly(_) => None,
        }
    }

    /// The part files under a top-level component: its own file, or every part file a
    /// sub-assembly uses (each once).
    pub fn part_paths_under(&self, component: ComponentId) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Some(path) = self.sources.get(&component) else {
            return out;
        };
        self.collect_part_paths(path, 0, &mut out);
        out
    }

    fn collect_part_paths(&self, path: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > MAX_DEPTH {
            return;
        }
        match self.docs.get(path) {
            Some(Ok(Loaded::Part(_))) => {
                if !out.iter().any(|p| p == path) {
                    out.push(path.to_path_buf());
                }
            }
            Some(Ok(Loaded::Assembly(sub))) => {
                for inner in sub.sources.values() {
                    self.collect_part_paths(inner, depth + 1, out);
                }
            }
            _ => {}
        }
    }

    /// The material of the part a component is (its name), when it is a part with one.
    pub fn component_material(&self, component: ComponentId) -> Option<&str> {
        match self.loaded(component)? {
            Loaded::Part(p) => p.part.material().map(|m| m.name.as_str()),
            Loaded::Assembly(_) => None,
        }
    }

    /// The file an occurrence shows.
    pub fn path_at(&self, o: &Occurrence) -> Option<PathBuf> {
        let mut path = self.sources.get(&o.top)?.clone();
        for id in &o.path {
            let Some(Ok(Loaded::Assembly(sub))) = self.docs.get(&path) else {
                return None;
            };
            path = sub.sources.get(id)?.clone();
        }
        Some(path)
    }

    /// Names of the components inside sub-assemblies leading to an occurrence.
    pub fn inner_names(&self, top: ComponentId, path: &[ComponentId]) -> Vec<String> {
        self.walk(top, path).map(|w| w.2).unwrap_or_default()
    }

    /// Every part shown for `assembly`'s visible components, at its place in the assembly.
    pub fn leaves<'a>(
        &'a self,
        assembly: &'a Assembly,
    ) -> Vec<(Occurrence, Placement, &'a LoadedPart)> {
        let mut out = Vec::new();
        for c in assembly.components.iter().filter(|c| !c.hidden) {
            if let Some(loaded) = self.loaded(c.id) {
                self.collect(loaded, Occurrence::top(c.id), c.placement, 0, &mut out);
            }
        }
        out
    }

    fn collect<'a>(
        &'a self,
        loaded: &'a Loaded,
        occurrence: Occurrence,
        placement: Placement,
        depth: usize,
        out: &mut Vec<(Occurrence, Placement, &'a LoadedPart)>,
    ) {
        match loaded {
            Loaded::Part(p) => out.push((occurrence, placement, p)),
            Loaded::Assembly(sub) if depth < MAX_DEPTH => {
                for c in sub.assembly.components.iter().filter(|c| !c.hidden) {
                    let Some(Ok(inner)) = sub.sources.get(&c.id).and_then(|p| self.docs.get(p))
                    else {
                        continue;
                    };
                    let mut path = occurrence.path.clone();
                    path.push(c.id);
                    let next = Occurrence {
                        top: occurrence.top,
                        path,
                    };
                    self.collect(inner, next, placement.compose(&c.placement), depth + 1, out);
                }
            }
            Loaded::Assembly(_) => {}
        }
    }

    // ---- Solving -------------------------------------------------------------------------

    /// Mate geometry in the top-level component's own coordinates (the assembly's, for
    /// its own planes and origin).
    pub fn local(&self, r: &MateRef) -> Result<Primitive, String> {
        if r.component == ComponentId::ASSEMBLY {
            return match &r.target {
                MateTarget::Plane(_) | MateTarget::Axis(_) | MateTarget::Origin => {
                    oxyde_assembly::resolve(&r.target, None)
                }
                _ => Err("the assembly itself has only planes, axes and an origin".to_owned()),
            };
        }
        let (loaded, placement, _) = self
            .walk(r.component, &r.path)
            .ok_or_else(|| "the component's file could not be opened".to_owned())?;
        let primitive = match (loaded, &r.target) {
            (Loaded::Part(p), target) => oxyde_assembly::resolve(target, p.part.body())?,
            // A sub-assembly's own planes, axes and origin.
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

    /// Mate geometry placed in the assembly.
    pub fn placed(&self, assembly: &Assembly, r: &MateRef) -> Option<Primitive> {
        let placement = assembly.placement_of(r.component)?;
        Some(self.local(r).ok()?.placed(&placement))
    }

    /// About the size of the assembly, in mm.
    pub fn size(&self) -> f64 {
        let (lo, hi) = self.bounds(&self.assembly).unwrap_or((
            Point3::new(-50.0, -50.0, -50.0),
            Point3::new(50.0, 50.0, 50.0),
        ));
        (hi - lo).length().max(1.0)
    }

    /// Solves `assembly`'s mates in place and returns the result. Each component turns
    /// about the middle of its geometry, so a part modelled away from its origin does not
    /// swing across the assembly.
    pub fn solve_assembly(&self, assembly: &mut Assembly) -> Solution {
        let center = |c: ComponentId| {
            self.component_bounds(c).map_or(Point3::ORIGIN, |(lo, hi)| {
                Point3::new(
                    (lo.x + hi.x) / 2.0,
                    (lo.y + hi.y) / 2.0,
                    (lo.z + hi.z) / 2.0,
                )
            })
        };
        let solution =
            oxyde_assembly::solve_about(assembly, |r| self.local(r), center, self.size());
        for (id, placement) in &solution.placements {
            if let Some(c) = assembly.component_mut(*id) {
                c.placement = *placement;
            }
        }
        solution
    }

    /// Solves the document's own mates.
    pub fn solve(&mut self) {
        let start = std::time::Instant::now();
        let mut assembly = std::mem::take(&mut self.assembly);
        let solution = self.solve_assembly(&mut assembly);
        self.assembly = assembly;
        self.solution = Some(solution);
        self.last_solve = Some(start.elapsed().as_secs_f64());
        self.revision = crate::document::next_revision();
    }

    // ---- Display -------------------------------------------------------------------------

    /// Bounding box of the visible parts of `assembly`.
    pub fn bounds(&self, assembly: &Assembly) -> Option<(Point3, Point3)> {
        bounds_of(
            self.leaves(assembly)
                .into_iter()
                .filter_map(|(_, placement, p)| Some((placement, p.bounds?))),
        )
    }

    /// Bounding box of one component, in its own coordinates.
    pub fn component_bounds(&self, component: ComponentId) -> Option<(Point3, Point3)> {
        let loaded = self.loaded(component)?;
        let mut leaves = Vec::new();
        self.collect(
            loaded,
            Occurrence::top(component),
            Placement::IDENTITY,
            0,
            &mut leaves,
        );
        bounds_of(
            leaves
                .into_iter()
                .filter_map(|(_, placement, p)| Some((placement, p.bounds?))),
        )
    }

    /// The visible parts of `assembly` as one mesh; remembers which faces are whose.
    pub fn scene_mesh(&mut self, assembly: &Assembly) -> Mesh {
        let (mesh, ranges) = {
            let leaves = self.leaves(assembly);
            scene::combine(
                leaves
                    .iter()
                    .filter_map(|(o, placement, p)| Some((o.clone(), placement, p.mesh.as_ref()?))),
            )
        };
        self.ranges = ranges;
        mesh
    }

    /// What a pick in the viewport is.
    pub fn picked(&self, pick: Pick) -> Option<AsmSelected> {
        match pick {
            Pick::Face(i) => {
                let (o, face) = scene::face_owner(&self.ranges, i)?;
                let body = self.part_at(o)?.part.body()?;
                Some(AsmSelected::Face(
                    o.clone(),
                    body.naming.face(face)?.clone(),
                ))
            }
            Pick::Edge(i) => {
                let (o, edge) = scene::edge_owner(&self.ranges, i)?;
                let body = self.part_at(o)?.part.body()?;
                Some(AsmSelected::Edge(
                    o.clone(),
                    body.naming.edge(edge)?.clone(),
                ))
            }
        }
    }

    /// Faces and edges to highlight for selected things.
    pub fn picks_of<'a>(&self, items: impl IntoIterator<Item = &'a AsmSelected>) -> Vec<Pick> {
        let mut out = Vec::new();
        for item in items {
            match item {
                AsmSelected::Face(o, f) => {
                    if let Some(i) = self
                        .part_at(o)
                        .and_then(|p| p.part.body())
                        .and_then(|b| b.naming.find_face(f))
                        .and_then(|i| scene::face_index(&self.ranges, o, i))
                    {
                        out.push(Pick::Face(i));
                    }
                }
                AsmSelected::Edge(o, e) => {
                    if let Some(i) = self
                        .part_at(o)
                        .and_then(|p| p.part.body())
                        .and_then(|b| b.naming.find_edge(e))
                        .and_then(|i| scene::edge_index(&self.ranges, o, i))
                    {
                        out.push(Pick::Edge(i));
                    }
                }
                AsmSelected::Component(c) => {
                    for r in self.ranges.iter().filter(|r| r.owner.top == *c) {
                        out.extend(r.faces.clone().map(Pick::Face));
                    }
                }
                AsmSelected::Mate(_) => {}
            }
        }
        out
    }

    /// Highlight for a mate reference.
    pub fn picks_of_ref(&self, r: &MateRef) -> Vec<Pick> {
        let o = Occurrence {
            top: r.component,
            path: r.path.clone(),
        };
        let item = match &r.target {
            MateTarget::Face(f) => AsmSelected::Face(o, f.clone()),
            MateTarget::Edge(e) => AsmSelected::Edge(o, e.clone()),
            _ => return Vec::new(),
        };
        self.picks_of([&item])
    }

    // ---- Editing -------------------------------------------------------------------------

    /// Records the current state as one undo step.
    pub fn checkpoint(&mut self) {
        self.checkpoint_from(self.assembly.clone());
    }

    /// Records `before`, the assembly just before a change already made, as one undo step.
    pub fn checkpoint_from(&mut self, before: Assembly) {
        self.undo.push(before);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.modified = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Restores the previous state; the caller loads files and solves.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo
            .push(std::mem::replace(&mut self.assembly, previous));
        self.after_history();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.assembly, next));
        self.after_history();
        true
    }

    fn after_history(&mut self) {
        self.modified = true;
        let assembly = &self.assembly;
        self.selection.retain(|s| match s {
            AsmSelected::Mate(m) => assembly.mate(*m).is_some(),
            other => other
                .component()
                .is_some_and(|c| assembly.component(c).is_some()),
        });
    }

    /// Where a new component goes: beside what is there, so that it does not overlap.
    pub fn free_spot(&self, new_bounds: Option<(Point3, Point3)>) -> Placement {
        let Some((_, hi)) = self.bounds(&self.assembly) else {
            return Placement::IDENTITY;
        };
        let (lo, size) =
            new_bounds.map_or((Point3::ORIGIN, 0.0), |(lo, hi)| (lo, (hi - lo).length()));
        let gap = (size * 0.25).max(10.0);
        Placement::from_translation(Vec3::new(hi.x - lo.x + gap, 0.0, 0.0))
    }

    /// Bounding box of a loaded file, in its own coordinates.
    pub fn file_bounds(&self, path: &Path) -> Option<(Point3, Point3)> {
        let loaded = self.docs.get(path)?.as_ref().ok()?;
        let mut leaves = Vec::new();
        self.collect(
            loaded,
            Occurrence::top(ComponentId(0)),
            Placement::IDENTITY,
            0,
            &mut leaves,
        );
        bounds_of(
            leaves
                .into_iter()
                .filter_map(|(_, placement, p)| Some((placement, p.bounds?))),
        )
    }

    /// Loads a file to insert, with what it holds. The assembly being edited may not be in
    /// it.
    pub fn load_file<K: Kernel>(&mut self, path: &Path, kernel: &mut K) -> Result<(), String> {
        if self.path.as_deref() == Some(path) {
            return Err("an assembly cannot contain itself".to_owned());
        }
        let mut stack: Vec<PathBuf> = self.path.iter().cloned().collect();
        let mut notes = Vec::new();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let fraction = self.mesh_fraction;
        load_into(
            &mut self.docs,
            path,
            &mut stack,
            kernel,
            &mut notes,
            &name,
            fraction,
        );
        match self.docs.get(path) {
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => {
                let e = e.clone();
                // Not kept: inserting it again should try again.
                self.docs.remove(path);
                Err(e)
            }
            None => Err(format!("{} could not be opened", path.display())),
        }
    }

    /// Stored path for a component file, given where the assembly is.
    pub fn stored_path(&self, path: &Path) -> String {
        paths::stored(path, self.folder().as_deref())
    }

    /// Re-stores every component path for a new assembly location.
    pub fn relocate(&mut self, new_path: &Path) {
        let folder = new_path.parent();
        for c in &mut self.assembly.components {
            if let Some(source) = self.sources.get(&c.id) {
                c.path = paths::stored(source, folder);
            }
        }
    }

    pub fn component(&self, id: ComponentId) -> Option<&Component> {
        self.assembly.component(id)
    }

    /// Key and content of a component's file, for the bill of materials: `parent` is the
    /// key of the assembly holding it, "" for this one.
    pub fn bom_open<'a>(&'a self, c: &Component, parent: &str) -> (String, Option<&'a Assembly>) {
        let sources = if parent.is_empty() {
            Some(&self.sources)
        } else {
            match self.docs.get(Path::new(parent)) {
                Some(Ok(Loaded::Assembly(sub))) => Some(&sub.sources),
                _ => None,
            }
        };
        let Some(path) = sources.and_then(|s| s.get(&c.id)) else {
            return (c.path.clone(), None);
        };
        let sub = match self.docs.get(path) {
            Some(Ok(Loaded::Assembly(sub))) => Some(&sub.assembly),
            _ => None,
        };
        (path.to_string_lossy().into_owned(), sub)
    }
}

fn bounds_of(
    boxes: impl Iterator<Item = (Placement, (Point3, Point3))>,
) -> Option<(Point3, Point3)> {
    let mut out: Option<(Point3, Point3)> = None;
    for (placement, (lo, hi)) in boxes {
        for corner in corners(lo, hi) {
            let p = placement.apply_point(corner);
            out = Some(match out {
                None => (p, p),
                Some((a, b)) => (
                    Point3::new(a.x.min(p.x), a.y.min(p.y), a.z.min(p.z)),
                    Point3::new(b.x.max(p.x), b.y.max(p.y), b.z.max(p.z)),
                ),
            });
        }
    }
    out
}

fn corners(lo: Point3, hi: Point3) -> [Point3; 8] {
    let mut out = [lo; 8];
    for (k, c) in out.iter_mut().enumerate() {
        *c = Point3::new(
            if k & 1 == 0 { lo.x } else { hi.x },
            if k & 2 == 0 { lo.y } else { hi.y },
            if k & 4 == 0 { lo.z } else { hi.z },
        );
    }
    out
}

/// Where a component's file is: its stored path, or, when that is gone, a file with its
/// document id in the assembly's folder (returned second, to report it).
fn locate(component: &Component, folder: Option<&Path>) -> (PathBuf, Option<PathBuf>) {
    let path = paths::absolute(&component.path, folder);
    if path.exists() {
        return (path, None);
    }
    match find_by_id(folder, &component.document_id) {
        Some(found) => (found.clone(), Some(found)),
        None => (path, None),
    }
}

/// Loads `path` (and, for an assembly, what it holds) into `docs` unless it is there.
/// `stack` holds the assemblies being loaded, to catch one that contains itself.
fn load_into<K: Kernel>(
    docs: &mut HashMap<PathBuf, Result<Loaded, String>>,
    path: &Path,
    stack: &mut Vec<PathBuf>,
    kernel: &mut K,
    notes: &mut Vec<String>,
    name: &str,
    fraction: f64,
) {
    if stack.iter().any(|p| p == path) {
        // Not stored: the assembly holding it is refused instead (see below).
        notes.push(format!(
            "{name}: {} contains the assembly itself",
            path.display()
        ));
        return;
    }
    if docs.contains_key(path) {
        return;
    }
    let loaded = if crate::assembly_ui::is_assembly(path) {
        match oxyde_format::load_assembly(path) {
            Ok(file) => {
                let folder = path.parent().map(Path::to_path_buf);
                let mut sources = HashMap::new();
                stack.push(path.to_path_buf());
                for c in &file.assembly.components {
                    let (inner, moved) = locate(c, folder.as_deref());
                    if let Some(found) = moved {
                        notes.push(format!(
                            "{name} › {}: {} was not found, using {}",
                            c.name,
                            c.path,
                            found.display()
                        ));
                    }
                    load_into(
                        docs,
                        &inner,
                        stack,
                        kernel,
                        notes,
                        &format!("{name} › {}", c.name),
                        fraction,
                    );
                    sources.insert(c.id, inner);
                }
                stack.pop();
                // A component that could not be stored is the assembly coming round again.
                let loops = sources.values().any(|p| !docs.contains_key(p));
                if loops {
                    Err(format!(
                        "{} contains the assembly being edited",
                        path.display()
                    ))
                } else {
                    Ok(Loaded::Assembly(LoadedAssembly {
                        document_id: file.document_id,
                        properties: file.properties,
                        assembly: file.assembly,
                        sources,
                    }))
                }
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    } else {
        load(path, kernel, fraction).map(Loaded::Part)
    };
    if let Err(e) = &loaded {
        notes.push(format!("{name}: {e}"));
    }
    docs.insert(path.to_path_buf(), loaded);
}

/// Opens and rebuilds a part file, and meshes it.
pub fn load<K: Kernel>(path: &Path, kernel: &mut K, fraction: f64) -> Result<LoadedPart, String> {
    let file = oxyde_format::load_part(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut part = Part::from_definition(file.definition).map_err(|e| e.to_string())?;
    part.rebuild(kernel);
    let (mesh, bounds) = match part.body() {
        Some(body) => {
            let mesh = crate::app::tessellate_bodies(kernel, body, fraction).ok();
            let bounds = mesh.as_ref().and_then(mesh_bounds);
            (mesh, bounds)
        }
        None => (None, None),
    };
    Ok(LoadedPart {
        document_id: file.document_id,
        properties: file.properties,
        part,
        mesh,
        bounds,
    })
}

fn mesh_bounds(mesh: &Mesh) -> Option<(Point3, Point3)> {
    let mut points = mesh
        .faces
        .iter()
        .flat_map(|f| f.positions.iter())
        .map(|p| Point3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2])));
    let first = points.next()?;
    Some(points.fold((first, first), |(a, b), p| {
        (
            Point3::new(a.x.min(p.x), a.y.min(p.y), a.z.min(p.z)),
            Point3::new(b.x.max(p.x), b.y.max(p.y), b.z.max(p.z)),
        )
    }))
}

/// A part or assembly file with this document id in `folder` or one level below.
fn find_by_id(folder: Option<&Path>, document_id: &str) -> Option<PathBuf> {
    let folder = folder?;
    if document_id.is_empty() {
        return None;
    }
    let is_document = |p: &Path| {
        p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            [
                oxyde_format::PART_EXTENSION,
                oxyde_format::LEGACY_PART_EXTENSION,
                oxyde_format::ASSEMBLY_EXTENSION,
            ]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
        })
    };
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(folder).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Ok(inner) = std::fs::read_dir(&p) {
                candidates.extend(inner.flatten().map(|e| e.path()).filter(|p| is_document(p)));
            }
        } else if is_document(&p) {
            candidates.push(p);
        }
    }
    candidates.sort();
    candidates
        .into_iter()
        .find(|p| oxyde_format::document_kind(p).is_ok_and(|(_, id)| id == document_id))
}
