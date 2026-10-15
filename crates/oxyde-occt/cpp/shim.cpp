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
#include <BRepBuilderAPI_MakeVertex.hxx>
#include <BRepExtrema_DistShapeShape.hxx>
#include <BRepBuilderAPI_MakeWire.hxx>
#include <BRepBuilderAPI_Transform.hxx>
#include <BRepFilletAPI_MakeChamfer.hxx>
#include <BRepFilletAPI_MakeFillet.hxx>
#include <BRepGProp.hxx>
#include <BRepMesh_IncrementalMesh.hxx>
#include <BRepOffsetAPI_MakeThickSolid.hxx>
#include <BRepOffset_Mode.hxx>
#include <BRepPrimAPI_MakeBox.hxx>
#include <BRepPrimAPI_MakePrism.hxx>
#include <BRepPrimAPI_MakeRevol.hxx>
#include <BRepTools.hxx>
#include <BRep_Builder.hxx>
#include <BRep_Tool.hxx>
#include <GC_MakeArcOfCircle.hxx>
#include <GCPnts_TangentialDeflection.hxx>
#include <GProp_GProps.hxx>
#include <GeomAbs_CurveType.hxx>
#include <GeomAbs_JoinType.hxx>
#include <GeomAbs_SurfaceType.hxx>
#include <Geom_Curve.hxx>
#include <Geom_TrimmedCurve.hxx>
#include <IFSelect_ReturnStatus.hxx>
#include <Poly_Triangle.hxx>
#include <Poly_Triangulation.hxx>
#include <STEPControl_Reader.hxx>
#include <STEPControl_Writer.hxx>
#include <STEPCAFControl_Reader.hxx>
#include <TCollection_AsciiString.hxx>
#include <TCollection_ExtendedString.hxx>
#include <TDF_Label.hxx>
#include <TDF_LabelSequence.hxx>
#include <TDF_Tool.hxx>
#include <TDataStd_Name.hxx>
#include <TDocStd_Document.hxx>
#include <XCAFApp_Application.hxx>
#include <XCAFDoc_DocumentTool.hxx>
#include <XCAFDoc_ShapeTool.hxx>
#include <BRepBuilderAPI_MakeSolid.hxx>
#include <BRepBuilderAPI_Sewing.hxx>
#include <ShapeFix_Shape.hxx>
#include <TopoDS_Shell.hxx>
#include <sstream>
#include <Standard_Failure.hxx>
#include <Standard_Version.hxx>
#include <TopAbs_ShapeEnum.hxx>
#include <TopLoc_Location.hxx>
#include <TopExp.hxx>
#include <TopExp_Explorer.hxx>
#include <TopTools_IndexedDataMapOfShapeListOfShape.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <TopTools_ListOfShape.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Compound.hxx>
#include <TopoDS_Edge.hxx>
#include <TopoDS_Face.hxx>
#include <TopoDS_Vertex.hxx>
#include <TopoDS_Wire.hxx>
#include <gp_Ax1.hxx>
#include <gp_Ax2.hxx>
#include <gp_Ax3.hxx>
#include <gp_Circ.hxx>
#include <gp_Cone.hxx>
#include <gp_Cylinder.hxx>
#include <gp_Dir.hxx>
#include <gp_Lin.hxx>
#include <gp_Pln.hxx>
#include <gp_Pnt.hxx>
#include <gp_Sphere.hxx>
#include <gp_Torus.hxx>
#include <gp_Trsf.hxx>
#include <gp_Vec.hxx>

#include <algorithm>
#include <cmath>
#include <initializer_list>
#include <stdexcept>
#include <map>
#include <mutex>
#include <string>
#include <utility>
#include <vector>

