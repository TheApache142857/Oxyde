//! The tools that look without changing anything: listings, sections, measures, pictures,
//! the reference and the comparison with it, and export.

use std::path::PathBuf;

use oxyde_geom::{Plane, Point3, Transform, Vec3};
use oxyde_kernel::{Kernel, Mesh};
use oxyde_render::Pick;
use serde_json::{Value, json};

use crate::args::Set;
use crate::args::{self, Result};
use crate::geometry::{self, Solids};
use crate::inspect;
use crate::picture::{self, Layer};
use crate::select::{Kind, Scope};
use crate::session::Session;
use crate::tools::{Image, ToolOutput};

/// The part's or the reference's solids, with their mesh, for selectors.
fn with_target<T>(
    session: &Session,
    args: &Value,
    f: impl FnOnce(&Scope<'_>) -> Result<T>,
) -> Result<T> {
    match args::opt_str(args, "target")?.unwrap_or("part") {
        "part" => {
            let doc = session.doc()?;
            let body = doc.part.body().ok_or("the part has no solid yet")?;
            let solids = Solids::of_body(body);
            let mesh = solids.mesh(&session.kernel)?;
            f(&Scope {
                solids: &solids,
                mesh: &mesh,
                part: Some(&doc.part),
            })
        }
        "reference" => {
            let reference = session.reference()?;
            let solids = reference.solids();
            let mesh = solids.mesh(&session.kernel)?;
            f(&Scope {
                solids: &solids,
                mesh: &mesh,
                part: None,
            })
        }
        other => Err(format!("unknown target \"{other}\" (part or reference)")),
    }
}

fn listing(session: &Session, args: &Value, kind: Kind) -> Result<ToolOutput> {
    let limit = args::opt_f64(args, "limit")?.unwrap_or(200.0).max(1.0) as usize;
    with_target(session, args, |scope| {
        let picked = match args::opt(args, "select") {
            Some(selector) => scope.select_any(kind, selector)?,
            None => {
                let n = match kind {
                    Kind::Face => scope.solids.topology.faces.len(),
                    Kind::Edge => scope.solids.topology.edges.len(),
                };
                (0..n as u32).collect()
            }
        };
        let rows: Vec<Value> = picked
            .iter()
            .take(limit)
            .map(|&i| match kind {
                Kind::Face => inspect::face_json(scope, i),
                Kind::Edge => inspect::edge_json(scope, i),
            })
            .collect();
        let mut out = json!({ "count": picked.len(), "items": rows });
        if picked.len() > limit {
            out.set(
                "truncated",
                json!(format!(
                    "{} more; narrow the selector or raise \"limit\"",
                    picked.len() - limit
                )),
            );
        }
        Ok(ToolOutput::json(&out))
    })
}

pub(crate) fn faces(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    listing(session, args, Kind::Face)
}

pub(crate) fn edges(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    listing(session, args, Kind::Edge)
}

/// A section plane: a standard plane moved along its normal, or any plane.
fn section_plane(value: &Value, offset: f64) -> Result<Plane> {
    let base = match value.as_str() {
        Some("front") => Plane::FRONT,
        Some("top") => Plane::TOP,
        Some("right") => Plane::RIGHT,
        Some(other) => return Err(format!("unknown plane \"{other}\"")),
        None => {
            let origin = args::point3(args::get(value, "origin")?, "\"origin\"")?;
            let normal = args::vec3(args::get(value, "normal")?, "\"normal\"")?;
            let frame = Plane::from_point_normal(origin, normal).ok_or("bad normal")?;
            Plane { origin, ..frame }
        }
    };
    Ok(base.offset(offset))
}

pub(crate) fn section(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let plane = section_plane(
        args::get(args, "plane")?,
        args::opt_f64(args, "offset")?.unwrap_or(0.0),
    )?;
    let shapes = match args::opt_str(args, "target")?.unwrap_or("part") {
        "part" => session
            .doc()?
            .part
            .body()
            .map(|b| b.shapes())
            .ok_or("the part has no solid yet")?,
        "reference" => session.reference()?.solids().shapes(),
        other => return Err(format!("unknown target \"{other}\"")),
    };
    let section = inspect::section(&mut session.kernel, &shapes, &plane)?;
    let mut out = inspect::section_json(&section, args::bool(args, "detail", false)?);
    out.set(
        "plane",
        json!({ "origin": args::round3(plane.origin), "normal": args::round_vec(plane.normal),
        "x_dir": args::round_vec(plane.x_dir), "y_dir": args::round_vec(plane.y_dir()) }),
    );
    let mut output = ToolOutput::json(&out);
    if args::bool(args, "image", false)? {
        let window = match args::opt(args, "region") {
            Some(r) => {
                let a = args::point2(args::get(r, "min")?, "region min")?;
                let b = args::point2(args::get(r, "max")?, "region max")?;
                Some([[a.x, a.y], [b.x, b.y]])
            }
            None => None,
        };
        let png = picture::plot_section(&section, 900, 900, window)?;
        output
            .images
            .push(write_image(session, args, "section", png)?);
    }
    Ok(output)
}

/// Material along lines through points: a depth gauge.
pub(crate) fn probe(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let axis = match args::opt_str(args, "axis")?.unwrap_or("y") {
        "x" => 0,
        "y" => 1,
        "z" => 2,
        other => return Err(format!("unknown axis \"{other}\"")),
    };
    let points: Vec<oxyde_geom::Point3> = args::list(args::get(args, "points")?)
        .into_iter()
        .map(|p| args::point3(p, "a point"))
        .collect::<Result<_>>()?;
    with_target(session, args, |scope| {
        let rows: Vec<Value> = points
            .iter()
            .map(|p| {
                let stretches = inspect::probe(scope.mesh, *p, axis);
                json!({
                    "at": args::round3(*p),
                    "material": stretches.iter().map(|(s, e)| [args::round(*s), args::round(*e)]).collect::<Vec<_>>(),
                    "thickness": args::round(stretches.iter().map(|(s, e)| e - s).sum::<f64>()),
                })
            })
            .collect();
        Ok(ToolOutput::json(
            &json!({ "axis": (["x", "y", "z"]).get(axis), "probes": rows }),
        ))
    })
}

pub(crate) fn measure(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let pick = |scope: &Scope<'_>, side: &Value| -> Result<(Kind, u32)> {
        let (kind, selector) = match (args::opt(side, "face"), args::opt(side, "edge")) {
            (Some(s), None) => (Kind::Face, s),
            (None, Some(s)) => (Kind::Edge, s),
            _ => return Err("each side is {\"face\": selector} or {\"edge\": selector}".to_owned()),
        };
        let picked = scope.select(kind, selector)?;
        Ok((kind, picked.first().copied().ok_or("nothing picked")?))
    };
    let kernel = &session.kernel;
    with_target(session, args, |scope| {
        let a = pick(scope, args::get(args, "a")?)?;
        let b = pick(scope, args::get(args, "b")?)?;
        let mut out = inspect::distance(kernel, scope.solids, a, b)?;
        for (key, (kind, i)) in [("a", a), ("b", b)] {
            out.set(
                key,
                match kind {
                    Kind::Face => inspect::face_json(scope, i),
                    Kind::Edge => inspect::edge_json(scope, i),
                },
            );
        }
        Ok(ToolOutput::json(&out))
    })
}

