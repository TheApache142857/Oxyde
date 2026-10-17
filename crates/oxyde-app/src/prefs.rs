//! User preferences: viewport graphics, navigation, keyboard shortcuts and debugging aids,
//! saved as JSON in the user's configuration folder (`%APPDATA%\Oxyde\preferences.json`
//! on Windows, `~/.config/oxyde/preferences.json` elsewhere).
//!
//! Every field has a default, so a file from an older or newer version still loads: what
//! it lacks takes the default, what this version does not know is ignored.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use oxyde_render::{Rgb, Style};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub graphics: Graphics,
    pub navigation: Navigation,
    pub previews: Previews,
    pub debug: Debug,
    /// Shortcuts changed from their defaults, by action id: a key combination such as
    /// `"Ctrl+Shift+Z"`, or `""` for none.
    pub shortcuts: BTreeMap<String, String>,
    /// Documents opened or saved lately, newest first.
    pub recent: Vec<PathBuf>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            graphics: Graphics::default(),
            navigation: Navigation::default(),
            previews: Previews::default(),
            debug: Debug::default(),
            shortcuts: BTreeMap::new(),
            recent: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Renderer {
    Gpu,
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshQuality {
    Coarse,
    Normal,
    Fine,
    VeryFine,
}

impl MeshQuality {
    pub const ALL: [(MeshQuality, &'static str); 4] = [
        (MeshQuality::Coarse, "Coarse (fastest)"),
        (MeshQuality::Normal, "Normal"),
        (MeshQuality::Fine, "Fine"),
        (MeshQuality::VeryFine, "Very fine (slowest)"),
    ];

    /// Mesh deflection as a fraction of the part's size.
    pub fn fraction(self) -> f64 {
        match self {
            MeshQuality::Coarse => 0.004,
            MeshQuality::Normal => 0.001,
            MeshQuality::Fine => 0.0004,
            MeshQuality::VeryFine => 0.00015,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Graphics {
    /// The interface's colours (and the viewport's, when changed).
    pub ui_theme: crate::theme::UiTheme,
    /// Takes effect at the next start.
    pub renderer: Renderer,
    /// 4× multisampling on the GPU; takes effect at the next start.
    pub antialiasing: bool,
    pub background_top: Rgb,
    pub background_bottom: Rgb,
    pub face: Rgb,
    pub face_selected: Rgb,
    pub face_hovered: Rgb,
    pub edge: Rgb,
    pub edge_selected: Rgb,
    pub edge_hovered: Rgb,
    pub show_edges: bool,
    pub edge_width: f32,
    pub highlight_width: f32,
    pub mesh_quality: MeshQuality,
    /// The small X/Y/Z triad in the corner.
    pub show_triad: bool,
    /// The part's reflection in a floor under it (View › Floor reflections).
    pub floor_reflections: bool,
    /// How strong the reflection is, 0..1.
    pub reflection_strength: f32,
    /// Ambient occlusion (View › Ambient occlusion): creases and inner corners darkened,
    /// so the shape reads better.
    #[serde(alias = "subsurface_scattering")]
    pub ambient_occlusion: bool,
    /// How strong it is, 0..1.
    #[serde(alias = "subsurface_strength")]
    pub ambient_occlusion_strength: f32,
}

impl Default for Graphics {
    fn default() -> Self {
        let mut g = Self {
            ui_theme: crate::theme::UiTheme::default(),
            renderer: Renderer::Gpu,
            antialiasing: true,
            background_top: [0; 3],
            background_bottom: [0; 3],
            face: [0; 3],
            face_selected: [0; 3],
            face_hovered: [0; 3],
            edge: [0; 3],
            edge_selected: [0; 3],
            edge_hovered: [0; 3],
            show_edges: true,
            edge_width: 0.0,
            highlight_width: 0.0,
            mesh_quality: MeshQuality::Normal,
            show_triad: true,
            floor_reflections: true,
            reflection_strength: 0.35,
            ambient_occlusion: true,
            ambient_occlusion_strength: 0.6,
        };
        g.set_style(&g.ui_theme.viewport());
        g
    }
}

impl Graphics {
    pub fn style(&self) -> Style {
        Style {
            background_top: self.background_top,
            background_bottom: self.background_bottom,
            face: self.face,
            face_selected: self.face_selected,
            face_hovered: self.face_hovered,
            edge: self.edge,
            edge_selected: self.edge_selected,
            edge_hovered: self.edge_hovered,
            show_edges: self.show_edges,
            edge_width: self.edge_width.clamp(0.5, 8.0),
            highlight_width: self.highlight_width.clamp(0.5, 12.0),
            floor_reflection: if self.floor_reflections {
                self.reflection_strength.clamp(0.0, 1.0)
            } else {
                0.0
            },
            ambient_occlusion: if self.ambient_occlusion {
                self.ambient_occlusion_strength.clamp(0.0, 1.0)
            } else {
                0.0
            },
        }
    }

    /// Takes the colours and edges of `style` (a preset).
    pub fn set_style(&mut self, style: &Style) {
        self.background_top = style.background_top;
        self.background_bottom = style.background_bottom;
        self.face = style.face;
        self.face_selected = style.face_selected;
        self.face_hovered = style.face_hovered;
        self.edge = style.edge;
        self.edge_selected = style.edge_selected;
        self.edge_hovered = style.edge_hovered;
        self.show_edges = style.show_edges;
        self.edge_width = style.edge_width;
        self.highlight_width = style.highlight_width;
    }
}

/// Which mouse button turns the view. The left button always selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseScheme {
    /// Right-drag orbits, middle-drag (or Shift+right-drag) pans; left-drag draws a
    /// selection box.
    #[default]
    RightTurns,
    /// SolidWorks: middle-drag orbits, Ctrl+middle-drag or right-drag pans; left-drag draws
    /// a selection box.
    MiddleTurns,
    /// Left-drag orbits; right-drag, middle-drag or Shift+left-drag pans. No selection box.
    LeftTurns,
}

impl MouseScheme {
    pub const ALL: [(MouseScheme, &'static str); 3] = [
        (MouseScheme::RightTurns, "Right button turns (Creo-like)"),
        (MouseScheme::MiddleTurns, "Middle button turns (SolidWorks)"),
        (MouseScheme::LeftTurns, "Left button turns"),
    ];

    /// How the buttons are used, for the help table: orbit, pan, select, and in sketch mode.
    pub fn help(self) -> [(&'static str, &'static str); 4] {
        match self {
            MouseScheme::RightTurns => [
                ("Orbit", "right-drag, about the point under the pointer"),
                ("Pan", "middle-drag, or Shift+right-drag"),
                (
                    "Select",
                    "click; Ctrl+click or Shift+click adds; drag a box (left to right: inside it, right to left: touching it)",
                ),
                ("Sketch mode", "right-drag orbits, middle-drag pans"),
            ],
            MouseScheme::MiddleTurns => [
                ("Orbit", "middle-drag, about the point under the pointer"),
                ("Pan", "Ctrl+middle-drag, or right-drag"),
                (
                    "Select",
                    "click; Ctrl+click or Shift+click adds; drag a box (left to right: inside it, right to left: touching it)",
                ),
                (
                    "Sketch mode",
                    "Ctrl+middle-drag orbits, middle-drag or right-drag pans",
                ),
            ],
            MouseScheme::LeftTurns => [
                ("Orbit", "left-drag, about the point under the pointer"),
                ("Pan", "right-drag, middle-drag, or Shift+left-drag"),
                ("Select", "click; Ctrl+click or Shift+click adds"),
                (
                    "Sketch mode",
                    "right-drag or middle-drag pans, Ctrl+middle-drag orbits",
                ),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Navigation {
    /// Which button turns the view.
    pub mouse: MouseScheme,
    /// 1 = default speed.
    pub orbit_speed: f64,
    pub zoom_speed: f64,
    /// Wheel forward zooms out (as in SolidWorks by default) rather than in.
    pub invert_zoom: bool,
    /// Orbit about the point under the pointer where the drag started, rather than the
    /// centre of the view.
    pub orbit_about_pointer: bool,
    /// Keep the model's Y axis up the screen while orbiting, as SolidWorks does (View ›
    /// Constrained orbit). Off, the view turns freely about the screen's axes and can be
    /// tilted, as Fusion 360's free orbit.
    pub constrained_orbit: bool,
    /// Standard views and Normal To turn the camera smoothly.
    pub animate_views: bool,
    /// Seconds.
    pub animation_time: f64,
}

impl Default for Navigation {
    fn default() -> Self {
        Self {
            mouse: MouseScheme::default(),
            orbit_speed: 1.0,
            zoom_speed: 1.0,
            invert_zoom: false,
            orbit_about_pointer: true,
            constrained_orbit: true,
            animate_views: true,
            animation_time: 0.45,
        }
    }
}

/// What commands show in the viewport before OK.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Previews {
    /// Feature commands show the part as the feature would make it.
    pub features: bool,
    /// The Mate command moves the parts as mates are picked, rather than on OK.
    pub mates: bool,
}

impl Default for Previews {
    fn default() -> Self {
        Self {
            features: true,
            mates: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Debug {
    /// Frame time, renderer, triangle and edge counts, rebuild and solve times.
    pub performance_overlay: bool,
    /// What is under the pointer: face or edge index and persistent name.
    pub hover_info: bool,
    /// egui's own debugging: widget outlines and sizes on hover.
    pub ui_debug_on_hover: bool,
    /// egui's settings and inspection windows.
    pub ui_inspector: bool,
}

/// Where the preferences file lives.
pub fn path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Oxyde"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|d| d.join("oxyde"))
    };
    base.map(|d| d.join("preferences.json"))
}

impl Preferences {
    /// The saved preferences, or the defaults when there are none or they cannot be read.
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<PathBuf, String> {
        let path = path().ok_or("no configuration folder")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// The key combination of an action: the user's, else its default.
    pub fn binding(&self, action: Action) -> Option<Binding> {
        match self.shortcuts.get(action.id()) {
            Some(text) => Binding::parse(text),
            None => action.default_binding(),
        }
    }

    pub fn set_binding(&mut self, action: Action, binding: Option<Binding>) {
        if binding == action.default_binding() {
            self.shortcuts.remove(action.id());
        } else {
            let text = binding.map(|b| b.to_string()).unwrap_or_default();
            self.shortcuts.insert(action.id().to_owned(), text);
        }
    }

    /// Whether `action`'s shortcut was pressed this frame (and takes the key press).
    /// Plain keys (no Ctrl or Alt) are left alone while a text field has the focus.
    pub fn pressed(&self, ctx: &egui::Context, action: Action) -> bool {
        let Some(binding) = self.binding(action) else {
            return false;
        };
        if !binding.ctrl && !binding.alt && ctx.wants_keyboard_input() {
            return false;
        }
        ctx.input_mut(|i| i.consume_shortcut(&binding.shortcut()))
    }

    /// Actions whose shortcut is also another action's, in the same context.
    pub fn conflicts(&self, action: Action) -> Vec<Action> {
        let Some(binding) = self.binding(action) else {
            return Vec::new();
        };
        Action::ALL
            .iter()
            .copied()
            .filter(|other| *other != action)
            .filter(|other| action.context().overlaps(other.context()))
            .filter(|other| self.binding(*other) == Some(binding))
            .collect()
    }
}

/// A key with modifiers. Ctrl is the Command key on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Binding {
    pub const fn new(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    pub const fn ctrl(key: Key) -> Self {
        Self {
            ctrl: true,
            ..Self::new(key)
        }
    }

    pub const fn ctrl_shift(key: Key) -> Self {
        Self {
            ctrl: true,
            shift: true,
            ..Self::new(key)
        }
    }

    pub fn modifiers(&self) -> Modifiers {
        let mut m = Modifiers::NONE;
        if self.ctrl {
            m = m | Modifiers::COMMAND;
        }
        if self.shift {
            m = m | Modifiers::SHIFT;
        }
        if self.alt {
            m = m | Modifiers::ALT;
        }
        m
    }

    pub fn shortcut(&self) -> KeyboardShortcut {
        KeyboardShortcut::new(self.modifiers(), self.key)
    }

    /// "Ctrl+Shift+Z", "F", "Delete"…
    pub fn parse(text: &str) -> Option<Self> {
        let mut binding = Binding::new(Key::Escape);
        let mut key = None;
        for part in text.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "" => return None,
                "ctrl" | "cmd" | "command" => binding.ctrl = true,
                "shift" => binding.shift = true,
                "alt" => binding.alt = true,
                _ => key = Some(Key::from_name(part)?),
            }
        }
        binding.key = key?;
        Some(binding)
    }

    /// The combination being pressed now, if a key went down this frame.
    pub fn captured(ctx: &egui::Context) -> Option<Self> {
        ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => Some(Binding {
                    key: *key,
                    ctrl: modifiers.command || modifiers.ctrl,
                    shift: modifiers.shift,
                    alt: modifiers.alt,
                }),
                _ => None,
            })
        })
    }
}

impl std::fmt::Display for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ctrl {
            write!(f, "Ctrl+")?;
        }
        if self.shift {
            write!(f, "Shift+")?;
        }
        if self.alt {
            write!(f, "Alt+")?;
        }
        write!(f, "{}", self.key.name())
    }
}

/// Where a shortcut works: two actions may share a key only if they never both apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    Everywhere,
    Part,
    Sketch,
    Assembly,
}

impl Context {
    fn overlaps(self, other: Context) -> bool {
        self == other || self == Context::Everywhere || other == Context::Everywhere
    }
}

/// Everything a shortcut can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    NewPart,
    Open,
    Save,
    Preferences,
    Undo,
    Redo,
    RedoAlternate,
    Delete,
    Rebuild,
    ZoomToFit,
    ViewFront,
    ViewTop,
    ViewRight,
    ViewIsometric,
    NormalTo,
    Sketch,
    Extrude,
    Revolve,
    Sweep,
    Fillet,
    Chamfer,
    Shell,
    LinearPattern,
    CircularPattern,
    Mirror,
    SmartDimension,
    Line,
    Centerline,
    Rectangle,
    Circle,
    Arc,
    Trim,
    InsertComponent,
    Mate,
}

