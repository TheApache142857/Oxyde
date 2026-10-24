//! A benchmark of booleans with many tools: panels like the hackintosh case's, a plate cut by
//! two staggered linear patterns of hexagonal holes, rebuilt by both kernels with their
//! times, volumes and face counts printed. A pattern's cuts are one boolean with every
//! instance as a tool. Run by hand (the native kernel optimised, as a release build is):
//! `cargo test -p oxyde-tests --test hole_panels --config
//! 'profile.dev.package.oxyde-native-kernel.opt-level=3' -- --ignored --nocapture`;
//! `OXYDE_PANEL_HOLES=36,100` picks the sizes (holes in all, a multiple of 4).

use std::time::Instant;

use oxyde_geom::Point2;
use oxyde_kernel::Kernel;
use oxyde_model::{
    AxisRef, Extrude, ExtrudeExtent, ExtrudeOp, FaceName, FeatureKind, FeatureStatus,
    LinearPattern, Part, PatternDirection, Sketch, SketchPlane, StandardAxis, StandardPlane,
};
use oxyde_native_kernel::NativeKernel;
use oxyde_occt::OcctKernel;

/// A plate 2 mm thick cut by two staggered grids of `columns` × `rows` hexagonal holes
/// (circumradius 2.5 mm, 8 mm apart along x and 7 mm along y), each a seed hole patterned.
pub fn hex_panel(columns: u32, rows: u32) -> Part {
    let (dx, dy, r) = (8.0, 7.0, 2.5);
    let (w, h) = (f64::from(columns) * dx + 10.0, f64::from(rows) * dy + 10.0);
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::new(0.0, 0.0), w, h);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let base = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        2.0,
    )));
    for (ox, oy) in [(7.0, 6.0), (7.0 + dx / 2.0, 6.0 + dy / 2.0)] {
        let mut sketch = Sketch::new(SketchPlane::Face(FaceName::end_cap(base)));
        let corners: Vec<(f64, f64)> = (0..6)
            .map(|k| {
                let a = std::f64::consts::FRAC_PI_6 + f64::from(k) * std::f64::consts::FRAC_PI_3;
                (ox + r * a.cos(), oy + r * a.sin())
            })
            .collect();
        for (i, &(x, y)) in corners.iter().enumerate() {
            let (nx, ny) = corners[(i + 1) % corners.len()];
            sketch.line(Point2::new(x, y), Point2::new(nx, ny));
        }
        let s = part.add_feature(FeatureKind::Sketch(sketch));
        let mut cut = Extrude::blind(s, ExtrudeOp::Cut, 1.0);
        cut.extent = ExtrudeExtent::ThroughAll { both: false };
        let hole = part.add_feature(FeatureKind::Extrude(cut));
        part.add_feature(FeatureKind::LinearPattern(LinearPattern {
            seeds: vec![hole],
            first: PatternDirection {
                axis: AxisRef::Standard(StandardAxis::X),
                spacing: dx,
                count: columns,
                reverse: false,
            },
            second: Some(PatternDirection {
                axis: AxisRef::Standard(StandardAxis::Y),
                spacing: dy,
                count: rows,
                reverse: false,
            }),
            skip: Vec::new(),
        }));
    }
    part
}

/// One kernel's rebuild of `part`: seconds, the body's volume and face count, and the
/// first feature that failed, if one did.
fn rebuild<K: Kernel>(part: &Part, kernel: &mut K) -> (f64, f64, usize, Option<String>) {
    let mut part = Part::from_definition(part.definition()).unwrap();
    let start = Instant::now();
    part.rebuild(kernel);
    let seconds = start.elapsed().as_secs_f64();
    let failed = part
        .timeline()
        .iter()
        .find(|e| e.status != FeatureStatus::Ok)
        .map(|e| format!("{}: {:?}", e.name, e.status));
    let (volume, faces) = part.body().map_or((0.0, 0), |body| {
        let volume = body
            .solids
            .iter()
            .map(|s| kernel.volume(s.shape).unwrap())
            .sum();
        (volume, body.topology.faces.len())
    });
    part.release(kernel);
    (seconds, volume, faces, failed)
}

#[test]
#[ignore = "a benchmark, run by hand"]
fn hole_panels_rebuild_with_both_kernels() {
    let sizes: Vec<u32> = std::env::var("OXYDE_PANEL_HOLES")
        .ok()
        .map(|s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![36, 100, 400]);
    for holes in sizes {
        // Two grids of columns × rows, about twice as wide as high.
        let per_grid = holes / 2;
        let rows = ((f64::from(per_grid) / 2.0).sqrt().round() as u32).max(1);
        let columns = per_grid.div_ceil(rows);
        let part = hex_panel(columns, rows);
        let (occt_s, occt_v, occt_f, occt_err) = rebuild(&part, &mut OcctKernel::new());
        let (native_s, native_v, native_f, native_err) = rebuild(&part, &mut NativeKernel::new());
        println!(
            "PANEL {} holes ({columns} x {rows} x 2): OpenCASCADE {occt_s:.2} s, {occt_f} faces, \
             volume {occt_v:.6}{}; native {native_s:.2} s, {native_f} faces, volume {native_v:.6}{}; \
             volume off {:.1e}",
            2 * columns * rows,
            occt_err.map(|e| format!(" ({e})")).unwrap_or_default(),
            native_err.map(|e| format!(" ({e})")).unwrap_or_default(),
            (native_v - occt_v).abs() / occt_v.abs().max(1.0),
        );
    }
}
