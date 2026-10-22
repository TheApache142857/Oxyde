//! Display meshes: one triangle mesh per face and one polyline per edge, numbered like the
//! shape's topology.
//!
//! Every face is planar and every edge straight for now, so the mesh is exact and the
//! deflection asked for does not change it.

// Indices into the solid's own vertices, and into the face's corners gathered here.
#![allow(clippy::indexing_slicing)]

use oxyde_geom::{Point2, Point3};
use oxyde_kernel::{FaceMesh, Mesh};

use crate::brep::{Face, Shape, Solid};
use crate::triangulate;

pub(crate) fn mesh(shape: &Shape) -> Result<Mesh, String> {
    let mut mesh = Mesh::default();
    for solid in &shape.solids {
        for face in &solid.faces {
            mesh.faces.push(face_mesh(solid, face)?);
        }
        for edge in &solid.edges {
            mesh.edges.push(vec![
                single(solid.vertices[edge.start as usize]),
                single(solid.vertices[edge.end as usize]),
            ]);
        }
    }
    Ok(mesh)
}

fn single(p: Point3) -> [f32; 3] {
    [p.x as f32, p.y as f32, p.z as f32]
}

/// The face's corners, each once (vertices of the solid), and triangles over them (indices
/// into the corners), triangulated in the face's plane. The plane's frame is right-handed
/// about the outward normal, so triangles counter-clockwise in it are counter-clockwise
/// seen from outside.
pub(crate) fn triangulate_face(
    solid: &Solid,
    face: &Face,
) -> Result<(Vec<u32>, Vec<[usize; 3]>), String> {
    let mut corners: Vec<u32> = Vec::new();
    let mut loops: Vec<Vec<usize>> = Vec::with_capacity(face.loops.len());
    for edges in &face.loops {
        let mut ring = Vec::with_capacity(edges.len());
        for &edge_use in edges {
            let vertex = solid.start_of(edge_use);
            let local = match corners.iter().position(|&v| v == vertex) {
                Some(local) => local,
                None => {
                    corners.push(vertex);
                    corners.len() - 1
                }
            };
            ring.push(local);
        }
        loops.push(ring);
    }
    let (origin, x) = (face.plane.origin, face.plane.x_dir);
    let y = face.plane.normal.cross(x);
    let flat: Vec<Point2> = corners
        .iter()
        .map(|&v| {
            let d = solid.vertices[v as usize] - origin;
            Point2::new(d.dot(x), d.dot(y))
        })
        .collect();
    let triangles = triangulate::polygon_with_holes(&flat, &loops)?;
    Ok((corners, triangles))
}

fn face_mesh(solid: &Solid, face: &Face) -> Result<FaceMesh, String> {
    let (corners, triangles) = triangulate_face(solid, face)?;
    Ok(FaceMesh {
        positions: corners
            .iter()
            .map(|&v| single(solid.vertices[v as usize]))
            .collect(),
        triangles: triangles
            .iter()
            .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
            .collect(),
    })
}
