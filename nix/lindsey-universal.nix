# Fat macOS .app: arm64 + x86_64 Mach-Os in one bundle.
#
# `lindsey-app` is a native-arch Nix wrap (store-path toolchains). A
# distributable universal binary cannot prepend aarch64 store paths — Intel
# cannot exec them — so this derivation copies the arm64 app, lipos the
# GUI / ORT / Go oracle, and replaces the launcher with a PATH-based wrap
# (same contract as .config/scripts/ci-package-lindsey.sh).
{
  pkgs,
  arm64App,
  x86_64Bin,
  x86_64Ort,
  goOracleAmd64,
}:

pkgs.stdenvNoCC.mkDerivation {
  pname = "lindsey-universal";
  version = arm64App.version or "0.1.0";

  dontUnpack = true;
  dontConfigure = true;
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    cp -R "${arm64App}/lindsey.app" "$out/lindsey.app"
    chmod -R u+w "$out/lindsey.app"

    app="$out/lindsey.app"
    macos="$app/Contents/MacOS"
    frameworks="$app/Contents/Frameworks"
    resources="$app/Contents/Resources/nudox"

    wrapped="$macos/.lindsey-wrapped"
    if [ ! -f "$wrapped" ]; then
      echo "arm64 app has no Contents/MacOS/.lindsey-wrapped" >&2
      ls -la "$macos" >&2
      exit 1
    fi

    x86_bin="${x86_64Bin}/bin/lindsey"
    [ -f "$x86_bin" ] || exit 1

    /usr/bin/lipo -create "$wrapped" "$x86_bin" -output "$TMPDIR/lindsey.fat"
    mv "$TMPDIR/lindsey.fat" "$wrapped"
    chmod +x "$wrapped"
    if ! /usr/bin/otool -l "$wrapped" | grep -q '@executable_path/../Frameworks'; then
      /usr/bin/install_name_tool -add_rpath '@executable_path/../Frameworks' "$wrapped"
    fi
    /usr/bin/strip -x "$wrapped" || true

    archs="$(/usr/bin/lipo -info "$wrapped")"
    echo "$archs" | grep -q 'x86_64' || { echo "GUI missing x86_64: $archs" >&2; exit 1; }
    echo "$archs" | grep -q 'arm64' || { echo "GUI missing arm64: $archs" >&2; exit 1; }

    arm_ort="$frameworks/libonnxruntime.1.dylib"
    x86_ort="${x86_64Ort}/lib/libonnxruntime.1.dylib"
    [ -e "$arm_ort" ] || { echo "arm64 app missing $arm_ort" >&2; exit 1; }
    [ -e "$x86_ort" ] || { echo "x86_64 ORT missing $x86_ort" >&2; exit 1; }

    # Resolve symlinks so lipo sees two real Mach-Os.
    arm_ort_real="$(/usr/bin/python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$arm_ort")"
    x86_ort_real="$(/usr/bin/python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$x86_ort")"
    /usr/bin/lipo -create "$arm_ort_real" "$x86_ort_real" -output "$TMPDIR/ort.fat"
    rm -f "$frameworks/"libonnxruntime*.dylib
    cp "$TMPDIR/ort.fat" "$frameworks/libonnxruntime.1.dylib"
    ort_archs="$(/usr/bin/lipo -info "$frameworks/libonnxruntime.1.dylib")"
    echo "$ort_archs" | grep -q 'x86_64' || { echo "ORT missing x86_64: $ort_archs" >&2; exit 1; }
    echo "$ort_archs" | grep -q 'arm64' || { echo "ORT missing arm64: $ort_archs" >&2; exit 1; }

    go_arm="$resources/nudox-go-oracle"
    go_x86="${goOracleAmd64}/bin/nudox-go-oracle"
    [ -f "$go_arm" ] || { echo "arm64 app missing go oracle" >&2; exit 1; }
    [ -f "$go_x86" ] || { echo "amd64 go oracle missing" >&2; exit 1; }
    /usr/bin/lipo -create "$go_arm" "$go_x86" -output "$TMPDIR/go.fat"
    mv "$TMPDIR/go.fat" "$go_arm"
    chmod +x "$go_arm"

    cat > "$macos/lindsey" <<'WRAP'
    #!/bin/sh
    set -eu
    self="$0"
    case "$self" in /*) ;; *) self="$(CDPATH= cd -- "$(dirname "$self")" && pwd)/$(basename "$self")" ;; esac
    here="$(CDPATH= cd -- "$(dirname "$self")" && pwd)"
    resources="$here/../Resources/nudox"
    frameworks="$here/../Frameworks"
    export NUDOX_GO_ORACLE_BIN="''${NUDOX_GO_ORACLE_BIN:-$resources/nudox-go-oracle}"
    export NUDOX_JAVA_ORACLE_CLASSES="''${NUDOX_JAVA_ORACLE_CLASSES:-$resources/java-oracle}"
    export NUDOX_CSHARP_ORACLE="''${NUDOX_CSHARP_ORACLE:-$resources/csharp-oracle/oracle.dll}"
    export NUDOX_DOTNET="''${NUDOX_DOTNET:-$(command -v dotnet || true)}"
    export NUDOX_EMBED_MODEL_DIR="''${NUDOX_EMBED_MODEL_DIR:-$resources/embed-model}"
    if [ -z "''${LIBCLANG_PATH:-}" ]; then
      if [ -d /opt/homebrew/opt/llvm/lib ]; then
        export LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib
      elif [ -d /usr/local/opt/llvm/lib ]; then
        export LIBCLANG_PATH=/usr/local/opt/llvm/lib
      fi
    fi
    if [ -d "$frameworks" ]; then
      export DYLD_FALLBACK_LIBRARY_PATH="$frameworks''${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"
    fi
    exec "$here/.lindsey-wrapped" "$@"
    WRAP
    chmod +x "$macos/lindsey"

    runHook postInstall
  '';

  meta = {
    description = "lindsey.app universal (arm64 + x86_64)";
    platforms = [ "aarch64-darwin" ];
  };
}
