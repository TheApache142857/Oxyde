//! Writing shapes to a STEP file (AP214, the `automotive_design` schema), as OpenCASCADE
//! writes them (observed as a black box: what it writes for the same shapes): a product for
//! each shape, its solids as `MANIFOLD_SOLID_BREP`s (`BREP_WITH_VOIDS` round a void) in an
//! `ADVANCED_BREP_SHAPE_REPRESENTATION`, in millimetres; each solid's and face's colour a
//! `STYLED_ITEM` (a face's overriding its solid's), each product's material its `material
//! name` and `density` properties.
//!
//! Planes, cylinders, cones, spheres, tori and B-spline surfaces are written as they are, so
//! are lines, circles and B-spline curves; an ellipse (a plane's section of a cylinder) as
//! an ellipse; any other section, and a traced curve, as a cubic B-spline within 1e-7 mm of
//! it (there is no exact form for them in STEP). Degenerate edges (at a pole, at an apex) are
//! left out of the loops, as OpenCASCADE leaves them; a reader puts them back.

use std::collections::HashMap;

use oxyde_geom::{Point3, Vec3};
use oxyde_kernel::{StepColor, StepColors, StepMaterial};

use super::p21::{Param, Writer};
use crate::NativeError;
use crate::brep::{Edge, Shape, Solid};
use crate::geometry::{Curve, Section, SplineCurve, SplineSurface, Surface, perpendicular};
use crate::nurbs::{BSplineCurve, BSplineSurface};
use crate::revolution::{Meridian, Revolution};

/// How close a curve STEP cannot carry exactly is written to it.
const FIT: f64 = 1e-7;

fn point(w: &mut Writer, p: Point3) -> u64 {
    w.add(
        "CARTESIAN_POINT",
        vec![Param::str(""), Param::reals(&[p.x, p.y, p.z])],
    )
}

fn direction(w: &mut Writer, v: Vec3) -> u64 {
    let v = v.normalized().unwrap_or(Vec3::Z);
    w.add(
        "DIRECTION",
        vec![Param::str(""), Param::reals(&[v.x, v.y, v.z])],
    )
}

fn placement(w: &mut Writer, origin: Point3, z: Vec3, x: Vec3) -> u64 {
    let (o, z) = (point(w, origin), direction(w, z));
    let x = direction(w, x);
    w.add(
        "AXIS2_PLACEMENT_3D",
        vec![Param::str(""), Param::Ref(o), Param::Ref(z), Param::Ref(x)],
    )
}

/// The distinct knots and their multiplicities.
fn knots_and_multiplicities(knots: &[f64]) -> (Vec<f64>, Vec<i64>) {
    let mut values: Vec<f64> = Vec::new();
    let mut counts: Vec<i64> = Vec::new();
    for &k in knots {
        match values.last() {
            Some(&last) if last == k => {
                if let Some(c) = counts.last_mut() {
                    *c += 1;
                }
            }
            _ => {
                values.push(k);
                counts.push(1);
            }
        }
    }
    (values, counts)
}

fn ints(values: &[i64]) -> Param {
    Param::List(values.iter().map(|&i| Param::Integer(i)).collect())
}

/// A B-spline curve's entity: `B_SPLINE_CURVE_WITH_KNOTS`, or the complex instance of a
/// rational one.
fn bspline_curve(w: &mut Writer, c: &BSplineCurve) -> u64 {
    let poles: Vec<u64> = c.poles.iter().map(|&p| point(w, p)).collect();
    let (knots, mults) = knots_and_multiplicities(&c.knots);
    let closed = c.is_closed();
    match &c.weights {
        None => w.add(
            "B_SPLINE_CURVE_WITH_KNOTS",
            vec![
                Param::str(""),
                Param::Integer(c.degree as i64),
                Param::refs(&poles),
                Param::enumerated("UNSPECIFIED"),
                Param::bool(closed),
                Param::bool(false),
                ints(&mults),
                Param::reals(&knots),
                Param::enumerated("UNSPECIFIED"),
            ],
        ),
        Some(weights) => w.add_complex(vec![
            ("BOUNDED_CURVE", vec![]),
            (
                "B_SPLINE_CURVE",
                vec![
                    Param::Integer(c.degree as i64),
                    Param::refs(&poles),
                    Param::enumerated("UNSPECIFIED"),
                    Param::bool(closed),
                    Param::bool(false),
                ],
            ),
            (
                "B_SPLINE_CURVE_WITH_KNOTS",
                vec![
                    ints(&mults),
                    Param::reals(&knots),
                    Param::enumerated("UNSPECIFIED"),
                ],
            ),
            ("CURVE", vec![]),
            ("GEOMETRIC_REPRESENTATION_ITEM", vec![]),
            ("RATIONAL_B_SPLINE_CURVE", vec![Param::reals(weights)]),
            ("REPRESENTATION_ITEM", vec![Param::str("")]),
        ]),
    }
}

