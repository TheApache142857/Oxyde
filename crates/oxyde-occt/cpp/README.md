# C++ shim

`shim.h` / `shim.cpp` will live here: a small, coarse-grained C++ API over OpenCASCADE,
exposed to Rust through `cxx`. See `docs/ARCHITECTURE.md` §3 for the rules
(no OCCT types in signatures, no exceptions across the boundary, operation history returned
with every result).
