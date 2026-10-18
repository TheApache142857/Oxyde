//! Evaluate › Section View: the model cut open by a plane, to look inside it, as
//! SolidWorks' Section View. The cut is made for real by the kernel (every solid minus a
//! half-space), so what is shown is the true section: the cut faces are coloured, and
//! their area is given. It is a way of looking, not a feature: the model is unchanged,
//! nothing can be picked while the section is shown, and closing the window puts the
//! model back.
//!
//! Works in a part and in an assembly (every part shown, where it sits). The plane is one
//! of the three standard planes, or a flat face selected before the window was opened
//! (the cut is then made along that face, the side it looks to removed), moved along its
//! normal; *Flip* keeps the other half.
//!
//! Only the solids the plane goes through are cut: from the box around each, a solid
//! wholly on the kept side is shown as it is, with the display mesh it already has, and
//! one wholly on the removed side is left out. The cut solids are meshed at the display's
//! precision, so the faces the cut leaves alone keep their triangulation. In an assembly
//! the part files are cut on every core at once, as they are rebuilt when it opens
//! ([`Kernel::open_worker`]).

use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};

use eframe::egui::{self, RichText};
use oxyde_assembly::{Placement, Primitive};
use oxyde_geom::{Plane, Point3, Transform, Vec3};
use oxyde_kernel::{BooleanOp, FaceSource, Kernel, Mesh, ShapeHandle, SurfaceKind};
use oxyde_model::{Appearance, Body, Part, Solid, StandardPlane};
use oxyde_render::{Material as RenderMaterial, Scene, TextureRef};

use crate::app::{OxydeApp, Selected};
use crate::appearance_ui::material;
use crate::assembly::AsmSelected;
use crate::dialogs::plane_name;
use crate::icons::{self, Icon};

/// The plane the section is made on, before it is moved along its normal.
#[derive(Debug, Clone, PartialEq)]
pub enum SectionBase {
    Standard(StandardPlane),
    /// A flat face selected when the window was opened, in the document's coordinates (an
    /// assembly's for a part placed in one), its normal pointing out of the solid.
    Face {
        plane: Plane,
        label: String,
    },
}

impl SectionBase {
    pub fn plane(&self) -> Plane {
        match self {
            SectionBase::Standard(s) => s.plane(),
            SectionBase::Face { plane, .. } => *plane,
        }
    }

    /// The name shown for it: `Front`, or the face's label.
    pub fn name(&self) -> String {
        match self {
            SectionBase::Standard(s) => plane_name(*s).to_owned(),
            SectionBase::Face { label, .. } => label.clone(),
        }
    }
}

/// Where the cut is made.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionSettings {
    pub plane: SectionBase,
    /// Distance of the section plane from the base plane, along its normal (mm).
    pub offset: f64,
    /// Keep the half the normal points away from, instead of removing it.
    pub flip: bool,
    /// Colour the faces the cut makes, rather than drawing them like the rest.
    pub color_caps: bool,
    pub cap_color: [u8; 3],
}

impl Default for SectionSettings {
    fn default() -> Self {
        Self {
            plane: SectionBase::Standard(StandardPlane::Front),
            offset: 0.0,
            flip: false,
            color_caps: true,
            cap_color: [214, 96, 70],
        }
    }
}

impl SectionSettings {
    /// The section plane: the base plane moved by `offset`, its normal pointing to the
    /// half that is removed.
    pub fn section_plane(&self) -> Plane {
        let base = self.plane.plane();
        let mut plane = base.offset(self.offset);
        if self.flip {
            // Mirror the normal and keep a right-handed frame: swap the in-plane axes.
            plane = Plane {
                origin: plane.origin,
                x_dir: base.y_dir(),
                normal: base.normal * -1.0,
            };
        }
        plane
    }
}

/// What a section computed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SectionReport {
    /// Area of the cut faces, mm².
    pub area: f64,
    /// Solids the plane goes through, and solids looked at in all.
    pub cut: usize,
    pub solids: usize,
    pub took: f64,
}

/// The Section View window, while it is open.
pub struct SectionView {
    pub settings: SectionSettings,
    /// The flat face selected when the window was opened, offered next to the standard
    /// planes.
    face: Option<SectionBase>,
    /// The settings and document revision the viewport shows the section of.
    shown: Option<(SectionSettings, u64)>,
    /// Where the model is along the plane's normal, for the slider.
    range: Option<(f64, f64)>,
    /// The boxes around the model, at the document revision they were measured at.
    bounds: Option<Bounds>,
    report: Option<SectionReport>,
    error: Option<String>,
}

impl SectionView {
    fn new() -> Self {
        Self {
            settings: SectionSettings::default(),
            face: None,
            shown: None,
            range: None,
            bounds: None,
            report: None,
            error: None,
        }
    }

    /// Where the box `lo..hi` lies along the plane's normal, as offsets from the plane:
    /// the offsets that go through it.
    fn offsets_through(plane: &Plane, lo: Point3, hi: Point3) -> (f64, f64) {
        extent_along(plane, corners(lo, hi))
    }
}

