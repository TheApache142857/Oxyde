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
| `oxyde-format` | `geom`, `model` | `occt`, `app` |
| `oxyde-render` | `kernel` (mesh types only) | `occt`, `model`, `app` |
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
| Construction | `make_box`, `extrude(plane, profile, vector)`, `revolve(plane, profile, axis, angle)`, `boolean(Fuse/Cut/Common, target, tool)`, `fillet(shape, edges, radius)`, `chamfer(shape, edges, distance)`, `shell(shape, removed faces, signed offset)`, `transform(shape, rigid motion or mirror)` |
| Queries | `topology` (faces, edges, vertices with adjacency, surface/curve kind, area, centroid, planar normal, axis of a surface of revolution, line or circle axis of an edge, edge midpoint), `tessellate` (display mesh: triangles per face, polyline per edge, indexed like the topology), `volume`, `topology_counts` |
| Exchange | `export_step(shapes, path)` |
| Lifetime | `release` |

Every construction operation returns an `OpResult { shape, history }`. The history lists, **for each face of the result**, where it came from:

| `FaceSource` | Meaning |
|---|---|
| `InputFace { input, face }` | an input face, unchanged, trimmed, split or merged |
| `InputEdge { input, edge }` | generated from an input edge (fillet / chamfer face) |
| `InputVertex { input, vertex }` | generated from an input vertex (corner blend) |
| `OffsetFace { input, face }` | offset copy of an input face (inner wall of a shell) |
| `ProfileSegment(id)` | side face swept (extruded or revolved) from the sketch segment with that id |
| `StartCap` / `EndCap` | the two ends of an extrusion, or of a revolve of less than a full turn |

`transform` reports every face as the `InputFace` it is a copy of, so pattern instances can be named after the seed's faces. The kernel never remembers anything between calls; turning this history into lasting names is the model's job (§6). Still to come: STEP import, BREP import/export (for an optional `.oxy` geometry cache).

A second, trivial backend (`NullKernel` / mock) lives in tests so that `oxyde-model` can be tested without OCCT installed.

## 5. Sketches and the constraint solver (`oxyde-sketch`)

Implemented (M2, solver only):

- **Entities**: points, lines, circles and counter-clockwise arcs. Lines and arcs refer to point entities, so connected geometry shares its points as in SolidWorks. Construction geometry is solved but left out of profiles.
- **Relations and dimensions**: fixed, coincident, horizontal, vertical, parallel, perpendicular, equal length, equal radius, point–point distance (true, horizontal, vertical), length, radius, angle, point on line, point on circle, midpoint, tangent (line–circle/arc). `rectangle()` adds its horizontal/vertical relations automatically.
- **Solver**: unknowns are point coordinates and circle radii (fixed points are not unknowns; arc radii are implied). Each relation contributes equations `r(x) = 0`. Steps are minimum-norm Gauss–Newton steps `Δ = −Jᵀ(JJᵀ + μI)⁻¹r`, so an under-defined sketch moves as little as possible (the right behaviour when dragging), with `μ` raised towards gradient descent when a step does not reduce the error, and a backtracking line search. The Jacobian is numerical (central differences), ample for sketch-sized systems.
- **Analysis**, from the Jacobian at the solution:
  - **degrees of freedom** = unknowns − rank;
  - an entity is **fully defined** (black) when no remaining motion (null space of the Jacobian) moves any of its parameters, otherwise under-defined (blue);
  - relations are taken in order: one adding no independent equation is **redundant**, one left unsatisfied is **conflicting**. A failed solve leaves the geometry untouched.
- `profile_segments()` turns the solved sketch into profile curves whose segment ids are the entity ids, so face names (`F2.side[7]`) follow sketch entities.

Relations have a stable `RelationId`. Dimensions are named `D1`, `D2`… in creation order, as in SolidWorks, and can be renamed. Entity ids, relation ids and default-name numbers come from counters saved with the sketch, so nothing is ever reused after a deletion.

In the model (M2b): a `Sketch` feature holds a `ConstrainedSketch`, solved during every rebuild. Conflicting relations put the sketch in error; redundant ones give a warning; an under-defined sketch is flagged in the timeline (`(-) Sketch1`, as in SolidWorks) but builds normally.

## 6. Feature tree and topological naming (`oxyde-model`)

