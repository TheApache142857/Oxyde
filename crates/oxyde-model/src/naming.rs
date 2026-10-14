//! Persistent topological naming: the memory of every face, edge and vertex across rebuilds.
//!
//! The kernel numbers faces, edges and vertices arbitrarily, and that numbering changes
//! whenever anything upstream changes. Features therefore never store indices; they store
//! *names* built from where geometry comes from:
//!
//! - **Faces** are named from the kernel's operation history: "the side face that
//!   Boss-Extrude1 swept from sketch segment 2", "the end cap of Cut-Extrude1", "the fillet
//!   face Fillet1 made from edge X". A face that is only trimmed or split by a later
//!   operation keeps its name.
//! - **Edges** are named by the faces they separate, **vertices** by the faces meeting there.
//! - When several entities end up with the same name (a face split in two, two faces
//!   meeting along two edges), they are told apart by a `part` ordinal assigned in a
//!   deterministic geometric order.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt;

use oxyde_geom::Point3;
use oxyde_kernel::Topology;

use crate::feature::FeatureId;

/// Where a face comes from, independent of kernel numbering.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FaceKey {
    /// Side face swept by an extrusion or a revolve from a sketch segment.
    Profile { feature: FeatureId, segment: u32 },
    /// Face of an extrusion or a revolve lying on its sketch plane.
    StartCap { feature: FeatureId },
    /// Face at the far end of an extrusion or a revolve.
    EndCap { feature: FeatureId },
    /// Face generated from an edge (fillet or chamfer face).
    FromEdge { feature: FeatureId, edge: EdgeName },
    /// Face generated from a vertex (corner blend).
    FromVertex {
        feature: FeatureId,
        vertex: VertexName,
    },
    /// Offset copy of a face (inner wall of a shell).
    Offset {
        feature: FeatureId,
        face: Box<FaceName>,
    },
    /// Copy of a seed's face made by a pattern or a mirror. `instance` is the position in
    /// the pattern: `[i, j]` for a linear pattern, `[i, 0]` for a circular one, `[1, 0]`
    /// for a mirror; `face` names the face in the seed's tool body.
    Instance {
        feature: FeatureId,
        instance: [u32; 2],
        face: Box<FaceName>,
    },
    /// Face of geometry imported from a file, by its position in the imported shape (stable,
    /// since the stored file does not change).
    Imported { feature: FeatureId, index: u32 },
    /// Face whose origin the kernel did not report. Not stable: references to it are
    /// flagged.
    Unknown { feature: FeatureId, index: u32 },
}

impl FaceKey {
    /// Appends every feature this name refers to, including through nested names.
    pub fn collect_features(&self, out: &mut Vec<FeatureId>) {
        out.push(self.feature());
        match self {
            FaceKey::FromEdge { edge, .. } => {
                edge.faces.iter().for_each(|f| f.key.collect_features(out))
            }
            FaceKey::FromVertex { vertex, .. } => vertex
                .faces
                .iter()
                .for_each(|f| f.key.collect_features(out)),
            FaceKey::Offset { face, .. } | FaceKey::Instance { face, .. } => {
                face.key.collect_features(out)
            }
            _ => {}
        }
    }

    /// The feature that created the face.
    pub fn feature(&self) -> FeatureId {
        match self {
            FaceKey::Profile { feature, .. }
            | FaceKey::StartCap { feature }
            | FaceKey::EndCap { feature }
            | FaceKey::FromEdge { feature, .. }
            | FaceKey::FromVertex { feature, .. }
            | FaceKey::Offset { feature, .. }
            | FaceKey::Instance { feature, .. }
            | FaceKey::Imported { feature, .. }
            | FaceKey::Unknown { feature, .. } => *feature,
        }
    }
}

/// Persistent name of a face.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceName {
    pub key: FaceKey,
    /// 0 for a whole face; 1, 2, … for the pieces of a face that was split.
    pub part: u32,
}

impl FaceName {
    pub fn new(key: FaceKey) -> Self {
        Self { key, part: 0 }
    }

