//! GPU rendering of the viewport with wgpu (eframe's wgpu backend).
//!
//! The part is drawn off-screen, with 4× multisampling and a depth buffer, into a texture
//! that egui shows like any image; the sketch and annotation overlays are painted over it by
//! egui as before. A second pass writes face and edge ids into an integer texture: picking
//! reads back the few pixels under the cursor, so it stays exact. Colours, shading and edge
//! widths match the software renderer (`oxyde-render`), which remains the fallback when no
//! GPU is available.
//!
//! Faces carry a material (colour, opacity, colour texture): opaque faces are drawn first,
//! then the edges, then the see-through faces back to front with blending. Textures are
//! kept in one texture array (every image resampled to the same size) and mapped along the
//! model's axes in the shader (triplanar), as in the software renderer. With a floor
//! reflection, the faces are drawn once more mirrored in the floor, faded with height,
//! before everything else.
//!
//! The projection is computed in the shaders from the camera's frame (target, right, up,
//! eye), relative to the target, so millimetre coordinates keep their precision.

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use oxyde_render::{Camera, Floor, GpuMesh, Highlight, Material, Pick, Style, TextureRef};

/// Anti-aliasing by default: 4× multisampling.
pub const DEFAULT_SAMPLES: u32 = 4;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const ID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;
/// Set on edge ids in the id texture; face ids are index + 1.
const EDGE_BIT: u32 = 0x8000_0000;
/// Every colour texture is resampled to this size, so they fit one texture array.
const TEXTURE_SIZE: u32 = 512;
/// Bytes of the `View` uniform: 16 vec4s.
const VIEW_BYTES: u64 = 16 * 16;

const SHADER: &str = r#"
struct View {
    center: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    eye: vec4<f32>,
    // xyz: towards the key light; w: subsurface scattering strength, 0..1.
    key: vec4<f32>,
    // x, y: millimetres to clip space; z: depth scale; w: edge depth bias (mm).
    scale: vec4<f32>,
    // x, y: viewport size in pixels; z: edge width, w: highlighted edge width (pixels).
    viewport: vec4<f32>,
    // sRGB 0..255: background top and bottom, face, selected and hovered face, edge,
    // selected and hovered edge.
    colors: array<vec4<f32>, 8>,
    // x: floor height (mm), y: reflection strength, z: fade height (mm), w: 1 when this
    // pass draws the reflection (faces mirrored in the floor).
    floor: vec4<f32>,
};

struct FaceMaterial {
    // sRGB 0..255 and opacity.
    color: vec4<f32>,
    // x: texture layer, or -1; y: mm per repeat; z: image height over width.
    texture: vec4<f32>,
};

@group(0) @binding(0) var<uniform> view: View;
// Bit 0: hovered, bit 1: selected.
@group(0) @binding(1) var<storage, read> face_flags: array<u32>;
@group(0) @binding(2) var<storage, read> edge_flags: array<u32>;
@group(0) @binding(3) var<storage, read> materials: array<FaceMaterial>;
@group(0) @binding(4) var textures: texture_2d_array<f32>;
@group(0) @binding(5) var texture_sampler: sampler;

fn project(p: vec3<f32>, bias: f32) -> vec4<f32> {
    let v = p - view.center.xyz;
    let x = dot(v, view.right.xyz) * view.scale.x;
    let y = dot(v, view.up.xyz) * view.scale.y;
    // Distance along the view, smaller is closer, mapped to 0..1.
    let d = -dot(v, view.eye.xyz) - bias;
    let z = clamp(d * view.scale.z + 0.5, 0.0, 1.0);
    return vec4<f32>(x, y, z, 1.0);
}

fn linear(c: vec3<f32>) -> vec3<f32> {
    // The colours below are sRGB 0..255, as in the software renderer.
    return pow(c / 255.0, vec3<f32>(2.2));
}

// ---- Background -------------------------------------------------------------------------

struct BackgroundOut {
    @builtin(position) position: vec4<f32>,
    @location(0) t: f32,
};

@vertex
fn background_vs(@builtin(vertex_index) i: u32) -> BackgroundOut {
    var corners = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let c = corners[i];
    var out: BackgroundOut;
    out.position = vec4<f32>(c, 1.0, 1.0);
    out.t = (1.0 - c.y) * 0.5;
    return out;
}

@fragment
fn background_fs(in: BackgroundOut) -> @location(0) vec4<f32> {
    let top = view.colors[0].xyz;
    let bottom = view.colors[1].xyz;
    return vec4<f32>(linear(mix(top, bottom, clamp(in.t, 0.0, 1.0))), 1.0);
}

// ---- Faces --------------------------------------------------------------------------------

struct FaceOut {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) @interpolate(flat) face: u32,
    @location(2) world: vec3<f32>,
};

@vertex
fn face_vs(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) face: u32,
) -> FaceOut {
    var out: FaceOut;
    var p = position;
    var n = normal;
    if (view.floor.w > 0.5) {
        // The reflection: mirrored in the floor.
        p.y = 2.0 * view.floor.x - p.y;
        n.y = -n.y;
    }
    out.position = project(p, 0.0);
    out.normal = n;
    out.face = face;
    out.world = p;
    return out;
}

