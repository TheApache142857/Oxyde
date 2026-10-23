//! The part: a timeline of features, and the engine that rebuilds it.
//!
//! Oxyde is bottom-up, like SolidWorks: a part is modeled on its own as an ordered list of
//! features (the timeline shown on the left of the screen); assemblies will place finished
//! parts later. Rebuilding replays the timeline through a [`Kernel`], keeping after each
//! feature the resulting body and the persistent names of all its faces, edges and
//! vertices.

use std::collections::{BTreeSet, HashSet};
use std::f64::consts::TAU;
use std::fmt;

use oxyde_geom::{
    Axis, Path as SweepPath, PathError, Plane, Point2, Point3, Profile, ProfileError, Transform,
    Vec3,
};
use oxyde_kernel::{
    BooleanOp, CurveKind, FaceSource, Kernel, OpHistory, OpResult, ShapeHandle, SurfaceKind,
};
use oxyde_sketch::{ConstrainedSketch, ConstraintState, EntityId, Geometry};

use crate::appearance::{Appearance, Appearances};
use crate::bodies::{Body, Solid, Working};
use crate::configurations::Configurations;
use crate::driven::DrivenBy;
use crate::equations::{EquationResults, Equations};
use crate::error::ModelError;
use crate::feature::{
    AxisRef, BodyId, ChamferShape, CircularPattern, Combine, CombineOp, CurveRef, EdgeBlend,
    Extrude, ExtrudeExtent, ExtrudeOp, Feature, FeatureId, FeatureKind, Hole, HoleKind, Import,
    LinearPattern, Loft, Mirror, PlaneRef, PointRef, RefAxis, RefPlane, Revolve, RevolveAxis,
    Shell, Sketch, SketchPlane, Sweep,
};
use crate::material::Material;
use crate::naming::{FaceKey, FaceName, Resolution};
use oxyde_kernel::{StepColor, StepColors};

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
    /// The sketch's curves as one chain, or why they are not one (only an error for a
    /// sweep following the sketch).
    pub path: Result<SweepPath, PathError>,
}

/// Geometry of a reference feature.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReferenceGeometry {
    Plane(Plane),
    Axis(Axis),
}

/// What a swept tool does to the bodies (and what pattern copies of it do).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolOp {
    Join,
    Cut,
    Intersect,
    NewBody,
}

impl ToolOp {
    fn of(op: ExtrudeOp) -> Self {
        match op {
            ExtrudeOp::Boss => ToolOp::Join,
            ExtrudeOp::Cut => ToolOp::Cut,
            ExtrudeOp::Intersect => ToolOp::Intersect,
            ExtrudeOp::NewBody => ToolOp::NewBody,
        }
    }

    fn boolean(self) -> BooleanOp {
        match self {
            ToolOp::Join | ToolOp::NewBody => BooleanOp::Fuse,
            ToolOp::Cut => BooleanOp::Cut,
            ToolOp::Intersect => BooleanOp::Common,
        }
    }
}

/// The solid an extrusion, a revolve or a sweep swept, kept so that patterns and mirrors can copy
/// it and do the same with the copies. A pattern or a mirror keeps the copies it made the same
/// way, so that it can be patterned or mirrored in turn (SolidWorks' pattern of a pattern).
#[derive(Debug)]
struct ToolBody {
    shape: ShapeHandle,
    /// False when the tool became a body itself (a new body), which owns the shape.
    owned: bool,
    /// Name of each face of the tool.
    names: Vec<Option<FaceName>>,
    op: ToolOp,
    /// The bodies it joined, cut or intersected.
    targets: Vec<BodyId>,
    /// A pattern's or a mirror's other copies, beyond `shape`, with the names of their
    /// faces; always owned. Empty for the tool of a single feature.
    more: Vec<(ShapeHandle, Vec<Option<FaceName>>)>,
}

impl ToolBody {
    /// Every solid of the tool, with the names of its faces.
    fn pieces(&self) -> impl Iterator<Item = (ShapeHandle, &[Option<FaceName>])> {
        std::iter::once((self.shape, self.names.as_slice())).chain(
            self.more
                .iter()
                .map(|(shape, names)| (*shape, names.as_slice())),
        )
    }
}

#[derive(Debug)]
struct FeatureResult {
    status: FeatureStatus,
    /// The body after this feature, if the feature changed it.
    produced: Option<Body>,
    sketch: Option<SketchResult>,
    tool: Option<ToolBody>,
    reference: Option<ReferenceGeometry>,
    /// Appearances an import feature read from its file, to be put on the part once.
    import_appearances: Option<ImportAppearances>,
}

/// The colours a STEP file gave the solids and faces an import feature added, as the
/// appearances they become.
#[derive(Debug, Clone, PartialEq, Default)]
struct ImportAppearances {
    body: Option<(BodyId, Appearance)>,
    faces: Vec<(FaceName, Appearance)>,
    /// The material the file names for the part, as the part's when it has none yet.
    material: Option<Material>,
}

impl FeatureResult {
    fn with_status(status: FeatureStatus) -> Self {
        Self {
            status,
            produced: None,
            sketch: None,
            tool: None,
            reference: None,
            import_appearances: None,
        }
    }

    fn release<K: Kernel>(self, kernel: &mut K) {
        if let Some(body) = self.produced {
            body.release(kernel);
        }
        if let Some(tool) = self.tool {
            if tool.owned {
                kernel.release(tool.shape);
            }
            for (shape, _) in tool.more {
                kernel.release(shape);
            }
        }
    }
}

/// A feature built on a part without being added to it ([`Part::preview`]): the body it
/// would make, or the plane or axis, or why it cannot be built.
///
/// It holds kernel shapes, and may share others with the part's cached results: give it
/// back with [`FeaturePreview::release`] before the part is rebuilt or released.
#[derive(Debug)]
pub struct FeaturePreview {
    feature: FeatureId,
    name: String,
    result: FeatureResult,
}

impl FeaturePreview {
    /// The id the feature has, or would get once added. Faces it makes are named after it
    /// (`FaceKey::feature`).
    pub fn feature(&self) -> FeatureId {
        self.feature
    }

    /// The feature's name ("Boss-Extrude3"), as it is or would be.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Built (perhaps with a warning), or why not.
    pub fn status(&self) -> &FeatureStatus {
        &self.result.status
    }

    /// Every body of the part after the feature, when the feature changes them.
    pub fn body(&self) -> Option<&Body> {
        self.result.produced.as_ref()
    }

    /// The plane or axis of a reference feature.
    pub fn reference(&self) -> Option<ReferenceGeometry> {
        self.result.reference
    }

    /// Frees the shapes the preview made (not those it shares with the part).
    pub fn release<K: Kernel>(self, kernel: &mut K) {
        self.result.release(kernel);
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
    /// Colours, transparency and textures of the part, its bodies and faces.
    pub appearances: Appearances,
    /// What the part is made of: its look where no appearance overrides it, and its
    /// density.
    pub material: Option<Material>,
    /// Named variants of the part: values of configured dimensions and suppression of
    /// configured features, one of them active.
    pub configurations: Configurations,
    /// Values of the part that assemblies' equations drive (SolidWorks' external
    /// references), sorted.
    pub driven_by: Vec<DrivenBy>,
}

impl PartDefinition {
    /// Takes away every appearance of the part, the colours its imported STEP files have
    /// not given it yet included: an import feature still due to apply them would put them
    /// back as appearances on the next rebuild, over the material. Used on a part file
    /// that has not been rebuilt, such as a material given to parts from an assembly.
    pub fn clear_appearances(&mut self) {
        self.appearances = Appearances::default();
        for feature in &mut self.features {
            if let FeatureKind::Import(import) = &mut feature.kind {
                import.apply_colors = false;
            }
        }
    }

