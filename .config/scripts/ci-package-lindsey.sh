#!/usr/bin/env bash
# Build and package lindsey for CI (macOS / Linux / Windows).
#
# Mirrors the contracts in nix/lindsey-bundle.nix and tests/lindsey-bundle/check.nu:
# subprocess oracles, embed model sidecars, ONNX Runtime beside the binary, and a
# launcher that sets NUDOX_* / LIBCLANG_PATH before exec'ing the release binary.
#
# Usage: ci-package-lindsey.sh <macos|macos-x64|linux|linux-arm64|windows>
# Env: LINDSEY_VERSION (optional) — stamped into the macOS Info.plist; defaults to 0.1.0.
# Output: dist/<artifact-name>/, dist/<artifact-name>.{tar.gz,zip}, and dist/<archive>.sha256

set -euo pipefail

PLATFORM="${1:?usage: ci-package-lindsey.sh <macos|macos-x64|linux|linux-arm64|windows>}"
ROOT="$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)"
ORT_VERSION="${ORT_VERSION:-1.28.0}"
DIST="$ROOT/dist"
STAGE="$DIST/stage"
ORT_DIR="$STAGE/ort"
RESOURCES="$STAGE/resources/nudox"
# Persistent CI caches (restored by actions/cache before this script runs).
EMBED_CACHE="${EMBED_CACHE_DIR:-$ROOT/.ci-cache/embed-model}"
ORT_CACHE="${ORT_CACHE_DIR:-$ROOT/.ci-cache/ort/${ORT_VERSION}/${PLATFORM}}"

# HuggingFace jina-embeddings-v2-base-code — same revision + hashes as flake.nix.
EMBED_REV="516f4baf13dec4ddddda8631e019b5737c8bc250"
EMBED_BASE="https://huggingface.co/jinaai/jina-embeddings-v2-base-code/resolve/${EMBED_REV}"

log() { printf 'ci-package-lindsey: %s\n' "$*"; }
die() { printf 'ci-package-lindsey: error: %s\n' "$*" >&2; exit 1; }

case "$PLATFORM" in
  macos|macos-x64|linux|linux-arm64|windows) ;;
  *) die "unknown platform: $PLATFORM" ;;
esac

RUST_TARGET=""
ORT_STATIC=0
case "$PLATFORM" in
  macos) RUST_TARGET="aarch64-apple-darwin" ;;
  macos-x64) RUST_TARGET="x86_64-apple-darwin"; ORT_STATIC=1 ;;
esac

# ── Known-good hashes for fetched inputs ────────────────────────────────────
# fetch_embed/fetch_ort below pull straight from HuggingFace/GitHub with no
# other integrity check upstream of this script. checksum_archive() (near
# the bottom) only proves "this is what CI produced" — it says nothing about
# whether what CI *started from* was legitimate. A compromised CDN edge or a
# MITM on either fetch would otherwise ship a backdoored binary carrying a
# checksum that faithfully validates it. These are the same plain-hex
# sha256 values already pinned for the same inputs in ../../flake.nix
# (`onnxruntimeLib`'s distBySystem, and `semanticModel`) — kept as hex here
# since this script has no Nix SRI (base64) decoder. If ORT_VERSION or
# EMBED_REV above is ever bumped, these must be updated in lockstep, or
# every fetch will (correctly) fail closed with a checksum mismatch instead
# of silently accepting a different version's bytes.
ort_sha256_for() {
  case "$1" in
    macos) echo "1268b359718099bde2cedb55787f182a130067bc4f31e8c88478c445b850d3d8" ;;
    linux) echo "a3e1b79d7bb1bf09696ce675f49e4064e6c81f6202b8225624fff0e93f8d6407" ;;
    linux-arm64) echo "e15ff8b5d85afe6c144d97c6fd432254bf76a219daaf17658087d6ecb3e8f0bb" ;;
    windows) echo "abef733dacbe2f571547a7150b479b5cb9cc0df22f96c24983a42cadb1b4f8bc" ;;
    *) die "ort_sha256_for: no pinned hash for platform '$1' (ORT_VERSION=$ORT_VERSION)" ;;
  esac
}

