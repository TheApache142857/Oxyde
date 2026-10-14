use std::fmt;

use crate::feature::FeatureId;

/// Invalid edits of the timeline. Geometric failures are not errors of this kind: they are
/// reported per feature in the timeline (see `FeatureStatus`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    UnknownFeature(FeatureId),
    HasDependents {
        feature: FeatureId,
        dependents: Vec<FeatureId>,
    },
    /// A [`PartDefinition`](crate::PartDefinition) that cannot form a valid timeline.
    InvalidDefinition(String),
    /// Feature and dimension names cannot be empty, start or end with a space, or contain
    /// `@` or `"`.
    InvalidName(String),
    /// Another feature (or another dimension of the same feature) already has this name.
    DuplicateName(String),
    /// No dimension with this full name or reference.
    UnknownDimension(String),
    /// An expression that cannot be read, or an invalid change to the equations.
    Expression(String),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::UnknownFeature(id) => write!(f, "no feature {id} in this part"),
            ModelError::HasDependents {
                feature,
                dependents,
            } => {
                write!(f, "feature {feature} is used by")?;
                for d in dependents {
                    write!(f, " {d}")?;
                }
                Ok(())
            }
            ModelError::InvalidDefinition(reason) => write!(f, "invalid part definition: {reason}"),
            ModelError::InvalidName(name) => write!(
                f,
                "\"{name}\" is not a valid name (no @ or quotes, no leading or trailing spaces)"
            ),
            ModelError::DuplicateName(name) => write!(f, "the name \"{name}\" is already used"),
            ModelError::UnknownDimension(name) => write!(f, "no dimension {name}"),
            ModelError::Expression(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ModelError {}
