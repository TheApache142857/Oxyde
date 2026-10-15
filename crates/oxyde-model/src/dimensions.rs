//! Every dimension of a part, by stable reference and by SolidWorks-style full name.
//!
//! A dimension lives either in a sketch (a length, radius, angle… relation) or in a feature
//! (an extrusion depth, a fillet radius). Its full name is `name@feature`, e.g. `D1@Sketch1`
//! or `Width@Boss-Extrude1`. Full names are for people and equations; internally a dimension
//! is always referred to by a [`DimensionRef`] made of stable ids, so renaming a feature or a
//! dimension changes how it is displayed and nothing else.

use std::collections::HashSet;
use std::fmt;

use oxyde_sketch::RelationId;

use crate::equations::Equations;
use crate::error::ModelError;
use crate::feature::{Feature, FeatureId, FeatureKind, FeatureParameter};
use crate::part::Part;

/// Which dimension of a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DimensionKey {
    /// A dimension relation of a sketch.
    Sketch(RelationId),
    /// A parameter of the feature itself.
    Feature(FeatureParameter),
}

/// Stable reference to a dimension: unaffected by renames, never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DimensionRef {
    pub feature: FeatureId,
    pub key: DimensionKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimensionUnit {
    /// Millimetres.
    Length,
    /// Radians internally, degrees in the user interface and in equations.
    Angle,
    /// A number of instances (patterns): a whole number, at least 1.
    Count,
}

impl DimensionUnit {
    /// Internal value → display units (degrees for angles).
    pub fn to_display(self, value: f64) -> f64 {
        match self {
            DimensionUnit::Angle => value.to_degrees(),
            DimensionUnit::Length | DimensionUnit::Count => value,
        }
    }

    /// Display units → internal value; counts are rounded and must be at least 1.
    pub fn from_display(self, value: f64) -> Result<f64, String> {
        match self {
            DimensionUnit::Length => Ok(value),
            DimensionUnit::Angle => Ok(value.to_radians()),
            DimensionUnit::Count => {
                let count = value.round();
                if count >= 1.0 {
                    Ok(count)
                } else {
                    Err(format!("{value} is not a valid number of instances"))
                }
            }
        }
    }

    /// Text shown after a value in display units.
    pub fn suffix(self) -> &'static str {
        match self {
            DimensionUnit::Length => " mm",
            DimensionUnit::Angle => "°",
            DimensionUnit::Count => "",
        }
    }
}

/// A dimension as listed for the user.
#[derive(Debug, Clone, PartialEq)]
pub struct Dimension {
    pub reference: DimensionRef,
    /// Name within its feature, e.g. "D1".
    pub name: String,
    /// `name@feature`, e.g. "D1@Sketch1".
    pub full_name: String,
    /// mm, radians for angles, or a count.
    pub value: f64,
    pub unit: DimensionUnit,
    /// Set by an equation, so not editable by hand.
    pub driven: bool,
}

impl fmt::Display for Dimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.full_name)
    }
}

/// `D1` + `Sketch1` → `D1@Sketch1`.
pub fn full_name(dimension: &str, feature: &str) -> String {
    format!("{dimension}@{feature}")
}

/// `D1@Sketch1` (quotes allowed, as written in equations) → (`D1`, `Sketch1`).
pub fn split_full_name(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    let text = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(text);
    let (dimension, feature) = text.split_once('@')?;
    (!dimension.is_empty() && !feature.is_empty()).then_some((dimension, feature))
}

/// All dimensions of a timeline, in order (sketch dimensions in creation order, then each
/// feature's own).
pub(crate) fn list_dimensions(features: &[Feature], equations: &Equations) -> Vec<Dimension> {
    let driven: HashSet<DimensionRef> = equations
        .equations
        .iter()
        .filter_map(|e| e.target)
        .collect();
    let mut out = Vec::new();
    for feature in features {
        if let FeatureKind::Sketch(sketch) = feature.kind() {
            for relation in sketch.geometry.dimensions() {
                let name = relation.name.clone().unwrap_or_default();
                let reference = DimensionRef {
                    feature: feature.id(),
                    key: DimensionKey::Sketch(relation.id),
                };
                out.push(Dimension {
                    reference,
                    full_name: full_name(&name, feature.name()),
                    name,
                    value: relation.constraint.value().unwrap_or_default(),
                    unit: if relation.constraint.is_angle() {
                        DimensionUnit::Angle
                    } else {
                        DimensionUnit::Length
                    },
                    driven: driven.contains(&reference),
                });
            }
        }
        for (parameter, value) in feature.kind().parameters() {
            let name = feature.parameter_name(parameter);
            let reference = DimensionRef {
                feature: feature.id(),
                key: DimensionKey::Feature(parameter),
            };
            out.push(Dimension {
                reference,
                full_name: full_name(&name, feature.name()),
                name,
                value,
                unit: parameter.unit(),
                driven: driven.contains(&reference),
            });
        }
    }
    out
}

