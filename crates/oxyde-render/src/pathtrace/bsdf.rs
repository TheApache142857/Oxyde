//! How a surface scatters light: the metallic-roughness model (a Lambert diffuse lobe
//! under a GGX microfacet specular lobe with Schlick's Fresnel, as glTF and Blender's
//! Principled BSDF define it), and smooth glass.
//!
//! Directions point away from the surface: `wo` towards where the light goes (the
//! viewer), `wi` towards where it comes from. Values returned include the cosine of `wi`
//! with the normal, so the tracer multiplies by them directly.

use std::f32::consts::PI;

use super::sampling::{Rng, cosine_hemisphere};
use super::vector::V;

/// Roughness below this is treated as this: a perfect mirror would need a special case.
const MIN_ALPHA: f32 = 0.002;

/// A surface at a hit point.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Surface {
    pub base: V,
    pub metallic: f32,
    /// GGX α: roughness squared.
    pub alpha: f32,
    pub ior: f32,
}

/// A direction drawn from the surface's scattering.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scatter {
    pub wi: V,
    /// Scattering × cosine ÷ density: what the path's throughput is multiplied by.
    pub weight: V,
    /// Density of `wi` (solid angle); meaningless for a delta lobe.
    pub pdf: f32,
    /// A mirror or a refraction: lights cannot be aimed at.
    pub delta: bool,
}

fn schlick(f0: V, cos: f32) -> V {
    let m = (1.0 - cos).clamp(0.0, 1.0);
    let m5 = m * m * m * m * m;
    f0 + (V::ONE - f0) * m5
}

fn ggx_d(alpha: f32, n_h: f32) -> f32 {
    let a2 = alpha * alpha;
    let d = n_h * n_h * (a2 - 1.0) + 1.0;
    a2 / (PI * d * d).max(1e-12)
}

fn smith_g1(alpha: f32, n_v: f32) -> f32 {
    let a2 = alpha * alpha;
    2.0 * n_v / (n_v + (a2 + (1.0 - a2) * n_v * n_v).sqrt()).max(1e-12)
}

impl Surface {
    pub fn new(base: V, metallic: f32, roughness: f32, ior: f32) -> Surface {
        Surface {
            base,
            metallic: metallic.clamp(0.0, 1.0),
            alpha: (roughness * roughness).max(MIN_ALPHA),
            ior: if ior.is_finite() && ior > 1.0 {
                ior
            } else {
                1.5
            },
        }
    }

    /// Reflectance at normal incidence: a dielectric's from its index, a metal's its
    /// colour.
    fn f0(&self) -> V {
        let r = (self.ior - 1.0) / (self.ior + 1.0);
        V::splat(r * r).lerp(self.base, self.metallic)
    }

    fn diffuse(&self) -> V {
        self.base * (1.0 - self.metallic)
    }

    /// The chance of drawing from the specular lobe rather than the diffuse one.
    fn specular_chance(&self, n_v: f32) -> f32 {
        let diffuse = self.diffuse().luminance();
        if diffuse <= 1e-4 {
            return 1.0;
        }
        let f = schlick(self.f0(), n_v).luminance();
        (f / (f + diffuse * (1.0 - f))).clamp(0.15, 0.85)
    }

    /// Scattering × cosine for light from `wi` to `wo` about the unit normal `n`, and the
    /// density [`Surface::sample`] draws `wi` with.
    pub fn eval(&self, n: V, wo: V, wi: V) -> (V, f32) {
        let (n_v, n_l) = (n.dot(wo), n.dot(wi));
        if n_v <= 1e-6 || n_l <= 1e-6 {
            return (V::ZERO, 0.0);
        }
        let h = (wo + wi).normalized_or(n);
        let (n_h, v_h) = (n.dot(h).max(0.0), wo.dot(h).max(1e-6));
        let d = ggx_d(self.alpha, n_h);
        let g = smith_g1(self.alpha, n_v) * smith_g1(self.alpha, n_l);
        let f = schlick(self.f0(), v_h);
        let specular = f * (d * g / (4.0 * n_v));
        let fresnel_view = schlick(self.f0(), n_v).luminance();
        let diffuse = self.diffuse() * ((1.0 - fresnel_view) * n_l / PI);
        let ps = self.specular_chance(n_v);
        let pdf = ps * d * n_h / (4.0 * v_h) + (1.0 - ps) * n_l / PI;
        (specular + diffuse, pdf)
    }

    /// Draws `wi` for light leaving towards `wo`.
    pub fn sample(&self, n: V, wo: V, rng: &mut Rng) -> Option<Scatter> {
        let n_v = n.dot(wo);
        if n_v <= 1e-6 {
            return None;
        }
        let ps = self.specular_chance(n_v);
        let wi = if rng.next() < ps {
            // A microfacet normal drawn with density D(h) cos θh.
            let (u1, u2) = rng.next2();
            let a2 = self.alpha * self.alpha;
            let cos_t = ((1.0 - u1) / (1.0 + (a2 - 1.0) * u1)).max(0.0).sqrt();
            let sin_t = (1.0 - cos_t * cos_t).max(0.0).sqrt();
            let phi = 2.0 * PI * u2;
            let (t, b) = n.basis();
            let h = (t * (sin_t * phi.cos()) + b * (sin_t * phi.sin()) + n * cos_t).normalized();
            (-wo).reflect(h)
        } else {
            cosine_hemisphere(n, rng.next2())
        };
        let (f, pdf) = self.eval(n, wo, wi);
        if pdf <= 1e-8 || !f.is_finite() {
            return None;
        }
        Some(Scatter {
            wi,
            weight: f / pdf,
            pdf,
            delta: false,
        })
    }
}

