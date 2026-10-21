//! Generative design: shapes grown between bodies to keep and bodies to stay clear of, as
//! Fusion 360's generative design makes them (`docs/SIMULATION.md` §9).
//!
//! - **The design space** ([`Grid`]): the box around the preserve bodies cut into cubic
//!   voxels, each *preserve* (always material), *obstacle* or *outside* (never material) or
//!   *design*. Fixtures and loads act on the grid's nodes through the preserve voxels.
//! - **The elements** are the voxels themselves, eight-node bricks all alike, solved by
//!   conjugate gradients preconditioned by geometric multigrid, the finest level never
//!   assembled (`multigrid`): that is what lets the grid be fine.
//! - **Minimize mass** ([`Objective::MinMass`]) keeps the estimated von Mises stress under
//!   the material's strength over the safety factor (and the displacement under a limit):
//!   the stresses at the voxels' centres relaxed by the density (`ρ^½ σ`, so void voxels
//!   carry none) and gathered by a p-norm (p = 8) scaled to the largest, its slopes by an
//!   adjoint solve per load case, and the design moved by the method of moving asymptotes
//!   (`mma`) towards the least mass that keeps them.
//! - **The optimization** is SIMP as in [`crate::topology`]: a design density per voxel (or
//!   per group of voxels the manufacturing ties together), averaged over a sphere (the
//!   density filter, of radius half the minimum thickness: members thinner than that are
//!   discouraged, not ruled out, as projection alone guarantees no length scale), then
//!   pushed towards solid or void by a smoothed Heaviside projection whose sharpness `β`
//!   doubles as the design settles, so the shape comes out crisp rather than grey; the
//!   stiffness goes as the cube of the projected density. Optimality criteria move the design, the mass target
//!   met by the projected densities (linearised at each iteration, so it is met once the
//!   design settles).
//! - **Manufacturing** ([`Method`]): *symmetry* ties each voxel to its mirror images (the
//!   grid is laid symmetric about the planes, so the images are voxels); *2-axis cutting*
//!   ties every voxel of a column along the cutting direction together; *casting* caps each
//!   projected density at the one behind it along the ejection direction (one way, or both
//!   ways from a parting plane), so nothing hangs over void the mould could not leave.
//!   Material a method cannot avoid (behind a preserve voxel, in line with one) is made
//!   solid, and room it cannot use (in front of an obstacle, in line with one) void.
//!   *Additive* prints layer by layer along the build direction (`additive`): a voxel can
//!   only be as dense as the voxel under it and that one's four neighbours across the layer
//!   (a cross), which keeps overhangs within 45° in every direction.
//! - **Milling from several sides** (Fusion 360's setup orientations, as Autodesk's
//!   machinery example mills from +Y and −Y): along each side's lines from the tool inwards,
//!   each voxel is raised to the densest one in front of it (what the tool cannot reach from
//!   that side stays), then the least over the sides is taken (what it reaches from one side
//!   is gone). Each void voxel (an obstacle) is reached from the side with the fewest design
//!   voxels in front of it, which are made void; a voxel behind a preserve voxel from every
//!   side is made solid. From one side this is the one-way pull of casting, which milling
//!   from one side keeps.
//! - **Grayness** (Sigmund's measure of non-discreteness, `4 ρ (1 − ρ)` averaged over the
//!   design voxels' projected densities: 0 when every one is solid or void, 1 when all are
//!   half) is reported with each outcome, and a design greyer than [`GRAYNESS_SETTLED`]
//!   does not count as settled.
//! - [`surface`] cuts the densities into a smooth closed surface; [`decimate`] and
//!   [`solid_pieces`] make it something a modelling kernel takes.
//! - [`check_part`] checks the part made of an outcome: its surface meshed with
//!   tetrahedra, each load case solved on it by the static solver, for numbers the voxels
//!   only estimate.

// Indices into the grid's arrays and the design's groups, all sized here from the grid.
#![allow(clippy::indexing_slicing)]

mod additive;
mod brick;
mod check;
mod decimate;
mod grid;
mod mma;
mod multigrid;
mod surface;

pub use check::{CheckError, Missing, PartCheck, check_part};
pub use decimate::{decimate, decimate_holding, solid_pieces};
pub use grid::{Grid, MAX_VOXELS, Voxel, design_box, voxel_size};
pub use multigrid::{Plan, PlanLevel, SMOOTHING_DEGREE, SPECTRUM_RATIO, Solved};
pub use surface::{exact_surface, surface};

use std::sync::atomic::{AtomicBool, Ordering};

use crate::material::Elastic;
use crate::math::V3;
use crate::mesher::SurfaceMesh;
use crate::solve::{Fixture, Load, SolveOptions, thread_count};
use crate::topology::Symmetry;
use brick::{Brick, DOFS, von_mises};
use multigrid::{Dims, Fine, par_fill};

/// The stiffness left in void voxels, as a fraction of the material's.
const VOID_STIFFNESS: f64 = 1e-6;
/// SIMP's penalty: stiffness goes as the projected density to this power.
const PENALTY: f64 = 3.0;
/// Voxels whose stiffness is under this share of the material's (a projected density under
/// about 2 %: void) are, in the solve, the brick's Laplacian scaled as the brick would be:
/// as weak, as free to move as a whole, a third of the work. Their slopes are still the
/// brick's, so material can grow back into them.
const SCALAR_BELOW: f64 = 1e-5;
/// The projection's threshold, and its sharpness at the end; it starts at 1 and doubles
/// when the design settles, or after `BETA_STEP_AFTER` iterations at one sharpness (which
/// brings it to the end within 125 iterations).
const ETA: f64 = 0.5;
const BETA_MAX: f64 = 32.0;
const BETA_STEP_AFTER: usize = 25;
/// The design densities never go under this, so optimality criteria can bring any back.
const DESIGN_MIN: f64 = 1e-3;
/// The most a design density moves in one iteration, and from a sharpness of 8 on.
const MOVE_LIMIT: f64 = 0.2;
const LATE_MOVE_LIMIT: f64 = 0.1;
/// Settled, at the last sharpness: no design density moves by more than this; or, the
/// densities going back and forth at the move limit as optimality criteria do once the
/// shape is crisp, the compliance has stayed within `STEADY_COMPLIANCE` over the last
/// `STEADY_OVER` iterations with the mass on target to `STEADY_MASS`; or the last stage has
/// run as long as the others. At the last sharpness the compliance jitters by a few tenths
/// of a percent and the mass by about one: a tighter test was met or missed by chance.
const SETTLED_CHANGE: f64 = 0.01;
/// Nor, by either of those tests, while its grayness is over this: a design that stopped
/// moving grey (a casting pulled in the plane the part bends in, in topology studies) has
/// not settled, and runs on to the end of the last stage. At the last sharpness a crisp
/// design's only grey voxels are a thin band where it ends, a few hundredths of the
/// design voxels.
pub const GRAYNESS_SETTLED: f64 = 0.1;
const STEADY_COMPLIANCE: f64 = 5e-3;
const STEADY_OVER: usize = 10;
const STEADY_MASS: f64 = 0.01;
/// Minimize mass's stress measure: the relaxation exponent (`ρ^q σ`) and the p-norm's.
const STRESS_RELAXATION: f64 = 0.5;
/// Under this projected density a voxel's weight goes as the cube of it, as its stiffness
/// does, above it as the density itself (value-continuous there): near-void material
/// weighs next to nothing, or its weight would outrun the stiffness carrying it and grey
/// regions sag under their own weight, the parasitic effect of self-weight in SIMP
/// (Bruyneel and Duysinx, 2005). The mass the target and the outcome count is the
/// density's own.
const WEIGHT_KNEE: f64 = 0.1;
const NORM_POWER: i32 = 8;
/// How far the p-norm's scaling to the largest value moves each iteration (Le et al. 2010).
const NORMALIZATION_DAMPING: f64 = 0.5;
/// MMA's move limit, and from a sharpness of 8 on.
const MMA_MOVE: f64 = 0.1;
const LATE_MMA_MOVE: f64 = 0.05;
/// A limit counts as met within this share of it.
const LIMIT_TOLERANCE: f64 = 0.01;
/// The densities are kept every this many iterations, for the slider through them.
pub const SNAPSHOT_EVERY: usize = 10;
/// The iterations' solves stop at this residual, a fraction of the loads (as topology
/// studies' do), and give up after this many conjugate gradient iterations. Before the
/// projection is at its sharpest, a tenth as tight is enough: the design changes far more
/// from one iteration to the next than that error moves it (on the test bracket, a fifth
/// fewer conjugate gradient iterations and the same compliance in the end), and the
/// sharpest stage, which settles on the compliance staying put, gets the tight one.
const SOLVE_TOLERANCE: f64 = 1e-5;
const EARLY_TOLERANCE: f64 = 1e-4;
const SOLVE_ITERATIONS: usize = 2000;
const NONE: u32 = u32::MAX;

/// A body no material may go into.
#[derive(Debug, Clone, PartialEq)]
pub struct Obstacle {
    pub surface: SurfaceMesh,
    /// mm it is grown by on every side; 0 for none.
    pub offset: f64,
}

/// Where material may go, from the model's bodies.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DesignSpace {
    /// The bodies kept as they are, their faces labelled: fixtures and loads name them.
    pub preserve: Vec<SurfaceMesh>,
    pub obstacles: Vec<Obstacle>,
    /// A body the material starts in and stays inside; `None` for the box around the
    /// preserve bodies.
    pub starting_shape: Option<SurfaceMesh>,
    /// mm the box is grown by around the preserve bodies when there is no starting shape;
    /// `None` for a tenth of its largest side.
    pub margin: Option<f64>,
}

/// Fixtures and loads the outcome carries at once: indices into the study's `fixtures`,
/// `loads`, `gravity` and `remote_masses`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LoadCase {
    pub fixtures: Vec<usize>,
    pub loads: Vec<usize>,
    /// The accelerations it carries: their sum weighs the material and the case's remote
    /// masses.
    pub gravity: Vec<usize>,
    /// The remote masses the case's accelerations act on.
    pub remote_masses: Vec<usize>,
}

/// A component carried by faces of the preserve bodies (Fusion 360's point mass, the
/// instrument a bracket holds): under a load case's acceleration its weight reaches the
/// faces as a force and a moment spread over them by area, as a distributing link (RBE3)
/// carries it, the faces moving on average as the mass does. It stiffens nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteMass {
    /// kg.
    pub mass: f64,
    /// mm: its centre of mass.
    pub point: V3,
    /// The faces it is carried by, by label.
    pub labels: Vec<u32>,
}

