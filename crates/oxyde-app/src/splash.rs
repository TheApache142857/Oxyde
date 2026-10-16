//! The splash screen, as in Blender: a card over the window while Oxyde starts, with the
//! version and a thin loading bar; once loaded it offers new documents and recent files,
//! until a click outside it or Escape.

use std::path::PathBuf;

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2,
};

use crate::app::{OxydeApp, PendingAction};

/// Steps of starting up, shown one per frame so the bar moves.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    Kernel,
    Viewport,
    Open(PathBuf),
    Done,
}

pub struct Splash {
    pub open: bool,
    steps: Vec<Step>,
    total: usize,
    status: String,
    /// Shown progress, eased towards the real one.
    shown: f32,
    /// When the loading finished (seconds), to fade the bar.
    finished_at: Option<f64>,
    /// Started with a file: the card goes once it is open.
    auto_close: bool,
    started_at: Option<f64>,
}

/// The card's size.
const CARD: Vec2 = Vec2::new(600.0, 360.0);
const ART_HEIGHT: f32 = 236.0;
/// Shortest time the loading bar is seen, so it does not flash by.
const MIN_LOADING: f64 = 0.9;

impl Splash {
    pub fn new(file: Option<PathBuf>) -> Self {
        let mut steps = vec![Step::Kernel, Step::Viewport];
        let auto_close = file.is_some();
        if let Some(file) = file {
            steps.push(Step::Open(file));
        }
        steps.push(Step::Done);
        Self {
            open: true,
            total: steps.len(),
            steps,
            status: "Starting".to_owned(),
            shown: 0.0,
            finished_at: None,
            auto_close,
            started_at: None,
        }
    }

    fn loading(&self) -> bool {
        self.finished_at.is_none()
    }

    fn progress(&self) -> f32 {
        1.0 - self.steps.len() as f32 / self.total as f32
    }
}

/// The Oxyde mark: a part seen in isometric, its top face pierced by a bore, outlined in
/// the accent colour. `size` is its height.
pub fn draw_mark(painter: &egui::Painter, center: Pos2, size: f32, accent: Color32, face: Color32) {
    let s = size / 2.0;
    let (dx, dy) = (s * 0.866, s * 0.5);
    let top = center + Vec2::new(0.0, -s);
    let p = |x: f32, y: f32| center + Vec2::new(x, y);
    // Top, left and right faces of a cube.
    let upper = [top, p(dx, -dy), center, p(-dx, -dy)];
    let left = [p(-dx, -dy), center, p(0.0, s), p(-dx, dy)];
    let right = [center, p(dx, -dy), p(dx, dy), p(0.0, s)];
    let shade = |c: Color32, k: f32| {
        Color32::from_rgb(
            (c.r() as f32 * k) as u8,
            (c.g() as f32 * k) as u8,
            (c.b() as f32 * k) as u8,
        )
    };
    let stroke = Stroke::new((size / 48.0).max(1.0), accent);
    painter.add(Shape::convex_polygon(
        upper.to_vec(),
        shade(face, 1.0),
        stroke,
    ));
    painter.add(Shape::convex_polygon(
        left.to_vec(),
        shade(face, 0.72),
        stroke,
    ));
    painter.add(Shape::convex_polygon(
        right.to_vec(),
        shade(face, 0.55),
        stroke,
    ));
    // The bore: an ellipse on the top face.
    let c = top + (center - top) * 0.5;
    let ring: Vec<Pos2> = (0..40)
        .map(|k| {
            let a = k as f32 / 40.0 * std::f32::consts::TAU;
            c + Vec2::new(a.cos() * dx * 0.42, a.sin() * dy * 0.42)
        })
        .collect();
    painter.add(Shape::convex_polygon(ring, shade(face, 0.3), stroke));
}

