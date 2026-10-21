//! The crate dependency rules of `docs/ARCHITECTURE.md` §2, checked against every
//! `crates/*/Cargo.toml`: no crate declares a dependency (normal, dev or build) on a crate the
//! table forbids it, and every crate of the workspace has a row. Keep [`FORBIDDEN`] the same as
//! the table's "must never depend on" column; change them together.
//!
//! The manifests are read by hand (a line-oriented pass over the TOML) rather than with a TOML
//! crate, so this test adds no dependency. It understands what the manifests here use:
//! `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, the same under
//! `[target.'cfg(...)'.…]`, and `[dependencies.<name>]` sub-tables. A dependency renamed with
//! `package = "…"` would be seen under its new name, which no manifest does.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The "must never depend on" column of ARCHITECTURE.md §2, in the table's order. `"*"` means
/// every other crate of the workspace.
const FORBIDDEN: &[(&str, &[&str])] = &[
    ("oxyde-geom", &["*"]),
    ("oxyde-sketch", &["oxyde-kernel", "oxyde-occt"]),
    ("oxyde-kernel", &["oxyde-occt", "oxyde-model"]),
    ("oxyde-occt", &["oxyde-model", "oxyde-sketch", "oxyde-app"]),
    ("oxyde-model", &["oxyde-occt", "oxyde-app"]),
    ("oxyde-assembly", &["oxyde-occt", "oxyde-app"]),
    ("oxyde-format", &["oxyde-occt", "oxyde-app"]),
    ("oxyde-render", &["oxyde-occt", "oxyde-model", "oxyde-app"]),
    ("oxyde-sim", &["*"]),
    (
        "oxyde-sim-gpu",
        &[
            "oxyde-geom",
            "oxyde-sketch",
            "oxyde-kernel",
            "oxyde-occt",
            "oxyde-model",
            "oxyde-assembly",
            "oxyde-format",
            "oxyde-render",
            "oxyde-app",
            "oxyde-cli",
            "oxyde-tests",
        ],
    ),
    ("oxyde-app", &[]),
    ("oxyde-cli", &["oxyde-app"]),
    ("oxyde-mcp", &["oxyde-app"]),
    ("oxyde-tests", &[]),
];

/// One declared dependency on a workspace crate.
#[derive(Debug, PartialEq, Eq)]
struct Edge {
    /// The dependency's crate name.
    to: String,
    /// The table it was declared in: `dependencies`, `dev-dependencies`, `build-dependencies`.
    table: String,
}

/// The crates of the workspace, each with its manifest path and its edges to other
/// workspace crates.
struct Workspace {
    crates: BTreeMap<String, (PathBuf, Vec<Edge>)>,
}

impl Workspace {
    fn read() -> Self {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/oxyde-tests sits in crates/")
            .to_path_buf();
        let mut crates = BTreeMap::new();
        for entry in fs::read_dir(&crates_dir).expect("read crates/") {
            let manifest = entry.expect("directory entry").path().join("Cargo.toml");
            if !manifest.is_file() {
                continue;
            }
            let text = fs::read_to_string(&manifest)
                .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));
            let name = package_name(&text)
                .unwrap_or_else(|| panic!("{} has no [package] name", manifest.display()));
            let edges = workspace_edges(&text);
            crates.insert(name, (manifest, edges));
        }
        assert!(
            !crates.is_empty(),
            "no crate manifests found under {}",
            crates_dir.display()
        );
        Self { crates }
    }
}

/// The `name` of the `[package]` table.
fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if let Some(header) = table_header(line) {
            in_package = header == "package";
        } else if in_package
            && let Some((key, value)) = key_value(line)
            && key == "name"
        {
            return Some(value);
        }
    }
    None
}

/// Every dependency on an `oxyde-*` crate, with the table it was declared in.
fn workspace_edges(manifest: &str) -> Vec<Edge> {
    let mut edges = Vec::new();
    // The dependency table the current lines belong to, if any.
    let mut table: Option<String> = None;
    for line in manifest.lines().map(str::trim) {
        if let Some(header) = table_header(line) {
            table = dependency_table(header);
            // `[dependencies.oxyde-geom]`: the header itself names the dependency.
            if let Some((kind, dep)) = dependency_subtable(header)
                && dep.starts_with("oxyde-")
            {
                edges.push(Edge {
                    to: dep,
                    table: kind,
                });
            }
        } else if let Some(kind) = &table
            && let Some((key, _)) = key_value(line)
            && key.starts_with("oxyde-")
        {
            edges.push(Edge {
                to: key,
                table: kind.clone(),
            });
        }
    }
    edges
}

