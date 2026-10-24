"""Writes the STEP files the native kernel's reader is tested on: small solids written the
ways other systems write them, not the way OpenCASCADE does (whose own files the tests
make as they go). Run from this directory: python3 make.py."""
import math

R2 = math.sqrt(0.5)


class Step:
    def __init__(self, units="mm", accuracy=None):
        self.lines = []
        self.n = 0
        self.units = units
        # The accuracy the file states for its geometry, in its length unit: 1e-6 unless given.
        self.accuracy = "1.E-06" if accuracy is None else repr(float(accuracy))

    def add(self, text):
        self.n += 1
        self.lines.append(f"#{self.n}={text};")
        return f"#{self.n}"

    def pt(self, p):
        return self.add("CARTESIAN_POINT('',(%s))" % ",".join(repr(float(x)) for x in p))

    def dir(self, d):
        return self.add("DIRECTION('',(%s))" % ",".join(repr(float(x)) for x in d))

    def place(self, o, z=(0, 0, 1), x=(1, 0, 0)):
        return self.add(f"AXIS2_PLACEMENT_3D('',{self.pt(o)},{self.dir(z)},{self.dir(x)})")

    def vertex(self, p):
        return self.add(f"VERTEX_POINT('',{self.pt(p)})")

    def line(self, a, b):
        d = [b[i] - a[i] for i in range(3)]
        l = math.sqrt(sum(x * x for x in d))
        v = self.add(f"VECTOR('',{self.dir([x / l for x in d])},{l!r})")
        return self.add(f"LINE('',{self.pt(a)},{v})")

    def circle(self, c, r, z=(0, 0, 1), x=(1, 0, 0)):
        return self.add(f"CIRCLE('',{self.place(c, z, x)},{float(r)!r})")

    def edge(self, a, b, curve, same=True):
        return self.add(f"EDGE_CURVE('',{a},{b},{curve},{'.T.' if same else '.F.'})")

    def loop(self, uses):
        oes = [self.add(f"ORIENTED_EDGE('',*,*,{e},{'.T.' if f else '.F.'})") for e, f in uses]
        return self.add("EDGE_LOOP('',(%s))" % ",".join(oes))

    def bound(self, lp, outer=False, sense=True):
        kind = "FACE_OUTER_BOUND" if outer else "FACE_BOUND"
        return self.add(f"{kind}('',{lp},{'.T.' if sense else '.F.'})")

    def vertex_loop(self, v):
        return self.add(f"FACE_BOUND('',{self.add(f'VERTEX_LOOP({chr(39)}{chr(39)},{v})')},.T.)")

    def face(self, bounds, surface, same=True):
        return self.add("ADVANCED_FACE('',(%s),%s,%s)" % (",".join(bounds), surface, ".T." if same else ".F."))

    def context(self):
        if self.units == "inch":
            mm = self.add("( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT($,.METRE.) )")
            m = self.add(f"LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(0.0254),{mm})")
            length = self.add(f"( CONVERSION_BASED_UNIT('INCH',{m}) LENGTH_UNIT() NAMED_UNIT(*) )")
            rad = self.add("( NAMED_UNIT(*) PLANE_ANGLE_UNIT() SI_UNIT($,.RADIAN.) )")
            dm = self.add(f"PLANE_ANGLE_MEASURE_WITH_UNIT(PLANE_ANGLE_MEASURE(0.0174532925199433),{rad})")
            angle = self.add(f"( CONVERSION_BASED_UNIT('DEGREE',{dm}) NAMED_UNIT(*) PLANE_ANGLE_UNIT() )")
        else:
            length = self.add("( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) )")
            angle = self.add("( NAMED_UNIT(*) PLANE_ANGLE_UNIT() SI_UNIT($,.RADIAN.) )")
        solid_angle = self.add("( NAMED_UNIT(*) SI_UNIT($,.STERADIAN.) SOLID_ANGLE_UNIT() )")
        u = self.add(
            f"UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE({self.accuracy}),{length},'distance_accuracy_value','')"
        )
        return self.add(
            f"( GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT(({u})) "
            f"GLOBAL_UNIT_ASSIGNED_CONTEXT(({length},{angle},{solid_angle})) REPRESENTATION_CONTEXT('',''))"
        )

    def product(self, name, app):
        pc = self.add(f"PRODUCT_CONTEXT('',{app},'mechanical')")
        p = self.add(f"PRODUCT('{name}','{name}','',({pc}))")
        pdf = self.add(f"PRODUCT_DEFINITION_FORMATION('','',{p})")
        pdc = self.add(f"PRODUCT_DEFINITION_CONTEXT('part definition',{app},'design')")
        pd = self.add(f"PRODUCT_DEFINITION('design','',{pdf},{pdc})")
        pds = self.add(f"PRODUCT_DEFINITION_SHAPE('','',{pd})")
        return pd, pds

    def app(self):
        app = self.add("APPLICATION_CONTEXT('automotive design')")
        self.add(f"APPLICATION_PROTOCOL_DEFINITION('international standard','automotive_design',2000,{app})")
        return app

    def part_file(self, name, faces):
        """One product holding one solid of these faces."""
        app = self.app()
        ctx = self.context()
        shell = self.add("CLOSED_SHELL('',(%s))" % ",".join(faces))
        brep = self.add(f"MANIFOLD_SOLID_BREP('{name}',{shell})")
        rep = self.add(f"ADVANCED_BREP_SHAPE_REPRESENTATION('',({brep},{self.place((0, 0, 0))}),{ctx})")
        pd, pds = self.product(name, app)
        self.add(f"SHAPE_DEFINITION_REPRESENTATION({pds},{rep})")
        return brep

    def text(self, name):
        name = name.replace("'", "''")
        head = (
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('" + name + "'),'2;1');\n"
            "FILE_NAME('" + name + "','2026-10-24T00:00:00',(''),(''),'oxyde tests','','');\n"
            "FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\nENDSEC;\nDATA;\n"
        )
        return head + "\n".join(self.lines) + "\nENDSEC;\nEND-ISO-10303-21;\n"


