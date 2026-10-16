use std::f64::consts::PI;

use oxyde_geom::{Curve2, Point2, Profile};

use crate::{
    ConstrainedSketch, Constraint::*, ConstraintState, Counters, Definition, EntityId, Geometry,
    SketchError,
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
    assert_eq!(
        report.count(ConstraintState::Satisfied),
        report.relations.len()
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
    assert_eq!(report.state(parallel), Some(ConstraintState::Redundant));
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
    assert_eq!(report.state(wrong), Some(ConstraintState::Conflicting));
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
    assert_eq!(s.relations().len(), 3);

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
        ConstrainedSketch::from_parts(s.entities().to_vec(), s.relations().to_vec(), s.counters())
            .unwrap();
    assert_eq!(rebuilt, s);

    let mut entities = s.entities().to_vec();
    entities.push(entities[0]);
    assert!(matches!(
        ConstrainedSketch::from_parts(entities, Vec::new(), Counters::default()),
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
    let fixed = s.constrain(Fixed(origin));
    assert!(s.is_fixed(origin));
    let length = s.constrain(Length(a, 5.0));
    assert!(s.set_dimension(length, 8.0));
    s.solve().unwrap();
    assert!((s.line_length(a).unwrap() - 8.0).abs() < 1e-7);
    assert!(!s.set_dimension(fixed, 1.0)); // Fixed has no value
}

#[test]
fn dimensions_get_stable_names_and_ids() {
    let mut s = ConstrainedSketch::new();
    let [bottom, right, ..] = s.rectangle(Point2::ORIGIN, 10.0, 5.0);
    let width = s.constrain(Length(bottom, 10.0));
    let height = s.constrain(Length(right, 5.0));
    assert_eq!(s.relation(width).unwrap().name.as_deref(), Some("D1"));
    assert_eq!(s.relation(height).unwrap().name.as_deref(), Some("D2"));
    // Plain relations have no name.
    assert!(s.relations()[0].name.is_none());

    s.rename_dimension(width, "Width").unwrap();
    assert_eq!(s.dimension_named("Width").map(|r| r.id), Some(width));
    // Names are unique within the sketch and cannot contain '@' or quotes.
    assert!(s.rename_dimension(height, "Width").is_err());
    assert!(s.rename_dimension(height, "a@b").is_err());
    let horizontal = s.relations()[0].id;
    assert!(s.rename_dimension(horizontal, "Flat").is_err());

    // Deleting the newest dimension never lets its id or name be handed out again.
    s.remove_relation(height);
    let next = s.constrain(Length(right, 6.0));
    assert_ne!(next, height);
    assert_eq!(s.relation(next).unwrap().name.as_deref(), Some("D3"));
}

#[test]
fn entity_ids_are_never_reused() {
    let mut s = ConstrainedSketch::new();
    let first = s.line(Point2::ORIGIN, Point2::new(1.0, 0.0));
    s.remove(&[first]);
    let second = s.line(Point2::ORIGIN, Point2::new(2.0, 0.0));
    // A face named after the deleted line must not attach to the new one.
    assert_ne!(first, second);

    // Counters survive a save: rebuilding from parts keeps handing out fresh ids.
    let mut reloaded =
        ConstrainedSketch::from_parts(s.entities().to_vec(), s.relations().to_vec(), s.counters())
            .unwrap();
    let third = reloaded.point(Point2::ORIGIN);
    assert!(third.0 > second.0);
}

// ---- Dimensions between entities, crossings and trimming -----------------------------------

#[test]
fn point_to_line_and_line_to_line_distances_solve() {
    let mut s = ConstrainedSketch::new();
    let base = s.line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0));
    let top = s.line(Point2::new(1.0, 3.0), Point2::new(9.0, 3.2));
    let p = s.point(Point2::new(4.0, -2.0));
    s.constrain(Horizontal(base));
    s.constrain(Horizontal(top));
    s.constrain(LineDistance(top, base, 7.5));
    s.constrain(PointLineDistance(p, base, 4.0));
    let report = s.solve().unwrap();
    assert!(report.converged, "{report:?}");
    let (a, _) = s.endpoints(base).unwrap();
    let (b, _) = s.endpoints(top).unwrap();
    let y0 = s.position(a).unwrap().y;
    assert!(((s.position(b).unwrap().y - y0).abs() - 7.5).abs() < 1e-7);
    assert!(((s.position(p).unwrap().y - y0).abs() - 4.0).abs() < 1e-7);
}

#[test]
fn connected_curves_do_not_count_as_crossing() {
    let mut s = ConstrainedSketch::new();
    s.rectangle(Point2::new(0.0, 0.0), 10.0, 5.0);
    assert!(s.intersections().is_empty());
    s.line(Point2::new(5.0, -1.0), Point2::new(5.0, 6.0));
    let crossings = s.intersections();
    assert_eq!(crossings.len(), 2);
    assert!(crossings.iter().any(|(_, _, p)| close(*p, 5.0, 0.0)));
    assert!(crossings.iter().any(|(_, _, p)| close(*p, 5.0, 5.0)));
}

#[test]
fn trimming_an_end_cuts_back_to_the_crossing() {
    let mut s = ConstrainedSketch::new();
    let h = s.line(Point2::new(-10.0, 0.0), Point2::new(10.0, 0.0));
    let v = s.line(Point2::new(0.0, -10.0), Point2::new(0.0, 10.0));
    let (_, old_end) = s.endpoints(h).unwrap();
    assert!(s.trim(h, Point2::new(6.0, 0.3)));
    let (start, end) = s.endpoints(h).unwrap();
    assert!(close(s.position(start).unwrap(), -10.0, 0.0));
    assert!(close(s.position(end).unwrap(), 0.0, 0.0));
    assert!(s.entity(old_end).is_none(), "the cut-off end point is gone");
    assert!(
        s.relations()
            .iter()
            .any(|r| r.constraint == PointOnLine(end, v))
    );
    assert!(s.solve().unwrap().converged);
}

#[test]
fn trimming_the_middle_splits_a_line() {
    let mut s = ConstrainedSketch::new();
    let h = s.line(Point2::new(-10.0, 0.0), Point2::new(10.0, 0.0));
    s.constrain(Horizontal(h));
    s.line(Point2::new(-5.0, -3.0), Point2::new(-5.0, 3.0));
    s.line(Point2::new(5.0, -3.0), Point2::new(5.0, 3.0));
    let preview = s.trim_preview(h, Point2::new(1.0, 0.0)).unwrap();
    assert!(close(preview[0], -5.0, 0.0) && close(*preview.last().unwrap(), 5.0, 0.0));

    let before = s.entities().len();
    assert!(s.trim(h, Point2::new(1.0, 0.0)));
    assert_eq!(
        s.entities().len(),
        before + 3,
        "two cut points and a second line"
    );
    let report = s.solve().unwrap();
    assert!(report.converged, "{report:?}");
    let lines: Vec<(Point2, Point2)> = s
        .entities()
        .iter()
        .filter_map(|e| match e.geometry {
            Geometry::Line { start, end } => {
                Some((s.position(start).unwrap(), s.position(end).unwrap()))
            }
            _ => None,
        })
        .filter(|(a, b)| a.y.abs() < 1e-9 && b.y.abs() < 1e-9)
        .collect();
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .iter()
            .any(|(a, b)| close(*a, -10.0, 0.0) && close(*b, -5.0, 0.0))
    );
    assert!(
        lines
            .iter()
            .any(|(a, b)| close(*a, 5.0, 0.0) && close(*b, 10.0, 0.0))
    );
}

