//! The tools: their names, descriptions and input schemas (what `tools/list` answers), and
//! the dispatch of a call to the code that does it ([`crate::modelling`],
//! [`crate::looking`], [`crate::assembling`]).

use std::path::PathBuf;

use oxyde_format::Properties;
use oxyde_model::{Appearance, MaterialLibrary};
use serde_json::{Value, json};

use crate::args::Set;
use crate::args::{self, Result};
use crate::session::{Session, body_summary, status_report};
use crate::{assembling, looking, modelling};

/// What a tool returns: text (JSON, mostly) and pictures.
#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub text: String,
    pub images: Vec<Image>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
        }
    }

    pub(crate) fn json(value: &Value) -> Self {
        Self::text(layout(value))
    }
}

/// JSON laid out to be read cheaply: one line per key of the top object, one line per
/// element of a list of objects under it, everything deeper on that line.
fn layout(value: &Value) -> String {
    let compact = |v: &Value| serde_json::to_string(v).unwrap_or_default();
    let Value::Object(map) = value else {
        return compact(value);
    };
    let mut out = String::from("{\n");
    let count = map.len();
    for (k, (key, v)) in map.iter().enumerate() {
        out.push_str(&format!("  {}: ", compact(&Value::String(key.clone()))));
        match v {
            Value::Array(items)
                if items.iter().any(|i| i.is_object() || i.is_array()) && items.len() > 1 =>
            {
                out.push_str("[\n");
                for (j, item) in items.iter().enumerate() {
                    out.push_str("    ");
                    out.push_str(&compact(item));
                    out.push_str(if j + 1 < items.len() { ",\n" } else { "\n" });
                }
                out.push_str("  ]");
            }
            other => out.push_str(&compact(other)),
        }
        out.push_str(if k + 1 < count { ",\n" } else { "\n" });
    }
    out.push('}');
    out
}

/// A PNG picture, and where it was written.
#[derive(Debug, Clone)]
pub struct Image {
    pub png: Vec<u8>,
    pub path: Option<PathBuf>,
}

const SELECTOR: &str = "Selector: a persistent name (\"F2.end\"), an index, or an object whose keys all \
apply: name, index, surface (plane|cylinder|cone|sphere|torus|bspline), curve (line|circle|...), \
normal ([x,y,z] or \"+y\"), axis ([x,y,z] or \"x\"), radius / diameter (value or [min,max]), area / \
length ([min,max]), box ({min,max} on centroid or midpoint), created_by (feature), of_faces (edges of \
those faces), extreme (\"+y\": furthest that way), near ([x,y,z]: closest; with count or within). A \
list is the union.";

const PLANE: &str = "A plane: \"front\" (XY), \"top\" (XZ), \"right\" (YZ), the name of a reference \
plane, {\"face\": selector} for a flat face, or {\"base\": plane, \"offset\": mm} for a new reference \
plane at a distance.";

const AXIS: &str = "An axis or direction: \"x\", \"y\", \"z\", the name of a reference axis, \
{\"edge\": selector} (a straight edge's line or a circular edge's axis) or {\"face\": selector} (a \
round face's axis or a flat face's normal).";

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        }
    })
}

fn any(description: &str) -> Value {
    json!({ "description": description })
}

fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn number(description: &str) -> Value {
    json!({ "type": "number", "description": description })
}

fn boolean(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}

