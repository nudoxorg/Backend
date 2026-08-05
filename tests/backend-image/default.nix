/*
  Flake check: full backend image pipeline via realistic HTTP.

  Thin Nix glue — all wait loops, service lifecycle, curl/polling live in
  tests/backend-image/check.nu + tests/lib modules.

  Provisions postgres, qdrant, terminusdb + the compiler daemon under $TMPDIR,
  starts the driver package (published as packages.server for compatibility)
  with NUDOX_ROLE=all, then:

    GET  /healthz
    POST /packages  { ecosystem: rust,       name: axum, version: … }
    POST /packages  { ecosystem: typescript, name: zod,  version: … }
    poll GET /packages/:id until state is Stored (or fail on Failed/DeadLettered)

  Local TCP + outbound HTTPS required. Prefer --impure when the compiler is
  still a snowydeer placeholder (buck2 rebuilds from PRJ_ROOT) or when
  NUDOX_COMPILER_DAEMON_PATH is set.
*/
{
  lib,
  pkgs,
  mkNuCheck,
  server,
  backend,
  compilerDaemon,
  buck2,
  rustToolchain,
  # Absolute path to the repo checkout for buck2 fallback.
  projectRoot ? "",
  axumVersion ? "0.7.9",
  zodVersion ? "3.24.2",
  pipelineDeadlineSecs ? 300,
  # Shared Nu modules directory (tests/lib).
  nuLib,
}:

mkNuCheck {
  name = "backend-image-check";
  script = ./check.nu;
  inherit nuLib;

  runtimeInputs = with pkgs; [
    bash
    curl
    jq
    postgresql
    qdrant
    terminusdb
    cacert
    coreutils
    findutils
    gnugrep
    gnused
    buck2
    rustToolchain
  ];

  env = {
    NUDOX_SERVER = toString server;
    NUDOX_BACKEND_IMAGE = toString backend;
    NUDOX_COMPILER_PACKAGE = toString compilerDaemon;
    NUDOX_PROJECT_ROOT = toString projectRoot;
    NUDOX_AXUM_VERSION = axumVersion;
    NUDOX_ZOD_VERSION = zodVersion;
    NUDOX_PIPELINE_DEADLINE_SECS = toString pipelineDeadlineSecs;
    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  };

  # Keep store-path references so the image/server rebuild when sources change.
  passthruAttrs = {
    inherit server backend compilerDaemon;
  };

  noChroot = true;
  darwinAllowLocalNetworking = true;
  preferLocalBuild = true;
  allowSubstitutes = false;
  impureEnvVars = [
    "PRJ_ROOT"
    "NUDOX_COMPILER_DAEMON_PATH"
  ];

  resultLines = [
    "backend-image-check: ok"
    "image: ${toString backend}"
    "server: ${toString server}"
    "compiler: ${toString compilerDaemon}"
    "axum: ${axumVersion}"
    "zod: ${zodVersion}"
    "coverage: OCI config + health + package add/poll + source download + blob emit"
    "pipeline: acquire → extract → compile (postcard) → emit blobs → Stored"
    "wire: application/x-postcard CompileRequest/CompileResponse"
  ];

  meta = {
    description = "Full-pipeline HTTP check for the NuDox backend image (axum + zod → Stored)";
  };
}
