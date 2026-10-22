//! Booleans of polygon soups by binary space partitioning.
//!
//! The method of W. Thibault and B. Naylor (*Set operations on polyhedra using binary space
//! partitioning trees*, SIGGRAPH 1987), in the compact form E. Wallace's csg.js made known:
//! each solid's boundary polygons are put in a BSP tree, one solid's polygons are clipped by
//! the other's tree to keep what lies outside it (or inside, with the tree inverted), and the
//! polygons both keep make the result.
//!
//! A polygon lying in a splitting plane goes to the side its normal points to: facing the
//! same way as the plane it counts as outside, facing the other way as inside. That is what
//! makes faces in contact come out right, the everyday case in CAD: a boss sketched on a
//! face, a cut started from one. Where the two faces touch they cancel, and where two faces
//! lie in one plane facing the same way only one of them is kept.
//!
//! Every polygon is convex (pieces of the faces' triangles, split by planes) and carries the
//! input face it is a piece of. The trees live in vectors and are walked with explicit
//! stacks, so a deep tree cannot overflow the stack.

// Indices into the tree's own nodes, made here.
#![allow(clippy::indexing_slicing)]

use oxyde_geom::{Point3, Vec3};

use crate::brep::cyclic_pairs;

/// An oriented plane: the points `p` with `normal · p = w`, `normal` of unit length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Plane3 {
    pub normal: Vec3,
    pub w: f64,
}

impl Plane3 {
    pub fn through(point: Point3, normal: Vec3) -> Self {
        Self {
            normal,
            w: normal.dot(point.to_vec()),
        }
    }

    fn flipped(self) -> Self {
        Self {
            normal: -self.normal,
            w: -self.w,
        }
    }

    /// Signed distance from the plane, positive on the side the normal points to.
    pub fn distance(&self, p: Point3) -> f64 {
        self.normal.dot(p.to_vec()) - self.w
    }
}

/// Where a piece of a face came from: the input (0 the target, 1… the tools) and the face's
/// index in that input's topology.
pub(crate) type Source = (u32, u32);

/// A convex polygon, its corners counter-clockwise about its plane's normal.
#[derive(Debug, Clone)]
pub(crate) struct Polygon {
    pub points: Vec<Point3>,
    pub plane: Plane3,
    pub source: Source,
}

impl Polygon {
    fn flip(&mut self) {
        self.points.reverse();
        self.plane = self.plane.flipped();
    }
}

const COPLANAR: u8 = 0;
const FRONT: u8 = 1;
const BACK: u8 = 2;
const SPANNING: u8 = FRONT | BACK;

/// What a plane makes of a polygon: the polygon itself if it lies in the plane (facing the
/// same way or the other) or on one side, or its pieces on either side.
#[derive(Default)]
struct Pieces {
    coplanar_front: Option<Polygon>,
    coplanar_back: Option<Polygon>,
    front: Option<Polygon>,
    back: Option<Polygon>,
}

/// Splits `polygon` by `plane`; corners within `eps` of it count as on it.
fn split(plane: &Plane3, polygon: Polygon, eps: f64) -> Pieces {
    let side = |p: Point3| {
        let t = plane.distance(p);
        if t < -eps {
            BACK
        } else if t > eps {
            FRONT
        } else {
            COPLANAR
        }
    };
    let marked: Vec<(Point3, u8)> = polygon.points.iter().map(|&p| (p, side(p))).collect();
    let kind = marked.iter().fold(COPLANAR, |kind, &(_, s)| kind | s);
    let mut pieces = Pieces::default();
    match kind {
        COPLANAR => {
            if plane.normal.dot(polygon.plane.normal) > 0.0 {
                pieces.coplanar_front = Some(polygon);
            } else {
                pieces.coplanar_back = Some(polygon);
            }
        }
        FRONT => pieces.front = Some(polygon),
        BACK => pieces.back = Some(polygon),
        _ => {
            let (mut front, mut back) = (Vec::new(), Vec::new());
            for ((p, sp), (q, sq)) in cyclic_pairs(&marked) {
                if sp != BACK {
                    front.push(p);
                }
                if sp != FRONT {
                    back.push(p);
                }
                if (sp | sq) == SPANNING {
                    let t = -plane.distance(p) / plane.normal.dot(q - p);
                    let crossing = p + (q - p) * t;
                    front.push(crossing);
                    back.push(crossing);
                }
            }
            let piece = |points: Vec<Point3>| {
                (points.len() >= 3).then(|| Polygon {
                    points,
                    plane: polygon.plane,
                    source: polygon.source,
                })
            };
            pieces.front = piece(front);
            pieces.back = piece(back);
        }
    }
    pieces
}

#[derive(Debug, Clone, Default)]
struct Node {
    plane: Option<Plane3>,
    front: Option<usize>,
    back: Option<usize>,
    /// The polygons lying in the node's plane, facing either way.
    polygons: Vec<Polygon>,
}

/// A BSP tree of a solid's boundary polygons; node 0 is the root. In front of every leaf is
/// outside the solid, behind it inside.
#[derive(Debug, Clone)]
struct Tree {
    nodes: Vec<Node>,
    eps: f64,
}

