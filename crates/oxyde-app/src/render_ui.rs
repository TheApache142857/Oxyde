//! The Render tab: the part or assembly drawn by the path tracer (`oxyde_render::pathtrace`)
//! with real materials, environment light and soft shadows, in place of the viewport's
//! engineering shading, as SolidWorks Visualize and Fusion 360's Render workspace do.
//!
//! While the tab is open the viewport shows the render, refined pass after pass on every
//! core; orbiting, panning or zooming shows the usual viewport and starts the render again
//! once the view is still. The panel at the top of the left side holds the setup: the
//! environment, background and floor, the lens, a finish per material (metal, plastic,
//! glass…), the samples and the picture size; *Save image…* writes the picture as a PNG.
//!
//! The model is meshed again for the render, finer than the display mesh, on the UI
//! thread when the model or its looks change (not when only the camera moves); the render
//! runs on a thread of its own, which posts a picture about every 150 ms.
//!
//! Nothing here is saved in the document: the setup and the finishes are preferences
//! (`render` in `preferences.json`), the finishes keyed by material name, or by the part's
//! document id for a part without a material. Where render settings belong in the file
//! formats is the owner's call (`docs/RENDERING.md` §5).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use oxyde_assembly::Placement;
use oxyde_format::stl::StlQuality;
use oxyde_kernel::{Kernel, Mesh};
use oxyde_model::{Appearance, Appearances, Body, Finish, Material, Part};
use oxyde_render::pathtrace::{
    Background, Environment, EnvironmentKind, Floor, Look, RenderCamera, RenderImage,
    RenderSettings, Renderer, SceneBuilder, TraceScene,
};
use oxyde_render::{Camera, Rgb};
use serde::{Deserialize, Serialize};

use crate::app::OxydeApp;
use crate::appearance_ui::TextureCache;
use crate::viewport::ViewportOutput;

// ---- Preferences ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentChoice {
    Studio,
    Sky,
    Uniform,
}

impl EnvironmentChoice {
    fn kind(self) -> EnvironmentKind {
        match self {
            EnvironmentChoice::Studio => EnvironmentKind::Studio,
            EnvironmentChoice::Sky => EnvironmentKind::Sky,
            EnvironmentChoice::Uniform => EnvironmentKind::Uniform,
        }
    }

    const ALL: [EnvironmentChoice; 3] = [
        EnvironmentChoice::Studio,
        EnvironmentChoice::Sky,
        EnvironmentChoice::Uniform,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundChoice {
    Environment,
    Color,
    Transparent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FloorChoice {
    None,
    Shadow,
    Visible,
}

/// How finely the model is meshed for the render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    Normal,
    Fine,
    VeryFine,
}

impl Detail {
    const ALL: [(Detail, &'static str); 3] = [
        (Detail::Normal, "Normal"),
        (Detail::Fine, "Fine"),
        (Detail::VeryFine, "Very fine"),
    ];

    fn label(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(d, _)| *d == self)
            .map_or("Fine", |(_, l)| l)
    }

    fn quality(self) -> StlQuality {
        match self {
            Detail::Normal => StlQuality::Normal,
            Detail::Fine => StlQuality::Fine,
            Detail::VeryFine => StlQuality::VeryFine,
        }
    }
}

/// The render setup, kept in the preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderPrefs {
    pub environment: EnvironmentChoice,
    /// 1 = as designed.
    pub strength: f32,
    /// Turn of the environment about the vertical, degrees.
    pub rotation: f32,
    /// The lights keep their place relative to the camera.
    pub lights_follow_view: bool,
    pub background: BackgroundChoice,
    pub background_color: Rgb,
    pub floor: FloorChoice,
    pub floor_color: Rgb,
    /// 0 = matte, 1 = mirror.
    pub floor_gloss: f32,
    pub perspective: bool,
    /// Full-frame equivalent, mm.
    pub focal_length: f64,
    pub samples: u32,
    pub bounces: u32,
    pub denoise: bool,
    /// Stops.
    pub exposure: f32,
    /// Fraction of the viewport's pixels rendered, when the size follows the viewport.
    pub resolution: f32,
    /// A fixed picture size instead of the viewport's.
    pub custom_size: bool,
    pub width: u32,
    pub height: u32,
    pub detail: Detail,
    /// Finish ids chosen by the user, by finish group (a material name, or `part:` and
    /// a document id).
    pub finishes: BTreeMap<String, String>,
}

impl Default for RenderPrefs {
    fn default() -> Self {
        Self {
            environment: EnvironmentChoice::Studio,
            strength: 1.0,
            rotation: 0.0,
            lights_follow_view: true,
            background: BackgroundChoice::Environment,
            background_color: [255, 255, 255],
            floor: FloorChoice::Shadow,
            floor_color: [200, 200, 204],
            floor_gloss: 0.3,
            perspective: true,
            focal_length: 50.0,
            samples: 256,
            bounces: 8,
            denoise: true,
            exposure: 0.0,
            resolution: 1.0,
            custom_size: false,
            width: 1920,
            height: 1080,
            detail: Detail::Fine,
            finishes: BTreeMap::new(),
        }
    }
}

impl RenderPrefs {
    fn settings(&self, width: usize, height: usize) -> RenderSettings {
        RenderSettings {
            width,
            height,
            samples: self.samples.clamp(1, 100_000),
            max_bounces: self.bounces.clamp(1, 64),
            environment: Environment {
                kind: self.environment.kind(),
                strength: self.strength.clamp(0.0, 100.0),
                rotation: self.rotation.to_radians(),
                follow_camera: self.lights_follow_view,
            },
            background: match self.background {
                BackgroundChoice::Environment => Background::Environment,
                BackgroundChoice::Color => Background::Color(self.background_color),
                BackgroundChoice::Transparent => Background::Transparent,
            },
            floor: match self.floor {
                FloorChoice::None => Floor::None,
                FloorChoice::Shadow => Floor::ShadowCatcher,
                FloorChoice::Visible => Floor::Visible {
                    color: self.floor_color,
                    gloss: self.floor_gloss,
                },
            },
            exposure: self.exposure,
            seed: 0,
            threads: 0,
        }
    }

