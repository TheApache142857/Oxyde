//! Reading OpenFOAM's logs as they are written: the solver's iterations and residuals,
//! convergence, fatal errors, and `checkMesh`'s summary.

use std::collections::VecDeque;

/// How many of the last lines are kept, for error messages.
const TAIL: usize = 25;

/// One iteration of a steady solver.
#[derive(Debug, Clone, PartialEq)]
pub struct Iteration {
    /// OpenFOAM's time, which counts the iterations of a steady solver.
    pub time: f64,
    /// The initial residual of each equation solved, by field (`Ux`, `p`, `k`…), in the
    /// order they were solved; the first solve of a field when it is solved more than once.
    pub residuals: Vec<(String, f64)>,
}

impl Iteration {
    /// The largest residual of the iteration.
    pub fn largest(&self) -> f64 {
        self.residuals.iter().map(|(_, r)| *r).fold(0.0, f64::max)
    }
}

/// What a program's log has said so far.
#[derive(Debug, Clone, Default)]
pub struct LogState {
    /// The solver's iterations.
    pub history: Vec<Iteration>,
    /// The solver met its residual targets.
    pub converged: bool,
    /// The program said `End`.
    pub ended: bool,
    /// A residual stopped being a number, or a floating point exception was caught.
    pub diverged: bool,
    /// The text of a `FOAM FATAL ERROR`.
    pub fatal: Option<String>,
    /// `checkMesh`: the number of cells.
    pub cells: Option<u64>,
    /// `checkMesh`: whether every check passed (`Mesh OK.`), and how many failed if not.
    pub mesh_ok: Option<bool>,
    pub failed_checks: u32,
    tail: VecDeque<String>,
    in_fatal: bool,
}

impl LogState {
    /// Takes one line of the log. Returns whether the iterations or residuals changed.
    pub fn feed(&mut self, line: &str) -> bool {
        if self.tail.len() == TAIL {
            self.tail.pop_front();
        }
        self.tail.push_back(line.to_string());
        let text = line.trim();

        if self.in_fatal {
            if text.starts_with("FOAM exiting") || text.starts_with("FOAM aborting") {
                self.in_fatal = false;
            } else if !text.is_empty()
                && let Some(fatal) = &mut self.fatal
            {
                if !fatal.is_empty() {
                    fatal.push('\n');
                }
                fatal.push_str(text);
            }
            return false;
        }
        if text.contains("FOAM FATAL") {
            self.in_fatal = true;
            self.fatal.get_or_insert_with(String::new);
            return false;
        }
        // Every program's banner announces floating point exception trapping (`trapFpe:
        // Floating point exception trapping enabled`); a trap that fires prints its handler
        // in the stack trace.
        if text.contains("sigFpe::sigHandler") {
            self.diverged = true;
            return false;
        }
        if let Some(rest) = text.strip_prefix("Time = ") {
            // `Time = 12`, or with a unit on other lines of OpenFOAM: `12s`.
            let number: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '-' | '+'))
                .collect();
            if let Ok(time) = number.parse::<f64>() {
                self.history.push(Iteration {
                    time,
                    residuals: Vec::new(),
                });
                return true;
            }
            return false;
        }
        if let Some(solved) = parse_residual(text) {
            let (field, residual) = solved;
            if !residual.is_finite() {
                self.diverged = true;
            }
            if let Some(iteration) = self.history.last_mut()
                && !iteration.residuals.iter().any(|(f, _)| *f == field)
            {
                iteration.residuals.push((field, residual));
                return true;
            }
            return false;
        }
        if text.contains("solution converged in") {
            self.converged = true;
            return true;
        }
        if text == "End" {
            self.ended = true;
            return false;
        }
        if let Some(rest) = text.strip_prefix("cells:") {
            if let Ok(cells) = rest.trim().parse::<u64>() {
                self.cells = Some(cells);
            }
            return false;
        }
        if text == "Mesh OK." {
            self.mesh_ok = Some(true);
        } else if let Some(rest) = text.strip_prefix("Failed ")
            && rest.ends_with("mesh checks.")
        {
            self.mesh_ok = Some(false);
            self.failed_checks = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        }
        false
    }

    /// The iterations so far.
    pub fn iterations(&self) -> u32 {
        u32::try_from(self.history.len()).unwrap_or(u32::MAX)
    }

    /// The last lines read, for a message when a program fails.
    pub fn tail(&self) -> String {
        self.tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// What went wrong, for the user: the fatal error's text, or the last lines.
    pub fn failure(&self) -> String {
        match &self.fatal {
            Some(text) if !text.is_empty() => text.clone(),
            _ => self.tail(),
        }
    }
}