### Feature tree

- A part is an ordered list of features (sketch, extrude, revolve, cut, fillet, chamfer, shell, pattern, mirror, reference plane/axis…), each with typed parameters and references.
- Features form a dependency DAG; editing one feature marks it and its dependants dirty, and only those are rebuilt.
- A rollback bar and per-feature suppression are model concepts, not UI tricks.
- Dimensions can be driven by equations over global variables and other dimensions (see *Equations* below).

### Timeline (implemented in `part.rs`)

- Oxyde is **bottom-up** like SolidWorks: parts are modeled on their own; assemblies (§8) place finished parts and never reach inside their timelines.
- `Part` holds the ordered features, the rollback bar and suppression flags. `Part::timeline()` returns one row per feature with its status (`Ok`, `Warning`, `Error`, `Suppressed`, `RolledBack`, `Pending`) — the data behind the timeline panel.
- New features are inserted at the rollback bar. Deleting a feature that others refer to is refused.
- `Part::rebuild(kernel)` replays from the first edited feature only. After each feature it keeps the resulting `Body`: kernel shape, topology and the names of every face, edge and vertex. A feature that fails is marked `Error` and leaves the body unchanged; later features still build if their references exist.
- Features implemented: sketch (on Front/Top/Right, a planar face or a reference plane), boss/cut extrude (blind, mid-plane, reversible), boss/cut revolve (about a sketch line, a model axis, a reference axis, an edge or a face's axis; any angle, mid-plane, reversible), fillet, chamfer, shell (inward or outward, any faces removed), reference plane (offset from a plane or a flat face), reference axis (along an edge or a face's axis, or where two planes meet), linear pattern (one or two directions), circular pattern (equal spacing or not), mirror.
- **References to planes and axes** are `PlaneRef` (standard plane, flat face, reference plane) and `AxisRef` (model axis, reference axis, edge, face), resolved at rebuild against the body before the feature, like every other reference. Reference features produce no body, only a `ReferenceGeometry` (plane or axis) that later features and the viewport use.
- **Patterns and mirrors copy tool bodies**, as SolidWorks patterns features rather than bodies: an extrusion or a revolve keeps the solid it added or removed (`ToolBody`), and a pattern transforms a copy of that tool to each instance and fuses or cuts it, seed by seed. So a pattern of a hole cuts holes, even where the seed's surroundings differ. Only extrusions and revolves can be seeds for now.

### Persistent naming (implemented in `naming.rs`)

The problem: a fillet stores "edge #17"; after an upstream edit, numbering changes and the fillet attaches to the wrong edge or fails. FreeCAD spent years on this; we designed for it from day one.

1. **Faces are named by provenance**, from the kernel history:
   - `F2.side[3]`: side face that feature F2 swept from sketch segment 3;
   - `F2.start` / `F2.end`: the two caps of extrusion F2;
   - `F5.blend(edge(…))`: fillet face that F5 made from a named edge;
   - `F7.offset(F2.end)`: inner wall that shell F7 offset from `F2.end`;
   - `F9[2,0](F4.side[2])`: the copy, in instance `[2, 0]` of pattern F9, of the seed face `F4.side[2]`. Instance positions are stable while the counts grow, so a fillet on the second hole of a pattern stays on it.

   A face that a later operation only trims, splits or merges **keeps its name**, so the top face of a block is still `F2.end` after holes are cut into it. When a boolean merges faces, the name from the oldest feature wins, so existing references stay valid.
2. **Edges are named by the faces they separate**, vertices by the faces meeting at them: `edge(F2.side[2] | F2.end)`. This needs no extra history from the kernel and is stable whenever the faces are.
3. **Duplicates get a `part` ordinal** (`F2.end@1`, `F2.end@2` when a slot splits the top face), assigned in a deterministic geometric order.
4. **Resolution** of a stored name against a rebuilt body: exact match, else a unique loose match ignoring part ordinals (built with a warning), else `Ambiguous` or `Lost` (feature in error, asking the user to reselect).
5. Names reference features by `FeatureId`, never by display name, so renaming is free. Feature ids are never reused.
6. Sketches on faces get a deterministic frame (origin = projection of the model origin, x axis = least-aligned model axis), so they land in the same place after the face is resized.