    fn focal(&self) -> Option<f64> {
        self.perspective
            .then_some(self.focal_length.clamp(8.0, 1200.0))
    }

    /// The finish the user chose for `key`, if any.
    fn chosen(&self, key: &str) -> Option<Finish> {
        self.finishes.get(key).and_then(|id| Finish::from_id(id))
    }
}

// ---- Finishes and looks --------------------------------------------------------------------

/// The finish group of a part: its material's name, or the part itself when it has none.
pub(crate) fn finish_key(part: &Part, document_id: &str) -> String {
    match part.material() {
        Some(m) => m.name.clone(),
        None => format!("part:{document_id}"),
    }
}

/// How one face looks in a render or a glTF file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceLook {
    pub color: [u8; 3],
    /// 0 = opaque.
    pub transparency: f32,
    pub finish: Finish,
}

impl FaceLook {
    /// The look of a face drawn with `appearance` (or the viewport's `default_color`), on
    /// a part of material `material` in finish group `key`.
    pub(crate) fn new(
        prefs: &RenderPrefs,
        key: &str,
        material: Option<&Material>,
        appearance: Option<&Appearance>,
        default_color: Rgb,
    ) -> FaceLook {
        FaceLook {
            color: appearance.map_or(default_color, |a| a.color),
            transparency: appearance.map_or(0.0, |a| a.transparency as f32),
            finish: prefs
                .chosen(key)
                .unwrap_or_else(|| Finish::guess(material, appearance)),
        }
    }

    pub(crate) fn gltf(&self) -> oxyde_format::gltf::GltfMaterial {
        oxyde_format::gltf::GltfMaterial::from_finish(
            self.color,
            f64::from(self.transparency),
            self.finish,
        )
    }
}

/// The path tracer's look for a face: the finish's parameters over the appearance's
/// colour, transparency and texture.
fn trace_look(
    look: &FaceLook,
    appearance: Option<&Appearance>,
    cache: &mut TextureCache,
    builder: &mut SceneBuilder,
) -> Look {
    let p = look.finish.params();
    let glass = look.finish == Finish::Glass;
    let color = look.finish.render_color(look.color);
    let mut out = Look::from_srgb(
        color,
        p.metallic as f32,
        p.roughness as f32,
        p.transmission as f32,
    );
    out.ior = p.ior as f32;
    if !glass {
        out.opacity = (1.0 - look.transparency).clamp(0.0, 1.0);
    }
    if let Some(t) = appearance.and_then(|a| a.texture.as_ref())
        && let Some(decoded) = cache.get(t)
    {
        out.texture = Some(builder.add_texture(decoded));
        out.texture_scale = t.scale as f32;
    }
    out
}

// ---- The scene -----------------------------------------------------------------------------

/// A solid to render: where it is and what decides its looks.
struct SolidItem<'a> {
    part: &'a Part,
    body: &'a Body,
    solid: usize,
    /// A component's own appearance, over the part's.
    own: Option<&'a Appearance>,
    placement: Option<Placement>,
    key: String,
    label: String,
    /// The same part file's same solid meshes the same: meshed once.
    mesh_id: (String, usize),
}

/// A finish group, for the panel's Surfaces list.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceGroup {
    pub key: String,
    pub label: String,
    /// What the finish would be without a choice.
    pub automatic: Finish,
}

/// What the meshes were made for.
#[derive(Debug, Clone, Copy, PartialEq)]
struct GeometryKey {
    revision: u64,
    assembly: bool,
    detail: Detail,
}

/// What the looks were made from.
#[derive(Debug, Clone, PartialEq)]
enum LooksSource {
    Part(Appearances, Option<Material>),
    Assembly(Vec<Option<Appearance>>),
}

#[derive(Debug, Clone, PartialEq)]
struct LooksKey {
    source: LooksSource,
    finishes: BTreeMap<String, String>,
    default_color: Rgb,
}

struct CachedScene {
    geometry: GeometryKey,
    meshes: Vec<Mesh>,
    looks: Option<LooksKey>,
    scene: Option<Arc<TraceScene>>,
    /// Bumped each time the scene is rebuilt, so a render of the old one restarts.
    version: u64,
}

// ---- The render job ------------------------------------------------------------------------

/// What the render thread posts.
struct Published {
    image: RenderImage,
    samples: u32,
    target: u32,
    /// A quick low-resolution first look, not the picture itself.
    preview: bool,
    done: bool,
    seconds: f64,
}

#[derive(Default)]
struct Shared {
    latest: Option<Published>,
    failed: Option<String>,
}

struct RenderJob {
    cancel: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
}

