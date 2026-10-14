//! Closed sketch profiles: the 2D input of extrusions and cuts.
//!
//! [`Profile::from_curves`] chains loose sketch curves into closed loops, then sorts the loops
//! into regions (an outer boundary with its holes), the way SolidWorks turns a sketch into
//! something it can extrude.

use std::f64::consts::TAU;
use std::fmt;

use crate::{LINEAR_TOLERANCE, Point2};

/// A 2D sketch curve in plane coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve2 {
    Line {
        start: Point2,
        end: Point2,
    },
    /// Circular arc from `start` through `mid` to `end`.
    Arc {
        start: Point2,
        mid: Point2,
        end: Point2,
    },
    Circle {
        center: Point2,
        radius: f64,
    },
}

impl Curve2 {
    /// Start and end points, or `None` for a full circle.
    pub fn endpoints(&self) -> Option<(Point2, Point2)> {
        match *self {
            Curve2::Line { start, end } | Curve2::Arc { start, end, .. } => Some((start, end)),
            Curve2::Circle { .. } => None,
        }
    }

    /// The same curve traversed the other way.
    pub fn reversed(&self) -> Curve2 {
        match *self {
            Curve2::Line { start, end } => Curve2::Line {
                start: end,
                end: start,
            },
            Curve2::Arc { start, mid, end } => Curve2::Arc {
                start: end,
                mid,
                end: start,
            },
            circle @ Curve2::Circle { .. } => circle,
        }
    }

    /// Appends points along the curve, from its start up to (excluding) its end.
    fn sample_into(&self, out: &mut Vec<Point2>) {
        const ARC_SAMPLES: usize = 16;
        const CIRCLE_SAMPLES: usize = 32;
        match *self {
            Curve2::Line { start, .. } => out.push(start),
            Curve2::Arc { start, mid, end } => match circle_through(start, mid, end) {
                Some((center, radius)) => {
                    let a0 = angle(center, start);
                    let sweep = arc_sweep(a0, angle(center, mid), angle(center, end));
                    for k in 0..ARC_SAMPLES {
                        let a = a0 + sweep * k as f64 / ARC_SAMPLES as f64;
                        out.push(Point2::new(
                            center.x + radius * a.cos(),
                            center.y + radius * a.sin(),
                        ));
                    }
                }
                None => {
                    out.push(start);
                    out.push(mid);
                }
            },
            Curve2::Circle { center, radius } => {
                for k in 0..CIRCLE_SAMPLES {
                    let a = TAU * k as f64 / CIRCLE_SAMPLES as f64;
                    out.push(Point2::new(
                        center.x + radius * a.cos(),
                        center.y + radius * a.sin(),
                    ));
                }
            }
        }
    }
}

fn angle(center: Point2, p: Point2) -> f64 {
    (p.y - center.y).atan2(p.x - center.x)
}

/// Signed sweep from angle `a0` to `a1` passing through `am` (positive = counter-clockwise).
fn arc_sweep(a0: f64, am: f64, a1: f64) -> f64 {
    let ccw_sweep = (a1 - a0).rem_euclid(TAU);
    let ccw_mid = (am - a0).rem_euclid(TAU);
    if ccw_mid < ccw_sweep {
        ccw_sweep
    } else {
        ccw_sweep - TAU
    }
}

/// Center and radius of the circle through three points, or `None` if they are collinear.
fn circle_through(a: Point2, b: Point2, c: Point2) -> Option<(Point2, f64)> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < LINEAR_TOLERANCE * LINEAR_TOLERANCE {
        return None;
    }
    let a2 = a.x * a.x + a.y * a.y;
    let b2 = b.x * b.x + b.y * b.y;
    let c2 = c.x * c.x + c.y * c.y;
    let center = Point2::new(
        (a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d,
        (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d,
    );
    Some((center, center.distance(a)))
}

/// A sketch curve tagged with the id of the sketch entity it comes from.
///
/// The id is what lets the modeling engine name the faces an extrusion generates
/// ("the side face made by sketch line 3") independently of kernel numbering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfileSegment {
    pub id: u32,
    pub curve: Curve2,
}

