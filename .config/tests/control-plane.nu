# Exercises the built command surface inside an isolated disposable Git worktree.
# Proves path safety, runtime role denial, structural findings, and skill rendering.
# Leaves no state outside the Nix-owned test directory supplied by the caller.

use std/assert

def role-environment [role: string, tool: string]: nothing -> record {
    let contract = open $env.BACKEND_CONTROL_PLANE | get roles | get $role
    {
        BACKEND_AGENT_ROLE: $role
        BACKEND_AGENT_TOOL: $tool
        BACKEND_AGENT_CONTRACT_DIGEST: $contract.contractDigest
        BACKEND_AGENT_RUN: "control-plane-fixture"
        BACKEND_AGENT_CARD: "control-plane-fixture"
        BACKEND_AGENT_CHANGE_DIGEST: "initial"
    }
}

let sandbox = (mktemp --directory --tmpdir backend-control-plane.XXXXXX)
cd $sandbox

^git init --quiet
^git config user.email "tooling@example.invalid"
^git config user.name "Tooling Test"

{
    workspace: {
        resolver: "3"
        members: ["crates/seed"]
        package: {
            version: "0.0.0"
            edition: "2024"
            rust-version: "1.85"
            license: "MIT"
            publish: false
        }
        lints: {
            rust: {unsafe_code: "deny"}
        }
    }
} | to toml | save --raw Cargo.toml
mkdir crates/seed
{
    package: {
        name: "fixture-seed"
        version: {workspace: true}
        edition: {workspace: true}
        rust-version: {workspace: true}
        license: {workspace: true}
        publish: {workspace: true}
    }
    lib: {path: "lib.rs"}
    lints: {workspace: true}
} | to toml | save --raw crates/seed/Cargo.toml
"//! Defines the fixture seed package.\n//! Keeps initial Cargo metadata nonempty.\n//! Provides no product behavior.\n" | save --raw crates/seed/lib.rs
^$env.BACKEND_STABLE_CARGO generate-lockfile
^git add Cargo.toml Cargo.lock crates/seed
^git commit --quiet --message "test: initialize fixture"

let doctor = (^backend doctor | complete)
assert equal $doctor.exit_code 0 $doctor.stderr

let scope = (^backend scope changed --base HEAD | complete)
assert equal $scope.exit_code 0 $scope.stderr

let purpose = "Defines a generated fixture file. Keeps path ownership explicit. Proves bounded creation behavior."
let workspace = (open Cargo.toml)
$workspace | upsert workspace.members ["crates/*"] | to toml | save --force Cargo.toml
let created = (
    ^backend create crate crates/example --name fixture-example --purpose $purpose | complete
)
assert equal $created.exit_code 0 $created.stderr
assert ($sandbox | path join "crates/example/lib.rs" | path exists)

let manifest = (open crates/example/Cargo.toml)
assert equal $manifest.package.name "fixture-example"
assert equal $manifest.lib.path "lib.rs"
assert equal $manifest.package.version.workspace true

^$env.BACKEND_STABLE_CARGO generate-lockfile
^git add crates/example Cargo.toml Cargo.lock
^git commit --quiet --message "test: add generated crate"

let source = (
    ^backend create file crates/example/model.rs --purpose $purpose | complete
)
assert equal $source.exit_code 0
let test_file = (
    ^backend create test crates/example public_contract --purpose $purpose | complete
)
assert equal $test_file.exit_code 0 $test_file.stderr
"#[test]\nfn generated_fixture_runs() {\n    assert_eq!(2 + 2, 4);\n}\n" | save --append crates/example/tests/public_contract.rs
let unit_file = (
    ^backend create file crates/example/tests.rs --purpose $purpose | complete
)
assert equal $unit_file.exit_code 0 $unit_file.stderr
"#[test]\nfn changed_scope_runs_unit_tests() {\n    assert_eq!(2 + 2, 4);\n}\n" | save --append crates/example/tests.rs
"#[cfg(test)]\nmod tests;\n" | save --append crates/example/lib.rs

let escaped = (
    ^backend create file ../escaped.rs --purpose $purpose | complete
)
assert not equal $escaped.exit_code 0
assert str contains $escaped.stderr "backend::path-escape"

let reviewer_write = (
    with-env (role-environment "terra-reviewer" "inspect") { ^backend create file crates/example/reviewer.rs --purpose $purpose | complete }
)
assert not equal $reviewer_write.exit_code 0
assert str contains $reviewer_write.stderr "backend::role-tool-mismatch"