/// What a generative study looks for.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Objective {
    /// The stiffest shape weighing [`GenerativeStudy::mass_target`].
    #[default]
    MaxStiffness,
    /// The lightest shape whose estimated von Mises stress stays under the material's
    /// strength over `safety_factor` and, when there is a `displacement_limit` (mm), whose
    /// displacement stays under it.
    MinMass {
        safety_factor: f64,
        displacement_limit: Option<f64>,
    },
}

/// What a generative study asks, for one material and one way of making it.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerativeStudy {
    pub space: DesignSpace,
    /// On faces of the preserve bodies, by label.
    pub fixtures: Vec<Fixture>,
    pub loads: Vec<Load>,
    /// Accelerations, m/s² (gravity, 9.81 down; a shock of so many g): a load case's sum
    /// weighs the material and the case's remote masses.
    pub gravity: Vec<V3>,
    pub remote_masses: Vec<RemoteMass>,
    /// The load cases, each carried in turn: the stiffest shape for them all, its
    /// compliance their sum. None: one case of every fixture, load, acceleration and
    /// remote mass.
    pub load_cases: Vec<LoadCase>,
    pub objective: Objective,
    /// kg: the outcome's mass, preserve voxels included ([`Objective::MaxStiffness`]).
    pub mass_target: f64,
    /// mm: the thinnest member wanted, which the density filter's radius (half of it)
    /// discourages thinner ones from; `None` for three voxels.
    pub member_size: Option<f64>,
    pub max_iterations: usize,
    /// mm: the voxels' edge ([`voxel_size`] gives it from a resolution).
    pub voxel_size: f64,
    /// Planes the outcome is mirrored about.
    pub symmetry: Option<Symmetry>,
}

impl GenerativeStudy {
    /// The load cases: [`GenerativeStudy::load_cases`], or one of every fixture and load.
    pub fn cases(&self) -> Vec<LoadCase> {
        if self.load_cases.is_empty() {
            vec![LoadCase {
                fixtures: (0..self.fixtures.len()).collect(),
                loads: (0..self.loads.len()).collect(),
                gravity: (0..self.gravity.len()).collect(),
                remote_masses: (0..self.remote_masses.len()).collect(),
            }]
        } else {
            self.load_cases.clone()
        }
    }
}

/// One of the six directions along the axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Direction {
    /// 0 for X, 1 for Y, 2 for Z.
    pub axis: usize,
    pub positive: bool,
}

impl Direction {
    /// Its bit in [`Sides`]: +X, −X, +Y, −Y, +Z, −Z.
    fn bit(self) -> u8 {
        1 << (2 * self.axis.min(2) + usize::from(!self.positive))
    }

    fn label(self) -> String {
        format!(
            "{}{}",
            if self.positive { "+" } else { "−" },
            ["X", "Y", "Z"][self.axis.min(2)]
        )
    }
}

/// Some of the six directions along the axes, as a set: the sides a part is milled from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Sides(u8);

impl Sides {
    /// The sides of `directions` (each once, whatever their order).
    pub fn of(directions: &[Direction]) -> Sides {
        Sides(directions.iter().fold(0, |bits, d| bits | d.bit()))
    }

    /// One side.
    pub fn single(direction: Direction) -> Sides {
        Sides(direction.bit())
    }

    /// The directions, in the order +X, −X, +Y, −Y, +Z, −Z.
    pub fn directions(self) -> Vec<Direction> {
        (0..6)
            .filter(|i| self.0 & (1 << i) != 0)
            .map(|i| Direction {
                axis: i / 2,
                positive: i % 2 == 0,
            })
            .collect()
    }

    pub fn contains(self, direction: Direction) -> bool {
        self.0 & direction.bit() != 0
    }

    pub fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// How an outcome is to be made.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Method {
    /// Any shape.
    Unrestricted,
    /// 2-axis cutting (a water jet, a laser): the shape the same all along the axis.
    Cutting { axis: usize },
    /// Cast in a mould pulled off along `direction`: going that way the shape never widens,
    /// so it has no undercut. With `parting`, a two-part mould pulled off both ways from the
    /// plane across the direction through that point.
    Casting {
        direction: Direction,
        parting: Option<V3>,
    },
    /// Printed in layers along `direction`, from a build plate at the grid's end behind it:
    /// nothing overhangs more than 45° from the direction.
    Additive { direction: Direction },
    /// 3-axis milled from the sides `sides` point to: the tool reaches every face from one
    /// of them, so the shape has no undercut seen from all of them at once. From one side
    /// that is a one-way pull towards the tool; from several, each voxel the tool cannot
    /// reach from any of them stays (module documentation, *Milling from several sides*).
    /// Pockets and gaps narrower than `tool_diameter` (mm) are discouraged: the density
    /// filter's radius is at least half of it, which tends to keep them out but does not
    /// rule them out.
    Milling {
        sides: Sides,
        tool_diameter: Option<f64>,
    },
}

impl Method {
    /// The one-way pull a method makes the shape keep, with its parting point: casting's
    /// own, or milling's from one side, towards the tool.
    fn pull(self) -> Option<(Direction, Option<V3>)> {
        match self {
            Method::Casting { direction, parting } => Some((direction, parting)),
            Method::Milling { sides, .. } if sides.len() == 1 => {
                sides.directions().first().map(|d| (*d, None))
            }
            _ => None,
        }
    }

    /// The sides a part milled from more than one side is milled from.
    fn several_sides(self) -> Option<Sides> {
        match self {
            Method::Milling { sides, .. } if sides.len() > 1 => Some(sides),
            _ => None,
        }
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let axis = |a: usize| ["X", "Y", "Z"][a.min(2)];
        match self {
            Method::Unrestricted => write!(f, "unrestricted"),
            Method::Cutting { axis: a } => write!(f, "2-axis cutting along {}", axis(*a)),
            Method::Casting { direction, parting } => write!(
                f,
                "casting pulled {}{}{}",
                if parting.is_some() {
                    "both ways along "
                } else if direction.positive {
                    "towards +"
                } else {
                    "towards −"
                },
                axis(direction.axis),
                if parting.is_some() {
                    " from the parting plane"
                } else {
                    ""
                }
            ),
            Method::Additive { direction } => write!(
                f,
                "additive, printed towards {}{}",
                if direction.positive { "+" } else { "−" },
                axis(direction.axis)
            ),
            Method::Milling { sides, .. } => {
                let labels: Vec<String> = sides.directions().iter().map(|d| d.label()).collect();
                match labels.split_last() {
                    Some((last, rest)) if !rest.is_empty() => {
                        write!(f, "3-axis milling from {} and {last}", rest.join(", "))
                    }
                    _ => write!(f, "3-axis milling from {}", labels.concat()),
                }
            }
        }
    }
}

/// One iteration of an outcome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GenerativeStep {
    /// N·mm: the compliance of the design solved (the work of the loads, summed over the
    /// load cases; the lower, the stiffer).
    pub compliance: f64,
    /// kg: its mass.
    pub mass: f64,
    /// The largest change of a design density the update made.
    pub change: f64,
}

/// A shape generated for one material and one method.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Each voxel's density at the end before the projection, 0 (void) to 255 (solid), as
    /// the files keep it: the shape is where it is over half, as it is where the projected
    /// density is (the projection keeps half where it is). The projected densities are all
    /// but 0 or 1, and a surface cut from them would step from voxel to voxel; these vary
    /// smoothly over the filter's radius, and place it between the voxels' centres.
    pub density: Vec<u8>,
    /// Every [`SNAPSHOT_EVERY`]th iteration's densities, as `density`, by iteration from 1.
    pub snapshots: Vec<(usize, Vec<u8>)>,
    pub history: Vec<GenerativeStep>,
    /// The design settled before the iteration limit.
    pub converged: bool,
    /// The run was stopped (`cancel` set) after an iteration or more: the outcome is the
    /// design as far as it got, its numbers from the last solve, which may not have
    /// finished.
    pub stopped: bool,
    /// kg.
    pub mass: f64,
    /// N·mm.
    pub compliance: f64,
    /// mm: the largest displacement of a node of the material.
    pub max_displacement: f64,
    /// MPa: the largest von Mises stress at the centres of solid voxels, but those the
    /// fixtures and loads act through (where stress is singular). Voxels' corners
    /// concentrate stress, so it is an estimate.
    pub max_von_mises: f64,
    /// The yield strength over it, when the material has one.
    pub min_factor_of_safety: Option<f64>,
    /// Conjugate gradient iterations, over every solve.
    pub solver_iterations: usize,
    /// mm: the density filter's radius.
    pub filter_radius: f64,
    /// Its projected densities' grayness: `4 ρ (1 − ρ)` averaged over the design voxels, 0
    /// when every one is solid or void, 1 when all are half (Sigmund's measure of
    /// non-discreteness).
    pub grayness: f64,
}

/// Why an outcome could not be generated.
#[derive(Debug, Clone, PartialEq)]
pub enum GenerativeError {
    /// No preserve body, or one without a surface.
    NoPreserve,
    /// The voxel size is not a positive length.
    BadVoxelSize,
    TooManyVoxels {
        count: usize,
    },
    /// No voxel is left for material to go in.
    NoDesignSpace,
    /// An obstacle reaches well into a preserve body (their indices).
    ObstacleOverPreserve {
        obstacle: usize,
        preserve: usize,
    },
    /// A fixture or a load is on a face of no preserve body.
    FaceNotOnPreserve {
        label: u32,
    },
    /// No fixture holds anything.
    NotHeld,
    /// The loads act on nothing.
    NoLoads,
    /// A remote mass (its index) has no positive mass, no point or no face of a preserve
    /// body.
    BadRemoteMass {
        index: usize,
    },
    /// The mass target is not between the preserve voxels' mass and the whole design
    /// space's, kg.
    BadTarget {
        min: f64,
        max: f64,
    },
    /// The material has no usable modulus, Poisson's ratio or density.
    BadMaterial,
    /// Minimize mass needs the material's strength (yield, else tensile), and a positive
    /// safety factor and displacement limit.
    NoStrength,
    /// The method cannot make the design space: a preserve body and an obstacle in line
    /// along its direction, the one behind the other, at `at` (mm).
    NotMakeable {
        method: Method,
        at: V3,
    },
    /// A casting direction crosses a symmetry plane that is not its parting plane.
    PullAcrossSymmetry,
    /// A build direction crosses a symmetry plane: layers printed one way cannot give a
    /// mirrored shape.
    BuildAcrossSymmetry,
    NotConverged {
        iterations: usize,
        residual: f64,
    },
    /// The solver failed: why.
    Solver(String),
    Cancelled,
}