/// Every tool, as `tools/list` gives them.
pub fn list() -> Vec<Value> {
    let feature = any("A feature, by name (\"Boss-Extrude1\") or id.");
    let operation = json!({ "type": "string", "enum": ["boss", "cut", "intersect", "new_body"],
        "description": "What the swept solid does: boss joins (default), cut removes, intersect keeps \
        the common part, new_body makes a separate body." });
    let bodies = any(
        "Bodies to work on (default: automatic): body names as part_info lists them \
        (\"Body 1\") or {\"feature\": name, \"index\": n}.",
    );
    let views = any(
        "Views: \"iso\", \"front\", \"back\", \"top\", \"bottom\", \"left\", \"right\", or \
        {\"yaw\": deg, \"pitch\": deg}; a list tiles them two per row. Default iso.",
    );
    let size = any("[width, height] of each view in pixels (default [640, 480]).");
    let out = string("Where to write the PNG (default: a temporary file).");
    let target = json!({ "type": "string", "enum": ["part", "reference"],
        "description": "The part being modelled (default) or the reference." });
    vec![
        // ---- Documents ----
        tool(
            "new_part",
            "Starts a new, empty part (the one being modelled). Nothing is written until save_part.",
            json!({ "path": string("Where save_part will write it (.oxyprt)."),
                "part_number": string("properties: part number"),
                "description": string("properties: description") }),
            &[],
        ),
        tool(
            "open_part",
            "Opens a part file (.oxyprt) to continue modelling it.",
            json!({ "path": string("The .oxyprt file.") }),
            &["path"],
        ),
        tool(
            "save_part",
            "Saves the part (to its path, or to a new one).",
            json!({ "path": string("Where to save (.oxyprt); default the part's own path.") }),
            &[],
        ),
        tool(
            "part_info",
            "The timeline (every feature, its status and message), the bodies with \
            volume and bounds, and optionally every dimension with its full name and value.",
            json!({ "target": target.clone(), "dimensions": boolean("List the dimensions too.") }),
            &[],
        ),
        tool(
            "undo",
            "Takes back the last change to the part.",
            json!({}),
            &[],
        ),
        tool(
            "set_properties",
            "Sets the part's properties for the bill of materials.",
            json!({ "part_number": string(""), "description": string(""), "revision": string("") }),
            &[],
        ),
        tool(
            "set_appearance",
            "Colours the whole part (or clears its colour).",
            json!({ "color": any("[r, g, b], 0-255; null clears."), "transparency": number("0 opaque to 1 invisible.") }),
            &[],
        ),
        tool(
            "set_material",
            "Gives the part a material from the standard library (its look and density), \
            or removes it.",
            json!({ "name": string("e.g. \"Aluminium 6061-T6\", \"ABS\", \"Acrylic\"; null removes; \"?\" lists them.") }),
            &[],
        ),
        // ---- Sketches and features ----
        tool(
            "sketch",
            "Adds a sketch made of shapes, fully dimensioned from the origin: rect (center or \
            corner, size, optional radius for rounded corners), circle (center, diameter), slot (start, end, \
            width), polygon (center, sides, radius, angle), polyline (points, closed, fillet; a point may be \
            {at, fillet} or {at, center, cw} for an arc from the previous point), line (start, end, \
            construction for a centerline), arc (center, start, end), point (at), text (text, size: the em in mm, font: a .ttf path or a system font file name such as \"arial\" or \"ariali\", at, angle, slant (degrees of synthetic oblique), spacing (mm added between letters), align left|center|right, valign baseline|middle|bottom|top; the letters' outlines, fixed). Each item may carry \
            \"id\" (a label; the result maps labels to entity ids), \"construction\" and \"position\" \
            (\"free\", \"fixed\"). Points are [x, y] in the sketch plane (Front: x, y; Top: x, -z; Right: \
            -z, y) or [x, y, z] in the model, projected.",
            json!({ "plane": any(PLANE), "items": json!({ "type": "array", "description": "The shapes." }),
                "name": string("Feature name (default Sketch<n>).") }),
            &["plane", "items"],
        ),
        tool(
            "extrude",
            "Extrudes a sketch's closed regions (Boss-Extrude / Cut-Extrude). Bosses go along the \
            sketch normal, cuts into the face, unless reversed.",
            json!({ "sketch": any("The sketch, by name or id, or a sketch spec made on the way."),
                "operation": operation.clone(), "depth": number("mm, for blind and mid_plane."),
                "end": any("\"blind\" (default), \"mid_plane\", \"through_all\", \"through_all_both\", or {\"up_to\": plane}."),
                "reverse": boolean("Go the other way."), "bodies": bodies.clone(), "name": string("Feature name.") }),
            &["sketch"],
        ),
        tool(
            "revolve",
            "Revolves a sketch's closed regions about an axis.",
            json!({ "sketch": any("The sketch, or a sketch spec made on the way."),
                "axis": any("{\"line\": label or entity id of a line in the sketch}, or an axis: x, y, z, a reference axis, an edge or a face."),
                "angle": number("Degrees, default 360."), "operation": operation.clone(),
                "mid_plane": boolean("Half the angle each side."), "reverse": boolean(""),
                "bodies": bodies.clone(), "name": string("") }),
            &["sketch", "axis"],
        ),
        tool(
            "fillet",
            "Rounds edges.",
            json!({ "edges": any(SELECTOR), "radius": number("mm"), "name": string("") }),
            &["edges", "radius"],
        ),
        tool(
            "chamfer",
            "Bevels edges: equal, two distances, or a distance and an angle.",
            json!({ "edges": any(SELECTOR), "distance": number("mm"), "distance2": number("Second distance (mm)."),
                "angle": number("Degrees, with distance."), "flip": boolean("Swap the faces."), "name": string("") }),
            &["edges", "distance"],
        ),
        tool(
            "shell",
            "Hollows the part, removing faces (or none for a closed void).",
            json!({ "faces": any(SELECTOR), "thickness": number("mm"), "outward": boolean("Walls outside."), "name": string("") }),
            &["thickness"],
        ),
        tool(
            "linear_pattern",
            "Copies features (extrusions, revolves, sweeps, lofts, holes) along one or two directions.",
            json!({ "features": any("Features to copy (names or ids)."),
                "direction": any(&format!("{{\"axis\", \"spacing\" (mm), \"count\" (with the seed), \"reverse\"}}. {AXIS}")),
                "direction2": any("The same for a second direction."), "name": string("") }),
            &["features", "direction"],
        ),
        tool(
            "circular_pattern",
            "Copies features around an axis.",
            json!({ "features": any("Features to copy."), "axis": any(AXIS), "count": number("Instances, the seed included."),
                "angle": number("Degrees: total with equal spacing (default 360), else between instances."),
                "equal_spacing": boolean("Default true."), "reverse": boolean(""), "name": string("") }),
            &["features", "axis", "count"],
        ),
        tool(
            "mirror",
            "Mirrors features (their tool bodies) or whole bodies through a plane.",
            json!({ "features": any("Features to mirror."), "bodies": bodies.clone(), "plane": any(PLANE), "name": string("") }),
            &["plane"],
        ),
        tool(
            "hole",
            "Hole Wizard: drills holes at points on a flat face (a sketch of points is made on it).",
            json!({ "face": any(SELECTOR), "points": any("[[x, y], ...] in the face's sketch frame, or [x, y, z] model points."),
                "sketch": any("Or an existing sketch of points."),
                "kind": json!({ "type": "string", "enum": ["simple", "counterbore", "countersink"] }),
                "diameter": number("mm"), "depth": number("mm of full diameter; omit for through all."),
                "counter_diameter": number("mm"), "counter_depth": number("mm (counterbore)"),
                "angle": number("Countersink angle, degrees (default 90)."), "reverse": boolean(""),
                "name": string("") }),
            &["diameter"],
        ),
        tool(
            "ref_plane",
            "A reference plane: at \"offset\" mm from \"base\" (a plane or a flat face); or turned by \
            \"angle\" degrees about \"line\" (an edge, an axis) lying in \"base\"; or through \"line\", \
            parallel to \"base\" or \"perpendicular\" to it. Planes through three points or normal to a \
            curve: add_feature with the plane feature's JSON.",
            json!({ "base": any(PLANE), "offset": number("mm"), "line": any(AXIS),
                "angle": number("Degrees about the line; negative turns the other way."),
                "perpendicular": boolean("Through the line, square to the base (default parallel)."),
                "flip": boolean("Other side, other way round, or normal reversed."), "name": string("") }),
            &["base"],
        ),
        tool(
            "ref_axis",
            "A reference axis along an edge or a face's axis, or where two planes meet.",
            json!({ "along": any(AXIS), "planes": any("[plane, plane]"), "name": string("") }),
            &[],
        ),
        tool(
            "loft",
            "Lofts through the closed contours of sketches, in order.",
            json!({ "profiles": any("Sketches, in order (names, ids or specs)."), "operation": operation.clone(),
                "ruled": boolean("Straight between sections."), "bodies": bodies.clone(), "name": string("") }),
            &["profiles"],
        ),
        tool(
            "sweep",
            "Sweeps a sketch's closed regions along the curves of another sketch.",
            json!({ "profile": any("Profile sketch."), "path": any("Path sketch."), "operation": operation.clone(),
                "bodies": bodies.clone(), "name": string("") }),
            &["profile", "path"],
        ),
        tool(
            "combine",
            "Joins, cuts or intersects bodies.",
            json!({ "target": any("The body changed."), "tools": any("Bodies used."),
                "operation": json!({ "type": "string", "enum": ["join", "cut", "intersect"] }),
                "keep_tools": boolean(""), "name": string("") }),
            &["target", "tools", "operation"],
        ),
        tool(
            "add_feature",
            "Adds a feature written as an entry of model.json (docs/FORMAT.md section 3); \
            id and name are given by the part.",
            json!({ "feature": any("The feature's JSON."), "name": string("") }),
            &["feature"],
        ),
        tool(
            "import_step",
            "Imports the solids of a STEP file into the part (File › Import STEP); the file is kept in the part.",
            json!({ "path": string("The .step or .stp file."), "name": string("") }),
            &["path"],
        ),
        tool(
            "get_feature",
            "A feature as model.json writes it.",
            json!({ "feature": feature.clone() }),
            &["feature"],
        ),
        tool(
            "edit_feature",
            "Replaces a feature's definition with JSON as model.json writes it.",
            json!({ "feature": feature.clone(), "definition": any("The new definition.") }),
            &["feature", "definition"],
        ),
        tool(
            "set_dimension",
            "Changes a dimension by full name (\"D1@Sketch1\"): mm, degrees, or a count.",
            json!({ "name": string("Full name."), "value": number("") }),
            &["name", "value"],
        ),
        tool(
            "release_dimension",
            "Makes a dimension or global variable that an assembly's equation drives the \
             part's own again (SolidWorks' Break external reference). The assembly drives it \
             again when it is next opened and saved, unless its equation is deleted there.",
            json!({ "name": string("Full name (\"D1@Sketch1\") or a variable's name.") }),
            &["name"],
        ),
        tool(
            "rename_dimension",
            "Renames a dimension (\"D1@Sketch1\" to \"Width\").",
            json!({ "name": string("Full name."), "new_name": string("") }),
            &["name", "new_name"],
        ),
        tool(
            "set_variable",
            "Adds or changes a global variable (\"Width\" = 120) of the part, or with \
             \"assembly\": true of the open assembly.",
            json!({
                "name": string(""),
                "expression": string("e.g. \"120\" or \"\\\"Depth\\\" / 2\""),
                "assembly": { "type": "boolean", "description": "The open assembly's variable, not the part's." },
            }),
            &["name", "expression"],
        ),
        tool(
            "link_equations",
            "Links the part to an equations file shared with other parts (SolidWorks' \
             \"Link to external file\"): a text file of \"Name\" = expression lines, found \
             relative to the part's folder. `variables` are written into the file first; the \
             part then takes every variable of the file, kept up to date when it is opened. \
             set_variable on a linked variable changes the file. \"file\": null unlinks. \
             With \"assembly\": true, links the open assembly: changing a linked variable \
             there also updates its parts linked to the same file.",
            json!({
                "file": string("e.g. \"Case dimensions.txt\", or null to unlink."),
                "variables": { "type": "object", "description": "e.g. {\"Width\": \"233\", \"Depth\": \"300\"}" },
                "assembly": { "type": "boolean", "description": "Link the open assembly, not the part." },
            }),
            &["file"],
        ),
        tool(
            "set_equation",
            "Drives a dimension by an equation (\"D1@Sketch1\" = \"Width\" * 2). With \
             \"assembly\": true, an equation of the open assembly, as SolidWorks has them: \
             it drives a Distance or Angle mate (\"D1@Distance1\", mm or degrees) or a \
             part's dimension or variable (\"D1@Sketch1@Top panel<1>.Part\", through a \
             sub-assembly \"D1@Sketch1@SSD<1>.Part@SSD holder<1>.Assembly\"), and may read \
             them. A part driven so changes for every instance of it and is saved with \
             save_assembly.",
            json!({
                "dimension": string("Full name."),
                "expression": string(""),
                "assembly": { "type": "boolean", "description": "An equation of the open assembly, not the part." },
            }),
            &["dimension", "expression"],
        ),
        tool(
            "delete_feature",
            "Deletes a feature (refused while others depend on it).",
            json!({ "feature": feature.clone() }),
            &["feature"],
        ),
        tool(
            "suppress_feature",
            "Suppresses or unsuppresses a feature.",
            json!({ "feature": feature.clone(), "suppressed": boolean("Default true.") }),
            &["feature"],
        ),
        tool(
            "rename_feature",
            "Renames a feature.",
            json!({ "feature": feature.clone(), "name": string("") }),
            &["feature", "name"],
        ),
        tool(
            "rollback",
            "Moves the rollback bar above a feature (new features go there), or to the end.",
            json!({ "before": any("A feature, or null for the end.") }),
            &[],
        ),
        // ---- Looking ----
        tool(
            "faces",
            "Lists faces: index, persistent name, surface, area, centroid, normal or axis, radius.",
            json!({ "target": target.clone(), "select": any(SELECTOR), "limit": number("At most this many (default 200).") }),
            &[],
        ),
        tool(
            "edges",
            "Lists edges: index, name, curve, length, ends, direction or centre and radius.",
            json!({ "target": target.clone(), "select": any(SELECTOR), "limit": number("Default 200.") }),
            &[],
        ),
        tool(
            "section",
            "Cuts the solids by a plane and reads the section exactly: outer outlines with their \
            lines and arcs, and the other loops recognised as circles, rectangles, slots and polygons, grouped \
            by size with their centres and grid spacing: what a sketch on that plane needs. Coordinates are \
            the plane's sketch coordinates.",
            json!({ "target": target.clone(), "plane": any("\"front\", \"top\", \"right\" (offset along their normal), or {\"origin\": [x,y,z], \"normal\": [x,y,z]}."),
                "offset": number("mm along the plane's normal."), "detail": boolean("Curves of every loop."),
                "image": boolean("A plot of the section."),
                "region": any("Zoom the plot on {\"min\": [x, y], \"max\": [x, y]} of the plane."),
                "out": out.clone() }),
            &["plane"],
        ),
        tool(
            "probe",
            "A depth gauge: where the solid is along lines through points, parallel to an axis             (the other two coordinates of each point fix the line). Tells a recess from a through             hole, the thickness of a wall, the depth of a pocket.",
            json!({ "target": target.clone(), "points": any("[[x, y, z], ...]"),
                "axis": json!({ "type": "string", "enum": ["x", "y", "z"], "description": "Default y." }) }),
            &["points"],
        ),
        tool(
            "measure",
            "The shortest distance between two faces or edges.",
            json!({ "target": target.clone(), "a": any("{\"face\": selector} or {\"edge\": selector}"), "b": any("The same.") }),
            &["a", "b"],
        ),
        tool(
            "render",
            "A shaded picture of the part, the reference, both overlaid (reference translucent red), or \
            the assembly, from one or more views; faces and edges can be highlighted.",
            json!({ "target": json!({ "type": "string", "enum": ["part", "reference", "both", "assembly"] }),
                "views": views.clone(), "size": size.clone(), "highlight": any("{\"faces\": selector, \"edges\": selector} on the part."),
                "region": any("Zoom on a box: {\"min\": [x, y, z], \"max\": [x, y, z]}."),
                "out": out.clone() }),
            &[],
        ),
        tool(
            "open_reference",
            "Opens a part (.oxyprt) or a STEP file to reproduce, read-only, optionally moved: \
            {\"center\": true} centres its box on the origin (\"xz\" only those axes), {\"translate\": [x,y,z]} and \
            {\"rotation\": ...} (applied first) move it.",
            json!({ "path": string(""), "center": any("true, or the axes to centre (\"xz\")."),
                "translate": any("[x, y, z]"), "rotation": any("A 3x3 matrix or {\"axis\", \"angle\"} steps.") }),
            &["path"],
        ),
        tool(
            "compare",
            "Compares the part with the reference by exact booleans: their volumes, what they share, \
            material too much and too little, a match score (common / union), their boxes; optionally a picture \
            (extra material red, missing blue, the part grey).",
            json!({ "image": boolean(""), "views": views.clone(), "size": size.clone(),
                "method": json!({ "type": "string", "enum": ["auto", "sampling"],
                    "description": "auto: exact booleans, checked; sampling: meshes only, for shapes the booleans choke on." }),
                "region": any("Zoom on a box: {\"min\": [x, y, z], \"max\": [x, y, z]}."), "out": out.clone() }),
            &[],
        ),
        // ---- Assemblies ----
        tool(
            "new_assembly",
            "Starts a new assembly; component paths are kept relative to its folder.",
            json!({ "path": string("The .oxyasm file it will be saved as."), "part_number": string("") }),
            &["path"],
        ),
        tool(
            "open_assembly",
            "Opens an assembly and loads its components.",
            json!({ "path": string("") }),
            &["path"],
        ),
        tool(
            "save_assembly",
            "Saves the assembly.",
            json!({ "path": string("Default its own path.") }),
            &[],
        ),
        tool(
            "add_component",
            "Inserts a part or a sub-assembly at a position and rotation.",
            json!({ "file": string("The .oxyprt or .oxyasm (absolute, or relative to the assembly's folder)."),
                "position": any("[x, y, z] of its origin."), "rotation": any("3x3 matrix or {\"axis\", \"angle\"} steps."),
                "fixed": boolean(""), "name": string("Instance name (default file<n>)."),
                "hidden": boolean(""), "color": any("[r, g, b] appearance of the component."),
                "transparency": number("0 opaque to 1 invisible, with color.") }),
            &["file"],
        ),
        tool(
            "place_component",
            "Moves a component, or fixes or floats it.",
            json!({ "component": any("Name or id."), "position": any("[x,y,z]"), "rotation": any(""), "fixed": boolean(""),
                "hidden": boolean(""), "color": any("[r, g, b], or null to clear."), "transparency": number("") }),
            &["component"],
        ),
        tool(
            "remove_component",
            "Removes a component and its mates.",
            json!({ "component": any("") }),
            &["component"],
        ),
        tool(
            "add_mate",
            "Mates two components and solves: coincident, concentric, parallel, perpendicular, \
            distance (mm), angle (degrees). A side is {\"component\", \"face\" | \"edge\": selector in the part's \
            coordinates} or {\"component\", \"plane\" | \"axis\" | \"origin\"}; component 0 or \"assembly\" is the \
            assembly's own planes. On a sub-assembly component, \"path\": [component names or ids inside it] \
            reaches the part whose face or edge is meant.",
            json!({ "kind": json!({ "type": "string", "enum": ["coincident", "concentric", "parallel", "perpendicular", "distance", "angle"] }),
                "a": any(""), "b": any(""), "value": number("mm or degrees"), "aligned": boolean("Normals the same way (default: as they are).") }),
            &["kind", "a", "b"],
        ),
        tool(
            "assembly_info",
            "Components (placement, fixed, file), mates and their status, and the volume.",
            json!({}),
            &[],
        ),
        tool(
            "export",
            "Writes the part (or the assembly's visible parts) as STEP, STL, OBJ, 3MF or glTF.",
            json!({ "target": json!({ "type": "string", "enum": ["part", "assembly"] }), "out": string("The file; the extension picks the format.") }),
            &["out"],
        ),
    ]
}

