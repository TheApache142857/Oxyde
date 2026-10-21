# Flow simulation: CFD in Oxyde

Status: **built** (October 2026): flow and cooling studies in the Simulation tab, run by
OpenFOAM as a separate program, saved in `.oxysim` and `.oxyres` with their OpenFOAM case
beside them, and OpenFOAM offered by the Windows installer (§4, §7). `oxyde-flow` is tested
against closed forms and correlations through OpenFOAM v2606. Next: flow trajectories,
radiation, transient flows (§8). §1–§3 are the survey the choice was made from.

The reference is SolidWorks Flow Simulation (and Fusion 360's CFD): a liquid or a gas
flowing through the model (*internal*: a valve, a manifold, a duct) or around it
(*external*: a vehicle, a heat sink in a wind tunnel), steady or over time, laminar or
turbulent, with heat carried by the fluid and conducted through the solids (*conjugate
heat transfer*, the electronics-cooling case). The user puts boundary conditions on faces
(an inlet velocity or flow rate, an outlet or environment pressure, walls), heat sources
in solids and *goals* (a pressure drop, a force, a flow rate, a temperature), runs, and
reads cut plots, surface plots, flow trajectories and the goals' values. SolidWorks also
hands the fluid's pressures and temperatures to a static or thermal study as loads.

## 1. What we have, what we lack

- **Labelled surface meshes.** `Kernel::tessellate_for_export` gives each solid as a
  closed triangle mesh, every triangle knowing its face, and the Simulation tab already
  turns persistent face names into labels and back. The conditions and goals of a flow
  study attach to faces the same way.
- **Netgen** (6.2.2401, `Kernel::volume_mesh`) fills solids with tetrahedra. The fluid is
  not a solid of the model: it would first have to be made one (§3.2). Netgen can also
  grow prism layers along walls; not tried in Oxyde.
- **`oxyde-sim`**: tetrahedral meshes, the scalar multigrid of the thermal solver (a
  pressure equation is the same kind of problem), heat conduction in solids; on the
  `topology-optimization` branch, voxel grids made from bodies, geometric multigrid on
  them and `oxyde-sim-gpu` (WGSL compute shaders on the `wgpu` that eframe brings).
- **The Simulation tab**: studies (*New Study*), faces picked into conditions, a job
  thread with progress and cancel, band plots on the model with a legend, `.oxysim` and
  `.oxyres`.

Missing, whatever the engine:

- **The fluid region**: the space around the model (a box minus the solids) or inside it
  (the cavity, closed where the fluid enters and leaves; SolidWorks has the user add
  *lids*, flat caps over the openings).
- **A fluid mesh**, with thin cells along the walls, where the velocity changes fastest.
- **The solver**: pressure–velocity coupling, a turbulence model (k-ω SST is the usual
  default), the energy equation, heat across the boundary between fluid and solid.
- **Fluids** (air, water, oils: density, viscosity, specific heat, conductivity), as
  materials hold the solids' properties.
- **Plots through the fluid**: cut planes, iso-surfaces, flow trajectories.

## 2. Licences decide how an engine can attach

Oxyde is MIT. As SIMULATION.md §2 found for Gmsh, GPL code linked into Oxyde would make
the bundled program GPL. So:

- **GPL and AGPL engines** can only run as **separate programs**: Oxyde writes input
  files, starts the engine's executables and reads their output files; it never links
  them and copies none of their code. Command lines and files at arm's length are how
  separate programs talk in the GPL FAQ's terms
  ([mere aggregation](https://www.gnu.org/licenses/gpl-faq.html#MereAggregation)), and
  FreeCAD's CfdOF workbench drives OpenFOAM exactly so.
- **LGPL engines** may also ship with Oxyde and be linked dynamically, as Netgen is (its
  exact source named in `packaging/THIRD-PARTY.txt`), but their code cannot be copied into
  our crates.
- **BSD, MIT and Apache** code may be read and ported into our crates, with attribution.
- **FluidX3D**, the well-known GPU lattice Boltzmann code, is not open source: its licence
  forbids commercial use.

## 3. The engines

| | OpenFOAM | SU2 | In house |
|---|---|---|---|
| Licence, attached as | GPL-3.0: a separate program | LGPL-2.1: a separate program or a shipped library | MIT: our crates |
| Fluid region and mesh | its own, from our surface meshes (`snappyHexMesh`) | ours: booleans for the region, Netgen for the mesh | ours |
| Physics | the widest (§3.1) | compressible and incompressible, SA and SST, heat, adjoints | what we build |
| Platforms | Windows, Linux, macOS | prebuilt for Windows, Linux, macOS | all |
| Users install | OpenFOAM, once | nothing | nothing |
| Our work | setup, case files, running, reading results, plots | the same, plus the fluid region and its mesh | all of it, and the validation |

### 3.1 OpenFOAM

The reference open-source CFD code, in two lines: OpenCFD's (openfoam.com, versions named
by year and month, **v2606** in June 2026) and the OpenFOAM Foundation's (openfoam.org,
numbered, **14** in July 2026). Finite volumes on any polyhedral mesh. Incompressible flows,
steady and transient (`simpleFoam`, `pimpleFoam`), heat with buoyancy (`buoyantSimpleFoam`,
`buoyantPimpleFoam`), fluids and solids together (`chtMultiRegionFoam`), compressible
flows, free surfaces (`interFoam`); k-ω SST, k-ε and Spalart–Allmaras turbulence, LES and
DES; porous zones; rotating parts in a moving reference frame or on a sliding mesh (AMI).
Those are OpenCFD's names: the Foundation's line has made its solvers modules of a single
`foamRun` since version 11, so cases differ between the two lines and we would pick one.

- **It meshes the fluid itself.** `snappyHexMesh` starts from a box of hexahedra, refines
  it towards STL surfaces, keeps the cells on the side of a given point
  (`locationInMesh`), snaps them onto the surfaces and grows thin layers along walls.
  Internal and external flows come from the same surfaces, the point saying which side is
  fluid: no boolean on our side, and the named regions of the STL become the boundary
  patches our conditions set. Our labelled surface meshes are exactly its input.
- **Windows**: OpenCFD's MinGW build, an installer (`OpenFOAM-windows-mingw.exe`) with the
  binaries and an MSYS2 terminal; parallel runs need Microsoft MPI 10; it cannot compile
  code (no dynamic code), which we would not need. The Foundation's line reaches Windows
  through the third-party blueCFD-Core, or WSL. **macOS**: openfoam-app, a native build of
  OpenCFD's v2606 for Apple silicon through Homebrew (Intel Macs stopped at v2506); the
  Foundation's line runs in a Linux virtual machine there. **Linux**: packages from both.
- **Proven in a CAD program**: FreeCAD's CfdOF (LGPL-3.0) writes OpenFOAM cases from
  FreeCAD models and runs them this way, meshing with `snappyHexMesh`, cfMesh or Gmsh, for
  laminar, RANS, LES and DES flows, buoyancy, porous zones and rotating regions. It supports
  Foundation 9 to 13 and OpenCFD 2006 to 2512, and notes that on Windows only OpenCFD's
  MinGW 2212 worked for its `createPatch` step: to be tested with the current build.
- **Against**: users install OpenFOAM (Oxyde finds it and says where to get it); its case
  files are a text format we write and read ourselves; the solver's progress reaches us
  through its log and files, not function calls.

### 3.2 SU2

Stanford's code, now the SU2 Foundation's: **8.5.0** in April 2026, prebuilt for Windows
(OpenMP or MPI), Linux and macOS. Finite volumes on unstructured meshes; compressible and
incompressible (variable-density) flows, Spalart–Allmaras and SST, hybrid RANS–LES, heat
in solids and across fluid–solid boundaries, and adjoint solvers that give the
sensitivity of a goal to the shape, which could serve generative design one day. Its LGPL
lets it ship inside Oxyde's installer.

Against: it does not mesh. We would make the fluid region a solid with OpenCASCADE
booleans (a box minus every body of the assembly, or the cavity closed by lids), which
fail easily on touching or overlapping parts, then fill it with Netgen's tetrahedra and
prism layers, whose robustness on real models is untried. Its home ground is
aerodynamics; slow internal flows with heat are less travelled.

### 3.3 In house

The way stress, heat and generative design went: a solver in `oxyde-sim`, the GPU later.
For flows the natural shape is SolidWorks Flow Simulation's own, a **Cartesian grid with
immersed boundaries**: cells that ignore the walls, cut where the surfaces cross them,
with wall functions standing in for the boundary layer. No fluid region to build and no
fluid mesher, robust on any closed model, and the grid suits the GPU, the geometric
multigrid of the generative engine (for the pressure) and the lattice mesher's exact cuts
against a bounding-volume hierarchy. A **lattice Boltzmann** solver on the GPU is the
other shape: transient, quick to write, good for a live wind-tunnel view of the kind
Ansys Discovery gives, weaker for steady answers and for heat.

Against: it is much more than the static solver was. Convection makes the equations
nonlinear, pressure and velocity must be coupled, and the turbulence model and the wall
treatment on cut cells are where results are won or lost; every step needs benchmarks
(the lid-driven cavity, the Schäfer–Turek cylinder, the backward-facing step, pipe
friction against Moody's chart) before its numbers can be trusted. Realistically laminar
flows first, turbulence well after.

### 3.4 Set aside

- **Code_Saturne** (EDF, GPL-2.0+): excellent and industrial, but Linux first (Windows only
  through WSL) and meshed elsewhere (SALOME).
- **Elmer** (CSC, GPL): finite-element multiphysics with Navier–Stokes and a k-ε model;
  thin on turbulence, meshed elsewhere.
- **Lethe** (Polytechnique Montréal, Apache-2.0 or LGPL-2.1): high-order finite elements,
  multiphase, CFD–DEM; built on deal.II, Trilinos and p4est, a Linux or Docker build.
- **Lattice Boltzmann frameworks**: Palabos (AGPL-3.0), OpenLB (GPL-2.0), waLBerla
  (GPL-3.0): research frameworks rather than engines for a CAD program to drive.
- **PyFR, NekRS** (BSD-3): high-order solvers for scale-resolving flows on GPUs, needing
  curved high-order meshes or hexahedra.
- **Gmsh** (GPL) as a mesher: already ruled out in SIMULATION.md §2.

## 4. Decisions and order

**Decided (October 2026, the owner):**

1. **OpenFOAM**, run as a separate program, in OpenCFD's line (openfoam.com, v2606 today):
   the line with native Windows and macOS builds, and the one the cases are written for
   (the Foundation's names its solvers differently).
2. **Oxyde's installer offers it**, so that users do not have to find it themselves.
3. **A new study type of the Simulation tab** (*New Study* › *Flow*), beside the static and
   thermal studies, as Fusion 360 lists CFD among its studies.
4. **Flow first**: incompressible flow, internal and external, laminar or turbulent.
   **Cooling** (heat carried by the fluid and conducted through the solids) next, as a
   study type of its own, **with conduction in the solids** from the start (conjugate heat
   transfer), in `format_version` 6.
5. **Files**: flow studies in `.oxysim` like the others, as `format_version` 5 (after the
   generative studies' 4 of the `topology-optimization` branch), their results in
   `.oxyres` with the OpenFOAM case folder kept beside them.
6. **The installer downloads OpenFOAM** from openfoam.com while installing, rather than
   carrying it.
7. **The crate** is `oxyde-flow`, standing alone like `oxyde-sim`.

**Order of the work:**

1. `oxyde-flow`, the engine's driver: built (§7).
2. The *Flow* study in the Simulation tab: built (§7).
3. The files: built (FORMAT.md §8–§9).
4. The installer's offer, and Preferences pointing at an installation: built (§5,
   *Installing*).
5. Cooling studies, with conduction in the solids (`chtMultiRegionSimpleFoam`): built (§5,
   *Cooling*; §7).
6. Next: flow trajectories.
7. Later: rotating parts (a moving reference frame), porous media, transient flows
   (`pimpleFoam`), radiation, the fluid's pressures and the solids' temperatures handed to
   static and thermal stress studies as loads.

## 5. How a flow study runs

As `oxyde-flow` does it:

- **One patch per model face.** The surfaces are written as one STL file, in metres, with
  a region per face named after its role and label (`wall_12`, `inlet_3`, `outlet_7`,
  `slip_9`). Each face becomes a boundary patch of its own, so conditions, goals and
  results work face by face, and the application maps labels to persistent names as it
  does for the static study. Faces in no condition are walls the fluid sticks to.
- **Meshing.** `blockMesh` fills the computational domain with cubes of the cell size (the
  box grows to whole cubes); `surfaceFeatureExtract` finds the sharp edges, and the faces'
  boundaries; `snappyHexMesh` halves the cells along the surfaces and edges as many times
  as the study asks, keeps those on the side of `locationInMesh`, snaps them to the
  surfaces and edges, and grows layers along walls when asked. For an internal flow the
  point is the study's (moved off the cells' planes by a small odd fraction of the finest
  cell); for an external flow it sits in the domain's first cube, which the study's check
  keeps clear of the model.
- **Internal flows need closed cavities.** The openings are capped by lids (SolidWorks'
  *Create Lids*), whose faces carry the inlet and outlet conditions. A cavity left open
  leaks into the domain's box, which is a wall in internal flows.
- **Solving.** `potentialFoam -writephi` gives the flow a start, then `simpleFoam`
  (SIMPLEC, `linearUpwindV` for the momentum, upwind for the turbulence, one
  non-orthogonal correction, GAMG for the pressure) runs until every initial residual is
  under the study's target or the iterations run out. Turbulence is k-ω SST with
  Spalding's wall function (`nutUSpaldingWallFunction`), which accepts any distance from
  the wall to the first cell; the turbulence coming in follows from an intensity and a
  length: k = 1.5 (I U)², ω = √k / (Cμ^¼ ℓ).
- **Boundary conditions.** Inlet velocities: `surfaceNormalFixedValue`; volume and mass
  flows: `flowRateInletVelocity`, a condition's rate shared between its faces by area;
  outlets: a fixed static pressure with `pressureInletOutletVelocity`, which lets the
  fluid come back in where it must; ideal walls: `slip`; external flows:
  `freestreamVelocity` and `freestreamPressure` all round the domain.
- **Goals.** Forces come from OpenFOAM's `forces` function object (pressure and friction).
  It is in the solver's `controlDict` only: the meshing programs read one without it, and
  the run copies `controlDict.solve` over it before solving. Flow rates are summed from the
  solver's face fluxes (`phi`), so what comes in is what goes out to the solver's
  tolerance; mean pressures are area-weighted over the faces.
- **Running.** The installation's environment is read once, through OpenCFD's launcher
  (`etc/openfoam -c`) or `setEnvVariables-v*.bat` on Windows; then each program starts
  directly in it, writing `log.<program>` in the case, which is read as it grows for
  progress and residuals. Cancel stops the program (and MPI's processes: `taskkill /T` on
  Windows). A program that fails stops the run with its `FOAM FATAL ERROR`; residuals that
  are not numbers, or a floating point trap, are a divergence.
- **Results.** The mesh (`constant/polyMesh`, its faces as lists or as OpenCFD's compact
  lists) and the last time's `U`, `p` and `phi`, all ASCII; cell centres and volumes worked
  out as OpenFOAM does. The case folder opens in ParaView (`case.foam`).
- **Units.** Millimetres at the crate's interface, metres in the case; pressures are gauge
  pressures in pascals, where OpenFOAM's incompressible solvers hold them divided by the
  density.

### Cooling

A *Cooling* study is a flow whose fluid carries heat away from the model, which conducts it
through its bodies (conjugate heat transfer, `chtMultiRegionSimpleFoam`), as SolidWorks
Flow Simulation's *heat conduction in solids*:

- **The fluid and the solids are meshed together.** Each body is an STL file of its own
  (`body_<k>.stl`, a region per model face). `snappyHexMesh` puts the cells inside each body
  in a cell zone (`body_<k>`, `cellZoneInside inside`) and the faces on each model face in a
  face zone of their own (`faceZoneNaming region`: `body_<k>_wall_<label>`), keeping every
  cell, since no surface blocks it. `topoSet` gathers the cells reached from the fluid's point
  without crossing a body in a `fluid` zone and the rest of the unzoned cells (round an
  internal flow's model, or pockets the bodies close off) in an `outside` zone, and
  `splitMeshRegions -cellZonesOnly -useFaceZones -addZones '((solid body_0 …))'` splits the
  mesh into the regions `fluid`, `solid` and `outside`, the last not solved. Between fluid
  and solid it makes a coupled pair of patches per model face, conformal face for face. No
  layers grow along the walls yet, turbulent or not: the walls are faces inside the mesh,
  where `snappyHexMesh` grows none (§8); the wall functions take the first cell as it is.
- **One solid region.** Every body is in the `solid` region, its material a species of a
  multi-component mixture whose mass fraction is one in the body's cells (`setFields` from
  the zones): heat crosses from body to body inside one matrix. Bodies as regions of their own
  were coupled only from one iteration to the next, which converged hundreds of times slower
  where they touch: a chip on an aluminium plate lost half a percent of its error per
  iteration, where one region converges in fifty. The species share one specific heat, which
  a steady state never uses, so that the enthalpy is continuous where the temperature is, and
  the conductivities meet harmonically at a change of material (`laplacian(alpha,h) Gauss
  harmonic`). OpenFOAM's mixture turns a mass fraction of exactly zero into a NaN, so the
  other species keep 10⁻⁸.
- **Patches by face and role.** After the split Oxyde renames the patches: on the fluid's side
  `wall_<label>` (coupled), `inlet_<label>`, `outlet_<label>` (made plain patches),
  `slip_<label>`; on the solid's, `wall_<label>` (coupled), `free_<label>` (under an opening:
  adiabatic) and `outer_<label>` (facing `outside`). The coupled walls are
  `compressible::turbulentTemperatureRadCoupledMixed`, each side taking the other's
  temperature and conductance.
- **The fluid** has constant properties and, with gravity, a density falling as it warms
  (Boussinesq: ρ = ρ₀ (1 − β (T − T₀))); warmer fluid rises. Its pressures are absolute
  (`p_rgh` around 10⁵ Pa). With gravity the fluid starts at rest under its weight: `p` is
  written hydrostatic from the cells' heights, since the solver makes `p_rgh = p − ρ g·x`
  from it (a uniform `p` left the whole fluid's weight unbalanced on the first iteration: a
  sphere in still air went down to 135 K and diverged). Inlets bring fluid in at the study's
  temperature; openings (a fixed `p_rgh`: their pressure above the surroundings', which are
  at rest) and the free stream (`freestreamPressure`, or `totalPressure` on `p_rgh` in still
  surroundings) take it in at that temperature where it flows back. SIMPLE with
  under-relaxation 0.7 for the velocity and 0.3 for the pressure, the energy unrelaxed.
- **Heat loads.** *Internal heat* is a source in the bodies' zones (`scalarSemiImplicitSource`,
  W in all, shared by volume); *heat power* and *heat flux* heat the solid's cells along their
  faces, wherever the faces are; a *temperature* holds the faces (both sides of a wetted
  one); *convection* acts on faces outside the fluid (`externalWallHeatFluxTemperature`),
  since where the fluid touches, the flow works it out.