#[test]
fn trimming_a_circle_leaves_an_arc() {
    let mut s = ConstrainedSketch::new();
    let circle = s.circle(Point2::ORIGIN, 5.0);
    s.line(Point2::new(-10.0, 0.0), Point2::new(10.0, 0.0));
    assert!(s.trim(circle, Point2::new(0.4, 5.1)));
    let Geometry::Arc { start, end, .. } = s.entity(circle).unwrap().geometry else {
        panic!("still a circle");
    };
    // The lower half remains: counter-clockwise from (-5, 0) to (5, 0).
    assert!(close(s.position(start).unwrap(), -5.0, 0.0));
    assert!(close(s.position(end).unwrap(), 5.0, 0.0));
    assert!(s.solve().unwrap().converged);
}

#[test]
fn trimming_a_curve_nothing_crosses_deletes_it() {
    let mut s = ConstrainedSketch::new();
    let lone = s.line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0));
    s.circle(Point2::new(0.0, 20.0), 3.0);
    assert!(s.trim(lone, Point2::new(5.0, 0.0)));
    assert!(s.entity(lone).is_none());
    assert_eq!(s.entities().len(), 2, "the circle and its centre remain");
}

#[test]
fn the_origin_is_a_fixed_point_that_survives_deletions() {
    let mut s = ConstrainedSketch::new();
    let origin = s.ensure_origin(Point2::ORIGIN);
    assert_eq!(s.ensure_origin(Point2::ORIGIN), origin, "made once");
    // A line from the origin, then deleted: the origin stays.
    let end = s.point(Point2::new(10.0, 0.0));
    let line = s.line_between(origin, end);
    s.remove(&[line]);
    assert_eq!(s.origin(), Some(origin));
    s.remove(&[origin]);
    assert_eq!(s.origin(), Some(origin), "cannot be deleted");
    // A rectangle centred on it with a dimension from it.
    let [bottom, right, ..] = s.rectangle(Point2::new(-4.0, -3.0), 9.0, 5.0);
    let (corner, _) = s.endpoints(bottom).unwrap();
    s.constrain(Length(bottom, 20.0));
    s.constrain(Length(right, 10.0));
    s.constrain(HorizontalDistance(origin, corner, 10.0));
    s.constrain(VerticalDistance(origin, corner, 5.0));
    let report = s.solve().unwrap();
    assert!(
        report.converged && report.degrees_of_freedom == 0,
        "{report:?}"
    );
    assert!(close(s.position(corner).unwrap(), -10.0, -5.0));
    assert!(close(s.position(origin).unwrap(), 0.0, 0.0));
}

