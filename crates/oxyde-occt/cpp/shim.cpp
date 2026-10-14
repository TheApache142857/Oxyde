#include "oxyde-occt/cpp/shim.h"
#include "oxyde-occt/src/ffi.rs.h"

#include <BRepAdaptor_Curve.hxx>
#include <BRepAdaptor_Surface.hxx>
#include <BRepAlgoAPI_BooleanOperation.hxx>
#include <BRepAlgoAPI_Common.hxx>
#include <BRepAlgoAPI_Cut.hxx>
#include <BRepAlgoAPI_Fuse.hxx>
#include <BRepBuilderAPI_MakeEdge.hxx>
#include <BRepBuilderAPI_MakeFace.hxx>
#include <BRepBuilderAPI_MakeShape.hxx>
#include <BRepBuilderAPI_MakeWire.hxx>
#include <BRepFilletAPI_MakeChamfer.hxx>
#include <BRepFilletAPI_MakeFillet.hxx>
#include <BRepGProp.hxx>
#include <BRepPrimAPI_MakeBox.hxx>
#include <BRepPrimAPI_MakePrism.hxx>
#include <BRepTools.hxx>
#include <BRep_Builder.hxx>
#include <BRep_Tool.hxx>
#include <GC_MakeArcOfCircle.hxx>
#include <GProp_GProps.hxx>
#include <GeomAbs_CurveType.hxx>
#include <GeomAbs_SurfaceType.hxx>
#include <Geom_Curve.hxx>
#include <Geom_TrimmedCurve.hxx>
#include <IFSelect_ReturnStatus.hxx>
#include <STEPControl_Writer.hxx>
#include <Standard_Failure.hxx>
#include <Standard_Version.hxx>
#include <TopAbs_ShapeEnum.hxx>
#include <TopExp.hxx>
#include <TopExp_Explorer.hxx>
#include <TopTools_IndexedDataMapOfShapeListOfShape.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <TopTools_ListIteratorOfListOfShape.hxx>
#include <TopTools_ListOfShape.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Compound.hxx>
#include <TopoDS_Edge.hxx>
#include <TopoDS_Face.hxx>
#include <TopoDS_Vertex.hxx>
#include <TopoDS_Wire.hxx>
#include <gp_Ax2.hxx>
#include <gp_Ax3.hxx>
#include <gp_Circ.hxx>
#include <gp_Dir.hxx>
#include <gp_Pln.hxx>
#include <gp_Pnt.hxx>
#include <gp_Vec.hxx>

#include <algorithm>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

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

// ---- History helpers -----------------------------------------------------------------------

OpResultData empty_result(const TopTools_IndexedMapOfShape& result_faces) {
    OpResultData out{};
    for (int i = 1; i <= result_faces.Extent(); ++i) {
        out.faces.push_back(FaceHistoryData{});
    }
    return out;
}

// Records `kind/input/index` as a source of `face`, if `face` is a face of the result.
void add_source(OpResultData& out, const TopTools_IndexedMapOfShape& result_faces,
                const TopoDS_Shape& face, SourceKind kind, std::uint32_t input,
                std::uint32_t index) {
    if (face.IsNull() || face.ShapeType() != TopAbs_FACE) {
        return;
    }
    const int position = result_faces.FindIndex(face);
    if (position <= 0) {
        return;
    }
    FaceHistoryData& entry = out.faces[static_cast<std::size_t>(position - 1)];
    for (const FaceSourceData& existing : entry.sources) {
        if (existing.kind == kind && existing.input == input && existing.index == index) {
            return;
        }
    }
    FaceSourceData source{};
    source.kind = kind;
    source.input = input;
    source.index = index;
    entry.sources.push_back(source);
}

void add_sources(OpResultData& out, const TopTools_IndexedMapOfShape& result_faces,
                 const TopTools_ListOfShape& faces, SourceKind kind, std::uint32_t input,
                 std::uint32_t index) {
    for (TopTools_ListIteratorOfListOfShape it(faces); it.More(); it.Next()) {
        add_source(out, result_faces, it.Value(), kind, input, index);
    }
}

// Faces of `input` that survive into the result, unchanged or modified.
void record_kept_faces(BRepBuilderAPI_MakeShape& algo, const TopoDS_Shape& input,
                       std::uint32_t input_index,
                       const TopTools_IndexedMapOfShape& result_faces, OpResultData& out) {
    TopTools_IndexedMapOfShape faces;
    TopExp::MapShapes(input, TopAbs_FACE, faces);
    for (int j = 1; j <= faces.Extent(); ++j) {
        const TopoDS_Shape& face = faces(j);
        const auto index = static_cast<std::uint32_t>(j - 1);
        if (algo.IsDeleted(face)) {
            continue;
        }
        const TopTools_ListOfShape& modified = algo.Modified(face);
        if (modified.IsEmpty()) {
            add_source(out, result_faces, face, SourceKind::InputFace, input_index, index);
        } else {
            add_sources(out, result_faces, modified, SourceKind::InputFace, input_index, index);
        }
    }
}

