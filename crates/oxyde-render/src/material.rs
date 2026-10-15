//! How faces look: a colour, an opacity and optionally a colour texture, decoded and ready
//! to sample. The application turns a part's appearances into one [`Material`] per face
//! of the scene; both renderers draw them the same way.

use std::sync::Arc;

use crate::style::Rgb;

/// The look of one face.
#[derive(Debug, Clone, PartialEq)]
pub struct Material {
    /// `None`: the style's part colour.
    pub color: Option<Rgb>,
    /// 1 = opaque, 0 = invisible.
    pub opacity: f32,
    /// Index into the scene's textures; it replaces the colour where set.
    pub texture: Option<usize>,
    /// Millimetres covered by one repeat of the texture, along its width.
    pub texture_scale: f32,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            color: None,
            opacity: 1.0,
            texture: None,
            texture_scale: 50.0,
        }
    }
}

impl Material {
    pub fn is_transparent(&self) -> bool {
        self.opacity < 0.999
    }
}

/// A decoded colour texture: RGBA, 8 bits per channel, rows from the top.
#[derive(Debug, Clone, PartialEq)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Texture {
    /// Height over width: a texture `scale` millimetres wide covers `scale × aspect`
    /// millimetres in height, so the image is not squashed.
    pub fn aspect(&self) -> f32 {
        if self.width == 0 {
            1.0
        } else {
            self.height as f32 / self.width as f32
        }
    }

    /// Bilinear sample at (`u`, `v`) in repeats (wrapping), as sRGB 0..255.
    pub fn sample(&self, u: f32, v: f32) -> [f32; 3] {
        if self.width == 0 || self.height == 0 {
            return [128.0; 3];
        }
        let (w, h) = (self.width as f32, self.height as f32);
        let x = (u.rem_euclid(1.0)) * w - 0.5;
        let y = (v.rem_euclid(1.0)) * h - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |px: f32, py: f32| -> [f32; 3] {
            let i = (px as i64).rem_euclid(self.width as i64) as usize;
            let j = (py as i64).rem_euclid(self.height as i64) as usize;
            let k = (j * self.width as usize + i) * 4;
            [
                f32::from(self.rgba[k]),
                f32::from(self.rgba[k + 1]),
                f32::from(self.rgba[k + 2]),
            ]
        };
        let (a, b, c, d) = (
            at(x0, y0),
            at(x0 + 1.0, y0),
            at(x0, y0 + 1.0),
            at(x0 + 1.0, y0 + 1.0),
        );
        std::array::from_fn(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = c[k] + (d[k] - c[k]) * fx;
            top + (bottom - top) * fy
        })
    }

    /// The colour at a model point `p` with unit normal `n`, mapped automatically: the
    /// image is projected along each axis and the three blended by how much the surface
    /// faces that axis (triplanar mapping), so no texture coordinates are needed.
    pub fn triplanar(&self, p: [f32; 3], n: [f32; 3], scale: f32) -> [f32; 3] {
        let scale = if scale > 1e-6 { scale } else { 1.0 };
        let sv = scale * self.aspect();
        let mut w = [n[0].abs().powi(4), n[1].abs().powi(4), n[2].abs().powi(4)];
        let sum = w[0] + w[1] + w[2];
        if sum < 1e-12 {
            w = [0.0, 1.0, 0.0];
        } else {
            w = w.map(|k| k / sum);
        }
        let mut out = [0.0f32; 3];
        if w[0] > 0.004 {
            let s = self.sample(p[2] / scale, -p[1] / sv);
            out = std::array::from_fn(|k| out[k] + s[k] * w[0]);
        }
        if w[1] > 0.004 {
            let s = self.sample(p[0] / scale, p[2] / sv);
            out = std::array::from_fn(|k| out[k] + s[k] * w[1]);
        }
        if w[2] > 0.004 {
            let s = self.sample(p[0] / scale, -p[1] / sv);
            out = std::array::from_fn(|k| out[k] + s[k] * w[2]);
        }
        out
    }
}

/// Textures shared between the scene and the renderers.
pub type TextureRef = Arc<Texture>;

#[cfg(test)]
mod tests {
    use super::*;

    fn checker() -> Texture {
        // 2 × 2: black, white / white, black.
        let px = |v: u8| [v, v, v, 255];
        Texture {
            width: 2,
            height: 2,
            rgba: [px(0), px(255), px(255), px(0)].concat(),
        }
    }

    #[test]
    fn sampling_wraps_and_interpolates() {
        let t = checker();
        assert_eq!(t.sample(0.25, 0.25), [0.0; 3]);
        assert_eq!(t.sample(0.75, 0.25), [255.0; 3]);
        assert_eq!(t.sample(1.25, 2.25), [0.0; 3]);
        assert_eq!(t.sample(-0.25, 0.25), [255.0; 3]);
        // Half way between two texels.
        assert_eq!(t.sample(0.5, 0.25), [127.5; 3]);
    }

    #[test]
    fn triplanar_follows_the_facing_axis() {
        let t = checker();
        // A face looking along +Y samples the X–Z projection.
        let on_top = t.triplanar([2.5, 0.0, 2.5], [0.0, 1.0, 0.0], 10.0);
        assert_eq!(on_top, t.sample(0.25, 0.25));
        let on_top = t.triplanar([7.5, 0.0, 2.5], [0.0, 1.0, 0.0], 10.0);
        assert_eq!(on_top, t.sample(0.75, 0.25));
        // A face looking along +X does not depend on x.
        let a = t.triplanar([0.0, 2.5, 2.5], [1.0, 0.0, 0.0], 10.0);
        let b = t.triplanar([99.0, 2.5, 2.5], [1.0, 0.0, 0.0], 10.0);
        assert_eq!(a, b);
        assert_eq!(t.aspect(), 1.0);
    }
}