impl std::fmt::Display for GenerativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenerativeError::NoPreserve => write!(
                f,
                "the study has no preserve body: give one or more bodies the Preserve role"
            ),
            GenerativeError::BadVoxelSize => {
                write!(f, "the resolution does not give a usable voxel size")
            }
            GenerativeError::TooManyVoxels { count } => write!(
                f,
                "the design space would need {count} voxels, more than {MAX_VOXELS}: lower the resolution"
            ),
            GenerativeError::NoDesignSpace => write!(
                f,
                "no room is left for material: the obstacles and preserve bodies fill the design space"
            ),
            GenerativeError::ObstacleOverPreserve { obstacle, preserve } => write!(
                f,
                "obstacle {} reaches into preserve body {}: material cannot be both kept and kept out",
                obstacle + 1,
                preserve + 1
            ),
            GenerativeError::FaceNotOnPreserve { .. } => write!(
                f,
                "a fixture or a load is on a face of a body that is not a preserve body: put them on preserve bodies"
            ),
            GenerativeError::NotHeld => write!(
                f,
                "nothing holds the design: add a fixture on a preserve body"
            ),
            GenerativeError::NoLoads => write!(
                f,
                "no load acts on the design: add a force or a pressure on a preserve body, or gravity"
            ),
            GenerativeError::BadRemoteMass { index } => write!(
                f,
                "remote mass {} needs a positive mass, a point and faces of a preserve body",
                index + 1
            ),
            GenerativeError::BadTarget { min, max } => write!(
                f,
                "the mass target must be between {:.3} kg (the preserve bodies) and {:.3} kg (the whole design space)",
                min, max
            ),
            GenerativeError::BadMaterial => {
                write!(f, "the material has no usable stiffness or density")
            }
            GenerativeError::NoStrength => write!(
                f,
                "minimize mass needs the material's yield or tensile strength, and a positive safety factor"
            ),
            GenerativeError::NotMakeable { method, at } => write!(
                f,
                "{method} cannot make this design space: a preserve body and an obstacle are in line along its direction near ({:.1}, {:.1}, {:.1}) mm",
                at[0], at[1], at[2]
            ),
            GenerativeError::BuildAcrossSymmetry => write!(
                f,
                "the build direction crosses a symmetry plane, and layers printed one way cannot give a mirrored shape: print along the plane, or take it off"
            ),
            GenerativeError::PullAcrossSymmetry => write!(
                f,
                "the casting or milling direction crosses a symmetry plane, and a shape made from one side cannot be mirrored across it: pull both ways from that plane, or take it off"
            ),
            GenerativeError::NotConverged {
                iterations,
                residual,
            } => write!(
                f,
                "the solve did not converge ({iterations} iterations, residual {residual:.1e}): the design may be held too loosely"
            ),
            GenerativeError::Solver(why) => write!(f, "the solver failed: {why}"),
            GenerativeError::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// What a cell is in a [`System`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    /// No element: no material at all.
    None,
    /// The brick scaled by the voxel's stiffness.
    Brick,
    /// The brick's Laplacian, the same for each direction, scaled the same way: a void
    /// voxel.
    Laplacian,
}

/// The system an optimization iteration solves, `K u = f`, as a [`Backend`] sees it: each
/// voxel's brick (or, void, its Laplacian) scaled by its stiffness, assembled over the
/// grid's nodes (node `(i, j, k)` is `i + (nx + 1) (j + (ny + 1) k)`, three unknowns each);
/// a held node keeping only the motions its fixtures allow, `P K P + d (I − P)`, `P` its
/// projector and `d` [`System::held_stiffness`]; a node no voxel touches left out (its rows
/// the identity, its value 0).
pub struct System<'a> {
    fine: &'a Fine<'a>,
    loads: Vec<f64>,
    threads: usize,
}

impl System<'_> {
    /// Voxels along X, Y and Z.
    pub fn cells(&self) -> [usize; 3] {
        self.fine.dims().cells
    }

    /// N/mm per voxel: what its brick or Laplacian is scaled by.
    pub fn stiffness(&self) -> &[f64] {
        self.fine.stiffness()
    }

    pub fn kind(&self, cell: usize) -> CellKind {
        match self.fine.scalar_cell(cell) {
            None => CellKind::None,
            Some(false) => CellKind::Brick,
            Some(true) => CellKind::Laplacian,
        }
    }

    /// The brick for a unit stiffness, 24 × 24, row-major, unknowns corner by corner (corner
    /// `a` at `(a & 1, (a >> 1) & 1, (a >> 2) & 1)`), x y z each.
    pub fn brick(&self) -> &[f64; brick::SIZE] {
        &self.fine.brick().k
    }

    /// Its Laplacian for a unit stiffness, 8 × 8, by corner.
    pub fn laplacian(&self) -> &[f64; 64] {
        &self.fine.brick().laplacian
    }

    /// Per node: the index of its projector, or `u32::MAX` when it is free.
    pub fn held(&self) -> &[u32] {
        self.fine.held()
    }

    /// The motions each held node keeps, `I` less its held directions, row-major.
    pub fn projectors(&self) -> &[[f64; 9]] {
        self.fine.projectors()
    }

    /// The stiffness of node `node`'s held directions.
    pub fn held_stiffness(&self, node: usize) -> f64 {
        self.fine.held_stiffness(node)
    }

    /// Per node: whether a voxel with an element touches it.
    pub fn active(&self) -> &[bool] {
        self.fine.active_nodes()
    }

    /// The loads, N, three per node, already kept to the allowed motions.
    pub fn loads(&self) -> &[f64] {
        &self.loads
    }

    /// The multigrid hierarchy the CPU would use, coarsened until a level has at most
    /// `coarsest_nodes` nodes, in plain arrays.
    pub fn plan(&self, coarsest_nodes: usize) -> Result<Plan, GenerativeError> {
        multigrid::plan(self.fine, coarsest_nodes, self.threads)
    }
}

/// Solves optimization iterations' systems in place of the CPU's multigrid: a GPU's.
pub trait Backend {
    /// Solves `system` from `x` (the last solve's displacements, kept to the allowed
    /// motions) to a residual of `tolerance` times the loads, in at most `max_iterations`
    /// conjugate gradient iterations.
    fn solve(
        &mut self,
        system: &System<'_>,
        x: &mut [f64],
        tolerance: f64,
        max_iterations: usize,
        cancel: &AtomicBool,
    ) -> Result<Solved, GenerativeError>;
}

/// The CPU's own multigrid as a [`Backend`]: the reference other backends are tested
/// against.
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuBackend;

impl Backend for CpuBackend {
    fn solve(
        &mut self,
        system: &System<'_>,
        x: &mut [f64],
        tolerance: f64,
        max_iterations: usize,
        cancel: &AtomicBool,
    ) -> Result<Solved, GenerativeError> {
        multigrid::solve(
            system.fine,
            &system.loads,
            x,
            tolerance,
            max_iterations,
            cancel,
            system.threads,
        )
    }
}

/// What a voxel is to the optimization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// A design density decides it.
    Free,
    /// Always material.
    Solid,
    /// Never material, and no element.
    Void,
}

