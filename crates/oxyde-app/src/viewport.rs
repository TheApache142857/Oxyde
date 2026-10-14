//! The 3D viewport: shows the part, handles camera navigation and picking.
//!
//! Mouse, part mode: left-drag orbits, middle-drag or Shift+left-drag pans, the wheel zooms
//! at the cursor, left-click selects (Ctrl+click adds to the selection).
//!
//! Sketch mode keeps the left button for drawing: middle-drag pans, Ctrl+middle-drag orbits,
//! and picking of the 3D model is off.

use eframe::egui;
use oxyde_geom::{Plane, Point2, Point3};
use oxyde_render::{Camera, Frame, Highlight, Pick, Scene, StandardView, render};

/// What happened in the viewport this frame.
pub struct ViewportOutput {
    pub response: egui::Response,
    /// `Some(pick)` when the user clicked; the pick is `None` on empty space.
    pub clicked: Option<Option<Pick>>,
    /// Ctrl was held during the click.
    pub additive: bool,
    /// Where the viewport is on screen, and the screen scale, for overlays.
    pub rect: egui::Rect,
    pub pixels_per_point: f32,
}

impl ViewportOutput {
    fn size_px(&self) -> (f64, f64) {
        let ppp = self.pixels_per_point as f64;
        (
            self.rect.width() as f64 * ppp,
            self.rect.height() as f64 * ppp,
        )
    }
}

/// Everything the current image depends on: re-render only when this changes.
#[derive(Clone, PartialEq)]
struct RenderKey {
    camera: Camera,
    size: [usize; 2],
    highlight: Highlight,
    scene: u64,
}

pub struct Viewport {
    camera: Camera,
    scene: Scene,
    scene_version: u64,
    frame: Option<Frame>,
    texture: Option<egui::TextureHandle>,
    rendered: Option<RenderKey>,
    hovered: Option<Pick>,
    fit_pending: bool,
}

impl Viewport {
    pub fn new() -> Self {
        Self {
            camera: Camera::default(),
            scene: Scene::default(),
            scene_version: 0,
            frame: None,
            texture: None,
            rendered: None,
            hovered: None,
            fit_pending: true,
        }
    }

    pub fn set_scene(&mut self, scene: Scene, fit: bool) {
        self.scene = scene;
        self.scene_version += 1;
        self.hovered = None;
        if fit {
            self.fit_pending = true;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.scene.is_empty()
    }

    pub fn request_fit(&mut self) {
        self.fit_pending = true;
    }

    pub fn set_view(&mut self, view: StandardView) {
        self.camera.set_view(view);
        self.fit_pending = true;
    }

    /// Looks straight at a plane (SolidWorks' "Normal To").
    pub fn look_at_plane(&mut self, plane: &Plane) {
        let n = plane.normal;
        self.camera.look_along([n.x, n.y, n.z]);
        self.fit_pending = true;
    }

    /// Screen position of a model-space point.
    pub fn to_screen(&self, p: Point3, out: &ViewportOutput) -> egui::Pos2 {
        let (w, h) = out.size_px();
        let q = self.camera.project([p.x, p.y, p.z], w, h);
        let ppp = out.pixels_per_point;
        out.rect.min + egui::vec2(q[0] as f32 / ppp, q[1] as f32 / ppp)
    }

    /// Where the viewing ray under a screen position meets `plane`, in plane coordinates.
    pub fn to_plane(&self, pos: egui::Pos2, out: &ViewportOutput, plane: &Plane) -> Option<Point2> {
        let (w, h) = out.size_px();
        let ppp = out.pixels_per_point as f64;
        let local = pos - out.rect.min;
        let (origin, direction) = self
            .camera
            .ray(local.x as f64 * ppp, local.y as f64 * ppp, w, h);
        let n = [plane.normal.x, plane.normal.y, plane.normal.z];
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let facing = dot(direction, n);
        if facing.abs() < 1e-9 {
            return None;
        }
        let o = [plane.origin.x, plane.origin.y, plane.origin.z];
        let t = dot([o[0] - origin[0], o[1] - origin[1], o[2] - origin[2]], n) / facing;
        let hit = Point3::new(
            origin[0] + direction[0] * t,
            origin[1] + direction[1] * t,
            origin[2] + direction[2] * t,
        );
        let v = hit - plane.origin;
        Some(Point2::new(v.dot(plane.x_dir), v.dot(plane.y_dir())))
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        selected: Vec<Pick>,
        hint: Option<&str>,
        sketching: bool,
    ) -> ViewportOutput {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        let width = ((rect.width() * ppp).round() as usize).max(1);
        let height = ((rect.height() * ppp).round() as usize).max(1);
        let (w, h) = (width as f64, height as f64);

        if self.fit_pending {
            if let Some((lo, hi)) = self.scene.bounds() {
                self.camera.fit(lo, hi, w, h);
                self.fit_pending = false;
            }
        }

        // Navigation.
        let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.ctrl));
        let delta = response.drag_delta();
        let middle = response.dragged_by(egui::PointerButton::Middle);
        let primary = response.dragged_by(egui::PointerButton::Primary);
        let (panning, orbiting) = if sketching {
            (middle && !ctrl, middle && ctrl)
        } else {
            let panning = middle || (shift && primary);
            (panning, !panning && primary)
        };
        if panning {
            self.camera
                .pan(delta.x as f64 * ppp as f64, delta.y as f64 * ppp as f64, h);
        } else if orbiting {
            self.camera.orbit(delta.x as f64, delta.y as f64);
        }