fn bspline_surface(w: &mut Writer, s: &BSplineSurface) -> u64 {
    let [nu, nv] = s.count;
    let rows: Vec<Param> = (0..nu)
        .map(|i| {
            let ids: Vec<u64> = (0..nv).map(|j| point(w, s.poles[i * nv + j])).collect();
            Param::refs(&ids)
        })
        .collect();
    let (ku, mu) = knots_and_multiplicities(&s.knots[0]);
    let (kv, mv) = knots_and_multiplicities(&s.knots[1]);
    let (cu, cv) = (s.closes(0), s.closes(1));
    match &s.weights {
        None => w.add(
            "B_SPLINE_SURFACE_WITH_KNOTS",
            vec![
                Param::str(""),
                Param::Integer(s.degree[0] as i64),
                Param::Integer(s.degree[1] as i64),
                Param::List(rows),
                Param::enumerated("UNSPECIFIED"),
                Param::bool(cu),
                Param::bool(cv),
                Param::bool(false),
                ints(&mu),
                ints(&mv),
                Param::reals(&ku),
                Param::reals(&kv),
                Param::enumerated("UNSPECIFIED"),
            ],
        ),
        Some(weights) => {
            let wrows: Vec<Param> = (0..nu)
                .map(|i| Param::reals(&weights[i * nv..(i + 1) * nv]))
                .collect();
            w.add_complex(vec![
                ("BOUNDED_SURFACE", vec![]),
                (
                    "B_SPLINE_SURFACE",
                    vec![
                        Param::Integer(s.degree[0] as i64),
                        Param::Integer(s.degree[1] as i64),
                        Param::List(rows),
                        Param::enumerated("UNSPECIFIED"),
                        Param::bool(cu),
                        Param::bool(cv),
                        Param::bool(false),
                    ],
                ),
                (
                    "B_SPLINE_SURFACE_WITH_KNOTS",
                    vec![
                        ints(&mu),
                        ints(&mv),
                        Param::reals(&ku),
                        Param::reals(&kv),
                        Param::enumerated("UNSPECIFIED"),
                    ],
                ),
                ("GEOMETRIC_REPRESENTATION_ITEM", vec![]),
                ("RATIONAL_B_SPLINE_SURFACE", vec![Param::List(wrows)]),
                ("REPRESENTATION_ITEM", vec![Param::str("")]),
                ("SURFACE", vec![]),
            ])
        }
    }
}

/// A B-spline curve's data where the kernel places it.
fn placed_curve(c: &SplineCurve) -> BSplineCurve {
    let d = &c.data.curve;
    BSplineCurve {
        poles: d.poles.iter().map(|&p| c.place.apply_point(p)).collect(),
        ..d.clone()
    }
}

fn placed_surface(s: &SplineSurface) -> BSplineSurface {
    let d = &s.data.surface;
    BSplineSurface {
        poles: d.poles.iter().map(|&p| s.place.apply_point(p)).collect(),
        ..d.clone()
    }
}