fn size(args: &Value) -> Result<(usize, usize)> {
    match args::opt(args, "size") {
        None => Ok((640, 480)),
        Some(v) => {
            let p = args::point2(v, "\"size\"")?;
            let clamp = |x: f64| x.round().clamp(64.0, 2048.0) as usize;
            Ok((clamp(p.x), clamp(p.y)))
        }
    }
}

/// The `region` argument of a picture: `{"min": [x, y, z], "max": [x, y, z]}`.
fn region(args: &Value) -> Result<Option<picture::Region>> {
    let Some(r) = args::opt(args, "region") else {
        return Ok(None);
    };
    let a = args::point3(args::get(r, "min")?, "region min")?;
    let b = args::point3(args::get(r, "max")?, "region max")?;
    Ok(Some([[a.x, a.y, a.z], [b.x, b.y, b.z]]))
}

fn write_image(session: &mut Session, args: &Value, stem: &str, png: Vec<u8>) -> Result<Image> {
    let path = match args::opt_str(args, "out")? {
        Some(p) => PathBuf::from(p),
        None => session.next_picture(stem),
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&path, &png).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Image {
        png,
        path: Some(path),
    })
}

const REFERENCE_COLOR: [u8; 3] = [230, 60, 50];

fn part_mesh(session: &Session) -> Result<Mesh> {
    let doc = session.doc()?;
    let body = doc.part.body().ok_or("the part has no solid yet")?;
    Solids::of_body(body).mesh(&session.kernel)
}

