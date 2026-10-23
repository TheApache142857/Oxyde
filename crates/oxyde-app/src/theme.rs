//! The look of the interface: colour tokens, widget styling, spacing and fonts.
//!
//! *Flux* (the default) is the dark navy and cyan of the KiCad Flux viewer; *Light* is its
//! daylight counterpart. *Gruvbox dark* and *Gruvbox light* are the warm "retro groove"
//! palette of the Gruvbox colour scheme, and *Classic 95* the grey, navy and teal of
//! Windows 95, with square corners and Microsoft Sans Serif where Windows has it. The others
//! use the system's interface font (Segoe UI on Windows) rather than egui's bundled one.
//!
//! Colours drawn by hand elsewhere (icons, the timeline's badges, selection boxes, sketch
//! geometry over the viewport) come from [`tokens`] and [`view_dark`] so they follow the
//! theme.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Rounding, TextStyle,
};
use oxyde_render::Style;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiTheme {
    #[default]
    Flux,
    Light,
    GruvboxDark,
    GruvboxLight,
    Classic95,
}

impl UiTheme {
    pub const ALL: [(UiTheme, &'static str); 5] = [
        (UiTheme::Flux, "Flux (dark)"),
        (UiTheme::Light, "Light"),
        (UiTheme::GruvboxDark, "Gruvbox dark"),
        (UiTheme::GruvboxLight, "Gruvbox light"),
        (UiTheme::Classic95, "Classic 95"),
    ];

    /// Its name in the preferences file.
    fn id(self) -> &'static str {
        match self {
            UiTheme::Flux => "flux",
            UiTheme::Light => "light",
            UiTheme::GruvboxDark => "gruvbox_dark",
            UiTheme::GruvboxLight => "gruvbox_light",
            UiTheme::Classic95 => "classic_95",
        }
    }

    /// The viewport colours that go with it.
    pub fn viewport(self) -> Style {
        match self {
            UiTheme::Flux => FLUX_VIEWPORT,
            UiTheme::Light => Style::LIGHT,
            UiTheme::GruvboxDark => GRUVBOX_DARK_VIEWPORT,
            UiTheme::GruvboxLight => GRUVBOX_LIGHT_VIEWPORT,
            UiTheme::Classic95 => CLASSIC_95_VIEWPORT,
        }
    }

    /// Its interface colours.
    pub fn tokens(self) -> &'static Tokens {
        match self {
            UiTheme::Flux => &FLUX,
            UiTheme::Light => &LIGHT,
            UiTheme::GruvboxDark => &GRUVBOX_DARK,
            UiTheme::GruvboxLight => &GRUVBOX_LIGHT,
            UiTheme::Classic95 => &CLASSIC_95,
        }
    }

    /// Whether its panels are dark, so that text and icons on them are light.
    pub fn is_dark(self) -> bool {
        matches!(self, UiTheme::Flux | UiTheme::GruvboxDark)
    }
}

impl Serialize for UiTheme {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}