/// A closed chain of segments: each segment ends where the next one starts, and the last
/// ends where the first starts. A full circle is a loop on its own. Orientation is arbitrary.
#[derive(Debug, Clone, PartialEq)]
pub struct Loop {
    pub segments: Vec<ProfileSegment>,
}

impl Loop {
    /// Closed polyline approximating the loop (last point not repeated).
    pub fn polygon(&self) -> Vec<Point2> {
        let mut points = Vec::new();
        for segment in &self.segments {
            segment.curve.sample_into(&mut points);
        }
        points
    }

    /// Enclosed area, positive when the loop runs counter-clockwise.
    pub fn signed_area(&self) -> f64 {
        polygon_area(&self.polygon())
    }
}

/// A face to extrude: one outer boundary and the holes directly inside it.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub outer: Loop,
    pub holes: Vec<Loop>,
}

/// All the extrudable regions of a sketch.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Profile {
    pub regions: Vec<Region>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileError {
    /// A segment end is not connected to any other segment.
    OpenChain { segment: u32 },
    /// More than two segment ends meet at one point.
    Branching { segment: u32 },
    /// A line of zero length or an arc whose three points are collinear or coincident.
    Degenerate { segment: u32 },
    /// A circle with a non-positive radius.
    InvalidCircle { segment: u32 },
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileError::OpenChain { segment } => {
                write!(f, "sketch is not closed: segment {segment} has a free end")
            }
            ProfileError::Branching { segment } => write!(
                f,
                "sketch contour branches: more than two segments meet at an end of segment {segment}"
            ),
            ProfileError::Degenerate { segment } => {
                write!(f, "segment {segment} is degenerate")
            }
            ProfileError::InvalidCircle { segment } => {
                write!(f, "circle {segment} has an invalid radius")
            }
        }
    }
}

impl std::error::Error for ProfileError {}

