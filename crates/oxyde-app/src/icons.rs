//! The application's icons, drawn as vector shapes so they stay sharp at any scale.
//!
//! Every icon is drawn on a 24 × 24 grid (y down) and scaled to the rectangle it is given.
//! One palette for all of them: dark outlines, blue-grey solids, blue sketch geometry,
//! orange for what a command does (direction, the face it makes), green and red for adding
//! and removing.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use eframe::egui::{self, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    // Features
    Sketch,
    Extrude,
    Revolve,
    Sweep,
    /// Loft: a square at the bottom turning into a circle at the top.
    Loft,
    Fillet,
    Chamfer,
    Shell,
    /// Hole Wizard: a block with a counterbored hole.
    Hole,
    LinearPattern,
    CircularPattern,
    Mirror,
    RefPlane,
    RefAxis,
    ReferenceGeometry,
    Imported,
    Combine,
    Appearance,
    // Bodies
    Bodies,
    Intersect,
    NewBody,
    // Assemblies
    Assembly,
    InsertComponent,
    Mate,
    BillOfMaterials,
    /// Move Component: a part with a four-way arrow.
    Move,
    /// The material library: swatches.
    Materials,
    // Timeline
    Part,
    Plane,
    Origin,
    // Sketch tools
    Select,
    Line,
    Centerline,
    Rectangle,
    CenterRectangle,
    Circle,
    Arc,
    /// 3 Point Arc: an arc through its two ends and a point between.
    ThreePointArc,
    /// Tangent Arc: a line going on into an arc.
    TangentArc,
    /// Straight Slot: a stadium with its centerline.
    Slot,
    /// Polygon: a hexagon in its construction circle.
    Polygon,
    /// Point: a sketch point.
    Point,
    SmartDimension,
    Trim,
    /// Extend Entities: a line lengthened, dashed, up to another one.
    Extend,
    SketchFillet,
    Offset,
    Convert,
    Construction,
    ExitSketch,
    Discard,
    // Evaluate
    MassProperties,
    Measure,
    /// Section View: a block cut open, the cut face coloured.
    SectionView,
    /// Interference Detection: two blocks overlapping, the overlap in red.
    Interference,
    /// The ConfigurationManager: a table with a marked row.
    Configurations,
    Equations,
    Rebuild,
    // Render
    /// Render: a shiny ball on a floor, its shadow under it.
    Render,
    /// Save Image: a picture with a mountain and the sun.
    SaveImage,
    /// glTF export: a block with an arrow leaving it.
    ExportMesh,
    // Simulation
    /// Fixed Geometry: a block held against hatched ground, in green.
    Fixture,
    /// Roller/Slider: a block on green rollers.
    Roller,
    /// Force: a purple arrow pushing on a block.
    Force,
    /// Pressure: purple arrows pressing on a block's top.
    Pressure,
    /// Gravity: a block with a heavy arrow down.
    Gravity,
    /// Mesh: a block split into triangles.
    Mesh,
    /// Run: a green play mark.
    RunStudy,
    /// Stress: a block in plot colours, blue to red.
    Stress,
    /// Displacement: a block and, dashed, where it moves.
    Displacement,
    /// Factor of safety: a shield with a check.
    Safety,
    /// New Study: a study sheet with a green plus.
    NewStudy,
    /// Temperature: a thermometer, red.
    Temperature,
    /// Heat Power: orange arrows into a block, a flame.
    HeatPower,
    /// Heat Flux: orange arrows into a block's top.
    HeatFlux,
    /// Convection: blue waves leaving a block.
    Convection,
    /// Radiation: orange zigzags leaving a block.
    Radiation,
    /// Internal Heat: a block with a glowing core.
    InternalHeat,
    /// The temperature plot: a block in plot colours, cold to hot, with a thermometer.
    TemperaturePlot,
    /// The heat flux plot: a block with arrows through it in plot colours.
    HeatFluxPlot,
    /// A flow study: streamlines bending round a body.
    FlowStudy,
    /// A cooling study: a heat sink's fins, warm at the base, in a blue stream.
    CoolingStudy,
    /// An inlet: blue arrows into a lid.
    Inlet,
    /// A static pressure opening, where the fluid goes out: blue arrows leaving a lid.
    Outlet,
    /// An ideal wall: a wall the fluid slips along.
    IdealWall,
    /// A goal: a flag on a face, what a flow study measures.
    Goal,
    /// A cut plot: a plane through the fluid in plot colours.
    CutPlot,
    /// A surface plot: a body's faces in plot colours, with flow lines past it.
    SurfacePlot,
    // View and edit
    ViewFront,
    ViewTop,
    ViewRight,
    ViewIso,
    Fit,
    NormalTo,
    Undo,
    Redo,
    Eye,
    EyeClosed,
    // Badges
    Add,
    Remove,
    Ok,
    Cancel,
    Error,
    Warning,
}

const INK: Color32 = Color32::from_rgb(46, 58, 70);
const BODY: Color32 = Color32::from_rgb(169, 193, 218);
const BODY_TOP: Color32 = Color32::from_rgb(207, 221, 235);
const BODY_SIDE: Color32 = Color32::from_rgb(124, 156, 190);
const SKETCH: Color32 = Color32::from_rgb(30, 111, 217);
const ACCENT: Color32 = Color32::from_rgb(232, 145, 40);
const ACCENT_LIGHT: Color32 = Color32::from_rgb(246, 196, 120);
const ADD: Color32 = Color32::from_rgb(46, 157, 87);
const REMOVE: Color32 = Color32::from_rgb(214, 69, 69);
const PLANE: Color32 = Color32::from_rgb(236, 222, 176);
const PLANE_EDGE: Color32 = Color32::from_rgb(184, 150, 62);
const AXIS_X: Color32 = Color32::from_rgb(210, 50, 50);
const AXIS_Y: Color32 = Color32::from_rgb(40, 150, 60);
/// Loads in the Simulation tab, purple as in SolidWorks.
const LOAD: Color32 = Color32::from_rgb(150, 60, 190);
/// Heat in the Simulation tab's thermal loads.
const HEAT: Color32 = Color32::from_rgb(235, 120, 25);
const HOT: Color32 = Color32::from_rgb(220, 45, 35);
/// A fluid carrying heat away.
const COOL: Color32 = Color32::from_rgb(40, 125, 215);
const WHITE: Color32 = Color32::WHITE;

/// Draws on the 24-unit grid of one icon.
struct Pen<'a> {
    painter: &'a egui::Painter,
    origin: Pos2,
    scale: f32,
    enabled: bool,
}

