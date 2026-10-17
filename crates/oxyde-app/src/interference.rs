//! Evaluate › Interference Detection, in an assembly: which parts run into each other, and
//! by how much. As SolidWorks' command, every pair of visible parts whose boxes overlap is
//! checked with an exact boolean (what the two solids have in common); a pair sharing a
//! volume is listed with that volume, the shared volume is drawn in red over the model, and
//! clicking a pair selects its two components. Parts that merely touch along a face are
//! not interferences.
//!
//! The pairs are checked on every core, each worker with a kernel of its own
//! ([`Kernel::open_worker`]), the solids moved to their places for the check and released
//! after it. The result holds for one revision of the assembly: moving a part makes the
//! window ask to calculate again.

use std::sync::atomic::{AtomicUsize, Ordering};

use eframe::egui::{self, RichText};
use oxyde_assembly::Placement;
use oxyde_geom::Point3;
use oxyde_kernel::{BooleanOp, Kernel, Mesh, ShapeHandle};
use oxyde_render::Material as RenderMaterial;

use crate::app::OxydeApp;
use crate::assembly::{AsmSelected, AssemblyDoc, Occurrence};
use crate::icons::{self, Icon};

/// Shared volumes below this are noise of the boolean, not interferences (mm³).
const MIN_VOLUME: f64 = 1e-6;

/// What checking a pair gives: the shared volume and its mesh, nothing, or why not.
type PairResult = Result<Option<(f64, Mesh)>, String>;

/// Two parts sharing a volume.
pub struct Interference {
    pub a: Occurrence,
    pub b: Occurrence,
    pub a_name: String,
    pub b_name: String,
    /// The shared volume, mm³.
    pub volume: f64,
    /// The shared volume meshed, in the assembly's coordinates.
    pub mesh: Mesh,
}

pub struct InterferenceWindow {
    /// The interferences found, or `None` before the first calculation.
    pub results: Option<Vec<Interference>>,
    /// The row clicked, whose pair is selected.
    pub selected: Option<usize>,
    /// Assembly revision the results hold for.
    pub revision: Option<u64>,
    /// Pairs whose boxes overlapped and were checked exactly.
    pub checked: usize,
    /// Pairs that could not be checked, with the reason.
    pub failures: Vec<String>,
    /// Seconds the last calculation took.
    pub took: Option<f64>,
}

/// A pair of placed solids to check.
struct PairJob {
    a: usize,
    b: usize,
    a_shapes: Vec<(ShapeHandle, Placement)>,
    b_shapes: Vec<(ShapeHandle, Placement)>,
}

/// The volume two placed solids share, meshed, or `None` when they only touch or miss.
fn common_volume<K: Kernel>(
    kernel: &mut K,
    (a, place_a): (ShapeHandle, &Placement),
    (b, place_b): (ShapeHandle, &Placement),
    deflection: f64,
) -> PairResult {
    let moved_a = kernel
        .transform(a, &place_a.transform())
        .map_err(|e| e.to_string())?;
    let moved_b = match kernel.transform(b, &place_b.transform()) {
        Ok(m) => m,
        Err(e) => {
            kernel.release(moved_a.shape);
            return Err(e.to_string());
        }
    };
    let common = kernel.boolean(BooleanOp::Common, moved_a.shape, moved_b.shape);
    kernel.release(moved_a.shape);
    kernel.release(moved_b.shape);
    let common = common.map_err(|e| e.to_string())?;
    let out = (|| -> PairResult {
        let volume = kernel.volume(common.shape).map_err(|e| e.to_string())?;
        if volume <= MIN_VOLUME {
            return Ok(None);
        }
        let mesh = kernel
            .tessellate(common.shape, deflection)
            .map_err(|e| e.to_string())?;
        Ok(Some((volume, mesh)))
    })();
    kernel.release(common.shape);
    out
}