def seamless_cylinder():
    """A cylinder of radius 5 and height 10, its side bounded by its two circles alone,
    their vertices a quarter turn apart (as SolidWorks and Creo write a whole cylinder)."""
    s = Step()
    v0 = s.vertex((5, 0, 0))
    v1 = s.vertex((0, 5, 10))
    c0 = s.edge(v0, v0, s.circle((0, 0, 0), 5))
    c1 = s.edge(v1, v1, s.circle((0, 0, 10), 5))
    cyl = s.add(f"CYLINDRICAL_SURFACE('',{s.place((0, 0, 0))},5.)")
    bottom = s.add(f"PLANE('',{s.place((0, 0, 0), (0, 0, -1))})")
    top = s.add(f"PLANE('',{s.place((0, 0, 10))})")
    side = s.face([s.bound(s.loop([(c0, True)]), True), s.bound(s.loop([(c1, False)]))], cyl)
    f0 = s.face([s.bound(s.loop([(c0, False)]), True)], bottom)
    f1 = s.face([s.bound(s.loop([(c1, True)]), True)], top)
    s.part_file("cylinder", [side, f0, f1])
    return s.text("seamless cylinder")


def pointed_pin():
    """In inches and degrees: a pin of radius 0.25 from z = 0 to 1 with a 45° point below,
    the cone bounded by its circle and a vertex loop at its apex, the side by its circles."""
    s = Step("inch")
    v0 = s.vertex((0.25, 0, 0))
    v1 = s.vertex((0.25, 0, 1))
    apex = s.vertex((0, 0, -0.25))
    c0 = s.edge(v0, v0, s.circle((0, 0, 0), 0.25))
    c1 = s.edge(v1, v1, s.circle((0, 0, 1), 0.25))
    cyl = s.add(f"CYLINDRICAL_SURFACE('',{s.place((0, 0, 0))},0.25)")
    cone = s.add(f"CONICAL_SURFACE('',{s.place((0, 0, 0))},0.25,45.)")
    top = s.add(f"PLANE('',{s.place((0, 0, 1))})")
    side = s.face([s.bound(s.loop([(c0, True)]), True), s.bound(s.loop([(c1, False)]))], cyl)
    point = s.face([s.bound(s.loop([(c0, False)]), True), s.vertex_loop(apex)], cone)
    f1 = s.face([s.bound(s.loop([(c1, True)]), True)], top)
    s.part_file("pin", [side, point, f1])
    return s.text("pointed pin")