#[test]
fn mirrored_geometry_follows_the_original() {
    let mut s = ConstrainedSketch::new();
    let axis = s.line(Point2::new(0.0, -10.0), Point2::new(0.0, 10.0));
    s.set_construction(axis, true);
    s.constrain(Vertical(axis));
    let (bottom, _) = s.endpoints(axis).unwrap();
    s.constrain(Fixed(bottom));
    // A line touching the axis and an arc, mirrored.
    let line = s.line(Point2::new(0.0, 5.0), Point2::new(6.0, 2.0));
    let arc = s.arc(
        Point2::new(4.0, -3.0),
        Point2::new(6.0, -3.0),
        Point2::new(4.0, -1.0),
    );
    let before = s.entities().len();
    let created = s.mirror(&[line, arc, axis], axis).unwrap();
    // The line's point on the axis is shared: 1 + 3 new points, a line and an arc.
    assert_eq!(s.entities().len(), before + created.len());
    assert_eq!(created.len(), 6);
    assert!(s.solve().unwrap().converged);
    let (_, end) = s.endpoints(line).unwrap();
    let mirrored_end = s
        .entities()
        .iter()
        .find(|e| matches!(e.geometry, Geometry::Point(p) if close(p, -6.0, 2.0)))
        .map(|e| e.id)
        .expect("mirrored end point");
    // Move the original: the copy follows.
    s.constrain(Distance(end, mirrored_end, 20.0));
    assert!(s.solve().unwrap().converged);
    let p = s.position(end).unwrap();
    let q = s.position(mirrored_end).unwrap();
    assert!((p.x + q.x).abs() < 1e-7 && (p.y - q.y).abs() < 1e-7);
    assert!(((p.x - q.x).abs() - 20.0).abs() < 1e-7);
}

#[test]
fn the_origin_is_remembered_among_other_fixed_construction_points() {
    let mut s = ConstrainedSketch::new();
    // A fixed construction point that is not the origin (Convert Entities makes such points
    // on the rim of converted circles).
    let rim = s.point(Point2::new(5.0, 0.0));
    s.set_construction(rim, true);
    s.constrain(Fixed(rim));
    let origin = s.ensure_origin(Point2::ORIGIN);
    assert_ne!(origin, rim);
    assert_eq!(s.origin(), Some(origin));
    // Placing the origin again finds it, and moves it, not the other point.
    assert_eq!(s.ensure_origin(Point2::new(3.0, 4.0)), origin);
    assert_eq!(s.position(rim), Some(Point2::new(5.0, 0.0)));
    assert_eq!(s.position(origin), Some(Point2::new(3.0, 4.0)));
    // Even with its relation gone, it is still the origin, and fixed again when placed.
    let fixed = s
        .relations()
        .iter()
        .find(|r| r.constraint == Fixed(origin))
        .map(|r| r.id)
        .unwrap();
    s.remove_relation(fixed);
    assert_eq!(s.ensure_origin(Point2::ORIGIN), origin);
    assert!(s.is_fixed(origin));
    // Deleting everything keeps it.
    let all: Vec<EntityId> = s.entities().iter().map(|e| e.id).collect();
    s.remove(&all);
    assert_eq!(s.origin(), Some(origin));
    assert_eq!(s.entities().len(), 1);
}