impl Pen<'_> {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x, y) * self.scale
    }

    fn pts(&self, points: &[(f32, f32)]) -> Vec<Pos2> {
        points.iter().map(|&(x, y)| self.p(x, y)).collect()
    }

    /// Greyed out when the command is not available. On a dark theme the outlines and
    /// sketch blue turn light, so the icons stand out from the panel.
    fn c(&self, color: Color32) -> Color32 {
        let color = if crate::theme::ui_dark() {
            match color {
                INK => Color32::from_rgb(196, 210, 224),
                SKETCH => Color32::from_rgb(79, 195, 247),
                _ => color,
            }
        } else {
            color
        };
        if self.enabled {
            color
        } else if crate::theme::ui_dark() {
            let [r, g, b, a] = color.to_array();
            let grey = ((r as u32 * 3 + g as u32 * 6 + b as u32) / 10) as u8;
            let grey = 70 + grey / 3;
            Color32::from_rgba_unmultiplied(grey, grey, grey, a)
        } else {
            let [r, g, b, a] = color.to_array();
            let grey = ((r as u32 * 3 + g as u32 * 6 + b as u32) / 10) as u8;
            let grey = 150 + grey / 3;
            Color32::from_rgba_unmultiplied(grey, grey, grey, a)
        }
    }

    fn stroke(&self, width: f32, color: Color32) -> Stroke {
        Stroke::new(width * self.scale / 1.5, self.c(color))
    }

    fn line(&self, points: &[(f32, f32)], width: f32, color: Color32) {
        self.painter
            .add(Shape::line(self.pts(points), self.stroke(width, color)));
    }

    fn closed(&self, points: &[(f32, f32)], width: f32, color: Color32) {
        self.painter.add(Shape::closed_line(
            self.pts(points),
            self.stroke(width, color),
        ));
    }

    fn dashed(&self, points: &[(f32, f32)], width: f32, color: Color32, dash: f32, gap: f32) {
        self.painter.extend(Shape::dashed_line(
            &self.pts(points),
            self.stroke(width, color),
            dash * self.scale,
            gap * self.scale,
        ));
    }

    /// A convex polygon, filled and outlined.
    fn poly(&self, points: &[(f32, f32)], fill: Color32, outline: Color32) {
        self.painter.add(Shape::convex_polygon(
            self.pts(points),
            self.c(fill),
            self.stroke(1.0, outline),
        ));
    }

    fn fill(&self, points: &[(f32, f32)], fill: Color32) {
        self.painter.add(Shape::convex_polygon(
            self.pts(points),
            self.c(fill),
            Stroke::NONE,
        ));
    }

    fn circle(&self, x: f32, y: f32, r: f32, fill: Color32, outline: Option<(f32, Color32)>) {
        let stroke = outline.map_or(Stroke::NONE, |(w, c)| self.stroke(w, c));
        self.painter
            .circle(self.p(x, y), r * self.scale, self.c(fill), stroke);
    }

    fn ring(&self, x: f32, y: f32, r: f32, width: f32, color: Color32) {
        self.painter
            .circle_stroke(self.p(x, y), r * self.scale, self.stroke(width, color));
    }

    /// Points on an arc (angles in radians, y down: positive angles turn clockwise).
    fn arc_points(cx: f32, cy: f32, rx: f32, ry: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
        let n = (((a1 - a0).abs() / (PI / 16.0)).ceil() as usize).max(2);
        (0..=n)
            .map(|k| {
                let a = a0 + (a1 - a0) * k as f32 / n as f32;
                (cx + rx * a.cos(), cy + ry * a.sin())
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn arc(&self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, width: f32, color: Color32) {
        self.line(&Self::arc_points(cx, cy, r, r, a0, a1), width, color);
    }

    /// A filled arrow head at `tip`, pointing along (dx, dy).
    fn head(&self, tip: (f32, f32), dx: f32, dy: f32, size: f32, color: Color32) {
        let length = (dx * dx + dy * dy).sqrt().max(1e-6);
        let (ux, uy) = (dx / length, dy / length);
        let back = (tip.0 - ux * size, tip.1 - uy * size);
        let half = size * 0.55;
        self.fill(
            &[
                tip,
                (back.0 - uy * half, back.1 + ux * half),
                (back.0 + uy * half, back.1 - ux * half),
            ],
            color,
        );
    }

    fn arrow(&self, from: (f32, f32), to: (f32, f32), width: f32, color: Color32) {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = (dx * dx + dy * dy).sqrt().max(1e-6);
        let size = 4.5;
        let end = (
            to.0 - dx / length * size * 0.8,
            to.1 - dy / length * size * 0.8,
        );
        self.line(&[from, end], width, color);
        self.head(to, dx, dy, size, color);
    }

    /// A box in a simple oblique projection: front face at (x, y) of size w × h, receding
    /// by `d` up and to the right.
    fn block(&self, x: f32, y: f32, w: f32, h: f32, d: f32, faces: [Color32; 3]) {
        let [top, front, side] = faces;
        self.poly(
            &[(x, y), (x + d, y - d), (x + w + d, y - d), (x + w, y)],
            top,
            INK,
        );
        self.poly(
            &[(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
            front,
            INK,
        );
        self.poly(
            &[
                (x + w, y),
                (x + w + d, y - d),
                (x + w + d, y + h - d),
                (x + w, y + h),
            ],
            side,
            INK,
        );
    }

    fn solid(&self, x: f32, y: f32, w: f32, h: f32, d: f32) {
        self.block(x, y, w, h, d, [BODY_TOP, BODY, BODY_SIDE]);
    }

    /// A sketch point: a small filled square.
    fn vertex(&self, x: f32, y: f32, color: Color32) {
        let s = 1.4;
        self.fill(
            &[
                (x - s, y - s),
                (x + s, y - s),
                (x + s, y + s),
                (x - s, y + s),
            ],
            color,
        );
    }

    fn check(&self, x: f32, y: f32, size: f32, color: Color32) {
        self.line(
            &[
                (x - size, y),
                (x - size * 0.3, y + size * 0.7),
                (x + size, y - size * 0.8),
            ],
            2.6,
            color,
        );
    }

    fn cross(&self, x: f32, y: f32, size: f32, color: Color32) {
        self.line(&[(x - size, y - size), (x + size, y + size)], 2.6, color);
        self.line(&[(x - size, y + size), (x + size, y - size)], 2.6, color);
    }

    fn badge(&self, x: f32, y: f32, color: Color32) {
        self.circle(x, y, 5.0, color, Some((1.0, WHITE)));
    }

    /// A cube with one face picked out, for the standard views.
    fn view_cube(&self, highlight: Option<usize>) {
        let faces = [0, 1, 2].map(|i| {
            if highlight == Some(i) {
                SKETCH
            } else {
                [BODY_TOP, BODY, BODY_SIDE][i]
            }
        });
        self.block(4.0, 9.0, 11.0, 11.0, 5.0, faces);
    }

    /// A tilted plane, as in the timeline.
    fn plane(&self, x: f32, y: f32, w: f32, h: f32) {
        let skew = h * 0.75;
        self.poly(
            &[(x + skew, y), (x + w, y), (x + w - skew, y + h), (x, y + h)],
            PLANE,
            PLANE_EDGE,
        );
    }
}

/// Paints `icon` into `rect` (a square works best).
pub fn paint(painter: &egui::Painter, rect: Rect, icon: Icon, enabled: bool) {
    let size = rect.width().min(rect.height());
    let origin = rect.center() - Vec2::splat(size / 2.0);
    let pen = Pen {
        painter,
        origin,
        scale: size / 24.0,
        enabled,
    };
    draw(&pen, icon);
}

fn draw(pen: &Pen, icon: Icon) {
    use Icon::*;
    match icon {
        Sketch => {
            pen.plane(1.0, 9.0, 18.0, 12.0);
            pen.closed(
                &[(6.0, 18.0), (9.0, 12.0), (15.0, 12.0), (12.0, 18.0)],
                1.6,
                SKETCH,
            );
            // Pencil
            pen.poly(
                &[(13.0, 9.0), (20.0, 2.0), (23.0, 5.0), (16.0, 12.0)],
                ACCENT,
                INK,
            );
            pen.poly(&[(13.0, 9.0), (16.0, 12.0), (11.5, 13.5)], BODY_TOP, INK);
        }
        Extrude => {
            pen.closed(
                &[(2.0, 22.0), (6.0, 18.0), (18.0, 18.0), (14.0, 22.0)],
                1.4,
                SKETCH,
            );
            pen.solid(3.0, 12.0, 11.0, 9.0, 4.0);
            pen.arrow((16.5, 11.0), (16.5, 1.5), 2.0, ACCENT);
        }
        Revolve => {
            pen.dashed(&[(12.0, 1.0), (12.0, 23.0)], 1.2, INK, 3.0, 1.5);
            // A cylinder.
            pen.fill(&[(6.0, 7.0), (18.0, 7.0), (18.0, 18.0), (6.0, 18.0)], BODY);
            let bottom = Pen::arc_points(12.0, 18.0, 6.0, 2.2, 0.0, PI);
            pen.painter.add(Shape::convex_polygon(
                pen.pts(&bottom),
                pen.c(BODY),
                Stroke::NONE,
            ));
            pen.line(&[(6.0, 7.0), (6.0, 18.0)], 1.0, INK);
            pen.line(&[(18.0, 7.0), (18.0, 18.0)], 1.0, INK);
            pen.line(&bottom, 1.0, INK);
            let top = Pen::arc_points(12.0, 7.0, 6.0, 2.2, 0.0, TAU);
            pen.painter.add(Shape::convex_polygon(
                pen.pts(&top),
                pen.c(BODY_TOP),
                pen.stroke(1.0, INK),
            ));
            // Turning arrow round the middle.
            let around = Pen::arc_points(12.0, 13.0, 10.0, 3.5, 0.15 * PI, 0.95 * PI);
            pen.line(&around, 2.0, ACCENT);
            let (x, y) = around[0];
            pen.head((x + 1.2, y - 1.6), 1.0, -1.2, 4.0, ACCENT);
        }
        Loft => {
            // A square seen from the side at the bottom, a circle (an ellipse) at the top,
            // their sides joined.
            let bottom = [(3.0, 18.0), (13.0, 22.0), (21.0, 18.0), (11.0, 14.0)];
            let top = Pen::arc_points(12.0, 5.5, 5.0, 2.2, 0.0, TAU);
            pen.poly(
                &[
                    (3.0, 18.0),
                    (7.0, 5.5),
                    (17.0, 5.5),
                    (21.0, 18.0),
                    (13.0, 22.0),
                ],
                BODY,
                INK,
            );
            pen.poly(&bottom, BODY_TOP, INK);
            pen.fill(&top, BODY_TOP);
            pen.closed(&top, 1.2, ACCENT);
        }
        Sweep => {
            // A round profile at the bottom left, carried along a bent path to the top
            // right: a pipe elbow, drawn as its two straight runs and the bend.
            let path = {
                let mut p = vec![(5.0, 21.0), (5.0, 13.0)];
                p.extend(Pen::arc_points(11.0, 13.0, 6.0, 6.0, PI, 1.5 * PI));
                p.push((19.0, 7.0));
                p
            };
            // The pipe: the path drawn thick, outlined.
            pen.line(&path, 7.4, INK);
            pen.line(&path, 5.6, BODY);
            // The profile at the start, and the path as a sketch line down the middle.
            pen.painter.add(Shape::convex_polygon(
                pen.pts(&Pen::arc_points(5.0, 21.0, 3.0, 1.4, 0.0, TAU)),
                pen.c(BODY_TOP),
                pen.stroke(1.0, INK),
            ));
            pen.dashed(&path, 1.2, SKETCH, 2.5, 1.5);
            pen.head((21.5, 7.0), 1.0, 0.0, 4.0, ACCENT);
        }
        Fillet => {
            // Front face with its top right corner rounded.
            let mut front = vec![(3.0, 21.0), (3.0, 9.0), (11.0, 9.0)];
            front.extend(Pen::arc_points(11.0, 15.0, 6.0, 6.0, -FRAC_PI_2, 0.0));
            front.push((17.0, 21.0));
            pen.poly(&front, BODY, INK);
            pen.poly(
                &[(3.0, 9.0), (7.0, 5.0), (15.0, 5.0), (11.0, 9.0)],
                BODY_TOP,
                INK,
            );
            // The rounded band, in quads.
            let near = Pen::arc_points(11.0, 15.0, 6.0, 6.0, -FRAC_PI_2, 0.0);
            let far: Vec<(f32, f32)> = near.iter().map(|&(x, y)| (x + 4.0, y - 4.0)).collect();
            for k in 0..near.len() - 1 {
                pen.fill(&[near[k], far[k], far[k + 1], near[k + 1]], ACCENT);
            }
            pen.line(&near, 1.0, INK);
            pen.line(&far, 1.0, INK);
            pen.poly(
                &[(17.0, 15.0), (21.0, 11.0), (21.0, 17.0), (17.0, 21.0)],
                BODY_SIDE,
                INK,
            );
        }
        Chamfer => {
            pen.poly(
                &[
                    (3.0, 21.0),
                    (3.0, 9.0),
                    (12.0, 9.0),
                    (17.0, 14.0),
                    (17.0, 21.0),
                ],
                BODY,
                INK,
            );
            pen.poly(
                &[(3.0, 9.0), (7.0, 5.0), (16.0, 5.0), (12.0, 9.0)],
                BODY_TOP,
                INK,
            );
            pen.poly(
                &[(12.0, 9.0), (16.0, 5.0), (21.0, 10.0), (17.0, 14.0)],
                ACCENT,
                INK,
            );
            pen.poly(
                &[(17.0, 14.0), (21.0, 10.0), (21.0, 17.0), (17.0, 21.0)],
                BODY_SIDE,
                INK,
            );
        }
        Shell => {
            pen.solid(2.0, 10.0, 13.0, 11.0, 6.0);
            // The cavity seen through the open top.
            pen.poly(
                &[(5.0, 9.0), (8.5, 5.5), (16.5, 5.5), (13.0, 9.0)],
                BODY_SIDE,
                INK,
            );
            pen.fill(&[(5.0, 9.0), (8.5, 5.5), (8.5, 7.0), (5.0, 10.5)], INK);
            pen.line(&[(13.0, 9.0), (16.5, 5.5)], 1.6, ACCENT);
            pen.line(&[(5.0, 9.0), (13.0, 9.0)], 1.6, ACCENT);
        }
        Hole => {
            pen.solid(2.0, 10.0, 14.0, 11.0, 6.0);
            // A counterbored hole in the top face, its axis going down into the block.
            let counterbore = Pen::arc_points(12.0, 7.0, 4.4, 1.9, 0.0, TAU);
            pen.fill(&counterbore, BODY_SIDE);
            pen.closed(&counterbore, 1.2, INK);
            let hole = Pen::arc_points(12.0, 7.6, 2.3, 1.0, 0.0, TAU);
            pen.fill(&hole, INK);
            pen.dashed(&[(12.0, 9.0), (12.0, 23.0)], 1.0, ACCENT, 2.0, 1.5);
        }
        LinearPattern => {
            for x in [2.0, 9.5, 17.0] {
                pen.solid(x, 7.0, 4.5, 4.5, 2.0);
            }
            pen.arrow((2.0, 17.0), (22.5, 17.0), 1.8, ACCENT);
            for x in [4.25, 11.75, 19.25] {
                pen.line(&[(x, 15.0), (x, 19.0)], 1.0, INK);
            }
        }
        CircularPattern => {
            pen.dashed(
                &Pen::arc_points(12.0, 12.0, 8.0, 8.0, 0.0, TAU),
                1.0,
                INK,
                2.0,
                1.5,
            );
            for k in 0..6 {
                let a = k as f32 * TAU / 6.0 - FRAC_PI_2;
                let filled = if k == 0 { ACCENT } else { BODY };
                pen.circle(
                    12.0 + 8.0 * a.cos(),
                    12.0 + 8.0 * a.sin(),
                    2.6,
                    filled,
                    Some((1.0, INK)),
                );
            }
            pen.line(&[(10.0, 12.0), (14.0, 12.0)], 1.0, INK);
            pen.line(&[(12.0, 10.0), (12.0, 14.0)], 1.0, INK);
        }
        Mirror => {
            pen.poly(&[(3.0, 19.0), (10.0, 19.0), (10.0, 6.0)], BODY, INK);
            pen.poly(&[(14.0, 19.0), (21.0, 19.0), (14.0, 6.0)], BODY_TOP, INK);
            pen.dashed(&[(12.0, 2.0), (12.0, 23.0)], 1.6, ACCENT, 2.5, 1.5);
        }
        RefPlane => {
            pen.plane(1.0, 7.0, 22.0, 11.0);
            pen.line(&[(8.0, 12.5), (16.0, 12.5)], 1.0, PLANE_EDGE);
        }
        RefAxis => {
            pen.plane(1.0, 12.0, 18.0, 9.0);
            pen.dashed(&[(4.0, 22.0), (20.0, 2.0)], 1.8, SKETCH, 4.0, 1.5);
        }
        ReferenceGeometry => {
            pen.plane(1.0, 9.0, 20.0, 10.0);
            pen.dashed(&[(14.0, 23.0), (22.0, 1.0)], 1.6, SKETCH, 3.5, 1.5);
            pen.circle(8.0, 14.0, 1.8, SKETCH, None);
        }
        Part => {
            pen.solid(3.0, 10.0, 12.0, 11.0, 6.0);
        }
        Imported => {
            // A solid with a page corner (the file) and an arrow coming in.
            pen.solid(2.0, 11.0, 12.0, 11.0, 5.0);
            pen.poly(
                &[
                    (13.0, 1.5),
                    (19.5, 1.5),
                    (22.5, 4.5),
                    (22.5, 11.0),
                    (13.0, 11.0),
                ],
                WHITE,
                INK,
            );
            pen.line(&[(19.5, 1.5), (19.5, 4.5), (22.5, 4.5)], 1.0, INK);
            pen.arrow((17.5, 6.5), (11.0, 13.0), 1.8, ACCENT);
        }
        Plane => {
            pen.plane(1.0, 6.0, 22.0, 12.0);
        }
        Origin => {
            pen.arrow((6.0, 18.0), (21.0, 18.0), 1.8, AXIS_X);
            pen.arrow((6.0, 18.0), (6.0, 3.0), 1.8, AXIS_Y);
            pen.circle(6.0, 18.0, 2.0, SKETCH, None);
        }
        Select => {
            let tip = (6.0, 3.0);
            pen.fill(&[tip, (6.0, 18.0), (16.5, 13.5)], WHITE);
            pen.fill(
                &[(9.0, 14.0), (11.5, 13.0), (14.5, 19.5), (12.0, 20.5)],
                WHITE,
            );
            pen.closed(
                &[
                    tip,
                    (6.0, 18.0),
                    (9.6, 14.6),
                    (12.0, 20.5),
                    (14.5, 19.5),
                    (12.1, 13.8),
                    (16.5, 13.5),
                ],
                1.4,
                INK,
            );
        }
        Line => {
            pen.line(&[(4.0, 19.0), (20.0, 5.0)], 2.0, SKETCH);
            pen.vertex(4.0, 19.0, INK);
            pen.vertex(20.0, 5.0, INK);
        }
        Centerline => {
            pen.dashed(&[(3.0, 20.0), (21.0, 4.0)], 1.8, SKETCH, 5.0, 2.0);
        }
        Rectangle => {
            pen.closed(
                &[(4.0, 6.0), (20.0, 6.0), (20.0, 18.0), (4.0, 18.0)],
                2.0,
                SKETCH,
            );
            for (x, y) in [(4.0, 6.0), (20.0, 6.0), (20.0, 18.0), (4.0, 18.0)] {
                pen.vertex(x, y, INK);
            }
        }
        CenterRectangle => {
            pen.dashed(&[(4.0, 6.0), (20.0, 18.0)], 1.0, INK, 2.0, 1.5);
            pen.dashed(&[(20.0, 6.0), (4.0, 18.0)], 1.0, INK, 2.0, 1.5);
            pen.closed(
                &[(4.0, 6.0), (20.0, 6.0), (20.0, 18.0), (4.0, 18.0)],
                2.0,
                SKETCH,
            );
            pen.circle(12.0, 12.0, 2.0, AXIS_X, None);
        }
        Convert => {
            // An edge of the part copied down into the sketch.
            pen.solid(3.0, 6.0, 11.0, 7.0, 4.0);
            pen.line(&[(3.0, 13.0), (14.0, 13.0)], 2.2, ACCENT);
            pen.arrow((8.5, 14.5), (8.5, 19.0), 1.4, ACCENT);
            pen.line(&[(3.0, 21.0), (21.0, 21.0)], 2.2, SKETCH);
        }
        Circle => {
            pen.ring(12.0, 12.0, 8.5, 2.0, SKETCH);
            pen.vertex(12.0, 12.0, INK);
        }
        Arc => {
            pen.arc(12.0, 15.0, 9.0, PI, TAU, 2.0, SKETCH);
            pen.vertex(12.0, 15.0, INK);
            pen.vertex(3.0, 15.0, INK);
            pen.vertex(21.0, 15.0, INK);
        }
        ThreePointArc => {
            pen.arc(12.0, 17.0, 9.0, PI, TAU, 2.0, SKETCH);
            pen.vertex(3.0, 17.0, INK);
            pen.vertex(21.0, 17.0, INK);
            pen.vertex(12.0, 8.0, ACCENT);
        }
        TangentArc => {
            pen.line(&[(2.0, 19.0), (12.0, 19.0)], 2.0, SKETCH);
            pen.arc(12.0, 12.0, 7.0, FRAC_PI_2, -FRAC_PI_2, 2.0, ACCENT);
            pen.vertex(12.0, 19.0, INK);
            pen.vertex(12.0, 5.0, INK);
        }
        Slot => {
            // Two half circles of radius 5 about (7, 12) and (17, 12), joined.
            pen.arc(7.0, 12.0, 5.0, FRAC_PI_2, 3.0 * FRAC_PI_2, 2.0, SKETCH);
            pen.arc(17.0, 12.0, 5.0, -FRAC_PI_2, FRAC_PI_2, 2.0, SKETCH);
            pen.line(&[(7.0, 7.0), (17.0, 7.0)], 2.0, SKETCH);
            pen.line(&[(7.0, 17.0), (17.0, 17.0)], 2.0, SKETCH);
            pen.dashed(&[(7.0, 12.0), (17.0, 12.0)], 1.0, INK, 2.0, 1.5);
            pen.vertex(7.0, 12.0, INK);
            pen.vertex(17.0, 12.0, INK);
        }
        Polygon => {
            pen.ring(12.0, 12.0, 9.0, 0.8, INK);
            let corners: Vec<(f32, f32)> = (0..6)
                .map(|k| {
                    let a = k as f32 * TAU / 6.0;
                    (12.0 + 9.0 * a.cos(), 12.0 + 9.0 * a.sin())
                })
                .collect();
            pen.closed(&corners, 2.0, SKETCH);
            pen.vertex(12.0, 12.0, INK);
        }
        Point => {
            pen.line(&[(12.0, 5.0), (12.0, 19.0)], 0.8, INK);
            pen.line(&[(5.0, 12.0), (19.0, 12.0)], 0.8, INK);
            pen.circle(12.0, 12.0, 3.2, SKETCH, Some((1.2, INK)));
        }
        SmartDimension => {
            pen.line(&[(4.0, 20.0), (20.0, 20.0)], 2.0, SKETCH);
            pen.line(&[(4.0, 18.0), (4.0, 6.0)], 1.0, INK);
            pen.line(&[(20.0, 18.0), (20.0, 6.0)], 1.0, INK);
            pen.line(&[(7.0, 9.0), (17.0, 9.0)], 1.2, INK);
            pen.head((4.5, 9.0), -1.0, 0.0, 3.5, INK);
            pen.head((19.5, 9.0), 1.0, 0.0, 3.5, INK);
            pen.poly(
                &[(8.5, 2.0), (15.5, 2.0), (15.5, 7.0), (8.5, 7.0)],
                WHITE,
                ACCENT,
            );
        }
        Trim => {
            pen.line(&[(9.0, 2.0), (9.0, 22.0)], 2.0, SKETCH);
            pen.line(&[(2.0, 12.0), (9.0, 12.0)], 2.0, SKETCH);
            pen.dashed(&[(9.0, 12.0), (22.0, 12.0)], 1.8, REMOVE, 2.5, 1.5);
            pen.vertex(9.0, 12.0, INK);
            // Scissors.
            pen.ring(15.0, 18.5, 2.0, 1.4, INK);
            pen.ring(20.0, 18.5, 2.0, 1.4, INK);
            pen.line(&[(16.0, 16.8), (20.5, 9.0)], 1.4, INK);
            pen.line(&[(19.0, 16.8), (14.5, 9.0)], 1.4, INK);
        }
        Extend => {
            // A line, the piece added to it dashed with an arrow, and the line it reaches.
            pen.line(&[(19.0, 3.0), (19.0, 21.0)], 2.0, SKETCH);
            pen.line(&[(2.0, 12.0), (10.0, 12.0)], 2.0, SKETCH);
            pen.dashed(&[(10.0, 12.0), (18.0, 12.0)], 1.8, ACCENT, 2.5, 1.5);
            pen.head((17.0, 12.0), 1.0, 0.0, 3.5, ACCENT);
            pen.vertex(10.0, 12.0, INK);
        }
        SketchFillet => {
            // A corner cut back to a tangent arc; the sharp corner it replaced, dashed.
            pen.dashed(
                &[(10.0, 20.0), (3.0, 20.0), (3.0, 13.0)],
                1.0,
                INK,
                2.0,
                1.5,
            );
            pen.line(&[(21.0, 20.0), (10.0, 20.0)], 2.0, SKETCH);
            pen.line(&[(3.0, 13.0), (3.0, 3.0)], 2.0, SKETCH);
            pen.arc(10.0, 13.0, 7.0, FRAC_PI_2, PI, 2.0, ACCENT);
            pen.vertex(10.0, 20.0, INK);
            pen.vertex(3.0, 13.0, INK);
        }
        Offset => {
            // A contour and its copy at a distance inside it.
            pen.closed(
                &[(2.0, 3.0), (22.0, 3.0), (22.0, 21.0), (2.0, 21.0)],
                2.0,
                SKETCH,
            );
            pen.closed(
                &[(7.0, 8.0), (17.0, 8.0), (17.0, 16.0), (7.0, 16.0)],
                1.8,
                ACCENT,
            );
            pen.line(&[(2.0, 12.0), (7.0, 12.0)], 1.0, INK);
            pen.head((2.5, 12.0), -1.0, 0.0, 3.0, INK);
            pen.head((6.5, 12.0), 1.0, 0.0, 3.0, INK);
        }
        Construction => {
            pen.dashed(
                &Pen::arc_points(12.0, 12.0, 8.5, 8.5, 0.0, TAU),
                1.8,
                SKETCH,
                3.0,
                2.0,
            );
            pen.dashed(&[(3.0, 21.0), (21.0, 3.0)], 1.4, INK, 4.0, 1.5);
        }
        ExitSketch => {
            pen.plane(1.0, 6.0, 18.0, 11.0);
            pen.closed(
                &[(6.0, 14.0), (8.5, 9.0), (13.0, 9.0), (10.5, 14.0)],
                1.4,
                SKETCH,
            );
            pen.badge(17.0, 17.0, ADD);
            pen.check(17.0, 17.3, 2.6, WHITE);
        }
        Discard => {
            pen.plane(1.0, 6.0, 18.0, 11.0);
            pen.closed(
                &[(6.0, 14.0), (8.5, 9.0), (13.0, 9.0), (10.5, 14.0)],
                1.4,
                SKETCH,
            );
            pen.badge(17.0, 17.0, REMOVE);
            pen.cross(17.0, 17.0, 2.2, WHITE);
        }
        Combine => {
            // Two bodies overlapping, the overlap in the accent colour.
            pen.solid(2.0, 11.0, 11.0, 10.0, 4.0);
            pen.block(9.0, 7.0, 11.0, 10.0, 4.0, [BODY_TOP, ACCENT_LIGHT, ACCENT]);
            pen.fill(&[(9.0, 11.0), (13.0, 11.0), (13.0, 17.0), (9.0, 17.0)], ADD);
        }
        Appearance => {
            // A shaded sphere with a highlight, and three paint dots.
            pen.circle(12.0, 13.5, 7.5, BODY_SIDE, Some((1.2, INK)));
            let cap: Vec<(f32, f32)> =
                Pen::arc_points(12.0, 13.5, 7.5, 7.5, PI + 0.35, 2.0 * PI - 0.35)
                    .into_iter()
                    .chain(Pen::arc_points(
                        12.0,
                        13.5,
                        7.5,
                        4.5,
                        2.0 * PI - 0.35,
                        PI + 0.35,
                    ))
                    .collect();
            pen.fill(&cap, BODY_TOP);
            pen.circle(9.5, 10.5, 1.8, WHITE, None);
            pen.circle(4.5, 4.5, 2.6, ACCENT, Some((1.0, INK)));
            pen.circle(11.0, 3.2, 2.6, SKETCH, Some((1.0, INK)));
            pen.circle(17.5, 4.5, 2.6, ADD, Some((1.0, INK)));
        }
        Bodies => {
            // A stack of separate solids.
            pen.solid(3.0, 15.0, 9.0, 7.0, 3.0);
            pen.solid(13.0, 15.0, 8.0, 7.0, 3.0);
            pen.block(7.0, 7.0, 9.0, 6.0, 3.0, [BODY_TOP, ACCENT_LIGHT, ACCENT]);
        }
        Intersect => {
            pen.ring(9.0, 12.0, 6.5, 1.6, INK);
            pen.ring(15.0, 12.0, 6.5, 1.6, INK);
            let lens: Vec<(f32, f32)> = Pen::arc_points(9.0, 12.0, 6.5, 6.5, -1.05, 1.05)
                .into_iter()
                .chain(Pen::arc_points(15.0, 12.0, 6.5, 6.5, PI - 1.05, PI + 1.05))
                .collect();
            pen.fill(&lens, ACCENT);
        }
        NewBody => {
            pen.solid(2.0, 12.0, 11.0, 10.0, 5.0);
            pen.badge(18.0, 6.0, SKETCH);
            pen.line(&[(15.0, 6.0), (21.0, 6.0)], 1.8, WHITE);
            pen.line(&[(18.0, 3.0), (18.0, 9.0)], 1.8, WHITE);
        }
        Assembly => {
            // Two parts, one resting on the other.
            pen.solid(2.0, 13.0, 12.0, 9.0, 5.0);
            pen.block(10.0, 7.0, 7.0, 6.0, 4.0, [BODY_TOP, ACCENT_LIGHT, ACCENT]);
        }
        InsertComponent => {
            pen.solid(2.0, 12.0, 11.0, 10.0, 5.0);
            pen.badge(18.0, 6.0, ADD);
            pen.line(&[(15.0, 6.0), (21.0, 6.0)], 1.8, WHITE);
            pen.line(&[(18.0, 3.0), (18.0, 9.0)], 1.8, WHITE);
        }
        Mate => {
            // Two faces brought together.
            pen.block(1.0, 12.0, 9.0, 10.0, 4.0, [BODY_TOP, BODY, BODY_SIDE]);
            pen.block(13.0, 6.0, 8.0, 9.0, 4.0, [BODY_TOP, ACCENT_LIGHT, ACCENT]);
            pen.arrow((7.0, 4.0), (12.0, 9.0), 1.8, SKETCH);
            pen.arrow((19.0, 21.0), (13.0, 17.0), 1.8, SKETCH);
        }
        Move => {
            pen.solid(2.0, 11.0, 10.0, 9.0, 4.0);
            let c = (17.0, 8.0);
            pen.arrow(c, (17.0, 1.5), 1.6, SKETCH);
            pen.arrow(c, (17.0, 14.5), 1.6, SKETCH);
            pen.arrow(c, (10.5, 8.0), 1.6, SKETCH);
            pen.arrow(c, (23.0, 8.0), 1.6, SKETCH);
        }
        Materials => {
            // Four swatches, one textured.
            pen.poly(
                &[(2.0, 2.0), (11.0, 2.0), (11.0, 11.0), (2.0, 11.0)],
                BODY,
                INK,
            );
            pen.poly(
                &[(13.0, 2.0), (22.0, 2.0), (22.0, 11.0), (13.0, 11.0)],
                ACCENT,
                INK,
            );
            pen.poly(
                &[(2.0, 13.0), (11.0, 13.0), (11.0, 22.0), (2.0, 22.0)],
                ADD,
                INK,
            );
            pen.poly(
                &[(13.0, 13.0), (22.0, 13.0), (22.0, 22.0), (13.0, 22.0)],
                BODY_TOP,
                INK,
            );
            for k in [15.5, 18.0, 20.5] {
                pen.line(&[(13.0, k + 2.0), (k + 2.0, 13.0)], 1.0, BODY_SIDE);
            }
        }
        BillOfMaterials => {
            pen.poly(
                &[(4.0, 2.0), (20.0, 2.0), (20.0, 22.0), (4.0, 22.0)],
                WHITE,
                INK,
            );
            for y in [7.0, 12.0, 17.0] {
                pen.line(&[(4.0, y), (20.0, y)], 1.0, INK);
            }
            pen.line(&[(9.0, 2.0), (9.0, 22.0)], 1.0, INK);
            pen.fill(&[(4.0, 2.0), (20.0, 2.0), (20.0, 7.0), (4.0, 7.0)], BODY);
        }
        MassProperties => {
            pen.ring(12.0, 6.0, 2.6, 1.6, INK);
            pen.poly(
                &[(6.0, 9.0), (18.0, 9.0), (21.0, 21.0), (3.0, 21.0)],
                BODY_SIDE,
                INK,
            );
            pen.line(&[(9.0, 15.0), (15.0, 15.0)], 1.6, WHITE);
        }
        Measure => {
            pen.poly(
                &[(2.0, 9.0), (22.0, 9.0), (22.0, 16.0), (2.0, 16.0)],
                ACCENT_LIGHT,
                INK,
            );
            for (k, x) in [5.0, 8.0, 11.0, 14.0, 17.0, 20.0].into_iter().enumerate() {
                let depth = if k % 2 == 0 { 4.0 } else { 2.5 };
                pen.line(&[(x, 9.0), (x, 9.0 + depth)], 1.0, INK);
            }
        }
        SectionView => {
            // The kept half of a block, its cut face in the section colour, and the
            // plane as a line where the other half was.
            pen.poly(
                &[(3.0, 9.0), (12.0, 9.0), (12.0, 21.0), (3.0, 21.0)],
                BODY,
                INK,
            );
            pen.poly(
                &[(3.0, 9.0), (7.0, 4.0), (16.0, 4.0), (12.0, 9.0)],
                BODY_TOP,
                INK,
            );
            pen.poly(
                &[(12.0, 9.0), (16.0, 4.0), (16.0, 16.0), (12.0, 21.0)],
                ACCENT,
                INK,
            );
            pen.line(&[(19.5, 2.0), (19.5, 22.0)], 1.2, INK);
        }
        Interference => {
            // Two blocks run into each other; what they share is red.
            pen.poly(
                &[(2.0, 20.0), (2.0, 9.0), (13.0, 9.0), (13.0, 20.0)],
                BODY,
                INK,
            );
            pen.poly(
                &[(2.0, 9.0), (6.0, 5.0), (17.0, 5.0), (13.0, 9.0)],
                BODY_TOP,
                INK,
            );
            pen.poly(
                &[(10.0, 22.0), (10.0, 13.0), (22.0, 13.0), (22.0, 22.0)],
                BODY_SIDE,
                INK,
            );
            pen.poly(
                &[(10.0, 13.0), (13.0, 10.0), (22.0, 10.0), (22.0, 13.0)],
                BODY_TOP,
                INK,
            );
            pen.poly(
                &[(10.0, 20.0), (10.0, 13.0), (13.0, 13.0), (13.0, 20.0)],
                REMOVE,
                INK,
            );
        }
        Render => {
            // The shadow on the floor, the ball, its highlight.
            pen.fill(
                &Pen::arc_points(12.0, 20.0, 8.0, 2.2, 0.0, TAU),
                Color32::from_rgb(120, 130, 140),
            );
            pen.circle(12.0, 11.0, 7.5, BODY_SIDE, Some((1.0, INK)));
            pen.circle(12.0, 11.0, 5.0, BODY, None);
            pen.circle(9.6, 8.4, 2.2, WHITE, None);
        }
        SaveImage => {
            pen.poly(
                &[(2.0, 4.0), (22.0, 4.0), (22.0, 20.0), (2.0, 20.0)],
                Color32::from_rgb(214, 232, 248),
                INK,
            );
            pen.fill(&[(4.0, 18.0), (10.0, 9.0), (15.0, 18.0)], BODY_SIDE);
            pen.fill(&[(11.0, 18.0), (16.0, 12.0), (20.0, 18.0)], BODY);
            pen.circle(17.0, 8.0, 2.0, ACCENT, None);
        }
        ExportMesh => {
            pen.solid(3.0, 11.0, 10.0, 9.0, 4.0);
            pen.arrow((12.0, 9.0), (21.0, 3.0), 2.0, ACCENT);
        }
        Fixture => {
            pen.solid(8.0, 9.0, 12.0, 10.0, 4.0);
            pen.line(&[(5.5, 4.0), (5.5, 21.0)], 1.6, ADD);
            for k in 0..5 {
                let y = 6.0 + 3.5 * k as f32;
                pen.line(&[(5.5, y), (2.5, y + 2.5)], 1.2, ADD);
            }
            pen.head((8.0, 14.0), 1.0, 0.0, 3.6, ADD);
        }
        Roller => {
            pen.solid(4.0, 6.0, 14.0, 9.0, 4.0);
            for x in [7.0, 12.0, 17.0] {
                pen.circle(x, 17.5, 2.2, ADD, Some((1.0, INK)));
            }
            pen.line(&[(2.0, 21.0), (22.0, 21.0)], 1.4, ADD);
        }
        Force => {
            pen.solid(3.0, 13.0, 13.0, 8.0, 4.0);
            pen.arrow((14.0, 1.5), (10.0, 11.5), 2.2, LOAD);
        }
        Pressure => {
            pen.solid(3.0, 13.0, 13.0, 8.0, 4.0);
            for x in [6.0, 11.0, 16.0] {
                pen.arrow((x, 2.0), (x, 10.5), 1.6, LOAD);
            }
        }
        Gravity => {
            pen.solid(4.0, 5.0, 11.0, 8.0, 4.0);
            pen.arrow((11.5, 11.0), (11.5, 22.0), 2.6, INK);
        }
        Mesh => {
            pen.solid(3.0, 9.0, 13.0, 12.0, 5.0);
            pen.line(
                &[(3.0, 21.0), (9.5, 9.0), (9.5, 21.0), (16.0, 9.0)],
                1.0,
                INK,
            );
            pen.line(&[(3.0, 15.0), (16.0, 15.0)], 1.0, INK);
            pen.line(&[(3.0, 9.0), (8.0, 4.0)], 1.0, INK);
            pen.line(&[(9.5, 9.0), (14.5, 4.0), (16.0, 9.0)], 1.0, INK);
            pen.line(&[(16.0, 15.0), (21.0, 4.0)], 1.0, INK);
        }
        RunStudy => {
            pen.poly(&[(6.0, 3.0), (20.0, 12.0), (6.0, 21.0)], ADD, INK);
        }
        Stress => {
            // Front face in bands, top and side in the end colours.
            let bands = [
                Color32::from_rgb(0, 60, 255),
                Color32::from_rgb(0, 200, 255),
                Color32::from_rgb(40, 220, 60),
                Color32::from_rgb(250, 220, 0),
                Color32::from_rgb(240, 40, 20),
            ];
            pen.poly(
                &[(3.0, 9.0), (7.0, 5.0), (21.0, 5.0), (17.0, 9.0)],
                Color32::from_rgb(0, 60, 255),
                INK,
            );
            for (k, color) in bands.iter().enumerate() {
                let x = 3.0 + 2.8 * k as f32;
                pen.fill(
                    &[(x, 9.0), (x + 2.8, 9.0), (x + 2.8, 20.0), (x, 20.0)],
                    *color,
                );
            }
            pen.poly(
                &[(17.0, 9.0), (21.0, 5.0), (21.0, 16.0), (17.0, 20.0)],
                Color32::from_rgb(240, 40, 20),
                INK,
            );
            pen.closed(
                &[(3.0, 9.0), (17.0, 9.0), (17.0, 20.0), (3.0, 20.0)],
                1.0,
                INK,
            );
        }
        Displacement => {
            pen.solid(3.0, 10.0, 11.0, 10.0, 4.0);
            pen.dashed(
                &[
                    (8.0, 6.0),
                    (19.0, 3.0),
                    (22.0, 6.0),
                    (22.0, 14.0),
                    (11.0, 17.0),
                    (8.0, 14.0),
                    (8.0, 6.0),
                ],
                1.2,
                LOAD,
                2.0,
                1.6,
            );
        }
        Safety => {
            pen.poly(
                &[
                    (12.0, 2.0),
                    (20.0, 5.0),
                    (19.0, 14.0),
                    (12.0, 22.0),
                    (5.0, 14.0),
                    (4.0, 5.0),
                ],
                BODY,
                INK,
            );
            pen.check(12.0, 12.0, 4.0, ADD);
        }
        NewStudy => {
            pen.poly(
                &[(4.0, 2.0), (16.0, 2.0), (16.0, 22.0), (4.0, 22.0)],
                WHITE,
                INK,
            );
            for y in [7.0, 11.0, 15.0] {
                pen.line(&[(7.0, y), (13.0, y)], 1.2, INK);
            }
            pen.circle(17.0, 17.0, 6.0, ADD, Some((1.0, WHITE)));
            pen.line(&[(14.0, 17.0), (20.0, 17.0)], 2.2, WHITE);
            pen.line(&[(17.0, 14.0), (17.0, 20.0)], 2.2, WHITE);
        }
        Temperature => {
            // A thermometer: the tube, the bulb, the red column.
            pen.poly(
                &[(10.0, 3.0), (14.0, 3.0), (14.0, 15.0), (10.0, 15.0)],
                WHITE,
                INK,
            );
            pen.circle(12.0, 18.0, 4.5, HOT, Some((1.2, INK)));
            pen.fill(&[(11.0, 8.0), (13.0, 8.0), (13.0, 16.0), (11.0, 16.0)], HOT);
            for y in [6.0, 9.0, 12.0] {
                pen.line(&[(14.0, y), (16.5, y)], 1.0, INK);
            }
        }
        HeatPower => {
            pen.solid(3.0, 13.0, 13.0, 8.0, 4.0);
            pen.arrow((4.0, 3.0), (8.0, 11.0), 1.6, HEAT);
            pen.arrow((15.0, 3.0), (12.0, 11.0), 1.6, HEAT);
            // A flame.
            pen.poly(
                &[
                    (19.0, 2.0),
                    (22.0, 7.0),
                    (20.5, 10.0),
                    (17.5, 10.0),
                    (16.5, 6.5),
                ],
                HEAT,
                HOT,
            );
        }
        HeatFlux => {
            pen.solid(3.0, 13.0, 13.0, 8.0, 4.0);
            for x in [6.0, 11.0, 16.0] {
                pen.line(&[(x - 1.0, 2.0), (x + 1.0, 4.5), (x - 1.0, 7.0)], 1.4, HEAT);
                pen.arrow((x - 1.0, 7.0), (x, 10.5), 1.4, HEAT);
            }
        }
        Convection => {
            pen.solid(3.0, 13.0, 13.0, 8.0, 4.0);
            for (k, y) in [3.0f32, 6.5, 10.0].into_iter().enumerate() {
                let x = 6.0 + k as f32;
                pen.line(
                    &Pen::arc_points(x + 3.0, y, 3.0, 1.4, PI, 2.0 * PI)
                        .into_iter()
                        .chain(Pen::arc_points(x + 9.0, y, 3.0, 1.4, PI, 0.0))
                        .collect::<Vec<_>>(),
                    1.5,
                    COOL,
                );
            }
            pen.head((20.0, 3.0), 1.0, 0.0, 3.0, COOL);
        }
        Radiation => {
            pen.solid(3.0, 13.0, 13.0, 8.0, 4.0);
            for (x, y) in [(5.0, 10.0), (10.0, 10.0), (15.0, 10.0)] {
                pen.line(
                    &[
                        (x, y),
                        (x + 1.5, y - 2.0),
                        (x, y - 4.0),
                        (x + 1.5, y - 6.0),
                        (x, y - 8.0),
                    ],
                    1.4,
                    HEAT,
                );
            }
            pen.circle(20.0, 4.0, 2.4, HOT, None);
        }
        InternalHeat => {
            pen.solid(3.0, 9.0, 13.0, 12.0, 5.0);
            pen.circle(9.5, 15.0, 3.6, HEAT, Some((1.0, HOT)));
            pen.circle(9.5, 15.0, 1.6, Color32::from_rgb(255, 225, 120), None);
        }
        TemperaturePlot => {
            let bands = [
                Color32::from_rgb(0, 60, 255),
                Color32::from_rgb(0, 200, 255),
                Color32::from_rgb(40, 220, 60),
                Color32::from_rgb(250, 220, 0),
                Color32::from_rgb(240, 40, 20),
            ];
            for (k, color) in bands.iter().enumerate() {
                let y = 20.0 - 2.4 * (k + 1) as f32;
                pen.fill(
                    &[(2.0, y), (13.0, y), (13.0, y + 2.4), (2.0, y + 2.4)],
                    *color,
                );
            }
            pen.closed(
                &[(2.0, 8.0), (13.0, 8.0), (13.0, 20.0), (2.0, 20.0)],
                1.0,
                INK,
            );
            pen.poly(
                &[(16.5, 3.0), (19.5, 3.0), (19.5, 14.0), (16.5, 14.0)],
                WHITE,
                INK,
            );
            pen.circle(18.0, 17.0, 3.6, HOT, Some((1.0, INK)));
            pen.fill(&[(17.3, 7.0), (18.7, 7.0), (18.7, 15.0), (17.3, 15.0)], HOT);
        }
        HeatFluxPlot => {
            pen.solid(3.0, 9.0, 15.0, 12.0, 4.0);
            let colors = [
                Color32::from_rgb(0, 60, 255),
                Color32::from_rgb(40, 220, 60),
                Color32::from_rgb(240, 40, 20),
            ];
            for (k, color) in colors.iter().enumerate() {
                let y = 12.0 + 3.0 * k as f32;
                pen.arrow((1.0, y), (21.0, y), 1.5, *color);
            }
        }
        CoolingStudy => {
            // A heat sink: a warm base and three fins, air flowing between them.
            pen.poly(
                &[(4.0, 16.0), (20.0, 16.0), (20.0, 20.0), (4.0, 20.0)],
                HOT,
                INK,
            );
            for x in [5.0, 10.5, 16.0] {
                pen.poly(
                    &[(x, 6.0), (x + 3.0, 6.0), (x + 3.0, 16.0), (x, 16.0)],
                    BODY,
                    INK,
                );
            }
            pen.arrow((1.0, 4.0), (23.0, 4.0), 1.5, COOL);
            pen.line(&[(8.0, 11.0), (10.5, 11.0)], 1.5, COOL);
            pen.line(&[(13.5, 11.0), (16.0, 11.0)], 1.5, COOL);
        }
        FlowStudy => {
            // A body, and three streamlines bending round it.
            pen.poly(
                &[(9.0, 9.0), (15.0, 9.0), (15.0, 15.0), (9.0, 15.0)],
                BODY,
                INK,
            );
            pen.line(
                &[
                    (2.0, 6.0),
                    (7.0, 6.0),
                    (12.0, 4.5),
                    (17.0, 6.0),
                    (22.0, 6.0),
                ],
                1.5,
                COOL,
            );
            pen.line(&[(2.0, 12.0), (7.0, 12.0)], 1.5, COOL);
            pen.line(&[(17.0, 12.0), (22.0, 12.0)], 1.5, COOL);
            pen.line(
                &[
                    (2.0, 18.0),
                    (7.0, 18.0),
                    (12.0, 19.5),
                    (17.0, 18.0),
                    (22.0, 18.0),
                ],
                1.5,
                COOL,
            );
            pen.head((22.0, 6.0), 1.0, 0.0, 3.0, COOL);
            pen.head((22.0, 18.0), 1.0, 0.0, 3.0, COOL);
        }
        Inlet => {
            // A lid, and the fluid coming in through it.
            pen.poly(
                &[(14.0, 3.0), (18.0, 3.0), (18.0, 21.0), (14.0, 21.0)],
                BODY_TOP,
                INK,
            );
            for y in [7.0, 12.0, 17.0] {
                pen.arrow((2.0, y), (13.0, y), 1.6, COOL);
            }
        }
        Outlet => {
            // A lid, and the fluid going out past it.
            pen.poly(
                &[(6.0, 3.0), (10.0, 3.0), (10.0, 21.0), (6.0, 21.0)],
                BODY_TOP,
                INK,
            );
            for y in [7.0, 12.0, 17.0] {
                pen.arrow((11.0, y), (22.0, y), 1.6, COOL);
            }
        }
        IdealWall => {
            // A hatched wall, and the fluid gliding along it.
            pen.line(&[(3.0, 16.0), (21.0, 16.0)], 1.8, INK);
            for x in [5.0, 9.0, 13.0, 17.0, 21.0] {
                pen.line(&[(x, 16.0), (x - 3.0, 20.0)], 1.0, INK);
            }
            pen.arrow((3.0, 12.0), (21.0, 12.0), 1.6, COOL);
            pen.arrow((6.0, 7.0), (18.0, 7.0), 1.4, COOL);
        }
        Goal => {
            // A flag planted on a face.
            pen.solid(3.0, 15.0, 14.0, 6.0, 3.0);
            pen.line(&[(11.0, 15.0), (11.0, 2.0)], 1.6, INK);
            pen.poly(&[(11.0, 2.5), (20.0, 5.0), (11.0, 8.0)], ACCENT, INK);
        }
        CutPlot => {
            // A plane through the fluid, banded cold to hot.
            let bands = [
                Color32::from_rgb(0, 60, 255),
                Color32::from_rgb(0, 200, 255),
                Color32::from_rgb(40, 220, 60),
                Color32::from_rgb(250, 220, 0),
                Color32::from_rgb(240, 40, 20),
            ];
            for (k, color) in bands.iter().enumerate() {
                let x = 4.0 + 3.2 * k as f32;
                let skew = 4.0;
                pen.fill(
                    &[
                        (x + skew, 5.0),
                        (x + 3.2 + skew, 5.0),
                        (x + 3.2, 19.0),
                        (x, 19.0),
                    ],
                    *color,
                );
            }
            pen.closed(
                &[(8.0, 5.0), (24.0, 5.0), (20.0, 19.0), (4.0, 19.0)],
                1.0,
                INK,
            );
            pen.arrow((1.0, 12.0), (7.0, 12.0), 1.3, COOL);
        }
        SurfacePlot => {
            // A body's faces in plot colours, and the fluid going past.
            pen.block(
                5.0,
                10.0,
                11.0,
                10.0,
                4.0,
                [
                    Color32::from_rgb(250, 220, 0),
                    Color32::from_rgb(240, 40, 20),
                    Color32::from_rgb(0, 200, 255),
                ],
            );
            pen.arrow((1.0, 5.0), (12.0, 5.0), 1.4, COOL);
        }
        Configurations => {
            // A small table: three rows, the middle one marked as the active configuration.
            pen.poly(
                &[(3.0, 4.0), (21.0, 4.0), (21.0, 20.0), (3.0, 20.0)],
                WHITE,
                INK,
            );
            pen.line(&[(3.0, 9.5), (21.0, 9.5)], 1.0, INK);
            pen.line(&[(3.0, 15.0), (21.0, 15.0)], 1.0, INK);
            pen.line(&[(9.0, 4.0), (9.0, 20.0)], 1.0, INK);
            pen.fill(
                &[(3.5, 10.0), (20.5, 10.0), (20.5, 14.5), (3.5, 14.5)],
                ACCENT_LIGHT,
            );
            pen.circle(6.0, 12.25, 1.6, ACCENT, None);
            pen.line(&[(11.0, 12.25), (19.0, 12.25)], 1.6, INK);
            pen.line(&[(11.0, 6.75), (19.0, 6.75)], 1.2, BODY_SIDE);
            pen.line(&[(11.0, 17.5), (19.0, 17.5)], 1.2, BODY_SIDE);
        }
        Equations => {
            pen.line(
                &[
                    (18.0, 5.0),
                    (6.0, 5.0),
                    (13.0, 12.0),
                    (6.0, 19.0),
                    (18.0, 19.0),
                ],
                2.4,
                INK,
            );
            pen.line(&[(18.0, 5.0), (18.0, 7.5)], 2.4, INK);
            pen.line(&[(18.0, 19.0), (18.0, 16.5)], 2.4, INK);
        }
        Rebuild => {
            pen.arc(12.0, 12.0, 8.0, -0.2 * PI, 1.25 * PI, 2.4, ADD);
            pen.head((18.6, 6.0), 0.9, 1.1, 5.0, ADD);
            pen.arc(12.0, 12.0, 8.0, 1.25 * PI, 1.6 * PI, 2.4, ADD);
        }
        ViewFront => pen.view_cube(Some(1)),
        ViewTop => pen.view_cube(Some(0)),
        ViewRight => pen.view_cube(Some(2)),
        ViewIso => pen.view_cube(None),
        Fit => {
            pen.solid(8.0, 10.0, 6.0, 6.0, 3.0);
            for (x, y, dx, dy) in [
                (2.0, 2.0, 1.0, 1.0),
                (22.0, 2.0, -1.0, 1.0),
                (2.0, 22.0, 1.0, -1.0),
                (22.0, 22.0, -1.0, -1.0),
            ] {
                pen.line(&[(x + 5.0 * dx, y), (x, y), (x, y + 5.0 * dy)], 2.0, INK);
            }
        }
        NormalTo => {
            pen.poly(
                &[(3.0, 6.0), (17.0, 6.0), (17.0, 20.0), (3.0, 20.0)],
                PLANE,
                PLANE_EDGE,
            );
            pen.circle(10.0, 13.0, 3.6, WHITE, Some((1.6, ACCENT)));
            pen.circle(10.0, 13.0, 1.4, ACCENT, None);
            pen.arrow((15.0, 9.0), (22.0, 2.0), 1.6, ACCENT);
        }
        Eye => {
            let upper = Pen::arc_points(12.0, 17.0, 10.0, 8.0, -0.85 * PI, -0.15 * PI);
            let lower = Pen::arc_points(12.0, 7.0, 10.0, 8.0, 0.15 * PI, 0.85 * PI);
            pen.line(&upper, 1.8, INK);
            pen.line(&lower, 1.8, INK);
            pen.circle(12.0, 12.0, 3.4, SKETCH, Some((1.0, INK)));
        }
        EyeClosed => {
            let lower = Pen::arc_points(12.0, 6.0, 10.0, 8.0, 0.2 * PI, 0.8 * PI);
            pen.line(&lower, 1.8, INK);
            for x in [6.0, 12.0, 18.0] {
                pen.line(&[(x, 13.8), (x + (x - 12.0) * 0.15, 17.0)], 1.4, INK);
            }
        }
        Undo => {
            pen.arc(13.0, 14.0, 7.0, -FRAC_PI_2, 0.6 * PI, 2.2, INK);
            pen.line(&[(13.0, 7.0), (8.0, 7.0)], 2.2, INK);
            pen.head((3.5, 7.0), -1.0, 0.0, 5.5, INK);
        }
        Redo => {
            pen.arc(11.0, 14.0, 7.0, 0.4 * PI, 1.5 * PI, 2.2, INK);
            pen.line(&[(11.0, 7.0), (16.0, 7.0)], 2.2, INK);
            pen.head((20.5, 7.0), 1.0, 0.0, 5.5, INK);
        }
        Add => {
            pen.circle(12.0, 12.0, 10.0, ADD, None);
            pen.line(&[(7.0, 12.0), (17.0, 12.0)], 3.4, WHITE);
            pen.line(&[(12.0, 7.0), (12.0, 17.0)], 3.4, WHITE);
        }
        Remove => {
            pen.circle(12.0, 12.0, 10.0, REMOVE, None);
            pen.line(&[(7.0, 12.0), (17.0, 12.0)], 3.4, WHITE);
        }
        Ok => pen.check(12.0, 13.0, 7.0, ADD),
        Cancel => pen.cross(12.0, 12.0, 6.0, REMOVE),
        Error => {
            pen.circle(12.0, 12.0, 9.0, REMOVE, None);
            pen.line(&[(12.0, 6.5), (12.0, 13.5)], 2.6, WHITE);
            pen.circle(12.0, 17.0, 1.5, WHITE, None);
        }
        Warning => {
            pen.poly(&[(12.0, 2.5), (22.0, 20.5), (2.0, 20.5)], ACCENT, ACCENT);
            pen.line(&[(12.0, 8.5), (12.0, 14.5)], 2.4, WHITE);
            pen.circle(12.0, 17.5, 1.4, WHITE, None);
        }
    }
}

/// A small icon in a row of text, e.g. in the timeline or a menu.
pub fn small(ui: &mut Ui, icon: Icon, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint(ui.painter(), rect, icon, ui.is_enabled());
    response
}

/// A command button in the command bar: a large icon over its label.
pub fn command_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    enabled: bool,
    active: bool,
) -> Response {
    // Wide enough for the longest word of the label; longer labels wrap.
    let label_width = label
        .split_whitespace()
        .map(|word| {
            ui.painter()
                .layout_no_wrap(
                    word.to_owned(),
                    egui::FontId::proportional(11.0),
                    Color32::WHITE,
                )
                .size()
                .x
        })
        .fold(0.0f32, f32::max);
    let width = (label_width + 10.0).max(68.0);
    let height = 66.0;
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), sense);
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let painter = ui.painter();
        if active {
            painter.rect(
                rect.shrink(1.0),
                4.0,
                visuals.selection.bg_fill.gamma_multiply(0.35),
                Stroke::new(1.0_f32, visuals.selection.stroke.color),
            );
        } else if enabled && response.hovered() {
            painter.rect_filled(rect.shrink(1.0), 4.0, visuals.widgets.hovered.weak_bg_fill);
        }
        let icon_rect = Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() + 20.0),
            Vec2::splat(34.0),
        );
        paint(painter, icon_rect, icon, enabled);
        let color = if enabled {
            visuals.text_color()
        } else {
            visuals.weak_text_color()
        };
        let galley = painter.layout(
            label.to_owned(),
            egui::FontId::proportional(11.0),
            color,
            width - 4.0,
        );
        let mut job_rect = galley.rect;
        job_rect = job_rect.translate(Vec2::new(
            rect.center().x - job_rect.width() / 2.0,
            rect.top() + 39.0,
        ));
        painter.galley(job_rect.min, galley, color);
    }
    response
}