impl Part {
    /// All dimensions, in timeline order (sketch dimensions in creation order, then the
    /// feature's own).
    pub fn dimensions(&self) -> Vec<Dimension> {
        list_dimensions(self.features(), self.equations())
    }

    /// Whether an equation sets this dimension.
    pub fn is_driven(&self, reference: DimensionRef) -> bool {
        self.equations()
            .equations
            .iter()
            .any(|e| e.target == Some(reference))
    }

    pub fn dimension(&self, reference: DimensionRef) -> Option<Dimension> {
        self.dimensions()
            .into_iter()
            .find(|d| d.reference == reference)
    }

    /// Looks a dimension up by full name, e.g. `D1@Sketch1` or `"Width@Boss-Extrude1"`.
    pub fn find_dimension(&self, full_name: &str) -> Option<DimensionRef> {
        let (dimension, feature) = split_full_name(full_name)?;
        self.dimensions()
            .into_iter()
            .find(|d| {
                d.name == dimension
                    && self.feature(d.reference.feature).map(|f| f.name()) == Some(feature)
            })
            .map(|d| d.reference)
    }

    /// Changes a dimension's value (mm, radians for angles, or a count). The feature and
    /// everything after it are rebuilt on the next rebuild. A dimension driven by an equation
    /// gets its value back from the equation on the next rebuild.
    pub fn set_dimension(&mut self, reference: DimensionRef, value: f64) -> Result<(), ModelError> {
        let unknown = || ModelError::UnknownDimension(format!("{reference:?}"));
        let kind = self.edit(reference.feature)?;
        let changed = match (kind, reference.key) {
            (FeatureKind::Sketch(sketch), DimensionKey::Sketch(id)) => {
                sketch.geometry.set_dimension(id, value)
            }
            (kind, DimensionKey::Feature(parameter)) => kind.set_parameter(parameter, value),
            _ => false,
        };
        if changed { Ok(()) } else { Err(unknown()) }
    }

    /// Renames a dimension. Names must be valid and unique within their feature.
    pub fn rename_dimension(
        &mut self,
        reference: DimensionRef,
        name: &str,
    ) -> Result<(), ModelError> {
        if !oxyde_sketch::is_valid_name(name) {
            return Err(ModelError::InvalidName(name.to_owned()));
        }
        let unknown = || ModelError::UnknownDimension(format!("{reference:?}"));
        let feature = self
            .feature_mut_quietly(reference.feature)
            .ok_or(ModelError::UnknownFeature(reference.feature))?;
        let renamed = match reference.key {
            DimensionKey::Sketch(id) => {
                let FeatureKind::Sketch(sketch) = &mut feature.kind else {
                    return Err(unknown());
                };
                sketch
                    .geometry
                    .rename_dimension(id, name)
                    .map_err(|_| ModelError::DuplicateName(name.to_owned()))
            }
            DimensionKey::Feature(parameter) => {
                let parameters: Vec<FeatureParameter> =
                    feature.kind.parameters().iter().map(|(p, _)| *p).collect();
                if !parameters.contains(&parameter) {
                    return Err(unknown());
                }
                // Default names of optional dimensions are kept free too (a linear pattern's
                // second direction brings D3 and D4 when it is added).
                let taken = parameters
                    .iter()
                    .any(|p| *p != parameter && feature.parameter_name(*p) == name)
                    || feature
                        .kind
                        .possible_parameters()
                        .iter()
                        .enumerate()
                        .any(|(i, p)| *p != parameter && name == format!("D{}", i + 1));
                if taken {
                    return Err(ModelError::DuplicateName(name.to_owned()));
                }
                if name == feature.kind.default_parameter_name(parameter) {
                    feature.parameter_names.remove(&parameter);
                } else {
                    feature.parameter_names.insert(parameter, name.to_owned());
                }
                Ok(())
            }
        };
        if renamed.is_ok() {
            self.refresh_equation_names();
        }
        renamed
    }
}

#[cfg(test)]
mod tests {
    use oxyde_geom::Point2;
    use oxyde_sketch::{Constraint, EntityId};

    use super::*;
    use crate::feature::{EdgeBlend, Extrude, ExtrudeOp, Sketch, SketchPlane, StandardPlane};
    use crate::naming::{EdgeName, FaceName};

