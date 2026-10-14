use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

use crate::vec3::{V3, add, dot, length, scale, sub};

/// Named orientations, as in SolidWorks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardView {
    Front,
    Back,
    Top,
    Bottom,
    Left,
    Right,
    Isometric,
}

/// Orthographic orbit camera.
///
/// The viewer sits in direction [`Camera::eye_direction`] from `target`, which is the point
/// at the centre of the view. `height` is how many millimetres the view spans vertically.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub target: V3,
    /// Rotation around the vertical (Y) axis, radians. 0 = looking from +Z (Front).
    pub yaw: f64,
    /// Elevation, radians, from −π/2 (from below) to π/2 (from above).
    pub pitch: f64,
    pub height: f64,
}

impl Default for Camera {
    fn default() -> Self {
        let mut camera = Camera {
            target: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            height: 100.0,
        };
        camera.set_view(StandardView::Isometric);
        camera
    }
}

impl Camera {
    pub fn set_view(&mut self, view: StandardView) {
        let (yaw, pitch) = match view {
            StandardView::Front => (0.0, 0.0),
            StandardView::Back => (PI, 0.0),
            StandardView::Top => (0.0, FRAC_PI_2),
            StandardView::Bottom => (0.0, -FRAC_PI_2),
            StandardView::Left => (-FRAC_PI_2, 0.0),
            StandardView::Right => (FRAC_PI_2, 0.0),
            StandardView::Isometric => (FRAC_PI_4, (1.0f64 / 3.0f64.sqrt()).asin()),
        };
        self.yaw = yaw;
        self.pitch = pitch;
    }

