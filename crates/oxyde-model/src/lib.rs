//! The parametric model: feature timeline, rebuild and persistent topological naming.
//!
//! - [`Part`] holds the timeline (sketches, extrusions, fillets, …), the rollback bar and
//!   suppression, and rebuilds it through any [`oxyde_kernel::Kernel`].
//! - [`FaceName`], [`EdgeName`] and [`VertexName`] identify geometry in a way that survives
//!   upstream edits, so features keep pointing at the right faces and edges.
//! - [`Part::dimensions`] lists every dimension with its SolidWorks-style full name
//!   (`D1@Sketch1`); [`DimensionRef`] refers to one by stable ids, for equations.
//!
//! Depends on the `Kernel` trait only, never on a concrete backend.
//! See `docs/ARCHITECTURE.md` §6.

mod dimensions;
mod error;
mod feature;
mod naming;
mod part;

pub use dimensions::{
    Dimension, DimensionKey, DimensionRef, DimensionUnit, full_name, split_full_name,
};
pub use error::ModelError;
pub use feature::{
    EdgeBlend, Extrude, ExtrudeExtent, ExtrudeOp, Feature, FeatureId, FeatureKind,
    FeatureParameter, Sketch, SketchPlane, StandardPlane,
};
pub use naming::{BodyNaming, EdgeName, FaceKey, FaceName, Resolution, VertexName};
pub use part::{Body, FeatureStatus, Part, PartDefinition, SketchResult, TimelineEntry};
