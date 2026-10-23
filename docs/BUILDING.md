# Building Oxyde

**Main platform: Windows 10/11, x64, MSVC toolchain.** Linux builds too, with the OpenCASCADE 7.8 of Debian and Ubuntu (see [Linux](#linux)), and macOS on Apple Silicon with Homebrew's OpenCASCADE (see [macOS](#macos-apple-silicon)). On any other target the build script stops with an explicit error. Executables for Windows and macOS are bundled by the scripts in `packaging/` (see [Releasing](#5-releasing)).

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

### Netgen, the volume mesher (optional)

The Simulation tab fills parts with tetrahedra using [Netgen](https://ngsolve.org) (LGPL-2.1, linked as a shared library) through OpenCASCADE (`Kernel::volume_mesh`). Oxyde builds without it: the build script prints *building without the Netgen volume mesher* and the Simulation tab uses its own, coarser mesher instead. With vcpkg:

```powershell
$env:VCPKG_MAX_CONCURRENCY = "4"     # at most 4 compilers at once
C:\dev\vcpkg\vcpkg install netgen[occ]:x64-windows
```

This builds Netgen 6.2.2401 (vcpkg's port, patched for OpenCASCADE 8) against the OpenCASCADE already installed, without rebuilding it (a few minutes), and puts it in the same folder: the build script finds it there, and `nglib.dll` and `ngcore.dll` land in the `bin` folder already on `PATH`. Run `cargo clean -p oxyde-occt` afterwards (or touch `crates/oxyde-occt/build.rs`) so the shim is rebuilt with it; `cargo test -p oxyde-tests --test netgen` then meshes real parts instead of skipping.

| Variable | Effect |
|---|---|
| `OXYDE_NETGEN_DIR` | a Netgen install root of its own (with `include/occ/occgeom.hpp` or `include/netgen/occ/occgeom.hpp`, and `lib/nglib` and `lib/ngcore`); the build fails if it is not there |
| `OXYDE_NETGEN=0` | build without Netgen even when it is found |
| `OXYDE_NETGEN=1` | fail the build when Netgen is not found |

**By hand** (another OpenCASCADE, or macOS, where Homebrew has no Netgen): build the same version with CMake against the OpenCASCADE Oxyde uses. Upstream 6.2.2401 predates OpenCASCADE 7.8's toolkit renames and 8.0's header removals, so apply the vcpkg port's `occt-compat.patch` first (`ports/netgen` in vcpkg; the other patches there are for vcpkg's packaging). The layout options make the install look like vcpkg's on every system, and on macOS the absolute install names let the executable find the dylibs as it finds Homebrew's:

```sh
git clone --branch v6.2.2401 --depth 1 https://github.com/NGSolve/netgen
git -C netgen apply /path/to/vcpkg/ports/netgen/occt-compat.patch
cmake -S netgen -B netgen-build -DCMAKE_BUILD_TYPE=Release \
  -DUSE_SUPERBUILD=OFF -DUSE_OCC=ON -DUSE_GUI=OFF -DUSE_PYTHON=OFF -DUSE_MPI=OFF \
  -DUSE_SPDLOG=OFF -DUSE_NATIVE_ARCH=OFF -DENABLE_UNIT_TESTS=OFF \
  -DOpenCASCADE_DIR="$(brew --prefix opencascade)/lib/cmake/opencascade" \
  -DCMAKE_INSTALL_PREFIX="$HOME/netgen" -DCMAKE_INSTALL_NAME_DIR="$HOME/netgen/lib" \
  -DNG_INSTALL_DIR_BIN=bin -DNG_INSTALL_DIR_LIB=lib -DNG_INSTALL_DIR_INCLUDE=include \
  -DNG_INSTALL_DIR_CMAKE=lib/cmake/netgen -DNG_INSTALL_DIR_RES=share
cmake --build netgen-build -j 4 && cmake --install netgen-build
export OXYDE_NETGEN_DIR="$HOME/netgen"
```

On Windows the same works from a *Developer PowerShell for VS 2022* with `-G Ninja` (or the Visual Studio generator and `--config Release`), `-DOpenCASCADE_DIR=C:\dev\vcpkg\installed\x64-windows\share\opencascade`, and Netgen's `bin` folder added to `PATH`. Only the vcpkg build has been tested; the macOS one has not been tried yet. Whichever way it is built, `packaging/THIRD-PARTY.txt` must name the exact source and options of the Netgen that is shipped (the LGPL asks for it): update it when the version or the build changes.

### OpenFOAM, the solver of flow studies (at run time, optional)

Flow studies (`docs/CFD.md`) run [OpenFOAM](https://www.openfoam.com), OpenCFD's line, v2606 or later, as a separate program: Oxyde builds without it and never links it (it is GPL-3.0). At run time `oxyde-flow` finds it in Preferences › Simulation, the variable `OXYDE_OPENFOAM_DIR`, or the usual places: `C:\Program Files\ESI-OpenCFD\OpenFOAM\v*` and `%APPDATA%\ESI-OpenCFD\OpenFOAM\v*` on Windows (OpenCFD's MinGW installer; parallel runs also need Microsoft MPI 10), `/usr/lib/openfoam/openfoam*` on Linux (OpenCFD's packages), `/Applications/OpenFOAM-v*.app` on macOS (openfoam-app, Apple silicon). The Foundation's line (openfoam.org) names its solvers differently and is not supported.

`cargo test -p oxyde-flow` runs its unit tests anywhere and its OpenFOAM tests (`tests/openfoam.rs`: a laminar pipe against Hagen–Poiseuille, the same on two processes, a sphere's drag, a turbulent pipe) where OpenFOAM is installed; elsewhere they are skipped with a note. Without OpenFOAM, run them in OpenCFD's image (amd64 and arm64), one at a time:

```sh
cargo test -p oxyde-flow --test openfoam --no-run      # prints the test binary's path
docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$PWD/target:$PWD/target:ro" \
  --entrypoint "$PWD/target/debug/deps/openfoam-<hash>" opencfd/openfoam-run:2606 \
  --test-threads=1 --nocapture
```

## 2. Build and test

```powershell
cargo run --release -p oxyde-app               # the application
cargo run --release -p oxyde-app -- part.oxyprt   # ... opening a file
cargo test --workspace                         # everything
cargo test -p oxyde-geom -p oxyde-sketch -p oxyde-kernel -p oxyde-model -p oxyde-assembly -p oxyde-format -p oxyde-render   # pure Rust, no OCCT needed
cargo test -p oxyde-tests                      # engine + OpenCASCADE end-to-end tests
cargo test -p oxyde-tests --test netgen        # the volume mesher (skips without Netgen)
cargo run  -p oxyde-occt --example make_box    # writes box.step in the current folder
```

On a machine without OpenCASCADE (Linux, macOS), the whole workspace can still be type-checked: `OXYDE_OCCT_CHECK_ONLY=1 cargo check --workspace --all-targets`. It skips compiling and linking the C++ shim, so only `cargo check` works this way, not `build`, `run` or `test`.

Open `box.step` in SolidWorks, FreeCAD or eDrawings: you should see a 40 × 20 × 10 mm box.

The application has a Cargo feature per kernel, both on by default: `occt` (OpenCASCADE, which needs the setup above) and `native` (Oxyde's own, in Rust). With both, each session builds its parts with one: Preferences › Debug › Kernel, from the next start, or `--kernel occt` / `--kernel native` for one session (`cargo run --release -p oxyde-app -- --kernel native part.oxyprt`). `cargo build -p oxyde-app --no-default-features --features native` builds an application with no OpenCASCADE at all, and so no DLLs to ship, that builds only what the native kernel can yet (docs/NATIVE_KERNEL.md): no fillets, chamfers, shells, sweeps, lofts, STEP or IGES, and no Netgen.

In the application, **File › Open demo bracket** builds a part using every feature type. Use `--release` for smooth interaction: the renderer crate is optimized even in debug builds, the rest of the application is not.

The viewport draws with the GPU (wgpu). If it shows nothing or crashes on startup on your machine, run with the software renderer and report the GPU and driver:

```powershell
$env:OXYDE_RENDERER = "cpu"; cargo run --release -p oxyde-app
```

## Linux

Debian 13 (trixie) and Ubuntu ship OpenCASCADE 7.8, which the build finds in `/usr` on its own:

```sh
sudo apt install g++ libocct-foundation-dev libocct-modeling-data-dev \
  libocct-modeling-algorithms-dev libocct-data-exchange-dev libocct-ocaf-dev
cargo run --release -p oxyde-app
```

Without root, the same packages in a `debian:trixie` container work too, with the host's Rust toolchain mounted. Tests run as on Windows. On a small machine such as a Raspberry Pi, test in `--release` and a few packages at a time (`cargo test --release -p oxyde-tests`): a debug build of the whole workspace takes gigabytes of disk. `cargo run -p oxyde-tests --example step_to_oxyde -- file.step out/` converts a STEP assembly into Oxyde files from the command line.

## macOS (Apple Silicon)

Homebrew's OpenCASCADE (7.9 at the time of writing) is found in `/opt/homebrew/opt/opencascade` on its own:

```sh
xcode-select --install          # the C++ compiler, if not there yet
brew install opencascade
cargo run --release -p oxyde-app
```

Tests run as on Windows (`cargo test --workspace`). Another OpenCASCADE: set `OXYDE_OCCT_DIR` to its install prefix (the folder holding `include/opencascade` and `lib`). The dylibs are found through their install names, so nothing needs to be on a path. Only Apple Silicon (arm64) is built and tested; an Intel Mac needs Homebrew's x86_64 OpenCASCADE and has not been tried.

## 3. Bundling for Windows

```powershell
powershell -ExecutionPolicy Bypass -File packaging\bundle.ps1
```

builds the release and writes:

- `dist\Oxyde\`: `Oxyde.exe` with exactly the OpenCASCADE DLLs it needs (and Netgen's, `nglib.dll` and `ngcore.dll`, when the build has it: the script says which) (followed with `dumpbin`; without it, every DLL of OpenCASCADE's `bin` folder), the licences and the README;
- `dist\Oxyde-<version>-windows-x64.zip`: the same, to unzip and run anywhere;
- `dist\Oxyde-Setup-<version>.exe`, when [Inno Setup 6](https://jrsoftware.org/isinfo.php) (6.1 or later) is installed, for every user or for you alone (the script looks for `ISCC.exe` on `PATH`, in `Program Files (x86)\Inno Setup 6` and in `%LOCALAPPDATA%\Programs\Inno Setup 6`): a per-user installer (no administrator rights) with a Start menu entry, an optional desktop icon, and `.oxyprt` / `.oxyasm` files opening in Oxyde. An optional task, *Download and install OpenFOAM*, downloads OpenCFD's own Windows installer for the version flow studies are written for (`OpenFoamUrl` in `packaging/installer.iss`, checked against its SHA-256) and runs it once Oxyde is installed; OpenFOAM is never inside Oxyde's installer. Moving to another OpenFOAM version means changing its four `#define`s together;
- `dist\Oxyde-<version>-windows-x64.msi`, when the [WiX Toolset](https://wixtoolset.org) 5 or later is installed (`dotnet tool install --global wix --version 5.0.2`, which needs the .NET SDK 8 or later; WiX 6 asks you to accept its maintenance-fee licence first, `wix eula accept wix6`): a Windows Installer package for every user of the machine, into `Program Files\Oxyde` (Windows asks for administrator rights), with a Start menu entry and the same file types, for IT departments and deployment tools (`msiexec /i Oxyde-<version>-windows-x64.msi /qn` installs it silently). A newer `.msi` replaces an older one; the two installers are alternatives, not meant to be installed side by side. Its definition is `packaging/installer.wxs`, whose `UpgradeCode` must never change.

The script checks the bundle by running `Oxyde.exe --version`, which loads OpenCASCADE: a missing DLL fails the bundle rather than the user. The version comes from the workspace `Cargo.toml`. The icon is drawn by `packaging/make_icon.py` (Pillow); its outputs are committed.

## 4. Bundling for macOS

Double-click `packaging/Build Oxyde.dmg.command` in the Finder, or in Terminal:

```sh
bash packaging/bundle-macos.sh
```

The `.command` file checks what the build needs first (an Apple Silicon Mac, Xcode's command line tools, Homebrew, Rust 1.88 or later, OpenCASCADE, 8 GB free), offers to install Xcode's tools and OpenCASCADE when they are missing and says how to get the rest, then runs `bundle-macos.sh`, keeps a log in `dist/build-macos.log` and opens `dist` in the Finder. Either way the build writes:

- `dist/Oxyde.app`: the executable with the OpenCASCADE dylibs it needs (followed with `otool -L`, copied into `Contents/Frameworks` and re-pointed through `@rpath`, so the app runs on a Mac without Homebrew), the icon (an `.icns` made from `packaging/oxyde.png` with `sips` and `iconutil`), the licences, and `.oxyprt` / `.oxyasm` declared as its document types (`packaging/Info.plist`);
- `dist/Oxyde-<version>-macos-arm64.dmg`: the app with an *Applications* shortcut, to drag across;
- `dist/Oxyde-<version>-macos-arm64.zip`: the same app zipped.

The app is signed **ad hoc** (Apple Silicon runs no unsigned code) unless `OXYDE_SIGN_IDENTITY` names a Developer ID certificate. Ad hoc signed apps are not notarised, so on another Mac the first launch is refused with *"cannot be opened because the developer cannot be verified"*: open it with right-click › *Open*, or run `xattr -dr com.apple.quarantine /Applications/Oxyde.app`. Notarisation (a paid Apple developer account, `notarytool`) is the next step for a wider release. Opening a file by double-clicking it in the Finder is not wired yet (the app takes the file on its command line); drag it onto the window or use File › Open.

## 5. Releasing

Executables are built by hand, on a machine of each kind (no hosted build service is used): `packaging\bundle.ps1` on a Windows PC gives the zip, the `.exe` installer and the `.msi`, `packaging/bundle-macos.sh` on an Apple Silicon Mac gives the `.dmg` and the zip. Both read the version from the workspace `Cargo.toml` and check the bundle by running it with `--version`. A Mac is needed for the `.dmg`: OpenCASCADE's dylibs, `codesign` and `hdiutil` exist only there. To release: bump the version, run both scripts, tag the commit (`git tag v0.0.8`) and attach the files to a GitHub release of that tag.

## 6. Troubleshooting


| Symptom | Cause / fix |
|---|---|
| Build script panics with "OpenCASCADE not configured" or "headers and libraries not found" | `OXYDE_OCCT_DIR` / `VCPKG_ROOT` unset or pointing at the wrong folder. The message lists every folder searched. |
| `fatal error C1083: Cannot open include file: 'Standard.hxx'` | Include directory wrong — use Option C to set it explicitly. |
| `LNK2019: unresolved external symbol` | Library directory from a different OCCT version than the headers, or a missing toolkit. Report the symbol name. |
| `C1083` for a `*Iterator*.hxx` header, or `'DynamicType': is not a member of 'Standard_Failure'` | OCCT 8 removed legacy typedef headers and changed its exception class. The shim handles both 7.x and 8.x; if a new error of this kind appears, report it with the OCCT version. |
| Tests exit with `0xc0000135` (`STATUS_DLL_NOT_FOUND`) | OCCT DLLs (or their third-party DLLs, or Netgen's `nglib.dll` and `ngcore.dll`) are not on `PATH`. |
| Warning *building without the Netgen volume mesher* | Netgen is not installed where the build looks (see [Netgen](#netgen-the-volume-mesher-optional)); the message lists the folders searched. Harmless: simulations use the built-in mesher. After installing it, `cargo clean -p oxyde-occt`. |
| Build script says oxyde-occt supports Windows (MSVC), Linux and macOS | On Windows, you are using the `x86_64-pc-windows-gnu` toolchain: run `rustup default stable-x86_64-pc-windows-msvc`. |