let luna_measure = (
    with-env (role-environment "luna-pair" "test") { ^backend observe benchmark capacity-planning | complete }
)
assert not equal $luna_measure.exit_code 0
assert str contains $luna_measure.stderr "backend::role-tool-mismatch"

let luna_lint = (
    with-env (role-environment "luna-pair" "test") { ^backend lint changed --base HEAD | complete }
)
assert not equal $luna_lint.exit_code 0
assert str contains $luna_lint.stderr "backend::role-tool-mismatch"

let evaluator = $sandbox | path join ".local/opaque-evaluator.nu"
mkdir ($evaluator | path dirname)
'#!/usr/bin/env nu
{schema: 1, status: "RED", feedback: [{code: "fixture", message: "preserve the rejected value"}]} | to json
' | save --raw $evaluator
^chmod "0700" $evaluator
let opaque = (
    with-env {
        BACKEND_AGENT_ROLE: "luna-pair",
        BACKEND_AGENT_TOOL: "test"
        BACKEND_AGENT_CONTRACT_DIGEST: (open $env.BACKEND_CONTROL_PLANE | get roles.luna-pair.contractDigest)
        BACKEND_AGENT_RUN: "control-plane-fixture",
        BACKEND_AGENT_CARD: "opaque-evaluator",
        BACKEND_AGENT_CHANGE_DIGEST: "opaque-initial"
        BACKEND_AGENT_EVALUATOR: $evaluator
        BACKEND_AGENT_EVALUATOR_ROOT: ($evaluator | path dirname)
        BACKEND_AGENT_EVALUATOR_DIGEST: (open --raw $evaluator | hash sha256)
    } { ^backend test | from json }
)
assert equal $opaque.status "RED"
assert equal $opaque.feedback.0.code "fixture"

let event_root = $sandbox | path join ".local/observability/tooling/events"

let role_test = (
    with-env {
        BACKEND_AGENT_ROLE: "human",
        BACKEND_AGENT_RUN: "role-toolbox-fixture",
        BACKEND_AGENT_CARD: "opaque-evaluator",
        BACKEND_AGENT_CHANGE_DIGEST: "opaque-initial"
        BACKEND_AGENT_EVALUATOR: $evaluator
        BACKEND_AGENT_EVALUATOR_ROOT: ($evaluator | path dirname)
        BACKEND_AGENT_EVALUATOR_DIGEST: (open --raw $evaluator | hash sha256)
    } { ^($env.BACKEND_LUNA_TOOLS | path join "bin/test") | from json }
)
assert equal $role_test.status "RED"
let role_outcomes = (
    glob ($event_root | path join "*.json")
    | each {|path| open $path }
    | where {|event| (($event.role == "luna-pair") and ($event.run == ("role-toolbox-fixture" | hash sha256))) and ($event.scope == "opaque-evaluator") }
)
assert equal ($role_outcomes | length) 1 "one normalized evaluator outcome must be emitted per Luna test"
let role_outcome = $role_outcomes | first
assert equal $role_outcome.status "failed" "evaluator truth must remain distinct from successful process transport"
let nested_role_test = (
    do {
        cd crates/example
        with-env {
            BACKEND_AGENT_ROLE: "human",
            BACKEND_AGENT_RUN: "nested-role-toolbox-fixture",
            BACKEND_AGENT_CARD: "opaque-evaluator"
            BACKEND_AGENT_EVALUATOR: $evaluator
            BACKEND_AGENT_EVALUATOR_ROOT: ($evaluator | path dirname)
            BACKEND_AGENT_EVALUATOR_DIGEST: (open --raw $evaluator | hash sha256)
        } { ^($env.BACKEND_LUNA_TOOLS | path join "bin/test") | from json }
    }
)
assert equal $nested_role_test.status "RED" "role tools must derive one candidate from any directory inside its worktree"
let first_candidate_digest = (
    glob ($event_root | path join "*.json")
    | each {|path| open $path }
    | where {|event| ($event.role == "luna-pair") and ($event.phase == "admitted") and ($event.run == ("role-toolbox-fixture" | hash sha256)) }
    | first
    | get changed_paths_digest
)
"pub const CANDIDATE_MUTATION: u8 = 1;\n" | save --append crates/example/model.rs
let mutated_role_test = (
    with-env {
        BACKEND_AGENT_ROLE: "human",
        BACKEND_AGENT_RUN: "role-toolbox-fixture-mutated",
        BACKEND_AGENT_CARD: "opaque-evaluator"
        BACKEND_AGENT_EVALUATOR: $evaluator
        BACKEND_AGENT_EVALUATOR_ROOT: ($evaluator | path dirname)
        BACKEND_AGENT_EVALUATOR_DIGEST: (open --raw $evaluator | hash sha256)
    } { ^($env.BACKEND_LUNA_TOOLS | path join "bin/test") | from json }
)
assert equal $mutated_role_test.status "RED"
let second_candidate_digest = (
    glob ($event_root | path join "*.json")
    | each {|path| open $path }
    | where {|event| ($event.role == "luna-pair") and ($event.phase == "admitted") and ($event.run == ("role-toolbox-fixture-mutated" | hash sha256)) }
    | first
    | get changed_paths_digest
)
assert not equal $first_candidate_digest $second_candidate_digest "same-path content changes must alter candidate identity"

