//! Offsets of solids, and shells made of them.
//!
//! A solid offset by a distance along each face's normal keeps its topology: each face's
//! surface is moved (a plane along its normal, a cylinder's or a cone's meridian line square
//! to itself, a sphere's or a torus's tube radius), each vertex goes where its faces' moved
//! surfaces meet again (by Gauss–Newton, from where their tangent planes would put it), and
//! each edge runs along the curve its two faces' moved surfaces meet in: a line, a parallel
//! at its vertex's height, a meridian circle, a circle where a plane cuts a sphere, a section
//! of the moved surfaces. A surface shrunk to nothing, a face turned inside out or an edge
//! run backwards mean walls too thick for the part: the offset is refused. Faces meet as
//! their surfaces extended do, never rounded off (OpenCASCADE's `GeomAbs_Intersection`).
//!
//! A shell is a boolean of the solid with an offset of it, named as OpenCASCADE names its
//! faces: inwards, the solid less its offset inside, every face moved in by the thickness
//! and each face to open moved out instead, so that the cut runs through it; outwards, the
//! solid offset outside (the faces to open left where they are) less the solid (those moved
//! out). The faces of the solid keep their names ([`FaceSource::InputFace`]); the walls
//! offset from them, and the rim left round each opening, are named after the faces they
//! come from ([`FaceSource::OffsetFace`]). With no face to open, the result is the solid
//! round a void (inwards) or a skin round the solid (outwards).

// Indices into a solid's own vertices, edges and faces.
#![allow(clippy::indexing_slicing)]

use oxyde_geom::{LINEAR_TOLERANCE, Point3, Vec3};
use oxyde_kernel::{BooleanOp, FaceSource, OpHistory};

use crate::NativeError;
use crate::boolean;
use crate::brep::{Edge, Face, Shape, Solid, Vertex};
use crate::geometry::{Circle, Curve, Section, Surface};
use crate::revolution::{Meridian, Revolution};

/// OpenCASCADE's words for a shell it cannot make.
const SHELL_FAILED: &str = "shell failed (walls too thick for the part?)";

fn too_thick() -> NativeError {
    NativeError::Failed(SHELL_FAILED.to_owned())
}

/// `shape`, one solid, hollowed: `removed` (indices of its faces) opened, walls `|offset|`
/// thick inside it for a negative `offset`, outside it for a positive one.
pub(crate) fn shell(
    shape: &Shape,
    removed: &[u32],
    offset: f64,
) -> Result<(Shape, OpHistory), NativeError> {
    let [solid] = shape.solids.as_slice() else {
        return Err(NativeError::Failed(
            "a shell hollows one solid at a time".to_owned(),
        ));
    };
    let t = offset.abs();
    if t.is_nan() || t <= LINEAR_TOLERANCE {
        return Err(NativeError::Failed("the wall thickness is zero".to_owned()));
    }
    let n = solid.faces.len() as u32;
    if let Some(&f) = removed.iter().find(|&&f| f >= n) {
        return Err(NativeError::Failed(format!("there is no face {f}")));
    }
    let opened = |f: u32| removed.contains(&f);
    let by_face = |inner: f64, open: f64| -> Vec<f64> {
        (0..n)
            .map(|f| if opened(f) { open } else { inner })
            .collect()
    };
    let whole = |s: Solid| Shape { solids: vec![s] };
    let inward = offset < 0.0;
    let (target, tool) = if inward {
        // The solid, less the space `t` inside its faces, the faces to open moved out.
        (
            whole(solid.clone()),
            whole(offset_solid(solid, &by_face(-t, t))?),
        )
    } else {
        // The solid grown `t` outside its faces (the faces to open where they are), less
        // the solid (those moved out).
        let core = if removed.is_empty() {
            solid.clone()
        } else {
            offset_solid(solid, &by_face(0.0, t))?
        };
        (whole(offset_solid(solid, &by_face(t, 0.0))?), whole(core))
    };
    let (result, history) = boolean::boolean(BooleanOp::Cut, &target, &[&tool])?;
    // Inwards the solid's faces keep their names but for the rims round the openings,
    // named after the faces opened, as the walls inside are after theirs; outwards the
    // walls outside are named after the faces they grew from, the solid's inside them keep
    // theirs.
    let name = |input: u32, face: u32| -> FaceSource {
        let offset_face = FaceSource::OffsetFace { input: 0, face };
        let input_face = FaceSource::InputFace { input: 0, face };
        match (inward, input) {
            (true, 0) if opened(face) => offset_face,
            (true, 0) => input_face,
            (true, _) => offset_face,
            (false, 0) => offset_face,
            (false, _) => input_face,
        }
    };
    let history = OpHistory {
        faces: history
            .faces
            .iter()
            .map(|sources| {
                let mut out: Vec<FaceSource> = sources
                    .iter()
                    .filter_map(|s| match *s {
                        FaceSource::InputFace { input, face } => Some(name(input, face)),
                        _ => None,
                    })
                    .collect();
                out.dedup();
                out
            })
            .collect(),
    };
    Ok((result, history))
}

