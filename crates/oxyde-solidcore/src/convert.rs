//! Oxyde's sketch geometry as solidcore records, and the extrusion's faces named after it.

use oxyde_geom::{Curve2, Loop, Plane, Point2, Profile};
use oxyde_kernel::FaceSource;
use solidcore_record::brep::{Brep as Record, Loop as RecordLoop, Surface};
use solidcore_record::{Loop2, Plane as RecordPlane, Region2, Seg2, Vec2, Vec3 as RecordVec3};

/// Two points closer than this, or a point this close to a surface, are on it (mm).
const ON: f64 = 1e-6;

fn v3(x: f64, y: f64, z: f64) -> RecordVec3 {
    RecordVec3 { x, y, z }
}

fn v2(p: Point2) -> Vec2 {
    Vec2 { x: p.x, y: p.y }
}

/// The sketch plane as solidcore's: its origin and its two in-plane directions.
pub(crate) fn plane(p: &Plane) -> RecordPlane {
    let y = p.y_dir();
    RecordPlane {
        origin: v3(p.origin.x, p.origin.y, p.origin.z),
        x: v3(p.x_dir.x, p.x_dir.y, p.x_dir.z),
        y: v3(y.x, y.y, y.z),
    }
}

/// The circle through three points: its centre and radius, or none when they are in line.
fn circle_through(a: Point2, b: Point2, c: Point2) -> Option<(Point2, f64)> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < 1e-12 {
        return None;
    }
    let sq = |p: Point2| p.x * p.x + p.y * p.y;
    let ux = (sq(a) * (b.y - c.y) + sq(b) * (c.y - a.y) + sq(c) * (a.y - b.y)) / d;
    let uy = (sq(a) * (c.x - b.x) + sq(b) * (a.x - c.x) + sq(c) * (b.x - a.x)) / d;
    let center = Point2::new(ux, uy);
    Some((center, center.distance(a)))
}

/// The angle of `p` seen from `center`.
fn angle(center: Point2, p: Point2) -> f64 {
    (p.y - center.y).atan2(p.x - center.x)
}

/// Why a profile curve has no solidcore segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unbuilt {
    /// An arc whose three points are in line.
    NoCircle,
    /// A kind of curve the adapter does not give solidcore.
    Kind,
}

/// One sketch curve as a solidcore segment; arcs run from `start` through `mid` to `end`.
fn segment(curve: &Curve2) -> Result<Seg2, Unbuilt> {
    match *curve {
        Curve2::Line { start, end } => Ok(Seg2::Line {
            a: v2(start),
            b: v2(end),
        }),
        Curve2::Arc { start, mid, end } => {
            let (center, radius) = circle_through(start, mid, end).ok_or(Unbuilt::NoCircle)?;
            let tau = std::f64::consts::TAU;
            let (a0, am, a1) = (angle(center, start), angle(center, mid), angle(center, end));
            // Counter-clockwise from start to end passes the middle, or the arc runs clockwise.
            let ccw = (a1 - a0).rem_euclid(tau);
            let through = (am - a0).rem_euclid(tau);
            let sweep = if through <= ccw { ccw } else { ccw - tau };
            Ok(Seg2::Arc {
                center: v2(center),
                radius,
                start: a0,
                sweep,
            })
        }
        ref other => whole_circle(other),
    }
}

/// A full circle as a solidcore segment. Every other curve, a kind `Curve2` gained after the
/// adapter was written included, is not built.
fn whole_circle(curve: &Curve2) -> Result<Seg2, Unbuilt> {
    if let Curve2::Circle { center, radius } = *curve {
        Ok(Seg2::Arc {
            center: v2(center),
            radius,
            start: 0.0,
            sweep: std::f64::consts::TAU,
        })
    } else {
        Err(Unbuilt::Kind)
    }
}

fn loop2(l: &Loop) -> Result<Loop2, Unbuilt> {
    let segs = l
        .segments
        .iter()
        .map(|s| segment(&s.curve))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Loop2 { segs })
}

/// The profile's regions as solidcore's.
pub(crate) fn regions(profile: &Profile) -> Result<Vec<Region2>, Unbuilt> {
    profile
        .regions
        .iter()
        .map(|r| {
            Ok(Region2 {
                outer: loop2(&r.outer)?,
                holes: r.holes.iter().map(loop2).collect::<Result<Vec<_>, _>>()?,
            })
        })
        .collect()
}

pub(crate) type P3 = [f64; 3];

fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: P3) -> f64 {
    dot(a, a).sqrt()
}

