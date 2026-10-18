//! Drawings: 2D views of a model laid out on a sheet with a frame and a title block, as
//! SolidWorks' standard views, written as PDF, SVG or DXF. The views' edges come from
//! the kernel's hidden line removal (`Kernel::project_edges`); this module lays them out
//! (first or third angle projection, ISO A sheets, a standard scale that fits) and writes
//! them, with ISO 128 line weights: visible edges thick, hidden ones thin and dashed (on
//! the orthographic views; the isometric one shows the visible edges only).
//!
//! The sheet's coordinates are millimetres from its lower left corner, y up. PDF and SVG
//! are the sheet at its size; DXF is the sheet divided by its scale, so that the views'
//! geometry has its true size there (for CAM, laser and waterjet programs), the frame and
//! the title block around it scaled alike.

use std::fmt::Write as _;

/// A box, `(min, max)`.
type Bounds = ([f64; 2], [f64; 2]);

/// A view's edges, in mm at full size, in the view's own frame (x to the right, y up).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ViewEdges {
    pub visible: Vec<Vec<[f64; 2]>>,
    pub hidden: Vec<Vec<[f64; 2]>>,
}

impl ViewEdges {
    /// The smallest box holding every edge, `(min, max)`; `None` for a view with none.
    pub fn bounds(&self) -> Option<([f64; 2], [f64; 2])> {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for [x, y] in self.visible.iter().chain(&self.hidden).flatten() {
            lo = [lo[0].min(*x), lo[1].min(*y)];
            hi = [hi[0].max(*x), hi[1].max(*y)];
        }
        lo[0].is_finite().then_some((lo, hi))
    }
}

/// The views of a standard drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    /// Looking along −Z, X to the right (SolidWorks' Front).
    Front,
    /// Looking down along −Y, X to the right.
    Top,
    /// Looking along −X, −Z to the right.
    Right,
    /// From the front, the right and above, X and −Z going down sideways.
    Isometric,
}

impl ViewKind {
    pub const ALL: [ViewKind; 4] = [
        ViewKind::Front,
        ViewKind::Top,
        ViewKind::Right,
        ViewKind::Isometric,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ViewKind::Front => "Front",
            ViewKind::Top => "Top",
            ViewKind::Right => "Right",
            ViewKind::Isometric => "Isometric",
        }
    }

    /// The plane the view looks at the model along (for `Kernel::project_edges`): its
    /// normal towards the eye, its x to the right, through the model origin.
    pub fn plane(self) -> oxyde_geom::Plane {
        use oxyde_geom::{Plane, Point3, Vec3};
        match self {
            ViewKind::Front => Plane::FRONT,
            ViewKind::Top => Plane::TOP,
            ViewKind::Right => Plane::RIGHT,
            ViewKind::Isometric => {
                let s = 1.0 / 3f64.sqrt();
                let h = std::f64::consts::FRAC_1_SQRT_2;
                Plane {
                    origin: Point3::ORIGIN,
                    x_dir: Vec3::new(h, 0.0, -h),
                    normal: Vec3::new(s, s, s),
                }
            }
        }
    }
}

/// Today's date, `2026-10-18`, from the system clock (UTC), for title blocks.
pub fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Where the views go around the front view (ISO 5456-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    /// European: the top view under the front view, the right view on its left.
    FirstAngle,
    /// American: the top view above the front view, the right view on its right.
    ThirdAngle,
}

/// ISO 216 sheets, landscape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetSize {
    A4,
    A3,
    A2,
    A1,
    A0,
}

impl SheetSize {
    pub const ALL: [SheetSize; 5] = [
        SheetSize::A4,
        SheetSize::A3,
        SheetSize::A2,
        SheetSize::A1,
        SheetSize::A0,
    ];

    /// Width and height, mm.
    pub fn size(self) -> [f64; 2] {
        match self {
            SheetSize::A4 => [297.0, 210.0],
            SheetSize::A3 => [420.0, 297.0],
            SheetSize::A2 => [594.0, 420.0],
            SheetSize::A1 => [841.0, 594.0],
            SheetSize::A0 => [1189.0, 841.0],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SheetSize::A4 => "A4",
            SheetSize::A3 => "A3",
            SheetSize::A2 => "A2",
            SheetSize::A1 => "A1",
            SheetSize::A0 => "A0",
        }
    }
}

