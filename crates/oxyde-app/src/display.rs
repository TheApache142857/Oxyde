//! What is shown in the viewport besides the part: planes, axes, sketches and the origin.
//!
//! Two levels, as in SolidWorks: each item can be shown or hidden (eye in the timeline,
//! right-click › Show/Hide), and View › Hide/Show turns whole kinds off. By default the
//! standard planes are hidden, reference geometry and the origin are shown, and a sketch is
//! shown until a feature uses it ("absorbs" it).

use std::collections::HashMap;

use oxyde_geom::Point2;
use oxyde_model::{FeatureId, FeatureKind, StandardPlane};
use oxyde_sketch::{ConstrainedSketch, Geometry};

use crate::app::OxydeApp;

/// Something that can be shown or hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
    Plane(StandardPlane),
    Origin,
    Feature(FeatureId),
}

/// View › Hide/Show: whole kinds of items.
#[derive(Debug, Clone)]
pub struct Visibility {
    pub planes: bool,
    pub axes: bool,
    pub sketches: bool,
    pub origins: bool,
    /// Items shown or hidden by the user, overriding the default.
    items: HashMap<Item, bool>,
}

impl Default for Visibility {
    fn default() -> Self {
        Self {
            planes: true,
            axes: true,
            sketches: true,
            origins: true,
            items: HashMap::new(),
        }
    }
}

impl OxydeApp {
    /// Sketches used by a feature, with the first feature using each: it absorbs the sketch
    /// in the timeline and hides it by default.
    pub(crate) fn absorbed_sketches(&self) -> HashMap<FeatureId, FeatureId> {
        let mut out = HashMap::new();
        for feature in self.doc.part.features() {
            let sketch = match feature.kind() {
                FeatureKind::Extrude(e) => Some(e.sketch),
                FeatureKind::Revolve(r) => Some(r.sketch),
                _ => None,
            };
            if let Some(sketch) = sketch {
                out.entry(sketch).or_insert(feature.id());
            }
        }
        out
    }

    fn default_visible(&self, item: Item, absorbed: &HashMap<FeatureId, FeatureId>) -> bool {
        match item {
            Item::Plane(_) => false,
            Item::Origin => true,
            Item::Feature(id) => !absorbed.contains_key(&id),
        }
    }

    /// Whether the user wants this item shown (before the View › Hide/Show filters).
    pub(crate) fn item_shown(&self, item: Item) -> bool {
        match self.visibility.items.get(&item) {
            Some(shown) => *shown,
            None => self.default_visible(item, &self.absorbed_sketches()),
        }
    }

    pub(crate) fn toggle_item(&mut self, item: Item) {
        let shown = self.item_shown(item);
        self.visibility.items.insert(item, !shown);
    }
}

impl Visibility {
    pub(crate) fn set_item_shown(&mut self, item: Item, shown: bool) {
        self.items.insert(item, shown);
    }
}

impl OxydeApp {

    /// Whether an item is drawn: shown, and its kind not filtered out.
    pub(crate) fn item_visible(&self, item: Item) -> bool {
        let kind_on = match item {
            Item::Plane(_) => self.visibility.planes,
            Item::Origin => self.visibility.origins,
            Item::Feature(id) => match self.doc.part.feature(id).map(|f| f.kind()) {
                Some(FeatureKind::Sketch(_)) => self.visibility.sketches,
                Some(FeatureKind::Plane(_)) => self.visibility.planes,
                Some(FeatureKind::Axis(_)) => self.visibility.axes,
                _ => true,
            },
        };
        kind_on && self.item_shown(item)
    }

    /// Whether the timeline shows an eye for this feature.
    pub(crate) fn has_visibility(&self, id: FeatureId) -> bool {
        matches!(
            self.doc.part.feature(id).map(|f| f.kind()),
            Some(FeatureKind::Sketch(_) | FeatureKind::Plane(_) | FeatureKind::Axis(_))
        )
    }
}

/// The curves of a sketch as polylines in sketch coordinates, with their construction flag.
/// The origin point is left out.
pub fn sketch_polylines(sketch: &ConstrainedSketch) -> Vec<(Vec<Point2>, bool)> {
    let at = |id| sketch.position(id).unwrap_or_default();
    let arc = |c: Point2, r: f64, a0: f64, sweep: f64, n: usize| -> Vec<Point2> {
        (0..=n)
            .map(|k| {
                let a = a0 + sweep * k as f64 / n as f64;
                Point2::new(c.x + r * a.cos(), c.y + r * a.sin())
            })
            .collect()
    };
    sketch
        .entities()
        .iter()
        .filter_map(|e| {
            let points = match e.geometry {
                Geometry::Point(_) => return None,
                Geometry::Line { start, end } => vec![at(start), at(end)],
                Geometry::Circle { center, radius } => {
                    arc(at(center), radius, 0.0, std::f64::consts::TAU, 64)
                }
                Geometry::Arc { center, start, end } => {
                    let (c, s, f) = (at(center), at(start), at(end));
                    let a0 = (s.y - c.y).atan2(s.x - c.x);
                    let a1 = (f.y - c.y).atan2(f.x - c.x);
                    let sweep = (a1 - a0).rem_euclid(std::f64::consts::TAU);
                    arc(c, c.distance(s), a0, sweep, 40)
                }
            };
            Some((points, e.construction))
        })
        .collect()
}
