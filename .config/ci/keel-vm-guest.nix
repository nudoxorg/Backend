{ lib, pkgs, keelGuestCommand, ... }:

# This module supplies guest isolation only. The caller must supply a
# store-backed command that verifies and runs the exact artifact inventory.
let
  run = pkgs.writeShellScript "keel-vm-guest" ''
    set -u
    result=/var/lib/keel-results
    mkdir -p "$result" || exit 125
    exec > "$result/guest.log" 2>&1
    finish() {
      status=$?
      printf '%s\n' "$status" > "$result/guest-exit-status.tmp"
      mv "$result/guest-exit-status.tmp" "$result/guest-exit-status"
    }
    trap finish EXIT

    group=$(${pkgs.gawk}/bin/awk -F: '$1 == "0" && $2 == "" {print $3}' /proc/self/cgroup)
    case "$group" in /*) ;; *) echo 'no cgroup v2 membership' >&2; exit 125;; esac
    export KEEL_CGROUP_ROOT="/sys/fs/cgroup$group"
    export KEEL_REQUIRE_CGROUP=1
    if [ ! -w "$KEEL_CGROUP_ROOT/cgroup.procs" ]; then
      echo 'delegated cgroup is not writable' >&2
      exit 125
    fi
    if ! mkdir "$KEEL_CGROUP_ROOT/keel-preflight"; then
      echo 'cannot create delegated child cgroup' >&2
      exit 125
    fi
    if [ ! -w "$KEEL_CGROUP_ROOT/keel-preflight/cgroup.kill" ]; then
      echo 'delegated child cannot be killed' >&2
      rmdir "$KEEL_CGROUP_ROOT/keel-preflight" || true
      exit 125
    fi
    rmdir "$KEEL_CGROUP_ROOT/keel-preflight" || exit 125
    ${lib.escapeShellArg (toString keelGuestCommand)}
  '';
in {
  virtualisation = {
    graphics = false;
    cores = 2;
    memorySize = 4096;
    restrictNetwork = true;
    qemu.forceAccel = true;
  };
  users.users.keel = { isSystemUser = true; group = "keel"; };
  users.groups.keel = {};
  systemd.services.keel-artifact-run = {
    description = "Run the artifact-backed Keel inventory";
    after = [ "local-fs.target" ];
    serviceConfig = {
      Type = "oneshot";
      User = "keel";
      Group = "keel";
      Delegate = true;
      ExecStart = run;
    };
  };
  system.stateVersion = "25.11";
}