embed_sha256_for() {
  case "$1" in
    model.onnx) echo "63363fc178428b74620c6f3780cbc7191883fa5c7f84c0945c45eb5c4256733b" ;;
    tokenizer.json) echo "b01c78a902aa4facb2f47f95449f48e2f7bbfea5d2472ee2f6ce92323c6f86e5" ;;
    config.json) echo "e426aa684c7f9a95c5f020aa855faf93a24f065f5fad0c9e17b124670cabdea6" ;;
    special_tokens_map.json) echo "06e405a36dfe4b9604f484f6a1e619af1a7f7d09e34a8555eb0b77b66318067f" ;;
    tokenizer_config.json) echo "f477aeb15ff9f78d3c1ddf2361d2b0b8b20cf55220f839f29a37f3a18efddd89" ;;
    *) die "embed_sha256_for: no pinned hash for file '$1' (EMBED_REV=$EMBED_REV)" ;;
  esac
}

# Same macOS-vs-Linux/Windows tool fallback as checksum_archive() below.
verify_sha256() {
  local file="$1" expected="$2" actual
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$file" | awk '{print $1}')"
  else
    actual="$(shasum -a 256 "$file" | awk '{print $1}')"
  fi
  [ "$actual" = "$expected" ] || die "checksum mismatch for $file: expected $expected, got $actual"
}

rm -rf "$STAGE"
mkdir -p "$ORT_DIR" "$RESOURCES" "$DIST" "$EMBED_CACHE" "$(dirname "$ORT_CACHE")"

# ── Embed model (5 files required by nudox-engine/src/embed/mod.rs) ───────────
fetch_embed() {
  local dest="$1"
  mkdir -p "$dest"
  local -a files=(
    "onnx/model.onnx"
    "tokenizer.json"
    "config.json"
    "special_tokens_map.json"
    "tokenizer_config.json"
  )
  for rel in "${files[@]}"; do
    local base
    base="$(basename "$rel")"
    local out="$dest/$base"
    if [ ! -e "$out" ]; then
      log "fetching embed model $base"
      curl -fsSL "${EMBED_BASE}/${rel}" -o "$out"
    fi
    # Verified unconditionally, not just on a fresh fetch: $out may also
    # come from EMBED_CACHE_DIR (restored via actions/cache in CI), and a
    # poisoned cache entry is exactly as dangerous as a poisoned network
    # fetch would be.
    verify_sha256 "$out" "$(embed_sha256_for "$base")"
  done
  [ -e "$dest/model.onnx" ] || die "embed model missing model.onnx"
}

