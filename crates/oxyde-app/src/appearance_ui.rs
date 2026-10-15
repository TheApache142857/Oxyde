//! Appearances in the application: the Appearance command (a PropertyManager panel, as
//! SolidWorks' Edit Appearance), decoding the texture images, and turning a document's
//! appearances into the viewport's materials.
//!
//! The panel edits one target at a time: the selected faces, a body, the whole part, or in
//! an assembly a component (which then overrides the part's own appearances). Its Material
//! page gives the part a material from the library (a look plus a density, see
//! `oxyde_model::Material`) and edits the library. Every change shows at once in the
//! viewport; OK keeps it as one undo step, Cancel puts things back.

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use oxyde_assembly::{Assembly, ComponentId};
use oxyde_model::{Appearance, BodyId, FaceName, FileData, Material, PartDefinition, Texture};
use oxyde_render::{Material as RenderMaterial, TextureRef};

use crate::app::{OxydeApp, Selected};
use crate::dialogs::Dialog;
use crate::property_manager::{PanelPick, section};

/// The largest a texture is kept at, per side: bigger images are shrunk.
const MAX_TEXTURE_SIZE: u32 = 1024;

/// Decoded texture images, by the bytes they came from (the same image used by several
/// appearances, or by several parts of an assembly, is decoded once).
#[derive(Default)]
pub struct TextureCache {
    /// Keyed by the address of the shared bytes, which stay alive while the entry holds
    /// a copy of the handle.
    entries: HashMap<usize, (FileData, TextureRef)>,
}

impl TextureCache {
    /// The decoded image, or `None` when the bytes are not an image Oxyde can read.
    pub fn get(&mut self, texture: &Texture) -> Option<TextureRef> {
        let key = texture.data.bytes().as_ptr() as usize;
        if let Some((_, decoded)) = self.entries.get(&key) {
            return Some(decoded.clone());
        }
        let decoded = Arc::new(decode(texture.data.bytes())?);
        self.entries
            .insert(key, (texture.data.clone(), decoded.clone()));
        Some(decoded)
    }
}

/// Reads a PNG or JPEG, shrinking very large images.
fn decode(bytes: &[u8]) -> Option<oxyde_render::Texture> {
    let image = image::load_from_memory(bytes).ok()?;
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return None;
    }
    let image = if w > MAX_TEXTURE_SIZE || h > MAX_TEXTURE_SIZE {
        let k = MAX_TEXTURE_SIZE as f64 / w.max(h) as f64;
        image.resize(
            ((w as f64 * k) as u32).max(1),
            ((h as f64 * k) as u32).max(1),
            image::imageops::FilterType::Triangle,
        )
    } else {
        image
    };
    let rgba = image.to_rgba8();
    Some(oxyde_render::Texture {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Whether `bytes` are an image Oxyde can use as a texture.
pub fn is_image(bytes: &[u8]) -> bool {
    image::guess_format(bytes).is_ok_and(|f| {
        matches!(f, image::ImageFormat::Png | image::ImageFormat::Jpeg)
    })
}

/// The material of an appearance; its texture is added to `textures` (once) when it
/// decodes.
pub fn material(
    appearance: &Appearance,
    cache: &mut TextureCache,
    textures: &mut Vec<TextureRef>,
) -> RenderMaterial {
    let mut m = RenderMaterial {
        color: Some(appearance.color),
        opacity: (1.0 - appearance.transparency as f32).clamp(0.0, 1.0),
        texture: None,
        texture_scale: Texture::DEFAULT_SCALE as f32,
    };
    if let Some(t) = &appearance.texture {
        if let Some(decoded) = cache.get(t) {
            let index = match textures.iter().position(|x| Arc::ptr_eq(x, &decoded)) {
                Some(i) => i,
                None => {
                    textures.push(decoded);
                    textures.len() - 1
                }
            };
            m.texture = Some(index);
            m.texture_scale = t.scale as f32;
        }
    }
    m
}

/// What an appearance is put on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The faces listed in the panel.
    Faces,
    Body(BodyId),
    Part,
    /// A top-level component of the assembly.
    Component(ComponentId),
}

/// Which page of the panel is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Appearance,
    Material,
}

