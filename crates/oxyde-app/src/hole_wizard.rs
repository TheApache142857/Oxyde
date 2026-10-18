//! The Hole Wizard: holes at the lone points of a sketch (`oxyde_model::Hole`), sized from
//! ISO metric tables as SolidWorks' Hole Wizard sizes them: clearance holes in three fits
//! (ISO 273), tap drills for tapped holes, counterbores for socket head cap screws
//! (ISO 4762, DIN 974-1) and 90° countersinks for flat head screws (ISO 10642). Every
//! number stays editable: a size fills them in, *Custom* keeps what is typed.

use eframe::egui::{self, RichText};
use oxyde_model::{BodyId, FeatureId, FeatureKind, FeatureParameter, Hole, HoleKind};

use crate::app::OxydeApp;
use crate::dialogs::{Dialog, positive_length};
use crate::property_manager::{section, selection_box};

/// One screw size: its name, the clearance hole for a close, normal and loose fit, the
/// tap drill, the counterbore's diameter and depth for its socket head, and the
/// countersink's diameter for its flat head (mm).
struct Size {
    name: &'static str,
    clearance: [f64; 3],
    tap_drill: f64,
    counterbore: (f64, f64),
    countersink: f64,
}

const fn size(
    name: &'static str,
    clearance: [f64; 3],
    tap_drill: f64,
    counterbore: (f64, f64),
    countersink: f64,
) -> Size {
    Size {
        name,
        clearance,
        tap_drill,
        counterbore,
        countersink,
    }
}

/// ISO metric coarse threads.
const SIZES: [Size; 13] = [
    size("M2", [2.2, 2.4, 2.6], 1.6, (4.4, 2.2), 3.8),
    size("M2.5", [2.7, 2.9, 3.1], 2.05, (5.5, 2.7), 4.7),
    size("M3", [3.2, 3.4, 3.6], 2.5, (6.5, 3.3), 6.72),
    size("M4", [4.3, 4.5, 4.8], 3.3, (8.0, 4.4), 8.96),
    size("M5", [5.3, 5.5, 5.8], 4.2, (10.0, 5.4), 11.2),
    size("M6", [6.4, 6.6, 7.0], 5.0, (11.0, 6.4), 13.44),
    size("M8", [8.4, 9.0, 10.0], 6.8, (15.0, 8.6), 17.92),
    size("M10", [10.5, 11.0, 12.0], 8.5, (18.0, 10.6), 22.4),
    size("M12", [13.0, 13.5, 14.5], 10.2, (20.0, 12.6), 26.88),
    size("M14", [15.0, 15.5, 16.5], 12.0, (24.0, 14.6), 30.8),
    size("M16", [17.0, 17.5, 18.5], 14.0, (26.0, 16.6), 33.6),
    size("M20", [21.0, 22.0, 24.0], 17.5, (33.0, 20.6), 40.32),
    size("M24", [25.0, 26.0, 28.0], 21.0, (40.0, 24.8), 48.0),
];

/// What the hole is for: SolidWorks' Hole Wizard types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoleType {
    /// A plain hole a screw goes through.
    Clearance,
    /// A hole drilled to be tapped: the tap drill's diameter (the thread is not modelled).
    Tapped,
    /// A clearance hole with a counterbore for a socket head.
    Counterbore,
    /// A clearance hole with a 90° countersink for a flat head.
    Countersink,
}

impl HoleType {
    const ALL: [(HoleType, &'static str); 4] = [
        (HoleType::Clearance, "Clearance"),
        (HoleType::Tapped, "Tapped (tap drill)"),
        (HoleType::Counterbore, "Counterbore"),
        (HoleType::Countersink, "Countersink"),
    ];
}

/// How much room a clearance hole leaves around its screw (ISO 273).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Close,
    Normal,
    Loose,
}

