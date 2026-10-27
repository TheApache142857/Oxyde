//! Translates CAD files to STEP with cadmpeg and reads each STEP with the native kernel:
//! `cargo run -p oxyde-cadmpeg --example cad_to_step -- FILE...`. Writes `FILE.step` next
//! to each file when OXYDE_WRITE_STEP is set.

use oxyde_kernel::Kernel;
use oxyde_native_kernel::NativeKernel;

fn main() {
    let mut kernel = NativeKernel::new();
    for path in std::env::args().skip(1) {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                println!("{path}: {e}");
                continue;
            }
        };
        let started = std::time::Instant::now();
        let converted = match oxyde_cadmpeg::to_step(&bytes) {
            Ok(converted) => converted,
            Err(e) => {
                println!("{path}: {e}");
                continue;
            }
        };
        if std::env::var_os("OXYDE_WRITE_STEP").is_some() {
            let _ = std::fs::write(format!("{path}.step"), &converted.step);
        }
        let read = match kernel.import_step(&converted.step) {
            Ok(import) => match kernel.volume(import.shape) {
                Ok(volume) => format!("solid, volume {volume:.3}"),
                Err(e) => format!("shape without volume: {e}"),
            },
            Err(e) => format!("native kernel could not read the STEP: {e}"),
        };
        println!(
            "{path}: {} -> {} bytes of STEP, {} loss(es), {:.2}s; {read}",
            converted.format,
            converted.step.len(),
            converted.losses,
            started.elapsed().as_secs_f64()
        );
    }
}
