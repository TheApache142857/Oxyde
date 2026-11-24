# oxyde-generative

Generative design on a grid of voxels: SIMP topology optimization of a design space between
bodies to keep and bodies to stay clear of, solved by matrix-free geometric multigrid, and the
closed surfaces of its outcomes. It is Oxyde's Generative Design tab's engine
(`docs/SIMULATION.md` §9 in the Oxyde repository), split out so that it can be read, tested
and taken elsewhere without the rest of Oxyde.

The crate is pure Rust, depends on no other crate and forbids `unsafe` code. Parallel steps
run on scoped threads (`std::thread::scope`) started for each step.

## Layout

| Module | What it does |
|---|---|
| `lib.rs` | The study (`GenerativeStudy`, `DesignSpace`, `Objective`, `Method`), the optimization loop (`generate`, `generate_on`), the `Backend` seam a solver on other hardware implements, `CpuBackend`. |
| `grid.rs` | The design space laid on cubic voxels (`Grid`): each voxel preserve, obstacle, outside or design; fixtures and loads carried to the grid's nodes. |
| `brick.rs` | The eight-node brick element, its stiffness for a unit modulus and edge, von Mises stress at its centre. |
| `multigrid.rs` | Conjugate gradients preconditioned by geometric multigrid on the voxel grid; the finest level is never assembled. |
| `mma.rs` | The method of moving asymptotes, for *Minimize mass* with a stress or displacement limit. |
| `additive.rs` | The overhang filter for parts printed layer by layer. |
| `frequency.rs` | The lowest natural frequency of a design and its slopes, for a minimum frequency limit. |
| `surface.rs` | The outcome's surface: marching tetrahedra over the densities, and the exact surface that meets the kept bodies on their faces. |
| `decimate.rs` | Quadric edge collapse that keeps marked points and flat faces; splitting into closed pieces. |
| `eigen.rs` | LOBPCG for the smallest eigenpairs, on closures over flat vectors. |
| `study.rs` | Fixtures, loads, solver options, symmetry planes: what a study is made of. |
| `shape.rs`, `surfaces.rs`, `tets.rs` | Triangle surfaces in and out, the hierarchy rays are cast in, the cube split into tetrahedra. |
| `math.rs`, `bvh.rs`, `material.rs` | Vector maths, a bounding volume hierarchy of triangles, elastic material properties. |
| `tests.rs` | The crate's tests. |

## Inputs and outputs

Units are millimetres, newtons and megapascals (N/mm²), which are consistent: displacements
come out in mm and stresses in MPa.

- **In:** a `GenerativeStudy`.
  - Its `DesignSpace` holds the bodies to keep (`preserve`), the bodies to stay clear of
    (`obstacles`) and an optional starting shape, each a closed triangle mesh
    (`SurfaceMesh`) whose triangles are labelled with the face they lie on.
  - Fixtures and loads name the labels they act on.
  - It also gives an objective (the stiffest shape for a mass target, or the lightest under a
    stress or displacement limit), a manufacturing method, the voxel size and an optional
    minimum frequency.
  - `generate` also takes an `Elastic` material, a cancel flag and a progress callback.
- **Out:** an `Outcome`.
  - The grid's densities, one byte per voxel (0 void, 255 solid), and the history of the
    iterations.
  - Mass, maximum stress and displacement, as the voxels estimate them.
  - `surface` and `exact_surface` turn it into a closed triangle surface (`Shape`);
    `decimate_holding` and `solid_pieces` simplify it and split it into closed pieces.

Oxyde checks the part made of an outcome on its own tetrahedral mesh with its finite element
solver (`oxyde_sim::check`); that check uses Oxyde's mesher and is not part of this crate.

## Tuned and derived values

- **Standard values:**
  - The SIMP penalty 3 (`PENALTY`) and the projection threshold 0.5 (`ETA`).
  - The stress p-norm power 8 (`NORM_POWER`) and the relaxation ρ^½ (`STRESS_RELAXATION`).
  - The method of moving asymptotes' constants (`mma.rs`, after Svanberg's paper).
  - The additive filter's parameters (`additive.rs`, after Langelaar's).
- **Tuned on Oxyde's test studies:** the Heaviside projection's schedule (`BETA_MAX`,
  `BETA_STEP_AFTER`), the move limits (`MOVE_LIMIT`, `LATE_MOVE_LIMIT`, `MMA_MOVE`), when the
  design counts as settled (`SETTLED_CHANGE`, `STEADY_*`), the void stiffness
  (`VOID_STIFFNESS`, `SCALAR_BELOW`), the multigrid's smoothing and spectrum bounds
  (`SMOOTHING_DEGREE`, `SPECTRUM_RATIO`, `*_MARGIN`), the solve tolerances, and the voxel
  counts of the automatic voxel size (`COARSE_VOXELS`, `FINE_VOXELS`). Each constant says
  what it does where it is defined.

## What a porter should know

- **Indexing:** the code still indexes slices with values that can come from the input
  (element and node ids, labels, triangle indices), behind module-level
  `#![allow(clippy::indexing_slicing)]`. A malformed input can therefore panic instead of
  returning an error. Turning those sites into errors is the subject of a second change in
  Oxyde.
- **Optimised builds:** the tests assume an optimised build. Unoptimised, the solver's
  studies take minutes and load every core for that long; Oxyde's workspace builds this crate
  at `opt-level = 3` in debug and test profiles for that reason.
- **Rounding:** an optimization amplifies differences in the last bits of its arithmetic,
  so another optimisation level, compiler or platform can reach another design. No result
  depends on time or on the number of threads. `load_cases_are_laid_apart_and_carried_together`
  asks for an outcome's mass within 6 % of its target: it passes at `opt-level = 3`, and at
  `opt-level = 0` its outcome lies 7.8 % under the target.
- **Threads:** each parallel step starts its threads and joins them before it returns. On
  sixteen threads a study takes about half as long again as on threads kept between steps
  (Oxyde's bracket study of `tests/generative.rs`, 30 iterations: 101 to 111 s against 69
  to 72 s).
- **Errors:** no `unwrap`, `expect` or `panic!` in non-test code; the crate denies them.
- **Tests:** the tests measure against values that can be derived (beam theory, finite
  differences, symmetry, equilibrium of the loads) or against the solver's own invariants,
  with the tolerance of each stated where it is used.

## Licence

MIT, as Oxyde:

```
MIT License

Copyright (c) 2026 TheApache142857

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
