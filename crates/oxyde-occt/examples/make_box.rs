//! Builds a 40 × 20 × 10 mm box and writes it to `box.step` in the current directory.
//!
//!     cargo run -p oxyde-occt --example make_box
//!
//! Open the result in any CAD program (SolidWorks, FreeCAD, eDrawings…) to check it.

use std::path::Path;

use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::Kernel;
use oxyde_occt::OcctKernel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("OpenCASCADE {}", OcctKernel::occt_version());

    let mut kernel = OcctKernel::new();
    let shape = kernel.make_box(Point3::ORIGIN, Vec3::new(40.0, 20.0, 10.0))?;

    let counts = kernel.topology_counts(shape)?;
    println!(
        "box: volume {} mm³, {} faces, {} edges, {} vertices",
        kernel.volume(shape)?,
        counts.faces,
        counts.edges,
        counts.vertices
    );

    let path = Path::new("box.step");
    kernel.export_step(&[shape], &[], path)?;
    println!("wrote {}", path.display());
    Ok(())
}