# ── ONNX Runtime 1.28 (ort-sys pin) ─────────────────────────────────────────
fetch_ort() {
  if [ "$ORT_STATIC" = 1 ]; then
    bash "$ROOT/.config/scripts/build-ort-static-macos.sh" x86_64
    ORT_DIR="$ROOT/.ci-cache/ort-static/${ORT_VERSION}/macos-x86_64"
    mkdir -p "$STAGE"
    return
  fi

  if [ -d "$ORT_CACHE/lib" ] && { find "$ORT_CACHE/lib" -name 'libonnxruntime*' | grep -q . || find "$ORT_CACHE/lib" -name 'onnxruntime.dll' | grep -q .; }; then
    log "using cached ONNX Runtime at $ORT_CACHE"
    cp -a "$ORT_CACHE/." "$ORT_DIR/"
    return
  fi
  mkdir -p "$ORT_CACHE"
  local expected
  expected="$(ort_sha256_for "$PLATFORM")"
  # Downloaded to a file and verified BEFORE extraction (not streamed
  # straight into tar) specifically so a bad checksum aborts before any of
  # it lands on disk as trusted input.
  case "$PLATFORM" in
    macos)
      local url="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-osx-arm64-${ORT_VERSION}.tgz"
      curl -fsSL "$url" -o "$ORT_CACHE/ort.tgz"
      verify_sha256 "$ORT_CACHE/ort.tgz" "$expected"
      tar -xz -C "$ORT_CACHE" --strip-components=1 -f "$ORT_CACHE/ort.tgz"
      rm -f "$ORT_CACHE/ort.tgz"
      ;;
    linux)
      local url="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-linux-x64-${ORT_VERSION}.tgz"
      curl -fsSL "$url" -o "$ORT_CACHE/ort.tgz"
      verify_sha256 "$ORT_CACHE/ort.tgz" "$expected"
      tar -xz -C "$ORT_CACHE" --strip-components=1 -f "$ORT_CACHE/ort.tgz"
      rm -f "$ORT_CACHE/ort.tgz"
      ;;
    linux-arm64)
      local url="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-linux-aarch64-${ORT_VERSION}.tgz"
      curl -fsSL "$url" -o "$ORT_CACHE/ort.tgz"
      verify_sha256 "$ORT_CACHE/ort.tgz" "$expected"
      tar -xz -C "$ORT_CACHE" --strip-components=1 -f "$ORT_CACHE/ort.tgz"
      rm -f "$ORT_CACHE/ort.tgz"
      ;;
    windows)
      local url="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-win-x64-${ORT_VERSION}.zip"
      curl -fsSL "$url" -o "$ORT_CACHE/ort.zip"
      verify_sha256 "$ORT_CACHE/ort.zip" "$expected"
      unzip -q "$ORT_CACHE/ort.zip" -d "$ORT_CACHE"
      rm -f "$ORT_CACHE/ort.zip"
      local inner
      inner="$(find "$ORT_CACHE" -maxdepth 1 -type d -name 'onnxruntime-*' | head -n 1)"
      if [ -n "$inner" ] && [ -d "$inner/lib" ]; then
        mkdir -p "$ORT_CACHE/lib"
        cp -R "$inner/lib/." "$ORT_CACHE/lib/"
      fi
      ;;
  esac
  cp -a "$ORT_CACHE/." "$ORT_DIR/"
  find "$ORT_DIR" -name 'libonnxruntime*' -o -name 'onnxruntime.dll' | head -n 1 >/dev/null \
    || die "ONNX Runtime extract failed under $ORT_CACHE"
}

# ── Subprocess oracles ────────────────────────────────────────────────────────
build_go_oracle() {
  local out="$1"
  local dir="$ROOT/workspace/compiler/languages/oracle/go"
  log "building go oracle"
  (
    cd "$dir"
    export GOPROXY="${GOPROXY:-https://proxy.golang.org,direct}"
    go mod vendor
    go build -mod=vendor -o "$out" .
  )
  chmod +x "$out" 2>/dev/null || true
}

build_java_oracle() {
  local out="$1"
  local dir="$ROOT/workspace/compiler/languages/oracle/java"
  log "building java oracle classes"
  mkdir -p "$out"
  javac -encoding UTF-8 -d "$out" "$dir/Extractor.java" "$dir/Json.java"
  [ -e "$out/nudox/oracle/Extractor.class" ] \
    || die "java oracle missing nudox/oracle/Extractor.class"
}

build_csharp_oracle() {
  local out="$1"
  local dir="$ROOT/workspace/compiler/languages/oracle/csharp"
  log "building csharp oracle"
  mkdir -p "$out"
  (
    cd "$dir"
    dotnet publish oracle.csproj -c Release -o "$out"
  )
  [ -e "$out/oracle.dll" ] || die "csharp oracle missing oracle.dll"
}

copy_resources() {
  build_go_oracle "$RESOURCES/nudox-go-oracle"
  rm -rf "$RESOURCES/java-oracle"
  build_java_oracle "$RESOURCES/java-oracle"
  rm -rf "$RESOURCES/csharp-oracle"
  mkdir -p "$RESOURCES/csharp-oracle"
  build_csharp_oracle "$RESOURCES/csharp-oracle"
  rm -rf "$RESOURCES/embed-model"
  fetch_embed "$EMBED_CACHE"
  mkdir -p "$RESOURCES/embed-model"
  cp -a "$EMBED_CACHE/." "$RESOURCES/embed-model/"
}

