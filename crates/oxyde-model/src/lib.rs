//! The parametric model: feature timeline, rebuild and persistent topological naming.
//!
//! - [`Part`] holds the timeline (sketches, extrusions, fillets, …), the rollback bar and
//!   suppression, and rebuilds it through any [`oxyde_kernel::Kernel`].
//! - [`FaceName`], [`EdgeName`] and [`VertexName`] identify geometry in a way that survives
//!   upstream edits, so features keep pointing at the right faces and edges.
//!
//! Depends on the `Kernel` trait only, never on a concrete backend.
//! See `docs/ARCHITECTURE.md` §6.

mod error;
mod feature;
mod naming;
mod part;

pub use error::ModelError;
pub use feature::{
    EdgeBlend, Extrude, ExtrudeExtent, ExtrudeOp, Feature, FeatureId, FeatureKind, Sketch,
    SketchEntity, SketchPlane, StandardPlane,
};
pub use naming::{BodyNaming, EdgeName, FaceKey, FaceName, Resolution, VertexName};
pub use part::{Body, FeatureStatus, Part, PartDefinition, SketchResult, TimelineEntry};