/// An ellipse's centre, normal (the way it runs round), major direction and semi-axes, of a
/// plane's section of a cylinder.
fn ellipse_of(s: &Section) -> Option<(Point3, Vec3, Vec3, f64, f64)> {
    let Surface::Plane(plane) = s.other else {
        return None;
    };
    let r = s.carrier.cylinder_radius()?.abs();
    let a = s.carrier.axis.direction;
    let n = plane.normal;
    let cos = a.dot(n);
    if cos.abs() < 1e-12 {
        return None;
    }
    let t = (plane.origin - s.carrier.axis.origin).dot(n) / cos;
    let center = s.carrier.axis.origin + a * t;
    // The axis's shadow on the plane is the major direction.
    let major = (a - n * cos)
        .normalized()
        .unwrap_or_else(|| perpendicular(n));
    let semi_major = r / cos.abs();
    // The section runs as the carrier's angle grows when `ccw`: about the axis, so about
    // the plane's normal turned the axis's way.
    let about = if cos > 0.0 { n } else { -n };
    let about = if s.ccw { about } else { -about };
    Some((center, about, major, semi_major, r))
}

/// An edge's curve entity, and whether the edge runs the curve's way.
fn curve_entity(w: &mut Writer, solid: &Solid, edge: &Edge) -> Result<(u64, bool), NativeError> {
    let (a, b) = (solid.point(edge.start), solid.point(edge.end));
    Ok(match edge.curve {
        Curve::Line => {
            let p = point(w, a);
            let d = direction(w, b - a);
            let v = w.add(
                "VECTOR",
                vec![
                    Param::str(""),
                    Param::Ref(d),
                    Param::Real(a.distance(b).max(1e-12)),
                ],
            );
            (
                w.add("LINE", vec![Param::str(""), Param::Ref(p), Param::Ref(v)]),
                true,
            )
        }
        Curve::Circle(c) => {
            let x = (a - c.center)
                .normalized()
                .unwrap_or_else(|| perpendicular(c.normal));
            let x = (x - c.normal * x.dot(c.normal)).normalized().unwrap_or(x);
            let pl = placement(w, c.center, c.normal, x);
            (
                w.add(
                    "CIRCLE",
                    vec![Param::str(""), Param::Ref(pl), Param::Real(c.radius)],
                ),
                true,
            )
        }
        Curve::Section(s) if s.is_ellipse() => match ellipse_of(&s) {
            Some((center, about, major, sa, sb)) => {
                let pl = placement(w, center, about, major);
                (
                    w.add(
                        "ELLIPSE",
                        vec![
                            Param::str(""),
                            Param::Ref(pl),
                            Param::Real(sa),
                            Param::Real(sb),
                        ],
                    ),
                    true,
                )
            }
            None => (fitted(w, solid, edge)?, true),
        },
        Curve::Section(_) | Curve::Traced(_) => (fitted(w, solid, edge)?, true),
        Curve::Spline(c) => (bspline_curve(w, &placed_curve(&c)), c.up),
        Curve::Degenerate { .. } => {
            return Err(NativeError::Internal(
                "a degenerate edge written as a curve".to_owned(),
            ));
        }
    })
}

/// A cubic B-spline through an edge, from its start to its end.
fn fitted(w: &mut Writer, solid: &Solid, edge: &Edge) -> Result<u64, NativeError> {
    let (a, b) = (solid.point(edge.start), solid.point(edge.end));
    let extent = edge.curve.extent(a, b, edge.is_closed());
    let at = |s: f64| edge.curve.at(a, b, s).unwrap_or(a);
    let size = a.to_vec().length().max(b.to_vec().length()).max(1.0);
    let curve = crate::nurbs::fit(&at, extent, FIT * size)
        .ok_or_else(|| NativeError::Internal("an edge no B-spline follows".to_owned()))?;
    Ok(bspline_curve(w, &curve))
}

/// A face's surface entity, and whether the face's normal is the surface's own.
fn surface_entity(w: &mut Writer, surface: &Surface) -> (u64, bool) {
    match surface {
        Surface::Plane(p) => {
            let pl = placement(w, p.origin, p.normal, p.x_dir);
            (w.add("PLANE", vec![Param::str(""), Param::Ref(pl)]), true)
        }
        Surface::Revolution(r) => (revolution_entity(w, r), r.outward),
        Surface::Spline(s) => (bspline_surface(w, &placed_surface(s)), s.outward),
    }
}

