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
| `oxyde-assembly` | `geom`, `kernel` (topology types only), `model` | `occt`, `app` |
| `oxyde-format` | `geom`, `model`, `assembly`, `kernel` (STEP structure types only) | `occt`, `app` |
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
- **Results are checked.** OCCT's builders can report success on geometry they could not really make: shells, fillets and chamfers are checked with `BRepCheck_Analyzer`, and each wall of a shell must lie the wall thickness from the face it copies (a wall thicker than a face's curvature comes out turned inside out, which `BRepCheck` accepts).
- **No C++ exceptions escape.** Each shim function catches `Standard_Failure` (which does not derive from `std::exception`) and everything else, and rethrows a `std::runtime_error` that `cxx` turns into a Rust `Err`.
- **Thread confinement.** All OCCT calls run on one dedicated kernel thread; the Rust side talks to it through a channel. This sidesteps OCCT's uneven thread-safety and keeps the UI responsive during long rebuilds. *(Not yet implemented: since M1 `OcctKernel` is simply `!Send`; the kernel thread arrives with the application.)*

Toolchain: `cxx` / `cxx-build` for the bridge and to compile the shim. **Windows/MSVC only for now.** `build.rs` locates OCCT through `OXYDE_OCCT_DIR`, `VCPKG_ROOT`, or explicit `OXYDE_OCCT_INCLUDE_DIR` + `OXYDE_OCCT_LIB_DIR` — see `BUILDING.md`.

## 4. The `Kernel` trait (`oxyde-kernel`)

Implemented (see `crates/oxyde-kernel/src/lib.rs` for the documented signatures):

| Group | Methods |
|---|---|
| Construction | `make_box`, `extrude(plane, profile, vector)`, `revolve(plane, profile, axis, angle)`, `boolean(Fuse/Cut/Common, target, tool)` and `boolean_many` (several tools in one operation), `fillet(shape, edges, radius)`, `chamfer(shape, edges, distance)`, `shell(shape, removed faces, signed offset)`, `transform(shape, rigid motion or mirror)` |
| Queries | `topology` (faces, edges, vertices with adjacency, surface/curve kind, area, centroid, planar normal, axis of a surface of revolution, line or circle axis of an edge, edge midpoint), `tessellate` (display mesh: triangles per face, polyline per edge, indexed like the topology), `volume`, `topology_counts` |
| Exchange | `export_step(shapes, path)`, `import_step(data)`, `read_step_assembly(data)` (product structure: each part once as its own STEP file, assemblies with placed instances) |
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

`transform` reports every face as the `InputFace` it is a copy of, so pattern instances can be named after the seed's faces. The kernel never remembers anything between calls; turning this history into lasting names is the model's job (§6). Still to come: BREP import/export (for an optional `.oxy` geometry cache).

A second, trivial backend (`NullKernel` / mock) lives in tests so that `oxyde-model` can be tested without OCCT installed.

## 5. Sketches and the constraint solver (`oxyde-sketch`)

Implemented (M2, solver only):

- **Entities**: points, lines, circles and counter-clockwise arcs. Lines and arcs refer to point entities, so connected geometry shares its points as in SolidWorks. Construction geometry is solved but left out of profiles.
- **Relations and dimensions**: fixed, coincident, horizontal, vertical, parallel, perpendicular, equal length, equal radius, point–point distance (true, horizontal, vertical), length, radius, angle, point on line, point on circle, midpoint, tangent (line–circle/arc). `rectangle()` adds its horizontal/vertical relations automatically.
- **Solver**: unknowns are point coordinates and circle radii (fixed points are not unknowns; arc radii are implied). Each relation contributes equations `r(x) = 0`. Steps are minimum-norm Gauss–Newton steps `Δ = −Jᵀ(JJᵀ + μI)⁻¹r`, so an under-defined sketch moves as little as possible (the right behaviour when dragging), with `μ` raised towards gradient descent when a step does not reduce the error, and a backtracking line search; a full step that overshoots is still kept when the next step from there brings the error down (a watchdog: a line turned a long way swings away from a point that must stay on it). The equations are first split into clusters that share no unknown (fixed points tie nothing together), each solved and analysed on its own, so the cost follows the size of each contour rather than of the whole sketch. The Jacobian is exact: the residuals are written once over a number type that is either `f64` or a dual number carrying a derivative (forward-mode automatic differentiation), and each derivative only evaluates the equations involving its unknown.
- **Analysis**, from the Jacobian at the solution:
  - **degrees of freedom** = unknowns − rank;
  - an entity is **fully defined** (black) when no remaining motion (null space of the Jacobian) moves any of its parameters, otherwise under-defined (blue);
  - relations are taken in order: one adding no independent equation is **redundant**, one left unsatisfied is **conflicting**. A failed solve leaves the geometry untouched. The rank, the degrees of freedom and the null space all come from one incrementally reduced basis of the Jacobian's rows, so they always agree with each other.