#[test]
fn a_sketch_saved_without_its_origin_recorded_finds_it() {
    // Old files: the origin is the fixed construction point where the model origin falls.
    let mut s = ConstrainedSketch::new();
    let rim = s.point(Point2::new(5.0, 0.0));
    s.set_construction(rim, true);
    s.constrain(Fixed(rim));
    let old_origin = s.point(Point2::ORIGIN);
    s.set_construction(old_origin, true);
    s.constrain(Fixed(old_origin));
    let s =
        ConstrainedSketch::from_parts(s.entities().to_vec(), s.relations().to_vec(), s.counters())
            .unwrap();
    let mut s = s;
    assert_eq!(s.ensure_origin(Point2::ORIGIN), old_origin);
    assert_eq!(s.position(rim), Some(Point2::new(5.0, 0.0)));
    // A wrong origin in a file is refused.
    let line = s.line(Point2::new(1.0, 1.0), Point2::new(2.0, 2.0));
    assert!(s.clone().with_origin(Some(line)).is_err());
    assert!(s.with_origin(Some(EntityId(999))).is_err());
}

#[test]
fn perpendicular_and_parallel_from_lines_that_start_the_other_way() {
    // The sine or cosine of the angle has no slope there, so the solver could not start.
    let direction = |s: &ConstrainedSketch, l: EntityId| {
        let (a, b) = s.endpoints(l).unwrap();
        let (a, b) = (s.position(a).unwrap(), s.position(b).unwrap());
        (b.x - a.x, b.y - a.y)
    };
    for (relation, start) in [
        (Perpendicular as fn(EntityId, EntityId) -> _, (10.0, 3.0)),
        (Parallel, (-3.0, 10.0)),
    ] {
        let mut s = ConstrainedSketch::new();
        let first = s.line(Point2::ORIGIN, Point2::new(10.0, 3.0));
        let (origin, end) = s.endpoints(first).unwrap();
        s.constrain(Fixed(origin));
        s.constrain(Fixed(end));
        let second = s.line(Point2::new(0.0, 5.0), Point2::new(start.0, 5.0 + start.1));
        let wanted = relation(first, second);
        s.constrain(wanted.clone());
        let report = s.solve().unwrap();
        assert!(report.converged, "{wanted:?}: {report:?}");
        let (u, v) = (direction(&s, first), direction(&s, second));
        let (dot, cross) = (u.0 * v.0 + u.1 * v.1, u.0 * v.1 - u.1 * v.0);
        let off = if matches!(wanted, Perpendicular(..)) {
            dot
        } else {
            cross
        };
        assert!(off.abs() < 1e-7, "{wanted:?}: {u:?} {v:?}");
    }
}

// ---- Sketch Fillet and Offset Entities ----------------------------------------------------

fn lines_of(s: &ConstrainedSketch) -> Vec<EntityId> {
    s.entities()
        .iter()
        .filter(|e| matches!(e.geometry, Geometry::Line { .. }))
        .map(|e| e.id)
        .collect()
}

fn arcs_of(s: &ConstrainedSketch) -> Vec<EntityId> {
    s.entities()
        .iter()
        .filter(|e| matches!(e.geometry, Geometry::Arc { .. }))
        .map(|e| e.id)
        .collect()
}

