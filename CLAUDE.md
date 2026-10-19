# Working on Oxyde

Oxyde is a parametric, feature-based 3D CAD modeller in the manner of SolidWorks, written in Rust
with a thin C++ shim around the OpenCASCADE (OCCT) geometry kernel. Parts are feature timelines
saved as open `.oxyprt` files, assemblies as `.oxyasm`; both are documented ZIP archives.

The documentation, and what each file is for:

- `README.md` — what the program does today, the crate map.
- `docs/ARCHITECTURE.md` — the design decisions and rules. Code that deviates from it changes it in the same commit.
- `docs/FORMAT.md` — the file formats, a contract with other tools.
- `docs/BUILDING.md` — toolchain, OpenCASCADE, packaging, troubleshooting.
- `CHANGELOG.md` — what changed for users, newest first; the top entry is the current state.

## Start every session here

1. Read the top entry of `CHANGELOG.md` (under *Unreleased*): what the app does now and what was just changed.
2. Read the sections of `docs/ARCHITECTURE.md` the task touches. §2 (crates and dependency rules) always;
   §3 the shim, §4 the `Kernel` trait, §5 sketches and the solver, §6 feature tree and naming,
   §7 units, §8 assemblies, §9 the app and viewport, §10 roadmap.
3. Before adding a pattern, look at how the neighbouring code does it. The tests in
   `crates/oxyde-tests/tests/` are the examples of driving the engine from Rust.

## This machine: constraints, not preferences

- **One cargo command at a time, ever.** A parallel release build crashed the PC. Other agent sessions share
  this machine: check for running `cargo`/`rustc` processes before starting one, and run long commands in the
  background rather than side by side.
- **Debug builds only.** Never `--release` locally (`oxyde-render` is already optimised in debug through
  `profile.dev.package` in `Cargo.toml`).
- **Never launch the application** (`cargo run -p oxyde-app`, `target/debug/oxyde.exe`) from an agent session.
  Behaviour is checked through tests; the owner runs the app.
- **`cargo test --workspace` is the check** and is usable: a few minutes warm, longer cold. A test exiting with
  `0xc0000135` means the OCCT DLLs are not on `PATH` (BUILDING.md §6).
- **Files are CRLF on disk, LF in git** (`core.autocrlf=true`). Git Bash tools hide the `\r`: normalise before any
  scripted edit (`sed`, Python, heredocs) and check that a file you changed has one line ending throughout.
  Inline heredocs over a few KB get mangled; write a script to a file and run it.
- **Format per package, never `cargo fmt --all`:** about 50 files were never formatted, and the whole-workspace
  command rewrites them all. `cargo fmt -p <crate>` and then check the diff touches only what you meant.