- `profile_segments()` turns the solved sketch into profile curves whose segment ids are the entity ids, so face names (`F2.side[7]`) follow sketch entities. Contours that cross or touch each other, or themselves, are refused (`ProfileError::Crossing`, found exactly, not on sampled polygons), as SolidWorks refuses self-intersecting contours: OpenCASCADE would extrude them into empty or overlapping solids without complaint.

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
- New features are inserted at the rollback bar. Deleting a feature that others refer to is refused, and so is a new definition for a feature (`Part::replace_feature`, after editing it) that refers to itself or to a later feature: such a part could not be opened again.
- `Part::rebuild(kernel)` replays from the first edited feature only. After each feature it keeps the resulting `Body`: kernel shape, topology and the names of every face, edge and vertex. A feature that fails is marked `Error` and leaves the body unchanged; later features still build if their references exist.
- Features implemented: sketch (on Front/Top/Right, a planar face or a reference plane), boss/cut extrude (blind, mid-plane, reversible), boss/cut revolve (about a sketch line, a model axis, a reference axis, an edge or a face's axis; any angle, mid-plane, reversible), fillet, chamfer, shell (inward or outward, any faces removed), reference plane (offset from a plane or a flat face), reference axis (along an edge or a face's axis, or where two planes meet), linear pattern (one or two directions), circular pattern (equal spacing or not), mirror.
- **References to planes and axes** are `PlaneRef` (standard plane, flat face, reference plane) and `AxisRef` (model axis, reference axis, edge, face), resolved at rebuild against the body before the feature, like every other reference. Reference features produce no body, only a `ReferenceGeometry` (plane or axis) that later features and the viewport use.
- **Patterns and mirrors copy tool bodies**, as SolidWorks patterns features rather than bodies: an extrusion or a revolve keeps the solid it added or removed (`ToolBody`), and a pattern transforms a copy of that tool to each instance and fuses or cuts it, seed by seed. So a pattern of a hole cuts holes, even where the seed's surroundings differ. All the copies of a seed go into one boolean (`Kernel::boolean_many`): one boolean per copy redid the work on a body growing with each (a 10 × 10 pattern of holes went from 13 s to 0.8 s on a Raspberry Pi). Copies of the part's first boss, which made the first body, join that body. Only extrusions and revolves can be seeds for now.

### Bodies (implemented in `bodies.rs`)

A part's geometry is a list of solid bodies (`Solid`: a `BodyId`, its kernel shape and its own topology), seen together as one `Body` whose topology concatenates theirs (faces of the first solid, then the second…) and whose `BodyNaming` names all of them at once. Everything that reads geometry by name or index (references, picking, highlighting, sketches on faces, assemblies) works on that merged view unchanged; `Body::solid_of_face` / `solid_of_edge` say which body something is on, and kernel operations run on one solid with local indices (`Solid::local_face`). Each feature result keeps the whole set of bodies after it; bodies it did not touch are shared with the previous result (not owned), so their shapes are freed once, by the feature that made them. Extrusions and revolves join, cut, intersect or add a new body (`ExtrudeOp`), on chosen bodies or automatically; *Combine* is a boolean between bodies. See `FORMAT.md`, *Bodies*.

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

