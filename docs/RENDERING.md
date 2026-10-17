# Rendering: a plan for a Render tab

Status: **first steps built** (October 2026): the CPU path tracer and the Render tab of
§3 step 3, the same path tracer on the GPU (the end of step 4), and the glTF exporter of
step 1, without any change to the file formats (see §6). The Blender add-on, a
rasterised PBR preview and the looks in the files wait for the owner's answers in §5.

This document lays out how Oxyde could get a Render
tab, the way SolidWorks has Visualize and Fusion 360 has its Render workspace: a picture of
the part or assembly with real materials, lights and shadows, rather than the viewport's
engineering shading. It weighs two routes, a renderer inside Oxyde and a link to Blender,
and recommends doing both, in that order, in small steps. Decisions here are proposals
for the owner; the roadmap (ARCHITECTURE.md §10) gets a row once one is taken.

## 1. What we have

- Two viewport renderers that agree pixel for pixel: the wgpu one (`gpu.rs`) and the
  software one (`oxyde-render`), both orthographic, with one directional light, ambient
  occlusion, floor reflections, per-face `Material { color, opacity, texture }` and
  triplanar textures. Neither knows metals, roughness, refraction, lights, shadows or a
  camera with perspective.
- Appearances on faces, bodies, parts and components (`oxyde_model::Appearances`), and
  physical materials with a density and a look (`Material`, the library in
  `materials.json`). Appearances carry an sRGB colour, a transparency and a colour texture
  only.
- A software snapshot (`oxyde_render::snapshot`, the command line's `snapshot`) and, since
  this round, File › Export › Image, which draws the viewport scene at any size, and File
  › Export › OBJ / 3MF with one named object per part or body and the part's colour.
- An export path for whole assemblies placed (the STEP, OBJ and 3MF exports walk
  `AssemblyDoc::leaves`), which is exactly what a renderer needs: every visible part, its
  world placement, its mesh and its appearance.

## 2. The two routes

### A. A renderer inside Oxyde

A **Render tab** next to Features, Sketch and Evaluate (and in assemblies next to
Assembly and Evaluate), with its own panel: scene (environment, floor, background),
camera (perspective, focal length, depth of field later), materials (a render look per
appearance: metal/dielectric, roughness, normal and roughness maps later), output (size,
samples, denoise) and a **Render** button that draws progressively into the viewport and
saves PNG.

Three ways to draw it, in increasing cost and quality:

1. **Rasterised PBR preview on wgpu**: metallic-roughness shading (the glTF model),
   image-based lighting from an HDRI (prefiltered once into a small cube map), shadow
   maps for one or two lights, screen-space reflections, the existing AO. Interactive,
   runs where the viewport runs (Direct3D 12, Vulkan, Metal). This is what solidcraft
   calls "opaque PBR with fixed direct lighting"; its own notes say refraction, HDRI and
   texture maps are not done, which is where the effort goes.
2. **A path tracer on the CPU**, in `oxyde-render`: the software rasterizer already owns
   the scene, the camera and the materials; a path tracer over the same triangles (a BVH,
   a few hundred lines) gives soft shadows, glossy reflections, refraction and an HDRI
   environment correctly with no shader work, progressively, on every core (the
   `std::thread::scope` pattern of the Section View). Slow for big assemblies but exact,
   and it is the reference picture the GPU path can be tested against, as the software
   rasterizer is the reference for the viewport today.
3. **A path tracer on the GPU** (wgpu compute, no ray-tracing extension needed): the same
   algorithm as 2, ten to fifty times faster. Only worth it once 2 exists and is right.

Oxyde's rules fit this well: the renderer lives in `oxyde-render` and `oxyde-app`, reads
`Mesh`, `Material` and placements only, and knows nothing of OCCT or the model. The
scene it needs is one struct, built by one function from the document (part or assembly),
reusing `leaves` and `assembly_materials`.

What it costs: the HDRI/IBL and material work is the bulk; Rust crates for images (`image`
is in already; `.hdr` and `.exr` readers exist as crates, a new dependency to ask about),
a BVH (write it, ~300 lines, or a crate), a denoiser later (Open Image Denoise is C++,
so: not for now; a simple bilateral or à-trous filter in Rust does enough for previews).

### B. A link to Blender

Blender is the best free renderer there is (Cycles, EEVEE), with a material editor,
HDRIs, cameras and compositing nobody should rewrite. The link is an **export plus an
add-on**:

1. **Export what Blender reads well**: glTF 2.0 (`.glb`), which carries the scene tree,
   per-object transforms, PBR materials (base colour, metallic, roughness, alpha, textures)
   and is Blender's best-supported interchange format; OBJ (done, one object per part,
   colours as `.mtl` later) as the low road; USD later if assemblies get big. A glTF
   writer is small (JSON + binary buffers, no dependency needed) and useful far beyond
   Blender (three.js, game engines, Windows 3D Viewer).