/// `solid` with each face moved `distances[face]` along its normal (out of the solid for a
/// positive distance), its topology kept.
pub(crate) fn offset_solid(solid: &Solid, distances: &[f64]) -> Result<Solid, NativeError> {
    let surfaces: Vec<Surface> = solid
        .faces
        .iter()
        .zip(distances)
        .map(|(face, &d)| face.surface.offset(d).ok_or_else(too_thick))
        .collect::<Result<_, _>>()?;
    let mut uses: Vec<Vec<u32>> = vec![Vec::new(); solid.edges.len()];
    for (f, face) in solid.faces.iter().enumerate() {
        for u in face.loops.iter().flatten() {
            if !uses[u.edge as usize].contains(&(f as u32)) {
                uses[u.edge as usize].push(f as u32);
            }
        }
    }
    let mut faces_at: Vec<Vec<u32>> = vec![Vec::new(); solid.vertices.len()];
    for (e, edge) in solid.edges.iter().enumerate() {
        for v in [edge.start, edge.end] {
            for &f in &uses[e] {
                if !faces_at[v as usize].contains(&f) {
                    faces_at[v as usize].push(f);
                }
            }
        }
    }
    let vertices: Vec<Vertex> = (0..solid.vertices.len())
        .map(|v| moved_vertex(solid, v as u32, &faces_at[v], &surfaces, distances).map(Vertex::new))
        .collect::<Result<_, _>>()?;
    let edges: Vec<Edge> = solid
        .edges
        .iter()
        .enumerate()
        .map(|(e, edge)| {
            let curve = moved_curve(solid, edge, &uses[e], &surfaces, &vertices)?;
            Ok(Edge::new(edge.start, edge.end, curve))
        })
        .collect::<Result<_, NativeError>>()?;
    let faces: Vec<Face> = solid
        .faces
        .iter()
        .zip(&surfaces)
        .map(|(face, surface)| Face {
            surface: *surface,
            loops: face.loops.clone(),
        })
        .collect();
    let mut out = Solid {
        vertices,
        edges,
        faces,
        shells: solid.shells.clone(),
    };
    // Straight edges keep their way; faces keep their side; everything still meets.
    for (old, new) in solid.edges.iter().zip(&out.edges) {
        if old.curve == Curve::Line {
            let before = solid.point(old.end) - solid.point(old.start);
            let after = out.point(new.end) - out.point(new.start);
            if before.dot(after) <= 0.0 || after.length() <= LINEAR_TOLERANCE {
                return Err(too_thick());
            }
        }
    }
    out.settle_tolerances().map_err(|_| too_thick())?;
    out.check().map_err(|_| too_thick())?;
    for (old, new) in solid.faces.iter().zip(&out.faces) {
        let before = solid
            .face_integrals(old, Point3::ORIGIN)
            .map_err(NativeError::Internal)?
            .area;
        let after = out
            .face_integrals(new, Point3::ORIGIN)
            .map_err(|_| too_thick())?
            .area;
        if after.is_nan()
            || after <= LINEAR_TOLERANCE * LINEAR_TOLERANCE
            || before.signum() != after.signum()
        {
            return Err(too_thick());
        }
    }
    let volume = Shape {
        solids: vec![out.clone()],
    }
    .volume()
    .map_err(|_| too_thick())?;
    if volume <= 0.0 {
        return Err(too_thick());
    }
    Ok(out)
}