impl Fit {
    const ALL: [(Fit, &'static str); 3] = [
        (Fit::Close, "Close"),
        (Fit::Normal, "Normal"),
        (Fit::Loose, "Loose"),
    ];
}

#[derive(Clone)]
pub struct HoleDialog {
    pub editing: Option<FeatureId>,
    /// Full names of its dimensions, when editing.
    pub dimensions: Vec<String>,
    /// The sketch whose lone points place the holes.
    pub sketch: Option<FeatureId>,
    pub hole_type: HoleType,
    /// The screw size, in `SIZES`; `None` for custom values.
    pub size: Option<usize>,
    pub fit: Fit,
    pub diameter: f64,
    pub counter_diameter: f64,
    pub counter_depth: f64,
    /// Degrees.
    pub counter_angle: f64,
    pub through_all: bool,
    pub depth: f64,
    pub reverse: bool,
    pub bodies: Vec<BodyId>,
}

impl HoleDialog {
    /// A new hole of `sketch`: an M6 clearance hole, normal fit, through everything.
    fn new(sketch: Option<FeatureId>) -> Self {
        let mut dialog = Self {
            editing: None,
            dimensions: Vec::new(),
            sketch,
            hole_type: HoleType::Clearance,
            size: SIZES.iter().position(|s| s.name == "M6"),
            fit: Fit::Normal,
            diameter: 6.6,
            counter_diameter: 11.0,
            counter_depth: 6.4,
            counter_angle: 90.0,
            through_all: true,
            depth: 10.0,
            reverse: false,
            bodies: Vec::new(),
        };
        dialog.apply_size();
        dialog
    }

    /// The dialog for an existing hole, its size found again in the table when its values
    /// are a size's.
    fn of(id: FeatureId, hole: &Hole, dimensions: Vec<String>) -> Self {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
        let (hole_type, counter_diameter, counter_depth, counter_angle) = match hole.kind {
            HoleKind::Simple => {
                let tapped = SIZES.iter().any(|s| near(s.tap_drill, hole.diameter));
                let kind = if tapped {
                    HoleType::Tapped
                } else {
                    HoleType::Clearance
                };
                (kind, hole.diameter * 1.6, hole.diameter, 90.0)
            }
            HoleKind::Counterbore { diameter, depth } => {
                (HoleType::Counterbore, diameter, depth, 90.0)
            }
            HoleKind::Countersink { diameter, angle } => (
                HoleType::Countersink,
                diameter,
                hole.diameter,
                angle.to_degrees(),
            ),
        };
        let mut dialog = Self {
            editing: Some(id),
            dimensions,
            sketch: Some(hole.sketch),
            hole_type,
            size: None,
            fit: Fit::Normal,
            diameter: hole.diameter,
            counter_diameter,
            counter_depth,
            counter_angle,
            through_all: hole.depth.is_none(),
            depth: hole.depth.unwrap_or(10.0),
            reverse: hole.reverse,
            bodies: hole.bodies.clone(),
        };
        // The size and fit these values are, if they are one's.
        'search: for i in 0..SIZES.len() {
            for (fit, _) in Fit::ALL {
                let mut trial = Self {
                    size: Some(i),
                    fit,
                    ..dialog.clone()
                };
                trial.apply_size();
                if trial.feature() == dialog.feature() {
                    dialog.size = Some(i);
                    dialog.fit = fit;
                    break 'search;
                }
            }
        }
        dialog
    }

    /// Fills the diameters and depths in from the size chosen.
    fn apply_size(&mut self) {
        let Some(s) = self.size.and_then(|i| SIZES.get(i)) else {
            return;
        };
        let [close, normal, loose] = s.clearance;
        let clearance = match self.fit {
            Fit::Close => close,
            Fit::Normal => normal,
            Fit::Loose => loose,
        };
        match self.hole_type {
            HoleType::Clearance => self.diameter = clearance,
            HoleType::Tapped => self.diameter = s.tap_drill,
            HoleType::Counterbore => {
                self.diameter = clearance;
                (self.counter_diameter, self.counter_depth) = s.counterbore;
            }
            HoleType::Countersink => {
                self.diameter = clearance;
                self.counter_diameter = s.countersink;
                self.counter_angle = 90.0;
            }
        }
    }