impl Tree {
    fn new(polygons: Vec<Polygon>, eps: f64) -> Tree {
        let mut tree = Tree {
            nodes: vec![Node::default()],
            eps,
        };
        tree.build(polygons);
        tree
    }

    /// Adds polygons to the tree, each node splitting by the plane of the first polygon
    /// that reached it.
    fn build(&mut self, polygons: Vec<Polygon>) {
        let mut stack = vec![(0, polygons)];
        while let Some((at, polygons)) = stack.pop() {
            let Some(first) = polygons.first() else {
                continue;
            };
            let plane = *self.nodes[at].plane.get_or_insert(first.plane);
            let (mut front, mut back) = (Vec::new(), Vec::new());
            for polygon in polygons {
                let pieces = split(&plane, polygon, self.eps);
                let node = &mut self.nodes[at];
                node.polygons.extend(pieces.coplanar_front);
                node.polygons.extend(pieces.coplanar_back);
                front.extend(pieces.front);
                back.extend(pieces.back);
            }
            if !front.is_empty() {
                stack.push((self.child(at, true), front));
            }
            if !back.is_empty() {
                stack.push((self.child(at, false), back));
            }
        }
    }

    /// The node in front of (or behind) node `at`, made if there is none yet.
    fn child(&mut self, at: usize, front: bool) -> usize {
        let existing = if front {
            self.nodes[at].front
        } else {
            self.nodes[at].back
        };
        if let Some(child) = existing {
            return child;
        }
        self.nodes.push(Node::default());
        let child = self.nodes.len() - 1;
        if front {
            self.nodes[at].front = Some(child);
        } else {
            self.nodes[at].back = Some(child);
        }
        child
    }

    /// The solid's complement: every polygon and plane turned round, front and back swapped.
    fn invert(&mut self) {
        for node in &mut self.nodes {
            for polygon in &mut node.polygons {
                polygon.flip();
            }
            node.plane = node.plane.map(Plane3::flipped);
            std::mem::swap(&mut node.front, &mut node.back);
        }
    }

    /// The parts of `polygons` outside this tree's solid.
    fn clip_polygons(&self, polygons: Vec<Polygon>) -> Vec<Polygon> {
        let mut kept = Vec::new();
        let mut stack = vec![(0, polygons)];
        while let Some((at, polygons)) = stack.pop() {
            let node = &self.nodes[at];
            let Some(plane) = node.plane else {
                kept.extend(polygons);
                continue;
            };
            let (mut front, mut back) = (Vec::new(), Vec::new());
            for polygon in polygons {
                let pieces = split(&plane, polygon, self.eps);
                front.extend(pieces.coplanar_front);
                front.extend(pieces.front);
                back.extend(pieces.coplanar_back);
                back.extend(pieces.back);
            }
            match node.front {
                Some(child) => stack.push((child, front)),
                None => kept.extend(front),
            }
            // With nothing behind the node, what is behind it is inside the solid: dropped.
            if let Some(child) = node.back {
                stack.push((child, back));
            }
        }
        kept
    }

    /// Keeps, of this tree's polygons, the parts outside `other`'s solid.
    fn clip_to(&mut self, other: &Tree) {
        for node in &mut self.nodes {
            let polygons = std::mem::take(&mut node.polygons);
            node.polygons = other.clip_polygons(polygons);
        }
    }

    fn into_polygons(self) -> Vec<Polygon> {
        self.nodes.into_iter().flat_map(|n| n.polygons).collect()
    }
}

/// The boundary of `a ∪ b`.
pub(crate) fn union(a: Vec<Polygon>, b: Vec<Polygon>, eps: f64) -> Vec<Polygon> {
    if a.is_empty() || b.is_empty() {
        return if a.is_empty() { b } else { a };
    }
    let (mut a, mut b) = (Tree::new(a, eps), Tree::new(b, eps));
    a.clip_to(&b);
    b.clip_to(&a);
    // What is left of b where a and b lie in one plane facing the same way goes too.
    b.invert();
    b.clip_to(&a);
    b.invert();
    a.build(b.into_polygons());
    a.into_polygons()
}

/// The boundary of `a − b`.
pub(crate) fn subtract(a: Vec<Polygon>, b: Vec<Polygon>, eps: f64) -> Vec<Polygon> {
    if a.is_empty() || b.is_empty() {
        return a;
    }
    let (mut a, mut b) = (Tree::new(a, eps), Tree::new(b, eps));
    a.invert();
    a.clip_to(&b);
    b.clip_to(&a);
    b.invert();
    b.clip_to(&a);
    b.invert();
    a.build(b.into_polygons());
    a.invert();
    a.into_polygons()
}

/// The boundary of `a ∩ b`.
pub(crate) fn intersect(a: Vec<Polygon>, b: Vec<Polygon>, eps: f64) -> Vec<Polygon> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let (mut a, mut b) = (Tree::new(a, eps), Tree::new(b, eps));
    a.invert();
    b.clip_to(&a);
    b.invert();
    a.clip_to(&b);
    b.clip_to(&a);
    a.build(b.into_polygons());
    a.invert();
    a.into_polygons()
}
