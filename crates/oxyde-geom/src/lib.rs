//! Geometric primitives for Oxyde: points, vectors, planes, sketch profiles and the central
//! tolerance policy.
//!
//! This crate has no dependencies on the rest of the workspace.
//!
//! All lengths are in millimetres (see `docs/ARCHITECTURE.md` §7).

mod plane;
mod profile;
mod vector;

pub use plane::Plane;
pub use profile::{Curve2, Loop, Profile, ProfileError, ProfileSegment, Region};
pub use vector::{Point2, Point3, Vec3};

/// Two points closer than this are the same point, in millimetres.
pub const LINEAR_TOLERANCE: f64 = 1e-6;

/// Two directions closer than this are the same direction, in radians.
pub const ANGULAR_TOLERANCE: f64 = 1e-9;