/// Fresnel reflectance of a smooth dielectric boundary, for unpolarised light meeting it
/// at `cos_i` from the side whose index is `eta` times the other's.
fn fresnel_dielectric(cos_i: f32, eta: f32) -> f32 {
    let sin2_t = eta * eta * (1.0 - cos_i * cos_i).max(0.0);
    if sin2_t >= 1.0 {
        return 1.0;
    }
    let cos_t = (1.0 - sin2_t).sqrt();
    let rs = (eta * cos_i - cos_t) / (eta * cos_i + cos_t);
    let rp = (cos_i - eta * cos_t) / (cos_i + eta * cos_t);
    0.5 * (rs * rs + rp * rp)
}

/// Smooth glass: reflects or refracts by Fresnel's law. `outward` is the surface's
/// outward normal (the meshes are closed solids wound outwards), `tint` the colour light
/// takes each time it crosses the surface.
pub(crate) fn sample_glass(outward: V, wo: V, ior: f32, tint: V, rng: &mut Rng) -> Scatter {
    let entering = outward.dot(wo) > 0.0;
    let (n, eta) = if entering {
        (outward, 1.0 / ior)
    } else {
        (-outward, ior)
    };
    let cos_i = n.dot(wo).clamp(0.0, 1.0);
    let f = fresnel_dielectric(cos_i, eta);
    if rng.next() < f {
        return Scatter {
            wi: (-wo).reflect(n),
            weight: V::ONE,
            pdf: 1.0,
            delta: true,
        };
    }
    // Refraction of the incoming direction -wo through the boundary.
    let d = -wo;
    let cos_d = -n.dot(d);
    let k = 1.0 - eta * eta * (1.0 - cos_d * cos_d);
    if k < 0.0 {
        return Scatter {
            wi: d.reflect(n),
            weight: V::ONE,
            pdf: 1.0,
            delta: true,
        };
    }
    let wi = (d * eta + n * (eta * cos_d - k.sqrt())).normalized();
    Scatter {
        wi,
        weight: tint,
        pdf: 1.0,
        delta: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fraction of light a surface sends back when lit evenly from the hemisphere
    /// (its albedo), estimated by sampling.
    fn albedo(s: &Surface, n: V, wo: V) -> V {
        let mut rng = Rng::new(9, 1);
        let count = 40_000;
        let mut sum = V::ZERO;
        for _ in 0..count {
            if let Some(sc) = s.sample(n, wo, &mut rng) {
                sum += sc.weight;
            }
        }
        sum / count as f32
    }

    #[test]
    fn surfaces_never_create_energy() {
        let n = V::Y;
        for (metallic, roughness) in [(0.0, 0.75), (0.0, 0.1), (1.0, 0.3), (1.0, 0.05), (0.5, 0.5)]
        {
            let s = Surface::new(V::ONE, metallic, roughness, 1.5);
            for angle in [0.0f32, 0.6, 1.2] {
                let wo = V::new(angle.sin(), angle.cos(), 0.0);
                let a = albedo(&s, n, wo);
                assert!(
                    a.max_component() < 1.03,
                    "{metallic} {roughness} {angle}: {a:?}"
                );
                assert!(
                    a.max_component() > 0.6,
                    "{metallic} {roughness} {angle}: {a:?}"
                );
            }
        }
    }

    #[test]
    fn sampling_density_matches_evaluation() {
        // The density a sample reports is the one eval gives for its direction.
        let s = Surface::new(V::new(0.8, 0.5, 0.3), 0.3, 0.4, 1.5);
        let n = V::Y;
        let wo = V::new(0.3, 0.9, 0.1).normalized();
        let mut rng = Rng::new(5, 5);
        for _ in 0..100 {
            if let Some(sc) = s.sample(n, wo, &mut rng) {
                let (f, pdf) = s.eval(n, wo, sc.wi);
                assert!((pdf - sc.pdf).abs() < 1e-4 * pdf.max(1.0));
                assert!(((f / pdf) - sc.weight).length() < 1e-3);
            }
        }
        // Light from below the surface does nothing.
        assert_eq!(s.eval(n, wo, -V::Y).1, 0.0);
    }

    #[test]
    fn glass_refracts_and_reflects_by_fresnel() {
        let mut rng = Rng::new(2, 3);
        let wo = V::Y;
        let (mut reflected, mut refracted) = (0, 0);
        for _ in 0..10_000 {
            let s = sample_glass(V::Y, wo, 1.5, V::ONE, &mut rng);
            assert!(s.delta);
            if s.wi.y > 0.0 {
                reflected += 1;
            } else {
                refracted += 1;
                // Straight through at normal incidence.
                assert!((s.wi.y + 1.0).abs() < 1e-5);
            }
        }
        // 4% reflected at normal incidence for n = 1.5.
        let r = f64::from(reflected) / 10_000.0;
        assert!((r - 0.04).abs() < 0.01, "{r}");
        assert!(refracted > 9000);
        // Leaving glass at a grazing angle: total internal reflection.
        let wo = V::new(0.95, -0.31, 0.0).normalized();
        let s = sample_glass(V::Y, wo, 1.5, V::ONE, &mut rng);
        assert!(s.wi.y < 0.0, "reflected back inside");
        assert!((fresnel_dielectric(1.0, 1.0 / 1.5) - 0.04).abs() < 1e-3);
    }
}
