// Netgen's volume mesher behind a plain interface, for shim.cpp.
//
// Compiled only when the build script finds Netgen (OXYDE_NETGEN defined), and in a
// translation unit of its own: Netgen's headers bring `using namespace std`, macros and
// globals that the rest of the shim must not see.
#pragma once

#include <TopoDS_Shape.hxx>

#include <cstdint>
#include <string>
#include <vector>

namespace oxyde::occt::netgen_bridge {

struct Options {
    double max_size = 1e10;
    double min_size = 0.0;
    double grading = 0.3;
    double curvature_safety = 2.0;
    double segments_per_edge = 1.0;
    bool second_order = false;
    int optimize_steps = 3;
};

// A mesh as Netgen makes it: nodes numbered from 0, Netgen's own node order within each
// element (TET10: corners, then the edges 01 02 03 12 13 23; TRIG6: corners, then the
// edges 12 02 01).
struct Result {
    std::vector<double> nodes; // x, y, z per node
    std::uint32_t nodes_per_tet = 4;
    std::vector<std::uint32_t> tets;
    std::uint32_t nodes_per_triangle = 3;
    std::vector<std::uint32_t> triangles;
    // The faces Netgen meshed, in its order, and per triangle an index into them.
    std::vector<TopoDS_Shape> faces;
    std::vector<std::uint32_t> triangle_faces;
};

// Meshes one solid. Throws (std::exception) on failure. Netgen keeps global state, so
// calls run one at a time.
Result mesh_solid(const TopoDS_Shape& solid, const Options& options);

// Netgen's version, e.g. "6.2.2401".
std::string version();

} // namespace oxyde::occt::netgen_bridge
