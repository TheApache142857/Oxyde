//! Contacts between bodies, as Fusion 360's Simulation offers them: bonded, separation,
//! sliding, rough and offset bonded, found automatically between faces that touch
//! ([`detect_contacts`]) or set by hand as two sets of face labels ([`Contact`]).
//!
//! Bodies meshed apart (`MeshOptions::separate_bodies`, or Netgen's per-body meshes) share
//! no node; a contact ties them. The code depends on [`TetMesh`] only: nodes, elements, the
//! body of each element and the labelled boundary facets, whatever mesher made them.
//!
//! - **Pairing, node to surface.** Of a contact's two sides, the one with the smaller
//!   facets is the *slave*: each of its boundary nodes (corners, and edge nodes of ten-node
//!   elements) is paired with the nearest point of the other side's facets (the *master*),
//!   when it lies within the contact's offset plus a quarter of the elements' size there.
//!   The point's motion is the master facet's own interpolation: the six-node triangle's
//!   shape functions on a ten-node mesh, the same the elements use, so a tie is consistent
//!   with them. A slave node stands for a share of its facets' area (a third of a
//!   three-node facet; a twelfth at each corner and a quarter at each edge node of a
//!   six-node one, its four sub-triangles' thirds).
//! - **Constraints.** The gap `g = u_slave − Σ N u_master` is held to zero in all three
//!   directions (bonded, offset bonded, rough while closed) or along the master's normal
//!   only (sliding, separation while closed). Separation and rough let the faces part:
//!   `g · n ≥ 0`, the contact pressure `p ≥ 0`, one of them zero.
//! - **Augmented Lagrangian.** The constraints enter the energy as `w (λ · g + ε/2 |g|²)`
//!   per slave node (`w` its area): a penalty `ε` (a stiffness per area) and a multiplier
//!   `λ` (a traction) updated after each solve by `λ ← λ + ε g` (Uzawa). The matrix stays
//!   symmetric positive definite, so the solver's conjugate gradients and multigrid work
//!   unchanged, and the constraints are met exactly in the limit with a moderate penalty,
//!   which plain penalties only reach by making the matrix ill-conditioned; Lagrange
//!   multipliers as unknowns would make it indefinite (a saddle point), which conjugate
//!   gradients and smoothed aggregation do not solve. The penalty is `30 E / h`, which
//!   meets the ties to a hundred-thousandth of the largest displacement in two to eight
//!   solves, each starting from the last one's displacements with the same multigrid.
//! - **Active set, for separation and rough.** Every node starts closed. Once the
//!   multipliers have converged for the current set of closed nodes, nodes whose
//!   traction (averaged over their facets) pulls open, with no force, and open nodes that
//!   penetrate close; the matrix changes and the multipliers converge again, until no node
//!   changes. Small margins (the tolerance's gap and its traction) keep nodes on the edge
//!   of the closed zone from flipping on rounding, and a node that has opened three times
//!   stays closed, which ends any cycle.
//! - **No load steps.** The study is linear and small-strain and the contacts have no
//!   friction coefficient, so the result does not depend on the loading path (rough takes a
//!   closed node as stuck from the start): the active set is iterated at full load.
//! - **Results.** The contact pressure at each slave node is `−λ · n`; averaged over each
//!   slave facet with the nodes' area shares (which removes the corner/edge-node
//!   oscillation of ten-node faces) and back to the nodes, then carried over to the master
//!   side's nodes at the nearest slave point.

// Indices into the mesh, checked by `TetMesh::validate` before anything is paired, and into
// arrays sized here.
#![allow(clippy::indexing_slicing)]

use std::collections::BTreeMap;

use crate::bvh::{Bvh, closest_point};
use crate::material::Elastic;
use crate::math::{V3, add, dot, norm, normalize, scale, sub};
use crate::mesh::{Facet, TetMesh};

/// How a contact lets its faces move against each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContactKind {
    /// The faces move together, as if glued (the default between touching parts).
    Bonded,
    /// The faces may not pass through each other but may part, and slide on each other
    /// without friction.
    Separation,
    /// The faces may not pass through each other nor part; they slide without friction.
    Sliding,
    /// The faces may part but not slip on each other (infinite friction).
    Rough,
    /// Bonded across a gap: the faces move together as if a rigid layer filled it.
    OffsetBonded,
}

impl ContactKind {
    /// Whether it holds all three directions while closed (else the normal only).
    pub(crate) fn ties_tangent(self) -> bool {
        matches!(
            self,
            ContactKind::Bonded | ContactKind::Rough | ContactKind::OffsetBonded
        )
    }

    /// Whether the faces may part.
    pub(crate) fn may_separate(self) -> bool {
        matches!(self, ContactKind::Separation | ContactKind::Rough)
    }
}

/// A contact between two sets of faces, normally on two bodies.
#[derive(Debug, Clone, PartialEq)]
pub struct Contact {
    pub kind: ContactKind,
    /// The face labels of one side.
    pub a_labels: Vec<u32>,
    /// The face labels of the other side.
    pub b_labels: Vec<u32>,
    /// The gap the contact bridges, mm: 0 for faces that touch. A node of one side is tied
    /// to the other side when it lies within this distance plus a quarter of the elements'
    /// size there, which covers the facets of curved faces and meshes that do not match.
    /// For offset bonded, the distance between the faces. Separation and rough treat any
    /// gap within it as closed, Fusion's and ANSYS's "adjust to touch".
    pub offset: f64,
}

/// Two faces on different bodies found facing each other within a tolerance, for the UI
/// to list and turn into contacts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactCandidate {
    /// The lower-numbered body, and its face.
    pub body_a: u32,
    pub a_label: u32,
    pub body_b: u32,
    pub b_label: u32,
    /// The area of the facets found close to the other face, mm² (the larger side's).
    pub area: f64,
    /// The largest distance found between the faces, mm: about 0 where they touch.
    pub gap: f64,
}

impl ContactCandidate {
    /// A contact of `kind` between the two faces, bridging the gap found.
    pub fn contact(&self, kind: ContactKind) -> Contact {
        Contact {
            kind,
            a_labels: vec![self.a_label],
            b_labels: vec![self.b_label],
            offset: self.gap,
        }
    }
}

/// What a contact carried, in a study's results.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactResult {
    /// Slave nodes tied to the other side, and those too far from it to be.
    pub paired: usize,
    pub unpaired: usize,
    /// N: the force side b exerts on side a.
    pub force: V3,
    /// N: the part of it pressing the faces together (negative for a bonded contact
    /// pulling them).
    pub normal_force: f64,
    /// mm²: the area of the slave side's facets that are closed (touching, or tied): those
    /// with at least half their area on closed nodes.
    pub area_in_contact: f64,
    /// MPa: the largest contact pressure (averaged over facets, as plotted).
    pub max_pressure: f64,
}

