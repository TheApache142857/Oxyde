//! Configurations: named variants of one part in one file, as SolidWorks' configurations
//! and design tables. A configuration remembers a value for each *configured* dimension
//! and whether each *configured* feature is suppressed; everything else is shared by all
//! of them. One configuration is active: the part is built with its values. Activating
//! another first stores what the part says now into the one leaving (so what was done
//! while it was active stays with it), then sets the dimensions and the suppressions of
//! the one arriving.
//!
//! A part has no configurations until the first one is added; adding it turns the part as
//! it is into "Default" and the new one into a copy of it.

use std::collections::BTreeMap;

use crate::dimensions::{DimensionRef, full_name, list_dimensions};
use crate::equations::Equations;
use crate::error::ModelError;
use crate::feature::FeatureId;
use crate::part::{Part, PartDefinition};

/// One named variant.
#[derive(Debug, Clone, PartialEq)]
pub struct Configuration {
    pub name: String,
    /// The configured dimensions' values: mm, radians, or counts.
    pub dimensions: BTreeMap<DimensionRef, f64>,
    /// Whether each configured feature is suppressed.
    pub suppressed: BTreeMap<FeatureId, bool>,
}

/// Every configuration of a part, and which one is active.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Configurations {
    pub list: Vec<Configuration>,
    pub active: usize,
}

/// The name a part's first configuration gets.
pub const DEFAULT_CONFIGURATION: &str = "Default";

impl Configurations {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn active(&self) -> Option<&Configuration> {
        self.list.get(self.active)
    }

    /// Every dimension any configuration sets, sorted.
    pub fn configured_dimensions(&self) -> Vec<DimensionRef> {
        let mut out: Vec<DimensionRef> = self
            .list
            .iter()
            .flat_map(|c| c.dimensions.keys().copied())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every feature any configuration suppresses or unsuppresses, sorted.
    pub fn configured_features(&self) -> Vec<FeatureId> {
        let mut out: Vec<FeatureId> = self
            .list
            .iter()
            .flat_map(|c| c.suppressed.keys().copied())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    pub fn position(&self, name: &str) -> Option<usize> {
        self.list.iter().position(|c| c.name == name)
    }

    /// Forgets a feature deleted from the part: whether it is suppressed, and its own
    /// dimensions and those of its sketch, in every configuration.
    pub(crate) fn forget_feature(&mut self, id: FeatureId) {
        for c in &mut self.list {
            c.suppressed.remove(&id);
            c.dimensions.retain(|r, _| r.feature != id);
        }
    }

    /// Unique, valid names and an active index in range.
    pub(crate) fn validate(&self) -> Result<(), ModelError> {
        let mut seen = std::collections::HashSet::new();
        for c in &self.list {
            if !is_valid_name(&c.name) || !seen.insert(c.name.as_str()) {
                return Err(ModelError::InvalidDefinition(format!(
                    "invalid or duplicate configuration name \"{}\"",
                    c.name
                )));
            }
        }
        if !self.list.is_empty() && self.active >= self.list.len() {
            return Err(ModelError::InvalidDefinition(
                "the active configuration does not exist".to_owned(),
            ));
        }
        Ok(())
    }
}

/// A configuration name: not empty, no control characters, at most 64 characters.
pub fn is_valid_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed == name
        && name.chars().count() <= 64
        && !name.chars().any(char::is_control)
}

// ---- Text form, for files ------------------------------------------------------------------

/// A configuration written with dimension names, as stored in files.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigurationText {
    pub name: String,
    /// Full dimension names ("D1@Sketch1") and their values (mm, radians, counts).
    pub dimensions: Vec<(String, f64)>,
    /// Feature ids and whether the configuration suppresses them.
    pub suppressed: Vec<(u32, bool)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConfigurationsText {
    pub active: String,
    pub list: Vec<ConfigurationText>,
}

impl PartDefinition {
    /// The configurations written with the current dimension names, e.g. for saving. A
    /// dimension that no longer exists is left out.
    pub fn configurations_text(&self) -> ConfigurationsText {
        let dimensions = list_dimensions(&self.features, &self.equations);
        let name_of = |r: &DimensionRef| {
            dimensions
                .iter()
                .find(|d| d.reference == *r)
                .map(|d| d.full_name.clone())
        };
        ConfigurationsText {
            active: self
                .configurations
                .active()
                .map(|c| c.name.clone())
                .unwrap_or_default(),
            list: self
                .configurations
                .list
                .iter()
                .map(|c| ConfigurationText {
                    name: c.name.clone(),
                    dimensions: c
                        .dimensions
                        .iter()
                        .filter_map(|(r, v)| Some((name_of(r)?, *v)))
                        .collect(),
                    suppressed: c.suppressed.iter().map(|(id, s)| (id.raw(), *s)).collect(),
                })
                .collect(),
        }
    }