    /// Side face of `extrude` swept from sketch segment `segment`.
    pub fn profile(extrude: FeatureId, segment: u32) -> Self {
        Self::new(FaceKey::Profile {
            feature: extrude,
            segment,
        })
    }

    /// Face of `extrude` on its sketch plane.
    pub fn start_cap(extrude: FeatureId) -> Self {
        Self::new(FaceKey::StartCap { feature: extrude })
    }

    /// Face at the far end of `extrude`.
    pub fn end_cap(extrude: FeatureId) -> Self {
        Self::new(FaceKey::EndCap { feature: extrude })
    }

    /// Fillet or chamfer face that `blend` made from `edge`.
    pub fn from_edge(blend: FeatureId, edge: EdgeName) -> Self {
        Self::new(FaceKey::FromEdge {
            feature: blend,
            edge,
        })
    }
}

/// Persistent name of an edge: the faces it separates.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeName {
    /// Sorted, without duplicates. Usually two faces; one for a seam edge.
    pub faces: Vec<FaceName>,
    /// 0 when unique; 1, 2, … when several edges separate the same faces.
    pub part: u32,
}

impl EdgeName {
    /// The edge between faces `a` and `b`.
    pub fn between(a: FaceName, b: FaceName) -> Self {
        let mut faces = vec![a, b];
        faces.sort();
        faces.dedup();
        Self { faces, part: 0 }
    }
}

/// Persistent name of a vertex: the faces meeting there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VertexName {
    /// Sorted, without duplicates.
    pub faces: Vec<FaceName>,
    /// 0 when unique; 1, 2, … when several vertices touch the same faces.
    pub part: u32,
}

/// Outcome of looking up a stored name in a rebuilt body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The name exists as is.
    Exact(u32),
    /// The exact name is gone but exactly one entity matches it loosely (ignoring `part`
    /// ordinals). Usable, but the feature is flagged with a warning.
    Fallback(u32),
    /// Several entities match loosely; the reference must be repaired.
    Ambiguous(usize),
    /// Nothing matches; the reference must be repaired.
    Lost,
}

/// Names of every face, edge and vertex of one body, indexed like its [`Topology`].
#[derive(Debug, Clone, Default)]
pub struct BodyNaming {
    faces: Vec<FaceName>,
    edges: Vec<EdgeName>,
    vertices: Vec<VertexName>,
    face_index: HashMap<FaceName, u32>,
    edge_index: HashMap<EdgeName, u32>,
    vertex_index: HashMap<VertexName, u32>,
}

