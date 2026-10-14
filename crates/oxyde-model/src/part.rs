//! The part: a timeline of features, and the engine that rebuilds it.
//!
//! Oxyde is bottom-up, like SolidWorks: a part is modeled on its own as an ordered list of
//! features (the timeline shown on the left of the screen); assemblies will place finished
//! parts later. Rebuilding replays the timeline through a [`Kernel`], keeping after each
//! feature the resulting body and the persistent names of all its faces, edges and
//! vertices.

use std::collections::HashSet;
use std::fmt;

use oxyde_geom::{Plane, Profile, ProfileError};
use oxyde_kernel::{BooleanOp, FaceSource, Kernel, ShapeHandle, SurfaceKind, Topology};
use oxyde_sketch::{ConstrainedSketch, ConstraintState};

use crate::error::ModelError;
use crate::feature::{
    EdgeBlend, Extrude, ExtrudeExtent, ExtrudeOp, Feature, FeatureId, FeatureKind, Sketch,
    SketchPlane,
};
use crate::naming::{BodyNaming, FaceKey, FaceName, Resolution};

/// State of a feature as shown in the timeline.
#[derive(Debug, Clone, PartialEq)]
pub enum FeatureStatus {
    Ok,
    /// Built, but something needs attention (e.g. a reference matched approximately).
    Warning(String),
    /// Not built; the body is left as it was before this feature.
    Error(String),
    Suppressed,
    /// Below the rollback bar.
    RolledBack,
    /// Edited (or after an edited feature) since the last rebuild.
    Pending,
}

/// One row of the timeline panel.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    pub id: FeatureId,
    pub name: String,
    pub type_name: &'static str,
    pub status: FeatureStatus,
    /// A sketch that can still move (shown "(-)" in front of its name, as in SolidWorks).
    pub under_defined: bool,
}

/// A solid produced by the timeline, with the persistent names of everything in it.
#[derive(Debug, Clone)]
pub struct Body {
    pub shape: ShapeHandle,
    pub topology: Topology,
    pub naming: BodyNaming,
}

/// A sketch solved and resolved in model space.
#[derive(Debug, Clone)]
pub struct SketchResult {
    pub plane: Plane,
    /// The geometry after solving its relations.
    pub solved: ConstrainedSketch,
    /// Independent ways the sketch can still move; 0 = fully defined.
    pub degrees_of_freedom: usize,
    /// Closed regions, or why the sketch cannot be extruded (an open sketch is not an error
    /// in itself, only for features that need a closed contour).
    pub profile: Result<Profile, ProfileError>,
}

#[derive(Debug)]
struct FeatureResult {
    status: FeatureStatus,
    /// The body after this feature, if the feature changed it.
    produced: Option<Body>,
    sketch: Option<SketchResult>,
}

impl FeatureResult {
    fn with_status(status: FeatureStatus) -> Self {
        Self {
            status,
            produced: None,
            sketch: None,
        }
    }
}

/// Everything that defines a part, without any computed geometry: what gets saved, and what
/// undo/redo snapshots hold.
#[derive(Debug, Clone, PartialEq)]
pub struct PartDefinition {
    pub features: Vec<Feature>,
    /// Number of active features; `None` = all.
    pub rollback: Option<usize>,
    /// Id the next new feature will get.
    pub next_id: u32,
}

/// A part document: its timeline and the cached results of the last rebuild.
///
/// Cached bodies hold kernel shapes; call [`Part::release`] with the same kernel before
/// dropping a part, or those shapes stay allocated until the kernel is dropped.
#[derive(Debug)]
pub struct Part {
    features: Vec<Feature>,
    /// Number of active features; `None` = all.
    rollback: Option<usize>,
    next_id: u32,
    results: Vec<FeatureResult>,
    /// Results from this index on are stale.
    dirty_from: usize,
}

impl Default for Part {
    fn default() -> Self {
        Self::new()
    }
}

