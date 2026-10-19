//! The modelling tools: sketches and features added to the part, and edits of what is
//! there. Each one is one undoable change, rebuilt at once; the result says how the new
//! feature built and what the body looks like now.

use oxyde_geom::Plane;
use oxyde_model::{
    AxisRef, BodyId, ChamferShape, CircularPattern, Combine, CombineOp, EdgeBlend, Extrude,
    ExtrudeExtent, ExtrudeOp, Feature, FeatureId, FeatureKind, Hole, HoleKind, LinearPattern, Loft,
    Mirror, PatternDirection, PlaneRef, RefAxis, RefPlane, ReferenceGeometry, Revolve, RevolveAxis,
    Shell, Sketch, SketchPlane, StandardAxis, StandardPlane, Sweep,
};
use oxyde_sketch::ConstrainedSketch;
use serde_json::{Value, json};

use crate::args::Set;
use crate::args::{self, Result};
use crate::geometry::Solids;
use crate::select::{Kind, Scope};
use crate::session::{PartDoc, Session, body_summary, feature_id, status_report};
use crate::sketch::Builder;
use crate::tools::ToolOutput;

/// Adds `kind` as one change, names it, rebuilds, and reports.
fn add(session: &mut Session, kind: FeatureKind, args: &Value) -> Result<ToolOutput> {
    let name = args::opt_str(args, "name")?.map(str::to_owned);
    let id = session.change(|doc| add_named(doc, kind, name.as_deref()))?;
    Ok(ToolOutput::json(&report(session, id)))
}

fn add_named(doc: &mut PartDoc, kind: FeatureKind, name: Option<&str>) -> Result<FeatureId> {
    let id = doc.part.add_feature(kind);
    if let Some(name) = name {
        doc.part.rename(id, name).map_err(|e| e.to_string())?;
    }
    Ok(id)
}

/// How feature `id` built, and the body after it.
fn report(session: &Session, id: FeatureId) -> Value {
    let Some(doc) = session.part.as_ref() else {
        return Value::Null;
    };
    let mut out = status_report(&doc.part, &[id]);
    out.set("body", body_summary(&session.kernel, &doc.part));
    out
}

