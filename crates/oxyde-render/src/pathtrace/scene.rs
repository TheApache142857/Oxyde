//! What the path tracer sees: triangles with smooth normals, each with a [`Look`], in a
//! [`Bvh`], built once per model and shared by every render of it (a camera move only
//! starts a new render of the same scene).

use std::sync::Arc;

use oxyde_kernel::Mesh;

use super::bvh::{Aabb, Bvh};
use super::vector::{V, srgb_to_linear};
use crate::material::TextureRef;

/// How a surface looks in a render: the metallic-roughness model of glTF and Blender's
/// Principled BSDF.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    /// Linear RGB, 0..1: the diffuse colour of a dielectric, the reflection colour of a
    /// metal, the tint of glass.
    pub base_color: [f32; 3],
    /// 1 = opaque. Less lets light through without bending it (a see-through
    /// appearance that is not glass).
    pub opacity: f32,
    /// 0 = dielectric, 1 = metal.
    pub metallic: f32,
    /// 0 = mirror-smooth, 1 = rough.
    pub roughness: f32,
    /// 0 = opaque, 1 = glass: light goes through and bends.
    pub transmission: f32,
    /// Index of refraction.
    pub ior: f32,
    /// Index into the scene's textures: a colour texture that replaces `base_color`,
    /// mapped along the model's axes as in the viewport.
    pub texture: Option<usize>,
    /// Millimetres covered by one repeat of the texture.
    pub texture_scale: f32,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            base_color: [0.45, 0.48, 0.53],
            opacity: 1.0,
            metallic: 0.0,
            roughness: 0.42,
            transmission: 0.0,
            ior: 1.5,
            texture: None,
            texture_scale: 50.0,
        }
    }
}

impl Look {
    /// A look from an 8-bit sRGB colour and finish parameters.
    pub fn from_srgb(color: [u8; 3], metallic: f32, roughness: f32, transmission: f32) -> Look {
        Look {
            base_color: color.map(|c| srgb_to_linear(f32::from(c))),
            metallic: metallic.clamp(0.0, 1.0),
            roughness: roughness.clamp(0.0, 1.0),
            transmission: transmission.clamp(0.0, 1.0),
            ..Look::default()
        }
    }

    pub(crate) fn is_glass(&self) -> bool {
        self.transmission >= 0.5
    }
}

/// One triangle: a corner and two edges (what the intersection test wants), the vertex
/// normals, and its look.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tri {
    pub v0: V,
    pub e1: V,
    pub e2: V,
    pub n: [V; 3],
    pub look: u32,
}

/// Where a ray met the scene.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Hit {
    pub t: f32,
    pub point: V,
    /// The triangle's own normal, unit, as wound.
    pub geometric: V,
    /// Interpolated from the vertex normals, unit, as wound.
    pub shading: V,
    pub look: u32,
}

/// Triangles collected from meshes, before the hierarchy is built.
#[derive(Default)]
pub struct SceneBuilder {
    tris: Vec<Tri>,
    looks: Vec<Look>,
    textures: Vec<TextureRef>,
}

impl SceneBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a look; returns its index for [`SceneBuilder::add_mesh`]. Equal looks are kept
    /// once.
    pub fn add_look(&mut self, look: Look) -> u32 {
        if let Some(i) = self.looks.iter().position(|l| *l == look) {
            return i as u32;
        }
        self.looks.push(look);
        (self.looks.len() - 1) as u32
    }

    /// Adds a texture for [`Look::texture`]; returns its index. The same image (by
    /// pointer) is kept once.
    pub fn add_texture(&mut self, texture: TextureRef) -> usize {
        if let Some(i) = self.textures.iter().position(|t| Arc::ptr_eq(t, &texture)) {
            return i;
        }
        self.textures.push(texture);
        self.textures.len() - 1
    }

    /// Adds the faces of `mesh`, face `k` with look `face_looks[k]` (the last look, or a
    /// default one, where the list is short). Normals are smoothed within each face, as
    /// in the viewport, so curved faces look round and edges stay sharp.
    pub fn add_mesh(&mut self, mesh: &Mesh, face_looks: &[u32]) {
        if self.looks.is_empty() {
            self.looks.push(Look::default());
        }
        let last = (self.looks.len() - 1) as u32;
        for (k, face) in mesh.faces.iter().enumerate() {
            let look = face_looks
                .get(k)
                .copied()
                .filter(|&l| (l as usize) < self.looks.len())
                .unwrap_or(last);
            let positions: Vec<V> = face.positions.iter().map(|p| V::from_array(*p)).collect();
            let mut normals = vec![V::ZERO; positions.len()];
            let corner = |i: u32| positions.get(i as usize).copied();
            for t in &face.triangles {
                let (Some(a), Some(b), Some(c)) = (corner(t[0]), corner(t[1]), corner(t[2])) else {
                    continue;
                };
                // Unnormalised: larger triangles weigh more.
                let n = (b - a).cross(c - a);
                for &i in t {
                    if let Some(slot) = normals.get_mut(i as usize) {
                        *slot += n;
                    }
                }
            }
            for t in &face.triangles {
                let (Some(a), Some(b), Some(c)) = (corner(t[0]), corner(t[1]), corner(t[2])) else {
                    continue;
                };
                let (e1, e2) = (b - a, c - a);
                let geometric = e1.cross(e2);
                if !geometric.is_finite() || geometric.length() <= 0.0 {
                    continue;
                }
                let geometric = geometric.normalized();
                let normal = |i: u32| {
                    normals
                        .get(i as usize)
                        .map_or(geometric, |n| n.normalized_or(geometric))
                };
                self.tris.push(Tri {
                    v0: a,
                    e1,
                    e2,
                    n: [normal(t[0]), normal(t[1]), normal(t[2])],
                    look,
                });
            }
        }
    }

    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    /// Builds the hierarchy: the scene is ready to render.
    pub fn build(self) -> TraceScene {
        let SceneBuilder {
            tris,
            mut looks,
            textures,
        } = self;
        if looks.is_empty() {
            looks.push(Look::default());
        }
        let bounds: Vec<Aabb> = tris
            .iter()
            .map(|t| Aabb::EMPTY.grow(t.v0).grow(t.v0 + t.e1).grow(t.v0 + t.e2))
            .collect();
        let (bvh, order) = Bvh::build(&bounds);
        let tris = order
            .iter()
            .filter_map(|&i| tris.get(i as usize).copied())
            .collect();
        for look in &mut looks {
            if look.texture.is_some_and(|t| t >= textures.len()) {
                look.texture = None;
            }
        }
        TraceScene {
            tris,
            bvh,
            looks,
            textures,
        }
    }
}