impl Part {
    pub fn new() -> Self {
        Self {
            features: Vec::new(),
            rollback: None,
            next_id: 1,
            results: Vec::new(),
            dirty_from: 0,
        }
    }

    /// Rebuilds a part from its definition (nothing is computed until [`Part::rebuild`]).
    pub fn from_definition(definition: PartDefinition) -> Result<Self, ModelError> {
        let invalid = |reason: String| Err(ModelError::InvalidDefinition(reason));
        let mut seen = HashSet::new();
        let mut names = HashSet::new();
        for (index, feature) in definition.features.iter().enumerate() {
            if feature.id.raw() == 0 || !seen.insert(feature.id) {
                return invalid(format!("duplicate or invalid feature id {}", feature.id));
            }
            if !oxyde_sketch::is_valid_name(&feature.name) || !names.insert(feature.name.as_str()) {
                return invalid(format!(
                    "invalid or duplicate feature name \"{}\"",
                    feature.name
                ));
            }
            let parameters: Vec<_> = feature.kind.parameters().iter().map(|(p, _)| *p).collect();
            let mut dimension_names = HashSet::new();
            for parameter in &parameters {
                let name = feature.parameter_name(*parameter);
                if !oxyde_sketch::is_valid_name(name) || !dimension_names.insert(name) {
                    return invalid(format!(
                        "invalid dimension name \"{name}\" in {}",
                        feature.name
                    ));
                }
            }
            if let Some(stray) = feature
                .parameter_names
                .keys()
                .find(|p| !parameters.contains(p))
            {
                return invalid(format!("{} has no {} dimension", feature.name, stray.key()));
            }
            for dependency in feature.kind.dependencies() {
                if !definition.features[..index]
                    .iter()
                    .any(|f| f.id == dependency)
                {
                    return invalid(format!(
                        "{} refers to {dependency}, which does not come before it",
                        feature.name
                    ));
                }
            }
        }
        if let Some(active) = definition.rollback {
            if active > definition.features.len() {
                return invalid(format!(
                    "rollback position {active} is past the last feature"
                ));
            }
        }
        let highest = definition
            .features
            .iter()
            .map(|f| f.id.raw())
            .max()
            .unwrap_or(0);
        Ok(Self {
            next_id: definition.next_id.max(highest + 1),
            features: definition.features,
            rollback: definition.rollback,
            results: Vec::new(),
            dirty_from: 0,
        })
    }

    /// A copy of the part's definition.
    pub fn definition(&self) -> PartDefinition {
        PartDefinition {
            features: self.features.clone(),
            rollback: self.rollback,
            next_id: self.next_id,
        }
    }

    // ---- Timeline editing --------------------------------------------------------------------

    pub fn features(&self) -> &[Feature] {
        &self.features
    }

    pub fn feature(&self, id: FeatureId) -> Option<&Feature> {
        self.features.iter().find(|f| f.id == id)
    }

    pub fn index_of(&self, id: FeatureId) -> Option<usize> {
        self.features.iter().position(|f| f.id == id)
    }

    /// Adds a feature at the rollback bar (at the end when not rolled back), with an
    /// automatic name such as "Boss-Extrude2".
    pub fn add_feature(&mut self, kind: FeatureKind) -> FeatureId {
        let id = FeatureId::from_raw(self.next_id);
        self.next_id += 1;
        let name = self.next_name(kind.type_name());
        let at = self.active_end();
        self.features.insert(
            at,
            Feature {
                id,
                name,
                suppressed: false,
                kind,
            },
        );
        if let Some(active) = self.rollback.as_mut() {
            *active = at + 1;
        }
        self.mark_dirty(at);
        id
    }

    /// Gives mutable access to a feature's definition; it and everything after it will be
    /// rebuilt.
    pub fn edit(&mut self, id: FeatureId) -> Result<&mut FeatureKind, ModelError> {
        let index = self.require(id)?;
        self.mark_dirty(index);
        Ok(&mut self.features[index].kind)
    }