let role_inspect = ^($env.BACKEND_TERRA_TOOLS | path join "bin/inspect") | complete
assert equal $role_inspect.exit_code 0 $role_inspect.stderr
assert not (($env.BACKEND_LUNA_TOOLS | path join "bin/backend") | path exists) "role PATH must not expose the general backend binary"
let luna_commands = ls ($env.BACKEND_LUNA_TOOLS | path join "bin") | get name | path basename | sort
assert equal $luna_commands ["test"] "Luna bundle must expose exactly one opaque evaluator command"
let luna_skills = glob ($env.BACKEND_LUNA_TOOLS | path join "**/SKILL.md")
assert equal ($luna_skills | length) 1 "Luna bundle must carry exactly one role skill"
assert equal (
    $luna_skills
    | first
    | path dirname
    | path basename
) "luna-pair"
let terra_skills = glob ($env.BACKEND_TERRA_TOOLS | path join "**/SKILL.md")
assert equal ($terra_skills | length) 1 "Terra bundle must carry exactly one role skill"
assert equal (
    $terra_skills
    | first
    | path dirname
    | path basename
) "terra-academic"

let decision_root = $sandbox | path join ".local/agent/runs/control-plane"
mkdir $decision_root
let trajectory_root = $decision_root | path join "events"
mkdir $trajectory_root
let role_run = "role-toolbox-fixture" | hash sha256
let role_card = "opaque-evaluator" | hash sha256
glob ($event_root | path join "*.json")
| each {|path| {path: $path, event: (open $path)} }
| where {|row| ($row.event.role == "luna-pair") and ($row.event.run == $role_run) and ($row.event.card == $role_card) }
| enumerate
| each {|row| $row.item.event | to json | save --raw ($trajectory_root | path join $"($row.index).json") }
let luna_decision = $decision_root | path join "luna.json"
{
    role_id: "luna-pair"
    verdict: "RED"
    findings: [
        {
            severity: "high"
            kind: "product"
            law: "typed rejection"
            evidence: "The opaque evaluator retained the rejected value."
            redesign: "Return the red card to Terra without broadening scope."
        }
    ]
    candidate: null
    red_owner: "terra-academic"
    next_role: "terra-academic"
} | to json | save --raw $luna_decision
let trajectory_grade = ^backend agents grade luna-pair $luna_decision --events $trajectory_root | complete
assert equal $trajectory_grade.exit_code 0 $trajectory_grade.stderr

let invalid_route = $decision_root | path join "luna-invalid-route.json"
(open $luna_decision | upsert next_role "terra-reviewer") | to json | save --raw $invalid_route
let invalid_route_grade = ^backend agents grade luna-pair $invalid_route --events $trajectory_root | complete
assert not equal $invalid_route_grade.exit_code 0
assert str contains $invalid_route_grade.stderr "backend::invalid-decision-route"

let invalid_kind = $decision_root | path join "luna-invalid-kind.json"
(
    open $luna_decision
    | update findings.0.kind "style-opinion"
    | to json
    | save --raw $invalid_kind
)
let invalid_kind_grade = ^backend agents grade luna-pair $invalid_kind --events $trajectory_root | complete
assert not equal $invalid_kind_grade.exit_code 0
assert str contains $invalid_kind_grade.stderr "backend::invalid-finding-kind"

