//! The part: a timeline of features, and the engine that rebuilds it.
//!
//! Oxyde is bottom-up, like SolidWorks: a part is modeled on its own as an ordered list of
//! features (the timeline shown on the left of the screen); assemblies will place finished
//! parts later. Rebuilding replays the timeline through a [`Kernel`], keeping after each
//! feature the resulting body and the persistent names of all its faces, edges and
//! vertices.

use std::collections::HashSet;
use std::fmt;

use oxyde_geom::{Axis, Plane, Profile, ProfileError, Transform};
use oxyde_kernel::{
    BooleanOp, FaceSource, Kernel, OpHistory, OpResult, ShapeHandle, SurfaceKind, Topology,
};
use oxyde_sketch::{ConstrainedSketch, ConstraintState, EntityId, Geometry};

use crate::equations::{EquationResults, Equations};
use crate::error::ModelError;
use crate::feature::{
    AxisRef, CircularPattern, EdgeBlend, Extrude, ExtrudeExtent, ExtrudeOp, Feature, FeatureId,
    FeatureKind, LinearPattern, Mirror, PlaneRef, RefAxis, RefPlane, Revolve, RevolveAxis, Shell,
    Sketch, SketchPlane,
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
/// Geometry of a reference feature.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReferenceGeometry {
    Plane(Plane),
    Axis(Axis),
}

/// The solid an extrusion or a revolve added or removed, kept so that patterns and mirrors
/// can copy it.
#[derive(Debug)]
struct ToolBody {
    shape: ShapeHandle,
    /// False when the tool is the feature's body itself (the first boss of a part).
    owned: bool,
    /// Name of each face of the tool.
    names: Vec<Option<FaceName>>,
    op: BooleanOp,
}

#[derive(Debug)]
struct FeatureResult {
    status: FeatureStatus,
    /// The body after this feature, if the feature changed it.
    produced: Option<Body>,
    sketch: Option<SketchResult>,
    tool: Option<ToolBody>,
    reference: Option<ReferenceGeometry>,
}

impl FeatureResult {
    fn with_status(status: FeatureStatus) -> Self {
        Self {
            status,
            produced: None,
            sketch: None,
            tool: None,
            reference: None,
        }
    }

    fn release<K: Kernel>(self, kernel: &mut K) {
        if let Some(body) = self.produced {
            kernel.release(body.shape);
        }
        if let Some(tool) = self.tool {
            if tool.owned {
                kernel.release(tool.shape);
            }
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
    /// Global variables and equations.
    pub equations: Equations,
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
    pub(crate) equations: Equations,
    pub(crate) equation_results: EquationResults,
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
            equations: Equations::default(),
            equation_results: EquationResults::default(),
            results: Vec::new(),
            dirty_from: 0,
        }
    }