# ── lindsey release binary ──────────────────────────────────────────────────
build_lindsey() {
  local gui="$ROOT/workspace/gui"
  local ort_lib
  ort_lib="$(find "$ORT_DIR" -type d -name lib | head -n 1)"
  [ -n "$ort_lib" ] || ort_lib="$ORT_DIR/lib"
  [ -d "$ort_lib" ] || die "ORT lib dir not found under $ORT_DIR"

  if [ "$ORT_STATIC" = 1 ]; then
    export ORT_LIB_PATH="$ort_lib"
    unset ORT_LIB_LOCATION ORT_PREFER_DYNAMIC_LINK
  else
    export ORT_LIB_LOCATION="$ort_lib"
    export ORT_PREFER_DYNAMIC_LINK=1
    unset ORT_LIB_PATH
  fi
  export CARGO_PROFILE_RELEASE_STRIP=symbols

  if [ -z "${LIBCLANG_PATH:-}" ]; then
    case "$PLATFORM" in
      macos|macos-x64)
        if [ -d /opt/homebrew/opt/llvm/lib ]; then
          export LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib
        elif [ -d /usr/local/opt/llvm/lib ]; then
          export LIBCLANG_PATH=/usr/local/opt/llvm/lib
        fi
        ;;
      linux|linux-arm64)
        if [ -d /usr/lib/llvm-18/lib ]; then
          export LIBCLANG_PATH=/usr/lib/llvm-18/lib
        elif [ -d /usr/lib/llvm-17/lib ]; then
          export LIBCLANG_PATH=/usr/lib/llvm-17/lib
        elif [ "$PLATFORM" = "linux-arm64" ] && [ -d /usr/lib/aarch64-linux-gnu ]; then
          export LIBCLANG_PATH=/usr/lib/aarch64-linux-gnu
        elif [ -d /usr/lib/x86_64-linux-gnu ]; then
          export LIBCLANG_PATH=/usr/lib/x86_64-linux-gnu
        fi
        ;;
      windows)
        if [ -n "${LLVM_PATH:-}" ] && [ -d "$LLVM_PATH/lib" ]; then
          export LIBCLANG_PATH="$LLVM_PATH/lib"
        fi
        ;;
    esac
  fi

  if [ -n "$RUST_TARGET" ]; then
    log "building lindsey for $RUST_TARGET (ORT_STATIC=$ORT_STATIC ORT_LIB=${ORT_LIB_PATH:-${ORT_LIB_LOCATION:-unset}})"
    (
      cd "$gui"
      rustup target add "$RUST_TARGET" >/dev/null 2>&1 || true
      cargo build --release --locked --bin lindsey --target "$RUST_TARGET"
    )
    echo "$gui/target/$RUST_TARGET/release"
    return
  fi

  log "building lindsey (ORT_LIB_LOCATION=${ORT_LIB_LOCATION:-unset})"
  (
    cd "$gui"
    cargo build --release --locked --bin lindsey
  )
  echo "$gui/target/release"
}

