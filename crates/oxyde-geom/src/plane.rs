use crate::{Point2, Point3, Vec3};

/// A right-handed planar frame: `x_dir` and `normal` are orthonormal and
/// `y_dir = normal × x_dir`, so a loop that is counter-clockwise in plane coordinates
/// faces along `normal`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    pub origin: Point3,
    pub x_dir: Vec3,
    pub normal: Vec3,
}

impl Plane {
    /// SolidWorks "Front": the XY plane, normal +Z.
    pub const FRONT: Plane = Plane {
        origin: Point3::ORIGIN,
        x_dir: Vec3::X,
        normal: Vec3::Z,
    };

    /// SolidWorks "Top": the XZ plane, normal +Y (sketch Y points towards −Z).
    pub const TOP: Plane = Plane {
        origin: Point3::ORIGIN,
        x_dir: Vec3::X,
        normal: Vec3::Y,
    };

    /// SolidWorks "Right": the YZ plane, normal +X (sketch X points towards −Z).
    pub const RIGHT: Plane = Plane {
        origin: Point3::ORIGIN,
        x_dir: Vec3::new(0.0, 0.0, -1.0),
        normal: Vec3::X,
    };

    pub fn y_dir(&self) -> Vec3 {
        self.normal.cross(self.x_dir)
    }

    /// Converts plane coordinates to model coordinates.
    pub fn to_world(&self, p: Point2) -> Point3 {
        self.origin + self.x_dir * p.x + self.y_dir() * p.y
    }

    /// The same frame moved by `distance` along the normal.
    pub fn offset(&self, distance: f64) -> Plane {
        Plane {
            origin: self.origin + self.normal * distance,
            ..*self
        }
    }

    /// A plane through `point` with the given `normal`, with a deterministic frame:
    /// the origin is the point of the plane closest to the model origin, and the x axis is
    /// the model axis least aligned with the normal (the first of X, Y, Z on a tie, within
    /// the angular tolerance), projected onto the plane.
    ///
    /// Determinism matters: sketches placed on faces must land in the same place every
    /// rebuild, even after the face has been resized.
    pub fn from_point_normal(point: Point3, normal: Vec3) -> Option<Plane> {
        let normal = normal.normalized()?;
        let origin = Point3::ORIGIN + normal * point.to_vec().dot(normal);

        // Near-ties go to the first of X, Y, Z: the normals of rebuilt geometry carry
        // rounding noise (a face normal to X has Y and Z tied but for 1e-16), which must
        // not turn the frame, and the sketch with it, by 90° from one rebuild to the next.
        let axes = [Vec3::X, Vec3::Y, Vec3::Z];
        let alignment = |a: Vec3| a.dot(normal).abs();
        let least = axes
            .into_iter()
            .map(alignment)
            .fold(f64::INFINITY, f64::min);
        let axis = axes
            .into_iter()
            .find(|a| alignment(*a) <= least + crate::ANGULAR_TOLERANCE)?;
        let x_dir = (axis - normal * axis.dot(normal)).normalized()?;
        Some(Plane {
            origin,
            x_dir,
            normal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point3, b: Point3) -> bool {
        a.distance(b) < 1e-9
    }

    trait Distance {
        fn distance_to_vec(self, other: Vec3) -> f64;
    }

    impl Distance for Vec3 {
        fn distance_to_vec(self, other: Vec3) -> f64 {
            (self - other).length()
        }
    }

    #[test]
    fn standard_planes_are_right_handed() {
        assert_eq!(Plane::FRONT.y_dir(), Vec3::Y);
        assert_eq!(Plane::TOP.y_dir(), Vec3::new(0.0, 0.0, -1.0));
        assert_eq!(Plane::RIGHT.y_dir(), Vec3::Y);
    }

    #[test]
    fn plane_from_top_face_of_a_box() {
        let plane = Plane::from_point_normal(Point3::new(20.0, 10.0, 10.0), Vec3::Z).unwrap();
        assert!(close(plane.origin, Point3::new(0.0, 0.0, 10.0)));
        assert_eq!(plane.x_dir, Vec3::X);
        assert!(close(
            plane.to_world(Point2::new(20.0, 10.0)),
            Point3::new(20.0, 10.0, 10.0)
        ));
    }

    #[test]
    fn rounding_noise_in_the_normal_does_not_turn_the_frame() {
        // A face normal to X, as rebuilt geometry gives it: Y and Z tie, whatever the noise.
        let a = Plane::from_point_normal(Point3::ORIGIN, Vec3::new(1.0, 2e-16, 1e-16)).unwrap();
        let b = Plane::from_point_normal(Point3::ORIGIN, Vec3::new(1.0, 1e-16, 2e-16)).unwrap();
        assert!(
            a.x_dir.distance_to_vec(b.x_dir) < 1e-12,
            "{:?} vs {:?}",
            a.x_dir,
            b.x_dir
        );
        let c = Plane::from_point_normal(Point3::ORIGIN, Vec3::new(-6e-17, 1.0, 1e-16)).unwrap();
        let d = Plane::from_point_normal(Point3::ORIGIN, Vec3::new(1e-16, 1.0, -6e-17)).unwrap();
        assert!(
            c.x_dir.distance_to_vec(d.x_dir) < 1e-12,
            "{:?} vs {:?}",
            c.x_dir,
            d.x_dir
        );
    }

    #[test]
    fn plane_frame_does_not_depend_on_the_point_within_the_plane() {
        let a = Plane::from_point_normal(Point3::new(1.0, 2.0, 5.0), Vec3::Z).unwrap();
        let b = Plane::from_point_normal(Point3::new(-7.0, 3.0, 5.0), Vec3::Z).unwrap();
        assert_eq!(a, b);
    }
}
