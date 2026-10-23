# Oxyde's own kernel (`oxyde-native-kernel`)

Status: **started**. This document records how the native kernel is built and what it does
so far; change it in the same commit as the code. ARCHITECTURE.md §2 places the crate, §4 is
the `Kernel` trait it implements, §9 *The kernel* how the application chooses it, and §10 the
roadmap row.

## 1. Decisions

- A B-rep kernel of Oxyde's own, in Rust, a second implementation of `Kernel` beside
  `oxyde-occt`. The end goal is that it becomes the kernel Oxyde uses, with OpenCASCADE kept
  as the second option; until it builds what OpenCASCADE builds, OpenCASCADE stays the
  default (owner's decision, October 2026).
- Written from the mathematics. OpenCASCADE (LGPL-2.1) is used as a black box only: the
  tests build the same parts with both kernels and compare what they give; its source is
  never read or adapted. Code may be adapted from SolidCraft's kernel (MIT OR Apache-2.0)
  where it does not depend on truck, with its notice kept.
- OpenCASCADE's topology is the reference where it is reasonable to follow: the same faces,
  edges and vertices for the same timeline, so that persistent names resolve the same way
  under both kernels. Where they differ it is written down (§8).
- Tolerances are carried by each vertex and each edge, as OpenCASCADE's are (owner's
  decision, October 2026), not one value for the whole model.

## 2. The B-rep

`brep.rs`. A `Shape` is the solids an operation made (the regions of a profile, extruded,
make several). A `Solid` holds its vertices, edges and faces, numbered in the order the
operation made them, and its shells: the outer one, then each void's, a closed shell whose
normals point into the void (`shells` empty means one shell of every face).

- A **vertex** is a point and a tolerance.
- An **edge** runs from a start vertex to an end vertex along a curve (`geometry.rs`): a
  line, a circle (counter-clockwise about its normal; a closed circle is one edge on one
  vertex), or **degenerate**: an edge of no length at the point where a surface of
  revolution meets its axis (a cone's apex, a sphere's pole), which its face's boundary runs
  along as it turns about the axis there, as OpenCASCADE's degenerate edges do. It carries
  its turn and the axis it turns about, belongs to one face and is run along once.
- A **face** lies on a surface, a plane (its normal out of the solid) or a surface of
  revolution, and is bounded by loops of edge uses: the outer one counter-clockwise seen
  from outside, then its holes, clockwise. A face that goes round its axis whole has a seam,
  an edge it runs along once each way, as OpenCASCADE has it.
- **Tolerances.** Each vertex and each edge says how far the geometry meeting there may be
  from its point or its curve. They are measured, not assumed (`Solid::settle_tolerances`):
  a vertex's is the furthest the surfaces of its faces and the curves of its edges are from
  its point, an edge's the furthest its faces' surfaces are from its curve; never less than
  `BASE_TOLERANCE` (1e-6 mm). More than `MAX_TOLERANCE` (1e-3 mm) means geometry that does
  not meet, and the operation is refused as a bug. Exact constructions stay at the base;
  approximate intersection curves (§9, step 2) will raise them where they need to.
- Every result is checked before the kernel keeps it (`Solid::check`): every loop closed,
  every edge run along once each way (a degenerate edge once), corners on their face's
  surface and ends on their edge's curve within their tolerances, unit normals, proper
  frames, each shell closed on its own, every face's integrals computable. A result that
  fails is an internal error: a wrong result that looks right is worse.

The kernel's shapes live in one store every `NativeKernel` of the process shares (a map
behind a read-write lock, each shape behind an `Arc`, the lock held only to find, add or drop
one), so `open_worker` gives a worker and the application's jobs run off the UI thread with
it, as they do with OpenCASCADE.

## 3. Surfaces of revolution

`revolution.rs`. A cylinder, a cone, a sphere and a torus are one type, `Revolution`: an
axis, an angle-zero direction square to it, and a meridian, the curve turned about the axis
in the half-plane of distance `ρ` from it and height `z` along it: a line
`ρ = radius + slope v, z = v` (a cylinder with no slope, else a cone) or a circle
`ρ = center + radius cos v, z = radius sin v` (a sphere centred on the axis, else a torus).
A point is `origin + ρ(v) e(θ) + z(v) a` with `e(θ) = cos θ x + sin θ y`; its face's normal
is `(z' e − ρ' a)` normalised, or its opposite (`outward`).