fn revolution_entity(w: &mut Writer, r: &Revolution) -> u64 {
    let (x, _, a) = r.frame();
    match r.meridian {
        Meridian::Line { radius, slope: 0.0 } => {
            let pl = placement(w, r.axis.origin, a, x);
            w.add(
                "CYLINDRICAL_SURFACE",
                vec![Param::str(""), Param::Ref(pl), Param::Real(radius.abs())],
            )
        }
        Meridian::Line { radius, slope } => {
            // STEP's cone opens along its axis from a radius at its origin: the axis turned
            // round where the meridian narrows the axis's way, the origin moved to where the
            // radius is not negative.
            let (axis, s) = if slope > 0.0 {
                (a, slope)
            } else {
                (-a, -slope)
            };
            let v0 = if radius >= 0.0 { 0.0 } else { -radius / slope };
            let origin = r.axis.origin + a * v0;
            let rho = (radius + slope * v0).max(0.0);
            let pl = placement(w, origin, axis, x);
            w.add(
                "CONICAL_SURFACE",
                vec![
                    Param::str(""),
                    Param::Ref(pl),
                    Param::Real(rho),
                    Param::Real(s.atan()),
                ],
            )
        }
        Meridian::Circle { center, radius } => {
            let pl = placement(w, r.axis.origin, a, x);
            if center.abs() <= 1e-12 {
                w.add(
                    "SPHERICAL_SURFACE",
                    vec![Param::str(""), Param::Ref(pl), Param::Real(radius)],
                )
            } else if center > radius {
                w.add(
                    "TOROIDAL_SURFACE",
                    vec![
                        Param::str(""),
                        Param::Ref(pl),
                        Param::Real(center),
                        Param::Real(radius),
                    ],
                )
            } else {
                // A spindle torus: its outer part (its apple), or for a meridian circle
                // centred across the axis its inner part (its lemon).
                w.add(
                    "DEGENERATE_TOROIDAL_SURFACE",
                    vec![
                        Param::str(""),
                        Param::Ref(pl),
                        Param::Real(center.abs()),
                        Param::Real(radius),
                        Param::bool(center >= 0.0),
                    ],
                )
            }
        }
    }
}

/// The colour's style: a fill colour, and how see-through it is.
fn style(w: &mut Writer, c: &StepColor) -> u64 {
    let [r, g, b] = c.rgb.map(|v| f64::from(v) / 255.0);
    let colour = w.add(
        "COLOUR_RGB",
        vec![
            Param::str(""),
            Param::Real(r),
            Param::Real(g),
            Param::Real(b),
        ],
    );
    let fac = w.add(
        "FILL_AREA_STYLE_COLOUR",
        vec![Param::str(""), Param::Ref(colour)],
    );
    let fas = w.add("FILL_AREA_STYLE", vec![Param::str(""), Param::refs(&[fac])]);
    let ssfa = w.add("SURFACE_STYLE_FILL_AREA", vec![Param::Ref(fas)]);
    let mut sides = vec![ssfa];
    if c.transparency > 0.0 {
        let t = w.add(
            "SURFACE_STYLE_TRANSPARENT",
            vec![Param::Real(c.transparency)],
        );
        sides.push(w.add(
            "SURFACE_STYLE_RENDERING_WITH_PROPERTIES",
            vec![
                Param::enumerated("NORMAL_SHADING"),
                Param::Ref(colour),
                Param::refs(&[t]),
            ],
        ));
    }
    let sss = w.add(
        "SURFACE_SIDE_STYLE",
        vec![Param::str(""), Param::refs(&sides)],
    );
    let usage = w.add(
        "SURFACE_STYLE_USAGE",
        vec![Param::enumerated("BOTH"), Param::Ref(sss)],
    );
    w.add("PRESENTATION_STYLE_ASSIGNMENT", vec![Param::refs(&[usage])])
}

