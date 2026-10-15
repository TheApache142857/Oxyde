//! STL meshes, for 3D printing: the triangles of a display [`Mesh`], scaled to the unit
//! the slicer expects, written as binary STL (compact, the usual choice) or ASCII STL, with
//! a report on the mesh first: how many triangles, how big the file, and whether the mesh
//! is watertight, which slicers need.
//!
//! STL has no unit; slicers assume millimetres. Faces are triangulated one by one, so the
//! nodes along an edge shared by two faces are written twice, once per face, at the same
//! place: the report welds them (within a micron) to count vertices and check edges.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

use oxyde_kernel::Mesh;

/// The unit the coordinates are written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StlUnit {
    Millimetre,
    Centimetre,
    Metre,
    Inch,
}

impl StlUnit {
    pub const ALL: [StlUnit; 4] = [
        StlUnit::Millimetre,
        StlUnit::Centimetre,
        StlUnit::Metre,
        StlUnit::Inch,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StlUnit::Millimetre => "Millimetres (what slicers expect)",
            StlUnit::Centimetre => "Centimetres",
            StlUnit::Metre => "Metres",
            StlUnit::Inch => "Inches",
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            StlUnit::Millimetre => "mm",
            StlUnit::Centimetre => "cm",
            StlUnit::Metre => "m",
            StlUnit::Inch => "in",
        }
    }

    /// How many of this unit one millimetre is.
    pub fn per_mm(self) -> f64 {
        match self {
            StlUnit::Millimetre => 1.0,
            StlUnit::Centimetre => 0.1,
            StlUnit::Metre => 0.001,
            StlUnit::Inch => 1.0 / 25.4,
        }
    }
}

/// One triangle of the file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StlTriangle {
    /// Unit normal, outward.
    pub normal: [f32; 3],
    /// Counter-clockwise seen from outside.
    pub vertices: [[f32; 3]; 3],
}

/// The triangles of `mesh` with every coordinate multiplied by `scale`; degenerate
/// triangles (no area) are left out.
pub fn triangles(mesh: &Mesh, scale: f64) -> Vec<StlTriangle> {
    let mut out = Vec::new();
    for face in &mesh.faces {
        for t in &face.triangles {
            let Some(vertices) = t
                .iter()
                .map(|&i| face.positions.get(i as usize))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let p: [[f64; 3]; 3] = std::array::from_fn(|k| {
                let v = vertices[k];
                [
                    f64::from(v[0]) * scale,
                    f64::from(v[1]) * scale,
                    f64::from(v[2]) * scale,
                ]
            });
            let u = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
            let v = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if !(length > 0.0) || !length.is_finite() {
                continue;
            }
            out.push(StlTriangle {
                normal: [
                    (n[0] / length) as f32,
                    (n[1] / length) as f32,
                    (n[2] / length) as f32,
                ],
                vertices: p.map(|q| [q[0] as f32, q[1] as f32, q[2] as f32]),
            });
        }
    }
    out
}

/// What the mesh is like, for the export window.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshReport {
    pub triangles: usize,
    /// Distinct corners, shared edges welded.
    pub vertices: usize,
    /// Edges used by one triangle only: holes in the surface.
    pub open_edges: usize,
    /// Edges used by three triangles or more: surfaces meeting in a line.
    pub crowded_edges: usize,
    /// Corners of the box around the mesh, in the file's unit.
    pub bounds: Option<([f32; 3], [f32; 3])>,
    /// Bytes of the binary file.
    pub binary_size: u64,
    /// About the bytes of the ASCII file.
    pub ascii_size: u64,
}

impl MeshReport {
    /// Every edge is shared by exactly two triangles: a closed surface slicers can fill.
    pub fn watertight(&self) -> bool {
        self.triangles > 0 && self.open_edges == 0 && self.crowded_edges == 0
    }
}