/// What the title block says.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TitleBlock {
    pub title: String,
    pub part_number: String,
    pub material: String,
    pub drawn_by: String,
    pub date: String,
}

/// A view where the sheet has it: a sheet point is `origin + scale * p` for the point `p`
/// of the view.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedView {
    pub kind: ViewKind,
    pub edges: ViewEdges,
    pub origin: [f64; 2],
}

/// A drawing sheet, ready to write.
#[derive(Debug, Clone, PartialEq)]
pub struct Sheet {
    pub size: SheetSize,
    pub projection: Projection,
    /// Sheet mm per model mm: 0.5 for 1:2.
    pub scale: f64,
    pub views: Vec<PlacedView>,
    pub title: TitleBlock,
    pub show_hidden: bool,
}

/// ISO 5455 scales, largest first.
pub const SCALES: [f64; 13] = [
    10.0, 5.0, 2.0, 1.0, 0.5, 0.2, 0.1, 0.05, 0.02, 0.01, 0.005, 0.002, 0.001,
];

/// "2:1", "1:1", "1:5".
pub fn scale_text(scale: f64) -> String {
    let trim = |v: f64| {
        let text = format!("{v:.3}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    };
    if scale >= 1.0 {
        format!("{}:1", trim(scale))
    } else {
        format!("1:{}", trim(1.0 / scale))
    }
}

/// From the sheet's edge to its frame, mm.
const MARGIN: f64 = 10.0;
/// The title block, mm.
const TITLE_WIDTH: f64 = 180.0;
const TITLE_HEIGHT: f64 = 30.0;
/// Between views, on the sheet, mm.
const GAP: f64 = 15.0;
/// Inside the frame, around the views, mm.
const PADDING: f64 = 8.0;

/// The cell of the 2 × 2 grid a view takes (column from the left, row from the top).
fn cell(kind: ViewKind, projection: Projection) -> (usize, usize) {
    match (projection, kind) {
        (Projection::FirstAngle, ViewKind::Right) => (0, 0),
        (Projection::FirstAngle, ViewKind::Front) => (1, 0),
        (Projection::FirstAngle, ViewKind::Isometric) => (0, 1),
        (Projection::FirstAngle, ViewKind::Top) => (1, 1),
        (Projection::ThirdAngle, ViewKind::Top) => (0, 0),
        (Projection::ThirdAngle, ViewKind::Isometric) => (1, 0),
        (Projection::ThirdAngle, ViewKind::Front) => (0, 1),
        (Projection::ThirdAngle, ViewKind::Right) => (1, 1),
    }
}

/// Lays the views out on a sheet: the front view, the top and right views where the
/// projection puts them (lined up with it), the isometric view in the corner left, at
/// the largest standard scale they all fit at (`scale`: that one instead).
pub fn layout(
    views: Vec<(ViewKind, ViewEdges)>,
    size: SheetSize,
    projection: Projection,
    scale: Option<f64>,
    title: TitleBlock,
    show_hidden: bool,
) -> Sheet {
    let [width, height] = size.size();
    // The room for views: inside the frame, above the title block.
    let (x0, x1) = (MARGIN + PADDING, width - MARGIN - PADDING);
    let (y0, y1) = (MARGIN + TITLE_HEIGHT + PADDING, height - MARGIN - PADDING);
    let mut columns = [0.0f64; 2];
    let mut rows = [0.0f64; 2];
    let mut used = ([false; 2], [false; 2]);
    let views: Vec<(ViewKind, ViewEdges, Bounds)> = views
        .into_iter()
        .filter_map(|(kind, edges)| {
            let bounds = edges.bounds()?;
            Some((kind, edges, bounds))
        })
        .collect();
    for (kind, _, (lo, hi)) in &views {
        let (c, r) = cell(*kind, projection);
        if let Some(column) = columns.get_mut(c) {
            *column = column.max(hi[0] - lo[0]);
        }
        if let Some(u) = used.0.get_mut(c) {
            *u = true;
        }
        if let Some(row) = rows.get_mut(r) {
            *row = row.max(hi[1] - lo[1]);
        }
        if let Some(u) = used.1.get_mut(r) {
            *u = true;
        }
    }
    let gaps = |u: [bool; 2]| if u[0] && u[1] { GAP } else { 0.0 };
    let (gap_x, gap_y) = (gaps(used.0), gaps(used.1));
    let fits = |s: f64| {
        s * (columns[0] + columns[1]) + gap_x <= x1 - x0
            && s * (rows[0] + rows[1]) + gap_y <= y1 - y0
    };
    let scale = scale
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or_else(|| SCALES.iter().copied().find(|s| fits(*s)).unwrap_or(0.001));
    // The grid centred in the room.
    let total_w = scale * (columns[0] + columns[1]) + gap_x;
    let total_h = scale * (rows[0] + rows[1]) + gap_y;
    let left = x0 + ((x1 - x0) - total_w) / 2.0;
    let top = y1 - ((y1 - y0) - total_h) / 2.0;
    let column_centre = |c: usize| {
        if c == 0 {
            left + scale * columns[0] / 2.0
        } else {
            left + scale * columns[0] + gap_x + scale * columns[1] / 2.0
        }
    };
    let row_centre = |r: usize| {
        if r == 0 {
            top - scale * rows[0] / 2.0
        } else {
            top - scale * rows[0] - gap_y - scale * rows[1] / 2.0
        }
    };
    let views = views
        .into_iter()
        .map(|(kind, edges, (lo, hi))| {
            let (c, r) = cell(kind, projection);
            let centre = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
            PlacedView {
                kind,
                edges,
                origin: [
                    column_centre(c) - scale * centre[0],
                    row_centre(r) - scale * centre[1],
                ],
            }
        })
        .collect();
    Sheet {
        size,
        projection,
        scale,
        views,
        title,
        show_hidden,
    }
}

// ---- What is drawn ---------------------------------------------------------------------------

/// How a line of the sheet is drawn (ISO 128).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pen {
    /// Visible edges: thick.
    Visible,
    /// Hidden edges: thin, dashed.
    Hidden,
    /// The frame.
    Frame,
    /// The title block's lines and the projection symbol.
    Thin,
    /// Centre marks of holes: thin chain lines.
    Center,
}

impl Pen {
    /// Width, mm on the sheet.
    pub fn width(self) -> f64 {
        match self {
            Pen::Visible => 0.5,
            Pen::Hidden => 0.25,
            Pen::Frame => 0.7,
            Pen::Thin | Pen::Center => 0.25,
        }
    }

    /// The dashes and gaps of a broken line, mm on the sheet: a hidden line's dashes, a
    /// centre line's long dash and dot.
    pub fn dash(self) -> Option<&'static [f64]> {
        match self {
            Pen::Hidden => Some(&[3.0, 1.5]),
            Pen::Center => Some(&[8.0, 1.5, 1.0, 1.5]),
            Pen::Visible | Pen::Frame | Pen::Thin => None,
        }
    }

    fn layer(self) -> &'static str {
        match self {
            Pen::Visible => "VISIBLE",
            Pen::Hidden => "HIDDEN",
            Pen::Center => "CENTER",
            Pen::Frame | Pen::Thin => "SHEET",
        }
    }
}

