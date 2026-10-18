//! Result plots: a quantity drawn as colour bands on the mesh's (displaced) boundary.
//!
//! Each boundary facet is drawn as one triangle, or as four for ten-node elements (its
//! corners and edge nodes), and cut along the values separating the bands, so the bands'
//! borders are the quantity's iso-lines. The pieces are grouped by model face and band,
//! so that a viewport drawing each group in one colour with smooth normals inside it
//! keeps the model's edges crisp.

// Indices into the mesh (validated by the solve that made the values) and into arrays
// sized here.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;

use crate::math::{V3, cross, lerp, norm, sub};
use crate::mesh::TetMesh;
use crate::solve::Results;

/// What a plot shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quantity {
    /// MPa.
    VonMises,
    /// The largest principal stress, MPa.
    FirstPrincipal,
    /// The length of the displacement, mm.
    Displacement,
    /// Yield strength over von Mises stress.
    FactorOfSafety,
}

impl Quantity {
    pub const ALL: [Quantity; 4] = [
        Quantity::VonMises,
        Quantity::FirstPrincipal,
        Quantity::Displacement,
        Quantity::FactorOfSafety,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Quantity::VonMises => "von Mises stress",
            Quantity::FirstPrincipal => "First principal stress",
            Quantity::Displacement => "Displacement",
            Quantity::FactorOfSafety => "Factor of safety",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Quantity::VonMises | Quantity::FirstPrincipal => "MPa",
            Quantity::Displacement => "mm",
            Quantity::FactorOfSafety => "",
        }
    }

    /// Its value at each node. Factors of safety where nothing is stressed, or the
    /// yield strength unknown, are infinite.
    pub fn values(self, results: &Results) -> Vec<f64> {
        match self {
            Quantity::VonMises => results.von_mises.clone(),
            Quantity::FirstPrincipal => results.first_principal(),
            Quantity::Displacement => results
                .displacement
                .iter()
                .map(|u| crate::math::norm(*u))
                .collect(),
            Quantity::FactorOfSafety => (0..results.von_mises.len())
                .map(|i| results.factor_of_safety(i).unwrap_or(f64::INFINITY))
                .collect(),
        }
    }
}

/// The triangles of one model face in one colour band.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Band {
    /// The face label of the facets it comes from.
    pub label: u32,
    /// 0 for the lowest values.
    pub band: u32,
    pub positions: Vec<[f32; 3]>,
    /// Counter-clockwise seen from outside.
    pub triangles: Vec<[u32; 3]>,
}

/// A point of a band: a node, or where the edge between two nodes (in order) crosses
/// the value between two bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Node(u32),
    Iso(u32, u32, u32),
}

impl Key {
    /// The mesh edge an iso point lies on.
    fn edge(self) -> Option<(u32, u32)> {
        match self {
            Key::Iso(a, b, _) => Some((a, b)),
            Key::Node(_) => None,
        }
    }
}

/// The boundary as triangles of nodes, each with its facet's label.
fn boundary_triangles(mesh: &TetMesh) -> Vec<([u32; 3], u32)> {
    let mut out = Vec::new();
    for f in &mesh.facets {
        let [a, b, c] = mesh.facet_corners(f);
        match mesh.facet_edge_nodes(f) {
            Some([ab, bc, ca]) => {
                out.push(([a, ab, ca], f.label));
                out.push(([ab, b, bc], f.label));
                out.push(([ca, bc, c], f.label));
                out.push(([ab, bc, ca], f.label));
            }
            None => out.push(([a, b, c], f.label)),
        }
    }
    out
}

