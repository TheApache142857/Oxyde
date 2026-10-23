use oxyde_geom::{Point3, Vec3};
use oxyde_model::{StandardAxis, StandardPlane};

use crate::*;

fn placed(translation: Vec3, rotation: Vec3) -> Placement {
    Placement::IDENTITY.moved(rotation, Point3::ORIGIN, translation)
}

/// A fixed base and a second component, somewhere else and turned a little.
fn two() -> (Assembly, ComponentId, ComponentId) {
    let mut asm = Assembly::new();
    let a = asm.add_component(
        "Base",
        "base.oxyprt".into(),
        "a".into(),
        Placement::IDENTITY,
    );
    let b = asm.add_component(
        "Pin",
        "pin.oxyprt".into(),
        "b".into(),
        placed(Vec3::new(10.0, 20.0, 30.0), Vec3::new(0.2, -0.1, 0.3)),
    );
    (asm, a, b)
}

fn r(component: ComponentId, target: MateTarget) -> MateRef {
    MateRef::new(component, target)
}

fn run(asm: &mut Assembly) -> Solution {
    let solution = solve(asm, |m| resolve(&m.target, None), 100.0);
    asm.apply_solution(&solution);
    solution
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

fn is_identity(p: &Placement) -> bool {
    let i = Placement::IDENTITY;
    (0..3).all(|r| (0..3).all(|c| close(p.rotation[r][c], i.rotation[r][c])))
        && close(p.translation.length(), 0.0)
}

#[test]
fn names_and_the_first_fixed_component() {
    let (mut asm, a, b) = two();
    assert_eq!(asm.component(a).unwrap().name, "Base<1>");
    assert!(asm.component(a).unwrap().fixed);
    assert!(!asm.component(b).unwrap().fixed);
    let c = asm.add_component("Pin", "pin.oxyprt".into(), "b".into(), Placement::IDENTITY);
    assert_eq!(asm.component(c).unwrap().name, "Pin<2>");
    let m = asm.add_mate(
        MateKind::Coincident,
        r(a, MateTarget::Origin),
        r(c, MateTarget::Origin),
        true,
    );
    assert_eq!(asm.mate(m).unwrap().name, "Coincident1");
    assert_eq!(asm.remove_component(c), vec!["Coincident1".to_owned()]);
    assert!(asm.mates.is_empty());
}

#[test]
fn three_coincident_planes_fully_define_a_component() {
    let (mut asm, a, b) = two();
    for plane in [
        StandardPlane::Front,
        StandardPlane::Top,
        StandardPlane::Right,
    ] {
        asm.add_mate(
            MateKind::Coincident,
            r(a, MateTarget::Plane(plane)),
            r(b, MateTarget::Plane(plane)),
            true,
        );
    }
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    assert!(is_identity(&asm.component(b).unwrap().placement));
    assert_eq!(solution.degrees(b), Some(0));
    assert_eq!(solution.degrees(a), Some(0));
}

#[test]
fn concentric_leaves_sliding_and_turning() {
    let (mut asm, a, b) = two();
    asm.add_mate(
        MateKind::Concentric,
        r(a, MateTarget::Axis(StandardAxis::Z)),
        r(b, MateTarget::Axis(StandardAxis::Z)),
        true,
    );
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let p = asm.component(b).unwrap().placement;
    let axis = p.apply_vector(Vec3::Z);
    assert!(close(axis.cross(Vec3::Z).length(), 0.0));
    let origin = p.apply_point(Point3::ORIGIN);
    assert!(close(origin.x, 0.0) && close(origin.y, 0.0));
    assert_eq!(solution.degrees(b), Some(2));
}

#[test]
fn distance_between_planes() {
    let (mut asm, a, b) = two();
    asm.add_mate(
        MateKind::Distance(15.0),
        r(a, MateTarget::Plane(StandardPlane::Front)),
        r(b, MateTarget::Plane(StandardPlane::Front)),
        true,
    );
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let p = asm.component(b).unwrap().placement;
    assert!(close(p.apply_vector(Vec3::Z).dot(Vec3::Z), 1.0));
    // It stays on the side it was on.
    assert!(close(p.translation.z, 15.0), "{p:?}");
    assert_eq!(solution.degrees(b), Some(3));
}

#[test]
fn anti_aligned_faces_face_each_other() {
    let (mut asm, a, b) = two();
    asm.add_mate(
        MateKind::Coincident,
        r(a, MateTarget::Plane(StandardPlane::Front)),
        r(b, MateTarget::Plane(StandardPlane::Front)),
        false,
    );
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let p = asm.component(b).unwrap().placement;
    assert!(close(p.apply_vector(Vec3::Z).z, -1.0));
    assert!(close(p.translation.z, 0.0));
}

/// A pin standing in a hole, head on the base: concentric with the hole's axis, its
/// head's underside on the base's top. Then the coincident mate is flipped, which needs
/// the pin turned over while it stays on the axis.
#[test]
fn a_flipped_alignment_turns_the_part_over() {
    let (mut asm, a, b) = two();
    asm.component_mut(b).unwrap().placement = Placement::from_translation(Vec3::new(40.0, 0.0, 0.0));
    asm.add_mate(MateKind::Concentric, r(a, MateTarget::Axis(StandardAxis::Z)), r(b, MateTarget::Axis(StandardAxis::Z)), true);
    let head = asm.add_mate(
        MateKind::Coincident,
        r(a, MateTarget::Plane(StandardPlane::Front)),
        r(b, MateTarget::Plane(StandardPlane::Front)),
        true,
    );
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let p = asm.component(b).unwrap().placement;
    assert!(close(p.apply_vector(Vec3::Z).z, 1.0), "{p:?}");
    assert!(close(p.translation.x, 0.0) && close(p.translation.y, 0.0), "{p:?}");

    asm.mate_mut(head).unwrap().aligned = false;
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let p = asm.component(b).unwrap().placement;
    assert!(close(p.apply_vector(Vec3::Z).z, -1.0), "{p:?}");
    assert!(close(p.translation.x, 0.0) && close(p.translation.y, 0.0), "{p:?}");
    assert!(close(p.translation.z, 0.0), "{p:?}");

    // And back again.
    asm.mate_mut(head).unwrap().aligned = true;
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let p = asm.component(b).unwrap().placement;
    assert!(close(p.apply_vector(Vec3::Z).z, 1.0), "{p:?}");
}

/// The concentric axis lies across the mated plane: the half turn is about that axis,
/// so the pin keeps it.
#[test]
fn a_flipped_alignment_turns_about_the_axis_it_keeps() {
    let (mut asm, a, b) = side_by_side();
    asm.add_mate(MateKind::Concentric, x_axis(a), x_axis(b), true);
    let side = asm.add_mate(MateKind::Coincident, front(a), front(b), true);
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    asm.mate_mut(side).unwrap().aligned = false;
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let p = asm.component(b).unwrap().placement;
    assert!(close(p.apply_vector(Vec3::Z).z, -1.0), "{p:?}");
    assert!(close(p.apply_vector(Vec3::X).x.abs(), 1.0), "{p:?}");
    assert!(close(p.translation.y, 0.0) && close(p.translation.z, 0.0), "{p:?}");
}

#[test]
fn faces_of_solids_start_anti_aligned_when_square() {
    let plane = |normal: Vec3| Primitive::Plane {
        origin: Point3::ORIGIN,
        normal,
    };
    // Square: solids touch, reference planes take the closest (aligned).
    assert!(!default_alignment_between(&plane(Vec3::Z), &plane(Vec3::X), true));
    assert!(default_alignment_between(&plane(Vec3::Z), &plane(Vec3::X), false));
    // Clearly the same way: aligned either way.
    assert!(default_alignment_between(&plane(Vec3::Z), &plane(Vec3::Z), true));
    // Opposite: anti-aligned.
    assert!(!default_alignment_between(&plane(Vec3::Z), &plane(-Vec3::Z), true));
    assert!(!default_alignment_between(&plane(Vec3::Z), &plane(-Vec3::Z), false));
}

#[test]
fn conflicting_mates_are_reported() {
    let (mut asm, a, b) = two();
    let front = |c| r(c, MateTarget::Plane(StandardPlane::Front));
    let ok = asm.add_mate(MateKind::Coincident, front(a), front(b), true);
    let bad = asm.add_mate(MateKind::Distance(10.0), front(a), front(b), true);
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 1);
    // The newer mate is the one that cannot hold; the first still does.
    assert_eq!(solution.status(ok), Some(&MateStatus::Ok));
    assert!(matches!(solution.status(bad), Some(MateStatus::Error(_))));
    let p = asm.component(b).unwrap().placement;
    assert!(close(p.translation.z, 0.0));
}

