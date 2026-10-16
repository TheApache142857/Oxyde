//! 2D sketch entities and the geometric constraint solver.
//!
//! A [`ConstrainedSketch`] holds points, lines, circles and arcs (lines and arcs share
//! points, as in SolidWorks), plus relations and dimensions ([`Constraint`]).
//! [`ConstrainedSketch::solve`] moves the geometry to satisfy them and reports the
//! remaining degrees of freedom, which entities are fully defined, and which relations are
//! redundant or conflicting. [`ConstrainedSketch::profile_segments`] then feeds the
//! extrusion profile.
//!
//! Pure Rust and kernel-independent. See `docs/ARCHITECTURE.md` §5.

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

mod derive;
mod edit;
mod linalg;
mod sketch;
mod solve;

pub use sketch::{
    ConstrainedSketch, Constraint, Counters, Entity, EntityId, Geometry, Relation, RelationId,
    SketchError, is_valid_name,
};
pub use solve::{ConstraintState, Definition, SolveReport};

#[cfg(test)]
mod tests;
