//! Opens one STEP file with one kernel, as the application does, and prints what it read as
//! JSON lines (each starting `STEPCMP `), one per stage, so that a run stopped part way
//! still tells how far it got:
//!
//! - `assembly`: the product structure (`read_step_assembly`), then every part's own file
//!   read (`import_step`), as opening a STEP file does;
//! - `import`: the whole file read as one shape, with its volume, centre of mass, bounds,
//!   area, counts, kinds of surface, colours and material;
//! - `mesh`: that shape meshed at a thousandth of its size, as the viewport meshes a body.
//!
//! Run by hand, one kernel per process (a script compares the two):
//! `OXYDE_STEP_FILE=<path> OXYDE_STEP_KERNEL=occt|native cargo test -p oxyde-tests
//! --test step_corpus -- --ignored --nocapture`.

use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use oxyde_kernel::{Kernel, StepColor, StepNodeContent, SurfaceKind};
use oxyde_native_kernel::NativeKernel;
use oxyde_occt::OcctKernel;
use serde_json::{Value, json};

fn emit(stage: &str, mut value: Value) {
    value["stage"] = json!(stage);
    println!("STEPCMP {value}");
}

/// The message of a panic caught while a kernel read or measured.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "panic".to_owned())
}

/// A short digest of something both kernels should read alike, to compare without
/// printing all of it.
fn digest(value: &impl Hash) -> String {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn colour_key(colours: &[Option<StepColor>]) -> Vec<Option<([u8; 3], u64)>> {
    colours
        .iter()
        .map(|c| c.map(|c| (c.rgb, (c.transparency * 1000.0).round() as u64)))
        .collect()
}

fn surface_name(kind: SurfaceKind) -> &'static str {
    match kind {
        SurfaceKind::Plane => "plane",
        SurfaceKind::Cylinder => "cylinder",
        SurfaceKind::Cone => "cone",
        SurfaceKind::Sphere => "sphere",
        SurfaceKind::Torus => "torus",
        SurfaceKind::BSpline => "bspline",
        SurfaceKind::Other => "other",
    }
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn open<K: Kernel>(kernel: &mut K, bytes: &[u8]) {
    // The product structure, then each part read on its own: what opening the file does.
    let start = Instant::now();
    let tree = catch_unwind(AssertUnwindSafe(|| kernel.read_step_assembly(bytes)));
    let read_ms = ms(start);
    match tree {
        Ok(Ok(tree)) => {
            let mut parts = Vec::new();
            let mut assemblies = 0;
            let mut instances = 0;
            let mut structure = Vec::new();
            let parts_start = Instant::now();
            for node in &tree.nodes {
                match &node.content {
                    StepNodeContent::Assembly { instances: of } => {
                        assemblies += 1;
                        instances += of.len();
                        structure.push((node.name.clone(), of.len()));
                    }
                    StepNodeContent::Part { step, .. } => {
                        structure.push((node.name.clone(), usize::MAX));
                        let start = Instant::now();
                        let read = catch_unwind(AssertUnwindSafe(|| kernel.import_step(step)));
                        let part_ms = ms(start);
                        parts.push(match read {
                            Ok(Ok(read)) => {
                                let volume = kernel.volume(read.shape).ok();
                                let faces =
                                    kernel.topology_counts(read.shape).ok().map(|c| c.faces);
                                kernel.release(read.shape);
                                json!({"name": node.name, "ok": true, "ms": part_ms,
                                       "volume": volume, "faces": faces})
                            }
                            Ok(Err(e)) => json!({"name": node.name, "ok": false, "ms": part_ms,
                                                 "error": e.to_string()}),
                            Err(p) => json!({"name": node.name, "ok": false, "ms": part_ms,
                                             "error": format!("panic: {}", panic_message(p))}),
                        });
                    }
                }
            }
            emit(
                "assembly",
                json!({
                    "ok": true, "read_ms": read_ms, "parts_ms": ms(parts_start),
                    "nodes": tree.nodes.len(), "assemblies": assemblies,
                    "instances": instances, "structure": digest(&structure),
                    "root": tree.nodes.get(tree.root).map(|n| n.name.clone()),
                    "parts": parts,
                }),
            );
        }
        Ok(Err(e)) => emit(
            "assembly",
            json!({"ok": false, "read_ms": read_ms, "error": e.to_string()}),
        ),
        Err(p) => emit(
            "assembly",
            json!({"ok": false, "read_ms": read_ms, "error": format!("panic: {}", panic_message(p))}),
        ),
    }

    // The whole file as one shape, measured.
    let start = Instant::now();
    let read = catch_unwind(AssertUnwindSafe(|| kernel.import_step(bytes)));
    let import_ms = ms(start);
    let read = match read {
        Ok(Ok(read)) => read,
        Ok(Err(e)) => {
            emit(
                "import",
                json!({"ok": false, "ms": import_ms, "error": e.to_string()}),
            );
            return;
        }
        Err(p) => {
            emit(
                "import",
                json!({"ok": false, "ms": import_ms, "error": format!("panic: {}", panic_message(p))}),
            );
            return;
        }
    };
    let shape = read.shape;
    let measured = catch_unwind(AssertUnwindSafe(|| {
        let counts = kernel.topology_counts(shape).map_err(|e| e.to_string())?;
        let volume = kernel.volume(shape).map_err(|e| e.to_string())?;
        let centre = kernel.centre_of_mass(shape).map_err(|e| e.to_string())?;
        let (lo, hi) = kernel.bounds(shape).map_err(|e| e.to_string())?;
        let topology = kernel.topology(shape).map_err(|e| e.to_string())?;
        let mut surfaces: BTreeMap<&str, usize> = BTreeMap::new();
        let mut area = 0.0;
        for face in &topology.faces {
            *surfaces.entry(surface_name(face.surface)).or_default() += 1;
            area += face.area;
        }
        Ok::<_, String>(json!({
            "solids": counts.solids, "shells": counts.shells, "faces": counts.faces,
            "edges": counts.edges, "vertices": counts.vertices,
            "volume": volume, "centre": [centre.x, centre.y, centre.z],
            "lo": [lo.x, lo.y, lo.z], "hi": [hi.x, hi.y, hi.z], "area": area,
            "surfaces": surfaces,
        }))
    }));
    let mut value = match measured {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => json!({"measure_error": e}),
        Err(p) => json!({"measure_error": format!("panic: {}", panic_message(p))}),
    };
    value["ok"] = json!(true);
    value["ms"] = json!(import_ms);
    value["solid_colours"] = json!(read.colors.solids.iter().flatten().count());
    value["face_colours"] = json!(read.colors.faces.iter().flatten().count());
    value["colours"] = json!(digest(&(
        colour_key(&read.colors.solids),
        colour_key(&read.colors.faces)
    )));
    value["material"] = json!(read.material.as_ref().map(|m| m.name.clone()));
    emit("import", value);

    // Meshed at a thousandth of its size, as the viewport meshes a body.
    let mesh = catch_unwind(AssertUnwindSafe(|| {
        let (lo, hi) = kernel.bounds(shape).map_err(|e| e.to_string())?;
        let deflection = (lo.distance(hi) * 1e-3).max(1e-3);
        let start = Instant::now();
        let mesh = kernel
            .tessellate(shape, deflection)
            .map_err(|e| e.to_string())?;
        let mesh_ms = ms(start);
        let triangles: usize = mesh.faces.iter().map(|f| f.triangles.len()).sum();
        let empty = mesh.faces.iter().filter(|f| f.triangles.is_empty()).count();
        // Where the triangles go, by kind of surface, and the face that takes the most.
        let topology = kernel.topology(shape).map_err(|e| e.to_string())?;
        let mut by_surface: BTreeMap<&str, usize> = BTreeMap::new();
        let mut most = (0, "", 0.0);
        for (face, info) in mesh.faces.iter().zip(&topology.faces) {
            let kind = surface_name(info.surface);
            *by_surface.entry(kind).or_default() += face.triangles.len();
            if face.triangles.len() > most.0 {
                most = (face.triangles.len(), kind, info.area);
            }
        }
        // Whether the faces' meshes close up: welded where they share a position (the
        // points of an edge's polyline, the same in both faces), each side of a triangle
        // should meet exactly one other going the other way; and the volume they hold.
        let mut ids: HashMap<[u32; 3], u32> = HashMap::new();
        let mut sides: HashMap<(u32, u32), i64> = HashMap::new();
        let mut volume = 0.0;
        for face in &mesh.faces {
            let index: Vec<u32> = face
                .positions
                .iter()
                .map(|p| {
                    let key = p.map(f32::to_bits);
                    let next = ids.len() as u32;
                    *ids.entry(key).or_insert(next)
                })
                .collect();
            for t in &face.triangles {
                let [a, b, c] = t.map(|i| face.positions[i as usize].map(f64::from));
                volume += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                    + a[2] * (b[0] * c[1] - b[1] * c[0]))
                    / 6.0;
                let t = t.map(|i| index[i as usize]);
                for k in 0..3 {
                    let (p, q) = (t[k], t[(k + 1) % 3]);
                    if p != q {
                        *sides.entry((p.min(q), p.max(q))).or_default() +=
                            if p < q { 1 } else { -1 };
                    }
                }
            }
        }
        let open = sides.values().filter(|&&n| n != 0).count();
        // Planar faces whose triangles face against the face's own normal (inside out).
        let flipped = mesh
            .faces
            .iter()
            .zip(&topology.faces)
            .filter(|(face, info)| {
                let Some(n) = info.normal else {
                    return false;
                };
                let along: f64 = face
                    .triangles
                    .iter()
                    .map(|t| {
                        let [a, b, c] = t.map(|i| {
                            let p = face.positions[i as usize];
                            oxyde_geom::Vec3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2]))
                        });
                        (b - a).cross(c - a).dot(n)
                    })
                    .sum();
                along < 0.0
            })
            .count();
        Ok::<_, String>(json!({
            "ok": true, "ms": mesh_ms, "deflection": deflection, "triangles": triangles,
            "empty_faces": empty, "by_surface": by_surface,
            "most": {"triangles": most.0, "surface": most.1, "area": most.2},
            "open_sides": open, "volume": volume, "flipped_planes": flipped,
        }))
    }));
    emit(
        "mesh",
        match mesh {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => json!({"ok": false, "error": e}),
            Err(p) => json!({"ok": false, "error": format!("panic: {}", panic_message(p))}),
        },
    );
    kernel.release(shape);
}

#[test]
#[ignore = "reads the STEP file OXYDE_STEP_FILE names, run by hand"]
fn step_file_with_one_kernel() {
    let path = std::env::var("OXYDE_STEP_FILE").expect("OXYDE_STEP_FILE");
    let which = std::env::var("OXYDE_STEP_KERNEL").expect("OXYDE_STEP_KERNEL: occt or native");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    emit(
        "start",
        json!({"file": path, "kernel": which, "bytes": bytes.len()}),
    );
    match which.as_str() {
        "occt" => open(&mut OcctKernel::new(), &bytes),
        "native" => open(&mut NativeKernel::new(), &bytes),
        other => panic!("OXYDE_STEP_KERNEL {other}: occt or native"),
    }
    emit("done", json!({}));
}
