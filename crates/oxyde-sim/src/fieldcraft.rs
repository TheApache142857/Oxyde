//! FieldCraft's tetrahedral mesher (`fieldcraft-mesh`) behind the `fieldcraft` feature: a
//! constrained Delaunay tetrahedralization of a solid's surface, refined to a size field,
//! smoothed and improved (FieldCraft's `tet::quality_mesh`). A third mesher next to the
//! lattice mesher ([`crate::mesher`]) and Netgen.

use crate::mesh::TetMesh;
use fieldcraft_mesh::Error;
use fieldcraft_mesh::cdt::{self, CdtOptions};
use fieldcraft_mesh::surface::{BoundarySurface, SurfaceOptions};
use fieldcraft_mesh::tet::{self, Order, QualityOptions};
use oxyde_generative::surfaces::SurfaceMesh;

/// Fills the closed surface `solid` with four-node elements of edges about `element_size`
/// long and appends them to `mesh` as body `body`, each boundary facet labelled with the
/// label of the surface triangles it lies on. FieldCraft welds the surface's coincident
/// vertices first, so faces meshed apart may share their edges' vertices. Call
/// [`TetMesh::make_quadratic`] after the last solid for ten-node elements.
pub fn append_quality_mesh(
    mesh: &mut TetMesh,
    solid: &SurfaceMesh,
    element_size: f64,
    body: u32,
) -> Result<(), Error> {
    let surface = BoundarySurface::new(
        &solid.positions,
        &solid.triangles,
        &solid.labels,
        &SurfaceOptions::default(),
    )?;
    let cdt = cdt::tetrahedralize(&surface, &CdtOptions::default())?;
    let options = QualityOptions {
        size: Some(element_size),
        order: Order::Linear,
        ..QualityOptions::default()
    };
    let quality = tet::quality_mesh(&surface, &cdt, Vec::new(), &options)?;
    let tets = quality
        .elements
        .iter()
        .map(|e| <[u32; 4]>::try_from(e.as_slice()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| Error::Failed("a linear mesh with an element of other than 4 nodes".into()))?;
    let boundary: Vec<([u32; 3], u32)> = quality
        .boundary
        .iter()
        .filter_map(|b| {
            let corners = tets.get(b.tet as usize)?;
            let mut face = corners
                .iter()
                .enumerate()
                .filter(|&(k, _)| k != usize::from(b.opposite))
                .map(|(_, &n)| n);
            Some(([face.next()?, face.next()?, face.next()?], b.face))
        })
        .collect();
    mesh.append_solid(&quality.points, &tets, &boundary, body);
    Ok(())
}