impl BodyNaming {
    /// Names a body produced by `feature`. `face_names` comes from the operation history
    /// (one entry per face of `topology`, `None` where the origin is unknown).
    pub fn build(
        feature: FeatureId,
        mut face_names: Vec<Option<FaceName>>,
        topology: &Topology,
    ) -> Self {
        face_names.resize(topology.faces.len(), None);
        let mut faces: Vec<FaceName> = face_names
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                name.unwrap_or_else(|| {
                    FaceName::new(FaceKey::Unknown {
                        feature,
                        index: i as u32,
                    })
                })
            })
            .collect();
        split_duplicate_faces(&mut faces, topology);

        let face_set = |indices: &mut dyn Iterator<Item = u32>| -> Vec<FaceName> {
            let mut set: Vec<FaceName> = indices
                .filter_map(|f| faces.get(f as usize).cloned())
                .collect();
            set.sort();
            set.dedup();
            set
        };

        let mut edges: Vec<EdgeName> = topology
            .edges
            .iter()
            .map(|e| EdgeName {
                faces: face_set(&mut e.faces.iter().copied()),
                part: 0,
            })
            .collect();
        number_duplicates(
            &mut edges,
            |i| topology.edges[i].midpoint,
            |name, part| name.part = part,
        );

        let mut vertex_faces: Vec<Vec<u32>> = vec![Vec::new(); topology.vertices.len()];
        for edge in &topology.edges {
            for &v in &edge.vertices {
                if let Some(list) = vertex_faces.get_mut(v as usize) {
                    list.extend(edge.faces.iter().copied());
                }
            }
        }
        let mut vertices: Vec<VertexName> = vertex_faces
            .iter()
            .map(|list| VertexName {
                faces: face_set(&mut list.iter().copied()),
                part: 0,
            })
            .collect();
        number_duplicates(
            &mut vertices,
            |i| topology.vertices[i].point,
            |name, part| name.part = part,
        );

        Self {
            face_index: index_of(&faces),
            edge_index: index_of(&edges),
            vertex_index: index_of(&vertices),
            faces,
            edges,
            vertices,
        }
    }

    pub fn faces(&self) -> &[FaceName] {
        &self.faces
    }

    pub fn edges(&self) -> &[EdgeName] {
        &self.edges
    }

    pub fn vertices(&self) -> &[VertexName] {
        &self.vertices
    }

    pub fn face(&self, index: u32) -> Option<&FaceName> {
        self.faces.get(index as usize)
    }

    pub fn edge(&self, index: u32) -> Option<&EdgeName> {
        self.edges.get(index as usize)
    }

    pub fn vertex(&self, index: u32) -> Option<&VertexName> {
        self.vertices.get(index as usize)
    }

    pub fn find_face(&self, name: &FaceName) -> Option<u32> {
        self.face_index.get(name).copied()
    }

    pub fn find_edge(&self, name: &EdgeName) -> Option<u32> {
        self.edge_index.get(name).copied()
    }

    pub fn find_vertex(&self, name: &VertexName) -> Option<u32> {
        self.vertex_index.get(name).copied()
    }

    pub fn resolve_face(&self, name: &FaceName) -> Resolution {
        if let Some(i) = self.find_face(name) {
            return Resolution::Exact(i);
        }
        loose_match(self.faces.iter().map(|f| f.key == name.key))
    }

    pub fn resolve_edge(&self, name: &EdgeName) -> Resolution {
        if let Some(i) = self.find_edge(name) {
            return Resolution::Exact(i);
        }
        let same_faces = loose_match(self.edges.iter().map(|e| e.faces == name.faces));
        if same_faces != Resolution::Lost {
            return same_faces;
        }
        // The faces themselves may have been renumbered after a split.
        let wanted = keys(&name.faces);
        loose_match(self.edges.iter().map(|e| keys(&e.faces) == wanted))
    }

    pub fn resolve_vertex(&self, name: &VertexName) -> Resolution {
        if let Some(i) = self.find_vertex(name) {
            return Resolution::Exact(i);
        }
        let same_faces = loose_match(self.vertices.iter().map(|v| v.faces == name.faces));
        if same_faces != Resolution::Lost {
            return same_faces;
        }
        let wanted = keys(&name.faces);
        loose_match(self.vertices.iter().map(|v| keys(&v.faces) == wanted))
    }
}

fn keys(faces: &[FaceName]) -> Vec<&FaceKey> {
    let mut keys: Vec<&FaceKey> = faces.iter().map(|f| &f.key).collect();
    keys.sort();
    keys.dedup();
    keys
}

fn loose_match(matches: impl Iterator<Item = bool>) -> Resolution {
    let hits: Vec<u32> = matches
        .enumerate()
        .filter(|&(_, hit)| hit)
        .map(|(i, _)| i as u32)
        .collect();
    match hits.len() {
        0 => Resolution::Lost,
        1 => Resolution::Fallback(hits[0]),
        n => Resolution::Ambiguous(n),
    }
}

fn index_of<N: Clone + Eq + std::hash::Hash>(names: &[N]) -> HashMap<N, u32> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i as u32))
        .collect()
}

/// Total order on points, insensitive to noise below the linear tolerance.
fn compare_points(a: Point3, b: Point3) -> Ordering {
    let q = |v: f64| (v / oxyde_geom::LINEAR_TOLERANCE).round() as i64;
    (q(a.x), q(a.y), q(a.z)).cmp(&(q(b.x), q(b.y), q(b.z)))
}

