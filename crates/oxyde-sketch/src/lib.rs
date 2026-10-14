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

mod linalg;
mod sketch;
mod solve;

pub use sketch::{ConstrainedSketch, Constraint, Entity, EntityId, Geometry, SketchError};
pub use solve::{ConstraintState, Definition, SolveReport};

#[cfg(test)]
mod tests;
