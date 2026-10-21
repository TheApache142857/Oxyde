# Flow simulation: a plan for CFD in Oxyde

Status: **proposal** (October 2026). Which open-source engines could compute flows for
Oxyde, how each would attach to it, and a recommendation. The engine is a new dependency,
so the choice is the owner's (§6); nothing is built yet.

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

## 4. Recommendation and order

1. **Whatever the engine, the study and the results are ours**, as for stress: a flow
   study (internal or external, the fluid, gravity, the computational domain's box),
   conditions on faces by persistent name (inlet velocity, mass or volume flow; outlet or
   environment pressure; walls, with roughness and heat), heat sources in solids, goals;
   the results read into plain arrays and drawn by us (surface plots on the model, cut
   plots, flow trajectories, goal and convergence plots), and later the pressures and
   temperatures passed to static and thermal studies.
2. **The engine: OpenFOAM, OpenCFD's line, as a separate program.** It is the only
   open-source engine covering what Flow Simulation users expect (internal and external
   flows, turbulence, heat in fluids and solids, rotating parts, porous media); it meshes
   the fluid from the surface meshes we already make, so neither booleans nor a fluid
   mesher are ours to write; it runs natively on Windows, Linux and macOS; and, never
   linked, it leaves Oxyde MIT. The price is an install for users.
3. **Later, if wanted: an in-house Cartesian solver**, for a quick look without any
   install and on the GPU, tested against OpenFOAM on the same cases, as the CPU solver is
   the GPU's reference in generative design.

SU2 is the better choice only if shipping everything inside Oxyde's installer matters
more than the meshing work and the narrower physics.

## 5. The OpenFOAM route, sketched

- **A crate standing alone**, `oxyde-flow`, like `oxyde-sim`: plain structs in (labelled
  triangle meshes, the study by labels), plain arrays out. It writes the case, runs the
  programs, follows them and reads the results; the app maps labels to persistent names.
- **The case**: the surfaces as one STL with a named region per group of faces (each
  inlet, outlet and wall condition; each solid, for heat in solids), scaled from
  millimetres to metres; `blockMeshDict` from the domain box; `snappyHexMeshDict` with its
  refinement and layers from one resolution setting (SolidWorks' *global mesh* level), and
  `locationInMesh` a point outside every solid for an external flow, inside the cavity for
  an internal one; the `0/` fields from the conditions; `controlDict` with function
  objects for the goals (forces, flow rates and mean pressures on patches, probes).
- **Lids**: flat openings capped automatically (their boundary loop is planar), besides
  lid features made by hand as SolidWorks asks.
- **Running**: `blockMesh`, `snappyHexMesh`, `checkMesh`, then the solver (split across
  cores with `decomposePar` and MPI when wanted), each started inside the distribution's
  environment, its install folder found in the usual places or set in Preferences. The
  log gives the residuals and the goals' values every iteration, for a live plot (Flow
  Simulation's solver monitor); Cancel stops the process; a run that diverges says so.
- **Results**: the mesh (`constant/polyMesh`: points, faces, owners, neighbours, patches)
  and the last time's fields, written as ASCII to keep the reader small. Incompressible
  solvers write the pressure divided by the density (m²/s²): the reader multiplies it
  back. Cut plots intersect the cells with a plane; trajectories are integrated through
  the cells by us. The case folder stays openable in ParaView.
- **Tests**: those that run OpenFOAM skip without it, as the Netgen tests do. Against
  closed forms and benchmarks: laminar pipe flow (the Hagen–Poiseuille pressure drop), a
  flat plate's drag (Blasius), the Schäfer–Turek cylinder, turbulent pipe friction
  (Moody), a duct with a heated wall (a Nusselt correlation).

## 6. Questions for the owner

1. **The engine**: OpenFOAM as a separate program (recommended), SU2 shipped with Oxyde,
   or in house from the start?
2. **If OpenFOAM**: OpenCFD's line (native Windows and macOS builds) rather than the
   Foundation's? Installed by users, Oxyde finding it, or its Windows installer offered
   from Oxyde's (allowed for a separate program, with its source)?
3. **Where it lives**: a *Flow* study type in the Simulation tab (*New Study*), as Fusion
   360 lists CFD among its studies, or a tab of its own, like SolidWorks' Flow Simulation
   add-in?
4. **First scope**: steady incompressible flow, internal and external, laminar and k-ω
   SST, then heat in the fluid, heat in the solids, transient flows, rotating parts; or
   heat from the start (electronics cooling)?
5. **Files**: flow studies in `.oxysim` (a new `format_version`); results as the OpenFOAM
   case folder beside the study, or read into `.oxyres`?
6. **The crate**: `oxyde-flow`, standing alone like `oxyde-sim` (the layering table and
   its test change with it)?