let green_evaluator = $sandbox | path join ".local/green-evaluator.nu"
'#!/usr/bin/env nu
{schema: 1, status: "GREEN", feedback: []} | to json
' | save --raw $green_evaluator
^chmod "0700" $green_evaluator
let green_run_name = "green-role-toolbox-fixture"

let green_result = (
    with-env {
        BACKEND_AGENT_RUN: $green_run_name
        BACKEND_AGENT_CARD: "green-evaluator"
        BACKEND_AGENT_EVALUATOR: $green_evaluator
        BACKEND_AGENT_EVALUATOR_ROOT: ($green_evaluator | path dirname)
        BACKEND_AGENT_EVALUATOR_DIGEST: (open --raw $green_evaluator | hash sha256)
    } { ^($env.BACKEND_LUNA_TOOLS | path join "bin/test") | from json }
)
assert equal $green_result.status "GREEN"
let green_events = (
    glob ($event_root | path join "*.json")
    | each {|path| {path: $path, event: (open $path)} }
    | where {|row| ($row.event.role == "luna-pair") and ($row.event.run == ($green_run_name | hash sha256)) }
)

let green_admissions = $green_events | where {|row| $row.event.phase == "admitted" }
assert equal ($green_admissions | length) 1
let green_candidate = $green_admissions | first | get event.changed_paths_digest
let green_root = $sandbox | path join ".local/agent/runs/green"
let green_trajectory = $green_root | path join "events"
mkdir $green_trajectory
$green_events
| enumerate
| each {|row| $row.item.event | to json | save --raw ($green_trajectory | path join $"($row.index).json") }
let green_decision = $green_root | path join "decision.json"
{
    role_id: "luna-pair"
    verdict: "CANDIDATE"
    findings: []
    candidate: $green_candidate
    red_owner: null
    next_role: "terra-academic"
} | to json | save --raw $green_decision
let green_grade = ^backend agents grade luna-pair $green_decision --events $green_trajectory | complete
assert equal $green_grade.exit_code 0 $green_grade.stderr
let mismatched_candidate = $green_root | path join "mismatched.json"
(open $green_decision | update candidate "unproved-candidate") | to json | save --raw $mismatched_candidate
let mismatched_grade = ^backend agents grade luna-pair $mismatched_candidate --events $green_trajectory | complete
assert not equal $mismatched_grade.exit_code 0
assert str contains $mismatched_grade.stderr "backend::candidate-identity-mismatch"

let escaped_decision = $sandbox | path join "escaped-agent-decision.json"
open $luna_decision | to json | save --raw $escaped_decision
let escaped_grade = ^backend agents grade luna-pair $escaped_decision --events $trajectory_root | complete
assert not equal $escaped_grade.exit_code 0
assert str contains $escaped_grade.stderr "backend::decision-path-escape"

let repeated_root = $sandbox | path join ".local/agent/runs/repeated/events"
mkdir $repeated_root
let luna_event = glob ($trajectory_root | path join "*.json") | each {|path| open $path } | where phase == "admitted" | first
$luna_event | to json | save --raw ($repeated_root | path join "000-admitted.json")
let failed_event = $luna_event | merge {
    phase: "result",
    scope: "external-process",
    status: "failed",
    failure_fingerprint: "same-failure",
    changed_paths_digest: "same-candidate",
    stderr_bytes: 1
}
$failed_event | to json | save --raw ($repeated_root | path join "001-failed.json")
$failed_event | to json | save --raw ($repeated_root | path join "002-failed.json")
let repeated_grade = ^backend agents grade luna-pair $luna_decision --events $repeated_root | complete
assert not equal $repeated_grade.exit_code 0
assert str contains $repeated_grade.stderr "backend::repeated-identical-failure"

let stalled_root = $sandbox | path join ".local/agent/runs/stalled/events"
mkdir $stalled_root
$luna_event | to json | save --raw ($stalled_root | path join "000-admitted.json")
$luna_event | upsert invocation_id "unchanged-second-call" | to json | save --raw ($stalled_root | path join "001-admitted.json")
let stalled_grade = ^backend agents grade luna-pair $luna_decision --events $stalled_root | complete
assert not equal $stalled_grade.exit_code 0
assert str contains $stalled_grade.stderr "backend::candidate-call-budget"