impl Profile {
    /// Builds the profile from sketch curves given in any order and orientation.
    pub fn from_curves(curves: &[ProfileSegment]) -> Result<Profile, ProfileError> {
        let loops = chain_loops(curves)?;
        Ok(Profile {
            regions: nest_loops(loops),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

fn near(a: Point2, b: Point2) -> bool {
    a.distance(b) <= LINEAR_TOLERANCE
}

fn chain_loops(curves: &[ProfileSegment]) -> Result<Vec<Loop>, ProfileError> {
    let mut loops = Vec::new();
    let mut open: Vec<ProfileSegment> = Vec::new();

    for segment in curves {
        match segment.curve {
            Curve2::Circle { radius, .. } => {
                if !(radius > LINEAR_TOLERANCE) {
                    return Err(ProfileError::InvalidCircle {
                        segment: segment.id,
                    });
                }
                loops.push(Loop {
                    segments: vec![*segment],
                });
            }
            Curve2::Line { start, end } => {
                if near(start, end) {
                    return Err(ProfileError::Degenerate {
                        segment: segment.id,
                    });
                }
                open.push(*segment);
            }
            Curve2::Arc { start, mid, end } => {
                if near(start, end) || circle_through(start, mid, end).is_none() {
                    return Err(ProfileError::Degenerate {
                        segment: segment.id,
                    });
                }
                open.push(*segment);
            }
        }
    }

    let ends: Vec<(Point2, Point2)> = open
        .iter()
        .map(|s| s.curve.endpoints().expect("open segments have endpoints"))
        .collect();

    // Every end must meet exactly one other end.
    for (i, &(a, b)) in ends.iter().enumerate() {
        for p in [a, b] {
            let meetings = ends
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, &(c, d))| usize::from(near(p, c)) + usize::from(near(p, d)))
                .sum::<usize>();
            match meetings {
                0 => {
                    return Err(ProfileError::OpenChain {
                        segment: open[i].id,
                    });
                }
                1 => {}
                _ => {
                    return Err(ProfileError::Branching {
                        segment: open[i].id,
                    });
                }
            }
        }
    }

    let mut used = vec![false; open.len()];
    for first in 0..open.len() {
        if used[first] {
            continue;
        }
        used[first] = true;
        let (loop_start, mut cursor) = ends[first];
        let mut segments = vec![open[first]];

        while !near(cursor, loop_start) {
            let next = (0..open.len())
                .find(|&j| !used[j] && (near(ends[j].0, cursor) || near(ends[j].1, cursor)));
            let Some(j) = next else {
                let last = segments.last().map_or(open[first].id, |s| s.id);
                return Err(ProfileError::OpenChain { segment: last });
            };
            used[j] = true;
            let segment = if near(ends[j].0, cursor) {
                open[j]
            } else {
                ProfileSegment {
                    id: open[j].id,
                    curve: open[j].curve.reversed(),
                }
            };
            cursor = segment.curve.endpoints().expect("open segment").1;
            segments.push(segment);
        }
        loops.push(Loop { segments });
    }
    Ok(loops)
}

/// Sorts loops into regions: loops nested an even number of times are outer boundaries,
/// the others are holes of the smallest loop containing them.
fn nest_loops(loops: Vec<Loop>) -> Vec<Region> {
    let polygons: Vec<Vec<Point2>> = loops.iter().map(Loop::polygon).collect();
    let areas: Vec<f64> = polygons.iter().map(|p| polygon_area(p).abs()).collect();

    let n = loops.len();
    let mut depth = vec![0usize; n];
    let mut parent: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let probe = polygons[i][0];
        for j in 0..n {
            if i != j && areas[j] > areas[i] && point_in_polygon(probe, &polygons[j]) {
                depth[i] += 1;
                if parent[i].is_none_or(|p| areas[j] < areas[p]) {
                    parent[i] = Some(j);
                }
            }
        }
    }

    let mut regions = Vec::new();
    for i in 0..n {
        if depth[i] % 2 != 0 {
            continue;
        }
        let holes = (0..n)
            .filter(|&k| depth[k] == depth[i] + 1 && parent[k] == Some(i))
            .map(|k| loops[k].clone())
            .collect();
        regions.push(Region {
            outer: loops[i].clone(),
            holes,
        });
    }
    regions
}

fn polygon_area(points: &[Point2]) -> f64 {
    let n = points.len();
    let mut twice = 0.0;
    for i in 0..n {
        let (a, b) = (points[i], points[(i + 1) % n]);
        twice += a.x * b.y - b.x * a.y;
    }
    twice / 2.0
}

fn point_in_polygon(p: Point2, polygon: &[Point2]) -> bool {
    let n = polygon.len();
    let mut inside = false;
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (a, b) = (polygon[i], polygon[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: u32, x0: f64, y0: f64, x1: f64, y1: f64) -> ProfileSegment {
        ProfileSegment {
            id,
            curve: Curve2::Line {
                start: Point2::new(x0, y0),
                end: Point2::new(x1, y1),
            },
        }
    }

    fn circle(id: u32, x: f64, y: f64, radius: f64) -> ProfileSegment {
        ProfileSegment {
            id,
            curve: Curve2::Circle {
                center: Point2::new(x, y),
                radius,
            },
        }
    }

    fn rectangle(first_id: u32, x: f64, y: f64, w: f64, h: f64) -> Vec<ProfileSegment> {
        vec![
            line(first_id, x, y, x + w, y),
            line(first_id + 1, x + w, y, x + w, y + h),
            line(first_id + 2, x + w, y + h, x, y + h),
            line(first_id + 3, x, y + h, x, y),
        ]
    }

    #[test]
    fn rectangle_is_one_region() {
        let profile = Profile::from_curves(&rectangle(1, 0.0, 0.0, 40.0, 20.0)).unwrap();
        assert_eq!(profile.regions.len(), 1);
        assert!(profile.regions[0].holes.is_empty());
        assert_eq!(profile.regions[0].outer.segments.len(), 4);
        assert!((profile.regions[0].outer.signed_area().abs() - 800.0).abs() < 1e-9);
    }

    #[test]
    fn shuffled_and_reversed_segments_still_chain() {
        let mut curves = rectangle(1, 0.0, 0.0, 10.0, 10.0);
        curves.swap(0, 2);
        curves[1] = ProfileSegment {
            id: curves[1].id,
            curve: curves[1].curve.reversed(),
        };
        let profile = Profile::from_curves(&curves).unwrap();
        let outer = &profile.regions[0].outer;
        for k in 0..outer.segments.len() {
            let end = outer.segments[k].curve.endpoints().unwrap().1;
            let next_start = outer.segments[(k + 1) % outer.segments.len()]
                .curve
                .endpoints()
                .unwrap()
                .0;
            assert!(near(end, next_start));
        }
    }

    #[test]
    fn circle_inside_rectangle_is_a_hole() {
        let mut curves = rectangle(1, 0.0, 0.0, 40.0, 20.0);
        curves.push(circle(5, 20.0, 10.0, 3.0));
        let profile = Profile::from_curves(&curves).unwrap();
        assert_eq!(profile.regions.len(), 1);
        assert_eq!(profile.regions[0].holes.len(), 1);
        assert_eq!(profile.regions[0].holes[0].segments[0].id, 5);
    }

    #[test]
    fn separate_rectangles_are_separate_regions() {
        let mut curves = rectangle(1, 0.0, 0.0, 10.0, 10.0);
        curves.extend(rectangle(5, 20.0, 0.0, 10.0, 10.0));
        let profile = Profile::from_curves(&curves).unwrap();
        assert_eq!(profile.regions.len(), 2);
    }

    #[test]
    fn island_inside_a_hole_is_its_own_region() {
        let mut curves = rectangle(1, 0.0, 0.0, 100.0, 100.0);
        curves.extend(rectangle(5, 10.0, 10.0, 80.0, 80.0));
        curves.extend(rectangle(9, 40.0, 40.0, 20.0, 20.0));
        let profile = Profile::from_curves(&curves).unwrap();
        assert_eq!(profile.regions.len(), 2);
        assert_eq!(profile.regions[0].holes.len(), 1);
        assert!(profile.regions[1].holes.is_empty());
    }

    #[test]
    fn slot_made_of_lines_and_arcs() {
        let curves = vec![
            line(1, 0.0, 0.0, 10.0, 0.0),
            ProfileSegment {
                id: 2,
                curve: Curve2::Arc {
                    start: Point2::new(10.0, 0.0),
                    mid: Point2::new(15.0, 5.0),
                    end: Point2::new(10.0, 10.0),
                },
            },
            line(3, 10.0, 10.0, 0.0, 10.0),
            ProfileSegment {
                id: 4,
                curve: Curve2::Arc {
                    start: Point2::new(0.0, 10.0),
                    mid: Point2::new(-5.0, 5.0),
                    end: Point2::new(0.0, 0.0),
                },
            },
        ];
        let profile = Profile::from_curves(&curves).unwrap();
        let area = profile.regions[0].outer.signed_area().abs();
        let exact = 100.0 + std::f64::consts::PI * 25.0;
        assert!((area - exact).abs() / exact < 0.01, "area {area}");
    }

    #[test]
    fn open_chain_is_rejected() {
        let mut curves = rectangle(1, 0.0, 0.0, 10.0, 10.0);
        curves.pop();
        assert!(matches!(
            Profile::from_curves(&curves),
            Err(ProfileError::OpenChain { .. })
        ));
    }

    #[test]
    fn branching_is_rejected() {
        let mut curves = rectangle(1, 0.0, 0.0, 10.0, 10.0);
        curves.push(line(5, 0.0, 0.0, 10.0, 10.0));
        assert!(matches!(
            Profile::from_curves(&curves),
            Err(ProfileError::Branching { .. })
        ));
    }

    #[test]
    fn degenerate_segments_are_rejected() {
        assert!(matches!(
            Profile::from_curves(&[line(1, 1.0, 1.0, 1.0, 1.0)]),
            Err(ProfileError::Degenerate { segment: 1 })
        ));
        assert!(matches!(
            Profile::from_curves(&[circle(2, 0.0, 0.0, 0.0)]),
            Err(ProfileError::InvalidCircle { segment: 2 })
        ));
    }

    #[test]
    fn arc_sweep_direction() {
        let cw = arc_sweep(0.0, -TAU / 4.0, TAU / 2.0);
        let ccw = arc_sweep(0.0, TAU / 4.0, TAU / 2.0);
        assert!(cw < 0.0 && ccw > 0.0);
    }
}
