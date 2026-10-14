# Oxyde architecture

Status: **draft**. This document records design decisions before code exists; change it in the same pull request as any code that deviates from it.

## 1. Guiding principles

1. **Rust owns the application; C++ owns only the geometry kernel.** All modeling logic — sketches, constraints, the feature tree, naming, file I/O, UI — is Rust. C++ exists solely in a thin shim around OpenCASCADE (OCCT).
2. **The kernel is replaceable.** Everything above `oxyde-kernel` talks to a Rust trait, never to OCCT. A future pure-Rust kernel (in-house, or one built on `truck`) must be a drop-in backend.
3. **The model is data; geometry is a cache.** A part is its feature tree. B-rep solids are derived by replaying that tree and may be discarded and rebuilt at any time.
4. **Rebuilds are deterministic.** Same feature tree + same kernel version ⇒ same topology, same persistent names.
5. **Open formats in and out.** Native files are documented (`FORMAT.md`); exchange uses STEP and friends.

## 2. Crates and dependency rules

| Crate | Depends on | Must never depend on |
|---|---|---|
| `oxyde-geom` | — | anything else in the workspace |
| `oxyde-sketch` | `geom` | `kernel`, `occt` |
| `oxyde-kernel` | `geom` | `occt`, `model` |
| `oxyde-occt` | `geom`, `kernel` | `model`, `sketch`, `app` |
| `oxyde-model` | `geom`, `sketch`, `kernel` | `occt`, `app` |
| `oxyde-format` | `model` | `occt`, `app` |
| `oxyde-app` | all of the above | — |

`oxyde-app` is the only place where a concrete kernel backend is chosen.

## 3. The Rust ⇄ C++ boundary (`oxyde-occt`)

```
crates/oxyde-occt/
├── build.rs        # locates OCCT, compiles cpp/ with the cxx bridge
├── cpp/
│   ├── shim.h      # coarse-grained C++ API, no OCCT types in signatures
│   └── shim.cpp    # implementation against OCCT
└── src/
    ├── ffi.rs      # #[cxx::bridge] declarations
    └── lib.rs      # impl oxyde_kernel::Kernel for OcctKernel
```

Rules:

- **No OCCT type crosses the boundary.** Shapes live in a C++-side arena; Rust holds opaque integer handles (`ShapeHandle(u64)`). Data crosses as plain structs (`f64` arrays, enums, strings).
- **Coarse operations only.** The shim exposes on the order of 50 functions (make prism, revolve, boolean, fillet, shell, tessellate, read/write STEP, …), not OCCT's API surface.
- **Every operation returns its history.** OCCT's `BRepBuilderAPI_MakeShape::Generated / Modified / IsDeleted` results are returned alongside the new shape. This is the raw material for topological naming (§6).
- **No C++ exceptions escape.** Each shim function catches `Standard_Failure` (which does not derive from `std::exception`) and everything else, and rethrows a `std::runtime_error` that `cxx` turns into a Rust `Err`.
- **Thread confinement.** All OCCT calls run on one dedicated kernel thread; the Rust side talks to it through a channel. This sidesteps OCCT's uneven thread-safety and keeps the UI responsive during long rebuilds. *(Not yet implemented: since M1 `OcctKernel` is simply `!Send`; the kernel thread arrives with the application.)*

Toolchain: `cxx` / `cxx-build` for the bridge and to compile the shim. **Windows/MSVC only for now.** `build.rs` locates OCCT through `OXYDE_OCCT_DIR`, `VCPKG_ROOT`, or explicit `OXYDE_OCCT_INCLUDE_DIR` + `OXYDE_OCCT_LIB_DIR` — see `BUILDING.md`.

## 4. The `Kernel` trait (`oxyde-kernel`)

The trait grows with each milestone. **M1** contains only `make_box`, `volume`, `topology_counts`, `export_step` (to a file path) and `release`. The target shape is indicative and will be refined as it is implemented:

```rust
pub trait Kernel {
    type Error: std::error::Error;

    // Construction
    fn extrude(&mut self, profile: &Profile, dir: Vec3, extent: Extent) -> OpResult<Self::Error>;
    fn revolve(&mut self, profile: &Profile, axis: Axis, angle: f64) -> OpResult<Self::Error>;
    fn boolean(&mut self, op: BoolOp, a: ShapeHandle, b: ShapeHandle) -> OpResult<Self::Error>;
    fn fillet(&mut self, shape: ShapeHandle, edges: &[EdgeRef], radius: f64) -> OpResult<Self::Error>;
    fn chamfer(&mut self, shape: ShapeHandle, edges: &[EdgeRef], distance: f64) -> OpResult<Self::Error>;
    fn shell(&mut self, shape: ShapeHandle, removed_faces: &[FaceRef], thickness: f64) -> OpResult<Self::Error>;

    // Queries
    fn topology(&self, shape: ShapeHandle) -> Topology;          // faces/edges/vertices + adjacency
    fn mass_properties(&self, shape: ShapeHandle) -> MassProps;
    fn tessellate(&self, shape: ShapeHandle, tol: Tolerance) -> Mesh;

    // Exchange
    fn import_step(&mut self, bytes: &[u8]) -> Result<Vec<ShapeHandle>, Self::Error>;
    fn export_step(&self, shapes: &[ShapeHandle], path: &Path) -> Result<(), Self::Error>;
    fn export_brep(&self, shape: ShapeHandle) -> Result<Vec<u8>, Self::Error>;
    fn import_brep(&mut self, bytes: &[u8]) -> Result<ShapeHandle, Self::Error>;

    fn release(&mut self, shape: ShapeHandle);
}

/// New shape plus the history mapping input sub-shapes to output sub-shapes.
pub type OpResult<E> = Result<(ShapeHandle, OpHistory), E>;
```