/// The boundary of `mesh` at `positions` (one per node: the nodes displaced, or not),
/// cut into `count` bands of equal width between `range.0` and `range.1` by `values`
/// (one per node; values outside the range go to the end bands).
pub fn bands(
    mesh: &TetMesh,
    positions: &[V3],
    values: &[f64],
    range: (f64, f64),
    count: usize,
) -> Vec<Band> {
    let count = count.max(1);
    let (lo, hi) = range;
    let width = (hi - lo) / count as f64;
    let flat = !(width.is_finite() && width > 0.0);
    let value = |i: u32| -> f64 {
        let v = values.get(i as usize).copied().unwrap_or(lo);
        if v.is_nan() {
            lo
        } else {
            v.clamp(lo.min(hi), hi.max(lo))
        }
    };
    let band_of = |v: f64| -> u32 {
        if flat {
            return 0;
        }
        (((v - lo) / width).floor().max(0.0) as usize).min(count - 1) as u32
    };
    let level_value = |level: u32| lo + width * f64::from(level);
    let position = |i: u32| positions.get(i as usize).copied().unwrap_or([0.0; 3]);
    // A point on the edge a–b where the value is that of `level`.
    let iso = |a: u32, b: u32, level: u32| -> (Key, V3, f64) {
        let (a, b) = (a.min(b), a.max(b));
        let (va, vb) = (value(a), value(b));
        let c = level_value(level);
        let t = if vb != va {
            ((c - va) / (vb - va)).clamp(0.0, 1.0)
        } else {
            0.5
        };
        (Key::Iso(a, b, level), lerp(position(a), position(b), t), c)
    };
    let mut groups: Vec<Band> = Vec::new();
    let mut group_of: HashMap<(u32, u32), usize> = HashMap::new();
    let mut vertex_of: Vec<HashMap<Key, u32>> = Vec::new();
    for (tri, label) in boundary_triangles(mesh) {
        let corners: Vec<(Key, V3, f64)> = tri
            .iter()
            .map(|&i| (Key::Node(i), position(i), value(i)))
            .collect();
        let bands: Vec<u32> = corners.iter().map(|c| band_of(c.2)).collect();
        let (first, last) = (
            bands.iter().copied().min().unwrap_or(0),
            bands.iter().copied().max().unwrap_or(0),
        );
        for band in first..=last {
            let mut polygon = corners.clone();
            if band > first {
                polygon = clip(&polygon, band, true, &iso, &level_value);
            }
            if band < last {
                polygon = clip(&polygon, band + 1, false, &iso, &level_value);
            }
            if polygon.len() < 3 {
                continue;
            }
            let g = *group_of.entry((label, band)).or_insert_with(|| {
                groups.push(Band {
                    label,
                    band,
                    ..Band::default()
                });
                vertex_of.push(HashMap::new());
                groups.len() - 1
            });
            let ids: Vec<u32> = polygon
                .iter()
                .map(|&(key, p, _)| {
                    let group = &mut groups[g];
                    *vertex_of[g].entry(key).or_insert_with(|| {
                        group.positions.push(p.map(|c| c as f32));
                        (group.positions.len() - 1) as u32
                    })
                })
                .collect();
            for k in 1..polygon.len() - 1 {
                // A band touching a corner exactly leaves slivers of no area: not drawn.
                let (a, b, c) = (polygon[0].1, polygon[k].1, polygon[k + 1].1);
                let (e1, e2) = (sub(b, a), sub(c, a));
                if norm(cross(e1, e2)) <= 1e-9 * norm(e1) * norm(e2) {
                    continue;
                }
                groups[g].triangles.push([ids[0], ids[k], ids[k + 1]]);
            }
        }
    }
    groups.retain(|g| !g.triangles.is_empty());
    groups
}

/// The part of a polygon whose values are at least (`above`) or at most the value of
/// `level`, the new points where its edges cross it.
fn clip(
    polygon: &[(Key, V3, f64)],
    level: u32,
    above: bool,
    iso: &dyn Fn(u32, u32, u32) -> (Key, V3, f64),
    level_value: &dyn Fn(u32) -> f64,
) -> Vec<(Key, V3, f64)> {
    let c = level_value(level);
    let inside = |v: f64| if above { v >= c } else { v <= c };
    let mut out = Vec::with_capacity(polygon.len() + 2);
    for (k, &p) in polygon.iter().enumerate() {
        let q = polygon[(k + 1) % polygon.len()];
        if inside(p.2) {
            out.push(p);
        }
        if inside(p.2) != inside(q.2) {
            // The mesh edge this side of the polygon lies on.
            let edge = match (p.0, q.0) {
                (Key::Node(a), Key::Node(b)) => Some((a, b)),
                (Key::Iso(a, b, _), Key::Node(x)) | (Key::Node(x), Key::Iso(a, b, _))
                    if x == a || x == b =>
                {
                    Some((a, b))
                }
                (Key::Iso(..), Key::Iso(..)) if p.0.edge() == q.0.edge() => p.0.edge(),
                _ => None,
            };
            if let Some((a, b)) = edge {
                out.push(iso(a, b, level));
            }
        }
    }
    out
}

