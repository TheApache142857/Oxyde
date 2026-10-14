# Building Oxyde

**Supported platform: Windows 10/11, x64, MSVC toolchain.** macOS and Linux will come later; on any other target the build script stops with an explicit error.

## 1. Prerequisites

1. **Visual Studio 2022** (Community or Build Tools) with the **Desktop development with C++** workload.
2. **Rust** via [rustup](https://rustup.rs), with the default host `x86_64-pc-windows-msvc`. Rust 1.85 or newer (edition 2024).
3. **OpenCASCADE (OCCT)** 7.7 or newer. Tested with **8.0.1** (current vcpkg version); 7.7 and 7.8 should also work (the shim keeps their code paths) but are not regularly tested. Choose one of the options below.

### Option A — vcpkg (recommended)

```powershell
git clone https://github.com/microsoft/vcpkg C:\dev\vcpkg
C:\dev\vcpkg\bootstrap-vcpkg.bat
C:\dev\vcpkg\vcpkg install opencascade:x64-windows
```

Building OCCT through vcpkg takes a while (expect 30 minutes or more). vcpkg currently installs OCCT 8.0.x, which is what Oxyde is tested against.

Then:

| Variable | Value |
|---|---|
| `OXYDE_OCCT_DIR` | `C:\dev\vcpkg\installed\x64-windows` (or just set `VCPKG_ROOT=C:\dev\vcpkg`) |
| `PATH` (append) | `C:\dev\vcpkg\installed\x64-windows\bin` |

### Option B — official installer

Download the Windows installer from [dev.opencascade.org](https://dev.opencascade.org/release) and install it, e.g. to `C:\OpenCASCADE-7.8.1-vc14-64`.

| Variable | Value |
|---|---|
| `OXYDE_OCCT_DIR` | `C:\OpenCASCADE-7.8.1-vc14-64` (the build script finds the `opencascade-7.8.1` folder inside) |
| `PATH` (append) | `...\opencascade-7.8.1\win64\vc14\bin` **and** each third-party `bin` folder shipped next to it (freetype, tbb, …) |

Alternatively, run the installer's `env.bat` in the terminal before `cargo`, which sets `PATH` for that session.

### Option C — explicit directories

For a custom build, point directly at the folders containing `Standard.hxx` and `TKernel.lib`:

```powershell
$env:OXYDE_OCCT_INCLUDE_DIR = "D:\occt\inc"
$env:OXYDE_OCCT_LIB_DIR     = "D:\occt\win64\vc14\lib"
```

Always use the **release** OCCT libraries, even for `cargo build` in debug: Rust links the release C runtime.

### Setting variables permanently (PowerShell)

```powershell
[Environment]::SetEnvironmentVariable("OXYDE_OCCT_DIR", "C:\dev\vcpkg\installed\x64-windows", "User")
[Environment]::SetEnvironmentVariable("Path", $env:Path + ";C:\dev\vcpkg\installed\x64-windows\bin", "User")
```

Open a new terminal afterwards.

Cargo must be run from the repository folder (`cd C:\path\to\Oxyde`); a new terminal usually starts in your home folder, where Cargo reports *could not find `Cargo.toml`*.

## 2. Build and test

```powershell
cargo run --release -p oxyde-app               # the application
cargo run --release -p oxyde-app -- part.oxy   # ... opening a file
cargo test --workspace                         # everything
cargo test -p oxyde-geom -p oxyde-sketch -p oxyde-model -p oxyde-format -p oxyde-render   # pure Rust, no OCCT needed
cargo test -p oxyde-tests                      # engine + OpenCASCADE end-to-end tests
cargo run  -p oxyde-occt --example make_box    # writes box.step in the current folder
```

Open `box.step` in SolidWorks, FreeCAD or eDrawings: you should see a 40 × 20 × 10 mm box.

In the application, **File › Open demo bracket** builds a part using every feature type. The viewport is software-rendered: use `--release` for smooth navigation (the renderer crate is optimized even in debug builds, the rest is not).

The viewport draws with the GPU (wgpu). If it shows nothing or crashes on startup on your machine, run with the software renderer and report the GPU and driver:

```powershell
$env:OXYDE_RENDERER = "cpu"; cargo run --release -p oxyde-app
```

## 3. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Build script panics with "OpenCASCADE not configured" or "headers and libraries not found" | `OXYDE_OCCT_DIR` / `VCPKG_ROOT` unset or pointing at the wrong folder. The message lists every folder searched. |
| `fatal error C1083: Cannot open include file: 'Standard.hxx'` | Include directory wrong — use Option C to set it explicitly. |
| `LNK2019: unresolved external symbol` | Library directory from a different OCCT version than the headers, or a missing toolkit. Report the symbol name. |
| `C1083` for a `*Iterator*.hxx` header, or `'DynamicType': is not a member of 'Standard_Failure'` | OCCT 8 removed legacy typedef headers and changed its exception class. The shim handles both 7.x and 8.x; if a new error of this kind appears, report it with the OCCT version. |
| Tests exit with `0xc0000135` (`STATUS_DLL_NOT_FOUND`) | OCCT DLLs (or their third-party DLLs) are not on `PATH`. |
| Build script says only Windows with MSVC is supported | You are on another OS, or using the `x86_64-pc-windows-gnu` toolchain. Run `rustup default stable-x86_64-pc-windows-msvc`. |
