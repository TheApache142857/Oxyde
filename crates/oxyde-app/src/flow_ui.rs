//! The Simulation tab's flow and cooling studies, as SolidWorks Flow Simulation has them
//! (`docs/CFD.md`). The panel sets the fluid and the flow: internal (through the model's
//! cavity, closed by lids over its openings) or external (around the model, in a stream),
//! laminar or turbulent. Faces take inlets, static pressures (where the fluid goes out),
//! ideal walls and goals (forces, flow rates, mean pressures); faces in none are walls.
//!
//! A cooling study adds heat: the fluid's thermal properties and temperature, gravity for
//! natural convection, the bodies' thermal conductivities (from their materials), thermal
//! loads (internal heat, heat power or flux, held temperatures, convection outside the
//! fluid) and goals of temperature and heat. Its plots add temperatures, through the fluid
//! and the solids.
//!
//! *Run* turns the study into an OpenFOAM case and runs it on a thread of its own
//! (`oxyde_flow`: OpenFOAM is a separate program, GPL, never linked), the residuals plotted
//! in the panel as it goes. Then the goals' values are listed (an external flow's forces
//! with their drag coefficient), and the viewport shows the pressure on the model's wetted
//! faces or a cut plane through the fluid, in colour bands, or the flow's trajectories:
//! particles carried by the flow, each with a trail, playing over the faint model.
//!
//! The case folder is the results' home: beside the study file once it is saved
//! (`<study file>-<study>.openfoam`), in the temporary folder before; it opens in ParaView.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use oxyde_flow::plot::Quantity;
use oxyde_flow::trajectory::{Seeding, Trajectory};
use oxyde_format::results::{FlowRun, GoalOutcome, GoalResult, SolverInfo};
use oxyde_format::simulation::{
    CoolingSettings, FlowAnalysis, FlowGoalKind, FlowSettings, FlowTurbulence, Fluid, StudyType,
};
use oxyde_geom::Point3;
use oxyde_kernel::{FaceMesh, Mesh};
use oxyde_render::{Material as RenderMaterial, Scene};
use oxyde_sim::{MeshReport, TetMesh};

use crate::app::OxydeApp;
use crate::icons::Icon;
use crate::simulation_ui::{
    Done, ItemKind, Legend, Outcome, PanelAction, PlotChoice, PlotKey, RunMode, Shared, SimJob,
    Study, Surfaces, format_number, format_value, item_row,
};
use crate::viewport::ViewportOutput;

/// What a flow run uses at its peak per cell of the mesh, bytes: OpenFOAM's meshing and
/// its turbulent solver, with room to spare.
const BYTES_PER_CELL: f64 = 2_000.0;

/// The most cells a flow mesh may have within `budget_gb` of memory.
pub(crate) fn max_cells(budget_gb: f64) -> u64 {
    let budget = if budget_gb.is_finite() {
        budget_gb.max(0.25)
    } else {
        8.0
    };
    ((budget * 1e9 / BYTES_PER_CELL) as u64).max(10_000)
}

/// A flow study's run, as the tab keeps it.
pub(crate) struct FlowDone {
    /// What a results file saves of it: the goals, the residuals, how it went. `case` is
    /// the folder's full path here; the results file makes it relative.
    pub(crate) run: FlowRun,
    /// The OpenFOAM case folder.
    pub(crate) case_dir: PathBuf,
    /// The flow read from the case, for the plots; `None` when the case is gone.
    pub(crate) fields: Option<oxyde_flow::FlowResults>,
    /// An external flow's free stream (m/s) and the model's frontal area seen along it
    /// (mm²), for the drag coefficient: worked out when the flow is read, never saved.
    pub(crate) frontal: Option<([f64; 3], f64)>,
}

/// The free stream `velocity` (m/s) and the model's frontal area along it (mm²): the
/// silhouette of the mesh's faces on the model. `None` in still surroundings.
fn frontal_of(fields: &oxyde_flow::FlowResults, velocity: [f64; 3]) -> Option<([f64; 3], f64)> {
    oxyde_flow::drag::frontal_area(fields, velocity).map(|area| (velocity, area))
}

/// What the status bar says once a flow is solved.
pub(crate) fn flow_status(f: &FlowDone, seconds: f64) -> String {
    let run = &f.run;
    let what = if f.fields.as_ref().is_some_and(|r| r.temperature.is_some()) {
        "Cooling"
    } else {
        "Flow"
    };
    let state = if run.converged {
        String::new()
    } else {
        format!("; not converged: {}", not_converged(run))
    };
    format!(
        "{what} solved in {seconds:.0} s: {} cells, {} iterations{state}",
        run.cells, run.iterations
    )
}

/// What may help a run whose residuals did not reach the target. Once they have stopped
/// falling, more iterations will not: a steady solver's residuals level off and keep
/// swinging when the flow does not settle, as a wake shedding eddies behind a blunt body
/// does not.
fn not_converged(run: &FlowRun) -> String {
    match oxyde_flow::log::stopped_falling(&run.residuals) {
        Some(after) => format!(
            "its residuals stopped falling after about {} iterations, so more will not help; \
             the flow may not settle (a wake shedding eddies), and a higher target ends such \
             a run sooner",
            ((after + 5) / 10 * 10).max(10)
        ),
        None => "more iterations, a finer mesh or a higher target may help".to_owned(),
    }
}

/// What to do first, in the viewport's corner while a flow or cooling study has no results.
pub(crate) fn setup_hint(study: &Study) -> &'static str {
    let cooling = study.study_type.is_cooling();
    match (study.flow.analysis, cooling) {
        (FlowAnalysis::Internal, false) => {
            "Select the inner faces of the lids, add an Inlet and a Static Pressure, then Run"
        }
        (FlowAnalysis::External { .. }, false) => {
            "Set the free stream in the panel, add goals on the faces to measure, then Run"
        }
        (FlowAnalysis::Internal, true) => {
            "Add Internal Heat where heat is made, an Inlet and a Static Pressure on the lids, then Run"
        }
        (FlowAnalysis::External { .. }, true) => {
            "Add Internal Heat where heat is made, set the stream or gravity in the panel, then Run"
        }
    }
}

/// What a goal measures, under its row in the panel.
pub(crate) fn goal_hint(kind: FlowGoalKind) -> &'static str {
    match kind {
        FlowGoalKind::Force => "The fluid's force on the faces: pressure and friction",
        FlowGoalKind::VolumeFlow => "The volume flowing out through the faces (negative in)",
        FlowGoalKind::MassFlow => "The mass flowing out through the faces (negative in)",
        FlowGoalKind::MeanPressure => "The mean static pressure on the faces",
        FlowGoalKind::MaxTemperature => "The hottest point of the bodies the faces belong to",
        FlowGoalKind::MeanTemperature => "The mean temperature of the faces",
        FlowGoalKind::HeatFlow => "The heat leaving the model through the faces (negative in)",
    }
}

fn goal_kind(kind: FlowGoalKind) -> oxyde_flow::GoalKind {
    match kind {
        FlowGoalKind::Force => oxyde_flow::GoalKind::Force,
        FlowGoalKind::VolumeFlow => oxyde_flow::GoalKind::VolumeFlow,
        FlowGoalKind::MassFlow => oxyde_flow::GoalKind::MassFlow,
        FlowGoalKind::MeanPressure => oxyde_flow::GoalKind::MeanPressure,
        FlowGoalKind::MaxTemperature => oxyde_flow::GoalKind::MaxTemperature,
        FlowGoalKind::MeanTemperature => oxyde_flow::GoalKind::MeanTemperature,
        FlowGoalKind::HeatFlow => oxyde_flow::GoalKind::HeatFlow,
    }
}

/// `text` as a sentence of its own.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The study as `oxyde_flow` takes it, or what is missing. The mesh comes from the level
/// and the model's size, the domain from the model's box, and an internal flow's point in
/// the fluid from its openings' faces. A cooling study's bodies conduct heat with their
/// materials' conductivities.
pub(crate) fn flow_study(
    study: &Study,
    surfaces: &Surfaces,
    max_cells: u64,
) -> Result<oxyde_flow::FlowStudy, String> {
    let flow = &study.flow;
    let mut conditions = Vec::new();
    let mut goals = Vec::new();
    for item in study.items.iter().filter(|i| i.kind.is_flow()) {
        let labels = surfaces.labels(&item.faces);
        if labels.is_empty() {
            return Err(format!(
                "{} is on no face of the model any more: select its faces again",
                item.name
            ));
        }
        let kind = match item.kind {
            ItemKind::InletVelocity { speed } => oxyde_flow::ConditionKind::InletVelocity { speed },
            ItemKind::InletVolumeFlow { rate } => {
                oxyde_flow::ConditionKind::InletVolumeFlow { rate }
            }
            ItemKind::InletMassFlow { rate } => oxyde_flow::ConditionKind::InletMassFlow { rate },
            ItemKind::StaticPressure { pascals } => {
                oxyde_flow::ConditionKind::OutletPressure { pressure: pascals }
            }
            ItemKind::IdealWall => oxyde_flow::ConditionKind::IdealWall,
            ItemKind::Goal(kind) => {
                goals.push(oxyde_flow::Goal {
                    name: item.name.clone(),
                    kind: goal_kind(kind),
                    labels,
                });
                continue;
            }
            _ => continue,
        };
        conditions.push(oxyde_flow::Condition { kind, labels });
    }
    let solids: Vec<oxyde_flow::Surface> = surfaces
        .solids
        .iter()
        .map(|s| oxyde_flow::Surface {
            positions: s.positions.clone(),
            triangles: s.triangles.clone(),
            labels: s.labels.clone(),
        })
        .collect();
    let mut boxes = solids.iter().filter_map(oxyde_flow::Surface::bounds);
    let Some(mut bounds) = boxes.next() else {
        return Err("There is no solid to study yet".to_owned());
    };
    for (lo, hi) in boxes {
        for (b, l) in bounds.0.iter_mut().zip(lo) {
            *b = b.min(l);
        }
        for (b, h) in bounds.1.iter_mut().zip(hi) {
            *b = b.max(h);
        }
    }
    let heat = if study.study_type.is_cooling() {
        Some(heat_of(study, surfaces)?)
    } else {
        None
    };
    let turbulent = matches!(flow.turbulence, FlowTurbulence::Turbulent { .. });
    let guess = match flow.analysis {
        FlowAnalysis::Internal => oxyde_flow::FlowType::Internal { point: [0.0; 3] },
        FlowAnalysis::External { velocity } => oxyde_flow::FlowType::External { velocity },
    };
    let mesh =
        oxyde_flow::setup::mesh_for_level(flow.mesh_level, bounds, guess, turbulent, max_cells);
    let flow_type = match flow.analysis {
        FlowAnalysis::Internal => {
            let openings: Vec<u32> = conditions
                .iter()
                .filter(|c| !matches!(c.kind, oxyde_flow::ConditionKind::IdealWall))
                .flat_map(|c| c.labels.iter().copied())
                .collect();
            if openings.is_empty() {
                return Err(
                    "An internal flow comes in and goes out through lids over its openings: \
                     select their inner faces, then Inlet and Static Pressure"
                        .to_owned(),
                );
            }
            // A few of the finest cells in from an opening: well inside a cell of the fluid.
            let point = oxyde_flow::setup::point_inside(&solids, &openings, 3.0 * mesh.finest())
                .ok_or(
                    "The inlets and outlets are on no face of the model: select their faces again",
                )?;
            oxyde_flow::FlowType::Internal { point }
        }
        FlowAnalysis::External { velocity } => oxyde_flow::FlowType::External { velocity },
    };
    // Still surroundings: the domain reaches further above, for the plume.
    let domain = match (flow_type, heat.as_ref().and_then(|h| h.gravity)) {
        (oxyde_flow::FlowType::External { velocity }, Some(g))
            if velocity.iter().all(|v| *v == 0.0) =>
        {
            oxyde_flow::Domain::external(bounds, [-g[0], -g[1], -g[2]])
        }
        _ => oxyde_flow::setup::automatic_domain(bounds, flow_type, mesh.cell_size),
    };
    let out = oxyde_flow::FlowStudy {
        surfaces: solids,
        flow_type,
        domain,
        fluid: oxyde_flow::Fluid {
            name: flow.fluid.name.clone(),
            density: flow.fluid.density,
            viscosity: flow.fluid.viscosity,
        },
        turbulence: match flow.turbulence {
            FlowTurbulence::Laminar => oxyde_flow::Turbulence::Laminar,
            FlowTurbulence::Turbulent { intensity, length } => {
                oxyde_flow::Turbulence::KOmegaSst { intensity, length }
            }
        },
        conditions,
        goals,
        mesh,
        solver: oxyde_flow::SolverSettings {
            iterations: flow.iterations,
            residual: flow.residual,
            processes: 1,
        },
        heat,
    };
    out.check().map_err(|e| sentence(&e.to_string()))?;
    Ok(out)
}

