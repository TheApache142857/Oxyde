//! The look of the interface: colour tokens, widget styling, spacing and fonts.
//!
//! *Flux* (the default) is the dark navy and cyan of the KiCad Flux viewer; *Light* is its
//! daylight counterpart. Both use the system's interface font (Segoe UI on Windows) rather
//! than egui's bundled one.
//!
//! Colours drawn by hand elsewhere (icons, the timeline's badges, selection boxes, sketch
//! geometry over the viewport) come from [`tokens`] and [`view_dark`] so they follow the
//! theme.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Rounding, TextStyle,
};
use oxyde_render::Style;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTheme {
    #[default]
    Flux,
    Light,
}

impl UiTheme {
    pub const ALL: [(UiTheme, &'static str); 2] =
        [(UiTheme::Flux, "Flux (dark)"), (UiTheme::Light, "Light")];

    /// The viewport colours that go with it.
    pub fn viewport(self) -> Style {
        match self {
            UiTheme::Flux => FLUX_VIEWPORT,
            UiTheme::Light => Style::LIGHT,
        }
    }
}

/// The viewport in the Flux colours: a deep navy gradient, a cool grey part, cyan
/// selection and amber hover.
pub const FLUX_VIEWPORT: Style = Style {
    background_top: [34, 48, 63],
    background_bottom: [13, 20, 28],
    face: [168, 181, 196],
    face_selected: [79, 195, 247],
    face_hovered: [255, 183, 77],
    edge: [12, 17, 24],
    edge_selected: [79, 195, 247],
    edge_hovered: [255, 183, 77],
    ..Style::LIGHT
};

/// Named colours of a theme.
#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub bg: Color32,
    pub panel: Color32,
    pub panel2: Color32,
    pub line: Color32,
    pub fg: Color32,
    pub dim: Color32,
    pub accent: Color32,
    /// Accent behind selected things: a tab that is on, the active selection box.
    pub accent_bg: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub ok: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const FLUX: Tokens = Tokens {
    bg: hex(0x0f1720),
    panel: hex(0x16212c),
    panel2: hex(0x1c2a37),
    line: hex(0x2a3a4a),
    fg: hex(0xdfe7ee),
    dim: hex(0x8aa0b2),
    accent: hex(0x4fc3f7),
    accent_bg: hex(0x1f4a63),
    warn: hex(0xffb74d),
    err: hex(0xef5350),
    ok: hex(0x66bb6a),
};

pub const LIGHT: Tokens = Tokens {
    bg: hex(0xe9edf2),
    panel: hex(0xf5f7f9),
    panel2: hex(0xffffff),
    line: hex(0xd2dae3),
    fg: hex(0x1c2733),
    dim: hex(0x5f7183),
    accent: hex(0x1e88e5),
    accent_bg: hex(0xd6e9fb),
    warn: hex(0xc77700),
    err: hex(0xd32f2f),
    ok: hex(0x2e7d32),
};

static DARK_UI: AtomicBool = AtomicBool::new(true);
static DARK_VIEW: AtomicBool = AtomicBool::new(true);

/// The current theme's colours.
pub fn tokens() -> &'static Tokens {
    if DARK_UI.load(Ordering::Relaxed) {
        &FLUX
    } else {
        &LIGHT
    }
}

pub fn ui_dark() -> bool {
    DARK_UI.load(Ordering::Relaxed)
}

/// Whether the viewport background is dark, so that what is drawn over it (sketches,
/// planes, labels) uses light ink.
pub fn view_dark() -> bool {
    DARK_VIEW.load(Ordering::Relaxed)
}

/// Remembers the viewport's background, from its style.
pub fn set_viewport(style: &Style) {
    let lum = |c: [u8; 3]| 0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32;
    let mid = (lum(style.background_top) + lum(style.background_bottom)) / 2.0;
    DARK_VIEW.store(mid < 128.0, Ordering::Relaxed);
}

/// Styles the whole interface.
pub fn apply(ctx: &egui::Context, theme: UiTheme) {
    let dark = theme == UiTheme::Flux;
    DARK_UI.store(dark, Ordering::Relaxed);
    let t = if dark { FLUX } else { LIGHT };
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let radius = Rounding::same(6.0);
    v.panel_fill = t.panel;
    v.window_fill = t.panel;
    v.extreme_bg_color = t.bg;
    v.faint_bg_color = t.panel2;
    v.code_bg_color = t.panel2;
    v.window_stroke = egui::Stroke::new(1.0, t.line);
    v.window_rounding = Rounding::same(8.0);
    v.menu_rounding = radius;
    v.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(if dark { 110 } else { 40 }),
    };
    v.popup_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 12.0,
        spread: 0.0,
        color: Color32::from_black_alpha(if dark { 90 } else { 30 }),
    };
    v.hyperlink_color = t.accent;
    v.warn_fg_color = t.warn;
    v.error_fg_color = t.err;
    v.selection.bg_fill = t.accent_bg;
    v.selection.stroke = egui::Stroke::new(1.0, if dark { t.fg } else { t.accent });
    v.striped = true;
    v.slider_trailing_fill = true;

    let w = &mut v.widgets;
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.rounding = radius;
        state.expansion = 0.0;
    }
    w.noninteractive.bg_fill = t.panel;
    w.noninteractive.weak_bg_fill = t.panel;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0, t.line);
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0, t.fg);
    w.inactive.bg_fill = t.panel2;
    w.inactive.weak_bg_fill = t.panel2;
    w.inactive.bg_stroke = egui::Stroke::new(1.0, t.line);
    w.inactive.fg_stroke = egui::Stroke::new(1.0, t.fg);
    w.hovered.bg_fill = t.panel2;
    w.hovered.weak_bg_fill = t.panel2;
    w.hovered.bg_stroke = egui::Stroke::new(1.0, t.accent);
    w.hovered.fg_stroke = egui::Stroke::new(1.5, t.fg);
    w.active.bg_fill = t.accent_bg;
    w.active.weak_bg_fill = t.accent_bg;
    w.active.bg_stroke = egui::Stroke::new(1.0, t.accent);
    w.active.fg_stroke = egui::Stroke::new(1.5, t.fg);
    w.open.bg_fill = t.panel2;
    w.open.weak_bg_fill = t.panel2;
    w.open.bg_stroke = egui::Stroke::new(1.0, t.accent);
    w.open.fg_stroke = egui::Stroke::new(1.0, t.fg);
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 5.0);
        s.spacing.button_padding = egui::vec2(9.0, 4.0);
        s.spacing.interact_size.y = 24.0;
        s.spacing.window_margin = egui::Margin::same(12.0);
        s.spacing.menu_margin = egui::Margin::same(6.0);
        s.spacing.combo_width = 120.0;
        s.text_styles = [
            (
                TextStyle::Small,
                FontId::new(11.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(13.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Heading,
                FontId::new(17.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(12.5, FontFamily::Monospace),
            ),
        ]
        .into();
    });
}

/// Uses the system's interface and code fonts, keeping egui's own as fallbacks for the
/// symbols they lack (▾, ✓…).
pub fn load_fonts(ctx: &egui::Context) {
    let candidates: &[(&str, &[&str])] = &[
        (
            "ui",
            &[
                r"C:\Windows\Fonts\segoeui.ttf",
                "/System/Library/Fonts/Supplemental/Arial.ttf",
                "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            ],
        ),
        (
            "code",
            &[
                r"C:\Windows\Fonts\CascadiaMono.ttf",
                r"C:\Windows\Fonts\consola.ttf",
                "/System/Library/Fonts/Menlo.ttc",
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            ],
        ),
    ];
    let mut fonts = FontDefinitions::default();
    for (name, paths) in candidates {
        let Some(bytes) = paths.iter().find_map(|p| std::fs::read(p).ok()) else {
            continue;
        };
        fonts
            .font_data
            .insert((*name).to_owned(), FontData::from_owned(bytes));
        let family = if *name == "ui" {
            FontFamily::Proportional
        } else {
            FontFamily::Monospace
        };
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, (*name).to_owned());
    }
    ctx.set_fonts(fonts);
}
