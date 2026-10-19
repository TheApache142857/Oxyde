//! Pictures: shaded views drawn by the software renderer (several views tiled in one
//! image, several layers in their own colours, faces and edges highlighted), and flat plots
//! of sections.

use oxyde_kernel::Mesh;
use oxyde_render::{Camera, Highlight, Material, Pick, Rgb, Scene, StandardView, Style};
use serde_json::Value;

use crate::args::{self, Result};
use crate::inspect::Section;

/// Meshes drawn together, each in its colour.
pub(crate) struct Layer {
    pub mesh: Mesh,
    /// `None`: the style's part colour.
    pub color: Option<Rgb>,
    /// 1 opaque, 0 invisible.
    pub opacity: f32,
}

/// A direction to look from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum View {
    Standard(StandardView),
    /// Yaw and pitch in radians (0, 0 is the Front view).
    Angles(f64, f64),
}

/// `"iso"`, `"front"`… or `{"yaw": deg, "pitch": deg}`, or a list of them.
pub(crate) fn views(value: Option<&Value>) -> Result<Vec<View>> {
    let Some(value) = value else {
        return Ok(vec![View::Standard(StandardView::Isometric)]);
    };
    let one = |v: &Value| -> Result<View> {
        if let Some(name) = v.as_str() {
            return Ok(View::Standard(match name {
                "iso" | "isometric" => StandardView::Isometric,
                "front" => StandardView::Front,
                "back" => StandardView::Back,
                "top" => StandardView::Top,
                "bottom" => StandardView::Bottom,
                "left" => StandardView::Left,
                "right" => StandardView::Right,
                other => return Err(format!("unknown view \"{other}\"")),
            }));
        }
        let yaw = args::f64(v, "yaw")?.to_radians();
        let pitch = args::f64(v, "pitch")?.to_radians();
        Ok(View::Angles(yaw, pitch))
    };
    args::list(value).into_iter().map(one).collect()
}

/// A box to zoom on, in model coordinates: `[min, max]`.
pub(crate) type Region = [[f64; 3]; 2];

/// Draws the layers from each view into a `width` x `height` tile, the tiles side by side
/// (two columns from two views on), and returns the PNG. With a `region`, each view is
/// fitted to that box rather than to the whole.
pub(crate) fn render(
    layers: &[Layer],
    views: &[View],
    width: usize,
    height: usize,
    highlight: &[Pick],
    region: Option<Region>,
) -> Result<Vec<u8>> {
    let mut mesh = Mesh::default();
    let mut materials = Vec::new();
    for layer in layers {
        mesh.faces.extend(layer.mesh.faces.iter().cloned());
        mesh.edges.extend(layer.mesh.edges.iter().cloned());
        materials.extend(layer.mesh.faces.iter().map(|_| Material {
            color: layer.color,
            opacity: layer.opacity,
            ..Material::default()
        }));
    }
    let mut scene = Scene::from_mesh(&mesh);
    scene.set_materials(materials, Vec::new());
    let style = Style {
        edge_width: 1.0,
        ..Style::default()
    };
    let highlight = Highlight {
        selected: highlight.to_vec(),
        ..Highlight::default()
    };
    let columns = if views.len() > 1 { 2 } else { 1 };
    let rows = views.len().div_ceil(columns).max(1);
    let (full_w, full_h) = (width * columns, height * rows);
    let mut image = vec![255u8; full_w * full_h * 4];
    for (k, view) in views.iter().enumerate() {
        let mut camera = Camera::default();
        match view {
            View::Standard(v) => camera.set_view(*v),
            View::Angles(yaw, pitch) => {
                camera.yaw = *yaw;
                camera.pitch = *pitch;
                camera.roll = 0.0;
            }
        }
        match region {
            Some([a, b]) => {
                let corners = (0..8).map(|k| {
                    [
                        if k & 1 == 0 { a[0] } else { b[0] },
                        if k & 2 == 0 { a[1] } else { b[1] },
                        if k & 4 == 0 { a[2] } else { b[2] },
                    ]
                });
                fit(&mut camera, corners, width as f64, height as f64);
            }
            None => {
                let points = mesh.faces.iter().flat_map(|f| {
                    f.positions
                        .iter()
                        .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
                });
                fit(&mut camera, points, width as f64, height as f64);
            }
        }
        let frame = oxyde_render::render_styled(&scene, &camera, width, height, &highlight, &style);
        let (ox, oy) = ((k % columns) * width, (k / columns) * height);
        for y in 0..height {
            let src = y * width * 4;
            let dst = ((oy + y) * full_w + ox) * 4;
            if let (Some(from), Some(to)) = (
                frame.rgba.get(src..src + width * 4),
                image.get_mut(dst..dst + width * 4),
            ) {
                to.copy_from_slice(from);
            }
        }
    }
    encode(&image, full_w, full_h)
}