impl RenderJob {
    fn spawn(
        scene: Arc<TraceScene>,
        camera: RenderCamera,
        settings: RenderSettings,
        denoise: bool,
    ) -> RenderJob {
        let cancel = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (c, s) = (cancel.clone(), shared.clone());
        let spawned = std::thread::Builder::new()
            .name("oxyde render".into())
            .spawn(move || {
                let result = oxyde_kernel::catch_panic(|| {
                    render_thread(&scene, camera, settings, denoise, &c, &s)
                });
                if let Err(message) = result
                    && let Ok(mut shared) = s.lock()
                {
                    shared.failed = Some(message);
                }
            });
        if let Err(e) = spawned
            && let Ok(mut s) = shared.lock()
        {
            s.failed = Some(format!("the render thread could not start: {e}"));
        }
        RenderJob { cancel, shared }
    }
}

impl Drop for RenderJob {
    /// The thread notices at its next row and ends; nothing waits for it.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn publish(shared: &Mutex<Shared>, p: Published) {
    if let Ok(mut s) = shared.lock() {
        s.latest = Some(p);
    }
}

fn render_thread(
    scene: &Arc<TraceScene>,
    camera: RenderCamera,
    settings: RenderSettings,
    denoise: bool,
    cancel: &AtomicBool,
    shared: &Mutex<Shared>,
) {
    let start = Instant::now();
    // A first look at a quarter of the size, one sample: there in a blink.
    if settings.width >= 64 && settings.height >= 64 {
        let small = RenderSettings {
            width: settings.width / 4,
            height: settings.height / 4,
            samples: 1,
            ..settings
        };
        let mut quick = Renderer::new(scene.clone(), camera, small);
        if !quick.render_pass(cancel) {
            return;
        }
        publish(
            shared,
            Published {
                image: quick.image(false),
                samples: 0,
                target: settings.samples,
                preview: true,
                done: false,
                seconds: start.elapsed().as_secs_f64(),
            },
        );
    }
    let mut renderer = Renderer::new(scene.clone(), camera, settings);
    let mut last: Option<Instant> = None;
    let mut interval = Duration::from_millis(150);
    while !renderer.is_done() {
        if !renderer.render_pass(cancel) {
            return;
        }
        let done = renderer.is_done();
        if done || last.is_none_or(|l| l.elapsed() >= interval) {
            let t = Instant::now();
            let image = renderer.image(denoise);
            // Denoising a big picture takes a while: post less often than that.
            interval = Duration::from_millis(150).max(t.elapsed() * 3);
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            publish(
                shared,
                Published {
                    image,
                    samples: renderer.samples(),
                    target: settings.samples,
                    preview: false,
                    done,
                    seconds: start.elapsed().as_secs_f64(),
                },
            );
            last = Some(Instant::now());
        }
    }
}

/// What a render was started for: anything else, and it starts again.
#[derive(Debug, Clone, PartialEq)]
struct RunKey {
    scene_version: u64,
    camera: Camera,
    size: (usize, usize),
    settings: RenderSettings,
    focal: Option<f64>,
    denoise: bool,
}

/// The Render tab's state.
pub struct RenderView {
    /// Rendering is on while the tab is open (the Render button).
    pub enabled: bool,
    scene: Option<CachedScene>,
    job: Option<RenderJob>,
    running: Option<RunKey>,
    /// Wanted last frame: a render starts once the wish holds two frames in a row (the
    /// view is still).
    pending: Option<RunKey>,
    texture: Option<egui::TextureHandle>,
    /// The full-size picture shown, for Save image.
    latest: Option<Arc<RenderImage>>,
    samples: u32,
    target: u32,
    seconds: f64,
    done: bool,
    pub groups: Vec<SurfaceGroup>,
    error: Option<String>,
    /// The setup changed: save the preferences once the pointer is up.
    prefs_dirty: bool,
}

impl Default for RenderView {
    fn default() -> Self {
        Self {
            enabled: true,
            scene: None,
            job: None,
            running: None,
            pending: None,
            texture: None,
            latest: None,
            samples: 0,
            target: 0,
            seconds: 0.0,
            done: false,
            groups: Vec::new(),
            error: None,
            prefs_dirty: false,
        }
    }
}

impl RenderView {
    /// Stops the render and forgets its picture (the meshes stay, for coming back).
    pub fn stop(&mut self) {
        self.job = None;
        self.running = None;
        self.pending = None;
        self.texture = None;
        self.latest = None;
        self.samples = 0;
        self.done = false;
    }

    /// Forgets everything, the meshes too: another document.
    pub fn clear(&mut self) {
        self.stop();
        self.scene = None;
        self.groups.clear();
        self.error = None;
    }