def loose_half_cylinder():
    """A half cylinder of radius 5 and height 10 (x >= 0), written as loosely as the file's
    stated accuracy of 0.01 mm allows, as Fusion 360 writes some faces: the top arc's centre
    5 microns off the axis (its own sweep a little more than the half turn about the axis,
    and 5e-3 off the cylinder, beyond the 1e-3 a reader takes untold), and one corner of the
    bottom arc 2e-5 above the other, the side's loop starting along a straight edge."""
    s = Step(accuracy=0.01)
    a = s.vertex((0, -5, 2e-5))
    b = s.vertex((0, 5, 0))
    c = s.vertex((0, 5, 10))
    d = s.vertex((0, -5, 10))
    bottom_arc = s.edge(a, b, s.circle((0, 0, 0), 5))
    top_arc = s.edge(d, c, s.circle((0.004, 0.003, 10), 5))
    right = s.edge(b, c, s.line((0, 5, 0), (0, 5, 10)))
    left = s.edge(a, d, s.line((0, -5, 0), (0, -5, 10)))
    bottom_flat = s.edge(b, a, s.line((0, 5, 0), (0, -5, 0)))
    top_flat = s.edge(c, d, s.line((0, 5, 10), (0, -5, 10)))
    cyl = s.add(f"CYLINDRICAL_SURFACE('',{s.place((0, 0, 0))},5.)")
    flat = s.add(f"PLANE('',{s.place((0, 0, 0), (-1, 0, 0), (0, 1, 0))})")
    bottom = s.add(f"PLANE('',{s.place((0, 0, 0), (0, 0, -1))})")
    top = s.add(f"PLANE('',{s.place((0, 0, 10))})")
    side = s.face(
        [s.bound(s.loop([(right, True), (top_arc, False), (left, False), (bottom_arc, True)]), True)], cyl
    )
    back = s.face(
        [s.bound(s.loop([(bottom_flat, True), (left, True), (top_flat, False), (right, False)]), True)], flat
    )
    f0 = s.face([s.bound(s.loop([(bottom_arc, False), (bottom_flat, False)]), True)], bottom)
    f1 = s.face([s.bound(s.loop([(top_arc, True), (top_flat, True)]), True)], top)
    s.part_file("half cylinder", [side, back, f0, f1])
    return s.text("loose half cylinder")


def hemisphere_across():
    """A hemisphere of radius 10 on the plane z = 0, its sphere's axis along x: its edge
    runs through both of the sphere's poles."""
    s = Step()
    v = s.vertex((0, 10, 0))
    c = s.edge(v, v, s.circle((0, 0, 0), 10))
    sphere = s.add(f"SPHERICAL_SURFACE('',{s.place((0, 0, 0), (1, 0, 0), (0, 1, 0))},10.)")
    plane = s.add(f"PLANE('',{s.place((0, 0, 0), (0, 0, -1))})")
    dome = s.face([s.bound(s.loop([(c, True)]), True)], sphere)
    base = s.face([s.bound(s.loop([(c, False)]), True)], plane)
    s.part_file("hemisphere", [dome, base])
    return s.text("hemisphere across its sphere's axis")


def rational_circle_poles(r, z=0.0):
    corners = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1), (1, 0)]
    return [(r * x, r * y, z) for x, y in corners], [1 if i % 2 == 0 else R2 for i in range(9)]


