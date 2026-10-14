//! A small bracket that exercises every feature type, to try the application quickly.

use oxyde_geom::Point2;
use oxyde_model::{
    EdgeBlend, EdgeName, Extrude, ExtrudeOp, FaceName, FeatureKind, Part, Sketch, SketchPlane,
    StandardPlane,
};

/// Ids of the lines of a rectangle drawn with `Sketch::rectangle`.
const RIGHT: u32 = 2;

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
    }));

    // Break the top inside edge of the upright.
    part.add_feature(FeatureKind::Chamfer(EdgeBlend {
        edges: vec![EdgeName::between(
            FaceName::profile(wall, RIGHT),
            FaceName::end_cap(wall),
        )],
        size: 2.0,
    }));

    part
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use oxyde_format::{PartFile, Properties, read_part, write_part};
    use oxyde_kernel::Kernel;
    use oxyde_model::{FeatureStatus, Part};
    use oxyde_occt::OcctKernel;

    use super::bracket;

    fn volume(part: &mut Part, kernel: &mut OcctKernel) -> f64 {
        part.rebuild(kernel);
        for entry in part.timeline() {
            assert_eq!(entry.status, FeatureStatus::Ok, "{}", entry.name);
        }
        kernel.volume(part.body().unwrap().shape).unwrap()
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
}
