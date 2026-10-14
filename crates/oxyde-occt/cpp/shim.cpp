#include "oxyde-occt/cpp/shim.h"
#include "oxyde-occt/src/ffi.rs.h"

#include <BRepGProp.hxx>
#include <BRepPrimAPI_MakeBox.hxx>
#include <GProp_GProps.hxx>
#include <IFSelect_ReturnStatus.hxx>
#include <STEPControl_Writer.hxx>
#include <Standard_Failure.hxx>
#include <Standard_Version.hxx>
#include <TopAbs_ShapeEnum.hxx>
#include <TopExp.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <gp_Pnt.hxx>

#include <stdexcept>
#include <string>

namespace oxyde::occt {

namespace {

// Runs `body`, converting every exception into std::runtime_error prefixed with `op`.
// OCCT's Standard_Failure does not derive from std::exception, so cxx would not catch it.
template <typename F>
auto guarded(const char* op, F&& body) -> decltype(body()) {
    try {
        return body();
    } catch (const Standard_Failure& e) {
        std::string message = std::string(op) + ": " + e.DynamicType()->Name();
        const char* detail = e.GetMessageString();
        if (detail != nullptr && *detail != '\0') {
            message += ": ";
            message += detail;
        }
        throw std::runtime_error(message);
    } catch (const std::exception& e) {
        throw std::runtime_error(std::string(op) + ": " + e.what());
    } catch (...) {
        throw std::runtime_error(std::string(op) + ": unknown C++ exception");
    }
}

std::uint32_t count_subshapes(const TopoDS_Shape& shape, TopAbs_ShapeEnum kind) {
    TopTools_IndexedMapOfShape map;
    TopExp::MapShapes(shape, kind, map);
    return static_cast<std::uint32_t>(map.Extent());
}

} // namespace

std::uint64_t Session::insert(const TopoDS_Shape& shape) {
    const std::uint64_t id = next_id_++;
    shapes_.emplace(id, shape);
    return id;
}

const TopoDS_Shape& Session::get(std::uint64_t shape) const {
    const auto it = shapes_.find(shape);
    if (it == shapes_.end()) {
        throw std::out_of_range("unknown shape handle " + std::to_string(shape));
    }
    return it->second;
}

std::uint64_t Session::make_box(double x, double y, double z, double dx, double dy, double dz) {
    return guarded("make_box", [&] {
        BRepPrimAPI_MakeBox maker(gp_Pnt(x, y, z), dx, dy, dz);
        maker.Build();
        if (!maker.IsDone()) {
            throw std::runtime_error("BRepPrimAPI_MakeBox did not complete");
        }
        return insert(maker.Solid());
    });
}

void Session::release(std::uint64_t shape) {
    shapes_.erase(shape);
}

double Session::volume(std::uint64_t shape) const {
    return guarded("volume", [&] {
        GProp_GProps props;
        BRepGProp::VolumeProperties(get(shape), props);
        return props.Mass();
    });
}

TopologyCounts Session::topology_counts(std::uint64_t shape) const {
    return guarded("topology_counts", [&] {
        const TopoDS_Shape& s = get(shape);
        TopologyCounts counts{};
        counts.solids = count_subshapes(s, TopAbs_SOLID);
        counts.shells = count_subshapes(s, TopAbs_SHELL);
        counts.faces = count_subshapes(s, TopAbs_FACE);
        counts.edges = count_subshapes(s, TopAbs_EDGE);
        counts.vertices = count_subshapes(s, TopAbs_VERTEX);
        return counts;
    });
}

void Session::export_step(rust::Slice<const std::uint64_t> shapes, rust::Str path) const {
    guarded("export_step", [&] {
        if (shapes.empty()) {
            throw std::invalid_argument("no shapes to export");
        }
        STEPControl_Writer writer;
        for (const std::uint64_t id : shapes) {
            if (writer.Transfer(get(id), STEPControl_AsIs) != IFSelect_RetDone) {
                throw std::runtime_error("transfer of shape " + std::to_string(id) + " failed");
            }
        }
        // OCCT interprets file names as UTF-8 on Windows.
        const std::string file(path);
        if (writer.Write(file.c_str()) != IFSelect_RetDone) {
            throw std::runtime_error("could not write " + file);
        }
    });
}

std::unique_ptr<Session> new_session() {
    return std::make_unique<Session>();
}

rust::String occt_version() {
    return rust::String(OCC_VERSION_COMPLETE);
}

} // namespace oxyde::occt