    /// The hole as it stands, once it has a sketch.
    pub fn feature(&self) -> Option<FeatureKind> {
        let kind = match self.hole_type {
            HoleType::Clearance | HoleType::Tapped => HoleKind::Simple,
            HoleType::Counterbore => HoleKind::Counterbore {
                diameter: self.counter_diameter,
                depth: self.counter_depth,
            },
            HoleType::Countersink => HoleKind::Countersink {
                diameter: self.counter_diameter,
                angle: self.counter_angle.to_radians(),
            },
        };
        Some(FeatureKind::Hole(Hole {
            sketch: self.sketch?,
            kind,
            diameter: self.diameter,
            depth: (!self.through_all).then_some(self.depth),
            reverse: self.reverse,
            bodies: self.bodies.clone(),
        }))
    }

    /// What it is, for the panel: "M6 Counterbore, normal fit".
    fn title(&self) -> String {
        let size = self
            .size
            .and_then(|i| SIZES.get(i))
            .map_or("Custom", |s| s.name);
        let kind = HoleType::ALL
            .iter()
            .find(|(t, _)| *t == self.hole_type)
            .map_or("", |(_, label)| label);
        format!("{size} {kind}")
    }
}

impl OxydeApp {
    /// Hole Wizard: holes at the lone points of the selected sketch; with a flat face
    /// selected instead, a new sketch on it to click the positions in, the Hole Wizard
    /// opening when it is left (as SolidWorks' Positions tab); else the latest sketch
    /// nothing uses yet.
    pub(crate) fn begin_hole(&mut self) {
        let selected_sketch = self.selected_feature.filter(|id| {
            matches!(
                self.doc.part.feature(*id).map(|f| f.kind()),
                Some(FeatureKind::Sketch(_))
            )
        });
        if selected_sketch.is_none()
            && let Some(face) = self.selected_planar_face()
        {
            self.pending_tool = Some(crate::sketch_mode::Tool::Point);
            self.start_new_sketch(oxyde_model::SketchPlane::Face(face));
            if self.sketch.is_some() {
                self.hole_positions = true;
                self.status =
                    "Click where the holes go, then Exit Sketch: the Hole Wizard opens".to_owned();
            }
            return;
        }
        let sketch = self.sketch_to_use();
        self.open_dialog(Dialog::Hole(HoleDialog::new(sketch)));
        self.status = if sketch.is_none() {
            "Select the sketch of the hole positions in the tree: points on a face (Sketch › Point)"
                .to_owned()
        } else {
            "Holes go at the sketch's lone points; pick the type and the size".to_owned()
        };
    }

    /// The Hole Wizard on an existing hole.
    pub(crate) fn open_hole(&mut self, id: FeatureId, hole: &Hole) {
        let dimensions = [
            FeatureParameter::Diameter,
            FeatureParameter::CounterDiameter,
            FeatureParameter::CounterDepth,
            FeatureParameter::Angle,
            FeatureParameter::Depth,
        ]
        .into_iter()
        .filter_map(|p| self.parameter_full_name(id, p))
        .collect();
        self.open_dialog(Dialog::Hole(HoleDialog::of(id, hole, dimensions)));
    }

