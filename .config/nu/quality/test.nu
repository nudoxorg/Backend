# Executes tests at a scope explicitly bounded by actor and change impact.
# Makes nextest groups the concurrency and scarce-resource ownership authority.
# Prevents implementors and researchers from silently escalating to broad suites.

# Runs the assignment-owned opaque evaluator and returns only normalized feedback.
# @class implementation-feedback
def "main test" []: nothing -> string {
    require-command "test"
    let started_at = date now
    let evaluator = $env.BACKEND_AGENT_EVALUATOR? | default ""
    if ($evaluator | is-empty) or not ($evaluator | path exists) {
        tooling-fail "missing-agent-evaluator" "the assignment did not provide its opaque evaluator"
    }
    let evaluator_root = $env.BACKEND_AGENT_EVALUATOR_ROOT? | default ""
    let expected_digest = $env.BACKEND_AGENT_EVALUATOR_DIGEST? | default ""
    if ($evaluator_root | is-empty) or ($expected_digest | is-empty) {
        tooling-fail "unbound-agent-evaluator" "the assignment did not bind its evaluator root and digest"
    }
    let selected = $evaluator | path expand
    let admitted_root = $evaluator_root | path expand
    if not (($selected == $admitted_root) or ($selected | str starts-with $"($admitted_root)/")) {
        tooling-fail "evaluator-path-escape" "the opaque evaluator is outside its card-owned root"
    }
    let evaluator_size = ls $selected | get size | first | into int
    if $evaluator_size > 1048576 {
        tooling-fail "evaluator-size" "the opaque evaluator exceeds the one-megabyte admission bound"
    }
    if (open --raw $selected | hash sha256) != $expected_digest {
        tooling-fail "evaluator-digest-mismatch" "the opaque evaluator differs from the card-bound identity"
    }
    let result = process-require $selected
    let report = try {
        $result.stdout | from json
    } catch {
        tooling-fail "invalid-agent-evaluator" "the opaque evaluator did not return its bounded JSON protocol"
    }
    let expected = ["feedback" "schema" "status"]
    if (($report | columns | sort) != $expected) or ($report.schema != 1) {
        tooling-fail "invalid-agent-evaluator" "the opaque evaluator returned an unknown record shape"
    }
    if $report.status not-in ["GREEN" "RED"] {
        tooling-fail "invalid-agent-evaluator" "the opaque evaluator returned an unknown status"
    }
    if ($report.feedback | describe) !~ '^(list|table)' {
        tooling-fail "invalid-agent-evaluator" "the opaque evaluator feedback must be a bounded list"
    }
    if ($report.feedback | length) > 16 {
        tooling-fail "invalid-agent-evaluator" "the opaque evaluator returned more than 16 findings"
    }
    for finding in $report.feedback {
        if ($finding | columns | sort) != ["code" "message"] {
            tooling-fail "invalid-agent-evaluator" "each opaque finding must contain only code and message"
        }
        if $finding.code !~ '^[a-z][a-z0-9-]{0,63}$' {
            tooling-fail "invalid-agent-evaluator" "opaque finding codes must be bounded kebab-case identifiers"
        }
        if ($finding.message | str length) > 4096 {
            tooling-fail "invalid-agent-evaluator" "opaque finding messages may not exceed 4096 characters"
        }
    }
    let evaluator_status = if $report.status == "GREEN" { 0 } else { 1 }
    let evaluator_fingerprint = if $evaluator_status == 0 {
        ""
    } else {
        $report.feedback | each {|finding| $finding.code } | to json | hash sha256
    }
    record-tooling-event "test" $started_at $evaluator_status ($result.stdout | str length) 0 "result" "opaque-evaluator" $evaluator_fingerprint | ignore
    $report | to json --indent 2
}