/// Whether there is a tool of that name.
pub fn exists(name: &str) -> bool {
    list().iter().any(|t| t["name"] == name)
}

/// The tools a session offers: in an Oxyde window, all but those that choose or save the
/// document ([`crate::window::DOCUMENT_TOOLS`]).
pub fn list_for(session: &Session) -> Vec<Value> {
    let mut tools = list();
    if session.window {
        tools.retain(|t| {
            !crate::window::DOCUMENT_TOOLS
                .iter()
                .any(|name| t["name"] == *name)
        });
    }
    tools
}

/// Runs tool `name` with `args`. A panic in the engine is caught and reported as the tool's
/// error, so the session survives it.
pub fn call(session: &mut Session, name: &str, args: &Value) -> Result<ToolOutput> {
    if session.window && crate::window::DOCUMENT_TOOLS.contains(&name) {
        return Err(crate::window::DOCUMENT_TOOL_REFUSED.to_owned());
    }
    match oxyde_kernel::catch_panic(|| dispatch(session, name, args)) {
        Ok(result) => result,
        Err(panic) => Err(format!("internal error: {panic}")),
    }
}

fn dispatch(session: &mut Session, name: &str, args: &Value) -> Result<ToolOutput> {
    match name {
        "new_part" => new_part(session, args),
        "open_part" => {
            session.open_part(&args::path(args, "path")?)?;
            part_info(session, &json!({}))
        }
        "save_part" => {
            let path = args::opt_str(args, "path")?.map(PathBuf::from);
            let saved = session.save_part(path)?;
            Ok(ToolOutput::json(
                &json!({ "saved": saved.display().to_string() }),
            ))
        }
        "part_info" => part_info(session, args),
        "undo" => {
            session.undo()?;
            let doc = session.doc()?;
            Ok(ToolOutput::json(&json!({ "undone": true,
                "features": doc.part.features().len(), "body": body_summary(&session.kernel, &doc.part) })))
        }
        "set_properties" => {
            let doc = session.doc_mut()?;
            if let Some(v) = args::opt_str(args, "part_number")? {
                doc.properties.part_number = v.to_owned();
            }
            if let Some(v) = args::opt_str(args, "description")? {
                doc.properties.description = v.to_owned();
            }
            if let Some(v) = args::opt_str(args, "revision")? {
                doc.properties.revision = v.to_owned();
            }
            Ok(ToolOutput::json(
                &json!({ "properties": serde_json::to_value(&doc.properties).unwrap_or_default() }),
            ))
        }
        "set_appearance" => {
            let appearance = match args::opt(args, "color") {
                None => None,
                Some(c) => {
                    let p = args::point3(c, "\"color\"")?;
                    let byte = |v: f64| v.round().clamp(0.0, 255.0) as u8;
                    Some(Appearance {
                        color: [byte(p.x), byte(p.y), byte(p.z)],
                        transparency: args::opt_f64(args, "transparency")?
                            .unwrap_or(0.0)
                            .clamp(0.0, 1.0),
                        texture: None,
                    })
                }
            };
            session.change(|doc| {
                doc.part.appearances_mut().set_part(appearance);
                Ok(())
            })?;
            Ok(ToolOutput::text("ok"))
        }
        "set_material" => {
            let library = MaterialLibrary::standard();
            match args::opt_str(args, "name")? {
                Some("?") => Ok(ToolOutput::json(&json!(
                    library
                        .materials()
                        .iter()
                        .map(|m| format!("{} ({}, {} kg/m3)", m.name, m.category, m.density))
                        .collect::<Vec<_>>()
                ))),
                Some(name) => {
                    let material = library
                        .materials()
                        .iter()
                        .find(|m| m.name.eq_ignore_ascii_case(name))
                        .cloned()
                        .ok_or_else(|| {
                            format!("no material \"{name}\" in the library (\"?\" lists them)")
                        })?;
                    let label = material.name.clone();
                    session.change(|doc| {
                        doc.part.set_material(Some(material));
                        Ok(())
                    })?;
                    Ok(ToolOutput::text(format!("material: {label}")))
                }
                None => {
                    session.change(|doc| {
                        doc.part.set_material(None);
                        Ok(())
                    })?;
                    Ok(ToolOutput::text("material removed"))
                }
            }
        }
        "sketch" => modelling::sketch(session, args),
        "extrude" => modelling::extrude(session, args),
        "revolve" => modelling::revolve(session, args),
        "fillet" => modelling::blend(session, args, false),
        "chamfer" => modelling::blend(session, args, true),
        "shell" => modelling::shell(session, args),
        "linear_pattern" => modelling::linear_pattern(session, args),
        "circular_pattern" => modelling::circular_pattern(session, args),
        "mirror" => modelling::mirror(session, args),
        "hole" => modelling::hole(session, args),
        "ref_plane" => modelling::ref_plane(session, args),
        "ref_axis" => modelling::ref_axis(session, args),
        "loft" => modelling::loft(session, args),
        "sweep" => modelling::sweep(session, args),
        "combine" => modelling::combine(session, args),
        "add_feature" => modelling::add_feature(session, args),
        "get_feature" => modelling::get_feature(session, args),
        "import_step" => modelling::import_step(session, args),
        "edit_feature" => modelling::edit_feature(session, args),
        "set_dimension" => modelling::set_dimension(session, args),
        "release_dimension" => modelling::release_dimension(session, args),
        "rename_dimension" => modelling::rename_dimension(session, args),
        "set_variable" if args::bool(args, "assembly", false)? => {
            crate::assembly_equations::set_variable(session, args)
        }
        "set_equation" if args::bool(args, "assembly", false)? => {
            crate::assembly_equations::set_equation(session, args)
        }
        "link_equations" if args::bool(args, "assembly", false)? => {
            crate::assembly_equations::link_equations(session, args)
        }
        "set_variable" => modelling::set_variable(session, args),
        "set_equation" => modelling::set_equation(session, args),
        "link_equations" => modelling::link_equations(session, args),
        "delete_feature" => modelling::delete_feature(session, args),
        "suppress_feature" => modelling::suppress_feature(session, args),
        "rename_feature" => modelling::rename_feature(session, args),
        "rollback" => modelling::rollback(session, args),
        "faces" => looking::faces(session, args),
        "edges" => looking::edges(session, args),
        "section" => looking::section(session, args),
        "measure" => looking::measure(session, args),
        "probe" => looking::probe(session, args),
        "render" => looking::render(session, args),
        "open_reference" => looking::open_reference(session, args),
        "compare" => looking::compare(session, args),
        "new_assembly" => assembling::new_assembly(session, args),
        "open_assembly" => assembling::open_assembly(session, args),
        "save_assembly" => assembling::save_assembly(session, args),
        "add_component" => assembling::add_component(session, args),
        "place_component" => assembling::place_component(session, args),
        "remove_component" => assembling::remove_component(session, args),
        "add_mate" => assembling::add_mate(session, args),
        "assembly_info" => assembling::assembly_info(session),
        "export" => looking::export(session, args),
        other => Err(format!("unknown tool \"{other}\"")),
    }
}