Known limits, to address later: unknown-origin faces (none expected from current operations) get index-based names; when pieces of a split face swap geometric order, their ordinals swap too.

### Dimension names (implemented in `dimensions.rs`)

The groundwork for equations, SolidWorks-style:

- Every driving value has a **full name** `dimension@feature`: sketch dimensions (`D1@Sketch1`) and feature parameters (`D1@Boss-Extrude1` for the depth, `D1@Fillet1` for the radius). The name part can be changed (`Width@Sketch1`).
- Internally a dimension is a `DimensionRef { feature: FeatureId, key: Sketch(RelationId) | Feature(FeatureParameter) }`: stable ids only. Renaming a feature or a dimension changes how it is shown, never what it points to; an equation will be stored with references and displayed with the current names.
- `Part::dimensions()` lists every dimension in timeline order; `find_dimension("D1@Sketch1")` resolves a full name (quotes allowed, as typed in an equation); `set_dimension` and `rename_dimension` change one. Names are validated: unique feature names per part, unique dimension names per feature, no `@` or `"`.
- Feature dimensions are numbered per feature in the order of `FeatureKind::parameters()` (`D1`, `D2`…); optional parameters come last so existing names never shift.

### Equations (implemented in `expression.rs` and `equations.rs`)

- **Global variables** (`"Width" = 50`) and **equations** driving dimensions (`"D1@Sketch1" = "Width" * 2`), as in the SolidWorks Equations window. Syntax: numbers, quoted names, `+ - * / ^`, parentheses, `pi`, trigonometry in degrees, `sqrt abs exp log log10 int round sgn min max`. Values are in display units: mm, degrees, counts.
- Parsed expressions hold `Operand::Global(GlobalId)` or `Operand::Dimension(DimensionRef)`: stable ids again. They are printed with current names; each name reference also remembers its last known name, so an equation pointing at something deleted is still shown, saved and reported ("\"D3@Sketch1\" does not exist") instead of vanishing.
- `Part::rebuild` first evaluates everything in **dependency order** (topological sort, whatever the order of the lists), sets the driven dimensions, and only dimensions whose value really changed mark features for rebuild. A circular reference is an error on each equation involved; an error propagates to whatever uses the failed value. `Part::equation_results()` holds each value or error for the UI.
- A driven dimension is flagged (`Dimension::driven`): read-only in the Dimensions tab, shown with Σ in sketch mode, and not editable from its sketch.
- In files, expressions are stored as text with names (readable by other tools), see `FORMAT.md`.
- **Tools › Equations…** opens the window: an *Equations* tab (global variables and equations, with the value each evaluates to or its error) and a *Dimensions* tab (every dimension, rename and edit values).

## 7. Units and tolerances

- Internal length unit: **millimetre**, `f64`. Angles in radians internally, degrees in UI.
- Display units are a per-document preference; conversion happens only at the UI and file boundary.
- One central tolerance policy in `oxyde-geom` (linear `1e-6 mm`, angular `1e-9 rad` as starting values) passed to the kernel explicitly.

## 8. Assemblies and drawings (later)

- Bottom-up: an assembly only places parts (and sub-assemblies) and mates them; editing a part happens in the part's own timeline.
- Assemblies reference parts by relative path + document UUID; mates are solved by a 3D constraint solver sharing infrastructure with `oxyde-sketch`.
- The assembly tree is the BOM source: quantities, part numbers and custom properties come straight from each part's `properties.json` (see `FORMAT.md`).
- Drawings: views computed by the kernel's hidden-line removal (OCCT `HLRBRep`), annotations and dimensions in Rust, export to PDF/SVG/DXF.

## 9. Application (`oxyde-app`) and viewport (`oxyde-render`)

Layout, SolidWorks-style: menu bar (with an Insert menu listing every feature), command bar (Sketch, Extruded and Revolved Boss/Cut, Fillet, Chamfer, Shell, Linear and Circular Pattern, Mirror, Reference Geometry › Plane/Axis, standard views, undo/redo), **timeline panel** on the left (reference planes, features with status colours and tooltips, rollback bar, context menu: rename, suppress, roll back, delete), the **3D viewport** in the centre, a status bar.