/// Where vertex `v` goes: where the moved surfaces of the faces round it meet, nearest
/// where their tangent planes would put it (along the curve two of them meet in, at the
/// vertex's place across it); a cone's apex or a sphere's pole where the moved surface has
/// it.
fn moved_vertex(
    solid: &Solid,
    v: u32,
    faces: &[u32],
    surfaces: &[Surface],
    distances: &[f64],
) -> Result<Point3, NativeError> {
    let p = solid.point(v);
    // On an axis: the moved surface's apex or pole nearest.
    for &f in faces {
        if let (Surface::Revolution(old), Surface::Revolution(new)) =
            (solid.faces[f as usize].surface, surfaces[f as usize])
            && old.rho_z(p).0 <= 1e-9 * old.size().max(1.0)
        {
            return pole_near(&new, p).ok_or_else(too_thick);
        }
    }
    let normal = |s: &Surface, q: Point3| match s {
        Surface::Plane(plane) => plane.normal,
        Surface::Revolution(r) => r.normal_at(q),
    };
    // The surfaces round the vertex, each with how far it moves: one surface once, and
    // surfaces meeting smoothly there (a round's cylinder and the face it runs into) as
    // one, which they are to the first order.
    let mut rows: Vec<(Surface, f64)> = Vec::new();
    for &f in faces {
        let surface = solid.faces[f as usize].surface;
        let d = distances[f as usize];
        let n = normal(&surface, p);
        if !rows.iter().any(|(s, e)| {
            same_geometry(s, &surface)
                || ((normal(s, p) - n).length() < 1e-6 && (e - d).abs() <= LINEAR_TOLERANCE)
        }) {
            rows.push((surface, d));
        }
    }
    // Two surfaces meet in a curve: the vertex stays where it is across it.
    let across = match rows.as_slice() {
        [(a, _), (b, _)] => normal(a, p).cross(normal(b, p)).normalized(),
        _ => None,
    };
    let mut q = p;
    for _ in 0..40 {
        // Rows of J δ = −r.
        let mut eqs: Vec<(Vec3, f64)> = rows
            .iter()
            .map(|(s, d)| (normal(s, q), -(s.signed_distance(q) - d)))
            .collect();
        if let Some(t) = across {
            eqs.push((t, -(q - p).dot(t)));
        }
        let Some(step) = least_squares(&eqs) else {
            break;
        };
        q = q + step;
        if step.length() < 1e-14 {
            break;
        }
    }
    Ok(q)
}