## 8. Assemblies (`oxyde-assembly`) and drawings (later)

- **Bottom-up**: an assembly only places parts and sub-assemblies and mates them; a component is edited in its own document (*Open part* / *Open sub-assembly*, then *File › Back to assembly*, which reloads the files).
- **Sub-assemblies** are rigid, as in SolidWorks: their components stay where their own file put them, and only the top-level components are unknowns of the solver. A mate may still reach a face or edge of a part deep inside one: `MateRef::path` lists the component ids from the sub-assembly down to that part, and the app composes the placements along it (`Placement::compose`) to give the solver the geometry in the top-level component's coordinates. Loading follows sub-assemblies recursively with a stack of open files, so an assembly that contains itself is refused.
- **Components** reference part files by path relative to the assembly + document UUID, so a part moved within the assembly's folder is found again by its id (see `FORMAT.md` §6). Each component has a placement (rotation + translation, never a mirror). The first one inserted is **fixed**; any can be fixed or floated.
- **Mates** (coincident, concentric, parallel, perpendicular, distance, angle) name geometry the way features do: persistent face and edge names in the part, or the part's standard planes, axes and origin. `resolve` turns a reference into a primitive in part coordinates using the part's rebuilt body: a flat face is a plane (outward normal), a cylindrical or conical face its axis, a straight edge a line, a circular edge a circle (centre and normal). Each mate kind writes residuals that vanish when it holds; directions are scaled by the assembly size so that they weigh like lengths. A mate between oriented geometry is *aligned* or *anti-aligned*; a new mate starts with whichever is closest to how the parts sit.
- **Solver**: Levenberg–Marquardt over a small rotation and a shift per free component, from the current placements, so the answer is the nearest one (which is what dragging needs). Mates are first split into groups tied together through components that can move (fixed components tie nothing), each solved on its own: the cost follows the size of each group, and a conflict in one group leaves the others alone. When the mates cannot all hold, they are added back one at a time in order and each one that cannot hold with those before it is reported, as SolidWorks blames the newest mate. Degrees of freedom left per component are the motions of its group that keep the mates (the null space of their Jacobian) seen on that component, so a component held only by one that can still move is not fully defined: the tree shows `(f)` fixed, `(-)` under-defined.
- **BOM**: components grouped by file (same path or same document id), with part number, description and material from each file's `properties.json`, in three views: top level (a sub-assembly is one line), parts only (through every level, quantities multiplied) and indented (`2`, `2.1`, `2.2`…); exported as CSV.
- **Display**: every part occurrence (a top-level component plus its path inside sub-assemblies) is moved to its placement and the meshes are combined into one for the viewport; the face and edge ranges of each occurrence map picks back to it and a persistent name.
- Drawings: views computed by the kernel's hidden-line removal (OCCT `HLRBRep`), annotations and dimensions in Rust, export to PDF/SVG/DXF.

## 9. Application (`oxyde-app`) and viewport (`oxyde-render`)

Layout, SolidWorks-style: menu bar (with an Insert menu listing every feature), a tabbed **command bar** (`command_bar.rs`, like the CommandManager) with icon buttons — *Features* (Extrude, Revolve, Fillet, Chamfer, Shell, Linear and Circular Pattern, Mirror, Reference Geometry › Plane/Axis), *Sketch* (Sketch or Exit Sketch/Discard, Smart Dimension, Line, Centerline, Rectangle, Circle, Arc, Trim, Construction, Normal To) and *Evaluate* (Mass Properties, Measure, Equations, Rebuild) — with undo/redo on the right; the **PropertyManager** and the **timeline panel** on the left (reference planes, features with icons, status colours and tooltips, rollback bar, context menu: rename, suppress, roll back, delete), the **3D viewport** in the centre with a heads-up view toolbar (standard views, zoom to fit, Normal To), a status bar.

- **Icons** (`icons.rs`) are drawn as vector shapes on a 24-unit grid, so they stay sharp at any scale and need no image files. One palette for all of them; disabled commands are greyed. The ignored test `icon_sheet` writes every icon to an SVG file (path in `OXYDE_ICON_SHEET`) for review.

