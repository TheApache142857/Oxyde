//! Reading STEP files (AP203, AP214, AP242) into the kernel's shapes: the B-rep solids of
//! every product, placed where the product structure puts them, in millimetres, with their
//! colours and the materials their products name.
//!
//! Planes, cylinders, cones, spheres, tori and B-spline surfaces (rational or not) are read
//! as they are; a surface of linear extrusion or of revolution as the plane, cylinder,
//! cone, sphere or torus it is when it sweeps a line or a circle that way, else as the
//! B-spline surface it is exactly. Lines, circles and B-spline curves are read as they are,
//! an ellipse as a plane's section of a cylinder (what the kernel's ellipses are), a
//! polyline as a B-spline of degree one.
//!
//! The kernel's faces are what OpenCASCADE's are, which STEP files do not always spell
//! out, so each face is settled once its solid is read:
//! - a face that goes round its axis between two loops without a seam (writers other
//!   than OpenCASCADE mostly write a cylinder's side as its two circles) is given one, up a
//!   straight line or a meridian where no hole is in the way, the loops' edges split there
//!   when they have no vertex at it; a face round to a pole (a sphere's cap, a cone's point)
//!   gets its seam to the pole; a whole sphere bounded by a vertex, or by holes alone, gets
//!   its seam and both poles;
//! - where a loop on a cone or a sphere reaches the apex or a pole, the degenerate edge
//!   STEP leaves out is put back, turning about the axis as far as the loop does there;
//!   edges of no length that some writers put there are left out;
//! - the outer loop is put first, and a shell whose faces point the wrong way is turned.

use std::collections::{HashMap, HashSet};
use std::f64::consts::{PI, TAU};
use std::rc::Rc;

use oxyde_geom::{Axis, Plane, Point3, Transform, Vec3};
use oxyde_kernel::{
    StepAssembly, StepColor, StepColors, StepInstance, StepMaterial, StepNode, StepNodeContent,
};

use super::exact::{self, ExactCurve};
use super::p21::{Exchange, Instance, Param, Record, parse};
use crate::brep::{BASE_TOLERANCE, Edge, EdgeUse, Face, MAX_TOLERANCE, Shape, Solid, Vertex};
use crate::geometry::{Circle, Curve, Section, SplineCurve, SplineSurface, Surface, perpendicular};
use crate::nurbs::{BSplineCurve, BSplineSurface, inverse};
use crate::revolution::{Meridian, ParamPath, Revolution};

type R<T> = Result<T, String>;

fn bad<T>(what: &str, id: u64) -> R<T> {
    Err(format!("#{id}: {what}"))
}

/// Points this close to a surface's axis have no angle about it.
const NEAR_AXIS: f64 = 10.0 * BASE_TOLERANCE;

/// An edge whose curve stays this close to its one vertex has no length.
const NO_LENGTH: f64 = 1e-6;

// ---------------------------------------------------------------------------------------
// The file.

/// A file read, with its instances looked up by name.
struct File {
    x: Exchange,
    by_name: HashMap<String, Vec<u64>>,
}

impl File {
    fn read(data: &[u8]) -> R<File> {
        let x = parse(data).map_err(|e| format!("not a STEP file: {e}"))?;
        let mut by_name: HashMap<String, Vec<u64>> = HashMap::new();
        for (&id, inst) in &x.data {
            match inst {
                Instance::Simple(r) => by_name.entry(r.name.clone()).or_default().push(id),
                Instance::Complex(rs) => {
                    for r in rs {
                        by_name.entry(r.name.clone()).or_default().push(id);
                    }
                }
            }
        }
        for ids in by_name.values_mut() {
            ids.sort_unstable();
            ids.dedup();
        }
        Ok(File { x, by_name })
    }

    /// The instances of this name, or with a part of it, in the order they are numbered.
    fn all(&self, name: &str) -> &[u64] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    fn get(&self, id: u64) -> R<&Instance> {
        self.x
            .data
            .get(&id)
            .map_or_else(|| bad("no such instance", id), Ok)
    }

    fn record(&self, id: u64, name: &str) -> R<&Record> {
        self.get(id)?
            .record(name)
            .map_or_else(|| bad(&format!("not a {name}"), id), Ok)
    }

    /// A simple instance's name, or the most particular one a complex instance's parts
    /// name.
    fn kind(&self, id: u64) -> R<&str> {
        const PARTICULAR: [&str; 8] = [
            "B_SPLINE_CURVE_WITH_KNOTS",
            "B_SPLINE_SURFACE_WITH_KNOTS",
            "BEZIER_CURVE",
            "BEZIER_SURFACE",
            "QUASI_UNIFORM_CURVE",
            "QUASI_UNIFORM_SURFACE",
            "UNIFORM_CURVE",
            "UNIFORM_SURFACE",
        ];
        match self.get(id)? {
            Instance::Simple(r) => Ok(&r.name),
            Instance::Complex(rs) => Ok(rs
                .iter()
                .map(|r| r.name.as_str())
                .find(|n| PARTICULAR.contains(n))
                .or_else(|| rs.first().map(|r| r.name.as_str()))
                .unwrap_or("")),
        }
    }

    /// A simple instance's record, or the first part of a complex one with at least
    /// `count` parameters.
    fn main_record(&self, id: u64, count: usize) -> Option<&Record> {
        match self.get(id).ok()? {
            Instance::Simple(r) => Some(r),
            Instance::Complex(rs) => rs.iter().find(|r| r.params.len() >= count),
        }
    }
}

fn real(p: &Param) -> Option<f64> {
    match p {
        Param::Real(x) => Some(*x),
        Param::Integer(i) => Some(*i as f64),
        Param::Typed(_, v) => real(v),
        _ => None,
    }
}

fn reference(p: &Param) -> Option<u64> {
    match p {
        Param::Ref(r) => Some(*r),
        _ => None,
    }
}

fn list(p: &Param) -> &[Param] {
    match p {
        Param::List(l) => l,
        _ => &[],
    }
}