/// A piece of text on the sheet: lower left corner, height (mm), text.
struct Text {
    at: [f64; 2],
    height: f64,
    text: String,
}

/// Everything on the sheet, in sheet mm: polylines with their pen, and texts.
struct Drawing {
    lines: Vec<(Pen, Vec<[f64; 2]>)>,
    texts: Vec<Text>,
}

impl Sheet {
    /// Every line of the sheet, in sheet mm, with its pen: what the writers write, for a
    /// preview.
    pub fn lines(&self) -> Vec<(Pen, Vec<[f64; 2]>)> {
        self.drawing().lines
    }

    /// Every text of the sheet: its lower left corner and height (sheet mm), and the text.
    pub fn texts(&self) -> Vec<([f64; 2], f64, String)> {
        self.drawing()
            .texts
            .into_iter()
            .map(|t| (t.at, t.height, t.text))
            .collect()
    }

    fn drawing(&self) -> Drawing {
        let [width, height] = self.size.size();
        let mut lines: Vec<(Pen, Vec<[f64; 2]>)> = Vec::new();
        let mut texts = Vec::new();
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]
        };
        lines.push((
            Pen::Frame,
            rect(MARGIN, MARGIN, width - MARGIN, height - MARGIN),
        ));
        for view in &self.views {
            let place = |p: &[f64; 2]| {
                [
                    view.origin[0] + self.scale * p[0],
                    view.origin[1] + self.scale * p[1],
                ]
            };
            // An isometric view is clearer without them, as SolidWorks draws it.
            if self.show_hidden && view.kind != ViewKind::Isometric {
                for line in &view.edges.hidden {
                    lines.push((Pen::Hidden, line.iter().map(place).collect()));
                }
            }
            for line in &view.edges.visible {
                lines.push((Pen::Visible, line.iter().map(place).collect()));
            }
            // Centre marks on the holes seen end on, past them by 2 mm.
            if view.kind != ViewKind::Isometric {
                for (centre, radius) in circles(&view.edges.visible) {
                    let c = place(&centre);
                    let reach = radius * self.scale + 2.0;
                    lines.push((
                        Pen::Center,
                        vec![[c[0] - reach, c[1]], [c[0] + reach, c[1]]],
                    ));
                    lines.push((
                        Pen::Center,
                        vec![[c[0], c[1] - reach], [c[0], c[1] + reach]],
                    ));
                }
            }
        }
        // The title block, in the lower right corner of the frame.
        let (x0, y0) = (width - MARGIN - TITLE_WIDTH, MARGIN);
        let (x1, y1) = (width - MARGIN, MARGIN + TITLE_HEIGHT);
        let ym = y0 + TITLE_HEIGHT / 2.0;
        lines.push((Pen::Frame, rect(x0, y0, x1, y1)));
        lines.push((Pen::Thin, vec![[x0, ym], [x1, ym]]));
        for x in [x0 + 90.0, x0 + 135.0] {
            lines.push((Pen::Thin, vec![[x, y0], [x, y1]]));
        }
        for x in [x0 + 45.0, x0 + 157.0] {
            lines.push((Pen::Thin, vec![[x, y0], [x, ym]]));
        }
        let mut field = |x: f64, y: f64, label: &str, value: &str, big: bool| {
            texts.push(Text {
                at: [x + 1.5, y + 10.5],
                height: 2.0,
                text: label.to_owned(),
            });
            texts.push(Text {
                at: [x + 1.5, y + 3.0],
                height: if big { 5.0 } else { 3.5 },
                text: value.to_owned(),
            });
        };
        let t = &self.title;
        field(x0, ym, "TITLE", &t.title, true);
        field(x0 + 90.0, ym, "PART NUMBER", &t.part_number, false);
        field(x0 + 135.0, ym, "MATERIAL", &t.material, false);
        field(x0, y0, "DRAWN BY", &t.drawn_by, false);
        field(x0 + 45.0, y0, "DATE", &t.date, false);
        field(x0 + 90.0, y0, "SCALE", &scale_text(self.scale), false);
        field(x0 + 157.0, y0, "SHEET", self.size.label(), false);
        // The projection symbol: a truncated cone seen from the front and from its end,
        // the end view on the side the projection puts it.
        let (cx, cy) = (x0 + 146.0, y0 + 6.0);
        let circles_left = self.projection == Projection::ThirdAngle;
        let (cone_x, circle_x) = if circles_left {
            (cx - 1.0, cx - 6.0)
        } else {
            (cx - 7.0, cx + 4.5)
        };
        lines.push((
            Pen::Thin,
            vec![
                [cone_x, cy - 3.0],
                [cone_x + 6.0, cy - 1.8],
                [cone_x + 6.0, cy + 1.8],
                [cone_x, cy + 3.0],
                [cone_x, cy - 3.0],
            ],
        ));
        for r in [3.0, 1.8] {
            let circle: Vec<[f64; 2]> = (0..=32)
                .map(|k| {
                    let a = std::f64::consts::TAU * f64::from(k) / 32.0;
                    [circle_x + r * a.cos(), cy + r * a.sin()]
                })
                .collect();
            lines.push((Pen::Thin, circle));
        }
        texts.push(Text {
            at: [x0 + 136.5, y0 + 10.5],
            height: 2.0,
            text: "PROJECTION".to_owned(),
        });
        texts.push(Text {
            at: [MARGIN + 3.0, MARGIN + 3.0],
            height: 2.5,
            text: "Dimensions in mm".to_owned(),
        });
        Drawing { lines, texts }
    }
}