/// `smoothSolver:  Solving for Ux, Initial residual = 0.0012, Final residual = …`: the
/// field and its initial residual.
fn parse_residual(text: &str) -> Option<(String, f64)> {
    let after = text.split_once("Solving for ")?.1;
    let (field, rest) = after.split_once(',')?;
    let value = rest.split_once("Initial residual = ")?.1;
    let number = value.split(',').next()?.trim();
    // OpenFOAM writes a residual that is not a number as `nan` or `-nan`.
    let residual = match number {
        "nan" | "-nan" | "NaN" => f64::NAN,
        n => n.parse::<f64>().ok()?,
    };
    Some((field.trim().to_string(), residual))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOLVER: &str = "\
Starting time loop

Time = 1

smoothSolver:  Solving for Ux, Initial residual = 1, Final residual = 0.0538, No Iterations 1
smoothSolver:  Solving for Uy, Initial residual = 1, Final residual = 0.0302, No Iterations 2
GAMG:  Solving for p, Initial residual = 1, Final residual = 0.0442, No Iterations 4
GAMG:  Solving for p, Initial residual = 0.231, Final residual = 0.0103, No Iterations 3
time step continuity errors : sum local = 1.8e-05, global = 2.1e-19, cumulative = 2.1e-19
ExecutionTime = 0.21 s  ClockTime = 0 s

Time = 2

smoothSolver:  Solving for Ux, Initial residual = 0.0427, Final residual = 0.0024, No Iterations 2
smoothSolver:  Solving for Uy, Initial residual = 0.311, Final residual = 0.0219, No Iterations 2
GAMG:  Solving for p, Initial residual = 0.0866, Final residual = 0.0071, No Iterations 3

SIMPLE solution converged in 2 iterations

End
";

    #[test]
    fn residuals_are_read_iteration_by_iteration() {
        let mut log = LogState::default();
        let changes = SOLVER.lines().filter(|l| log.feed(l)).count();
        assert_eq!(log.iterations(), 2);
        assert_eq!(
            log.history[0].residuals,
            vec![
                ("Ux".to_string(), 1.0),
                ("Uy".to_string(), 1.0),
                ("p".to_string(), 1.0)
            ]
        );
        assert_eq!(log.history[1].time, 2.0);
        assert_eq!(log.history[1].largest(), 0.311);
        assert!(log.converged && log.ended && !log.diverged);
        assert!(log.fatal.is_none());
        // Two times, six first solves, convergence.
        assert_eq!(changes, 9);
    }

    #[test]
    fn a_fatal_error_is_kept_for_the_user() {
        let mut log = LogState::default();
        for line in "\
--> FOAM FATAL ERROR: (openfoam-2606)
Cannot find patchField entry for wall_3

    From void Foam::GeometricField<...>::readFields()
    in file GeometricField.C at line 123.

FOAM exiting
"
        .lines()
        {
            log.feed(line);
        }
        let text = log.failure();
        assert!(
            text.starts_with("Cannot find patchField entry for wall_3"),
            "{text}"
        );
        assert!(text.contains("From void"));
        assert!(!text.contains("FOAM exiting"));
    }

    #[test]
    fn divergence_and_mesh_checks_are_noticed() {
        let mut log = LogState::default();
        log.feed("trapFpe: Floating point exception trapping enabled (FOAM_SIGFPE).");
        log.feed("sigFpe : Enabling floating point exception trapping (FOAM_SIGFPE).");
        assert!(!log.diverged, "the banner is not a divergence");
        log.feed("Time = 7");
        log.feed("GAMG:  Solving for p, Initial residual = nan, Final residual = nan, No Iterations 1000");
        assert!(log.diverged);
        let mut trapped = LogState::default();
        trapped.feed("#1  Foam::sigFpe::sigHandler(int) at ??:?");
        assert!(trapped.diverged);

        let mut check = LogState::default();
        for line in [
            "Mesh stats",
            "    points:           12001",
            "    cells:            10240",
            "Failed 1 mesh checks.",
        ] {
            check.feed(line);
        }
        assert_eq!(check.cells, Some(10240));
        assert_eq!(check.mesh_ok, Some(false));
        assert_eq!(check.failed_checks, 1);

        let mut ok = LogState::default();
        ok.feed("Mesh OK.");
        assert_eq!(ok.mesh_ok, Some(true));
    }
}