/// Marks a command button as opening a menu: a small triangle in its corner.
pub fn dropdown_marker(ui: &Ui, button: &Response) {
    let corner = button.rect.right_bottom() - Vec2::new(9.0, 9.0);
    let color = ui.visuals().text_color();
    ui.painter().add(Shape::convex_polygon(
        vec![
            corner + Vec2::new(-4.0, -2.0),
            corner + Vec2::new(4.0, -2.0),
            corner + Vec2::new(0.0, 3.0),
        ],
        color,
        Stroke::NONE,
    ));
}

/// The "remove material" badge over a feature icon (cut extrusions, revolves and sweeps).
pub fn cut_badge(painter: &egui::Painter, icon_rect: Rect) {
    let r = icon_rect.width() * 0.24;
    let c = icon_rect.right_bottom() - Vec2::splat(r * 0.9);
    painter.circle(c, r, REMOVE, Stroke::new(1.0_f32, WHITE));
    painter.line_segment(
        [c - Vec2::new(r * 0.55, 0.0), c + Vec2::new(r * 0.55, 0.0)],
        Stroke::new(1.6_f32, WHITE),
    );
}

/// A small square icon button, e.g. in the heads-up view toolbar or a panel header.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: f32, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), sense);
    if ui.is_rect_visible(rect) {
        if enabled && response.hovered() {
            ui.painter()
                .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        paint(ui.painter(), rect.shrink(size * 0.12), icon, enabled);
    }
    response
}