    /// Rebuilds a part from its definition (nothing is computed until [`Part::rebuild`]).
    pub fn from_definition(mut definition: PartDefinition) -> Result<Self, ModelError> {
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
                if !oxyde_sketch::is_valid_name(&name) || !dimension_names.insert(name.clone()) {
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
        definition.equations.validate()?;
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
            equations: definition.equations,
            equation_results: EquationResults::default(),
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
            equations: self.equations.clone(),
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
        self.features
            .insert(at, Feature::new(id, name, false, kind));
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
        self.refresh_equation_names();
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

    /// Evaluates the equations, then replays the timeline from the first edited feature up
    /// to the rollback bar.
    pub fn rebuild<K: Kernel>(&mut self, kernel: &mut K) {
        self.evaluate_equations();
        let end = self.active_end();
        let start = self.dirty_from.min(self.results.len()).min(end);
        for stale in self.results.drain(start..) {
            stale.release(kernel);
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
            result.release(kernel);
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

    /// Geometry of a built reference plane or axis.
    pub fn reference(&self, id: FeatureId) -> Option<ReferenceGeometry> {
        let index = self.index_of(id)?;
        if index >= self.built().min(self.active_end()) {
            return None;
        }
        self.results[index].reference
    }

    /// Every built reference plane and axis, for display.
    pub fn references(&self) -> Vec<(FeatureId, ReferenceGeometry)> {
        let built = self.built().min(self.active_end());
        self.features[..built]
            .iter()
            .zip(&self.results)
            .filter_map(|(f, r)| r.reference.map(|g| (f.id, g)))
            .collect()
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

/// What a feature being built can see: the features before it, their results, and the body
/// as it is just before it.
struct Context<'a> {
    features: &'a [Feature],
    previous: &'a [FeatureResult],
    index: usize,
    body: Option<&'a Body>,
}

/// Pattern instances above this count are refused, to keep a typo from freezing the
/// application.
const MAX_INSTANCES: u64 = 1000;

impl<'a> Context<'a> {
    fn feature(&self) -> &'a Feature {
        &self.features[self.index]
    }

    /// An earlier feature and its result.
    fn earlier(&self, id: FeatureId) -> Result<(&'a Feature, &'a FeatureResult), String> {
        let index = self.features[..self.index]
            .iter()
            .position(|f| f.id == id)
            .ok_or_else(|| format!("{id} must come before this feature"))?;
        Ok((&self.features[index], &self.previous[index]))
    }

    fn body(&self) -> Result<&'a Body, String> {
        self.body.ok_or_else(|| "there is no body yet".to_owned())
    }

    fn plane(&self, plane: &PlaneRef, warnings: &mut Vec<String>) -> Result<Plane, String> {
        match plane {
            PlaneRef::Standard(standard) => Ok(standard.plane()),
            PlaneRef::Face(name) => {
                let body = self
                    .body
                    .ok_or_else(|| format!("face {name} not found: no body yet"))?;
                let index = resolve(body.naming.resolve_face(name), "face", name, warnings)?;
                face_plane(body, index).ok_or_else(|| format!("face {name} is not planar"))
            }
            PlaneRef::Reference(id) => {
                let (feature, result) = self.earlier(*id)?;
                match result.reference {
                    Some(ReferenceGeometry::Plane(plane)) => Ok(plane),
                    Some(ReferenceGeometry::Axis(_)) => {
                        Err(format!("{} is an axis, not a plane", feature.name))
                    }
                    None => Err(format!("{} is suppressed or has errors", feature.name)),
                }
            }
        }
    }

    fn axis(&self, axis: &AxisRef, warnings: &mut Vec<String>) -> Result<Axis, String> {
        match axis {
            AxisRef::Standard(standard) => Ok(standard.axis()),
            AxisRef::Reference(id) => {
                let (feature, result) = self.earlier(*id)?;
                match result.reference {
                    Some(ReferenceGeometry::Axis(axis)) => Ok(axis),
                    Some(ReferenceGeometry::Plane(_)) => {
                        Err(format!("{} is a plane, not an axis", feature.name))
                    }
                    None => Err(format!("{} is suppressed or has errors", feature.name)),
                }
            }
            AxisRef::Edge(name) => {
                let body = self.body()?;
                let index = resolve(body.naming.resolve_edge(name), "edge", name, warnings)?;
                body.topology.edges[index as usize]
                    .axis
                    .ok_or_else(|| format!("edge {name} is neither straight nor circular"))
            }
            AxisRef::Face(name) => {
                let body = self.body()?;
                let index = resolve(body.naming.resolve_face(name), "face", name, warnings)?;
                let face = &body.topology.faces[index as usize];
                face.axis
                    .or_else(|| face.normal.and_then(|n| Axis::new(face.centroid, n)))
                    .ok_or_else(|| format!("face {name} has no axis"))
            }
        }
    }

    /// The solved sketch an extrusion or a revolve uses, with a closed profile.
    fn profile_sketch(&self, sketch: FeatureId) -> Result<(&'a SketchResult, &'a Profile), String> {
        let (feature, result) = self.earlier(sketch)?;
        let name = &feature.name;
        let solved = result
            .sketch
            .as_ref()
            .ok_or_else(|| format!("{name} is suppressed or has errors"))?;
        let profile = solved
            .profile
            .as_ref()
            .map_err(|e| format!("{name}: {e}"))?;
        if profile.is_empty() {
            return Err(format!("{name} has no closed contour"));
        }
        Ok((solved, profile))
    }
}

fn build_feature<K: Kernel>(
    features: &[Feature],
    previous: &[FeatureResult],
    index: usize,
    kernel: &mut K,
) -> FeatureResult {
    let ctx = Context {
        features,
        previous,
        index,
        body: current_body(previous),
    };
    let feature = ctx.feature();
    let mut warnings = Vec::new();
    let mut result = FeatureResult::with_status(FeatureStatus::Ok);

    let outcome = (|| -> Result<(), String> {
        match &feature.kind {
            FeatureKind::Sketch(sketch) => {
                result.sketch = Some(build_sketch(&ctx, sketch, &mut warnings)?);
            }
            FeatureKind::Extrude(extrude) => {
                let (body, tool) = build_extrude(&ctx, extrude, kernel)?;
                result.produced = Some(body);
                result.tool = Some(tool);
            }
            FeatureKind::Revolve(revolve) => {
                let (body, tool) = build_revolve(&ctx, revolve, kernel, &mut warnings)?;
                result.produced = Some(body);
                result.tool = Some(tool);
            }
            FeatureKind::Fillet(blend) => {
                result.produced = Some(build_blend(
                    &ctx,
                    blend,
                    Blend::Fillet,
                    kernel,
                    &mut warnings,
                )?);
            }
            FeatureKind::Chamfer(blend) => {
                result.produced = Some(build_blend(
                    &ctx,
                    blend,
                    Blend::Chamfer,
                    kernel,
                    &mut warnings,
                )?);
            }
            FeatureKind::Shell(shell) => {
                result.produced = Some(build_shell(&ctx, shell, kernel, &mut warnings)?);
            }
            FeatureKind::Plane(plane) => {
                result.reference = Some(build_ref_plane(&ctx, plane, &mut warnings)?);
            }
            FeatureKind::Axis(axis) => {
                result.reference = Some(build_ref_axis(&ctx, axis, &mut warnings)?);
            }
            FeatureKind::LinearPattern(pattern) => {
                let instances = linear_instances(&ctx, pattern, &mut warnings)?;
                result.produced = Some(build_instances(&ctx, &pattern.seeds, &instances, kernel)?);
            }
            FeatureKind::CircularPattern(pattern) => {
                let instances = circular_instances(&ctx, pattern, &mut warnings)?;
                result.produced = Some(build_instances(&ctx, &pattern.seeds, &instances, kernel)?);
            }
            FeatureKind::Mirror(mirror) => {
                let instances = mirror_instances(&ctx, mirror, &mut warnings)?;
                result.produced = Some(build_instances(&ctx, &mirror.seeds, &instances, kernel)?);
            }
        }
        Ok(())
    })();

    match outcome {
        Ok(()) => {
            if !warnings.is_empty() {
                result.status = FeatureStatus::Warning(warnings.join("; "));
            }
            result
        }
        Err(message) => {
            // Nothing is kept from a failed feature.
            result.release(kernel);
            FeatureResult::with_status(FeatureStatus::Error(message))
        }
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
    ctx: &Context<'_>,
    sketch: &Sketch,
    warnings: &mut Vec<String>,
) -> Result<SketchResult, String> {
    let plane = match &sketch.plane {
        SketchPlane::Standard(standard) => standard.plane(),
        SketchPlane::Face(name) => ctx.plane(&PlaneRef::Face(name.clone()), warnings)?,
        SketchPlane::Reference(id) => ctx.plane(&PlaneRef::Reference(*id), warnings)?,
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

/// Names of a swept tool's faces (extrusion or revolve).
fn sweep_names(id: FeatureId, history: &OpHistory) -> Vec<Option<FaceName>> {
    history
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
        .collect()
}

/// Names after a boolean of `body` (input 0) with a tool (input 1) named `tool_names`.
fn boolean_names(
    history: &OpHistory,
    body: &Body,
    tool_names: &[Option<FaceName>],
) -> Vec<Option<FaceName>> {
    history
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
        .collect()
}

/// Names after an operation on one body (fillet, chamfer, shell): faces kept from the body,
/// faces made from its edges and vertices, and offset copies of its faces.
fn derived_names(id: FeatureId, history: &OpHistory, body: &Body) -> Vec<Option<FaceName>> {
    history
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
                    FaceSource::OffsetFace { input: 0, face } => body.naming.face(face).map(|f| {
                        FaceName::new(FaceKey::Offset {
                            feature: id,
                            face: Box::new(f.clone()),
                        })
                    }),
                    _ => None,
                }
            }))
        })
        .collect()
}

