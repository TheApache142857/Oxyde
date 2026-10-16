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

use eframe::egui::{self, RichText};
use oxyde_assembly::{Placement, Primitive};
use oxyde_geom::{Plane, Point3, Transform, Vec3};
use oxyde_kernel::{BooleanOp, FaceSource, Kernel, Mesh, ShapeHandle, SurfaceKind};
use oxyde_model::{Appearance, Body, Part, StandardPlane};
use oxyde_render::{Material as RenderMaterial, Scene, TextureRef};

use crate::app::{OxydeApp, Selected, deflection};
use crate::appearance_ui::{TextureCache, material};
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
            report: None,
            error: None,
        }
    }

    /// Where the box `lo..hi` lies along the plane's normal, as offsets from the plane:
    /// the offsets that go through it.
    fn offsets_through(plane: &Plane, lo: Point3, hi: Point3) -> (f64, f64) {
        let n = plane.normal;
        let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
        for x in [lo.x, hi.x] {
            for y in [lo.y, hi.y] {
                for z in [lo.z, hi.z] {
                    let d = (Point3::new(x, y, z) - plane.origin).dot(n);
                    min = min.min(d);
                    max = max.max(d);
                }
            }
        }
        (min, max)
    }
}

/// One solid cut open: its mesh, in the solid's own coordinates, with a material per face.
struct CutSolid {
    mesh: Mesh,
    materials: Vec<RenderMaterial>,
    /// Area of its cut faces, mm²; 0 when the plane misses it.
    cap_area: f64,
}

/// The matrix that takes the plane's own frame (x along `x_dir`, z along the normal) to
/// the world.
fn plane_transform(p: &Plane) -> Transform {
    let y = p.y_dir();
    Transform {
        rows: [
            [p.x_dir.x, y.x, p.normal.x, p.origin.x],
            [p.x_dir.y, y.y, p.normal.y, p.origin.y],
            [p.x_dir.z, y.z, p.normal.z, p.origin.z],
        ],
    }
}

/// The half-space on the side `plane`'s normal points to, as a box big enough to hold
/// everything within `reach` mm of the plane's origin.
fn half_space<K: Kernel>(kernel: &mut K, plane: &Plane, reach: f64) -> Result<ShapeHandle, String> {
    let l = reach.max(1.0);
    let raw = kernel
        .make_box(Point3::new(-l, -l, 0.0), Vec3::new(2.0 * l, 2.0 * l, l))
        .map_err(|e| e.to_string())?;
    let placed = kernel.transform(raw, &plane_transform(plane));
    kernel.release(raw);
    Ok(placed.map_err(|e| e.to_string())?.shape)
}

/// Cuts every solid of `body` by `tool`, meshing what is left. `appearance(face)` is the
/// look of a face of the body (by its index in the body's topology), `own` what the
/// whole part is drawn with when it has one (a component appearance in an assembly).
fn cut_body<K: Kernel>(
    kernel: &mut K,
    body: &Body,
    tool: ShapeHandle,
    fraction: f64,
    settings: &SectionSettings,
    own: Option<&Appearance>,
    appearance: &dyn Fn(u32) -> Option<Appearance>,
    cache: &mut TextureCache,
    textures: &mut Vec<TextureRef>,
) -> Result<Vec<CutSolid>, String> {
    let cap = RenderMaterial {
        color: Some(settings.cap_color),
        ..RenderMaterial::default()
    };
    let mut out = Vec::new();
    for solid in &body.solids {
        let result = kernel
            .boolean(BooleanOp::Cut, solid.shape, tool)
            .map_err(|e| e.to_string())?;
        let topology = match kernel.topology(result.shape) {
            Ok(t) => t,
            Err(e) => {
                kernel.release(result.shape);
                return Err(e.to_string());
            }
        };
        if topology.faces.is_empty() {
            // The whole solid was on the removed side.
            kernel.release(result.shape);
            continue;
        }
        let mesh = kernel.tessellate(result.shape, deflection(&topology, fraction));
        kernel.release(result.shape);
        let mesh = mesh.map_err(|e| e.to_string())?;
        let mut materials = Vec::with_capacity(topology.faces.len());
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
            let from_solid = sources.iter().find_map(|s| match s {
                FaceSource::InputFace { input: 0, face } => Some(*face),
                _ => None,
            });
            if from_tool {
                cap_area += face.area;
            }
            let m = if from_tool && settings.color_caps {
                cap.clone()
            } else {
                let look = own
                    .cloned()
                    .or_else(|| from_solid.and_then(|local| appearance(solid.faces.start + local)));
                match look {
                    Some(a) => material(&a, cache, textures),
                    None => RenderMaterial::default(),
                }
            };
            materials.push(m);
        }
        out.push(CutSolid {
            mesh,
            materials,
            cap_area,
        });
    }
    Ok(out)
}

