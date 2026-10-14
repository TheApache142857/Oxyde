# The `.oxy` file format

Status: **version 1, implemented** in `crates/oxyde-format`. The format is open: anyone may read and write it, with or without Oxyde.

## 1. Container

An `.oxy` file is a **ZIP archive** (deflate or stored):

```
part.oxy
├── manifest.json        required  — format identity and version (first entry)
├── model.json           required  — the feature timeline: the source of truth
└── properties.json      required  — part metadata, BOM-ready
```

Rules:

- `manifest.json` is written first so readers can identify a file cheaply.
- All JSON is UTF-8, written with stable key order and indentation so files diff cleanly.
- Geometry is **not stored**: Oxyde rebuilds the part from `model.json` on load. A later version may add an optional `geometry/` cache (BREP/STEP) for tools that cannot rebuild; readers must ignore entries they do not know.
- Oxyde saves by writing `<name>.oxy.tmp` and renaming it over the target, so a failed save never destroys the previous file.

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

- `kind`: `"part"` (assemblies and drawings later).
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
        { "type": "fixed", "point": 1 },
        { "type": "horizontal", "line": 5 },
        { "type": "length", "line": 5, "value": 40 },
        { "type": "radius", "curve": 10, "value": 3 }
      ]
    },
    {
      "id": 2, "name": "Boss-Extrude1", "type": "extrude",
      "sketch": 1, "operation": "boss",
      "extent": { "type": "blind", "depth": 10 },
      "reverse": false
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
  ]
}
```

### Timeline

- `features` is the timeline, in order. `id`s are unique, never reused, and features refer to each other by id only; `name` is cosmetic.
- `rollback`: number of active features, or `null` for all.
- `next_feature_id`: id the next new feature will get (readers raise it if it is not above every id).
- `suppressed` defaults to `false`, `construction` and `reverse` too, and `part` defaults to `0`.

### Feature types

| `type` | Fields |
|---|---|
| `sketch` | `plane`: `{"standard": "front" \| "top" \| "right"}` or `{"face": FaceRef}`; `entities` and `relations`, see below |
| `extrude` | `sketch` (feature id), `operation` (`boss` \| `cut`), `extent` (`{"type": "blind", "depth"}` \| `{"type": "mid_plane", "depth"}`), `reverse` |
| `fillet` | `edges` (list of EdgeRef), `radius` |
| `chamfer` | `edges` (list of EdgeRef), `distance` |

### Sketches

`entities`, each with a unique `id` and optional `construction` (solved, but not part of the profile):

| `type` | Fields |
|---|---|
| `point` | `at`: `[x, y]` |
| `line` | `start`, `end`: point ids |
| `circle` | `center`: point id, `radius` |
| `arc` | `center`, `start`, `end`: point ids; counter-clockwise from `start` to `end` |

Connected geometry shares point entities. The ids of lines, circles and arcs are the `segment` numbers in face references, so they must stay stable.

`relations` (optional, default empty); lengths in mm, angles in **radians**:

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

Positions are stored as last solved; readers solve the relations again on rebuild.

Sketch coordinates are in the sketch plane's frame. For standard planes: Front = XY, Top = XZ (sketch Y towards −Z), Right = YZ (sketch X towards −Z). On a face, the origin is the model origin projected onto the face and the X axis is the model axis least aligned with the face normal, projected.

### References: persistent names

References to geometry are never indices; they are the persistent names described in `ARCHITECTURE.md` §6, written as JSON:

- **FaceRef** `{ "key": FaceKey, "part": n }`, where FaceKey is one of:
  - `{"type": "profile", "feature": id, "segment": sketch_entity_id}`: side face swept by an extrusion
  - `{"type": "start_cap", "feature": id}`, `{"type": "end_cap", "feature": id}`: ends of an extrusion
  - `{"type": "from_edge", "feature": id, "edge": EdgeRef}`: fillet/chamfer face
  - `{"type": "from_vertex", "feature": id, "vertex": VertexRef}`: corner blend
  - `{"type": "unknown", "feature": id, "index": n}`: origin not reported (unstable)
- **EdgeRef** `{ "faces": [FaceRef, …], "part": n }`: the faces the edge separates.
- **VertexRef** `{ "faces": [FaceRef, …], "part": n }`: the faces meeting at the vertex.

`part` is 0 unless several entities share the same name; readers sort `faces` lists themselves.

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
- Validate the timeline on open: unique feature ids, every feature refers only to features before it, and sketch entities and relations refer to existing entities of the right kind. An unknown feature or relation `type` is an error in version 1.
- Additive changes (new optional fields, new entries) do not bump the version; anything else does.
