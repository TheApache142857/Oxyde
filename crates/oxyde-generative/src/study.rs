//! What a study is made of: fixtures, loads, the solver's options and symmetry planes.

use crate::math::V3;

/// How a fixture holds its faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureKind {
    /// No motion at all (SolidWorks' *Fixed Geometry*).
    Fixed,
    /// No motion normal to the faces; free to slide along them (*Roller/Slider*).
    Roller,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fixture {
    pub kind: FixtureKind,
    /// The face labels it holds.
    pub labels: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoadKind {
    /// A total force in newtons, spread over the faces by area: along `direction` (any
    /// length), or, when `None`, normal to each face, pushing into it.
    Force { total: f64, direction: Option<V3> },
    /// MPa, normal to the faces, pushing into them when positive.
    Pressure { value: f64 },
    /// A force in newtons acting at `point`, off the faces (a component's weight at its
    /// centre of mass), carried to them as a distributing link (RBE3) carries it: spread
    /// over them by area, with the moment its offset makes spread in proportion to each
    /// node's distance from their centre. It stiffens nothing.
    Remote { force: V3, point: V3 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Load {
    pub kind: LoadKind,
    pub labels: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolveOptions {
    /// The solve stops when the residual is this fraction of the loads.
    pub tolerance: f64,
    pub max_iterations: usize,
    /// Threads; 0 for every core.
    pub threads: usize,
}

impl Default for SolveOptions {
    fn default() -> Self {
        Self {
            tolerance: 1e-8,
            max_iterations: 40_000,
            threads: 0,
        }
    }
}

pub fn thread_count(wanted: usize) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    if wanted == 0 {
        cores
    } else {
        wanted.min(cores)
    }
    .max(1)
}

/// Planes the shape is mirrored about, through `point` and parallel to the standard planes:
/// SolidWorks' symmetry control, Fusion 360's symmetry planes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Symmetry {
    /// mm: the point the planes pass through; the origin for the standard planes.
    pub point: V3,
    /// Mirrored across the plane normal to X (the Right plane's), to Y (the Top plane's) and
    /// to Z (the Front plane's).
    pub planes: [bool; 3],
}
