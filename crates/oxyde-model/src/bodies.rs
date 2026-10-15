//! Solid bodies: a part's geometry is a list of separate solids (Fusion 360's and Creo's
//! bodies), each a kernel shape, seen together as one [`Body`] whose topology and names
//! cover all of them.
//!
//! References (faces, edges) are persistent names, unique across the part, so a feature
//! points at geometry without saying which body it is on; [`Body::solid_of_face`] and
//! [`Body::solid_of_edge`] find out. Indices in [`Body::topology`] are global: the faces of
//! the first solid, then those of the second, and so on, as [`Solid::faces`] says.

use std::ops::Range;

use oxyde_kernel::{Kernel, ShapeHandle, Topology};

use crate::feature::{BodyId, FeatureId};
use crate::naming::{BodyNaming, FaceName};

/// One solid body of the part.
#[derive(Debug, Clone)]
pub struct Solid {
    pub id: BodyId,
    pub shape: ShapeHandle,
    /// Its faces, edges and vertices in the part's [`Body::topology`].
    pub faces: Range<u32>,
    pub edges: Range<u32>,
    pub vertices: Range<u32>,
    /// Its own topology (indices local to this solid), as the kernel numbers it.
    pub(crate) topology: Topology,
    /// Whether the feature result holding this body made the shape (and frees it).
    pub(crate) owned: bool,
}

impl Solid {
    /// Index in the solid's own topology of a face of the part.
    pub fn local_face(&self, face: u32) -> Option<u32> {
        self.faces.contains(&face).then(|| face - self.faces.start)
    }

    pub fn local_edge(&self, edge: u32) -> Option<u32> {
        self.edges.contains(&edge).then(|| edge - self.edges.start)
    }
}

/// All the solid bodies of a part after a feature, with the topology and persistent names
/// of all of them together.
#[derive(Debug, Clone)]
pub struct Body {
    pub topology: Topology,
    pub naming: BodyNaming,
    pub solids: Vec<Solid>,
}

impl Body {
    pub fn solid(&self, id: BodyId) -> Option<&Solid> {
        self.solids.iter().find(|s| s.id == id)
    }

    pub fn solid_of_face(&self, face: u32) -> Option<&Solid> {
        self.solids.iter().find(|s| s.faces.contains(&face))
    }

    pub fn solid_of_edge(&self, edge: u32) -> Option<&Solid> {
        self.solids.iter().find(|s| s.edges.contains(&edge))
    }

    /// The kernel shapes of every body, in order.
    pub fn shapes(&self) -> Vec<ShapeHandle> {
        self.solids.iter().map(|s| s.shape).collect()
    }

    /// The persistent name of face `local` of `solid`.
    pub(crate) fn face_name(&self, solid: &Solid, local: u32) -> Option<FaceName> {
        self.naming.face(solid.faces.start + local).cloned()
    }

    pub(crate) fn release<K: Kernel>(self, kernel: &mut K) {
        for solid in self.solids {
            if solid.owned {
                kernel.release(solid.shape);
            }
        }
    }
}

/// A body while a feature changes the set: either carried over unchanged, or made by
/// this feature.
#[derive(Debug)]
pub(crate) struct Piece {
    pub id: BodyId,
    pub shape: ShapeHandle,
    pub topology: Topology,
    /// Name of each face (local order); `None` where the operation did not tell.
    pub names: Vec<Option<FaceName>>,
    pub owned: bool,
}

/// The bodies a feature is building, starting from those before it.
#[derive(Debug)]
pub(crate) struct Working {
    pub pieces: Vec<Piece>,
}