#[test]
fn a_chain_of_components_solves_quickly() {
    let mut asm = Assembly::new();
    let mut previous = asm.add_component(
        "Link",
        "link.oxyprt".into(),
        "l".into(),
        Placement::IDENTITY,
    );
    let mut ids = vec![previous];
    for i in 1..20 {
        let wobble = Vec3::new(0.05 * i as f64, -0.03, 0.02);
        let id = asm.add_component(
            "Link",
            "link.oxyprt".into(),
            "l".into(),
            placed(Vec3::new(30.0 * i as f64, 5.0, -3.0), wobble),
        );
        asm.add_mate(
            MateKind::Concentric,
            r(previous, MateTarget::Axis(StandardAxis::Z)),
            r(id, MateTarget::Axis(StandardAxis::Z)),
            true,
        );
        asm.add_mate(
            MateKind::Distance(10.0),
            r(previous, MateTarget::Plane(StandardPlane::Front)),
            r(id, MateTarget::Plane(StandardPlane::Front)),
            true,
        );
        previous = id;
        ids.push(id);
    }
    let start = std::time::Instant::now();
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    // Each link can still turn about the shared axis.
    assert!(ids[1..].iter().all(|id| solution.degrees(*id) == Some(1)));
    assert!(start.elapsed().as_secs_f64() < 5.0, "{:?}", start.elapsed());
}

#[test]
fn angle_between_planes() {
    let (mut asm, a, b) = two();
    asm.add_mate(
        MateKind::Coincident,
        r(a, MateTarget::Plane(StandardPlane::Front)),
        r(b, MateTarget::Plane(StandardPlane::Front)),
        true,
    );
    asm.add_mate(
        MateKind::Angle(30f64.to_radians()),
        r(a, MateTarget::Plane(StandardPlane::Right)),
        r(b, MateTarget::Plane(StandardPlane::Right)),
        true,
    );
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let x = asm.component(b).unwrap().placement.apply_vector(Vec3::X);
    assert!(close(x.dot(Vec3::X), 30f64.to_radians().cos()), "{x:?}");
}

#[test]
fn mates_that_do_not_apply_are_errors() {
    let (mut asm, a, b) = two();
    let m = asm.add_mate(
        MateKind::Concentric,
        r(a, MateTarget::Origin),
        r(b, MateTarget::Origin),
        true,
    );
    let solution = run(&mut asm);
    assert!(matches!(solution.status(m), Some(MateStatus::Error(_))));
}

#[test]
fn fixed_components_do_not_move() {
    let (mut asm, a, b) = two();
    asm.component_mut(b).unwrap().fixed = true;
    let before = asm.component(b).unwrap().placement;
    asm.add_mate(
        MateKind::Coincident,
        r(a, MateTarget::Origin),
        r(b, MateTarget::Origin),
        true,
    );
    let solution = run(&mut asm);
    assert_eq!(asm.component(b).unwrap().placement, before);
    assert_eq!(solution.errors(), 1);
}