// Faces generated from sub-shapes of type `type` of `input` (fillet faces from edges, ...).
void record_generated_faces(BRepBuilderAPI_MakeShape& algo, const TopoDS_Shape& input,
                            std::uint32_t input_index, TopAbs_ShapeEnum type,
                            SourceKind kind, const TopTools_IndexedMapOfShape& result_faces,
                            OpResultData& out) {
    TopTools_IndexedMapOfShape subshapes;
    TopExp::MapShapes(input, type, subshapes);
    for (int j = 1; j <= subshapes.Extent(); ++j) {
        add_sources(out, result_faces, algo.Generated(subshapes(j)), kind, input_index,
                    static_cast<std::uint32_t>(j - 1));
    }
}

// ---- Profile construction ------------------------------------------------------------------

struct Frame {
    gp_Ax3 axes;
    gp_Pnt origin;
    gp_Vec x;
    gp_Vec y;

    explicit Frame(const PlaneData& p)
        : axes(gp_Pnt(p.ox, p.oy, p.oz), gp_Dir(p.nx, p.ny, p.nz), gp_Dir(p.xx, p.xy, p.xz)),
          origin(axes.Location()), x(axes.XDirection()), y(axes.YDirection()) {}

    gp_Pnt at(double u, double v) const { return origin.Translated(x * u + y * v); }
};

TopoDS_Edge make_edge(const SegmentData& seg, const Frame& frame) {
    switch (seg.kind) {
    case SegmentKind::Line: {
        BRepBuilderAPI_MakeEdge maker(frame.at(seg.x0, seg.y0), frame.at(seg.x1, seg.y1));
        if (maker.IsDone()) {
            return maker.Edge();
        }
        break;
    }
    case SegmentKind::Arc: {
        GC_MakeArcOfCircle arc(frame.at(seg.x0, seg.y0), frame.at(seg.x1, seg.y1),
                               frame.at(seg.x2, seg.y2));
        if (arc.IsDone()) {
            const Handle(Geom_Curve) curve = arc.Value();
            BRepBuilderAPI_MakeEdge maker(curve);
            if (maker.IsDone()) {
                return maker.Edge();
            }
        }
        break;
    }
    case SegmentKind::Circle: {
        const gp_Circ circle(
            gp_Ax2(frame.at(seg.x0, seg.y0), frame.axes.Direction(), frame.axes.XDirection()),
            seg.r);
        BRepBuilderAPI_MakeEdge maker(circle);
        if (maker.IsDone()) {
            return maker.Edge();
        }
        break;
    }
    default:
        break;
    }
    throw std::runtime_error("cannot build profile segment " + std::to_string(seg.id));
}

gp_Pnt edge_midpoint(const TopoDS_Edge& edge) {
    const BRepAdaptor_Curve curve(edge);
    return curve.Value(0.5 * (curve.FirstParameter() + curve.LastParameter()));
}

struct SegmentMark {
    std::uint32_t id;
    gp_Pnt midpoint;
};

TopoDS_Wire make_wire(const LoopData& loop, const Frame& frame,
                      std::vector<SegmentMark>& marks) {
    BRepBuilderAPI_MakeWire wire;
    for (const SegmentData& seg : loop.segments) {
        const TopoDS_Edge edge = make_edge(seg, frame);
        marks.push_back(SegmentMark{seg.id, edge_midpoint(edge)});
        wire.Add(edge);
        if (!wire.IsDone()) {
            throw std::runtime_error("profile segment " + std::to_string(seg.id) +
                                     " does not connect to the previous one");
        }
    }
    return wire.Wire();
}

// Wire building may copy edges, so segments are matched back to edges geometrically.
bool find_segment(const TopoDS_Edge& edge, const std::vector<SegmentMark>& marks,
                  std::uint32_t& id) {
    const gp_Pnt mid = edge_midpoint(edge);
    double best = 1e300;
    for (const SegmentMark& mark : marks) {
        const double d = mid.Distance(mark.midpoint);
        if (d < best) {
            best = d;
            id = mark.id;
        }
    }
    return best < 1e-4;
}

