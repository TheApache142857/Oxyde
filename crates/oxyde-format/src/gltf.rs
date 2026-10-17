//! glTF 2.0 binary files (`.glb`): the interchange format Blender, three.js, game engines
//! and Windows' 3D Viewer read best, with a scene tree, named objects and physically
//! based materials. One node per named mesh (a part of an assembly, a body of a part),
//! one primitive per material within it, smooth normals within each face and sharp ones
//! across edges, as in the viewport.
//!
//! glTF's unit is the metre and its up axis Y, like Oxyde's world: coordinates are
//! written in metres, nothing is turned. Materials are the metallic-roughness model, with
//! `KHR_materials_transmission` and `KHR_materials_ior` for glass, which Blender maps to
//! its Principled BSDF. Textures are left out for now: Oxyde maps them along the model's
//! axes and stores no texture coordinates.
//!
//! See `docs/RENDERING.md`; this is the first step of the link to Blender.

use std::io::{self, Write};
use std::path::Path;

use oxyde_kernel::Mesh;
use oxyde_model::Finish;
use serde_json::{Value, json};

/// A physically based material, as glTF's metallic-roughness model has it.
#[derive(Debug, Clone, PartialEq)]
pub struct GltfMaterial {
    pub name: String,
    /// Linear RGB and alpha (1 = opaque), 0..1.
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    /// 0 = opaque, 1 = clear glass.
    pub transmission: f32,
    pub ior: f32,
}

impl GltfMaterial {
    /// The material of a surface of sRGB colour `color`, see-through by `transparency`
    /// (0 = opaque), in `finish`: the finish's metallic, roughness and transmission over
    /// the colour (glass lightly tinted and opaque, its transmission doing the rest).
    pub fn from_finish(color: [u8; 3], transparency: f64, finish: Finish) -> GltfMaterial {
        let p = finish.params();
        let glass = finish == Finish::Glass;
        let lin = finish
            .render_color(color)
            .map(|c| srgb_to_linear(f32::from(c)));
        let alpha = if glass {
            1.0
        } else {
            (1.0 - transparency as f32).clamp(0.0, 1.0)
        };
        GltfMaterial {
            name: format!(
                "{} #{:02x}{:02x}{:02x}",
                finish.label(),
                color[0],
                color[1],
                color[2]
            ),
            base_color: [lin[0], lin[1], lin[2], alpha],
            metallic: p.metallic as f32,
            roughness: p.roughness as f32,
            transmission: p.transmission as f32,
            ior: p.ior as f32,
        }
    }
}

/// An 8-bit sRGB channel as linear light, 0..1 (glTF's colour factors are linear).
fn srgb_to_linear(c: f32) -> f32 {
    let c = (c / 255.0).clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// A mesh to write as one named object, face `k` with material `face_materials[k]` (an
/// index into the materials passed alongside; the first where the list is short).
pub struct GltfObject<'a> {
    pub name: &'a str,
    pub mesh: &'a Mesh,
    pub face_materials: &'a [usize],
}

/// Millimetres to metres.
const SCALE: f32 = 0.001;

const GLB_MAGIC: u32 = 0x4654_6C67;
const CHUNK_JSON: u32 = 0x4E4F_534A;
const CHUNK_BIN: u32 = 0x004E_4942;
const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const FLOAT: u32 = 5126;
const UNSIGNED_INT: u32 = 5125;

