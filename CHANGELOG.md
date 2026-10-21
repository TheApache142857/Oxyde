# Changelog

What changes for people using Oxyde and its file formats. Oxyde is pre-alpha and has no
release yet; the git history has every detail.

## Unreleased

### Generative design, under way (October 2026)

- **A Generative Design tab in place of the Topology tab.** As in Fusion 360: give the part's
  bodies their roles (bodies to keep, bodies to stay clear of, a starting shape the material
  stays inside), each coloured in the viewport; hold and load the bodies to keep as in a
  static study; set the mass target, the safety factor, the ways of making it (unrestricted,
  2-axis cutting, casting in a mould pulled along a direction) and the materials to try.
  *Generate* grows a shape for every material and way of making it, on the graphics card when
  there is one, each shown as it is done; *Stop* keeps the one being grown as far as it got.
  *Explore* plots the outcomes' mass against their stress (or stiffness, displacement, safety
  factor) to compare them, draws the one picked with its iterations on a slider, and *Export*
  writes it as STL, OBJ or 3MF. The outcomes are saved beside the study and come back with
  it. Topology studies are still made in the Simulation tab, under New Study.
- **Additive manufacturing and load cases.** *Additive* grows shapes that print without
  supports along the build direction picked: nothing overhangs more than 45°, in any
  direction. *Load cases* (Design conditions › Add a load case) give the loads a part meets
  at different times, each with its own fixtures: the outcomes are as stiff as can be for
  them all. In files: `load_cases` in the study's `generative` block (FORMAT.md §8).
- **Minimize mass, a displacement limit, milling.** *Minimize mass* (Objectives) finds the
  lightest shape whose stress stays under the material's strength over the safety factor,
  and whose displacement stays under a limit when one is set; *Maximize stiffness* still
  takes a mass target. *Milling* grows shapes a 3-axis mill reaches from the side picked,
  pockets narrower than the tool kept out where the shape allows. In files:
  `objective: "min_mass"`, `displacement_limit` and the `milling` method (FORMAT.md §8).
- **Overhangs within 45° in every direction.** Additive outcomes could lean up to about
  55° from the build direction where a wall ran along a diagonal between the axes: each
  layer was allowed to rest on a diagonal neighbour. Now nothing overhangs more than 45° in
  any direction (walls leaning along a diagonal come out a little steeper than needed), so
  the outcomes print without supports as promised. Additive outcomes generated again
  differ from earlier ones.
- **How grey an outcome is.** Explore shows each outcome's *grayness*: how much of it is
  neither solid nor void. A few percent is the thin band where the shape ends; more is
  marked, as parts of the shape did not settle crisp there. An outcome that stops changing
  while still grey is no longer called settled and runs on to the end. In files: each
  outcome's `grayness` (FORMAT.md §9).
- **Gravity and remote masses.** *Gravity* weighs the shape as it grows (9.81 m/s² down, or
  so many g for a shock, along any direction), and *Remote Mass* hangs a component on the
  faces of a body kept: its mass at its centre of mass, which gravity weighs and whose
  weight reaches the faces as a force and a moment, as Fusion 360's point mass does (a
  1.35 kg instrument under 10 g, as NASA's bracket carries). Each is a load the load cases
  pick, so a case can be a shock along X and another the weight alone. Weighted outcomes
  are the stiffest for their mass target at most: a lighter one may be stiffer when the
  material weighs. In files: `gravity` in generative studies and the new `remote_mass`
  load (FORMAT.md §8).
- **Milling from several sides.** *Milling* can make one outcome milled from all the sides
  picked (*All sides in one outcome*), the part turned over between setups, as Autodesk's
  machinery example mills from +Y and −Y: every spot is reached from one side or another,
  so the shape can have hollows on both faces. Off, it is an outcome per side as before.
  In files: `combined` on the `milling` method, and a milled outcome's `directions`
  (FORMAT.md §8–§9).
- **Minimum thickness and the tool's width are aims.** The panel and the documents now say
  what they are: the density filter keeps bars and walls from coming out thinner than the
  minimum thickness, and pockets narrower than a milling tool, wherever the shape allows,
  but does not rule them out. Check the part made.
- **Generative design in assemblies.** A generative study of an assembly takes the bodies
  of its parts: keep the brackets' mounting bosses, stay clear of the motor next to them, and
  the outcome grows between them; *Create Part* inserts the part made where it was grown,
  fixed. In files: bodies named with their component, as faces are (FORMAT.md §8).
- **Create Part.** The outcome picked becomes a part file of its own: its surface made a
  solid that meets the bodies kept, the starting shape and the bodies to stay clear of on
  their faces (flat faces exactly, round ones as their tessellation does), in seconds even
  for a fine outcome, in the outcome's material, ready to be studied in the Simulation tab.
  Where it meets a flat face, that is one flat face of the new part, to select, sketch on
  or mate to, rather than hundreds of triangles; elsewhere it stays faceted, and features
  cut into it are slow at that many faces. The part being studied stays open; the panel
  offers to open the new one.
