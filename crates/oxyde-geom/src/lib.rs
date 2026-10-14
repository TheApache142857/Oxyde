//! Geometric primitives for Oxyde: points, vectors, planes, axes, transforms and the central tolerance policy.
//!
//! This crate has no dependencies on the rest of the workspace.
//!
//! All lengths are in millimetres (see `docs/ARCHITECTURE.md` §7).

/// A position in 3D model space, in millimetres.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Point3 {
    pub const ORIGIN: Self = Self::new(0.0, 0.0, 0.0);

    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }
}

/// A displacement or direction in 3D model space, in millimetres.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);

    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }
}