/// The faces and edges of the part's body as it is at the rollback bar, for selectors.
fn with_scope<T>(session: &Session, f: impl FnOnce(&Scope<'_>) -> Result<T>) -> Result<T> {
    let doc = session.doc()?;
    let body = doc
        .part
        .body()
        .ok_or("the part has no solid yet to pick faces or edges on")?;
    let solids = Solids::of_body(body);
    let mesh = solids.mesh(&session.kernel)?;
    let scope = Scope {
        solids: &solids,
        mesh: &mesh,
        part: Some(&doc.part),
    };
    f(&scope)
}

// ---- Planes, axes and bodies -------------------------------------------------------------------

/// A plane argument; `{"base", "offset"}` adds a reference plane first.
pub(crate) fn plane_ref(session: &mut Session, value: &Value) -> Result<PlaneRef> {
    if let Some(name) = value.as_str() {
        return Ok(match name {
            "front" | "Front" | "xy" => PlaneRef::Standard(StandardPlane::Front),
            "top" | "Top" | "xz" => PlaneRef::Standard(StandardPlane::Top),
            "right" | "Right" | "yz" => PlaneRef::Standard(StandardPlane::Right),
            other => PlaneRef::Reference(feature_id(&session.doc()?.part, &json!(other))?),
        });
    }
    if let Some(id) = value.as_u64() {
        return Ok(PlaneRef::Reference(feature_id(
            &session.doc()?.part,
            &json!(id),
        )?));
    }
    if let Some(face) = args::opt(value, "face") {
        let names = with_scope(session, |scope| {
            let faces = scope.select(Kind::Face, face)?;
            if faces.len() != 1 {
                return Err(format!(
                    "the plane must be one flat face; {} match",
                    faces.len()
                ));
            }
            scope.face_names(&faces)
        })?;
        return names
            .into_iter()
            .next()
            .map(PlaneRef::Face)
            .ok_or_else(|| "no face".to_owned());
    }
    if let Some(plane) = args::opt(value, "plane") {
        return plane_ref(session, plane);
    }
    if let Some(base) = args::opt(value, "base") {
        let base = plane_ref(session, base)?;
        let offset = args::f64(value, "offset")?;
        let flip = args::bool(value, "flip", false)? || offset < 0.0;
        let id = session.change(|doc| {
            add_named(
                doc,
                FeatureKind::Plane(RefPlane {
                    base,
                    offset: offset.abs(),
                    flip,
                }),
                None,
            )
        })?;
        return Ok(PlaneRef::Reference(id));
    }
    Err(format!("not a plane: {value}"))
}

/// An axis argument.
pub(crate) fn axis_ref(session: &Session, value: &Value) -> Result<AxisRef> {
    if let Some(name) = value.as_str() {
        return Ok(match name {
            "x" | "X" => AxisRef::Standard(StandardAxis::X),
            "y" | "Y" => AxisRef::Standard(StandardAxis::Y),
            "z" | "Z" => AxisRef::Standard(StandardAxis::Z),
            other => AxisRef::Reference(feature_id(&session.doc()?.part, &json!(other))?),
        });
    }
    if let Some(edge) = args::opt(value, "edge") {
        let names = with_scope(session, |scope| {
            let edges = scope.select(Kind::Edge, edge)?;
            scope.edge_names(edges.get(..1).unwrap_or(&[]))
        })?;
        return names
            .into_iter()
            .next()
            .map(AxisRef::Edge)
            .ok_or_else(|| "no edge".to_owned());
    }
    if let Some(face) = args::opt(value, "face") {
        let names = with_scope(session, |scope| {
            let faces = scope.select(Kind::Face, face)?;
            scope.face_names(faces.get(..1).unwrap_or(&[]))
        })?;
        return names
            .into_iter()
            .next()
            .map(AxisRef::Face)
            .ok_or_else(|| "no face".to_owned());
    }
    if let Some(axis) = args::opt(value, "axis") {
        return axis_ref(session, axis);
    }
    Err(format!("not an axis: {value}"))
}

/// The plane a sketch on `plane` lies in, in the model.
fn plane_geometry(session: &Session, plane: &PlaneRef) -> Result<Plane> {
    let doc = session.doc()?;
    match plane {
        PlaneRef::Standard(s) => Ok(s.plane()),
        PlaneRef::Reference(id) => match doc.part.reference(*id) {
            Some(ReferenceGeometry::Plane(p)) => Ok(p),
            _ => Err("that reference plane is not built".to_owned()),
        },
        PlaneRef::Face(name) => {
            let body = doc.part.body().ok_or("no body")?;
            let index = body
                .naming
                .find_face(name)
                .ok_or("the face is not in the body")?;
            let face = body
                .topology
                .faces
                .get(index as usize)
                .ok_or("the face is not in the body")?;
            let normal = face.normal.ok_or("the face is not flat")?;
            Plane::from_point_normal(face.centroid, normal).ok_or_else(|| "bad face".to_owned())
        }
    }
}

fn operation(args: &Value, default: ExtrudeOp) -> Result<ExtrudeOp> {
    Ok(match args::opt_str(args, "operation")? {
        None => default,
        Some("boss" | "join" | "add") => ExtrudeOp::Boss,
        Some("cut" | "remove") => ExtrudeOp::Cut,
        Some("intersect") => ExtrudeOp::Intersect,
        Some("new_body" | "new") => ExtrudeOp::NewBody,
        Some(other) => return Err(format!("unknown operation \"{other}\"")),
    })
}

/// Bodies by `"Body 2"` (as part_info lists them), by the feature that made them, or by
/// `{"feature", "index"}`.
fn body_ids(session: &Session, value: Option<&Value>) -> Result<Vec<BodyId>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let doc = session.doc()?;
    let solids = doc.part.body().map(|b| b.solids.as_slice()).unwrap_or(&[]);
    args::list(value)
        .into_iter()
        .map(|v| {
            if let Some(text) = v.as_str()
                && let Some(n) = text
                    .strip_prefix("Body ")
                    .and_then(|n| n.parse::<usize>().ok())
            {
                return solids
                    .get(n.wrapping_sub(1))
                    .map(|s| s.id)
                    .ok_or_else(|| format!("there is no {text}"));
            }
            if v.is_object() {
                let feature = feature_id(&doc.part, args::get(v, "feature")?)?;
                let index = args::opt_f64(v, "index")?.unwrap_or(0.0) as u32;
                return Ok(BodyId::new(feature, index));
            }
            Ok(BodyId::new(feature_id(&doc.part, v)?, 0))
        })
        .collect()
}

