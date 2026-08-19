# Wrap `cargo bundle` so a packaged lindsey.app carries the subprocess
# oracles, toolchains, and embed model that `cargo bundle` itself does not
# know about.
#
# cargo-bundle only copies the GUI binary and the icon/DMG metadata in
# workspace/gui/Cargo.toml. Go/Java/C# oracles are separate artifacts, the
# embed model is a Nix-pinned directory, `go`/`javadoc`/`dotnet` have to
# be on PATH, and the GUI's @rpath/libonnxruntime.1.dylib is not a Cargo
# resource. This file is the declarative seam: one wrapper, one set of
# environment names, one Frameworks copy of ONNX Runtime.
{
  pkgs,
  cargoBundle,
  goOracle,
  javaOracle,
  csharpOracle,
  semanticModel,
  goToolchain,
  jdk,
  libclang,
  dotnet,
  # Official ONNX Runtime 1.28 lib dir (dylibs). Null on platforms that
  # do not pin a tarball; the wrap then skips Contents/Frameworks.
  onnxruntimeLib ? null,
  # When true, ORT is linked statically — no Contents/Frameworks copy.
  ortStaticLink ? false,
}:

let
  inherit (pkgs) lib;

  # The environment the wrapper injects. Names must match the producer
  # lookups in workspace/compiler/languages (ORACLE_BIN_ENV,
  # ORACLE_CLASSES_ENV, ORACLE_PATH_ENV, DOTNET_ENV, MODEL_DIR_ENV,
  # LIBCLANG_PATH). tests/lindsey-bundle/check.nu greps the installed
  # wrapper for every key.
  envNames = {
    goOracle = "NUDOX_GO_ORACLE_BIN";
    javaOracle = "NUDOX_JAVA_ORACLE_CLASSES";
    csharpOracle = "NUDOX_CSHARP_ORACLE";
    dotnet = "NUDOX_DOTNET";
    embedModel = "NUDOX_EMBED_MODEL_DIR";
    libclang = "LIBCLANG_PATH";
  };

  toolchainPath = lib.concatStringsSep ":" [
    "${goToolchain}/bin"
    "${jdk}/bin"
    "${dotnet}/bin"
  ];

  # Runtime wrapper dropped in as Contents/MacOS/lindsey. Store paths are
  # baked by Nix; `$here` / user overrides are expanded when lindsey starts.
  # `$0` is the absolute bundle executable (Finder cwd is $HOME).
  macosWrapper = pkgs.writeText "lindsey-macos-wrapper" ''
    #!/bin/sh
    set -eu
    self="$0"
    case "$self" in
      /*) ;;
      *) self="$(CDPATH= cd -- "$(dirname "$self")" && pwd)/$(basename "$self")" ;;
    esac
    here="$(CDPATH= cd -- "$(dirname "$self")" && pwd)"
    resources="$here/../Resources/nudox"
    frameworks="$here/../Frameworks"
    # User-set values win so NUDOX_PACKAGE_ROOT / a hand-built oracle still
    # work. The bundled copies are the defaults that make a Go/Java/C# package
    # load without a scavenger hunt for NUDOX_*_ORACLE_*.
    export ${envNames.goOracle}="''${${envNames.goOracle}:-$resources/nudox-go-oracle}"
    export ${envNames.javaOracle}="''${${envNames.javaOracle}:-$resources/java-oracle}"
    export ${envNames.csharpOracle}="''${${envNames.csharpOracle}:-$resources/csharp-oracle/oracle.dll}"
    export ${envNames.dotnet}="''${${envNames.dotnet}:-${dotnet}/bin/dotnet}"
    export ${envNames.embedModel}="''${${envNames.embedModel}:-$resources/embed-model}"
    export ${envNames.libclang}="''${${envNames.libclang}:-${libclang}/lib}"
    export PATH="${toolchainPath}:$PATH"
    # Belt for @rpath/libonnxruntime.1.dylib. The Mach-O also has
    # LC_RPATH=@executable_path/../Frameworks; DYLD_FALLBACK is what SIP
    # is less likely to strip than DYLD_LIBRARY_PATH when Finder launches.
    if [ -d "$frameworks" ]; then
      export DYLD_FALLBACK_LIBRARY_PATH="$frameworks''${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"
    fi
    exec "$here/.lindsey-wrapped" "$@"
  '';

  # In-place wrap of one .app. Copies oracle artifacts and the embed model
  # into Resources/nudox so the bundle is not a dangling pointer at a
  # build-machine OUT_DIR or a Nix store path the user never installed.
  installWrapper = pkgs.writeShellApplication {
    name = "lindsey-install-wrapper";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.file
    ];
    text = ''
      set -euo pipefail

      if [ "$#" -lt 1 ]; then
        echo "usage: lindsey-install-wrapper <lindsey.app>" >&2
        exit 2
      fi

      app="$1"
      macos="$app/Contents/MacOS"
      bin="$macos/lindsey"
      if [ ! -e "$bin" ]; then
        echo "lindsey-install-wrapper: no Contents/MacOS/lindsey in $app" >&2
        exit 1
      fi

      resources="$app/Contents/Resources/nudox"
      mkdir -p "$resources"

      cp -L "${goOracle}/bin/nudox-go-oracle" "$resources/nudox-go-oracle"
      chmod +x "$resources/nudox-go-oracle"
      rm -rf "$resources/java-oracle"
      mkdir -p "$resources/java-oracle"
      cp -R "${javaOracle}/." "$resources/java-oracle/"

      rm -rf "$resources/csharp-oracle"
      mkdir -p "$resources/csharp-oracle"
      csharp_root="${csharpOracle}"
      if [ -d "$csharp_root/lib" ]; then
        csharp_lib="$(find "$csharp_root/lib" -name oracle.dll | head -n 1)"
        if [ -n "$csharp_lib" ]; then
          cp -R "$(dirname "$csharp_lib")/." "$resources/csharp-oracle/"
        fi
      fi
      if [ ! -e "$resources/csharp-oracle/oracle.dll" ]; then
        echo "lindsey-install-wrapper: csharp oracle package has no oracle.dll" >&2
        find "$csharp_root" -type f >&2 || true
        exit 1
      fi

      rm -rf "$resources/embed-model"
      mkdir -p "$resources/embed-model"
      cp -R "${semanticModel}/." "$resources/embed-model/"
      if [ ! -e "$resources/embed-model/model.onnx" ]; then
        echo "lindsey-install-wrapper: embed model is missing model.onnx" >&2
        exit 1
      fi

      unwrapped="$macos/.lindsey-wrapped"
      if [ -e "$unwrapped" ]; then
        rm -f "$bin"
        cp "$unwrapped" "$bin"
        chmod +x "$bin"
      else
        mv "$bin" "$unwrapped"
      fi

      chmod u+w "$unwrapped" || true

      ${lib.optionalString (onnxruntimeLib != null && !ortStaticLink) ''
        # The GUI links @rpath/libonnxruntime.1.dylib. cargo-bundle (and the
        # handwritten .app assemble) never copy that dylib, so dyld aborts
        # before main. Ship the loader name + its target; skip .dSYM (48MB of
        # ONNX debug info) and the duplicate unversioned 38MB copy.
        frameworks="$app/Contents/Frameworks"
        mkdir -p "$frameworks"
        ort="${onnxruntimeLib}"
        if [ -e "$ort/libonnxruntime.1.dylib" ]; then
          cp -a "$ort/libonnxruntime.1.dylib" "$frameworks/"
          if [ -L "$ort/libonnxruntime.1.dylib" ]; then
            target="$(readlink "$ort/libonnxruntime.1.dylib")"
            case "$target" in
              /*) cp -a "$target" "$frameworks/" ;;
              *) cp -a "$ort/$target" "$frameworks/" ;;
            esac
          fi
        else
          echo "lindsey-install-wrapper: $ort has no libonnxruntime.1.dylib" >&2
          ls -la "$ort" >&2 || true
          exit 1
        fi
        chmod -R u+w "$frameworks"
      ''}

      if file "$unwrapped" | grep -q 'Mach-O'; then
        # Release profile already ran; this only drops the symbol table so
        # Get Info is not a 73MB unstripped Mach-O next to a 612MB model.
        /usr/bin/strip -x "$unwrapped" || true
        ${lib.optionalString (onnxruntimeLib != null && !ortStaticLink && pkgs.stdenv.isDarwin) ''
          if ! /usr/bin/otool -l "$unwrapped" | grep -q '@executable_path/../Frameworks'; then
            /usr/bin/install_name_tool -add_rpath '@executable_path/../Frameworks' "$unwrapped"
          fi
        ''}
      fi

      cp ${macosWrapper} "$bin"
      chmod +x "$bin"
    '';
  };

  lindseyBundle = pkgs.writeShellApplication {
    name = "lindsey-bundle";
    runtimeInputs = [
      cargoBundle
      installWrapper
      pkgs.coreutils
    ];
    text = ''
      set -euo pipefail

      # cargo-bundle reads [package.metadata.bundle] relative to this
      # crate, never the root workspace (see workspace/gui/Cargo.toml).
      root="''${PRJ_ROOT:-}"
      if [ -z "$root" ] && command -v git >/dev/null 2>&1; then
        root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
      fi
      if [ -n "$root" ] && [ -f "$root/workspace/gui/Cargo.toml" ]; then
        cd "$root/workspace/gui"
      elif [ -f ./workspace/gui/Cargo.toml ]; then
        cd ./workspace/gui
      elif [ -f ./Cargo.toml ] && grep -q 'name = "lindsey"' ./Cargo.toml; then
        :
      else
        echo "lindsey-bundle: run from the repo (or set PRJ_ROOT) so workspace/gui is findable" >&2
        exit 1
      fi

      if [ "$#" -eq 0 ]; then
        case "$(uname -s)" in
          Darwin) set -- --release --format osx ;;
          *) set -- --release ;;
        esac
      fi

      cargo bundle "$@"

      found=0
      for app in target/release/bundle/osx/lindsey.app target/release/bundle/dmg/lindsey.app; do
        if [ -d "$app" ]; then
          lindsey-install-wrapper "$app"
          found=1
        fi
      done
      if [ "$found" -eq 0 ]; then
        echo "lindsey-bundle: cargo bundle produced no lindsey.app under target/release/bundle/{osx,dmg}" >&2
        exit 1
      fi
    '';
  };

  wrapLindseyApp =
    app:
    pkgs.stdenv.mkDerivation {
      name = "lindsey-app-wrapped";
      dontUnpack = true;
      nativeBuildInputs = [ installWrapper ];
      buildPhase = ''
        runHook preBuild
        mkdir -p "$out"
        cp -R ${app} "$out/lindsey.app"
        chmod -R u+w "$out/lindsey.app"
        lindsey-install-wrapper "$out/lindsey.app"
        runHook postBuild
      '';
      installPhase = "true";
    };
in
{
  inherit
    envNames
    macosWrapper
    installWrapper
    lindseyBundle
    wrapLindseyApp
    toolchainPath
    ;
}
