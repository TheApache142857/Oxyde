//! The Render tab's path tracer on the GPU: `pathtrace.wgsl` run with wgpu on the device
//! egui already draws with.
//!
//! The scene (`TraceScene::gpu_scene`) is uploaded once per scene; a render keeps three
//! sums per pixel on the GPU (colour and alpha, first surface's colour and depth, its
//! normal) and adds one sample per pixel at a time, a band of rows per dispatch. Each
//! frame of the window submits as many bands as fit in a time budget, measured from when
//! the GPU reports the previous submission done, so the window stays responsive and no
//! dispatch runs long enough for Windows to reset the driver. A second pass develops the
//! sums into the texture egui shows, every frame. When the render is done (or a picture is
//! saved) the sums are read back and developed on the CPU exactly as the CPU renderer does
//! (`develop_sums`), denoiser included.
//!
//! Anything that goes wrong comes back as an `Err` (a shader or pipeline the driver
//! refuses, a scene or picture too big for the device's limits), and the Render tab falls
//! back to the CPU renderer.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use oxyde_render::pathtrace::{GpuFrame, GpuScene, PARAMS_BYTES, RenderImage, develop_sums};

pub const SHADER: &str = include_str!("pathtrace.wgsl");

/// Uniform blocks are bound at offsets that are multiples of 256.
const PARAMS_STRIDE: u64 = 512;
/// Dispatches a frame may submit: one uniform block each.
const MAX_BANDS: u64 = 32;
const WORKGROUP: u32 = 8;
/// The time one submission should take on the GPU, seconds.
const TARGET_SECONDS: f64 = 0.03;

/// Waits for a wgpu future (an error scope) by polling the device.
fn block_on<F: Future>(device: &wgpu::Device, future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    loop {
        if let std::task::Poll::Ready(v) = future.as_mut().poll(&mut cx) {
            return v;
        }
        device.poll(wgpu::Maintain::Wait);
    }
}

/// Runs `f` and reports a validation or memory error wgpu raised meanwhile.
fn checked<T>(device: &wgpu::Device, f: impl FnOnce() -> T) -> Result<T, String> {
    device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let value = f();
    let validation = block_on(device, device.pop_error_scope());
    let memory = block_on(device, device.pop_error_scope());
    match validation.or(memory) {
        Some(e) => Err(e.to_string()),
        None => Ok(value),
    }
}

/// The sums of a render read back from the GPU, for `develop_sums`.
pub struct Sums {
    pub width: u32,
    pub height: u32,
    pub samples: u32,
    pub color: Vec<[f32; 4]>,
    /// Empty when the denoiser is not wanted.
    pub albedo: Vec<[f32; 4]>,
    pub normals: Vec<[f32; 4]>,
}

/// A whole buffer bound at `binding`.
fn whole(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

struct SceneBuffers {
    version: u64,
    nodes: wgpu::Buffer,
    tris: wgpu::Buffer,
    looks: wgpu::Buffer,
}

struct Target {
    width: u32,
    height: u32,
    /// Row length of the developed picture, in pixels (a multiple of 64, as texture
    /// copies want rows of 256 bytes).
    stride: u32,
    accum: wgpu::Buffer,
    feat_a: wgpu::Buffer,
    feat_b: wgpu::Buffer,
    picture: wgpu::Buffer,
    texture: wgpu::Texture,
    texture_id: egui::TextureId,
}

/// The GPU path tracer.
pub struct GpuTracer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    renderer: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    layout: wgpu::BindGroupLayout,
    trace: wgpu::ComputePipeline,
    develop: wgpu::ComputePipeline,
    params: wgpu::Buffer,
    scene: Option<SceneBuffers>,
    target: Option<Target>,
    bind_group: Option<wgpu::BindGroup>,
    frame: Option<GpuFrame>,
    /// Complete samples, and rows of the next one done.
    sample: u32,
    next_row: u32,
    /// Pixels per frame, adapted to the GPU's speed.
    budget: u64,
    in_flight: Arc<AtomicBool>,
    /// When the last submission was made, and how long it took (microseconds).
    submitted: Option<(Instant, u64)>,
    took: Arc<AtomicU64>,
    pub name: String,
}