// ---- Sketches ------------------------------------------------------------------------------------

/// The `sketch` tool.
pub(crate) fn sketch(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let id = make_sketch(session, args)?;
    Ok(ToolOutput::json(&sketch_report(session, id)))
}

/// Makes the sketch a spec describes and returns its id.
fn make_sketch(session: &mut Session, spec: &Value) -> Result<FeatureId> {
    let plane = plane_ref(session, args::get(spec, "plane")?)?;
    let geometry = plane_geometry(session, &plane)?;
    let items = args::get(spec, "items")?
        .as_array()
        .ok_or("\"items\" must be a list")?
        .clone();
    let mut sketch = ConstrainedSketch::new();
    let labels = {
        let mut builder = Builder::new(&mut sketch, geometry);
        builder.add_all(&items)?;
        builder.labels.clone()
    };
    let sketch_plane = match plane {
        PlaneRef::Standard(s) => SketchPlane::Standard(s),
        PlaneRef::Face(f) => SketchPlane::Face(f),
        PlaneRef::Reference(id) => SketchPlane::Reference(id),
    };
    let name = args::opt_str(spec, "name")?.map(str::to_owned);
    session.change(|doc| {
        let id = add_named(
            doc,
            FeatureKind::Sketch(Sketch {
                plane: sketch_plane,
                geometry: sketch,
            }),
            name.as_deref(),
        )?;
        doc.labels.insert(id, labels);
        Ok(id)
    })
}

fn sketch_report(session: &Session, id: FeatureId) -> Value {
    let mut out = report(session, id);
    let Some(doc) = session.part.as_ref() else {
        return out;
    };
    if let Some(result) = doc.part.sketch_result(id) {
        out.set("degrees_of_freedom", json!(result.degrees_of_freedom));
        out.set(
            "profile",
            match &result.profile {
                Ok(p) => json!({ "regions": p.regions.len(),
                "holes": p.regions.iter().map(|r| r.holes.len()).sum::<usize>() }),
                Err(e) => json!({ "error": format!("{e:?}") }),
            },
        );
    }
    let name = doc
        .part
        .feature(id)
        .map(|f| f.name().to_owned())
        .unwrap_or_default();
    let dims: Vec<Value> = doc
        .part
        .dimensions()
        .into_iter()
        .filter(|d| d.reference.feature == id)
        .map(|d| json!([d.full_name, args::round(d.unit.to_display(d.value))]))
        .collect();
    out.set("sketch", json!(name));
    out.set("dimensions", json!(dims.len()));
    if dims.len() <= 24 {
        out.set("dimension_values", Value::Array(dims));
    }
    if let Some(labels) = doc.labels.get(&id)
        && !labels.is_empty()
        && labels.len() <= 60
    {
        out.set("labels", json!(labels));
    }
    out
}

/// A sketch argument: a name or id, or a spec made on the way.
fn sketch_arg(session: &mut Session, value: &Value) -> Result<FeatureId> {
    if value.is_object() {
        return make_sketch(session, value);
    }
    let id = feature_id(&session.doc()?.part, value)?;
    match session.doc()?.part.feature(id).map(Feature::kind) {
        Some(FeatureKind::Sketch(_)) => Ok(id),
        _ => Err(format!("{value} is not a sketch")),
    }
}

// ---- Features ------------------------------------------------------------------------------------

