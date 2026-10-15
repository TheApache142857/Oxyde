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
└── imports/<id>.step    one per import feature — the imported file, byte for byte
```

Rules:

- `manifest.json` is written first so readers can identify a file cheaply.
- All JSON is UTF-8, written with stable key order and indentation so files diff cleanly.
- Geometry is **not stored**: Oxyde rebuilds the part from `model.json` on load. The one exception is imported geometry: an `import` feature's STEP file is kept as it was read, under `imports/`, so the part does not depend on the original file. A later version may add an optional `geometry/` cache (BREP/STEP) for tools that cannot rebuild; readers must ignore entries they do not know.
- Oxyde saves by writing `<name>.<extension>.tmp` and renaming it over the target, so a failed save never destroys the previous file.

## 2. `manifest.json`

```json
{
  "format": "oxyde",
  "format_version": 1,
  "kind": "part",
  "document_id": "6f1c1f6e-3c9b-4f62-9a47-2d1e8f0c5b11",
  "units": { "length": "mm", "angle": "deg" },
  "created_by": "oxyde 0.0.1"
}
```

- `kind`: `"part"`, `"assembly"` or `"drawing"`.
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
| `extrude` | `sketch` (feature id), `operation` (`boss` \| `cut` \| `intersect` \| `new_body`), `extent` (`{"type": "blind", "depth"}` \| `{"type": "mid_plane", "depth"}`), `reverse`, `bodies` (optional, see Bodies below) |
| `revolve` | `sketch`, `operation`, `axis` (`{"sketch_line": entity id}` \| `{"axis": AxisRef}`), `angle` (radians, up to 2π), `mid_plane`, `reverse`, `bodies` (optional) |
| `fillet` | `edges` (list of EdgeRef), `radius` |
| `chamfer` | `edges` (list of EdgeRef), `distance` |
| `shell` | `faces` (list of FaceRef: the faces removed, may be empty), `thickness`, `outward` |
| `plane` | Reference plane: `base` (PlaneRef), `offset` (mm, along the base normal), `flip` |
| `axis` | Reference axis: `axis`: `{"along": AxisRef}` \| `{"two_planes": [PlaneRef, PlaneRef]}` |
| `linear_pattern` | `seeds` (feature ids), `direction` and optional `direction2`: `{"axis": AxisRef, "spacing", "count", "reverse"}` |
| `circular_pattern` | `seeds`, `axis` (AxisRef), `angle` (radians: total with `equal_spacing`, else between instances), `count`, `equal_spacing`, `reverse` |
| `mirror` | `seeds`, `plane` (PlaneRef) |
| `import` | `file_name` (the original file's name, for display), `file`: the archive entry holding the STEP file (`imports/<id>.step`). Its solids are joined to the newest body (or make the first). |
| `combine` | `target` (body), `tools` (bodies), `operation` (`join` \| `cut` \| `intersect`), `keep_tools` (optional, default false) |

Counts include the seed. Seeds of patterns and mirrors are extrusions and revolves (their tool bodies are copied).

**PlaneRef**: `{"standard": "front" | "top" | "right"}`, `{"face": FaceRef}` (a planar face) or `{"reference": feature id}` (a reference plane).
**AxisRef**: `{"standard": "x" | "y" | "z"}`, `{"reference": feature id}` (a reference axis), `{"edge": EdgeRef}` (a straight edge's line, or a circular edge's axis) or `{"face": FaceRef}` (a cylindrical or conical face's axis, or a planar face's normal).

### Bodies

A part may hold several separate solid bodies (as in Fusion 360 or Creo). A body is named by the feature that made it and which of that feature's bodies it is: `{"feature": 7, "index": 0}` (`index` optional, default 0; a pattern of a `new_body` feature makes one body per copy, numbered in order).

- `operation` of an extrusion or a revolve: `boss` joins the swept solid to a body (the first `boss` of a part makes its first body), `cut` removes it from bodies, `intersect` keeps only what it shares with a body, `new_body` keeps it as a body of its own.
- `bodies` lists the bodies a `boss`, `cut` or `intersect` works on. Absent or empty means automatic: a `boss` or an `intersect` goes into the body the sketch's face is on (else the newest body), a `cut` goes through every body. Several bodies in a `boss` are joined into the first.
- Fillets, chamfers and shells work on the bodies their edges or faces are on. Pattern and mirror copies do what their seed did, to the same bodies.
- A `combine` changes its `target` body by its `tools`, which disappear unless `keep_tools`.
- A body cut away entirely disappears. Face and edge names stay unique across all bodies.

Every feature may also carry `dimension_names` (optional, default empty): custom names of its own dimensions, keyed by parameter: `depth` (extrude), `angle` (revolve, circular pattern), `radius` (fillet), `distance` (chamfer), `thickness` (shell), `offset` (reference plane), `spacing`, `count`, `spacing2`, `count2` (patterns). Unlisted parameters have default names numbered in that order within the feature: `D1`, `D2`…

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
| `parallel`, `perpendicular`, `equal_length` | `a`, `b` (lines) |
| `equal_radius` | `a`, `b` (circles or arcs) |
| `distance`, `horizontal_distance`, `vertical_distance` | `a`, `b` (points), `value` |
| `length` | `line`, `value` |
| `radius` | `curve`, `value` |
| `angle` | `a`, `b` (lines), `value`: counter-clockwise from `a` to `b` |
| `point_on_line`, `midpoint` | `point`, `line` |
| `point_on_circle` | `point`, `curve` |
| `tangent` | `line`, `curve` |
| `point_line_distance` | `point`, `line`, `value`: distance to the line's infinite extension |
| `symmetric` | `a`, `b` (points), `line`: mirror images about the line's infinite extension |
| `line_distance` | `a`, `b` (lines), `value`: from the middle of `a` to the infinite extension of `b` (for parallel lines) |

Positions are stored as last solved; readers solve the relations again on rebuild.

`next_ids` (optional): `entity` and `relation` are the ids the next new entity and relation will get, `dimension` the number of the next default dimension name (`D3`). They keep ids from being reused after a deletion, which would silently re-attach face references and equations to other geometry. Readers raise them if they are not above every id in use. A dimension without a `name` gets the next default name on load.

Sketch coordinates are in the sketch plane's frame. For standard planes: Front = XY, Top = XZ (sketch Y towards −Z), Right = YZ (sketch X towards −Z). On a face, the origin is the model origin projected onto the face and the X axis is the model axis least aligned with the face normal, projected (on a tie within 1e-9, the first of X, Y, Z).

### References: persistent names

References to geometry are never indices; they are the persistent names described in `ARCHITECTURE.md` §6, written as JSON:

- **FaceRef** `{ "key": FaceKey, "part": n }`, where FaceKey is one of:
  - `{"type": "profile", "feature": id, "segment": sketch_entity_id}`: side face swept by an extrusion or a revolve
  - `{"type": "start_cap", "feature": id}`, `{"type": "end_cap", "feature": id}`: ends of an extrusion or of a revolve of less than a full turn
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

All fields are optional when reading. `volume_mm3` is computed at save time and informational only.

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
      "rotation": [[1, 0, 0], [0, 1, 0], [0, 0, 1]], "translation": [20, 0, 10] }
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

- **Components** are instances of part files or of other assemblies (sub-assemblies; the file's manifest says which). `path` is relative to the assembly's folder, with `/` separators. When the file is not there, Oxyde looks in the assembly's folder for a part with the same `document_id`, so a moved or renamed part is still found. `name` is unique in the assembly (`Stem<n>`, as in SolidWorks). A component's placement maps part coordinates to assembly coordinates: `p' = rotation · p + translation` (rotation rows, an orthonormal matrix with determinant +1; lengths in mm). `fixed` components never move when mates are solved; `hidden` is display only.
- **Mates**: `type` is `coincident`, `concentric`, `parallel`, `perpendicular`, `distance` (`value` in mm) or `angle` (`value` in radians, 0 to π; an angle with a line is read as at most π/2, and between a line and a plane it is measured from the plane). `a` and `b` name a component, an optional `path` and one of: `face` or `edge` (persistent names, as in a part's `model.json`, resolved in that part's rebuilt body), `plane` (`front`, `top`, `right`: the part's standard planes), `axis` (`x`, `y`, `z`) or `origin` (`true`). `aligned`: normals or axes point the same way (otherwise they face each other). `suppressed` mates are kept but not solved. When the component is a sub-assembly, `path` lists component ids inside it, level by level, down to the part holding the face or edge (`"path": [3, 1]`: component 3 of the sub-assembly, then component 1 of that one); without `path`, `plane`, `axis` and `origin` are the sub-assembly's own.
- Placements are the solved result, saved so that a reader without a solver sees the assembly as it was. Oxyde re-solves on open, starting from them.
- Readers check that component and mate ids are unique and below the `next_…` counters, that component names are unique, and that mates refer to existing components.

- **Sub-assemblies** are rigid in their parent: their components keep the placements saved in their own file, and only the parent's own components move when the parent's mates are solved. An assembly must not contain itself, directly or through other assemblies; readers refuse such a component.
