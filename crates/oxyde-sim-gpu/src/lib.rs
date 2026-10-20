//! Generative design's solve on a graphics card (`docs/SIMULATION.md` §9): the same
//! preconditioned conjugate gradients as `oxyde_sim::generative`'s CPU multigrid, as WGSL
//! compute shaders through `wgpu`, behind `oxyde_sim::generative::Backend`.
//!
//! - **What stays on the CPU**: the multigrid plan (`System::plan`: the Galerkin coarse
//!   levels, the block Jacobi inverses, the smoothing intervals, the coarsest level
//!   inverted), built as the CPU's own solve builds it and uploaded each optimization
//!   iteration with the voxels' stiffness.
//! - **What runs on the card**: the whole solve. The finest level's product is gathered
//!   node by node from the voxels around each node (bricks, Laplacians for void ones, held
//!   nodes projected), never assembled; the coarse levels' from their cells' matrices;
//!   Chebyshev smoothing with each step's correction formed in the product's pass, the
//!   restriction and prolongation, the coarsest level by its inverse; the vector updates,
//!   the dot products (in two passes, summed in a fixed order, so a run repeats exactly on
//!   one card) and the conjugate gradients' scalars, kept in a buffer on the card. The
//!   residual is read back every few iterations to see whether it is small enough.
//! - **Two precisions**: the conjugate gradients (their vectors, dot products, scalars and
//!   the product they take) in double precision, the multigrid cycle that preconditions
//!   them in single. A node's rows of `K u` sum terms near a million newtons down to loads
//!   of tens; single precision cannot follow that cancellation far, and conjugate gradients
//!   run in it drifted: on a grid of high contrast their recursive residual reached 10⁻³
//!   while the true one had not moved. A preconditioner only has to be roughly right. Cards
//!   without double precision in shaders (`SHADER_F64`: Apple's, for one) are refused, and
//!   the CPU solves there.
//!
//! [`GpuBackend::new`] finds a card through the backends the application's own `wgpu` has
//! (Vulkan on Windows and Linux, Metal on macOS: `eframe` leaves Direct3D 12 out) and
//! refuses software ones, so a caller falls back to the CPU's solve when there is none.

// Never crash: a panic in a solve would take the document with it. Errors are returned.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use oxyde_sim::generative::{
    Backend, CellKind, GenerativeError, Plan, PlanLevel, SMOOTHING_DEGREE, SPECTRUM_RATIO, Solved,
    System,
};

const COMMON: &str = include_str!("shaders/common.wgsl");
const FINISH: &str = include_str!("shaders/finish.wgsl");
const FINE: &str = include_str!("shaders/fine_product.wgsl");
const FINE64: &str = include_str!("shaders/fine_product64.wgsl");
const COARSE: &str = include_str!("shaders/coarse_product.wgsl");
const SMALL: &str = include_str!("shaders/small.wgsl");
const SMALL64: &str = include_str!("shaders/small64.wgsl");

/// The coarsest level, solved by its inverse, has at most this many nodes.
const COARSEST_NODES: usize = 64;
/// Invocations per workgroup, as the shaders declare.
const GROUP: u32 = 256;
/// Values a dot product's first pass sums per workgroup.
const DOT_BLOCK: u32 = 1024;
/// Parameter slots are this far apart in their buffer (the dynamic offsets' alignment).
const SLOT: u64 = 256;
const PARAMS_BYTES: u64 = 80;
/// Conjugate gradient iterations between two looks at the residual.
const BATCH: usize = 8;
const FREE: u32 = u32::MAX;
const INACTIVE: u32 = u32::MAX - 1;
/// The scalars' slots, as the shaders name them.
const RZ: u32 = 0;
const PQ: u32 = 1;
const RZ_NEXT: u32 = 4;
const RR: u32 = 5;
const SCALARS: usize = 8;

/// The kernels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum K {
    // The cycle, in single precision.
    FineProduct,
    CoarseProduct,
    DinvStart,
    Add,
    Restrict,
    Prolong,
    Coarsest,
    // The conjugate gradients, in double.
    FineProduct64,
    ToF32,
    ToF64,
    Copy64,
    DotPartial64,
    DotFinish64,
    Alpha64,
    Beta64,
    Start64,
    UpdateXr64,
    UpdateDir64,
}

impl K {
    const ALL: [K; 18] = [
        K::FineProduct,
        K::CoarseProduct,
        K::DinvStart,
        K::Add,
        K::Restrict,
        K::Prolong,
        K::Coarsest,
        K::FineProduct64,
        K::ToF32,
        K::ToF64,
        K::Copy64,
        K::DotPartial64,
        K::DotFinish64,
        K::Alpha64,
        K::Beta64,
        K::Start64,
        K::UpdateXr64,
        K::UpdateDir64,
    ];

