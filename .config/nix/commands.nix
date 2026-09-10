# Builds the single Nushell command surface from invariant-owned source parts.
# Injects only pinned tools and paths, leaving repository discovery to runtime.
# Statically checks the assembled script during every Nix package build.
{
  pkgs,
  tools,
  toolchains,
  controlFile,
  astGrepSuite,
  artifacts,
  formatting,
  control,
}:
let
  sourceParts = [
    ../nu/core/failure.nu
    ../nu/core/control.nu
    ../nu/core/capability.nu
    ../nu/core/root.nu
    ../nu/core/telemetry.nu
    ../nu/core/process.nu
    ../nu/scope/git.nu
    ../nu/scope/cargo.nu
    ../nu/create/file.nu
    ../nu/create/crate.nu
    ../nu/quality/format.nu
    ../nu/quality/lint.nu
    ../nu/quality/test.nu
    ../nu/quality/observe.nu
    ../nu/quality/commit.nu
    ../nu/agents/catalog.nu
    ../nu/agents/generate.nu
    ../nu/main.nu
  ];
  source = builtins.concatStringsSep "\n\n" (map builtins.readFile sourceParts);
in
let
  makeBackend =
    {
      runtimeEnv ? { },
      runtimeInputs ? [ ],
    }:
    pkgs.nuenv.writeShellApplication {
      name = "backend";
      text = source;
      # serviceTools is qdrant + curl, and the command surface invokes
      # neither — the only "qdrant" under .config/nu is the live-qdrant
      # nextest *group name*. Carrying it here puts qdrant in the closure of
      # every shell that includes a backend command, `compiler` among them,
      # and qdrant 1.18.2 cannot be built on x86_64-linux: it emits
      # llvm.x86.avx512.vpdpwssd.512 from a path compiled without that target
      # feature, so rustc aborts with "Cannot select". It is in no binary
      # cache either, so the build cannot be skipped. Darwin keeps the exact
      # closure it had; the `services` and `complete` shells still get qdrant
      # directly from tools.services / tools.complete, so nothing that
      # actually runs the service loses it.
      runtimeInputs =
        tools.qualityTools
        ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin tools.serviceTools
        ++ runtimeInputs;
      runtimeEnv = {
        CARGO_TARGET_DIR = ".local/target";
        BACKEND_CONFIG_SNAPSHOT = toString ../.;
        BACKEND_AST_GREP = "${astGrepSuite}/sgconfig.yml";
        BACKEND_KOJI_CONFIG = artifacts.koji;
        BACKEND_NEXTEST_CONFIG = artifacts.nextest;
        BACKEND_OTEL_COLLECTOR = artifacts.otelCollector;
        BACKEND_TREEFMT = "${formatting.wrapper}/bin/treefmt";
        BACKEND_CONTROL_PLANE = "${controlFile}/share/backend/control-plane.json";
        BACKEND_COMMAND_CATALOG_DIGEST = builtins.hashString "sha256" source;
        BACKEND_STABLE_CARGO = toolchains.stableCargo;
        BACKEND_DYLINT_TOOLCHAIN = toolchains.dylintToolchain;
        BACKEND_RUSTFMT = toolchains.rustfmt;
      }
      // runtimeEnv;
    };
in
rec {
  backend = makeBackend { };
  backendVerifier = makeBackend {
    runtimeInputs = tools.verifierTools ++ [ toolchains.nightly ];
    runtimeEnv.BACKEND_NIGHTLY_CARGO = toolchains.nightlyCargo;
  };
  agentSkills =
    pkgs.runCommand "backend-agent-skills" { nativeBuildInputs = [ (makeBackend { }) ]; }
      ''
        COLUMNS=120 BACKEND_CONFIG_MODE=immutable backend agents generate --output "$out" >/dev/null
      '';
  roleSkills = pkgs.lib.mapAttrs (
    roleId: _:
    pkgs.runCommand "backend-${roleId}-skill" { nativeBuildInputs = [ (makeBackend { }) ]; } ''
      COLUMNS=120 BACKEND_CONFIG_MODE=immutable backend agents generate --role ${roleId} --output "$out" >/dev/null
    ''
  ) control.roles;
  roleRunners = pkgs.lib.mapAttrs (
    roleId: role:
    makeBackend {
      runtimeInputs =
        if
          builtins.elem roleId [
            "terra-reviewer"
            "sol-integrator"
          ]
        then
          tools.verifierTools ++ [ toolchains.nightly ]
        else
          [ ];
      runtimeEnv = {
        BACKEND_AGENT_ROLE = roleId;
        BACKEND_AGENT_CONTRACT_DIGEST = role.contractDigest;
      }
      // pkgs.lib.optionalAttrs (roleId != "luna-pair") {
        BACKEND_PROCESS_ARTIFACT_POLICY = "private-debug";
      }
      //
        pkgs.lib.optionalAttrs
          (builtins.elem roleId [
            "terra-reviewer"
            "sol-integrator"
          ])
          {
            BACKEND_NIGHTLY_CARGO = toolchains.nightlyCargo;
          };
    }
  ) control.roles;
  roleTools = import ./role-tools.nix {
    inherit pkgs roleRunners;
    inherit (control) roles;
  };
  roleBundles = pkgs.lib.mapAttrs (
    roleId: tools:
    pkgs.symlinkJoin {
      name = "backend-${roleId}-bundle";
      paths = [
        tools
        roleSkills.${roleId}
      ];
    }
  ) roleTools;
  telemetry = pkgs.nuenv.writeShellApplication {
    name = "telemetry";
    runtimeInputs = [ pkgs.opentelemetry-collector-contrib ];
    runtimeEnv.BACKEND_OTEL_COLLECTOR = artifacts.otelCollector;
    text = ''
      let repository = (pwd | path expand)
      let local = $repository | path join ".local/observability"
      let events = $local | path join "tooling/events"
      let export = $local | path join "otel.json"
      mkdir $events
      with-env {
        BACKEND_TOOLING_EVENTS: $events
        BACKEND_OTEL_EXPORT: $export
      } {
        run-external "otelcol-contrib" "--config" $env.BACKEND_OTEL_COLLECTOR
      }
    '';
  };
}