- **Stopping.** `chtMultiRegionSimpleFoam` has no residual test of its own: Oxyde follows the
  fluid's residuals and, for the solid, `T (solid)`, how far its hottest temperature moved
  in the iteration over its rise above the fluid's coldest (the solid's own residual is no
  measure: a body that conducts well is nearly uniform, and OpenFOAM divides its residual by
  how far it is from uniform, so a copper sphere settled to a thousandth of a kelvin still
  showed 0.05). Once every one is under the target, Oxyde sets `stopAt writeNow` in the
  `controlDict`, which the solver re-reads while it runs. A plume never quite stops moving,
  though: in still air the fluid's residuals level off near 0.2 however long the run goes.
  So the run also stops once the bodies' hottest temperature has settled: after a hundred
  iterations at least, it kept within twenty times the target of its rise over the last
  fifty (a sphere in still air, 39 K above the air, within 0.08 K at 10⁻⁴: stopped at 103
  iterations, its surface where 1500 left it). The pressure's solve is capped at a hundred
  cycles: near convergence it can stall short of its relative tolerance.
- **Goals.** The force and flow goals as a flow study's (the fluxes are of mass there);
  *max temperature* is the hottest cell or face of the bodies of the goal's faces, *mean
  temperature* the faces' area-weighted temperature, *heat flow* Fourier's law across each
  face's half cell: on the fluid's side where the fluid touches, (κ + cp αt) (T_face −
  T_cell) A / d with the wall functions' αt, on the solid's elsewhere. (The solid's side
  is no measure where it conducts thousands of times better than the fluid: a copper
  sphere's faces differ from its cells by a ten-thousandth of a kelvin, and the sum came
  out 44 % short.)

