//! The drag coefficient of a body in an external flow, as Autodesk Flow Design gave it:
//! Cd = 2 F / (ρ V² A), F the force along the free stream, V its speed, ρ the fluid's
//! density and A the model's frontal area, the area of its silhouette seen along the
//! stream.
//!
//! The silhouette is rasterised: the mesh's faces on the model, projected on the plane
//! square to the stream, are filled into a grid of pixels and the pixels covered are
//! counted, so that parts in front of one another count once (a car's wheels behind its
//! bumper), which a sum of face areas would not.

use crate::results::FlowResults;
use crate::study::{dot, norm, scale};
use crate::trajectory::basis;

/// The pixels across the silhouette's longer side.
const PIXELS: usize = 1024;

/// The model's frontal area seen along `direction`, mm²: the silhouette of the fluid
/// mesh's faces on the model. `None` without a direction or a model.
pub fn frontal_area(r: &FlowResults, direction: [f64; 3]) -> Option<f64> {
    let length = norm(direction);
    if !(length > 0.0 && length.is_finite()) {
        return None;
    }
    let d = scale(direction, 1.0 / length);
    let (u, v) = basis(d);
    let mesh = &r.mesh;
    let first = mesh.internal_faces();
    // Each face on the model, its corners on the plane.
    let faces: Vec<Vec<[f64; 2]>> = r
        .boundary_labels
        .iter()
        .enumerate()
        .filter(|(_, l)| l.is_some())
        .map(|(i, _)| {
            mesh.face(first + i)
                .iter()
                .filter_map(|&p| mesh.points.get(p as usize))
                .map(|&p| [dot(p, u), dot(p, v)])
                .collect()
        })
        .collect();
    silhouette(&faces)
}