    fn name(self) -> &'static str {
        match self {
            K::FineProduct => "fine_product",
            K::CoarseProduct => "coarse_product",
            K::DinvStart => "dinv_start",
            K::Add => "add",
            K::Restrict => "restrict",
            K::Prolong => "prolong",
            K::Coarsest => "coarsest",
            K::FineProduct64 => "fine_product64",
            K::ToF32 => "to_f32",
            K::ToF64 => "to_f64",
            K::Copy64 => "copy64",
            K::DotPartial64 => "dot_partial64",
            K::DotFinish64 => "dot_finish64",
            K::Alpha64 => "alpha64",
            K::Beta64 => "beta64",
            K::Start64 => "start64",
            K::UpdateXr64 => "update_xr64",
            K::UpdateDir64 => "update_dir64",
        }
    }

    /// Whether each binding is written, in binding order, as the shader declares it.
    fn writes(self) -> &'static [bool] {
        match self {
            K::FineProduct | K::CoarseProduct => {
                &[false, false, true, false, false, false, false, false]
            }
            K::FineProduct64 => &[false, false, true, false, false, false, false],
            K::DinvStart => &[false, false, true, true],
            K::Add => &[true, false],
            K::Restrict | K::Prolong | K::ToF32 | K::Copy64 | K::DotFinish64 => &[false, true],
            K::Coarsest | K::DotPartial64 => &[false, false, true],
            K::ToF64 => &[false, true, false, false],
            K::Alpha64 => &[true],
            K::Beta64 | K::Start64 => &[true, true],
            K::UpdateXr64 => &[true, true, false, false, false],
            K::UpdateDir64 => &[true, false, false],
        }
    }

    /// The kernel's WGSL: the shared header, then its own part.
    fn source(self) -> Option<String> {
        let body = match self {
            K::FineProduct => format!("{FINE}\n{FINISH}"),
            K::CoarseProduct => format!("{COARSE}\n{FINISH}"),
            K::FineProduct64 => FINE64.to_owned(),
            K::DinvStart | K::Add | K::Restrict | K::Prolong | K::Coarsest => {
                section(SMALL, self.name())?
            }
            _ => section(SMALL64, self.name())?,
        };
        Some(format!("{COMMON}\n{body}"))
    }
}

/// The section of `file` headed `//!! name`.
fn section(file: &str, name: &str) -> Option<String> {
    let mut out: Option<String> = None;
    for line in file.lines() {
        if let Some(header) = line.strip_prefix("//!! ") {
            if out.is_some() {
                break;
            }
            if header.trim() == name {
                out = Some(String::new());
            }
            continue;
        }
        if let Some(text) = out.as_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    out
}

struct Kernel {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

/// A dispatch's parameters, as `Params` in common.wgsl lays them out.
#[derive(Debug, Clone, Copy, Default)]
struct Params {
    cells: [u32; 3],
    count: u32,
    mode: u32,
    slot: u32,
    c1: f32,
    c2: f32,
    coarse: [u32; 3],
    factor: [u32; 3],
}

impl Params {
    fn bytes(&self) -> [u8; PARAMS_BYTES as usize] {
        let words: [u32; 20] = [
            self.cells[0],
            self.cells[1],
            self.cells[2],
            self.count,
            self.mode,
            self.slot,
            0,
            0,
            self.c1.to_bits(),
            self.c2.to_bits(),
            0,
            0,
            self.coarse[0],
            self.coarse[1],
            self.coarse[2],
            0,
            self.factor[0],
            self.factor[1],
            self.factor[2],
            0,
        ];
        let mut out = [0u8; PARAMS_BYTES as usize];
        for (chunk, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *chunk = w.to_le_bytes();
        }
        out
    }
}

/// Why there is no [`GpuBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No graphics card, only a software renderer, or one without double precision in its
    /// shaders: the CPU's solve is the one to use.
    NoCard(String),
    /// A card, but it would not take the solver (a driver refusing a shader, no memory).
    Failed(String),
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unavailable::NoCard(why) | Unavailable::Failed(why) => f.write_str(why),
        }
    }
}

/// Generative design's solves on a graphics card.
pub struct GpuBackend {
    device: wgpu::Device,
    queue: wgpu::Queue,
    name: String,
    kernels: BTreeMap<K, Kernel>,
    params_layout: wgpu::BindGroupLayout,
    /// Buffers kept from one solve to the next, by name, grown when too small.
    buffers: BTreeMap<String, wgpu::Buffer>,
    /// What wgpu reported going wrong, instead of panicking as it would by default.
    error: Arc<Mutex<Option<String>>>,
}