- UI toolkit: `egui`/`eframe` 0.29, immediate mode. The app owns one `OcctKernel`; it is the only crate that chooses a backend.
- **GPU rendering** (`gpu.rs`): the viewport is drawn with wgpu (Direct3D 12, Vulkan or Metal, through eframe's wgpu backend). The part goes into an off-screen texture with 4× multisampling and a depth buffer, which egui shows like any image; sketches and annotations are painted over it by egui. Edges are screen-space quads (1.25 px, 3 px when highlighted); a second pass writes face and edge ids into an integer texture, and picking reads back the few pixels under the cursor. Shading, colours and pick rules match the software renderer, which a test checks pixel by pixel (`gpu_picking_matches_the_software_renderer`, ignored without a GPU). When the GPU cannot start, or with `OXYDE_RENDERER=cpu`, the window falls back to OpenGL and the viewport to the software renderer; Help › About tells which is in use.
- **Software rendering**: `oxyde-render` is a software rasterizer (orthographic, Y-up like SolidWorks). It draws shaded faces (normals smooth within a face, crisp between faces) and edges with a depth buffer, and fills **id buffers** in the same pass, so picking the face or edge under the mouse is exact; edges win within a few pixels. The image is shown as an egui texture and re-rendered only when the camera, size, highlight or scene changes. It is the fallback when no GPU is available, and the reference the GPU renderer is tested against.
- **Preferences** (`prefs.rs`, `prefs_ui.rs`; Tools › Preferences, Ctrl+,): saved as JSON in the user's configuration folder (`%APPDATA%\Oxyde\preferences.json`), every field defaulted so old and new files load. *Graphics*: renderer and anti-aliasing (next start), colour themes and every viewport colour, edges shown or hidden and their widths, mesh quality, orientation triad. Both renderers draw from one `oxyde_render::Style`; the GPU shader reads it from its uniform buffer. *Navigation*: orbit and zoom speed, wheel direction, view animation. *Shortcuts*: every command has an id, a context (everywhere, part, sketch, assembly) and a SolidWorks-like default; the user's changes are stored by id, conflicts in the same context are shown in red, and keys with more modifiers are matched first. *Debug*: performance overlay (frame time, triangles, rebuild and solve times), what is under the pointer (index and persistent name), egui's widget outlines and inspector.
- Navigation: left-drag orbit, middle-drag or Shift+left-drag pan, wheel zoom at cursor, `F` zoom to fit; Front/Top/Right/Isometric views.
- **Assembly mode** (`assembly.rs`, `assembly_ui.rs`): the app holds either a part or an assembly. In an assembly the command bar has one *Assembly* tab (Insert Component, Mate, Fix/Float, Reload Parts, Bill of Materials); the tree lists components (`(f)` fixed, `(-)` can still move, a red badge when the part file cannot be opened; expand one to pick its planes or origin) and the mates (red when they cannot hold). *Mate* opens in the PropertyManager with two selection boxes filled by clicking faces and edges in the viewport or planes in the tree (two faces or edges selected beforehand fill them); the type follows the geometry (round faces and circular edges: concentric, otherwise coincident) until chosen, and the viewport previews the solved result. Alt+left-drag moves a free component, Alt+Shift+left-drag turns it, and the mates are re-solved as it moves. Undo/redo keep snapshots of the assembly.
- **Selection is held by persistent name** (`FaceName`/`EdgeName`), not index, so it survives rebuilds.
- **Select, then command.** Commands start from what is selected: Extrude and Revolve take the sketch selected in the timeline (else the latest sketch no feature uses yet), Fillet/Chamfer the selected edges, Shell the selected faces, patterns the selected feature, Sketch the selected plane or flat face (else it asks for one, drawing the three standard planes in the viewport to click). Extrude and Revolve are one command each, with *Add material* / *Remove material* (the feature names still follow SolidWorks: `Boss-Extrude1`, `Cut-Extrude1`; cuts carry a red badge in the timeline).
- **Undo/redo** stores `PartDefinition` snapshots (the same data that is saved); one drag of a value (in a feature window or the Dimensions window) is one step.
- **PropertyManager** (`property_manager.rs`): a command's options show at the top of the left panel, with OK (Enter) and Cancel (Esc). While it is open the timeline and viewport stay live and **clicks go to the command**: a sketch clicked in the tree becomes the sketch to extrude, edges and faces toggle in Fillet/Chamfer/Shell (a face adds all its edges), features toggle in a pattern's *Features to copy*, a plane, flat face, edge or axis fills the plane or direction. Selection boxes list the picks, each with a remove button; what the command refers to is highlighted in the viewport and the timeline. Existing features open the same way, by double-click or right-click › Edit. Planes, axes and directions can also be chosen from lists: the standard planes and axes, the reference planes and axes before the feature, and the picks. Revolve lists the sketch's lines first, centerlines on top.
- **STEP import**: File › Import STEP… (or the *Import STEP* button) adds an *Imported* feature at the rollback bar; its solids are fused with the body, and later features build on its faces, named by their position in the imported shape. File › Open (or *New part from STEP…*) on a `.step` file makes a new part from it. The file's contents are stored in the part (`imports/` in the `.oxy` archive), so the original file is no longer needed.
- **Evaluate** (`evaluate.rs`): *Mass Properties* (volume, surface area, size along the axes, topology counts) and *Measure* (area of selected faces, length of selected edges, totals, the angle between two flat faces or two straight edges and the distance between parallel faces).
- Reference planes and axes are drawn over the image (translucent squares and dash-dot lines sized to the part) and can be clicked; the one selected in the timeline is highlighted, and selecting a plane then *Sketch* sketches on it. A selected standard plane is drawn too.
- Selecting a face or an edge highlights, in the timeline, the feature that created it (for an edge, the later of the two features that made its faces).
- The rollback bar is a plain line with a round handle; drag it between features.
- **Absorbed sketches**, as in Creo and SolidWorks: the sketch an Extrude or Revolve uses (the first one, if several do) is not listed on its own; it sits under that feature, behind an expand arrow.
- **Visibility** (`display.rs`): planes, the origin, sketches and reference geometry each have an eye in the timeline (and right-click › Show/Hide); View › Hide/Show (also the eye in the heads-up toolbar) turns whole kinds off. Standard planes start hidden, absorbed sketches hidden, everything else shown. Shown sketches are drawn over the part and can be clicked to select them; the origin is drawn as a small triad.
- **Animated views**: standard views, Normal To and opening a sketch turn the camera smoothly (about half a second); opening a sketch centres its geometry, as SolidWorks does.
- **Sketch mode** (`sketch_mode.rs`): editing a sketch rolls back the features after it, turns the view normal to the plane, and edits a working copy written back on exit (Escape or *Exit sketch*; *Discard* drops the changes, and a brand-new empty sketch is removed). Tools: select, Smart Dimension, line (chains; clicking an existing point closes the contour), centerline (construction line), corner and center rectangle (the centre held by construction diagonals), circle, arc, trim, Convert Entities. Every sketch has an **origin point** where the model origin falls in its plane (a fixed construction point): it can be snapped to, selected, dimensioned and related, but not deleted. Drawing snaps, within 8 px, to existing points, to the end points, middles and centres of the **part's edges lying in the sketch plane** (including imported geometry; the new point is fixed there), to the middle of lines, to crossings of curves, and onto curves, with a glyph at the cursor; the new point keeps the relation (coincident, fixed, midpoint, on line/circle). Centres of round part edges are marked with a cross. **Convert Entities** copies a clicked part edge (line, circle, arc) into the sketch, fixed in place; edges converted one after the other share their end points (`model_refs.rs`). Nearly horizontal or vertical lines get the relation automatically. **Smart Dimension**: click one entity then empty space (a line's length; a circle or arc is dimensioned at once), or two entities: point–point (horizontal or vertical when lined up, else aligned), point–line (perpendicular distance), parallel lines (distance), other line pairs (angle); circles count by their centre. **Trim** previews in red the piece under the cursor, up to the nearest crossings, and removes it on click (an end piece is cut back, a middle piece splits the curve, a circle becomes an arc, a curve nothing crosses is deleted). Dimensions are drawn with extension lines and arrows, lengths outside the shape. A click on a dimension selects it (Delete removes it), a double click edits it; the value window opens with the value selected, so after picking two entities with Smart Dimension the value can be typed at once. **Mirror Entities**: select the geometry, then click the line or centerline to mirror about; each copied point is tied to its original by a *symmetric* relation (also offered for two points and a line in the right-click menu), so the copy follows later edits. In select mode, dragging moves points, lines or circle radii live through the solver: the dragged points stay under the cursor while the rest follows when the relations allow it (else they go as near as they can), and fixed points never move. Right-click offers the relations and dimensions that fit the selection; clicking a dimension label edits its value and name, and hovering it shows its full name (`D1@Sketch1`). Geometry is coloured blue (under-defined), black (fully defined) or red (in a conflicting relation). Undo inside the sketch is local to the session; the whole edit is one undo step in the part.