/// Gives distinct `part` ordinals to entities that share a name, in geometric order.
fn number_duplicates<N: Clone + Eq + std::hash::Hash>(
    names: &mut [N],
    position: impl Fn(usize) -> Point3,
    set_part: impl Fn(&mut N, u32),
) {
    let mut groups: HashMap<N, Vec<usize>> = HashMap::new();
    for (i, name) in names.iter().enumerate() {
        groups.entry(name.clone()).or_default().push(i);
    }
    for mut members in groups.into_values() {
        if members.len() < 2 {
            continue;
        }
        members.sort_by(|&a, &b| compare_points(position(a), position(b)).then(a.cmp(&b)));
        for (k, &i) in members.iter().enumerate() {
            set_part(&mut names[i], k as u32 + 1);
        }
    }
}

/// Faces inherit names through modifications, so a face split by a later operation
/// produces several faces with the same name. All faces sharing such a key are renumbered.
fn split_duplicate_faces(faces: &mut [FaceName], topology: &Topology) {
    let mut seen = HashSet::new();
    let mut duplicated_keys = HashSet::new();
    for face in faces.iter() {
        if !seen.insert(face.clone()) {
            duplicated_keys.insert(face.key.clone());
        }
    }
    for key in duplicated_keys {
        let mut members: Vec<usize> = (0..faces.len()).filter(|&i| faces[i].key == key).collect();
        members.sort_by(|&a, &b| {
            compare_points(topology.faces[a].centroid, topology.faces[b].centroid).then(a.cmp(&b))
        });
        for (k, &i) in members.iter().enumerate() {
            faces[i].part = k as u32 + 1;
        }
    }
}

// ---- Display: compact, human-readable names (also used in error messages) -------------------

impl fmt::Display for FaceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FaceKey::Profile { feature, segment } => write!(f, "{feature}.side[{segment}]"),
            FaceKey::StartCap { feature } => write!(f, "{feature}.start"),
            FaceKey::EndCap { feature } => write!(f, "{feature}.end"),
            FaceKey::FromEdge { feature, edge } => write!(f, "{feature}.blend({edge})"),
            FaceKey::FromVertex { feature, vertex } => write!(f, "{feature}.corner({vertex})"),
            FaceKey::Offset { feature, face } => write!(f, "{feature}.offset({face})"),
            FaceKey::Instance {
                feature,
                instance: [i, j],
                face,
            } => write!(f, "{feature}[{i},{j}]({face})"),
            FaceKey::Imported { feature, index } => write!(f, "{feature}.imported[{index}]"),
            FaceKey::Unknown { feature, index } => write!(f, "{feature}.face#{index}"),
        }
    }
}

impl fmt::Display for FaceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.key)?;
        if self.part > 0 {
            write!(f, "@{}", self.part)?;
        }
        Ok(())
    }
}

fn write_face_list(f: &mut fmt::Formatter<'_>, faces: &[FaceName], part: u32) -> fmt::Result {
    for (i, face) in faces.iter().enumerate() {
        if i > 0 {
            write!(f, " | ")?;
        }
        write!(f, "{face}")?;
    }
    if part > 0 {
        write!(f, " @{part}")?;
    }
    Ok(())
}

impl fmt::Display for EdgeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "edge(")?;
        write_face_list(f, &self.faces, self.part)?;
        write!(f, ")")
    }
}