/// How the contacts' iterations went, in a study's results.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ContactSolve {
    /// Linear solves (augmented Lagrangian updates); 0 without contacts.
    pub iterations: usize,
    /// Times the matrix changed because a node of a separation or rough contact opened
    /// or closed.
    pub rebuilds: usize,
    /// Whether the constraints were met and no node changed state on the last update.
    /// When not, the results are those of the last solve: the UI should warn.
    pub converged: bool,
    /// The largest violation of a constraint left, mm (a gap where a contact is closed,
    /// a penetration where it is open).
    pub violation: f64,
}

/// Facets facing each other: unit normals with a dot product under this (within about
/// 25° of opposite), so that faces meeting at an edge are not taken for a contact.
const FACING: f64 = -0.9;
/// Times a tie of a separation or rough contact may open before it stays closed.
const MAX_OPENINGS: u8 = 3;
/// The search distance beyond a contact's offset, as a fraction of the element size.
const REACH: f64 = 0.25;
/// The penalty, as a multiple of `E / h` (`E` the smaller modulus of the two sides, `h`
/// the slave side's element size): stiff enough that the augmented Lagrangian meets the
/// constraints in a few updates, soft enough not to slow the solve. Tried on the tests:
/// 10 takes about twice the updates, 100 a third more conjugate gradient iterations in
/// all (the multigrid preconditioner likes the matrix less).
const PENALTY: f64 = 30.0;

/// Pairs of faces on different bodies whose facets lie within `tolerance` (mm) of each
/// other and face each other, in the order of their bodies and labels. Facets without a
/// label are left out; a mesh that does not validate gives none.
pub fn detect_contacts(mesh: &TetMesh, tolerance: f64) -> Vec<ContactCandidate> {
    if mesh.validate().is_err() {
        return Vec::new();
    }
    let tolerance = if tolerance.is_finite() {
        tolerance.max(0.0)
    } else {
        0.0
    };
    struct Side {
        body: u32,
        facets: Vec<usize>,
        bvh: Bvh,
        lo: V3,
        hi: V3,
    }
    let mut by_body: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (k, f) in mesh.facets.iter().enumerate() {
        if f.label != crate::NO_LABEL {
            by_body.entry(mesh.facet_body(f)).or_default().push(k);
        }
    }
    let mut size = 0.0f64;
    let sides: Vec<Side> = by_body
        .into_iter()
        .map(|(body, facets)| {
            let triangles: Vec<[V3; 3]> = facets
                .iter()
                .map(|&k| corner_points(mesh, &mesh.facets[k]))
                .collect();
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            for p in triangles.iter().flatten() {
                for a in 0..3 {
                    lo[a] = lo[a].min(p[a]);
                    hi[a] = hi[a].max(p[a]);
                }
            }
            size = size.max(norm(sub(hi, lo)));
            Side {
                body,
                facets,
                bvh: Bvh::new(triangles),
                lo,
                hi,
            }
        })
        .collect();
    let slack = tolerance + 1e-9 * size;
    let near = |p: V3, s: &Side| (0..3).all(|a| p[a] >= s.lo[a] - slack && p[a] <= s.hi[a] + slack);
    // (body a, body b, label a, label b) → (area found on a, on b, largest gap).
    type Faces = (u32, u32, u32, u32);
    let mut found: BTreeMap<Faces, (f64, f64, f64)> = BTreeMap::new();
    for side in &sides {
        for other in &sides {
            if other.body == side.body
                || (0..3)
                    .any(|a| side.lo[a] > other.hi[a] + slack || other.lo[a] > side.hi[a] + slack)
            {
                continue;
            }
            for &k in &side.facets {
                let f = &mesh.facets[k];
                let c = corner_points(mesh, f);
                let centre = scale(add(add(c[0], c[1]), c[2]), 1.0 / 3.0);
                if !near(centre, other) {
                    continue;
                }
                let Some((t, d2)) = other.bvh.nearest(centre) else {
                    continue;
                };
                let Some(&g) = other.facets.get(t as usize) else {
                    continue;
                };
                let g = &mesh.facets[g];
                if d2 > slack * slack {
                    continue;
                }
                let an = mesh.facet_area_normal(f);
                let (Some(n), Some(m)) = (normalize(an), normalize(mesh.facet_area_normal(g)))
                else {
                    continue;
                };
                if dot(n, m) > FACING {
                    continue;
                }
                let (area, gap) = (norm(an), d2.sqrt());
                if side.body < other.body {
                    let e = found
                        .entry((side.body, other.body, f.label, g.label))
                        .or_insert((0.0, 0.0, 0.0));
                    e.0 += area;
                    e.2 = e.2.max(gap);
                } else {
                    let e = found
                        .entry((other.body, side.body, g.label, f.label))
                        .or_insert((0.0, 0.0, 0.0));
                    e.1 += area;
                    e.2 = e.2.max(gap);
                }
            }
        }
    }
    found
        .into_iter()
        .map(
            |((body_a, body_b, a_label, b_label), (on_a, on_b, gap))| ContactCandidate {
                body_a,
                a_label,
                body_b,
                b_label,
                area: on_a.max(on_b),
                gap,
            },
        )
        .collect()
}

fn corner_points(mesh: &TetMesh, f: &Facet) -> [V3; 3] {
    mesh.facet_corners(f).map(|i| mesh.nodes[i as usize])
}

/// A facet's nodes (corners, then edge nodes) and the share of its area each stands for.
fn facet_shares(mesh: &TetMesh, f: &Facet) -> ([u32; 6], [f64; 6], usize) {
    let area = norm(mesh.facet_area_normal(f));
    let c = mesh.facet_corners(f);
    match mesh.facet_edge_nodes(f) {
        Some(m) => {
            let (corner, edge) = (area / 12.0, area / 4.0);
            (
                [c[0], c[1], c[2], m[0], m[1], m[2]],
                [corner, corner, corner, edge, edge, edge],
                6,
            )
        }
        None => {
            let third = area / 3.0;
            (
                [c[0], c[1], c[2], 0, 0, 0],
                [third, third, third, 0.0, 0.0, 0.0],
                3,
            )
        }
    }
}

/// The barycentric coordinates of `p`, a point of the triangle.
fn barycentric(p: V3, t: &[V3; 3]) -> [f64; 3] {
    let (v0, v1, v2) = (sub(t[1], t[0]), sub(t[2], t[0]), sub(p, t[0]));
    let (d00, d01, d11) = (dot(v0, v0), dot(v0, v1), dot(v1, v1));
    let (d20, d21) = (dot(v2, v0), dot(v2, v1));
    let denominator = d00 * d11 - d01 * d01;
    if denominator.is_nan() || denominator <= 0.0 {
        return [1.0 / 3.0; 3];
    }
    let v = ((d11 * d20 - d01 * d21) / denominator).clamp(0.0, 1.0);
    let w = ((d00 * d21 - d01 * d20) / denominator).clamp(0.0, 1.0);
    let sum = v + w;
    let (v, w) = if sum > 1.0 {
        (v / sum, w / sum)
    } else {
        (v, w)
    };
    [1.0 - v - w, v, w]
}