/// One solid's entities: its `MANIFOLD_SOLID_BREP` (or `BREP_WITH_VOIDS`), and each face's.
fn solid_entities(w: &mut Writer, solid: &Solid) -> Result<(u64, Vec<u64>), NativeError> {
    let vertices: Vec<u64> = solid
        .vertices
        .iter()
        .map(|v| {
            let p = point(w, v.point);
            w.add("VERTEX_POINT", vec![Param::str(""), Param::Ref(p)])
        })
        .collect();
    let mut edges: HashMap<u32, u64> = HashMap::new();
    for (i, edge) in solid.edges.iter().enumerate() {
        if edge.curve.is_degenerate() {
            continue;
        }
        let (curve, same) = curve_entity(w, solid, edge)?;
        // An edge always runs from its start vertex to its end one; the curve's way is the
        // flag.
        let id = w.add(
            "EDGE_CURVE",
            vec![
                Param::str(""),
                Param::Ref(vertices[edge.start as usize]),
                Param::Ref(vertices[edge.end as usize]),
                Param::Ref(curve),
                Param::bool(same),
            ],
        );
        edges.insert(i as u32, id);
    }
    let mut faces = Vec::with_capacity(solid.faces.len());
    for face in &solid.faces {
        let mut bounds = Vec::new();
        for (k, uses) in face.loops.iter().enumerate() {
            let oriented: Vec<u64> = uses
                .iter()
                .filter_map(|u| {
                    edges.get(&u.edge).map(|&e| {
                        w.add(
                            "ORIENTED_EDGE",
                            vec![
                                Param::str(""),
                                Param::Derived,
                                Param::Derived,
                                Param::Ref(e),
                                Param::bool(!u.reversed),
                            ],
                        )
                    })
                })
                .collect();
            let lp = if oriented.is_empty() {
                // Only degenerate edges: a loop round a single vertex.
                let v = uses
                    .first()
                    .map(|u| vertices[solid.edges[u.edge as usize].start as usize])
                    .ok_or_else(|| NativeError::Internal("an empty loop".to_owned()))?;
                w.add("VERTEX_LOOP", vec![Param::str(""), Param::Ref(v)])
            } else {
                w.add("EDGE_LOOP", vec![Param::str(""), Param::refs(&oriented)])
            };
            let kind = if k == 0 {
                "FACE_OUTER_BOUND"
            } else {
                "FACE_BOUND"
            };
            bounds.push(w.add(
                kind,
                vec![Param::str(""), Param::Ref(lp), Param::bool(true)],
            ));
        }
        let (surface, same) = surface_entity(w, &face.surface);
        faces.push(w.add(
            "ADVANCED_FACE",
            vec![
                Param::str(""),
                Param::refs(&bounds),
                Param::Ref(surface),
                Param::bool(same),
            ],
        ));
    }
    let shells: Vec<Vec<u32>> = if solid.shells.is_empty() {
        vec![(0..solid.faces.len() as u32).collect()]
    } else {
        solid.shells.clone()
    };
    let shell_ids: Vec<u64> = shells
        .iter()
        .map(|fs| {
            let ids: Vec<u64> = fs.iter().map(|&f| faces[f as usize]).collect();
            w.add("CLOSED_SHELL", vec![Param::str(""), Param::refs(&ids)])
        })
        .collect();
    let brep = if shell_ids.len() == 1 {
        w.add(
            "MANIFOLD_SOLID_BREP",
            vec![Param::str(""), Param::Ref(shell_ids[0])],
        )
    } else {
        // A void's faces point into it, out of the solid: its shell as written, which a
        // reader takes as it stands.
        let voids: Vec<u64> = shell_ids[1..]
            .iter()
            .map(|&s| {
                w.add(
                    "ORIENTED_CLOSED_SHELL",
                    vec![
                        Param::str(""),
                        Param::Derived,
                        Param::Ref(s),
                        Param::bool(true),
                    ],
                )
            })
            .collect();
        w.add(
            "BREP_WITH_VOIDS",
            vec![
                Param::str(""),
                Param::Ref(shell_ids[0]),
                Param::refs(&voids),
            ],
        )
    };
    Ok((brep, faces))
}

