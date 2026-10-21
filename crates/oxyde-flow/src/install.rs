//! Finding OpenFOAM, and starting its programs in its environment.
//!
//! Only OpenCFD's line (openfoam.com, versions named like `v2606`) is supported: the
//! Foundation's line names its solvers differently. An installation is a project folder:
//!
//! - **Linux**: OpenCFD's packages put it in `/usr/lib/openfoam/openfoam2606`; builds of
//!   one's own in `~/openfoam/OpenFOAM-v2606` or `~/OpenFOAM/OpenFOAM-v2606`.
//! - **macOS**: openfoam-app, `/Applications/OpenFOAM-v2606.app/Contents/Resources`.
//! - **Windows**: OpenCFD's MinGW build, `C:\Program Files\ESI-OpenCFD\OpenFOAM\v2606`
//!   (or under `%APPDATA%` for a per-user install), with `setEnvVariables-v2606.bat`.
//!
//! The environment is read once, when the installation is opened: on Linux and macOS by
//! OpenCFD's launcher (`etc/openfoam -c`, which sources `etc/bashrc`; openfoam-app's also
//! mounts its disk image), or by sourcing `etc/bashrc` where there is no launcher; on
//! Windows by calling the batch file and listing the variables it set. Programs then start
//! directly with that environment, so that stopping one is stopping its process.
//!
//! The variable `OXYDE_OPENFOAM_DIR` names an installation to use ahead of the usual places.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The environment variable naming an installation to use ahead of the usual places.
pub const DIR_VARIABLE: &str = "OXYDE_OPENFOAM_DIR";

/// Where OpenCFD's Windows installers are found, for messages.
pub const DOWNLOAD_PAGE: &str = "https://www.openfoam.com/download";

/// What the environment listing starts with, so that anything a setup script prints first
/// is skipped.
const MARKER: &str = "OXYDE_ENVIRONMENT_FOLLOWS";

/// An OpenFOAM installation, with the environment its programs run in.
#[derive(Debug, Clone)]
pub struct Installation {
    root: PathBuf,
    version: String,
    env: Vec<(OsString, OsString)>,
}