    /// Replaces the configurations with ones read from text (e.g. from a file). A
    /// dimension name or feature id that matches nothing is dropped; the stored values are
    /// not applied to the features, which the file holds as the active configuration
    /// left them.
    pub fn set_configurations_text(&mut self, text: ConfigurationsText) -> Result<(), ModelError> {
        // Driven flags do not matter for name resolution.
        let dimensions = list_dimensions(&self.features, &Equations::default());
        let resolve = |name: &str| {
            let name = name.trim().trim_matches('"');
            dimensions
                .iter()
                .find(|d| d.full_name == name)
                .map(|d| d.reference)
        };
        let known = |id: u32| self.features.iter().any(|f| f.id().raw() == id);
        let list: Vec<Configuration> = text
            .list
            .iter()
            .map(|c| Configuration {
                name: c.name.clone(),
                dimensions: c
                    .dimensions
                    .iter()
                    .filter_map(|(n, v)| Some((resolve(n)?, *v)))
                    .collect(),
                suppressed: c
                    .suppressed
                    .iter()
                    .filter(|(id, _)| known(*id))
                    .map(|(id, s)| (FeatureId::from_raw(*id), *s))
                    .collect(),
            })
            .collect();
        let active = list.iter().position(|c| c.name == text.active).unwrap_or(0);
        let new = Configurations { list, active };
        new.validate()?;
        self.configurations = new;
        Ok(())
    }
}

impl Part {
    pub fn configurations(&self) -> &Configurations {
        &self.configurations
    }

    /// The part's current value of a dimension, if it exists.
    fn current_value(&self, reference: DimensionRef) -> Option<f64> {
        self.dimension(reference).map(|d| d.value)
    }

    fn require_configuration(&self, index: usize) -> Result<&Configuration, ModelError> {
        self.configurations
            .list
            .get(index)
            .ok_or_else(|| ModelError::InvalidDefinition(format!("no configuration {index}")))
    }

    fn check_name(&self, name: &str, except: Option<usize>) -> Result<(), ModelError> {
        if !is_valid_name(name) {
            return Err(ModelError::InvalidDefinition(format!(
                "\"{name}\" is not a valid configuration name"
            )));
        }
        let taken = self
            .configurations
            .list
            .iter()
            .enumerate()
            .any(|(i, c)| Some(i) != except && c.name == name);
        if taken {
            return Err(ModelError::InvalidDefinition(format!(
                "there is already a configuration named \"{name}\""
            )));
        }
        Ok(())
    }

    /// A snapshot of the part as it is, over the configured dimensions and features.
    fn snapshot(&self, name: &str) -> Configuration {
        Configuration {
            name: name.to_owned(),
            dimensions: self
                .configurations
                .configured_dimensions()
                .into_iter()
                .filter_map(|r| Some((r, self.current_value(r)?)))
                .collect(),
            suppressed: self
                .configurations
                .configured_features()
                .into_iter()
                .filter_map(|id| Some((id, self.feature(id)?.is_suppressed())))
                .collect(),
        }
    }

    /// Writes what the part says now into the active configuration.
    fn store_active(&mut self) {
        let active = self.configurations.active;
        let Some(name) = self.configurations.list.get(active).map(|c| c.name.clone()) else {
            return;
        };
        let snapshot = self.snapshot(&name);
        if let Some(c) = self.configurations.list.get_mut(active) {
            *c = snapshot;
        }
    }

    /// Adds a configuration named `name`, a copy of the part as it is, and returns its
    /// index. The first one added turns the part as it is into "Default" first. The new
    /// configuration is not made active.
    pub fn add_configuration(&mut self, name: &str) -> Result<usize, ModelError> {
        self.check_name(name, None)?;
        if self.configurations.list.is_empty() {
            if name == DEFAULT_CONFIGURATION {
                return Err(ModelError::InvalidDefinition(format!(
                    "\"{DEFAULT_CONFIGURATION}\" is the name of the configuration the part is now"
                )));
            }
            let default = self.snapshot(DEFAULT_CONFIGURATION);
            self.configurations.list.push(default);
            self.configurations.active = 0;
        }
        self.store_active();
        let copy = self.snapshot(name);
        self.configurations.list.push(copy);
        Ok(self.configurations.list.len() - 1)
    }

    pub fn rename_configuration(&mut self, index: usize, name: &str) -> Result<(), ModelError> {
        self.require_configuration(index)?;
        self.check_name(name, Some(index))?;
        if let Some(c) = self.configurations.list.get_mut(index) {
            c.name = name.to_owned();
        }
        Ok(())
    }

