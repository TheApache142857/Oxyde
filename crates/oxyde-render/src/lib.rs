//! Software renderer for the Oxyde viewport.
//!
//! Draws a display [`Mesh`](oxyde_kernel::Mesh) — shaded faces and their edges — into an
//! RGBA image with a depth buffer, and keeps an id buffer alongside so that the face or
//! edge under any pixel is known exactly (picking). Faces can have a [`Material`]: a
//! colour, an opacity (transparent faces are drawn last, back to front) and a colour
//! texture mapped automatically along the model's axes. A [`Style`] can ask for the
//! part's reflection in a floor under it. It needs no GPU, which keeps the first
//! versions of the application simple and fully testable. The application's GPU viewport
//! draws the same [`Scene`] (see [`Scene::gpu_mesh`]) with the same colours and shading, and
//! falls back to this renderer when no GPU is available.
//!
//! The projection is orthographic, as is customary in mechanical CAD, and the world is
//! Y-up like SolidWorks: the Front view looks down −Z with X to the right.
//!
//! Without the application, [`mesh_deflection`] gives the display precision it would use
//! and [`snapshot`] draws a mesh from a standard view, fitted to the image.

mod camera;
mod headless;
mod material;
mod raster;
mod style;
mod vec3;

pub use camera::{Camera, StandardView};
pub use headless::{NORMAL_MESH_FRACTION, mesh_deflection, snapshot};
pub use material::{Material, Texture, TextureRef};
pub use raster::{
    AO_RADIUS, AO_REACH, Floor, Frame, GpuMesh, Highlight, Pick, Scene, picks_in_rect, render,
    render_styled,
};
pub use style::{Rgb, Style};
