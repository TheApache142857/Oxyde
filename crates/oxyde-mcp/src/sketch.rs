//! Sketches from shape descriptions: what a person draws with the sketch tools, written as
//! JSON, made into sketch entities with the relations and dimensions Oxyde's own tools give
//! them, so the sketch comes out fully defined and its dimensions can be edited by name
//! (`D3@Sketch2`).
//!
//! Items (each may carry `"id"`, a label to refer to its entities later, `"construction":
//! true`, and `"position": "free"` to leave it unplaced, or `"fixed"` to fix its reference
//! point instead of dimensioning it from the origin):
//!
//! - `{"type": "rect", "center" | "corner": [x, y], "size": [w, h], "radius": r}`: a centre
//!   or corner rectangle, rounded with tangent arcs when `radius` is given;
//! - `{"type": "circle", "center": [x, y], "diameter": d}` (or `"radius"`);
//! - `{"type": "slot", "start": [x, y], "end": [x, y], "width": w}`: a straight slot between
//!   two arc centres;
//! - `{"type": "polygon", "center": [x, y], "sides": n, "radius": r, "angle": deg}`: a
//!   regular polygon, its corners on a construction circle of radius `r`, the first at
//!   `angle` degrees from +x;
//! - `{"type": "polyline", "points": [...], "closed": true, "fillet": r}`: lines through the
//!   points; a point may be `{"at": [x, y], "fillet": r}` (a tangent arc at that corner) or
//!   `{"at": [x, y], "center": [cx, cy]}` (an arc from the previous point to this one around
//!   that centre, counter-clockwise unless `"cw": true`);
//! - `{"type": "line", "start": [x, y], "end": [x, y]}` (a centerline with
//!   `"construction": true`);
//! - `{"type": "arc", "center": [x, y], "start": [x, y], "end": [x, y]}`, counter-clockwise;
//! - `{"type": "point", "at": [x, y]}`: a lone point (where the Hole feature drills).
//!
//! Placement follows SolidWorks habits: a point on the origin is made coincident with it,
//! one level with it or above it gets a horizontal or vertical relation, any other is
//! dimensioned from it horizontally and vertically (ordinate style). Lines drawn
//! horizontal or vertical get the relation; the corners of a polyline that share an x or a
//! y through such lines share one dimension.

use std::collections::BTreeMap;
use std::f64::consts::{PI, TAU};

use oxyde_geom::{Plane, Point2, Point3};
use oxyde_sketch::{ConstrainedSketch, Constraint, EntityId};
use serde_json::Value;

use crate::args::{self, Result};

/// Below this a coordinate difference is zero (mm).
const EPS: f64 = 1e-7;

/// Builds sketch items into `sketch`, whose origin point is `origin`.
pub(crate) struct Builder<'a> {
    sketch: &'a mut ConstrainedSketch,
    origin: EntityId,
    origin_at: Point2,
    /// The plane, to take points given in model coordinates (`[x, y, z]`).
    plane: Plane,
    pub labels: BTreeMap<String, u32>,
}

impl<'a> Builder<'a> {
    pub fn new(sketch: &'a mut ConstrainedSketch, plane: Plane) -> Self {
        // The origin point sits where the model origin falls in the sketch plane.
        let v = Point3::ORIGIN - plane.origin;
        let origin_at = Point2::new(v.dot(plane.x_dir), v.dot(plane.y_dir()));
        let origin = sketch.ensure_origin(origin_at);
        Self {
            sketch,
            origin,
            origin_at,
            plane,
            labels: BTreeMap::new(),
        }
    }

    /// Adds every item; stops at the first one that is wrong.
    pub fn add_all(&mut self, items: &[Value]) -> Result<()> {
        for (index, item) in items.iter().enumerate() {
            self.add(item)
                .map_err(|e| format!("sketch item {}: {e}", index + 1))?;
        }
        Ok(())
    }

    pub fn add(&mut self, item: &Value) -> Result<()> {
        let kind = args::str(item, "type")?;
        let label = args::opt_str(item, "id")?.map(str::to_owned);
        let construction = args::bool(item, "construction", false)?;
        let place = match args::opt_str(item, "position")? {
            None | Some("dimensioned") => Place::Dimensioned,
            Some("free") => Place::Free,
            Some("fixed") => Place::Fixed,
            Some(other) => return Err(format!("unknown position \"{other}\"")),
        };
        let before = self.sketch.entities().len();
        let names: Vec<(String, EntityId)> = match kind {
            "rect" | "rectangle" => self.rect(item, place)?,
            "circle" => self.circle(item, place)?,
            "slot" => self.slot(item, place)?,
            "polygon" => self.polygon(item, place)?,
            "polyline" => self.polyline(item, place)?,
            "line" | "centerline" => {
                let start = self.point_arg(item, "start")?;
                let end = self.point_arg(item, "end")?;
                let names =
                    self.chain(&[Corner::sharp(start), Corner::sharp(end)], false, place)?;
                if kind == "centerline" {
                    for (_, id) in &names {
                        self.sketch.set_construction(*id, true);
                    }
                }
                // A lone line is its label itself; its ends are `start` and `end`.
                names
                    .into_iter()
                    .map(|(suffix, id)| {
                        let suffix = match suffix.as_str() {
                            "0" => "",
                            "p0" => "start",
                            "p1" => "end",
                            other => other,
                        };
                        (suffix.to_owned(), id)
                    })
                    .collect()
            }
            "arc" => self.arc(item, place)?,
            "text" => self.text(item)?,
            "point" => {
                let at = self.point_arg(item, "at")?;
                let p = self.sketch.point(at);
                self.locate(p, at, place);
                vec![(String::new(), p)]
            }
            other => return Err(format!("unknown sketch item type \"{other}\"")),
        };
        if construction {
            let new: Vec<EntityId> = self
                .sketch
                .entities()
                .iter()
                .skip(before)
                .map(|e| e.id)
                .collect();
            for id in new {
                self.sketch.set_construction(id, true);
            }
        }
        if let Some(label) = label {
            for (suffix, id) in names {
                let key = if suffix.is_empty() {
                    label.clone()
                } else {
                    format!("{label}.{suffix}")
                };
                self.labels.insert(key, id.0);
            }
        }
        Ok(())
    }