/// Waits for a future that `wgpu` completes when the device is polled (or at once, before
/// there is a device).
fn block_on<F: std::future::Future>(device: Option<&wgpu::Device>, future: F) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    for _ in 0..100_000 {
        if let std::task::Poll::Ready(v) = future.as_mut().poll(&mut cx) {
            return Some(v);
        }
        match device {
            Some(d) => {
                d.poll(wgpu::Maintain::Wait);
            }
            None => std::thread::yield_now(),
        }
    }
    None
}

fn gpu_error(why: impl std::fmt::Display) -> GenerativeError {
    GenerativeError::Solver(format!("on the graphics card: {why}"))
}

impl GpuBackend {
    /// The fastest graphics card `wgpu` finds, ready to solve; why not when there is none
    /// (or only a software one, or one without double precision), and the CPU's solve
    /// should be used, or when the card would not take the solver.
    pub fn new() -> Result<GpuBackend, Unavailable> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = block_on(
            None,
            instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            }),
        )
        .flatten()
        .ok_or_else(|| Unavailable::NoCard("no graphics card was found".to_owned()))?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return Err(Unavailable::NoCard(format!(
                "{} is a software renderer",
                info.name
            )));
        }
        if !adapter.features().contains(wgpu::Features::SHADER_F64) {
            return Err(Unavailable::NoCard(format!(
                "{} has no double precision in its shaders",
                info.name
            )));
        }
        let supported = adapter.limits();
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: supported.max_storage_buffer_binding_size,
            max_buffer_size: supported.max_buffer_size,
            ..wgpu::Limits::default()
        };
        let (device, queue) = block_on(
            None,
            adapter.request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("oxyde generative design"),
                    required_features: wgpu::Features::SHADER_F64,
                    required_limits: limits,
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            ),
        )
        .ok_or_else(|| Unavailable::Failed("the graphics card did not answer".to_owned()))?
        .map_err(|e| Unavailable::Failed(e.to_string()))?;
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let sink = error.clone();
        device.on_uncaptured_error(Box::new(move |e| {
            let mut slot = sink.lock().unwrap_or_else(PoisonError::into_inner);
            slot.get_or_insert_with(|| e.to_string());
        }));
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("generative params"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(PARAMS_BYTES),
                },
                count: None,
            }],
        });
        let mut kernels = BTreeMap::new();
        for k in K::ALL {
            let source = k
                .source()
                .ok_or_else(|| Unavailable::Failed(format!("kernel {} has no source", k.name())))?;
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(k.name()),
                source: wgpu::ShaderSource::Wgsl(Cow::Owned(source)),
            });
            let entries: Vec<wgpu::BindGroupLayoutEntry> = k
                .writes()
                .iter()
                .enumerate()
                .map(|(i, &w)| wgpu::BindGroupLayoutEntry {
                    binding: i as u32,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: !w },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                })
                .collect();
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(k.name()),
                entries: &entries,
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(k.name()),
                bind_group_layouts: &[&layout, &params_layout],
                push_constant_ranges: &[],
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(k.name()),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            });
            kernels.insert(k, Kernel { pipeline, layout });
        }
        let backend = GpuBackend {
            device,
            queue,
            name: format!("{} ({:?})", info.name, info.backend),
            kernels,
            params_layout,
            buffers: BTreeMap::new(),
            error,
        };
        // Compiling the shaders is where a driver would object.
        backend.device.poll(wgpu::Maintain::Wait);
        backend
            .check()
            .map_err(|e| Unavailable::Failed(e.to_string()))?;
        Ok(backend)
    }

    /// The card's name and the API used to reach it.
    pub fn name(&self) -> &str {
        &self.name
    }

    fn check(&self) -> Result<(), GenerativeError> {
        let mut slot = self.error.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.take() {
            Some(e) => Err(gpu_error(e)),
            None => Ok(()),
        }
    }

    /// Makes sure buffer `name` holds at least `bytes`.
    fn reserve(&mut self, name: &str, bytes: u64) {
        let bytes = bytes.max(16).next_multiple_of(8);
        if self.buffers.get(name).is_some_and(|b| b.size() >= bytes) {
            return;
        }
        // A quarter to spare, so that a level growing a little does not reallocate.
        let size = (bytes + bytes / 4).next_multiple_of(16);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(name),
            size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        self.buffers.insert(name.to_owned(), buffer);
    }

    fn buffer(&self, name: &str) -> Result<&wgpu::Buffer, GenerativeError> {
        self.buffers
            .get(name)
            .ok_or_else(|| gpu_error(format!("buffer {name} is missing")))
    }

    /// Writes `bytes` into buffer `name`, reserving it first.
    fn upload(&mut self, name: &str, bytes: &[u8]) -> Result<(), GenerativeError> {
        self.reserve(name, bytes.len() as u64);
        if !bytes.is_empty() {
            self.queue.write_buffer(self.buffer(name)?, 0, bytes);
        }
        Ok(())
    }

    fn upload_f32(
        &mut self,
        name: &str,
        values: impl ExactSizeIterator<Item = f32>,
    ) -> Result<(), GenerativeError> {
        let mut bytes = Vec::with_capacity(4 * values.len());
        for v in values {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        self.upload(name, &bytes)
    }

    fn upload_f64(
        &mut self,
        name: &str,
        values: impl ExactSizeIterator<Item = f64>,
    ) -> Result<(), GenerativeError> {
        let mut bytes = Vec::with_capacity(8 * values.len());
        for v in values {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        self.upload(name, &bytes)
    }

    fn upload_u32(
        &mut self,
        name: &str,
        values: impl ExactSizeIterator<Item = u32>,
    ) -> Result<(), GenerativeError> {
        let mut bytes = Vec::with_capacity(4 * values.len());
        for v in values {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        self.upload(name, &bytes)
    }

    /// A bind group of `kernel` over the buffers named, in binding order.
    fn group(&self, kernel: K, names: &[&str]) -> Result<wgpu::BindGroup, GenerativeError> {
        let k = self
            .kernels
            .get(&kernel)
            .ok_or_else(|| gpu_error("a kernel is missing"))?;
        let mut entries = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: i as u32,
                resource: self.buffer(name)?.as_entire_binding(),
            });
        }
        Ok(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(kernel.name()),
            layout: &k.layout,
            entries: &entries,
        }))
    }

    /// Reads `count` doubles of buffer `name` back.
    fn read_f64(&mut self, name: &str, count: usize) -> Result<Vec<f64>, GenerativeError> {
        let bytes = (8 * count as u64).max(8);
        let fits = self
            .buffers
            .get("readback")
            .is_some_and(|b| b.size() >= bytes);
        if !fits {
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size: bytes.next_multiple_of(16),
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.buffers.insert("readback".to_owned(), buffer);
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_buffer_to_buffer(self.buffer(name)?, 0, self.buffer("readback")?, 0, bytes);
        self.queue.submit(Some(encoder.finish()));
        let staging = self.buffer("readback")?;
        let slice = staging.slice(0..bytes);
        let done: Arc<Mutex<Option<Result<(), wgpu::BufferAsyncError>>>> =
            Arc::new(Mutex::new(None));
        let sink = done.clone();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            *sink.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        let mapped = done.lock().unwrap_or_else(PoisonError::into_inner).take();
        match mapped {
            Some(Ok(())) => {}
            Some(Err(e)) => return Err(gpu_error(e)),
            None => return Err(gpu_error("the results did not come back")),
        }
        let values = {
            let data = slice.get_mapped_range();
            data.as_chunks::<8>()
                .0
                .iter()
                .take(count)
                .map(|c| f64::from_le_bytes(*c))
                .collect()
        };
        staging.unmap();
        self.check()?;
        Ok(values)
    }

    /// The parameter slots in a uniform buffer, and their bind group.
    fn params(
        &self,
        recorder: &Recorder,
    ) -> Result<(wgpu::Buffer, wgpu::BindGroup), GenerativeError> {
        let mut bytes = vec![0u8; recorder.params.len().max(1) * SLOT as usize];
        for (i, p) in recorder.params.iter().enumerate() {
            let at = i * SLOT as usize;
            bytes[at..at + PARAMS_BYTES as usize].copy_from_slice(&p.bytes());
        }
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("generative params"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buffer, 0, &bytes);
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("generative params"),
            layout: &self.params_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(PARAMS_BYTES),
                }),
            }],
        });
        self.check()?;
        Ok((buffer, group))
    }

    /// Records `list` in one compute pass and submits it.
    fn run(
        &self,
        recorder: &Recorder,
        params: &wgpu::BindGroup,
        list: &[Dispatch],
    ) -> Result<(), GenerativeError> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("generative solve"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("generative solve"),
                timestamp_writes: None,
            });
            for d in list {
                let k = self
                    .kernels
                    .get(&d.kernel)
                    .ok_or_else(|| gpu_error("a kernel is missing"))?;
                let group = recorder
                    .groups
                    .get(d.group)
                    .ok_or_else(|| gpu_error("a bind group is missing"))?;
                pass.set_pipeline(&k.pipeline);
                pass.set_bind_group(0, group, &[]);
                pass.set_bind_group(1, params, &[(u64::from(d.slot) * SLOT) as u32]);
                let (gx, gy) = if d.groups <= 65_535 {
                    (d.groups, 1)
                } else {
                    (65_535, d.groups.div_ceil(65_535))
                };
                pass.dispatch_workgroups(gx, gy, 1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        Ok(())
    }
}

