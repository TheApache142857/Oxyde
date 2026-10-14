use std::f64::consts::PI;

use oxyde_geom::{Curve2, Point2, Profile};

use crate::{
    ConstrainedSketch, Constraint::*, ConstraintState, Definition, EntityId, Geometry, SketchError,
};

fn close(p: Point2, x: f64, y: f64) -> bool {
    (p.x - x).abs() < 1e-7 && (p.y - y).abs() < 1e-7
}

fn center_of(s: &ConstrainedSketch, round: EntityId) -> EntityId {
    match s.entity(round).unwrap().geometry {
        Geometry::Circle { center, .. } | Geometry::Arc { center, .. } => center,
        _ => panic!("not round"),
    }
}

/// A rough rectangle whose lower-left corner is fixed at the origin.
fn anchored_rectangle(s: &mut ConstrainedSketch) -> [EntityId; 4] {
    let lines = s.rectangle(Point2::new(0.3, -0.2), 38.0, 19.0);
    let (origin, _) = s.endpoints(lines[0]).unwrap();
    s.move_point(origin, Point2::ORIGIN);
    s.constrain(Fixed(origin));
    lines
}

#[test]
fn dimensioned_rectangle_is_fully_defined() {
    let mut s = ConstrainedSketch::new();
    let [bottom, right, _, _] = anchored_rectangle(&mut s);
    s.constrain(Length(bottom, 40.0));
    s.constrain(Length(right, 20.0));

    let report = s.solve().unwrap();
    assert!(report.converged, "{report:?}");
    assert_eq!(report.degrees_of_freedom, 0);
    assert!(report.is_fully_defined());
    assert!(report.entities.iter().all(|(_, d)| *d == Definition::Fully));
    assert!(
        report
            .constraints
            .iter()
            .all(|c| *c == ConstraintState::Satisfied)
    );

    let (_, top_right) = s.endpoints(right).unwrap();
    assert!(close(s.position(top_right).unwrap(), 40.0, 20.0));
}

#[test]
fn free_rectangle_has_four_degrees_of_freedom() {
    let mut s = ConstrainedSketch::new();
    s.rectangle(Point2::ORIGIN, 10.0, 5.0);
    let report = s.solve().unwrap();
    assert!(report.converged);
    // Position (2) and size (2).
    assert_eq!(report.degrees_of_freedom, 4);
    assert!(report.entities.iter().all(|(_, d)| *d == Definition::Under));
}

#[test]
fn fixing_a_corner_defines_only_that_corner() {
    let mut s = ConstrainedSketch::new();
    let [bottom, ..] = anchored_rectangle(&mut s);
    let (origin, bottom_right) = s.endpoints(bottom).unwrap();
    let report = s.solve().unwrap();
    assert_eq!(report.degrees_of_freedom, 2);
    assert_eq!(report.definition(origin), Some(Definition::Fully));
    assert_eq!(report.definition(bottom_right), Some(Definition::Under));
    assert_eq!(report.definition(bottom), Some(Definition::Under));
}

#[test]
fn implied_relations_are_redundant() {
    let mut s = ConstrainedSketch::new();
    let [bottom, _, top, _] = s.rectangle(Point2::ORIGIN, 10.0, 5.0);
    let parallel = s.constrain(Parallel(bottom, top));
    let report = s.solve().unwrap();
    assert!(report.converged);
    assert_eq!(report.constraints[parallel], ConstraintState::Redundant);
    assert_eq!(report.degrees_of_freedom, 4);
}

#[test]
fn contradictory_dimensions_conflict_and_leave_the_sketch_unchanged() {
    let mut s = ConstrainedSketch::new();
    let [bottom, right, top, _] = s.rectangle(Point2::ORIGIN, 40.0, 20.0);
    let (origin, _) = s.endpoints(bottom).unwrap();
    s.constrain(Fixed(origin));
    s.constrain(Length(bottom, 40.0));
    let wrong = s.constrain(Length(top, 50.0));
    let before = s.clone();

    let report = s.solve().unwrap();
    assert!(!report.converged);
    assert_eq!(report.constraints[wrong], ConstraintState::Conflicting);
    assert_eq!(s, before);
    let (_, top_right) = s.endpoints(right).unwrap();
    assert!(close(s.position(top_right).unwrap(), 40.0, 20.0));
}

#[test]
fn circle_tangent_to_a_line() {
    let mut s = ConstrainedSketch::new();
    let line = s.line(Point2::ORIGIN, Point2::new(10.0, 0.0));
    let (a, b) = s.endpoints(line).unwrap();
    s.constrain(Fixed(a));
    s.constrain(Fixed(b));
    let circle = s.circle(Point2::new(5.0, 3.0), 1.0);
    s.constrain(Tangent(line, circle));
    s.constrain(Radius(circle, 2.0));

    let report = s.solve().unwrap();
    assert!(report.converged);
    // Free to slide along the line.
    assert_eq!(report.degrees_of_freedom, 1);
    let c = s.position(center_of(&s, circle)).unwrap();
    assert!((c.y - 2.0).abs() < 1e-7, "{c:?}");
    assert!((c.x - 5.0).abs() < 1e-6, "{c:?}");
}