/// A cooling study's heat: the fluid's, each body's conductivity, the thermal loads.
fn heat_of(study: &Study, surfaces: &Surfaces) -> Result<oxyde_flow::Heat, String> {
    let solids = surfaces
        .conductivities(study)?
        .into_iter()
        .map(|(name, conductivity, density)| oxyde_flow::Solid {
            name,
            conductivity,
            density,
        })
        .collect();
    let mut loads = Vec::new();
    for item in study
        .items
        .iter()
        .filter(|i| i.kind.is_thermal() && i.kind.fits(StudyType::Cooling))
    {
        let labels = surfaces.labels(&item.faces);
        if labels.is_empty() {
            return Err(format!(
                "{} is on no face of the model any more: select its faces again",
                item.name
            ));
        }
        let kind = match item.kind {
            ItemKind::Temperature { celsius } => oxyde_flow::HeatLoadKind::Temperature { celsius },
            ItemKind::HeatPower { watts } => oxyde_flow::HeatLoadKind::Power { watts },
            ItemKind::HeatFlux { flux } => oxyde_flow::HeatLoadKind::Flux { flux },
            ItemKind::Convection {
                coefficient,
                ambient,
            } => oxyde_flow::HeatLoadKind::Convection {
                coefficient,
                ambient,
            },
            ItemKind::InternalHeat { watts } => oxyde_flow::HeatLoadKind::Internal { watts },
            _ => continue,
        };
        loads.push(oxyde_flow::HeatLoad { kind, labels });
    }
    if !loads
        .iter()
        .any(|l| !matches!(l.kind, oxyde_flow::HeatLoadKind::Convection { .. }))
    {
        return Err(
            "Nothing heats the model: select faces and add Internal Heat, a Heat Power or Flux, or a Temperature"
                .to_owned(),
        );
    }
    let c = &study.cooling;
    Ok(oxyde_flow::Heat {
        specific_heat: c.specific_heat,
        conductivity: c.conductivity,
        expansion: c.expansion,
        temperature: c.temperature,
        gravity: c.gravity,
        solids,
        loads,
    })
}

/// Where a flow study's OpenFOAM case is kept beside its study file:
/// `<study file>-<study>.openfoam`, as its results are `<study file>-<study>.oxyres`.
pub(crate) fn case_path(study_file: &Path, study: &str) -> PathBuf {
    crate::simulation_files::results_path(study_file, study).with_extension("openfoam")
}

/// Copies the case folder `from` to `to`, leaving out its processor folders (the case is
/// put back together after a parallel run). An earlier case at `to` is replaced; anything
/// else there is refused.
fn copy_case(from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() {
        if !to.join("case.foam").is_file() {
            return Err(format!(
                "{} is in the way: it is not an OpenFOAM case of Oxyde's",
                to.display()
            ));
        }
        std::fs::remove_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    copy_folder(from, to)
}

fn copy_folder(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let entries = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let target = to.join(&name);
        if path.is_dir() {
            if name.to_string_lossy().starts_with("processor") {
                continue;
            }
            copy_folder(&path, &target)?;
        } else {
            std::fs::copy(&path, &target).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// A name made safe for a folder.
fn safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// What the study's thread runs.
struct FlowInput {
    study: oxyde_flow::FlowStudy,
    case_dir: PathBuf,
    /// Preferences › Simulation's installation, else the usual places.
    openfoam_dir: Option<PathBuf>,
    /// Preferences › Simulation's processes; 0 for one per core when MPI is there.
    processes: u32,
}

/// The processes to run on: those asked for, else one per core (up to eight) when the
/// installation has MPI, else one.
fn processes(asked: u32, install: &oxyde_flow::Installation) -> u32 {
    if asked > 0 {
        return asked;
    }
    let launcher = if cfg!(windows) { "mpiexec" } else { "mpirun" };
    if install.program(launcher).is_err() {
        return 1;
    }
    std::thread::available_parallelism()
        .map_or(1, |n| n.get() as u32)
        .clamp(1, 8)
}

/// The fields of `history` in the order they were first solved, and each iteration's
/// residuals in that order (NaN for a field not solved in it).
pub(crate) fn residual_table(history: &[Vec<(String, f64)>]) -> (Vec<String>, Vec<Vec<f64>>) {
    let mut fields: Vec<String> = Vec::new();
    for row in history {
        for (name, _) in row {
            if !fields.contains(name) {
                fields.push(name.clone());
            }
        }
    }
    let rows = history
        .iter()
        .map(|row| {
            fields
                .iter()
                .map(|f| {
                    row.iter()
                        .find(|(name, _)| name == f)
                        .map_or(f64::NAN, |(_, r)| *r)
                })
                .collect()
        })
        .collect();
    (fields, rows)
}

/// The study's work on its own thread: OpenFOAM found, the case written and run, the
/// flow read back.
fn flow_thread(
    input: FlowInput,
    cancel: &AtomicBool,
    shared: &Mutex<Shared>,
) -> Result<Outcome, String> {
    let report_progress = |stage: &'static str, fraction: f64| {
        if let Ok(mut s) = shared.lock() {
            s.stage = stage;
            s.fraction = fraction.clamp(0.0, 1.0);
        }
    };
    report_progress("Finding OpenFOAM", 0.0);
    let install = match &input.openfoam_dir {
        Some(dir) => oxyde_flow::Installation::open(dir),
        None => oxyde_flow::Installation::find(),
    }
    .map_err(|e| sentence(&e.to_string()))?;
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".to_owned());
    }
    let mut study = input.study;
    study.solver.processes = processes(input.processes, &install);
    report_progress("Writing the case", 0.01);
    let case = oxyde_flow::write_case(&input.case_dir, &study)
        .map_err(|e| format!("The OpenFOAM case could not be written: {e}"))?;
    let started = Instant::now();
    let report = oxyde_flow::run(&case, &install, cancel, &mut |p| {
        use oxyde_flow::Stage;
        let fraction = match p.stage {
            Stage::Background => 0.02,
            Stage::Features => 0.04,
            Stage::Snapping => 0.06,
            Stage::Regions => 0.2,
            Stage::Checking => 0.25,
            Stage::Decomposing => 0.27,
            Stage::Initialising => 0.28,
            Stage::Solving => 0.3 + 0.7 * f64::from(p.iteration) / f64::from(p.iterations.max(1)),
            Stage::Reconstructing => 0.99,
        };
        if let Ok(mut s) = shared.lock() {
            s.stage = p.stage.describe();
            s.fraction = fraction.clamp(0.0, 1.0);
            if p.stage == Stage::Solving && p.iteration > 0 {
                let at = p.iteration as usize;
                if s.residuals.len() < at {
                    s.residuals.resize(at, Vec::new());
                }
                if let Some(row) = s.residuals.get_mut(at - 1) {
                    row.clone_from(&p.residuals);
                }
            }
        }
    })
    .map_err(|e| match e {
        oxyde_flow::RunError::Cancelled => "cancelled".to_owned(),
        other => sentence(&other.to_string()),
    })?;
    report_progress("Reading the results", 1.0);
    let fields = oxyde_flow::FlowResults::read(&case)
        .map_err(|e| format!("The results could not be read: {e}"))?;
    let goals = case
        .goals
        .iter()
        .zip(fields.goals(&case))
        .map(|(goal, value)| GoalResult {
            name: goal.name.clone(),
            kind: match goal.kind {
                oxyde_flow::GoalKind::Force => FlowGoalKind::Force,
                oxyde_flow::GoalKind::VolumeFlow => FlowGoalKind::VolumeFlow,
                oxyde_flow::GoalKind::MassFlow => FlowGoalKind::MassFlow,
                oxyde_flow::GoalKind::MeanPressure => FlowGoalKind::MeanPressure,
                oxyde_flow::GoalKind::MaxTemperature => FlowGoalKind::MaxTemperature,
                oxyde_flow::GoalKind::MeanTemperature => FlowGoalKind::MeanTemperature,
                oxyde_flow::GoalKind::HeatFlow => FlowGoalKind::HeatFlow,
            },
            value: match value {
                oxyde_flow::GoalValue::Scalar { value, .. } => GoalOutcome::Value(value),
                oxyde_flow::GoalValue::Force {
                    total,
                    pressure,
                    viscous,
                    ..
                } => GoalOutcome::Force {
                    total,
                    pressure,
                    viscous,
                },
                oxyde_flow::GoalValue::Missing { reason, .. } => GoalOutcome::Missing(reason),
            },
        })
        .collect();
    let frontal = match study.flow_type {
        oxyde_flow::FlowType::External { velocity } => frontal_of(&fields, velocity),
        oxyde_flow::FlowType::Internal { .. } => None,
    };
    let history: Vec<Vec<(String, f64)>> =
        report.history.iter().map(|i| i.residuals.clone()).collect();
    let (residual_fields, residuals) = residual_table(&history);
    let run = FlowRun {
        case: input.case_dir.display().to_string(),
        time: fields.time.clone(),
        openfoam: install.version().to_owned(),
        cells: report.cells.unwrap_or(fields.mesh.cells as u64),
        iterations: report.iterations,
        converged: report.converged,
        goals,
        residual_fields,
        residuals,
    };
    Ok(Outcome {
        mesh: TetMesh::default(),
        report: no_mesh(),
        results: None,
        thermal: None,
        topology: None,
        frequency: None,
        mesh_seconds: 0.0,
        solve_seconds: started.elapsed().as_secs_f64(),
        candidates: None,
        mesher: "openfoam",
        note: None,
        flow: Some(FlowDone {
            run,
            case_dir: input.case_dir,
            fields: Some(fields),
            frontal,
        }),
    })
}

/// The mesher's report of a flow run, which has no tetrahedra.
fn no_mesh() -> MeshReport {
    MeshReport {
        element_size: 0.0,
        nodes: 0,
        elements: 0,
        volume: 0.0,
        model_volume: 0.0,
        dropped: 0,
    }
}

/// The colours of a residual chart's fields, as they come.
const FIELD_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(220, 60, 50),
    egui::Color32::from_rgb(40, 160, 70),
    egui::Color32::from_rgb(40, 110, 220),
    egui::Color32::from_rgb(230, 150, 30),
    egui::Color32::from_rgb(150, 70, 200),
    egui::Color32::from_rgb(30, 170, 180),
];

