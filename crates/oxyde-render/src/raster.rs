use oxyde_kernel::Mesh;

use crate::camera::Camera;
use crate::material::{Material, Texture, TextureRef};
use crate::style::{EdgePass, HIDDEN_DASH, Rgb, Style};
use crate::vec3::{V3, add, cross, dot, length, lerp, normalize, scale, sub};

/// What is under a pixel: a face or an edge, by index into the shape's topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pick {
    Face(u32),
    Edge(u32),
}

/// Faces and edges drawn in highlight colours.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Highlight {
    pub hovered: Option<Pick>,
    pub selected: Vec<Pick>,
    /// Drawn like `hovered`: everything a hovered tree item stands for (a component, the
    /// faces a feature made).
    pub preselected: Vec<Pick>,
}

impl Highlight {
    fn is_hovered(&self, pick: Pick) -> bool {
        self.hovered == Some(pick) || self.preselected.contains(&pick)
    }
}

struct SceneFace {
    positions: Vec<V3>,
    /// Smooth within the face, so curved faces look round while edges stay crisp.
    normals: Vec<V3>,
    triangles: Vec<[usize; 3]>,
    centroid: V3,
}

/// A display mesh prepared for rendering.
#[derive(Default)]
pub struct Scene {
    faces: Vec<SceneFace>,
    edges: Vec<Vec<V3>>,
    bounds: Option<(V3, V3)>,
    /// One per face, or empty: every face in the style's part colour.
    materials: Vec<Material>,
    textures: Vec<TextureRef>,
}

impl Scene {
    pub fn from_mesh(mesh: &Mesh) -> Self {
        let mut bounds: Option<(V3, V3)> = None;
        let mut grow = |p: V3| {
            bounds = Some(match bounds {
                None => (p, p),
                Some((lo, hi)) => (
                    [lo[0].min(p[0]), lo[1].min(p[1]), lo[2].min(p[2])],
                    [hi[0].max(p[0]), hi[1].max(p[1]), hi[2].max(p[2])],
                ),
            });
        };
        let to_v3 = |p: &[f32; 3]| [p[0] as f64, p[1] as f64, p[2] as f64];

        let mut faces = Vec::with_capacity(mesh.faces.len());
        for face in &mesh.faces {
            let positions: Vec<V3> = face.positions.iter().map(to_v3).collect();
            positions.iter().for_each(|&p| grow(p));
            let triangles: Vec<[usize; 3]> = face
                .triangles
                .iter()
                .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
                .filter(|t| t.iter().all(|&i| i < positions.len()))
                .collect();

            let mut normals = vec![[0.0; 3]; positions.len()];
            for t in &triangles {
                let [a, b, c] = t.map(|i| positions[i]);
                // Unnormalized: larger triangles weigh more.
                let n = cross(sub(b, a), sub(c, a));
                for &i in t {
                    normals[i] = add(normals[i], n);
                }
            }
            let normals = normals
                .into_iter()
                .map(|n| normalize(n).unwrap_or([0.0, 0.0, 1.0]))
                .collect();

            let mut centroid = [0.0; 3];
            if !positions.is_empty() {
                for p in &positions {
                    centroid = add(centroid, *p);
                }
                centroid = scale(centroid, 1.0 / positions.len() as f64);
            }
            faces.push(SceneFace {
                positions,
                normals,
                triangles,
                centroid,
            });
        }

        let edges: Vec<Vec<V3>> = mesh
            .edges
            .iter()
            .map(|e| e.iter().map(to_v3).collect())
            .collect();
        edges.iter().flatten().for_each(|&p| grow(p));

        Scene {
            faces,
            edges,
            bounds,
            materials: Vec::new(),
            textures: Vec::new(),
        }
    }

    /// Gives the faces their materials: one per face (missing ones take the default), with
    /// the textures they index.
    pub fn set_materials(&mut self, mut materials: Vec<Material>, textures: Vec<TextureRef>) {
        materials.resize(self.faces.len(), Material::default());
        for m in &mut materials {
            if m.texture.is_some_and(|t| t >= textures.len()) {
                m.texture = None;
            }
        }
        if materials.iter().all(|m| *m == Material::default()) {
            materials.clear();
        }
        self.materials = materials;
        self.textures = textures;
    }

    pub fn materials(&self) -> &[Material] {
        &self.materials
    }

    pub fn textures(&self) -> &[TextureRef] {
        &self.textures
    }

    fn material(&self, face: usize) -> Material {
        self.materials.get(face).cloned().unwrap_or_default()
    }

    fn texture_of(&self, material: &Material) -> Option<(&Texture, f32)> {
        let t = self.textures.get(material.texture?)?;
        Some((t.as_ref(), material.texture_scale))
    }

    /// Whether any face is see-through.
    pub fn has_transparency(&self) -> bool {
        self.materials.iter().any(Material::is_transparent)
    }

    /// The scene as flat arrays for a GPU: indexed triangles with per-vertex normals and
    /// face indices, and edge segments with their edge index.
    pub fn gpu_mesh(&self) -> GpuMesh {
        let f = |v: V3| [v[0] as f32, v[1] as f32, v[2] as f32];
        let mut mesh = GpuMesh {
            materials: self.materials.clone(),
            textures: self.textures.clone(),
            ..GpuMesh::default()
        };
        let mut next_index = 0u32;
        for face in &self.faces {
            mesh.centroids.push(f(face.centroid));
            let count = (face.triangles.len() * 3) as u32;
            mesh.triangle_ranges.push((next_index, next_index + count));
            next_index += count;
        }
        for (index, face) in self.faces.iter().enumerate() {
            let base = mesh.positions.len() as u32;
            for (p, n) in face.positions.iter().zip(&face.normals) {
                mesh.positions.push(f(*p));
                mesh.normals.push(f(*n));
                mesh.faces.push(index as u32);
            }
            for t in &face.triangles {
                mesh.indices.extend(t.iter().map(|&i| base + i as u32));
            }
        }
        for (index, edge) in self.edges.iter().enumerate() {
            let mut along = 0.0;
            for (&a, &b) in edge.iter().zip(edge.iter().skip(1)) {
                mesh.segments.push((f(a), f(b), index as u32));
                mesh.segment_starts.push(along as f32);
                along += length(sub(b, a));
            }
        }
        mesh
    }

    /// Axis-aligned bounding box, or `None` for an empty scene.
    pub fn bounds(&self) -> Option<(V3, V3)> {
        self.bounds
    }

