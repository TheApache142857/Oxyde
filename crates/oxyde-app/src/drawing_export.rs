//! File › Export › Drawing: the model's standard views (front, top, right, isometric) on
//! an ISO sheet with a frame and a title block, as PDF, SVG or DXF, the first step of
//! the drawings of `docs/ARCHITECTURE.md` §8. The kernel removes the hidden lines
//! (`Kernel::project_edges`) of the part's bodies, or of an assembly's visible parts
//! where they sit; `oxyde_format::drawing` lays the views out and writes them.

use eframe::egui::{self, Color32, Pos2, Stroke};
use oxyde_format::drawing::{
    self, Pen, Projection, Sheet, SheetSize, TitleBlock, ViewEdges, ViewKind,
};
use oxyde_kernel::Kernel;

use crate::app::OxydeApp;

/// The export window's settings, and the views as last projected.
pub struct DrawingExport {
    pub size: SheetSize,
    pub projection: Projection,
    /// Which of `ViewKind::ALL` are on the sheet.
    pub views: [bool; 4],
    pub show_hidden: bool,
    /// A scale of `drawing::SCALES`, or `None` for the largest that fits.
    pub scale: Option<f64>,
    pub title: TitleBlock,
    /// Every view, or why they could not be made, for the document revision they were
    /// projected at (a failure is not tried again until the document changes).
    projected: Option<(u64, Projected)>,
}

/// The views of the model, or why they could not be made.
type Projected = Result<Vec<(ViewKind, ViewEdges)>, String>;

impl OxydeApp {
    /// File › Export › Drawing: opens the window, its title block filled in from the
    /// document.
    pub(crate) fn begin_drawing_export(&mut self) {
        let has_solid = match &self.asm {
            Some(asm) => !asm.leaves(&asm.assembly).is_empty(),
            None => self.doc.part.body().is_some(),
        };
        if !has_solid {
            self.status = "Nothing to draw: there is no solid yet".to_owned();
            return;
        }
        let (title, part_number, material) = match &self.asm {
            Some(asm) => (
                asm.display_name(),
                asm.properties.part_number.clone(),
                String::new(),
            ),
            None => (
                self.doc.display_name(),
                self.doc.properties.part_number.clone(),
                self.doc
                    .part
                    .material()
                    .map_or_else(|| self.doc.properties.material.clone(), |m| m.name.clone()),
            ),
        };
        self.drawing_export = Some(DrawingExport {
            size: SheetSize::A4,
            projection: Projection::FirstAngle,
            views: [true; 4],
            show_hidden: true,
            scale: None,
            title: TitleBlock {
                title,
                part_number,
                material,
                drawn_by: String::new(),
                date: drawing::today(),
            },
            projected: None,
        });
    }

    /// The revision the views are of: the assembly's, or the part's.
    fn drawn_revision(&self) -> u64 {
        match &self.asm {
            Some(asm) => asm.revision,
            None => self.doc.revision,
        }
    }

    /// Every view of the model, hidden lines removed, in model mm.
    fn project_views(&mut self) -> Result<Vec<(ViewKind, ViewEdges)>, String> {
        let shapes = self.export_shapes()?;
        let mut size: f64 = 1.0;
        for shape in &shapes.shapes {
            if let Ok((lo, hi)) = self.kernel.bounds(*shape) {
                size = size.max(lo.distance(hi));
            }
        }
        let deflection = (size * 1e-3).max(0.005);
        let mut views = Vec::new();
        let mut failure = None;
        for kind in ViewKind::ALL {
            match self
                .kernel
                .project_edges(&shapes.shapes, &kind.plane(), deflection)
            {
                Ok(Some(edges)) => {
                    let lines = |lines: Vec<Vec<oxyde_geom::Point2>>| {
                        lines
                            .into_iter()
                            .map(|l| l.into_iter().map(|p| [p.x, p.y]).collect())
                            .collect()
                    };
                    views.push((
                        kind,
                        ViewEdges {
                            visible: lines(edges.visible),
                            hidden: lines(edges.hidden),
                        },
                    ));
                }
                Ok(None) => {
                    failure = Some("This kernel cannot remove hidden lines".to_owned());
                    break;
                }
                Err(e) => {
                    failure = Some(format!("The {} view failed: {e}", kind.label()));
                    break;
                }
            }
        }
        self.release_temporary(&shapes);
        match failure {
            Some(e) => Err(e),
            None => Ok(views),
        }
    }