/// What a face of the extrusion of `profile` on `p`, from `start` to `end` along its normal,
/// was made from: a cap where it is a plane parallel to the sketch at `start` or `end`, else
/// the profile segment it was swept from, found by the face's edge in the plane at `start`:
/// that edge's middle (`edge_middle`, by record edge index) lies on the segment. Matching by
/// the edge holds whatever surface the side has (solidcore makes round sides as rational
/// splines, in two halves for a whole circle). None for a face nothing matches.
pub(crate) fn extrusion_source(
    record: &Record,
    face: usize,
    p: &Plane,
    profile: &Profile,
    start: f64,
    end: f64,
    edge_middle: &dyn Fn(usize) -> Option<P3>,
) -> Option<FaceSource> {
    let f = record.faces.get(face)?;
    let n = [p.normal.x, p.normal.y, p.normal.z];
    let o = [p.origin.x, p.origin.y, p.origin.z];
    let y = p.y_dir();
    let height = |q: P3| dot(sub(q, o), n);
    if let Surface::Plane {
        origin,
        u_axis,
        v_axis,
    } = &f.surface
    {
        let normal = cross(*u_axis, *v_axis);
        let len = length(normal);
        if len > 0.0
            && length(cross(
                [normal[0] / len, normal[1] / len, normal[2] / len],
                n,
            )) < 1e-9
        {
            let h = height(*origin);
            if (h - start).abs() < ON {
                return Some(FaceSource::StartCap);
            }
            if (h - end).abs() < ON {
                return Some(FaceSource::EndCap);
            }
            return None;
        }
    }
    // A side: its edge lying in the start plane, seen in sketch coordinates.
    let in_sketch = |q: P3| {
        let d = sub(q, o);
        Point2::new(
            d[0] * p.x_dir.x + d[1] * p.x_dir.y + d[2] * p.x_dir.z,
            d[0] * y.x + d[1] * y.y + d[2] * y.z,
        )
    };
    let bottom = f.loops.iter().find_map(|l| match l {
        RecordLoop::Edges { edges } => edges.iter().find_map(|e| {
            let edge = record.edges.get(e.edge as usize)?;
            let [a, b] = edge.vertices;
            let at = |v: u32| record.vertices.get(v as usize).map(|v| height(v.point));
            let flat = (at(a)? - start).abs() < ON && (at(b)? - start).abs() < ON;
            flat.then(|| edge_middle(e.edge as usize)).flatten()
        }),
        RecordLoop::Vertex { .. } => None,
    })?;
    let m = in_sketch(bottom);
    profile_segments(profile).find_map(|(id, curve)| {
        let on = match *curve {
            Curve2::Line { start: a, end: b } => {
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let len2 = dx * dx + dy * dy;
                if len2 <= 0.0 {
                    return None;
                }
                let t = ((m.x - a.x) * dx + (m.y - a.y) * dy) / len2;
                let off = Point2::new(a.x + t * dx, a.y + t * dy).distance(m);
                (-1e-9..=1.0 + 1e-9).contains(&t) && off < ON
            }
            Curve2::Arc { start, mid, end } => {
                let (center, radius) = circle_through(start, mid, end)?;
                (center.distance(m) - radius).abs() < ON
            }
            ref other => on_circle(other, m),
        };
        on.then_some(FaceSource::ProfileSegment(id))
    })
}

/// Whether `m` lies on `curve` when it is a full circle; on no other curve, since the
/// adapter builds no other kind of whole curve.
fn on_circle(curve: &Curve2, m: Point2) -> bool {
    if let Curve2::Circle { center, radius } = *curve {
        (center.distance(m) - radius).abs() < ON
    } else {
        false
    }
}

/// Every segment of the profile with its id.
fn profile_segments(profile: &Profile) -> impl Iterator<Item = (u32, &Curve2)> {
    profile.regions.iter().flat_map(|r| {
        std::iter::once(&r.outer)
            .chain(r.holes.iter())
            .flat_map(|l| l.segments.iter().map(|s| (s.id, &s.curve)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arcs_keep_their_turning_direction() {
        let a = Point2::new(1.0, 0.0);
        let top = Point2::new(0.0, 1.0);
        let b = Point2::new(-1.0, 0.0);
        let Ok(Seg2::Arc { sweep, radius, .. }) = segment(&Curve2::Arc {
            start: a,
            mid: top,
            end: b,
        }) else {
            panic!("an arc");
        };
        assert!((radius - 1.0).abs() < 1e-12);
        assert!((sweep - std::f64::consts::PI).abs() < 1e-12);
        let Ok(Seg2::Arc { sweep, .. }) = segment(&Curve2::Arc {
            start: a,
            mid: Point2::new(0.0, -1.0),
            end: b,
        }) else {
            panic!("an arc");
        };
        assert!((sweep + std::f64::consts::PI).abs() < 1e-12);
        assert!(circle_through(a, Point2::new(2.0, 0.0), Point2::new(3.0, 0.0)).is_none());
        assert!(matches!(
            segment(&Curve2::Arc {
                start: a,
                mid: Point2::new(2.0, 0.0),
                end: Point2::new(3.0, 0.0),
            }),
            Err(Unbuilt::NoCircle)
        ));
    }

    #[test]
    fn a_curve_other_than_a_circle_reaching_the_fallback_is_not_built() {
        let line = Curve2::Line {
            start: Point2::new(0.0, 0.0),
            end: Point2::new(1.0, 0.0),
        };
        assert!(matches!(whole_circle(&line), Err(Unbuilt::Kind)));
        assert!(!on_circle(&line, Point2::new(0.5, 0.0)));
        let circle = Curve2::Circle {
            center: Point2::new(0.0, 0.0),
            radius: 2.0,
        };
        assert!(whole_circle(&circle).is_ok());
        assert!(on_circle(&circle, Point2::new(0.0, 2.0)));
    }
}
