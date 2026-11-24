//! Oxyde's sketch geometry as solidcore records, and the extrusion's faces named after it.

use std::f64::consts::TAU;

use oxyde_geom::{Curve2, Ellipse2, Loop, Plane, Point2, Profile};
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

/// The longest piece of an ellipse one rational quadratic Bézier holds here: a third of a
/// turn, so a whole ellipse is three pieces.
const CONIC_PIECE: f64 = TAU / 3.0;

/// Part of an ellipse as exact rational quadratic Béziers (solidcore's conic segments), from
/// eccentric angle `from` turning by `sweep`, starting at `first` and ending at `last`.
///
/// A circle's arc of angle `2h` is the quadratic with end weights 1, the middle weight
/// `cos h`, and its control point where the tangents at its ends meet: the arc's middle
/// pushed out from the centre by `1 / cos h`. An ellipse is that circle stretched along its
/// axes, which carries the control point along.
fn conic_pieces(
    ellipse: &Ellipse2,
    from: f64,
    sweep: f64,
    first: Point2,
    last: Point2,
) -> Result<Vec<Seg2>, Unbuilt> {
    if !ellipse.is_valid() {
        return Err(Unbuilt::Kind);
    }
    let count = (sweep.abs() / CONIC_PIECE).ceil().max(1.0);
    let step = sweep / count;
    let w = (step / 2.0).cos();
    let c = ellipse.center;
    let pieces = count as usize;
    let mut out = Vec::with_capacity(pieces);
    let mut a = first;
    for i in 0..pieces {
        let t = from + step * i as f64;
        let b = if i + 1 == pieces {
            last
        } else {
            ellipse.at(t + step)
        };
        let mid = ellipse.at(t + step / 2.0);
        let apex = Point2::new(c.x + (mid.x - c.x) / w, c.y + (mid.y - c.y) / w);
        out.push(Seg2::Conic {
            a: v2(a),
            apex: v2(apex),
            b: v2(b),
            w,
        });
        a = b;
    }
    Ok(out)
}

/// One sketch curve as solidcore segments; arcs run from `start` through `mid` to `end`.
fn segment(curve: &Curve2) -> Result<Vec<Seg2>, Unbuilt> {
    match *curve {
        Curve2::Line { start, end } => Ok(vec![Seg2::Line {
            a: v2(start),
            b: v2(end),
        }]),
        Curve2::Ellipse(ellipse) => {
            let first = ellipse.at(0.0);
            conic_pieces(&ellipse, 0.0, TAU, first, first)
        }
        Curve2::EllipticArc {
            ellipse,
            start,
            mid,
            end,
        } => {
            let (a0, am, a1) = (
                ellipse.angle_of(start),
                ellipse.angle_of(mid),
                ellipse.angle_of(end),
            );
            let ccw = (a1 - a0).rem_euclid(TAU);
            let through = (am - a0).rem_euclid(TAU);
            let sweep = if through <= ccw { ccw } else { ccw - TAU };
            conic_pieces(&ellipse, a0, sweep, start, end)
        }
        Curve2::Arc { start, mid, end } => {
            let (center, radius) = circle_through(start, mid, end).ok_or(Unbuilt::NoCircle)?;
            let tau = std::f64::consts::TAU;
            let (a0, am, a1) = (angle(center, start), angle(center, mid), angle(center, end));
            // Counter-clockwise from start to end passes the middle, or the arc runs clockwise.
            let ccw = (a1 - a0).rem_euclid(tau);
            let through = (am - a0).rem_euclid(tau);
            let sweep = if through <= ccw { ccw } else { ccw - tau };
            Ok(vec![Seg2::Arc {
                center: v2(center),
                radius,
                start: a0,
                sweep,
            }])
        }
        ref other => whole_circle(other).map(|s| vec![s]),
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
    let mut segs = Vec::new();
    for s in &l.segments {
        segs.extend(segment(&s.curve)?);
    }
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
            Curve2::Ellipse(ellipse) | Curve2::EllipticArc { ellipse, .. } => {
                ellipse.at(ellipse.angle_of(m)).distance(m) < ON
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
        let built = segment(&Curve2::Arc {
            start: a,
            mid: top,
            end: b,
        });
        let Ok(&[Seg2::Arc { sweep, radius, .. }]) = built.as_deref() else {
            panic!("an arc");
        };
        assert!((radius - 1.0).abs() < 1e-12);
        assert!((sweep - std::f64::consts::PI).abs() < 1e-12);
        let built = segment(&Curve2::Arc {
            start: a,
            mid: Point2::new(0.0, -1.0),
            end: b,
        });
        let Ok(&[Seg2::Arc { sweep, .. }]) = built.as_deref() else {
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

    /// The point of a rational quadratic Bézier at `s`.
    fn conic_at(seg: &Seg2, s: f64) -> Point2 {
        let Seg2::Conic { a, apex, b, w } = *seg else {
            panic!("not a conic: {seg:?}");
        };
        let (k0, k1, k2) = ((1.0 - s) * (1.0 - s), 2.0 * s * (1.0 - s) * w, s * s);
        let sum = k0 + k1 + k2;
        Point2::new(
            (k0 * a.x + k1 * apex.x + k2 * b.x) / sum,
            (k0 * a.y + k1 * apex.y + k2 * b.y) / sum,
        )
    }

    /// How far a point of a conic piece may lie from the ellipse it was made from: rounding
    /// only.
    const EXACT: f64 = 1e-12;

    #[test]
    fn ellipses_become_exact_conic_pieces() {
        let ellipse = Ellipse2 {
            center: Point2::new(1.0, -2.0),
            major: Point2::new(4.0, 2.0),
            minor: 2.0,
        };
        let on = |p: Point2| ellipse.at(ellipse.angle_of(p)).distance(p);
        let whole = segment(&Curve2::Ellipse(ellipse)).unwrap();
        assert_eq!(whole.len(), 3);
        for seg in &whole {
            for k in 0..=10 {
                let p = conic_at(seg, k as f64 / 10.0);
                assert!(on(p) < EXACT, "{p:?}");
            }
        }
        // Closed: the last piece ends where the first starts.
        assert_eq!(conic_at(&whole[2], 1.0), conic_at(&whole[0], 0.0));
        // A quarter, clockwise from the major point through the minor axis's far side.
        let (start, end) = (ellipse.at(0.0), ellipse.at(-std::f64::consts::FRAC_PI_2));
        let arc = segment(&Curve2::EllipticArc {
            ellipse,
            start,
            mid: ellipse.at(-std::f64::consts::FRAC_PI_4),
            end,
        })
        .unwrap();
        assert_eq!(arc.len(), 1);
        assert_eq!(conic_at(&arc[0], 0.0), start);
        assert_eq!(conic_at(&arc[0], 1.0), end);
        let middle = conic_at(&arc[0], 0.5);
        assert!(on(middle) < EXACT);
        assert!(middle.distance(ellipse.at(-std::f64::consts::FRAC_PI_4)) < EXACT);
    }
}
