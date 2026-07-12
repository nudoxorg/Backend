# Toolchain packages for Buck2 via buck2.nix (https://github.com/tweag/buck2.nix).
#
# When the `nix` external cell is configured in .buckconfig, targets in
# build/toolchains/BUCK reference these packages with flake.package():
#
#   load("@nix//:flake.bzl", "flake")
#   flake.package(name = "rustc", binary = "rustc", binaries = ["rustdoc"],
#                 path = "//build/toolchains/nix:flake")
#
# flake.package runs `nix build path:<dir>#packages.<system>.<pkg>.out` as a
# Buck2 action, so the building host only needs `nix` on PATH. Running
# `nix flake lock build/toolchains/nix` pins the exact store paths in
# build/toolchains/nix/flake.lock, giving hermetic, reproducible tool versions
# whose closures can be shared cross-machine via the Attic binary cache.
{
  description = "Buck2 toolchain packages (rust + cxx) — mirrors the repo devshell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
    }:
    let
      inherit (nixpkgs) lib;
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];

      eachSystem =
        f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system} fenix.packages.${system});
    in
    {
      packages = eachSystem (
        pkgs: fenix-pkg:
        let
          # ── Rust (nightly via fenix) ────────────────────────────────────────
          # MUST mirror the devShells.default toolchain in the repo-root flake.nix
          # so rust-analyzer, the devshell build, and the Buck2 build all agree on
          # the same nightly. `complete` = latest fenix nightly (no date/toolchain
          # -file pin upstream, so none here either). Component list copied verbatim
          # from the root flake's `rust-nightly = fenix-pkg.complete.withComponents`.
          rust = fenix-pkg.complete.withComponents [
            "cargo"
            "clippy"
            "rust-src"
            "rust-docs"
            "rustc"
            # rust-analyzer component ships libexec/rust-analyzer-proc-macro-srv
            # so ra_ap_load_cargo can use ProcMacroServerChoice::Sysroot.
            "rust-analyzer"
            "llvm-tools"
            "rustfmt"
            "rustc-codegen-cranelift-preview"
          ];

          # ── Runtime/build env deps surfaced for the release binary ──────────
          # The devshell exports OPENSSL_* (and the plan flags LLVM_SYS_181_PREFIX).
          # NOTE: this repo's current dependency graph is rustls-based — there is no
          # openssl-sys / native-tls / llvm-sys edge in build/third-party/registry.bzl
          # (only openssl-probe + rustls_pki_types). openssl/llvm are therefore
          # included here defensively (so a future native-tls or llvm-sys dep builds
          # hermetically and cross-machine cacheable) and to mirror the devshell env,
          # NOT because anything links them today. See PLANS.md migration note.
          llvm = pkgs.llvmPackages_18.llvm;
        in
        {
          # Rust compiler + rustdoc (same combined toolchain derivation exposes
          # bin/rustc, bin/rustdoc, bin/clippy-driver).
          rustc = rust;
          rustdoc = rust;
          clippy = rust;

          # ── C/C++ toolchain ─────────────────────────────────────────────────
          # A single package exposing cc, c++, ar, nm, ranlib, strip, objcopy under
          # $out/bin, as required by nix_cxx_toolchain's `nix_cc` sub-targets.
          #
          # Follows the buck2.nix upstream reference (examples/toolchains/nix): wrap
          # the stdenv cc wrapper capturing the NIX_* env vars that nixpkgs' cc needs
          # at exec time, and symlink the binutils-family tools from $NIX_CC.
          #
          # On Darwin, stdenv's $NIX_CC is the clang wrapper (hermetic clang from
          # nixpkgs) — this fixes the previous buggy `pkgs.darwin.apple_sdk_11_0.xcode`
          # reference, which was not a usable cc/ar. The SYSTEM escape-hatch toolchain
          # (build/toolchains/BUCK :cxx_system) still uses /usr/bin/clang for anyone
          # opting out with -c nix.toolchain=0.
          cxx = pkgs.stdenv.mkDerivation {
            name = "buck2-cxx";
            dontUnpack = true;
            dontCheck = true;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            # Surface OPENSSL_* so cc-rs / build scripts invoked through this cc can
            # find openssl if a native-tls/openssl-sys dep is ever added. Harmless
            # when unused. Mirrors the devshell's OPENSSL_DIR/LIB_DIR/INCLUDE_DIR.
            OPENSSL_DIR = "${pkgs.openssl.dev}";
            OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
            OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";
            LLVM_SYS_181_PREFIX = "${llvm.dev or llvm}";
            buildPhase = ''
              function capture_env() {
                  # All nixpkgs cc-wrapper NIX_* vars whose names start with these
                  # prefixes are forwarded into the wrapped cc/c++ so the compiler
                  # behaves identically to an interactive nix shell invocation.
                  local -ar vars=(
                      NIX_CC_WRAPPER_TARGET_HOST_
                      NIX_CFLAGS_COMPILE
                      NIX_DONT_SET_RPATH
                      NIX_ENFORCE_NO_NATIVE
                      NIX_HARDENING_ENABLE
                      NIX_IGNORE_LD_THROUGH_GCC
                      NIX_LDFLAGS
                      NIX_NO_SELF_RPATH
                      OPENSSL_DIR
                      OPENSSL_LIB_DIR
                      OPENSSL_INCLUDE_DIR
                      LLVM_SYS_181_PREFIX
                  )
                  for prefix in "''${vars[@]}"; do
                      for v in $( eval 'echo "''${!'"$prefix"'@}"' ); do
                          echo "--set"
                          echo "$v"
                          echo "''${!v}"
                      done
                  done
              }

              mkdir -p "$out/bin"

              for tool in ar nm objcopy ranlib strip; do
                  ln -st "$out/bin" "$NIX_CC/bin/$tool"
              done

              mapfile -t < <(capture_env)

              makeWrapper "$NIX_CC/bin/$CC" "$out/bin/cc" "''${MAPFILE[@]}"
              makeWrapper "$NIX_CC/bin/$CXX" "$out/bin/c++" "''${MAPFILE[@]}"
            '';
          };

          # ── snowydeer Phase 4 — ripgrep NAR ref-scanner ────────────────────
          # Used by snowydeer/build_store_path.py to reference-scan the NAR of
          # the deploy binary, implementing Dolstra thesis §5.12 scanForReferences.
          # Exposed to Buck2 as toolchains//:ripgrep (build/toolchains/BUCK).
          #
          # TODO(human): run `nix flake lock build/toolchains/nix` after this
          # change to pin ripgrep in flake.lock, then commit the updated lock
          # file so the Buck2 action cache key is deterministic cross-machine.
          ripgrep = pkgs.ripgrep;

          # ── Tier C — tsgo (TypeScript 7 native checker) ────────────────────
          # The OXC TypeScript producer's checker oracle (OXC-PLAN §Phase 6).
          # Microsoft ships the native (Go) compiler as per-platform npm packages
          # `@typescript/native-preview-<platform>` (Apache-2.0). It is a *noembed*
          # build: `tsgo` resolves its bundled `lib.*.d.ts` relative to the real
          # executable path, so the binary and libs MUST live in the same dir —
          # we install both into $out/lib and wrap $out/bin/tsgo (the wrapper
          # execs the real $out/lib/tsgo, whose siblings are the libs).
          # Exposed to Buck2 as toolchains//:tsgo (build/toolchains/BUCK).
          tsgo =
            let
              sys = pkgs.stdenv.hostPlatform.system;
              version = "7.0.0-dev.20260707.2";
              plat =
                if sys == "aarch64-darwin" then
                  { npm = "darwin-arm64"; hash = "sha256-ptC0bXE1jbLWAjJMVTNmpY+h0DH136EsCaNba+F/5QM="; }
                else if sys == "x86_64-linux" then
                  { npm = "linux-x64"; hash = "sha256-Xm7hBSGQUmrgrCd7L7UroMfogGGSAUk71TQpdqCzPrY="; }
                else
                  throw "tsgo: unsupported system ${sys}";
            in
            pkgs.stdenvNoCC.mkDerivation {
              pname = "tsgo";
              inherit version;
              src = pkgs.fetchurl {
                url = "https://registry.npmjs.org/@typescript/native-preview-${plat.npm}/-/native-preview-${plat.npm}-${version}.tgz";
                inherit (plat) hash;
              };
              nativeBuildInputs = [ pkgs.makeWrapper ];
              dontStrip = true;
              installPhase = ''
                runHook preInstall
                mkdir -p $out/lib $out/bin
                cp -R ./lib/. $out/lib/
                chmod +x $out/lib/tsgo
                makeWrapper $out/lib/tsgo $out/bin/tsgo
                runHook postInstall
              '';
              meta.mainProgram = "tsgo";
            };
        }
      );
    };
}