/// Centres the points and zooms so they fill the view, from their extent across the screen
/// (tighter than the camera's own fit, which allows for any orientation).
fn fit(camera: &mut Camera, points: impl Iterator<Item = [f64; 3]>, width: f64, height: f64) {
    let (right, up) = (camera.right(), camera.up());
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    let mut count = 0.0;
    let mut box_lo = [f64::INFINITY; 3];
    let mut box_hi = [f64::NEG_INFINITY; 3];
    for p in points {
        let (u, v) = (dot(p, right), dot(p, up));
        lo = [lo[0].min(u), lo[1].min(v)];
        hi = [hi[0].max(u), hi[1].max(v)];
        for ((l, h), x) in box_lo.iter_mut().zip(box_hi.iter_mut()).zip(p) {
            *l = l.min(x);
            *h = h.max(x);
        }
        count += 1.0;
    }
    if count == 0.0 {
        return;
    }
    let center = [
        (box_lo[0] + box_hi[0]) / 2.0,
        (box_lo[1] + box_hi[1]) / 2.0,
        (box_lo[2] + box_hi[2]) / 2.0,
    ];
    // Shift the target across the screen to the middle of the extent.
    let (cu, cv) = (dot(center, right), dot(center, up));
    let (mu, mv) = ((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
    camera.target = [
        center[0] + right[0] * (mu - cu) + up[0] * (mv - cv),
        center[1] + right[1] * (mu - cu) + up[1] * (mv - cv),
        center[2] + right[2] * (mu - cu) + up[2] * (mv - cv),
    ];
    let span_w = (hi[0] - lo[0]).max(1e-3);
    let span_h = (hi[1] - lo[1]).max(1e-3);
    camera.height = (span_h).max(span_w * height / width) * 1.08;
}

/// A section drawn flat: outer boundaries black, holes blue, the plane's origin as a red
/// cross; returns the PNG. `window` (`[min, max]` in the plane) zooms on part of it.
pub(crate) fn plot_section(
    section: &Section,
    width: usize,
    height: usize,
    window: Option<[[f64; 2]; 2]>,
) -> Result<Vec<u8>> {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    if let Some([a, b]) = window {
        lo = [a[0].min(b[0]), a[1].min(b[1])];
        hi = [a[0].max(b[0]), a[1].max(b[1])];
    } else {
        for p in section.loops.iter().flat_map(|l| l.polygon.iter()) {
            lo = [lo[0].min(p.x), lo[1].min(p.y)];
            hi = [hi[0].max(p.x), hi[1].max(p.y)];
        }
        // The origin is always in the picture.
        lo = [lo[0].min(0.0), lo[1].min(0.0)];
        hi = [hi[0].max(0.0), hi[1].max(0.0)];
    }
    let span = ((hi[0] - lo[0]) / width as f64).max((hi[1] - lo[1]) / height as f64) * 1.06;
    let span = if span > 0.0 { span } else { 1.0 };
    let (cx, cy) = ((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
    let to_pixel = |x: f64, y: f64| {
        (
            width as f64 / 2.0 + (x - cx) / span,
            height as f64 / 2.0 - (y - cy) / span,
        )
    };
    let mut image = vec![255u8; width * height * 4];
    let mut plot = |x: f64, y: f64, color: [u8; 3]| {
        if x < 0.0 || y < 0.0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= width || y >= height {
            return;
        }
        let k = (y * width + x) * 4;
        if let Some(px) = image.get_mut(k..k + 3) {
            px.copy_from_slice(&color);
        }
    };
    let line =
        |a: (f64, f64), b: (f64, f64), color: [u8; 3], plot: &mut dyn FnMut(f64, f64, [u8; 3])| {
            let steps = ((b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil() as usize).max(1);
            for k in 0..=steps {
                let t = k as f64 / steps as f64;
                plot(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, color);
            }
        };
    for l in &section.loops {
        let color = if l.depth % 2 == 0 {
            [20, 20, 30]
        } else {
            [20, 90, 220]
        };
        let n = l.polygon.len();
        for i in 0..n {
            let (Some(a), Some(b)) = (l.polygon.get(i), l.polygon.get((i + 1) % n)) else {
                continue;
            };
            line(to_pixel(a.x, a.y), to_pixel(b.x, b.y), color, &mut plot);
        }
    }
    let o = to_pixel(0.0, 0.0);
    line((o.0 - 8.0, o.1), (o.0 + 8.0, o.1), [220, 30, 30], &mut plot);
    line((o.0, o.1 - 8.0), (o.0, o.1 + 8.0), [220, 30, 30], &mut plot);
    encode(&image, width, height)
}

fn encode(rgba: &[u8], width: usize, height: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgba).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}
