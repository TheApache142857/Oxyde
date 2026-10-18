//! The tetrahedral mesh: nodes, four-node elements with optional edge nodes (ten-node
//! elements), a body per element and the boundary facets with their face labels.
//!
//! Elements are positively oriented (the fourth corner on the side the first three wind
//! counter-clockwise around), and edge nodes sit at the middle of their edges, so every
//! element is straight-sided and its mapping from reference coordinates is affine.

// Indices into the mesh's own arrays, checked by `TetMesh::validate` before any use.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;

use crate::math::{V3, cross, lerp, norm, scale, sub, tet_volume6};

/// The edges of a tetrahedron, as pairs of corners, in the order of the edge nodes.
pub const EDGES: [(usize, usize); 6] = [(0, 1), (1, 2), (0, 2), (0, 3), (1, 3), (2, 3)];

/// The face opposite each corner, wound counter-clockwise seen from outside.
pub const FACES: [[usize; 3]; 4] = [[1, 2, 3], [0, 3, 2], [0, 1, 3], [0, 2, 1]];

/// A face of an element on the mesh's boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facet {
    pub tet: u32,
    /// The corner the facet is opposite (an index into [`FACES`]).
    pub face: u8,
    /// The model face it lies on, or [`crate::NO_LABEL`].
    pub label: u32,
}

/// A tetrahedral mesh.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TetMesh {
    pub nodes: Vec<V3>,
    /// Corner nodes of each element.
    pub tets: Vec<[u32; 4]>,
    /// For ten-node elements, the node at the middle of each edge, in [`EDGES`] order.
    pub edge_nodes: Option<Vec<[u32; 6]>>,
    /// The body (solid) each element belongs to, for its material.
    pub bodies: Vec<u32>,
    pub facets: Vec<Facet>,
}

impl TetMesh {
    pub fn is_quadratic(&self) -> bool {
        self.edge_nodes.is_some()
    }

    /// Nodes per element: 4 or 10.
    pub fn nodes_per_element(&self) -> usize {
        if self.is_quadratic() { 10 } else { 4 }
    }

    /// Every node of element `e`: the corners, then the edge nodes.
    pub fn element_nodes(&self, e: usize) -> ([u32; 10], usize) {
        let mut out = [0u32; 10];
        out[..4].copy_from_slice(&self.tets[e]);
        match &self.edge_nodes {
            Some(edges) => {
                out[4..].copy_from_slice(&edges[e]);
                (out, 10)
            }
            None => (out, 4),
        }
    }

    /// Whether every index refers to something that exists and every element has a
    /// positive volume; what is wrong otherwise.
    pub fn validate(&self) -> Result<(), String> {
        let n = self.nodes.len() as u64;
        if self.bodies.len() != self.tets.len() {
            return Err("an element without a body".to_owned());
        }
        if let Some(edges) = &self.edge_nodes {
            if edges.len() != self.tets.len() {
                return Err("an element without edge nodes".to_owned());
            }
            if edges.iter().flatten().any(|&i| u64::from(i) >= n) {
                return Err("an edge node that does not exist".to_owned());
            }
        }
        if self.tets.iter().flatten().any(|&i| u64::from(i) >= n) {
            return Err("a corner that does not exist".to_owned());
        }
        if self.nodes.iter().flatten().any(|c| !c.is_finite()) {
            return Err("a node at an infinite position".to_owned());
        }
        if self
            .facets
            .iter()
            .any(|f| f.tet as usize >= self.tets.len() || f.face > 3)
        {
            return Err("a facet on no element".to_owned());
        }
        if (0..self.tets.len()).any(|e| self.tet_volume(e).is_none_or(|v| v <= 0.0)) {
            return Err("an element that is flat or turned inside out".to_owned());
        }
        Ok(())
    }