pub(crate) fn extrude(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let op = operation(args, ExtrudeOp::Boss)?;
    let depth = args::opt_f64(args, "depth")?;
    let extent = match args::opt(args, "end") {
        None => ExtrudeExtent::Blind {
            depth: depth.ok_or("a blind extrusion needs \"depth\"")?,
        },
        Some(Value::String(end)) => match end.as_str() {
            "blind" => ExtrudeExtent::Blind {
                depth: depth.ok_or("a blind extrusion needs \"depth\"")?,
            },
            "mid_plane" | "midplane" | "mid-plane" => ExtrudeExtent::MidPlane {
                depth: depth.ok_or("a mid-plane extrusion needs \"depth\"")?,
            },
            "through_all" => ExtrudeExtent::ThroughAll { both: false },
            "through_all_both" => ExtrudeExtent::ThroughAll { both: true },
            other => return Err(format!("unknown end condition \"{other}\"")),
        },
        Some(other) => {
            let plane = args::get(other, "up_to")?;
            ExtrudeExtent::UpToSurface {
                plane: plane_ref(session, plane)?,
            }
        }
    };
    if extent.depth().is_some_and(|d| d <= 0.0) {
        return Err(
            "\"depth\" must be more than 0 (use \"reverse\" for the other side)".to_owned(),
        );
    }
    let bodies = body_ids(session, args::opt(args, "bodies"))?;
    let reverse = args::bool(args, "reverse", false)?;
    let sketch = sketch_arg(session, args::get(args, "sketch")?)?;
    add(
        session,
        FeatureKind::Extrude(Extrude {
            sketch,
            op,
            extent,
            reverse,
            bodies,
        }),
        args,
    )
}

pub(crate) fn revolve(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let op = operation(args, ExtrudeOp::Boss)?;
    let angle = args::opt_f64(args, "angle")?.unwrap_or(360.0);
    if !(angle > 0.0 && angle <= 360.0) {
        return Err("\"angle\" must be more than 0 and at most 360 degrees".to_owned());
    }
    let bodies = body_ids(session, args::opt(args, "bodies"))?;
    let sketch = sketch_arg(session, args::get(args, "sketch")?)?;
    let axis_value = args::get(args, "axis")?;
    let axis = match args::opt(axis_value, "line") {
        Some(line) => {
            let entity = match line.as_u64() {
                Some(n) => n as u32,
                None => {
                    let label = line.as_str().ok_or("\"line\" is a label or an entity id")?;
                    *session
                        .doc()?
                        .labels
                        .get(&sketch)
                        .and_then(|l| l.get(label))
                        .ok_or_else(|| format!("no line labelled \"{label}\" in the sketch"))?
                }
            };
            RevolveAxis::SketchLine(entity)
        }
        None => RevolveAxis::Axis(axis_ref(session, axis_value)?),
    };
    add(
        session,
        FeatureKind::Revolve(Revolve {
            sketch,
            op,
            axis,
            angle: angle.to_radians(),
            mid_plane: args::bool(args, "mid_plane", false)?,
            reverse: args::bool(args, "reverse", false)?,
            bodies,
        }),
        args,
    )
}

/// Fillet, or chamfer when `chamfer`.
pub(crate) fn blend(session: &mut Session, args: &Value, chamfer: bool) -> Result<ToolOutput> {
    let selector = args::get(args, "edges")?;
    let edges = with_scope(session, |scope| {
        let picked = scope.select(Kind::Edge, selector)?;
        scope.edge_names(&picked)
    })?;
    let (size, shape) = if chamfer {
        let distance = args::positive(args, "distance")?;
        let flip = args::bool(args, "flip", false)?;
        let shape = match (
            args::opt_f64(args, "distance2")?,
            args::opt_f64(args, "angle")?,
        ) {
            (Some(d), None) => ChamferShape::TwoDistances { distance: d, flip },
            (None, Some(a)) => ChamferShape::AngleDistance {
                angle: a.to_radians(),
                flip,
            },
            (None, None) => ChamferShape::Equal,
            _ => return Err("give \"distance2\" or \"angle\", not both".to_owned()),
        };
        (distance, shape)
    } else {
        (args::positive(args, "radius")?, ChamferShape::Equal)
    };
    let blend = EdgeBlend {
        edges,
        size,
        chamfer: shape,
    };
    let kind = if chamfer {
        FeatureKind::Chamfer(blend)
    } else {
        FeatureKind::Fillet(blend)
    };
    add(session, kind, args)
}

pub(crate) fn shell(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let faces = match args::opt(args, "faces") {
        Some(selector) => with_scope(session, |scope| {
            let picked = scope.select(Kind::Face, selector)?;
            scope.face_names(&picked)
        })?,
        None => Vec::new(),
    };
    add(
        session,
        FeatureKind::Shell(Shell {
            faces,
            thickness: args::positive(args, "thickness")?,
            outward: args::bool(args, "outward", false)?,
        }),
        args,
    )
}