/// The scene, ready to trace. Immutable and shared between the render threads.
pub struct TraceScene {
    tris: Vec<Tri>,
    bvh: Bvh,
    looks: Vec<Look>,
    textures: Vec<TextureRef>,
}

impl TraceScene {
    /// The box around every triangle, or `None` for an empty scene.
    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        let b = self.bvh.bounds();
        (!b.is_empty() && !self.tris.is_empty()).then(|| {
            (
                [f64::from(b.lo.x), f64::from(b.lo.y), f64::from(b.lo.z)],
                [f64::from(b.hi.x), f64::from(b.hi.y), f64::from(b.hi.z)],
            )
        })
    }

    pub(crate) fn aabb(&self) -> Aabb {
        self.bvh.bounds()
    }

    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    pub(crate) fn look(&self, index: u32) -> &Look {
        static DEFAULT: std::sync::OnceLock<Look> = std::sync::OnceLock::new();
        self.looks
            .get(index as usize)
            .unwrap_or_else(|| DEFAULT.get_or_init(Look::default))
    }

    /// The base colour of `look` at `point` with normal `n`: its texture's, mapped along
    /// the axes, where it has one.
    pub(crate) fn base_color(&self, look: &Look, point: V, n: V) -> V {
        if let Some(texture) = look.texture.and_then(|t| self.textures.get(t)) {
            let srgb = texture.triplanar(point.to_array(), n.to_array(), look.texture_scale);
            return V::new(
                srgb_to_linear(srgb[0]),
                srgb_to_linear(srgb[1]),
                srgb_to_linear(srgb[2]),
            );
        }
        V::from_array(look.base_color)
    }

    /// The nearest triangle the ray meets between `t_min` and `t_max`.
    pub(crate) fn intersect(&self, origin: V, dir: V, t_min: f32, t_max: f32) -> Option<Hit> {
        let mut best: Option<(u32, f32, f32, f32)> = None;
        self.bvh
            .traverse(origin, dir, t_max, |first, count, t_max| {
                for k in first..first + count {
                    if let Some(tri) = self.tris.get(k as usize)
                        && let Some((t, u, v)) = intersect_tri(tri, origin, dir)
                        && t > t_min
                        && t < *t_max
                    {
                        *t_max = t;
                        best = Some((k, t, u, v));
                    }
                }
                true
            });
        let (k, t, u, v) = best?;
        let tri = self.tris.get(k as usize)?;
        let geometric = tri.e1.cross(tri.e2).normalized();
        let shading =
            (tri.n[0] * (1.0 - u - v) + tri.n[1] * u + tri.n[2] * v).normalized_or(geometric);
        Some(Hit {
            t,
            point: origin + dir * t,
            geometric,
            shading,
            look: tri.look,
        })
    }

    /// How much light gets from `origin` along `dir` to `t_max` unblocked: 0 behind an
    /// opaque surface, a tint through glass and see-through looks (no refraction, which a
    /// shadow ray cannot follow), 1 in the open.
    pub(crate) fn transmittance(&self, origin: V, dir: V, t_min: f32, t_max: f32) -> V {
        let mut through = V::ONE;
        self.bvh.traverse(origin, dir, t_max, |first, count, _| {
            for k in first..first + count {
                let Some(tri) = self.tris.get(k as usize) else {
                    continue;
                };
                let Some((t, _, _)) = intersect_tri(tri, origin, dir) else {
                    continue;
                };
                if t <= t_min || t >= t_max {
                    continue;
                }
                let look = self.look(tri.look);
                if look.is_glass() {
                    // Each surface crossed tints once: a solid is crossed twice.
                    through *= V::from_array(look.base_color).map(f32::sqrt) * 0.96;
                } else if look.opacity < 0.999 {
                    through *= 1.0 - look.opacity;
                } else {
                    through = V::ZERO;
                }
                if through.max_component() <= 1e-4 {
                    through = V::ZERO;
                    return false;
                }
            }
            true
        });
        through
    }
}

/// Möller–Trumbore: distance and barycentric coordinates where the ray crosses the
/// triangle, either side.
fn intersect_tri(tri: &Tri, origin: V, dir: V) -> Option<(f32, f32, f32)> {
    let p = dir.cross(tri.e2);
    let det = tri.e1.dot(p);
    if det.abs() < 1e-14 {
        return None;
    }
    let inv = 1.0 / det;
    let s = origin - tri.v0;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(tri.e1);
    let v = dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    Some((tri.e2.dot(q) * inv, u, v))
}
