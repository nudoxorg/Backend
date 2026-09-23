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
  gui,
  workspaceRoot,
  corpus,
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
    ../nu/gui/harness.nu
    ../nu/agents/catalog.nu
    ../nu/agents/generate.nu
    ../nu/cutover/main.nu
    ../nu/main.nu
  ];
  source = builtins.concatStringsSep "\n\n" (map builtins.readFile sourceParts);
  # Native-toolchain and corpus authorities for the seven-language corpus,
  # shared with the interactive `.#complete` shell (`shells.nix`) so
  # `backend test workspace` sees the same compilers and corpus data that
  # tests pass under interactively. Referencing these store paths from the
  # wrapper derivation also makes them gate dependencies, so `nix store gc`
  # cannot strand the corpora between runs.
  nativeTestEnv = import ./corpus-env.nix {
    inherit
      pkgs
      tools
      toolchains
      corpus
      ;
  };
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
      runtimeInputs =
        tools.qualityTools
        ++ tools.coreServiceTools
        ++ tools.nativeCompilers
        ++ tools.authorityHelpers
        ++ gui.allPackages
        ++ runtimeInputs;
      runtimeEnv =
        nativeTestEnv
        // {
          CARGO_TARGET_DIR = ".local/target";
          BACKEND_CONFIG_SNAPSHOT = toString ../.;
          BACKEND_AST_GREP = "${astGrepSuite}/sgconfig.yml";
          BACKEND_KOJI_CONFIG = artifacts.koji;
          BACKEND_NEXTEST_CONFIG = artifacts.nextest;
          BACKEND_OTEL_COLLECTOR = artifacts.otelCollector;
          BACKEND_TREEFMT = "${formatting.wrapper}/bin/treefmt";
          BACKEND_CONTROL_PLANE = "${controlFile}/share/backend/control-plane.json";
          BACKEND_POLICY_ROOT_DIGEST = control.policyRootDigest;
          BACKEND_COMMAND_CATALOG_DIGEST = builtins.hashString "sha256" source;
          BACKEND_STABLE_CARGO = toolchains.stableCargo;
          BACKEND_CONTROL_SOURCE = toString workspaceRoot;
          # The control binary remains available as the dedicated
          # `.#backend-control` package. Keeping it out of this general shell
          # prevents each Cargo.lock edit from vendoring and rebuilding the
          # workspace before ordinary checks can begin; cutover commands retain
          # their explicit pinned `cargo run` fallback.
          BACKEND_CONTROL_BIN = "";
          BACKEND_DYLINT_TOOLCHAIN = toolchains.dylintToolchain;
          BACKEND_RUSTFMT = toolchains.rustfmt;
          BACKEND_GUI_CONFIG = "${gui.configFile}/share/nudox/gui-control-plane.json";
          BACKEND_GUI_FONTCONFIG = gui.fontConfig;
          BACKEND_GUI_FONT_MANIFEST = "${gui.fontManifest}/share/nudox/fonts.sha256";
          NUDOX_GUI_GPUI_SOURCE_DIGEST = gui.gpuiSourceDigest;
          NUDOX_GUI_GPUI_COMPONENT_SOURCE_DIGEST =
            if gui.gpuiComponentSourceDigest == null then "" else gui.gpuiComponentSourceDigest;
          NUDOX_GUI_GPUI_SOURCE_MANIFEST = gui.gpuiSourceManifest;
          NUDOX_GUI_DEPENDENCY_GRAPH_SHA256 = gui.dependencyGraphDigest;
          NUDOX_GUI_GPU_BACKEND = control.gui.gpu.defaultGpuBackend;
          NUDOX_GUI_GPU_DEVICE = control.gui.gpu.expectedGpuDevice;
          NUDOX_GUI_GPU_PROBE = "${gui.gpuProbe}/bin/nudox-gui-gpu-probe";
          WGPU_BACKEND = control.gui.gpu.forceEnvironment.WGPU_BACKEND;
          LIBGL_ALWAYS_SOFTWARE = control.gui.gpu.forceEnvironment.LIBGL_ALWAYS_SOFTWARE;
          MESA_LOADER_DRIVER_OVERRIDE = control.gui.gpu.forceEnvironment.MESA_LOADER_DRIVER_OVERRIDE;
          NUDOX_GUI_TOOLCHAIN = toString toolchains.stable;
          NUDOX_GUI_ENCODER_VERSION = pkgs.ffmpeg.version;
          NUDOX_GUI_TOOL_CLOSURE = "${gui.toolsBundle}";
          NUDOX_GUI_HARNESS = "nix shell .#gui-harness .#gui-tools .#gui-runtime";
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
