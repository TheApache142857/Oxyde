//! 3MF (3D Manufacturing Format) files: a ZIP holding an XML model with one mesh object per
//! named mesh (a part of an assembly, a body of a part), each placed once in the build, in a
//! declared unit. Slicers (PrusaSlicer, Bambu Studio, Cura) and Windows 3D Viewer read it;
//! unlike STL it carries the unit, the object names and a colour per object.
//!
//! The triangles are the same as the STL writer's ([`crate::stl::triangles`]), welded by
//! position within each object; a 3MF mesh must have shared vertices and be oriented
//! outward, which they are.

use std::collections::HashMap;
use std::io::{self, Seek, Write};
use std::path::Path;

use oxyde_kernel::Mesh;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::stl::{self, StlUnit};

/// A mesh to write as one object, named, with an optional colour (sRGB).
pub struct NamedMesh<'a> {
    pub name: &'a str,
    pub mesh: &'a Mesh,
    pub color: Option<[u8; 3]>,
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
</Types>
"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>
"#;

fn unit_name(unit: StlUnit) -> &'static str {
    match unit {
        StlUnit::Millimetre => "millimeter",
        StlUnit::Centimetre => "centimeter",
        StlUnit::Metre => "meter",
        StlUnit::Inch => "inch",
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The model XML: every mesh as an object, each built once at the identity.
pub fn model_xml(meshes: &[NamedMesh<'_>], unit: StlUnit) -> String {
    let scale = unit.per_mm();
    let mut xml = String::new();
    xml.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    xml.push('\n');
    xml.push_str(&format!(
        r#"<model unit="{}" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">"#,
        unit_name(unit)
    ));
    xml.push('\n');
    xml.push_str("  <metadata name=\"Application\">Oxyde</metadata>\n");
    xml.push_str("  <resources>\n");
    // One base material per coloured object, all in one group (id 1); objects follow.
    let colored: Vec<(usize, [u8; 3])> = meshes
        .iter()
        .enumerate()
        .filter_map(|(i, m)| m.color.map(|c| (i, c)))
        .collect();
    let material_index: HashMap<usize, usize> = colored
        .iter()
        .enumerate()
        .map(|(k, (i, _))| (*i, k))
        .collect();
    if !colored.is_empty() {
        xml.push_str("    <basematerials id=\"1\">\n");
        for (i, [r, g, b]) in &colored {
            let name = meshes.get(*i).map_or("", |m| m.name);
            xml.push_str(&format!(
                "      <base name=\"{}\" displaycolor=\"#{r:02X}{g:02X}{b:02X}FF\"/>\n",
                escape(name)
            ));
        }
        xml.push_str("    </basematerials>\n");
    }
    let first_object = 2;
    for (i, m) in meshes.iter().enumerate() {
        let id = first_object + i;
        let material = material_index
            .get(&i)
            .map(|k| format!(" pid=\"1\" pindex=\"{k}\""));
        xml.push_str(&format!(
            "    <object id=\"{id}\" type=\"model\" name=\"{}\"{}>\n",
            escape(m.name),
            material.unwrap_or_default()
        ));
        xml.push_str("      <mesh>\n        <vertices>\n");
        let triangles = stl::triangles(m.mesh, scale);
        let (vertices, faces) = weld(&triangles);
        for [x, y, z] in &vertices {
            xml.push_str(&format!(
                "          <vertex x=\"{x}\" y=\"{y}\" z=\"{z}\"/>\n"
            ));
        }
        xml.push_str("        </vertices>\n        <triangles>\n");
        for [a, b, c] in &faces {
            xml.push_str(&format!(
                "          <triangle v1=\"{a}\" v2=\"{b}\" v3=\"{c}\"/>\n"
            ));
        }
        xml.push_str("        </triangles>\n      </mesh>\n    </object>\n");
    }
    xml.push_str("  </resources>\n  <build>\n");
    for i in 0..meshes.len() {
        xml.push_str(&format!("    <item objectid=\"{}\"/>\n", first_object + i));
    }
    xml.push_str("  </build>\n</model>\n");
    xml
}

/// Shared vertices (welded to a micron) and the triangles as index triples.
fn weld(triangles: &[stl::StlTriangle]) -> (Vec<[f32; 3]>, Vec<[usize; 3]>) {
    let key = |v: [f32; 3]| v.map(|c| (f64::from(c) * 1.0e6).round() as i64);
    let mut index: HashMap<[i64; 3], usize> = HashMap::new();
    let mut vertices = Vec::new();
    let mut faces = Vec::with_capacity(triangles.len());
    for t in triangles {
        let ids = t.vertices.map(|v| {
            *index.entry(key(v)).or_insert_with(|| {
                vertices.push(v);
                vertices.len() - 1
            })
        });
        if ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2] {
            faces.push(ids);
        }
    }
    (vertices, faces)
}

/// Writes the 3MF package.
pub fn write<W: Write + Seek>(w: W, meshes: &[NamedMesh<'_>], unit: StlUnit) -> io::Result<W> {
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut zip = ZipWriter::new(w);
    for (name, text) in [
        ("[Content_Types].xml", CONTENT_TYPES.to_owned()),
        ("_rels/.rels", RELS.to_owned()),
        ("3D/3dmodel.model", model_xml(meshes, unit)),
    ] {
        zip.start_file(name, options).map_err(io::Error::other)?;
        zip.write_all(text.as_bytes())?;
    }
    zip.finish().map_err(io::Error::other)
}

/// Writes the 3MF package to `path`.
pub fn save(path: &Path, meshes: &[NamedMesh<'_>], unit: StlUnit) -> io::Result<()> {
    let file = io::BufWriter::new(std::fs::File::create(path)?);
    write(file, meshes, unit).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxyde_kernel::FaceMesh;
    use std::io::{Cursor, Read};

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
    fn the_package_holds_a_model_with_named_coloured_objects_in_the_unit() {
        let a = square();
        let b = square();
        let meshes = [
            NamedMesh {
                name: "Plate <A>",
                mesh: &a,
                color: Some([255, 0, 0]),
            },
            NamedMesh {
                name: "Lid",
                mesh: &b,
                color: None,
            },
        ];
        let bytes = write(Cursor::new(Vec::new()), &meshes, StlUnit::Inch)
            .unwrap()
            .into_inner();
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert!(zip.by_name("[Content_Types].xml").is_ok());
        assert!(zip.by_name("_rels/.rels").is_ok());
        let mut model = String::new();
        zip.by_name("3D/3dmodel.model")
            .unwrap()
            .read_to_string(&mut model)
            .unwrap();
        assert!(model.contains(r#"unit="inch""#));
        assert!(
            model.contains(r#"name="Plate &lt;A&gt;" pid="1" pindex="0""#),
            "{model}"
        );
        assert!(model.contains(r##"displaycolor="#FF0000FF""##));
        assert!(model.contains(r#"<object id="3" type="model" name="Lid">"#));
        assert_eq!(model.matches("<vertex ").count(), 8);
        assert_eq!(model.matches("<triangle ").count(), 4);
        assert_eq!(model.matches("<item ").count(), 2);
        // Inches: the square is 1 mm, written as 1/25.4.
        assert!(model.contains("x=\"0.0393700"), "{model}");
    }
}