### Installing

On Windows, the Inno Setup installer gets a task, *Install OpenFOAM v2606 for flow
simulations*, which downloads OpenCFD's installer from openfoam.com
(`https://dl.openfoam.com/source/v2606/OpenFOAM-v2606-windows-mingw.exe`, 203 MB; the
version is pinned so that every install meets the cases this crate writes) and runs it.
Downloaded from its makers, OpenFOAM is not inside Oxyde's download, which stays small and
carries no GPL code. Parallel runs also need Microsoft MPI 10. The `.msi` cannot chain
another installer, so there, and on macOS (openfoam-app, through Homebrew) and Linux
(OpenCFD's packages), Preferences say where to get it. Preferences also take an
installation's folder; `OXYDE_OPENFOAM_DIR` does the same for tests and scripts.

## 6. The owner's answers

The questions this plan asked are answered (§4, *Decided*): OpenFOAM in OpenCFD's line, run
as a separate program; the installer downloads it; flow studies are a study type of the
Simulation tab; flow first and cooling later; flow studies and results in `.oxysim` and
`.oxyres`, the case folder beside them; the crate `oxyde-flow`. Cooling studies are a study
type of their own in `format_version` 6, with conduction in the solids.

## 7. What is built

### In the application

- **New Study › Flow** (`simulation_studies.rs`); the study's settings are
  `oxyde_format::simulation::FlowSettings` (the fluid, internal or external, laminar or
  turbulent, the mesh level, the iterations and the residual), its conditions and goals
  items of the Simulation tab like fixtures and loads (`ItemKind::InletVelocity`,
  `InletVolumeFlow`, `InletMassFlow`, `StaticPressure`, `IdealWall`, `Goal`), picked from the
  selection, listed, edited and drawn on the model the same way.
- **`flow_ui.rs`**: the command bar (Inlet ▾, Static Pressure, Ideal Wall, Goal ▾, Run, the
  plots), the panel (Fluid, Boundary conditions, Goals, Mesh and solver, Results with the
  residual chart, live while it runs), the study turned into an `oxyde_flow::FlowStudy`
  (the mesh from the level and the model's size, `setup::mesh_for_level`, within the
  memory budget at 2 KB a cell; the domain from the model's box; an internal flow's point
  in the fluid three of the finest cells in from its largest opening triangle,
  `setup::point_inside`), the run on its own thread (OpenFOAM found, the processes chosen,
  the case written and run, the flow read back, the goals measured), and the plots: cut
  planes (`plot::cut`, square to X, Y or Z, the model drawn faint around them) and the
  wetted faces (`plot::surface`), split into the legend's colour bands.
- **Leaks**: after meshing, an internal flow whose fluid reached the domain's box (an open
  cavity) is stopped with what to do (`RunError::Leak`).