fn seeds(session: &Session, value: &Value) -> Result<Vec<FeatureId>> {
    let part = &session.doc()?.part;
    args::list(value)
        .into_iter()
        .map(|v| feature_id(part, v))
        .collect()
}

fn direction(session: &Session, value: &Value) -> Result<PatternDirection> {
    Ok(PatternDirection {
        axis: axis_ref(session, args::get(value, "axis")?)?,
        spacing: args::f64(value, "spacing")?,
        count: args::count(value, "count")?,
        reverse: args::bool(value, "reverse", false)?,
    })
}

pub(crate) fn linear_pattern(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let seeds = seeds(session, args::get(args, "features")?)?;
    let first = direction(session, args::get(args, "direction")?)?;
    let second = args::opt(args, "direction2")
        .map(|d| direction(session, d))
        .transpose()?;
    add(
        session,
        FeatureKind::LinearPattern(LinearPattern {
            seeds,
            first,
            second,
            skip: Vec::new(),
        }),
        args,
    )
}

pub(crate) fn circular_pattern(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let seeds = seeds(session, args::get(args, "features")?)?;
    let axis = axis_ref(session, args::get(args, "axis")?)?;
    let equal_spacing = args::bool(args, "equal_spacing", true)?;
    let angle = args::opt_f64(args, "angle")?.unwrap_or(360.0);
    add(
        session,
        FeatureKind::CircularPattern(CircularPattern {
            seeds,
            axis,
            angle: angle.to_radians(),
            count: args::count(args, "count")?,
            equal_spacing,
            reverse: args::bool(args, "reverse", false)?,
            skip: Vec::new(),
        }),
        args,
    )
}

pub(crate) fn mirror(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let seeds = match args::opt(args, "features") {
        Some(v) => seeds(session, v)?,
        None => Vec::new(),
    };
    let bodies = body_ids(session, args::opt(args, "bodies"))?;
    if seeds.is_empty() == bodies.is_empty() {
        return Err("mirror \"features\" or \"bodies\" (one of them)".to_owned());
    }
    let plane = plane_ref(session, args::get(args, "plane")?)?;
    add(
        session,
        FeatureKind::Mirror(Mirror {
            seeds,
            plane,
            bodies,
        }),
        args,
    )
}

pub(crate) fn hole(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let sketch = match (args::opt(args, "sketch"), args::opt(args, "face")) {
        (Some(s), _) => sketch_arg(session, s)?,
        (None, Some(face)) => {
            let points = args::get(args, "points")?
                .as_array()
                .ok_or("\"points\" must be a list")?;
            let items: Vec<Value> = points
                .iter()
                .map(|p| json!({ "type": "point", "at": p }))
                .collect();
            make_sketch(
                session,
                &json!({ "plane": { "face": face }, "items": items }),
            )?
        }
        (None, None) => {
            return Err("a hole needs a \"face\" and \"points\", or a \"sketch\"".to_owned());
        }
    };
    let diameter = args::positive(args, "diameter")?;
    let kind = match args::opt_str(args, "kind")?.unwrap_or("simple") {
        "simple" => HoleKind::Simple,
        "counterbore" => HoleKind::Counterbore {
            diameter: args::positive(args, "counter_diameter")?,
            depth: args::positive(args, "counter_depth")?,
        },
        "countersink" => HoleKind::Countersink {
            diameter: args::positive(args, "counter_diameter")?,
            angle: args::opt_f64(args, "angle")?.unwrap_or(90.0).to_radians(),
        },
        other => return Err(format!("unknown hole kind \"{other}\"")),
    };
    let bodies = body_ids(session, args::opt(args, "bodies"))?;
    add(
        session,
        FeatureKind::Hole(Hole {
            sketch,
            kind,
            diameter,
            depth: args::opt_f64(args, "depth")?,
            reverse: args::bool(args, "reverse", false)?,
            bodies,
        }),
        args,
    )
}

pub(crate) fn ref_plane(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let base = plane_ref(session, args::get(args, "base")?)?;
    let offset = args::f64(args, "offset")?;
    let flip = args::bool(args, "flip", false)? != (offset < 0.0);
    add(
        session,
        FeatureKind::Plane(RefPlane {
            base,
            offset: offset.abs(),
            flip,
        }),
        args,
    )
}