fn string(p: &Param) -> String {
    match p {
        Param::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// A logical: false only when `.F.`.
fn boolean(p: &Param) -> bool {
    !matches!(p, Param::Enum(e) if e == "F")
}

const UNSET: Param = Param::Unset;

fn param(r: &Record, i: usize) -> &Param {
    r.params.get(i).unwrap_or(&UNSET)
}

fn arg_ref(r: &Record, i: usize, id: u64) -> R<u64> {
    reference(param(r, i)).map_or_else(
        || {
            bad(
                &format!("{} without the reference it needs at {i}", r.name),
                id,
            )
        },
        Ok,
    )
}

fn arg_real(r: &Record, i: usize, id: u64) -> R<f64> {
    real(param(r, i)).map_or_else(
        || {
            bad(
                &format!("{} without the number it needs at {i}", r.name),
                id,
            )
        },
        Ok,
    )
}

/// A name, or the identifier when the name is empty.
fn name_or_id(r: &Record, name: usize, id: usize) -> String {
    let n = string(param(r, name));
    if n.trim().is_empty() {
        string(param(r, id))
    } else {
        n
    }
}

// ---------------------------------------------------------------------------------------
// Units.

/// How a representation's lengths and angles convert to millimetres and radians, and how
/// far apart in millimetres it says geometry that meets may be.
#[derive(Debug, Clone, Copy)]
struct Units {
    length: f64,
    angle: f64,
    uncertainty: f64,
}

impl Default for Units {
    fn default() -> Self {
        Units {
            length: 1.0,
            angle: 1.0,
            uncertainty: 0.0,
        }
    }
}

fn prefix(e: &str) -> f64 {
    match e {
        "EXA" => 1e18,
        "PETA" => 1e15,
        "TERA" => 1e12,
        "GIGA" => 1e9,
        "MEGA" => 1e6,
        "KILO" => 1e3,
        "HECTO" => 1e2,
        "DECA" => 1e1,
        "DECI" => 1e-1,
        "CENTI" => 1e-2,
        "MILLI" => 1e-3,
        "MICRO" => 1e-6,
        "NANO" => 1e-9,
        "PICO" => 1e-12,
        "FEMTO" => 1e-15,
        "ATTO" => 1e-18,
        _ => 1.0,
    }
}

/// Whether a unit is a length (or else an angle), and its size: in millimetres for a
/// length, radians for an angle.
fn unit_size(f: &File, id: u64, depth: u32) -> Option<(bool, f64)> {
    let inst = f.get(id).ok()?;
    if let Some(si) = inst.record("SI_UNIT") {
        let name = match param(si, 1) {
            Param::Enum(e) => e.as_str(),
            _ => "",
        };
        let p = match param(si, 0) {
            Param::Enum(e) => prefix(e),
            _ => 1.0,
        };
        return if inst.is("LENGTH_UNIT") || name == "METRE" {
            Some((true, p * 1000.0))
        } else if inst.is("PLANE_ANGLE_UNIT") || name == "RADIAN" {
            Some((false, p))
        } else {
            None
        };
    }
    let conv = inst.record("CONVERSION_BASED_UNIT")?;
    if depth > 4 {
        return None;
    }
    let measure = reference(param(conv, 1))?;
    let m = match f.get(measure).ok()? {
        Instance::Simple(r) => r,
        Instance::Complex(rs) => rs
            .iter()
            .find(|r| r.name.ends_with("MEASURE_WITH_UNIT") && r.params.len() >= 2)?,
    };
    let value = real(param(m, 0))?;
    let (base_length, base) = unit_size(f, reference(param(m, 1))?, depth + 1)?;
    let length = if inst.is("LENGTH_UNIT") {
        true
    } else if inst.is("PLANE_ANGLE_UNIT") {
        false
    } else {
        base_length
    };
    Some((length, value * base))
}

/// The units a representation context assigns.
fn units_of(f: &File, context: u64) -> Units {
    let mut out = Units::default();
    let Some(assigned) = f
        .get(context)
        .ok()
        .and_then(|i| i.record("GLOBAL_UNIT_ASSIGNED_CONTEXT"))
    else {
        return out;
    };
    for unit in list(param(assigned, 0)) {
        if let Some((length, size)) = reference(unit).and_then(|u| unit_size(f, u, 0)) {
            if length {
                out.length = size;
            } else {
                out.angle = size;
            }
        }
    }
    let uncertainties = f
        .get(context)
        .ok()
        .and_then(|i| i.record("GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT"))
        .map(|r| list(param(r, 0)).to_vec())
        .unwrap_or_default();
    for u in uncertainties.iter().filter_map(reference) {
        let Some(r) = f.main_record(u, 2) else {
            continue;
        };
        let value = real(param(r, 0)).unwrap_or(0.0);
        let size = reference(param(r, 1))
            .and_then(|unit| unit_size(f, unit, 0))
            .filter(|(length, _)| *length)
            .map_or(out.length, |(_, s)| s);
        out.uncertainty = out.uncertainty.max(value * size);
    }
    out
}

/// The context of a representation.
fn context_of(f: &File, rep: u64) -> u64 {
    f.main_record(rep, 3)
        .and_then(|r| reference(param(r, 2)))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------------------
// Geometry.

/// What reading one representation's geometry needs: the file, its units, the surfaces
/// read already.
struct Geometry<'a> {
    f: &'a File,
    units: Units,
    surfaces: HashMap<u64, Surface>,
}

/// A curve swept into a surface: its instance, and whether the sweep runs along it its
/// own way.
type Profile = (u64, bool);

impl<'a> Geometry<'a> {
    fn new(f: &'a File, units: Units) -> Self {
        Geometry {
            f,
            units,
            surfaces: HashMap::new(),
        }
    }

    fn point(&self, id: u64) -> R<Point3> {
        let r = self.f.record(id, "CARTESIAN_POINT")?;
        let c = list(param(r, 1));
        let v = |i: usize| c.get(i).and_then(real).unwrap_or(0.0) * self.units.length;
        Ok(Point3::new(v(0), v(1), v(2)))
    }

    fn direction(&self, id: u64) -> R<Vec3> {
        let r = self.f.record(id, "DIRECTION")?;
        let c = list(param(r, 1));
        let v = |i: usize| c.get(i).and_then(real).unwrap_or(0.0);
        Vec3::new(v(0), v(1), v(2))
            .normalized()
            .map_or_else(|| bad("a direction of no length", id), Ok)
    }

    fn vector(&self, id: u64) -> R<Vec3> {
        let r = self.f.record(id, "VECTOR")?;
        let d = self.direction(arg_ref(r, 1, id)?)?;
        let m = real(param(r, 2)).unwrap_or(1.0) * self.units.length;
        Ok(d * m)
    }

    /// An `AXIS2_PLACEMENT_3D`: its origin, its z and its x, square to z (ISO 10303-42's
    /// first projected axis when the file gives none).
    fn placement(&self, id: u64) -> R<(Point3, Vec3, Vec3)> {
        let r = self.f.record(id, "AXIS2_PLACEMENT_3D")?;
        let origin = self.point(arg_ref(r, 1, id)?)?;
        let z = match reference(param(r, 2)) {
            Some(d) => self.direction(d)?,
            None => Vec3::Z,
        };
        let given = match reference(param(r, 3)) {
            Some(d) => Some(self.direction(d)?),
            None => None,
        };
        let x0 = given.unwrap_or(if z.cross(Vec3::X).length() > 1e-9 {
            Vec3::X
        } else {
            Vec3::Z
        });
        let x = (x0 - z * x0.dot(z))
            .normalized()
            .unwrap_or_else(|| perpendicular(z));
        Ok((origin, z, x))
    }

    /// An `AXIS1_PLACEMENT`: its origin and direction.
    fn axis1(&self, id: u64) -> R<Axis> {
        let r = self.f.record(id, "AXIS1_PLACEMENT")?;
        let origin = self.point(arg_ref(r, 1, id)?)?;
        let direction = match reference(param(r, 2)) {
            Some(d) => self.direction(d)?,
            None => Vec3::Z,
        };
        Ok(Axis { origin, direction })
    }

    /// A face's surface as STEP gives it, its normal its own (the face's sense turns it).
    /// `near` are points of the face, which a swept surface made a B-spline must reach.
    fn surface(&mut self, id: u64, near: &[Point3]) -> R<Surface> {
        if let Some(s) = self.surfaces.get(&id) {
            return Ok(*s);
        }
        let (s, shared) = self.surface_of(id, near)?;
        if shared {
            self.surfaces.insert(id, s);
        }
        Ok(s)
    }

    /// The surface, and whether it is the same for every face on it (rather than spanning
    /// this face's points).
    fn surface_of(&self, id: u64, near: &[Point3]) -> R<(Surface, bool)> {
        let f = self.f;
        let kind = f.kind(id)?;
        let r = f.main_record(id, 1);
        let revolution = |origin: Point3, z: Vec3, x: Vec3, meridian: Meridian| {
            Surface::Revolution(Revolution {
                axis: Axis {
                    origin,
                    direction: z,
                },
                x_dir: x,
                meridian,
                outward: true,
            })
        };
        let length = self.units.length;
        let s = match (kind, r) {
            ("PLANE", Some(r)) => {
                let (origin, z, x) = self.placement(arg_ref(r, 1, id)?)?;
                Surface::Plane(Plane {
                    origin,
                    x_dir: x,
                    normal: z,
                })
            }
            ("CYLINDRICAL_SURFACE", Some(r)) => {
                let (o, z, x) = self.placement(arg_ref(r, 1, id)?)?;
                let radius = arg_real(r, 2, id)? * length;
                revolution(o, z, x, Meridian::Line { radius, slope: 0.0 })
            }
            ("CONICAL_SURFACE", Some(r)) => {
                let (o, z, x) = self.placement(arg_ref(r, 1, id)?)?;
                let radius = arg_real(r, 2, id)? * length;
                let angle = arg_real(r, 3, id)? * self.units.angle;
                revolution(
                    o,
                    z,
                    x,
                    Meridian::Line {
                        radius,
                        slope: angle.tan(),
                    },
                )
            }
            ("SPHERICAL_SURFACE", Some(r)) => {
                let (o, z, x) = self.placement(arg_ref(r, 1, id)?)?;
                let radius = arg_real(r, 2, id)? * length;
                revolution(
                    o,
                    z,
                    x,
                    Meridian::Circle {
                        center: 0.0,
                        radius,
                    },
                )
            }
            ("TOROIDAL_SURFACE" | "DEGENERATE_TOROIDAL_SURFACE", Some(r)) => {
                let (o, z, x) = self.placement(arg_ref(r, 1, id)?)?;
                let major = arg_real(r, 2, id)? * length;
                let minor = arg_real(r, 3, id)? * length;
                if major < 0.0 || minor <= 0.0 {
                    return bad("a torus of no tube", id);
                }
                // A spindle torus's inner part (its lemon) is the arc of its meridian circle
                // centred across the axis.
                let lemon = kind == "DEGENERATE_TOROIDAL_SURFACE" && !boolean(param(r, 4));
                revolution(
                    o,
                    z,
                    x,
                    Meridian::Circle {
                        center: if lemon { -major } else { major },
                        radius: minor,
                    },
                )
            }
            ("B_SPLINE_SURFACE_WITH_KNOTS", _) => spline_or_exact(self.bspline_surface(id)?),
            ("RECTANGULAR_TRIMMED_SURFACE" | "CURVE_BOUNDED_SURFACE", Some(r)) => {
                return self.surface_of(arg_ref(r, 1, id)?, near);
            }
            ("SURFACE_OF_LINEAR_EXTRUSION", Some(r)) => {
                let profile = self.profile(arg_ref(r, 1, id)?)?;
                let d = self.vector(arg_ref(r, 2, id)?)?;
                return self.extrusion(id, profile, d, near);
            }
            ("SURFACE_OF_REVOLUTION", Some(r)) => {
                let profile = self.profile(arg_ref(r, 1, id)?)?;
                let axis = self.axis1(arg_ref(r, 2, id)?)?;
                return self.revolution(id, profile, axis, near);
            }
            ("OFFSET_SURFACE", Some(r)) => {
                let (basis, shared) = self.surface_of(arg_ref(r, 1, id)?, near)?;
                let d = arg_real(r, 2, id)? * length;
                let s = basis.offset(d).map_or_else(
                    || {
                        bad(
                            "an offset of a B-spline surface, which the kernel cannot read yet",
                            id,
                        )
                    },
                    Ok,
                )?;
                return Ok((s, shared));
            }
            (other, _) => {
                return bad(
                    &format!("a surface the kernel cannot read yet ({other})"),
                    id,
                );
            }
        };
        Ok((s, true))
    }

    fn bspline_surface(&self, id: u64) -> R<BSplineSurface> {
        let f = self.f;
        let inst = f.get(id)?;
        // The parameters after the name, and the knots'.
        let (base, knots): (&[Param], &[Param]) = match inst {
            Instance::Simple(r) => (
                r.params.get(1..8).unwrap_or(&[]),
                r.params.get(8..).unwrap_or(&[]),
            ),
            Instance::Complex(_) => (
                &f.record(id, "B_SPLINE_SURFACE")?.params,
                &f.record(id, "B_SPLINE_SURFACE_WITH_KNOTS")?.params,
            ),
        };
        let at = |l: &'a [Param], i: usize| l.get(i).unwrap_or(&UNSET);
        let du = real(at(base, 0)).unwrap_or(0.0) as usize;
        let dv = real(at(base, 1)).unwrap_or(0.0) as usize;
        let rows = list(at(base, 2));
        let nu = rows.len();
        let nv = rows.first().map_or(0, |r| list(r).len());
        let mut poles = Vec::with_capacity(nu * nv);
        for row in rows {
            let row = list(row);
            if row.len() != nv {
                return bad(
                    "a B-spline surface's rows of poles of different lengths",
                    id,
                );
            }
            for p in row {
                let p = reference(p).map_or_else(|| bad("a pole that is not a point", id), Ok)?;
                poles.push(self.point(p)?);
            }
        }
        let ku = expand_knots(at(knots, 0), at(knots, 2));
        let kv = expand_knots(at(knots, 1), at(knots, 3));
        let weights = inst.record("RATIONAL_B_SPLINE_SURFACE").map(|rw| {
            list(param(rw, 0))
                .iter()
                .flat_map(|row| list(row).iter().filter_map(real))
                .collect::<Vec<f64>>()
        });
        BSplineSurface::new([du, dv], [ku, kv], [nu, nv], poles, weights)
            .map_err(|e| format!("#{id}: {e}"))
    }

    fn bspline_curve(&self, id: u64) -> R<BSplineCurve> {
        let f = self.f;
        let inst = f.get(id)?;
        let (base, knots): (&[Param], &[Param]) = match inst {
            Instance::Simple(r) => (
                r.params.get(1..6).unwrap_or(&[]),
                r.params.get(6..).unwrap_or(&[]),
            ),
            Instance::Complex(_) => (
                &f.record(id, "B_SPLINE_CURVE")?.params,
                &f.record(id, "B_SPLINE_CURVE_WITH_KNOTS")?.params,
            ),
        };
        let at = |l: &'a [Param], i: usize| l.get(i).unwrap_or(&UNSET);
        let degree = real(at(base, 0)).unwrap_or(0.0) as usize;
        let poles = list(at(base, 1))
            .iter()
            .map(|p| {
                reference(p)
                    .map_or_else(|| bad("a pole that is not a point", id), |p| self.point(p))
            })
            .collect::<R<Vec<Point3>>>()?;
        let knots = expand_knots(at(knots, 0), at(knots, 1));
        let weights = inst
            .record("RATIONAL_B_SPLINE_CURVE")
            .map(|rw| list(param(rw, 0)).iter().filter_map(real).collect());
        BSplineCurve::new(degree, knots, poles, weights).map_err(|e| format!("#{id}: {e}"))
    }

    fn polyline(&self, id: u64) -> R<BSplineCurve> {
        let r = self.f.record(id, "POLYLINE")?;
        let points = list(param(r, 1))
            .iter()
            .filter_map(reference)
            .map(|p| self.point(p))
            .collect::<R<Vec<Point3>>>()?;
        let n = points.len();
        if n < 2 {
            return bad("a polyline of one point", id);
        }
        let knots = std::iter::once(0.0)
            .chain((0..n).map(|i| i as f64))
            .chain(std::iter::once((n - 1) as f64))
            .collect();
        BSplineCurve::new(1, knots, points, None).map_err(|e| format!("#{id}: {e}"))
    }

    /// A curve through the trims and the surface curves around it, and whether it runs its
    /// own way there.
    fn profile(&self, id: u64) -> R<Profile> {
        let f = self.f;
        let mut id = id;
        let mut same = true;
        for _ in 0..16 {
            match f.kind(id)? {
                "TRIMMED_CURVE" => {
                    let r = f.record(id, "TRIMMED_CURVE")?;
                    same = same == r.params.get(4).is_none_or(boolean);
                    id = arg_ref(r, 1, id)?;
                }
                "SURFACE_CURVE" | "SEAM_CURVE" | "INTERSECTION_CURVE" | "BOUNDED_SURFACE_CURVE" => {
                    let r = f
                        .main_record(id, 2)
                        .map_or_else(|| bad("a surface curve without its curve", id), Ok)?;
                    id = arg_ref(r, 1, id)?;
                }
                _ => return Ok((id, same)),
            }
        }
        bad("curves nested too deep", id)
    }

    /// A swept curve as a B-spline, run the sweep's way: exactly, a circle or an ellipse
    /// rational of degree two; a line (only ever swept into a hyperboloid) over `range`.
    fn profile_bspline(&self, (id, same): Profile, range: (f64, f64)) -> R<BSplineCurve> {
        let f = self.f;
        let r = f
            .main_record(id, 1)
            .map_or_else(|| bad("an empty curve", id), Ok)?;
        let c = match f.kind(id)? {
            "B_SPLINE_CURVE_WITH_KNOTS" => self.bspline_curve(id)?,
            "LINE" => {
                let p = self.point(arg_ref(r, 1, id)?)?;
                let d = self.vector(arg_ref(r, 2, id)?)?;
                BSplineCurve::bezier(vec![p + d * range.0, p + d * range.1])
                    .map_err(|e| format!("#{id}: {e}"))?
            }
            kind @ ("CIRCLE" | "ELLIPSE") => {
                let (o, z, x) = self.placement(arg_ref(r, 1, id)?)?;
                let a = arg_real(r, 2, id)? * self.units.length;
                let b = if kind == "ELLIPSE" {
                    arg_real(r, 3, id)? * self.units.length
                } else {
                    a
                };
                rational_ellipse(o, x, z.cross(x), a, b)
            }
            "POLYLINE" => self.polyline(id)?,
            other => {
                return bad(
                    &format!("a swept curve the kernel cannot read yet ({other})"),
                    id,
                );
            }
        };
        Ok(if same { c } else { reversed_bspline(&c) })
    }

    /// `SURFACE_OF_LINEAR_EXTRUSION`, `S(u, v) = C(u) + v d`: a plane when it sweeps a
    /// line, a cylinder when a circle along its axis, else the B-spline surface of degree
    /// one along `d`, as far as the face's points `near` need.
    fn extrusion(&self, id: u64, profile: Profile, d: Vec3, near: &[Point3]) -> R<(Surface, bool)> {
        let f = self.f;
        let (curve, same) = profile;
        let r = f
            .main_record(curve, 1)
            .map_or_else(|| bad("an empty curve", curve), Ok)?;
        let dir = d
            .normalized()
            .map_or_else(|| bad("an extrusion of no length", id), Ok)?;
        match f.kind(curve)? {
            "LINE" => {
                let p = self.point(arg_ref(r, 1, curve)?)?;
                let l = self.vector(arg_ref(r, 2, curve)?)?;
                let ld = l
                    .normalized()
                    .map_or_else(|| bad("a line of no length", curve), Ok)?;
                let ld = if same { ld } else { -ld };
                let n = ld
                    .cross(dir)
                    .normalized()
                    .map_or_else(|| bad("a line extruded along itself", id), Ok)?;
                return Ok((
                    Surface::Plane(Plane {
                        origin: p,
                        x_dir: ld,
                        normal: n,
                    }),
                    true,
                ));
            }
            "CIRCLE" => {
                let (o, z, x) = self.placement(arg_ref(r, 1, curve)?)?;
                if z.cross(dir).length() <= 1e-12 {
                    let radius = arg_real(r, 2, curve)? * self.units.length;
                    // S_u × S_v: the circle's tangent across d, out of the axis when d runs
                    // along z and the circle its own way.
                    return Ok((
                        Surface::Revolution(Revolution {
                            axis: Axis {
                                origin: o,
                                direction: z,
                            },
                            x_dir: x,
                            meridian: Meridian::Line { radius, slope: 0.0 },
                            outward: (z.dot(dir) > 0.0) == same,
                        }),
                        true,
                    ));
                }
            }
            _ => {}
        }
        let c = self.profile_bspline(profile, (0.0, 1.0))?;
        // Every point of the face is a point of the curve moved along d: as far each way
        // as the furthest of them from the nearest of the curve's poles, and some more.
        let along = |p: &Point3| p.to_vec().dot(dir);
        let (lo_c, hi_c) = bounds_of(c.poles.iter().map(along));
        let (lo_p, hi_p) = bounds_of(near.iter().map(along));
        if !(lo_p.is_finite() && hi_p.is_finite()) {
            return bad("an extruded surface with no face on it", id);
        }
        let (mut v0, mut v1) = (lo_p - hi_c, hi_p - lo_c);
        let margin = 0.1 * (v1 - v0) + 1e-3;
        v0 -= margin;
        v1 += margin;
        let mut poles = Vec::with_capacity(c.poles.len() * 2);
        for &p in &c.poles {
            poles.push(p + dir * v0);
            poles.push(p + dir * v1);
        }
        let weights = c
            .weights
            .as_ref()
            .map(|w| w.iter().flat_map(|&x| [x, x]).collect());
        let s = BSplineSurface::new(
            [c.degree, 1],
            [c.knots.clone(), vec![0.0, 0.0, 1.0, 1.0]],
            [c.poles.len(), 2],
            poles,
            weights,
        )
        .map_err(|e| format!("#{id}: {e}"))?;
        Ok((spline_or_exact(s), false))
    }

    /// `SURFACE_OF_REVOLUTION`, the profile turned about the axis by `u`: a cylinder, a
    /// cone or a plane for a line in a plane through the axis (or square to it), a sphere
    /// or a torus for a circle in such a plane, else the rational B-spline surface it is.
    fn revolution(
        &self,
        id: u64,
        profile: Profile,
        axis: Axis,
        near: &[Point3],
    ) -> R<(Surface, bool)> {
        let f = self.f;
        let (curve, same) = profile;
        let r = f
            .main_record(curve, 1)
            .map_or_else(|| bad("an empty curve", curve), Ok)?;
        let (o, a) = (axis.origin, axis.direction);
        let scale = near.iter().map(|p| p.distance(o)).fold(1.0, f64::max);
        let tol = 1e-9 * scale;
        let radial = |p: Point3| {
            let q = p - o;
            q - a * q.dot(a)
        };
        let n = near.len().max(1) as f64;
        let mean = near.iter().fold(Vec3::ZERO, |s, p| s + p.to_vec()) * (1.0 / n);
        let centre = Point3::new(mean.x, mean.y, mean.z);
        // Whether S_u × S_v runs along the surface's natural normal at a point of the
        // profile, the profile's tangent there `t`: `u` is the turn, `v` the profile's
        // parameter (ISO 10303-42), so S_u × S_v = (a × (p − o)) × t.
        let natural = |surface: &Revolution, p: Point3, t: Vec3| {
            let t = if same { t } else { -t };
            a.cross(p - o).cross(t).dot(surface.normal_at(p)) > 0.0
        };
        match f.kind(curve)? {
            "LINE" => {
                let p = self.point(arg_ref(r, 1, curve)?)?;
                let ld = self
                    .vector(arg_ref(r, 2, curve)?)?
                    .normalized()
                    .map_or_else(|| bad("a line of no length", curve), Ok)?;
                let q = p - o;
                let zp = q.dot(a);
                if ld.dot(a).abs() <= 1e-12 {
                    // Square to the axis: the plane it turns in.
                    let at = if radial(p).length() > tol { p } else { p + ld };
                    let t = if same { ld } else { -ld };
                    let n = a.cross(at - o).cross(t);
                    let normal = if n.dot(a) >= 0.0 { a } else { -a };
                    return Ok((
                        Surface::Plane(Plane {
                            origin: o + a * zp,
                            x_dir: perpendicular(normal),
                            normal,
                        }),
                        true,
                    ));
                }
                if a.cross(ld).length() <= 1e-12 {
                    let rv = radial(p);
                    let x = rv.normalized().unwrap_or_else(|| perpendicular(a));
                    let mut s = Revolution::cylinder(axis, x, rv.length(), true);
                    s.outward = natural(&s, p, ld);
                    return Ok((Surface::Revolution(s), true));
                }
                if q.dot(a.cross(ld)).abs() <= tol {
                    // A cone, on the nappe the face is on.
                    let e = radial(p)
                        .normalized()
                        .or_else(|| (ld - a * ld.dot(a)).normalized())
                        .unwrap_or_else(|| perpendicular(a));
                    let slope = ld.dot(e) / ld.dot(a);
                    let z_apex = zp - q.dot(e) / slope;
                    let zc = (centre - o).dot(a);
                    let slope = if zc >= z_apex {
                        slope.abs()
                    } else {
                        -slope.abs()
                    };
                    let mut s = Revolution {
                        axis: Axis {
                            origin: o + a * zc,
                            direction: a,
                        },
                        x_dir: radial(centre).normalized().unwrap_or(e),
                        meridian: Meridian::Line {
                            radius: slope.abs() * (zc - z_apex).abs(),
                            slope,
                        },
                        outward: true,
                    };
                    let at = p + ld * ((zc - zp) / ld.dot(a));
                    s.outward = natural(&s, at, ld);
                    return Ok((Surface::Revolution(s), true));
                }
                // A hyperboloid: the line as far along as the face's heights need.
                let (lo, hi) = bounds_of(near.iter().map(|h| ((*h - o).dot(a) - zp) / ld.dot(a)));
                let m = 0.1 * (hi - lo) + 1e-3;
                let c = self.profile_bspline(profile, (lo - m, hi + m))?;
                let s = revolved(&c, axis).map_err(|e| format!("#{id}: {e}"))?;
                return Ok((spline_or_exact(s).reversed(), false));
            }
            "CIRCLE" => {
                let (c, n, x) = self.placement(arg_ref(r, 1, curve)?)?;
                let rr = arg_real(r, 2, curve)? * self.units.length;
                let rc = radial(c);
                let major = rc.length();
                if n.dot(a).abs() <= 1e-9 && (c - o).dot(n).abs() <= tol {
                    let meridian = Meridian::Circle {
                        center: if major <= tol { 0.0 } else { major },
                        radius: rr,
                    };
                    let mut s = Revolution {
                        axis: Axis {
                            origin: o + a * (c - o).dot(a),
                            direction: a,
                        },
                        x_dir: rc.normalized().unwrap_or_else(|| perpendicular(a)),
                        meridian,
                        outward: true,
                    };
                    // The circle's point furthest from the axis, and its tangent there.
                    let y = n.cross(x);
                    let (p, t) = (0..8)
                        .map(|k| {
                            let u = f64::from(k) * TAU / 8.0;
                            (
                                c + (x * u.cos() + y * u.sin()) * rr,
                                y * u.cos() - x * u.sin(),
                            )
                        })
                        .max_by(|p, q| radial(p.0).length().total_cmp(&radial(q.0).length()))
                        .unwrap_or((c + x * rr, y));
                    s.outward = natural(&s, p, t);
                    return Ok((Surface::Revolution(s), true));
                }
            }
            _ => {}
        }
        let c = self.profile_bspline(profile, (0.0, 1.0))?;
        let s = revolved(&c, axis).map_err(|e| format!("#{id}: {e}"))?;
        Ok((spline_or_exact(s).reversed(), true))
    }

    /// An edge's curve, run from its start the curve's way when `same`.
    fn curve(&self, id: u64, same: bool) -> R<Curve> {
        let (id, agree) = self.profile(id)?;
        let same = same == agree;
        let f = self.f;
        let r = f
            .main_record(id, 1)
            .map_or_else(|| bad("an empty curve", id), Ok)?;
        match f.kind(id)? {
            "LINE" => Ok(Curve::Line),
            "POLYLINE" => Ok(Curve::Spline(SplineCurve {
                up: same,
                ..SplineCurve::new(self.polyline(id)?)
            })),
            "CIRCLE" => {
                let (o, z, _) = self.placement(arg_ref(r, 1, id)?)?;
                let radius = arg_real(r, 2, id)? * self.units.length;
                Ok(Curve::Circle(Circle {
                    center: o,
                    normal: if same { z } else { -z },
                    radius,
                }))
            }
            "ELLIPSE" => {
                let (o, z, x) = self.placement(arg_ref(r, 1, id)?)?;
                let a = arg_real(r, 2, id)? * self.units.length;
                let b = arg_real(r, 3, id)? * self.units.length;
                Ok(ellipse(o, z, x, a, b, same))
            }
            "B_SPLINE_CURVE_WITH_KNOTS" => {
                let c = self.bspline_curve(id)?;
                Ok(match exact::curve(&c) {
                    Some(ExactCurve::Line) => Curve::Line,
                    Some(ExactCurve::Circle(circle)) => Curve::Circle(Circle {
                        normal: if same { circle.normal } else { -circle.normal },
                        ..circle
                    }),
                    None => Curve::Spline(SplineCurve {
                        up: same,
                        ..SplineCurve::new(c)
                    }),
                })
            }
            other => bad(&format!("a curve the kernel cannot read yet ({other})"), id),
        }
    }
}

/// A B-spline surface as the plane, cylinder, cone, sphere or torus it is exactly, else as
/// itself; its normal along `S_u × S_v`.
fn spline_or_exact(s: BSplineSurface) -> Surface {
    exact::surface(&s).unwrap_or_else(|| Surface::Spline(SplineSurface::new(s, true)))
}

/// The least and the greatest of some numbers.
fn bounds_of(values: impl Iterator<Item = f64>) -> (f64, f64) {
    values.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| {
        (lo.min(x), hi.max(x))
    })
}

/// Knots from their multiplicities and values.
fn expand_knots(mults: &Param, values: &Param) -> Vec<f64> {
    let mut out = Vec::new();
    for (m, v) in list(mults).iter().zip(list(values)) {
        if let (Some(m), Some(v)) = (real(m), real(v)) {
            out.extend(std::iter::repeat_n(v, m.max(0.0) as usize));
        }
    }
    out
}

/// The same curve with its parameter run the other way, over the same domain.
fn reversed_bspline(c: &BSplineCurve) -> BSplineCurve {
    let (a, b) = (c.knots[0], c.knots[c.knots.len() - 1]);
    BSplineCurve {
        degree: c.degree,
        knots: c.knots.iter().rev().map(|k| a + b - k).collect(),
        poles: c.poles.iter().rev().copied().collect(),
        weights: c
            .weights
            .as_ref()
            .map(|w| w.iter().rev().copied().collect()),
    }
}

/// The rational quadratic B-spline of a whole ellipse about `o`, its axes `x` (semi-axis
/// `a`) and `y` (`b`): four quarters, counter-clockwise about `x × y` from `x`.
fn rational_ellipse(o: Point3, x: Vec3, y: Vec3, a: f64, b: f64) -> BSplineCurve {
    let w = std::f64::consts::FRAC_1_SQRT_2;
    let corners = [
        (1.0, 0.0),
        (1.0, 1.0),
        (0.0, 1.0),
        (-1.0, 1.0),
        (-1.0, 0.0),
        (-1.0, -1.0),
        (0.0, -1.0),
        (1.0, -1.0),
        (1.0, 0.0),
    ];
    BSplineCurve {
        degree: 2,
        knots: vec![
            0.0, 0.0, 0.0, 0.25, 0.25, 0.5, 0.5, 0.75, 0.75, 1.0, 1.0, 1.0,
        ],
        poles: corners
            .iter()
            .map(|&(u, v)| o + x * (a * u) + y * (b * v))
            .collect(),
        weights: Some((0..9).map(|i| if i % 2 == 1 { w } else { 1.0 }).collect()),
    }
}

/// The rational B-spline surface of `c` turned once about `axis`: `u` along the curve, `v`
/// the turn, counter-clockwise about the axis (a surface of revolution's parameters the
/// other way round, its normal the other way). Each pole is carried round on a circle of
/// its own from where it is, which turns the whole curve, a turn being linear.
fn revolved(c: &BSplineCurve, axis: Axis) -> Result<BSplineSurface, String> {
    let (o, a) = (axis.origin, axis.direction);
    let circle = rational_ellipse(Point3::ORIGIN, Vec3::X, Vec3::Y, 1.0, 1.0);
    let (nu, nv) = (c.poles.len(), circle.poles.len());
    let cw = c.weights.clone().unwrap_or_else(|| vec![1.0; nu]);
    let qw = circle.weights.clone().unwrap_or_else(|| vec![1.0; nv]);
    let mut poles = Vec::with_capacity(nu * nv);
    let mut weights = Vec::with_capacity(nu * nv);
    for (&p, &w) in c.poles.iter().zip(&cw) {
        let d = p - o;
        let h = d.dot(a);
        let radial = d - a * h;
        let rho = radial.length();
        let x = radial.normalized().unwrap_or_else(|| perpendicular(a));
        let y = a.cross(x);
        for (q, &wq) in circle.poles.iter().zip(&qw) {
            poles.push(o + a * h + (x * q.x + y * q.y) * rho);
            weights.push(w * wq);
        }
    }
    BSplineSurface::new(
        [c.degree, circle.degree],
        [c.knots.clone(), circle.knots.clone()],
        [nu, nv],
        poles,
        Some(weights),
    )
}

/// An ellipse as the kernel holds one: the section of a cylinder of radius `b` by the
/// ellipse's plane, the cylinder's axis through its centre leaning towards its major axis
/// so that its shadow there is `a` long. Run counter-clockwise about `z` when `same`.
fn ellipse(o: Point3, z: Vec3, x: Vec3, a: f64, b: f64, same: bool) -> Curve {
    if (a - b).abs() <= 1e-12 * a.max(1.0) {
        return Curve::Circle(Circle {
            center: o,
            normal: if same { z } else { -z },
            radius: a,
        });
    }
    let (a, b, x) = if b > a { (b, a, z.cross(x)) } else { (a, b, x) };
    let cos = b / a;
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    let d = (z * cos + x * sin).normalized().unwrap_or(z);
    let carrier = Revolution::cylinder(
        Axis {
            origin: o,
            direction: d,
        },
        z.cross(x),
        b,
        true,
    );
    Curve::Section(Section {
        carrier,
        other: Surface::Plane(Plane {
            origin: o,
            x_dir: x,
            normal: z,
        }),
        branch: 0,
        ccw: same,
    })
}

// ---------------------------------------------------------------------------------------
// Solids.

/// A face's loops as the file gives them, until its solid is read and they are settled:
/// each loop's edge uses and whether the file calls it the outer one, the vertices of its
/// vertex loops, and whether its shell is turned round.
#[derive(Debug, Clone, Default)]
struct RawFace {
    loops: Vec<(Vec<EdgeUse>, bool)>,
    lone: Vec<u32>,
    turn: bool,
}

struct SolidReader<'a, 'g> {
    g: &'g mut Geometry<'a>,
    solid: Solid,
    vertices: HashMap<u64, u32>,
    /// Each edge read, `None` for one of no length.
    edges: HashMap<u64, Option<u32>>,
    raw: Vec<RawFace>,
    /// Each face's instance.
    ids: Vec<u64>,
    /// The edges split where a seam was put, into their two pieces.
    splits: HashMap<u32, (u32, u32)>,
}

impl SolidReader<'_, '_> {
    fn vertex(&mut self, id: u64) -> R<u32> {
        if let Some(&v) = self.vertices.get(&id) {
            return Ok(v);
        }
        let r = self.g.f.record(id, "VERTEX_POINT")?;
        let p = self.g.point(arg_ref(r, 1, id)?)?;
        self.solid.vertices.push(Vertex::new(p));
        let v = self.solid.vertices.len() as u32 - 1;
        self.vertices.insert(id, v);
        Ok(v)
    }

    fn edge(&mut self, id: u64) -> R<Option<u32>> {
        if let Some(&e) = self.edges.get(&id) {
            return Ok(e);
        }
        let f = self.g.f;
        let r = f.record(id, "EDGE_CURVE")?;
        let start = self.vertex(arg_ref(r, 1, id)?)?;
        let end = self.vertex(arg_ref(r, 2, id)?)?;
        let same = r.params.get(4).is_none_or(boolean);
        let curve = self.g.curve(arg_ref(r, 3, id)?, same)?;
        let edge = Edge::new(start, end, curve);
        let out = if start == end && self.no_length(&edge) {
            None
        } else {
            self.solid.edges.push(edge);
            Some(self.solid.edges.len() as u32 - 1)
        };
        self.edges.insert(id, out);
        Ok(out)
    }

    fn no_length(&self, edge: &Edge) -> bool {
        let p = self.solid.point(edge.start);
        if matches!(edge.curve, Curve::Line) {
            return true;
        }
        let extent = edge.curve.extent(p, p, true);
        (0..=8).all(|k| {
            edge.curve
                .at(p, p, extent * f64::from(k) / 8.0)
                .is_none_or(|q| q.distance(p) <= NO_LENGTH)
        })
    }

    /// A bound's edge uses the way the bound runs, or for a vertex loop (or a loop of
    /// edges of no length) its vertex; and whether it is the face's outer bound.
    fn bound(&mut self, id: u64) -> R<(Vec<EdgeUse>, Option<u32>, bool)> {
        let f = self.g.f;
        let inst = f.get(id)?;
        let outer = inst.is("FACE_OUTER_BOUND");
        let r = inst
            .record("FACE_OUTER_BOUND")
            .or_else(|| inst.record("FACE_BOUND"))
            .map_or_else(|| bad("not a face bound", id), Ok)?;
        let lp = arg_ref(r, 1, id)?;
        let orientation = r.params.get(2).is_none_or(boolean);
        let mut uses = Vec::new();
        let mut point = None;
        match f.kind(lp)? {
            "EDGE_LOOP" => {
                let l = f.record(lp, "EDGE_LOOP")?;
                for oe in list(param(l, 1)) {
                    let mut oe = reference(oe).map_or_else(|| bad("a loop of no edges", lp), Ok)?;
                    let mut forward = true;
                    // An oriented edge of an oriented edge, as the schema allows.
                    for _ in 0..8 {
                        let o = f.record(oe, "ORIENTED_EDGE")?;
                        forward = forward == o.params.get(4).is_none_or(boolean);
                        let element = arg_ref(o, 3, oe)?;
                        if f.get(element)?.is("ORIENTED_EDGE") {
                            oe = element;
                            continue;
                        }
                        match self.edge(element)? {
                            Some(e) => uses.push(EdgeUse {
                                edge: e,
                                reversed: !forward,
                            }),
                            None => {
                                let ec = f.record(element, "EDGE_CURVE")?;
                                point = Some(self.vertex(arg_ref(ec, 1, element)?)?);
                            }
                        }
                        break;
                    }
                }
            }
            "VERTEX_LOOP" => {
                let l = f.record(lp, "VERTEX_LOOP")?;
                return Ok((Vec::new(), Some(self.vertex(arg_ref(l, 1, lp)?)?), outer));
            }
            "POLY_LOOP" => {
                let l = f.record(lp, "POLY_LOOP")?;
                let points = list(param(l, 1))
                    .iter()
                    .filter_map(reference)
                    .map(|p| self.g.point(p))
                    .collect::<R<Vec<Point3>>>()?;
                let first = self.solid.vertices.len() as u32;
                self.solid
                    .vertices
                    .extend(points.iter().map(|&p| Vertex::new(p)));
                let n = points.len() as u32;
                for k in 0..n {
                    self.solid
                        .edges
                        .push(Edge::line(first + k, first + (k + 1) % n));
                    uses.push(EdgeUse::forward(self.solid.edges.len() as u32 - 1));
                }
            }
            other => return bad(&format!("a loop the kernel cannot read ({other})"), lp),
        }
        if uses.is_empty() {
            return Ok((Vec::new(), point, outer));
        }
        if !orientation {
            uses = turned(&uses);
        }
        Ok((uses, None, outer))
    }

    /// A face as read: its surface, turned by its sense, and its raw loops.
    fn face(&mut self, id: u64, turn: bool) -> R<()> {
        let f = self.g.f;
        let inst = f.get(id)?;
        let r = inst
            .record("ADVANCED_FACE")
            .or_else(|| inst.record("FACE_SURFACE"))
            .map_or_else(|| bad("not a face", id), Ok)?;
        let surface_id = arg_ref(r, 2, id)?;
        let same = r.params.get(3).is_none_or(boolean);
        let mut raw = RawFace {
            turn,
            ..RawFace::default()
        };
        for b in list(param(r, 1)).iter().filter_map(reference) {
            let (uses, vertex, outer) = self.bound(b)?;
            if let Some(v) = vertex {
                raw.lone.push(v);
            } else if !uses.is_empty() {
                raw.loops.push((uses, outer));
            }
        }
        // The face's points, which a swept surface made a B-spline must reach.
        let mut near: Vec<Point3> = raw.lone.iter().map(|&v| self.solid.point(v)).collect();
        for &u in raw.loops.iter().flat_map(|l| &l.0) {
            let e = &self.solid.edges[u.edge as usize];
            let (a, b) = (self.solid.point(e.start), self.solid.point(e.end));
            let extent = e.curve.extent(a, b, e.is_closed());
            for k in 0..4 {
                near.push(e.curve.at(a, b, extent * f64::from(k) / 4.0).unwrap_or(a));
            }
        }
        let surface = self.g.surface(surface_id, &near)?;
        let surface = if same { surface } else { surface.reversed() };
        self.solid.faces.push(Face {
            surface,
            loops: Vec::new(),
        });
        self.raw.push(raw);
        self.ids.push(id);
        Ok(())
    }

    /// A shell's faces read, turned round when it is; their indices.
    fn shell(&mut self, id: u64, turn: bool) -> R<Vec<u32>> {
        let f = self.g.f;
        let inst = f.get(id)?;
        if let Some(o) = inst
            .record("ORIENTED_CLOSED_SHELL")
            .or_else(|| inst.record("ORIENTED_OPEN_SHELL"))
        {
            let inner = arg_ref(o, 2, id)?;
            let flip = o.params.get(3).is_some_and(|p| !boolean(p));
            return self.shell(inner, turn != flip);
        }
        let r = inst
            .record("CLOSED_SHELL")
            .or_else(|| inst.record("OPEN_SHELL"))
            .or_else(|| inst.record("CONNECTED_FACE_SET"))
            .map_or_else(|| bad("not a shell", id), Ok)?;
        let mut out = Vec::new();
        for fc in list(param(r, 1)).iter().filter_map(reference) {
            out.push(self.solid.faces.len() as u32);
            self.face(fc, turn)?;
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------------------
    // Settling the faces.

    /// A face's loops settled: its seams and poles put in, its outer loop first.
    fn settle(&mut self, k: usize) -> R<()> {
        let raw = std::mem::take(&mut self.raw[k]);
        let mut surface = self.solid.faces[k].surface;
        let mut loops: Vec<(Vec<EdgeUse>, bool)> = raw
            .loops
            .iter()
            .map(|(l, outer)| (self.current(l), *outer))
            .collect();
        if raw.turn {
            surface = surface.reversed();
            for l in &mut loops {
                l.0 = turned(&l.0);
            }
        }
        let surface = match surface {
            Surface::Revolution(rev) => {
                let rev = self.sphere_axis(&rev, &loops, &raw.lone);
                self.straighten(&rev, &loops);
                Surface::Revolution(rev)
            }
            other => other,
        };
        self.solid.faces[k].surface = surface;
        let mut loops = match surface {
            Surface::Spline(ss) => {
                self.straighten_lines(&loops);
                self.spline_seams(k, &ss, loops)?
            }
            Surface::Revolution(rev) => {
                // The poles' edges first (a loop with its seam but not them seems to go round
                // the axis), the seams, then the poles the seams reach.
                for l in &mut loops {
                    l.0 = self.poles(&rev, &l.0);
                }
                let mut loops = self.seams(k, &rev, loops, &raw.lone)?;
                for l in &mut loops {
                    l.0 = self.poles(&rev, &l.0);
                }
                loops
            }
            Surface::Plane(_) => loops,
        };
        if loops.is_empty() {
            return bad("a face without a boundary", self.ids[k]);
        }
        // The outer loop first: the widest (its box holds the holes'), the one the file
        // calls the outer bound (or the seams made) among loops as wide. Some writers call
        // a hole the outer bound.
        let spans: Vec<f64> = loops.iter().map(|l| self.span(&l.0)).collect();
        let widest = spans.iter().copied().fold(0.0, f64::max);
        let first = (0..loops.len())
            .filter(|&i| spans[i] >= widest * (1.0 - 1e-9))
            .max_by_key(|&i| loops[i].1)
            .unwrap_or(0);
        loops.swap(0, first);
        self.solid.faces[k].loops = loops.into_iter().map(|l| l.0).collect();
        Ok(())
    }

    /// Points along a loop's edges: their ends and seven between.
    fn samples(&self, uses: &[EdgeUse]) -> Vec<Point3> {
        let mut out = Vec::with_capacity(uses.len() * 8);
        for &u in uses {
            let e = &self.solid.edges[u.edge as usize];
            let (a, b) = (self.solid.point(e.start), self.solid.point(e.end));
            let extent = e.curve.extent(a, b, e.is_closed());
            for k in 0..8 {
                out.push(e.curve.at(a, b, extent * f64::from(k) / 8.0).unwrap_or(a));
            }
        }
        out
    }

    /// A sphere's axis turned to keep its poles clear of the face's boundary, which would
    /// otherwise turn half round about it in passing, or close by (a box's corner rounded,
    /// its arcs through a pole or near one): the axis that leaves them furthest from every
    /// point of the loops, unless the loops have a vertex at a pole already (where the
    /// file means it to be).
    fn sphere_axis(
        &self,
        rev: &Revolution,
        loops: &[(Vec<EdgeUse>, bool)],
        lone: &[u32],
    ) -> Revolution {
        let Meridian::Circle { center, radius } = rev.meridian else {
            return *rev;
        };
        if center != 0.0 {
            return *rev;
        }
        let c = rev.axis.origin;
        let points: Vec<Point3> = loops.iter().flat_map(|l| self.samples(&l.0)).collect();
        if points.is_empty() {
            return *rev;
        }
        let at_pole = |a: Vec3| {
            let tol = 1e-6 * radius.max(1.0);
            let poles = [c + a * radius, c - a * radius];
            loops
                .iter()
                .flat_map(|l| l.0.iter().map(|&u| self.solid.start_of(u)))
                .chain(lone.iter().copied())
                .any(|v| poles.iter().any(|p| p.distance(self.solid.point(v)) <= tol))
        };
        // How near the poles come to the loops: the least of `1 - |cos|` of the angle
        // between an axis and the way to a point of the loops.
        let clearance = |a: Vec3| {
            points
                .iter()
                .map(|&p| 1.0 - ((p - c).dot(a) / radius).abs().min(1.0))
                .fold(f64::INFINITY, f64::min)
        };
        let own = rev.axis.direction;
        if at_pole(own) {
            return *rev;
        }
        let mean =
            points.iter().fold(Vec3::ZERO, |s, &p| s + (p - c)) * (1.0 / points.len() as f64);
        let mut candidates = vec![own, Vec3::X, Vec3::Y, Vec3::Z];
        if let Some(m) = mean.normalized() {
            let e1 = perpendicular(m);
            let e2 = m.cross(e1);
            candidates.push(m);
            for k in 0..8 {
                let t = f64::from(k) * PI / 8.0;
                candidates.push(e1 * t.cos() + e2 * t.sin());
            }
        }
        let best = candidates
            .into_iter()
            .max_by(|a, b| clearance(*a).total_cmp(&clearance(*b)))
            .unwrap_or(own);
        Revolution {
            axis: Axis {
                origin: c,
                direction: best,
            },
            x_dir: perpendicular(best),
            ..*rev
        }
    }

    /// The straight edges of a face on a surface of revolution that are not its straight
    /// lines, made B-splines of degree one (the same points) for the walk round the face
    /// to follow: a line across a wide cone that the file says is straight where it bends
    /// less than the file's accuracy; any line on a sphere or a torus.
    fn straighten(&mut self, rev: &Revolution, loops: &[(Vec<EdgeUse>, bool)]) {
        let ruled = matches!(rev.meridian, Meridian::Line { .. });
        let tol = 1e-6 * rev.size().max(1.0);
        for &u in loops.iter().flat_map(|l| &l.0) {
            let e = self.solid.edges[u.edge as usize];
            if !matches!(e.curve, Curve::Line) {
                continue;
            }
            let (a, b) = (self.solid.point(e.start), self.solid.point(e.end));
            let along = ruled && {
                let (ra, rb) = (rev.rho_z(a).0, rev.rho_z(b).0);
                ra <= NEAR_AXIS
                    || rb <= NEAR_AXIS
                    || angle_between(rev.params(a).0, rev.params(b).0) * ra.max(rb) <= tol
            };
            if !along && let Ok(c) = BSplineCurve::bezier(vec![a, b]) {
                self.solid.edges[u.edge as usize].curve = Curve::Spline(SplineCurve::new(c));
            }
        }
    }

    /// Every straight edge of a face on a B-spline surface made a B-spline of degree one
    /// (the same points), which the kernel's layout of such a face follows.
    fn straighten_lines(&mut self, loops: &[(Vec<EdgeUse>, bool)]) {
        for &u in loops.iter().flat_map(|l| &l.0) {
            let e = self.solid.edges[u.edge as usize];
            if matches!(e.curve, Curve::Line) {
                let (a, b) = (self.solid.point(e.start), self.solid.point(e.end));
                if let Ok(c) = BSplineCurve::bezier(vec![a, b]) {
                    self.solid.edges[u.edge as usize].curve = Curve::Spline(SplineCurve::new(c));
                }
            }
        }
    }

    /// A loop on a B-spline surface laid out in its `(u, v)` plane from its first vertex,
    /// unwrapped where the surface closes, as the kernel lays its faces out: each use's
    /// points, how far along it each is.
    fn uv_path(&self, ss: &SplineSurface, uses: &[EdgeUse]) -> Vec<Vec<(f64, f64, f64)>> {
        let mut out = Vec::with_capacity(uses.len());
        let mut near: Option<(f64, f64)> = None;
        for &u in uses {
            let walk = self.solid.walk_edge(u);
            let closed = self.solid.edges[u.edge as usize].is_closed();
            let extent = walk.curve.extent(walk.a, walk.b, closed);
            let mut samples = Vec::with_capacity(33);
            for i in 0..=32 {
                let s = extent * f64::from(i) / 32.0;
                let p = walk.curve.at(walk.a, walk.b, s).unwrap_or(walk.a);
                let uv = match near {
                    Some(prev) => crate::spline_face::uv_near(ss, p, prev),
                    None => ss.params(p),
                };
                samples.push((s, uv.0, uv.1));
                near = Some(uv);
            }
            out.push(samples);
        }
        out
    }

    /// The loops of a face on a B-spline surface that closes round on itself, with the
    /// seam it needs between two loops that go round it (a tube's two ends): along the
    /// curve of constant `u` (or `v`) where both loops have a vertex, else where the first
    /// has one, the other's edge split there.
    fn spline_seams(
        &mut self,
        k: usize,
        ss: &SplineSurface,
        loops: Vec<(Vec<EdgeUse>, bool)>,
    ) -> R<Vec<(Vec<EdgeUse>, bool)>> {
        let id = self.ids[k];
        let period = |d: usize| {
            ss.data.closed[d].then(|| {
                let (a, b) = ss.domain(d);
                b - a
            })
        };
        if period(0).is_none() && period(1).is_none() {
            return Ok(loops);
        }
        let s = if ss.outward { 1.0 } else { -1.0 };
        let mut round: [Vec<(usize, f64)>; 2] = [Vec::new(), Vec::new()];
        let mut others = Vec::new();
        for (i, l) in loops.iter().enumerate() {
            let path = self.uv_path(ss, &l.0);
            let (first, last) = (path[0][0], path[path.len() - 1][32]);
            let w = [last.1 - first.1, last.2 - first.2];
            match (0..2).find(|&d| period(d).is_some_and(|p| w[d].abs() > 0.5 * p)) {
                Some(d) => round[d].push((i, w[d])),
                None => others.push(i),
            }
        }
        let mut out = Vec::new();
        match (round[0].len(), round[1].len()) {
            (0, 0) => {}
            (2, 0) | (0, 2) => {
                // Along `u` round the surface: the seam runs along `v`, and the other way.
                let d = if round[0].len() == 2 { 0 } else { 1 };
                let (ia, wa) = round[d][0];
                let ib = round[d][1].0;
                let c = self.spline_seam_at(ss, &loops[ia].0, &loops[ib].0, d);
                let a = self.spline_vertex_at(ss, &loops[ia].0, d, c, id)?;
                let b = self.spline_vertex_at(ss, &loops[ib].0, d, c, id)?;
                // From the first loop into the face: the face lies on the left of a loop
                // that runs its surface's way round, more `v` of one along `u`, less `u` of
                // one along `v`.
                let toward = (wa * s > 0.0) == (d == 0);
                let mut seam = ss.iso(1 - d, c);
                seam.up = toward;
                self.solid.edges.push(Edge::new(a, b, Curve::Spline(seam)));
                let e = self.solid.edges.len() as u32 - 1;
                let la = self.current(&loops[ia].0);
                let lb = self.current(&loops[ib].0);
                let mut l = rotated_to(&self.solid, &la, a, id)?;
                l.push(EdgeUse::forward(e));
                l.extend(rotated_to(&self.solid, &lb, b, id)?);
                l.push(EdgeUse::backward(e));
                out.push((l, true));
            }
            _ => {
                return bad(
                    "a face round a pole of a B-spline surface, or round it more ways than one, which the kernel cannot read yet",
                    id,
                );
            }
        }
        let merged = !out.is_empty();
        for i in others {
            out.push((self.current(&loops[i].0), loops[i].1 && !merged));
        }
        Ok(out)
    }

    /// Where a seam along `1 - d` goes between two loops round a closed B-spline surface:
    /// the parameter `d` of a vertex both have, else of the first's first vertex.
    fn spline_seam_at(&self, ss: &SplineSurface, la: &[EdgeUse], lb: &[EdgeUse], d: usize) -> f64 {
        let (lo, hi) = ss.domain(d);
        let at = |uses: &[EdgeUse]| -> Vec<f64> {
            uses.iter()
                .map(|&u| {
                    let (pu, pv) = ss.params(self.solid.point(self.solid.start_of(u)));
                    if d == 0 { pu } else { pv }
                })
                .collect()
        };
        let (ta, tb) = (at(la), at(lb));
        let tol = 1e-9 * (hi - lo);
        let same = |x: f64, y: f64| {
            let p = hi - lo;
            let r = (x - y).rem_euclid(p);
            r.min(p - r) <= tol
        };
        ta.iter()
            .copied()
            .find(|&x| tb.iter().any(|&y| same(x, y)))
            .or_else(|| ta.first().copied())
            .unwrap_or(lo)
    }

    /// The vertex of a loop round a closed B-spline surface where its parameter `d` is `c`,
    /// the loop's edge that crosses there split when it has none.
    fn spline_vertex_at(
        &mut self,
        ss: &SplineSurface,
        uses: &[EdgeUse],
        d: usize,
        c: f64,
        id: u64,
    ) -> R<u32> {
        let uses = self.current(uses);
        let (lo, hi) = ss.domain(d);
        let period = hi - lo;
        let tol = 1e-9 * period;
        let pick = |q: (f64, f64)| if d == 0 { q.0 } else { q.1 };
        for &u in &uses {
            let v = self.solid.start_of(u);
            let r = (pick(ss.params(self.solid.point(v))) - c).rem_euclid(period);
            if r.min(period - r) <= tol {
                return Ok(v);
            }
        }
        let path = self.uv_path(ss, &uses);
        for (k, samples) in path.iter().enumerate() {
            let u = uses[k];
            let walk = self.solid.walk_edge(u);
            for w in samples.windows(2) {
                let (x0, x1) = (
                    if d == 0 { w[0].1 } else { w[0].2 },
                    if d == 0 { w[1].1 } else { w[1].2 },
                );
                let target = c + period * ((x0.min(x1) - c) / period).ceil();
                if x0 == x1 || target > x0.max(x1) {
                    continue;
                }
                let rising = x1 > x0;
                let (mut s0, mut s1) = (w[0].0, w[1].0);
                let near0 = (w[0].1, w[0].2);
                for _ in 0..60 {
                    let m = 0.5 * (s0 + s1);
                    let p = walk.curve.at(walk.a, walk.b, m).unwrap_or(walk.a);
                    let uv = crate::spline_face::uv_near(ss, p, near0);
                    if (pick(uv) < target) == rising {
                        s0 = m;
                    } else {
                        s1 = m;
                    }
                }
                let p = walk
                    .curve
                    .at(walk.a, walk.b, 0.5 * (s0 + s1))
                    .unwrap_or(walk.a);
                let close = 1e-7 * ss.scale().max(1.0);
                for v in [self.solid.start_of(u), self.solid.end_of(u)] {
                    if self.solid.point(v).distance(p) <= close {
                        return Ok(v);
                    }
                }
                return Ok(self.split(u.edge, p));
            }
        }
        bad("a loop that never reaches where its seam goes", id)
    }

    /// How wide a loop is: the diagonal of the box round its edges' points.
    fn span(&self, uses: &[EdgeUse]) -> f64 {
        let mut lo = Vec3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
        let mut hi = Vec3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for u in uses {
            let e = &self.solid.edges[u.edge as usize];
            let (a, b) = (self.solid.point(e.start), self.solid.point(e.end));
            let extent = e.curve.extent(a, b, e.is_closed());
            for k in 0..=8 {
                let p = e.curve.at(a, b, extent * f64::from(k) / 8.0).unwrap_or(a);
                lo = Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
                hi = Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
            }
        }
        (hi - lo).length()
    }

    /// A loop on a surface of revolution followed in the surface's `(θ, v)` plane from the
    /// first of its vertices off the axis, `θ` (and `v` round a torus's tube) unwrapped.
    fn param_points(&self, rev: &Revolution, uses: &[EdgeUse]) -> R<Vec<(f64, f64)>> {
        let n = uses.len();
        let off = |u: EdgeUse| rev.rho_z(self.solid.point(self.solid.start_of(u))).0 > NEAR_AXIS;
        let first = (0..n).find(|&i| off(uses[i])).unwrap_or(0);
        let start = self.solid.point(self.solid.start_of(uses[first]));
        let mut at = rev.params(start);
        let mut points = vec![at];
        for k in 0..n {
            let path = self.follow(rev, uses[(first + k) % n], at)?;
            points.extend_from_slice(path.get(1..).unwrap_or(&[]));
            at = path.last().copied().unwrap_or(at);
        }
        Ok(points)
    }

    /// An edge use followed in `(θ, v)` from `at`, as [`ParamPath::follow`] does, a
    /// degenerate edge turning `θ` as it says.
    fn follow(&self, rev: &Revolution, u: EdgeUse, at: (f64, f64)) -> R<Vec<(f64, f64)>> {
        let walk = self.solid.walk_edge(u);
        if let Curve::Degenerate { axis, turn } = walk.curve {
            let t = if axis.dot(rev.axis.direction) > 0.0 {
                turn
            } else {
                -turn
            };
            return Ok((0..=8)
                .map(|i| (at.0 + t * f64::from(i) / 8.0, at.1))
                .collect());
        }
        Ok(ParamPath::follow(rev, &walk, at, NEAR_AXIS)?
            .points()
            .to_vec())
    }

    /// The signed area a loop bounds in the `(θ, v)` plane, counter-clockwise positive.
    fn param_area(&self, rev: &Revolution, uses: &[EdgeUse]) -> R<f64> {
        let p = self.param_points(rev, uses)?;
        let n = p.len();
        Ok((0..n)
            .map(|i| {
                let (a, b) = (p[i], p[(i + 1) % n]);
                a.0 * b.1 - b.0 * a.1
            })
            .sum::<f64>()
            / 2.0)
    }

    /// The loops of a face on a surface of revolution with the seams it needs: up a
    /// straight line or a meridian (constant `θ`) between two loops that go round the axis,
    /// or from one to the pole the face reaches; along a parallel (constant `v`) between
    /// two loops that go round a torus's tube; a whole sphere's seam and poles for a face
    /// bounded by a vertex or by holes alone.
    fn seams(
        &mut self,
        k: usize,
        rev: &Revolution,
        loops: Vec<(Vec<EdgeUse>, bool)>,
        lone: &[u32],
    ) -> R<Vec<(Vec<EdgeUse>, bool)>> {
        let id = self.ids[k];
        let s = if rev.outward { 1.0 } else { -1.0 };
        let mut round_axis = Vec::new();
        let mut round_tube = Vec::new();
        let mut others = Vec::new();
        let mut holes: Vec<(f64, f64)> = Vec::new();
        for (i, l) in loops.iter().enumerate() {
            let p = self.param_points(rev, &l.0)?;
            let (first, last) = (p[0], p[p.len() - 1]);
            if (last.0 - first.0).abs() > PI {
                round_axis.push((i, last.0 - first.0));
            } else if rev.v_periodic() && (last.1 - first.1).abs() > PI {
                round_tube.push((i, last.1 - first.1));
            } else {
                others.push(i);
                holes.push(bounds_of(p.iter().map(|q| q.0)));
            }
        }
        let sphere = matches!(rev.meridian, Meridian::Circle { center, .. } if center == 0.0);
        let mut out: Vec<(Vec<EdgeUse>, bool)> = Vec::new();
        match (round_axis.len(), round_tube.len()) {
            (0, 0) => {
                // A whole sphere: bounded by a vertex, or by its holes alone (each running
                // clockwise seen from outside).
                let mut all_holes = !others.is_empty();
                for &i in &others {
                    all_holes &= self.param_area(rev, &loops[i].0)? * s < 0.0;
                }
                if sphere && (others.is_empty() || all_holes) {
                    if others.is_empty() && lone.is_empty() {
                        return bad("a face without a boundary", id);
                    }
                    let near = lone.first().map(|&v| rev.params(self.solid.point(v)).0);
                    let theta = free_angle(&holes, near);
                    out.push((self.whole_sphere(rev, theta, lone), true));
                }
            }
            (1, 0) => {
                let (i, w) = round_axis[0];
                // To the pole on the face's side of the loop.
                let up = w * s > 0.0;
                let (_, va) = rev.params(self.solid.point(self.solid.start_of(loops[i].0[0])));
                let pole = match rev.meridian {
                    Meridian::Circle {
                        center: 0.0,
                        radius,
                    } => rev.axis.origin + rev.axis.direction * if up { radius } else { -radius },
                    Meridian::Line { radius, slope } if slope != 0.0 => {
                        let z = -radius / slope;
                        if (z > va) != up {
                            return bad("a face on a cone bounded on its open side alone", id);
                        }
                        rev.axis.origin + rev.axis.direction * z
                    }
                    _ => {
                        return bad(
                            "a face on a cylinder or a torus bounded on one side alone",
                            id,
                        );
                    }
                };
                let theta = self.seam_angle(rev, &loops[i].0, None, &holes);
                let a = self.vertex_at(rev, &loops[i].0, theta, true, id)?;
                let la = self.current(&loops[i].0);
                let size = rev.size().max(1.0);
                let p = match lone
                    .iter()
                    .copied()
                    .find(|&v| self.solid.point(v).distance(pole) <= 1e-6 * size)
                {
                    Some(v) => v,
                    None => {
                        self.solid.vertices.push(Vertex::new(pole));
                        self.solid.vertices.len() as u32 - 1
                    }
                };
                let seam = self.meridian_edge(rev, a, p, theta, up);
                let mut l = rotated_to(&self.solid, &la, a, id)?;
                l.push(EdgeUse::forward(seam));
                l.push(EdgeUse::backward(seam));
                out.push((l, true));
            }
            (2, 0) | (0, 2) => {
                let along_axis = round_axis.len() == 2;
                let pair = if along_axis { &round_axis } else { &round_tube };
                let (ia, wa) = pair[0];
                let ib = pair[1].0;
                let toward = wa * s > 0.0;
                let (a, b, seam) = if along_axis {
                    let theta = self.seam_angle(rev, &loops[ia].0, Some(&loops[ib].0), &holes);
                    let a = self.vertex_at(rev, &loops[ia].0, theta, true, id)?;
                    let b = self.vertex_at(rev, &loops[ib].0, theta, true, id)?;
                    (a, b, self.meridian_edge(rev, a, b, theta, toward))
                } else {
                    let v = self.seam_height(rev, &loops[ia].0, &loops[ib].0);
                    let a = self.vertex_at(rev, &loops[ia].0, v, false, id)?;
                    let b = self.vertex_at(rev, &loops[ib].0, v, false, id)?;
                    (a, b, self.parallel_edge(rev, a, b, !toward))
                };
                let la = self.current(&loops[ia].0);
                let lb = self.current(&loops[ib].0);
                let mut l = rotated_to(&self.solid, &la, a, id)?;
                l.push(EdgeUse::forward(seam));
                l.extend(rotated_to(&self.solid, &lb, b, id)?);
                l.push(EdgeUse::backward(seam));
                out.push((l, true));
            }
            _ => {
                return bad(
                    "a face whose loops go round its surface in a way the kernel cannot read",
                    id,
                );
            }
        }
        let merged = !out.is_empty();
        for i in others {
            out.push((self.current(&loops[i].0), loops[i].1 && !merged));
        }
        Ok(out)
    }

    /// The angle a seam is put at between a loop that goes round the axis and another (or
    /// the pole): where both have a vertex, else where either has, else anywhere; clear of
    /// the face's holes.
    fn seam_angle(
        &self,
        rev: &Revolution,
        la: &[EdgeUse],
        lb: Option<&[EdgeUse]>,
        holes: &[(f64, f64)],
    ) -> f64 {
        let angles = |uses: &[EdgeUse]| -> Vec<(f64, f64)> {
            uses.iter()
                .filter_map(|&u| {
                    let p = self.solid.point(self.solid.start_of(u));
                    let rho = rev.rho_z(p).0;
                    (rho > NEAR_AXIS).then(|| (rev.params(p).0, rho))
                })
                .collect()
        };
        let ta = angles(la);
        let tb = lb.map(angles).unwrap_or_default();
        let tol = 1e-6 * rev.size().max(1.0);
        let same = |(t, r): (f64, f64), (u, q): (f64, f64)| angle_between(t, u) * r.max(q) <= tol;
        if let Some(&(t, _)) = ta
            .iter()
            .find(|&&a| tb.iter().any(|&b| same(a, b)) && !blocked(holes, a.0))
        {
            return t;
        }
        if let Some(&(t, _)) = ta.iter().chain(&tb).find(|a| !blocked(holes, a.0)) {
            return t;
        }
        free_angle(holes, ta.first().map(|a| a.0))
    }

    /// The `v` a seam round a torus's tube is put at: where both loops have a vertex, else
    /// where the first has.
    fn seam_height(&self, rev: &Revolution, la: &[EdgeUse], lb: &[EdgeUse]) -> f64 {
        let heights = |uses: &[EdgeUse]| -> Vec<f64> {
            uses.iter()
                .map(|&u| rev.params(self.solid.point(self.solid.start_of(u))).1)
                .collect()
        };
        let (va, vb) = (heights(la), heights(lb));
        let tol = 1e-9;
        va.iter()
            .copied()
            .find(|&a| vb.iter().any(|&b| angle_between(a, b) <= tol))
            .or_else(|| va.first().copied())
            .unwrap_or(0.0)
    }

    /// The loop's vertex at `θ = c` (or `v = c`), the loop's edge that crosses there split
    /// into two when it has none.
    fn vertex_at(
        &mut self,
        rev: &Revolution,
        uses: &[EdgeUse],
        c: f64,
        theta: bool,
        id: u64,
    ) -> R<u32> {
        let uses = self.current(uses);
        let tol = 1e-6 * rev.size().max(1.0);
        let reach = match rev.meridian {
            Meridian::Circle { radius, .. } if !theta => radius,
            _ => 1.0,
        };
        let coordinate = |q: (f64, f64)| if theta { q.0 } else { q.1 };
        for &u in &uses {
            let v = self.solid.start_of(u);
            let p = self.solid.point(v);
            let rho = rev.rho_z(p).0;
            let off = angle_between(coordinate(rev.params(p)), c) * if theta { rho } else { reach };
            if rho > NEAR_AXIS && off <= tol {
                return Ok(v);
            }
        }
        // Where the loop crosses `c`, taken round by whole turns.
        let points = self.param_points(rev, &uses)?;
        let n = uses.len();
        let off = |u: EdgeUse| rev.rho_z(self.solid.point(self.solid.start_of(u))).0 > NEAR_AXIS;
        let first = (0..n).find(|&i| off(uses[i])).unwrap_or(0);
        let mut at = points[0];
        for k in 0..n {
            let u = uses[(first + k) % n];
            let walk = self.solid.walk_edge(u);
            let ps = self.follow(rev, u, at)?;
            let steps = (ps.len() - 1) as f64;
            if walk.curve.is_degenerate() {
                at = ps[ps.len() - 1];
                continue;
            }
            for i in 0..ps.len() - 1 {
                let (x0, x1) = (coordinate(ps[i]), coordinate(ps[i + 1]));
                let lo = x0.min(x1);
                let target = c + TAU * ((lo - c) / TAU).ceil();
                if x0 == x1 || target > x0.max(x1) {
                    continue;
                }
                // Halved down to where the edge reaches `target`.
                let (mut s0, mut s1) = (
                    walk.sweep * i as f64 / steps,
                    walk.sweep * (i + 1) as f64 / steps,
                );
                let rising = x1 > x0;
                let value = |s: f64, near: f64| -> f64 {
                    let p = walk.curve.at(walk.a, walk.b, s).unwrap_or(walk.a);
                    let raw = coordinate(rev.params(p));
                    raw + TAU * ((near - raw) / TAU).round()
                };
                for _ in 0..60 {
                    let m = 0.5 * (s0 + s1);
                    let near = x0 + (x1 - x0) * ((m - s0) / (s1 - s0).max(1e-300)).clamp(0.0, 1.0);
                    if (value(m, near) < target) == rising {
                        s0 = m;
                    } else {
                        s1 = m;
                    }
                }
                let s = 0.5 * (s0 + s1);
                let p = walk.curve.at(walk.a, walk.b, s).unwrap_or(walk.a);
                for v in [self.solid.start_of(u), self.solid.end_of(u)] {
                    if self.solid.point(v).distance(p) <= tol {
                        return Ok(v);
                    }
                }
                return Ok(self.split(u.edge, p));
            }
            at = ps[ps.len() - 1];
        }
        bad("a loop that never reaches where its seam goes", id)
    }

    /// The edge cut at `p` into two, the first from its start to `p`; the new vertex.
    fn split(&mut self, e: u32, p: Point3) -> u32 {
        let edge = self.solid.edges[e as usize];
        self.solid.vertices.push(Vertex::new(p));
        let m = self.solid.vertices.len() as u32 - 1;
        self.solid.edges.push(Edge::new(edge.start, m, edge.curve));
        self.solid.edges.push(Edge::new(m, edge.end, edge.curve));
        let n = self.solid.edges.len() as u32;
        self.splits.insert(e, (n - 2, n - 1));
        m
    }

    /// A loop's uses as they are now: the edges split since cut into their pieces.
    fn current(&self, uses: &[EdgeUse]) -> Vec<EdgeUse> {
        let mut out = Vec::with_capacity(uses.len());
        for &u in uses {
            self.pieces(u, &mut out);
        }
        out
    }

    fn pieces(&self, u: EdgeUse, out: &mut Vec<EdgeUse>) {
        match self.splits.get(&u.edge) {
            Some(&(first, second)) => {
                if u.reversed {
                    self.pieces(EdgeUse::backward(second), out);
                    self.pieces(EdgeUse::backward(first), out);
                } else {
                    self.pieces(EdgeUse::forward(first), out);
                    self.pieces(EdgeUse::forward(second), out);
                }
            }
            None => out.push(u),
        }
    }

    /// A seam from `a` to `b` along the meridian at `θ`: a straight line on a cylinder or a
    /// cone, an arc of the meridian circle on a sphere or a torus, `v` growing when `up`.
    fn meridian_edge(&mut self, rev: &Revolution, a: u32, b: u32, theta: f64, up: bool) -> u32 {
        let curve = match rev.meridian {
            Meridian::Line { .. } => Curve::Line,
            Meridian::Circle { center, radius } => {
                let (x, y, ax) = rev.frame();
                let e = x * theta.cos() + y * theta.sin();
                // `v` grows counter-clockwise about e × a.
                let normal = e.cross(ax);
                Curve::Circle(Circle {
                    center: rev.axis.origin + e * center,
                    normal: if up { normal } else { -normal },
                    radius,
                })
            }
        };
        self.solid.edges.push(Edge::new(a, b, curve));
        self.solid.edges.len() as u32 - 1
    }

    /// A seam from `a` to `b` along the parallel through them, `θ` growing when `ccw`.
    fn parallel_edge(&mut self, rev: &Revolution, a: u32, b: u32, ccw: bool) -> u32 {
        let (rho, z) = rev.rho_z(self.solid.point(a));
        let ax = rev.axis.direction;
        let curve = Curve::Circle(Circle {
            center: rev.axis.origin + ax * z,
            normal: if ccw { ax } else { -ax },
            radius: rho,
        });
        self.solid.edges.push(Edge::new(a, b, curve));
        self.solid.edges.len() as u32 - 1
    }

    /// The loop of a whole sphere: its seam at `θ` from the south pole to the north and
    /// back (the poles' degenerate edges are put in after). A pole among `lone` is kept.
    fn whole_sphere(&mut self, rev: &Revolution, theta: f64, lone: &[u32]) -> Vec<EdgeUse> {
        let Meridian::Circle { radius, .. } = rev.meridian else {
            return Vec::new();
        };
        let a = rev.axis.direction;
        let size = radius.max(1.0);
        let pole = |p: Point3, solid: &mut Solid| -> u32 {
            match lone
                .iter()
                .copied()
                .find(|&v| solid.point(v).distance(p) <= 1e-6 * size)
            {
                Some(v) => v,
                None => {
                    solid.vertices.push(Vertex::new(p));
                    solid.vertices.len() as u32 - 1
                }
            }
        };
        let south = pole(rev.axis.origin - a * radius, &mut self.solid);
        let north = pole(rev.axis.origin + a * radius, &mut self.solid);
        let seam = self.meridian_edge(rev, south, north, theta, true);
        vec![EdgeUse::forward(seam), EdgeUse::backward(seam)]
    }

    /// A loop on a surface of revolution with the degenerate edges put back where it
    /// reaches the axis at an apex or a pole: between the edges arriving there and leaving,
    /// turning about the axis as far as the angle between them, the way the loop runs there
    /// (in `(θ, v)` counter-clockwise for a face whose normal is the surface's natural one:
    /// `θ` increasing at the lowest `v`, decreasing at the highest).
    fn poles(&mut self, rev: &Revolution, uses: &[EdgeUse]) -> Vec<EdgeUse> {
        let n = uses.len();
        let mut out = Vec::with_capacity(n + 2);
        for k in 0..n {
            let u = uses[k];
            out.push(u);
            let next = uses[(k + 1) % n];
            if self.solid.edges[u.edge as usize].curve.is_degenerate()
                || self.solid.edges[next.edge as usize].curve.is_degenerate()
            {
                continue;
            }
            let v = self.solid.start_of(next);
            let p = self.solid.point(v);
            if rev.rho_z(p).0 > 1e-6 * rev.size().max(1.0) {
                continue;
            }
            // The angles of the edges just before the pole and just after.
            let near = |solid: &Solid, use_: EdgeUse, at_end: bool| -> (f64, f64) {
                let walk = solid.walk_edge(use_);
                let closed = solid.edges[use_.edge as usize].is_closed();
                let extent = walk.curve.extent(walk.a, walk.b, closed);
                let s = if at_end {
                    extent * 0.999
                } else {
                    extent * 0.001
                };
                rev.params(walk.curve.at(walk.a, walk.b, s).unwrap_or(walk.mid))
            };
            let (t_in, v_in) = near(&self.solid, u, true);
            let (t_out, _) = near(&self.solid, next, false);
            let (_, v_pole) = rev.params(p);
            let lowest = v_in > v_pole;
            let up = lowest == rev.outward;
            let mut turn = if up { t_out - t_in } else { t_in - t_out }.rem_euclid(TAU);
            if turn <= 1e-9 {
                turn = TAU;
            }
            let a = rev.axis.direction;
            self.solid.edges.push(Edge::new(
                v,
                v,
                Curve::Degenerate {
                    axis: if up { a } else { -a },
                    turn,
                },
            ));
            out.push(EdgeUse::forward(self.solid.edges.len() as u32 - 1));
        }
        out
    }
}

/// The loop run the other way.
fn turned(uses: &[EdgeUse]) -> Vec<EdgeUse> {
    uses.iter().rev().map(|u| u.flipped()).collect()
}

/// The loop started at vertex `v`.
fn rotated_to(solid: &Solid, uses: &[EdgeUse], v: u32, id: u64) -> R<Vec<EdgeUse>> {
    let i = uses
        .iter()
        .position(|&u| solid.start_of(u) == v)
        .map_or_else(|| bad("a seam's vertex off its loop", id), Ok)?;
    Ok(uses[i..].iter().chain(&uses[..i]).copied().collect())
}

/// How far apart two angles are, round the shorter way.
fn angle_between(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(TAU);
    d.min(TAU - d)
}

/// Whether an angle is in one of `ranges` (each `(lo, hi)`, unwrapped), give or take.
fn blocked(ranges: &[(f64, f64)], t: f64) -> bool {
    let margin = 1e-3;
    ranges.iter().any(|&(lo, hi)| {
        let t = t - TAU * ((t - lo + margin) / TAU).floor();
        t <= hi + margin
    })
}

/// An angle in none of `ranges`: `preferred` when it is free, else the middle of the
/// widest gap between them.
fn free_angle(ranges: &[(f64, f64)], preferred: Option<f64>) -> f64 {
    let fallback = preferred.unwrap_or(0.0);
    if preferred.is_some_and(|t| !blocked(ranges, t)) || ranges.is_empty() {
        return fallback;
    }
    // The arcs covered, taken round into [0, 2π) and sorted.
    let mut arcs: Vec<(f64, f64)> = Vec::new();
    for &(lo, hi) in ranges {
        if hi - lo >= TAU {
            return fallback;
        }
        let a = lo.rem_euclid(TAU);
        let b = a + (hi - lo);
        if b > TAU {
            arcs.push((a, TAU));
            arcs.push((0.0, b - TAU));
        } else {
            arcs.push((a, b));
        }
    }
    arcs.sort_by(|p, q| p.0.total_cmp(&q.0));
    let first = arcs[0].0;
    let mut end = arcs[0].1;
    let mut best = (0.0, fallback);
    for &(a, b) in &arcs[1..] {
        if a - end > best.0 {
            best = (a - end, (a + end) / 2.0);
        }
        end = end.max(b);
    }
    let wrap = first + TAU - end;
    if wrap > best.0 {
        best = (wrap, end + wrap / 2.0);
    }
    best.1
}

/// The solid of a set of shells: the outer one, then its voids.
fn read_shells(g: &mut Geometry<'_>, outer: u64, voids: &[u64], id: u64) -> R<(Solid, Vec<u64>)> {
    let uncertainty = g.units.uncertainty;
    let mut reader = SolidReader {
        g,
        solid: Solid::default(),
        vertices: HashMap::new(),
        edges: HashMap::new(),
        raw: Vec::new(),
        ids: Vec::new(),
        splits: HashMap::new(),
    };
    let mut shells = vec![reader.shell(outer, false)?];
    for &v in voids {
        shells.push(reader.shell(v, false)?);
    }
    for k in 0..reader.solid.faces.len() {
        reader.settle(k)?;
    }
    // Faces settled before a later face split their edges take the pieces.
    let loops: Vec<Vec<Vec<EdgeUse>>> = reader
        .solid
        .faces
        .iter()
        .map(|face| face.loops.iter().map(|l| reader.current(l)).collect())
        .collect();
    for (face, l) in reader.solid.faces.iter_mut().zip(loops) {
        face.loops = l;
    }
    let ids = reader.ids;
    let mut solid = reader.solid;
    if shells.len() > 1 {
        solid.shells = shells;
    }
    compact(&mut solid);
    // As far apart as the file says its geometry may be, a few times over.
    let max = MAX_TOLERANCE.max(4.0 * uncertainty);
    solid
        .settle_tolerances_within(max)
        .map_err(|e| format!("#{id}: {e}"))?;
    orient_shells(&mut solid, &ids)?;
    solid.check().map_err(|e| format!("#{id}: {e}"))?;
    Ok((solid, ids))
}

/// The solids of a representation item: one for a B-rep, one for each shell that closes
/// of a shell-based surface model; each with its faces' instances.
fn read_item(g: &mut Geometry<'_>, id: u64) -> R<Vec<(Solid, Vec<u64>)>> {
    let f = g.f;
    let inst = f.get(id)?;
    if let Some(r) = inst.record("SHELL_BASED_SURFACE_MODEL") {
        let mut out = Vec::new();
        for shell in list(param(r, 1)).iter().filter_map(reference) {
            match read_shells(g, shell, &[], shell) {
                Ok(s) => out.push(s),
                // An open shell that does not close a volume is not a solid.
                Err(_) if !f.get(shell)?.is("CLOSED_SHELL") => {}
                Err(e) => return Err(e),
            }
        }
        return Ok(out);
    }
    let r = inst
        .record("BREP_WITH_VOIDS")
        .or_else(|| inst.record("MANIFOLD_SOLID_BREP"))
        .or_else(|| inst.record("FACETED_BREP"))
        .map_or_else(|| bad("not a solid", id), Ok)?;
    let outer = arg_ref(r, 1, id)?;
    let voids: Vec<u64> = list(param(r, 2)).iter().filter_map(reference).collect();
    Ok(vec![read_shells(g, outer, &voids, id)?])
}

/// The solid without the vertices and edges no face uses (those of edges of no length,
/// of vertex loops, of edges split for a seam), numbered again.
fn compact(solid: &mut Solid) {
    let mut edge_map = vec![u32::MAX; solid.edges.len()];
    let mut edges = Vec::new();
    for face in &mut solid.faces {
        for u in face.loops.iter_mut().flatten() {
            let e = u.edge as usize;
            if edge_map[e] == u32::MAX {
                edge_map[e] = edges.len() as u32;
                edges.push(solid.edges[e]);
            }
            u.edge = edge_map[e];
        }
    }
    let mut vertex_map = vec![u32::MAX; solid.vertices.len()];
    let mut vertices = Vec::new();
    for edge in &mut edges {
        for v in [&mut edge.start, &mut edge.end] {
            let i = *v as usize;
            if vertex_map[i] == u32::MAX {
                vertex_map[i] = vertices.len() as u32;
                vertices.push(solid.vertices[i]);
            }
            *v = vertex_map[i];
        }
    }
    solid.edges = edges;
    solid.vertices = vertices;
}

/// Each shell turned to face the way the kernel's do: the outer one's normals out of the
/// volume it bounds, each void's into it.
fn orient_shells(solid: &mut Solid, ids: &[u64]) -> R<()> {
    let shells: Vec<Vec<u32>> = if solid.shells.is_empty() {
        vec![(0..solid.faces.len() as u32).collect()]
    } else {
        solid.shells.clone()
    };
    let (lo, hi) = solid.vertices.iter().fold(
        (
            Vec3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY),
            Vec3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY),
        ),
        |(lo, hi), v| {
            let p = v.point.to_vec();
            (
                Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)),
                Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)),
            )
        },
    );
    let c = (lo + hi) * 0.5;
    let reference = Point3::new(c.x, c.y, c.z);
    for (i, shell) in shells.iter().enumerate() {
        let mut volume = 0.0;
        for &f in shell {
            volume += solid
                .face_integrals(&solid.faces[f as usize], reference)
                .map_err(|e| format!("#{}: {e}", ids[f as usize]))?
                .flux
                / 3.0;
        }
        if volume != 0.0 && (volume > 0.0) != (i == 0) {
            for &f in shell {
                let face = &mut solid.faces[f as usize];
                face.surface = face.surface.reversed();
                for l in &mut face.loops {
                    *l = turned(l);
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Products, placements, colours and materials.

/// A product of the file: its name, its definition, the solid items of its shape (with the
/// context each is read in), and its children placed.
#[derive(Debug, Clone, Default)]
struct Product {
    name: String,
    definition: u64,
    items: Vec<(u64, u64)>,
    children: Vec<(usize, String, Transform)>,
}

const SOLID_ITEMS: [&str; 4] = [
    "MANIFOLD_SOLID_BREP",
    "BREP_WITH_VOIDS",
    "FACETED_BREP",
    "SHELL_BASED_SURFACE_MODEL",
];

fn is_solid_item(f: &File, id: u64) -> bool {
    f.get(id).is_ok_and(|i| SOLID_ITEMS.iter().any(|k| i.is(k)))
}

/// The representations a representation is related to without a motion (a product's
/// `SHAPE_REPRESENTATION` and the B-rep representation beside it), itself first.
fn related(f: &File, rep: u64) -> Vec<u64> {
    let mut out = vec![rep];
    let mut seen: HashSet<u64> = HashSet::from([rep]);
    let mut stack = vec![rep];
    while let Some(r) = stack.pop() {
        for &id in f.all("SHAPE_REPRESENTATION_RELATIONSHIP") {
            let Ok(inst) = f.get(id) else { continue };
            if inst.is("REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION") {
                continue;
            }
            let Some(rr) = inst
                .record("SHAPE_REPRESENTATION_RELATIONSHIP")
                .filter(|x| x.params.len() >= 4)
                .or_else(|| inst.record("REPRESENTATION_RELATIONSHIP"))
            else {
                continue;
            };
            let (a, b) = (reference(param(rr, 2)), reference(param(rr, 3)));
            for (x, y) in [(a, b), (b, a)] {
                if x == Some(r)
                    && let Some(y) = y
                    && seen.insert(y)
                {
                    out.push(y);
                    stack.push(y);
                }
            }
        }
    }
    out
}

/// The solid items of a representation and those related to it, with their contexts.
fn items_of(f: &File, rep: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for r in related(f, rep) {
        let Some(rec) = f.main_record(r, 3) else {
            continue;
        };
        let context = reference(param(rec, 2)).unwrap_or(0);
        for item in list(param(rec, 1)).iter().filter_map(reference) {
            if is_solid_item(f, item) {
                out.push((item, context));
            }
        }
    }
    out
}

/// The product structure: every product definition, with its solids and its children.
struct Products {
    list: Vec<Product>,
    roots: Vec<usize>,
}

fn products(f: &File) -> R<Products> {
    // Each product definition shape's definition (a product definition, or an occurrence).
    let mut definition_of: HashMap<u64, u64> = HashMap::new();
    for &id in f.all("PRODUCT_DEFINITION_SHAPE") {
        if let Some(d) = f
            .record(id, "PRODUCT_DEFINITION_SHAPE")
            .ok()
            .and_then(|r| reference(param(r, 2)))
        {
            definition_of.insert(id, d);
        }
    }
    let mut reps_of: HashMap<u64, Vec<u64>> = HashMap::new();
    for &id in f.all("SHAPE_DEFINITION_REPRESENTATION") {
        let Ok(r) = f.record(id, "SHAPE_DEFINITION_REPRESENTATION") else {
            continue;
        };
        let (Some(pds), Some(rep)) = (reference(param(r, 0)), reference(param(r, 1))) else {
            continue;
        };
        if let Some(&d) = definition_of.get(&pds) {
            reps_of.entry(d).or_default().push(rep);
        }
    }
    let mut index: HashMap<u64, usize> = HashMap::new();
    let mut list_: Vec<Product> = Vec::new();
    for &pd in f.all("PRODUCT_DEFINITION") {
        let Ok(r) = f.record(pd, "PRODUCT_DEFINITION") else {
            continue;
        };
        let name = reference(param(r, 2))
            .and_then(|pdf| f.main_record(pdf, 3))
            .and_then(|r| reference(param(r, 2)))
            .and_then(|p| f.record(p, "PRODUCT").ok())
            .map(|r| name_or_id(r, 1, 0))
            .unwrap_or_default();
        let mut items: Vec<(u64, u64)> = reps_of
            .get(&pd)
            .map(|reps| reps.iter().flat_map(|&rep| items_of(f, rep)).collect())
            .unwrap_or_default();
        items.sort_unstable();
        items.dedup();
        index.insert(pd, list_.len());
        list_.push(Product {
            name,
            definition: pd,
            items,
            children: Vec::new(),
        });
    }
    // Each occurrence's placement: its shape representation relationship's transformation.
    let mut placed: HashMap<u64, (u64, u64, u64)> = HashMap::new();
    for &id in f.all("CONTEXT_DEPENDENT_SHAPE_REPRESENTATION") {
        let Ok(r) = f.record(id, "CONTEXT_DEPENDENT_SHAPE_REPRESENTATION") else {
            continue;
        };
        let (Some(rel), Some(pds)) = (reference(param(r, 0)), reference(param(r, 1))) else {
            continue;
        };
        let Some(&nauo) = definition_of.get(&pds) else {
            continue;
        };
        let Ok(rel) = f.get(rel) else { continue };
        let (Some(rr), Some(rt)) = (
            rel.record("REPRESENTATION_RELATIONSHIP"),
            rel.record("REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION"),
        ) else {
            continue;
        };
        if let (Some(r1), Some(r2), Some(t)) = (
            reference(param(rr, 2)),
            reference(param(rr, 3)),
            reference(param(rt, 0)),
        ) {
            placed.insert(nauo, (r1, r2, t));
        }
    }
    let mut is_child: HashSet<usize> = HashSet::new();
    for &id in f.all("NEXT_ASSEMBLY_USAGE_OCCURRENCE") {
        let Ok(r) = f.record(id, "NEXT_ASSEMBLY_USAGE_OCCURRENCE") else {
            continue;
        };
        let (Some(&parent), Some(&child)) = (
            reference(param(r, 3)).and_then(|p| index.get(&p)),
            reference(param(r, 4)).and_then(|c| index.get(&c)),
        ) else {
            continue;
        };
        let transform = match placed.get(&id) {
            Some(&(r1, r2, t)) => {
                let child_reps: HashSet<u64> = reps_of
                    .get(&list_[child].definition)
                    .map(|v| v.iter().flat_map(|&rep| related(f, rep)).collect())
                    .unwrap_or_default();
                occurrence_transform(f, &child_reps, r1, r2, t)?
            }
            None => Transform::IDENTITY,
        };
        list_[parent]
            .children
            .push((child, name_or_id(r, 1, 0), transform));
        is_child.insert(child);
    }
    let roots = (0..list_.len())
        .filter(|i| !is_child.contains(i) && has_solids(&list_, *i, 0))
        .collect();
    Ok(Products { list: list_, roots })
}

/// Whether a product or one of its children has a solid.
fn has_solids(list: &[Product], i: usize, depth: u32) -> bool {
    depth <= 64
        && (!list[i].items.is_empty()
            || list[i]
                .children
                .iter()
                .any(|c| has_solids(list, c.0, depth + 1)))
}

/// The rigid motion an `AXIS2_PLACEMENT_3D` makes of the world's frame.
fn frame_transform(g: &Geometry<'_>, id: u64) -> R<Transform> {
    let (o, z, x) = g.placement(id)?;
    let y = z.cross(x);
    Ok(Transform {
        rows: [
            [x.x, y.x, z.x, o.x],
            [x.y, y.y, z.y, o.y],
            [x.z, y.z, z.z, o.z],
        ],
    })
}

/// Where an occurrence puts its child: the `ITEM_DEFINED_TRANSFORMATION`'s placement in
/// the parent's representation taken from its placement in the child's (the standard has
/// the child's representation first; some writers swap them).
fn occurrence_transform(
    f: &File,
    child_reps: &HashSet<u64>,
    r1: u64,
    r2: u64,
    t: u64,
) -> R<Transform> {
    let tr = f.record(t, "ITEM_DEFINED_TRANSFORMATION")?;
    let (Some(i1), Some(i2)) = (reference(param(tr, 2)), reference(param(tr, 3))) else {
        return bad("a transformation without its placements", t);
    };
    let units = |rep: u64| units_of(f, context_of(f, rep));
    let ((from, uf), (to, ut)) = if child_reps.contains(&r2) && !child_reps.contains(&r1) {
        ((i2, units(r2)), (i1, units(r1)))
    } else {
        ((i1, units(r1)), (i2, units(r2)))
    };
    let a = frame_transform(&Geometry::new(f, uf), from)?;
    let b = frame_transform(&Geometry::new(f, ut), to)?;
    Ok(inverse(&a).then(&b))
}

/// A colour of a presentation style, if it has one: the first surface colour found down
/// its styles, and a transparency.
fn style_color(f: &File, id: u64, depth: u32) -> Option<StepColor> {
    if depth > 10 {
        return None;
    }
    let inst = f.get(id).ok()?;
    if let Some(r) = inst.record("COLOUR_RGB") {
        let c = |k: usize| (real(param(r, k)).unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
        return Some(StepColor {
            rgb: [c(1), c(2), c(3)],
            transparency: 0.0,
        });
    }
    if let Some(r) = inst.record("DRAUGHTING_PRE_DEFINED_COLOUR") {
        return named_colour(&string(param(r, 0))).map(|rgb| StepColor {
            rgb,
            transparency: 0.0,
        });
    }
    if inst.is("CURVE_STYLE") || inst.is("POINT_STYLE") {
        return None;
    }
    let mut colour: Option<StepColor> = None;
    let mut transparency: f64 = 0.0;
    let records: Vec<&Record> = match inst {
        Instance::Simple(r) => vec![r],
        Instance::Complex(rs) => rs.iter().collect(),
    };
    for r in records {
        if r.name == "SURFACE_STYLE_TRANSPARENT" {
            transparency = transparency.max(real(param(r, 0)).unwrap_or(0.0));
            continue;
        }
        for p in &r.params {
            for q in std::iter::once(p).chain(list(p)) {
                let Some(next) = reference(q) else { continue };
                if let Some(c) = style_color(f, next, depth + 1) {
                    colour = colour.or(Some(c));
                    transparency = transparency.max(c.transparency);
                }
                if let Some(t) = f
                    .get(next)
                    .ok()
                    .and_then(|i| i.record("SURFACE_STYLE_TRANSPARENT"))
                {
                    transparency = transparency.max(real(param(t, 0)).unwrap_or(0.0));
                }
            }
        }
    }
    colour.map(|c| StepColor {
        rgb: c.rgb,
        transparency: transparency.clamp(0.0, 1.0),
    })
}

fn named_colour(name: &str) -> Option<[u8; 3]> {
    Some(match name {
        "red" => [255, 0, 0],
        "green" => [0, 255, 0],
        "blue" => [0, 0, 255],
        "yellow" => [255, 255, 0],
        "magenta" => [255, 0, 255],
        "cyan" => [0, 255, 255],
        "black" => [0, 0, 0],
        "white" => [255, 255, 255],
        _ => return None,
    })
}

/// The colour each styled item is given (solids, shells, faces), the overriding styles
/// after the others.
fn colours(f: &File) -> HashMap<u64, StepColor> {
    let mut out = HashMap::new();
    for kind in ["STYLED_ITEM", "OVER_RIDING_STYLED_ITEM"] {
        for &id in f.all(kind) {
            let Ok(inst) = f.get(id) else { continue };
            if kind == "STYLED_ITEM" && inst.is("OVER_RIDING_STYLED_ITEM") {
                continue;
            }
            let Some(r) = inst.record(kind) else { continue };
            let Some(item) = reference(param(r, 2)) else {
                continue;
            };
            let colour = list(param(r, 1))
                .iter()
                .filter_map(reference)
                .find_map(|psa| style_color(f, psa, 0));
            if let Some(c) = colour {
                out.insert(item, c);
            }
        }
    }
    out
}

/// A density as a file gives it in kg/m³, as Fusion 360 and SolidWorks write it (or
/// g/cm³, g/mm³), told from the size of the number as the OpenCASCADE kernel tells it.
fn density_kg_per_m3(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        0.0
    } else if value < 0.03 {
        value * 1e6
    } else if value < 50.0 {
        value * 1e3
    } else {
        value
    }
}

/// The material a product definition's properties name: `material name` and `density`.
fn material_of(f: &File, definition: u64) -> Option<StepMaterial> {
    let mut name: Option<(String, String)> = None;
    let mut density = 0.0;
    for &pdr in f.all("PROPERTY_DEFINITION_REPRESENTATION") {
        let Ok(r) = f.record(pdr, "PROPERTY_DEFINITION_REPRESENTATION") else {
            continue;
        };
        let (Some(pd), Some(rep)) = (reference(param(r, 0)), reference(param(r, 1))) else {
            continue;
        };
        let Ok(prop) = f.record(pd, "PROPERTY_DEFINITION") else {
            continue;
        };
        if reference(param(prop, 2)) != Some(definition) {
            continue;
        }
        let what = string(param(prop, 1));
        let Some(rr) = f.main_record(rep, 2) else {
            continue;
        };
        for item in list(param(rr, 1)).iter().filter_map(reference) {
            let Ok(it) = f.get(item) else { continue };
            if let Some(d) = it.record("DESCRIPTIVE_REPRESENTATION_ITEM")
                && (what == "material name" || name.is_none())
            {
                name = Some((string(param(d, 0)), string(param(d, 1))));
            }
            if let Some(m) = it.record("MEASURE_REPRESENTATION_ITEM")
                && (what == "density" || string(param(m, 0)).contains("density"))
            {
                density = real(param(m, 1)).unwrap_or(0.0);
            }
        }
    }
    let (name, description) = name?;
    (!name.trim().is_empty()).then(|| StepMaterial {
        name,
        description,
        density: density_kg_per_m3(density),
    })
}

/// A product's own solids, read in its own coordinates, with their colours.
#[derive(Debug, Clone, Default)]
struct Read {
    solids: Vec<Solid>,
    colors: StepColors,
}

impl Read {
    fn add(&mut self, other: &Read, place: &Transform) {
        if *place == Transform::IDENTITY {
            self.solids.extend(other.solids.iter().cloned());
        } else {
            self.solids
                .extend(other.solids.iter().map(|s| s.transformed(place)));
        }
        self.colors
            .solids
            .extend(other.colors.solids.iter().copied());
        self.colors.faces.extend(other.colors.faces.iter().copied());
    }
}

/// What reading a file's products needs.
struct Reader {
    f: File,
    products: Products,
    colors: HashMap<u64, StepColor>,
    read: HashMap<usize, Rc<Read>>,
}

impl Reader {
    fn new(data: &[u8]) -> R<Reader> {
        let f = File::read(data)?;
        let products = products(&f)?;
        let colors = colours(&f);
        Ok(Reader {
            f,
            products,
            colors,
            read: HashMap::new(),
        })
    }

    /// The solids of some items, with their colours: a face's own, else its solid's (or
    /// its shell's).
    fn read_items(&self, items: &[(u64, u64)]) -> R<Read> {
        let f = &self.f;
        let mut out = Read::default();
        for &(item, context) in items {
            let mut g = Geometry::new(f, units_of(f, context));
            let shells: Vec<u64> = f
                .main_record(item, 2)
                .map(|r| {
                    list(param(r, 1))
                        .iter()
                        .chain(std::iter::once(param(r, 1)))
                        .filter_map(reference)
                        .collect()
                })
                .unwrap_or_default();
            let item_colour = self
                .colors
                .get(&item)
                .copied()
                .or_else(|| shells.iter().find_map(|s| self.colors.get(s).copied()));
            for (solid, faces) in read_item(&mut g, item)? {
                out.colors.solids.push(item_colour);
                out.colors.faces.extend(
                    faces
                        .iter()
                        .map(|id| self.colors.get(id).copied().or(item_colour)),
                );
                out.solids.push(solid);
            }
        }
        Ok(out)
    }

    /// A product's own solids, read once.
    fn own(&mut self, n: usize) -> R<Rc<Read>> {
        if let Some(r) = self.read.get(&n) {
            return Ok(r.clone());
        }
        let r = Rc::new(self.read_items(&self.products.list[n].items.clone())?);
        self.read.insert(n, r.clone());
        Ok(r)
    }

    /// A product's solids and its children's, placed by `place`.
    fn flatten(&mut self, n: usize, place: &Transform, depth: u32, out: &mut Read) -> R<()> {
        if depth > 64 {
            return Err("an assembly that holds itself".to_owned());
        }
        let own = self.own(n)?;
        out.add(&own, place);
        let children = self.products.list[n].children.clone();
        for (child, _, t) in children {
            self.flatten(child, &t.then(place), depth + 1, out)?;
        }
        Ok(())
    }

    /// Every solid of a file without products to hold them, where it is.
    fn loose(&self) -> R<Read> {
        let f = &self.f;
        let mut items = Vec::new();
        for kind in SOLID_ITEMS {
            for &item in f.all(kind) {
                // Each item once, and not a part of another item (a shell-based model's
                // shells are its own).
                let context = f.x.data.iter().find_map(|(&rep, inst)| {
                    let r = match inst {
                        Instance::Simple(r) => r,
                        Instance::Complex(rs) => rs.iter().find(|r| r.params.len() >= 3)?,
                    };
                    (r.name.ends_with("REPRESENTATION")
                        && list(param(r, 1)).iter().any(|p| reference(p) == Some(item)))
                    .then(|| context_of(f, rep))
                });
                if let Some(context) = context {
                    items.push((item, context));
                }
            }
        }
        items.sort_unstable();
        items.dedup();
        self.read_items(&items)
    }
}

/// What `import_step` gives: every solid of the file placed, as one shape, their colours,
/// and the first product's material.
pub(crate) struct Imported {
    pub shape: Shape,
    pub colors: StepColors,
    pub material: Option<StepMaterial>,
}

pub(crate) fn import(data: &[u8]) -> R<Imported> {
    let mut reader = Reader::new(data)?;
    let mut all = Read::default();
    let mut material = None;
    for r in reader.products.roots.clone() {
        reader.flatten(r, &Transform::IDENTITY, 0, &mut all)?;
    }
    for p in &reader.products.list {
        if material.is_none() && !p.items.is_empty() {
            material = material_of(&reader.f, p.definition);
        }
    }
    if all.solids.is_empty() {
        all = reader.loose()?;
    }
    if all.solids.is_empty() {
        return Err("the file holds no solid".to_owned());
    }
    Ok(Imported {
        shape: Shape { solids: all.solids },
        colors: all.colors,
        material,
    })
}

/// The product structure of a file: every part (its solids written as a STEP file of
/// their own, in its own coordinates, with its colours and material) and every assembly
/// with its children placed, each once however often it is used. A mirrored instance is a
/// part of its own, the mirror applied; a product with solids of its own and children
/// holds its solids as a part beside them.
pub(crate) fn assembly(data: &[u8]) -> R<StepAssembly> {
    let mut reader = Reader::new(data)?;
    let mut nodes: Vec<StepNode> = Vec::new();
    let mut done: HashMap<usize, Option<usize>> = HashMap::new();
    let mut tops = Vec::new();
    for r in reader.products.roots.clone() {
        if let Some(n) = build(&mut reader, r, &mut nodes, &mut done, 0)? {
            tops.push((n, reader.products.list[r].name.clone()));
        }
    }
    if tops.is_empty() {
        let loose = reader.loose()?;
        if loose.solids.is_empty() {
            return Err("the file holds no solid".to_owned());
        }
        nodes.push(part_node(String::new(), &loose, None)?);
        return Ok(StepAssembly { nodes, root: 0 });
    }
    let root = if let [(n, _)] = tops.as_slice() {
        *n
    } else {
        // Several top-level products: an unnamed assembly holds them.
        nodes.push(StepNode {
            name: String::new(),
            content: StepNodeContent::Assembly {
                instances: tops
                    .iter()
                    .map(|(n, name)| StepInstance {
                        node: *n,
                        name: name.clone(),
                        transform: Transform::IDENTITY,
                    })
                    .collect(),
            },
        });
        nodes.len() - 1
    };
    Ok(StepAssembly { nodes, root })
}

fn part_node(name: String, read: &Read, material: Option<StepMaterial>) -> R<StepNode> {
    let shape = Shape {
        solids: read.solids.clone(),
    };
    let step = super::write::write(
        &[&shape],
        std::slice::from_ref(&name),
        std::slice::from_ref(&read.colors),
        std::slice::from_ref(&material),
    )
    .map_err(|e| e.to_string())?;
    Ok(StepNode {
        name,
        content: StepNodeContent::Part {
            step: step.into_bytes(),
            material,
        },
    })
}

/// The node of a product, made once: `None` for one with no solids anywhere.
fn build(
    reader: &mut Reader,
    n: usize,
    nodes: &mut Vec<StepNode>,
    done: &mut HashMap<usize, Option<usize>>,
    depth: u32,
) -> R<Option<usize>> {
    if let Some(&d) = done.get(&n) {
        return Ok(d);
    }
    if depth > 64 {
        return Err("an assembly that holds itself".to_owned());
    }
    let product = reader.products.list[n].clone();
    let material = material_of(&reader.f, product.definition);
    let own = reader.own(n)?;
    let made = if product.children.is_empty() {
        if own.solids.is_empty() {
            None
        } else {
            nodes.push(part_node(product.name.clone(), &own, material)?);
            Some(nodes.len() - 1)
        }
    } else {
        let mut instances = Vec::new();
        if !own.solids.is_empty() {
            nodes.push(part_node(product.name.clone(), &own, material)?);
            instances.push(StepInstance {
                node: nodes.len() - 1,
                name: product.name.clone(),
                transform: Transform::IDENTITY,
            });
        }
        for (child, name, t) in &product.children {
            if t.is_mirror() {
                let mut moved = Read::default();
                reader.flatten(*child, t, depth + 1, &mut moved)?;
                if moved.solids.is_empty() {
                    continue;
                }
                let child_product = &reader.products.list[*child];
                let material = material_of(&reader.f, child_product.definition);
                nodes.push(part_node(
                    format!("{} (mirrored)", child_product.name),
                    &moved,
                    material,
                )?);
                instances.push(StepInstance {
                    node: nodes.len() - 1,
                    name: name.clone(),
                    transform: Transform::IDENTITY,
                });
            } else if let Some(c) = build(reader, *child, nodes, done, depth + 1)? {
                instances.push(StepInstance {
                    node: c,
                    name: name.clone(),
                    transform: *t,
                });
            }
        }
        if instances.is_empty() {
            None
        } else {
            nodes.push(StepNode {
                name: product.name.clone(),
                content: StepNodeContent::Assembly { instances },
            });
            Some(nodes.len() - 1)
        }
    };
    done.insert(n, made);
    Ok(made)
}