    pub fn has_image(&self) -> bool {
        self.latest.is_some()
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(job) = self.job.as_ref() else {
            return;
        };
        let (latest, failed) = match job.shared.lock() {
            Ok(mut s) => (s.latest.take(), s.failed.take()),
            Err(_) => (None, Some("the render thread stopped".to_owned())),
        };
        if let Some(message) = failed {
            self.error = Some(format!("Render failed: {message}"));
            self.job = None;
            return;
        }
        let Some(p) = latest else {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        };
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [p.image.width, p.image.height],
            &p.image.rgba,
        );
        let options = egui::TextureOptions::LINEAR;
        match self.texture.as_mut() {
            Some(t) => t.set(image, options),
            None => self.texture = Some(ctx.load_texture("oxyde render", image, options)),
        }
        self.samples = p.samples;
        self.target = p.target;
        self.seconds = p.seconds;
        self.done = p.done;
        self.latest = (!p.preview).then(|| Arc::new(p.image));
        if p.done {
            self.job = None;
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

// ---- The application side ------------------------------------------------------------------

impl OxydeApp {
    /// Whether the Render tab is showing the render now.
    pub(crate) fn render_active(&self) -> bool {
        let tab = match self.asm {
            Some(_) => self.asm_tab == crate::assembly_ui::AsmTab::Render,
            None => self.tab == crate::app::CommandTab::Render,
        };
        tab && self.render.enabled
            && self.sketch.is_none()
            && self.section.is_none()
            && self.mesh_export.is_none()
            && !self.busy()
    }

    /// Every frame: stops the render when the tab is left, saves the setup once a drag
    /// is over.
    pub(crate) fn render_housekeeping(&mut self, ctx: &egui::Context) {
        if !self.render_active() && (self.render.job.is_some() || self.render.texture.is_some()) {
            self.render.stop();
        }
        if self.render.prefs_dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.render.prefs_dirty = false;
            self.save_preferences();
        }
    }

    /// The solids to render, in a fixed order.
    fn solid_items(&self) -> Vec<SolidItem<'_>> {
        let mut out = Vec::new();
        match self.asm.as_ref() {
            None => {
                let part = &self.doc.part;
                if let Some(body) = part.body() {
                    let key = finish_key(part, &self.doc.document_id);
                    let label = part
                        .material()
                        .map_or_else(|| self.doc.display_name(), |m| m.name.clone());
                    for solid in 0..body.solids.len() {
                        out.push(SolidItem {
                            part,
                            body,
                            solid,
                            own: None,
                            placement: None,
                            key: key.clone(),
                            label: label.clone(),
                            mesh_id: (self.doc.document_id.clone(), solid),
                        });
                    }
                }
            }
            Some(asm) => {
                for (o, placement, loaded) in asm.leaves(&asm.assembly) {
                    let Some(body) = loaded.part.body() else {
                        continue;
                    };
                    let key = finish_key(&loaded.part, &loaded.document_id);
                    let label = loaded.part.material().map_or_else(
                        || {
                            let name = asm.occurrence_name(&o);
                            name.rsplit(" / ").next().unwrap_or(&name).to_owned()
                        },
                        |m| m.name.clone(),
                    );
                    let own = asm.occurrence_appearance(&asm.assembly, &o);
                    for solid in 0..body.solids.len() {
                        out.push(SolidItem {
                            part: &loaded.part,
                            body,
                            solid,
                            own,
                            placement: Some(placement),
                            key: key.clone(),
                            label: label.clone(),
                            mesh_id: (loaded.document_id.clone(), solid),
                        });
                    }
                }
            }
        }
        out
    }

    fn looks_key(&self) -> LooksKey {
        let source = match self.asm.as_ref() {
            None => LooksSource::Part(
                self.doc.part.appearances().clone(),
                self.doc.part.material().cloned(),
            ),
            Some(asm) => LooksSource::Assembly(
                asm.assembly
                    .components
                    .iter()
                    .map(|c| c.appearance.clone())
                    .collect(),
            ),
        };
        LooksKey {
            source,
            finishes: self.prefs.render.finishes.clone(),
            default_color: self.prefs.graphics.face,
        }
    }

    /// Meshes the model again when it changed and rebuilds the scene when the model or
    /// its looks changed. The scene's version, or why there is none.
    fn ensure_render_scene(&mut self) -> Result<u64, String> {
        let geometry = GeometryKey {
            revision: self.document_revision(),
            assembly: self.asm.is_some(),
            detail: self.prefs.render.detail,
        };
        if self
            .render
            .scene
            .as_ref()
            .is_none_or(|s| s.geometry != geometry)
        {
            let meshes = self.render_meshes(geometry.detail)?;
            let version = self.render.scene.as_ref().map_or(1, |s| s.version + 1);
            self.render.scene = Some(CachedScene {
                geometry,
                meshes,
                looks: None,
                scene: None,
                version,
            });
        }
        let looks = self.looks_key();
        let stale = self
            .render
            .scene
            .as_ref()
            .is_none_or(|s| s.looks.as_ref() != Some(&looks) || s.scene.is_none());
        if stale {
            let meshes = self
                .render
                .scene
                .as_mut()
                .map(|s| std::mem::take(&mut s.meshes))
                .unwrap_or_default();
            let mut cache = std::mem::take(&mut self.texture_cache);
            let (scene, groups) = self.build_trace_scene(&meshes, &mut cache);
            self.texture_cache = cache;
            self.render.groups = groups;
            if let Some(s) = self.render.scene.as_mut() {
                s.meshes = meshes;
                s.looks = Some(looks);
                s.scene = Some(Arc::new(scene));
                s.version += 1;
            }
        }
        match self.render.scene.as_ref() {
            Some(s) if s.scene.as_ref().is_some_and(|t| !t.is_empty()) => Ok(s.version),
            _ => Err("Nothing to render: there is no solid yet".to_owned()),
        }
    }

    /// The model meshed for the render, one mesh per solid in [`OxydeApp::solid_items`]
    /// order, placed where it sits.
    fn render_meshes(&self, detail: Detail) -> Result<Vec<Mesh>, String> {
        let diagonal = self.model_diagonal();
        let (deflection, angle) = detail.quality().settings(diagonal).unwrap_or((0.02, 10.0));
        let angle = angle.to_radians();
        let mut cache: HashMap<(String, usize), Mesh> = HashMap::new();
        let mut out = Vec::new();
        for item in self.solid_items() {
            let Some(solid) = item.body.solids.get(item.solid) else {
                out.push(Mesh::default());
                continue;
            };
            let mesh = match cache.get(&item.mesh_id) {
                Some(m) => m.clone(),
                None => {
                    let m = self
                        .kernel
                        .tessellate_for_export(solid.shape, deflection, angle)
                        .map_err(|e| format!("Meshing for the render failed: {e}"))?;
                    cache.insert(item.mesh_id.clone(), m.clone());
                    m
                }
            };
            out.push(match &item.placement {
                Some(p) => oxyde_assembly::scene::placed(&mesh, p),
                None => mesh,
            });
        }
        Ok(out)
    }