    /// The sheet as the window sets it up.
    fn drawing_sheet(d: &DrawingExport) -> Option<Sheet> {
        let (_, Ok(all)) = d.projected.as_ref()? else {
            return None;
        };
        let views: Vec<(ViewKind, ViewEdges)> = all
            .iter()
            .filter(|(kind, _)| {
                ViewKind::ALL
                    .iter()
                    .position(|k| k == kind)
                    .and_then(|i| d.views.get(i))
                    .copied()
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        Some(drawing::layout(
            views,
            d.size,
            d.projection,
            d.scale,
            d.title.clone(),
            d.show_hidden,
        ))
    }

    pub(crate) fn drawing_export_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.drawing_export.take() else {
            return;
        };
        // The views of the model as it is now.
        let revision = self.drawn_revision();
        if d.projected.as_ref().is_none_or(|(r, _)| *r != revision) {
            d.projected = Some((revision, self.project_views()));
        }
        let mut open = true;
        let mut save = false;
        egui::Window::new("Export Drawing")
            .open(&mut open)
            .resizable(false)
            .default_pos(ctx.screen_rect().center() - egui::vec2(260.0, 260.0))
            .show(ctx, |ui| {
                egui::Grid::new("drawing_export")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Sheet");
                        ui.horizontal(|ui| {
                            for size in SheetSize::ALL {
                                ui.radio_value(&mut d.size, size, size.label());
                            }
                        });
                        ui.end_row();
                        ui.label("Projection");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut d.projection, Projection::FirstAngle, "First angle")
                                .on_hover_text("ISO, Europe: the top view under the front view");
                            ui.radio_value(&mut d.projection, Projection::ThirdAngle, "Third angle")
                                .on_hover_text("ANSI, America: the top view above the front view");
                        });
                        ui.end_row();
                        ui.label("Views");
                        ui.horizontal(|ui| {
                            for (i, kind) in ViewKind::ALL.into_iter().enumerate() {
                                if let Some(on) = d.views.get_mut(i) {
                                    ui.checkbox(on, kind.label());
                                }
                            }
                        });
                        ui.end_row();
                        ui.label("Hidden lines");
                        ui.checkbox(&mut d.show_hidden, "Dashed");
                        ui.end_row();
                        ui.label("Scale");
                        let current = match d.scale {
                            Some(s) => drawing::scale_text(s),
                            None => "Automatic".to_owned(),
                        };
                        egui::ComboBox::from_id_salt("drawing_scale")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut d.scale, None, "Automatic");
                                for s in drawing::SCALES {
                                    ui.selectable_value(&mut d.scale, Some(s), drawing::scale_text(s));
                                }
                            });
                        ui.end_row();
                        for (label, value) in [
                            ("Title", &mut d.title.title),
                            ("Part number", &mut d.title.part_number),
                            ("Material", &mut d.title.material),
                            ("Drawn by", &mut d.title.drawn_by),
                            ("Date", &mut d.title.date),
                        ] {
                            ui.label(label);
                            ui.text_edit_singleline(value);
                            ui.end_row();
                        }
                    });
                if let Some((_, Err(e))) = &d.projected {
                    ui.colored_label(crate::theme::tokens().err, e);
                }
                if let Some(sheet) = Self::drawing_sheet(&d) {
                    preview(ui, &sheet);
                    ui.label(
                        egui::RichText::new(format!(
                            "Scale {}. PDF and SVG at the sheet's size; DXF at the views' true size.",
                            drawing::scale_text(sheet.scale)
                        ))
                        .small()
                        .weak(),
                    );
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            matches!(d.projected, Some((_, Ok(_)))),
                            egui::Button::new("Save…"),
                        )
                        .clicked()
                    {
                        save = true;
                    }
                });
            });
        if save && let Some(sheet) = Self::drawing_sheet(&d) {
            self.save_drawing(&sheet);
        }
        if open {
            self.drawing_export = Some(d);
        }
    }

    /// Asks where, and writes the sheet in the format its extension names.
    fn save_drawing(&mut self, sheet: &Sheet) {
        let name = match &self.asm {
            Some(asm) => asm.display_name(),
            None => self.doc.display_name(),
        };
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .add_filter("SVG", &["svg"])
            .add_filter("DXF", &["dxf"])
            .set_file_name(format!("{name}.pdf"))
            .save_file()
        else {
            return;
        };
        let has = |e: &str| crate::assembly_ui::has_extension(&path, e);
        if !(has("pdf") || has("svg") || has("dxf")) {
            path.set_extension("pdf");
        }
        let bytes = if crate::assembly_ui::has_extension(&path, "svg") {
            drawing::to_svg(sheet).into_bytes()
        } else if crate::assembly_ui::has_extension(&path, "dxf") {
            drawing::to_dxf(sheet).into_bytes()
        } else {
            drawing::to_pdf(sheet)
        };
        self.status = match std::fs::write(&path, bytes) {
            Ok(()) => format!("Drawing written to {}", path.display()),
            Err(e) => format!("Could not write {}: {e}", path.display()),
        };
    }
}

/// The sheet drawn small, white, in the window.
fn preview(ui: &mut egui::Ui, sheet: &Sheet) {
    let [w, h] = sheet.size.size();
    let width = 480.0_f32;
    let height = width * (h / w) as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::WHITE);
    let k = width as f64 / w;
    let at = |p: &[f64; 2]| {
        Pos2::new(
            rect.left() + (p[0] * k) as f32,
            rect.bottom() - (p[1] * k) as f32,
        )
    };
    for (pen, line) in sheet.lines() {
        let points: Vec<Pos2> = line.iter().map(at).collect();
        let stroke = Stroke::new(
            (pen.width() * k).max(0.5) as f32,
            if pen == Pen::Hidden {
                Color32::from_gray(110)
            } else {
                Color32::BLACK
            },
        );
        match pen.dash() {
            Some(pattern) => {
                let dash = pattern.first().copied().unwrap_or(3.0);
                let gap = pattern.get(1).copied().unwrap_or(1.5);
                painter.extend(egui::Shape::dashed_line(
                    &points,
                    stroke,
                    (dash * k).max(2.0) as f32,
                    (gap * k).max(1.5) as f32,
                ));
            }
            None => {
                painter.add(egui::Shape::line(points, stroke));
            }
        }
    }
    for (corner, size, text) in sheet.texts() {
        painter.text(
            at(&corner),
            egui::Align2::LEFT_BOTTOM,
            text,
            egui::FontId::proportional(((size * k) as f32).max(5.0)),
            Color32::BLACK,
        );
    }
}