- **The part made is checked.** Once *Create Part* has made the part, the panel checks it
  as a static study would: the part filled with tetrahedra, the faces of the bodies kept
  found on it, every load case solved on it. Its mass, compliance, largest displacement,
  largest stress and lowest factor of safety then stand beside the outcome's estimates,
  which come from voxels whose corners gather stress. A bar shows the check going, and
  *Stop* ends it (the part stays). The panel warns when the check's mesh lost some of the
  part's thinnest members. Nothing of the check is saved: it is made again with the part.
- **Files:** the `.oxysim` and `.oxyres` formats hold generative design studies, as
  Fusion 360 has them, for the Generative Design tab to come: the bodies to keep and to stay
  clear of, the mass target, safety factor, minimum thickness and resolution, the ways of
  making the shapes (unrestricted, additive, 2-axis cutting, casting, each along chosen
  directions) and the materials to try (`type` `generative`, FORMAT.md §8), and their
  results: the grid of voxels and, for each material and way of making it, the shape's
  densities, a few snapshots on the way and its mass, stiffness and estimated stress
  (FORMAT.md §9). A document holding them is written as `format_version` 4, which earlier
  versions of Oxyde refuse; the others are written as before.

### Topology optimization (October 2026)

- **A Topology tab, and topology studies.** *Topology* is a new study type in the
  Simulation tab's New Study, as SolidWorks has its topology study and Fusion 360 its shape
  optimization, and it has a tab of its own next to Simulation, which shows the topology
  studies only. Hold and load the part as in a static study (Fixed Geometry, Roller/Slider,
  Force, Pressure), say how much of its mass to keep (half by default), and *Run* takes
  material away where the loads need it least until that much is left: the stiffest shape
  for that mass. The viewport shows the shape that remains as a smooth surface, the whole
  part faint around it; a *Threshold* slider makes it thinner or thicker, and *Export STL*
  writes it out, to print it or to model a part over it. The panel says how much of the
  mass is kept and what that costs in stiffness ("1.9 times as flexible as the whole
  part"). Parts of several bodies and assemblies are optimized with their contacts.
- **Material that stays.** The faces of fixtures and loads keep their material; *Preserve
  Region* keeps that of other faces (mounting faces, bolt holes), as deep under them as set
  in the panel. *Thinnest member* keeps bars and walls from coming out thinner than wanted;
  by default they can be as thin as the mesh allows, about three elements.
- **Symmetry and a pull direction**, the manufacturing controls of SolidWorks' topology
  study and Fusion 360's shape optimization, in the Target section. *Symmetry* mirrors the
  shape across the Right, Top or Front plane (any of them, or planes parallel to them at a
  position typed) whatever the loads, the kept regions too. *Pull direction* (±X, ±Y, ±Z)
  gives a shape a mould pulled off that way can release: going along it the shape never
  widens, so it has no undercut; *Both ways* is a two-part mould, the shape narrowing away
  from a parting plane on each side. A plane the part is not symmetric about is refused,
  and so is a pull one way across a symmetry plane. Pulled across the way the part bends
  (through a plate's thickness, as most castings are) the shape is as crisp as without;
  pulled in that plane (a beam loaded down and pulled up) it comes out grey, neither solid
  nor void, until a later version sharpens the densities.
- **Keep it in one piece.** Material nothing loads can come out floating: a preserved
  ring around a hole the loads do not reach, held or not, ends up on its own, as it would in
  SolidWorks or Fusion 360's shape optimization. *Keep it in one piece* (in the Target
  section, on for new studies) joins every piece holding a fixture, a load or a preserved
  region to the rest by a bar as thick as the thinnest member, where it adds least material,
  and takes away loose bits holding none of them; the panel says how much was added. Without
  it, or at another threshold, the panel says when the shape is in pieces and which
  fixtures, loads and preserved regions are on the pieces the loads do not reach. In files:
  `one_piece` in the study's `topology`, and `joining` in the results (FORMAT.md §8–§9).
- **How it works:** each element of the mesh gets a density, its stiffness going as the
  cube of it, the densities averaged over a small sphere so no member is thinner than it,
  and moved by optimality criteria, one static solve per iteration (60 at most by default,
  fewer once the shape settles). Gravity is not taken: the weight would change with the
  shape. New topology studies mesh with four-node elements (*Draft*): the shape comes from
  the elements' densities, which they give as well as ten-node ones with a seventh of the
  unknowns, so every iteration is several times quicker (on a servo arm meshed at 0.3 mm,
  5 s instead of about 100); the Mesh section says so when a topology study is set to
  ten-node elements.
- **Files:** topology studies are saved in the `.oxysim` with their target, member size,
  iterations, preserved regions, symmetry planes and pull direction, and their results in
  the `.oxyres`: each element's density and every iteration's compliance and mass (`type`
  `topology`, `fields/density.bin`; FORMAT.md §8–§9). A document holding a topology study
  is written as `format_version` 3, which earlier versions of Oxyde refuse; the others are
  written as before.

### Third bug hunt: end conditions, a fuller sketcher, Measure (0.0.8, October 2026)

A read through the modelling engine and the application for bugs, behaviours nobody asked
for and what SolidWorks and Fusion 360 users reach for every day. Fixed:

- **Edits to a sketch could be lost without a question.** With a saved part open, editing
  a sketch and then closing the window, opening another file or starting a new one did
  not ask to save: the drawing only counted as a change once the sketch was left. The
  window title now shows `*` as soon as the sketch changes, and every way out asks.
- **Saving no longer leaves the sketch.** Ctrl+S (or Save As) in a sketch saved it by
  closing it first; it now writes the sketch as drawn, with the features after it as
  they are, and stays in the sketch, as SolidWorks does. The save is one undo step.
- **A revolve across its axis is refused.** A sketch lying on both sides of the axis of
  revolution was turned into a solid passing through itself, without a word, which later
  features then failed on in odd ways. The revolve now fails with "the sketch crosses the
  axis", as SolidWorks refuses it; a contour touching the axis is still fine.
- **Redo in sketches.** Ctrl+Y and the Redo button did nothing in a sketch; they now put
  back what Undo took away there (and a change that is refused, such as a fillet with no
  room, does not lose what Redo has).
- **Deleting a feature takes it out of the configurations.** A deleted feature, or one
  whose dimensions were configured, stayed in the ConfigurationManager's table as a
  "(gone)" column.
- **Study names that would share a results file are refused.** On Windows and macOS,
  *Static 1* and *static 1* (or *Load/Case* and *Load_Case*) wrote the same `.oxyres`,
  one study's results over the other's; New Study and Rename now tell them apart.
- **Meshing with Netgen shows its progress and can be cancelled.** The progress stayed
  at 0 % until Netgen had meshed a whole body, so a fine mesh looked stuck for minutes,
  and Cancel did not reach Netgen: it went on taking the processor and memory in the
  background. The progress now follows Netgen through *Meshing the faces*, *Optimising
  the faces*, *Filling the volume* and *Optimising the volume*, and Cancel stops it.
- **Right-click selects what it is on.** A right-click on a face or an edge acted on what
  was selected before; it now selects what is under the pointer first, as in SolidWorks.
- **Direction and axis boxes are one line high again.** In Linear and Circular Pattern,
  Mirror, Revolve, Extrude's *Up To Surface*, Plane and Axis, the box holding a direction,
  an axis, a plane or a face stretched down to the bottom of the window, hiding the
  spacing, the number of instances and every option under it. Its list button showed an
  empty square instead of an arrow, and so did the button removing an appearance's
  texture.
- **A material given to imported parts from an assembly keeps its look.** Parts written
  by a STEP assembly import and never opened on their own showed the material's look
  until the assembly was opened again, when the colours of the STEP file came back over
  it (glass-clear polycarbonate panels turned opaque white). *Apply and remove
  appearances* now removes those colours from the files for good. Parts already given a
  material this way need it applied once more.
- **A material removed from imported parts in an assembly stays removed.** On such parts,
  never opened on their own, removing the material wrote the file with the colours and
  the material of the STEP file still to be applied, as when it was imported:
  the material the STEP file names came back the next time the assembly was opened (or
  after editing a component in place), with its weight in the bill of materials. The
  file now gets the part as the assembly shows it.

New, for daily work:

- **Extrude end conditions** (the *End condition* list of the Extrude command, as
  SolidWorks' Boss- and Cut-Extrude): *Blind* and *Mid Plane* as before, and
  - *Through All*: through everything the part holds on that side (*Reverse direction*
    picks the side), the usual way to cut a hole through a part whatever its thickness;
  - *Through All - Both*: through everything on both sides of the sketch;
  - *Up To Surface*: up to a flat face (its plane, past its edges) or a plane, picked in
    the graphics area, the tree or the list; it goes towards it on whichever side it is,
    and a face at an angle to the sketch is followed exactly. A surface parallel to the
    direction, or one cutting through the sketch, is refused with why.
  These have no depth dimension. In files: `extent` `through_all` and `up_to_surface`
  (FORMAT.md §3).
- **Loft** (Features tab, Insert menu, a shortcut to set): a solid through the closed
  contours of several sketches, in the order picked (Ctrl+click them in the tree before,
  or click them after; *Reverse order* turns the list round), smooth through them or
  straight from one to the next: a square turning into a circle, a tapered handle. It
  joins, cuts, intersects or makes a new body like an extrusion, previews live, absorbs
  its sketches in the tree, and can be patterned and mirrored. Each sketch must be one
  closed contour; contours drawn clockwise and counter-clockwise are turned to match, so
  the loft does not twist. In files: the `loft` feature (FORMAT.md §3).
- **Hole Wizard** (Features tab, Insert menu, a shortcut to set): holes at the points of
  a sketch, *Clearance* (close, normal or loose fit), *Tapped* (the tap drill),
  *Counterbore* (for socket head cap screws) or *Countersink* (90°, for flat heads),
  sized for ISO metric screws M2 to M24 (every value editable; *Custom* keeps what is
  typed), blind with a 118° drill point or *Through All*. Select a flat face first and
  the command opens a sketch on it with the *Point* tool: click where the holes go, leave
  the sketch, and the Hole Wizard opens with it (or select a sketch of points). Holes
  keep their face names when points are added or removed, and can be patterned and
  mirrored. In files: the `hole` feature (FORMAT.md §3).
- **Drawings** (File › Export › *Drawing (PDF, SVG, DXF)*, and `oxyde-cli drawing`): the
  front, top, right and isometric views of the part, or of the assembly's visible parts
  where they sit, on an A4 to A0 sheet in first angle (ISO) or third angle projection,
  at the largest standard scale that fits (or one chosen), with a frame and a title block
  (title, part number, material, drawn by, date, scale, projection symbol). Hidden lines
  are worked out exactly and dashed (the isometric view shows the visible edges only),
  holes seen end on get centre marks, and the window previews the sheet. PDF and SVG are
  the sheet at its size; DXF has the views at their true size, on layers, for laser,
  waterjet and CAM programs. A first step towards drawing documents: no dimensions or
  annotations yet, and nothing is saved but the file written.
- **Diameter dimensions.** Smart Dimension on a full circle now gives its diameter (Ø), on
  an arc its radius, as SolidWorks does; right-click offers the other. In files: the
  `diameter` relation.
- **Values typed as arithmetic and with units**, in the dimension window, the sketch tool
  windows and the value fields of the commands, mates, equations and configurations:
  `80/2`, `(120 - 2*6)/3`, `1in`, `3/8 in`, `2.5 cm + 1mm`, `45deg`, `pi/2 rad`; a
  decimal comma is read as a point. Lengths are converted to millimetres and angles to
  degrees.
- **New sketch tools**: *Straight Slot* (the two centres, then the width: half circles
  and tangent lines with a construction centerline), *Polygon* (centre then a corner;
  the number of sides in the tool window; its corners on a construction circle, its
  sides equal), *Point*, *3 Point Arc* (start, end, then a point on it), *Tangent Arc*
  (from the end of a line, tangent to it) and *Extend*, next to Trim (as SolidWorks'
  Extend Entities: point at a line or an arc near the end to lengthen, see the dashed
  piece it would add, and click: a line goes straight on, an arc round its circle, up to
  the next curve it meets, and its new end stays on that curve). A *Centerpoint Arc* now
  goes the way the pointer turns from its start, clockwise or counter-clockwise, instead
  of always counter-clockwise. Each has a shortcut to set in Preferences › Shortcuts.
- **Chamfer types**: besides *Distance* (equal), *Distance - Distance* and *Angle -
  Distance*, as SolidWorks offers them, with *Flip direction* to swap the faces; the
  second distance or the angle is a dimension like the first (`D2@Chamfer1`). In files:
  the chamfer's optional `second`.
- **Mirror a whole body**: the Mirror command has a *Bodies to mirror* box (pick them in
  the Solid Bodies folder, or click faces after *Pick bodies*); each body is joined to
  its mirror image, so half a symmetric part becomes whole, as SolidWorks' Bodies to
  Mirror. With one body and no feature selected, Mirror starts with it. In files: the
  mirror's optional `bodies`.
- **Convert Entities on a whole outline**: Ctrl+click a part edge to copy every edge
  joined to it (a face's outline in the sketch plane) at once, as SolidWorks converts a
  selected face.
- **More relations** in the sketch's right-click menu: *Horizontal* and *Vertical*
  between two points (to line up holes; in files `horizontal_points` and
  `vertical_points`), *Collinear* for two lines, *Fix* for a line or an arc.
- **Measure** tells the shortest distance between the two selected faces or edges, with
  its dX, dY and dZ (in an assembly too, between faces of different components where
  they sit), the *centre distance* between two holes, cylinders or circular edges, and
  the diameter of a cylindrical face; Mass Properties gives the centre of mass.
- **Edit a feature from the part.** Right-click a face or an edge: *Edit* the feature
  that made it, or its sketch. In the tree, a feature that uses a sketch offers *Edit*
  of that sketch too, and every feature *Roll back* (the bar above it) and *Roll to end*.
- Version 0.0.8, with the end conditions, Loft, the Hole Wizard, drawings, the new
  sketch tools and the fixes above.

### Several studies per model, and thermal studies (0.0.7, October 2026)

- **New Study.** The Simulation tab now opens on *New Study*, as Fusion 360's does: pick
  *Static Stress*, *Thermal*, *Transient Thermal* or *Thermal Stress*. A model can have as
  many studies as wanted, each with its own loads, contacts, mesh settings, material
  values and results; the list at the top of the panel switches between them (*New*,
  *Rename*, *Delete* beside it), and switching keeps each study's results. *Save* writes
  them all to the model's `.oxysim`, which now opens with the model when it sits next to
  it, and each study's results go to `<study file>-<study>.oxyres`.
- **Thermal studies.** Select faces, then *Temperature* (held at a temperature), *Heat
  Power* (watts in, spread by area), *Heat Flux* (W/m²), *Convection* (a film coefficient
  and the fluid's temperature), *Radiation* (an emissivity and the surroundings'
  temperature) or *Internal Heat* (watts made inside the faces' bodies, a motor's losses
  or a resistor). Faces with none are insulated, and heat crosses every contact as if
  bonded. *Run* gives the temperatures and the heat flux, plotted in colour bands with
  the hottest point marked (*Temperature* and *Heat Flux* in the command bar).
- **Transient thermal studies** follow the temperatures over time from a uniform start:
  set the duration, the time steps and the starting temperature in the panel; the
  results keep about a hundred times, shown with a slider and played with ▶, the colours
  keeping one range over the whole history.
- **Thermal stress studies** solve the steady temperatures, then the stresses of the
  expansion they cause with the fixtures (and any forces, pressures or gravity): a part
  free to grow is unstressed, a held one is stressed. Set the temperature at which the
  model is free of stress in the panel. Stress, displacement, factor of safety and
  temperature can all be plotted.
- **Materials know how they take heat**: conductivity, specific heat and thermal
  expansion, edited on the Material page (*Add properties* starts from typical values for
  the material's name) and saved with the material, in the part and in `materials.json`
  (a new optional `thermal` block, `docs/FORMAT.md` §3). Every material of the starter
  library has them; older files and STEP materials get typical values by name. The
  Simulation panel shows the values a study uses and lets a study type its own.
- Study and results files holding a thermal study are `format_version` 2; files with
  static studies only stay version 1, so earlier Oxyde builds still open them
  (`docs/FORMAT.md` §8–§9).
- Version 0.0.7, with the Simulation tab, the material rows and the memory budget below.

### Materials a click away, a maximum element size (unreleased, October 2026)

- **A memory budget for simulations** (Tools › Preferences › *Simulation*, 8 GB by
  default): a mesh with more nodes than the budget allows is refused before it is solved,
  with how many nodes it would have and how many fit, instead of the fixed limit of
  250 000 nodes. It applies to Netgen's meshes too, which had no limit. The page says
  about how many nodes fit (some 470 000 ten-node or 320 000 four-node at 8 GB: a solve
  takes about 16 KB per node with ten-node elements and 24 KB with four-node ones, which
  sit in more elements each).

- **A Material row in every tree.** The part's tree has a *Material* row above the
  features (SolidWorks' `Material <not specified>`), and in an assembly every part
  shows its Material row under it, without expanding it. Clicking the row lists the
  library's materials and *None*; double-clicking opens the Material page (the Materials
  window in an assembly); right-click has the same choices and *Select parts with this
  material*. The trees are there in every tab, Simulation and Render included.
- **The Simulation panel gives materials.** Each line of its Material list has *Choose*
  (a part without a material) or *Change*: the library's materials, for the part, or for
  every part of the assembly in that line. The results go stale and the panel asks for
  another run, as for any change to the model.
- **A maximum element size box under the mesh slider.** The Simulation panel's Mesh
  section shows the *Max element size* in use (mm) under the Coarse–Fine slider, always:
  type a value to mesh finer (or coarser) than the slider goes, and Netgen makes no
  element edge longer than it (smaller ones near small features and curved faces).
  Moving the slider, or *Automatic*, gives the size back to it. This replaces the
  *Automatic / Typed* choice; studies save it as before.

### A Simulation tab (unreleased, October 2026)

- **Simulation tab** (parts and assemblies), the first steps of `docs/SIMULATION.md`: a
  linear static study as SolidWorks Simulation runs one. Select faces, then:
  - *Fixed Geometry* holds them where they are; *Roller/Slider* lets them slide along
    themselves but not move off them.
  - *Force* pushes on them with a total in newtons, spread by area, normal to the faces or
    along X, Y or Z; *Pressure* presses into them (MPa); *Gravity* adds the model's weight
    from its materials' densities, downwards.
  - *Run* fills the model with tetrahedra and solves it on a thread of its own, with a
    progress bar and *Cancel*; *Mesh* shows the mesh alone.
  - Then *Stress* (von Mises, or the first principal stress from the panel),
    *Displacement* and *Factor of Safety* draw the result on the deformed model in
    SolidWorks' colours, with a legend, the largest value (the smallest factor of safety)
    marked, and the deformation exaggerated automatically or at a scale of your choice.
  The panel at the top of the left side lists the fixtures and loads (click one to select
  its faces, *Use selection* to move it, values edited in place), the material's elastic
  modulus, Poisson's ratio and yield strength (typical values for the library's materials
  and for steels, aluminium, plastics and other families by name; type others in), the mesh
  (high quality ten-node or draft four-node elements, a coarse-to-fine density or a typed
  element size, and how much of the model the mesh fills) and the results (largest stress
  and displacement, smallest factor of safety, a warning when the part yields). Parts that
  are not held are named before anything is solved, as SolidWorks warns of a model that
  could move freely.
- **Materials know how they behave under load**: elastic modulus, Poisson's ratio, yield
  and tensile strength, edited on the Material page (*Add properties* starts from typical
  values for the material's name) and saved with the material, in the part and in
  `materials.json` (a new optional `mechanical` block, `docs/FORMAT.md`). Every material of
  the starter library has them. The Simulation panel shows where the values come from (the
  material's own, or typical values when it has none); typing others there changes them
  for the study only, and *Apply to the material* keeps them in the part's material. A
  material with no yield strength (cast iron, glass, acrylic) gets its factor of safety
  from its tensile strength. Older files and materials named by STEP files simply have no
  block and keep working.
- **Contacts, as in Fusion 360.** Each body (each part of an assembly, each body of a
  multi-body part) is meshed on its own and contacts say how bodies interact: *bonded*
  (glued), *separation* (may part, never pass through each other), *sliding* (slide without
  friction, stay in touch), *rough* (may part, never slide) and *offset bonded* (glued across
  a gap). *Run* finds the faces of different bodies that touch (within a tolerance, 0.1 mm
  by default) and bonds them; *Find Contacts* lists them first so their type can be changed
  in the panel's Contacts list; *Manual Contact* makes one between the selected faces of two
  bodies. A *Contact pressure* plot shows what the faces carry, and the panel warns when a
  separation or rough contact did not settle.
- **Netgen meshes the model** (when Oxyde is built with it, `docs/BUILDING.md`): elements
  graded to the model's features and true to its faces and edges, instead of the lattice
  mesher's bevelled edges. The Mesh section chooses the mesher; without Netgen, or if it
  fails on a solid, the lattice mesher is used and the status bar says why.
- **Studies and results are saved**: *Save* in the panel writes the study (fixtures, loads,
  contacts, mesh settings, material overrides) as a `.oxysim` next to the model, and every
  *Run* of a saved study saves it again with its results beside it as a `.oxyres` (the mesh
  and every nodal field). *Open…* reads a study back for the model it was made for; *Open results…*
  shows saved results without solving again, provided neither the model nor the study
  changed since (each file records what it was computed from). Both are documented in
  `docs/FORMAT.md` §8–§9.
- Results go stale when the model changes: the model comes back and the panel asks for
  another run.
- Known limits of the lattice mesher (the fallback): elements are all about the same size,
  sharp edges are bevelled at that size, and walls thinner than an element are lost (the
  panel says how much of the model the mesh fills). Ten-node elements are straight-sided,
  also on Netgen's meshes: curved faces follow their facets.
- Solves take seconds: the solver's iterations are guided by multigrid, so a
  70 000-node mesh solves in about 2 s (it took about 15 s with the simpler method, which
  remains the fallback).
- Plot settings are preferences (`simulation` in `preferences.json`); everything else
  belongs to the study and its file.

### The Render tab on the GPU (0.0.6, October 2026)

- **Renders run on the GPU.** The Render tab's path tracer now runs on the graphics card
  the window draws with (Direct3D 12, Vulkan or Metal), typically ten to fifty times
  faster than on the CPU, so the picture refines almost as fast as the view settles.
  It is the same algorithm, ported to a compute shader: same materials, lights, floor,
  glass and background, same picture. The GPU adds samples a band of rows at a time,
  sized each frame to what the card manages in about 30 ms, so the window stays
  responsive and no single piece of work runs long enough for Windows to reset the
  driver. When the render is done, the final picture (denoised if asked) is made from
  the GPU's sums exactly as on the CPU; *Save Image* works mid-render too.
- **Output › Device**: *GPU when available* (the default) or *CPU*. The panel says which
  GPU renders, or why the CPU does: a window drawn without a GPU, a model with more
  triangles than the card's largest buffer holds, or any error from the GPU, after which
  the tab carries on with the CPU renderer.
- **Big models on the GPU.** Oxyde now asks the graphics card for its largest buffers
  (up to 2 GB with Direct3D 12 and Vulkan) instead of wgpu's default 128 MB, which
  stopped the GPU renderer at about two million triangles.
- **Render While Moving** (a button in the Render tab, and a box in the panel's Camera
  section): the render stays on screen while orbiting, panning and zooming, restarting
  at once, instead of the viewport showing until the view is still. Smooth on the GPU;
  on the CPU the picture follows a little behind. Off by default.
- On the GPU a new view replaces the picture once one whole sample of it is in, instead
  of appearing band by band.
- The Render panel scrolls, so a long list of surfaces no longer pushes the rest of the
  setup and the tree out of reach.
- **The macOS installer by double-click**: `packaging/Build Oxyde.dmg.command`, opened
  from the Finder on a Mac, checks what the build needs (Xcode's tools, Homebrew, Rust,
  OpenCASCADE, disk space), offers to install what is missing, builds the `.dmg` and
  shows it in the Finder. No Terminal commands to type.
- **An .msi installer for Windows** (`packaging\bundle.ps1`, when the WiX Toolset is
  installed): Oxyde for every user of the machine, into Program Files, with a Start menu
  entry and the file types, for IT departments and deployment tools. The per-user
  `.exe` installer stays; macOS keeps its `.dmg`.
- Version 0.0.6.

### A Render tab, glTF export, File › Open recent (unreleased, October 2026)

- **Render tab** (parts and assemblies), the first step of `docs/RENDERING.md`: the model
  drawn by a path tracer with real materials, light and soft shadows instead of the
  viewport's shading, as SolidWorks Visualize does. The picture refines pass after pass
  on every core; orbiting, panning or zooming shows the usual viewport and the render
  starts again once the view is still. The panel at the top of the left side holds the
  setup:
  - *Scene*: a built-in environment (a studio with a key, a fill and a rim light, a sky
    with the sun, or a uniform light), its brightness and rotation, lights that follow
    the view or stay with the model; the background (the environment, a plain colour,
    or transparent for the PNG); the floor (shadows only, visible with a colour and a
    gloss, or none).
  - *Camera*: perspective with a focal length (full-frame equivalent), or the
    viewport's orthographic view.
  - *Surfaces*: a finish per material (matte, satin or glossy plastic, brushed or
    polished metal, glass, rubber). *Automatic* guesses it from the material's category
    and name (steel and aluminium are brushed metal, plastics satin, wood matte; a
    see-through appearance is glass, which refracts). Parts without a material get a
    choice of their own.
  - *Output*: samples, bounces, exposure, denoising of the first passes, the size (the
    viewport's at 25–200 %, or a fixed size; at most 8 megapixels, so a render stays
    under about 1 GB of memory) and how finely the model is meshed for it.
  - **Save Image** writes the picture as it is as a PNG (with transparency when the
    background is transparent).
  The setup and the finishes are kept in the preferences, not in the document: where
  they belong in the files is still open (`docs/RENDERING.md` §5). Nothing in the
  document changes.
- **glTF export** (File › Export › glTF, the Render tab's *glTF for Blender* button, and
  the command line's `export` with a `.glb` file): a binary glTF 2.0 file with one named
  object per part or body and a physically based material per appearance and finish
  (glass with transmission and an index of refraction), in metres, which Blender,
  three.js and Windows' 3D Viewer open. Textures are not written yet.
- **File › Open recent** (parts and assemblies): the last ten documents opened or saved,
  newest first, as on the start screen; files that are gone are greyed out, and *Clear
  list* empties it.

### Sweep, configurations, interference detection, an Export menu (unreleased, October 2026)

Features and tools SolidWorks users reach for next, after a look at what solidcraft offers:

- **Sweep.** Features › Sweep carries the closed contours of one sketch (the profile) along
  the curves of another (the path): a round profile along a bent path gives a pipe, a
  square along a closed path a frame. The path is one chain of lines and arcs, open or
  closed (a full circle gives a ring); a path in two pieces, a branching one or the
  profile sketch itself are refused with a message. The profile keeps its angle to the
  path through bends. Sweeps join, cut, intersect or make a new body like extrusions, are
  named `Sweep1` and `Cut-Sweep1`, show both sketches under them in the tree, preview
  live, and can be the seed of a pattern or a mirror. Side faces are named after the
  profile's segments as an extrusion's are, so fillets and later features on a sweep keep
  their references. The PropertyManager takes the profile first and the path next
  (buttons switch which box the next click fills); the Insert menu, the Features tab and
  a keyboard shortcut (Preferences › Shortcuts › Sweep) start it. In files: a new feature
  `type` `sweep` (FORMAT.md §3).
- **Configurations.** The left panel has two tabs now, as SolidWorks' FeatureManager and
  ConfigurationManager: *Features* (the tree) and *Configurations*. A configuration is a
  named variant of the part in the same file: its own values for chosen dimensions and
  its own suppressed features, everything else shared. Add the first one and the part as
  it is becomes *Default*; add dimensions and features to the table with the two menus;
  click a name to activate a configuration (the part rebuilds with its values), edit the
  active row to edit the part, edit another row to set what that configuration will apply.
  What is changed while a configuration is active stays with it. Right-click a name to
  rename or remove it, a column heading to stop configuring it. Every change is one undo
  step. Saved in `model.json` under `configurations` (FORMAT.md §3); files without any open
  as before.
- **Interference Detection** (assemblies, Evaluate tab). Every pair of visible parts whose
  boxes overlap is checked exactly, on every core; the pairs that share a volume are
  listed with it (largest first), the shared volumes are drawn in red over the model,
  and clicking a pair selects its two components. Parts that only touch along a face are
  not interferences. The window says when the assembly changed since the last
  calculation.
- **File › Export** is a menu now, in parts and assemblies: **STEP** (as before; an
  assembly's visible parts placed, with their colours and materials, in one file),
  **IGES** 5.3 (new), **STL**, **OBJ** (new: one named object per part or body, with
  normals, for Blender and renderers) and **3MF** (new: named, coloured objects and the
  unit, for modern slicers) through the mesh export window, which previews the mesh as it
  will be written and reports its triangles and whether it is watertight, and **Image
  (PNG)** (new): the view at any size, from a standard view or as on screen, drawn by the
  software renderer with the viewport's colours. The command line's `export` writes the
  new formats too, by extension (`.igs`, `.obj`, `.3mf`).
- **Edit in place at any depth.** A part inside a sub-assembly (inside another, at any
  depth) can be edited in place straight from the top assembly: right-click it in the tree
  or on a face in the graphics area, or double-click its row. The rest of the whole
  assembly is ghosted around it, in the part's own coordinates, and File › Finish editing
  comes back with the files read again. Opening a sub-assembly in place and a part from
  there still works.
- **A plan for a Render tab** is in `docs/RENDERING.md`: a renderer inside Oxyde (a CPU
  path tracer as the reference, a wgpu preview), a link to Blender (glTF export and an
  add-on with reload), and the order to build them in, with the questions the owner
  decides. Nothing of it is built yet.

### Booleans on every core (unreleased, October 2026)

- **Booleans use every core.** A fuse, cut or intersection (an extrusion joining or
  cutting a body, a pattern of holes, Combine, the Section View's cut) now runs on every
  core of the machine for the parts of its work that can be shared out; a section cut
  through a big imported solid takes up to a third less time. The result is the same
  shape with the same faces in the same order and the same names as before: a new test
  rebuilds parts made of several booleans ten times and checks every face, edge, name and
  history against the first rebuild.

### Section View, faster (unreleased, October 2026)

Evaluate › Section View does less work for the same picture:

- **Only the solids the plane goes through are cut.** A solid wholly on the kept side is
  shown as it is, from the mesh the display already has; one wholly on the removed side
  is left out. In an assembly, where the plane goes through a few parts of many, this is
  most of the work saved.
- **An assembly's parts are cut on every core at once**, as they are rebuilt when the
  assembly opens, the slowest parts first so that a big part listed late in the tree does
  not run alone at the end.
- **The window no longer measures the model every frame.** The box around the model,
  which the slider's range comes from, is asked of the kernel once per change of the
  document; on a big imported part it took up to 0.2 s a frame while the window was open.
- The cut solids are meshed at the display's precision, so a part looks the same cut and
  whole.

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

### Never crash (unreleased, October 2026)

A bug in Oxyde no longer takes the unsaved part with it. A panic in a kernel call or
in the building of a feature fails that feature alone, with `internal error: …` in the
tree, and the rest of the timeline is still built; a panic in a rebuild or anywhere in a
frame of the window ends that frame, not the application: the change that led to it is
undone and a window says so, with the message to report. Every production crate now
refuses `unwrap`, `expect`, `panic!` and friends at compile time (21 such places were
replaced by errors or harmless fallbacks). See `docs/ARCHITECTURE.md`, "Never crash".

### Command line: `oxyde-cli` (unreleased, October 2026)

- **A headless command line**, `oxyde-cli`, for scripts, agents and continuous
  integration, which could not open a part without the window until now. `eval
  part.oxyprt [--json]` rebuilds the part through OpenCASCADE as the application does
  and prints every feature's status (ok, warning, error with its message, suppressed)
  and, per body, volume, surface area, centre of mass, bounds and face, edge and vertex
  counts (exit code 1 when a feature failed); `check part.oxyprt --expect expected.json
  [--tol 1e-3]` compares those measures with a file of expected values and prints a
  pass/fail table, the seed of a reference test suite; `export part.oxyprt --out
  part.step|part.stl` writes the body as STEP (with colours and material) or binary STL
  at the application's Normal quality; `snapshot part.oxyprt --out part.png [--view
  iso|front|top|right]` draws it with the software renderer, 1024 x 768 by default.
- The kernel gained a **centre of mass** query (`Kernel::centre_of_mass`), which the
  command line reports and checks.
- `cargo test --workspace` now also runs the command line on a filleted box whose
  volume, area and centre of mass are known by hand, and on the STEP cart.

### Previews (unreleased, October 2026)

Commands show what they will do before OK, as in every CAD system:

- **Every feature command previews its result.** While Extrude, Revolve, Fillet,
  Chamfer, Shell, Linear or Circular Pattern, Mirror or Combine is open, the viewport
  shows the part as the feature would make it, updated as the options change: the faces
  the feature makes are tinted amber, or red where it takes material away (cuts,
  intersections, patterns of cuts). A new reference plane or axis is drawn where it
  would go, and an edited one moves as its values change. Dragging a value updates the
  preview live; a preview that takes long to build waits until the value is let go.
- **Errors show before OK.** When the feature cannot be built (a fillet too large, a
  sketch with no closed contour…), the PropertyManager says why, in red, under the
  options; warnings show in amber. OK still adds the feature, as before.
- **Editing a feature shows the part as it is at that feature**, as SolidWorks rolls
  back while a feature is edited; the features after it come back on OK. Geometry made
  by later features can therefore no longer be clicked by mistake.
- **Picking works on the preview.** Faces and edges are clicked on the previewed part as
  on the part itself. What the feature makes cannot be picked for it, and the status bar
  says so; clicking a fillet or chamfer face of the preview takes its edge out of the
  selection.
- **Mates preview by default**: the parts go where the mates put them as soon as both
  sides of a pair are picked, and move again as the type, distance, angle or alignment
  changes. The *Preview* checkbox of the Mate command turns this off (every mate then
  picked where the parts sit now, as before) and is remembered.
- **Sketch**: Mirror Entities draws the mirrored copies dashed while the pointer is on a
  line to mirror about; the dimension value window draws, dashed, where the value being
  typed would move the geometry, and says when the other relations cannot meet it.
- **Preferences › Graphics › Previews** turns feature previews and mate previews on and
  off; the *Preview* switch at the bottom of each feature command does the same. The
  performance overlay shows how long the last preview took.

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