/// The mesh's boundary edges as polylines at `positions`, for showing the mesh: each
/// element edge once, through its edge node for ten-node elements.
pub fn mesh_edges(mesh: &TetMesh, positions: &[V3]) -> Vec<Vec<[f32; 3]>> {
    let position = |i: u32| {
        positions
            .get(i as usize)
            .copied()
            .unwrap_or([0.0; 3])
            .map(|c| c as f32)
    };
    let mut seen: HashMap<(u32, u32), ()> = HashMap::new();
    let mut out = Vec::new();
    for f in &mesh.facets {
        let c = mesh.facet_corners(f);
        let mids = mesh.facet_edge_nodes(f);
        for k in 0..3 {
            let (a, b) = (c[k], c[(k + 1) % 3]);
            if seen.insert((a.min(b), a.max(b)), ()).is_some() {
                continue;
            }
            let line = match mids {
                Some(m) => vec![position(a), position(m[k]), position(b)],
                None => vec![position(a), position(b)],
            };
            out.push(line);
        }
    }
    out
}

/// SolidWorks' colours for a plot: blue for the lowest values through cyan, green and
/// yellow to red for the highest, `t` from 0 to 1.
pub fn rainbow(t: f64) -> [u8; 3] {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
    let ramp = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    match t {
        t if t < 0.25 => [0, ramp(4.0 * t), 255],
        t if t < 0.5 => [0, 255, ramp(1.0 - 4.0 * (t - 0.25))],
        t if t < 0.75 => [ramp(4.0 * (t - 0.5)), 255, 0],
        t => [255, ramp(1.0 - 4.0 * (t - 0.75)), 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_follow_the_values_and_cover_the_boundary() {
        let mesh = TetMesh::structured_box([0.0; 3], [10.0, 1.0, 1.0], [5, 1, 1]);
        // The value is x: ten bands of 1 mm.
        let values: Vec<f64> = mesh.nodes.iter().map(|p| p[0]).collect();
        let bands = bands(&mesh, &mesh.nodes, &values, (0.0, 10.0), 10);
        let area = |b: &Band| -> f64 {
            b.triangles
                .iter()
                .map(|t| {
                    let p = t.map(|i| b.positions[i as usize].map(f64::from));
                    let e1 = crate::math::sub(p[1], p[0]);
                    let e2 = crate::math::sub(p[2], p[0]);
                    0.5 * crate::math::norm(crate::math::cross(e1, e2))
                })
                .sum()
        };
        let total: f64 = bands.iter().map(area).sum();
        // The box's surface: 4 sides of 10 × 1 and two ends of 1 × 1.
        assert!((total - 42.0).abs() < 1e-5, "{total}");
        for b in &bands {
            for p in &b.positions {
                let x = f64::from(p[0]);
                assert!(
                    x >= f64::from(b.band) - 1e-5 && x <= f64::from(b.band) + 1.0 + 1e-5,
                    "band {} at x {x}",
                    b.band
                );
            }
        }
        // Each side of the box in each band of x, and the ends in theirs.
        let side_bands = bands.iter().filter(|b| b.label == 2).count();
        assert_eq!(side_bands, 10);
        assert_eq!(bands.iter().filter(|b| b.label == 0).count(), 1);
        // Flat values: one band per face.
        let flat = super::bands(
            &mesh,
            &mesh.nodes,
            &vec![3.0; mesh.nodes.len()],
            (3.0, 3.0),
            8,
        );
        assert_eq!(flat.len(), 6);
        assert!(flat.iter().all(|b| b.band == 0));
    }

    #[test]
    fn colours_and_edges() {
        assert_eq!(rainbow(0.0), [0, 0, 255]);
        assert_eq!(rainbow(0.5), [0, 255, 0]);
        assert_eq!(rainbow(1.0), [255, 0, 0]);
        assert_eq!(rainbow(f64::NAN), [0, 0, 255]);
        let mut mesh = TetMesh::structured_box([0.0; 3], [1.0, 1.0, 1.0], [1, 1, 1]);
        // A cube's surface in Kuhn triangles: 12 cube edges and 6 face diagonals.
        assert_eq!(mesh_edges(&mesh, &mesh.nodes).len(), 18);
        mesh.make_quadratic();
        let edges = mesh_edges(&mesh, &mesh.nodes);
        assert_eq!(edges.len(), 18);
        assert!(edges.iter().all(|e| e.len() == 3));
    }
}
