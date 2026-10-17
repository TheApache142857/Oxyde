//! A denoiser for early, noisy passes: the edge-avoiding à-trous wavelet filter
//! (Dammertz et al., 2010). Five widening 5 × 5 blurs, each weighing a neighbour by how
//! alike the first surfaces seen there are (normal, colour, depth), so noise melts while
//! edges, creases and textures stay. The light is divided by the surface colour before
//! filtering and multiplied back after, so a texture is not blurred with the noise.
//!
//! It is a preview aid, not a substitute for samples: fine detail in shadows and
//! reflections softens.

use super::vector::V;

/// B3-spline weights of the 5-tap kernel.
const KERNEL: [f32; 5] = [1.0 / 16.0, 1.0 / 4.0, 3.0 / 8.0, 1.0 / 4.0, 1.0 / 16.0];
const ITERATIONS: usize = 5;

/// `color` (linear, one per pixel, `width` × `height`) smoothed, guided by `features(i)`
/// (albedo, normal, depth of the first surface hit at pixel `i`), on `threads` threads (0
/// = one per core). The features are read where they are, not copied, to keep memory
/// down on big pictures.
pub fn denoise<F>(color: &[V], features: &F, width: usize, height: usize, threads: usize) -> Vec<V>
where
    F: Fn(usize) -> (V, V, f32) + Sync,
{
    let n = width * height;
    if color.len() != n || n == 0 {
        return color.to_vec();
    }
    let albedo = |i: usize| features(i).0.max(V::splat(0.02));
    let mut light: Vec<V> = color
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let a = albedo(i);
            V::new(c.x / a.x, c.y / a.y, c.z / a.z)
        })
        .collect();
    let threads = match threads {
        0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
        t => t,
    }
    .clamp(1, 64);
    // How different the light may be, relative to its level, before a neighbour stops
    // counting; tightened at each wider step.
    let mut sigma_light = 0.6f32;
    for iteration in 0..ITERATIONS {
        let step = 1usize << iteration;
        let src = &light;
        let rows_per = height.div_ceil(threads);
        let out: Vec<Vec<V>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|t| {
                    scope.spawn(move || {
                        let y0 = t * rows_per;
                        let y1 = ((t + 1) * rows_per).min(height);
                        let mut out = Vec::with_capacity(y1.saturating_sub(y0) * width);
                        for y in y0..y1 {
                            for x in 0..width {
                                out.push(filter_pixel(
                                    src,
                                    features,
                                    width,
                                    height,
                                    x,
                                    y,
                                    step,
                                    sigma_light,
                                ));
                            }
                        }
                        out
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        let next: Vec<V> = out.into_iter().flatten().collect();
        if next.len() == n {
            light = next;
        }
        sigma_light *= 0.5;
    }
    light
        .iter()
        .enumerate()
        .map(|(i, l)| *l * albedo(i))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn filter_pixel<F: Fn(usize) -> (V, V, f32)>(
    light: &[V],
    features: &F,
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    step: usize,
    sigma_light: f32,
) -> V {
    let i = y * width + x;
    let Some(&centre) = light.get(i) else {
        return V::ZERO;
    };
    let (albedo, normal, depth) = features(i);
    let level = centre.luminance().max(1e-3);
    let has_normal = normal.length() > 0.5;
    let mut sum = V::ZERO;
    let mut weights = 0.0f32;
    for (ky, wy) in KERNEL.iter().enumerate() {
        let sy = y as isize + (ky as isize - 2) * step as isize;
        if sy < 0 || sy >= height as isize {
            continue;
        }
        for (kx, wx) in KERNEL.iter().enumerate() {
            let sx = x as isize + (kx as isize - 2) * step as isize;
            if sx < 0 || sx >= width as isize {
                continue;
            }
            let j = sy as usize * width + sx as usize;
            let Some(&l) = light.get(j) else {
                continue;
            };
            let (a, nn, d) = features(j);
            // Background against surface, or two different surfaces: no mixing.
            let w_normal = match (has_normal, nn.length() > 0.5) {
                (false, false) => 1.0,
                (true, true) => normal.dot(nn).max(0.0).powi(16),
                _ => 0.0,
            };
            if w_normal <= 1e-4 {
                continue;
            }
            let da = albedo - a;
            let w_albedo = (-da.dot(da) / 0.02).exp();
            let w_depth = (-(depth - d).abs() / (0.02 * depth.abs().max(1e-3))).exp();
            let dl = (l.luminance() - centre.luminance()).abs() / level;
            let w_light = (-dl / sigma_light.max(1e-3)).exp();
            let w = wx * wy * w_normal * w_albedo * w_depth * w_light;
            sum += l * w;
            weights += w;
        }
    }
    if weights > 0.0 { sum / weights } else { centre }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_drops_and_edges_stay() {
        let (w, h) = (32, 16);
        // Left half: a surface facing +Z, right half one facing +X; light 0.5 and 0.2
        // with noise.
        let mut s = 7u32;
        let mut noise = || {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
            ((s >> 8) as f32 / 16_777_216.0 - 0.5) * 0.3
        };
        let mut color = Vec::new();
        let mut features = Vec::new();
        for _y in 0..h {
            for x in 0..w {
                let left = x < w / 2;
                let base = if left { 0.5 } else { 0.2 };
                color.push(V::splat(base + noise() * base));
                features.push((
                    V::splat(0.8),
                    if left {
                        V::new(0.0, 0.0, 1.0)
                    } else {
                        V::new(1.0, 0.0, 0.0)
                    },
                    100.0,
                ));
            }
        }
        let out = denoise(&color, &|i: usize| features[i], w, h, 2);
        let spread = |v: &[V], x0: usize, x1: usize| {
            let vals: Vec<f32> = (0..h)
                .flat_map(|y| (x0..x1).map(move |x| y * w + x))
                .map(|i| v[i].x)
                .collect();
            let mean = vals.iter().sum::<f32>() / vals.len() as f32;
            let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32;
            (mean, var.sqrt())
        };
        let (_, before) = spread(&color, 0, w / 2);
        let (mean_left, after) = spread(&out, 0, w / 2);
        assert!(after < before * 0.5, "{before} -> {after}");
        // The edge column keeps its own side's level.
        let (mean_right, _) = spread(&out, w / 2, w);
        assert!((mean_left - 0.5).abs() < 0.03 && (mean_right - 0.2).abs() < 0.02);
        for y in 0..h {
            assert!((out[y * w + w / 2 - 1].x - 0.5).abs() < 0.08);
            assert!((out[y * w + w / 2].x - 0.2).abs() < 0.05);
        }
    }
}
