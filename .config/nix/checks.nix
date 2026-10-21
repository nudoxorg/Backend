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
  workspaceRoot,
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
  nativeCompilerEnv = import ./corpus-env.nix {
    inherit pkgs tools toolchains;
    corpus = null;
  };
  nativeCompilerFlags = pkgs.lib.filterAttrs (
    name: _: pkgs.lib.hasPrefix "NIX_" name
  ) nativeCompilerEnv;
  surfaceCorpus = import ./surface-corpus.nix { inherit pkgs; };
  surfaceCargoLockContract = pkgs.runCommand "nudox-surface-cargo-lock-contract" { } ''
    set -e
    cmp ${surfaceCorpus.rust-itoa}/Cargo.lock ${../../tests/surface/cargo-locks/rust-itoa.lock}
    cmp ${surfaceCorpus.rust-ryu}/Cargo.lock ${../../tests/surface/cargo-locks/rust-ryu.lock}
    mkdir -p "$out/share"
    printf '%s\n' validated > "$out/share/surface-cargo-lock-contract"
  '';
  localHostRuntimeContract =
    if builtins.pathExists (workspaceRoot + "/Cargo.toml") then
      pkgs.runCommand "nudox-local-host-runtime-contract"
        {
          nativeBuildInputs = [ pkgs.python3 ];
        }
        ''
          python3 ${../scripts/nix-host-runtime-contract.py} \
            ${workspaceRoot + "/crates/engine/src/application/host.rs"} \
            ${workspaceRoot + "/.config/nix/corpus-env.nix"} \
            ${workspaceRoot + "/.config/nix/shells.nix"} \
            ${workspaceRoot + "/frontends/typescript/src/legacy/checker.rs"} \
            ${workspaceRoot + "/frontends/go/src/legacy/oracle.rs"} \
            ${workspaceRoot + "/frontends/python/src/legacy/checker.rs"} \
            ${workspaceRoot + "/frontends/csharp/src/legacy/oracle.rs"} \
            ${workspaceRoot + "/tests/fleet/src/lib.rs"} \
            ${workspaceRoot + "/tests/fleet/run-fleet.sh"} \
            ${workspaceRoot + "/crates/compile/src/supervisor.rs"} \
            ${workspaceRoot + "/crates/compile/src/tests.rs"}
          mkdir -p "$out/share"
          printf '%s\n' validated > "$out/share/local-host-runtime-contract"
        ''
    else
      null;