/// Generates the outcome of `study` on `grid` (from [`Grid::build`] of the same study) for
/// one material and one method. `progress` hears the fraction done, from 0 to 1, as the
/// iteration limit counts it.
pub fn generate(
    grid: &Grid,
    study: &GenerativeStudy,
    method: Method,
    material: &Elastic,
    options: &SolveOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<Outcome, GenerativeError> {
    generate_on(
        grid, study, method, material, options, None, cancel, progress,
    )
}

/// [`generate`], each iteration's system solved by `backend` (a GPU's) instead of the CPU's
/// multigrid when there is one.
#[allow(clippy::too_many_arguments)]
pub fn generate_on(
    grid: &Grid,
    study: &GenerativeStudy,
    method: Method,
    material: &Elastic,
    options: &SolveOptions,
    mut backend: Option<&mut dyn Backend>,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<Outcome, GenerativeError> {
    let modulus = material.youngs_modulus;
    if !(modulus.is_finite()
        && modulus > 0.0
        && material.poisson > -1.0
        && material.poisson < 0.5
        && material.density.is_finite()
        && material.density > 0.0)
    {
        return Err(GenerativeError::BadMaterial);
    }
    let threads = thread_count(options.threads);
    let h = grid.size;
    let voxels = grid.voxel_count();
    let symmetric = grid_symmetry(grid, study);
    let state = states(grid, method, study.symmetry.as_ref(), symmetric)?;
    let (group, groups) = groups(grid, method, &state, symmetric);

    // A milling tool's radius too: no pocket narrower than the tool.
    let tool = match method {
        Method::Milling {
            tool_diameter: Some(d),
            ..
        } if d.is_finite() && d > 0.0 => 0.5 * d,
        _ => 0.0,
    };
    let radius = study
        .member_size
        .filter(|d| d.is_finite() && *d > 0.0)
        .map_or(1.5 * h, |d| 0.5 * d)
        .max(1.5 * h)
        .max(tool);
    // 2-axis cutting filters across the cut only, so whole columns stay alike.
    let flat = match method {
        Method::Cutting { axis } => Some(axis.min(2)),
        _ => None,
    };
    let filter = Filter::new(grid, &state, radius, flat, threads);
    let chains = method
        .pull()
        .map(|(direction, parting)| casting_chains(grid, direction, parting));
    // Milled from several sides: each side's chains, from the back to the front.
    let reaches: Option<Vec<Vec<Vec<usize>>>> = method.several_sides().map(|sides| {
        sides
            .directions()
            .into_iter()
            .map(|d| casting_chains(grid, d, None))
            .collect()
    });
    let printing = match method {
        Method::Additive { direction } => Some(additive::Printing::new(grid, direction)),
        _ => None,
    };

    // kg per voxel: kg/m³ × mm³ × 1e-9.
    let voxel_mass = material.density * h * h * h * 1e-9;
    let solid = state.iter().filter(|s| **s == State::Solid).count();
    let free = state.iter().filter(|s| **s == State::Free).count();
    let min = solid as f64 * voxel_mass;
    let max = (solid + free) as f64 * voxel_mass;
    let target = study.mass_target;
    // Minimize mass: the stress the material may take, and the displacement limit.
    let limits = match study.objective {
        Objective::MaxStiffness => {
            if !(target > min && target < max) {
                return Err(GenerativeError::BadTarget { min, max });
            }
            None
        }
        Objective::MinMass {
            safety_factor,
            displacement_limit,
        } => {
            let stress = material.yield_strength / safety_factor;
            let displacement = displacement_limit.filter(|d| d.is_finite() && *d > 0.0);
            if !(stress.is_finite() && stress > 0.0)
                || displacement_limit.is_some() && displacement.is_none()
            {
                return Err(GenerativeError::NoStrength);
            }
            Some((stress, displacement))
        }
    };

    let brick = Brick::new(material.poisson);
    let dims = grid.dims();
    let nodes = dims.node_count();
    // Each load case's held nodes, their projectors, its loads and its acceleration.
    let cases: Vec<CaseSystem> = grid
        .cases
        .iter()
        .map(|case| {
            let mut held = vec![NONE; nodes];
            let mut projectors = Vec::with_capacity(case.holds.len());
            for (node, p) in &case.holds {
                if let Some(slot) = held.get_mut(*node as usize) {
                    *slot = projectors.len() as u32;
                    projectors.push(*p);
                }
            }
            let mut loads = vec![0.0; 3 * nodes];
            for (node, f) in &case.forces {
                let n = *node as usize;
                if n < nodes {
                    loads[3 * n..3 * n + 3].copy_from_slice(f);
                }
            }
            CaseSystem {
                held,
                projectors,
                loads,
                acceleration: case.acceleration,
            }
        })
        .collect();
    if cases.is_empty() {
        return Err(GenerativeError::NoLoads);
    }
    // The material weighs in some case: its loads change with the design.
    let weighted = cases.iter().any(|c| c.acceleration != [0.0; 3]);

    // The design starts uniform: at the density whose projection meets the target, or solid
    // (where the stresses are least) to minimize mass.
    let mut beta = 1.0;
    let start = match limits {
        None => {
            let share = (target - min) / (max - min);
            let t = (beta * ETA).tanh();
            (ETA + ((2.0 * share - 1.0) * t).atanh() / beta).clamp(DESIGN_MIN, 1.0)
        }
        Some(_) => 1.0,
    };
    let mut x = vec![start; groups];
    let mut mma = mma::Mma::default();
    // The adjoint solutions, kept to start the next ones from: per constraint, per case.
    let mut adjoints = vec![vec![vec![0.0; 3 * nodes]; cases.len()]; 2];
    let mut stress_scale = 1.0;
    let mut displacement_scale = 1.0;
    let mut feasible = true;
    let mut displacements = vec![vec![0.0; 3 * nodes]; cases.len()];
    let iterations = study.max_iterations.max(1);
    let mut history: Vec<GenerativeStep> = Vec::new();
    let mut snapshots: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut converged = false;
    let mut stopped = false;
    let mut at_beta = 0;
    let mut solver_iterations = 0;
    let mut solved_rho: Vec<f64> = Vec::new();
    let mut solved_field: Vec<f64> = Vec::new();
    let mut compliance = 0.0;
    // N·mm: the first compliance, what weighted stiffness studies scale theirs to.
    let mut first_compliance: Option<f64> = None;

    'iterations: for iteration in 0..iterations {
        // Stopped after an iteration or more: the design as far as it got.
        if cancel.load(Ordering::Relaxed) {
            if history.is_empty() {
                return Err(GenerativeError::Cancelled);
            }
            stopped = true;
            break;
        }
        // Design, filtered, capped along a casting direction, projected.
        let design: Vec<f64> = (0..voxels)
            .map(|v| match state[v] {
                State::Free => x[group[v] as usize],
                State::Solid => 1.0,
                State::Void => 0.0,
            })
            .collect();
        let filtered = filter.forward(&design, threads);
        // Capped along a casting direction, reached from the milling sides, or printed
        // layer by layer.
        let printed = printing.as_ref().map(|p| p.forward(&filtered, &state));
        let (capped, source) = match (&chains, &reaches, &printed) {
            (Some(chains), _, _) => cap(chains, &state, &filtered),
            (None, Some(sides), _) => reach(sides, &state, &filtered),
            (None, None, Some(p)) => (p.printed.clone(), Vec::new()),
            (None, None, None) => (filtered.clone(), Vec::new()),
        };
        let rho: Vec<f64> = (0..voxels)
            .map(|v| match state[v] {
                State::Free => project(capped[v], beta),
                State::Solid => 1.0,
                State::Void => 0.0,
            })
            .collect();
        let gray = grayness(&rho, &state);
        let stiffness: Vec<f64> = (0..voxels)
            .map(|v| match state[v] {
                State::Free => {
                    modulus * h * (VOID_STIFFNESS + rho[v].powf(PENALTY) * (1.0 - VOID_STIFFNESS))
                }
                State::Solid => modulus * h,
                State::Void => 0.0,
            })
            .collect();
        let tolerance = options.tolerance.max(if beta < BETA_MAX {
            EARLY_TOLERANCE
        } else {
            SOLVE_TOLERANCE
        });
        // kg: each voxel's weight as the loads see it, when the material weighs.
        let weights: Vec<f64> = if weighted {
            (0..voxels)
                .map(|v| match state[v] {
                    State::Free => voxel_mass * weight_share(rho[v]).0,
                    State::Solid => voxel_mass,
                    State::Void => 0.0,
                })
                .collect()
        } else {
            Vec::new()
        };
        // Each case's loads with the material's weight under its acceleration.
        let totals: Vec<Option<Vec<f64>>> = cases
            .iter()
            .map(|case| {
                (case.acceleration != [0.0; 3]).then(|| {
                    let mut f = case.loads.clone();
                    add_weight(dims, &weights, case.acceleration, &mut f);
                    f
                })
            })
            .collect();
        // Each load case solved in turn, its energies and its work added up.
        let mut energies = vec![0.0; voxels];
        compliance = 0.0;
        for ((case, total), displacement) in cases.iter().zip(&totals).zip(&mut displacements) {
            let loads: &[f64] = total.as_deref().unwrap_or(&case.loads);
            // Void voxels are Laplacians in the solve, a third of a brick's work.
            let fine = Fine::new(
                dims,
                &brick,
                &stiffness,
                &case.held,
                &case.projectors,
                SCALAR_BELOW * modulus * h,
            );
            let solved = match backend.as_deref_mut() {
                None => multigrid::solve(
                    &fine,
                    loads,
                    displacement,
                    tolerance,
                    SOLVE_ITERATIONS,
                    cancel,
                    threads,
                ),
                Some(backend) => {
                    fine.project(displacement);
                    let mut kept = loads.to_vec();
                    fine.project(&mut kept);
                    let system = System {
                        fine: &fine,
                        loads: kept,
                        threads,
                    };
                    backend.solve(&system, displacement, tolerance, SOLVE_ITERATIONS, cancel)
                }
            };
            let solved = match solved {
                Ok(s) => s,
                Err(GenerativeError::Cancelled) if !history.is_empty() => {
                    stopped = true;
                    break 'iterations;
                }
                Err(e) => return Err(e),
            };
            solver_iterations += solved.iterations;
            let case = element_energies(dims, &brick, displacement, &stiffness, threads);
            for (e, c) in energies.iter_mut().zip(&case) {
                *e += c;
            }
            // The work of the loads on the displacements solved.
            compliance += loads
                .iter()
                .zip(displacement.iter())
                .map(|(f, u)| f * u)
                .sum::<f64>();
        }
        let mass_now: f64 = rho.iter().sum::<f64>() * voxel_mass;

        // The slopes of the compliance and the mass for the projected densities, carried
        // back through the projection, the cap and the filter to the design groups.
        let back = |per_voxel: &dyn Fn(usize) -> f64| -> Vec<f64> {
            let mut s: Vec<f64> = (0..voxels)
                .map(|v| {
                    if state[v] == State::Free {
                        per_voxel(v) * project_slope(capped[v], beta)
                    } else {
                        0.0
                    }
                })
                .collect();
            if !source.is_empty() {
                let mut carried = vec![0.0; voxels];
                for v in 0..voxels {
                    if state[v] == State::Free {
                        carried[source[v] as usize] += s[v];
                    }
                }
                s = carried;
            }
            if let (Some(printing), Some(p)) = (&printing, &printed) {
                s = printing.backward(&filtered, &state, p, &s);
            }
            let per_design = filter.backward(&s, threads);
            let mut out = vec![0.0; groups];
            for v in 0..voxels {
                if state[v] == State::Free {
                    out[group[v] as usize] += per_design[v];
                }
            }
            out
        };
        let dm = back(&|_| voxel_mass);
        // dK/dρ for each free voxel, per unit of uᵀ k u.
        let stiffness_slope =
            |v: usize| PENALTY * rho[v].powf(PENALTY - 1.0) * (1.0 - VOID_STIFFNESS) * modulus * h;
        // kg: what each free voxel's weight gains per unit of projected density.
        let weight_slope = |v: usize| voxel_mass * weight_share(rho[v]).1;
        let next = match limits {
            None => {
                // The compliance's slope: the stiffness's, and where the material weighs,
                // its weight's, twice (it loads and it is what moves).
                let mut per_voxel: Vec<f64> = (0..voxels)
                    .map(|v| -stiffness_slope(v) * energies[v])
                    .collect();
                for (case, u) in cases.iter().zip(&displacements) {
                    if case.acceleration == [0.0; 3] {
                        continue;
                    }
                    let work = corner_work(dims, u, case.acceleration);
                    for v in 0..voxels {
                        if state[v] == State::Free {
                            per_voxel[v] += 2.0 * weight_slope(v) * work[v];
                        }
                    }
                }
                let dc = back(&|v| per_voxel[v]);
                if weighted {
                    // Weight turns the compliance's slope positive where material loads
                    // more than it stiffens, and optimality criteria assume it never does:
                    // the method of moving asymptotes moves the design, the compliance
                    // scaled to the first one its objective, the mass target its limit
                    // (the stiffest design may weigh less than it).
                    let reference =
                        *first_compliance.get_or_insert(compliance.max(f64::MIN_POSITIVE));
                    let df0: Vec<f64> = dc.iter().map(|d| d / reference).collect();
                    let value = mass_now / target - 1.0;
                    feasible = value <= LIMIT_TOLERANCE;
                    let slope: Vec<f64> = dm.iter().map(|m| m / target).collect();
                    let limit = if beta >= 8.0 { LATE_MMA_MOVE } else { MMA_MOVE };
                    mma.update(&x, DESIGN_MIN, 1.0, limit, &df0, &[value], &[slope])
                } else {
                    let budget =
                        target - mass_now + dm.iter().zip(&x).map(|(m, x)| m * x).sum::<f64>();
                    let limit = if beta >= 8.0 {
                        LATE_MOVE_LIMIT
                    } else {
                        MOVE_LIMIT
                    };
                    optimality_update(&x, &dc, &dm, budget, limit)
                }
            }
            Some((allowed_stress, allowed_displacement)) => {
                let element = ElementContext {
                    dims,
                    brick: &brick,
                    rho: &rho,
                    state: &state,
                    anchored: &grid.anchored,
                    modulus,
                    h,
                };
                let mut values = Vec::new();
                let mut slopes = Vec::new();
                // The stress, gathered over every case.
                let stress = element.stress_norm(&displacements);
                if stress.norm > 0.0 {
                    let scale = stress.largest / stress.norm;
                    stress_scale = NORMALIZATION_DAMPING * scale
                        + (1.0 - NORMALIZATION_DAMPING) * stress_scale;
                }
                values.push(stress_scale * stress.norm / allowed_stress - 1.0);
                let mut per_voxel = element.stress_slopes(&stress);
                // The displacement, likewise, when it is limited.
                let displacement =
                    allowed_displacement.map(|_| element.displacement_norm(&displacements));
                if let Some(d) = &displacement
                    && d.norm > 0.0
                {
                    let scale = d.largest / d.norm;
                    displacement_scale = NORMALIZATION_DAMPING * scale
                        + (1.0 - NORMALIZATION_DAMPING) * displacement_scale;
                }
                let mut displacement_voxel = displacement
                    .as_ref()
                    .map(|d| element.displacement_slopes(d, &displacements))
                    .unwrap_or_default();
                // The adjoint part: one solve per case and measure.
                for (c, (case, u)) in cases.iter().zip(&displacements).enumerate() {
                    let fine = Fine::new(
                        dims,
                        &brick,
                        &stiffness,
                        &case.held,
                        &case.projectors,
                        SCALAR_BELOW * modulus * h,
                    );
                    let mut measures: Vec<(usize, Vec<f64>)> =
                        vec![(0, element.stress_rhs(&stress, c, u))];
                    if let Some(d) = &displacement {
                        measures.push((1, element.displacement_rhs(d, u)));
                    }
                    for (k, rhs) in measures {
                        let lambda = &mut adjoints[k][c];
                        let solved = solve_system(
                            &fine,
                            &rhs,
                            lambda,
                            tolerance,
                            backend.as_deref_mut(),
                            cancel,
                            threads,
                        );
                        match solved {
                            Ok(s) => solver_iterations += s.iterations,
                            Err(GenerativeError::Cancelled) if !history.is_empty() => {
                                stopped = true;
                                break 'iterations;
                            }
                            Err(e) => return Err(e),
                        }
                        let products =
                            element_products(dims, &brick, lambda, u, &stiffness, threads);
                        // Where the material weighs, its weight's slope against λ too.
                        let work = (case.acceleration != [0.0; 3])
                            .then(|| corner_work(dims, lambda, case.acceleration));
                        let target = if k == 0 {
                            &mut per_voxel
                        } else {
                            &mut displacement_voxel
                        };
                        for v in 0..voxels {
                            if state[v] == State::Free {
                                target[v] -= products[v] * stiffness_slope(v);
                                if let Some(w) = &work {
                                    target[v] += weight_slope(v) * w[v];
                                }
                            }
                        }
                    }
                }
                let stress_factor = stress_scale / allowed_stress;
                slopes.push(back(&|v| stress_factor * per_voxel[v]));
                if let (Some(d), Some(allowed)) = (&displacement, allowed_displacement) {
                    values.push(displacement_scale * d.norm / allowed - 1.0);
                    let factor = displacement_scale / allowed;
                    slopes.push(back(&|v| factor * displacement_voxel[v]));
                }
                feasible = values.iter().all(|g| *g <= LIMIT_TOLERANCE);
                // Mass as a share of the whole design space's.
                let df0: Vec<f64> = dm.iter().map(|m| m / max).collect();
                let limit = if beta >= 8.0 { LATE_MMA_MOVE } else { MMA_MOVE };
                mma.update(&x, DESIGN_MIN, 1.0, limit, &df0, &values, &slopes)
            }
        };
        let change = x
            .iter()
            .zip(&next)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        history.push(GenerativeStep {
            compliance,
            mass: mass_now,
            change,
        });
        if (iteration + 1) % SNAPSHOT_EVERY == 0 {
            snapshots.push((iteration + 1, bytes(&capped, &state)));
        }
        solved_rho = rho;
        solved_field = capped;
        x = next;
        at_beta += 1;
        progress((iteration + 1) as f64 / iterations as f64);
        let steady = beta >= BETA_MAX && at_beta >= STEADY_OVER && {
            let recent = &history[history.len() - STEADY_OVER..];
            match limits {
                None => {
                    let (lo, hi) = recent.iter().fold((f64::INFINITY, 0.0f64), |(lo, hi), s| {
                        (lo.min(s.compliance), hi.max(s.compliance))
                    });
                    // Weighted, the stiffest design may weigh less than the target.
                    let on_target = if weighted {
                        mass_now <= (1.0 + STEADY_MASS) * target
                    } else {
                        (mass_now - target).abs() <= STEADY_MASS * target
                    };
                    hi - lo <= STEADY_COMPLIANCE * hi && on_target
                }
                // The mass steady, the limits met.
                Some(_) => {
                    let (lo, hi) = recent.iter().fold((f64::INFINITY, 0.0f64), |(lo, hi), s| {
                        (lo.min(s.mass), hi.max(s.mass))
                    });
                    feasible && hi - lo <= STEADY_MASS * hi
                }
            }
        };
        let last_stage_done = beta >= BETA_MAX && at_beta >= BETA_STEP_AFTER;
        let crisp = gray <= GRAYNESS_SETTLED;
        let settled = (beta >= BETA_MAX && change < SETTLED_CHANGE && feasible) || steady;
        if (settled && crisp) || last_stage_done {
            converged = true;
            break;
        }
        if beta < BETA_MAX && (change < SETTLED_CHANGE || at_beta >= BETA_STEP_AFTER) {
            beta *= 2.0;
            at_beta = 0;
        }
    }

    // What the last design solved weighs, how far it moves and how hard it is worked.
    let material_voxel =
        |v: usize| state[v] == State::Solid || (state[v] == State::Free && solved_rho[v] >= 0.5);
    let mass = solved_rho.iter().sum::<f64>() * voxel_mass;
    let mut max_displacement = 0.0f64;
    let mut max_von_mises = 0.0f64;
    let offsets = dims.corner_offsets();
    for (v, displacement) in (0..voxels)
        .filter(|&v| material_voxel(v))
        .flat_map(|v| displacements.iter().map(move |d| (v, d)))
    {
        let c = grid.coords(v);
        let base = dims.node(c);
        let mut u = [0.0; DOFS];
        for (a, off) in offsets.iter().enumerate() {
            let n = base + off;
            u[3 * a..3 * a + 3].copy_from_slice(&displacement[3 * n..3 * n + 3]);
            let d = (u[3 * a].powi(2) + u[3 * a + 1].powi(2) + u[3 * a + 2].powi(2)).sqrt();
            max_displacement = max_displacement.max(d);
        }
        if grid.anchored.binary_search(&(v as u32)).is_ok() {
            continue;
        }
        let strain: [f64; 6] = std::array::from_fn(|r| {
            (0..DOFS).map(|k| brick.b[r * DOFS + k] * u[k]).sum::<f64>() / h
        });
        let stress: [f64; 6] = std::array::from_fn(|r| {
            modulus * (0..6).map(|k| brick.d[r * 6 + k] * strain[k]).sum::<f64>()
        });
        max_von_mises = max_von_mises.max(von_mises(&stress));
    }
    let min_factor_of_safety = (material.yield_strength > 0.0 && max_von_mises > 0.0)
        .then(|| material.yield_strength / max_von_mises);
    progress(1.0);
    Ok(Outcome {
        density: bytes(&solved_field, &state),
        snapshots,
        history,
        converged,
        stopped,
        mass,
        compliance,
        max_displacement,
        max_von_mises,
        min_factor_of_safety,
        solver_iterations,
        filter_radius: radius,
        grayness: grayness(&solved_rho, &state),
    })
}

/// `fine u = rhs` solved from `x`, on `backend` when there is one.
fn solve_system(
    fine: &Fine<'_>,
    rhs: &[f64],
    x: &mut [f64],
    tolerance: f64,
    backend: Option<&mut (dyn Backend + '_)>,
    cancel: &AtomicBool,
    threads: usize,
) -> Result<Solved, GenerativeError> {
    match backend {
        None => multigrid::solve(fine, rhs, x, tolerance, SOLVE_ITERATIONS, cancel, threads),
        Some(backend) => {
            fine.project(x);
            let mut kept = rhs.to_vec();
            fine.project(&mut kept);
            let system = System {
                fine,
                loads: kept,
                threads,
            };
            backend.solve(&system, x, tolerance, SOLVE_ITERATIONS, cancel)
        }
    }
}

/// A p-norm over voxels or nodes and every load case: its value, the largest value the
/// outcome reports (the stress in its material, the displacement at its corners), which it
/// is scaled to, and what each term is, per case.
struct Gathered {
    norm: f64,
    largest: f64,
    /// Per case, per voxel (stress) or node (displacement): the value measured.
    values: Vec<Vec<f64>>,
}

/// What the stress and displacement measures need of the design.
struct ElementContext<'a> {
    dims: Dims,
    brick: &'a Brick,
    rho: &'a [f64],
    state: &'a [State],
    /// The voxels fixtures and loads act through, where stress is singular: left out.
    anchored: &'a [u32],
    modulus: f64,
    h: f64,
}

impl ElementContext<'_> {
    /// Whether voxel `v`'s stress counts: material at all, and no fixture or load through
    /// it.
    fn counts(&self, v: usize) -> bool {
        self.state[v] != State::Void
            && self.rho[v] > 0.0
            && self.anchored.binary_search(&(v as u32)).is_err()
    }

    /// The displacements of voxel `v`'s corners in `u`.
    fn corners(&self, v: usize, u: &[f64]) -> [f64; DOFS] {
        let base = self.dims.node(self.dims.cell_coords(v));
        let mut out = [0.0; DOFS];
        for (a, off) in self.dims.corner_offsets().iter().enumerate() {
            let n = base + off;
            out[3 * a..3 * a + 3].copy_from_slice(&u[3 * n..3 * n + 3]);
        }
        out
    }

    /// The solid material's stress at the centre of voxel `v`, MPa.
    fn stress(&self, v: usize, u: &[f64]) -> [f64; 6] {
        let ue = self.corners(v, u);
        let b = &self.brick.b;
        let strain: [f64; 6] = std::array::from_fn(|r| {
            (0..DOFS).map(|k| b[r * DOFS + k] * ue[k]).sum::<f64>() / self.h
        });
        std::array::from_fn(|r| {
            self.modulus
                * (0..6)
                    .map(|k| self.brick.d[r * 6 + k] * strain[k])
                    .sum::<f64>()
        })
    }

    /// The relaxed stresses `ρ^q σ_vm` gathered by their p-norm over every case.
    fn stress_norm(&self, displacements: &[Vec<f64>]) -> Gathered {
        let voxels = self.rho.len();
        let mut sum = 0.0;
        let mut largest = 0.0f64;
        let mut values = Vec::with_capacity(displacements.len());
        for u in displacements {
            let mut vm = vec![0.0; voxels];
            for (v, out) in vm.iter_mut().enumerate() {
                if !self.counts(v) {
                    continue;
                }
                *out = von_mises(&self.stress(v, u));
                let relaxed = self.rho[v].powf(STRESS_RELAXATION) * *out;
                sum += relaxed.powi(NORM_POWER);
                // Scaled to the largest stress the outcome reports: in its material.
                if self.rho[v] >= 0.5 {
                    largest = largest.max(*out);
                }
            }
            values.push(vm);
        }
        Gathered {
            norm: sum.powf(1.0 / f64::from(NORM_POWER)),
            largest,
            values,
        }
    }

    /// The p-norm's explicit slopes for each voxel's density.
    fn stress_slopes(&self, g: &Gathered) -> Vec<f64> {
        let p = f64::from(NORM_POWER);
        let voxels = self.rho.len();
        let mut out = vec![0.0; voxels];
        if g.norm <= 0.0 {
            return out;
        }
        let outer = g.norm.powf(1.0 - p);
        for (v, o) in out.iter_mut().enumerate() {
            if self.state[v] != State::Free || !self.counts(v) {
                continue;
            }
            let vm_p: f64 = g.values.iter().map(|vm| vm[v].powi(NORM_POWER)).sum();
            *o = outer * STRESS_RELAXATION * self.rho[v].powf(STRESS_RELAXATION * p - 1.0) * vm_p;
        }
        out
    }

    /// The p-norm's slopes for case `case`'s displacements: the adjoint's right-hand side.
    fn stress_rhs(&self, g: &Gathered, case: usize, u: &[f64]) -> Vec<f64> {
        let p = f64::from(NORM_POWER);
        let mut rhs = vec![0.0; u.len()];
        let Some(vm) = g.values.get(case) else {
            return rhs;
        };
        if g.norm <= 0.0 {
            return rhs;
        }
        let outer = g.norm.powf(1.0 - p);
        let b = &self.brick.b;
        let base_offsets = self.dims.corner_offsets();
        for (v, &vm_v) in vm.iter().enumerate() {
            if vm_v <= 0.0 || !self.counts(v) {
                continue;
            }
            let rho_q = self.rho[v].powf(STRESS_RELAXATION);
            let weight = outer * (rho_q * vm_v).powi(NORM_POWER - 1) * rho_q;
            let sigma = self.stress(v, u);
            // d σ_vm / d σ, then through D and B to the corners.
            let dvm: [f64; 6] = [
                (2.0 * sigma[0] - sigma[1] - sigma[2]) / (2.0 * vm_v),
                (2.0 * sigma[1] - sigma[0] - sigma[2]) / (2.0 * vm_v),
                (2.0 * sigma[2] - sigma[0] - sigma[1]) / (2.0 * vm_v),
                3.0 * sigma[3] / vm_v,
                3.0 * sigma[4] / vm_v,
                3.0 * sigma[5] / vm_v,
            ];
            let dstrain: [f64; 6] = std::array::from_fn(|k| {
                self.modulus
                    * (0..6)
                        .map(|r| self.brick.d[r * 6 + k] * dvm[r])
                        .sum::<f64>()
            });
            let base = self.dims.node(self.dims.cell_coords(v));
            for (a, off) in base_offsets.iter().enumerate() {
                let n = base + off;
                for x in 0..3 {
                    let k = 3 * a + x;
                    let du: f64 =
                        (0..6).map(|r| b[r * DOFS + k] * dstrain[r]).sum::<f64>() / self.h;
                    rhs[3 * n + x] += weight * du;
                }
            }
        }
        rhs
    }

    /// Each node's weight: the mean density of the eight voxels around it.
    fn node_weights(&self) -> Vec<f64> {
        let mut w = vec![0.0; self.dims.node_count()];
        let offsets = self.dims.corner_offsets();
        for (v, r) in self.rho.iter().enumerate() {
            if *r <= 0.0 {
                continue;
            }
            let base = self.dims.node(self.dims.cell_coords(v));
            for off in &offsets {
                w[base + off] += r / 8.0;
            }
        }
        w
    }

    /// The displacements, weighted by the material around each node, gathered by their
    /// p-norm over every case.
    fn displacement_norm(&self, displacements: &[Vec<f64>]) -> Gathered {
        let p = f64::from(NORM_POWER);
        let weights = self.node_weights();
        let mut sum = 0.0;
        let mut largest = 0.0f64;
        for u in displacements {
            for (n, w) in weights.iter().enumerate() {
                if *w <= 0.0 {
                    continue;
                }
                let d = (u[3 * n].powi(2) + u[3 * n + 1].powi(2) + u[3 * n + 2].powi(2)).sqrt();
                sum += w * d.powi(NORM_POWER);
                // Scaled to the largest displacement the outcome reports: at its material's
                // corners (half a voxel of material around the node, or more).
                if *w >= 1.0 / 16.0 {
                    largest = largest.max(d);
                }
            }
        }
        Gathered {
            norm: sum.powf(1.0 / p),
            largest,
            values: vec![weights],
        }
    }

    /// Its explicit slopes for each voxel's density, through the weights of its corners.
    fn displacement_slopes(&self, g: &Gathered, displacements: &[Vec<f64>]) -> Vec<f64> {
        let p = f64::from(NORM_POWER);
        let mut out = vec![0.0; self.rho.len()];
        if g.norm <= 0.0 {
            return out;
        }
        let outer = g.norm.powf(1.0 - p) / p;
        let offsets = self.dims.corner_offsets();
        for (v, o) in out.iter_mut().enumerate() {
            if self.state[v] != State::Free {
                continue;
            }
            let base = self.dims.node(self.dims.cell_coords(v));
            let mut sum = 0.0;
            for u in displacements {
                for off in &offsets {
                    let n = base + off;
                    let d2 = u[3 * n].powi(2) + u[3 * n + 1].powi(2) + u[3 * n + 2].powi(2);
                    sum += d2.powf(0.5 * p);
                }
            }
            *o = outer * sum / 8.0;
        }
        out
    }

    /// The displacement p-norm's slopes for one case's displacements.
    fn displacement_rhs(&self, g: &Gathered, u: &[f64]) -> Vec<f64> {
        let p = f64::from(NORM_POWER);
        let mut rhs = vec![0.0; u.len()];
        let Some(weights) = g.values.first() else {
            return rhs;
        };
        if g.norm <= 0.0 {
            return rhs;
        }
        let outer = g.norm.powf(1.0 - p);
        for (n, w) in weights.iter().enumerate() {
            if *w <= 0.0 {
                continue;
            }
            let d2 = u[3 * n].powi(2) + u[3 * n + 1].powi(2) + u[3 * n + 2].powi(2);
            let factor = outer * w * d2.powf(0.5 * (p - 2.0));
            for x in 0..3 {
                rhs[3 * n + x] = factor * u[3 * n + x];
            }
        }
        rhs
    }
}