    /// The path tracer's scene from the meshes (in [`OxydeApp::solid_items`] order) and
    /// the current looks, and the finish groups met.
    fn build_trace_scene(
        &self,
        meshes: &[Mesh],
        cache: &mut TextureCache,
    ) -> (TraceScene, Vec<SurfaceGroup>) {
        let prefs = &self.prefs.render;
        let default_color = self.prefs.graphics.face;
        let mut builder = SceneBuilder::new();
        let mut groups: Vec<SurfaceGroup> = Vec::new();
        for (item, mesh) in self.solid_items().iter().zip(meshes) {
            let Some(solid) = item.body.solids.get(item.solid) else {
                continue;
            };
            let material = item.part.material();
            if !groups.iter().any(|g| g.key == item.key) {
                groups.push(SurfaceGroup {
                    key: item.key.clone(),
                    label: item.label.clone(),
                    automatic: Finish::guess(material, material.map(|m| &m.appearance)),
                });
            }
            let looks: Vec<u32> = (0..mesh.faces.len() as u32)
                .map(|k| {
                    let appearance = item
                        .own
                        .or_else(|| item.part.face_appearance(item.body, solid.faces.start + k));
                    let face = FaceLook::new(prefs, &item.key, material, appearance, default_color);
                    let look = trace_look(&face, appearance, cache, &mut builder);
                    builder.add_look(look)
                })
                .collect();
            builder.add_mesh(mesh, &looks);
        }
        (builder.build(), groups)
    }

    /// The picture size for the viewport at `rect` (points).
    fn render_size(&self, rect: egui::Rect, pixels_per_point: f32) -> (usize, usize) {
        let p = &self.prefs.render;
        let (w, h) = if p.custom_size {
            (
                p.width.clamp(16, 8192) as usize,
                p.height.clamp(16, 8192) as usize,
            )
        } else {
            let k = pixels_per_point * p.resolution.clamp(0.1, 2.0);
            (
                ((rect.width() * k).round() as usize).max(16),
                ((rect.height() * k).round() as usize).max(16),
            )
        };
        // Memory: the renderer and the denoiser need about 0.1 KB per pixel.
        oxyde_render::pathtrace::fit_pixel_budget(w, h)
    }

    /// After the viewport is drawn: starts or restarts the render as needed and paints
    /// it over the viewport.
    pub(crate) fn render_over_viewport(&mut self, ui: &mut egui::Ui, output: &ViewportOutput) {
        if !self.render_active() {
            return;
        }
        let ctx = ui.ctx().clone();
        let version = match self.ensure_render_scene() {
            Ok(v) => {
                self.render.error = None;
                v
            }
            Err(e) => {
                self.render.stop();
                self.render.error = Some(e);
                self.paint_render_hud(ui, output.rect);
                return;
            }
        };
        let interacting = output.response.is_pointer_button_down_on()
            || output.response.dragged()
            || self.grabbing.is_some();
        let size = self.render_size(output.rect, output.pixels_per_point);
        let key = RunKey {
            scene_version: version,
            camera: self.viewport.camera(),
            size,
            settings: self.prefs.render.settings(size.0, size.1),
            focal: self.prefs.render.focal(),
            denoise: self.prefs.render.denoise,
        };
        if self.render.running.as_ref() != Some(&key) {
            // The view, the model or the setup moved on: the picture shown is wrong now.
            self.render.job = None;
            self.render.texture = None;
            self.render.latest = None;
            self.render.running = None;
            self.render.samples = 0;
            self.render.done = false;
            if !interacting && self.render.pending.as_ref() == Some(&key) {
                self.start_render(key);
            } else {
                self.render.pending = Some(key);
                ctx.request_repaint();
            }
        }
        self.render.poll(&ctx);
        self.paint_render(ui, output.rect);
    }

    fn start_render(&mut self, key: RunKey) {
        let Some(scene) = self.render.scene.as_ref().and_then(|s| s.scene.clone()) else {
            return;
        };
        let camera = RenderCamera::from_view(&key.camera, key.focal);
        self.render.job = Some(RenderJob::spawn(scene, camera, key.settings, key.denoise));
        self.render.target = key.settings.samples;
        self.render.seconds = 0.0;
        self.render.pending = None;
        self.render.running = Some(key);
    }