A face of one is a region of the surface's `(θ, v)` plane. What the kernel builds so far
meets these surfaces only along lines of constant `θ` (their straight lines, their meridian
circles) and lines of constant `v` (their parallels, their poles), so every edge of such a
face is a straight segment there, and `walk` lays a loop out: `θ` (and `v` round a ring
torus's tube) unwrapped as it goes round, a pole run along as a segment of constant `v`.

- **Area and mass.** By Green's theorem in `(θ, v)`, the integral of `f(θ) g(v)` over a face
  is `∮ F(θ) g(v) dv`, `F' = f`, to which only the boundary's lines of constant `θ`
  contribute; `f` is 1, cos, sin and their squares and product, whose antiderivatives are
  closed forms, and the `v` integrals are taken by 16-point Gauss–Legendre quadrature
  (`RevolutionSums`): exact for a line's polynomials, to rounding for a circle's sines and
  cosines in pieces of an eighth of a turn. Area, centroid, and by the divergence theorem
  the solid's volume and centre of mass, come out as OpenCASCADE gives them to 1e-9.
- **Coaxial surfaces meet where their meridians cross** (`coaxial_meridian`,
  `meridian_crossings`): another surface's meridian is laid in this one's half-plane and
  line–line, line–circle and circle–circle crossings give the circles the two share. A plane
  square to the axis is the line of its height; a plane holding the axis cuts along the
  meridians at the two angles square to its normal.
- **Rays** (inside or outside, §5) meet a cylinder, a cone or a sphere in the roots of a
  quadratic, a torus in those of a quartic, bracketed along the ray within the torus's
  bounding sphere and refined by bisection.
- **Bounds.** A plane, a cylinder or a cone reaches its extremes along a model axis on its
  edges; a sphere or a torus can bulge out between them, so its face adds the points inside
  it where its normal is along the axis (`Solid::bulges`).

## 4. Making solids

- `make_box`, `extrude` (`build.rs`): a prism over each region of a planar profile of lines,
  arcs and circles, its side faces in the profile's order, then the start and end caps; an
  arc sweeps a cylinder whose axis goes through the arc's centre on the sketch plane, against
  the sweep, as OpenCASCADE reports it.
- `transform`: a moved or mirrored copy; a mirror turns every loop round.
- `revolve` (`revolve.rs`): each segment of the profile sweeps a face: a line square to the
  axis a plane (a disc, an annulus, a sector of one), a line along it a cylinder, a slanted
  one a cone, an arc centred on the axis a sphere, any other arc a torus (an arc whose circle
  reaches the axis off its centre, a spindle or horn torus, is not supported yet); a line on
  the axis sweeps nothing. The topology is OpenCASCADE's (`BRepPrimAPI_MakeRevol`, observed
  as a black box):
  - each corner off the axis sweeps a circle about the axis (an arc for less than a whole
    turn) from the corner to its copy at the end angle, run counter-clockwise about the
    axis; a corner on the axis that a cone or a sphere reaches is a degenerate edge, one a
    disc reaches is nothing for a whole turn;
  - each segment off the axis is an edge where it is, and for less than a whole turn at the
    end angle too; for a whole turn a plane's segment is no edge (an annulus is bounded by
    its two circles) and a curved face's is its seam. A segment on the axis is an edge both
    caps share;
  - a surface's axis runs the way its parameter `v` grows along its segment: the segment's
    own way for a line, counter-clockwise in the meridian half-plane for an arc (so an arc
    on the inside of a torus's tube gives the axis the other way from one on the outside);
    its origin is the foot on the axis of the segment's start (a line) or of its circle's
    centre (an arc), and its angle zero points from the axis to the profile;
  - faces come in the profile's order, then for less than a whole turn the start cap (facing
    against the turn) and the end cap.

## 5. Booleans

`boolean.rs` dispatches and checks: a result must be closed (checked when kept), a fuse
never loses volume, a cut or a common never gains any, nothing reaches beyond the inputs.

- **Straight-sided solids** go through BSP trees (`bsp.rs`, Thibault & Naylor 1987 in the
  form csg.js made known: a polygon in a splitting plane counts as outside when it faces the
  same way and inside when it faces the other, so faces in contact cancel), the pieces glued
  back into faces plane by plane (`assemble.rs`), coplanar touching faces merged and edges in
  line joined, as OpenCASCADE's `SimplifyResult` leaves them. A result with a void is made the
  other way.
- **Everything else** (`curved.rs`) is cut face by face. Each face is laid out flat in its
  chart: a plane's coordinates, or a surface of revolution's `(k θ, m v)`, scaled to lengths,
  where everything it meets is straight. It is cut along the curves it shares with the other
  solid's faces in a planar arrangement of segments and arcs (`arrangement.rs`: split where
  they cross or touch, welded, regions traced round each point by direction then curvature).
  Each piece is kept or dropped by where a point well inside it lies (on a face of the other
  solid facing the same way or the other, or else inside or outside it by rays cast against
  its exact surfaces, another ray when one grazes an edge). The pieces are welded, merged
  face by face in a chart where they lie in one piece (a face that goes round whole cut open
  at its angle zero, where its seam goes; one that does not left whole), only the regions
  the pieces cover kept, edges along one line or circle between the same two faces joined.
  A piece reaching a pole or an apex runs along a degenerate edge there, never shared. Each
  connected set of faces is a shell; a shell facing inwards is a void of the smallest solid
  it lies in.