/// Adds or removes a swept tool: the body after the feature, and the tool kept for
/// patterns.
fn apply_tool<K: Kernel>(
    id: FeatureId,
    op: ExtrudeOp,
    tool: OpResult,
    body: Option<&Body>,
    kernel: &mut K,
) -> Result<(Body, ToolBody), String> {
    let tool_names = sweep_names(id, &tool.history);
    let boolean = match op {
        ExtrudeOp::Boss => BooleanOp::Fuse,
        ExtrudeOp::Cut => BooleanOp::Cut,
    };
    let Some(body) = body else {
        return match op {
            ExtrudeOp::Boss => {
                let built = finish(id, tool.shape, tool_names.clone(), kernel)?;
                Ok((
                    built,
                    ToolBody {
                        shape: tool.shape,
                        owned: false,
                        names: tool_names,
                        op: boolean,
                    },
                ))
            }
            ExtrudeOp::Cut => {
                kernel.release(tool.shape);
                Err("nothing to cut: the part has no body yet".to_owned())
            }
        };
    };
    let combined = match kernel.boolean(boolean, body.shape, tool.shape) {
        Ok(combined) => combined,
        Err(e) => {
            kernel.release(tool.shape);
            return Err(e.to_string());
        }
    };
    let names = boolean_names(&combined.history, body, &tool_names);
    match finish(id, combined.shape, names, kernel) {
        Ok(built) => Ok((
            built,
            ToolBody {
                shape: tool.shape,
                owned: true,
                names: tool_names,
                op: boolean,
            },
        )),
        Err(e) => {
            kernel.release(tool.shape);
            Err(e)
        }
    }
}

