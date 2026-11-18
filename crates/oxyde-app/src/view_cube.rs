//! The view cube of the modern look: a small cube in the viewport's top-right corner, turned
//! as the model is, whose faces name the standard views. Clicking a face turns the view to
//! look at it; the home button beside it goes back to the isometric view. The cube is drawn
//! by egui over the image, so it is the same with either renderer.

use eframe::egui::{self, Pos2, Rect, Stroke, vec2};
use oxyde_render::{Camera, StandardView};

/// Half the cube's side, in points.
const HALF: f32 = 24.0;
/// The cube's centre, from the viewport's top-right corner, in points.
const INSET: f32 = 62.0;
/// A face turned further away than this (the cosine of its angle to the viewer) is not
/// drawn and cannot be clicked: it is seen edge-on.
const MIN_FACING: f64 = 0.05;
/// Below this facing a face's name would be squashed, so it is left out.
const LABEL_FACING: f64 = 0.35;

/// What a click on the cube does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CubeTarget {
    View(StandardView),
    Home,
}

impl CubeTarget {
    /// The view it turns to.
    pub(crate) fn view(self) -> StandardView {
        match self {
            CubeTarget::View(view) => view,
            CubeTarget::Home => StandardView::Isometric,
        }
    }
}

struct CubeFace {
    view: StandardView,
    label: &'static str,
    corners: [Pos2; 4],
    center: Pos2,
    facing: f64,
}

/// The cube laid out for one frame: the faces the viewer sees, and the home button.
pub(crate) struct ViewCube {
    faces: Vec<CubeFace>,
    home: Rect,
}

