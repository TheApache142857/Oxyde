//! GPU rendering of the viewport with wgpu (eframe's wgpu backend).
//!
//! The part is drawn off-screen, with 4× multisampling and a depth buffer, into a texture
//! that egui shows like any image; the sketch and annotation overlays are painted over it by
//! egui as before. A second pass writes face and edge ids into an integer texture: picking
//! reads back the few pixels under the cursor, so it stays exact. Colours, shading and edge
//! widths match the software renderer (`oxyde-render`), which remains the fallback when no
//! GPU is available.
//!
//! The projection is computed in the shaders from the camera's frame (target, right, up,
//! eye), relative to the target, so millimetre coordinates keep their precision.

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use oxyde_render::{Camera, GpuMesh, Highlight, Pick};

const SAMPLES: u32 = 4;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const ID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;
/// Set on edge ids in the id texture; face ids are index + 1.
const EDGE_BIT: u32 = 0x8000_0000;

const SHADER: &str = r#"
struct View {
    center: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    eye: vec4<f32>,
    key: vec4<f32>,
    // x, y: millimetres to clip space; z: depth scale; w: edge depth bias (mm).
    scale: vec4<f32>,
    // x, y: viewport size in pixels.
    viewport: vec4<f32>,
};

@group(0) @binding(0) var<uniform> view: View;
// Bit 0: hovered, bit 1: selected.
@group(0) @binding(1) var<storage, read> face_flags: array<u32>;
@group(0) @binding(2) var<storage, read> edge_flags: array<u32>;

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
    let top = vec3<f32>(205.0, 214.0, 228.0);
    let bottom = vec3<f32>(246.0, 247.0, 249.0);
    return vec4<f32>(linear(mix(top, bottom, clamp(in.t, 0.0, 1.0))), 1.0);
}

// ---- Faces --------------------------------------------------------------------------------

struct FaceOut {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) @interpolate(flat) face: u32,
};

@vertex
fn face_vs(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) face: u32,
) -> FaceOut {
    var out: FaceOut;
    out.position = project(position, 0.0);
    out.normal = normal;
    out.face = face;
    return out;
}

