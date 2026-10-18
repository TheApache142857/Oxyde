//! How the viewport looks: background, part and edge colours, highlight colours, edge
//! widths and the display style. Both renderers (software here, GPU in the application)
//! draw with the same [`Style`].

/// An sRGB colour, 0–255 per channel.
pub type Rgb = [u8; 3];

/// How the part is drawn, as SolidWorks' display styles (View › Display Style).
///
/// In every style the faces are still in the id buffers, so they can be picked, and the
/// edges too wherever they are drawn or could be (in *Shaded*, under the pointer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DisplayStyle {
    /// Shaded faces and their edges: the default.
    #[default]
    ShadedWithEdges,
    /// Shaded faces alone. An edge shows only when it is under the pointer or selected,
    /// so edges can still be picked.
    Shaded,
    /// Edges alone, those behind faces left out. The faces are not painted (but in the
    /// highlight colours), yet they hide what is behind them and can be picked.
    HiddenLinesRemoved,
    /// As *Hidden Lines Removed*, with the hidden edges drawn dashed and greyed.
    HiddenLinesVisible,
    /// Every edge, nothing hidden; the faces are not painted.
    Wireframe,
}

/// Which part of the edges, by depth, one drawing of them covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgePass {
    /// Where no face is in front of them: solid.
    Visible,
    /// Where a face is in front of them: dashed ([`HIDDEN_DASH`]), in the hidden-edge
    /// colour unless highlighted.
    Hidden,
    /// Everywhere, solid, whatever is in front.
    All,
}

/// The dashes of hidden edges: drawn and skipped length, in pixels. The pattern runs
/// along each edge's length in millimetres scaled to the screen, so it is continuous
/// along curves (and shorter on an edge seen at a slant).
pub const HIDDEN_DASH: (f32, f32) = (6.0, 4.0);

impl DisplayStyle {
    /// Every style with its name, in the order SolidWorks lists them.
    pub const ALL: [(DisplayStyle, &'static str); 5] = [
        (DisplayStyle::ShadedWithEdges, "Shaded With Edges"),
        (DisplayStyle::Shaded, "Shaded"),
        (DisplayStyle::HiddenLinesRemoved, "Hidden Lines Removed"),
        (DisplayStyle::HiddenLinesVisible, "Hidden Lines Visible"),
        (DisplayStyle::Wireframe, "Wireframe"),
    ];

    pub fn label(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(s, _)| *s == self)
            .map_or("", |(_, l)| l)
    }

    /// Whether the faces are painted, shaded. In the line styles only the hovered and
    /// selected faces are, in their highlight colour; the others leave the background.
    pub fn shades_faces(self) -> bool {
        matches!(self, DisplayStyle::ShadedWithEdges | DisplayStyle::Shaded)
    }

    /// Whether every edge is drawn, or only the hovered and selected ones (*Shaded*).
    /// Edges are picked the same either way.
    pub fn draws_all_edges(self) -> bool {
        self != DisplayStyle::Shaded
    }

    /// The drawings of the edges, in order, each one over the ones before.
    pub fn edge_passes(self) -> &'static [EdgePass] {
        match self {
            DisplayStyle::ShadedWithEdges
            | DisplayStyle::Shaded
            | DisplayStyle::HiddenLinesRemoved => &[EdgePass::Visible],
            DisplayStyle::HiddenLinesVisible => &[EdgePass::Hidden, EdgePass::Visible],
            DisplayStyle::Wireframe => &[EdgePass::All],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// Gradient from the top of the viewport to the bottom.
    pub background_top: Rgb,
    pub background_bottom: Rgb,
    pub face: Rgb,
    pub face_selected: Rgb,
    pub face_hovered: Rgb,
    pub edge: Rgb,
    pub edge_selected: Rgb,
    pub edge_hovered: Rgb,
    /// Shaded or lines, with or without edges.
    pub display: DisplayStyle,
    /// Edge width in pixels, and of a selected or hovered edge.
    pub edge_width: f32,
    pub highlight_width: f32,
    /// Strength of the part's reflection in a floor just under it, 0 (none) to 1: a
    /// mirrored, faded copy drawn below the part. Only in the shaded styles
    /// ([`Style::reflection`]).
    pub floor_reflection: f32,
    /// Ambient occlusion, 0 (none) to 1: creases, inner corners and the foot of a wall
    /// are darkened, since less ambient light reaches them, so the shape reads better
    /// than with the plain shading. Screen-space, from the depth buffer. Only in the
    /// shaded styles ([`Style::occlusion`]).
    pub ambient_occlusion: f32,
}

/// Brightness of an sRGB colour, 0..1, near enough to tell lines from a background.
fn luminance(c: Rgb) -> f32 {
    (0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])) / 255.0
}

/// How much brighter or darker than the background a line must be to be seen.
const LINE_CONTRAST: f32 = 0.3;

/// The colour half way between `a` and `b`.
fn halfway(a: Rgb, b: Rgb) -> Rgb {
    let mid = |x: u8, y: u8| ((u16::from(x) + u16::from(y)) / 2) as u8;
    [mid(a[0], b[0]), mid(a[1], b[1]), mid(a[2], b[2])]
}