    /// The volume of element `e` (negative when it is inside out).
    pub fn tet_volume(&self, e: usize) -> Option<f64> {
        let t = self.tets.get(e)?;
        let p = |i: u32| self.nodes.get(i as usize).copied();
        Some(tet_volume6(p(t[0])?, p(t[1])?, p(t[2])?, p(t[3])?) / 6.0)
    }

    /// The total volume, mm³.
    pub fn volume(&self) -> f64 {
        (0..self.tets.len())
            .filter_map(|e| self.tet_volume(e))
            .sum()
    }

    /// The corner nodes of a facet, wound counter-clockwise seen from outside.
    pub fn facet_corners(&self, f: &Facet) -> [u32; 3] {
        let t = &self.tets[f.tet as usize];
        let [a, b, c] = FACES[f.face as usize & 3];
        [t[a], t[b], t[c]]
    }

    /// For a ten-node mesh, the edge nodes of a facet: between its corners 0–1, 1–2 and
    /// 2–0.
    pub fn facet_edge_nodes(&self, f: &Facet) -> Option<[u32; 3]> {
        let edges = self.edge_nodes.as_ref()?.get(f.tet as usize)?;
        let [a, b, c] = FACES[f.face as usize & 3];
        let between = |i: usize, j: usize| {
            EDGES
                .iter()
                .position(|&(p, q)| (p, q) == (i, j) || (p, q) == (j, i))
                .map(|k| edges[k])
        };
        Some([between(a, b)?, between(b, c)?, between(c, a)?])
    }

    /// A facet's area and its outward unit normal (times the area).
    pub fn facet_area_normal(&self, f: &Facet) -> V3 {
        let [a, b, c] = self.facet_corners(f).map(|i| self.nodes[i as usize]);
        scale(cross(sub(b, a), sub(c, a)), 0.5)
    }

    /// Finds the boundary facets (faces of one element only), each labelled `label` by
    /// the given function of its corners' positions, in element order.
    pub fn find_boundary(&mut self, mut label: impl FnMut([V3; 3]) -> u32) {
        let mut count: HashMap<[u32; 3], u32> = HashMap::new();
        for t in &self.tets {
            for face in FACES {
                let mut key = face.map(|k| t[k]);
                key.sort_unstable();
                *count.entry(key).or_insert(0) += 1;
            }
        }
        let mut facets = Vec::new();
        for (e, t) in self.tets.iter().enumerate() {
            for (k, face) in FACES.iter().enumerate() {
                let mut key = face.map(|i| t[i]);
                key.sort_unstable();
                if count.get(&key) == Some(&1) {
                    let corners = face.map(|i| self.nodes[t[i] as usize]);
                    facets.push(Facet {
                        tet: e as u32,
                        face: k as u8,
                        label: label(corners),
                    });
                }
            }
        }
        self.facets = facets;
    }

    /// Adds a node at the middle of every edge, making ten-node elements. Edges shared by
    /// elements share their node. Nothing happens when the mesh already has them.
    pub fn make_quadratic(&mut self) {
        if self.edge_nodes.is_some() {
            return;
        }
        let mut made: HashMap<(u32, u32), u32> = HashMap::new();
        let mut edge_nodes = Vec::with_capacity(self.tets.len());
        for t in &self.tets {
            let mut ids = [0u32; 6];
            for (k, &(i, j)) in EDGES.iter().enumerate() {
                let (a, b) = (t[i].min(t[j]), t[i].max(t[j]));
                ids[k] = *made.entry((a, b)).or_insert_with(|| {
                    let p = lerp(self.nodes[a as usize], self.nodes[b as usize], 0.5);
                    self.nodes.push(p);
                    (self.nodes.len() - 1) as u32
                });
            }
            edge_nodes.push(ids);
        }
        self.edge_nodes = Some(edge_nodes);
    }

