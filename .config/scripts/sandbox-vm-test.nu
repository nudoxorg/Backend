#!/usr/bin/env nu

# Run the sandbox's real microVM tests. This command is deliberately strict:
# an absent launcher/rootfs is a blocker, not a passing test run.

def require-command [name: string] {
    if (which $name | is-empty) {
        print --stderr $"BLOCKED: ($name) is not on PATH; enter the Nix dev shell or provide the pinned smolvm launcher"
        exit 2
    }
}

def run-required [label: string, argv: list<string>] {
    print $"==> ($label)"
    let result = (^$argv.0 ...($argv | skip 1) -- --ignored --nocapture | complete)
    if ($result.stdout | str trim) != "" {
        print $result.stdout
    }
    if ($result.stderr | str trim) != "" {
        print --stderr $result.stderr
    }
    if $result.exit_code != 0 {
        print --stderr $"FAIL: ($label) (exit ($result.exit_code)); no VM result was synthesized"
        exit $result.exit_code
    }
}

def main [] {
    let rootfs = ($env.NUDOX_GUEST_ROOTFS? | default "")
    if $rootfs == "" {
        print --stderr "BLOCKED: NUDOX_GUEST_ROOTFS is unset; provide a prepared smolvm guest rootfs"
        exit 2
    }
    if not ($rootfs | path exists) {
        print --stderr $"BLOCKED: NUDOX_GUEST_ROOTFS does not exist: ($rootfs)"
        exit 2
    }
    if not (($rootfs | path join "base") | path exists) {
        print --stderr $"BLOCKED: NUDOX_GUEST_ROOTFS has no base/ guest image: ($rootfs)"
        exit 2
    }

    require-command smolvm

    print $"==> real sandbox VM tests \(rootfs: ($rootfs)\)"
    run-required "sandbox backend VM tests" [cargo test -p sandbox --lib]
    run-required "sandbox cage VM tests" [cargo test -p sandbox --test escape]
    print "PASS: real sandbox VM tests"
}
