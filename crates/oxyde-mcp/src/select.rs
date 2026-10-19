//! Selectors: how a tool call picks faces and edges, by what they are and where they are,
//! since an agent sees no viewport to click in. A selector is an object whose keys all
//! apply (a list of selectors is their union):
//!
//! - `"name"`: the persistent name as the tools print it (`"F2.end"`), or as `model.json`
//!   writes it (a FaceRef or EdgeRef object); a plain string is taken as a name;
//! - `"index"`: index (or list of indices) in the listing of `faces` or `edges`;
//! - `"surface"` (faces: `plane`, `cylinder`, `cone`, `sphere`, `torus`, `bspline`) or
//!   `"curve"` (edges: `line`, `circle`, `ellipse`, `bspline`);
//! - `"normal"`: `[x, y, z]` or `"+x"`, `"-y"`…: flat faces facing that way;
//! - `"axis"`: `[x, y, z]` or `"x"`…: round faces turning about, straight edges running
//!   along, or circular edges facing that direction, either way;
//! - `"radius"` or `"diameter"`: a value (within 0.001) or `[min, max]`;
//! - `"area"` (faces) or `"length"` (edges): `[min, max]`;
//! - `"box"`: `{"min": [x, y, z], "max": [x, y, z]}` holding the face's centroid or the
//!   edge's midpoint;
//! - `"created_by"`: a feature name or id: the faces it made, the edges of those faces;
//! - `"of_faces"` (edges): a selector of faces whose edges are wanted;
//! - `"extreme"`: `"+y"`, `"-x"`…: of what is left, those furthest that way;
//! - `"near"`: `[x, y, z]`: of what is left, the one closest to the point (`"count": n`
//!   for the n closest, `"within": d` for all within d mm of it).

use std::collections::BTreeSet;

use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{CurveKind, Mesh, SurfaceKind, Topology};
use oxyde_model::{EdgeName, FaceName, Part};
use serde_json::{Value, json};

use crate::args::{self, Result};
use crate::geometry::{Solids, point_segment_distance, point_triangle_distance, v3};
use crate::session::feature_id;

/// Faces or edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Face,
    Edge,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Face => "face",
            Kind::Edge => "edge",
        }
    }
}

/// What selectors look at: the solids, their mesh, and the part (for `created_by`).
pub(crate) struct Scope<'a> {
    pub solids: &'a Solids<'a>,
    pub mesh: &'a Mesh,
    pub part: Option<&'a Part>,
}