A second, trivial backend (`NullKernel` / mock) lives in tests so that `oxyde-model` can be tested without OCCT installed.

## 5. Sketches and the constraint solver (`oxyde-sketch`)

- Entities: point, line, arc, circle, ellipse, B-spline; construction flag per entity.
- Constraints: coincident, horizontal, vertical, parallel, perpendicular, tangent, equal, symmetric, distance, angle, radius/diameter, fixed, point-on-curve, midpoint.
- Solver: assemble residuals into a nonlinear system and solve with damped Newton / Levenberg–Marquardt; decompose into independent clusters first so large sketches stay fast.
- Report **degrees of freedom** per entity (under / fully / over-constrained) and identify the conflicting constraints when over-constrained — this drives the sketch colouring in the UI.
- The sketcher is pure Rust and kernel-independent; it outputs closed profiles (`Profile`) consumed by the kernel.

## 6. Feature tree and topological naming (`oxyde-model`)

### Feature tree

- A part is an ordered list of features (sketch, extrude, revolve, cut, fillet, chamfer, shell, pattern, mirror, reference plane/axis…), each with typed parameters and references.
- Features form a dependency DAG; editing one feature marks it and its dependants dirty, and only those are rebuilt.
- A rollback bar and per-feature suppression are model concepts, not UI tricks.
- Parameters can be expressions over named global variables (`width * 2 + 5 mm`).

### Persistent naming

The problem: a fillet stores "edge #17"; after an upstream edit, edge numbering changes and the fillet attaches to the wrong edge or fails. FreeCAD spent years on this; we design for it from day one.

Approach:

1. Every sub-shape produced by a feature gets a **persistent name** built from *provenance*, not index:
   `feature_id / role / source_name` — e.g. "face generated by extrude #3 from sketch edge `s2:line7`", "lateral face", "top cap".
2. Each kernel operation returns `OpHistory` (§3); the model propagates names through *modified* sub-shapes and mints new names for *generated* ones.
3. When a reference no longer resolves exactly, fall back to **geometric matching** (surface type, normal, centroid, area within tolerance) against the previous rebuild, and flag the feature with a warning rather than silently guessing.
4. Names are stored in the file, so references survive save/load.

## 7. Units and tolerances

- Internal length unit: **millimetre**, `f64`. Angles in radians internally, degrees in UI.
- Display units are a per-document preference; conversion happens only at the UI and file boundary.
- One central tolerance policy in `oxyde-geom` (linear `1e-6 mm`, angular `1e-9 rad` as starting values) passed to the kernel explicitly.

## 8. Assemblies and drawings (later)

- Assemblies reference parts by relative path + document UUID; mates are solved by a 3D constraint solver sharing infrastructure with `oxyde-sketch`.
- The assembly tree is the BOM source: quantities, part numbers and custom properties come straight from each part's `properties.json` (see `FORMAT.md`).
- Drawings: views computed by the kernel's hidden-line removal (OCCT `HLRBRep`), annotations and dimensions in Rust, export to PDF/SVG/DXF.

## 9. Application (`oxyde-app`)

- UI toolkit: `egui` with a `wgpu` viewport (to be confirmed by a small prototype).
- Rendering consumes tessellated meshes from the kernel; selection maps triangles back to persistent face/edge names.
- Undo/redo operates on the feature tree (model commands), never on geometry.

## 10. Roadmap

| Milestone | Content |
|---|---|
| **M0** | Workspace, docs, CI (format, clippy, tests) |
| **M1** (code written, awaiting first Windows build) | `oxyde-occt` shim builds on Windows; make a box, check volume/topology, export STEP from a test |
| **M2** | Sketch entities + solver with DOF reporting (pure Rust, no UI) |
| **M3** | Feature tree: sketch → extrude / cut / revolve, rebuild, `.oxy` save/load |
| **M4** | Viewport + sketcher UI; selection; fillet / chamfer with persistent naming |
| **M5** | Patterns, mirror, shell, reference geometry, expressions |
| **M6** | Assemblies with basic mates; BOM export |
| **M7** | Drawings: views, dimensions, PDF/DXF |