/// The box around every solid of a part, in its own coordinates.
fn part_bounds<K: Kernel>(kernel: &K, body: &Body) -> Option<(Point3, Point3)> {
    let mut out: Option<(Point3, Point3)> = None;
    for solid in &body.solids {
        let Ok((a, b)) = kernel.bounds(solid.shape) else {
            continue;
        };
        out = Some(match out {
            None => (a, b),
            Some((lo, hi)) => (
                Point3::new(lo.x.min(a.x), lo.y.min(a.y), lo.z.min(a.z)),
                Point3::new(hi.x.max(b.x), hi.y.max(b.y), hi.z.max(b.z)),
            ),
        });
    }
    out
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
        if let Some((lo, hi)) = self.model_bounds() {
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

    /// The box around the open document: the part's solids, or every part of the assembly
    /// where it sits.
    fn model_bounds(&self) -> Option<(Point3, Point3)> {
        match &self.asm {
            Some(doc) => doc.bounds(&doc.assembly),
            None => part_bounds(&self.kernel, self.doc.part.body()?),
        }
    }

    fn section_revision(&self) -> u64 {
        match &self.asm {
            Some(asm) => asm.revision,
            None => self.doc.revision,
        }
    }

    /// The Section View window, and the section in the viewport while it is open.
    pub(crate) fn section_window(&mut self, ctx: &egui::Context) {
        if self.section.is_none() {
            return;
        }
        // The STL preview owns the viewport while its window is open.
        if self.stl.is_some() {
            self.close_section_view();
            return;
        }
        let mut open = true;
        let mut look = false;
        let mut centre = false;
        let bounds = self.model_bounds();
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
        if centre {
            if let Some((lo, hi)) = view.range {
                view.settings.offset = (lo + hi) / 2.0;
            }
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
        let result = match self.model_bounds() {
            None => Err("there is no solid".to_owned()),
            Some(bounds) => {
                let reach = reach(&plane, bounds);
                match half_space(&mut self.kernel, &plane, reach) {
                    Err(e) => Err(e),
                    Ok(tool) => {
                        let outcome = self.cut_document(tool, &settings, fraction);
                        self.kernel.release(tool);
                        outcome
                    }
                }
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

    /// Every solid of the document cut by `tool` (in world coordinates), as one mesh with
    /// a material per face, and the report.
    #[allow(clippy::type_complexity)]
    fn cut_document(
        &mut self,
        tool: ShapeHandle,
        settings: &SectionSettings,
        fraction: f64,
    ) -> Result<(Mesh, Vec<RenderMaterial>, Vec<TextureRef>, SectionReport), String> {
        let mut textures = Vec::new();
        let mut report = SectionReport::default();
        let Some(doc) = self.asm.as_ref() else {
            // ---- A part: its solids, in place.
            let part: &Part = &self.doc.part;
            let Some(body) = part.body() else {
                return Err("there is no solid".to_owned());
            };
            report.solids = body.solids.len();
            let appearance = |face: u32| part.face_appearance(body, face).cloned();
            let cuts = cut_body(
                &mut self.kernel,
                body,
                tool,
                fraction,
                settings,
                None,
                &appearance,
                &mut self.texture_cache,
                &mut textures,
            )?;
            let mut mesh = Mesh::default();
            let mut materials = Vec::new();
            for c in cuts {
                if c.cap_area > 0.0 {
                    report.cut += 1;
                }
                report.area += c.cap_area;
                mesh.faces.extend(c.mesh.faces);
                mesh.edges.extend(c.mesh.edges);
                materials.extend(c.materials);
            }
            return Ok((mesh, materials, textures, report));
        };
        // ---- An assembly: every part shown, cut where it sits. The tool is brought into
        // each part's own coordinates rather than the part into the world.
        let assembly = doc.assembly.clone();
        let leaves = doc.leaves(&assembly);
        let mut placed: Vec<(Placement, Mesh)> = Vec::new();
        let mut materials = Vec::new();
        for (occurrence, placement, part) in leaves {
            let Some(body) = part.part.body() else {
                continue;
            };
            report.solids += body.solids.len();
            let own = doc.occurrence_appearance(&assembly, &occurrence).cloned();
            let local_tool = self
                .kernel
                .transform(tool, &placement.inverse().transform())
                .map_err(|e| e.to_string())?
                .shape;
            let appearance = |face: u32| part.part.face_appearance(body, face).cloned();
            let cuts = cut_body(
                &mut self.kernel,
                body,
                local_tool,
                fraction,
                settings,
                own.as_ref(),
                &appearance,
                &mut self.texture_cache,
                &mut textures,
            );
            self.kernel.release(local_tool);
            for c in cuts? {
                if c.cap_area > 0.0 {
                    report.cut += 1;
                }
                report.area += c.cap_area;
                materials.extend(c.materials);
                placed.push((placement, c.mesh));
            }
        }
        let (mesh, _) = oxyde_assembly::scene::combine(placed.iter().map(|(p, m)| ((), p, m)));
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
        let t = plane_transform(&plane);
        // The frame's z goes along the normal, from the plane's origin.
        let on_axis = t.apply_point(Point3::new(0.0, 0.0, 3.0));
        assert!((on_axis - (plane.origin + plane.normal * 3.0)).length() < 1e-12);
        let (lo, hi) = (Point3::new(-10.0, 0.0, 0.0), Point3::new(30.0, 20.0, 10.0));
        let (a, b) = SectionView::offsets_through(&StandardPlane::Right.plane(), lo, hi);
        assert_eq!((a, b), (-10.0, 30.0));
        assert!(reach(&plane, (lo, hi)) > 40.0);
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