    /// `[x, y]` in the sketch, or `[x, y, z]` in model coordinates, projected.
    fn point(&self, value: &Value, what: &str) -> Result<Point2> {
        match value.as_array().map(Vec::len) {
            Some(3) => {
                let p = args::point3(value, what)?;
                let v = p - self.plane.origin;
                Ok(Point2::new(
                    v.dot(self.plane.x_dir),
                    v.dot(self.plane.y_dir()),
                ))
            }
            _ => args::point2(value, what),
        }
    }

    fn point_arg(&self, item: &Value, key: &str) -> Result<Point2> {
        self.point(args::get(item, key)?, &format!("\"{key}\""))
    }

    fn constrain(&mut self, c: Constraint) {
        self.sketch.constrain(c);
    }

    /// Places point `p` (at `at`) from the origin.
    fn locate(&mut self, p: EntityId, at: Point2, place: Place) {
        match place {
            Place::Free => {}
            Place::Fixed => self.constrain(Constraint::Fixed(p)),
            Place::Dimensioned => {
                self.locate_x(p, at.x);
                self.locate_y(p, at.y);
            }
        }
    }

    fn locate_x(&mut self, p: EntityId, x: f64) {
        let dx = x - self.origin_at.x;
        if dx.abs() < EPS {
            self.constrain(Constraint::VerticalPoints(self.origin, p));
        } else {
            self.constrain(Constraint::HorizontalDistance(self.origin, p, dx.abs()));
        }
    }

    fn locate_y(&mut self, p: EntityId, y: f64) {
        let dy = y - self.origin_at.y;
        if dy.abs() < EPS {
            self.constrain(Constraint::HorizontalPoints(self.origin, p));
        } else {
            self.constrain(Constraint::VerticalDistance(self.origin, p, dy.abs()));
        }
    }

    fn size(item: &Value) -> Result<(f64, f64)> {
        let size = args::get(item, "size")?;
        let p = args::point2(size, "\"size\"")?;
        if p.x > EPS && p.y > EPS {
            Ok((p.x, p.y))
        } else {
            Err("\"size\" must be [width, height], both more than 0".to_owned())
        }
    }

    fn rect(&mut self, item: &Value, place: Place) -> Result<Vec<(String, EntityId)>> {
        let (w, h) = Self::size(item)?;
        let (center, corner) = match (args::opt(item, "center"), args::opt(item, "corner")) {
            (Some(c), None) => {
                let c = self.point(c, "\"center\"")?;
                (Some(c), Point2::new(c.x - w / 2.0, c.y - h / 2.0))
            }
            (None, Some(c)) => (None, self.point(c, "\"corner\"")?),
            _ => return Err("a rectangle needs \"center\" or \"corner\"".to_owned()),
        };
        let radius = args::opt_f64(item, "radius")?.unwrap_or(0.0);
        if radius < 0.0 || (radius > 0.0 && 2.0 * radius >= w.min(h) - EPS) {
            return Err(format!(
                "a corner radius of {radius} does not fit a {w} x {h} rectangle"
            ));
        }
        if radius > 0.0 {
            return self.rounded_rect(center, corner, w, h, radius, place);
        }
        let s = &mut *self.sketch;
        let (x0, y0, x1, y1) = (corner.x, corner.y, corner.x + w, corner.y + h);
        let p = [
            s.point(Point2::new(x0, y0)),
            s.point(Point2::new(x1, y0)),
            s.point(Point2::new(x1, y1)),
            s.point(Point2::new(x0, y1)),
        ];
        let [p0, p1, p2, p3] = p;
        let lines = [
            s.line_between(p0, p1),
            s.line_between(p1, p2),
            s.line_between(p2, p3),
            s.line_between(p3, p0),
        ];
        let [bottom, right, top, left] = lines;
        s.constrain(Constraint::Horizontal(bottom));
        s.constrain(Constraint::Vertical(right));
        s.constrain(Constraint::Horizontal(top));
        s.constrain(Constraint::Vertical(left));
        s.constrain(Constraint::Length(bottom, w));
        s.constrain(Constraint::Length(right, h));
        let mut names = vec![
            ("bottom".to_owned(), bottom),
            ("right".to_owned(), right),
            ("top".to_owned(), top),
            ("left".to_owned(), left),
            ("p0".to_owned(), p0),
            ("p1".to_owned(), p1),
            ("p2".to_owned(), p2),
            ("p3".to_owned(), p3),
        ];
        match center {
            Some(c) => {
                // A centre rectangle, as SolidWorks draws it: a construction diagonal with
                // the centre point at its middle.
                let diagonal = s.line_between(p0, p2);
                s.set_construction(diagonal, true);
                let cp = s.point(c);
                s.set_construction(cp, true);
                s.constrain(Constraint::Midpoint(cp, diagonal));
                self.locate(cp, c, place);
                names.push(("center".to_owned(), cp));
            }
            None => self.locate(p0, corner, place),
        }
        Ok(names)
    }

