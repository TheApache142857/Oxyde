//! Flow trajectories, as SolidWorks Flow Simulation and Autodesk Flow Design draw them:
//! particles carried by the solved velocity from seeds upstream of the model (an external
//! flow) or on its inlets (an internal flow), each path a polyline with the time and the
//! speed at each of its points, which the application animates.
//!
//! A particle always knows the cell it is in: each step walks from cell to neighbour
//! across the faces its path crosses, as OpenFOAM's own particle tracking does, so
//! following it never searches the mesh. Inside a cell the velocity is the cell's plus its
//! Gauss gradient times the offset from the cell's centre, kept within the values on the
//! cell's faces: smooth from cell to cell, and never faster than the flow around it. Steps
//! use the midpoint rule, each a fraction of the cell's size. A path ends where it leaves
//! the fluid (the domain's box, an opening), on the model's walls, where the flow stands
//! still, or after a set length.
//!
//! Pure Rust and deterministic: the same results and seeds give the same paths.

use crate::mesh::PolyMesh;
use crate::results::FlowResults;
use crate::study::{add, cross, dot, norm, scale, sub, zip};

/// A step's length, in cell sizes.
const STEP: f64 = 0.3;
/// The most steps a path takes.
const MAX_STEPS: usize = 20_000;
/// The longest path, in diagonals of the domain.
const MAX_LENGTH: f64 = 3.0;
/// The most cells a single move may cross: a guard against cycles on warped cells.
const MAX_HOPS: usize = 100_000;
/// A particle slower than this share of the fastest flow stands still.
const STALLED: f64 = 1e-6;
/// m/s to mm/s.
const MM_PER_M: f64 = 1000.0;

/// Where trajectories start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seeding {
    /// A grid on a plane square to the free stream `velocity` (m/s), upstream of the
    /// model, across its frontal extent and a margin round it: an external flow.
    Upstream { velocity: [f64; 3] },
    /// Points spread evenly over the inlets' faces: an internal flow.
    Inlets,
}

/// Why a trajectory ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// It left the fluid: through the domain's box or an opening.
    Left,
    /// It reached a wall of the model.
    Wall,
    /// The flow there is too slow to follow.
    Stalled,
    /// It reached the longest path or the most steps allowed.
    Length,
    /// It was lost between cells too warped to walk across.
    Lost,
}

/// A particle's path: its positions (mm), the time it reaches each (s, from 0 at its seed)
/// and its speed there (m/s), as many of each.
#[derive(Debug, Clone, PartialEq)]
pub struct Trajectory {
    pub points: Vec<[f64; 3]>,
    pub times: Vec<f64>,
    pub speeds: Vec<f64>,
    pub end: End,
}

/// A particle on its path: where it is (mm), when (s from its seed) and how fast (m/s).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub position: [f64; 3],
    pub time: f64,
    pub speed: f64,
}

impl Trajectory {
    /// The time from its seed to its end, s.
    pub fn duration(&self) -> f64 {
        self.times.last().copied().unwrap_or(0.0)
    }

    /// Point `i` of the path.
    fn sample(&self, i: usize) -> Option<Sample> {
        Some(Sample {
            position: *self.points.get(i)?,
            time: *self.times.get(i)?,
            speed: *self.speeds.get(i)?,
        })
    }

    /// Where the particle is at time `t` (s); `None` outside the path.
    pub fn at(&self, t: f64) -> Option<Sample> {
        let last = *self.times.last()?;
        if !(t >= 0.0 && t <= last) {
            return None;
        }
        // The first point reached after `t`, and the one before it.
        let i = self.times.partition_point(|&s| s <= t);
        let Some(a) = self.sample(i.wrapping_sub(1)) else {
            return self.sample(0);
        };
        let Some(b) = self.sample(i) else {
            return Some(a);
        };
        let w = if b.time > a.time {
            (t - a.time) / (b.time - a.time)
        } else {
            0.0
        };
        Some(Sample {
            position: add(a.position, scale(sub(b.position, a.position), w)),
            time: t,
            speed: a.speed + (b.speed - a.speed) * w,
        })
    }

    /// The path from time `from` to time `to` (s): the particle at both and the points
    /// between. Empty where the two miss the path.
    pub fn between(&self, from: f64, to: f64) -> Vec<Sample> {
        let from = from.max(0.0);
        let to = to.min(self.duration());
        if !(from <= to && to.is_finite()) {
            return Vec::new();
        }
        let mut out = Vec::new();
        out.extend(self.at(from));
        let first = self.times.partition_point(|&s| s <= from);
        for i in first.. {
            match self.sample(i) {
                Some(s) if s.time < to => out.push(s),
                _ => break,
            }
        }
        out.extend(self.at(to));
        out
    }
}