fn build_extrude<K: Kernel>(
    ctx: &Context<'_>,
    extrude: &Extrude,
    kernel: &mut K,
) -> Result<(Body, ToolBody), String> {
    let (sketch, profile) = ctx.profile_sketch(extrude.sketch)?;
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
    apply_tool(ctx.feature().id, extrude.op, tool, ctx.body, kernel)
}

fn build_revolve<K: Kernel>(
    ctx: &Context<'_>,
    revolve: &Revolve,
    kernel: &mut K,
    warnings: &mut Vec<String>,
) -> Result<(Body, ToolBody), String> {
    let (sketch, profile) = ctx.profile_sketch(revolve.sketch)?;
    let full_turn = std::f64::consts::TAU;
    if !(revolve.angle > 0.0) || revolve.angle > full_turn + 1e-9 {
        return Err("the angle must be more than 0° and at most 360°".to_owned());
    }
    let angle = revolve.angle.min(full_turn);

    let mut axis = match &revolve.axis {
        RevolveAxis::SketchLine(line) => {
            let id = EntityId(*line);
            let is_line = matches!(
                sketch.solved.entity(id).map(|e| &e.geometry),
                Some(Geometry::Line { .. })
            );
            let points = sketch.solved.endpoints(id).filter(|_| is_line);
            let (a, b) = points
                .and_then(|(a, b)| Some((sketch.solved.position(a)?, sketch.solved.position(b)?)))
                .ok_or_else(|| format!("the axis (line {line}) is not a line of the sketch"))?;
            let (a, b) = (sketch.plane.to_world(a), sketch.plane.to_world(b));
            Axis::new(a, b - a).ok_or_else(|| "the axis line has no length".to_owned())?
        }
        RevolveAxis::Axis(axis) => ctx.axis(axis, warnings)?,
    };
    let plane = sketch.plane;
    let off_plane = (axis.origin - plane.origin).dot(plane.normal).abs();
    if axis.direction.dot(plane.normal).abs() > 1e-6 || off_plane > 1e-6 {
        return Err("the axis must lie in the sketch plane".to_owned());
    }
    if revolve.reverse {
        axis = axis.reversed();
    }
    // Mid-plane: start half the angle before the sketch plane.
    let plane = if revolve.mid_plane && angle < full_turn {
        plane.transformed(&Transform::rotation(&axis, -angle / 2.0))
    } else {
        plane
    };

    let tool = kernel
        .revolve(&plane, profile, &axis, angle)
        .map_err(|e| e.to_string())?;
    apply_tool(ctx.feature().id, revolve.op, tool, ctx.body, kernel)
}