namespace oxyde::occt {

namespace {

// Runs `body`, converting every exception into std::runtime_error prefixed with `op`.
// Before OCCT 8, Standard_Failure did not derive from std::exception, so cxx would not catch it.
template <typename F>
auto guarded(const char* op, F&& body) -> decltype(body()) {
    try {
        return body();
    } catch (const Standard_Failure& e) {
#if OCC_VERSION_HEX >= 0x080000
        // OCCT 8: exceptions derive from std::exception and are no longer Standard_Transient.
        std::string message = std::string(op) + ": " + e.ExceptionType();
        const char* detail = e.what();
#else
        std::string message = std::string(op) + ": " + e.DynamicType()->Name();
        const char* detail = e.GetMessageString();
#endif
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

// ---- STEP import helpers -------------------------------------------------------------------

// OCCT's STEP translators and the XCAF application keep global state: two sessions reading
// or writing STEP at the same time can crash. Every STEP read and write holds this lock.
std::mutex& step_mutex() {
    static std::mutex mutex;
    return mutex;
}

// The solids of `all` as one shape (a compound when there are several), repaired and
// facing outward. Sewn surfaces count when they close a volume. Null when there is none.
TopoDS_Shape solid_body(const TopoDS_Shape& all) {
    TopTools_IndexedMapOfShape solids;
    TopExp::MapShapes(all, TopAbs_SOLID, solids);
    if (solids.Extent() == 0) {
        // Surfaces only: sew them, and keep the closed shells as solids.
        BRepBuilderAPI_Sewing sewing(1.0e-3);
        sewing.Add(all);
        sewing.Perform();
        TopTools_IndexedMapOfShape shells;
        TopExp::MapShapes(sewing.SewedShape(), TopAbs_SHELL, shells);
        for (int i = 1; i <= shells.Extent(); ++i) {
            const TopoDS_Shell& shell = TopoDS::Shell(shells(i));
            if (BRep_Tool::IsClosed(shell)) {
                BRepBuilderAPI_MakeSolid solid(shell);
                if (solid.IsDone()) {
                    solids.Add(solid.Solid());
                }
            }
        }
    }
    if (solids.Extent() == 0) {
        return TopoDS_Shape();
    }

    TopoDS_Shape result;
    if (solids.Extent() == 1) {
        result = solids(1);
    } else {
        TopoDS_Compound compound;
        BRep_Builder builder;
        builder.MakeCompound(compound);
        for (int i = 1; i <= solids.Extent(); ++i) {
            builder.Add(compound, solids(i));
        }
        result = compound;
    }
    ShapeFix_Shape fix(result);
    fix.Perform();
    result = fix.Shape();
    // Some exporters write solids inside out.
    GProp_GProps props;
    BRepGProp::VolumeProperties(result, props);
    if (props.Mass() < 0.0) {
        result.Reverse();
    }
    return result;
}

// `shape` written as a STEP file, in millimetres.
rust::Vec<std::uint8_t> step_bytes(const TopoDS_Shape& shape) {
    STEPControl_Writer writer;
    if (writer.Transfer(shape, STEPControl_AsIs) != IFSelect_RetDone) {
        throw std::runtime_error("transfer of a part failed");
    }
    std::ostringstream stream;
    if (writer.WriteStream(stream) != IFSelect_RetDone) {
        throw std::runtime_error("could not write a part");
    }
    const std::string text = stream.str();
    rust::Vec<std::uint8_t> out;
    out.reserve(text.size());
    for (const char c : text) {
        out.push_back(static_cast<std::uint8_t>(c));
    }
    return out;
}

// The name attribute of an XCAF label, in UTF-8; empty when it has none.
std::string label_name(const TDF_Label& label) {
    Handle(TDataStd_Name) name;
    if (!label.FindAttribute(TDataStd_Name::GetID(), name)) {
        return std::string();
    }
    const TCollection_ExtendedString& text = name->Get();
    std::string out(static_cast<std::size_t>(text.LengthOfCString()) + 1, '\0');
    Standard_PCharacter buffer = &out[0];
    const int length = text.ToUTF8CString(buffer);
    out.resize(static_cast<std::size_t>(std::max(length, 0)));
    // OCCT names unnamed instances after the label they refer to: "=>[0:1:1:4]".
    if (out.rfind("=>[", 0) == 0) {
        return std::string();
    }
    return out;
}

StepInstanceData instance_data(std::uint32_t node, const std::string& name, const gp_Trsf& trsf) {
    StepInstanceData out{};
    out.node = node;
    out.name = rust::String(name);
    for (int row = 1; row <= 3; ++row) {
        for (int col = 1; col <= 4; ++col) {
            out.rows.push_back(trsf.Value(row, col));
        }
    }
    return out;
}

// Walks an XCAF document, adding each part and assembly to `out` once, children first.
class StepTreeBuilder {
public:
    explicit StepTreeBuilder(StepAssemblyData& out) : out_(out) {}

    // Index of the node for a shape definition label.
    std::uint32_t node(const TDF_Label& label) {
        TCollection_AsciiString entry;
        TDF_Tool::Entry(label, entry);
        const std::string key(entry.ToCString());
        const auto found = done_.find(key);
        if (found != done_.end()) {
            return found->second;
        }
        std::uint32_t index;
        if (XCAFDoc_ShapeTool::IsAssembly(label)) {
            StepNodeData node{};
            node.name = rust::String(label_name(label));
            node.is_assembly = true;
            TDF_LabelSequence components;
            XCAFDoc_ShapeTool::GetComponents(label, components, false);
            for (int i = 1; i <= components.Length(); ++i) {
                const TDF_Label& component = components.Value(i);
                TDF_Label referred;
                if (!XCAFDoc_ShapeTool::GetReferredShape(component, referred)) {
                    continue;
                }
                const gp_Trsf trsf = XCAFDoc_ShapeTool::GetLocation(component).Transformation();
                const bool rigid = !trsf.IsNegative() && std::abs(trsf.ScaleFactor() - 1.0) < 1e-9;
                if (rigid) {
                    const std::uint32_t child = this->node(referred);
                    node.instances.push_back(instance_data(child, label_name(component), trsf));
                } else {
                    // Mirrored or scaled: a part of its own, with the motion applied.
                    const TopoDS_Shape moved =
                        BRepBuilderAPI_Transform(XCAFDoc_ShapeTool::GetShape(referred), trsf, true)
                            .Shape();
                    const std::uint32_t child =
                        part(label_name(referred) + " (mirrored)", moved);
                    node.instances.push_back(instance_data(child, label_name(component), gp_Trsf()));
                }
            }
            index = static_cast<std::uint32_t>(out_.nodes.size());
            out_.nodes.push_back(std::move(node));
        } else {
            index = part(label_name(label), XCAFDoc_ShapeTool::GetShape(label));
        }
        done_.emplace(key, index);
        return index;
    }

private:
    std::uint32_t part(const std::string& name, const TopoDS_Shape& shape) {
        StepNodeData node{};
        node.name = rust::String(name);
        node.is_assembly = false;
        if (!shape.IsNull()) {
            const TopoDS_Shape body = solid_body(shape);
            if (!body.IsNull()) {
                node.step = step_bytes(body);
            }
        }
        out_.nodes.push_back(std::move(node));
        return static_cast<std::uint32_t>(out_.nodes.size() - 1);
    }

    StepAssemblyData& out_;
    std::map<std::string, std::uint32_t> done_;
};

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
    for (TopTools_ListOfShape::Iterator it(faces); it.More(); it.Next()) {
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

// The profile as one face per region (a compound when there are several), lying on the
// plane and facing along its normal.
TopoDS_Shape make_profile_base(rust::Slice<const RegionData> regions, const Frame& frame,
                               std::vector<SegmentMark>& marks) {
    if (regions.empty()) {
        throw std::invalid_argument("profile has no closed region");
    }
    std::vector<TopoDS_Face> faces;
    for (const RegionData& region : regions) {
        faces.push_back(make_region_face(region, frame, marks));
    }
    if (faces.size() == 1) {
        return faces[0];
    }
    BRep_Builder builder;
    TopoDS_Compound compound;
    builder.MakeCompound(compound);
    for (const TopoDS_Face& face : faces) {
        builder.Add(compound, face);
    }
    return compound;
}

// Side faces swept from the profile's edges, named by segment id.
void record_profile_faces(BRepBuilderAPI_MakeShape& sweep, const TopoDS_Shape& base,
                          const std::vector<SegmentMark>& marks,
                          const TopTools_IndexedMapOfShape& result_faces, OpResultData& out) {
    TopTools_IndexedMapOfShape base_edges;
    TopExp::MapShapes(base, TopAbs_EDGE, base_edges);
    for (int i = 1; i <= base_edges.Extent(); ++i) {
        std::uint32_t id = 0;
        if (find_segment(TopoDS::Edge(base_edges(i)), marks, id)) {
            add_sources(out, result_faces, sweep.Generated(base_edges(i)),
                        SourceKind::ProfileSegment, 0, id);
        }
    }
}

// Faces the sweep's history left without a source, matched to the profile edge lying on them.
// OCCT 8 no longer reports the planar faces of a revolve through Generated(); a revolved
// face still contains its profile edge (at angle zero), so the edge's middle lies on it.
void record_unnamed_profile_faces(const TopoDS_Shape& base,
                                  const std::vector<SegmentMark>& marks,
                                  const TopTools_IndexedMapOfShape& result_faces,
                                  OpResultData& out) {
    TopTools_IndexedMapOfShape base_edges;
    TopExp::MapShapes(base, TopAbs_EDGE, base_edges);
    for (int f = 1; f <= result_faces.Extent(); ++f) {
        if (!out.faces[static_cast<std::size_t>(f - 1)].sources.empty()) {
            continue;
        }
        const TopoDS_Face& face = TopoDS::Face(result_faces(f));
        for (int i = 1; i <= base_edges.Extent(); ++i) {
            const TopoDS_Edge& edge = TopoDS::Edge(base_edges(i));
            std::uint32_t id = 0;
            if (!find_segment(edge, marks, id)) {
                continue;
            }
            const BRepAdaptor_Curve curve(edge);
            const gp_Pnt middle =
                curve.Value((curve.FirstParameter() + curve.LastParameter()) / 2.0);
            BRepExtrema_DistShapeShape distance(BRepBuilderAPI_MakeVertex(middle).Vertex(), face);
            if (distance.IsDone() && distance.Value() < 1e-6) {
                add_source(out, result_faces, face, SourceKind::ProfileSegment, 0, id);
                break;
            }
        }
    }
}

// ---- Topology helpers ----------------------------------------------------------------------

AxisData axis_data(const gp_Ax1& axis) {
    AxisData data{};
    data.ox = axis.Location().X();
    data.oy = axis.Location().Y();
    data.oz = axis.Location().Z();
    data.dx = axis.Direction().X();
    data.dy = axis.Direction().Y();
    data.dz = axis.Direction().Z();
    return data;
}

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
        const Frame frame(plane);
        const gp_Vec direction(dx, dy, dz);
        if (direction.Magnitude() <= 1e-9) {
            throw std::invalid_argument("extrusion length is zero");
        }

        std::vector<SegmentMark> marks;
        TopoDS_Shape base = make_profile_base(regions, frame, marks);
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

        record_profile_faces(prism, base, marks, result_faces, out);
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

OpResultData Session::revolve(const PlaneData& plane, rust::Slice<const RegionData> regions,
                              const AxisData& axis, double angle) {
    return guarded("revolve", [&] {
        const double full_turn = 6.283185307179586;
        if (!(angle > 1e-9) || angle > full_turn + 1e-9) {
            throw std::invalid_argument("the angle must be more than 0 and at most 360 degrees");
        }
        const bool full = angle >= full_turn - 1e-9;
        const Frame frame(plane);
        std::vector<SegmentMark> marks;
        const TopoDS_Shape base = make_profile_base(regions, frame, marks);
        const gp_Ax1 ax(gp_Pnt(axis.ox, axis.oy, axis.oz), gp_Dir(axis.dx, axis.dy, axis.dz));

        // No copy of the base, so that Generated() answers for the base's own edges.
        BRepPrimAPI_MakeRevol revol(base, ax, full ? full_turn : angle, Standard_False);
        revol.Build();
        if (!revol.IsDone()) {
            throw std::runtime_error(
                "BRepPrimAPI_MakeRevol did not complete (does the profile cross the axis?)");
        }
        TopoDS_Shape result = revol.Shape();
        // Depending on the turning direction the solid may come out inside out.
        GProp_GProps props;
        BRepGProp::VolumeProperties(result, props);
        if (props.Mass() < 0.0) {
            result.Reverse();
        }

        TopTools_IndexedMapOfShape result_faces;
        TopExp::MapShapes(result, TopAbs_FACE, result_faces);
        OpResultData out = empty_result(result_faces);
        record_profile_faces(revol, base, marks, result_faces, out);
        // Caps first: the start cap holds every profile edge, so the matching below would
        // otherwise take it for a side face.
        if (!full) {
            for (TopExp_Explorer it(revol.FirstShape(), TopAbs_FACE); it.More(); it.Next()) {
                add_source(out, result_faces, it.Current(), SourceKind::StartCap, 0, 0);
            }
            for (TopExp_Explorer it(revol.LastShape(), TopAbs_FACE); it.More(); it.Next()) {
                add_source(out, result_faces, it.Current(), SourceKind::EndCap, 0, 0);
            }
        }
        record_unnamed_profile_faces(base, marks, result_faces, out);

        out.shape = insert(result);
        return out;
    });
}

OpResultData Session::shell(std::uint64_t shape, rust::Slice<const std::uint32_t> faces,
                            double offset) {
    return guarded("shell", [&] {
        if (!(std::abs(offset) > 1e-9)) {
            throw std::invalid_argument("the wall thickness is zero");
        }
        const TopoDS_Shape& input = get(shape);
        TopTools_IndexedMapOfShape input_faces;
        TopExp::MapShapes(input, TopAbs_FACE, input_faces);
        TopTools_ListOfShape removed;
        for (const std::uint32_t index : faces) {
            if (index >= static_cast<std::uint32_t>(input_faces.Extent())) {
                throw std::out_of_range("face index " + std::to_string(index) + " out of range");
            }
            removed.Append(input_faces(static_cast<int>(index) + 1));
        }

        BRepOffsetAPI_MakeThickSolid maker;
        maker.MakeThickSolidByJoin(input, removed, offset, 1.0e-6, BRepOffset_Skin,
                                   Standard_False, Standard_False, GeomAbs_Intersection);
        maker.Build();
        if (!maker.IsDone()) {
            throw std::runtime_error("shell failed (walls too thick for the part?)");
        }
        const TopoDS_Shape result = maker.Shape();

        TopTools_IndexedMapOfShape result_faces;
        TopExp::MapShapes(result, TopAbs_FACE, result_faces);
        OpResultData out = empty_result(result_faces);
        for (int j = 1; j <= input_faces.Extent(); ++j) {
            const TopoDS_Shape& face = input_faces(j);
            const auto index = static_cast<std::uint32_t>(j - 1);
            // The original faces stay as the skin. Depending on the OCCT version, their
            // offset copies are reported as modified or as generated: take both.
            add_source(out, result_faces, face, SourceKind::InputFace, 0, index);
            // Both may return the same internal list, so each is copied before the next call.
            const TopTools_ListOfShape modified = maker.Modified(face);
            const TopTools_ListOfShape generated = maker.Generated(face);
            for (const TopTools_ListOfShape* images : {&modified, &generated}) {
                for (TopTools_ListOfShape::Iterator it(*images); it.More(); it.Next()) {
                    const SourceKind kind =
                        it.Value().IsSame(face) ? SourceKind::InputFace : SourceKind::OffsetFace;
                    add_source(out, result_faces, it.Value(), kind, 0, index);
                }
            }
        }
        record_generated_faces(maker, input, 0, TopAbs_EDGE, SourceKind::InputEdge,
                               result_faces, out);
        record_generated_faces(maker, input, 0, TopAbs_VERTEX, SourceKind::InputVertex,
                               result_faces, out);

        out.shape = insert(result);
        return out;
    });
}

OpResultData Session::transform(std::uint64_t shape, rust::Slice<const double> rows) {
    return guarded("transform", [&] {
        if (rows.size() != 12) {
            throw std::invalid_argument("a transform needs 12 values");
        }
        const TopoDS_Shape& input = get(shape);
        gp_Trsf trsf;
        trsf.SetValues(rows[0], rows[1], rows[2], rows[3], rows[4], rows[5], rows[6], rows[7],
                       rows[8], rows[9], rows[10], rows[11]);
        BRepBuilderAPI_Transform maker(input, trsf, Standard_True);
        if (!maker.IsDone()) {
            throw std::runtime_error("transform failed");
        }
        const TopoDS_Shape result = maker.Shape();

        TopTools_IndexedMapOfShape result_faces;
        TopExp::MapShapes(result, TopAbs_FACE, result_faces);
        OpResultData out = empty_result(result_faces);
        TopTools_IndexedMapOfShape input_faces;
        TopExp::MapShapes(input, TopAbs_FACE, input_faces);
        for (int j = 1; j <= input_faces.Extent(); ++j) {
            add_source(out, result_faces, maker.ModifiedShape(input_faces(j)),
                       SourceKind::InputFace, 0, static_cast<std::uint32_t>(j - 1));
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
                // The surface normal is X × Y of the plane's frame: against its main
                // direction when the frame is left-handed, as mirrored copies and some
                // revolved faces have.
                const gp_Pln plane = surface.Plane();
                gp_Dir normal = plane.Axis().Direction();
                if (!plane.Position().Direct()) {
                    normal.Reverse();
                }
                if (face.Orientation() == TopAbs_REVERSED) {
                    normal.Reverse();
                }
                data.has_normal = true;
                data.nx = normal.X();
                data.ny = normal.Y();
                data.nz = normal.Z();
            }
            switch (surface.GetType()) {
            case GeomAbs_Cylinder:
                data.has_axis = true;
                data.axis = axis_data(surface.Cylinder().Axis());
                break;
            case GeomAbs_Cone:
                data.has_axis = true;
                data.axis = axis_data(surface.Cone().Axis());
                break;
            case GeomAbs_Sphere:
                data.has_axis = true;
                data.axis = axis_data(surface.Sphere().Position().Axis());
                break;
            case GeomAbs_Torus:
                data.has_axis = true;
                data.axis = axis_data(surface.Torus().Axis());
                break;
            default:
                break;
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
                if (curve.GetType() == GeomAbs_Line) {
                    data.has_axis = true;
                    data.axis = axis_data(
                        gp_Ax1(curve.Value(curve.FirstParameter()), curve.Line().Direction()));
                } else if (curve.GetType() == GeomAbs_Circle) {
                    data.has_axis = true;
                    data.axis = axis_data(curve.Circle().Axis());
                }
            }
            data.mx = mid.X();
            data.my = mid.Y();
            data.mz = mid.Z();

            if (edge_faces.Contains(edge)) {
                const TopTools_ListOfShape& adjacent = edge_faces.FindFromKey(edge);
                for (TopTools_ListOfShape::Iterator it(adjacent); it.More(); it.Next()) {
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

MeshData Session::tessellate(std::uint64_t shape, double deflection) const {
    return guarded("tessellate", [&] {
        if (!(deflection > 0.0)) {
            throw std::invalid_argument("deflection must be positive");
        }
        const TopoDS_Shape& s = get(shape);
        // Stores a triangulation on every face of the shape (reused by later calls).
        BRepMesh_IncrementalMesh mesher(s, deflection, Standard_False, 0.35, Standard_False);

        TopTools_IndexedMapOfShape faces;
        TopTools_IndexedMapOfShape edges;
        TopExp::MapShapes(s, TopAbs_FACE, faces);
        TopExp::MapShapes(s, TopAbs_EDGE, edges);

        MeshData out{};
        for (int i = 1; i <= faces.Extent(); ++i) {
            const TopoDS_Face& face = TopoDS::Face(faces(i));
            FaceMeshData data{};
            TopLoc_Location location;
            const Handle(Poly_Triangulation) triangulation =
                BRep_Tool::Triangulation(face, location);
            if (!triangulation.IsNull()) {
                const gp_Trsf transform = location.Transformation();
                for (int n = 1; n <= triangulation->NbNodes(); ++n) {
                    const gp_Pnt p = triangulation->Node(n).Transformed(transform);
                    data.positions.push_back(static_cast<float>(p.X()));
                    data.positions.push_back(static_cast<float>(p.Y()));
                    data.positions.push_back(static_cast<float>(p.Z()));
                }
                const bool reversed = face.Orientation() == TopAbs_REVERSED;
                for (int t = 1; t <= triangulation->NbTriangles(); ++t) {
                    Standard_Integer a = 0;
                    Standard_Integer b = 0;
                    Standard_Integer c = 0;
                    triangulation->Triangle(t).Get(a, b, c);
                    if (reversed) {
                        std::swap(b, c);
                    }
                    data.indices.push_back(static_cast<std::uint32_t>(a - 1));
                    data.indices.push_back(static_cast<std::uint32_t>(b - 1));
                    data.indices.push_back(static_cast<std::uint32_t>(c - 1));
                }
            }
            out.faces.push_back(std::move(data));
        }

        for (int i = 1; i <= edges.Extent(); ++i) {
            const TopoDS_Edge& edge = TopoDS::Edge(edges(i));
            EdgeLineData data{};
            if (!BRep_Tool::Degenerated(edge)) {
                const BRepAdaptor_Curve curve(edge);
                const GCPnts_TangentialDeflection points(curve, 0.2, deflection);
                for (int k = 1; k <= points.NbPoints(); ++k) {
                    const gp_Pnt p = points.Value(k);
                    data.points.push_back(static_cast<float>(p.X()));
                    data.points.push_back(static_cast<float>(p.Y()));
                    data.points.push_back(static_cast<float>(p.Z()));
                }
            }
            out.edges.push_back(std::move(data));
        }
        return out;
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
        const std::lock_guard<std::mutex> lock(step_mutex());
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

std::uint64_t Session::import_step(rust::Slice<const std::uint8_t> data) {
    return guarded("import_step", [&] {
        const std::lock_guard<std::mutex> lock(step_mutex());
        std::istringstream stream(
            std::string(reinterpret_cast<const char*>(data.data()), data.size()));
        STEPControl_Reader reader;
        if (reader.ReadStream("import.step", stream) != IFSelect_RetDone) {
            throw std::runtime_error("not a readable STEP file");
        }
        reader.TransferRoots();
        const TopoDS_Shape all = reader.OneShape();
        if (all.IsNull()) {
            throw std::runtime_error("the STEP file holds no geometry");
        }
        const TopoDS_Shape result = solid_body(all);
        if (result.IsNull()) {
            throw std::runtime_error("the STEP file has no solid body (open surfaces only)");
        }
        return insert(result);
    });
}

StepAssemblyData Session::read_step_assembly(rust::Slice<const std::uint8_t> data) {
    return guarded("read_step_assembly", [&] {
        const std::lock_guard<std::mutex> lock(step_mutex());
        std::istringstream stream(
            std::string(reinterpret_cast<const char*>(data.data()), data.size()));
        const Handle(XCAFApp_Application) app = XCAFApp_Application::GetApplication();
        Handle(TDocStd_Document) doc;
        app->NewDocument("MDTV-XCAF", doc);
        // Closes the document however this function ends.
        struct Closer {
            const Handle(XCAFApp_Application)& app;
            Handle(TDocStd_Document)& doc;
            ~Closer() {
                try {
                    if (!doc.IsNull() && doc->IsOpened()) {
                        app->Close(doc);
                    }
                } catch (...) {
                }
            }
        } closer{app, doc};

        STEPCAFControl_Reader reader;
        reader.SetNameMode(true);
        if (reader.ChangeReader().ReadStream("import.step", stream) != IFSelect_RetDone) {
            throw std::runtime_error("not a readable STEP file");
        }
        if (!reader.Transfer(doc)) {
            throw std::runtime_error("could not transfer the STEP file");
        }
        TDF_LabelSequence roots;
        XCAFDoc_DocumentTool::ShapeTool(doc->Main())->GetFreeShapes(roots);
        if (roots.Length() == 0) {
            throw std::runtime_error("the STEP file holds no geometry");
        }

        StepAssemblyData out{};
        StepTreeBuilder builder(out);
        if (roots.Length() == 1) {
            out.root = builder.node(roots.Value(1));
        } else {
            // Several top-level products: an unnamed assembly holds them.
            StepNodeData top{};
            top.is_assembly = true;
            for (int i = 1; i <= roots.Length(); ++i) {
                const std::uint32_t child = builder.node(roots.Value(i));
                top.instances.push_back(instance_data(child, label_name(roots.Value(i)), gp_Trsf()));
            }
            out.root = static_cast<std::uint32_t>(out.nodes.size());
            out.nodes.push_back(std::move(top));
        }
        return out;
    });
}

std::unique_ptr<Session> new_session() {
    return std::make_unique<Session>();
}

rust::String occt_version() {
    return rust::String(OCC_VERSION_COMPLETE);
}

} // namespace oxyde::occt