/// Where a straight move through the mesh ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Walk {
    /// In this cell.
    Inside(usize),
    /// Out of `cell` through its boundary face `face`, at `at` (mm).
    Out {
        cell: usize,
        face: usize,
        at: [f64; 3],
    },
    /// Lost: the move crossed more cells than any path through the mesh could.
    Lost,
}

/// A face of a cell: its centre (mm) and its area vector (mm²) out of the cell.
#[derive(Debug, Clone, Copy)]
struct Plane {
    face: usize,
    centre: [f64; 3],
    area: [f64; 3],
}

/// A cell as a particle in it sees it: its faces and its velocity.
struct Local {
    cell: usize,
    planes: Vec<Plane>,
    field: Field,
}

/// A cell's velocity as a linear field: its value at the centre, its gradient, and the
/// bounds the values on its faces set.
struct Field {
    centre: [f64; 3],
    value: [f64; 3],
    /// Row `i` is the gradient of component `i`, (m/s)/mm.
    gradient: [[f64; 3]; 3],
    lo: [f64; 3],
    hi: [f64; 3],
}

impl Field {
    fn at(&self, p: [f64; 3]) -> [f64; 3] {
        let d = sub(p, self.centre);
        let [g0, g1, g2] = self.gradient;
        let [u0, u1, u2] = self.value;
        let u = [u0 + dot(g0, d), u1 + dot(g1, d), u2 + dot(g2, d)];
        zip(zip(u, self.lo, f64::max), self.hi, f64::min)
    }
}

/// The fluid mesh of a solved flow made ready for following particles: each cell's faces.
pub struct Tracer<'a> {
    r: &'a FlowResults,
    /// Cell `c`'s faces are `cell_faces[cell_start[c]..cell_start[c + 1]]`.
    cell_start: Vec<usize>,
    cell_faces: Vec<u32>,
    /// The box holding the mesh, mm.
    lo: [f64; 3],
    hi: [f64; 3],
    /// The fastest flow in a cell, m/s.
    fastest: f64,
}

impl<'a> Tracer<'a> {
    pub fn new(r: &'a FlowResults) -> Self {
        let mesh = &r.mesh;
        let sides = |f: usize| {
            [
                mesh.owner.get(f).map(|&c| c as usize),
                mesh.neighbour.get(f).map(|&c| c as usize),
            ]
        };
        let mut cell_start = vec![0usize; mesh.cells + 1];
        for f in 0..mesh.faces() {
            for c in sides(f).into_iter().flatten() {
                if let Some(s) = cell_start.get_mut(c + 1) {
                    *s += 1;
                }
            }
        }
        for c in 0..mesh.cells {
            let before = cell_start.get(c).copied().unwrap_or(0);
            if let Some(s) = cell_start.get_mut(c + 1) {
                *s += before;
            }
        }
        let mut filled = cell_start.clone();
        let mut cell_faces = vec![0u32; cell_start.last().copied().unwrap_or(0)];
        for f in 0..mesh.faces() {
            for c in sides(f).into_iter().flatten() {
                if let Some(at) = filled.get_mut(c) {
                    if let Some(slot) = cell_faces.get_mut(*at) {
                        *slot = f as u32;
                    }
                    *at += 1;
                }
            }
        }
        let first = mesh.points.first().copied().unwrap_or_default();
        let (lo, hi) = mesh.points.iter().fold((first, first), |(lo, hi), p| {
            (zip(lo, *p, f64::min), zip(hi, *p, f64::max))
        });
        let fastest = r.velocity.iter().map(|u| norm(*u)).fold(0.0, f64::max);
        Self {
            r,
            cell_start,
            cell_faces,
            lo,
            hi,
            fastest,
        }
    }

    fn mesh(&self) -> &PolyMesh {
        &self.r.mesh
    }

    fn faces_of(&self, cell: usize) -> &[u32] {
        match (self.cell_start.get(cell), self.cell_start.get(cell + 1)) {
            (Some(&a), Some(&b)) => self.cell_faces.get(a..b).unwrap_or_default(),
            _ => &[],
        }
    }

    /// Face `f`'s centre (its corners' average, mm) and area vector (mm²) pointing out of
    /// `cell`. Newell's area vector, exact for any polygon: the faces of a closed cell add
    /// up to nothing, so a uniform flow has no gradient.
    fn face_out_of(&self, f: usize, cell: usize) -> ([f64; 3], [f64; 3]) {
        let mesh = self.mesh();
        let corners = mesh.face(f);
        let point = |k: usize| {
            corners
                .get(k % corners.len().max(1))
                .and_then(|&p| mesh.points.get(p as usize))
                .copied()
                .unwrap_or_default()
        };
        let n = corners.len();
        let mut centre = [0.0; 3];
        let mut area = [0.0; 3];
        for k in 0..n {
            let (p, q) = (point(k), point(k + 1));
            centre = add(centre, p);
            area = add(area, cross(p, q));
        }
        let centre = scale(centre, 1.0 / n.max(1) as f64);
        let area = scale(area, 0.5);
        let owned = mesh.owner.get(f).is_some_and(|&c| c as usize == cell);
        (centre, if owned { area } else { scale(area, -1.0) })
    }