/// The eight corners of the box `lo..hi`.
fn corners(lo: Point3, hi: Point3) -> [Point3; 8] {
    let mut out = [lo; 8];
    for (i, corner) in out.iter_mut().enumerate() {
        *corner = Point3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
    }
    out
}

/// Where `points` lie along the plane's normal, as offsets from the plane: the least and
/// the greatest.
fn extent_along(plane: &Plane, points: impl IntoIterator<Item = Point3>) -> (f64, f64) {
    let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in points {
        let d = (p - plane.origin).dot(plane.normal);
        min = min.min(d);
        max = max.max(d);
    }
    (min, max)
}

/// Where a solid lies against the section plane, whose normal points to the removed half,
/// from the box around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    /// Wholly on the kept side: shown as it is, no cut made.
    Kept,
    /// Wholly on the removed side: not shown.
    Removed,
    /// The plane goes through the box: cut.
    Through,
}

fn side_of(plane: &Plane, (lo, hi): (Point3, Point3)) -> Side {
    let (min, max) = extent_along(plane, corners(lo, hi));
    if max <= 0.0 {
        Side::Kept
    } else if min >= 0.0 {
        Side::Removed
    } else {
        Side::Through
    }
}

/// One body to cut, and where it is shown: a part's body once, at the origin; in an
/// assembly, the body of a part file at every place the file is used. Units are cut on
/// several cores at once, so a unit holds everything that shares shapes: a whole body.
struct Unit<'a> {
    part: &'a Part,
    body: &'a Body,
    /// The box around each solid of the body, by index, when known on the main thread; a
    /// solid without one is asked of the kernel.
    bounds: Vec<Option<(Point3, Point3)>>,
    /// The display precision of the body, the same as the whole model is drawn at: the
    /// faces the cut leaves as they were keep the triangulation the display made.
    deflection: f64,
    copies: Vec<Placed<'a>>,
}

impl Unit<'_> {
    /// About how long the unit takes against `plane` (world coordinates): the faces of the
    /// solids the plane goes through, at every place the body is shown; a solid the plane
    /// misses costs next to nothing, and one whose box is unknown is counted as cut.
    fn cost(&self, plane: &Plane) -> usize {
        let mut cost = 0;
        for placed in &self.copies {
            let local = plane.transformed(&placed.placement.inverse().transform());
            for (i, solid) in self.body.solids.iter().enumerate() {
                let side = self
                    .bounds
                    .get(i)
                    .copied()
                    .flatten()
                    .map_or(Side::Through, |b| side_of(&local, b));
                cost += match side {
                    Side::Through => solid.faces.len().max(1),
                    Side::Kept => 1,
                    Side::Removed => 0,
                };
            }
        }
        cost
    }
}

/// One place a unit's body is shown at.
struct Placed<'a> {
    placement: Placement,
    /// The display mesh of the whole body, faces numbered as its topology, reused for the
    /// solids the plane misses.
    mesh: Option<&'a Mesh>,
    /// What the whole part is drawn with when it has one (a component appearance in an
    /// assembly).
    own: Option<Appearance>,
}

/// What a face of a piece is: one the cut made, or a face of the part (its index in the
/// part's topology, when the kernel says which).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PieceFace {
    Cap,
    Of(Option<u32>),
}

/// The boxes around the model at a document revision. They are wanted every frame (the
/// slider's range) and at every cut (which solids the plane goes through), and asking the
/// kernel for an exact box takes a fifth of a second on a big imported part.
struct Bounds {
    revision: u64,
    /// Around the whole model, in world coordinates.
    all: Option<(Point3, Point3)>,
    /// Around each solid of the part, by its index in the body; empty in an assembly, where
    /// the parts' display meshes give them.
    solids: Vec<Option<(Point3, Point3)>>,
}

/// The box around a solid from the display mesh of its body (faces numbered as the body's
/// topology), grown by `margin`: the mesh's points lie on the surface, so the true box is
/// within the mesh's deflection of the mesh's.
fn mesh_solid_bounds(
    mesh: &Mesh,
    body: &Body,
    solid: &Solid,
    margin: f64,
) -> Option<(Point3, Point3)> {
    if mesh.faces.len() != body.topology.faces.len() {
        return None;
    }
    let faces = mesh
        .faces
        .get(solid.faces.start as usize..solid.faces.end as usize)?;
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in faces.iter().flat_map(|f| f.positions.iter()) {
        for ((l, h), &v) in lo.iter_mut().zip(hi.iter_mut()).zip(p) {
            *l = l.min(f64::from(v));
            *h = h.max(f64::from(v));
        }
    }
    if lo[0] > hi[0] {
        return None;
    }
    Some((
        Point3::new(lo[0] - margin, lo[1] - margin, lo[2] - margin),
        Point3::new(hi[0] + margin, hi[1] + margin, hi[2] + margin),
    ))
}