    /// Sketch1 (rectangle with width and height dimensions), Boss-Extrude1, Fillet1.
    fn bracket() -> (Part, FeatureId, FeatureId, RelationId) {
        let mut part = Part::new();
        let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
        let lines = sketch.rectangle(Point2::ORIGIN, 40.0, 20.0);
        let width = sketch
            .geometry
            .constrain(Constraint::Length(EntityId(lines[0]), 40.0));
        sketch
            .geometry
            .constrain(Constraint::Length(EntityId(lines[1]), 20.0));
        let s1 = part.add_feature(FeatureKind::Sketch(sketch));
        let boss = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            10.0,
        )));
        part.add_feature(FeatureKind::Fillet(EdgeBlend {
            edges: vec![EdgeName::between(
                FaceName::profile(boss, lines[1]),
                FaceName::end_cap(boss),
            )],
            size: 2.0,
        }));
        (part, s1, boss, width)
    }

    #[test]
    fn every_dimension_has_a_solidworks_style_full_name() {
        let (part, ..) = bracket();
        let names: Vec<String> = part.dimensions().into_iter().map(|d| d.full_name).collect();
        assert_eq!(
            names,
            ["D1@Sketch1", "D2@Sketch1", "D1@Boss-Extrude1", "D1@Fillet1"]
        );
    }

    #[test]
    fn full_names_resolve_to_stable_references() {
        let (mut part, s1, boss, width) = bracket();
        let reference = part.find_dimension("D1@Sketch1").unwrap();
        assert_eq!(
            reference,
            DimensionRef {
                feature: s1,
                key: DimensionKey::Sketch(width)
            }
        );
        // Quoted, as in an equation.
        assert_eq!(
            part.find_dimension("\"D1@Boss-Extrude1\"")
                .map(|r| r.feature),
            Some(boss)
        );
        assert_eq!(part.find_dimension("D9@Sketch1"), None);
        assert_eq!(part.find_dimension("no at sign"), None);

        // Renaming the feature and the dimension changes the full name, not the reference.
        part.rename(s1, "Base").unwrap();
        part.rename_dimension(reference, "Width").unwrap();
        assert_eq!(part.dimension(reference).unwrap().full_name, "Width@Base");
        assert_eq!(part.find_dimension("Width@Base"), Some(reference));
        assert_eq!(part.find_dimension("D1@Sketch1"), None);
    }

    #[test]
    fn dimension_values_can_be_set_through_their_reference() {
        let (mut part, _, boss, _) = bracket();
        let depth = part.find_dimension("D1@Boss-Extrude1").unwrap();
        part.set_dimension(depth, 25.0).unwrap();
        assert_eq!(part.dimension(depth).unwrap().value, 25.0);
        match part.feature(boss).unwrap().kind() {
            FeatureKind::Extrude(e) => assert_eq!(e.extent.depth(), 25.0),
            _ => unreachable!(),
        }
        let width = part.find_dimension("D1@Sketch1").unwrap();
        part.set_dimension(width, 60.0).unwrap();
        assert_eq!(part.dimension(width).unwrap().value, 60.0);
    }

    #[test]
    fn names_are_validated() {
        let (mut part, s1, boss, _) = bracket();
        assert!(matches!(
            part.rename(s1, "Boss-Extrude1"),
            Err(ModelError::DuplicateName(_))
        ));
        assert!(matches!(
            part.rename(s1, "a@b"),
            Err(ModelError::InvalidName(_))
        ));
        assert!(matches!(
            part.rename(s1, " padded"),
            Err(ModelError::InvalidName(_))
        ));

        let d1 = part.find_dimension("D1@Sketch1").unwrap();
        assert!(matches!(
            part.rename_dimension(d1, "D2"),
            Err(ModelError::DuplicateName(_))
        ));
        let depth = part.find_dimension("D1@Boss-Extrude1").unwrap();
        part.rename_dimension(depth, "Thickness").unwrap();
        assert_eq!(
            part.feature(boss)
                .unwrap()
                .parameter_name(FeatureParameter::Depth),
            "Thickness"
        );
        // Back to the default name: nothing stored.
        part.rename_dimension(depth, "D1").unwrap();
        assert!(part.feature(boss).unwrap().parameter_names().is_empty());
    }

    #[test]
    fn full_names_split_on_the_at_sign() {
        assert_eq!(split_full_name("D1@Sketch1"), Some(("D1", "Sketch1")));
        assert_eq!(split_full_name(" \"Width@Base\" "), Some(("Width", "Base")));
        assert_eq!(split_full_name("@Sketch1"), None);
        assert_eq!(split_full_name("D1"), None);
    }
}