#[test]
fn a_fillet_rounds_a_corner_with_an_arc_tangent_to_both_lines() {
    let mut s = ConstrainedSketch::new();
    let [_, right, top, _] = s.rectangle(Point2::ORIGIN, 10.0, 5.0);
    let corner = s.shared_corner(right, top).unwrap();
    assert_eq!(s.corner_lines(corner), Some((right, top)));
    let preview = s.fillet_preview(right, top, 2.0).unwrap();
    assert!(
        preview
            .iter()
            .all(|p| (p.distance(Point2::new(8.0, 3.0)) - 2.0).abs() < 1e-9)
    );

    let arc = s.fillet(right, top, 2.0).unwrap();
    assert!(s.entity(corner).is_none(), "a free corner goes");
    let Geometry::Arc { center, start, end } = s.entity(arc).unwrap().geometry else {
        panic!("not an arc");
    };
    assert!(close(s.position(center).unwrap(), 8.0, 3.0));
    // Counter-clockwise from the right line's new end to the top line's.
    assert!(close(s.position(start).unwrap(), 10.0, 3.0));
    assert!(close(s.position(end).unwrap(), 8.0, 5.0));
    assert_eq!(s.endpoints(right).unwrap().1, start);
    assert_eq!(s.endpoints(top).unwrap().0, end);
    assert!(
        s.relations()
            .iter()
            .any(|r| r.constraint == Tangent(right, arc))
    );
    assert!(
        s.relations()
            .iter()
            .any(|r| r.constraint == Tangent(top, arc))
    );
    let report = s.solve().unwrap();
    assert!(report.converged, "{report:?}");
    // Position, size, and the radius.
    assert_eq!(report.degrees_of_freedom, 5);
    s.constrain(Radius(arc, 2.0));
    assert_eq!(s.solve().unwrap().degrees_of_freedom, 4);
    // Too big for the lines, or lines that do not meet: refused.
    assert!(s.fillet(right, top, 20.0).is_none());
    let [bottom, ..] = s.rectangle(Point2::new(50.0, 50.0), 4.0, 4.0);
    assert!(s.fillet(right, bottom, 1.0).is_none());
}

#[test]
fn a_fillet_keeps_a_dimensioned_corner_as_a_virtual_sharp() {
    let mut s = ConstrainedSketch::new();
    let [bottom, right, ..] = anchored_rectangle(&mut s);
    s.constrain(Length(bottom, 40.0));
    s.constrain(Length(right, 20.0));
    assert_eq!(s.solve().unwrap().degrees_of_freedom, 0);
    let corner = s.shared_corner(bottom, right).unwrap();
    let (origin, _) = s.endpoints(bottom).unwrap();

    let arc = s.fillet(bottom, right, 5.0).unwrap();
    let sharp = s.entity(corner).expect("kept");
    assert!(sharp.construction);
    assert!(
        s.relations()
            .iter()
            .any(|r| r.constraint == PointOnLine(corner, bottom))
    );
    assert!(
        s.relations()
            .iter()
            .any(|r| r.constraint == PointOnLine(corner, right))
    );
    assert!(
        s.relations()
            .iter()
            .any(|r| r.constraint == Distance(origin, corner, 40.0)),
        "the length goes on measuring to the corner"
    );
    assert!(
        !s.relations()
            .iter()
            .any(|r| matches!(r.constraint, Length(..)))
    );
    s.constrain(Radius(arc, 5.0));
    let report = s.solve().unwrap();
    assert!(
        report.converged && report.degrees_of_freedom == 0,
        "{report:?}"
    );
    assert!(close(s.position(corner).unwrap(), 40.0, 0.0));
    let (_, cut) = s.endpoints(bottom).unwrap();
    assert!(close(s.position(cut).unwrap(), 35.0, 0.0));
}

#[test]
fn offsetting_a_rectangle_makes_a_fully_defined_copy_one_value_moves() {
    let mut s = ConstrainedSketch::new();
    let lines = anchored_rectangle(&mut s);
    s.constrain(Length(lines[0], 40.0));
    s.constrain(Length(lines[1], 20.0));
    s.solve().unwrap();
    assert_eq!(s.chain_of(lines[2]).len(), 4);
    // Left of the bottom line, walked from the origin: inside.
    let created = s.offset(&lines, 3.0).unwrap();
    assert_eq!(created.len(), 4);
    let report = s.solve().unwrap();
    assert!(
        report.converged && report.degrees_of_freedom == 0,
        "{report:?}"
    );
    let corners: Vec<Point2> = s
        .entities()
        .iter()
        .filter_map(|e| match e.geometry {
            Geometry::Point(p) => Some(p),
            _ => None,
        })
        .collect();
    assert!(corners.iter().any(|p| close(*p, 3.0, 3.0)));
    assert!(corners.iter().any(|p| close(*p, 37.0, 17.0)));
    // The one distance dimension moves every line.
    let dimension = s
        .relations()
        .iter()
        .find(|r| matches!(r.constraint, LineDistance(..)))
        .unwrap()
        .id;
    assert_eq!(
        s.relations()
            .iter()
            .filter(|r| matches!(r.constraint, EqualDistance(..)))
            .count(),
        3
    );
    assert!(s.set_dimension(dimension, 5.0));
    let report = s.solve().unwrap();
    assert!(
        report.converged && report.degrees_of_freedom == 0,
        "{report:?}"
    );
    let corners: Vec<Point2> = s
        .entities()
        .iter()
        .filter_map(|e| match e.geometry {
            Geometry::Point(p) => Some(p),
            _ => None,
        })
        .collect();
    assert!(corners.iter().any(|p| close(*p, 5.0, 5.0)));
    assert!(corners.iter().any(|p| close(*p, 35.0, 15.0)));
    // The other way: outside.
    let outside = s.offset(&lines, -2.0).unwrap();
    assert_eq!(outside.len(), 4);
    assert!(s.solve().unwrap().converged);
    assert!(
        s.entities()
            .iter()
            .any(|e| matches!(e.geometry, Geometry::Point(p) if close(p, -2.0, -2.0)))
    );
}

