use oxyde_kernel::Mesh;

use crate::camera::Camera;
use crate::vec3::{V3, add, cross, dot, lerp, normalize, scale, sub};

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
}

struct SceneFace {
    positions: Vec<V3>,
    /// Smooth within the face, so curved faces look round while edges stay crisp.
    normals: Vec<V3>,
    triangles: Vec<[usize; 3]>,
}

/// A display mesh prepared for rendering.
#[derive(Default)]
pub struct Scene {
    faces: Vec<SceneFace>,
    edges: Vec<Vec<V3>>,
    bounds: Option<(V3, V3)>,
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

            faces.push(SceneFace {
                positions,
                normals,
                triangles,
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
        }
    }

    /// Axis-aligned bounding box, or `None` for an empty scene.
    pub fn bounds(&self) -> Option<(V3, V3)> {
        self.bounds
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty() && self.edges.is_empty()
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

    fn index(&self, x: i64, y: i64) -> Option<usize> {
        (x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height)
            .then(|| y as usize * self.width + x as usize)
    }

    fn put(&mut self, index: usize, color: [u8; 3]) {
        self.rgba[index * 4..index * 4 + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
    }
}

const BACKGROUND_TOP: [f32; 3] = [205.0, 214.0, 228.0];
const BACKGROUND_BOTTOM: [f32; 3] = [246.0, 247.0, 249.0];
const FACE: [f32; 3] = [182.0, 190.0, 202.0];
const FACE_SELECTED: [f32; 3] = [96.0, 160.0, 255.0];
const FACE_HOVERED: [f32; 3] = [255.0, 178.0, 92.0];
const EDGE: [u8; 3] = [32, 36, 44];
const EDGE_SELECTED: [u8; 3] = [0, 110, 255];
const EDGE_HOVERED: [u8; 3] = [255, 128, 0];

fn face_color(face: u32, highlight: &Highlight) -> [f32; 3] {
    if highlight.hovered == Some(Pick::Face(face)) {
        FACE_HOVERED
    } else if highlight.selected.contains(&Pick::Face(face)) {
        FACE_SELECTED
    } else {
        FACE
    }
}

fn edge_style(edge: u32, highlight: &Highlight) -> ([u8; 3], i64) {
    if highlight.hovered == Some(Pick::Edge(edge)) {
        (EDGE_HOVERED, 3)
    } else if highlight.selected.contains(&Pick::Edge(edge)) {
        (EDGE_SELECTED, 3)
    } else {
        (EDGE, 1)
    }
}

/// Renders `scene` seen through `camera` into a `width` × `height` pixel frame.
pub fn render(
    scene: &Scene,
    camera: &Camera,
    width: usize,
    height: usize,
    highlight: &Highlight,
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

    for y in 0..height {
        let t = y as f32 / (height.max(2) - 1) as f32;
        let c: [u8; 3] = std::array::from_fn(|k| {
            (BACKGROUND_TOP[k] + (BACKGROUND_BOTTOM[k] - BACKGROUND_TOP[k]) * t) as u8
        });
        for x in 0..width {
            frame.put(y * width + x, c);
        }
    }

    let (w, h) = (width as f64, height as f64);
    let view = camera.eye_direction();

    for (index, face) in scene.faces.iter().enumerate() {
        let base = face_color(index as u32, highlight);
        let projected: Vec<V3> = face
            .positions
            .iter()
            .map(|&p| camera.project(p, w, h))
            .collect();
        for t in &face.triangles {
            raster_triangle(
                &mut frame,
                t.map(|i| projected[i]),
                t.map(|i| face.normals[i]),
                view,
                base,
                index as u32 + 1,
            );
        }
    }

    // Edges are drawn over faces they lie on, but hidden behind faces in front of them.
    let bias = (camera.height * 0.004) as f32;
    for (index, edge) in scene.edges.iter().enumerate() {
        let (color, thickness) = edge_style(index as u32, highlight);
        let points: Vec<V3> = edge.iter().map(|&p| camera.project(p, w, h)).collect();
        for segment in points.windows(2) {
            draw_line(
                &mut frame,
                segment[0],
                segment[1],
                color,
                thickness,
                bias,
                index as u32 + 1,
            );
        }
    }
    frame
}

/// Twice the signed area of the triangle (a, b, (x, y)).
fn edge_fn(a: V3, b: V3, x: f64, y: f64) -> f64 {
    (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0])
}

fn raster_triangle(
    frame: &mut Frame,
    p: [V3; 3],
    normals: [V3; 3],
    view: V3,
    base: [f32; 3],
    id: u32,
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
            frame.depth[index] = z;
            frame.face_ids[index] = id;

            let n = add(
                add(scale(normals[0], w0), scale(normals[1], w1)),
                scale(normals[2], w2),
            );
            let facing = normalize(n).map_or(1.0, |n| dot(n, view).abs()) as f32;
            let light = 0.32 + 0.68 * facing;
            frame.put(index, base.map(|c| (c * light).min(255.0) as u8));
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

fn draw_line(frame: &mut Frame, a: V3, b: V3, color: [u8; 3], thickness: i64, bias: f32, id: u32) {
    if a.iter().chain(b.iter()).any(|v| !v.is_finite()) {
        return;
    }
    let margin = 2.0;
    let Some((a, b)) = clip(
        a,
        b,
        -margin,
        frame.width as f64 + margin,
        -margin,
        frame.height as f64 + margin,
    ) else {
        return;
    };
    let steps = (b[0] - a[0]).abs().max((b[1] - a[1]).abs()).ceil().max(1.0) as usize;
    let offset = thickness / 2;
    for i in 0..=steps {
        let q = lerp(a, b, i as f64 / steps as f64);
        let (cx, cy) = (q[0].floor() as i64, q[1].floor() as i64);
        let z = q[2] as f32 - bias;
        for oy in 0..thickness {
            for ox in 0..thickness {
                if let Some(index) = frame.index(cx + ox - offset, cy + oy - offset) {
                    if z <= frame.depth[index] {
                        frame.put(index, color);
                        frame.edge_ids[index] = id;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StandardView;
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
    fn highlighted_faces_change_colour() {
        let scene = Scene::from_mesh(&mesh());
        let plain = render(&scene, &front_camera(), 100, 100, &Highlight::default());
        let selected = render(
            &scene,
            &front_camera(),
            100,
            100,
            &Highlight {
                hovered: None,
                selected: vec![Pick::Face(0)],
            },
        );
        let at = (50 * 100 + 50) * 4;
        assert_ne!(plain.rgba[at..at + 3], selected.rgba[at..at + 3]);
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
}