/// `aₑᵀ K bₑ` of each voxel with an element, for a unit modulus and edge (the brick's own).
fn element_products(
    dims: Dims,
    brick: &Brick,
    a: &[f64],
    b: &[f64],
    stiffness: &[f64],
    threads: usize,
) -> Vec<f64> {
    let offsets = dims.corner_offsets();
    let mut out = vec![0.0; dims.cell_count()];
    par_fill(&mut out, 1, threads, 2048, |first, part| {
        let mut ae = [0.0; DOFS];
        let mut be = [0.0; DOFS];
        for (m, o) in part.iter_mut().enumerate() {
            let e = first + m;
            if stiffness[e] <= 0.0 {
                continue;
            }
            let base = dims.node(dims.cell_coords(e));
            for (k, off) in offsets.iter().enumerate() {
                let n = base + off;
                ae[3 * k..3 * k + 3].copy_from_slice(&a[3 * n..3 * n + 3]);
                be[3 * k..3 * k + 3].copy_from_slice(&b[3 * n..3 * n + 3]);
            }
            let mut kb = [0.0; DOFS];
            for (row, &bj) in brick.k.as_chunks::<DOFS>().0.iter().zip(&be) {
                for (o, kv) in kb.iter_mut().zip(row) {
                    *o += kv * bj;
                }
            }
            *o = kb.iter().zip(&ae).map(|(x, y)| x * y).sum();
        }
    });
    out
}