pub struct AppearanceDialog {
    pub page: Page,
    pub scope: Scope,
    /// The faces for [`Scope::Faces`], picked in the viewport.
    pub faces: Vec<FaceName>,
    pub appearance: Appearance,
    /// Whether the appearance was changed in the panel: only then is it written to the
    /// target, so that choosing a material alone does not also paint the part.
    pub touched: bool,
    /// The part's material as edited on the Material page (`None`: no material).
    pub material: Option<Material>,
    /// The body the Body choice stands for: the one selected, or the one under the
    /// first selected face.
    pub body: Option<BodyId>,
    /// The part as it was when the panel opened: Cancel restores it, OK records it as
    /// the undo step.
    before: Option<PartDefinition>,
    /// Same for an assembly.
    before_asm: Option<Assembly>,
}

/// A named colour or material to click.
struct Preset {
    name: &'static str,
    color: [u8; 3],
    transparency: f64,
}

const fn preset(name: &'static str, color: [u8; 3], transparency: f64) -> Preset {
    Preset {
        name,
        color,
        transparency,
    }
}

const COLORS: [Preset; 12] = [
    preset("Red", [200, 40, 40], 0.0),
    preset("Orange", [235, 130, 30], 0.0),
    preset("Yellow", [240, 200, 40], 0.0),
    preset("Green", [60, 160, 70], 0.0),
    preset("Teal", [40, 160, 160], 0.0),
    preset("Blue", [50, 100, 210], 0.0),
    preset("Purple", [130, 70, 180], 0.0),
    preset("Pink", [225, 110, 160], 0.0),
    preset("White", [240, 240, 240], 0.0),
    preset("Light grey", [190, 190, 190], 0.0),
    preset("Dark grey", [90, 90, 90], 0.0),
    preset("Black", [30, 30, 30], 0.0),
];

const LOOKS: [Preset; 12] = [
    preset("Steel", [150, 152, 158], 0.0),
    preset("Aluminium", [200, 203, 208], 0.0),
    preset("Brass", [205, 170, 80], 0.0),
    preset("Copper", [190, 110, 80], 0.0),
    preset("Gold", [230, 190, 70], 0.0),
    preset("Black plastic", [40, 40, 42], 0.0),
    preset("White plastic", [235, 235, 230], 0.0),
    preset("Rubber", [50, 50, 50], 0.0),
    preset("Wood", [170, 120, 70], 0.0),
    preset("PCB", [30, 110, 60], 0.0),
    preset("Glass", [180, 220, 235], 0.7),
    preset("Smoked glass", [60, 70, 80], 0.5),
];

fn swatch(ui: &mut egui::Ui, p: &Preset) -> bool {
    let hover = if p.transparency > 0.0 {
        format!("{} ({:.0}% transparent)", p.name, p.transparency * 100.0)
    } else {
        p.name.to_owned()
    };
    color_swatch(ui, p.color, 20.0).on_hover_text(hover).clicked()
}

/// A small square of colour.
fn color_swatch(ui: &mut egui::Ui, color: [u8; 3], size: f32) -> egui::Response {
    let [r, g, b] = color;
    let button = egui::Button::new("")
        .fill(Color32::from_rgb(r, g, b))
        .min_size(egui::vec2(size, size))
        .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color));
    ui.add(button)
}

