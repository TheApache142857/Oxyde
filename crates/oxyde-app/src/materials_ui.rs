//! The Materials window (Tools › Materials…, the *Materials* button of an assembly): the
//! material library edited in one place, and the materials of the open documents.
//!
//! A material is a look plus a density; a part made of it is drawn with the material's
//! appearance. Editing a material here changes every open part made of it: the open part,
//! or in an assembly every part file whose material has that name (the files are
//! rewritten, as parts are documents of their own). Applying a material to a part that
//! has appearances of its own asks first, since the material's look replaces them
//! ([`Confirm::Material`]).

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use oxyde_model::Material;

use crate::app::OxydeApp;
use crate::appearance_ui::{appearance_controls, color_swatch};
use crate::assembly::Loaded;
use crate::dialogs::Dialog;
use crate::property_manager::section;

#[derive(Default)]
pub struct MaterialsWindow {
    pub open: bool,
    /// The material being edited, and the name it had when picked (the parts made of
    /// it are found by that name, so renaming carries them along).
    draft: Option<(Material, String)>,
    status: String,
}

/// A question asked before something is done, shown over everything.
#[derive(Debug, Clone, PartialEq)]
pub enum Confirm {
    /// Give these part files (empty: the open part) the library material `name`, removing
    /// their own appearances, which the material's look replaces.
    Material {
        paths: Vec<PathBuf>,
        name: String,
        /// How many of them have appearances of their own.
        with_appearances: usize,
    },
}

/// Where a material in use was found, for the list.
struct InUse {
    material: Material,
    /// Part files made of it (an assembly), or 1 for the open part.
    count: usize,
    in_library: bool,
}

impl OxydeApp {
    pub(crate) fn open_materials_window(&mut self) {
        self.materials_window.open = true;
    }

    /// The materials the open documents use, by name: the open part's, or those of every
    /// part file loaded in the assembly.
    fn materials_in_use(&self) -> Vec<InUse> {
        let mut out: Vec<InUse> = Vec::new();
        let mut add = |m: &Material| match out.iter_mut().find(|u| u.material.name == m.name) {
            Some(u) => u.count += 1,
            None => out.push(InUse {
                material: m.clone(),
                count: 1,
                in_library: self.materials.get(&m.name).is_some(),
            }),
        };
        match &self.asm {
            Some(doc) => {
                for loaded in doc.docs.values() {
                    if let Ok(Loaded::Part(p)) = loaded {
                        if let Some(m) = p.part.material() {
                            add(m);
                        }
                    }
                }
            }
            None => {
                if let Some(m) = self.doc.part.material() {
                    add(m);
                }
            }
        }
        out.sort_by(|a, b| a.material.name.cmp(&b.material.name));
        out
    }

    /// The part files made of `name` (an assembly), or an empty list for the open part.
    fn paths_made_of(&self, name: &str) -> Vec<PathBuf> {
        let Some(doc) = self.asm.as_ref() else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = doc
            .docs
            .iter()
            .filter(|(_, loaded)| {
                matches!(loaded, Ok(Loaded::Part(p)) if p.part.material().is_some_and(|m| m.name == name))
            })
            .map(|(path, _)| path.clone())
            .collect();
        paths.sort();
        paths
    }

    /// Writes `material` into every part made of `source` (by name): the open part, or the
    /// part files of the assembly. Their own appearances stay, as they are on top of the
    /// material's look anyway.
    fn propagate_material(&mut self, source: &str, material: &Material) -> usize {
        if self.asm.is_some() {
            let paths = self.paths_made_of(source);
            let count = paths.len();
            self.write_material_to_paths(&paths, Some(material.clone()), false);
            count
        } else if self.doc.part.material().is_some_and(|m| m.name == source) {
            let before = self.doc.part.definition();
            self.doc.part.set_material(Some(material.clone()));
            self.doc.checkpoint_from(before);
            self.sync_material_property();
            self.doc.revision = crate::document::next_revision();
            self.refresh_materials();
            1
        } else {
            0
        }
    }

