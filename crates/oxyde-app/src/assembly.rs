//! The open assembly: its components' part files, loaded and rebuilt, the mates solved,
//! the scene shown in the viewport, the selection and the undo history.

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

use crate::app::deflection;

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

/// What can be selected in an assembly.
#[derive(Debug, Clone, PartialEq)]
pub enum AsmSelected {
    Component(ComponentId),
    Face(ComponentId, FaceName),
    Edge(ComponentId, EdgeName),
    Mate(MateId),
}

impl AsmSelected {
    pub fn component(&self) -> Option<ComponentId> {
        match self {
            AsmSelected::Component(c) | AsmSelected::Face(c, _) | AsmSelected::Edge(c, _) => {
                Some(*c)
            }
            AsmSelected::Mate(_) => None,
        }
    }

    /// The face or edge as a mate reference.
    pub fn mate_ref(&self) -> Option<MateRef> {
        match self {
            AsmSelected::Face(c, f) => Some(MateRef {
                component: *c,
                target: MateTarget::Face(f.clone()),
            }),
            AsmSelected::Edge(c, e) => Some(MateRef {
                component: *c,
                target: MateTarget::Edge(e.clone()),
            }),
            _ => None,
        }
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
    /// Part files by absolute path; `Err` when one could not be opened.
    pub parts: HashMap<PathBuf, Result<LoadedPart, String>>,
    /// Where each component's part file is.
    pub sources: HashMap<ComponentId, PathBuf>,
    /// The last solve: mate statuses and freedom.
    pub solution: Option<Solution>,
    pub selection: Vec<AsmSelected>,
    /// Components expanded in the tree (showing their planes and origin).
    pub expanded: HashSet<ComponentId>,
    /// The combined mesh's faces and edges, per component, from the last scene.
    pub ranges: Vec<MeshRange>,
    undo: Vec<Assembly>,
    redo: Vec<Assembly>,
}

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
            revision: 0,
            parts: HashMap::new(),
            sources: HashMap::new(),
            solution: None,
            selection: Vec::new(),
            expanded: HashSet::new(),
            ranges: Vec::new(),
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

    // ---- Parts ---------------------------------------------------------------------------

    /// Opens the part files not loaded yet. A part that moved is looked for, by document id,
    /// in the assembly's folder. Returns messages about parts found elsewhere or missing.
    pub fn load_parts<K: Kernel>(&mut self, kernel: &mut K) -> Vec<String> {
        let mut notes = Vec::new();
        let folder = self.folder();
        for i in 0..self.assembly.components.len() {
            let component = &self.assembly.components[i];
            let mut path = paths::absolute(&component.path, folder.as_deref());
            if !path.exists() {
                if let Some(found) = find_by_id(folder.as_deref(), &component.document_id) {
                    notes.push(format!(
                        "{}: {} was not found, using {}",
                        component.name,
                        component.path,
                        found.display()
                    ));
                    let stored = paths::stored(&found, folder.as_deref());
                    self.assembly.components[i].path = stored;
                    self.modified = true;
                    path = found;
                }
            }
            let component = &self.assembly.components[i];
            if !self.parts.contains_key(&path) {
                let loaded = load(&path, kernel);
                if let Err(e) = &loaded {
                    notes.push(format!("{}: {e}", component.name));
                }
                self.parts.insert(path.clone(), loaded);
            }
            self.sources.insert(component.id, path);
        }
        self.revision += 1;
        notes
    }

    /// Reads every part file again: after a part was edited and saved.
    pub fn reload_parts<K: Kernel>(&mut self, kernel: &mut K) -> Vec<String> {
        self.release(kernel);
        self.load_parts(kernel)
    }

    pub fn release<K: Kernel>(&mut self, kernel: &mut K) {
        for (_, loaded) in self.parts.drain() {
            if let Ok(mut loaded) = loaded {
                loaded.part.release(kernel);
            }
        }
        self.sources.clear();
    }

    /// The loaded part of a component, if it could be opened.
    pub fn part_of(&self, component: ComponentId) -> Option<&LoadedPart> {
        let path = self.sources.get(&component)?;
        self.parts.get(path)?.as_ref().ok()
    }

    /// Why a component's part could not be opened.
    pub fn part_error(&self, component: ComponentId) -> Option<&str> {
        let path = self.sources.get(&component)?;
        self.parts.get(path)?.as_ref().err().map(String::as_str)
    }

    // ---- Solving -------------------------------------------------------------------------

    /// Mate geometry in the component's own coordinates.
    pub fn local(&self, r: &MateRef) -> Result<Primitive, String> {
        let part = self
            .part_of(r.component)
            .ok_or_else(|| "the component's part could not be opened".to_owned())?;
        oxyde_assembly::resolve(&r.target, part.part.body())
    }

    /// Mate geometry placed in the assembly.
    pub fn placed(&self, assembly: &Assembly, r: &MateRef) -> Option<Primitive> {
        let component = assembly.component(r.component)?;
        Some(self.local(r).ok()?.placed(&component.placement))
    }

    /// About the size of the assembly, in mm.
    pub fn size(&self) -> f64 {
        let (lo, hi) = self.bounds(&self.assembly).unwrap_or((
            Point3::new(-50.0, -50.0, -50.0),
            Point3::new(50.0, 50.0, 50.0),
        ));
        (hi - lo).length().max(1.0)
    }

    /// Solves `assembly`'s mates in place and returns the result.
    pub fn solve_assembly(&self, assembly: &mut Assembly) -> Solution {
        let solution = oxyde_assembly::solve(assembly, |r| self.local(r), self.size());
        for (id, placement) in &solution.placements {
            if let Some(c) = assembly.component_mut(*id) {
                c.placement = *placement;
            }
        }
        solution
    }

