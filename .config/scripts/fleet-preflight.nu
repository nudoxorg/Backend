#!/usr/bin/env nu

# Evidence preflight for claims that cannot be established by a Darwin-only
# checkout. This command never invents evidence: unavailable prerequisites are
# reported as BLOCKED with the exact missing artifact/tool.

def has-command [name: string] {
    not (which $name | is-empty)
}

def check [label: string, ok: bool, detail: string] {
    if $ok {
        print $"PASS ($label): ($detail)"
        true
    } else {
        print --stderr $"BLOCKED ($label): ($detail)"
        false
    }
}

def path-present [path: string] {
    $path | path exists
}

def vm-check [] {
    let rootfs = ($env.NUDOX_GUEST_ROOTFS? | default "")
    let rootfs_ok = ($rootfs != "" and (path-present $rootfs) and (path-present ($rootfs | path join "base")))
    let launcher_ok = has-command "smolvm"
    [
        (check "L26/rootfs" $rootfs_ok (
            if $rootfs_ok { $"NUDOX_GUEST_ROOTFS=($rootfs), base/ present" }
            else { "NUDOX_GUEST_ROOTFS is unset or has no base/ rootfs" }
        ))
        (check "L26/runtime" $launcher_ok (
            if $launcher_ok { "smolvm is on PATH" }
            else { "smolvm is not on PATH (libkrun alone cannot boot a guest)" }
        ))
    ] | all {|result| $result }
}

def linux-check [] {
    let os = (sys host | get name)
    let linux = ($os == "Linux")
    let kvm = (path-present "/dev/kvm")
    if not $linux {
        check "L48/linux" false $"host is ($os); Linux execution evidence is unavailable"
    } else {
        check "L48/linux" $kvm (
            if $kvm { "/dev/kvm is present" }
            else { "Linux host has no accessible /dev/kvm" }
        )
    }
}

def model-check [] {
    let reachable = (do -i { ^curl -sf -m 2 http://127.0.0.1:11434/api/tags } | complete).exit_code == 0
    let model = if $reachable {
        (do -i { ^curl -sf -m 2 http://127.0.0.1:11434/api/tags } | complete).stdout | str contains "nomic-embed-text"
    } else {
        false
    }
    let ok = $reachable and $model
    check "L41/model" $ok (
        if not $reachable { "Ollama is not reachable on 127.0.0.1:11434" }
        else if not $model { "nomic-embed-text is not provisioned in Ollama" }
        else { "nomic-embed-text is provisioned and Ollama is reachable" }
    )
}

def rss-check [] {
    let os = (sys host | get name)
    let supported = ($os == "Linux" or $os == "Darwin")
    # A platform counter is not evidence that the bounded multi-package
    # pressure workload actually ran. Keep this claim blocked until that
    # workload emits a measured RSS result; never report capability as coverage.
    check "L46-prop/RSS" false (
        if $supported {
            $"($os) exposes RSS [VmHWM or getrusage], but no bounded memory-pressure workload was run"
        } else {
            $"RSS measurement is unsupported on ($os); no memory-pressure claim can be made"
        }
    )
}

def vendor-check [] {
    let qdrant = [
        "workspace/vendor/qdrant-edge/cpp/quantization/sse.c"
        "workspace/vendor/qdrant-edge/cpp/quantization/avx2.c"
        "workspace/vendor/qdrant-edge/src/segment/spaces/metric_f16/cpp/neon.c"
    ] | all {|p| path-present $p }
    let dolt = (path-present "workspace/vendor/doltlite/doltlite.c")
    [
        (check "L7/qdrant-edge" $qdrant (
            if $qdrant { "required SIMD sources are present" }
            else { "required qdrant-edge SIMD sources are missing" }
        ))
        (check "L7/doltlite" $dolt (
            if $dolt { "doltlite.c is present" }
            else { "workspace/vendor/doltlite/doltlite.c is missing" }
        ))
    ] | all {|result| $result }
}

def main [
    --strict # Exit non-zero when any claim is blocked.
] {
    let results = [
        (vm-check)
        (linux-check)
        (model-check)
        (rss-check)
        (vendor-check)
    ]
    let blocked = ($results | where {|result| not $result } | length)
    print $"Fleet preflight: ($blocked) blocked claims"
    if $strict and $blocked > 0 {
        exit 1
    }
}
