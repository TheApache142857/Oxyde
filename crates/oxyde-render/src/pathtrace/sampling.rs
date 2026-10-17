//! Random numbers and the directions drawn from them.
//!
//! Every pixel sample gets its own generator, seeded from the pixel, the sample number
//! and the render's seed, so a picture does not depend on how the rows were shared out
//! between threads: the same scene and seed give the same image on any machine with any
//! number of cores.

use std::f32::consts::TAU;

use super::vector::V;

/// PCG32 (O'Neill): small, fast and good enough for Monte Carlo.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
    inc: u64,
}

/// SplitMix64's finaliser: spreads close seeds far apart.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Rng {
    pub fn new(seed: u64, stream: u64) -> Rng {
        let mut rng = Rng {
            state: 0,
            inc: (mix(stream) << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(mix(seed));
        rng.next_u32();
        rng
    }

    /// The generator of one pixel sample.
    pub fn for_sample(seed: u64, pixel: u64, sample: u32) -> Rng {
        Rng::new(
            mix(seed ^ pixel.wrapping_mul(0x2545_F491_4F6C_DD1D)),
            u64::from(sample),
        )
    }

    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform in [0, 1).
    pub fn next(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
    }

    pub fn next2(&mut self) -> (f32, f32) {
        (self.next(), self.next())
    }
}

/// A direction about the unit normal `n` with probability density cos θ / π.
pub fn cosine_hemisphere(n: V, u: (f32, f32)) -> V {
    let r = u.0.sqrt();
    let phi = TAU * u.1;
    let (t, b) = n.basis();
    let z = (1.0 - u.0).max(0.0).sqrt();
    (t * (r * phi.cos()) + b * (r * phi.sin()) + n * z).normalized_or(n)
}

/// A direction inside the cone of half-angle acos(`cos_max`) about the unit `axis`,
/// uniformly over its solid angle (density 1 / (2π (1 − cos_max))).
pub fn uniform_cone(axis: V, cos_max: f32, u: (f32, f32)) -> V {
    let cos_t = 1.0 - u.0 * (1.0 - cos_max);
    let sin_t = (1.0 - cos_t * cos_t).max(0.0).sqrt();
    let phi = TAU * u.1;
    let (t, b) = axis.basis();
    (t * (sin_t * phi.cos()) + b * (sin_t * phi.sin()) + axis * cos_t).normalized_or(axis)
}

/// The density of [`uniform_cone`] inside the cone.
pub fn cone_pdf(cos_max: f32) -> f32 {
    1.0 / (TAU * (1.0 - cos_max).max(1e-7))
}

/// The power heuristic (β = 2) for multiple importance sampling: the weight of a sample
/// drawn with density `a` when `b` could have drawn it too.
pub fn power_heuristic(a: f32, b: f32) -> f32 {
    let (a2, b2) = (a * a, b * b);
    if a2 + b2 <= 0.0 { 0.0 } else { a2 / (a2 + b2) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_uniform_and_reproducible() {
        let mut a = Rng::for_sample(7, 1234, 3);
        let mut b = Rng::for_sample(7, 1234, 3);
        let mut sum = 0.0f64;
        for _ in 0..10_000 {
            let x = a.next();
            assert_eq!(x, b.next());
            assert!((0.0..1.0).contains(&x));
            sum += f64::from(x);
        }
        assert!((sum / 10_000.0 - 0.5).abs() < 0.02);
        // Neighbouring pixels and samples are not correlated in the obvious way.
        assert_ne!(
            Rng::for_sample(7, 1, 0).next(),
            Rng::for_sample(7, 2, 0).next()
        );
        assert_ne!(
            Rng::for_sample(7, 1, 0).next(),
            Rng::for_sample(7, 1, 1).next()
        );
    }

    #[test]
    fn hemisphere_and_cone_directions_stay_where_they_should() {
        let mut rng = Rng::new(1, 2);
        let n = V::new(0.2, 0.9, -0.3).normalized();
        let mut mean_cos = 0.0;
        for _ in 0..20_000 {
            let d = cosine_hemisphere(n, rng.next2());
            assert!(d.dot(n) >= -1e-5);
            assert!((d.length() - 1.0).abs() < 1e-4);
            mean_cos += d.dot(n) / 20_000.0;
        }
        // E[cos θ] under cos θ / π is 2/3.
        assert!((mean_cos - 2.0 / 3.0).abs() < 0.01, "{mean_cos}");
        let cos_max = 0.9;
        for _ in 0..2000 {
            let d = uniform_cone(n, cos_max, rng.next2());
            assert!(d.dot(n) >= cos_max - 1e-4);
        }
        assert!((power_heuristic(1.0, 1.0) - 0.5).abs() < 1e-6);
        assert_eq!(power_heuristic(0.0, 0.0), 0.0);
    }
}
