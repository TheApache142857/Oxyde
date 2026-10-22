//! Small demo parts to try the application quickly: a bracket (extrusions, fillet,
//! chamfer) and a flange (revolve, circular pattern, equations).

use oxyde_geom::Point2;
use oxyde_model::{
    AxisRef, CircularPattern, EdgeBlend, EdgeName, Extrude, ExtrudeOp, FaceName, FeatureKind, Part,
    Revolve, RevolveAxis, Sketch, SketchPlane, StandardAxis, StandardPlane,
};

/// Id of the right line of a rectangle drawn first in a sketch (corner points are 1–4, then
/// the bottom, right, top and left lines are 5–8).
const RIGHT: u32 = 6;

pub fn bracket() -> Part {
    let mut part = Part::new();

    // Base plate, 80 × 50 × 10, on the Front plane.
    let mut plate = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    plate.rectangle(Point2::new(-40.0, -25.0), 80.0, 50.0);
    let plate_sketch = part.add_feature(FeatureKind::Sketch(plate));
    let base = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        plate_sketch,
        ExtrudeOp::Boss,
        10.0,
    )));

    // Upright at the left end, sketched on the top of the plate.
    let mut upright = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    upright.rectangle(Point2::new(-40.0, -25.0), 15.0, 50.0);
    let upright_sketch = part.add_feature(FeatureKind::Sketch(upright));
    let wall = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        upright_sketch,
        ExtrudeOp::Boss,
        30.0,
    )));

    // Two mounting holes through the plate.
    let mut holes = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
    holes.circle(Point2::new(15.0, 12.0), 4.0);
    holes.circle(Point2::new(15.0, -12.0), 4.0);
    let holes_sketch = part.add_feature(FeatureKind::Sketch(holes));
    part.add_feature(FeatureKind::Extrude(Extrude::blind(
        holes_sketch,
        ExtrudeOp::Cut,
        12.0,
    )));

    // Round the inside corner between the upright and the plate.
    part.add_feature(FeatureKind::Fillet(EdgeBlend {
        edges: vec![EdgeName::between(
            FaceName::profile(wall, RIGHT),
            FaceName::end_cap(base),
        )],
        size: 4.0,
        chamfer: Default::default(),
    }));

    // Break the top inside edge of the upright.
    part.add_feature(FeatureKind::Chamfer(EdgeBlend {
        edges: vec![EdgeName::between(
            FaceName::profile(wall, RIGHT),
            FaceName::end_cap(wall),
        )],
        size: 2.0,
        chamfer: Default::default(),
    }));

    part
}

/// A flange: a hub and a disc revolved about a centerline, six bolt holes in a circular
/// pattern whose count is a global variable.
pub fn flange() -> Part {
    let mut part = Part::new();

    // Half cross-section on the Front plane, revolved about the Y axis.
    let mut section = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    let axis = section
        .geometry
        .line(Point2::new(0.0, -5.0), Point2::new(0.0, 30.0));
    section.geometry.set_construction(axis, true);
    let corners = [
        (10.0, 0.0),
        (40.0, 0.0),
        (40.0, 6.0),
        (16.0, 6.0),
        (16.0, 25.0),
        (10.0, 25.0),
    ]
    .map(|(x, y)| section.geometry.point(Point2::new(x, y)));
    let mut lines = Vec::new();
    for i in 0..corners.len() {
        lines.push(
            section
                .geometry
                .line_between(corners[i], corners[(i + 1) % corners.len()]),
        );
    }
    let section_id = part.add_feature(FeatureKind::Sketch(section));
    let body = part.add_feature(FeatureKind::Revolve(Revolve {
        sketch: section_id,
        op: ExtrudeOp::Boss,
        axis: RevolveAxis::SketchLine(axis.0),
        angle: std::f64::consts::TAU,
        mid_plane: false,
        reverse: false,
        bodies: Vec::new(),
    }));

    // One bolt hole through the disc, sketched on its top face (swept from line 2).
    let mut hole = Sketch::new(SketchPlane::Face(FaceName::profile(body, lines[2].0)));
    hole.circle(Point2::new(28.0, 0.0), 3.0);
    let hole_sketch = part.add_feature(FeatureKind::Sketch(hole));
    let hole = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        hole_sketch,
        ExtrudeOp::Cut,
        6.0,
    )));

    // Six of them around the axis; the count comes from the global variable "Bolts".
    part.add_feature(FeatureKind::CircularPattern(CircularPattern {
        seeds: vec![hole],
        axis: AxisRef::Standard(StandardAxis::Y),
        angle: std::f64::consts::TAU,
        count: 6,
        equal_spacing: true,
        reverse: false,
        skip: Vec::new(),
    }));
    if let Ok(bolts) = part.parse_expression("6") {
        let _ = part.add_global("Bolts", bolts);
    }
    if let (Some(count), Ok(expr)) = (
        part.find_dimension("D2@CirPattern1"),
        part.parse_expression("\"Bolts\""),
    ) {
        let _ = part.add_equation(count, expr);
    }
    part
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use oxyde_format::{PartFile, Properties, read_part, write_part};
    use oxyde_model::{FeatureStatus, Part};
    use oxyde_occt::OcctKernel;

    use super::{bracket, flange};

    fn volume(part: &mut Part, kernel: &mut OcctKernel) -> f64 {
        part.rebuild(kernel);
        for entry in part.timeline() {
            assert_eq!(entry.status, FeatureStatus::Ok, "{}", entry.name);
        }
        crate::app::bodies_volume(kernel, part.body().unwrap()).unwrap()
    }

    #[test]
    fn demo_bracket_builds_and_survives_save_and_load() {
        let mut kernel = OcctKernel::new();
        let mut part = bracket();
        let built = volume(&mut part, &mut kernel);
        // Plate + upright − holes, plus the fillet's material, minus the chamfer's.
        assert!(built > 80.0 * 50.0 * 10.0, "volume {built}");

        let file = PartFile {
            document_id: oxyde_format::new_document_id(),
            definition: part.definition(),
            properties: Properties::default(),
            kernel: oxyde_format::PartKernel::Occt,
        };
        let bytes = write_part(Cursor::new(Vec::new()), &file)
            .unwrap()
            .into_inner();
        let loaded = read_part(Cursor::new(bytes)).unwrap();
        let mut reloaded = Part::from_definition(loaded.definition).unwrap();
        let rebuilt = volume(&mut reloaded, &mut kernel);
        assert!((built - rebuilt).abs() < 1e-6, "{built} vs {rebuilt}");

        part.release(&mut kernel);
        reloaded.release(&mut kernel);
    }

    #[test]
    fn demo_flange_builds() {
        let mut kernel = OcctKernel::new();
        let mut part = flange();
        let built = volume(&mut part, &mut kernel);
        let pi = std::f64::consts::PI;
        let revolved = pi * ((1600.0 - 100.0) * 6.0 + (256.0 - 100.0) * 19.0);
        let holes = 6.0 * pi * 9.0 * 6.0;
        assert!(
            (built - (revolved - holes)).abs() < 1e-3 * revolved,
            "volume {built}"
        );
        part.release(&mut kernel);
    }
}
