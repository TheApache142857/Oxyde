// Coarse-grained C++ API over OpenCASCADE, exposed to Rust through cxx (see src/ffi.rs).
//
// Rules (docs/ARCHITECTURE.md §3):
//  - no OCCT type appears in a function exposed to Rust; shapes are referred to by handle;
//  - every OCCT exception is caught and rethrown as std::runtime_error, which cxx turns
//    into a Rust Err;
//  - every modeling operation reports, for each face of its result, where that face came
//    from (the raw material of persistent naming).
#pragma once

#include "rust/cxx.h"

#include <TopoDS_Shape.hxx>

#include <cstdint>
#include <memory>
#include <unordered_map>

namespace oxyde::occt {

// Shared structs, defined by the cxx-generated header.
struct OpResultData;
struct PlaneData;
struct RegionData;
struct TopologyCounts;
struct TopologyData;

class Session {
public:
    std::uint64_t make_box(double x, double y, double z, double dx, double dy, double dz);
    OpResultData extrude(const PlaneData& plane, rust::Slice<const RegionData> regions,
                         double dx, double dy, double dz);
    OpResultData boolean_op(std::uint8_t kind, std::uint64_t target, std::uint64_t tool);
    OpResultData fillet(std::uint64_t shape, rust::Slice<const std::uint32_t> edges,
                        double radius);
    OpResultData chamfer(std::uint64_t shape, rust::Slice<const std::uint32_t> edges,
                         double distance);
    void release(std::uint64_t shape);

    TopologyData topology(std::uint64_t shape) const;
    double volume(std::uint64_t shape) const;
    TopologyCounts topology_counts(std::uint64_t shape) const;

    void export_step(rust::Slice<const std::uint64_t> shapes, rust::Str path) const;

private:
    std::uint64_t insert(const TopoDS_Shape& shape);
    const TopoDS_Shape& get(std::uint64_t shape) const;

    std::unordered_map<std::uint64_t, TopoDS_Shape> shapes_;
    std::uint64_t next_id_ = 1; // 0 is never issued
};

std::unique_ptr<Session> new_session();

rust::String occt_version();

} // namespace oxyde::occt