// The texture projected along each axis, blended by how much the surface faces it.
fn triplanar(layer: i32, scale: f32, aspect: f32, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let s = max(scale, 1e-6);
    let sv = s * max(aspect, 1e-6);
    var w = pow(abs(n), vec3<f32>(4.0));
    let sum = w.x + w.y + w.z;
    if (sum < 1e-12) {
        w = vec3<f32>(0.0, 1.0, 0.0);
    } else {
        w = w / sum;
    }
    let x = textureSample(textures, texture_sampler, vec2<f32>(p.z / s, -p.y / sv), layer).xyz;
    let y = textureSample(textures, texture_sampler, vec2<f32>(p.x / s, p.z / sv), layer).xyz;
    let z = textureSample(textures, texture_sampler, vec2<f32>(p.x / s, -p.y / sv), layer).xyz;
    return (x * w.x + y * w.y + z * w.z) * 255.0;
}

@fragment
fn face_fs(in: FaceOut) -> @location(0) vec4<f32> {
    var n = normalize(in.normal);
    // Seen from outside: face the viewer (also tolerates inconsistent winding).
    if (dot(n, view.eye.xyz) < 0.0) {
        n = -n;
    }
    // As `shade` in the software renderer: ambient, headlight, key light, and with
    // subsurface scattering a wrapped key light and a glow near the silhouette.
    let facing = dot(n, view.eye.xyz);
    let towards_key = dot(n, view.key.xyz);
    var key = max(towards_key, 0.0);
    var rim = 0.0;
    let s = view.key.w;
    if (s > 0.0) {
        let wrap = 0.6 * s;
        let soft = max((towards_key + wrap) / (1.0 + wrap), 0.0);
        key = key + (soft - key) * s;
        let edge = max(1.0 - facing, 0.0);
        rim = edge * edge * edge * 0.45 * s;
    }
    let light = 0.38 + 0.37 * facing + 0.40 * key + rim;
    let flags = face_flags[in.face];
    let material = materials[in.face];
    var base = material.color.xyz;
    var alpha = material.color.w;
    if ((flags & 1u) != 0u) {
        base = view.colors[4].xyz;
    } else if ((flags & 2u) != 0u) {
        base = view.colors[3].xyz;
    } else if (material.texture.x >= 0.0) {
        base = triplanar(i32(material.texture.x), material.texture.y, material.texture.z, in.world, n);
    }
    if (view.floor.w > 0.5) {
        let height = max(view.floor.x - in.world.y, 0.0);
        alpha = alpha * view.floor.y * clamp(1.0 - height / max(view.floor.z, 1e-6), 0.0, 1.0);
    }
    return vec4<f32>(linear(min(base * light, vec3<f32>(255.0))), alpha);
}

@fragment
fn face_id_fs(in: FaceOut) -> @location(0) u32 {
    return in.face + 1u;
}

// ---- Edges, as screen-space quads -------------------------------------------------------

struct EdgeOut {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) edge: u32,
};

@vertex
fn edge_vs(
    @builtin(vertex_index) i: u32,
    @location(0) start: vec3<f32>,
    @location(1) end: vec3<f32>,
    @location(2) edge: u32,
) -> EdgeOut {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
        vec2(0.0, -1.0), vec2(1.0, 1.0), vec2(0.0, 1.0),
    );
    let corner = corners[i];
    let a = project(start, view.scale.w);
    let b = project(end, view.scale.w);
    let half = view.viewport.xy * 0.5;
    var along = (b.xy - a.xy) * half;
    if (length(along) < 1e-6) {
        along = vec2<f32>(1.0, 0.0);
    }
    let side = normalize(vec2<f32>(-along.y, along.x));
    var width = view.viewport.z;
    if (edge_flags[edge] != 0u) {
        width = view.viewport.w;
    }
    let p = mix(a, b, corner.x);
    var out: EdgeOut;
    out.position = vec4<f32>(p.xy + side * corner.y * width * 0.5 / half, p.z, 1.0);
    out.edge = edge;
    return out;
}

@fragment
fn edge_fs(in: EdgeOut) -> @location(0) vec4<f32> {
    let flags = edge_flags[in.edge];
    var color = view.colors[5].xyz;
    if ((flags & 1u) != 0u) {
        color = view.colors[7].xyz;
    } else if ((flags & 2u) != 0u) {
        color = view.colors[6].xyz;
    }
    return vec4<f32>(linear(color), 1.0);
}

@fragment
fn edge_id_fs(in: EdgeOut) -> @location(0) u32 {
    return (in.edge + 1u) | 0x80000000u;
}
"#;

fn bytes_f32(values: impl IntoIterator<Item = f32>) -> Vec<u8> {
    values.into_iter().flat_map(f32::to_le_bytes).collect()
}

fn bytes_u32(values: impl IntoIterator<Item = u32>) -> Vec<u8> {
    values.into_iter().flat_map(u32::to_le_bytes).collect()
}

/// Off-screen targets for one viewport size.
struct Targets {
    width: u32,
    height: u32,
    /// Multisampled image, resolved into `resolved`; none without multisampling.
    color: Option<wgpu::TextureView>,
    #[allow(dead_code)]
    resolved: wgpu::Texture,
    resolved_view: wgpu::TextureView,
    depth: wgpu::TextureView,
    ids: wgpu::Texture,
    ids_view: wgpu::TextureView,
    ids_depth: wgpu::TextureView,
}

