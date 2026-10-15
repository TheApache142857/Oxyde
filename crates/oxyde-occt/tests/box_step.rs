//! M1 acceptance tests: build a box with OpenCASCADE, inspect it, export it to STEP.

use std::path::PathBuf;

use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{Kernel, ShapeHandle, TopologyCounts};
use oxyde_occt::OcctKernel;

fn temp_step_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("oxyde-{name}-{}.step", std::process::id()))
}

#[test]
fn reports_occt_version() {
    let version = OcctKernel::occt_version();
    assert!(
        version.starts_with('7') || version.starts_with('8'),
        "unexpected version {version}"
    );
}

#[test]
fn box_has_expected_volume_and_topology() {
    let mut kernel = OcctKernel::new();
    let shape = kernel
        .make_box(Point3::new(5.0, -3.0, 1.0), Vec3::new(10.0, 20.0, 30.0))
        .unwrap();

    let volume = kernel.volume(shape).unwrap();
    assert!((volume - 6000.0).abs() < 1e-6, "volume = {volume}");

    assert_eq!(
        kernel.topology_counts(shape).unwrap(),
        TopologyCounts {
            solids: 1,
            shells: 1,
            faces: 6,
            edges: 12,
            vertices: 8,
        }
    );
}

#[test]
fn box_exports_to_step() {
    let mut kernel = OcctKernel::new();
    let shape = kernel
        .make_box(Point3::ORIGIN, Vec3::new(40.0, 20.0, 10.0))
        .unwrap();

    let path = temp_step_path("box");
    kernel.export_step(&[shape], &[], &path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);

    assert!(text.starts_with("ISO-10303-21;"), "not a STEP file");
    assert!(
        text.contains("MANIFOLD_SOLID_BREP"),
        "no solid in STEP output"
    );
}

#[test]
fn zero_size_box_is_an_error() {
    let mut kernel = OcctKernel::new();
    let result = kernel.make_box(Point3::ORIGIN, Vec3::new(0.0, 10.0, 10.0));
    assert!(result.is_err());
}

#[test]
fn released_handle_is_an_error() {
    let mut kernel = OcctKernel::new();
    let shape = kernel
        .make_box(Point3::ORIGIN, Vec3::new(1.0, 1.0, 1.0))
        .unwrap();
    kernel.release(shape);
    kernel.release(shape); // releasing twice is a no-op

    let err = kernel.volume(shape).unwrap_err();
    assert!(err.message().contains("unknown shape handle"), "{err}");
}

#[test]
fn foreign_handle_is_an_error() {
    let kernel = OcctKernel::new();
    assert!(kernel.volume(ShapeHandle::from_raw(42)).is_err());
}

#[test]
fn exporting_nothing_is_an_error() {
    let kernel = OcctKernel::new();
    assert!(kernel.export_step(&[], &[], &temp_step_path("empty")).is_err());
}

#[test]
fn box_tessellation_matches_topology() {
    let mut kernel = OcctKernel::new();
    let shape = kernel
        .make_box(Point3::ORIGIN, Vec3::new(10.0, 20.0, 30.0))
        .unwrap();
    let mesh = kernel.tessellate(shape, 0.1).unwrap();
    let topology = kernel.topology(shape).unwrap();

    assert_eq!(mesh.faces.len(), topology.faces.len());
    assert_eq!(mesh.edges.len(), topology.edges.len());
    for face in &mesh.faces {
        assert!(face.triangles.len() >= 2);
        for t in &face.triangles {
            assert!(t.iter().all(|&i| (i as usize) < face.positions.len()));
        }
    }
    // Straight edges need only their two end points.
    assert!(mesh.edges.iter().all(|e| e.len() == 2));
}

/// Planar faces whose normal does not point away from the centre of `shape`.
fn inward_faces(kernel: &OcctKernel, shape: ShapeHandle, center: Point3) -> Vec<usize> {
    let topology = kernel.topology(shape).unwrap();
    (0..topology.faces.len())
        .filter(|&i| {
            let face = &topology.faces[i];
            let normal = face.normal.expect("a box has flat faces");
            (face.centroid - center).dot(normal) <= 0.0
        })
        .collect()
}

#[test]
fn mirrored_and_rotated_faces_still_face_outward() {
    use oxyde_geom::{Axis, Plane, Transform};
    let mut kernel = OcctKernel::new();
    let shape = kernel
        .make_box(Point3::new(10.0, 0.0, 0.0), Vec3::new(10.0, 20.0, 30.0))
        .unwrap();
    assert_eq!(
        inward_faces(&kernel, shape, Point3::new(15.0, 10.0, 15.0)),
        []
    );

    // A mirror turns the planes' frames left-handed: normals must still point out.
    let mirrored = kernel
        .transform(shape, &Transform::mirror(&Plane::RIGHT))
        .unwrap();
    assert!((kernel.volume(mirrored.shape).unwrap() - 6000.0).abs() < 1e-6);
    assert_eq!(
        inward_faces(&kernel, mirrored.shape, Point3::new(-15.0, 10.0, 15.0)),
        []
    );

    let quarter = Transform::rotation(&Axis::Z, std::f64::consts::FRAC_PI_2);
    let rotated = kernel.transform(shape, &quarter).unwrap();
    assert_eq!(
        inward_faces(&kernel, rotated.shape, Point3::new(-10.0, 15.0, 15.0)),
        []
    );
}

#[test]
fn bounds_are_exact() {
    let mut kernel = OcctKernel::new();
    let shape = kernel
        .make_box(Point3::new(5.0, -3.0, 1.0), Vec3::new(10.0, 20.0, 30.0))
        .unwrap();
    let (lo, hi) = kernel.bounds(shape).unwrap();
    for (got, want) in [
        (lo.x, 5.0),
        (lo.y, -3.0),
        (lo.z, 1.0),
        (hi.x, 15.0),
        (hi.y, 17.0),
        (hi.z, 31.0),
    ] {
        assert!((got - want).abs() < 1e-9, "{got} vs {want}");
    }
}
