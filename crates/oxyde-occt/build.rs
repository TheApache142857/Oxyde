//! Compiles the C++ shim against OpenCASCADE and links the OCCT toolkits.
//!
//! Windows (MSVC) only for now. OCCT is located from, in order:
//!   1. `OXYDE_OCCT_INCLUDE_DIR` + `OXYDE_OCCT_LIB_DIR` (explicit, both required together)
//!   2. `OXYDE_OCCT_DIR` — an install root (vcpkg `installed\x64-windows`, the official
//!      installer folder, or a CMake install prefix)
//!   3. `VCPKG_ROOT` — uses `%VCPKG_ROOT%\installed\x64-windows`
//!
//! `OXYDE_OCCT_CHECK_ONLY=1` skips all of this, for `cargo check` on other systems.
//!
//! See `docs/BUILDING.md`.

use std::env;
use std::path::{Path, PathBuf};

/// Toolkits needed by the shim, present in every supported OCCT version.
const CORE_TOOLKITS: &[&str] = &[
    "TKernel",
    "TKMath",
    "TKG2d",
    "TKG3d",
    "TKGeomBase",
    "TKBRep",
    "TKGeomAlgo",
    "TKTopAlgo",
    "TKPrim",
    "TKBO",
    "TKBool",
    "TKFillet",
    "TKOffset",
    "TKMesh",
    "TKShHealing",
    "TKXSBase",
    // XCAF documents, for reading the product structure of STEP assemblies.
    "TKCDF",
    "TKLCAF",
    "TKXCAF",
];

/// OCCT 7.8 merged the STEP toolkits into TKDESTEP; older versions ship them separately.
const STEP_TOOLKITS_7_8: &[&str] = &["TKDESTEP"];
const STEP_TOOLKITS_LEGACY: &[&str] = &["TKSTEP", "TKSTEPBase", "TKXDESTEP"];

fn main() {
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=cpp/shim.h");
    println!("cargo:rerun-if-changed=cpp/shim.cpp");
    for var in [
        "OXYDE_OCCT_DIR",
        "OXYDE_OCCT_INCLUDE_DIR",
        "OXYDE_OCCT_LIB_DIR",
        "VCPKG_ROOT",
        "OXYDE_OCCT_CHECK_ONLY",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    // `cargo check` without OpenCASCADE, on any system: nothing to compile or link.
    if env::var_os("OXYDE_OCCT_CHECK_ONLY").is_some() {
        return;
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        fail(&format!(
            "oxyde-occt currently supports only Windows with the MSVC toolchain \
             (target_os = \"{target_os}\", target_env = \"{target_env}\")."
        ));
    }

    let occt = Occt::locate();

    cxx_build::bridge("src/ffi.rs")
        .file("cpp/shim.cpp")
        .include(&occt.include_dir)
        .std("c++17")
        // Keep <windows.h>, if an OCCT header pulls it in, from defining min/max macros.
        .define("NOMINMAX", None)
        .define("WIN32_LEAN_AND_MEAN", None)
        // OCCT headers are noisy at high warning levels.
        .warnings(false)
        .compile("oxyde_occt_shim");

    println!("cargo:rustc-link-search=native={}", occt.lib_dir.display());
    let step = if occt.has_lib("TKDESTEP") {
        STEP_TOOLKITS_7_8
    } else {
        STEP_TOOLKITS_LEGACY
    };
    for lib in CORE_TOOLKITS.iter().chain(step) {
        if !occt.has_lib(lib) {
            fail(&format!(
                "OpenCASCADE library {lib}.lib not found in {}",
                occt.lib_dir.display()
            ));
        }
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}

struct Occt {
    include_dir: PathBuf,
    lib_dir: PathBuf,
}

impl Occt {
    fn locate() -> Self {
        let explicit_include = env::var_os("OXYDE_OCCT_INCLUDE_DIR").map(PathBuf::from);
        let explicit_lib = env::var_os("OXYDE_OCCT_LIB_DIR").map(PathBuf::from);

        let (include_dir, lib_dir) = match (explicit_include, explicit_lib) {
            (Some(include), Some(lib)) => (include, lib),
            (Some(_), None) | (None, Some(_)) => fail(
                "OXYDE_OCCT_INCLUDE_DIR and OXYDE_OCCT_LIB_DIR must be set together \
                 (or use OXYDE_OCCT_DIR instead).",
            ),
            (None, None) => Self::search_roots(),
        };

        if !include_dir.join("Standard.hxx").is_file() {
            fail(&format!(
                "Standard.hxx not found in OpenCASCADE include directory {}",
                include_dir.display()
            ));
        }
        if !lib_dir.join("TKernel.lib").is_file() {
            fail(&format!(
                "TKernel.lib not found in OpenCASCADE library directory {}",
                lib_dir.display()
            ));
        }
        Self {
            include_dir,
            lib_dir,
        }
    }

    fn search_roots() -> (PathBuf, PathBuf) {
        let mut roots = Vec::new();
        if let Some(dir) = env::var_os("OXYDE_OCCT_DIR") {
            roots.push(PathBuf::from(dir));
        }
        if let Some(vcpkg) = env::var_os("VCPKG_ROOT") {
            roots.push(PathBuf::from(vcpkg).join("installed").join("x64-windows"));
        }
        if roots.is_empty() {
            fail(
                "OpenCASCADE not configured: set OXYDE_OCCT_DIR to your OCCT install \
                 (e.g. C:\\dev\\vcpkg\\installed\\x64-windows), or set VCPKG_ROOT.",
            );
        }

        // The official installer nests the SDK in an `opencascade-<version>` folder.
        let mut candidates = Vec::new();
        for root in &roots {
            candidates.push(root.clone());
            candidates.extend(opencascade_subdirs(root));
        }

        for root in &candidates {
            if let (Some(include), Some(lib)) = (find_include_dir(root), find_lib_dir(root)) {
                return (include, lib);
            }
        }

        let searched: Vec<String> = candidates
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect();
        fail(&format!(
            "OpenCASCADE headers and libraries not found. Searched:\n{}",
            searched.join("\n")
        ))
    }

    fn has_lib(&self, name: &str) -> bool {
        self.lib_dir.join(format!("{name}.lib")).is_file()
    }
}

fn find_include_dir(root: &Path) -> Option<PathBuf> {
    // Official installer / CMake on Windows: inc; vcpkg: include\opencascade; others: include.
    ["inc", "include/opencascade", "include"]
        .iter()
        .map(|rel| root.join(rel))
        .find(|dir| dir.join("Standard.hxx").is_file())
}

fn find_lib_dir(root: &Path) -> Option<PathBuf> {
    // Official installer / CMake on Windows: win64\vc14\lib; vcpkg and others: lib.
    // Always the release libraries: Rust links the release C runtime even in debug builds.
    ["win64/vc14/lib", "lib"]
        .iter()
        .map(|rel| root.join(rel))
        .find(|dir| dir.join("TKernel.lib").is_file())
}

fn opencascade_subdirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter(|e| e.file_name().to_string_lossy().starts_with("opencascade-"))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

fn fail(message: &str) -> ! {
    panic!("\n\n{message}\n\nSee docs/BUILDING.md for setup instructions.\n");
}
