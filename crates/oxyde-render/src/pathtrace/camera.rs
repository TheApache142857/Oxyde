//! The render camera: the viewport's orbit camera seen through a lens. With a focal
//! length it is a perspective camera placed so that the point at the centre of the view
//! keeps its size on screen; without one it is orthographic, exactly the viewport's view.

use super::vector::V;
use crate::camera::Camera;

/// Height of a full-frame (35 mm) sensor, to turn focal lengths into angles of view as
/// photographers and SolidWorks Visualize state them.
const SENSOR_HEIGHT_MM: f64 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Projection {
    /// Tangent of half the vertical angle of view.
    Perspective { tan_half_fov: f32 },
    /// Millimetres the view spans vertically.
    Orthographic { height: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderCamera {
    /// Where rays start (the eye, for a perspective camera; the centre of the view for an
    /// orthographic one, from which rays are backed off before the scene).
    pub eye: V,
    /// Unit vectors: into the view, to the right, up the image.
    pub forward: V,
    pub right: V,
    pub up: V,
    pub projection: Projection,
}

impl RenderCamera {
    /// The viewport camera `camera`, with a lens of `focal_length` mm (full-frame
    /// equivalent), or orthographic with `None`.
    pub fn from_view(camera: &Camera, focal_length: Option<f64>) -> RenderCamera {
        let eye_dir = V::from_f64(camera.eye_direction());
        let target = V::from_f64(camera.target);
        let forward = -eye_dir;
        let right = V::from_f64(camera.right());
        let up = V::from_f64(camera.up());
        match focal_length.filter(|f| f.is_finite() && *f > 0.0) {
            Some(f) => {
                let tan_half = SENSOR_HEIGHT_MM / 2.0 / f;
                // Far enough that the view spans `camera.height` at the target.
                let distance = camera.height / 2.0 / tan_half;
                RenderCamera {
                    eye: target + eye_dir * distance as f32,
                    forward,
                    right,
                    up,
                    projection: Projection::Perspective {
                        tan_half_fov: tan_half as f32,
                    },
                }
            }
            None => RenderCamera {
                eye: target,
                forward,
                right,
                up,
                projection: Projection::Orthographic {
                    height: camera.height as f32,
                },
            },
        }
    }

    /// The ray through image point (`x`, `y`) (pixels, from the top left, fractions
    /// allowed) of a `width` × `height` image: origin and unit direction. `back` is how
    /// far an orthographic ray starts behind the centre of the view, to be in front of
    /// everything.
    pub fn ray(&self, x: f32, y: f32, width: f32, height: f32, back: f32) -> (V, V) {
        let h = height.max(1.0);
        let u = (x - width / 2.0) / h;
        let v = -(y - height / 2.0) / h;
        match self.projection {
            Projection::Perspective { tan_half_fov } => {
                let k = 2.0 * tan_half_fov;
                let dir = (self.forward + self.right * (u * k) + self.up * (v * k)).normalized();
                (self.eye, dir)
            }
            Projection::Orthographic { height } => {
                let origin = self.eye + self.right * (u * height) + self.up * (v * height)
                    - self.forward * back;
                (origin, self.forward)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::StandardView;

    #[test]
    fn the_centre_of_the_view_keeps_its_place_and_size() {
        let mut camera = Camera::default();
        camera.set_view(StandardView::Front);
        camera.target = [10.0, 20.0, 0.0];
        camera.height = 50.0;
        let (w, h) = (300.0, 200.0);
        for focal in [None, Some(50.0), Some(24.0)] {
            let rc = RenderCamera::from_view(&camera, focal);
            // The centre ray goes through the target.
            let (o, d) = rc.ray(w / 2.0, h / 2.0, w, h, 100.0);
            let to = V::from_f64(camera.target) - o;
            let along = to.dot(d);
            assert!((to - d * along).length() < 1e-3, "{focal:?}");
            // The top edge of the image, at the target's depth, is height / 2 above it.
            let (o, d) = rc.ray(w / 2.0, 0.0, w, h, 100.0);
            let s = (V::from_f64(camera.target).z - o.z) / d.z;
            let p = o + d * s;
            assert!((p.y - 45.0).abs() < 1e-2, "{focal:?}: {}", p.y);
            assert!((p.x - 10.0).abs() < 1e-2);
        }
        // The perspective eye looks down -Z from the front.
        let rc = RenderCamera::from_view(&camera, Some(50.0));
        assert!(rc.eye.z > 0.0 && (rc.forward.z + 1.0).abs() < 1e-6);
    }
}
