# Oxyde file formats

Status: **version 1, implemented** in `crates/oxyde-format`. The format is open: anyone may read and write it, with or without Oxyde.

| Extension | Document | `manifest.kind` |
|---|---|---|
| `.oxyprt` | Part (files saved before assemblies existed use `.oxy`; same content, still opened) | `"part"` |
| `.oxyasm` | Assembly (§6) | `"assembly"` |
| `.oxydrw` | Drawing (reserved, not written yet) | `"drawing"` |

Readers identify a document by its manifest, not its extension.

## 1. Container

Every document is a **ZIP archive** (deflate or stored). A part:

```
part.oxyprt
├── manifest.json        required  — format identity and version (first entry)
├── model.json           required  — the feature timeline: the source of truth
├── properties.json      required  — part metadata, BOM-ready
├── imports/<id>.step    one per import feature — the imported file, byte for byte
└── textures/<n>.png     one per texture image used by an appearance (PNG or JPEG), as read
```

Rules:

- `manifest.json` is written first so readers can identify a file cheaply.
- All JSON is UTF-8, written with stable key order and indentation so files diff cleanly.
- Geometry is **not stored**: Oxyde rebuilds the part from `model.json` on load. The one exception is imported geometry: an `import` feature's STEP file is kept as it was read, under `imports/`, so the part does not depend on the original file. A later version may add an optional `geometry/` cache (BREP/STEP) for tools that cannot rebuild; readers must ignore entries they do not know.
- Texture images are kept as they were read, under `textures/`, each image once however many appearances use it (stored, not deflated: they are compressed already).
- Oxyde saves by writing `<name>.<extension>.tmp` and renaming it over the target, so a failed save never destroys the previous file.

## 2. `manifest.json`

```json
{
  "format": "oxyde",
  "format_version": 1,
  "kind": "part",
  "document_id": "6f1c1f6e-3c9b-4f62-9a47-2d1e8f0c5b11",
  "units": { "length": "mm", "angle": "deg" },
  "created_by": "oxyde 0.0.3"
}
```

- `kind`: `"part"`, `"assembly"` or `"drawing"`; `"simulation"` and `"simulation_results"` for §8 and §9.
- `format_version`: 1 for parts, assemblies and drawings. Simulation studies and results are 1 when they hold only static studies and 2 when they hold a thermal one (§8, §9), so readers of version 1 still open static studies. Readers refuse a version higher than the newest they know for that `kind`.
- `document_id`: a UUID that never changes for the life of the document; assemblies will find parts by it even after a rename.
- `units` are display units. Values in `model.json` are always millimetres.

## 3. `model.json`

```json
{
  "next_feature_id": 6,
  "rollback": null,
  "features": [
    {
      "id": 1, "name": "Sketch1", "suppressed": false, "type": "sketch",
      "plane": { "standard": "front" },
      "entities": [
        { "id": 1, "type": "point", "at": [0, 0] },
        { "id": 2, "type": "point", "at": [40, 0] },
        { "id": 5, "construction": false, "type": "line", "start": 1, "end": 2 },
        { "id": 9, "type": "point", "at": [20, 10] },
        { "id": 10, "type": "circle", "center": 9, "radius": 3 },
        { "id": 11, "type": "arc", "center": 9, "start": 12, "end": 13 }
      ],
      "relations": [
        { "id": 1, "type": "fixed", "point": 1 },
        { "id": 2, "type": "horizontal", "line": 5 },
        { "id": 3, "name": "Width", "type": "length", "line": 5, "value": 40 },
        { "id": 4, "name": "D2", "type": "radius", "curve": 10, "value": 3 }
      ],
      "next_ids": { "entity": 14, "relation": 5, "dimension": 3 }
    },
    {
      "id": 2, "name": "Boss-Extrude1", "type": "extrude",
      "sketch": 1, "operation": "boss",
      "extent": { "type": "blind", "depth": 10 },
      "reverse": false,
      "dimension_names": { "depth": "Thickness" }
    },
    {
      "id": 3, "name": "Sketch2", "type": "sketch",
      "plane": { "face": { "key": { "type": "end_cap", "feature": 2 }, "part": 0 } },
      "entities": [
        { "id": 1, "type": "point", "at": [20, 10] },
        { "id": 2, "type": "circle", "center": 1, "radius": 3 }
      ]
    },
    {
      "id": 5, "name": "Fillet1", "type": "fillet", "radius": 2,
      "edges": [
        { "faces": [
            { "key": { "type": "profile", "feature": 2, "segment": 2 } },
            { "key": { "type": "end_cap", "feature": 2 } }
          ],
          "part": 0 }
      ]
    }
  ],
  "global_variables": [
    { "id": 1, "name": "Thick", "expression": "12" }
  ],
  "equations": [
    { "dimension": "Thickness@Boss-Extrude1", "expression": "\"Thick\"" },
    { "dimension": "D2@Sketch1", "expression": "\"Thickness@Boss-Extrude1\" / 4" }
  ],
  "next_global_id": 2
}
```

### Timeline

- `features` is the timeline, in order. `id`s are unique, never reused, and features refer to each other by id only. `name` is what people and equations see (`Sketch1`, `Boss-Extrude1`): unique within the part, see *Names* below.
- `rollback`: number of active features, or `null` for all.
- `next_feature_id`: id the next new feature will get (readers raise it if it is not above every id).
- `suppressed` defaults to `false`, `construction` and `reverse` too, and `part` defaults to `0`.

### Feature types