    fn rounded_rect(
        &mut self,
        center: Option<Point2>,
        corner: Point2,
        w: f64,
        h: f64,
        r: f64,
        place: Place,
    ) -> Result<Vec<(String, EntityId)>> {
        let s = &mut *self.sketch;
        let (x0, y0, x1, y1) = (corner.x, corner.y, corner.x + w, corner.y + h);
        let pt = |s: &mut ConstrainedSketch, x: f64, y: f64| s.point(Point2::new(x, y));
        // Tangent points, two per side, counter-clockwise from the bottom side.
        let a0 = pt(s, x0 + r, y0);
        let a1 = pt(s, x1 - r, y0);
        let b0 = pt(s, x1, y0 + r);
        let b1 = pt(s, x1, y1 - r);
        let c0 = pt(s, x1 - r, y1);
        let c1 = pt(s, x0 + r, y1);
        let d0 = pt(s, x0, y1 - r);
        let d1 = pt(s, x0, y0 + r);
        let k0 = pt(s, x0 + r, y0 + r);
        let k1 = pt(s, x1 - r, y0 + r);
        let k2 = pt(s, x1 - r, y1 - r);
        let k3 = pt(s, x0 + r, y1 - r);
        let bottom = s.line_between(a0, a1);
        let right = s.line_between(b0, b1);
        let top = s.line_between(c0, c1);
        let left = s.line_between(d0, d1);
        let arc0 = s.arc_between(k0, d1, a0);
        let arc1 = s.arc_between(k1, a1, b0);
        let arc2 = s.arc_between(k2, b1, c0);
        let arc3 = s.arc_between(k3, c1, d0);
        s.constrain(Constraint::Horizontal(bottom));
        s.constrain(Constraint::Vertical(right));
        s.constrain(Constraint::Horizontal(top));
        s.constrain(Constraint::Vertical(left));
        for (line, arc) in [
            (bottom, arc0),
            (bottom, arc1),
            (right, arc1),
            (right, arc2),
            (top, arc2),
            (top, arc3),
            (left, arc3),
            (left, arc0),
        ] {
            s.constrain(Constraint::Tangent(line, arc));
        }
        s.constrain(Constraint::Radius(arc0, r));
        for arc in [arc1, arc2, arc3] {
            s.constrain(Constraint::EqualRadius(arc0, arc));
        }
        s.constrain(Constraint::LineDistance(left, right, w));
        s.constrain(Constraint::LineDistance(bottom, top, h));
        let mut names = vec![
            ("bottom".to_owned(), bottom),
            ("right".to_owned(), right),
            ("top".to_owned(), top),
            ("left".to_owned(), left),
            ("arc0".to_owned(), arc0),
            ("arc1".to_owned(), arc1),
            ("arc2".to_owned(), arc2),
            ("arc3".to_owned(), arc3),
        ];
        // The centre is the middle of the diagonal between two opposite arc centres.
        let diagonal = s.line_between(k0, k2);
        s.set_construction(diagonal, true);
        let c = center.unwrap_or(Point2::new(x0 + w / 2.0, y0 + h / 2.0));
        let cp = s.point(c);
        s.set_construction(cp, true);
        s.constrain(Constraint::Midpoint(cp, diagonal));
        names.push(("center".to_owned(), cp));
        match center {
            Some(_) => self.locate(cp, c, place),
            None => {
                // A corner rectangle is placed by its corner: the virtual sharp of the
                // first arc, on the bottom and left lines.
                let sharp = self.sketch.point(corner);
                self.sketch.set_construction(sharp, true);
                self.constrain(Constraint::PointOnLine(sharp, bottom));
                self.constrain(Constraint::PointOnLine(sharp, left));
                self.locate(sharp, corner, place);
            }
        }
        Ok(names)
    }

    fn circle(&mut self, item: &Value, place: Place) -> Result<Vec<(String, EntityId)>> {
        let center = self.point_arg(item, "center")?;
        let diameter = match (
            args::opt_f64(item, "diameter")?,
            args::opt_f64(item, "radius")?,
        ) {
            (Some(d), None) => d,
            (None, Some(r)) => 2.0 * r,
            _ => return Err("a circle needs \"diameter\" or \"radius\"".to_owned()),
        };
        if diameter <= EPS {
            return Err("a circle's diameter must be more than 0".to_owned());
        }
        let cp = self.sketch.point(center);
        let circle = self.sketch.circle_around(cp, diameter / 2.0);
        if args::opt(item, "radius").is_some() {
            self.constrain(Constraint::Radius(circle, diameter / 2.0));
        } else {
            self.constrain(Constraint::Diameter(circle, diameter));
        }
        self.locate(cp, center, place);
        Ok(vec![(String::new(), circle), ("center".to_owned(), cp)])
    }

