//! Live previews of the feature commands. While Extrude, Revolve, Fillet, Shell, a pattern,
//! Combine or a reference plane or axis is open in the PropertyManager, the viewport shows
//! the part as the feature would make it, built by [`Part::preview`] on the last rebuild:
//! the faces the feature makes are tinted (red where it takes material away), a plane or
//! an axis is drawn where it would be, and the PropertyManager says why the feature
//! cannot be built before OK is clicked. Editing a feature shows the part as it is at that
//! feature, the way SolidWorks rolls back to a feature while it is edited; what comes
//! after it is rebuilt on OK.
//!
//! The preview's body is picked like the part's: faces and edges are told apart by their
//! persistent names, which the faces it shares with the part keep. What the feature
//! itself makes cannot be picked for it, but a fillet or chamfer face stands for the edge
//! it rounds, so clicking it takes the edge out again. The preview is built again when the
//! command changes: at once, or, when building it is slow, once the value being dragged
//! is let go.
//!
//! [`Part::preview`]: oxyde_model::Part::preview

use eframe::egui::{self, RichText};
use oxyde_kernel::Kernel;
use oxyde_model::{
    Body, CombineOp, ExtrudeOp, FaceKey, FeatureId, FeatureKind, FeaturePreview, FeatureStatus,
    ReferenceGeometry,
};
use oxyde_render::Rgb;

use crate::app::OxydeApp;
use crate::dialogs::Dialog;
use crate::document::{Document, next_revision};
use crate::icons::{self, Icon};
use crate::property_manager::PanelPick;

/// A preview that took longer than this to build (seconds) is not built again while a
/// value is being dragged.
const SLOW: f64 = 0.08;

/// The tint of the faces a feature adds, and of those where it takes material away.
const ADDED: Rgb = [236, 178, 64];
const REMOVED: Rgb = [222, 88, 80];

/// What a command makes: a feature, and the one it replaces when it is edited.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FeatureSpec {
    pub editing: Option<FeatureId>,
    pub kind: FeatureKind,
}

/// The open command's feature, built.
pub(crate) struct Preview {
    spec: FeatureSpec,
    /// The document revision it was built on, whose shapes it may share.
    revision: u64,
    /// `None` when the part is not rebuilt up to there.
    built: Option<FeaturePreview>,
    /// What the viewport shows: the document's own revision when that is the document's
    /// usual scene, else a revision of the preview's own.
    scene: u64,
    /// How long building it took.
    seconds: f64,
}

impl Preview {
    /// Builds `spec` on `doc`. `base_scene` is the scene of the part as it is before the
    /// feature edited, from the preview this one replaces, kept if it is shown again.
    fn build<K: Kernel>(
        doc: &Document,
        spec: FeatureSpec,
        base_scene: Option<u64>,
        kernel: &mut K,
    ) -> Preview {
        let start = std::time::Instant::now();
        let built = doc.part.preview(spec.kind.clone(), spec.editing, kernel);
        let seconds = start.elapsed().as_secs_f64();
        // Editing a feature that later features change: the part is shown as it is there.
        let rolled_back =
            spec.editing
                .is_some_and(|id| match (doc.part.body_before(id), doc.part.body()) {
                    (Some(before), Some(now)) => !std::ptr::eq(before, now),
                    (before, now) => before.is_some() != now.is_some(),
                });
        let made_body = built.as_ref().is_some_and(|b| b.body().is_some());
        let scene = if made_body {
            next_revision()
        } else if rolled_back {
            base_scene.unwrap_or_else(next_revision)
        } else {
            doc.revision
        };
        Preview {
            spec,
            revision: doc.revision,
            built,
            scene,
            seconds,
        }
    }

    /// The scene of the part before the feature edited, if that is what this preview
    /// shows, for a preview of `spec` replacing it.
    fn base_scene(&self, spec: &FeatureSpec) -> Option<u64> {
        (self.spec.editing == spec.editing && self.scene != self.revision && self.body().is_none())
            .then_some(self.scene)
    }

    fn body(&self) -> Option<&Body> {
        self.built.as_ref().and_then(FeaturePreview::body)
    }

    fn release<K: Kernel>(self, kernel: &mut K) {
        if let Some(built) = self.built {
            built.release(kernel);
        }
    }
}

/// The preview, if it is of the document as it is now.
fn live<'a>(doc: &Document, preview: Option<&'a Preview>) -> Option<&'a Preview> {
    preview.filter(|p| p.revision == doc.revision)
}