/// Measures `triangles`.
pub fn report(triangles: &[StlTriangle]) -> MeshReport {
    // Welded by position, to a micron (STL readers do the same).
    let key = |v: [f32; 3]| v.map(|c| (f64::from(c) * 1.0e6).round() as i64);
    let mut vertices: HashMap<[i64; 3], u32> = HashMap::new();
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    let mut bounds: Option<([f32; 3], [f32; 3])> = None;
    for t in triangles {
        let ids: [u32; 3] = t.vertices.map(|v| {
            bounds = Some(match bounds {
                None => (v, v),
                Some((lo, hi)) => (
                    [lo[0].min(v[0]), lo[1].min(v[1]), lo[2].min(v[2])],
                    [hi[0].max(v[0]), hi[1].max(v[1]), hi[2].max(v[2])],
                ),
            });
            let next = vertices.len() as u32;
            *vertices.entry(key(v)).or_insert(next)
        });
        for k in 0..3 {
            let (a, b) = (ids[k], ids[(k + 1) % 3]);
            if a != b {
                *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
    }
    MeshReport {
        triangles: triangles.len(),
        vertices: vertices.len(),
        open_edges: edges.values().filter(|n| **n == 1).count(),
        crowded_edges: edges.values().filter(|n| **n > 2).count(),
        bounds,
        binary_size: 84 + 50 * triangles.len() as u64,
        // "facet normal", "outer loop", three "vertex" lines, "endloop", "endfacet":
        // about 240 bytes with numbers of ordinary length.
        ascii_size: 20 + 240 * triangles.len() as u64,
    }
}

/// Writes binary STL: an 80-byte header, the triangle count, then 50 bytes per triangle.
pub fn write_binary<W: Write>(mut w: W, name: &str, triangles: &[StlTriangle]) -> io::Result<()> {
    // A binary file must not start with "solid", which marks ASCII files.
    let mut header = format!("Oxyde binary STL: {name}").into_bytes();
    header.truncate(80);
    header.resize(80, 0);
    w.write_all(&header)?;
    w.write_all(&(triangles.len() as u32).to_le_bytes())?;
    for t in triangles {
        for c in t.normal.iter().chain(t.vertices.iter().flatten()) {
            w.write_all(&c.to_le_bytes())?;
        }
        w.write_all(&0u16.to_le_bytes())?;
    }
    w.flush()
}

/// Writes ASCII STL.
pub fn write_ascii<W: Write>(mut w: W, name: &str, triangles: &[StlTriangle]) -> io::Result<()> {
    let name: String = name
        .chars()
        .map(|c| if c.is_ascii_graphic() { c } else { '_' })
        .collect();
    writeln!(w, "solid {name}")?;
    for t in triangles {
        let [nx, ny, nz] = t.normal;
        writeln!(w, "  facet normal {nx:e} {ny:e} {nz:e}")?;
        writeln!(w, "    outer loop")?;
        for [x, y, z] in t.vertices {
            writeln!(w, "      vertex {x:e} {y:e} {z:e}")?;
        }
        writeln!(w, "    endloop")?;
        writeln!(w, "  endfacet")?;
    }
    writeln!(w, "endsolid {name}")?;
    w.flush()
}

/// Writes the triangles to `path`, binary or ASCII.
pub fn save(path: &Path, name: &str, triangles: &[StlTriangle], binary: bool) -> io::Result<()> {
    let file = io::BufWriter::new(std::fs::File::create(path)?);
    if binary {
        write_binary(file, name, triangles)
    } else {
        write_ascii(file, name, triangles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxyde_kernel::FaceMesh;

    /// A 10 mm cube, one face mesh per side, each two triangles, wound outward.
    fn cube() -> Mesh {
        let quad = |corners: [[f32; 3]; 4]| FaceMesh {
            positions: corners.to_vec(),
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        };
        let (a, b) = (0.0, 10.0);
        Mesh {
            faces: vec![
                quad([[a, a, b], [b, a, b], [b, b, b], [a, b, b]]), // front (+Z)
                quad([[b, a, a], [a, a, a], [a, b, a], [b, b, a]]), // back (-Z)
                quad([[b, a, b], [b, a, a], [b, b, a], [b, b, b]]), // right (+X)
                quad([[a, a, a], [a, a, b], [a, b, b], [a, b, a]]), // left (-X)
                quad([[a, b, b], [b, b, b], [b, b, a], [a, b, a]]), // top (+Y)
                quad([[a, a, a], [b, a, a], [b, a, b], [a, a, b]]), // bottom (-Y)
            ],
            edges: Vec::new(),
        }
    }

    #[test]
    fn a_closed_cube_is_watertight_with_outward_normals() {
        let tris = triangles(&cube(), 1.0);
        assert_eq!(tris.len(), 12);
        assert_eq!(tris[0].normal, [0.0, 0.0, 1.0]);
        assert_eq!(tris[10].normal, [0.0, -1.0, 0.0]);
        let r = report(&tris);
        assert!(r.watertight(), "{r:?}");
        assert_eq!((r.vertices, r.open_edges, r.crowded_edges), (8, 0, 0));
        assert_eq!(r.bounds, Some(([0.0; 3], [10.0; 3])));
        assert_eq!(r.binary_size, 84 + 12 * 50);
    }

    #[test]
    fn a_missing_side_leaves_open_edges_and_units_scale() {
        let mut mesh = cube();
        mesh.faces.pop();
        let tris = triangles(&mesh, StlUnit::Inch.per_mm());
        let r = report(&tris);
        assert_eq!((r.triangles, r.open_edges), (10, 4));
        assert!(!r.watertight());
        let (_, hi) = r.bounds.unwrap();
        assert!((hi[0] - 10.0 / 25.4).abs() < 1e-6);
        // A degenerate triangle is dropped.
        mesh.faces[0].triangles.push([0, 1, 1]);
        assert_eq!(triangles(&mesh, 1.0).len(), 10);
    }

    #[test]
    fn binary_and_ascii_files_have_the_expected_shape() {
        let tris = triangles(&cube(), 1.0);
        let mut binary = Vec::new();
        write_binary(&mut binary, "cube", &tris).unwrap();
        assert_eq!(binary.len() as u64, report(&tris).binary_size);
        assert!(binary.starts_with(b"Oxyde binary STL: cube"));
        assert_eq!(u32::from_le_bytes(binary[80..84].try_into().unwrap()), 12);
        // The first triangle's normal and first vertex come straight after the count.
        let f = |at: usize| f32::from_le_bytes(binary[at..at + 4].try_into().unwrap());
        assert_eq!([f(84), f(88), f(92)], [0.0, 0.0, 1.0]);
        assert_eq!([f(96), f(100), f(104)], [0.0, 0.0, 10.0]);
        assert_eq!(&binary[84 + 48..84 + 50], &[0, 0]);

        let mut ascii = Vec::new();
        write_ascii(&mut ascii, "my cube", &tris).unwrap();
        let text = String::from_utf8(ascii).unwrap();
        assert!(text.starts_with("solid my_cube\n  facet normal"));
        assert!(text.trim_end().ends_with("endsolid my_cube"));
        assert_eq!(text.matches("endfacet").count(), 12);
        assert_eq!(text.matches("vertex").count(), 36);
    }
}