/// The vertices and triangles of one primitive.
#[derive(Default)]
struct Primitive {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalized(v: [f32; 3]) -> Option<[f32; 3]> {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (len > 0.0 && len.is_finite()).then(|| [v[0] / len, v[1] / len, v[2] / len])
}

impl Primitive {
    /// Adds a face: its vertices with normals averaged over its triangles, so the face is
    /// smooth and its edges stay sharp.
    fn add_face(&mut self, positions: &[[f32; 3]], triangles: &[[u32; 3]]) {
        let mut normals = vec![[0.0f32; 3]; positions.len()];
        let mut kept = Vec::with_capacity(triangles.len());
        for t in triangles {
            let (Some(a), Some(b), Some(c)) = (
                positions.get(t[0] as usize),
                positions.get(t[1] as usize),
                positions.get(t[2] as usize),
            ) else {
                continue;
            };
            let n = cross(sub(*b, *a), sub(*c, *a));
            if normalized(n).is_none() {
                continue;
            }
            for &i in t {
                if let Some(slot) = normals.get_mut(i as usize) {
                    *slot = [slot[0] + n[0], slot[1] + n[1], slot[2] + n[2]];
                }
            }
            kept.push(*t);
        }
        if kept.is_empty() {
            return;
        }
        let base = self.positions.len() as u32;
        for (p, n) in positions.iter().zip(&normals) {
            self.positions
                .push([p[0] * SCALE, p[1] * SCALE, p[2] * SCALE]);
            self.normals.push(normalized(*n).unwrap_or([0.0, 1.0, 0.0]));
        }
        for t in kept {
            self.indices.extend(t.iter().map(|&i| base + i));
        }
    }
}

/// Appends `bytes` to the buffer, 4-byte aligned, and describes them as a buffer view;
/// returns the view's index.
fn push_view(bin: &mut Vec<u8>, views: &mut Vec<Value>, bytes: &[u8], target: u32) -> usize {
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    views.push(json!({
        "buffer": 0,
        "byteOffset": bin.len(),
        "byteLength": bytes.len(),
        "target": target,
    }));
    bin.extend_from_slice(bytes);
    views.len() - 1
}

fn vec3_bytes(values: &[[f32; 3]]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|v| v.iter().flat_map(|c| c.to_le_bytes()))
        .collect()
}

fn material_json(m: &GltfMaterial) -> Value {
    let clamp = |v: f32| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let color = m.base_color.map(clamp);
    let mut out = json!({
        "name": m.name,
        "pbrMetallicRoughness": {
            "baseColorFactor": color,
            "metallicFactor": clamp(m.metallic),
            "roughnessFactor": clamp(m.roughness),
        },
    });
    if let Some(object) = out.as_object_mut() {
        if color[3] < 0.999 {
            object.insert("alphaMode".into(), json!("BLEND"));
        }
        if clamp(m.transmission) > 0.0 {
            let ior = if m.ior.is_finite() && m.ior >= 1.0 {
                m.ior
            } else {
                1.5
            };
            object.insert(
                "extensions".into(),
                json!({
                    "KHR_materials_transmission": { "transmissionFactor": clamp(m.transmission) },
                    "KHR_materials_ior": { "ior": ior },
                }),
            );
        }
    }
    out
}

