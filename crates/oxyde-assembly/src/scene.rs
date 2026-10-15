//! One display mesh for the whole assembly, and the way back from what was picked in it to
//! a component.

use std::ops::Range;

use oxyde_kernel::{FaceMesh, Mesh};

use crate::{ComponentId, Placement};

/// Which faces and edges of the combined mesh belong to a component.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshRange {
    pub component: ComponentId,
    pub faces: Range<u32>,
    pub edges: Range<u32>,
}

/// The meshes of `parts`, each moved to its placement, as one mesh: faces and edges keep
/// their order, component after component.
pub fn combine<'a>(
    parts: impl IntoIterator<Item = (ComponentId, &'a Placement, &'a Mesh)>,
) -> (Mesh, Vec<MeshRange>) {
    let mut out = Mesh {
        faces: Vec::new(),
        edges: Vec::new(),
    };
    let mut ranges = Vec::new();
    for (component, placement, mesh) in parts {
        let place = |p: &[f32; 3]| {
            let q = placement.apply_point(oxyde_geom::Point3::new(
                f64::from(p[0]),
                f64::from(p[1]),
                f64::from(p[2]),
            ));
            [q.x as f32, q.y as f32, q.z as f32]
        };
        let faces = out.faces.len() as u32;
        let edges = out.edges.len() as u32;
        out.faces.extend(mesh.faces.iter().map(|f| FaceMesh {
            positions: f.positions.iter().map(place).collect(),
            triangles: f.triangles.clone(),
        }));
        out.edges
            .extend(mesh.edges.iter().map(|e| e.iter().map(place).collect()));
        ranges.push(MeshRange {
            component,
            faces: faces..out.faces.len() as u32,
            edges: edges..out.edges.len() as u32,
        });
    }
    (out, ranges)
}

/// The component a face of the combined mesh belongs to, and the face's index in its part.
pub fn face_owner(ranges: &[MeshRange], face: u32) -> Option<(ComponentId, u32)> {
    ranges
        .iter()
        .find(|r| r.faces.contains(&face))
        .map(|r| (r.component, face - r.faces.start))
}

/// Same for an edge.
pub fn edge_owner(ranges: &[MeshRange], edge: u32) -> Option<(ComponentId, u32)> {
    ranges
        .iter()
        .find(|r| r.edges.contains(&edge))
        .map(|r| (r.component, edge - r.edges.start))
}

/// Where a part's face sits in the combined mesh.
pub fn face_index(ranges: &[MeshRange], component: ComponentId, face: u32) -> Option<u32> {
    let r = ranges.iter().find(|r| r.component == component)?;
    let i = r.faces.start + face;
    r.faces.contains(&i).then_some(i)
}

/// Same for an edge.
pub fn edge_index(ranges: &[MeshRange], component: ComponentId, edge: u32) -> Option<u32> {
    let r = ranges.iter().find(|r| r.component == component)?;
    let i = r.edges.start + edge;
    r.edges.contains(&i).then_some(i)
}
