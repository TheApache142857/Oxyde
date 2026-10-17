//! Open or closed sketch paths: the 2D input of sweeps.
//!
//! [`Path::from_curves`] chains loose sketch curves into one chain, each curve starting where
//! the one before ends, the way SolidWorks takes a sketch as the path of a sweep: a path is
//! a single chain, open (a line, a line and an arc…) or closed (a full circle, a rounded
//! rectangle). A sketch with two chains, a branching chain or a loose segment is refused.

use std::fmt;

use crate::profile::{Curve2, ProfileSegment};
use crate::{LINEAR_TOLERANCE, Point2};

/// One connected chain of sketch curves, in order, each starting where the previous one
/// ends. A closed path's last curve ends where its first one starts (a full circle is a
/// closed path of one curve).
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub segments: Vec<ProfileSegment>,
    pub closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    /// The sketch has no curve at all.
    Empty,
    /// The curves make more than one chain (or a chain and a circle).
    Disconnected { a: u32, b: u32 },
    /// More than two curve ends meet at one point.
    Branching { segment: u32 },
    /// A line of zero length or an arc whose three points are collinear or coincident.
    Degenerate { segment: u32 },
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::Empty => write!(f, "the path sketch has no curve"),
            PathError::Disconnected { a, b } => write!(
                f,
                "the path must be one chain of connected curves: segments {a} and {b} are not connected"
            ),
            PathError::Branching { segment } => write!(
                f,
                "the path branches: more than two curves meet at an end of segment {segment}"
            ),
            PathError::Degenerate { segment } => write!(f, "segment {segment} is degenerate"),
        }
    }
}

impl std::error::Error for PathError {}

fn near(a: Point2, b: Point2) -> bool {
    a.distance(b) <= LINEAR_TOLERANCE
}

impl Path {
    /// Chains sketch curves given in any order and orientation into one path. Curves are
    /// reversed where needed so that each starts where the one before ends.
    pub fn from_curves(curves: &[ProfileSegment]) -> Result<Path, PathError> {
        let Some(first) = curves.first() else {
            return Err(PathError::Empty);
        };
        // A full circle is a path on its own; anything else with it is disconnected.
        if let Curve2::Circle { radius, .. } = first.curve {
            if radius.is_nan() || radius <= LINEAR_TOLERANCE {
                return Err(PathError::Degenerate { segment: first.id });
            }
            if let Some(other) = curves.get(1) {
                return Err(PathError::Disconnected {
                    a: first.id,
                    b: other.id,
                });
            }
            return Ok(Path {
                segments: vec![*first],
                closed: true,
            });
        }
        let mut ends = Vec::with_capacity(curves.len());
        for segment in curves {
            let (start, end) = match segment.curve {
                Curve2::Circle { .. } => {
                    return Err(PathError::Disconnected {
                        a: first.id,
                        b: segment.id,
                    });
                }
                Curve2::Line { start, end } | Curve2::Arc { start, end, .. } => (start, end),
            };
            if near(start, end) {
                return Err(PathError::Degenerate {
                    segment: segment.id,
                });
            }
            ends.push((start, end));
        }
        // Every end meets at most one other end; the chain starts at a free end if there
        // is one (an open path), else anywhere (a closed one).
        let mut start_at = None;
        for (i, &(a, b)) in ends.iter().enumerate() {
            for p in [a, b] {
                let meetings = ends
                    .iter()
                    .enumerate()
                    .filter(|&(j, _)| j != i)
                    .map(|(_, &(c, d))| usize::from(near(p, c)) + usize::from(near(p, d)))
                    .sum::<usize>();
                match meetings {
                    0 if start_at.is_none() => start_at = Some((i, p)),
                    0 | 1 => {}
                    _ => {
                        return Err(PathError::Branching {
                            segment: curves.get(i).map_or(0, |s| s.id),
                        });
                    }
                }
            }
        }
        let (first_index, free_end) = match start_at {
            Some(found) => found,
            // A closed chain: start with the first curve, from its own start.
            None => (0, ends.first().map_or(Point2::ORIGIN, |e| e.0)),
        };
        let mut used = vec![false; curves.len()];
        let mut segments = Vec::with_capacity(curves.len());
        // The first curve runs away from the free end.
        let oriented = |j: usize, from: Point2| -> ProfileSegment {
            let segment = curves.get(j).copied().unwrap_or(*first);
            if near(ends.get(j).map_or(from, |e| e.0), from) {
                segment
            } else {
                ProfileSegment {
                    id: segment.id,
                    curve: segment.curve.reversed(),
                }
            }
        };
        if let Some(u) = used.get_mut(first_index) {
            *u = true;
        }
        let mut cursor = free_end;
        let segment = oriented(first_index, cursor);
        cursor = segment.curve.endpoints().map_or(cursor, |e| e.1);
        segments.push(segment);
        let path_start = segment.curve.endpoints().map_or(cursor, |e| e.0);
        loop {
            let next = (0..curves.len()).find(|&j| {
                !used.get(j).copied().unwrap_or(true)
                    && ends
                        .get(j)
                        .is_some_and(|e| near(e.0, cursor) || near(e.1, cursor))
            });
            let Some(j) = next else {
                break;
            };
            if let Some(u) = used.get_mut(j) {
                *u = true;
            }
            let segment = oriented(j, cursor);
            cursor = segment.curve.endpoints().map_or(cursor, |e| e.1);
            segments.push(segment);
        }
        if let Some(left) = used.iter().position(|u| !u) {
            return Err(PathError::Disconnected {
                a: segments.last().map_or(first.id, |s| s.id),
                b: curves.get(left).map_or(first.id, |s| s.id),
            });
        }
        let closed = segments.len() > 1 && near(cursor, path_start);
        Ok(Path { segments, closed })
    }

