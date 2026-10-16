# Changelog

What changes for people using Oxyde and its file formats. Oxyde is pre-alpha and has no
release yet; the git history has every detail.

## Unreleased

### Agent guide, continuous integration, a layering test (unreleased, October 2026)

Nothing changes in the application; this is for the people and agents working on it.

- **`CLAUDE.md`** at the repository root: what to read first, the constraints of the build
  machine, the checks to run before every commit, the architecture rules in short, the
  conventions of the code, and the decisions that are the owner's to make.
- **Continuous integration** (`.github/workflows/ci.yml`): on every push and pull request,
  the pure-Rust crates are format-checked, linted and tested on Linux in minutes, and the
  whole workspace on Windows with OpenCASCADE from vcpkg, kept in the Actions cache after
  the first run. Written without a run: the repository has no Actions minutes yet.
- **The crate layering is a test** (`crates/oxyde-tests/tests/layering.rs`): the "must
  never depend on" column of ARCHITECTURE.md §2 is checked against every crate's
  `Cargo.toml`, and a wrong dependency fails the test naming the edge.
- ARCHITECTURE.md's roadmap now says what shipped: multi-body parts and assemblies are
  done (0.0.5), and so is CI.

### Second bug hunt: unwanted behaviours (unreleased, October 2026)

Read through the application and the modelling engine for what they do that nobody
asked for, and fixed:

- **An over-defined sketch can be opened again.** A sketch whose relations conflict
  (two widths typed, an equation driving a dimension to an impossible value) failed
  with an error and vanished from the viewport, and *Edit sketch* refused it with
  "its plane is missing", so the conflict could not be taken away. The sketch now keeps
  its geometry as stored: it is drawn, and opens in sketch mode with the conflicting
  relations in red. Features built on it still fail until it is repaired.
- **A cut that removes nothing says so.** A Cut-Extrude or Cut-Revolve that misses every
  body (sketched on a standard plane, it goes against the plane's normal, away from a
  boss built along it) rebuilt silently and changed nothing. It now gets a warning in
  the tree, "the cut removed nothing: reverse its direction, or check that the sketch
  lies on the body", named per body when only some were missed; Combine › Cut likewise.
  An **Intersect** with nothing in common made the body vanish without a word; it is now
  an error and the body stays. Same for Combine › Intersect.
- **The arc's last click joins what it snapped to.** The Arc tool showed the snap
  glyph on an existing point (the end of a line, to close a slot) and then put a new,
  unrelated point there, so the contour stayed open. The arc now ends on that point,
  as a line does, when the relations allow it (its two ends must stay as far from the
  centre as each other), and a snap to a middle, a crossing or a curve is kept as the
  matching relation.
- **Angle dimensions read without a sign.** An angle measured clockwise showed as
  "-45.0°" on the sketch, in the value window and in the Dimensions tab of the
  Equations window; typing 45 then swung the line to the other side. They show 45°; a
  value typed keeps the angle on the side it is, and a negative value puts it on the
  other side. (An equation driving an angle still sets the signed value.)
- **Arcs can be dragged** in a sketch, as lines are: the whole arc moves with the
  pointer. Dragging one did nothing.
- **Concentric** is offered in the sketch's right-click menu for two circles or arcs
  (their centres made one), next to Equal.
- **Flip alignment** is no longer offered on concentric and perpendicular mates, where
  it changed nothing.
- **A rolled-back reference plane cannot be picked for a new sketch** from the tree
  while the Sketch command asks for a plane: the sketch went in before the plane it
  was drawn on, failed, and the file was refused at Save ("would not open again").
- **Leaving a new sketch with nothing drawn removes it**, whether by Exit Sketch,
  Escape or Discard (only Discard did), and leaves neither an undo step nor an unsaved
  mark behind: the document is as it was before the Sketch command.
- **Opening another document forgets the one before**: what was selected in the Solid
  Bodies folder, which features were expanded and which planes, sketches and axes were
  hidden by hand stayed, by feature id, and applied to the new document's features of
  the same id (a plane hidden in one part hid an unrelated sketch in the next).
- **The Appearance panel's preview no longer leaks.** Opening another command or
  window (double-clicking a feature, Rename, File › New…) over an Appearance panel
  being previewed dropped the panel and left its preview on the part: a colour that was
  never confirmed, with no undo step and no unsaved mark. The panel is cancelled first.
- **Cancelling the lock question keeps the assembly.** Opening a part file that
  another window edits, from an assembly, left the assembly before asking what to do;
  Cancel then showed the part last open instead. The question comes first.
- **Material and appearance changes respect other windows' edit locks.** Right-click ›
  Material in an assembly rewrites the part files, and an appearance on a component
  inside a sub-assembly rewrites that sub-assembly's file: a file being edited in
  another Oxyde window was written over under it. Those files are skipped, with the
  holder named, as Save refuses them.

### Sketch Fillet, Offset Entities, a move handle (unreleased, October 2026)

- **XYZ move handle.** While Move Component is on, the selected component (or the one
  being dragged) shows three arrows along the assembly's X, Y and Z axes, as Creo's drag
  handle or Cura's; dragging an arrow slides the part along that axis alone, with the
  mates pulling it back as with a plain drag. A fixed component shows none.

- **Sketch Fillet** (Sketch tab): rounds the corner where two lines meet. Set the radius in
  the tool's window, then click the corner point, or the two lines one after the other; the
  arc to come is previewed under the cursor. The lines are cut back and an arc tangent to
  both is put between them, with its radius as a dimension (the next fillets of the same
  radius are made *equal* to the first instead, as SolidWorks does). A corner that relations
  or a length dimension refer to is kept as a construction point on both lines (a virtual
  sharp), and the length goes on measuring to it.
- **Offset Entities** (Sketch tab): copies of a contour at a distance, as a wall thickness.
  Click a curve to select the curves joined to it end to end (Ctrl+click adds or removes a
  chain), set the distance, and click on the side the copy should go, following the dashed
  preview. Each line is kept parallel to its original, each arc and circle concentric,
  lines tangent to an arc stay so, and one distance dimension moves the whole offset: the
  other lines follow it through a new *equal distance* relation (file format:
  `equal_distance`, FORMAT.md). A closed contour comes out fully defined when its original
  is; the ends of an open chain are free, to be dimensioned as needed. An arc smaller than
  the distance, or three selected curves meeting at a point, are refused with a message.

### Section along a face, parts inside sub-assemblies kept after editing in place (0.0.5, October 2026)

- **Section View along a selected face.** Select a flat face, then Evaluate › Section
  View: the cut is made along that face, in a part or in an assembly (the face where its
  part sits), with the side the face looks to removed, as SolidWorks does with a face as
  the section plane. Offset 0 is the face itself; the slider moves the plane into the
  model, *Flip* keeps the other half. The face stays on offer as a fourth *Face* choice
  next to Front, Top and Right while the window is open. With nothing selected the window
  opens as before, and the *Face* choice says what to select.
- **A part inside a sub-assembly no longer vanishes after editing in place.** Coming back
  from a component edited in place reads again only the files that changed; a changed
  part file used through a sub-assembly (its material changed from the tree or the
  Materials window, or edited and saved) was dropped and never read again, so it
  disappeared from the viewport with a warning in the tree until *Reload Parts*. Kept
  sub-assemblies now have their files checked too.

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
