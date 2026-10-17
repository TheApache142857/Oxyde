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
//!
//! The application also runs the same algorithm on the GPU (`oxyde-app`'s
//! `gpu_trace.rs` and `pathtrace.wgsl`): [`TraceScene::gpu_scene`] and [`GpuFrame`] lay the
//! scene and the settings out for its shader, and [`develop_sums`] turns what it read
//! back into the same picture, so both paths share everything but the tracing itself.

mod bsdf;
mod bvh;
mod camera;
mod denoise;
mod environment;
mod gpu;
mod render;
mod sampling;
mod scene;
mod vector;

pub use camera::{Projection, RenderCamera};
pub use environment::{Environment, EnvironmentKind};
pub use gpu::{
    GpuFrame, GpuScene, LEAF_BIT, LOOK_BYTES, MAX_GPU_LIGHTS, NODE_BYTES, PARAMS_BYTES, TRI_BYTES,
    decode_normal, encode_normal,
};
pub use render::{
    Background, Floor, MAX_PIXELS, RenderImage, RenderSettings, Renderer, develop_sums,
    fit_pixel_budget, inverse_tonemap, render_image, tonemap,
};
pub use scene::{Look, SceneBuilder, TraceScene};
