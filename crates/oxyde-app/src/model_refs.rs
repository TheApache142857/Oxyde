//! The part's edges as seen from a sketch, in sketch coordinates. Sketch mode snaps to the end
//! points, middles and centres of the straight, circular and arc edges lying in the sketch
//! plane. Convert Entities copies those edges into the sketch, and also projects the part's
//! other edges and its vertices onto the plane along its normal, as SolidWorks does, so that
//! new geometry can be placed exactly on existing (or imported) geometry.

use oxyde_geom::{Axis, Plane, Point2, Point3};
use oxyde_kernel::{CurveKind, EdgeInfo, Topology};

/// Edges further than this from the sketch plane are projected onto it, not snapped to (mm).
const ON_PLANE: f64 = 1e-4;

/// Two projected points closer than this are one (mm).
const SAME_POINT: f64 = 1e-9;

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
    /// Points to snap to: those of the edges lying in the sketch plane.
    pub points: Vec<(Point2, ModelPointKind)>,
    /// Curves Convert Entities can copy: the edges lying in the sketch plane first, then the
    /// other edges projected onto it. A projection that repeats a curve already listed is
    /// left out, so the top and bottom edges of a box seen from above give one curve per
    /// side.
    pub curves: Vec<ModelCurve>,
    /// Every vertex of the part projected onto the sketch plane, once each, for Convert
    /// Entities to copy as a fixed point.
    pub vertices: Vec<Point2>,
}

fn local(plane: &Plane, p: Point3) -> Point2 {
    let v = p - plane.origin;
    Point2::new(v.dot(plane.x_dir), v.dot(plane.y_dir()))
}

fn on_plane(plane: &Plane, p: Point3) -> bool {
    (p - plane.origin).dot(plane.normal).abs() < ON_PLANE
}

/// Whether a circle about `axis` lies in planes parallel to the sketch plane, so that it
/// projects onto it as a circle of the same radius.
fn flat(plane: &Plane, axis: &Axis) -> bool {
    axis.direction.cross(plane.normal).length() < 1e-6
}

/// Whether `p` lies on the counter-clockwise arc from `start` to `end` around `center`.
fn on_ccw_arc(center: Point2, start: Point2, end: Point2, p: Point2) -> bool {
    let angle = |q: Point2| (q.y - center.y).atan2(q.x - center.x);
    let tau = std::f64::consts::TAU;
    let sweep = (angle(end) - angle(start)).rem_euclid(tau);
    let at = (angle(p) - angle(start)).rem_euclid(tau);
    at <= sweep
}

/// A circular edge about a flat axis, seen in the sketch plane: an arc between its two ends,
/// counter-clockwise through its middle, or a whole circle.
fn round_curve(plane: &Plane, axis: &Axis, edge: &EdgeInfo, ends: &[Point3]) -> ModelCurve {
    let center = local(plane, axis.origin);
    let middle = local(plane, edge.midpoint);
    match ends {
        [a, b] if a.distance(*b) > 1e-9 => {
            let (mut start, mut end) = (local(plane, *a), local(plane, *b));
            if !on_ccw_arc(center, start, end, middle) {
                std::mem::swap(&mut start, &mut end);
            }
            ModelCurve::Arc { center, start, end }
        }
        _ => ModelCurve::Circle(center, center.distance(middle)),
    }
}

impl ModelCurve {
    /// Its two ends, or none for a full circle.
    fn ends(&self) -> Option<[Point2; 2]> {
        match *self {
            ModelCurve::Line(a, b) => Some([a, b]),
            ModelCurve::Arc { start, end, .. } => Some([start, end]),
            ModelCurve::Circle(..) => None,
        }
    }

    /// Whether `other` is the same curve: a line either way round, a circle, or an arc (both
    /// counter-clockwise, so their ends compare in order).
    fn same_as(&self, other: &ModelCurve) -> bool {
        let near = |a: Point2, b: Point2| a.distance(b) < SAME_POINT;
        match (*self, *other) {
            (ModelCurve::Line(a, b), ModelCurve::Line(c, d)) => {
                (near(a, c) && near(b, d)) || (near(a, d) && near(b, c))
            }
            (ModelCurve::Circle(a, r), ModelCurve::Circle(b, s)) => {
                near(a, b) && (r - s).abs() < SAME_POINT
            }
            (
                ModelCurve::Arc { center, start, end },
                ModelCurve::Arc {
                    center: c,
                    start: s,
                    end: e,
                },
            ) => near(center, c) && near(start, s) && near(end, e),
            _ => false,
        }
    }
}