    /// Takes from `built`, the same timeline once rebuilt, what its import features did on
    /// that rebuild: the colours and the material their STEP files gave the part, which
    /// this definition's imports are still due to apply. Used before writing a change
    /// into a part file that was never rebuilt on its own (from an assembly): without it,
    /// the next rebuild applies them again, over what the change did, such as a STEP
    /// material coming back on a part whose material was removed.
    ///
    /// Changes nothing and returns false when no import of this definition is still due
    /// to apply its file's colours, or when `built` is not the same timeline.
    pub fn settle_imports(&mut self, built: &PartDefinition) -> bool {
        let pending = self
            .features
            .iter()
            .any(|f| matches!(&f.kind, FeatureKind::Import(i) if i.apply_colors));
        if !pending {
            return false;
        }
        // The same timeline but for what the rebuild turned off (an import rolled back or
        // suppressed in `built` is still pending there, and makes them differ).
        let settled = |features: &[Feature]| {
            let mut features = features.to_vec();
            for feature in &mut features {
                if let FeatureKind::Import(import) = &mut feature.kind {
                    import.apply_colors = false;
                }
            }
            features
        };
        if settled(&self.features) != built.features {
            return false;
        }
        self.features = built.features.clone();
        self.appearances = built.appearances.clone();
        self.material = built.material.clone();
        true
    }
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
    appearances: Appearances,
    material: Option<Material>,
    pub(crate) configurations: Configurations,
    pub(crate) driven_by: Vec<DrivenBy>,
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
            appearances: Appearances::default(),
            material: None,
            configurations: Configurations::default(),
            driven_by: Vec::new(),
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
        definition.configurations.validate()?;
        crate::driven::tidy(&mut definition.driven_by);
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
            appearances: definition.appearances,
            material: definition.material.map(Material::clamped),
            configurations: definition.configurations,
            driven_by: definition.driven_by,
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
            appearances: self.appearances.clone(),
            material: self.material.clone(),
            configurations: self.configurations.clone(),
            driven_by: self.driven_by.clone(),
        }
    }

    // ---- Appearances and material ------------------------------------------------------------

    /// Colours, transparency and textures of the part, its bodies and faces.
    pub fn appearances(&self) -> &Appearances {
        &self.appearances
    }

    /// Changing appearances needs no rebuild: the viewport just redraws.
    pub fn appearances_mut(&mut self) -> &mut Appearances {
        &mut self.appearances
    }

    /// What the part is made of, if it has been given a material.
    pub fn material(&self) -> Option<&Material> {
        self.material.as_ref()
    }

    /// Gives the part a material (a copy of a library entry), or takes it away with
    /// `None`. Needs no rebuild.
    pub fn set_material(&mut self, material: Option<Material>) {
        self.material = material.map(Material::clamped);
    }

    /// Whether anything changes how the part is drawn: an appearance or a material.
    pub fn has_appearances(&self) -> bool {
        !self.appearances.is_empty() || self.material.is_some()
    }

    /// The colours to write with the current body's solids to a STEP file, indexed like
    /// [`Body::shapes`]: each solid's appearance (its body's, else the part's, else the
    /// material's), and the faces whose appearance differs from it.
    pub fn step_colors(&self, body: &Body) -> Vec<StepColors> {
        let to_color = |a: &Appearance| StepColor {
            rgb: a.color,
            transparency: a.transparency,
        };
        body.solids
            .iter()
            .map(|solid| {
                let solid_color = self
                    .appearances
                    .body(solid.id)
                    .or(self.appearances.part.as_ref())
                    .or(self.material.as_ref().map(|m| &m.appearance))
                    .map(to_color);
                let faces = solid
                    .faces
                    .clone()
                    .map(|index| {
                        let face = self.face_appearance(body, index).map(to_color);
                        if face == solid_color { None } else { face }
                    })
                    .collect();
                StepColors {
                    solids: vec![solid_color],
                    faces,
                }
            })
            .collect()
    }

    /// The appearance face `index` of `body` (the part's current body) is drawn with: its
    /// own, else its body's, else the part's, else the material's; `None` for the
    /// viewport's default.
    pub fn face_appearance(&self, body: &Body, index: u32) -> Option<&Appearance> {
        if !self.has_appearances() {
            return None;
        }
        let name = body.naming.face(index);
        let solid = body.solid_of_face(index).map(|s| s.id);
        self.appearances
            .of_face(name, solid)
            .or(self.material.as_ref().map(|m| &m.appearance))
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
    /// rebuilt. To swap in a whole new definition, prefer [`Part::replace_feature`], which
    /// checks its references.
    pub fn edit(&mut self, id: FeatureId) -> Result<&mut FeatureKind, ModelError> {
        let index = self.require(id)?;
        self.mark_dirty(index);
        Ok(&mut self.features[index].kind)
    }

    /// Replaces a feature's definition (after editing it in its window). Refused when the
    /// new definition refers to the feature itself or to a later one: only what comes before
    /// a feature exists when it is built, and such a part could not be opened again. User
    /// names of dimensions the new definition no longer has are dropped.
    pub fn replace_feature(&mut self, id: FeatureId, kind: FeatureKind) -> Result<(), ModelError> {
        let index = self.require(id)?;
        let earlier = &self.features[..index];
        if let Some(reference) = kind
            .dependencies()
            .into_iter()
            .find(|d| !earlier.iter().any(|f| f.id == *d))
        {
            return Err(ModelError::LaterReference {
                feature: id,
                reference,
            });
        }
        // A dimension an assembly drives keeps its value, as SolidWorks keeps an external
        // reference: the edit is refused.
        let held = self.assembly_driven_values(id);
        let feature = &mut self.features[index];
        let old = std::mem::replace(&mut feature.kind, kind);
        feature.tidy_parameter_names();
        self.mark_dirty(index);
        self.keep_assembly_driven_values(id, old, &held)
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
        self.appearances.remove_feature(id);
        self.configurations.forget_feature(id);
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
            let mut result = if self.features[index].suppressed {
                FeatureResult::with_status(FeatureStatus::Suppressed)
            } else {
                build_feature(
                    &self.features[..index],
                    &self.features[index],
                    &self.results,
                    kernel,
                )
            };
            if let Some(imported) = result.import_appearances.take() {
                self.apply_import_appearances(index, imported);
            }
            self.results.push(result);
        }
        self.dirty_from = end;
    }

    /// Puts the colours an import feature read from its file on the part, once: a body
    /// appearance for the solid and face appearances where faces differ from it. Faces and
    /// bodies that already have an appearance keep it.
    fn apply_import_appearances(&mut self, index: usize, imported: ImportAppearances) {
        if let FeatureKind::Import(import) = &mut self.features[index].kind {
            import.apply_colors = false;
        }
        if let Some((body, appearance)) = imported.body {
            if self.appearances.body(body).is_none() {
                self.appearances.set_body(body, Some(appearance));
            }
        }
        for (face, appearance) in imported.faces {
            if self.appearances.face(&face).is_none() {
                self.appearances.set_face(&face, Some(appearance));
            }
        }
        if let Some(material) = imported.material {
            if self.material.is_none() {
                self.material = Some(material);
            }
        }
    }

    /// Frees every cached kernel shape. The next rebuild starts from scratch.
    pub fn release<K: Kernel>(&mut self, kernel: &mut K) {
        for result in self.results.drain(..) {
            result.release(kernel);
        }
        self.dirty_from = 0;
    }

    /// Builds `kind` the way it would be built if it were added at the rollback bar
    /// (`replacing` = `None`) or put in place of feature `replacing`, on the results of the
    /// last rebuild, without changing the part: what the viewport shows while a feature's
    /// command is open. Only that one feature is built, so features after the one replaced
    /// are not (as SolidWorks rolls back to a feature while it is edited).
    ///
    /// `None` when the part is not rebuilt up to there, or `replacing` is not one of its
    /// active features. The preview's body may share shapes with the part's results: it
    /// is valid until the part is rebuilt or released, and must be given back with
    /// [`FeaturePreview::release`] before then.
    pub fn preview<K: Kernel>(
        &self,
        kind: FeatureKind,
        replacing: Option<FeatureId>,
        kernel: &mut K,
    ) -> Option<FeaturePreview> {
        let (index, feature) = match replacing {
            Some(id) => {
                let index = self.index_of(id)?;
                if index >= self.active_end() {
                    return None;
                }
                let mut feature = self.features[index].clone();
                feature.kind = kind;
                feature.suppressed = false;
                (index, feature)
            }
            None => {
                let id = FeatureId::from_raw(self.next_id);
                let name = self.next_name(kind.type_name());
                (self.active_end(), Feature::new(id, name, false, kind))
            }
        };
        if index > self.built() {
            return None;
        }
        let result = build_feature(
            &self.features[..index],
            &feature,
            &self.results[..index],
            kernel,
        );
        Some(FeaturePreview {
            feature: feature.id,
            name: feature.name,
            result,
        })
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

    /// The solved sketch of a built sketch feature. A sketch whose relations conflict
    /// (status [`FeatureStatus::Error`]) is kept as stored, unsolved, so that it can be
    /// shown and opened again.
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
/// as it is just before it. The feature itself need not be in the timeline (a preview).
struct Context<'a> {
    earlier: &'a [Feature],
    feature: &'a Feature,
    previous: &'a [FeatureResult],
    body: Option<&'a Body>,
}

/// Pattern instances above this count are refused, to keep a typo from freezing the
/// application.
const MAX_INSTANCES: u64 = 1000;

impl<'a> Context<'a> {
    fn feature(&self) -> &'a Feature {
        self.feature
    }

    /// An earlier feature and its result.
    fn earlier(&self, id: FeatureId) -> Result<(&'a Feature, &'a FeatureResult), String> {
        let index = self
            .earlier
            .iter()
            .position(|f| f.id == id)
            .ok_or_else(|| format!("{id} must come before this feature"))?;
        Ok((&self.earlier[index], &self.previous[index]))
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
            AxisRef::SketchLine { sketch, entity } => {
                let (name, solved) = self.solved_sketch(*sketch)?;
                let (start, end) = match solved.solved.entity(EntityId(*entity)) {
                    Some(e) => match e.geometry {
                        Geometry::Line { start, end } => (start, end),
                        _ => return Err(format!("entity {entity} of {name} is not a line")),
                    },
                    None => return Err(gone_from_sketch("line", *entity, name)),
                };
                let point = |id: EntityId| {
                    solved
                        .solved
                        .position(id)
                        .map(|p| solved.plane.to_world(p))
                        .ok_or_else(|| format!("line {entity} of {name} has lost an end"))
                };
                let (a, b) = (point(start)?, point(end)?);
                Axis::new(a, b - a).ok_or_else(|| format!("line {entity} of {name} has no length"))
            }
        }
    }

    /// The solved geometry of an earlier sketch, with its name: what references to its
    /// points and curves read.
    fn solved_sketch(&self, sketch: FeatureId) -> Result<(&'a str, &'a SketchResult), String> {
        let (feature, result) = self.earlier(sketch)?;
        let name = feature.name.as_str();
        if matches!(result.status, FeatureStatus::Error(_)) {
            return Err(format!("{name} has errors"));
        }
        let solved = result
            .sketch
            .as_ref()
            .ok_or_else(|| format!("{name} is suppressed or has errors"))?;
        Ok((name, solved))
    }

    fn point(&self, point: &PointRef, warnings: &mut Vec<String>) -> Result<Point3, String> {
        match point {
            PointRef::Vertex(name) => {
                let body = self
                    .body
                    .ok_or_else(|| format!("vertex {name} not found: no body yet"))?;
                let index = resolve(body.naming.resolve_vertex(name), "vertex", name, warnings)?;
                body.topology
                    .vertices
                    .get(index as usize)
                    .map(|v| v.point)
                    .ok_or_else(|| format!("vertex {name} no longer exists: select it again"))
            }
            PointRef::SketchPoint { sketch, entity } => {
                let (name, solved) = self.solved_sketch(*sketch)?;
                match solved.solved.entity(EntityId(*entity)) {
                    Some(e) => match e.geometry {
                        Geometry::Point(p) => Ok(solved.plane.to_world(p)),
                        _ => Err(format!("entity {entity} of {name} is not a point")),
                    },
                    None => Err(gone_from_sketch("point", *entity, name)),
                }
            }
        }
    }

    /// Where a curve passes closest to `near`, and which way it runs there (unit length).
    fn curve_tangent<K: Kernel>(
        &self,
        curve: &CurveRef,
        near: Point3,
        kernel: &K,
        warnings: &mut Vec<String>,
    ) -> Result<(Point3, Vec3), String> {
        match curve {
            CurveRef::Edge(name) => {
                let body = self.body()?;
                let index = resolve(body.naming.resolve_edge(name), "edge", name, warnings)?;
                let edge = body
                    .topology
                    .edges
                    .get(index as usize)
                    .ok_or_else(|| format!("edge {name} no longer exists: select it again"))?;
                // The kernel knows every kind of curve; lines and circles are worked out
                // here when it cannot tell.
                let local = body
                    .solid_of_edge(index)
                    .and_then(|s| Some((s.shape, s.local_edge(index)?)));
                if let Some((shape, local)) = local
                    && let Some(t) = kernel
                        .edge_tangent(shape, local, near)
                        .map_err(|e| format!("edge {name}: {e}"))?
                {
                    return Ok((t.point, t.tangent));
                }
                match (edge.curve, edge.axis) {
                    (CurveKind::Line, Some(line)) => {
                        let t = (near - line.origin)
                            .dot(line.direction)
                            .clamp(0.0, edge.length);
                        Ok((line.origin + line.direction * t, line.direction))
                    }
                    (CurveKind::Circle, Some(axis)) => {
                        let v = near - axis.origin;
                        let along = v.dot(axis.direction);
                        let tangent = axis
                            .direction
                            .cross(v - axis.direction * along)
                            .normalized()
                            .ok_or_else(|| format!("the point is on the axis of edge {name}"))?;
                        Ok((near - axis.direction * along, tangent))
                    }
                    _ => Err(format!(
                        "the direction of edge {name} cannot be worked out: pick another curve"
                    )),
                }
            }
            CurveRef::SketchCurve { sketch, entity } => {
                let (name, solved) = self.solved_sketch(*sketch)?;
                let plane = &solved.plane;
                let local = near - plane.origin;
                let q = Point2::new(local.dot(plane.x_dir), local.dot(plane.y_dir()));
                let geometry = solved
                    .solved
                    .entity(EntityId(*entity))
                    .map(|e| e.geometry)
                    .ok_or_else(|| gone_from_sketch("curve", *entity, name))?;
                let at = |id: EntityId| {
                    solved
                        .solved
                        .position(id)
                        .ok_or_else(|| format!("curve {entity} of {name} has lost a point"))
                };
                let (point, (tx, ty)) = match geometry {
                    Geometry::Point(_) => {
                        return Err(format!("entity {entity} of {name} is a point, not a curve"));
                    }
                    Geometry::Line { start, end } => {
                        let (a, b) = (at(start)?, at(end)?);
                        let (dx, dy) = (b.x - a.x, b.y - a.y);
                        let length = dx.hypot(dy);
                        if length <= 1e-9 {
                            return Err(format!("line {entity} of {name} has no length"));
                        }
                        let t = (((q.x - a.x) * dx + (q.y - a.y) * dy) / (length * length))
                            .clamp(0.0, 1.0);
                        (
                            Point2::new(a.x + dx * t, a.y + dy * t),
                            (dx / length, dy / length),
                        )
                    }
                    Geometry::Circle { center, radius } => {
                        let c = at(center)?;
                        circle_point(c, radius, (q.y - c.y).atan2(q.x - c.x))
                    }
                    Geometry::Arc { center, start, end } => {
                        let (c, s, e) = (at(center)?, at(start)?, at(end)?);
                        let radius = (s.x - c.x).hypot(s.y - c.y);
                        let from = (s.y - c.y).atan2(s.x - c.x);
                        let sweep = ((e.y - c.y).atan2(e.x - c.x) - from).rem_euclid(TAU);
                        let sweep = if sweep <= 1e-12 { TAU } else { sweep };
                        // Counter-clockwise from the start; past the end, the nearer end.
                        let mut t = ((q.y - c.y).atan2(q.x - c.x) - from).rem_euclid(TAU);
                        if t > sweep {
                            t = if t - sweep < TAU - t { sweep } else { 0.0 };
                        }
                        circle_point(c, radius, from + t)
                    }
                };
                let tangent = plane.x_dir * tx + plane.y_dir() * ty;
                Ok((plane.to_world(point), tangent))
            }
        }
    }

    /// The solved sketch an extrusion, a revolve or a sweep uses, with a closed profile.
    fn profile_sketch(&self, sketch: FeatureId) -> Result<(&'a SketchResult, &'a Profile), String> {
        let (feature, result) = self.earlier(sketch)?;
        let name = &feature.name;
        if matches!(result.status, FeatureStatus::Error(_)) {
            return Err(format!("{name} has errors"));
        }
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

    /// The solved sketch a sweep follows, with its curves chained into one path.
    fn path_sketch(&self, sketch: FeatureId) -> Result<(&'a SketchResult, &'a SweepPath), String> {
        let (feature, result) = self.earlier(sketch)?;
        let name = &feature.name;
        if matches!(result.status, FeatureStatus::Error(_)) {
            return Err(format!("{name} has errors"));
        }
        let solved = result
            .sketch
            .as_ref()
            .ok_or_else(|| format!("{name} is suppressed or has errors"))?;
        let path = solved.path.as_ref().map_err(|e| format!("{name}: {e}"))?;
        Ok((solved, path))
    }
}

