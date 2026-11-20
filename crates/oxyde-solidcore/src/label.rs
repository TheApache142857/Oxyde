//! Where each face came from, carried on the face through solidcore's own edits.
//!
//! solidcore's application attributes are the kernel's record of what an application hangs on
//! its entities: an operation that splits a face hands the attribute to both pieces, one that
//! merges two faces asks it how to join them, and a boolean that finds a face lying on another
//! asks what the survivor carries. A [`Label`] holds the [`FaceSource`]s of one face; a split
//! keeps them on both pieces, a merge and a coincidence unite them. This is how solidcore-ops
//! carries an application's provenance (its `Carried`), written against the kernel's public
//! attribute interface.

use oxyde_kernel::FaceSource;
use solidcore::geom::attribute::{
    ApplicationValue, Attribute, AttributeHandlers, AttributeValue, AttributeView, Behavior,
    ObjectKey, Placement, ids,
};
use solidcore::{Brep, EntityRef};

/// The attribute id Oxyde's labels are stored under: the first one the kernel leaves to
/// applications.
pub(crate) const LABEL_ID: u64 = ids::USER_1;

/// The sources of one face, in a fixed order and without repeats, so that two rebuilds give
/// the same history.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Label(pub(crate) Vec<FaceSource>);

impl Label {
    pub(crate) fn one(source: FaceSource) -> Label {
        Label(vec![source])
    }

    /// Both labels' sources, in order, each once.
    fn union(&self, other: Option<&Label>) -> Label {
        let mut all = other.map(|o| o.0.clone()).unwrap_or_default();
        for s in &self.0 {
            if !all.contains(s) {
                all.push(*s);
            }
        }
        all.sort_by_key(sort_key);
        Label(all)
    }
}

/// A total order of sources, for a deterministic history.
fn sort_key(s: &FaceSource) -> (u8, u32, u32) {
    match *s {
        FaceSource::InputFace { input, face } => (0, input, face),
        FaceSource::InputEdge { input, edge } => (1, input, edge),
        FaceSource::InputVertex { input, vertex } => (2, input, vertex),
        FaceSource::OffsetFace { input, face } => (3, input, face),
        FaceSource::ProfileSegment(id) => (4, id, 0),
        FaceSource::StartCap => (5, 0, 0),
        FaceSource::EndCap => (6, 0, 0),
    }
}

/// The label on `x`, as the attribute view sees it during an edit.
fn on(
    view: &AttributeView<'_>,
    x: ObjectKey,
) -> Option<(solidcore::geom::attribute::AttributeHandle, Label)> {
    let (h, a) = view.find_attribute(x, LABEL_ID)?;
    a.application_value::<Label>().map(|l| (h, l.clone()))
}

/// Puts `new` on `x`, replacing the label it carries.
fn place(view: &AttributeView<'_>, x: ObjectKey, new: Label) -> Vec<Placement> {
    if let Some((handle, _)) = on(view, x) {
        return vec![Placement::Edit {
            handle,
            value: AttributeValue::Application(ApplicationValue::new(new)),
        }];
    }
    vec![Placement::Add {
        on: x,
        attribute: Attribute::application(LABEL_ID, Behavior::Copy, new),
    }]
}

impl AttributeHandlers for Label {
    /// A face cut in two: both pieces come from where the face came from.
    fn split(
        &self,
        view: &AttributeView<'_>,
        split: ObjectKey,
        a: ObjectKey,
        b: ObjectKey,
    ) -> solidcore::Result<Vec<Placement>> {
        let mut out = Vec::new();
        for x in [a, b] {
            if x != split {
                let now = on(view, x).map(|(_, l)| l);
                out.extend(place(view, x, self.union(now.as_ref())));
            }
        }
        Ok(out)
    }

    /// Two faces merged into one: the survivor comes from both.
    fn merge(
        &self,
        view: &AttributeView<'_>,
        a: ObjectKey,
        b: ObjectKey,
        result: ObjectKey,
    ) -> solidcore::Result<Vec<Placement>> {
        let mut now = on(view, result).map(|(_, l)| l);
        for x in [a, b] {
            if x == result {
                continue;
            }
            if let Some((_, from)) = on(view, x) {
                now = Some(from.union(now.as_ref()));
            }
        }
        Ok(now.map_or_else(Vec::new, |l| place(view, result, l)))
    }

    /// A face found lying on another, which survives: the survivor comes from both.
    fn coincident(
        &self,
        view: &AttributeView<'_>,
        _from: ObjectKey,
        to: ObjectKey,
    ) -> solidcore::Result<Vec<Placement>> {
        let now = on(view, to).map(|(_, l)| l);
        Ok(place(view, to, self.union(now.as_ref())))
    }
}

/// Puts `label` on face `e` of `b`, replacing any label it carries.
pub(crate) fn attach(b: &mut Brep, e: EntityRef, label: Label) -> solidcore::Result<()> {
    let old = b
        .entity_attributes(e)
        .into_iter()
        .find(|(_, a)| a.attribute_id() == LABEL_ID)
        .map(|(h, _)| h);
    if let Some(h) = old {
        b.remove_attribute(e, h)?;
    }
    b.add_attribute(e, Attribute::application(LABEL_ID, Behavior::Copy, label))?;
    Ok(())
}

/// The sources on face `e` of `b`, none when it carries no label.
pub(crate) fn read(b: &Brep, e: EntityRef) -> Vec<FaceSource> {
    b.find_attribute(e, LABEL_ID)
        .and_then(|a| a.application_value::<Label>())
        .map(|l| l.0.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_unite_in_a_fixed_order() {
        let a = Label(vec![FaceSource::InputFace { input: 1, face: 3 }]);
        let b = Label(vec![
            FaceSource::EndCap,
            FaceSource::InputFace { input: 0, face: 5 },
        ]);
        let u = a.union(Some(&b));
        assert_eq!(
            u.0,
            vec![
                FaceSource::InputFace { input: 0, face: 5 },
                FaceSource::InputFace { input: 1, face: 3 },
                FaceSource::EndCap,
            ]
        );
        assert_eq!(u.union(Some(&u)), u);
        assert_eq!(a.union(None), a);
    }
}