/// A facet's nodes and their shape functions at barycentric coordinates `l`: the
/// three-node or six-node triangle's, as the elements interpolate on their faces.
fn facet_interpolation(mesh: &TetMesh, f: &Facet, l: [f64; 3]) -> ([u32; 6], [f64; 6], usize) {
    let c = mesh.facet_corners(f);
    match mesh.facet_edge_nodes(f) {
        Some(m) => (
            [c[0], c[1], c[2], m[0], m[1], m[2]],
            [
                l[0] * (2.0 * l[0] - 1.0),
                l[1] * (2.0 * l[1] - 1.0),
                l[2] * (2.0 * l[2] - 1.0),
                4.0 * l[0] * l[1],
                4.0 * l[1] * l[2],
                4.0 * l[2] * l[0],
            ],
            6,
        ),
        None => (
            [c[0], c[1], c[2], 0, 0, 0],
            [l[0], l[1], l[2], 0.0, 0.0, 0.0],
            3,
        ),
    }
}

/// One slave node of a contact, tied to a point on a master facet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Tie {
    /// The contact's index in the study.
    pub contact: usize,
    pub kind: ContactKind,
    pub slave: u32,
    /// The master facet's nodes and their shape functions at the point.
    pub masters: [u32; 6],
    pub shape: [f64; 6],
    pub count: usize,
    /// The master's outward unit normal there, towards the slave side.
    pub normal: V3,
    /// The slave area the node stands for, mm².
    pub area: f64,
    /// ε, MPa/mm.
    pub penalty: f64,
}

impl Tie {
    /// The nodes in the gap and their coefficients: +1 for the slave, −N for each master.
    pub fn terms(&self) -> impl Iterator<Item = (u32, f64)> + '_ {
        std::iter::once((self.slave, 1.0)).chain(
            self.masters[..self.count]
                .iter()
                .zip(&self.shape[..self.count])
                .map(|(&m, &n)| (m, -n)),
        )
    }

    /// The slave's displacement relative to the point it is tied to, mm.
    pub fn gap(&self, u: &[V3]) -> V3 {
        self.terms().fold([0.0; 3], |g, (i, c)| {
            add(g, scale(u.get(i as usize).copied().unwrap_or([0.0; 3]), c))
        })
    }

    /// The part of `v` the tie holds: all of it, or its normal part.
    pub fn held(&self, v: V3) -> V3 {
        if self.kind.ties_tangent() {
            v
        } else {
            scale(self.normal, dot(v, self.normal))
        }
    }

    /// The directions it holds, for the rigid-body check.
    pub fn directions(&self) -> Vec<V3> {
        if self.kind.ties_tangent() {
            vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        } else {
            vec![self.normal]
        }
    }
}

/// The ties of a study's contacts, and what the results need to report on them.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pairing {
    pub ties: Vec<Tie>,
    pub sides: Vec<Sides>,
    /// Per tie, the ties (and weights) whose normal tractions average into its own over
    /// its slave facets: what decides whether it opens.
    pub smoothing: Vec<Vec<(usize, f64)>>,
}

/// One contact's two sides.
#[derive(Debug, Clone, Default)]
pub(crate) struct Sides {
    pub slave_is_a: bool,
    pub slave: Vec<usize>,
    pub master: Vec<usize>,
    pub paired: usize,
    pub unpaired: usize,
    /// The search distance's offset, mm.
    pub offset: f64,
    /// Whether the faces may part (and so never pull on each other).
    pub may_separate: bool,
}

/// Pairs the slave nodes of every contact with the master side.
pub(crate) fn pair(mesh: &TetMesh, contacts: &[Contact], materials: &[Elastic]) -> Pairing {
    let modulus = |f: &Facet| {
        materials
            .get(mesh.facet_body(f) as usize)
            .map_or(0.0, |m| m.youngs_modulus)
    };
    let mut pairing = Pairing::default();
    for (c, contact) in contacts.iter().enumerate() {
        let side = |labels: &[u32]| -> Vec<usize> {
            (0..mesh.facets.len())
                .filter(|&k| {
                    let label = mesh.facets[k].label;
                    label != crate::NO_LABEL && labels.contains(&label)
                })
                .collect()
        };
        let (on_a, on_b) = (side(&contact.a_labels), side(&contact.b_labels));
        let mean = |facets: &[usize]| {
            if facets.is_empty() {
                return f64::INFINITY;
            }
            facets
                .iter()
                .map(|&k| norm(mesh.facet_area_normal(&mesh.facets[k])))
                .sum::<f64>()
                / facets.len() as f64
        };
        // The finer side is the slave: its nodes are the ones held.
        let slave_is_a = mean(&on_a) <= mean(&on_b);
        let (slave, master) = if slave_is_a {
            (on_a, on_b)
        } else {
            (on_b, on_a)
        };
        let offset = if contact.offset.is_finite() {
            contact.offset.max(0.0)
        } else {
            0.0
        };
        let mut sides = Sides {
            slave_is_a,
            offset,
            may_separate: contact.kind.may_separate(),
            ..Sides::default()
        };
        if !slave.is_empty() && !master.is_empty() {
            // Per slave node: its area, the sum of its facets' sizes and their count, the
            // smallest modulus around it.
            let mut nodes: BTreeMap<u32, (f64, f64, u32, f64)> = BTreeMap::new();
            for &k in &slave {
                let f = &mesh.facets[k];
                let h = (2.0 * norm(mesh.facet_area_normal(f))).sqrt();
                let e = modulus(f);
                let (ids, shares, count) = facet_shares(mesh, f);
                for (&i, &w) in ids[..count].iter().zip(&shares[..count]) {
                    let entry = nodes.entry(i).or_insert((0.0, 0.0, 0, f64::INFINITY));
                    entry.0 += w;
                    entry.1 += h;
                    entry.2 += 1;
                    entry.3 = entry.3.min(e);
                }
            }
            let bvh = Bvh::new(
                master
                    .iter()
                    .map(|&k| corner_points(mesh, &mesh.facets[k]))
                    .collect(),
            );
            for (&node, &(area, sizes, count, e_slave)) in &nodes {
                let h = sizes / f64::from(count.max(1));
                let p = mesh.nodes[node as usize];
                let reach = offset + REACH * h;
                let found = bvh
                    .nearest(p)
                    .filter(|&(_, d2)| d2 <= reach * reach)
                    .and_then(|(t, _)| master.get(t as usize));
                let Some(&k) = found else {
                    sides.unpaired += 1;
                    continue;
                };
                let g = &mesh.facets[k];
                let triangle = corner_points(mesh, g);
                let l = barycentric(closest_point(p, &triangle), &triangle);
                let (masters, shape, n) = facet_interpolation(mesh, g, l);
                let normal = normalize(mesh.facet_area_normal(g));
                let e = e_slave.min(modulus(g));
                let (Some(normal), true) = (normal, !masters[..n].contains(&node)) else {
                    sides.unpaired += 1;
                    continue;
                };
                if !(e > 0.0 && e.is_finite() && h > 0.0 && area > 0.0) {
                    sides.unpaired += 1;
                    continue;
                }
                pairing.ties.push(Tie {
                    contact: c,
                    kind: contact.kind,
                    slave: node,
                    masters,
                    shape,
                    count: n,
                    normal,
                    area,
                    penalty: PENALTY * e / h,
                });
                sides.paired += 1;
            }
        }
        smooth(mesh, &slave, &mut pairing, c);
        sides.slave = slave;
        sides.master = master;
        pairing.sides.push(sides);
    }
    pairing
}

