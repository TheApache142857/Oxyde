# Oxyde

An open-source, parametric, feature-based 3D CAD modeler — written in Rust, with a thin C++ bridge to the [OpenCASCADE](https://dev.opencascade.org/) geometry kernel.

> **Status: pre-alpha.** The repository currently contains the design documents and empty crate skeletons. Nothing is usable yet.

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
| `oxyde-sketch` | 2D sketch entities and the geometric constraint solver |
| `oxyde-kernel` | The `Kernel` trait: solid operations, topology queries, operation history, STEP I/O |
| `oxyde-occt` | `Kernel` implementation backed by OpenCASCADE through a small C++ shim (`cxx`) |
| `oxyde-model` | Feature tree, dependency graph, rebuild, persistent topological naming |
| `oxyde-format` | Reading and writing `.oxy` files |
| `oxyde-app` | Desktop application (UI and viewport) |

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and [`docs/FORMAT.md`](docs/FORMAT.md).

## Building

Not meaningful yet — the crates are empty. Once the OCCT backend lands, building will require:

- Rust (stable, edition 2024)
- A C++17 compiler
- OpenCASCADE 7.8 or newer, discoverable via CMake or `pkg-config`

## License

[MIT](LICENSE). OpenCASCADE itself is distributed under LGPL-2.1 with an additional exception, which is compatible with this project being MIT-licensed.