/// The full circles among `lines` (closed polylines whose points all lie at one distance
/// from their middle), one per centre: the largest of concentric ones.
fn circles(lines: &[Vec<[f64; 2]>]) -> Vec<([f64; 2], f64)> {
    let mut out: Vec<([f64; 2], f64)> = Vec::new();
    for line in lines {
        let (Some(first), Some(last)) = (line.first(), line.last()) else {
            continue;
        };
        if line.len() < 8 || (first[0] - last[0]).hypot(first[1] - last[1]) > 1e-6 {
            continue;
        }
        let n = (line.len() - 1) as f64;
        let points = line.get(..line.len() - 1).unwrap_or_default();
        let centre = [
            points.iter().map(|p| p[0]).sum::<f64>() / n,
            points.iter().map(|p| p[1]).sum::<f64>() / n,
        ];
        let radius = points
            .iter()
            .map(|p| (p[0] - centre[0]).hypot(p[1] - centre[1]))
            .sum::<f64>()
            / n;
        let round = radius > 1e-6
            && points.iter().all(|p| {
                ((p[0] - centre[0]).hypot(p[1] - centre[1]) - radius).abs() <= 0.01 * radius
            });
        if !round {
            continue;
        }
        let tolerance = 1e-3 * radius.max(1.0);
        match out.iter_mut().find(|(c, _)| {
            (c[0] - centre[0]).abs() <= tolerance && (c[1] - centre[1]).abs() <= tolerance
        }) {
            Some((_, r)) => *r = r.max(radius),
            None => out.push((centre, radius)),
        }
    }
    out
}

