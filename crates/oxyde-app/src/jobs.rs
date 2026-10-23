//! Long work off the UI thread: opening a part or an assembly (its part files rebuilt and
//! meshed), reading a STEP file, reloading an assembly's parts. The window keeps
//! drawing, with a progress window over everything that says what is being done, how far
//! it got and how long it has taken, instead of looking frozen.
//!
//! The work runs on a thread of its own with a kernel of its own
//! ([`Kernel::open_worker`]), whose shapes the main kernel can use once the result is
//! handed back. What the job works on (a document being opened, an assembly being
//! reloaded) belongs to the job until then: the main thread never touches it meanwhile.
//! A kernel that cannot open workers does the work on the UI thread instead, so nothing
//! depends on the thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui;
use oxyde_format::step_import::ImportPlan;
use oxyde_kernel::Kernel;
use oxyde_model::{FileData, Import, Part, PartDefinition};
use oxyde_occt::OcctKernel;

use crate::app::OxydeApp;
use crate::assembly::AssemblyDoc;
use crate::document::Document;
use crate::edit_in_place::Suspended;

/// How far a job got, written by the job and read by the window.
#[derive(Default)]
pub struct Progress {
    /// What is being done right now ("Reading Bracket.oxyprt").
    message: Mutex<String>,
    done: AtomicUsize,
    /// 0 while the amount of work is unknown.
    total: AtomicUsize,
}

impl Progress {
    pub fn say(&self, message: impl Into<String>) {
        if let Ok(mut m) = self.message.lock() {
            *m = message.into();
        }
    }