impl Scope<'_> {
    fn topology(&self) -> &Topology {
        self.solids.topology
    }

    fn count(&self, kind: Kind) -> usize {
        match kind {
            Kind::Face => self.topology().faces.len(),
            Kind::Edge => self.topology().edges.len(),
        }
    }

    /// Picks faces or edges with `selector`; an empty result is an error, since a feature
    /// given nothing to work on is never what was meant.
    pub fn select(&self, kind: Kind, selector: &Value) -> Result<Vec<u32>> {
        let found = self.select_any(kind, selector)?;
        if found.is_empty() {
            return Err(format!("no {} matches {selector}", kind.word()));
        }
        Ok(found)
    }

    /// Like [`Scope::select`], an empty result allowed.
    pub fn select_any(&self, kind: Kind, selector: &Value) -> Result<Vec<u32>> {
        if let Value::Array(items) = selector {
            let mut all = BTreeSet::new();
            for item in items {
                all.extend(self.select_one(kind, item)?);
            }
            return Ok(all.into_iter().collect());
        }
        self.select_one(kind, selector)
    }

    fn select_one(&self, kind: Kind, selector: &Value) -> Result<Vec<u32>> {
        if let Some(name) = selector.as_str() {
            return self.by_name(kind, &Value::String(name.to_owned()));
        }
        if let Some(index) = selector.as_u64() {
            return self.by_index(kind, &json!(index));
        }
        let object = selector.as_object().ok_or_else(|| {
            format!(
                "a {} selector is an object, a name or an index",
                kind.word()
            )
        })?;
        const KEYS: [&str; 18] = [
            "name",
            "index",
            "surface",
            "curve",
            "normal",
            "axis",
            "radius",
            "diameter",
            "area",
            "length",
            "box",
            "created_by",
            "of_faces",
            "extreme",
            "near",
            "count",
            "within",
            "all",
        ];
        if let Some(unknown) = object.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("unknown selector key \"{unknown}\""));
        }
        let mut candidates: Vec<u32> = if let Some(name) = object.get("name") {
            self.by_name(kind, name)?
        } else if let Some(index) = object.get("index") {
            self.by_index(kind, index)?
        } else {
            (0..self.count(kind) as u32).collect()
        };
        let topology = self.topology();
        if let Some(surface) = object.get("surface") {
            let want = surface_kind(surface)?;
            candidates.retain(|&f| face(topology, f).is_some_and(|x| x.surface == want));
        }
        if let Some(curve) = object.get("curve") {
            let want = curve_kind(curve)?;
            candidates.retain(|&e| edge(topology, e).is_some_and(|x| x.curve == want));
        }
        if let Some(normal) = object.get("normal") {
            let want = direction(normal)?;
            candidates.retain(|&f| {
                face(topology, f)
                    .and_then(|x| x.normal)
                    .is_some_and(|n| n.dot(want) > 1.0 - 1e-6)
            });
        }
        if let Some(axis) = object.get("axis") {
            let want = direction(axis)?;
            candidates.retain(|&i| {
                self.axis_of(kind, i)
                    .is_some_and(|d| d.dot(want).abs() > 1.0 - 1e-6)
            });
        }
        for (key, scale) in [("radius", 1.0), ("diameter", 2.0)] {
            if let Some(value) = object.get(key) {
                let (lo, hi) = range(value, key, 1e-3)?;
                candidates.retain(|&i| {
                    self.radius_of(kind, i)
                        .is_some_and(|r| r * scale >= lo && r * scale <= hi)
                });
            }
        }
        if let Some(area) = object.get("area") {
            let (lo, hi) = range(area, "area", 1e-6)?;
            candidates.retain(|&f| face(topology, f).is_some_and(|x| x.area >= lo && x.area <= hi));
        }
        if let Some(length) = object.get("length") {
            let (lo, hi) = range(length, "length", 1e-6)?;
            candidates
                .retain(|&e| edge(topology, e).is_some_and(|x| x.length >= lo && x.length <= hi));
        }
        if let Some(b) = object.get("box") {
            let lo = args::point3(args::get(b, "min")?, "box min")?;
            let hi = args::point3(args::get(b, "max")?, "box max")?;
            candidates.retain(|&i| {
                self.center_of(kind, i).is_some_and(|p| {
                    p.x >= lo.x - 1e-9
                        && p.y >= lo.y - 1e-9
                        && p.z >= lo.z - 1e-9
                        && p.x <= hi.x + 1e-9
                        && p.y <= hi.y + 1e-9
                        && p.z <= hi.z + 1e-9
                })
            });
        }
        if let Some(feature) = object.get("created_by") {
            let part = self
                .part
                .ok_or("\"created_by\" only applies to the part being modelled")?;
            let naming = self.solids.naming.ok_or("these faces have no names")?;
            let id = feature_id(part, feature)?;
            let made = |f: u32| naming.face(f).is_some_and(|n| n.key.feature() == id);
            candidates.retain(|&i| match kind {
                Kind::Face => made(i),
                Kind::Edge => edge(topology, i).is_some_and(|e| e.faces.iter().any(|&f| made(f))),
            });
        }
        if let Some(faces) = object.get("of_faces") {
            if kind != Kind::Edge {
                return Err("\"of_faces\" selects edges".to_owned());
            }
            let faces = self.select(Kind::Face, faces)?;
            let edges: BTreeSet<u32> = faces
                .iter()
                .filter_map(|&f| face(topology, f))
                .flat_map(|f| f.edges.iter().copied())
                .collect();
            candidates.retain(|e| edges.contains(e));
        }
        if let Some(extreme) = object.get("extreme") {
            let way = direction(extreme)?;
            let along = |i: u32| self.center_of(kind, i).map(|p| p.to_vec().dot(way));
            let best = candidates
                .iter()
                .filter_map(|&i| along(i))
                .fold(f64::NEG_INFINITY, f64::max);
            candidates.retain(|&i| along(i).is_some_and(|d| d >= best - 1e-6));
        }
        if let Some(near) = object.get("near") {
            let p = args::point3(near, "\"near\"")?.to_vec();
            let mut scored: Vec<(f64, u32)> = candidates
                .iter()
                .map(|&i| (self.distance_to(kind, i, p), i))
                .collect();
            scored.sort_by(|a, b| a.0.total_cmp(&b.0));
            let closest = scored.first().map_or(0.0, |s| s.0);
            candidates = if let Some(within) = object.get("within") {
                let d = args::number(within, "\"within\"")?;
                scored
                    .iter()
                    .filter(|(s, _)| *s <= closest + d)
                    .map(|(_, i)| *i)
                    .collect()
            } else {
                let count = object.get("count").and_then(Value::as_u64).unwrap_or(1) as usize;
                scored.iter().take(count).map(|(_, i)| *i).collect()
            };
        }
        candidates.sort_unstable();
        candidates.dedup();
        Ok(candidates)
    }

    fn by_index(&self, kind: Kind, value: &Value) -> Result<Vec<u32>> {
        let count = self.count(kind);
        args::list(value)
            .into_iter()
            .map(|v| {
                v.as_u64()
                    .filter(|&i| (i as usize) < count)
                    .map(|i| i as u32)
                    .ok_or_else(|| format!("no {} with index {v} (there are {count})", kind.word()))
            })
            .collect()
    }

    fn by_name(&self, kind: Kind, value: &Value) -> Result<Vec<u32>> {
        let naming = self
            .solids
            .naming
            .ok_or("these faces and edges have no names: select them another way")?;
        let found = match (value, kind) {
            (Value::String(text), _) => {
                let label = |i: u32| match kind {
                    Kind::Face => self.solids.face_label(i),
                    Kind::Edge => self.solids.edge_label(i),
                };
                (0..self.count(kind) as u32)
                    .filter(|&i| label(i) == *text)
                    .collect()
            }
            (object, Kind::Face) => {
                let name = oxyde_format::values::face_from_json(object.clone())
                    .map_err(|e| format!("not a face name: {e}"))?;
                naming.find_face(&name).into_iter().collect()
            }
            (object, Kind::Edge) => {
                let name = oxyde_format::values::edge_from_json(object.clone())
                    .map_err(|e| format!("not an edge name: {e}"))?;
                naming.find_edge(&name).into_iter().collect()
            }
        };
        Ok(found)
    }

    /// A face's centroid, an edge's midpoint.
    pub fn center_of(&self, kind: Kind, i: u32) -> Option<Point3> {
        match kind {
            Kind::Face => face(self.topology(), i).map(|f| f.centroid),
            Kind::Edge => edge(self.topology(), i).map(|e| e.midpoint),
        }
    }

    /// The axis of a round face, the direction of a straight edge, the normal of a
    /// circular one.
    pub fn axis_of(&self, kind: Kind, i: u32) -> Option<Vec3> {
        match kind {
            Kind::Face => face(self.topology(), i)
                .and_then(|f| f.axis)
                .map(|a| a.direction),
            Kind::Edge => edge(self.topology(), i)
                .filter(|e| matches!(e.curve, CurveKind::Line | CurveKind::Circle))
                .and_then(|e| e.axis)
                .map(|a| a.direction),
        }
    }

    /// Radius of a round face or a circular edge.
    pub fn radius_of(&self, kind: Kind, i: u32) -> Option<f64> {
        let topology = self.topology();
        match kind {
            Kind::Edge => {
                let e = edge(topology, i)?;
                if e.curve != CurveKind::Circle {
                    return None;
                }
                let axis = e.axis?;
                Some((e.midpoint - axis.origin).length())
            }
            Kind::Face => {
                let f = face(topology, i)?;
                if !matches!(f.surface, SurfaceKind::Cylinder | SurfaceKind::Sphere) {
                    return None;
                }
                let axis = f.axis?;
                if f.surface == SurfaceKind::Sphere {
                    let p = self.mesh.faces.get(i as usize)?.positions.first()?;
                    return Some((Point3::ORIGIN + v3(*p) - axis.origin).length());
                }
                // The circles bounding a cylinder have its radius; else its mesh.
                f.edges
                    .iter()
                    .filter_map(|&e| self.radius_of(Kind::Edge, e))
                    .next()
                    .or_else(|| {
                        let mesh = self.mesh.faces.get(i as usize)?;
                        let p = mesh.positions.first()?;
                        Some(axis.distance_to(Point3::ORIGIN + v3(*p)))
                    })
            }
        }
    }

    fn distance_to(&self, kind: Kind, i: u32, p: Vec3) -> f64 {
        match kind {
            Kind::Face => self
                .mesh
                .faces
                .get(i as usize)
                .map(|m| {
                    m.triangles
                        .iter()
                        .filter_map(|t| {
                            let a = m.positions.get(t[0] as usize)?;
                            let b = m.positions.get(t[1] as usize)?;
                            let c = m.positions.get(t[2] as usize)?;
                            Some(point_triangle_distance(p, v3(*a), v3(*b), v3(*c)))
                        })
                        .fold(f64::INFINITY, f64::min)
                })
                .unwrap_or(f64::INFINITY),
            Kind::Edge => self
                .mesh
                .edges
                .get(i as usize)
                .map(|line| {
                    line.windows(2)
                        .filter_map(|w| {
                            let (a, b) = (w.first()?, w.get(1)?);
                            Some(point_segment_distance(p, v3(*a), v3(*b)))
                        })
                        .fold(f64::INFINITY, f64::min)
                })
                .unwrap_or(f64::INFINITY),
        }
    }

    /// Persistent names of faces, for a feature to keep.
    pub fn face_names(&self, faces: &[u32]) -> Result<Vec<FaceName>> {
        let naming = self.solids.naming.ok_or("these faces have no names")?;
        faces
            .iter()
            .map(|&f| {
                naming
                    .face(f)
                    .cloned()
                    .ok_or_else(|| format!("face {f} has no name"))
            })
            .collect()
    }

    pub fn edge_names(&self, edges: &[u32]) -> Result<Vec<EdgeName>> {
        let naming = self.solids.naming.ok_or("these edges have no names")?;
        edges
            .iter()
            .map(|&e| {
                naming
                    .edge(e)
                    .cloned()
                    .ok_or_else(|| format!("edge {e} has no name"))
            })
            .collect()
    }
}