impl OxydeApp {
    /// Does one start-up step per frame.
    pub(crate) fn splash_step(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let splash = &mut self.splash;
        let started = *splash.started_at.get_or_insert(now);
        if splash.steps.is_empty() {
            return;
        }
        // Each step gets its share of the minimum time, so the bar is seen to move.
        let share = MIN_LOADING / splash.total as f64;
        let due = started + share * (splash.total - splash.steps.len()) as f64;
        ctx.request_repaint();
        if now < due {
            return;
        }
        let step = splash.steps.remove(0);
        splash.status = match splash.steps.first() {
            Some(Step::Viewport) => {
                format!("Preparing the viewport ({})", self.viewport.renderer_name())
            }
            Some(Step::Open(path)) => format!(
                "Opening {}",
                path.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            ),
            Some(Step::Done) | None => "Ready".to_owned(),
            Some(Step::Kernel) => String::new(),
        };
        match step {
            Step::Kernel | Step::Viewport => {}
            Step::Open(path) => self.open_path(&path),
            Step::Done => {
                self.splash.finished_at = Some(now);
                if self.splash.auto_close {
                    self.splash.open = false;
                }
            }
        }
    }

    pub(crate) fn splash_ui(&mut self, ctx: &egui::Context) {
        if !self.splash.open {
            return;
        }
        let t = *crate::theme::tokens();
        let screen = ctx.screen_rect();
        let now = ctx.input(|i| i.time);
        let target = self.splash.progress();
        self.splash.shown += (target - self.splash.shown) * 0.25;
        // Loaded: the bar is full, whatever the easing reached (egui stops repainting when
        // idle, so it may never get there by itself).
        if !self.splash.loading() {
            self.splash.shown = 1.0;
        }
        let mut close = false;
        let mut action: Option<SplashAction> = None;

        egui::Area::new(egui::Id::new("splash"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                // The dimmed window behind: a click there closes the card.
                let (backdrop, response) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter()
                    .rect_filled(backdrop, 0.0, Color32::from_black_alpha(120));
                let card = Rect::from_center_size(screen.center(), CARD);
                let clicked_outside = response.clicked()
                    && response
                        .interact_pointer_pos()
                        .is_some_and(|p| !card.contains(p));
                if clicked_outside && !self.splash.loading() {
                    close = true;
                }
                let painter = ui.painter().clone();
                painter.rect_filled(card.expand(1.0), 10.0, t.line);
                painter.rect_filled(card, 10.0, t.panel);

                // The art: a navy gradient, a faint grid, the mark and the name.
                // Inset so its square corners stay inside the card's rounded ones.
                let art = Rect::from_min_size(
                    card.min + Vec2::splat(3.0),
                    Vec2::new(CARD.x - 6.0, ART_HEIGHT),
                );
                let mut mesh = egui::Mesh::default();
                let (top, bottom) = (Color32::from_rgb(28, 44, 60), Color32::from_rgb(11, 17, 24));
                mesh.colored_vertex(art.left_top(), top);
                mesh.colored_vertex(art.right_top(), top);
                mesh.colored_vertex(art.right_bottom(), bottom);
                mesh.colored_vertex(art.left_bottom(), bottom);
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                painter.add(mesh);
                // A faint grid, fading out towards the bottom.
                let mut x = art.left() + 24.0;
                while x < art.right() {
                    for k in 0..4 {
                        let y0 = art.top() + art.height() * k as f32 / 4.0;
                        let alpha = 16 - 4 * k as u8;
                        painter.vline(
                            x,
                            y0..=y0 + art.height() / 4.0,
                            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(79, 195, 247, alpha)),
                        );
                    }
                    x += 24.0;
                }
                let mut y = art.top() + 24.0;
                while y < art.bottom() {
                    let k = (y - art.top()) / art.height();
                    let alpha = (16.0 * (1.0 - k)) as u8;
                    painter.hline(
                        art.x_range(),
                        y,
                        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(79, 195, 247, alpha)),
                    );
                    y += 24.0;
                }
                draw_mark(
                    &painter,
                    art.right_center() + Vec2::new(-150.0, 6.0),
                    150.0,
                    t.accent,
                    Color32::from_rgb(150, 168, 186),
                );
                painter.text(
                    art.left_bottom() + Vec2::new(28.0, -58.0),
                    Align2::LEFT_BOTTOM,
                    "Oxyde",
                    FontId::proportional(46.0),
                    Color32::from_rgb(232, 240, 246),
                );
                painter.text(
                    art.left_bottom() + Vec2::new(30.0, -32.0),
                    Align2::LEFT_BOTTOM,
                    "Parametric CAD · Rust + OpenCASCADE",
                    FontId::proportional(13.0),
                    Color32::from_rgb(138, 160, 178),
                );
                painter.text(
                    art.right_top() + Vec2::new(-16.0, 14.0),
                    Align2::RIGHT_TOP,
                    format!("{} · pre-alpha", env!("CARGO_PKG_VERSION")),
                    FontId::proportional(12.0),
                    Color32::from_rgb(200, 214, 226),
                );

                // Below the art: progress while loading, then what to do.
                let below = Rect::from_min_max(
                    art.left_bottom() + Vec2::new(20.0, 14.0),
                    card.right_bottom() - Vec2::new(20.0, 12.0),
                );
                if self.splash.loading() {
                    painter.text(
                        below.left_top(),
                        Align2::LEFT_TOP,
                        &self.splash.status,
                        FontId::proportional(12.0),
                        t.dim,
                    );
                } else {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(below));
                    action = welcome(&mut child, &self.prefs.recent);
                }
                // The thin bar along the bottom edge, fading out once loaded.
                let fade = self.splash.finished_at.map_or(1.0, |at| {
                    (1.0 - ((now - at - 0.3) / 0.4).clamp(0.0, 1.0)) as f32
                });
                if fade > 0.0 {
                    let track = Rect::from_min_max(
                        Pos2::new(card.left() + 10.0, card.bottom() - 4.0),
                        Pos2::new(card.right() - 10.0, card.bottom() - 2.0),
                    );
                    painter.rect_filled(track, 1.0, t.line.gamma_multiply(fade));
                    let mut fill = track;
                    fill.set_right(
                        track.left() + track.width() * self.splash.shown.clamp(0.0, 1.0),
                    );
                    painter.rect_filled(fill, 1.0, t.accent.gamma_multiply(fade));
                    ctx.request_repaint();
                }
            });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !self.splash.loading() {
            close = true;
        }
        if let Some(action) = action {
            close = true;
            match action {
                SplashAction::New(pending) => self.guard(pending, ctx),
                SplashAction::Recent(path) => self.open_path(&path),
            }
        }
        if close {
            self.splash.open = false;
        }
    }

    /// Puts `path` at the top of the recent files, and saves the preferences.
    pub(crate) fn remember_recent(&mut self, path: &std::path::Path) {
        let path = path.to_path_buf();
        let recent = &mut self.prefs.recent;
        recent.retain(|p| *p != path);
        recent.insert(0, path);
        recent.truncate(10);
        let _ = self.prefs.save();
    }
}