- UI toolkit: `egui`/`eframe` 0.29, immediate mode. The app owns one `OcctKernel`; it is the only crate that chooses a backend.
- **Rendering**: `oxyde-render` is a software rasterizer (orthographic, Y-up like SolidWorks). It draws shaded faces (normals smooth within a face, crisp between faces) and edges with a depth buffer, and fills **id buffers** in the same pass, so picking the face or edge under the mouse is exact; edges win within a few pixels. The image is shown as an egui texture and re-rendered only when the camera, size, highlight or scene changes. No GPU code means nothing driver-specific can break; a GPU backend can replace it behind the same `Scene`/`Camera`/`Frame` interface when parts get large.
- Navigation: left-drag orbit, middle-drag or Shift+left-drag pan, wheel zoom at cursor, `F` zoom to fit; Front/Top/Right/Isometric views.
- **Selection is held by persistent name** (`FaceName`/`EdgeName`), not index, so it survives rebuilds. Selected edges feed Fillet/Chamfer; a selected planar face becomes the sketch plane.
- **Undo/redo** stores `PartDefinition` snapshots (the same data that is saved); one drag of a value (in a feature window or the Dimensions window) is one step.
- No properties panel: features are edited SolidWorks-style, by double-click or right-click › Edit, which opens the feature's window or sketch mode.
- Feature windows pick planes, axes and directions from lists: the standard planes and axes, the reference planes and axes before the feature, and suitable faces and edges selected in the viewport (selected before the command, as in SolidWorks' pre-selection). Revolve lists the sketch's lines first, centerlines on top.
- Reference planes and axes are drawn over the image (translucent squares and dash-dot lines sized to the part); the one selected in the timeline is highlighted, and selecting a reference plane then *Sketch* sketches on it.
- Selecting a face or an edge highlights, in the timeline, the feature that created it (for an edge, the later of the two features that made its faces).
- The rollback bar is a plain line with a round handle; drag it between features.
- **Sketch mode** (`sketch_mode.rs`): editing a sketch rolls back the features after it, turns the view normal to the plane, and edits a working copy written back on exit (Escape or *Exit sketch*; *Discard* drops the changes, and a brand-new empty sketch is removed). Tools: select, line (chains; clicking an existing point closes the contour), rectangle, circle, arc. Clicks within 8 px of a point reuse it; nearly horizontal or vertical lines get the relation automatically. In select mode, dragging moves points, lines or circle radii live through the solver. Right-click offers the relations and dimensions that fit the selection; clicking a dimension label edits its value and name, and hovering it shows its full name (`D1@Sketch1`). Geometry is coloured blue (under-defined), black (fully defined) or red (in a conflicting relation). Undo inside the sketch is local to the session; the whole edit is one undo step in the part.

## 10. Roadmap

| Milestone | Content |
|---|---|
| **M0** | Workspace, docs, CI (format, clippy, tests) |
| **M1** (code written, awaiting first Windows build) | `oxyde-occt` shim builds on Windows; make a box, check volume/topology, export STEP from a test |
| **Engine** (code written, awaiting first Windows build) | Kernel history + topology; part timeline with rollback/suppression; persistent naming; sketch/extrude/cut/fillet/chamfer; end-to-end tests in `oxyde-tests` |
| **M2** (code written, awaiting first build) | Sketch entities + solver with DOF, fully-defined and redundant/conflicting reporting (pure Rust, no UI) |
| **M2b** (code written, awaiting first build) | Solver in the model and the file format; interactive sketch mode; timeline without properties panel, draggable rollback bar, creator highlight |
| **App** (code written, awaiting first Windows build) | Timeline panel, software-rendered viewport with picking, `.oxy` save/load, STEP export, undo/redo |
| **Naming** (code written, awaiting first build) | Stable relation ids, SolidWorks-style dimension names (`D1@Sketch1`), unique feature names, Dimensions window |
| **Equations** (code written, awaiting first build) | Global variables and equations, evaluated in dependency order before every rebuild; Equations window |
| **M3** (code written, awaiting first build) | Revolve (boss/cut), shell, reference planes and axes |
| **M4** (done in App and M2b) | Viewport, interactive sketch mode, selection, fillet/chamfer with persistent naming |
| **M5** (code written, awaiting first build) | Linear and circular patterns, mirror (tool-body copies with instance names) |
| **M6** | Assemblies with basic mates; BOM export |
| **M7** | Drawings: views, dimensions, PDF/DXF |