    /// A box from `lo` to `hi` cut into `n` cells along each axis, each cell into six
    /// tetrahedra, with the boundary labelled by side: 0 −X, 1 +X, 2 −Y, 3 +Y, 4 −Z, 5 +Z.
    pub fn structured_box(lo: V3, hi: V3, n: [usize; 3]) -> TetMesh {
        let n = n.map(|k| k.max(1));
        let (nx, ny, nz) = (n[0] + 1, n[1] + 1, n[2] + 1);
        let index = |i: usize, j: usize, k: usize| (i + nx * (j + ny * k)) as u32;
        let mut nodes = Vec::with_capacity(nx * ny * nz);
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let f = [
                        i as f64 / n[0] as f64,
                        j as f64 / n[1] as f64,
                        k as f64 / n[2] as f64,
                    ];
                    nodes.push([
                        lo[0] + (hi[0] - lo[0]) * f[0],
                        lo[1] + (hi[1] - lo[1]) * f[1],
                        lo[2] + (hi[2] - lo[2]) * f[2],
                    ]);
                }
            }
        }
        let mut mesh = TetMesh {
            nodes,
            ..TetMesh::default()
        };
        for k in 0..n[2] {
            for j in 0..n[1] {
                for i in 0..n[0] {
                    let corner = |d: [usize; 3]| index(i + d[0], j + d[1], k + d[2]);
                    for tet in kuhn_tets(corner) {
                        mesh.push_tet(tet, 0);
                    }
                }
            }
        }
        let size = norm(sub(hi, lo)).max(1e-300);
        mesh.find_boundary(|c| {
            let centre = scale(
                [
                    c[0][0] + c[1][0] + c[2][0],
                    c[0][1] + c[1][1] + c[2][1],
                    c[0][2] + c[1][2] + c[2][2],
                ],
                1.0 / 3.0,
            );
            (0..6)
                .find(|&s| {
                    let axis = s / 2;
                    let side = if s % 2 == 0 { lo[axis] } else { hi[axis] };
                    (centre[axis] - side).abs() < 1e-9 * size
                })
                .map_or(crate::NO_LABEL, |s| s as u32)
        });
        mesh
    }

    /// Adds `other`'s nodes, elements and facets after this mesh's own, sharing no node
    /// with them: bodies meshed apart, to be tied by contacts. Its elements keep their
    /// bodies. When only one of the two has edge nodes, the other gets them too.
    pub fn append(&mut self, mut other: TetMesh) {
        if self.tets.is_empty() && self.nodes.is_empty() {
            *self = other;
            return;
        }
        if self.is_quadratic() != other.is_quadratic() {
            self.make_quadratic();
            other.make_quadratic();
        }
        let node_base = self.nodes.len() as u32;
        let tet_base = self.tets.len() as u32;
        self.nodes.extend(other.nodes);
        self.tets
            .extend(other.tets.iter().map(|t| t.map(|i| i + node_base)));
        if let (Some(mine), Some(theirs)) = (&mut self.edge_nodes, &other.edge_nodes) {
            mine.extend(theirs.iter().map(|e| e.map(|i| i + node_base)));
        }
        self.bodies.extend(other.bodies);
        self.facets.extend(other.facets.iter().map(|f| Facet {
            tet: f.tet + tet_base,
            ..*f
        }));
    }

    /// The body each boundary facet belongs to.
    pub fn facet_body(&self, f: &Facet) -> u32 {
        self.bodies.get(f.tet as usize).copied().unwrap_or(0)
    }

    /// Adds an element, turned round if it is inside out.
    pub(crate) fn push_tet(&mut self, mut t: [u32; 4], body: u32) {
        let p = |i: u32| self.nodes[i as usize];
        if tet_volume6(p(t[0]), p(t[1]), p(t[2]), p(t[3])) < 0.0 {
            t.swap(1, 2);
        }
        self.tets.push(t);
        self.bodies.push(body);
    }

    /// Adds a solid meshed on its own (by another mesher, Netgen's say) as body `body`:
    /// its nodes, its four-node elements, and as facets the element faces that are among
    /// its `boundary` triangles, each with the triangle's label. Elements naming a node it
    /// does not have are left out. Call [`TetMesh::make_quadratic`] after the last solid
    /// for ten-node elements.
    pub fn append_solid(
        &mut self,
        nodes: &[V3],
        tets: &[[u32; 4]],
        boundary: &[([u32; 3], u32)],
        body: u32,
    ) {
        let offset = self.nodes.len() as u32;
        let count = nodes.len() as u32;
        self.nodes.extend_from_slice(nodes);
        let labels: HashMap<[u32; 3], u32> = boundary
            .iter()
            .filter(|(t, _)| t.iter().all(|&i| i < count))
            .map(|(t, label)| {
                let mut key = t.map(|i| i + offset);
                key.sort_unstable();
                (key, *label)
            })
            .collect();
        for t in tets.iter().filter(|t| t.iter().all(|&i| i < count)) {
            self.push_tet(t.map(|i| i + offset), body);
            let e = self.tets.len() - 1;
            let tet = self.tets[e];
            for (k, face) in FACES.iter().enumerate() {
                let mut key = face.map(|i| tet[i]);
                key.sort_unstable();
                if let Some(&label) = labels.get(&key) {
                    self.facets.push(Facet {
                        tet: e as u32,
                        face: k as u8,
                        label,
                    });
                }
            }
        }
    }
}