/// Fills in the smoothing weights of contact `c`'s ties: a node's traction averaged over
/// its slave facets, each facet's traction being its nodes' weighted by their area
/// shares. On ten-node faces the corner nodes carry no force under a uniform pressure
/// and the edge nodes a third more than their share: the average is even.
fn smooth(mesh: &TetMesh, slave: &[usize], pairing: &mut Pairing, c: usize) {
    let tie_of: BTreeMap<u32, usize> = pairing
        .ties
        .iter()
        .enumerate()
        .filter(|(_, t)| t.contact == c)
        .map(|(k, t)| (t.slave, k))
        .collect();
    pairing.smoothing.resize(pairing.ties.len(), Vec::new());
    let mut total: BTreeMap<usize, f64> = BTreeMap::new();
    for &f in slave {
        let f = &mesh.facets[f];
        let area = norm(mesh.facet_area_normal(f));
        let (ids, shares, count) = facet_shares(mesh, f);
        for &i in &ids[..count] {
            let Some(&k) = tie_of.get(&i) else {
                continue;
            };
            *total.entry(k).or_insert(0.0) += area;
            for (&j, &w) in ids[..count].iter().zip(&shares[..count]) {
                let Some(&other) = tie_of.get(&j) else {
                    continue;
                };
                let list = &mut pairing.smoothing[k];
                match list.iter_mut().find(|e| e.0 == other) {
                    Some(e) => e.1 += w,
                    None => list.push((other, w)),
                }
            }
        }
    }
    for (k, area) in total {
        if area > 0.0 {
            for e in &mut pairing.smoothing[k] {
                e.1 /= area;
            }
        }
    }
}

/// The augmented Lagrangian's state: a multiplier (a traction, MPa) per tie, and whether
/// it is closed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Multipliers {
    pub lambda: Vec<V3>,
    pub active: Vec<bool>,
    /// Times each tie has opened.
    opened: Vec<u8>,
}

impl Multipliers {
    /// No traction yet, every tie closed: faces that touch start out pressing.
    pub fn new(ties: &[Tie]) -> Multipliers {
        Multipliers {
            lambda: vec![[0.0; 3]; ties.len()],
            active: vec![true; ties.len()],
            opened: vec![0; ties.len()],
        }
    }

    /// Adds the multipliers' nodal forces, `−w Gᵀ λ`, to `f`.
    pub fn add_forces(&self, ties: &[Tie], f: &mut [V3]) {
        for (k, tie) in ties.iter().enumerate() {
            if !self.active[k] {
                continue;
            }
            let force = scale(self.lambda[k], -tie.area);
            for (i, c) in tie.terms() {
                if let Some(v) = f.get_mut(i as usize) {
                    *v = add(*v, scale(force, c));
                }
            }
        }
    }

    /// One Uzawa update from the displacements `u` the current state gave: the closed
    /// ties' multipliers move by `ε g` (their held part). The largest gap left where a
    /// tie is closed and the largest penetration where one is open, mm: the first is
    /// what the updates reduce, the second what [`Multipliers::decide`] acts on.
    pub fn update(&mut self, ties: &[Tie], u: &[V3]) -> (f64, f64) {
        let (mut gap, mut penetration) = (0.0f64, 0.0f64);
        for (k, tie) in ties.iter().enumerate() {
            let g = tie.gap(u);
            if self.active[k] {
                let held = tie.held(g);
                gap = gap.max(norm(held));
                self.lambda[k] = add(self.lambda[k], scale(held, tie.penalty));
            } else {
                penetration = penetration.max(-dot(g, tie.normal));
            }
        }
        (gap, penetration)
    }

    /// Opens the separation and rough ties that pull and closes those that penetrate,
    /// once the multipliers have converged for the current state: whether any changed.
    ///
    /// `tolerance` (mm) is the violation the caller accepts. A tie opens only when it
    /// pulls by more than the traction of that gap, and closes only when it penetrates by
    /// more, so that nodes on the edge of the closed zone do not flip on rounding alone.
    /// The traction deciding is converged (not each update's) and averaged over the
    /// node's facets: ten-node faces' corner nodes carry no force under a uniform
    /// pressure, and would otherwise open inside a pressed zone and close again. A tie
    /// that has opened [`MAX_OPENINGS`] times stays closed, which ends any cycle.
    pub fn decide(&mut self, pairing: &Pairing, u: &[V3], tolerance: f64) -> bool {
        let ties = &pairing.ties;
        let pull: Vec<f64> = (0..ties.len())
            .map(|k| {
                pairing.smoothing.get(k).map_or(0.0, |list| {
                    list.iter()
                        .map(|&(j, w)| w * dot(self.lambda[j], ties[j].normal))
                        .sum()
                })
            })
            .collect();
        let mut changed = false;
        for (k, tie) in ties.iter().enumerate() {
            if !tie.kind.may_separate() {
                continue;
            }
            if self.active[k] {
                if pull[k] > tie.penalty * tolerance && self.opened[k] < MAX_OPENINGS {
                    self.active[k] = false;
                    self.lambda[k] = [0.0; 3];
                    self.opened[k] += 1;
                    changed = true;
                }
            } else if dot(tie.gap(u), tie.normal) < -tolerance {
                self.active[k] = true;
                changed = true;
            }
        }
        changed
    }
}