/// The colour, preset, transparency and texture controls, shared by both pages.
fn appearance_controls(ui: &mut egui::Ui, a: &mut Appearance, status: &mut String) {
    section(ui, "Colour");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
        for p in &COLORS {
            if swatch(ui, p) {
                a.color = p.color;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.color_edit_button_srgb(&mut a.color);
        ui.label(RichText::new("Custom").small());
    });

    section(ui, "Presets");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
        for p in &LOOKS {
            if swatch(ui, p) {
                a.color = p.color;
                a.transparency = p.transparency;
            }
        }
    });

    section(ui, "Transparency");
    let mut percent = (a.transparency * 100.0).round();
    ui.add(egui::Slider::new(&mut percent, 0.0..=95.0).suffix("%"));
    a.transparency = (percent / 100.0).clamp(0.0, 1.0);

    section(ui, "Texture");
    ui.horizontal(|ui| {
        if ui
            .button("Image…")
            .on_hover_text("A PNG or JPEG repeated over the surface")
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg"])
                .pick_file()
            {
                match std::fs::read(&path) {
                    Ok(bytes) if is_image(&bytes) => {
                        let scale = a.texture.as_ref().map_or(Texture::DEFAULT_SCALE, |t| t.scale);
                        a.texture = Some(Texture {
                            file_name: path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            data: FileData::new(bytes),
                            scale,
                        });
                    }
                    Ok(_) => *status = "That file is not a PNG or JPEG image".to_owned(),
                    Err(e) => *status = format!("Could not read the image: {e}"),
                }
            }
        }
        match &a.texture {
            Some(t) => {
                ui.label(&t.file_name);
                if ui.small_button("✕").on_hover_text("Remove the texture").clicked() {
                    a.texture = None;
                }
            }
            None => {
                ui.label(RichText::new("None").weak());
            }
        }
    });
    if let Some(t) = a.texture.as_mut() {
        ui.horizontal(|ui| {
            ui.label("Size");
            ui.add(
                egui::Slider::new(&mut t.scale, 1.0..=1000.0)
                    .logarithmic(true)
                    .suffix(" mm"),
            )
            .on_hover_text("Millimetres covered by one repeat of the image");
        });
        ui.label(
            RichText::new("Mapped along the model's axes, so it needs no unwrapping.")
                .small()
                .weak(),
        );
    }
}

impl OxydeApp {
    // ---- Opening -----------------------------------------------------------------------------

    /// Appearance from the command bar or the viewport's menu: the selected faces, else
    /// the selected body, else the whole part.
    pub(crate) fn begin_appearance(&mut self) {
        if self.doc.part.body().is_none() {
            self.status = "There is nothing to colour yet".to_owned();
            return;
        }
        let faces: Vec<FaceName> = self
            .selection
            .iter()
            .filter_map(|s| match s {
                Selected::Face(f) => Some(f.clone()),
                Selected::Edge(_) => None,
            })
            .collect();
        let scope = if !faces.is_empty() {
            Scope::Faces
        } else if let Some(body) = self.selected_body {
            Scope::Body(body)
        } else {
            Scope::Part
        };
        self.open_appearance(scope, faces, Page::Appearance);
    }

    /// The Material page of the panel: what the part is made of.
    pub(crate) fn begin_material(&mut self) {
        if self.doc.part.body().is_none() {
            self.status = "There is nothing to give a material to yet".to_owned();
            return;
        }
        self.open_appearance(Scope::Part, Vec::new(), Page::Material);
    }

    /// Appearance from the Solid Bodies folder.
    pub(crate) fn begin_body_appearance(&mut self, id: BodyId) {
        self.open_appearance(Scope::Body(id), Vec::new(), Page::Appearance);
    }

    /// Appearance of a component, from the assembly tree or the viewport.
    pub(crate) fn begin_component_appearance(&mut self, id: ComponentId) {
        self.open_appearance(Scope::Component(id), Vec::new(), Page::Appearance);
    }

    fn open_appearance(&mut self, scope: Scope, faces: Vec<FaceName>, page: Page) {
        let body = match scope {
            Scope::Body(id) => Some(id),
            _ => self
                .selected_body
                .or_else(|| self.body_of_faces(&faces))
                .or_else(|| {
                    let body = self.doc.part.body()?;
                    (body.solids.len() == 1).then(|| body.solids[0].id)
                }),
        };
        let appearance = self.current_appearance(scope, &faces);
        let (before, before_asm) = match scope {
            Scope::Component(_) => (None, self.asm.as_ref().map(|d| d.assembly.clone())),
            _ => (Some(self.doc.part.definition()), None),
        };
        let material = match scope {
            Scope::Component(_) => None,
            _ => self.doc.part.material().cloned(),
        };
        self.dialog = Some(Dialog::Appearance(AppearanceDialog {
            page,
            scope,
            faces,
            appearance,
            touched: false,
            material,
            body,
            before,
            before_asm,
        }));
    }