    /// The Hole Wizard's options in the PropertyManager.
    pub(crate) fn hole_body(&self, ui: &mut egui::Ui, d: &mut HoleDialog) {
        section(ui, "Positions");
        let items: Vec<(String, String)> = d
            .sketch
            .and_then(|id| self.doc.part.feature(id))
            .map(|f| (f.name().to_owned(), String::new()))
            .into_iter()
            .collect();
        if selection_box(ui, &items, "Select a sketch of points in the tree").is_some() {
            d.sketch = None;
        }
        ui.label(
            RichText::new(
                "A hole at each lone point of the sketch (Sketch › Point), into the face it is on.",
            )
            .small()
            .weak(),
        );
        section(ui, "Hole type");
        let mut refill = false;
        egui::Grid::new("hole_type").num_columns(2).show(ui, |ui| {
            for (i, (t, label)) in HoleType::ALL.into_iter().enumerate() {
                if ui.radio_value(&mut d.hole_type, t, label).changed() {
                    refill = true;
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        section(ui, "Size (ISO metric)");
        egui::Grid::new("hole_size").num_columns(2).show(ui, |ui| {
            ui.label("Size");
            let current = d
                .size
                .and_then(|i| SIZES.get(i))
                .map_or("Custom", |s| s.name);
            egui::ComboBox::from_id_salt("hole_size_list")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (i, s) in SIZES.iter().enumerate() {
                        if ui.selectable_label(d.size == Some(i), s.name).clicked() {
                            d.size = Some(i);
                            refill = true;
                        }
                    }
                    if ui.selectable_label(d.size.is_none(), "Custom").clicked() {
                        d.size = None;
                    }
                });
            ui.end_row();
            if d.hole_type != HoleType::Tapped {
                ui.label("Fit");
                ui.horizontal(|ui| {
                    for (fit, label) in Fit::ALL {
                        if ui.radio_value(&mut d.fit, fit, label).changed() {
                            refill = true;
                        }
                    }
                });
                ui.end_row();
            }
        });
        if refill {
            d.apply_size();
        }
        section(ui, "Dimensions");
        // Typing a value makes the size a custom one.
        let before = (
            d.diameter,
            d.counter_diameter,
            d.counter_depth,
            d.counter_angle,
        );
        egui::Grid::new("hole_dimensions")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Hole Ø");
                positive_length(ui, &mut d.diameter);
                ui.end_row();
                match d.hole_type {
                    HoleType::Counterbore => {
                        ui.label("Counterbore Ø");
                        positive_length(ui, &mut d.counter_diameter);
                        ui.end_row();
                        ui.label("Counterbore depth");
                        positive_length(ui, &mut d.counter_depth);
                        ui.end_row();
                    }
                    HoleType::Countersink => {
                        ui.label("Countersink Ø");
                        positive_length(ui, &mut d.counter_diameter);
                        ui.end_row();
                        ui.label("Angle");
                        ui.add(
                            egui::DragValue::new(&mut d.counter_angle)
                                .speed(1.0)
                                .range(1.0..=179.0)
                                .suffix("°")
                                .custom_parser(crate::dialogs::angle_parser),
                        );
                        ui.end_row();
                    }
                    HoleType::Clearance | HoleType::Tapped => {}
                }
            });
        let after = (
            d.diameter,
            d.counter_diameter,
            d.counter_depth,
            d.counter_angle,
        );
        if after != before {
            d.size = None;
        }
        section(ui, "End condition");
        ui.horizontal(|ui| {
            ui.radio_value(&mut d.through_all, true, "Through All");
            ui.radio_value(&mut d.through_all, false, "Blind");
        });
        if !d.through_all {
            ui.horizontal(|ui| {
                ui.label("Depth");
                positive_length(ui, &mut d.depth);
            });
            ui.label(
                RichText::new("The full diameter's depth; a 118° drill point is under it.")
                    .small()
                    .weak(),
            );
        }
        ui.checkbox(&mut d.reverse, "Reverse direction");
        ui.label(RichText::new(d.title()).small().weak());
        if !d.dimensions.is_empty() {
            ui.label(RichText::new(d.dimensions.join(", ")).small().weak());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_fill_the_hole_in_and_are_found_again() {
        let mut d = HoleDialog::new(Some(FeatureId::from_raw(3)));
        assert_eq!(d.diameter, 6.6, "an M6 clearance hole, normal fit");
        d.hole_type = HoleType::Counterbore;
        d.apply_size();
        assert_eq!((d.counter_diameter, d.counter_depth), (11.0, 6.4));
        d.hole_type = HoleType::Tapped;
        d.apply_size();
        assert_eq!(d.diameter, 5.0);

        // An M8 countersink, close fit, as a feature and back.
        d.size = SIZES.iter().position(|s| s.name == "M8");
        d.hole_type = HoleType::Countersink;
        d.fit = Fit::Close;
        d.apply_size();
        let Some(FeatureKind::Hole(hole)) = d.feature() else {
            panic!("a hole")
        };
        assert_eq!(hole.diameter, 8.4);
        let again = HoleDialog::of(FeatureId::from_raw(4), &hole, Vec::new());
        assert_eq!(again.size, d.size);
        assert_eq!(again.fit, Fit::Close);
        assert_eq!(again.hole_type, HoleType::Countersink);
        assert_eq!(again.title(), "M8 Countersink");

        // Values no size has: custom.
        let odd = Hole {
            diameter: 7.77,
            ..hole
        };
        assert_eq!(
            HoleDialog::of(FeatureId::from_raw(4), &odd, Vec::new()).size,
            None
        );
    }
}