/// The six faces: outward normal, two directions across it, the view looking at it.
const FACES: [([f64; 3], [f64; 3], [f64; 3], StandardView, &str); 6] = [
    (
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        StandardView::Front,
        "FRONT",
    ),
    (
        [0.0, 0.0, -1.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        StandardView::Back,
        "BACK",
    ),
    (
        [1.0, 0.0, 0.0],
        [0.0, 0.0, -1.0],
        [0.0, 1.0, 0.0],
        StandardView::Right,
        "RIGHT",
    ),
    (
        [-1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0],
        StandardView::Left,
        "LEFT",
    ),
    (
        [0.0, 1.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 0.0, -1.0],
        StandardView::Top,
        "TOP",
    ),
    (
        [0.0, -1.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0],
        StandardView::Bottom,
        "BOTTOM",
    ),
];

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Whether `p` lies inside the convex quadrilateral `q`, whichever way it winds.
fn inside(q: &[Pos2; 4], p: Pos2) -> bool {
    let mut sign = 0.0f32;
    for (&a, &b) in q.iter().zip(q.iter().cycle().skip(1)) {
        let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if cross.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    sign != 0.0
}

impl ViewCube {
    /// The cube for the viewport `rect`, turned as `camera` sees the model.
    pub(crate) fn layout(rect: Rect, camera: &Camera) -> ViewCube {
        let (right, up, eye) = (camera.right(), camera.up(), camera.eye_direction());
        let centre = rect.right_top() + vec2(-INSET, INSET);
        let screen = |p: [f64; 3]| centre + vec2(dot(p, right) as f32, -dot(p, up) as f32) * HALF;
        let faces = FACES
            .iter()
            .filter_map(|&(n, u, v, view, label)| {
                let facing = dot(n, eye);
                if facing <= MIN_FACING {
                    return None;
                }
                let corner = |su: f64, sv: f64| {
                    screen([
                        n[0] + su * u[0] + sv * v[0],
                        n[1] + su * u[1] + sv * v[1],
                        n[2] + su * u[2] + sv * v[2],
                    ])
                };
                Some(CubeFace {
                    view,
                    label,
                    corners: [
                        corner(-1.0, -1.0),
                        corner(1.0, -1.0),
                        corner(1.0, 1.0),
                        corner(-1.0, 1.0),
                    ],
                    center: screen(n),
                    facing,
                })
            })
            .collect();
        let home = Rect::from_center_size(
            centre + vec2(-HALF * 2.0 - 4.0, -HALF * 1.2),
            vec2(20.0, 20.0),
        );
        ViewCube { faces, home }
    }

    /// What is under `p`: a face of the cube, the home button, or nothing.
    pub(crate) fn hit(&self, p: Pos2) -> Option<CubeTarget> {
        if self.home.contains(p) {
            return Some(CubeTarget::Home);
        }
        self.faces
            .iter()
            .find(|f| inside(&f.corners, p))
            .map(|f| CubeTarget::View(f.view))
    }

    /// Draws the cube and the home button, `hovered` lit.
    pub(crate) fn paint(&self, painter: &egui::Painter, hovered: Option<CubeTarget>) {
        let t = crate::theme::tokens();
        let edge = Stroke::new(1.0_f32, t.line);
        for face in &self.faces {
            let lit = hovered == Some(CubeTarget::View(face.view));
            let fill = if lit { t.accent_bg } else { t.panel2 };
            painter.add(egui::Shape::convex_polygon(
                face.corners.to_vec(),
                fill.gamma_multiply(0.92),
                edge,
            ));
            if face.facing >= LABEL_FACING {
                painter.text(
                    face.center,
                    egui::Align2::CENTER_CENTER,
                    face.label,
                    egui::FontId::proportional(9.0),
                    if lit { t.accent } else { t.dim },
                );
            }
        }
        // The home button: a small house.
        let lit = hovered == Some(CubeTarget::Home);
        let r = self.home;
        painter.rect_filled(r, 4.0, if lit { t.accent_bg } else { t.panel2 });
        painter.rect_stroke(r, 4.0, edge);
        let ink = Stroke::new(1.3_f32, if lit { t.accent } else { t.dim });
        let c = r.center();
        let roof = [
            c + vec2(-6.0, -1.0),
            c + vec2(0.0, -6.0),
            c + vec2(6.0, -1.0),
        ];
        painter.line_segment([roof[0], roof[1]], ink);
        painter.line_segment([roof[1], roof[2]], ink);
        let body = Rect::from_min_max(c + vec2(-4.0, -1.0), c + vec2(4.0, 5.0));
        painter.rect_stroke(body, 0.0, ink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))
    }

    fn views(cube: &ViewCube) -> Vec<StandardView> {
        cube.faces.iter().map(|f| f.view).collect()
    }

    #[test]
    fn the_isometric_view_shows_front_top_and_right() {
        let mut camera = Camera::default();
        camera.set_view(StandardView::Isometric);
        let cube = ViewCube::layout(viewport(), &camera);
        let mut seen = views(&cube);
        seen.sort_by_key(|v| format!("{v:?}"));
        assert_eq!(
            seen,
            vec![StandardView::Front, StandardView::Right, StandardView::Top]
        );
        // Each face is picked at its centre; the corner of the viewport picks nothing.
        for face in &cube.faces {
            assert_eq!(cube.hit(face.center), Some(CubeTarget::View(face.view)));
        }
        assert_eq!(cube.hit(Pos2::new(5.0, 590.0)), None);
        assert_eq!(cube.hit(cube.home.center()), Some(CubeTarget::Home));
        assert_eq!(CubeTarget::Home.view(), StandardView::Isometric);
    }

    #[test]
    fn a_standard_view_shows_one_face_square_on() {
        let mut camera = Camera::default();
        camera.set_view(StandardView::Top);
        let cube = ViewCube::layout(viewport(), &camera);
        assert_eq!(views(&cube), vec![StandardView::Top]);
        let face = &cube.faces[0];
        let side = (face.corners[1] - face.corners[0]).length();
        assert!((side - 2.0 * HALF).abs() < 1e-3, "{side}");
        // The cube sits in the top-right corner, inside the viewport.
        assert!(viewport().contains(face.center));
        assert!(face.center.x > 700.0 && face.center.y < 100.0);
    }

    #[test]
    fn points_inside_a_quad_whatever_its_winding() {
        let q = [
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
        ];
        let mut r = q;
        r.reverse();
        for quad in [q, r] {
            assert!(inside(&quad, Pos2::new(5.0, 5.0)));
            assert!(!inside(&quad, Pos2::new(15.0, 5.0)));
        }
    }
}
