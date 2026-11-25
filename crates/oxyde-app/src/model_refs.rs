//! The part's edges as seen from a sketch, in sketch coordinates. Sketch mode snaps to the end
//! points, middles and centres of the straight, circular and arc edges lying in the sketch
//! plane. Convert Entities copies those edges into the sketch, and also the part's other
//! edges and its vertices projected onto the plane (`oxyde_model::projection`), so that new
//! geometry can be placed exactly on existing (or imported) geometry.

use oxyde_geom::{Plane, Point2, Point3};
use oxyde_kernel::{CurveKind, EdgeCurve, Topology};
use oxyde_model::BodyNaming;
use oxyde_model::projection::{
    ModelCurve, ProjectionSource, SAME_POINT, flat, local, on_plane, project_edge,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModelPointKind {
    /// End of an edge.
    Vertex,
    /// Middle of a straight edge.
    Middle,
    /// Centre of a circular edge.
    Center,
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
    /// The edge (by index in the topology) each of `curves` comes from.
    pub curve_edges: Vec<usize>,
    /// Every vertex of the part projected onto the sketch plane, once each, for Convert
    /// Entities to copy as a fixed point.
    pub vertices: Vec<Point2>,
    /// The vertex (by index in the topology) each of `vertices` comes from.
    pub vertex_indices: Vec<u32>,
    /// The names of `curve_edges` and `vertex_indices` in the part, which a copy links to.
    pub curve_sources: Vec<Option<ProjectionSource>>,
    pub vertex_sources: Vec<Option<ProjectionSource>>,
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

    /// `curve_of` gives the kernel's exact curve of an edge (by index in `topology`), asked
    /// for the edges that are neither lines nor circles.
    pub fn from_body(
        topology: &Topology,
        plane: &Plane,
        curve_of: &dyn Fn(usize) -> Option<EdgeCurve>,
    ) -> Self {
        let mut out = Self::default();
        let vertex = |i: u32| topology.vertices.get(i as usize).map(|v| v.point);
        // The edges in the plane first, so that their curves keep the first places.
        let mut away = Vec::new();
        for (index, edge) in topology.edges.iter().enumerate() {
            let ends: Vec<Point3> = edge.vertices.iter().filter_map(|i| vertex(*i)).collect();
            if !ends.iter().all(|p| on_plane(plane, *p)) || !on_plane(plane, edge.midpoint) {
                away.push(index);
                continue;
            }
            let curve = project_edge(plane, topology, index, curve_of);
            match (edge.curve, ends.as_slice()) {
                (CurveKind::Line, [p, q]) => {
                    out.add(local(plane, *p), ModelPointKind::Vertex);
                    out.add(local(plane, *q), ModelPointKind::Vertex);
                    out.add(local(plane, edge.midpoint), ModelPointKind::Middle);
                }
                (CurveKind::Circle, _) => {
                    // Its axis along the normal: a circle standing across the plane has its
                    // vertex and its middle on it too (two ends of a diameter).
                    if let Some(axis) = edge.axis
                        && flat(plane, &axis)
                        && on_plane(plane, axis.origin)
                    {
                        out.add(local(plane, axis.origin), ModelPointKind::Center);
                    }
                    if let Some(ModelCurve::Arc { start, end, .. }) = curve {
                        out.add(start, ModelPointKind::Vertex);
                        out.add(end, ModelPointKind::Vertex);
                    }
                }
                (CurveKind::Degenerate, _) => {}
                _ => {
                    for p in &ends {
                        out.add(local(plane, *p), ModelPointKind::Vertex);
                    }
                }
            }
            if let Some(curve) = curve {
                out.curves.push(curve);
                out.curve_edges.push(index);
            }
        }
        // The other edges projected along the normal.
        for index in away {
            if let Some(curve) = project_edge(plane, topology, index, curve_of)
                && !out.curves.iter().any(|c| c.same_as(&curve))
            {
                out.curves.push(curve);
                out.curve_edges.push(index);
            }
        }
        for (index, v) in topology.vertices.iter().enumerate() {
            let p = local(plane, v.point);
            if !out.vertices.iter().any(|q| q.distance(p) < SAME_POINT) {
                out.vertices.push(p);
                out.vertex_indices
                    .push(u32::try_from(index).unwrap_or(u32::MAX));
            }
        }
        out
    }

    /// Names the sources of the curves and vertices in the part's `naming`, for the copies
    /// Convert Entities makes to follow them.
    pub fn name_sources(&mut self, naming: &BodyNaming) {
        self.curve_sources = self
            .curve_edges
            .iter()
            .map(|i| {
                let i = u32::try_from(*i).ok()?;
                naming.edge(i).cloned().map(ProjectionSource::Edge)
            })
            .collect();
        self.vertex_sources = self
            .vertex_indices
            .iter()
            .map(|i| naming.vertex(*i).cloned().map(ProjectionSource::Vertex))
            .collect();
    }

    fn add(&mut self, p: Point2, kind: ModelPointKind) {
        if !self.points.iter().any(|(q, _)| q.distance(p) < 1e-9) {
            self.points.push((p, kind));
        }
    }
}

#[cfg(test)]
mod tests {
    use oxyde_geom::{Axis, Ellipse2, Nurbs2, Plane, Point2, Point3, Vec3};
    use oxyde_kernel::{CurveKind, EdgeCurve, EdgeInfo, Nurbs3, Topology, VertexInfo};

    use oxyde_model::projection::{ModelCurve, local};

    use super::{ModelGeometry, ModelPointKind};

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
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| None);
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
                // The vertex of a circle seen edge-on.
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
                // Its axis in the plane: seen edge-on, across its diameter.
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
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| None);
        let near = |a: Point2, b: Point2| a.distance(b) < 1e-9;
        // The edge in the plane, the slanted edge, the half circle and the circle seen
        // edge-on: the copy of edge 0-1 above the plane is the same curve and is listed once.
        assert_eq!(geometry.curves.len(), 4, "{:?}", geometry.curves);
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
        assert!(oxyde_model::projection::on_ccw_arc(
            center,
            start,
            end,
            Point2::new(5.0, -7.0)
        ));
        // The circle about X seen from above: the line x = 20 from z = -2 to 2.
        assert!(matches!(
            geometry.curves[3],
            ModelCurve::Line(a, b) if (near(a, Point2::new(20.0, -2.0)) && near(b, Point2::new(20.0, 2.0)))
                || (near(a, Point2::new(20.0, 2.0)) && near(b, Point2::new(20.0, -2.0)))
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

    /// A circle of radius 2 about an axis through (30, 4, 0) tilted 45° from the Top plane's
    /// normal towards +Z, as a whole circle from its vertex at (32, 4, 0), or as the half from
    /// there through its highest point.
    fn tilted_circle(half: bool) -> Topology {
        let s = std::f64::consts::FRAC_1_SQRT_2;
        let direction = Vec3::new(0.0, s, s);
        let (ends, midpoint) = if half {
            // Counter-clockwise about the axis from (32, 4, 0): through the point along
            // axis × (1, 0, 0) = (0, s, −s), to (28, 4, 0).
            (
                vec![(32.0, 4.0, 0.0), (28.0, 4.0, 0.0)],
                Point3::new(30.0, 4.0 + 2.0 * s, -2.0 * s),
            )
        } else {
            (vec![(32.0, 4.0, 0.0)], Point3::new(28.0, 4.0, 0.0))
        };
        let n = ends.len() as u32;
        Topology {
            faces: Vec::new(),
            vertices: ends
                .into_iter()
                .map(|(x, y, z)| VertexInfo {
                    point: Point3::new(x, y, z),
                })
                .collect(),
            edges: vec![edge(
                CurveKind::Circle,
                (0..n).collect(),
                midpoint,
                Axis::new(Point3::new(30.0, 4.0, 0.0), direction),
            )],
        }
    }

    #[test]
    fn a_tilted_circle_projects_to_an_ellipse() {
        let geometry = ModelGeometry::from_body(&tilted_circle(false), &Plane::TOP, &|_| None);
        let [ModelCurve::Ellipse(ellipse)] = geometry.curves[..] else {
            panic!("{:?}", geometry.curves);
        };
        // The radius across the tilt, along x; the radius times cos 45° along the tilt.
        let want = Ellipse2 {
            center: Point2::new(30.0, 0.0),
            major: Point2::new(32.0, 0.0),
            minor: 2.0 * std::f64::consts::FRAC_1_SQRT_2,
        };
        assert!(ellipse.same_as(&want, 1e-12), "{ellipse:?}");
        // Every point of the circle lands on it: the projection is exact.
        let s = std::f64::consts::FRAC_1_SQRT_2;
        for k in 0..12 {
            let t = std::f64::consts::TAU * k as f64 / 12.0;
            let p = Point3::new(
                30.0 + 2.0 * t.cos(),
                4.0 + 2.0 * s * t.sin(),
                -2.0 * s * t.sin(),
            );
            let q = local(&Plane::TOP, p);
            let (x, y) = ((q.x - 30.0) / 2.0, q.y / (2.0 * s));
            assert!((x * x + y * y - 1.0).abs() < 1e-12, "{q:?}");
        }
    }

    #[test]
    fn a_tilted_half_circle_projects_to_half_an_ellipse_through_its_middle() {
        let geometry = ModelGeometry::from_body(&tilted_circle(true), &Plane::TOP, &|_| None);
        let [
            ModelCurve::EllipticArc {
                ellipse,
                start,
                end,
            },
        ] = geometry.curves[..]
        else {
            panic!("{:?}", geometry.curves);
        };
        // The middle (30, 4 + √2, −√2) lands at (30, √2): the sketch's y is −z.
        let middle = Point2::new(30.0, std::f64::consts::SQRT_2);
        let (a0, sweep) = ellipse.sweep(start, end);
        let at = (ellipse.angle_of(middle) - a0).rem_euclid(std::f64::consts::TAU);
        assert!(at < sweep, "{start:?} {end:?}");
        assert!((sweep - std::f64::consts::PI).abs() < 1e-12);
        let ends = [start, end];
        assert!(
            ends.iter()
                .any(|p| p.distance(Point2::new(32.0, 0.0)) < 1e-12)
        );
        assert!(
            ends.iter()
                .any(|p| p.distance(Point2::new(28.0, 0.0)) < 1e-12)
        );
    }

    /// One edge of the given kind above the Top plane, its curve handed over by `curve`.
    fn one_edge(kind: CurveKind, ends: &[(f64, f64, f64)], midpoint: Point3) -> Topology {
        Topology {
            faces: Vec::new(),
            vertices: ends
                .iter()
                .map(|&(x, y, z)| VertexInfo {
                    point: Point3::new(x, y, z),
                })
                .collect(),
            edges: vec![edge(kind, (0..ends.len() as u32).collect(), midpoint, None)],
        }
    }

    #[test]
    fn a_tilted_ellipse_edge_projects_to_the_exact_ellipse() {
        // An ellipse 6 by 2 about (1, 5, 2), its long axis along (1, 1, 0)/√2 and its short
        // one along Z, so tilted to the Top plane (normal Y).
        let s = std::f64::consts::FRAC_1_SQRT_2;
        let curve = EdgeCurve::Conic {
            center: Point3::new(1.0, 5.0, 2.0),
            x_dir: Vec3::new(s, s, 0.0),
            y_dir: Vec3::new(0.0, 0.0, 1.0),
            major: 6.0,
            minor: 2.0,
            start: 0.0,
            end: std::f64::consts::TAU,
        };
        let topology = one_edge(
            CurveKind::Ellipse,
            &[(1.0 + 6.0 * s, 5.0 + 6.0 * s, 2.0)],
            Point3::new(1.0 - 6.0 * s, 5.0 - 6.0 * s, 2.0),
        );
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| Some(curve.clone()));
        let [ModelCurve::Ellipse(ellipse)] = geometry.curves[..] else {
            panic!("{:?}", geometry.curves);
        };
        // Every point of the edge lands on it.
        for k in 0..24 {
            let t = std::f64::consts::TAU * k as f64 / 24.0;
            let p = Point3::new(
                1.0 + 6.0 * s * t.cos(),
                5.0 + 6.0 * s * t.cos(),
                2.0 + 2.0 * t.sin(),
            );
            let q = super::local(&Plane::TOP, p);
            assert!(ellipse.at(ellipse.angle_of(q)).distance(q) < 1e-9, "{q:?}");
        }
        // Semi-axes 6/√2 along x and 2 along z.
        assert!((ellipse.largest_radius() - 6.0 * s).abs() < 1e-9);
        assert!((ellipse.smallest_radius() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_conic_seen_edge_on_projects_to_the_line_it_covers() {
        // An ellipse in the plane x = 3, seen from above: the stretch z = −4..4 of x = 3.
        let curve = EdgeCurve::Conic {
            center: Point3::new(3.0, 5.0, 0.0),
            x_dir: Vec3::new(0.0, 0.0, 1.0),
            y_dir: Vec3::new(0.0, 1.0, 0.0),
            major: 4.0,
            minor: 2.0,
            start: 0.0,
            end: std::f64::consts::TAU,
        };
        let topology = one_edge(
            CurveKind::Ellipse,
            &[(3.0, 5.0, 4.0)],
            Point3::new(3.0, 5.0, -4.0),
        );
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| Some(curve.clone()));
        let [ModelCurve::Line(a, b)] = geometry.curves[..] else {
            panic!("{:?}", geometry.curves);
        };
        let ends = [a, b];
        assert!(
            ends.iter()
                .any(|p| p.distance(Point2::new(3.0, -4.0)) < 1e-9)
        );
        assert!(
            ends.iter()
                .any(|p| p.distance(Point2::new(3.0, 4.0)) < 1e-9)
        );
    }

    #[test]
    fn a_spline_edge_projects_to_the_spline_on_its_projected_poles() {
        // A rational cubic wandering in space above the Top plane.
        let poles3 = [
            (0.0, 4.0, 0.0),
            (2.0, 9.0, -3.0),
            (6.0, 2.0, 1.0),
            (9.0, 7.0, -2.0),
        ];
        let curve = Nurbs3 {
            degree: 3,
            poles: poles3
                .iter()
                .map(|&(x, y, z)| Point3::new(x, y, z))
                .collect(),
            weights: vec![1.0, 0.7, 1.6, 1.0],
            knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
        };
        let topology = one_edge(
            CurveKind::BSpline,
            &[(0.0, 4.0, 0.0), (9.0, 7.0, -2.0)],
            Point3::new(4.0, 6.0, -1.0),
        );
        let edge_curve = EdgeCurve::Nurbs {
            curve: curve.clone(),
            exact: true,
        };
        let geometry =
            ModelGeometry::from_body(&topology, &Plane::TOP, &|_| Some(edge_curve.clone()));
        let [ModelCurve::Spline(ref spline)] = geometry.curves[..] else {
            panic!("{:?}", geometry.curves);
        };
        // The space curve evaluated by components: (x, y) and (z, 0) with the same weights
        // and knots.
        let component = |f: fn(&Point3) -> (f64, f64)| Nurbs2 {
            degree: 3,
            poles: curve
                .poles
                .iter()
                .map(|p| {
                    let (a, b) = f(p);
                    Point2::new(a, b)
                })
                .collect(),
            weights: curve.weights.clone(),
            knots: curve.knots.clone(),
        };
        let (xy, z) = (component(|p| (p.x, p.y)), component(|p| (p.z, 0.0)));
        for k in 0..=20 {
            let t = k as f64 / 20.0;
            let (a, b) = (xy.point_at(t), z.point_at(t));
            let q = super::local(&Plane::TOP, Point3::new(a.x, a.y, b.x));
            assert!(spline.point_at(t).distance(q) < 1e-12, "{t}");
        }
        // Without the kernel's curve, the edge gives its vertices only.
        let without = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| None);
        assert!(without.curves.is_empty());
        assert_eq!(without.vertices.len(), 2);
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
        let geometry = ModelGeometry::from_body(&topology, &Plane::TOP, &|_| None);
        assert_eq!(geometry.curves.len(), 4);
        let mut chain = geometry.chain_of(2);
        chain.sort_unstable();
        assert_eq!(chain, vec![0, 1, 2, 3]);
    }
}