- **Files** (`simulation_files.rs`): the study's items and settings to and from `.oxysim`;
  the run's record (`oxyde_format::results::FlowRun`) to `.oxyres`, the case copied beside
  the study when it was solved before the study was saved (without its processor folders),
  and read back from there when the results are opened.
- **Preferences › Simulation**: the OpenFOAM folder (with *Check*), the processes (0: one
  per core up to eight when MPI is there), where to get OpenFOAM.
- **The Windows installer** (`packaging/installer.iss`): the task *Download and install
  OpenFOAM v2606*, OpenCFD's installer downloaded with Inno Setup's download page, checked
  against its SHA-256 and run after Oxyde's files are in place; a failed download leaves
  Oxyde installed and says where to get OpenFOAM. The `.msi` cannot chain an installer:
  Preferences says where to get it.
- **New Study › Cooling**: everything a flow study has, and in the panel a **Heat** section
  (the temperature the fluid comes in at, gravity for natural convection, the fluid's
  specific heat, conductivity and expansion, which the fluid's preset sets too) and a
  **Material** section (each body's conductivity, from its material or typed in, as in
  thermal studies). The command bar's **Heat ▾** adds *Internal Heat*, *Heat Power*, *Heat
  Flux*, *Temperature* and *Convection*, the thermal studies' items; **Goal ▾** adds *Max
  Temperature*, *Mean Temperature* and *Heat Flow*; the plots add a temperature cut plot
  (through the fluid and the bodies) and the temperature on the model's faces.
  `flow_study` hands `oxyde_flow` a `Heat` with each body's conductivity
  (`Surfaces::conductivities`) and the thermal items as loads; the study and its results
  are saved as `format_version` 6.

### `oxyde-flow`

- `study`: the study as plain data (`FlowStudy`: labelled surfaces, `FlowType` internal or
  external, `Fluid`, `Turbulence`, `Condition`s, `Goal`s, `Domain`, `MeshSettings`,
  `SolverSettings`) and its check, with messages for the user (`StudyError`).
- `case`: `write_case`, the OpenFOAM case of §5; it clears what an earlier run left (mesh,
  time and processor folders, logs) and nothing else.
- `install`: `Installation::find` (`OXYDE_OPENFOAM_DIR`, then the usual places per system,
  newest first), the environment read once, programs started in it.
- `run`: the programs one after the other, logs followed, `Progress` reported, Cancel;
  `RunReport` (cells, mesh checks, iterations, convergence, the residual history).
- `log`, `foam`, `mesh`, `results`: OpenFOAM's logs, its ASCII files, the mesh with its
  geometry, the fields on it, the goals; `results::label_of_patch` reads a patch's model
  face from its name, so a case folder alone is enough to read results back.
- `setup`: the mesh of a level, the automatic domain, the point inside a cavity.
- `plot`: cut planes and wetted surfaces as polygons with values; for a cooling study's
  temperature, the cut goes through the solid too and the surface is the solid's.
- `cooling`: a cooling study's case (§5, *Cooling*): the bodies' STL files and zones, the
  `topoSet` and `splitMeshRegions` steps, then [`cooling::finish`] after the split: the
  patches renamed, the leak check, and every region's files (thermophysics, a heat source
  per load, a cell set per face load, the starting fields). `run` runs it with the solver
  stopped at convergence; `results` reads both regions back (temperatures in °C, pressures
  gauge) and works out the temperature and heat goals.