    /// The model point where the ray from `origin` along `direction` meets what was
    /// picked: the nearest crossing of the face's triangles, or the point of the edge
    /// nearest to the ray. The orbit pivot.
    pub fn hit(&self, pick: Pick, origin: V3, direction: V3) -> Option<V3> {
        match pick {
            Pick::Face(index) => {
                let face = self.faces.get(index as usize)?;
                let mut best: Option<f64> = None;
                for t in &face.triangles {
                    let [a, b, c] = t.map(|i| face.positions[i]);
                    if let Some(s) = ray_triangle(origin, direction, a, b, c) {
                        if best.is_none_or(|d| s < d) {
                            best = Some(s);
                        }
                    }
                }
                best.map(|s| add(origin, scale(direction, s)))
            }
            Pick::Edge(index) => {
                let edge = self.edges.get(index as usize)?;
                let mut best: Option<(f64, V3)> = None;
                for w in edge.windows(2) {
                    let p = nearest_on_segment(origin, direction, w[0], w[1]);
                    // Distance from the ray.
                    let d = sub(p, origin);
                    let across = sub(d, scale(direction, dot(d, direction)));
                    let dist = dot(across, across);
                    if best.is_none_or(|(b, _)| dist < b) {
                        best = Some((dist, p));
                    }
                }
                best.map(|(_, p)| p)
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty() && self.edges.is_empty()
    }
}

/// Distance along the ray to the triangle `a b c` (Möller–Trumbore), if it crosses it.
fn ray_triangle(origin: V3, direction: V3, a: V3, b: V3, c: V3) -> Option<f64> {
    let (e1, e2) = (sub(b, a), sub(c, a));
    let p = cross(direction, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(origin, a);
    let u = dot(s, p) * inv;
    if !(-1e-9..=1.0 + 1e-9).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(direction, q) * inv;
    if v < -1e-9 || u + v > 1.0 + 1e-9 {
        return None;
    }
    Some(dot(e2, q) * inv)
}

/// The point of segment `a b` nearest to the line from `origin` along `direction`.
fn nearest_on_segment(origin: V3, direction: V3, a: V3, b: V3) -> V3 {
    let ab = sub(b, a);
    let len2 = dot(ab, ab);
    if len2 < 1e-18 {
        return a;
    }
    // Minimise |a + t ab − (origin + s direction)|² over t, s (direction is unit).
    let w = sub(a, origin);
    let (d_ab, w_d, w_ab) = (dot(direction, ab), dot(w, direction), dot(w, ab));
    let denom = len2 - d_ab * d_ab;
    let t = if denom.abs() < 1e-12 {
        0.0
    } else {
        ((d_ab * w_d) - w_ab) / denom
    };
    add(a, scale(ab, t.clamp(0.0, 1.0)))
}

/// A [`Scene`] as flat arrays, ready to upload to a GPU.
#[derive(Debug, Clone, Default)]
pub struct GpuMesh {
    pub positions: Vec<[f32; 3]>,
    /// Smooth within each face, as in the software renderer.
    pub normals: Vec<[f32; 3]>,
    /// Face index of each vertex.
    pub faces: Vec<u32>,
    /// Triangles, three vertex indices each.
    pub indices: Vec<u32>,
    /// Edge segments: start, end, edge index.
    pub segments: Vec<([f32; 3], [f32; 3], u32)>,
    /// Where each segment starts along its edge, in millimetres: the dashes of hidden
    /// edges follow it, as in the software renderer.
    pub segment_starts: Vec<f32>,
    /// Range of `indices` holding each face's triangles.
    pub triangle_ranges: Vec<(u32, u32)>,
    /// Centroid of each face, to sort transparent faces by depth.
    pub centroids: Vec<[f32; 3]>,
    /// One per face, or empty for the default everywhere.
    pub materials: Vec<Material>,
    pub textures: Vec<TextureRef>,
}

/// Where the floor is for the reflection: just under the scene (the world is Y-up), and
/// how fast the reflection fades with the height of what it mirrors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Floor {
    pub y: f64,
    pub fade: f64,
}

impl Floor {
    /// The floor of a scene with bounds `lo`..`hi`: a hair below its lowest point, so a
    /// face lying on the floor does not fight with its own reflection.
    pub fn under(lo: V3, hi: V3) -> Self {
        let size = length(sub(hi, lo)).max(1e-3);
        Floor {
            y: lo[1] - size * 0.001,
            fade: ((hi[1] - lo[1]) * 1.5).max(size * 0.25),
        }
    }

    /// Whether the reflection shows from this camera: only when it looks down on the
    /// floor.
    pub fn visible(&self, camera: &Camera) -> bool {
        camera.eye_direction()[1] > 0.02
    }
}

/// A rendered image with its depth and id buffers.
pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// Row-major RGBA, top row first, not premultiplied.
    pub rgba: Vec<u8>,
    depth: Vec<f32>,
    /// Face index + 1 per pixel, 0 = none.
    face_ids: Vec<u32>,
    /// Edge index + 1 per pixel, 0 = none.
    edge_ids: Vec<u32>,
}

impl Frame {
    /// What is at pixel (`x`, `y`). Edges within `radius` pixels win over faces, since they
    /// are thin and hard to hit exactly.
    pub fn pick(&self, x: f64, y: f64, radius: f64) -> Option<Pick> {
        let (cx, cy) = (x.floor() as i64, y.floor() as i64);
        let r = radius.max(0.0).ceil() as i64;
        let mut best: Option<(i64, u32)> = None;
        for dy in -r..=r {
            for dx in -r..=r {
                let d2 = dx * dx + dy * dy;
                if d2 > r * r {
                    continue;
                }
                if let Some(index) = self.index(cx + dx, cy + dy) {
                    let id = self.edge_ids[index];
                    if id != 0 && best.is_none_or(|(bd, _)| d2 < bd) {
                        best = Some((d2, id));
                    }
                }
            }
        }
        if let Some((_, id)) = best {
            return Some(Pick::Edge(id - 1));
        }
        let id = self.face_ids[self.index(cx, cy)?];
        (id != 0).then(|| Pick::Face(id - 1))
    }

    /// The faces and edges inside the rectangle from `min` to `max` (pixels): with
    /// `crossing`, everything with a visible pixel in it, otherwise only what lies
    /// entirely inside (every one of its visible pixels), as SolidWorks' box selection.
    pub fn picks_in(&self, min: [f64; 2], max: [f64; 2], crossing: bool) -> Vec<Pick> {
        picks_in_rect(
            &self.face_ids,
            &self.edge_ids,
            self.width,
            min,
            max,
            crossing,
        )
    }

    fn index(&self, x: i64, y: i64) -> Option<usize> {
        (x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height)
            .then(|| y as usize * self.width + x as usize)
    }

