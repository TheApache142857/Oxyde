//! The path tracer's scene and settings as flat 32-bit words for a GPU compute shader, so
//! the GPU traces exactly what the CPU would: the same hierarchy, triangles, looks,
//! camera, lights, floor and background, worked out here once and tested on the CPU.
//!
//! This crate knows nothing of wgpu: the application uploads these bytes and runs the
//! shader (`oxyde-app/src/pathtrace.wgsl`), whose structs mirror the layouts below. Every
//! struct is a whole number of 16-byte rows, as WGSL's storage layout wants.
//!
//! | WGSL struct | Bytes | Layout |
//! |---|---|---|
//! | `Node` | 32 | `lo: vec3<f32>`, `a: u32`, `hi: vec3<f32>`, `b: u32` — a leaf has `b` = count with [`LEAF_BIT`] set and `a` = first triangle; an inner node has `a`, `b` = its children |
//! | `Tri` | 64 | `v0: vec3<f32>`, `look: u32`, `e1: vec3<f32>`, `n0: u32`, `e2: vec3<f32>`, `n1: u32`, `n2: u32`, 3 × `u32` padding — normals octahedral, two snorm16 |
//! | `Look` | 32 | `base: vec3<f32>`, `opacity`, `metallic`, `roughness`, `transmission`, `ior` |
//! | `Params` | [`PARAMS_BYTES`] | see [`GpuFrame::params`] |

use super::camera::{Projection, RenderCamera};
use super::environment::{EnvironmentKind, Lighting};
use super::render::{Background, Floor, RenderSettings, Stage};
use super::scene::TraceScene;
use super::vector::{V, srgb8_to_linear};

/// Set in a node's `b` word when it is a leaf.
pub const LEAF_BIT: u32 = 0x8000_0000;
pub const NODE_BYTES: usize = 32;
pub const TRI_BYTES: usize = 64;
pub const LOOK_BYTES: usize = 32;
/// Lights the shader has room for.
pub const MAX_GPU_LIGHTS: usize = 4;
/// Bytes of the `Params` uniform: eleven 16-byte rows and the lights.
pub const PARAMS_BYTES: usize = 11 * 16 + MAX_GPU_LIGHTS * 32;

/// The scene's arrays, ready to upload as storage buffers.
#[derive(Debug, Clone, Default)]
pub struct GpuScene {
    pub nodes: Vec<u8>,
    pub tris: Vec<u8>,
    pub looks: Vec<u8>,
}

impl GpuScene {
    pub fn node_count(&self) -> usize {
        self.nodes.len() / NODE_BYTES
    }