/// The STEP file of `shapes`, each a product of its own, `names` naming them.
pub(crate) fn write(
    shapes: &[&Shape],
    names: &[String],
    colors: &[StepColors],
    materials: &[Option<StepMaterial>],
) -> Result<String, NativeError> {
    let mut w = Writer::new();
    let app = w.add(
        "APPLICATION_CONTEXT",
        vec![Param::str(
            "core data for automotive mechanical design processes",
        )],
    );
    w.add(
        "APPLICATION_PROTOCOL_DEFINITION",
        vec![
            Param::str("international standard"),
            Param::str("automotive_design"),
            Param::Integer(2000),
            Param::Ref(app),
        ],
    );
    let product_context = w.add(
        "PRODUCT_CONTEXT",
        vec![Param::str(""), Param::Ref(app), Param::str("mechanical")],
    );
    let definition_context = w.add(
        "PRODUCT_DEFINITION_CONTEXT",
        vec![
            Param::str("part definition"),
            Param::Ref(app),
            Param::str("design"),
        ],
    );
    // Millimetres, radians, steradians; 1e-7 mm apart is one point.
    let mm = w.add_complex(vec![
        ("LENGTH_UNIT", vec![]),
        ("NAMED_UNIT", vec![Param::Derived]),
        (
            "SI_UNIT",
            vec![Param::enumerated("MILLI"), Param::enumerated("METRE")],
        ),
    ]);
    let rad = w.add_complex(vec![
        ("NAMED_UNIT", vec![Param::Derived]),
        ("PLANE_ANGLE_UNIT", vec![]),
        ("SI_UNIT", vec![Param::Unset, Param::enumerated("RADIAN")]),
    ]);
    let sr = w.add_complex(vec![
        ("NAMED_UNIT", vec![Param::Derived]),
        (
            "SI_UNIT",
            vec![Param::Unset, Param::enumerated("STERADIAN")],
        ),
        ("SOLID_ANGLE_UNIT", vec![]),
    ]);
    let uncertainty = w.add(
        "UNCERTAINTY_MEASURE_WITH_UNIT",
        vec![
            Param::Typed("LENGTH_MEASURE".to_owned(), Box::new(Param::Real(1e-7))),
            Param::Ref(mm),
            Param::str("distance_accuracy_value"),
            Param::str("confusion accuracy"),
        ],
    );
    let context = w.add_complex(vec![
        ("GEOMETRIC_REPRESENTATION_CONTEXT", vec![Param::Integer(3)]),
        (
            "GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT",
            vec![Param::refs(&[uncertainty])],
        ),
        (
            "GLOBAL_UNIT_ASSIGNED_CONTEXT",
            vec![Param::refs(&[mm, rad, sr])],
        ),
        (
            "REPRESENTATION_CONTEXT",
            vec![
                Param::str("Context #1"),
                Param::str("3D Context with UNIT and UNCERTAINTY"),
            ],
        ),
    ]);
    let mut styled: Vec<u64> = Vec::new();
    for (k, shape) in shapes.iter().enumerate() {
        let name = names.get(k).map_or("SOLID", String::as_str);
        let product = w.add(
            "PRODUCT",
            vec![
                Param::str(name),
                Param::str(name),
                Param::str(""),
                Param::refs(&[product_context]),
            ],
        );
        let formation = w.add(
            "PRODUCT_DEFINITION_FORMATION",
            vec![Param::str(""), Param::str(""), Param::Ref(product)],
        );
        let definition = w.add(
            "PRODUCT_DEFINITION",
            vec![
                Param::str("design"),
                Param::str(""),
                Param::Ref(formation),
                Param::Ref(definition_context),
            ],
        );
        let definition_shape = w.add(
            "PRODUCT_DEFINITION_SHAPE",
            vec![Param::str(""), Param::str(""), Param::Ref(definition)],
        );
        let origin = placement(&mut w, Point3::ORIGIN, Vec3::Z, Vec3::X);
        let mut items = vec![origin];
        let colors_here = colors.get(k);
        let mut face_offset = 0usize;
        for (s, solid) in shape.solids.iter().enumerate() {
            let (brep, faces) = solid_entities(&mut w, solid)?;
            items.push(brep);
            if let Some(c) = colors_here {
                let solid_style = c.solids.get(s).copied().flatten().map(|color| {
                    let psa = style(&mut w, &color);
                    w.add(
                        "STYLED_ITEM",
                        vec![Param::str("color"), Param::refs(&[psa]), Param::Ref(brep)],
                    )
                });
                if let Some(id) = solid_style {
                    styled.push(id);
                }
                for (f, &face) in faces.iter().enumerate() {
                    let Some(Some(color)) = c.faces.get(face_offset + f) else {
                        continue;
                    };
                    let psa = style(&mut w, color);
                    styled.push(match solid_style {
                        Some(over) => w.add(
                            "OVER_RIDING_STYLED_ITEM",
                            vec![
                                Param::str("overriding color"),
                                Param::refs(&[psa]),
                                Param::Ref(face),
                                Param::Ref(over),
                            ],
                        ),
                        None => w.add(
                            "STYLED_ITEM",
                            vec![Param::str("color"), Param::refs(&[psa]), Param::Ref(face)],
                        ),
                    });
                }
            }
            face_offset += solid.faces.len();
        }
        let rep = w.add(
            "ADVANCED_BREP_SHAPE_REPRESENTATION",
            vec![Param::str(""), Param::refs(&items), Param::Ref(context)],
        );
        w.add(
            "SHAPE_DEFINITION_REPRESENTATION",
            vec![Param::Ref(definition_shape), Param::Ref(rep)],
        );
        w.add(
            "PRODUCT_RELATED_PRODUCT_CATEGORY",
            vec![Param::str("part"), Param::Unset, Param::refs(&[product])],
        );
        if let Some(Some(m)) = materials.get(k) {
            write_material(&mut w, m, definition, context);
        }
    }
    if !styled.is_empty() {
        w.add(
            "MECHANICAL_DESIGN_GEOMETRIC_PRESENTATION_REPRESENTATION",
            vec![Param::str(""), Param::refs(&styled), Param::Ref(context)],
        );
    }
    Ok(w.finish(
        "Oxyde Model",
        names.first().map_or("Oxyde", String::as_str),
        "AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }",
    ))
}