/// The residuals of each iteration on a log scale, one line per field, with the target.
fn residual_chart(ui: &mut egui::Ui, fields: &[String], rows: &[Vec<f64>], target: f64) {
    let size = egui::vec2(ui.available_width().max(120.0), 120.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let text = ui.visuals().text_color();
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0_f32, ui.visuals().weak_text_color()),
    );
    let plot = rect.shrink2(egui::vec2(6.0, 14.0));
    // From the target down a decade to 1, on powers of ten.
    let lowest = rows
        .iter()
        .flatten()
        .copied()
        .filter(|r| r.is_finite() && *r > 0.0)
        .fold(target.max(1e-12), f64::min);
    let bottom = (lowest.log10().floor() - 0.5).min(target.log10() - 1.0);
    let top = 0.5_f64;
    let count = rows.len().max(2);
    let at = |i: usize, r: f64| -> egui::Pos2 {
        let x = plot.left() + plot.width() * i as f32 / (count - 1) as f32;
        let t = ((r.log10() - bottom) / (top - bottom)).clamp(0.0, 1.0) as f32;
        egui::pos2(x, plot.bottom() - plot.height() * t)
    };
    // The target, dashed.
    let y = at(0, target).y;
    painter.extend(egui::Shape::dashed_line(
        &[egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
        egui::Stroke::new(1.0_f32, text.gamma_multiply(0.6)),
        4.0,
        3.0,
    ));
    for (k, name) in fields.iter().enumerate() {
        let color = FIELD_COLORS
            .get(k % FIELD_COLORS.len())
            .copied()
            .unwrap_or(text);
        let points: Vec<egui::Pos2> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                let r = *row.get(k)?;
                (r.is_finite() && r > 0.0).then(|| at(i, r))
            })
            .collect();
        if points.len() > 1 {
            painter.add(egui::Shape::line(points, egui::Stroke::new(1.3_f32, color)));
        }
        painter.text(
            egui::pos2(rect.left() + 6.0 + 34.0 * k as f32, rect.top() + 2.0),
            egui::Align2::LEFT_TOP,
            name,
            egui::FontId::proportional(10.5),
            color,
        );
    }
    response.on_hover_text(format!(
        "Each equation's residual at every iteration, on a log scale; the run has converged once all are under the dashed line ({})",
        format_number(target)
    ));
}

/// The fluids the panel offers, their properties, and how they carry heat.
fn fluid_presets() -> [(Fluid, CoolingSettings); 2] {
    [
        (Fluid::water(), CoolingSettings::water()),
        (Fluid::air(), CoolingSettings::air()),
    ]
}