## 10. Roadmap

| Milestone | Content |
|---|---|
| **M0** (CI not set up yet) | Workspace, docs, CI (format, clippy, tests) |
| **M1** (done) | `oxyde-occt` shim builds on Windows; make a box, check volume/topology, export STEP from a test |
| **Engine** (done) | Kernel history + topology; part timeline with rollback/suppression; persistent naming; sketch/extrude/cut/fillet/chamfer; end-to-end tests in `oxyde-tests` |
| **M2** (done) | Sketch entities + solver with DOF, fully-defined and redundant/conflicting reporting (pure Rust, no UI) |
| **M2b** (done) | Solver in the model and the file format; interactive sketch mode; timeline without properties panel, draggable rollback bar, creator highlight |
| **App** (done) | Timeline panel, viewport with picking, part save/load, STEP export, undo/redo |
| **Naming** (done) | Stable relation ids, SolidWorks-style dimension names (`D1@Sketch1`), unique feature names, Dimensions window |
| **Equations** (done) | Global variables and equations, evaluated in dependency order before every rebuild; Equations window |
| **M3** (done) | Revolve (boss/cut), shell, reference planes and axes |
| **M4** (done) | Viewport, interactive sketch mode, selection, fillet/chamfer with persistent naming |
| **M5** (done) | Linear and circular patterns, mirror (tool-body copies with instance names) |
| **GPU** (done) | wgpu viewport with GPU picking, software renderer as fallback |
| **Bodies** (code written, awaiting first Windows build) | Multi-body parts: join / cut / intersect / new body, Combine, Solid Bodies folder |
| **M6** (code written, awaiting first Windows build) | Assemblies (`.oxyasm`): components from part files and sub-assemblies, mates with a 3D solver (reaching into sub-assemblies), fixed/floating, Alt+drag, BOM (top level, parts only, indented) exported as CSV; parts renamed `.oxyprt` |
| **M7** | Drawings: views, dimensions, PDF/DXF |

## STEP assembly import

File › Import STEP assembly turns a STEP file's product structure into Oxyde documents:

1. `Kernel::read_step_assembly` (OCCT's XCAF reader) lists every distinct product once.
   A part comes back as its solids written to a STEP file of its own, in its own
   coordinates; an assembly as instances of other products with rigid placements.
   Mirrored or scaled instances become parts of their own with the motion applied.
2. `oxyde_format::step_import::plan_step_import` decides the files: one `.oxyprt` per
   part, holding a single *Imported* feature with that STEP file, and one `.oxyasm` per
   assembly placing its components where the STEP file had them. Names come from the
   products, made valid and unique (ignoring case) file names; products without a solid
   are dropped. All files go into one folder.
3. The app writes them (asking before replacing files) and opens the top level.

Mates are not in STEP files; components keep their imported positions until mated.
OCCT's STEP translators and XCAF application are not thread-safe, so the shim serializes
every STEP read and write behind one lock.
