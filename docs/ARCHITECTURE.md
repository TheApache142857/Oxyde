# Oxyde architecture

Status: **draft**. This document records design decisions; change it in the same pull request as any code that deviates from it.

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
| `oxyde-tests` | `model`, `occt` (dev only) | — |

`oxyde-app` is the only place where a concrete kernel backend is chosen. `oxyde-tests` holds end-to-end tests that drive the model through OCCT, so neither crate has to depend on the other.

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

Implemented (see `crates/oxyde-kernel/src/lib.rs` for the documented signatures):

| Group | Methods |
|---|---|
| Construction | `make_box`, `extrude(plane, profile, vector)`, `boolean(Fuse/Cut/Common, target, tool)`, `fillet(shape, edges, radius)`, `chamfer(shape, edges, distance)` |
| Queries | `topology` (faces, edges, vertices with adjacency, surface/curve kind, area, centroid, planar normal, edge midpoint), `volume`, `topology_counts` |
| Exchange | `export_step(shapes, path)` |
| Lifetime | `release` |

Every construction operation returns an `OpResult { shape, history }`. The history lists, **for each face of the result**, where it came from:

| `FaceSource` | Meaning |
|---|---|
| `InputFace { input, face }` | an input face, unchanged, trimmed, split or merged |
| `InputEdge { input, edge }` | generated from an input edge (fillet / chamfer face) |
| `InputVertex { input, vertex }` | generated from an input vertex (corner blend) |
| `ProfileSegment(id)` | side face swept from the sketch segment with that id |
| `StartCap` / `EndCap` | the two ends of an extrusion |

The kernel never remembers anything between calls; turning this history into lasting names is the model's job (§6). Still to come: `revolve`, `shell`, patterns, `tessellate` (for the viewport), STEP import, BREP import/export (for the `.oxy` geometry cache).

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

### Timeline (implemented in `part.rs`)

- Oxyde is **bottom-up** like SolidWorks: parts are modeled on their own; assemblies (§8) place finished parts and never reach inside their timelines.
- `Part` holds the ordered features, the rollback bar and suppression flags. `Part::timeline()` returns one row per feature with its status (`Ok`, `Warning`, `Error`, `Suppressed`, `RolledBack`, `Pending`) — the data behind the timeline panel.
- New features are inserted at the rollback bar. Deleting a feature that others refer to is refused.
- `Part::rebuild(kernel)` replays from the first edited feature only. After each feature it keeps the resulting `Body`: kernel shape, topology and the names of every face, edge and vertex. A feature that fails is marked `Error` and leaves the body unchanged; later features still build if their references exist.
- Features implemented: sketch (on Front/Top/Right or on a planar face), boss/cut extrude (blind, mid-plane, reversible), fillet, chamfer. Sketch geometry is explicit until the solver (M2) lands.

### Persistent naming (implemented in `naming.rs`)

The problem: a fillet stores "edge #17"; after an upstream edit, numbering changes and the fillet attaches to the wrong edge or fails. FreeCAD spent years on this; we designed for it from day one.

1. **Faces are named by provenance**, from the kernel history:
   - `F2.side[3]`: side face that feature F2 swept from sketch segment 3;
   - `F2.start` / `F2.end`: the two caps of extrusion F2;
   - `F5.blend(edge(…))`: fillet face that F5 made from a named edge.

   A face that a later operation only trims, splits or merges **keeps its name**, so the top face of a block is still `F2.end` after holes are cut into it. When a boolean merges faces, the name from the oldest feature wins, so existing references stay valid.
2. **Edges are named by the faces they separate**, vertices by the faces meeting at them: `edge(F2.side[2] | F2.end)`. This needs no extra history from the kernel and is stable whenever the faces are.
3. **Duplicates get a `part` ordinal** (`F2.end@1`, `F2.end@2` when a slot splits the top face), assigned in a deterministic geometric order.
4. **Resolution** of a stored name against a rebuilt body: exact match, else a unique loose match ignoring part ordinals (built with a warning), else `Ambiguous` or `Lost` (feature in error, asking the user to reselect).
5. Names reference features by `FeatureId`, never by display name, so renaming is free. Feature ids are never reused.
6. Sketches on faces get a deterministic frame (origin = projection of the model origin, x axis = least-aligned model axis), so they land in the same place after the face is resized.

Known limits, to address later: unknown-origin faces (none expected from current operations) get index-based names; when pieces of a split face swap geometric order, their ordinals swap too.

## 7. Units and tolerances

- Internal length unit: **millimetre**, `f64`. Angles in radians internally, degrees in UI.
- Display units are a per-document preference; conversion happens only at the UI and file boundary.
- One central tolerance policy in `oxyde-geom` (linear `1e-6 mm`, angular `1e-9 rad` as starting values) passed to the kernel explicitly.

## 8. Assemblies and drawings (later)

- Bottom-up: an assembly only places parts (and sub-assemblies) and mates them; editing a part happens in the part's own timeline.
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
| **Engine** (code written, awaiting first Windows build) | Kernel history + topology; part timeline with rollback/suppression; persistent naming; sketch/extrude/cut/fillet/chamfer; end-to-end tests in `oxyde-tests` |
| **M2** | Sketch entities + solver with DOF reporting (pure Rust, no UI) |
| **M3** | Revolve, `.oxy` save/load (feature tree + names) |
| **M4** | Viewport + sketcher UI; selection; fillet / chamfer with persistent naming |
| **M5** | Patterns, mirror, shell, reference geometry, expressions |
| **M6** | Assemblies with basic mates; BOM export |
| **M7** | Drawings: views, dimensions, PDF/DXF |