    /// The curves of the path, in order and oriented.
    pub fn curves(&self) -> impl Iterator<Item = Curve2> + '_ {
        self.segments.iter().map(|s| s.curve)
    }

    /// Where the path starts (any point of a closed round path).
    pub fn start(&self) -> Option<Point2> {
        let first = self.segments.first()?;
        match first.curve {
            Curve2::Circle { center, radius } => Some(Point2::new(center.x + radius, center.y)),
            other => other.endpoints().map(|e| e.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: u32, a: (f64, f64), b: (f64, f64)) -> ProfileSegment {
        ProfileSegment {
            id,
            curve: Curve2::Line {
                start: Point2::new(a.0, a.1),
                end: Point2::new(b.0, b.1),
            },
        }
    }

    #[test]
    fn an_open_chain_is_ordered_and_oriented_from_its_free_end() {
        // Given backwards and out of order: (0,0) → (10,0) → (10,10) → (20,10).
        let curves = [
            line(2, (10.0, 10.0), (10.0, 0.0)),
            line(3, (20.0, 10.0), (10.0, 10.0)),
            line(1, (0.0, 0.0), (10.0, 0.0)),
        ];
        let path = Path::from_curves(&curves).unwrap();
        assert!(!path.closed);
        let ids: Vec<u32> = path.segments.iter().map(|s| s.id).collect();
        assert!(ids == [1, 2, 3] || ids == [3, 2, 1], "{ids:?}");
        let mut cursor = None;
        for s in &path.segments {
            let (start, end) = s.curve.endpoints().unwrap();
            if let Some(c) = cursor {
                assert!(near(c, start));
            }
            cursor = Some(end);
        }
    }

    #[test]
    fn a_closed_chain_is_closed_and_a_circle_is_a_path() {
        let square = [
            line(1, (0.0, 0.0), (10.0, 0.0)),
            line(2, (10.0, 0.0), (10.0, 10.0)),
            line(3, (10.0, 10.0), (0.0, 10.0)),
            line(4, (0.0, 10.0), (0.0, 0.0)),
        ];
        assert!(Path::from_curves(&square).unwrap().closed);
        let circle = [ProfileSegment {
            id: 7,
            curve: Curve2::Circle {
                center: Point2::ORIGIN,
                radius: 5.0,
            },
        }];
        let path = Path::from_curves(&circle).unwrap();
        assert!(path.closed);
        assert_eq!(path.start(), Some(Point2::new(5.0, 0.0)));
    }

    #[test]
    fn two_chains_and_branches_are_refused() {
        let two = [
            line(1, (0.0, 0.0), (10.0, 0.0)),
            line(2, (0.0, 5.0), (10.0, 5.0)),
        ];
        assert!(matches!(
            Path::from_curves(&two),
            Err(PathError::Disconnected { .. })
        ));
        let fork = [
            line(1, (0.0, 0.0), (10.0, 0.0)),
            line(2, (10.0, 0.0), (20.0, 5.0)),
            line(3, (10.0, 0.0), (20.0, -5.0)),
        ];
        assert!(matches!(
            Path::from_curves(&fork),
            Err(PathError::Branching { segment: _ })
        ));
        assert_eq!(Path::from_curves(&[]), Err(PathError::Empty));
    }
}