/// A theme this build does not know (written by a newer one) reads as the default rather
/// than failing, which would lose every other preference with it.
impl<'de> Deserialize<'de> for UiTheme {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Ok(UiTheme::ALL
            .iter()
            .map(|(theme, _)| *theme)
            .find(|theme| theme.id() == id)
            .unwrap_or_default())
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

/// Gruvbox's dark viewport: a warm charcoal, a tan part, aqua selection and yellow hover.
pub const GRUVBOX_DARK_VIEWPORT: Style = Style {
    background_top: [50, 48, 47],
    background_bottom: [40, 40, 40],
    face: [168, 153, 132],
    face_selected: [131, 165, 152],
    face_hovered: [250, 189, 47],
    edge: [29, 32, 33],
    edge_selected: [131, 165, 152],
    edge_hovered: [250, 189, 47],
    ..Style::LIGHT
};

/// Gruvbox's light viewport: cream paper, the same tan part, blue selection and amber hover.
pub const GRUVBOX_LIGHT_VIEWPORT: Style = Style {
    background_top: [235, 219, 178],
    background_bottom: [251, 241, 199],
    face: [168, 153, 132],
    face_selected: [69, 133, 136],
    face_hovered: [215, 153, 33],
    edge: [60, 56, 54],
    edge_selected: [7, 102, 120],
    edge_hovered: [181, 118, 20],
    ..Style::LIGHT
};

/// Windows 95's teal desktop, a grey part, navy selection and yellow hover.
pub const CLASSIC_95_VIEWPORT: Style = Style {
    background_top: [0, 128, 128],
    background_bottom: [0, 128, 128],
    face: [192, 192, 192],
    face_selected: [64, 64, 192],
    face_hovered: [255, 255, 0],
    edge: [0, 0, 0],
    edge_selected: [0, 0, 128],
    edge_hovered: [255, 255, 0],
    ..Style::LIGHT
};

/// The themes' viewport colours, offered beside the renderer's own presets. (Light's are
/// the renderer's Light.)
pub const VIEWPORT_PRESETS: [(&str, Style); 4] = [
    ("Flux", FLUX_VIEWPORT),
    ("Gruvbox dark", GRUVBOX_DARK_VIEWPORT),
    ("Gruvbox light", GRUVBOX_LIGHT_VIEWPORT),
    ("Classic 95", CLASSIC_95_VIEWPORT),
];

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
    /// The outlines of the command icons, and their sketch blue.
    pub icon_ink: Color32,
    pub icon_sketch: Color32,
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
    icon_ink: hex(0xc4d2e0),
    icon_sketch: hex(0x4fc3f7),
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
    icon_ink: hex(0x2e3a46),
    icon_sketch: hex(0x1e6fd9),
};

/// Gruvbox's dark palette: cream on warm charcoal, the aqua of its blue as the accent.
pub const GRUVBOX_DARK: Tokens = Tokens {
    bg: hex(0x1d2021),
    panel: hex(0x282828),
    panel2: hex(0x3c3836),
    line: hex(0x504945),
    fg: hex(0xebdbb2),
    dim: hex(0xa89984),
    accent: hex(0x83a598),
    accent_bg: hex(0x48544f),
    warn: hex(0xfabd2f),
    err: hex(0xfb4934),
    icon_ink: hex(0xd5c4a1),
    icon_sketch: hex(0x83a598),
};

/// Gruvbox's light palette: dark brown on cream, its faded blue as the accent.
pub const GRUVBOX_LIGHT: Tokens = Tokens {
    bg: hex(0xf2e5bc),
    panel: hex(0xfbf1c7),
    panel2: hex(0xf9f5d7),
    line: hex(0xd5c4a1),
    fg: hex(0x3c3836),
    dim: hex(0x7c6f64),
    accent: hex(0x076678),
    accent_bg: hex(0xcfdcd0),
    warn: hex(0xb57614),
    err: hex(0x9d0006),
    icon_ink: hex(0x3c3836),
    icon_sketch: hex(0x076678),
};

/// Windows 95: black on button grey, white fields, navy accents.
pub const CLASSIC_95: Tokens = Tokens {
    bg: hex(0xffffff),
    panel: hex(0xc0c0c0),
    panel2: hex(0xd4d0c8),
    line: hex(0x808080),
    fg: hex(0x000000),
    dim: hex(0x505050),
    accent: hex(0x000080),
    accent_bg: hex(0xb9c3e6),
    warn: hex(0x8a5a00),
    err: hex(0xb00000),
    icon_ink: hex(0x000000),
    icon_sketch: hex(0x000080),
};

/// The theme in use, as its place in [`UiTheme::ALL`].
static THEME: AtomicU8 = AtomicU8::new(0);
static DARK_VIEW: AtomicBool = AtomicBool::new(true);

/// The theme in use.
pub fn current() -> UiTheme {
    let i = usize::from(THEME.load(Ordering::Relaxed));
    UiTheme::ALL
        .get(i)
        .map(|(theme, _)| *theme)
        .unwrap_or_default()
}

/// The current theme's colours.
pub fn tokens() -> &'static Tokens {
    current().tokens()
}