# ── macOS .app ──────────────────────────────────────────────────────────────
package_macos() {
  local release_dir="$1"
  local artifact="${2:-lindsey-macos-arm64}"
  local bin="$release_dir/lindsey"
  [ -x "$bin" ] || die "missing release binary $bin"

  local app="$STAGE/lindsey.app"
  local macos="$app/Contents/MacOS"
  local frameworks="$app/Contents/Frameworks"
  local res="$app/Contents/Resources"
  mkdir -p "$macos" "$frameworks" "$res/nudox"

  cp "$bin" "$macos/.lindsey-wrapped"
  chmod +x "$macos/.lindsey-wrapped"

  if [ "$ORT_STATIC" != 1 ]; then
    # ORT dylibs into Frameworks (skip dSYM).
    cp -a "$ORT_DIR/lib/libonnxruntime.1.dylib" "$frameworks/" 2>/dev/null \
      || cp -a "$ORT_DIR/lib/"libonnxruntime*.dylib "$frameworks/" 2>/dev/null
    if [ -L "$ORT_DIR/lib/libonnxruntime.1.dylib" ]; then
      local target
      target="$(readlink "$ORT_DIR/lib/libonnxruntime.1.dylib")"
      cp -a "$ORT_DIR/lib/$target" "$frameworks/" 2>/dev/null || true
    fi

    if ! otool -l "$macos/.lindsey-wrapped" | grep -q '@executable_path/../Frameworks'; then
      install_name_tool -add_rpath '@executable_path/../Frameworks' "$macos/.lindsey-wrapped" || true
    fi
  elif otool -L "$macos/.lindsey-wrapped" | grep -q libonnxruntime; then
    die "expected static ORT link but binary still references libonnxruntime dylib"
  fi
  strip -x "$macos/.lindsey-wrapped" 2>/dev/null || true

  cp -R "$RESOURCES/." "$res/nudox/"

  cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleName</key><string>lindsey</string>
  <key>CFBundleDisplayName</key><string>lindsey</string>
  <key>CFBundleIdentifier</key><string>com.nudox.lindsey</string>
  <key>CFBundleVersion</key><string>${LINDSEY_VERSION:-0.1.0}</string>
  <key>CFBundleShortVersionString</key><string>${LINDSEY_VERSION:-0.1.0}</string>
  <key>CFBundleExecutable</key><string>lindsey</string>
  <key>CFBundleIconFile</key><string>lindsey</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

  # Icon from logo.svg when resvg + iconutil are available.
  local svg="$ROOT/workspace/gui/assets/logo.svg"
  if [ -f "$svg" ] && command -v resvg >/dev/null && command -v iconutil >/dev/null; then
    local iconset="$STAGE/lindsey.iconset"
    mkdir -p "$iconset"
    render_icon() { resvg -w "$1" -h "$1" "$svg" "$iconset/$2"; }
    render_icon 16 icon_16x16.png
    render_icon 32 icon_16x16@2x.png
    render_icon 32 icon_32x32.png
    render_icon 64 icon_32x32@2x.png
    render_icon 128 icon_128x128.png
    render_icon 256 icon_128x128@2x.png
    render_icon 256 icon_256x256.png
    render_icon 512 icon_256x256@2x.png
    render_icon 512 icon_512x512.png
    render_icon 1024 icon_512x512@2x.png
    iconutil -c icns "$iconset" -o "$res/lindsey.icns"
  fi

  cat > "$macos/lindsey" <<'WRAP'
#!/bin/sh
set -eu
self="$0"
case "$self" in /*) ;; *) self="$(CDPATH= cd -- "$(dirname "$self")" && pwd)/$(basename "$self")" ;; esac
here="$(CDPATH= cd -- "$(dirname "$self")" && pwd)"
resources="$here/../Resources/nudox"
frameworks="$here/../Frameworks"
export NUDOX_GO_ORACLE_BIN="${NUDOX_GO_ORACLE_BIN:-$resources/nudox-go-oracle}"
export NUDOX_JAVA_ORACLE_CLASSES="${NUDOX_JAVA_ORACLE_CLASSES:-$resources/java-oracle}"
export NUDOX_CSHARP_ORACLE="${NUDOX_CSHARP_ORACLE:-$resources/csharp-oracle/oracle.dll}"
export NUDOX_DOTNET="${NUDOX_DOTNET:-$(command -v dotnet)}"
export NUDOX_EMBED_MODEL_DIR="${NUDOX_EMBED_MODEL_DIR:-$resources/embed-model}"
export LIBCLANG_PATH="${LIBCLANG_PATH:-${LIBCLANG_PATH_DEFAULT:-}}"
export PATH="${GO_BIN_DIR:-}:${JAVA_BIN_DIR:-}:${DOTNET_BIN_DIR:-}:$PATH"
if [ -d "$frameworks" ]; then
  export DYLD_FALLBACK_LIBRARY_PATH="$frameworks${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"
fi
exec "$here/.lindsey-wrapped" "$@"
WRAP
  chmod +x "$macos/lindsey"

  local out="$DIST/$artifact"
  rm -rf "$out"
  cp -R "$app" "$out/lindsey.app"
  ( cd "$DIST" && tar -czf "${artifact}.tar.gz" "$artifact" )
}

# ── Linux portable tree (x64 and arm64 share layout; names differ) ───────────
package_linux() {
  local release_dir="$1"
  local artifact="$2"
  local bin="$release_dir/lindsey"
  [ -x "$bin" ] || die "missing release binary $bin"

  local tree="$STAGE/$artifact"
  mkdir -p "$tree/bin" "$tree/lib" "$tree/share/nudox"
  cp "$bin" "$tree/bin/.lindsey-wrapped"
  chmod +x "$tree/bin/.lindsey-wrapped"
  strip -x "$tree/bin/.lindsey-wrapped" 2>/dev/null || true

  cp -a "$ORT_DIR/lib/"libonnxruntime*.so* "$tree/lib/" 2>/dev/null || true
  cp -a "$ORT_DIR/lib/"libonnxruntime_providers*.so* "$tree/lib/" 2>/dev/null || true
  cp -R "$RESOURCES/." "$tree/share/nudox/"

  cat > "$tree/bin/lindsey" <<'WRAP'
#!/bin/sh
set -eu
here="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
root="$(CDPATH= cd -- "$here/.." && pwd)"
resources="$root/share/nudox"
export NUDOX_GO_ORACLE_BIN="${NUDOX_GO_ORACLE_BIN:-$resources/nudox-go-oracle}"
export NUDOX_JAVA_ORACLE_CLASSES="${NUDOX_JAVA_ORACLE_CLASSES:-$resources/java-oracle}"
export NUDOX_CSHARP_ORACLE="${NUDOX_CSHARP_ORACLE:-$resources/csharp-oracle/oracle.dll}"
export NUDOX_DOTNET="${NUDOX_DOTNET:-$(command -v dotnet)}"
export NUDOX_EMBED_MODEL_DIR="${NUDOX_EMBED_MODEL_DIR:-$resources/embed-model}"
export LIBCLANG_PATH="${LIBCLANG_PATH:-${LIBCLANG_PATH_DEFAULT:-}}"
export PATH="${GO_BIN_DIR:-}:${JAVA_BIN_DIR:-}:${DOTNET_BIN_DIR:-}:$PATH"
export LD_LIBRARY_PATH="$root/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$here/.lindsey-wrapped" "$@"
WRAP
  chmod +x "$tree/bin/lindsey"

  local out="$DIST/$artifact"
  rm -rf "$out"
  cp -R "$tree/." "$out/"
  ( cd "$DIST" && tar -czf "${artifact}.tar.gz" "$artifact" )
}

# ── Windows portable tree ───────────────────────────────────────────────────
package_windows() {
  local release_dir="$1"
  local bin="$release_dir/lindsey.exe"
  [ -x "$bin" ] || [ -f "$bin" ] || die "missing release binary $bin"

  local tree="$STAGE/lindsey-windows-x64"
  mkdir -p "$tree/nudox"
  cp "$bin" "$tree/lindsey.exe"
  cp -a "$ORT_DIR/lib/onnxruntime.dll" "$tree/" 2>/dev/null \
    || cp -a "$ORT_DIR/"onnxruntime*.dll "$tree/" 2>/dev/null || true
  cp -a "$ORT_DIR/lib/"*.dll "$tree/" 2>/dev/null || true
  cp -R "$RESOURCES/." "$tree/nudox/"

  cat > "$tree/lindsey.cmd" <<'WRAP'
@echo off
set "HERE=%~dp0"
set "RES=%HERE%nudox"
if not defined NUDOX_GO_ORACLE_BIN (
  if exist "%RES%\nudox-go-oracle.exe" (
    set "NUDOX_GO_ORACLE_BIN=%RES%\nudox-go-oracle.exe"
  ) else (
    set "NUDOX_GO_ORACLE_BIN=%RES%\nudox-go-oracle"
  )
)
if not defined NUDOX_JAVA_ORACLE_CLASSES set "NUDOX_JAVA_ORACLE_CLASSES=%RES%\java-oracle"
if not defined NUDOX_CSHARP_ORACLE set "NUDOX_CSHARP_ORACLE=%RES%\csharp-oracle\oracle.dll"
if not defined NUDOX_EMBED_MODEL_DIR set "NUDOX_EMBED_MODEL_DIR=%RES%\embed-model"
if not defined LIBCLANG_PATH if defined LIBCLANG_PATH_DEFAULT set "LIBCLANG_PATH=%LIBCLANG_PATH_DEFAULT%"
if not defined NUDOX_DOTNET set "NUDOX_DOTNET=dotnet"
if defined GO_BIN_DIR set "PATH=%GO_BIN_DIR%;%PATH%"
if defined JAVA_BIN_DIR set "PATH=%JAVA_BIN_DIR%;%PATH%"
if defined DOTNET_BIN_DIR set "PATH=%DOTNET_BIN_DIR%;%PATH%"
"%HERE%lindsey.exe" %*
WRAP

  local out="$DIST/lindsey-windows-x64"
  rm -rf "$out"
  cp -R "$tree/." "$out/"
  if command -v zip >/dev/null; then
    ( cd "$DIST" && zip -qr lindsey-windows-x64.zip lindsey-windows-x64 )
  elif command -v 7z >/dev/null; then
    ( cd "$DIST" && 7z a -tzip lindsey-windows-x64.zip lindsey-windows-x64 >/dev/null )
  fi
}

# ── Checksums ─────────────────────────────────────────────────────────────
# macOS ships `shasum -a 256`, not `sha256sum`; Linux/Windows(git-bash) ship
# `sha256sum`. Written next to the archive as `<archive>.sha256`, matching
# the `sha256sum -c` line format so a downloader can verify with either tool.
checksum_archive() {
  local archive="$1"
  local path="$DIST/$archive"
  [ -e "$path" ] || { log "no archive at $path, skipping checksum"; return; }
  (
    cd "$DIST"
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum "$archive" > "$archive.sha256"
    else
      shasum -a 256 "$archive" > "$archive.sha256"
    fi
  )
}

# ── Smoke: oracle schema handshake ──────────────────────────────────────────
smoke_oracles() {
  log "smoke: go oracle schema"
  local go_bin="$RESOURCES/nudox-go-oracle"
  if [ "$PLATFORM" = "windows" ]; then
    [ -x "$go_bin" ] || go_bin="${go_bin}.exe"
  fi
  local fixture="$STAGE/go-fixture"
  mkdir -p "$fixture"
  printf 'module example.com/fixture\ngo 1.23.0\n' > "$fixture/go.mod"
  printf 'package fixture\nfunc Hello() string { return "ok" }\n' > "$fixture/hello.go"
  local schema
  schema="$("$go_bin" "$fixture" | jq -r '.schemaVersion')"
  [ "$schema" = "2" ] || die "go oracle schema=$schema expected 2"

  log "smoke: csharp oracle format"
  local csharp_fixture="$STAGE/csharp-fixture"
  mkdir -p "$csharp_fixture"
  printf 'namespace Fixture;\npublic class Hello { public static string Greet() => "ok"; }\n' \
    > "$csharp_fixture/Hello.cs"
  local fmt
  fmt="$(dotnet "$RESOURCES/csharp-oracle/oracle.dll" --mode source --root "$csharp_fixture" --assembly-name fixture 2>/dev/null \
    | jq -r '.format')"
  [ "$fmt" = "1" ] || die "csharp oracle format=$fmt expected 1"
}

# ── Main ────────────────────────────────────────────────────────────────────
log "platform=$PLATFORM root=$ROOT"
fetch_ort
copy_resources
smoke_oracles
release_dir="$(build_lindsey)"

case "$PLATFORM" in
  macos) package_macos "$release_dir" lindsey-macos-arm64; checksum_archive lindsey-macos-arm64.tar.gz ;;
  macos-x64) package_macos "$release_dir" lindsey-macos-x64; checksum_archive lindsey-macos-x64.tar.gz ;;
  linux) package_linux "$release_dir" lindsey-linux-x64; checksum_archive lindsey-linux-x64.tar.gz ;;
  linux-arm64) package_linux "$release_dir" lindsey-linux-arm64; checksum_archive lindsey-linux-arm64.tar.gz ;;
  windows) package_windows "$release_dir"; checksum_archive lindsey-windows-x64.zip ;;
esac

log "done — artifacts in $DIST"
ls -la "$DIST"