impl OxydeApp {
    /// Run, for a flow study: OpenFOAM on a thread of its own.
    pub(crate) fn run_flow(&mut self) {
        if let Err(e) = self.ensure_surfaces() {
            self.simulation.error = Some(e);
            return;
        }
        let Some(surfaces) = self.simulation.surfaces.as_ref() else {
            return;
        };
        let budget = self.prefs.simulation.memory_budget_gb;
        let study = match flow_study(&self.simulation.study, surfaces, max_cells(budget)) {
            Ok(s) => s,
            Err(e) => {
                self.simulation.error = Some(e.clone());
                self.status = e;
                return;
            }
        };
        let faces = surfaces.faces.clone();
        let diagonal = surfaces.diagonal;
        let dir = self.prefs.simulation.openfoam_dir.trim();
        let input = FlowInput {
            study,
            case_dir: self.flow_case_dir(),
            openfoam_dir: (!dir.is_empty()).then(|| PathBuf::from(dir)),
            processes: self.prefs.simulation.flow_processes,
        };
        let fingerprint = self.simulation_fingerprint().ok();
        let cancel = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared {
            stage: "Starting OpenFOAM",
            ..Shared::default()
        }));
        let (c, s) = (cancel.clone(), shared.clone());
        let spawned = std::thread::Builder::new()
            .name("oxyde flow".into())
            .spawn(move || {
                let result = oxyde_kernel::catch_panic(|| flow_thread(input, &c, &s));
                let outcome = match result {
                    Ok(r) => r,
                    Err(message) => Err(format!("Internal error in the flow study: {message}")),
                };
                if let Ok(mut shared) = s.lock() {
                    shared.outcome = Some(outcome);
                }
            });
        if let Err(e) = spawned {
            self.simulation.error = Some(format!("The flow study's thread could not start: {e}"));
            return;
        }
        self.simulation.job = Some(SimJob {
            cancel,
            shared,
            revision: self.document_revision(),
            assembly: self.asm.is_some(),
            started: Instant::now(),
            diagonal,
            mode: RunMode::Run,
            faces,
            bodies: Vec::new(),
            fingerprint,
            study_type: self.simulation.study.study_type,
        });
        self.status = if self.simulation.study.study_type.is_cooling() {
            "Running the cooling study with OpenFOAM…".to_owned()
        } else {
            "Running the flow study with OpenFOAM…".to_owned()
        };
    }

    /// A flow study's case moved beside its study file, where its results go, when it is
    /// elsewhere (in the temporary folder: it was solved before the study was saved).
    pub(crate) fn keep_flow_case(&mut self, study_file: &Path) -> Result<(), String> {
        let target = case_path(study_file, &self.simulation.study.name);
        let Some(flow) = self.simulation.done.as_mut().and_then(|d| d.flow.as_mut()) else {
            return Ok(());
        };
        if flow.case_dir == target {
            return Ok(());
        }
        copy_case(&flow.case_dir, &target)
            .map_err(|e| format!("Could not keep the OpenFOAM case beside the study: {e}"))?;
        flow.case_dir = target;
        Ok(())
    }

    /// Where the open flow study's case goes: beside its study file once saved, else in the
    /// temporary folder, by document and study.
    pub(crate) fn flow_case_dir(&self) -> PathBuf {
        match &self.simulation.file {
            Some(f) => case_path(&f.path, &self.simulation.study.name),
            None => {
                let document = match &self.asm {
                    Some(asm) => asm.document_id.clone(),
                    None => self.doc.document_id.clone(),
                };
                std::env::temp_dir()
                    .join("oxyde-flow")
                    .join(safe(&document))
                    .join(safe(&self.simulation.study.name))
            }
        }
    }

    /// Before the viewport is drawn: a flow's plot in place of the model, made again when
    /// the result or the plot settings changed.
    pub(crate) fn flow_scene(&mut self) {
        let prefs = self.prefs.simulation.clone();
        let Some(done) = self.simulation.done.as_ref() else {
            return;
        };
        let Some(flow) = done.flow.as_ref() else {
            return;
        };
        let plot = done.plot_for(prefs.plot).filter(|p| p.is_flow());
        // The plot shown is the one chosen from now on, so the command bar lights it.
        if let Some(p) = plot
            && p != prefs.plot
        {
            self.prefs.simulation.plot = p;
            self.simulation.prefs_dirty = true;
        }
        let quantity = match plot {
            Some(PlotChoice::FlowCutVelocity | PlotChoice::FlowTrajectories) => Quantity::Speed,
            Some(PlotChoice::CoolingCutTemperature | PlotChoice::CoolingSurfaceTemperature) => {
                Quantity::Temperature
            }
            _ => Quantity::Pressure,
        };
        let cutting = plot.is_some_and(PlotChoice::is_cut);
        let tracing = plot == Some(PlotChoice::FlowTrajectories);
        // The paths, followed again for other results or another number of them.
        let wanted = (done.id, prefs.trajectories.clamp(10, 1000));
        if tracing
            && self
                .simulation
                .particles
                .as_ref()
                .is_none_or(|p| p.key != wanted)
            && let Some(fields) = flow.fields.as_ref()
        {
            let playing = self.simulation.particles.as_ref().is_none_or(|p| p.playing);
            let mut particles =
                Particles::follow(fields, self.simulation.study.flow.analysis, wanted);
            particles.playing = playing;
            self.simulation.particles = Some(particles);
            // The legend follows the new paths' speeds.
            self.simulation.shown = None;
        }
        let mut position = prefs.cut_position.clamp(0.0, 1.0);
        // A new result, or another axis, starts the plane through the middle of the model
        // rather than of the fluid, which for an external flow is far behind it; the slider
        // then moves it anywhere across the fluid.
        let start = Some((done.id, prefs.cut_axis));
        if cutting && self.simulation.cut_centred != start {
            if let Some(middle) = flow
                .fields
                .as_ref()
                .and_then(|f| oxyde_flow::plot::model_middle(f, prefs.cut_axis.index()))
            {
                position = middle;
                self.prefs.simulation.cut_position = middle;
                self.simulation.prefs_dirty = true;
            }
            self.simulation.cut_centred = start;
        }
        let key = PlotKey {
            id: done.id,
            plot,
            scale: 0.0,
            bands: prefs.bands.clamp(2, 64),
            show_mesh: false,
            frame: None,
            cut: cutting.then_some((prefs.cut_axis, position)),
            threshold: 0.0,
            smooth: false,
            ghost: false,
        };
        if self.simulation.shown == Some(key) {
            return;
        }
        let Some(fields) = flow.fields.as_ref() else {
            return;
        };
        let polygons = if cutting {
            let Some((lo, hi)) = oxyde_flow::plot::bounds(fields) else {
                return;
            };
            let axis = prefs.cut_axis.index();
            let mut origin = [
                0.5 * (lo[0] + hi[0]),
                0.5 * (lo[1] + hi[1]),
                0.5 * (lo[2] + hi[2]),
            ];
            let mut normal = [0.0; 3];
            if let (Some(o), Some(n), Some(l), Some(h)) = (
                origin.get_mut(axis),
                normal.get_mut(axis),
                lo.get(axis),
                hi.get(axis),
            ) {
                *o = l + (h - l) * position;
                *n = 1.0;
            }
            oxyde_flow::plot::cut(fields, origin, normal, quantity)
        } else if tracing {
            // Drawn over the viewport, not in the scene: they move every frame.
            Vec::new()
        } else {
            oxyde_flow::plot::surface(fields, quantity)
        };
        let particles = self.simulation.particles.as_ref().filter(|_| tracing);
        let (lo, hi) = match particles {
            Some(p) => p.range,
            None => oxyde_flow::plot::range(&polygons).unwrap_or((0.0, 1.0)),
        };
        let bands = key.bands as usize;
        // Each polygon fanned into triangles, one face of the scene per colour band.
        let mut faces: Vec<FaceMesh> = (0..bands)
            .map(|_| FaceMesh {
                positions: Vec::new(),
                triangles: Vec::new(),
            })
            .collect();
        for p in &polygons {
            let t = if hi > lo {
                ((p.value - lo) / (hi - lo)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let band = ((t * bands as f64) as usize).min(bands.saturating_sub(1));
            let Some(face) = faces.get_mut(band) else {
                continue;
            };
            let base = face.positions.len() as u32;
            face.positions.extend(
                p.points
                    .iter()
                    .map(|q| [q[0] as f32, q[1] as f32, q[2] as f32]),
            );
            for k in 1..p.points.len().saturating_sub(1) {
                let k = k as u32;
                face.triangles.push([base, base + k, base + k + 1]);
            }
        }
        let mut materials: Vec<RenderMaterial> = (0..bands)
            .map(|b| RenderMaterial {
                color: Some(oxyde_sim::rainbow((b as f64 + 0.5) / bands as f64)),
                ..RenderMaterial::default()
            })
            .collect();
        // A cut plane or the trajectories are seen in the model, drawn faint around them.
        if (cutting || tracing)
            && let Some(surfaces) = &self.simulation.surfaces
        {
            let mut model = FaceMesh {
                positions: Vec::new(),
                triangles: Vec::new(),
            };
            for s in &surfaces.solids {
                let base = model.positions.len() as u32;
                model.positions.extend(
                    s.positions
                        .iter()
                        .map(|q| [q[0] as f32, q[1] as f32, q[2] as f32]),
                );
                model.triangles.extend(
                    s.triangles
                        .iter()
                        .map(|t| [base + t[0], base + t[1], base + t[2]]),
                );
            }
            faces.push(model);
            materials.push(RenderMaterial {
                color: Some([200, 205, 212]),
                opacity: 0.22,
                ..RenderMaterial::default()
            });
        }
        let marker = polygons
            .iter()
            .filter(|p| p.value.is_finite() && !p.points.is_empty())
            .max_by(|a, b| a.value.total_cmp(&b.value))
            .map(|p| {
                let n = p.points.len() as f64;
                let c = p.points.iter().fold([0.0; 3], |s, q| {
                    [s[0] + q[0] / n, s[1] + q[1] / n, s[2] + q[2] / n]
                });
                (c, p.value)
            })
            .or_else(|| particles.and_then(Particles::fastest))
            .map(|(c, v)| (c, format!("Max: {}", format_value(v, quantity.unit()))));
        let title = plot.map_or("Flow", PlotChoice::label).to_owned();
        let legend = Legend {
            title,
            unit: quantity.unit(),
            range: (lo, hi),
            bands: key.bands,
            reversed: false,
            scale: 0.0,
            marker,
        };
        let mut scene = Scene::from_mesh(&Mesh {
            faces,
            edges: Vec::new(),
        });
        scene.set_materials(materials, Vec::new());
        // Looked at, not picked: the faces are the plot's, not the model's.
        self.viewport.pickable = Some((0, 0));
        self.viewport.set_scene(scene, false);
        match self.asm.as_ref() {
            Some(asm) => self.asm_scene = Some(asm.revision),
            None => self.scene_revision = Some(self.scene_key()),
        }
        self.simulation.shown = Some(key);
        self.simulation.legend = Some(legend);
    }

    /// The command bar of a flow study: inlets, static pressure, ideal wall, goals, Run, and
    /// the plots.
    pub(crate) fn flow_tab(
        &mut self,
        ui: &mut egui::Ui,
        has_model: bool,
        running: bool,
        has_faces: bool,
    ) {
        use crate::icons::command_button;
        let enabled = has_model && !running;
        let cooling = self.simulation.study.study_type.is_cooling();
        let hint = |text: &str| {
            if has_faces {
                text.to_owned()
            } else {
                format!("{text}\n(select faces first)")
            }
        };
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        // Inlet, a menu of its three kinds, as SolidWorks' inlet conditions.
        let inlet = command_button(ui, Icon::Inlet, "Inlet", enabled, false)
            .on_hover_text(hint("Fluid coming in through the selected faces: the inner faces of lids over an internal flow's openings"));
        crate::icons::dropdown_marker(ui, &inlet);
        let inlet_popup = ui.make_persistent_id("flow_inlet_menu");
        if inlet.clicked() {
            ui.memory_mut(|m| m.toggle_popup(inlet_popup));
        }
        let mut add = None;
        egui::popup::popup_below_widget(
            ui,
            inlet_popup,
            &inlet,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(170.0);
                for (label, kind, text) in [
                    (
                        "Inlet Velocity",
                        ItemKind::InletVelocity { speed: 1.0 },
                        "At a speed, square to the faces",
                    ),
                    (
                        "Inlet Volume Flow",
                        ItemKind::InletVolumeFlow { rate: 1e-3 / 60.0 },
                        "A volume a minute, shared between the faces by area",
                    ),
                    (
                        "Inlet Mass Flow",
                        ItemKind::InletMassFlow { rate: 0.01 },
                        "A mass a second, shared between the faces by area",
                    ),
                ] {
                    if ui.button(label).on_hover_text(text).clicked() {
                        add = Some(kind);
                    }
                }
            },
        );
        if command_button(ui, Icon::Outlet, "Static Pressure", enabled, false)
            .on_hover_text(hint(
                "An opening at a pressure, where the fluid goes out (0 Pa: the surroundings')",
            ))
            .clicked()
        {
            add = Some(ItemKind::StaticPressure { pascals: 0.0 });
        }
        if command_button(ui, Icon::IdealWall, "Ideal Wall", enabled, false)
            .on_hover_text(hint(
                "A wall the fluid slips along without friction; also a plane of symmetry",
            ))
            .clicked()
        {
            add = Some(ItemKind::IdealWall);
        }
        if cooling {
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(2.0);
            // Heat, a menu of the thermal loads a cooling study takes.
            let heat = command_button(ui, Icon::InternalHeat, "Heat", enabled, false)
                .on_hover_text(hint(
                    "Heat made in the bodies or let in through faces, held temperatures, convection where no fluid touches",
                ));
            crate::icons::dropdown_marker(ui, &heat);
            let heat_popup = ui.make_persistent_id("cooling_heat_menu");
            if heat.clicked() {
                ui.memory_mut(|m| m.toggle_popup(heat_popup));
            }
            egui::popup::popup_below_widget(
                ui,
                heat_popup,
                &heat,
                egui::PopupCloseBehavior::CloseOnClick,
                |ui| {
                    ui.set_min_width(170.0);
                    for (label, kind, text) in [
                        (
                            "Internal Heat",
                            ItemKind::InternalHeat { watts: 5.0 },
                            "Heat made inside the bodies of the selected faces, W in all: a chip's or a motor's losses",
                        ),
                        (
                            "Heat Power",
                            ItemKind::HeatPower { watts: 10.0 },
                            "Heat coming in through the selected faces, W in all, shared by area",
                        ),
                        (
                            "Heat Flux",
                            ItemKind::HeatFlux { flux: 1000.0 },
                            "Heat coming in through the selected faces, W/m²",
                        ),
                        (
                            "Temperature",
                            ItemKind::Temperature { celsius: 60.0 },
                            "Hold the selected faces at a temperature",
                        ),
                        (
                            "Convection",
                            ItemKind::Convection {
                                coefficient: 10.0,
                                ambient: 20.0,
                            },
                            "Faces the study's fluid does not touch lose heat to surroundings (10 W/m²·K: still air)",
                        ),
                    ] {
                        if ui.button(label).on_hover_text(text).clicked() {
                            add = Some(kind);
                        }
                    }
                },
            );
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        let goal = command_button(ui, Icon::Goal, "Goal", enabled, false).on_hover_text(hint(
            "Something to measure on the selected faces: a force, a flow rate, a mean pressure",
        ));
        crate::icons::dropdown_marker(ui, &goal);
        let goal_popup = ui.make_persistent_id("flow_goal_menu");
        if goal.clicked() {
            ui.memory_mut(|m| m.toggle_popup(goal_popup));
        }
        egui::popup::popup_below_widget(
            ui,
            goal_popup,
            &goal,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(170.0);
                let thermal = [
                    ("Max Temperature", FlowGoalKind::MaxTemperature),
                    ("Mean Temperature", FlowGoalKind::MeanTemperature),
                    ("Heat Flow", FlowGoalKind::HeatFlow),
                ];
                for (label, kind) in [
                    ("Force", FlowGoalKind::Force),
                    ("Volume Flow", FlowGoalKind::VolumeFlow),
                    ("Mass Flow", FlowGoalKind::MassFlow),
                    ("Mean Pressure", FlowGoalKind::MeanPressure),
                ]
                .into_iter()
                .chain(thermal.into_iter().filter(|_| cooling))
                {
                    if ui.button(label).on_hover_text(goal_hint(kind)).clicked() {
                        add = Some(ItemKind::Goal(kind));
                    }
                }
            },
        );
        if let Some(kind) = add {
            self.add_study_item(kind);
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        if running {
            if command_button(ui, Icon::Cancel, "Cancel", true, false)
                .on_hover_text("Stop OpenFOAM")
                .clicked()
            {
                self.cancel_simulation();
            }
        } else if command_button(ui, Icon::RunStudy, "Run", has_model, false)
            .on_hover_text("Mesh the fluid and solve the flow with OpenFOAM")
            .clicked()
        {
            self.run_simulation(RunMode::Run);
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        let done = self
            .simulation
            .done
            .as_ref()
            .filter(|d| d.solved() && d.revision == self.document_revision());
        let mut show = None;
        let temperatures = [
            (
                Icon::TemperaturePlot,
                "Temperature",
                PlotChoice::CoolingCutTemperature,
                "The temperature on a plane through the fluid and the solids (set the plane in the panel)",
            ),
            (
                Icon::SurfacePlot,
                "Surface Temperature",
                PlotChoice::CoolingSurfaceTemperature,
                "The temperature on the model's faces",
            ),
        ];
        let temperatures = temperatures.into_iter().filter(|_| cooling);
        for (icon, label, plot, text) in temperatures.chain([
            (
                Icon::CutPlot,
                "Velocity",
                PlotChoice::FlowCutVelocity,
                "The speed on a plane through the fluid (set the plane in the panel)",
            ),
            (
                Icon::CutPlot,
                "Pressure",
                PlotChoice::FlowCutPressure,
                "The pressure on a plane through the fluid",
            ),
            (
                Icon::SurfacePlot,
                "Surface Pressure",
                PlotChoice::FlowSurfacePressure,
                "The pressure on the model's faces the fluid wets",
            ),
            (
                Icon::Trajectories,
                "Trajectories",
                PlotChoice::FlowTrajectories,
                "Flow trajectories: particles carried by the flow from upstream of the model (or from the inlets), each with a trail coloured by speed",
            ),
        ]) {
            let available = done.is_some_and(|d| d.has(plot));
            let active =
                available && self.simulation.show_plot && self.prefs.simulation.plot == plot;
            if command_button(ui, icon, label, available, active)
                .on_hover_text(text)
                .clicked()
            {
                show = Some(plot);
            }
        }
        if let Some(plot) = show {
            self.show_plot(Some(plot));
        }
    }

    /// A flow study's panel: the fluid and the flow, the conditions and goals, the mesh and
    /// the solver, and the results. What it asks for, once drawn.
    pub(crate) fn flow_panel(&mut self, ui: &mut egui::Ui, max_height: f32) -> Option<PanelAction> {
        let mut action = None;
        let live = self
            .simulation
            .job
            .as_ref()
            .and_then(|j| j.shared.lock().ok().map(|s| s.residuals.clone()));
        let current = self.document_revision();
        egui::ScrollArea::vertical()
            .id_salt("flow_panel")
            .max_height(max_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let study = &mut self.simulation.study;
                let selected = &mut self.simulation.selected;
                let mut edited = false;
                let cooling = study.study_type.is_cooling();
                egui::CollapsingHeader::new("Fluid")
                    .default_open(true)
                    .show(ui, |ui| {
                        let heat = cooling.then_some(&mut study.cooling);
                        edited |= fluid_rows(ui, &mut study.flow, heat);
                    });
                egui::CollapsingHeader::new("Boundary conditions")
                    .default_open(true)
                    .show(ui, |ui| {
                        let mut any = false;
                        for (i, item) in study.items.iter_mut().enumerate() {
                            if !item.kind.is_flow() || item.kind.is_goal() {
                                continue;
                            }
                            any = true;
                            item_row(ui, i, item, selected, &mut action);
                        }
                        if !any {
                            let text = match study.flow.analysis {
                                FlowAnalysis::Internal => {
                                    "Close each opening with a lid, select the lids' inner faces, then Inlet or Static Pressure. Faces with none are walls."
                                }
                                FlowAnalysis::External { .. } => {
                                    "The free stream flows round the model: walls everywhere, none needed. Ideal Wall makes faces slip."
                                }
                            };
                            ui.label(egui::RichText::new(text).small().weak());
                        }
                    });
                if cooling {
                    egui::CollapsingHeader::new("Heat")
                        .default_open(true)
                        .show(ui, |ui| {
                            edited |= heat_rows(ui, &mut study.cooling);
                            let mut any = false;
                            for (i, item) in study.items.iter_mut().enumerate() {
                                if !item.kind.is_thermal() {
                                    continue;
                                }
                                any = true;
                                item_row(ui, i, item, selected, &mut action);
                            }
                            if !any {
                                ui.label(
                                    egui::RichText::new(
                                        "Select a face of each body that makes heat, then Heat › Internal Heat. Bodies conduct with their materials' conductivity.",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                        });
                }
                egui::CollapsingHeader::new("Goals")
                    .default_open(true)
                    .show(ui, |ui| {
                        let mut any = false;
                        for (i, item) in study.items.iter_mut().enumerate() {
                            if !item.kind.is_goal() {
                                continue;
                            }
                            any = true;
                            item_row(ui, i, item, selected, &mut action);
                        }
                        if !any {
                            ui.label(
                                egui::RichText::new(if cooling {
                                    "Select faces, then Goal: a temperature, a heat flow, a force or a flow rate to measure on them"
                                } else {
                                    "Select faces, then Goal: a force, a flow rate or a mean pressure to measure on them"
                                })
                                .small()
                                .weak(),
                            );
                        }
                    });
                egui::CollapsingHeader::new("Mesh and solver")
                    .default_open(false)
                    .show(ui, |ui| {
                        edited |= solver_rows(ui, &mut study.flow);
                    });
                if edited {
                    action.get_or_insert(PanelAction::Edited);
                }
                egui::CollapsingHeader::new("Results")
                    .default_open(true)
                    .show(ui, |ui| {
                        let target = study.flow.residual;
                        if let Some(history) = &live {
                            let (fields, rows) = residual_table(history);
                            if rows.is_empty() {
                                ui.label(egui::RichText::new("Meshing the fluid…").weak());
                            } else {
                                residual_chart(ui, &fields, &rows, target);
                            }
                            return;
                        }
                        let done = self.simulation.done.as_ref();
                        let Some((d, flow)) = done.and_then(|d| d.flow.as_ref().map(|f| (d, f))) else {
                            ui.label(egui::RichText::new("Not run yet").weak());
                            return;
                        };
                        let run = &flow.run;
                        if d.revision != current {
                            ui.label(
                                egui::RichText::new("The model changed: run the study again")
                                    .small()
                                    .color(egui::Color32::from_rgb(210, 140, 30)),
                            );
                        }
                        // The free stream the forces' drag coefficients are taken along: the
                        // one solved, while the study still has it.
                        let drag = match (study.flow.analysis, flow.frontal) {
                            (FlowAnalysis::External { velocity }, Some((solved, area)))
                                if velocity == solved =>
                            {
                                Some(Drag {
                                    velocity,
                                    density: study.flow.fluid.density,
                                    area,
                                })
                            }
                            _ => None,
                        };
                        for goal in &run.goals {
                            goal_row(ui, goal, drag);
                        }
                        let state = if run.converged {
                            "converged".to_owned()
                        } else {
                            "not converged".to_owned()
                        };
                        let line = format!(
                            "{} cells, {} iterations, {state}, OpenFOAM {}",
                            run.cells, run.iterations, run.openfoam
                        );
                        if run.converged {
                            ui.label(egui::RichText::new(line).small().weak());
                        } else {
                            ui.label(
                                egui::RichText::new(format!("{line}: {}", not_converged(run)))
                                    .small()
                                    .color(egui::Color32::from_rgb(210, 140, 30)),
                            );
                        }
                        if !run.residuals.is_empty() {
                            residual_chart(ui, &run.residual_fields, &run.residuals, target);
                        }
                        if flow.fields.is_none() {
                            ui.label(
                                egui::RichText::new(
                                    "The OpenFOAM case is not there any more: run again to plot the flow",
                                )
                                .small()
                                .color(egui::Color32::from_rgb(210, 140, 30)),
                            );
                        }
                        ui.horizontal(|ui| {
                            let folder = flow.case_dir.display().to_string();
                            ui.label(egui::RichText::new("Case").small().weak())
                                .on_hover_text(&folder);
                            if ui
                                .small_button("Copy its folder")
                                .on_hover_text(format!(
                                    "{folder}\nOpenFOAM's own files: open case.foam in ParaView"
                                ))
                                .clicked()
                            {
                                ui.ctx().copy_text(folder);
                            }
                        });
                        let mut shown = self.simulation.show_plot;
                        if ui.checkbox(&mut shown, "Show results").changed() {
                            action = Some(PanelAction::ShowPlot(shown));
                        }
                        let p = &mut self.prefs.simulation;
                        let particles = &mut self.simulation.particles;
                        let study_type = d.study_type;
                        egui::Grid::new("flow_plot")
                            .num_columns(2)
                            .spacing([10.0, 4.0])
                            .show(ui, |ui| {
                                ui.label("Plot");
                                let shown = d.plot_for(p.plot).filter(|c| c.is_flow()).unwrap_or(p.plot);
                                egui::ComboBox::from_id_salt("flow_plot_choice")
                                    .selected_text(shown.label())
                                    .show_ui(ui, |ui| {
                                        for c in PlotChoice::ALL
                                            .into_iter()
                                            .filter(|c| c.is_flow() && c.fits(study_type))
                                        {
                                            ui.selectable_value(&mut p.plot, c, c.label());
                                        }
                                    });
                                ui.end_row();
                                if shown.is_cut() {
                                    ui.label("Plane");
                                    egui::ComboBox::from_id_salt("flow_cut_axis")
                                        .selected_text(p.cut_axis.label())
                                        .show_ui(ui, |ui| {
                                            for a in crate::simulation_ui::CutAxis::ALL {
                                                ui.selectable_value(&mut p.cut_axis, a, a.label());
                                            }
                                        });
                                    ui.end_row();
                                    ui.label("Where");
                                    ui.add(
                                        egui::Slider::new(&mut p.cut_position, 0.0..=1.0)
                                            .show_value(false),
                                    )
                                    .on_hover_text("Where the plane lies across the fluid");
                                    ui.end_row();
                                }
                                if shown == PlotChoice::FlowTrajectories {
                                    ui.label("Trajectories");
                                    egui::ComboBox::from_id_salt("flow_trajectories")
                                        .selected_text(p.trajectories.to_string())
                                        .show_ui(ui, |ui| {
                                            for n in [25, 50, 100, 200, 400] {
                                                ui.selectable_value(&mut p.trajectories, n, n.to_string());
                                            }
                                        })
                                        .response
                                        .on_hover_text(match study.flow.analysis {
                                            FlowAnalysis::External { .. } => "About how many particles to follow, from a grid on a plane upstream of the model",
                                            FlowAnalysis::Internal => "About how many particles to follow, from points spread over the inlets",
                                        });
                                    ui.end_row();
                                    if let Some(q) = particles.as_mut().filter(|q| q.key.0 == d.id) {
                                        ui.label("");
                                        ui.horizontal(|ui| {
                                            if ui
                                                .small_button(if q.playing { "⏸" } else { "▶" })
                                                .on_hover_text(if q.playing { "Pause" } else { "Play the particles" })
                                                .clicked()
                                            {
                                                q.playing = !q.playing;
                                            }
                                            let text = match q.note {
                                                Some(note) => egui::RichText::new(note)
                                                    .small()
                                                    .color(egui::Color32::from_rgb(210, 140, 30)),
                                                None => egui::RichText::new(format!("{} paths", q.paths.len()))
                                                    .small()
                                                    .weak(),
                                            };
                                            ui.label(text);
                                        });
                                        ui.end_row();
                                    }
                                }
                                ui.label("Bands");
                                ui.add(egui::Slider::new(&mut p.bands, 4..=64));
                                ui.end_row();
                            });
                    });
            });
        action
    }

    /// A flow study's results read from a `.oxyres`, shown as if just solved: the run's
    /// record, and the flow from its case folder when it is still there.
    pub(crate) fn show_loaded_flow(
        &mut self,
        run: FlowRun,
        case_dir: PathBuf,
        faces: std::collections::BTreeMap<u32, crate::simulation_ui::SimFace>,
        fingerprint: oxyde_format::results::Fingerprint,
        solver: &SolverInfo,
    ) {
        let density = self.simulation.study.flow.fluid.density;
        let fields = oxyde_flow::FlowResults::read_folder(&case_dir, density).ok();
        let frontal = match (&fields, self.simulation.study.flow.analysis) {
            (Some(f), FlowAnalysis::External { velocity }) => frontal_of(f, velocity),
            _ => None,
        };
        self.restore_simulation_scene();
        self.simulation.frame = 0;
        self.simulation.playing = None;
        self.simulation.done = Some(Done {
            id: crate::document::next_revision(),
            revision: self.document_revision(),
            assembly: self.asm.is_some(),
            mesh: TetMesh::default(),
            report: no_mesh(),
            study_type: self.simulation.study.study_type,
            results: None,
            summary: None,
            thermal: None,
            thermal_summary: None,
            topology: None,
            frequency: None,
            mesh_seconds: 0.0,
            solve_seconds: solver.seconds,
            diagonal: self
                .simulation
                .surfaces
                .as_ref()
                .map_or(1.0, |s| s.diagonal),
            mesher: "openfoam",
            faces,
            bodies: Vec::new(),
            fingerprint: Some(fingerprint),
            flow: Some(FlowDone {
                run,
                case_dir,
                fields,
                frontal,
            }),
        });
        self.simulation.error = None;
        self.simulation.show_plot = true;
    }

    /// While a flow's trajectories play: their clock moves on, and the viewport is drawn
    /// again a few dozen times a second.
    pub(crate) fn advance_particles(&mut self, ctx: &egui::Context) {
        let shown = self
            .simulation
            .shown
            .filter(|k| k.plot == Some(PlotChoice::FlowTrajectories))
            .map(|k| k.id);
        let active = self.simulation_active();
        let Some(p) = self.simulation.particles.as_mut() else {
            return;
        };
        if !(p.playing && active && shown == Some(p.key.0) && !p.paths.is_empty()) {
            p.last = None;
            return;
        }
        let now = Instant::now();
        if let Some(last) = p.last {
            // A slow frame does not make the particles jump.
            p.clock += now.duration_since(last).as_secs_f64().min(0.1);
        }
        // What is drawn repeats every spacing of flow time: the clock stays small.
        let period = p.spacing / p.rate;
        if period > 0.0 && period.is_finite() {
            p.clock = p.clock.rem_euclid(period);
        }
        p.last = Some(now);
        ctx.request_repaint_after(Duration::from_millis(30));
    }

    /// Over the viewport, while the trajectories are shown: each particle with the trail of
    /// its recent path, coloured by speed in the legend's bands and fading towards its
    /// tail. Painted over the faint model rather than put in the scene, since they move
    /// every frame.
    pub(crate) fn paint_trajectories(&self, painter: &egui::Painter, output: &ViewportOutput) {
        let Some(p) = self.simulation.particles.as_ref() else {
            return;
        };
        let shown = self
            .simulation
            .shown
            .is_some_and(|k| k.plot == Some(PlotChoice::FlowTrajectories) && k.id == p.key.0);
        if !shown || !(p.spacing > 0.0 && p.trail > 0.0) {
            return;
        }
        let bands = self.prefs.simulation.bands.clamp(2, 64);
        let (lo, hi) = p.range;
        let colour = |speed: f64, alpha: f64| {
            let t = if hi > lo {
                ((speed - lo) / (hi - lo)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let band = ((t * f64::from(bands)) as u32).min(bands - 1);
            let [r, g, b] = oxyde_sim::rainbow((f64::from(band) + 0.5) / f64::from(bands));
            egui::Color32::from_rgba_unmultiplied(r, g, b, (alpha.clamp(0.0, 1.0) * 255.0) as u8)
        };
        let to_screen = |q: [f64; 3]| {
            self.viewport
                .to_screen(Point3::new(q[0], q[1], q[2]), output)
        };
        let seen = output.rect.expand(8.0);
        let time = p.clock * p.rate;
        let mut trails = egui::Mesh::default();
        let mut heads = Vec::new();
        let mut kept: Vec<(egui::Pos2, egui::Color32)> = Vec::new();
        for (j, path) in p.paths.iter().enumerate() {
            let duration = path.duration();
            // Each seed lets its particles go at its own moment, so that they do not move
            // in rows.
            let offset = (j as f64 * GOLDEN).fract() * p.spacing;
            let mut age = (time + offset).rem_euclid(p.spacing);
            while age <= duration {
                let start = age - p.trail;
                let samples = path.between(start, age);
                kept.clear();
                for (k, s) in samples.iter().enumerate() {
                    let at = to_screen(s.position);
                    // A point every few pixels is enough for a smooth trail; the head
                    // always.
                    let near = kept
                        .last()
                        .is_some_and(|(last, _)| (at - *last).length() < 3.0);
                    if near && k + 1 < samples.len() {
                        continue;
                    }
                    let fade = (s.time - start) / p.trail;
                    kept.push((at, colour(s.speed, 0.1 + 0.9 * fade)));
                }
                for pair in kept.windows(2) {
                    if let [a, b] = pair
                        && (seen.contains(a.0) || seen.contains(b.0))
                    {
                        add_segment(&mut trails, *a, *b, 1.6);
                    }
                }
                if let (Some(&(head, _)), Some(s)) = (kept.last(), samples.last())
                    && seen.contains(head)
                {
                    heads.push((head, colour(s.speed, 1.0)));
                }
                age += p.spacing;
            }
        }
        painter.add(egui::Shape::mesh(trails));
        for (at, c) in heads {
            painter.circle_filled(at, 2.2, c);
        }
    }
}

/// The golden ratio's fraction: offsets spread by it never line up.
const GOLDEN: f64 = 0.618_033_988_749_894_9;

/// A flow's trajectories as the viewport animates them, as Autodesk Flow Design did: each
/// seed lets a particle go at a steady rate, each particle drawn with a trail of its recent
/// path. A view of the results: no undo step, no unsaved mark.
pub(crate) struct Particles {
    /// The results (`Done::id`) and the number of paths asked for.
    pub(crate) key: (u64, u32),
    pub(crate) paths: Vec<Trajectory>,
    /// The slowest and fastest speeds along the paths, m/s: the legend's range.
    pub(crate) range: (f64, f64),
    /// Flow time between two particles from the same seed, and along a trail, s.
    spacing: f64,
    trail: f64,
    /// Flow time played in a second.
    rate: f64,
    /// Seconds played, within one spacing.
    clock: f64,
    pub(crate) playing: bool,
    /// When the clock last moved, while it plays.
    last: Option<Instant>,
    /// Why there are no paths, when there are none.
    pub(crate) note: Option<&'static str>,
}

impl Particles {
    /// The paths of `fields`' flow: from a plane upstream of the model in an external flow,
    /// from the inlets in an internal one. The median path plays in eight seconds, and each
    /// seed lets five particles go along it.
    fn follow(fields: &oxyde_flow::FlowResults, analysis: FlowAnalysis, key: (u64, u32)) -> Self {
        let seeding = match analysis {
            FlowAnalysis::External { velocity } => Seeding::Upstream { velocity },
            FlowAnalysis::Internal => Seeding::Inlets,
        };
        let paths = oxyde_flow::trajectory::trajectories(fields, seeding, key.1 as usize);
        let note = paths.is_empty().then_some(match analysis {
            FlowAnalysis::External { .. } => {
                "No trajectories: the surroundings are still, so nothing comes from upstream"
            }
            FlowAnalysis::Internal => "No trajectories: there is no inlet to start from",
        });
        let speeds = || {
            paths
                .iter()
                .flat_map(|p| p.speeds.iter().copied())
                .filter(|s| s.is_finite())
        };
        let lo = speeds().fold(f64::INFINITY, f64::min);
        let hi = speeds().fold(f64::NEG_INFINITY, f64::max);
        let mut durations: Vec<f64> = paths
            .iter()
            .map(Trajectory::duration)
            .filter(|d| *d > 0.0 && d.is_finite())
            .collect();
        durations.sort_by(f64::total_cmp);
        let median = durations.get(durations.len() / 2).copied().unwrap_or(1.0);
        Self {
            key,
            paths,
            range: if lo <= hi { (lo, hi) } else { (0.0, 1.0) },
            spacing: median / 5.0,
            trail: 0.6 * median / 5.0,
            rate: median / 8.0,
            clock: 0.0,
            playing: true,
            last: None,
            note,
        }
    }

    /// The fastest point along the paths (mm) and its speed (m/s).
    fn fastest(&self) -> Option<([f64; 3], f64)> {
        self.paths
            .iter()
            .flat_map(|p| p.points.iter().zip(&p.speeds))
            .filter(|(_, s)| s.is_finite())
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(p, s)| (*p, *s))
    }
}

/// A band `width` points wide from `a` to `b`, its colour going from one end's to the
/// other's.
fn add_segment(
    mesh: &mut egui::Mesh,
    (a, from): (egui::Pos2, egui::Color32),
    (b, to): (egui::Pos2, egui::Color32),
    width: f32,
) {
    let d = b - a;
    let length = d.length();
    if length <= 1e-3 || !length.is_finite() {
        return;
    }
    let n = egui::vec2(-d.y, d.x) * (0.5 * width / length);
    let i = mesh.vertices.len() as u32;
    mesh.colored_vertex(a + n, from);
    mesh.colored_vertex(a - n, from);
    mesh.colored_vertex(b - n, to);
    mesh.colored_vertex(b + n, to);
    mesh.add_triangle(i, i + 1, i + 2);
    mesh.add_triangle(i, i + 2, i + 3);
}

/// What a force's drag coefficient is taken from: the free stream (m/s), the fluid's
/// density (kg/m³) and the model's frontal area (mm²).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Drag {
    velocity: [f64; 3],
    density: f64,
    area: f64,
}

/// A goal's value in the panel; with `drag`, a force's drag coefficient under it.
fn goal_row(ui: &mut egui::Ui, goal: &GoalResult, drag: Option<Drag>) {
    let unit = match goal.kind {
        FlowGoalKind::Force => "N",
        FlowGoalKind::VolumeFlow => "m³/s",
        FlowGoalKind::MassFlow => "kg/s",
        FlowGoalKind::MeanPressure => "Pa",
        FlowGoalKind::MaxTemperature | FlowGoalKind::MeanTemperature => "°C",
        FlowGoalKind::HeatFlow => "W",
    };
    match &goal.value {
        GoalOutcome::Value(v) => {
            let extra = if goal.kind == FlowGoalKind::VolumeFlow {
                format!(" ({})", format_value(v * 60_000.0, "L/min"))
            } else {
                String::new()
            };
            ui.label(format!("{}: {}{extra}", goal.name, format_value(*v, unit)));
        }
        GoalOutcome::Force {
            total,
            pressure,
            viscous,
        } => {
            let length = (total[0] * total[0] + total[1] * total[1] + total[2] * total[2]).sqrt();
            ui.label(format!("{}: {}", goal.name, format_value(length, unit)))
                .on_hover_text(format!(
                    "X {}  Y {}  Z {}\npressure X {}  Y {}  Z {}\nfriction X {}  Y {}  Z {}",
                    format_number(total[0]),
                    format_number(total[1]),
                    format_number(total[2]),
                    format_number(pressure[0]),
                    format_number(pressure[1]),
                    format_number(pressure[2]),
                    format_number(viscous[0]),
                    format_number(viscous[1]),
                    format_number(viscous[2]),
                ));
            ui.label(
                egui::RichText::new(format!(
                    "    X {}  Y {}  Z {} N",
                    format_number(total[0]),
                    format_number(total[1]),
                    format_number(total[2])
                ))
                .small()
                .weak(),
            );
            if let Some(line) = drag.and_then(|d| drag_line(*total, d)) {
                ui.label(egui::RichText::new(format!("    {line}")).small())
                    .on_hover_text(
                        "The drag coefficient, 2 F / (ρ V² A): F the force along the free stream, \
                         V the stream's speed, ρ the fluid's density, A the model's frontal area, \
                         its silhouette seen along the stream",
                    );
            }
        }
        GoalOutcome::Missing(why) => {
            ui.label(
                egui::RichText::new(format!("{}: not measured ({why})", goal.name))
                    .color(egui::Color32::from_rgb(210, 140, 30)),
            );
        }
    }
}

/// A force's drag coefficient and the frontal area it is taken on: `Cd 0.47 (frontal area
/// 12.5 cm²)`.
fn drag_line(force: [f64; 3], d: Drag) -> Option<String> {
    let cd = oxyde_flow::drag::drag_coefficient(force, d.velocity, d.density, d.area)?;
    Some(format!(
        "Cd {} (frontal area {})",
        format_number(cd),
        format_value(d.area / 100.0, "cm²")
    ))
}

/// The fluid and the flow; whether one changed. A cooling study's `heat` takes the
/// preset fluid's thermal properties with it.
fn fluid_rows(
    ui: &mut egui::Ui,
    flow: &mut FlowSettings,
    mut heat: Option<&mut CoolingSettings>,
) -> bool {
    let mut changed = false;
    egui::Grid::new("flow_fluid")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            ui.label("Analysis");
            let internal = matches!(flow.analysis, FlowAnalysis::Internal);
            egui::ComboBox::from_id_salt("flow_analysis")
                .selected_text(if internal { "Internal" } else { "External" })
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(internal, "Internal")
                        .on_hover_text("Through the model: a pipe, a valve, a manifold, its openings closed by lids")
                        .clicked()
                        && !internal
                    {
                        flow.analysis = FlowAnalysis::Internal;
                        changed = true;
                    }
                    if ui
                        .selectable_label(!internal, "External")
                        .on_hover_text("Around the model: a body in a stream of air or water")
                        .clicked()
                        && internal
                    {
                        flow.analysis = FlowAnalysis::External {
                            velocity: [1.0, 0.0, 0.0],
                        };
                        changed = true;
                    }
                });
            ui.end_row();
            if let FlowAnalysis::External { velocity } = &mut flow.analysis {
                ui.label("Free stream");
                ui.horizontal(|ui| {
                    for (v, axis) in velocity.iter_mut().zip(["X ", "Y ", "Z "]) {
                        changed |= ui
                            .add(
                                egui::DragValue::new(v)
                                    .speed(0.05)
                                    .max_decimals(3)
                                    .prefix(axis),
                            )
                            .changed();
                    }
                    ui.label("m/s");
                })
                .response
                .on_hover_text("The fluid's velocity far from the model, in world axes (Y up)");
                ui.end_row();
            }
            ui.label("Fluid");
            let presets = fluid_presets();
            let current = presets
                .iter()
                .find(|(f, _)| *f == flow.fluid)
                .map_or_else(|| "Custom".to_owned(), |(f, _)| f.name.clone());
            egui::ComboBox::from_id_salt("flow_fluid")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (f, thermal) in presets {
                        let hint = format!(
                            "At 20 °C: {} kg/m³, {} mPa·s",
                            format_number(f.density),
                            format_number(f.viscosity * 1000.0)
                        );
                        if ui
                            .selectable_label(flow.fluid == f, &f.name)
                            .on_hover_text(hint)
                            .clicked()
                        {
                            flow.fluid = f;
                            if let Some(h) = heat.as_deref_mut() {
                                h.specific_heat = thermal.specific_heat;
                                h.conductivity = thermal.conductivity;
                                h.expansion = thermal.expansion;
                            }
                            changed = true;
                        }
                    }
                });
            ui.end_row();
            ui.label("Density");
            if ui
                .add(
                    egui::DragValue::new(&mut flow.fluid.density)
                        .range(1e-3..=30_000.0)
                        .speed(0.1)
                        .max_decimals(3)
                        .suffix(" kg/m³"),
                )
                .changed()
            {
                flow.fluid.name = "Custom".to_owned();
                changed = true;
            }
            ui.end_row();
            ui.label("Viscosity");
            // Typed in mPa·s (centipoise), kept in Pa·s.
            let mut mpa_s = flow.fluid.viscosity * 1000.0;
            if ui
                .add(
                    egui::DragValue::new(&mut mpa_s)
                        .range(1e-4..=1e7)
                        .speed(0.001)
                        .max_decimals(5)
                        .suffix(" mPa·s"),
                )
                .on_hover_text("Dynamic viscosity: water 1.0, air 0.018, olive oil about 80")
                .changed()
            {
                flow.fluid.viscosity = mpa_s / 1000.0;
                flow.fluid.name = "Custom".to_owned();
                changed = true;
            }
            ui.end_row();
            ui.label("Flow");
            let laminar = matches!(flow.turbulence, FlowTurbulence::Laminar);
            egui::ComboBox::from_id_salt("flow_turbulence")
                .selected_text(if laminar { "Laminar" } else { "Turbulent" })
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(laminar, "Laminar")
                        .on_hover_text("Slow or viscous flow, Reynolds number below about 2000 in a pipe")
                        .clicked()
                        && !laminar
                    {
                        flow.turbulence = FlowTurbulence::Laminar;
                        changed = true;
                    }
                    if ui
                        .selectable_label(!laminar, "Turbulent")
                        .on_hover_text("Most flows of air and water in practice: the k-ω SST model, wall functions")
                        .clicked()
                        && laminar
                    {
                        flow.turbulence = FlowTurbulence::Turbulent {
                            intensity: 0.05,
                            length: 1.0,
                        };
                        changed = true;
                    }
                });
            ui.end_row();
            if let FlowTurbulence::Turbulent { intensity, length } = &mut flow.turbulence {
                ui.label("Intensity");
                let mut percent = *intensity * 100.0;
                if ui
                    .add(
                        egui::DragValue::new(&mut percent)
                            .range(0.1..=50.0)
                            .speed(0.1)
                            .suffix(" %"),
                    )
                    .on_hover_text("The turbulence of the fluid coming in: 1 % calm, 5 % usual, 10 % rough")
                    .changed()
                {
                    *intensity = percent / 100.0;
                    changed = true;
                }
                ui.end_row();
                ui.label("Length");
                changed |= ui
                    .add(
                        egui::DragValue::new(length)
                            .range(1e-3..=1e5)
                            .speed(0.05)
                            .suffix(" mm"),
                    )
                    .on_hover_text("The size of the eddies coming in: about 7 % of an inlet's diameter")
                    .changed();
                ui.end_row();
            }
        });
    changed
}