/// Every icon, for previewing the set (`icon_sheet` test).
#[cfg(test)]
pub const ALL: [Icon; 103] = {
    use Icon::*;
    [
        Sketch,
        Extrude,
        Revolve,
        Sweep,
        Loft,
        Fillet,
        Chamfer,
        Shell,
        Hole,
        LinearPattern,
        CircularPattern,
        Mirror,
        RefPlane,
        RefAxis,
        ReferenceGeometry,
        Imported,
        Combine,
        Bodies,
        Intersect,
        NewBody,
        Assembly,
        InsertComponent,
        Mate,
        BillOfMaterials,
        Move,
        Materials,
        Part,
        Plane,
        Origin,
        Select,
        Line,
        Centerline,
        Rectangle,
        CenterRectangle,
        Circle,
        Arc,
        ThreePointArc,
        TangentArc,
        Slot,
        Polygon,
        Point,
        SmartDimension,
        Trim,
        Extend,
        SketchFillet,
        Offset,
        Convert,
        Construction,
        ExitSketch,
        Discard,
        MassProperties,
        Measure,
        SectionView,
        Interference,
        Configurations,
        Equations,
        Rebuild,
        Render,
        SaveImage,
        ExportMesh,
        Fixture,
        Roller,
        Force,
        Pressure,
        Gravity,
        Mesh,
        RunStudy,
        Stress,
        Displacement,
        Safety,
        NewStudy,
        Temperature,
        HeatPower,
        HeatFlux,
        Convection,
        Radiation,
        InternalHeat,
        TemperaturePlot,
        HeatFluxPlot,
        FlowStudy,
        CoolingStudy,
        Inlet,
        Outlet,
        IdealWall,
        Goal,
        CutPlot,
        SurfacePlot,
        ViewFront,
        ViewTop,
        ViewRight,
        ViewIso,
        Fit,
        NormalTo,
        Undo,
        Redo,
        Eye,
        EyeClosed,
        Add,
        Remove,
        Ok,
        Cancel,
        Error,
        Warning,
    ]
};

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Color32, Shape, epaint};

    use super::{ALL, paint};

    fn color(c: Color32) -> String {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        format!("rgba({r},{g},{b},{:.3})", a as f32 / 255.0)
    }

    fn solid(mode: &epaint::ColorMode) -> Color32 {
        match mode {
            epaint::ColorMode::Solid(c) => *c,
            _ => Color32::BLACK,
        }
    }

    fn svg(shape: &Shape, out: &mut String) {
        match shape {
            Shape::Vec(shapes) => shapes.iter().for_each(|s| svg(s, out)),
            Shape::Circle(c) => out.push_str(&format!(
                "<circle cx='{}' cy='{}' r='{}' fill='{}' stroke='{}' stroke-width='{}'/>\n",
                c.center.x,
                c.center.y,
                c.radius,
                color(c.fill),
                color(c.stroke.color),
                c.stroke.width
            )),
            Shape::LineSegment { points, stroke } => out.push_str(&format!(
                "<line x1='{}' y1='{}' x2='{}' y2='{}' stroke='{}' stroke-width='{}' stroke-linecap='round'/>\n",
                points[0].x, points[0].y, points[1].x, points[1].y,
                color(solid(&stroke.color)),
                stroke.width
            )),
            Shape::Path(p) => {
                let points: Vec<String> =
                    p.points.iter().map(|q| format!("{},{}", q.x, q.y)).collect();
                let tag = if p.closed { "polygon" } else { "polyline" };
                let stroke = solid(&p.stroke.color);
                let fill = if p.closed { color(p.fill) } else { "none".to_owned() };
                out.push_str(&format!(
                    "<{tag} points='{}' fill='{fill}' stroke='{}' stroke-width='{}' stroke-linejoin='round' stroke-linecap='round'/>\n",
                    points.join(" "),
                    color(stroke),
                    p.stroke.width
                ));
            }
            _ => {}
        }
    }

    /// Writes every icon at two sizes to the SVG file named by `OXYDE_ICON_SHEET`.
    #[test]
    #[ignore = "writes a preview file"]
    fn icon_sheet() {
        let Ok(path) = std::env::var("OXYDE_ICON_SHEET") else {
            return;
        };
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            for (i, icon) in ALL.iter().enumerate() {
                let (col, row) = ((i % 8) as f32, (i / 8) as f32);
                let big = egui::Rect::from_min_size(
                    egui::pos2(20.0 + col * 120.0, 20.0 + row * 110.0),
                    egui::vec2(64.0, 64.0),
                );
                paint(&painter, big, *icon, true);
                let small = egui::Rect::from_min_size(
                    big.right_top() + egui::vec2(10.0, 0.0),
                    egui::vec2(20.0, 20.0),
                );
                paint(&painter, small, *icon, true);
                let off = egui::Rect::from_min_size(
                    small.left_bottom() + egui::vec2(0.0, 8.0),
                    egui::vec2(28.0, 28.0),
                );
                paint(&painter, off, *icon, false);
            }
        });
        let mut body = String::new();
        for clipped in &output.shapes {
            svg(&clipped.shape, &mut body);
        }
        let mut labels = String::new();
        for (i, icon) in ALL.iter().enumerate() {
            let (col, row) = ((i % 8) as f32, (i / 8) as f32);
            labels.push_str(&format!(
                "<text x='{}' y='{}' font-size='11' font-family='sans-serif'>{icon:?}</text>\n",
                20.0 + col * 120.0,
                98.0 + row * 110.0
            ));
        }
        let sheet = format!(
            "<svg xmlns='http://www.w3.org/2000/svg' width='980' height='700' style='background:#f4f4f4'>\n{body}{labels}</svg>\n"
        );
        std::fs::write(path, sheet).unwrap();
    }
}
