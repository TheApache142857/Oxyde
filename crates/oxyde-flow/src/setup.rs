//! What a study leaves to Oxyde, worked out from the model: the mesh from a level, as
//! SolidWorks Flow Simulation's *level of initial mesh*, the computational domain, and a
//! point inside an internal flow's cavity.

use crate::study::{Domain, FlowType, MeshSettings, Surface, add, cross, dot, norm, scale, sub};

/// The lowest and highest mesh levels.
pub const LEVELS: std::ops::RangeInclusive<u32> = 1..=7;

/// The level new studies start at.
pub const DEFAULT_LEVEL: u32 = 3;

/// The mesh of `level` (1 coarse to 7 fine) for a model whose box is `bounds` (mm):
///
/// - an internal flow puts 4 cells across the model's thinnest side at level 1, half as
///   many again per level (9 at level 3, 45 at 7), and halves them once along its surfaces;
/// - an external flow puts 2 cells across its longest side at level 1, a quarter more per
///   level, and halves them two to four times along its surfaces (3 at level 3).
///
/// Turbulent flows grow three layers of thin cells along walls. When the domain's
/// background cells alone would pass a quarter of `max_cells`, they are made larger.
pub fn mesh_for_level(
    level: u32,
    bounds: ([f64; 3], [f64; 3]),
    flow_type: FlowType,
    turbulent: bool,
    max_cells: u64,
) -> MeshSettings {
    let level = level.clamp(*LEVELS.start(), *LEVELS.end());
    let steps = f64::from(level - 1);
    let extents = sub(bounds.1, bounds.0).map(|e| if e.is_finite() { e.max(0.0) } else { 0.0 });
    let largest = extents.into_iter().fold(0.0, f64::max).max(1e-3);
    let (cell_size, surface_level) = match flow_type {
        FlowType::Internal { .. } => {
            let thinnest = extents
                .into_iter()
                .filter(|e| *e > 0.0)
                .fold(f64::INFINITY, f64::min);
            let thinnest = if thinnest.is_finite() {
                thinnest
            } else {
                largest
            };
            (thinnest / (4.0 * 1.5f64.powf(steps)), 1)
        }
        FlowType::External { .. } => (largest / (2.0 * 1.26f64.powf(steps)), 2 + level / 3),
    };
    let mut mesh = MeshSettings {
        cell_size,
        surface_level,
        feature_level: surface_level,
        layers: if turbulent { 3 } else { 0 },
        max_cells: max_cells.max(1000),
    };
    // The background must leave room for the refinement along the surfaces. An internal
    // flow's domain grows with the cells, so this is done until they fit.
    let room = mesh.max_cells as f64 / 4.0;
    for _ in 0..20 {
        let cells = background_cells(
            automatic_domain(bounds, flow_type, mesh.cell_size),
            mesh.cell_size,
        );
        if cells <= room {
            break;
        }
        mesh.cell_size *= (cells / room).cbrt() * 1.01;
    }
    mesh
}

/// The background cubes of edge `h` in `domain`.
fn background_cells(domain: Domain, h: f64) -> f64 {
    sub(domain.max, domain.min)
        .into_iter()
        .map(|e| (e / h).ceil().max(1.0))
        .product()
}

/// The computational domain for a model whose box is `bounds` (mm): for an internal flow,
/// the model's box with two background cells to spare all round; for an external one,
/// [`Domain::external`].
pub fn automatic_domain(
    bounds: ([f64; 3], [f64; 3]),
    flow_type: FlowType,
    cell_size: f64,
) -> Domain {
    match flow_type {
        FlowType::Internal { .. } => Domain::internal(bounds, 2.0 * cell_size),
        FlowType::External { velocity } => Domain::external(bounds, velocity),
    }
}