/// A cooling study's heat: the fluid's temperature coming in, gravity, and how it carries
/// heat; whether one changed.
fn heat_rows(ui: &mut egui::Ui, heat: &mut CoolingSettings) -> bool {
    let mut changed = false;
    egui::Grid::new("cooling_heat")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            ui.label("Fluid in at");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut heat.temperature)
                        .range(-100.0..=1000.0)
                        .speed(0.5)
                        .max_decimals(2)
                        .suffix(" °C"),
                )
                .on_hover_text(
                    "The temperature of the fluid coming in, of the free stream and of everything at the start",
                )
                .changed();
            ui.end_row();
            ui.label("Gravity");
            let mut on = heat.gravity.is_some();
            if ui
                .checkbox(&mut on, "Warm fluid rises")
                .on_hover_text(
                    "Natural convection: gravity along −Y (the model's down), so warmer, lighter fluid rises",
                )
                .changed()
            {
                heat.gravity = on.then_some([0.0, -9.81, 0.0]);
                changed = true;
            }
            ui.end_row();
            ui.label("Specific heat");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut heat.specific_heat)
                        .range(1.0..=100_000.0)
                        .speed(1.0)
                        .max_decimals(1)
                        .suffix(" J/(kg·K)"),
                )
                .changed();
            ui.end_row();
            ui.label("Conductivity");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut heat.conductivity)
                        .range(1e-4..=1000.0)
                        .speed(0.001)
                        .max_decimals(4)
                        .suffix(" W/(m·K)"),
                )
                .on_hover_text("The fluid's: water 0.6, air 0.026")
                .changed();
            ui.end_row();
            ui.label("Expansion");
            let mut per_mille = heat.expansion * 1000.0;
            if ui
                .add(
                    egui::DragValue::new(&mut per_mille)
                        .range(0.0..=100.0)
                        .speed(0.01)
                        .max_decimals(4)
                        .suffix(" ×10⁻³/K"),
                )
                .on_hover_text("How much the fluid swells as it warms: water 0.21, air 3.4 (1/T)")
                .changed()
            {
                heat.expansion = per_mille / 1000.0;
                changed = true;
            }
            ui.end_row();
        });
    changed
}