# Materializes one run-scoped nextest config so parallel JUnit evidence cannot race.
def nextest-invocation [profile: string]: nothing -> record {
    let run_id = $"($profile)-(date now | format date '%Y%m%dT%H%M%S%.fZ')-(random uuid)"
    let local = (local-root)
    let config = $local | path join "nextest" "configs" $"($run_id).toml"
    mkdir ($config | path dirname)
    let declaration = (open $env.BACKEND_NEXTEST_CONFIG)
    let store = $declaration.store | upsert dir ($local | path join "nextest" "runs" $run_id)
    $declaration | upsert store $store | to toml | save --raw $config
    {
        id: $run_id
        config: $config
        evidence: ($store.dir | path join $profile)
    }
}

# Runs unit tests for packages selected by changed paths.
# @class verification
def "main test changed" [--base: string]: nothing -> record {
    require-command "test-changed"
    let packages = (required-changed-package-names --base $base)
    let package_arguments = $packages | each {|name| ["--package" $name] } | flatten
    let invocation = (nextest-invocation "default")
    process-require $env.BACKEND_STABLE_CARGO (
        [
            "nextest"
            "run"
            "--locked"
            "--no-tests=fail"
            "--config-file" $invocation.config
            "--profile" "default"
            "-E" "kind(lib)"
        ]
        | append $package_arguments
    ) | ignore
    {
        level: "unit"
        packages: $packages
        run: $invocation.id
        evidence: $invocation.evidence
    }
}

# Runs all affected targets for changed packages after Terra owns closure.
# @class expanded-verification
def "main test affected" [--base: string]: nothing -> record {
    require-command "test-affected"
    let packages = (required-changed-package-names --base $base)
    let package_arguments = $packages | each {|name| ["--package" $name] } | flatten
    let invocation = (nextest-invocation "affected")
    process-require $env.BACKEND_STABLE_CARGO (
        [
            "nextest"
            "run"
            "--locked"
            "--no-tests=fail"
            "--config-file" $invocation.config
            "--profile" "affected"
        ]
        | append $package_arguments
    ) | ignore
    {
        level: "affected"
        packages: $packages
        owner: (active-role)
        run: $invocation.id
        evidence: $invocation.evidence
    }
}

# Runs the complete workspace only for a short-lived Sol integration closure.
# @class closure
def "main test workspace" []: nothing -> record {
    require-command "test-workspace"
    let invocation = (nextest-invocation "closure")
    process-require $env.BACKEND_STABLE_CARGO [
        "nextest"
        "run"
        "--locked"
        "--no-tests=fail"
        "--workspace"
        "--config-file" $invocation.config
        "--profile" "closure"
    ] | ignore
    {
        level: "workspace"
        owner: (active-role)
        run: $invocation.id
        evidence: $invocation.evidence
    }
}

# Required PR workspace lane. Three diagnostic corpus sweeps remain in the
# deep lane (`test workspace`): the 1,000-package sequential fleet audit, the
# 210-case semantic-gap census, and the 20-crate Rust feature/publication
# census. They report product-roadmap gaps, not platform build/runtime health.
# Keep every exclusion exact so new tests join the required gate by default.
# @class closure
def "main test pr" []: nothing -> record {
    require-command "test-workspace"
    let cargo = ($env.BACKEND_PARALLEL_CARGO? | default $env.BACKEND_STABLE_CARGO)
    # A test compile is not a product build. Require every shipped process
    # surface explicitly before running the platform's behavioral closure.
    process-require $cargo [
        "build"
        "--locked"
        "--package" "backend-cli"
        "--package" "backend-mcp"
        "--package" "backend-locald"
        "--package" "backend-worker"
        "--package" "backend-desktop"
    ] | ignore
    let invocation = (nextest-invocation "pr")
    process-require $cargo [
        "nextest"
        "run"
        "--locked"
        "--no-tests=fail"
        "--workspace"
        "--no-fail-fast"
        "--config-file" $invocation.config
        "--profile" "pr"
        "-E"
        "not test(real_package_inventory_keeps_source_provenance_and_closed_terminals) and not test(all_two_hundred_ten_cases_compare_source_to_ir_publish_reopen_and_render) and not test(twenty_real_crates_compile_with_decoded_lanes)"
    ] | ignore
    {
        level: "pr"
        owner: (active-role)
        run: $invocation.id
        evidence: $invocation.evidence
    }
}
