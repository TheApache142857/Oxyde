//! The light around the model: a built-in environment (a photo studio, an outdoor sky,
//! or a plain uniform light), made of a smooth background radiance and a few lights the
//! size of a softbox or the sun, which the tracer aims at directly so their shadows come
//! out clean in few samples.
//!
//! No image files: the environments are procedural, so nothing has to be shipped or
//! found on disk (HDRI files are a later step, see `docs/RENDERING.md`).

use std::f32::consts::PI;

use super::vector::V;

/// Which built-in environment lights the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvironmentKind {
    /// A neutral grey studio with a key softbox above and to the left of the camera, a
    /// fill on the right and a rim light behind: product shots.
    Studio,
    /// A blue sky with a warm sun.
    Sky,
    /// The same light from every direction and no lights: soft, shadowless. With white
    /// light it is the furnace test, where a white diffuse object disappears.
    Uniform,
}

impl EnvironmentKind {
    pub const ALL: [(EnvironmentKind, &'static str); 3] = [
        (EnvironmentKind::Studio, "Studio"),
        (EnvironmentKind::Sky, "Sky and sun"),
        (EnvironmentKind::Uniform, "Uniform light"),
    ];

    pub fn label(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(k, _)| *k == self)
            .map_or("Studio", |(_, l)| l)
    }
}

/// The environment and how it is placed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Environment {
    pub kind: EnvironmentKind,
    /// Multiplies every light; 1 = as designed.
    pub strength: f32,
    /// Turn about the vertical axis, radians.
    pub rotation: f32,
    /// The lights keep their place relative to the camera (the model is always lit from
    /// the front, however it is turned), rather than to the model.
    pub follow_camera: bool,
}

impl Default for Environment {
    fn default() -> Self {
        Environment {
            kind: EnvironmentKind::Studio,
            strength: 1.0,
            rotation: 0.0,
            follow_camera: true,
        }
    }
}

/// A light far away seen as a disc: a softbox, the sun.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DiscLight {
    /// Unit vector towards the light.
    pub dir: V,
    /// Cosine of the disc's angular radius.
    pub cos_max: f32,
    /// Radiance of the disc (linear RGB).
    pub radiance: V,
}

impl DiscLight {
    /// A light of angular radius `radius_deg` that gives `irradiance` on a surface facing
    /// it.
    fn new(dir: V, radius_deg: f32, irradiance: V) -> DiscLight {
        let cos_max = radius_deg.to_radians().cos();
        let solid_angle_cos = PI * (1.0 - cos_max * cos_max);
        DiscLight {
            dir: dir.normalized(),
            cos_max,
            radiance: irradiance / solid_angle_cos.max(1e-6),
        }
    }
}

/// The environment ready to light a render: the frame it is placed in, its lights and
/// its background.
#[derive(Debug, Clone)]
pub(crate) struct Lighting {
    kind: EnvironmentKind,
    strength: f32,
    /// The environment's own axes in the world: right, up, back.
    frame: (V, V, V),
    pub lights: Vec<DiscLight>,
}

impl Lighting {
    /// Places `env`: in the camera's frame (`right`, `up`, `back` = towards the viewer,
    /// levelled so the environment's up stays the world's) when it follows the camera,
    /// else in the world's; then turned by its rotation.
    pub fn new(env: &Environment, camera_right: V, camera_back: V) -> Lighting {
        let up = V::Y;
        let (right, back) = if env.follow_camera {
            // Level the camera's frame: its back direction projected on the floor.
            let flat = V::new(camera_back.x, 0.0, camera_back.z);
            let back = if flat.length() > 1e-3 {
                flat.normalized()
            } else {
                // Looking straight down or up: the back direction is square to the
                // camera's right.
                V::new(camera_right.x, 0.0, camera_right.z)
                    .normalized_or(V::new(1.0, 0.0, 0.0))
                    .cross(up)
            };
            (up.cross(back).normalized(), back)
        } else {
            (V::new(1.0, 0.0, 0.0), V::new(0.0, 0.0, 1.0))
        };
        let (s, c) = env.rotation.sin_cos();
        let right_r = right * c - back * s;
        let back_r = right * s + back * c;
        let frame = (right_r, up, back_r);
        let at = |x: f32, y: f32, z: f32| (right_r * x + up * y + back_r * z).normalized();
        let k = env.strength.max(0.0);
        let lights = match env.kind {
            EnvironmentKind::Studio => vec![
                // Key: above, left, in front.
                DiscLight::new(at(-0.55, 0.75, 0.6), 14.0, V::new(2.3, 2.25, 2.15) * k),
                // Fill: right, low, in front, broad and soft.
                DiscLight::new(at(0.85, 0.2, 0.5), 22.0, V::new(0.55, 0.58, 0.62) * k),
                // Rim: behind and above, to draw the silhouette.
                DiscLight::new(at(0.25, 0.6, -0.8), 9.0, V::new(1.3, 1.3, 1.35) * k),
            ],
            EnvironmentKind::Sky => vec![
                // The sun: warm, small, high.
                DiscLight::new(at(-0.45, 0.8, 0.4), 2.5, V::new(3.0, 2.75, 2.35) * k),
            ],
            EnvironmentKind::Uniform => Vec::new(),
        };
        Lighting {
            kind: env.kind,
            strength: k,
            frame,
            lights,
        }
    }