fn number(v: f64) -> String {
    let text = format!("{v:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

// ---- SVG -----------------------------------------------------------------------------------

/// The sheet as an SVG file, at its size in millimetres.
pub fn to_svg(sheet: &Sheet) -> String {
    let [width, height] = sheet.size.size();
    let drawing = sheet.drawing();
    let mut out = String::new();
    let _ = writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}mm\" height=\"{}mm\" viewBox=\"0 0 {} {}\">",
        number(width),
        number(height),
        number(width),
        number(height)
    );
    let _ = writeln!(
        out,
        "<rect width=\"{}\" height=\"{}\" fill=\"#ffffff\"/>",
        number(width),
        number(height)
    );
    // The sheet's y goes up, the SVG's down.
    let y = |v: f64| number(height - v);
    for (pen, line) in &drawing.lines {
        let mut d = String::new();
        for (i, [px, py]) in line.iter().enumerate() {
            let _ = write!(
                d,
                "{}{} {} ",
                if i == 0 { "M" } else { "L" },
                number(*px),
                y(*py)
            );
        }
        let dash = pen
            .dash()
            .map(|pattern| {
                let pattern: Vec<String> = pattern.iter().map(|v| number(*v)).collect();
                format!(" stroke-dasharray=\"{}\"", pattern.join(" "))
            })
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "<path d=\"{}\" fill=\"none\" stroke=\"#000000\" stroke-width=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"{dash}/>",
            d.trim_end(),
            number(pen.width())
        );
    }
    for text in &drawing.texts {
        let _ = writeln!(
            out,
            "<text x=\"{}\" y=\"{}\" font-family=\"Helvetica, Arial, sans-serif\" font-size=\"{}\">{}</text>",
            number(text.at[0]),
            y(text.at[1]),
            number(text.height),
            xml_escape(&text.text)
        );
    }
    out.push_str("</svg>\n");
    out
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---- PDF -----------------------------------------------------------------------------------