    pub(crate) fn materials_window(&mut self, ctx: &egui::Context) {
        if !self.materials_window.open {
            return;
        }
        let mut open = true;
        let in_use = self.materials_in_use();
        // What the list click chose, applied after drawing.
        let mut pick: Option<(Material, String)> = None;
        let mut save = false;
        let mut delete = false;
        let mut apply = false;
        let mut update_parts = false;
        let mut new_material = false;
        let mut duplicate = false;
        let target = match &self.asm {
            Some(doc) => {
                let n = doc
                    .selection
                    .iter()
                    .filter_map(crate::assembly::AsmSelected::component)
                    .count();
                if n == 0 {
                    None
                } else {
                    Some(format!("Apply to {n} selected component(s)"))
                }
            }
            None => self.doc.part.body().map(|_| "Apply to the part".to_owned()),
        };
        egui::Window::new("Materials")
            .open(&mut open)
            .resizable(true)
            .default_width(640.0)
            .default_height(520.0)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    // ---- The list ----
                    ui.vertical(|ui| {
                        ui.set_width(230.0);
                        ui.horizontal(|ui| {
                            if ui.button("New").on_hover_text("A material to fill in").clicked() {
                                new_material = true;
                            }
                            if ui
                                .add_enabled(self.materials_window.draft.is_some(), egui::Button::new("Duplicate"))
                                .clicked()
                            {
                                duplicate = true;
                            }
                        });
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let current = self
                                    .materials_window
                                    .draft
                                    .as_ref()
                                    .map(|(_, source)| source.clone());
                                if !in_use.is_empty() {
                                    section(
                                        ui,
                                        if self.asm.is_some() {
                                            "In this assembly"
                                        } else {
                                            "In this part"
                                        },
                                    );
                                    for u in &in_use {
                                        ui.horizontal(|ui| {
                                            color_swatch(ui, u.material.appearance.color, 14.0);
                                            let selected =
                                                current.as_deref() == Some(u.material.name.as_str());
                                            let label = if self.asm.is_some() {
                                                format!("{} ({})", u.material.name, u.count)
                                            } else {
                                                u.material.name.clone()
                                            };
                                            let hover = if u.in_library {
                                                format!("{:.0} kg/m³", u.material.density)
                                            } else {
                                                format!(
                                                    "{:.0} kg/m³ · not in the library",
                                                    u.material.density
                                                )
                                            };
                                            let text = if u.in_library {
                                                RichText::new(label)
                                            } else {
                                                RichText::new(label).italics()
                                            };
                                            if ui.selectable_label(selected, text).on_hover_text(hover).clicked() {
                                                pick = Some((u.material.clone(), u.material.name.clone()));
                                            }
                                        });
                                    }
                                }
                                section(ui, "Library");
                                let library = &self.materials;
                                for category in library.categories() {
                                    ui.label(RichText::new(category).small().weak());
                                    for m in library.materials().iter().filter(|m| m.category == category) {
                                        ui.horizontal(|ui| {
                                            color_swatch(ui, m.appearance.color, 14.0);
                                            let selected = current.as_deref() == Some(m.name.as_str());
                                            let used = in_use.iter().find(|u| u.material.name == m.name);
                                            let hover = match used {
                                                Some(u) if self.asm.is_some() => format!(
                                                    "{:.0} kg/m³ · {} part file(s) made of it",
                                                    m.density, u.count
                                                ),
                                                Some(_) => format!("{:.0} kg/m³ · the part's material", m.density),
                                                None => format!("{:.0} kg/m³", m.density),
                                            };
                                            if ui.selectable_label(selected, &m.name).on_hover_text(hover).clicked() {
                                                pick = Some((m.clone(), m.name.clone()));
                                            }
                                        });
                                    }
                                }
                                if library.is_empty() {
                                    ui.label(RichText::new("The library is empty.").weak());
                                }
                            });
                    });
                    ui.separator();
                    // ---- The editor ----
                    ui.vertical(|ui| {
                        ui.set_min_width(340.0);
                        let Some((m, source)) = self.materials_window.draft.as_mut() else {
                            ui.add_space(12.0);
                            ui.label(
                                RichText::new(
                                    "Pick a material to edit it, or New. A part made of a \
                                     material is drawn with its look and weighs by its density.",
                                )
                                .weak(),
                            );
                            return;
                        };
                        let in_library = self.materials.get(source).is_some();
                        let made_of = in_use.iter().find(|u| &u.material.name == source).map(|u| u.count);
                        egui::ScrollArea::vertical()
                            .id_salt("material_editor")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                section(ui, "Material");
                                egui::Grid::new("materials_window_edit")
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
                                appearance_controls(ui, &mut m.appearance, &mut self.materials_window.status);
                                ui.add_space(8.0);
                                let valid = Material::is_valid_name(&m.name);
                                ui.horizontal_wrapped(|ui| {
                                    let label = if in_library { "Update in library" } else { "Save to library" };
                                    if ui
                                        .add_enabled(valid, egui::Button::new(label))
                                        .on_hover_text("Keeps it in materials.json for every document")
                                        .clicked()
                                    {
                                        save = true;
                                    }
                                    if ui
                                        .add_enabled(in_library, egui::Button::new("Delete from library"))
                                        .clicked()
                                    {
                                        delete = true;
                                    }
                                });
                                ui.horizontal_wrapped(|ui| {
                                    if let Some(count) = made_of {
                                        let label = if self.asm.is_some() {
                                            format!("Update the {count} part file(s) made of it")
                                        } else {
                                            "Update the part".to_owned()
                                        };
                                        if ui
                                            .add_enabled(valid, egui::Button::new(label))
                                            .on_hover_text(
                                                "Writes this look and density into every part made of \
                                                 this material; their own face and body appearances stay",
                                            )
                                            .clicked()
                                        {
                                            update_parts = true;
                                        }
                                    }
                                    if let Some(label) = &target {
                                        if ui
                                            .add_enabled(valid, egui::Button::new(label))
                                            .on_hover_text(
                                                "Makes it the material of the selection (saved to the \
                                                 library first). A part with appearances of its own asks.",
                                            )
                                            .clicked()
                                        {
                                            apply = true;
                                        }
                                    }
                                });
                                if !self.materials_window.status.is_empty() {
                                    ui.add_space(4.0);
                                    ui.label(RichText::new(&self.materials_window.status).small());
                                }
                            });
                    });
                });
            });
        self.materials_window.open = open;

        if new_material {
            let m = Material::new("New material", "", Default::default(), 1000.0);
            self.materials_window.draft = Some((m.clone(), m.name));
            self.materials_window.status.clear();
        }
        if duplicate {
            if let Some((m, _)) = self.materials_window.draft.clone() {
                let mut copy = m;
                copy.name = format!("{} (copy)", copy.name);
                self.materials_window.draft = Some((copy.clone(), copy.name));
            }
        }
        if let Some(picked) = pick {
            self.materials_window.draft = Some(picked);
            self.materials_window.status.clear();
        }
        let Some((draft, source)) = self.materials_window.draft.clone() else {
            return;
        };
        if save {
            let draft = draft.clone().clamped();
            self.materials.insert(draft.clone());
            self.materials_window.status = match crate::materials::save(&self.materials) {
                Ok(path) => format!("Saved {} to {}", draft.name, path.display()),
                Err(e) => format!("Could not save the material library: {e}"),
            };
            // Parts made of it follow the edit, as the user sees one material.
            let changed = self.propagate_material(&source, &draft);
            if changed > 0 {
                self.materials_window.status += &format!("; {changed} part(s) updated");
            }
            self.materials_window.draft = Some((draft.clone(), draft.name));
        }
        if delete {
            self.materials.remove(&source);
            self.materials_window.status = match crate::materials::save(&self.materials) {
                Ok(_) => {
                    format!("{source} removed from the library; parts made of it keep their copy")
                }
                Err(e) => format!("Could not save the material library: {e}"),
            };
        }
        if update_parts {
            let draft = draft.clone().clamped();
            let changed = self.propagate_material(&source, &draft);
            self.materials_window.status = format!("{changed} part(s) now made of {}", draft.name);
            self.materials_window.draft = Some((draft.clone(), draft.name));
        }
        if apply {
            let draft = draft.clone().clamped();
            self.materials.insert(draft.clone());
            if let Err(e) = crate::materials::save(&self.materials) {
                self.materials_window.status = format!("Could not save the material library: {e}");
            }
            self.materials_window.draft = Some((draft.clone(), draft.name.clone()));
            match &self.asm {
                Some(doc) => {
                    let targets: Vec<_> = doc
                        .selection
                        .iter()
                        .filter_map(crate::assembly::AsmSelected::component)
                        .collect();
                    self.apply_component_material(&targets, Some(draft.name));
                }
                None => self.apply_material_to_part(&draft.name),
            }
        }
    }

    /// Gives the open part a library material, asking first when the part has
    /// appearances of its own.
    pub(crate) fn apply_material_to_part(&mut self, name: &str) {
        let Some(material) = self.materials.get(name).cloned() else {
            self.status = format!("{name} is not in the material library");
            return;
        };
        if !self.doc.part.appearances().is_empty() {
            self.confirm = Some(Confirm::Material {
                paths: Vec::new(),
                name: name.to_owned(),
                with_appearances: 1,
            });
            return;
        }
        self.set_part_material(Some(material), false);
    }

    /// Sets the open part's material as one undo step, clearing its own appearances when
    /// asked.
    pub(crate) fn set_part_material(&mut self, material: Option<Material>, clear: bool) {
        let before = self.doc.part.definition();
        if clear {
            *self.doc.part.appearances_mut() = Default::default();
        }
        self.doc.part.set_material(material.clone());
        if self.doc.part.definition() != before {
            self.doc.checkpoint_from(before);
        }
        self.sync_material_property();
        self.doc.revision = crate::document::next_revision();
        self.refresh_materials();
        self.status = match material {
            Some(m) => format!("Material: {}", m.name),
            None => "Material removed".to_owned(),
        };
    }

    /// The question over everything, if one is pending. Answered Yes, it is carried out.
    pub(crate) fn confirm_window(&mut self, ctx: &egui::Context) {
        let Some(confirm) = self.confirm.clone() else {
            return;
        };
        let mut answer: Option<bool> = None;
        match &confirm {
            Confirm::Material {
                paths,
                name,
                with_appearances,
            } => {
                crate::dialogs::window("Replace appearances?").show(ctx, |ui| {
                    ui.set_max_width(440.0);
                    let what = if paths.is_empty() {
                        "The part has".to_owned()
                    } else if *with_appearances == 1 {
                        "One of the parts has".to_owned()
                    } else {
                        format!("{with_appearances} of the parts have")
                    };
                    ui.label(format!(
                        "{what} appearances of their own (colours, transparency or textures \
                         on faces, bodies or the whole part). Applying {name} removes them: \
                         the part is then drawn with the material's look."
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Apply and remove appearances").clicked() {
                            answer = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            answer = Some(false);
                        }
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        answer = Some(false);
                    }
                });
            }
        }
        let Some(yes) = answer else {
            return;
        };
        self.confirm = None;
        if !yes {
            return;
        }
        match confirm {
            Confirm::Material { paths, name, .. } => {
                let Some(material) = self.materials.get(&name).cloned() else {
                    return;
                };
                if paths.is_empty() {
                    // The open part: through the Appearance panel when it is open, so
                    // that Cancel there still puts things back.
                    if let Some(Dialog::Appearance(d)) = self.dialog.as_mut() {
                        d.material = Some(material);
                        d.clear_appearances = true;
                        let d = d.clone();
                        self.preview_appearance(&d);
                    } else {
                        self.set_part_material(Some(material), true);
                    }
                } else {
                    self.write_material_to_paths(&paths, Some(material), true);
                }
            }
        }
    }
}
