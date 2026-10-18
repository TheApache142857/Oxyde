// Netgen's OpenCASCADE mesher (netgen/occ), called on one solid. See netgen_mesh.h.
//
// Thin on purpose: set Netgen's parameters, run its pipeline (surface, volume,
// optimisation, then second order on the true geometry), copy the result out as plain
// numbers. Orientation and node order are converted on the Rust side.
#include "oxyde-occt/cpp/netgen_mesh.h"

#include <meshing.hpp>
#include <occgeom.hpp>
#if __has_include(<netgen_version.hpp>)
#include <netgen_version.hpp>
#endif

#include <memory>
#include <mutex>
#include <stdexcept>

namespace oxyde::occt::netgen_bridge {

namespace {

// Netgen keeps its progress, cancel flag and diagnostics in globals: one mesh at a time.
std::mutex& netgen_mutex() {
    static std::mutex mutex;
    return mutex;
}

std::uint32_t node_index(netgen::PointIndex pi) {
    const int i = static_cast<int>(pi) - static_cast<int>(netgen::PointIndex::BASE);
    if (i < 0) {
        throw std::runtime_error("Netgen gave an element with an unset node");
    }
    return static_cast<std::uint32_t>(i);
}

} // namespace

Result mesh_solid(const TopoDS_Shape& solid, const Options& options) {
    const std::lock_guard<std::mutex> lock(netgen_mutex());
    netgen::multithread.terminate = 0;

    auto geometry = std::make_shared<netgen::OCCGeometry>(solid);
    auto mesh = std::make_shared<netgen::Mesh>();
    mesh->SetGeometry(geometry);

    netgen::MeshingParameters mp;
    mp.maxh = options.max_size;
    mp.minh = options.min_size;
    mp.grading = options.grading;
    mp.curvaturesafety = options.curvature_safety;
    mp.segmentsperedge = options.segments_per_edge;
    mp.optsteps3d = options.optimize_steps;
    mp.uselocalh = true;

    if (geometry->GenerateMesh(mesh, mp) != 0 || !mesh) {
        throw std::runtime_error("Netgen could not fill the solid with tetrahedra");
    }
    if (mesh->GetNE() == 0) {
        throw std::runtime_error("Netgen made no tetrahedra (is the shape a closed solid?)");
    }
    if (options.second_order) {
        // Puts every new edge node of a boundary edge on the face (or edge) it belongs to.
        geometry->GetRefinement().MakeSecondOrder(*mesh);
    }

    Result out;
    const int np = static_cast<int>(mesh->GetNP());
    out.nodes.reserve(static_cast<std::size_t>(np) * 3);
    for (int i = 1; i <= np; ++i) {
        const netgen::MeshPoint& p = mesh->Point(i);
        out.nodes.push_back(p(0));
        out.nodes.push_back(p(1));
        out.nodes.push_back(p(2));
    }

    out.nodes_per_tet = options.second_order ? 10 : 4;
    for (const netgen::Element& el : mesh->VolumeElements()) {
        if (el.IsDeleted()) {
            continue;
        }
        if (static_cast<std::uint32_t>(el.GetNP()) != out.nodes_per_tet) {
            throw std::runtime_error("Netgen made an element that is not a tetrahedron");
        }
        for (int j = 0; j < el.GetNP(); ++j) {
            out.tets.push_back(node_index(el[j]));
        }
    }

    out.faces.reserve(static_cast<std::size_t>(geometry->fmap.Extent()));
    for (int k = 1; k <= geometry->fmap.Extent(); ++k) {
        out.faces.push_back(geometry->fmap(k));
    }
    out.nodes_per_triangle = options.second_order ? 6 : 3;
    for (const netgen::Element2d& el : mesh->SurfaceElements()) {
        if (el.IsDeleted()) {
            continue;
        }
        if (static_cast<std::uint32_t>(el.GetNP()) != out.nodes_per_triangle) {
            throw std::runtime_error("Netgen made a boundary element that is not a triangle");
        }
        for (int j = 0; j < el.GetNP(); ++j) {
            out.triangles.push_back(node_index(el[j]));
        }
        // The face descriptor's surface number is the face's position in `fmap`, from 1.
        const int surface = mesh->GetFaceDescriptor(el.GetIndex()).SurfNr();
        if (surface < 1 || surface > geometry->fmap.Extent()) {
            throw std::runtime_error("Netgen gave a boundary triangle on no face");
        }
        out.triangle_faces.push_back(static_cast<std::uint32_t>(surface - 1));
    }
    return out;
}

std::string version() {
#ifdef NETGEN_VERSION
    return NETGEN_VERSION;
#else
    return "unknown";
#endif
}

} // namespace oxyde::occt::netgen_bridge
