#!/usr/bin/env bash
# build.sh — Builds the MymeIM.app input method bundle.
#
# Usage:
#   ./macos/build.sh              # release build (default)
#   ./macos/build.sh --debug      # debug build of the Rust library
#
# The resulting bundle is written to:
#   macos/build/MymeIM.app
#
# To install:
#   cp -R macos/build/MymeIM.app ~/Library/Input\ Methods/
#   killall -HUP SystemUIServer   # or log out and back in

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

SOURCE_DIR="${SCRIPT_DIR}/MymeIM/Sources"
BRIDGE_HEADER="${SCRIPT_DIR}/MymeIM/myme-bridge.h"
INCLUDE_DIR="${REPO_ROOT}/crates/myme-core/include"
INFO_PLIST="${SCRIPT_DIR}/MymeIM/Info.plist"
DICT_SRC="${REPO_ROOT}/data/dict/system.dict"

BUILD_DIR="${SCRIPT_DIR}/build"
APP_BUNDLE="${BUILD_DIR}/MymeIM.app"
APP_CONTENTS="${APP_BUNDLE}/Contents"
APP_MACOS="${APP_CONTENTS}/MacOS"
APP_RESOURCES="${APP_CONTENTS}/Resources"

RUST_PROFILE="release"
if [[ "${1:-}" == "--debug" ]]; then
    RUST_PROFILE="debug"
fi

RUST_LIB_DIR="${REPO_ROOT}/target/${RUST_PROFILE}"
RUST_STATIC_LIB="${RUST_LIB_DIR}/libmyme_core.a"

BINARY_NAME="MymeIM"

# ---------------------------------------------------------------------------
# Step 1: Build the Rust static library
# ---------------------------------------------------------------------------

echo "==> Building Rust library (profile: ${RUST_PROFILE}) ..."
if [[ "${RUST_PROFILE}" == "release" ]]; then
    cargo build -p myme-core --release --manifest-path "${REPO_ROOT}/Cargo.toml"
else
    cargo build -p myme-core --manifest-path "${REPO_ROOT}/Cargo.toml"
fi

if [[ ! -f "${RUST_STATIC_LIB}" ]]; then
    echo "ERROR: Rust static library not found at ${RUST_STATIC_LIB}" >&2
    exit 1
fi
echo "    Static lib: ${RUST_STATIC_LIB}"

# ---------------------------------------------------------------------------
# Step 2: Compile Swift sources
# ---------------------------------------------------------------------------

echo "==> Compiling Swift sources ..."

# Gather all .swift files in Sources/
SWIFT_SOURCES=("${SOURCE_DIR}"/*.swift)

mkdir -p "${BUILD_DIR}"

# Compiler flags:
#  -import-objc-header   : bridging header that imports myme.h
#  -I                    : directory containing myme.h
#  -L / -l               : link against libmyme_core.a
#  -framework            : macOS frameworks required by InputMethodKit
#  -target               : aim for arm64+x86_64 (see below for universal)
#
# We compile for the host architecture here.  For a universal binary run
# this script twice with explicit -arch flags and lipo the results together.

swiftc \
    -module-name MymeIM \
    -o "${BUILD_DIR}/${BINARY_NAME}" \
    -import-objc-header "${BRIDGE_HEADER}" \
    -I "${INCLUDE_DIR}" \
    -framework Cocoa \
    -framework InputMethodKit \
    -Xlinker -force_load -Xlinker "${RUST_STATIC_LIB}" \
    "${SWIFT_SOURCES[@]}"

echo "    Binary: ${BUILD_DIR}/${BINARY_NAME}"

# ---------------------------------------------------------------------------
# Step 3: Assemble the .app bundle
# ---------------------------------------------------------------------------

echo "==> Assembling ${APP_BUNDLE} ..."

rm -rf "${APP_BUNDLE}"
mkdir -p "${APP_MACOS}"
mkdir -p "${APP_RESOURCES}"

# Executable
cp "${BUILD_DIR}/${BINARY_NAME}" "${APP_MACOS}/${BINARY_NAME}"

# Info.plist
cp "${INFO_PLIST}" "${APP_CONTENTS}/Info.plist"

# SKK dictionary (optional; the engine still works without it)
if [[ -f "${DICT_SRC}" ]]; then
    cp "${DICT_SRC}" "${APP_RESOURCES}/system.dict"
    echo "    Bundled dictionary: ${DICT_SRC}"
else
    echo "    WARNING: dictionary not found at ${DICT_SRC}; skipping"
fi

# ---------------------------------------------------------------------------
# Step 4: Ad-hoc code-sign the bundle
# ---------------------------------------------------------------------------
# Input methods must be code-signed (at least ad-hoc) to be loaded by the
# system.  A proper Developer ID signature is required for distribution.

echo "==> Ad-hoc signing ${APP_BUNDLE} ..."
codesign --force --deep --sign - "${APP_BUNDLE}"

echo ""
echo "Build complete: ${APP_BUNDLE}"
echo ""
echo "To install:"
echo "  cp -R \"${APP_BUNDLE}\" ~/Library/Input\\ Methods/"
echo "  killall -HUP SystemUIServer"
echo "  (or log out and back in)"
