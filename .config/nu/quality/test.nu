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

# Names the tests nextest counted but never finished. nextest reports only
# "N tests were not run" once any test fails, which cannot be acted on.
def report-unfinished-tests [cargo: string, filter: string, invocation: record]: nothing -> nothing {
    let junit = $invocation.evidence | path join "junit.xml"
    if not ($junit | path exists) {
        print --stderr $"no JUnit report at ($junit); cannot name unfinished tests"
        return
    }
    let listed = (
        process-result $cargo [
            "nextest"
            "list"
            "--locked"
            "--workspace"
            "--config-file" $invocation.config
            "--profile" "pr"
            "--message-format" "oneline"
            "-E" $filter
        ]
    )
    if $listed.status != 0 {
        print --stderr $"nextest list exited ($listed.status); cannot name unfinished tests"
        return
    }
    let expected = $listed.stdout | lines | where {|line| $line | is-not-empty }
    let finished = (
        open $junit
        | get content
        | where tag == "testsuite"
        | each {|suite|
            $suite.content
            | where tag == "testcase"
            | each {|case| $"($case.attributes.classname) ($case.attributes.name)" }
        }
        | flatten
    )
    let unfinished = $expected | where {|test| $test not-in $finished }
    print --stderr $"== ($unfinished | length) of ($expected | length) listed tests never finished =="
    for test in $unfinished {
        print --stderr $"NOT FINISHED ($test)"
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
    let cargo = $env.BACKEND_PARALLEL_CARGO? | default $env.BACKEND_STABLE_CARGO
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
    # The PR lane proves the product works: every shipped binary builds, and
    # unit, MCP, CLI, locald, desktop, journey, storage, and crash tests pass.
    # Deep engine-accuracy suites (real-package corpora, pinned reference and
    # render snapshots, native-compiler probes, timing budgets) are slow and
    # platform-sensitive; they stay in `backend test workspace` and do not
    # gate a PR. The semantic-history journey joins them: on the Linux worker
    # its real rust-analyzer publication reads back "rejected" with no daemon
    # diagnostic (builds 2411, 2469), and it needs its own investigation.
    let deep_accuracy = "binary_id(/^backend-engine::.*(corpus|repro|snapshot|terminals|golden|fleet|lane|render|image|lifecycle|packaging|identity_regressions)/) or binary_id(/^backend-flow::(compiler|system)_corpus$/) or binary_id(/^backend-semantic::render_snapshot_corpus$/) or package(backend-performance-tests)"
    let quarantine = (pr-quarantine | each {|entry| $"\(($entry.filter)\)" } | str join " or ")
    let filter = $"not \(($deep_accuracy)\) and not \(($quarantine)\) and not test\(real_package_inventory_keeps_source_provenance_and_closed_terminals\) and not test\(all_two_hundred_ten_cases_compare_source_to_ir_publish_reopen_and_render\) and not test\(twenty_real_crates_compile_with_decoded_lanes\) and not test\(semantic_version_selection_is_exact_and_durable_across_restart\)"
    # Contention proofs (dozens of writers, thousands of keys) starve the
    # timing-sensitive tests beside them (build 2397), and the nextest group
    # that once throttled them never started any of them on Linux (builds
    # 2321-2390). Run them in their own pass, two at a time, after the rest;
    # both passes always run so one red run reports every failure.
    let contention = "test(/loom|contention|concurrent/)"
    let passes = [
        {
            filter: $"($filter) and not \(($contention)\)"
            threads: null
        }
        {
            filter: $"($filter) and \(($contention)\)"
            threads: "2"
        }
    ]
    let results = $passes | each {|pass| run-pr-pass $cargo $pass.filter $pass.threads }
    let failures = $results | where failure != null
    if not ($failures | is-empty) {
        error make {
            msg: ($failures | get failure.msg | str join "; ")
        }
    }
    {
        level: "pr"
        owner: (active-role)
        run: ($results | get invocation.id)
        evidence: ($results | get invocation.evidence)
    }
}

# Tests held out of the PR lane, each with the error that put it here. Every
# entry still runs in `backend test workspace`. Remove an entry as soon as
# its cause is fixed; this list is debt, not policy.
def pr-quarantine []: nothing -> list<record<filter: string, reason: string>> {
    # Canonical regressions from the fe0c4e979 merge (build 2606). These
    # tests passed on this branch in build 2490; the product code under them
    # changed in canonical without the tests.
    let regressions = [
        {
            filter: "test(/^registry_package_journey_(cpp|csharp|java|go)$/) or test(=remote_add_materializes_searchable_rows_and_reuses_cursor_after_restart) or test(=registry_archive_cache_reuses_published_object_after_process_restart) or test(=unix_journeys::real_locald_socket_dispatches_to_worker_then_reuses_fallback_bytes)"
            reason: "registry add refuses: 'publish product source view: indexed package ... has no staged source manifest'"
        }
        {
            filter: "binary_id(/^backend-journeys::new_user_(catalog|follow|projects|versions)$/)"
            reason: "CLI package-versions: 'read indexed project src/lib.rs: Not a directory (os error 20)'"
        }
        {
            filter: "test(=every_surface_variant_crosses_the_real_cli_and_mcp_processes) or test(=every_surface_renders_identity_equal_content_for_the_same_revision) or test(=production_surface_matrix_is_identity_equal_across_languages_and_restarts)"
            reason: "MCP tools/list lost backend.dependencies; a documented card query fails; 'declaration row external semantic target has no source path'"
        }
        {
            filter: "test(=builtin::commands::semantic_query::project_call_tests::project_references_reads_are_not_callers)"
            reason: "'Reads foreign link produced 1 sync-site facts'"
        }
        {
            filter: "test(=every_board_reads_real_content_through_the_desktop_runtime)"
            reason: "new desktop content_truth test: 'references gap'"
        }
        {
            filter: "test(=generic_box_flow) or test(=explicit_events_flow) or binary_id(backend-engine::typescript_ambient_index_signatures) or test(=ambient_async_transform_index_signatures_keep_distinct_object_owners) or test(=nested_object_literal_types_lower_on_a_small_stack) or test(=fifteen_additional_real_sdists_preserve_source_facts)"
            reason: "engine lowering: Java missing dependency is no longer UnresolvedDependencies, C# explicit events lose is_explicit_interface, TypeScript ambient index signatures refuse to lower, nested object types overflow the small stack, Python sdist facts drift"
        }
        {
            filter: "test(=a_missing_import_is_dependencies_unresolved_not_a_sealed_package) or test(=module_root_is_excluded_and_its_absence_stays_a_typed_compilation_refusal) or test(=dotnet_regeneration_is_byte_exact_and_deterministic) or test(=legacy::checker::tests::probe_plan_splices_and_appends_exactly)"
            reason: "frontend producers: Java refusals and C# regenerated bytes differ; Python probe plan errors with Plan"
        }
    ]
    # The Linux worker's headless GPUI has no direct offscreen renderer
    # (Metal only; see tools/gui-harness/src/gpui_driver.rs), and its native
    # geometry and timings differ from the macOS values these tests pin.
    let linux_platform = [
        {
            filter: "package(backend-facet) and (test(/^graph::gallery::adversarial::/) or test(=data::tests::harness_storms_over_the_marks_find_nothing) or test(=overlay::float::storm::float_storm_keeps_every_invariant_and_settles_to_a_fresh_boot))"
            reason: "pixel captures need the macOS offscreen renderer: 'the current GPUI platform has no direct offscreen renderer'"
        }
        {
            filter: "package(backend-facet) and (test(=motion::shared::tests::across_views::a_retained_target_consumes_each_explicit_handoff_once) or test(=motion::tests::a_class_change_mid_drag_springs_from_the_painted_position_and_keeps_following) or test(=graph::view::tests::brief_hover_handoff_preserves_the_stronger_departing_envelope) or test(=graph::view::tests::cold_discovery_completion_after_blur_cannot_restart_search))"
            reason: "exact float and native-timing values recorded on macOS (98.0 vs 97.99999, spring jumps, native input blur)"
        }
        {
            filter: "package(backend-desktop) and test(=shell::tests::native_graph_handoff_uses_the_scaled_translated_canvas_and_rejects_absent_sources)"
            reason: "composited bounds differ by sub-pixel snapping (734.75 vs 734.6)"
        }
    ]
    if $nu.os-info.name == "linux" {
        $regressions | append $linux_platform
    } else {
        $regressions
    }
}

# Runs one nextest pass of the PR lane with its own JUnit evidence, and names
# any listed test that never finished when the pass fails.
def run-pr-pass [cargo: string, filter: string, threads]: nothing -> record {
    let invocation = (nextest-invocation "pr")
    let thread_arguments = if $threads == null { [] } else { ["--test-threads" $threads] }
    let failure = (
        try {
            process-require $cargo (
                [
                    "nextest"
                    "run"
                    "--locked"
                    "--no-tests=fail"
                    "--workspace"
                    "--no-fail-fast"
                    "--config-file" $invocation.config
                    "--profile" "pr"
                ]
                | append $thread_arguments
                | append ["-E" $filter]
            ) | ignore
            null
        } catch {|error| $error }
    )
    if $failure != null {
        report-unfinished-tests $cargo $filter $invocation
    }
    {invocation: $invocation, failure: $failure}
}