    /// Renames a feature. Names must be valid and unique in the part; dimension full names
    /// ("D1@NewName") follow automatically.
    pub fn rename(&mut self, id: FeatureId, name: impl Into<String>) -> Result<(), ModelError> {
        let index = self.require(id)?;
        let name = name.into();
        if !oxyde_sketch::is_valid_name(&name) {
            return Err(ModelError::InvalidName(name));
        }
        if self.features.iter().any(|f| f.id != id && f.name == name) {
            return Err(ModelError::DuplicateName(name));
        }
        self.features[index].name = name;
        Ok(())
    }

    /// Mutable access to a feature without marking anything for rebuild, for changes that
    /// do not affect geometry (dimension names).
    pub(crate) fn feature_mut_quietly(&mut self, id: FeatureId) -> Option<&mut Feature> {
        self.features.iter_mut().find(|f| f.id == id)
    }

    pub fn set_suppressed(&mut self, id: FeatureId, suppressed: bool) -> Result<(), ModelError> {
        let index = self.require(id)?;
        if self.features[index].suppressed != suppressed {
            self.features[index].suppressed = suppressed;
            self.mark_dirty(index);
        }
        Ok(())
    }

    /// Deletes a feature. Refused while other features depend on it.
    pub fn delete(&mut self, id: FeatureId) -> Result<(), ModelError> {
        let index = self.require(id)?;
        let dependents = self.dependents(id);
        if !dependents.is_empty() {
            return Err(ModelError::HasDependents {
                feature: id,
                dependents,
            });
        }
        self.features.remove(index);
        if let Some(active) = self.rollback.as_mut() {
            if index < *active {
                *active -= 1;
            }
        }
        self.mark_dirty(index);
        Ok(())
    }

    /// Features that refer to `id`.
    pub fn dependents(&self, id: FeatureId) -> Vec<FeatureId> {
        self.features
            .iter()
            .filter(|f| f.id != id && f.kind.dependencies().contains(&id))
            .map(|f| f.id)
            .collect()
    }

    /// Moves the rollback bar just above feature `before`, or to the end with `None`.
    pub fn roll_back_to(&mut self, before: Option<FeatureId>) -> Result<(), ModelError> {
        self.rollback = match before {
            Some(id) => Some(self.require(id)?),
            None => None,
        };
        Ok(())
    }

    /// Number of active features when rolled back.
    pub fn rollback_position(&self) -> Option<usize> {
        self.rollback
    }

    // ---- Rebuild -----------------------------------------------------------------------------

    /// Replays the timeline from the first edited feature up to the rollback bar.
    pub fn rebuild<K: Kernel>(&mut self, kernel: &mut K) {
        let end = self.active_end();
        let start = self.dirty_from.min(self.results.len()).min(end);
        for stale in self.results.drain(start..) {
            if let Some(body) = stale.produced {
                kernel.release(body.shape);
            }
        }
        for index in start..end {
            let result = if self.features[index].suppressed {
                FeatureResult::with_status(FeatureStatus::Suppressed)
            } else {
                build_feature(&self.features, &self.results, index, kernel)
            };
            self.results.push(result);
        }
        self.dirty_from = end;
    }

    /// Frees every cached kernel shape. The next rebuild starts from scratch.
    pub fn release<K: Kernel>(&mut self, kernel: &mut K) {
        for result in self.results.drain(..) {
            if let Some(body) = result.produced {
                kernel.release(body.shape);
            }
        }
        self.dirty_from = 0;
    }

    // ---- Queries -----------------------------------------------------------------------------