| `type` | Fields |
|---|---|
| `sketch` | `plane` (PlaneRef); `entities` and `relations`, see below |
| `extrude` | `sketch` (feature id), `operation` (`boss` \| `cut` \| `intersect` \| `new_body`), `extent` (see below), `reverse`, `bodies` (optional, see Bodies below) |
| `revolve` | `sketch`, `operation`, `axis` (`{"sketch_line": entity id}` \| `{"axis": AxisRef}`), `angle` (radians, up to 2π), `mid_plane`, `reverse`, `bodies` (optional) |
| `sweep` | `profile` (feature id of the sketch swept), `path` (feature id of the sketch followed: one chain of curves, open or closed), `operation`, `bodies` (optional) |
| `loft` | `profiles` (feature ids of the sketches lofted through, in order, each one closed contour without holes), `operation`, `ruled` (optional, default false: smooth through them all; true: straight from one to the next), `bodies` (optional). Its side faces are named after the first profile's segments (`profile`), its ends `start_cap` and `end_cap` |
| `fillet` | `edges` (list of EdgeRef), `radius` |
| `chamfer` | `edges` (list of EdgeRef), `distance`, `second` (optional, for an unequal chamfer): `{"type": "distance", "distance", "flip"}` (a distance along each face) \| `{"type": "angle", "angle", "flip"}` (radians, from the face `distance` runs along). `distance` runs along the face named first in each edge's sorted name, the other one with `flip` (optional, default false) |
| `shell` | `faces` (list of FaceRef: the faces removed, may be empty), `thickness`, `outward` |
| `plane` | Reference plane: `base` (PlaneRef), `offset` (mm, along the base normal), `flip` |
| `axis` | Reference axis: `axis`: `{"along": AxisRef}` \| `{"two_planes": [PlaneRef, PlaneRef]}` |
| `linear_pattern` | `seeds` (feature ids), `direction` and optional `direction2`: `{"axis": AxisRef, "spacing", "count", "reverse"}` |
| `circular_pattern` | `seeds`, `axis` (AxisRef), `angle` (radians: total with `equal_spacing`, else between instances), `count`, `equal_spacing`, `reverse` |
| `mirror` | `seeds`, `plane` (PlaneRef), `bodies` (optional, default empty): bodies mirrored whole, each joined to its image; a mirror has seeds or bodies, not both. A body's copy has its faces named as the mirror's instance `[1, 0]` of the body's own |
| `import` | `file_name` (the original file's name, for display), `file`: the archive entry holding the STEP file (`imports/<id>.step`). Its solids are joined to the newest body (or make the first). `apply_colors` (optional, default false): the colours the STEP file gives its solids and faces are still to become appearances (see *Appearances*); Oxyde writes it only for a part it has not rebuilt yet, such as the parts of an imported assembly. |
| `combine` | `target` (body), `tools` (bodies), `operation` (`join` \| `cut` \| `intersect`), `keep_tools` (optional, default false) |
| `hole` | Holes at the lone points of a sketch (its points that are no curve's end or centre, not its origin, not construction): `sketch` (feature id), `kind` (`{"type": "simple"}` \| `{"type": "counterbore", "diameter", "depth"}` \| `{"type": "countersink", "diameter", "angle"}`, the counterbore's or countersink's diameter in mm, its depth in mm, the countersink's included angle in radians), `diameter` (the hole's, mm), `depth` (mm of full diameter from the sketch plane, with a 118° drill point under it; `null` goes through everything), `reverse` (drill along the sketch normal instead of against it), `bodies` (optional, as a `cut`'s). Each hole is the hole's half section turned about its axis; its faces are named as pattern copies, the instance being the point's entity id: `{"type": "instance", "feature": hole, "instance": [point, 0], "face": {"type": "profile", "feature": hole, "segment": n}}` with `n` 1 the top (above the face, never left), 2 the counterbore's wall or the countersink's cone, 3 the counterbore's floor, 4 the hole's wall, 5 its bottom or drill point |

An extrusion's `extent` is its end condition:

| `extent` | Meaning |
|---|---|
| `{"type": "blind", "depth"}` | `depth` mm from the sketch plane, along its normal for a `boss` or a `new_body` and against it for a `cut` or an `intersect`; `reverse` flips the side |
| `{"type": "mid_plane", "depth"}` | `depth` mm in all, half on each side of the sketch plane |
| `{"type": "through_all", "both"}` | Through everything the part holds on the side a blind extrusion would take (`reverse` flips it), or on both sides with `both` (optional, default false). It reaches as far as the bodies' bounding boxes go along the sketch normal; a `cut` or an `intersect` goes 1% of their size further. No depth, so no `depth` dimension |
| `{"type": "up_to_surface", "plane": PlaneRef}` | Up to a plane (a flat face's plane extends past its edges), towards it on whichever side of the sketch it is (`reverse` is ignored). The plane must not be parallel to the sketch normal nor pass through the sketch's contours. The face where it ends is named its `end_cap`. No `depth` dimension |

Counts include the seed. Seeds of patterns and mirrors are extrusions, revolves, sweeps, lofts and holes (their tool bodies are copied).

**PlaneRef**: `{"standard": "front" | "top" | "right"}`, `{"face": FaceRef}` (a planar face) or `{"reference": feature id}` (a reference plane).
**AxisRef**: `{"standard": "x" | "y" | "z"}`, `{"reference": feature id}` (a reference axis), `{"edge": EdgeRef}` (a straight edge's line, or a circular edge's axis) or `{"face": FaceRef}` (a cylindrical or conical face's axis, or a planar face's normal).

### Bodies

A part may hold several separate solid bodies (as in Fusion 360 or Creo). A body is named by the feature that made it and which of that feature's bodies it is: `{"feature": 7, "index": 0}` (`index` optional, default 0; a pattern of a `new_body` feature makes one body per copy, numbered in order).

- `operation` of an extrusion, a revolve or a sweep: `boss` joins the swept solid to a body (the first `boss` of a part makes its first body), `cut` removes it from bodies, `intersect` keeps only what it shares with a body, `new_body` keeps it as a body of its own.
- `bodies` lists the bodies a `boss`, `cut` or `intersect` works on. Absent or empty means automatic: a `boss` or an `intersect` goes into the body the sketch's face is on (else the newest body), a `cut` goes through every body. Several bodies in a `boss` are joined into the first.
- Fillets, chamfers and shells work on the bodies their edges or faces are on. Pattern and mirror copies do what their seed did, to the same bodies.
- A `combine` changes its `target` body by its `tools`, which disappear unless `keep_tools`.
- A body cut away entirely disappears. Face and edge names stay unique across all bodies.

Every feature may also carry `dimension_names` (optional, default empty): custom names of its own dimensions, keyed by parameter: `depth` (a blind or mid-plane extrude), `angle` (revolve, circular pattern), `radius` (fillet), `distance`, `distance2` (two-distance chamfer) and `angle` (angle-distance chamfer), `thickness` (shell), `offset` (reference plane), `spacing`, `count`, `spacing2`, `count2` (patterns), `diameter`, `counter_diameter`, `counter_depth`, `angle` (countersink) and `depth` (blind) for holes. Unlisted parameters have default names numbered in that order within the feature: `D1`, `D2`…

### Sketches

`entities`, each with a unique `id` and optional `construction` (solved, but not part of the profile):

| `type` | Fields |
|---|---|
| `point` | `at`: `[x, y]` |
| `line` | `start`, `end`: point ids |
| `circle` | `center`: point id, `radius` |
| `arc` | `center`, `start`, `end`: point ids; counter-clockwise from `start` to `end` |

The **sketch origin** is a construction point held by a `fixed` relation, named by the sketch's optional `origin` (an entity id); Oxyde places it where the model origin falls in the sketch plane, and other geometry may share it or refer to it like any point. A sketch has at most one, and `origin` is written once it is known. A sketch without it gets it when it is next edited: the fixed construction point where the model origin falls, else a new one; other fixed construction points (Convert Entities makes them) are never taken for it.

Connected geometry shares point entities. The ids of lines, circles and arcs are the `segment` numbers in face references, so they must stay stable.

`relations` (optional, default empty), each with a unique `id` (never reused within the sketch) and, for dimensions (the relations with a `value`), a `name` unique within the sketch; lengths in mm, angles in **radians**:

| `type` | Fields |
|---|---|
| `fixed` | `point` |
| `coincident` | `a`, `b` (points) |
| `horizontal`, `vertical` | `line` |
| `horizontal_points`, `vertical_points` | `a`, `b` (points): level with each other, or one above the other |
| `parallel`, `perpendicular`, `equal_length` | `a`, `b` (lines) |
| `equal_radius` | `a`, `b` (circles or arcs) |
| `distance`, `horizontal_distance`, `vertical_distance` | `a`, `b` (points), `value` |
| `length` | `line`, `value` |
| `radius`, `diameter` | `curve`, `value` |
| `angle` | `a`, `b` (lines), `value`: counter-clockwise from `a` to `b` |
| `point_on_line`, `midpoint` | `point`, `line` |
| `point_on_circle` | `point`, `curve` |
| `tangent` | `line`, `curve` |
| `point_line_distance` | `point`, `line`, `value`: distance to the line's infinite extension |
| `symmetric` | `a`, `b` (points), `line`: mirror images about the line's infinite extension |
| `line_distance` | `a`, `b` (lines), `value`: from the middle of `a` to the infinite extension of `b` (for parallel lines) |
| `equal_distance` | `a`, `b`, `c`, `d` (lines): `a` is as far from `b` as `c` from `d`, each measured as `line_distance` (the lines of an offset) |

Positions are stored as last solved; readers solve the relations again on rebuild.

`next_ids` (optional): `entity` and `relation` are the ids the next new entity and relation will get, `dimension` the number of the next default dimension name (`D3`). They keep ids from being reused after a deletion, which would silently re-attach face references and equations to other geometry. Readers raise them if they are not above every id in use. A dimension without a `name` gets the next default name on load.

Sketch coordinates are in the sketch plane's frame. For standard planes: Front = XY, Top = XZ (sketch Y towards −Z), Right = YZ (sketch X towards −Z). On a face, the origin is the model origin projected onto the face and the X axis is the model axis least aligned with the face normal, projected (on a tie within 1e-9, the first of X, Y, Z).

### References: persistent names

References to geometry are never indices; they are the persistent names described in `ARCHITECTURE.md` §6, written as JSON:

- **FaceRef** `{ "key": FaceKey, "part": n }`, where FaceKey is one of:
  - `{"type": "profile", "feature": id, "segment": sketch_entity_id}`: side face swept by an extrusion, a revolve or a sweep
  - `{"type": "start_cap", "feature": id}`, `{"type": "end_cap", "feature": id}`: ends of an extrusion, of a revolve of less than a full turn or of a sweep along an open path
  - `{"type": "from_edge", "feature": id, "edge": EdgeRef}`: fillet/chamfer face
  - `{"type": "from_vertex", "feature": id, "vertex": VertexRef}`: corner blend
  - `{"type": "offset", "feature": id, "face": FaceRef}`: inner wall of a shell, offset from that face
  - `{"type": "instance", "feature": id, "instance": [i, j], "face": FaceRef}`: copy made by a pattern or a mirror of the seed's face `face`; `[i, j]` is the position in the pattern (`[1, 0]` for a mirror)
  - `{"type": "imported", "feature": id, "index": n}`: face `n` of the shape read from an import feature's file (stable, since the stored file does not change)
  - `{"type": "unknown", "feature": id, "index": n}`: origin not reported (unstable)
- **EdgeRef** `{ "faces": [FaceRef, …], "part": n }`: the faces the edge separates.
- **VertexRef** `{ "faces": [FaceRef, …], "part": n }`: the faces meeting at the vertex.

`part` is 0 unless several entities share the same name; readers sort `faces` lists themselves.

### Equations

Optional, default empty, SolidWorks style:

- `global_variables`: `id` (unique, never reused), `name` (unique, see *Names*) and `expression`.
- `equations`: `dimension`, the full name of the dimension the equation drives (each dimension at most once), and `expression`.
- `next_global_id`: id the next new global variable will get (default 1; readers raise it above every id).

Expressions are text: numbers, names between double quotes (`"Width"` for a global variable, `"D1@Sketch1"` for a dimension), `+ - * / ^`, parentheses, `pi`, and the functions `sin cos tan asin acos atan` (in degrees), `sqrt abs exp log` (natural) `log10 int round sgn` (one argument) and `min max` (two). Values are in mm, angles in **degrees** (unlike the rest of the file, as in SolidWorks), instance counts are rounded. Names are written as they are at save time. A name that matches nothing on load is kept and reported as an error when the equations are evaluated (a broken equation, not a broken file); a syntax error refuses the file.

### Configurations

`configurations` (optional; absent when the part has none) holds the part's named variants, as SolidWorks' configurations: which one is active, and for each one the values of the *configured* dimensions and whether the *configured* features are suppressed. Everything else in the timeline is shared. The features themselves are saved as the active configuration left them, so a reader that ignores this key opens the part as the active configuration.

```json
"configurations": {
  "active": "Thick",
  "list": [
    { "name": "Default", "dimensions": { "D1@Boss-Extrude1": 10 }, "suppressed": { "5": false } },
    { "name": "Thick", "dimensions": { "D1@Boss-Extrude1": 25 }, "suppressed": { "5": true } }
  ]
}
```

- `dimensions` maps full dimension names (as equations write them) to values in internal units: millimetres, radians, or a count. A name that matches no dimension is dropped on reading.
- `suppressed` maps feature ids (as strings, JSON keys being strings) to `true` (suppressed in that configuration) or `false`. An unknown id is dropped.
- Names are unique, not empty, at most 64 characters; `active` names one of them (else the first is active).

### Appearances

How the part looks, optional (absent = the viewport's part colour everywhere); display only, never part of the rebuild:

```json
"appearances": {
  "part": { "color": [200, 160, 90], "texture": { "file": "textures/1.png", "file_name": "oak.png", "scale": 80 } },
  "bodies": [
    { "body": { "feature": 2, "index": 0 }, "color": [120, 125, 130], "transparency": 0.4 }
  ],
  "faces": [
    { "face": { "key": { "type": "end_cap", "feature": 2 }, "part": 0 }, "color": [10, 20, 30] }
  ]
}
```

- `part` is the appearance of the whole part; `bodies` and `faces` override it for single bodies (by the feature that made them, see *Bodies*) and single faces (by persistent name, see *References*). The most specific wins: face over body over part.
- An appearance has a `color` (sRGB, 0–255), a `transparency` from 0 (opaque, the default when absent) to 1 (invisible), and optionally a `texture`: an image in the archive (`file`), its original name (`file_name`, display only) and `scale`, the millimetres one repeat of the image covers along its width. The image is mapped automatically along the part's axes (projected along X, Y and Z and blended by the surface's orientation), so no texture coordinates are stored; it replaces the colour where it is.
- Appearances of bodies and faces that no longer exist are kept and ignored; a `texture.file` missing from the archive is an error.
- **STEP colours.** When an `import` feature is first rebuilt, the colours its STEP file carries (AP214/AP242 colour and transparency per solid and per face, as SolidWorks, Fusion and FreeCAD write them) become appearances: a body appearance for a single solid that makes a body of its own, and face appearances (named `imported` faces) where faces differ from it or when the solids join an existing body. They are ordinary appearances from then on (the feature's `apply_colors` is cleared), so the user can change or remove them. STEP has no textures. Export STEP writes the appearances back the same way: each solid's colour (its body's, else the part's, else the material's) and the faces that differ.
- **STEP materials.** The same first rebuild gives the part the material its STEP file names, when the part has none yet: the AP242 `material name` property and its `density` measure, which Fusion 360 and SolidWorks write for every body (`Acier`, 7850 kg/m³). It becomes an ordinary *Material* (below) with `"source": "step"`, the default look (the file's colours win over it anyway) and the file's density in kg/m³ (0 when the file gives none; such a part has no mass until a density is set). Importing a STEP assembly does the same for every part file it writes. Export STEP writes the part's material name and density with each solid.

### Material

What the part is made of, optional (absent = no material):

```json
"material": {
  "name": "Aluminium 6061-T6",
  "category": "Aluminium",
  "density": 2700,
  "appearance": { "color": [200, 203, 208] },
  "mechanical": {
    "elastic_modulus": 68900,
    "poisson_ratio": 0.33,
    "yield_strength": 275,
    "tensile_strength": 310
  },
  "thermal": {
    "conductivity": 167,
    "specific_heat": 896,
    "expansion": 2.36e-5
  }
}
```

- A part has at most one material. It is a **copy** of a library entry, so the file never depends on a library: `name` (unique in a library; `properties.json` gets it as `material`, for the bill of materials), `category` (for grouping; optional), `density` in kg/m³ (0 when unknown; mass = volume × density) and an `appearance` (same shape as in *Appearances*; its texture image goes under `textures/` like the others).
- `mechanical` (optional): how the material behaves under load, for the Simulation tab (isotropic, linear elastic). `elastic_modulus` (Young's modulus) and `poisson_ratio` are required in it; `yield_strength` and `tensile_strength` (ultimate) are optional, 0 or absent when unknown. Moduli and strengths are in **MPa** (N/mm²). A factor of safety is measured against the yield strength, or the tensile strength when there is no yield strength (brittle materials: grey cast iron, glass, acrylic). Readers treat a block a solver could not use (a modulus that is not positive, a Poisson's ratio outside −1 to 0.5, a negative strength) as absent; Oxyde then uses typical values for the material's name, as it does for materials without the block (files written before it, and materials named by STEP files).
- `thermal` (optional): how the material conducts, stores and expands with heat, for the Simulation tab's thermal studies (isotropic, constant with temperature). `conductivity` in **W/(m·K)** is required in it; `specific_heat` in **J/(kg·K)** (with the density, the heat a transient study stores) and `expansion`, the coefficient of linear thermal expansion in **1/K** (what a thermal stress study expands by; it may be negative), are optional, 0 or absent when unknown. Readers treat a block a solver could not use (a conductivity that is not positive, a negative specific heat, an expansion of a hundredth per kelvin or more) as absent; Oxyde then uses typical values for the material's name.
- `source` (optional): where the material comes from. Absent or `"library"`: Oxyde's material library (the starter set, or one the user made). `"step"`: named by the STEP file the part was imported from, with the density that file gave, so that a library's own materials and the ones a STEP file brought are always told apart (the material keeps `"source": "step"` when it is added to the library; readers refuse other values). Imported STEP materials are filed under the category `Imported from STEP` until renamed.
- The material's appearance is what the part is drawn with where nothing more specific applies: face over body over part appearance over material. Oxyde clears a part's own appearances when it applies a material to it (after asking), so a part normally has either; both can still be stored.

**Material library.** The user's library is a plain JSON file outside any document, `materials.json` next to the preferences (`%APPDATA%\Oxyde\materials.json` on Windows, `~/.config/oxyde/materials.json` elsewhere), with texture images in a `textures/` folder beside it:

```json
{
  "format": "oxyde-materials",
  "format_version": 1,
  "materials": [
    { "name": "Oak", "category": "Wood", "density": 750,
      "appearance": { "color": [170, 120, 70], "texture": { "file": "textures/1.png", "file_name": "oak.png", "scale": 80 } } }
  ]
}
```

Entries have the same shape as a part's `material` (with `mechanical` and `thermal` when known), `texture.file` relative to the library file's folder. Readers refuse another `format` or a higher `format_version`; a material whose image is missing is kept without it. Oxyde writes the file the first time a material is saved to the library; until then it uses its built-in starter set (steels, aluminium alloys, brass, copper, titanium, ABS, PLA, nylon, polycarbonate, acrylic, rubber, glass, woods, FR-4, carbon fibre), every one with its mechanical and thermal properties.

### Names

Feature and dimension names follow SolidWorks: a dimension's full name is `dimension@feature`, e.g. `D1@Sketch1` or `Thickness@Boss-Extrude1`, which is how equations refer to it. A name is valid when it is not empty, has no leading or trailing spaces, and contains no `@`, `"` or control character. Feature names are unique within the part, dimension names within their feature. Names are display only: every internal reference uses ids, so renaming never breaks anything.

## 4. `properties.json`

Readable by BOM tools without understanding geometry:

```json
{
  "part_number": "OXY-0001",
  "description": "Mounting bracket",
  "revision": "A",
  "material": "Aluminium 6061-T6",
  "custom": { "finish": "anodized" },
  "volume_mm3": 7859.3
}
```

All fields are optional when reading. `volume_mm3` is computed at save time and informational only. `material` is free text; when the part has a material in `model.json`, Oxyde keeps this equal to its name.

## 5. Reading rules

- Refuse files whose `format` is not `"oxyde"`, whose `kind` is not supported, or whose `format_version` is higher than supported, with a clear message.
- Older versions are upgraded through explicit, tested migrations (v1 → v2 → …), never by guessing.
- Validate the timeline on open: unique feature ids, every feature refers only to features before it, sketch entities and relations refer to existing entities of the right kind, relation ids are unique, and names are valid and unique as described above. An unknown feature or relation `type` is an error in version 1.
- Additive changes (new optional fields, new entries) do not bump the version; anything else does.

## 6. Assemblies (`.oxyasm`)

```
robot.oxyasm
├── manifest.json        required  — "kind": "assembly"
├── assembly.json        required  — components and mates
└── properties.json      optional  — the assembly's own metadata (same fields as a part)
```

An assembly stores no geometry and no copy of its parts: it refers to part files, which Oxyde opens and rebuilds.

```json
{
  "next_component_id": 3,
  "next_mate_id": 2,
  "components": [
    { "id": 1, "name": "Base<1>", "path": "parts/base.oxyprt",
      "document_id": "6f1c1f6e-3c9b-4f62-9a47-2d1e8f0c5b11",
      "rotation": [[1, 0, 0], [0, 1, 0], [0, 0, 1]], "translation": [0, 0, 0],
      "fixed": true, "hidden": false },
    { "id": 2, "name": "Pin<1>", "path": "pin.oxyprt", "document_id": "…",
      "rotation": [[1, 0, 0], [0, 1, 0], [0, 0, 1]], "translation": [20, 0, 10],
      "appearance": { "color": [200, 40, 40], "transparency": 0.25 } }
  ],
  "mates": [
    { "id": 1, "name": "Concentric1", "type": "concentric", "aligned": false,
      "a": { "component": 1, "face": { "key": { "type": "profile", "feature": 4, "segment": 2 } } },
      "b": { "component": 2, "axis": "z" } },
    { "id": 2, "name": "Distance1", "type": "distance", "value": 5, "aligned": false,
      "a": { "component": 1, "plane": "top" },
      "b": { "component": 2, "face": { "key": { "type": "end_cap", "feature": 2 } } } }
  ]
}
```

- **Components** are instances of part files or of other assemblies (sub-assemblies; the file's manifest says which). `path` is relative to the assembly's folder, with `/` separators. When the file is not there, Oxyde looks in the assembly's folder for a part with the same `document_id`, so a moved or renamed part is still found. `name` is unique in the assembly (`Stem<n>`, as in SolidWorks). A component's placement maps part coordinates to assembly coordinates: `p' = rotation · p + translation` (rotation rows, an orthonormal matrix with determinant +1; lengths in mm). `fixed` components never move when mates are solved; `hidden` is display only. An optional `appearance` (same shape as a part's, see §3 *Appearances*; its texture images go under `textures/` in the assembly file) is drawn instead of the part's own appearances for that component, as in SolidWorks. Through sub-assemblies, the outermost component appearance on the way to a part wins: the top assembly's component, else the sub-assembly's, else the part's own.
- **Mates**: `type` is `coincident`, `concentric`, `parallel`, `perpendicular`, `distance` (`value` in mm) or `angle` (`value` in radians, 0 to π; an angle with a line is read as at most π/2, and between a line and a plane it is measured from the plane). `a` and `b` name a component, an optional `path` and one of: `face` or `edge` (persistent names, as in a part's `model.json`, resolved in that part's rebuilt body), `plane` (`front`, `top`, `right`: the part's standard planes), `axis` (`x`, `y`, `z`) or `origin` (`true`). `aligned`: normals or axes point the same way (otherwise they face each other). `suppressed` mates are kept but not solved. When the component is a sub-assembly, `path` lists component ids inside it, level by level, down to the part holding the face or edge (`"path": [3, 1]`: component 3 of the sub-assembly, then component 1 of that one); without `path`, `plane`, `axis` and `origin` are the sub-assembly's own.
- Placements are the solved result, saved so that a reader without a solver sees the assembly as it was. Oxyde re-solves on open, starting from them.
- Readers check that component and mate ids are unique and below the `next_…` counters, that component names are unique, and that mates refer to existing components. Component `0` in a mate is the assembly itself: its own `plane`, `axis` or `origin`, fixed at the identity (components are numbered from 1).

- **Sub-assemblies** are rigid in their parent: their components keep the placements saved in their own file, and only the parent's own components move when the parent's mates are solved. An assembly must not contain itself, directly or through other assemblies; readers refuse such a component.

## 7. Edit locks (`.lock`)

While Oxyde has a part or assembly open for editing, a small file sits next to it with `.lock` appended to the document's name (`bracket.oxyprt.lock`, `robot.oxyasm.lock`):

```json
{ "user": "hugo", "host": "WORKSTATION", "pid": 18324, "since": 1760530200 }
```

`since` is in seconds since 1970 (UTC). The lock is advisory: it tells another Oxyde window (and other tools that care) that the file is being edited, so that window opens the file read-only or takes it over; nothing stops a program from writing the file. Oxyde removes the lock when the document is closed or the window quits. A lock whose `host` is this computer and whose `pid` no longer runs is one left behind by a crash: Oxyde replaces it without asking. Other tools reading a folder of Oxyde documents should ignore `.lock` files.

## 8. Simulation studies (`.oxysim`)

```
bracket.oxysim
├── manifest.json        required  — "kind": "simulation"
└── simulation.json      required  — the model studied and its studies
```

A study document holds what the Simulation tab needs to solve a model again: no geometry, no mesh and no results (those go in a `.oxyres`, §9). It refers to one part or assembly file and to its faces by persistent name, so it survives edits of the model as mates do. It holds every study of the model, of six types: static stress, thermal (steady state), transient thermal, thermal stress, topology and generative design. Oxyde saves it next to the model as `<model>.oxysim` by default and opens it with the model.

```json
{
  "model": { "kind": "assembly", "path": "robot.oxyasm",
             "document_id": "6f1c1f6e-3c9b-4f62-9a47-2d1e8f0c5b11" },
  "studies": [
    {
      "name": "Static 1",
      "type": "static",
      "fixtures": [
        { "name": "Fixed-1", "type": "fixed",
          "faces": [ { "component": 1, "face": { "key": { "type": "start_cap", "feature": 2 } } } ] },
        { "name": "Roller/Slider-1", "type": "roller", "suppressed": true,
          "faces": [ { "component": 3, "path": [2, 1], "face": { "key": { "type": "profile", "feature": 2, "segment": 4 } } } ] }
      ],
      "loads": [
        { "name": "Force-1", "type": "force", "force": -250, "direction": [0, 1, 0],
          "faces": [ { "component": 1, "face": { "key": { "type": "end_cap", "feature": 5 } } } ] },
        { "name": "Force-2", "type": "force", "force": 10.5, "direction": "normal", "faces": [ … ] },
        { "name": "Pressure-1", "type": "pressure", "pressure": 0.2, "faces": [ … ] },
        { "name": "Gravity-1", "type": "gravity", "acceleration": [0, -9.81, 0] }
      ],
      "contacts": [
        { "name": "Contact-1", "type": "bonded", "a": [ … ], "b": [ … ],
          "automatic": true, "tolerance": 0.05 },
        { "name": "Contact-2", "type": "offset_bonded", "a": [ … ], "b": [ … ], "offset": 0.4 }
      ],
      "mesh": { "mesher": "netgen", "quality": "high", "density": 0.5 },
      "materials": {
        "Aluminium 6061-T6": { "elastic_modulus": 68900, "poisson_ratio": 0.33,
                               "yield_strength": 275, "tensile_strength": 310 },
        "part:11111111-1111-1111-1111-111111111111": { "elastic_modulus": 2000, "poisson_ratio": 0.35 }
      }
    },
    {
      "name": "Transient 1",
      "type": "transient_thermal",
      "loads": [
        { "name": "Temperature-1", "type": "temperature", "temperature": 80, "faces": [ … ] },
        { "name": "Heat Power-1", "type": "heat_power", "power": 12.5, "faces": [ … ] },
        { "name": "Heat Flux-1", "type": "heat_flux", "flux": -1500, "faces": [ … ] },
        { "name": "Convection-1", "type": "convection", "coefficient": 25, "ambient": 20, "faces": [ … ] },
        { "name": "Radiation-1", "type": "radiation", "emissivity": 0.9, "ambient": 15, "faces": [ … ] },
        { "name": "Internal Heat-1", "type": "internal_heat", "power": 5, "faces": [ … ] }
      ],
      "transient": { "duration": 120, "steps": 40, "initial_temperature": 25 },
      "thermal_materials": {
        "Copper (C110)": { "conductivity": 391, "specific_heat": 385, "expansion": 1.7e-5 }
      }
    },
    {
      "name": "Thermal Stress 1",
      "type": "thermal_stress",
      "fixtures": [ … ],
      "loads": [ { "name": "Temperature-1", "type": "temperature", "temperature": 150, "faces": [ … ] } ],
      "reference_temperature": 22
    },
    {
      "name": "Topology 1",
      "type": "topology",
      "fixtures": [ … ],
      "loads": [ { "name": "Force-1", "type": "force", "force": 500, "direction": [0, -1, 0], "faces": [ … ] } ],
      "topology": { "target_mass": 0.4, "member_size": 5, "iterations": 60,
                    "preserved": [ { "name": "Preserve-1", "depth": 2, "faces": [ … ] } ],
                    "symmetry": { "planes": ["right", "front"], "point": [0, 0, 0] },
                    "pull": { "direction": [0, 1, 0], "parting": [0, 10, 0] },
                    "one_piece": true }
    },
    {
      "name": "Generative 1",
      "type": "generative",
      "fixtures": [ … ],
      "loads": [ { "name": "Force-1", "type": "force", "force": 800, "direction": [0, -1, 0], "faces": [ … ] } ],
      "generative": {
        "preserve": [ { "feature": 3 }, { "feature": 9, "index": 1 } ],
        "obstacles": [ { "body": { "feature": 12 }, "offset": 1.5 } ],
        "starting_shape": { "feature": 15 },
        "objective": "max_stiffness", "mass_target": 0.25, "safety_factor": 2,
        "member_size": 4, "iterations": 150, "resolution": { "density": 0.5 },
        "symmetry": { "planes": ["right"], "point": [0, 0, 0] },
        "methods": [
          { "type": "unrestricted" },
          { "type": "additive", "directions": ["+z", "+y"], "overhang": 45 },
          { "type": "cutting", "directions": ["+y"] },
          { "type": "casting", "directions": ["+y"], "parting": [0, 10, 0] },
          { "type": "milling", "directions": ["+z", "-z"], "tool_diameter": 6 }
        ],
        "materials": [ { "name": "Aluminium 6061-T6", "density": 2700, "mechanical": { … } },
                       { "name": "Titanium (Ti-6Al-4V)", "density": 4430, "mechanical": { … } } ],
        "load_cases": [ { "name": "Load Case 1", "fixtures": ["Fixed-1"], "loads": ["Force-1"] },
                        { "name": "Load Case 2", "fixtures": ["Fixed-1"], "loads": ["Pressure-1"] } ]
      }
    }
  ]
}
```

- **`model`**: `kind` is `part` or `assembly`; `path` is relative to the study document's folder, with `/` separators. `document_id` is the model's, so a moved or renamed model can be found again by it, as an assembly finds its parts (§6).
- **Studies**: `name` is unique in the document. `type` is `static` (linear static stress: displacements, stresses, factor of safety), `thermal` (the temperatures once heat flow has settled), `transient_thermal` (the temperatures over time) `thermal_stress` (the steady temperatures, then the stresses of the expansion they cause), `topology` (the stiffest shape that keeps a share of the model's mass under fixtures and loads) or `generative` (shapes grown between bodies to keep and bodies to stay clear of, for several materials and ways of making them: generative design, as Fusion 360 has it); another type is an error. `fixtures`, `loads` and `contacts` may be absent (empty). Their `name`s are unique within the study, together (`Fixed-1`, `Force-2`, `Temperature-1`, `Contact-1`, as SolidWorks numbers them). `suppressed` items are kept but not applied.
- **What each type takes**: a `static` study takes fixtures and the `force`, `pressure` and `gravity` loads; `thermal` and `transient_thermal` take the thermal loads only (no fixtures); `thermal_stress` takes all of them; `topology` takes fixtures and the `force` and `pressure` loads (no `gravity`: the weight would change with the shape), and its `topology` block; `generative` takes fixtures and the `force` and `pressure` loads, and its `generative` block, but no `contacts`, `mesh` or `materials` (its grid and materials are in the block). Readers refuse a study holding what its type does not take. `transient` is only in a `transient_thermal` study and `reference_temperature` only in a `thermal_stress` one; readers refuse them elsewhere.
- **Faces** are written as a mate's references are (§6): `face` is a FaceRef (§3 *References*), resolved in the rebuilt part. In an assembly study, `component` names the component holding it (from 1) and `path`, when the component is a sub-assembly, the component ids inside it down to the part; in a part study both are absent. Readers refuse a face of the other kind.
- **Fixtures**: `fixed` (no displacement at all) or `roller` (roller/slider: free in the face's plane, not along its normal).
- **Loads**: `force` is a total in N shared over its faces by area, along `direction`: `"normal"` (normal to each face, into it) or a vector in world coordinates (Y up; only its direction counts, not zero). A negative force pushes the other way. `pressure` is in MPa (N/mm²), into each face along its normal. `gravity` has no faces: an `acceleration` vector in m/s², in world coordinates, applied to the whole model with its materials' densities.
- **Thermal loads**, temperatures in °C (not under −273.15): `temperature` holds the faces at `temperature`. `heat_power` is a total heat flow into the faces in W, shared by area (`power`; negative takes heat out). `heat_flux` is in W/m² into the faces (`flux`). `convection` cools (or warms) the faces towards a fluid at `ambient` with a film `coefficient` in W/(m²·K), not negative. `radiation` exchanges heat with surroundings at `ambient` by `emissivity` (0 to 1) × σ (T⁴ − T∞⁴), in kelvin. `internal_heat` is a `power` in W made inside the bodies the faces belong to, spread through their volume. Faces with no thermal load are insulated.
- **`transient`** (a `transient_thermal` study; these defaults when absent): the `duration` in seconds (positive), the number of time `steps` (1 to 100 000; 60) and the uniform `initial_temperature` the model starts at (°C; 20). The boundary conditions apply from the first instant.
- **`reference_temperature`** (a `thermal_stress` study; 20 when absent): the temperature, °C, at which the model is free of stress; it expands by its materials' `expansion` (§3) times the difference.
- **`topology`** (a `topology` study; these defaults when absent, and readers refuse it in another type): `target_mass`, the share of the model's mass the shape keeps, between 0 and 1, both excluded (0.5); `member_size`, the thinnest member wanted in mm, positive (absent: as thin as the mesh allows, about three elements); `iterations`, the most the optimization makes, 1 to 10 000 (60); `preserved`, regions whose material stays: a `name` unique with the study's items (`Preserve-1`), `faces`, a `depth` in mm under the faces (0 or absent: the elements on the faces only) and `suppressed`. The faces of fixtures and loads keep their material without being listed. The manufacturing controls, each absent for none: `symmetry`, the planes the shape is mirrored about, `planes` naming the standard planes they are parallel to (`right`, normal to X; `top`, Y; `front`, Z; at least one, another name an error) and `point` the point in mm they pass through (absent: the origin, the standard planes themselves); `pull`, the direction a mould is pulled off the shape along, so that going that way the shape never widens and has no undercut: `direction` a vector in world coordinates (Y up; only its direction counts, not zero), and `parting` a point in mm for a two-part mould pulled off both ways from the plane through it across the direction (absent: one way). `one_piece: true` asks for the shape in one piece: once optimized, the pieces of the shape cut at half density that hold a fixture, a load or a preserved region but that the loads do not reach are joined to the one the loads are on, and loose bits holding none of them taken away (absent or false: the shape as the optimization leaves it, floating pieces and all). Readers that do not know these keys (an earlier build of version 3) ignore them.
- **`generative`** (a `generative` study, required there; readers refuse it in another type). The model's bodies by role, each a body as §3 *Bodies* names it (`{"feature": 7, "index": 0}`), in an assembly's study with the `component` holding it and its `path`, as faces have them (`{"component": 2, "path": [5], "feature": 7, "index": 0}`; readers refuse a body of the other kind): `preserve`, the bodies kept as they are (at least one); `obstacles`, bodies no material may go into, each `body` with an `offset` in mm that grows it (0 or absent: none); `starting_shape`, a body the material starts in and stays inside (absent: the box around the preserve bodies, grown on every side by `margin` mm, absent a tenth of the box's largest side). A body has one role at most; bodies with none are left out. Fixtures and loads must be on faces of preserve bodies: Oxyde refuses to generate otherwise (a reader cannot tell which body a face is on without rebuilding the part). `objective`: `max_stiffness`, the stiffest shape for `mass_target`, the outcome's mass in kg (positive, required with this objective), preserve bodies included, the same for every material; or `min_mass`, the lightest shape whose estimated von Mises stress stays under the material's yield strength (else its tensile strength) over `safety_factor` and, with a `displacement_limit` (mm, positive, only with this objective), whose displacement stays under it; `mass_target` then optional and unused (another objective is an error; absent: `max_stiffness`). `safety_factor`: what each outcome's estimated factor of safety is held against, the limit itself when minimizing mass (positive; 2). `member_size`: the minimum thickness wanted, in mm (positive; absent: three voxels): the density filter's radius is half of it, which discourages thinner members without ruling them out. `iterations`: the most each outcome makes, 1 to 10 000 (150). `resolution`: the voxel size, as `mesh` gives an element size: `density` from coarse (0) to fine (1) relative to the design space (0.5 when neither is given), or `element_size` in mm, not both. `symmetry`: as in `topology`. `methods`, the ways of making it, at least one, no `type` twice: `unrestricted` (no constraint); `additive` (printed in layers along each of `directions`, nothing overhanging more than `overhang` degrees from it in any direction: 45, the only value for now, also when absent; another is an error); `cutting` (2-axis cutting: the shape the same all along each of `directions`, whose sign does not count, so `+y` and `-y` are the same direction there, not both); `casting` (a mould pulled off along each of `directions`, one way, or both ways from the plane through `parting`, a point in mm, across it: `topology`'s `pull`); `milling` (3-axis milled from the side each of `directions` points to: no undercut seen from the tool, and pockets or gaps narrower than `tool_diameter`, mm, positive, when given, discouraged: the density filter's radius is at least half of it). `directions` (required except for `unrestricted`, at least one, none twice) are `+x`, `-x`, `+y`, `-y`, `+z` or `-z`, world axes (Y up); every direction is a method of its own. `materials`, at least one: entries shaped as a part's `material` (§3), copied in so the study never depends on a library, with a `density` and a `mechanical` block a solver can use (a material without them cannot be generated), names unique (absent: the part's own material). A study document holds no images, so they go without a texture: readers ignore one. Every material is generated with every method and direction: each pair is an *outcome*. `load_cases` (absent or empty: one case of every fixture and load): sets of the study's fixtures and loads the outcome carries each in turn, as Fusion 360's load cases, the stiffest shape for them all; each with a `name` unique among the cases and the `name`s of its `fixtures` and `loads` (at least one of each, items of the study of that kind, none twice; an item may be in several cases, and a load in none is not applied). A file without the key reads as before.
- **Contacts** between two sets of faces `a` and `b`, as Fusion 360 names them: `bonded` (glued), `separation` (may part, not pass through, no friction), `sliding` (stays in touch, slides without friction), `rough` (may part, not slide) and `offset_bonded` (glued across a gap of `offset` mm; `offset` is 0 or absent for the others). `automatic: true` marks a contact Oxyde found itself, faces of different bodies closer than `tolerance` mm (required then, positive); a manual contact (`automatic` absent) has no `tolerance`.
- **`mesh`** (optional; these defaults when absent): `mesher` `netgen` (default) or `lattice` (Oxyde's own); `quality` `high` (ten-node tetrahedra, default) or `draft` (four-node); and either `density`, from coarse (0) to fine (1) relative to the model's size (0.5 when neither is given), or `element_size` in mm (positive), not both.
- **`materials`** (optional): mechanical properties this study uses instead of the materials' own, by material group: a material's `name`, or `part:` and a part's `document_id` for a part with no material. The values have the same four keys and units as a material's `mechanical` block (§3) and are read the same way: an entry a solver could not use is ignored, and the material's own values (or the typical ones for its name) apply.
- **`thermal_materials`** (optional): thermal properties this study uses instead of the materials' own, by material group as `materials`, with the keys and units of a material's `thermal` block (§3), read the same way.
- **Contacts in thermal studies** conduct heat as if bonded, whatever their `type`.
- **Version.** A document is written with the oldest `format_version` that has every type it holds, so older readers open what they know: only `static` studies, 1; thermal ones too, 2; a `topology` study, 3; a `generative` study, 4. Readers of a version refuse the types of later ones anyway.

## 9. Simulation results (`.oxyres`)

```
bracket-Static 1.oxyres
├── manifest.json                    required  — "kind": "simulation_results"
├── results.json                     required  — what the results are of, summary, entry table
├── mesh/nodes.bin                   required  — f64 × 3 per node
├── mesh/elements.bin                required  — u32 × 4 or × 10 per element
├── mesh/element_bodies.bin          required  — u32 per element
├── mesh/facet_elements.bin          required  — u32 per boundary facet
├── mesh/facet_faces.bin             required  — u8 per boundary facet
├── mesh/facet_labels.bin            required  — u32 per boundary facet
├── fields/displacement.bin          static, thermal stress   — f64 × 3 per node
├── fields/stress.bin                static, thermal stress   — f64 × 6 per node
├── fields/von_mises.bin             static, thermal stress   — f64 per node
├── fields/yield_strength.bin        static, thermal stress   — f64 per node
├── fields/contact_pressure.bin      optional                 — f64 per node
├── fields/temperature.bin           thermal types            — f64 per node
├── fields/heat_flux.bin             thermal types            — f64 × 3 per node
├── fields/temperature_history.bin   transient thermal        — f64 × nodes per time
├── fields/density.bin               topology                 — f64 per element
├── grid/voxels.bin                  generative               — u8 per voxel
├── outcomes/<id>/density.bin        generative               — u8 per voxel
└── outcomes/<id>/snapshots.bin      generative, optional     — u8 × voxels per snapshot
```

The `mesh/` entries are required in every type but `generative`, whose results are on a grid of voxels and have no tetrahedra (nor `bodies` or `labels`); readers ignore `mesh/` entries there.

Results are kept apart from the study so the study stays small and diffs cleanly, and results can be deleted and computed again. They are a cache of a solve, never a source of truth. Oxyde saves each study's results beside its study document as `<study document>-<study name>.oxyres`.

```json
{
  "simulation": "9b2d…",
  "study": "Static 1",
  "type": "static",
  "fingerprint": {
    "study": "fnv1a128:8c0e…",
    "documents": [
      { "document_id": "11111111-…", "hash": "fnv1a128:5d1f…" },
      { "document_id": "6f1c1f6e-…", "hash": "fnv1a128:a07b…" }
    ]
  },
  "units": { "length": "mm", "force": "N", "stress": "MPa", "temperature": "degC", "heat_flux": "W/m2" },
  "summary": {
    "max_von_mises": { "value": 212.5, "node": 1033 },
    "max_displacement": { "value": 0.125, "node": 87 },
    "min_factor_of_safety": { "value": 1.29, "node": 1033 }
  },
  "solver": { "program": "oxyde 0.0.6", "mesher": "netgen", "method": "pcg",
              "iterations": 412, "residual": 1e-9, "seconds": 3.2 },
  "bodies": [ { "name": "Base<1>", "material": "Aluminium 6061-T6" } ],
  "labels": [
    { "label": 0, "component": 1, "face": { "key": { "type": "start_cap", "feature": 2 } } }
  ],
  "entries": [
    { "name": "mesh/nodes.bin", "dtype": "f64", "count": 2140, "components": 3 },
    …
  ]
}
```

- **`simulation`** and **`study`**: the document id of the `.oxysim` (§8) and the name of the study in it. **`type`**: the study's type, as in §8 (absent: `static`); it says which fields there are.
- **`fingerprint`**: what the results were computed from, so a reader can tell stale results. `study` is the hash of the study as `simulation.json` holds it (its name, items, mesh settings and material overrides); `documents` lists every part and assembly file the model is made of, sorted by `document_id`, with the hash of its `model.json` (a part) or `assembly.json` (an assembly: components, placements and mates) as Oxyde writes it. A hash is `fnv1a128:` and 32 lower-case hex digits: FNV-1a, 128 bits, of the entry's bytes. The results hold while every hash matches the model and study as they are now; otherwise they are out of date and should be shown as such until the study is solved again.
- **`units`**: always `mm`, `N`, `MPa`, `degC` and `W/m2` (the last two absent in version 1, which had no temperatures), and `mass` `kg` from version 4; readers refuse others. Displacements are in mm, stresses and strengths in MPa, temperatures in °C, heat fluxes in W/m².
- **`summary`**, what the study computed, each with the node (an index into `mesh/nodes.bin`) where it is reached: for static and thermal stress studies the largest von Mises stress, the largest displacement (its length) and the smallest factor of safety (strength / von Mises stress; absent when no material has a known strength); for thermal studies `max_temperature`, `min_temperature` and `max_heat_flux` (its length), at the end of a transient study; nothing for topology and generative studies, whose numbers are in `topology` and `generative`.
- **`topology`** (topology results, required there): `initial_compliance`, the compliance of the whole model before material was taken away, in N·mm (the work the loads do, `Fᵀu`: the lower, the stiffer); `converged`, whether the densities settled before the iteration limit; `filter_radius`, the radius in mm the densities were averaged over; `history`, every iteration in order, each with its design's `compliance` (N·mm), its mass as a share of the model's (`mass_fraction`) and the largest change of a density it made (`change`). The last iteration is the design `fields/density.bin` holds, as solved; with `one_piece` in the study, `joining` (absent otherwise) says what was done to those densities afterwards: the pieces `joined` to the one the loads are on and the mass added to join them (`added_mass`, a share of the model's), the loose bits `removed` and their mass (`removed_mass`):

  ```json
  "topology": { "initial_compliance": 52.1, "converged": true, "filter_radius": 3.2,
                "history": [ { "compliance": 812.5, "mass_fraction": 0.4, "change": 0.2 }, … ],
                "joining": { "joined": 1, "added_mass": 0.012, "removed": 0, "removed_mass": 0.0 } }
  ```
- **`generative`** (generative results, required there): the `grid` and the `outcomes`. `grid`: `origin`, the corner of the first voxel with the smallest coordinates, in mm; `size`, a voxel's edge in mm; `counts`, the voxels along X, Y and Z. Voxel (i, j, k) spans `origin + size × (i, j, k)` to `origin + size × (i + 1, j + 1, k + 1)` and is record `i + nx (j + ny k)` of every per-voxel array. Each outcome: `id` (from 1, unique: its entries are under `outcomes/<id>/`), `material` (the name of one of the study's `materials`), `method` (one of the study's `methods` with one direction, `direction` in place of `directions`), `status` (`converged`; `limit`, the iteration limit reached; `stopped`, the run stopped with the densities as far as they got; `failed`, with a `message`, which only a failed outcome has, and no entries or `snapshots`), `iterations`, `seconds`, and the outcome's numbers: `mass` (kg), `compliance` (N·mm, the work of the loads summed over the load cases), `max_displacement` (mm), `max_von_mises` (MPa) and `min_factor_of_safety` (absent when the material has no known strength), the last three the worst over the load cases, the last two estimated at the centres of solid voxels; `grayness`, how far its projected densities are from solid or void, `4 ρ (1 − ρ)` averaged over the design voxels, from 0 (each solid or void) to 1 (each at a half), absent for a failed outcome (readers refuse one outside 0 to 1; files written before it was kept have none); `history`, every iteration's `compliance` (summed likewise), `mass` and largest `change` of a density, in order; `snapshots`, the iterations (from 1, increasing) whose densities `snapshots.bin` holds:

  ```json
  "generative": {
    "grid": { "origin": [-40, -5, -30], "size": 0.8, "counts": [150, 90, 75] },
    "outcomes": [
      { "id": 1, "material": "Aluminium 6061-T6", "method": { "type": "additive", "direction": "+z", "overhang": 45 },
        "status": "converged", "iterations": 112, "seconds": 140.2,
        "mass": 0.251, "compliance": 35.2, "max_displacement": 0.081,
        "max_von_mises": 92.4, "min_factor_of_safety": 2.98, "grayness": 0.031,
        "history": [ { "compliance": 120.4, "mass": 0.251, "change": 0.2 }, … ],
        "snapshots": [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110] }
    ]
  }
  ```
- **`times`** (transient thermal): the time, in seconds from the start, of each record of `fields/temperature_history.bin`, the start (0) first, going forward.
- **`solver`**: for the record, free text and numbers: the program, the mesher, the solution method, iterations, the relative residual reached and the wall time in seconds.
- **`bodies`**: the solids of the mesh, by `mesh/element_bodies.bin` index: a display `name` and the material group used (as in §8 `materials`).
- **`labels`**: the model face each facet label stands for, written as §8 faces are (`component` and `path` in an assembly). Labels are unique; `4294967295` (`u32` max) is reserved for facets on no model face and is not listed.
- **`entries`** describes each binary entry: `dtype` (`f64`, `u32` or `u8`), `count` (records: nodes, elements or facets) and `components` (values per record). An entry's length is `count × components × size` bytes exactly. Binary entries are little-endian arrays, record after record, without header or padding.

The arrays:

- `mesh/nodes.bin`: node positions, mm, world coordinates (Y up).
- `mesh/elements.bin`: tetrahedra by node index, 4 per element (linear) or 10 (quadratic): the four corners, positively oriented (the fourth on the side the first three wind counter-clockwise around), then for ten-node elements the middles of the edges 0-1, 1-2, 0-2, 0-3, 1-3 and 2-3.
- `mesh/element_bodies.bin`: the body of each element, an index into `bodies`.
- `mesh/facet_elements.bin`, `mesh/facet_faces.bin`, `mesh/facet_labels.bin`: the boundary facets, one record per facet across the three arrays (same count): the element, the corner the facet is opposite (0 to 3; the facet is the element's other three corners, wound counter-clockwise seen from outside: 1 2 3, 0 3 2, 0 1 3, 0 2 1) and its label.
- `fields/displacement.bin`: mm, x y z. `fields/stress.bin`: the Cauchy stress, MPa, xx yy zz xy yz zx (in a thermal stress study, the stress the expansion leaves once the fixtures hold it). `fields/von_mises.bin`: MPa. `fields/yield_strength.bin`: MPa, the strength the factor of safety is measured against at the node (the yield strength, else the tensile strength; 0 when unknown). `fields/contact_pressure.bin`: MPa, the normal pressure between bodies in contact, present when the study has contacts. All have one record per node; static and thermal stress results have them, thermal results do not.
- `fields/temperature.bin`: °C, one per node: the steady state, or the end of a transient study. `fields/heat_flux.bin`: the heat flux −k ∇T, W/m², x y z, one record per node, at the same time. Thermal, transient thermal and thermal stress results have them.
- `fields/temperature_history.bin` (transient thermal only, and required there): one record per time of `times`, each with `components` equal to the node count: the temperature of every node at that time, °C.
- `fields/density.bin` (topology only, and required there): the density of each element, from 0 (no material) to 1 (solid), one record per element of `mesh/elements.bin`. Topology results have no displacements, stresses or temperatures. The shape is where the density, averaged at the elements' corners, is above a threshold (Oxyde shows 0.5 by default).
- `grid/voxels.bin` (generative only, and required there): what each voxel is: 0 design (material may go there), 1 preserve (always material), 2 obstacle and 3 outside the design space (never material).
- `outcomes/<id>/density.bin` (generative, required for every outcome but a failed one): each voxel's density at the end, from 0 (no material) to 255 (solid), the density × 255 rounded: 255 in preserve voxels, 0 in obstacle and outside ones. These are the densities before the projection that makes them solid or void (the design averaged over the minimum thickness, capped along a casting direction): they vary smoothly where the shape ends. The outcome is where they are above half, interpolated between the voxels' centres, the voxels beyond the grid void (Oxyde draws the preserve bodies exact over it). One byte per voxel keeps a million-voxel outcome to a megabyte before the archive compresses it, and the isosurface needs no more.
- `outcomes/<id>/snapshots.bin` (generative, optional): one record per iteration of the outcome's `snapshots`, each with `components` equal to the voxel count: the densities as `density.bin` has them, at that iteration.
- **Version.** Static results are written with `format_version` 1, thermal ones with 2, topology ones with 3, generative ones with 4.

Readers refuse a described entry that is missing or whose length does not match, a required entry not described, a `dtype` or `components` other than the above for a known entry, field arrays whose count is not the node count (the element count for densities, the voxel count for a grid's arrays), densities outside 0 to 1, voxel kinds above 3, a grid with no voxel along an axis or more than 2³² − 1 in all, outcome ids 0 or used twice, snapshots whose iterations do not go forward or that `snapshots.bin` does not hold one for one, and node, element, body or label indices out of range. They ignore entries, described or not, and keys they do not know, so later versions can add fields (strains, for instance) without a version bump.
