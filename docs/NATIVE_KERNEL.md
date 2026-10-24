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
  its turn and the axis it turns about, belongs to one face and is run along once; or
  **traced**, where two surfaces cross in a curve no closed form or single angle follows
  (§5); or a **B-spline** curve.
- A **face** lies on a surface, a plane (its normal out of the solid), a surface of
  revolution or a B-spline surface, and is bounded by loops of edge uses: the outer one counter-clockwise seen
  from outside, then its holes, clockwise. A face that goes round its axis whole has a seam,
  an edge it runs along once each way, as OpenCASCADE has it.
- **Tolerances.** Each vertex and each edge says how far the geometry meeting there may be
  from its point or its curve. They are measured, not assumed (`Solid::settle_tolerances`):
  a vertex's is the furthest the surfaces of its faces and the curves of its edges are from
  its point, an edge's the furthest its faces' surfaces are from its curve; never less than
  `BASE_TOLERANCE` (1e-6 mm). More than `MAX_TOLERANCE` (1e-3 mm) means geometry that does
  not meet, and the operation is refused as a bug. Exact constructions stay at the base, and so do
  sections, each of whose points is found to rounding; curves fitted to others (B-splines,
  §9) will raise them where they need to. A solid read from a STEP file may need more, as
  much as the file says its geometry is accurate to (`settle_tolerances_within`, §4).