    /// Rows for the timeline panel, in order.
    pub fn timeline(&self) -> Vec<TimelineEntry> {
        let end = self.active_end();
        let built = self.built();
        self.features
            .iter()
            .enumerate()
            .map(|(i, f)| TimelineEntry {
                id: f.id,
                name: f.name.clone(),
                type_name: f.kind.type_name(),
                status: if i >= end {
                    FeatureStatus::RolledBack
                } else if i < built {
                    self.results[i].status.clone()
                } else {
                    FeatureStatus::Pending
                },
                under_defined: i < built.min(end)
                    && self.results[i]
                        .sketch
                        .as_ref()
                        .is_some_and(|s| s.degrees_of_freedom > 0),
            })
            .collect()
    }

    pub fn status(&self, id: FeatureId) -> Option<FeatureStatus> {
        self.timeline()
            .into_iter()
            .find(|e| e.id == id)
            .map(|e| e.status)
    }

    /// The final body of the last rebuild (at the rollback bar).
    pub fn body(&self) -> Option<&Body> {
        current_body(&self.results[..self.built()])
    }

    /// The body as it is just before feature `id`: what that feature's references point
    /// into (the edges a fillet may select, the faces a sketch may sit on).
    pub fn body_before(&self, id: FeatureId) -> Option<&Body> {
        let index = self.index_of(id)?;
        current_body(&self.results[..index.min(self.built())])
    }

    pub fn sketch_result(&self, id: FeatureId) -> Option<&SketchResult> {
        let index = self.index_of(id)?;
        if index >= self.built() {
            return None;
        }
        self.results[index].sketch.as_ref()
    }

    // ---- Internals ---------------------------------------------------------------------------

    fn active_end(&self) -> usize {
        self.rollback
            .map_or(self.features.len(), |n| n.min(self.features.len()))
    }

    fn built(&self) -> usize {
        self.dirty_from.min(self.results.len())
    }

    fn mark_dirty(&mut self, index: usize) {
        self.dirty_from = self.dirty_from.min(index);
    }

    fn require(&self, id: FeatureId) -> Result<usize, ModelError> {
        self.index_of(id).ok_or(ModelError::UnknownFeature(id))
    }