fn face(topology: &Topology, i: u32) -> Option<&oxyde_kernel::FaceInfo> {
    topology.faces.get(i as usize)
}

fn edge(topology: &Topology, i: u32) -> Option<&oxyde_kernel::EdgeInfo> {
    topology.edges.get(i as usize)
}

fn surface_kind(value: &Value) -> Result<SurfaceKind> {
    Ok(match value.as_str().unwrap_or("") {
        "plane" | "planar" | "flat" => SurfaceKind::Plane,
        "cylinder" | "cylindrical" => SurfaceKind::Cylinder,
        "cone" | "conical" => SurfaceKind::Cone,
        "sphere" | "spherical" => SurfaceKind::Sphere,
        "torus" | "toroidal" => SurfaceKind::Torus,
        "bspline" => SurfaceKind::BSpline,
        "other" => SurfaceKind::Other,
        _ => return Err(format!("unknown surface kind {value}")),
    })
}

fn curve_kind(value: &Value) -> Result<CurveKind> {
    Ok(match value.as_str().unwrap_or("") {
        "line" | "straight" => CurveKind::Line,
        "circle" | "arc" | "circular" => CurveKind::Circle,
        "ellipse" => CurveKind::Ellipse,
        "bspline" => CurveKind::BSpline,
        "other" => CurveKind::Other,
        _ => return Err(format!("unknown curve kind {value}")),
    })
}

