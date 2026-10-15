//! How the viewport looks: background, part and edge colours, highlight colours and edge
//! widths. Both renderers (software here, GPU in the application) draw with the same
//! [`Style`].

/// An sRGB colour, 0–255 per channel.
pub type Rgb = [u8; 3];

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
    /// Draw the part's edges (they can still be picked when hidden only if shown).
    pub show_edges: bool,
    /// Edge width in pixels, and of a selected or hovered edge.
    pub edge_width: f32,
    pub highlight_width: f32,
    /// Strength of the part's reflection in a floor just under it, 0 (none) to 1: a
    /// mirrored, faded copy drawn below the part.
    pub floor_reflection: f32,
    /// Subsurface scattering, 0 (none) to 1: light wraps round the shadow side and glows
    /// through near the silhouette, in the material's colour, so parts look like wax,
    /// resin or printed plastic rather than painted metal.
    pub subsurface: f32,
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
        show_edges: true,
        edge_width: 1.25,
        highlight_width: 3.0,
        floor_reflection: 0.0,
        subsurface: 0.0,
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
}

impl Default for Style {
    fn default() -> Self {
        Style::LIGHT
    }
}