#[derive(Debug, Clone, Copy)]
enum Blend {
    Fillet,
    Chamfer,
}

fn build_blend<K: Kernel>(
    ctx: &Context<'_>,
    blend: &EdgeBlend,
    kind: Blend,
    kernel: &mut K,
    warnings: &mut Vec<String>,
) -> Result<Body, String> {
    let body = ctx.body()?;
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
    let id = ctx.feature().id;
    let names = derived_names(id, &result.history, body);
    finish(id, result.shape, names, kernel)
}

fn build_shell<K: Kernel>(
    ctx: &Context<'_>,
    shell: &Shell,
    kernel: &mut K,
    warnings: &mut Vec<String>,
) -> Result<Body, String> {
    let body = ctx.body()?;
    if !(shell.thickness > 0.0) {
        return Err("the thickness must be positive".to_owned());
    }
    let mut faces = Vec::with_capacity(shell.faces.len());
    for name in &shell.faces {
        faces.push(resolve(
            body.naming.resolve_face(name),
            "face",
            name,
            warnings,
        )?);
    }
    faces.sort_unstable();
    faces.dedup();
    let offset = if shell.outward {
        shell.thickness
    } else {
        -shell.thickness
    };
    let result = kernel
        .shell(body.shape, &faces, offset)
        .map_err(|e| e.to_string())?;
    let id = ctx.feature().id;
    let names = derived_names(id, &result.history, body);
    finish(id, result.shape, names, kernel)
}

fn build_ref_plane(
    ctx: &Context<'_>,
    plane: &RefPlane,
    warnings: &mut Vec<String>,
) -> Result<ReferenceGeometry, String> {
    let base = ctx.plane(&plane.base, warnings)?;
    let offset = if plane.flip {
        -plane.offset
    } else {
        plane.offset
    };
    Ok(ReferenceGeometry::Plane(base.offset(offset)))
}

fn build_ref_axis(
    ctx: &Context<'_>,
    axis: &RefAxis,
    warnings: &mut Vec<String>,
) -> Result<ReferenceGeometry, String> {
    let axis = match axis {
        RefAxis::Along(axis) => ctx.axis(axis, warnings)?,
        RefAxis::TwoPlanes(a, b) => {
            let (a, b) = (ctx.plane(a, warnings)?, ctx.plane(b, warnings)?);
            Axis::intersection(&a, &b).ok_or_else(|| "the two planes are parallel".to_owned())?
        }
    };
    Ok(ReferenceGeometry::Axis(axis))
}

/// One copy of the seeds: its position in the pattern and how it is placed.
type Instance = ([u32; 2], Transform);

fn check_count(total: u64) -> Result<(), String> {
    if total > MAX_INSTANCES {
        Err(format!(
            "{total} instances is too many (at most {MAX_INSTANCES})"
        ))
    } else {
        Ok(())
    }
}

fn linear_instances(
    ctx: &Context<'_>,
    pattern: &LinearPattern,
    warnings: &mut Vec<String>,
) -> Result<Vec<Instance>, String> {
    let step = |direction: &crate::feature::PatternDirection,
                warnings: &mut Vec<String>|
     -> Result<oxyde_geom::Vec3, String> {
        let axis = ctx.axis(&direction.axis, warnings)?;
        let sign = if direction.reverse { -1.0 } else { 1.0 };
        Ok(axis.direction * (direction.spacing * sign))
    };
    let first = step(&pattern.first, warnings)?;
    let count1 = pattern.first.count.max(1);
    let (second, count2) = match &pattern.second {
        Some(direction) => (step(direction, warnings)?, direction.count.max(1)),
        None => (oxyde_geom::Vec3::ZERO, 1),
    };
    check_count(u64::from(count1) * u64::from(count2))?;
    let mut out = Vec::new();
    for j in 0..count2 {
        for i in 0..count1 {
            if i == 0 && j == 0 {
                continue;
            }
            let shift = first * f64::from(i) + second * f64::from(j);
            out.push(([i, j], Transform::translation(shift)));
        }
    }
    Ok(out)
}