fn reference_mesh(session: &Session) -> Result<Mesh> {
    session.reference()?.solids().mesh(&session.kernel)
}

pub(crate) fn render(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let views = picture::views(args::opt(args, "views"))?;
    let (w, h) = size(args)?;
    let mut highlight = Vec::new();
    let layers = match args::opt_str(args, "target")?.unwrap_or("part") {
        "part" => {
            if let Some(hl) = args::opt(args, "highlight") {
                with_target(session, &json!({}), |scope| {
                    if let Some(s) = args::opt(hl, "faces") {
                        highlight.extend(scope.select(Kind::Face, s)?.into_iter().map(Pick::Face));
                    }
                    if let Some(s) = args::opt(hl, "edges") {
                        highlight.extend(scope.select(Kind::Edge, s)?.into_iter().map(Pick::Edge));
                    }
                    Ok(())
                })?;
            }
            vec![Layer {
                mesh: part_mesh(session)?,
                color: None,
                opacity: 1.0,
            }]
        }
        "reference" => vec![Layer {
            mesh: reference_mesh(session)?,
            color: None,
            opacity: 1.0,
        }],
        "both" => vec![
            Layer {
                mesh: part_mesh(session)?,
                color: None,
                opacity: 1.0,
            },
            Layer {
                mesh: reference_mesh(session)?,
                color: Some(REFERENCE_COLOR),
                opacity: 0.35,
            },
        ],
        "assembly" => {
            let doc = session.assembly.as_ref().ok_or("no assembly is open")?;
            crate::assembly::placed_meshes(doc)
                .into_iter()
                .map(|(mesh, look, _)| Layer {
                    mesh,
                    color: look.map(|(c, _)| c),
                    opacity: look.map_or(1.0, |(_, t)| (1.0 - t as f32).clamp(0.05, 1.0)),
                })
                .collect()
        }
        other => return Err(format!("unknown target \"{other}\"")),
    };
    let png = picture::render(&layers, &views, w, h, &highlight, region(args)?)?;
    let image = write_image(session, args, "render", png)?;
    let text = format!(
        "rendered {} view(s) to {}",
        views.len(),
        image
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
    Ok(ToolOutput {
        text,
        images: vec![image],
    })
}

pub(crate) fn open_reference(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let path = args::path(args, "path")?;
    let rotation = crate::assembly::rotation(args::opt(args, "rotation"))?;
    let mut transform = oxyde_assembly::Placement {
        rotation,
        translation: Vec3::ZERO,
    }
    .transform();
    if let Some(t) = args::opt(args, "translate") {
        transform = transform.then(&Transform::translation(args::vec3(t, "\"translate\"")?));
    }
    session.open_reference(&path, transform)?;
    // Centring needs the box, so it comes after a first placement.
    let center = match args::opt(args, "center") {
        Some(Value::Bool(true)) => Some("xyz".to_owned()),
        Some(Value::String(axes)) => Some(axes.to_ascii_lowercase()),
        _ => None,
    };
    if let Some(axes) = center {
        let shapes = session.reference()?.solids().shapes();
        let (lo, hi) = geometry::bounds(&session.kernel, &shapes).ok_or("no box")?;
        let mid = Point3::new(
            (lo.x + hi.x) / 2.0,
            (lo.y + hi.y) / 2.0,
            (lo.z + hi.z) / 2.0,
        );
        let shift = Vec3::new(
            if axes.contains('x') { -mid.x } else { 0.0 },
            if axes.contains('y') { -mid.y } else { 0.0 },
            if axes.contains('z') { -mid.z } else { 0.0 },
        );
        transform = transform.then(&Transform::translation(shift));
        session.open_reference(&path, transform)?;
    }
    let reference = session.reference()?;
    let shapes = reference.solids().shapes();
    let volume: f64 = shapes
        .iter()
        .filter_map(|s| session.kernel.volume(*s).ok())
        .sum();
    let mut out = json!({
        "path": path.display().to_string(),
        "solids": shapes.len(),
        "volume": args::round(volume),
        "topology": inspect::counts(&reference.topology),
        "moved_by": transform.rows.iter().map(|r| r.iter().map(|v| args::round(*v)).collect::<Vec<_>>()).collect::<Vec<_>>(),
    });
    if let Some((lo, hi)) = geometry::bounds(&session.kernel, &shapes) {
        out.set("bounds", json!({ "min": args::round3(lo), "max": args::round3(hi), "size": args::round_vec(hi - lo) }));
    }
    Ok(ToolOutput::json(&out))
}

pub(crate) fn compare(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let image = args::bool(args, "image", false)?;
    let a = session
        .doc()?
        .part
        .body()
        .map(|b| b.shapes())
        .ok_or("the part has no solid yet")?;
    let b = session.reference()?.solids().shapes();
    let comparison = match args::opt_str(args, "method")?.unwrap_or("auto") {
        "auto" | "booleans" => inspect::compare(&mut session.kernel, &a, &b, image)?,
        "sampling" => inspect::compare_sampled(&session.kernel, &a, &b),
        other => return Err(format!("unknown method \"{other}\" (auto or sampling)")),
    };
    let mut out = comparison.json();
    let bounds = |shapes: &[oxyde_kernel::ShapeHandle]| {
        geometry::bounds(&session.kernel, shapes)
            .map(|(lo, hi)| json!({ "min": args::round3(lo), "max": args::round3(hi) }))
    };
    out.set("bounds_model", json!(bounds(&a)));
    out.set("bounds_reference", json!(bounds(&b)));
    let mut output = ToolOutput::json(&out);
    if image {
        let deflection = 0.05;
        let mesh_of = |kernel: &oxyde_occt::OcctKernel, shapes: &[oxyde_kernel::ShapeHandle]| {
            let mut mesh = Mesh::default();
            for s in shapes {
                if let Ok(m) = kernel.tessellate(*s, deflection) {
                    mesh.faces.extend(m.faces);
                    mesh.edges.extend(m.edges);
                }
            }
            mesh
        };
        let layers = vec![
            Layer {
                mesh: mesh_of(&session.kernel, &a),
                color: None,
                opacity: 0.25,
            },
            Layer {
                mesh: mesh_of(&session.kernel, &comparison.extra),
                color: Some([235, 50, 40]),
                opacity: 1.0,
            },
            Layer {
                mesh: mesh_of(&session.kernel, &comparison.missing),
                color: Some([40, 90, 235]),
                opacity: 1.0,
            },
        ];
        let views = picture::views(args::opt(args, "views"))?;
        let (w, h) = size(args)?;
        let rendered = picture::render(&layers, &views, w, h, &[], region(args)?);
        comparison.release(&mut session.kernel);
        let png = rendered?;
        output
            .images
            .push(write_image(session, args, "compare", png)?);
    } else {
        comparison.release(&mut session.kernel);
    }
    Ok(output)
}

pub(crate) fn export(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let out = args::path(args, "out")?;
    match args::opt_str(args, "target")?.unwrap_or("part") {
        "part" => {
            let doc = session.doc()?;
            if doc.part.body().is_none() {
                return Err("the part has no solid".to_owned());
            }
            // Export through the command line's code, which a part file drives: write the
            // part's definition to a scratch file, so what is exported is what is saved.
            let scratch = std::env::temp_dir()
                .join(format!("oxyde-mcp-export-{}.oxyprt", std::process::id()));
            let file = oxyde_format::PartFile {
                document_id: doc.document_id.clone(),
                definition: doc.part.definition(),
                properties: doc.properties.clone(),
            };
            oxyde_format::save_part(&scratch, &file).map_err(|e| e.to_string())?;
            let result = oxyde_cli::export(&scratch, &out);
            let _ = std::fs::remove_file(&scratch);
            let (format, _) = result.map_err(|e| e.to_string())?;
            Ok(ToolOutput::text(format!(
                "wrote {} ({format:?})",
                out.display()
            )))
        }
        "assembly" => {
            let doc = session.assembly.as_ref().ok_or("no assembly is open")?;
            let placed = crate::assembly::placed_solids(&mut session.kernel, doc);
            let shapes: Vec<_> = placed.iter().flat_map(|(_, s)| s.iter().copied()).collect();
            let result = session.kernel.export_step(&shapes, &[], &[], &out);
            for s in shapes {
                session.kernel.release(s);
            }
            result.map_err(|e| e.to_string())?;
            Ok(ToolOutput::text(format!("wrote {} (STEP)", out.display())))
        }
        other => Err(format!("unknown target \"{other}\"")),
    }
}
