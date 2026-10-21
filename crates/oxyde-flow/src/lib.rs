//! Flow simulation for Oxyde: the Simulation tab's *Flow* studies, computed by OpenFOAM
//! run as a separate program (`docs/CFD.md`).
//!
//! OpenFOAM is GPL-3.0 and Oxyde MIT, so this crate never links it: it writes an OpenFOAM
//! case (a folder of text files), starts OpenFOAM's programs in their own environment and
//! reads back the files they write.
//!
//! - [`study`] holds a flow study as plain data: the closed surfaces bounding the fluid
//!   (each triangle labelled with the model face it lies on), internal or external flow,
//!   the fluid, laminar or turbulent flow, conditions and goals on faces, mesh and solver
//!   settings.
//! - [`case`] writes it as an OpenFOAM case: the surfaces as an STL file with one region,
//!   hence one boundary patch, per model face; a box of hexahedra (`blockMesh`) that
//!   `snappyHexMesh` refines, cuts and snaps to the surfaces; the solver's dictionaries and
//!   starting fields.
//! - [`install`] finds an OpenFOAM installation of OpenCFD's line (openfoam.com: the MinGW
//!   build on Windows, the packages on Linux, openfoam-app on macOS) and starts its programs
//!   in its environment.
//! - [`run`] runs a case's programs one after the other, following their logs ([`log`]) for
//!   progress and residuals, and stops them on cancel.
//! - [`results`] reads the mesh and the last fields back (OpenFOAM's ASCII files, through
//!   [`foam`]) and works out the goals: forces, flow rates and mean pressures.
//!
//! Units at the interface are Oxyde's: millimetres for positions and lengths, SI otherwise
//! (m/s, Pa, kg/m³, Pa·s, m³/s, kg/s, N). OpenFOAM works in metres: the case is scaled on
//! the way in and the mesh on the way out. Pressures are gauge pressures, in pascals, where
//! OpenFOAM's incompressible solvers hold the pressure divided by the density.
//!
//! The crate depends on no other crate of the workspace, as `oxyde-sim` does not: the
//! application turns the kernel's meshes into [`Surface`]s and persistent face names into
//! labels.

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

pub mod case;
mod dict;
pub mod foam;
pub mod install;
pub mod log;
pub mod mesh;
pub mod results;
pub mod run;
pub mod study;

pub use case::{Case, CaseError, FacePatch, Role, write_case};
pub use install::{InstallError, Installation};
pub use log::{Iteration, LogState};
pub use mesh::PolyMesh;
pub use results::{FlowResults, GoalValue, ReadError};
pub use run::{Progress, RunError, RunReport, Stage, run};
pub use study::{
    Condition, ConditionKind, Domain, FlowStudy, FlowType, Fluid, Goal, GoalKind, MeshSettings,
    SolverSettings, StudyError, Surface, Turbulence,
};
