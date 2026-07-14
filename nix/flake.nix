{
  description = "NuDox Buck2 toolchain packages";

  # Inputs use the same URLs as the root flake.nix; nix/flake.lock pins them
  # to the exact same revisions via nix flake lock nix/ after nix flake update.
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, nixpkgs, fenix }:
    let
      supportedSystems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
    in
    {
      packages = nixpkgs.lib.genAttrs supportedSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };

          # Exact same component set as devshell's rustNightlyToolchain in flake.nix.
          # Identical derivation hash → one Nix store closure shared by both consumers.
          rustNightlyToolchain = pkgs.fenix.complete.withComponents [
            "cargo"
            "clippy"
            "rust-src"
            "rust-docs"
            "rustc"
            "rustfmt"
            "rustc-codegen-cranelift-preview"
          ];
        in
        {
          # flake.package(name = "rustc", binary = "rustc", binaries = ["rustdoc"], ...)
          # rustdoc ships inside the rustc component, so the combined toolchain covers both.
          rustc = rustNightlyToolchain;

          # flake.package(name = "clippy", binary = "clippy-driver", package = "clippy", ...)
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

          # flake.package(name = "ripgrep_nix", binary = "rg", package = "ripgrep", ...)
          ripgrep = pkgs.ripgrep;
        }
      );
    };
}