impl Style {
    /// Light grey-blue, as SolidWorks: the default.
    pub const LIGHT: Style = Style {
        background_top: [205, 214, 228],
        background_bottom: [246, 247, 249],
        face: [182, 190, 202],
        face_selected: [96, 160, 255],
        face_hovered: [255, 178, 92],
        edge: [32, 36, 44],
        edge_selected: [0, 110, 255],
        edge_hovered: [255, 128, 0],
        display: DisplayStyle::ShadedWithEdges,
        edge_width: 1.25,
        highlight_width: 3.0,
        floor_reflection: 0.0,
        ambient_occlusion: 0.0,
    };

    /// Dark background, light part.
    pub const DARK: Style = Style {
        background_top: [28, 31, 38],
        background_bottom: [58, 63, 74],
        face: [168, 176, 188],
        face_selected: [80, 150, 255],
        face_hovered: [255, 170, 80],
        edge: [12, 14, 18],
        edge_selected: [60, 150, 255],
        edge_hovered: [255, 140, 20],
        ..Style::LIGHT
    };

    /// Plain white background, for screenshots and printing.
    pub const WHITE: Style = Style {
        background_top: [255, 255, 255],
        background_bottom: [255, 255, 255],
        face: [200, 205, 212],
        ..Style::LIGHT
    };

    /// The named presets, for a settings page.
    pub const PRESETS: [(&'static str, Style); 3] = [
        ("Light", Style::LIGHT),
        ("Dark", Style::DARK),
        ("White", Style::WHITE),
    ];

    /// The background half way down.
    fn background_middle(&self) -> Rgb {
        halfway(self.background_top, self.background_bottom)
    }

    /// The colour edges are drawn in. Over shaded faces, the edge colour; in the line
    /// styles the edges lie on the background instead, and the dark edges of a dark
    /// theme would vanish there, so they take the part's colour when it stands out more.
    pub fn edge_color(&self) -> Rgb {
        if self.display.shades_faces() {
            return self.edge;
        }
        let background = luminance(self.background_middle());
        let contrast = |c: Rgb| (luminance(c) - background).abs();
        if contrast(self.edge) < LINE_CONTRAST && contrast(self.face) > contrast(self.edge) {
            self.face
        } else {
            self.edge
        }
    }

    /// The colour of hidden edges (*Hidden Lines Visible*): the edge colour faded half
    /// way into the background.
    pub fn hidden_edge_color(&self) -> Rgb {
        halfway(self.edge_color(), self.background_middle())
    }

    /// The floor reflection's strength, 0..1: none in the line styles, which paint no
    /// faces to reflect.
    pub fn reflection(&self) -> f32 {
        if self.display.shades_faces() {
            self.floor_reflection.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// The ambient occlusion's strength, 0..1: none in the line styles, which shade no
    /// faces to darken.
    pub fn occlusion(&self) -> f32 {
        if self.display.shades_faces() {
            self.ambient_occlusion.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

impl Default for Style {
    fn default() -> Self {
        Style::LIGHT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_style_has_a_name_and_its_edge_passes() {
        for (style, name) in DisplayStyle::ALL {
            assert_eq!(style.label(), name);
            assert!(!style.edge_passes().is_empty());
        }
        assert_eq!(DisplayStyle::default(), DisplayStyle::ShadedWithEdges);
        // Only the hidden-line style draws hidden edges, and before the visible ones so
        // those cover them.
        let hidden: Vec<DisplayStyle> = DisplayStyle::ALL
            .iter()
            .map(|(s, _)| *s)
            .filter(|s| s.edge_passes().contains(&EdgePass::Hidden))
            .collect();
        assert_eq!(hidden, vec![DisplayStyle::HiddenLinesVisible]);
        assert_eq!(
            DisplayStyle::HiddenLinesVisible.edge_passes(),
            &[EdgePass::Hidden, EdgePass::Visible]
        );
        assert!(DisplayStyle::Shaded.shades_faces() && !DisplayStyle::Shaded.draws_all_edges());
        assert!(!DisplayStyle::Wireframe.shades_faces());
    }

    #[test]
    fn lines_stand_out_from_the_background() {
        // On a light background the dark edge colour is kept, in every style.
        for (display, _) in DisplayStyle::ALL {
            let light = Style {
                display,
                ..Style::LIGHT
            };
            assert_eq!(light.edge_color(), Style::LIGHT.edge);
        }
        // On a dark one, lines drawn on the background take the light part colour;
        // over shaded faces the edges stay dark.
        let dark = Style {
            display: DisplayStyle::HiddenLinesRemoved,
            ..Style::DARK
        };
        assert_eq!(dark.edge_color(), Style::DARK.face);
        assert_eq!(Style::DARK.edge_color(), Style::DARK.edge);
        // Hidden edges sit between the lines and the background.
        let hidden = Style {
            display: DisplayStyle::HiddenLinesVisible,
            ..Style::LIGHT
        };
        let [e, h, b] = [
            hidden.edge_color(),
            hidden.hidden_edge_color(),
            hidden.background_middle(),
        ]
        .map(luminance);
        assert!(e < h && h < b, "{e} {h} {b}");
    }

    #[test]
    fn line_styles_have_no_reflection_or_occlusion() {
        let shaded = Style {
            floor_reflection: 0.4,
            ambient_occlusion: 0.6,
            ..Style::LIGHT
        };
        assert_eq!((shaded.reflection(), shaded.occlusion()), (0.4, 0.6));
        let lines = Style {
            display: DisplayStyle::Wireframe,
            ..shaded
        };
        assert_eq!((lines.reflection(), lines.occlusion()), (0.0, 0.0));
    }
}
