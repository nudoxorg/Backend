# The CI worker must explicitly delegate this boundary. The runner validates
# the cgroup-v2 mount and creates only private children inside the declared root.
export def required-keel-isolation []: nothing -> record {
    let declared = $env.KEEL_CGROUP_ROOT? | default ""
    if ($declared | is-empty) {
        error make {msg: "CI problem: Keel requires an explicitly delegated KEEL_CGROUP_ROOT; the Linux product gate cannot use process-group fallback"}
    }
    if not ($declared | str starts-with "/") {
        error make {msg: "CI problem: KEEL_CGROUP_ROOT must be an absolute delegated cgroup-v2 directory"}
    }
    let root = $declared | path expand --strict
    for control in ["cgroup.controllers" "cgroup.procs" "cgroup.events"] {
        if not ($root | path join $control | path exists) {
            error make {msg: $"CI problem: delegated Keel root lacks ($control): ($root)"}
        }
    }
    {KEEL_REQUIRE_CGROUP: "1", KEEL_CGROUP_ROOT: $root}
}