pub(crate) fn ref_axis(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let axis = match (args::opt(args, "along"), args::opt(args, "planes")) {
        (Some(along), None) => RefAxis::Along(axis_ref(session, along)?),
        (None, Some(Value::Array(planes))) if planes.len() == 2 => {
            let a = plane_ref(session, planes.first().unwrap_or(&Value::Null))?;
            let b = plane_ref(session, planes.get(1).unwrap_or(&Value::Null))?;
            RefAxis::TwoPlanes(a, b)
        }
        _ => return Err("give \"along\" (an axis) or \"planes\" (two planes)".to_owned()),
    };
    add(session, FeatureKind::Axis(axis), args)
}

pub(crate) fn loft(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let op = operation(args, ExtrudeOp::Boss)?;
    let mut profiles = Vec::new();
    for p in args::list(args::get(args, "profiles")?) {
        profiles.push(sketch_arg(session, p)?);
    }
    let bodies = body_ids(session, args::opt(args, "bodies"))?;
    add(
        session,
        FeatureKind::Loft(Loft {
            profiles,
            op,
            ruled: args::bool(args, "ruled", false)?,
            bodies,
        }),
        args,
    )
}

pub(crate) fn sweep(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let op = operation(args, ExtrudeOp::Boss)?;
    let profile = sketch_arg(session, args::get(args, "profile")?)?;
    let path = sketch_arg(session, args::get(args, "path")?)?;
    let bodies = body_ids(session, args::opt(args, "bodies"))?;
    add(
        session,
        FeatureKind::Sweep(Sweep {
            profile,
            path,
            op,
            bodies,
        }),
        args,
    )
}

pub(crate) fn combine(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let target = body_ids(session, args::opt(args, "target"))?
        .into_iter()
        .next()
        .ok_or("no target body")?;
    let tools = body_ids(session, args::opt(args, "tools"))?;
    let op = match args::str(args, "operation")? {
        "join" => CombineOp::Join,
        "cut" => CombineOp::Cut,
        "intersect" => CombineOp::Intersect,
        other => return Err(format!("unknown operation \"{other}\"")),
    };
    add(
        session,
        FeatureKind::Combine(Combine {
            target,
            tools,
            op,
            keep_tools: args::bool(args, "keep_tools", false)?,
        }),
        args,
    )
}

// ---- Raw features and edits ----------------------------------------------------------------------

/// A feature from its `model.json` form; `id` and `name` are filled in when missing.
fn parse_feature(value: &Value) -> Result<Feature> {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        object.entry("id").or_insert(json!(u32::MAX));
        object.entry("name").or_insert(json!("Feature"));
    }
    oxyde_format::values::feature_from_json(value).map_err(|e| e.to_string())
}

pub(crate) fn add_feature(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let feature = parse_feature(args::get(args, "feature")?)?;
    add(session, feature.kind().clone(), args)
}

/// Imports a STEP file's solids into the part (File › Import STEP): an Imported feature,
/// the file kept in the part.
pub(crate) fn import_step(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let path = args::path(args, "path")?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    add(
        session,
        FeatureKind::Import(oxyde_model::Import::new(
            name,
            oxyde_model::FileData::new(bytes),
        )),
        args,
    )
}

pub(crate) fn get_feature(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let part = &session.doc()?.part;
    let id = feature_id(part, args::get(args, "feature")?)?;
    let feature = part.feature(id).ok_or("no such feature")?;
    let value = oxyde_format::values::feature_to_json(feature).map_err(|e| e.to_string())?;
    Ok(ToolOutput::json(&value))
}

pub(crate) fn edit_feature(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let id = feature_id(&session.doc()?.part, args::get(args, "feature")?)?;
    let feature = parse_feature(args::get(args, "definition")?)?;
    session.change(|doc| {
        doc.part
            .replace_feature(id, feature.kind().clone())
            .map_err(|e| e.to_string())
    })?;
    Ok(ToolOutput::json(&report(session, id)))
}

pub(crate) fn set_dimension(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let name = args::str(args, "name")?;
    let value = args::f64(args, "value")?;
    let part = &session.doc()?.part;
    let reference = part
        .find_dimension(name)
        .ok_or_else(|| format!("no dimension \"{name}\""))?;
    let unit = part
        .dimension(reference)
        .map(|d| d.unit)
        .ok_or("no such dimension")?;
    let internal = unit.from_display(value)?;
    session.change(|doc| {
        doc.part
            .set_dimension(reference, internal)
            .map_err(|e| e.to_string())
    })?;
    Ok(ToolOutput::json(&report(session, reference.feature)))
}