fn new_part(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let path = args::opt_str(args, "path")?.map(PathBuf::from);
    let properties = Properties {
        part_number: args::opt_str(args, "part_number")?
            .map(str::to_owned)
            .or_else(|| {
                path.as_ref()
                    .and_then(|p| p.file_stem())
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .unwrap_or_default(),
        description: args::opt_str(args, "description")?.unwrap_or("").to_owned(),
        ..Properties::default()
    };
    session.new_part(path, properties);
    Ok(ToolOutput::text(
        "new part: sketch on \"front\", \"top\" or \"right\" to start",
    ))
}

fn part_info(session: &mut Session, args: &Value) -> Result<ToolOutput> {
    let reference = args::opt_str(args, "target")? == Some("reference");
    let part = if reference {
        &session.reference()?.part
    } else {
        &session.doc()?.part
    };
    let timeline: Vec<Value> = part
        .timeline()
        .into_iter()
        .map(|e| {
            let status = match &e.status {
                oxyde_model::FeatureStatus::Ok => "ok".to_owned(),
                oxyde_model::FeatureStatus::Warning(m) => format!("warning: {m}"),
                oxyde_model::FeatureStatus::Error(m) => format!("error: {m}"),
                oxyde_model::FeatureStatus::Suppressed => "suppressed".to_owned(),
                oxyde_model::FeatureStatus::RolledBack => "rolled back".to_owned(),
                oxyde_model::FeatureStatus::Pending => "pending".to_owned(),
            };
            let mut row =
                json!({ "id": e.id.raw(), "name": e.name, "type": e.type_name, "status": status });
            if let Some(sketch) = part.sketch_result(e.id) {
                row.set("degrees_of_freedom", json!(sketch.degrees_of_freedom));
            }
            row
        })
        .collect();
    let mut out = json!({ "features": timeline, "body": body_summary(&session.kernel, part) });
    // What assemblies drive in the part (SolidWorks' external references).
    if !part.driven_by().is_empty() {
        out.set(
            "driven_by",
            Value::Array(
                part.driven_by()
                    .iter()
                    .map(|d| json!({ "name": d.name, "assembly": d.assembly }))
                    .collect(),
            ),
        );
    }
    if reference {
        out.set(
            "path",
            json!(session.reference()?.path.display().to_string()),
        );
    }
    if let Some(body) = part.body() {
        let names: Vec<Value> = body
            .solids
            .iter()
            .enumerate()
            .map(|(i, s)| {
                json!({ "name": format!("Body {}", i + 1),
                    "made_by": part.feature(s.id.feature).map(|f| f.name().to_owned()),
                    "volume": session.kernel_volume(s.shape) })
            })
            .collect();
        out.set("bodies", Value::Array(names));
    }
    if args::bool(args, "dimensions", false)? {
        out.set("dimensions", Value::Array(
            part.dimensions()
                .into_iter()
                .map(|d| {
                    let mut row = json!({ "name": d.full_name, "value": args::round(d.unit.to_display(d.value)),
                        "unit": d.unit.suffix().trim(), "driven": d.driven });
                    if let Some(assembly) = &d.driven_by {
                        row.set("driven_by", json!(assembly));
                    }
                    row
                })
                .collect(),
        ));
        let globals: Vec<Value> = part
            .definition()
            .equations_text()
            .globals
            .into_iter()
            .map(|g| json!({ "name": g.name, "expression": g.expression }))
            .collect();
        if !globals.is_empty() {
            out.set("variables", Value::Array(globals));
        }
    }
    if !reference && let Some(doc) = &session.part {
        out.set(
            "path",
            json!(doc.path.as_ref().map(|p| p.display().to_string())),
        );
        let problems = status_report(&doc.part, &[]);
        if let Some(p) = problems.get("other_problems") {
            out.set("problems", p.clone());
        }
    }
    Ok(ToolOutput::json(&out))
}

impl Session {
    fn kernel_volume(&self, shape: oxyde_kernel::ShapeHandle) -> f64 {
        use oxyde_kernel::Kernel;
        args::round(self.kernel.volume(shape).unwrap_or(0.0))
    }
}
