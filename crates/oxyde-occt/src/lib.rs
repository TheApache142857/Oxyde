//! OpenCASCADE backend for `oxyde-kernel`.
//!
//! Talks to OCCT through a thin C++ shim (`cpp/`) bridged with `cxx`. No OCCT type crosses
//! the boundary: Rust holds opaque shape handles. See `docs/ARCHITECTURE.md` §3.