    fn put(&mut self, index: usize, color: [u8; 3]) {
        self.rgba[index * 4..index * 4 + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
    }

    /// Blends `color` over the pixel with `alpha` coverage.
    fn blend(&mut self, index: usize, color: [f32; 3], alpha: f32) {
        let k = index * 4;
        for c in 0..3 {
            let under = f32::from(self.rgba[k + c]);
            self.rgba[k + c] = (under + (color[c] - under) * alpha).clamp(0.0, 255.0) as u8;
        }
        self.rgba[k + 3] = 255;
    }
}

fn rgb(c: [u8; 3]) -> [f32; 3] {
    c.map(f32::from)
}

/// Box selection over id buffers (`index + 1` per pixel, 0 for none, rows of `width`
/// pixels from the top): the faces and edges with a visible pixel inside the rectangle
/// (`crossing`), or with all their visible pixels inside it. Edges come after faces.
pub fn picks_in_rect(
    face_ids: &[u32],
    edge_ids: &[u32],
    width: usize,
    min: [f64; 2],
    max: [f64; 2],
    crossing: bool,
) -> Vec<Pick> {
    use std::collections::HashMap;
    if width == 0 {
        return Vec::new();
    }
    let (x0, x1) = (min[0].min(max[0]), min[0].max(max[0]));
    let (y0, y1) = (min[1].min(max[1]), min[1].max(max[1]));
    let inside = |i: usize| {
        let (x, y) = ((i % width) as f64 + 0.5, (i / width) as f64 + 0.5);
        x >= x0 && x <= x1 && y >= y0 && y <= y1
    };
    // Pixels of each id: inside the rectangle, and in all.
    let count = |ids: &[u32]| -> HashMap<u32, (u32, u32)> {
        let mut out: HashMap<u32, (u32, u32)> = HashMap::new();
        for (i, &id) in ids.iter().enumerate() {
            if id == 0 {
                continue;
            }
            let entry = out.entry(id).or_insert((0, 0));
            entry.1 += 1;
            if inside(i) {
                entry.0 += 1;
            }
        }
        out
    };
    let chosen = |counts: HashMap<u32, (u32, u32)>| -> Vec<u32> {
        let mut ids: Vec<u32> = counts
            .into_iter()
            .filter(|(_, (within, all))| *within > 0 && (crossing || within == all))
            .map(|(id, _)| id - 1)
            .collect();
        ids.sort_unstable();
        ids
    };
    let mut picks: Vec<Pick> = chosen(count(face_ids))
        .into_iter()
        .map(Pick::Face)
        .collect();
    picks.extend(chosen(count(edge_ids)).into_iter().map(Pick::Edge));
    picks
}

/// The highlight colour of a face that is hovered or selected.
fn face_highlight(face: u32, highlight: &Highlight, style: &Style) -> Option<[f32; 3]> {
    if highlight.is_hovered(Pick::Face(face)) {
        Some(rgb(style.face_hovered))
    } else if highlight.selected.contains(&Pick::Face(face)) {
        Some(rgb(style.face_selected))
    } else {
        None
    }
}

/// The colour a face is drawn in, and whether its texture (if any) replaces it:
/// highlights win over the material, the material over the style.
fn face_color(
    face: u32,
    material: &Material,
    highlight: &Highlight,
    style: &Style,
) -> ([f32; 3], bool) {
    match face_highlight(face, highlight, style) {
        Some(color) => (color, false),
        None => (rgb(material.color.unwrap_or(style.face)), true),
    }
}

/// Colour and width in whole pixels (the software renderer draws square pens), and
/// whether the edge is highlighted; `normal` is the colour of an edge that is not.
fn edge_style(edge: u32, highlight: &Highlight, style: &Style, normal: Rgb) -> (Rgb, i64, bool) {
    let px = |w: f32| (w.round() as i64).max(1);
    if highlight.is_hovered(Pick::Edge(edge)) {
        (style.edge_hovered, px(style.highlight_width), true)
    } else if highlight.selected.contains(&Pick::Edge(edge)) {
        (style.edge_selected, px(style.highlight_width), true)
    } else {
        (normal, px(style.edge_width), false)
    }
}

/// Light directions, in model space, fixed relative to the camera.
#[derive(Debug, Clone, Copy)]
struct Lights {
    /// Towards the viewer.
    eye: V3,
    /// Towards the key light.
    key: V3,
}

/// Brightness factor for a surface with unit normal `n`: ambient, plus a headlight so no
/// face goes black, plus a key light so differently oriented faces get different shades.
/// The GPU shader (`gpu.rs` in the application) computes the same.
fn shade(n: V3, lights: &Lights) -> f32 {
    // Closed solids are seen from outside: orient the normal towards the viewer, which also
    // tolerates meshes with inconsistent winding.
    let n = if dot(n, lights.eye) < 0.0 {
        scale(n, -1.0)
    } else {
        n
    };
    let facing = dot(n, lights.eye);
    let key = dot(n, lights.key).max(0.0);
    (0.38 + 0.37 * facing + 0.40 * key) as f32
}

/// How far ambient occlusion looks around a pixel, as a fraction of the image's smaller
/// side, and how deep (in multiples of that reach, in millimetres) a nearer neighbour
/// still counts as hiding the ambient light.
pub const AO_RADIUS: f64 = 0.035;
pub const AO_REACH: f32 = 6.0;

/// Darkens creases, inner corners and the foot of walls: a pixel whose neighbours within
/// `radius_px` lie nearer the viewer than it, by up to [`AO_REACH`] × `radius_mm`, gets
/// less ambient light. Screen-space, from the depth buffer, after the opaque faces and
/// before the edges (which stay crisp); the GPU renderer does the same in a shader. The
/// ids are untouched.
fn ambient_occlusion(frame: &mut Frame, strength: f32, radius_px: f64, radius_mm: f32) {
    let (w, h) = (frame.width, frame.height);
    let depth = &frame.depth;
    // Two rings of six directions.
    let samples: Vec<(f64, f64)> = (0..12)
        .map(|k| {
            let a = k as f64 / 12.0 * std::f64::consts::TAU;
            let r = if k % 2 == 0 { 1.0 } else { 0.55 } * radius_px;
            (a.cos() * r, a.sin() * r)
        })
        .collect();
    let reach = radius_mm * AO_REACH;
    let bias = radius_mm * 0.05;
    let mut factors = vec![1.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let d = depth[i];
            if !d.is_finite() {
                continue;
            }
            let mut occlusion = 0.0f32;
            for (ox, oy) in &samples {
                let (sx, sy) = (x as f64 + ox, y as f64 + oy);
                if sx < 0.0 || sy < 0.0 || sx >= w as f64 || sy >= h as f64 {
                    continue;
                }
                let sd = depth[sy as usize * w + sx as usize];
                let diff = d - sd;
                if diff > bias && diff < reach {
                    occlusion += 1.0 - diff / reach;
                }
            }
            factors[i] = 1.0 - 0.75 * strength * occlusion / samples.len() as f32;
        }
    }
    for (i, k) in factors.into_iter().enumerate() {
        if k < 1.0 {
            for v in &mut frame.rgba[i * 4..i * 4 + 3] {
                *v = (f32::from(*v) * k).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// Renders `scene` seen through `camera` into a `width` × `height` pixel frame, in the
/// default style.
pub fn render(
    scene: &Scene,
    camera: &Camera,
    width: usize,
    height: usize,
    highlight: &Highlight,
) -> Frame {
    render_styled(scene, camera, width, height, highlight, &Style::default())
}

/// [`render`] in a given style.
pub fn render_styled(
    scene: &Scene,
    camera: &Camera,
    width: usize,
    height: usize,
    highlight: &Highlight,
    style: &Style,
) -> Frame {
    let n = width * height;
    let mut frame = Frame {
        width,
        height,
        rgba: vec![0; n * 4],
        depth: vec![f32::INFINITY; n],
        face_ids: vec![0; n],
        edge_ids: vec![0; n],
    };

    // The background gradient, one colour per row.
    let (top, bottom) = (rgb(style.background_top), rgb(style.background_bottom));
    let background: Vec<Rgb> = (0..height)
        .map(|y| {
            let t = y as f32 / (height.max(2) - 1) as f32;
            let mix = |a: f32, b: f32| (a + (b - a) * t) as u8;
            [
                mix(top[0], bottom[0]),
                mix(top[1], bottom[1]),
                mix(top[2], bottom[2]),
            ]
        })
        .collect();
    for (y, &c) in background.iter().enumerate() {
        for x in 0..width {
            frame.put(y * width + x, c);
        }
    }

    let (w, h) = (width as f64, height as f64);
    let lights = Lights {
        eye: camera.eye_direction(),
        key: camera.key_light(),
    };
    let depth_of = |p: V3| -dot(sub(p, camera.target), camera.eye_direction());
    let draw_face =
        |frame: &mut Frame, face: &SceneFace, world: &[V3], flip: bool, paint: &Paint| {
            let projected: Vec<V3> = world.iter().map(|&p| camera.project(p, w, h)).collect();
            for t in &face.triangles {
                let normals = t.map(|i| {
                    let n = face.normals[i];
                    if flip { [n[0], -n[1], n[2]] } else { n }
                });
                raster_triangle(
                    frame,
                    t.map(|i| projected[i]),
                    t.map(|i| world[i]),
                    normals,
                    &lights,
                    paint,
                );
            }
        };

    // The reflection: the faces mirrored in the floor, faded with height, drawn first so
    // the part itself covers it. Front to back, so each pixel is blended once.
    let floor = scene.bounds().map(|(lo, hi)| Floor::under(lo, hi));
    if let Some(floor) = floor.filter(|f| style.reflection() > 0.0 && f.visible(camera)) {
        let mirror = |p: V3| [p[0], 2.0 * floor.y - p[1], p[2]];
        let mut order: Vec<(f64, usize)> = (0..scene.faces.len())
            .map(|i| (depth_of(mirror(scene.faces[i].centroid)), i))
            .collect();
        order.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, index) in order {
            let face = &scene.faces[index];
            let material = scene.material(index);
            let (base, textured) = face_color(index as u32, &material, highlight, style);
            let paint = Paint {
                base,
                alpha: style.reflection() * material.opacity,
                texture: textured.then(|| scene.texture_of(&material)).flatten(),
                fade: Some(&floor),
                background: None,
                write_depth: true,
                id: 0,
            };
            let world: Vec<V3> = face.positions.iter().map(|&p| mirror(p)).collect();
            draw_face(&mut frame, face, &world, true, &paint);
        }
    }

    // How a face is painted: opaque ones write the depth, see-through ones are blended.
    // In the line styles a face shows only in its highlight colour. Otherwise an opaque
    // one is painted in the background, so it still hides what is behind it (a
    // highlighted face too), and a see-through one not at all; both are picked.
    let shaded = style.display.shades_faces();
    let face_paint = |index: usize, opaque: bool| {
        let material = scene.material(index);
        let alpha = if opaque {
            1.0
        } else {
            material.opacity.clamp(0.0, 1.0)
        };
        let (base, alpha, texture, erase) = if shaded {
            let (base, textured) = face_color(index as u32, &material, highlight, style);
            let texture = textured.then(|| scene.texture_of(&material)).flatten();
            (base, alpha, texture, None)
        } else {
            match face_highlight(index as u32, highlight, style) {
                Some(base) => (base, alpha, None, None),
                None if opaque => ([0.0; 3], 1.0, None, Some(background.as_slice())),
                None => ([0.0; 3], 0.0, None, None),
            }
        };
        Paint {
            base,
            alpha,
            texture,
            fade: None,
            background: erase,
            write_depth: opaque,
            id: index as u32 + 1,
        }
    };

    let mut transparent: Vec<(f64, usize)> = Vec::new();
    for (index, face) in scene.faces.iter().enumerate() {
        if scene.material(index).is_transparent() {
            transparent.push((depth_of(face.centroid), index));
            continue;
        }
        draw_face(
            &mut frame,
            face,
            &face.positions,
            false,
            &face_paint(index, true),
        );
    }

    let ao = style.occlusion();
    if ao > 0.0 && width > 0 && height > 0 {
        let radius_px = (width.min(height) as f64 * AO_RADIUS).max(2.0);
        let radius_mm = radius_px / camera.pixels_per_mm(h);
        ambient_occlusion(&mut frame, ao, radius_px, radius_mm as f32);
    }

    // See-through faces, the farthest first, blended over what is behind them. They
    // leave the depth alone, so what is behind stays visible, but they can be picked.
    // Over the edges when the faces are shaded; under them in the line styles, where
    // the edges are what is looked at.
    transparent.sort_by(|a, b| b.0.total_cmp(&a.0));
    let see_through = |frame: &mut Frame| {
        for &(_, index) in &transparent {
            if let Some(face) = scene.faces.get(index) {
                draw_face(
                    frame,
                    face,
                    &face.positions,
                    false,
                    &face_paint(index, false),
                );
            }
        }
    };
    if !shaded {
        see_through(&mut frame);
    }

    // Edges are drawn over faces they lie on (the bias brings them nearer), and in one or
    // two passes by depth as the display style asks: the parts in front of every face,
    // the parts behind one (dashed), or all of them. In *Shaded* they are only drawn
    // when highlighted, but picked all the same.
    let bias = (camera.height * 0.004) as f32;
    let (line, hidden) = (style.edge_color(), style.hidden_edge_color());
    for &pass in style.display.edge_passes() {
        for (index, edge) in scene.edges.iter().enumerate() {
            let normal = if pass == EdgePass::Hidden {
                hidden
            } else {
                line
            };
            let (color, thickness, highlighted) =
                edge_style(index as u32, highlight, style, normal);
            let pen = Pen {
                color: (highlighted || style.display.draws_all_edges()).then_some(color),
                thickness,
                bias,
                id: index as u32 + 1,
                pass,
                pixels_per_mm: camera.pixels_per_mm(h),
            };
            let points: Vec<V3> = edge.iter().map(|&p| camera.project(p, w, h)).collect();
            let segments = points.iter().zip(points.iter().skip(1));
            let world = edge.iter().zip(edge.iter().skip(1));
            let mut along = 0.0;
            for ((&a, &b), (&wa, &wb)) in segments.zip(world) {
                let piece = length(sub(wb, wa));
                draw_line(&mut frame, a, b, [along, along + piece], &pen);
                along += piece;
            }
        }
    }

    if shaded {
        see_through(&mut frame);
    }
    frame
}

/// How a triangle is painted.
struct Paint<'a> {
    base: [f32; 3],
    alpha: f32,
    /// Replaces `base`, with its scale in mm per repeat.
    texture: Option<(&'a Texture, f32)>,
    /// A reflection: fades with the height of the mirrored point above the floor.
    fade: Option<&'a Floor>,
    /// The background's colour per row, painted instead of the face: a face the line
    /// styles leave unseen, which still covers what is behind it.
    background: Option<&'a [Rgb]>,
    write_depth: bool,
    /// Face index + 1, or 0 to leave the id buffer alone.
    id: u32,
}

/// Twice the signed area of the triangle (a, b, (x, y)).
fn edge_fn(a: V3, b: V3, x: f64, y: f64) -> f64 {
    (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0])
}

fn raster_triangle(
    frame: &mut Frame,
    p: [V3; 3],
    world: [V3; 3],
    normals: [V3; 3],
    lights: &Lights,
    paint: &Paint,
) {
    let area = edge_fn(p[0], p[1], p[2][0], p[2][1]);
    if area.abs() < 1e-12 || p.iter().flatten().any(|v| !v.is_finite()) {
        return;
    }
    let min_x = (p[0][0].min(p[1][0]).min(p[2][0]).floor() as i64).max(0);
    let max_x = (p[0][0].max(p[1][0]).max(p[2][0]).ceil() as i64).min(frame.width as i64 - 1);
    let min_y = (p[0][1].min(p[1][1]).min(p[2][1]).floor() as i64).max(0);
    let max_y = (p[0][1].max(p[1][1]).max(p[2][1]).ceil() as i64).min(frame.height as i64 - 1);
    if min_x > max_x || min_y > max_y {
        return;
    }
    let opaque = paint.alpha >= 0.999 && paint.fade.is_none();

    for y in min_y..=max_y {
        let py = y as f64 + 0.5;
        for x in min_x..=max_x {
            let px = x as f64 + 0.5;
            let w0 = edge_fn(p[1], p[2], px, py) / area;
            let w1 = edge_fn(p[2], p[0], px, py) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < -1e-9 || w1 < -1e-9 || w2 < -1e-9 {
                continue;
            }
            let z = (w0 * p[0][2] + w1 * p[1][2] + w2 * p[2][2]) as f32;
            let index = y as usize * frame.width + x as usize;
            if z >= frame.depth[index] {
                continue;
            }
            if paint.write_depth {
                frame.depth[index] = z;
            }
            if paint.id != 0 {
                frame.face_ids[index] = paint.id;
            }
            if let Some(rows) = paint.background {
                if let Some(&c) = rows.get(y as usize) {
                    frame.put(index, c);
                }
                continue;
            }

            let n = add(
                add(scale(normals[0], w0), scale(normals[1], w1)),
                scale(normals[2], w2),
            );
            let n = normalize(n);
            let light = n.map_or(1.0, |n| shade(n, lights));
            let q = add(
                add(scale(world[0], w0), scale(world[1], w1)),
                scale(world[2], w2),
            );
            let base = match paint.texture {
                Some((texture, scale_mm)) => texture.triplanar(
                    [q[0] as f32, q[1] as f32, q[2] as f32],
                    n.map_or([0.0, 1.0, 0.0], |n| [n[0] as f32, n[1] as f32, n[2] as f32]),
                    scale_mm,
                ),
                None => paint.base,
            };
            let color = base.map(|c| (c * light).min(255.0));
            if opaque {
                frame.put(index, color.map(|c| c as u8));
                continue;
            }
            let mut alpha = paint.alpha;
            if let Some(floor) = paint.fade {
                let height = (floor.y - q[1]).max(0.0);
                alpha *= (1.0 - height / floor.fade).clamp(0.0, 1.0) as f32;
            }
            if alpha > 0.0 {
                frame.blend(index, color, alpha);
            }
        }
    }
}

/// Clips segment a–b (screen space) to the rectangle, or `None` if entirely outside.
fn clip(a: V3, b: V3, xmin: f64, xmax: f64, ymin: f64, ymax: f64) -> Option<(V3, V3)> {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, a[0] - xmin),
        (dx, xmax - a[0]),
        (-dy, a[1] - ymin),
        (dy, ymax - a[1]),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                if r > t1 {
                    return None;
                }
                t0 = t0.max(r);
            } else {
                if r < t0 {
                    return None;
                }
                t1 = t1.min(r);
            }
        }
    }
    Some((lerp(a, b, t0), lerp(a, b, t1)))
}

