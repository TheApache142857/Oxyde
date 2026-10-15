//! The 3D viewport: shows the part, handles camera navigation and picking.
//!
//! Mouse, part mode: left-drag orbits, middle-drag or Shift+left-drag pans, the wheel zooms
//! at the cursor, left-click selects (Ctrl+click adds to the selection).
//!
//! Sketch mode keeps the left button for drawing: middle-drag pans, Ctrl+middle-drag orbits,
//! and picking of the 3D model is off.
//!
//! In an assembly, Alt+left-drag grabs what is under the pointer instead of orbiting
//! (Alt+Shift+left-drag to turn it); see [`Grab`].

use eframe::egui;
use oxyde_geom::{Plane, Point2, Point3};
use oxyde_render::{Camera, Frame, Highlight, Pick, Scene, StandardView, Style, render_styled};

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
    /// An Alt+drag in progress, when grabbing is enabled.
    pub grab: Option<Grab>,
}

/// An Alt+left-drag this frame: what was under the pointer when it started, and how far
/// the pointer moved, in model terms.
#[derive(Debug, Clone, Copy)]
pub struct Grab {
    pub start: Option<Pick>,
    /// Movement this frame in mm, along the screen's right and up directions…
    pub along: [f64; 2],
    /// …which are these model directions.
    pub right: [f64; 3],
    pub up: [f64; 3],
    /// Shift held: turn rather than slide.
    pub rotate: bool,
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

/// Where an animated view change ends.
#[derive(Clone, Copy)]
enum Orientation {
    View(StandardView),
    /// Looking against this direction (Normal To).
    Normal([f64; 3]),
}

/// A view change waiting for the viewport size, which the end camera depends on.
#[derive(Clone, Copy)]
struct ViewRequest {
    orientation: Orientation,
    /// What to frame; the whole part when `None`.
    bounds: Option<([f64; 3], [f64; 3])>,
}

/// A camera move in progress.
#[derive(Clone, Copy)]
struct Animation {
    from: Camera,
    to: Camera,
    start: f64,
}

/// How the viewport looks and responds: set from the preferences.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewSettings {
    pub style: Style,
    pub show_triad: bool,
    /// 1 = default.
    pub orbit_speed: f64,
    pub zoom_speed: f64,
    pub invert_zoom: bool,
    /// Seconds a view change takes; 0 = at once.
    pub animation_time: f64,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            style: Style::default(),
            show_triad: true,
            orbit_speed: 1.0,
            zoom_speed: 1.0,
            invert_zoom: false,
            animation_time: 0.45,
        }
    }
}

/// The camera between `from` (t = 0) and `to` (t = 1): the shorter way round, zooming
/// geometrically.
fn interpolate(from: &Camera, to: &Camera, t: f64) -> Camera {
    use std::f64::consts::{PI, TAU};
    let turn = (to.yaw - from.yaw + PI).rem_euclid(TAU) - PI;
    let lerp = |a: f64, b: f64| a + (b - a) * t;
    Camera {
        target: [
            lerp(from.target[0], to.target[0]),
            lerp(from.target[1], to.target[1]),
            lerp(from.target[2], to.target[2]),
        ],
        yaw: from.yaw + turn * t,
        pitch: lerp(from.pitch, to.pitch),
        height: (lerp(from.height.ln(), to.height.ln())).exp(),
    }
}

