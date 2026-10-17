//! Wavefront OBJ meshes: the triangles of display [`Mesh`]es, one `o` object per named mesh
//! (a part of an assembly, a body of a part), with vertex normals so that renderers shade
//! them smoothly, for Blender, Unity, game engines and most mesh tools. OBJ has no unit:
//! coordinates are written as given, scaled by the caller.
//!
//! The triangles are the same as the STL writer's ([`crate::stl::triangles`]): degenerate
//! ones dropped, counter-clockwise seen from outside.

use std::io::{self, Write};
use std::path::Path;

use oxyde_kernel::Mesh;

use crate::stl::{self, StlTriangle};

/// A mesh to write as one object, named.
pub struct NamedMesh<'a> {
    pub name: &'a str,
    pub mesh: &'a Mesh,
}

/// A name usable after `o ` in an OBJ file: a single token, no spaces.
fn token(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_whitespace() || c == '#' {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.is_empty() {
        "object".to_owned()
    } else {
        cleaned
    }
}

/// Writes `meshes` as OBJ, every coordinate multiplied by `scale`. Vertices are shared
/// within an object where they coincide (to a micron), as OBJ readers expect.
pub fn write<W: Write>(mut w: W, meshes: &[NamedMesh<'_>], scale: f64) -> io::Result<()> {
    writeln!(w, "# Oxyde OBJ export")?;
    let mut offset: usize = 1;
    for (k, m) in meshes.iter().enumerate() {
        let triangles = stl::triangles(m.mesh, scale);
        let (vertices, normals, faces) = weld(&triangles);
        let mut name = token(m.name);
        if meshes.iter().filter(|o| token(o.name) == name).count() > 1 {
            name = format!("{name}_{}", k + 1);
        }
        writeln!(w, "o {name}")?;
        for [x, y, z] in &vertices {
            writeln!(w, "v {x} {y} {z}")?;
        }
        for [x, y, z] in &normals {
            writeln!(w, "vn {x} {y} {z}")?;
        }
        for (vertex, normal) in &faces {
            // Indices are global to the file and start at 1.
            writeln!(
                w,
                "f {}//{} {}//{} {}//{}",
                vertex[0] + offset,
                normal[0] + offset,
                vertex[1] + offset,
                normal[1] + offset,
                vertex[2] + offset,
                normal[2] + offset
            )?;
        }
        // Normals are written one per vertex, so both index spaces advance together.
        offset += vertices.len();
    }
    w.flush()
}

/// Shared vertices with one normal each (a vertex met with several normals, at a sharp
/// edge, is kept once per normal, so edges stay sharp), and the faces as index triples.
#[allow(clippy::type_complexity)]
fn weld(
    triangles: &[StlTriangle],
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<([usize; 3], [usize; 3])>) {
    use std::collections::HashMap;
    let key = |v: [f32; 3], n: [f32; 3]| {
        let q = |c: f32, s: f64| (f64::from(c) * s).round() as i64;
        [
            q(v[0], 1.0e6),
            q(v[1], 1.0e6),
            q(v[2], 1.0e6),
            q(n[0], 1.0e3),
            q(n[1], 1.0e3),
            q(n[2], 1.0e3),
        ]
    };
    let mut index: HashMap<[i64; 6], usize> = HashMap::new();
    let mut vertices = Vec::new();
    let mut normals = Vec::new();
    let mut faces = Vec::with_capacity(triangles.len());
    for t in triangles {
        let ids = t.vertices.map(|v| {
            *index.entry(key(v, t.normal)).or_insert_with(|| {
                vertices.push(v);
                normals.push(t.normal);
                vertices.len() - 1
            })
        });
        faces.push((ids, ids));
    }
    (vertices, normals, faces)
}

/// Writes `meshes` to `path`.
pub fn save(path: &Path, meshes: &[NamedMesh<'_>], scale: f64) -> io::Result<()> {
    let file = io::BufWriter::new(std::fs::File::create(path)?);
    write(file, meshes, scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxyde_kernel::FaceMesh;

    fn square() -> Mesh {
        Mesh {
            faces: vec![FaceMesh {
                positions: vec![[0.0; 3], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]],
                triangles: vec![[0, 1, 2], [0, 2, 3]],
            }],
            edges: Vec::new(),
        }
    }

    #[test]
    fn objects_are_named_and_indices_run_through_the_file() {
        let a = square();
        let b = square();
        let mut out = Vec::new();
        write(
            &mut out,
            &[
                NamedMesh {
                    name: "Base plate",
                    mesh: &a,
                },
                NamedMesh {
                    name: "Lid",
                    mesh: &b,
                },
            ],
            1.0,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("o Base_plate\n"));
        assert!(text.contains("o Lid\n"));
        // Four shared vertices per square (coplanar: one normal), faces of the second
        // object start at index 5.
        assert_eq!(text.matches("\nv ").count(), 8);
        assert!(text.contains("f 5//5 6//6 7//7\n"), "{text}");
    }
}
