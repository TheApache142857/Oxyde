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

namespace oxyde::occt {

// Shared structs, defined by the cxx-generated header.
struct AxisData;
struct BoundsData;
struct MeshData;
struct OpResultData;
struct PlaneData;
struct RegionData;
struct StepAssemblyData;
struct StepImportData;
struct StepShapeColors;
struct TopologyCounts;
struct TopologyData;

class Session {
public:
    std::uint64_t make_box(double x, double y, double z, double dx, double dy, double dz);
    OpResultData extrude(const PlaneData& plane, rust::Slice<const RegionData> regions,
                         double dx, double dy, double dz);
    OpResultData revolve(const PlaneData& plane, rust::Slice<const RegionData> regions,
                         const AxisData& axis, double angle);
    OpResultData shell(std::uint64_t shape, rust::Slice<const std::uint32_t> faces,
                       double offset);
    OpResultData transform(std::uint64_t shape, rust::Slice<const double> rows);
    OpResultData boolean_op(std::uint8_t kind, std::uint64_t target,
                            rust::Slice<const std::uint64_t> tools);
    OpResultData fillet(std::uint64_t shape, rust::Slice<const std::uint32_t> edges,
                        double radius);
    OpResultData chamfer(std::uint64_t shape, rust::Slice<const std::uint32_t> edges,
                         double distance);
    void release(std::uint64_t shape);

    TopologyData topology(std::uint64_t shape) const;
    MeshData tessellate(std::uint64_t shape, double deflection) const;
    MeshData tessellate_with(std::uint64_t shape, double deflection, double angle) const;
    double volume(std::uint64_t shape) const;
    BoundsData bounds(std::uint64_t shape) const;
    TopologyCounts topology_counts(std::uint64_t shape) const;

    void export_step(rust::Slice<const std::uint64_t> shapes,
                     rust::Slice<const StepShapeColors> colors, rust::Str path) const;
    StepImportData import_step(rust::Slice<const std::uint8_t> data);
    StepAssemblyData read_step_assembly(rust::Slice<const std::uint8_t> data);

private:
    // Shapes live in one arena shared by every Session of the process (shim.cpp), so a
    // handle is valid in any session, on any thread. `get` returns a copy of the
    // reference-counted handle: nothing points into the arena.
    static std::uint64_t insert(const TopoDS_Shape& shape);
    static TopoDS_Shape get(std::uint64_t shape);
};

std::unique_ptr<Session> new_session();

rust::String occt_version();

} // namespace oxyde::occt
