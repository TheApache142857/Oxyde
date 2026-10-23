//! The parametric model: feature timeline, rebuild and persistent topological naming.
//!
//! - [`Part`] holds the timeline (sketches, extrusions, revolves, fillets, shells, reference
//!   planes and axes, patterns, mirrors…), the rollback bar and suppression, and rebuilds it
//!   through any [`oxyde_kernel::Kernel`].
//! - [`FaceName`], [`EdgeName`] and [`VertexName`] identify geometry in a way that survives
//!   upstream edits, so features keep pointing at the right faces and edges.
//! - [`Part::dimensions`] lists every dimension with its SolidWorks-style full name
//!   (`D1@Sketch1`); [`DimensionRef`] refers to one by stable ids.
//! - [`Equations`]: global variables and equations driving dimensions
//!   (`"D1@Sketch1" = "Width" * 2`), evaluated before every rebuild.
//! - [`Appearances`]: colours, transparency and colour textures of the part, its bodies and
//!   single faces (display only; not part of the rebuild).
//!
//! Depends on the `Kernel` trait only, never on a concrete backend.
//! See `docs/ARCHITECTURE.md` §6.

// Never crash: a panic in a modelling or UI path would take the unsaved document with it.
// Return an error or fall back to a safe default instead (docs/ARCHITECTURE.md, "Never crash").
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]
#![warn(clippy::indexing_slicing)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing
    )
)]

mod appearance;
mod bodies;
mod configurations;
mod dimensions;
mod equations;
mod error;
mod expression;
mod feature;
mod finish;
mod material;
mod naming;
mod part;

pub use appearance::{Appearance, Appearances, Texture};
pub use bodies::{Body, Solid};
pub use configurations::{
    Configuration, ConfigurationText, Configurations, ConfigurationsText, DEFAULT_CONFIGURATION,
};
pub use dimensions::{
    Dimension, DimensionKey, DimensionRef, DimensionUnit, full_name, split_full_name,
};
pub use equations::{
    Dimensions, Equation, EquationResults, EquationText, Equations, EquationsText, GlobalText,
    GlobalVariable, LinkedValue,
};
pub use error::ModelError;
pub use expression::{BinaryOp, Expr, Function, GlobalId, Operand, ValueOf};
pub use feature::{
    AxisRef, BodyId, ChamferShape, CircularPattern, Combine, CombineOp, CurveRef, EdgeBlend,
    Extrude, ExtrudeExtent, ExtrudeOp, Feature, FeatureId, FeatureKind, FeatureParameter, FileData,
    Hole, HoleKind, Import, LinearPattern, Loft, Mirror, PatternDirection, PlaneRef, PointRef,
    RefAxis, RefPlane, Revolve, RevolveAxis, Shell, Sketch, SketchPlane, StandardAxis,
    StandardPlane, Sweep,
};
pub use finish::{Finish, FinishParams};
pub use material::{Material, MaterialLibrary, MaterialSource, Mechanical, Thermal};
pub use naming::{BodyNaming, EdgeName, FaceKey, FaceName, Resolution, VertexName};
pub use part::{
    FeaturePreview, FeatureStatus, Part, PartDefinition, ReferenceGeometry, SketchResult,
    TimelineEntry,
};