pub fn ui_dark() -> bool {
    current().is_dark()
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

/// Styles the whole interface, and loads the theme's fonts.
pub fn apply(ctx: &egui::Context, theme: UiTheme) {
    let index = UiTheme::ALL.iter().position(|(t, _)| *t == theme);
    THEME.store(
        index.and_then(|i| u8::try_from(i).ok()).unwrap_or(0),
        Ordering::Relaxed,
    );
    load_fonts(ctx, theme);
    let dark = theme.is_dark();
    // Windows 95 had square corners and hard shadows.
    let classic = theme == UiTheme::Classic95;
    let t = *theme.tokens();
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let radius = Rounding::same(if classic { 0.0 } else { 6.0 });
    v.panel_fill = t.panel;
    v.window_fill = t.panel;
    v.extreme_bg_color = t.bg;
    v.faint_bg_color = t.panel2;
    v.code_bg_color = t.panel2;
    v.window_stroke = egui::Stroke::new(1.0_f32, t.line);
    v.window_rounding = Rounding::same(if classic { 0.0 } else { 8.0 });
    v.menu_rounding = radius;
    v.window_shadow = if classic {
        egui::epaint::Shadow {
            offset: egui::vec2(2.0, 2.0),
            blur: 0.0,
            spread: 0.0,
            color: Color32::from_black_alpha(140),
        }
    } else {
        egui::epaint::Shadow {
            offset: egui::vec2(0.0, 8.0),
            blur: 24.0,
            spread: 0.0,
            color: Color32::from_black_alpha(if dark { 110 } else { 40 }),
        }
    };
    v.popup_shadow = if classic {
        egui::epaint::Shadow {
            offset: egui::vec2(2.0, 2.0),
            blur: 0.0,
            spread: 0.0,
            color: Color32::from_black_alpha(140),
        }
    } else {
        egui::epaint::Shadow {
            offset: egui::vec2(0.0, 4.0),
            blur: 12.0,
            spread: 0.0,
            color: Color32::from_black_alpha(if dark { 90 } else { 30 }),
        }
    };
    v.hyperlink_color = t.accent;
    v.warn_fg_color = t.warn;
    v.error_fg_color = t.err;
    v.selection.bg_fill = t.accent_bg;
    v.selection.stroke = egui::Stroke::new(1.0_f32, if dark { t.fg } else { t.accent });
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
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, t.line);
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, t.fg);
    w.inactive.bg_fill = t.panel2;
    w.inactive.weak_bg_fill = t.panel2;
    w.inactive.bg_stroke = egui::Stroke::new(1.0_f32, t.line);
    w.inactive.fg_stroke = egui::Stroke::new(1.0_f32, t.fg);
    w.hovered.bg_fill = t.panel2;
    w.hovered.weak_bg_fill = t.panel2;
    w.hovered.bg_stroke = egui::Stroke::new(1.0_f32, t.accent);
    w.hovered.fg_stroke = egui::Stroke::new(1.5_f32, t.fg);
    w.active.bg_fill = t.accent_bg;
    w.active.weak_bg_fill = t.accent_bg;
    w.active.bg_stroke = egui::Stroke::new(1.0_f32, t.accent);
    w.active.fg_stroke = egui::Stroke::new(1.5_f32, t.fg);
    w.open.bg_fill = t.panel2;
    w.open.weak_bg_fill = t.panel2;
    w.open.bg_stroke = egui::Stroke::new(1.0_f32, t.accent);
    w.open.fg_stroke = egui::Stroke::new(1.0_f32, t.fg);
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
/// symbols they lack (⏷, emoji…). A character none of them has draws as an empty box: the
/// test below checks every character in the interface's strings.
///
/// Classic 95 puts Windows 95's own interface font, Microsoft Sans Serif, in front, with
/// the usual one behind it for what it lacks.
pub fn load_fonts(ctx: &egui::Context, theme: UiTheme) {
    let mut candidates: Vec<(&str, &[&str])> = vec![
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
    if theme == UiTheme::Classic95 {
        candidates.push(("retro", &[r"C:\Windows\Fonts\micross.ttf"]));
    }
    let mut fonts = FontDefinitions::default();
    for (name, paths) in candidates {
        let Some(bytes) = paths.iter().find_map(|p| std::fs::read(p).ok()) else {
            continue;
        };
        fonts
            .font_data
            .insert(name.to_owned(), FontData::from_owned(bytes));
        let family = if name == "code" {
            FontFamily::Monospace
        } else {
            FontFamily::Proportional
        };
        // Each font goes in front of the ones before it.
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.to_owned());
    }
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The characters outside ASCII in `source`, outside comments, with their line numbers.
    /// A small scanner rather than a parser: it knows comments, strings, raw strings and
    /// character literals, which is all it takes to tell interface text from comments.
    fn literal_chars(source: &str) -> Vec<(usize, char)> {
        let chars: Vec<char> = source.chars().collect();
        let (mut out, mut line, mut i) = (Vec::new(), 1, 0);
        let at = |i: usize, s: &str| {
            s.chars()
                .enumerate()
                .all(|(k, c)| chars.get(i + k) == Some(&c))
        };
        while i < chars.len() {
            if at(i, "//") {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            if at(i, "/*") {
                while i < chars.len() && !at(i, "*/") {
                    line += usize::from(chars[i] == '\n');
                    i += 1;
                }
                i += 2;
                continue;
            }
            if at(i, "r#\"") || at(i, "r\"") {
                let hashes = usize::from(chars[i + 1] == '#');
                let end = if hashes == 1 { "\"#" } else { "\"" };
                i += 2 + hashes;
                while i < chars.len() && !at(i, end) {
                    line += usize::from(chars[i] == '\n');
                    if !chars[i].is_ascii() {
                        out.push((line, chars[i]));
                    }
                    i += 1;
                }
                i += end.len();
                continue;
            }
            match chars[i] {
                '"' => {
                    i += 1;
                    while i < chars.len() && chars[i] != '"' {
                        if chars[i] == '\\' {
                            i += 1;
                        }
                        line += usize::from(chars[i] == '\n');
                        if !chars[i].is_ascii() {
                            out.push((line, chars[i]));
                        }
                        i += 1;
                    }
                }
                // A character literal ('"', '\'', '°'), not a lifetime.
                '\'' if chars.get(i + 2) == Some(&'\'') || chars.get(i + 1) == Some(&'\\') => {
                    i += 1;
                    while i < chars.len() && chars[i] != '\'' {
                        if chars[i] == '\\' {
                            i += 1;
                        }
                        if !chars[i].is_ascii() {
                            out.push((line, chars[i]));
                        }
                        i += 1;
                    }
                }
                '\n' => line += 1,
                _ => {}
            }
            i += 1;
        }
        out
    }

    #[test]
    fn literal_chars_skip_comments() {
        let found = literal_chars("// °\nlet a = \"×\"; /* ² */ let b = '\"'; // µ\nlet c = '·';");
        assert_eq!(found, vec![(2, '×'), (3, '·')]);
    }

    /// Every character the interface writes has a glyph in the fonts `load_fonts` sets up,
    /// in both families and for each set of fonts: a missing one draws as an empty box (the
    /// PropertyManager's ▾ did).
    #[test]
    fn interface_characters_have_glyphs() {
        for theme in [UiTheme::Flux, UiTheme::Classic95] {
            characters_have_glyphs(theme);
        }
    }

    fn characters_have_glyphs(theme: UiTheme) {
        let ctx = egui::Context::default();
        load_fonts(&ctx, theme);
        // Fonts set with `set_fonts` take effect at the start of the next frame.
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "rs"))
            .collect();
        files.sort();
        let mut missing = Vec::new();
        for path in &files {
            let source = std::fs::read_to_string(path).unwrap();
            for (line, c) in literal_chars(&source) {
                for font in [FontId::proportional(13.0), FontId::monospace(12.5)] {
                    if !ctx.fonts(|f| f.has_glyph(&font, c)) {
                        let file = path.file_name().unwrap().to_string_lossy();
                        missing.push(format!(
                            "{file}:{line}: {c} (U+{:04X}) in {font:?}",
                            c as u32
                        ));
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "characters with no glyph in {theme:?}'s fonts:\n{}",
            missing.join("\n")
        );
    }

    /// Every theme is written to the preferences under its own name and read back, and a
    /// name this build does not know reads as the default.
    #[test]
    fn themes_round_trip_through_the_preferences() {
        for (theme, _) in UiTheme::ALL {
            let json = serde_json::to_string(&theme).unwrap();
            assert_eq!(serde_json::from_str::<UiTheme>(&json).unwrap(), theme);
        }
        assert_eq!(
            serde_json::to_string(&UiTheme::GruvboxDark).unwrap(),
            "\"gruvbox_dark\""
        );
        assert_eq!(
            serde_json::from_str::<UiTheme>("\"vaporwave\"").unwrap(),
            UiTheme::Flux
        );
    }
}