/// The mesh level and the solver's settings; whether one changed.
fn solver_rows(ui: &mut egui::Ui, flow: &mut FlowSettings) -> bool {
    let mut changed = false;
    egui::Grid::new("flow_solver")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            ui.label("Mesh level");
            changed |= ui
                .add(egui::Slider::new(
                    &mut flow.mesh_level,
                    oxyde_flow::setup::LEVELS,
                ))
                .on_hover_text(
                    "1 coarse to 7 fine, as SolidWorks' level of initial mesh: each level makes \
                     the cells about a third smaller",
                )
                .changed();
            ui.end_row();
            ui.label("Iterations");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut flow.iterations)
                        .range(10..=100_000)
                        .speed(10.0),
                )
                .on_hover_text("The most iterations; the run stops earlier once converged")
                .changed();
            ui.end_row();
            ui.label("Converged at");
            egui::ComboBox::from_id_salt("flow_residual")
                .selected_text(format_number(flow.residual))
                .show_ui(ui, |ui| {
                    for r in [1e-3, 1e-4, 1e-5, 1e-6] {
                        changed |= ui
                            .selectable_value(&mut flow.residual, r, format_number(r))
                            .changed();
                    }
                })
                .response
                .on_hover_text(
                    "The residual every equation must fall under: smaller is more exact and slower",
                );
            ui.end_row();
        });
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(
                "OpenFOAM meshes and solves the fluid; it is installed apart (Preferences › Simulation).",
            )
            .small()
            .weak(),
        );
        ui.hyperlink_to(
            egui::RichText::new("Get OpenFOAM").small(),
            oxyde_flow::install::DOWNLOAD_PAGE,
        );
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation_ui::{SimFace, StudyItem};
    use oxyde_model::{FaceName, FeatureId};
    use oxyde_sim::SurfaceMesh;

    fn face(k: u32) -> SimFace {
        SimFace {
            occurrence: None,
            face: FaceName::end_cap(FeatureId::from_raw(k + 1)),
        }
    }

    /// A 10 mm cubic cavity round the origin: the faces of the solid around it, so their
    /// normals point into it. Label k is side k: −x, +x, −y, +y, −z, +z.
    fn cavity() -> Surfaces {
        let mut positions = Vec::new();
        for i in 0..8u32 {
            let c = |bit: u32| if i & bit == 0 { -5.0 } else { 5.0 };
            positions.push([c(1), c(2), c(4)]);
        }
        let mut triangles = Vec::new();
        let mut labels = Vec::new();
        for (label, [a, b, c, d]) in [
            [0, 2, 6, 4],
            [1, 5, 7, 3],
            [0, 4, 5, 1],
            [2, 3, 7, 6],
            [0, 1, 3, 2],
            [4, 6, 7, 5],
        ]
        .into_iter()
        .enumerate()
        {
            triangles.extend([[a, b, c], [a, c, d]]);
            labels.extend([label as u32, label as u32]);
        }
        Surfaces::of_solids(
            vec![SurfaceMesh {
                positions,
                triangles,
                labels,
            }],
            (0..6).map(face).collect(),
        )
    }

    fn item(name: &str, kind: ItemKind, side: u32) -> StudyItem {
        StudyItem {
            name: name.to_owned(),
            kind,
            faces: vec![face(side)],
        }
    }

    #[test]
    fn an_internal_flow_starts_inside_its_cavity() {
        let mut study = Study::of_type(StudyType::Flow, "Flow 1");
        study.items = vec![
            item(
                "Inlet Velocity-1",
                ItemKind::InletVelocity { speed: 1.0 },
                0,
            ),
            item(
                "Static Pressure-1",
                ItemKind::StaticPressure { pascals: 0.0 },
                1,
            ),
            item("Force Goal-1", ItemKind::Goal(FlowGoalKind::Force), 2),
        ];
        let flow = flow_study(&study, &cavity(), 1_000_000).unwrap();
        assert_eq!(flow.conditions.len(), 2);
        assert_eq!(flow.conditions[0].labels, [0]);
        assert_eq!(flow.goals[0].name, "Force Goal-1");
        assert_eq!(flow.goals[0].labels, [2]);
        let oxyde_flow::FlowType::Internal { point } = flow.flow_type else {
            panic!("{:?}", flow.flow_type);
        };
        assert!(point.iter().all(|x| x.abs() < 5.0), "{point:?}");
        // In from the inlet (−x), towards +x.
        assert!(point[0] > -5.0 && point[0] < 0.0, "{point:?}");
        assert!(flow.domain.contains(point));
    }

    #[test]
    fn what_a_flow_lacks_is_said() {
        let mut study = Study::of_type(StudyType::Flow, "Flow 1");
        // No opening: nowhere to start from.
        let why = flow_study(&study, &cavity(), 1_000_000).unwrap_err();
        assert!(why.contains("lids"), "{why}");
        // An inlet but no static pressure: the pressure has no level.
        study.items = vec![item(
            "Inlet Velocity-1",
            ItemKind::InletVelocity { speed: 1.0 },
            0,
        )];
        let why = flow_study(&study, &cavity(), 1_000_000).unwrap_err();
        assert!(why.starts_with("An internal flow needs"), "{why}");
        // A face the model no longer has.
        study.items.push(StudyItem {
            name: "Static Pressure-1".to_owned(),
            kind: ItemKind::StaticPressure { pascals: 0.0 },
            faces: vec![face(40)],
        });
        let why = flow_study(&study, &cavity(), 1_000_000).unwrap_err();
        assert!(why.contains("Static Pressure-1 is on no face"), "{why}");
    }

    #[test]
    fn an_external_flow_has_its_stream_and_domain() {
        let mut study = Study::of_type(StudyType::Flow, "Flow 1");
        study.flow.analysis = FlowAnalysis::External {
            velocity: [0.0, 0.0, 4.0],
        };
        study.items = vec![item("Force Goal-1", ItemKind::Goal(FlowGoalKind::Force), 4)];
        let flow = flow_study(&study, &cavity(), 1_000_000).unwrap();
        assert_eq!(
            flow.flow_type,
            oxyde_flow::FlowType::External {
                velocity: [0.0, 0.0, 4.0]
            }
        );
        // Five sizes downstream (+z), two upstream.
        assert_eq!(flow.domain.max[2], 5.0 + 50.0);
        assert_eq!(flow.domain.min[2], -5.0 - 20.0);
        assert!(flow.conditions.is_empty());
    }

    #[test]
    fn a_cooling_study_takes_the_bodies_heat() {
        let mut study = Study::of_type(StudyType::Cooling, "Cooling 1");
        study.items = vec![
            item(
                "Inlet Velocity-1",
                ItemKind::InletVelocity { speed: 1.0 },
                0,
            ),
            item(
                "Static Pressure-1",
                ItemKind::StaticPressure { pascals: 0.0 },
                1,
            ),
            item("Internal Heat-1", ItemKind::InternalHeat { watts: 3.0 }, 2),
            item(
                "Max Temperature Goal-1",
                ItemKind::Goal(FlowGoalKind::MaxTemperature),
                2,
            ),
        ];
        // A body without a conductivity is named.
        let why = flow_study(&study, &cavity(), 1_000_000).unwrap_err();
        assert!(
            why.contains("Body 0") && why.contains("thermal conductivity"),
            "{why}"
        );
        study.thermal_overrides.insert(
            "test:0".to_owned(),
            oxyde_model::Thermal::new(200.0, 900.0, 2.3e-5),
        );
        let flow = flow_study(&study, &cavity(), 1_000_000).unwrap();
        assert_eq!(flow.goals[0].kind, oxyde_flow::GoalKind::MaxTemperature);
        let heat = flow.heat.unwrap();
        assert_eq!(heat.solids[0].conductivity, 200.0);
        assert_eq!(heat.temperature, 20.0);
        assert_eq!(heat.loads.len(), 1);
        assert!(matches!(
            heat.loads[0].kind,
            oxyde_flow::HeatLoadKind::Internal { watts } if watts == 3.0
        ));
        // Nothing heating it is said.
        study.items.retain(|i| !i.kind.is_thermal());
        let why = flow_study(&study, &cavity(), 1_000_000).unwrap_err();
        assert!(why.contains("Nothing heats"), "{why}");
    }

    #[test]
    fn still_air_reaches_further_above() {
        let mut study = Study::of_type(StudyType::Cooling, "Cooling 1");
        study.flow.analysis = FlowAnalysis::External { velocity: [0.0; 3] };
        study.cooling.gravity = Some([0.0, -9.81, 0.0]);
        study.items = vec![item(
            "Internal Heat-1",
            ItemKind::InternalHeat { watts: 1.0 },
            0,
        )];
        study.thermal_overrides.insert(
            "test:0".to_owned(),
            oxyde_model::Thermal::new(400.0, 385.0, 1.7e-5),
        );
        let flow = flow_study(&study, &cavity(), 1_000_000).unwrap();
        // The 10 mm cube: twice its size below, five times above, for the plume.
        assert_eq!(flow.domain.min[1], -5.0 - 20.0);
        assert_eq!(flow.domain.max[1], 5.0 + 50.0);
        assert_eq!(flow.domain.max[0], 5.0 + 20.0);
    }

    #[test]
    fn residuals_line_up_by_field() {
        let history = vec![
            vec![("Ux".to_owned(), 1.0), ("p".to_owned(), 1.0)],
            vec![
                ("Ux".to_owned(), 0.1),
                ("k".to_owned(), 0.5),
                ("p".to_owned(), 0.2),
            ],
        ];
        let (fields, rows) = residual_table(&history);
        assert_eq!(fields, ["Ux", "p", "k"]);
        assert_eq!(rows[1], [0.1, 0.2, 0.5]);
        assert!(rows[0][2].is_nan());
    }

    /// A run whose residuals levelled off is not told that more iterations may help.
    #[test]
    fn a_run_that_stopped_falling_is_not_told_to_run_longer() {
        let falling = |i: u32| 0.1 * 0.97f64.powf(f64::from(i));
        let mut run = FlowRun {
            iterations: 500,
            residual_fields: vec!["Ux".to_owned()],
            residuals: (0..500).map(|i| vec![falling(i)]).collect(),
            ..FlowRun::default()
        };
        assert!(not_converged(&run).starts_with("more iterations"));
        // Level from the 120th on, swinging by a tenth.
        run.residuals = (0..500)
            .map(|i| vec![falling(i.min(120)) * (1.0 + 0.1 * f64::from(i % 7) / 7.0)])
            .collect();
        let said = not_converged(&run);
        assert!(
            said.starts_with("its residuals stopped falling after about"),
            "{said}"
        );
        assert!(!said.contains("more iterations"), "{said}");
    }

    #[test]
    fn cases_sit_beside_their_study() {
        assert_eq!(
            case_path(Path::new("/models/valve.oxysim"), "Flow 1"),
            Path::new("/models/valve-Flow 1.openfoam")
        );
        assert_eq!(safe("a/b:c"), "a_b_c");
        assert_eq!(max_cells(8.0), 4_000_000);
        assert_eq!(max_cells(f64::NAN), max_cells(8.0));
    }

    #[test]
    fn a_case_is_copied_without_its_processors() {
        let root = std::env::temp_dir().join(format!("oxyde-flow-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let from = root.join("from");
        std::fs::create_dir_all(from.join("constant/polyMesh")).unwrap();
        std::fs::create_dir_all(from.join("processor0")).unwrap();
        std::fs::write(from.join("case.foam"), "").unwrap();
        std::fs::write(from.join("constant/polyMesh/points"), "1(0 0 0)").unwrap();
        let to = root.join("to");
        copy_case(&from, &to).unwrap();
        assert!(to.join("constant/polyMesh/points").is_file());
        assert!(!to.join("processor0").exists());
        // Again: the earlier copy is replaced.
        copy_case(&from, &to).unwrap();
        // Something else in the way is not.
        let other = root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("notes.txt"), "mine").unwrap();
        assert!(copy_case(&from, &other).is_err());
        assert!(other.join("notes.txt").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn goals_and_hints_read_well() {
        let study = Study::of_type(StudyType::Flow, "Flow 1");
        assert!(setup_hint(&study).contains("lids"));
        let cooling = Study::of_type(StudyType::Cooling, "Cooling 1");
        assert!(setup_hint(&cooling).contains("Internal Heat"));
        assert!(goal_hint(FlowGoalKind::Force).contains("force"));
        assert_eq!(sentence("the fluid leaks"), "The fluid leaks");
    }

    #[test]
    fn a_force_reads_with_its_drag_coefficient() {
        // 1 N along a 10 m/s stream of 1.2 kg/m³ on 100 cm²: 2 / (1.2 · 100 · 0.01).
        let drag = Drag {
            velocity: [10.0, 0.0, 0.0],
            density: 1.2,
            area: 1e4,
        };
        assert_eq!(
            drag_line([1.0, 0.3, 0.0], drag).as_deref(),
            Some("Cd 1.667 (frontal area 100 cm²)")
        );
        let still = Drag {
            velocity: [0.0; 3],
            ..drag
        };
        assert_eq!(drag_line([1.0, 0.0, 0.0], still), None);
    }

    #[test]
    fn flow_and_cooling_studies_have_trajectories() {
        let trajectories = PlotChoice::FlowTrajectories;
        assert!(trajectories.is_flow() && !trajectories.is_cut() && !trajectories.is_cooling());
        assert!(trajectories.fits(StudyType::Flow) && trajectories.fits(StudyType::Cooling));
        assert!(!trajectories.fits(StudyType::Static));
        assert_eq!(trajectories.unit(), "m/s");
    }
}
