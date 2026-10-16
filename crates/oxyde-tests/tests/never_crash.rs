//! A kernel that panics must not take the modelling engine down with it: the feature
//! fails with an error, the rest of the timeline is built, and the part stays usable.
//! See `docs/ARCHITECTURE.md`, "Never crash".

use std::fmt;
use std::path::Path;

use oxyde_geom::{Axis, Plane, Point2, Point3, Profile, Transform, Vec3};
use oxyde_kernel::{
    BooleanOp, Kernel, Mesh, OpResult, ShapeHandle, StepAssembly, StepColors, StepImport,
    StepMaterial, Topology, TopologyCounts,
};
use oxyde_model::{
    Extrude, ExtrudeOp, FeatureKind, FeatureStatus, Part, Sketch, SketchPlane, StandardPlane,
};

#[derive(Debug)]
struct NeverError;

impl fmt::Display for NeverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "never")
    }
}

impl std::error::Error for NeverError {}

/// A kernel with a bug in every operation.
struct PanickingKernel;

impl Kernel for PanickingKernel {
    type Error = NeverError;

    fn make_box(&mut self, _: Point3, _: Vec3) -> Result<ShapeHandle, NeverError> {
        panic!("make_box has a bug")
    }

    fn extrude(&mut self, _: &Plane, _: &Profile, _: Vec3) -> Result<OpResult, NeverError> {
        panic!("extrude has a bug")
    }

    fn revolve(
        &mut self,
        _: &Plane,
        _: &Profile,
        _: &Axis,
        _: f64,
    ) -> Result<OpResult, NeverError> {
        panic!("revolve has a bug")
    }

    fn shell(&mut self, _: ShapeHandle, _: &[u32], _: f64) -> Result<OpResult, NeverError> {
        panic!("shell has a bug")
    }

    fn transform(&mut self, _: ShapeHandle, _: &Transform) -> Result<OpResult, NeverError> {
        panic!("transform has a bug")
    }

    fn boolean_many(
        &mut self,
        _: BooleanOp,
        _: ShapeHandle,
        _: &[ShapeHandle],
    ) -> Result<OpResult, NeverError> {
        panic!("boolean has a bug")
    }

    fn fillet(&mut self, _: ShapeHandle, _: &[u32], _: f64) -> Result<OpResult, NeverError> {
        panic!("fillet has a bug")
    }

    fn chamfer(&mut self, _: ShapeHandle, _: &[u32], _: f64) -> Result<OpResult, NeverError> {
        panic!("chamfer has a bug")
    }

    fn topology(&self, _: ShapeHandle) -> Result<Topology, NeverError> {
        panic!("topology has a bug")
    }

    fn tessellate(&self, _: ShapeHandle, _: f64) -> Result<Mesh, NeverError> {
        panic!("tessellate has a bug")
    }

    fn volume(&self, _: ShapeHandle) -> Result<f64, NeverError> {
        panic!("volume has a bug")
    }

    fn bounds(&self, _: ShapeHandle) -> Result<(Point3, Point3), NeverError> {
        panic!("bounds has a bug")
    }

    fn topology_counts(&self, _: ShapeHandle) -> Result<TopologyCounts, NeverError> {
        panic!("topology_counts has a bug")
    }

    fn export_step(
        &self,
        _: &[ShapeHandle],
        _: &[StepColors],
        _: &[Option<StepMaterial>],
        _: &Path,
    ) -> Result<(), NeverError> {
        panic!("export_step has a bug")
    }

    fn import_step(&mut self, _: &[u8]) -> Result<StepImport, NeverError> {
        panic!("import_step has a bug")
    }

    fn read_step_assembly(&mut self, _: &[u8]) -> Result<StepAssembly, NeverError> {
        panic!("read_step_assembly has a bug")
    }

    fn release(&mut self, _: ShapeHandle) {}
}

#[test]
fn a_panicking_kernel_fails_the_feature_instead_of_the_process() {
    let mut part = Part::new();
    let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
    sketch.rectangle(Point2::new(0.0, 0.0), 10.0, 20.0);
    let s = part.add_feature(FeatureKind::Sketch(sketch));
    let e = part.add_feature(FeatureKind::Extrude(Extrude::blind(
        s,
        ExtrudeOp::Boss,
        5.0,
    )));

    // Would unwind through `Part::rebuild` without the net under every feature build.
    part.rebuild(&mut PanickingKernel);

    assert_eq!(
        part.status(s),
        Some(FeatureStatus::Ok),
        "the sketch needs no kernel"
    );
    match part.status(e) {
        Some(FeatureStatus::Error(message)) => {
            assert!(
                message.contains("internal error") && message.contains("extrude has a bug"),
                "the panic's message is reported: {message}"
            );
        }
        other => panic!("the extrusion should have failed, got {other:?}"),
    }
    assert!(part.body().is_none(), "nothing half-built is kept");

    // The part is still editable and rebuilds again without trouble.
    assert!(part.delete(e).is_ok());
    part.rebuild(&mut PanickingKernel);
    assert_eq!(part.status(s), Some(FeatureStatus::Ok));
}

#[test]
fn catch_panic_carries_the_message() {
    assert_eq!(oxyde_kernel::catch_panic(|| 3), Ok(3));
    let caught: Result<(), String> = oxyde_kernel::catch_panic(|| panic!("boom"));
    assert_eq!(caught, Err("boom".to_owned()));
    let formatted: Result<(), String> = oxyde_kernel::catch_panic(|| panic!("n = {}", 7));
    assert_eq!(formatted, Err("n = 7".to_owned()));
    let unwrapped: Result<u8, String> = oxyde_kernel::catch_panic(|| None::<u8>.unwrap());
    assert!(unwrapped.is_err());
}