/// The least-squares solution of `n_i · x = b_i`, through its normal equations.
fn least_squares(rows: &[(Vec3, f64)]) -> Option<Vec3> {
    let (mut a, mut b) = ([[0.0f64; 3]; 3], [0.0f64; 3]);
    for &(n, r) in rows {
        let n = [n.x, n.y, n.z];
        for i in 0..3 {
            for j in 0..3 {
                a[i][j] += n[i] * n[j];
            }
            b[i] += n[i] * r;
        }
    }
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if det.abs() < 1e-12 {
        // Fewer than three ways to move: the least move that meets the rows, a little
        // damped so that it is unique.
        for (i, row) in a.iter_mut().enumerate() {
            row[i] += 1e-9;
        }
        return solve_3x3(a, b);
    }
    let solve = |k: usize| {
        let mut m = a;
        for (row, &bi) in m.iter_mut().zip(&b) {
            row[k] = bi;
        }
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    Some(Vec3::new(solve(0) / det, solve(1) / det, solve(2) / det))
}

/// `x` with `a x = b`, by Cramer's rule.
fn solve_3x3(a: [[f64; 3]; 3], b: [f64; 3]) -> Option<Vec3> {
    let det3 = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let det = det3(a);
    if det.abs() < 1e-300 {
        return None;
    }
    let solve = |k: usize| {
        let mut m = a;
        for (row, &bi) in m.iter_mut().zip(&b) {
            row[k] = bi;
        }
        det3(m) / det
    };
    Some(Vec3::new(solve(0), solve(1), solve(2)))
}

/// The apex or pole of a surface of revolution nearest `p`.
fn pole_near(r: &Revolution, p: Point3) -> Option<Point3> {
    let a = r.axis.direction;
    let candidates: Vec<Point3> = match r.meridian {
        Meridian::Line { radius, slope } if slope != 0.0 => {
            vec![r.axis.origin + a * (-radius / slope)]
        }
        Meridian::Circle { center, radius } if center.abs() <= radius => {
            let h = (radius * radius - center * center).max(0.0).sqrt();
            vec![r.axis.origin + a * h, r.axis.origin - a * h]
        }
        _ => Vec::new(),
    };
    candidates
        .into_iter()
        .min_by(|x, y| x.distance(p).total_cmp(&y.distance(p)))
}

/// Whether two surfaces are one, whichever way their normals point.
fn same_geometry(a: &Surface, b: &Surface) -> bool {
    let tol = 10.0 * LINEAR_TOLERANCE;
    match (a, b) {
        (Surface::Plane(p), Surface::Plane(q)) => {
            p.normal.cross(q.normal).length() < 1e-9
                && (q.origin - p.origin).dot(p.normal).abs() <= tol
        }
        (Surface::Revolution(r), Surface::Revolution(s)) => {
            r.same_as(s, tol)
                || r.same_as(
                    &Revolution {
                        outward: !s.outward,
                        ..*s
                    },
                    tol,
                )
        }
        _ => false,
    }
}

/// The curve an edge runs along once its faces have moved, between its moved vertices.
fn moved_curve(
    solid: &Solid,
    edge: &Edge,
    faces: &[u32],
    surfaces: &[Surface],
    vertices: &[Vertex],
) -> Result<Curve, NativeError> {
    let unsupported = NativeError::Unsupported("offset an edge between these faces");
    let start = vertices[edge.start as usize].point;
    let moved: Vec<(Surface, Surface)> = faces
        .iter()
        .map(|&f| (solid.faces[f as usize].surface, surfaces[f as usize]))
        .collect();
    match edge.curve {
        Curve::Line => Ok(Curve::Line),
        Curve::Degenerate { axis, turn } => Ok(Curve::Degenerate { axis, turn }),
        Curve::Circle(c) => {
            // A parallel of a surface of revolution it lies on: at its moved vertex.
            for (old, new) in &moved {
                if let (Surface::Revolution(o), Surface::Revolution(r)) = (old, new)
                    && o.parallel_v(&c, 10.0 * LINEAR_TOLERANCE).is_some()
                {
                    let (rho, z) = r.rho_z(start);
                    if rho <= LINEAR_TOLERANCE {
                        return Err(too_thick());
                    }
                    return Ok(Curve::Circle(Circle {
                        center: r.axis.origin + r.axis.direction * z,
                        normal: c.normal,
                        radius: rho,
                    }));
                }
            }
            // A meridian circle of a sphere or a torus: about the same centre.
            for (old, new) in &moved {
                if let (Surface::Revolution(o), Surface::Revolution(r)) = (old, new)
                    && o.meridian_theta(&c, 10.0 * LINEAR_TOLERANCE).is_some()
                    && let Meridian::Circle { radius, .. } = r.meridian
                {
                    return Ok(Curve::Circle(Circle { radius, ..c }));
                }
            }
            // Where a plane cuts a sphere.
            let sphere = moved.iter().find_map(|(_, s)| match s {
                Surface::Revolution(r) => match r.meridian {
                    Meridian::Circle {
                        center: 0.0,
                        radius,
                    } => Some((r.axis.origin, radius)),
                    _ => None,
                },
                _ => None,
            });
            let plane = moved.iter().find_map(|(_, s)| s.plane().copied());
            if let (Some((o, radius)), Some(plane)) = (sphere, plane) {
                let d = (o - plane.origin).dot(plane.normal);
                if d.abs() >= radius {
                    return Err(too_thick());
                }
                return Ok(Curve::Circle(Circle {
                    center: o - plane.normal * d,
                    normal: c.normal,
                    radius: (radius * radius - d * d).sqrt(),
                }));
            }
            Err(unsupported)
        }
        Curve::Section(section) => {
            // The same section of the moved surfaces.
            let carrier = moved.iter().find_map(|(old, new)| match (old, new) {
                (Surface::Revolution(_), Surface::Revolution(r))
                    if same_geometry(old, &Surface::Revolution(section.carrier)) =>
                {
                    Some(*r)
                }
                _ => None,
            });
            let other = moved
                .iter()
                .find_map(|(old, new)| same_geometry(old, &section.other).then_some(*new));
            match (carrier, other) {
                (Some(carrier), Some(other)) => Ok(Curve::Section(Section {
                    carrier,
                    other,
                    ..section
                })),
                _ => Err(unsupported),
            }
        }
    }
}
