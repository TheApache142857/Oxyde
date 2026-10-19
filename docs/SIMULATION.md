# Simulation: a plan for a Simulation tab

Status: **built** (October 2026): a linear static study end to end, in parts and
assemblies, with Netgen (or the in-house lattice mesher) and an in-house solver, contacts
as in Fusion 360, mechanical properties on materials, and studies and results saved as
`.oxysim` and `.oxyres` (see §5 for the owner's answers, §6 for what is built).

This document lays out how Oxyde gets a Simulation tab, the way SolidWorks has SolidWorks
Simulation (and Fusion 360 its Simulation workspace): the part or assembly held by
fixtures, loaded by forces, pressures or its weight, meshed into tetrahedra and solved by
the finite element method, then stress, displacement and factor-of-safety plots drawn on
the model. A linear static study comes first; frequency, buckling, thermal and nonlinear
studies are later steps. Decisions here are proposals for the owner; the roadmap
(ARCHITECTURE.md §10) gets a row once one is taken.

## 1. What we have

- **Geometry as meshes.** `Kernel::tessellate_for_export` gives a closed triangle mesh of
  every solid, one `FaceMesh` per B-rep face, at a chosen deflection: exactly the
  boundary a volume mesher starts from, with each triangle knowing its face, so
  fixtures and loads named by persistent face names (`FaceName`, and
  `(Occurrence, FaceName)` in assemblies) map onto the mesh.
- **Materials** (`oxyde_model::Material`): a name, a category and a density. No elastic
  modulus, Poisson's ratio or yield strength: the format has no keys for them.
- **The Render tab** as the pattern for a tab that works on the whole model without
  changing it: a panel at the top of the left side, settings in the preferences, heavy
  work on a thread of its own, results painted in the viewport.
- **A viewport scene that can be replaced** (the Section View and the STL preview do it):
  any `Mesh` with a material (colour) per face, drawn by the GPU or the software renderer,
  pinned until the document changes. Result plots are such a mesh.
- **No volume mesher.** OpenCASCADE's free distribution meshes surfaces only (BRepMesh);
  its tetrahedral mesher (the *Express Mesh* / Mesh Framework add-ons) is commercial.

## 2. The routes

A linear static study is three pieces: a **volume mesh** (tetrahedra), a **solver**
(assemble the stiffness matrix, apply fixtures and loads, solve `K u = f`, recover
stresses) and the **UI** (study setup, plots). The solver and the UI are ours in every
route; only the mesher has real alternatives.

### The mesher

1. **In house, from the surface mesh: a lattice mesher with boundary snapping**
   (isosurface stuffing, Labelle and Shewchuk 2007, on a cubic lattice). Tetrahedra fill
   a regular lattice over the model; lattice edges crossing the boundary are cut where
   they meet the surface mesh (exact segment–triangle hits), lattice points close to the
   boundary are moved onto it, and the cells cut by the boundary are split into
   tetrahedra by fixed stencils. Robust (no failure cases on any closed mesh), fast,
   a few hundred lines, no dependency. Its limits: the elements are all about the same
   size (no grading towards small features), features thinner than an element vanish
   (the mesh volume shows it), and sharp edges and corners are bevelled at the scale of
   the element size, which softens stress concentrations at re-entrant corners and
   makes thin plates a little thinner.
2. **Netgen** (C++, LGPL-2.1): the mesher of SALOME and FreeCAD's FEM workbench. It
   meshes OpenCASCADE geometry directly (`netgen/occ`), with grading, curvature- and
   size-based refinement and curved second-order elements on the true surface. The best
   quality available for free, and it fits the shim: it would be called from
   `crates/oxyde-occt/cpp/` like any OCCT algorithm, through a new `Kernel` method
   returning a plain tetrahedral mesh. It is a new C++ dependency built against the same
   OpenCASCADE (a vcpkg port or a build of our own), dynamically linked for the LGPL.
3. **Gmsh** (C++, GPL-2.0+ with exceptions): excellent too, also reads OpenCASCADE
   geometry. Its GPL would make a bundled Oxyde GPL: not without the owner changing the
   licence.
4. **TetGen** (AGPL-3 or commercial) and **fTetWild** (MPL-2, with Eigen, Geogram and
   more): constrained Delaunay from the surface mesh and robust "wild" meshing. TetGen's
   licence rules it out; fTetWild is heavy to build and slow.

### The linear solver

1. **In house: conjugate gradients** with a block-Jacobi preconditioner on a block
   sparse matrix (3 × 3 blocks), the matrix-vector product on every core. No dependency,
   little memory, good enough for tens of thousands of nodes; slow on badly conditioned
   models (thin parts, very different stiffnesses in one assembly).
2. **faer** (pure Rust, MIT): a sparse Cholesky factorisation with fill-reducing
   ordering. Direct, so robust on any well-posed model and fast for several load cases
   on one mesh; more memory. A new crate dependency.
3. Multigrid or incomplete-Cholesky preconditioners in house: more work, a middle way.
   (Smoothed-aggregation multigrid is built: §6, *Speed*.)

## 3. Recommendation and order

1. **The solver crate** `oxyde-sim`, pure Rust, depending on no other crate: elements
   (four- and ten-node tetrahedra), assembly, fixtures and loads, CG, stress recovery,
   von Mises, factor of safety, contour bands for plots. Tested against known answers:
   a bar in tension, a cantilever's tip deflection and bending stress.
2. **The in-house lattice mesher** in the same crate, behind one function from closed
   triangle meshes (with a face label per triangle) to a tetrahedral mesh (with a face
   label per boundary facet and a body per element), so that Netgen can replace it
   later without touching the solver or the UI.
3. **The Simulation tab** (parts and assemblies): the study set up from the selection,
   meshed and solved on a thread, plots in the viewport. Study kept for the session.
4. **Owner decisions** (§5): mechanical properties on materials and studies in the
   files; then Netgen behind a `Kernel` method and faer for the solve, if wanted.
5. Later: probing values, section clipping of plots, a report, mesh controls (local
   sizes), bolt and pin connectors, contact other than bonded, frequency and buckling
   studies (an eigen solver on the same matrices), thermal.

Steps 1–3 need no decision and are built (§6).

## 4. Design

- **Units**: millimetres, newtons and megapascals (N/mm²) throughout, which are
  consistent: stresses come out in MPa, displacements in mm. Densities in kg/m³ as the
  materials hold them, turned into N/mm³ for weight.
- **Mesh**: `TetMesh` with nodes, tetrahedra (corners, and for quadratic elements six
  edge nodes at the edge midpoints, so every element is straight-sided), a body index per
  element, and the boundary facets with a face label. Labels are plain numbers: the app
  keeps the table from labels to `(solid, face)` and from there to persistent names.
- **Elements**: ten-node tetrahedra by default (SolidWorks' *high quality*), four-node
  ones as *draft*. Linear tetrahedra are far too stiff in bending: a cantilever two
  elements thick deflects less than half of what it should.
- **Fixtures**: *Fixed geometry* (all three displacements zero on the faces) and
  *Roller/Slider* (no motion normal to the faces, sliding along them). Constraints are
  applied exactly by turning the constrained nodes' coordinates into a local frame and
  removing the constrained directions. Before solving, each connected piece of the mesh
  is checked for rigid-body motions left free (the six of them, against the constraints'
  directions): a part that would float is refused with its name, as SolidWorks warns that
  the model is not held, instead of a solve that never converges.
- **Loads**: *Force* (a total in newtons spread over the faces by area, along a direction
  or normal to each face), *Pressure* (MPa normal to the faces, positive pressing in),
  *Gravity* (the material's weight, along −Y by default, the world's down). Consistent
  nodal loads (for a six-node triangle the corners take nothing and the edge nodes a third
  each).
- **Assemblies and contacts** (`contact`), as Fusion 360 has them (§5.6). Each solid is
  meshed on its own by default (`MeshOptions::separate_bodies`; Netgen's per-body meshes
  arrive the same way): one `TetMesh` whose bodies share no node, each closed by its own
  labelled boundary. Contacts tie them, and depend on the `TetMesh` only, not on the
  mesher. Meshing the solids together instead (the lattice running across them, touching
  parts sharing nodes) remains as an option, which is bonded everywhere.
  - *Kinds*: **bonded** (the faces move together), **separation** (no penetration, may
    part, frictionless), **sliding** (no penetration and no parting, frictionless),
    **rough** (no slip, may part) and **offset bonded** (bonded across a gap). A
    `Contact { kind, a_labels, b_labels, offset }` in the `Study`; `offset` is the gap it
    bridges, 0 for faces that touch.
  - *Detection*: `detect_contacts(mesh, tolerance)` lists pairs of face labels on
    different bodies whose facets lie within the tolerance of each other and face each
    other (normals within 25° of opposite, so faces meeting at an edge are not taken),
    with the area found and the largest gap, for the UI to list and turn into contacts of
    the global kind (`ContactCandidate::contact`). Pairs set by hand are the same
    `Contact`, from two sets of labels.
  - *Discretisation*, node to surface: of a contact's two sides the one with the smaller
    facets is the slave; each of its boundary nodes (corners and edge nodes) is tied to the
    nearest point of the master's facets, when within the offset plus a quarter of an
    element. The point moves with the master facet's own shape functions (the six-node
    triangle's on a ten-node mesh), and each slave node stands for its share of its facets'
    area (a twelfth at a corner and a quarter at an edge node of a six-node facet).
    Bonded, offset bonded and closed rough nodes hold the relative displacement in all
    three directions; sliding and closed separation nodes along the master's normal.
  - *Solution*: **augmented Lagrangian**. Each tie adds `w (λ·g + ε/2 |g|²)` to the energy:
    the penalty `ε = 30 E/h` goes in the matrix, the multiplier `λ` (a traction) in the
    loads, updated by `λ ← λ + ε g` after each solve until the gaps are a
    hundred-thousandth of the largest displacement. The matrix stays symmetric positive
    definite, so conjugate gradients and the multigrid preconditioner work unchanged
    (Lagrange multipliers as unknowns would make it a saddle point, which neither
    solves), and the ties end up exact with a penalty moderate enough not to slow them
    (a pure penalty would have to be stiff, and the solve slow, to be as exact). Each
    solve starts from the last one's displacements with the same multigrid hierarchy.
  - *Separation and rough* are nonlinear: every node starts closed; when the multipliers
    have converged, nodes whose traction (averaged over their facets, since a ten-node
    face's corner nodes carry no force under a uniform pressure) pulls open, open nodes
    that penetrate close, the matrix changes and the multipliers converge again, until no
    node changes. Small margins stop nodes on the edge of the closed zone flipping on
    rounding, and a node that has opened three times stays closed, which ends any cycle.
    The study being linear and frictionless, the result does not depend on the loading
    path, so there are no load steps. Initial gaps within a contact's reach count as
    closed (Fusion's and ANSYS's *adjust to touch*).
  - *Held or not*: pieces tied by contacts are checked for free rigid motions together;
    a sliding or separation contact holds along the normal only, so a block resting on a
    frictionless plane with nothing else holding it is refused, as it would slide away.
  - *Results*: per contact, the nodes tied and those too far to be, the force side b
    exerts on side a, its normal part, the area closed and the largest pressure; the
    contact pressure at the nodes of both sides (facet averages, carried over to the
    master side); and how the iterations went (`ContactSolve`: solves, changes of the
    matrix, converged or not, the violation left). Non-convergence returns the last
    solve's results with `converged` false, for the UI to warn.
- **Results**: displacement per node, the stress tensor per node (averaged over the
  elements around it), von Mises stress, and the factor of safety (yield strength over
  von Mises, SolidWorks' default *max von Mises* criterion). The summary gives the maxima
  and minimum factor of safety with where they are.
- **Plots**: the mesh's boundary (each six-node facet as four triangles), displaced by a
  scale (automatic: the largest displacement shown at 5 % of the model's size, or true
  scale, or a number), split along iso-values into colour bands, one face per band and
  model face, so the viewport draws it with its usual lighting. SolidWorks' rainbow
  colours, a legend over the viewport, the maximum marked on the model.
- **Off the UI thread**: the surface meshes are made on the UI thread (the kernel is
  not shared), then meshing and solving run on a thread with progress and cancel.
- **Nothing in the documents**: the study (fixtures, loads, which faces) is held for the
  session in memory, by persistent face names so it survives rebuilds; the mesh and plot
  settings and any mechanical properties typed in are preferences, the latter keyed by
  material name, as the Render tab keeps finishes.

## 5. Open questions for the owner

**Answered (October 2026):** Netgen replaces the lattice mesher (LGPL-2.1, dynamically
linked, the licence text and the exact source shipped; the lattice mesher stays as the
fallback); studies are saved in a `.oxysim` document and results in a separate `.oxyres`
(5.1, 5.5); materials carry their mechanical properties (5.2, built: `mechanical` in
FORMAT.md §3); contacts as Fusion 360 has them: bonded, separation, sliding, rough and
offset bonded, found automatically between touching faces or set by hand (5.6). The
solver stays in house (5.4): the multigrid preconditioner made it fast enough. The
questions as they were asked:

1. **Studies in the files.** SolidWorks keeps studies in the part or assembly. Proposal:
   a `simulation.json` entry in `.oxyprt` and `.oxyasm` (studies by name, each with its
   type, fixtures and loads by persistent face name, gravity, mesh settings; results not
   saved, they are rebuilt by running). A format change: FORMAT.md, readers ignoring
   unknown entries as they already do.
2. **Mechanical properties on materials**: elastic modulus, Poisson's ratio, yield and
   tensile strength (and later thermal expansion and conductivity) as new optional keys of
   a material in `materials.json` and in the part's copy in `model.json`. Until then they
   are guessed from the material's name (a table of the starter library's materials and of
   families: steels, aluminium, plastics…) and can be typed in the Simulation panel, kept
   in the preferences.
3. **The mesher**: keep the lattice mesher only, or add **Netgen** behind a new `Kernel`
   method (a new C++ dependency, LGPL, built against our OpenCASCADE) for graded,
   feature-true, curved meshes? Gmsh and TetGen are ruled out by their licences unless the
   owner says otherwise.
4. **The solver**: add **faer** (pure Rust, MIT) for a direct sparse Cholesky, or keep the
   in-house conjugate gradients?
5. **Results on disk**: never (run again), a cache entry in the document, or an export
   (VTK `.vtu` for ParaView, which is plain XML and needs no dependency)?
6. **Contact**: bonded everywhere is the first step; is *no penetration* between
   touching parts wanted next, or connectors (bolts, pins) first?

## 6. What is built

Built without touching the file formats or adding a dependency, so every answer of §5
is still open:

- **`oxyde-sim`**, a new crate depending on nothing (ARCHITECTURE.md §2):
  - `material`: `Elastic` (modulus, Poisson's ratio, yield strength, density) and
    `Elastic::guess` from a material's name and category.
  - `mesh`: `TetMesh`, structured box meshes (for tests and checks), quadratic edge nodes,
    boundary facets with outward normals, volume, appending a mesh as further bodies.
  - `mesher`: the lattice mesher of §2 (Kuhn cells, exact boundary cuts against a
    bounding-volume hierarchy of the surface triangles, snapping of near-boundary points,
    stencils with conforming splits of quadrilateral faces), cancellable, deterministic;
    each solid on its own lattice by default, or all together.
  - `contact`: bonded, separation, sliding, rough and offset bonded contacts and their
    detection (§4), solved inside `solve`.
  - `solve`: assembly of the stiffness matrix in 3 × 3 blocks on every core, fixtures in
    local frames, consistent loads, preconditioned conjugate gradients, the rigid-body
    check, stress recovery, von Mises, principal stresses, factor of safety.
  - `amg`: the conjugate gradients' preconditioner, smoothed-aggregation algebraic
    multigrid (below).
  - `plot`: colour bands along iso-values on the displaced boundary, the rainbow colour
    map.
  - Tests: a bar in tension (displacement `PL/EA`, stress `P/A`, exact), a cantilever's
    tip deflection (`PL³/3EI`, within 3 %) and bending stress (`Mc/I`, within 5 %) with
    ten-node elements on a structured mesh, and within 12 % on the lattice mesher's own
    mesh; linear elements' stiffness in bending, rollers, pressure, a hanging bar under its
    weight, the rigid-body check, the mesher on a box, a cylinder and two touching boxes
    (volume, positive elements, a closed boundary, labels, determinism), colour bands.
    Contacts, on two blocks meshed apart with meshes that do not match: a cantilever cut
    in two and bonded bends as the whole one (0.02 % apart), offset bonded across a
    0.5 mm gap as bonded, and a 2 mm gap without offset is refused as not held; a block
    pressed onto another through a separation contact carries `P/A` (1 kN, 10 MPa within
    3 % at every interface node, the column shortening `σL/E`), and pulled off it parts
    (no pressure, the lower block unmoved, the upper one as if alone); sliding lets a
    block held elsewhere stretch as a free bar (`PL/EA` to 0.1 %, nothing taken by the
    interface); rough holds a block that sliding and separation let go (refused as not
    held), and pushed at mid-height it lifts off a quarter of the interface, as statics on
    a rigid base says; detection finds touching faces and gaps within the tolerance, not
    faces meeting at an edge; the mesher's own meshes of two touching blocks, found and
    bonded, give the one-piece cantilever's deflection within 0.5 %.
    Through OpenCASCADE (`oxyde-tests/tests/simulation.rs`): a cantilever part meshed from
    its export mesh, and the same beam as two bodies bonded where they touch, both against
    beam theory.
- **Speed**: the matrix product is limited by memory bandwidth, so more threads do not
  make block-Jacobi conjugate gradients faster (on 70 000 nodes: one thread 22 s, four
  15 s, sixteen 16 s); fewer iterations do. The preconditioner is therefore
  smoothed-aggregation algebraic multigrid (`amg`, Vaněk, Mandel and Brezina), written in
  house:
  - nodes are grouped greedily, in node order, with their strongly coupled neighbours (a
    block whose norm is at least 0.08 of the geometric mean of the two diagonal blocks');
  - the tentative prolongator maps each aggregate's six coarse unknowns onto its nodes
    through the six rigid-body motions, in each node's frame with held directions zeroed,
    orthonormalized aggregate by aggregate (a local QR whose `R` is the next level's rigid
    motions; a motion an aggregate cannot carry gets no coarse unknown);
  - one damped block-Jacobi step smooths it (`ω = 4 / 3λ`, λ the largest eigenvalue of
    `D⁻¹A` by power iterations); the coarse matrices are `Pᵀ A P` in 6 × 6 blocks, down to
    600 unknowns or fewer, factored by dense Cholesky;
  - a V-cycle with degree-2 Chebyshev smoothing on block-Jacobi (over `[λ/30, 1.1 λ]`),
    the same before and after the coarse correction, so the preconditioner is symmetric.
    The products run on every core, each thread owning rows, so results do not depend on
    the number of threads.

  The iteration count no longer grows with the mesh. The mesher's 20 × 20 × 200 mm
  cantilever, the whole solve (assembly, multigrid setup, iterations, stresses; a debug
  build, `oxyde-sim` being optimised in it), from
  `cargo test -p oxyde-sim --lib -- --ignored --nocapture solve_timings`:

  | Element size | Nodes | Block-Jacobi | Multigrid |
  |---|---|---|---|
  | 4 mm | 19 693 | 2 324 iterations, 2.2 s | 24 iterations, 0.48 s |
  | 2.5 mm | 68 533 | 3 566 iterations, 15.1 s | 24 iterations, 1.7 s |

  On 70 000 nodes the hierarchy has four levels (68 533, 2 873, 312 and 46 nodes) and
  takes 0.5 s to build; the 24 cycles take 1 s, a cycle costing about ten matrix
  products. Tip deflection and largest stress agree with block-Jacobi to six digits.
  Tried and not kept: Chebyshev degree 1 (37 iterations) or 3 (19) take the same time;
  a strength threshold of 0 (48 iterations) or 0.15 (256) is slower. If the hierarchy
  cannot be built, or has not converged in 500 iterations, block-Jacobi carries on from
  where it got to. A direct solve (faer, §5.4) would now matter only for much larger
  meshes or many load cases.

  **Memory.** A solve's peak memory grows with the nodes, measured on the lattice
  mesher's 100 mm cube held on one face and loaded on the other (every core, `oxyde-sim`
  optimised as in the app's debug build, the peak footprint from `/usr/bin/time -l`):

  | Element type | Nodes | Elements | Peak memory | Mesh + solve |
  |---|---|---|---|---|
  | ten-node | 156 153 | 106 032 | 2.7 GB | 0.1 + 4.8 s |
  | ten-node | 336 635 | 225 624 | 5.3 GB | 0.2 + 13.1 s |
  | four-node | 79 408 | 416 832 | 1.9 GB | 0.4 + 4.3 s |
  | four-node | 202 785 | 1 100 220 | 4.9 GB | 1.0 + 16.9 s |

  About 16 KB per node with ten-node elements and 24 KB with four-node ones (whose nodes
  are each in more elements); the Preferences' memory budget is turned into a node limit
  with 17 and 25 KB, and checked on Netgen's meshes as on the lattice mesher's.

  The automatic element size aims at about 2 800 lattice cells inside the model, at least
  two across its walls (from twice its volume over its area).

  Contacts cost a few more solves on the same hierarchy, each starting where the last one
  stopped. The same cantilever meshed as two blocks touching at mid-span and bonded by a
  detected contact, against one piece (`cargo test -p oxyde-sim --lib -- --ignored
  --nocapture contact_timings`):

  | Element size | Nodes | One piece | Two bodies, bonded contact |
  |---|---|---|---|
  | 4 mm | 19 586 | 24 iterations, 0.46 s | 2 solves, 41 iterations, 0.67 s |
  | 2.5 mm | 68 426 | 24 iterations, 1.6 s | 2 solves, 45 iterations, 2.6 s |

  The block tests above take 1 to 12 solves (12 for rough, with two changes of the
  matrix) and under 0.3 s each; the split cantilevers 2 solves and 0.7 s. A penalty of `10 E/h` took about twice the solves, `100 E/h` a third
  more iterations in all. The lattice mesher bevels each body's edges, so where two
  bodies touch it leaves a groove an element wide around the seam, whose nodes are too
  far from the other side to be tied (a third of the seam's nodes at 4 mm on a 20 mm
  face, a fifth at 2.5 mm); the deflection is 0.5 % off the one-piece mesh's. Netgen's
  meshes, true to the faces' edges, will not have it.
- **The Simulation tab** (`oxyde-app/src/simulation_ui.rs`), parts and assemblies:
  fixtures and loads made from the selected faces, the materials' mechanical properties
  (the material's own, typical values, or the study's override), contacts (found by *Run*
  or *Find Contacts* and bonded, their type set per pair, or made by hand from the
  selection), the mesher (Netgen, else the lattice mesher), mesh size and quality, *Run*,
  then von Mises or first principal stress, displacement, factor of safety or contact
  pressure drawn on the deformed model with a legend, and the mesh on its own. Results go
  stale, and are hidden, when the model changes.
- **Files** (`oxyde-app/src/simulation_files.rs`, `oxyde_format::simulation` and
  `results`, FORMAT.md §8–§9): the study saved as `.oxysim`, its results as `.oxyres`
  (written after every *Run* of a saved study), results checked against a fingerprint of
  the model and the study when opened.
- **Netgen in the tab**: first-order meshes from `Kernel::volume_mesh` made ten-node by
  midpoints, because the solver's ten-node elements are straight-sided (their geometry
  is taken from the corners). Curved second-order elements on the true surface need
  isoparametric elements in `oxyde_sim` (a Jacobian per integration point): the next step
  for curved parts. A Netgen cantilever is tested against beam theory
  (`oxyde-tests/tests/simulation.rs`).

Still to do, in the order of §3: probing, a report, mesh controls, frequency and buckling studies.

## 7. Studies and thermal studies

**Decided (October 2026):** the tab opens on *New Study*, as Fusion 360's does, and a model
has any number of studies, saved together in its `.oxysim`; thermal studies are steady,
transient and thermal stress, with every boundary condition of SolidWorks and Fusion
(fixed temperature, heat power and flux, convection, radiation, internal heat);
materials carry their thermal properties (`thermal` in FORMAT.md §3) and studies and
results files with a thermal study are `format_version` 2.

- **Several studies.** The open study stays where the tab's code expects it
  (`SimulationView::study`, `done`); the others wait in `others`, in order, each with its
  results, so switching keeps every study's results. New studies are numbered per type
  (`Static 1`, `Thermal 2`). Each study's results go to `<study file>-<study>.oxyres`.
- **The thermal solver** (`oxyde_sim::thermal`): `ρc ∂T/∂t = ∇·(k ∇T) + Q` on the static
  solve's four- and ten-node tetrahedra. Conduction is integrated as the stiffness is;
  the heat capacity `∫ ρc Nₐ N_b` exactly, from the integrals of barycentric monomials;
  boundary terms on the facets' triangles with a seven-point rule exact to degree 5.
  Fixed temperatures are eliminated (their columns move to the right-hand side).
  Convection adds `∫ h Nₐ N_b` and `∫ h T∞ Nₐ`. Radiation, `εσ (T⁴ − T∞⁴)`, is solved by
  Newton's method from 20 °C or more (convex in T, so the iterates come down steadily);
  its Jacobian `∫ 4εσ T³ Nₐ N_b` is symmetric and positive, so each Newton step is a
  conduction problem. Contacts reuse the static pairing with a conductance of `30 k / h`
  per area and the same augmented Lagrangian (a scalar multiplier per slave node), so
  heat crosses every contact as a perfect bond. Transient studies march by backward
  Euler, stable at any step, from a uniform temperature, keeping about a hundred evenly
  spaced sets of temperatures. The heat flux `−k ∇T` is averaged at the nodes as the
  stresses are. A steady study whose piece of the mesh has no fixed temperature,
  convection or radiation is refused (its temperature level is undefined).
- **Multigrid for temperatures.** `amg::Multigrid` takes the number of near-null modes:
  six rigid motions for elasticity, one (a uniform temperature, zero at fixed nodes) for
  conduction. The hierarchy is built once per matrix: once for a whole steady or
  transient study without radiation, once per Newton step with it.
- **Thermal stress.** The steady temperatures, then `solve_thermal_stress`: the load
  `∫ ∇Nₐ (3λ + 2μ) α ΔT dV` with ΔT interpolated at the integration points, and the
  same hydrostatic stress taken out of the recovered stresses, so a part free to grow is
  unstressed and a held one carries `−E α ΔT` along the held direction.
- **Tests**, against closed forms: a bar between two temperatures (linear profile, the
  flux `k ΔT / L` to 0.1 %), heat in at one end (`P L / kA`), internal heat (the parabola
  `Q x (L − x) / 2k`, exact on ten-node elements), convection in series with conduction,
  radiation balancing a heat input (`T⁴ = P / σA` to a millikelvin), a cooling bar's first
  two modes in a transient study (within 1 % with 200 steps), refusals (no anchoring, no
  heat capacity), heat across a contact between blocks meshed apart, 60 000 nodes in
  under 60 iterations; a heated bar free to grow (unstressed, grows `α ΔT L`) and held
  (`−E α ΔT`). Through OpenCASCADE: a bar part held at one end and convected at the other,
  in one body and in two bodies tied by a detected contact, against the series
  resistances.

## 8. Topology studies

**Decided (October 2026):** topology optimization is a study type, `topology`, as
SolidWorks has its topology study and Fusion 360 its shape optimization, saved in the
`.oxysim` with its target and preserved regions and its densities in the `.oxyres`
(`format_version` 3, FORMAT.md §8–§9). It has a tab of its own, *Topology*, next to
Simulation, which is the same set of studies showing the topology ones.

- **The problem:** least compliance (`Fᵀu`, the work of the loads) for a share of the
  model's mass, under a static study's fixtures, forces, pressures and contacts. Gravity is
  refused: the weight would change with the shape, and the usual scheme handles that badly.
- **SIMP** (`oxyde_sim::topology`): a design density per element, the stiffness the
  material's times `ε + ρ̃³ (1 − ε)`, `ε = 10⁻⁶`. Multigrid copes with that contrast: the
  cantilever of the tests takes about 15 conjugate gradient iterations per solve.
- **The density filter:** `ρ̃` is the design averaged over a sphere of radius `r`, weights
  `max(0, r − d) V`, so members are about `2r` thick at least. `r` is half the *thinnest
  member* typed, and 1.5 element sizes at least (the size from each element's volume, as a
  regular tetrahedron's edge), which is also the automatic radius. Neighbours are found in a
  grid of cells `r` wide, sorted by cell, so sums go in the same order on any number of
  threads; the filter is applied on the fly rather than stored, since a radius of two
  elements reaches some 300 neighbours.
- **The update:** optimality criteria, densities times `√(−∂c/∂ρ / λ ∂m/∂ρ)`, moving 0.2 at
  most, `λ` by bisection on its logarithm. The filtered mass is linear in the design, so its
  slopes are worked out once, and every update meets the target exactly. It stops when no
  density moves by 0.01, or after 15 iterations when the compliance has changed by less
  than 0.05 % over five with the mass on target, or at the iteration limit.
- **Kept elements:** those with a face on a fixture's, a load's or a preserved region's
  faces, and those whose centre is within a region's depth of its faces (exact distance to
  the faces' triangles, in a grid).
- **Manufacturing controls** (October 2026), as SolidWorks' topology study and Fusion
  360's shape optimization have them, set in the Target section and saved in the study
  (`symmetry`, `pull`, FORMAT.md §8):
  - *Symmetry*: up to three planes parallel to the standard planes through a point (the
    origin by default: the standard planes themselves). The density filter averages around
    each element's mirror images (every combination of the planes) as well as around its
    centre: `wᵢⱼ = Σ_g max(0, r − |g cᵢ − cⱼ|)`. A reflection moves both centres alike and
    undoes itself, so the weights stay symmetric and the same sum still carries the slopes
    back; and the filtered density is a function of position that is the same at mirror
    images, so the shape is mirrored on any mesh, the design densities left free. Tying
    each element's design density to the nearest element to its mirror image (groups
    followed through by union–find) was tried first: exact on a symmetric mesh, but on one
    that is not, nearest is not one-to-one, a group can hold two elements on one side and
    one on the other, and a beam whose stiffness does not care which side its material is
    on came out as lopsided as without symmetry. The nearest mirror image still mirrors the
    kept elements, and a plane across which less than half of the model (by volume) has
    one is refused, which catches a part not modelled about the origin. The filter costs
    as many neighbour sums as there are images (two to eight), small beside the solve.
  - *Pull direction*: a mould pulled off along a direction, one way (the shape standing on
    its far side) or both ways from a parting plane through a point. Each element's
    neighbour behind it is, among the elements about one element behind (and on the same
    side of the parting plane), the one closest to the line through its centre along the
    direction; after the filter each density is capped at its neighbour's, from the back
    to the front, so densities never grow going along the direction and nothing hangs over
    void. Each element's compliance slope goes to the element whose density it took. The
    mass is then linear in the design only while those stay the same, so its slopes and
    the design's budget are worked out again each iteration, and the mass comes to the
    target as the design settles. The elements behind kept ones are kept, so a kept element
    is never capped. Capping after the filter keeps the densities exactly monotone (capped
    before it and then filtered they would be only nearly so, the filter's normalisation
    at the model's faces breaking it); filling material in behind instead (a maximum from
    the front) was tried and shaped no better.
  - A pull across a symmetry plane is refused unless it goes both ways from that plane: a
    mould pulled one way cannot give a mirrored shape.
  - **Grey when pulled in the plane of bending.** Pulled along the direction the part bends
    in (a cantilever loaded down and pulled up), the densities stay grey: a grey column of
    density ρ is as stiff in bending as a solid block ρ of its height (both go as the cube),
    so SIMP has nothing to push them to solid or void, and a higher penalty (4, 5) did not
    get them out either. Pulled across the bending (through a plate's thickness, the usual
    casting), the shapes come out as crisp as without a pull. The density projection still
    to do should cure it.
- **Solves:** the static solver was split so that what does not change between iterations
  (fixtures' frames, contact pairs, loads, the matrix's pattern, element geometry) is
  prepared once (`solve::Prepared`), each iteration assembling the matrix with the elements
  scaled and starting from the last displacements. The element energies `uₑᵀKₑuₑ` are
  integrated as the stiffness is. A full solve first gives the whole part's compliance, to
  say what the material taken away costs.
- **The shape** (`oxyde_sim::shape`): the densities averaged at the corners by volume, cut
  at the threshold by marching tetrahedra (the triangles' way round taken from the edges'
  middles, which never fall together, so a node exactly on the threshold does not flip
  one), and the mesh's boundary facets clipped to the side above it, sharing the cut's
  points: a closed surface. Taubin smoothing (λ 0.5, μ −0.53, eight rounds in the
  application) moves the cut's points only, so the faces the shape keeps stay where the
  model's are. The application draws it with the boundary of the mesh at 12 % opacity
  around it, and writes it as binary STL.
- **Tests:** a 60 × 20 × 10 mm cantilever held at one end and loaded on a strip of the
  other keeps 40 % of its mass to 1 %, comes out about 3.4 times as flexible as the whole
  bar (the same mass spread evenly would be 15.6 times), its shape cut at half density
  holds 40 % of the volume to 6 %, and held and loaded elements stay solid; the same
  densities on one thread and on every core; preserved regions stay solid, and too much
  kept or a target out of range is refused; shapes are closed (every edge used once each
  way), full densities give the whole boundary and its volume exactly, and a cut through
  elements holds the solid's volume. With the controls: on a beam meshed symmetrically
  about its middle planes, held at both ends and loaded near one, symmetry about two planes
  gives densities equal to 10⁻⁹ where without it they differ by more than 0.1, and the
  loaded faces' mirror images stay solid; a plane the model is not symmetric about is
  refused; the cantilever pulled up, or both ways from its middle, has no element denser
  than the one behind it (without the pull some are by more than 0.2) and its mass on
  target to 2 %; a one-way pull across a symmetry plane and a zero direction are refused;
  the controls give the same densities on one thread and on every core; and on a real
  part, the beam meshed by the lattice mesher (not symmetric: its own centroid is 0.06 mm
  off the middle plane), symmetry and a two-way pull through its thickness put the shape's
  centre of mass within 0.1 mm of that plane (0.03 mm; 0.23 mm without symmetry, the
  beam's bending stiffness not caring which side its material is on) and no more mass in
  the outer half of the thickness than in the middle. An ignored test writes a finer cantilever's shape to
  an OBJ file (`OXYDE_TOPOLOGY_OBJ`): 36 864 four-node elements, 60 iterations in 30 s in a
  debug build (`oxyde-sim` optimised), the shape a hollow box section with openings near
  the root, as bending wants.
- **Speed** (`oxyde-tests/tests/topology_timings.rs`, ignored, on a real part; times
  taken while the application ran a study beside them, so on the slow side). The owner's
  Servo Arm, 45 × 12 × 6 mm with a 24-tooth spline, held at the hub and loaded at an end
  hole: Netgen at 1 mm makes 74 308 elements, graded down around the teeth (their mean edge
  0.29 mm). As ten-node elements, 122 170 nodes and about 7 s an iteration: 2.7 s to
  assemble the matrix and build the multigrid hierarchy, the rest 25 to 35 conjugate
  gradient iterations; the filter, the slopes and the update take 0.2 s. As four-node
  elements, 18 369 nodes and 1.5 s an iteration, settled after 35 iterations in 51 s. At
  0.3 mm, 352 484 elements: 524 137 nodes as ten-node elements (about 100 s an iteration, as
  the owner met it), 73 311 as four-node ones and 4 to 8 s an iteration (assembly 0.2 s,
  multigrid 1 s, the filter and the update 1 s). So topology studies are made with
  four-node elements by default (the panel warns when one is set to ten-node elements):
  the shape comes from the elements' densities, which they give as well. The iterations'
  solves stop at a residual of 10⁻⁵ of the loads instead of the static study's 10⁻⁸ (the
  whole part's solve keeps it): over four iterations at 0.3 mm, 148 conjugate gradient
  iterations instead of 187, 25 s instead of 33. Building the multigrid hierarchy once for
  several iterations would save most of the remaining second.
- **In one piece** (`oxyde_sim::pieces`, *Keep it in one piece*). Least compliance has no
  reason to join what carries no load: on the owner's Servo Arm, held at the hub and both
  right-hand holes and loaded at the far left one, 70 % of the mass kept, the ring preserved
  around the far right hole came out on its own (the hub and the nearer hole hold the load,
  and the spare 30 % was taken from where it mattered least). The shape cut at a threshold
  is in pieces exactly as its corners above the threshold are joined through the elements
  they share (the material above it in an element is convex, the density being linear
  there), so the pieces are a union-find over the corners, each with the faces it reaches.
  The panel names the fixtures, loads and preserved regions on each piece the loads do not
  reach, at the threshold shown. With *Keep it in one piece* (on for new studies; FORMAT.md
  §8 `one_piece`), once optimized, the shape cut at half density loses the pieces that reach
  no fixture, load or preserved region, and every other piece is joined to the largest one a
  load is on, the largest first: Dijkstra's through the elements' face neighbours, entering
  an element costing the material it lacks (plus a thousandth of its volume, so the shortest
  of the ways through material is taken), the path grown to the filter's radius (a bar as
  thick as the thinnest member), mirrored and shadowed along the pull like the shape. On the
  Servo Arm, one piece joined for 0.14 % more mass, by a bar 0.9 mm thick (the automatic
  thinnest member on a mesh graded down to the spline's teeth; a typed one makes it
  sturdier); 16 iterations in 27 s at 0.3 mm. Tests: pieces counted and what each reaches; a
  held block, a loaded one apart and a loose bit: the bit taken away, the blocks joined, one
  closed shape; a grey cantilever kept in one piece comes out in one piece with its held and
  loaded faces.

Still to do: a stress limit as well as a mass target, a projection of the densities for
crisper shapes (which should also make shapes pulled in the plane of bending solid or void),
a pull direction picked from a face or an edge (the file takes any vector, the panel offers
the axes), and promoting the shape into a part rather than exporting it.

## 9. Generative design

**Decided (October 2026, the owner):** a *Generative Design* tab like Fusion 360's
generative design workspace takes the Topology tab's place. Topology studies (§8) stay a
study type in the Simulation tab's *New Study*, as Fusion 360 keeps shape optimization in
its Simulation workspace, and documents holding them open as before. The first objective is
the stiffest shape for a mass target, each outcome's factor of safety reported; the lightest
shape for a factor of safety comes next. An outcome becomes a new part (a faceted solid, the
preserve bodies exact) or a mesh file. Studies are saved as a `generative` type in the
`.oxysim` and their outcomes in its `.oxyres` (FORMAT.md §8–§9, `format_version` 4).

**Why topology studies were not it.** A topology study carves the part that was modelled:
the shape stays inside it, and at half its mass it reads as that part with holes in it.
Generative design starts from what has to be there and nothing more (the faces things bolt
to, the room other things need, the space between them empty) and grows material through
that space. The branching, organic shapes Fusion 360 is known for come from that: a space
much larger than what is kept (a tenth of it or less), solved on a fine grid, cut by a
smooth surface.

**What the user sets up**, as in Fusion 360, whose help is the reference for names and
behaviour:

- **The design space**, from bodies of the part, each given a role and drawn in its colour.
  *Preserve* bodies (green) are kept as they are: bosses, rings round bolt holes, pads.
  *Obstacle* bodies (red, see-through) are where no material may go: bolts, a shaft, the
  room a tool or a neighbouring part needs, each with an *offset* that grows it. A *Starting
  Shape* body (yellow, optional) is where the material starts. Bodies with no role are left
  out. Without a starting shape, material may go anywhere in the box around the preserve
  bodies, grown on every side by a margin (a tenth of the box's largest side by default);
  with one, only inside it. Fusion 360 starts from a bounding box too, or from the starting
  shape instead; its help does not say whether outcomes may leave the starting shape, and
  keeping them inside makes it a design space one can model. Obstacles only take material
  away, so they should reach past the box (Fusion's help gives the same advice).
- **Design conditions:** *Fixed Geometry*, *Roller/Slider*, *Force* and *Pressure*, as in a
  static study, on faces of preserve bodies: Fusion 360 asks for them there too, and
  *Generate* refuses a fixture or a load elsewhere. No gravity, as in topology studies.
  Load cases (several sets of loads, the stiffest shape for all of them) come later.
- **Objective and limits:** *Maximize stiffness* for a *mass target* in kg: the whole
  outcome's mass, preserve bodies included, the same for every material (Fusion 360's
  target is a mass too, not a share). A target under the preserve bodies' mass or over the
  whole design space's is refused. A *safety factor* (2 by default) that each outcome's
  estimated factor of safety is held against in the results; it becomes a limit with
  *Minimize mass*.
- **Manufacturing:** one or more methods, each with outcomes of its own. *Unrestricted*.
  *Additive*, with a build direction (+X, +Y, +Z, −X, −Y, −Z, any of them, a set of outcomes
  for each, as in Fusion 360) and an overhang angle (45°, the only one at first). *2-axis
  cutting* along X, Y or Z: the shape the same all along it, a profile a water jet or a laser
  cuts. *Casting*, with an ejection direction, one way or both ways from a parting plane
  through a point (the topology studies' pull direction). For the whole study, the *minimum
  thickness* and *symmetry* planes, as in topology studies. Milling (2.5, 3 and 5 axes, with
  tool directions and diameter) comes later. Directions are the axes for now, since the grid
  is aligned with them.
- **Materials:** one or more, copied from the library into the study; every material is
  tried with every method and direction, each pair an *outcome*. None listed: the part's
  own.
- **Settings:** the resolution (the grid's voxel size, from coarse to fine as a mesh's
  density is, or typed in mm) and the iteration limit.

*Generate* runs the outcomes one after another in the background on this computer (Fusion
360 sends them to Autodesk's servers), each shown as it finishes; *Stop* keeps the finished
ones. *Explore* lists them as cards with a picture, the material and method, the mass, the
largest displacement and the estimated factor of safety, sortable, beside a plot of mass
against displacement. The outcome picked is drawn in the viewport with a slider through its
iterations. *Create Part* makes a new part of it; *Export* writes it as STL, OBJ or 3MF.

**How it works** (`oxyde_sim::generative`, pure Rust, no new dependency):

- **The grid.** The design space's box cut into cubic voxels, placed so that the symmetry
  planes fall between voxels, which makes the mirror exact. Each voxel is *preserve* (its
  centre inside a preserve body, or a fixture's or a load's face passing through it),
  *obstacle* (its centre inside an obstacle grown by its offset: never material), *outside*
  (out of the starting shape: never material) or *design*. Inside is decided from the bodies'
  tessellations by the lattice mesher's parity test (`mesher.rs`). An obstacle over a
  preserve body is refused.
- **The elements:** eight-node bricks, trilinear and all alike, so one 24 × 24 stiffness per
  material, scaled for each voxel by `E_min + ρ̄³ (E − E_min)`, `E_min` a millionth of `E`
  (SIMP, as in §8). No matrix is assembled at the finest level: each brick's product is
  worked out once, by the columns of its stiffness (the rows' sums side by side, which the
  compiler vectorises), into a buffer kept from one product to the next, then summed at each
  node in the order of its corners, so the sums are the same on any number of threads.
  Gathered node by node instead, each node working out its own rows of the eight bricks
  around it, a product took twice as long. Memory goes as a few numbers per voxel, not as a
  matrix's nonzeros. Void voxels stay bricks, at a millionth of the stiffness. Making them
  springs at their corners (their bricks' diagonals) instead, to skip their products, was
  tried: springs hold to the ground, a void continuum does not, and with two hundred
  thousand of them the optimizer spread specks of material through the box near the
  loaded ring to lean on them, a load path to nowhere as stiff as the bracket itself, at
  twice the mass target.
- **The solve:** conjugate gradients to a residual of 10⁻⁵ of the loads, preconditioned by
  geometric multigrid on the grid's own levels, each halving the one above along every axis
  with two voxels or more, down to a couple of hundred nodes factored densely; each
  optimization iteration starts from the last displacements. The coarse levels are Galerkin
  products, exact for nested trilinear grids: each coarse cell's 24 × 24 matrix is its eight
  children's seen through the interpolation, worked out one coarse cell at a time and kept in
  single precision (it only preconditions). They carry the fixtures down with no special
  case; averaged moduli were not tried. Fixtures project: a held node keeps only the motions its fixtures allow (`P`),
  the operator is `P K P + d (I − P)`, `d` the node's own stiffness (so the coarse levels
  see the held directions at the right scale), and conjugate gradients stay in the allowed
  motions. Smoothing is Chebyshev's on block Jacobi, degree 2, the finest level's interval
  bounded by the brick's own (no assembly of bricks has a larger top of `D⁻¹K` than one
  brick), the coarse levels' found by 20 power iterations from a pseudo-random start, with
  a fifth to spare. From a smooth start, a mode held to a few nodes can be missed: with
  coarse cells over void left out (tried with the springs above), one was, the smoother
  amplified it, and conjugate gradients went from about 40 iterations to hundreds.
- **Fixtures and loads** act through samples of their faces every half voxel. Each sample
  lands in the voxel holding it, made a preserve voxel (a thin preserve body has no voxel
  centre in it), or, in an obstacle voxel, in the nearest preserve voxel around. A fixture
  holds that voxel's corner nearest the sample (a roller only along the face's normal, two
  directions where its faces meet at an angle); a load spreads its sample's share of the
  force, by area, over the voxel's corners by trilinear weights.
- **The optimization:** §8's density filter over the voxels that are not void (its radius
  half the minimum thickness, 1.5 voxels at least; across the cut only for 2-axis cutting,
  so whole columns stay alike), then a smoothed Heaviside projection, `η` 0.5 and `β` from 1,
  doubled when no design density moves by 0.01 or after 25 iterations, up to 32, so that the
  shape comes out solid or void, not grey. Optimality criteria as in §8 (moves of 0.2, then
  0.1 from `β` 8; design densities kept over 0.001 so that any can come back), the mass
  target met by the projected densities, linearised at each iteration: it is met once the
  design settles. The design starts uniform at the density whose projection weighs the
  target. Settled, at `β` 32: no design density moving by 0.01, or, as the densities go back
  and forth at the move limit once the shape is crisp, the compliance steady to 0.1 % over
  ten iterations with the mass on target to 1 %. The projection is also what §8's shapes
  pulled in the plane of bending lack.
- **Manufacturing on the grid:** symmetry ties each voxel's design to its mirror images (one
  value, the slopes summed); 2-axis cutting keeps one value per column along the direction;
  casting caps each voxel at the one behind it along the direction (§8's pull, the
  neighbour exact on a grid); additive applies Langelaar's filter (2016, 2017): a layer is
  printed only where the 3 × 3 voxels under it hold it up, smoothly, with its adjoint for the
  slopes, and the first layer stands on the build plate.
- **Each outcome** keeps its densities at the end and every tenth iteration (one byte per
  voxel, as the files keep them: the densities before the projection, which the surface is
  cut from, below), its history (compliance, mass, largest change), its mass,
  its compliance (the work of the loads on the displacements solved) and largest
  displacement, and an estimate of its largest von Mises stress and smallest factor of
  safety at the centres of its solid voxels, leaving out those the fixtures and loads act
  through, where stress is singular. Voxel corners concentrate stress, so the estimate is
  rough: *Create Part* and a static study give the exact one.
- **The surface:** the densities before the projection cut at half (where the projected
  ones are half too: the projection keeps half where it is) by marching tetrahedra on the
  lattice through the voxels' centres, a layer of empty ones all around, each cell cut along
  its Kuhn split (no ambiguous case, a closed surface, as §8's shape). The projected
  densities are all but 0 or 1, so a surface cut from them passes midway between every
  solid voxel and its void neighbour and steps from voxel to voxel, which twelve rounds of
  smoothing did not hide; before the projection they vary smoothly over the filter's
  radius and place it between the centres. On the lattice through the centres a member one
  voxel thin keeps its place; cut at the voxels' corners (densities averaged there, as §8
  does on its elements), such a member vanished, every corner of it at half, and a test
  bracket's surface held half its material. The threshold, 127.5, is no byte, so no
  triangle is flat. Then Taubin's smoothing, every point free. The preserve bodies are drawn
  exact over it.
- **Create Part:** the surface decimated (quadric edge collapse) to as many triangles as the
  kernel handles well, sewn into a solid by a new `Kernel` operation (a solid from a closed
  triangle mesh, ARCHITECTURE.md §4), fused with the exact preserve bodies, the obstacles cut
  away, and written as STEP into a new part's *Imported* feature, kept as an imported STEP
  file is: no change to `.oxyprt`. If the fuse fails, the outcome and the preserve bodies
  stay separate bodies of the part.

**Order of the work**, each step a commit with its tests and documents:

1. The engine: the grid from bodies, the bricks, multigrid, SIMP with the filter and the
   projection, the controls on the grid (symmetry, cutting, casting), the surface. Tests in
   `oxyde-sim` (grids from boxes) and `oxyde-tests` (a bracket whose preserve bosses and
   obstacles are modelled through OpenCASCADE), and an ignored timing test that sets the
   resolution's range and picks the coarse levels.
2. The files: the `generative` study and its results (FORMAT.md §8–§9, version 4), with
   round trips.
3. The tab: the Topology tab becomes Generative Design; setting up, *Generate*, *Explore*
   with its plot, *Export*.
4. *Create Part*.
5. Additive, and load cases.
6. *Minimize mass* for a safety factor (a stress limit aggregated by a p-norm, the method of
   moving asymptotes in place of optimality criteria), a displacement limit, milling, and
   assemblies.

**Built (October 2026): step 1**, the engine, `oxyde_sim::generative` (`Grid::build`,
`generate`, `surface`, `voxel_size`), not yet in the application.

- **Tests** in `oxyde-sim`: the brick (rigid motions cost nothing, a uniform strain what the
  material says); the operator (its cells' matrices summed, symmetric, a roller and a nearly
  void cell among them); conjugate gradients against a dense solve, and, with a slab of
  nearly void voxels every fourth, in under 80 iterations and the same on one thread and on
  eight; grids from boxes (pads and an obstacle grown by its offset sorted to the voxel, a
  box made symmetric about a plane off its middle, a starting shape, the held face's nodes,
  the load's forces adding up to it); studies set up wrong refused (a face on no preserve
  body, an obstacle in a preserve body, nothing held, too many voxels, a target out of
  reach, 2-axis cutting with a preserve body and an obstacle in line, a one-way pull across
  a symmetry plane); a bracket of two pads grown to its mass target to 2 %, stiffer than it
  started, its pads joined, its preserve voxels solid, its surface closed and holding the
  volume its mass says to 15 % (a grey design would not); the same outcome on one thread
  and on eight; symmetry exact to the byte, 2-axis cutting the same all along each column,
  casting never denser in front (one way and both ways); surfaces closed, holding a block's
  volume. In `oxyde-tests` (`generative.rs`), a bracket modelled through OpenCASCADE as
  three bodies of a part, two rings and a post between them, the bores held and loaded:
  the outcome on target to 3 %, nothing in the post, the rings solid and joined.
- **Timings** (`generative_timings`, ignored, the same bracket, 0.12 kg of steel to keep;
  Ryzen 7 5800X, 16 threads, a debug build with `oxyde-sim` optimised):

  | Resolution | Voxels | An iteration | Conjugate gradient iterations a solve | Outcome |
  |---|---|---|---|---|
  | 0 | 21 840 of 2.27 mm | 0.7 s | 50 | 150 iterations in 103 s |
  | 0.5 (default) | 211 120 of 1.06 mm | 5.4 s | 80 | settled after 148 iterations, 13 min |

  The mass stays on target throughout, and the shapes are what generative design gives: on
  the bracket at 0.5, a curved web from the held ring sweeping round to the loaded one,
  ribbed, open where the post passes. The solves take about 25 conjugate gradient
  iterations at first and more as the shape sharpens and solid stands next to void. Void
  voxels cost as much as solid ones, most of the box by the end: leaving them out of the
  solve without grounding them (a weak operator that leaves translations free, unlike the
  springs) is what would pay most, then cheaper coarse levels (node stencils instead of a
  matrix per cell).