/// The Chebyshev coefficients `(c1, c2)` of each smoothing step for an interval topped at
/// `upper`, as the CPU's smoother works them out.
fn chebyshev(upper: f64) -> [(f32, f32); SMOOTHING_DEGREE] {
    let lower = upper / SPECTRUM_RATIO;
    let theta = 0.5 * (upper + lower);
    let delta = 0.5 * (upper - lower);
    let sigma = theta / delta;
    let mut rho = 1.0 / sigma;
    let mut out = [(0.0f32, 0.0f32); SMOOTHING_DEGREE];
    for (step, slot) in out.iter_mut().enumerate() {
        *slot = if step == 0 {
            (0.0, (1.0 / theta) as f32)
        } else {
            let next = 1.0 / (2.0 * sigma - rho);
            let c = ((next * rho) as f32, (2.0 * next / delta) as f32);
            rho = next;
            c
        };
    }
    out
}

fn nodes(cells: [usize; 3]) -> usize {
    cells.iter().map(|n| n + 1).product()
}

fn cells32(cells: [usize; 3]) -> [u32; 3] {
    cells.map(|n| n as u32)
}

fn norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

/// What one dispatch needs: its kernel, its buffers, its parameters, how many groups.
#[derive(Debug, Clone, Copy)]
struct Dispatch {
    kernel: K,
    group: usize,
    slot: u32,
    groups: u32,
}

