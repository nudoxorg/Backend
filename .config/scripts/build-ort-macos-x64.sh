#!/usr/bin/env bash
# Build ONNX Runtime 1.28 CPU for macOS x86_64 (shared dylib).
#
# Upstream stopped shipping osx-x86_64 prebuilts at 1.28. We compile from
# source and bundle the dylib into Contents/Frameworks like arm64 releases.
#
# Usage: build-ort-macos-x64.sh
#
# Output: .ci-cache/ort/${ORT_VERSION}/macos-x64/lib/libonnxruntime.1.dylib

set -euo pipefail

ROOT="$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)"
ORT_VERSION="${ORT_VERSION:-1.28.0}"
ORT_TAG="v${ORT_VERSION}"
SRC="$ROOT/.ci-cache/onnxruntime-src/${ORT_TAG}"
BUILD="$ROOT/.ci-cache/onnxruntime-build/${ORT_VERSION}/macos-x86_64-shared"
OUT="$ROOT/.ci-cache/ort/${ORT_VERSION}/macos-x64"

log() { printf 'build-ort-macos-x64: %s\n' "$*"; }
die() { printf 'build-ort-macos-x64: error: %s\n' "$*" >&2; exit 1; }

for tool in cmake python3 git clang++; do
  command -v "$tool" >/dev/null || die "missing $tool on PATH"
done

if [ -f "$OUT/lib/libonnxruntime.1.dylib" ]; then
  log "reusing cached ORT dylib at $OUT"
  exit 0
fi

if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$(dirname "$SRC")"
  log "cloning onnxruntime ${ORT_TAG}"
  git clone --depth 1 --branch "$ORT_TAG" https://github.com/microsoft/onnxruntime.git "$SRC"
fi

log "building shared ORT ${ORT_VERSION} for macos x86_64 (this takes a while)"
(
  cd "$SRC"
  ./build.sh \
    --config Release \
    --parallel \
    --skip_tests \
    --compile_no_warning_as_error \
    --build_dir "$BUILD" \
    --osx_arch x86_64 \
    --build_shared_lib \
    --cmake_extra_defines \
      onnxruntime_ENABLE_PYTHON=OFF \
      onnxruntime_BUILD_UNIT_TESTS=OFF \
      CMAKE_OSX_DEPLOYMENT_TARGET=14.0
)

libdir="$BUILD/Release"
[ -f "$libdir/libonnxruntime.1.dylib" ] || die "missing $libdir/libonnxruntime.1.dylib"

mkdir -p "$OUT/lib"
cp -a "$libdir/libonnxruntime.1.dylib" "$OUT/lib/"
if [ -L "$libdir/libonnxruntime.1.dylib" ]; then
  target="$(readlink "$libdir/libonnxruntime.1.dylib")"
  cp -a "$libdir/$target" "$OUT/lib/" 2>/dev/null || true
fi
log "done — ORT dylib at $OUT/lib"