/// The body the viewport shows: the preview's, or the part as it is before the feature
/// being edited when that feature cannot be built, else the part's own.
pub(crate) fn shown_body<'a>(doc: &'a Document, preview: Option<&'a Preview>) -> Option<&'a Body> {
    let Some(p) = live(doc, preview) else {
        return doc.part.body();
    };
    if let Some(body) = p.body() {
        return Some(body);
    }
    if p.scene != doc.revision {
        return p.spec.editing.and_then(|id| doc.part.body_before(id));
    }
    doc.part.body()
}

/// The colour of the faces the previewed feature makes, and for each face of the shown
/// body whether it is one of them.
pub(crate) fn tint(doc: &Document, preview: Option<&Preview>) -> Option<(Rgb, Vec<bool>)> {
    let p = live(doc, preview)?;
    let built = p.built.as_ref()?;
    let body = built.body()?;
    let id = built.feature();
    let mut faces: Vec<bool> = (0..body.topology.faces.len() as u32)
        .map(|i| body.naming.face(i).is_some_and(|n| n.key.feature() == id))
        .collect();
    // A combine makes no faces of its own: the body it changes is the one shown.
    if let FeatureKind::Combine(c) = &p.spec.kind
        && let Some(solid) = body.solid(c.target)
    {
        for i in solid.faces.clone() {
            faces[i as usize] = true;
        }
    }
    let color = if removes(doc, &p.spec.kind) {
        REMOVED
    } else {
        ADDED
    };
    Some((color, faces))
}

/// Whether a feature takes material away: a cut or an intersection, or a pattern of them.
fn removes(doc: &Document, kind: &FeatureKind) -> bool {
    let removing = |op: ExtrudeOp| matches!(op, ExtrudeOp::Cut | ExtrudeOp::Intersect);
    let seeds = match kind {
        FeatureKind::Combine(c) => return matches!(c.op, CombineOp::Cut | CombineOp::Intersect),
        FeatureKind::LinearPattern(p) => &p.seeds,
        FeatureKind::CircularPattern(p) => &p.seeds,
        FeatureKind::Mirror(m) => &m.seeds,
        other => return other.sweep_op().is_some_and(removing),
    };
    !seeds.is_empty()
        && seeds.iter().all(|id| {
            doc.part
                .feature(*id)
                .and_then(|f| f.kind().sweep_op())
                .is_some_and(removing)
        })
}

impl OxydeApp {
    /// The feature the open command makes, once it has what it needs.
    pub(crate) fn dialog_feature(&self) -> Option<FeatureSpec> {
        let dialog = self.dialog.as_ref()?;
        let kind = match dialog {
            Dialog::Extrude(d) => d.feature(),
            Dialog::Revolve(d) => d.feature().ok(),
            Dialog::Sweep(d) => d.feature().ok(),
            Dialog::Loft(d) => d.feature().ok(),
            Dialog::Blend(d) => d.feature(),
            Dialog::Shell(d) => Some(d.feature()),
            Dialog::Hole(d) => d.feature(),
            Dialog::Pattern(d) => d.feature(&self.doc.part).ok(),
            Dialog::Combine(d) => d.feature().ok(),
            Dialog::RefPlane(d) => d.feature(),
            Dialog::RefAxis(d) => d.feature(),
            _ => None,
        }?;
        Some(FeatureSpec {
            editing: dialog.editing(),
            kind,
        })
    }

    /// Builds the preview again when the open command changed. Called before the scene is
    /// refreshed, after the PropertyManager.
    pub(crate) fn sync_preview(&mut self, ctx: &egui::Context) {
        let on = self.prefs.previews.features
            && self.sketch.is_none()
            && self.section.is_none()
            && self.mesh_export.is_none()
            && !self.busy();
        let Some(spec) = on.then(|| self.dialog_feature()).flatten() else {
            self.clear_preview();
            return;
        };
        let live_preview = live(&self.doc, self.preview.as_ref());
        if let Some(p) = live_preview {
            if p.spec == spec {
                return;
            }
            if p.seconds > SLOW && ctx.input(|i| i.pointer.any_down()) {
                return;
            }
        }
        // The part as it is before the edited feature, shown while that feature cannot be
        // built, keeps its scene as the command changes.
        let base_scene = live_preview.and_then(|p| p.base_scene(&spec));
        self.clear_preview();
        let preview = Preview::build(&self.doc, spec, base_scene, &mut self.kernel);
        self.debug_stats.preview = Some(preview.seconds);
        self.preview = Some(preview);
    }