/// A point of the fluid `depth` (mm) in from the faces of `labels`: from the middle of
/// their largest triangle, along its outward normal (out of the solid, which is into the
/// fluid of an internal flow), but never more than half way to the next surface in that
/// direction. `None` when no triangle carries those labels.
pub fn point_inside(surfaces: &[Surface], labels: &[u32], depth: f64) -> Option<[f64; 3]> {
    let mut best: Option<(f64, [[f64; 3]; 3])> = None;
    for surface in surfaces {
        for (t, label) in surface.labels.iter().enumerate() {
            if !labels.contains(label) {
                continue;
            }
            let Some(corners) = surface.corners(t) else {
                continue;
            };
            let [a, b, c] = corners;
            let area = norm(cross(sub(b, a), sub(c, a)));
            if area > 0.0 && best.is_none_or(|(largest, _)| area > largest) {
                best = Some((area, corners));
            }
        }
    }
    let (_, [a, b, c]) = best?;
    let normal = cross(sub(b, a), sub(c, a));
    let unit = scale(normal, 1.0 / norm(normal));
    let centre = scale(add(add(a, b), c), 1.0 / 3.0);
    // The next surface along the normal: the point stays well inside a thin channel.
    let room = surfaces
        .iter()
        .flat_map(|s| (0..s.triangles.len()).filter_map(move |t| s.corners(t)))
        .filter_map(|tri| ray_hit(centre, unit, tri))
        .filter(|&t| t > 1e-9 * (1.0 + depth))
        .fold(f64::INFINITY, f64::min);
    let reach = if room.is_finite() {
        depth.min(room / 2.0)
    } else {
        depth
    };
    (reach > 0.0).then(|| add(centre, scale(unit, reach)))
}

/// Where the ray from `origin` along `direction` meets the triangle, as a multiple of
/// `direction` (Möller and Trumbore).
fn ray_hit(origin: [f64; 3], direction: [f64; 3], [a, b, c]: [[f64; 3]; 3]) -> Option<f64> {
    let e1 = sub(b, a);
    let e2 = sub(c, a);
    let p = cross(direction, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-14 * (dot(e1, e1) + dot(e2, e2)) {
        return None;
    }
    let s = sub(origin, a);
    let u = dot(s, p) / det;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(direction, q) / det;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    Some(dot(e2, q) / det)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::study::cube;

    #[test]
    fn levels_refine_the_mesh() {
        let bounds = ([0.0, 0.0, 0.0], [10.0, 10.0, 100.0]);
        let internal = FlowType::Internal { point: [5.0; 3] };
        let coarse = mesh_for_level(1, bounds, internal, false, 100_000_000);
        let middle = mesh_for_level(3, bounds, internal, false, 100_000_000);
        let fine = mesh_for_level(7, bounds, internal, true, 100_000_000);
        // Across the 10 mm side: 4, 9 and about 45 cells (with memory enough for them).
        assert!((coarse.cell_size - 2.5).abs() < 1e-12);
        assert!((middle.cell_size - 10.0 / 9.0).abs() < 1e-12);
        assert!((fine.cell_size - 10.0 / (4.0 * 1.5f64.powi(6))).abs() < 1e-12);
        assert_eq!((middle.surface_level, middle.layers), (1, 0));
        assert_eq!(fine.layers, 3);
        // Out of range: the nearest level.
        assert_eq!(
            mesh_for_level(12, bounds, internal, false, 100_000_000),
            fine_but_laminar(fine)
        );

        let external = FlowType::External {
            velocity: [1.0, 0.0, 0.0],
        };
        let around = mesh_for_level(3, bounds, external, false, 100_000_000);
        assert!((around.cell_size - 100.0 / (2.0 * 1.26f64.powi(2))).abs() < 1e-9);
        assert_eq!(around.surface_level, 3);
    }

    fn fine_but_laminar(mut m: MeshSettings) -> MeshSettings {
        m.layers = 0;
        m
    }

    #[test]
    fn too_many_cells_make_larger_ones() {
        let bounds = ([0.0; 3], [100.0, 100.0, 100.0]);
        let internal = FlowType::Internal { point: [50.0; 3] };
        let mesh = mesh_for_level(7, bounds, internal, false, 100_000);
        let domain = automatic_domain(bounds, internal, mesh.cell_size);
        let cells = background_cells(domain, mesh.cell_size);
        assert!(cells <= 25_000.0, "{cells} background cells");
    }

    #[test]
    fn a_point_inside_a_cavity_from_its_inlet() {
        // A cavity: the cube turned inside out, its sides' normals pointing into it, as a
        // cavity's faces do (out of the solid around it). Its −x side is the inlet.
        let mut cavity = cube(10.0, [0.0; 3]);
        for t in &mut cavity.triangles {
            t.swap(1, 2);
        }
        let p = point_inside(&[cavity.clone()], &[0], 2.0).unwrap();
        // From the −x side's middle, 2 mm in.
        assert!(
            (p[0] - -3.0).abs() < 1e-9 && p[1].abs() < 5.0 && p[2].abs() < 5.0,
            "{p:?}"
        );
        // Asked for 30 mm, it stops half way across the 10 mm cavity.
        let p = point_inside(&[cavity.clone()], &[0], 30.0).unwrap();
        assert!((p[0] - 0.0).abs() < 1e-9, "{p:?}");
        assert!(point_inside(&[cavity], &[42], 1.0).is_none());
    }
}
