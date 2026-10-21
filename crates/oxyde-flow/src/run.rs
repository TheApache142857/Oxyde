//! Running a written case: the meshing programs, then the solver, one after the other.
//!
//! Each program writes its output to `log.<program>` in the case folder, which is read as
//! it grows for progress (the solver's iterations and residuals) and kept for the user. A
//! cancel request stops the running program; a program that fails stops the run with what
//! its log said.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::case::Case;
use crate::install::{InstallError, Installation};
use crate::log::{Iteration, LogState};

/// How often a running program's log is read and the cancel flag checked.
const POLL: Duration = Duration::from_millis(100);

/// Where a run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// `blockMesh`: the domain cut into cubes.
    Background,
    /// `surfaceFeatureExtract`: the surfaces' sharp edges.
    Features,
    /// `snappyHexMesh`: cells refined, cut and snapped to the surfaces.
    Snapping,
    /// `checkMesh`: the mesh's quality.
    Checking,
    /// `decomposePar`: the mesh split between processes.
    Decomposing,
    /// `potentialFoam`: a first flow without viscosity to start from.
    Initialising,
    /// The solver.
    Solving,
    /// `reconstructPar`: the processes' results put back together.
    Reconstructing,
}

impl Stage {
    /// What the stage does, for a progress window.
    pub fn describe(self) -> &'static str {
        match self {
            Stage::Background => "Filling the domain with cells",
            Stage::Features => "Finding sharp edges",
            Stage::Snapping => "Fitting the mesh to the model",
            Stage::Checking => "Checking the mesh",
            Stage::Decomposing => "Sharing the mesh between processes",
            Stage::Initialising => "Starting the flow",
            Stage::Solving => "Solving",
            Stage::Reconstructing => "Gathering the results",
        }
    }
}

/// A run's progress, reported as it changes.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub stage: Stage,
    /// The solver's iterations so far, and the most it will take.
    pub iteration: u32,
    pub iterations: u32,
    /// The initial residuals of the last iteration, by field.
    pub residuals: Vec<(String, f64)>,
}

/// How a finished run went.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunReport {
    /// The mesh's cells, as `checkMesh` counted them.
    pub cells: Option<u64>,
    /// Whether every mesh check passed, and how many failed if not. A mesh failing some
    /// checks still solves, less accurately.
    pub mesh_ok: Option<bool>,
    pub failed_checks: u32,
    /// The solver's iterations, and whether it met its residual targets before the most
    /// iterations allowed.
    pub iterations: u32,
    pub converged: bool,
    /// The residuals of every iteration, for a plot.
    pub history: Vec<Iteration>,
}