/// The bind groups and parameters a solve's dispatches use.
struct Recorder {
    groups: Vec<wgpu::BindGroup>,
    params: Vec<Params>,
}

impl Recorder {
    fn slot(&mut self, params: Params) -> u32 {
        self.params.push(params);
        (self.params.len() - 1) as u32
    }
}

/// The groups needed for `records` records, at `GROUP` a group.
fn groups_for(records: usize) -> u32 {
    (records as u32).div_ceil(GROUP).max(1)
}

/// A level's dispatches in the cycle.
struct LevelDispatches {
    smooth_first: Dispatch,
    smooth_steps: Vec<Dispatch>,
    post_steps: Vec<Dispatch>,
    add: Dispatch,
    residual: Dispatch,
    restrict: Dispatch,
    prolong: Dispatch,
}

/// A solve's dispatches: those before the first iteration and those of one iteration.
struct Script {
    start: Vec<Dispatch>,
    iteration: Vec<Dispatch>,
}

impl Backend for GpuBackend {
    fn solve(
        &mut self,
        system: &System<'_>,
        x: &mut [f64],
        tolerance: f64,
        max_iterations: usize,
        cancel: &AtomicBool,
    ) -> Result<Solved, GenerativeError> {
        let scale = norm(system.loads());
        if scale == 0.0 {
            x.fill(0.0);
            return Ok(Solved {
                iterations: 0,
                residual: 0.0,
            });
        }
        let plan = system.plan(COARSEST_NODES)?;
        self.load(system, &plan, x, max_iterations)?;
        let mut recorder = Recorder {
            groups: Vec::new(),
            params: Vec::new(),
        };
        let script = self.script(system, &plan, &mut recorder)?;
        let (_params_buffer, params) = self.params(&recorder)?;
        self.run(&recorder, &params, &script.start)?;
        let goal = (tolerance * scale) * (tolerance * scale);
        let mut done = 0usize;
        let mut last = f64::INFINITY;
        let mut reached = None;
        while done < max_iterations {
            if cancel.load(Ordering::Relaxed) {
                return Err(GenerativeError::Cancelled);
            }
            let batch = BATCH.min(max_iterations - done);
            let mut list: Vec<Dispatch> = Vec::with_capacity(batch * script.iteration.len());
            for _ in 0..batch {
                list.extend(script.iteration.iter().copied());
            }
            self.run(&recorder, &params, &list)?;
            done += batch;
            let history = self.read_f64("history", done + 1)?;
            if history.iter().any(|v| v.is_nan()) {
                return Err(gpu_error("the solve broke down"));
            }
            last = history.last().copied().unwrap_or(f64::INFINITY);
            if let Some(k) = history.iter().position(|&rr| rr <= goal) {
                reached = Some((k, history[k].max(0.0).sqrt() / scale));
                break;
            }
        }
        let values = self.read_f64("x64", x.len())?;
        x.copy_from_slice(&values);
        match reached {
            Some((iterations, residual)) => Ok(Solved {
                iterations,
                residual,
            }),
            None => Err(GenerativeError::NotConverged {
                iterations: done,
                residual: last.max(0.0).sqrt() / scale,
            }),
        }
    }
}