#[test]
fn angle_and_length_place_a_line() {
    let mut s = ConstrainedSketch::new();
    let base = s.line(Point2::ORIGIN, Point2::new(10.0, 0.0));
    let (origin, far) = s.endpoints(base).unwrap();
    s.constrain(Fixed(origin));
    s.constrain(Fixed(far));
    let tip = s.point(Point2::new(8.0, 6.0));
    let arm = s.line_between(origin, tip);
    s.constrain(Angle(base, arm, 30f64.to_radians()));
    s.constrain(Length(arm, 10.0));

    let report = s.solve().unwrap();
    assert!(report.is_fully_defined(), "{report:?}");
    let p = s.position(tip).unwrap();
    assert!(close(p, 10.0 * (PI / 6.0).cos(), 5.0));
}

#[test]
fn perpendicular_lines_of_equal_length() {
    let mut s = ConstrainedSketch::new();
    let first = s.line(Point2::ORIGIN, Point2::new(10.0, 0.0));
    let (origin, end) = s.endpoints(first).unwrap();
    s.constrain(Fixed(origin));
    s.constrain(Fixed(end));
    let tip = s.point(Point2::new(1.0, 7.0));
    let second = s.line_between(origin, tip);
    s.constrain(Perpendicular(first, second));
    s.constrain(EqualLength(first, second));

    let report = s.solve().unwrap();
    assert!(report.is_fully_defined());
    assert!(close(s.position(tip).unwrap(), 0.0, 10.0));
}

#[test]
fn arcs_keep_a_single_radius() {
    let mut s = ConstrainedSketch::new();
    let arc = s.arc(Point2::ORIGIN, Point2::new(5.0, 0.0), Point2::new(0.0, 5.5));
    let center = center_of(&s, arc);
    s.constrain(Fixed(center));
    s.constrain(Radius(arc, 5.0));

    let report = s.solve().unwrap();
    assert!(report.converged);
    let Geometry::Arc { start, end, .. } = s.entity(arc).unwrap().geometry else {
        unreachable!()
    };
    let (start, end) = (s.position(start).unwrap(), s.position(end).unwrap());
    assert!((start.distance(Point2::ORIGIN) - 5.0).abs() < 1e-7);
    assert!((end.distance(Point2::ORIGIN) - 5.0).abs() < 1e-7);

    // As a profile curve, its middle point is halfway round, counter-clockwise.
    let segments = s.profile_segments();
    let Curve2::Arc { mid, .. } = segments[0].curve else {
        panic!("expected an arc")
    };
    assert!((mid.distance(Point2::ORIGIN) - 5.0).abs() < 1e-6);
    assert!(mid.x > 0.0 && mid.y > 0.0);
}

#[test]
fn points_on_lines_circles_and_midpoints() {
    let mut s = ConstrainedSketch::new();
    let line = s.line(Point2::ORIGIN, Point2::new(10.0, 0.0));
    let (a, b) = s.endpoints(line).unwrap();
    s.constrain(Fixed(a));
    s.constrain(Fixed(b));
    let mid = s.point(Point2::new(3.0, 2.0));
    s.constrain(Midpoint(mid, line));

    let circle = s.circle(Point2::new(20.0, 0.0), 5.0);
    s.constrain(Fixed(center_of(&s, circle)));
    s.constrain(Radius(circle, 5.0));
    let on = s.point(Point2::new(23.0, 3.0));
    s.constrain(PointOnCircle(on, circle));
    let on_line = s.point(Point2::new(4.0, 1.0));
    s.constrain(PointOnLine(on_line, line));

    let report = s.solve().unwrap();
    assert!(report.converged);
    assert!(close(s.position(mid).unwrap(), 5.0, 0.0));
    let p = s.position(on).unwrap();
    assert!((p.distance(Point2::new(20.0, 0.0)) - 5.0).abs() < 1e-7);
    assert!(s.position(on_line).unwrap().y.abs() < 1e-7);
}

#[test]
fn horizontal_and_vertical_distances() {
    let mut s = ConstrainedSketch::new();
    let a = s.point(Point2::ORIGIN);
    s.constrain(Fixed(a));
    let b = s.point(Point2::new(4.0, 7.0));
    s.constrain(HorizontalDistance(a, b, 12.0));
    s.constrain(VerticalDistance(a, b, 3.0));
    let report = s.solve().unwrap();
    assert!(report.is_fully_defined());
    assert!(close(s.position(b).unwrap(), 12.0, 3.0));
}