TopoDS_Face make_region_face(const RegionData& region, const Frame& frame,
                             std::vector<SegmentMark>& marks) {
    if (region.loops.empty()) {
        throw std::invalid_argument("profile region without boundary");
    }
    const gp_Pln plane(frame.axes);
    // Inside = true orients the outer wire so the face is the bounded side.
    BRepBuilderAPI_MakeFace face(plane, make_wire(region.loops[0], frame, marks),
                                 Standard_True);
    if (!face.IsDone()) {
        throw std::runtime_error("cannot build a face from the profile's outer boundary");
    }
    for (std::size_t i = 1; i < region.loops.size(); ++i) {
        // Orient the hole like a bounded face, then reverse it.
        BRepBuilderAPI_MakeFace hole(plane, make_wire(region.loops[i], frame, marks),
                                     Standard_True);
        if (!hole.IsDone()) {
            throw std::runtime_error("cannot build a hole of the profile");
        }
        const TopoDS_Wire hole_wire = BRepTools::OuterWire(hole.Face());
        face.Add(TopoDS::Wire(hole_wire.Reversed()));
    }
    return face.Face();
}

// ---- Topology helpers ----------------------------------------------------------------------

SurfaceKind surface_kind(GeomAbs_SurfaceType type) {
    switch (type) {
    case GeomAbs_Plane:
        return SurfaceKind::Plane;
    case GeomAbs_Cylinder:
        return SurfaceKind::Cylinder;
    case GeomAbs_Cone:
        return SurfaceKind::Cone;
    case GeomAbs_Sphere:
        return SurfaceKind::Sphere;
    case GeomAbs_Torus:
        return SurfaceKind::Torus;
    case GeomAbs_BezierSurface:
    case GeomAbs_BSplineSurface:
        return SurfaceKind::BSpline;
    default:
        return SurfaceKind::Other;
    }
}

CurveKind curve_kind(GeomAbs_CurveType type) {
    switch (type) {
    case GeomAbs_Line:
        return CurveKind::Line;
    case GeomAbs_Circle:
        return CurveKind::Circle;
    case GeomAbs_Ellipse:
        return CurveKind::Ellipse;
    case GeomAbs_BezierCurve:
    case GeomAbs_BSplineCurve:
        return CurveKind::BSpline;
    default:
        return CurveKind::Other;
    }
}

void push_unique(rust::Vec<std::uint32_t>& values, std::uint32_t value) {
    if (std::find(values.begin(), values.end(), value) == values.end()) {
        values.push_back(value);
    }
}