/// The per-contact results and the contact pressure at every node (MPa, pressing
/// positive; on the nodes of both sides).
pub(crate) fn outcome(
    mesh: &TetMesh,
    pairing: &Pairing,
    multipliers: &Multipliers,
) -> (Vec<ContactResult>, Vec<f64>) {
    let n = mesh.nodes.len();
    let mut pressure = vec![0.0f64; n];
    let mut results = Vec::with_capacity(pairing.sides.len());
    let mut nodal = vec![0.0f64; n];
    let mut acc = vec![0.0f64; n];
    let mut weight = vec![0.0f64; n];
    let mut closed = vec![false; n];
    for (c, sides) in pairing.sides.iter().enumerate() {
        let mut result = ContactResult {
            paired: sides.paired,
            unpaired: sides.unpaired,
            force: [0.0; 3],
            normal_force: 0.0,
            area_in_contact: 0.0,
            max_pressure: 0.0,
        };
        let mut touched: Vec<u32> = Vec::new();
        for (k, tie) in pairing.ties.iter().enumerate() {
            if tie.contact != c || !multipliers.active[k] {
                continue;
            }
            let lambda = multipliers.lambda[k];
            let p = -dot(lambda, tie.normal);
            result.force = add(result.force, scale(lambda, -tie.area));
            result.normal_force += p * tie.area;
            nodal[tie.slave as usize] = p;
            closed[tie.slave as usize] = true;
            touched.push(tie.slave);
        }
        if !sides.slave_is_a {
            result.force = scale(result.force, -1.0);
        }
        // Facet averages with the nodes' area shares, then back to the nodes.
        for &k in &sides.slave {
            let f = &mesh.facets[k];
            let area = norm(mesh.facet_area_normal(f));
            if area <= 0.0 {
                continue;
            }
            let (ids, shares, count) = facet_shares(mesh, f);
            let on: f64 = ids[..count]
                .iter()
                .zip(&shares[..count])
                .filter(|(i, _)| closed[**i as usize])
                .map(|(_, w)| w)
                .sum();
            if on >= 0.5 * area {
                result.area_in_contact += area;
            }
            let mut mean = ids[..count]
                .iter()
                .zip(&shares[..count])
                .map(|(&i, &w)| w * nodal[i as usize])
                .sum::<f64>()
                / area;
            // A ten-node corner next to a lifted zone can carry a little pull, which the
            // facets around it make up for; a contact that cannot pull shows none.
            if sides.may_separate {
                mean = mean.max(0.0);
            }
            for &i in &ids[..count] {
                acc[i as usize] += area * mean;
                weight[i as usize] += area;
            }
        }
        let mut smoothed: Vec<(u32, f64)> = Vec::new();
        for &k in &sides.slave {
            let (ids, _, count) = facet_shares(mesh, &mesh.facets[k]);
            for &i in &ids[..count] {
                let i = i as usize;
                if weight[i] > 0.0 {
                    smoothed.push((i as u32, acc[i] / weight[i]));
                    acc[i] = 0.0;
                    weight[i] = 0.0;
                }
            }
        }
        for &i in &touched {
            nodal[i as usize] = 0.0;
            closed[i as usize] = false;
        }
        let put = |i: u32, p: f64, pressure: &mut [f64]| {
            if let Some(slot) = pressure.get_mut(i as usize)
                && p.abs() > slot.abs()
            {
                *slot = p;
            }
        };
        for &(i, p) in &smoothed {
            put(i, p, &mut pressure);
            nodal[i as usize] = p;
            result.max_pressure = result.max_pressure.max(p);
        }
        // Over to the master side, at the nearest slave point.
        if !sides.slave.is_empty() {
            let bvh = Bvh::new(
                sides
                    .slave
                    .iter()
                    .map(|&k| corner_points(mesh, &mesh.facets[k]))
                    .collect(),
            );
            let mut seen: Vec<u32> = Vec::new();
            for &k in &sides.master {
                let f = &mesh.facets[k];
                let h = (2.0 * norm(mesh.facet_area_normal(f))).sqrt();
                let (ids, _, count) = facet_shares(mesh, f);
                for &i in &ids[..count] {
                    seen.push(i);
                    let p = mesh.nodes[i as usize];
                    let reach = sides.offset + REACH * h;
                    let Some((t, d2)) = bvh.nearest(p) else {
                        continue;
                    };
                    let Some(&s) = sides.slave.get(t as usize) else {
                        continue;
                    };
                    if d2 > reach * reach {
                        continue;
                    }
                    let g = &mesh.facets[s];
                    let triangle = corner_points(mesh, g);
                    let l = barycentric(closest_point(p, &triangle), &triangle);
                    let corners = mesh.facet_corners(g);
                    let value = (0..3).map(|j| l[j] * nodal[corners[j] as usize]).sum();
                    put(i, value, &mut pressure);
                }
            }
        }
        for &(i, _) in &smoothed {
            nodal[i as usize] = 0.0;
        }
        results.push(result);
    }
    (results, pressure)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::solve::{Fixture, FixtureKind, Load, LoadKind, Results, SolveOptions, Study, solve};

    const STEEL: Elastic = Elastic::new(200_000.0, 0.3, 250.0, 7850.0);

    /// Two boxes meshed apart, the second's labels moved up by 10: one mesh, bodies 0 and 1.
    fn two_boxes(a: (V3, V3, [usize; 3]), b: (V3, V3, [usize; 3]), quadratic: bool) -> TetMesh {
        let mut mesh = TetMesh::structured_box(a.0, a.1, a.2);
        let mut other = TetMesh::structured_box(b.0, b.1, b.2);
        other.bodies.fill(1);
        for f in &mut other.facets {
            f.label += 10;
        }
        mesh.append(other);
        if quadratic {
            mesh.make_quadratic();
        }
        mesh
    }

    fn run(mesh: &TetMesh, study: &Study) -> (Results, f64) {
        let cancel = AtomicBool::new(false);
        let start = std::time::Instant::now();
        let r = solve(
            mesh,
            &[STEEL, STEEL],
            study,
            &SolveOptions::default(),
            &cancel,
            &|_| {},
        )
        .unwrap();
        (r, start.elapsed().as_secs_f64())
    }

    fn node_at(mesh: &TetMesh, p: V3, body: Option<u32>) -> usize {
        let mut on_body = vec![body.is_none(); mesh.nodes.len()];
        if let Some(b) = body {
            for (e, t) in mesh.tets.iter().enumerate() {
                if mesh.bodies[e] == b {
                    for &i in t {
                        on_body[i as usize] = true;
                    }
                }
            }
        }
        (0..mesh.nodes.len())
            .filter(|&i| on_body[i])
            .min_by(|&a, &b| norm(sub(mesh.nodes[a], p)).total_cmp(&norm(sub(mesh.nodes[b], p))))
            .unwrap()
    }

    fn fixture(kind: FixtureKind, labels: &[u32]) -> Fixture {
        Fixture {
            kind,
            labels: labels.to_vec(),
        }
    }

    fn force(total: f64, direction: V3, labels: &[u32]) -> Load {
        Load {
            kind: LoadKind::Force {
                total,
                direction: Some(direction),
            },
            labels: labels.to_vec(),
        }
    }

    fn contact(kind: ContactKind, a: &[u32], b: &[u32], offset: f64) -> Contact {
        Contact {
            kind,
            a_labels: a.to_vec(),
            b_labels: b.to_vec(),
            offset,
        }
    }

    /// The tip deflection of a 200 × 20 × 10 mm cantilever along X, fixed at x = 0 and
    /// loaded by 1 kN down on its end, made of two blocks: [0, 100] and [100 + gap, 200 +
    /// gap], tied by `kind`. The second block's mesh is finer and does not match.
    fn split_cantilever(kind: ContactKind, gap: f64) -> (f64, Results, f64) {
        let mesh = two_boxes(
            ([0.0; 3], [100.0, 20.0, 10.0], [10, 4, 2]),
            (
                [100.0 + gap, 0.0, 0.0],
                [200.0 + gap, 20.0, 10.0],
                [12, 5, 3],
            ),
            true,
        );
        let study = Study {
            fixtures: vec![fixture(FixtureKind::Fixed, &[0])],
            loads: vec![force(1000.0, [0.0, -1.0, 0.0], &[11])],
            contacts: vec![contact(kind, &[1], &[10], gap)],
            gravity: None,
        };
        let (r, seconds) = run(&mesh, &study);
        let tip = node_at(&mesh, [200.0 + gap, 10.0, 5.0], None);
        (-r.displacement[tip][1], r, seconds)
    }

    #[test]
    fn two_blocks_bonded_by_contact_bend_as_one() {
        let one = TetMesh::structured_box([0.0; 3], [200.0, 20.0, 10.0], [20, 4, 2]);
        let mut one = one;
        one.make_quadratic();
        let study = Study {
            fixtures: vec![fixture(FixtureKind::Fixed, &[0])],
            loads: vec![force(1000.0, [0.0, -1.0, 0.0], &[1])],
            ..Study::default()
        };
        let (whole, whole_seconds) = run(&one, &study);
        let expected = -whole.displacement[node_at(&one, [200.0, 10.0, 5.0], None)][1];
        let (delta, r, seconds) = split_cantilever(ContactKind::Bonded, 0.0);
        eprintln!(
            "bonded: one block {expected:.6} mm ({} iterations, {whole_seconds:.2} s), two blocks {delta:.6} mm ({} CG iterations, {} contact solves, violation {:.1e} mm, {seconds:.2} s)",
            whole.iterations, r.iterations, r.contact_solve.iterations, r.contact_solve.violation
        );
        assert!(r.contact_solve.converged);
        assert!(
            (delta - expected).abs() < 0.01 * expected,
            "two blocks {delta} mm, one {expected} mm"
        );
        // The seam carries the shear force: the second block pushes the first one down
        // with the whole 1 kN, and no net force along the beam.
        let c = r.contacts[0];
        assert!(c.paired > 0 && c.unpaired == 0, "{c:?}");
        assert!((c.force[1] + 1000.0).abs() < 5.0, "{c:?}");
        assert!(c.force[0].abs() < 5.0 && c.force[2].abs() < 5.0, "{c:?}");
    }

    #[test]
    fn offset_bonded_bridges_a_gap() {
        let (bonded, _, _) = split_cantilever(ContactKind::Bonded, 0.0);
        let (offset, r, seconds) = split_cantilever(ContactKind::OffsetBonded, 0.5);
        eprintln!(
            "offset bonded across 0.5 mm: {offset:.6} mm against bonded {bonded:.6} mm ({} contact solves, {seconds:.2} s)",
            r.contact_solve.iterations
        );
        assert!(r.contact_solve.converged);
        assert!(r.contacts[0].unpaired == 0, "{:?}", r.contacts[0]);
        // The beam is 0.5 mm longer, but rigid over that length: within 2 %.
        assert!(
            (offset - bonded).abs() < 0.02 * bonded,
            "{offset} vs {bonded}"
        );
        // A 2 mm gap without an offset is too wide to pair anything: the second block
        // floats and is refused.
        let mesh = two_boxes(
            ([0.0; 3], [100.0, 20.0, 10.0], [10, 4, 2]),
            ([102.0, 0.0, 0.0], [202.0, 20.0, 10.0], [12, 5, 3]),
            true,
        );
        let study = Study {
            fixtures: vec![fixture(FixtureKind::Fixed, &[0])],
            loads: vec![force(1000.0, [0.0, -1.0, 0.0], &[11])],
            contacts: vec![contact(ContactKind::Bonded, &[1], &[10], 0.0)],
            gravity: None,
        };
        let cancel = AtomicBool::new(false);
        let refused = solve(
            &mesh,
            &[STEEL, STEEL],
            &study,
            &SolveOptions::default(),
            &cancel,
            &|_| {},
        );
        assert_eq!(refused, Err(crate::SolveError::NotHeld { body: 1 }));
    }

    /// A 10 × 10 × 10 mm block on another (the lower one 0 ≤ y ≤ 10, held by rollers on
    /// its bottom and on the sides x = 0 and z = 0), with meshes that do not match.
    fn stacked(quadratic: bool) -> TetMesh {
        two_boxes(
            ([0.0; 3], [10.0, 10.0, 10.0], [3, 3, 3]),
            ([0.0, 10.0, 0.0], [10.0, 20.0, 10.0], [4, 4, 4]),
            quadratic,
        )
    }

    #[test]
    fn a_block_pressed_on_another_carries_p_over_a() {
        let mesh = stacked(true);
        // 1 kN down on the top block's top: 10 MPa through the interface.
        let study = Study {
            fixtures: vec![
                fixture(FixtureKind::Roller, &[2, 0, 4]),
                fixture(FixtureKind::Roller, &[10, 14]),
            ],
            loads: vec![force(1000.0, [0.0, -1.0, 0.0], &[13])],
            contacts: vec![contact(ContactKind::Separation, &[3], &[12], 0.0)],
            gravity: None,
        };
        let (r, seconds) = run(&mesh, &study);
        let c = r.contacts[0];
        eprintln!(
            "separation, pressed: {} contact solves, {} rebuilds, violation {:.1e} mm, normal force {:.3} N, max pressure {:.3} MPa, {seconds:.2} s",
            r.contact_solve.iterations,
            r.contact_solve.rebuilds,
            r.contact_solve.violation,
            c.normal_force,
            c.max_pressure
        );
        assert!(r.contact_solve.converged);
        assert!((c.normal_force - 1000.0).abs() < 1.0, "{c:?}");
        assert!((c.area_in_contact - 100.0).abs() < 1e-6, "{c:?}");
        // The pressure is P/A everywhere on the interface, on both sides.
        for (i, p) in mesh.nodes.iter().enumerate() {
            if (p[1] - 10.0).abs() < 1e-9 {
                assert!(
                    (r.contact_pressure[i] - 10.0).abs() < 0.3,
                    "{p:?}: {}",
                    r.contact_pressure[i]
                );
            }
        }
        // And the column shortens by σ L / E over its 20 mm.
        let top = node_at(&mesh, [5.0, 20.0, 5.0], Some(1));
        let expected = 10.0 * 20.0 / 200_000.0;
        assert!(
            (r.displacement[top][1] + expected).abs() < 1e-3 * expected,
            "{:?} vs {expected}",
            r.displacement[top]
        );
    }

    #[test]
    fn a_block_pulled_off_another_parts_from_it() {
        // The same 1 kN pulling the faces apart: the top block held by its top face, its
        // bottom face lifted. The contact opens, carries nothing, and the top block
        // deforms as if it were alone.
        let mesh = stacked(true);
        let study = Study {
            fixtures: vec![
                fixture(FixtureKind::Roller, &[2, 0, 4]),
                fixture(FixtureKind::Fixed, &[13]),
            ],
            loads: vec![force(1000.0, [0.0, 1.0, 0.0], &[12])],
            contacts: vec![contact(ContactKind::Separation, &[3], &[12], 0.0)],
            gravity: None,
        };
        let (r, seconds) = run(&mesh, &study);
        let c = r.contacts[0];
        eprintln!(
            "separation, pulled: {} contact solves, {} rebuilds, violation {:.1e} mm, {seconds:.2} s",
            r.contact_solve.iterations, r.contact_solve.rebuilds, r.contact_solve.violation
        );
        assert!(r.contact_solve.converged);
        assert_eq!(c.normal_force, 0.0, "{c:?}");
        assert_eq!(c.area_in_contact, 0.0, "{c:?}");
        assert!(r.contact_pressure.iter().all(|&p| p == 0.0));
        // The lower block is unloaded; the top one lifted off it.
        let lower = node_at(&mesh, [10.0, 10.0, 10.0], Some(0));
        assert!(norm(r.displacement[lower]) < 1e-12);
        let middle = node_at(&mesh, [5.0, 10.0, 5.0], Some(1));
        assert!(
            r.displacement[middle][1] > 1e-5,
            "{:?}",
            r.displacement[middle]
        );
        // The same as the top block on its own.
        let mut alone = TetMesh::structured_box([0.0, 10.0, 0.0], [10.0, 20.0, 10.0], [4, 4, 4]);
        alone.make_quadratic();
        let study = Study {
            fixtures: vec![fixture(FixtureKind::Fixed, &[3])],
            loads: vec![force(1000.0, [0.0, 1.0, 0.0], &[2])],
            ..Study::default()
        };
        let (a, _) = run(&alone, &study);
        for p in [[5.0, 10.0, 5.0], [10.0, 10.0, 10.0], [0.0, 15.0, 0.0]] {
            let (i, j) = (node_at(&alone, p, None), node_at(&mesh, p, Some(1)));
            let size = norm(a.displacement[i]);
            assert!(
                norm(sub(a.displacement[i], r.displacement[j])) < 1e-6 * size,
                "{p:?}: {:?} vs {:?}",
                a.displacement[i],
                r.displacement[j]
            );
        }
    }

    /// The top block of [`stacked`] held by rollers on its sides x = 0 and z = 0 only (the
    /// lower one fixed at its bottom), pulled along +X by 1 kN on its side x = 10.
    fn pulled_along(kind: ContactKind) -> (TetMesh, Results, f64) {
        let mesh = stacked(true);
        let study = Study {
            fixtures: vec![
                fixture(FixtureKind::Fixed, &[2]),
                fixture(FixtureKind::Roller, &[10, 14]),
            ],
            loads: vec![force(1000.0, [1.0, 0.0, 0.0], &[11])],
            contacts: vec![contact(kind, &[3], &[12], 0.0)],
            gravity: None,
        };
        let (r, seconds) = run(&mesh, &study);
        (mesh, r, seconds)
    }

    #[test]
    fn sliding_lets_a_block_slide_without_friction() {
        let (mesh, r, seconds) = pulled_along(ContactKind::Sliding);
        eprintln!(
            "sliding: {} contact solves, violation {:.1e} mm, {seconds:.2} s",
            r.contact_solve.iterations, r.contact_solve.violation
        );
        assert!(r.contact_solve.converged);
        // A bar in tension, nothing taken by the interface: the far side moves PL/EA,
        // the lower block not at all.
        let expected = 1000.0 * 10.0 / (200_000.0 * 100.0);
        for (i, p) in mesh.nodes.iter().enumerate() {
            if (p[0] - 10.0).abs() < 1e-9 && p[1] > 10.0 + 1e-9 {
                assert!(
                    (r.displacement[i][0] - expected).abs() < 1e-3 * expected,
                    "{p:?}: {:?}",
                    r.displacement[i]
                );
            }
        }
        let lower = node_at(&mesh, [10.0, 10.0, 10.0], Some(0));
        assert!(norm(r.displacement[lower]) < 1e-4 * expected);
        assert!(r.contacts[0].force[0].abs() < 1e-3, "{:?}", r.contacts[0]);
    }

    /// The top block of [`stacked`] held by nothing but the contact, pressed down by 1 kN
    /// on its top and pushed along +X by `push` on its side x = 0.
    fn pushed(kind: ContactKind, push: f64) -> Result<(TetMesh, Results, f64), crate::SolveError> {
        let mesh = stacked(true);
        let study = Study {
            fixtures: vec![fixture(FixtureKind::Fixed, &[2])],
            loads: vec![
                force(1000.0, [0.0, -1.0, 0.0], &[13]),
                force(push, [1.0, 0.0, 0.0], &[10]),
            ],
            contacts: vec![contact(kind, &[3], &[12], 0.0)],
            gravity: None,
        };
        let cancel = AtomicBool::new(false);
        let start = std::time::Instant::now();
        let r = solve(
            &mesh,
            &[STEEL, STEEL],
            &study,
            &SolveOptions::default(),
            &cancel,
            &|_| {},
        )?;
        Ok((mesh, r, start.elapsed().as_secs_f64()))
    }

    #[test]
    fn rough_holds_the_block_where_sliding_lets_it_go() {
        // Without friction nothing holds the block along the plane: refused.
        assert_eq!(
            pushed(ContactKind::Sliding, 500.0).err(),
            Some(crate::SolveError::NotHeld { body: 1 })
        );
        assert_eq!(
            pushed(ContactKind::Separation, 500.0).err(),
            Some(crate::SolveError::NotHeld { body: 1 })
        );
        // Rough holds it. Pushed at mid-height, 5 mm above the interface, the block tips:
        // the resultant of the contact pressure sits 2.5 mm from the middle, beyond the
        // middle third, so the face's back part (x near 0) lifts, as on a rigid base a
        // quarter of it would.
        let (mesh, rough, seconds) = pushed(ContactKind::Rough, 500.0).unwrap();
        let (_, bonded, _) = pushed(ContactKind::Bonded, 500.0).unwrap();
        let c = rough.contacts[0];
        eprintln!(
            "rough: {} contact solves, {} rebuilds, violation {:.1e} mm, {} CG iterations, {seconds:.2} s; area in contact {:.1} mm², force {:?}",
            rough.contact_solve.iterations,
            rough.contact_solve.rebuilds,
            rough.contact_solve.violation,
            rough.iterations,
            c.area_in_contact,
            c.force
        );
        assert!(rough.contact_solve.converged);
        // The whole push and the whole weight go through the interface (onto side a, the
        // lower block).
        assert!((c.force[0] - 500.0).abs() < 1.0, "{c:?}");
        assert!((c.force[1] + 1000.0).abs() < 1.0, "{c:?}");
        assert!((c.normal_force - 1000.0).abs() < 1.0, "{c:?}");
        assert!(
            c.area_in_contact > 50.0 && c.area_in_contact < 95.0,
            "{c:?}"
        );
        // Where it lifts, nothing presses; the lifted corner rose off the lower block.
        let back = node_at(&mesh, [0.0, 10.0, 5.0], Some(1));
        let below = node_at(&mesh, [0.0, 10.0, 5.0], Some(0));
        assert!(rough.contact_pressure[back].abs() < 1e-3);
        assert!(rough.displacement[back][1] > rough.displacement[below][1] + 1e-6);
        // It may part where bonded pulls: so it is no stiffer than bonded, and close.
        let front = node_at(&mesh, [10.0, 15.0, 5.0], Some(1));
        let (r, b) = (rough.displacement[front][0], bonded.displacement[front][0]);
        assert!(
            r >= b * (1.0 - 1e-6) && r < 1.5 * b,
            "rough {r} vs bonded {b}"
        );
        assert!(bonded.contacts[0].area_in_contact > 99.9);
    }

    #[test]
    fn touching_faces_are_found_and_others_are_not() {
        // Two blocks touching along x = 10, a third 0.5 mm above the first, a fourth
        // meeting the first along an edge only.
        let mut mesh = two_boxes(
            ([0.0; 3], [10.0, 10.0, 10.0], [2, 2, 2]),
            ([10.0, 0.0, 0.0], [20.0, 10.0, 10.0], [3, 3, 3]),
            false,
        );
        let mut third = TetMesh::structured_box([0.0, 10.5, 0.0], [10.0, 20.0, 10.0], [2, 2, 2]);
        third.bodies.fill(2);
        for f in &mut third.facets {
            f.label += 20;
        }
        mesh.append(third);
        let mut fourth = TetMesh::structured_box([-10.0, -10.0, 0.0], [0.0, 0.0, 10.0], [2, 2, 2]);
        fourth.bodies.fill(3);
        for f in &mut fourth.facets {
            f.label += 30;
        }
        mesh.append(fourth);
        let found = detect_contacts(&mesh, 0.1);
        assert_eq!(found.len(), 1, "{found:?}");
        let c = found[0];
        assert_eq!((c.body_a, c.a_label, c.body_b, c.b_label), (0, 1, 1, 10));
        assert!((c.area - 100.0).abs() < 1e-6 && c.gap < 1e-9, "{c:?}");
        let wider = detect_contacts(&mesh, 1.0);
        assert_eq!(wider.len(), 2, "{wider:?}");
        let gap = wider[1];
        assert_eq!(
            (gap.body_a, gap.a_label, gap.body_b, gap.b_label),
            (0, 3, 2, 22)
        );
        assert!((gap.gap - 0.5).abs() < 1e-9, "{gap:?}");
        assert_eq!(gap.contact(ContactKind::OffsetBonded).offset, gap.gap);
    }

    /// The mesher's own meshes of two touching blocks, each on its own lattice, at element
    /// size `size`: the seam is found and a bonded contact makes them one cantilever again.
    fn mesher_cantilever(size: f64) {
        let surfaces = [crate::mesher::box_surface([0.0; 3], [100.0, 20.0, 20.0]), {
            let mut s = crate::mesher::box_surface([100.0, 0.0, 0.0], [200.0, 20.0, 20.0]);
            for l in &mut s.labels {
                *l += 10;
            }
            s
        }];
        let cancel = AtomicBool::new(false);
        let options = crate::MeshOptions {
            element_size: size,
            ..crate::MeshOptions::default()
        };
        let start = std::time::Instant::now();
        let (mesh, _) = crate::mesh_solids(&surfaces, &options, &cancel, &|_| {}).unwrap();
        let mesh_seconds = start.elapsed().as_secs_f64();
        let found = detect_contacts(&mesh, 0.01);
        let seam: Vec<&ContactCandidate> = found.iter().filter(|c| c.area > 50.0).collect();
        assert_eq!(seam.len(), 1, "{found:?}");
        assert_eq!((seam[0].a_label, seam[0].b_label), (1, 10));
        let study = Study {
            fixtures: vec![fixture(FixtureKind::Fixed, &[0])],
            loads: vec![force(1000.0, [0.0, -1.0, 0.0], &[11])],
            contacts: vec![seam[0].contact(ContactKind::Bonded)],
            gravity: None,
        };
        let start = std::time::Instant::now();
        let pairing = pair(&mesh, &study.contacts, &[STEEL, STEEL]);
        let pair_seconds = start.elapsed().as_secs_f64();
        // Each lattice bevels the seam's rim, an element wide: the nodes in that groove are
        // too far from the other side to be tied (a third of them on this coarse mesh).
        let (paired, unpaired) = (pairing.sides[0].paired, pairing.sides[0].unpaired);
        assert!(unpaired < paired, "{paired} paired, {unpaired} not");
        let (r, seconds) = run(&mesh, &study);
        // Meshed together, the same blocks share their nodes.
        let together = crate::MeshOptions {
            separate_bodies: false,
            ..options
        };
        let (one, _) = crate::mesh_solids(&surfaces, &together, &cancel, &|_| {}).unwrap();
        let shared = Study {
            contacts: Vec::new(),
            ..study.clone()
        };
        let (s, shared_seconds) = run(&one, &shared);
        let tip = -r.displacement[node_at(&mesh, [200.0, 10.0, 10.0], None)][1];
        let tip_shared = -s.displacement[node_at(&one, [200.0, 10.0, 10.0], None)][1];
        eprintln!(
            "mesher, two bodies at {size}: {} nodes meshed in {mesh_seconds:.2} s, {paired} nodes paired and {unpaired} not in {pair_seconds:.2} s, tip {tip:.6} mm ({} CG iterations, {} contact solves, {seconds:.2} s); meshed together {} nodes, tip {tip_shared:.6} mm ({} CG iterations, {shared_seconds:.2} s)",
            mesh.nodes.len(),
            r.iterations,
            r.contact_solve.iterations,
            one.nodes.len(),
            s.iterations
        );
        assert!(r.contact_solve.converged);
        // The two meshes bevel the seam's rim differently: a few per cent.
        assert!(
            (tip - tip_shared).abs() < 0.04 * tip_shared,
            "{tip} vs {tip_shared}"
        );
    }

    #[test]
    fn bodies_meshed_apart_are_tied_by_detected_contacts() {
        mesher_cantilever(4.0);
    }

    /// The speed of contacts on the mesher's two-block cantilever, 20 × 20 × 200 mm, at
    /// the element sizes `docs/SIMULATION.md` §6 quotes:
    /// `cargo test -p oxyde-sim --lib -- --ignored --nocapture contact_timings`.
    #[test]
    #[ignore = "a timing run, not a check"]
    fn contact_timings() {
        for size in [4.0, 2.5] {
            mesher_cantilever(size);
        }
    }
}
