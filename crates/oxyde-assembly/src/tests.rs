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
    for (id, p) in &solution.placements {
        asm.component_mut(*id).unwrap().placement = *p;
    }
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
