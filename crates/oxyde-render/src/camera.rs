use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

use crate::vec3::{V3, add, cross, dot, length, scale, sub};

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
///
/// The orientation is three angles: `yaw` and `pitch` place the viewer, and `roll` turns
/// the view about the line of sight. With `roll` at 0 the model's Y axis points up the
/// screen (a *constrained* orbit, as SolidWorks keeps it); a *free* orbit
/// ([`Camera::orbit_free_about`]) turns about the screen's own axes and so can tilt the
/// model any way, which `roll` records.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub target: V3,
    /// Rotation around the vertical (Y) axis, radians. 0 = looking from +Z (Front).
    pub yaw: f64,
    /// Elevation, radians, from −π/2 (from below) to π/2 (from above).
    pub pitch: f64,
    /// Rotation about the line of sight, radians; 0 keeps Y up the screen.
    pub roll: f64,
    pub height: f64,
}

impl Default for Camera {
    fn default() -> Self {
        let mut camera = Camera {
            target: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
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
        self.roll = 0.0;
    }

    /// Turns the camera to look against `normal` (the viewer ends up on the side `normal`
    /// points to), as SolidWorks' "Normal To".
    pub fn look_along(&mut self, normal: V3) {
        let len = length(normal);
        if len < 1e-12 {
            return;
        }
        let n = scale(normal, 1.0 / len);
        self.pitch = n[1].clamp(-1.0, 1.0).asin();
        self.yaw = n[0].atan2(n[2]);
        self.roll = 0.0;
    }

    /// Puts the model's Y axis back up the screen, undoing any tilt of a free orbit.
    pub fn level(&mut self) {
        self.roll = 0.0;
    }

    /// Whether the view is tilted (not level): a free orbit has rolled it.
    pub fn is_tilted(&self) -> bool {
        self.roll.abs() > 1e-9
    }

    /// The viewing ray through pixel (`x`, `y`): a point on it and its direction (away from
    /// the viewer).
    pub fn ray(&self, x: f64, y: f64, width: f64, height: f64) -> (V3, V3) {
        let s = self.pixels_per_mm(height);
        let point = add(
            self.target,
            add(
                scale(self.right(), (x - width / 2.0) / s),
                scale(self.up(), -(y - height / 2.0) / s),
            ),
        );
        (point, scale(self.eye_direction(), -1.0))
    }

    /// Unit vector from the target towards the viewer.
    pub fn eye_direction(&self) -> V3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        [cp * sy, sp, cp * cy]
    }

    /// The right direction of the level view (roll 0): horizontal in the model.
    fn level_right(&self) -> V3 {
        let (sy, cy) = self.yaw.sin_cos();
        [cy, 0.0, -sy]
    }

    /// Unit vector pointing to the right of the screen.
    pub fn right(&self) -> V3 {
        let right = self.level_right();
        if self.roll == 0.0 {
            return right;
        }
        let up = cross(self.eye_direction(), right);
        let (s, c) = self.roll.sin_cos();
        add(scale(right, c), scale(up, s))
    }

    /// Unit vector pointing to the top of the screen.
    pub fn up(&self) -> V3 {
        cross(self.eye_direction(), self.right())
    }

