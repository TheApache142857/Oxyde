#!/bin/bash
# Double-click in the Finder to build Oxyde's macOS installer: dist/Oxyde-<version>-macos-arm64.dmg
# (and the same app as a zip), on a Mac with Apple Silicon.
#
# It checks what the build needs first (Xcode's command line tools, Homebrew, Rust,
# OpenCASCADE, disk space), offers to install what is missing where that is one step, then
# runs packaging/bundle-macos.sh and opens the folder with the .dmg. Everything it prints
# also goes to dist/build-macos.log.
#
# The first build takes a while (Rust compiles everything once); later ones are quicker.

cd "$(dirname "$0")/.." || exit 1
ROOT="$(pwd)"

# A script started from the Finder does not read the shell's start-up files: find Homebrew
# and Rust where their installers put them.
export PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin:$PATH"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

bold() { printf '\033[1m%s\033[0m\n' "$1"; }
ok() { printf '  \033[32m✓\033[0m %s\n' "$1"; }
missing() { printf '  \033[31m✗\033[0m %s\n' "$1"; }

finish() {
    echo
    read -r -p "Press Return to close this window. "
    exit "$1"
}

# Asks a yes/no question; yes only on y or Y.
ask() {
    local answer
    read -r -p "  $1 [y/N] " answer
    [ "$answer" = "y" ] || [ "$answer" = "Y" ]
}

bold "Oxyde — build the macOS installer"
echo "Repository: $ROOT"
echo

# ---- What the build needs ----------------------------------------------------------------------
bold "Checking what the build needs"
problems=0

if [ "$(uname -s)" != "Darwin" ]; then
    missing "This is not a Mac."
    finish 1
fi
if [ "$(uname -m)" = "arm64" ]; then
    ok "Apple Silicon Mac"
else
    missing "This Mac has an Intel processor: only Apple Silicon builds are made and tested."
    problems=1
fi

if xcode-select -p >/dev/null 2>&1; then
    ok "Xcode command line tools"
else
    missing "Xcode command line tools (the compiler, codesign)"
    if ask "Open Apple's installer for them now?"; then
        xcode-select --install
        echo "  Finish that installation, then double-click this file again."
        finish 1
    fi
    problems=1
fi

if command -v brew >/dev/null 2>&1; then
    ok "Homebrew ($(brew --prefix))"
else
    missing "Homebrew, which installs OpenCASCADE. Install it from https://brew.sh (one command"
    echo "    to paste in Terminal), then double-click this file again."
    problems=1
fi

if command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1; then
    rust="$(rustc --version | awk '{print $2}')"
    major="$(echo "$rust" | cut -d. -f1)"
    minor="$(echo "$rust" | cut -d. -f2)"
    if [ "$major" -gt 1 ] || { [ "$major" -eq 1 ] && [ "$minor" -ge 88 ]; }; then
        ok "Rust $rust"
    else
        missing "Rust $rust is too old: Oxyde needs 1.88 or later."
        if command -v rustup >/dev/null 2>&1 && ask "Update Rust with rustup now?"; then
            rustup update stable && ok "Rust $(rustc --version | awk '{print $2}')" || problems=1
        else
            problems=1
        fi
    fi
else
    missing "Rust. Install it from https://rustup.rs (one command to paste in Terminal), then"
    echo "    double-click this file again."
    problems=1
fi

if [ -n "${OXYDE_OCCT_DIR:-}" ] && [ -d "$OXYDE_OCCT_DIR/lib" ]; then
    ok "OpenCASCADE in $OXYDE_OCCT_DIR"
elif command -v brew >/dev/null 2>&1 && brew list --versions opencascade >/dev/null 2>&1; then
    ok "OpenCASCADE $(brew list --versions opencascade | awk '{print $2}') (Homebrew)"
elif command -v brew >/dev/null 2>&1; then
    missing "OpenCASCADE, the geometry kernel"
    if ask "Install it with Homebrew now (a few hundred MB)?"; then
        brew install opencascade && ok "OpenCASCADE installed" || problems=1
    else
        problems=1
    fi
else
    missing "OpenCASCADE (installed with Homebrew, once Homebrew is there)"
    problems=1
fi

free_gb="$(df -g "$ROOT" | awk 'NR == 2 {print $4}')"
if [ -n "$free_gb" ] && [ "$free_gb" -lt 8 ]; then
    missing "Only $free_gb GB free on this disk: the build needs about 8 GB."
    problems=1
else
    ok "Disk space (${free_gb:-?} GB free)"
fi

if [ "$problems" -ne 0 ]; then
    echo
    bold "Something the build needs is missing (✗ above). Fix it and double-click this file again."
    finish 1
fi

# ---- Build -----------------------------------------------------------------------------------
echo
bold "Building (the first time takes a while)"
mkdir -p dist
LOG="$ROOT/dist/build-macos.log"
if bash packaging/bundle-macos.sh 2>&1 | tee "$LOG"; [ "${PIPESTATUS[0]}" -eq 0 ]; then
    echo
    dmg="$(ls -t dist/Oxyde-*-macos-*.dmg 2>/dev/null | head -n 1)"
    bold "Done: $dmg"
    echo "Open it and drag Oxyde to Applications. The first launch on another Mac needs"
    echo "right-click › Open (the app is not notarised by Apple)."
    open "$ROOT/dist"
    finish 0
else
    echo
    bold "The build failed. What went wrong is above, and in dist/build-macos.log."
    finish 1
fi