impl GpuTracer {
    pub fn new(state: &egui_wgpu::RenderState) -> Result<Self, String> {
        let device = state.device.clone();
        let info = state.adapter.get_info();
        let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let built = checked(&device, || {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("oxyde path tracer"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("oxyde path tracer"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: wgpu::BufferSize::new(PARAMS_BYTES as u64),
                        },
                        count: None,
                    },
                    storage(1, true),
                    storage(2, true),
                    storage(3, true),
                    storage(4, false),
                    storage(5, false),
                    storage(6, false),
                    storage(7, false),
                ],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("oxyde path tracer"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
            let pipeline = |entry: &str| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: Some(&pipeline_layout),
                    module: &module,
                    entry_point: entry,
                    compilation_options: Default::default(),
                    cache: None,
                })
            };
            let trace = pipeline("trace");
            let develop = pipeline("develop");
            let params = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("oxyde path tracer params"),
                size: PARAMS_STRIDE * MAX_BANDS,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            (layout, trace, develop, params)
        })?;
        let (layout, trace, develop, params) = built;
        Ok(GpuTracer {
            device,
            queue: state.queue.clone(),
            renderer: state.renderer.clone(),
            layout,
            trace,
            develop,
            params,
            scene: None,
            target: None,
            bind_group: None,
            frame: None,
            sample: 0,
            next_row: 0,
            budget: 256 * 1024,
            in_flight: Arc::new(AtomicBool::new(false)),
            submitted: None,
            took: Arc::new(AtomicU64::new(0)),
            name: format!("{} ({:?})", info.name, info.backend),
        })
    }

    /// Why a scene of these arrays and a picture of this size cannot be traced on this
    /// device, if they cannot (the device's buffer limits).
    pub fn refuses(&self, scene: &GpuScene, width: u32, height: u32) -> Option<String> {
        let limits = self.device.limits();
        let largest = u64::from(limits.max_storage_buffer_binding_size).min(limits.max_buffer_size);
        let mb = |bytes: u64| bytes.div_ceil(1024 * 1024);
        let tris = scene.tris.len() as u64;
        let per_pixel = u64::from(width) * u64::from(height) * 16;
        if tris.max(scene.nodes.len() as u64) > largest {
            return Some(format!(
                "the model has {:.1} million triangles ({} MB), more than the GPU's largest \
                 buffer ({} MB); Mesh detail Normal makes far fewer",
                scene.tri_count() as f64 / 1e6,
                mb(tris),
                mb(largest)
            ));
        }
        (per_pixel > largest).then(|| {
            format!(
                "the picture needs {} MB per buffer, more than the GPU's largest ({} MB); a \
                 smaller size fits",
                mb(per_pixel),
                mb(largest)
            )
        })
    }

    fn storage_buffer(&self, label: &str, bytes: &[u8]) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        // A binding may not be empty: tiny arrays are padded. Big ones are uploaded as they
        // are, never copied (a scene's triangles can be hundreds of megabytes).
        let padded;
        let contents = if bytes.len() < 64 {
            let mut data = bytes.to_vec();
            data.resize(64, 0);
            padded = data;
            &padded[..]
        } else {
            bytes
        };
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage: wgpu::BufferUsages::STORAGE,
            })
    }

    /// Whether the scene of version `version` is uploaded.
    pub fn has_scene(&self, version: u64) -> bool {
        self.scene.as_ref().is_some_and(|s| s.version == version)
    }

    /// Lets the picture's buffers go (the scene stays): nothing is rendering.
    pub fn idle(&mut self) {
        self.bind_group = None;
        self.frame = None;
        self.free_target();
    }

    /// The picture's size, once a render has started.
    pub fn size(&self) -> Option<(u32, u32)> {
        self.target.as_ref().map(|t| (t.width, t.height))
    }

    /// Uploads the scene of version `version`, unless it is there already.
    pub fn set_scene(&mut self, version: u64, scene: &GpuScene) -> Result<(), String> {
        if self.scene.as_ref().is_some_and(|s| s.version == version) {
            return Ok(());
        }
        self.scene = None;
        self.bind_group = None;
        let buffers = checked(&self.device, || SceneBuffers {
            version,
            nodes: self.storage_buffer("oxyde trace nodes", &scene.nodes),
            tris: self.storage_buffer("oxyde trace triangles", &scene.tris),
            looks: self.storage_buffer("oxyde trace looks", &scene.looks),
        })?;
        self.scene = Some(buffers);
        Ok(())
    }

    /// Starts a render of `frame`: the sums cleared, the picture resized if needed.
    pub fn start(&mut self, frame: GpuFrame) -> Result<(), String> {
        let (w, h) = (frame.width.max(1), frame.height.max(1));
        if self
            .target
            .as_ref()
            .is_none_or(|t| t.width != w || t.height != h)
        {
            self.bind_group = None;
            self.free_target();
            self.target = Some(checked(&self.device, || self.make_target(w, h))?);
        }
        if self.bind_group.is_none() {
            self.bind_group = Some(self.make_bind_group()?);
        }
        if let Some(t) = &self.target {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("oxyde trace clear"),
                });
            encoder.clear_buffer(&t.accum, 0, None);
            encoder.clear_buffer(&t.feat_a, 0, None);
            encoder.clear_buffer(&t.feat_b, 0, None);
            self.queue.submit([encoder.finish()]);
        }
        self.frame = Some(frame);
        self.sample = 0;
        self.next_row = 0;
        Ok(())
    }

    fn make_target(&self, width: u32, height: u32) -> Target {
        let pixels = u64::from(width) * u64::from(height);
        let sums = |label| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: pixels * 16,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let stride = width.div_ceil(64) * 64;
        let picture = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("oxyde trace picture"),
            size: u64::from(stride) * u64::from(height) * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("oxyde trace texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // The shader writes sRGB-encoded bytes; sampling decodes them, as for the
            // viewport's own texture.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let texture_id = self.renderer.write().register_native_texture(
            &self.device,
            &view,
            wgpu::FilterMode::Linear,
        );
        Target {
            width,
            height,
            stride,
            accum: sums("oxyde trace colour"),
            feat_a: sums("oxyde trace albedo"),
            feat_b: sums("oxyde trace normals"),
            picture,
            texture,
            texture_id,
        }
    }

    fn free_target(&mut self) {
        if let Some(t) = self.target.take() {
            self.renderer.write().free_texture(&t.texture_id);
        }
    }

    fn make_bind_group(&self) -> Result<wgpu::BindGroup, String> {
        let (Some(s), Some(t)) = (&self.scene, &self.target) else {
            return Err("no scene or picture to trace".to_owned());
        };
        checked(&self.device, || {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("oxyde path tracer"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.params,
                            offset: 0,
                            size: wgpu::BufferSize::new(PARAMS_BYTES as u64),
                        }),
                    },
                    whole(1, &s.nodes),
                    whole(2, &s.tris),
                    whole(3, &s.looks),
                    whole(4, &t.accum),
                    whole(5, &t.feat_a),
                    whole(6, &t.feat_b),
                    whole(7, &t.picture),
                ],
            })
        })
    }

    /// Complete samples so far.
    pub fn samples(&self) -> u32 {
        self.sample
    }

    pub fn is_done(&self) -> bool {
        self.frame.as_ref().is_none_or(|f| self.sample >= f.samples)
            && !self.in_flight.load(Ordering::Relaxed)
    }

    /// The texture egui shows, once a render has started.
    pub fn texture(&self) -> Option<egui::TextureId> {
        self.target.as_ref().map(|t| t.texture_id)
    }

    /// Adds as many bands of samples as the frame's budget allows, then develops the
    /// picture. Does nothing while the previous submission is still on the GPU.
    pub fn step(&mut self) {
        self.device.poll(wgpu::Maintain::Poll);
        if self.in_flight.load(Ordering::Relaxed) {
            return;
        }
        // Adapt the budget to how long the last submission took.
        if let Some((_, pixels)) = self.submitted.take() {
            let took = self.took.load(Ordering::Relaxed) as f64 * 1e-6;
            if took > 0.0 && pixels > 0 {
                let rate = pixels as f64 / took;
                let wanted = (rate * TARGET_SECONDS) as u64;
                self.budget = wanted.clamp(16 * 1024, 64 * 1024 * 1024);
            }
        }
        let (Some(frame), Some(target), Some(group)) =
            (&self.frame, &self.target, &self.bind_group)
        else {
            return;
        };
        let (w, h) = (target.width, target.height);
        let mut blocks: Vec<u8> = Vec::new();
        let mut bands: Vec<(u32, u32)> = Vec::new();
        let mut left = self.budget;
        let (mut sample, mut row) = (self.sample, self.next_row);
        while sample < frame.samples && left > 0 && (bands.len() as u64) < MAX_BANDS - 1 {
            let rows = ((left / u64::from(w)).max(1) as u32).min(h - row);
            let mut block = frame.params(sample, row, rows, 0, 0, target.stride);
            block.resize(PARAMS_STRIDE as usize, 0);
            blocks.extend_from_slice(&block);
            bands.push((rows, sample));
            left = left.saturating_sub(u64::from(rows) * u64::from(w));
            row += rows;
            if row >= h {
                row = 0;
                sample += 1;
            }
        }
        // The develop pass's block: how far the samples got.
        let mut block = frame.params(sample, 0, 0, row, sample, target.stride);
        block.resize(PARAMS_STRIDE as usize, 0);
        let develop_offset = blocks.len() as u32;
        blocks.extend_from_slice(&block);
        self.queue.write_buffer(&self.params, 0, &blocks);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("oxyde trace"),
            });
        let mut traced = 0u64;
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("oxyde trace"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.trace);
            for (i, (rows, _)) in bands.iter().enumerate() {
                pass.set_bind_group(0, group, &[(i as u64 * PARAMS_STRIDE) as u32]);
                pass.dispatch_workgroups(w.div_ceil(WORKGROUP), rows.div_ceil(WORKGROUP), 1);
                traced += u64::from(*rows) * u64::from(w);
            }
            // Until a whole sample is in, the texture keeps the last picture (of the view
            // before, while it moves) rather than showing a half-drawn one.
            if sample > 0 {
                pass.set_pipeline(&self.develop);
                pass.set_bind_group(0, group, &[develop_offset]);
                pass.dispatch_workgroups(w.div_ceil(WORKGROUP), h.div_ceil(WORKGROUP), 1);
            }
        }
        if sample > 0 {
            encoder.copy_buffer_to_texture(
                wgpu::ImageCopyBuffer {
                    buffer: &target.picture,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(target.stride * 4),
                        rows_per_image: Some(h),
                    },
                },
                wgpu::ImageCopyTexture {
                    texture: &target.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
        self.in_flight.store(true, Ordering::Relaxed);
        let start = Instant::now();
        self.queue.submit([encoder.finish()]);
        let (flag, took) = (self.in_flight.clone(), self.took.clone());
        self.queue.on_submitted_work_done(move || {
            took.store(start.elapsed().as_micros() as u64, Ordering::Relaxed);
            flag.store(false, Ordering::Relaxed);
        });
        self.submitted = Some((start, traced));
        self.sample = sample;
        self.next_row = row;
    }

    /// Reads a sums buffer back, waiting for the GPU.
    fn read_back(&self, buffer: &wgpu::Buffer, pixels: u64) -> Option<Vec<[f32; 4]>> {
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("oxyde trace read-back"),
            size: pixels * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("oxyde trace read-back"),
            });
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, pixels * 16);
        self.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        let mapped = Arc::new(AtomicBool::new(false));
        let ok = mapped.clone();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            ok.store(r.is_ok(), Ordering::Relaxed)
        });
        self.device.poll(wgpu::Maintain::Wait);
        if !mapped.load(Ordering::Relaxed) {
            return None;
        }
        let out = {
            let data = slice.get_mapped_range();
            let floats: Vec<f32> = data
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect();
            floats.as_chunks::<4>().0.to_vec()
        };
        staging.unmap();
        Some(out)
    }

    /// The sums so far, read back from the GPU (waiting for it), one buffer at a time to
    /// keep memory down; the features only for the denoiser.
    pub fn read_sums(&self, denoise: bool) -> Option<Sums> {
        let t = self.target.as_ref()?;
        self.device.poll(wgpu::Maintain::Wait);
        let pixels = u64::from(t.width) * u64::from(t.height);
        let color = self.read_back(&t.accum, pixels)?;
        let (albedo, normals) = if denoise {
            (
                self.read_back(&t.feat_a, pixels)?,
                self.read_back(&t.feat_b, pixels)?,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        Some(Sums {
            width: t.width,
            height: t.height,
            // Rows of a sample in progress have one sample more; averaged over the whole
            // samples they come out a hair brighter, only in a picture saved mid-render.
            samples: self.sample.max(1),
            color,
            albedo,
            normals,
        })
    }

    /// The picture so far, developed on the CPU as the CPU renderer develops its own
    /// (denoised if asked).
    pub fn image(&self, exposure: f32, denoise: bool) -> Option<RenderImage> {
        let s = self.read_sums(denoise)?;
        Some(develop_sums(
            s.width as usize,
            s.height as usize,
            s.samples,
            exposure,
            denoise,
            &s.color,
            &s.albedo,
            &s.normals,
        ))
    }
}

impl Drop for GpuTracer {
    fn drop(&mut self) {
        self.free_target();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_is_valid_wgsl() {
        use wgpu::naga;
        let module = naga::front::wgsl::parse_str(SHADER)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        let _ = info;
        let entries: Vec<&str> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(entries.contains(&"trace") && entries.contains(&"develop"));
    }

    #[test]
    fn the_shader_structs_match_the_layouts_written_on_the_cpu() {
        use oxyde_render::pathtrace::{LOOK_BYTES, NODE_BYTES, TRI_BYTES};
        use wgpu::naga;
        let module = naga::front::wgsl::parse_str(SHADER).unwrap();
        let mut layouter = naga::proc::Layouter::default();
        layouter.update(module.to_ctx()).unwrap();
        let size_of = |name: &str| {
            let (handle, _) = module
                .types
                .iter()
                .find(|(_, t)| t.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("no struct {name}"));
            layouter[handle].size as usize
        };
        assert_eq!(size_of("Node"), NODE_BYTES);
        assert_eq!(size_of("Tri"), TRI_BYTES);
        assert_eq!(size_of("Look"), LOOK_BYTES);
        assert_eq!(size_of("Params"), PARAMS_BYTES);
        assert!(PARAMS_BYTES as u64 <= PARAMS_STRIDE && PARAMS_STRIDE.is_multiple_of(256));
    }

    /// A headless GPU, or `None` on machines without one.
    fn gpu() -> Option<egui_wgpu::RenderState> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default(), None)).ok()?;
        let renderer =
            egui_wgpu::Renderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb, None, 1, false);
        Some(egui_wgpu::RenderState {
            available_adapters: vec![].into(),
            adapter: adapter.into(),
            device: device.into(),
            queue: queue.into(),
            target_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
        })
    }

    /// The furnace test on the GPU, and the GPU against the CPU on the same scene: means
    /// within a few per cent (the random numbers differ, so pixels do not match).
    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_tracing_matches_the_cpu() {
        use oxyde_kernel::{FaceMesh, Mesh};
        use oxyde_render::pathtrace::{
            Background, Environment, EnvironmentKind, Floor, Look, RenderCamera, RenderSettings,
            SceneBuilder, render_image,
        };
        use oxyde_render::{Camera, StandardView};
        let Some(state) = gpu() else {
            return;
        };
        // A sphere, as in the CPU renderer's tests.
        let (rings, segments) = (24u32, 48u32);
        let mut positions = Vec::new();
        for i in 0..=rings {
            let theta = std::f32::consts::PI * i as f32 / rings as f32;
            for j in 0..segments {
                let phi = std::f32::consts::TAU * j as f32 / segments as f32;
                positions.push([
                    10.0 * theta.sin() * phi.cos(),
                    10.0 * theta.cos(),
                    -10.0 * theta.sin() * phi.sin(),
                ]);
            }
        }
        let mut triangles = Vec::new();
        for i in 0..rings {
            for j in 0..segments {
                let a = i * segments + j;
                let b = i * segments + (j + 1) % segments;
                triangles.push([a, a + segments, b]);
                triangles.push([b, a + segments, b + segments]);
            }
        }
        let mesh = Mesh {
            faces: vec![FaceMesh {
                positions,
                triangles,
            }],
            edges: Vec::new(),
        };
        for (look, env) in [
            (
                Look {
                    base_color: [1.0; 3],
                    roughness: 1.0,
                    ..Look::default()
                },
                EnvironmentKind::Uniform,
            ),
            (
                Look::from_srgb([200, 120, 60], 1.0, 0.3, 0.0),
                EnvironmentKind::Studio,
            ),
        ] {
            let mut b = SceneBuilder::new();
            let l = b.add_look(look);
            b.add_mesh(&mesh, &[l]);
            let scene = Arc::new(b.build());
            let mut camera = Camera::default();
            camera.set_view(StandardView::Front);
            camera.height = 30.0;
            let rc = RenderCamera::from_view(&camera, Some(50.0));
            let settings = RenderSettings {
                width: 64,
                height: 64,
                samples: 64,
                environment: Environment {
                    kind: env,
                    ..Environment::default()
                },
                background: Background::Environment,
                floor: Floor::ShadowCatcher,
                ..RenderSettings::default()
            };
            let mut tracer = GpuTracer::new(&state).unwrap();
            tracer.set_scene(1, &scene.gpu_scene()).unwrap();
            tracer.start(GpuFrame::new(&scene, &rc, &settings)).unwrap();
            while !tracer.is_done() {
                tracer.step();
            }
            let gpu = tracer.image(0.0, false).unwrap();
            let cpu = render_image(scene, rc, settings, false);
            let mean = |img: &RenderImage| {
                img.rgba.iter().map(|&v| f64::from(v)).sum::<f64>() / img.rgba.len() as f64
            };
            let (g, c) = (mean(&gpu), mean(&cpu));
            assert!(
                (g - c).abs() < 4.0,
                "{env:?}: GPU mean {g:.1}, CPU mean {c:.1}"
            );
        }
    }
}
