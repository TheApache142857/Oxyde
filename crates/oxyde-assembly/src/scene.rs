//! One display mesh for the whole assembly, and the way back from what was picked in it to
//! what it belongs to: a component, or a part deep inside a sub-assembly (an *occurrence*,
//! whatever key the caller uses).

use std::ops::Range;

use oxyde_kernel::{FaceMesh, Mesh};

use crate::Placement;

/// Which faces and edges of the combined mesh belong to an occurrence `K`.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshRange<K> {
    pub owner: K,
    pub faces: Range<u32>,
    pub edges: Range<u32>,
}

/// A copy of `mesh` moved to `placement`, faces and edges in the same order.
pub fn placed(mesh: &Mesh, placement: &Placement) -> Mesh {
    let place = |p: &[f32; 3]| {
        let q = placement.apply_point(oxyde_geom::Point3::new(
            f64::from(p[0]),
            f64::from(p[1]),
            f64::from(p[2]),
        ));
        [q.x as f32, q.y as f32, q.z as f32]
    };
    Mesh {
        faces: mesh
            .faces
            .iter()
            .map(|f| FaceMesh {
                positions: f.positions.iter().map(place).collect(),
                triangles: f.triangles.clone(),
            })
            .collect(),
        edges: mesh
            .edges
            .iter()
            .map(|e| e.iter().map(place).collect())
            .collect(),
    }
}

/// The meshes of `parts`, each moved to its placement, as one mesh: faces and edges keep
/// their order, component after component.
pub fn combine<'a, K>(
    parts: impl IntoIterator<Item = (K, &'a Placement, &'a Mesh)>,
) -> (Mesh, Vec<MeshRange<K>>) {
    let mut out = Mesh {
        faces: Vec::new(),
        edges: Vec::new(),
    };
    let mut ranges = Vec::new();
    for (owner, placement, mesh) in parts {
        let faces = out.faces.len() as u32;
        let edges = out.edges.len() as u32;
        let moved = placed(mesh, placement);
        out.faces.extend(moved.faces);
        out.edges.extend(moved.edges);
        ranges.push(MeshRange {
            owner,
            faces: faces..out.faces.len() as u32,
            edges: edges..out.edges.len() as u32,
        });
    }
    (out, ranges)
}

/// What a face of the combined mesh belongs to, and the face's index in its part.
pub fn face_owner<K>(ranges: &[MeshRange<K>], face: u32) -> Option<(&K, u32)> {
    ranges
        .iter()
        .find(|r| r.faces.contains(&face))
        .map(|r| (&r.owner, face - r.faces.start))
}

/// Same for an edge.
pub fn edge_owner<K>(ranges: &[MeshRange<K>], edge: u32) -> Option<(&K, u32)> {
    ranges
        .iter()
        .find(|r| r.edges.contains(&edge))
        .map(|r| (&r.owner, edge - r.edges.start))
}

/// Where a part's face sits in the combined mesh.
pub fn face_index<K: PartialEq>(ranges: &[MeshRange<K>], owner: &K, face: u32) -> Option<u32> {
    let r = ranges.iter().find(|r| r.owner == *owner)?;
    let i = r.faces.start + face;
    r.faces.contains(&i).then_some(i)
}

/// Same for an edge.
pub fn edge_index<K: PartialEq>(ranges: &[MeshRange<K>], owner: &K, edge: u32) -> Option<u32> {
    let r = ranges.iter().find(|r| r.owner == *owner)?;
    let i = r.edges.start + edge;
    r.edges.contains(&i).then_some(i)
}