@fragment
fn face_fs(in: FaceOut) -> @location(0) vec4<f32> {
    var n = normalize(in.normal);
    // Seen from outside: face the viewer (also tolerates inconsistent winding).
    if (dot(n, view.eye.xyz) < 0.0) {
        n = -n;
    }
    let light = 0.38 + 0.37 * dot(n, view.eye.xyz) + 0.40 * max(dot(n, view.key.xyz), 0.0);
    let flags = face_flags[in.face];
    var base = vec3<f32>(182.0, 190.0, 202.0);
    if ((flags & 1u) != 0u) {
        base = vec3<f32>(255.0, 178.0, 92.0);
    } else if ((flags & 2u) != 0u) {
        base = vec3<f32>(96.0, 160.0, 255.0);
    }
    return vec4<f32>(linear(min(base * light, vec3<f32>(255.0))), 1.0);
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
    var width = 1.25;
    if (edge_flags[edge] != 0u) {
        width = 3.0;
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
    var color = vec3<f32>(32.0, 36.0, 44.0);
    if ((flags & 1u) != 0u) {
        color = vec3<f32>(255.0, 128.0, 0.0);
    } else if ((flags & 2u) != 0u) {
        color = vec3<f32>(0.0, 110.0, 255.0);
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
    color: wgpu::TextureView,
    resolved: wgpu::Texture,
    resolved_view: wgpu::TextureView,
    depth: wgpu::TextureView,
    ids: wgpu::Texture,
    ids_view: wgpu::TextureView,
    ids_depth: wgpu::TextureView,
}

impl Targets {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
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
        let color = make("oxyde color", COLOR_FORMAT, SAMPLES, attachment);
        let resolved = make(
            "oxyde image",
            COLOR_FORMAT,
            1,
            attachment | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth = make("oxyde depth", DEPTH_FORMAT, SAMPLES, attachment);
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
            color: view(&color),
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
    indices: wgpu::Buffer,
    index_count: u32,
    segments: wgpu::Buffer,
    segment_count: u32,
    face_count: usize,
    edge_count: usize,
}

pub struct GpuViewport {
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
    renderer: std::sync::Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    background: wgpu::RenderPipeline,
    faces: wgpu::RenderPipeline,
    edges: wgpu::RenderPipeline,
    face_ids: wgpu::RenderPipeline,
    edge_ids: wgpu::RenderPipeline,
    mesh: Option<MeshBuffers>,
    /// Bounds of the scene, for the depth range.
    bounds: Option<([f64; 3], [f64; 3])>,
    targets: Option<Targets>,
    texture: Option<egui::TextureId>,
}

impl GpuViewport {
    pub fn new(state: &egui_wgpu::RenderState) -> Self {
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
            ],
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
                        format: wgpu::TextureFormat,
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
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
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
        let background = pipeline(
            "background",
            "background_vs",
            "background_fs",
            &[],
            COLOR_FORMAT,
            SAMPLES,
            depth(false, wgpu::CompareFunction::Always),
        );
        let faces = pipeline(
            "faces",
            "face_vs",
            "face_fs",
            &face_buffers,
            COLOR_FORMAT,
            SAMPLES,
            depth(true, less),
        );
        let edges = pipeline(
            "edges",
            "edge_vs",
            "edge_fs",
            &edge_buffers,
            COLOR_FORMAT,
            SAMPLES,
            depth(true, wgpu::CompareFunction::LessEqual),
        );
        let face_ids = pipeline(
            "face ids",
            "face_vs",
            "face_id_fs",
            &face_buffers,
            ID_FORMAT,
            1,
            depth(true, less),
        );
        let edge_ids = pipeline(
            "edge ids",
            "edge_vs",
            "edge_id_fs",
            &edge_buffers,
            ID_FORMAT,
            1,
            depth(true, wgpu::CompareFunction::LessEqual),
        );
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("oxyde view"),
            size: 7 * 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            device,
            queue: state.queue.clone(),
            renderer: state.renderer.clone(),
            layout,
            uniforms,
            background,
            faces,
            edges,
            face_ids,
            edge_ids,
            mesh: None,
            bounds: None,
            targets: None,
            texture: None,
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
        let face_count = mesh.faces.iter().max().map_or(0, |m| *m as usize + 1);
        let edge_count = mesh
            .segments
            .iter()
            .map(|s| s.2)
            .max()
            .map_or(0, |m| m as usize + 1);
        self.mesh = Some(MeshBuffers {
            vertices: buffer("oxyde vertices", &pad(vertices), wgpu::BufferUsages::VERTEX),
            indices: buffer(
                "oxyde indices",
                &pad(bytes_u32(mesh.indices.iter().copied())),
                wgpu::BufferUsages::INDEX,
            ),
            index_count: mesh.indices.len() as u32,
            segments: buffer("oxyde segments", &pad(segments), wgpu::BufferUsages::VERTEX),
            segment_count: mesh.segments.len() as u32,
            face_count,
            edge_count,
        });
    }

    /// Renders the scene for `camera` at `width` × `height` pixels and returns the texture
    /// egui shows.
    pub fn render(
        &mut self,
        camera: &Camera,
        width: u32,
        height: u32,
        highlight: &Highlight,
    ) -> egui::TextureId {
        let (width, height) = (width.max(1), height.max(1));
        let resized = self
            .targets
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height);
        if resized {
            let targets = Targets::new(&self.device, width, height);
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
        let v4 = |v: [f64; 3]| [v[0] as f32, v[1] as f32, v[2] as f32, 0.0];
        let mut view: Vec<f32> = Vec::with_capacity(28);
        view.extend(v4(camera.target));
        view.extend(v4(right));
        view.extend(v4(up));
        view.extend(v4(eye));
        view.extend(v4(key));
        view.extend([
            (2.0 / (camera.height * aspect)) as f32,
            (2.0 / camera.height) as f32,
            (1.0 / (2.0 * reach)) as f32,
            (camera.height * 0.004) as f32,
        ]);
        view.extend([width as f32, height as f32, 0.0, 0.0]);
        self.queue.write_buffer(&self.uniforms, 0, &bytes_f32(view));

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
        if let Some(pick) = highlight.hovered {
            flag(pick, 1);
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
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("oxyde view"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: face_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: edge_buffer.as_entire_binding(),
                },
            ],
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
                    view: &targets.color,
                    resolve_target: Some(&targets.resolved_view),
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
                pass.set_pipeline(&self.faces);
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                pass.set_pipeline(&self.edges);
                pass.set_vertex_buffer(0, mesh.segments.slice(..));
                pass.draw(0..6, 0..mesh.segment_count);
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
                pass.set_pipeline(&self.edge_ids);
                pass.set_vertex_buffer(0, mesh.segments.slice(..));
                pass.draw(0..6, 0..mesh.segment_count);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.texture.unwrap_or_default()
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
    use oxyde_render::{Camera, Highlight, Pick, Scene, StandardView, render};

    use super::GpuViewport;

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

        let mut gpu = GpuViewport::new(&state);
        gpu.set_mesh(&scene.gpu_mesh(), scene.bounds());
        gpu.render(&camera, w as u32, h as u32, &Highlight::default());
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