/// Everything the current image depends on: re-render only when this changes.
#[derive(Clone, PartialEq)]
struct RenderKey {
    camera: Camera,
    size: [usize; 2],
    highlight: Highlight,
    scene: u64,
    style: Style,
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
    view_request: Option<ViewRequest>,
    animation: Option<Animation>,
    /// GPU renderer; the software renderer draws when there is none.
    gpu: Option<crate::gpu::GpuViewport>,
    gpu_texture: Option<egui::TextureId>,
    /// Last pick, reused while neither the pointer nor the image changed.
    last_pick: Option<([f64; 2], Option<Pick>)>,
    /// "GPU (adapter)" or "software", for the About window.
    renderer: String,
    /// Alt+left-drag grabs instead of orbiting (set by the assembly each frame).
    pub grab_enabled: bool,
    /// The pick under the pointer when the current grab started.
    grabbed: Option<Option<Pick>>,
    pub settings: ViewSettings,
    /// Triangles and edge segments of the scene.
    scene_counts: (usize, usize),
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
            view_request: None,
            animation: None,
            gpu: None,
            gpu_texture: None,
            last_pick: None,
            renderer: "software".to_owned(),
            grab_enabled: false,
            grabbed: None,
            settings: ViewSettings::default(),
            scene_counts: (0, 0),
        }
    }

    /// Draws with the GPU from now on.
    pub fn enable_gpu(&mut self, gpu: crate::gpu::GpuViewport, name: String) {
        if !self.scene.is_empty() {
            let mut gpu = gpu;
            gpu.set_mesh(&self.scene.gpu_mesh(), self.scene.bounds());
            self.gpu = Some(gpu);
        } else {
            self.gpu = Some(gpu);
        }
        self.renderer = name;
        self.rendered = None;
    }

    /// Which renderer draws the viewport.
    pub fn renderer_name(&self) -> &str {
        &self.renderer
    }

    pub fn set_scene(&mut self, scene: Scene, fit: bool) {
        let mesh = scene.gpu_mesh();
        self.scene_counts = (mesh.indices.len() / 3, mesh.segments.len());
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_mesh(&mesh, scene.bounds());
        }
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

    /// Turns smoothly to a standard view, framing the part.
    pub fn set_view(&mut self, view: StandardView) {
        self.view_request = Some(ViewRequest {
            orientation: Orientation::View(view),
            bounds: None,
        });
    }

    /// Turns smoothly to look straight at a plane (SolidWorks' "Normal To"), framing the
    /// part.
    pub fn look_at_plane(&mut self, plane: &Plane) {
        self.look_at_plane_framing(plane, None);
    }

    /// Turns smoothly to look straight at a plane, centring the box `bounds` (e.g. a
    /// sketch being opened), or the part when `None`.
    pub fn look_at_plane_framing(&mut self, plane: &Plane, bounds: Option<(Point3, Point3)>) {
        let n = plane.normal;
        self.view_request = Some(ViewRequest {
            orientation: Orientation::Normal([n.x, n.y, n.z]),
            bounds: bounds.map(|(a, b)| ([a.x, a.y, a.z], [b.x, b.y, b.z])),
        });
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

        // Animated view changes.
        let now = ui.input(|i| i.time);
        if let Some(request) = self.view_request.take() {
            let mut to = self.camera;
            match request.orientation {
                Orientation::View(view) => to.set_view(view),
                Orientation::Normal(n) => to.look_along(n),
            }
            if let Some((lo, hi)) = request.bounds.or_else(|| self.scene.bounds()) {
                to.fit(lo, hi, w, h);
            }
            self.fit_pending = false;
            self.animation = Some(Animation {
                from: self.camera,
                to,
                start: now,
            });
        }
        if let Some(animation) = self.animation {
            let duration = self.settings.animation_time;
            let t = if duration <= 0.0 {
                1.0
            } else {
                ((now - animation.start) / duration).clamp(0.0, 1.0)
            };
            let eased = t * t * (3.0 - 2.0 * t);
            self.camera = interpolate(&animation.from, &animation.to, eased);
            if t >= 1.0 {
                self.animation = None;
            } else {
                ui.ctx().request_repaint();
            }
        }

        // Navigation.
        let (shift, ctrl, alt) =
            ui.input(|i| (i.modifiers.shift, i.modifiers.ctrl, i.modifiers.alt));
        let delta = response.drag_delta();
        let middle = response.dragged_by(egui::PointerButton::Middle);
        let mut primary = response.dragged_by(egui::PointerButton::Primary);
        // Alt+drag grabs; it keeps grabbing until the button is released.
        if self.grab_enabled
            && alt
            && response.drag_started_by(egui::PointerButton::Primary)
            && self.grabbed.is_none()
        {
            self.grabbed = Some(self.hovered);
        }
        if !primary || !self.grab_enabled {
            self.grabbed = None;
        }
        let grab = match self.grabbed {
            Some(start) if primary => {
                primary = false;
                let scale = ppp as f64 / self.camera.pixels_per_mm(h);
                Some(Grab {
                    start,
                    along: [delta.x as f64 * scale, -(delta.y as f64) * scale],
                    right: self.camera.right(),
                    up: self.camera.up(),
                    rotate: shift,
                })
            }
            _ => None,
        };
        let (panning, orbiting) = if sketching {
            (middle && !ctrl, middle && ctrl)
        } else {
            let panning = middle || (shift && primary);
            (panning, !panning && primary)
        };
        if panning || orbiting {
            // The user takes over.
            self.animation = None;
        }
        if panning {
            self.camera
                .pan(delta.x as f64 * ppp as f64, delta.y as f64 * ppp as f64, h);
        } else if orbiting {
            let k = self.settings.orbit_speed;
            self.camera.orbit(delta.x as f64 * k, delta.y as f64 * k);
        }

        let pointer = response.hover_pos().map(|p| {
            let v = (p - rect.min) * ppp;
            [v.x as f64, v.y as f64]
        });
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.animation = None;
                if let Some(cursor) = pointer {
                    let sign = if self.settings.invert_zoom { 1.0 } else { -1.0 };
                    let rate = 0.0015 * self.settings.zoom_speed;
                    let factor = (sign * scroll as f64 * rate).exp();
                    self.camera.zoom_at(factor, cursor, w, h);
                }
            }
        }

        // Picking uses the previous image, which is what the user is looking at.
        let same_size = self
            .rendered
            .as_ref()
            .is_some_and(|k| k.size == [width, height]);
        let busy = panning || orbiting || grab.is_some() || sketching || !same_size;
        self.hovered = match (pointer, busy) {
            (Some(cursor), false) => match (&self.gpu, &self.frame) {
                (Some(gpu), _) => match self.last_pick {
                    Some((at, pick)) if at == cursor => pick,
                    _ => {
                        let pick = gpu.pick(cursor[0], cursor[1], 4.0 * ppp as f64);
                        self.last_pick = Some((cursor, pick));
                        pick
                    }
                },
                (None, Some(frame)) => frame.pick(cursor[0], cursor[1], 4.0 * ppp as f64),
                (None, None) => None,
            },
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
            style: self.settings.style,
        };
        let image_changed = self.rendered.as_ref().is_none_or(|k| {
            k.camera != key.camera
                || k.size != key.size
                || k.scene != key.scene
                || k.style != key.style
        });
        if image_changed {
            self.last_pick = None;
        }
        if let (Some(gpu), true) = (self.gpu.as_mut(), self.rendered.as_ref() != Some(&key)) {
            self.gpu_texture = Some(gpu.render(
                &self.camera,
                width as u32,
                height as u32,
                &key.highlight,
                &key.style,
            ));
            self.rendered = Some(key);
        } else if self.rendered.as_ref() != Some(&key) {
            let frame = render_styled(
                &self.scene,
                &self.camera,
                width,
                height,
                &key.highlight,
                &key.style,
            );
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
        let image = self.gpu_texture.or(self.texture.as_ref().map(|t| t.id()));
        if let Some(texture) = image {
            painter.image(
                texture,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        if self.settings.show_triad {
            self.draw_triad(&painter, rect);
        }
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
            grab,
        }
    }

    /// What is under the pointer now.
    pub fn hovered(&self) -> Option<Pick> {
        self.hovered
    }

    /// Faces (triangles) and edge segments in the scene, for the debug overlay.
    pub fn scene_size(&self) -> (usize, usize) {
        self.scene_counts
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