/// One solid as the section shows it: its mesh in the body's own coordinates, and where
/// it sits.
struct Piece {
    placement: Placement,
    own: Option<Appearance>,
    mesh: Mesh,
    /// One per face of the mesh.
    faces: Vec<PieceFace>,
    /// Area of its cut faces, mm²; 0 when the plane misses it.
    cap_area: f64,
}

/// The half-space on the side `plane`'s normal points to, as a box big enough to hold
/// everything within `reach` mm of the plane's origin.
fn half_space<K: Kernel>(kernel: &mut K, plane: &Plane, reach: f64) -> Result<ShapeHandle, String> {
    let l = reach.max(1.0);
    let raw = kernel
        .make_box(Point3::new(-l, -l, 0.0), Vec3::new(2.0 * l, 2.0 * l, l))
        .map_err(|e| e.to_string())?;
    let placed = kernel.transform(raw, &Transform::from_plane(plane));
    kernel.release(raw);
    Ok(placed.map_err(|e| e.to_string())?.shape)
}

/// A solid the plane misses, as the display shows it: from the body's display mesh when
/// the unit has it, else meshed again (the kernel keeps the triangulation the display
/// made, so this is quick).
fn whole_piece<K: Kernel>(
    kernel: &K,
    unit: &Unit,
    placed: &Placed,
    solid: &Solid,
) -> Result<Piece, String> {
    let topology = &unit.body.topology;
    let range = |r: &Range<u32>| r.start as usize..r.end as usize;
    let shown = placed
        .mesh
        .filter(|m| m.faces.len() == topology.faces.len() && m.edges.len() == topology.edges.len())
        .and_then(|m| {
            Some(Mesh {
                faces: m.faces.get(range(&solid.faces))?.to_vec(),
                edges: m.edges.get(range(&solid.edges))?.to_vec(),
            })
        });
    let mesh = match shown {
        Some(mesh) => mesh,
        None => kernel
            .tessellate(solid.shape, unit.deflection)
            .map_err(|e| e.to_string())?,
    };
    let faces = (0..mesh.faces.len() as u32)
        .map(|i| PieceFace::Of(Some(solid.faces.start + i)))
        .collect();
    Ok(Piece {
        placement: placed.placement,
        own: placed.own.clone(),
        mesh,
        faces,
        cap_area: 0.0,
    })
}

/// A solid the plane goes through, cut by `tool` (in the body's coordinates) and meshed;
/// `None` when nothing of it is left.
fn cut_piece<K: Kernel>(
    kernel: &mut K,
    unit: &Unit,
    placed: &Placed,
    solid: &Solid,
    tool: ShapeHandle,
) -> Result<Option<Piece>, String> {
    let result = kernel
        .boolean(BooleanOp::Cut, solid.shape, tool)
        .map_err(|e| e.to_string())?;
    let topology = kernel.topology(result.shape);
    let mesh = match &topology {
        Ok(t) if !t.faces.is_empty() => Some(kernel.tessellate(result.shape, unit.deflection)),
        _ => None,
    };
    kernel.release(result.shape);
    let topology = topology.map_err(|e| e.to_string())?;
    let Some(mesh) = mesh else {
        // The whole solid was on the removed side.
        return Ok(None);
    };
    let mesh = mesh.map_err(|e| e.to_string())?;
    let mut faces = Vec::with_capacity(topology.faces.len());
    let mut cap_area = 0.0;
    for (i, face) in topology.faces.iter().enumerate() {
        let sources = result
            .history
            .faces
            .get(i)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        // A face of the tool is a face the cut made; one of the solid keeps its look.
        let from_tool = sources
            .iter()
            .any(|s| matches!(s, FaceSource::InputFace { input: 1, .. }));
        if from_tool {
            cap_area += face.area;
            faces.push(PieceFace::Cap);
        } else {
            let from_solid = sources.iter().find_map(|s| match s {
                FaceSource::InputFace { input: 0, face } => Some(solid.faces.start + *face),
                _ => None,
            });
            faces.push(PieceFace::Of(from_solid));
        }
    }
    Ok(Some(Piece {
        placement: placed.placement,
        own: placed.own.clone(),
        mesh,
        faces,
        cap_area,
    }))
}

/// The pieces of a unit's solids at one place: the tool is brought into the body's
/// coordinates rather than the body into the world, and made only if a solid needs it.
fn cut_placed<K: Kernel>(
    kernel: &mut K,
    plane: &Plane,
    reach: f64,
    unit: &Unit,
    placed: &Placed,
    solids: &[(&Solid, Option<(Point3, Point3)>)],
    out: &mut Vec<Piece>,
) -> Result<(), String> {
    let local = plane.transformed(&placed.placement.inverse().transform());
    let mut tool: Option<ShapeHandle> = None;
    let mut cut_all = || -> Result<(), String> {
        for (solid, bounds) in solids {
            // A solid whose box the kernel cannot give is cut, which is right for it too.
            match bounds.map_or(Side::Through, |b| side_of(&local, b)) {
                Side::Removed => {}
                Side::Kept => out.push(whole_piece(kernel, unit, placed, solid)?),
                Side::Through => {
                    let tool = match tool {
                        Some(t) => t,
                        None => *tool.insert(half_space(kernel, &local, reach)?),
                    };
                    if let Some(piece) = cut_piece(kernel, unit, placed, solid, tool)? {
                        out.push(piece);
                    }
                }
            }
        }
        Ok(())
    };
    let outcome = cut_all();
    if let Some(t) = tool {
        kernel.release(t);
    }
    outcome
}