    fn body_of_faces(&self, faces: &[FaceName]) -> Option<BodyId> {
        let body = self.doc.part.body()?;
        let index = body.naming.find_face(faces.first()?)?;
        body.solid_of_face(index).map(|s| s.id)
    }

    /// The appearance the target has now, to start from: for faces, the first face's own
    /// (or what it inherits); the viewport's part colour when there is none.
    fn current_appearance(&self, scope: Scope, faces: &[FaceName]) -> Appearance {
        let fallback = Appearance::colored(self.viewport.settings.style.face);
        let a = self.doc.part.appearances();
        let found = match scope {
            Scope::Faces => faces
                .first()
                .and_then(|f| {
                    let body = self.body_of_faces(faces);
                    a.of_face(Some(f), body)
                })
                .or(self.doc.part.material().map(|m| &m.appearance)),
            Scope::Body(id) => a
                .body(id)
                .or(a.part.as_ref())
                .or(self.doc.part.material().map(|m| &m.appearance)),
            Scope::Part => a
                .part
                .as_ref()
                .or(self.doc.part.material().map(|m| &m.appearance)),
            Scope::Component(id) => self
                .asm
                .as_ref()
                .and_then(|d| d.assembly.component(id))
                .and_then(|c| c.appearance.as_ref()),
        };
        found.cloned().unwrap_or(fallback)
    }

    // ---- The panel ---------------------------------------------------------------------------

    /// Title and whether the target is set.
    pub(crate) fn appearance_header(&self, d: &AppearanceDialog) -> (&'static str, bool) {
        let ready = match d.scope {
            Scope::Faces => !d.faces.is_empty(),
            _ => true,
        };
        let title = match d.page {
            Page::Appearance => "Appearance",
            Page::Material => "Material",
        };
        (title, ready)
    }

