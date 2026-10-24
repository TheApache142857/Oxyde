//! STEP files (ISO 10303): written from the kernel's shapes and read into them
//! (docs/NATIVE_KERNEL.md §4, STEP).

// Indices into the instances and the shapes' parts, gathered here.
#![allow(clippy::indexing_slicing)]

pub(crate) mod exact;
pub(crate) mod p21;
pub(crate) mod read;
pub(crate) mod write;
