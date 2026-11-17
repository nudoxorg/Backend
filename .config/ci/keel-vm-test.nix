{ pkgs, bundle, sourceSha, inventory, sourceTree, driver, compilerEnvironment, compilerPath, python, runtimeRoots, runNonce }:
let
  lib = pkgs.lib;
  allowedCompilerKeys = [
    "BACKEND_LOCALD_DEFAULT_PYTHON" "COMPILER_PYTHON_COMPILER" "COMPILER_STABLE_TOOLCHAIN"
    "COMPILER_TYPESCRIPT_COMPILER" "LIBRARY_PATH" "NIX_BINTOOLS_WRAPPER_TARGET_HOST_x86_64_unknown_linux_gnu"
    "NIX_CC_WRAPPER_TARGET_HOST_x86_64_unknown_linux_gnu" "NIX_CFLAGS_COMPILE" "NIX_CFLAGS_COMPILE_BEFORE"
    "NIX_CFLAGS_LINK" "NIX_CXXSTDLIB_COMPILE" "NIX_CXXSTDLIB_LINK" "NIX_ENFORCE_NO_NATIVE"
    "NIX_HARDENING_ENABLE" "NIX_LDFLAGS" "NIX_LDFLAGS_BEFORE"
    "NIX_PKG_CONFIG_WRAPPER_TARGET_HOST_x86_64_unknown_linux_gnu" "NUDOX_CARGO" "NUDOX_PYREFLY"
    "NUDOX_PYREFLY_BIN" "NUDOX_PYTHON" "NUDOX_RUSTC" "NUDOX_TYPESCRIPT_CHECKER_BIN"
    "NUDOX_TYPESCRIPT_REPORT_PROGRAM" "RUSTC"
  ] ++ map (n: "BACKEND_LOCALD_PYTHON_CANDIDATE_${toString n}") (lib.range 1 11);
  safeEnvironment = compilerEnvironment;
  guestCommand = pkgs.writeShellScript "keel-artifact-command" ''
    set -eu
    ${lib.concatStringsSep "\n" (lib.mapAttrsToList (name: value: "export ${name}=${lib.escapeShellArg (toString value)}") safeEnvironment)}
    export PATH=${lib.escapeShellArg "${compilerPath}/bin"}:"$PATH"
    export NUDOX_CARGO_HOME="$(${pkgs.coreutils}/bin/mktemp -d)"
    export HOME="$NUDOX_CARGO_HOME"
    unset NUDOX_TSC NUDOX_TYPESCRIPT_NODE NUDOX_TYPESCRIPT_MODULE_ROOT NUDOX_TYPESCRIPT_REPORT_PROGRAM || true
    exec ${python}/bin/python3 ${./keel-vm-run.py} \
      ${lib.escapeShellArg sourceSha} ${lib.escapeShellArg (toString bundle)} \
      ${lib.escapeShellArg (toString inventory)} ${lib.escapeShellArg "${driver}/bin/backend-keel-harness"} \
      /var/lib/keel-results/evidence ${lib.escapeShellArg "${sourceTree}/.config/ci/t1_artifacts.py"}
  '';
  test = pkgs.nixosTest {
    name = "keel-artifact-inventory";
    nodes.machine = {
      imports = [ ./keel-vm-guest.nix ];
      _module.args.keelGuestCommand = guestCommand;
      virtualisation.additionalPaths = [ bundle inventory sourceTree driver compilerPath python ] ++ runtimeRoots;
    };
    testScript = ''
      # A fresh nonce forces execution for every CI run.
      run_nonce = ${builtins.toJSON runNonce}
      import os
      from pathlib import Path

      failure = None
      try:
          machine.start()
          machine.wait_for_unit("multi-user.target")
          machine.succeed("install -d -o keel -g keel -m 0700 /var/lib/keel-results")
          machine.succeed("systemctl start --no-block keel-artifact-run.service")
          machine.wait_until_succeeds("test -f /var/lib/keel-results/guest-exit-status", timeout=3600)
      except Exception as exc:
          failure = repr(exc)
      try:
          machine.execute("journalctl -u keel-artifact-run.service > /var/lib/keel-results/service.journal 2>&1")
          machine.copy_from_vm("/var/lib/keel-results")
      except Exception as exc:
          failure = (failure or "") + " receipt collection: " + repr(exc)
      if failure is not None:
          Path(os.environ["out"], "guest-driver-error").write_text(failure + "\\n")
    '';
  };
in assert lib.all (name: builtins.elem name allowedCompilerKeys) (builtins.attrNames compilerEnvironment);
assert lib.all (name: builtins.hasAttr name safeEnvironment) [ "NUDOX_CARGO" "NUDOX_RUSTC" "NUDOX_PYTHON" ];
test.overrideAttrs (old: {
  requiredSystemFeatures = (old.requiredSystemFeatures or []) ++ [ "kvm" "nixos-test" ];
})