/// The whole file in memory.
pub fn glb_bytes(
    objects: &[GltfObject<'_>],
    materials: &[GltfMaterial],
    generator: &str,
) -> io::Result<Vec<u8>> {
    let mut materials = materials.to_vec();
    if materials.is_empty() {
        materials.push(GltfMaterial {
            name: "Default".into(),
            base_color: [0.45, 0.48, 0.53, 1.0],
            metallic: 0.0,
            roughness: 0.5,
            transmission: 0.0,
            ior: 1.5,
        });
    }
    let mut bin = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    for object in objects {
        // One primitive per material, in the order of the materials.
        let mut primitives: Vec<(usize, Primitive)> = Vec::new();
        for (k, face) in object.mesh.faces.iter().enumerate() {
            let material = object
                .face_materials
                .get(k)
                .copied()
                .filter(|&m| m < materials.len())
                .unwrap_or(0);
            let slot = match primitives.iter().position(|(m, _)| *m == material) {
                Some(i) => i,
                None => {
                    primitives.push((material, Primitive::default()));
                    primitives.len() - 1
                }
            };
            if let Some((_, p)) = primitives.get_mut(slot) {
                p.add_face(&face.positions, &face.triangles);
            }
        }
        primitives.retain(|(_, p)| !p.indices.is_empty());
        primitives.sort_by_key(|(m, _)| *m);
        if primitives.is_empty() {
            continue;
        }
        let mut prims_json = Vec::new();
        for (material, p) in &primitives {
            let mut lo = [f32::INFINITY; 3];
            let mut hi = [f32::NEG_INFINITY; 3];
            for v in &p.positions {
                for ((l, h), c) in lo.iter_mut().zip(hi.iter_mut()).zip(v) {
                    *l = l.min(*c);
                    *h = h.max(*c);
                }
            }
            let view = push_view(
                &mut bin,
                &mut views,
                &vec3_bytes(&p.positions),
                ARRAY_BUFFER,
            );
            accessors.push(json!({
                "bufferView": view, "componentType": FLOAT, "count": p.positions.len(),
                "type": "VEC3", "min": lo, "max": hi,
            }));
            let position = accessors.len() - 1;
            let view = push_view(&mut bin, &mut views, &vec3_bytes(&p.normals), ARRAY_BUFFER);
            accessors.push(json!({
                "bufferView": view, "componentType": FLOAT, "count": p.normals.len(),
                "type": "VEC3",
            }));
            let normal = accessors.len() - 1;
            let index_bytes: Vec<u8> = p.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
            let view = push_view(&mut bin, &mut views, &index_bytes, ELEMENT_ARRAY_BUFFER);
            accessors.push(json!({
                "bufferView": view, "componentType": UNSIGNED_INT, "count": p.indices.len(),
                "type": "SCALAR",
            }));
            let indices = accessors.len() - 1;
            prims_json.push(json!({
                "attributes": { "POSITION": position, "NORMAL": normal },
                "indices": indices,
                "material": material,
                "mode": 4,
            }));
        }
        meshes.push(json!({ "name": object.name, "primitives": prims_json }));
        nodes.push(json!({ "name": object.name, "mesh": meshes.len() - 1 }));
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let uses_glass = materials.iter().any(|m| m.transmission > 0.0);
    let mut root = json!({
        "asset": { "version": "2.0", "generator": generator },
        "scene": 0,
        "scenes": [{ "nodes": (0..nodes.len()).collect::<Vec<_>>() }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials.iter().map(material_json).collect::<Vec<_>>(),
        "accessors": accessors,
        "bufferViews": views,
    });
    if let Some(object) = root.as_object_mut() {
        // A buffer may not be empty: a file without triangles has none.
        if !bin.is_empty() {
            object.insert("buffers".into(), json!([{ "byteLength": bin.len() }]));
        }
    }
    if uses_glass && let Some(object) = root.as_object_mut() {
        object.insert(
            "extensionsUsed".into(),
            json!(["KHR_materials_ior", "KHR_materials_transmission"]),
        );
    }
    let mut text = serde_json::to_vec(&root).map_err(io::Error::other)?;
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    let bin_chunk = if bin.is_empty() { 0 } else { 8 + bin.len() };
    let total = 12 + 8 + text.len() + bin_chunk;
    let total = u32::try_from(total)
        .map_err(|_| io::Error::other("the model is too big for a glTF binary file (4 GB)"))?;
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(&GLB_MAGIC.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(&CHUNK_JSON.to_le_bytes());
    out.extend_from_slice(&text);
    if !bin.is_empty() {
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(&CHUNK_BIN.to_le_bytes());
        out.extend_from_slice(&bin);
    }
    Ok(out)
}

/// Writes the file to `w`.
pub fn write<W: Write>(
    mut w: W,
    objects: &[GltfObject<'_>],
    materials: &[GltfMaterial],
    generator: &str,
) -> io::Result<()> {
    w.write_all(&glb_bytes(objects, materials, generator)?)?;
    w.flush()
}

/// Writes the file to `path`.
pub fn save(
    path: &Path,
    objects: &[GltfObject<'_>],
    materials: &[GltfMaterial],
    generator: &str,
) -> io::Result<()> {
    let file = io::BufWriter::new(std::fs::File::create(path)?);
    write(file, objects, materials, generator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxyde_kernel::FaceMesh;

    fn square(z: f32) -> FaceMesh {
        FaceMesh {
            positions: vec![
                [0.0, 0.0, z],
                [10.0, 0.0, z],
                [10.0, 10.0, z],
                [0.0, 10.0, z],
            ],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        }
    }

    fn read_glb(bytes: &[u8]) -> (Value, Vec<u8>) {
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!(word(0), GLB_MAGIC);
        assert_eq!(word(4), 2);
        assert_eq!(word(8) as usize, bytes.len());
        let json_len = word(12) as usize;
        assert_eq!(word(16), CHUNK_JSON);
        let json: Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
        let bin_at = 20 + json_len;
        if bin_at == bytes.len() {
            return (json, Vec::new());
        }
        let bin_len = word(bin_at) as usize;
        assert_eq!(word(bin_at + 4), CHUNK_BIN);
        (json, bytes[bin_at + 8..bin_at + 8 + bin_len].to_vec())
    }

    #[test]
    fn objects_materials_and_buffers_are_consistent() {
        let mesh = Mesh {
            faces: vec![square(0.0), square(5.0), FaceMesh::default()],
            edges: Vec::new(),
        };
        let empty = Mesh::default();
        let materials = vec![
            GltfMaterial {
                name: "Steel".into(),
                base_color: [0.3, 0.3, 0.32, 1.0],
                metallic: 1.0,
                roughness: 0.3,
                transmission: 0.0,
                ior: 1.5,
            },
            GltfMaterial {
                name: "Glass".into(),
                base_color: [0.8, 0.9, 1.0, 1.0],
                metallic: 0.0,
                roughness: 0.0,
                transmission: 1.0,
                ior: 1.5,
            },
        ];
        let objects = [
            GltfObject {
                name: "Bracket",
                mesh: &mesh,
                face_materials: &[0, 1, 1],
            },
            // Nothing to draw: left out.
            GltfObject {
                name: "Nothing",
                mesh: &empty,
                face_materials: &[],
            },
        ];
        let bytes = glb_bytes(&objects, &materials, "oxyde test").unwrap();
        assert_eq!(bytes.len() % 4, 0);
        let (json, bin) = read_glb(&bytes);
        assert_eq!(json["asset"]["version"], "2.0");
        assert_eq!(json["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(json["nodes"][0]["name"], "Bracket");
        let prims = json["meshes"][0]["primitives"].as_array().unwrap();
        assert_eq!(prims.len(), 2, "one per material");
        assert_eq!(prims[1]["material"], 1);
        assert_eq!(
            json["buffers"][0]["byteLength"].as_u64().unwrap() as usize,
            bin.len()
        );
        // Positions in metres, with their bounds.
        let pos = &json["accessors"][prims[0]["attributes"]["POSITION"].as_u64().unwrap() as usize];
        assert_eq!(pos["count"], 4);
        assert!((pos["max"][0].as_f64().unwrap() - 0.01).abs() < 1e-6);
        let idx = &json["accessors"][prims[0]["indices"].as_u64().unwrap() as usize];
        assert_eq!(idx["count"], 6);
        // Every view lies inside the buffer, aligned.
        for view in json["bufferViews"].as_array().unwrap() {
            let offset = view["byteOffset"].as_u64().unwrap() as usize;
            let len = view["byteLength"].as_u64().unwrap() as usize;
            assert_eq!(offset % 4, 0);
            assert!(offset + len <= bin.len());
        }
        // Glass is declared with its extensions.
        assert_eq!(
            json["materials"][1]["extensions"]["KHR_materials_transmission"]["transmissionFactor"],
            1.0
        );
        assert!(
            json["extensionsUsed"]
                .as_array()
                .unwrap()
                .contains(&json!("KHR_materials_transmission"))
        );
        // The normal of the first square points along +Z.
        let normal_view = &json["bufferViews"][json["accessors"]
            [prims[0]["attributes"]["NORMAL"].as_u64().unwrap() as usize]["bufferView"]
            .as_u64()
            .unwrap() as usize];
        let at = normal_view["byteOffset"].as_u64().unwrap() as usize;
        let z = f32::from_le_bytes(bin[at + 8..at + 12].try_into().unwrap());
        assert!((z - 1.0).abs() < 1e-6);
    }

    #[test]
    fn materials_follow_the_finish() {
        let steel = GltfMaterial::from_finish([128, 128, 128], 0.0, Finish::BrushedMetal);
        assert_eq!(steel.metallic, 1.0);
        assert!((steel.base_color[0] - 0.2158).abs() < 1e-3);
        assert_eq!(steel.base_color[3], 1.0);
        let tinted = GltfMaterial::from_finish([255, 0, 0], 0.4, Finish::Satin);
        assert!((tinted.base_color[3] - 0.6).abs() < 1e-6);
        let glass = GltfMaterial::from_finish([180, 220, 235], 0.7, Finish::Glass);
        assert_eq!(glass.base_color[3], 1.0);
        assert_eq!(glass.transmission, 1.0);
        assert!(glass.base_color[0] > 0.6, "lightly tinted");
    }

    #[test]
    fn no_materials_and_no_objects_still_make_a_valid_file() {
        let bytes = glb_bytes(&[], &[], "oxyde").unwrap();
        let (json, bin) = read_glb(&bytes);
        assert!(bin.is_empty());
        assert!(json.get("buffers").is_none());
        assert_eq!(json["materials"].as_array().unwrap().len(), 1);
        assert!(json.get("extensionsUsed").is_none());
    }
}
