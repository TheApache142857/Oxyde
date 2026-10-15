# Changelog

What changes for people using Oxyde and its file formats. Oxyde is pre-alpha and has no
release yet; the git history has every detail.

## Unreleased

### October 2026 bug hunt

Checked on Linux against OpenCASCADE 7.8.1: the end-to-end tests, 31 STEP files from
SolidWorks and other systems, 20,000 random sketches solved by the new and the previous
sketch solver, and the application itself. Not yet built on Windows with OpenCASCADE 8:
see [the last part](#not-yet-checked-on-windows-with-opencascade-8).

#### Behaviour changes

- **Crossing contours are refused.** A sketch whose contours cross or touch anywhere but
  at the end points they share no longer extrudes or revolves, as in SolidWorks. It used to
  give a solid of no volume (a figure eight) or overlapping solids in one body (a hole
  across the outline).
- **A shell with no face removed is hollow**: walls around a closed void, instead of the
  void itself. A shell thicker than the part fails instead of turning it inside out.
- **Angle mates.** Between a line (an edge or an axis) and a plane, the angle is measured
  from the plane, so 0° agrees with Parallel; it used to be measured from the normal, which
  made 0° mean Perpendicular. An angle involving a line is at most 90° (a line has no way
  round: 150° reads as 30°), and the nearer of the two poses is taken. 0° and 180° between
  planes hold the normals exactly in line.
- **Distance mates** between planes keep the side the parts start on, and planes that
  start level part along the first plane's normal. A distance of 0 between points or lines
  holds them together.
- **Save As** gives the copy a new document id, for parts and assemblies, as in SolidWorks.
  With the same id, an assembly using both took the copy for the original.
- **Saving reads the file back** before it replaces the previous one: a document that would
  not open again is refused with a message, and the old file is left untouched.
- **Assembly files** whose placements are not a rotation and a translation (scaled,
  mirrored) are refused when read.
- **Sketches record their origin point** (optional `origin` in `model.json`, see
  [FORMAT.md](docs/FORMAT.md)). It used to be taken as the first fixed construction point,
  which a point fixed by Convert Entities can also be, and then moved onto the origin. A
  sketch without the record gets it when it is next edited: the fixed construction point
  where the model origin falls, never another.

#### Fixed

Geometry and kernel

- Flat faces of mirrored bodies, and one flat face of revolved rings, had inward normals:
  sketches, offset planes, measurements and mates on them were on the wrong side.
- The start cap of a revolve of less than a full turn was named as a side face.
- A sketch on a face whose normal is a model axis could turn by 90° from one rebuild to the
  next, as rounding noise chose its X axis.
- A STEP solid written inside out among others stayed inside out: only the total volume
  was checked.
- Shells, fillets and chamfers that OpenCASCADE built invalid reported success. A fillet on
  an edge whose faces meet smoothly now says so, instead of "no suitable edges".

Parts and files

- A pattern or mirror of a part's first boss made separate, overlapping bodies.
- Editing a feature could make it refer to geometry of later features, and the saved file
  then refused to open.
- Typing "inf" or "nan" in a value field saved a file that would not open.
- Expressions nested thousands deep overflowed the stack when read.
- Mass Properties measured a cylinder 0 mm across: sizes now come from the exact bounding
  box.
- The status bar kept "No body" after a part was opened.

Sketches

- Whether a relation counted as redundant depended on rounding noise: derivatives are now
  exact.
- Perpendicular between lines that start parallel, and Parallel between lines that start
  square, did not solve, or stretched a line kilometres long.
- Dragging moved fixed points, lagged behind the cursor, and a quick drag missed the point
  it started on.
- A circle standing across the sketch plane was offered to snap to and to Convert Entities.

Assemblies

- Mates between parts as they are inserted (side by side, turned alike) failed outright:
  a distance between their level front planes ("off by 10.000"), an angle or a
  perpendicular between parallel planes, a distance between points that start together.
- Angle mates near 0° or 180° stopped about 0.002° short.
- A component held only by one that can still move showed as fully defined.
- A spherical face mated by an axis instead of by its centre.
- A conflict between mates made the solver redo every other component as well.

#### Faster

| On a Raspberry Pi | Before | After |
|---|---|---|
| Solving a sketch of 80 separate rectangles | 5.8 s | 2.6 ms |
| Solving a sketch contour of 160 lines | 1.7 s | 19 ms |
| Rebuilding a 10 × 10 pattern of holes | 13 s | 0.8 s |
| Solving 40 pins mated to a base (once per frame while dragging) | 288 ms | 5.6 ms |

#### Not yet checked on Windows with OpenCASCADE 8

These calls in `crates/oxyde-occt/cpp/shim.cpp` are new and were compiled and tested only
against 7.8:

- `BRepCheck_Analyzer`: validity of shells, fillets and chamfers;
- `BRepOffsetAPI_MakeOffsetShape::PerformByJoin`, then a cut: closed shells;
- `BRepAlgoAPI_*::SetArguments` / `SetTools`: one boolean for many tools (patterns);
- `BRepBndLib::AddOptimal`: exact part size;
- `gp_Ax3::Direct`: face normals of mirrored geometry;
- `failure_text`: reads OpenCASCADE's exception messages, with one branch for 7.x and one
  for 8.x.

### Other fixes

- A line drawn between the midpoints of two opposite sides of a rectangle no longer gets
  a horizontal or vertical relation that only over-defined the sketch.
- Dimension labels are readable on a dark viewport.