def spline_dome():
    """A hemisphere of radius 10 as the rational B-spline surface it is exactly (a circle
    turned along a quarter circle), on the plane z = 0, its edge a rational circle."""
    s = Step()
    r = 10.0
    circle, cw = rational_circle_poles(r)
    pts = [s.pt(p) for p in circle]
    curve = s.add(
        "( BOUNDED_CURVE() B_SPLINE_CURVE(2,(%s),.CIRCULAR_ARC.,.T.,.F.) "
        "B_SPLINE_CURVE_WITH_KNOTS((3,2,2,2,3),(0.,0.25,0.5,0.75,1.),.UNSPECIFIED.) CURVE() "
        "GEOMETRIC_REPRESENTATION_ITEM() RATIONAL_B_SPLINE_CURVE((%s)) REPRESENTATION_ITEM('') )"
        % (",".join(pts), ",".join(repr(float(w)) for w in cw))
    )
    v = s.vertex((r, 0, 0))
    e = s.edge(v, v, curve)
    # Poles: u round the circle, v from the equator to the pole along a quarter circle.
    meridian = [(1.0, 0.0, 1.0), (1.0, 1.0, R2), (0.0, 1.0, 1.0)]
    rows, weights = [], []
    for (x, y, _), wu in zip(*rational_circle_poles(1.0)):
        row, wrow = [], []
        for rho, z, wv in meridian:
            row.append(s.pt((r * rho * x, r * rho * y, r * z)))
            wrow.append(wu * wv)
        rows.append("(" + ",".join(row) + ")")
        weights.append("(" + ",".join(repr(float(w)) for w in wrow) + ")")
    surface = s.add(
        "( BOUNDED_SURFACE() B_SPLINE_SURFACE(2,2,(%s),.UNSPECIFIED.,.T.,.F.,.F.) "
        "B_SPLINE_SURFACE_WITH_KNOTS((3,2,2,2,3),(3,3),(0.,0.25,0.5,0.75,1.),(0.,1.),.UNSPECIFIED.) "
        "GEOMETRIC_REPRESENTATION_ITEM() RATIONAL_B_SPLINE_SURFACE((%s)) REPRESENTATION_ITEM('') SURFACE() )"
        % (",".join(rows), ",".join(weights))
    )
    plane = s.add(f"PLANE('',{s.place((0, 0, 0), (0, 0, -1))})")
    dome = s.face([s.bound(s.loop([(e, True)]), True)], surface)
    base = s.face([s.bound(s.loop([(e, False)]), True)], plane)
    s.part_file("dome", [dome, base])
    return s.text("B-spline dome")


def block_faces(s, size):
    """The six faces of a block from the origin to `size`."""
    (a, b, c) = size
    corners = {}
    for i in (0, 1):
        for j in (0, 1):
            for k in (0, 1):
                corners[(i, j, k)] = s.vertex((i * a, j * b, k * c))
    edges = {}

    def edge(p, q):
        key = (p, q)
        if (q, p) in edges:
            return edges[(q, p)], False
        if key not in edges:
            P = (p[0] * a, p[1] * b, p[2] * c)
            Q = (q[0] * a, q[1] * b, q[2] * c)
            edges[key] = s.edge(corners[p], corners[q], s.line(P, Q))
        return edges[key], True

    faces = []
    # Each face: its corners counter-clockwise seen from outside, its outward normal.
    quads = [
        ([(0, 0, 0), (0, 1, 0), (1, 1, 0), (1, 0, 0)], (0, 0, -1), (0, 0, 0)),
        ([(0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)], (0, 0, 1), (0, 0, c)),
        ([(0, 0, 0), (1, 0, 0), (1, 0, 1), (0, 0, 1)], (0, -1, 0), (0, 0, 0)),
        ([(0, 1, 0), (0, 1, 1), (1, 1, 1), (1, 1, 0)], (0, 1, 0), (0, b, 0)),
        ([(0, 0, 0), (0, 0, 1), (0, 1, 1), (0, 1, 0)], (-1, 0, 0), (0, 0, 0)),
        ([(1, 0, 0), (1, 1, 0), (1, 1, 1), (1, 0, 1)], (1, 0, 0), (a, 0, 0)),
    ]
    for ring, n, o in quads:
        uses = [edge(ring[i], ring[(i + 1) % 4]) for i in range(4)]
        x = (1, 0, 0) if abs(n[0]) < 0.5 else (0, 1, 0)
        plane = s.add(f"PLANE('',{s.place(o, n, x)})")
        faces.append(s.face([s.bound(s.loop(uses), True)], plane))
    return faces


def colour(s, rgb, item):
    c = s.add("COLOUR_RGB('',%r,%r,%r)" % tuple(float(x) for x in rgb))
    fill = s.add(f"FILL_AREA_STYLE_COLOUR('',{c})")
    area = s.add(f"FILL_AREA_STYLE('',({fill}))")
    ssfa = s.add(f"SURFACE_STYLE_FILL_AREA({area})")
    side = s.add(f"SURFACE_SIDE_STYLE('',({ssfa}))")
    usage = s.add(f"SURFACE_STYLE_USAGE(.BOTH.,{side})")
    psa = s.add(f"PRESENTATION_STYLE_ASSIGNMENT(({usage}))")
    return s.add(f"STYLED_ITEM('color',({psa}),{item})")


