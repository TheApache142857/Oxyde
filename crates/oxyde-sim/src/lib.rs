//! Finite element simulation for Oxyde: the Simulation tab's engine, in the manner of
//! SolidWorks Simulation's linear static study.
//!
//! - [`mesher`] fills closed triangle meshes (one per solid, each triangle labelled with
//!   the model face it lies on) with tetrahedra: a lattice mesher with boundary snapping,
//!   which needs no dependency (see `docs/SIMULATION.md` §2).
//! - [`mesh`] holds the tetrahedral mesh: four- or ten-node elements, a body per element,
//!   the boundary facets with their face labels.
//! - [`contact`] ties bodies meshed apart: bonded, separation, sliding, rough and offset
//!   bonded contacts, as Fusion 360 has them, and their automatic detection.
//! - [`solve`] assembles the stiffness matrix, applies fixtures and loads, solves by
//!   conjugate gradients on every core, preconditioned by algebraic multigrid (`amg`),
//!   and recovers the stresses.
//! - [`thermal`] conducts heat: steady and transient thermal studies (fixed temperatures,
//!   heat flows and fluxes, convection, radiation, heat made inside bodies), on the same
//!   meshes and contacts, solved with the same multigrid; their temperatures expand the
//!   model in a thermal stress study ([`solve::ThermalStrain`]).
//! - [`topology`] optimizes a static study's shape: the stiffest one that keeps a given
//!   share of the mass (SIMP densities, a density filter, optimality criteria), and cuts
//!   it out of the mesh as a closed surface.
//! - [`plot`] turns a result into colour bands on the displaced boundary, for the
//!   viewport.
//! - [`material`] holds the elastic properties, and a guess of them from a material's
//!   name.
//!
//! Units are millimetres, newtons and megapascals (N/mm²), which are consistent:
//! displacements come out in mm and stresses in MPa. The crate depends on no other crate:
//! the application turns the kernel's meshes into [`mesher::SurfaceMesh`]es and the
//! persistent face names of fixtures and loads into labels.

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

mod amg;
mod bvh;
pub mod contact;
pub mod material;
mod math;
pub mod mesh;
pub mod mesher;
pub mod plot;
pub mod solve;
pub mod thermal;
pub mod topology;

pub use contact::{
    Contact, ContactCandidate, ContactKind, ContactResult, ContactSolve, detect_contacts,
};
pub use material::{Elastic, ThermalProps};
pub use math::V3;
pub use mesh::{Facet, TetMesh};
pub use mesher::{MeshError, MeshOptions, MeshReport, SurfaceMesh, mesh_solids};
pub use plot::{Band, Quantity, bands, mesh_edges, rainbow};
pub use solve::{
    Fixture, FixtureKind, Load, LoadKind, Results, SolveError, SolveOptions, Study, Summary,
    ThermalStrain, solve, solve_thermal_stress,
};
pub use thermal::{
    Frame, InternalHeat, KELVIN, ThermalError, ThermalLoad, ThermalLoadKind, ThermalMaterial,
    ThermalResults, ThermalStudy, ThermalSummary, Transient, solve_thermal,
};
pub use topology::{
    Joining, Piece, Preserve, Pull, Shape, Symmetry, TopologyError, TopologyResults, TopologyStep,
    TopologyStudy, optimize, pieces, shape,
};

/// A label on no face: a boundary facet the mesher could not tie to a model face.
pub const NO_LABEL: u32 = u32::MAX;