fn circular_instances(
    ctx: &Context<'_>,
    pattern: &CircularPattern,
    warnings: &mut Vec<String>,
) -> Result<Vec<Instance>, String> {
    let mut axis = ctx.axis(&pattern.axis, warnings)?;
    if pattern.reverse {
        axis = axis.reversed();
    }
    let count = pattern.count.max(1);
    check_count(u64::from(count))?;
    let full_turn = std::f64::consts::TAU;
    let step = if !pattern.equal_spacing {
        pattern.angle
    } else if (pattern.angle - full_turn).abs() < 1e-9 {
        // All the way round: the last instance must not land on the seed.
        pattern.angle / f64::from(count)
    } else if count > 1 {
        pattern.angle / f64::from(count - 1)
    } else {
        0.0
    };
    Ok((1..count)
        .map(|i| ([i, 0], Transform::rotation(&axis, step * f64::from(i))))
        .collect())
}

fn mirror_instances(
    ctx: &Context<'_>,
    mirror: &Mirror,
    warnings: &mut Vec<String>,
) -> Result<Vec<Instance>, String> {
    let plane = ctx.plane(&mirror.plane, warnings)?;
    Ok(vec![([1, 0], Transform::mirror(&plane))])
}

/// Copies the tools of `seeds` to every instance and adds or removes each copy, seed by
/// seed. Faces of a copy are named after the seed's face and the instance.
fn build_instances<K: Kernel>(
    ctx: &Context<'_>,
    seeds: &[FeatureId],
    instances: &[Instance],
    kernel: &mut K,
) -> Result<Body, String> {
    let id = ctx.feature().id;
    let body = ctx.body()?;
    if seeds.is_empty() {
        return Err("no features to copy".to_owned());
    }
    let mut tools = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let (feature, result) = ctx.earlier(*seed)?;
        if !feature.kind.is_patternable() {
            return Err(format!(
                "{} cannot be copied: only extrusions and revolves can, for now",
                feature.name
            ));
        }
        let tool = result
            .tool
            .as_ref()
            .ok_or_else(|| format!("{} is suppressed or has errors", feature.name))?;
        tools.push(tool);
    }

    // The body being built, once it differs from the one before this feature.
    let mut current: Option<Body> = None;
    let outcome = (|| -> Result<(), String> {
        for tool in &tools {
            for (key, transform) in instances {
                let copy = kernel
                    .transform(tool.shape, transform)
                    .map_err(|e| e.to_string())?;
                let copy_names: Vec<Option<FaceName>> =
                    copy.history
                        .faces
                        .iter()
                        .map(|sources| {
                            sources.iter().find_map(|source| match *source {
                                FaceSource::InputFace { input: 0, face } => {
                                    tool.names.get(face as usize).cloned().flatten().map(
                                        |seed_face| {
                                            FaceName::new(FaceKey::Instance {
                                                feature: id,
                                                instance: *key,
                                                face: Box::new(seed_face),
                                            })
                                        },
                                    )
                                }
                                _ => None,
                            })
                        })
                        .collect();
                let base = current.as_ref().unwrap_or(body);
                let combined = kernel.boolean(tool.op, base.shape, copy.shape);
                kernel.release(copy.shape);
                let combined = combined.map_err(|e| e.to_string())?;
                let names = boolean_names(&combined.history, base, &copy_names);
                let built = finish(id, combined.shape, names, kernel)?;
                if let Some(old) = current.replace(built) {
                    kernel.release(old.shape);
                }
            }
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => current.ok_or_else(|| "the pattern has a single instance".to_owned()),
        Err(e) => {
            if let Some(partial) = current {
                kernel.release(partial.shape);
            }
            Err(e)
        }
    }
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