2. **A render look per appearance**: metallic, roughness, index of refraction, emission,
   normal/roughness maps, kept in `model.json` next to the appearance (a format change,
   new optional keys: FORMAT.md). Both routes need this; it is the one piece of modelling
   data the plan adds.
3. **A Blender add-on** (`tools/blender/oxyde_bridge/`, Python, 300–500 lines): *File ›
   Import › Oxyde (.oxyprt/.oxyasm)* that calls `oxyde-cli export --out scene.glb` and
   imports the result, sets up a studio (HDRI, floor, camera framed to the part), maps
   the looks to Principled BSDF nodes, and a **Reload** button that re-exports after the
   part changed in Oxyde and swaps the meshes while keeping the lights, camera and any
   material edits made in Blender (objects are matched by Oxyde's stable names:
   component path, body number). A *Render in Blender* item in Oxyde's Render tab can
   launch Blender with that script when its path is set in Preferences.
4. **Live link (later)**: Oxyde writes the scene to a folder on every save or on demand
   and the add-on watches it; or a small socket. Not needed for a first version.

What it costs: the glTF writer and the render looks (both reusable by route A), the
Python add-on, and a Blender path in Preferences. Everything else is Blender's.

## 3. Recommendation: both, in this order

1. **Render looks on appearances** (metallic, roughness, IOR, emission; maps later) and
   a **glTF exporter** in `oxyde-format`, with the CLI's `export` growing `.glb`. Small,
   testable, useful on its own (Blender, three.js, Windows). Format change: ask first.
2. **The Blender add-on**, import + reload. Gives real renders in weeks, not months, and
   shows which looks and scene settings people actually touch before any renderer is
   written in Oxyde.
3. **A Render tab with the CPU path tracer** in `oxyde-render`: perspective camera,
   HDRI environment with a floor and shadow catcher, the looks of step 1, progressive
   drawing into the viewport, PNG out, on every core. The reference renderer.
4. **A wgpu PBR preview** of the same scene for interactive placing of camera and lights,
   tested against the path tracer the way the GPU viewport is tested against the software
   one today; then the GPU path tracer if the CPU one is too slow for assemblies.

Steps 1–2 and 3 are independent: a session each, after the owner picks the looks' keys.

## 4. Design sketch for the Render tab

- **Model**: `RenderSetup` saved in the document (`render.json` entry, or a key in
  `model.json`/`assembly.json`): camera (position, target, up, focal length, perspective
  or orthographic), environment (HDRI file or a built-in studio, rotation, strength,
  background shown or transparent), floor (on/off, shadow catcher, reflection), lights
  (a list of key/fill/rim lights, later), output (width, height, samples, denoise). One
  setup per document to begin with; named setups later, as in Visualize.
- **Looks**: `Appearance` gains optional `look: Look { metallic, roughness, ior,
  emission, normal_map, roughness_map }`. The viewport keeps ignoring them; the renderers
  and the glTF export use them. The material library's materials get a default look
  (brushed aluminium, steel, ABS, glass…), so a part with physical materials renders
  plausibly without any editing.
- **Scene building**: `render_scene(doc) -> RenderScene { meshes: Vec<(Mesh, Placement,
  Vec<Look>)>, camera, environment, floor }`, one function for parts and assemblies,
  built from `leaves` + `assembly_materials` as the mesh exporters are. Meshes at export
  precision (`tessellate_for_export`), since the viewport mesh is coarse for close-ups.
- **Progressive drawing**: the render runs on a job thread (`jobs.rs`), accumulates
  samples into a float buffer, and posts a tonemapped frame to the UI every ~100 ms;
  moving the camera restarts it. Cancel at any time; nothing is written until Save.
- **Where it shows**: the viewport is replaced by the render image while the tab is
  active (as the mesh export preview does), with the camera still orbitable (orbit =
  restart); the panel on the left holds the setup; a *Save image…* button writes PNG.
- **Testing**: a small scene (a sphere on a floor under a uniform sky) has a known answer
  (the furnace test: a white diffuse sphere in a white environment renders white; a
  mirror sphere shows the environment), run in `oxyde-render`'s unit tests at low sample
  counts with a fixed seed, so the renderer stays deterministic and the GPU path can be
  compared to it.
- **Keep the kernel out of it**: the renderers see meshes only, through `oxyde-render`;
  the Blender add-on sees files only, through the CLI. Nothing new crosses into
  `oxyde-model` but the looks.

## 5. Open questions for the owner