    /// The cell across internal face `f` from `cell`.
    fn across(&self, f: usize, cell: usize) -> Option<usize> {
        let mesh = self.mesh();
        let own = *mesh.owner.get(f)? as usize;
        let nei = *mesh.neighbour.get(f)? as usize;
        Some(if own == cell { nei } else { own })
    }

    /// Whether `p` lies in the mesh's box.
    fn in_box(&self, p: [f64; 3]) -> bool {
        (0..3).all(|k| {
            let (Some(&v), Some(&lo), Some(&hi)) = (p.get(k), self.lo.get(k), self.hi.get(k))
            else {
                return false;
            };
            v >= lo && v <= hi
        })
    }

    /// The box's diagonal, mm.
    fn diagonal(&self) -> f64 {
        norm(sub(self.hi, self.lo))
    }

    /// A cell's size, mm: the edge of a cube of its volume.
    fn size(&self, cell: usize) -> f64 {
        self.r
            .cell_volumes
            .get(cell)
            .map_or(0.0, |v| v.abs().cbrt())
    }

    /// Each face of `cell` with its centre and its area vector out of the cell.
    fn planes(&self, cell: usize) -> impl Iterator<Item = Plane> + '_ {
        self.faces_of(cell).iter().map(move |&f| {
            let f = f as usize;
            let (centre, area) = self.face_out_of(f, cell);
            Plane {
                face: f,
                centre,
                area,
            }
        })
    }

    /// `cell` as a particle in it sees it: its faces and its velocity.
    fn local(&self, cell: usize) -> Local {
        let planes: Vec<Plane> = self.planes(cell).collect();
        let field = self.field(cell, &planes);
        Local {
            cell,
            planes,
            field,
        }
    }

    /// Walks in a straight line from `from` (mm), in `cell`, to `to`: the cell holding
    /// `to`, or the boundary face the line leaves the fluid through.
    pub fn walk(&self, cell: usize, from: [f64; 3], to: [f64; 3]) -> Walk {
        self.walk_from(cell, self.planes(cell), from, to)
    }

    /// [`Tracer::walk`], the first cell's faces given.
    fn walk_from(
        &self,
        mut cell: usize,
        planes: impl Iterator<Item = Plane>,
        from: [f64; 3],
        to: [f64; 3],
    ) -> Walk {
        let first = self.mesh().internal_faces();
        let mut a = from;
        let mut exit = first_exit(planes, a, to);
        for _ in 0..MAX_HOPS {
            let Some((lambda, f)) = exit else {
                return Walk::Inside(cell);
            };
            let at = add(a, scale(sub(to, a), lambda));
            if f >= first {
                return Walk::Out { cell, face: f, at };
            }
            let Some(next) = self.across(f, cell) else {
                return Walk::Lost;
            };
            cell = next;
            a = at;
            exit = first_exit(self.planes(cell), a, to);
        }
        Walk::Lost
    }

    /// The cell holding `p` (mm), if it is in the fluid: from the cell whose centre is
    /// nearest, a walk to it. Looks through every cell: for a first point only, the rest
    /// walk from there.
    pub fn find(&self, p: [f64; 3]) -> Option<usize> {
        if !self.in_box(p) {
            return None;
        }
        let nearest = self
            .r
            .cell_centres
            .iter()
            .enumerate()
            .map(|(c, x)| (c, dot(sub(*x, p), sub(*x, p))))
            .min_by(|a, b| a.1.total_cmp(&b.1))?
            .0;
        let centre = *self.r.cell_centres.get(nearest)?;
        match self.walk(nearest, centre, p) {
            Walk::Inside(c) => Some(c),
            _ => None,
        }
    }

    /// The cell holding `p`, walking from `cell` and the point `from` in it; the whole
    /// mesh looked through when the walk leaves the fluid on the way.
    fn find_from(&self, cell: usize, from: [f64; 3], p: [f64; 3]) -> Option<usize> {
        match self.walk(cell, from, p) {
            Walk::Inside(c) => Some(c),
            _ => self.find(p),
        }
    }

    /// The velocity in `cell`, whose faces are `planes`, as a linear field.
    fn field(&self, cell: usize, planes: &[Plane]) -> Field {
        let r = self.r;
        let first = self.mesh().internal_faces();
        let centre = r.cell_centres.get(cell).copied().unwrap_or_default();
        let value = r.velocity.get(cell).copied().unwrap_or_default();
        let volume = r.cell_volumes.get(cell).copied().unwrap_or(0.0);
        let mut sum = [[0.0; 3]; 3];
        let (mut lo, mut hi) = (value, value);
        for &Plane {
            face: f,
            centre: c,
            area: s,
        } in planes
        {
            let on_face = match self.across(f, cell) {
                Some(other) => {
                    // Linear between the two centres, by their distances from the face.
                    let x = r.cell_centres.get(other).copied().unwrap_or(c);
                    let u = r.velocity.get(other).copied().unwrap_or(value);
                    let (here, there) = (dot(sub(c, centre), s), dot(sub(x, c), s));
                    let w = if here + there > 0.0 {
                        (there / (here + there)).clamp(0.0, 1.0)
                    } else {
                        0.5
                    };
                    add(scale(value, w), scale(u, 1.0 - w))
                }
                None => r
                    .boundary_velocity
                    .get(f.wrapping_sub(first))
                    .copied()
                    .unwrap_or(value),
            };
            for (row, &u) in sum.iter_mut().zip(&on_face) {
                *row = add(*row, scale(s, u));
            }
            lo = zip(lo, on_face, f64::min);
            hi = zip(hi, on_face, f64::max);
        }
        let gradient = if volume > 0.0 {
            sum.map(|row| scale(row, 1.0 / volume))
        } else {
            [[0.0; 3]; 3]
        };
        Field {
            centre,
            value,
            gradient,
            lo,
            hi,
        }
    }

    /// Whether the boundary face `f` is an opening or the domain's box, which the fluid
    /// leaves through, rather than a wall of the model.
    fn is_opening(&self, f: usize) -> bool {
        let first = self.mesh().internal_faces();
        if self
            .r
            .boundary_labels
            .get(f.wrapping_sub(first))
            .is_none_or(|l| l.is_none())
        {
            return true;
        }
        self.mesh()
            .patches
            .iter()
            .find(|p| p.start <= f && f < p.start + p.count)
            .is_some_and(|p| p.name.starts_with("outlet_") || p.name.starts_with("inlet_"))
    }

    /// Follows a particle from `seed` (mm), in `cell`, until it leaves the fluid, reaches
    /// a wall, stalls or goes too far.
    pub fn trace(&self, cell: usize, seed: [f64; 3]) -> Trajectory {
        let mut path = Trajectory {
            points: vec![seed],
            times: vec![0.0],
            speeds: Vec::new(),
            end: End::Length,
        };
        let stalled = STALLED * self.fastest;
        let longest = MAX_LENGTH * self.diagonal();
        let (mut x, mut t, mut length) = (seed, 0.0, 0.0);
        // The cell the particle is in, and the one its last midpoint was in, which it
        // usually enters next: each cell's faces and field worked out once.
        let mut here = self.local(cell);
        let mut ahead: Option<Local> = None;
        for _ in 0..MAX_STEPS {
            let u1 = here.field.at(x);
            let speed = norm(u1);
            path.speeds.push(speed);
            if !(speed > stalled && speed.is_finite()) {
                path.end = End::Stalled;
                return path;
            }
            if length > longest {
                path.end = End::Length;
                return path;
            }
            // A fraction of the cell, in seconds.
            let dt = STEP * self.size(here.cell) / (speed * MM_PER_M);
            let mid = add(x, scale(u1, 0.5 * dt * MM_PER_M));
            let u2 = match self.walk_from(here.cell, here.planes.iter().copied(), x, mid) {
                Walk::Inside(c) if c != here.cell => {
                    let there = match ahead.take() {
                        Some(l) if l.cell == c => l,
                        _ => self.local(c),
                    };
                    let u = there.field.at(mid);
                    ahead = Some(there);
                    u
                }
                // Near the boundary the cell's own field reaches the midpoint.
                _ => here.field.at(mid),
            };
            let next = add(x, scale(u2, dt * MM_PER_M));
            match self.walk_from(here.cell, here.planes.iter().copied(), x, next) {
                Walk::Inside(c) => {
                    length += norm(sub(next, x));
                    t += dt;
                    path.points.push(next);
                    path.times.push(t);
                    x = next;
                    if c != here.cell {
                        here = match ahead.take() {
                            Some(l) if l.cell == c => l,
                            other => {
                                ahead = other;
                                self.local(c)
                            }
                        };
                    }
                }
                Walk::Out { cell: c, face, at } => {
                    let full = norm(sub(next, x));
                    let part = norm(sub(at, x));
                    let share = if full > 0.0 { part / full } else { 0.0 };
                    let last = if c == here.cell {
                        here.field.at(at)
                    } else {
                        self.local(c).field.at(at)
                    };
                    path.points.push(at);
                    path.times.push(t + dt * share);
                    path.speeds.push(norm(last));
                    path.end = if self.is_opening(face) {
                        End::Left
                    } else {
                        End::Wall
                    };
                    return path;
                }
                Walk::Lost => {
                    path.end = End::Lost;
                    return path;
                }
            }
        }
        // The last point's speed, to keep one per point.
        if path.speeds.len() < path.points.len() {
            path.speeds.push(norm(here.field.at(x)));
        }
        path.end = End::Length;
        path
    }

    /// About `count` seeds (mm), each with the cell holding it.
    pub fn seeds(&self, seeding: Seeding, count: usize) -> Vec<(usize, [f64; 3])> {
        if count == 0 {
            return Vec::new();
        }
        match seeding {
            Seeding::Upstream { velocity } => self.upstream(velocity, count),
            Seeding::Inlets => self.on_inlets(count),
        }
    }

    /// The corners of the mesh's faces on the model, mm.
    fn model_points(&self) -> Vec<[f64; 3]> {
        let mesh = self.mesh();
        let first = mesh.internal_faces();
        let mut used = vec![false; mesh.points.len()];
        for (i, label) in self.r.boundary_labels.iter().enumerate() {
            if label.is_none() {
                continue;
            }
            for &p in mesh.face(first + i) {
                if let Some(u) = used.get_mut(p as usize) {
                    *u = true;
                }
            }
        }
        mesh.points
            .iter()
            .zip(&used)
            .filter(|(_, u)| **u)
            .map(|(p, _)| *p)
            .collect()
    }

    /// A grid on a plane square to `velocity`, upstream of the model: across its frontal
    /// extent with a quarter of its size round it, half its size before it (less where
    /// the domain begins closer).
    fn upstream(&self, velocity: [f64; 3], count: usize) -> Vec<(usize, [f64; 3])> {
        let speed = norm(velocity);
        if !(speed > 0.0 && speed.is_finite()) {
            return Vec::new();
        }
        let d = scale(velocity, 1.0 / speed);
        let (u, v) = basis(d);
        let model = self.model_points();
        if model.is_empty() {
            return Vec::new();
        }
        let (u_lo, u_hi) = span(u, &model);
        let (v_lo, v_hi) = span(v, &model);
        let (d_lo, d_hi) = span(d, &model);
        let size = (u_hi - u_lo).max(v_hi - v_lo).max(d_hi - d_lo);
        if !(size > 0.0 && size.is_finite()) {
            return Vec::new();
        }
        // The domain's box, in the same terms.
        let corners: Vec<[f64; 3]> = (0..8)
            .map(|k| {
                let pick = |bit: usize, axis: usize| {
                    let (lo, hi) = (
                        self.lo.get(axis).copied().unwrap_or(0.0),
                        self.hi.get(axis).copied().unwrap_or(0.0),
                    );
                    if k & bit == 0 { lo } else { hi }
                };
                [pick(1, 0), pick(2, 1), pick(4, 2)]
            })
            .collect();
        let (box_u_lo, box_u_hi) = span(u, &corners);
        let (box_v_lo, box_v_hi) = span(v, &corners);
        let (box_d_lo, _) = span(d, &corners);
        let gap = (d_lo - box_d_lo).max(0.0);
        let along = d_lo - (0.5 * size).min(0.8 * gap);
        let margin = 0.25 * size;
        // Inside the box by a hair, so that no seed lies on its faces.
        let inset = 1e-6 * self.diagonal();
        let a_lo = (u_lo - margin).max(box_u_lo + inset);
        let a_hi = (u_hi + margin).min(box_u_hi - inset);
        let b_lo = (v_lo - margin).max(box_v_lo + inset);
        let b_hi = (v_hi + margin).min(box_v_hi - inset);
        let (width, height) = (a_hi - a_lo, b_hi - b_lo);
        if !(width > 0.0 && height > 0.0) {
            return Vec::new();
        }
        let spacing = (width * height / count as f64).sqrt();
        let nu = ((width / spacing).round() as usize).max(1);
        let nv = ((height / spacing).round() as usize).max(1);
        let mut out = Vec::with_capacity(nu * nv);
        let mut last: Option<(usize, [f64; 3])> = None;
        for j in 0..nv {
            for i in 0..nu {
                let a = a_lo + width * (i as f64 + 0.5) / nu as f64;
                let b = b_lo + height * (j as f64 + 0.5) / nv as f64;
                let p = add(add(scale(d, along), scale(u, a)), scale(v, b));
                let cell = match last {
                    Some((c, from)) => self.find_from(c, from, p),
                    None => self.find(p),
                };
                if let Some(c) = cell {
                    out.push((c, p));
                    last = Some((c, p));
                }
            }
        }
        out
    }

    /// `count` points spread evenly over the inlets' faces (each face's centre and the
    /// points halfway to its corners, the farthest from those taken picked in turn), each
    /// moved a little into its cell.
    fn on_inlets(&self, count: usize) -> Vec<(usize, [f64; 3])> {
        let mesh = self.mesh();
        let mut candidates: Vec<(usize, [f64; 3])> = Vec::new();
        for patch in mesh.patches.iter().filter(|p| p.name.starts_with("inlet_")) {
            for f in patch.start..patch.start + patch.count {
                let Some(&owner) = mesh.owner.get(f) else {
                    continue;
                };
                let cell = owner as usize;
                let Some(&inside) = self.r.cell_centres.get(cell) else {
                    continue;
                };
                let (centre, _) = self.face_out_of(f, cell);
                let corners = mesh
                    .face(f)
                    .iter()
                    .filter_map(|&p| mesh.points.get(p as usize))
                    .map(|&p| add(centre, scale(sub(p, centre), 0.5)));
                for p in std::iter::once(centre).chain(corners) {
                    // A twentieth of the way to the cell's centre: off the face.
                    candidates.push((cell, add(p, scale(sub(inside, p), 0.05))));
                }
            }
        }
        if candidates.is_empty() {
            return Vec::new();
        }
        // The first nearest the inlets' middle, then the farthest from those taken.
        let n = candidates.len() as f64;
        let middle = scale(
            candidates.iter().fold([0.0; 3], |s, (_, p)| add(s, *p)),
            1.0 / n,
        );
        let distance = |a: [f64; 3], b: [f64; 3]| dot(sub(a, b), sub(a, b));
        let mut nearest: Vec<f64> = candidates
            .iter()
            .map(|(_, p)| -distance(*p, middle))
            .collect();
        let mut out = Vec::with_capacity(count.min(candidates.len()));
        let mut started = false;
        for _ in 0..count.min(candidates.len()) {
            let Some((k, _)) = nearest
                .iter()
                .enumerate()
                .filter(|(_, d)| d.is_finite())
                .max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(&a.0)))
            else {
                break;
            };
            let Some(&(cell, p)) = candidates.get(k) else {
                break;
            };
            if let Some(slot) = nearest.get_mut(k) {
                *slot = f64::NEG_INFINITY;
            }
            if !started {
                // From here on, distances from the points taken.
                started = true;
                for (d, (_, q)) in nearest.iter_mut().zip(&candidates) {
                    if d.is_finite() {
                        *d = distance(*q, p);
                    }
                }
            } else {
                for (d, (_, q)) in nearest.iter_mut().zip(&candidates) {
                    if d.is_finite() {
                        *d = d.min(distance(*q, p));
                    }
                }
            }
            let centre = self.r.cell_centres.get(cell).copied().unwrap_or(p);
            if let Walk::Inside(c) = self.walk(cell, centre, p) {
                out.push((c, p));
            }
        }
        out
    }
}

