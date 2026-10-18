//! The Simulation tab's studies, as Fusion 360 keeps them: the tab opens on *New Study*,
//! which makes a static stress, thermal, transient thermal or thermal stress study, and a
//! model has as many as wanted, each with its own fixtures, loads, contacts, mesh settings,
//! material overrides and results. One is open at a time; the panel's list switches
//! between them, and *Save* writes them all to the model's `.oxysim`.
//!
//! The open study lives where the rest of the tab expects it (`SimulationView::study`,
//! `done`); the others wait in `SimulationView::others`, in order, with their results.

use eframe::egui;
use oxyde_format::simulation::StudyType;

use crate::app::OxydeApp;
use crate::simulation_ui::{Done, Study};

/// A study not open now, with what the tab keeps of it.
pub(crate) struct Parked {
    pub study: Study,
    pub done: Option<Done>,
    pub selected: Option<usize>,
    pub selected_contact: Option<usize>,
}

/// The study types *New Study* offers: label and what it computes.
pub(crate) const STUDY_TYPES: [(StudyType, &str, &str); 4] = [
    (
        StudyType::Static,
        "Static Stress",
        "Stresses, displacements and the factor of safety under fixtures, forces, pressures and gravity",
    ),
    (
        StudyType::Thermal,
        "Thermal",
        "Temperatures once heat flow has settled: held temperatures, heat flows, convection, radiation, heat made inside",
    ),
    (
        StudyType::TransientThermal,
        "Transient Thermal",
        "Temperatures over time from a uniform start, played back step by step",
    ),
    (
        StudyType::ThermalStress,
        "Thermal Stress",
        "The steady temperatures, then the stresses of the expansion they cause, with fixtures and loads",
    ),
];

/// What the panel and the menus call a study type.
pub(crate) fn type_label(t: StudyType) -> &'static str {
    STUDY_TYPES
        .iter()
        .find(|(k, _, _)| *k == t)
        .map_or("Study", |(_, label, _)| label)
}

/// The name new studies of `t` are numbered after: `Static 1`, `Thermal 2`…
fn base_name(t: StudyType) -> &'static str {
    match t {
        StudyType::Static => "Static",
        StudyType::Thermal => "Thermal",
        StudyType::TransientThermal => "Transient",
        StudyType::ThermalStress => "Thermal Stress",
    }
}

/// The first `base n` (n from 1) not in `taken`.
fn unique_name(base: &str, taken: &[String]) -> String {
    (1..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !taken.iter().any(|t| same_file(t, name)))
        .unwrap_or_else(|| base.to_owned())
}

/// Whether two study names would give one results file (`<study file>-<study>.oxyres`):
/// the same once made safe for a file name and compared without case, as Windows and
/// macOS compare file names.
fn same_file(a: &str, b: &str) -> bool {
    let key = |name: &str| {
        let path = crate::simulation_files::results_path(std::path::Path::new("s.oxysim"), name);
        path.to_string_lossy().to_lowercase()
    };
    key(a) == key(b)
}

/// The entries of a New Study menu; the type picked, once one is.
pub(crate) fn new_study_menu(ui: &mut egui::Ui) -> Option<StudyType> {
    let mut picked = None;
    for (t, label, hint) in STUDY_TYPES {
        if ui.button(label).on_hover_text(hint).clicked() {
            picked = Some(t);
            ui.close_menu();
        }
    }
    picked
}

impl crate::simulation_ui::SimulationView {
    /// The studies' names and types, in order, the open one at `active`.
    pub(crate) fn study_list(&self) -> Vec<(String, StudyType)> {
        if !self.started {
            return Vec::new();
        }
        let mut out: Vec<(String, StudyType)> = self
            .others
            .iter()
            .map(|p| (p.study.name.clone(), p.study.study_type))
            .collect();
        let at = self.active.min(out.len());
        out.insert(at, (self.study.name.clone(), self.study.study_type));
        out
    }

    /// Every study, in order, as saved.
    pub(crate) fn all_studies(&self) -> Vec<Study> {
        if !self.started {
            return Vec::new();
        }
        let mut out: Vec<Study> = self.others.iter().map(|p| p.study.clone()).collect();
        out.insert(self.active.min(out.len()), self.study.clone());
        out
    }

    /// The open study set aside, with its results, as the others wait.
    fn park(&mut self) -> Option<Parked> {
        if !self.started {
            return None;
        }
        self.started = false;
        Some(Parked {
            study: std::mem::take(&mut self.study),
            done: self.done.take(),
            selected: self.selected.take(),
            selected_contact: self.selected_contact.take(),
        })
    }

    /// `p` open, as the study at `index` among them all.
    fn open(&mut self, p: Parked, index: usize) {
        self.study = p.study;
        self.done = p.done;
        self.selected = p.selected;
        self.selected_contact = p.selected_contact;
        self.active = index;
        self.started = true;
        self.renaming = None;
        self.frame = self
            .done
            .as_ref()
            .and_then(|d| d.thermal.as_ref())
            .map_or(0, |t| t.frames.len().saturating_sub(1));
        self.playing = None;
        // The model comes back to set up the study; its results show when asked for.
        self.show_plot = false;
    }
}