    pub fn tri_count(&self) -> usize {
        self.tris.len() / TRI_BYTES
    }
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_v(out: &mut Vec<u8>, v: V) {
    push_f32(out, v.x);
    push_f32(out, v.y);
    push_f32(out, v.z);
}

/// A unit normal in 32 bits: octahedral mapping, two snorm16 (the low half x), which
/// WGSL's `unpack2x16snorm` reads back.
pub fn encode_normal(n: V) -> u32 {
    let l1 = n.x.abs() + n.y.abs() + n.z.abs();
    let (mut x, mut y) = if l1 > 0.0 {
        (n.x / l1, n.y / l1)
    } else {
        (0.0, 0.0)
    };
    if n.z < 0.0 {
        let (ox, oy) = (x, y);
        x = (1.0 - oy.abs()) * if ox >= 0.0 { 1.0 } else { -1.0 };
        y = (1.0 - ox.abs()) * if oy >= 0.0 { 1.0 } else { -1.0 };
    }
    let q = |v: f32| ((v.clamp(-1.0, 1.0) * 32767.0).round() as i16) as u16;
    u32::from(q(x)) | (u32::from(q(y)) << 16)
}

/// The inverse of [`encode_normal`], as the shader does it.
pub fn decode_normal(p: u32) -> V {
    let s = |bits: u32| ((bits & 0xffff) as u16 as i16 as f32 / 32767.0).max(-1.0);
    let (ex, ey) = (s(p), s(p >> 16));
    let mut v = V::new(ex, ey, 1.0 - ex.abs() - ey.abs());
    let t = (-v.z).max(0.0);
    v.x += if v.x >= 0.0 { -t } else { t };
    v.y += if v.y >= 0.0 { -t } else { t };
    v.normalized()
}

impl TraceScene {
    /// The hierarchy, triangles and looks as storage buffers for the GPU shader.
    pub fn gpu_scene(&self) -> GpuScene {
        let mut out = GpuScene::default();
        for (bounds, first, count, right) in self.bvh().flat() {
            push_v(&mut out.nodes, bounds.lo);
            push_u32(&mut out.nodes, first);
            push_v(&mut out.nodes, bounds.hi);
            push_u32(
                &mut out.nodes,
                if count > 0 { count | LEAF_BIT } else { right },
            );
        }
        for t in self.tris() {
            push_v(&mut out.tris, t.v0);
            push_u32(&mut out.tris, t.look);
            push_v(&mut out.tris, t.e1);
            push_u32(&mut out.tris, encode_normal(t.n[0]));
            push_v(&mut out.tris, t.e2);
            push_u32(&mut out.tris, encode_normal(t.n[1]));
            push_u32(&mut out.tris, encode_normal(t.n[2]));
            for _ in 0..3 {
                push_u32(&mut out.tris, 0);
            }
        }
        for l in self.looks() {
            push_v(&mut out.looks, V::from_array(l.base_color));
            push_f32(&mut out.looks, l.opacity);
            push_f32(&mut out.looks, l.metallic);
            push_f32(&mut out.looks, l.roughness);
            push_f32(&mut out.looks, l.transmission);
            push_f32(&mut out.looks, l.ior);
        }
        out
    }
}

/// One render's settings, worked out for the shader: everything in `Params` but the
/// sample and the band of rows, which change from dispatch to dispatch.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuFrame {
    pub width: u32,
    pub height: u32,
    pub samples: u32,
    rows: Vec<[u32; 4]>,
}

impl GpuFrame {
    /// The frame for `scene` seen through `camera` with `settings` (its size held to the
    /// pixel budget, as the CPU renderer does).
    pub fn new(scene: &TraceScene, camera: &RenderCamera, settings: &RenderSettings) -> GpuFrame {
        let stage = Stage::new(scene, camera, settings);
        let settings = &stage.settings;
        let lighting: &Lighting = &stage.lighting;
        let f = f32::to_bits;
        let row = |v: V, w: f32| [f(v.x), f(v.y), f(v.z), f(w)];
        let (kind, view) = match camera.projection {
            Projection::Perspective { tan_half_fov } => (0.0, tan_half_fov),
            Projection::Orthographic { height } => (1.0, height),
        };
        let exposure = 2f32.powf(settings.exposure.clamp(-20.0, 20.0));
        let env_kind = match settings.environment.kind {
            EnvironmentKind::Studio => 0,
            EnvironmentKind::Sky => 1,
            EnvironmentKind::Uniform => 2,
        };
        let (background_kind, background) = match settings.background {
            Background::Environment => (0, V::ZERO),
            Background::Color(c) => (
                1,
                srgb8_to_linear(c).map(super::render::inverse_tonemap) / exposure,
            ),
            Background::Transparent => (2, V::ZERO),
        };
        let (floor_kind, floor_y, floor_look) = match (&stage.floor, settings.floor) {
            (Some(fl), Floor::ShadowCatcher) => (1, fl.y, (V::ZERO, 1.0)),
            (Some(fl), Floor::Visible { .. }) => (
                2,
                fl.y,
                (V::from_array(fl.look.base_color), fl.look.roughness),
            ),
            _ => (0, 0.0, (V::ZERO, 1.0)),
        };
        let lights: Vec<_> = lighting.lights.iter().take(MAX_GPU_LIGHTS).collect();
        let mut rows = vec![
            row(camera.eye, kind),
            row(camera.forward, view),
            row(camera.right, stage.back),
            row(camera.up, stage.eps),
            // size: width, height, seed, sample (per dispatch).
            [
                settings.width as u32,
                settings.height as u32,
                (settings.seed as u32) ^ ((settings.seed >> 32) as u32),
                0,
            ],
            // band: first row, rows (per dispatch), rows of the sample in progress done,
            // samples complete (per develop pass).
            [0, 0, 0, 0],
            [env_kind, background_kind, floor_kind, lights.len() as u32],
            // limits: max bounces, output row stride in words (set by the application).
            [
                settings.max_bounces.clamp(1, 64),
                settings.width as u32,
                0,
                0,
            ],
            [f(floor_y), f(lighting.strength()), f(exposure), f(0.0)],
            row(background, 1.0),
            row(floor_look.0, floor_look.1),
        ];
        for k in 0..MAX_GPU_LIGHTS {
            match lights.get(k) {
                Some(l) => {
                    rows.push(row(l.dir, l.cos_max));
                    rows.push(row(l.radiance, 0.0));
                }
                None => {
                    rows.push([0; 4]);
                    rows.push([0; 4]);
                }
            }
        }
        GpuFrame {
            width: settings.width as u32,
            height: settings.height as u32,
            samples: settings.samples,
            rows,
        }
    }