mkdir fixture/structure/nested/src fixture/structure/named
cp (
    $env.BACKEND_CONFIG_SNAPSHOT
    | path join "tests/control-plane/fixtures/structure/valid.rs"
) fixture/structure/valid.rs
cp (
    $env.BACKEND_CONFIG_SNAPSHOT
    | path join "tests/control-plane/fixtures/structure/missing-purpose.rs"
) fixture/structure/missing-purpose.rs
cp (
    $env.BACKEND_CONFIG_SNAPSHOT
    | path join "tests/control-plane/fixtures/structure/nested/src/item.rs"
) fixture/structure/nested/src/item.rs
cp (
    $env.BACKEND_CONFIG_SNAPSHOT
    | path join "tests/control-plane/fixtures/structure/named/mod.rs"
) fixture/structure/named/mod.rs

let structure = (
    ^backend lint structure fixture/structure/valid.rs fixture/structure/missing-purpose.rs fixture/structure/nested/src/item.rs fixture/structure/named/mod.rs --json
    | from json
)
assert equal ($structure | length) 3
assert equal ($structure.rule | sort) ["file-purpose" "flattened-crate" "named-module"]
rm --recursive fixture

mkdir .direnv
"# Deliberately occupies the forbidden cache root.\n# Exercises repository layout policy.\n# Is removed before fixture closure.\n" | save --raw .direnv/cache.nu
let cache_root = (^backend lint structure .direnv/cache.nu --json | from json)
assert equal $cache_root.rule ["local-artifact-root"]
rm --recursive .direnv

let changed_scope = (
    with-env (role-environment "terra-academic" "scope") { ^backend scope changed --base HEAD --json | from json }
)
assert ("crates/example/model.rs" in $changed_scope.paths)
assert equal $changed_scope.packages.name ["fixture-example"]

let changed_lint = (
    with-env (role-environment "terra-academic" "lint") { ^backend lint changed --base HEAD | complete }
)
assert equal $changed_lint.exit_code 0 $changed_lint.stderr
let changed_test = (
    with-env (role-environment "terra-academic" "verify") { ^backend test affected --base HEAD | complete }
)
assert equal $changed_test.exit_code 0 $changed_test.stderr

let adversarial = "//! Demonstrates a forbidden public accessor.\n//! Exercises changed-file syntax policy.\n//! Must remain red under the generated lint.\n\npub struct Count(pub u64);\nimpl Count { pub const fn get(self) -> u64 { self.0 } }\n"
$adversarial | save --force crates/example/model.rs
let rejected_lint = (
    with-env (role-environment "terra-academic" "lint") { ^backend lint changed --base HEAD | complete }
)
assert not equal $rejected_lint.exit_code 0
assert str contains $rejected_lint.stderr "backend::process-failed"

let escaped_skills = (
    ^backend agents generate --output ($sandbox | path join "rendered-skills")
    | complete
)
assert not equal $escaped_skills.exit_code 0
assert str contains $escaped_skills.stderr "backend::skill-output-escape"

let skills = $env.out
let generated = (^backend agents generate --output $skills | complete)
assert equal $generated.exit_code 0
assert equal (glob ($skills | path join "*/skills/*/SKILL.md") | length) 4
assert equal (glob ($skills | path join "*/skills/*/references/engineering.json") | length) 2
assert equal (glob ($skills | path join "*/skills/*/references/representation.json") | length) 2
assert equal (glob ($skills | path join "*/skills/*/references/migration.json") | length) 2
assert equal (
    glob ($skills | path join "*/skills/luna-pair/references/behavior.json")
    | length
) 1
assert equal (
    glob ($skills | path join "*/skills/sol-integrator/references/checklist.json")
    | length
) 1

let events = glob ($sandbox | path join ".local/observability/tooling/events/*.json")
assert (not ($events | is-empty)) "backend external processes must emit local bounded telemetry"
for event in $events {
    let fact = (open $event)
    let expected_event_fields = open $env.BACKEND_CONTROL_PLANE | get observability.eventRequired | sort
    assert equal ($fact | columns | sort) $expected_event_fields
}

let nextest_configs = glob ($sandbox | path join ".local/nextest/configs/*.toml")
assert equal ($nextest_configs | length) 1 "each test command needs one run-scoped nextest config"

assert not (($sandbox | path join ".cargo") | path exists) ".cargo must never be created"
assert not (($sandbox | path join ".direnv") | path exists) ".direnv must never be created"
