//! Shapes are shared between kernels: one made on a worker thread's kernel is usable, and
//! can be released, from the main thread's.

use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::Kernel;
use oxyde_occt::OcctKernel;

#[test]
fn shapes_made_on_worker_threads_are_usable_from_the_main_kernel() {
    let mut main = OcctKernel::new();
    let made: Vec<(f64, _)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (1..=4)
            .map(|i| {
                scope.spawn(move || {
                    let mut kernel = OcctKernel::open_worker().expect("OCCT opens workers");
                    let length = i as f64;
                    let shape = kernel
                        .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(length, 1.0, 1.0))
                        .unwrap();
                    // Meshed on the worker too, as a part being loaded is.
                    let mesh = kernel.tessellate(shape, 0.1).unwrap();
                    assert_eq!(mesh.faces.len(), 6);
                    let volume = kernel.volume(shape).expect("volume on the worker");
                    assert!((volume - length).abs() < 1e-9, "worker volume = {volume}");
                    (length, shape)
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap()).collect()
    });
    assert_eq!(made.len(), 4);
    for (length, shape) in made {
        assert_eq!(
            main.topology_counts(shape).expect("lookup on main").faces,
            6
        );
        let volume = main.volume(shape).expect("volume on main");
        assert!((volume - length).abs() < 1e-9, "volume = {volume}");
        main.release(shape);
        assert!(main.volume(shape).is_err(), "released shapes are gone");
    }
}

#[test]
fn handles_are_unique_across_kernels() {
    let mut a = OcctKernel::new();
    let mut b = OcctKernel::open_worker().unwrap();
    let origin = Point3::new(0.0, 0.0, 0.0);
    let x = a.make_box(origin, Vec3::new(1.0, 1.0, 1.0)).unwrap();
    let y = b.make_box(origin, Vec3::new(2.0, 1.0, 1.0)).unwrap();
    assert_ne!(x, y);
    assert!((b.volume(x).unwrap() - 1.0).abs() < 1e-9);
    assert!((a.volume(y).unwrap() - 2.0).abs() < 1e-9);
    a.release(x);
    b.release(y);
}