/// The sheet as a one-page PDF at its size, its texts in Helvetica.
pub fn to_pdf(sheet: &Sheet) -> Vec<u8> {
    const POINTS_PER_MM: f64 = 72.0 / 25.4;
    let [width, height] = sheet.size.size();
    let drawing = sheet.drawing();
    // The page in millimetres: one scale for everything.
    let mut content = format!("{POINTS_PER_MM:.6} 0 0 {POINTS_PER_MM:.6} 0 0 cm\n1 J 1 j\n");
    for (pen, line) in &drawing.lines {
        let _ = writeln!(content, "{} w", number(pen.width()));
        match pen.dash() {
            Some(pattern) => {
                let pattern: Vec<String> = pattern.iter().map(|v| number(*v)).collect();
                let _ = writeln!(content, "[{}] 0 d", pattern.join(" "));
            }
            None => content.push_str("[] 0 d\n"),
        }
        for (i, [x, y]) in line.iter().enumerate() {
            let _ = writeln!(
                content,
                "{} {} {}",
                number(*x),
                number(*y),
                if i == 0 { "m" } else { "l" }
            );
        }
        content.push_str("S\n");
    }
    for text in &drawing.texts {
        let _ = writeln!(
            content,
            "BT /F1 {} Tf {} {} Td ({}) Tj ET",
            number(text.height),
            number(text.at[0]),
            number(text.at[1]),
            pdf_text(&text.text)
        );
    }
    let page = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        number(width * POINTS_PER_MM),
        number(height * POINTS_PER_MM)
    );
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        page,
        format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_owned(),
    ];
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        let _ = writeln!(table, "{offset:010} 00000 n ");
    }
    let _ = write!(
        table,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    out.extend_from_slice(table.as_bytes());
    out
}

/// A PDF string's contents in WinAnsi (Latin-1 here): brackets and backslashes escaped,
/// characters beyond Latin-1 as `?`.
fn pdf_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            ' '..='~' => out.push(c),
            c if (0xa0..=0xff).contains(&(c as u32)) => {
                let _ = write!(out, "\\{:03o}", c as u32);
            }
            _ => out.push('?'),
        }
    }
    out
}

// ---- DXF -----------------------------------------------------------------------------------

/// The sheet as a DXF file (R12, every program reads it) at the views' true size: the
/// sheet divided by its scale. Visible edges on the `VISIBLE` layer, hidden ones on
/// `HIDDEN` (dashed), the frame and the title block on `SHEET`.
pub fn to_dxf(sheet: &Sheet) -> String {
    let unscale = 1.0 / sheet.scale;
    let drawing = sheet.drawing();
    let mut out = String::new();
    let mut pair = |code: u32, value: &str| {
        let _ = write!(out, "{code}\n{value}\n");
    };
    pair(0, "SECTION");
    pair(2, "HEADER");
    pair(9, "$ACADVER");
    pair(1, "AC1009");
    pair(9, "$INSUNITS");
    pair(70, "4");
    pair(9, "$DWGCODEPAGE");
    pair(3, "ANSI_1252");
    pair(0, "ENDSEC");
    pair(0, "SECTION");
    pair(2, "TABLES");
    // Line types: solid, and the hidden and centre lines' patterns at the true size
    // (dashes positive, gaps negative).
    pair(0, "TABLE");
    pair(2, "LTYPE");
    pair(70, "3");
    pair(0, "LTYPE");
    pair(2, "CONTINUOUS");
    pair(70, "0");
    pair(3, "Solid line");
    pair(72, "65");
    pair(73, "0");
    pair(40, "0.0");
    for (pen, name, description) in [
        (Pen::Hidden, "HIDDEN", "Hidden __ __ __"),
        (Pen::Center, "CENTER", "Center ____ _ ____"),
    ] {
        let pattern = pen.dash().unwrap_or(&[]);
        pair(0, "LTYPE");
        pair(2, name);
        pair(70, "0");
        pair(3, description);
        pair(72, "65");
        pair(73, &pattern.len().to_string());
        pair(40, &number(pattern.iter().sum::<f64>() * unscale));
        for (i, v) in pattern.iter().enumerate() {
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            pair(49, &number(sign * v * unscale));
        }
    }
    pair(0, "ENDTAB");
    pair(0, "TABLE");
    pair(2, "LAYER");
    pair(70, "4");
    for (layer, color, ltype) in [
        ("VISIBLE", "7", "CONTINUOUS"),
        ("HIDDEN", "8", "HIDDEN"),
        ("CENTER", "1", "CENTER"),
        ("SHEET", "7", "CONTINUOUS"),
    ] {
        pair(0, "LAYER");
        pair(2, layer);
        pair(70, "0");
        pair(62, color);
        pair(6, ltype);
    }
    pair(0, "ENDTAB");
    pair(0, "ENDSEC");
    pair(0, "SECTION");
    pair(2, "ENTITIES");
    for (pen, line) in &drawing.lines {
        for segment in line.windows(2) {
            let [[ax, ay], [bx, by]] = [
                segment.first().copied().unwrap_or_default(),
                segment.get(1).copied().unwrap_or_default(),
            ];
            pair(0, "LINE");
            pair(8, pen.layer());
            pair(10, &number(ax * unscale));
            pair(20, &number(ay * unscale));
            pair(30, "0.0");
            pair(11, &number(bx * unscale));
            pair(21, &number(by * unscale));
            pair(31, "0.0");
        }
    }
    for text in &drawing.texts {
        pair(0, "TEXT");
        pair(8, "SHEET");
        pair(10, &number(text.at[0] * unscale));
        pair(20, &number(text.at[1] * unscale));
        pair(30, "0.0");
        pair(40, &number(text.height * unscale));
        pair(1, &dxf_text(&text.text));
    }
    pair(0, "ENDSEC");
    pair(0, "EOF");
    out
}