/// About `count` trajectories of the flow in `r`, from the seeds of `seeding`, in the
/// seeds' order; paths that never moved are left out. The paths are followed on several
/// threads, each its own share of the seeds.
pub fn trajectories(r: &FlowResults, seeding: Seeding, count: usize) -> Vec<Trajectory> {
    let tracer = Tracer::new(r);
    let seeds = tracer.seeds(seeding, count);
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 8);
    let share = seeds.len().div_ceil(threads).max(1);
    let tracer = &tracer;
    let mut paths = Vec::with_capacity(seeds.len());
    std::thread::scope(|scope| {
        let parts: Vec<_> = seeds
            .chunks(share)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .map(|&(cell, p)| tracer.trace(cell, p))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        // A thread that failed loses its paths, not the application.
        for part in parts {
            if let Ok(part) = part.join() {
                paths.extend(part);
            }
        }
    });
    paths.retain(|t| t.points.len() >= 2);
    paths
}

/// The face the line from `a` to `to` leaves a cell of `planes` through first, and how far
/// along the line (0 to 1); `None` when `to` lies in the cell.
fn first_exit(
    planes: impl Iterator<Item = Plane>,
    a: [f64; 3],
    to: [f64; 3],
) -> Option<(f64, usize)> {
    let mut exit: Option<(f64, usize)> = None;
    for Plane {
        face,
        centre: c,
        area: s,
    } in planes
    {
        let beyond = dot(sub(to, c), s);
        if beyond <= 0.0 {
            continue;
        }
        let before = dot(sub(a, c), s);
        let lambda = if before < 0.0 {
            before / (before - beyond)
        } else {
            0.0
        };
        if exit.is_none_or(|(l, _)| lambda < l) {
            exit = Some((lambda, face));
        }
    }
    exit
}

