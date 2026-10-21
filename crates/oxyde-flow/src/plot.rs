//! Plots of a flow's results, as SolidWorks Flow Simulation draws them: the model's faces
//! coloured by the pressure on them (a surface plot), and a plane through the fluid
//! coloured by speed or pressure (a cut plot). Both are polygons with a value each, which
//! the application splits into colour bands and draws.

use crate::results::FlowResults;
use crate::study::{add, cross, dot, norm, scale, sub};

/// What a plot shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantity {
    /// The static pressure, Pa (gauge).
    Pressure,
    /// The flow's speed, m/s.
    Speed,
}

impl Quantity {
    pub fn label(self) -> &'static str {
        match self {
            Quantity::Pressure => "Pressure",
            Quantity::Speed => "Velocity",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Quantity::Pressure => "Pa",
            Quantity::Speed => "m/s",
        }
    }

    /// Its value in each cell.
    pub fn cells(self, r: &FlowResults) -> Vec<f64> {
        match self {
            Quantity::Pressure => r.pressure.clone(),
            Quantity::Speed => r.velocity.iter().map(|u| norm(*u)).collect(),
        }
    }

    /// Its value on each boundary face, in face order from the first boundary face.
    pub fn boundary(self, r: &FlowResults) -> Vec<f64> {
        match self {
            Quantity::Pressure => r.boundary_pressure.clone(),
            Quantity::Speed => r.boundary_velocity.iter().map(|u| norm(*u)).collect(),
        }
    }
}

/// A flat polygon of a plot, mm, and the value it shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon {
    pub points: Vec<[f64; 3]>,
    pub value: f64,
}

/// The mesh's faces on the model (not on the domain's box): the wetted surface, each face
/// with `quantity` on it.
pub fn surface(r: &FlowResults, quantity: Quantity) -> Vec<Polygon> {
    let values = quantity.boundary(r);
    let first = r.mesh.internal_faces();
    r.boundary_labels
        .iter()
        .zip(&values)
        .enumerate()
        .filter(|(_, (label, _))| label.is_some())
        .map(|(i, (_, &value))| Polygon {
            points: face_points(r, first + i),
            value,
        })
        .filter(|p| p.points.len() >= 3)
        .collect()
}