/// The area covered by `polygons` (mm, on a plane), overlaps counted once: the pixels of a
/// grid over them whose centres any of them covers. Each polygon is fanned into triangles
/// from its first corner.
fn silhouette(polygons: &[Vec<[f64; 2]>]) -> Option<f64> {
    let corners = polygons.iter().flatten();
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in corners {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    let (width, height) = (hi[0] - lo[0], hi[1] - lo[1]);
    if !(width.is_finite() && height.is_finite() && width.max(height) > 0.0) {
        return None;
    }
    let pixel = width.max(height) / PIXELS as f64;
    let columns = ((width / pixel).ceil() as usize).clamp(1, PIXELS);
    let rows = ((height / pixel).ceil() as usize).clamp(1, PIXELS);
    let mut covered = vec![false; columns * rows];
    // A corner in pixels from the grid's corner.
    let to_grid = |p: [f64; 2]| [(p[0] - lo[0]) / pixel, (p[1] - lo[1]) / pixel];
    for polygon in polygons {
        let Some(&a) = polygon.first() else {
            continue;
        };
        let a = to_grid(a);
        for pair in polygon.windows(2).skip(1) {
            let &[b, c] = pair else {
                continue;
            };
            fill(&mut covered, columns, rows, [a, to_grid(b), to_grid(c)]);
        }
    }
    let count = covered.iter().filter(|&&c| c).count();
    Some(count as f64 * pixel * pixel)
}

/// Marks the pixels of a `columns` × `rows` grid whose centres lie in the triangle (in
/// pixels), whichever way round its corners go.
fn fill(covered: &mut [bool], columns: usize, rows: usize, [a, b, c]: [[f64; 2]; 3]) {
    let edge = |p: [f64; 2], q: [f64; 2], x: f64, y: f64| {
        (q[0] - p[0]) * (y - p[1]) - (q[1] - p[1]) * (x - p[0])
    };
    let twice = edge(a, b, c[0], c[1]);
    if twice == 0.0 || !twice.is_finite() {
        return;
    }
    let sign = twice.signum();
    // The pixels whose centres (i + ½) lie between `lo` and `hi`.
    let range = |lo: f64, hi: f64, size: usize| {
        let first = (lo - 0.5).ceil().max(0.0) as usize;
        let last = ((hi - 0.5).floor().max(-1.0) + 1.0).min(size as f64) as usize;
        first..last
    };
    let ys = range(a[1].min(b[1]).min(c[1]), a[1].max(b[1]).max(c[1]), rows);
    let xs = range(a[0].min(b[0]).min(c[0]), a[0].max(b[0]).max(c[0]), columns);
    for j in ys {
        let y = j as f64 + 0.5;
        for i in xs.clone() {
            let x = i as f64 + 0.5;
            let inside = [(a, b), (b, c), (c, a)]
                .into_iter()
                .all(|(p, q)| edge(p, q, x, y) * sign >= 0.0);
            if inside && let Some(cell) = covered.get_mut(j * columns + i) {
                *cell = true;
            }
        }
    }
}

/// The drag coefficient of `force` (N) in a free stream of `velocity` (m/s) and `density`
/// (kg/m³) on a frontal area of `area` mm²: the force's part along the stream over the
/// dynamic pressure on the area. `None` without a stream or an area.
pub fn drag_coefficient(
    force: [f64; 3],
    velocity: [f64; 3],
    density: f64,
    area: f64,
) -> Option<f64> {
    let speed = norm(velocity);
    let area_m2 = area * 1e-6;
    let pressure = 0.5 * density * speed * speed * area_m2;
    if !(pressure > 0.0 && pressure.is_finite()) {
        return None;
    }
    let along = dot(force, scale(velocity, 1.0 / speed));
    Some(along / pressure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trajectory::tests::uniform;

    #[test]
    fn a_box_shows_its_side() {
        // The two cubes, 2000 × 1000 × 1000 mm, every boundary face on the model.
        let r = uniform([1.0, 0.0, 0.0], |_| Some(0));
        let area = |d: [f64; 3]| frontal_area(&r, d).unwrap();
        // Square to the stream, the faces at both ends overlap: counted once.
        assert!((area([1.0, 0.0, 0.0]) / 1e6 - 1.0).abs() < 1e-9);
        assert!((area([0.0, -2.0, 0.0]) / 2e6 - 1.0).abs() < 1e-9);
        assert!((area([0.0, 0.0, 1.0]) / 2e6 - 1.0).abs() < 1e-9);
        // Seen along a diagonal: a 1000-wide end and a 2000-wide side, each shortened by
        // √2 (the pixels' edges cost a little).
        let oblique = area([1.0, 1.0, 0.0]);
        let expected = 3e6 / 2f64.sqrt();
        assert!(
            (oblique / expected - 1.0).abs() < 2e-3,
            "{oblique} {expected}"
        );
        assert!(frontal_area(&r, [0.0; 3]).is_none());
        // No model, no area.
        let none = uniform([1.0, 0.0, 0.0], |_| None);
        assert!(frontal_area(&none, [1.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn overlapping_triangles_count_once() {
        let square = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let half = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        let a = silhouette(std::slice::from_ref(&square)).unwrap();
        let b = silhouette(&[square, half]).unwrap();
        assert!(
            (a - 100.0).abs() < 1e-9 && (b - 100.0).abs() < 1e-9,
            "{a} {b}"
        );
        // A disc of radius 5: π r² to a tenth of a percent.
        let disc: Vec<[f64; 2]> = (0..720)
            .map(|k| {
                let t = std::f64::consts::TAU * k as f64 / 720.0;
                [5.0 * t.cos(), 5.0 * t.sin()]
            })
            .collect();
        let area = silhouette(&[disc]).unwrap();
        let exact = std::f64::consts::PI * 25.0;
        assert!((area / exact - 1.0).abs() < 1e-3, "{area} {exact}");
    }

    #[test]
    fn the_drag_coefficient_takes_the_force_along_the_stream() {
        // 1 N along a 10 m/s stream of 1.2 kg/m³ on 0.01 m²: 2 / (1.2 · 100 · 0.01).
        let cd = drag_coefficient([1.0, 0.5, 0.0], [10.0, 0.0, 0.0], 1.2, 1e4).unwrap();
        assert!((cd - 2.0 / 1.2).abs() < 1e-12, "{cd}");
        let back = drag_coefficient([1.0, 0.0, 0.0], [-10.0, 0.0, 0.0], 1.2, 1e4).unwrap();
        assert!((back + 2.0 / 1.2).abs() < 1e-12);
        assert!(drag_coefficient([1.0, 0.0, 0.0], [0.0; 3], 1.2, 1e4).is_none());
        assert!(drag_coefficient([1.0, 0.0, 0.0], [10.0, 0.0, 0.0], 1.2, 0.0).is_none());
    }
}