/// The six tetrahedra of a cube's Kuhn (Freudenthal) split, all around the diagonal from
/// corner (0, 0, 0) to (1, 1, 1): one per order of the axes. Neighbouring cubes split
/// this way share their faces' diagonals, so the tetrahedra conform.
pub(crate) fn kuhn_tets<T: Copy>(corner: impl Fn([usize; 3]) -> T) -> [[T; 4]; 6] {
    const ORDERS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    ORDERS.map(|order| {
        let mut d = [0usize; 3];
        let v0 = corner(d);
        d[order[0]] = 1;
        let v1 = corner(d);
        d[order[1]] = 1;
        let v2 = corner(d);
        let v3 = corner([1, 1, 1]);
        [v0, v1, v2, v3]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::normalize;

    #[test]
    fn a_structured_box_fills_its_volume_and_has_an_outward_boundary() {
        let mut mesh = TetMesh::structured_box([0.0; 3], [10.0, 2.0, 3.0], [5, 2, 3]);
        assert!(mesh.validate().is_ok());
        assert_eq!(mesh.tets.len(), 5 * 2 * 3 * 6);
        assert!((mesh.volume() - 60.0).abs() < 1e-9);
        // Two triangles per cell face on the outside.
        let expected = 2 * (5 * 2 + 2 * 3 + 5 * 3) * 2;
        assert_eq!(mesh.facets.len(), expected);
        let mut area = [0.0; 6];
        for f in &mesh.facets {
            assert_ne!(f.label, crate::NO_LABEL);
            let an = mesh.facet_area_normal(f);
            let n = normalize(an).unwrap();
            // Outward: along the side's own axis and sign.
            let axis = f.label as usize / 2;
            let sign = if f.label % 2 == 0 { -1.0 } else { 1.0 };
            assert!((n[axis] - sign).abs() < 1e-12, "{f:?} {n:?}");
            area[f.label as usize] += norm(an);
        }
        assert!((area[0] - 6.0).abs() < 1e-9 && (area[3] - 30.0).abs() < 1e-9);
        mesh.make_quadratic();
        assert!(mesh.validate().is_ok());
        // Nodes: the lattice, plus one per edge.
        let edges = mesh.nodes.len() - 6 * 3 * 4;
        assert!(edges > 0);
        let f = mesh.facets[0];
        let corners = mesh.facet_corners(&f);
        let mids = mesh.facet_edge_nodes(&f).unwrap();
        let m = mesh.nodes[mids[0] as usize];
        let ab = lerp(
            mesh.nodes[corners[0] as usize],
            mesh.nodes[corners[1] as usize],
            0.5,
        );
        assert!(norm(sub(m, ab)) < 1e-12);
    }
}