/// The `material name` and `density` properties of a product's definition, as OpenCASCADE
/// writes them (its density in g/cm³ as kg/m³ reads, the unit as it writes it).
fn write_material(w: &mut Writer, m: &StepMaterial, definition: u64, context: u64) {
    let item = w.add(
        "DESCRIPTIVE_REPRESENTATION_ITEM",
        vec![Param::str(&m.name), Param::str(&m.description)],
    );
    let name = w.add(
        "REPRESENTATION",
        vec![
            Param::str("material name"),
            Param::refs(&[item]),
            Param::Ref(context),
        ],
    );
    let gram = w.add_complex(vec![
        ("MASS_UNIT", vec![]),
        ("NAMED_UNIT", vec![Param::Derived]),
        ("SI_UNIT", vec![Param::Unset, Param::enumerated("GRAM")]),
    ]);
    let g = w.add(
        "DERIVED_UNIT_ELEMENT",
        vec![Param::Ref(gram), Param::Real(3.0)],
    );
    let cm = w.add_complex(vec![
        ("LENGTH_UNIT", vec![]),
        ("NAMED_UNIT", vec![Param::Derived]),
        (
            "SI_UNIT",
            vec![Param::enumerated("CENTI"), Param::enumerated("METRE")],
        ),
    ]);
    let c = w.add(
        "DERIVED_UNIT_ELEMENT",
        vec![Param::Ref(cm), Param::Real(2.0)],
    );
    let unit = w.add("DERIVED_UNIT", vec![Param::refs(&[g, c])]);
    let measure = w.add(
        "MEASURE_REPRESENTATION_ITEM",
        vec![
            Param::str("density measure"),
            Param::Typed(
                "POSITIVE_RATIO_MEASURE".to_owned(),
                Box::new(Param::Real(m.density)),
            ),
            Param::Ref(unit),
        ],
    );
    let density = w.add(
        "REPRESENTATION",
        vec![
            Param::str("density"),
            Param::refs(&[measure]),
            Param::Ref(context),
        ],
    );
    for (what, rep) in [("material name", name), ("density", density)] {
        let pd = w.add(
            "PROPERTY_DEFINITION",
            vec![
                Param::str("material property"),
                Param::str(what),
                Param::Ref(definition),
            ],
        );
        w.add(
            "PROPERTY_DEFINITION_REPRESENTATION",
            vec![Param::Ref(pd), Param::Ref(rep)],
        );
    }
}