/// Builds `feature` on the features `earlier` (with their results `previous`, one each).
fn build_feature<K: Kernel>(
    earlier: &[Feature],
    feature: &Feature,
    previous: &[FeatureResult],
    kernel: &mut K,
) -> FeatureResult {
    let ctx = Context {
        earlier,
        feature,
        previous,
        body: current_body(previous),
    };
    let feature = ctx.feature();
    let mut warnings = Vec::new();
    let mut result = FeatureResult::with_status(FeatureStatus::Ok);

    // A panic while building (in the kernel or here) fails the feature like any other
    // error, with the rest of the timeline still built; the document stays whole.
    let outcome = oxyde_kernel::catch_panic(|| -> Result<(), String> {
        match &feature.kind {
            FeatureKind::Sketch(sketch) => match build_sketch(&ctx, sketch, &mut warnings) {
                Ok(built) => result.sketch = Some(built),
                Err(failure) => {
                    result.sketch = failure.partial;
                    return Err(failure.message);
                }
            },
            FeatureKind::Extrude(extrude) => {
                let (body, tool) = build_extrude(&ctx, extrude, kernel, &mut warnings)?;
                result.produced = Some(body);
                result.tool = Some(tool);
            }
            FeatureKind::Revolve(revolve) => {
                let (body, tool) = build_revolve(&ctx, revolve, kernel, &mut warnings)?;
                result.produced = Some(body);
                result.tool = Some(tool);
            }
            FeatureKind::Sweep(sweep) => {
                let (body, tool) = build_sweep(&ctx, sweep, kernel, &mut warnings)?;
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
                result.reference = Some(build_ref_plane(&ctx, plane, kernel, &mut warnings)?);
            }
            FeatureKind::Axis(axis) => {
                result.reference = Some(build_ref_axis(&ctx, axis, &mut warnings)?);
            }
            FeatureKind::LinearPattern(pattern) => {
                let instances = linear_instances(&ctx, pattern, &mut warnings)?;
                let skip = skipped_instances(
                    &pattern.skip,
                    &instances,
                    LinearPattern::instance_label,
                    &mut warnings,
                );
                let (body, tool) =
                    build_instances(&ctx, &pattern.seeds, &instances, &skip, kernel)?;
                result.produced = Some(body);
                result.tool = tool;
            }
            FeatureKind::CircularPattern(pattern) => {
                let instances = circular_instances(&ctx, pattern, &mut warnings)?;
                let skip = skipped_instances(
                    &pattern.skip,
                    &instances,
                    CircularPattern::instance_label,
                    &mut warnings,
                );
                let (body, tool) =
                    build_instances(&ctx, &pattern.seeds, &instances, &skip, kernel)?;
                result.produced = Some(body);
                result.tool = tool;
            }
            FeatureKind::Mirror(mirror) => {
                let instances = mirror_instances(&ctx, mirror, &mut warnings)?;
                if mirror.bodies.is_empty() {
                    let (body, tool) =
                        build_instances(&ctx, &mirror.seeds, &instances, &BTreeSet::new(), kernel)?;
                    result.produced = Some(body);
                    result.tool = tool;
                } else if mirror.seeds.is_empty() {
                    result.produced =
                        Some(mirror_bodies(&ctx, &mirror.bodies, &instances, kernel)?);
                } else {
                    return Err(
                        "a mirror copies features or bodies, not both: make two mirrors".to_owned(),
                    );
                }
            }
            FeatureKind::Import(import) => {
                let (body, appearances) = build_import(&ctx, import, kernel)?;
                result.produced = Some(body);
                result.import_appearances = appearances;
            }
            FeatureKind::Combine(combine) => {
                result.produced = Some(build_combine(&ctx, combine, kernel, &mut warnings)?);
            }
            FeatureKind::Hole(hole) => {
                let (body, tool) = build_hole(&ctx, hole, kernel, &mut warnings)?;
                result.produced = Some(body);
                result.tool = Some(tool);
            }
            FeatureKind::Loft(loft) => {
                let (body, tool) = build_loft(&ctx, loft, kernel, &mut warnings)?;
                result.produced = Some(body);
                result.tool = Some(tool);
            }
        }
        Ok(())
    })
    .unwrap_or_else(|message| Err(format!("internal error: {message}")));

    match outcome {
        Ok(()) => {
            if !warnings.is_empty() {
                result.status = FeatureStatus::Warning(warnings.join("; "));
            }
            result
        }
        Err(message) => {
            // Nothing is kept from a failed feature, but the geometry of a sketch whose
            // relations conflict (as stored, unsolved): it is still shown, and can be
            // opened to be repaired.
            let sketch = result.sketch.take();
            result.release(kernel);
            let mut failed = FeatureResult::with_status(FeatureStatus::Error(message));
            failed.sketch = sketch;
            failed
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

/// Why a reference to a point or a curve of a sketch no longer holds: it was deleted.
fn gone_from_sketch(what: &str, entity: u32, sketch: &str) -> String {
    format!("{what} {entity} of {sketch} no longer exists: select it again")
}

/// The point of a circle at `angle`, and its counter-clockwise direction there.
fn circle_point(center: Point2, radius: f64, angle: f64) -> (Point2, (f64, f64)) {
    let (sin, cos) = angle.sin_cos();
    (
        Point2::new(center.x + radius * cos, center.y + radius * sin),
        (-sin, cos),
    )
}

fn face_plane(body: &Body, index: u32) -> Option<Plane> {
    let face = body.topology.faces.get(index as usize)?;
    if face.surface != SurfaceKind::Plane {
        return None;
    }
    Plane::from_point_normal(face.centroid, face.normal?)
}

/// Why a sketch could not be built, and what could be kept of it: a sketch whose
/// relations conflict keeps its geometry as stored, so that it is still drawn and can be
/// opened to be repaired (a sketch without its plane keeps nothing).
struct SketchFailure {
    message: String,
    partial: Option<SketchResult>,
}

impl From<String> for SketchFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            partial: None,
        }
    }
}

fn build_sketch(
    ctx: &Context<'_>,
    sketch: &Sketch,
    warnings: &mut Vec<String>,
) -> Result<SketchResult, SketchFailure> {
    let plane = match &sketch.plane {
        SketchPlane::Standard(standard) => standard.plane(),
        SketchPlane::Face(name) => ctx.plane(&PlaneRef::Face(name.clone()), warnings)?,
        SketchPlane::Reference(id) => ctx.plane(&PlaneRef::Reference(*id), warnings)?,
    };
    let mut solved = sketch.geometry.clone();
    let report = solved.solve().map_err(|e| e.to_string())?;
    if !report.converged {
        // The geometry is left as stored: an over-defined sketch must still be seen, and
        // opened to take the conflicting relation away.
        return Err(SketchFailure {
            message: format!(
                "the sketch is over-defined: {} conflicting relation(s)",
                report.count(ConstraintState::Conflicting).max(1)
            ),
            partial: Some(SketchResult {
                plane,
                profile: Profile::from_curves(&solved.profile_segments()),
                path: SweepPath::from_curves(&solved.profile_segments()),
                degrees_of_freedom: 0,
                solved,
            }),
        });
    }
    let redundant = report.count(ConstraintState::Redundant);
    if redundant > 0 {
        warnings.push(format!("{redundant} redundant relation(s) in the sketch"));
    }
    Ok(SketchResult {
        plane,
        profile: Profile::from_curves(&solved.profile_segments()),
        path: SweepPath::from_curves(&solved.profile_segments()),
        degrees_of_freedom: report.degrees_of_freedom,
        solved,
    })
}

