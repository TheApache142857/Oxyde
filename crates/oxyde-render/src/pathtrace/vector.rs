//! A small `f32` vector for the path tracer: half the memory of the rasterizer's `f64`
//! arrays and faster to trace with, which millions of rays per second need. Model
//! coordinates fit easily: an f32 resolves a micron over ten metres.

use std::ops::{Add, AddAssign, Div, Mul, MulAssign, Neg, Sub};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct V {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl V {
    pub const ZERO: V = V::new(0.0, 0.0, 0.0);
    pub const ONE: V = V::new(1.0, 1.0, 1.0);
    pub const Y: V = V::new(0.0, 1.0, 0.0);

    pub const fn new(x: f32, y: f32, z: f32) -> V {
        V { x, y, z }
    }

    pub const fn splat(v: f32) -> V {
        V::new(v, v, v)
    }

    pub fn from_f64(p: [f64; 3]) -> V {
        V::new(p[0] as f32, p[1] as f32, p[2] as f32)
    }

    pub fn from_array(p: [f32; 3]) -> V {
        V::new(p[0], p[1], p[2])
    }

    pub fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }

    pub fn dot(self, o: V) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: V) -> V {
        V::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// The unit vector along `self`, or `fallback` for a vector too short to have a
    /// direction.
    pub fn normalized_or(self, fallback: V) -> V {
        let len = self.length();
        if len > 1e-20 && len.is_finite() {
            self / len
        } else {
            fallback
        }
    }

    pub fn normalized(self) -> V {
        self.normalized_or(V::Y)
    }

    pub fn min(self, o: V) -> V {
        V::new(self.x.min(o.x), self.y.min(o.y), self.z.min(o.z))
    }

    pub fn max(self, o: V) -> V {
        V::new(self.x.max(o.x), self.y.max(o.y), self.z.max(o.z))
    }

    pub fn max_component(self) -> f32 {
        self.x.max(self.y).max(self.z)
    }

    /// Component `k` (0 = x, 1 = y, anything else = z).
    pub fn axis(self, k: usize) -> f32 {
        match k {
            0 => self.x,
            1 => self.y,
            _ => self.z,
        }
    }

    pub fn map(self, f: impl Fn(f32) -> f32) -> V {
        V::new(f(self.x), f(self.y), f(self.z))
    }

    /// Relative luminance of a linear colour (Rec. 709).
    pub fn luminance(self) -> f32 {
        0.2126 * self.x + 0.7152 * self.y + 0.0722 * self.z
    }

    pub fn lerp(self, o: V, t: f32) -> V {
        self + (o - self) * t
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    /// `self` mirrored about the unit normal `n`.
    pub fn reflect(self, n: V) -> V {
        self - n * (2.0 * self.dot(n))
    }

    /// Two unit vectors completing the unit vector `self` to an orthonormal basis
    /// (Duff et al., "Building an orthonormal basis, revisited").
    pub fn basis(self) -> (V, V) {
        let sign = if self.z >= 0.0 { 1.0 } else { -1.0 };
        let a = -1.0 / (sign + self.z);
        let b = self.x * self.y * a;
        (
            V::new(1.0 + sign * self.x * self.x * a, sign * b, -sign * self.x),
            V::new(b, sign + self.y * self.y * a, -self.y),
        )
    }
}

impl Add for V {
    type Output = V;
    fn add(self, o: V) -> V {
        V::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl AddAssign for V {
    fn add_assign(&mut self, o: V) {
        *self = *self + o;
    }
}

impl Sub for V {
    type Output = V;
    fn sub(self, o: V) -> V {
        V::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl Mul<f32> for V {
    type Output = V;
    fn mul(self, k: f32) -> V {
        V::new(self.x * k, self.y * k, self.z * k)
    }
}

/// Component by component: a colour filtering another.
impl Mul for V {
    type Output = V;
    fn mul(self, o: V) -> V {
        V::new(self.x * o.x, self.y * o.y, self.z * o.z)
    }
}

impl MulAssign for V {
    fn mul_assign(&mut self, o: V) {
        *self = *self * o;
    }
}

impl MulAssign<f32> for V {
    fn mul_assign(&mut self, k: f32) {
        *self = *self * k;
    }
}

impl Div<f32> for V {
    type Output = V;
    fn div(self, k: f32) -> V {
        self * (1.0 / k)
    }
}

impl Neg for V {
    type Output = V;
    fn neg(self) -> V {
        V::new(-self.x, -self.y, -self.z)
    }
}

/// The sRGB transfer curve undone: an 8-bit sRGB channel as linear light, 0..1.
pub fn srgb_to_linear(c: f32) -> f32 {
    let c = (c / 255.0).clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light, 0..1, encoded with the sRGB transfer curve, 0..1.
pub fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// An 8-bit sRGB colour as linear light.
pub fn srgb8_to_linear(c: [u8; 3]) -> V {
    V::new(
        srgb_to_linear(f32::from(c[0])),
        srgb_to_linear(f32::from(c[1])),
        srgb_to_linear(f32::from(c[2])),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basis_is_orthonormal() {
        for n in [
            V::new(0.0, 0.0, 1.0),
            V::new(0.0, 0.0, -1.0),
            V::new(0.3, -0.8, 0.2).normalized(),
            V::new(-1.0, 0.0, 0.0),
        ] {
            let (t, b) = n.basis();
            assert!(t.dot(n).abs() < 1e-5 && b.dot(n).abs() < 1e-5 && t.dot(b).abs() < 1e-5);
            assert!((t.length() - 1.0).abs() < 1e-5 && (b.length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn srgb_round_trips() {
        for c in [0u8, 1, 10, 54, 128, 200, 255] {
            let back = linear_to_srgb(srgb_to_linear(f32::from(c))) * 255.0;
            assert!((back - f32::from(c)).abs() < 0.01, "{c}");
        }
        assert!((srgb_to_linear(128.0) - 0.2158).abs() < 1e-3);
    }
}