#[test]
fn placement_moves_about_a_center() {
    let p = Placement::IDENTITY.moved(
        Vec3::new(0.0, 0.0, std::f64::consts::FRAC_PI_2),
        Point3::new(1.0, 0.0, 0.0),
        Vec3::ZERO,
    );
    let q = p.apply_point(Point3::new(2.0, 0.0, 0.0));
    assert!(
        close(q.x, 1.0) && close(q.y, 1.0) && close(q.z, 0.0),
        "{q:?}"
    );
}

#[test]
fn bill_of_materials_counts_instances() {
    let (mut asm, _, _) = two();
    asm.add_component("Pin", "pin.oxyprt".into(), "b".into(), Placement::IDENTITY);
    // Same document found under another path: still the same part.
    asm.add_component(
        "Pin",
        "sub/pin.oxyprt".into(),
        "b".into(),
        Placement::IDENTITY,
    );
    let lines = bom::lines(&asm);
    assert_eq!(lines.len(), 2);
    assert_eq!((lines[0].item, lines[0].quantity), (1, 1));
    assert_eq!((lines[1].item, lines[1].quantity), (2, 3));
    assert_eq!(lines[1].instances, ["Pin<1>", "Pin<2>", "Pin<3>"]);
    let csv = bom::to_csv(&[vec!["a,b".into(), "say \"hi\"".into(), "c".into()]]);
    assert_eq!(csv, "\"a,b\",\"say \"\"hi\"\"\",c\r\n");
}

#[test]
fn combined_meshes_map_picks_back_to_components() {
    use oxyde_kernel::{FaceMesh, Mesh};
    let mesh = Mesh {
        faces: vec![
            FaceMesh {
                positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                triangles: vec![[0, 1, 2]],
            };
            2
        ],
        edges: vec![vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]; 3],
    };
    let a = Placement::IDENTITY;
    let b = Placement::from_translation(Vec3::new(10.0, 0.0, 0.0));
    let (combined, ranges) =
        scene::combine([(ComponentId(1), &a, &mesh), (ComponentId(2), &b, &mesh)]);
    assert_eq!((combined.faces.len(), combined.edges.len()), (4, 6));
    assert_eq!(combined.faces[2].positions[1], [11.0, 0.0, 0.0]);
    assert_eq!(scene::face_owner(&ranges, 3), Some((&ComponentId(2), 1)));
    assert_eq!(scene::edge_owner(&ranges, 2), Some((&ComponentId(1), 2)));
    assert_eq!(scene::face_index(&ranges, &ComponentId(2), 0), Some(2));
    assert_eq!(scene::edge_index(&ranges, &ComponentId(1), 5), None);
}

#[test]
fn bill_of_materials_through_sub_assemblies() {
    use bom::{BomMode, rows};
    // A robot: one base, two arms. An arm: two links and four pins.
    let mut arm = Assembly::new();
    for (stem, n) in [("Link", 2), ("Pin", 4)] {
        for _ in 0..n {
            arm.add_component(
                stem,
                format!("{}.oxyprt", stem.to_lowercase()),
                stem.into(),
                Placement::IDENTITY,
            );
        }
    }
    let mut robot = Assembly::new();
    robot.add_component(
        "Base",
        "base.oxyprt".into(),
        "Base".into(),
        Placement::IDENTITY,
    );
    for _ in 0..2 {
        robot.add_component(
            "Arm",
            "arm/arm.oxyasm".into(),
            "Arm".into(),
            Placement::IDENTITY,
        );
    }
    robot.add_component(
        "Pin",
        "arm/pin.oxyprt".into(),
        "Pin".into(),
        Placement::IDENTITY,
    );
    // Keys: the file path relative to the robot; the arm's parts live in arm/.
    let open = |c: &Component, parent: &str| -> (String, Option<&Assembly>) {
        let folder = parent.rsplit_once('/').map_or("", |(f, _)| f);
        let key = if folder.is_empty() {
            c.path.clone()
        } else {
            format!("{folder}/{}", c.path)
        };
        let sub = (c.document_id == "Arm").then_some(&arm);
        (key, sub)
    };
    let top = rows(&robot, "robot.oxyasm", BomMode::TopLevel, &open);
    let summary = |rows: &[bom::BomRow]| {
        rows.iter()
            .map(|r| format!("{} {} x{}", r.item, r.key, r.quantity))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        summary(&top),
        [
            "1 base.oxyprt x1",
            "2 arm/arm.oxyasm x2",
            "3 arm/pin.oxyprt x1"
        ]
    );
    assert!(top[1].is_assembly);
    let indented = rows(&robot, "robot.oxyasm", BomMode::Indented, &open);
    assert_eq!(
        summary(&indented),
        [
            "1 base.oxyprt x1",
            "2 arm/arm.oxyasm x2",
            "2.1 arm/link.oxyprt x2",
            "2.2 arm/pin.oxyprt x4",
            "3 arm/pin.oxyprt x1",
        ]
    );
    assert_eq!(indented[2].level, 1);
    // Parts only: the pins inside both arms and the loose one add up.
    let parts = rows(&robot, "robot.oxyasm", BomMode::PartsOnly, &open);
    assert_eq!(
        summary(&parts),
        [
            "1 base.oxyprt x1",
            "2 arm/link.oxyprt x4",
            "3 arm/pin.oxyprt x9"
        ]
    );
}