impl GpuBackend {
    /// Uploads the system, its plan and the start into the buffers the script binds.
    fn load(
        &mut self,
        system: &System<'_>,
        plan: &Plan,
        x: &[f64],
        max_iterations: usize,
    ) -> Result<(), GenerativeError> {
        let cells = system.cells();
        let n = nodes(cells);
        let stiffness = system.stiffness();
        let signed: Vec<f64> = (0..stiffness.len())
            .map(|e| match system.kind(e) {
                CellKind::None => 0.0,
                CellKind::Brick => stiffness[e],
                CellKind::Laplacian => -stiffness[e],
            })
            .collect();
        self.upload_f32("cell", signed.iter().map(|&v| v as f32))?;
        self.upload_f64("cell64", signed.iter().copied())?;
        let consts: Vec<f64> = system
            .brick()
            .iter()
            .chain(system.laplacian().iter())
            .copied()
            .collect();
        self.upload_f32("consts", consts.iter().map(|&v| v as f32))?;
        self.upload_f64("consts64", consts.iter().copied())?;
        let held = system.held();
        let active = system.active();
        let flags: Vec<u32> = (0..n)
            .map(|node| {
                if !active.get(node).copied().unwrap_or(false) {
                    INACTIVE
                } else {
                    held.get(node).copied().unwrap_or(FREE)
                }
            })
            .collect();
        self.upload_u32("held", flags.into_iter())?;
        let projectors = system.projectors();
        let mut proj = vec![0.0f64; 12 * projectors.len().max(1)];
        for (node, &h) in held.iter().enumerate() {
            if h == FREE {
                continue;
            }
            let Some(p) = projectors.get(h as usize) else {
                continue;
            };
            let o = 12 * h as usize;
            proj[o..o + 9].copy_from_slice(p);
            proj[o + 9] = system.held_stiffness(node);
        }
        self.upload_f32("proj", proj.iter().map(|&v| v as f32))?;
        self.upload_f64("proj64", proj.iter().copied())?;
        self.upload_f64("loads64", system.loads().iter().copied())?;
        self.upload_f64("x64", x.iter().copied())?;
        for name in ["r64", "z64", "dir64", "q64"] {
            self.reserve(name, 24 * n as u64);
        }
        for name in ["r32", "z32", "d0", "r0"] {
            self.reserve(name, 12 * n as u64);
        }
        for (l, level) in plan.levels.iter().enumerate() {
            let dinv: Vec<f32> = level
                .dinv
                .iter()
                .flat_map(|m| m.iter().map(|&v| v as f32))
                .collect();
            self.upload_f32(&format!("dinv{l}"), dinv.into_iter())?;
            if l == 0 {
                continue;
            }
            self.upload_u32(&format!("info{l}"), level.cell.iter().copied())?;
            self.upload_f32(&format!("mat{l}"), level.matrices.iter().copied())?;
            self.upload_f32(&format!("scal{l}"), level.scalars.iter().copied())?;
            self.upload_u32(
                &format!("active{l}"),
                level.active.iter().map(|&a| u32::from(a)),
            )?;
            let ln = nodes(level.cells);
            for v in ["x", "b", "d", "r"] {
                self.reserve(&format!("{v}{l}"), 12 * ln as u64);
            }
        }
        self.upload_f32("inverse", plan.coarsest_inverse.iter().map(|&v| v as f32))?;
        self.reserve(
            "partials",
            8 * (3 * n as u64).div_ceil(u64::from(DOT_BLOCK)),
        );
        self.upload_f64("scalars", std::iter::repeat_n(0.0, SCALARS))?;
        self.upload_f64(
            "history",
            std::iter::repeat_n(f64::INFINITY, max_iterations + 2),
        )?;
        self.check()
    }

    /// The names of level `l`'s solution, right-hand side, correction and residual.
    fn level_vectors(l: usize) -> [String; 4] {
        if l == 0 {
            ["z32".into(), "r32".into(), "d0".into(), "r0".into()]
        } else {
            [
                format!("x{l}"),
                format!("b{l}"),
                format!("d{l}"),
                format!("r{l}"),
            ]
        }
    }

