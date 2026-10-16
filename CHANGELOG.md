# Changelog

What changes for people using Oxyde and its file formats. Oxyde is pre-alpha and has no
release yet; the git history has every detail.

## Unreleased

### Section View, STEP materials, material lines in the tree, release builds (0.0.4, October 2026)

- **Section View** (Evaluate tab, in a part and in an assembly): the model cut open by a
  plane to look inside it, as SolidWorks' Section View. The cut is made for real by the
  kernel (every solid minus a half-space), so the section is exact: pick the Front, Top or
  Right plane, move it along its normal with a slider (the range is where the model is),
  *Flip* to keep the other half, *Centre* to put it through the middle, *Look at* to face
  it. The cut faces are coloured (colour of your choice, or off) and the window gives the
  section area and how many solids the plane goes through. It is a way of looking, not a
  feature: nothing is changed, nothing can be picked while it is shown, and closing the
  window shows the whole model again. On a big assembly the cut is made when the slider is
  let go of.
- **Materials from STEP files.** Fusion 360 and SolidWorks write the material of every body
  into their STEP exports (its name and density); Oxyde now reads them. An imported part
  gets that material (density converted to kg/m³, so masses are right at once), marked as
  coming from a STEP file: *(STEP)* in the tree, *· STEP* in the Materials window, *Source:
  Imported STEP file* in its editor, so Oxyde's own library materials and the ones a STEP
  file named are always told apart. In the Materials window, *Add the N STEP materials to
  the library* keeps them in `materials.json` (under *Imported from STEP*, still marked)
  for other documents. A material whose file gave no density weighs nothing until one is
  set; Mass Properties says so. Export STEP writes the part's material name and density
  back. File format: a material's optional `"source": "step"` (FORMAT.md §3).
- **Material line in the assembly tree.** Expanding a part shows what it is made of under
  its planes and mates (`Material: Acier (STEP)`, or *Material: none*), with the density
  and where it comes from on hover; right-click changes it (the part file is rewritten),
  opens it in the Materials window (also a double-click) or selects every part made of it.
  Parts inside sub-assemblies get the same line.
- **Free orbit fixed**: dragging down now lifts the viewer over the model, as the
  constrained orbit does; it turned the other way. Turning the constrained orbit back on
  from Preferences › Navigation levels the view too (only View › Constrained orbit did).
- **macOS builds.** New `packaging/bundle-macos.sh` makes the macOS (Apple Silicon)
  bundle: `Oxyde.app` with the OpenCASCADE dylibs relocated into it, the icon, the
  document types, ad hoc signing (`OXYDE_SIGN_IDENTITY` for a Developer ID), in a `.dmg`
  and a zip. Building on a Mac: `brew install opencascade`; releasing both platforms:
  docs/BUILDING.md.

### Materials window, free orbit, Move Component, work off the UI thread (0.0.3, October 2026)

- **Materials window** (Tools › Materials…, the *Materials* button of an assembly's
  command bar, *Edit library…* on the Material page): the library edited in one place,
  with the materials the open part or assembly uses listed first (in italics when they
  are not in the library). Editing a material and saving it changes every open part made
  of it: the open part, or in an assembly every part file whose material has that name,
  rewritten. *Apply to the part* / *Apply to the selected components* gives the
  selection the material being edited.
- **A material replaces a part's own appearances.** Applying a material to a part that
  has appearances of its own (on faces, bodies or the whole part) asks first, since the
  material's look takes their place; cancelling leaves everything as it was. Editing a
  material a part is already made of never touches its appearances.
