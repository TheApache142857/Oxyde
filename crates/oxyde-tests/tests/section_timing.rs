//! Where the time goes in a Section View cut, step by step (the boolean, the topology,
//! the meshing), on the STEP cart or on a part file named by `OXYDE_SECTION_PART`.
//! Ignored by default: a measurement, not a check. Run it with
//! `cargo test -p oxyde-tests --test section_timing -- --ignored --nocapture`.

use std::time::Instant;

use oxyde_geom::{Plane, Point3, Transform, Vec3};
use oxyde_kernel::{BooleanOp, Kernel, Topology};
use oxyde_model::{FeatureKind, FileData, Import, Part, StandardPlane};
use oxyde_occt::OcctKernel;

const CART: &[u8] = include_bytes!("data/cart.step");

/// As `mesh_deflection`, which this crate does not depend on.
fn mesh_deflection(topology: &Topology, fraction: f64) -> f64 {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let points = topology
        .vertices
        .iter()
        .map(|v| v.point)
        .chain(topology.faces.iter().map(|f| f.centroid));
    for p in points {
        for (k, value) in [p.x, p.y, p.z].into_iter().enumerate() {
            lo[k] = lo[k].min(value);
            hi[k] = hi[k].max(value);
        }
    }
    let diagonal = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f64>().sqrt();
    if diagonal.is_finite() && diagonal > 0.0 {
        (diagonal * fraction).clamp(0.0005, 5.0)
    } else {
        0.05
    }
}

fn plane_transform(p: &Plane) -> Transform {
    let y = p.y_dir();
    Transform {
        rows: [
            [p.x_dir.x, y.x, p.normal.x, p.origin.x],
            [p.x_dir.y, y.y, p.normal.y, p.origin.y],
            [p.x_dir.z, y.z, p.normal.z, p.origin.z],
        ],
    }
}

#[test]
#[ignore]
fn section_timing() {
    // OXYDE_SECTION_PART=path.oxyprt measures a real part instead of the cart.
    let t = Instant::now();
    let mut rebuilt = None;
    let mut local = None;
    let (kernel, part): (&mut OcctKernel, &Part) = match std::env::var_os("OXYDE_SECTION_PART") {
        Some(path) => {
            let r = rebuilt.insert(oxyde_cli::rebuild(std::path::Path::new(&path)).unwrap());
            (&mut r.kernel, &r.part)
        }
        None => {
            let mut kernel = OcctKernel::new();
            let mut part = Part::new();
            part.add_feature(FeatureKind::Import(Import::new(
                "cart.step",
                FileData::new(CART.to_vec()),
            )));
            part.rebuild(&mut kernel);
            let (k, p) = local.insert((kernel, part));
            (k, &*p)
        }
    };
    eprintln!("rebuild: {:?}", t.elapsed());
    let body = part.body().unwrap().clone();
    eprintln!(
        "solids: {}, faces: {}",
        body.solids.len(),
        body.topology.faces.len()
    );
    let body_deflection = mesh_deflection(&body.topology, 0.001);

    // The display mesh, as the app has it before the section opens.
    let t = Instant::now();
    for s in &body.solids {
        kernel.tessellate(s.shape, body_deflection).unwrap();
    }
    eprintln!("display tessellate (cold): {:?}", t.elapsed());
    let t = Instant::now();
    for s in &body.solids {
        kernel.tessellate(s.shape, body_deflection).unwrap();
    }
    eprintln!("display tessellate (warm): {:?}", t.elapsed());

    let t = Instant::now();
    let mut lo = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut hi = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for s in &body.solids {
        let (a, b) = kernel.bounds(s.shape).unwrap();
        lo = Point3::new(lo.x.min(a.x), lo.y.min(a.y), lo.z.min(a.z));
        hi = Point3::new(hi.x.max(b.x), hi.y.max(b.y), hi.z.max(b.z));
    }
    eprintln!("bounds of every solid: {:?}  {lo:?}..{hi:?}", t.elapsed());

    for std_plane in [
        StandardPlane::Front,
        StandardPlane::Top,
        StandardPlane::Right,
    ] {
        let base = std_plane.plane();
        let n = base.normal;
        let mid = (Point3::new(
            (lo.x + hi.x) / 2.0,
            (lo.y + hi.y) / 2.0,
            (lo.z + hi.z) / 2.0,
        ) - base.origin)
            .dot(n);
        let plane = base.offset(mid);
        let l = (hi - lo).length() * 2.0 + 10.0;
        let raw = kernel
            .make_box(Point3::new(-l, -l, 0.0), Vec3::new(2.0 * l, 2.0 * l, l))
            .unwrap();
        let tool = kernel
            .transform(raw, &plane_transform(&plane))
            .unwrap()
            .shape;
        kernel.release(raw);

        let (mut t_bool, mut t_topo, mut t_mesh_cut, mut t_mesh_body) = (0.0, 0.0, 0.0, 0.0);
        let (mut straddle, mut kept, mut removed) = (0, 0, 0);
        for s in &body.solids {
            let (a, b) = kernel.bounds(s.shape).unwrap();
            let mut dmin = f64::INFINITY;
            let mut dmax = f64::NEG_INFINITY;
            for x in [a.x, b.x] {
                for y in [a.y, b.y] {
                    for z in [a.z, b.z] {
                        let d = (Point3::new(x, y, z) - plane.origin).dot(n);
                        dmin = dmin.min(d);
                        dmax = dmax.max(d);
                    }
                }
            }
            if dmax <= 0.0 {
                kept += 1;
            } else if dmin >= 0.0 {
                removed += 1;
            } else {
                straddle += 1;
            }
            let t = Instant::now();
            let result = kernel.boolean(BooleanOp::Cut, s.shape, tool).unwrap();
            t_bool += t.elapsed().as_secs_f64();
            let t = Instant::now();
            let topo = kernel.topology(result.shape).unwrap();
            t_topo += t.elapsed().as_secs_f64();
            if topo.faces.is_empty() {
                kernel.release(result.shape);
                continue;
            }
            let t = Instant::now();
            let d = mesh_deflection(&topo, 0.001);
            kernel.tessellate(result.shape, d).unwrap();
            t_mesh_cut += t.elapsed().as_secs_f64();
            kernel.release(result.shape);
            // Again, meshed at the body's deflection instead.
            let result = kernel.boolean(BooleanOp::Cut, s.shape, tool).unwrap();
            let t = Instant::now();
            kernel.tessellate(result.shape, body_deflection).unwrap();
            t_mesh_body += t.elapsed().as_secs_f64();
            kernel.release(result.shape);
        }
        kernel.release(tool);
        eprintln!(
            "{std_plane:?}: straddle {straddle} kept {kept} removed {removed}; boolean {:.0} ms, \
             topology {:.0} ms, tessellate at cut deflection {:.0} ms, at body deflection {:.0} ms",
            t_bool * 1000.0,
            t_topo * 1000.0,
            t_mesh_cut * 1000.0,
            t_mesh_body * 1000.0
        );
    }
}