- What meets in lines and circles only: planes; cylinders square to the planes they meet or
  along them, and parallel to the other cylinders; cones, spheres and tori sharing their axis
  with the surfaces of revolution they meet, square to the planes they meet or holding them
  along their axis. A face's box on the far side of a plane meets nothing. Anything else
  (two holes crossing, a cylinder slanted to a plane, a cone or a sphere cut along its axis
  but away from it) answers *not supported yet*, and the feature fails with that message, its
  body as it was. A ring torus's face is cut at its tube's seam (v zero) as well as at its
  angle zero, and its pieces are laid out round the tube from a gap between them before they
  are merged, so that pieces either side of the seam become one face again.

## 6. Display meshes

`tessellate.rs`. An arc is cut into chords within the deflection asked for, at most a twelfth
of a turn each. A planar face is triangulated over its edges' chords (holes bridged to the
outer boundary, then ear clipping). A face on a surface of revolution is meshed in its
`(θ, v)` plane: cut into strips at every angle a point of its boundary is at and at steps fine
enough for its widest circle, each strip into trapezoids between the boundary's chords that
cross it, each trapezoid stitched into triangles between its two sides (rows of points on the
surface along a curved meridian, or the points of a boundary edge lying there). Every mesh
vertex on an edge is a point of that edge's polyline or of one of its chords, so faces meet
without gaps; triangles with two corners at a pole are left out.

## 7. Tests

- Unit tests in the crate: integrals against closed forms, quadrature, rays, meridian
  crossings, prisms, booleans of boxes and cylinders, voids, meshes of revolved faces.
- `crates/oxyde-tests/tests/native_kernel.rs` builds the same parts with both kernels and
  compares volumes, centres of mass, bounds, counts, every face, edge and vertex name, and
  each face's surface, area, centroid and normal or axis: extrusions, round holes and
  bosses, slots, patterns, revolves of every kind of profile each way about the axis for a
  whole turn and less, grooves and channels, every kind of hole, the demo flange, balls on
  and in blocks, balls, cones and tori cut in half along their axis, a rod with a ball on
  its end and one with a ball cut from it, a torus half sunk into a block, an O-ring's
  groove, a void.
- `crates/oxyde-tests/tests/kernel_coverage.rs` is the measure of progress and the
  benchmark: a corpus of parts holding every feature type (the application's demo bracket
  and flange among them) rebuilt by both kernels, feature by feature. It reports what the
  native kernel builds, what it cannot yet and why, whether what it builds is what
  OpenCASCADE builds, and each kernel's rebuild and meshing time and triangle count
  (`kernel-coverage.md` in Cargo's directory for test files; `OXYDE_CORPUS` adds a folder of
  `.oxyprt` files). It fails when a part listed as built natively stops building or differs
  from OpenCASCADE's. On the Raspberry Pi, October 2026: 20 of 28 parts, 46 of 55 features;
  the native kernel rebuilds what it builds 5 to 35 times faster (the demo flange, whose six
  holes are cut one boolean at a time, is the slowest).

## 8. Where it differs from OpenCASCADE

- A hole exactly tangent to a face: OpenCASCADE splits the face along the line they touch
  on and gives that line to three faces; the native kernel leaves the face whole.

## 9. Steps

Each step is tested against OpenCASCADE on the same parts in `oxyde-tests`.

0. **Done.** Main merged in, the coverage harness, workers.
1. **Done.** One surface-of-revolution type with each face's `(θ, v)` plane, tolerances on
   every vertex and edge, solids with voids; `revolve` with cones, spheres and tori; the
   booleans of what meets in lines and circles across a shared axis; the Revolve and Hole
   features and the demo flange.
2. General intersections: ellipses (a plane slanted to a cylinder), curves traced point by
   point where two surfaces cross (two holes crossing), B-spline curves and surfaces, mass
   properties by quadrature over any boundary, one boolean engine instead of two.
3. Chamfers, then fillets (the demo bracket): planes and cones for chamfers; cylinders
   between planes, tori where a plane meets a cylinder, then chains of tangent edges and the
   corners where three fillets meet.
4. Shell (an offset of a plane, a cylinder, a cone, a sphere or a torus is one of the same
   kind), then sweep along paths of lines and arcs.
5. Loft.
6. STEP: writing first, then reading (SolidCraft's Part 21 parser is a start).
7. Measure, planes normal to a curve, hidden-line drawings, a volume mesher.
8. The native kernel as the default, once every feature test of `oxyde-tests` passes on it.