/// Every piece of a unit: its solids at each of its places, those the plane misses as
/// they are, those it goes through cut by the half-space on the plane's normal side.
fn cut_unit<K: Kernel>(
    kernel: &mut K,
    plane: &Plane,
    reach: f64,
    unit: &Unit,
) -> Result<Vec<Piece>, String> {
    let solids: Vec<(&Solid, Option<(Point3, Point3)>)> = unit
        .body
        .solids
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let known = unit.bounds.get(i).copied().flatten();
            (s, known.or_else(|| kernel.bounds(s.shape).ok()))
        })
        .collect();
    let mut out = Vec::new();
    for placed in &unit.copies {
        cut_placed(kernel, plane, reach, unit, placed, &solids, &mut out)?;
    }
    Ok(out)
}

/// Cuts every unit, shared out between the calling thread and one worker per further
/// core, each worker with a kernel of its own ([`Kernel::open_worker`]) that shares the
/// shapes; a kernel that cannot open workers cuts them one by one. The results are in
/// unit order.
fn cut_units<K: Kernel>(
    kernel: &mut K,
    plane: &Plane,
    reach: f64,
    units: &[Unit],
) -> Vec<Result<Vec<Piece>, String>> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(units.len());
    let next = AtomicUsize::new(0);
    // Takes units from the queue until it is empty.
    let work = |kernel: &mut K| {
        let mut done = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(unit) = units.get(i) else {
                break;
            };
            done.push((i, cut_unit(kernel, plane, reach, unit)));
        }
        done
    };
    let mut results: Vec<Option<Result<Vec<Piece>, String>>> = units.iter().map(|_| None).collect();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (1..threads)
            .map(|_| {
                scope.spawn(|| match K::open_worker() {
                    Some(mut kernel) => work(&mut kernel),
                    None => Vec::new(),
                })
            })
            .collect();
        let mut done = work(kernel);
        for worker in workers {
            // A worker that panicked leaves its units uncut; they are reported below.
            done.extend(worker.join().unwrap_or_default());
        }
        for (i, result) in done {
            if let Some(slot) = results.get_mut(i) {
                *slot = Some(result);
            }
        }
    });
    results
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err("the solid could not be cut".to_owned())))
        .collect()
}

/// How far the half-space must reach from the plane's origin to cover the box.
fn reach(plane: &Plane, (lo, hi): (Point3, Point3)) -> f64 {
    let centre = Point3::new(
        (lo.x + hi.x) / 2.0,
        (lo.y + hi.y) / 2.0,
        (lo.z + hi.z) / 2.0,
    );
    let radius = (hi - lo).length() / 2.0;
    (centre - plane.origin).length() + 2.0 * radius + 1.0
}

impl OxydeApp {
    /// Evaluate › Section View: opens the window, or closes it and shows the model whole.
    pub(crate) fn toggle_section_view(&mut self) {
        if self.section.is_some() {
            self.close_section_view();
            return;
        }
        let mut view = SectionView::new();
        // A flat face selected first: the cut is made along it, the side it looks to
        // removed, as SolidWorks does with a face as the section plane.
        view.face = self.selected_section_face();
        if let Some(face) = &view.face {
            view.settings.plane = face.clone();
            view.settings.offset = 0.0;
        }
        let bounds = self.model_bounds();
        let all = bounds.all;
        view.bounds = Some(bounds);
        if let Some((lo, hi)) = all {
            let (a, b) = SectionView::offsets_through(&view.settings.plane.plane(), lo, hi);
            view.range = Some((a, b));
            if view.face.is_none() {
                view.settings.offset = (a + b) / 2.0;
            }
        }
        self.status = match &view.face {
            Some(face) => format!(
                "Section View along the {}: move the plane with the slider, Flip keeps the \
                 other half; close the window to see the whole model again",
                face.name().to_lowercase()
            ),
            None => "Section View: move the plane with the slider (select a flat face first \
                     to cut along it); close the window to see the whole model again"
                .to_owned(),
        };
        self.section = Some(view);
    }