#[test]
fn placements_compose_like_nested_frames() {
    let outer = placed(
        Vec3::new(5.0, 0.0, 0.0),
        Vec3::new(0.0, 0.0, std::f64::consts::FRAC_PI_2),
    );
    let inner = placed(Vec3::new(1.0, 2.0, 3.0), Vec3::new(0.3, 0.0, 0.1));
    let p = Point3::new(0.5, -1.0, 2.0);
    let expected = outer.apply_point(inner.apply_point(p));
    let got = outer.compose(&inner).apply_point(p);
    assert!((got - expected).length() < 1e-12, "{got:?} {expected:?}");
}

#[test]
fn a_component_held_by_one_that_moves_is_not_fully_defined() {
    // Base (fixed), Pin concentric with it (it can still turn and slide), Cap held to the
    // Pin by three planes: the cap cannot move on the pin, but moves with it.
    let (mut asm, base, pin) = two();
    asm.add_mate(
        MateKind::Concentric,
        r(base, MateTarget::Axis(StandardAxis::Z)),
        r(pin, MateTarget::Axis(StandardAxis::Z)),
        true,
    );
    let cap = asm.add_component(
        "Cap",
        "cap.oxyprt".into(),
        "c".into(),
        placed(Vec3::new(-5.0, 3.0, 8.0), Vec3::new(0.1, 0.2, -0.1)),
    );
    for plane in [
        StandardPlane::Front,
        StandardPlane::Top,
        StandardPlane::Right,
    ] {
        asm.add_mate(
            MateKind::Coincident,
            r(pin, MateTarget::Plane(plane)),
            r(cap, MateTarget::Plane(plane)),
            true,
        );
    }
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    assert_eq!(solution.degrees(pin), Some(2));
    assert_eq!(solution.degrees(cap), Some(2));
    // Fixing the pin defines both.
    asm.component_mut(pin).unwrap().fixed = true;
    let solution = run(&mut asm);
    assert_eq!(solution.degrees(pin), Some(0));
    assert_eq!(solution.degrees(cap), Some(0));
}

#[test]
fn a_ball_mates_by_its_centre() {
    use oxyde_geom::Axis;
    use oxyde_kernel::{FaceInfo, SurfaceKind, Topology};
    use oxyde_model::{Body, BodyNaming, FaceName, FeatureId};
    let feature = FeatureId::from_raw(2);
    let center = Point3::new(1.0, 2.0, 3.0);
    let topology = Topology {
        faces: vec![FaceInfo {
            surface: SurfaceKind::Sphere,
            area: 50.0,
            centroid: center,
            normal: None,
            axis: Axis::new(center, Vec3::Z),
            edges: Vec::new(),
        }],
        edges: Vec::new(),
        vertices: Vec::new(),
    };
    let ball = FaceName::profile(feature, 1);
    let body = Body {
        naming: BodyNaming::build(feature, vec![Some(ball.clone())], &topology),
        topology,
        solids: Vec::new(),
    };
    assert_eq!(
        resolve(&MateTarget::Face(ball), Some(&body)),
        Ok(Primitive::Point(center))
    );
}

#[test]
fn a_conflict_in_one_group_leaves_the_others_alone() {
    // Two pins on a fixed base, mated to it only: two groups solved apart. The second pin's
    // two distances contradict each other; the newest one is blamed, the first pin is fine.
    let (mut asm, base, first) = two();
    let second = asm.add_component(
        "Pin",
        "pin.oxyprt".into(),
        "b".into(),
        placed(Vec3::new(-10.0, 5.0, 2.0), Vec3::new(0.1, 0.0, 0.2)),
    );
    let front = |c| r(c, MateTarget::Plane(StandardPlane::Front));
    let axis = |c| r(c, MateTarget::Axis(StandardAxis::Z));
    asm.add_mate(MateKind::Concentric, axis(base), axis(first), true);
    asm.add_mate(MateKind::Distance(5.0), front(base), front(first), true);
    asm.add_mate(MateKind::Concentric, axis(base), axis(second), true);
    asm.add_mate(MateKind::Distance(8.0), front(base), front(second), true);
    let late = asm.add_mate(MateKind::Distance(12.0), front(base), front(second), true);
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 1, "{:?}", solution.mates);
    assert!(matches!(solution.status(late), Some(MateStatus::Error(_))));
    // Both pins on the axis, at their first distance.
    for (pin, gap) in [(first, 5.0), (second, 8.0)] {
        let p = asm.component(pin).unwrap().placement;
        assert!(
            close(p.translation.x, 0.0) && close(p.translation.y, 0.0),
            "{p:?}"
        );
        assert!(close(p.translation.z.abs(), gap), "{p:?}");
        assert_eq!(solution.degrees(pin), Some(1));
    }
}

fn front(c: ComponentId) -> MateRef {
    r(c, MateTarget::Plane(StandardPlane::Front))
}

fn right(c: ComponentId) -> MateRef {
    r(c, MateTarget::Plane(StandardPlane::Right))
}

fn x_axis(c: ComponentId) -> MateRef {
    r(c, MateTarget::Axis(StandardAxis::X))
}

/// Two components the way the app inserts them: side by side along X and turned alike, so
/// their front and top planes are level and all their planes in line.
fn side_by_side() -> (Assembly, ComponentId, ComponentId) {
    let (mut asm, a, b) = two();
    asm.component_mut(b).unwrap().placement =
        Placement::from_translation(Vec3::new(60.0, 0.0, 0.0));
    (asm, a, b)
}