/// The smallest and largest of `points` along `axis`.
fn span(axis: [f64; 3], points: &[[f64; 3]]) -> (f64, f64) {
    points
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
            let s = dot(*p, axis);
            (lo.min(s), hi.max(s))
        })
}

/// Two unit vectors square to the unit vector `d` and to each other, along the world's
/// axes where `d` is: the axis least along `d`, made square to it, then `d × u`.
pub(crate) fn basis(d: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let axes = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let helper = axes
        .into_iter()
        .min_by(|a, b| dot(*a, d).abs().total_cmp(&dot(*b, d).abs()))
        .unwrap_or([1.0, 0.0, 0.0]);
    let u = sub(helper, scale(d, dot(helper, d)));
    let u = scale(u, 1.0 / norm(u).max(f64::MIN_POSITIVE));
    (u, cross(d, u))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mesh::tests::write_two_cubes;

    /// The two unit cubes (1000 mm) of the mesh's tests with `velocity` (m/s) everywhere,
    /// the boundary faces on the model faces `labels` gives (by boundary face).
    pub(crate) fn uniform(
        velocity: [f64; 3],
        labels: impl Fn(usize) -> Option<u32>,
    ) -> FlowResults {
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "oxyde-flow-trajectory-{}-{call}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        write_two_cubes(&dir, false);
        let mesh = PolyMesh::read(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let (cell_centres, cell_volumes) = mesh.cell_geometry();
        let boundary = mesh.faces() - mesh.internal_faces();
        FlowResults {
            time: "1".to_string(),
            cell_centres,
            cell_volumes,
            velocity: vec![velocity; 2],
            pressure: vec![0.0; 2],
            boundary_velocity: vec![velocity; boundary],
            boundary_pressure: vec![0.0; boundary],
            boundary_flux: None,
            boundary_labels: (0..boundary).map(labels).collect(),
            mesh,
            temperature: None,
            boundary_temperature: None,
            boundary_alphat: None,
            solid: None,
        }
    }

    /// Every boundary face on the model but the second cube's far end (x = 2000 mm), the
    /// domain's.
    fn far_end_open(i: usize) -> Option<u32> {
        (i != 1).then_some(if i == 0 { 0 } else { 1 })
    }

    fn close(a: [f64; 3], b: [f64; 3], tolerance: f64) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < tolerance)
    }

    #[test]
    fn a_walk_crosses_from_cell_to_cell() {
        let r = uniform([2.0, 0.0, 0.0], far_end_open);
        let tracer = Tracer::new(&r);
        let start = [500.0, 500.0, 500.0];
        assert_eq!(
            tracer.walk(0, start, [600.0, 400.0, 300.0]),
            Walk::Inside(0)
        );
        assert_eq!(
            tracer.walk(0, start, [1500.0, 200.0, 700.0]),
            Walk::Inside(1)
        );
        // Back again.
        assert_eq!(
            tracer.walk(1, [1500.0, 200.0, 700.0], [100.0, 900.0, 900.0]),
            Walk::Inside(0)
        );
        // Out through the far end, face 2, where the line meets it.
        match tracer.walk(0, start, [2500.0, 500.0, 1000.0]) {
            Walk::Out { cell, face, at } => {
                assert_eq!((cell, face), (1, 2));
                assert!(close(at, [2000.0, 500.0, 875.0], 1e-9), "{at:?}");
            }
            other => panic!("{other:?}"),
        }
        // Out through a side of the first cube, before reaching the second.
        match tracer.walk(0, start, [1200.0, -500.0, 500.0]) {
            Walk::Out { cell, face, at } => {
                assert_eq!((cell, face), (0, 3));
                assert!(close(at, [850.0, 0.0, 500.0], 1e-9), "{at:?}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(tracer.find([1500.0, 500.0, 500.0]), Some(1));
        assert_eq!(tracer.find([250.0, 10.0, 990.0]), Some(0));
        assert_eq!(tracer.find([-10.0, 500.0, 500.0]), None);
    }

    #[test]
    fn a_uniform_flow_gives_straight_lines() {
        let speed = 2.0;
        let r = uniform([speed, 0.0, 0.0], far_end_open);
        let tracer = Tracer::new(&r);
        // The inlet's centre and the four points halfway to its corners.
        let seeds = tracer.seeds(Seeding::Inlets, 10);
        assert_eq!(seeds.len(), 5);
        assert!(close(seeds[0].1, [25.0, 500.0, 500.0], 1e-9), "{seeds:?}");
        for (cell, seed) in seeds {
            assert_eq!(cell, 0);
            let path = tracer.trace(cell, seed);
            assert_eq!(path.end, End::Left, "{path:?}");
            assert_eq!(path.points.len(), path.times.len());
            assert_eq!(path.points.len(), path.speeds.len());
            for (p, w) in path.points.iter().zip(path.points.iter().skip(1)) {
                assert!(w[0] > p[0]);
            }
            for p in &path.points {
                assert!((p[1] - seed[1]).abs() < 1e-9 && (p[2] - seed[2]).abs() < 1e-9);
            }
            let end = *path.points.last().unwrap();
            assert!((end[0] - 2000.0).abs() < 1e-9, "{end:?}");
            // 2 m/s over the rest of the two metres.
            let expected = (2000.0 - seed[0]) / 1000.0 / speed;
            assert!(
                (path.duration() - expected).abs() < 1e-9,
                "{}",
                path.duration()
            );
            assert!(path.speeds.iter().all(|s| (s - speed).abs() < 1e-12));
            // Halfway in time, halfway along.
            let half = path.at(expected / 2.0).unwrap();
            let x = seed[0] + 1000.0 * speed * expected / 2.0;
            assert!((half.position[0] - x).abs() < 1e-6);
            assert_eq!((half.time, half.speed), (expected / 2.0, speed));
            assert!(path.at(-1.0).is_none() && path.at(expected + 1.0).is_none());
            let piece = path.between(0.1, 0.3);
            assert!((piece[0].position[0] - (seed[0] + 200.0)).abs() < 1e-6);
            assert_eq!(piece[0].time, 0.1);
            let last = piece.last().unwrap();
            assert!((last.position[0] - (seed[0] + 600.0)).abs() < 1e-6);
            assert!(piece.windows(2).all(|w| w[1].time > w[0].time));
            assert!(
                piece
                    .windows(2)
                    .all(|w| w[1].position[0] > w[0].position[0])
            );
            assert!(path.between(0.5, 0.2).is_empty());
        }
    }

    #[test]
    fn seeds_lie_upstream_of_the_model_and_stop_on_it() {
        // The model is the second cube's far end (x = 2000 mm), the rest the domain's.
        let r = uniform([3.0, 0.0, 0.0], |i| (i == 1).then_some(7));
        let tracer = Tracer::new(&r);
        let seeds = tracer.seeds(
            Seeding::Upstream {
                velocity: [3.0, 0.0, 0.0],
            },
            16,
        );
        // Half the model's size (1000 mm) before it, a grid across the domain's section.
        assert_eq!(seeds.len(), 16);
        for &(cell, p) in &seeds {
            assert_eq!(cell, 1);
            assert!((p[0] - 1500.0).abs() < 1e-9, "{p:?}");
            let path = tracer.trace(cell, p);
            assert_eq!(path.end, End::Wall);
            let end = *path.points.last().unwrap();
            assert!(close(end, [2000.0, p[1], p[2]], 1e-9), "{end:?}");
        }
        // No free stream, no seeds.
        assert!(
            tracer
                .seeds(Seeding::Upstream { velocity: [0.0; 3] }, 16)
                .is_empty()
        );
        let all = trajectories(
            &r,
            Seeding::Upstream {
                velocity: [3.0, 0.0, 0.0],
            },
            16,
        );
        assert_eq!(all.len(), 16);
    }

    #[test]
    fn a_sheared_flow_is_followed_between_cells() {
        // Faster in the second cube: the gradient carries a particle smoothly from one
        // speed to the other.
        let mut r = uniform([1.0, 0.0, 0.0], far_end_open);
        r.velocity[1] = [3.0, 0.0, 0.0];
        let first = r.mesh.internal_faces();
        for (i, u) in r.boundary_velocity.iter_mut().enumerate() {
            if r.mesh.owner[first + i] == 1 {
                *u = [3.0, 0.0, 0.0];
            }
        }
        let tracer = Tracer::new(&r);
        let path = tracer.trace(0, [100.0, 500.0, 500.0]);
        assert_eq!(path.end, End::Left);
        // Never slower than the slowest nor faster than the fastest, and faster on.
        assert!(
            path.speeds
                .iter()
                .all(|&s| (1.0 - 1e-9..=3.0 + 1e-9).contains(&s))
        );
        assert!(path.speeds.last().unwrap() > path.speeds.first().unwrap());
        // Each cell's gradient from the values on its faces: (2 − 1) / 1000 per mm in the
        // first, the face between them at the cells' mean, the inlet at the first's.
        let face = [1000.0, 500.0, 500.0];
        let (first, second) = (tracer.local(0).field, tracer.local(1).field);
        assert!((first.at(face)[0] - 1.5).abs() < 1e-9);
        assert!((second.at(face)[0] - 2.5).abs() < 1e-9);
        // Kept within the values round the cell: never slower than the inlet's.
        assert!((first.at([0.0, 500.0, 500.0])[0] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_still_flow_stalls() {
        let r = uniform([0.0; 3], far_end_open);
        let tracer = Tracer::new(&r);
        let path = tracer.trace(0, [500.0, 500.0, 500.0]);
        assert_eq!(path.end, End::Stalled);
        assert_eq!(path.points.len(), 1);
        assert!(trajectories(&r, Seeding::Inlets, 4).is_empty());
    }

    #[test]
    fn bases_follow_the_axes() {
        let (u, v) = basis([1.0, 0.0, 0.0]);
        assert_eq!((u, v), ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]));
        let d = [0.6, 0.0, 0.8];
        let (u, v) = basis(d);
        assert!(dot(u, d).abs() < 1e-12 && dot(v, d).abs() < 1e-12 && dot(u, v).abs() < 1e-12);
        assert!((norm(u) - 1.0).abs() < 1e-12 && (norm(v) - 1.0).abs() < 1e-12);
    }
}