/// How an edge's segments are drawn.
struct Pen {
    /// `None`: into the id buffer only (an edge in *Shaded* that is not highlighted).
    color: Option<Rgb>,
    /// Width of the square pen, in pixels.
    thickness: i64,
    /// Towards the viewer, in millimetres, so an edge wins over the faces it bounds.
    bias: f32,
    /// Edge index + 1.
    id: u32,
    /// Which parts of the edge, by depth; the hidden ones are dashed.
    pass: EdgePass,
    /// To turn the length along the edge into pixels, for the dashes.
    pixels_per_mm: f64,
}

/// Draws the segment from `a` to `b` (screen space), which runs from `along[0]` to
/// `along[1]` millimetres along its edge.
fn draw_line(frame: &mut Frame, a: V3, b: V3, along: [f64; 2], pen: &Pen) {
    if a.iter().chain(b.iter()).any(|v| !v.is_finite()) {
        return;
    }
    let margin = 2.0;
    let Some((ca, cb)) = clip(
        a,
        b,
        -margin,
        frame.width as f64 + margin,
        -margin,
        frame.height as f64 + margin,
    ) else {
        return;
    };
    // Where the clipped ends lie along the edge, so the dashes do not move with the
    // edge of the image.
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let along_at = |p: V3| {
        let t = if len2 < 1e-12 {
            0.0
        } else {
            ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2
        };
        along[0] + (along[1] - along[0]) * t
    };
    let (s0, s1) = (along_at(ca), along_at(cb));
    let (dash, gap) = (f64::from(HIDDEN_DASH.0), f64::from(HIDDEN_DASH.1));
    let steps = (cb[0] - ca[0])
        .abs()
        .max((cb[1] - ca[1]).abs())
        .ceil()
        .max(1.0) as usize;
    let offset = pen.thickness / 2;
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let q = lerp(ca, cb, t);
        let (cx, cy) = (q[0].floor() as i64, q[1].floor() as i64);
        let z = q[2] as f32 - pen.bias;
        // Hidden edges are dashed in colour; the gaps still pick the edge.
        let inked = pen.pass != EdgePass::Hidden
            || ((s0 + (s1 - s0) * t) * pen.pixels_per_mm).rem_euclid(dash + gap) < dash;
        for oy in 0..pen.thickness {
            for ox in 0..pen.thickness {
                let Some(index) = frame.index(cx + ox - offset, cy + oy - offset) else {
                    continue;
                };
                let Some(&depth) = frame.depth.get(index) else {
                    continue;
                };
                let shown = match pen.pass {
                    EdgePass::Visible => z <= depth,
                    EdgePass::Hidden => z > depth,
                    EdgePass::All => true,
                };
                if !shown {
                    continue;
                }
                if let (Some(color), true) = (pen.color, inked) {
                    frame.put(index, color);
                }
                if let Some(id) = frame.edge_ids.get_mut(index) {
                    *id = pen.id;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DisplayStyle, StandardView};
    use oxyde_kernel::FaceMesh;

    /// Face 0: a 20 × 20 square at z = 0. Face 1: a larger square behind it, at z = −5.
    /// Edges 0..4: the outline of face 0 (bottom, right, top, left).
    fn mesh() -> Mesh {
        let square = |half: f32, z: f32| FaceMesh {
            positions: vec![
                [-half, -half, z],
                [half, -half, z],
                [half, half, z],
                [-half, half, z],
            ],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        };
        Mesh {
            faces: vec![square(10.0, 0.0), square(30.0, -5.0)],
            edges: vec![
                vec![[-10.0, -10.0, 0.0], [10.0, -10.0, 0.0]],
                vec![[10.0, -10.0, 0.0], [10.0, 10.0, 0.0]],
                vec![[10.0, 10.0, 0.0], [-10.0, 10.0, 0.0]],
                vec![[-10.0, 10.0, 0.0], [-10.0, -10.0, 0.0]],
            ],
        }
    }

    fn front_camera() -> Camera {
        let mut camera = Camera {
            target: [0.0; 3],
            height: 40.0,
            ..Camera::default()
        };
        camera.set_view(StandardView::Front);
        camera
    }

    #[test]
    fn nearest_face_wins_and_picks_exactly() {
        let scene = Scene::from_mesh(&mesh());
        let frame = render(&scene, &front_camera(), 100, 100, &Highlight::default());
        // 2.5 px per mm: face 0 spans pixels 25..75, face 1 covers the whole frame.
        assert_eq!(frame.pick(50.5, 50.5, 0.0), Some(Pick::Face(0)));
        assert_eq!(frame.pick(5.5, 5.5, 0.0), Some(Pick::Face(1)));
    }

    #[test]
    fn edges_are_picked_near_the_cursor() {
        let scene = Scene::from_mesh(&mesh());
        let frame = render(&scene, &front_camera(), 100, 100, &Highlight::default());
        // The left edge (x = −10 mm) is at pixel column 25.
        assert_eq!(frame.pick(27.0, 50.0, 3.0), Some(Pick::Edge(3)));
        assert_eq!(frame.pick(27.0, 50.0, 0.0), Some(Pick::Face(0)));
    }

    #[test]
    fn empty_pixels_pick_nothing() {
        let scene = Scene::from_mesh(&Mesh {
            faces: vec![mesh().faces[0].clone()],
            edges: Vec::new(),
        });
        let frame = render(&scene, &front_camera(), 100, 100, &Highlight::default());
        assert_eq!(frame.pick(2.0, 2.0, 0.0), None);
        assert_eq!(frame.pick(-5.0, 500.0, 2.0), None);
    }

    #[test]
    fn hidden_edges_are_not_drawn() {
        // Seen from the back, the outline of face 0 is behind the large face 1.
        let scene = Scene::from_mesh(&mesh());
        let mut camera = front_camera();
        camera.set_view(StandardView::Back);
        let frame = render(&scene, &camera, 100, 100, &Highlight::default());
        assert_eq!(frame.pick(75.5, 50.5, 3.0), Some(Pick::Face(1)));
    }

    #[test]
    fn faces_of_a_box_get_distinct_shades_in_the_isometric_view() {
        let camera = Camera::default(); // isometric
        let lights = Lights {
            eye: camera.eye_direction(),
            key: camera.key_light(),
        };
        let x = shade([1.0, 0.0, 0.0], &lights);
        let y = shade([0.0, 1.0, 0.0], &lights);
        let z = shade([0.0, 0.0, 1.0], &lights);
        // Top brightest, front next, right side darkest, as in SolidWorks.
        assert!(y > z && z > x, "x {x}, y {y}, z {z}");
        assert!(z - x > 0.1 && y - z > 0.1);
        // Flipped normals shade the same.
        assert_eq!(shade([-1.0, 0.0, 0.0], &lights), x);
    }

    #[test]
    fn highlighted_faces_change_colour() {
        let scene = Scene::from_mesh(&mesh());
        let plain = render(&scene, &front_camera(), 100, 100, &Highlight::default());
        let selected = render(
            &scene,
            &front_camera(),
            100,
            100,
            &Highlight {
                selected: vec![Pick::Face(0)],
                ..Highlight::default()
            },
        );
        let at = (50 * 100 + 50) * 4;
        assert_ne!(plain.rgba[at..at + 3], selected.rgba[at..at + 3]);
    }

    #[test]
    fn preselected_faces_look_hovered() {
        let scene = Scene::from_mesh(&mesh());
        let draw = |highlight: Highlight| render(&scene, &front_camera(), 100, 100, &highlight);
        let hovered = draw(Highlight {
            hovered: Some(Pick::Face(0)),
            ..Highlight::default()
        });
        let preselected = draw(Highlight {
            preselected: vec![Pick::Face(0), Pick::Face(1)],
            ..Highlight::default()
        });
        let at = (50 * 100 + 50) * 4;
        assert_eq!(hovered.rgba[at..at + 3], preselected.rgba[at..at + 3]);
        // The big face behind is lit up too.
        let corner = (5 * 100 + 5) * 4;
        assert_ne!(
            hovered.rgba[corner..corner + 3],
            preselected.rgba[corner..corner + 3]
        );
    }

    #[test]
    fn scene_bounds_cover_faces_and_edges() {
        let scene = Scene::from_mesh(&mesh());
        let (lo, hi) = scene.bounds().unwrap();
        assert_eq!(lo, [-30.0, -30.0, -5.0]);
        assert_eq!(hi, [30.0, 30.0, 0.0]);
        assert!(Scene::default().bounds().is_none());
    }

    #[test]
    fn lines_far_outside_are_clipped_cheaply() {
        let clipped = clip([-1e9, 50.0, 0.0], [1e9, 50.0, 0.0], 0.0, 100.0, 0.0, 100.0).unwrap();
        assert!((clipped.0[0] - 0.0).abs() < 1e-3 && (clipped.1[0] - 100.0).abs() < 1e-3);
        assert!(
            clip(
                [-10.0, -10.0, 0.0],
                [-5.0, -1.0, 0.0],
                0.0,
                100.0,
                0.0,
                100.0
            )
            .is_none()
        );
    }

    fn pixel(frame: &Frame, x: usize, y: usize) -> [u8; 3] {
        let at = (y * frame.width + x) * 4;
        [frame.rgba[at], frame.rgba[at + 1], frame.rgba[at + 2]]
    }

    #[test]
    fn materials_colour_faces_and_transparent_ones_blend() {
        let mut scene = Scene::from_mesh(&mesh());
        let camera = front_camera();
        let style = Style::LIGHT;
        let plain = render_styled(&scene, &camera, 100, 100, &Highlight::default(), &style);
        // Face 0 red and half transparent, face 1 (behind, covering everything) green.
        scene.set_materials(
            vec![
                Material {
                    color: Some([255, 0, 0]),
                    opacity: 0.5,
                    ..Material::default()
                },
                Material {
                    color: Some([0, 255, 0]),
                    ..Material::default()
                },
            ],
            Vec::new(),
        );
        assert!(scene.has_transparency());
        let frame = render_styled(&scene, &camera, 100, 100, &Highlight::default(), &style);
        let corner = pixel(&frame, 5, 5);
        assert!(
            corner[1] > 100 && corner[0] == 0 && corner[2] == 0,
            "{corner:?}"
        );
        let middle = pixel(&frame, 50, 50);
        // Red over green, half and half: both channels present.
        assert!(
            middle[0] > 60 && middle[1] > 60 && middle[2] == 0,
            "{middle:?}"
        );
        assert_ne!(middle, pixel(&plain, 50, 50));
        // The transparent face is still what the cursor is on; the edges still show.
        assert_eq!(frame.pick(50.5, 50.5, 0.0), Some(Pick::Face(0)));
        assert_eq!(frame.pick(27.0, 50.0, 3.0), Some(Pick::Edge(3)));
        // A selected transparent face takes the selection colour.
        let selected = render_styled(
            &scene,
            &camera,
            100,
            100,
            &Highlight {
                selected: vec![Pick::Face(0)],
                ..Highlight::default()
            },
            &style,
        );
        assert_ne!(pixel(&selected, 50, 50), middle);
    }

    #[test]
    fn textures_are_mapped_over_faces() {
        let mut scene = Scene::from_mesh(&mesh());
        // A 2 × 2 checker: black and white squares, 10 mm per repeat on face 0 (20 mm
        // wide: two repeats, four squares across).
        let px = |v: u8| [v, v, v, 255];
        let checker = Texture {
            width: 2,
            height: 2,
            rgba: [px(0), px(255), px(255), px(0)].concat(),
        };
        scene.set_materials(
            vec![Material {
                texture: Some(0),
                texture_scale: 10.0,
                ..Material::default()
            }],
            vec![std::sync::Arc::new(checker)],
        );
        let frame = render_styled(
            &scene,
            &front_camera(),
            100,
            100,
            &Highlight::default(),
            &Style::LIGHT,
        );
        // Face 0 spans pixels 25..75 (2.5 px/mm). Squares are 5 mm = 12.5 px wide.
        let a = pixel(&frame, 31, 44);
        let b = pixel(&frame, 44, 44);
        assert!(a[0].abs_diff(b[0]) > 100, "{a:?} vs {b:?}");
        // The big face behind keeps the plain colour.
        assert_eq!(pixel(&frame, 5, 5), pixel(&frame, 94, 94));
        // Out-of-range texture indices are ignored rather than crashing.
        scene.set_materials(
            vec![Material {
                texture: Some(7),
                ..Material::default()
            }],
            Vec::new(),
        );
        assert!(scene.materials().is_empty());
    }

    #[test]
    fn the_floor_reflects_the_part_when_asked() {
        // A square standing up (its plane is XY), seen from an isometric view: the
        // reflection shows under it.
        let scene = Scene::from_mesh(&Mesh {
            faces: vec![mesh().faces[0].clone()],
            edges: Vec::new(),
        });
        let (w, h) = (120usize, 120usize);
        let mut camera = Camera::default();
        camera.set_view(StandardView::Isometric);
        let (lo, hi) = scene.bounds().unwrap();
        camera.fit(
            [lo[0], lo[1] - 25.0, lo[2]],
            [hi[0], hi[1], hi[2]],
            w as f64,
            h as f64,
        );
        let plain = render_styled(&scene, &camera, w, h, &Highlight::default(), &Style::LIGHT);
        let mirrored = Style {
            floor_reflection: 0.5,
            ..Style::LIGHT
        };
        let frame = render_styled(&scene, &camera, w, h, &Highlight::default(), &mirrored);
        // Where the reflection is, below the part, the image differs from the plain one;
        // the part itself and the ids are untouched.
        let below = camera.project([0.0, -14.0, 0.0], w as f64, h as f64);
        let (bx, by) = (below[0] as usize, below[1] as usize);
        assert_ne!(pixel(&frame, bx, by), pixel(&plain, bx, by));
        assert_eq!(frame.pick(bx as f64, by as f64, 0.0), None);
        let on = camera.project([0.0, 5.0, 0.0], w as f64, h as f64);
        let (ox, oy) = (on[0] as usize, on[1] as usize);
        assert_eq!(pixel(&frame, ox, oy), pixel(&plain, ox, oy));
        assert_eq!(frame.pick(ox as f64, oy as f64, 0.0), Some(Pick::Face(0)));
        // From below the floor there is no reflection.
        camera.set_view(StandardView::Bottom);
        let frame = render_styled(&scene, &camera, w, h, &Highlight::default(), &mirrored);
        let plain = render_styled(&scene, &camera, w, h, &Highlight::default(), &Style::LIGHT);
        assert_eq!(frame.rgba, plain.rgba);
    }

    #[test]
    fn ambient_occlusion_darkens_the_foot_of_a_step() {
        // Face 0 stands 5 mm in front of face 1: on face 1, the pixels next to face 0's
        // edge lose ambient light; those far from it, and face 0 itself (nothing is in
        // front of it), keep their shade. The ids never change.
        let scene = Scene::from_mesh(&mesh());
        let camera = front_camera();
        let plain = render_styled(
            &scene,
            &camera,
            100,
            100,
            &Highlight::default(),
            &Style::LIGHT,
        );
        let occluded = Style {
            ambient_occlusion: 1.0,
            ..Style::LIGHT
        };
        let frame = render_styled(&scene, &camera, 100, 100, &Highlight::default(), &occluded);
        assert_eq!(frame.face_ids, plain.face_ids);
        let red = |f: &Frame, x: usize, y: usize| f.rgba[(y * 100 + x) * 4];
        // 2.5 px per mm: face 0 spans pixels 25..75. Just outside it, on face 1.
        assert!(red(&frame, 22, 50) < red(&plain, 22, 50));
        // Far from the step, and on face 0 (seen through nothing) : unchanged.
        assert_eq!(red(&frame, 5, 50), red(&plain, 5, 50));
        assert_eq!(red(&frame, 50, 50), red(&plain, 50, 50));
        let darker = (0..100 * 100)
            .filter(|i| frame.rgba[i * 4] < plain.rgba[i * 4])
            .count();
        let brighter = (0..100 * 100)
            .filter(|i| frame.rgba[i * 4] > plain.rgba[i * 4])
            .count();
        assert!(
            darker > 0 && brighter == 0,
            "{darker} darker, {brighter} brighter"
        );
    }

    #[test]
    fn box_selection_takes_what_is_inside_or_touched() {
        let scene = Scene::from_mesh(&mesh());
        let frame = render(&scene, &front_camera(), 100, 100, &Highlight::default());
        // Face 0 spans pixels 25..75; face 1 shows around it. A box around face 0 only.
        let inside = frame.picks_in([20.0, 20.0], [80.0, 80.0], false);
        assert_eq!(
            inside,
            vec![
                Pick::Face(0),
                Pick::Edge(0),
                Pick::Edge(1),
                Pick::Edge(2),
                Pick::Edge(3)
            ]
        );
        // Crossing: the big face behind is touched too.
        let crossing = frame.picks_in([20.0, 20.0], [80.0, 80.0], true);
        assert!(crossing.contains(&Pick::Face(1)) && crossing.contains(&Pick::Face(0)));
        // A box over the left half: nothing lies entirely inside, but the left edge.
        let half = frame.picks_in([10.0, 10.0], [40.0, 90.0], false);
        assert_eq!(half, vec![Pick::Edge(3)]);
        let half = frame.picks_in([10.0, 10.0], [40.0, 90.0], true);
        assert!(half.contains(&Pick::Face(0)) && !half.contains(&Pick::Edge(1)));
        // Nothing in an empty corner... of face 1, which covers it.
        assert_eq!(
            frame.picks_in([0.0, 0.0], [3.0, 3.0], true),
            vec![Pick::Face(1)]
        );
    }

    #[test]
    fn styles_change_colours_and_can_hide_edges() {
        let scene = Scene::from_mesh(&mesh());
        let camera = front_camera();
        let style = Style::DARK;
        let empty = render_styled(
            &Scene::default(),
            &camera,
            100,
            100,
            &Highlight::default(),
            &style,
        );
        assert_eq!(&empty.rgba[0..3], &style.background_top);
        let frame = render_styled(&scene, &camera, 100, 100, &Highlight::default(), &style);
        assert_eq!(frame.pick(50.5, 50.5, 0.0), Some(Pick::Face(0)));
        // Shaded: the edges are not drawn, but they are still picked, and one under the
        // pointer shows.
        let with_edges = render(&scene, &camera, 100, 100, &Highlight::default());
        let shaded = Style {
            display: DisplayStyle::Shaded,
            ..Style::LIGHT
        };
        let frame = render_styled(&scene, &camera, 100, 100, &Highlight::default(), &shaded);
        assert_ne!(pixel(&frame, 25, 50), pixel(&with_edges, 25, 50));
        assert_eq!(pixel(&frame, 25, 50), pixel(&frame, 30, 50));
        assert_eq!(frame.edge_ids, with_edges.edge_ids);
        assert_eq!(frame.pick(27.0, 50.0, 3.0), Some(Pick::Edge(3)));
        let hovered = Highlight {
            hovered: Some(Pick::Edge(3)),
            ..Highlight::default()
        };
        let frame = render_styled(&scene, &camera, 100, 100, &hovered, &shaded);
        assert_eq!(pixel(&frame, 25, 50), Style::LIGHT.edge_hovered);
        assert_eq!(pixel(&frame, 50, 25), pixel(&frame, 50, 30));
    }

    fn styled(display: DisplayStyle) -> Style {
        Style {
            display,
            ..Style::LIGHT
        }
    }

    #[test]
    fn faces_are_picked_alike_in_every_display_style() {
        // Whatever is drawn, the face under the pointer is the same: the nearest one.
        let scene = Scene::from_mesh(&mesh());
        for view in [StandardView::Front, StandardView::Back] {
            let mut camera = front_camera();
            camera.set_view(view);
            let shaded = render(&scene, &camera, 100, 100, &Highlight::default());
            for (display, name) in DisplayStyle::ALL {
                let frame = render_styled(
                    &scene,
                    &camera,
                    100,
                    100,
                    &Highlight::default(),
                    &styled(display),
                );
                assert!(frame.face_ids == shaded.face_ids, "{name}, {view:?}");
            }
        }
    }

    #[test]
    fn hidden_lines_removed_draws_edges_on_the_background() {
        let scene = Scene::from_mesh(&mesh());
        let camera = front_camera();
        let style = styled(DisplayStyle::HiddenLinesRemoved);
        let empty = render_styled(
            &Scene::default(),
            &camera,
            100,
            100,
            &Highlight::default(),
            &style,
        );
        let frame = render_styled(&scene, &camera, 100, 100, &Highlight::default(), &style);
        // The faces leave the background; face 0's outline is drawn and picked.
        assert_eq!(pixel(&frame, 50, 50), pixel(&empty, 50, 50));
        assert_eq!(pixel(&frame, 5, 5), pixel(&empty, 5, 5));
        assert_eq!(pixel(&frame, 25, 50), style.edge_color());
        assert_eq!(frame.pick(27.0, 50.0, 3.0), Some(Pick::Edge(3)));
        assert_eq!(frame.pick(50.5, 50.5, 0.0), Some(Pick::Face(0)));
        // A face under the pointer shows in its highlight colour.
        let hovered = Highlight {
            hovered: Some(Pick::Face(0)),
            ..Highlight::default()
        };
        let frame = render_styled(&scene, &camera, 100, 100, &hovered, &style);
        assert_ne!(pixel(&frame, 50, 50), pixel(&empty, 50, 50));
        assert_eq!(pixel(&frame, 5, 5), pixel(&empty, 5, 5));
        // From the back the outline is behind face 1: neither drawn nor picked.
        let mut back = camera;
        back.set_view(StandardView::Back);
        let frame = render_styled(&scene, &back, 100, 100, &Highlight::default(), &style);
        assert!(frame.edge_ids.iter().all(|&e| e == 0));
        assert_eq!(frame.rgba, empty.rgba);
        // So is face 0 under the pointer: the unseen face in front still covers it.
        let frame = render_styled(&scene, &back, 100, 100, &hovered, &style);
        assert_eq!(frame.rgba, empty.rgba);
    }

    #[test]
    fn hidden_lines_visible_dashes_the_hidden_edges() {
        // From the back, face 0's outline is behind face 1: dashed, in the hidden-edge
        // colour, and still picked.
        let scene = Scene::from_mesh(&mesh());
        let mut camera = front_camera();
        camera.set_view(StandardView::Back);
        let style = styled(DisplayStyle::HiddenLinesVisible);
        let frame = render_styled(&scene, &camera, 100, 100, &Highlight::default(), &style);
        // The left edge (x = −10 mm) is at pixel column 75 seen from the back.
        let inked_at = |f: &Frame, y: usize, color: Rgb| (74..=76).any(|x| pixel(f, x, y) == color);
        let inked = (30..70)
            .filter(|&y| inked_at(&frame, y, style.hidden_edge_color()))
            .count();
        assert!(inked > 10 && inked < 35, "{inked} of 40 rows inked");
        assert_eq!(frame.pick(75.5, 50.5, 3.0), Some(Pick::Edge(3)));
        // From the front the same edges are in front of everything: solid, as in Hidden
        // Lines Removed.
        let front = front_camera();
        let removed = render_styled(
            &scene,
            &front,
            100,
            100,
            &Highlight::default(),
            &styled(DisplayStyle::HiddenLinesRemoved),
        );
        let visible = render_styled(&scene, &front, 100, 100, &Highlight::default(), &style);
        assert_eq!(visible.rgba, removed.rgba);
        assert_eq!(visible.edge_ids, removed.edge_ids);
    }

    #[test]
    fn wireframe_draws_every_edge_solid() {
        let scene = Scene::from_mesh(&mesh());
        let mut camera = front_camera();
        camera.set_view(StandardView::Back);
        let style = styled(DisplayStyle::Wireframe);
        let frame = render_styled(&scene, &camera, 100, 100, &Highlight::default(), &style);
        let color = style.edge_color();
        assert!((30..70).all(|y| (74..=76).any(|x| pixel(&frame, x, y) == color)));
        assert_eq!(frame.pick(75.5, 50.5, 3.0), Some(Pick::Edge(3)));
        // The nearest face is still the one picked away from the edges.
        assert_eq!(frame.pick(50.5, 50.5, 0.0), Some(Pick::Face(1)));
    }

    #[test]
    fn line_styles_show_no_reflection_or_occlusion() {
        let scene = Scene::from_mesh(&mesh());
        let camera = front_camera();
        let style = styled(DisplayStyle::HiddenLinesRemoved);
        let effects = Style {
            floor_reflection: 0.5,
            ambient_occlusion: 1.0,
            ..style
        };
        let plain = render_styled(&scene, &camera, 100, 100, &Highlight::default(), &style);
        let frame = render_styled(&scene, &camera, 100, 100, &Highlight::default(), &effects);
        assert_eq!(frame.rgba, plain.rgba);
    }

    #[test]
    fn gpu_segments_know_where_they_start_along_their_edge() {
        let scene = Scene::from_mesh(&Mesh {
            faces: Vec::new(),
            edges: vec![
                vec![[0.0, 0.0, 0.0], [3.0, 4.0, 0.0], [3.0, 10.0, 0.0]],
                vec![[1.0, 1.0, 1.0], [2.0, 1.0, 1.0]],
            ],
        });
        let mesh = scene.gpu_mesh();
        assert_eq!(mesh.segment_starts, vec![0.0, 5.0, 0.0]);
        assert_eq!(mesh.segments.len(), mesh.segment_starts.len());
    }
}
