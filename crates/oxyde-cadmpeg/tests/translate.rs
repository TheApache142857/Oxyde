//! FreeCAD files translated to STEP by cadmpeg and read back by the native kernel: each
//! comes through as one solid of the volume its authoring script gives
//! (`tests/fixtures/README.md`).

use oxyde_cadmpeg::{ConvertError, to_step};
use oxyde_kernel::Kernel;
use oxyde_native_kernel::NativeKernel;

/// Translates a fixture, checks the losses cadmpeg reports, and gives the volume the native
/// kernel reads from its STEP.
fn volume_of(name: &str, losses: usize) -> f64 {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let converted = to_step(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(converted.format, "fcstd", "{name}");
    assert_eq!(converted.losses, losses, "{name}: losses cadmpeg reported");
    let mut kernel = NativeKernel::new();
    let import = kernel
        .import_step(&converted.step)
        .unwrap_or_else(|e| panic!("{name}: the native kernel could not read the STEP: {e}"));
    kernel
        .volume(import.shape)
        .unwrap_or_else(|e| panic!("{name}: no volume: {e}"))
}

fn assert_volume(name: &str, losses: usize, expected: f64) {
    let volume = volume_of(name, losses);
    assert!(
        (volume - expected).abs() < 1e-3 * expected,
        "{name}: volume {volume}, expected {expected}"
    );
}

#[test]
fn a_freecad_box_comes_through_as_a_solid() {
    assert_volume("uniform_appearance.FCStd", 0, 6000.0);
}

#[test]
fn a_freecad_cylinder_comes_through_as_a_solid() {
    assert_volume("external_component.FCStd", 0, 100.0 * std::f64::consts::PI);
}

#[test]
fn a_freecad_cylinder_fused_with_a_sphere_comes_through_as_a_solid() {
    let pi = std::f64::consts::PI;
    let root15 = 15f64.sqrt();
    let common = pi * (512.0 - 512.0 / 3.0 - 59.0 * root15 + 49.0 * root15);
    let expected = 588.0 * pi + 2048.0 * pi / 3.0 - common;
    // The sphere's pole is an edge without a 3D curve, which cadmpeg's STEP leaves out and
    // reports twice (the edge, and the count of such edges); the solid is whole without it.
    assert_volume("binary_exact_shape.FCStd", 2, expected);
}

#[test]
fn a_file_no_cad_format_matches_is_unrecognized() {
    let text = b"Just some notes, not a CAD file.\n".repeat(64);
    assert_eq!(to_step(&text).unwrap_err(), ConvertError::Unrecognized);
}
