# Exposes cheap structural checks as ordinary flake checks.
# Uses nuenv derivations so validation never falls back to shell scripting.
# Reserves expensive product proof for explicitly scoped Nushell commands.
{
  pkgs,
  tools,
  formatting,
  commands,
  astGrepSuite,
  artifacts,
  controlFile,
  control,
  helpers,
  toolchains,
  gui,
  lunaTools,
}:
let
  # Materialize the closure during evaluation. A sandboxed check cannot
  # reliably open a second connection to the Nix daemon, and a failed
  # `nix-store --query` pipeline previously degraded into an empty successful
  # result. `closureInfo` gives the check an immutable, dependency-tracked
  # manifest instead.
  lunaToolsClosure = pkgs.closureInfo {
    rootPaths = [ lunaTools ];
  };
in
{
  nushell-command = commands.backend;
  agent-skills = commands.agentSkills;
  formatting = formatting.check;

  # Exercises the Cargo lease protocol with fake Cargo/git/sccache processes;
  # it is deliberately independent from the workspace build graph.
  cargo-cache-protocol =
    pkgs.runCommand "nudox-cargo-cache-protocol"
      {
        nativeBuildInputs = [
          pkgs.bash
          pkgs.coreutils
          pkgs.gawk
          # Lease recovery compares an owner's process start time via `ps`.
          pkgs.ps
          pkgs.python3
        ];
      }
      ''
        NUDOX_CARGO_CACHE_SCRIPT=${../scripts/cargo-shared-cache.sh} \
          ${pkgs.bash}/bin/bash ${../../tests/cargo-shared-cache.sh}
        mkdir -p $out/share
        echo validated > $out/share/cargo-cache-protocol
      '';

  # Guard the public `luna-tools` shell's actual Nix requisites. Checking
  # PATH alone would allow a wrapper to hide an accidental backend/GUI
  # dependency. The complete role bundle is checked separately below.
  luna-tools-closure = helpers.nuCheck {
    inherit pkgs;
    name = "nudox-luna-tools-closure";
    packages = [ lunaTools ];
    build = ''
      let closure = (open ${lunaToolsClosure}/store-paths | lines)
      let forbidden = ["backend-control" "nudox-gui-runtime" "nudox-gui-tools" "bmake"]
      let violations = ($closure | where {|path|
        $forbidden | any {|needle| $path | str contains $needle }
      })
      if ($closure | length) > 512 {
        error make {msg: ("luna-tools closure unexpectedly contains " + (($closure | length) | into string) + " requisites; inspect the package graph before merging")}
      }
      if not ($violations | is-empty) {
        error make {msg: ("luna-tools closure contains forbidden products: " + ($violations | str join ", "))}
      }
      for command in ["cargo" "rustc" "rustfmt" "clippy-driver" "git" "jq" "nu" "sccache"] {
        if (which $command | is-empty) {
          error make {msg: ("luna-tools is missing pinned command: " + $command)}
        }
      }
      mkdir ($env.out | path join "share")
      ^cargo --version | str trim | save --raw ($env.out | path join "share" "cargo-version")
      {
        package: "${lunaTools}"
        closure_entries: ($closure | length)
        forbidden_products: $forbidden
      } | to json | save --raw ($env.out | path join "share" "luna-tools-closure.json")
    '';
  };

  gui-contract = helpers.nuCheck {
    inherit pkgs;
    name = "nudox-gui-contract";
    packages = [
      commands.backend
      gui.toolsBundle
      pkgs.jq
    ];
    environment = {
      BACKEND_CONFIG_MODE = "immutable";
      BACKEND_CONFIG_SNAPSHOT = toString ../.;
      BACKEND_GUI_CONFIG = "${gui.configFile}/share/nudox/gui-control-plane.json";
      BACKEND_GUI_FONTCONFIG = gui.fontConfig;
    };
    build = ''
      backend gui validate
      # The build script is Nushell: one command per line, no `\` continuations,
      # and environment variables are read through `$env`.
      ^jq --exit-status '(.schema == 1) and (.viewport.required | length == 9) and (.viewport.scales == [1,2]) and (.animation.requiredPhases | length == 8) and (.acceptance.loops | map(.name) == ["property","randomized","differential","metamorphic"]) and (.gpu.framework == "gpui-ce") and (.gpu.componentFramework == "gpui-ce-component") and (.services.serverIndex.protocol == "nudox-locald-framed-v1") and (.services.hiddenHoldouts.required == true)' $env.BACKEND_GUI_CONFIG
      mkdir ($env.out | path join "share")
      "validated" | save ($env.out | path join "share" "gui-contract")
    '';
  };

  gui-service-contract = helpers.nuCheck {
    inherit pkgs;
    name = "nudox-gui-service-contract";
    packages = pkgs.lib.optional (tools.guiRuntime != null) tools.guiRuntime;
    environment = {
      BACKEND_CONFIG_MODE = "immutable";
      BACKEND_GUI_CONFIG = "${gui.configFile}/share/nudox/gui-control-plane.json";
    };
    build =
      if tools.guiRuntime == null then
        ''
          echo "GUI service contract requires the workspace runtime closure" >&2
          exit 78
        ''
      else
        ''
          nudox-gui-service-test
          mkdir ($env.out | path join "share")
          "validated" | save ($env.out | path join "share" "gui-service-contract")
        '';
  };

  ast-grep-rules = helpers.nuCheck {
    inherit pkgs;
    name = "backend-ast-grep-rules";
    packages = [ pkgs.ast-grep ];
    build = ''
      ast-grep test --config ${astGrepSuite}/sgconfig.yml --test-dir ${astGrepSuite}/tests
      mkdir ($env.out | path join "share")
      "validated" | save ($env.out | path join "share" "ast-grep")
    '';
  };

  semantic-lints = helpers.nuCheck {
    inherit pkgs;
    name = "backend-semantic-lints";
    packages = [
      tools.cargoDylint
      tools.dylintLink
      toolchains.nightly
      pkgs.coreutils
      pkgs.stdenv.cc
      pkgs.libiconv
      pkgs.zlib
    ];
    environment = {
      BACKEND_DYLINT_ROOT = toString ../dylint;
      BACKEND_DYLINT_TOOLCHAIN = toolchains.dylintToolchain;
      BACKEND_DYLINT_DECLARATIONS = builtins.toJSON (
        map (rule: rule.id) (builtins.filter (rule: rule.engine == "dylint") control.lint.rules)
      );
      BACKEND_NIGHTLY_CARGO = toolchains.nightlyCargo;
      LIBRARY_PATH = pkgs.lib.makeLibraryPath [
        pkgs.libiconv
        pkgs.zlib
      ];
    }
    // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
      SDKROOT = pkgs.apple-sdk.sdkroot;
    };
    build = ''
      with-env { BACKEND_DYLINT_ARTIFACTS: ($env.TMPDIR | path join "dylint") } {
        nu --no-config-file ${../.}/dylint/test.nu
      }
      mkdir ($env.out | path join "share")
      "validated" | save ($env.out | path join "share" "semantic-lints")
    '';
  };

  telemetry-config = helpers.nuCheck {
    inherit pkgs;
    name = "backend-telemetry-config";
    packages = [ pkgs.opentelemetry-collector-contrib ];
    build = ''
      let local = $env.out | path join "observability"
      let events = $local | path join "events"
      mkdir $events
      with-env {
        BACKEND_TOOLING_EVENTS: $events
        BACKEND_OTEL_EXPORT: ($local | path join "otel.json")
      } {
        otelcol-contrib validate --config ${artifacts.otelCollector}
      }
      "validated" | save ($env.out | path join "telemetry")
    '';
  };

  tooling-contracts = helpers.nuCheck {
    inherit pkgs;
    name = "backend-tooling-contracts";
    packages = [
      commands.backend
      pkgs.ast-grep
      pkgs.nushell
    ];
    environment = {
      BACKEND_AST_GREP = "${astGrepSuite}/sgconfig.yml";
      BACKEND_CONFIG_MODE = "immutable";
      BACKEND_CONFIG_SNAPSHOT = toString ../.;
      BACKEND_CONTROL_PLANE = "${controlFile}/share/backend/control-plane.json";
    };
    build = ''
      backend agents verify
      nu --no-config-file ${../nu/tests.nu}
      mkdir ($env.out | path join "share")
      "validated" | save ($env.out | path join "share" "tooling-contracts")
    '';
  };

  control-plane = helpers.nuCheck {
    inherit pkgs;
    name = "backend-control-plane";
    packages = [
      commands.backend
      pkgs.git
      pkgs.nix
      pkgs.nushell
      pkgs.coreutils
      pkgs.b3sum
      pkgs.bash
      pkgs.stdenv.cc
      toolchains.stable
    ];
    environment = {
      BACKEND_CONFIG_MODE = "immutable";
      BACKEND_CONFIG_SNAPSHOT = toString ../.;
      BACKEND_CONTROL_PLANE = "${controlFile}/share/backend/control-plane.json";
      BACKEND_LUNA_TOOLS = commands.roleBundles."luna-pair";
      BACKEND_TERRA_TOOLS = commands.roleBundles."terra-academic";
      BACKEND_STABLE_CARGO = toolchains.stableCargo;
    };
    build = ''
      nu --no-config-file ${../tests/control-plane.nu}
      mkdir nu
      ^cp -R ${../nu}/. nu/
      nu --no-config-file nu/cutover/tests.nu
      mkdir ($env.out | path join "share")
      "validated" | save ($env.out | path join "share" "control-plane")
    '';
  };
}