- **B-splines** (`nurbs.rs`), curves and surfaces, rational or not, of degrees up to 31,
  written from the mathematics (the algorithms of Piegl and Tiller's *The NURBS Book*):
  evaluation with derivatives (the basis functions' table on the stack), knot insertion, the
  nearest point (Newton's method, a long step halved while it leads further off, from the
  last point found on the same surface, kept when it converged inside the domain and as
  near as the grid's nearest, else from the nearest of a grid of its points, the three
  nearest and those nearly as near (on a narrow band the points across it are all about
  as near); a tenth of the domain past an open edge, where the last span runs on, so that
  a distance changes sign smoothly across the surface near its edges), where a line
  crosses a surface (its Bézier pieces halved while their poles' boxes meet the line, then
  Newton's method). A B-spline's poles are kept once for the life of the process (the same
  B-spline made again is found again), and the kernel's curves and surfaces refer to them
  with the rigid motion that places them: moving or patterning a part never copies them,
  and its faces' integrals are moved with it, not taken again. A face on a B-spline surface
  is a region of its `(u, v)` plane, its edges taken onto the surface by the nearest point
  (`spline_face.rs`); where a loop goes through a pole of the surface (a side of its
  domain drawn to a point), it runs along that side from one edge to the next. Its
  integrals: Green's theorem, inside along the parameter whose knot spans across the face
  are fewer, from the face's own edge; along the edges adaptively, to a hundred-billionth
  of the face's size or a thousandth of what its edges' tolerances leave its area
  uncertain by, whichever is larger (a face read from a file is only as exact as its
  geometry).
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

- `loft` (`loft.rs`), with the B-splines of `nurbs.rs`: what OpenCASCADE's
  `BRepOffsetAPI_ThruSections` builds, observed as a black box (its output's poles, knots
  and weights read through its public API, never its source), followed to the pole:
  - **Sections matched.** Sections with as many edges each are matched corner to corner,
    each one's start and way round chosen to keep its corners nearest the previous one's
    (the least sum of the distances, of every start and both ways). Otherwise each section
    is cut at the polar angles, about its centroid along its perimeter, of every section's
    corners, so that all have a corner at each angle, all run counter-clockwise along the
    loft and start at the first section's first corner's angle. The angles are taken in
    each section's frame: the first plane's, carried from plane to plane by the least
    rotation between their normals.
  - **Edges as polynomials.** A line is a B-spline of degree one. An arc is the polynomial
    OpenCASCADE makes of it: of the least degree from 3 to 14 that keeps within 1e-7 mm of
    the arc at the point of the same fraction of its angle, with the ends and their
    tangents exact and the rest the least-squares fit; an arc no degree up to 14 follows is
    halved (a whole circle of radius 100 is two halves of degree 11).
    `nurbs::polynomial_arc` reproduces OpenCASCADE's poles to 1e-10.
  - **Ruled lofts, and lofts of two sections.** A face for each pair of edges from one
    section to the next, straight across: a cone or a cylinder between arcs of circles on
    one axis at the same angles round it (its axis along the loft), else a B-spline surface
    of degree one across, the higher of the two edges' degrees along. The sections' edges
    are their exact lines and circles (the B-spline faces' own edges within 1e-7 of them);
    the edges up the loft are lines, as B-splines of degree one.
  - **Smooth lofts** of three sections to nine: one surface for each edge through every
    section, of degree one less than the number of sections across them, through each
    section at its share of the polygon of the sections' poles (the root mean square of the
    distances between corresponding poles: every section's edges raised to the highest
    degree among them and their knots to one set, each section one curve of all its
    edges). The end sections' edges are the surfaces' B-spline curves there, the edges up
    the loft their B-spline curves across. Through ten sections or more OpenCASCADE
    approximates; the native kernel says it cannot yet.
  - Faces in OpenCASCADE's order: the side faces span after span, edge after edge from
    the first corner, then the first section's cap and the last's; a side face holding the
    middle of one of the first section's edges is named after that edge.

### STEP files

`step/`: the exchange structure of ISO 10303-21 (`p21.rs`: complex instances, typed
parameters, the `\X2\`, `\X\` and `\S\` encodings of strings) read and written.

- **Writing** (`export_step`, `write.rs`) as OpenCASCADE writes, from its files read as a
  black box: AP214, a product for each shape, its solids `MANIFOLD_SOLID_BREP` (or
  `BREP_WITH_VOIDS`) in millimetres, uncertainty 1e-7 (or a solid's largest tolerance where
  that is beyond the kernel's own 1e-3 mm, a solid read from a file of coarser accuracy, so
  that its file reads back: an assembly's parts are written so and read again when it is
  opened); every face an `ADVANCED_FACE` along
  its surface's normal or not; degenerate edges left out (a cone's apex, a sphere's poles; a
  whole sphere bounded by a `VERTEX_LOOP`). Planes, cylinders, cones (the axis turned where
  the slope is negative), spheres, tori (a spindle torus a `DEGENERATE_TOROIDAL_SURFACE`) and
  B-spline surfaces as they are; lines, circles, ellipses (a plane's sections of a
  cylinder) and B-spline curves as they are, other sections and traced curves as cubic
  B-splines through their points within 1e-7 of the part's size. Colours of solids and
  faces, see-through or not; materials (name and density) as Fusion 360 writes them.
- **Reading** (`import_step`, `read_step_assembly`, `read.rs`) of AP203, AP214 and AP242:
  every product's solids (`MANIFOLD_SOLID_BREP`, `BREP_WITH_VOIDS`, `FACETED_BREP`, the
  closed shells of a `SHELL_BASED_SURFACE_MODEL`), placed by the assembly's occurrences
  (`NEXT_ASSEMBLY_USAGE_OCCURRENCE` with `ITEM_DEFINED_TRANSFORMATION`s), each in its
  representation's units (SI with any prefix, or converted: inches, degrees); the colours
  of solids, shells and faces, a face's own over its solid's; the material of the first
  product with solids.
  - Surfaces as they are; a surface of linear extrusion the plane or cylinder it is (a line
    swept, a circle along its axis), else the B-spline surface of degree one across,
    spanning its face; a surface of revolution the plane, cylinder or cone (a line in a
    plane through its axis, or square to it), sphere or torus (a circle in such a plane) it
    is, else the rational B-spline surface it is exactly (its parameters the turn and then
    the curve, ISO 10303-42's: its normal the turn's tangent across the curve's); offsets of
    planes and surfaces of revolution; a trimmed surface its basis. Curves: lines,
    circles, ellipses, B-splines, polylines (B-splines of degree one); a trimmed curve or a
    surface curve its basis, the way it runs kept.
  - **Exact B-splines** (`exact.rs`). A B-spline curve that is a line or a circle, a
    B-spline surface that is a plane, a cylinder, a cone, a sphere or a torus (circles on
    one axis across it, a line or a circle along them), within a billionth of its size at
    its knots and between, is read as what it is: a rational B-spline of degree two holds a
    circle exactly, and some systems write every round surface so. One that only follows
    them (a polynomial within 1e-7 of a circle) is kept.
  - **Faces settled** to the kernel's, once their solid is read. Loops round a surface of
    revolution's axis without a seam (other systems than OpenCASCADE mostly write a
    cylinder's side as its two circles, a cone's point as its circle and a `VERTEX_LOOP` at
    the apex) are joined by one, up a straight line or a meridian, where both loops have a
    vertex (else where the first has one, the other's edge split there), clear of the
    face's holes; a face round to a pole gets its seam to it; loops round a torus's tube are
    joined along a parallel; a whole sphere bounded by a vertex or by holes alone gets its
    seam and both poles; two loops round a B-spline surface that closes (a tube's ends),
    along its curve of constant parameter. Where a loop reaches an apex or a pole, the
    degenerate edge is put back, turning about the axis as far as the loop does there
    (counter-clockwise in `(θ, v)` for a face along its surface's natural normal); edges of
    no length that some systems write there are left out. A sphere's axis is turned to keep
    its poles clear of the face's edges (a box's rounded corner, its arcs through the
    file's pole) unless the file puts a vertex at a pole. A straight edge on a surface of
    revolution that is not one of its straight lines (across a wide cone, within the
    file's accuracy) and every straight edge on a B-spline surface become B-splines of
    degree one. The outer loop first: the widest, the file's outer bound only among loops
    as wide (some writers call a hole the outer bound). A shell facing the wrong way is
    turned.
  - **Tolerances**: each vertex's and edge's measured as for every solid
    (`settle_tolerances_within`), up to four times the accuracy the file states for its
    geometry (`GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT`; Creo says 1.5e-3 mm), and at least the
    kernel's own 1e-3 mm; then the solid is checked as every result is. Laid out in a
    surface of revolution's `(θ, v)` plane, a loop closes within its edges' tolerances in
    `v` as in `θ` (a corner 2e-5 above its arc's other end), and an arc within tolerance of
    a parallel but about a centre of its own ends at its end's own angle, not after its own
    sweep (Fusion 360 bounds some faces by arcs microns off their surface).
  - **Assemblies**: each product once; a part's solids written as a STEP file of their own
    in its own coordinates, with its colours and material; an assembly its children placed;
    a mirrored occurrence a part of its own, the mirror applied; a product with solids and
    children its solids a part beside the children; several top-level products under an
    unnamed assembly; a file without products every solid where it is.
- IGES is not written yet.
- Faces on B-spline surfaces with a pole (a side of the domain drawn to a point, as a
  corner's blend between three rounds) run along that side from one edge to the next in
  `(u, v)` (`spline_face::gaps`), their integrals with it.

## 5. Booleans

`boolean.rs` dispatches and checks: a result must be closed (checked when kept), a fuse
never loses volume, a cut or a common never gains any, nothing reaches beyond the inputs.

- **Every solid** (`curved.rs`), flat-faced or curved, is cut face by face: one engine for
  all (a BSP-tree one for flat-faced solids was dropped once this one was as good, and
  still 10 to 30 times faster than OpenCASCADE's on them). Each face is laid out flat in its
  chart: a plane's coordinates, a surface of revolution's `(k θ, m v)`, a B-spline
  surface's `(k (u − u₀), m (v − v₀))`, scaled to lengths (`k` and `m` the B-spline's mean
  speeds), where lines and the circles it shares an axis with are straight or arcs. It is cut along
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
- **Traced curves** (`trace.rs`). Where none of these follows the curve (a torus a plane
  or a surface off its axis crosses, a hole that only partly crosses another, whose curve
  turns back along either one's lines and goes round neither, a hole across a fillet), it
  is traced. A traced curve is held by what makes it: the two surfaces, the point it
  starts at, the way it runs from there (along the cross product of the normals or
  against it), its length, whether it closes; so it is a small value that copies, and two
  copies of it are one curve. Its points are marched from the start, a step along the
  tangent and Newton's method back onto both surfaces in the plane square to it, the steps
  shortened where it bends, and kept in a cache that the curve itself can always fill
  again, the same every time. Its parameter is the length along those points; a point at
  any parameter is the cubic between the two marched points about it moved onto both
  surfaces again, exact to rounding as a section's points are, and lengths, extremes and
  integrals are taken along it. Where to start is found over a grid of each face's chart:
  where the other surface's signed distance changes sign, by bisection. Each curve is
  traced both ways from the first such point not already on one, to where it closes or
  leaves a box round both faces; the curves found for two surfaces are kept for the rest
  of the boolean, so that every pair of faces on them cuts along the same ones. OpenCASCADE
  fits B-splines to these curves, and they are reported as B-splines.
- A curve of two faces is laid into each only where it lies inside the other, each stretch
  ending exactly where it crosses the other face's edges (`clipped_into`, Gauss–Newton on
  the two curves; a face's seam is no edge to cross), so that where the other face meets a
  face beside it smoothly (a fillet and its wall), the curves on the two end at one point,
  not both running on side by side; a stretch is inside or out whole, told at a few of its
  points. Points closer than twice the tolerance are one, in the arrangement as where the
  cuts are found.
- What is left of a ring torus's face that went round its axis but no longer does, while
  it still goes round its tube, keeps an edge at its angle zero, cut there in two, as
  OpenCASCADE leaves it; a piece round neither, or of any other surface, stays whole. A ring
  torus's face is laid out round its tube from a gap between its pieces before they are
  merged, so that pieces either side of the tube's seam become one face again.
- **B-spline faces** meet every other face in traced curves: found from the crossings
  over both faces' grids (against a B-spline's distance only near its face's box, half as
  fine: the grid on the B-spline face itself, against the other surface, is the fine one),
  each by regula falsi; a curve running past a B-spline's reach ends there, and one seed
  near a curve traced already (within a step of it) starts none. Their charts close round
  where the surface does, with a seam at `u₀` (an edge along `v`, the surface's own curve
  there); where a cut runs straight across a B-spline face's chart along `u` or `v`, its
  edge is the surface's curve there, or a line where the surface holds one (a ruled face's
  rulings). Rays to tell inside from outside cross B-spline faces where their Bézier
  pieces say. Two faces on one B-spline surface are one surface.
- These still answer *not supported yet*, and the feature fails with that message, its body
  as it was: surfaces that touch where they cross (a hole whose wall touches a fillet just
  where it crosses it, the curve through the touching point: the tracing stops where the
  normals are within a thousandth of a radian), surfaces that touch along a curve across
  their faces, B-spline faces that touch other faces where they cross them, shells and
  blends of B-spline faces.

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
without gaps; triangles with two corners at a pole are left out. A face on a B-spline surface
is meshed the same way in its `(u, v)` plane, its edges' points taken onto the surface, its
strips and rows at steps within the deflection by the surface's second derivatives, and
turning no more than a twelfth of a turn each (second over first derivative at the same
point, where the surface's line has a length: not at a pole). An edge along one `u` within
a thousandth of a step is a side of the face at that `u`, with the ends of the edges meeting
it, rather than a strip at each of its points (an edge along a side of the domain projects
onto it only within its tolerance). A B-spline edge's polyline is two steps a span at least,
halved while its middle or its quarters stand further than the deflection off the chord:
exporters write rounded edges as curves of hundreds of spans, and sixteen points a span gave
a CATIA part's 5 mm² face 269,315 triangles. A face the strips cannot mesh (some faces of
STEP files, bounded by curves their writer fitted) is triangulated in its parameter plane
from its boundary's points (ear clipping), its triangles' sides inside the face halved
where they stand further than the deflection off the surface, a triangle at its centroid
where none does, never cutting the boundary, so that it meets its neighbours all the same.

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
  through one beside its centre; tori cut by planes beside their axis and holes through
  their side; holes across a rounded rim, and one touching the round where it crosses it,
  refused; chamfers and fillets of a block's edge, of two and three
  edges at a corner, of every edge, of a hole's rims and a round boss's top and foot, of a
  slot's top edge (run round its ends), of the inner edge of an L, of a pocket's corners and
  then its rim, of a boss's upright edges down to the plate, of an edge ending on slanted
  faces, of a turned shaft's collar, cone and neck, of the demo flange; unequal chamfers;
  and the blends that cannot be made, too large or on a smooth edge; shells of a block and
  a round bar, opened at one face, two or none, inwards and outwards, of a block with a
  hole, of one with rounded edges, of a turned cup with a cone, and one too thick; sweeps of
  a circle along a line, an arc, round a circle and along a line and an arc, of a square and
  a slot along arcs and lines, one cut into a block, and one round a sharp corner, refused;
  lofts, ruled and smooth, between squares, circles (cones and cylinders), a circle and a
  square, a rectangle and a circle, a triangle and a square, slots, rectangles across each
  other, through three to seven sections of squares, circles, a rectangle, a circle and a
  square, slots, and to sections on planes at an angle; a loft on a block, one cut through
  a block, holes across a loft and its corner, a loft cut by a slab beside its axis.
- STEP: the files OpenCASCADE writes of such parts (balls, cones, tori, fillets at a corner
  of three, chamfers, holes crossing, lofts) read by the native kernel and compared with
  what OpenCASCADE reads of them; the native kernel's own files read back, with colours
  and materials, and by OpenCASCADE; small files in `tests/data/step`, written by
  `make.py` the ways other systems write such solids (a cylinder without a seam, a pin in
  inches and degrees with a point bounded by a vertex loop, a hemisphere whose sphere's
  axis runs through its edge, a dome of a rational B-spline, an assembly of a part used
  twice with colours and a material, a half cylinder written as loosely as its stated
  accuracy of 0.01 mm allows, read whole and through its part's own file), against
  OpenCASCADE and their closed forms; the assembly's tree against OpenCASCADE's.
  `step_corpus.rs` (ignored, run by hand) opens any STEP file with one kernel as the
  application does (the tree, each part's own file, the whole file measured, meshed at a
  thousandth of its size, the mesh's open sides and volume) and prints each stage as a JSON
  line, one process a file and kernel, for a script to compare. On the owner's PC, October
  2026, 30 files (Fusion 360, SolidWorks 2007 to 2019, CATIA V5, Creo, KiCad, FreeCAD,
  ASCON, OpenCASCADE): of the 29 with solids OpenCASCADE reads all and the native kernel 27,
  with the same solids and faces, 21 within 1e-5 of OpenCASCADE's volume and 26 within
  1e-4 (a Fusion 360 connector's rational B-splines 5e-4, §8), 5 times faster (median); not
  read: a Fusion 360 body written with a vertex 0.57 mm off a face (OpenCASCADE takes up to
  1 mm) and the hackintosh case (§10). Its meshes are half OpenCASCADE's on prismatic
  parts and up to 5.3 times on B-spline ones; on several files they hold a volume 10 to
  200 percent off the solid's, where OpenCASCADE's are within 2e-3 (curved faces' meshes,
  not planar ones: to be found). Beside the tests, in October 2026, the 98 STEP
  files found on the Raspberry Pi (not in the repository: KiCad's libraries and board
  exports, EasyEDA's parts, Creo's assemblies, SolidWorks' and other systems' parts) were
  each read and meshed by the native kernel and read by OpenCASCADE: every one read and
  meshed, 92 within 1e-5 of OpenCASCADE's volume, two of Creo's within 1.5e-5 (the
  accuracy those files state), and four whose rational B-spline surfaces OpenCASCADE
  integrates 0.5 to 0.9 percent off (§8), the native kernel exactly. A board's export of
  8,074 faces read in 18 s against OpenCASCADE's 120; Creo's assemblies of 1,150 faces in
  15 to 17 s against 23.
- In the crate, the B-splines: a rational circle and cylinder against their closed forms,
  derivatives against differences, knot insertion and degree raising keeping the curve,
  nearest points and a line's crossings; OpenCASCADE's polynomial arcs to its poles; a smooth
  loft's faces' integrals against a brute-force double integral of their surfaces (1e-10),
  its volume against OpenCASCADE's and a loft of circles' against the closed form, its mesh
  within the deflection.
- How close is close: 1e-6 relative, except where OpenCASCADE bounds a face by a B-spline it
  fitted to an intersection, or a B-spline surface to a blend, where its areas and volumes
  are only as good as the fit (`FITTED`, 1e-4; §8). A blend face's axis is compared as a
  line (either way along it) and a corner's sphere by its centre (§8). A loft, whose
  B-splines the native kernel makes as OpenCASCADE does, is held to 1e-6 but for its
  B-spline faces' areas and centroids (1e-3: OpenCASCADE's own are off, §8).
- `crates/oxyde-tests/tests/kernel_coverage.rs` is the measure of progress and the
  benchmark: a corpus of parts holding every feature type (the application's demo bracket
  and flange among them) rebuilt by both kernels, feature by feature. It reports what the
  native kernel builds, what it cannot yet and why, whether what it builds is what
  OpenCASCADE builds, and each kernel's rebuild and meshing time and triangle count
  (`kernel-coverage.md` in Cargo's directory for test files; `OXYDE_CORPUS` adds a folder of
  `.oxyprt` files). It fails when a part listed as built natively stops building or differs
  from OpenCASCADE's. On the Raspberry Pi, October 2026: 31 of 31 parts, 61 of 61
  features.
  The native kernel rebuilds what meets in lines and circles 5 to 40 times faster than
  OpenCASCADE, what meets in sections 1.1 to 3 times faster (two holes crossing: 73 ms
  against 183 ms), where laying the curves out and merging the pieces takes most of the
  time, its blends 7 to 40 times faster (the demo bracket, a fillet and a chamfer on its
  plate and upright: 13 ms against 89), its shells and sweeps 15 to 30 times (a block's
  shell: 1.8 ms against 55), its lofts 2.5 times (a circle to a square: 16 ms against 41).
  Booleans of B-spline faces are slower: each crossing is traced through searches for the
  surfaces' nearest points (a smooth loft of circles cut by a slab beside its axis: 2 s).

## 8. Where it differs from OpenCASCADE

- A hole exactly tangent to a face: OpenCASCADE splits the face along the line they touch
  on and gives that line to three faces; the native kernel leaves the face whole.
- Where two surfaces meet in a curve that is not a line, a circle or an ellipse,
  OpenCASCADE fits a B-spline to it, and its areas and volumes are a few millionths off the
  exact ones (a hole across another: 8e-6 of its wall's area); the native kernel's section
  or traced curve is the exact curve, and its areas match the integral to 1e-9.
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

- A B-spline face's area and centroid: OpenCASCADE takes them by a Gauss rule of so many
  points, further off the higher the surface's degree (a smooth loft through four
  sections: 3e-6 of a face's area; through seven: 1.5e-4 of a face's centroid); the native
  kernel's match the integral to 1e-10 (adaptive Gauss–Legendre, a traced edge stretch by
  stretch between its samples). Volumes agree to 1e-9.
- A smooth loft through ten sections or more: OpenCASCADE approximates through them with
  knots; the native kernel says it cannot yet.
- STEP files read: a rational B-spline surface's area, which OpenCASCADE takes by a rule
  of too few points (a cylinder, a sphere and a torus written as rational B-splines: 0.8,
  0.5 and 0.9 percent of their volumes off), the native kernel reads as the cylinder,
  sphere or torus it is. A face bounded by a curve the writing system fitted: OpenCASCADE
  integrates along the file's curves in the surface's parameters, the native kernel along
  the curves themselves, as far apart as the file's accuracy (an EasyEDA part: 6e-5 of a
  cone's area). A sphere's axis may be turned (§4, STEP files); OpenCASCADE keeps the
  file's. A file whose top product is an assembly: OpenCASCADE's `import_step` gives no
  material, the native kernel the first part's.

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
   faces that only touch or meet along their edges; one boolean engine for every solid;
   traced curves: tori crossed off their axis (a plane beside it, a hole through a torus's
   side), curves that turn back on themselves (holes partly crossing), holes across fillets;
   B-spline curves and surfaces (evaluation, nearest points, faces on them, their integrals,
   meshes and booleans, the poles kept once and placed). To do: curves through points
   where the surfaces touch.
3. **Done.** Chamfers and fillets: equal, two distances, a distance and an angle; along
   straight edges between planes and parallel cylinders and round edges between surfaces on
   one axis; runs of edges that carry on smoothly; corners of one, two (a mitre) and three
   blends (a triangle, a sphere); valleys filled in. The demo bracket.
4. **Done.** Shell, inwards and outwards, opened or closed, by offsets that keep the
   topology (planes, cylinders, cones, spheres, tori moved along their normals); sweep along
   paths of lines and arcs that turn smoothly, glued piece by piece.
5. **Done** (but through ten sections or more). Loft, ruled and smooth, as OpenCASCADE's to
   the pole: sections matched corner to corner or cut at each other's polar angles, on
   planes at any angle; cones and cylinders between circles on one axis; joined to, cut
   from and cut by other bodies.
6. **Done** (but IGES). STEP files written as OpenCASCADE writes them and read: AP203,
   AP214, AP242, assemblies, units, colours, materials, every surface and curve of the
   corpus; faces settled to the kernel's (seams, poles, exact B-splines). The cart's STEP
   import in `kernel_coverage.rs`: 6.9 ms against OpenCASCADE's 259.
7. Booleans with many tools in one pass, measure, planes normal to a curve, hidden-line
   drawings, a volume mesher: what opening files like the hackintosh case needs (§10).
8. The native kernel as the default, once every feature test of `oxyde-tests` passes on it.

## 10. Opening files like the hackintosh case

The hackintosh case is the owner's test of October 2026: an imported STEP assembly
(*Proto Case Hackintosh mATX HEAVY v106*, 1.67 L of parts) remodelled feature by feature
through `oxyde-mcp` into an Oxyde assembly of 31 components (34 occurrences of 23 parts,
three sub-assemblies, mates). Its parts are panels and brackets: sketches of lines, arcs and
circles, extrusions and cuts, linear patterns of hexagonal holes (two staggered grids per
panel, hundreds of holes), mirrors of patterns, R4 fillets, holes, offset planes, text
outlines (TrueType glyphs flattened to lines) and a logo. Opening files like it with the
native kernel means reading the STEP assembly, rebuilding the 23 parts, solving the mates,
and measuring, drawing and exporting them, as with OpenCASCADE. The files are on the
owner's PC; what follows comes from the STEP corpus (§7) and from parts built like the
case's (a probe, October 2026, on the Raspberry Pi), each built by both kernels:

| Part like the case's | OpenCASCADE | Native kernel |
|---|---|---|
| a panel with two staggered grids of hexagonal holes, 36 holes | 0.93 s | 2.8 s |
| the same, 100 holes | 3.2 s | 49 s |
| the same, 400 holes | 28 s | over 35 min (stopped) |
| a plate's slots, patterned (15) and mirrored | 0.86 s | 1.5 s |
| an island on a plate, its corners and top rounded (R4) | 0.08 s | 0.12 s |
| 40 engraved glyph outlines (1,440 lines) cut 0.5 deep | 3.9 s | 8.0 s |

Every one of them is the same solid with either kernel (volume and faces), and the native
kernel meshes each 10 to 50 times faster. What is missing, most needed first:

1. **Booleans with many tools.** A pattern's cuts are one boolean with every instance as
   a tool, which the native kernel takes as that many booleans in turn (`curved::boolean`),
   each cutting the whole solid again, its faces holding every hole cut so far: the time
   grows with the square of the holes or faster, so a panel of the case's would take an
   hour. Wanted: one pass for all the tools, each face of the target meeting only the
   tools whose boxes meet it, its curves from all of them laid out in one arrangement, its
   pieces kept or dropped against all the tools at once (a tree of the tools' boxes), tools
   that do not meet each other never compared; and the arrangement of one face with many
   curves made faster on its own (the 40 glyphs: twice OpenCASCADE's time). The goal: the
   time linear in the tools, and the panels faster than OpenCASCADE, as the kernel's other
   operations are.
2. **The case itself, tried.** `oxyde-cli` rebuilds with OpenCASCADE only: give it
   `--kernel native`, and a command that rebuilds a part, an assembly or a STEP file with
   both kernels and reports each feature's status and time and how far the results differ
   (what `kernel_coverage.rs` does, for any file); run it on the case's 23 parts and its
   original STEP file on the owner's PC. Whatever fails there joins this list. Known risks:
   the original comes from a system the corpus does not hold (its name, `v106`, suggests
   Fusion 360); its
   top panel's solid holds an island twice, overlapping itself, which the kernel's check of
   every solid may refuse where OpenCASCADE reads it (then a solid read from a file would
   need a gentler check, or the copy dropped); its CPU block carries lettering on a B-spline
   surface (OpenCASCADE's boolean of it never finished). Its original STEP file, opened by
   `step_corpus.rs` (October 2026): OpenCASCADE reads its 466 solids in 80 s (141 s as the
   application opens it, part by part); the native kernel, past the loose arcs it refused
   at first, stops at an edge 0.0449 mm off its faces where the file's accuracy (0.01 mm)
   lets the reader take 0.04, and, with that limit at OpenCASCADE's 1 mm, at a face round
   a pole of a B-spline surface it cannot read yet. Which of the two to take on (a gentler
   limit for files, or such faces) is the owner's call.
3. **Blends the native kernel still refuses**, each with its message: an edge ending where
   more than three faces meet, a ridge and a valley meeting at a corner, more than three
   edges meeting, corners where curved edges or curved faces meet, a blend ending on a
   curved face or on a face running nearly along its edge, an arc going nearly all the way
   round; and shells of B-spline faces. Which of them the case's fillets need is for step 2
   to tell.
4. **What the application asks of a kernel besides building**: `distance` (Measure shows
   nothing with the native kernel), `edge_tangent` (a plane normal to a B-spline edge
   fails; lines and circles are worked out without the kernel), `project_edges` (drawings
   cannot be made), all answered "cannot" so far; `volume_mesh` (simulation uses the
   lattice mesher instead of Netgen). Mates use the faces' planes and axes from the
   topology, which the native kernel gives; interference checks are booleans (item 1).
5. **B-spline faces made faster.** Reading Creo's 1,150-face assemblies takes 15 to 17 s
   (OpenCASCADE: 23), 13 of them integrating 190 B-spline faces (Green's theorem along
   edges of many spans, each point's inner integral across the face); booleans of B-spline
   faces trace their curves through nearest-point searches (a loft cut by a slab: 2 s).
   Gauss–Kronrod rules reusing their points, and the inner integrals carried along each
   edge rather than taken afresh at every point, are the next steps; then booleans on the
   imported solids (a STEP import's faces built on).
6. **Text in the application's sketcher**, with its glyphs' quadratic curves kept as
   B-splines (or arcs) rather than flattened to lines: fewer, exact edges for the logo and
   the lettering. A model feature, not the kernel's, but what the kernel is given.
7. Still to do from §9: IGES, a spindle or horn torus revolved from a sketch's arc, a sweep
   along an arc it does not start square to, arcs extruded at a slant, smooth lofts through
   ten sections or more, curves through points where surfaces touch.

Steps 1 and 2 first: the first makes the case's panels buildable at all, the second turns
this list from what parts like the case's need into what the case's own files need.