/// A unit direction: `[x, y, z]`, or `"x"`, `"+y"`, `"-z"`.
pub(crate) fn direction(value: &Value) -> Result<Vec3> {
    if let Some(text) = value.as_str() {
        let (sign, axis) = match text.as_bytes().first() {
            Some(b'-') => (-1.0, text.get(1..).unwrap_or("")),
            Some(b'+') => (1.0, text.get(1..).unwrap_or("")),
            _ => (1.0, text),
        };
        let v = match axis.to_ascii_lowercase().as_str() {
            "x" => Vec3::X,
            "y" => Vec3::Y,
            "z" => Vec3::Z,
            _ => return Err(format!("unknown direction \"{text}\"")),
        };
        return Ok(v * sign);
    }
    args::vec3(value, "a direction")?
        .normalized()
        .ok_or_else(|| "a direction cannot be zero".to_owned())
}

/// A value within `tolerance`, or `[min, max]`.
fn range(value: &Value, key: &str, tolerance: f64) -> Result<(f64, f64)> {
    if let Some(v) = value.as_f64() {
        return Ok((v - tolerance, v + tolerance));
    }
    match value.as_array().map(Vec::as_slice) {
        Some([lo, hi]) => Ok((args::number(lo, key)?, args::number(hi, key)?)),
        _ => Err(format!("\"{key}\" must be a number or [min, max]")),
    }
}
