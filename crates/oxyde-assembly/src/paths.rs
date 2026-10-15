//! Component paths: relative to the assembly's folder, with `/` separators, so that an
//! assembly and its parts can be moved together and shared between systems.

use std::path::{Component, Path, PathBuf};

/// `target` relative to the folder `base`, with `/` separators; `None` when there is no
/// relative way (another drive).
pub fn relative(target: &Path, base: &Path) -> Option<String> {
    let target: Vec<Component> = target.components().collect();
    let base: Vec<Component> = base.components().collect();
    // Different roots (drives) cannot be joined by a relative path.
    if target.first() != base.first() {
        return None;
    }
    let common = target.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = Vec::new();
    for c in &base[common..] {
        match c {
            Component::Normal(_) => parts.push("..".to_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    for c in &target[common..] {
        match c {
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// A stored component path made usable: relative ones are joined to the assembly's
/// folder; absolute ones (an assembly not saved yet, or parts on another drive) are kept.
pub fn absolute(stored: &str, folder: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(stored.replace('\\', "/"));
    match folder {
        Some(folder) if path.is_relative() => {
            let mut out = folder.to_path_buf();
            for part in stored.split(['/', '\\']) {
                match part {
                    "" | "." => {}
                    ".." => {
                        out.pop();
                    }
                    name => out.push(name),
                }
            }
            out
        }
        _ => path,
    }
}

/// How a component path is stored for an assembly in `folder`: relative when possible.
pub fn stored(path: &Path, folder: Option<&Path>) -> String {
    folder
        .and_then(|f| relative(path, f))
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute folder on any system.
    fn root() -> PathBuf {
        std::env::temp_dir().join("work")
    }

    #[test]
    fn relative_paths() {
        let base = root().join("robot");
        let rel = |p: PathBuf| relative(&p, &base);
        assert_eq!(rel(base.join("arm.oxyprt")).as_deref(), Some("arm.oxyprt"));
        assert_eq!(
            rel(base.join("parts").join("pin.oxyprt")).as_deref(),
            Some("parts/pin.oxyprt")
        );
        assert_eq!(
            rel(root().join("common").join("bolt.oxyprt")).as_deref(),
            Some("../common/bolt.oxyprt")
        );
    }

    #[test]
    fn absolute_paths() {
        let folder = root().join("robot");
        assert_eq!(
            absolute("../common/bolt.oxyprt", Some(&folder)),
            root().join("common").join("bolt.oxyprt")
        );
        assert_eq!(
            absolute("parts\\pin.oxyprt", Some(&folder)),
            folder.join("parts").join("pin.oxyprt")
        );
        let elsewhere = std::env::temp_dir().join("elsewhere").join("a.oxyprt");
        assert_eq!(
            absolute(&elsewhere.to_string_lossy(), Some(&folder)),
            elsewhere
        );
        let pin = folder.join("parts").join("pin.oxyprt");
        assert_eq!(absolute(&stored(&pin, Some(&folder)), Some(&folder)), pin);
    }
}