pub(crate) fn rename_dimension(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let name = args::str(args, "name")?;
    let new_name = args::str(args, "new_name")?;
    let reference = session
        .doc()?
        .part
        .find_dimension(name)
        .ok_or_else(|| format!("no dimension \"{name}\""))?;
    session.change(|doc| {
        doc.part
            .rename_dimension(reference, new_name)
            .map_err(|e| e.to_string())
    })?;
    Ok(ToolOutput::text(format!("{name} is now {new_name}")))
}

pub(crate) fn set_variable(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let name = args::str(args, "name")?.to_owned();
    let text = args::str(args, "expression")?.to_owned();
    session.change(|doc| {
        let expr = doc
            .part
            .parse_expression(&text)
            .map_err(|e| e.to_string())?;
        let existing = doc
            .part
            .equations()
            .globals
            .iter()
            .find(|g| g.name == name)
            .map(|g| g.id);
        match existing {
            Some(id) => doc.part.set_global_expression(id, expr),
            None => doc.part.add_global(&name, expr).map(|_| ()),
        }
        .map_err(|e| e.to_string())
    })?;
    equation_report(session)
}

pub(crate) fn set_equation(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let dimension = args::str(args, "dimension")?.to_owned();
    let text = args::str(args, "expression")?.to_owned();
    session.change(|doc| {
        let target = doc
            .part
            .find_dimension(&dimension)
            .ok_or_else(|| format!("no dimension \"{dimension}\""))?;
        let expr = doc
            .part
            .parse_expression(&text)
            .map_err(|e| e.to_string())?;
        let existing = doc
            .part
            .equations()
            .equations
            .iter()
            .position(|e| e.target == Some(target));
        match existing {
            Some(index) => doc.part.set_equation_expression(index, expr),
            None => doc.part.add_equation(target, expr).map(|_| ()),
        }
        .map_err(|e| e.to_string())
    })?;
    equation_report(session)
}

fn equation_report(session: &Session) -> Result<ToolOutput> {
    let part = &session.doc()?.part;
    let text = part.definition().equations_text();
    let mut out = json!({
        "variables": text.globals.iter().map(|g| json!([g.name, g.expression])).collect::<Vec<_>>(),
        "equations": text.equations.iter().map(|e| json!([e.dimension, e.expression])).collect::<Vec<_>>(),
    });
    let problems = status_report(part, &[]);
    if let Some(p) = problems.get("other_problems") {
        out.set("problems", p.clone());
    }
    Ok(ToolOutput::json(&out))
}

pub(crate) fn delete_feature(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let id = feature_id(&session.doc()?.part, args::get(args, "feature")?)?;
    session.change(|doc| doc.part.delete(id).map_err(|e| e.to_string()))?;
    let doc = session.doc()?;
    Ok(ToolOutput::json(
        &json!({ "deleted": id.raw(), "body": body_summary(&session.kernel, &doc.part) }),
    ))
}

pub(crate) fn suppress_feature(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let id = feature_id(&session.doc()?.part, args::get(args, "feature")?)?;
    let suppressed = args::bool(args, "suppressed", true)?;
    session.change(|doc| {
        doc.part
            .set_suppressed(id, suppressed)
            .map_err(|e| e.to_string())
    })?;
    Ok(ToolOutput::json(&report(session, id)))
}

pub(crate) fn rename_feature(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let id = feature_id(&session.doc()?.part, args::get(args, "feature")?)?;
    let name = args::str(args, "name")?.to_owned();
    session.change(|doc| doc.part.rename(id, name).map_err(|e| e.to_string()))?;
    Ok(ToolOutput::text("renamed"))
}

pub(crate) fn rollback(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let before = match args::opt(args, "before") {
        None => None,
        Some(v) => Some(feature_id(&session.doc()?.part, v)?),
    };
    session.change(|doc| doc.part.roll_back_to(before).map_err(|e| e.to_string()))?;
    let doc = session.doc()?;
    Ok(ToolOutput::json(
        &json!({ "rollback": doc.part.rollback_position(),
        "body": body_summary(&session.kernel, &doc.part) }),
    ))
}