- **STEP in the shim runs behind one lock** (OCCT's STEP and XCAF code is not thread-safe), and the shim must be
  compiled with `/EHsc` on MSVC (`crates/oxyde-occt/build.rs` does it). Without it an OCCT exception unwinds
  without destructors, leaks that lock, and the next STEP test hangs forever.
- Building needs Visual Studio 2022 with the C++ workload, Rust 1.85+ (edition 2024) and OpenCASCADE 7.7+
  (8.0 tested) through vcpkg: `OXYDE_OCCT_DIR` or `VCPKG_ROOT`, see `docs/BUILDING.md`.
- Test parts and assemblies to read from a script live on the Desktop in `Oxyde test files`
  (`.oxyprt` is a ZIP; `model.json` inside is the timeline).

## Quality gate, before every commit

This is the only check: there is no hosted CI. GitHub Actions are switched off for the repository because
they cost paid minutes; add no workflow under `.github/`. Run these one after the other, never at the same time:

```
cargo fmt -p <each crate you touched> --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

Clippy: add no warning. The workspace carries about 90 from clippy 1.98 (October 2026, mostly `collapsible_if`,
in every crate but `oxyde-kernel` and `oxyde-tests`); fix the ones in code you touch, leave the rest for a sweep
the owner asks for. The same goes for formatting: if `cargo fmt --check` on a crate lists files you did not
touch, format your files only (`rustfmt <file>`), not the crate.

## Architecture rules, in short (`docs/ARCHITECTURE.md` is the authority)

- **Rust owns the application.** C++ exists only in `crates/oxyde-occt/cpp/`, a thin shim around OCCT. No
  modelling logic, no naming, no file I/O in C++.
- **Everything above `oxyde-kernel` talks to the `Kernel` trait.** Only `oxyde-app` chooses a backend
  (`OcctKernel`), and `oxyde-tests` drives it in tests. The kernel must stay replaceable.
- **No OCCT type crosses the boundary.** Shapes are opaque `ShapeHandle` ids in a C++-side arena; data crosses
  as plain structs. Every shim function catches every C++ exception and returns `Err`; nothing panics.
- **Every kernel construction operation returns its history** (`OpResult { shape, history }`: where each
  result face came from). Persistent face and edge naming (§6) is built on it; an operation without history
  breaks naming for everything after it.
- **The model is data; geometry is a cache.** A part is its feature timeline (`PartDefinition`, exactly what is
  saved and what undo snapshots). B-rep is rebuilt by replaying the timeline and is never written to a file,
  except imported STEP, kept byte for byte.
- **Rebuilds are deterministic.** Same timeline and same kernel version give the same topology and the same
  names: no randomness and no dependence on the iteration order of a `HashMap` anywhere on a rebuild path.
- **Units:** millimetres and radians internally (`f64`); degrees and display units only at the UI and file
  boundary. Tolerances come from one policy in `oxyde-geom`. The world is **Y up**, as in SolidWorks: the Front
  view looks down −Z with X to the right (`oxyde-render`).
- **Crate layering** is the table in ARCHITECTURE.md §2, enforced by `crates/oxyde-tests/tests/layering.rs`.
  A new dependency between crates changes the table and the test's data together.
- **References are persistent names, never indices:** `FaceName`, `EdgeName`, `D1@Sketch1`, relation ids.
  Selection survives rebuilds because of this.

## Conventions already in the code

- SolidWorks is the reference for names and behaviour: features `Sketch1`, `Boss-Extrude1`, `Cut-Extrude1`,
  `Fillet1`; dimensions `D1@Sketch1`; equations `"D1@Sketch1" = "Width" * 2`; select first, then run the command;
  a PropertyManager panel with OK (Enter) and Cancel (Esc). When unsure how something should behave, do what
  SolidWorks does and say so.
- **Previews leave no trace.** A panel that shows its result before OK (mates, appearances, sketch fillet and
  offset, STL export) draws a preview that makes no undo step and no unsaved mark; OK records exactly one undo
  step (a `PartDefinition` snapshot), Cancel restores everything, and a dialog opened over a previewing panel
  cancels the preview first (`dialogs.rs`).
- **A failing feature never makes geometry vanish silently.** It gets a status and a message in the tree, the
  body stays as it was, and features after it still rebuild where they can.
- **Files:** `.oxyprt` and `.oxyasm` are ZIPs with `manifest.json` first and JSON written with stable key order;
  readers refuse an unknown `format`, `kind` or a higher `format_version` and ignore entries they do not know;
  saving writes `<name>.tmp` and renames it over the target.
- **Tests:** pure-Rust crates keep unit tests in-crate; anything needing OpenCASCADE goes in
  `crates/oxyde-tests/tests/`, built from small sketches on the standard planes (see `bodies.rs`).
- **Every user-visible change gets a `CHANGELOG.md` entry** under *Unreleased*: what it does for the user, in
  words, not which code changed. A file-format change updates `docs/FORMAT.md`; a design change updates
  `docs/ARCHITECTURE.md`.
- Commit messages: a title naming the user-visible changes, then bullets. Comments explain why, in full
  sentences; public items have doc comments.

## Ask the owner; do not decide

- The kernel: switching backends, or anything that would make code above `oxyde-kernel` know about OCCT.
- File-format changes: new keys or entries, a `format_version` bump, anything other tools would have to follow.
- New dependencies: crates, C++ libraries, vcpkg ports, toolchain or minimum Rust version changes.
- Version bumps in the workspace `Cargo.toml`, releases and tags. Executables are bundled by hand
  (BUILDING.md §5); there is no hosted release build.
- Repository-wide sweeps unrelated to the task: formatting passes, clippy clean-ups, renames.
- Behaviour the user did not ask for. If a fix needs a change in what the program does, say so and ask.