    /// The panel's contents; returns true when the panel finished by itself (the
    /// appearance was removed).
    pub(crate) fn appearance_body(&mut self, ui: &mut egui::Ui, d: &mut AppearanceDialog) -> bool {
        if !matches!(d.scope, Scope::Component(_)) {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut d.page, Page::Appearance, "Appearance");
                ui.selectable_value(&mut d.page, Page::Material, "Material");
            });
        }
        match d.page {
            Page::Appearance => self.appearance_page(ui, d),
            Page::Material => {
                self.material_page(ui, d);
                false
            }
        }
    }

    /// The Appearance page: the target and its look.
    fn appearance_page(&mut self, ui: &mut egui::Ui, d: &mut AppearanceDialog) -> bool {
        let before = (d.scope, d.faces.len(), d.appearance.clone());
        let mut finished = false;

        section(ui, "Apply to");
        match d.scope {
            Scope::Component(id) => {
                let name = self
                    .asm
                    .as_ref()
                    .and_then(|a| a.assembly.component(id))
                    .map_or_else(String::new, |c| c.name.clone());
                ui.label(format!("Component {name}"));
                ui.label(
                    RichText::new("Overrides the part's own appearances in this assembly.")
                        .small()
                        .weak(),
                );
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut d.scope, Scope::Faces, "Faces");
                    if let Some(body) = d.body {
                        let label = if self.body_count() > 1 {
                            self.body_label(body)
                        } else {
                            "Body".to_owned()
                        };
                        ui.radio_value(&mut d.scope, Scope::Body(body), label);
                    }
                    ui.radio_value(&mut d.scope, Scope::Part, "Part");
                });
                if d.scope == Scope::Faces {
                    let items: Vec<(String, String)> = d
                        .faces
                        .iter()
                        .enumerate()
                        .map(|(i, f)| (format!("Face {}", i + 1), f.to_string()))
                        .collect();
                    if let Some(i) = crate::property_manager::selection_box(
                        ui,
                        &items,
                        "Click faces in the graphics area",
                    ) {
                        d.faces.remove(i);
                    }
                }
            }
        }

        appearance_controls(ui, &mut d.appearance, &mut self.status);

        ui.add_space(8.0);
        let has_own = self.has_own_appearance(d);
        if ui
            .add_enabled(has_own, egui::Button::new("Remove appearance"))
            .on_hover_text("Back to what it inherits: the body's, the part's, or the viewport's colour")
            .clicked()
        {
            self.write_appearance(d, None);
            self.commit_appearance(d);
            self.refresh_materials();
            finished = true;
        }

        if !finished && (d.scope, d.faces.len(), d.appearance.clone()) != before {
            if d.appearance != before.2 {
                d.touched = true;
            }
            self.preview_appearance(d);
        }
        finished
    }

    /// The Material page: the part's material, the library to pick from, and the editor
    /// that saves to the library.
    fn material_page(&mut self, ui: &mut egui::Ui, d: &mut AppearanceDialog) {
        let before = d.material.clone();

        section(ui, "Part material");
        match &d.material {
            Some(m) => {
                ui.horizontal(|ui| {
                    color_swatch(ui, m.appearance.color, 16.0);
                    ui.label(RichText::new(&m.name).strong());
                });
                ui.label(
                    RichText::new(format!("{} · {:.0} kg/m³", m.category, m.density))
                        .small()
                        .weak(),
                );
            }
            None => {
                ui.label(RichText::new("None").weak());
                ui.label(
                    RichText::new("Pick one below: it gives the part its look and its mass.")
                        .small()
                        .weak(),
                );
            }
        }
        if ui
            .add_enabled(d.material.is_some(), egui::Button::new("Remove material"))
            .clicked()
        {
            d.material = None;
        }

        section(ui, "Library");
        let selected = d.material.as_ref().map(|m| m.name.clone());
        let mut pick: Option<Material> = None;
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let library = &self.materials;
                for category in library.categories() {
                    ui.label(RichText::new(category).small().weak());
                    for m in library.materials().iter().filter(|m| m.category == category) {
                        ui.horizontal(|ui| {
                            color_swatch(ui, m.appearance.color, 14.0);
                            let is_selected = selected.as_deref() == Some(m.name.as_str());
                            let response = ui
                                .selectable_label(is_selected, &m.name)
                                .on_hover_text(format!("{:.0} kg/m³", m.density));
                            if response.clicked() {
                                pick = Some(m.clone());
                            }
                        });
                    }
                }
                if library.is_empty() {
                    ui.label(RichText::new("The library is empty.").weak());
                }
            });
        if let Some(m) = pick {
            d.material = Some(m);
        }

        if let Some(m) = d.material.as_mut() {
            section(ui, "Edit");
            egui::Grid::new("material_edit")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut m.name);
                    ui.end_row();
                    ui.label("Category");
                    ui.text_edit_singleline(&mut m.category);
                    ui.end_row();
                    ui.label("Density");
                    ui.add(
                        egui::DragValue::new(&mut m.density)
                            .range(0.0..=30000.0)
                            .speed(10.0)
                            .suffix(" kg/m³"),
                    );
                    ui.end_row();
                });
            appearance_controls(ui, &mut m.appearance, &mut self.status);
            ui.add_space(8.0);
            let valid = Material::is_valid_name(&m.name);
            let in_library = self.materials.get(&m.name).is_some();
            ui.horizontal(|ui| {
                let save = if in_library {
                    "Update in library"
                } else {
                    "Save to library"
                };
                if ui
                    .add_enabled(valid, egui::Button::new(save))
                    .on_hover_text("Keeps this material for every part, in materials.json")
                    .clicked()
                {
                    self.materials.insert(m.clone());
                    self.status = match crate::materials::save(&self.materials) {
                        Ok(path) => format!("Saved {} to {}", m.name, path.display()),
                        Err(e) => format!("Could not save the material library: {e}"),
                    };
                }
                if ui
                    .add_enabled(in_library, egui::Button::new("Delete from library"))
                    .clicked()
                {
                    self.materials.remove(&m.name);
                    self.status = match crate::materials::save(&self.materials) {
                        Ok(_) => format!("{} removed from the library", m.name),
                        Err(e) => format!("Could not save the material library: {e}"),
                    };
                }
            });
            ui.label(
                RichText::new(
                    "The part keeps its own copy; appearances of faces and bodies stay on top.",
                )
                .small()
                .weak(),
            );
        }

        if d.material != before {
            self.preview_appearance(d);
        }
    }

    /// A face, body or component clicked while the panel is open.
    pub(crate) fn appearance_pick(&mut self, d: &mut AppearanceDialog, pick: PanelPick) -> bool {
        match pick {
            PanelPick::Face(f) => {
                if matches!(d.scope, Scope::Component(_)) {
                    return false;
                }
                match d.faces.iter().position(|x| *x == f) {
                    Some(i) => {
                        d.faces.remove(i);
                    }
                    None => d.faces.push(f),
                }
                d.scope = Scope::Faces;
                if d.body.is_none() {
                    d.body = self.body_of_faces(&d.faces);
                }
                self.preview_appearance(d);
                true
            }
            PanelPick::Body(id) => {
                d.body = Some(id);
                d.scope = Scope::Body(id);
                self.preview_appearance(d);
                true
            }
            _ => false,
        }
    }

    /// Whether the target has an appearance of its own (not inherited).
    fn has_own_appearance(&self, d: &AppearanceDialog) -> bool {
        let a = self.doc.part.appearances();
        match d.scope {
            Scope::Faces => d.faces.iter().any(|f| a.face(f).is_some()),
            Scope::Body(id) => a.body(id).is_some(),
            Scope::Part => a.part.is_some(),
            Scope::Component(id) => self
                .asm
                .as_ref()
                .and_then(|x| x.assembly.component(id))
                .is_some_and(|c| c.appearance.is_some()),
        }
    }

    /// Puts `appearance` on the dialog's target (or takes it off with `None`).
    fn write_appearance(&mut self, d: &AppearanceDialog, appearance: Option<Appearance>) {
        match d.scope {
            Scope::Faces => {
                let a = self.doc.part.appearances_mut();
                for f in &d.faces {
                    a.set_face(f, appearance.clone());
                }
            }
            Scope::Body(id) => self.doc.part.appearances_mut().set_body(id, appearance),
            Scope::Part => self.doc.part.appearances_mut().set_part(appearance),
            Scope::Component(id) => {
                if let Some(c) = self
                    .asm
                    .as_mut()
                    .and_then(|x| x.assembly.component_mut(id))
                {
                    c.appearance = appearance.map(Appearance::clamped);
                }
            }
        }
    }

    /// Shows the panel's current values in the viewport, starting from the state the
    /// panel opened with (so moving a face out of the list takes its colour back).
    fn preview_appearance(&mut self, d: &AppearanceDialog) {
        self.restore_appearance(d);
        self.write_panel(d);
        self.refresh_materials();
    }

    /// Puts the panel's values on the part: the appearance when it was changed (or the
    /// target had one already), and the material.
    fn write_panel(&mut self, d: &AppearanceDialog) {
        if d.touched || self.has_own_appearance(d) {
            self.write_appearance(d, Some(d.appearance.clone()));
        }
        if !matches!(d.scope, Scope::Component(_)) {
            self.doc.part.set_material(d.material.clone());
        }
    }

    fn restore_appearance(&mut self, d: &AppearanceDialog) {
        if let Some(before) = &d.before {
            *self.doc.part.appearances_mut() = before.appearances.clone();
            self.doc.part.set_material(before.material.clone());
        }
        if let (Some(before), Some(doc)) = (&d.before_asm, self.asm.as_mut()) {
            for c in &mut doc.assembly.components {
                if let Some(b) = before.component(c.id) {
                    c.appearance = b.appearance.clone();
                }
            }
        }
    }

    /// Records the change as one undo step.
    fn commit_appearance(&mut self, d: &AppearanceDialog) {
        match d.scope {
            Scope::Component(_) => {
                if let (Some(before), Some(doc)) = (&d.before_asm, self.asm.as_mut()) {
                    if *before != doc.assembly {
                        doc.checkpoint_from(before.clone());
                    }
                }
            }
            _ => {
                if let Some(before) = &d.before {
                    let part = &self.doc.part;
                    if before.appearances != *part.appearances()
                        || before.material.as_ref() != part.material()
                    {
                        self.doc.checkpoint_from(before.clone());
                    }
                }
            }
        }
    }

    /// OK: keep what is shown.
    pub(crate) fn apply_appearance(&mut self, d: AppearanceDialog) {
        if d.scope == Scope::Faces && d.faces.is_empty() {
            self.restore_appearance(&d);
            self.refresh_materials();
            return;
        }
        self.restore_appearance(&d);
        self.write_panel(&d);
        self.commit_appearance(&d);
        self.sync_material_property();
        self.refresh_materials();
        self.status = match (&d.material, d.page) {
            (Some(m), Page::Material) => format!("Material: {}", m.name),
            _ => "Appearance set".to_owned(),
        };
    }

    /// Keeps `properties.material` (what the bill of materials shows) equal to the
    /// material's name.
    pub(crate) fn sync_material_property(&mut self) {
        if let Some(m) = self.doc.part.material() {
            if self.doc.properties.material != m.name {
                self.doc.properties.material = m.name.clone();
                self.doc.modified = true;
            }
        }
    }

    /// Cancel: back to how it was.
    pub(crate) fn cancel_appearance(&mut self, d: AppearanceDialog) {
        self.restore_appearance(&d);
        self.refresh_materials();
    }

    // ---- Materials for the viewport ------------------------------------------------------

    /// The materials of the part's faces, in face order; empty when the part has no
    /// appearances.
    pub(crate) fn part_materials(&mut self) -> (Vec<RenderMaterial>, Vec<TextureRef>) {
        let part = &self.doc.part;
        let Some(body) = part.body() else {
            return (Vec::new(), Vec::new());
        };
        if !part.has_appearances() {
            return (Vec::new(), Vec::new());
        }
        let cache = &mut self.texture_cache;
        let mut textures = Vec::new();
        let materials = (0..body.topology.faces.len() as u32)
            .map(|i| match part.face_appearance(body, i) {
                Some(a) => material(a, cache, &mut textures),
                None => RenderMaterial::default(),
            })
            .collect();
        (materials, textures)
    }

    /// The materials of the assembly's combined mesh (from the last `scene_mesh`): a
    /// component's own appearance, else the part's.
    pub(crate) fn assembly_materials(
        &mut self,
        assembly: &Assembly,
    ) -> (Vec<RenderMaterial>, Vec<TextureRef>) {
        let Some(doc) = self.asm.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        let cache = &mut self.texture_cache;
        let total = doc.ranges.iter().map(|r| r.faces.end).max().unwrap_or(0) as usize;
        let mut materials = vec![RenderMaterial::default(); total];
        let mut textures = Vec::new();
        for r in &doc.ranges {
            let own = assembly
                .component(r.owner.top)
                .and_then(|c| c.appearance.as_ref());
            let part = doc.part_at(&r.owner).map(|p| &p.part);
            let body = part.and_then(|p| p.body());
            if own.is_none() && part.is_none_or(|p| !p.has_appearances()) {
                continue;
            }
            for (local, index) in r.faces.clone().enumerate() {
                let appearance = own.or_else(|| {
                    part.zip(body)
                        .and_then(|(p, b)| p.face_appearance(b, local as u32))
                });
                if let Some(a) = appearance {
                    materials[index as usize] = material(a, cache, &mut textures);
                }
            }
        }
        (materials, textures)
    }

    /// Gives the viewport the current materials without re-meshing.
    pub(crate) fn refresh_materials(&mut self) {
        let (materials, textures) = if self.asm.is_some() {
            let assembly = match &self.mate_preview {
                Some(p) => p.assembly.clone(),
                None => self.asm.as_ref().map(|d| d.assembly.clone()).unwrap_or_default(),
            };
            self.assembly_materials(&assembly)
        } else {
            self.part_materials()
        };
        self.viewport.set_materials(materials, textures);
    }
}
