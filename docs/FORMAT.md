# The `.oxy` file format

Status: **draft, version 1**. The format is open: anyone may read and write it, with or without Oxyde.

## 1. Container

An `.oxy` file is a **ZIP archive** (deflate or stored). Required and optional entries:

```
part.oxy
├── manifest.json        required  — format identity and version
├── model.json           required  — the feature tree (the source of truth)
├── properties.json      required  — part metadata, BOM-ready
├── geometry/            optional  — cached results of the last rebuild
│   ├── body-<n>.brep    OCCT BREP text format
│   └── body-<n>.step    optional STEP AP242 copy for kernel-less consumers
├── thumbnail.png        optional  — 256×256 preview
└── attachments/         optional  — embedded images, notes, etc.
```

Rules:

- `manifest.json` should be the first entry so readers can identify the file cheaply.
- All JSON is UTF-8, no BOM, and is written with **stable key order and indentation** so that files diff cleanly under version control.
- Paths inside the archive use `/` and are case-sensitive.
- Everything under `geometry/` is a **cache**: a reader may ignore it and rebuild from `model.json`; a writer may omit it. If the cache's `model_hash` (see manifest) does not match `model.json`, the cache is stale and must not be trusted.

## 2. `manifest.json`

```json
{
  "format": "oxyde",
  "format_version": 1,
  "kind": "part",
  "document_id": "6f1c1f6e-3c9b-4f62-9a47-2d1e8f0c5b11",
  "units": { "length": "mm", "angle": "deg" },
  "created_by": "oxyde 0.1.0",
  "model_hash": "sha256:…",
  "kernel": { "name": "occt", "version": "7.8.1" }
}
```

- `kind`: `"part" | "assembly" | "drawing"`.
- `document_id`: a UUID that never changes for the life of the document; used by assemblies and drawings to find their references even after a rename.
- `units` are the **display** units; numeric values in `model.json` are always stored in internal units (mm, radians).

## 3. `model.json` (part)

```json
{
  "variables": [
    { "name": "width", "expr": "40" },
    { "name": "depth", "expr": "width / 2" }
  ],
  "features": [
    {
      "id": "f1",
      "type": "sketch",
      "name": "Base sketch",
      "plane": { "ref": "origin/XY" },
      "entities": [
        { "id": "p1", "type": "point", "x": 0.0, "y": 0.0 },
        { "id": "p2", "type": "point", "x": 40.0, "y": 0.0 },
        { "id": "l1", "type": "line", "start": "p1", "end": "p2" }
      ],
      "constraints": [
        { "type": "horizontal", "entities": ["l1"] },
        { "type": "distance", "entities": ["p1", "p2"], "value": { "expr": "width" } }
      ]
    },
    {
      "id": "f2",
      "type": "extrude",
      "name": "Boss",
      "profile": { "ref": "f1" },
      "extent": { "type": "blind", "distance": { "expr": "depth" } },
      "operation": "new_body"
    },
    {
      "id": "f3",
      "type": "fillet",
      "name": "Edge fillet",
      "edges": [{ "ref": "f2/lateral/f1:l1/end" }],
      "radius": { "expr": "2" },
      "suppressed": false
    }
  ],
  "rollback_index": null
}
```

- Feature `id`s are unique within the document and never reused.
- `ref` strings are **persistent topological names** (see `ARCHITECTURE.md` §6), never kernel indices.
- Numeric parameters are either a number or `{ "expr": "…" }` referencing `variables`.
- Unknown feature types must be preserved on round-trip by readers that cannot interpret them.

## 4. `properties.json`

Designed so that BOM tools can read a part without understanding geometry:

```json
{
  "part_number": "OXY-0001",
  "description": "Mounting bracket",
  "revision": "A",
  "material": "Aluminium 6061-T6",
  "mass_kg": 0.0421,
  "custom": {
    "supplier": "",
    "finish": "anodized"
  }
}
```

- `mass_kg` is computed at save time from the last successful rebuild; it is informational.
- `custom` is a free-form string-to-string map.

## 5. Assemblies (`kind: "assembly"`)

`model.json` lists component instances instead of features:

```json
{
  "components": [
    {
      "id": "c1",
      "document_id": "6f1c1f6e-3c9b-4f62-9a47-2d1e8f0c5b11",
      "path": "parts/bracket.oxy",
      "transform": [1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1],
      "configuration": null
    }
  ],
  "mates": []
}
```

- Readers resolve components by `path` first, then by searching nearby files for a matching `document_id`.
- A BOM is the count of component instances grouped by `document_id` (and configuration), joined with each part's `properties.json`.

## 6. Versioning

- `format_version` is an integer, incremented on any incompatible change.
- Readers **must** refuse files with a higher `format_version` than they support, with a clear message.
- Readers **must** upgrade older versions through explicit, tested migration steps (v1 → v2 → …), never by guessing.
- Additive changes (new optional fields, new feature types) do not bump the version.
