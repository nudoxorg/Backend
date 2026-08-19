#!/usr/bin/env bash
# Build ONNX Runtime 1.28 CPU-only static libraries for macOS.
#
# Upstream stopped shipping osx-x86_64 prebuilts at 1.28, but ort can link
# statically when pointed at a local Release tree (ORT_LIB_PATH).
#
# Usage:
#   build-ort-static-macos.sh <x86_64|arm64>
#
# Output: .ci-cache/ort-static/${ORT_VERSION}/macos-<arch>/lib/*.a

set -euo pipefail

ARCH="${1:?usage: build-ort-static-macos.sh <x86_64|arm64>}"
ROOT="$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)"
ORT_VERSION="${ORT_VERSION:-1.28.0}"
ORT_TAG="v${ORT_VERSION}"
SRC="$ROOT/.ci-cache/onnxruntime-src/${ORT_TAG}"
OUT="$ROOT/.ci-cache/ort-static/${ORT_VERSION}/macos-${ARCH}"
BUILD="$ROOT/.ci-cache/onnxruntime-build/${ORT_TAG}/macos-${ARCH}"

log() { printf 'build-ort-static-macos: %s\n' "$*"; }
die() { printf 'build-ort-static-macos: error: %s\n' "$*" >&2; exit 1; }

case "$ARCH" in
  x86_64|arm64) ;;
  *) die "unknown arch: $ARCH (expected x86_64 or arm64)" ;;
esac

for tool in cmake python3 git clang++; do
  command -v "$tool" >/dev/null || die "missing $tool on PATH"
done

if [ -d "$OUT/lib" ] && find "$OUT/lib" -name 'libonnxruntime*.a' | grep -q .; then
  log "reusing cached static ORT at $OUT"
  exit 0
fi

if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$(dirname "$SRC")"
  log "cloning onnxruntime ${ORT_TAG}"
  git clone --depth 1 --branch "$ORT_TAG" https://github.com/microsoft/onnxruntime.git "$SRC"
fi

rm -rf "$BUILD"
mkdir -p "$OUT/lib" "$BUILD"

log "building static ORT ${ORT_VERSION} for macos-${ARCH} (this takes a while)"
(
  cd "$SRC"
  ./build.sh \
    --config Release \
    --parallel \
    --skip_tests \
    --compile_no_warning_as_error \
    --build_dir "$BUILD" \
    --osx_arch "$ARCH" \
    --cmake_extra_defines \
      onnxruntime_BUILD_SHARED_LIB=OFF \
      onnxruntime_ENABLE_PYTHON=OFF \
      onnxruntime_BUILD_UNIT_TESTS=OFF \
      CMAKE_OSX_DEPLOYMENT_TARGET=14.0
)

# ort-sys static linking expects the Release lib directory.
libdir=""
for candidate in \
  "$BUILD/Release" \
  "$BUILD/build/Release" \
  "$BUILD/build/MacOS/Release"; do
  if find "$candidate" -name 'libonnxruntime*.a' 2>/dev/null | grep -q .; then
    libdir="$candidate"
    break
  fi
done

if [ -z "$libdir" ]; then
  log "could not find libonnxruntime*.a; listing build tree"
  find "$BUILD" -name 'libonnxruntime*.a' 2>/dev/null | head -20 || true
  die "static ORT build produced no libonnxruntime*.a under $BUILD"
fi

log "installing static libs from $libdir -> $OUT/lib"
find "$libdir" -name 'libonnxruntime*.a' -exec cp -a {} "$OUT/lib/" \;
find "$libdir" -name 'libonnxruntime*.a' | head -5
log "done — set ORT_LIB_PATH=$OUT/lib for static macOS builds"