const TopoDS_Edge& edge_at(const TopTools_IndexedMapOfShape& edges, std::uint32_t index) {
    if (index >= static_cast<std::uint32_t>(edges.Extent())) {
        throw std::out_of_range("edge index " + std::to_string(index) + " out of range");
    }
    return TopoDS::Edge(edges(static_cast<int>(index) + 1));
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

OpResultData Session::extrude(const PlaneData& plane, rust::Slice<const RegionData> regions,
                              double dx, double dy, double dz) {
    return guarded("extrude", [&] {
        if (regions.empty()) {
            throw std::invalid_argument("profile has no closed region");
        }
        const Frame frame(plane);
        const gp_Vec direction(dx, dy, dz);
        if (direction.Magnitude() <= 1e-9) {
            throw std::invalid_argument("extrusion length is zero");
        }

        std::vector<SegmentMark> marks;
        std::vector<TopoDS_Face> faces;
        for (const RegionData& region : regions) {
            faces.push_back(make_region_face(region, frame, marks));
        }

        TopoDS_Shape base;
        if (faces.size() == 1) {
            base = faces[0];
        } else {
            BRep_Builder builder;
            TopoDS_Compound compound;
            builder.MakeCompound(compound);
            for (const TopoDS_Face& face : faces) {
                builder.Add(compound, face);
            }
            base = compound;
        }
        // Profile faces point along the plane normal; sweep along the face normal so the
        // solid is never built inside out.
        if (direction.Dot(gp_Vec(frame.axes.Direction())) < 0.0) {
            base.Reverse();
        }

        BRepPrimAPI_MakePrism prism(base, direction);
        prism.Build();
        if (!prism.IsDone()) {
            throw std::runtime_error("BRepPrimAPI_MakePrism did not complete");
        }
        const TopoDS_Shape result = prism.Shape();

        TopTools_IndexedMapOfShape result_faces;
        TopExp::MapShapes(result, TopAbs_FACE, result_faces);
        OpResultData out = empty_result(result_faces);

        TopTools_IndexedMapOfShape base_edges;
        TopExp::MapShapes(base, TopAbs_EDGE, base_edges);
        for (int i = 1; i <= base_edges.Extent(); ++i) {
            std::uint32_t id = 0;
            if (find_segment(TopoDS::Edge(base_edges(i)), marks, id)) {
                add_sources(out, result_faces, prism.Generated(base_edges(i)),
                            SourceKind::ProfileSegment, 0, id);
            }
        }
        const TopoDS_Shape start_cap = prism.FirstShape();
        const TopoDS_Shape end_cap = prism.LastShape();
        for (TopExp_Explorer it(start_cap, TopAbs_FACE); it.More(); it.Next()) {
            add_source(out, result_faces, it.Current(), SourceKind::StartCap, 0, 0);
        }
        for (TopExp_Explorer it(end_cap, TopAbs_FACE); it.More(); it.Next()) {
            add_source(out, result_faces, it.Current(), SourceKind::EndCap, 0, 0);
        }

        out.shape = insert(result);
        return out;
    });
}

OpResultData Session::boolean_op(std::uint8_t kind, std::uint64_t target, std::uint64_t tool) {
    return guarded("boolean", [&] {
        const TopoDS_Shape& a = get(target);
        const TopoDS_Shape& b = get(tool);

        std::unique_ptr<BRepAlgoAPI_BooleanOperation> op;
        switch (kind) {
        case 0:
            op = std::make_unique<BRepAlgoAPI_Fuse>(a, b);
            break;
        case 1:
            op = std::make_unique<BRepAlgoAPI_Cut>(a, b);
            break;
        case 2:
            op = std::make_unique<BRepAlgoAPI_Common>(a, b);
            break;
        default:
            throw std::invalid_argument("unknown boolean kind " + std::to_string(kind));
        }
        if (!op->IsDone() || op->HasErrors()) {
            throw std::runtime_error("boolean operation failed");
        }
        // Merge coplanar faces and collinear edges left by the operation; history is kept.
        op->SimplifyResult();
        const TopoDS_Shape result = op->Shape();

        TopTools_IndexedMapOfShape result_faces;
        TopExp::MapShapes(result, TopAbs_FACE, result_faces);
        OpResultData out = empty_result(result_faces);
        record_kept_faces(*op, a, 0, result_faces, out);
        record_kept_faces(*op, b, 1, result_faces, out);

        out.shape = insert(result);
        return out;
    });
}

OpResultData Session::fillet(std::uint64_t shape, rust::Slice<const std::uint32_t> edges,
                             double radius) {
    return guarded("fillet", [&] {
        if (edges.empty()) {
            throw std::invalid_argument("no edges to fillet");
        }
        const TopoDS_Shape& input = get(shape);
        TopTools_IndexedMapOfShape input_edges;
        TopExp::MapShapes(input, TopAbs_EDGE, input_edges);

        BRepFilletAPI_MakeFillet maker(input);
        for (const std::uint32_t index : edges) {
            maker.Add(radius, edge_at(input_edges, index));
        }
        maker.Build();
        if (!maker.IsDone()) {
            throw std::runtime_error("fillet failed (radius too large for the selected edges?)");
        }
        const TopoDS_Shape result = maker.Shape();

        TopTools_IndexedMapOfShape result_faces;
        TopExp::MapShapes(result, TopAbs_FACE, result_faces);
        OpResultData out = empty_result(result_faces);
        record_kept_faces(maker, input, 0, result_faces, out);
        record_generated_faces(maker, input, 0, TopAbs_EDGE, SourceKind::InputEdge,
                               result_faces, out);
        record_generated_faces(maker, input, 0, TopAbs_VERTEX, SourceKind::InputVertex,
                               result_faces, out);

        out.shape = insert(result);
        return out;
    });
}

OpResultData Session::chamfer(std::uint64_t shape, rust::Slice<const std::uint32_t> edges,
                              double distance) {
    return guarded("chamfer", [&] {
        if (edges.empty()) {
            throw std::invalid_argument("no edges to chamfer");
        }
        const TopoDS_Shape& input = get(shape);
        TopTools_IndexedMapOfShape input_edges;
        TopExp::MapShapes(input, TopAbs_EDGE, input_edges);

        BRepFilletAPI_MakeChamfer maker(input);
        for (const std::uint32_t index : edges) {
            maker.Add(distance, edge_at(input_edges, index));
        }
        maker.Build();
        if (!maker.IsDone()) {
            throw std::runtime_error(
                "chamfer failed (distance too large for the selected edges?)");
        }
        const TopoDS_Shape result = maker.Shape();

        TopTools_IndexedMapOfShape result_faces;
        TopExp::MapShapes(result, TopAbs_FACE, result_faces);
        OpResultData out = empty_result(result_faces);
        record_kept_faces(maker, input, 0, result_faces, out);
        record_generated_faces(maker, input, 0, TopAbs_EDGE, SourceKind::InputEdge,
                               result_faces, out);
        record_generated_faces(maker, input, 0, TopAbs_VERTEX, SourceKind::InputVertex,
                               result_faces, out);

        out.shape = insert(result);
        return out;
    });
}

void Session::release(std::uint64_t shape) {
    shapes_.erase(shape);
}

TopologyData Session::topology(std::uint64_t shape) const {
    return guarded("topology", [&] {
        const TopoDS_Shape& s = get(shape);
        TopTools_IndexedMapOfShape faces;
        TopTools_IndexedMapOfShape edges;
        TopTools_IndexedMapOfShape vertices;
        TopExp::MapShapes(s, TopAbs_FACE, faces);
        TopExp::MapShapes(s, TopAbs_EDGE, edges);
        TopExp::MapShapes(s, TopAbs_VERTEX, vertices);
        TopTools_IndexedDataMapOfShapeListOfShape edge_faces;
        TopExp::MapShapesAndAncestors(s, TopAbs_EDGE, TopAbs_FACE, edge_faces);

        TopologyData topo{};

        for (int i = 1; i <= faces.Extent(); ++i) {
            const TopoDS_Face& face = TopoDS::Face(faces(i));
            FaceData data{};
            const BRepAdaptor_Surface surface(face);
            data.surface = surface_kind(surface.GetType());

            GProp_GProps props;
            BRepGProp::SurfaceProperties(face, props);
            const gp_Pnt centroid = props.CentreOfMass();
            data.area = props.Mass();
            data.cx = centroid.X();
            data.cy = centroid.Y();
            data.cz = centroid.Z();

            if (surface.GetType() == GeomAbs_Plane) {
                gp_Dir normal = surface.Plane().Axis().Direction();
                if (face.Orientation() == TopAbs_REVERSED) {
                    normal.Reverse();
                }
                data.has_normal = true;
                data.nx = normal.X();
                data.ny = normal.Y();
                data.nz = normal.Z();
            }

            TopTools_IndexedMapOfShape face_edges;
            TopExp::MapShapes(face, TopAbs_EDGE, face_edges);
            for (int j = 1; j <= face_edges.Extent(); ++j) {
                const int index = edges.FindIndex(face_edges(j));
                if (index > 0) {
                    push_unique(data.edges, static_cast<std::uint32_t>(index - 1));
                }
            }
            topo.faces.push_back(std::move(data));
        }

        for (int i = 1; i <= edges.Extent(); ++i) {
            const TopoDS_Edge& edge = TopoDS::Edge(edges(i));
            EdgeData data{};

            TopoDS_Vertex first;
            TopoDS_Vertex last;
            TopExp::Vertices(edge, first, last);
            for (const TopoDS_Vertex& v : {first, last}) {
                if (!v.IsNull()) {
                    const int index = vertices.FindIndex(v);
                    if (index > 0) {
                        push_unique(data.vertices, static_cast<std::uint32_t>(index - 1));
                    }
                }
            }

            gp_Pnt mid;
            if (BRep_Tool::Degenerated(edge)) {
                data.curve = CurveKind::Degenerate;
                data.length = 0.0;
                if (!first.IsNull()) {
                    mid = BRep_Tool::Pnt(first);
                }
            } else {
                const BRepAdaptor_Curve curve(edge);
                data.curve = curve_kind(curve.GetType());
                GProp_GProps props;
                BRepGProp::LinearProperties(edge, props);
                data.length = props.Mass();
                mid = curve.Value(0.5 * (curve.FirstParameter() + curve.LastParameter()));
            }
            data.mx = mid.X();
            data.my = mid.Y();
            data.mz = mid.Z();

            if (edge_faces.Contains(edge)) {
                const TopTools_ListOfShape& adjacent = edge_faces.FindFromKey(edge);
                for (TopTools_ListIteratorOfListOfShape it(adjacent); it.More(); it.Next()) {
                    const int index = faces.FindIndex(it.Value());
                    if (index > 0) {
                        push_unique(data.faces, static_cast<std::uint32_t>(index - 1));
                    }
                }
            }
            topo.edges.push_back(std::move(data));
        }

        for (int i = 1; i <= vertices.Extent(); ++i) {
            const gp_Pnt p = BRep_Tool::Pnt(TopoDS::Vertex(vertices(i)));
            VertexData data{};
            data.x = p.X();
            data.y = p.Y();
            data.z = p.Z();
            topo.vertices.push_back(data);
        }
        return topo;
    });
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