/// Why a run stopped.
#[derive(Debug)]
pub enum RunError {
    /// Stopped on request.
    Cancelled,
    /// OpenFOAM or one of its programs is missing.
    Install(InstallError),
    /// A program could not be started.
    Start { program: String, error: io::Error },
    /// A program failed; `message` is what its log said.
    Failed {
        program: String,
        message: String,
        log: PathBuf,
    },
    /// The solution blew up: residuals that are not numbers, or a floating point exception.
    Diverged { iteration: u32, log: PathBuf },
    /// An internal flow's fluid reached the domain's box: its cavity is open somewhere.
    Leak { faces: usize },
    /// A file of the case could not be read or written.
    Io { path: PathBuf, error: io::Error },
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Cancelled => write!(f, "the run was cancelled"),
            RunError::Install(e) => write!(f, "{e}"),
            RunError::Start { program, error } => {
                write!(f, "OpenFOAM's {program} could not be started: {error}")
            }
            RunError::Failed {
                program, message, ..
            } => write!(f, "OpenFOAM's {program} failed:\n{message}"),
            RunError::Diverged { iteration, .. } => write!(
                f,
                "the solution diverged at iteration {iteration}: try a finer mesh, \
                 or check the conditions"
            ),
            RunError::Leak { faces } => write!(
                f,
                "the fluid leaks out of the model ({faces} faces of the mesh reached the \
                 computational domain): close every opening with a lid, and check that \
                 the inlets and outlets are on the lids' inner faces"
            ),
            RunError::Io { path, error } => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for RunError {}

impl From<InstallError> for RunError {
    fn from(e: InstallError) -> Self {
        RunError::Install(e)
    }
}

/// Runs `case` with `install`: meshes it, solves it, and reports progress through
/// `progress` as it goes. `cancel` stops it.
pub fn run(
    case: &Case,
    install: &Installation,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<RunReport, RunError> {
    let parallel = case.processes > 1;
    let mut report = RunReport::default();
    let mut runner = Runner {
        case,
        install,
        cancel,
        progress,
    };

    runner.program(Stage::Background, "blockMesh", &[], false)?;
    runner.program(Stage::Features, "surfaceFeatureExtract", &[], false)?;
    runner.program(Stage::Snapping, "snappyHexMesh", &["-overwrite"], false)?;
    if case.internal {
        check_closed(&case.dir)?;
    }
    let check = runner.program(Stage::Checking, "checkMesh", &[], false)?;
    report.cells = check.cells;
    report.mesh_ok = check.mesh_ok;
    report.failed_checks = check.failed_checks;

    copy_initial_fields(&case.dir)?;
    copy_file(
        &case.dir.join("system/controlDict.solve"),
        &case.dir.join("system/controlDict"),
    )?;
    if parallel {
        runner.program(Stage::Decomposing, "decomposePar", &["-force"], false)?;
    }
    runner.program(
        Stage::Initialising,
        "potentialFoam",
        &["-writephi"],
        parallel,
    )?;
    let solve = runner.program(Stage::Solving, &case.solver, &[], parallel)?;
    report.iterations = solve.iterations();
    report.converged = solve.converged;
    report.history = solve.history;
    if parallel {
        runner.program(
            Stage::Reconstructing,
            "reconstructPar",
            &["-latestTime"],
            false,
        )?;
    }
    Ok(report)
}

struct Runner<'a> {
    case: &'a Case,
    install: &'a Installation,
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(&Progress),
}

impl Runner<'_> {
    /// Runs one program to its end, following its log.
    fn program(
        &mut self,
        stage: Stage,
        program: &str,
        args: &[&str],
        parallel: bool,
    ) -> Result<LogState, RunError> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(RunError::Cancelled);
        }
        let dir = &self.case.dir;
        let log_path = dir.join(format!("log.{program}"));
        let io_err = |path: &Path, error| RunError::Io {
            path: path.to_path_buf(),
            error,
        };
        let log = File::create(&log_path).map_err(|e| io_err(&log_path, e))?;
        let log_err = log.try_clone().map_err(|e| io_err(&log_path, e))?;

        let mut command = if parallel {
            let mut c = self.install.command(MPI_LAUNCHER)?;
            c.arg(MPI_COUNT)
                .arg(self.case.processes.to_string())
                .arg(self.install.program(program)?)
                .arg("-parallel");
            c
        } else {
            self.install.command(program)?
        };
        command
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err);
        let mut child = command.spawn().map_err(|error| RunError::Start {
            program: program.to_string(),
            error,
        })?;

        let mut reader = File::open(&log_path).map_err(|e| io_err(&log_path, e))?;
        let mut pending: Vec<u8> = Vec::new();
        let mut state = LogState::default();
        let iterations = self.case.iterations;
        let report = |state: &LogState, progress: &mut dyn FnMut(&Progress)| {
            progress(&Progress {
                stage,
                iteration: state.iterations(),
                iterations,
                residuals: state
                    .history
                    .last()
                    .map(|i| i.residuals.clone())
                    .unwrap_or_default(),
            });
        };
        report(&state, &mut *self.progress);

        let status = loop {
            let finished = child.try_wait().map_err(|e| io_err(&log_path, e))?;
            // Read after checking: a program that has exited has written all its log.
            let changed = read_lines(&mut reader, &mut pending, &mut state)
                .map_err(|e| io_err(&log_path, e))?;
            if changed {
                report(&state, &mut *self.progress);
            }
            if let Some(status) = finished {
                break status;
            }
            if self.cancel.load(Ordering::Relaxed) {
                stop(&mut child);
                return Err(RunError::Cancelled);
            }
            thread::sleep(POLL);
        };
        if !pending.is_empty() {
            state.feed(&String::from_utf8_lossy(&pending));
        }
        if state.diverged {
            return Err(RunError::Diverged {
                iteration: state.iterations(),
                log: log_path,
            });
        }
        if !status.success() {
            return Err(RunError::Failed {
                program: program.to_string(),
                message: state.failure(),
                log: log_path,
            });
        }
        Ok(state)
    }
}

/// The MPI launcher and its option for the number of processes: OpenMPI's `mpirun` on
/// Linux and macOS, Microsoft MPI's `mpiexec` on Windows.
#[cfg(not(windows))]
const MPI_LAUNCHER: &str = "mpirun";
#[cfg(not(windows))]
const MPI_COUNT: &str = "-np";
#[cfg(windows)]
const MPI_LAUNCHER: &str = "mpiexec";
#[cfg(windows)]
const MPI_COUNT: &str = "-n";