impl OxydeApp {
    /// A new study of type `t`, open, after the others.
    pub(crate) fn new_study(&mut self, t: StudyType) {
        if self.simulation.job.is_some() {
            self.status = "Wait for the study to finish, or cancel it".to_owned();
            return;
        }
        let taken: Vec<String> = self
            .simulation
            .study_list()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        let name = unique_name(base_name(t), &taken);
        self.restore_simulation_scene();
        if let Some(open) = self.simulation.park() {
            let at = self.simulation.active.min(self.simulation.others.len());
            self.simulation.others.insert(at, open);
        }
        let index = self.simulation.others.len();
        self.simulation.open(
            Parked {
                study: Study::of_type(t, &name),
                done: None,
                selected: None,
                selected_contact: None,
            },
            index,
        );
        self.status = format!(
            "{name}: {}",
            match t {
                StudyType::Static => "select faces, add fixtures and loads, then Run".to_owned(),
                StudyType::Thermal | StudyType::TransientThermal =>
                    "select faces, add temperatures, heat and cooling, then Run".to_owned(),
                StudyType::ThermalStress =>
                    "select faces, add fixtures and temperatures or heat, then Run".to_owned(),
            }
        );
    }

    /// Opens the study at `index` of the list.
    pub(crate) fn switch_study(&mut self, index: usize) {
        if !self.simulation.started || index == self.simulation.active {
            return;
        }
        if self.simulation.job.is_some() {
            self.status = "Wait for the study to finish, or cancel it".to_owned();
            return;
        }
        let count = self.simulation.others.len() + 1;
        if index >= count {
            return;
        }
        self.restore_simulation_scene();
        let Some(open) = self.simulation.park() else {
            return;
        };
        let active = self.simulation.active.min(self.simulation.others.len());
        self.simulation.others.insert(active, open);
        let next = self.simulation.others.remove(index);
        self.simulation.open(next, index);
        self.status = format!("Study: {}", self.simulation.study.name);
    }

    /// Deletes the open study; the next one opens, or the tab goes back to New Study.
    pub(crate) fn delete_study(&mut self) {
        if self.simulation.job.is_some() {
            self.status = "Wait for the study to finish, or cancel it".to_owned();
            return;
        }
        self.restore_simulation_scene();
        let Some(gone) = self.simulation.park() else {
            return;
        };
        self.status = format!(
            "{} deleted (Save writes the study file without it)",
            gone.study.name
        );
        if self.simulation.others.is_empty() {
            self.simulation.active = 0;
            return;
        }
        let index = self.simulation.active.min(self.simulation.others.len() - 1);
        let next = self.simulation.others.remove(index);
        self.simulation.open(next, index);
    }

