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
  line, a circle (counter-clockwise about its normal, at any slant; a closed circle is one
  edge on one vertex), a **section**, where a cylinder or a cone meets another surface off
  its axis (§5), or **degenerate**: an edge of no length at the point where a surface of
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
  not meet, and the operation is refused as a bug. Exact constructions stay at the base, and so do
  sections, each of whose points is found to rounding; curves fitted to others (B-splines,
  §9) will raise them where they need to.
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

A face of one is a region of the surface's `(θ, v)` plane. An edge along one of its straight
lines or meridian circles (constant `θ`) or its parallels or poles (constant `v`) is a
straight segment there; any other, a circle at a slant or a section (§5), is bent. `walk`
lays a loop out: `θ` (and `v` round a ring torus's tube) unwrapped as it goes round, a pole
run along as a segment of constant `v`, a bent edge as 128 of its points, each mapped to
`(θ, v)` and unwrapped next to the one before (`ParamPath`).

- **Area and mass.** By Green's theorem in `(θ, v)`, the integral of `f(θ) g(v)` over a face
  is `∮ F(θ) g(v) dv`, `F' = f`, to which, of the straight edges, only lines of constant `θ`
  contribute; `f` is 1, cos, sin and their squares and product, whose antiderivatives are
  closed forms, and the `v` integrals are taken by 16-point Gauss–Legendre quadrature
  (`RevolutionSums`): exact for a line's polynomials, to rounding for a circle's sines and
  cosines in pieces of an eighth of a turn. Along a bent edge `θ` and `v` both change, and
  `F(θ(t)) g(v(t)) v'(t)` is integrated over the edge's own parameter `t` by the same
  quadrature in 32 pieces, `θ(t)` and `v(t)` found on the exact curve (a plane's
  integrals along a section likewise, `PlaneSums::add_path`). Area, centroid, and by the divergence theorem
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
  it where its normal is along the axis (`Solid::bulges`). A section's own extremes are
  found by sampling it and refining each by golden-section search.

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
- `sweep` (`sweep.rs`): the profile carried along a path of lines and arcs, keeping its angle
  to the path (OpenCASCADE's corrected Frenet frame), which on a plane path is the path's own
  turn: each line of the path an extrusion of the profile where the line starts, its
  cylinders' axes the way the path runs (an extrusion's run against it), each arc a revolve
  about the arc's axis, its tori about that axis the way the path turns. The pieces are
  glued where one ends and the next begins (the profile's faces there dropped, their edges
  and corners shared, each piece's faces as they are: one per segment of the profile and of
  the path, as OpenCASCADE's); a closed path makes a ring. A path that turns sharply where
  two curves meet is refused (OpenCASCADE's frame jumps there), and so is a profile not in a
  plane holding an arc's axis where the arc starts.
- `shell` (`offset.rs`): a boolean of the solid with an offset of it. An offset keeps the
  solid's topology: each face's surface moved along its normal (a plane; a cylinder's or a
  cone's meridian line square to itself, its point at the origin moved with it as
  OpenCASCADE moves a cone's; a sphere's or a torus's tube radius), each vertex where its
  faces' moved surfaces meet again (Gauss–Newton from where their tangent planes put it;
  surfaces meeting smoothly there are one; two surfaces meet in a curve, along which the
  vertex keeps its place across; an apex or a pole where the moved surface has it), each
  edge on the curve its faces' moved surfaces meet in (a line, a parallel at its vertex's
  height, a meridian circle, a plane's circle on a sphere, a section of the moved
  surfaces). Faces meet as their surfaces extended do, never rounded. A surface shrunk to
  nothing, a face turned inside out, an edge run backwards: walls too thick for the part,
  refused in OpenCASCADE's words. Inwards the shell is the solid less its offset by the
  thickness inside, each face to open moved out instead so that the cut runs through it;
  outwards the solid grown by the thickness (the faces to open where they are) less the
  solid (those moved out). Names as OpenCASCADE's: the solid's faces keep theirs, the walls
  are named after the faces they are offset from, and so is the rim left round an opening
  (`OffsetFace`); with nothing to open, a void inside or a skin round the solid.

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
  where lines and the circles it shares an axis with are straight or arcs. It is cut along
  the curves it shares with the other solid's faces in a planar arrangement of segments and
  arcs (`arrangement.rs`: split where they cross or touch, welded, regions traced round each
  point by direction then curvature; candidate pairs found by sweeping their boxes, points
  welded through a grid). A curve that is bent in the chart (a circle at a slant, a section)
  is laid in as a polyline, its points 1/256 of a turn apart in its own parameter, the same
  for every copy of it, each segment tagged with the curve and the stretch of it it stands
  for; after the regions are traced, each run of tagged segments becomes one exact edge
  again, its ends found where the exact curves meet (Gauss–Newton from where the polylines
  did, `meeting_point`). The polyline only says which piece is where.
  Each piece is kept or dropped by where a point well inside it lies (on a face of the other
  solid facing the same way or the other, or else inside or outside it by rays cast against
  its exact surfaces, another ray when one grazes an edge). The pieces are welded, merged
  face by face in a chart where they lie in one piece (a face that goes round whole cut open
  at its angle zero, where its seam goes; one that does not left whole), only the regions
  the pieces cover kept, edges along one line or circle between the same two faces joined.
  A piece reaching a pole or an apex runs along a degenerate edge there, never shared. Each
  connected set of faces is a shell; a shell facing inwards is a void of the smallest solid
  it lies in.
- What meets in lines and circles: planes; cylinders square to the planes they meet or
  along them, and parallel to the other cylinders; cones, spheres and tori sharing their axis
  with the surfaces of revolution they meet, square to the planes they meet or holding them
  along their axis. A face's box on the far side of a plane meets nothing.
- **Off the axis** (`plane_section`, `crossing`): a plane meets a sphere in a circle at a
  slant, and two spheres meet in the circle in their radical plane; a whole one has its
  vertex where it crosses the surface's angle zero, where the seam goes (`seam_start`). A
  sphere is turned about every line through its centre, so one centred on another
  surface's axis is taken as sharing it. A plane slanted to a cylinder or a cone, a plane
  along a cone's axis but away from it, and a cylinder or a cone with a cylinder, a cone or
  a sphere on another axis meet in a **section** (`geometry.rs`): the curve traced by where
  each straight line of the carrier (the cylinder or cone, the narrower of two) meets the
  other surface, the root of a linear equation on a plane or a quadratic on the others that
  `branch` picks. It is parametrised by the carrier's angle, so it is exact at every point
  and its length, extremes and integrals are taken along it. A plane's section of a cylinder
  is an ellipse, and is reported as one (OpenCASCADE's `Geom_Ellipse`); the others are
  reported as B-splines, which is what OpenCASCADE fits to them.
- Two cylinders of one radius whose axes cross meet in two ellipses, in the two planes
  through the crossing that halve the angles between the axes, crossing each other where
  the cylinders touch; each is the section of one cylinder by its plane.
- A section that does not go all the way round its carrier is taken where a plane is the
  other surface (a parabola or a hyperbola on a cone): the stretches of it near the
  carrier's face, each run of angles where its point is within the face's stretch of `v`
  and a little more, its ends found by bisection, laid in as an open arc that the face's
  edges then cut (`stretches`).
- Faces whose surfaces meet only along edges both faces end at, or touch at a point, or
  meet away from both faces, have nothing to cut: over a fine grid of each face's chart,
  where the other surface's signed distance changes sign the crossing is found by bisection,
  and only one well inside both faces counts (`cross_inside`; along a carrier's lines for
  sections, `meets_only_at_edges`). Two blends meeting at a joint, a fillet's torus touching
  the walls beside it, are such.
- Where curves touch, rounding is not let through a square root: a line or a circle that
  touches a circle within the tolerance does so at one point, the foot of the touch
  (`arrangement::meet`, `meridian_crossings`, `plane_height_crossings`); round a point,
  directions within a millionth of a radian are one and the curve turning least left comes
  first; a hole is given to the region holding a point just outside the middle of its
  longest piece, never a corner a circle may touch.
- These still answer *not supported yet*, and the feature fails with that message, its body
  as it was: a torus that a plane or a surface off its axis crosses inside both faces;
  curves that turn back on themselves (a hole that only partly crosses another, whose curve
  goes round neither); surfaces that touch along a curve across their faces. A ring torus's
  face is cut at its tube's seam (v zero) as well as at its angle zero, and its pieces are
  laid out round the tube from a gap between them before they are merged, so that pieces
  either side of the seam become one face again.

### Chamfers and fillets

`blend.rs`. A blend is made by booleans with tools built exactly. For each edge, the tool is
the region its bevel or round takes off (a ridge, cut from the solid) or fills in (a
valley, fused onto it), swept along the edge: a straight edge between faces that do not
change along it (planes holding its direction, cylinders whose axes are parallel to it)
sweeps its cross-section square to it along it, a prism (`build::extrude`); an arc or a
circle about an axis its faces are turned about (planes square to it; cylinders, cones,
spheres and tori on it) turns its cross-section in the meridian half-plane about the axis
(`revolve::revolve`). The cross-section is bounded by the two faces' traces, from the edge to
where the blend leaves each face, and the bevel's line or the round's arc, tangent to both
(the round's centre where the traces moved by the radius cross): a bevel is a plane or a
cone, a round a cylinder or a torus. The tool's faces on the solid's faces cancel with them
in the boolean, its blend face stays. Its history names the blend face after its edge
(`InputEdge`), a corner's after its vertex (`InputVertex`), the rest after the faces they
are part of. What OpenCASCADE builds, observed as a black box, sets the rest:

- A blend runs on along the edges that carry the selected ones on smoothly (the straight
  sides and round ends of a slot, a pocket's rim round its rounded corners): one face per
  edge, the tools meeting square to the edges at the joints.
- Where a blend ends at a corner of three faces, its tool runs into the third face's plane:
  square to the edge it simply stops there, slanted it is cut by it (a cylinder's ellipse).
  Where two blended edges meet at a corner whose third edge is not, each runs into the face
  the other blends, and the two meet in a mitre (two rounds of one radius in an ellipse,
  §5). Where three meet, chamfers leave a triangle across the corner through the three
  points where their edges on each face cross, made by a tetrahedron cut away besides; and
  rounds between three planes a piece of the sphere touching all three, made by the box
  between the planes and the planes through its centre square to the edges, less the ball,
  the rounds' tools stopping at those planes.
- A selected edge whose faces meet smoothly has no corner to blend, and the blend fails
  saying so, in OpenCASCADE's words; one that would run off its faces (checked along where
  it leaves each face, as far as its tool goes) fails as too large.
- A torus's axis runs the way the cylinder it rolls on does when its tube's centre is
  outside the cylinder, the other way inside; a cone's runs towards its apex; a cylinder's
  along its edge from its start; a sphere's from where it touches the face the lowest
  blended edge does not. Blend faces have their angle zero at the edge's start.
- Not yet: blends where faces other than these meet (a round between two crossing
  cylinders needs a surface rolled along a curve, a B-spline in OpenCASCADE), a corner of
  more than three faces, a ridge and a valley meeting at a corner, three valleys' chamfers.

## 6. Display meshes

`tessellate.rs`. An arc is cut into chords within the deflection asked for, at most a twelfth
of a turn each; a section into chords halved until each is within it. A planar face is triangulated over its edges' chords (holes bridged to the
outer boundary, then ear clipping). A face on a surface of revolution is meshed in its
`(θ, v)` plane: cut into strips at every angle a point of its boundary is at and at steps fine
enough for its widest circle, each strip into trapezoids between the boundary's chords that
cross it, each trapezoid stitched into triangles between its two sides (rows of points on the
surface along a curved meridian, or the points of a boundary edge lying there). Every mesh
vertex on an edge is a point of that edge's polyline or of one of its chords, so faces meet
without gaps; triangles with two corners at a pole are left out.

## 7. Tests

- Unit tests in the crate: integrals against closed forms, quadrature, rays, meridian
  crossings, prisms, booleans of boxes and cylinders, voids, meshes of revolved faces, and
  holes across each other and through a round bar, whose wall areas are checked against the
  integral of the curve to 1e-7 and volume against the closed form to 1e-9; a bevel and a
  round of a box's edge, a pocket's corners rounded, against their closed forms.
- `crates/oxyde-tests/tests/native_kernel.rs` builds the same parts with both kernels and
  compares volumes, centres of mass, bounds, counts, every face, edge and vertex name, and
  each face's surface, area, centroid and normal or axis: extrusions, round holes and
  bosses, slots, patterns, revolves of every kind of profile each way about the axis for a
  whole turn and less, grooves and channels, every kind of hole, the demo flange, balls on
  and in blocks, balls, cones and tori cut in half along their axis, a rod with a ball on
  its end and one with a ball cut from it, a torus half sunk into a block, an O-ring's
  groove, a void, holes across each other at every offset, a hole through a round bar,
  round bars, bosses and cones cut at a slant, a ball cut beside its axis and a hole
  through one beside its centre; chamfers and fillets of a block's edge, of two and three
  edges at a corner, of every edge, of a hole's rims and a round boss's top and foot, of a
  slot's top edge (run round its ends), of the inner edge of an L, of a pocket's corners and
  then its rim, of a boss's upright edges down to the plate, of an edge ending on slanted
  faces, of a turned shaft's collar, cone and neck, of the demo flange; unequal chamfers;
  and the blends that cannot be made, too large or on a smooth edge; shells of a block and
  a round bar, opened at one face, two or none, inwards and outwards, of a block with a
  hole, of one with rounded edges, of a turned cup with a cone, and one too thick; sweeps of
  a circle along a line, an arc, round a circle and along a line and an arc, of a square and
  a slot along arcs and lines, one cut into a block, and one round a sharp corner, refused.
- How close is close: 1e-6 relative, except where OpenCASCADE bounds a face by a B-spline it
  fitted to an intersection, or a B-spline surface to a blend, where its areas and volumes
  are only as good as the fit (`FITTED`, 1e-4; §8). A blend face's axis is compared as a
  line (either way along it) and a corner's sphere by its centre (§8).
- `crates/oxyde-tests/tests/kernel_coverage.rs` is the measure of progress and the
  benchmark: a corpus of parts holding every feature type (the application's demo bracket
  and flange among them) rebuilt by both kernels, feature by feature. It reports what the
  native kernel builds, what it cannot yet and why, whether what it builds is what
  OpenCASCADE builds, and each kernel's rebuild and meshing time and triangle count
  (`kernel-coverage.md` in Cargo's directory for test files; `OXYDE_CORPUS` adds a folder of
  `.oxyprt` files). It fails when a part listed as built natively stops building or differs
  from OpenCASCADE's. On the Raspberry Pi, October 2026: 29 of 31 parts, 59 of 61 features,
  what is left a loft and a STEP import.
  The native kernel rebuilds what meets in lines and circles 5 to 40 times faster than
  OpenCASCADE, what meets in sections 1.1 to 3 times faster (two holes crossing: 66 ms
  against 194 ms; a round bar cut at a slant: 21 against 22), where laying the curves out
  and merging the pieces takes most of the time, and its blends 10 to 40 times faster (the
  demo bracket, a fillet and a chamfer on its plate and upright: 10 ms against 90), and its
  shells and sweeps 25 to 40 times (a block's shell: 1.5 ms against 58).

## 8. Where it differs from OpenCASCADE

- A hole exactly tangent to a face: OpenCASCADE splits the face along the line they touch
  on and gives that line to three faces; the native kernel leaves the face whole.
- Where two surfaces meet in a curve that is not a line, a circle or an ellipse,
  OpenCASCADE fits a B-spline to it, and its areas and volumes are a few millionths off the
  exact ones (a hole across another: 8e-6 of its wall's area); the native kernel's section
  is the exact curve, and its areas match the integral to 1e-9.
- A bevel between a cylinder and a cone on one axis is a cone; OpenCASCADE fits a B-spline
  surface to it.
- Two holes of one radius crossing: OpenCASCADE also cuts each of their two ellipses at the
  ends of its long axis, where only two edges meet between the same two faces; the native
  kernel cuts them only where a seam ends on them.
- The way a blend face's axis runs: a round's cylinder runs along its edge, a torus or a
  cone the way the edge's circle or the cylinder it leaves runs, and each kernel runs its
  edges its own way (OpenCASCADE's follow how it made them, the native kernel's how its
  booleans put them together); where those are a sketch's, an extrusion's or a revolve's,
  the native kernel's tori and cones run as OpenCASCADE's. A corner's sphere has its poles
  through one of its three faces, OpenCASCADE choosing by its own numbering of the edges:
  the same sphere, turned.
- A sweep along a path that turns sharply where two of its curves meet: OpenCASCADE builds
  a solid whose part after the corner has no volume; the native kernel refuses it.

## 9. Steps

Each step is tested against OpenCASCADE on the same parts in `oxyde-tests`.

0. **Done.** Main merged in, the coverage harness, workers.
1. **Done.** One surface-of-revolution type with each face's `(θ, v)` plane, tolerances on
   every vertex and edge, solids with voids; `revolve` with cones, spheres and tori; the
   booleans of what meets in lines and circles across a shared axis; the Revolve and Hole
   features and the demo flange.
2. **Partly done.** General intersections: circles at a slant, ellipses and sections (a
   plane slanted to a cylinder or a cone, holes crossing, holes of one size crossing, a hole
   through a ball or a round bar, a cone's parabolas and hyperbolas), laid out as tagged
   polylines and rebuilt as exact edges, with mass properties by quadrature along them;
   faces that only touch or meet along their edges. To do: tori crossed off their axis,
   curves that turn back on themselves, B-spline curves and surfaces (stored pcurves will be
   needed then), and one boolean engine instead of two.
3. **Done.** Chamfers and fillets: equal, two distances, a distance and an angle; along
   straight edges between planes and parallel cylinders and round edges between surfaces on
   one axis; runs of edges that carry on smoothly; corners of one, two (a mitre) and three
   blends (a triangle, a sphere); valleys filled in. The demo bracket.
4. **Done.** Shell, inwards and outwards, opened or closed, by offsets that keep the
   topology (planes, cylinders, cones, spheres, tori moved along their normals); sweep along
   paths of lines and arcs that turn smoothly, glued piece by piece.
5. Loft.
6. STEP: writing first, then reading (SolidCraft's Part 21 parser is a start).
7. Measure, planes normal to a curve, hidden-line drawings, a volume mesher.
8. The native kernel as the default, once every feature test of `oxyde-tests` passes on it.