impl Installation {
    /// The installation `OXYDE_OPENFOAM_DIR` names, else the newest one of the usual places
    /// whose environment loads.
    pub fn find() -> Result<Self, InstallError> {
        if let Some(dir) = std::env::var_os(DIR_VARIABLE) {
            return Self::open(Path::new(&dir));
        }
        let mut last = InstallError::NotFound;
        for root in candidates() {
            match Self::open(&root) {
                Ok(found) => return Ok(found),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// The installation in `root`, its environment read.
    pub fn open(root: &Path) -> Result<Self, InstallError> {
        let env = read_environment(root)?;
        let get = |name: &str| {
            env.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        if get("WM_PROJECT_DIR").is_none() {
            return Err(InstallError::Environment {
                root: root.to_path_buf(),
                detail: "its setup did not set WM_PROJECT_DIR".to_string(),
            });
        }
        let version = get("WM_PROJECT_VERSION").unwrap_or_else(|| {
            root.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        Ok(Self {
            root: root.to_path_buf(),
            version,
            env,
        })
    }

    /// The project folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The version, such as `v2606`.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// A variable of OpenFOAM's environment.
    pub fn var(&self, name: &str) -> Option<&OsStr> {
        self.env
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_os_str())
    }

    /// The path of `program` (`blockMesh`, `simpleFoam`, `mpirun`…) on OpenFOAM's `PATH`.
    pub fn program(&self, program: &str) -> Result<PathBuf, InstallError> {
        let missing = || InstallError::ProgramMissing {
            program: program.to_string(),
        };
        let path = self.var("PATH").ok_or_else(missing)?;
        find_on_path(path, program).ok_or_else(missing)
    }

    /// A command starting `program` in OpenFOAM's environment.
    pub fn command(&self, program: &str) -> Result<Command, InstallError> {
        let mut command = Command::new(self.program(program)?);
        command.env_clear();
        command.envs(self.env.iter().map(|(k, v)| (k, v)));
        Ok(command)
    }
}

/// The program `name` in the folders of a `PATH` value.
fn find_on_path(path: &OsStr, name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::split_paths(path)
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.is_file())
}

/// Folders that may hold an installation, newest version first within each place.
pub fn candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut places: Vec<(PathBuf, &str, &str)> = Vec::new();
    if cfg!(windows) {
        for base in [
            std::env::var_os("ProgramFiles").map(PathBuf::from),
            std::env::var_os("APPDATA").map(PathBuf::from),
        ]
        .into_iter()
        .flatten()
        {
            places.push((base.join("ESI-OpenCFD").join("OpenFOAM"), "v", ""));
        }
    } else {
        if cfg!(target_os = "macos") {
            places.push((PathBuf::from("/Applications"), "OpenFOAM-v", ".app"));
            if let Some(home) = &home {
                places.push((home.join("Applications"), "OpenFOAM-v", ".app"));
            }
        }
        places.push((PathBuf::from("/usr/lib/openfoam"), "openfoam", ""));
        if let Some(home) = &home {
            places.push((home.join("openfoam"), "OpenFOAM-v", ""));
            places.push((home.join("OpenFOAM"), "OpenFOAM-v", ""));
        }
    }
    let mut found = Vec::new();
    for (dir, prefix, suffix) in places {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut here: Vec<(u32, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let version = version_number(&name, prefix, suffix)?;
                let mut path = e.path();
                if suffix == ".app" {
                    path = path.join("Contents").join("Resources");
                }
                path.is_dir().then_some((version, path))
            })
            .collect();
        here.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
        found.extend(here.into_iter().map(|(_, p)| p));
    }
    found
}

/// The version number in a folder name such as `openfoam2606`, `OpenFOAM-v2606.app` or
/// `v2606`; `None` for names of another shape.
fn version_number(name: &str, prefix: &str, suffix: &str) -> Option<u32> {
    let digits = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The environment OpenFOAM's setup leaves.
fn read_environment(root: &Path) -> Result<Vec<(OsString, OsString)>, InstallError> {
    let failed = |detail: String| InstallError::Environment {
        root: root.to_path_buf(),
        detail,
    };
    let mut command = setup_command(root)?;
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| failed(format!("its setup could not be started: {e}")))?;
    let env = parse_environment(&output.stdout);
    if env.is_empty() {
        return Err(failed(format!(
            "its setup listed no environment (exit status {})",
            output.status
        )));
    }
    Ok(env)
}

#[cfg(not(windows))]
fn setup_command(root: &Path) -> Result<Command, InstallError> {
    let script = format!("printf '%s\\0' {MARKER}; env -0");
    let launcher = root.join("etc").join("openfoam");
    if launcher.is_file() {
        let mut command = Command::new(launcher);
        command.arg("-c").arg(script);
        return Ok(command);
    }
    if root.join("etc").join("bashrc").is_file() {
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(format!(
                "source \"$1/etc/bashrc\" >/dev/null 2>&1; {script}"
            ))
            .arg("bash")
            .arg(root);
        return Ok(command);
    }
    Err(InstallError::NotAnInstallation {
        root: root.to_path_buf(),
    })
}

#[cfg(windows)]
fn setup_command(root: &Path) -> Result<Command, InstallError> {
    use std::os::windows::process::CommandExt;
    // OpenCFD's MinGW build sets its environment up with `setEnvVariables-v<version>.bat`.
    let batch = std::fs::read_dir(root)
        .ok()
        .and_then(|entries| {
            entries.flatten().map(|e| e.path()).find(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .is_some_and(|n| n.starts_with("setenvvariables") && n.ends_with(".bat"))
            })
        })
        .ok_or_else(|| InstallError::NotAnInstallation {
            root: root.to_path_buf(),
        })?;
    // `/U` makes `set` write UTF-16, so paths outside ASCII survive.
    let mut command = Command::new("cmd");
    command.raw_arg(format!(
        "/D /U /V:ON /C \"call \"{}\" >nul 2>&1 & echo {MARKER}& set\"",
        batch.display()
    ));
    Ok(command)
}

/// The variables of an environment listing: `NAME=value` entries after the marker,
/// separated by NUL bytes (`env -0`) or, on Windows, UTF-16 lines (`cmd /U` and `set`).
fn parse_environment(output: &[u8]) -> Vec<(OsString, OsString)> {
    let entries: Vec<OsString> = if cfg!(windows) {
        let units: Vec<u16> = output
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .collect();
        String::from_utf16_lossy(&units)
            .lines()
            .map(|l| OsString::from(l.trim_end_matches('\r')))
            .collect()
    } else {
        output
            .split(|&b| b == 0)
            .map(|b| os_string_from_bytes(b.to_vec()))
            .collect()
    };
    let mut env = Vec::new();
    let mut started = false;
    for entry in entries {
        let text = entry.to_string_lossy();
        if !started {
            started = text.trim() == MARKER;
            continue;
        }
        // On Windows `set` lists only `NAME=value` lines, and a variable's name may not
        // contain `=` (apart from the hidden `=C:` ones `set` does not list).
        let Some((name, value)) = split_entry(&entry) else {
            continue;
        };
        if !name.is_empty() {
            env.push((name, value));
        }
    }
    env
}

#[cfg(unix)]
fn os_string_from_bytes(bytes: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes)
}