    /// Sets the three angles from an eye direction and a right direction (unit, at a
    /// right angle to it).
    fn set_basis(&mut self, eye: V3, right: V3) {
        self.pitch = eye[1].clamp(-1.0, 1.0).asin();
        self.yaw = eye[0].atan2(eye[2]);
        let level_right = self.level_right();
        let level_up = cross(eye, level_right);
        self.roll = dot(right, level_up).atan2(dot(right, level_right));
        if self.roll.abs() < 1e-12 {
            self.roll = 0.0;
        }
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

    /// Rotates the view as if dragging the model by `dx`, `dy` pixels, about the centre of
    /// the view.
    pub fn orbit(&mut self, dx: f64, dy: f64) {
        let pivot = self.target;
        self.orbit_about(dx, dy, pivot);
    }

    /// Rotates the view as if dragging the model by `dx`, `dy` pixels, about `pivot` (a
    /// model point, e.g. the one under the pointer when the drag started): it stays where
    /// it is on screen, as in SolidWorks and Creo.
    pub fn orbit_about(&mut self, dx: f64, dy: f64, pivot: V3) {
        const RADIANS_PER_PIXEL: f64 = 0.008;
        // Where the pivot is on screen, relative to the centre.
        let v = sub(pivot, self.target);
        let (sx, sy) = (dot(v, self.right()), dot(v, self.up()));
        self.yaw -= dx * RADIANS_PER_PIXEL;
        self.pitch = (self.pitch + dy * RADIANS_PER_PIXEL).clamp(-FRAC_PI_2, FRAC_PI_2);
        // The centre that puts the pivot back at the same place on screen.
        self.target = sub(pivot, add(scale(self.right(), sx), scale(self.up(), sy)));
    }

    /// A free orbit: turns the view about the screen's own vertical and horizontal
    /// axes (as Fusion 360 with the constrained orbit off, or a trackball), so the model
    /// can be tilted any way. `pivot` stays where it is on screen.
    pub fn orbit_free_about(&mut self, dx: f64, dy: f64, pivot: V3) {
        const RADIANS_PER_PIXEL: f64 = 0.008;
        let v = sub(pivot, self.target);
        let (sx, sy) = (dot(v, self.right()), dot(v, self.up()));
        let (right, up, eye) = (self.right(), self.up(), self.eye_direction());
        // Dragging right turns the model to the right (the viewer goes the other way);
        // dragging down brings the viewer up over the model, as in the constrained orbit.
        // Turning the eye about `right` by a positive angle takes it towards -up (right ×
        // eye = -up), so a downward drag (dy > 0) needs the negative angle.
        let eye = rotate(eye, up, -dx * RADIANS_PER_PIXEL);
        let eye = rotate(eye, right, -dy * RADIANS_PER_PIXEL);
        let right = rotate(right, up, -dx * RADIANS_PER_PIXEL);
        let eye = unit(eye);
        // Keep the basis orthonormal whatever rounding did.
        let right = unit(sub(right, scale(eye, dot(right, eye))));
        self.set_basis(eye, right);
        self.target = sub(pivot, add(scale(self.right(), sx), scale(self.up(), sy)));
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

/// `v` turned by `angle` radians about the unit axis `axis` (Rodrigues).
fn rotate(v: V3, axis: V3, angle: f64) -> V3 {
    let (s, c) = angle.sin_cos();
    add(
        add(scale(v, c), scale(cross(axis, v), s)),
        scale(axis, dot(axis, v) * (1.0 - c)),
    )
}

fn unit(v: V3) -> V3 {
    let len = length(v);
    if len < 1e-12 { v } else { scale(v, 1.0 / len) }
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
    fn orbiting_about_a_pivot_keeps_it_on_screen() {
        let mut camera = Camera::default();
        let (w, h) = (300.0, 200.0);
        let pivot = [12.0, -7.0, 30.0];
        let before = camera.project(pivot, w, h);
        camera.orbit_about(40.0, -25.0, pivot);
        let after = camera.project(pivot, w, h);
        assert!((after[0] - before[0]).abs() < 1e-9 && (after[1] - before[1]).abs() < 1e-9);
        // And the view did turn.
        assert!((camera.yaw - FRAC_PI_4).abs() > 0.1);
    }

    #[test]
    fn free_orbit_tilts_the_view_and_level_undoes_it() {
        let mut camera = Camera::default();
        camera.set_view(StandardView::Front);
        let (w, h) = (300.0, 200.0);
        let pivot = [12.0, -7.0, 30.0];
        let before = camera.project(pivot, w, h);
        // Turning about the screen's vertical from the front is a plain yaw: still level.
        camera.orbit_free_about(50.0, 0.0, pivot);
        assert!(!camera.is_tilted());
        assert!(close(camera.up(), [0.0, 1.0, 0.0]));
        let after = camera.project(pivot, w, h);
        assert!((after[0] - before[0]).abs() < 1e-9 && (after[1] - before[1]).abs() < 1e-9);
        // Dragging down lifts the viewer over the model, exactly as the constrained orbit
        // does from the same view (the free orbit must not feel upside down).
        let mut constrained = Camera::default();
        constrained.set_view(StandardView::Front);
        constrained.orbit_about(50.0, 60.0, pivot);
        camera.orbit_free_about(0.0, 60.0, pivot);
        assert!(camera.eye_direction()[1] > 0.1);
        assert!(close(camera.eye_direction(), constrained.eye_direction()));
        // Then about the vertical again: the model's Y is no longer up the screen, which
        // a constrained orbit can never do.
        camera.orbit_free_about(70.0, 0.0, pivot);
        assert!(camera.is_tilted());
        assert!(!close(camera.up(), [0.0, 1.0, 0.0]));
        // The basis stays orthonormal.
        let (r, u, e) = (camera.right(), camera.up(), camera.eye_direction());
        assert!(dot(r, u).abs() < 1e-9 && dot(r, e).abs() < 1e-9 && dot(u, e).abs() < 1e-9);
        assert!((length(r) - 1.0).abs() < 1e-9 && (length(u) - 1.0).abs() < 1e-9);
        // Levelling keeps the viewer where it is and puts Y back up.
        let eye = camera.eye_direction();
        camera.level();
        assert!(close(camera.eye_direction(), eye));
        assert!(camera.up()[1] > 0.0 && camera.right()[1].abs() < 1e-9);
    }

    #[test]
    fn a_rolled_view_round_trips_through_its_basis() {
        let mut camera = Camera::default();
        camera.roll = 0.7;
        let (eye, right) = (camera.eye_direction(), camera.right());
        let mut other = Camera::default();
        other.set_basis(eye, right);
        assert!((other.roll - 0.7).abs() < 1e-9);
        assert!(close(other.right(), right) && close(other.eye_direction(), eye));
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
    fn looking_along_a_normal_and_casting_rays() {
        let mut camera = Camera::default();
        camera.look_along([0.0, 0.0, 5.0]);
        assert!(close(camera.eye_direction(), [0.0, 0.0, 1.0]));
        let (w, h) = (200.0, 100.0);
        let p = [3.0, -2.0, 7.0];
        let screen = camera.project(p, w, h);
        let (origin, direction) = camera.ray(screen[0], screen[1], w, h);
        // The ray passes through the point: same position across the view.
        let to_p = sub(p, origin);
        let along = dot(to_p, direction);
        assert!(length(sub(to_p, scale(direction, along))) < 1e-9);
    }

    #[test]
    fn fit_centres_the_box() {
        let mut camera = Camera::default();
        camera.fit([0.0, 0.0, 0.0], [40.0, 20.0, 10.0], 800.0, 600.0);
        assert!(close(camera.target, [20.0, 10.0, 5.0]));
        assert!(camera.height > 40.0);
    }
}