    /// Unit vector from the target towards the viewer.
    pub fn eye_direction(&self) -> V3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        [cp * sy, sp, cp * cy]
    }

    /// Unit vector pointing to the right of the screen.
    pub fn right(&self) -> V3 {
        let (sy, cy) = self.yaw.sin_cos();
        [cy, 0.0, -sy]
    }

    /// Unit vector pointing to the top of the screen.
    pub fn up(&self) -> V3 {
        crate::vec3::cross(self.eye_direction(), self.right())
    }

    /// Direction towards the key light: above and to the left of the viewer, so that the
    /// faces of a part get different shades (with a headlight alone, the three visible faces
    /// of a box in the isometric view look identical).
    pub fn key_light(&self) -> V3 {
        let raw = add(
            add(scale(self.eye_direction(), 0.6), scale(self.up(), 0.6)),
            scale(self.right(), -0.5),
        );
        scale(raw, 1.0 / length(raw))
    }

    /// Pixels per millimetre for a viewport `viewport_height` pixels tall.
    pub fn pixels_per_mm(&self, viewport_height: f64) -> f64 {
        viewport_height / self.height
    }

    /// Screen position in pixels (origin top-left) and depth in mm (smaller = closer).
    pub fn project(&self, p: V3, width: f64, height: f64) -> V3 {
        let v = sub(p, self.target);
        let s = self.pixels_per_mm(height);
        [
            width / 2.0 + dot(v, self.right()) * s,
            height / 2.0 - dot(v, self.up()) * s,
            -dot(v, self.eye_direction()),
        ]
    }

    /// Rotates the view as if dragging the model by `dx`, `dy` pixels.
    pub fn orbit(&mut self, dx: f64, dy: f64) {
        const RADIANS_PER_PIXEL: f64 = 0.008;
        self.yaw -= dx * RADIANS_PER_PIXEL;
        self.pitch = (self.pitch + dy * RADIANS_PER_PIXEL).clamp(-FRAC_PI_2, FRAC_PI_2);
    }

    /// Moves the view so the model follows a drag of `dx`, `dy` pixels.
    pub fn pan(&mut self, dx: f64, dy: f64, viewport_height: f64) {
        let s = self.pixels_per_mm(viewport_height);
        self.target = add(
            sub(self.target, scale(self.right(), dx / s)),
            scale(self.up(), dy / s),
        );
    }

    /// Zooms by `factor` (< 1 zooms in) keeping the point under `cursor` (pixels) fixed.
    pub fn zoom_at(&mut self, factor: f64, cursor: [f64; 2], width: f64, height: f64) {
        let s = self.pixels_per_mm(height);
        let ox = (cursor[0] - width / 2.0) / s;
        let oy = -(cursor[1] - height / 2.0) / s;
        let new_height = (self.height * factor).clamp(1e-3, 1e7);
        let k = 1.0 - new_height / self.height;
        self.target = add(
            self.target,
            add(scale(self.right(), ox * k), scale(self.up(), oy * k)),
        );
        self.height = new_height;
    }

    /// Centres the box `min`..`max` and zooms so it fits a `width` × `height` viewport
    /// whatever the orientation.
    pub fn fit(&mut self, min: V3, max: V3, width: f64, height: f64) {
        self.target = scale(add(min, max), 0.5);
        let diagonal = length(sub(max, min)).max(1e-3);
        let aspect = if width > 0.0 {
            (height / width).max(1.0)
        } else {
            1.0
        };
        self.height = diagonal * 1.15 * aspect;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: V3, b: V3) -> bool {
        length(sub(a, b)) < 1e-9
    }

    #[test]
    fn standard_views_match_solidworks() {
        let mut camera = Camera::default();
        camera.set_view(StandardView::Front);
        assert!(close(camera.right(), [1.0, 0.0, 0.0]));
        assert!(close(camera.up(), [0.0, 1.0, 0.0]));

        camera.set_view(StandardView::Top);
        assert!(close(camera.right(), [1.0, 0.0, 0.0]));
        assert!(close(camera.up(), [0.0, 0.0, -1.0]));

        camera.set_view(StandardView::Right);
        assert!(close(camera.right(), [0.0, 0.0, -1.0]));
        assert!(close(camera.up(), [0.0, 1.0, 0.0]));
    }

    #[test]
    fn target_projects_to_the_centre() {
        let camera = Camera {
            target: [5.0, 6.0, 7.0],
            ..Camera::default()
        };
        let p = camera.project([5.0, 6.0, 7.0], 200.0, 100.0);
        assert!((p[0] - 100.0).abs() < 1e-9 && (p[1] - 50.0).abs() < 1e-9);
    }

    #[test]
    fn closer_points_have_smaller_depth() {
        let mut camera = Camera::default();
        camera.set_view(StandardView::Front);
        let near = camera.project([0.0, 0.0, 10.0], 100.0, 100.0);
        let far = camera.project([0.0, 0.0, -10.0], 100.0, 100.0);
        assert!(near[2] < far[2]);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let mut camera = Camera::default();
        let (w, h) = (300.0, 200.0);
        let cursor = [250.0, 40.0];
        // The model point currently under the cursor.
        let s = camera.pixels_per_mm(h);
        let under = add(
            camera.target,
            add(
                scale(camera.right(), (cursor[0] - w / 2.0) / s),
                scale(camera.up(), -(cursor[1] - h / 2.0) / s),
            ),
        );
        camera.zoom_at(0.5, cursor, w, h);
        let p = camera.project(under, w, h);
        assert!((p[0] - cursor[0]).abs() < 1e-6 && (p[1] - cursor[1]).abs() < 1e-6);
        assert!((camera.height - 50.0).abs() < 1e-9);
    }

    #[test]
    fn pan_moves_the_model_with_the_mouse() {
        let mut camera = Camera::default();
        let before = camera.project([1.0, 2.0, 3.0], 100.0, 100.0);
        camera.pan(10.0, -4.0, 100.0);
        let after = camera.project([1.0, 2.0, 3.0], 100.0, 100.0);
        assert!((after[0] - before[0] - 10.0).abs() < 1e-9);
        assert!((after[1] - before[1] + 4.0).abs() < 1e-9);
    }

    #[test]
    fn fit_centres_the_box() {
        let mut camera = Camera::default();
        camera.fit([0.0, 0.0, 0.0], [40.0, 20.0, 10.0], 800.0, 600.0);
        assert!(close(camera.target, [20.0, 10.0, 5.0]));
        assert!(camera.height > 40.0);
    }
}