        let pointer = response.hover_pos().map(|p| {
            let v = (p - rect.min) * ppp;
            [v.x as f64, v.y as f64]
        });
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                if let Some(cursor) = pointer {
                    let factor = (-(scroll as f64) * 0.0015).exp();
                    self.camera.zoom_at(factor, cursor, w, h);
                }
            }
        }

        // Picking uses the previous image, which is what the user is looking at.
        self.hovered = match (&self.frame, pointer, panning || orbiting || sketching) {
            (Some(frame), Some(cursor), false)
                if frame.width == width && frame.height == height =>
            {
                frame.pick(cursor[0], cursor[1], 4.0 * ppp as f64)
            }
            _ => None,
        };
        let clicked = (response.clicked() && !sketching).then_some(self.hovered);
        let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);

        let key = RenderKey {
            camera: self.camera,
            size: [width, height],
            highlight: Highlight {
                hovered: self.hovered,
                selected,
            },
            scene: self.scene_version,
        };
        if self.rendered.as_ref() != Some(&key) {
            let frame = render(&self.scene, &self.camera, width, height, &key.highlight);
            let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &frame.rgba);
            if let Some(texture) = self.texture.as_mut() {
                texture.set(image, egui::TextureOptions::NEAREST);
            } else {
                self.texture = Some(ui.ctx().load_texture(
                    "oxyde-viewport",
                    image,
                    egui::TextureOptions::NEAREST,
                ));
            }
            self.frame = Some(frame);
            self.rendered = Some(key);
        }

        let painter = ui.painter_at(rect);
        if let Some(texture) = &self.texture {
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        self.draw_triad(&painter, rect);
        if let Some(hint) = hint {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                hint,
                egui::FontId::proportional(16.0),
                egui::Color32::from_gray(90),
            );
        }

        ViewportOutput {
            response,
            clicked,
            additive,
            rect,
            pixels_per_point: ppp,
        }
    }

    /// Small X/Y/Z axes in the lower-left corner, showing the current orientation.
    fn draw_triad(&self, painter: &egui::Painter, rect: egui::Rect) {
        let origin = egui::pos2(rect.left() + 45.0, rect.bottom() - 45.0);
        let (right, up) = (self.camera.right(), self.camera.up());
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let axes = [
            ([1.0, 0.0, 0.0], "X", egui::Color32::from_rgb(210, 50, 50)),
            ([0.0, 1.0, 0.0], "Y", egui::Color32::from_rgb(40, 150, 60)),
            ([0.0, 0.0, 1.0], "Z", egui::Color32::from_rgb(50, 90, 220)),
        ];
        for (axis, label, color) in axes {
            let d = egui::vec2(dot(axis, right) as f32, -dot(axis, up) as f32) * 30.0;
            painter.line_segment([origin, origin + d], egui::Stroke::new(2.0, color));
            painter.text(
                origin + d * 1.3,
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(12.0),
                color,
            );
        }
    }
}
