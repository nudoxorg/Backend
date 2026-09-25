/*
  Shared Nix helpers for NuDox packaging and checks.

  App package recipes live under workspace/{server,compiler,registry}.
  This file is helpers only: toolchains, rustService/image wrappers,
  environment-derived overrides, and Nu-based check derivations.
*/
{
  pkgs,
  fenixPackages,
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

  # Host fenix toolchain plus extra rust-std targets (universal macOS).
  rustToolchainWithTargets =
    extraTargets:
    fenixPackages.combine (
      [ rustToolchain ] ++ map (t: fenixPackages.targets.${t}.stable.rust-std) extraTargets
    );

  # ── Repository-local Rust service package helper ──────────────────────────
  # Keep the package recipe contract small and implement it entirely with
  # nixpkgs. This replaces the former private MachineConfigurations helper.
  mkRustService =
    {
      pname,
      version,
      src,
      cargoPackage,
      mainProgram ? "",
      description ? "",
    }:
    pkgs.rustPlatform.buildRustPackage {
      inherit pname version src;
      # cargo vendor, not importCargoLock: the lockfile contains two
      # trustfall 0.8.1 crates and cargo is the tool that names both.
      cargoHash = "sha256-XwLTqM9Mm+T6OR6gw7nuwhAT+J7p+RsZ9/ebL2m23Io=";
      depsExtraArgs = {
        GIT_CONFIG_GLOBAL = ./git-https-instead-of-ssh.config;
      };
      cargoBuildFlags = [
        "-p"
        cargoPackage
      ]
      ++ lib.optional (mainProgram != "") [
        "--bin"
        mainProgram
      ];
      doCheck = false;
      installPhase =
        if mainProgram == "" then
          "mkdir -p $out"
        else
          ''
            install -Dm755 "target/release/${mainProgram}" "$out/bin/${mainProgram}"
          '';
      meta = {
        inherit description mainProgram;
      };
    };

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
  # Uses the platform stdenv so Rust build scripts can invoke the native
  # C/C++ compiler and linker while retaining the existing sandbox attrs.
  # PATH includes nushell + runtimeInputs; NU_LIB_DIRS points at test modules.
  mkNuCheck =
    {
      name,
      # Path to the .nu entry script (run as the buildPhase).
      script,
      # Optional source tree to unpack before running the check.
      src ? null,
      # Store path / source dir of shared Nu modules (tests/lib).
      nuLib,
      runtimeInputs ? [ ],
      # Extra environment (string values) exported before the script runs.
      env ? { },
      # Optional source preparation performed after unpacking and before the
      # check script. This keeps fetched build inputs in the derivation rather
      # than relying on files outside the flake source snapshot.
      preBuild ? "",
      # Files declared by Cargo manifests that may be untracked in a dirty
      # checkout. Each file is copied into the unpacked source at its relative
      # path; callers must keep this list narrow and target-specific.
      extraSrcFiles ? [ ],
      # Source subtrees copied with their own caller-supplied filter. This is
      # for dirty checkouts where a package's declared Rust modules are
      # untracked; the filter must reject artifacts and unrelated files.
      extraSrcTrees ? [ ],
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
    pkgs.stdenv.mkDerivation (
      {
        inherit name meta;

        nativeBuildInputs = runtimeInputs ++ [ pkgs.nushell ];

        inherit preferLocalBuild allowSubstitutes impureEnvVars;

        __noChroot = noChroot;
        __darwinAllowLocalNetworking = darwinAllowLocalNetworking;

        dontUnpack = src == null;
        dontConfigure = true;

        inherit src;

        buildPhase = ''
          runHook preBuild
          ${lib.concatMapStringsSep "\n" (n: "export ${n}=${lib.escapeShellArg (toString env.${n})}") (
            builtins.attrNames env
          )}
          ${lib.concatMapStringsSep "\n" (
            file:
            "mkdir -p \"$(dirname ${lib.escapeShellArg file.relPath})\"; cp ${lib.escapeShellArg file.source} ${lib.escapeShellArg file.relPath}"
          ) extraSrcFiles}
          ${lib.concatMapStringsSep "\n" (
            tree:
            "mkdir -p ${lib.escapeShellArg tree.relPath}; cp -R ${lib.escapeShellArg tree.source}/. ${lib.escapeShellArg tree.relPath}/"
          ) extraSrcTrees}
          ${preBuild}
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
    rustToolchainWithTargets
    mkRustService
    optionalEnvStorePath
    mkServiceImage
    mkCxx
    mkNuCheck
    ;
}
