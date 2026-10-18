//! Tools › Preferences: graphics, navigation, keyboard shortcuts, simulation and debugging. Changes
//! apply at once and are saved as they are made. Also the debug overlay drawn over the
//! viewport, and the dispatch of keyboard shortcuts to commands.

use eframe::egui::{self, Color32, RichText};
use oxyde_render::{Rgb, StandardView, Style};

use crate::app::{Command, OxydeApp, PendingAction};
use crate::features_ui::PatternKind;
use crate::prefs::{Action, Binding, MeshQuality, Preferences, Renderer};
use crate::sketch_mode::Tool;
use crate::viewport::ViewSettings;

/// The pages of the Preferences window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrefsTab {
    #[default]
    Graphics,
    Navigation,
    Shortcuts,
    Simulation,
    Debug,
}

/// The Preferences window's own state.
#[derive(Default)]
pub struct PrefsWindow {
    pub open: bool,
    pub tab: PrefsTab,
    /// The action waiting for a key combination.
    capturing: Option<Action>,
    filter: String,
    /// The interface theme in use, to restyle only when it changes.
    applied_theme: Option<crate::theme::UiTheme>,
}

/// Timings and counts for the debug overlay.
#[derive(Default)]
pub struct DebugStats {
    /// Smoothed frame time, seconds.
    pub frame: f64,
    /// Last part rebuild, seconds.
    pub rebuild: Option<f64>,
    /// Last feature preview built, seconds.
    pub preview: Option<f64>,
    /// Last assembly solve, seconds.
    pub solve: Option<f64>,
}

fn color_row(ui: &mut egui::Ui, label: &str, color: &mut Rgb) -> bool {
    ui.label(label);
    let changed = ui.color_edit_button_srgb(color).changed();
    ui.end_row();
    changed
}

impl OxydeApp {
    /// What the viewport takes from the preferences.
    pub(crate) fn apply_preferences(&mut self, ctx: &egui::Context) {
        let p = &self.prefs;
        self.viewport.settings = ViewSettings {
            style: p.graphics.style(),
            mouse: p.navigation.mouse,
            show_triad: p.graphics.show_triad,
            orbit_speed: p.navigation.orbit_speed,
            zoom_speed: p.navigation.zoom_speed,
            invert_zoom: p.navigation.invert_zoom,
            orbit_about_pointer: p.navigation.orbit_about_pointer,
            constrained_orbit: p.navigation.constrained_orbit,
            animation_time: if p.navigation.animate_views {
                p.navigation.animation_time
            } else {
                0.0
            },
        };
        crate::theme::set_viewport(&self.viewport.settings.style);
        let theme = p.graphics.ui_theme;
        if self.prefs_window.applied_theme != Some(theme) {
            crate::theme::apply(ctx, theme);
            self.prefs_window.applied_theme = Some(theme);
        }
        // egui only has its widget debugging in debug builds.
        #[cfg(debug_assertions)]
        {
            let debug = self.prefs.debug.ui_debug_on_hover;
            ctx.style_mut(|s| s.debug.debug_on_hover = debug);
        }
    }

    pub(crate) fn mesh_fraction(&self) -> f64 {
        self.prefs.graphics.mesh_quality.fraction()
    }

    pub(crate) fn save_preferences(&mut self) {
        if let Err(e) = self.prefs.save() {
            self.status = format!("Could not save the preferences: {e}");
        }
    }

    pub(crate) fn open_preferences(&mut self, tab: PrefsTab) {
        self.prefs_window.open = true;
        self.prefs_window.tab = tab;
    }