impl Targets {
    fn new(device: &wgpu::Device, width: u32, height: u32, samples: u32) -> Self {
        let make = |label, format, samples, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let attachment = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        // Without multisampling the image is drawn straight into the resolved texture.
        let color =
            (samples > 1).then(|| view(&make("oxyde color", COLOR_FORMAT, samples, attachment)));
        let resolved = make(
            "oxyde image",
            COLOR_FORMAT,
            1,
            attachment | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        let depth = make("oxyde depth", DEPTH_FORMAT, samples, attachment);
        let ids = make(
            "oxyde ids",
            ID_FORMAT,
            1,
            attachment | wgpu::TextureUsages::COPY_SRC,
        );
        let ids_depth = make("oxyde id depth", DEPTH_FORMAT, 1, attachment);
        Self {
            width,
            height,
            color,
            resolved_view: view(&resolved),
            resolved,
            depth: view(&depth),
            ids_view: view(&ids),
            ids,
            ids_depth: view(&ids_depth),
        }
    }
}

/// Mesh buffers of the current scene.
struct MeshBuffers {
    vertices: wgpu::Buffer,
    /// Every triangle: for the id pass and the reflection.
    indices: wgpu::Buffer,
    index_count: u32,
    /// The triangles of the opaque faces only.
    opaque: wgpu::Buffer,
    opaque_count: u32,
    /// See-through faces, with where their triangles are in the full index list.
    transparent: Vec<(usize, (u32, u32))>,
    indices_cpu: Vec<u32>,
    centroids: Vec<[f32; 3]>,
    segments: wgpu::Buffer,
    segment_count: u32,
    face_count: usize,
    edge_count: usize,
    materials: wgpu::Buffer,
    textures: wgpu::TextureView,
}

/// The face materials as the shader reads them: 32 bytes each.
fn material_bytes(materials: &[Material], textures: &[TextureRef], count: usize, style: &Style) -> Vec<u8> {
    let mut out = Vec::with_capacity(count.max(1) * 32);
    let default = Material::default();
    for i in 0..count.max(1) {
        let m = materials.get(i).unwrap_or(&default);
        let c = m.color.unwrap_or(style.face);
        let (layer, aspect) = match m.texture.and_then(|t| textures.get(t)) {
            Some(t) => (m.texture.unwrap() as f32, t.aspect()),
            None => (-1.0, 1.0),
        };
        out.extend(bytes_f32([
            f32::from(c[0]),
            f32::from(c[1]),
            f32::from(c[2]),
            m.opacity.clamp(0.0, 1.0),
            layer,
            m.texture_scale,
            aspect,
            0.0,
        ]));
    }
    out
}

/// Every texture resampled to `TEXTURE_SIZE` square, as the layers of one array; one
/// white layer when there are none.
fn texture_array(device: &wgpu::Device, queue: &wgpu::Queue, textures: &[TextureRef]) -> wgpu::TextureView {
    let layers = textures.len().max(1) as u32;
    let size = if textures.is_empty() { 1 } else { TEXTURE_SIZE };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("oxyde textures"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Not sRGB: samples are the image's own 0..255 values, like the style's colours.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for layer in 0..layers {
        pixels.clear();
        match textures.get(layer as usize) {
            Some(t) => {
                for y in 0..size {
                    for x in 0..size {
                        let c = t.sample((x as f32 + 0.5) / size as f32, (y as f32 + 0.5) / size as f32);
                        pixels.extend([c[0] as u8, c[1] as u8, c[2] as u8, 255]);
                    }
                }
            }
            None => pixels.extend([255, 255, 255, 255]),
        }
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(size * 4),
                rows_per_image: Some(size),
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
    }
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}

pub struct GpuViewport {
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
    renderer: std::sync::Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    /// The same view, flagged to draw the reflection.
    mirror_uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    background: wgpu::RenderPipeline,
    faces: wgpu::RenderPipeline,
    /// See-through faces: blended, leaving the depth alone.
    faces_blend: wgpu::RenderPipeline,
    /// The reflection: its depth first, then its colour blended once per pixel.
    mirror_depth: wgpu::RenderPipeline,
    mirror: wgpu::RenderPipeline,
    edges: wgpu::RenderPipeline,
    face_ids: wgpu::RenderPipeline,
    edge_ids: wgpu::RenderPipeline,
    mesh: Option<MeshBuffers>,
    /// The style the material buffer was built with (it holds the default face colour).
    material_style: Option<Style>,
    /// The materials of the current mesh, to rebuild the buffer when the style changes.
    materials_cpu: (Vec<Material>, Vec<TextureRef>),
    /// Bounds of the scene, for the depth range.
    bounds: Option<([f64; 3], [f64; 3])>,
    targets: Option<Targets>,
    texture: Option<egui::TextureId>,
    samples: u32,
}

impl GpuViewport {
    /// `samples`: 1 (no anti-aliasing) or 4.
    pub fn new(state: &egui_wgpu::RenderState, samples: u32) -> Self {
        let samples = if samples > 1 { 4 } else { 1 };
        let device = state.device.clone();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("oxyde viewport"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("oxyde view"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(1),
                storage(2),
                storage(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("oxyde textures"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("oxyde viewport"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });

        let face_attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 12,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 24,
                shader_location: 2,
            },
        ];
        let face_buffers = [wgpu::VertexBufferLayout {
            array_stride: 28,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &face_attributes,
        }];
        let edge_attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 12,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 24,
                shader_location: 2,
            },
        ];
        let edge_buffers = [wgpu::VertexBufferLayout {
            array_stride: 28,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &edge_attributes,
        }];

        let depth = |write: bool, compare| {
            Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: write,
                depth_compare: compare,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            })
        };
        let pipeline = |label: &str,
                        vs: &str,
                        fs: &str,
                        buffers: &[wgpu::VertexBufferLayout],
                        target: wgpu::ColorTargetState,
                        samples: u32,
                        depth_stencil: Option<wgpu::DepthStencilState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: vs,
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: fs,
                    targets: &[Some(target)],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil,
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                multiview: None,
                cache: None,
            })
        };
        let less = wgpu::CompareFunction::Less;
        let less_equal = wgpu::CompareFunction::LessEqual;
        let opaque = |format| wgpu::ColorTargetState {
            format,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        };
        let blended = wgpu::ColorTargetState {
            format: COLOR_FORMAT,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        };
        let depth_only = wgpu::ColorTargetState {
            format: COLOR_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::empty(),
        };
        let background = pipeline(
            "background",
            "background_vs",
            "background_fs",
            &[],
            opaque(COLOR_FORMAT),
            samples,
            depth(false, wgpu::CompareFunction::Always),
        );
        let faces = pipeline(
            "faces",
            "face_vs",
            "face_fs",
            &face_buffers,
            opaque(COLOR_FORMAT),
            samples,
            depth(true, less),
        );
        let faces_blend = pipeline(
            "transparent faces",
            "face_vs",
            "face_fs",
            &face_buffers,
            blended.clone(),
            samples,
            depth(false, less_equal),
        );
        let mirror_depth = pipeline(
            "reflection depth",
            "face_vs",
            "face_fs",
            &face_buffers,
            depth_only,
            samples,
            depth(true, less),
        );
        let mirror = pipeline(
            "reflection",
            "face_vs",
            "face_fs",
            &face_buffers,
            blended,
            samples,
            depth(false, less_equal),
        );
        let edges = pipeline(
            "edges",
            "edge_vs",
            "edge_fs",
            &edge_buffers,
            opaque(COLOR_FORMAT),
            samples,
            depth(true, less_equal),
        );
        let face_ids = pipeline(
            "face ids",
            "face_vs",
            "face_id_fs",
            &face_buffers,
            opaque(ID_FORMAT),
            1,
            depth(true, less),
        );
        let edge_ids = pipeline(
            "edge ids",
            "edge_vs",
            "edge_id_fs",
            &edge_buffers,
            opaque(ID_FORMAT),
            1,
            depth(true, less_equal),
        );
        let uniform = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: VIEW_BYTES,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let uniforms = uniform("oxyde view");
        let mirror_uniforms = uniform("oxyde mirrored view");
        Self {
            device,
            queue: state.queue.clone(),
            renderer: state.renderer.clone(),
            layout,
            uniforms,
            mirror_uniforms,
            sampler,
            background,
            faces,
            faces_blend,
            mirror_depth,
            mirror,
            edges,
            face_ids,
            edge_ids,
            mesh: None,
            material_style: None,
            materials_cpu: (Vec::new(), Vec::new()),
            bounds: None,
            targets: None,
            texture: None,
            samples,
        }
    }

    /// Uploads the scene's mesh.
    pub fn set_mesh(&mut self, mesh: &GpuMesh, bounds: Option<([f64; 3], [f64; 3])>) {
        use wgpu::util::DeviceExt;
        self.bounds = bounds;
        if mesh.indices.is_empty() && mesh.segments.is_empty() {
            self.mesh = None;
            return;
        }
        let mut vertices = Vec::with_capacity(mesh.positions.len() * 28);
        for ((p, n), face) in mesh.positions.iter().zip(&mesh.normals).zip(&mesh.faces) {
            vertices.extend(bytes_f32(p.iter().chain(n.iter()).copied()));
            vertices.extend(face.to_le_bytes());
        }
        let mut segments = Vec::with_capacity(mesh.segments.len() * 28);
        for (a, b, edge) in &mesh.segments {
            segments.extend(bytes_f32(a.iter().chain(b.iter()).copied()));
            segments.extend(edge.to_le_bytes());
        }
        // Empty buffers are not allowed: pad with a few bytes.
        let pad = |mut v: Vec<u8>| {
            if v.is_empty() {
                v.resize(28, 0);
            }
            v
        };
        let buffer = |label, contents: &[u8], usage| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents,
                    usage,
                })
        };
        let face_count = mesh
            .faces
            .iter()
            .max()
            .map_or(0, |m| *m as usize + 1)
            .max(mesh.triangle_ranges.len());
        let edge_count = mesh
            .segments
            .iter()
            .map(|s| s.2)
            .max()
            .map_or(0, |m| m as usize + 1);
        // Opaque faces are drawn from their own index list; see-through ones are sorted
        // at every render.
        let mut opaque = Vec::with_capacity(mesh.indices.len());
        let mut transparent = Vec::new();
        for (face, &range) in mesh.triangle_ranges.iter().enumerate() {
            let (a, b) = (range.0 as usize, range.1 as usize);
            if mesh.materials.get(face).is_some_and(Material::is_transparent) {
                transparent.push((face, range));
            } else {
                opaque.extend_from_slice(&mesh.indices[a..b]);
            }
        }
        let style = self.material_style.unwrap_or_default();
        self.mesh = Some(MeshBuffers {
            vertices: buffer("oxyde vertices", &pad(vertices), wgpu::BufferUsages::VERTEX),
            indices: buffer(
                "oxyde indices",
                &pad(bytes_u32(mesh.indices.iter().copied())),
                wgpu::BufferUsages::INDEX,
            ),
            index_count: mesh.indices.len() as u32,
            opaque_count: opaque.len() as u32,
            opaque: buffer(
                "oxyde opaque indices",
                &pad(bytes_u32(opaque)),
                wgpu::BufferUsages::INDEX,
            ),
            transparent,
            indices_cpu: mesh.indices.clone(),
            centroids: mesh.centroids.clone(),
            segments: buffer("oxyde segments", &pad(segments), wgpu::BufferUsages::VERTEX),
            segment_count: mesh.segments.len() as u32,
            face_count,
            edge_count,
            materials: buffer(
                "oxyde materials",
                &material_bytes(&mesh.materials, &mesh.textures, face_count, &style),
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            textures: texture_array(&self.device, &self.queue, &mesh.textures),
        });
        self.material_style = Some(style);
        self.materials_cpu = (mesh.materials.clone(), mesh.textures.clone());
    }

    /// The see-through faces' triangles, farthest first for this camera.
    fn sorted_transparent(mesh: &MeshBuffers, camera: &Camera) -> Vec<u32> {
        let eye = camera.eye_direction();
        let depth = |c: [f32; 3]| {
            -((c[0] as f64 - camera.target[0]) * eye[0]
                + (c[1] as f64 - camera.target[1]) * eye[1]
                + (c[2] as f64 - camera.target[2]) * eye[2])
        };
        let mut faces: Vec<(f64, (u32, u32))> = mesh
            .transparent
            .iter()
            .map(|(face, range)| {
                (
                    mesh.centroids.get(*face).map_or(0.0, |c| depth(*c)),
                    *range,
                )
            })
            .collect();
        faces.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut out = Vec::new();
        for (_, (a, b)) in faces {
            out.extend_from_slice(&mesh.indices_cpu[a as usize..b as usize]);
        }
        out
    }

    /// Renders the scene for `camera` at `width` × `height` pixels and returns the texture
    /// egui shows.
    pub fn render(
        &mut self,
        camera: &Camera,
        width: u32,
        height: u32,
        highlight: &Highlight,
        style: &Style,
    ) -> egui::TextureId {
        let (width, height) = (width.max(1), height.max(1));
        let resized = self
            .targets
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height);
        if resized {
            let targets = Targets::new(&self.device, width, height, self.samples);
            let mut renderer = self.renderer.write();
            match self.texture {
                Some(id) => renderer.update_egui_texture_from_wgpu_texture(
                    &self.device,
                    &targets.resolved_view,
                    wgpu::FilterMode::Nearest,
                    id,
                ),
                None => {
                    self.texture = Some(renderer.register_native_texture(
                        &self.device,
                        &targets.resolved_view,
                        wgpu::FilterMode::Nearest,
                    ));
                }
            }
            self.targets = Some(targets);
        }
        let Some(targets) = &self.targets else {
            unreachable!("created above");
        };

        // The view: millimetres to clip space, and a depth range covering the scene.
        let aspect = width as f64 / height as f64;
        let (right, up, eye, key) = (
            camera.right(),
            camera.up(),
            camera.eye_direction(),
            camera.key_light(),
        );
        let reach = match self.bounds {
            Some((lo, hi)) => {
                let corner_distance = |c: [f64; 3]| {
                    ((c[0] - camera.target[0]).powi(2)
                        + (c[1] - camera.target[1]).powi(2)
                        + (c[2] - camera.target[2]).powi(2))
                    .sqrt()
                };
                let mut r = 0.0f64;
                for x in [lo[0], hi[0]] {
                    for y in [lo[1], hi[1]] {
                        for z in [lo[2], hi[2]] {
                            r = r.max(corner_distance([x, y, z]));
                        }
                    }
                }
                r * 1.1 + 1.0
            }
            None => 1000.0,
        };
        // The default face colour lives in the material buffer: refresh it with the style.
        if self.material_style.as_ref() != Some(style) {
            if let Some(mesh) = &self.mesh {
                let (materials, textures) = &self.materials_cpu;
                self.queue.write_buffer(
                    &mesh.materials,
                    0,
                    &material_bytes(materials, textures, mesh.face_count, style),
                );
            }
            self.material_style = Some(*style);
        }
        let floor = self
            .bounds
            .map(|(lo, hi)| Floor::under(lo, hi))
            .filter(|f| style.floor_reflection > 0.0 && f.visible(camera));

        let v4 = |v: [f64; 3]| [v[0] as f32, v[1] as f32, v[2] as f32, 0.0];
        let mut view: Vec<f32> = Vec::with_capacity(64);
        view.extend(v4(camera.target));
        view.extend(v4(right));
        view.extend(v4(up));
        view.extend(v4(eye));
        view.extend([
            key[0] as f32,
            key[1] as f32,
            key[2] as f32,
            style.subsurface.clamp(0.0, 1.0),
        ]);
        view.extend([
            (2.0 / (camera.height * aspect)) as f32,
            (2.0 / camera.height) as f32,
            (1.0 / (2.0 * reach)) as f32,
            (camera.height * 0.004) as f32,
        ]);
        view.extend([
            width as f32,
            height as f32,
            style.edge_width,
            style.highlight_width,
        ]);
        for c in [
            style.background_top,
            style.background_bottom,
            style.face,
            style.face_selected,
            style.face_hovered,
            style.edge,
            style.edge_selected,
            style.edge_hovered,
        ] {
            view.extend([f32::from(c[0]), f32::from(c[1]), f32::from(c[2]), 0.0]);
        }
        let (floor_y, fade) = floor.map_or((0.0, 1.0), |f| (f.y as f32, f.fade as f32));
        view.extend([floor_y, style.floor_reflection.clamp(0.0, 1.0), fade, 0.0]);
        self.queue.write_buffer(&self.uniforms, 0, &bytes_f32(view.clone()));
        let n = view.len();
        view[n - 1] = 1.0;
        self.queue.write_buffer(&self.mirror_uniforms, 0, &bytes_f32(view));

        let (faces, edges) = self
            .mesh
            .as_ref()
            .map_or((0, 0), |m| (m.face_count, m.edge_count));
        let mut face_flags = vec![0u32; faces.max(1)];
        let mut edge_flags = vec![0u32; edges.max(1)];
        let mut flag = |pick: Pick, bit: u32| match pick {
            Pick::Face(i) if (i as usize) < faces => face_flags[i as usize] |= bit,
            Pick::Edge(i) if (i as usize) < edges => edge_flags[i as usize] |= bit,
            _ => {}
        };
        for pick in &highlight.selected {
            flag(*pick, 2);
        }
        for pick in highlight.hovered.iter().chain(&highlight.preselected) {
            flag(*pick, 1);
        }
        use wgpu::util::DeviceExt;
        let storage = |label, data: Vec<u32>| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: &bytes_u32(data),
                    usage: wgpu::BufferUsages::STORAGE,
                })
        };
        let face_buffer = storage("oxyde face flags", face_flags);
        let edge_buffer = storage("oxyde edge flags", edge_flags);
        // Without a mesh, placeholder material and texture bindings.
        let empty_materials;
        let empty_textures;
        let (materials, textures) = match &self.mesh {
            Some(m) => (&m.materials, &m.textures),
            None => {
                empty_materials = storage("oxyde materials", vec![0; 8]);
                empty_textures = texture_array(&self.device, &self.queue, &[]);
                (&empty_materials, &empty_textures)
            }
        };
        let bind = |label, uniforms: &wgpu::Buffer| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: face_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: edge_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: materials.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(textures),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            })
        };
        let bind_group = bind("oxyde view", &self.uniforms);
        let mirror_group = bind("oxyde mirrored view", &self.mirror_uniforms);
        let transparent = self
            .mesh
            .as_ref()
            .filter(|m| !m.transparent.is_empty())
            .map(|m| Self::sorted_transparent(m, camera))
            .filter(|t| !t.is_empty())
            .map(|t| {
                let count = t.len() as u32;
                (
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("oxyde transparent indices"),
                            contents: &bytes_u32(t),
                            usage: wgpu::BufferUsages::INDEX,
                        }),
                    count,
                )
            });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("oxyde viewport"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("oxyde image"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: targets.color.as_ref().unwrap_or(&targets.resolved_view),
                    resolve_target: targets.color.as_ref().map(|_| &targets.resolved_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_pipeline(&self.background);
            pass.draw(0..3, 0..1);
            if let Some(mesh) = &self.mesh {
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                if floor.is_some() {
                    // The reflection, under everything: its depth first so that each pixel
                    // is then blended exactly once.
                    pass.set_bind_group(0, &mirror_group, &[]);
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.set_pipeline(&self.mirror_depth);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    pass.set_pipeline(&self.mirror);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    pass.set_bind_group(0, &bind_group, &[]);
                }
                pass.set_pipeline(&self.faces);
                pass.set_index_buffer(mesh.opaque.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.opaque_count, 0, 0..1);
                if style.show_edges {
                    pass.set_pipeline(&self.edges);
                    pass.set_vertex_buffer(0, mesh.segments.slice(..));
                    pass.draw(0..6, 0..mesh.segment_count);
                }
                if let Some((buffer, count)) = &transparent {
                    pass.set_pipeline(&self.faces_blend);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(buffer.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..*count, 0, 0..1);
                }
            }
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("oxyde ids"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.ids_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.ids_depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &bind_group, &[]);
            if let Some(mesh) = &self.mesh {
                pass.set_pipeline(&self.face_ids);
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                if style.show_edges {
                    pass.set_pipeline(&self.edge_ids);
                    pass.set_vertex_buffer(0, mesh.segments.slice(..));
                    pass.draw(0..6, 0..mesh.segment_count);
                }
            }
        }
        self.queue.submit([encoder.finish()]);
        self.texture.unwrap_or_default()
    }

    /// The faces and edges inside a rectangle of the last image (see
    /// [`oxyde_render::picks_in_rect`]).
    pub fn picks_in(&self, min: [f64; 2], max: [f64; 2], crossing: bool) -> Vec<Pick> {
        let Some((w, _, ids)) = self.read_ids() else {
            return Vec::new();
        };
        let faces: Vec<u32> = ids
            .iter()
            .map(|&id| if id & EDGE_BIT == 0 { id } else { 0 })
            .collect();
        let edges: Vec<u32> = ids
            .iter()
            .map(|&id| if id & EDGE_BIT != 0 { id & !EDGE_BIT } else { 0 })
            .collect();
        oxyde_render::picks_in_rect(&faces, &edges, w as usize, min, max, crossing)
    }

    /// The whole id image: face index + 1, or edge index + 1 with `EDGE_BIT`.
    fn read_ids(&self) -> Option<(u32, u32, Vec<u32>)> {
        let targets = self.targets.as_ref()?;
        let (w, h, rgba) = self.read_texture(&targets.ids, targets.width, targets.height)?;
        let ids = rgba
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        Some((w, h, ids))
    }

    /// The last image, as RGBA rows from the top (for tests and screenshots).
    #[allow(dead_code)]
    pub fn read_image(&self) -> Option<(u32, u32, Vec<u8>)> {
        let targets = self.targets.as_ref()?;
        self.read_texture(&targets.resolved, targets.width, targets.height)
    }

    /// A 4-byte-per-pixel texture, rows from the top.
    fn read_texture(&self, texture: &wgpu::Texture, w: u32, h: u32) -> Option<(u32, u32, Vec<u8>)> {
        // Rows of a texture copy are 256-byte aligned.
        let row = (w * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("oxyde image read-back"),
            size: u64::from(row * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("oxyde image read-back"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let start = (y * row) as usize;
            rgba.extend_from_slice(&data[start..start + (w * 4) as usize]);
        }
        drop(data);
        buffer.unmap();
        Some((w, h, rgba))
    }

    /// What is at pixel (`x`, `y`) of the last image. Edges within `radius` pixels win over
    /// faces, as in the software renderer.
    pub fn pick(&self, x: f64, y: f64, radius: f64) -> Option<Pick> {
        let targets = self.targets.as_ref()?;
        let r = radius.max(0.0).ceil().min(31.0) as i64;
        let (cx, cy) = (x.floor() as i64, y.floor() as i64);
        let (w, h) = (targets.width as i64, targets.height as i64);
        if cx < 0 || cy < 0 || cx >= w || cy >= h {
            return None;
        }
        let (x0, y0) = ((cx - r).max(0), (cy - r).max(0));
        let (x1, y1) = ((cx + r).min(w - 1), (cy + r).min(h - 1));
        let (rw, rh) = ((x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
        // Rows of a texture copy are 256-byte aligned; 64 pixels of 4 bytes fit in one.
        let row = 256u32;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("oxyde pick"),
            size: (row * rh) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("oxyde pick"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &targets.ids,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: x0 as u32,
                    y: y0 as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(rh),
                },
            },
            wgpu::Extent3d {
                width: rw,
                height: rh,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let id_at = |px: i64, py: i64| {
            let offset = ((py - y0) as usize) * row as usize + ((px - x0) as usize) * 4;
            u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap_or([0; 4]))
        };
        let mut best: Option<(i64, u32)> = None;
        for py in y0..=y1 {
            for px in x0..=x1 {
                let d2 = (px - cx).pow(2) + (py - cy).pow(2);
                if d2 > r * r {
                    continue;
                }
                let id = id_at(px, py);
                if id & EDGE_BIT != 0 && best.is_none_or(|(bd, _)| d2 < bd) {
                    best = Some((d2, id & !EDGE_BIT));
                }
            }
        }
        let center = id_at(cx, cy);
        drop(data);
        buffer.unmap();
        if let Some((_, id)) = best {
            return Some(Pick::Edge(id - 1));
        }
        (center != 0 && center & EDGE_BIT == 0).then(|| Pick::Face(center - 1))
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui_wgpu::{self, wgpu};
    use oxyde_geom::{Point3, Vec3};
    use oxyde_kernel::Kernel;
    use oxyde_render::{
        Camera, Highlight, Material, Pick, Scene, StandardView, Style, Texture, render,
        render_styled,
    };

    use super::GpuViewport;

    /// Writes an image as PNG into `$OXYDE_SNAPSHOT_DIR`, when set, to look at.
    fn snapshot(name: &str, width: u32, height: u32, rgba: &[u8]) {
        let Some(dir) = std::env::var_os("OXYDE_SNAPSHOT_DIR") else {
            return;
        };
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        image::save_buffer(&path, rgba, width, height, image::ColorType::Rgba8).unwrap();
    }

    /// A scene with everything the renderers must agree on: two boxes, face colours, a
    /// see-through face, a checker texture, and the floor reflection on.
    fn material_scene(kernel: &mut oxyde_occt::OcctKernel) -> Scene {
        let a = kernel
            .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(40.0, 20.0, 10.0))
            .unwrap();
        let b = kernel
            .make_box(Point3::new(50.0, 0.0, -5.0), Vec3::new(15.0, 30.0, 20.0))
            .unwrap();
        let mut mesh = kernel.tessellate(a, 0.05).unwrap();
        let more = kernel.tessellate(b, 0.05).unwrap();
        let faces_a = mesh.faces.len();
        mesh.faces.extend(more.faces);
        mesh.edges.extend(more.edges);
        let mut scene = Scene::from_mesh(&mesh);
        let px = |r: u8, g: u8, b: u8| [r, g, b, 255];
        let checker = Texture {
            width: 4,
            height: 2,
            rgba: [
                px(200, 60, 40),
                px(240, 220, 200),
                px(200, 60, 40),
                px(240, 220, 200),
                px(240, 220, 200),
                px(200, 60, 40),
                px(240, 220, 200),
                px(200, 60, 40),
            ]
            .concat(),
        };
        let mut materials = Vec::new();
        for i in 0..mesh.faces.len() {
            materials.push(if i < faces_a {
                Material {
                    texture: Some(0),
                    texture_scale: 8.0,
                    ..Material::default()
                }
            } else if i == faces_a {
                Material {
                    color: Some([60, 120, 220]),
                    opacity: 0.4,
                    ..Material::default()
                }
            } else {
                Material {
                    color: Some([60, 120, 220]),
                    ..Material::default()
                }
            });
        }
        scene.set_materials(materials, vec![std::sync::Arc::new(checker)]);
        scene
    }

    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_matches_the_software_renderer_with_materials() {
        let Some(state) = gpu() else {
            return;
        };
        let mut kernel = oxyde_occt::OcctKernel::new();
        let scene = material_scene(&mut kernel);
        let (w, h) = (480usize, 360usize);
        let mut camera = Camera::default();
        camera.set_view(StandardView::Isometric);
        let (lo, hi) = scene.bounds().unwrap();
        camera.fit(
            [lo[0], lo[1] - 20.0, lo[2]],
            [hi[0], hi[1], hi[2]],
            w as f64,
            h as f64,
        );
        let style = Style {
            floor_reflection: 0.4,
            subsurface: 0.5,
            ..Style::LIGHT
        };
        let mut gpu = GpuViewport::new(&state, super::DEFAULT_SAMPLES);
        gpu.set_mesh(&scene.gpu_mesh(), scene.bounds());
        gpu.render(&camera, w as u32, h as u32, &Highlight::default(), &style);
        let (gw, gh, gpu_rgba) = gpu.read_image().unwrap();
        assert_eq!((gw as usize, gh as usize), (w, h));
        let cpu = render_styled(&scene, &camera, w, h, &Highlight::default(), &style);
        snapshot("materials_gpu", gw, gh, &gpu_rgba);
        snapshot("materials_software", gw, gh, &cpu.rgba);

        // Per-pixel differences, ignoring the thin edges and anti-aliasing: the mean
        // absolute difference over the image stays small when both draw the same thing.
        let mut total = 0u64;
        for (a, b) in gpu_rgba.chunks(4).zip(cpu.rgba.chunks(4)) {
            for c in 0..3 {
                total += u64::from(a[c].abs_diff(b[c]));
            }
        }
        let mean = total as f64 / (w * h * 3) as f64;
        assert!(mean < 6.0, "mean channel difference {mean:.2}");
        // The reflection is there in both: below the first box, the background is tinted.
        let below = camera.project([20.0, -12.0, 5.0], w as f64, h as f64);
        let at = |rgba: &[u8], x: f64, y: f64| {
            let i = ((y as usize) * w + x as usize) * 4;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        };
        let plain = render_styled(&scene, &camera, w, h, &Highlight::default(), &Style::LIGHT);
        assert_ne!(at(&gpu_rgba, below[0], below[1]), at(&plain.rgba, below[0], below[1]));
        assert_ne!(at(&cpu.rgba, below[0], below[1]), at(&plain.rgba, below[0], below[1]));
    }

    /// A headless GPU, or `None` on machines without one.
    fn gpu() -> Option<egui_wgpu::RenderState> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default(), None)).ok()?;
        let renderer = egui_wgpu::Renderer::new(&device, super::COLOR_FORMAT, None, 1, false);
        Some(egui_wgpu::RenderState {
            available_adapters: vec![].into(),
            adapter: adapter.into(),
            device: device.into(),
            queue: queue.into(),
            target_format: super::COLOR_FORMAT,
            renderer: std::sync::Arc::new(eframe::egui::mutex::RwLock::new(renderer)),
        })
    }

    #[test]
    fn the_shader_is_valid_wgsl() {
        use wgpu::naga;
        let module = naga::front::wgsl::parse_str(super::SHADER)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(super::SHADER)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(super::SHADER)));
    }

    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_picking_matches_the_software_renderer() {
        let Some(state) = gpu() else {
            return;
        };
        let mut kernel = oxyde_occt::OcctKernel::new();
        let shape = kernel
            .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(40.0, 20.0, 10.0))
            .unwrap();
        let scene = Scene::from_mesh(&kernel.tessellate(shape, 0.05).unwrap());
        let (w, h) = (320usize, 240usize);
        let mut camera = Camera::default();
        camera.set_view(StandardView::Isometric);
        let (lo, hi) = scene.bounds().unwrap();
        camera.fit(lo, hi, w as f64, h as f64);

        let mut gpu = GpuViewport::new(&state, super::DEFAULT_SAMPLES);
        gpu.set_mesh(&scene.gpu_mesh(), scene.bounds());
        gpu.render(
            &camera,
            w as u32,
            h as u32,
            &Highlight::default(),
            &oxyde_render::Style::default(),
        );
        let cpu = render(&scene, &camera, w, h, &Highlight::default());

        let (mut faces, mut agree, mut checked) = (0, 0, 0);
        for y in (5..h - 5).step_by(9) {
            for x in (5..w - 5).step_by(9) {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let a = gpu.pick(px, py, 3.0);
                let b = cpu.pick(px, py, 3.0);
                checked += 1;
                if a == b {
                    agree += 1;
                }
                if matches!(a, Some(Pick::Face(_))) {
                    faces += 1;
                }
            }
        }
        assert!(
            faces > 50,
            "the box covers part of the image ({faces} face picks)"
        );
        // Rasterization rules differ by a pixel along silhouettes and edges.
        assert!(
            agree * 100 >= checked * 95,
            "{agree} of {checked} picks agree"
        );
    }
}