    /// The `Params` uniform for one dispatch: `sample` the sample being added, rows
    /// `first_row` to `first_row + rows`; for the develop pass, `rows_done` rows of
    /// sample `samples_complete` are done; `stride` is the output's row length in words.
    pub fn params(
        &self,
        sample: u32,
        first_row: u32,
        rows: u32,
        rows_done: u32,
        samples_complete: u32,
        stride: u32,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(PARAMS_BYTES);
        for (i, r) in self.rows.iter().enumerate() {
            let r = match i {
                4 => [r[0], r[1], r[2], sample],
                5 => [first_row, rows, rows_done, samples_complete],
                7 => [r[0], stride, r[2], r[3]],
                _ => *r,
            };
            for w in r {
                push_u32(&mut out, w);
            }
        }
        out.resize(PARAMS_BYTES, 0);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::{Camera, StandardView};
    use crate::pathtrace::scene::{Look, SceneBuilder};
    use oxyde_kernel::{FaceMesh, Mesh};

    fn word(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    fn float(bytes: &[u8], at: usize) -> f32 {
        f32::from_bits(word(bytes, at))
    }

    fn cube_scene() -> TraceScene {
        // Two triangles per face of a 10 mm cube, enough for a hierarchy with depth.
        let mut faces = Vec::new();
        for (axis, sign) in [
            (0, 1.0f32),
            (0, -1.0),
            (1, 1.0),
            (1, -1.0),
            (2, 1.0),
            (2, -1.0),
        ] {
            let p = |u: f32, v: f32| {
                let mut c = [0.0f32; 3];
                c[axis] = 5.0 * sign;
                c[(axis + 1) % 3] = u;
                c[(axis + 2) % 3] = v;
                c
            };
            faces.push(FaceMesh {
                positions: vec![p(-5.0, -5.0), p(5.0, -5.0), p(5.0, 5.0), p(-5.0, 5.0)],
                triangles: vec![[0, 1, 2], [0, 2, 3]],
            });
        }
        let mut b = SceneBuilder::new();
        let red = b.add_look(Look::from_srgb([200, 10, 10], 1.0, 0.3, 0.0));
        b.add_mesh(
            &Mesh {
                faces,
                edges: Vec::new(),
            },
            &[red; 6],
        );
        b.build()
    }

    #[test]
    fn normals_survive_the_octahedral_round_trip() {
        for n in [
            V::new(0.0, 0.0, 1.0),
            V::new(0.0, 0.0, -1.0),
            V::new(1.0, 0.0, 0.0),
            V::new(-0.3, 0.8, -0.5).normalized(),
            V::new(0.577, -0.577, -0.577).normalized(),
        ] {
            let back = decode_normal(encode_normal(n));
            assert!((back - n).length() < 1e-3, "{n:?} -> {back:?}");
        }
    }

    #[test]
    fn scene_arrays_have_the_shader_layout() {
        let scene = cube_scene();
        let g = scene.gpu_scene();
        assert_eq!(g.tris.len(), 12 * TRI_BYTES);
        assert_eq!(g.looks.len(), LOOK_BYTES);
        assert_eq!(g.nodes.len() % NODE_BYTES, 0);
        // The look: linear red, metallic, its roughness.
        assert!(float(&g.looks, 0) > 0.5 && float(&g.looks, 4) < 0.01);
        assert_eq!(float(&g.looks, 16), 1.0);
        assert!((float(&g.looks, 20) - 0.3).abs() < 1e-6);
        // Walking the nodes as the shader does reaches every triangle once, and children
        // always come after their parent.
        let mut seen = vec![0u32; 12];
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let at = i * NODE_BYTES;
            let (a, b) = (word(&g.nodes, at + 12), word(&g.nodes, at + 28));
            if b & LEAF_BIT != 0 {
                for t in a..a + (b & !LEAF_BIT) {
                    seen[t as usize] += 1;
                    // Each triangle lies in its leaf's box.
                    let v0x = float(&g.tris, t as usize * TRI_BYTES);
                    assert!(v0x >= float(&g.nodes, at) - 1e-4);
                    assert!(v0x <= float(&g.nodes, at + 16) + 1e-4);
                }
            } else {
                assert!(a as usize > i && b as usize > i);
                stack.push(a as usize);
                stack.push(b as usize);
            }
        }
        assert!(seen.iter().all(|&s| s == 1), "{seen:?}");
    }

    #[test]
    fn params_carry_the_camera_lights_and_the_dispatch() {
        let scene = cube_scene();
        let mut camera = Camera::default();
        camera.set_view(StandardView::Front);
        camera.height = 30.0;
        let rc = RenderCamera::from_view(&camera, Some(50.0));
        let settings = RenderSettings {
            width: 300,
            height: 200,
            background: Background::Color([255, 255, 255]),
            ..RenderSettings::default()
        };
        let frame = GpuFrame::new(&scene, &rc, &settings);
        let p = frame.params(7, 16, 32, 5, 3, 320);
        assert_eq!(p.len(), PARAMS_BYTES);
        // eye.w = 0: perspective; forward.w = tan(fov / 2) = 12 / 50.
        assert_eq!(float(&p, 12), 0.0);
        assert!((float(&p, 28) - 0.24).abs() < 1e-6);
        assert!((float(&p, 8) - rc.eye.z).abs() < 1e-3);
        // size and band.
        assert_eq!([word(&p, 64), word(&p, 68), word(&p, 76)], [300, 200, 7]);
        assert_eq!(
            [word(&p, 80), word(&p, 84), word(&p, 88), word(&p, 92)],
            [16, 32, 5, 3]
        );
        // kinds: studio, a plain background, the shadow catcher, three lights.
        assert_eq!(
            [word(&p, 96), word(&p, 100), word(&p, 104), word(&p, 108)],
            [0, 1, 1, 3]
        );
        assert_eq!(word(&p, 116), 320);
        // The plain white background comes out white through the tone curve.
        let bg = float(&p, 144);
        assert!((crate::pathtrace::tonemap(bg * float(&p, 136)) - 0.999).abs() < 2e-3);
        // The first light is a unit direction with a cosine below 1.
        let d = V::new(float(&p, 176), float(&p, 180), float(&p, 184));
        assert!((d.length() - 1.0).abs() < 1e-5 && float(&p, 188) < 1.0);
        // A huge size is held to the pixel budget, as on the CPU.
        let big = GpuFrame::new(
            &scene,
            &rc,
            &RenderSettings {
                width: 20_000,
                height: 20_000,
                ..settings
            },
        );
        assert!((big.width as usize) * (big.height as usize) <= crate::pathtrace::MAX_PIXELS);
    }
}