enum SplashAction {
    New(PendingAction),
    Recent(PathBuf),
}

/// New documents on the left, recent files on the right.
fn welcome(ui: &mut egui::Ui, recent: &[PathBuf]) -> Option<SplashAction> {
    let t = crate::theme::tokens();
    let mut action = None;
    ui.columns(2, |cols| {
        let title = |ui: &mut egui::Ui, text: &str| {
            ui.label(
                RichText::new(text.to_uppercase())
                    .size(11.0)
                    .extra_letter_spacing(0.6)
                    .color(t.dim),
            );
        };
        title(&mut cols[0], "New");
        for (label, pending) in [
            ("New part", PendingAction::New),
            ("New assembly", PendingAction::NewAssembly),
            ("Open…", PendingAction::Open),
        ] {
            if cols[0].link(label).clicked() {
                action = Some(SplashAction::New(pending));
            }
        }
        title(&mut cols[1], "Recent files");
        let existing: Vec<&PathBuf> = recent.iter().filter(|p| p.exists()).take(4).collect();
        if existing.is_empty() {
            cols[1].label(RichText::new("Nothing yet").italics().color(t.dim));
        }
        for path in existing {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            if cols[1]
                .link(name)
                .on_hover_text(path.display().to_string())
                .clicked()
            {
                action = Some(SplashAction::Recent(path.clone()));
            }
        }
    });
    action
}