    /// Removes a configuration; the active one cannot go while others remain (activate
    /// another first). Removing the last one leaves the part without configurations, as
    /// it is.
    pub fn remove_configuration(&mut self, index: usize) -> Result<(), ModelError> {
        self.require_configuration(index)?;
        if self.configurations.list.len() == 1 {
            self.configurations = Configurations::default();
            return Ok(());
        }
        if index == self.configurations.active {
            return Err(ModelError::InvalidDefinition(
                "the active configuration cannot be removed: activate another first".to_owned(),
            ));
        }
        self.configurations.list.remove(index);
        if self.configurations.active > index {
            self.configurations.active -= 1;
        }
        Ok(())
    }

    /// Makes a dimension configurable: every configuration gets the part's current value
    /// of it. Dimensions driven by an equation cannot be configured.
    pub fn configure_dimension(&mut self, reference: DimensionRef) -> Result<(), ModelError> {
        let value = self
            .current_value(reference)
            .ok_or_else(|| ModelError::UnknownDimension(format!("{reference:?}")))?;
        if self.is_driven(reference) {
            return Err(ModelError::InvalidDefinition(
                "a dimension driven by an equation cannot be configured".to_owned(),
            ));
        }
        for c in &mut self.configurations.list {
            c.dimensions.entry(reference).or_insert(value);
        }
        Ok(())
    }

    /// Takes a dimension out of every configuration; its value stays as it is.
    pub fn unconfigure_dimension(&mut self, reference: DimensionRef) {
        for c in &mut self.configurations.list {
            c.dimensions.remove(&reference);
        }
    }

    /// Makes a feature's suppression configurable: every configuration gets its state now.
    pub fn configure_feature(&mut self, id: FeatureId) -> Result<(), ModelError> {
        let suppressed = self
            .feature(id)
            .ok_or(ModelError::UnknownFeature(id))?
            .is_suppressed();
        for c in &mut self.configurations.list {
            c.suppressed.entry(id).or_insert(suppressed);
        }
        Ok(())
    }

    pub fn unconfigure_feature(&mut self, id: FeatureId) {
        for c in &mut self.configurations.list {
            c.suppressed.remove(&id);
        }
    }

    /// Sets a configured dimension's value in configuration `index`; in the active one it
    /// is the part's value that changes (mm, radians or a count).
    pub fn set_configuration_value(
        &mut self,
        index: usize,
        reference: DimensionRef,
        value: f64,
    ) -> Result<(), ModelError> {
        self.require_configuration(index)?;
        if index == self.configurations.active {
            self.set_dimension(reference, value)?;
        }
        if let Some(c) = self.configurations.list.get_mut(index) {
            c.dimensions.insert(reference, value);
        }
        Ok(())
    }

    /// Sets whether configuration `index` suppresses a configured feature; in the active
    /// one the feature itself is suppressed or not.
    pub fn set_configuration_suppressed(
        &mut self,
        index: usize,
        id: FeatureId,
        suppressed: bool,
    ) -> Result<(), ModelError> {
        self.require_configuration(index)?;
        if index == self.configurations.active {
            self.set_suppressed(id, suppressed)?;
        }
        if let Some(c) = self.configurations.list.get_mut(index) {
            c.suppressed.insert(id, suppressed);
        }
        Ok(())
    }

    /// Makes configuration `index` the active one: the one leaving remembers the part as
    /// it is, then the part takes the values and suppressions of the one arriving. A value
    /// that cannot be set (its dimension is gone) is skipped.
    pub fn activate_configuration(&mut self, index: usize) -> Result<(), ModelError> {
        self.require_configuration(index)?;
        if index == self.configurations.active {
            return Ok(());
        }
        self.store_active();
        self.configurations.active = index;
        let Some(target) = self.configurations.list.get(index).cloned() else {
            return Ok(());
        };
        for (reference, value) in target.dimensions {
            let _ = self.set_dimension(reference, value);
        }
        for (id, suppressed) in target.suppressed {
            let _ = self.set_suppressed(id, suppressed);
        }
        Ok(())
    }

    /// The full name of a configured dimension, for the table's headings.
    pub fn configured_dimension_name(&self, reference: DimensionRef) -> Option<String> {
        let d = self.dimension(reference)?;
        let feature = self.feature(reference.feature)?;
        Some(full_name(&d.name, feature.name()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::{Extrude, ExtrudeOp, FeatureKind, FeatureParameter, Sketch, SketchPlane};
    use crate::{DimensionKey, StandardPlane};
    use oxyde_geom::Point2;

    /// A block whose depth is D1@Boss-Extrude1 and a second sketch to suppress.
    fn part() -> (Part, DimensionRef, FeatureId) {
        let mut part = Part::new();
        let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
        sketch.rectangle(Point2::ORIGIN, 40.0, 20.0);
        let s = part.add_feature(FeatureKind::Sketch(sketch));
        let boss = part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s,
            ExtrudeOp::Boss,
            10.0,
        )));
        let other = part.add_feature(FeatureKind::Sketch(Sketch::new(SketchPlane::Standard(
            StandardPlane::Top,
        ))));
        let depth = DimensionRef {
            feature: boss,
            key: DimensionKey::Feature(FeatureParameter::Depth),
        };
        (part, depth, other)
    }