    pub(crate) fn preferences_window(&mut self, ctx: &egui::Context) {
        if !self.prefs_window.open {
            return;
        }
        let before = self.prefs.clone();
        let mut open = true;
        egui::Window::new("Preferences")
            .open(&mut open)
            .resizable(true)
            .default_width(520.0)
            .default_height(480.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (PrefsTab::Graphics, "Graphics"),
                        (PrefsTab::Navigation, "Navigation"),
                        (PrefsTab::Shortcuts, "Shortcuts"),
                        (PrefsTab::Simulation, "Simulation"),
                        (PrefsTab::Debug, "Debug"),
                    ] {
                        let text = RichText::new(label).size(14.0);
                        let current = self.prefs_window.tab == tab;
                        if ui
                            .selectable_label(current, if current { text.strong() } else { text })
                            .clicked()
                        {
                            self.prefs_window.tab = tab;
                            self.prefs_window.capturing = None;
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.prefs_window.tab {
                        PrefsTab::Graphics => self.graphics_page(ui),
                        PrefsTab::Navigation => self.navigation_page(ui),
                        PrefsTab::Shortcuts => self.shortcuts_page(ui),
                        PrefsTab::Simulation => self.simulation_page(ui),
                        PrefsTab::Debug => self.debug_page(ui),
                    });
            });
        if !open {
            self.prefs_window.open = false;
            self.prefs_window.capturing = None;
        }
        if self.prefs != before {
            if self.prefs.graphics.mesh_quality != before.graphics.mesh_quality {
                self.remesh();
            }
            self.apply_preferences(ctx);
            self.save_preferences();
            // Back to the constrained orbit: Y up again, as View › Constrained orbit does,
            // or the view would stay tilted and the change would not show.
            if self.prefs.navigation.constrained_orbit && !before.navigation.constrained_orbit {
                self.viewport.level();
            }
        }
    }

    /// Meshes everything again at the new quality.
    fn remesh(&mut self) {
        self.scene_revision = None;
        if self.asm.is_some() {
            self.reload_assembly_parts();
        }
    }

    fn graphics_page(&mut self, ui: &mut egui::Ui) {
        let g = &mut self.prefs.graphics;
        crate::property_manager::section(ui, "Interface");
        ui.horizontal(|ui| {
            for (theme, label) in crate::theme::UiTheme::ALL {
                if ui.radio(g.ui_theme == theme, label).clicked() && g.ui_theme != theme {
                    g.ui_theme = theme;
                    // The viewport follows; its colours can still be changed below.
                    g.set_style(&theme.viewport());
                }
            }
        });
        ui.add_space(8.0);
        crate::property_manager::section(ui, "Renderer");
        ui.horizontal(|ui| {
            ui.radio_value(&mut g.renderer, Renderer::Gpu, "GPU");
            ui.radio_value(&mut g.renderer, Renderer::Software, "Software");
        });
        ui.checkbox(&mut g.antialiasing, "Anti-aliasing (4× multisampling, GPU)");
        ui.label(
            RichText::new(format!(
                "These two take effect at the next start. Now: {}.",
                self.viewport.renderer_name()
            ))
            .small()
            .weak(),
        );

        ui.add_space(8.0);
        crate::property_manager::section(ui, "Viewport colours");
        ui.horizontal(|ui| {
            ui.label("Preset");
            let presets = [("Flux", crate::theme::FLUX_VIEWPORT)]
                .into_iter()
                .chain(Style::PRESETS);
            for (name, style) in presets {
                if ui.button(name).clicked() {
                    g.set_style(&style);
                }
            }
        });
        egui::Grid::new("colours").num_columns(2).show(ui, |ui| {
            color_row(ui, "Background, top", &mut g.background_top);
            color_row(ui, "Background, bottom", &mut g.background_bottom);
            color_row(ui, "Part", &mut g.face);
            color_row(ui, "Selected face", &mut g.face_selected);
            color_row(ui, "Face under the pointer", &mut g.face_hovered);
            color_row(ui, "Edges", &mut g.edge);
            color_row(ui, "Selected edge", &mut g.edge_selected);
            color_row(ui, "Edge under the pointer", &mut g.edge_hovered);
        });

        ui.add_space(8.0);
        crate::property_manager::section(ui, "Display");
        ui.checkbox(&mut g.show_edges, "Show edges")
            .on_hover_text("Hidden edges cannot be clicked: pick faces instead");
        ui.add_enabled_ui(g.show_edges, |ui| {
            egui::Grid::new("edges").num_columns(2).show(ui, |ui| {
                ui.label("Edge width");
                ui.add(egui::Slider::new(&mut g.edge_width, 0.5..=4.0).suffix(" px"));
                ui.end_row();
                ui.label("Highlighted edge width");
                ui.add(egui::Slider::new(&mut g.highlight_width, 1.0..=8.0).suffix(" px"));
                ui.end_row();
            });
        });
        ui.checkbox(&mut g.show_triad, "Orientation triad in the corner");
        ui.checkbox(&mut g.floor_reflections, "Floor reflections")
            .on_hover_text("The part mirrored in a floor just under it (also in View)");
        ui.add_enabled_ui(g.floor_reflections, |ui| {
            ui.horizontal(|ui| {
                ui.label("Reflection strength");
                ui.add(egui::Slider::new(&mut g.reflection_strength, 0.05..=1.0));
            });
        });
        ui.checkbox(&mut g.ambient_occlusion, "Ambient occlusion")
            .on_hover_text(
                "Creases, inner corners and the foot of walls are darkened, as less ambient \
                 light reaches them: the shape reads better (also in View)",
            );
        ui.add_enabled_ui(g.ambient_occlusion, |ui| {
            ui.horizontal(|ui| {
                ui.label("Occlusion strength");
                ui.add(egui::Slider::new(
                    &mut g.ambient_occlusion_strength,
                    0.05..=1.0,
                ));
            });
        });
        ui.horizontal(|ui| {
            ui.label("Mesh quality");
            egui::ComboBox::from_id_salt("mesh_quality")
                .selected_text(
                    MeshQuality::ALL
                        .iter()
                        .find(|(q, _)| *q == g.mesh_quality)
                        .map_or("", |(_, l)| *l),
                )
                .show_ui(ui, |ui| {
                    for (quality, label) in MeshQuality::ALL {
                        ui.selectable_value(&mut g.mesh_quality, quality, label);
                    }
                });
        });
        ui.label(
            RichText::new("Finer meshes show curved faces smoother but take longer to draw.")
                .small()
                .weak(),
        );
        crate::property_manager::section(ui, "Previews");
        let p = &mut self.prefs.previews;
        ui.checkbox(
            &mut p.features,
            "Preview features while their command is open",
        )
        .on_hover_text(
            "Extrude, Fillet, Shell, patterns… show the part as the feature makes it, the \
             faces it makes tinted (red where it removes material), before OK",
        );
        ui.checkbox(&mut p.mates, "Move the parts as mates are picked")
            .on_hover_text(
                "The Mate command places the parts as soon as both sides are picked; off, \
                 they stay put until OK, so every mate is picked where the parts sit now",
            );
        ui.add_space(8.0);
        if ui.button("Reset graphics to defaults").clicked() {
            self.prefs.graphics = Default::default();
            self.prefs.previews = Default::default();
        }
    }

    fn navigation_page(&mut self, ui: &mut egui::Ui) {
        let n = &mut self.prefs.navigation;
        egui::Grid::new("navigation").num_columns(2).show(ui, |ui| {
            ui.label("Mouse buttons");
            egui::ComboBox::from_id_salt("mouse_scheme")
                .selected_text(
                    crate::prefs::MouseScheme::ALL
                        .iter()
                        .find(|(s, _)| *s == n.mouse)
                        .map_or("", |(_, l)| *l),
                )
                .show_ui(ui, |ui| {
                    for (scheme, label) in crate::prefs::MouseScheme::ALL {
                        ui.selectable_value(&mut n.mouse, scheme, label);
                    }
                });
            ui.end_row();
            ui.label("Orbit speed");
            ui.add(egui::Slider::new(&mut n.orbit_speed, 0.2..=3.0).suffix("×"));
            ui.end_row();
            ui.label("Zoom speed");
            ui.add(
                egui::Slider::new(&mut n.zoom_speed, 0.1..=5.0)
                    .logarithmic(true)
                    .suffix("×"),
            )
            .on_hover_text("How much one wheel notch zooms");
            ui.end_row();
        });
        ui.checkbox(
            &mut n.invert_zoom,
            "Reverse the mouse wheel (forward zooms out)",
        );
        ui.checkbox(
            &mut n.orbit_about_pointer,
            "Orbit about the point under the pointer",
        )
        .on_hover_text(
            "The model point under the pointer when the drag starts stays put on screen; \
             off, the view turns about its centre",
        );
        ui.checkbox(&mut n.constrained_orbit, "Constrained orbit: keep Y up")
            .on_hover_text(
                "Orbiting keeps the model's vertical axis up the screen, as in SolidWorks. \
                 Off, the view turns freely about the screen's axes and can be tilted, as \
                 Fusion 360's free orbit (also in View). Turning it back on levels the view.",
            );
        ui.checkbox(&mut n.animate_views, "Animate view changes");
        ui.add_enabled_ui(n.animate_views, |ui| {
            ui.horizontal(|ui| {
                ui.label("Duration");
                ui.add(egui::Slider::new(&mut n.animation_time, 0.1..=1.5).suffix(" s"));
            });
        });
        ui.add_space(8.0);
        crate::property_manager::section(ui, "Mouse");
        let help = n.mouse.help();
        for (what, how) in [
            help[0],
            help[1],
            ("Zoom", "wheel, at the pointer"),
            ("Zoom to fit", "double-click the middle button, or F"),
            help[2],
            (
                "Move a component (assembly)",
                "Alt+left-drag; Alt+Shift+left-drag turns it",
            ),
            help[3],
        ] {
            ui.horizontal(|ui| {
                ui.label(RichText::new(what).strong());
                ui.label(how);
            });
        }
        ui.add_space(8.0);
        if ui.button("Reset navigation to defaults").clicked() {
            self.prefs.navigation = Default::default();
        }
    }

    fn shortcuts_page(&mut self, ui: &mut egui::Ui) {
        // A key pressed while waiting sets the shortcut; Escape cancels.
        if let Some(action) = self.prefs_window.capturing {
            if let Some(binding) = Binding::captured(ui.ctx()) {
                if binding.key != egui::Key::Escape || binding.ctrl || binding.alt {
                    self.prefs.set_binding(action, Some(binding));
                }
                self.prefs_window.capturing = None;
                // Do not let the key run its command too.
                ui.ctx().input_mut(|i| i.events.clear());
            }
        }
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.text_edit_singleline(&mut self.prefs_window.filter);
            if ui.button("Reset all").clicked() {
                self.prefs.shortcuts.clear();
                self.prefs_window.capturing = None;
            }
        });
        ui.label(
            RichText::new(
                "Click a shortcut, then press the new keys (Escape cancels). Keys without \
                 Ctrl or Alt do nothing while you type in a field.",
            )
            .small()
            .weak(),
        );
        ui.add_space(4.0);
        let filter = self.prefs_window.filter.to_lowercase();
        let mut category = "";
        egui::Grid::new("shortcuts")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                for action in Action::ALL {
                    if !filter.is_empty() && !action.label().to_lowercase().contains(&filter) {
                        continue;
                    }
                    if action.category() != category {
                        category = action.category();
                        ui.label(RichText::new(category).strong());
                        ui.end_row();
                    }
                    ui.label(format!("   {}", action.label()));
                    let capturing = self.prefs_window.capturing == Some(action);
                    let text = if capturing {
                        "Press keys…".to_owned()
                    } else {
                        self.prefs
                            .binding(action)
                            .map_or_else(|| "—".to_owned(), |b| b.to_string())
                    };
                    let conflicts = self.prefs.conflicts(action);
                    let mut text = RichText::new(text).monospace();
                    if !conflicts.is_empty() {
                        text = text.color(crate::theme::tokens().err);
                    }
                    let mut button =
                        ui.add(egui::Button::new(text).min_size(egui::vec2(130.0, 0.0)));
                    if !conflicts.is_empty() {
                        let names: Vec<&str> = conflicts.iter().map(|a| a.label()).collect();
                        button = button.on_hover_text(format!("Also used by {}", names.join(", ")));
                    }
                    if button.clicked() {
                        self.prefs_window.capturing = (!capturing).then_some(action);
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("Clear")
                            .on_hover_text("No shortcut")
                            .clicked()
                        {
                            self.prefs.set_binding(action, None);
                        }
                        let default = action.default_binding();
                        if self.prefs.binding(action) != default
                            && ui
                                .small_button("Default")
                                .on_hover_text(
                                    default.map_or_else(|| "None".to_owned(), |b| b.to_string()),
                                )
                                .clicked()
                        {
                            self.prefs.set_binding(action, default);
                        }
                    });
                    ui.end_row();
                }
            });
    }

    fn simulation_page(&mut self, ui: &mut egui::Ui) {
        let p = &mut self.prefs.simulation;
        crate::property_manager::section(ui, "Memory");
        egui::Grid::new("simulation_prefs")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Memory budget");
                ui.add(
                    egui::DragValue::new(&mut p.memory_budget_gb)
                        .range(0.5..=1024.0)
                        .speed(0.1)
                        .max_decimals(1)
                        .suffix(" GB"),
                )
                .on_hover_text(
                    "The most memory a study may take. A finer mesh is refused before it is \
                     solved, rather than filling the memory and slowing the computer down.",
                );
                ui.end_row();
                ui.label("");
                ui.label(
                    RichText::new(format!(
                        "About {} nodes with high quality (10-node) elements, {} with draft \
                         (4-node) ones",
                        crate::simulation_ui::max_nodes(p.memory_budget_gb, true),
                        crate::simulation_ui::max_nodes(p.memory_budget_gb, false),
                    ))
                    .small()
                    .weak(),
                );
                ui.end_row();
            });
        ui.label(
            RichText::new(
                "Keep it under the computer's memory, leaving room for the rest: half of it \
                 is a safe choice.",
            )
            .small()
            .weak(),
        );
    }

    fn debug_page(&mut self, ui: &mut egui::Ui) {
        let d = &mut self.prefs.debug;
        ui.checkbox(&mut d.performance_overlay, "Performance overlay")
            .on_hover_text("Frame time, renderer, triangles and edges, rebuild and solve times");
        ui.checkbox(&mut d.hover_info, "What is under the pointer")
            .on_hover_text("Face or edge index and its persistent name");
        ui.add_enabled(
            cfg!(debug_assertions),
            egui::Checkbox::new(
                &mut d.ui_debug_on_hover,
                "Interface: outline widgets on hover",
            ),
        )
        .on_disabled_hover_text("Only in debug builds (cargo run without --release)");
        ui.checkbox(&mut d.ui_inspector, "Interface: egui inspector window");
        ui.add_space(8.0);
        crate::property_manager::section(ui, "About this build");
        egui::Grid::new("build").num_columns(2).show(ui, |ui| {
            ui.label("Oxyde");
            ui.label(env!("CARGO_PKG_VERSION"));
            ui.end_row();
            ui.label("OpenCASCADE");
            ui.label(oxyde_occt::OcctKernel::occt_version());
            ui.end_row();
            ui.label("Viewport");
            ui.label(self.viewport.renderer_name());
            ui.end_row();
            ui.label("Preferences file");
            ui.label(
                crate::prefs::path()
                    .map_or_else(|| "(none)".to_owned(), |p| p.display().to_string()),
            );
            ui.end_row();
        });
        ui.add_space(8.0);
        if ui.button("Reset all preferences").clicked() {
            self.prefs = Preferences::default();
        }
    }

    // ---- Overlay ---------------------------------------------------------------------------

    /// Debug information drawn over the viewport.
    pub(crate) fn debug_overlay(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        let dt = ctx.input(|i| i.unstable_dt) as f64;
        self.debug_stats.frame = if self.debug_stats.frame == 0.0 {
            dt
        } else {
            self.debug_stats.frame * 0.9 + dt * 0.1
        };
        let d = &self.prefs.debug;
        if d.ui_inspector {
            let mut open = true;
            egui::Window::new("egui inspector")
                .open(&mut open)
                .vscroll(true)
                .show(ctx, |ui| {
                    ctx.settings_ui(ui);
                    ui.separator();
                    ctx.inspection_ui(ui);
                });
            if !open {
                self.prefs.debug.ui_inspector = false;
                self.save_preferences();
            }
        }
        self.debug_stats.solve = self.asm.as_ref().and_then(|d| d.last_solve);
        let d = &self.prefs.debug;
        let mut lines = Vec::new();
        if d.performance_overlay {
            let (triangles, edges) = self.viewport.scene_size();
            let ms =
                |s: Option<f64>| s.map_or_else(|| "—".to_owned(), |s| format!("{:.1} ms", s * 1e3));
            lines.push(format!(
                "{:.1} ms/frame ({:.0} fps) · {}",
                self.debug_stats.frame * 1e3,
                1.0 / self.debug_stats.frame.max(1e-6),
                self.viewport.renderer_name()
            ));
            lines.push(format!("{triangles} triangles · {edges} edge segments"));
            lines.push(format!(
                "rebuild {} · preview {} · solve {}",
                ms(self.debug_stats.rebuild),
                ms(self.debug_stats.preview),
                ms(self.debug_stats.solve)
            ));
            // Keep the frame time moving.
            ctx.request_repaint();
        }
        if d.hover_info {
            lines.push(self.hover_description());
        }
        if lines.is_empty() {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("debug_overlay"),
        ));
        let font = egui::FontId::monospace(12.0);
        let mut y = rect.top() + 8.0;
        for line in lines {
            let galley = painter.layout_no_wrap(line, font.clone(), Color32::WHITE);
            let at = egui::pos2(rect.right() - galley.size().x - 10.0, y);
            painter.rect_filled(
                egui::Rect::from_min_size(
                    at - egui::vec2(4.0, 2.0),
                    galley.size() + egui::vec2(8.0, 4.0),
                ),
                3.0,
                Color32::from_black_alpha(160),
            );
            y += galley.size().y + 6.0;
            painter.galley(at, galley, Color32::WHITE);
        }
    }

    fn hover_description(&self) -> String {
        use oxyde_render::Pick;
        let Some(pick) = self.viewport.hovered() else {
            return "under the pointer: nothing".to_owned();
        };
        if let Some(doc) = &self.asm {
            let what = doc.picked(pick).map_or_else(
                || "?".to_owned(),
                |s| match s {
                    crate::assembly::AsmSelected::Face(o, f) => {
                        format!("face {f} of {}", occurrence_name(doc, &o))
                    }
                    crate::assembly::AsmSelected::Edge(o, e) => {
                        format!("edge {e} of {}", occurrence_name(doc, &o))
                    }
                    _ => String::new(),
                },
            );
            return format!("{pick:?}: {what}");
        }
        let name = self.shown_body().and_then(|b| match pick {
            Pick::Face(i) => b.naming.face(i).map(|n| format!("face {n}")),
            Pick::Edge(i) => b.naming.edge(i).map(|n| format!("edge {n}")),
        });
        format!("{pick:?}: {}", name.unwrap_or_else(|| "?".to_owned()))
    }

    // ---- Shortcuts -------------------------------------------------------------------------

    /// Runs the commands whose shortcuts were pressed this frame.
    pub(crate) fn dispatch_shortcuts(&mut self, ctx: &egui::Context) {
        if self.prefs_window.capturing.is_some() {
            return;
        }
        // Combinations with more modifiers first: Ctrl+Shift+Z before Ctrl+Z.
        let mut actions = Action::ALL;
        actions.sort_by_key(|a| {
            std::cmp::Reverse(
                self.prefs
                    .binding(*a)
                    .map_or(0, |b| b.ctrl as u8 + b.shift as u8 + b.alt as u8),
            )
        });
        let assembly = self.asm.is_some();
        let sketching = self.sketch.is_some();
        for action in actions {
            let applies = match action.context() {
                crate::prefs::Context::Everywhere => true,
                crate::prefs::Context::Part => !assembly && !sketching,
                // Tool keys act inside a sketch; the command bar starts one.
                crate::prefs::Context::Sketch => sketching,
                crate::prefs::Context::Assembly => assembly,
            };
            // Commands wait while a window asks something.
            let busy = self.dialog.as_ref().is_some_and(|d| !d.is_panel());
            if applies && !busy && self.prefs.pressed(ctx, action) {
                self.run_action(action, ctx);
            }
        }
    }

    fn run_action(&mut self, action: Action, ctx: &egui::Context) {
        let assembly = self.asm.is_some();
        match action {
            Action::NewPart => self.guard(PendingAction::New, ctx),
            Action::Open => self.guard(PendingAction::Open, ctx),
            Action::Save => {
                if assembly {
                    self.save_assembly(false);
                } else {
                    self.save();
                }
            }
            Action::Preferences => self.open_preferences(PrefsTab::Graphics),
            Action::Undo => {
                if assembly {
                    self.asm_undo();
                } else if let Some(session) = self.sketch.as_mut() {
                    session.undo();
                } else {
                    self.undo();
                }
            }
            Action::Redo | Action::RedoAlternate => {
                if assembly {
                    self.asm_redo();
                } else if self.sketch.is_none() {
                    self.redo();
                }
            }
            Action::Delete => {
                // An open command keeps its selection.
                if self.dialog.is_some() || self.mate_dialog.is_some() {
                    return;
                }
                if assembly {
                    self.delete_selected();
                } else if let Some(session) = self.sketch.as_mut() {
                    session.delete_selection();
                } else {
                    let selected = self.selected_feature_set();
                    if !selected.is_empty() {
                        self.delete_features(&selected);
                    }
                }
            }
            Action::Rebuild => {
                if assembly {
                    self.reload_assembly_parts();
                } else {
                    self.doc.release(&mut self.kernel);
                    self.rebuild();
                }
            }
            Action::ZoomToFit => self.viewport.request_fit(),
            Action::ViewFront => self.viewport.set_view(StandardView::Front),
            Action::ViewTop => self.viewport.set_view(StandardView::Top),
            Action::ViewRight => self.viewport.set_view(StandardView::Right),
            Action::ViewIsometric => self.viewport.set_view(StandardView::Isometric),
            Action::NormalTo => {
                if let Some(plane) = self.normal_to_target() {
                    self.viewport.look_at_plane(&plane);
                }
            }
            Action::Sketch => self.run(Command::Sketch),
            Action::Extrude => self.run(Command::Extrude),
            Action::Revolve => self.run(Command::Revolve),
            Action::Sweep => self.run(Command::Sweep),
            Action::Fillet => self.run(Command::Blend(true)),
            Action::Chamfer => self.run(Command::Blend(false)),
            Action::Shell => self.run(Command::Shell),
            Action::LinearPattern => self.run(Command::Pattern(PatternKind::Linear)),
            Action::CircularPattern => self.run(Command::Pattern(PatternKind::Circular)),
            Action::Mirror => self.run(Command::Pattern(PatternKind::Mirror)),
            Action::SmartDimension => self.sketch_tool(Tool::Dimension),
            Action::Line => self.sketch_tool(Tool::Line),
            Action::Centerline => self.sketch_tool(Tool::Centerline),
            Action::Rectangle => self.sketch_tool(Tool::Rectangle),
            Action::Circle => self.sketch_tool(Tool::Circle),
            Action::Arc => self.sketch_tool(Tool::Arc),
            Action::Trim => self.sketch_tool(Tool::Trim),
            Action::InsertComponent => self.insert_component(),
            Action::Mate => {
                if self
                    .asm
                    .as_ref()
                    .is_some_and(|d| d.assembly.components.len() >= 2)
                {
                    self.begin_mate();
                }
            }
        }
    }

    /// A sketch tool from the keyboard: in the open sketch, or a new one for drawing tools.
    fn sketch_tool(&mut self, tool: Tool) {
        match self.sketch.as_mut() {
            Some(session) => session.set_tool(tool),
            None if tool.draws() => self.begin_sketch(Some(tool)),
            None => {}
        }
    }
}

fn occurrence_name(doc: &crate::assembly::AssemblyDoc, o: &crate::assembly::Occurrence) -> String {
    let top = doc
        .component(o.top)
        .map_or_else(|| "?".to_owned(), |c| c.name.clone());
    let mut names = doc.inner_names(o.top, &o.path);
    names.insert(0, top);
    names.join(" › ")
}
