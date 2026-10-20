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

#include "oxyde-occt/cpp/netgen_mesh.h"

#include <cstdint>
#include <memory>

namespace oxyde::occt {

// Shared structs, defined by the cxx-generated header.
struct AxisData;
struct BoundsData;
struct LoftSectionData;
struct DistanceData;
struct PolylineData;
struct ProjectionData;
struct MeshData;
struct MeshProgressData;
struct OpResultData;
struct PlaneData;
struct PointData;
struct RegionData;
struct SegmentData;
struct StepAssemblyData;
struct StepImportData;
struct StepMaterialData;
struct StepShapeColors;
struct TopologyCounts;
struct TopologyData;
struct VolumeMeshData;
struct VolumeMeshOptionsData;

// A volume mesh's progress and request to stop, shared with the Rust thread watching it.
// Thread-safe; meaningful without Netgen too (it then never moves).
class MeshMonitor {
public:
    void request_stop() const;
    MeshProgressData progress() const;

    netgen_bridge::Monitor inner;
};

std::unique_ptr<MeshMonitor> new_mesh_monitor();

class Session {
public:
    std::uint64_t make_box(double x, double y, double z, double dx, double dy, double dz);
    OpResultData extrude(const PlaneData& plane, rust::Slice<const RegionData> regions,
                         double dx, double dy, double dz);
    OpResultData revolve(const PlaneData& plane, rust::Slice<const RegionData> regions,
                         const AxisData& axis, double angle);
    OpResultData loft(rust::Slice<const LoftSectionData> sections, bool ruled);
    OpResultData sweep(const PlaneData& plane, rust::Slice<const RegionData> regions,
                       const PlaneData& path_plane, rust::Slice<const SegmentData> path);
    OpResultData shell(std::uint64_t shape, rust::Slice<const std::uint32_t> faces,
                       double offset);
    OpResultData transform(std::uint64_t shape, rust::Slice<const double> rows);
    OpResultData boolean_op(std::uint8_t kind, std::uint64_t target,
                            rust::Slice<const std::uint64_t> tools);
    OpResultData solid_from_mesh(rust::Slice<const double> points,
                                 rust::Slice<const std::uint32_t> triangles);
    OpResultData fillet(std::uint64_t shape, rust::Slice<const std::uint32_t> edges,
                        double radius);
    OpResultData chamfer(std::uint64_t shape, rust::Slice<const std::uint32_t> edges,
                         double distance);
    OpResultData chamfer_with(std::uint64_t shape, rust::Slice<const std::uint32_t> edges,
                              rust::Slice<const std::uint32_t> faces, double first,
                              std::uint8_t kind, double second);
    void release(std::uint64_t shape);

    TopologyData topology(std::uint64_t shape) const;
    MeshData tessellate(std::uint64_t shape, double deflection) const;
    MeshData tessellate_with(std::uint64_t shape, double deflection, double angle) const;
    double volume(std::uint64_t shape) const;
    PointData centre_of_mass(std::uint64_t shape) const;
    BoundsData bounds(std::uint64_t shape) const;
    DistanceData distance(std::uint64_t a, std::uint8_t a_kind, std::uint32_t a_index,
                          std::uint64_t b, std::uint8_t b_kind, std::uint32_t b_index) const;
    ProjectionData project_edges(rust::Slice<const std::uint64_t> shapes, const PlaneData& view,
                                 double deflection) const;
    TopologyCounts topology_counts(std::uint64_t shape) const;
    VolumeMeshData volume_mesh(std::uint64_t shape, const VolumeMeshOptionsData& options,
                               const MeshMonitor& monitor) const;

    void export_step(rust::Slice<const std::uint64_t> shapes,
                     rust::Slice<const StepShapeColors> colors,
                     rust::Slice<const StepMaterialData> materials, rust::Str path) const;
    void export_iges(rust::Slice<const std::uint64_t> shapes, rust::Str path) const;
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

bool has_netgen();

rust::String netgen_version();

} // namespace oxyde::occt