def assembly():
    """A block of 10 × 20 × 30 used twice in an assembly: once moved by (100, 0, 0), once
    turned a quarter about z and moved up by 50; the block red, made of steel (density in
    kg/m³, as Fusion 360 writes it), one of its faces blue."""
    s = Step()
    app = s.app()
    ctx = s.context()
    faces = block_faces(s, (10, 20, 30))
    shell = s.add("CLOSED_SHELL('',(%s))" % ",".join(faces))
    brep = s.add(f"MANIFOLD_SOLID_BREP('block',{shell})")
    origin = s.place((0, 0, 0))
    block_rep = s.add(f"ADVANCED_BREP_SHAPE_REPRESENTATION('block',({brep},{origin}),{ctx})")
    block_pd, block_pds = s.product("block", app)
    s.add(f"SHAPE_DEFINITION_REPRESENTATION({block_pds},{block_rep})")
    assy_origin = s.place((0, 0, 0))
    p1 = s.place((100, 0, 0))
    p2 = s.place((0, 0, 50), (0, 0, 1), (0, 1, 0))
    assy_rep = s.add(f"SHAPE_REPRESENTATION('assy',({assy_origin},{p1},{p2}),{ctx})")
    assy_pd, assy_pds = s.product("assy", app)
    s.add(f"SHAPE_DEFINITION_REPRESENTATION({assy_pds},{assy_rep})")
    for k, placement in enumerate([p1, p2]):
        nauo = s.add(f"NEXT_ASSEMBLY_USAGE_OCCURRENCE('block:{k + 1}','block {k + 1}','',{assy_pd},{block_pd},$)")
        pds = s.add(f"PRODUCT_DEFINITION_SHAPE('','',{nauo})")
        idt = s.add(f"ITEM_DEFINED_TRANSFORMATION('','',{origin},{placement})")
        rel = s.add(
            f"( REPRESENTATION_RELATIONSHIP('','',{block_rep},{assy_rep}) "
            f"REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION({idt}) SHAPE_REPRESENTATION_RELATIONSHIP() )"
        )
        s.add(f"CONTEXT_DEPENDENT_SHAPE_REPRESENTATION({rel},{pds})")
    styled = [colour(s, (1, 0, 0), brep), colour(s, (0, 0, 1), faces[1])]
    s.add("MECHANICAL_DESIGN_GEOMETRIC_PRESENTATION_REPRESENTATION('',(%s),%s)" % (",".join(styled), ctx))
    item = s.add("DESCRIPTIVE_REPRESENTATION_ITEM('Steel','AISI 1018')")
    name = s.add(f"REPRESENTATION('material name',({item}),{ctx})")
    kg = s.add("( MASS_UNIT() NAMED_UNIT(*) SI_UNIT(.KILO.,.GRAM.) )")
    m = s.add("( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT($,.METRE.) )")
    unit = s.add(f"DERIVED_UNIT(({s.add(f'DERIVED_UNIT_ELEMENT({kg},1.)')},{s.add(f'DERIVED_UNIT_ELEMENT({m},-3.)')}))")
    measure = s.add(f"MEASURE_REPRESENTATION_ITEM('density measure',POSITIVE_RATIO_MEASURE(7870.),{unit})")
    density = s.add(f"REPRESENTATION('density',({measure}),{ctx})")
    for what, rep in (("material name", name), ("density", density)):
        prop = s.add(f"PROPERTY_DEFINITION('material property','{what}',{block_pd})")
        s.add(f"PROPERTY_DEFINITION_REPRESENTATION({prop},{rep})")
    return s.text("assembly of two blocks")


for name, make in [
    ("seamless_cylinder", seamless_cylinder),
    ("pointed_pin", pointed_pin),
    ("hemisphere_across", hemisphere_across),
    ("loose_half_cylinder", loose_half_cylinder),
    ("spline_dome", spline_dome),
    ("assembly", assembly),
]:
    with open(f"{name}.step", "w") as f:
        f.write(make())
