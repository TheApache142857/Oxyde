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

Still to do, in the order of §3: the owner's decisions of §5, then probing, a report, mesh controls and the other study types.