    /// The render over the viewport: fitted to it (letterboxed when its shape differs),
    /// on a checkerboard where it is transparent, with the progress below.
    fn paint_render(&self, ui: &mut egui::Ui, rect: egui::Rect) {
        let Some(texture) = self.render.texture.as_ref() else {
            self.paint_render_hud(ui, rect);
            return;
        };
        let painter = ui.painter_at(rect);
        let size = texture.size_vec2();
        let (custom, transparent) = (
            self.prefs.render.custom_size,
            self.prefs.render.background == BackgroundChoice::Transparent,
        );
        let image_rect = if custom && size.x > 0.0 && size.y > 0.0 {
            let k = (rect.width() / size.x).min(rect.height() / size.y);
            egui::Rect::from_center_size(rect.center(), size * k)
        } else {
            rect
        };
        if custom {
            painter.rect_filled(rect, 0.0, egui::Color32::from_gray(40));
        }
        if transparent {
            let cell = 12.0;
            painter.rect_filled(image_rect, 0.0, egui::Color32::from_gray(205));
            let (nx, ny) = (
                (image_rect.width() / cell).ceil() as i32,
                (image_rect.height() / cell).ceil() as i32,
            );
            for y in 0..ny {
                for x in 0..nx {
                    if (x + y) % 2 == 0 {
                        let min = image_rect.min + egui::vec2(x as f32 * cell, y as f32 * cell);
                        let r = egui::Rect::from_min_size(min, egui::vec2(cell, cell))
                            .intersect(image_rect);
                        painter.rect_filled(r, 0.0, egui::Color32::from_gray(160));
                    }
                }
            }
        }
        painter.image(
            texture.id(),
            image_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        self.paint_render_hud(ui, rect);
    }

    /// The progress (or the reason there is no render) in the corner of the viewport.
    fn paint_render_hud(&self, ui: &mut egui::Ui, rect: egui::Rect) {
        let r = &self.render;
        let text = if let Some(e) = &r.error {
            e.clone()
        } else if r.running.is_none() {
            "Render starts when the view is still".to_owned()
        } else if r.done {
            format!("Done: {} samples in {:.1} s", r.samples, r.seconds)
        } else if r.samples == 0 {
            "Rendering…".to_owned()
        } else {
            format!(
                "Rendering: {} / {} samples, {:.1} s",
                r.samples, r.target, r.seconds
            )
        };
        let painter = ui.painter_at(rect);
        let pos = rect.left_bottom() + egui::vec2(10.0, -10.0);
        let galley =
            painter.layout_no_wrap(text, egui::FontId::proportional(12.0), egui::Color32::WHITE);
        let bg = egui::Rect::from_min_size(
            pos - egui::vec2(6.0, galley.size().y + 4.0),
            galley.size() + egui::vec2(12.0, 8.0),
        );
        painter.rect_filled(bg, 4.0, egui::Color32::from_black_alpha(150));
        painter.galley(
            pos - egui::vec2(0.0, galley.size().y),
            galley,
            egui::Color32::WHITE,
        );
    }

    /// Render › Save image: the picture as it is now, as a PNG (with transparency when
    /// the background is transparent).
    pub(crate) fn save_render_image(&mut self) {
        let Some(image) = self.render.latest.clone() else {
            self.status = "Nothing to save yet: the render has not drawn a full pass".to_owned();
            return;
        };
        let Some(path) = self.ask_export_path("PNG image", &["png"]) else {
            return;
        };
        let result =
            image::RgbaImage::from_raw(image.width as u32, image.height as u32, image.rgba.clone())
                .ok_or_else(|| "the picture could not be assembled".to_owned())
                .and_then(|img| img.save(&path).map_err(|e| e.to_string()));
        self.status = match result {
            Ok(()) => format!(
                "Saved {} ({} × {}, {} samples)",
                path.display(),
                image.width,
                image.height,
                self.render.samples
            ),
            Err(e) => format!("Could not save the image: {e}"),
        };
    }

    /// The Render tab's buttons.
    pub(crate) fn render_tab(&mut self, ui: &mut egui::Ui) {
        use crate::icons::{Icon, command_button};
        let has_model = match &self.asm {
            Some(asm) => !asm.leaves(&asm.assembly).is_empty(),
            None => self.doc.part.body().is_some(),
        };
        if command_button(ui, Icon::Render, "Render", has_model, self.render.enabled)
            .on_hover_text(
                "Draw the model with real materials, light and shadows; turn off to see the \
                 viewport",
            )
            .clicked()
        {
            self.render.enabled = !self.render.enabled;
        }
        if command_button(
            ui,
            Icon::SaveImage,
            "Save Image",
            self.render.has_image(),
            false,
        )
        .on_hover_text("Save the render as it is now as a PNG")
        .clicked()
        {
            self.save_render_image();
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        if command_button(ui, Icon::ExportMesh, "glTF for Blender", has_model, false)
            .on_hover_text(
                "Export the model as glTF (.glb) with its colours and finishes, which \
                 Blender, three.js and Windows' 3D Viewer open",
            )
            .clicked()
        {
            self.begin_mesh_export(crate::mesh_export::MeshFormat::Gltf);
        }
    }

    /// The render setup, at the top of the left panel while the Render tab is open.
    pub(crate) fn render_panel(&mut self, ui: &mut egui::Ui) {
        let before = self.prefs.render.clone();
        let mut save_image = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.strong("Render");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(self.render.has_image(), egui::Button::new("Save image…"))
                        .clicked()
                    {
                        save_image = true;
                    }
                });
            });
            let r = &self.render;
            if r.target > 0 && (r.running.is_some() || r.done) {
                let fraction = r.samples as f32 / r.target.max(1) as f32;
                ui.add(
                    egui::ProgressBar::new(fraction.clamp(0.0, 1.0))
                        .text(format!("{} / {} samples", r.samples, r.target)),
                );
            }
            if !self.render.enabled {
                ui.label(
                    egui::RichText::new("Rendering is off: the Render button turns it on.")
                        .small()
                        .weak(),
                );
            }
            let p = &mut self.prefs.render;
            egui::CollapsingHeader::new("Scene")
                .default_open(true)
                .show(ui, |ui| {
                    egui::Grid::new("render_scene")
                        .num_columns(2)
                        .spacing([10.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Environment");
                            egui::ComboBox::from_id_salt("render_env")
                                .selected_text(p.environment.kind().label())
                                .show_ui(ui, |ui| {
                                    for e in EnvironmentChoice::ALL {
                                        ui.selectable_value(
                                            &mut p.environment,
                                            e,
                                            e.kind().label(),
                                        );
                                    }
                                });
                            ui.end_row();
                            ui.label("Brightness");
                            ui.add(egui::Slider::new(&mut p.strength, 0.1..=4.0).logarithmic(true));
                            ui.end_row();
                            ui.label("Rotation");
                            ui.add(egui::Slider::new(&mut p.rotation, -180.0..=180.0).suffix("°"));
                            ui.end_row();
                            ui.label("");
                            ui.checkbox(&mut p.lights_follow_view, "Lights follow the view")
                                .on_hover_text(
                                    "Keep the lights where they are relative to the camera, \
                                     so the model is lit from the front however it is turned",
                                );
                            ui.end_row();
                            ui.label("Background");
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt("render_bg")
                                    .selected_text(match p.background {
                                        BackgroundChoice::Environment => "Environment",
                                        BackgroundChoice::Color => "Colour",
                                        BackgroundChoice::Transparent => "Transparent",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut p.background,
                                            BackgroundChoice::Environment,
                                            "Environment",
                                        );
                                        ui.selectable_value(
                                            &mut p.background,
                                            BackgroundChoice::Color,
                                            "Colour",
                                        );
                                        ui.selectable_value(
                                            &mut p.background,
                                            BackgroundChoice::Transparent,
                                            "Transparent",
                                        );
                                    });
                                if p.background == BackgroundChoice::Color {
                                    ui.color_edit_button_srgb(&mut p.background_color);
                                }
                            });
                            ui.end_row();
                            ui.label("Floor");
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt("render_floor")
                                    .selected_text(match p.floor {
                                        FloorChoice::None => "None",
                                        FloorChoice::Shadow => "Shadows only",
                                        FloorChoice::Visible => "Visible",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut p.floor,
                                            FloorChoice::None,
                                            "None",
                                        );
                                        ui.selectable_value(
                                            &mut p.floor,
                                            FloorChoice::Shadow,
                                            "Shadows only",
                                        )
                                        .on_hover_text(
                                            "An invisible floor that shows the model's shadow \
                                             over the background",
                                        );
                                        ui.selectable_value(
                                            &mut p.floor,
                                            FloorChoice::Visible,
                                            "Visible",
                                        );
                                    });
                                if p.floor == FloorChoice::Visible {
                                    ui.color_edit_button_srgb(&mut p.floor_color);
                                }
                            });
                            ui.end_row();
                            if p.floor == FloorChoice::Visible {
                                ui.label("Floor gloss");
                                ui.add(egui::Slider::new(&mut p.floor_gloss, 0.0..=1.0));
                                ui.end_row();
                            }
                        });
                });
            egui::CollapsingHeader::new("Camera")
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut p.perspective, "Perspective");
                        ui.add_enabled(
                            p.perspective,
                            egui::DragValue::new(&mut p.focal_length)
                                .range(8.0..=1200.0)
                                .speed(0.5)
                                .suffix(" mm"),
                        )
                        .on_hover_text(
                            "Focal length (full-frame equivalent): short for a wide, dramatic \
                             view, long for a flat one. The orbit, pan and zoom are the \
                             viewport's.",
                        );
                    });
                });
            let groups = self.render.groups.clone();
            egui::CollapsingHeader::new("Surfaces")
                .default_open(true)
                .show(ui, |ui| {
                    if groups.is_empty() {
                        ui.label(egui::RichText::new("Nothing to render yet").weak());
                    }
                    egui::Grid::new("render_surfaces")
                        .num_columns(2)
                        .spacing([10.0, 4.0])
                        .show(ui, |ui| {
                            for g in &groups {
                                ui.label(&g.label)
                                    .on_hover_text(if g.key.starts_with("part:") {
                                        "A part without a material: its finish is kept for it alone"
                                    } else {
                                        "Every part of this material"
                                    });
                                let chosen = p.chosen(&g.key);
                                let shown = match chosen {
                                    Some(f) => f.label().to_owned(),
                                    None => format!("Automatic ({})", g.automatic.label()),
                                };
                                egui::ComboBox::from_id_salt(("render_finish", &g.key))
                                    .selected_text(shown)
                                    .width(170.0)
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_label(
                                                chosen.is_none(),
                                                format!("Automatic ({})", g.automatic.label()),
                                            )
                                            .clicked()
                                        {
                                            p.finishes.remove(&g.key);
                                        }
                                        for (f, label) in Finish::ALL {
                                            if ui
                                                .selectable_label(chosen == Some(f), label)
                                                .clicked()
                                            {
                                                p.finishes.insert(g.key.clone(), f.id().to_owned());
                                            }
                                        }
                                    });
                                ui.end_row();
                            }
                        });
                    ui.label(
                        egui::RichText::new(
                            "Automatic follows the material: metals brushed, plastics satin, \
                             see-through appearances glass.",
                        )
                        .small()
                        .weak(),
                    );
                });
            egui::CollapsingHeader::new("Output")
                .default_open(false)
                .show(ui, |ui| {
                    egui::Grid::new("render_output")
                        .num_columns(2)
                        .spacing([10.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Samples");
                            ui.add(
                                egui::DragValue::new(&mut p.samples)
                                    .range(1..=100_000)
                                    .speed(4.0),
                            )
                            .on_hover_text("Per pixel: more is smoother and slower");
                            ui.end_row();
                            ui.label("Bounces");
                            ui.add(egui::DragValue::new(&mut p.bounces).range(1..=64));
                            ui.end_row();
                            ui.label("Exposure");
                            ui.add(egui::Slider::new(&mut p.exposure, -4.0..=4.0).suffix(" EV"));
                            ui.end_row();
                            ui.label("");
                            ui.checkbox(&mut p.denoise, "Denoise")
                                .on_hover_text("Smooth the noise of the first passes");
                            ui.end_row();
                            ui.label("Size");
                            ui.horizontal(|ui| {
                                ui.radio_value(&mut p.custom_size, false, "Viewport");
                                ui.radio_value(&mut p.custom_size, true, "Fixed");
                            });
                            ui.end_row();
                            if p.custom_size {
                                ui.label("");
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::DragValue::new(&mut p.width)
                                            .range(16..=8192)
                                            .suffix(" px"),
                                    );
                                    ui.label("×");
                                    ui.add(
                                        egui::DragValue::new(&mut p.height)
                                            .range(16..=8192)
                                            .suffix(" px"),
                                    );
                                });
                                ui.end_row();
                                if (p.width as usize) * (p.height as usize)
                                    > oxyde_render::pathtrace::MAX_PIXELS
                                {
                                    ui.label("");
                                    ui.label(
                                        egui::RichText::new(
                                            "Above 8 megapixels: rendered smaller, to keep \
                                             memory under 1 GB",
                                        )
                                        .small()
                                        .weak(),
                                    );
                                    ui.end_row();
                                }
                            } else {
                                ui.label("Resolution");
                                egui::ComboBox::from_id_salt("render_res")
                                    .selected_text(format!("{:.0} %", p.resolution * 100.0))
                                    .show_ui(ui, |ui| {
                                        for r in [0.25f32, 0.5, 1.0, 2.0] {
                                            ui.selectable_value(
                                                &mut p.resolution,
                                                r,
                                                format!("{:.0} %", r * 100.0),
                                            );
                                        }
                                    });
                                ui.end_row();
                            }
                            ui.label("Mesh detail");
                            egui::ComboBox::from_id_salt("render_detail")
                                .selected_text(p.detail.label())
                                .show_ui(ui, |ui| {
                                    for (d, label) in Detail::ALL {
                                        ui.selectable_value(&mut p.detail, d, label);
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "How closely the triangles follow curved faces: finer is \
                                     smoother in close-ups and slower to prepare",
                                );
                            ui.end_row();
                        });
                });
        });
        ui.add_space(4.0);
        if self.prefs.render != before {
            self.render.prefs_dirty = true;
        }
        if save_image {
            self.save_render_image();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_round_trip_and_fill_in_defaults() {
        let mut p = RenderPrefs::default();
        p.finishes.insert("Steel".into(), "polished_metal".into());
        p.environment = EnvironmentChoice::Sky;
        let text = serde_json::to_string(&p).unwrap();
        let back: RenderPrefs = serde_json::from_str(&text).unwrap();
        assert_eq!(back, p);
        let partial: RenderPrefs = serde_json::from_str(r#"{ "samples": 12 }"#).unwrap();
        assert_eq!(partial.samples, 12);
        assert_eq!(partial.environment, EnvironmentChoice::Studio);
        assert_eq!(p.chosen("Steel"), Some(Finish::PolishedMetal));
        assert_eq!(p.chosen("Oak"), None);
    }

    #[test]
    fn settings_follow_the_preferences() {
        let mut p = RenderPrefs {
            background: BackgroundChoice::Color,
            background_color: [1, 2, 3],
            floor: FloorChoice::Visible,
            ..RenderPrefs::default()
        };
        let s = p.settings(320, 200);
        assert_eq!((s.width, s.height), (320, 200));
        assert_eq!(s.background, Background::Color([1, 2, 3]));
        assert!(matches!(s.floor, Floor::Visible { .. }));
        assert_eq!(p.focal(), Some(50.0));
        p.perspective = false;
        assert_eq!(p.focal(), None);
    }

    #[test]
    fn face_looks_take_the_choice_else_the_guess() {
        let mut p = RenderPrefs::default();
        let steel = Material::new("Steel", "Steel", Appearance::default(), 7850.0);
        let look = FaceLook::new(&p, "Steel", Some(&steel), None, [9, 9, 9]);
        assert_eq!(look.finish, Finish::BrushedMetal);
        assert_eq!(look.color, [9, 9, 9]);
        p.finishes.insert("Steel".into(), "glossy".into());
        let painted = Appearance::colored([200, 10, 10]);
        let look = FaceLook::new(&p, "Steel", Some(&steel), Some(&painted), [9, 9, 9]);
        assert_eq!(look.finish, Finish::Glossy);
        assert_eq!(look.color, [200, 10, 10]);
        let m = look.gltf();
        assert_eq!(m.metallic, 0.0);
        assert!(m.base_color[0] > 0.5 && m.base_color[1] < 0.01);
        // Glass is tinted lightly and stays opaque in glTF (transmission does the rest).
        let glass = FaceLook {
            color: [180, 220, 235],
            transparency: 0.7,
            finish: Finish::Glass,
        };
        let g = glass.gltf();
        assert_eq!(g.base_color[3], 1.0);
        assert_eq!(g.transmission, 1.0);
    }
}