    /// The first flat face selected, as a section plane in the document's coordinates:
    /// in a part, one of its faces; in an assembly, a face of a part where it sits.
    fn selected_section_face(&self) -> Option<SectionBase> {
        if let Some(doc) = self.asm.as_ref() {
            return doc.selection.iter().find_map(|item| {
                let AsmSelected::Face(o, name) = item else {
                    return None;
                };
                let body = doc.part_at(o)?.part.body()?;
                let face = body
                    .topology
                    .faces
                    .get(body.naming.find_face(name)? as usize)?;
                if face.surface != SurfaceKind::Plane {
                    return None;
                }
                let Primitive::Plane { origin, normal } =
                    doc.placed(&doc.assembly, &item.mate_ref()?)?
                else {
                    return None;
                };
                let plane = Plane::from_point_normal(origin, normal)?;
                let mut names = vec![doc.assembly.component(o.top)?.name.clone()];
                names.extend(doc.inner_names(o.top, &o.path));
                Some(SectionBase::Face {
                    plane,
                    label: format!("Face of {}", names.join(" › ")),
                })
            });
        }
        let body = self.doc.part.body()?;
        self.selection.iter().find_map(|s| {
            let Selected::Face(name) = s else {
                return None;
            };
            let face = body
                .topology
                .faces
                .get(body.naming.find_face(name)? as usize)?;
            if face.surface != SurfaceKind::Plane {
                return None;
            }
            let plane = Plane::from_point_normal(face.centroid, face.normal?)?;
            Some(SectionBase::Face {
                plane,
                label: "Selected face".to_owned(),
            })
        })
    }

    pub(crate) fn close_section_view(&mut self) {
        if self.section.take().is_none() {
            return;
        }
        // The usual scene comes back on the next frame.
        self.viewport.pickable = None;
        self.scene_revision = None;
        self.asm_scene = None;
    }