/// Feeds the log's new complete lines to `state`; returns whether progress changed.
fn read_lines(reader: &mut File, pending: &mut Vec<u8>, state: &mut LogState) -> io::Result<bool> {
    let mut fresh = Vec::new();
    reader.read_to_end(&mut fresh)?;
    pending.extend_from_slice(&fresh);
    let mut changed = false;
    while let Some(end) = pending.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = pending.drain(..=end).collect();
        let text = String::from_utf8_lossy(&line);
        changed |= state.feed(text.trim_end_matches(['\n', '\r']));
    }
    Ok(changed)
}

/// Stops a program and whatever it started (MPI processes).
fn stop(child: &mut Child) {
    #[cfg(windows)]
    {
        // The whole tree: `mpiexec` starts the solver's processes.
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        // A polite signal first, which `mpirun` passes on to the processes it started.
        let _ = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        for _ in 0..30 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(POLL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// An internal flow's mesh must stay inside the model: faces on the domain's box mean the
/// cavity is open somewhere and the fluid filled the box around the model.
fn check_closed(dir: &Path) -> Result<(), RunError> {
    let path = dir.join("constant").join("polyMesh").join("boundary");
    let text = fs::read(&path).map_err(|error| RunError::Io {
        path: path.clone(),
        error,
    })?;
    let faces = domain_faces(&text).unwrap_or(0);
    if faces > 0 {
        return Err(RunError::Leak { faces });
    }
    Ok(())
}

/// The faces of the domain's patch in a `boundary` file's text.
fn domain_faces(text: &[u8]) -> Option<usize> {
    let mut lex = crate::foam::Lexer::new(text);
    crate::foam::header(&mut lex).ok()?;
    let patches = lex
        .list(|l| {
            let name = l.word()?.to_string();
            l.expect(b'{')?;
            let mut faces = 0;
            while !l.eat(b'}') {
                match l.word()? {
                    "nFaces" => {
                        faces = l.label()?;
                        l.expect(b';')?;
                    }
                    _ => l.skip_value()?,
                }
            }
            Ok((name, faces))
        })
        .ok()?;
    patches
        .into_iter()
        .find(|(name, _)| name == crate::case::DOMAIN_PATCH)
        .map(|(_, faces)| faces)
}

/// Copies the starting fields from `0.orig` to `0`, which `snappyHexMesh` may have
/// written mesh data into.
fn copy_initial_fields(dir: &Path) -> Result<(), RunError> {
    let from = dir.join("0.orig");
    let to = dir.join("0");
    fs::create_dir_all(&to).map_err(|error| RunError::Io {
        path: to.clone(),
        error,
    })?;
    let entries = fs::read_dir(&from).map_err(|error| RunError::Io {
        path: from.clone(),
        error,
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            copy_file(&path, &to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn copy_file(from: &Path, to: &Path) -> Result<(), RunError> {
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|error| RunError::Io {
            path: from.to_path_buf(),
            error,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_lines_only_are_read() {
        let path = std::env::temp_dir().join(format!("oxyde-flow-log-{}", std::process::id()));
        fs::write(&path, "Time = 1\nsmoothSolver:  Solving for Ux, Initial residual = 0.5, Final residual = 0.01, No Iterations 1\nTime = ").unwrap();
        let mut reader = File::open(&path).unwrap();
        let mut pending = Vec::new();
        let mut state = LogState::default();
        assert!(read_lines(&mut reader, &mut pending, &mut state).unwrap());
        assert_eq!(state.iterations(), 1);
        assert_eq!(pending, b"Time = ");
        // The rest of the line arrives later.
        use std::io::Write;
        let mut writer = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writer.write_all(b"2\n").unwrap();
        assert!(read_lines(&mut reader, &mut pending, &mut state).unwrap());
        assert_eq!(state.iterations(), 2);
        assert_eq!(state.history[0].residuals, vec![("Ux".to_string(), 0.5)]);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_mesh_reaching_the_domain_is_a_leak() {
        let boundary = b"FoamFile { format ascii; class polyBoundaryMesh; object boundary; }
3
(
    domain { type wall; inGroups 1(wall); nFaces 12; startFace 100; }
    inlet_0 { type patch; nFaces 4; startFace 112; }
    wall_1 { type wall; nFaces 40; startFace 116; }
)";
        assert_eq!(domain_faces(boundary), Some(12));
        let closed = String::from_utf8_lossy(boundary).replace("nFaces 12", "nFaces 0");
        assert_eq!(domain_faces(closed.as_bytes()), Some(0));
        assert!(RunError::Leak { faces: 12 }.to_string().contains("lid"));
    }

    #[test]
    fn stages_say_what_they_do() {
        assert_eq!(Stage::Snapping.describe(), "Fitting the mesh to the model");
    }
}