    /// Renames the open study, keeping names unique; why not when it cannot.
    pub(crate) fn rename_study(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err("A study needs a name".to_owned());
        }
        if let Some(other) = self
            .simulation
            .others
            .iter()
            .find(|p| same_file(&p.study.name, name))
        {
            return Err(if other.study.name == name {
                format!("Another study is called {name}")
            } else {
                format!(
                    "{name} would share its results file with {}: choose another name",
                    other.study.name
                )
            });
        }
        self.simulation.study.name = name.to_owned();
        Ok(())
    }

    /// Studies read from a file in place of the model's: the first opens, the others wait,
    /// none with results.
    pub(crate) fn set_studies(&mut self, studies: Vec<Study>) {
        self.simulation.job = None;
        self.restore_simulation_scene();
        self.simulation.park();
        self.simulation.others.clear();
        let mut studies = studies.into_iter();
        if let Some(first) = studies.next() {
            self.simulation.others = studies
                .map(|study| Parked {
                    study,
                    done: None,
                    selected: None,
                    selected_contact: None,
                })
                .collect();
            self.simulation.open(
                Parked {
                    study: first,
                    done: None,
                    selected: None,
                    selected_contact: None,
                },
                0,
            );
        } else {
            self.simulation.active = 0;
        }
    }

    /// The panel's top while no study is open: the four types to start one, as Fusion 360's
    /// New Study dialog offers them, and Open for a study saved before.
    pub(crate) fn new_study_chooser(&mut self, ui: &mut egui::Ui) {
        ui.strong("New Study");
        ui.label(
            egui::RichText::new("Pick what to study; a model can have any number of studies.")
                .small()
                .weak(),
        );
        ui.add_space(4.0);
        let mut picked = None;
        for (t, label, hint) in STUDY_TYPES {
            let response = egui::Frame::group(ui.style())
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let icon = match t {
                            StudyType::Static => crate::icons::Icon::Stress,
                            StudyType::Thermal => crate::icons::Icon::TemperaturePlot,
                            StudyType::TransientThermal => crate::icons::Icon::Temperature,
                            StudyType::ThermalStress => crate::icons::Icon::HeatPower,
                        };
                        crate::icons::small(ui, icon, 26.0);
                        ui.vertical(|ui| {
                            ui.strong(label);
                            ui.label(egui::RichText::new(hint).small().weak());
                        });
                    });
                })
                .response
                .interact(egui::Sense::click());
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if response
                .on_hover_text(format!("New {label} study"))
                .clicked()
            {
                picked = Some(t);
            }
        }
        ui.add_space(4.0);
        if ui
            .small_button("Open…")
            .on_hover_text("Open the studies saved for this model (.oxysim)")
            .clicked()
        {
            self.open_study();
        }
        if let Some(t) = picked {
            self.new_study(t);
        }
    }

    /// The open study's line at the top of the panel: its name and type, the list of the
    /// model's studies, New Study, Rename and Delete.
    pub(crate) fn study_bar(&mut self, ui: &mut egui::Ui) {
        let list = self.simulation.study_list();
        let running = self.simulation.job.is_some();
        let mut switch_to = None;
        let mut new_type = None;
        let mut delete = false;
        ui.horizontal(|ui| {
            let current = format!(
                "{} — {}",
                self.simulation.study.name,
                type_label(self.simulation.study.study_type)
            );
            ui.add_enabled_ui(!running, |ui| {
                egui::ComboBox::from_id_salt("simulation_studies")
                    .selected_text(egui::RichText::new(current).strong())
                    .width((ui.available_width() - 80.0).max(120.0))
                    .show_ui(ui, |ui| {
                        for (i, (name, t)) in list.iter().enumerate() {
                            let text = format!("{name} — {}", type_label(*t));
                            if ui
                                .selectable_label(i == self.simulation.active, text)
                                .clicked()
                            {
                                switch_to = Some(i);
                            }
                        }
                    })
                    .response
                    .on_hover_text("The model's studies: pick one to open it");
                ui.menu_button("New", |ui| {
                    new_type = new_study_menu(ui);
                })
                .response
                .on_hover_text("A new study of this model");
            });
        });
        ui.horizontal(|ui| match self.simulation.renaming.as_mut() {
            Some(name) => {
                let edit = ui.add(egui::TextEdit::singleline(name).desired_width(140.0));
                let done = edit.lost_focus() || ui.small_button("OK").clicked();
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.simulation.renaming = None;
                } else if done {
                    let name = name.clone();
                    match self.rename_study(&name) {
                        Ok(()) => self.simulation.renaming = None,
                        Err(e) => self.status = e,
                    }
                }
            }
            None => {
                if ui
                    .add_enabled(!running, egui::Button::new("Rename").small())
                    .clicked()
                {
                    self.simulation.renaming = Some(self.simulation.study.name.clone());
                }
                if ui
                    .add_enabled(!running, egui::Button::new("Delete").small())
                    .on_hover_text("Delete this study (Save writes the study file without it)")
                    .clicked()
                {
                    delete = true;
                }
            }
        });
        if let Some(i) = switch_to {
            self.switch_study(i);
        }
        if let Some(t) = new_type {
            self.new_study(t);
        }
        if delete {
            self.delete_study();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_studies_are_numbered_per_type() {
        let taken = vec!["Static 1".to_owned(), "Static 3".to_owned()];
        assert_eq!(unique_name("Static", &taken), "Static 2");
        assert_eq!(unique_name("Thermal", &taken), "Thermal 1");
        assert_eq!(type_label(StudyType::TransientThermal), "Transient Thermal");
        assert_eq!(base_name(StudyType::ThermalStress), "Thermal Stress");
        // Names one file name apart are the same study's results.
        let taken = vec!["static 1".to_owned()];
        assert_eq!(unique_name("Static", &taken), "Static 2");
        assert!(same_file("Load/Case", "load_case"));
        assert!(!same_file("Load 1", "Load 2"));
    }

    fn view_with(names: &[&str]) -> crate::simulation_ui::SimulationView {
        let mut view = crate::simulation_ui::SimulationView::default();
        let mut parked: Vec<Parked> = names
            .iter()
            .map(|n| Parked {
                study: Study::of_type(StudyType::Thermal, n),
                done: None,
                selected: None,
                selected_contact: None,
            })
            .collect();
        let first = parked.remove(0);
        view.others = parked;
        view.open(first, 0);
        view
    }

    #[test]
    fn the_open_study_keeps_its_place_in_the_list() {
        let mut view = view_with(&["A", "B", "C"]);
        let names = |v: &crate::simulation_ui::SimulationView| -> Vec<String> {
            v.study_list().into_iter().map(|(n, _)| n).collect()
        };
        assert_eq!(names(&view), ["A", "B", "C"]);
        // Open C: A and B wait in order, the list stays A, B, C.
        let open = view.park().unwrap();
        view.others.insert(0, open);
        let c = view.others.remove(2);
        view.open(c, 2);
        assert_eq!(view.study.name, "C");
        assert_eq!(names(&view), ["A", "B", "C"]);
        assert_eq!(
            view.all_studies()
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
        // Nothing open: nothing listed.
        view.park();
        assert!(view.study_list().is_empty());
    }
}