    fn slot(&mut self, item: &Value, place: Place) -> Result<Vec<(String, EntityId)>> {
        let a = self.point_arg(item, "start")?;
        let b = self.point_arg(item, "end")?;
        let width = args::positive(item, "width")?;
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let length = dx.hypot(dy);
        if length <= EPS {
            return Err("a slot's start and end must differ".to_owned());
        }
        let (ux, uy) = (dx / length, dy / length);
        let (nx, ny) = (-uy, ux);
        let r = width / 2.0;
        let s = &mut *self.sketch;
        let ca = s.point(a);
        let cb = s.point(b);
        let a_left = s.point(Point2::new(a.x + nx * r, a.y + ny * r));
        let a_right = s.point(Point2::new(a.x - nx * r, a.y - ny * r));
        let b_left = s.point(Point2::new(b.x + nx * r, b.y + ny * r));
        let b_right = s.point(Point2::new(b.x - nx * r, b.y - ny * r));
        let right_side = s.line_between(a_right, b_right);
        let left_side = s.line_between(b_left, a_left);
        // Counter-clockwise from the right side to the left one goes round the far end.
        let arc_b = s.arc_between(cb, b_right, b_left);
        let arc_a = s.arc_between(ca, a_left, a_right);
        let centerline = s.line_between(ca, cb);
        s.set_construction(centerline, true);
        for (line, arc) in [
            (right_side, arc_b),
            (right_side, arc_a),
            (left_side, arc_a),
            (left_side, arc_b),
        ] {
            s.constrain(Constraint::Tangent(line, arc));
        }
        s.constrain(Constraint::EqualRadius(arc_a, arc_b));
        s.constrain(Constraint::Radius(arc_a, r));
        if dy.abs() < EPS {
            s.constrain(Constraint::Horizontal(centerline));
            s.constrain(Constraint::Length(centerline, length));
            self.locate(ca, a, place);
        } else if dx.abs() < EPS {
            s.constrain(Constraint::Vertical(centerline));
            s.constrain(Constraint::Length(centerline, length));
            self.locate(ca, a, place);
        } else {
            self.locate(ca, a, place);
            self.locate(cb, b, place);
        }
        Ok(vec![
            ("centerline".to_owned(), centerline),
            ("start".to_owned(), ca),
            ("end".to_owned(), cb),
            ("arc_start".to_owned(), arc_a),
            ("arc_end".to_owned(), arc_b),
        ])
    }

    fn polygon(&mut self, item: &Value, place: Place) -> Result<Vec<(String, EntityId)>> {
        let center = self.point_arg(item, "center")?;
        let sides = args::count(item, "sides")?;
        if !(3..=64).contains(&sides) {
            return Err("a polygon has 3 to 64 sides".to_owned());
        }
        let radius = args::positive(item, "radius")?;
        let angle = args::opt_f64(item, "angle")?.unwrap_or(0.0).to_radians();
        let s = &mut *self.sketch;
        let cp = s.point(center);
        let circle = s.circle_around(cp, radius);
        s.set_construction(circle, true);
        s.constrain(Constraint::Diameter(circle, 2.0 * radius));
        let n = sides as usize;
        let corners: Vec<EntityId> = (0..n)
            .map(|k| {
                let t = angle + TAU * k as f64 / n as f64;
                s.point(Point2::new(
                    center.x + radius * t.cos(),
                    center.y + radius * t.sin(),
                ))
            })
            .collect();
        let mut lines = Vec::with_capacity(n);
        for (k, corner) in corners.iter().enumerate() {
            let next = corners.get((k + 1) % n).copied().unwrap_or(*corner);
            lines.push(s.line_between(*corner, next));
            s.constrain(Constraint::PointOnCircle(*corner, circle));
        }
        if let Some(first) = lines.first().copied() {
            for line in lines.iter().skip(1) {
                s.constrain(Constraint::EqualLength(first, *line));
            }
        }
        // Its turn: a side level or upright gets the relation, else the first corner is
        // at an angle from a horizontal construction line through the centre.
        let mut oriented = false;
        for (k, line) in lines.iter().enumerate() {
            let t0 = angle + TAU * k as f64 / n as f64;
            let t1 = angle + TAU * (k + 1) as f64 / n as f64;
            let (dx, dy) = (t1.cos() - t0.cos(), t1.sin() - t0.sin());
            if dy.abs() < 1e-9 {
                s.constrain(Constraint::Horizontal(*line));
                oriented = true;
                break;
            }
            if dx.abs() < 1e-9 {
                s.constrain(Constraint::Vertical(*line));
                oriented = true;
                break;
            }
        }
        if !oriented {
            let first = corners.first().copied().unwrap_or(cp);
            let level = s.point(Point2::new(center.x + radius, center.y));
            let horizontal = s.line_between(cp, level);
            s.set_construction(horizontal, true);
            s.set_construction(level, true);
            s.constrain(Constraint::Horizontal(horizontal));
            s.constrain(Constraint::Length(horizontal, radius));
            let spoke = s.line_between(cp, first);
            s.set_construction(spoke, true);
            s.constrain(Constraint::Angle(horizontal, spoke, wrap(angle)));
        }
        self.locate(cp, center, place);
        let mut names = vec![("center".to_owned(), cp), ("circle".to_owned(), circle)];
        names.extend(
            lines
                .iter()
                .enumerate()
                .map(|(k, l)| (format!("side{k}"), *l)),
        );
        Ok(names)
    }

