# Oxyde

An open-source, parametric, feature-based 3D CAD modeler — written in Rust, with a thin C++ bridge to the [OpenCASCADE](https://dev.opencascade.org/) geometry kernel.

> **Status: pre-alpha.** A first desktop application is written: timeline with a draggable rollback bar, interactive sketch mode with a constraint solver (relations, dimensions, fully/under-defined colouring), boss/cut extrude and revolve, fillet, chamfer, shell, reference planes and axes, linear and circular patterns, mirror, persistent face/edge naming, SolidWorks-style dimension names (`D1@Sketch1`), global variables and equations, a 3D viewport with picking, `.oxy` save/load and STEP export. Everything awaits its first Windows build.

## Goals

- Sketch → constrain → extrude / revolve / cut → fillet / chamfer, with a full, editable feature tree.
- Assemblies with mates, and 2D drawings with a bill of materials.
- Exchange with every other CAD system through **STEP**, Parasolid XT, IGES, STL/3MF and DXF.
- An **open, documented native file format** (`.oxy`) that any tool — BOM extractors, viewers, scripts — can read without Oxyde installed.

## Non-goals

- Reading or writing native SolidWorks (or other proprietary) files. Interop goes through open formats.
- Writing our own B-rep kernel *now*. The kernel sits behind a Rust trait so it can be replaced later.

## Architecture at a glance

```
            oxyde-app
            /    |    \
 oxyde-format    |    oxyde-occt ── C++ shim ── OpenCASCADE
            \    |    /
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
| `oxyde-format` | Reading and writing `.oxy` files |
| `oxyde-render` | Software renderer for the viewport, with pixel-exact picking |
| `oxyde-app` | Desktop application (UI and viewport) |
| `oxyde-tests` | End-to-end tests: the model driving OpenCASCADE |

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and [`docs/FORMAT.md`](docs/FORMAT.md).

## Building

**Windows (x64, MSVC) only for now.** You need Visual Studio 2022 with the C++ workload, Rust 1.85+, and OpenCASCADE 7.8+ (vcpkg recommended). Full instructions: [`docs/BUILDING.md`](docs/BUILDING.md).

```powershell
cargo run --release -p oxyde-app              # the application (File › Open demo bracket)
cargo test --workspace                        # unit + end-to-end tests
```

## License

[MIT](LICENSE). OpenCASCADE itself is distributed under LGPL-2.1 with an additional exception, which is compatible with this project being MIT-licensed.