#[test]
fn mates_solve_from_where_inserted_parts_sit() {
    // Each mate starts where its equation is level: no slope says which way to go.
    // Level planes: the second moves off along the first one's normal.
    let (mut asm, a, b) = side_by_side();
    asm.add_mate(MateKind::Distance(10.0), front(a), front(b), true);
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let t = asm.component(b).unwrap().placement.translation;
    assert!(close(t.z, 10.0), "{t:?}");
    assert_eq!(solution.degrees(b), Some(3));

    // Lines in line.
    let (mut asm, a, b) = side_by_side();
    asm.add_mate(MateKind::Distance(10.0), x_axis(a), x_axis(b), true);
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let t = asm.component(b).unwrap().placement.translation;
    assert!(close(t.y.hypot(t.z), 10.0), "{t:?}");

    // Points together.
    let (mut asm, a, b) = side_by_side();
    asm.component_mut(b).unwrap().placement = Placement::IDENTITY;
    let origin = |c| r(c, MateTarget::Origin);
    asm.add_mate(MateKind::Distance(10.0), origin(a), origin(b), true);
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let t = asm.component(b).unwrap().placement.translation;
    assert!(close(t.length(), 10.0), "{t:?}");
    assert_eq!(solution.degrees(b), Some(5));

    // Planes in line, to be turned apart.
    for (kind, degrees) in [
        (MateKind::Angle(30f64.to_radians()), 30f64),
        (MateKind::Perpendicular, 90.0),
    ] {
        let (mut asm, a, b) = side_by_side();
        asm.add_mate(MateKind::Coincident, front(a), front(b), true);
        asm.add_mate(kind, right(a), right(b), true);
        let solution = run(&mut asm);
        assert_eq!(solution.errors(), 0, "{kind:?}: {:?}", solution.mates);
        let x = asm.component(b).unwrap().placement.apply_vector(Vec3::X);
        assert!(close(x.dot(Vec3::X), degrees.to_radians().cos()), "{x:?}");
        assert_eq!(solution.degrees(b), Some(2));
    }
}

#[test]
fn angles_of_0_and_180_degrees_hold_exactly() {
    // There the normals are in line, which also stops the turn the angle leaves elsewhere.
    for (degrees, aligned, dot) in [(0f64, true, 1.0), (180.0, true, -1.0), (0.0, false, -1.0)] {
        let (mut asm, a, b) = two();
        asm.add_mate(MateKind::Coincident, front(a), front(b), true);
        asm.add_mate(
            MateKind::Angle(degrees.to_radians()),
            right(a),
            right(b),
            aligned,
        );
        let solution = run(&mut asm);
        assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
        let x = asm.component(b).unwrap().placement.apply_vector(Vec3::X);
        assert!((x.dot(Vec3::X) - dot).abs() < 1e-12, "{degrees}: {x:?}");
        assert_eq!(solution.degrees(b), Some(2));
    }
}

#[test]
fn an_angle_between_lines_has_no_way_round() {
    // Lines have no direction, so 150° between them is 30°: from 154° the part turns 4°,
    // not 124°.
    let (mut asm, a, b) = two();
    asm.component_mut(b).unwrap().placement = placed(
        Vec3::new(10.0, 20.0, 0.0),
        Vec3::new(0.0, 0.0, 154f64.to_radians()),
    );
    asm.add_mate(MateKind::Coincident, front(a), front(b), true);
    asm.add_mate(
        MateKind::Angle(30f64.to_radians()),
        x_axis(a),
        x_axis(b),
        true,
    );
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let x = asm.component(b).unwrap().placement.apply_vector(Vec3::X);
    assert!(close(x.dot(Vec3::X), 150f64.to_radians().cos()), "{x:?}");
}

#[test]
fn an_angle_between_a_line_and_a_plane_is_measured_from_the_plane() {
    // As Parallel and Perpendicular read them: at 0° the line runs along the plane.
    for degrees in [0f64, 30.0, 90.0] {
        let (mut asm, a, b) = two();
        asm.add_mate(
            MateKind::Angle(degrees.to_radians()),
            front(a),
            x_axis(b),
            true,
        );
        let solution = run(&mut asm);
        assert_eq!(solution.errors(), 0, "{degrees}: {:?}", solution.mates);
        let x = asm.component(b).unwrap().placement.apply_vector(Vec3::X);
        assert!(
            close(x.dot(Vec3::Z).abs(), degrees.to_radians().sin()),
            "{degrees}: {x:?}"
        );
    }
}

#[test]
fn a_zero_distance_holds_points_together() {
    let (mut asm, a, b) = two();
    let origin = |c| r(c, MateTarget::Origin);
    asm.add_mate(MateKind::Distance(0.0), origin(a), origin(b), true);
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let t = asm.component(b).unwrap().placement.translation;
    assert!(close(t.length(), 0.0), "{t:?}");
    assert_eq!(solution.degrees(b), Some(3));
}

#[test]
fn axes_that_start_square_can_be_made_concentric() {
    // A screw inserted beside a plate often has its axis square to the hole's.
    for kind in [MateKind::Concentric, MateKind::Coincident, MateKind::Parallel] {
        let (mut asm, a, b) = side_by_side();
        asm.add_mate(
            kind,
            r(a, MateTarget::Axis(StandardAxis::Z)),
            r(b, MateTarget::Axis(StandardAxis::Y)),
            true,
        );
        let solution = run(&mut asm);
        assert_eq!(solution.errors(), 0, "{kind:?}: {:?}", solution.mates);
        let y = asm.component(b).unwrap().placement.apply_vector(Vec3::Y);
        assert!(close(y.cross(Vec3::Z).length(), 0.0), "{kind:?}: {y:?}");
        if kind != MateKind::Parallel {
            let t = asm.component(b).unwrap().placement.translation;
            assert!(close(t.x, 0.0) && close(t.y, 0.0), "{kind:?}: {t:?}");
        }
    }
}

