//! A path tracer: the reference picture of the Render tab (`docs/RENDERING.md`).
//!
//! It draws the same meshes the viewport does, seen through a perspective (or the
//! viewport's orthographic) camera, with physically based surfaces ([`Look`]: colour,
//! metallic, roughness, glass), lit by a built-in environment (a studio, a sky, a uniform
//! light) whose lights cast soft shadows, on a floor that catches them. Light bounces
//! between surfaces, metals reflect, glass refracts.
//!
//! The work: build a [`TraceScene`] once from meshes and looks ([`SceneBuilder`]), then a
//! [`Renderer`] per view, which adds one sample per pixel per [`Renderer::render_pass`] on
//! every core and can be looked at after any pass ([`Renderer::image`]). Pictures are
//! deterministic: the same scene, view and seed give the same image whatever the number
//! of threads, which the tests rely on.
//!
//! Like the rest of this crate it sees meshes only: no kernel, no model. The application
//! turns appearances and finishes into looks.

mod bsdf;
mod bvh;
mod camera;
mod denoise;
mod environment;
mod render;
mod sampling;
mod scene;
mod vector;

pub use camera::{Projection, RenderCamera};
pub use environment::{Environment, EnvironmentKind};
pub use render::{
    Background, Floor, MAX_PIXELS, RenderImage, RenderSettings, Renderer, fit_pixel_budget,
    inverse_tonemap, render_image, tonemap,
};
pub use scene::{Look, SceneBuilder, TraceScene};
