//! Converts a STEP assembly into Oxyde part and assembly files, as File › Import STEP
//! assembly does, from the command line:
//!
//!     cargo run -p oxyde-tests --example step_to_oxyde -- cart.step out/
//!
//! Prints the top-level file it wrote.

use std::path::PathBuf;

use oxyde_format::step_import::plan_step_import;
use oxyde_kernel::Kernel;
use oxyde_occt::OcctKernel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let (Some(step), Some(out)) = (
        args.next().map(PathBuf::from),
        args.next().map(PathBuf::from),
    ) else {
        eprintln!("usage: step_to_oxyde <file.step> <output folder>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&step)?;
    let mut kernel = OcctKernel::new();
    let tree = kernel.read_step_assembly(&bytes)?;
    let source = step.file_name().map_or_else(
        || "import.step".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let plan = plan_step_import(&tree, &source)?;
    let top = plan.write(&out)?;
    println!(
        "{} ({} parts, {} assemblies)",
        top.display(),
        plan.part_count(),
        plan.assembly_count()
    );
    Ok(())
}
