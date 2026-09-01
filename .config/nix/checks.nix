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
}:
{
  nushell-command = commands.backend;
  agent-skills = commands.agentSkills;
  formatting = formatting.check;

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
      mkdir ($env.out | path join "share")
      "validated" | save ($env.out | path join "share" "control-plane")
    '';
  };
}