/// A load case as each iteration solves it.
struct CaseSystem {
    /// Per node: the index of its projector, or `NONE`.
    held: Vec<u32>,
    projectors: Vec<[f64; 9]>,
    /// N, three per node: the forces, the pressures and the remote masses' weight.
    loads: Vec<f64>,
    /// m/s²: what weighs the material too; zero for none.
    acceleration: V3,
}

/// A voxel's share of its solid weight at projected density `r`, and its slope: the
/// density above [`WEIGHT_KNEE`], its cube scaled to meet it below.
fn weight_share(r: f64) -> (f64, f64) {
    if r >= WEIGHT_KNEE {
        (r, 1.0)
    } else {
        let c = 1.0 / (WEIGHT_KNEE * WEIGHT_KNEE);
        (c * r * r * r, 3.0 * c * r * r)
    }
}

/// Adds to `forces` (N, three per node) the weight of voxels of `masses` (kg) under
/// `acceleration` (m/s²): an eighth of each at each of its corners, voxel by voxel in
/// order.
fn add_weight(dims: Dims, masses: &[f64], acceleration: V3, forces: &mut [f64]) {
    let offsets = dims.corner_offsets();
    for (v, m) in masses.iter().enumerate() {
        if *m == 0.0 {
            continue;
        }
        let base = dims.node(dims.cell_coords(v));
        for off in &offsets {
            let n = base + off;
            for (x, a) in acceleration.iter().enumerate() {
                forces[3 * n + x] += m * a / 8.0;
            }
        }
    }
}

/// Per voxel: `field` at its corners against `acceleration`, averaged, `Σ aᵀ f / 8`, what a
/// kilogram more of it does to `fieldᵀ f` through its weight.
fn corner_work(dims: Dims, field: &[f64], acceleration: V3) -> Vec<f64> {
    let offsets = dims.corner_offsets();
    (0..dims.cell_count())
        .map(|v| {
            let base = dims.node(dims.cell_coords(v));
            offsets
                .iter()
                .map(|off| {
                    let n = base + off;
                    (0..3)
                        .map(|x| acceleration[x] * field[3 * n + x])
                        .sum::<f64>()
                })
                .sum::<f64>()
                / 8.0
        })
        .collect()
}

