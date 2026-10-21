# Flow simulation: CFD in Oxyde

Status: **decided, being built** (October 2026). The owner chose OpenFOAM, run as a
separate program and offered by Oxyde's installer; flow studies are a new study type of
the Simulation tab; flow first, cooling later (§4). Built so far: `oxyde-flow`, which
writes, runs and reads back OpenFOAM cases, tested against closed forms through
OpenFOAM v2606 (§7). Not built yet: the study in the Simulation tab, its files and the
installer's offer. §1–§3 are the survey the choice was made from.

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
   **Cooling** (heat carried by the fluid and conducted through the solids) comes later,
   as a study type of its own.

**Order of the work:**

1. `oxyde-flow`, the engine's driver: built (§7).
2. The *Flow* study in the Simulation tab: internal or external flow, the fluid (air and
   water to start with), conditions on faces (inlet velocity, volume or mass flow; outlet
   pressure; ideal walls), goals (forces, flow rates, mean pressures), the computational
   domain drawn as a box, a mesh level, *Run* on a job thread with a live residual plot
   and Cancel. Results: the goals, surface plots (pressure, wall shear) and cut plots
   (velocity and pressure on a plane through the fluid), flow trajectories next.
3. The files: flow studies and their results in `.oxysim` and `.oxyres` (§6).
4. The installer's offer, and Preferences pointing at an installation (§5, *Installing*).
5. Cooling studies: `buoyantSimpleFoam` for heat carried by the fluid,
   `chtMultiRegionSimpleFoam` for heat through the solids as well.
6. Later: rotating parts (a moving reference frame), porous media, transient flows
   (`pimpleFoam`), runs on every core by default, and the fluid's pressures handed to
   static studies as loads.

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

### Installing (to build)

On Windows, the Inno Setup installer gets a task, *Install OpenFOAM v2606 for flow
simulations*, which downloads OpenCFD's installer from openfoam.com
(`https://dl.openfoam.com/source/v2606/OpenFOAM-v2606-windows-mingw.exe`, 203 MB; the
version is pinned so that every install meets the cases this crate writes) and runs it.
Downloaded from its makers, OpenFOAM is not inside Oxyde's download, which stays small and
carries no GPL code. Parallel runs also need Microsoft MPI 10. The `.msi` cannot chain
another installer, so there, and on macOS (openfoam-app, through Homebrew) and Linux
(OpenCFD's packages), Preferences say where to get it. Preferences also take an
installation's folder; `OXYDE_OPENFOAM_DIR` does the same for tests and scripts.

## 6. Questions still open for the owner

1. **Files.** Flow studies saved in `.oxysim` like the others (a new `format_version`,
   numbered after the generative studies' 4 on the `topology-optimization` branch), with
   their results in `.oxyres` (the goals, the residual history and what the plots need)
   and the OpenFOAM case folder kept beside them for ParaView; or only the case folder?
2. **The installer.** Download OpenFOAM while installing (proposed above), or ship its
   installer inside Oxyde's (some 200 MB more, and Oxyde's download then carries GPL
   software, with its source)?
3. **The crate.** Built as `oxyde-flow`, standing alone like `oxyde-sim` (the layering
   table and its test changed with it); renaming it is easy now.

## 7. What is built: `oxyde-flow`

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
  geometry, the fields on it, the goals.

**Tests.** 34 unit tests that need no OpenFOAM: the case's files, the mesh and field
readers (both face formats, compact lists, every kind of patch entry), the geometry of
faces and cells, the log reader (residuals, convergence, fatal errors, divergence,
`checkMesh`), the run's log following, an installation's environment from a fake
`etc/bashrc`. Then `tests/openfoam.rs`, through OpenFOAM v2606 (skipped without it), on a
Raspberry Pi 4 in OpenCFD's arm64 image, on one core (two for the MPI run):

| Case | Cells | Iterations | Meshing + solving | Against |
|---|---|---|---|---|
| Laminar pipe: water, 10 mm bore, 100 mm long, Re 20 | 41 696 | 44 | 37 s + 31 s | Pressure gradient past the entrance −0.6347 Pa/m, Hagen–Poiseuille's −0.6413 (−1.0 %); velocity on the axis twice the mean within 2.5 %; flow out equal to flow in to 1.2 × 10⁻⁷; the wall's friction (5.28 µN) balancing the inlet pressure over the bore (5.37 µN) |
| The same pipe on two processes (MPI) | 41 696 | 45 | 36 s + 24 s | The same gradient (−1.0 %) |
| Sphere in air at Re 20, the domain five diameters round it and ten behind | 19 351 | 102 | 13 s + 29 s | Drag coefficient 2.738, Clift, Grace and Weber's correlation 2.735 (+0.1 %); side forces 10⁻⁴ of the drag |
| Turbulent pipe: water at Re 19 924, k-ω SST, three layers along the wall | 75 296 | 100 | 43 s + 180 s | Inlet pressure 812 Pa, 1.5 times Blasius's 531 Pa for a developed flow over L/D = 10: the entrance adds the rest (a check of order only) |

## 8. Testing without OpenFOAM installed

OpenCFD publishes images for amd64 and arm64. Build the test binary, then run it in one:

```sh
cargo test -p oxyde-flow --test openfoam --no-run      # prints the binary's path
docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp \
  -v "$PWD/target:$PWD/target:ro" --entrypoint "$PWD/target/debug/deps/openfoam-<hash>" \
  opencfd/openfoam-run:2606 --test-threads=1 --nocapture
```

The image (Ubuntu 26.04) runs a binary built on an older Linux. Add
`-e OXYDE_FLOW_TEST_DIR=<folder> -v <folder>:<folder>` to keep the cases and their logs.