#[test]
fn a_component_mates_to_the_assembly_itself() {
    // The first component is fixed where it was inserted; floated, it can be mated to the
    // assembly's own planes and origin, which never move.
    let (mut asm, a, b) = two();
    asm.component_mut(a).unwrap().fixed = false;
    let me = ComponentId::ASSEMBLY;
    assert!(asm.knows(me) && asm.is_fixed(me));
    asm.add_mate(MateKind::Coincident, front(me), front(b), true);
    asm.add_mate(MateKind::Coincident, right(me), right(b), true);
    asm.add_mate(
        MateKind::Coincident,
        r(me, MateTarget::Plane(StandardPlane::Top)),
        r(b, MateTarget::Plane(StandardPlane::Top)),
        true,
    );
    // A fixed component cannot follow a mate to the assembly.
    asm.component_mut(a).unwrap().fixed = true;
    let stuck = asm.add_mate(
        MateKind::Distance(5.0),
        r(me, MateTarget::Origin),
        r(a, MateTarget::Origin),
        true,
    );
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 1, "{:?}", solution.mates);
    assert!(is_identity(&asm.component(b).unwrap().placement));
    assert_eq!(solution.degrees(b), Some(0));
    match solution.status(stuck) {
        Some(MateStatus::Error(e)) => assert!(e.contains("fixed"), "{e}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_part_turns_about_its_own_middle() {
    // A pin modelled 1000 mm from its origin, inserted beside the base, square to it: made
    // concentric with the base's axis, it should turn in place, not swing about its origin.
    let (mut asm, a, b) = side_by_side();
    let far = Point3::new(1000.0, 0.0, 0.0);
    asm.add_mate(
        MateKind::Concentric,
        r(a, MateTarget::Axis(StandardAxis::Z)),
        r(b, MateTarget::Axis(StandardAxis::Y)),
        true,
    );
    let before = asm.component(b).unwrap().placement.apply_point(far);
    let solution = solve_about(&asm, |m| resolve(&m.target, None), |_| far, 100.0);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    let placement = solution.placements[0].1;
    let after = placement.apply_point(far);
    // The middle moved only sideways onto the axis, not by a thousand millimetres.
    assert!((after - before).length() < 100.0, "{before:?} -> {after:?}");
    assert!(close(placement.apply_vector(Vec3::Y).cross(Vec3::Z).length(), 0.0));
}

// ---- Equations -------------------------------------------------------------------------------

/// A distance and an angle mate between the two components of [`two`].
fn mated() -> (Assembly, ComponentId, ComponentId, MateId, MateId) {
    let (mut asm, a, b) = two();
    let front = |c| r(c, MateTarget::Plane(StandardPlane::Front));
    let top = |c| r(c, MateTarget::Plane(StandardPlane::Top));
    let distance = asm.add_mate(MateKind::Distance(5.0), front(a), front(b), true);
    let angle = asm.add_mate(MateKind::Angle(0.5), top(a), top(b), true);
    (asm, a, b, distance, angle)
}

fn add_equation(asm: &mut Assembly, target: &str, text: &str) {
    let target = asm.find_dimension(target, &TopLevelOnly).unwrap();
    let name = asm.dimension_name(&target, &TopLevelOnly).unwrap();
    let expr = asm.parse_expression(text, &TopLevelOnly).unwrap();
    asm.equations.add_equation(target, name, expr).unwrap();
}

fn mate_value(asm: &Assembly, id: MateId) -> f64 {
    match asm.mate(id).unwrap().kind {
        MateKind::Distance(d) => d,
        MateKind::Angle(a) => a.to_degrees(),
        _ => panic!("no value"),
    }
}

#[test]
fn equations_drive_mate_values() {
    let (mut asm, _, _, distance, angle) = mated();
    let gap = asm.parse_expression("2", &TopLevelOnly).unwrap();
    asm.equations.add_global("Gap", gap).unwrap();
    // Listed before the variable it uses: order does not matter.
    add_equation(&mut asm, "D1@Angle1", "90 - \"D1@Distance1\" * 10");
    add_equation(&mut asm, "D1@Distance1", "\"Gap\" * 3");
    let results = asm.evaluate_equations(&mut NoParts);
    assert_eq!(results.globals, vec![Ok(2.0)]);
    assert_eq!(results.equations, vec![Ok(30.0), Ok(6.0)]);
    assert!(close(mate_value(&asm, distance), 6.0));
    assert!(close(mate_value(&asm, angle), 30.0));
    assert!(asm.is_mate_driven(distance));
    let dimensions = asm.mate_dimensions();
    assert_eq!(dimensions.len(), 2);
    assert_eq!(dimensions[0].full_name, "D1@Distance1");
    assert!(dimensions[1].angle && dimensions[1].driven);
    // Only mates with a value have a dimension, and only D1.
    assert!(
        asm.find_dimension("D1@Coincident1", &TopLevelOnly)
            .is_none()
    );
    assert!(asm.find_dimension("D2@Distance1", &TopLevelOnly).is_none());
}

#[test]
fn values_a_mate_cannot_take_are_errors() {
    let (mut asm, _, _, distance, angle) = mated();
    add_equation(&mut asm, "D1@Distance1", "-4");
    add_equation(&mut asm, "D1@Angle1", "200");
    let results = asm.evaluate_equations(&mut NoParts);
    assert!(
        results.equations[0]
            .as_ref()
            .unwrap_err()
            .contains("negative")
    );
    assert!(results.equations[1].as_ref().unwrap_err().contains("180"));
    assert!(close(mate_value(&asm, distance), 5.0));
    assert!(close(mate_value(&asm, angle), 0.5f64.to_degrees()));

    // A deleted mate leaves its equation broken, kept by name.
    asm.remove_mate(distance);
    let results = asm.evaluate_equations(&mut NoParts);
    assert!(
        results.equations[0]
            .as_ref()
            .unwrap_err()
            .contains("D1@Distance1")
    );
    assert_eq!(
        asm.equations_text(&TopLevelOnly).equations[0].dimension,
        "D1@Distance1"
    );
    // Names that exist nowhere are refused when typed.
    assert!(
        asm.parse_expression("\"D1@Distance9\"", &TopLevelOnly)
            .is_err()
    );
}

/// Parts by path and name, as the application's loaded files would give them; each
/// component its own file unless `files` says otherwise.
#[derive(Default)]
struct FakeParts {
    values: std::collections::HashMap<(Vec<ComponentId>, String), f64>,
    files: std::collections::HashMap<Vec<ComponentId>, String>,
}

impl PartValues for FakeParts {
    fn file(&self, path: &[ComponentId]) -> Option<std::path::PathBuf> {
        let file = self.files.get(path).cloned();
        Some(file.unwrap_or_else(|| format!("{path:?}")).into())
    }

    fn value(&self, path: &[ComponentId], name: &str) -> Option<f64> {
        self.values.get(&(path.to_vec(), name.to_owned())).copied()
    }

    fn drive(&mut self, path: &[ComponentId], name: &str, value: f64) -> Result<f64, String> {
        let slot = self
            .values
            .get_mut(&(path.to_vec(), name.to_owned()))
            .ok_or_else(|| format!("\"{name}\" does not exist"))?;
        *slot = value;
        Ok(value)
    }
}

/// One sub-assembly, component 1 of the assembly, holding component 4 "SSD<1>".
struct OneSub;

impl ComponentTree for OneSub {
    fn components(&self, path: &[ComponentId]) -> Option<Vec<(ComponentId, String)>> {
        (path == [ComponentId(1)]).then(|| vec![(ComponentId(4), "SSD<1>".to_owned())])
    }
}

#[test]
fn equations_read_and_drive_parts() {
    let (mut asm, a, b, distance, _) = mated();
    let mut parts = FakeParts::default();
    parts.values.insert((vec![a], "Thickness".to_owned()), 3.0);
    parts
        .values
        .insert((vec![b], "D1@Sketch1".to_owned()), 40.0);
    add_equation(&mut asm, "D1@Distance1", "\"Thickness@Base<1>.Part\" * 2");
    add_equation(&mut asm, "D1@Sketch1@Pin<1>.Part", "\"D1@Distance1\" + 1");
    let results = asm.evaluate_equations(&mut parts);
    assert_eq!(results.equations, vec![Ok(6.0), Ok(7.0)]);
    assert!(close(mate_value(&asm, distance), 6.0));
    assert_eq!(parts.value(&[b], "D1@Sketch1"), Some(7.0));
    assert_eq!(
        asm.find_dimension("D1@Sketch1@Pin<1>.Part", &TopLevelOnly),
        Some(AsmDimension::Part {
            path: vec![b],
            name: "D1@Sketch1".to_owned()
        })
    );
    // A part that is not loaded is reported.
    let results = asm.evaluate_equations(&mut NoParts);
    assert!(results.equations[0].is_err() && results.equations[1].is_err());
}

#[test]
fn instances_of_a_part_take_one_value() {
    let (mut asm, a, b, ..) = mated();
    let mut parts = FakeParts::default();
    for c in [a, b] {
        parts
            .values
            .insert((vec![c], "D1@Sketch1".to_owned()), 40.0);
        parts.files.insert(vec![c], "plate.oxyprt".to_owned());
    }
    add_equation(&mut asm, "D1@Sketch1@Base<1>.Part", "50");
    add_equation(&mut asm, "D1@Sketch1@Pin<1>.Part", "60");
    let results = asm.evaluate_equations(&mut parts);
    assert_eq!(results.equations[0], Ok(50.0));
    assert!(
        results.equations[1]
            .as_ref()
            .unwrap_err()
            .contains("instance")
    );
    // The same value through both is fine.
    let same = asm.parse_expression("50", &TopLevelOnly).unwrap();
    asm.equations.set_equation_expression(1, same).unwrap();
    let results = asm.evaluate_equations(&mut parts);
    assert_eq!(results.equations, vec![Ok(50.0), Ok(50.0)]);
}

#[test]
fn names_inside_sub_assemblies_resolve_once_known() {
    let (mut asm, a, ..) = mated();
    let name = "D1@Boss-Extrude1@SSD<1>.Part@Base<1>.Assembly";
    let expected = AsmDimension::Part {
        path: vec![a, ComponentId(4)],
        name: "D1@Boss-Extrude1".to_owned(),
    };
    assert_eq!(asm.find_dimension(name, &OneSub), Some(expected.clone()));
    assert_eq!(
        asm.dimension_name(&expected, &OneSub).as_deref(),
        Some(name)
    );
    assert!(asm.find_dimension(name, &TopLevelOnly).is_none());

    // Read before the sub-assembly is loaded: kept by name, found afterwards.
    let text = oxyde_model::EquationsText {
        equations: vec![oxyde_model::EquationText {
            dimension: name.to_owned(),
            expression: "\"D1@Distance1\" * 2".to_owned(),
        }],
        next_global_id: 1,
        ..Default::default()
    };
    asm.set_equations_text(text.clone(), &TopLevelOnly).unwrap();
    assert_eq!(asm.equations.equations[0].target, None);
    assert_eq!(asm.equations_text(&TopLevelOnly), text);
    asm.resolve_equation_names(&OneSub);
    assert_eq!(asm.equations.equations[0].target, Some(expected));
    assert_eq!(asm.equations_text(&OneSub), text);
}

#[test]
fn equations_text_round_trips_and_links_files() {
    let (mut asm, ..) = mated();
    let own = asm.parse_expression("7", &TopLevelOnly).unwrap();
    asm.equations.add_global("Own", own).unwrap();
    let linked = |name: &str, expression: &str| oxyde_model::LinkedValue {
        name: name.to_owned(),
        expression: expression.to_owned(),
    };
    asm.link_equations(
        Some("Case dimensions.txt".to_owned()),
        &[linked("Width", "233"), linked("Half", "\"Width\" / 2")],
        &TopLevelOnly,
    )
    .unwrap();
    add_equation(&mut asm, "D1@Distance1", "\"Half\" - 100");
    let results = asm.evaluate_equations(&mut NoParts);
    assert_eq!(results.equations, vec![Ok(16.5)]);
    assert_eq!(
        asm.linked_values(),
        [linked("Width", "233"), linked("Half", "\"Width\" / 2")]
    );

    let text = asm.equations_text(&TopLevelOnly);
    let mut copy = asm.clone();
    copy.equations = Default::default();
    copy.set_equations_text(text, &TopLevelOnly).unwrap();
    assert_eq!(copy.equations, asm.equations);

    // Unlinking keeps every variable as the assembly's own.
    asm.link_equations(None, &[], &TopLevelOnly).unwrap();
    assert!(asm.equations.linked_file.is_none());
    assert!(asm.equations.globals.iter().all(|g| !g.linked));
    assert_eq!(asm.equations.globals.len(), 3);
}

/// A pin in a hole and its head on the base: Concentric with *Lock rotation* and one
/// Coincident mate.
fn locked_pin(lock: bool) -> (Assembly, ComponentId, MateId, MateId) {
    let (mut asm, a, b) = two();
    let axis = asm.add_mate(
        MateKind::Concentric,
        r(a, MateTarget::Axis(StandardAxis::Z)),
        r(b, MateTarget::Axis(StandardAxis::Z)),
        true,
    );
    asm.mate_mut(axis).unwrap().lock_rotation = lock;
    let head = asm.add_mate(
        MateKind::Coincident,
        r(a, MateTarget::Plane(StandardPlane::Front)),
        r(b, MateTarget::Plane(StandardPlane::Front)),
        true,
    );
    (asm, b, axis, head)
}

/// The two mates leave the pin free to spin, until its concentric mate locks the rotation,
/// as SolidWorks' *Lock rotation* does: then it is fully defined.
#[test]
fn a_locked_concentric_and_a_coincident_hold_a_pin_fully() {
    for lock in [false, true] {
        let (mut asm, b, axis, _) = locked_pin(lock);
        let solution = run(&mut asm);
        assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
        assert_eq!(
            solution.degrees(b),
            Some(if lock { 0 } else { 1 }),
            "locked: {lock}"
        );
        // The turn held is kept for the next solve.
        assert_eq!(asm.mate(axis).unwrap().locked_angle.is_some(), lock);
    }
}

/// A drag turns the pin about its axis, then the mates are solved again: a locked pin turns
/// back to the turn it held, a free one stays turned.
#[test]
fn a_locked_pin_turns_back_after_a_drag() {
    for lock in [false, true] {
        let (mut asm, b, axis, _) = locked_pin(lock);
        run(&mut asm);
        let before = asm.component(b).unwrap().placement;
        let held = asm.mate(axis).unwrap().locked_angle;
        let c = asm.component_mut(b).unwrap();
        c.placement = c.placement.moved(Vec3::Z * 0.5, Point3::ORIGIN, Vec3::ZERO);
        let solution = run(&mut asm);
        assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
        let after = asm.component(b).unwrap().placement;
        let back = (after.apply_vector(Vec3::X) - before.apply_vector(Vec3::X)).length() < 1e-6;
        assert_eq!(back, lock, "locked: {lock}, {before:?} then {after:?}");
        assert_eq!(asm.mate(axis).unwrap().locked_angle, held);
    }
}

/// Flipping the head's mate turns the locked pin over, and it keeps its turn about the axis
/// it shares with the hole as well as the half turn allows: no error, still fully defined.
#[test]
fn a_locked_pin_turns_over_when_its_head_is_flipped() {
    let (mut asm, b, _, head) = locked_pin(true);
    run(&mut asm);
    asm.mate_mut(head).unwrap().aligned = false;
    let solution = run(&mut asm);
    assert_eq!(solution.errors(), 0, "{:?}", solution.mates);
    assert_eq!(solution.degrees(b), Some(0));
    let p = asm.component(b).unwrap().placement;
    assert!(close(p.apply_vector(Vec3::Z).z, -1.0), "{p:?}");
}

/// Changing the lock or the geometry takes the turn again from where the parts are: a lock
/// set on a pin already turned holds that turn.
#[test]
fn a_new_lock_holds_the_turn_the_parts_have() {
    let (mut asm, b, axis, _) = locked_pin(false);
    run(&mut asm);
    let c = asm.component_mut(b).unwrap();
    c.placement = c.placement.moved(Vec3::Z * 0.8, Point3::ORIGIN, Vec3::ZERO);
    run(&mut asm);
    let turned = asm.component(b).unwrap().placement;
    asm.mate_mut(axis).unwrap().lock_rotation = true;
    run(&mut asm);
    let c = asm.component_mut(b).unwrap();
    c.placement = c
        .placement
        .moved(Vec3::Z * -0.3, Point3::ORIGIN, Vec3::ZERO);
    run(&mut asm);
    let after = asm.component(b).unwrap().placement;
    assert!(
        (after.apply_vector(Vec3::X) - turned.apply_vector(Vec3::X)).length() < 1e-6,
        "{turned:?} then {after:?}"
    );
}

/// Lock rotation is a Concentric mate's, between two axes.
#[test]
fn only_concentric_between_axes_locks_rotation() {
    use PrimitiveKind::{Circle, Line, Plane, Point};
    assert!(MateKind::Concentric.can_lock_rotation(Line, Circle));
    assert!(MateKind::Concentric.can_lock_rotation(Circle, Circle));
    assert!(!MateKind::Concentric.can_lock_rotation(Line, Point));
    assert!(!MateKind::Coincident.can_lock_rotation(Circle, Circle));
    assert!(!MateKind::Parallel.can_lock_rotation(Plane, Plane));
}