impl ModelGeometry {
    /// The curves joined end to end with curve `index`, itself included, in no particular
    /// order: a face's outline in the sketch plane, or its projection, for Convert Entities
    /// to copy at once.
    pub fn chain_of(&self, index: usize) -> Vec<usize> {
        let near = |a: Point2, b: Point2| a.distance(b) < 1e-6;
        let mut chain = vec![index];
        let mut k = 0;
        while let Some(&current) = chain.get(k) {
            k += 1;
            let Some(ends) = self.curves.get(current).and_then(ModelCurve::ends) else {
                continue;
            };
            for (i, curve) in self.curves.iter().enumerate() {
                if chain.contains(&i) {
                    continue;
                }
                let joined = curve
                    .ends()
                    .is_some_and(|other| ends.iter().any(|a| other.iter().any(|b| near(*a, *b))));
                if joined {
                    chain.push(i);
                }
            }
        }
        chain
    }

    pub fn from_body(topology: &Topology, plane: &Plane) -> Self {
        let mut out = Self::default();
        let vertex = |i: u32| topology.vertices.get(i as usize).map(|v| v.point);
        // The edges in the plane first, so that their curves keep the first places.
        let mut away = Vec::new();
        for edge in &topology.edges {
            let ends: Vec<Point3> = edge.vertices.iter().filter_map(|i| vertex(*i)).collect();
            if !ends.iter().all(|p| on_plane(plane, *p)) || !on_plane(plane, edge.midpoint) {
                away.push((edge, ends));
                continue;
            }
            match (edge.curve, ends.as_slice()) {
                (CurveKind::Line, [p, q]) => {
                    let (a, b) = (local(plane, *p), local(plane, *q));
                    out.curves.push(ModelCurve::Line(a, b));
                    out.add(a, ModelPointKind::Vertex);
                    out.add(b, ModelPointKind::Vertex);
                    out.add(local(plane, edge.midpoint), ModelPointKind::Middle);
                }
                (CurveKind::Circle, _) => {
                    let Some(axis) = edge.axis else { continue };
                    // Its axis along the normal: a circle standing across the plane has its
                    // vertex and its middle on it too (two ends of a diameter).
                    if !flat(plane, &axis) || !on_plane(plane, axis.origin) {
                        continue;
                    }
                    out.add(local(plane, axis.origin), ModelPointKind::Center);
                    let curve = round_curve(plane, &axis, edge, &ends);
                    if let ModelCurve::Arc { start, end, .. } = curve {
                        out.add(start, ModelPointKind::Vertex);
                        out.add(end, ModelPointKind::Vertex);
                    }
                    out.curves.push(curve);
                }
                _ => {
                    for p in &ends {
                        out.add(local(plane, *p), ModelPointKind::Vertex);
                    }
                }
            }
        }
        // The other edges projected along the normal. A line square to the plane projects to
        // its vertex alone. A circle tilted to the plane would project to an ellipse, and
        // other curves to splines, which sketches cannot hold: those give their vertices only.
        for (edge, ends) in away {
            let curve = match (edge.curve, ends.as_slice()) {
                (CurveKind::Line, [p, q]) => {
                    let (a, b) = (local(plane, *p), local(plane, *q));
                    (a.distance(b) >= SAME_POINT).then_some(ModelCurve::Line(a, b))
                }
                (CurveKind::Circle, _) => edge
                    .axis
                    .filter(|axis| flat(plane, axis))
                    .map(|axis| round_curve(plane, &axis, edge, &ends)),
                _ => None,
            };
            if let Some(curve) = curve
                && !out.curves.iter().any(|c| c.same_as(&curve))
            {
                out.curves.push(curve);
            }
        }
        for v in &topology.vertices {
            let p = local(plane, v.point);
            if !out.vertices.iter().any(|q| q.distance(p) < SAME_POINT) {
                out.vertices.push(p);
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

    #[test]
    fn edges_and_vertices_off_the_plane_are_projected_onto_it() {
        // On the Top plane a model point (x, y, z) projects to sketch (x, -z): the plane's x
        // direction is +X and its y direction is normal × x = -Z.
        let topology = Topology {
            faces: Vec::new(),
            vertices: [
                (0.0, 0.0, 0.0),
                (10.0, 0.0, 0.0),
                // The edge 0-1 again, 5 mm above the plane.
                (0.0, 5.0, 0.0),
                (10.0, 5.0, 0.0),
                // A slanted edge above the plane.
                (2.0, 7.0, 3.0),
                (8.0, 9.0, 3.0),
                // Ends of a half circle of radius 2 about a vertical axis, 9 mm up.
                (7.0, 9.0, 5.0),
                (3.0, 9.0, 5.0),
                // The vertex of a circle tilted to the plane.
                (20.0, 5.0, 0.0),
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
                    vec![2, 3],
                    Point3::new(5.0, 5.0, 0.0),
                    None,
                ),
                // Square to the plane: its vertex only.
                edge(
                    CurveKind::Line,
                    vec![0, 2],
                    Point3::new(0.0, 2.5, 0.0),
                    None,
                ),
                edge(
                    CurveKind::Line,
                    vec![4, 5],
                    Point3::new(5.0, 8.0, 3.0),
                    None,
                ),
                edge(
                    CurveKind::Circle,
                    vec![6, 7],
                    Point3::new(5.0, 9.0, 7.0),
                    Axis::new(Point3::new(5.0, 9.0, 5.0), Vec3::Y),
                ),
                // Tilted: it would project to an ellipse.
                edge(
                    CurveKind::Circle,
                    vec![8],
                    Point3::new(20.0, 1.0, 0.0),
                    Axis::new(Point3::new(20.0, 3.0, 0.0), Vec3::X),
                ),
                edge(
                    CurveKind::BSpline,
                    vec![4, 6],
                    Point3::new(5.0, 8.0, 4.0),
                    None,
                ),
            ],
        };
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP);
        let near = |a: Point2, b: Point2| a.distance(b) < 1e-9;
        // The edge in the plane, the slanted edge and the half circle: the copy of edge 0-1
        // above the plane is the same curve and is listed once.
        assert_eq!(geometry.curves.len(), 3, "{:?}", geometry.curves);
        assert!(matches!(
            geometry.curves[0],
            ModelCurve::Line(a, b) if near(a, Point2::new(0.0, 0.0)) && near(b, Point2::new(10.0, 0.0))
        ));
        assert!(matches!(
            geometry.curves[1],
            ModelCurve::Line(a, b) if near(a, Point2::new(2.0, -3.0)) && near(b, Point2::new(8.0, -3.0))
        ));
        let ModelCurve::Arc { center, start, end } = geometry.curves[2] else {
            panic!(
                "the half circle projects to an arc: {:?}",
                geometry.curves[2]
            );
        };
        assert!(near(center, Point2::new(5.0, -5.0)));
        assert!((start.distance(center) - 2.0).abs() < 1e-9);
        assert!((end.distance(center) - 2.0).abs() < 1e-9);
        // Counter-clockwise through the projected middle (5, -7).
        assert!(super::on_ccw_arc(
            center,
            start,
            end,
            Point2::new(5.0, -7.0)
        ));
        // Snapping stays on the plane: the ends and middle of edge 0-1, nothing projected.
        assert_eq!(geometry.points.len(), 3);
        assert!(
            geometry
                .points
                .iter()
                .all(|(_, k)| *k != ModelPointKind::Center)
        );
        // Every vertex once: 2 and 3 project onto 0 and 1.
        let expected = [
            (0.0, 0.0),
            (10.0, 0.0),
            (2.0, -3.0),
            (8.0, -3.0),
            (7.0, -5.0),
            (3.0, -5.0),
            (20.0, 0.0),
        ];
        assert_eq!(geometry.vertices.len(), expected.len());
        for (x, y) in expected {
            assert!(
                geometry
                    .vertices
                    .iter()
                    .any(|v| near(*v, Point2::new(x, y))),
                "({x}, {y}) in {:?}",
                geometry.vertices
            );
        }
    }

    #[test]
    fn a_face_above_the_plane_projects_to_an_outline_converted_at_once() {
        // The top face of a 10 x 4 box standing 6 mm tall on the Top plane, alone.
        let corners = [
            (0.0, 6.0, 0.0),
            (10.0, 6.0, 0.0),
            (10.0, 6.0, 4.0),
            (0.0, 6.0, 4.0),
        ];
        let topology = Topology {
            faces: Vec::new(),
            vertices: corners
                .map(|(x, y, z)| VertexInfo {
                    point: Point3::new(x, y, z),
                })
                .to_vec(),
            edges: (0..4u32)
                .map(|i| {
                    let (a, b) = (corners[i as usize], corners[(i as usize + 1) % 4]);
                    edge(
                        CurveKind::Line,
                        vec![i, (i + 1) % 4],
                        Point3::new((a.0 + b.0) / 2.0, 6.0, (a.2 + b.2) / 2.0),
                        None,
                    )
                })
                .collect(),
        };
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP);
        assert_eq!(geometry.curves.len(), 4);
        let mut chain = geometry.chain_of(2);
        chain.sort_unstable();
        assert_eq!(chain, vec![0, 1, 2, 3]);
    }
}
