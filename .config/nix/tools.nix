# Collects every executable admitted to repository tooling and development.
# Keeps optional platform tools explicit and prevents ambient PATH discovery.
# Supplies one source of truth to shells, commands, and checks.
{
  pkgs,
  toolchains,
  workspaceRoot,
}:
let
  workspaceAvailable = builtins.pathExists (workspaceRoot + "/Cargo.toml");
  stableRustPlatform = pkgs.makeRustPlatform {
    cargo = toolchains.stable;
    rustc = toolchains.stable;
  };
  controlSourcePrefixes = [
    "tools/control"
    "crates/store"
    "crates/version"
  ];
  workspaceSource =
    if workspaceAvailable then
      pkgs.lib.cleanSourceWith {
        src = workspaceRoot;
        filter =
          path: type:
          let
            absolute = toString path;
            root = toString workspaceRoot;
            relative = pkgs.lib.removePrefix "${root}/" absolute;
          in
          absolute == root
          || builtins.elem relative [
            "Cargo.toml"
            "Cargo.lock"
            "crates"
          ]
          || builtins.any (
            prefix: relative == prefix || pkgs.lib.hasPrefix "${prefix}/" relative
          ) controlSourcePrefixes;
      }
    else
      null;
  dylintSource = pkgs.fetchFromGitHub {
    owner = "trailofbits";
    repo = "dylint";
    rev = "v6.0.4";
    hash = "sha256-CROuPpPzUobUcH3Xl2fpEOVxEBBppmFBaJSRXsEuaXg=";
  };
  mkDylintTool =
    package:
    pkgs.rustPlatform.buildRustPackage {
      pname = package;
      version = "6.0.4";
      src = dylintSource;
      cargoDeps = cargoDylint.cargoDeps;
      cargoBuildFlags = [
        "--package"
        package
      ];
      cargoInstallFlags = [
        "--package"
        package
      ];
      doCheck = false;
    };
  cargoDylint = pkgs.rustPlatform.buildRustPackage rec {
    pname = "cargo-dylint";
    version = "6.0.4";
    src = dylintSource;
    cargoHash = "sha256-9YAYtVoqMfyeG5sy8Jtt8a894k9AzyhIDEFzyqdzyeI=";
    cargoBuildFlags = [
      "--package"
      "cargo-dylint"
      "--no-default-features"
      "--features"
      "cargo-cli"
    ];
    cargoInstallFlags = [
      "--package"
      "cargo-dylint"
    ];
    doCheck = false;
  };
  dylintLink = mkDylintTool "dylint-link";
  backendControl =
    if workspaceAvailable then
      stableRustPlatform.buildRustPackage {
        cargoBuildFlags = [
          "--package"
          "backend-control"
          "--bin"
          "backend-control"
        ];
        pname = "backend-control";
        version = "0.1.0";
        src = workspaceSource;
        postPatch = ''
                substituteInPlace Cargo.toml \
                  --replace-fail \
                  'members = [
              "crates/*",
              "frontends/*",
              "extensions/*",
              "apps/*",
              "tests/*",
              "tools/*",
          ]' \
                  'members = [
              "tools/control",
              "crates/store",
              "crates/version",
          ]'
        '';
        cargoLock.lockFile = workspaceRoot + "/Cargo.lock";
        cargoInstallFlags = [
          "--package"
          "backend-control"
          "--bin"
          "backend-control"
        ];
        doCheck = false;
      }
    else
      null;
  nativeCompilers = [
    pkgs.clang
    pkgs.dotnet-sdk_8
    pkgs.go
    pkgs.jdk
    pkgs.nodejs_22
    pkgs.python3
    pkgs.typescript
  ];
  qualityTools = [
    pkgs.ast-grep
    pkgs.b3sum
    pkgs.cargo-audit
    pkgs.cargo-deny
    pkgs.cargo-nextest
    pkgs.coreutils
    pkgs.direnv
    pkgs.git
    pkgs.jq
    pkgs.koji
    pkgs.nixfmt
    pkgs.nix-direnv
    pkgs.nufmt
    pkgs.nushell
    pkgs.taplo
    pkgs.yamlfmt
  ];
  verifierTools = [
    pkgs.cargo-bloat
    cargoDylint
    dylintLink
    pkgs.cargo-llvm-cov
    pkgs.hyperfine
    pkgs.samply
  ]
  ++ pkgs.lib.optional (backendControl != null) backendControl
  ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.valgrind ];
  serviceTools = [
    pkgs.curl
    pkgs.qdrant
  ];
  observabilityTools = [
    pkgs.otel-cli
    pkgs.otel-desktop-viewer
    pkgs.opentelemetry-collector-contrib
  ];
in
{
  inherit
    backendControl
    cargoDylint
    dylintLink
    nativeCompilers
    qualityTools
    serviceTools
    observabilityTools
    verifierTools
    ;
  development = [ toolchains.stable ] ++ qualityTools;
  compiler = [ toolchains.stable ] ++ qualityTools ++ nativeCompilers;
  services = [ toolchains.stable ] ++ qualityTools ++ serviceTools;
  observability = [ toolchains.stable ] ++ qualityTools ++ observabilityTools;
  verification = [
    toolchains.stable
    toolchains.nightly
  ]
  ++ qualityTools
  ++ verifierTools;
  complete = [
    toolchains.stable
    toolchains.nightly
  ]
  ++ qualityTools
  ++ nativeCompilers
  ++ serviceTools
  ++ observabilityTools
  ++ verifierTools;
  nixFormatter = pkgs.nixfmt;
}