    /// Counts the time the preview took to be shown (meshed) with the time it took to build.
    pub(crate) fn preview_shown_in(&mut self, seconds: f64) {
        if let Some(p) = self.preview.as_mut() {
            p.seconds += seconds;
            self.debug_stats.preview = Some(p.seconds);
        }
    }

    /// Drops the preview and frees its shapes.
    pub(crate) fn clear_preview(&mut self) {
        if let Some(p) = self.preview.take() {
            p.release(&mut self.kernel);
        }
    }

    /// What the viewport should show, as compared with `scene_revision`: the document's
    /// revision, or the preview's scene.
    pub(crate) fn scene_key(&self) -> u64 {
        match live(&self.doc, self.preview.as_ref()) {
            Some(p) => p.scene,
            None => self.doc.revision,
        }
    }

    /// The body shown in the viewport, which picks and highlights refer to.
    pub(crate) fn shown_body(&self) -> Option<&Body> {
        shown_body(&self.doc, self.preview.as_ref())
    }

    /// The plane or axis the open command makes, its name, and the feature it replaces.
    pub(crate) fn preview_reference(
        &self,
    ) -> Option<(Option<FeatureId>, String, ReferenceGeometry)> {
        let p = live(&self.doc, self.preview.as_ref())?;
        let built = p.built.as_ref()?;
        Some((p.spec.editing, built.name().to_owned(), built.reference()?))
    }

    /// Why the open command's feature cannot be built (`true`), or what its warning says.
    pub(crate) fn preview_problem(&self) -> Option<(bool, String)> {
        let built = live(&self.doc, self.preview.as_ref())?.built.as_ref()?;
        match built.status() {
            FeatureStatus::Error(message) => Some((true, message.clone())),
            FeatureStatus::Warning(message) => Some((false, message.clone())),
            _ => None,
        }
    }

    /// A click on a face the preview makes, as what it stands for: a fillet or chamfer
    /// face for the edge it rounds.
    pub(crate) fn unpreview_pick(&self, pick: PanelPick) -> PanelPick {
        let made_by = live(&self.doc, self.preview.as_ref())
            .and_then(|p| p.built.as_ref())
            .map(FeaturePreview::feature);
        match (&pick, made_by) {
            (PanelPick::Face(face), Some(id)) => match &face.key {
                FaceKey::FromEdge { feature, edge } if *feature == id => {
                    PanelPick::Edge(edge.clone())
                }
                _ => pick,
            },
            _ => pick,
        }
    }

    /// Why geometry clicked in the preview, which does not exist before the feature, cannot
    /// be used by it; `None` when the preview is not what the viewport shows.
    pub(crate) fn preview_refusal(&self, pick: &PanelPick) -> Option<String> {
        let built = live(&self.doc, self.preview.as_ref())?.built.as_ref()?;
        built.body()?;
        let (id, name) = (built.feature(), built.name());
        let made = match pick {
            PanelPick::Face(f) => f.key.feature() == id,
            PanelPick::Edge(e) => e.faces.iter().any(|f| f.key.feature() == id),
            _ => false,
        };
        Some(if made {
            format!("That is what {name} makes: pick geometry that exists before it")
        } else {
            format!("That only exists once {name} is made: pick geometry that exists before it")
        })
    }