impl fmt::Display for VertexName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "vertex(")?;
        write_face_list(f, &self.faces, self.part)?;
        write!(f, ")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxyde_geom::Vec3;
    use oxyde_kernel::{CurveKind, EdgeInfo, FaceInfo, SurfaceKind, VertexInfo};

    fn face_at(x: f64) -> FaceInfo {
        FaceInfo {
            surface: SurfaceKind::Plane,
            area: 1.0,
            centroid: Point3::new(x, 0.0, 0.0),
            normal: Some(Vec3::Z),
            axis: None,
            edges: Vec::new(),
        }
    }

    fn edge(faces: Vec<u32>, vertices: Vec<u32>, x: f64) -> EdgeInfo {
        EdgeInfo {
            curve: CurveKind::Line,
            length: 1.0,
            midpoint: Point3::new(x, 0.0, 0.0),
            axis: None,
            vertices,
            faces,
        }
    }

    fn vertex(x: f64) -> VertexInfo {
        VertexInfo {
            point: Point3::new(x, 0.0, 0.0),
        }
    }

    const F1: FeatureId = FeatureId::from_raw(1);
    const F2: FeatureId = FeatureId::from_raw(2);

    /// A top face split in two by a slot, plus one side face touching both pieces.
    fn split_body() -> (Vec<Option<FaceName>>, Topology) {
        let topology = Topology {
            faces: vec![face_at(10.0), face_at(-10.0), face_at(0.0)],
            edges: vec![
                edge(vec![0, 2], vec![0, 1], 10.0),
                edge(vec![1, 2], vec![1, 2], -10.0),
            ],
            vertices: vec![vertex(20.0), vertex(0.0), vertex(-20.0)],
        };
        let names = vec![
            Some(FaceName::end_cap(F1)),
            Some(FaceName::end_cap(F1)),
            Some(FaceName::profile(F1, 2)),
        ];
        (names, topology)
    }

    #[test]
    fn split_face_pieces_are_numbered_by_position() {
        let (names, topology) = split_body();
        let naming = BodyNaming::build(F2, names, &topology);
        // The piece with the smaller x comes first.
        assert_eq!(naming.face(1).unwrap().part, 1);
        assert_eq!(naming.face(0).unwrap().part, 2);
        assert_eq!(naming.face(2).unwrap().part, 0);
    }

    #[test]
    fn edges_are_named_by_their_faces() {
        let (names, topology) = split_body();
        let naming = BodyNaming::build(F2, names, &topology);
        let expected = EdgeName {
            faces: {
                let mut faces = vec![
                    FaceName {
                        key: FaceKey::EndCap { feature: F1 },
                        part: 2,
                    },
                    FaceName::profile(F1, 2),
                ];
                faces.sort();
                faces
            },
            part: 0,
        };
        assert_eq!(naming.find_edge(&expected), Some(0));
    }

    #[test]
    fn vertex_shared_by_both_pieces_touches_three_faces() {
        let (names, topology) = split_body();
        let naming = BodyNaming::build(F2, names, &topology);
        assert_eq!(naming.vertex(1).unwrap().faces.len(), 3);
        assert_eq!(naming.vertex(0).unwrap().faces.len(), 2);
    }

    #[test]
    fn unknown_faces_get_placeholder_names() {
        let (mut names, topology) = split_body();
        names[2] = None;
        let naming = BodyNaming::build(F2, names, &topology);
        assert_eq!(
            naming.face(2).unwrap().key,
            FaceKey::Unknown {
                feature: F2,
                index: 2
            }
        );
    }

    #[test]
    fn resolution_falls_back_when_only_the_part_changed() {
        let (names, topology) = split_body();
        let naming = BodyNaming::build(F2, names, &topology);

        // Stored before the split: the edge between the whole top face and the side.
        let before_split = EdgeName::between(FaceName::end_cap(F1), FaceName::profile(F1, 2));
        assert_eq!(naming.resolve_edge(&before_split), Resolution::Ambiguous(2));

        let side = FaceName::profile(F1, 2);
        assert_eq!(naming.resolve_face(&side), Resolution::Exact(2));
        let top = FaceName::end_cap(F1);
        assert_eq!(naming.resolve_face(&top), Resolution::Ambiguous(2));
        assert_eq!(
            naming.resolve_face(&FaceName::end_cap(F2)),
            Resolution::Lost
        );
    }

    #[test]
    fn display_is_readable() {
        let edge = EdgeName::between(FaceName::profile(F1, 2), FaceName::end_cap(F1));
        assert_eq!(edge.to_string(), "edge(F1.side[2] | F1.end)");
        assert_eq!(
            FaceName::from_edge(F2, edge).to_string(),
            "F2.blend(edge(F1.side[2] | F1.end))"
        );
    }
}
