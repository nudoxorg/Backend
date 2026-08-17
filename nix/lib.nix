/*
  Shared Nix helpers for NuDox packaging and checks.

  App package recipes live under workspace/{server,compiler,registry}.
  This file is helpers only: toolchains, rustService/image wrappers,
  environment-derived overrides, and Nu-based check derivations.
*/
{
  pkgs,
  fenixPackages,
  nixos,
}:

let
  inherit (pkgs) lib;

  # One complete toolchain serves packages and the devshell. Hooks only omit
  # source/docs, which they do not consume.
  rustComponents = rec {
    complete = [
      "cargo"
      "clippy"
      "rust-src"
      "rust-docs"
      "rustc"
      "rustfmt"
      "rustc-codegen-cranelift-preview"
    ];
    hooks = lib.subtractLists [ "rust-src" "rust-docs" ] complete;
  };

  mkRustToolchain = components: fenixPackages.complete.withComponents components;

  rustToolchain = mkRustToolchain rustComponents.complete;
  rustToolchainDev = rustToolchain;
  rustToolchainHooks = mkRustToolchain rustComponents.hooks;

  # ── rustService from MachineConfigurations ───────────────────────────────
  mkRustService = nixos.lib.build.rustService { inherit pkgs; };

  # ── Snowydeer / impure environment → package override ───────────────────
  # Pure evaluation returns {}, while --impure adds exactly one store path.
  optionalEnvStorePath =
    attr: name:
    let
      path = builtins.getEnv name;
    in
    lib.optionalAttrs (path != "") { ${attr} = builtins.storePath path; };

  # ── nix2container service image (shared buildEnv pattern) ────────────────
  mkServiceImage =
    {
      buildImage,
      name,
      package,
      entrypoint,
      imageName,
      imageTag ? "latest",
      ports ? [ "8080/tcp" ],
      env ? [ ],
      labels ? { },
      extraPaths ? [ ],
      maxLayers ? 60,
    }:
    let
      rootEnv = pkgs.buildEnv {
        name = "${name}-image-root";
        paths = [
          package
          pkgs.cacert
          pkgs.stdenv.cc.cc
        ]
        ++ extraPaths;
        pathsToLink = [ "/" ];
        ignoreCollisions = true;
      };
    in
    buildImage {
      name = imageName;
      tag = imageTag;
      inherit maxLayers;
      copyToRoot = rootEnv;
      config = {
        Entrypoint = entrypoint;
        ExposedPorts = lib.genAttrs ports (_: { });
        Env = env;
        Labels = labels;
      };
    };

  # ── Buck2 cxx tools package (also exported by the root flake) ────────────
  mkCxx = pkgs.stdenv.mkDerivation {
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

  # ── Integration / unit check driven by a Nushell script ──────────────────
  # Logic lives in .nu (and tests/lib modules). Nix only wires store paths,
  # PATH, sandbox attrs, and impure env vars.
  #
  # Prefers running under stdenvNoCC so __noChroot / darwin networking /
  # preferLocalBuild / impureEnvVars behave like the previous check.
  # PATH includes nushell + runtimeInputs; NU_LIB_DIRS points at test modules.
  mkNuCheck =
    {
      name,
      # Path to the .nu entry script (run as the buildPhase).
      script,
      # Store path / source dir of shared Nu modules (tests/lib).
      nuLib,
      runtimeInputs ? [ ],
      # Extra environment (string values) exported before the script runs.
      env ? { },
      # Optional attrs merged into the derivation (e.g. server = ... for deps).
      passthruAttrs ? { },
      noChroot ? false,
      darwinAllowLocalNetworking ? false,
      preferLocalBuild ? true,
      allowSubstitutes ? false,
      impureEnvVars ? [ ],
      meta ? { },
      # Optional install-phase summary lines written to $out/result.txt.
      resultLines ? [ "${name}: ok" ],
    }:
    pkgs.stdenvNoCC.mkDerivation (
      {
        inherit name meta;

        nativeBuildInputs = runtimeInputs ++ [ pkgs.nushell ];

        inherit preferLocalBuild allowSubstitutes impureEnvVars;

        __noChroot = noChroot;
        __darwinAllowLocalNetworking = darwinAllowLocalNetworking;

        dontUnpack = true;
        dontConfigure = true;

        buildPhase = ''
          runHook preBuild
          ${lib.concatMapStringsSep "\n" (n: "export ${n}=${lib.escapeShellArg (toString env.${n})}") (
            builtins.attrNames env
          )}
          ${pkgs.nushell}/bin/nu --no-config-file --include-path ${toString nuLib} ${script}
          runHook postBuild
        '';

        installPhase = ''
          runHook preInstall
          mkdir -p "$out"
          {
          ${lib.concatMapStringsSep "\n" (line: "  echo ${lib.escapeShellArg line}") resultLines}
          } > "$out/result.txt"
          runHook postInstall
        '';
      }
      // passthruAttrs
    );
in
{
  inherit
    rustComponents
    mkRustToolchain
    rustToolchain
    rustToolchainDev
    rustToolchainHooks
    mkRustService
    optionalEnvStorePath
    mkServiceImage
    mkCxx
    mkNuCheck
    ;
}