    /// The boxes around the open document: the part's solids, or every part of the
    /// assembly where it sits.
    fn model_bounds(&self) -> Bounds {
        let revision = self.section_revision();
        match &self.asm {
            Some(doc) => Bounds {
                revision,
                all: doc.bounds(&doc.assembly),
                solids: Vec::new(),
            },
            None => {
                let solids: Vec<Option<(Point3, Point3)>> = self
                    .doc
                    .part
                    .body()
                    .map(|body| {
                        body.solids
                            .iter()
                            .map(|s| self.kernel.bounds(s.shape).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                let all =
                    solids
                        .iter()
                        .flatten()
                        .fold(None, |all: Option<(Point3, Point3)>, &(a, b)| {
                            Some(match all {
                                None => (a, b),
                                Some((lo, hi)) => (
                                    Point3::new(lo.x.min(a.x), lo.y.min(a.y), lo.z.min(a.z)),
                                    Point3::new(hi.x.max(b.x), hi.y.max(b.y), hi.z.max(b.z)),
                                ),
                            })
                        });
                Bounds {
                    revision,
                    all,
                    solids,
                }
            }
        }
    }

    fn section_revision(&self) -> u64 {
        match &self.asm {
            Some(asm) => asm.revision,
            None => self.doc.revision,
        }
    }

    /// The box around the open document, measured once per document revision.
    fn section_bounds(&mut self) -> Option<(Point3, Point3)> {
        let revision = self.section_revision();
        let measured = self
            .section
            .as_ref()
            .and_then(|v| v.bounds.as_ref())
            .filter(|b| b.revision == revision);
        if let Some(bounds) = measured {
            return bounds.all;
        }
        let bounds = self.model_bounds();
        let all = bounds.all;
        if let Some(view) = self.section.as_mut() {
            view.bounds = Some(bounds);
        }
        all
    }

    /// The Section View window, and the section in the viewport while it is open.
    pub(crate) fn section_window(&mut self, ctx: &egui::Context) {
        if self.section.is_none() {
            return;
        }
        // The mesh export preview owns the viewport while its window is open.
        if self.mesh_export.is_some() {
            self.close_section_view();
            return;
        }
        let mut open = true;
        let mut look = false;
        let mut centre = false;
        let bounds = self.section_bounds();
        let Some(view) = self.section.as_mut() else {
            return;
        };
        if let Some((lo, hi)) = bounds {
            view.range = Some(SectionView::offsets_through(
                &view.settings.plane.plane(),
                lo,
                hi,
            ));
        }
        egui::Window::new("Section View")
            .open(&mut open)
            .resizable(false)
            .default_pos(ctx.screen_rect().right_top() + egui::vec2(-340.0, 220.0))
            .show(ctx, |ui| {
                let s = &mut view.settings;
                let face = view.face.as_ref();
                egui::Grid::new("section_view")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Plane");
                        ui.horizontal(|ui| {
                            for plane in [
                                StandardPlane::Front,
                                StandardPlane::Top,
                                StandardPlane::Right,
                            ] {
                                let base = SectionBase::Standard(plane);
                                if ui
                                    .selectable_label(s.plane == base, plane_name(plane))
                                    .clicked()
                                    && s.plane != base
                                {
                                    s.plane = base;
                                    centre = true;
                                }
                            }
                            match face {
                                Some(face) => {
                                    if ui
                                        .selectable_label(s.plane == *face, "Face")
                                        .on_hover_text(format!(
                                            "{}: the flat face selected when the window \
                                             was opened; the side it looks to is removed",
                                            face.name()
                                        ))
                                        .clicked()
                                        && s.plane != *face
                                    {
                                        s.plane = face.clone();
                                        s.offset = 0.0;
                                    }
                                }
                                None => {
                                    ui.add_enabled(
                                        false,
                                        egui::SelectableLabel::new(false, "Face"),
                                    )
                                    .on_disabled_hover_text(
                                        "Select a flat face before opening Section View \
                                             to cut along it",
                                    );
                                }
                            }
                        });
                        ui.end_row();
                        ui.label("Offset");
                        ui.horizontal(|ui| {
                            let (lo, hi) = view.range.unwrap_or((-100.0, 100.0));
                            let margin = ((hi - lo) * 0.02).max(0.5);
                            ui.add(
                                egui::Slider::new(&mut s.offset, (lo - margin)..=(hi + margin))
                                    .show_value(false),
                            )
                            .on_hover_text("Where the model is along the plane's normal");
                            ui.add(egui::DragValue::new(&mut s.offset).speed(0.5).suffix(" mm"));
                        });
                        ui.end_row();
                        ui.label("Side");
                        ui.horizontal(|ui| {
                            if ui
                                .button(if s.flip { "Flip back" } else { "Flip" })
                                .on_hover_text("Keep the other half")
                                .clicked()
                            {
                                s.flip = !s.flip;
                            }
                            if ui
                                .button("Centre")
                                .on_hover_text("Put the plane through the middle of the model")
                                .clicked()
                            {
                                centre = true;
                            }
                            if ui
                                .button("Look at")
                                .on_hover_text("Turn the view to face the section")
                                .clicked()
                            {
                                look = true;
                            }
                        });
                        ui.end_row();
                        ui.label("Cut faces");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut s.color_caps, "Coloured");
                            ui.add_enabled_ui(s.color_caps, |ui| {
                                ui.color_edit_button_srgb(&mut s.cap_color);
                            });
                        });
                        ui.end_row();
                    });
                ui.add_space(6.0);
                match (&view.report, &view.error) {
                    (_, Some(e)) => {
                        ui.label(
                            RichText::new(format!("Could not cut: {e}"))
                                .color(egui::Color32::LIGHT_RED),
                        );
                    }
                    (Some(r), None) => {
                        egui::Grid::new("section_report")
                            .num_columns(2)
                            .spacing([16.0, 4.0])
                            .show(ui, |ui| {
                                ui.label("Section area");
                                ui.label(RichText::new(format!("{:.2} mm²", r.area)).monospace());
                                ui.end_row();
                                ui.label("Solids cut");
                                ui.label(
                                    RichText::new(format!("{} of {}", r.cut, r.solids)).monospace(),
                                );
                                ui.end_row();
                            });
                        ui.label(
                            RichText::new(format!("Cut in {:.0} ms", r.took * 1000.0))
                                .small()
                                .weak(),
                        );
                    }
                    (None, None) => {
                        ui.label(RichText::new("Cutting…").weak());
                    }
                }
                ui.label(
                    RichText::new(
                        "The model itself is unchanged; nothing can be picked while the \
                         section is shown. Close the window to see the whole model.",
                    )
                    .small()
                    .weak(),
                );
            });
        if let Some((lo, hi)) = view.range.filter(|_| centre) {
            view.settings.offset = (lo + hi) / 2.0;
        }
        let plane = view.settings.section_plane();
        let revision = self.section_revision();
        let dirty = self.section.as_ref().is_some_and(|v| {
            v.shown
                .as_ref()
                .is_none_or(|(s, r)| *s != v.settings || *r != revision)
        });
        if look {
            // Face the kept half: look against the normal, from the removed side.
            self.viewport.look_at_plane(&plane);
        }
        if !open {
            self.close_section_view();
            return;
        }
        // Cut once the slider is let go of: a boolean per solid is too slow to follow a
        // drag on a big assembly.
        let dragging = ctx.input(|i| i.pointer.any_down());
        if dirty && !dragging && !self.busy() {
            self.compute_section();
        } else if dirty {
            ctx.request_repaint();
        }
    }

    /// Cuts the model with the current settings and shows the result in the viewport.
    fn compute_section(&mut self) {
        let Some(view) = self.section.as_ref() else {
            return;
        };
        let settings = view.settings.clone();
        let revision = self.section_revision();
        let plane = settings.section_plane();
        let fraction = self.mesh_fraction();
        let start = std::time::Instant::now();
        let result = match self.section_bounds() {
            None => Err("there is no solid".to_owned()),
            Some(bounds) => {
                let reach = reach(&plane, bounds);
                self.cut_document(&plane, reach, &settings, fraction)
            }
        };
        let took = start.elapsed().as_secs_f64();
        let Some(view) = self.section.as_mut() else {
            return;
        };
        view.shown = Some((settings, revision));
        match result {
            Ok((mesh, materials, textures, mut report)) => {
                report.took = took;
                view.report = Some(report);
                view.error = None;
                let mut mesh = mesh;
                let mut materials = materials;
                self.add_context(&mut mesh, &mut materials);
                let mut scene = Scene::from_mesh(&mesh);
                scene.set_materials(materials, textures);
                // Looked at, not picked: the faces are the cut's, not the model's.
                self.viewport.pickable = Some((0, 0));
                self.viewport.set_scene(scene, false);
                // The usual scene is not rebuilt over the section until the window closes.
                if self.asm.is_some() {
                    self.asm_scene = Some(revision);
                } else {
                    self.scene_revision = Some(revision);
                }
            }
            Err(e) => {
                view.report = None;
                view.error = Some(e);
            }
        }
    }

    /// Every solid of the document against `plane` (in world coordinates, its normal to
    /// the removed half), as one mesh with a material per face, and the report.
    #[allow(clippy::type_complexity)]
    fn cut_document(
        &mut self,
        plane: &Plane,
        reach: f64,
        settings: &SectionSettings,
        fraction: f64,
    ) -> Result<(Mesh, Vec<RenderMaterial>, Vec<TextureRef>, SectionReport), String> {
        // ---- The units: a part's body, or every part file of the assembly with the
        // places it is shown at.
        let mut units: Vec<Unit> = Vec::new();
        match &self.asm {
            Some(doc) => {
                for (occurrence, placement, loaded) in doc.leaves(&doc.assembly) {
                    let Some(body) = loaded.part.body() else {
                        continue;
                    };
                    // The boxes from the display meshes, grown by their deflection.
                    let margin = oxyde_render::mesh_deflection(&body.topology, doc.mesh_fraction);
                    let bounds = body
                        .solids
                        .iter()
                        .map(|s| {
                            loaded
                                .mesh
                                .as_ref()
                                .and_then(|m| mesh_solid_bounds(m, body, s, margin))
                        })
                        .collect();
                    let placed = Placed {
                        placement,
                        mesh: loaded.mesh.as_ref(),
                        own: doc
                            .occurrence_appearance(&doc.assembly, &occurrence)
                            .cloned(),
                    };
                    match units
                        .iter_mut()
                        .find(|u| std::ptr::eq(u.part, &loaded.part))
                    {
                        Some(unit) => unit.copies.push(placed),
                        None => units.push(Unit {
                            part: &loaded.part,
                            body,
                            bounds,
                            deflection: oxyde_render::mesh_deflection(&body.topology, fraction),
                            copies: vec![placed],
                        }),
                    }
                }
            }
            None => {
                let part: &Part = &self.doc.part;
                if let Some(body) = part.body() {
                    // The exact boxes measured with the model's (`section_bounds`).
                    let known = self
                        .section
                        .as_ref()
                        .and_then(|v| v.bounds.as_ref())
                        .map(|b| b.solids.as_slice())
                        .unwrap_or(&[]);
                    units.push(Unit {
                        part,
                        body,
                        bounds: (0..body.solids.len())
                            .map(|i| known.get(i).copied().flatten())
                            .collect(),
                        deflection: oxyde_render::mesh_deflection(&body.topology, fraction),
                        copies: vec![Placed {
                            placement: Placement::IDENTITY,
                            mesh: None,
                            own: None,
                        }],
                    });
                }
            }
        }
        if units.is_empty() {
            return Err("there is no solid".to_owned());
        }
        // The slowest units first: handed out in tree order, a big part listed late ran
        // alone at the end while the other cores had nothing left to do.
        units.sort_by_key(|u| std::cmp::Reverse(u.cost(plane)));
        let results = cut_units(&mut self.kernel, plane, reach, &units);

        // ---- The looks, on this thread: the texture cache is the window's.
        let cap = RenderMaterial {
            color: Some(settings.cap_color),
            ..RenderMaterial::default()
        };
        let mut textures = Vec::new();
        let mut report = SectionReport::default();
        let mut materials = Vec::new();
        let mut pieces = Vec::new();
        for (unit, result) in units.iter().zip(results) {
            report.solids += unit.body.solids.len() * unit.copies.len();
            for piece in result? {
                if piece.cap_area > 0.0 {
                    report.cut += 1;
                }
                report.area += piece.cap_area;
                for face in &piece.faces {
                    let look = match face {
                        PieceFace::Cap if settings.color_caps => {
                            materials.push(cap.clone());
                            continue;
                        }
                        PieceFace::Cap => piece.own.clone(),
                        PieceFace::Of(index) => piece.own.clone().or_else(|| {
                            index.and_then(|i| unit.part.face_appearance(unit.body, i).cloned())
                        }),
                    };
                    materials.push(match look {
                        Some(a) => material(&a, &mut self.texture_cache, &mut textures),
                        None => RenderMaterial::default(),
                    });
                }
                pieces.push(piece);
            }
        }
        let (mesh, _) =
            oxyde_assembly::scene::combine(pieces.iter().map(|p| ((), &p.placement, &p.mesh)));
        Ok((mesh, materials, textures, report))
    }

    /// The Section View button of the Evaluate tab, in a part and in an assembly.
    pub(crate) fn section_view_button(&mut self, ui: &mut egui::Ui, enabled: bool) {
        if icons::command_button(
            ui,
            Icon::SectionView,
            "Section\nView",
            enabled,
            self.section.is_some(),
        )
        .on_hover_text("Cut the model open along a plane to look inside it")
        .clicked()
        {
            self.toggle_section_view();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_section_plane_moves_along_its_normal_and_flips() {
        let mut s = SectionSettings {
            plane: SectionBase::Standard(StandardPlane::Top),
            offset: 12.0,
            ..SectionSettings::default()
        };
        let p = s.section_plane();
        assert_eq!(p.normal, Vec3::Y);
        assert!((p.origin.y - 12.0).abs() < 1e-12);
        s.flip = true;
        let q = s.section_plane();
        assert_eq!(q.normal, Vec3::Y * -1.0);
        assert_eq!(q.origin, p.origin);
        // Still a right-handed frame.
        let z = q.x_dir.cross(q.y_dir());
        assert!((z - q.normal).length() < 1e-12);
    }

    #[test]
    fn the_half_space_frame_is_the_planes_frame() {
        let plane = SectionSettings {
            plane: SectionBase::Standard(StandardPlane::Right),
            offset: 5.0,
            ..SectionSettings::default()
        }
        .section_plane();
        let t = Transform::from_plane(&plane);
        // The frame's z goes along the normal, from the plane's origin.
        let on_axis = t.apply_point(Point3::new(0.0, 0.0, 3.0));
        assert!((on_axis - (plane.origin + plane.normal * 3.0)).length() < 1e-12);
        let (lo, hi) = (Point3::new(-10.0, 0.0, 0.0), Point3::new(30.0, 20.0, 10.0));
        let (a, b) = SectionView::offsets_through(&StandardPlane::Right.plane(), lo, hi);
        assert_eq!((a, b), (-10.0, 30.0));
        assert!(reach(&plane, (lo, hi)) > 40.0);
    }

    #[test]
    fn a_solid_is_cut_only_where_the_plane_goes_through_its_box() {
        // The Top plane raised to y = 10, its normal +Y: what is above is removed.
        let plane = StandardPlane::Top.plane().offset(10.0);
        let below = (Point3::new(0.0, 0.0, 0.0), Point3::new(5.0, 10.0, 5.0));
        let above = (Point3::new(-3.0, 10.0, -3.0), Point3::new(5.0, 30.0, 5.0));
        let through = (Point3::new(0.0, 5.0, 0.0), Point3::new(5.0, 15.0, 5.0));
        assert_eq!(side_of(&plane, below), Side::Kept);
        assert_eq!(side_of(&plane, above), Side::Removed);
        assert_eq!(side_of(&plane, through), Side::Through);
        // Flipped, the halves swap.
        let flipped = Plane {
            origin: plane.origin,
            x_dir: plane.y_dir(),
            normal: plane.normal * -1.0,
        };
        assert_eq!(side_of(&flipped, below), Side::Removed);
        assert_eq!(side_of(&flipped, above), Side::Kept);
        assert_eq!(side_of(&flipped, through), Side::Through);
        // A plane at an angle sees the corners, not the axes.
        let slanted =
            Plane::from_point_normal(Point3::new(5.0, 10.0, 0.0), Vec3::new(1.0, 1.0, 0.0))
                .unwrap();
        assert_eq!(side_of(&slanted, below), Side::Through);
        assert_eq!(corners(below.0, below.1).len(), 8);
    }

    #[test]
    fn a_face_is_cut_along_at_offset_zero_and_the_range_is_measured_from_it() {
        // The top face of a 20 mm tall block: its outward normal is +Y, the plane sits at
        // y = 20, so offset 0 cuts along the face and the block lies at offsets -20..0.
        let face = SectionBase::Face {
            plane: Plane::from_point_normal(Point3::new(5.0, 20.0, 5.0), Vec3::Y).unwrap(),
            label: "Selected face".to_owned(),
        };
        let s = SectionSettings {
            plane: face.clone(),
            offset: 0.0,
            ..SectionSettings::default()
        };
        let p = s.section_plane();
        assert_eq!(p.normal, Vec3::Y);
        assert!((p.origin.y - 20.0).abs() < 1e-12);
        let (lo, hi) = (Point3::new(0.0, 0.0, 0.0), Point3::new(10.0, 20.0, 10.0));
        let (a, b) = SectionView::offsets_through(&face.plane(), lo, hi);
        assert!((a + 20.0).abs() < 1e-12 && b.abs() < 1e-12, "{a}..{b}");
        // Moved 5 mm into the block, the plane is at y = 15.
        let inside = SectionSettings { offset: -5.0, ..s }.section_plane();
        assert!((inside.origin.y - 15.0).abs() < 1e-12);
        assert_eq!(face.name(), "Selected face");
    }
}