/// The plane through `origin` (mm) square to `normal` cut through the fluid: one polygon
/// per cell it crosses, with that cell's `quantity`.
pub fn cut(
    r: &FlowResults,
    origin: [f64; 3],
    normal: [f64; 3],
    quantity: Quantity,
) -> Vec<Polygon> {
    let length = norm(normal);
    if !(length > 0.0 && length.is_finite()) {
        return Vec::new();
    }
    let n = scale(normal, 1.0 / length);
    let values = quantity.cells(r);
    let mesh = &r.mesh;
    let side: Vec<f64> = mesh
        .points
        .iter()
        .map(|p| dot(sub(*p, origin), n))
        .collect();
    // The faces of each cell.
    let mut start = vec![0usize; mesh.cells + 1];
    let sides = |f: usize| {
        [
            mesh.owner.get(f).map(|&c| c as usize),
            mesh.neighbour.get(f).map(|&c| c as usize),
        ]
    };
    for f in 0..mesh.faces() {
        for c in sides(f).into_iter().flatten() {
            if let Some(s) = start.get_mut(c + 1) {
                *s += 1;
            }
        }
    }
    for c in 0..mesh.cells {
        let before = start.get(c).copied().unwrap_or(0);
        if let Some(s) = start.get_mut(c + 1) {
            *s += before;
        }
    }
    let mut filled = start.clone();
    let mut faces_of = vec![0usize; start.last().copied().unwrap_or(0)];
    for f in 0..mesh.faces() {
        for c in sides(f).into_iter().flatten() {
            if let Some(at) = filled.get_mut(c) {
                if let Some(slot) = faces_of.get_mut(*at) {
                    *slot = f;
                }
                *at += 1;
            }
        }
    }
    // A basis of the plane, to order each polygon's corners around it.
    let helper = if n[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u = {
        let c = cross(n, helper);
        scale(c, 1.0 / norm(c))
    };
    let v = cross(n, u);

    let mut out = Vec::new();
    let mut edges: Vec<(u32, u32)> = Vec::new();
    let mut points: Vec<[f64; 3]> = Vec::new();
    for c in 0..mesh.cells {
        let (Some(&a), Some(&b)) = (start.get(c), start.get(c + 1)) else {
            continue;
        };
        let Some(faces) = faces_of.get(a..b) else {
            continue;
        };
        edges.clear();
        points.clear();
        for &f in faces {
            let corners = mesh.face(f);
            for (k, &p) in corners.iter().enumerate() {
                let q = corners.get((k + 1) % corners.len()).copied().unwrap_or(p);
                let (Some(&dp), Some(&dq)) = (side.get(p as usize), side.get(q as usize)) else {
                    continue;
                };
                if (dp >= 0.0) == (dq >= 0.0) {
                    continue;
                }
                let key = (p.min(q), p.max(q));
                if edges.contains(&key) {
                    continue;
                }
                edges.push(key);
                let (Some(&pp), Some(&pq)) =
                    (mesh.points.get(p as usize), mesh.points.get(q as usize))
                else {
                    continue;
                };
                let t = dp / (dp - dq);
                points.push(add(pp, scale(sub(pq, pp), t)));
            }
        }
        if points.len() < 3 {
            continue;
        }
        let centre = scale(
            points.iter().fold([0.0; 3], |s, p| add(s, *p)),
            1.0 / points.len() as f64,
        );
        let angle = |p: &[f64; 3]| {
            let d = sub(*p, centre);
            dot(d, v).atan2(dot(d, u))
        };
        let mut ordered = points.clone();
        ordered.sort_by(|p, q| angle(p).total_cmp(&angle(q)));
        out.push(Polygon {
            points: ordered,
            value: values.get(c).copied().unwrap_or(f64::NAN),
        });
    }
    out
}

/// The smallest and largest finite value of `polygons`.
pub fn range(polygons: &[Polygon]) -> Option<(f64, f64)> {
    let finite = || polygons.iter().map(|p| p.value).filter(|v| v.is_finite());
    let lo = finite().fold(f64::INFINITY, f64::min);
    let hi = finite().fold(f64::NEG_INFINITY, f64::max);
    lo.is_finite().then_some((lo, hi))
}

/// The box holding the fluid mesh, mm.
pub fn bounds(r: &FlowResults) -> Option<([f64; 3], [f64; 3])> {
    let first = *r.mesh.points.first()?;
    Some(r.mesh.points.iter().fold((first, first), |(lo, hi), p| {
        (
            crate::study::zip(lo, *p, f64::min),
            crate::study::zip(hi, *p, f64::max),
        )
    }))
}

fn face_points(r: &FlowResults, f: usize) -> Vec<[f64; 3]> {
    r.mesh
        .face(f)
        .iter()
        .filter_map(|&p| r.mesh.points.get(p as usize).copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::PolyMesh;
    use crate::mesh::tests::write_two_cubes;

    /// The two unit cubes (1000 mm), the first at 10 Pa moving at 2 m/s, the second at
    /// 20 Pa at 3 m/s; on the inlet 15 Pa, on the walls their cells'.
    fn results() -> FlowResults {
        // A folder per call: the tests run side by side.
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("oxyde-flow-plot-{}-{call}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_two_cubes(&dir, false);
        let mesh = PolyMesh::read(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let (cell_centres, cell_volumes) = mesh.cell_geometry();
        let first = mesh.internal_faces();
        let boundary = mesh.faces() - first;
        let mut boundary_labels = vec![None; boundary];
        boundary_labels[0] = Some(0);
        for l in &mut boundary_labels[1..] {
            *l = Some(1);
        }
        // The x = 2 wall of the second cube is the domain's, on no model face.
        boundary_labels[1] = None;
        let owner_value = |f: usize, a: f64, b: f64| if mesh.owner[f] == 0 { a } else { b };
        FlowResults {
            time: "1".to_string(),
            cell_centres,
            cell_volumes,
            velocity: vec![[2.0, 0.0, 0.0], [0.0, 3.0, 0.0]],
            pressure: vec![10.0, 20.0],
            boundary_velocity: (first..mesh.faces())
                .map(|f| [owner_value(f, 2.0, 0.0), owner_value(f, 0.0, 3.0), 0.0])
                .collect(),
            boundary_pressure: (first..mesh.faces())
                .map(|f| {
                    if f == first {
                        15.0
                    } else {
                        owner_value(f, 10.0, 20.0)
                    }
                })
                .collect(),
            boundary_flux: None,
            boundary_labels,
            mesh,
        }
    }

    fn area(p: &Polygon) -> [f64; 3] {
        let first = p.points[0];
        let mut sum = [0.0; 3];
        for w in p.points.windows(2).skip(1) {
            sum = add(sum, cross(sub(w[0], first), sub(w[1], first)));
        }
        scale(sum, 0.5)
    }

    #[test]
    fn a_plane_square_to_x_cuts_one_cube() {
        let r = results();
        let cut = cut(&r, [500.0, 0.0, 0.0], [1.0, 0.0, 0.0], Quantity::Speed);
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].value, 2.0);
        assert_eq!(cut[0].points.len(), 4);
        assert!(cut[0].points.iter().all(|p| (p[0] - 500.0).abs() < 1e-9));
        // A square metre, its corners in order round it.
        assert!((norm(area(&cut[0])) - 1e6).abs() < 1e-3);
    }

    #[test]
    fn a_slanted_plane_cuts_both() {
        let r = results();
        let n = [1.0, 1.0, 0.0];
        let o = [1000.0, 500.0, 500.0];
        let cut = cut(&r, o, n, Quantity::Pressure);
        assert_eq!(cut.len(), 2);
        let values: Vec<f64> = cut.iter().map(|p| p.value).collect();
        assert_eq!(values, [10.0, 20.0]);
        for p in &cut {
            assert!(p.points.iter().all(|q| dot(sub(*q, o), n).abs() < 1e-6));
            // Convex and in order: the area's normal lies along the plane's.
            let a = area(p);
            assert!(norm(cross(a, n)) < 1e-6 * norm(a) * norm(n), "{a:?}");
        }
        assert_eq!(range(&cut), Some((10.0, 20.0)));
        assert!(super::cut(&r, o, [0.0; 3], Quantity::Pressure).is_empty());
        assert!(super::cut(&r, [5000.0, 0.0, 0.0], [1.0, 0.0, 0.0], Quantity::Pressure).is_empty());
    }

    #[test]
    fn the_surface_is_the_model_faces_only() {
        let r = results();
        let s = surface(&r, Quantity::Pressure);
        // Ten boundary faces, one on the domain's box.
        assert_eq!(s.len(), 9);
        assert_eq!(s[0].value, 15.0);
        assert_eq!(s.iter().filter(|p| p.value == 20.0).count(), 4);
        let speeds = surface(&r, Quantity::Speed);
        assert_eq!(speeds[0].value, 2.0);
        assert_eq!(bounds(&r), Some(([0.0; 3], [2000.0, 1000.0, 1000.0])));
    }
}