    /// A line of text as the outlines of its letters, flattened into lines fixed in place.
    fn text(&mut self, item: &Value) -> Result<Vec<(String, EntityId)>> {
        let text = args::str(item, "text")?;
        let size = args::positive(item, "size")?;
        let font_name = args::opt_str(item, "font")?.unwrap_or("arial");
        let font = crate::font::Font::load(&crate::font::find(font_name)?)?;
        let tolerance = args::opt_f64(item, "tolerance")?.unwrap_or(0.01).max(1e-4);
        let spacing = args::opt_f64(item, "spacing")?.unwrap_or(0.0);
        let mut outlines = font.layout(text, size, spacing, tolerance)?;
        if outlines.is_empty() {
            return Err("the text has no outline".to_owned());
        }
        // A synthetic oblique: the letters leaned by `slant` degrees, as CAD programs make
        // italics of fonts that have none.
        let shear = args::opt_f64(item, "slant")?
            .unwrap_or(0.0)
            .to_radians()
            .tan();
        if shear != 0.0 {
            for p in outlines.iter_mut().flatten() {
                p.0 += p.1 * shear;
            }
        }
        // Alignment on the anchor: the box of the outlines, horizontally and vertically.
        let (mut x0, mut x1, mut y0, mut y1) = (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        );
        for (x, y) in outlines.iter().flatten() {
            x0 = x0.min(*x);
            x1 = x1.max(*x);
            y0 = y0.min(*y);
            y1 = y1.max(*y);
        }
        let dx = match args::opt_str(item, "align")?.unwrap_or("left") {
            "left" => 0.0,
            "center" => -(x0 + x1) / 2.0,
            "right" => -x1,
            other => return Err(format!("unknown alignment \"{other}\"")),
        };
        let dy = match args::opt_str(item, "valign")?.unwrap_or("baseline") {
            "baseline" => 0.0,
            "middle" => -(y0 + y1) / 2.0,
            "bottom" => -y0,
            "top" => -y1,
            other => return Err(format!("unknown vertical alignment \"{other}\"")),
        };
        let at = match args::opt(item, "at") {
            Some(v) => self.point(v, "\"at\"")?,
            None => Point2::ORIGIN,
        };
        let (sin, cos) = args::opt_f64(item, "angle")?
            .unwrap_or(0.0)
            .to_radians()
            .sin_cos();
        for outline in &mut outlines {
            for p in outline.iter_mut() {
                let (x, y) = (p.0 + dx, p.1 + dy);
                *p = (at.x + x * cos - y * sin, at.y + x * sin + y * cos);
            }
        }
        let mut names = Vec::new();
        for (k, outline) in outlines.iter().enumerate() {
            let points: Vec<EntityId> = outline
                .iter()
                .map(|(x, y)| {
                    let p = self.sketch.point(Point2::new(*x, *y));
                    self.sketch.constrain(Constraint::Fixed(p));
                    p
                })
                .collect();
            let n = points.len();
            for (i, p) in points.iter().enumerate() {
                let next = points.get((i + 1) % n).copied().unwrap_or(*p);
                let line = self.sketch.line_between(*p, next);
                if i == 0 {
                    names.push((format!("outline{k}"), line));
                }
            }
        }
        Ok(names)
    }

    fn arc(&mut self, item: &Value, place: Place) -> Result<Vec<(String, EntityId)>> {
        let c = self.point_arg(item, "center")?;
        let a = self.point_arg(item, "start")?;
        let b = self.point_arg(item, "end")?;
        let r = c.distance(a);
        if r <= EPS || (c.distance(b) - r).abs() > 1e-6 * r.max(1.0) {
            return Err(
                "an arc's start and end must be at the same distance from its centre".to_owned(),
            );
        }
        let s = &mut *self.sketch;
        let (cp, sp, ep) = (s.point(c), s.point(a), s.point(b));
        let arc = s.arc_between(cp, sp, ep);
        s.constrain(Constraint::Radius(arc, r));
        self.locate(cp, c, place);
        if place == Place::Dimensioned {
            self.place_on_arc(sp, cp, c, a);
            self.place_on_arc(ep, cp, c, b);
        } else if place == Place::Fixed {
            self.constrain(Constraint::Fixed(sp));
            self.constrain(Constraint::Fixed(ep));
        }
        Ok(vec![
            (String::new(), arc),
            ("center".to_owned(), cp),
            ("start".to_owned(), sp),
            ("end".to_owned(), ep),
        ])
    }

    /// Where an arc's end sits on its circle: level with or above the centre gets the
    /// relation, else the coordinate that moves fastest along the arc is dimensioned.
    fn place_on_arc(&mut self, p: EntityId, center: EntityId, c: Point2, at: Point2) {
        let (dx, dy) = (at.x - c.x, at.y - c.y);
        if dy.abs() < EPS {
            self.constrain(Constraint::HorizontalPoints(center, p));
        } else if dx.abs() < EPS {
            self.constrain(Constraint::VerticalPoints(center, p));
        } else if dy.abs() > dx.abs() {
            self.locate_x(p, at.x);
        } else {
            self.locate_y(p, at.y);
        }
    }

