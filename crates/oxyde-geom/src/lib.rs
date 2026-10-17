//! Geometric primitives for Oxyde: points, vectors, planes, axes, transforms, sketch profiles
//! and the central tolerance policy.
//!
//! This crate has no dependencies on the rest of the workspace.
//!
//! All lengths are in millimetres (see `docs/ARCHITECTURE.md` §7).

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

mod path;
mod plane;
mod profile;
mod transform;
mod vector;

pub use path::{Path, PathError};
pub use plane::Plane;
pub use profile::{Curve2, Loop, Profile, ProfileError, ProfileSegment, Region};
pub use transform::{Axis, Transform};
pub use vector::{Point2, Point3, Vec3};

/// Two points closer than this are the same point, in millimetres.
pub const LINEAR_TOLERANCE: f64 = 1e-6;

/// Two directions closer than this are the same direction, in radians.
pub const ANGULAR_TOLERANCE: f64 = 1e-9;