    fn depth_of(part: &Part, depth: DimensionRef) -> f64 {
        part.dimension(depth).unwrap().value
    }

    #[test]
    fn a_deleted_feature_leaves_the_configurations() {
        let (mut part, depth, other) = part();
        part.add_configuration("Thick").unwrap();
        part.configure_dimension(depth).unwrap();
        part.configure_feature(other).unwrap();
        part.delete(other).unwrap();
        assert!(part.configurations().configured_features().is_empty());
        assert_eq!(part.configurations().configured_dimensions(), vec![depth]);
        part.delete(depth.feature).unwrap();
        assert!(part.configurations().configured_dimensions().is_empty());
    }

    #[test]
    fn the_first_configuration_makes_default_and_activation_swaps_values() {
        let (mut part, depth, other) = part();
        assert!(part.configurations().is_empty());
        let thick = part.add_configuration("Thick").unwrap();
        assert_eq!(thick, 1);
        assert_eq!(part.configurations().list[0].name, DEFAULT_CONFIGURATION);
        assert_eq!(part.configurations().active, 0);

        part.configure_dimension(depth).unwrap();
        part.configure_feature(other).unwrap();
        // Both configurations start from the part as it is.
        assert_eq!(part.configurations().list[1].dimensions[&depth], 10.0);
        part.set_configuration_value(thick, depth, 25.0).unwrap();
        part.set_configuration_suppressed(thick, other, true)
            .unwrap();
        // Not active: the part did not change.
        assert_eq!(depth_of(&part, depth), 10.0);
        assert!(!part.feature(other).unwrap().is_suppressed());

        part.activate_configuration(thick).unwrap();
        assert_eq!(depth_of(&part, depth), 25.0);
        assert!(part.feature(other).unwrap().is_suppressed());

        // What is done while Thick is active stays with Thick.
        part.set_dimension(depth, 30.0).unwrap();
        part.activate_configuration(0).unwrap();
        assert_eq!(depth_of(&part, depth), 10.0);
        assert!(!part.feature(other).unwrap().is_suppressed());
        assert_eq!(part.configurations().list[thick].dimensions[&depth], 30.0);

        // Editing the active configuration's cell edits the part.
        part.set_configuration_value(0, depth, 12.0).unwrap();
        assert_eq!(depth_of(&part, depth), 12.0);
    }

    #[test]
    fn names_are_checked_and_removal_keeps_the_active_one() {
        let (mut part, _, _) = part();
        assert!(part.add_configuration("").is_err());
        assert!(part.add_configuration(DEFAULT_CONFIGURATION).is_err());
        let a = part.add_configuration("A").unwrap();
        assert!(part.add_configuration("A").is_err());
        assert!(part.rename_configuration(a, "Default").is_err());
        part.rename_configuration(a, "B").unwrap();
        assert_eq!(part.configurations().list[a].name, "B");
        // The active one (Default) cannot go while B remains.
        assert!(part.remove_configuration(0).is_err());
        part.remove_configuration(a).unwrap();
        // The last one: the part is left without configurations.
        part.remove_configuration(0).unwrap();
        assert!(part.configurations().is_empty());
    }

    #[test]
    fn configurations_round_trip_through_their_text_form() {
        let (mut part, depth, other) = part();
        let thick = part.add_configuration("Thick").unwrap();
        part.configure_dimension(depth).unwrap();
        part.configure_feature(other).unwrap();
        part.set_configuration_value(thick, depth, 25.0).unwrap();
        part.set_configuration_suppressed(thick, other, true)
            .unwrap();
        part.activate_configuration(thick).unwrap();
        let definition = part.definition();
        let text = definition.configurations_text();
        assert_eq!(text.active, "Thick");
        assert_eq!(
            text.list[1].dimensions,
            vec![("D1@Boss-Extrude1".to_owned(), 25.0)]
        );
        assert_eq!(text.list[1].suppressed, vec![(other.raw(), true)]);

        let mut read = definition.clone();
        read.configurations = Configurations::default();
        read.set_configurations_text(text).unwrap();
        assert_eq!(read.configurations, definition.configurations);
    }
}