    pub fn start(&self, total: usize) {
        self.total.store(total, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
    }

    pub fn step(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    /// The message, and the fraction done when the amount of work is known.
    fn snapshot(&self) -> (String, Option<f32>) {
        let message = self.message.lock().map(|m| m.clone()).unwrap_or_default();
        let total = self.total.load(Ordering::Relaxed);
        let fraction = (total > 0)
            .then(|| (self.done.load(Ordering::Relaxed) as f32 / total as f32).clamp(0.0, 1.0));
        (message, fraction)
    }
}

/// What a job gives back.
pub enum Outcome {
    /// An assembly opened: its parts loaded and its mates solved.
    AssemblyOpened {
        doc: AssemblyDoc,
        notes: Vec<String>,
        read_only: bool,
    },
    /// An assembly set aside while a component was edited, read again.
    AssemblyBack {
        suspended: Suspended,
        notes: Vec<String>,
    },
    /// An assembly's parts read again (Reload Parts).
    AssemblyReloaded {
        doc: AssemblyDoc,
        notes: Vec<String>,
    },
    /// A part opened, rebuilt.
    PartOpened {
        doc: Result<Document, String>,
        path: PathBuf,
        read_only: bool,
    },
    /// A part rebuilt with an Imported feature added (a new part from a STEP file, or
    /// Import STEP into the open part).
    PartImported {
        part: Part,
        file_name: String,
        /// A new part named after the file: no undo step, nothing unsaved.
        fresh: Option<String>,
    },
    /// A part made from a generative outcome and saved to `path`: what was done (or why it
    /// could not be made), and the surfaces it was made of, to check it on.
    PartCreated {
        path: PathBuf,
        result: Result<String, String>,
        pieces: Vec<oxyde_sim::Shape>,
    },
    /// A STEP assembly read, and the files it would become.
    StepPlanned {
        plan: Result<ImportPlan, String>,
        path: PathBuf,
        source: String,
    },
}

pub struct Job {
    pub title: String,
    progress: Arc<Progress>,
    receiver: Receiver<Outcome>,
    started: Instant,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Job {
    /// Runs `work` on a thread with a worker kernel, or at once on this thread when the
    /// kernel cannot open workers.
    fn spawn<F>(title: &str, kernel: &mut OcctKernel, work: F) -> Self
    where
        F: FnOnce(&mut OcctKernel, &Progress) -> Outcome + Send + 'static,
    {
        let progress = Arc::new(Progress::default());
        let (sender, receiver) = mpsc::channel();
        let worker_progress = progress.clone();
        let thread = match OcctKernel::open_worker() {
            Some(_) => Some(std::thread::spawn(move || {
                let Some(mut worker) = OcctKernel::open_worker() else {
                    return;
                };
                let outcome = work(&mut worker, &worker_progress);
                // The window may be gone: nothing to tell then.
                let _ = sender.send(outcome);
            })),
            None => {
                let outcome = work(kernel, &progress);
                let _ = sender.send(outcome);
                None
            }
        };
        Job {
            title: title.to_owned(),
            progress,
            receiver,
            started: Instant::now(),
            thread,
        }
    }

    /// Waits for the job to end: when the window closes while it runs, so that the
    /// kernel is not torn down under it.
    pub fn join(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Opens, rebuilds and meshes a part file.
fn open_part(
    path: &Path,
    kernel: &mut OcctKernel,
    progress: &Progress,
) -> Result<Document, String> {
    progress.say(format!("Reading {}", name_of(path)));
    let file = oxyde_format::load_part(path).map_err(|e| e.to_string())?;
    progress.say(format!("Rebuilding {}", name_of(path)));
    let mut doc = Document::from_file(file, Some(path.to_path_buf()))?;
    doc.rebuild(kernel)
        .map_err(|e| format!("internal error while rebuilding: {}", e.message))?;
    Ok(doc)
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl OxydeApp {
    /// Whether a job is running: the document must not change meanwhile.
    pub(crate) fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub(crate) fn start_job<F>(&mut self, title: &str, work: F)
    where
        F: FnOnce(&mut OcctKernel, &Progress) -> Outcome + Send + 'static,
    {
        if let Some(job) = self.job.take() {
            job.join();
        }
        self.job = Some(Job::spawn(title, &mut self.kernel, work));
        self.status = format!("{title}…");
    }

    // ---- Starting jobs ---------------------------------------------------------------------

    /// Opens a part file: read and rebuilt off the UI thread, then shown.
    pub(crate) fn open_part_job(&mut self, path: &Path, read_only: bool) {
        let path = path.to_path_buf();
        self.start_job(
            &format!("Opening {}", name_of(&path)),
            move |kernel, progress| {
                let doc = open_part(&path, kernel, progress);
                Outcome::PartOpened {
                    doc,
                    path,
                    read_only,
                }
            },
        );
    }

    /// Loads an assembly's parts and solves its mates off the UI thread, then enters it.
    pub(crate) fn open_assembly_job(&mut self, mut doc: AssemblyDoc, read_only: bool) {
        doc.mesh_fraction = self.mesh_fraction();
        let name = doc.display_name();
        self.start_job(&format!("Opening {name}"), move |kernel, progress| {
            let notes = doc.load_parts_with(kernel, Some(progress));
            progress.say("Solving the mates");
            doc.note_stale_parts();
            doc.apply_equations(kernel);
            Outcome::AssemblyOpened {
                doc,
                notes,
                read_only,
            }
        });
    }

    /// Reads again the files of an assembly set aside for editing in place that changed,
    /// then comes back to it.
    pub(crate) fn assembly_back_job(&mut self, mut suspended: Suspended) {
        let name = suspended.asm.display_name();
        self.start_job(&format!("Back to {name}"), move |kernel, progress| {
            let notes = suspended.asm.reload_changed_with(kernel, Some(progress));
            progress.say("Solving the mates");
            suspended.asm.refresh_linked_equations();
            suspended.asm.note_stale_parts();
            suspended.asm.apply_equations(kernel);
            suspended.asm.revision = crate::document::next_revision();
            Outcome::AssemblyBack { suspended, notes }
        });
    }

    /// Reads every part file of the open assembly again (Reload Parts).
    pub(crate) fn reload_assembly_job(&mut self) {
        let fraction = self.mesh_fraction();
        let Some(current) = self.asm.as_mut() else {
            return;
        };
        // The assembly's data is the job's meanwhile; a stand-in with the same
        // components keeps the tree in place, and the viewport keeps its picture
        // (`refresh_assembly_scene` waits while a job runs).
        let mut stand_in = AssemblyDoc::from_file(
            oxyde_format::AssemblyFile {
                document_id: current.document_id.clone(),
                assembly: current.assembly.clone(),
                properties: current.properties.clone(),
            },
            current.path.clone(),
        );
        stand_in.read_only = current.read_only;
        stand_in.modified = current.modified;
        stand_in.expanded = current.expanded.clone();
        let mut doc = std::mem::replace(current, stand_in);
        doc.mesh_fraction = fraction;
        self.leave_assembly_panels();
        let name = doc.display_name();
        self.start_job(
            &format!("Reloading the parts of {name}"),
            move |kernel, progress| {
                let notes = doc.reload_parts_with(kernel, Some(progress));
                progress.say("Solving the mates");
                doc.refresh_linked_equations();
                doc.note_stale_parts();
                doc.apply_equations(kernel);
                Outcome::AssemblyReloaded { doc, notes }
            },
        );
    }

    /// Reads a STEP file into an Imported feature: into a copy of `definition`, rebuilt
    /// off the UI thread; `fresh` names a new part made from the file.
    pub(crate) fn import_step_job(
        &mut self,
        path: &Path,
        definition: PartDefinition,
        fresh: Option<String>,
    ) {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.status = format!("Could not read {}: {e}", path.display());
                return;
            }
        };
        let file_name = name_of(path);
        self.start_job(
            &format!("Importing {file_name}"),
            move |kernel, progress| {
                progress.say(format!("Reading {file_name}"));
                let mut part = Part::from_definition(definition).unwrap_or_else(|e| {
                    // Cannot happen for the definition of a live part; reported anyway.
                    progress.say(e.to_string());
                    Part::new()
                });
                part.add_feature(oxyde_model::FeatureKind::Import(Import::new(
                    file_name.clone(),
                    FileData::new(bytes),
                )));
                part.rebuild(kernel);
                Outcome::PartImported {
                    part,
                    file_name,
                    fresh,
                }
            },
        );
    }

    /// Reads the product structure of a STEP file off the UI thread; the folder is
    /// asked for once it is known what the file holds.
    pub(crate) fn plan_step_job(&mut self, path: &Path) {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.status = format!("Could not read {}: {e}", path.display());
                return;
            }
        };
        let source = name_of(path);
        let path = path.to_path_buf();
        self.start_job(&format!("Reading {source}"), move |kernel, progress| {
            progress.say(format!("Reading the parts and assemblies of {source}"));
            let plan = match kernel.read_step_assembly(&bytes) {
                Ok(tree) => oxyde_format::step_import::plan_step_import(&tree, &source)
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            Outcome::StepPlanned { plan, path, source }
        });
    }

    // ---- The window, and finishing ---------------------------------------------------------

    /// The progress window over everything while a job runs; takes the result when it is
    /// in. Everything underneath is shaded and takes no clicks.
    pub(crate) fn job_window(&mut self, ctx: &egui::Context) {
        let Some(job) = self.job.as_ref() else {
            return;
        };
        let outcome = match job.receiver.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                // The thread died without an answer.
                self.status = format!("{} failed", job.title);
                self.job = None;
                return;
            }
        };
        if let Some(outcome) = outcome {
            if let Some(job) = self.job.take() {
                job.join();
            }
            self.finish_job(outcome, ctx);
            return;
        }
        let (message, fraction) = job.progress.snapshot();
        let elapsed = job.started.elapsed().as_secs_f64();
        if std::env::var_os("OXYDE_JOB_TRACE").is_some() {
            eprintln!("job frame: {elapsed:.2} s, {message:?}, {fraction:?}");
        }
        let title = job.title.clone();
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new("oxyde job shade"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .interactable(true)
            .show(ctx, |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(90));
            });
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if fraction.is_none() {
                        ui.add(egui::Spinner::new().size(16.0));
                    }
                    ui.label(if message.is_empty() {
                        "Working".to_owned()
                    } else {
                        message
                    });
                });
                ui.add_space(6.0);
                match fraction {
                    Some(f) => {
                        ui.add(egui::ProgressBar::new(f).show_percentage());
                    }
                    None => {
                        ui.add(egui::ProgressBar::new(0.0).animate(true));
                    }
                }
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!("{elapsed:.0} s"))
                        .small()
                        .weak(),
                );
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn finish_job(&mut self, outcome: Outcome, ctx: &egui::Context) {
        match outcome {
            Outcome::PartOpened {
                doc,
                path,
                read_only,
            } => match doc {
                Ok(mut doc) => {
                    doc.read_only = read_only;
                    self.replace_document_built(doc);
                    self.remember_recent(&path);
                    self.status = if read_only {
                        format!("Opened {} read-only", path.display())
                    } else {
                        format!("Opened {}", path.display())
                    };
                    self.report_errors();
                }
                Err(e) => {
                    self.status = format!("Could not open {}: {e}", path.display());
                    // Opening a component in place that failed: back to the assembly.
                    if !self.edit_stack.is_empty() && self.asm.is_none() {
                        self.finish_edit(ctx);
                    }
                }
            },
            Outcome::AssemblyOpened {
                doc,
                notes,
                read_only,
            } => {
                let path = doc.path.clone();
                self.enter_assembly_loaded(doc, notes);
                if let Some(path) = path {
                    self.remember_recent(&path);
                }
                if read_only {
                    self.status = format!("{} (read-only)", self.status);
                }
            }
            Outcome::AssemblyBack { suspended, notes } => self.assembly_back(suspended, notes, ctx),
            Outcome::AssemblyReloaded { mut doc, notes } => {
                if let Some(stand_in) = self.asm.take() {
                    // Changes made to the stand-in meanwhile (none expected: the
                    // window takes no clicks while a job runs) would be lost.
                    doc.expanded = stand_in.expanded;
                }
                self.asm = Some(doc);
                self.asm_scene = None;
                self.status = notes
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| "Parts reloaded".to_owned());
            }
            Outcome::PartImported {
                part,
                file_name,
                fresh,
            } => self.part_imported(part, &file_name, fresh),
            Outcome::PartCreated {
                path,
                result,
                pieces,
            } => self.part_created(path, result, pieces),
            Outcome::StepPlanned { plan, path, source } => match plan {
                Ok(plan) => self.write_step_plan(&plan, &path, &source),
                Err(e) => self.status = format!("Could not import {source}: {e}"),
            },
        }
    }
}