/// `[a.b.c]` → `a.b.c`.
fn table_header(line: &str) -> Option<&str> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    // `[[bin]]` and the like are arrays of tables, never dependencies.
    (!inner.starts_with('[')).then_some(inner.trim())
}

/// The kind of dependency table a header opens: `dependencies`, `dev-dependencies` or
/// `build-dependencies`, also under `target.'cfg(…)'.`; `None` for any other table or for a
/// `[dependencies.<name>]` sub-table (handled by [`dependency_subtable`]).
fn dependency_table(header: &str) -> Option<String> {
    let last = header.rsplit('.').next()?;
    is_dependency_kind(last).then(|| last.to_owned())
}

/// `dependencies.oxyde-geom` or `target.'cfg(windows)'.dev-dependencies."oxyde-geom"` →
/// `(dev-dependencies, oxyde-geom)`.
fn dependency_subtable(header: &str) -> Option<(String, String)> {
    let (kind, dep) = header.rsplit_once('.')?;
    let kind = kind.rsplit('.').next()?;
    is_dependency_kind(kind).then(|| (kind.to_owned(), unquote(dep)))
}

fn is_dependency_kind(name: &str) -> bool {
    matches!(
        name,
        "dependencies" | "dev-dependencies" | "build-dependencies"
    )
}

/// `key = value` with the key unquoted and the value's surrounding quotes removed; `None` for
/// comments, blank lines and anything else.
fn key_value(line: &str) -> Option<(String, String)> {
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    Some((unquote(key.trim()), unquote(value.trim())))
}

fn unquote(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '\'').to_owned()
}

#[test]
fn every_crate_has_a_row_in_the_dependency_table() {
    let ws = Workspace::read();
    let missing: Vec<&String> = ws
        .crates
        .keys()
        .filter(|name| !FORBIDDEN.iter().any(|(row, _)| row == name))
        .collect();
    assert!(
        missing.is_empty(),
        "crates without a row in ARCHITECTURE.md §2 and FORBIDDEN in this test: {missing:?}. \
         Add the crate to the table in the doc, and the same row here."
    );
    let stale: Vec<&&str> = FORBIDDEN
        .iter()
        .map(|(row, _)| row)
        .filter(|row| !ws.crates.contains_key(**row))
        .collect();
    assert!(
        stale.is_empty(),
        "rows for crates that no longer exist: {stale:?}. Remove them from ARCHITECTURE.md §2 and \
         from FORBIDDEN in this test."
    );
}

#[test]
fn no_crate_depends_on_one_the_architecture_forbids() {
    let ws = Workspace::read();
    let mut violations = Vec::new();
    for (name, forbidden) in FORBIDDEN {
        let Some((manifest, edges)) = ws.crates.get(*name) else {
            continue; // reported by the test above
        };
        let forbids = |dep: &str| {
            forbidden
                .iter()
                .any(|f| (*f == "*" && dep != *name) || *f == dep)
        };
        for edge in edges.iter().filter(|e| forbids(&e.to)) {
            violations.push(format!(
                "{name} -> {} ([{}] in {})",
                edge.to,
                edge.table,
                manifest.display()
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "dependencies that docs/ARCHITECTURE.md §2 forbids ('must never depend on' column):\n  {}\n\
         Either the dependency is wrong, or the layering changed: then update the table in the \
         doc and FORBIDDEN in crates/oxyde-tests/tests/layering.rs together.",
        violations.join("\n  ")
    );
}

#[test]
fn manifest_parsing_sees_every_dependency_table() {
    let manifest = r#"
[package]
name = "oxyde-example"

[[bin]]
name = "example"

[dependencies]
oxyde-geom = { workspace = true }
serde = "1"
# oxyde-occt = { workspace = true }

[dev-dependencies]
"oxyde-model" = { workspace = true }

[target.'cfg(windows)'.build-dependencies]
oxyde-render = "0.1"

[dependencies.oxyde-sketch]
workspace = true
"#;
    assert_eq!(package_name(manifest).as_deref(), Some("oxyde-example"));
    let edges = workspace_edges(manifest);
    let edge = |to: &str, table: &str| Edge {
        to: to.to_owned(),
        table: table.to_owned(),
    };
    assert_eq!(
        edges,
        vec![
            edge("oxyde-geom", "dependencies"),
            edge("oxyde-model", "dev-dependencies"),
            edge("oxyde-render", "build-dependencies"),
            edge("oxyde-sketch", "dependencies"),
        ]
    );
}
