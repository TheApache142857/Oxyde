//! The part's edges as seen from a sketch: the straight, circular and arc edges lying in the
//! sketch plane, in sketch coordinates. Sketch mode snaps to their end points, middles and
//! centres, and Convert Entities copies them into the sketch, so that new geometry can be
//! placed exactly on existing (or imported) geometry.

use oxyde_geom::{Plane, Point2, Point3};
use oxyde_kernel::{CurveKind, Topology};

/// Edges further than this from the sketch plane are left out (mm).
const ON_PLANE: f64 = 1e-4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModelPointKind {
    /// End of an edge.
    Vertex,
    /// Middle of a straight edge.
    Middle,
    /// Centre of a circular edge.
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModelCurve {
    Line(Point2, Point2),
    Circle(Point2, f64),
    /// Counter-clockwise from `start` to `end`.
    Arc {
        center: Point2,
        start: Point2,
        end: Point2,
    },
}

/// The part geometry a sketch can refer to.
#[derive(Debug, Clone, Default)]
pub struct ModelGeometry {
    pub points: Vec<(Point2, ModelPointKind)>,
    pub curves: Vec<ModelCurve>,
}

fn local(plane: &Plane, p: Point3) -> Point2 {
    let v = p - plane.origin;
    Point2::new(v.dot(plane.x_dir), v.dot(plane.y_dir()))
}

fn on_plane(plane: &Plane, p: Point3) -> bool {
    (p - plane.origin).dot(plane.normal).abs() < ON_PLANE
}

/// Whether `p` lies on the counter-clockwise arc from `start` to `end` around `center`.
fn on_ccw_arc(center: Point2, start: Point2, end: Point2, p: Point2) -> bool {
    let angle = |q: Point2| (q.y - center.y).atan2(q.x - center.x);
    let tau = std::f64::consts::TAU;
    let sweep = (angle(end) - angle(start)).rem_euclid(tau);
    let at = (angle(p) - angle(start)).rem_euclid(tau);
    at <= sweep
}

impl ModelGeometry {
    pub fn from_body(topology: &Topology, plane: &Plane) -> Self {
        let mut out = Self::default();
        let vertex = |i: u32| topology.vertices.get(i as usize).map(|v| v.point);
        for edge in &topology.edges {
            let ends: Vec<Point3> = edge.vertices.iter().filter_map(|i| vertex(*i)).collect();
            if !ends.iter().all(|p| on_plane(plane, *p)) || !on_plane(plane, edge.midpoint) {
                continue;
            }
            match edge.curve {
                CurveKind::Line if ends.len() == 2 => {
                    let (a, b) = (local(plane, ends[0]), local(plane, ends[1]));
                    out.curves.push(ModelCurve::Line(a, b));
                    out.add(a, ModelPointKind::Vertex);
                    out.add(b, ModelPointKind::Vertex);
                    out.add(local(plane, edge.midpoint), ModelPointKind::Middle);
                }
                CurveKind::Circle => {
                    let Some(axis) = edge.axis else { continue };
                    // Its axis along the normal: a circle standing across the plane has its
                    // vertex and its middle on it too (two ends of a diameter).
                    let flat = axis.direction.cross(plane.normal).length() < 1e-6;
                    if !flat || !on_plane(plane, axis.origin) {
                        continue;
                    }
                    let center = local(plane, axis.origin);
                    let middle = local(plane, edge.midpoint);
                    let radius = center.distance(middle);
                    out.add(center, ModelPointKind::Center);
                    if ends.len() == 2 && ends[0].distance(ends[1]) > 1e-9 {
                        let (mut start, mut end) = (local(plane, ends[0]), local(plane, ends[1]));
                        if !on_ccw_arc(center, start, end, middle) {
                            std::mem::swap(&mut start, &mut end);
                        }
                        out.curves.push(ModelCurve::Arc { center, start, end });
                        out.add(start, ModelPointKind::Vertex);
                        out.add(end, ModelPointKind::Vertex);
                    } else {
                        out.curves.push(ModelCurve::Circle(center, radius));
                    }
                }
                _ => {
                    for p in &ends {
                        out.add(local(plane, *p), ModelPointKind::Vertex);
                    }
                }
            }
        }
        out
    }

    fn add(&mut self, p: Point2, kind: ModelPointKind) {
        if !self.points.iter().any(|(q, _)| q.distance(p) < 1e-9) {
            self.points.push((p, kind));
        }
    }
}

#[cfg(test)]
mod tests {
    use oxyde_geom::{Axis, Plane, Point2, Point3, Vec3};
    use oxyde_kernel::{CurveKind, EdgeInfo, Topology, VertexInfo};

    use super::{ModelCurve, ModelGeometry, ModelPointKind};

    fn edge(
        curve: CurveKind,
        vertices: Vec<u32>,
        midpoint: Point3,
        axis: Option<Axis>,
    ) -> EdgeInfo {
        EdgeInfo {
            curve,
            length: 0.0,
            midpoint,
            axis,
            vertices,
            faces: Vec::new(),
        }
    }

    #[test]
    fn edges_in_the_sketch_plane_become_references() {
        // On the Top plane (XZ): a line and a hole in it, and an edge above it (left out).
        let topology = Topology {
            faces: Vec::new(),
            vertices: [
                (0.0, 0.0, 0.0),
                (10.0, 0.0, 0.0),
                (0.0, 5.0, 0.0),
                (3.0, 0.0, 4.0),
            ]
            .map(|(x, y, z)| VertexInfo {
                point: Point3::new(x, y, z),
            })
            .to_vec(),
            edges: vec![
                edge(
                    CurveKind::Line,
                    vec![0, 1],
                    Point3::new(5.0, 0.0, 0.0),
                    None,
                ),
                edge(
                    CurveKind::Line,
                    vec![0, 2],
                    Point3::new(0.0, 2.5, 0.0),
                    None,
                ),
                edge(
                    CurveKind::Circle,
                    vec![3],
                    Point3::new(3.0, 0.0, 5.0),
                    Axis::new(Point3::new(3.0, 0.0, 6.0), Vec3::Y),
                ),
                // A circle standing across the plane: its vertex and middle are on it.
                edge(
                    CurveKind::Circle,
                    vec![1],
                    Point3::new(20.0, 0.0, 0.0),
                    Axis::new(Point3::new(15.0, 0.0, 0.0), Vec3::Z),
                ),
            ],
        };
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP);
        assert_eq!(geometry.curves.len(), 2);
        assert!(matches!(geometry.curves[1], ModelCurve::Circle(_, r) if (r - 1.0).abs() < 1e-9));
        let centers: Vec<Point2> = geometry
            .points
            .iter()
            .filter(|(_, k)| *k == ModelPointKind::Center)
            .map(|(p, _)| *p)
            .collect();
        assert_eq!(centers.len(), 1);
        assert_eq!(
            geometry
                .points
                .iter()
                .filter(|(_, k)| *k == ModelPointKind::Middle)
                .count(),
            1
        );
    }
}