    /// What every light and the background are multiplied by.
    pub fn strength(&self) -> f32 {
        self.strength
    }

    /// Radiance arriving from direction `dir` (unit), leaving the lights out.
    pub fn background(&self, dir: V) -> V {
        let (_, up, _) = self.frame;
        let h = dir.dot(up);
        let k = self.strength;
        match self.kind {
            EnvironmentKind::Studio => {
                // A soft grey dome, brighter above, a dark floor below the horizon.
                let top = V::new(0.62, 0.63, 0.66);
                let horizon = V::new(0.42, 0.42, 0.43);
                let floor = V::new(0.16, 0.16, 0.16);
                let c = if h >= 0.0 {
                    horizon.lerp(top, h.sqrt())
                } else {
                    horizon.lerp(floor, (-h).sqrt().min(1.0))
                };
                c * k
            }
            EnvironmentKind::Sky => {
                let zenith = V::new(0.18, 0.34, 0.75);
                let horizon = V::new(0.70, 0.80, 0.95);
                let ground = V::new(0.22, 0.20, 0.17);
                let c = if h >= 0.0 {
                    horizon.lerp(zenith, h.powf(0.6))
                } else {
                    horizon.lerp(ground, (-h * 4.0).min(1.0))
                };
                c * k
            }
            EnvironmentKind::Uniform => V::ONE * k,
        }
    }

    /// The light whose disc `dir` falls in, if any, with its radiance.
    pub fn light_in(&self, dir: V) -> Option<(usize, V)> {
        self.lights
            .iter()
            .enumerate()
            .find(|(_, l)| dir.dot(l.dir) >= l.cos_max)
            .map(|(i, l)| (i, l.radiance))
    }

    /// Everything seen in direction `dir`: background and lights.
    #[cfg(test)]
    pub fn radiance(&self, dir: V) -> V {
        let mut c = self.background(dir);
        for l in &self.lights {
            if dir.dot(l.dir) >= l.cos_max {
                c += l.radiance;
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lights_give_the_irradiance_they_were_designed_for() {
        // Integrate the disc's radiance over its solid angle, weighted by cos θ, for a
        // surface facing it: the irradiance it was made with.
        let light = DiscLight::new(V::Y, 14.0, V::splat(2.0));
        let n = 200_000;
        let mut rng = super::super::sampling::Rng::new(3, 4);
        let mut e = 0.0f64;
        for _ in 0..n {
            let d = super::super::sampling::cosine_hemisphere(V::Y, rng.next2());
            if d.dot(light.dir) >= light.cos_max {
                // Cosine sampling: E = π × mean radiance.
                e += f64::from(light.radiance.x) * std::f64::consts::PI / n as f64;
            }
        }
        assert!((e - 2.0).abs() < 0.1, "{e}");
    }

    #[test]
    fn following_the_camera_turns_the_lights_with_it() {
        let env = Environment::default();
        let front = Lighting::new(&env, V::new(1.0, 0.0, 0.0), V::new(0.0, 0.0, 1.0));
        let side = Lighting::new(&env, V::new(0.0, 0.0, -1.0), V::new(1.0, 0.0, 0.0));
        // The key light is in front of the camera both times (on the viewer's side).
        assert!(front.lights[0].dir.z > 0.0);
        assert!(side.lights[0].dir.x > 0.0);
        // Fixed to the world, it does not move.
        let fixed = Environment {
            follow_camera: false,
            ..env
        };
        let a = Lighting::new(&fixed, V::new(1.0, 0.0, 0.0), V::new(0.0, 0.0, 1.0));
        let b = Lighting::new(&fixed, V::new(0.0, 0.0, -1.0), V::new(1.0, 0.0, 0.0));
        assert_eq!(a.lights[0].dir, b.lights[0].dir);
        // Looking straight down still gives a sane frame.
        let down = Lighting::new(&env, V::new(1.0, 0.0, 0.0), V::Y);
        assert!(down.lights[0].dir.is_finite());
    }

    #[test]
    fn uniform_light_is_uniform() {
        let env = Environment {
            kind: EnvironmentKind::Uniform,
            strength: 0.5,
            ..Environment::default()
        };
        let l = Lighting::new(&env, V::new(1.0, 0.0, 0.0), V::new(0.0, 0.0, 1.0));
        assert!(l.lights.is_empty());
        assert_eq!(l.radiance(V::Y), V::splat(0.5));
        assert_eq!(l.radiance(-V::Y), V::splat(0.5));
    }
}