- **Sub-assemblies in the tree are no longer greyed out.** Their parts and nested
  sub-assemblies can be given a material or an appearance from the top assembly's tree
  (right-click; an appearance goes into the sub-assembly's file), and hovering one lights
  it up in the viewport. Their own mates are listed under them. Editing in place is for
  the rest: mates, placement, a part's timeline.
- **Appearances set inside a sub-assembly now show in the assembly that uses it**: an
  occurrence takes the outermost component appearance on the way to it (the top
  assembly's wins), then the part's own.
- **Mates listed under their components**: expand a component to see the mates it takes
  part in, with the same menu as the Mates list.
- **Move Component** (command bar, right-click a component): while it is on, a plain
  left-drag moves the component under the pointer and Shift+drag turns it, as
  SolidWorks' Move Component, to place parts before mating them. Esc or the button ends
  it. Alt+drag still works at any time.
- **Free orbit** (View › *Constrained orbit (Y up)*, also in Preferences › Navigation):
  off, the view turns about the screen's own axes and can be tilted any way, as Fusion
  360's free orbit; on (the default), the model's Y axis stays up the screen, as in
  SolidWorks. Turning it back on levels the view.
- **Measure gives the radius and diameter of arcs** (`R 5.000 (Ø 10.000)`), not only
  their length; full circles show the diameter as before.
- **Ambient occlusion** replaces subsurface scattering (View menu, Preferences ›
  Graphics with a strength slider; on by default): creases, inner corners and the foot
  of walls are darkened, so the shape reads better. Screen-space, from the depth buffer,
  in both renderers. The old `subsurface_scattering` preference is read as it.
- **Long work no longer freezes the window.** Opening a part or an assembly, reading a
  STEP file (New part from STEP, Import STEP, Import STEP assembly), Reload Parts and
  coming back from editing in place run off the UI thread, behind a progress window that
  says what is being read, how far it got and how long it has taken.
- **Coming back from editing in place reads only what changed**: the files whose
  modification time changed since they were loaded (the edited one, usually), not every
  part of the assembly.
- **Faster ghosted assemblies.** The see-through faces (the ghosted rest of the
  assembly while editing in place, transparent appearances) are sorted again only when
  the view direction changes, not every frame, and the highlight buffers only when the
  highlight changes.
- **Editing an appearance while editing in place keeps the rest of the assembly
  ghosted** (it used to turn opaque while the panel was open).
- Version 0.0.3.

### Editing in place, STL export, multi-selection (October 2026)

- **Edit a component in place.** Right-click a component › *Edit part in place* (or
  *Edit sub-assembly in place*; double-click does the same) opens its file for editing
  with the rest of the assembly shown ghosted around it, in the component's own
  coordinates. The assembly is set aside meanwhile, with its undo history and selection;
  *File › Finish editing* (also the button under the tree's heading) comes back to it
  and reads the edited files again. Editing nests: a part inside a sub-assembly inside
  the main assembly. New, Open or Exit while editing finish each level first, asking
  about unsaved changes level by level. The old *Back to assembly*, which needed the
  assembly saved first and reloaded everything, is gone.
- **Edit locks.** While a part or assembly is open for editing, a small file sits next to
  it (`Bracket.oxyprt.lock`) saying who edits it from which window. A second window
  opening the same file (directly, or as a component to edit in place) is told, and can
  open it read-only (Save then asks for another name) or take the file over. A lock left
  behind by a crashed window on the same computer is replaced without asking. Saving
  over a file another window edits is refused. See [FORMAT.md](docs/FORMAT.md) §7.
- **Export STL** (File menu, parts and assemblies) for 3D printing. A window chooses the
  quality (presets scaled to the model's size, or a custom maximum deviation and angle),
  the unit (millimetres as slicers expect, or cm, m, inches) and binary or ASCII STL, and
  shows the mesh in the viewport as it will be written, triangles and all, before the
  file is saved. It reports the triangle and vertex counts, the size, the file size and
  whether the mesh is watertight. An assembly is exported as one mesh of every visible
  part where it sits. The mesh is made afresh with both a linear and an angular
  deflection (`Kernel::tessellate_for_export`), so small fillets and holes keep their
  shape at any size.
- **Shift+click selects ranges**, in the timeline and in the assembly tree (components
  and mates); Ctrl+click adds or removes one, as before, and Shift+click in the viewport
  adds like Ctrl+click. Right-click › Fix / Float, Hide / Show and Delete act on every
  selected component (the Fix button of the command bar too); in the timeline Suppress,
  Hide / Show and Delete act on every selected feature, and Delete (the key or the Edit
  menu) deletes the whole selection as one undo step, later features first so a feature
  and its sketch go together.
- **Subsurface scattering** (View menu, and Preferences › Graphics with a strength
  slider): soft, translucent-looking shading, as of wax, resin or printed plastic, in
  both renderers. (Replaced by ambient occlusion in 0.0.3.)

### STEP colours and materials (October 2026)

- **STEP colours come in and go out.** Import STEP (and New part from STEP, and the
  assembly import) reads the colour and transparency that STEP files carry per solid and
  per face, as SolidWorks, Fusion and FreeCAD write them, and turns them into appearances
  of the Imported feature: the body's colour, and face appearances where faces differ.
  They are ordinary appearances afterwards (change or remove them as usual; they are not
  re-applied). Export STEP writes the part's appearances back as STEP colours, so a
  coloured part survives a round trip through another CAD system. Textures stay in
  Oxyde: STEP has none.
- **Materials.** A part can be given a material: a look plus a density, as in SolidWorks'
  and Fusion's material libraries. Right-click › Material… (or the Material page of the
  Appearance panel) lists the library by category; a pick shows at once, OK is one undo
  step. The material's look is what the part is drawn with underneath any face, body or
  part appearance, its density gives **Mass** in Mass Properties (parts and whole
  assemblies; parts without a material are counted apart), and its name fills the
  `material` property the bill of materials shows. The page edits name, category,
  density and look, and saves to the library (or deletes from it).
- **Material library**: a starter set (steels, aluminium alloys, brass, bronze, copper,
  titanium, ABS, PLA, nylon, polycarbonate, acrylic, rubber, glass, woods, FR-4, carbon
  fibre) until the user saves a material, then `materials.json` next to the preferences,
  a plain JSON file ([FORMAT.md](docs/FORMAT.md) §3) that can be edited or shared.
- **Materials in assemblies**: right-click › Material on a component applies a library
  material to every selected component at once, down into sub-assemblies; the part files
  are updated, since parts are documents of their own. *Select parts with this material*
  selects every component made of the same material.
- **File format**: optional `material` in `model.json` (a copy of the library entry, so
  files never depend on a library) and `apply_colors` on an `import` feature whose
  colours have not been applied yet. Files without them read as before.

### Appearances (October 2026)

- **Appearances.** Faces, bodies, the whole part, or a component of an assembly can be
  given a colour, a transparency and a colour texture (PNG or JPEG), as in SolidWorks:
  Features › Appearance, right-click › Appearance… in the graphics area, on a body in the
  Solid Bodies folder or on a component. The most specific appearance wins (face over body
  over part; a component's over the part's). The panel has colour and material presets
  (steel, brass, glass…), a transparency slider, and an image picker with the size one
  repeat of the image covers; textures are mapped along the model's axes, so nothing has
  to be unwrapped. Every change shows at once; OK is one undo step. See-through faces
  show what is behind them and can still be picked.
- **Floor reflections** (View › Floor reflections, on by default; strength in
  Preferences › Graphics): the model mirrored in a floor just under it, fading with
  height.
- **File format**: optional `appearances` in `model.json` and `appearance` on a
  component in `assembly.json`; texture images under `textures/` in the archive
  ([FORMAT.md](docs/FORMAT.md)). Files without them read as before; older versions ignore
  them.

### Mates and navigation (October 2026)

- **Flipping a mate's alignment works** when an axis mate holds the same part: it used to
  be reported as "cannot be met together with the other mates", because the half turn
  the flip needs flips the axis too, and the solver had fixed which way round the axis
  mate was taken. The part is now turned over first (about the axis it is concentric
  with, when that lies across the mated face, so the axis mate keeps holding) and the
  solve goes on from there.
- **A coincident mate between two faces of solids starts anti-aligned** when their
  normals are square, as a screw's head beside a hole: the faces touch instead of the
  part turning upside down. Reference planes and axes keep the nearest alignment.
- **Mouse buttons** (Preferences › Navigation): by default the right button now turns the
  view, the middle button (or Shift+right) pans, and the left button is for selecting
  only. The SolidWorks mapping (middle turns) and the previous one (left turns) are still
  there.
- **Selection box**: drag with the left button. Left to right selects what lies entirely
  inside the box, right to left (dashed) what it touches, as in SolidWorks; Ctrl adds to
  the selection. In an assembly a box selects components; with a command such as Fillet
  open, the faces and edges go to the command.

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