/// Densities as the files keep them: 0 to 255.
fn bytes(rho: &[f64], state: &[State]) -> Vec<u8> {
    rho.iter()
        .zip(state)
        .map(|(r, s)| match s {
            State::Solid => 255,
            State::Void => 0,
            State::Free => (r.clamp(0.0, 1.0) * 255.0).round() as u8,
        })
        .collect()
}

/// Sigmund's measure of non-discreteness of the projected densities `rho`: `4 ρ (1 − ρ)`
/// averaged over the free voxels, 0 when each is solid or void, 1 when all are half; 0
/// without a free voxel.
fn grayness(rho: &[f64], state: &[State]) -> f64 {
    let (sum, count) = rho
        .iter()
        .zip(state)
        .filter(|(_, s)| **s == State::Free)
        .fold((0.0, 0usize), |(sum, n), (r, _)| {
            let r = r.clamp(0.0, 1.0);
            (sum + 4.0 * r * (1.0 - r), n + 1)
        });
    if count == 0 { 0.0 } else { sum / count as f64 }
}

/// The smoothed Heaviside projection at sharpness `beta`, and its slope.
fn project(x: f64, beta: f64) -> f64 {
    let t = (beta * ETA).tanh();
    (t + (beta * (x - ETA)).tanh()) / (t + (beta * (1.0 - ETA)).tanh())
}

fn project_slope(x: f64, beta: f64) -> f64 {
    let t = (beta * ETA).tanh();
    let th = (beta * (x - ETA)).tanh();
    beta * (1.0 - th * th) / (t + (beta * (1.0 - ETA)).tanh())
}