    fn polyline(&mut self, item: &Value, place: Place) -> Result<Vec<(String, EntityId)>> {
        let closed = args::bool(item, "closed", true)?;
        let fillet = args::opt_f64(item, "fillet")?.unwrap_or(0.0);
        let points = args::get(item, "points")?
            .as_array()
            .ok_or("\"points\" must be a list")?;
        let mut corners = Vec::with_capacity(points.len());
        for (k, p) in points.iter().enumerate() {
            let what = format!("point {}", k + 1);
            let corner = if p.is_array() {
                Corner {
                    at: self.point(p, &what)?,
                    fillet,
                    center: None,
                    cw: false,
                }
            } else {
                Corner {
                    at: self.point(args::get(p, "at")?, &what)?,
                    fillet: args::opt_f64(p, "fillet")?.unwrap_or(fillet),
                    center: args::opt(p, "center")
                        .map(|c| self.point(c, &what))
                        .transpose()?,
                    cw: args::bool(p, "cw", false)?,
                }
            };
            corners.push(corner);
        }
        if corners.len() < 2 {
            return Err("a polyline needs at least 2 points".to_owned());
        }
        self.chain(&corners, closed, place)
    }

    /// Lines (and arcs) through `corners`, dimensioned ordinate style, with fillets.
    fn chain(
        &mut self,
        corners: &[Corner],
        closed: bool,
        place: Place,
    ) -> Result<Vec<(String, EntityId)>> {
        let n = corners.len();
        let segments = if closed { n } else { n - 1 };
        let at = |k: usize| corners.get(k % n).map_or(Point2::ORIGIN, |c| c.at);
        // The arc ending at a corner that gives a centre, else a line.
        let arc_center = |k: usize| {
            corners
                .get((k + 1) % n)
                .and_then(|c| c.center.map(|p| (p, c.cw)))
        };
        for k in 0..segments {
            if at(k).distance(at(k + 1)) <= EPS {
                return Err(format!(
                    "points {} and {} are the same",
                    k + 1,
                    (k + 1) % n + 1
                ));
            }
        }
        if closed && n < 3 && (0..segments).all(|k| arc_center(k).is_none()) {
            return Err("a closed polyline of lines needs at least 3 points".to_owned());
        }
        // Fillets first, as geometry: where each rounded corner's lines stop.
        let mut rounds: Vec<Option<Round>> = Vec::with_capacity(n);
        for k in 0..n {
            let r = corners.get(k).map_or(0.0, |c| c.fillet);
            let interior = closed || (k > 0 && k + 1 < n);
            let between_lines = interior
                && arc_center((k + n - 1) % n).is_none()
                && (k >= segments || arc_center(k).is_none());
            rounds.push(if r > 0.0 && between_lines {
                Some(Round::at(at(k + n - 1), at(k), at(k + 1), r)?)
            } else {
                None
            });
        }
        // The vertex points, then the tangent points and centres of the fillets.
        let vertices: Vec<EntityId> = corners.iter().map(|c| self.sketch.point(c.at)).collect();
        let vertex = |k: usize| vertices.get(k % n).copied().unwrap_or(EntityId(0));
        let mut round_points: Vec<Option<(EntityId, EntityId, EntityId)>> = Vec::with_capacity(n);
        for round in &rounds {
            round_points.push(round.map(|r| {
                (
                    self.sketch.point(r.t_in),
                    self.sketch.point(r.t_out),
                    self.sketch.point(r.center),
                )
            }));
        }
        let start_of = |k: usize| match round_points.get(k % n) {
            Some(Some((_, t_out, _))) => *t_out,
            _ => vertex(k),
        };
        let end_of = |k: usize| match round_points.get((k + 1) % n) {
            Some(Some((t_in, _, _))) => *t_in,
            _ => vertex(k + 1),
        };
        let mut curves: Vec<EntityId> = Vec::with_capacity(segments);
        // Arc centres, with which coordinate places them: the one along the line through
        // the arc's ends' bisector (`true` for y), which the ends leave free.
        let mut centers: Vec<(EntityId, Point2, bool, EntityId, f64)> = Vec::new();
        let mut level: Vec<(usize, usize, bool)> = Vec::new();
        for k in 0..segments {
            let (a, b) = (start_of(k), end_of(k));
            if let Some((c, cw)) = arc_center(k) {
                let r = c.distance(at(k));
                if (c.distance(at(k + 1)) - r).abs() > 1e-6 * r.max(1.0) {
                    return Err(format!(
                        "point {}: the arc's ends are not at the same distance from its centre",
                        (k + 1) % n + 1
                    ));
                }
                let cp = self.sketch.point(c);
                let arc = if cw {
                    self.sketch.arc_between(cp, b, a)
                } else {
                    self.sketch.arc_between(cp, a, b)
                };
                let (pa, pb) = (at(k), at(k + 1));
                let along_y = (pb.x - pa.x).abs() >= (pb.y - pa.y).abs();
                centers.push((cp, c, along_y, arc, r));
                curves.push(arc);
            } else {
                let line = self.sketch.line_between(a, b);
                let (pa, pb) = (at(k), at(k + 1));
                if (pb.y - pa.y).abs() < EPS {
                    self.constrain(Constraint::Horizontal(line));
                    level.push((k, (k + 1) % n, true));
                } else if (pb.x - pa.x).abs() < EPS {
                    self.constrain(Constraint::Vertical(line));
                    level.push((k, (k + 1) % n, false));
                }
                curves.push(line);
            }
        }
        // Ordinate dimensions: one per x shared through vertical lines, one per y shared
        // through horizontal lines.
        match place {
            Place::Dimensioned => {
                let mut x_class: Vec<usize> = (0..n).collect();
                let mut y_class: Vec<usize> = (0..n).collect();
                for (a, b, horizontal) in &level {
                    let classes = if *horizontal {
                        &mut y_class
                    } else {
                        &mut x_class
                    };
                    let (ra, rb) = (find(classes, *a), find(classes, *b));
                    if let Some(slot) = classes.get_mut(ra.max(rb)) {
                        *slot = ra.min(rb);
                    }
                }
                for k in 0..n {
                    if find(&mut x_class, k) == k {
                        self.locate_x(vertex(k), at(k).x);
                    }
                    if find(&mut y_class, k) == k {
                        self.locate_y(vertex(k), at(k).y);
                    }
                }
                for (cp, c, along_y, ..) in &centers {
                    if *along_y {
                        self.locate_y(*cp, c.y);
                    } else {
                        self.locate_x(*cp, c.x);
                    }
                }
            }
            Place::Fixed => {
                for k in 0..n {
                    self.constrain(Constraint::Fixed(vertex(k)));
                }
                for (cp, ..) in &centers {
                    self.constrain(Constraint::Fixed(*cp));
                }
            }
            Place::Free => {
                for (.., arc, r) in &centers {
                    self.constrain(Constraint::Radius(*arc, *r));
                }
            }
        }
        // The fillets: the corner stays as a virtual sharp on both lines, which keeps its
        // dimensions, and an arc tangent to both joins the tangent points.
        let mut first_of_radius: Vec<(f64, EntityId)> = Vec::new();
        let mut names: Vec<(String, EntityId)> = Vec::new();
        for k in 0..n {
            let (Some(Some(round)), Some(Some((t_in, t_out, center)))) =
                (rounds.get(k), round_points.get(k))
            else {
                continue;
            };
            let previous = (k + segments - 1) % segments;
            let (Some(before), Some(after)) = (curves.get(previous), curves.get(k % segments))
            else {
                continue;
            };
            let sharp = vertex(k);
            self.sketch.set_construction(sharp, true);
            self.constrain(Constraint::PointOnLine(sharp, *before));
            self.constrain(Constraint::PointOnLine(sharp, *after));
            let arc = if round.left_turn {
                self.sketch.arc_between(*center, *t_in, *t_out)
            } else {
                self.sketch.arc_between(*center, *t_out, *t_in)
            };
            self.constrain(Constraint::Tangent(*before, arc));
            self.constrain(Constraint::Tangent(*after, arc));
            match first_of_radius
                .iter()
                .find(|(radius, _)| (radius - round.radius).abs() < 1e-9)
            {
                Some((_, first)) => self.constrain(Constraint::EqualRadius(*first, arc)),
                None => {
                    self.constrain(Constraint::Radius(arc, round.radius));
                    first_of_radius.push((round.radius, arc));
                }
            }
            names.push((format!("fillet{k}"), arc));
        }
        names.extend(
            curves
                .iter()
                .enumerate()
                .map(|(k, id)| (format!("{k}"), *id)),
        );
        names.extend(
            vertices
                .iter()
                .enumerate()
                .map(|(k, id)| (format!("p{k}"), *id)),
        );
        Ok(names)
    }
}

