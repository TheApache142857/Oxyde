//! The progressive renderer: one sample per pixel per pass, on every core, accumulated
//! into a floating-point buffer that can be looked at (tonemapped, optionally denoised)
//! after any pass. A unidirectional path tracer with next-event estimation towards the
//! environment's lights and multiple importance sampling, Russian roulette after three
//! bounces, a floor (a shadow catcher or a visible one) and a background.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::bsdf::{Surface, sample_glass};
use super::camera::RenderCamera;
use super::environment::{Environment, EnvironmentKind, Lighting};
use super::sampling::{Rng, cone_pdf, cosine_hemisphere, power_heuristic, uniform_cone};
use super::scene::{Look, TraceScene};
use super::vector::{V, linear_to_srgb, srgb8_to_linear};

/// What is seen behind the model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Background {
    /// The environment itself.
    Environment,
    /// A plain sRGB colour, exactly that colour in the picture.
    Color([u8; 3]),
    /// Nothing: transparent pixels in the PNG (the shadow on a shadow-catcher floor
    /// stays, as a translucent black).
    Transparent,
}

/// What the model stands on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Floor {
    None,
    /// An invisible floor that only shows the shadows the model casts on it, over the
    /// background: the usual product shot.
    ShadowCatcher,
    /// A visible floor of this sRGB colour; `gloss` from 0 (matte) to 1 (mirror).
    Visible {
        color: [u8; 3],
        gloss: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderSettings {
    pub width: usize,
    pub height: usize,
    /// Samples per pixel to reach.
    pub samples: u32,
    /// Longest path, in bounces.
    pub max_bounces: u32,
    pub environment: Environment,
    pub background: Background,
    pub floor: Floor,
    /// Exposure in stops: +1 doubles the light.
    pub exposure: f32,
    pub seed: u64,
    /// Threads to render with; 0 = one per core.
    pub threads: usize,
}

impl Default for RenderSettings {
    fn default() -> Self {
        RenderSettings {
            width: 640,
            height: 480,
            samples: 128,
            max_bounces: 8,
            environment: Environment::default(),
            background: Background::Environment,
            floor: Floor::ShadowCatcher,
            exposure: 0.0,
            seed: 0,
            threads: 0,
        }
    }
}

/// A finished picture: RGBA, 8 bits per channel, rows from the top, alpha not
/// premultiplied.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// What one pixel has gathered: sums over its samples.
#[derive(Debug, Clone, Copy, Default)]
struct Pixel {
    /// Premultiplied by coverage.
    color: V,
    alpha: f32,
    /// What the first surface hit looks like, for the denoiser.
    albedo: V,
    normal: V,
    depth: f32,
}

/// What one camera path brings back.
#[derive(Debug, Clone, Copy)]
struct PathResult {
    color: V,
    alpha: f32,
    albedo: V,
    normal: V,
    depth: f32,
}

/// The floor, placed under the scene.
#[derive(Debug, Clone)]
struct FloorPlane {
    y: f32,
    catcher: bool,
    look: Look,
}

/// A render in progress.
pub struct Renderer {
    scene: Arc<TraceScene>,
    camera: RenderCamera,
    settings: RenderSettings,
    lighting: Lighting,
    floor: Option<FloorPlane>,
    /// Offset of new rays from the surface they leave, mm.
    eps: f32,
    /// How far behind the centre of the view orthographic rays start, mm.
    back: f32,
    pixels: Vec<Pixel>,
    samples: u32,
}

/// Paths that pass through see-through surfaces stop after this many crossings.
const MAX_CROSSINGS: u32 = 64;

/// The most pixels a render may have: 8 megapixels (a 4K UHD frame). The renderer keeps
/// 44 bytes per pixel and the denoiser about as much again while it runs, so this holds
/// a render under about 0.8 GB; a bigger size asked for is scaled down to fit.
pub const MAX_PIXELS: usize = 8_388_608;

/// `width` × `height` scaled down, keeping its shape, to at most [`MAX_PIXELS`].
pub fn fit_pixel_budget(width: usize, height: usize) -> (usize, usize) {
    let (w, h) = (width.max(1), height.max(1));
    if w.saturating_mul(h) <= MAX_PIXELS {
        return (w, h);
    }
    let k = (MAX_PIXELS as f64 / (w as f64 * h as f64)).sqrt();
    (
        ((w as f64 * k).floor() as usize).max(1),
        ((h as f64 * k).floor() as usize).max(1),
    )
}

impl Renderer {
    pub fn new(scene: Arc<TraceScene>, camera: RenderCamera, settings: RenderSettings) -> Self {
        let mut settings = settings;
        (settings.width, settings.height) = fit_pixel_budget(settings.width, settings.height);
        let bounds = scene.aabb();
        let (centre, diagonal) = if bounds.is_empty() {
            (camera.eye, 100.0)
        } else {
            (bounds.centre(), (bounds.hi - bounds.lo).length().max(1e-3))
        };
        let eps = (diagonal * 2e-5).max(1e-5);
        let back = (centre - camera.eye).length() + diagonal * 2.0;
        let lighting = Lighting::new(&settings.environment, camera.right, -camera.forward);
        let floor = match settings.floor {
            Floor::None => None,
            _ if bounds.is_empty() => None,
            Floor::ShadowCatcher => Some(FloorPlane {
                y: bounds.lo.y - diagonal * 1e-4,
                catcher: true,
                look: Look::default(),
            }),
            Floor::Visible { color, gloss } => {
                let gloss = gloss.clamp(0.0, 1.0);
                Some(FloorPlane {
                    y: bounds.lo.y - diagonal * 1e-4,
                    catcher: false,
                    look: Look::from_srgb(color, 0.0, 0.9 - 0.88 * gloss, 0.0),
                })
            }
        };
        Renderer {
            pixels: vec![Pixel::default(); settings.width * settings.height],
            scene,
            camera,
            settings,
            lighting,
            floor,
            eps,
            back,
            samples: 0,
        }
    }

    pub fn settings(&self) -> &RenderSettings {
        &self.settings
    }

    /// Samples per pixel so far.
    pub fn samples(&self) -> u32 {
        self.samples
    }

    pub fn is_done(&self) -> bool {
        self.samples >= self.settings.samples
    }

    /// Adds one sample to every pixel, on every core. Returns false (and leaves the
    /// buffer half done: the render should be dropped) when `cancel` was raised.
    pub fn render_pass(&mut self, cancel: &AtomicBool) -> bool {
        let width = self.settings.width;
        let threads = match self.settings.threads {
            0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
            n => n,
        }
        .clamp(1, 256);
        let sample = self.samples;
        // The buffer is taken out so the threads can write their rows into it in place
        // while reading the rest of the renderer: no second full-size buffer per pass.
        let mut pixels = std::mem::take(&mut self.pixels);
        let this = &*self;
        // Rows are dealt out in turn, so a band of sky and a band of model share the
        // work evenly; every sample's random numbers depend on its pixel only.
        let mut shares: Vec<Vec<(usize, &mut [Pixel])>> =
            (0..threads).map(|_| Vec::new()).collect();
        for (y, row) in pixels.chunks_mut(width).enumerate() {
            if let Some(share) = shares.get_mut(y % threads) {
                share.push((y, row));
            }
        }
        std::thread::scope(|scope| {
            for share in shares {
                scope.spawn(move || {
                    for (y, row) in share {
                        if cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        for (x, d) in row.iter_mut().enumerate() {
                            let s = this.sample_pixel(x, y, sample);
                            d.color += s.color;
                            d.alpha += s.alpha;
                            d.albedo += s.albedo;
                            d.normal += s.normal;
                            d.depth += s.depth;
                        }
                    }
                });
            }
        });
        self.pixels = pixels;
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        self.samples += 1;
        true
    }

    /// Renders every remaining sample.
    pub fn render_all(&mut self, cancel: &AtomicBool) {
        while !self.is_done() && self.render_pass(cancel) {}
    }

    fn sample_pixel(&self, x: usize, y: usize, sample: u32) -> Pixel {
        let (w, h) = (self.settings.width, self.settings.height);
        let mut rng = Rng::for_sample(self.settings.seed, (y * w + x) as u64, sample);
        let (jx, jy) = rng.next2();
        let (origin, dir) =
            self.camera
                .ray(x as f32 + jx, y as f32 + jy, w as f32, h as f32, self.back);
        let r = self.trace(origin, dir, &mut rng);
        let ok = r.color.is_finite() && r.alpha.is_finite();
        Pixel {
            // A rare NaN from a degenerate triangle would ruin the pixel for good.
            color: if ok { r.color } else { V::ZERO },
            alpha: if ok { r.alpha } else { 0.0 },
            albedo: r.albedo,
            normal: r.normal,
            depth: if r.depth.is_finite() { r.depth } else { 0.0 },
        }
    }

    /// The exposure as a factor.
    fn exposure(&self) -> f32 {
        2f32.powf(self.settings.exposure.clamp(-20.0, 20.0))
    }

    /// What a camera ray sees when it leaves the scene, in linear light before exposure,
    /// and its alpha.
    fn backdrop(&self, dir: V) -> (V, f32) {
        match self.settings.background {
            Background::Environment => {
                let d = if self.settings.environment.kind == EnvironmentKind::Studio {
                    // A seamless studio sweep: no dark floor behind the model.
                    V::new(dir.x, dir.y.max(0.0), dir.z).normalized()
                } else {
                    dir
                };
                (self.lighting.background(d), 1.0)
            }
            Background::Color(c) => {
                // Chosen so that the tone curve gives the colour back exactly.
                let target = srgb8_to_linear(c);
                (target.map(inverse_tonemap) / self.exposure(), 1.0)
            }
            Background::Transparent => (V::ZERO, 0.0),
        }
    }

    fn floor_hit(&self, origin: V, dir: V) -> Option<f32> {
        let floor = self.floor.as_ref()?;
        if dir.y >= -1e-9 || origin.y <= floor.y {
            return None;
        }
        let t = (floor.y - origin.y) / dir.y;
        (t > 0.0 && t.is_finite()).then_some(t)
    }

    fn trace(&self, mut origin: V, mut dir: V, rng: &mut Rng) -> PathResult {
        let mut throughput = V::ONE;
        let mut radiance = V::ZERO;
        let mut first: Option<(V, V, f32)> = None;
        let mut prev_pdf = 0.0f32;
        let mut prev_delta = true;
        let mut bounce = 0u32;
        let mut crossings = 0u32;
        let light_count = self.lighting.lights.len();
        loop {
            let hit = self.scene.intersect(origin, dir, 0.0, f32::INFINITY);
            let floor_t = self
                .floor_hit(origin, dir)
                .filter(|t| hit.is_none_or(|h| *t < h.t));
            // ---- Into the environment -------------------------------------------------
            if hit.is_none() && floor_t.is_none() {
                if bounce == 0 && crossings == 0 {
                    let (color, alpha) = self.backdrop(dir);
                    return PathResult {
                        color: color * alpha,
                        alpha,
                        albedo: color,
                        normal: V::ZERO,
                        depth: 0.0,
                    };
                }
                radiance += throughput * self.lighting.background(dir);
                if let Some((i, l)) = self.lighting.light_in(dir) {
                    // A light found by scattering: weighed against the chance that
                    // aiming at the lights would have found it, unless nothing could
                    // have aimed (a mirror, glass, the camera).
                    let weight = if prev_delta || light_count == 0 {
                        1.0
                    } else {
                        let light_pdf = self
                            .lighting
                            .lights
                            .get(i)
                            .map_or(0.0, |li| cone_pdf(li.cos_max))
                            / light_count as f32;
                        power_heuristic(prev_pdf, light_pdf)
                    };
                    radiance += throughput * l * weight;
                }
                break;
            }
            // ---- The floor ------------------------------------------------------------
            let (point, geometric, shading, look) = if let Some(t) = floor_t {
                let point = origin + dir * t;
                let Some(floor) = self.floor.as_ref() else {
                    break;
                };
                if floor.catcher {
                    if bounce == 0 && crossings == 0 {
                        let ratio = self.shadow_ratio(point, rng);
                        let (bg, alpha) = self.backdrop(dir);
                        let (color, alpha) = if alpha > 0.0 {
                            (bg * ratio, 1.0)
                        } else {
                            (V::ZERO, 1.0 - ratio)
                        };
                        return PathResult {
                            color,
                            alpha,
                            albedo: bg,
                            normal: V::Y,
                            depth: t,
                        };
                    }
                    // Seen from the model, the catcher is not there.
                    origin = point + dir * self.eps;
                    crossings += 1;
                    if crossings > MAX_CROSSINGS {
                        break;
                    }
                    continue;
                }
                (point, V::Y, V::Y, &floor.look)
            } else if let Some(h) = hit {
                (h.point, h.geometric, h.shading, self.scene.look(h.look))
            } else {
                break;
            };
            let wo = -dir;
            let base = if floor_t.is_some() {
                V::from_array(look.base_color)
            } else {
                self.scene.base_color(look, point, shading)
            };
            if first.is_none() {
                let t = (point - origin).length();
                first = Some((
                    base,
                    if shading.dot(wo) < 0.0 {
                        -shading
                    } else {
                        shading
                    },
                    t,
                ));
            }
            // ---- See-through looks: straight on ---------------------------------------
            if !look.is_glass() && look.opacity < 0.999 && rng.next() >= look.opacity {
                origin = point + dir * self.eps;
                crossings += 1;
                if crossings > MAX_CROSSINGS {
                    break;
                }
                continue;
            }
            // ---- Glass -----------------------------------------------------------------
            if look.is_glass() {
                let s = sample_glass(geometric, wo, look.ior, base.map(f32::sqrt), rng);
                throughput *= s.weight;
                origin = point + s.wi * self.eps;
                dir = s.wi;
                prev_delta = true;
                bounce += 1;
                if bounce > self.settings.max_bounces + 8 {
                    break;
                }
                continue;
            }
            // ---- Opaque: light it, then scatter -----------------------------------------
            let ng = if geometric.dot(wo) < 0.0 {
                -geometric
            } else {
                geometric
            };
            let mut n = if shading.dot(ng) < 0.0 {
                -shading
            } else {
                shading
            };
            if n.dot(wo) <= 1e-4 {
                n = ng;
            }
            let surface = Surface::new(base, look.metallic, look.roughness, look.ior);
            let lift = point + ng * self.eps;
            if light_count > 0 {
                let pick = ((rng.next() * light_count as f32) as usize).min(light_count - 1);
                let u = rng.next2();
                if let Some(light) = self.lighting.lights.get(pick) {
                    let wi = uniform_cone(light.dir, light.cos_max, u);
                    if wi.dot(ng) > 0.0 {
                        let (f, bsdf_pdf) = surface.eval(n, wo, wi);
                        if f.max_component() > 0.0 {
                            let through = self.scene.transmittance(lift, wi, 0.0, f32::INFINITY);
                            if through.max_component() > 0.0 {
                                let light_pdf = cone_pdf(light.cos_max) / light_count as f32;
                                let weight = power_heuristic(light_pdf, bsdf_pdf);
                                radiance += throughput
                                    * f
                                    * light.radiance
                                    * through
                                    * (weight / light_pdf);
                            }
                        }
                    }
                }
            }
            let Some(s) = surface.sample(n, wo, rng) else {
                break;
            };
            if s.wi.dot(ng) <= 0.0 {
                break;
            }
            throughput *= s.weight;
            prev_pdf = s.pdf;
            prev_delta = s.delta;
            origin = lift;
            dir = s.wi;
            bounce += 1;
            if bounce > self.settings.max_bounces {
                break;
            }
            if bounce > 3 {
                let p = throughput.max_component().clamp(0.05, 0.95);
                if rng.next() >= p {
                    break;
                }
                throughput *= 1.0 / p;
            }
        }
        let (albedo, normal, depth) = first.unwrap_or((V::ZERO, V::ZERO, 0.0));
        PathResult {
            color: radiance,
            alpha: 1.0,
            albedo,
            normal,
            depth,
        }
    }

    /// For a point on a shadow-catcher floor: the share of the light it would get that
    /// the model does not block, from one light sample and one sample of the sky.
    fn shadow_ratio(&self, point: V, rng: &mut Rng) -> f32 {
        let lift = point + V::Y * self.eps;
        let mut open = 0.0;
        let mut total = 0.0;
        let count = self.lighting.lights.len();
        if count > 0 {
            let pick = ((rng.next() * count as f32) as usize).min(count - 1);
            if let Some(light) = self.lighting.lights.get(pick) {
                let wi = uniform_cone(light.dir, light.cos_max, rng.next2());
                if wi.y > 0.0 {
                    let c =
                        light.radiance.luminance() * wi.y * count as f32 / cone_pdf(light.cos_max);
                    total += c;
                    open += c * self
                        .scene
                        .transmittance(lift, wi, 0.0, f32::INFINITY)
                        .luminance();
                }
            }
        }
        let wi = cosine_hemisphere(V::Y, rng.next2());
        let c = self.lighting.background(wi).luminance() * std::f32::consts::PI;
        total += c;
        open += c * self
            .scene
            .transmittance(lift, wi, 0.0, f32::INFINITY)
            .luminance();
        if total > 0.0 {
            (open / total).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// The mean of every pixel so far: linear colour (not premultiplied) and alpha.
    pub fn linear(&self) -> Vec<([f32; 3], f32)> {
        let k = 1.0 / self.samples.max(1) as f32;
        self.pixels
            .iter()
            .map(|p| {
                let alpha = p.alpha * k;
                let color = if alpha > 1e-6 {
                    p.color * (k / alpha)
                } else {
                    V::ZERO
                };
                (color.to_array(), alpha)
            })
            .collect()
    }

    /// The picture so far, tonemapped for the screen; `denoise` smooths the noise first,
    /// guided by the surfaces' colours, normals and depths so edges and textures stay.
    pub fn image(&self, denoise: bool) -> RenderImage {
        let (w, h) = (self.settings.width, self.settings.height);
        let k = 1.0 / self.samples.max(1) as f32;
        let mut color: Vec<V> = self
            .pixels
            .iter()
            .map(|p| {
                let alpha = p.alpha * k;
                if alpha > 1e-6 {
                    p.color * (k / alpha)
                } else {
                    V::ZERO
                }
            })
            .collect();
        if denoise && self.samples > 0 {
            let pixels = &self.pixels;
            let feature = |i: usize| {
                pixels.get(i).map_or((V::ZERO, V::ZERO, 0.0), |p| {
                    (p.albedo * k, p.normal * k, p.depth * k)
                })
            };
            color = super::denoise::denoise(&color, &feature, w, h, self.settings.threads);
        }
        let exposure = self.exposure();
        let mut rgba = Vec::with_capacity(w * h * 4);
        for (c, p) in color.iter().zip(&self.pixels) {
            let mapped = (*c * exposure).map(tonemap);
            for v in mapped.to_array() {
                rgba.push((linear_to_srgb(v) * 255.0 + 0.5).clamp(0.0, 255.0) as u8);
            }
            rgba.push(((p.alpha * k).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
        RenderImage {
            width: w,
            height: h,
            rgba,
        }
    }
}

/// The filmic tone curve (Narkowicz's fit of ACES): bright light rolls off instead of
/// clipping, as a camera does.
pub fn tonemap(x: f32) -> f32 {
    let x = x.max(0.0);
    ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0)
}

/// The light that [`tonemap`] turns into `y` (0..1).
pub fn inverse_tonemap(y: f32) -> f32 {
    let y = y.clamp(0.0, 0.999);
    let a = 2.51 - 2.43 * y;
    let b = 0.03 - 0.59 * y;
    let c = -0.14 * y;
    let disc = (b * b - 4.0 * a * c).max(0.0);
    ((-b + disc.sqrt()) / (2.0 * a)).max(0.0)
}

/// Renders `scene` from `camera` with every sample of `settings` at once: for the command
/// line and tests.
pub fn render_image(
    scene: Arc<TraceScene>,
    camera: RenderCamera,
    settings: RenderSettings,
    denoise: bool,
) -> RenderImage {
    let mut renderer = Renderer::new(scene, camera, settings);
    renderer.render_all(&AtomicBool::new(false));
    renderer.image(denoise)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::{Camera, StandardView};
    use crate::pathtrace::scene::SceneBuilder;
    use oxyde_kernel::{FaceMesh, Mesh};

    /// A UV sphere of radius `r` at `c`, wound outwards, as one face.
    fn sphere(c: [f32; 3], r: f32) -> Mesh {
        let (rings, segments) = (24u32, 48u32);
        let mut positions = Vec::new();
        for i in 0..=rings {
            let theta = std::f32::consts::PI * i as f32 / rings as f32;
            for j in 0..segments {
                let phi = std::f32::consts::TAU * j as f32 / segments as f32;
                positions.push([
                    c[0] + r * theta.sin() * phi.cos(),
                    c[1] + r * theta.cos(),
                    c[2] + r * theta.sin() * phi.sin(),
                ]);
            }
        }
        let mut triangles = Vec::new();
        for i in 0..rings {
            for j in 0..segments {
                let a = i * segments + j;
                let b = i * segments + (j + 1) % segments;
                let c2 = a + segments;
                let d = b + segments;
                triangles.push([a, b, c2]);
                triangles.push([b, d, c2]);
            }
        }
        // Make sure the winding faces out (it depends on the parametrisation).
        let p = |i: u32| V::from_array(positions[i as usize]);
        let t = triangles[segments as usize * 4];
        let n = (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0]));
        if n.dot(p(t[0]) - V::from_array(c)) < 0.0 {
            for t in &mut triangles {
                t.swap(1, 2);
            }
        }
        Mesh {
            faces: vec![FaceMesh {
                positions,
                triangles,
            }],
            edges: Vec::new(),
        }
    }

    fn scene_with(look: Look) -> Arc<TraceScene> {
        let mut b = SceneBuilder::new();
        let l = b.add_look(look);
        b.add_mesh(&sphere([0.0, 0.0, 0.0], 10.0), &[l]);
        Arc::new(b.build())
    }

    fn front_camera(fov: Option<f64>) -> RenderCamera {
        let mut camera = Camera::default();
        camera.set_view(StandardView::Front);
        camera.height = 30.0;
        RenderCamera::from_view(&camera, fov)
    }

    fn uniform(strength: f32) -> RenderSettings {
        RenderSettings {
            width: 32,
            height: 32,
            samples: 64,
            environment: Environment {
                kind: EnvironmentKind::Uniform,
                strength,
                ..Environment::default()
            },
            background: Background::Environment,
            floor: Floor::None,
            threads: 2,
            ..RenderSettings::default()
        }
    }

    fn renderer(scene: Arc<TraceScene>, settings: RenderSettings) -> Renderer {
        let mut r = Renderer::new(scene, front_camera(Some(50.0)), settings);
        r.render_all(&AtomicBool::new(false));
        r
    }

    /// Mean linear colour of the pixels within `radius` of the centre.
    fn centre_mean(r: &Renderer, radius: f32) -> V {
        let lin = r.linear();
        let (w, h) = (r.settings.width, r.settings.height);
        let mut sum = V::ZERO;
        let mut n = 0.0;
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (
                    x as f32 + 0.5 - w as f32 / 2.0,
                    y as f32 + 0.5 - h as f32 / 2.0,
                );
                if (dx * dx + dy * dy).sqrt() < radius {
                    sum += V::from_array(lin[y * w + x].0);
                    n += 1.0;
                }
            }
        }
        sum / n
    }

    #[test]
    fn furnace_a_white_matte_sphere_disappears_in_white_light() {
        let look = Look {
            base_color: [1.0; 3],
            roughness: 1.0,
            ..Look::default()
        };
        let r = renderer(scene_with(look), uniform(1.0));
        let m = centre_mean(&r, 8.0);
        for c in m.to_array() {
            assert!((c - 1.0).abs() < 0.08, "{m:?}");
        }
    }

    #[test]
    fn a_mirror_sphere_shows_the_environment() {
        let look = Look {
            base_color: [1.0; 3],
            metallic: 1.0,
            roughness: 0.0,
            ..Look::default()
        };
        let r = renderer(scene_with(look), uniform(0.5));
        let m = centre_mean(&r, 8.0);
        for c in m.to_array() {
            assert!((c - 0.5).abs() < 0.03, "{m:?}");
        }
    }

    #[test]
    fn renders_are_reproducible_whatever_the_threads() {
        let settings = RenderSettings {
            width: 24,
            height: 18,
            samples: 4,
            threads: 1,
            ..RenderSettings::default()
        };
        let scene = scene_with(Look::default());
        let a = render_image(scene.clone(), front_camera(Some(35.0)), settings, false);
        let b = render_image(
            scene.clone(),
            front_camera(Some(35.0)),
            RenderSettings {
                threads: 5,
                ..settings
            },
            false,
        );
        assert_eq!(a, b);
        let c = render_image(
            scene,
            front_camera(Some(35.0)),
            RenderSettings {
                seed: 9,
                ..settings
            },
            false,
        );
        assert_ne!(a, c);
    }

    #[test]
    fn the_model_casts_a_shadow_on_the_catcher_and_transparency_keeps_it() {
        let settings = RenderSettings {
            width: 48,
            height: 48,
            samples: 32,
            floor: Floor::ShadowCatcher,
            threads: 2,
            ..RenderSettings::default()
        };
        let mut camera = Camera::default();
        camera.set_view(StandardView::Top);
        camera.height = 60.0;
        let cam = RenderCamera::from_view(&camera, None);
        let scene = scene_with(Look::default());
        let mut r = Renderer::new(scene.clone(), cam, settings);
        r.render_all(&AtomicBool::new(false));
        // From the top, the sphere hides its own shadow's core; compare the floor near
        // the sphere on the side away from the key light with the far corner.
        let lin = r.linear();
        let at = |x: usize, y: usize| V::from_array(lin[y * 48 + x].0).luminance();
        // The key light is to the left of and in front of the viewer: the shadow falls
        // right and away (up in a top view's picture).
        let near: f32 = (0..48)
            .flat_map(|y| (0..48).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let (dx, dy) = (x as f32 - 24.0, y as f32 - 24.0);
                let d = (dx * dx + dy * dy).sqrt();
                d > 9.0 && d < 13.0
            })
            .map(|(x, y)| at(x, y))
            .fold(f32::INFINITY, f32::min);
        let far = at(1, 46).max(at(46, 46)).max(at(1, 1)).max(at(46, 1));
        assert!(near < far * 0.8, "near {near}, far {far}");

        // With a transparent background, the open floor is transparent and the shadow is
        // not.
        let mut t = Renderer::new(
            scene,
            cam,
            RenderSettings {
                background: Background::Transparent,
                ..settings
            },
        );
        t.render_all(&AtomicBool::new(false));
        let img = t.image(false);
        let alpha = |x: usize, y: usize| img.rgba[(y * 48 + x) * 4 + 3];
        assert_eq!(alpha(24, 24), 255, "the sphere is opaque");
        assert!(alpha(0, 47) < 60, "open floor: {}", alpha(0, 47));
    }

    #[test]
    fn a_plain_background_comes_out_as_its_colour() {
        let settings = RenderSettings {
            width: 16,
            height: 16,
            samples: 2,
            background: Background::Color([200, 120, 40]),
            floor: Floor::None,
            exposure: 1.5,
            threads: 1,
            ..RenderSettings::default()
        };
        let img = render_image(
            scene_with(Look::default()),
            front_camera(Some(200.0)),
            settings,
            true,
        );
        let px = &img.rgba[0..4];
        for (got, want) in px.iter().zip([200u8, 120, 40, 255]) {
            assert!((i32::from(*got) - i32::from(want)).abs() <= 1, "{px:?}");
        }
        for y in [0.0, 0.1, 0.5, 0.9] {
            assert!((tonemap(inverse_tonemap(y)) - y).abs() < 1e-3);
        }
    }

    #[test]
    fn huge_sizes_are_scaled_to_the_pixel_budget() {
        assert_eq!(fit_pixel_budget(1920, 1080), (1920, 1080));
        let (w, h) = fit_pixel_budget(16_384, 16_384);
        assert!(w * h <= MAX_PIXELS && w == h && w > 2800);
        let (w, h) = fit_pixel_budget(20_000, 100);
        assert!(w * h <= MAX_PIXELS && h <= 100);
        assert_eq!(fit_pixel_budget(0, 0), (1, 1));
    }

    #[test]
    fn an_empty_scene_renders_its_background() {
        let scene = Arc::new(SceneBuilder::new().build());
        let settings = RenderSettings {
            width: 8,
            height: 8,
            samples: 1,
            background: Background::Color([10, 20, 30]),
            threads: 1,
            ..RenderSettings::default()
        };
        let img = render_image(scene, front_camera(None), settings, false);
        assert!((i32::from(img.rgba[0]) - 10).abs() <= 1);
    }

    #[test]
    fn cancelling_stops_a_pass() {
        let mut r = Renderer::new(
            scene_with(Look::default()),
            front_camera(None),
            RenderSettings {
                width: 64,
                height: 48,
                threads: 2,
                ..RenderSettings::default()
            },
        );
        assert!(!r.render_pass(&AtomicBool::new(true)));
        assert_eq!(r.samples(), 0);
        assert!(r.render_pass(&AtomicBool::new(false)));
        assert_eq!(r.samples(), 1);
    }
}