/// The next design by optimality criteria: each group's density times the square root of
/// its compliance slope over `λ` times its mass slope, within the move limit and
/// `DESIGN_MIN` to 1, `λ` found by bisection (on its logarithm) so that the mass,
/// linearised by `dm`, is `budget`.
fn optimality_update(x: &[f64], dc: &[f64], dm: &[f64], budget: f64, limit: f64) -> Vec<f64> {
    let largest_dm = dm.iter().copied().fold(0.0, f64::max);
    let ratio: Vec<Option<f64>> = dc
        .iter()
        .zip(dm)
        .map(|(c, m)| (*m > 1e-12 * largest_dm).then(|| (-c).max(0.0) / m))
        .collect();
    let largest = ratio.iter().flatten().copied().fold(0.0, f64::max);
    if !(largest > 0.0 && largest.is_finite()) {
        return x.to_vec();
    }
    let at = |lambda: f64| -> Vec<f64> {
        x.iter()
            .zip(&ratio)
            .map(|(&d, r)| match r {
                None => d,
                Some(r) => {
                    let lo = (d - limit).max(DESIGN_MIN);
                    let hi = (d + limit).min(1.0);
                    (d * (r / lambda).sqrt()).clamp(lo, hi)
                }
            })
            .collect()
    };
    let mass_of = |next: &[f64]| -> f64 { next.iter().zip(dm).map(|(x, m)| x * m).sum() };
    let (mut lo, mut hi) = ((largest * 1e-12).ln(), (largest * 1e12).ln());
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        if mass_of(&at(mid.exp())) > budget {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at((0.5 * (lo + hi)).exp())
}

/// `uₑᵀ K uₑ` of each voxel with an element, for a unit modulus and edge (the brick's own).
fn element_energies(
    dims: Dims,
    brick: &Brick,
    u: &[f64],
    stiffness: &[f64],
    threads: usize,
) -> Vec<f64> {
    let offsets = dims.corner_offsets();
    let mut out = vec![0.0; dims.cell_count()];
    par_fill(&mut out, 1, threads, 2048, |first, part| {
        let mut ue = [0.0; DOFS];
        for (m, o) in part.iter_mut().enumerate() {
            let e = first + m;
            if stiffness[e] <= 0.0 {
                continue;
            }
            let base = dims.node(dims.cell_coords(e));
            for (a, off) in offsets.iter().enumerate() {
                let n = base + off;
                ue[3 * a..3 * a + 3].copy_from_slice(&u[3 * n..3 * n + 3]);
            }
            // K u by columns (K is symmetric), then u · K u.
            let mut ku = [0.0; DOFS];
            for (row, &uj) in brick.k.as_chunks::<DOFS>().0.iter().zip(&ue) {
                for (o, kv) in ku.iter_mut().zip(row) {
                    *o += kv * uj;
                }
            }
            *o = ku.iter().zip(&ue).map(|(a, b)| a * b).sum();
        }
    });
    out
}

/// The symmetry planes the grid was laid out for: those of the study whose plane is the
/// grid's middle along its axis.
fn grid_symmetry(grid: &Grid, study: &GenerativeStudy) -> [bool; 3] {
    let Some(s) = study.symmetry else {
        return [false; 3];
    };
    std::array::from_fn(|a| {
        let middle = grid.origin[a] + 0.5 * grid.counts[a] as f64 * grid.size;
        s.planes[a]
            && grid.counts[a].is_multiple_of(2)
            && (middle - s.point[a]).abs() < 1e-6 * grid.size.max(1.0)
    })
}

/// Voxel `v`'s mirror images across the planes (itself included).
fn mirror_images(grid: &Grid, v: usize, planes: [bool; 3]) -> Vec<usize> {
    let c = grid.coords(v);
    let mut out = vec![c];
    for a in 0..3 {
        if planes[a] {
            let more: Vec<[usize; 3]> = out
                .iter()
                .map(|p| {
                    let mut m = *p;
                    m[a] = grid.counts[a] - 1 - p[a];
                    m
                })
                .collect();
            out.extend(more);
        }
    }
    let mut out: Vec<usize> = out.into_iter().map(|c| grid.index(c)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The voxels of each line of the grid along `axis`, in order.
fn lines(grid: &Grid, axis: usize) -> impl Iterator<Item = Vec<usize>> + '_ {
    let n = grid.counts;
    let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
    (0..n[b]).flat_map(move |j| {
        (0..n[a]).map(move |i| {
            (0..n[axis])
                .map(|k| {
                    let mut c = [0; 3];
                    c[axis] = k;
                    c[a] = i;
                    c[b] = j;
                    grid.index(c)
                })
                .collect()
        })
    })
}

/// The chains a casting direction caps along: each from the back to the front.
fn casting_chains(grid: &Grid, direction: Direction, parting: Option<V3>) -> Vec<Vec<usize>> {
    let axis = direction.axis.min(2);
    let mut out = Vec::new();
    for line in lines(grid, axis) {
        match parting {
            None => {
                let mut line = line;
                if !direction.positive {
                    line.reverse();
                }
                out.push(line);
            }
            Some(p) => {
                // On each side, from the parting plane outwards.
                let (front, back): (Vec<usize>, Vec<usize>) = line
                    .into_iter()
                    .partition(|&v| grid.centre(v)[axis] >= p[axis]);
                let mut back = back;
                back.reverse();
                for chain in [front, back] {
                    if !chain.is_empty() {
                        out.push(chain);
                    }
                }
            }
        }
    }
    out
}

/// What each voxel is to the optimization, once symmetry and the method have made the
/// design space one they can give.
fn states(
    grid: &Grid,
    method: Method,
    symmetry: Option<&Symmetry>,
    symmetric: [bool; 3],
) -> Result<Vec<State>, GenerativeError> {
    let mut state: Vec<State> = grid
        .kinds
        .iter()
        .map(|k| match k {
            Voxel::Design => State::Free,
            Voxel::Preserve => State::Solid,
            Voxel::Obstacle | Voxel::Outside => State::Void,
        })
        .collect();
    if let (Some((direction, parting)), Some(s)) = (method.pull(), symmetry) {
        // A pull along a plane keeps the mirror; across one only from it both ways.
        let a = direction.axis.min(2);
        if s.planes[a] && !parting.is_some_and(|p| (p[a] - s.point[a]).abs() < 1e-6) {
            return Err(GenerativeError::PullAcrossSymmetry);
        }
    }
    if let (Method::Additive { direction }, Some(s)) = (method, symmetry)
        && s.planes[direction.axis.min(2)]
    {
        return Err(GenerativeError::BuildAcrossSymmetry);
    }
    if let (Some(sides), Some(s)) = (method.several_sides(), symmetry) {
        // Milled from both ends of an axis a plane crosses, the mirror holds; from one end
        // only, it cannot.
        for a in 0..3 {
            let plus = sides.contains(Direction {
                axis: a,
                positive: true,
            });
            let minus = sides.contains(Direction {
                axis: a,
                positive: false,
            });
            if s.planes[a] && plus != minus {
                return Err(GenerativeError::PullAcrossSymmetry);
            }
        }
    }
    if symmetric.contains(&true) {
        for v in 0..state.len() {
            let images = mirror_images(grid, v, symmetric);
            if images.first() != Some(&v) {
                continue;
            }
            let forced = if images.iter().any(|&w| state[w] == State::Void) {
                Some(State::Void)
            } else if images.iter().any(|&w| state[w] == State::Solid) {
                Some(State::Solid)
            } else {
                None
            };
            if let Some(f) = forced {
                for w in images {
                    if state[w] == State::Free {
                        state[w] = f;
                    }
                }
            }
        }
    }
    let not_makeable = |v: usize| GenerativeError::NotMakeable {
        method,
        at: grid.centre(v),
    };
    match method {
        // Printing makes any design space: what nothing holds up is printed empty.
        Method::Unrestricted | Method::Additive { .. } => {}
        Method::Milling { sides, .. } if sides.len() > 1 => {
            reachable(grid, sides, &mut state).map_err(not_makeable)?
        }
        Method::Milling { sides, .. } => {
            if let Some(&direction) = sides.directions().first() {
                one_way(grid, direction, None, &mut state).map_err(not_makeable)?
            }
        }
        Method::Cutting { axis } => {
            for line in lines(grid, axis.min(2)) {
                let void = line.iter().find(|&&v| state[v] == State::Void);
                let solid = line.iter().find(|&&v| state[v] == State::Solid);
                let forced = match (void, solid) {
                    (Some(_), Some(&s)) => return Err(not_makeable(s)),
                    (Some(_), None) => State::Void,
                    (None, Some(_)) => State::Solid,
                    (None, None) => continue,
                };
                for v in line {
                    if state[v] == State::Free {
                        state[v] = forced;
                    }
                }
            }
        }
        Method::Casting { direction, parting } => {
            one_way(grid, direction, parting, &mut state).map_err(not_makeable)?;
        }
    }
    Ok(state)
}

/// The states a one-way pull along `direction` (both ways from `parting`) leaves: in front
/// of a void, void; behind a solid, solid. A solid in front of a void is an undercut: that
/// voxel is the error.
fn one_way(
    grid: &Grid,
    direction: Direction,
    parting: Option<V3>,
    state: &mut [State],
) -> Result<(), usize> {
    for chain in casting_chains(grid, direction, parting) {
        let mut void = false;
        for &v in &chain {
            match state[v] {
                State::Void => void = true,
                State::Solid if void => return Err(v),
                State::Free if void => state[v] = State::Void,
                _ => {}
            }
        }
        let mut solid = false;
        for &v in chain.iter().rev() {
            match state[v] {
                State::Solid => solid = true,
                State::Free if solid => state[v] = State::Solid,
                _ => {}
            }
        }
    }
    Ok(())
}

/// The states milling from several sides at once leaves: a voxel behind a preserve voxel
/// from every side the tool comes from is solid (no side reaches it); a void voxel (an
/// obstacle, outside the starting shape) is reached from the side with the fewest design
/// voxels in front of it, those made void. A void voxel behind a preserve voxel from every
/// side is an undercut: that voxel is the error.
fn reachable(grid: &Grid, sides: Sides, state: &mut [State]) -> Result<(), usize> {
    let n = state.len();
    let directions = sides.directions();
    let chains: Vec<Vec<Vec<usize>>> = directions
        .iter()
        .map(|&d| casting_chains(grid, d, None))
        .collect();
    // Per side: whether a preserve voxel is in front of each voxel (towards the tool), and
    // how many design voxels are.
    let mut blocked = vec![vec![false; n]; chains.len()];
    let mut cost = vec![vec![0u32; n]; chains.len()];
    for (k, side) in chains.iter().enumerate() {
        for chain in side {
            let (mut solid, mut free) = (false, 0u32);
            for &v in chain.iter().rev() {
                blocked[k][v] = solid;
                cost[k][v] = free;
                match state[v] {
                    State::Solid => solid = true,
                    State::Free => free += 1,
                    State::Void => {}
                }
            }
        }
    }
    // Each void voxel's side: the cheapest unblocked one, the first of equals.
    let mut chosen = vec![usize::MAX; n];
    for v in 0..n {
        if state[v] != State::Void {
            continue;
        }
        let best = (0..chains.len())
            .filter(|&k| !blocked[k][v])
            .min_by_key(|&k| (cost[k][v], k));
        match best {
            Some(k) => chosen[v] = k,
            None => return Err(v),
        }
    }
    let solid: Vec<usize> = (0..n)
        .filter(|&v| state[v] == State::Free && (0..chains.len()).all(|k| blocked[k][v]))
        .collect();
    // In front of a void voxel along its side, from the back to the front: void.
    for (k, side) in chains.iter().enumerate() {
        for chain in side {
            let mut clearing = false;
            for &v in chain {
                if state[v] == State::Void && chosen[v] == k {
                    clearing = true;
                } else if clearing && state[v] == State::Free {
                    state[v] = State::Void;
                }
            }
        }
    }
    for v in solid {
        if state[v] == State::Free {
            state[v] = State::Solid;
        }
    }
    Ok(())
}

/// Each free voxel's design group (mirror images, and with 2-axis cutting whole columns,
/// share one), `NONE` for the others, numbered in voxel order; and how many groups.
fn groups(grid: &Grid, method: Method, state: &[State], symmetric: [bool; 3]) -> (Vec<u32>, usize) {
    let n = state.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(parent: &mut [u32], mut v: u32) -> u32 {
        while parent[v as usize] != v {
            let up = parent[parent[v as usize] as usize];
            parent[v as usize] = up;
            v = up;
        }
        v
    }
    fn union(parent: &mut [u32], a: usize, b: usize) {
        let (ra, rb) = (find(parent, a as u32), find(parent, b as u32));
        if ra != rb {
            let (lo, hi) = (ra.min(rb), ra.max(rb));
            parent[hi as usize] = lo;
        }
    }
    if symmetric.contains(&true) {
        for v in 0..n {
            if state[v] != State::Free {
                continue;
            }
            for w in mirror_images(grid, v, symmetric) {
                if w > v && state[w] == State::Free {
                    union(&mut parent, v, w);
                }
            }
        }
    }
    if let Method::Cutting { axis } = method {
        for line in lines(grid, axis.min(2)) {
            let free: Vec<usize> = line
                .into_iter()
                .filter(|&v| state[v] == State::Free)
                .collect();
            for pair in free.windows(2) {
                union(&mut parent, pair[0], pair[1]);
            }
        }
    }
    let mut id = vec![NONE; n];
    let mut count = 0usize;
    let mut out = vec![NONE; n];
    for v in 0..n {
        if state[v] != State::Free {
            continue;
        }
        let root = find(&mut parent, v as u32) as usize;
        if id[root] == NONE {
            id[root] = count as u32;
            count += 1;
        }
        out[v] = id[root];
    }
    (out, count)
}

/// Densities capped along each chain at the one behind them, each with the voxel whose
/// filtered density it took.
fn cap(chains: &[Vec<usize>], state: &[State], filtered: &[f64]) -> (Vec<f64>, Vec<u32>) {
    let mut out = filtered.to_vec();
    let mut source: Vec<u32> = (0..filtered.len() as u32).collect();
    for chain in chains {
        let mut behind: Option<(f64, u32)> = None;
        for &v in chain {
            match state[v] {
                State::Void => {
                    out[v] = 0.0;
                    behind = Some((0.0, v as u32));
                }
                State::Solid => {
                    out[v] = 1.0;
                    behind = Some((1.0, v as u32));
                }
                State::Free => {
                    if let Some((b, s)) = behind
                        && b < filtered[v]
                    {
                        out[v] = b;
                        source[v] = s;
                    }
                    behind = Some((out[v], source[v]));
                }
            }
        }
    }
    (out, source)
}

/// Densities milled from several sides: along each side's `chains` (back to front) from the
/// tool inwards, every voxel raised to the densest one in front of it, as the tool cannot
/// take away what it cannot reach from there; then the least over the sides, as what it
/// reaches from one side is gone. Each with the voxel whose filtered density it took (the
/// first side's of equals, the voxel's own over an equal one in front).
fn reach(sides: &[Vec<Vec<usize>>], state: &[State], filtered: &[f64]) -> (Vec<f64>, Vec<u32>) {
    let n = filtered.len();
    let own = |v: usize| match state[v] {
        State::Solid => 1.0,
        State::Void => 0.0,
        State::Free => filtered[v],
    };
    let mut out = vec![f64::INFINITY; n];
    let mut source: Vec<u32> = (0..n as u32).collect();
    let mut fill = vec![0.0; n];
    let mut from = vec![0u32; n];
    for chains in sides {
        for chain in chains {
            let mut front: Option<(f64, u32)> = None;
            for &v in chain.iter().rev() {
                let mine = (own(v), v as u32);
                let taken = match front {
                    Some(f) if f.0 > mine.0 => f,
                    _ => mine,
                };
                fill[v] = taken.0;
                from[v] = taken.1;
                front = Some(taken);
            }
        }
        for v in 0..n {
            if fill[v] < out[v] {
                out[v] = fill[v];
                source[v] = from[v];
            }
        }
    }
    for v in 0..n {
        match state[v] {
            State::Solid => out[v] = 1.0,
            State::Void => out[v] = 0.0,
            State::Free if !out[v].is_finite() => {
                out[v] = filtered[v];
                source[v] = v as u32;
            }
            State::Free => {}
        }
    }
    (out, source)
}

/// The density filter on the grid: each free voxel's density the design averaged over a
/// sphere of radius `r` (a disc across the axis it is flat along), weights `r − d`, over
/// the voxels that are not void.
struct Filter<'a> {
    grid: &'a Grid,
    state: &'a [State],
    /// Offsets in voxels within the radius, and their weights.
    offsets: Vec<([i64; 3], f64)>,
    norms: Vec<f64>,
}

impl<'a> Filter<'a> {
    fn new(
        grid: &'a Grid,
        state: &'a [State],
        radius: f64,
        flat: Option<usize>,
        threads: usize,
    ) -> Filter<'a> {
        let reach = (radius / grid.size).ceil() as i64;
        let mut offsets = Vec::new();
        for dz in -reach..=reach {
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    let d = grid.size * ((dx * dx + dy * dy + dz * dz) as f64).sqrt();
                    if d < radius && flat.is_none_or(|a| [dx, dy, dz][a] == 0) {
                        offsets.push(([dx, dy, dz], radius - d));
                    }
                }
            }
        }
        let mut filter = Filter {
            grid,
            state,
            offsets,
            norms: Vec::new(),
        };
        let ones = vec![1.0; grid.voxel_count()];
        filter.norms = filter.sum(&ones, threads);
        filter
    }

    /// `Σ w x` over the stencil around each free voxel, the void ones left out (0 for the
    /// voxels that are not free).
    fn sum(&self, x: &[f64], threads: usize) -> Vec<f64> {
        let grid = self.grid;
        let n = grid.counts.map(|c| c as i64);
        let mut out = vec![0.0; grid.voxel_count()];
        par_fill(&mut out, 1, threads, 2048, |first, part| {
            for (m, o) in part.iter_mut().enumerate() {
                let v = first + m;
                if self.state[v] != State::Free {
                    continue;
                }
                let c = grid.coords(v).map(|x| x as i64);
                let mut s = 0.0;
                for (off, w) in &self.offsets {
                    let at = [c[0] + off[0], c[1] + off[1], c[2] + off[2]];
                    if (0..3).any(|a| at[a] < 0 || at[a] >= n[a]) {
                        continue;
                    }
                    let j = (at[0] + n[0] * (at[1] + n[1] * at[2])) as usize;
                    if self.state[j] != State::Void {
                        s += w * x[j];
                    }
                }
                *o = s;
            }
        });
        out
    }

    /// The filtered densities of `design` (1 at solid voxels).
    fn forward(&self, design: &[f64], threads: usize) -> Vec<f64> {
        let sums = self.sum(design, threads);
        sums.iter()
            .zip(&self.norms)
            .zip(self.state)
            .map(|((s, n), st)| {
                if *st == State::Free && *n > 0.0 {
                    s / n
                } else {
                    0.0
                }
            })
            .collect()
    }

    /// The filter's transpose: slopes for the filtered densities carried to the design.
    /// The weights are symmetric, so it is the same sum over the slopes divided by each
    /// voxel's norm.
    fn backward(&self, slopes: &[f64], threads: usize) -> Vec<f64> {
        let scaled: Vec<f64> = slopes
            .iter()
            .zip(&self.norms)
            .zip(self.state)
            .map(|((s, n), st)| {
                if *st == State::Free && *n > 0.0 {
                    s / n
                } else {
                    0.0
                }
            })
            .collect();
        self.sum(&scaled, threads)
    }
}

#[cfg(test)]
mod tests;