impl Working {
    /// The bodies before the feature, unchanged (and not owned by it).
    pub fn from(previous: Option<&Body>) -> Self {
        let pieces = previous
            .map(|body| {
                body.solids
                    .iter()
                    .map(|s| Piece {
                        id: s.id,
                        shape: s.shape,
                        topology: s.topology.clone(),
                        names: s
                            .faces
                            .clone()
                            .map(|f| body.naming.face(f).cloned())
                            .collect(),
                        owned: false,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { pieces }
    }

    pub fn find(&self, id: BodyId) -> Option<usize> {
        self.pieces.iter().position(|p| p.id == id)
    }

    /// Names of the faces of piece `index`, by local index.
    pub fn names(&self, index: usize) -> &[Option<FaceName>] {
        &self.pieces[index].names
    }

    /// Puts a shape this feature made in place of piece `index` (same body). A shape left
    /// with no faces (cut away entirely) removes the body.
    pub fn replace<K: Kernel>(
        &mut self,
        index: usize,
        shape: ShapeHandle,
        names: Vec<Option<FaceName>>,
        kernel: &mut K,
    ) -> Result<(), String> {
        let topology = match topology_of(shape, kernel) {
            Ok(t) => t,
            Err(e) => {
                kernel.release(shape);
                return Err(e);
            }
        };
        let old = &mut self.pieces[index];
        if old.owned {
            kernel.release(old.shape);
        }
        if topology.faces.is_empty() {
            kernel.release(shape);
            self.pieces.remove(index);
            return Ok(());
        }
        *old = Piece {
            id: old.id,
            shape,
            topology,
            names,
            owned: true,
        };
        Ok(())
    }

    /// Adds a new body made by this feature.
    pub fn push<K: Kernel>(
        &mut self,
        id: BodyId,
        shape: ShapeHandle,
        names: Vec<Option<FaceName>>,
        owned: bool,
        kernel: &mut K,
    ) -> Result<(), String> {
        let topology = match topology_of(shape, kernel) {
            Ok(t) => t,
            Err(e) => {
                if owned {
                    kernel.release(shape);
                }
                return Err(e);
            }
        };
        self.pieces.push(Piece {
            id,
            shape,
            topology,
            names,
            owned,
        });
        Ok(())
    }

    /// Takes a body out of the set (it was combined into another).
    pub fn remove<K: Kernel>(&mut self, index: usize, kernel: &mut K) {
        let piece = self.pieces.remove(index);
        if piece.owned {
            kernel.release(piece.shape);
        }
    }

    /// Frees what this feature made, when it fails.
    pub fn abort<K: Kernel>(self, kernel: &mut K) {
        for piece in self.pieces {
            if piece.owned {
                kernel.release(piece.shape);
            }
        }
    }

    /// The bodies as the part's geometry after `feature`, named together.
    pub fn finish(self, feature: FeatureId) -> Body {
        let mut topology = Topology {
            faces: Vec::new(),
            edges: Vec::new(),
            vertices: Vec::new(),
        };
        let mut names = Vec::new();
        let mut solids = Vec::with_capacity(self.pieces.len());
        for piece in self.pieces {
            let (f0, e0, v0) = (
                topology.faces.len() as u32,
                topology.edges.len() as u32,
                topology.vertices.len() as u32,
            );
            let local = &piece.topology;
            topology.faces.extend(local.faces.iter().map(|f| {
                let mut f = f.clone();
                f.edges.iter_mut().for_each(|e| *e += e0);
                f
            }));
            topology.edges.extend(local.edges.iter().map(|e| {
                let mut e = e.clone();
                e.vertices.iter_mut().for_each(|v| *v += v0);
                e.faces.iter_mut().for_each(|f| *f += f0);
                e
            }));
            topology.vertices.extend(local.vertices.iter().cloned());
            let mut piece_names = piece.names;
            piece_names.resize(local.faces.len(), None);
            names.extend(piece_names);
            solids.push(Solid {
                id: piece.id,
                shape: piece.shape,
                faces: f0..topology.faces.len() as u32,
                edges: e0..topology.edges.len() as u32,
                vertices: v0..topology.vertices.len() as u32,
                topology: piece.topology,
                owned: piece.owned,
            });
        }
        let naming = BodyNaming::build(feature, names, &topology);
        Body {
            topology,
            naming,
            solids,
        }
    }
}

fn topology_of<K: Kernel>(shape: ShapeHandle, kernel: &mut K) -> Result<Topology, String> {
    kernel.topology(shape).map_err(|e| e.to_string())
}