1. Looks on appearances: agreed to add them to `model.json` (new optional keys)? Which
   set first: `metallic`, `roughness`, `ior`, `emission`?
2. glTF as the interchange format for Blender (and USD later), or OBJ + `.mtl` only?
3. Render setup saved in the document (a new archive entry) or in the preferences per
   document? SolidWorks and Fusion keep it with the document; I would too.
4. HDRI files: a couple bundled (small, 1k, licence-free) or user-supplied only?
5. Dependencies: an `.hdr`/`.exr` reader crate (or write a Radiance HDR reader, ~100
   lines, and skip EXR), and whether a BVH crate is preferred to an in-house one.

## 6. What is built

Built without touching the file formats or adding a dependency, so every answer of §5
is still open:

- **Path tracer** (`oxyde_render::pathtrace`): a binned-SAH BVH written in house, the
  metallic-roughness BSDF (Lambert under GGX with Schlick's Fresnel) and smooth glass,
  next-event estimation towards the environment's lights with multiple importance
  sampling, Russian roulette, a shadow-catcher or visible floor, perspective or
  orthographic camera, ACES tone curve, an edge-avoiding à-trous denoiser guided by
  albedo, normal and depth. Deterministic: each pixel sample seeds its own generator,
  so the picture does not depend on the threads (tested). The tests are the furnace
  test, a mirror sphere showing a uniform environment, the shadow on the catcher,
  energy conservation of the BSDF, exact plain backgrounds and reproducibility.
- **Environments** are procedural (studio, sky and sun, uniform), no image files: the
  HDRI question (§5.4) and the reader dependency (§5.5) stay open.
- **Looks**: instead of new keys on appearances (§5.1), a *finish* (`oxyde_model::Finish`:
  matte, satin, glossy, brushed metal, polished metal, glass, rubber) is guessed from the
  part's material and the appearance's transparency, and the user can choose another
  per material (or per part without one) in the Render tab. The choices are
  preferences (`render.finishes` in `preferences.json`), as is the whole render setup
  (`render`), not document data (§5.3).
- **Render tab** (`oxyde-app/src/render_ui.rs`): the scene is meshed with
  `tessellate_for_export` on the UI thread when the model or its looks change, the
  render runs on a thread of its own and posts a picture about every 150 ms; camera
  moves restart it once the view is still. Pictures are capped at 8 megapixels
  (`MAX_PIXELS`) and passes accumulate in place, which keeps a render under about 1 GB.
- **glTF** (`oxyde_format::gltf`, File › Export, `oxyde-cli export x.glb`): §2 B.1,
  materials from the finishes. No textures yet (no texture coordinates; they would
  need to be generated from the triplanar mapping).

- **GPU path tracer** (`oxyde-app/src/gpu_trace.rs`, `pathtrace.wgsl`; 0.0.6): the CPU
  tracer ported line for line to a WGSL compute shader on the device egui draws with (no
  ray-tracing extension, no new dependency). `oxyde_render::pathtrace::gpu` lays the
  scene and the settings out as flat words (`TraceScene::gpu_scene`, `GpuFrame`): BVH
  nodes 32 bytes, triangles 64 (normals octahedral in snorm16 pairs), looks 32, one
  uniform block per dispatch at 512-byte strides (the window's device asks for the
  card's largest storage buffers, `device_descriptor` in `main.rs`, instead of wgpu's
  128 MB default, which held the triangles to about two million). The GPU keeps three sums per pixel
  (colour and alpha, albedo and depth, normal: 48 bytes) and adds one sample per pixel a
  band of rows at a time; the bands a frame submits are sized from how long the last
  submission took (`on_submitted_work_done`), aiming at 30 ms, so the UI stays live and
  no dispatch nears the Windows driver timeout. A second entry point develops the sums
  into an sRGB texture egui draws. The final picture is the sums read back (one buffer
  at a time) and developed on the CPU by `develop_sums`, the same code as the CPU
  renderer's, denoiser included. Tests without a GPU: the shader parses and validates
  with naga, its struct sizes equal the CPU-side layouts, the hierarchy as written is
  walkable, normals survive the encoding, the parameters land where the shader reads
  them, and `develop_sums` equals the CPU renderer's own picture. The ignored
  `gpu_tracing_matches_the_cpu` test compares the GPU with the CPU on a furnace sphere
  and a metal sphere in the studio, on a machine with a GPU. The random numbers differ
  from the CPU's (PCG hash per pixel and sample), so pictures agree in the mean, not
  pixel for pixel. Colour textures are not on the GPU yet: a textured face renders in
  its appearance colour there.

Still to do, in the order of §3: the Blender add-on (step 2), HDRI environments, textures
in the GPU tracer and in glTF, a rasterised PBR preview, and moving looks and render
setups into the files once the owner decides how.
