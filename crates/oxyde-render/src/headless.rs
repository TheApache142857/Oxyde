//! Drawing a part without the application: the display mesh precision the application
//! uses, and a picture of a mesh from a standard view, fitted to the image. For the
//! command line (`oxyde-cli snapshot`) and tests.

use oxyde_kernel::{Mesh, Topology};

use crate::{Camera, Frame, Highlight, Scene, StandardView, Style, render_styled};

/// Mesh precision as a fraction of the part's size: the application's default
/// (Preferences > Graphics > Mesh quality: Normal).
pub const NORMAL_MESH_FRACTION: f64 = 0.001;

/// Mesh precision for a part: `fraction` of its size (the diagonal of the box around its
/// vertices and face centroids), kept between half a micron and 5 mm; 0.05 mm when the
/// size is unknown.
pub fn mesh_deflection(topology: &Topology, fraction: f64) -> f64 {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let points = topology
        .vertices
        .iter()
        .map(|v| v.point)
        .chain(topology.faces.iter().map(|f| f.centroid));
    for p in points {
        for (k, value) in [p.x, p.y, p.z].into_iter().enumerate() {
            lo[k] = lo[k].min(value);
            hi[k] = hi[k].max(value);
        }
    }
    let diagonal = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f64>().sqrt();
    if diagonal.is_finite() && diagonal > 0.0 {
        (diagonal * fraction).clamp(0.0005, 5.0)
    } else {
        0.05
    }
}

/// `mesh` drawn from a standard view, centred and zoomed to fit a `width` x `height`
/// image, in `style`, with nothing highlighted.
pub fn snapshot(
    mesh: &Mesh,
    view: StandardView,
    width: usize,
    height: usize,
    style: &Style,
) -> Frame {
    let scene = Scene::from_mesh(mesh);
    let mut camera = Camera::default();
    camera.set_view(view);
    if let Some((lo, hi)) = scene.bounds() {
        camera.fit(lo, hi, width as f64, height as f64);
    }
    render_styled(&scene, &camera, width, height, &Highlight::default(), style)
}
