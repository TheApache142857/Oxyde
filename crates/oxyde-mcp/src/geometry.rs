//! Solids seen together for inspection: the part's bodies, a reference's placed solids or a
//! component's, with their topology merged as the model merges a part's bodies (the faces
//! of the first solid, then those of the second…), and their display meshes indexed the
//! same way.

use std::ops::Range;

use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{Kernel, Mesh, ShapeHandle, Topology};
use oxyde_model::{Body, BodyNaming};
use oxyde_occt::OcctKernel;

/// Where one solid's faces and edges sit in the merged topology.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SolidRange {
    pub shape: ShapeHandle,
    pub faces: Range<u32>,
    pub edges: Range<u32>,
}

/// Solids with their merged topology, and their persistent names when they are a part's.
pub(crate) struct Solids<'a> {
    pub solids: Vec<SolidRange>,
    pub topology: &'a Topology,
    pub naming: Option<&'a BodyNaming>,
}

impl<'a> Solids<'a> {
    pub fn of_body(body: &'a Body) -> Self {
        Self {
            solids: body
                .solids
                .iter()
                .map(|s| SolidRange {
                    shape: s.shape,
                    faces: s.faces.clone(),
                    edges: s.edges.clone(),
                })
                .collect(),
            topology: &body.topology,
            naming: Some(&body.naming),
        }
    }

    pub fn shapes(&self) -> Vec<ShapeHandle> {
        self.solids.iter().map(|s| s.shape).collect()
    }

    /// The display mesh of every solid, indexed like the merged topology.
    pub fn mesh(&self, kernel: &OcctKernel) -> Result<Mesh, String> {
        let deflection = oxyde_render::mesh_deflection(self.topology, 0.0005);
        let mut out = Mesh::default();
        for solid in &self.solids {
            let mesh = kernel
                .tessellate(solid.shape, deflection)
                .map_err(|e| e.to_string())?;
            out.faces.extend(mesh.faces);
            out.edges.extend(mesh.edges);
        }
        Ok(out)
    }

    /// The exact box around every solid.
    pub fn bounds(&self, kernel: &OcctKernel) -> Option<(Point3, Point3)> {
        bounds(kernel, &self.shapes())
    }

    /// The solid holding face `face`.
    pub fn solid_of_face(&self, face: u32) -> Option<&SolidRange> {
        self.solids.iter().find(|s| s.faces.contains(&face))
    }

    pub fn solid_of_edge(&self, edge: u32) -> Option<&SolidRange> {
        self.solids.iter().find(|s| s.edges.contains(&edge))
    }

    /// A face's persistent name, as the model prints it, or its index.
    pub fn face_label(&self, face: u32) -> String {
        self.naming
            .and_then(|n| n.face(face))
            .map_or_else(|| format!("face {face}"), ToString::to_string)
    }

    pub fn edge_label(&self, edge: u32) -> String {
        self.naming
            .and_then(|n| n.edge(edge))
            .map_or_else(|| format!("edge {edge}"), ToString::to_string)
    }
}

/// The box around several shapes, `None` when there are none.
pub(crate) fn bounds(kernel: &OcctKernel, shapes: &[ShapeHandle]) -> Option<(Point3, Point3)> {
    let mut out: Option<(Point3, Point3)> = None;
    for shape in shapes {
        let Ok((lo, hi)) = kernel.bounds(*shape) else {
            continue;
        };
        out = Some(match out {
            None => (lo, hi),
            Some((a, b)) => (
                Point3::new(a.x.min(lo.x), a.y.min(lo.y), a.z.min(lo.z)),
                Point3::new(b.x.max(hi.x), b.y.max(hi.y), b.z.max(hi.z)),
            ),
        });
    }
    out
}

/// Several solids' topologies as one, indices shifted, and where each solid's faces and
/// edges went.
pub(crate) fn merge(parts: &[(ShapeHandle, Topology)]) -> (Topology, Vec<SolidRange>) {
    let mut out = Topology::default();
    let mut ranges = Vec::new();
    for (shape, topology) in parts {
        let (f0, e0, v0) = (
            out.faces.len() as u32,
            out.edges.len() as u32,
            out.vertices.len() as u32,
        );
        for face in &topology.faces {
            let mut face = face.clone();
            face.edges.iter_mut().for_each(|e| *e += e0);
            out.faces.push(face);
        }
        for edge in &topology.edges {
            let mut edge = edge.clone();
            edge.vertices.iter_mut().for_each(|v| *v += v0);
            edge.faces.iter_mut().for_each(|f| *f += f0);
            out.edges.push(edge);
        }
        out.vertices.extend(topology.vertices.iter().copied());
        ranges.push(SolidRange {
            shape: *shape,
            faces: f0..out.faces.len() as u32,
            edges: e0..out.edges.len() as u32,
        });
    }
    (out, ranges)
}

/// Distance from `p` to the triangle `a b c`.
pub(crate) fn point_triangle_distance(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> f64 {
    // Ericson, Real-Time Collision Detection, 5.1.5: the closest point by region.
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return (p - a).length();
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return (p - b).length();
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return (p - (a + ab * v)).length();
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return (p - c).length();
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return (p - (a + ac * w)).length();
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return (p - (b + (c - b) * w)).length();
    }
    let denom = va + vb + vc;
    if denom.abs() < 1e-300 {
        return (p - a).length();
    }
    let (v, w) = (vb / denom, vc / denom);
    (p - (a + ab * v + ac * w)).length()
}

/// Distance from `p` to the segment `a b`.
pub(crate) fn point_segment_distance(p: Vec3, a: Vec3, b: Vec3) -> f64 {
    let ab = b - a;
    let len2 = ab.dot(ab);
    if len2 < 1e-300 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

pub(crate) fn v3(p: [f32; 3]) -> Vec3 {
    Vec3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_to_triangles_and_segments() {
        let (a, b, c) = (Vec3::ZERO, Vec3::X, Vec3::Y);
        assert!((point_triangle_distance(Vec3::new(0.2, 0.2, 3.0), a, b, c) - 3.0).abs() < 1e-12);
        assert!((point_triangle_distance(Vec3::new(-1.0, 0.0, 0.0), a, b, c) - 1.0).abs() < 1e-12);
        let d = point_triangle_distance(Vec3::new(1.0, 1.0, 0.0), a, b, c);
        assert!((d - 0.5f64.sqrt()).abs() < 1e-12);
        assert!((point_segment_distance(Vec3::new(0.5, 2.0, 0.0), a, b) - 2.0).abs() < 1e-12);
        assert!((point_segment_distance(Vec3::new(3.0, 0.0, 0.0), a, b) - 2.0).abs() < 1e-12);
    }
}