/// Checks one pair: every solid of one part against every solid of the other.
fn check_pair<K: Kernel>(kernel: &mut K, job: &PairJob, deflection: f64) -> PairResult {
    let mut total = 0.0;
    let mut mesh = Mesh::default();
    for (a, place_a) in &job.a_shapes {
        for (b, place_b) in &job.b_shapes {
            if let Some((volume, m)) =
                common_volume(kernel, (*a, place_a), (*b, place_b), deflection)?
            {
                total += volume;
                mesh.faces.extend(m.faces);
                mesh.edges.extend(m.edges);
            }
        }
    }
    Ok((total > MIN_VOLUME).then_some((total, mesh)))
}

/// Checks every pair on every core; the results are in pair order.
fn check_pairs<K: Kernel>(kernel: &mut K, jobs: &[PairJob], deflection: f64) -> Vec<PairResult> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(jobs.len().max(1));
    let next = AtomicUsize::new(0);
    let work = |kernel: &mut K| {
        let mut done = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(job) = jobs.get(i) else {
                break;
            };
            done.push((i, check_pair(kernel, job, deflection)));
        }
        done
    };
    let mut results: Vec<Option<PairResult>> = jobs.iter().map(|_| None).collect();
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
            // A worker that panicked leaves its pairs unchecked; they are reported below.
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
        .map(|r| r.unwrap_or_else(|| Err("the pair could not be checked".to_owned())))
        .collect()
}

/// Whether two boxes overlap (or touch).
fn boxes_meet((lo_a, hi_a): (Point3, Point3), (lo_b, hi_b): (Point3, Point3)) -> bool {
    const SLACK: f64 = 1e-6;
    lo_a.x <= hi_b.x + SLACK
        && lo_b.x <= hi_a.x + SLACK
        && lo_a.y <= hi_b.y + SLACK
        && lo_b.y <= hi_a.y + SLACK
        && lo_a.z <= hi_b.z + SLACK
        && lo_b.z <= hi_a.z + SLACK
}