    /// Solves the document's own mates.
    pub fn solve(&mut self) {
        let mut assembly = std::mem::take(&mut self.assembly);
        let solution = self.solve_assembly(&mut assembly);
        self.assembly = assembly;
        self.solution = Some(solution);
        self.revision += 1;
    }

    // ---- Display -------------------------------------------------------------------------

    /// Bounding box of the visible components of `assembly`.
    pub fn bounds(&self, assembly: &Assembly) -> Option<(Point3, Point3)> {
        let mut out: Option<(Point3, Point3)> = None;
        for c in assembly.components.iter().filter(|c| !c.hidden) {
            let Some((lo, hi)) = self.part_of(c.id).and_then(|p| p.bounds) else {
                continue;
            };
            for corner in corners(lo, hi) {
                let p = c.placement.apply_point(corner);
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

    /// The visible components of `assembly` as one mesh; remembers which faces are whose.
    pub fn scene_mesh(&mut self, assembly: &Assembly) -> Mesh {
        let visible: Vec<(ComponentId, &Placement, &Mesh)> = assembly
            .components
            .iter()
            .filter(|c| !c.hidden)
            .filter_map(|c| {
                let mesh = self.part_of(c.id)?.mesh.as_ref()?;
                Some((c.id, &c.placement, mesh))
            })
            .collect();
        let (mesh, ranges) = scene::combine(visible);
        self.ranges = ranges;
        mesh
    }

    /// What a pick in the viewport is.
    pub fn picked(&self, pick: Pick) -> Option<AsmSelected> {
        match pick {
            Pick::Face(i) => {
                let (c, face) = scene::face_owner(&self.ranges, i)?;
                let body = self.part_of(c)?.part.body()?;
                Some(AsmSelected::Face(c, body.naming.face(face)?.clone()))
            }
            Pick::Edge(i) => {
                let (c, edge) = scene::edge_owner(&self.ranges, i)?;
                let body = self.part_of(c)?.part.body()?;
                Some(AsmSelected::Edge(c, body.naming.edge(edge)?.clone()))
            }
        }
    }

    /// Faces and edges to highlight for selected things.
    pub fn picks_of<'a>(&self, items: impl IntoIterator<Item = &'a AsmSelected>) -> Vec<Pick> {
        let mut out = Vec::new();
        for item in items {
            match item {
                AsmSelected::Face(c, f) => {
                    if let Some(i) = self
                        .part_of(*c)
                        .and_then(|p| p.part.body())
                        .and_then(|b| b.naming.find_face(f))
                        .and_then(|i| scene::face_index(&self.ranges, *c, i))
                    {
                        out.push(Pick::Face(i));
                    }
                }
                AsmSelected::Edge(c, e) => {
                    if let Some(i) = self
                        .part_of(*c)
                        .and_then(|p| p.part.body())
                        .and_then(|b| b.naming.find_edge(e))
                        .and_then(|i| scene::edge_index(&self.ranges, *c, i))
                    {
                        out.push(Pick::Edge(i));
                    }
                }
                AsmSelected::Component(c) => {
                    if let Some(r) = self.ranges.iter().find(|r| r.component == *c) {
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
        let item = match &r.target {
            MateTarget::Face(f) => AsmSelected::Face(r.component, f.clone()),
            MateTarget::Edge(e) => AsmSelected::Edge(r.component, e.clone()),
            _ => return Vec::new(),
        };
        self.picks_of([&item])
    }

    // ---- Editing -------------------------------------------------------------------------

    /// Records the current state as one undo step.
    pub fn checkpoint(&mut self) {
        self.undo.push(self.assembly.clone());
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

    /// Returns the state restored, if any; the caller loads parts and solves.
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
    pub fn free_spot(&self, part: &LoadedPart) -> Placement {
        let Some((_, hi)) = self.bounds(&self.assembly) else {
            return Placement::IDENTITY;
        };
        let (lo, size) = part
            .bounds
            .map_or((Point3::ORIGIN, 0.0), |(lo, hi)| (lo, (hi - lo).length()));
        let gap = (size * 0.25).max(10.0);
        Placement::from_translation(Vec3::new(hi.x - lo.x + gap, 0.0, 0.0))
    }

    /// Stored path for a part file, given where the assembly is (or will be) saved.
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

/// Opens and rebuilds a part file, and meshes it.
pub fn load<K: Kernel>(path: &Path, kernel: &mut K) -> Result<LoadedPart, String> {
    let file = oxyde_format::load_part(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut part = Part::from_definition(file.definition).map_err(|e| e.to_string())?;
    part.rebuild(kernel);
    let (mesh, bounds) = match part.body() {
        Some(body) => {
            let mesh = kernel
                .tessellate(body.shape, deflection(&body.topology))
                .ok();
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

/// A part file with this document id in `folder` or one level below.
fn find_by_id(folder: Option<&Path>, document_id: &str) -> Option<PathBuf> {
    let folder = folder?;
    if document_id.is_empty() {
        return None;
    }
    let is_part = |p: &Path| {
        p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            e.eq_ignore_ascii_case(oxyde_format::PART_EXTENSION)
                || e.eq_ignore_ascii_case(oxyde_format::LEGACY_PART_EXTENSION)
        })
    };
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(folder).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Ok(inner) = std::fs::read_dir(&p) {
                candidates.extend(inner.flatten().map(|e| e.path()).filter(|p| is_part(p)));
            }
        } else if is_part(&p) {
            candidates.push(p);
        }
    }
    candidates.sort();
    candidates
        .into_iter()
        .find(|p| oxyde_format::document_kind(p).is_ok_and(|(_, id)| id == document_id))
}