#[test]
fn offsetting_a_rounded_contour_keeps_it_tangent_and_concentric() {
    let mut s = ConstrainedSketch::new();
    let [bottom, right, top, left] = anchored_rectangle(&mut s);
    s.constrain(Length(bottom, 40.0));
    s.constrain(Length(right, 20.0));
    let arc = s.fillet(right, top, 5.0).unwrap();
    s.constrain(Radius(arc, 5.0));
    assert_eq!(s.solve().unwrap().degrees_of_freedom, 0);
    let contour = [bottom, right, arc, top, left];
    assert_eq!(s.chain_of(arc).len(), 5);
    let before = s.entities().len();
    let created = s.offset(&contour, 2.0).unwrap();
    assert_eq!(created.len(), 5);
    assert_eq!(
        s.entities().len(),
        before + 5 + 5,
        "five curves and five points: the centre is shared"
    );
    let report = s.solve().unwrap();
    assert!(
        report.converged && report.degrees_of_freedom == 0,
        "{report:?}"
    );
    let inner = arcs_of(&s).into_iter().find(|a| *a != arc).unwrap();
    let Geometry::Arc { center, start, .. } = s.entity(inner).unwrap().geometry else {
        panic!()
    };
    assert_eq!(center, center_of(&s, arc), "concentric");
    assert!(
        (s.position(center)
            .unwrap()
            .distance(s.position(start).unwrap())
            - 3.0)
            .abs()
            < 1e-9
    );
    assert_eq!(
        s.relations()
            .iter()
            .filter(|r| matches!(r.constraint, Tangent(_, a) if a == inner))
            .count(),
        2
    );
    // An arc smaller than the offset cannot be made.
    assert!(s.offset(&contour, 6.0).is_none());
}

#[test]
fn offsetting_a_circle_and_an_open_chain() {
    let mut s = ConstrainedSketch::new();
    let circle = s.circle(Point2::new(5.0, 5.0), 10.0);
    assert_eq!(s.chain_of(circle), vec![circle]);
    // Negative: outwards.
    let created = s.offset(&[circle], -4.0).unwrap();
    let Geometry::Circle { center, radius } = s.entity(created[0]).unwrap().geometry else {
        panic!()
    };
    assert_eq!(center, center_of(&s, circle));
    assert!((radius - 14.0).abs() < 1e-9);
    assert!(
        s.relations()
            .iter()
            .any(|r| r.constraint == Radius(created[0], 14.0))
    );

    // An L of two lines, offset to the outside of the corner: the lines are extended to meet.
    let a = s.line(Point2::new(100.0, 0.0), Point2::new(110.0, 0.0));
    let tip = s.point(Point2::new(110.0, 10.0));
    let b = s.line_between(s.endpoints(a).unwrap().1, tip);
    assert_eq!(s.chain_of(a).len(), 2);
    let created = s.offset(&[a, b], -2.0).unwrap();
    assert_eq!(created.len(), 2);
    assert!(s.solve().unwrap().converged);
    assert!(
        s.entities()
            .iter()
            .any(|e| matches!(e.geometry, Geometry::Point(p) if close(p, 112.0, -2.0))),
        "the corner of the offset"
    );
    assert_eq!(lines_of(&s).len(), 4);

    // Three curves meeting at a point cannot be offset together; a point is no chain.
    let away = s.point(Point2::new(120.0, -5.0));
    let c = s.line_between(s.endpoints(a).unwrap().1, away);
    assert!(s.offset(&[a, b, c], 1.0).is_none());
    assert_eq!(s.chain_of(a), vec![a], "the chain stops at the junction");
    assert!(s.chain_of(s.endpoints(a).unwrap().0).is_empty());
}