/// Finds every interference of the assembly's visible parts.
fn find_interferences<K: Kernel>(
    kernel: &mut K,
    doc: &AssemblyDoc,
) -> (Vec<Interference>, usize, Vec<String>) {
    let leaves = doc.leaves(&doc.assembly);
    // Each part's box in the assembly, for the cheap test first.
    let boxes: Vec<Option<(Point3, Point3)>> = leaves
        .iter()
        .map(|(_, placement, part)| {
            part.bounds
                .and_then(|b| crate::assembly::placed_bounds(placement, b))
        })
        .collect();
    let size = doc
        .bounds(&doc.assembly)
        .map_or(100.0, |(lo, hi)| (hi - lo).length());
    let deflection = (size * doc.mesh_fraction).clamp(0.0005, 5.0);
    let mut jobs = Vec::new();
    for i in 0..leaves.len() {
        for j in (i + 1)..leaves.len() {
            let (Some(box_a), Some(box_b)) = (
                boxes.get(i).copied().flatten(),
                boxes.get(j).copied().flatten(),
            ) else {
                continue;
            };
            if !boxes_meet(box_a, box_b) {
                continue;
            }
            let (Some(a), Some(b)) = (leaves.get(i), leaves.get(j)) else {
                continue;
            };
            let shapes = |leaf: &(Occurrence, Placement, &crate::assembly::LoadedPart)| {
                leaf.2
                    .part
                    .body()
                    .map(|body| {
                        body.solids
                            .iter()
                            .map(|s| (s.shape, leaf.1))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            let (a_shapes, b_shapes) = (shapes(a), shapes(b));
            if a_shapes.is_empty() || b_shapes.is_empty() {
                continue;
            }
            jobs.push(PairJob {
                a: i,
                b: j,
                a_shapes,
                b_shapes,
            });
        }
    }
    let checked = jobs.len();
    let mut found = Vec::new();
    let mut failures = Vec::new();
    for (job, result) in jobs.iter().zip(check_pairs(kernel, &jobs, deflection)) {
        let (Some(a), Some(b)) = (leaves.get(job.a), leaves.get(job.b)) else {
            continue;
        };
        let (a_name, b_name) = (doc.occurrence_name(&a.0), doc.occurrence_name(&b.0));
        match result {
            Ok(Some((volume, mesh))) => found.push(Interference {
                a: a.0.clone(),
                b: b.0.clone(),
                a_name,
                b_name,
                volume,
                mesh,
            }),
            Ok(None) => {}
            Err(e) => failures.push(format!("{a_name} and {b_name}: {e}")),
        }
    }
    found.sort_by(|x, y| y.volume.total_cmp(&x.volume));
    (found, checked, failures)
}

impl OxydeApp {
    /// The Evaluate tab's Interference Detection button.
    pub(crate) fn interference_button(&mut self, ui: &mut egui::Ui, enabled: bool) {
        if icons::command_button(
            ui,
            Icon::Interference,
            "Interference\nDetection",
            enabled,
            self.interference.is_some(),
        )
        .on_hover_text("Find the parts that run into each other, and the volume they share")
        .clicked()
        {
            if self.interference.is_some() {
                self.close_interference();
            } else {
                self.interference = Some(InterferenceWindow {
                    results: None,
                    selected: None,
                    revision: None,
                    checked: 0,
                    failures: Vec::new(),
                    took: None,
                });
            }
        }
    }

    pub(crate) fn close_interference(&mut self) {
        if self.interference.take().is_some() {
            // The red volumes go with the window.
            self.asm_scene = None;
        }
    }

    /// Runs the check now, for the assembly as it is.
    fn calculate_interferences(&mut self) {
        let Some(doc) = self.asm.as_ref() else {
            return;
        };
        let start = std::time::Instant::now();
        let (results, checked, failures) = find_interferences(&mut self.kernel, doc);
        let revision = doc.revision;
        let count = results.len();
        if let Some(w) = self.interference.as_mut() {
            w.results = Some(results);
            w.selected = None;
            w.revision = Some(revision);
            w.checked = checked;
            w.failures = failures;
            w.took = Some(start.elapsed().as_secs_f64());
        }
        self.asm_scene = None;
        self.status = match count {
            0 => "No interference found".to_owned(),
            1 => "1 interference found".to_owned(),
            n => format!("{n} interferences found"),
        };
    }

    /// Appends the shared volumes to the scene, in red, after everything else; they
    /// cannot be picked. Returns whether anything was added.
    pub(crate) fn add_interferences(
        &self,
        mesh: &mut Mesh,
        materials: &mut Vec<RenderMaterial>,
    ) -> bool {
        let Some(results) = self
            .interference
            .as_ref()
            .and_then(|w| w.results.as_ref())
            .filter(|r| !r.is_empty())
        else {
            return false;
        };
        let selected = self.interference.as_ref().and_then(|w| w.selected);
        materials.resize(mesh.faces.len(), RenderMaterial::default());
        for (i, found) in results.iter().enumerate() {
            let color = if selected.is_none() || selected == Some(i) {
                [220, 40, 40]
            } else {
                [230, 120, 120]
            };
            let material = RenderMaterial {
                color: Some(color),
                opacity: 0.9,
                ..RenderMaterial::default()
            };
            mesh.faces.extend(found.mesh.faces.iter().cloned());
            materials.extend(std::iter::repeat_n(material, found.mesh.faces.len()));
        }
        true
    }

    /// The Interference Detection window.
    pub(crate) fn interference_window(&mut self, ctx: &egui::Context) {
        let Some(revision) = self.asm.as_ref().map(|d| d.revision) else {
            return;
        };
        if self.interference.is_none() {
            return;
        }
        let mut open = true;
        let mut calculate = false;
        let mut select: Option<usize> = None;
        egui::Window::new("Interference Detection")
            .open(&mut open)
            .resizable(false)
            .default_pos(ctx.screen_rect().right_top() + egui::vec2(-380.0, 220.0))
            .show(ctx, |ui| {
                let Some(w) = self.interference.as_ref() else {
                    return;
                };
                ui.set_width(340.0);
                let stale = w.revision != Some(revision);
                ui.horizontal(|ui| {
                    if ui
                        .button(if w.results.is_some() {
                            "Calculate again"
                        } else {
                            "Calculate"
                        })
                        .clicked()
                    {
                        calculate = true;
                    }
                    if stale && w.results.is_some() {
                        ui.label(
                            RichText::new("The assembly changed since")
                                .small()
                                .color(crate::theme::tokens().warn),
                        );
                    }
                });
                ui.add_space(4.0);
                match &w.results {
                    None => {
                        ui.label(
                            "Every pair of visible parts whose boxes overlap is checked exactly; \
                             the volumes they share are listed and drawn in red.",
                        );
                    }
                    Some(results) if results.is_empty() => {
                        ui.label(format!(
                            "No interference: {} pair(s) checked{}.",
                            w.checked,
                            w.took.map_or(String::new(), |t| format!(" in {t:.2} s"))
                        ));
                    }
                    Some(results) => {
                        crate::property_manager::section(ui, "Results");
                        let total: f64 = results.iter().map(|r| r.volume).sum();
                        egui::ScrollArea::vertical()
                            .max_height(240.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (i, r) in results.iter().enumerate() {
                                    let text = format!(
                                        "{}  ↔  {}:  {}",
                                        r.a_name,
                                        r.b_name,
                                        volume_text(r.volume)
                                    );
                                    if ui
                                        .selectable_label(w.selected == Some(i), text)
                                        .on_hover_text("Click to select the two components")
                                        .clicked()
                                    {
                                        select = Some(i);
                                    }
                                }
                            });
                        ui.add_space(4.0);
                        ui.label(format!(
                            "{} interference(s) in {} pair(s) checked{}; {} in all.",
                            results.len(),
                            w.checked,
                            w.took.map_or(String::new(), |t| format!(" in {t:.2} s")),
                            volume_text(total)
                        ));
                    }
                }
                for failure in &w.failures {
                    ui.colored_label(
                        crate::theme::tokens().err,
                        format!("Could not check {failure}"),
                    );
                }
                ui.label(
                    RichText::new(
                        "Hidden components are left out. Parts that only touch along a face \
                         are not interferences.",
                    )
                    .small()
                    .weak(),
                );
            });
        if calculate {
            self.calculate_interferences();
        }
        if let Some(i) = select {
            let pair = self
                .interference
                .as_ref()
                .and_then(|w| w.results.as_ref())
                .and_then(|r| r.get(i))
                .map(|r| (r.a.clone(), r.b.clone()));
            if let Some(w) = self.interference.as_mut() {
                w.selected = if w.selected == Some(i) { None } else { Some(i) };
            }
            if let (Some((a, b)), Some(doc)) = (pair, self.asm.as_mut()) {
                doc.selection = vec![AsmSelected::Component(a.top), AsmSelected::Component(b.top)];
                doc.selection.dedup();
            }
            // The chosen pair is drawn brighter than the rest.
            self.asm_scene = None;
        }
        if !open {
            self.close_interference();
        }
    }
}

fn volume_text(volume: f64) -> String {
    if volume >= 1000.0 {
        format!("{:.2} cm³", volume / 1000.0)
    } else {
        format!("{volume:.3} mm³")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boxes_meet_when_they_overlap_or_touch_and_not_when_apart() {
        let a = (Point3::new(0.0, 0.0, 0.0), Point3::new(10.0, 10.0, 10.0));
        let b = (Point3::new(5.0, 5.0, 5.0), Point3::new(20.0, 20.0, 20.0));
        let touching = (Point3::new(10.0, 0.0, 0.0), Point3::new(20.0, 10.0, 10.0));
        let apart = (Point3::new(11.0, 0.0, 0.0), Point3::new(20.0, 10.0, 10.0));
        assert!(boxes_meet(a, b));
        assert!(boxes_meet(a, touching));
        assert!(!boxes_meet(a, apart));
        assert_eq!(volume_text(12.3456), "12.346 mm³");
        assert_eq!(volume_text(1234.5), "1.23 cm³");
    }
}