    /// The end of a feature command's panel: the Preview switch, and why the feature
    /// cannot be built, or its warning.
    pub(crate) fn preview_footer(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let mut on = self.prefs.previews.features;
        if ui
            .checkbox(&mut on, "Preview")
            .on_hover_text(
                "Show the part as the feature makes it while the command is open, with the \
                 faces it makes tinted",
            )
            .changed()
        {
            self.prefs.previews.features = on;
            self.save_preferences();
        }
        if !on {
            return;
        }
        if let Some((error, text)) = self.preview_problem() {
            let tokens = crate::theme::tokens();
            ui.horizontal_wrapped(|ui| {
                let (icon, color) = if error {
                    (Icon::Error, tokens.err)
                } else {
                    (Icon::Warning, tokens.warn)
                };
                icons::small(ui, icon, 16.0);
                ui.label(RichText::new(text).color(color));
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use oxyde_geom::Point2;
    use oxyde_model::{EdgeBlend, EdgeName, Extrude, FaceName, Sketch, SketchPlane, StandardPlane};
    use oxyde_occt::OcctKernel;

    use super::*;

    // `Sketch::rectangle` makes the corner points first (ids 1–4), then the bottom,
    // right, top and left lines (5–8).
    const RIGHT: u32 = 6;

    /// A 40 × 20 × 10 block with a fillet on one edge, and a 10 × 10 sketch at its corner.
    fn block(kernel: &mut OcctKernel) -> (Document, FeatureId, FeatureId, FeatureId) {
        let mut doc = Document::new();
        let rectangle = |width, height| {
            let mut sketch = Sketch::new(SketchPlane::Standard(StandardPlane::Front));
            sketch.rectangle(Point2::ORIGIN, width, height);
            FeatureKind::Sketch(sketch)
        };
        let s1 = doc.part.add_feature(rectangle(40.0, 20.0));
        let base = doc.part.add_feature(FeatureKind::Extrude(Extrude::blind(
            s1,
            ExtrudeOp::Boss,
            10.0,
        )));
        let edge = EdgeName::between(FaceName::profile(base, RIGHT), FaceName::end_cap(base));
        let round = doc.part.add_feature(FeatureKind::Fillet(EdgeBlend {
            edges: vec![edge],
            size: 2.0,
            chamfer: Default::default(),
        }));
        let s2 = doc.part.add_feature(rectangle(10.0, 10.0));
        doc.rebuild(kernel)
            .expect("the preview test model rebuilds");
        (doc, base, round, s2)
    }

    #[test]
    fn a_cut_is_shown_with_the_faces_it_makes_in_red() {
        let mut kernel = OcctKernel::new();
        let (doc, _, _, s2) = block(&mut kernel);
        let mut cut = Extrude::blind(s2, ExtrudeOp::Cut, 10.0);
        cut.reverse = true;
        let spec = FeatureSpec {
            editing: None,
            kind: FeatureKind::Extrude(cut),
        };
        let preview = Preview::build(&doc, spec, None, &mut kernel);
        assert_ne!(preview.scene, doc.revision);
        let shown = shown_body(&doc, Some(&preview)).unwrap();
        assert!(std::ptr::eq(shown, preview.body().unwrap()));
        let (color, faces) = tint(&doc, Some(&preview)).unwrap();
        assert_eq!(color, REMOVED);
        assert_eq!(faces.len(), shown.topology.faces.len());
        // The two walls of the notch and its floor… none of the block's own faces.
        let made = faces.iter().filter(|t| **t).count();
        assert!((2..=3).contains(&made), "{made} faces tinted");

        // Once the part is rebuilt, the preview is of a part that is no more.
        let mut doc = doc;
        doc.revision = next_revision();
        assert!(std::ptr::eq(
            shown_body(&doc, Some(&preview)).unwrap(),
            doc.part.body().unwrap()
        ));
        assert!(tint(&doc, Some(&preview)).is_none());
        preview.release(&mut kernel);
        doc.release(&mut kernel);
    }

    #[test]
    fn an_edited_feature_that_fails_shows_the_part_before_it() {
        let mut kernel = OcctKernel::new();
        let (doc, base, round, _) = block(&mut kernel);
        let huge = FeatureSpec {
            editing: Some(round),
            kind: match doc.part.feature(round).unwrap().kind() {
                FeatureKind::Fillet(b) => FeatureKind::Fillet(EdgeBlend {
                    size: 50.0,
                    ..b.clone()
                }),
                other => panic!("{other:?}"),
            },
        };
        let preview = Preview::build(&doc, huge.clone(), None, &mut kernel);
        assert!(preview.body().is_none());
        assert_ne!(preview.scene, doc.revision);
        // The block without its fillet.
        let before = doc.part.body_before(round).unwrap();
        assert!(std::ptr::eq(
            shown_body(&doc, Some(&preview)).unwrap(),
            before
        ));
        // Still failing with another value, it keeps that scene.
        let base_scene = preview.base_scene(&huge);
        assert_eq!(base_scene, Some(preview.scene));
        let again = Preview::build(&doc, huge, base_scene, &mut kernel);
        assert_eq!(again.scene, preview.scene);
        preview.release(&mut kernel);
        again.release(&mut kernel);

        // Editing the block itself: its faces are the ones tinted, in the colour of what
        // is added.
        let spec = FeatureSpec {
            editing: Some(base),
            kind: doc.part.feature(base).unwrap().kind().clone(),
        };
        let preview = Preview::build(&doc, spec, None, &mut kernel);
        let (color, faces) = tint(&doc, Some(&preview)).unwrap();
        assert_eq!(color, ADDED);
        assert_eq!(faces.iter().filter(|t| **t).count(), 6);
        preview.release(&mut kernel);
        let mut doc = doc;
        doc.release(&mut kernel);
    }
}