    /// SolidWorks-style naming: one more than the highest number already used.
    fn next_name(&self, base: &str) -> String {
        let highest = self
            .features
            .iter()
            .filter_map(|f| f.name.strip_prefix(base)?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        format!("{base}{}", highest + 1)
    }
}

// ---- Feature builders ------------------------------------------------------------------------

fn current_body(results: &[FeatureResult]) -> Option<&Body> {
    results.iter().rev().find_map(|r| r.produced.as_ref())
}

fn build_feature<K: Kernel>(
    features: &[Feature],
    previous: &[FeatureResult],
    index: usize,
    kernel: &mut K,
) -> FeatureResult {
    let feature = &features[index];
    let body = current_body(previous);
    let mut warnings = Vec::new();

    let outcome = match &feature.kind {
        FeatureKind::Sketch(sketch) => {
            build_sketch(sketch, body, &mut warnings).map(|s| (None, Some(s)))
        }
        FeatureKind::Extrude(extrude) => {
            build_extrude(feature.id, extrude, features, previous, index, body, kernel)
                .map(|b| (Some(b), None))
        }
        FeatureKind::Fillet(blend) => build_blend(
            feature.id,
            blend,
            Blend::Fillet,
            body,
            kernel,
            &mut warnings,
        )
        .map(|b| (Some(b), None)),
        FeatureKind::Chamfer(blend) => build_blend(
            feature.id,
            blend,
            Blend::Chamfer,
            body,
            kernel,
            &mut warnings,
        )
        .map(|b| (Some(b), None)),
    };

    match outcome {
        Ok((produced, sketch)) => FeatureResult {
            status: if warnings.is_empty() {
                FeatureStatus::Ok
            } else {
                FeatureStatus::Warning(warnings.join("; "))
            },
            produced,
            sketch,
        },
        Err(message) => FeatureResult::with_status(FeatureStatus::Error(message)),
    }
}

fn resolve(
    resolution: Resolution,
    what: &str,
    name: &dyn fmt::Display,
    warnings: &mut Vec<String>,
) -> Result<u32, String> {
    match resolution {
        Resolution::Exact(index) => Ok(index),
        Resolution::Fallback(index) => {
            warnings.push(format!("{what} {name} was matched approximately"));
            Ok(index)
        }
        Resolution::Ambiguous(count) => Err(format!(
            "{what} {name} is ambiguous ({count} candidates): select it again"
        )),
        Resolution::Lost => Err(format!("{what} {name} no longer exists: select it again")),
    }
}

fn face_plane(body: &Body, index: u32) -> Option<Plane> {
    let face = body.topology.faces.get(index as usize)?;
    if face.surface != SurfaceKind::Plane {
        return None;
    }
    Plane::from_point_normal(face.centroid, face.normal?)
}

fn build_sketch(
    sketch: &Sketch,
    body: Option<&Body>,
    warnings: &mut Vec<String>,
) -> Result<SketchResult, String> {
    let plane = match &sketch.plane {
        SketchPlane::Standard(standard) => standard.plane(),
        SketchPlane::Face(name) => {
            let body = body.ok_or_else(|| format!("face {name} not found: no body yet"))?;
            let index = resolve(body.naming.resolve_face(name), "face", name, warnings)?;
            face_plane(body, index).ok_or_else(|| format!("face {name} is not planar"))?
        }
    };
    let mut solved = sketch.geometry.clone();
    let report = solved.solve().map_err(|e| e.to_string())?;
    if !report.converged {
        return Err(format!(
            "the sketch is over-defined: {} conflicting relation(s)",
            report.count(ConstraintState::Conflicting).max(1)
        ));
    }
    let redundant = report.count(ConstraintState::Redundant);
    if redundant > 0 {
        warnings.push(format!("{redundant} redundant relation(s) in the sketch"));
    }
    Ok(SketchResult {
        plane,
        profile: Profile::from_curves(&solved.profile_segments()),
        degrees_of_freedom: report.degrees_of_freedom,
        solved,
    })
}

/// Several sources can name one face (e.g. coplanar faces merged by a boolean); the name
/// from the oldest feature wins, so that existing references stay valid.
fn oldest(names: impl Iterator<Item = FaceName>) -> Option<FaceName> {
    names.min_by(|a, b| a.key.feature().cmp(&b.key.feature()).then_with(|| a.cmp(b)))
}

fn finish<K: Kernel>(
    id: FeatureId,
    shape: ShapeHandle,
    face_names: Vec<Option<FaceName>>,
    kernel: &mut K,
) -> Result<Body, String> {
    match kernel.topology(shape) {
        Ok(topology) => {
            let naming = BodyNaming::build(id, face_names, &topology);
            Ok(Body {
                shape,
                topology,
                naming,
            })
        }
        Err(e) => {
            kernel.release(shape);
            Err(e.to_string())
        }
    }
}

fn build_extrude<K: Kernel>(
    id: FeatureId,
    extrude: &Extrude,
    features: &[Feature],
    previous: &[FeatureResult],
    index: usize,
    body: Option<&Body>,
    kernel: &mut K,
) -> Result<Body, String> {
    let sketch_index = features[..index]
        .iter()
        .position(|f| f.id == extrude.sketch)
        .ok_or_else(|| format!("sketch {} must come before this feature", extrude.sketch))?;
    let sketch_name = &features[sketch_index].name;
    let sketch = previous[sketch_index]
        .sketch
        .as_ref()
        .ok_or_else(|| format!("{sketch_name} is suppressed or has errors"))?;
    let profile = sketch
        .profile
        .as_ref()
        .map_err(|e| format!("{sketch_name}: {e}"))?;
    if profile.is_empty() {
        return Err(format!("{sketch_name} has no closed contour"));
    }
    let depth = extrude.extent.depth();
    if !(depth > 0.0) {
        return Err("depth must be positive".to_owned());
    }

    let mut along = match extrude.op {
        ExtrudeOp::Boss => 1.0,
        ExtrudeOp::Cut => -1.0,
    };
    if extrude.reverse {
        along = -along;
    }
    let (plane, vector) = match extrude.extent {
        ExtrudeExtent::Blind { depth } => (sketch.plane, sketch.plane.normal * (depth * along)),
        ExtrudeExtent::MidPlane { depth } => (
            sketch.plane.offset(-depth / 2.0),
            sketch.plane.normal * depth,
        ),
    };

    let tool = kernel
        .extrude(&plane, profile, vector)
        .map_err(|e| e.to_string())?;
    let tool_names: Vec<Option<FaceName>> = tool
        .history
        .faces
        .iter()
        .map(|sources| {
            oldest(sources.iter().filter_map(|source| match *source {
                FaceSource::ProfileSegment(segment) => Some(FaceName::profile(id, segment)),
                FaceSource::StartCap => Some(FaceName::start_cap(id)),
                FaceSource::EndCap => Some(FaceName::end_cap(id)),
                _ => None,
            }))
        })
        .collect();

    let Some(body) = body else {
        return match extrude.op {
            ExtrudeOp::Boss => finish(id, tool.shape, tool_names, kernel),
            ExtrudeOp::Cut => {
                kernel.release(tool.shape);
                Err("nothing to cut: the part has no body yet".to_owned())
            }
        };
    };

    let op = match extrude.op {
        ExtrudeOp::Boss => BooleanOp::Fuse,
        ExtrudeOp::Cut => BooleanOp::Cut,
    };
    let combined = kernel.boolean(op, body.shape, tool.shape);
    kernel.release(tool.shape);
    let combined = combined.map_err(|e| e.to_string())?;
    let names = combined
        .history
        .faces
        .iter()
        .map(|sources| {
            oldest(sources.iter().filter_map(|source| match *source {
                FaceSource::InputFace { input: 0, face } => body.naming.face(face).cloned(),
                FaceSource::InputFace { input: 1, face } => {
                    tool_names.get(face as usize).cloned().flatten()
                }
                _ => None,
            }))
        })
        .collect();
    finish(id, combined.shape, names, kernel)
}

#[derive(Debug, Clone, Copy)]
enum Blend {
    Fillet,
    Chamfer,
}

fn build_blend<K: Kernel>(
    id: FeatureId,
    blend: &EdgeBlend,
    kind: Blend,
    body: Option<&Body>,
    kernel: &mut K,
    warnings: &mut Vec<String>,
) -> Result<Body, String> {
    let body = body.ok_or_else(|| "there is no body yet".to_owned())?;
    if blend.edges.is_empty() {
        return Err("no edges selected".to_owned());
    }
    if !(blend.size > 0.0) {
        return Err("size must be positive".to_owned());
    }

    let mut edges = Vec::with_capacity(blend.edges.len());
    for name in &blend.edges {
        edges.push(resolve(
            body.naming.resolve_edge(name),
            "edge",
            name,
            warnings,
        )?);
    }
    edges.sort_unstable();
    edges.dedup();

    let result = match kind {
        Blend::Fillet => kernel.fillet(body.shape, &edges, blend.size),
        Blend::Chamfer => kernel.chamfer(body.shape, &edges, blend.size),
    }
    .map_err(|e| e.to_string())?;

    let names = result
        .history
        .faces
        .iter()
        .map(|sources| {
            oldest(sources.iter().filter_map(|source| {
                match *source {
                    FaceSource::InputFace { input: 0, face } => body.naming.face(face).cloned(),
                    FaceSource::InputEdge { input: 0, edge } => body
                        .naming
                        .edge(edge)
                        .map(|e| FaceName::from_edge(id, e.clone())),
                    FaceSource::InputVertex { input: 0, vertex } => {
                        body.naming.vertex(vertex).map(|v| {
                            FaceName::new(FaceKey::FromVertex {
                                feature: id,
                                vertex: v.clone(),
                            })
                        })
                    }
                    _ => None,
                }
            }))
        })
        .collect();
    finish(id, result.shape, names, kernel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::{SketchPlane, StandardPlane};
    use crate::naming::EdgeName;
    use oxyde_geom::Point2;

    fn rectangle_sketch() -> FeatureKind {
        let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
        sketch.rectangle(Point2::ORIGIN, 10.0, 10.0);
        FeatureKind::Sketch(sketch)
    }

    #[test]
    fn features_get_solidworks_style_names() {
        let mut part = Part::new();
        let s1 = part.add_feature(rectangle_sketch());
        part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            5.0,
        )));
        let s2 = part.add_feature(rectangle_sketch());
        part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s2,
            ExtrudeOp::Cut,
            1.0,
        )));
        part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s2,
            ExtrudeOp::Boss,
            1.0,
        )));
        let names: Vec<&str> = part.features().iter().map(|f| f.name()).collect();
        assert_eq!(
            names,
            [
                "Sketch1",
                "Boss-Extrude1",
                "Sketch2",
                "Cut-Extrude1",
                "Boss-Extrude2"
            ]
        );
    }

    #[test]
    fn new_features_are_inserted_at_the_rollback_bar() {
        let mut part = Part::new();
        let s1 = part.add_feature(rectangle_sketch());
        let boss = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            5.0,
        )));
        part.roll_back_to(Some(boss)).unwrap();
        let s2 = part.add_feature(rectangle_sketch());
        let order: Vec<FeatureId> = part.features().iter().map(|f| f.id()).collect();
        assert_eq!(order, [s1, s2, boss]);
        assert_eq!(part.rollback_position(), Some(2));

        let timeline = part.timeline();
        assert_eq!(timeline[2].status, FeatureStatus::RolledBack);
        assert_eq!(timeline[0].status, FeatureStatus::Pending);
    }

    #[test]
    fn features_with_dependents_cannot_be_deleted() {
        let mut part = Part::new();
        let s1 = part.add_feature(rectangle_sketch());
        let boss = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            5.0,
        )));
        let fillet = part.add_feature(FeatureKind::Fillet(EdgeBlend {
            edges: vec![EdgeName::between(
                FaceName::profile(boss, 1),
                FaceName::end_cap(boss),
            )],
            size: 1.0,
        }));

        assert_eq!(
            part.delete(s1),
            Err(ModelError::HasDependents {
                feature: s1,
                dependents: vec![boss]
            })
        );
        assert_eq!(part.dependents(boss), vec![fillet]);
        part.delete(fillet).unwrap();
        part.delete(boss).unwrap();
        part.delete(s1).unwrap();
        assert!(part.features().is_empty());
    }

    #[test]
    fn definition_round_trip_and_validation() {
        let mut part = Part::new();
        let s1 = part.add_feature(rectangle_sketch());
        part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            5.0,
        )));
        part.roll_back_to(Some(s1)).unwrap();

        let definition = part.definition();
        let restored = Part::from_definition(definition.clone()).unwrap();
        assert_eq!(restored.definition(), definition);

        // A feature referring to a later one is rejected.
        let mut swapped = definition.clone();
        swapped.features.swap(0, 1);
        assert!(matches!(
            Part::from_definition(swapped),
            Err(ModelError::InvalidDefinition(_))
        ));

        // Duplicate ids are rejected.
        let mut duplicated = definition;
        duplicated.features.push(duplicated.features[0].clone());
        assert!(Part::from_definition(duplicated).is_err());
    }

    #[test]
    fn unknown_features_are_reported() {
        let mut part = Part::new();
        let ghost = FeatureId::from_raw(42);
        assert_eq!(
            part.edit(ghost).err(),
            Some(ModelError::UnknownFeature(ghost))
        );
    }
}