impl Action {
    pub const ALL: [Action; 34] = [
        Action::NewPart,
        Action::Open,
        Action::Save,
        Action::Preferences,
        Action::Undo,
        Action::Redo,
        Action::RedoAlternate,
        Action::Delete,
        Action::Rebuild,
        Action::ZoomToFit,
        Action::ViewFront,
        Action::ViewTop,
        Action::ViewRight,
        Action::ViewIsometric,
        Action::NormalTo,
        Action::Sketch,
        Action::Extrude,
        Action::Revolve,
        Action::Sweep,
        Action::Fillet,
        Action::Chamfer,
        Action::Shell,
        Action::LinearPattern,
        Action::CircularPattern,
        Action::Mirror,
        Action::SmartDimension,
        Action::Line,
        Action::Centerline,
        Action::Rectangle,
        Action::Circle,
        Action::Arc,
        Action::Trim,
        Action::InsertComponent,
        Action::Mate,
    ];

    /// Stable name in the preferences file.
    pub fn id(self) -> &'static str {
        match self {
            Action::NewPart => "file.new_part",
            Action::Open => "file.open",
            Action::Save => "file.save",
            Action::Preferences => "file.preferences",
            Action::Undo => "edit.undo",
            Action::Redo => "edit.redo",
            Action::RedoAlternate => "edit.redo_alternate",
            Action::Delete => "edit.delete",
            Action::Rebuild => "edit.rebuild",
            Action::ZoomToFit => "view.zoom_to_fit",
            Action::ViewFront => "view.front",
            Action::ViewTop => "view.top",
            Action::ViewRight => "view.right",
            Action::ViewIsometric => "view.isometric",
            Action::NormalTo => "view.normal_to",
            Action::Sketch => "features.sketch",
            Action::Extrude => "features.extrude",
            Action::Revolve => "features.revolve",
            Action::Sweep => "features.sweep",
            Action::Fillet => "features.fillet",
            Action::Chamfer => "features.chamfer",
            Action::Shell => "features.shell",
            Action::LinearPattern => "features.linear_pattern",
            Action::CircularPattern => "features.circular_pattern",
            Action::Mirror => "features.mirror",
            Action::SmartDimension => "sketch.smart_dimension",
            Action::Line => "sketch.line",
            Action::Centerline => "sketch.centerline",
            Action::Rectangle => "sketch.rectangle",
            Action::Circle => "sketch.circle",
            Action::Arc => "sketch.arc",
            Action::Trim => "sketch.trim",
            Action::InsertComponent => "assembly.insert_component",
            Action::Mate => "assembly.mate",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::NewPart => "New part",
            Action::Open => "Open",
            Action::Save => "Save",
            Action::Preferences => "Preferences",
            Action::Undo => "Undo",
            Action::Redo => "Redo",
            Action::RedoAlternate => "Redo (alternate)",
            Action::Delete => "Delete selection",
            Action::Rebuild => "Rebuild / reload parts",
            Action::ZoomToFit => "Zoom to fit",
            Action::ViewFront => "Front view",
            Action::ViewTop => "Top view",
            Action::ViewRight => "Right view",
            Action::ViewIsometric => "Isometric view",
            Action::NormalTo => "Normal To",
            Action::Sketch => "Sketch",
            Action::Extrude => "Extrude",
            Action::Revolve => "Revolve",
            Action::Sweep => "Sweep",
            Action::Fillet => "Fillet",
            Action::Chamfer => "Chamfer",
            Action::Shell => "Shell",
            Action::LinearPattern => "Linear Pattern",
            Action::CircularPattern => "Circular Pattern",
            Action::Mirror => "Mirror",
            Action::SmartDimension => "Smart Dimension",
            Action::Line => "Line",
            Action::Centerline => "Centerline",
            Action::Rectangle => "Rectangle",
            Action::Circle => "Circle",
            Action::Arc => "Arc",
            Action::Trim => "Trim",
            Action::InsertComponent => "Insert Component",
            Action::Mate => "Mate",
        }
    }

    pub fn category(self) -> &'static str {
        match self.id().split('.').next() {
            Some("file") => "File",
            Some("edit") => "Edit",
            Some("view") => "View",
            Some("features") => "Features",
            Some("sketch") => "Sketch",
            _ => "Assembly",
        }
    }

    pub fn context(self) -> Context {
        match self.category() {
            "Features" => Context::Part,
            "Sketch" => Context::Sketch,
            "Assembly" => Context::Assembly,
            _ => Context::Everywhere,
        }
    }

    /// SolidWorks' defaults where it has one.
    pub fn default_binding(self) -> Option<Binding> {
        Some(match self {
            Action::NewPart => Binding::ctrl(Key::N),
            Action::Open => Binding::ctrl(Key::O),
            Action::Save => Binding::ctrl(Key::S),
            Action::Preferences => Binding::ctrl(Key::Comma),
            Action::Undo => Binding::ctrl(Key::Z),
            Action::Redo => Binding::ctrl(Key::Y),
            Action::RedoAlternate => Binding::ctrl_shift(Key::Z),
            Action::Delete => Binding::new(Key::Delete),
            Action::Rebuild => Binding::ctrl(Key::B),
            Action::ZoomToFit => Binding::new(Key::F),
            Action::ViewFront => Binding::ctrl(Key::Num1),
            Action::ViewTop => Binding::ctrl(Key::Num5),
            Action::ViewRight => Binding::ctrl(Key::Num4),
            Action::ViewIsometric => Binding::ctrl(Key::Num7),
            Action::NormalTo => Binding::ctrl(Key::Num8),
            Action::SmartDimension => Binding::new(Key::D),
            Action::Line => Binding::new(Key::L),
            Action::Rectangle => Binding::new(Key::R),
            Action::Circle => Binding::new(Key::C),
            Action::Arc => Binding::new(Key::A),
            Action::Trim => Binding::new(Key::T),
            Action::Mate => Binding::new(Key::M),
            _ => return None,
        })
    }
}