/// Text for a DXF file: ASCII, other characters as `\U+XXXX` (which AutoCAD, LibreCAD
/// and QCAD read).
fn dxf_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii() && !c.is_ascii_control() {
            out.push(c);
        } else if !c.is_control() {
            let _ = write!(out, "\\U+{:04X}", c as u32);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rectangle(w: f64, h: f64) -> ViewEdges {
        ViewEdges {
            visible: vec![vec![[0.0, 0.0], [w, 0.0], [w, h], [0.0, h], [0.0, 0.0]]],
            hidden: vec![vec![[w / 2.0, 0.0], [w / 2.0, h]]],
        }
    }

    /// A 40 × 20 × 10 block: front 40 wide, 20 high; top 40 by 10; right 10 by 20.
    fn block_views() -> Vec<(ViewKind, ViewEdges)> {
        vec![
            (ViewKind::Front, rectangle(40.0, 20.0)),
            (ViewKind::Top, rectangle(40.0, 10.0)),
            (ViewKind::Right, rectangle(10.0, 20.0)),
        ]
    }

    fn centre(sheet: &Sheet, kind: ViewKind) -> [f64; 2] {
        let view = sheet.views.iter().find(|v| v.kind == kind).unwrap();
        let (lo, hi) = view.edges.bounds().unwrap();
        [
            view.origin[0] + sheet.scale * (lo[0] + hi[0]) / 2.0,
            view.origin[1] + sheet.scale * (lo[1] + hi[1]) / 2.0,
        ]
    }

    #[test]
    fn view_planes_are_right_handed_and_the_date_is_a_date() {
        for kind in ViewKind::ALL {
            let p = kind.plane();
            assert!((p.x_dir.length() - 1.0).abs() < 1e-12);
            assert!(p.x_dir.dot(p.normal).abs() < 1e-12);
        }
        // The isometric view keeps Y up the page.
        assert!(ViewKind::Isometric.plane().y_dir().y > 0.5);
        let date = today();
        assert_eq!(date.len(), 10);
        assert!(date.starts_with("20"));
    }

    #[test]
    fn views_go_where_the_projection_puts_them_lined_up_with_the_front() {
        for projection in [Projection::FirstAngle, Projection::ThirdAngle] {
            let sheet = layout(
                block_views(),
                SheetSize::A4,
                projection,
                None,
                TitleBlock::default(),
                true,
            );
            let (front, top, right) = (
                centre(&sheet, ViewKind::Front),
                centre(&sheet, ViewKind::Top),
                centre(&sheet, ViewKind::Right),
            );
            // Lined up: the top view under or over the front, the right one level with it.
            assert!((front[0] - top[0]).abs() < 1e-9);
            assert!((front[1] - right[1]).abs() < 1e-9);
            match projection {
                Projection::FirstAngle => {
                    assert!(top[1] < front[1] && right[0] < front[0]);
                }
                Projection::ThirdAngle => {
                    assert!(top[1] > front[1] && right[0] > front[0]);
                }
            }
            // The largest standard scale that fits a 297 × 210 sheet: 2:1.
            assert_eq!(sheet.scale, 2.0);
            assert_eq!(scale_text(sheet.scale), "2:1");
        }
        // Bigger parts get smaller scales, never bigger than the sheet.
        let big: Vec<(ViewKind, ViewEdges)> = vec![(ViewKind::Front, rectangle(900.0, 300.0))];
        let sheet = layout(
            big,
            SheetSize::A4,
            Projection::FirstAngle,
            None,
            TitleBlock::default(),
            true,
        );
        assert_eq!(scale_text(sheet.scale), "1:5");
        assert_eq!(scale_text(0.02), "1:50");
    }

    #[test]
    fn the_writers_write_the_sheet() {
        let title = TitleBlock {
            title: "Équerre (bracket)".to_owned(),
            part_number: "OXY-1".to_owned(),
            ..TitleBlock::default()
        };
        let sheet = layout(
            block_views(),
            SheetSize::A3,
            Projection::FirstAngle,
            Some(1.0),
            title,
            true,
        );
        let svg = to_svg(&sheet);
        assert!(svg.starts_with("<?xml") && svg.trim_end().ends_with("</svg>"));
        assert!(svg.contains("width=\"420mm\""));
        assert!(svg.contains("stroke-dasharray"), "hidden lines are dashed");
        assert!(svg.contains("Équerre (bracket)"));

        let pdf = to_pdf(&sheet);
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.trim_end().ends_with("%%EOF"));
        // The cross-reference table points at the objects (byte offsets: the binary
        // marker of the second line is not UTF-8).
        let marker = b"startxref\n";
        let at = pdf
            .windows(marker.len())
            .rposition(|w| w == marker)
            .unwrap();
        let tail = std::str::from_utf8(&pdf[at + marker.len()..]).unwrap();
        let start: usize = tail.lines().next().unwrap().parse().unwrap();
        assert!(pdf[start..].starts_with(b"xref"));
        let table = std::str::from_utf8(&pdf[start..]).unwrap();
        for (k, line) in table.lines().skip(3).take(5).enumerate() {
            let offset: usize = line[..10].parse().unwrap();
            assert!(
                pdf[offset..].starts_with(format!("{} 0 obj", k + 1).as_bytes()),
                "{line}"
            );
        }
        // É is Latin-1 octal 311, brackets escaped.
        assert!(text.contains("(\\311querre \\(bracket\\))"));

        let dxf = to_dxf(&sheet);
        assert!(dxf.starts_with("0\nSECTION\n2\nHEADER"));
        assert!(dxf.trim_end().ends_with("0\nEOF"));
        assert!(dxf.contains("\\U+00C9querre"));
        // At 1:1 the block's front view is 40 long in the DXF: its bottom edge.
        let lines = dxf.matches("\nLINE\n").count();
        assert!(lines > 20);
        assert!(dxf.contains("HIDDEN"));
        // The front view's hole circle gets a centre mark; DXF has its line type.
        let sheet_with_hole = layout(
            vec![(
                ViewKind::Front,
                ViewEdges {
                    visible: vec![
                        (0..=32)
                            .map(|k| {
                                let a = std::f64::consts::TAU * f64::from(k) / 32.0;
                                [5.0 + 3.0 * a.cos(), 5.0 + 3.0 * a.sin()]
                            })
                            .collect(),
                    ],
                    hidden: Vec::new(),
                },
            )],
            SheetSize::A4,
            Projection::FirstAngle,
            Some(1.0),
            TitleBlock::default(),
            true,
        );
        let centre_lines = sheet_with_hole
            .lines()
            .into_iter()
            .filter(|(pen, _)| *pen == Pen::Center)
            .count();
        assert_eq!(centre_lines, 2);
        assert!(to_dxf(&sheet_with_hole).contains("CENTER"));
        // Without hidden lines, none are written.
        let plain = Sheet {
            show_hidden: false,
            ..sheet
        };
        assert!(!to_svg(&plain).contains("stroke-dasharray"));
    }
}