#[test]
fn dragging_a_corner_keeps_the_rectangle() {
    let mut s = ConstrainedSketch::new();
    let [bottom, right, ..] = s.rectangle(Point2::ORIGIN, 10.0, 5.0);
    let (_, corner) = s.endpoints(right).unwrap();
    s.move_point(corner, Point2::new(14.0, 8.0));
    let report = s.solve().unwrap();
    assert!(report.converged);
    // Still a rectangle: its bottom is horizontal and its right side vertical.
    let (a, b) = s.endpoints(bottom).unwrap();
    assert!((s.position(a).unwrap().y - s.position(b).unwrap().y).abs() < 1e-7);
    let (c, d) = s.endpoints(right).unwrap();
    assert!((s.position(c).unwrap().x - s.position(d).unwrap().x).abs() < 1e-7);
}

#[test]
fn solved_sketch_becomes_an_extrudable_profile() {
    let mut s = ConstrainedSketch::new();
    let lines = anchored_rectangle(&mut s);
    s.constrain(Length(lines[0], 40.0));
    s.constrain(Length(lines[1], 20.0));
    let guide = s.circle(Point2::new(20.0, 10.0), 3.0);
    s.set_construction(guide, true);
    s.solve().unwrap();

    let segments = s.profile_segments();
    let mut ids: Vec<u32> = segments.iter().map(|seg| seg.id).collect();
    ids.sort();
    let mut expected: Vec<u32> = lines.iter().map(|l| l.0).collect();
    expected.sort();
    assert_eq!(ids, expected);

    let profile = Profile::from_curves(&segments).unwrap();
    assert_eq!(profile.regions.len(), 1);
    assert!((profile.regions[0].outer.signed_area().abs() - 800.0).abs() < 1e-6);
}

#[test]
fn bad_references_are_reported() {
    let mut s = ConstrainedSketch::new();
    let p = s.point(Point2::ORIGIN);
    s.constrain(Horizontal(p));
    assert!(matches!(
        s.solve(),
        Err(SketchError::WrongKind {
            constraint: Some(0),
            ..
        })
    ));

    let mut s = ConstrainedSketch::new();
    let p = s.point(Point2::ORIGIN);
    s.constrain(Distance(p, EntityId(99), 1.0));
    assert_eq!(s.solve(), Err(SketchError::UnknownEntity(EntityId(99))));
}

#[test]
fn deleting_a_line_removes_its_relations_and_orphan_points() {
    let mut s = ConstrainedSketch::new();
    let [bottom, right, top, left] = s.rectangle(Point2::ORIGIN, 10.0, 5.0);
    let before = s.entities().len();
    s.remove(&[bottom]);
    // The bottom line and its horizontal relation go; its end points are still used by
    // the left and right lines, so they stay.
    assert_eq!(s.entities().len(), before - 1);
    assert!(s.entity(bottom).is_none());
    assert_eq!(s.constraints().len(), 3);

    // Removing a corner point takes the two lines standing on it.
    let (corner, _) = s.endpoints(top).unwrap();
    s.remove(&[corner]);
    assert!(s.entity(right).is_none() && s.entity(top).is_none());
    assert!(s.entity(left).is_some());
}

#[test]
fn stored_sketches_are_validated() {
    let mut s = ConstrainedSketch::new();
    s.rectangle(Point2::ORIGIN, 10.0, 5.0);
    let rebuilt =
        ConstrainedSketch::from_parts(s.entities().to_vec(), s.constraints().to_vec()).unwrap();
    assert_eq!(rebuilt, s);

    let mut entities = s.entities().to_vec();
    entities.push(entities[0]);
    assert!(matches!(
        ConstrainedSketch::from_parts(entities, Vec::new()),
        Err(SketchError::DuplicateId(_))
    ));
}

#[test]
fn measurements_and_dimension_edits() {
    let mut s = ConstrainedSketch::new();
    let a = s.line(Point2::ORIGIN, Point2::new(3.0, 4.0));
    let b = s.line(Point2::ORIGIN, Point2::new(0.0, 2.0));
    assert!((s.line_length(a).unwrap() - 5.0).abs() < 1e-12);
    let angle = s.angle_between(a, b).unwrap();
    assert!((angle - (PI / 2.0 - (4.0f64).atan2(3.0))).abs() < 1e-12);

    let (origin, _) = s.endpoints(a).unwrap();
    s.constrain(Fixed(origin));
    assert!(s.is_fixed(origin));
    let length = s.constrain(Length(a, 5.0));
    assert!(s.set_dimension(length, 8.0));
    s.solve().unwrap();
    assert!((s.line_length(a).unwrap() - 8.0).abs() < 1e-7);
    assert!(!s.set_dimension(0, 1.0)); // Fixed has no value
}
