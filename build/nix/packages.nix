/*
  Package set for the NuDox Backend — imported by the root flake.nix.

  This is intentionally NOT a flake. The root flake owns inputs/lock; this file
  is a plain function that returns packages once it has been given `pkgs`,
  toolchains, and image builders.

  Buck2 hermetic toolchains still use the thin flake at build/toolchains/nix
  (via flake.package). That sub-flake mirrors the rust/cxx/ripgrep attr names
  below so both consumers share the same derivation shapes.
*/
{
  pkgs,
  fenixPackages,
  nixos,
  buildImage,
  src,
  version,
  # Optional snowydeer-imported store paths for the compiler image path.
  # When empty (pure eval), compiler-daemon emits placeholder scripts.
  compilerDaemonPath ? null,
  producerWorkerPath ? null,
  goOraclePath ? null,
  javaOraclePath ? null,
  csharpOraclePath ? null,
}:

let
  inherit (pkgs) lib callPackage;

  rustNightlyToolchain = fenixPackages.complete.withComponents [
    "cargo"
    "clippy"
    "rust-src"
    "rust-docs"
    "rustc"
    "rustfmt"
    "rustc-codegen-cranelift-preview"
  ];

  rustService = nixos.lib.build.rustService { inherit pkgs; };

  # ── Service binaries ──────────────────────────────────────────────────────
  server = rustService {
    pname = "nudox-backend";
    inherit version src;
    cargoPackage = "server";
    mainProgram = "server";
    description = "NuDox backend server";
  };

  registry =
    (rustService {
      pname = "nudox-registry";
      inherit version src;
      cargoPackage = "registry";
      mainProgram = "";
      description = "NuDox registry library";
    }).overrideAttrs
      {
        postFixup = "";
      };

  compiler-daemon = callPackage ./compiler.nix {
    compilerDaemon = compilerDaemonPath;
    producerWorker = producerWorkerPath;
    goOracle = goOraclePath;
    javaOracle = javaOraclePath;
    csharpOracle = csharpOraclePath;
  };

  # ── Container images (nix2container) ──────────────────────────────────────
  backend = callPackage ./backend-image.nix {
    inherit buildImage;
    inherit server;
  };

  compilerImage = callPackage ./compiler-image.nix {
    inherit buildImage;
    compiler = compiler-daemon;
    bwrap = if builtins.hasAttr "bwrap" pkgs then pkgs.bwrap else null;
  };

  # ── Buck2 toolchain packages (mirrored by build/toolchains/nix flake) ─────
  # flake.package(name = "rustc", binary = "rustc", binaries = ["rustdoc"], ...)
  rustc = rustNightlyToolchain;
  clippy = rustNightlyToolchain;

  # flake.package(name = "nix_cc", binaries = ["ar","cc","c++","nm","objcopy","ranlib","strip"], ...)
  # Runs inside stdenv.cc's build env; $NIX_CC/bin/ holds the wrapped tools.
  # On Darwin, objcopy is unavailable natively; fall back to llvm-objcopy if present.
  cxx = pkgs.stdenv.mkDerivation {
    name = "buck2-cxx-tools";
    dontUnpack = true;
    dontCheck = true;
    nativeBuildInputs = [ pkgs.makeWrapper ];
    buildPhase = ''
      function capture_env() {
        local -ar vars=(
          NIX_CC_WRAPPER_TARGET_HOST_
          NIX_CFLAGS_COMPILE
          NIX_DONT_SET_RPATH
          NIX_ENFORCE_NO_NATIVE
          NIX_HARDENING_ENABLE
          NIX_IGNORE_LD_THROUGH_GCC
          NIX_LDFLAGS
          NIX_NO_SELF_RPATH
        )
        for prefix in "''${vars[@]}"; do
          for v in $(eval 'echo "''${!'"$prefix"'@}"'); do
            echo "--set"
            echo "$v"
            echo "''${!v}"
          done
        done
      }

      mkdir -p "$out/bin"

      for tool in ar nm ranlib strip; do
        ln -st "$out/bin" "$NIX_CC/bin/$tool"
      done

      if [ -e "$NIX_CC/bin/objcopy" ]; then
        ln -st "$out/bin" "$NIX_CC/bin/objcopy"
      elif [ -e "$NIX_CC/bin/llvm-objcopy" ]; then
        ln -s "$NIX_CC/bin/llvm-objcopy" "$out/bin/objcopy"
      else
        printf '#!/bin/sh\nexec llvm-objcopy "$@"\n' > "$out/bin/objcopy"
        chmod +x "$out/bin/objcopy"
      fi

      mapfile -t < <(capture_env)

      makeWrapper "$NIX_CC/bin/$CC"  "$out/bin/cc"  "''${MAPFILE[@]}"
      makeWrapper "$NIX_CC/bin/$CXX" "$out/bin/c++" "''${MAPFILE[@]}"
    '';

    installPhase = "true";
  };

  ripgrep = pkgs.ripgrep;
in
{
  # Default = runnable server binary (nixos getExe / local `nix run`).
  default = server;

  inherit
    server
    registry
    compiler-daemon
    backend
    compilerImage
    rustc
    clippy
    cxx
    ripgrep
    ;
}