/// A rounded corner, as geometry: where the lines stop and the arc's centre.
#[derive(Debug, Clone, Copy)]
struct Round {
    t_in: Point2,
    t_out: Point2,
    center: Point2,
    radius: f64,
    /// The path turns left at the corner, so the arc runs counter-clockwise from `t_in`.
    left_turn: bool,
}

impl Round {
    /// The fillet of radius `r` at corner `p` between the line from `a` and the line to
    /// `b`.
    fn at(a: Point2, p: Point2, b: Point2, r: f64) -> Result<Self> {
        let unit = |to: Point2| {
            let (dx, dy) = (to.x - p.x, to.y - p.y);
            let l = dx.hypot(dy);
            (dx / l, dy / l, l)
        };
        let (ux, uy, la) = unit(a);
        let (vx, vy, lb) = unit(b);
        let half = (ux * vx + uy * vy).clamp(-1.0, 1.0).acos() / 2.0;
        if half < 1e-6 || (PI / 2.0 - half) < 1e-6 {
            return Err(format!(
                "the corner at ({}, {}) is not between two lines at an angle",
                p.x, p.y
            ));
        }
        let back = r / half.tan();
        if back >= la - EPS || back >= lb - EPS {
            return Err(format!(
                "a fillet of {r} does not fit the corner at ({}, {})",
                p.x, p.y
            ));
        }
        let (bx, by) = (ux + vx, uy + vy);
        let bl = bx.hypot(by);
        let d = r / half.sin();
        Ok(Self {
            t_in: Point2::new(p.x + ux * back, p.y + uy * back),
            t_out: Point2::new(p.x + vx * back, p.y + vy * back),
            center: Point2::new(p.x + bx / bl * d, p.y + by / bl * d),
            radius: r,
            left_turn: (p.x - a.x) * (b.y - p.y) - (p.y - a.y) * (b.x - p.x) > 0.0,
        })
    }
}

// How an item is placed in the sketch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// Dimensioned from the origin (the default).
    Dimensioned,
    /// Its reference point fixed.
    Fixed,
    /// Left free to move.
    Free,
}

