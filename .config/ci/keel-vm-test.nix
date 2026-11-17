{ pkgs, bundle, sourceSha, inventory, sourceTree, driver, compilerEnvironment, compilerPath, python, runtimeRoots, runNonce }:
let
  lib = pkgs.lib;
  safeEnvironment = lib.filterAttrs (name: _: builtins.match "[A-Za-z_][A-Za-z0-9_]*" name != null
    && !(builtins.elem name [ "HOME" "NUDOX_CARGO_HOME" "KEEL_CGROUP_ROOT" "KEEL_REQUIRE_CGROUP" ])) compilerEnvironment;
  guestCommand = pkgs.writeShellScript "keel-artifact-command" ''
    set -eu
    ${lib.concatStringsSep "\n" (lib.mapAttrsToList (name: value: "export ${name}=${lib.escapeShellArg (toString value)}") safeEnvironment)}
    export PATH=${lib.escapeShellArg "${compilerPath}/bin"}:"$PATH"
    export NUDOX_CARGO_HOME="$(${pkgs.coreutils}/bin/mktemp -d)"
    export HOME="$NUDOX_CARGO_HOME"
    unset NUDOX_TYPESCRIPT NUDOX_TYPESCRIPT_ROOT || true
    exec ${python}/bin/python3 ${./keel-vm-run.py} \
      ${lib.escapeShellArg sourceSha} ${lib.escapeShellArg (toString bundle)} \
      ${lib.escapeShellArg (toString inventory)} ${lib.escapeShellArg "${driver}/bin/backend-keel-harness"} \
      /var/lib/keel-results/evidence
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
          machine.succeed("install -d -m 0777 /var/lib/keel-results")
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
in assert lib.all (name: builtins.hasAttr name safeEnvironment) [ "NUDOX_CARGO" "NUDOX_RUSTC" "NUDOX_PYTHON" ];
test.overrideAttrs (old: {
  requiredSystemFeatures = (old.requiredSystemFeatures or []) ++ [ "kvm" "nixos-test" ];
})
