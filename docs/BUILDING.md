# Building Oxyde

**Main platform: Windows 10/11, x64, MSVC toolchain.** Linux builds too, with the OpenCASCADE 7.8 of Debian and Ubuntu (see [Linux](#linux)), and macOS on Apple Silicon with Homebrew's OpenCASCADE (see [macOS](#macos-apple-silicon)). On any other target the build script stops with an explicit error. Executables for Windows and macOS are built by the [release workflow](#5-release-builds-github-actions).

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
cargo run --release -p oxyde-app -- part.oxyprt   # ... opening a file
cargo test --workspace                         # everything
cargo test -p oxyde-geom -p oxyde-sketch -p oxyde-kernel -p oxyde-model -p oxyde-assembly -p oxyde-format -p oxyde-render   # pure Rust, no OCCT needed
cargo test -p oxyde-tests                      # engine + OpenCASCADE end-to-end tests
cargo run  -p oxyde-occt --example make_box    # writes box.step in the current folder
```

On a machine without OpenCASCADE (Linux, macOS), the whole workspace can still be type-checked: `OXYDE_OCCT_CHECK_ONLY=1 cargo check --workspace --all-targets`. It skips compiling and linking the C++ shim, so only `cargo check` works this way, not `build`, `run` or `test`.

Open `box.step` in SolidWorks, FreeCAD or eDrawings: you should see a 40 × 20 × 10 mm box.

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

- `dist\Oxyde\`: `Oxyde.exe` with exactly the OpenCASCADE DLLs it needs (followed with `dumpbin`; without it, every DLL of OpenCASCADE's `bin` folder), the licences and the README;
- `dist\Oxyde-<version>-windows-x64.zip`: the same, to unzip and run anywhere;
- `dist\Oxyde-Setup-<version>.exe`, when [Inno Setup 6](https://jrsoftware.org/isinfo.php) is installed: a per-user installer (no administrator rights) with a Start menu entry, an optional desktop icon, and `.oxyprt` / `.oxyasm` files opening in Oxyde.

The script checks the bundle by running `Oxyde.exe --version`, which loads OpenCASCADE: a missing DLL fails the bundle rather than the user. The version comes from the workspace `Cargo.toml`. The icon is drawn by `packaging/make_icon.py` (Pillow); its outputs are committed.

## 4. Bundling for macOS

```sh
bash packaging/bundle-macos.sh
```

builds the release and writes:

- `dist/Oxyde.app`: the executable with the OpenCASCADE dylibs it needs (followed with `otool -L`, copied into `Contents/Frameworks` and re-pointed through `@rpath`, so the app runs on a Mac without Homebrew), the icon (an `.icns` made from `packaging/oxyde.png` with `sips` and `iconutil`), the licences, and `.oxyprt` / `.oxyasm` declared as its document types (`packaging/Info.plist`);
- `dist/Oxyde-<version>-macos-arm64.dmg`: the app with an *Applications* shortcut, to drag across;
- `dist/Oxyde-<version>-macos-arm64.zip`: the same app zipped.

The app is signed **ad hoc** (Apple Silicon runs no unsigned code) unless `OXYDE_SIGN_IDENTITY` names a Developer ID certificate. Ad hoc signed apps are not notarised, so on another Mac the first launch is refused with *"cannot be opened because the developer cannot be verified"*: open it with right-click › *Open*, or run `xattr -dr com.apple.quarantine /Applications/Oxyde.app`. Notarisation (a paid Apple developer account, `notarytool`) is the next step for a wider release. Opening a file by double-clicking it in the Finder is not wired yet (the app takes the file on its command line); drag it onto the window or use File › Open.

## 5. Release builds (GitHub Actions)

`.github/workflows/release.yml` builds both executables on GitHub's runners, running the tests first:

- **Windows x64** (`windows-latest`): OpenCASCADE is built by the runner's vcpkg and cached between runs (an hour or so the first time, seconds afterwards; bump `OCCT_CACHE` in the workflow to rebuild it), then `packaging\bundle.ps1` makes the zip and the Inno Setup installer.
- **macOS Apple Silicon** (`macos-latest`): `brew install opencascade`, then `packaging/bundle-macos.sh` makes the `.dmg` and the zip.

Pushing a tag `v<version>` (e.g. `git tag v0.0.4 && git push origin v0.0.4`) builds both and attaches them to a **draft** GitHub release of that tag, to write the notes before publishing. *Run workflow* on the Actions page builds both without a release and keeps them as workflow artifacts, to try a build. The version in the file names comes from the workspace `Cargo.toml`; keep it and the tag in step.

## 6. Troubleshooting


| Symptom | Cause / fix |
|---|---|
| Build script panics with "OpenCASCADE not configured" or "headers and libraries not found" | `OXYDE_OCCT_DIR` / `VCPKG_ROOT` unset or pointing at the wrong folder. The message lists every folder searched. |
| `fatal error C1083: Cannot open include file: 'Standard.hxx'` | Include directory wrong — use Option C to set it explicitly. |
| `LNK2019: unresolved external symbol` | Library directory from a different OCCT version than the headers, or a missing toolkit. Report the symbol name. |
| `C1083` for a `*Iterator*.hxx` header, or `'DynamicType': is not a member of 'Standard_Failure'` | OCCT 8 removed legacy typedef headers and changed its exception class. The shim handles both 7.x and 8.x; if a new error of this kind appears, report it with the OCCT version. |
| Tests exit with `0xc0000135` (`STATUS_DLL_NOT_FOUND`) | OCCT DLLs (or their third-party DLLs) are not on `PATH`. |
| Build script says oxyde-occt supports Windows (MSVC), Linux and macOS | On Windows, you are using the `x86_64-pc-windows-gnu` toolchain: run `rustup default stable-x86_64-pc-windows-msvc`. |