/// A point of a polyline.
#[derive(Debug, Clone, Copy)]
struct Corner {
    at: Point2,
    /// Radius of a fillet at this corner; 0 for none.
    fillet: f64,
    /// Centre of an arc from the previous point to this one.
    center: Option<Point2>,
    /// That arc runs clockwise.
    cw: bool,
}

impl Corner {
    fn sharp(at: Point2) -> Self {
        Self {
            at,
            fillet: 0.0,
            center: None,
            cw: false,
        }
    }
}

/// Union-find root.
fn find(classes: &mut [usize], k: usize) -> usize {
    let mut root = k;
    while let Some(&parent) = classes.get(root) {
        if parent == root {
            break;
        }
        root = parent;
    }
    if let Some(slot) = classes.get_mut(k) {
        *slot = root;
    }
    root
}

/// An angle in (−π, π].
fn wrap(angle: f64) -> f64 {
    -((PI - angle).rem_euclid(TAU) - PI)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn built(items: Value) -> (ConstrainedSketch, BTreeMap<String, u32>) {
        let mut sketch = ConstrainedSketch::new();
        let labels = {
            let mut b = Builder::new(&mut sketch, Plane::FRONT);
            b.add_all(items.as_array().unwrap()).unwrap();
            b.labels.clone()
        };
        (sketch, labels)
    }

    fn dof(sketch: &ConstrainedSketch) -> usize {
        let mut s = sketch.clone();
        let report = s.solve().unwrap();
        assert!(report.converged);
        report.degrees_of_freedom
    }

    #[test]
    fn shapes_come_out_fully_defined() {
        for item in [
            json!({"type": "rect", "center": [0, 0], "size": [100, 50]}),
            json!({"type": "rect", "center": [10, -5], "size": [20, 30]}),
            json!({"type": "rect", "corner": [0, 0], "size": [20, 30]}),
            json!({"type": "rect", "center": [0, 0], "size": [100, 50], "radius": 5}),
            json!({"type": "rect", "corner": [3, 4], "size": [100, 50], "radius": 5}),
            json!({"type": "circle", "center": [12, 7], "diameter": 4}),
            json!({"type": "circle", "center": [0, 0], "radius": 4}),
            json!({"type": "slot", "start": [0, 0], "end": [40, 0], "width": 6}),
            json!({"type": "slot", "start": [5, 5], "end": [5, 40], "width": 6}),
            json!({"type": "slot", "start": [5, 5], "end": [25, 40], "width": 6}),
            json!({"type": "polygon", "center": [0, 0], "sides": 6, "radius": 10}),
            json!({"type": "polygon", "center": [3, 2], "sides": 6, "radius": 10, "angle": 30}),
            json!({"type": "polygon", "center": [3, 2], "sides": 5, "radius": 10, "angle": 10}),
            json!({"type": "polyline", "points": [[0, 0], [50, 0], [50, 10], [30, 10], [30, 30], [0, 30]]}),
            json!({"type": "polyline", "points": [[0, 0], [50, 0], [40, 30]]}),
            json!({"type": "polyline", "points": [[0, 0], [50, 0], [50, 10], [0, 30]], "fillet": 2}),
            json!({"type": "polyline", "points": [{"at": [-10, 0], "center": [0, 0]}, [10, 0]]}),
            json!({"type": "line", "start": [0, -20], "end": [0, 20], "construction": true}),
            json!({"type": "arc", "center": [0, 0], "start": [10, 0], "end": [0, 10]}),
            json!({"type": "point", "at": [7, 3]}),
        ] {
            let (sketch, _) = built(json!([item.clone()]));
            assert_eq!(dof(&sketch), 0, "{item}");
        }
    }

    #[test]
    fn closed_shapes_make_profiles() {
        let (sketch, labels) = built(json!([
            {"type": "rect", "id": "outer", "center": [0, 0], "size": [100, 60], "radius": 6},
            {"type": "circle", "center": [30, 10], "diameter": 8},
            {"type": "slot", "start": [-30, -10], "end": [-10, -10], "width": 6},
            {"type": "polygon", "center": [0, 15], "sides": 6, "radius": 5},
            {"type": "polyline", "points": [[10, -25], [40, -25], [40, -5]], "fillet": 3},
        ]));
        let profile = oxyde_geom::Profile::from_curves(&sketch.profile_segments()).unwrap();
        assert_eq!(profile.regions.len(), 1);
        assert_eq!(profile.regions[0].holes.len(), 4);
        assert!(labels.contains_key("outer.top"));
        assert_eq!(dof(&sketch), 0);
    }

    #[test]
    fn a_dimension_moves_what_it_should() {
        let (mut sketch, _) = built(json!([
            {"type": "rect", "center": [0, 0], "size": [100, 50]},
        ]));
        let width = sketch
            .dimensions()
            .find(|d| d.constraint.value() == Some(100.0))
            .unwrap()
            .id;
        sketch.set_dimension(width, 120.0);
        sketch.solve().unwrap();
        let xs: Vec<f64> = sketch
            .entities()
            .iter()
            .filter_map(|e| match e.geometry {
                oxyde_sketch::Geometry::Point(p) if !e.construction => Some(p.x),
                _ => None,
            })
            .collect();
        // Still centred: the corners went out 10 on each side.
        assert!(xs.iter().all(|x| (x.abs() - 60.0).abs() < 1e-6), "{xs:?}");
    }
}