#[cfg(not(unix))]
fn os_string_from_bytes(bytes: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// `NAME=value` split at the first `=`.
fn split_entry(entry: &OsStr) -> Option<(OsString, OsString)> {
    let text = entry.to_str()?;
    let (name, value) = text.split_once('=')?;
    Some((OsString::from(name), OsString::from(value)))
}

/// Why OpenFOAM could not be used.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallError {
    /// No installation in the usual places.
    NotFound,
    /// The folder is not an OpenFOAM installation of OpenCFD's line.
    NotAnInstallation { root: PathBuf },
    /// The installation's environment could not be set up.
    Environment { root: PathBuf, detail: String },
    /// A program is not in the installation.
    ProgramMissing { program: String },
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstallError::NotFound => write!(
                f,
                "OpenFOAM is not installed: install OpenFOAM v2606 from openfoam.com ({DOWNLOAD_PAGE}), \
                 or set its folder in Preferences"
            ),
            InstallError::NotAnInstallation { root } => write!(
                f,
                "{} is not an OpenFOAM installation (openfoam.com's v2606 or later)",
                root.display()
            ),
            InstallError::Environment { root, detail } => write!(
                f,
                "OpenFOAM in {} could not be set up: {detail}",
                root.display()
            ),
            InstallError::ProgramMissing { program } => {
                write!(f, "OpenFOAM's {program} was not found in the installation")
            }
        }
    }
}

impl std::error::Error for InstallError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_numbers_come_from_folder_names() {
        assert_eq!(version_number("openfoam2606", "openfoam", ""), Some(2606));
        assert_eq!(
            version_number("OpenFOAM-v2512.app", "OpenFOAM-v", ".app"),
            Some(2512)
        );
        assert_eq!(version_number("v2606", "v", ""), Some(2606));
        assert_eq!(version_number("openfoam", "openfoam", ""), None);
        assert_eq!(version_number("OpenFOAM-13", "OpenFOAM-v", ""), None);
        assert_eq!(version_number("v2606-old", "v", ""), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_listing_starts_after_the_marker() {
        let mut out = b"Welcome to OpenFOAM\nsome warning\0".to_vec();
        out.extend_from_slice(MARKER.as_bytes());
        out.push(0);
        out.extend_from_slice(b"PATH=/a:/b\0WM_PROJECT_DIR=/x\0FUNC=() { echo\n}\0");
        let env = parse_environment(&out);
        assert_eq!(
            env,
            vec![
                ("PATH".into(), "/a:/b".into()),
                ("WM_PROJECT_DIR".into(), "/x".into()),
                ("FUNC".into(), "() { echo\n}".into()),
            ]
        );
    }

    /// A fake installation: an `etc/bashrc` that puts a `bin` folder on `PATH`, holding a
    /// `blockMesh` that reports where it ran.
    #[cfg(unix)]
    #[test]
    fn a_bashrc_installation_runs_its_programs() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("oxyde-flow-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(
            root.join("etc/bashrc"),
            "echo noise\nexport WM_PROJECT_DIR=\"$1\"\nexport WM_PROJECT_VERSION=v9999\n\
             export PATH=\"$WM_PROJECT_DIR/bin:$PATH\"\n",
        )
        .unwrap();
        let program = root.join("bin/blockMesh");
        std::fs::write(
            &program,
            "#!/bin/sh\necho \"meshed in $(pwd) with $WM_PROJECT_VERSION\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

        // `etc/bashrc` gets the root as `$1` from the command, as OpenCFD's does not need.
        let found = Installation::open(&root).unwrap();
        assert_eq!(found.version(), "v9999");
        assert_eq!(found.program("blockMesh").unwrap(), program);
        assert!(matches!(
            found.program("simpleFoam"),
            Err(InstallError::ProgramMissing { .. })
        ));
        let out = found
            .command("blockMesh")
            .unwrap()
            .current_dir(root.join("etc"))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("/etc with v9999"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_without_openfoam_is_refused() {
        let root = std::env::temp_dir().join(format!("oxyde-flow-empty-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(matches!(
            Installation::open(&root),
            Err(InstallError::NotAnInstallation { .. })
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}