    /// The dispatches of a solve, their bind groups and parameters into `rec`.
    fn script(
        &self,
        system: &System<'_>,
        plan: &Plan,
        rec: &mut Recorder,
    ) -> Result<Script, GenerativeError> {
        let fine_cells = system.cells();
        let n = nodes(fine_cells);
        let values = 3 * n;
        let last = plan.levels.len().saturating_sub(1);
        let add_group =
            |rec: &mut Recorder, kernel: K, names: &[&str]| -> Result<usize, GenerativeError> {
                rec.groups.push(self.group(kernel, names)?);
                Ok(rec.groups.len() - 1)
            };
        let count = |records: usize| Params {
            count: records as u32,
            ..Params::default()
        };

        // The cycle, in single precision: level by level, then the coarsest.
        let mut levels: Vec<LevelDispatches> = Vec::new();
        for (l, level) in plan.levels.iter().enumerate().take(last) {
            let ln = nodes(level.cells);
            let [xv, bv, dv, rv] = Self::level_vectors(l);
            let dinv = format!("dinv{l}");
            let product_kernel = if l == 0 {
                K::FineProduct
            } else {
                K::CoarseProduct
            };
            let product_names = |out: &str| -> Vec<String> {
                if l == 0 {
                    vec![
                        xv.clone(),
                        bv.clone(),
                        out.to_owned(),
                        "cell".into(),
                        "consts".into(),
                        "held".into(),
                        "proj".into(),
                        dinv.clone(),
                    ]
                } else {
                    vec![
                        xv.clone(),
                        bv.clone(),
                        out.to_owned(),
                        format!("info{l}"),
                        format!("mat{l}"),
                        format!("scal{l}"),
                        format!("active{l}"),
                        dinv.clone(),
                    ]
                }
            };
            let smooth_group = add_group(rec, product_kernel, &as_strs(&product_names(&dv)))?;
            let resid_group = add_group(rec, product_kernel, &as_strs(&product_names(&rv)))?;
            let coefficients = chebyshev(level.upper);
            let base = Params {
                cells: cells32(level.cells),
                count: ln as u32,
                ..Params::default()
            };
            let smooth_first = Dispatch {
                kernel: K::DinvStart,
                group: add_group(rec, K::DinvStart, &[&bv, &dinv, &dv, &xv])?,
                slot: rec.slot(Params {
                    c2: coefficients[0].1,
                    ..base
                }),
                groups: groups_for(ln),
            };
            let step = |rec: &mut Recorder, c: (f32, f32)| Dispatch {
                kernel: product_kernel,
                group: smooth_group,
                slot: rec.slot(Params {
                    mode: 2,
                    c1: c.0,
                    c2: c.1,
                    ..base
                }),
                groups: groups_for(ln),
            };
            let smooth_steps: Vec<Dispatch> =
                coefficients[1..].iter().map(|&c| step(rec, c)).collect();
            let post_steps: Vec<Dispatch> = coefficients.iter().map(|&c| step(rec, c)).collect();
            let add = Dispatch {
                kernel: K::Add,
                group: add_group(rec, K::Add, &[&xv, &dv])?,
                slot: rec.slot(count(3 * ln)),
                groups: groups_for(3 * ln),
            };
            let residual = Dispatch {
                kernel: product_kernel,
                group: resid_group,
                slot: rec.slot(Params { mode: 1, ..base }),
                groups: groups_for(ln),
            };
            let next: &PlanLevel = plan
                .levels
                .get(l + 1)
                .ok_or_else(|| gpu_error("the plan ends early"))?;
            let [next_x, next_b, _, _] = Self::level_vectors(l + 1);
            let transfer = Params {
                cells: cells32(level.cells),
                coarse: cells32(next.cells),
                factor: level.factor.map(|f| f as u32),
                ..Params::default()
            };
            let restrict = Dispatch {
                kernel: K::Restrict,
                group: add_group(rec, K::Restrict, &[&rv, &next_b])?,
                slot: rec.slot(Params {
                    count: nodes(next.cells) as u32,
                    ..transfer
                }),
                groups: groups_for(nodes(next.cells)),
            };
            let prolong = Dispatch {
                kernel: K::Prolong,
                group: add_group(rec, K::Prolong, &[&next_x, &xv])?,
                slot: rec.slot(Params {
                    count: ln as u32,
                    ..transfer
                }),
                groups: groups_for(ln),
            };
            levels.push(LevelDispatches {
                smooth_first,
                smooth_steps,
                post_steps,
                add,
                residual,
                restrict,
                prolong,
            });
        }
        let coarsest_level = plan
            .levels
            .get(last)
            .ok_or_else(|| gpu_error("the plan has no level"))?;
        let unknowns = 3 * nodes(coarsest_level.cells);
        let [coarsest_x, coarsest_b, _, _] = Self::level_vectors(last);
        let coarsest = Dispatch {
            kernel: K::Coarsest,
            group: add_group(rec, K::Coarsest, &["inverse", &coarsest_b, &coarsest_x])?,
            slot: rec.slot(count(unknowns)),
            groups: groups_for(unknowns),
        };
        let mut cycle: Vec<Dispatch> = Vec::new();
        for lv in &levels {
            cycle.push(lv.smooth_first);
            for s in &lv.smooth_steps {
                cycle.push(*s);
                cycle.push(lv.add);
            }
            cycle.push(lv.residual);
            cycle.push(lv.restrict);
        }
        cycle.push(coarsest);
        for lv in levels.iter().rev() {
            cycle.push(lv.prolong);
            for s in &lv.post_steps {
                cycle.push(*s);
                cycle.push(lv.add);
            }
        }

        // The conjugate gradients, in double precision, z = M⁻¹ r through the cycle.
        let fine64 = |rec: &mut Recorder,
                      x: &str,
                      b: &str,
                      out: &str,
                      mode: u32|
         -> Result<Dispatch, GenerativeError> {
            Ok(Dispatch {
                kernel: K::FineProduct64,
                group: add_group(
                    rec,
                    K::FineProduct64,
                    &[x, b, out, "cell64", "consts64", "held", "proj64"],
                )?,
                slot: rec.slot(Params {
                    cells: cells32(fine_cells),
                    count: n as u32,
                    mode,
                    ..Params::default()
                }),
                groups: groups_for(n),
            })
        };
        let init_residual = fine64(rec, "x64", "loads64", "r64", 1)?;
        let product_q = fine64(rec, "dir64", "loads64", "q64", 0)?;
        let to_f32 = Dispatch {
            kernel: K::ToF32,
            group: add_group(rec, K::ToF32, &["r64", "r32"])?,
            slot: rec.slot(count(values)),
            groups: groups_for(values),
        };
        let to_f64 = Dispatch {
            kernel: K::ToF64,
            group: add_group(rec, K::ToF64, &["z32", "z64", "held", "proj64"])?,
            slot: rec.slot(count(n)),
            groups: groups_for(n),
        };
        let copy = Dispatch {
            kernel: K::Copy64,
            group: add_group(rec, K::Copy64, &["z64", "dir64"])?,
            slot: rec.slot(count(values)),
            groups: groups_for(values),
        };
        let partial_groups = (values as u32).div_ceil(DOT_BLOCK).max(1);
        let finish_group = add_group(rec, K::DotFinish64, &["partials", "scalars"])?;
        let dot = |rec: &mut Recorder,
                   a: &str,
                   b: &str,
                   slot: u32|
         -> Result<[Dispatch; 2], GenerativeError> {
            Ok([
                Dispatch {
                    kernel: K::DotPartial64,
                    group: add_group(rec, K::DotPartial64, &[a, b, "partials"])?,
                    slot: rec.slot(count(values)),
                    groups: partial_groups,
                },
                Dispatch {
                    kernel: K::DotFinish64,
                    group: finish_group,
                    slot: rec.slot(Params {
                        count: partial_groups,
                        slot,
                        ..Params::default()
                    }),
                    groups: 1,
                },
            ])
        };
        let rz = dot(rec, "r64", "z64", RZ)?;
        let rr = dot(rec, "r64", "r64", RR)?;
        let pq = dot(rec, "dir64", "q64", PQ)?;
        let rz_next = dot(rec, "r64", "z64", RZ_NEXT)?;
        let one =
            |rec: &mut Recorder, kernel: K, names: &[&str]| -> Result<Dispatch, GenerativeError> {
                Ok(Dispatch {
                    kernel,
                    group: add_group(rec, kernel, names)?,
                    slot: rec.slot(Params::default()),
                    groups: 1,
                })
            };
        let start = one(rec, K::Start64, &["scalars", "history"])?;
        let alpha = one(rec, K::Alpha64, &["scalars"])?;
        let beta = one(rec, K::Beta64, &["scalars", "history"])?;
        let update_xr = Dispatch {
            kernel: K::UpdateXr64,
            group: add_group(
                rec,
                K::UpdateXr64,
                &["x64", "r64", "dir64", "q64", "scalars"],
            )?,
            slot: rec.slot(count(values)),
            groups: groups_for(values),
        };
        let update_dir = Dispatch {
            kernel: K::UpdateDir64,
            group: add_group(rec, K::UpdateDir64, &["dir64", "z64", "scalars"])?,
            slot: rec.slot(count(values)),
            groups: groups_for(values),
        };

        let mut start_list = vec![init_residual, to_f32];
        start_list.extend(cycle.iter().copied());
        start_list.push(to_f64);
        start_list.push(copy);
        start_list.extend(rz);
        start_list.extend(rr);
        start_list.push(start);

        let mut iteration = vec![product_q];
        iteration.extend(pq);
        iteration.push(alpha);
        iteration.push(update_xr);
        iteration.extend(rr);
        iteration.push(to_f32);
        iteration.extend(cycle);
        iteration.push(to_f64);
        iteration.extend(rz_next);
        iteration.push(beta);
        iteration.push(update_dir);
        Ok(Script {
            start: start_list,
            iteration,
        })
    }
}

fn as_strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests;