**Tests.** 49 unit tests that need no OpenFOAM: the case's files, the mesh and field
readers (both face formats, compact lists, every kind of patch entry), the geometry of
faces and cells, the log reader (residuals, convergence, fatal errors, divergence,
`checkMesh`, the solid's temperature), the run's log following and stopping, the leak
check, an installation's environment from a fake `etc/bashrc`, the mesh levels, the point
inside a cavity, cut and surface plots; for cooling, the bodies' zones, the split's patches
renamed and every region's files. Then `tests/openfoam.rs`, through OpenFOAM v2606 (skipped
without it), on a Raspberry Pi 4 in OpenCFD's arm64 image, on one core (two for the MPI run
and the first cold plate):

| Case | Cells | Iterations | Meshing + solving | Against |
|---|---|---|---|---|
| Laminar pipe: water, 10 mm bore, 100 mm long, Re 20 | 41 696 | 44 | 37 s + 31 s | Pressure gradient past the entrance −0.6347 Pa/m, Hagen–Poiseuille's −0.6413 (−1.0 %); velocity on the axis twice the mean within 2.5 %; flow out equal to flow in to 1.2 × 10⁻⁷; the wall's friction (5.28 µN) balancing the inlet pressure over the bore (5.37 µN) |
| The same pipe on two processes (MPI) | 41 696 | 45 | 36 s + 24 s | The same gradient (−1.0 %) |
| Sphere in air at Re 20, the domain five diameters round it and ten behind | 19 351 | 102 | 13 s + 29 s | Drag coefficient 2.738, Clift, Grace and Weber's correlation 2.735 (+0.1 %); side forces 10⁻⁴ of the drag |
| Turbulent pipe: water at Re 19 924, k-ω SST, three layers along the wall | 75 296 | 100 | 43 s + 180 s | Inlet pressure 812 Pa, 1.5 times Blasius's 531 Pa for a developed flow over L/D = 10: the entrance adds the rest (a check of order only) |
| Cold plate (cooling): water at 0.1 m/s through a 4 mm square channel in an aluminium plate of four bars and two plastic lids, a 10 W ceramic chip on top; on two processes | 65 856 | 98 | 71 s + 117 s | Heat into the water through the channel's walls 10.0000 W, carried out by it (ṁ cp ΔT) 9.9989 W; the chip at 37.5 °C |
| The same plate turbulent: water at 2 m/s (Re 8000, k-ω SST), the 10 W let in through the chip's top face, the bottom face losing heat to 20 °C (50 W/m²·K) | 65 856 | 102 | 71 s + 124 s | Into the water 9.977 W, off the bottom 0.023 W: 10.000 W; stopped once its temperatures settled, the same to four figures as at 120 iterations, where its residuals met the target |
| Copper sphere, 10 mm, heated by 0.033 W inside, in air at Re 20 | 21 303 | 124 | 17 s + 157 s | Heat flow off its faces 0.03298 W (−0.1 %); Nusselt number 3.98, Clift, Grace and Weber's 4.12 (−3.4 %) |
| The same sphere, 20 mm, heated by 0.5 W in still air with gravity (natural convection, Ra 3.3 × 10⁴) | 25 454 | 103 | 19 s + 53 s | Nusselt number 7.87, Churchill's 8.12 (−3.1 %); stopped once its temperature settled, which 1500 iterations left as it was (59.35 °C) |

## 8. Next

- **Flow trajectories**: lines seeded on inlets and followed through the cells.
- **Cooling, further**: radiation between faces and to the surroundings, contact
  resistance between bodies, layers along cooled walls, fans as conditions, and the
  temperatures handed to thermal stress studies.
- Seeing the mesh before solving, a domain the user sizes, rotating regions, porous media,
  transient flows.
- To check on Windows and macOS: OpenCFD's MinGW build and openfoam-app found and run
  (`install.rs`, untested there), Microsoft MPI for parallel runs, a case folder whose path
  has spaces.

## 9. Testing without OpenFOAM installed

OpenCFD publishes images for amd64 and arm64. Build the test binary, then run it in one:

```sh
cargo test -p oxyde-flow --test openfoam --no-run      # prints the binary's path
docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp \
  -v "$PWD/target:$PWD/target:ro" --entrypoint "$PWD/target/debug/deps/openfoam-<hash>" \
  opencfd/openfoam-run:2606 --test-threads=1 --nocapture
```

The image (Ubuntu 26.04) runs a binary built on an older Linux. Add
`-e OXYDE_FLOW_TEST_DIR=<folder> -v <folder>:<folder>` to keep the cases and their logs.