/// Several sources can name one face (e.g. coplanar faces merged by a boolean); the name
/// from the oldest feature wins, so that existing references stay valid.
fn oldest(names: impl Iterator<Item = FaceName>) -> Option<FaceName> {
    names.min_by(|a, b| a.key.feature().cmp(&b.key.feature()).then_with(|| a.cmp(b)))
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

/// Names after a boolean of input 0 (its faces named `a`) with inputs 1, 2… (named
/// `tools[0]`, `tools[1]`…).
fn boolean_names(
    history: &OpHistory,
    a: &[Option<FaceName>],
    tools: &[&[Option<FaceName>]],
) -> Vec<Option<FaceName>> {
    history
        .faces
        .iter()
        .map(|sources| {
            oldest(sources.iter().filter_map(|source| {
                match *source {
                    FaceSource::InputFace { input: 0, face } => {
                        a.get(face as usize).cloned().flatten()
                    }
                    FaceSource::InputFace { input, face } => tools
                        .get(input as usize - 1)
                        .and_then(|names| names.get(face as usize))
                        .cloned()
                        .flatten(),
                    _ => None,
                }
            }))
        })
        .collect()
}

/// Names after an operation on one body (fillet, chamfer, shell): faces kept from the body,
/// faces made from its edges and vertices, and offset copies of its faces. Inputs are
/// numbered in `solid`'s own topology.
fn derived_names(
    id: FeatureId,
    history: &OpHistory,
    body: &Body,
    solid: &Solid,
) -> Vec<Option<FaceName>> {
    let face = |f: u32| body.face_name(solid, f);
    let edge = |e: u32| body.naming.edge(solid.edges.start + e).cloned();
    let vertex = |v: u32| body.naming.vertex(solid.vertices.start + v).cloned();
    history
        .faces
        .iter()
        .map(|sources| {
            oldest(sources.iter().filter_map(|source| match *source {
                FaceSource::InputFace { input: 0, face: f } => face(f),
                FaceSource::InputEdge { input: 0, edge: e } => {
                    edge(e).map(|e| FaceName::from_edge(id, e))
                }
                FaceSource::InputVertex {
                    input: 0,
                    vertex: v,
                } => vertex(v).map(|v| {
                    FaceName::new(FaceKey::FromVertex {
                        feature: id,
                        vertex: v,
                    })
                }),
                FaceSource::OffsetFace { input: 0, face: f } => face(f).map(|f| {
                    FaceName::new(FaceKey::Offset {
                        feature: id,
                        face: Box::new(f),
                    })
                }),
                _ => None,
            }))
        })
        .collect()
}

/// Booleans `tool` (faces named `tool_names`) into body `index` of `work`, in place.
fn combine_into<K: Kernel>(
    work: &mut Working,
    index: usize,
    op: BooleanOp,
    tool: ShapeHandle,
    tool_names: &[Option<FaceName>],
    kernel: &mut K,
) -> Result<(), String> {
    combine_all_into(work, index, op, &[(tool, tool_names)], kernel)
}

/// Booleans several tools (each with its faces' names) into body `index` of `work`, in
/// one operation.
fn combine_all_into<K: Kernel>(
    work: &mut Working,
    index: usize,
    op: BooleanOp,
    tools: &[(ShapeHandle, &[Option<FaceName>])],
    kernel: &mut K,
) -> Result<(), String> {
    let target = work.pieces[index].shape;
    let shapes: Vec<ShapeHandle> = tools.iter().map(|(shape, _)| *shape).collect();
    let result = kernel
        .boolean_many(op, target, &shapes)
        .map_err(|e| e.to_string())?;
    let tool_names: Vec<&[Option<FaceName>]> = tools.iter().map(|(_, names)| *names).collect();
    let names = boolean_names(&result.history, work.names(index), &tool_names);
    work.replace(index, result.shape, names, kernel)
}

/// Booleans body `other` into body `target` and removes `other` (unless `keep`).
fn merge_bodies<K: Kernel>(
    work: &mut Working,
    target: BodyId,
    other: BodyId,
    op: BooleanOp,
    keep: bool,
    kernel: &mut K,
) -> Result<(), String> {
    let o = work
        .find(other)
        .ok_or_else(|| format!("body {other} no longer exists"))?;
    let (shape, names) = (work.pieces[o].shape, work.names(o).to_vec());
    let t = work
        .find(target)
        .ok_or_else(|| "the target body was removed entirely".to_owned())?;
    combine_into(work, t, op, shape, &names, kernel)?;
    if !keep {
        if let Some(o) = work.find(other) {
            work.remove(o, kernel);
        }
    }
    Ok(())
}

/// The body a join or an intersection goes into when none is chosen: the body the sketch
/// is on, else the newest body.
fn default_target(ctx: &Context<'_>, sketch: Option<FeatureId>) -> Option<BodyId> {
    let body = ctx.body?;
    let on_face =
        sketch
            .and_then(|id| ctx.earlier(id).ok())
            .and_then(|(feature, _)| match &feature.kind {
                FeatureKind::Sketch(Sketch {
                    plane: SketchPlane::Face(name),
                    ..
                }) => match body.naming.resolve_face(name) {
                    Resolution::Exact(i) | Resolution::Fallback(i) => body.solid_of_face(i),
                    _ => None,
                },
                _ => None,
            });
    on_face.or(body.solids.last()).map(|s| s.id)
}

/// The bodies a tool works on: those chosen, else the default for its operation.
fn tool_targets(
    ctx: &Context<'_>,
    op: ToolOp,
    chosen: &[BodyId],
    sketch: Option<FeatureId>,
) -> Result<Vec<BodyId>, String> {
    if !chosen.is_empty() {
        for id in chosen {
            if ctx.body.and_then(|b| b.solid(*id)).is_none() {
                return Err(format!(
                    "body {id} no longer exists: choose the bodies again"
                ));
            }
        }
        return Ok(chosen.to_vec());
    }
    Ok(match op {
        ToolOp::NewBody => Vec::new(),
        ToolOp::Cut => ctx
            .body
            .map(|b| b.solids.iter().map(|s| s.id).collect())
            .unwrap_or_default(),
        ToolOp::Join | ToolOp::Intersect => default_target(ctx, sketch).into_iter().collect(),
    })
}

/// Joins, cuts, intersects or adds as a new body a swept tool: the bodies after the
/// feature, and the tool kept for patterns.
/// Whether a boolean left a body as it was, by its volume: a cut that missed it.
fn same_volume(before: Option<f64>, after: Option<f64>) -> bool {
    match (before, after) {
        (Some(b), Some(a)) => (b - a).abs() <= 1e-9 * b.abs().max(1.0),
        _ => false,
    }
}

fn apply_tool<K: Kernel>(
    ctx: &Context<'_>,
    op: ExtrudeOp,
    chosen: &[BodyId],
    sketch: FeatureId,
    tool: OpResult,
    kernel: &mut K,
    warnings: &mut Vec<String>,
) -> Result<(Body, ToolBody), String> {
    let names = sweep_names(ctx.feature().id, &tool.history);
    let tool = NamedTool {
        shape: tool.shape,
        names,
    };
    apply_named_tool(ctx, op, chosen, sketch, tool, kernel, warnings)
}

/// A tool body and the names of its faces.
struct NamedTool {
    shape: ShapeHandle,
    names: Vec<Option<FaceName>>,
}

/// [`apply_tool`] for a tool whose faces are named already.
fn apply_named_tool<K: Kernel>(
    ctx: &Context<'_>,
    op: ExtrudeOp,
    chosen: &[BodyId],
    sketch: FeatureId,
    tool: NamedTool,
    kernel: &mut K,
    warnings: &mut Vec<String>,
) -> Result<(Body, ToolBody), String> {
    let id = ctx.feature().id;
    let tool_names = tool.names;
    let requested = ToolOp::of(op);
    let mut op = requested;
    let targets = match tool_targets(ctx, op, chosen, Some(sketch)) {
        Ok(t) => t,
        Err(e) => {
            kernel.release(tool.shape);
            return Err(e);
        }
    };
    if targets.is_empty() {
        match op {
            // The first boss of a part makes its first body.
            ToolOp::Join | ToolOp::NewBody => op = ToolOp::NewBody,
            ToolOp::Cut | ToolOp::Intersect => {
                kernel.release(tool.shape);
                return Err("nothing to cut: the part has no body yet".to_owned());
            }
        }
    }
    let mut work = Working::from(ctx.body);
    let outcome = (|| -> Result<(), String> {
        match op {
            ToolOp::NewBody => work.push(
                BodyId::new(id, 0),
                tool.shape,
                tool_names.clone(),
                true,
                kernel,
            ),
            ToolOp::Join => {
                let first = work
                    .find(targets[0])
                    .ok_or_else(|| format!("body {} no longer exists", targets[0]))?;
                combine_into(
                    &mut work,
                    first,
                    BooleanOp::Fuse,
                    tool.shape,
                    &tool_names,
                    kernel,
                )?;
                // Joining several bodies makes them one.
                for other in &targets[1..] {
                    merge_bodies(
                        &mut work,
                        targets[0],
                        *other,
                        BooleanOp::Fuse,
                        false,
                        kernel,
                    )?;
                }
                Ok(())
            }
            ToolOp::Cut | ToolOp::Intersect => {
                let mut untouched = Vec::new();
                for target in &targets {
                    let Some(index) = work.find(*target) else {
                        continue;
                    };
                    let before = kernel.volume(work.pieces[index].shape).ok();
                    combine_into(
                        &mut work,
                        index,
                        op.boolean(),
                        tool.shape,
                        &tool_names,
                        kernel,
                    )?;
                    match work.find(*target) {
                        // Nothing in common: the body would vanish without a word. (A cut
                        // may take a whole body away: that is what it was for.)
                        None if op == ToolOp::Intersect => {
                            return Err(format!(
                                "body {target} has nothing in common with the sketch's \
                                 shape: the intersection is empty"
                            ));
                        }
                        None => {}
                        Some(index) if op == ToolOp::Cut => {
                            let after = kernel.volume(work.pieces[index].shape).ok();
                            if same_volume(before, after) {
                                untouched.push(*target);
                            }
                        }
                        Some(_) => {}
                    }
                }
                // A cut that misses every body does nothing, which is easy to miss: it
                // usually goes the wrong way (away from the body) or lies beside it.
                if !untouched.is_empty() {
                    warnings.push(if untouched.len() == targets.len() {
                        "the cut removed nothing: reverse its direction, or check that \
                         the sketch lies on the body"
                            .to_owned()
                    } else {
                        format!(
                            "the cut removed nothing from {}",
                            untouched
                                .iter()
                                .map(|b| format!("body {b}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    });
                }
                Ok(())
            }
        }
    })();
    let new_body = op == ToolOp::NewBody;
    // The first boss of a part makes its first body, but it is still a boss: pattern and
    // mirror copies of it join that body, as copies of any other boss join theirs.
    let (copies_op, targets) = if requested == ToolOp::Join && new_body {
        (ToolOp::Join, vec![BodyId::new(id, 0)])
    } else {
        (op, targets)
    };
    match outcome {
        Ok(()) => Ok((
            work.finish(id),
            ToolBody {
                shape: tool.shape,
                owned: !new_body,
                names: tool_names,
                op: copies_op,
                targets,
                more: Vec::new(),
            },
        )),
        Err(e) => {
            // A new body's shape is the tool: freed with the bodies.
            work.abort(kernel);
            if !new_body {
                kernel.release(tool.shape);
            }
            Err(e)
        }
    }
}

/// Reads the stored STEP file and joins its solids to the newest body (or makes the first
/// body). With `apply_colors`, the colours the file gives its solids and faces come back
/// as the appearances they should become.
fn build_import<K: Kernel>(
    ctx: &Context<'_>,
    import: &Import,
    kernel: &mut K,
) -> Result<(Body, Option<ImportAppearances>), String> {
    let id = ctx.feature().id;
    let imported = kernel
        .import_step(import.data.bytes())
        .map_err(|e| format!("{}: {e}", import.file_name))?;
    let shape = imported.shape;
    let faces = match kernel.topology(shape) {
        Ok(topology) => topology.faces.len() as u32,
        Err(e) => {
            kernel.release(shape);
            return Err(e.to_string());
        }
    };
    let names: Vec<Option<FaceName>> = (0..faces)
        .map(|index| Some(FaceName::new(FaceKey::Imported { feature: id, index })))
        .collect();
    let mut work = Working::from(ctx.body);
    let target = default_target(ctx, None).and_then(|t| work.find(t));
    let appearances = import.apply_colors.then(|| {
        let mut appearances = import_appearances(id, &imported.colors, target.is_none());
        appearances.material = imported
            .material
            .as_ref()
            .and_then(|m| Material::from_step(&m.name, m.density));
        appearances
    });
    let outcome = match target {
        None => work.push(BodyId::new(id, 0), shape, names, true, kernel),
        Some(index) => {
            let joined = combine_into(&mut work, index, BooleanOp::Fuse, shape, &names, kernel);
            kernel.release(shape);
            joined
        }
    };
    match outcome {
        Ok(()) => Ok((work.finish(id), appearances)),
        Err(e) => {
            work.abort(kernel);
            Err(e)
        }
    }
}

/// The appearances STEP colours become. A single solid that makes a body of its own gets
/// a body appearance, and its faces an appearance only where they differ from it; otherwise
/// (several solids, or solids fused into an existing body) every coloured face gets its
/// own, so nothing else is recoloured.
fn import_appearances(
    feature: FeatureId,
    colors: &StepColors,
    new_body: bool,
) -> ImportAppearances {
    let appearance = |c: &StepColor| Appearance {
        color: c.rgb,
        transparency: c.transparency,
        texture: None,
    };
    let mut out = ImportAppearances::default();
    let whole = match (new_body, colors.solids.as_slice()) {
        (true, [Some(solid)]) => Some(*solid),
        _ => None,
    };
    if let Some(solid) = whole {
        out.body = Some((BodyId::new(feature, 0), appearance(&solid)));
    }
    for (index, color) in colors.faces.iter().enumerate() {
        let Some(color) = color else {
            continue;
        };
        if whole.is_some_and(|w| w == *color) {
            continue;
        }
        let name = FaceName::new(FaceKey::Imported {
            feature,
            index: index as u32,
        });
        out.faces.push((name, appearance(color)));
    }
    out
}

fn build_extrude<K: Kernel>(
    ctx: &Context<'_>,
    extrude: &Extrude,
    kernel: &mut K,
    warnings: &mut Vec<String>,
) -> Result<(Body, ToolBody), String> {
    let (sketch, profile) = ctx.profile_sketch(extrude.sketch)?;
    if extrude
        .extent
        .depth()
        .is_some_and(|depth| depth.is_nan() || depth <= 0.0)
    {
        return Err("depth must be positive".to_owned());
    }

    let mut along = if extrude.op.adds() { 1.0 } else { -1.0 };
    if extrude.reverse {
        along = -along;
    }
    let normal = sketch.plane.normal;
    let (plane, vector) = match &extrude.extent {
        ExtrudeExtent::Blind { depth } => (sketch.plane, normal * (depth * along)),
        ExtrudeExtent::MidPlane { depth } => (sketch.plane.offset(-depth / 2.0), normal * *depth),
        ExtrudeExtent::ThroughAll { both } => {
            let (back, front) = through_all_reach(ctx, &sketch.plane, extrude.op, kernel)?;
            if *both {
                (sketch.plane.offset(-back), normal * (back + front))
            } else {
                let reach = if along > 0.0 { front } else { back };
                if reach.is_nan() || reach <= 0.0 {
                    return Err(
                        "there is nothing to go through on that side of the sketch: reverse the direction"
                            .to_owned(),
                    );
                }
                (sketch.plane, normal * (reach * along))
            }
        }
        ExtrudeExtent::UpToSurface { plane: target } => {
            let target = ctx.plane(target, warnings)?;
            match up_to_surface(&sketch.plane, profile, &target)? {
                UpTo::Parallel(distance) => (sketch.plane, normal * distance),
                UpTo::Slanted {
                    vector,
                    keep,
                    reach,
                } => {
                    let tool = trimmed_prism(&sketch.plane, profile, vector, &keep, reach, kernel)?;
                    return apply_tool(
                        ctx,
                        extrude.op,
                        &extrude.bodies,
                        extrude.sketch,
                        tool,
                        kernel,
                        warnings,
                    );
                }
            }
        }
    };

    let tool = kernel
        .extrude(&plane, profile, vector)
        .map_err(|e| e.to_string())?;
    apply_tool(
        ctx,
        extrude.op,
        &extrude.bodies,
        extrude.sketch,
        tool,
        kernel,
        warnings,
    )
}

/// The included angle of a drill's point, at the bottom of a blind hole.
const DRILL_POINT: f64 = 118.0 * std::f64::consts::PI / 180.0;

/// The lone points of a sketch, where holes go: neither the end nor the centre of any of
/// its curves, nor its origin, nor construction geometry. With their entity ids, which
/// name the holes' faces, so a hole keeps its names while others come and go.
fn hole_points(sketch: &ConstrainedSketch) -> Vec<(u32, oxyde_geom::Point2)> {
    let mut used = HashSet::new();
    for entity in sketch.entities() {
        match entity.geometry {
            Geometry::Line { start, end } => {
                used.insert(start);
                used.insert(end);
            }
            Geometry::Circle { center, .. } => {
                used.insert(center);
            }
            Geometry::Arc { center, start, end } => {
                used.extend([center, start, end]);
            }
            Geometry::Point(_) => {}
        }
    }
    let origin = sketch.origin();
    sketch
        .entities()
        .iter()
        .filter(|e| !e.construction && Some(e.id) != origin && !used.contains(&e.id))
        .filter_map(|e| match e.geometry {
            Geometry::Point(at) => Some((e.id.0, at)),
            _ => None,
        })
        .collect()
}

/// The half section of a hole, to turn about its axis: the radius along x, the depth
/// into the part along y, from a little above the sketch plane (so the cut does not end
/// on the face it is drilled from) down to `depth` and, for a blind hole, its drill
/// point. Segment ids name the faces whatever the kind: 1 the top (above the part), 2 the
/// counterbore's wall or the countersink's cone, 3 the counterbore's floor, 4 the hole's
/// wall, 5 its bottom (or drill point), 6 the axis.
fn hole_profile(hole: &Hole, depth: f64) -> Result<Profile, String> {
    let r = hole.diameter / 2.0;
    let lift = 0.05 * hole.diameter + 0.5;
    let tip = if hole.depth.is_some() {
        r / (DRILL_POINT / 2.0).tan()
    } else {
        0.0
    };
    let p = oxyde_geom::Point2::new;
    let mut corners: Vec<(u32, oxyde_geom::Point2)> = vec![(1, p(0.0, -lift))];
    match hole.kind {
        HoleKind::Simple => corners.push((4, p(r, -lift))),
        HoleKind::Counterbore {
            diameter,
            depth: counter,
        } => {
            if diameter.is_nan() || diameter <= hole.diameter {
                return Err("the counterbore must be wider than the hole".to_owned());
            }
            if counter.is_nan() || counter <= 0.0 {
                return Err("the counterbore's depth must be positive".to_owned());
            }
            if counter >= depth {
                return Err("the counterbore is as deep as the hole, or deeper".to_owned());
            }
            corners.push((2, p(diameter / 2.0, -lift)));
            corners.push((3, p(diameter / 2.0, counter)));
            corners.push((4, p(r, counter)));
        }
        HoleKind::Countersink { diameter, angle } => {
            if diameter.is_nan() || diameter <= hole.diameter {
                return Err("the countersink must be wider than the hole".to_owned());
            }
            if angle.is_nan() || angle <= 0.0 || angle >= std::f64::consts::PI {
                return Err("the countersink's angle must be between 0° and 180°".to_owned());
            }
            let slope = (angle / 2.0).tan();
            let bottom = (diameter / 2.0 - r) / slope;
            if bottom >= depth {
                return Err("the countersink is as deep as the hole, or deeper".to_owned());
            }
            corners.push((2, p(diameter / 2.0 + lift * slope, -lift)));
            corners.push((4, p(r, bottom)));
        }
    }
    corners.push((5, p(r, depth)));
    corners.push((6, p(0.0, depth + tip)));
    // Each corner starts the segment named by its id, to the next corner.
    let segments: Vec<oxyde_geom::ProfileSegment> = corners
        .iter()
        .zip(corners.iter().cycle().skip(1))
        .map(|((id, start), (_, end))| oxyde_geom::ProfileSegment {
            id: *id,
            curve: oxyde_geom::Curve2::Line {
                start: *start,
                end: *end,
            },
        })
        .collect();
    Profile::from_curves(&segments).map_err(|e| format!("the hole's section: {e}"))
}

/// Holes at the lone points of a sketch: each the hole's section turned about an axis
/// through its point along the sketch normal, into the part, all of them cut from the
/// bodies as one tool (a Cut-Extrude's), which patterns and mirrors copy.
fn build_hole<K: Kernel>(
    ctx: &Context<'_>,
    hole: &Hole,
    kernel: &mut K,
    warnings: &mut Vec<String>,
) -> Result<(Body, ToolBody), String> {
    let id = ctx.feature().id;
    let (feature, result) = ctx.earlier(hole.sketch)?;
    if matches!(result.status, FeatureStatus::Error(_)) {
        return Err(format!("{} has errors", feature.name));
    }
    let sketch = result
        .sketch
        .as_ref()
        .ok_or_else(|| format!("{} is suppressed or has errors", feature.name))?;
    if hole.diameter.is_nan() || hole.diameter <= 0.0 {
        return Err("the hole's diameter must be positive".to_owned());
    }
    if hole.depth.is_some_and(|d| d.is_nan() || d <= 0.0) {
        return Err("the hole's depth must be positive".to_owned());
    }
    let points = hole_points(&sketch.solved);
    if points.is_empty() {
        return Err(format!(
            "{} has no lone points: put a point (Sketch › Point) where each hole goes",
            feature.name
        ));
    }
    check_count(points.len() as u64)?;
    let plane = sketch.plane;
    let into = if hole.reverse {
        plane.normal
    } else {
        plane.normal * -1.0
    };
    let depth = match hole.depth {
        Some(depth) => depth,
        None => {
            let (back, front) = through_all_reach(ctx, &plane, ExtrudeOp::Cut, kernel)?;
            let reach = if hole.reverse { front } else { back };
            if reach.is_nan() || reach <= 0.0 {
                return Err(
                    "there is nothing to drill through on that side of the sketch: reverse the direction"
                        .to_owned(),
                );
            }
            reach
        }
    };
    let section = hole_profile(hole, depth)?;
    // One turned section per point, its faces named after the point.
    let mut tools: Vec<NamedTool> = Vec::with_capacity(points.len());
    let mut failure = None;
    for (point, at) in &points {
        let origin = plane.to_world(*at);
        let section_plane = Plane {
            origin,
            x_dir: plane.x_dir,
            normal: plane.x_dir.cross(into),
        };
        let Some(axis) = Axis::new(origin, into) else {
            failure = Some("the sketch plane has no direction".to_owned());
            break;
        };
        match kernel.revolve(&section_plane, &section, &axis, std::f64::consts::TAU) {
            Ok(turned) => {
                let names = sweep_names(id, &turned.history)
                    .into_iter()
                    .map(|name| {
                        name.map(|face| {
                            FaceName::new(FaceKey::Instance {
                                feature: id,
                                instance: [*point, 0],
                                face: Box::new(face),
                            })
                        })
                    })
                    .collect();
                tools.push(NamedTool {
                    shape: turned.shape,
                    names,
                });
            }
            Err(e) => {
                failure = Some(e.to_string());
                break;
            }
        }
    }
    if let Some(e) = failure {
        for tool in tools {
            kernel.release(tool.shape);
        }
        return Err(e);
    }
    // All of them as one tool.
    let mut tools = tools.into_iter();
    let Some(first) = tools.next() else {
        return Err("no hole was made".to_owned());
    };
    let rest: Vec<NamedTool> = tools.collect();
    let tool = if rest.is_empty() {
        first
    } else {
        let shapes: Vec<ShapeHandle> = rest.iter().map(|t| t.shape).collect();
        let fused = kernel.boolean_many(BooleanOp::Fuse, first.shape, &shapes);
        let rest_names: Vec<&[Option<FaceName>]> =
            rest.iter().map(|t| t.names.as_slice()).collect();
        let named = fused.map(|f| NamedTool {
            names: boolean_names(&f.history, &first.names, &rest_names),
            shape: f.shape,
        });
        kernel.release(first.shape);
        for t in &rest {
            kernel.release(t.shape);
        }
        named.map_err(|e| e.to_string())?
    };
    apply_named_tool(
        ctx,
        ExtrudeOp::Cut,
        &hole.bodies,
        hole.sketch,
        tool,
        kernel,
        warnings,
    )
}

/// How far the part's bodies reach from `plane`, behind it and in front of it along its
/// normal: where Through All goes (0 on a side with nothing). A cut or an intersection
/// goes a little further, so that it ends outside every body rather than on a face,
/// which could leave a skin; a boss ends level with the furthest geometry.
fn through_all_reach<K: Kernel>(
    ctx: &Context<'_>,
    plane: &Plane,
    op: ExtrudeOp,
    kernel: &K,
) -> Result<(f64, f64), String> {
    let none = || "Through All needs a body to go through: the part has none yet".to_owned();
    let body = ctx.body.ok_or_else(none)?;
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut size: f64 = 0.0;
    for solid in &body.solids {
        let (a, b) = kernel.bounds(solid.shape).map_err(|e| e.to_string())?;
        size = size.max(a.distance(b));
        for corner in 0..8 {
            let pick = |bit: u32, low: f64, high: f64| if corner & bit == 0 { low } else { high };
            let p =
                oxyde_geom::Point3::new(pick(1, a.x, b.x), pick(2, a.y, b.y), pick(4, a.z, b.z));
            let t = (p - plane.origin).dot(plane.normal);
            lo = lo.min(t);
            hi = hi.max(t);
        }
    }
    if !(lo.is_finite() && hi.is_finite()) {
        return Err(none());
    }
    let tolerance = 1e-6 * size.max(1.0);
    let margin = if op.adds() { 0.0 } else { 0.01 * size + 0.1 };
    let side = |reach: f64| {
        if reach > tolerance {
            reach + margin
        } else {
            0.0
        }
    };
    Ok((side(-lo), side(hi)))
}

/// How an extrusion reaches a plane (Up To Surface).
#[derive(Debug)]
enum UpTo {
    /// The plane is parallel to the sketch: a blind extrusion of this signed distance along
    /// the sketch normal.
    Parallel(f64),
    /// The plane is slanted: a prism along `vector`, long enough to pass the plane all
    /// over the sketch, to be cut back to the side of `keep` its normal points to. `reach`
    /// bounds how far the prism goes from `keep`'s origin.
    Slanted {
        vector: oxyde_geom::Vec3,
        keep: Plane,
        reach: f64,
    },
}

/// How far a profile's points lie from its plane's origin, at most.
fn profile_radius(profile: &Profile) -> f64 {
    let mut radius: f64 = 0.0;
    for region in &profile.regions {
        for segment in &region.outer.segments {
            let (x0, x1) = segment.curve.range_along(1.0, 0.0);
            let (y0, y1) = segment.curve.range_along(0.0, 1.0);
            radius = radius.max(x0.abs().max(x1.abs()).hypot(y0.abs().max(y1.abs())));
        }
    }
    radius
}

/// The extrusion of `profile`, sketched on `sketch`, up to the plane `target`, towards it
/// whichever side of the sketch it lies on. Refused when the plane is parallel to the
/// direction, or passes through the sketch's contours (some of them would go backwards).
fn up_to_surface(sketch: &Plane, profile: &Profile, target: &Plane) -> Result<UpTo, String> {
    let n = target.normal;
    let cos = sketch.normal.dot(n);
    if cos.abs() < 1e-6 {
        return Err(
            "that surface is parallel to the extrusion's direction: it is never reached".to_owned(),
        );
    }
    // The distance along the sketch normal from the sketch point (x, y) to the plane is
    // `t0 + x * tx + y * ty`.
    let t0 = (target.origin - sketch.origin).dot(n) / cos;
    let (tx, ty) = (-sketch.x_dir.dot(n) / cos, -sketch.y_dir().dot(n) / cos);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for region in &profile.regions {
        for segment in &region.outer.segments {
            let (a, b) = segment.curve.range_along(tx, ty);
            lo = lo.min(t0 + a);
            hi = hi.max(t0 + b);
        }
    }
    if !(lo.is_finite() && hi.is_finite()) {
        return Err("the sketch has no closed contour".to_owned());
    }
    let radius = profile_radius(profile);
    let tolerance = 1e-6 * (radius + t0.abs()).max(1.0);
    // Towards the plane: backwards when it lies behind the sketch.
    let (sign, lo, hi) = if hi > tolerance {
        (1.0, lo, hi)
    } else {
        (-1.0, -hi, -lo)
    };
    if lo <= tolerance {
        return Err(if hi <= tolerance {
            "the sketch lies on that surface: there is nothing to extrude up to".to_owned()
        } else {
            "that surface passes through the sketch: pick one beyond it, or a blind depth"
                .to_owned()
        });
    }
    if hi - lo <= tolerance {
        return Ok(UpTo::Parallel(sign * hi));
    }
    // Cut back by the half-space on the sketch's side of the plane.
    let length = hi + 0.1 * (hi - lo) + 1.0;
    let towards_sketch = n * -(sign * cos).signum();
    let keep = Plane::from_point_normal(target.origin, towards_sketch)
        .ok_or_else(|| "that surface has no direction".to_owned())?;
    Ok(UpTo::Slanted {
        vector: sketch.normal * (sign * length),
        keep,
        reach: 2.0 * ((sketch.origin - target.origin).length() + radius + length) + 1.0,
    })
}

/// A prism of `profile` along `vector`, cut back to the half-space on the side `keep`'s
/// normal points to (a box `reach` mm across), with an extrusion's history: the face it
/// was cut along is its end cap, the rest keep their names.
fn trimmed_prism<K: Kernel>(
    sketch: &Plane,
    profile: &Profile,
    vector: oxyde_geom::Vec3,
    keep: &Plane,
    reach: f64,
    kernel: &mut K,
) -> Result<OpResult, String> {
    let error = |e: K::Error| e.to_string();
    let prism = kernel.extrude(sketch, profile, vector).map_err(error)?;
    let l = reach.max(1.0);
    let raw = match kernel.make_box(
        oxyde_geom::Point3::new(-l, -l, 0.0),
        oxyde_geom::Vec3::new(2.0 * l, 2.0 * l, l),
    ) {
        Ok(raw) => raw,
        Err(e) => {
            kernel.release(prism.shape);
            return Err(e.to_string());
        }
    };
    let placed = kernel.transform(raw, &Transform::from_plane(keep));
    kernel.release(raw);
    let half = match placed {
        Ok(placed) => placed.shape,
        Err(e) => {
            kernel.release(prism.shape);
            return Err(e.to_string());
        }
    };
    let common = kernel.boolean(BooleanOp::Common, prism.shape, half);
    kernel.release(half);
    kernel.release(prism.shape);
    let common = common.map_err(error)?;
    let faces = common
        .history
        .faces
        .iter()
        .map(|sources| {
            let mut out = Vec::new();
            for source in sources {
                match *source {
                    FaceSource::InputFace { input: 0, face } => {
                        if let Some(from) = prism.history.faces.get(face as usize) {
                            out.extend(from.iter().copied());
                        }
                    }
                    FaceSource::InputFace { input: 1, .. } => out.push(FaceSource::EndCap),
                    _ => {}
                }
            }
            out
        })
        .collect();
    Ok(OpResult {
        shape: common.shape,
        history: OpHistory { faces },
    })
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
    if crosses_axis(profile, &plane, &axis) {
        return Err(
            "the sketch crosses the axis: keep its contours on one side of it (they may touch it)"
                .to_owned(),
        );
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
    apply_tool(
        ctx,
        revolve.op,
        &revolve.bodies,
        revolve.sketch,
        tool,
        kernel,
        warnings,
    )
}

/// Whether the profile's contours lie on both sides of `axis`, which lies in `plane`: the
/// solid they would turn into passes through itself, which the kernel builds without a
/// word and no later feature can use. Touching the axis is fine (a solid of revolution
/// whose profile has a side on its axis).
fn crosses_axis(profile: &Profile, plane: &Plane, axis: &Axis) -> bool {
    // The signed distance from the axis, in the plane, of the sketch point (x, y) is
    // `s0 + x * gx + y * gy`.
    let across = |v: oxyde_geom::Vec3| axis.direction.cross(v).dot(plane.normal);
    let (gx, gy) = (across(plane.x_dir), across(plane.y_dir()));
    let s0 = across(plane.origin - axis.origin);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for region in &profile.regions {
        for contour in std::iter::once(&region.outer).chain(&region.holes) {
            for segment in &contour.segments {
                let (a, b) = segment.curve.range_along(gx, gy);
                lo = lo.min(s0 + a);
                hi = hi.max(s0 + b);
            }
        }
    }
    let tolerance = 1e-6 * (hi - lo).abs().max(1.0);
    lo < -tolerance && hi > tolerance
}

fn build_sweep<K: Kernel>(
    ctx: &Context<'_>,
    sweep: &Sweep,
    kernel: &mut K,
    warnings: &mut Vec<String>,
) -> Result<(Body, ToolBody), String> {
    if sweep.profile == sweep.path {
        return Err("the profile and the path must be two different sketches".to_owned());
    }
    let (profile_sketch, profile) = ctx.profile_sketch(sweep.profile)?;
    let (path_sketch, path) = ctx.path_sketch(sweep.path)?;
    // SolidWorks carries the profile from the path's start: a path that does not start on
    // the profile's plane still sweeps (the profile is moved along with it), but is
    // usually a mistake, so it is said.
    if let Some(start) = path.start() {
        let start = path_sketch.plane.to_world(start);
        let plane = &profile_sketch.plane;
        let off_plane = (start - plane.origin).dot(plane.normal).abs();
        if off_plane > 1e-6 {
            warnings.push(format!(
                "the path starts {off_plane:.3} mm off the profile's plane; the profile is swept from where it is"
            ));
        }
    }
    let tool = kernel
        .sweep(&profile_sketch.plane, profile, &path_sketch.plane, path)
        .map_err(|e| e.to_string())?;
    apply_tool(
        ctx,
        sweep.op,
        &sweep.bodies,
        sweep.profile,
        tool,
        kernel,
        warnings,
    )
}

/// A loft through the closed contours of its sketches, in order, each turned to run the
/// same way round along the loft (a contour drawn clockwise and the next counter-clockwise
/// would twist it inside out).
fn build_loft<K: Kernel>(
    ctx: &Context<'_>,
    loft: &Loft,
    kernel: &mut K,
    warnings: &mut Vec<String>,
) -> Result<(Body, ToolBody), String> {
    if loft.profiles.len() < 2 {
        return Err("a loft goes through two sketches at least".to_owned());
    }
    let mut seen = HashSet::new();
    let mut sections: Vec<(Plane, oxyde_geom::Loop)> = Vec::with_capacity(loft.profiles.len());
    for id in &loft.profiles {
        if !seen.insert(*id) {
            return Err("a sketch is in the loft twice".to_owned());
        }
        let (feature, _) = ctx.earlier(*id)?;
        let (sketch, profile) = ctx.profile_sketch(*id)?;
        let contour = match profile.regions.as_slice() {
            [region] if region.holes.is_empty() => region.outer.clone(),
            _ => {
                return Err(format!(
                    "{}: a loft's sketch must be one closed contour, without holes",
                    feature.name
                ));
            }
        };
        sections.push((sketch.plane, contour));
    }
    // Along the loft: from the first contour's middle to the last one's.
    let middle = |(plane, contour): &(Plane, oxyde_geom::Loop)| {
        let points = contour.polygon();
        let n = points.len().max(1) as f64;
        let (x, y) = points
            .iter()
            .fold((0.0, 0.0), |(x, y), p| (x + p.x / n, y + p.y / n));
        plane.to_world(oxyde_geom::Point2::new(x, y))
    };
    let along = match (sections.first(), sections.last()) {
        (Some(first), Some(last)) => {
            let d = middle(last) - middle(first);
            if d.length() > 1e-9 { d } else { first.0.normal }
        }
        _ => return Err("a loft goes through two sketches at least".to_owned()),
    };
    for (plane, contour) in &mut sections {
        let turn = contour.signed_area() * plane.normal.dot(along);
        if turn < 0.0 {
            contour.segments.reverse();
            for segment in &mut contour.segments {
                segment.curve = segment.curve.reversed();
            }
        }
    }
    let tool = kernel
        .loft(&sections, loft.ruled)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "this kernel cannot loft".to_owned())?;
    let first = loft
        .profiles
        .first()
        .copied()
        .ok_or_else(|| "a loft goes through two sketches at least".to_owned())?;
    apply_tool(ctx, loft.op, &loft.bodies, first, tool, kernel, warnings)
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
    let second = match (kind, blend.chamfer) {
        (Blend::Fillet, _) | (Blend::Chamfer, ChamferShape::Equal) => None,
        (Blend::Chamfer, ChamferShape::TwoDistances { distance, .. }) => {
            if distance.is_nan() || distance <= 0.0 {
                return Err("the second distance must be positive".to_owned());
            }
            Some(oxyde_kernel::ChamferSecond::Distance(distance))
        }
        (Blend::Chamfer, ChamferShape::AngleDistance { angle, .. }) => {
            if angle.is_nan() || angle <= 0.0 || angle >= std::f64::consts::FRAC_PI_2 {
                return Err("the angle must be between 0° and 90°".to_owned());
            }
            Some(oxyde_kernel::ChamferSecond::Angle(angle))
        }
    };
    let mut edges = Vec::with_capacity(blend.edges.len());
    for name in &blend.edges {
        edges.push(resolve(
            body.naming.resolve_edge(name),
            "edge",
            name,
            warnings,
        )?);
    }
    // Each body is filleted on its own, with its own edges.
    let id = ctx.feature().id;
    let mut work = Working::from(ctx.body);
    let outcome = (|| -> Result<(), String> {
        for solid in &body.solids {
            let mut local: Vec<u32> = edges.iter().filter_map(|e| solid.local_edge(*e)).collect();
            if local.is_empty() {
                continue;
            }
            local.sort_unstable();
            local.dedup();
            let result = match (kind, second) {
                (Blend::Fillet, _) => kernel.fillet(solid.shape, &local, blend.size),
                (Blend::Chamfer, None) => kernel.chamfer(solid.shape, &local, blend.size),
                (Blend::Chamfer, Some(second)) => {
                    let pairs = chamfer_faces(body, solid, &blend.edges, &edges, blend.chamfer)?;
                    match kernel.chamfer_with(solid.shape, &pairs, blend.size, second) {
                        Ok(Some(result)) => Ok(result),
                        Ok(None) => {
                            return Err("this kernel makes equal chamfers only".to_owned());
                        }
                        Err(e) => Err(e),
                    }
                }
            }
            .map_err(|e| e.to_string())?;
            let names = derived_names(id, &result.history, body, solid);
            let Some(index) = work.find(solid.id) else {
                // The pieces come from the same body as `solid`: cannot happen.
                kernel.release(result.shape);
                return Err("internal error: the blended body is missing".to_owned());
            };
            work.replace(index, result.shape, names, kernel)?;
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => Ok(work.finish(id)),
        Err(e) => {
            work.abort(kernel);
            Err(e)
        }
    }
}

/// For an unequal chamfer, the edges of `solid` (by their index in it) with the face the
/// chamfer's first size runs along: the edge's face named first in its persistent name
/// (names are sorted, so the choice is stable), or the other one when `shape` flips.
fn chamfer_faces(
    body: &Body,
    solid: &Solid,
    names: &[crate::naming::EdgeName],
    edges: &[u32],
    shape: ChamferShape,
) -> Result<Vec<(u32, u32)>, String> {
    let side = usize::from(shape.flipped());
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (name, edge) in names.iter().zip(edges) {
        let Some(local) = solid.local_edge(*edge) else {
            continue;
        };
        if out.iter().any(|(e, _)| *e == local) {
            continue;
        }
        let adjacent = body
            .topology
            .edges
            .get(*edge as usize)
            .map(|e| e.faces.clone())
            .unwrap_or_default();
        let wanted = name.faces.get(side);
        // The adjacent face of that name, else the edge's faces in the kernel's order.
        let face = adjacent
            .iter()
            .copied()
            .find(|f| body.naming.face(*f) == wanted)
            .or_else(|| adjacent.get(side).copied())
            .or_else(|| adjacent.first().copied())
            .ok_or_else(|| format!("edge {name} has no face to chamfer along"))?;
        let face = solid
            .local_face(face)
            .ok_or_else(|| format!("edge {name} is not on one body"))?;
        out.push((local, face));
    }
    Ok(out)
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
    // One body is shelled: the one the faces are on, or the only one.
    let solid = match faces.first() {
        Some(f) => body.solid_of_face(*f).ok_or("face not found")?,
        None if body.solids.len() == 1 => &body.solids[0],
        None => {
            return Err(
                "the part has several bodies: select a face to remove on the one to shell"
                    .to_owned(),
            );
        }
    };
    let mut local = Vec::with_capacity(faces.len());
    for f in &faces {
        local.push(
            solid
                .local_face(*f)
                .ok_or("the faces are on several bodies: shell one body at a time")?,
        );
    }
    local.sort_unstable();
    local.dedup();
    let offset = if shell.outward {
        shell.thickness
    } else {
        -shell.thickness
    };
    let result = kernel
        .shell(solid.shape, &local, offset)
        .map_err(|e| e.to_string())?;
    let id = ctx.feature().id;
    let names = derived_names(id, &result.history, body, solid);
    let mut work = Working::from(ctx.body);
    let Some(index) = work.find(solid.id) else {
        // The pieces come from the same body as `solid`: cannot happen.
        kernel.release(result.shape);
        work.abort(kernel);
        return Err("internal error: the shelled body is missing".to_owned());
    };
    match work.replace(index, result.shape, names, kernel) {
        Ok(()) => Ok(work.finish(id)),
        Err(e) => {
            work.abort(kernel);
            Err(e)
        }
    }
}

/// Combine: the target body joined with, cut by or intersected with the tool bodies.
fn build_combine<K: Kernel>(
    ctx: &Context<'_>,
    combine: &Combine,
    kernel: &mut K,
    warnings: &mut Vec<String>,
) -> Result<Body, String> {
    let body = ctx.body()?;
    if body.solid(combine.target).is_none() {
        return Err(format!(
            "the target body {} no longer exists: choose it again",
            combine.target
        ));
    }
    if combine.tools.is_empty() {
        return Err("choose the tool bodies".to_owned());
    }
    if combine.tools.contains(&combine.target) {
        return Err("the target body cannot also be a tool".to_owned());
    }
    let op = match combine.op {
        CombineOp::Join => BooleanOp::Fuse,
        CombineOp::Cut => BooleanOp::Cut,
        CombineOp::Intersect => BooleanOp::Common,
    };
    let id = ctx.feature().id;
    let mut work = Working::from(ctx.body);
    let outcome = (|| -> Result<(), String> {
        let volume = |work: &Working, kernel: &mut K| {
            work.find(combine.target)
                .and_then(|i| kernel.volume(work.pieces[i].shape).ok())
        };
        let before = volume(&work, kernel);
        for tool in &combine.tools {
            merge_bodies(
                &mut work,
                combine.target,
                *tool,
                op,
                combine.keep_tools,
                kernel,
            )?;
            if work.find(combine.target).is_none() {
                return Err(match combine.op {
                    CombineOp::Intersect => {
                        "the bodies have nothing in common: the intersection is empty"
                    }
                    _ => "nothing is left of the target body",
                }
                .to_owned());
            }
        }
        if combine.op == CombineOp::Cut && same_volume(before, volume(&work, kernel)) {
            warnings.push(
                "the cut removed nothing: the tool bodies do not reach the target".to_owned(),
            );
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => Ok(work.finish(id)),
        Err(e) => {
            work.abort(kernel);
            Err(e)
        }
    }
}

fn build_ref_plane<K: Kernel>(
    ctx: &Context<'_>,
    plane: &RefPlane,
    kernel: &K,
    warnings: &mut Vec<String>,
) -> Result<ReferenceGeometry, String> {
    let built = match plane {
        RefPlane::Offset { base, offset, flip } => {
            let base = ctx.plane(base, warnings)?;
            base.offset(if *flip { -offset } else { *offset })
        }
        RefPlane::Angle {
            base,
            line,
            angle,
            flip,
        } => {
            let base = ctx.plane(base, warnings)?;
            let line = ctx.axis(line, warnings)?;
            if line.direction.dot(base.normal).abs() > 1e-6
                || (line.origin - base.origin).dot(base.normal).abs() > 1e-6
            {
                return Err("the line does not lie in the plane".to_owned());
            }
            turned_about(&base, &line, if *flip { -angle } else { *angle })
        }
        RefPlane::ThroughLine {
            line,
            plane,
            perpendicular,
            flip,
        } => {
            let line = ctx.axis(line, warnings)?;
            let plane = ctx.plane(plane, warnings)?;
            let across = line.direction.dot(plane.normal);
            let built = if *perpendicular {
                // A quarter turn about the line's shadow on the plane: the plane through
                // the line, square to the other one.
                let height = (line.origin - plane.origin).dot(plane.normal);
                let shadow = Axis::new(
                    line.origin - plane.normal * height,
                    line.direction - plane.normal * across,
                )
                .filter(|_| across.abs() < 1.0 - 1e-9)
                .ok_or_else(|| {
                    "the line is normal to the plane: every plane through it is perpendicular to it"
                        .to_owned()
                })?;
                turned_about(&plane, &shadow, std::f64::consts::FRAC_PI_2)
            } else {
                if across.abs() > 1e-6 {
                    return Err("the line is not parallel to the plane".to_owned());
                }
                plane.offset((line.origin - plane.origin).dot(plane.normal))
            };
            reversed_if(built, *flip)
        }
        RefPlane::ThreePoints { points, flip } => {
            let [a, b, c] = [
                ctx.point(&points[0], warnings)?,
                ctx.point(&points[1], warnings)?,
                ctx.point(&points[2], warnings)?,
            ];
            let (ab, ac) = (b - a, c - a);
            let size = ab.length().max(ac.length()).max(1.0);
            if [ab, ac, c - b].iter().any(|v| v.length() <= 1e-6 * size) {
                return Err("two of the points are the same point".to_owned());
            }
            let normal = ab.cross(ac);
            let normal = (normal.length() > 1e-9 * ab.length() * ac.length())
                .then(|| normal.normalized())
                .flatten();
            let (Some(normal), Some(x_dir)) = (normal, ab.normalized()) else {
                return Err("the three points are in line".to_owned());
            };
            reversed_if(
                Plane {
                    origin: a,
                    x_dir,
                    normal,
                },
                *flip,
            )
        }
        RefPlane::NormalToCurve { curve, point, flip } => {
            let at = ctx.point(point, warnings)?;
            let (_, tangent) = ctx.curve_tangent(curve, at, kernel, warnings)?;
            let frame = Plane::from_point_normal(at, tangent)
                .ok_or_else(|| "the curve has no direction at that point".to_owned())?;
            reversed_if(
                Plane {
                    origin: at,
                    ..frame
                },
                *flip,
            )
        }
    };
    Ok(ReferenceGeometry::Plane(built))
}

/// `plane` turned by `angle` (radians, right-hand rule) about `line`, its frame with it.
fn turned_about(plane: &Plane, line: &Axis, angle: f64) -> Plane {
    plane.transformed(&Transform::rotation(line, angle))
}

/// The plane with its normal reversed when `flip` is set (its x axis kept, so that its
/// frame stays right-handed).
fn reversed_if(plane: Plane, flip: bool) -> Plane {
    if flip {
        Plane {
            normal: -plane.normal,
            ..plane
        }
    } else {
        plane
    }
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
    // Parallel directions would stack the second row on the first.
    if count1 > 1
        && count2 > 1
        && first.cross(second).length() <= 1e-9 * first.length() * second.length()
    {
        return Err(
            "the two directions are parallel: pick a second direction across the first".to_owned(),
        );
    }
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

/// The instances of a pattern to leave out (SolidWorks' Instances to Skip), of those it
/// has. A key that is not one of them, the seed's or one gone with a smaller count, is
/// ignored with a warning: a change of count must not make the pattern fail.
fn skipped_instances(
    skip: &[[u32; 2]],
    instances: &[Instance],
    label: fn([u32; 2]) -> String,
    warnings: &mut Vec<String>,
) -> BTreeSet<[u32; 2]> {
    if skip.is_empty() {
        return BTreeSet::new();
    }
    let existing: BTreeSet<[u32; 2]> = instances.iter().map(|(key, _)| *key).collect();
    let (kept, ignored): (BTreeSet<[u32; 2]>, BTreeSet<[u32; 2]>) =
        skip.iter().partition(|key| existing.contains(*key));
    if ignored.contains(&[0, 0]) {
        warnings.push(format!(
            "the seed {} cannot be skipped: it is kept",
            label([0, 0])
        ));
    }
    let gone: Vec<String> = ignored
        .iter()
        .filter(|key| **key != [0, 0])
        .map(|key| label(*key))
        .collect();
    match gone.as_slice() {
        [] => {}
        [one] => warnings.push(format!(
            "the skipped instance {one} is no longer in the pattern"
        )),
        many => warnings.push(format!(
            "the skipped instances {} are no longer in the pattern",
            many.join(", ")
        )),
    }
    kept
}

fn mirror_instances(
    ctx: &Context<'_>,
    mirror: &Mirror,
    warnings: &mut Vec<String>,
) -> Result<Vec<Instance>, String> {
    let plane = ctx.plane(&mirror.plane, warnings)?;
    Ok(vec![([1, 0], Transform::mirror(&plane))])
}

/// Mirrors whole bodies: each joined to its copy through the plane (the one instance),
/// whose faces are named after the body's own and the instance, so that features after
/// the mirror find the faces of either half.
fn mirror_bodies<K: Kernel>(
    ctx: &Context<'_>,
    bodies: &[BodyId],
    instances: &[Instance],
    kernel: &mut K,
) -> Result<Body, String> {
    let id = ctx.feature().id;
    let body = ctx.body()?;
    let Some((key, transform)) = instances.first() else {
        return Err("no mirror plane".to_owned());
    };
    let mut work = Working::from(ctx.body);
    let outcome = (|| -> Result<(), String> {
        for target in bodies {
            let solid = body.solid(*target).ok_or_else(|| {
                format!("body {target} no longer exists: choose the bodies again")
            })?;
            let copy = kernel
                .transform(solid.shape, transform)
                .map_err(|e| e.to_string())?;
            let names: Vec<Option<FaceName>> = copy
                .history
                .faces
                .iter()
                .map(|sources| {
                    sources.iter().find_map(|source| match *source {
                        FaceSource::InputFace { input: 0, face } => {
                            body.face_name(solid, face).map(|original| {
                                FaceName::new(FaceKey::Instance {
                                    feature: id,
                                    instance: *key,
                                    face: Box::new(original),
                                })
                            })
                        }
                        _ => None,
                    })
                })
                .collect();
            let joined = match work.find(*target) {
                Some(index) => combine_into(
                    &mut work,
                    index,
                    BooleanOp::Fuse,
                    copy.shape,
                    &names,
                    kernel,
                ),
                None => Err(format!("body {target} no longer exists")),
            };
            kernel.release(copy.shape);
            joined?;
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => Ok(work.finish(id)),
        Err(e) => {
            work.abort(kernel);
            Err(e)
        }
    }
}

/// Copies the tools of `seeds` to every instance but those in `skip` and does with each
/// copy what the seed did: joined to or cut from the same bodies, or a new body. Faces of a
/// copy are named after the seed's face and the instance. A seed may be a pattern or a
/// mirror itself, whose copies are then copied (a pattern of a pattern, a mirror of a
/// pattern).
///
/// Returns the bodies, and the copies as the feature's own tool when they all do the same
/// to the same bodies, so that the feature can be copied in turn; `None` when they differ
/// or make new bodies.
fn build_instances<K: Kernel>(
    ctx: &Context<'_>,
    seeds: &[FeatureId],
    instances: &[Instance],
    skip: &BTreeSet<[u32; 2]>,
    kernel: &mut K,
) -> Result<(Body, Option<ToolBody>), String> {
    let id = ctx.feature().id;
    if seeds.is_empty() {
        return Err("no features to copy".to_owned());
    }
    if instances.is_empty() {
        return Err("the pattern has a single instance".to_owned());
    }
    if instances.iter().all(|(key, _)| skip.contains(key)) {
        return Err("every instance but the seed is skipped".to_owned());
    }
    let mut tools = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let (feature, result) = ctx.earlier(*seed)?;
        if !feature.kind.is_patternable() {
            return Err(format!(
                "{} cannot be copied: only extrusions, revolves, sweeps, lofts, holes, patterns \
                 and mirrors of features can",
                feature.name
            ));
        }
        let copies_features = matches!(
            feature.kind,
            FeatureKind::LinearPattern(_)
                | FeatureKind::CircularPattern(_)
                | FeatureKind::Mirror(_)
        );
        let tool = match &result.tool {
            Some(tool) => tool,
            None if copies_features && !matches!(result.status, FeatureStatus::Error(_)) => {
                return Err(format!(
                    "{} cannot be copied: the features it copies do not all add, or all remove, \
                     material on the same bodies",
                    feature.name
                ));
            }
            None => return Err(format!("{} is suppressed or has errors", feature.name)),
        };
        tools.push(tool);
    }
    // The copies stay as this feature's tool when they all do one thing to the same bodies.
    let keep = tools.first().is_some_and(|first| {
        tools
            .iter()
            .all(|t| t.op != ToolOp::NewBody && t.op == first.op && t.targets == first.targets)
    });

    let mut work = Working::from(ctx.body);
    let mut new_bodies = 0;
    let mut copies: Vec<(ShapeHandle, Vec<Option<FaceName>>)> = Vec::new();
    let mut kept: Vec<(ShapeHandle, Vec<Option<FaceName>>)> = Vec::new();
    let outcome = (|| -> Result<(), String> {
        for tool in &tools {
            for (piece, piece_names) in tool.pieces() {
                for (key, transform) in instances {
                    if skip.contains(key) {
                        // A skipped copy keeps its body number, so that the bodies of the
                        // copies after it keep theirs, and what refers to them stays on them.
                        if tool.op == ToolOp::NewBody {
                            new_bodies += 1;
                        }
                        continue;
                    }
                    let copy = kernel
                        .transform(piece, transform)
                        .map_err(|e| e.to_string())?;
                    let names = copy
                        .history
                        .faces
                        .iter()
                        .map(|sources| {
                            sources.iter().find_map(|source| match *source {
                                FaceSource::InputFace { input: 0, face } => {
                                    piece_names.get(face as usize).cloned().flatten().map(
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
                    if tool.op == ToolOp::NewBody {
                        let body = BodyId::new(id, new_bodies);
                        new_bodies += 1;
                        // The copy is the new body's shape.
                        work.push(body, copy.shape, names, true, kernel)?;
                    } else {
                        copies.push((copy.shape, names));
                    }
                }
            }
            // Every copy of a seed at once: one boolean per copy would redo the work on a
            // body that grows with each.
            let all: Vec<(ShapeHandle, &[Option<FaceName>])> = copies
                .iter()
                .map(|(shape, names)| (*shape, names.as_slice()))
                .collect();
            match tool.op {
                ToolOp::NewBody => {}
                ToolOp::Join => {
                    let index = tool
                        .targets
                        .first()
                        .and_then(|t| work.find(*t))
                        .ok_or("the body the seed joined no longer exists")?;
                    combine_all_into(&mut work, index, BooleanOp::Fuse, &all, kernel)?;
                }
                ToolOp::Cut => {
                    for target in &tool.targets {
                        if let Some(index) = work.find(*target) {
                            combine_all_into(&mut work, index, BooleanOp::Cut, &all, kernel)?;
                        }
                    }
                }
                // What a body shares with every copy, as each copy of the seed would keep;
                // one at a time (a common with several tools keeps what is in any of them).
                ToolOp::Intersect => {
                    for target in &tool.targets {
                        for copy in &all {
                            if let Some(index) = work.find(*target) {
                                combine_all_into(
                                    &mut work,
                                    index,
                                    BooleanOp::Common,
                                    std::slice::from_ref(copy),
                                    kernel,
                                )?;
                            }
                        }
                    }
                }
            }
            if keep {
                kept.append(&mut copies);
            } else {
                for (shape, _) in copies.drain(..) {
                    kernel.release(shape);
                }
            }
        }
        Ok(())
    })();
    for (shape, _) in copies {
        kernel.release(shape);
    }
    match outcome {
        Ok(()) => {
            let tool = match (keep, tools.first()) {
                (true, Some(first)) if !kept.is_empty() => {
                    let (shape, names) = kept.remove(0);
                    Some(ToolBody {
                        shape,
                        owned: true,
                        names,
                        op: first.op,
                        targets: first.targets.clone(),
                        more: kept,
                    })
                }
                _ => {
                    for (shape, _) in kept {
                        kernel.release(shape);
                    }
                    None
                }
            };
            Ok((work.finish(id), tool))
        }
        Err(e) => {
            for (shape, _) in kept {
                kernel.release(shape);
            }
            work.abort(kernel);
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
            chamfer: Default::default(),
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
    fn a_replaced_definition_may_only_refer_to_earlier_features() {
        let mut part = Part::new();
        let s1 = part.add_feature(rectangle_sketch());
        let boss = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            5.0,
        )));
        let edge = EdgeName::between(FaceName::profile(boss, 1), FaceName::end_cap(boss));
        let fillet = part.add_feature(FeatureKind::Fillet(EdgeBlend {
            edges: vec![edge.clone()],
            size: 1.0,
            chamfer: Default::default(),
        }));
        let s2 = part.add_feature(rectangle_sketch());
        let cut = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s2,
            ExtrudeOp::Cut,
            1.0,
        )));

        // A sketch after the extrusion, an edge made by a later cut, the fillet's own face.
        let later_sketch = FeatureKind::Extrude(Extrude::blind(s2, ExtrudeOp::Boss, 5.0));
        let later_edge = FeatureKind::Fillet(EdgeBlend {
            edges: vec![EdgeName::between(
                FaceName::profile(cut, 1),
                FaceName::end_cap(boss),
            )],
            size: 1.0,
            chamfer: Default::default(),
        });
        let own_face = FeatureKind::Fillet(EdgeBlend {
            edges: vec![EdgeName::between(
                FaceName::from_edge(fillet, edge.clone()),
                FaceName::end_cap(boss),
            )],
            size: 1.0,
            chamfer: Default::default(),
        });
        for (feature, kind, reference) in [
            (boss, later_sketch, s2),
            (fillet, later_edge, cut),
            (fillet, own_face, fillet),
        ] {
            assert_eq!(
                part.replace_feature(feature, kind),
                Err(ModelError::LaterReference { feature, reference })
            );
        }
        // Nothing changed, and what is saved can be opened again.
        assert!(Part::from_definition(part.definition()).is_ok());

        part.replace_feature(
            fillet,
            FeatureKind::Fillet(EdgeBlend {
                edges: vec![edge],
                size: 2.0,
                chamfer: Default::default(),
            }),
        )
        .unwrap();
        assert!(matches!(
            part.feature(fillet).unwrap().kind(),
            FeatureKind::Fillet(b) if b.size == 2.0
        ));
    }

    #[test]
    fn dimension_names_survive_a_change_of_definition() {
        use crate::dimensions::{DimensionKey, DimensionRef};
        use crate::feature::{
            AxisRef, FeatureParameter, LinearPattern, PatternDirection, StandardAxis,
        };
        let mut part = Part::new();
        let s1 = part.add_feature(rectangle_sketch());
        let boss = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            5.0,
        )));
        let direction = |axis| PatternDirection {
            axis: AxisRef::Standard(axis),
            spacing: 20.0,
            count: 2,
            reverse: false,
        };
        let one_way = FeatureKind::LinearPattern(LinearPattern {
            seeds: vec![boss],
            first: direction(StandardAxis::X),
            second: None,
            skip: Vec::new(),
        });
        let two_ways = FeatureKind::LinearPattern(LinearPattern {
            seeds: vec![boss],
            first: direction(StandardAxis::X),
            second: Some(direction(StandardAxis::Y)),
            skip: Vec::new(),
        });
        let pattern = part.add_feature(one_way.clone());
        let dimension = |parameter| DimensionRef {
            feature: pattern,
            key: DimensionKey::Feature(parameter),
        };
        // D3 and D4 belong to the second direction, even before it exists.
        assert_eq!(
            part.rename_dimension(dimension(FeatureParameter::Spacing), "D3"),
            Err(ModelError::DuplicateName("D3".to_owned()))
        );
        part.replace_feature(pattern, two_ways).unwrap();
        part.rename_dimension(dimension(FeatureParameter::Spacing2), "Pitch")
            .unwrap();
        // Without the second direction, its name goes too: the part still opens.
        part.replace_feature(pattern, one_way).unwrap();
        assert!(part.feature(pattern).unwrap().parameter_names().is_empty());
        assert!(Part::from_definition(part.definition()).is_ok());
    }

    #[test]
    fn imports_settle_only_from_the_same_timeline() {
        let mut part = Part::new();
        let id = part.add_feature(FeatureKind::Import(Import::new(
            "bar.step",
            crate::feature::FileData::new(b"ISO-10303-21;".to_vec()),
        )));
        let unbuilt = part.definition();
        // What a rebuild leaves: the colour and the material of the file, applied once.
        let mut built = unbuilt.clone();
        if let FeatureKind::Import(import) = &mut built.features[0].kind {
            import.apply_colors = false;
        }
        let red = Appearance::colored([200, 40, 40]);
        built.appearances.set_body(BodyId::new(id, 0), Some(red));
        built.material = Material::from_step("Acier", 7850.0);

        let mut settled = unbuilt.clone();
        assert!(settled.settle_imports(&built));
        assert_eq!(settled, built);
        // Nothing left to apply: nothing to take.
        let mut again = settled.clone();
        again.appearances = Appearances::default();
        assert!(!again.settle_imports(&built));
        assert!(again.appearances.is_empty());

        // Another timeline (here another file) is not taken from.
        let mut other = built.clone();
        if let FeatureKind::Import(import) = &mut other.features[0].kind {
            import.file_name = "other.step".to_owned();
        }
        let mut kept = unbuilt.clone();
        assert!(!kept.settle_imports(&other));
        assert_eq!(kept, unbuilt);
        // Nor is a copy whose import was not rebuilt (rolled back, suppressed).
        assert!(!kept.settle_imports(&unbuilt));
        assert_eq!(kept, unbuilt);
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
