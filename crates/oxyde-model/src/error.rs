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
        }
    }
}

impl std::error::Error for ModelError {}