in
{
  nu-check-failure-contract =
    let
      success = pkgs.writeText "check-success.nu" (
        helpers.nuCheckScript ''
          ^sh -c "exit 0"
          print VALIDATED
        ''
      );
      externalFailure = pkgs.writeText "check-external-failure.nu" (
        helpers.nuCheckScript ''
          ^sh -c "exit 7"
          print VALIDATED
        ''
      );
      internalFailure = pkgs.writeText "check-internal-failure.nu" (
        helpers.nuCheckScript ''
          error make {msg: "expected controlled failure"}
          print VALIDATED
        ''
      );
    in
    pkgs.runCommand "nudox-nu-check-failure-contract" { nativeBuildInputs = [ pkgs.nushell ]; } ''
      nu --no-config-file ${success} > success.log
      grep -qx VALIDATED success.log
      for fixture in ${externalFailure} ${internalFailure}; do
        if nu --no-config-file "$fixture" > failure.log 2>&1; then
          echo "failed check was accepted" >&2
          exit 1
        fi
        if grep -qx VALIDATED failure.log; then
          echo "failed check continued to its validation marker" >&2
          exit 1
        fi
      done
      touch "$out"
    '';

  native-c-probe-contract =
    pkgs.runCommand "nudox-native-c-probe-contract"
      {
        nativeBuildInputs = [ pkgs.stdenv.cc ];
      }
      ''
        # The backend command installs these captured flags after the caller's
        # environment. Exercise that actual environment, rather than the shell's
        # hardeningDisable setting, with jemalloc's real -O0/-Werror pattern.
        ${pkgs.lib.concatMapStringsSep "\n" (
          name: "export ${name}=${pkgs.lib.escapeShellArg nativeCompilerFlags.${name}}"
        ) (builtins.attrNames nativeCompilerFlags)}
        cat > probe.c <<'C'
        #define _GNU_SOURCE
        #include <errno.h>
        #include <stdio.h>
        #include <stdlib.h>
        #include <string.h>
        int main(void) {
          char buffer[100];
        #ifdef __linux__
          char *message = strerror_r(EINVAL, buffer, sizeof(buffer));
          return message == NULL;
        #else
          return strerror_r(EINVAL, buffer, sizeof(buffer)) != 0;
        #endif
        }
        C
        cc -std=gnu11 -O0 -Werror probe.c -o probe
        ./probe
        touch "$out"
      '';

  surface-smoke-contract =
    pkgs.runCommand "nudox-surface-smoke-contract"
      {
        nativeBuildInputs = [
          pkgs.python3
          pkgs.git
        ];
      }
      ''
        cp ${../ci/smoke.py} smoke.py
        cp ${../ci/test_smoke.py} test_smoke.py
        cp ${../ci/t1_capacity.py} t1_capacity.py
        cp ${../ci/test_t1_capacity.py} test_t1_capacity.py
        cp ${../ci/quarantine.py} quarantine.py
        cp ${../ci/test_quarantine.py} test_quarantine.py
        python3 test_quarantine.py
        cp ${../ci/fast_cache.py} fast_cache.py
        cp ${../ci/test_fast_cache.py} test_fast_cache.py
        python3 test_fast_cache.py
        cp ${../ci/source_ignores.py} source_ignores.py
        cp ${../ci/test_source_ignores.py} test_source_ignores.py
        python3 test_source_ignores.py
        cp ${../ci/junit_timing.py} junit_timing.py
        cp ${../ci/test_junit_timing.py} test_junit_timing.py
        python3 test_junit_timing.py
        python3 test_smoke.py
        python3 test_t1_capacity.py
        cp ${../ci/t1_artifacts.py} t1_artifacts.py
        cp ${../ci/test_t1_artifacts.py} test_t1_artifacts.py
        python3 test_t1_artifacts.py
        mkdir -p .config/ci
        cp ${../ci/t1_artifacts.py} .config/ci/t1_artifacts.py
        cp ${../ci/source_identity.py} .config/ci/source_identity.py
        cp ${../ci/cpu_placement.py} .config/ci/cpu_placement.py
        cp ${../ci/test_cpu_placement.py} .config/ci/test_cpu_placement.py
        python3 .config/ci/test_cpu_placement.py
        cp ${../ci/perf_context.py} .config/ci/perf_context.py
        cp ${../ci/test_perf_context.py} .config/ci/test_perf_context.py
        python3 .config/ci/test_perf_context.py
        cp ${../ci/surface.py} .config/ci/surface.py
        cp ${../ci/process_custody.py} .config/ci/process_custody.py
        cp ${../ci/test_surface.py} .config/ci/test_surface.py
        python3 .config/ci/test_surface.py
        cp ${../ci/smoke.py} .config/ci/smoke.py
        cp ${../ci/gui_contract.py} .config/ci/gui_contract.py
        cp ${../ci/test_gui_contract.py} .config/ci/test_gui_contract.py
        python3 .config/ci/test_gui_contract.py
        cp ${../ci/cli_contract.py} .config/ci/cli_contract.py
        cp ${../ci/test_cli_contract.py} .config/ci/test_cli_contract.py
        python3 .config/ci/test_cli_contract.py
        cp ${../ci/catalog.py} .config/ci/catalog.py
        cp ${../ci/test_catalog.py} .config/ci/test_catalog.py
        cp ${../ci/test.py} .config/ci/test.py
        python3 .config/ci/test_catalog.py
        cp ${../ci/test_tiers.py} .config/ci/test_tiers.py
        cp ${../ci/selection.py} .config/ci/selection.py
        python3 .config/ci/test_tiers.py
        cp ${../ci/test_selection.py} .config/ci/test_selection.py
        python3 .config/ci/test_selection.py
        cp ${../ci/test_layout.py} test_layout.py
        cp ${../ci/test_test_layout.py} test_test_layout.py
        python3 test_test_layout.py
        cp ${../ci/restore-mtimes.py} restore-mtimes.py
        cp ${../ci/test_restore_mtimes.py} test_restore_mtimes.py
        HOME="$TMPDIR" python3 test_restore_mtimes.py
        touch "$out"
      '';

  linux-flake-runner-contract =
    pkgs.runCommand "nudox-linux-flake-runner-contract" { nativeBuildInputs = [ pkgs.python3 ]; }
      ''
        cp ${../ci/linux-flake-check.py} linux-flake-check.py
        cp ${../ci/test_linux_flake_check.py} test_linux_flake_check.py
        python3 test_linux_flake_check.py
        touch "$out"
      '';

  nushell-command = commands.backendCi;
  agent-skills = commands.agentSkills;
  formatting = formatting.check;
  surface-cargo-lock-contract = surfaceCargoLockContract;

  # These files are compile-time byte authorities in local-service. Check the
  # actual filtered product source without compiling the product first.
  packaged-source-contract =
    if tools.guiRuntime == null then
      pkgs.writeText "packaged-source-contract-config-only" "No product workspace in configuration-only snapshot."
    else
      pkgs.runCommand "nudox-packaged-source-contract" { nativeBuildInputs = [ pkgs.diffutils ]; } ''
        for script in cargo-shared-cache.sh cargo-rustc-cache.sh; do
          cmp "${tools.guiRuntime.src}/.config/scripts/$script" \
            "${workspaceRoot}/.config/scripts/$script"
        done
        touch "$out"
      '';

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
          pkgs.nushell
        ];
      }
      ''
        # The test finds its sibling scripts (cargo-in.sh, the rustc cache
        # wrapper, the provenance helper) from its own location, so run it
        # from a tree with the repository's layout rather than the store.
        mkdir -p repo/.config/scripts repo/.config/ci repo/tests \
          repo/frontends/rust/fixtures/toml_pin \
          repo/apps/desktop/tests/fixtures/browse_tree
        cp -r ${../scripts}/. repo/.config/scripts/
        cp ${../ci/linux.nu} repo/.config/ci/linux.nu
        cp ${../ci/lib.nu} repo/.config/ci/lib.nu
        cp ${../../frontends/rust/fixtures/toml_pin/Cargo.lock} repo/frontends/rust/fixtures/toml_pin/Cargo.lock
        cp ${../../apps/desktop/tests/fixtures/browse_tree/Cargo.lock} repo/apps/desktop/tests/fixtures/browse_tree/Cargo.lock
        cp ${../../tests/cargo-shared-cache.sh} repo/tests/cargo-shared-cache.sh
        NUDOX_LINUX_CI_REPO="$PWD/repo" \
        NUDOX_CARGO_CACHE_SCRIPT="$PWD/repo/.config/scripts/cargo-shared-cache.sh" \
          ${pkgs.bash}/bin/bash repo/tests/cargo-shared-cache.sh
        mkdir -p $out/share
        echo validated > $out/share/cargo-cache-protocol
      '';

  # Exercise the small bound used by the two backend buildRustPackage
  # products without evaluating or building either product derivation.
  cargo-build-job-cap =
    pkgs.runCommand "nudox-cargo-build-job-cap" { nativeBuildInputs = [ pkgs.bash ]; }
      ''
        ${pkgs.bash}/bin/bash ${../scripts/test-nix-cargo-jobs.sh} ${../scripts/nix-cargo-jobs.sh}
        touch "$out"
      '';

  # Guard the public `luna-tools` shell's actual Nix requisites. Checking
  # PATH alone would allow a wrapper to hide an accidental backend/GUI
  # dependency. The complete role bundle is checked separately below.
  luna-tools-closure = helpers.nuCheck {
    inherit pkgs;
    name = "nudox-luna-tools-closure";
    packages = [ lunaTools ];
    build = ''
      # The managed Cargo shim prepares its cache even for --version. Nix's
      # default /homeless-shelter is intentionally not writable in a sandbox.
      $env.HOME = ($env.TMPDIR | path join "luna-tools-home")
      mkdir $env.HOME
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
      BACKEND_CONFIG_SNAPSHOT = "${../.}";
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
      # dylint/test.nu starts a nested Cargo build; openssl-sys needs the
      # discovery tool in this isolated check.
      pkgs.pkg-config
      pkgs.zlib
    ];
    environment = {
      BACKEND_DYLINT_ROOT = "${../dylint}";
      BACKEND_DYLINT_TOOLCHAIN = toolchains.dylintToolchain;
      BACKEND_DYLINT_DECLARATIONS = builtins.toJSON (
        map (rule: rule.id) (builtins.filter (rule: rule.engine == "dylint") control.lint.rules)
      );
      BACKEND_NIGHTLY_CARGO = toolchains.nightlyCargo;
      # The nested Cargo build runs in the build sandbox, which has no
      # network (on Linux; Darwin builds unsandboxed by default): its
      # dependencies come vendored from the lint crate's own lockfile, plus
      # the pinned graph of the rustc driver Dylint builds on first use.
      BACKEND_DYLINT_VENDOR = "${pkgs.symlinkJoin {
        name = "backend-dylint-vendor";
        paths = map (lockFile: pkgs.rustPlatform.importCargoLock { inherit lockFile; }) [
          ../dylint/Cargo.lock
          ../dylint/driver/Cargo.lock
        ];
      }}";
      # nuenv only adds packages to PATH; it does not run stdenv setup hooks
      # that would discover OpenSSL's .pc file automatically.
      PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
      LIBRARY_PATH = pkgs.lib.makeLibraryPath [
        pkgs.libiconv
        pkgs.zlib
      ];
    }
    # `apple-sdk` refuses to evaluate on Linux. Omit `SDKROOT` there; Darwin
    # still receives the MacOSX SDK root the semantic-lint toolchain expects.
    // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
      SDKROOT = pkgs.apple-sdk.sdkroot;
    };
    build = ''
      # Fail before the expensive nested Cargo build if native discovery drifts.
      pkg-config --modversion openssl
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
      commands.backendCi
      pkgs.ast-grep
      pkgs.nushell
    ];
    environment = {
      BACKEND_AST_GREP = "${astGrepSuite}/sgconfig.yml";
      BACKEND_CONFIG_MODE = "immutable";
      BACKEND_CONFIG_SNAPSHOT = "${../.}";
      BACKEND_CONTROL_PLANE = "${controlFile}/share/backend/control-plane.json";
    };
    build = ''
      backend agents verify
      # From the .config snapshot rather than a lone copy of tests.nu, so its
      # relative `use ../ci/emulated.nu` resolves.
      nu --no-config-file ${../.}/nu/tests.nu
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
    ]
    ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.procps ]
    ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.darwin.ps ];
    environment = {
      BACKEND_CONFIG_MODE = "immutable";
      BACKEND_CONFIG_SNAPSHOT = "${../.}";
      # Retain source context so remote/sandboxed builds receive the tree.
      # toString loses the input dependency even when it prints a store path.
      BACKEND_WORKSPACE_SNAPSHOT = "${workspaceRoot}";
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
// pkgs.lib.optionalAttrs (localHostRuntimeContract != null) {
  local-host-runtime-contract = localHostRuntimeContract;
}
