# Oxyde

An open-source, parametric, feature-based 3D CAD modeler — written in Rust, with a thin C++ bridge to the [OpenCASCADE](https://dev.opencascade.org/) geometry kernel.

> **Status: pre-alpha.** A first desktop application is written: a tabbed command bar with icons (Features, Sketch, Evaluate, Render) and a SolidWorks-style PropertyManager (select first, then run the command) with a live preview of every feature in the viewport (the faces it makes tinted, red for cuts, and why it fails before OK), timeline with a draggable rollback bar, interactive sketch mode with a constraint solver (relations, Smart Dimension between any two entities, snapping, trim, fully/under-defined colouring), extrude, revolve and sweep (a profile along a path) joining, cutting, intersecting or making new bodies (multi-body parts with Combine), fillet, chamfer, shell, reference planes and axes, linear and circular patterns, mirror, persistent face/edge naming, SolidWorks-style dimension names (`D1@Sketch1`), global variables and equations, configurations (named variants of a part, each with its own dimension values and suppressed features, in a ConfigurationManager tab), STEP import (as an *Imported* feature to build on, with the colours and the materials Fusion 360 and SolidWorks write) and export, STEP assembly import (one part file per part, one assembly file per assembly, e.g. from a SolidWorks STEP AP242 export), mass properties, measuring, a Section View (the model cut open by a plane) and Interference Detection in assemblies, a GPU-rendered 3D viewport with picking, appearances (colour, transparency and colour textures on faces, bodies, parts or components) with floor reflections and optional subsurface scattering, a Render tab (a path tracer with materials, environment light, soft shadows and glass, saved as PNG), an Export menu (STEP, IGES, STL, OBJ, 3MF and glTF with mesh settings and a preview, and PNG images), `.oxyprt` save/load, and edit locks so two windows do not edit one file. **Assemblies** (`.oxyasm`): insert part files and sub-assemblies, mate them (coincident, concentric, parallel, perpendicular, distance, angle) with a live preview, move free components with Alt+drag while mates hold, fixed/floating components with degrees of freedom shown in the tree, parts and sub-assemblies edited in place with the rest of the assembly ghosted around them, and a bill of materials (top level, parts only or indented) exported as CSV. A Preferences window covers viewport colours and display, navigation, configurable keyboard shortcuts and debugging overlays. Builds and runs on Windows with OpenCASCADE 8, on macOS (Apple Silicon) with Homebrew's OpenCASCADE, and on Linux with the OpenCASCADE 7.8 that Debian and Ubuntu ship; `packaging/` bundles the Windows and macOS executables.

## Goals

- Sketch → constrain → extrude / revolve / sweep / cut → fillet / chamfer, with a full, editable feature tree.
- Assemblies with mates, and 2D drawings with a bill of materials.
- Exchange with every other CAD system through **STEP**, Parasolid XT, IGES, STL/3MF and DXF.
- **Open, documented native file formats** (`.oxyprt` parts, `.oxyasm` assemblies, `.oxydrw` drawings) that any tool — BOM extractors, viewers, scripts — can read without Oxyde installed.

## Non-goals

- Reading or writing native SolidWorks (or other proprietary) files. Interop goes through open formats.
- Writing our own B-rep kernel *now*. The kernel sits behind a Rust trait so it can be replaced later.

## Architecture at a glance

```
            oxyde-app
            /    |    \
 oxyde-format    |    oxyde-occt ── C++ shim ── OpenCASCADE
      |     \    |    /
      |   oxyde-assembly
      |      /
           oxyde-model
            /        \
  oxyde-sketch     oxyde-kernel (trait)
            \        /
            oxyde-geom
```

| Crate | Role |
|---|---|
| `oxyde-geom` | Math primitives: points, vectors, planes, transforms, tolerances |
| `oxyde-sketch` | 2D sketch entities and the geometric constraint solver (degrees of freedom, fully defined, redundant/conflicting) |
| `oxyde-kernel` | The `Kernel` trait: solid operations, topology queries, operation history, STEP I/O |
| `oxyde-occt` | `Kernel` implementation backed by OpenCASCADE through a small C++ shim (`cxx`) |
| `oxyde-model` | Feature tree, dependency graph, rebuild, persistent topological naming |
| `oxyde-assembly` | Assemblies: components, mates and the 3D mate solver, bill of materials |
| `oxyde-format` | Reading and writing `.oxyprt` parts and `.oxyasm` assemblies |
| `oxyde-render` | Viewport scene and camera; software renderer (GPU fallback and reference), pixel-exact picking; path tracer for the Render tab |
| `oxyde-app` | Desktop application (UI and viewport) |
| `oxyde-cli` | Headless command line: rebuild a part and measure, check, export or picture it |
| `oxyde-tests` | End-to-end tests: the model driving OpenCASCADE |

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and [`docs/FORMAT.md`](docs/FORMAT.md), and [`CHANGELOG.md`](CHANGELOG.md) for what changed recently.

## Building

**Windows (x64, MSVC)** is the main platform: you need Visual Studio 2022 with the C++ workload, Rust 1.85+, and OpenCASCADE 7.7+ (8.0 tested; vcpkg recommended). **macOS (Apple Silicon)** builds with `brew install opencascade`; **Linux** with the distribution's OpenCASCADE 7.8. Full instructions, bundling and releasing: [`docs/BUILDING.md`](docs/BUILDING.md).

```powershell
cargo run --release -p oxyde-app              # the application (File › Open demo bracket)
cargo test --workspace                        # unit + end-to-end tests
```

## Command line

`oxyde-cli` rebuilds a `.oxyprt` through OpenCASCADE without opening a window, for scripts, agents and continuous integration:

```powershell
cargo run -p oxyde-cli -- eval part.oxyprt [--json]           # feature statuses; per body volume, area, centre of mass, bounds, face/edge/vertex counts
cargo run -p oxyde-cli -- check part.oxyprt --expect expected.json [--tol 1e-3]   # pass/fail table against expected measures
cargo run -p oxyde-cli -- export part.oxyprt --out part.step   # or part.stl (binary, mm)
cargo run -p oxyde-cli -- snapshot part.oxyprt --out part.png [--view iso|front|top|right] [--width 1024 --height 768]
```

Exit code 1 when a feature failed to rebuild or a check did not pass, 2 when the command could not run. `expected.json` holds any of `bodies`, `volume`, `area`, `centre_of_mass`, `bounds` (`min`, `max`), `faces`, `edges`, `vertices`; counts must match exactly, volume and area within the relative tolerance, coordinates within the tolerance times the part's size. `eval --json` writes the same object the tests in `crates/oxyde-tests/tests/cli.rs` read back.

## License

[MIT](LICENSE). OpenCASCADE itself is distributed under LGPL-2.1 with an additional exception, which is compatible with this project being MIT-licensed.
