#!/usr/bin/env bash
# Builds the Oxyde bundle for macOS on Apple Silicon: dist/Oxyde.app (the executable, the
# OpenCASCADE dylibs it needs and theirs, icon, licences), dist/Oxyde-<version>-macos-arm64.dmg
# (drag to Applications) and the same as a zip.
#
#   bash packaging/bundle-macos.sh
#
# OpenCASCADE is found as for building (see docs/BUILDING.md): Homebrew's, or OXYDE_OCCT_DIR.
# Netgen, the volume mesher, when the build has it: from OXYDE_NETGEN_DIR.
# The dylibs are copied into Contents/Frameworks and the executable and every dylib are
# pointed at them through @rpath, so the .app runs on a Mac without Homebrew.
#
# Signing: ad hoc unless OXYDE_SIGN_IDENTITY names a Developer ID certificate. An ad hoc
# signed app is not notarised: the first time, open it with right-click › Open (or
# `xattr -dr com.apple.quarantine Oxyde.app`).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [ "$(uname -s)" != "Darwin" ]; then
    echo "This script bundles for macOS; run it on a Mac." >&2
    exit 1
fi
ARCH="$(uname -m)"
VERSION="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)"
echo "Oxyde $VERSION ($ARCH)"

# ---- Build ----------------------------------------------------------------------------------
cargo build --release -p oxyde-app

# ---- Where the OpenCASCADE and Netgen dylibs are --------------------------------------------
# Libraries referred to through @rpath or @loader_path are looked for here.
SEARCH_DIRS=()
if [ -n "${OXYDE_OCCT_DIR:-}" ]; then
    SEARCH_DIRS+=("$OXYDE_OCCT_DIR/lib")
fi
if [ -n "${OXYDE_NETGEN_DIR:-}" ]; then
    SEARCH_DIRS+=("$OXYDE_NETGEN_DIR/lib")
fi
if command -v brew >/dev/null 2>&1; then
    SEARCH_DIRS+=("$(brew --prefix opencascade 2>/dev/null || true)/lib" "$(brew --prefix)/lib")
fi
SEARCH_DIRS+=(/usr/local/lib /opt/homebrew/lib)

# ---- Assemble dist/Oxyde.app ----------------------------------------------------------------
DIST="$ROOT/dist"
APP="$DIST/Oxyde.app"
CONTENTS="$APP/Contents"
FRAMEWORKS="$CONTENTS/Frameworks"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources" "$FRAMEWORKS"
cp target/release/oxyde "$CONTENTS/MacOS/Oxyde"
chmod u+w "$CONTENTS/MacOS/Oxyde"
sed "s/@VERSION@/$VERSION/g" packaging/Info.plist > "$CONTENTS/Info.plist"
printf 'APPL????' > "$CONTENTS/PkgInfo"
cp LICENSE "$CONTENTS/Resources/LICENSE.txt"
cp packaging/THIRD-PARTY.txt "$CONTENTS/Resources/"

# The icon: an .icns made from packaging/oxyde.png with the system's tools.
ICONSET="$(mktemp -d)/oxyde.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    double=$((size * 2))
    sips -z "$size" "$size" packaging/oxyde.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    sips -z "$double" "$double" packaging/oxyde.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$CONTENTS/Resources/oxyde.icns"

# ---- The dylibs: followed from the executable, copied, and re-pointed at @rpath -------------
# resolve <reference> <file that refers to it>: the path of a library on disk, or nothing
# for a system library.
resolve() {
    local ref="$1" from="$2" name
    case "$ref" in
        /usr/lib/*|/System/*) return 0 ;;
        @rpath/*|@loader_path/*|@executable_path/*)
            name="$(basename "$ref")"
            for dir in "$(dirname "$from")" "${SEARCH_DIRS[@]}"; do
                if [ -f "$dir/$name" ]; then
                    echo "$dir/$name"
                    return 0
                fi
            done
            echo "Cannot find $ref (needed by $(basename "$from"))" >&2
            return 1
            ;;
        *) echo "$ref" ;;
    esac
}

# Every library the file depends on, copied into Frameworks (once) with its own
# dependencies, and the file's reference changed to @rpath/<name>.
copied=0
copy_deps() {
    local file="$1" ref path name target
    # `otool -L` lists the file itself first; skip it.
    while IFS= read -r ref; do
        path="$(resolve "$ref" "$file")" || exit 1
        [ -n "$path" ] || continue
        name="$(basename "$path")"
        target="$FRAMEWORKS/$name"
        if [ ! -f "$target" ]; then
            cp "$path" "$target"
            chmod u+w "$target"
            install_name_tool -id "@rpath/$name" "$target"
            copied=$((copied + 1))
            copy_deps "$target"
        fi
        if [ "$ref" != "@rpath/$name" ]; then
            install_name_tool -change "$ref" "@rpath/$name" "$file"
        fi
    done < <(otool -L "$file" | tail -n +2 | awk '{print $1}')
}
copy_deps "$CONTENTS/MacOS/Oxyde"
# Where @rpath points: the Frameworks folder, for the executable and for the dylibs.
install_name_tool -add_rpath "@executable_path/../Frameworks" "$CONTENTS/MacOS/Oxyde" 2>/dev/null || true
for lib in "$FRAMEWORKS"/*.dylib; do
    [ -e "$lib" ] || continue
    install_name_tool -add_rpath "@loader_path" "$lib" 2>/dev/null || true
done
echo "$(ls "$FRAMEWORKS" | wc -l | tr -d ' ') dylibs in Frameworks"
# Netgen is optional at build time (docs/BUILDING.md): say which kind of bundle this is.
if [ -f "$FRAMEWORKS/libnglib.dylib" ]; then
    echo "Netgen volume mesher included (libnglib.dylib, libngcore.dylib)"
else
    echo "Warning: no Netgen in this build: the Simulation tab meshes with its own mesher" >&2
fi

# ---- Sign: Apple Silicon runs no unsigned code, and install_name_tool broke the signatures --
IDENTITY="${OXYDE_SIGN_IDENTITY:--}"
codesign --force --deep --sign "$IDENTITY" --timestamp=none "$APP"
if [ "$IDENTITY" = "-" ]; then
    echo "Signed ad hoc (set OXYDE_SIGN_IDENTITY to a Developer ID to sign for distribution)"
else
    echo "Signed with $IDENTITY"
fi

# ---- Smoke test: the executable must start and find every dylib ----------------------------
"$CONTENTS/MacOS/Oxyde" --version > "$DIST/version.txt"
echo "Bundle runs: $(cat "$DIST/version.txt")"

# ---- Disk image and zip ---------------------------------------------------------------------
DMG="$DIST/Oxyde-$VERSION-macos-$ARCH.dmg"
STAGE="$(mktemp -d)/Oxyde"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "Oxyde $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
echo "Wrote $DMG"

ZIP="$DIST/Oxyde-$VERSION-macos-$ARCH.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
echo "Wrote $ZIP"
