//! Software renderer for the Oxyde viewport.
//!
//! Draws a display [`Mesh`](oxyde_kernel::Mesh) — shaded faces and their edges — into an
//! RGBA image with a depth buffer, and keeps an id buffer alongside so that the face or
//! edge under any pixel is known exactly (picking). It needs no GPU, which keeps the first
//! versions of the application simple and fully testable; a GPU backend can replace it
//! behind the same interface later.
//!
//! The projection is orthographic, as is customary in mechanical CAD, and the world is
//! Y-up like SolidWorks: the Front view looks down −Z with X to the right.

mod camera;
mod raster;
mod vec3;

pub use camera::{Camera, StandardView};
pub use raster::{Frame, Highlight, Pick, Scene, render};
