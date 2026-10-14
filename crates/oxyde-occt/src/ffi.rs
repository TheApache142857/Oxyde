//! Rust ⇄ C++ bridge to the OCCT shim in `cpp/`.
//!
//! Only plain data and opaque handles cross this boundary — never OCCT types.

#[cxx::bridge(namespace = "oxyde::occt")]
pub(crate) mod bridge {
    struct TopologyCounts {
        solids: u32,
        shells: u32,
        faces: u32,
        edges: u32,
        vertices: u32,
    }

    unsafe extern "C++" {
        include!("oxyde-occt/cpp/shim.h");

        /// Owns every shape created through it, keyed by handle.
        type Session;

        fn new_session() -> UniquePtr<Session>;

        /// Version of the OCCT headers the shim was compiled against, e.g. "7.8.1".
        fn occt_version() -> String;

        fn make_box(
            self: Pin<&mut Session>,
            x: f64,
            y: f64,
            z: f64,
            dx: f64,
            dy: f64,
            dz: f64,
        ) -> Result<u64>;

        fn release(self: Pin<&mut Session>, shape: u64);

        fn volume(self: &Session, shape: u64) -> Result<f64>;

        fn topology_counts(self: &Session, shape: u64) -> Result<TopologyCounts>;

        fn export_step(self: &Session, shapes: &[u64], path: &str) -> Result<()>;
    }
}

pub(crate) use bridge::*;
